use super::*;

/// Build a framed NMEA sentence with a computed checksum, so fixtures
/// cannot silently carry a wrong checksum (the classic documentation
/// sentence below doubles as the positive control that this helper
/// matches the parser's checksum algorithm).
fn nmea(body: &str) -> Vec<u8> {
    let ck = body.bytes().fold(0u8, |a, b| a ^ b);
    format!("${body}*{ck:02X}\r\n").into_bytes()
}

/// Valid RMC (status A): 1994-11-19T22:54:46Z, the classic NMEA
/// documentation sentence body.
fn valid_rmc() -> Vec<u8> {
    nmea("GPRMC,225446,A,4916.45,N,12311.12,W,000.5,054.7,191194,020.3,E")
}

/// Populated but not-valid RMC (status V, no FAA mode field → mode
/// falls back to NotValid).
fn invalid_rmc() -> Vec<u8> {
    nmea("GPRMC,225446,V,4916.45,N,12311.12,W,000.5,054.7,191194,020.3,E")
}

/// Cold-start void RMC as a real receiver emits it indoors before any
/// satellite: every data field empty, mode N. Parses checksum-clean to
/// `RMC(None)`.
fn void_rmc() -> Vec<u8> {
    nmea("GNRMC,,V,,,,,,,,,,N")
}

/// GGA with a fix solution (quality 1, 7 sats).
fn fix_gga() -> Vec<u8> {
    nmea("GPGGA,225446,4916.45,N,12311.12,W,1,07,1.0,9.0,M,46.9,M,,")
}

/// Line noise: no '$', no structure. What a wrong-baud stream looks
/// like after the UART mangles the framing.
const GARBAGE: &[u8] = &[0xff, 0x00, 0x5a, 0xa5, 0x13, 0x37, 0x00, 0xfe, 0x80, 0x7f];

fn feed(m: &mut PresenceMachine, bytes: &[u8], now_ms: u64) -> Vec<Output> {
    let mut out = Vec::new();
    m.on_bytes(bytes, now_ms, &mut |o| out.push(o));
    out
}

fn poll(m: &mut PresenceMachine, now_ms: u64) -> Vec<Output> {
    let mut out = Vec::new();
    m.poll(now_ms, &mut |o| out.push(o));
    out
}

fn transitions(outputs: &[Output]) -> Vec<(Presence, u32)> {
    outputs
        .iter()
        .filter_map(|o| match o {
            Output::Transition { state, baud } => Some((*state, *baud)),
            _ => None,
        })
        .collect()
}

fn set_bauds(outputs: &[Output]) -> Vec<u32> {
    outputs
        .iter()
        .filter_map(|o| match o {
            Output::SetBaud(b) => Some(*b),
            _ => None,
        })
        .collect()
}

fn rmc_count(outputs: &[Output]) -> usize {
    outputs
        .iter()
        .filter(|o| matches!(o, Output::Rmc(_)))
        .count()
}

fn gga_count(outputs: &[Output]) -> usize {
    outputs
        .iter()
        .filter(|o| matches!(o, Output::Gga(_)))
        .count()
}

// ---- Sweep ----

// A parsed sentence at the first baud locks it: NoFix is published at
// 9600 and no baud change is requested. The void cold-start sentence is
// deliberately the lock evidence — locking must not require a valid fix.
#[test]
fn locks_first_baud_on_valid_nmea() {
    let mut m = PresenceMachine::new(0);
    assert_eq!(m.published(), None);
    let out = feed(&mut m, &void_rmc(), 100);
    assert_eq!(transitions(&out), vec![(Presence::NoFix, 9600)]);
    assert_eq!(set_bauds(&out), Vec::<u32>::new());
    assert_eq!(m.current_baud(), 9600);
}

// Garbage at 9600, valid NMEA at 38400: the machine requests the baud
// change at window expiry and locks the second baud. Positive control
// inside: before the window expires no SetBaud is emitted.
#[test]
fn sweeps_to_next_baud_on_garbage() {
    let mut m = PresenceMachine::new(0);
    let out = feed(&mut m, GARBAGE, 500);
    assert_eq!(
        set_bauds(&out),
        Vec::<u32>::new(),
        "window must not end early"
    );
    assert_eq!(transitions(&out), vec![]);

    let out = poll(&mut m, DETECT_WINDOW_MS);
    assert_eq!(set_bauds(&out), vec![38_400]);
    assert_eq!(m.current_baud(), 38_400);

    let out = feed(&mut m, &void_rmc(), DETECT_WINDOW_MS + 500);
    assert_eq!(transitions(&out), vec![(Presence::NoFix, 38_400)]);
}

