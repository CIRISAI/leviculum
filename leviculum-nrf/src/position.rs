//! The board's `lev.position` announce (order 1301, Refs #232).
//!
//! A board that shares its position announces a third destination beside
//! its delivery destination and the propagation role: a SINGLE destination
//! of its own identity named `lev.position`, whose announce carries the
//! fix in the codec of [`leviculum_lxmf::lev_position`]. Every node in
//! range hears it, so the owner's map shows the board without a mailbox
//! and without a message (concept "Loquax: Zuhause, Mesh und Nutzer",
//! section 5).
//!
//! The rules are host-tested in [`leviculum_announce_policy`] and only
//! wired here:
//!
//! * **who reads it** — [`leviculum_announce_policy::PositionShare::resolve`]:
//!   sealed to the owner (the telemetry target's identity) by default on a
//!   tracker, off by default everywhere else, in the clear only when
//!   somebody set `lnflash --share-position everyone`;
//! * **when** — the slot [`AnnounceSlot::Position`] of the one cadence,
//!   with the 25 m advance and the channel thinning
//!   ([`leviculum_announce_policy::AnnounceCadence::advance_position`],
//!   [`leviculum_announce_policy::thinned_interval_ms`]);
//! * **no fix, no announce** — a due slot without a fix is pushed out by
//!   [`leviculum_announce_policy::POSITION_NO_FIX_RETRY_MS`] and sends
//!   nothing.
//!
//! What this module owns is the state the rules read, each fed by the
//! module that already has it: the stored setting (boot and the
//! `TYPE_POSITION_SHARE` frame), the telemetry target
//! ([`crate::telemetry::Reporter::apply_target`] calls [`note_target`]),
//! and the fixed position ([`note_fixed`]).

extern crate alloc;

use alloc::vec::Vec;
use core::cell::RefCell;
use core::sync::atomic::{AtomicU8, Ordering::Relaxed};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use leviculum_announce_policy::{
    AnnounceSlot, Decision, PositionShare, ShareSetting, POSITION_NO_FIX_RETRY_MS,
};
use leviculum_core::envelope::FixedPositionWire;
use leviculum_core::identity::Identity;
use leviculum_core::node::NodeCore;
use leviculum_core::traits::{Clock, Storage};
use leviculum_core::transport::Action;
use leviculum_core::DestinationHash;
use leviculum_core::{Destination, DestinationType, Direction, ProofStrategy};
use leviculum_lxmf::lev_position::{Position, Source, APP_NAME, ASPECT};
use leviculum_telemetry_policy::Fix;
use rand_core::CryptoRngCore;

use crate::announce::with_cadence;

/// The announce cap the policy crate restates, held to the core's.
const _: () = assert!(
    leviculum_announce_policy::ANNOUNCE_CAP_PERCENT
        == leviculum_core::constants::DEFAULT_ANNOUNCE_CAP_PERCENT as u64
);

/// The policy's settings are the stored record's bytes and `lnflash`'s
/// names, held equal here because neither crate depends on the other.
const _: () = {
    const fn same(a: &str, b: &str) -> bool {
        let (a, b) = (a.as_bytes(), b.as_bytes());
        if a.len() != b.len() {
            return false;
        }
        let mut i = 0;
        while i < a.len() {
            if a[i] != b[i] {
                return false;
            }
            i += 1;
        }
        true
    }
    use leviculum_core::position_share_store::{POSITION_SHARE_MAX, SETTING_NAMES};
    let all = [
        ShareSetting::Default,
        ShareSetting::Off,
        ShareSetting::Owner,
        ShareSetting::Everyone,
    ];
    assert!(ShareSetting::Everyone.wire() == POSITION_SHARE_MAX);
    let mut i = 0;
    while i < all.len() {
        assert!(all[i].wire() as usize == i);
        assert!(same(all[i].as_str(), SETTING_NAMES[i]));
        i += 1;
    }
};

/// The setting byte as stored ([`ShareSetting::wire`]).
static SETTING: AtomicU8 = AtomicU8::new(ShareSetting::Default.wire());

/// The telemetry target as the position needs it: its destination hash,
/// whose identity the sealed form is encrypted to, and whether it runs the
/// tracker profile, which decides the default.
#[derive(Clone, Copy)]
struct Owner {
    destination: [u8; 16],
    tracker: bool,
}

static OWNER: Mutex<CriticalSectionRawMutex, RefCell<Option<Owner>>> =
    Mutex::new(RefCell::new(None));

