//! When this board announces itself, and to whom (Codeberg #376).
//!
//! Until #376 a board announced its `lxmf.delivery` destination at
//! exactly one moment: immediately before a telemetry report
//! ([`crate::telemetry::Reporter::tick`]). Two consequences the field
//! reported on 2026-09-09 — a phone that connects between two reports
//! learns nothing about the board it is linked to, and a board with no
//! target (or an unreachable one) never announces at all, so an operator
//! watching a stationary relay saw an hour with no announce and could
//! not judge its hop count.
//!
//! This is the board's half of the answer. Two occasions:
//!
//! * [`AnnounceGate::peer_up`] — a BLE peer finished its identity
//!   handshake. The announce is addressed at THAT peer
//!   (`NodeCore::announce_destination_to_peer`), so it travels on that
//!   link alone. Broadcasting it is what produced the two-hop reading
//!   this issue is named for: the neighbour board gets a copy it was
//!   never meant to have, rebroadcasts it, and the relayed announce
//!   races the direct one into the phone's path table.
//! * [`AnnounceGate::periodic`] — the timer, on every interface, so a
//!   board that reports rarely is still findable.
//!
//! Both send the announce the telemetry path sends: same destination,
//! same [`crate::telemetry::announce_app_data`], same clock gate. One
//! announce shape exists on this board and this is it; the `lnflash
//! --announce` arm in the binaries is the third caller of the same pair.
//!
//! The WHEN is not decided here either — the cadence, the per-peer rate
//! limit and the clock gate live in [`leviculum_announce_policy`], which
//! is host-tested and which `lnsd` uses verbatim. This module is the
//! wiring: read the gate, build the announce, write the line.
//!
//! # One cadence for both halves (Codeberg #401)
//!
//! The periodic cadence is no longer a constant and is no longer chosen
//! at flash time. A board that moves announces fast, a board that does
//! not falls back to an hourly floor, movement resuming or a previously
//! unknown neighbour each buy one immediate announce, and the carrier's
//! duty budget can stretch all of it. The rule is
//! [`leviculum_announce_policy::AnnounceCadence`]; this module holds the
//! board's single instance of it in [`with_cadence`], because the
//! board's own announce and the propagation role
//! ([`crate::pn::Engine::tick_announce`]) must be decided together. A
//! board whose own announce is withheld is unreachable as a recipient
//! while still usable as a mailbox, and two cadences would hide that
//! asymmetry.
//!
//! What this module owes the cadence, on the board's side of the seam:
//!
//! * the position samples ([`sample_movement`], every
//!   [`MOVEMENT_SAMPLE_INTERVAL_MS`] off the periodic arm's own wake),
//! * the new-neighbour trigger ([`note_announce_heard`], from the event
//!   pass),
//! * the carrier's budget ([`note_duty_budget`], from
//!   [`crate::lora::AnnounceCap`], the one place that already knows both
//!   a frame's airtime and the band's lawful allowance).

use leviculum_announce_policy::{
    AnnounceCadence, AnnounceSlot, Decision, DutyBudget, PeerAnnounceLimiter, Withheld,
    MOVEMENT_SAMPLE_INTERVAL_MS,
};
use leviculum_core::node::NodeCore;
use leviculum_core::traits::{Clock, Storage};
use leviculum_core::transport::Action;
use leviculum_core::DestinationHash;
use rand_core::CryptoRngCore;

extern crate alloc;
use alloc::vec::Vec;

/// How many peer identities the rate limit remembers.
///
/// `MAX_LINKS` is 4 on both boards (`crate::ble::MAX_LINKS`), so eight
/// slots hold every currently linked peer plus four that have come and
/// gone inside the fifteen-minute window. Overflowing it costs one extra
/// announce on one link, never a wrongly withheld one — see
/// [`PeerAnnounceLimiter`].
const PEER_SLOTS: usize = 8;

/// The BLE interface's index in the board's interface table. Serial is 0,
/// LoRa 1, BLE 2 on both binaries; the peer-up edge only ever comes from
/// BLE, and the announce it triggers has to name the same interface the
/// peer is behind.
const BLE_IFACE: usize = 2;

