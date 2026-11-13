//! SPI2 shared by the panel and the LR1110 (1478b).
//!
//! The ThinkNode M9 wires its ST7789 panel, its LR1110 and its SD card to
//! one SPI bus (`leviculum-esp`'s `boards::thinknode_m9`, "The shared SPI
//! bus"). The panel is flushed by DMA on the UI core, in chunks of up to
//! 51 rows (6.5 ms at 40 MHz, a full frame 30.8 ms); the radio is driven
//! by core 0. The reviewer's decision (order 1478b): one arbiter on the
//! bus, the radio taking it between the panel's chunks, not the UI core
//! doing the radio's SPI on its behalf. The radio's receive read and its
//! channel-access timing must not inherit the UI core's frame cadence; a
//! 31 ms flush is the panel's to split.
//!
//! The rules, each pinned by a test below:
//!
//! - **One owner.** The bus is free, the panel's or the radio's; every
//!   change of owner is a compare-and-swap from free, so the two cores can
//!   never both hold it.
//! - **The radio first.** The radio announces before it tries
//!   ([`Arbiter::radio_take`]); the panel, before each chunk, looks for a
//!   fresh announcement ([`Arbiter::panel_take`]) and yields instead of
//!   starting the chunk. So the radio waits at most one chunk.
//! - **An ask goes stale.** An announcement older than [`ASK_FRESH_US`] no
//!   longer holds the panel back: a core 0 busy in a long turn does not
//!   stall the UI core's flush past that bound. The radio's next try
//!   announces afresh.
//! - **The clock follows the owner** ([`run`]): the radio's clock for its
//!   transaction, the panel's restored before the bus is released, also
//!   when the transaction failed. The panel never sets the clock.
//! - **The radio's chip select is held only inside its transaction**
//!   ([`run`]): selected after the clock switched, released before the
//!   clock goes back, so no byte at the panel's clock ever reaches the
//!   radio and the panel never sees a byte with the radio selected.
//!
//! The panel's own chip select is the firmware's (`Panel::pump`): a panel
//! that yields inside a row window closes it and reopens it at its next
//! row, so the radio's bytes never land in the panel's memory.
//!
//! The clock is the caller's, microseconds on a wrapping `u32`, as in
//! [`crate::mspi`], so the host steps both cores ([`interleave`]).

use core::fmt;
use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};

/// How long an announcement of the radio holds the panel back: a core 0
/// that tries again within it is a core 0 waiting for the bus, one that
/// did not is busy elsewhere, and the panel goes on. Core 0's longest
/// idle wait, one keyboard tick (10 ms, `keyboard::POLL_MS`), and 2 ms
/// besides: longer than a chunk (6.5 ms), short beside the UI core's 5 s
/// watchdog.
pub const ASK_FRESH_US: u32 = 12_000;

const FREE: u8 = 0;
const PANEL: u8 = 1;
const RADIO: u8 = 2;

/// Who holds the bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Free,
    Panel,
    Radio,
}

impl Owner {
    /// The word the `[SPI]` line says.
    pub fn word(self) -> &'static str {
        match self {
            Owner::Free => "free",
            Owner::Panel => "panel",
            Owner::Radio => "radio",
        }
    }
}

/// `at` lies within [`ASK_FRESH_US`] before `now` (and not after it).
fn fresh(at: u32, now: u32) -> bool {
    at != 0 && now.wrapping_sub(at) < ASK_FRESH_US
}

/// The two cores' claims on SPI2.
#[derive(Debug)]
pub struct Arbiter {
    owner: AtomicU8,
    /// When the waiting transaction first asked; 0 when none waits.
    asked: AtomicU32,
    /// The waiting transaction's latest try; 0 when none waits.
    tried: AtomicU32,
    /// The longest wait from first ask to grant since the last
    /// [`Arbiter::take_window`].
    wait_max_us: AtomicU32,
    /// Grants to the radio, ever.
    grants: AtomicU32,
    /// Chunks the panel started, ever.
    chunks: AtomicU32,
    /// Times the panel gave the bus up for a radio that asked, ever.
    yields: AtomicU32,
}

impl Default for Arbiter {
    fn default() -> Self {
        Self::new()
    }
}

