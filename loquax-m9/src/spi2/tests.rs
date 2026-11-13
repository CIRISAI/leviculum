//! The arbiter's rules (1478b), and the two cores stepped over a full
//! frame with the radio polling every 10 ms.

extern crate std;

use std::vec::Vec;

use super::*;

const CLOCKS: Clocks = Clocks {
    panel_hz: 40_000_000,
    radio_hz: 4_000_000,
};

/// What the bus saw, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ev {
    Clock(u32),
    Select(bool),
    Byte,
}

#[derive(Default)]
struct Bus {
    evs: Vec<Ev>,
    hz: u32,
    selected: bool,
    fail_clock_at: Option<u32>,
}

impl Wire for Bus {
    type Error = &'static str;
    fn clock(&mut self, hz: u32) -> Result<(), Self::Error> {
        if self.fail_clock_at == Some(hz) {
            return Err("clock");
        }
        self.hz = hz;
        self.evs.push(Ev::Clock(hz));
        Ok(())
    }
    fn select(&mut self, selected: bool) {
        self.selected = selected;
        self.evs.push(Ev::Select(selected));
    }
}

impl Bus {
    /// A byte on the bus: only ever with the radio selected at its clock.
    fn byte(&mut self) -> Result<(), &'static str> {
        assert!(self.selected, "a radio byte with the radio not selected");
        assert_eq!(
            self.hz, CLOCKS.radio_hz,
            "a radio byte at the panel's clock"
        );
        self.evs.push(Ev::Byte);
        Ok(())
    }
}

#[test]
fn one_owner_at_a_time() {
    let arb = Arbiter::new();
    assert!(arb.panel_take(10));
    assert_eq!(arb.owner(), Owner::Panel);
    assert!(!arb.radio_take(11), "the radio took a bus the panel holds");
    assert_eq!(arb.owner(), Owner::Panel);
    // The panel yields to the ask, then the radio holds it alone.
    assert!(!arb.panel_take(12));
    arb.panel_release(12);
    assert!(arb.radio_take(13));
    assert_eq!(arb.owner(), Owner::Radio);
    assert!(!arb.panel_take(14), "the panel took a bus the radio holds");
    arb.radio_release();
    assert_eq!(arb.owner(), Owner::Free);
    assert!(arb.panel_take(15));
}

#[test]
fn a_radio_that_asked_goes_before_the_next_chunk() {
    let arb = Arbiter::new();
    assert!(arb.panel_take(100));
    arb.panel_chunk();
    // The radio asks while the chunk runs, and tries again each core 0
    // turn.
    assert!(!arb.radio_take(200));
    assert!(arb.radio_waits(300));
    assert!(!arb.radio_take(5_000));
    // Before its next chunk the panel looks and yields.
    assert!(!arb.panel_take(6_000));
    arb.panel_release(6_000);
    assert!(arb.radio_take(6_100));
    let w = arb.take_window();
    assert_eq!(w.wait_max_us, 5_900, "first ask at 200, granted at 6100");
    assert_eq!((w.grants, w.chunks, w.yields), (1, 1, 1));
    // The window's longest wait starts again; the counts do not.
    arb.radio_release();
    assert_eq!(arb.take_window().wait_max_us, 0);
}

#[test]
fn a_free_bus_goes_to_the_radio_without_a_wait() {
    let arb = Arbiter::new();
    assert!(arb.radio_take(50));
    assert_eq!(arb.take_window().wait_max_us, 0);
    arb.radio_release();
    // The grant cleared the ask: the panel is not held back by it.
    assert!(!arb.radio_waits(51));
    assert!(arb.panel_take(51));
}

#[test]
fn an_ask_nobody_renews_goes_stale() {
    let arb = Arbiter::new();
    assert!(arb.panel_take(1));
    assert!(!arb.radio_take(1_000));
    assert!(!arb.panel_take(1_000 + ASK_FRESH_US - 1));
    // Core 0 went on to a long turn and did not try again: the panel
    // flushes on.
    assert!(arb.panel_take(1_000 + ASK_FRESH_US));
    // The next try asks afresh, and the panel yields again; the wait is
    // counted from the first ask.
    assert!(!arb.radio_take(20_000));
    assert!(!arb.panel_take(20_001));
    arb.panel_release(20_001);
    assert!(arb.radio_take(20_002));
    assert_eq!(arb.take_window().wait_max_us, 19_002);
}

#[test]
fn the_clock_is_the_radios_for_its_transaction_and_the_panels_after() {
    let mut bus = Bus {
        hz: CLOCKS.panel_hz,
        ..Bus::default()
    };
    let r = run(&mut bus, CLOCKS, |b| {
        b.byte()?;
        b.byte()
    });
    assert_eq!(r, Ok(()));
    assert_eq!(
        bus.evs,
        [
            Ev::Clock(CLOCKS.radio_hz),
            Ev::Select(true),
            Ev::Byte,
            Ev::Byte,
            Ev::Select(false),
            Ev::Clock(CLOCKS.panel_hz),
        ]
    );
    assert_eq!(bus.hz, CLOCKS.panel_hz);
    assert!(!bus.selected);
}