// A garbage half-sentence at the old baud must not poison parsing at
// the new baud: a '$' start swallowed by wrong-baud noise would leave
// the parser mid-sentence. The parser is reset per window.
#[test]
fn parser_resets_across_baud_change() {
    let mut m = PresenceMachine::new(0);
    // A partial sentence start with valid-looking prefix, never finished.
    feed(&mut m, b"$GPRMC,2254", 500);
    poll(&mut m, DETECT_WINDOW_MS);
    let out = feed(&mut m, &void_rmc(), DETECT_WINDOW_MS + 500);
    assert_eq!(transitions(&out), vec![(Presence::NoFix, 38_400)]);
}

// Total silence sweeps all three bauds, then settles NoHardware
// (baud=0: none found) and parks at 9600.
#[test]
fn silence_settles_no_hardware_after_full_sweep() {
    let mut m = PresenceMachine::new(0);
    let out = poll(&mut m, DETECT_WINDOW_MS);
    assert_eq!(set_bauds(&out), vec![38_400]);
    assert_eq!(transitions(&out), vec![]);

    let out = poll(&mut m, 2 * DETECT_WINDOW_MS);
    assert_eq!(set_bauds(&out), vec![115_200]);
    assert_eq!(transitions(&out), vec![]);

    let out = poll(&mut m, 3 * DETECT_WINDOW_MS);
    assert_eq!(transitions(&out), vec![(Presence::NoHardware, 0)]);
    assert_eq!(set_bauds(&out), vec![9_600]);
    assert_eq!(m.published(), Some(Presence::NoHardware));
}

// Positive control for the silence path: a single garbage byte anywhere
// in the pass suppresses NoHardware — the pass ends in a quiet re-sweep
// instead. A second, fully silent pass then settles NoHardware once.
#[test]
fn any_activity_suppresses_no_hardware() {
    let mut m = PresenceMachine::new(0);
    feed(&mut m, &[0xff], 100);
    poll(&mut m, DETECT_WINDOW_MS);
    poll(&mut m, 2 * DETECT_WINDOW_MS);
    let out = poll(&mut m, 3 * DETECT_WINDOW_MS);
    assert_eq!(
        transitions(&out),
        vec![],
        "activity in the pass must suppress the NoHardware settle"
    );
    assert_eq!(set_bauds(&out), vec![9_600], "pass must restart, not park");

    // Second pass: fully silent → NoHardware, exactly one event.
    poll(&mut m, 4 * DETECT_WINDOW_MS);
    poll(&mut m, 5 * DETECT_WINDOW_MS);
    let out = poll(&mut m, 6 * DETECT_WINDOW_MS);
    assert_eq!(transitions(&out), vec![(Presence::NoHardware, 0)]);
}

// A UART error (framing at wrong baud) is line activity: it must
// suppress NoHardware exactly as bytes do.
#[test]
fn uart_error_counts_as_activity() {
    let mut m = PresenceMachine::new(0);
    let mut out = Vec::new();
    m.on_uart_error(100, &mut |o| out.push(o));
    poll(&mut m, DETECT_WINDOW_MS);
    poll(&mut m, 2 * DETECT_WINDOW_MS);
    let out = poll(&mut m, 3 * DETECT_WINDOW_MS);
    assert_eq!(transitions(&out), vec![]);
    assert_eq!(set_bauds(&out), vec![9_600]);
}

// NoHardware is not a dead end: bytes appearing later (receiver plugged
// in at runtime) restart the sweep, and valid NMEA then locks a baud.
#[test]
fn no_hardware_recovers_when_bytes_appear() {
    let mut m = PresenceMachine::new(0);
    poll(&mut m, DETECT_WINDOW_MS);
    poll(&mut m, 2 * DETECT_WINDOW_MS);
    poll(&mut m, 3 * DETECT_WINDOW_MS);
    assert_eq!(m.published(), Some(Presence::NoHardware));

    let t = 100 * DETECT_WINDOW_MS;
    let out = feed(&mut m, &void_rmc(), t);
    assert_eq!(transitions(&out), vec![(Presence::NoFix, 9600)]);
}

