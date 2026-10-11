//! Host assertions for the USB decisions `leviculum-nrf/src/usb.rs` used to
//! carry as comments (leviculum#413).
//!
//! Run: `cargo test -p leviculum-usb-policy --target x86_64-unknown-linux-gnu -- --nocapture`
//! from `leviculum-nrf/` (the workspace defaults to the thumbv7em target).

use leviculum_usb_policy::*;

// ---------------------------------------------------------------------------
// The 1200-baud touch
// ---------------------------------------------------------------------------

/// The request a 1200-baud touch puts on the wire. lnflash sends no bytes
/// itself: `touch_1200` (`lnflash/src/entry.rs:44`) calls `set_baud_1200`
/// (`lnflash/src/sys.rs:64`), which `cfmakeraw`s the termios (8 data bits, no
/// parity, CSTOPB untouched, i.e. one stop bit) and sets B1200 both ways.
/// Linux's cdc-acm driver turns that `tcsetattr` into SET_LINE_CODING
/// (`acm_tty_set_termios` -> `acm_set_line` in `drivers/usb/class/cdc-acm.c`):
/// bmRequestType 0x21 (`USB_RT_ACM`: class, interface, host-to-device),
/// bRequest 0x20, and the 7-byte LINE_CODING below.
const TOUCH_BM_REQUEST_TYPE: u8 = 0x21;
const TOUCH_LINE_CODING: [u8; 7] = [
    0xB0, 0x04, 0x00, 0x00, // dwDTERate = 1200, little-endian
    0x00, // bCharFormat: 1 stop bit
    0x00, // bParityType: none
    0x08, // bDataBits: 8
];

/// The lnflash request, decoded the way embassy-usb decodes it.
fn touch() -> (u8, u8, u8, [u8; 7]) {
    let (direction, kind, recipient) = split_bm_request_type(TOUCH_BM_REQUEST_TYPE);
    assert_eq!(direction, 0, "SET_LINE_CODING is host-to-device");
    (kind, recipient, REQ_SET_LINE_CODING, TOUCH_LINE_CODING)
}

#[test]
fn the_lnflash_touch_resets() {
    let (kind, recipient, request, data) = touch();
    assert_eq!(line_coding_rate(&data), Some(1200));
    assert!(is_bootloader_touch(kind, recipient, request, &data));
    println!("TOUCH bm=0x21 req=0x20 len=7 rate=1200 -> reset");
}

/// Each of the four gating terms wrong on its own, the rest exactly the
/// lnflash request: none of them resets.
#[test]
fn each_gating_term_wrong_alone_does_not_reset() {
    let (kind, recipient, request, data) = touch();

    // Request type: standard (0), vendor (2), reserved (3).
    for wrong in [0u8, 2, 3] {
        assert!(
            !is_bootloader_touch(wrong, recipient, request, &data),
            "request type {wrong}"
        );
    }
    // Recipient: device (0), endpoint (2), other (3), reserved (4).
    for wrong in [0u8, 2, 3, 4] {
        assert!(
            !is_bootloader_touch(kind, wrong, request, &data),
            "recipient {wrong}"
        );
    }
    // Request code: SEND_ENCAPSULATED_COMMAND (0x00), GET_LINE_CODING (0x21),
    // SET_CONTROL_LINE_STATE (0x22), SEND_BREAK (0x23).
    for wrong in [0x00u8, 0x21, 0x22, 0x23] {
        assert!(
            !is_bootloader_touch(kind, recipient, wrong, &data),
            "request 0x{wrong:02x}"
        );
    }
    // Payload length: every truncation of the LINE_CODING, including the
    // 4-byte one that would still hold a whole 1200 rate.
    for len in 0..LINE_CODING_LEN {
        assert!(
            !is_bootloader_touch(kind, recipient, request, &data[..len]),
            "payload of {len} bytes"
        );
    }
}

