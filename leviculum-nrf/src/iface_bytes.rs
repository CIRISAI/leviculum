//! Per-interface byte counters (Codeberg #235, #346).
//!
//! Until this module the board had no idea how many bytes any of its three
//! carriers had moved: `transport_stats.rs` counts packets and drop reasons,
//! and its header said in so many words that byte counters did not exist. A
//! `/status` answer needs them — `rnstatus` reads `interfaces[].rxb`/`txb`
//! unguarded (`reference/Reticulum/RNS/Utilities/rnstatus.py:575-576`), and a
//! mast node queried over the air is queried precisely to learn what it has
//! carried.
//!
//! # Where the bytes are counted
//!
//! The reference's semantics, not the radio's: Python counts the *unframed
//! packet* length, on receive where the interface hands the packet up
//! (`RNodeInterface.process_incoming`, `RNodeInterface.py:702`) and on
//! transmit where the interface accepts it for the medium
//! (`RNodeInterface.process_outgoing`, `RNodeInterface.py:725` — `datalen`,
//! the unescaped payload, counted around the serial write). So:
//!
//! * **tx** is counted in each `Interface::try_send` impl, on the success
//!   path only. A packet the media profile drops with the carrier off, or one
//!   the outbound queue refuses, never entered the medium and is not counted.
//! * **rx** is counted where the medium's task hands the packet to the main
//!   loop's channel, which is this firmware's `process_incoming`.
//!
//! KISS/split framing overhead is therefore outside the count, exactly as it
//! is in the reference. The numbers here are what feeds both the periodic
//! `[TRANSPORT]` lines and the `/status` bundle, so the board's own
//! instrument and its remote answer cannot disagree.
//!
//! # Why a critical-section cell and not an atomic
//!
//! The counters are `u64` and are written from the medium tasks and read from
//! the main loop. `AtomicU64` does not exist on `thumbv7em-none-eabihf`
//! (32-bit max atomic width), and a pair of `AtomicU32` halves cannot be read
//! consistently. `embassy_sync`'s blocking mutex over a `Cell` is the
//! allocation-free option that is already linked: the update is a handful of
//! instructions inside a critical section, on a path that runs once per
//! packet.

use core::cell::Cell;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;

/// Interface index of the USB CDC serial interface (`InterfaceId(0)`).
pub const SERIAL: usize = 0;
/// Interface index of the LoRa interface (`InterfaceId(1)`).
pub const LORA: usize = 1;
/// Interface index of the BLE interface (`InterfaceId(2)`).
pub const BLE: usize = 2;
/// Number of interfaces every firmware binary registers.
pub const IFACE_COUNT: usize = 3;

/// The name each interface reports, indexed by interface id.
///
/// The one table: the `Interface::name()` impls read it, the `[TRANSPORT]`
/// lines print it, and the `/status` bundle answers with it. A remote
/// `rnstatus -R` and a debug capture therefore name the same carrier the same
/// way, which is the whole point of feeding both from one set of counters.
pub const NAMES: [&str; IFACE_COUNT] = ["serial_usb", "lora_sx1262", "ble"];

/// Shortest window a speed may be derived from, in milliseconds.
///
/// A sample taken a few milliseconds after the last one divides a byte count
/// by near-zero and reports a speed the medium never reached. Below this the
/// previous window's figure stands, which is what Python's own decaying
/// speed estimate does between updates.
const MIN_WINDOW_MS: u64 = 1_000;

/// Lifetime byte totals of one interface.
#[derive(Clone, Copy)]
struct Bytes {
    rx: u64,
    tx: u64,
}

/// The last speed window: where the totals stood when it opened, and the
/// bytes/s it produced when it closed.
#[derive(Clone, Copy)]
struct Window {
    rx: u64,
    tx: u64,
    at_ms: u64,
    rxs: f64,
    txs: f64,
}

/// One interface's counters.
pub struct Counters {
    live: Mutex<CriticalSectionRawMutex, Cell<Bytes>>,
    window: Mutex<CriticalSectionRawMutex, Cell<Window>>,
}

impl Counters {
    const fn new() -> Self {
        Self {
            live: Mutex::new(Cell::new(Bytes { rx: 0, tx: 0 })),
            window: Mutex::new(Cell::new(Window {
                rx: 0,
                tx: 0,
                at_ms: 0,
                rxs: 0.0,
                txs: 0.0,
            })),
        }
    }

    fn add_rx(&self, bytes: usize) {
        self.live.lock(|c| {
            let mut v = c.get();
            v.rx = v.rx.saturating_add(bytes as u64);
            c.set(v);
        });
    }

    fn add_tx(&self, bytes: usize) {
        self.live.lock(|c| {
            let mut v = c.get();
            v.tx = v.tx.saturating_add(bytes as u64);
            c.set(v);
        });
    }

    /// Lifetime `(rxb, txb)` since boot.
    pub fn totals(&self) -> (u64, u64) {
        self.live.lock(|c| {
            let v = c.get();
            (v.rx, v.tx)
        })
    }

    /// The last closed window's `(rxs, txs)` in bytes per second.
    pub fn speeds(&self) -> (f64, f64) {
        self.window.lock(|c| {
            let w = c.get();
            (w.rxs, w.txs)
        })
    }

    /// Close the current speed window at `now_ms` and open the next one.
    ///
    /// Called from the one periodic emitter, so the window a `/status` answer
    /// reports is the same window the `[TRANSPORT]` line printed.
    fn sample(&self, now_ms: u64) {
        let (rx, tx) = self.totals();
        self.window.lock(|c| {
            let prev = c.get();
            let elapsed = now_ms.saturating_sub(prev.at_ms);
            if elapsed < MIN_WINDOW_MS {
                return;
            }
            let secs = elapsed as f64 / 1000.0;
            c.set(Window {
                rx,
                tx,
                at_ms: now_ms,
                rxs: rx.saturating_sub(prev.rx) as f64 / secs,
                txs: tx.saturating_sub(prev.tx) as f64 / secs,
            });
        });
    }
}

static COUNTERS: [Counters; IFACE_COUNT] = [Counters::new(), Counters::new(), Counters::new()];

/// The counters of one interface, or `None` for an index no binary registers.
pub fn counters(iface: usize) -> Option<&'static Counters> {
    COUNTERS.get(iface)
}

/// Count `bytes` received on `iface`. Out-of-range indices are ignored: a
/// miscounted byte must not be able to panic a board.
pub fn note_rx(iface: usize, bytes: usize) {
    if let Some(c) = COUNTERS.get(iface) {
        c.add_rx(bytes);
    }
}

/// Count `bytes` accepted for transmission on `iface`.
pub fn note_tx(iface: usize, bytes: usize) {
    if let Some(c) = COUNTERS.get(iface) {
        c.add_tx(bytes);
    }
}

/// Close every interface's speed window at `now_ms`. One call per
/// `[TRANSPORT]` emission.
pub fn sample_all(now_ms: u64) {
    for c in &COUNTERS {
        c.sample(now_ms);
    }
}