/// The fixed position somebody set, which wins over the receiver, as it
/// does for telemetry.
static FIXED: Mutex<CriticalSectionRawMutex, RefCell<Option<FixedPositionWire>>> =
    Mutex::new(RefCell::new(None));

/// The `lev.position` destination, registered on the first announce that
/// needs it, so a board that shares nothing never holds it.
static DESTINATION: Mutex<CriticalSectionRawMutex, RefCell<Option<DestinationHash>>> =
    Mutex::new(RefCell::new(None));

/// The share last written as a `[POSITION] share=` line, so the line is an
/// edge and not a drum. `0xFF` before the first.
static LAST_SHARE: AtomicU8 = AtomicU8::new(0xFF);

/// The withhold reason last written, cleared by a sent announce. `0` for
/// none.
static LAST_WITHHELD: AtomicU8 = AtomicU8::new(0);

/// Read the stored setting at boot.
pub fn load_at_boot(page: u32) {
    let stored = crate::telemetry::load_position_share(page);
    SETTING.store(stored.unwrap_or(ShareSetting::Default.wire()), Relaxed);
    note_share();
}

/// The `TYPE_POSITION_SHARE` frame: apply the setting at once and ask for
/// it to be persisted. The classifier already refused a byte that names
/// no setting.
pub fn apply_setting(setting: u8) -> crate::telemetry::PendingSave {
    SETTING.store(setting, Relaxed);
    note_share();
    crate::telemetry::request_save_position_share(setting)
}

/// The telemetry target changed: `None` for cleared.
pub fn note_target(destination: Option<[u8; 16]>, tracker: bool) {
    let owner = destination.map(|destination| Owner {
        destination,
        tracker,
    });
    OWNER.lock(|cell| *cell.borrow_mut() = owner);
    note_share();
}

/// The fixed position changed: `None` for cleared.
pub fn note_fixed(position: Option<FixedPositionWire>) {
    FIXED.lock(|cell| *cell.borrow_mut() = position);
}

fn setting() -> ShareSetting {
    ShareSetting::from_wire(SETTING.load(Relaxed)).unwrap_or_default()
}

/// Who reads this board's position announce right now.
#[must_use]
pub fn share() -> PositionShare {
    let tracker = OWNER.lock(|cell| cell.borrow().is_some_and(|o| o.tracker));
    PositionShare::resolve(setting(), tracker)
}

/// `[POSITION] share=<who> setting=<what>`, on each change: what a board
/// tells about where it is must be legible in a log tail.
fn note_share() {
    let share = share();
    let code = share as u8;
    if LAST_SHARE.swap(code, Relaxed) == code {
        return;
    }
    crate::log::log_fmt_critical(
        "[INFO!] ",
        format_args!(
            "[POSITION] share={} setting={}",
            share.as_str(),
            setting().as_str()
        ),
    );
}

/// Why a due position announce did not go out.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Withheld {
    NoFix = 1,
    NoOwner = 2,
    NoOwnerKey = 3,
    NoClock = 4,
    Unbuilt = 5,
}

impl Withheld {
    const fn as_str(self) -> &'static str {
        match self {
            Self::NoFix => "no-fix",
            Self::NoOwner => "no-owner",
            Self::NoOwnerKey => "no-owner-key",
            Self::NoClock => "no-clock",
            Self::Unbuilt => "unbuilt",
        }
    }
}

/// `[POSITION] withheld reason=<r>`, once per change.
fn log_withheld(reason: Withheld) {
    if LAST_WITHHELD.swap(reason as u8, Relaxed) == reason as u8 {
        return;
    }
    crate::log::log_fmt_critical(
        "[INFO!] ",
        format_args!("[POSITION] withheld reason={}", reason.as_str()),
    );
}

/// The board's position now, and the fix the cadence measures it by:
/// the fixed position when one is set, else the receiver's last usable
/// fix (valid, with an HDOP inside the tracker's gate). `None` is "no
/// fix", and no announce.
fn current(unix_secs: Option<u64>) -> Option<(Position, Fix)> {
    if let Some(fixed) = FIXED.lock(|cell| *cell.borrow()) {
        let position = Position {
            lat_e7: fixed.latitude_e6.saturating_mul(10),
            lon_e7: fixed.longitude_e6.saturating_mul(10),
            alt_m: fixed.altitude_e2.map(|a| a / 100),
            speed_cm_s: None,
            heading_e2: None,
            fix_age_s: 0,
            source: Some(Source::Manual),
            name: None,
        };
        let fix = Fix {
            latitude_e6: fixed.latitude_e6,
            longitude_e6: fixed.longitude_e6,
            hdop_e2: Some(leviculum_telemetry_policy::FIXED_POSITION_HDOP_E2),
        };
        return Some((position, fix));
    }
    gnss(unix_secs)
}