/// The rate is strict equality: one baud either side is a terminal that
/// rounded, not a flasher.
#[test]
fn a_neighbouring_rate_does_not_reset() {
    let (kind, recipient, request, _) = touch();
    for rate in [1199u32, 1201, 1215, 2400, 115_200, 0, 1200 << 8] {
        let mut data = TOUCH_LINE_CODING;
        data[..4].copy_from_slice(&rate.to_le_bytes());
        assert!(
            !is_bootloader_touch(kind, recipient, request, &data),
            "rate {rate}"
        );
    }
}

/// The 1201 case as its own line, because the instruction names it.
#[test]
fn a_1201_baud_line_coding_does_not_reset() {
    let (kind, recipient, request, _) = touch();
    let data = [0xB1, 0x04, 0x00, 0x00, 0x00, 0x00, 0x08];
    assert_eq!(line_coding_rate(&data), Some(1201));
    assert!(!is_bootloader_touch(kind, recipient, request, &data));
}

/// What does NOT gate: the framing bytes after the rate, and a payload longer
/// than LINE_CODING. The firmware only ever read the rate; a 1200 touch with
/// 2 stop bits or a parity set is still a touch.
#[test]
fn only_the_rate_of_the_line_coding_is_read() {
    let (kind, recipient, request, _) = touch();
    let odd_framing = [0xB0, 0x04, 0x00, 0x00, 0x02, 0x02, 0x07];
    assert!(is_bootloader_touch(kind, recipient, request, &odd_framing));
    let long = [0xB0, 0x04, 0x00, 0x00, 0x00, 0x00, 0x08, 0xFF];
    assert!(is_bootloader_touch(kind, recipient, request, &long));
}

// ---------------------------------------------------------------------------
// The control-envelope answer windows
// ---------------------------------------------------------------------------

/// SF10 at 62.5 kHz, CR 4/5: the slow profile `CONFIG_APPLY_WITHIN`'s comment
/// names (`leviculum-nrf/src/usb.rs`), whose deferral bound is "4.8 s".
const SLOW_SF10: Modulation = Modulation {
    bandwidth_hz: 62_500,
    sf: 10,
    cr: 5,
};

/// The three windows are today's constants, and they are what the firmware
/// re-exports (`leviculum-nrf/src/usb.rs` builds its `Duration`s from these).
#[test]
fn the_windows_are_todays_constants() {
    assert_eq!(CONFIG_DELIVER_WITHIN_MS, 500);
    assert_eq!(CONFIG_APPLY_WITHIN_MS, 1_200);
    assert_eq!(FRAME_TIMEOUT_MS, 500);
}

/// The derivation the comment states, recomputed: one maximum-size frame at
/// the default PHY is ~730 ms, at SF10/62.5 kHz it is ~4.8 s, and the ratio is
/// the airtime ratio. These are equalities, so a change to any term of the
/// derivation (payload size, preamble, airtime formula) turns them red.
#[test]
fn the_deferral_bound_is_one_max_frame_at_the_live_modulation() {
    let default = retune_deferral_bound_ms(DEFAULT_MODULATION);
    let slow = retune_deferral_bound_ms(SLOW_SF10);
    println!(
        "DEFERRAL default(sf=8 bw=125000 cr=5)={default}ms \
         slow(sf=10 bw=62500 cr=5)={slow}ms ratio_x1000={}",
        slow * 1_000 / default
    );
    assert_eq!(default, 728, "the comment's ~730 ms at SF8/125 kHz");
    assert_eq!(slow, 4_756, "the comment's 4.8 s at SF10/62.5 kHz");
    // SF8 -> SF10 is 4x the symbol time, 125 -> 62.5 kHz another 2x; the
    // derived preamble and the payload symbol count bring it to 6.53x.
    assert_eq!(slow * 1_000 / default, 6_532);
}

/// The default deferral fits the apply window: an ordinary retune is acked on
/// the first attempt.
#[test]
fn the_default_deferral_is_acked_first_time() {
    assert!(retune_deferral_bound_ms(DEFAULT_MODULATION) <= CONFIG_APPLY_WITHIN_MS);
    assert_eq!(apply_answer(DEFAULT_MODULATION), ApplyAnswer::Acked);
}