// Repeated silent passes must not re-emit NoHardware (event-channel
// spam): after the settle, later silent windows publish nothing.
#[test]
fn duplicate_no_hardware_is_suppressed() {
    let mut m = PresenceMachine::new(0);
    poll(&mut m, DETECT_WINDOW_MS);
    poll(&mut m, 2 * DETECT_WINDOW_MS);
    let first = poll(&mut m, 3 * DETECT_WINDOW_MS);
    assert_eq!(transitions(&first).len(), 1);

    // Noise wakes it into a sweep that then goes silent again.
    feed(&mut m, &[0x55], 4 * DETECT_WINDOW_MS);
    poll(&mut m, 5 * DETECT_WINDOW_MS);
    poll(&mut m, 6 * DETECT_WINDOW_MS);
    let resweep = poll(&mut m, 7 * DETECT_WINDOW_MS);
    // Pass had activity (the wake byte) → re-sweep, no settle.
    assert_eq!(transitions(&resweep), vec![]);
    poll(&mut m, 8 * DETECT_WINDOW_MS);
    poll(&mut m, 9 * DETECT_WINDOW_MS);
    let second = poll(&mut m, 10 * DETECT_WINDOW_MS);
    assert_eq!(
        transitions(&second),
        vec![],
        "re-settling NoHardware must not re-emit the event"
    );
    assert_eq!(m.published(), Some(Presence::NoHardware));
}

// Once locked, the baud sticks: windows keep expiring without any
// further SetBaud, even through long silence.
#[test]
fn locked_baud_sticks() {
    let mut m = PresenceMachine::new(0);
    poll(&mut m, DETECT_WINDOW_MS); // → 38400
    feed(&mut m, &void_rmc(), DETECT_WINDOW_MS + 500);
    assert_eq!(m.current_baud(), 38_400);

    let out = poll(&mut m, 50 * DETECT_WINDOW_MS);
    assert_eq!(set_bauds(&out), Vec::<u32>::new());
    assert_eq!(m.current_baud(), 38_400);
}

// ---- Fix / hysteresis ----

// A valid RMC promotes to Fix immediately and the RMC is forwarded for
// the GnssFix fold. Order: the transition precedes the RMC so consumers
// never see fix data under a stale presence.
#[test]
fn valid_rmc_promotes_to_fix() {
    let mut m = PresenceMachine::new(0);
    let out = feed(&mut m, &valid_rmc(), 100);
    assert_eq!(
        transitions(&out),
        vec![(Presence::NoFix, 9600), (Presence::Fix, 9600)]
    );
    assert_eq!(rmc_count(&out), 1);
    let trans_pos = out
        .iter()
        .position(|o| {
            matches!(
                o,
                Output::Transition {
                    state: Presence::Fix,
                    ..
                }
            )
        })
        .unwrap();
    let rmc_pos = out
        .iter()
        .position(|o| matches!(o, Output::Rmc(_)))
        .unwrap();
    assert!(
        trans_pos < rmc_pos,
        "Fix transition must precede the RMC data"
    );
}

// GGA never touches presence — even a fix-quality GGA stream leaves the
// state at NoFix. This pins the "position/timebase gating stays keyed
// to valid RMC only" contract (#166): GGA content is forwarded for the
// snapshot, nothing more.
#[test]
fn gga_never_promotes_presence() {
    let mut m = PresenceMachine::new(0);
    let mut out = feed(&mut m, &fix_gga(), 100);
    for i in 1..5 {
        out.extend(feed(&mut m, &fix_gga(), 100 + i * 1000));
    }
    assert_eq!(transitions(&out), vec![(Presence::NoFix, 9600)]);
    assert_eq!(gga_count(&out), 5);
    assert_eq!(m.published(), Some(Presence::NoFix));
}