/// How many neighbours the new-neighbour trigger remembers.
///
/// A neighbour in this table has already bought its one announce and
/// never buys another for the rest of the boot, which is what keeps rule
/// 4 a trigger rather than a cadence. Thirty-two is well above the peer
/// table the propagation role sizes for (`BOARD_MAX_PEERS`, 16), so a
/// board reaches it only on a channel busier than anything either stack
/// is provisioned for; the cost when it does is one extra announce for
/// the neighbour whose entry was displaced, bounded again by the
/// cadence's own trigger floor.
const NEIGHBOUR_SLOTS: usize = 32;

/// The one cadence both halves of the announce obey (#401).
///
/// A static rather than a field, because the two emitters are two
/// objects with two lifetimes — [`AnnounceGate`] and
/// [`crate::pn::Engine`], the second of which exists only on a boot that
/// runs the propagation role — and the decision is one. Same locking
/// shape as [`crate::identity`]'s statics, uncontended on the
/// single-core cooperative executor.
static CADENCE: embassy_sync::blocking_mutex::Mutex<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    core::cell::RefCell<AnnounceCadence>,
> = embassy_sync::blocking_mutex::Mutex::new(core::cell::RefCell::new(AnnounceCadence::new()));

/// Destination hashes this boot has already heard an announce from, so a
/// neighbour buys exactly one immediate announce and not one per
/// announce it sends.
static NEIGHBOURS: embassy_sync::blocking_mutex::Mutex<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    core::cell::RefCell<NeighbourTable>,
> = embassy_sync::blocking_mutex::Mutex::new(core::cell::RefCell::new(NeighbourTable::new()));

/// The stretch last written as an `[ANNOUNCE_DUTY]` line, `0` for "no
/// stretch in force". An edge, like `[ANNOUNCE_CAP]`'s: the line states a
/// change, and a board that repeated it every minute would bury the
/// events a capture is taken for.
///
/// 32 bits because thumbv7em has no 64-bit atomic, and a saturating cast
/// because this value is only ever compared for equality: the worst a
/// clamp could cost is one unwritten line at an interval of 49 days,
/// which no band and no carrier produce.
static LAST_STRETCH_MS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Run `f` against the board's one [`AnnounceCadence`].
pub fn with_cadence<T>(f: impl FnOnce(&mut AnnounceCadence) -> T) -> T {
    CADENCE.lock(|cell| f(&mut cell.borrow_mut()))
}

/// Tell the cadence what one announce frame costs on the live carrier and
/// what the band in force lawfully allows.
///
/// Called from [`crate::lora::AnnounceCap::sync`], which is where both
/// numbers already exist: it derives the frame's airtime from the PHY the
/// radio is actually running, and the long-term airtime lock is what the
/// interface resolved from its own TX frequency. Neither is re-derived.
///
/// The frame count is the announce SET — the board's own destination plus
/// the propagation role when this boot registered one — because the two
/// halves are decided together and a budget for one of them would be a
/// budget for a cadence nobody runs.
pub fn note_duty_budget(now_ms: u64, frame_airtime_ms: u64, lawful_duty_e4: u16) {
    let frames_per_set = if crate::identity::propagation_hash().is_some() {
        2
    } else {
        1
    };
    let budget = DutyBudget {
        frame_airtime_ms,
        frames_per_set,
        lawful_duty_e4,
    };
    if with_cadence(|cadence| cadence.set_budget(budget)) {
        log_stretch(now_ms);
    }
}

