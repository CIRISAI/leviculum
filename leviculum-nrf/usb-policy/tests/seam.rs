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