#[cfg(feature = "gnss")]
fn gnss(unix_secs: Option<u64>) -> Option<(Position, Fix)> {
    let reading = leviculum_firmware::board_state::GNSS_FIX.try_get()?;
    // The movement proof's own conversion, so the board holds one copy of
    // the degrees-to-integers arithmetic; the HDOP gate is the proof's too.
    let fix = leviculum_announce_policy::movement_fix(false, crate::announce::gnss_reading())?;
    if fix.hdop_e2? > leviculum_telemetry_policy::PolicyParams::TRACKER.max_hdop_e2 {
        return None;
    }
    let fix_age_s = match (unix_secs, reading.unix_secs) {
        (Some(now), Some(at)) => now.saturating_sub(at).min(u32::MAX as u64) as u32,
        _ => 0,
    };
    let position = Position {
        lat_e7: fix.latitude_e6.saturating_mul(10),
        lon_e7: fix.longitude_e6.saturating_mul(10),
        alt_m: reading.altitude_m.map(|a| a as i32),
        speed_cm_s: reading.speed_mps.map(|v| (v * 100.0) as u32),
        heading_e2: reading.bearing_deg.map(|b| (b * 100.0) as u16),
        fix_age_s,
        source: Some(Source::Gnss),
        name: None,
    };
    Some((position, fix))
}

/// See the `gnss` twin: no receiver in this binary.
#[cfg(not(feature = "gnss"))]
fn gnss(_unix_secs: Option<u64>) -> Option<(Position, Fix)> {
    None
}

/// Feed the 25 m advance one sample, from the movement sample's own wake
/// ([`crate::announce::AnnounceGate`]), and the CHTM's hour-long channel
/// load, which thins the slot.
pub fn sample(now_ms: u64) {
    let load = crate::chtm::last().map_or(0, |s| s.channel_load_long);
    with_cadence(|cadence| cadence.set_channel_load(load));
    if share() == PositionShare::Off {
        return;
    }
    let Some((_, fix)) = current(None) else {
        return;
    };
    if with_cadence(|cadence| cadence.advance_position(now_ms, fix)) {
        crate::log::log_fmt(
            "[INFO ] ",
            format_args!("[ANNOUNCE] trigger reason=position-moved"),
        );
    }
}

/// How many announce frames the position adds to the set the duty budget
/// covers ([`PositionShare::frames`]).
#[must_use]
pub fn frames() -> u32 {
    share().frames()
}

/// Register `lev.position` on first use: SINGLE, of the node's identity,
/// no proofs and no links, like the delivery destination this board keeps
/// no inbox behind.
#[inline(never)]
fn destination<R, C, S>(node: &mut NodeCore<R, C, S>) -> Option<DestinationHash>
where
    R: CryptoRngCore,
    C: Clock,
    S: Storage,
{
    if let Some(hash) = DESTINATION.lock(|cell| *cell.borrow()) {
        return Some(hash);
    }
    let bytes = node.identity().private_key_bytes().ok()?;
    let identity = Identity::from_private_key_bytes(&bytes).ok()?;
    let mut destination = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        APP_NAME,
        &[ASPECT],
    )
    .ok()?;
    destination.set_proof_strategy(ProofStrategy::None);
    destination.set_accepts_links(false);
    let hash = *destination.hash();
    node.register_destination(destination);
    DESTINATION.lock(|cell| *cell.borrow_mut() = Some(hash));
    Some(hash)
}

/// The position slot's tick, from [`crate::announce::AnnounceGate::periodic`].
///
/// Never inlined, and neither is the registration, `destination`: inlined into the four
/// binaries' main tasks the two cost the Pocket image 1.5 KiB more than
/// out of line (`just nrf-flash-budget`, order 1301: 7880 B free inlined,
/// 9416 B out of line, against the 8 KiB floor).
#[inline(never)]
pub fn periodic<R, C, S>(node: &mut NodeCore<R, C, S>) -> Vec<Action>
where
    R: CryptoRngCore,
    C: Clock,
    S: Storage,
{
    let share = share();
    let now_ms = node.now_ms();
    if share == PositionShare::Off
        || with_cadence(|cadence| cadence.wait_ms(AnnounceSlot::Position, now_ms)) > 0
    {
        return Vec::new();
    }
    let clock_ok = node.has_plausible_wall_clock();
    let ready = match ready(node, share, clock_ok) {
        Ok(ready) => ready,
        Err(reason) => {
            with_cadence(|cadence| {
                cadence.withhold(AnnounceSlot::Position, now_ms, POSITION_NO_FIX_RETRY_MS)
            });
            log_withheld(reason);
            return Vec::new();
        }
    };
    match with_cadence(|cadence| cadence.poll(AnnounceSlot::Position, now_ms, clock_ok)) {
        Some(Decision::Announce) => {}
        Some(Decision::Withheld(_)) => {
            log_withheld(Withheld::NoClock);
            return Vec::new();
        }
        None => return Vec::new(),
    }
    send(node, ready, AnnounceSlot::Position.as_str())
}