/// An announce was heard from `destination`. Rule 4: the FIRST time, both
/// halves announce at once.
///
/// This is what keeps two stationary boards in one room from waiting an
/// hour to find each other, and it costs one announce rather than a raised
/// cadence.
///
/// "Previously unknown" means: no announce from this destination has
/// reached this board since boot. Deliberately NOT hop-filtered — a
/// relayed announce from three hops away is still a node that may not
/// know us, and the trigger it buys is bounded twice over (once per
/// destination per boot, and never inside the cadence's own trigger
/// floor), so the cost of counting it is at most one announce.
///
/// Explicitly not a movement signal. A changed neighbour set is free and
/// immune to receiver drift, and it fails exactly where a fast announce
/// is worth the most — the single hiker in empty terrain with no
/// neighbours at all. It is a trigger here and nothing else: the cadence
/// afterwards is the one the movement state asked for.
pub fn note_announce_heard(now_ms: u64, destination: &[u8; 16]) {
    let new = NEIGHBOURS.lock(|cell| cell.borrow_mut().insert(*destination));
    if !new {
        return;
    }
    with_cadence(|cadence| cadence.trigger(now_ms));
    crate::log::log_fmt(
        "[INFO ] ",
        format_args!(
            "[ANNOUNCE] trigger reason=new-neighbour peer={:02x}{:02x}{:02x}{:02x}",
            destination[0], destination[1], destination[2], destination[3]
        ),
    );
}

/// `[ANNOUNCE_DUTY]`: the board is quieter than its configuration, and
/// this says why.
///
/// Written at the same level as `[ANNOUNCE_CAP]` and for the same reason
/// — without it the next person measures a cadence that is not the one in
/// effect — naming the configured interval, the interval actually used,
/// and the arithmetic that forced it.
fn log_stretch(now_ms: u64) {
    use core::sync::atomic::Ordering::Relaxed;
    let (configured_ms, effective_ms, budget) = with_cadence(|cadence| {
        (
            cadence.configured_interval_ms(now_ms),
            cadence.interval_ms(now_ms),
            cadence.budget(),
        )
    });
    let in_force = if effective_ms > configured_ms {
        effective_ms.min(u32::MAX as u64) as u32
    } else {
        0
    };
    if LAST_STRETCH_MS.swap(in_force, Relaxed) == in_force {
        return;
    }
    if in_force == 0 {
        crate::log::log_fmt_critical(
            "[ANNOUNCE_DUTY] ",
            format_args!(
                "stretch=lifted configured_ms={configured_ms} effective_ms={effective_ms}"
            ),
        );
        return;
    }
    crate::log::log_fmt_critical(
        "[ANNOUNCE_DUTY] ",
        format_args!(
            "stretch=in-force configured_ms={} effective_ms={} set_airtime_ms={} frames={} lawful_duty_e4={} share=1/{}",
            configured_ms,
            effective_ms,
            budget.set_airtime_ms(),
            budget.frames_per_set,
            budget.lawful_duty_e4,
            leviculum_announce_policy::OWN_ANNOUNCE_DUTY_SHARE,
        ),
    );
}

/// The neighbour set, as a fixed table with no allocation: a board with
/// no heap left must still be able to answer "have I heard this one
/// before".
struct NeighbourTable {
    slots: [Option<[u8; 16]>; NEIGHBOUR_SLOTS],
    next: usize,
}

impl NeighbourTable {
    const fn new() -> Self {
        Self {
            slots: [None; NEIGHBOUR_SLOTS],
            next: 0,
        }
    }

    /// Record a neighbour. `true` when it was not already known, which is
    /// the edge rule 4 fires on. Overflow displaces in round-robin order,
    /// the cheapest bound that never denies a genuinely new neighbour.
    fn insert(&mut self, destination: [u8; 16]) -> bool {
        if self.slots.contains(&Some(destination)) {
            return false;
        }
        self.slots[self.next] = Some(destination);
        self.next = (self.next + 1) % NEIGHBOUR_SLOTS;
        true
    }
}

/// The board's current position as the movement proof wants it, or `None`
/// when there is nothing to judge.
///
/// `None` in three cases, and each is deliberate:
///
/// * no GNSS receiver in this binary, or no fix yet — a board that cannot
///   observe its position has not proved movement, and the floor is the
///   safe answer;
/// * the receiver reports `valid=false` — `GnssFix` keeps the last good
///   coordinates across non-valid sentences so the display has something
///   to render, and a stale position offered as evidence is how a board
///   invents a jump it never made;
/// * a user has pinned a fixed position — that pin is an assertion that
///   this board does not move and it replaces the sensor entirely
///   (`leviculum_telemetry_policy::choose_position`), so the sensor is
///   not consulted behind the user's back.
#[must_use]
pub fn movement_fix() -> Option<leviculum_telemetry_policy::Fix> {
    if crate::telemetry::position_source_flags() & crate::telemetry::POSITION_SOURCE_FIXED != 0 {
        return None;
    }
    #[cfg(not(feature = "gnss"))]
    {
        None
    }
    #[cfg(feature = "gnss")]
    {
        let fix = crate::baseboard::GNSS_FIX.try_get()?;
        if !fix.valid {
            return None;
        }
        Some(leviculum_telemetry_policy::Fix {
            latitude_e6: (fix.latitude? * 1e6) as i32,
            longitude_e6: (fix.longitude? * 1e6) as i32,
            hdop_e2: fix.hdop.map(|h| (h * 100.0).clamp(0.0, 65535.0) as u16),
        })
    }
}