/// What the arbiter counted, for the `[SPI]` line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Window {
    pub owner: Option<Owner>,
    pub wait_max_us: u32,
    pub grants: u32,
    pub chunks: u32,
    pub yields: u32,
}

impl Arbiter {
    pub const fn new() -> Self {
        Self {
            owner: AtomicU8::new(FREE),
            asked: AtomicU32::new(0),
            tried: AtomicU32::new(0),
            wait_max_us: AtomicU32::new(0),
            grants: AtomicU32::new(0),
            chunks: AtomicU32::new(0),
            yields: AtomicU32::new(0),
        }
    }

    /// Who holds the bus now.
    pub fn owner(&self) -> Owner {
        match self.owner.load(Ordering::SeqCst) {
            PANEL => Owner::Panel,
            RADIO => Owner::Radio,
            _ => Owner::Free,
        }
    }

    /// Whether a radio that asked at most [`ASK_FRESH_US`] ago still waits.
    pub fn radio_waits(&self, now: u32) -> bool {
        fresh(self.tried.load(Ordering::SeqCst), now)
    }

    /// The panel takes the bus, or keeps it, for its next chunk at `now`:
    /// `false` when a radio waits (the caller then gives the bus up with
    /// [`Arbiter::panel_release`] if it held it) or holds the bus.
    pub fn panel_take(&self, now: u32) -> bool {
        if self.radio_waits(now) {
            return false;
        }
        match self
            .owner
            .compare_exchange(FREE, PANEL, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) | Err(PANEL) => true,
            Err(_) => false,
        }
    }

    /// A chunk the panel started under its hold, for the count.
    pub fn panel_chunk(&self) {
        self.chunks.fetch_add(1, Ordering::Relaxed);
    }

    /// The panel gives the bus up: its flush is done, or a radio waits.
    /// The caller has put the bus where the radio takes it from first.
    pub fn panel_release(&self, now: u32) {
        if self
            .owner
            .compare_exchange(PANEL, FREE, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
            && self.radio_waits(now)
        {
            self.yields.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The radio asks for the bus at `now` and takes it when free: `true`
    /// grants one transaction, ended by [`Arbiter::radio_release`]. A
    /// `false` leaves the ask standing, and the panel yields at its next
    /// chunk; the radio tries again.
    pub fn radio_take(&self, now: u32) -> bool {
        let now = now.max(1);
        // The first ask's time stands, for the wait; the latest try
        // keeps the ask fresh. Both before the swap: a panel that looks
        // after it sees the ask.
        let _ = self
            .asked
            .compare_exchange(0, now, Ordering::SeqCst, Ordering::SeqCst);
        self.tried.store(now, Ordering::SeqCst);
        if self
            .owner
            .compare_exchange(FREE, RADIO, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        let first = self.asked.swap(0, Ordering::SeqCst);
        self.tried.store(0, Ordering::SeqCst);
        let waited = match first {
            0 => 0,
            first => now.wrapping_sub(first),
        };
        self.wait_max_us.fetch_max(waited, Ordering::Relaxed);
        self.grants.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// The radio's transaction is over; the caller has put the bus back,
    /// on the panel's clock.
    pub fn radio_release(&self) {
        let _ = self
            .owner
            .compare_exchange(RADIO, FREE, Ordering::SeqCst, Ordering::SeqCst);
    }

    /// The counts, the longest wait since the last call starting again.
    pub fn take_window(&self) -> Window {
        Window {
            owner: Some(self.owner()),
            wait_max_us: self.wait_max_us.swap(0, Ordering::Relaxed),
            grants: self.grants.load(Ordering::Relaxed),
            chunks: self.chunks.load(Ordering::Relaxed),
            yields: self.yields.load(Ordering::Relaxed),
        }
    }
}

/// The bus's two clocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clocks {
    /// The panel's, the bus's resting clock.
    pub panel_hz: u32,
    /// The radio's, for its transactions only.
    pub radio_hz: u32,
}

/// The bus as the radio drives it while it holds it.
pub trait Wire {
    type Error;
    /// Set the bus clock.
    fn clock(&mut self, hz: u32) -> Result<(), Self::Error>;
    /// Drive the radio's chip select: `true` selects (low).
    fn select(&mut self, selected: bool);
}

/// One radio transaction `f` on a bus the radio was granted: the radio's
/// clock, its chip select held for `f` alone, then the panel's clock back,
/// whatever `f` returned. The first error wins.
pub fn run<W: Wire, R>(
    wire: &mut W,
    clocks: Clocks,
    f: impl FnOnce(&mut W) -> Result<R, W::Error>,
) -> Result<R, W::Error> {
    let r = match wire.clock(clocks.radio_hz) {
        Ok(()) => {
            wire.select(true);
            let r = f(wire);
            wire.select(false);
            r
        }
        Err(e) => Err(e),
    };
    let restored = wire.clock(clocks.panel_hz);
    match (r, restored) {
        (Ok(v), Ok(())) => Ok(v),
        (Err(e), _) | (Ok(_), Err(e)) => Err(e),
    }
}

// ---------------------------------------------------------------------
// The two cores stepped on the host
// ---------------------------------------------------------------------

/// What [`interleave`] steps: a flush of `chunks_us` with the radio polling
/// every `radio_every_us`, `0` for a radio that never asks.
#[derive(Clone, Copy, Debug)]
pub struct Model<'a> {
    /// Each chunk's time on the bus, in the flush's order.
    pub chunks_us: &'a [u32],
    /// The window the panel reopens after it yielded inside one: CASET,
    /// RASET and RAMWR, and the DMA's start.
    pub reopen_us: u32,
    /// The radio's period, `0` for none.
    pub radio_every_us: u32,
    /// When, in the flush, the radio first asks.
    pub radio_phase_us: u32,
    /// One radio transaction on the bus, both clock switches in.
    pub radio_txn_us: u32,
    /// Core 0's turn between two tries of a radio that waits.
    pub io_turn_us: u32,
    /// How often the UI core pumps the flush: `0` for a core that spins
    /// on it (`Panel::settle`, before a draw), else its turn (the loop's
    /// one `Panel::pump` a turn). A chunk that ended holds the bus until
    /// the next pump sees it.
    pub pump_every_us: u32,
}

/// The board's full frame at 40 MHz: 240 rows in chunks of 51
/// (`ROWS_PER_DMA`, esp-hal's 32,736 bytes a transfer), 640 bytes a row.
pub const FULL_FRAME_CHUNKS_US: [u32; 5] = [6_528, 6_528, 6_528, 6_528, 4_608];

/// The board as the model steps it (1478b): a full frame, the radio's
/// status read every 10 ms, the UI core spinning on the flush (its loop
/// does not idle while one is on its way). The chunks are the bytes' time
/// at 40 MHz; the rest are estimates the bench's `[SPI]` and `[DISP]`
/// lines replace: a reopened window's three commands and the DMA's start
/// 30 us, a six-byte status read at 4 MHz with both clock switches 60 us,
/// a quick core 0 turn 200 us.
pub const BOARD: Model<'static> = Model {
    chunks_us: &FULL_FRAME_CHUNKS_US,
    reopen_us: 30,
    radio_every_us: 10_000,
    radio_phase_us: 0,
    radio_txn_us: 60,
    io_turn_us: 200,
    pump_every_us: 0,
};

/// What [`interleave`] measured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Interleaved {
    /// From the first chunk's start to the last chunk's end.
    pub flush_us: u32,
    /// The longest a radio transaction waited for the bus.
    pub wait_max_us: u32,
    /// The longest the panel went without a chunk running, between its
    /// first and its last: the gap its watchdog feeding sees.
    pub gap_max_us: u32,
    pub grants: u32,
    pub yields: u32,
    pub chunks: u32,
}