/// The slow profile outgrows the window, and the window does NOT grow with
/// it: it is answered busy and acked on the host's retry, exactly as the
/// comment says.
#[test]
fn the_slow_deferral_is_answered_busy_not_waited_out() {
    assert!(retune_deferral_bound_ms(SLOW_SF10) > CONFIG_APPLY_WITHIN_MS);
    assert_eq!(apply_answer(SLOW_SF10), ApplyAnswer::BusyThenAcked);
}

/// Why the window cannot be the deferral bound: at the slow profile a window
/// that waited the bound out would blow the host's answer window, and lnsd
/// would count the answer as lost rather than busy.
#[test]
fn waiting_out_the_slow_deferral_would_miss_the_host() {
    let waited = CONFIG_DELIVER_WITHIN_MS + retune_deferral_bound_ms(SLOW_SF10);
    assert!(waited > LNSD_CONFIG_ACK_TIMEOUT_MS);
    assert!(waited > LNFLASH_ACK_WITHIN_MS);
}

/// Delivery plus apply stays inside the tightest host window (lnsd, 2 s), and
/// delivery alone inside lnflash's 3.5 s, so a refusal always reaches the
/// host. The slack is in the record, not only the verdict.
#[test]
fn the_envelope_fits_the_host_answer_windows() {
    let envelope = CONFIG_DELIVER_WITHIN_MS + CONFIG_APPLY_WITHIN_MS;
    assert!(envelope <= LNSD_CONFIG_ACK_TIMEOUT_MS);
    assert_eq!(LNSD_CONFIG_ACK_TIMEOUT_MS - envelope, 300);
    assert_eq!(envelope, control_envelope_ms());
    // Delivery alone, and so the whole envelope, inside lnflash's window.
    assert!(envelope < LNFLASH_ACK_WITHIN_MS);
}

/// `FRAME_TIMEOUT_MS`'s comment: a 167-byte frame delivered byte by byte at
/// 115200 baud 8N1 is 14.5 ms, and 500 ms covers it 34 times over.
#[test]
fn the_frame_timeout_covers_a_byte_by_byte_frame() {
    assert_eq!(serial_frame_us(167, 115_200), 14_497);
    assert_eq!(frame_timeout_margin(167, 115_200), 34);
    // The modulation does not enter it: the timeout is the serial line's.
    assert!(frame_timeout_margin(167, 115_200) >= 33);
}

/// Degenerate inputs are answered, not divided by.
#[test]
fn degenerate_inputs_are_answered() {
    assert_eq!(serial_frame_us(167, 0), u64::MAX);
    assert_eq!(frame_timeout_margin(167, 0), 0);
    let none = Modulation {
        bandwidth_hz: 0,
        sf: 8,
        cr: 5,
    };
    assert_eq!(retune_deferral_bound_ms(none), 1);
    assert_eq!(line_coding_rate(&[0xB0, 0x04, 0x00]), None);
}

// ---------------------------------------------------------------------------
// The duty-hold notice (leviculum#501)
// ---------------------------------------------------------------------------

fn decoded(frame: &[u8]) -> leviculum_core::envelope::DutyHoldWire {
    let f = leviculum_core::envelope::decode_frame(frame).expect("an envelope");
    assert_eq!(f.frame_type, leviculum_core::envelope::TYPE_DUTY_HOLD);
    leviculum_core::envelope::decode_duty_hold_payload(f.payload).expect("a duty-hold payload")
}

/// One frame per edge, held then lifted, each carrying the ledger it was
/// read at; nothing between the edges and nothing at boot.
#[test]
fn the_notice_sends_one_frame_on_each_edge_and_none_between() {
    let mut notice = DutyHoldNotice::new();
    assert_eq!(
        notice.edge(false, 0),
        None,
        "a board boots unheld and says nothing"
    );

    let held = notice
        .edge(true, 36_000)
        .expect("the hold opening is an edge");
    let held = decoded(&held);
    assert!(held.held);
    assert_eq!(held.lt_ms, 36_000);
    assert_eq!(
        notice.edge(true, 36_100),
        None,
        "a wake inside the hold is no edge"
    );

    let lifted = notice.edge(false, 35_900).expect("the lift is an edge");
    let lifted = decoded(&lifted);
    assert!(!lifted.held);
    assert_eq!(lifted.lt_ms, 35_900);
    assert_eq!(notice.edge(false, 35_000), None);
}