/// The board's announce occasions, and the gates on them.
pub struct AnnounceGate {
    peers: PeerAnnounceLimiter<PEER_SLOTS>,
    /// When the movement proof is next owed a position sample. Separate
    /// from the announce deadline because the two are hours apart on a
    /// still board: a detector polled only when an announce is due would
    /// never see the board start moving.
    next_sample_ms: u64,
    /// The last critical withhold reason written, so a standing
    /// condition is stated once and not once a minute for as long as it
    /// lasts. Same rule, and the same argument, as
    /// [`crate::telemetry::Reporter`]'s `last_withheld`: a silent board
    /// has to be legible in a log tail, which a flood is not.
    last_withheld: Option<Withheld>,
}

impl AnnounceGate {
    /// Arm both halves of the announce relative to `now_ms` (boot).
    #[must_use]
    pub fn new(now_ms: u64) -> Self {
        with_cadence(|cadence| cadence.arm(now_ms));
        Self {
            peers: PeerAnnounceLimiter::new(),
            next_sample_ms: now_ms.saturating_add(MOVEMENT_SAMPLE_INTERVAL_MS),
            last_withheld: None,
        }
    }

    /// How long the caller may sleep before the next [`Self::periodic`]
    /// is worth calling.
    ///
    /// Clamped by the movement sample, not only by the announce deadline:
    /// on a still board those are an hour apart, and the proof needs a
    /// position every [`MOVEMENT_SAMPLE_INTERVAL_MS`] to have anything to
    /// reason about when the board is picked up.
    #[must_use]
    pub fn periodic_wait_ms(&self, now_ms: u64) -> u64 {
        let announce = with_cadence(|cadence| cadence.wait_ms(AnnounceSlot::Own, now_ms));
        announce.min(self.next_sample_ms.saturating_sub(now_ms))
    }

    /// Feed the movement proof one position, if a sample is due.
    ///
    /// Rules 1 to 3 in one call: the sample decides the cadence, and a
    /// board that has just been proven to be moving announces at once
    /// rather than waiting for the tick.
    fn sample_movement(&mut self, now_ms: u64) {
        if now_ms < self.next_sample_ms {
            return;
        }
        self.next_sample_ms = now_ms.saturating_add(MOVEMENT_SAMPLE_INTERVAL_MS);
        let fix = movement_fix();
        let resumed = with_cadence(|cadence| cadence.note_fix(now_ms, fix));
        if resumed {
            crate::log::log_fmt_critical(
                "[INFO!] ",
                format_args!("[ANNOUNCE] trigger reason=movement-resumed"),
            );
        }
        // The fast state can also END without a sample saying so (the
        // fifteen-minute bound), and either edge changes what the budget
        // has to cover.
        log_stretch(now_ms);
    }