/// Step the panel's flush and the radio's polls one microsecond at a time
/// over a real [`Arbiter`], each core as its loop drives it: the panel
/// tries for the bus before each chunk and spins while it may not; the
/// radio asks every `radio_every_us` and, refused, tries again a core 0
/// turn later.
pub fn interleave(m: Model<'_>) -> Interleaved {
    if m.chunks_us.is_empty() {
        return Interleaved::default();
    }
    let arb = Arbiter::new();
    let mut now: u32 = 1;
    let begin = now;
    // The panel.
    let mut next = 0usize;
    let mut dma_end: Option<u32> = None;
    let mut held = false;
    let mut reopen = false;
    let mut idle_since: Option<u32> = None;
    let mut gap_max = 0u32;
    // The radio.
    let mut radio_next = begin.wrapping_add(m.radio_phase_us);
    let mut radio_end: Option<u32> = None;
    let mut grants = 0;
    let mut next_pump = begin;
    let end;
    loop {
        // The radio core.
        if let Some(e) = radio_end {
            if now == e {
                arb.radio_release();
                radio_end = None;
            }
        }
        if m.radio_every_us > 0 && radio_end.is_none() && now == radio_next {
            if arb.radio_take(now) {
                grants += 1;
                radio_end = Some(now.wrapping_add(m.radio_txn_us.max(1)));
                radio_next = now.wrapping_add(m.radio_every_us);
            } else {
                radio_next = now.wrapping_add(m.io_turn_us.max(1));
            }
        }
        // The UI core: the DMA ends on its own, the pump sees it when it
        // runs.
        let pumps = m.pump_every_us == 0 || now == next_pump;
        if pumps && m.pump_every_us > 0 {
            next_pump = now.wrapping_add(m.pump_every_us);
        }
        if let Some(e) = dma_end {
            if pumps && now.wrapping_sub(e) as i32 >= 0 {
                dma_end = None;
                idle_since = Some(e);
                if next == m.chunks_us.len() {
                    arb.panel_release(now);
                    end = e;
                    break;
                }
            }
        }
        if pumps && dma_end.is_none() && next < m.chunks_us.len() {
            if arb.panel_take(now) {
                held = true;
                arb.panel_chunk();
                let cost = m.chunks_us[next] + if reopen { m.reopen_us } else { 0 };
                reopen = false;
                dma_end = Some(now.wrapping_add(cost.max(1)));
                next += 1;
                if let Some(since) = idle_since.take() {
                    gap_max = gap_max.max(now.wrapping_sub(since));
                }
            } else if held {
                arb.panel_release(now);
                held = false;
                // Inside the frame's one window: closed, reopened at the
                // next row.
                reopen = next > 0;
            }
        }
        now = now.wrapping_add(1);
    }
    let w = arb.take_window();
    Interleaved {
        flush_us: end.wrapping_sub(begin),
        wait_max_us: w.wait_max_us,
        gap_max_us: gap_max,
        grants,
        yields: w.yields,
        chunks: w.chunks,
    }
}