#[test]
fn a_failed_transaction_still_deselects_and_restores_the_clock() {
    let mut bus = Bus::default();
    let r: Result<(), _> = run(&mut bus, CLOCKS, |b| {
        b.byte()?;
        Err("spi")
    });
    assert_eq!(r, Err("spi"));
    assert!(!bus.selected);
    assert_eq!(bus.hz, CLOCKS.panel_hz);
    // A clock the radio's could not be set never selects the radio.
    let mut bus = Bus {
        fail_clock_at: Some(CLOCKS.radio_hz),
        ..Bus::default()
    };
    let r = run(&mut bus, CLOCKS, |b| b.byte());
    assert_eq!(r, Err("clock"));
    assert!(!bus.evs.contains(&Ev::Select(true)));
    assert_eq!(bus.evs.last(), Some(&Ev::Clock(CLOCKS.panel_hz)));
}

#[test]
fn chip_select_is_held_only_inside_the_transaction() {
    // Ten transactions back to back: every selection opens after the
    // radio's clock and closes before the panel's.
    let mut bus = Bus::default();
    for _ in 0..10 {
        run(&mut bus, CLOCKS, |b| b.byte()).unwrap();
    }
    let mut selected = false;
    let mut hz = 0;
    for ev in &bus.evs {
        match *ev {
            Ev::Clock(h) => {
                assert!(!selected, "the clock moved with the radio selected");
                hz = h;
            }
            Ev::Select(s) => {
                if s {
                    assert_eq!(hz, CLOCKS.radio_hz);
                }
                selected = s;
            }
            Ev::Byte => assert!(selected),
        }
    }
    assert!(!selected);
}

#[test]
fn a_full_frame_alone_takes_its_bytes_time() {
    let alone = interleave(Model {
        radio_every_us: 0,
        ..BOARD
    });
    assert_eq!(alone.flush_us, FULL_FRAME_CHUNKS_US.iter().sum::<u32>());
    assert_eq!((alone.grants, alone.yields, alone.chunks), (0, 0, 5));
}

#[test]
fn the_radio_polling_every_10_ms_waits_at_most_a_chunk() {
    let alone = interleave(Model {
        radio_every_us: 0,
        ..BOARD
    })
    .flush_us;
    let chunk_max = *FULL_FRAME_CHUNKS_US.iter().max().unwrap();
    // Every phase of the radio's period against the flush.
    let mut worst = Interleaved::default();
    let mut added_max = 0;
    for phase in (0..10_000).step_by(97) {
        let with = interleave(Model {
            radio_phase_us: phase,
            ..BOARD
        });
        // A 31 ms flush holds three polls or four; the model stops at
        // the flush's end, the last one may be after it.
        assert!(with.grants >= 2, "phase {phase}: {with:?}");
        assert!(
            with.wait_max_us <= chunk_max + BOARD.io_turn_us,
            "phase {phase}: the radio waited {} us",
            with.wait_max_us
        );
        added_max = added_max.max(with.flush_us - alone);
        if with.wait_max_us > worst.wait_max_us {
            worst = with;
        }
        // The panel never stands still longer than a core 0 turn and a
        // transaction: far inside its 5 s watchdog.
        assert!(
            with.gap_max_us <= BOARD.io_turn_us + BOARD.radio_txn_us + 1,
            "phase {phase}: {with:?}"
        );
    }
    // Each grant inside the flush costs the panel at most a core 0 turn,
    // the transaction and the reopened window.
    let per = BOARD.io_turn_us + BOARD.radio_txn_us + BOARD.reopen_us + 1;
    assert!(added_max <= 4 * per, "added {added_max} us");
    std::println!(
        "[SPI] model wait_max_us={} added_flush_us_max={added_max} gap_max_us={} alone_us={alone}",
        worst.wait_max_us,
        worst.gap_max_us
    );
}

#[test]
fn a_pump_a_turn_holds_the_radio_until_the_turn_sees_the_chunk_end() {
    // The UI loop pumps once a turn; at its 16 ms tick a chunk that ended
    // holds the bus until the next turn. The radio's wait is bounded by a
    // chunk and a turn, not by the flush.
    let turn = 16_000;
    let chunk_max = *FULL_FRAME_CHUNKS_US.iter().max().unwrap();
    let mut wait_max = 0;
    for phase in (0..10_000).step_by(97) {
        let with = interleave(Model {
            radio_phase_us: phase,
            pump_every_us: turn,
            ..BOARD
        });
        assert!(
            with.wait_max_us <= chunk_max + turn + BOARD.io_turn_us,
            "{with:?}"
        );
        wait_max = wait_max.max(with.wait_max_us);
    }
    std::println!("[SPI] model pump_every_us={turn} wait_max_us={wait_max}");
}

#[test]
fn a_radio_that_never_comes_back_does_not_hold_the_flush() {
    // Core 0 asks once, then sits in a turn longer than the flush.
    let stuck = interleave(Model {
        io_turn_us: 100_000,
        radio_phase_us: 3_000,
        ..BOARD
    });
    let alone = interleave(Model {
        radio_every_us: 0,
        ..BOARD
    });
    assert!(
        stuck.flush_us <= alone.flush_us + ASK_FRESH_US + BOARD.reopen_us + 1,
        "{stuck:?}"
    );
}

#[test]
fn the_lines_say_what_the_order_names() {
    let ok = InitOk {
        hw: 0x22,
        fw: 0x0401,
        phy: DEFAULT_PHY,
    };
    assert_eq!(
        std::format!("{ok}"),
        "init ok hw=0x22 fw=0x0401 phy=869463000/125000/8/5"
    );
    let w = Window {
        owner: Some(Owner::Panel),
        wait_max_us: 6_600,
        grants: 3,
        chunks: 40,
        yields: 2,
    };
    assert_eq!(
        std::format!("{w}"),
        "SPI2 owner=panel wait_max_us=6600 chunks=40 grants=3 yields=2"
    );
}
