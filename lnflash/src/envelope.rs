//! The host side of the #238 control envelope.
//!
//! One speaker for every control conversation on the transport CDC: write
//! an HDLC-framed request, wait for the frame that answers it, retry a
//! bounded number of times. The wire format lives in
//! [`leviculum_core::envelope`]; this module adds only the serial-port
//! choreography, shared by the radio-config sender ([`crate::radio`]) and
//! the wall-time session — the framing is written exactly once.
//!
//! The capability probe replaces guess-by-timeout: one
//! [`TYPE_CAPABILITIES`](leviculum_core::envelope::TYPE_CAPABILITIES)
//! query tells the host which frame types the firmware accepts. Firmware
//! older than the envelope answers nothing — the probe's bounded silence
//! is the one place the old guessing survives, and it decides the legacy
//! fallback for the transition window (see
//! `docs/src/firmware/usb-control-envelope.md` for how that retires).

use std::io;
use std::time::{Duration, Instant};

use leviculum_core::envelope::{
    decode_ack_payload, decode_capability_report_payload, decode_frame, decode_refusal_payload,
    encode_capability_query, encode_radio_config, encode_telemetry_clear, encode_telemetry_target,
    encode_wall_time, TelemetryTargetWire, REFUSE_BUSY, REFUSE_MALFORMED, REFUSE_UNKNOWN_TYPE,
    REFUSE_VALUE, TYPE_ACK, TYPE_CAPABILITY_REPORT, TYPE_REFUSAL,
};
use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use leviculum_core::rnode::RadioConfigWire;

use crate::sys::Fd;

/// How long one write may take before the port counts as gone.
const WRITE_WITHIN: Duration = Duration::from_secs(2);

/// Attempt/window budget for one control conversation.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub attempts: u8,
    pub window: Duration,
}

/// The capability probe's budget. Against firmware that speaks the
/// envelope the report arrives in milliseconds; against older firmware
/// this is the whole price of finding that out, so it is kept short.
pub const PROBE_TIMING: Timing = Timing {
    attempts: 2,
    window: Duration::from_millis(700),
};

/// The budget for a command that changes board state, matching the
/// legacy radio-config sender's patience.
pub const CONTROL_TIMING: Timing = Timing {
    attempts: crate::radio::ATTEMPTS,
    window: crate::radio::ACK_WITHIN,
};

/// What the firmware said to a command frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlOutcome {
    /// The enveloped ack for this frame type arrived.
    Acked,
    /// A named refusal for this frame type arrived.
    Refused { reason: u8 },
    /// The window closed without an answer.
    NoAnswer,
}

/// A named refusal reason, for the transcript.
pub fn reason_str(reason: u8) -> &'static str {
    match reason {
        REFUSE_UNKNOWN_TYPE => "the firmware does not accept this frame type",
        REFUSE_MALFORMED => "the firmware calls the frame malformed",
        REFUSE_VALUE => "the firmware refused the value",
        REFUSE_BUSY => "the firmware is busy",
        _ => "an unnamed reason",
    }
}

/// What the capability report said the firmware accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub version: u8,
    pub accepted: Vec<u8>,
}

impl Capabilities {
    pub fn accepts(&self, frame_type: u8) -> bool {
        self.accepted.contains(&frame_type)
    }
}

/// Write `payload` HDLC-framed and wait for a deframed answer `classify`
/// accepts, retrying up to the budget. `Ok(None)` is "the board never
/// answered" — a fact for the caller, not a failure of the port.
///
/// One deframer across all attempts: a late answer to a previous attempt
/// is still an answer, and resetting would drop a frame mid-arrival.
pub fn transact<T>(
    fd: &Fd,
    payload: &[u8],
    timing: Timing,
    classify: impl Fn(&[u8]) -> Option<T>,
) -> io::Result<Option<T>> {
    let mut framed = Vec::new();
    frame(payload, &mut framed);
    let mut deframer = Deframer::new();
    for _ in 0..timing.attempts {
        fd.write_all(&framed, Instant::now() + WRITE_WITHIN)?;
        if let Some(answer) =
            wait_for(fd, Instant::now() + timing.window, &mut deframer, &classify)?
        {
            return Ok(Some(answer));
        }
    }
    Ok(None)
}