// ---------------------------------------------------------------------
// The boot probe's lines
// ---------------------------------------------------------------------

/// A LoRa PHY as the probe programs and says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Phy {
    pub frequency_hz: u32,
    pub bandwidth_hz: u32,
    pub sf: u8,
    /// Coding rate denominator, 4/`cr_denom`.
    pub cr_denom: u8,
}

/// The PHY every Leviculum board comes up on: the ReticulumNet NL
/// channel, 869.463 MHz, BW 125 kHz, SF8, CR 4/5, as
/// `leviculum-nrf/src/lora.rs`'s `RadioConfig::eu_medium`. The M9 image
/// fails to build when its board constants say anything else.
pub const DEFAULT_PHY: Phy = Phy {
    frequency_hz: 869_463_000,
    bandwidth_hz: 125_000,
    sf: 8,
    cr_denom: 5,
};

impl fmt::Display for Phy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{}/{}/{}",
            self.frequency_hz, self.bandwidth_hz, self.sf, self.cr_denom
        )
    }
}

/// The body of `[LORA] init ok`: the chip's hardware and firmware version
/// out of `GetVersion`, and the PHY it was configured on.
pub struct InitOk {
    pub hw: u8,
    pub fw: u16,
    pub phy: Phy,
}

impl fmt::Display for InitOk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "init ok hw={:#04x} fw={:#06x} phy={}",
            self.hw, self.fw, self.phy
        )
    }
}

/// The body of the `[SPI]` line: who holds SPI2 when it is said, the
/// longest radio wait in the window, the panel's chunks, the radio's
/// grants and the panel's yields since boot.
impl fmt::Display for Window {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SPI2 owner={} wait_max_us={} chunks={} grants={} yields={}",
            self.owner.unwrap_or(Owner::Free).word(),
            self.wait_max_us,
            self.chunks,
            self.grants,
            self.yields
        )
    }
}

#[cfg(test)]
mod tests;