/// The media-query answer states the hold as it stands, every time, and
/// counts as telling: an edge to that same state afterwards sends nothing.
#[test]
fn the_query_answer_states_the_hold_and_counts_as_told() {
    let mut notice = DutyHoldNotice::new();
    assert!(!decoded(&notice.state(false, 0)).held);
    assert!(decoded(&notice.state(true, 36_000)).held);
    assert!(
        decoded(&notice.state(true, 36_000)).held,
        "asked twice, answered twice"
    );
    assert_eq!(notice.edge(true, 36_000), None);
    assert!(notice.edge(false, 1).is_some());
}

// ---------------------------------------------------------------------------
// The commanded reset (leviculum#359)
// ---------------------------------------------------------------------------

/// A reset frame just classified, on a board running the SoftDevice with a
/// host reading the port.
fn facts() -> ResetFacts {
    ResetFacts {
        softdevice_enabled: true,
        ble_links: 0,
        host_present: true,
        since_command_ms: 0,
    }
}

fn steps(facts: &ResetFacts) -> Vec<ResetStep> {
    reset_plan(facts).steps().to_vec()
}

/// The order the firmware has always run, with the deadline the ack never
/// had: line, ack, 100 ms, reset.
const TODAY_WITH_DEADLINE: [ResetStep; 4] = [
    ResetStep::Announce,
    ResetStep::Ack { within_ms: 1_000 },
    ResetStep::Drain { ms: 100 },
    ResetStep::SystemReset,
];

#[test]
fn the_bounds_are_the_numbers_the_host_windows_allow() {
    assert_eq!(RESET_ACK_WITHIN_MS, 1_000);
    assert_eq!(RESET_DRAIN_MS, 100);
    assert_eq!(PERICULUM_RESET_ACK_WINDOW_MS, 2_000);
    assert_eq!(PERICULUM_REENUM_GONE_WINDOW_MS, 5_000);
    // `RESET_ACK_WITHIN_MS + RESET_DRAIN_MS < PERICULUM_RESET_ACK_WINDOW_MS`
    // is a build-time assert in the crate; the plan-level form is below.
}

/// The census's link-up arm: a board with a BLE link, a host reading. The
/// link adds no step.
#[test]
fn a_board_with_a_link_up_resets_in_todays_order() {
    for links in 1..=4u8 {
        let f = ResetFacts {
            ble_links: links,
            ..facts()
        };
        assert_eq!(steps(&f), TODAY_WITH_DEADLINE, "ble_links={links}");
    }
}

/// The census's quiet arm: no link, a host reading.
#[test]
fn a_quiet_board_resets_in_todays_order() {
    assert_eq!(steps(&facts()), TODAY_WITH_DEADLINE);
}

/// The dead state's reading: a host holds DTR up and never takes the ack.
/// The plan's waits are all bounded, so the board leaves the bus within
/// 1.1 s of the command, inside periculum's first ack attempt.
#[test]
fn an_ack_that_never_comes_still_ends_in_a_reset_within_the_host_window() {
    let plan = reset_plan(&facts());
    assert_eq!(plan.worst_case_ms(), 1_100);
    assert!(plan.worst_case_ms() < PERICULUM_RESET_ACK_WINDOW_MS);
    assert!(plan.worst_case_ms() < PERICULUM_REENUM_GONE_WINDOW_MS);
}

/// No host: nobody can read an ack, so none is written; the drain and the
/// reset stay.
#[test]
fn with_no_host_the_ack_is_not_written() {
    let f = ResetFacts {
        host_present: false,
        ..facts()
    };
    assert_eq!(
        steps(&f),
        [
            ResetStep::Announce,
            ResetStep::Drain { ms: 100 },
            ResetStep::SystemReset
        ]
    );
    assert_eq!(reset_plan(&f).worst_case_ms(), 100);
}