    /// A BLE peer completed its identity handshake: announce to it, on
    /// its link alone.
    ///
    /// Returns the actions to dispatch — empty when the gate withheld,
    /// when this board registered no delivery destination, or when the
    /// announce could not be built. The packet goes through the ordinary
    /// BLE outbound path, so the peripheral hold (`leviculum_ble_tx::hold`)
    /// makes it WAIT in the link's queue until the peer has subscribed
    /// rather than failing as an early notify.
    pub fn peer_up<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        delivery_hash: Option<&DestinationHash>,
        peer: [u8; 16],
    ) -> Vec<Action>
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let Some(hash) = delivery_hash else {
            return Vec::new();
        };
        let now_ms = node.now_ms();
        match self
            .peers
            .peer_up(peer, now_ms, node.has_plausible_wall_clock())
        {
            Decision::Announce => {
                self.last_withheld = None;
                let app_data = crate::telemetry::announce_app_data(node.identity());
                match node.announce_destination_to_peer(hash, Some(&app_data), BLE_IFACE, peer) {
                    Ok(out) => {
                        log_sent(hash, "peer-up", Some(&peer));
                        out.actions
                    }
                    Err(_) => Vec::new(),
                }
            }
            Decision::Withheld(reason) => {
                self.log_withheld(reason, Some(&peer));
                Vec::new()
            }
        }
    }

    /// The periodic tick. Call it whenever the loop wakes; it is a no-op
    /// before its own deadline, so the caller does not have to be exact.
    pub fn periodic<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        delivery_hash: Option<&DestinationHash>,
    ) -> Vec<Action>
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let now_ms = node.now_ms();
        self.sample_movement(now_ms);
        let clock_ok = node.has_plausible_wall_clock();
        let Some(decision) =
            with_cadence(|cadence| cadence.poll(AnnounceSlot::Own, now_ms, clock_ok))
        else {
            return Vec::new();
        };
        let Some(hash) = delivery_hash else {
            return Vec::new();
        };
        match decision {
            Decision::Announce => {
                self.last_withheld = None;
                let app_data = crate::telemetry::announce_app_data(node.identity());
                match node.announce_destination(hash, Some(&app_data)) {
                    Ok(out) => {
                        log_sent(hash, AnnounceSlot::Own.as_str(), None);
                        out.actions
                    }
                    Err(_) => Vec::new(),
                }
            }
            Decision::Withheld(reason) => {
                self.log_withheld(reason, None);
                Vec::new()
            }
        }
    }
}

/// `[ANNOUNCE] sent dst=<hex8> reason=<r> [peer=<hex8>]`, the line the
/// desk recipe greps for. Critical, like the `reason=host` line the
/// `lnflash --announce` arm already writes: an announce is the event a
/// capture is opened for.
fn log_sent(hash: &DestinationHash, reason: &str, peer: Option<&[u8; 16]>) {
    let d = hash.as_bytes();
    crate::log::log_fmt_critical(
        "[INFO!] ",
        format_args!(
            "[ANNOUNCE] sent dst={:02x}{:02x}{:02x}{:02x} reason={}{}",
            d[0],
            d[1],
            d[2],
            d[3],
            reason,
            Peer(peer)
        ),
    );
}

/// The withheld sibling. `no-clock` is critical — it is why a board is
/// silent, and an operator hunting silence must see it without raising
/// the log level; it is written once per change, not once per retry.
/// `rate-limited` is not critical: a phone rotating its BLE address
/// relinks about once a minute, and one critical line per minute per
/// peer would bury the events a capture is taken for.
impl AnnounceGate {
    fn log_withheld(&mut self, reason: Withheld, peer: Option<&[u8; 16]>) {
        match reason {
            Withheld::NoClock => {
                if self.last_withheld == Some(reason) {
                    return;
                }
                self.last_withheld = Some(reason);
                crate::log::log_fmt_critical(
                    "[INFO!] ",
                    format_args!(
                        "[ANNOUNCE] withheld reason={}{}",
                        reason.as_str(),
                        Peer(peer)
                    ),
                );
            }
            Withheld::RateLimited => crate::log::log_fmt(
                "[BLE  ] ",
                format_args!(
                    "[ANNOUNCE] withheld reason={}{}",
                    reason.as_str(),
                    Peer(peer)
                ),
            ),
        }
    }
}

/// The optional ` peer=<hex8>` tail of an `[ANNOUNCE]` line: written
/// when the occasion names a peer, absent when it does not. A formatter
/// rather than two format strings, so the two occasions cannot drift
/// into two spellings of the same field.
struct Peer<'a>(Option<&'a [u8; 16]>);

impl core::fmt::Display for Peer<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(p) => write!(f, " peer={:02x}{:02x}{:02x}{:02x}", p[0], p[1], p[2], p[3]),
            None => Ok(()),
        }
    }
}
