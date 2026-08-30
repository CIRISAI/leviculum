//! The media profile: which carriers this node meshes over.
//!
//! # Why the node declares its carriers
//!
//! An LNode meshes over LoRa **and** BLE at once by default, so a packet
//! that arrived over the other medium masks a loss on the medium under
//! test: every single-medium number such a node produces is falsifiable.
//! The cure is a profile that is *declared*, *applied* and *proven* — this
//! module is the applied and the proven half on the board:
//!
//! * **applied** — [`lora_active`] / [`ble_active`] are read by the two
//!   interfaces and by the binaries' RX arms, and by the boot path that
//!   decides whether to spawn a carrier's tasks at all.
//! * **proven** — [`log_banner`] emits one `[MEDIA]` line per boot. That
//!   line is the interface a measurement asserts on; its shape is frozen
//!   in `docs/src/structured-event-logs.md` and must not drift.
//!
//! The record itself lives on the telemetry flash page (layout in
//! [`crate::telemetry`]); the bytes are
//! [`leviculum_core::media_profile_store`].
//!
//! # Running versus configured
//!
//! Two states, because they honestly differ:
//!
//! * **configured** — what a reboot would come up with. Set from flash at
//!   boot (or from the both-on default), and by a host over
//!   [`TYPE_MEDIA_PROFILE`](leviculum_core::envelope::TYPE_MEDIA_PROFILE).
//! * **running** — what this boot is actually carrying traffic on:
//!   `configured AND booted_with`.
//!
//! Switching a carrier off always takes effect at once, and switching one
//! back on does too **as long as it came up at boot** — it was only being
//! ignored, its task is still there. A carrier that did *not* come up at
//! boot has no task to un-ignore: an embassy task cannot be spawned from
//! nothing after the fact, so it cannot start before the next reset.
//!
//! That case is answered with a report saying "configured, still not
//! running", never with an ack that would claim otherwise. See
//! [`leviculum_core::envelope::TYPE_MEDIA_REPORT`].
//!
//! # Teardown semantics of a runtime OFF
//!
//! Switching a medium off at runtime stops it carrying Reticulum traffic
//! in **both** directions and does so immediately: the interface drops
//! what the core hands it, and the binary's RX arm drops what the medium
//! hands up, so nothing crosses in either direction from the moment the
//! frame is answered. What it does *not* do is take the carrier off the
//! air — a live BLE connection stays connected and the advertisement
//! keeps going, and the LoRa task keeps listening. Those need the boot
//! path (which never starts them), which is why the rig acceptance for
//! "no advertisement on air" is flash-set-reboot and not a runtime set.
//! Stated rather than papered over: an operator who needs radio silence
//! reboots, and the `[MEDIA]` banner then proves it.

use core::sync::atomic::{AtomicBool, Ordering};

use leviculum_core::envelope::MediaProfileWire;

/// Whether this binary reads the profile and gates its carriers on it —
/// the capability the serial task's media answers are gated on
/// ([`leviculum_core::envelope::media_profile_answer`]). Set once via
/// [`load_at_boot`], which every binary calls before `usb::init`, so no
/// frame can be answered before the declaration exists.
///
/// The same ack-honesty rule as the telemetry reporter's, and it bites
/// harder here: an ack is what a measurement run reads as "this node is
/// now single-medium", so a binary that acked without gating would make
/// every number that run produced a lie.
static MEDIA_WIRED: AtomicBool = AtomicBool::new(false);

/// What a reboot would come up with.
static CONFIGURED_LORA: AtomicBool = AtomicBool::new(true);
static CONFIGURED_BLE: AtomicBool = AtomicBool::new(true);

/// What this boot actually started. A carrier whose tasks were never
/// spawned can never become running again before the next reset.
static BOOTED_LORA: AtomicBool = AtomicBool::new(false);
static BOOTED_BLE: AtomicBool = AtomicBool::new(false);

/// Where the configured profile came from, for the boot banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// No usable record on the page: blank, corrupt, or naming a carrier
    /// this firmware does not know. Both carriers on — today's behaviour,
    /// which is what the absence of a record has to mean.
    Default,
    /// A valid record, written by a host.
    Flash,
}

impl Source {
    /// The `src=` value of the `[MEDIA]` banner.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Flash => "flash",
        }
    }
}

/// Read the persisted profile, declare this binary's media gate, and
/// return what the boot should honour.
///
/// Call **before** `usb::init` and before either carrier is brought up:
/// before USB so a host frame can never be answered against the default
/// while the record is still unread, and before the carriers because the
/// answer decides which of them start at all. The read is an ordinary
/// memory-mapped flash read and is safe at any point in boot, including
/// before `Softdevice::enable`.
pub fn load_at_boot(page: u32) -> (MediaProfileWire, Source) {
    MEDIA_WIRED.store(true, Ordering::Relaxed);
    let (profile, source) = match crate::telemetry::load_media_profile(page) {
        Some(profile) => (profile, Source::Flash),
        None => (MediaProfileWire::BOTH, Source::Default),
    };
    CONFIGURED_LORA.store(profile.lora_enabled, Ordering::Relaxed);
    CONFIGURED_BLE.store(profile.ble_enabled, Ordering::Relaxed);
    (profile, source)
}