/// A position announce with everything it needs except the cadence's
/// say: the position, the fix the 25 m advance measures from, the sealed
/// form's recipient (`None` in the clear) and the destination.
pub struct Ready {
    position: Position,
    fix: Fix,
    owner: Option<Identity>,
    hash: DestinationHash,
}

/// Whether a double press would send a position announce now (order 1316):
/// the share resolves to owner or everyone, the clock is plausible, there
/// is a fix and, when sealed, the owner's key. The answer
/// [`leviculum_announce_policy::AnnounceCadence::manual`] books the slot
/// on.
///
/// A share that is off answers `None` and writes nothing, so a press on a
/// board that shares nothing reads as it did before. Any other refusal is
/// the tick's own `[POSITION] withheld reason=<r>` line, once per change.
#[inline(never)]
pub fn for_button<R, C, S>(node: &mut NodeCore<R, C, S>) -> Option<Ready>
where
    R: CryptoRngCore,
    C: Clock,
    S: Storage,
{
    let share = share();
    if share == PositionShare::Off {
        return None;
    }
    let clock_ok = node.has_plausible_wall_clock();
    if !clock_ok {
        log_withheld(Withheld::NoClock);
        return None;
    }
    match ready(node, share, clock_ok) {
        Ok(ready) => Some(ready),
        Err(reason) => {
            log_withheld(reason);
            None
        }
    }
}

/// Everything but the cadence: the fix, the owner's key when sealed, the
/// destination. The refusal is the withhold reason.
#[inline(never)]
fn ready<R, C, S>(
    node: &mut NodeCore<R, C, S>,
    share: PositionShare,
    clock_ok: bool,
) -> Result<Ready, Withheld>
where
    R: CryptoRngCore,
    C: Clock,
    S: Storage,
{
    let unix_secs = clock_ok.then(|| node.emission_secs());
    let (position, fix) = current(unix_secs).ok_or(Withheld::NoFix)?;
    // The sealed form's recipient, before the slot is spent on it. The
    // telemetry reporter asks the mesh for the key of a target it does not
    // know; this only waits for it.
    let owner = match share {
        PositionShare::Owner => {
            let owner = OWNER.lock(|cell| *cell.borrow()).ok_or(Withheld::NoOwner)?;
            let identity = node
                .storage()
                .get_identity(&owner.destination)
                .and_then(|known| Identity::from_public_key_bytes(&known.public_key_bytes()).ok())
                .ok_or(Withheld::NoOwnerKey)?;
            Some(identity)
        }
        _ => None,
    };
    let hash = destination(node).ok_or(Withheld::Unbuilt)?;
    Ok(Ready {
        position,
        fix,
        owner,
        hash,
    })
}

/// Build and send a position announce the cadence has already booked,
/// writing `[ANNOUNCE] sent ... reason=<reason>`.
#[inline(never)]
pub fn send<R, C, S>(node: &mut NodeCore<R, C, S>, ready: Ready, reason: &str) -> Vec<Action>
where
    R: CryptoRngCore,
    C: Clock,
    S: Storage,
{
    let Ready {
        mut position,
        fix,
        owner,
        hash,
    } = ready;
    position.name = Some(
        crate::name::mesh_name(node.identity().hash())
            .as_str()
            .into(),
    );
    let app_data = match &owner {
        Some(owner) => position.seal(owner, &mut leviculum_firmware::entropy::Entropy),
        None => Some(position.encode()),
    };
    let Some(app_data) = app_data else {
        log_withheld(Withheld::Unbuilt);
        return Vec::new();
    };
    match node.announce_destination(&hash, Some(&app_data)) {
        Ok(out) => {
            with_cadence(|cadence| cadence.note_position_sent(fix));
            LAST_WITHHELD.store(0, Relaxed);
            // The `[ANNOUNCE] sent` line every announce writes; whom it is
            // readable by is the `[POSITION] share=` line's, written on
            // each change.
            crate::announce::log_sent(&hash, reason, None);
            out.actions
        }
        Err(_) => Vec::new(),
    }
}
