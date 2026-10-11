//! The LoRa radio's channel ledgers, as one static (leviculum#636).
//!
//! What is measured and how is `leviculum_channel_access::load`, where a
//! host test can drive it. This module is only where the one instance
//! lives and how the firmware reaches it: the receive wait in
//! [`crate::sx1262`] feeds it carrier and RSSI samples, the LoRa task
//! charges airtime to its duty ledger and reads the lock, and the USB task
//! answers `TYPE_CHANNEL_STATS_QUERY` from its last snapshot.
//!
//! A static rather than a field of the LoRa task or of the radio driver:
//! the sampler sits under `leviculum_rx_arming`'s generic receive path,
//! which has no way to hand a ledger down, and the driver is moved by
//! value into the task at spawn, which a 2 KB field would make a 2 KB
//! stack copy. `ChannelLedger::new` is `const`, so this costs `.bss` and
//! no stack at all.
//!
//! Every access is one short critical section. The longest is an update,
//! once a second: two sums over 480 bins.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;
use leviculum_channel_access::load::{Carrier, ChannelLedger, ChannelSnapshot};

static LEDGER: Mutex<CriticalSectionRawMutex, RefCell<ChannelLedger>> =
    Mutex::new(RefCell::new(ChannelLedger::new()));

/// Run `f` on the ledger. `f` must not call back into this module: the
/// ledger is borrowed for its duration.
pub fn with<R>(f: impl FnOnce(&mut ChannelLedger) -> R) -> R {
    LEDGER.lock(|cell| f(&mut cell.borrow_mut()))
}

/// Take one carrier-and-RSSI sample, and report if a second has passed.
pub fn sample(now_ms: u64, carrier: Carrier, rssi_dbm: i16) {
    let snapshot = with(|l| {
        l.sample(now_ms, carrier, rssi_dbm);
        l.update(now_ms)
    });
    if let Some(snapshot) = snapshot {
        report(&snapshot);
    }
}

/// Recompute and report, if a second has passed since the last report.
pub fn update(now_ms: u64) {
    if let Some(snapshot) = with(|l| l.update(now_ms)) {
        report(&snapshot);
    }
}

/// The last snapshot, for the envelope's channel-stats report. `None`
/// before the LoRa task's first turn.
pub fn last() -> Option<ChannelSnapshot> {
    with(|l| l.last())
}

/// The once-a-second line, the board's twin of the host's `LORA_CHTM`
/// event. Kept out of the persistent crash tail, which a 1 Hz line would
/// otherwise own (`log_fmt_volatile`).
fn report(snapshot: &ChannelSnapshot) {
    crate::log::log_fmt_volatile("[LORA_CHTM] ", format_args!("{}", snapshot));
}
