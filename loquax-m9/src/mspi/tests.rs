//! The two cores of a pull, stepped a microsecond at a time on the host
//! (1563): core 0 writes flash op after op, each op parks core 1, and
//! core 1 flushes a frame in chunks by DMA. The bench's numbers: a page
//! write about a millisecond, an erase 45 ms every tenth op, a few
//! microseconds of core 0's own work between two ops, and core 1 slower
//! than that to run again after its park (the cache it refills).

extern crate std;

use super::*;

/// Core 0's work between one op's end and the next one's announce.
const GAP_US: u32 = 3;
/// Core 1's wake after a park, before its next instruction.
const RESUME_US: u32 = 30;
/// A page's write, and an erase every tenth op.
const WRITE_US: u32 = 1_000;
const ERASE_US: u32 = 45_000;
/// The panel's chunk: 51 rows of 320 at 16 bits, at 40 MHz.
const CHUNK_BYTES: usize = 51 * 320 * 2;
/// The bound the order sets: a flush waits no longer than a second.
const BOUND_US: u32 = 1_000_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Io {
    /// Before the op: the yield to a DMA that asked (when `yields`).
    Yield,
    /// Announced, waiting for a DMA's end.
    Waits,
    /// The op runs, core 1 parked, until then.
    Op(u32),
    /// Core 0's own work until then.
    Gap(u32),
}

/// What a flush of `chunks` chunks took under a pull, and how long it
/// waited for the flash, both in microseconds; `None` when it was not
/// out after [`BOUND_US`]. `yields`: core 0 owes the gap a DMA asked
/// for (1563); without it, the arbitration as 1507 had it.
fn flush_under_pull(chunks: u32, yields: bool) -> (Option<u32>, u32) {
    let arb = Arbiter::new();
    let transfer = (CHUNK_BYTES as u32 * 8) / 40;
    let announced = transfer_us(CHUNK_BYTES, 40);
    let mut io = Io::Yield;
    let mut ops = 0u32;
    // Core 1 runs again at this time after a park.
    let mut ui_at = 0u32;
    let mut inflight: Option<u32> = None;
    let mut sent = 0u32;
    let mut waited = 0u32;
    // A start at 1 keeps the clock off 0, which means "none" to it.
    for now in 1..BOUND_US {
        io = match io {
            Io::Yield if yields && arb.flash_yields(now) => Io::Yield,
            Io::Yield => {
                arb.flash_announce();
                Io::Waits
            }
            Io::Waits if arb.flash_waits(now) => Io::Waits,
            Io::Waits => {
                ops += 1;
                let us = if ops.is_multiple_of(10) {
                    ERASE_US
                } else {
                    WRITE_US
                };
                Io::Op(now + us)
            }
            Io::Op(end) if now < end => Io::Op(end),
            Io::Op(_) => {
                arb.flash_done();
                ui_at = now + RESUME_US;
                Io::Gap(now + GAP_US)
            }
            Io::Gap(end) if now < end => Io::Gap(end),
            Io::Gap(_) => Io::Yield,
        };
        // The DMA runs on while core 1 is parked.
        if inflight.is_some_and(|end| now >= end) {
            inflight = None;
            arb.dma_done();
            sent += 1;
            if sent == chunks {
                return (Some(now), waited);
            }
        }
        let parked = matches!(io, Io::Op(_)) || now < ui_at;
        if parked {
            if inflight.is_none() {
                waited += 1;
            }
            continue;
        }
        if inflight.is_none() {
            if arb.dma_begin(now, announced) {
                inflight = Some(now + transfer);
            } else {
                waited += 1;
            }
        }
    }
    (None, waited)
}

/// 1563, the red test: under the pull's continuous writes the 1507
/// arbitration never lets a chunk start, and the flush waits past a
/// second (on the board, past MWDT1's five).
#[test]
fn without_the_yield_a_flush_starves_under_a_pull() {
    let (took, waited) = flush_under_pull(5, false);
    std::println!("NO-YIELD flush took={took:?} waited_us={waited}");
    assert_eq!(
        took, None,
        "the flush got out: the model lost the starvation"
    );
    assert!(waited > BOUND_US - 10);
}

/// 1563: with the yield the same pull lets every chunk through, one flash
/// op at worst ahead of each, and the frame is out well inside the bound.
#[test]
fn a_flush_gets_a_gap_between_two_flash_ops() {
    let (took, waited) = flush_under_pull(5, true);
    std::println!("YIELD flush took_us={took:?} waited_us={waited}");
    let took = took.expect("the flush is out");
    // Five chunks of 6.5 ms, and at worst an erase ahead of each.
    assert!(took < 5 * (6_528 + ERASE_US + RESUME_US + 100), "{took}");
    assert!(took < BOUND_US / 2, "{took}");
}

/// A DMA that asked and then went on to other work holds the flash once,
/// for [`YIELD_US`], and the ops after it not at all.
#[test]
fn an_ask_nobody_takes_up_costs_one_bounded_wait() {
    let arb = Arbiter::new();
    arb.flash_announce();
    assert!(!arb.dma_begin(100, 500), "the flash is announced");
    arb.flash_done();
    assert!(arb.flash_yields(101));
    assert!(arb.flash_yields(100 + YIELD_US - 1));
    assert!(!arb.flash_yields(100 + YIELD_US), "the bound");
    assert!(!arb.flash_yields(100 + YIELD_US + 1), "the ask is dropped");
}

/// The ask's time is its first: a UI core that keeps asking while every
/// op runs does not move the bound on.
#[test]
fn an_ask_keeps_its_first_time() {
    let arb = Arbiter::new();
    arb.flash_announce();
    assert!(!arb.dma_begin(10, 500));
    assert!(!arb.dma_begin(1_500, 500));
    arb.flash_done();
    assert!(!arb.flash_yields(10 + YIELD_US));
}

/// A DMA that starts takes its ask back, and the flash that sees no ask
/// sees the transfer's end and waits for it.
#[test]
fn a_started_dma_holds_the_flash_until_its_end() {
    let arb = Arbiter::new();
    arb.flash_announce();
    assert!(!arb.dma_begin(10, 500));
    arb.flash_done();
    assert!(arb.flash_yields(20));
    assert!(arb.dma_begin(30, 500));
    assert!(!arb.flash_yields(31), "the ask is taken back");
    arb.flash_announce();
    assert!(arb.flash_waits(100));
    assert!(!arb.flash_waits(530), "the announced end");
    arb.dma_done();
    assert!(!arb.flash_waits(101));
    assert!(!arb.dma_begin(102, 500), "the flash is announced");
}

/// The clock wraps: an end just past the wrap is still ahead.
#[test]
fn the_clock_wraps() {
    let arb = Arbiter::new();
    assert!(arb.dma_begin(u32::MAX - 10, 100));
    arb.flash_announce();
    assert!(arb.flash_waits(u32::MAX - 5));
    assert!(arb.flash_waits(20));
    assert!(!arb.flash_waits(100));
}

/// What the flash waits for: twice the transfer, and 100 µs.
#[test]
fn transfer_us_is_twice_the_bits_and_a_tenth() {
    assert_eq!(transfer_us(CHUNK_BYTES, 40), 2 * 6_528 + 100);
    assert_eq!(transfer_us(0, 0), 100);
}