/// The ack gets what is left of its window, and nothing once it is spent.
#[test]
fn the_ack_gets_what_is_left_of_its_window() {
    let at = |since_command_ms| {
        steps(&ResetFacts {
            since_command_ms,
            ..facts()
        })
    };
    assert_eq!(at(1).get(1), Some(&ResetStep::Ack { within_ms: 999 }));
    assert_eq!(at(999).get(1), Some(&ResetStep::Ack { within_ms: 1 }));
    for spent in [1_000, 1_001, u64::MAX] {
        assert!(
            !at(spent).iter().any(|s| matches!(s, ResetStep::Ack { .. })),
            "since_command_ms={spent}"
        );
    }
}

/// The SoftDevice state adds no step: `sd_nvic_SystemReset` is the same
/// reset `sys_reset` makes.
#[test]
fn the_softdevice_state_does_not_change_the_plan() {
    for host_present in [false, true] {
        let on = ResetFacts {
            host_present,
            ..facts()
        };
        let off = ResetFacts {
            softdevice_enabled: false,
            ..on
        };
        assert_eq!(steps(&on), steps(&off), "host_present={host_present}");
    }
}

/// Every combination: the line first, the reset last, exactly once each,
/// the drain right before the reset, an ack only before the drain, and no
/// plan that keeps the board on the bus past the host's ack window.
#[test]
fn every_plan_starts_with_the_line_and_ends_in_the_reset() {
    for softdevice_enabled in [false, true] {
        for ble_links in [0u8, 1, 4, u8::MAX] {
            for host_present in [false, true] {
                for since_command_ms in [0u64, 500, 999, 1_000, 60_000] {
                    let f = ResetFacts {
                        softdevice_enabled,
                        ble_links,
                        host_present,
                        since_command_ms,
                    };
                    let plan = reset_plan(&f);
                    let s = plan.steps();
                    assert_eq!(s.first(), Some(&ResetStep::Announce), "{f:?}");
                    assert_eq!(s.last(), Some(&ResetStep::SystemReset), "{f:?}");
                    assert_eq!(
                        s.get(s.len() - 2),
                        Some(&ResetStep::Drain { ms: RESET_DRAIN_MS }),
                        "{f:?}"
                    );
                    assert_eq!(
                        s.iter().filter(|x| **x == ResetStep::SystemReset).count(),
                        1,
                        "{f:?}"
                    );
                    assert!(plan.worst_case_ms() < PERICULUM_RESET_ACK_WINDOW_MS);
                    println!("RESET_PLAN {f:?} -> {s:?}");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The transport reader
// ---------------------------------------------------------------------------

use leviculum_core::envelope::{self, ControlAction};
use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};

/// The transport port's deframer bound, `SERIAL_HW_MTU` in `usb.rs`.
const SERIAL_HW_MTU: usize = 564;

/// One read outcome at a time through the step the firmware takes, the
/// deframer handled as `retic_serial_task` handles it. Returns the frames
/// that came out, classified the way the task classifies them.
enum Read<'a> {
    Bytes(&'a [u8]),
    Disconnect,
    Timeout,
}

fn run_reader(reads: &[Read]) -> Vec<ControlAction> {
    let mut deframer = Deframer::with_max_frame(SERIAL_HW_MTU);
    let mut out = Vec::new();
    for read in reads {
        let outcome = match read {
            Read::Bytes(_) => ReadOutcome::Bytes,
            Read::Disconnect => ReadOutcome::Disconnected,
            Read::Timeout => ReadOutcome::TimedOut,
        };
        match reader_step(outcome, deframer.is_in_frame()) {
            ReaderStep::Deframe => {
                let Read::Bytes(bytes) = read else {
                    unreachable!("only bytes deframe")
                };
                for r in deframer.process(bytes) {
                    if let DeframeResult::Frame(data) = r {
                        out.push(envelope::classify_control_frame(
                            &data,
                            &[envelope::TYPE_RESET],
                        ));
                    }
                }
            }
            // The firmware breaks to `wait_connection` and resets the
            // deframer when the next session starts.
            ReaderStep::Rewait => deframer.reset(),
            ReaderStep::DropPartial => deframer.reset(),
            ReaderStep::Idle => {}
        }
    }
    out
}

/// The envelope reset frame on the wire, HDLC-framed as the host writes it.
fn reset_on_the_wire() -> Vec<u8> {
    let mut wire = Vec::new();
    frame(&envelope::encode_reset(), &mut wire);
    wire
}

#[test]
fn the_read_timeout_is_the_frame_timeout_only_inside_a_frame() {
    assert_eq!(read_timeout_ms(true), FRAME_TIMEOUT_MS);
    assert_eq!(read_timeout_ms(true), 500);
    assert_eq!(read_timeout_ms(false), IDLE_READ_TIMEOUT_MS);
    assert_eq!(IDLE_READ_TIMEOUT_MS, 3_600 * 1_000);
}

#[test]
fn a_disconnect_ends_the_session_in_or_out_of_a_frame() {
    assert_eq!(
        reader_step(ReadOutcome::Disconnected, true),
        ReaderStep::Rewait
    );
    assert_eq!(
        reader_step(ReadOutcome::Disconnected, false),
        ReaderStep::Rewait
    );
    assert_eq!(reader_step(ReadOutcome::Bytes, true), ReaderStep::Deframe);
    assert_eq!(reader_step(ReadOutcome::Bytes, false), ReaderStep::Deframe);
}

#[test]
fn a_timeout_drops_only_an_open_frame() {
    assert_eq!(
        reader_step(ReadOutcome::TimedOut, true),
        ReaderStep::DropPartial
    );
    assert_eq!(reader_step(ReadOutcome::TimedOut, false), ReaderStep::Idle);
}

/// leviculum#359: a host that closes the port in the middle of a reset frame
/// and writes the whole frame again after it reopens. The torn half must not
/// reach the classifier, and the whole frame must still read as a reset; a
/// half carried across the session would close as a garbage frame on the
/// retry's opening flag and swallow the retry with it.
#[test]
fn a_reset_frame_torn_by_a_disconnect_resets_on_the_retry() {
    let wire = reset_on_the_wire();
    let (torn, _) = wire.split_at(wire.len() / 2);
    let actions = run_reader(&[Read::Bytes(torn), Read::Disconnect, Read::Bytes(&wire)]);
    assert_eq!(actions, vec![ControlAction::Reset]);
    println!(
        "TORN_RESET wire={} torn={} -> {:?}",
        wire.len(),
        torn.len(),
        actions
    );
}

/// The same tear with no read error, a host that just stops writing: the
/// frame timeout drops the half and the retry is a reset.
#[test]
fn a_reset_frame_torn_by_a_silent_host_resets_on_the_retry() {
    let wire = reset_on_the_wire();
    let (torn, _) = wire.split_at(wire.len() / 2);
    let actions = run_reader(&[Read::Bytes(torn), Read::Timeout, Read::Bytes(&wire)]);
    assert_eq!(actions, vec![ControlAction::Reset]);
}

/// Positive control for the two above: the deframer alone, given the torn
/// half and then the retry with nothing between them, emits the half as a
/// frame and loses the reset. That is the failure the step prevents.
#[test]
fn without_the_step_the_torn_half_swallows_the_retry() {
    let wire = reset_on_the_wire();
    let (torn, _) = wire.split_at(wire.len() / 2);
    let mut deframer = Deframer::with_max_frame(SERIAL_HW_MTU);
    let mut frames = Vec::new();
    for bytes in [torn, &wire[..]] {
        for r in deframer.process(bytes) {
            if let DeframeResult::Frame(data) = r {
                frames.push(envelope::classify_control_frame(
                    &data,
                    &[envelope::TYPE_RESET],
                ));
            }
        }
    }
    assert!(!frames.contains(&ControlAction::Reset), "{frames:?}");
}

/// A timeout with no frame open changes nothing: the next frame is read
/// whole.
#[test]
fn an_idle_timeout_leaves_the_next_frame_alone() {
    let wire = reset_on_the_wire();
    let actions = run_reader(&[Read::Timeout, Read::Bytes(&wire), Read::Timeout]);
    assert_eq!(actions, vec![ControlAction::Reset]);
}

// ---------------------------------------------------------------------------
// The outgoing queue
// ---------------------------------------------------------------------------

#[test]
fn a_queued_packet_counts_its_unframed_bytes() {
    // A packet whose framing grows it: two bytes that need escaping.
    let packet = [0x7Eu8, 0x7D, 0x01, 0x02];
    let mut framed = Vec::new();
    frame(&packet, &mut framed);
    assert!(framed.len() > packet.len());
    assert_eq!(
        offered(true, packet.len()),
        Offered::Queued {
            counted: packet.len()
        }
    );
}

/// A host that holds DTR up and does not read: the writer pends on the first
/// packet, the queue fills behind it, and the packet past its depth is
/// dropped uncounted.
#[test]
fn the_packet_past_a_full_queue_is_dropped_uncounted() {
    let mut queued = 0usize;
    let mut counted = 0usize;
    let mut full = 0usize;
    for _ in 0..OUTGOING_DEPTH + 3 {
        match offered(queued < OUTGOING_DEPTH, 100) {
            Offered::Queued { counted: n } => {
                queued += 1;
                counted += n;
            }
            Offered::Full => full += 1,
        }
    }
    assert_eq!(queued, 8);
    assert_eq!(counted, 800);
    assert_eq!(full, 3);
}

// ---------------------------------------------------------------------------
// The debug port's drain
// ---------------------------------------------------------------------------

#[test]
fn the_drain_bounds_are_todays_numbers() {
    assert_eq!(DEBUG_POLL_MS, 100);
    assert_eq!(DEBUG_WRITE_WITHIN_MS, 50);
    assert_eq!(HEADLESS_DRAIN_OPEN_MS, 30_000);
    // That a stalled write ends inside one poll period is a build-time
    // assert in the crate.
}

#[test]
fn the_debug_port_is_written_only_while_dtr_is_up() {
    for since_boot_ms in [0, 29_999, 30_000, 3_600_000] {
        assert_eq!(drain_wake(true, since_boot_ms), DrainWake::Drain);
        assert_ne!(drain_wake(false, since_boot_ms), DrainWake::Drain);
    }
}

/// The wake order a headless boot produces, every 100 ms: the gate stays
/// shut until 30 s and opens on the first wake at or past it.
#[test]
fn a_headless_boot_opens_the_gate_at_thirty_seconds() {
    let first_open = (0..)
        .map(|wake: u64| wake * DEBUG_POLL_MS)
        .find(|&t| drain_wake(false, t) == DrainWake::OpenGate);
    assert_eq!(first_open, Some(HEADLESS_DRAIN_OPEN_MS));
    assert_eq!(
        drain_wake(false, HEADLESS_DRAIN_OPEN_MS - 1),
        DrainWake::Wait
    );
}

/// A host that attaches at 3 s opens the gate then, not at 30 s.
#[test]
fn the_first_host_opens_the_gate_before_the_headless_deadline() {
    assert_eq!(drain_wake(false, 2_900), DrainWake::Wait);
    assert_eq!(drain_wake(true, 3_000), DrainWake::Drain);
}

#[test]
fn a_write_that_did_not_land_ends_the_drain() {
    assert!(drain_continues(DebugWrite::Written));
    assert!(!drain_continues(DebugWrite::Failed));
    assert!(!drain_continues(DebugWrite::TimedOut));
}

// ---------------------------------------------------------------------------
// The debug port's commands
// ---------------------------------------------------------------------------

#[test]
fn each_command_byte_asks_for_its_command() {
    let none = DebugCommands::default();
    assert_eq!(
        debug_commands(b"p"),
        DebugCommands {
            postmortem: true,
            ..none
        }
    );
    assert_eq!(
        debug_commands(b"s"),
        DebugCommands {
            stack_reset: true,
            ..none
        }
    );
    assert_eq!(
        debug_commands(b"c"),
        DebugCommands {
            heap_census: true,
            ..none
        }
    );
}

#[test]
fn a_terminal_line_runs_each_command_once() {
    // `echo p > /dev/ttyACM1` and a held key both reach the port as one
    // packet; the query runs once, not once per byte.
    let asked = debug_commands(b"ppp\r\n");
    assert!(asked.postmortem);
    assert!(!asked.stack_reset && !asked.heap_census);
    let all = debug_commands(b"c s p\n");
    assert!(all.postmortem && all.stack_reset && all.heap_census);
}

#[test]
fn stray_input_asks_for_nothing() {
    assert_eq!(debug_commands(b"\r\n"), DebugCommands::default());
    assert_eq!(debug_commands(b"PSC"), DebugCommands::default());
    assert_eq!(debug_commands(b""), DebugCommands::default());
}

// ---------------------------------------------------------------------------
// Where a radio config goes
// ---------------------------------------------------------------------------

use leviculum_core::envelope::{
    radio_config_answer, ConfigDelivery, Persist, REFUSE_BUSY, REFUSE_NOT_RUNNING, REFUSE_PERSIST,
};
use leviculum_core::rnode::RadioConfigWire;

/// The rig's default PHY, as lnsd sends it on every connect.
fn eu_default() -> RadioConfigWire {
    RadioConfigWire {
        frequency_hz: 869_463_000,
        bandwidth_hz: 125_000,
        sf: 8,
        cr: 5,
        tx_power_dbm: 14,
        preamble_len: 18,
        csma_enabled: true,
        radio_silent: false,
        st_alock: 0,
        lt_alock: 0,
        lt_alock_present: false,
        silence_lease_s: 0,
    }
}

/// The refusal reason byte an answer carries, the last byte of the frame.
fn reason(answer: &[u8]) -> u8 {
    *answer.last().expect("an answer has bytes")
}

#[test]
fn a_boot_without_lora_takes_the_config_to_the_page_not_the_channel() {
    assert_eq!(config_route(false), ConfigRoute::Store);
    assert_eq!(config_route(true), ConfigRoute::Channel);
}

#[test]
fn a_lora_off_boot_whose_page_write_lands_answers_not_running() {
    let delivery = stored_answer(Persist::Durable);
    assert_eq!(delivery, ConfigDelivery::Stored);
    assert_eq!(reason(&radio_config_answer(delivery)), REFUSE_NOT_RUNNING);
}

#[test]
fn a_lora_off_boot_whose_page_write_is_lost_answers_not_persisted() {
    let delivery = stored_answer(Persist::Lost);
    assert_eq!(delivery, ConfigDelivery::StoreFailed);
    assert_eq!(reason(&radio_config_answer(delivery)), REFUSE_PERSIST);
}

#[test]
fn a_retry_of_the_queued_config_waits_for_its_apply_while_the_task_stalls() {
    // The LoRa task has not drained the slot; the host's busy retry is the
    // same config, and it must not be refused as undeliverable.
    assert_eq!(
        full_slot(Some(eu_default()), eu_default()),
        FullSlot::AlreadyQueued
    );
}

#[test]
fn a_different_config_behind_a_stalled_task_gets_one_grace_send() {
    let mut wide = eu_default();
    wide.bandwidth_hz = 250_000;
    assert_eq!(full_slot(Some(eu_default()), wide), FullSlot::GraceSend);
    // A grace send that times out is answered busy, so the host retries.
    assert_eq!(
        reason(&radio_config_answer(ConfigDelivery::Undeliverable)),
        REFUSE_BUSY
    );
}

#[test]
fn a_full_slot_with_no_published_config_gets_the_grace_send() {
    // The slot is full but the LoRa task has not published what it holds
    // (the boot window before its first config): never treated as a repeat.
    assert_eq!(full_slot(None, eu_default()), FullSlot::GraceSend);
}
