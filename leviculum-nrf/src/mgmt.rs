//! Remote management on the board: who may read its status (Codeberg
//! #235).
//!
//! A standalone LNode serves `rnstransport.remote.management` with the
//! `/status` request handler, so `rnstatus -R <board>` and `lnstatus -R
//! <board>` read a board exactly as they read a daemon — no second tool,
//! no serial cable, over whichever carrier the board is on. The gate is
//! an identity allow-list, and on a board that list is **writable over
//! USB and never over the air**.
//!
//! # Where the decisions live, and why not here
//!
//! This module is wiring. Everything that could be got wrong is in
//! [`leviculum_core::mgmt_allow_store`], which the host test suite
//! exercises (41d545d5: the firmware crate builds for thumbv7em, runs no
//! test of its own, and a decision written inside it is a decision nobody
//! has checked):
//!
//! * the record's bytes ([`decode_mgmt_allow`], and its torn-write
//!   refusals),
//! * the bound on how many identities a board may carry
//!   ([`MGMT_ALLOW_MAX_IDENTITIES`], argued there against the flash slot
//!   and the #388 heap budget),
//! * and the rule this feature turns on: **an absent or empty list
//!   registers nothing**
//!   ([`remote_mgmt_decision`]).
//!
//! That last one is a deliberate divergence from the daemon. `lnsd` and
//! Python's `rnsd` register the handler even with an empty
//! `remote_management_allowed` and consult the list per request
//! (`leviculum-std/src/config.rs:88-96` carries Python's key names and
//! Python's semantics). **A board must not**, and the reason is the
//! deployment, not the protocol: a daemon sits on a machine with an
//! operator, a config file and a login, while a board is handed to
//! somebody and left on a mast. A mast node that registers a management
//! destination and announces it with nobody on the list is advertising a
//! door, and #235 forbids exactly that. So on a board: no list, no
//! destination, no handler, no announce.
//!
//! # Two moments, and the board says which
//!
//! The destination is created while the node is BUILT
//! (`NodeCoreBuilder::remote_management`), from the record this module
//! read at boot. A list set over USB therefore takes effect at the next
//! reset — the running node cannot grow or withdraw a management
//! destination, and pretending otherwise would be worst for the two
//! operators who care most: the one who just enabled remote management and
//! the one who just revoked an identity. So the control frame is answered
//! with a report whose
//! [`MGMT_ALLOW_FLAG_RUNNING`](leviculum_core::envelope::MGMT_ALLOW_FLAG_RUNNING)
//! says whether this boot is serving anything, the same honesty the media
//! profile and the node name publish for their own reset-behind halves.
//!
//! # The USB-only guarantee
//!
//! It is a property of where the parser sits, not of a check inside it:
//! [`leviculum_core::envelope::classify_control_frame`] has exactly one
//! caller in this crate — the transport CDC read path in [`crate::usb`] —
//! and the LoRa and BLE tasks hand their bytes to the node core as
//! Reticulum packets. The same frame arriving from a radio is dropped in
//! packet parsing, which
//! `leviculum_core::node::mvr_mgmt_allow_is_usb_only` drives on a real
//! NodeCore rather than asserting by inspection.

use core::cell::Cell;
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;

use leviculum_core::constants::TRUNCATED_HASHBYTES;
use leviculum_core::mgmt_allow_store::{
    remote_mgmt_decision, StoredMgmtAllow, MGMT_ALLOW_MAX_IDENTITIES,
};

/// Re-exported so a binary names the decision it matches on without also
/// depending on the core module directly: the wiring layer is the seam,
/// and the seam owns its vocabulary (the same argument
/// [`crate::telemetry`] makes for `TargetOutcome`).
pub use leviculum_core::mgmt_allow_store::RemoteMgmtDecision;

/// Whether [`load_at_boot`] ran — the capability the serial task's
/// allow-list answers are gated on.
///
/// The same ack-honesty rule as the telemetry reporter's, the media
/// gate's and the name gate's: a binary that never read the record must
/// refuse rather than report, because an operator told "this board now
/// answers to that identity" who then finds it answers to nobody has been
/// lied to, and no retry fixes it.
static WIRED: AtomicBool = AtomicBool::new(false);

/// The allow-list as it stands on the page — what a reset would serve.
/// Written by the boot path and by the serial task.
///
/// A critical-section `Cell` rather than atomics, for the reason
/// [`crate::name`] uses one: the value is 129 bytes, there is no atomic
/// wide enough, and a read must not be able to see half of one list and
/// half of another. Uncontended in practice — the executor is cooperative
/// and single-core.
///
/// `None` is "no record on the page", distinct from a stored empty list
/// even though both serve nobody: only the first means nobody ever chose.
static STORED: Mutex<CriticalSectionRawMutex, Cell<Option<StoredMgmtAllow>>> =
    Mutex::new(Cell::new(None));

/// Whether THIS boot registered the management destination, recorded by
/// [`note_registered`] once the node is built.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Read the persisted allow-list, declare this binary's management gate,
/// and return what the node should be built with.
///
/// Call **before** the `NodeCoreBuilder` is finished — the answer decides
/// whether the node gets a management destination at all — and therefore
/// before `usb::init`, so no host frame can be answered against an unread
/// record. The read is an ordinary memory-mapped flash read and is legal
/// this early, including before `Softdevice::enable` (the argument
/// [`crate::telemetry::load`] makes).
///
/// The returned [`RemoteMgmtDecision`] is the whole policy; a caller that
/// pattern-matches it cannot accidentally enable an empty list, which is
/// the mistake this feature exists to prevent.
pub fn load_at_boot(page: u32) -> RemoteMgmtDecision {
    let stored = crate::telemetry::load_mgmt_allow(page);
    STORED.lock(|cell| cell.set(stored));
    WIRED.store(true, Ordering::Release);
    remote_mgmt_decision(stored.as_ref())
}