// A populated but not-valid RMC must not promote either (and is still
// forwarded, because the fold clears the stale time claim from it).
#[test]
fn invalid_rmc_never_promotes() {
    let mut m = PresenceMachine::new(0);
    let out = feed(&mut m, &invalid_rmc(), 100);
    assert_eq!(transitions(&out), vec![(Presence::NoFix, 9600)]);
    assert_eq!(rmc_count(&out), 1);
}

// Fix survives invalid RMCs shorter than the hold (margin flaps), and a
// valid RMC inside the hold restarts it. Positive control: the same
// stream with the refresh removed demotes (next test).
#[test]
fn fix_holds_through_brief_invalidity() {
    let mut m = PresenceMachine::new(0);
    feed(&mut m, &valid_rmc(), 1_000);
    assert_eq!(m.published(), Some(Presence::Fix));

    // Invalid RMCs at 1 Hz for just under the hold.
    let mut out = Vec::new();
    for i in 1..(FIX_HOLD_MS / 1000) {
        out.extend(feed(&mut m, &invalid_rmc(), 1_000 + i * 1_000));
    }
    assert_eq!(transitions(&out), vec![], "no demotion inside the hold");

    // A valid RMC refreshes the hold...
    feed(&mut m, &valid_rmc(), 1_000 + FIX_HOLD_MS - 500);
    // ...so invalidity keeps being tolerated well past the original
    // deadline.
    let out = feed(&mut m, &invalid_rmc(), FIX_HOLD_MS + 5_000);
    assert_eq!(transitions(&out), vec![]);
    assert_eq!(m.published(), Some(Presence::Fix));
}

// Fix demotes to NoFix once no valid RMC arrived for the full hold —
// via an invalid sentence carrying the clock forward.
#[test]
fn fix_demotes_after_hold_via_sentences() {
    let mut m = PresenceMachine::new(0);
    feed(&mut m, &valid_rmc(), 1_000);
    let out = feed(&mut m, &invalid_rmc(), 1_000 + FIX_HOLD_MS);
    assert_eq!(transitions(&out), vec![(Presence::NoFix, 9600)]);
    assert_eq!(m.published(), Some(Presence::NoFix));
}

// ...and via bare time (receiver unplugged mid-run: bytes stop
// entirely, only poll carries the clock).
#[test]
fn fix_demotes_after_hold_via_poll() {
    let mut m = PresenceMachine::new(0);
    feed(&mut m, &valid_rmc(), 1_000);
    let out = poll(&mut m, 999 + FIX_HOLD_MS);
    assert_eq!(transitions(&out), vec![], "hold must run its full length");
    let out = poll(&mut m, 1_000 + FIX_HOLD_MS);
    assert_eq!(transitions(&out), vec![(Presence::NoFix, 9600)]);
}

// After a demotion the next valid RMC re-promotes immediately —
// reacquisition must not be penalised.
#[test]
fn repromotes_after_demotion() {
    let mut m = PresenceMachine::new(0);
    feed(&mut m, &valid_rmc(), 1_000);
    poll(&mut m, 1_000 + FIX_HOLD_MS);
    assert_eq!(m.published(), Some(Presence::NoFix));
    let out = feed(&mut m, &valid_rmc(), 2_000 + FIX_HOLD_MS);
    assert_eq!(transitions(&out), vec![(Presence::Fix, 9600)]);
}

// ---- Event tokens ----

// The exact debug-channel tokens periculum replays grep for.
#[test]
fn event_tokens_are_stable() {
    assert_eq!(Presence::NoHardware.as_str(), "no-hardware");
    assert_eq!(Presence::NoFix.as_str(), "no-fix");
    assert_eq!(Presence::Fix.as_str(), "fix");
}

// The sweep constants the doc-comments justify: order and window/hold
// relations (window ≥ 2 sentence periods, hold ≥ a handful of them).
#[test]
fn constants_hold_their_justifications() {
    assert_eq!(BAUD_SWEEP, [9600, 38_400, 115_200]);
    assert!(
        DETECT_WINDOW_MS >= 2_000,
        "window must cover ≥2 NMEA periods"
    );
    assert!(
        FIX_HOLD_MS >= 5_000,
        "hold must ride out multi-sentence flaps"
    );
}