/// Record what this boot actually brought up. Call once, after both
/// carriers' spawn decisions have been made, and pass what was really
/// spawned rather than what was asked for — this is the value that makes
/// "a carrier that did not come up cannot be started" a fact the board
/// states instead of a hope.
pub fn note_boot_state(lora_started: bool, ble_started: bool) {
    BOOTED_LORA.store(lora_started, Ordering::Relaxed);
    BOOTED_BLE.store(ble_started, Ordering::Relaxed);
}

/// Whether [`load_at_boot`] was called — the capability the serial task's
/// media answers are gated on.
pub fn media_wired() -> bool {
    MEDIA_WIRED.load(Ordering::Relaxed)
}

/// Whether LoRa is carrying Reticulum traffic right now.
pub fn lora_active() -> bool {
    BOOTED_LORA.load(Ordering::Relaxed) && CONFIGURED_LORA.load(Ordering::Relaxed)
}

/// Whether BLE is carrying Reticulum traffic right now.
pub fn ble_active() -> bool {
    BOOTED_BLE.load(Ordering::Relaxed) && CONFIGURED_BLE.load(Ordering::Relaxed)
}

/// What this boot is carrying traffic on (see the module docs).
pub fn running() -> MediaProfileWire {
    MediaProfileWire {
        lora_enabled: lora_active(),
        ble_enabled: ble_active(),
    }
}

/// What a reboot would come up with (see the module docs).
pub fn configured() -> MediaProfileWire {
    MediaProfileWire {
        lora_enabled: CONFIGURED_LORA.load(Ordering::Relaxed),
        ble_enabled: CONFIGURED_BLE.load(Ordering::Relaxed),
    }
}

/// Apply a profile a host sent: take effect where that is possible, and
/// persist it either way.
///
/// Runs on the serial task rather than being handed to the main loop: the
/// whole apply is two atomic stores and a non-blocking save request, and
/// nothing here needs the node. That also means the answer the serial
/// task writes is the state that is already in force, not a prediction of
/// one — [`running`] and [`configured`] read back correct the instant
/// this returns.
///
/// The returned [`crate::telemetry::PendingSave`] is the other half of
/// the answer, and the caller must not write the report before waiting on
/// it (`crate::telemetry::confirm`, Codeberg #358). The report says what a
/// reboot would come up with; sent while the page write was still owed,
/// it was a claim about a reboot that the reboot disproved.
pub fn apply(profile: MediaProfileWire) -> crate::telemetry::PendingSave {
    CONFIGURED_LORA.store(profile.lora_enabled, Ordering::Relaxed);
    CONFIGURED_BLE.store(profile.ble_enabled, Ordering::Relaxed);
    crate::telemetry::request_save_media_profile(profile)
}

/// **The proof line.** One per boot, and re-emitted with the firmware
/// build banner so a capture attached after the boot window still reads
/// the running profile off the board rather than off an operator's
/// memory.
///
/// ```text
/// [MEDIA] lora=on ble=off src=flash t=1183
/// ```
///
/// `lora=`/`ble=` are what this boot is **running** (a medium configured
/// on but not started reads `off` here, which is the honest answer), and
/// `src=` says whether the configuration came off the page or from the
/// both-on default. The shape is an interface: periculum asserts on it,
/// and it is frozen in `docs/src/structured-event-logs.md`.
pub fn log_banner(source: Source) {
    let running = running();
    crate::log::log_fmt_critical(
        "[MEDIA] ",
        format_args!(
            "lora={} ble={} src={}",
            on_off(running.lora_enabled),
            on_off(running.ble_enabled),
            source.as_str()
        ),
    );
}

/// The banner's boolean spelling. `on`/`off` rather than `1`/`0`: the
/// line is read by operators at least as often as by periculum.
const fn on_off(enabled: bool) -> &'static str {
    if enabled {
        "on"
    } else {
        "off"
    }
}

/// Say that a carrier was held down at boot because the profile said so.
///
/// Emitted next to the carrier's own init log so a reader who greps for
/// `[LORA]` or `[BLE ]` and finds nothing has the reason on the line
/// where the bring-up would have been, not only in the `[MEDIA]` banner.
pub fn log_carrier_held_down(carrier: &str) {
    crate::log::log_fmt_critical(
        "[MEDIA] ",
        format_args!("carrier={} state=down reason=profile", carrier),
    );
}