fn wait_for<T>(
    fd: &Fd,
    deadline: Instant,
    deframer: &mut Deframer,
    classify: &impl Fn(&[u8]) -> Option<T>,
) -> io::Result<Option<T>> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        // End of file is the board going away — rebooting, unplugged — and
        // waiting the rest of the window out would only be a spin.
        let Some(chunk) = fd.read_available(remaining)? else {
            return Ok(None);
        };
        for result in deframer.process(&chunk) {
            if let DeframeResult::Frame(data) = result {
                if let Some(answer) = classify(&data) {
                    return Ok(Some(answer));
                }
            }
        }
    }
}

/// Ask the firmware what it accepts. `Ok(None)` is firmware from before
/// the envelope: the query is 5 bytes, shorter than any Reticulum packet,
/// so old firmware drops it silently and the probe times out.
pub fn probe_capabilities(fd: &Fd) -> io::Result<Option<Capabilities>> {
    transact(fd, &encode_capability_query(), PROBE_TIMING, |data| {
        let frame = decode_frame(data).ok()?;
        if frame.frame_type != TYPE_CAPABILITY_REPORT {
            return None;
        }
        let (version, accepted) = decode_capability_report_payload(frame.payload)?;
        Some(Capabilities {
            version,
            accepted: accepted.to_vec(),
        })
    })
}

/// The answer-classifier every command frame shares: its own typed ack,
/// or its own typed refusal. Answers about other frame types stay
/// unclaimed so a late ack from an earlier conversation cannot be
/// mistaken for this one's.
fn command_answer(expected_type: u8) -> impl Fn(&[u8]) -> Option<ControlOutcome> {
    move |data| {
        let frame = decode_frame(data).ok()?;
        match frame.frame_type {
            TYPE_ACK if decode_ack_payload(frame.payload) == Some(expected_type) => {
                Some(ControlOutcome::Acked)
            }
            TYPE_REFUSAL => match decode_refusal_payload(frame.payload) {
                Some((refused, reason)) if refused == expected_type => {
                    Some(ControlOutcome::Refused { reason })
                }
                _ => None,
            },
            _ => None,
        }
    }
}

/// Tell the board what time it is (#166 item 2, `TYPE_WALL_TIME`).
pub fn send_wall_time(fd: &Fd, unix_secs: u64) -> io::Result<ControlOutcome> {
    let payload = encode_wall_time(unix_secs);
    let outcome = transact(
        fd,
        &payload,
        CONTROL_TIMING,
        command_answer(leviculum_core::envelope::TYPE_WALL_TIME),
    )?;
    Ok(outcome.unwrap_or(ControlOutcome::NoAnswer))
}

/// Set the telemetry target (#236, `TYPE_TELEMETRY_TARGET`).
///
/// The public key is optional and its absence is the common case: a user
/// knows the LXMF address, and the node resolves the key over the air.
/// The frame is longer than the 19-byte Reticulum minimum, so like the
/// radio config it must only be sent to firmware whose capability report
/// includes the type — against anything older it would be packet-shaped
/// noise rather than a named refusal.
pub fn send_telemetry_target(fd: &Fd, target: &TelemetryTargetWire) -> io::Result<ControlOutcome> {
    let payload = encode_telemetry_target(target);
    let outcome = transact(
        fd,
        &payload,
        CONTROL_TIMING,
        command_answer(leviculum_core::envelope::TYPE_TELEMETRY_TARGET),
    )?;
    Ok(outcome.unwrap_or(ControlOutcome::NoAnswer))
}

/// Clear the telemetry target: telemetry off, since the configured target
/// is the switch (#236).
pub fn send_telemetry_clear(fd: &Fd) -> io::Result<ControlOutcome> {
    let payload = encode_telemetry_clear();
    let outcome = transact(
        fd,
        &payload,
        CONTROL_TIMING,
        command_answer(leviculum_core::envelope::TYPE_TELEMETRY_TARGET),
    )?;
    Ok(outcome.unwrap_or(ControlOutcome::NoAnswer))
}