/// Say out loud what the boot decided, once, on the critical log path.
///
/// Two lines and no third: `enabled allowed=N` names the count an
/// operator can check against what they flashed, and `disabled (no
/// allow-list)` is the state a board ships in. Boot-critical because
/// `log_fmt` DROPS a line while the runtime-drain gate is shut (#234), and
/// this is the one line that says whether a board is answering `rnstatus
/// -R` at all — the first thing a reviewer greps for when it does not.
///
/// `registered` is what the node actually did: on a binary whose builder
/// could not derive a management identity the count would be a promise
/// nothing keeps, so the line reports the withheld case by name instead.
pub fn log_banner(decision: &RemoteMgmtDecision, registered: bool) {
    match decision {
        RemoteMgmtDecision::Enabled(allowed) if registered => {
            crate::log::log_fmt_critical(
                "[MGMT] ",
                format_args!("enabled allowed={}", allowed.len()),
            );
        }
        RemoteMgmtDecision::Enabled(allowed) => {
            crate::log::log_fmt_critical(
                "[MGMT] ",
                format_args!(
                    "withheld allowed={} reason=destination-underivable",
                    allowed.len()
                ),
            );
        }
        RemoteMgmtDecision::Disabled => {
            crate::log::log_fmt_critical("[MGMT] ", format_args!("disabled (no allow-list)"));
        }
    }
}

/// Record that this boot registered the management destination. Call
/// right after the node is built, with whether
/// `NodeCore::remote_mgmt_dest_hash()` came back set.
pub fn note_registered(registered: bool) {
    RUNNING.store(registered, Ordering::Release);
}

/// Whether [`load_at_boot`] was called.
pub fn mgmt_wired() -> bool {
    WIRED.load(Ordering::Acquire)
}

/// Whether this boot is serving `rnstransport.remote.management` right
/// now — the report's `RUNNING` flag.
pub fn running() -> bool {
    RUNNING.load(Ordering::Acquire)
}

/// The list on the page, or `None` when there is no record.
pub fn stored() -> Option<StoredMgmtAllow> {
    STORED.lock(|cell| cell.get())
}

/// Apply an allow-list a host sent — [`StoredMgmtAllow::EMPTY`] is the
/// explicit clear — and persist it.
///
/// Runs on the serial task rather than being handed to the main loop, like
/// [`crate::name::apply`]: the whole apply is one guarded store and a
/// non-blocking save request, and nothing here needs the node. The list is
/// already inside [`MAX_IDENTITIES`] and free of duplicates — the envelope
/// classifier refused anything else.
///
/// The returned [`crate::telemetry::PendingSave`] is the other half of the
/// answer and the caller must not write the report before waiting on it
/// (`crate::telemetry::confirm`, Codeberg #358). The entire meaning of a
/// set is what the NEXT boot serves, so a report sent while the page write
/// was still owed is a claim about a reboot the reboot would disprove.
pub fn apply(list: StoredMgmtAllow) -> crate::telemetry::PendingSave {
    STORED.lock(|cell| cell.set(Some(list)));
    crate::log::log_fmt_critical(
        "[MGMT] ",
        format_args!("allow-list set allowed={} effective=next-boot", list.len()),
    );
    crate::telemetry::request_save_mgmt_allow(list)
}

/// Put the published list back to `previous` after a save that did not
/// reach the page.
///
/// Required, not tidiness: the frame is answered `REFUSE_PERSIST` and
/// nothing is claimed, but the published value would go on describing a
/// list no reset will bring up, and the next
/// [`crate::envelope::TYPE_MGMT_ALLOW_QUERY`] would report it as stored.
/// For an access-control list that is the worst possible lie — an operator
/// reading back the identity they just added would believe it is on the
/// board. The caller passes what [`stored`] answered BEFORE its
/// [`apply`].
pub fn revert(previous: Option<StoredMgmtAllow>) {
    STORED.lock(|cell| cell.set(previous));
    crate::log::log_fmt_critical(
        "[MGMT] ",
        format_args!(
            "allow-list not persisted, reverted to allowed={}",
            previous.map(|list| list.len()).unwrap_or(0)
        ),
    );
}

/// The identity hashes a
/// [`TYPE_MGMT_ALLOW_REPORT`](leviculum_core::envelope::TYPE_MGMT_ALLOW_REPORT)
/// carries, in a buffer the caller owns.
///
/// A fixed array rather than a `Vec` because the serial task answers one
/// frame at a time on a 128 KiB stack and the list is 128 bytes: nothing
/// here needs the heap. `None` is "no record", which the report turns into
/// a cleared `STORED` flag rather than into an empty list that looks like
/// somebody's decision.
pub type ReportHashes = [[u8; TRUNCATED_HASHBYTES]; MGMT_ALLOW_MAX_IDENTITIES];

/// A zeroed [`ReportHashes`] for a caller to fill, so the serial task
/// never spells the buffer's dimensions and cannot get one of them wrong.
pub const EMPTY_REPORT: ReportHashes = [[0u8; TRUNCATED_HASHBYTES]; MGMT_ALLOW_MAX_IDENTITIES];

/// Fill `buf` with the stored list and return how many of its entries the
/// report should carry, or `None` when there is no record at all.
pub fn report_into(buf: &mut ReportHashes) -> Option<usize> {
    let stored = stored()?;
    let hashes = stored.hashes();
    buf[..hashes.len()].copy_from_slice(hashes);
    Some(hashes.len())
}