/// Send the radio configuration as an envelope frame. Only for firmware
/// whose capability report includes `TYPE_RADIO_CONFIG`: the frame is
/// longer than the 19-byte Reticulum minimum, so it must never be sent
/// on a guess.
pub fn send_radio_config(fd: &Fd, cfg: &RadioConfigWire) -> io::Result<ControlOutcome> {
    let payload = encode_radio_config(cfg);
    let outcome = transact(
        fd,
        &payload,
        CONTROL_TIMING,
        command_answer(leviculum_core::envelope::TYPE_RADIO_CONFIG),
    )?;
    Ok(outcome.unwrap_or(ControlOutcome::NoAnswer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::testpty::{spawn_stub, Pty};
    use leviculum_core::constants::EMISSION_PLAUSIBLE_MIN_SECS;
    use leviculum_core::envelope::{
        classify_control_frame, encode_ack, encode_capability_report, encode_refusal,
        ControlAction, TELEMETRY_PROFILE_OFF, TELEMETRY_PROFILE_STATION, TYPE_CAPABILITIES,
        TYPE_RADIO_CONFIG, TYPE_RESET, TYPE_TELEMETRY_TARGET, TYPE_WALL_TIME,
    };
    use leviculum_core::rnode::RADIO_CONFIG_ACK;
    use std::sync::{Arc, Mutex};

    /// The accepted list the current firmware advertises. Must stay
    /// identical to `leviculum_nrf::usb::ACCEPTED_CONTROL_TYPES` — a stub
    /// that accepts more than the board does proves the host against a
    /// device that does not exist.
    const FIRMWARE_ACCEPTS: &[u8] = &[
        TYPE_RADIO_CONFIG,
        TYPE_RESET,
        TYPE_WALL_TIME,
        TYPE_CAPABILITIES,
        TYPE_TELEMETRY_TARGET,
    ];

    /// The accepted list of firmware from before #236 landed its
    /// telemetry consumer: everything else, and a named refusal for the
    /// target frame. This is how a #236-aware host detects an old board.
    const PRE_236_ACCEPTS: &[u8] = &[
        TYPE_RADIO_CONFIG,
        TYPE_RESET,
        TYPE_WALL_TIME,
        TYPE_CAPABILITIES,
    ];

    /// Short budgets so the negative tests do not sit out field windows.
    fn quick(fd_window_ms: u64) -> Timing {
        Timing {
            attempts: 1,
            window: Duration::from_millis(fd_window_ms),
        }
    }

    /// A scripted device running the firmware's actual decision function
    /// (`classify_control_frame`) — the stub answers exactly what the
    /// usb.rs accept path would answer, so these tests exercise the same
    /// contract the board implements.
    fn envelope_firmware_stub(pty: &Pty, seen: Arc<Mutex<Vec<Vec<u8>>>>) {
        spawn_stub(pty, move |frame_bytes| {
            seen.lock().unwrap().push(frame_bytes.to_vec());
            match classify_control_frame(frame_bytes, FIRMWARE_ACCEPTS) {
                ControlAction::CapabilityQuery => Some(encode_capability_report(FIRMWARE_ACCEPTS)),
                ControlAction::WallTime(unix) => Some(if unix >= EMISSION_PLAUSIBLE_MIN_SECS {
                    encode_ack(TYPE_WALL_TIME)
                } else {
                    encode_refusal(TYPE_WALL_TIME, REFUSE_VALUE)
                }),
                ControlAction::RadioConfig(_) => Some(encode_ack(TYPE_RADIO_CONFIG)),
                ControlAction::TelemetryTarget(_) => Some(encode_ack(TYPE_TELEMETRY_TARGET)),
                ControlAction::Refuse {
                    refused_type,
                    reason,
                } => Some(encode_refusal(refused_type, reason)),
                _ => None,
            }
        });
    }

    /// A scripted device running firmware from before #236: it speaks
    /// the envelope but has no telemetry consumer, so the target frame
    /// comes back refused by name rather than acked.
    fn pre_236_firmware_stub(pty: &Pty) {
        spawn_stub(pty, move |frame_bytes| {
            match classify_control_frame(frame_bytes, PRE_236_ACCEPTS) {
                ControlAction::CapabilityQuery => Some(encode_capability_report(PRE_236_ACCEPTS)),
                ControlAction::Refuse {
                    refused_type,
                    reason,
                } => Some(encode_refusal(refused_type, reason)),
                _ => None,
            }
        });
    }

    /// A scripted device running firmware from before the envelope: it
    /// answers the legacy config magic and nothing else — an envelope
    /// frame is packet-shaped noise to it.
    fn old_firmware_stub(pty: &Pty, seen: Arc<Mutex<Vec<Vec<u8>>>>) {
        spawn_stub(pty, move |frame_bytes| {
            seen.lock().unwrap().push(frame_bytes.to_vec());
            match classify_control_frame(frame_bytes, FIRMWARE_ACCEPTS) {
                ControlAction::LegacyRadioConfig(_) => Some(RADIO_CONFIG_ACK.to_vec()),
                _ => None,
            }
        });
    }

    #[test]
    fn the_capability_probe_learns_what_the_firmware_accepts() {
        let pty = Pty::open();
        envelope_firmware_stub(&pty, Arc::new(Mutex::new(Vec::new())));
        let fd = Fd::open_serial(&pty.slave_path).unwrap();
        let caps = probe_capabilities(&fd).unwrap().unwrap();
        assert_eq!(caps.version, leviculum_core::envelope::ENVELOPE_VERSION);
        assert_eq!(caps.accepted, FIRMWARE_ACCEPTS);
        assert!(caps.accepts(TYPE_WALL_TIME));
        assert!(caps.accepts(TYPE_TELEMETRY_TARGET));
    }

    #[test]
    fn the_capability_probe_reports_silence_as_none_not_as_an_error() {
        let pty = Pty::open();
        old_firmware_stub(&pty, Arc::new(Mutex::new(Vec::new())));
        let fd = Fd::open_serial(&pty.slave_path).unwrap();
        let answer = transact(&fd, &encode_capability_query(), quick(200), |data| {
            decode_frame(data).ok().map(|f| f.frame_type)
        })
        .unwrap();
        assert_eq!(answer, None);
    }

    #[test]
    fn a_plausible_wall_time_is_acked() {
        let pty = Pty::open();
        envelope_firmware_stub(&pty, Arc::new(Mutex::new(Vec::new())));
        let fd = Fd::open_serial(&pty.slave_path).unwrap();
        assert_eq!(
            send_wall_time(&fd, 1_790_000_000).unwrap(),
            ControlOutcome::Acked
        );
    }

    #[test]
    fn an_implausible_wall_time_is_refused_by_name_not_by_timeout() {
        let pty = Pty::open();
        envelope_firmware_stub(&pty, Arc::new(Mutex::new(Vec::new())));
        let fd = Fd::open_serial(&pty.slave_path).unwrap();
        let started = Instant::now();
        assert_eq!(
            send_wall_time(&fd, 1_000).unwrap(),
            ControlOutcome::Refused {
                reason: REFUSE_VALUE
            }
        );
        // The named refusal must arrive as an answer, not as a run-down
        // window: well under one attempt window proves it was spoken.
        assert!(started.elapsed() < CONTROL_TIMING.window);
    }

    #[test]
    fn wall_time_against_old_firmware_times_out_to_no_answer() {
        let pty = Pty::open();
        old_firmware_stub(&pty, Arc::new(Mutex::new(Vec::new())));
        let fd = Fd::open_serial(&pty.slave_path).unwrap();
        let outcome = transact(
            &fd,
            &encode_wall_time(1_790_000_000),
            quick(200),
            command_answer(TYPE_WALL_TIME),
        )
        .unwrap();
        assert_eq!(outcome, None);
    }

    #[test]
    fn an_unknown_frame_type_comes_back_as_a_named_refusal() {
        let pty = Pty::open();
        envelope_firmware_stub(&pty, Arc::new(Mutex::new(Vec::new())));
        let fd = Fd::open_serial(&pty.slave_path).unwrap();
        let outcome = transact(
            &fd,
            &leviculum_core::envelope::encode_frame(0x6E, &[]),
            CONTROL_TIMING,
            command_answer(0x6E),
        )
        .unwrap();
        assert_eq!(
            outcome,
            Some(ControlOutcome::Refused {
                reason: REFUSE_UNKNOWN_TYPE
            })
        );
    }

    // -----------------------------------------------------------------
    // Telemetry target (Codeberg #236)
    // -----------------------------------------------------------------

    fn hash_only_target() -> TelemetryTargetWire {
        TelemetryTargetWire {
            profile: TELEMETRY_PROFILE_STATION,
            dest_hash: [0xA7; 16],
            public_key: None,
        }
    }

    #[test]
    fn a_hash_only_target_is_acked_by_a_236_firmware() {
        // The common case per the 2026-08-22 UX decision: the user knows
        // the address and nothing else.
        let pty = Pty::open();
        let seen = Arc::new(Mutex::new(Vec::new()));
        envelope_firmware_stub(&pty, seen.clone());
        let fd = Fd::open_serial(&pty.slave_path).unwrap();

        let target = hash_only_target();
        assert_eq!(
            send_telemetry_target(&fd, &target).unwrap(),
            ControlOutcome::Acked
        );

        // What went on the wire is what the board decoded: a hash-only
        // payload, key-present flag explicitly absent.
        let frames = seen.lock().unwrap().clone();
        let payload = frames
            .iter()
            .find_map(|f| match classify_control_frame(f, FIRMWARE_ACCEPTS) {
                ControlAction::TelemetryTarget(t) => Some(t),
                _ => None,
            })
            .expect("no telemetry target frame reached the stub");
        assert_eq!(payload, target);
        assert_eq!(payload.public_key, None);
    }

    #[test]
    fn a_target_with_a_key_is_acked_too() {
        let pty = Pty::open();
        let seen = Arc::new(Mutex::new(Vec::new()));
        envelope_firmware_stub(&pty, seen.clone());
        let fd = Fd::open_serial(&pty.slave_path).unwrap();

        let target = TelemetryTargetWire {
            public_key: Some([0x5E; 64]),
            ..hash_only_target()
        };
        assert_eq!(
            send_telemetry_target(&fd, &target).unwrap(),
            ControlOutcome::Acked
        );
    }

    #[test]
    fn a_clear_frame_carries_the_off_profile_and_is_acked() {
        let pty = Pty::open();
        let seen = Arc::new(Mutex::new(Vec::new()));
        envelope_firmware_stub(&pty, seen.clone());
        let fd = Fd::open_serial(&pty.slave_path).unwrap();

        assert_eq!(send_telemetry_clear(&fd).unwrap(), ControlOutcome::Acked);

        let frames = seen.lock().unwrap().clone();
        let payload = frames
            .iter()
            .find_map(|f| match classify_control_frame(f, FIRMWARE_ACCEPTS) {
                ControlAction::TelemetryTarget(t) => Some(t),
                _ => None,
            })
            .expect("no telemetry clear frame reached the stub");
        assert_eq!(payload.profile, TELEMETRY_PROFILE_OFF);
        assert_eq!(payload.dest_hash, [0u8; 16]);
        assert_eq!(payload.public_key, None);
    }

    #[test]
    fn a_pre_236_board_refuses_the_target_by_name_instead_of_timing_out() {
        // The detection path: an old board answers, and what it answers
        // says exactly what is missing.
        let pty = Pty::open();
        pre_236_firmware_stub(&pty);
        let fd = Fd::open_serial(&pty.slave_path).unwrap();

        let caps = probe_capabilities(&fd).unwrap().unwrap();
        assert!(!caps.accepts(TYPE_TELEMETRY_TARGET));
        assert_eq!(
            send_telemetry_target(&fd, &hash_only_target()).unwrap(),
            ControlOutcome::Refused {
                reason: REFUSE_UNKNOWN_TYPE
            }
        );
    }
}
