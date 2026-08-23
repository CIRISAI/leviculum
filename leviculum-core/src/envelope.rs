//! Framed control envelope for the LNode USB channel (Codeberg #238).
//!
//! The transport CDC carries HDLC-framed Reticulum packets, plus a small
//! out-of-band control plane between the attached host (`lnflash`, `lnsd`)
//! and the firmware. Historically that control plane was one hand-cut magic
//! per feature ([`RADIO_CONFIG_MAGIC`], [`RADIO_RESET_FRAME`]); this module
//! is the single envelope every further control frame rides in, so a new
//! frame type is a new constant and a payload codec, never new framing.
//!
//! # Wire format
//!
//! One envelope per HDLC frame:
//!
//! ```text
//! [0xA4, 0xA5] [type: u8] [len: u16 BE] [payload: len bytes]
//! ```
//!
//! The length is strict: a frame whose payload is shorter or longer than
//! `len` is malformed and answered with a refusal, never silence. A reader
//! that knows the envelope but not the type refuses it by name and stays in
//! sync — the HDLC delimiter bounds the frame, the header names what was
//! skipped.
//!
//! # Distinguishability from Reticulum packets
//!
//! Same argument as the legacy magics, sharpened per frame:
//!
//! * The first byte `0xA4` has the IFAC bit set; the USB channel runs
//!   without IFAC, so no Reticulum peer on it ever emits a packet whose
//!   first byte matches, and firmware older than the envelope drops a
//!   received envelope frame in packet parsing for the same reason.
//! * Every frame a host sends *before* it knows the firmware speaks the
//!   envelope (the [`TYPE_CAPABILITIES`] probe, [`TYPE_WALL_TIME`],
//!   [`TYPE_RESET`]) is shorter than the 19-byte minimum Reticulum wire
//!   packet, so it cannot be packet-shaped at all. Longer frames
//!   ([`TYPE_RADIO_CONFIG`], [`TYPE_TELEMETRY_TARGET`]) are only sent after
//!   a capability report proved the peer is envelope-speaking firmware.
//!
//! # Compatibility window
//!
//! The two legacy magics stay accepted by the firmware, and
//! [`classify_control_frame`] answers them with their legacy two-byte acks,
//! so a field board and a field host tool interoperate across the
//! transition in both directions. The retirement story lives in
//! `docs/src/firmware/usb-control-envelope.md`.

use alloc::vec::Vec;

use crate::constants::{IDENTITY_KEY_SIZE, TRUNCATED_HASHBYTES};
use crate::rnode::{
    parse_radio_config, RadioConfigWire, RADIO_CONFIG_FRAME_LEN, RADIO_CONFIG_MAGIC,
    RADIO_RESET_FRAME,
};

/// Magic prefix of every envelope frame. Distinct from the legacy
/// [`RADIO_CONFIG_MAGIC`] in the second byte, so the two control planes can
/// never shadow each other during the compatibility window.
pub const ENVELOPE_MAGIC: [u8; 2] = [0xA4, 0xA5];

/// Envelope header: magic (2) + type (1) + length (2, big-endian).
pub const ENVELOPE_HEADER_LEN: usize = 5;

/// Version reported in the capability report. Bumped only if the envelope
/// framing itself ever changes shape; new frame types do not bump it.
pub const ENVELOPE_VERSION: u8 = 1;

// ---------------------------------------------------------------------------
// Frame types: host -> board commands
// ---------------------------------------------------------------------------

/// Placeholder type used in a refusal when the offending frame was too
/// short to carry a type byte at all.
pub const TYPE_UNSPECIFIED: u8 = 0x00;
/// Radio configuration; payload is the parameter block of the legacy frame
/// (see [`parse_radio_config`]), without any magic.
pub const TYPE_RADIO_CONFIG: u8 = 0x01;
/// Full system reset; empty payload.
pub const TYPE_RESET: u8 = 0x02;
/// Wall-time injection; payload is unix seconds as u64 big-endian.
pub const TYPE_WALL_TIME: u8 = 0x03;
/// Capability query; empty payload. Answered with
/// [`TYPE_CAPABILITY_REPORT`], which replaces guess-by-timeout probing.
pub const TYPE_CAPABILITIES: u8 = 0x04;
/// Telemetry target (Codeberg #236): destination hash, optional public
/// key, profile id. `profile == TELEMETRY_PROFILE_OFF` clears the target,
/// which is how telemetry is switched off — the configured target is the
/// switch.
pub const TYPE_TELEMETRY_TARGET: u8 = 0x05;

// ---------------------------------------------------------------------------
// Frame types: board -> host responses
// ---------------------------------------------------------------------------

/// Positive acknowledgement; payload is the one type byte being acked.
pub const TYPE_ACK: u8 = 0x81;
/// Named refusal; payload is `[refused_type, reason]`.
pub const TYPE_REFUSAL: u8 = 0x82;
/// Capability report; payload is `[ENVELOPE_VERSION, accepted types...]`.
pub const TYPE_CAPABILITY_REPORT: u8 = 0x83;

// ---------------------------------------------------------------------------
// Refusal reasons
// ---------------------------------------------------------------------------

/// The firmware does not accept this frame type (unknown, or known but not
/// yet consumed by this firmware).
pub const REFUSE_UNKNOWN_TYPE: u8 = 0x01;
/// The envelope header or the payload shape is wrong (truncated header,
/// length mismatch, payload that fails its codec).
pub const REFUSE_MALFORMED: u8 = 0x02;
/// The frame parsed but its value was refused (e.g. a wall time outside
/// the plausibility window).
pub const REFUSE_VALUE: u8 = 0x03;
/// The firmware is momentarily unable to take the frame; retry.
pub const REFUSE_BUSY: u8 = 0x04;

// ---------------------------------------------------------------------------
// Generic encode / decode
// ---------------------------------------------------------------------------

/// A decoded envelope frame, borrowing its payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlFrame<'a> {
    pub frame_type: u8,
    pub payload: &'a [u8],
}

/// Why a byte sequence is not a well-formed envelope frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeError {
    /// The magic is absent: this is not an envelope frame at all (most
    /// likely a Reticulum packet; not an error on the channel).
    NotEnvelope,
    /// The magic is present but the frame is not `header + len` bytes.
    Malformed {
        /// The type byte if the frame was long enough to carry one,
        /// [`TYPE_UNSPECIFIED`] otherwise. For the refusal answer.
        frame_type: u8,
    },
}

/// Whether these bytes claim to be an envelope frame (magic check only).
pub fn is_envelope(data: &[u8]) -> bool {
    data.len() >= ENVELOPE_MAGIC.len() && data[..ENVELOPE_MAGIC.len()] == ENVELOPE_MAGIC
}

/// Encode one envelope frame.
pub fn encode_frame(frame_type: u8, payload: &[u8]) -> Vec<u8> {
    debug_assert!(payload.len() <= u16::MAX as usize);
    let mut out = Vec::with_capacity(ENVELOPE_HEADER_LEN + payload.len());
    out.extend_from_slice(&ENVELOPE_MAGIC);
    out.push(frame_type);
    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Decode one envelope frame. Strict: the frame must be exactly
/// `ENVELOPE_HEADER_LEN + len` bytes.
pub fn decode_frame(data: &[u8]) -> Result<ControlFrame<'_>, EnvelopeError> {
    if !is_envelope(data) {
        return Err(EnvelopeError::NotEnvelope);
    }
    let frame_type = if data.len() > 2 {
        data[2]
    } else {
        TYPE_UNSPECIFIED
    };
    if data.len() < ENVELOPE_HEADER_LEN {
        return Err(EnvelopeError::Malformed { frame_type });
    }
    let len = u16::from_be_bytes([data[3], data[4]]) as usize;
    if data.len() != ENVELOPE_HEADER_LEN + len {
        return Err(EnvelopeError::Malformed { frame_type });
    }
    Ok(ControlFrame {
        frame_type,
        payload: &data[ENVELOPE_HEADER_LEN..],
    })
}

// ---------------------------------------------------------------------------
// Payload codecs
// ---------------------------------------------------------------------------

/// Encode a complete wall-time frame.
pub fn encode_wall_time(unix_secs: u64) -> Vec<u8> {
    encode_frame(TYPE_WALL_TIME, &unix_secs.to_be_bytes())
}

/// Decode a wall-time payload: exactly 8 bytes, u64 big-endian.
pub fn decode_wall_time_payload(payload: &[u8]) -> Option<u64> {
    let bytes: [u8; 8] = payload.try_into().ok()?;
    Some(u64::from_be_bytes(bytes))
}

/// Encode a complete reset frame.
pub fn encode_reset() -> Vec<u8> {
    encode_frame(TYPE_RESET, &[])
}

/// Encode a complete capability query.
pub fn encode_capability_query() -> Vec<u8> {
    encode_frame(TYPE_CAPABILITIES, &[])
}

/// Encode a complete radio-config frame; the payload is the same
/// parameter block the legacy magic frame carries after its magic.
pub fn encode_radio_config(cfg: &RadioConfigWire) -> Vec<u8> {
    let legacy = crate::rnode::build_radio_config_frame(cfg);
    encode_frame(TYPE_RADIO_CONFIG, &legacy[RADIO_CONFIG_MAGIC.len()..])
}

/// Encode a complete acknowledgement for `acked_type`.
pub fn encode_ack(acked_type: u8) -> Vec<u8> {
    encode_frame(TYPE_ACK, &[acked_type])
}

/// Decode an ack payload into the acked type.
pub fn decode_ack_payload(payload: &[u8]) -> Option<u8> {
    match payload {
        [acked] => Some(*acked),
        _ => None,
    }
}

/// Encode a complete named refusal of `refused_type` for `reason`.
pub fn encode_refusal(refused_type: u8, reason: u8) -> Vec<u8> {
    encode_frame(TYPE_REFUSAL, &[refused_type, reason])
}

/// Decode a refusal payload into `(refused_type, reason)`.
pub fn decode_refusal_payload(payload: &[u8]) -> Option<(u8, u8)> {
    match payload {
        [refused, reason] => Some((*refused, *reason)),
        _ => None,
    }
}

/// Encode a complete capability report for the given accepted types.
pub fn encode_capability_report(accepted: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(1 + accepted.len());
    payload.push(ENVELOPE_VERSION);
    payload.extend_from_slice(accepted);
    encode_frame(TYPE_CAPABILITY_REPORT, &payload)
}

/// Decode a capability-report payload into `(version, accepted types)`.
pub fn decode_capability_report_payload(payload: &[u8]) -> Option<(u8, &[u8])> {
    let (version, accepted) = payload.split_first()?;
    Some((*version, accepted))
}

// ---------------------------------------------------------------------------
// Telemetry target (Codeberg #236 — wire format allocated here)
// ---------------------------------------------------------------------------

/// Telemetry-target profile id that **clears** the target instead of
/// setting one (Codeberg #236).
///
/// The configured target is the on-switch, so "off" is the absence of a
/// target and needs an encoding of its own. It rides in the profile slot
/// rather than in a magic destination hash: the payload already carries a
/// field whose whole job is to say which cadence applies, and "none"
/// belongs in that field's vocabulary. The rest of the payload is still
/// parsed and must still be well-formed — a clear frame is not a licence
/// to send a short one — and [`decode_telemetry_target_payload`] returns
/// it like any other, so the *reader* decides what an absent profile
/// means rather than the framing.
pub const TELEMETRY_PROFILE_OFF: u8 = 0x00;
/// Telemetry-target profile id: movement-driven reporting (see #236).
pub const TELEMETRY_PROFILE_TRACKER: u8 = 0x01;
/// Telemetry-target profile id: slow stationary heartbeat (see #236).
pub const TELEMETRY_PROFILE_STATION: u8 = 0x02;

/// Encode a complete telemetry-target frame that clears the target.
///
/// The destination hash is zeroed and no key is carried: with
/// [`TELEMETRY_PROFILE_OFF`] in the profile slot neither is read, and
/// sending the old target back to say "forget it" would put a
/// destination on the wire for no reason.
pub fn encode_telemetry_clear() -> Vec<u8> {
    encode_telemetry_target(&TelemetryTargetWire {
        profile: TELEMETRY_PROFILE_OFF,
        dest_hash: [0u8; TRUNCATED_HASHBYTES],
        public_key: None,
    })
}

/// The telemetry-target frame payload (Codeberg #236, amended 2026-08-22):
/// the public key is OPTIONAL and its presence is an explicit flag byte,
/// never inferred from the length. Hash-only is the common case — the user
/// knows the LXMF address, the node resolves the key over the air.
///
/// Payload layout:
///
/// ```text
/// [profile: u8] [dest_hash: 16] [key_present: u8] ([public_key: 64])
/// ```
///
/// `key_present` is `0x00` (absent, 18-byte payload) or `0x01` (present,
/// 82-byte payload); any other value is malformed. Profile semantics —
/// which id is the default, what each cadence policy means — belong to
/// #236; this module only fixes the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelemetryTargetWire {
    pub profile: u8,
    pub dest_hash: [u8; TRUNCATED_HASHBYTES],
    pub public_key: Option<[u8; IDENTITY_KEY_SIZE]>,
}

/// Encode a complete telemetry-target frame.
pub fn encode_telemetry_target(target: &TelemetryTargetWire) -> Vec<u8> {
    let mut payload = Vec::with_capacity(1 + TRUNCATED_HASHBYTES + 1 + IDENTITY_KEY_SIZE);
    payload.push(target.profile);
    payload.extend_from_slice(&target.dest_hash);
    match &target.public_key {
        Some(key) => {
            payload.push(0x01);
            payload.extend_from_slice(key);
        }
        None => payload.push(0x00),
    }
    encode_frame(TYPE_TELEMETRY_TARGET, &payload)
}

/// Decode a telemetry-target payload.
pub fn decode_telemetry_target_payload(payload: &[u8]) -> Option<TelemetryTargetWire> {
    const HASH_END: usize = 1 + TRUNCATED_HASHBYTES;
    if payload.len() < HASH_END + 1 {
        return None;
    }
    let profile = payload[0];
    let mut dest_hash = [0u8; TRUNCATED_HASHBYTES];
    dest_hash.copy_from_slice(&payload[1..HASH_END]);
    let public_key = match (payload[HASH_END], payload.len() - HASH_END - 1) {
        (0x00, 0) => None,
        (0x01, IDENTITY_KEY_SIZE) => {
            let mut key = [0u8; IDENTITY_KEY_SIZE];
            key.copy_from_slice(&payload[HASH_END + 1..]);
            Some(key)
        }
        _ => return None,
    };
    Some(TelemetryTargetWire {
        profile,
        dest_hash,
        public_key,
    })
}

// ---------------------------------------------------------------------------
// Control-plane classification (the firmware accept path, testable on host)
// ---------------------------------------------------------------------------

/// What one deframed HDLC frame on the transport CDC asks the firmware to
/// do. Produced by [`classify_control_frame`]; the firmware executes the
/// action and writes the named answer, so the whole decision — including
/// the legacy magics and every refusal — is pure and host-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAction {
    /// Not control traffic: hand the frame to the node core as a
    /// Reticulum packet.
    NotControl,
    /// Legacy 4-byte reset magic: answer `RADIO_RESET_ACK`, then reset.
    LegacyReset,
    /// Legacy 21-byte config magic, payload valid: apply, persist, answer
    /// `RADIO_CONFIG_ACK`.
    LegacyRadioConfig(RadioConfigWire),
    /// Legacy config magic with an invalid payload. The legacy behaviour —
    /// log locally, answer nothing — is preserved verbatim; audible
    /// refusals begin with the envelope.
    LegacyRadioConfigInvalid,
    /// Envelope radio config: apply, persist, answer
    /// `encode_ack(TYPE_RADIO_CONFIG)`.
    RadioConfig(RadioConfigWire),
    /// Envelope reset: answer `encode_ack(TYPE_RESET)`, then reset.
    Reset,
    /// Envelope wall time: seed the calendar via
    /// `set_wall_time_unix_secs(.., TimeSource::Host)`; the seam's bool
    /// picks `encode_ack` or `encode_refusal(.., REFUSE_VALUE)`.
    WallTime(u64),
    /// Envelope capability query: answer
    /// `encode_capability_report(accepted)`.
    CapabilityQuery,
    /// Envelope telemetry target (Codeberg #236): set or clear the
    /// reporting target, persist it, answer
    /// `encode_ack(TYPE_TELEMETRY_TARGET)`. `profile ==
    /// TELEMETRY_PROFILE_OFF` is the clear encoding; the destination hash
    /// and key are then meaningless and the firmware ignores them.
    TelemetryTarget(TelemetryTargetWire),
    /// Anything envelope-shaped that cannot be executed: answer
    /// `encode_refusal(refused_type, reason)`. Never silence.
    Refuse { refused_type: u8, reason: u8 },
}

/// Classify one deframed frame from the transport CDC.
///
/// `accepted` is the firmware's accepted-type list (what the capability
/// report advertises). A type outside it is refused with
/// [`REFUSE_UNKNOWN_TYPE`] — which is also how a #236-aware host detects
/// a pre-#236 board: [`TYPE_TELEMETRY_TARGET`] comes back refused by
/// name instead of acked.
pub fn classify_control_frame(data: &[u8], accepted: &[u8]) -> ControlAction {
    if data == RADIO_RESET_FRAME {
        return ControlAction::LegacyReset;
    }
    if data.len() == RADIO_CONFIG_FRAME_LEN && data[..2] == RADIO_CONFIG_MAGIC {
        return match parse_radio_config(&data[2..]) {
            Some(cfg) => ControlAction::LegacyRadioConfig(cfg),
            None => ControlAction::LegacyRadioConfigInvalid,
        };
    }
    let frame = match decode_frame(data) {
        Ok(frame) => frame,
        Err(EnvelopeError::NotEnvelope) => return ControlAction::NotControl,
        Err(EnvelopeError::Malformed { frame_type }) => {
            return ControlAction::Refuse {
                refused_type: frame_type,
                reason: REFUSE_MALFORMED,
            }
        }
    };
    if !accepted.contains(&frame.frame_type) {
        return ControlAction::Refuse {
            refused_type: frame.frame_type,
            reason: REFUSE_UNKNOWN_TYPE,
        };
    }
    let malformed = ControlAction::Refuse {
        refused_type: frame.frame_type,
        reason: REFUSE_MALFORMED,
    };
    match frame.frame_type {
        TYPE_RADIO_CONFIG => match parse_radio_config(frame.payload) {
            Some(cfg) => ControlAction::RadioConfig(cfg),
            None => malformed,
        },
        TYPE_RESET => {
            if frame.payload.is_empty() {
                ControlAction::Reset
            } else {
                malformed
            }
        }
        TYPE_WALL_TIME => match decode_wall_time_payload(frame.payload) {
            Some(unix_secs) => ControlAction::WallTime(unix_secs),
            None => malformed,
        },
        TYPE_CAPABILITIES => {
            if frame.payload.is_empty() {
                ControlAction::CapabilityQuery
            } else {
                malformed
            }
        }
        TYPE_TELEMETRY_TARGET => match decode_telemetry_target_payload(frame.payload) {
            Some(target) => ControlAction::TelemetryTarget(target),
            None => malformed,
        },
        // In the accepted list but without an executor here: refusing is
        // more honest than a firmware that acks what it cannot do.
        _ => ControlAction::Refuse {
            refused_type: frame.frame_type,
            reason: REFUSE_UNKNOWN_TYPE,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// The accepted list of the current firmware (both boards).
    const ACCEPTED: &[u8] = &[
        TYPE_RADIO_CONFIG,
        TYPE_RESET,
        TYPE_WALL_TIME,
        TYPE_CAPABILITIES,
        TYPE_TELEMETRY_TARGET,
    ];

    /// A firmware from before #236 landed its telemetry consumer.
    const ACCEPTED_PRE_236: &[u8] = &[
        TYPE_RADIO_CONFIG,
        TYPE_RESET,
        TYPE_WALL_TIME,
        TYPE_CAPABILITIES,
    ];

    fn sample_config() -> RadioConfigWire {
        RadioConfigWire {
            frequency_hz: 869_463_000,
            bandwidth_hz: 125_000,
            sf: 8,
            cr: 5,
            tx_power_dbm: 22,
            preamble_len: 16,
            csma_enabled: true,
            radio_silent: false,
            st_alock: 0,
            lt_alock: 1000,
            lt_alock_present: true,
        }
    }

    #[test]
    fn a_frame_round_trips_through_encode_and_decode() {
        let payload = [0xDE, 0xAD, 0xBE, 0xEF];
        let bytes = encode_frame(0x7F, &payload);
        let frame = decode_frame(&bytes).unwrap();
        assert_eq!(frame.frame_type, 0x7F);
        assert_eq!(frame.payload, &payload);
    }

    #[test]
    fn an_empty_payload_round_trips() {
        let bytes = encode_frame(TYPE_RESET, &[]);
        assert_eq!(bytes.len(), ENVELOPE_HEADER_LEN);
        let frame = decode_frame(&bytes).unwrap();
        assert_eq!(frame.frame_type, TYPE_RESET);
        assert!(frame.payload.is_empty());
    }

    #[test]
    fn a_reticulum_packet_is_not_mistaken_for_an_envelope() {
        // A plausible packet header: no envelope magic.
        let packet = [0x02u8; 24];
        assert_eq!(decode_frame(&packet), Err(EnvelopeError::NotEnvelope));
        assert_eq!(
            classify_control_frame(&packet, ACCEPTED),
            ControlAction::NotControl
        );
    }

    #[test]
    fn a_truncated_header_is_malformed_with_the_type_it_managed_to_carry() {
        // Magic + type, but no length bytes.
        let bytes = [ENVELOPE_MAGIC[0], ENVELOPE_MAGIC[1], TYPE_WALL_TIME];
        assert_eq!(
            decode_frame(&bytes),
            Err(EnvelopeError::Malformed {
                frame_type: TYPE_WALL_TIME
            })
        );
        // Magic alone: not even a type byte survived.
        let bytes = ENVELOPE_MAGIC;
        assert_eq!(
            decode_frame(&bytes),
            Err(EnvelopeError::Malformed {
                frame_type: TYPE_UNSPECIFIED
            })
        );
    }

    #[test]
    fn a_length_mismatch_is_malformed_in_both_directions() {
        let mut truncated = encode_wall_time(1_790_000_000);
        truncated.pop();
        assert_eq!(
            decode_frame(&truncated),
            Err(EnvelopeError::Malformed {
                frame_type: TYPE_WALL_TIME
            })
        );
        let mut oversized = encode_wall_time(1_790_000_000);
        oversized.push(0x00);
        assert_eq!(
            decode_frame(&oversized),
            Err(EnvelopeError::Malformed {
                frame_type: TYPE_WALL_TIME
            })
        );
    }

    #[test]
    fn an_unknown_type_is_refused_by_name_not_silence() {
        let bytes = encode_frame(0x6E, &[1, 2, 3]);
        assert_eq!(
            classify_control_frame(&bytes, ACCEPTED),
            ControlAction::Refuse {
                refused_type: 0x6E,
                reason: REFUSE_UNKNOWN_TYPE
            }
        );
    }

    #[test]
    fn a_pre_236_firmware_refuses_the_telemetry_type_by_name() {
        // How a #236-aware host detects an older board: a named refusal,
        // not a timeout.
        let target = TelemetryTargetWire {
            profile: TELEMETRY_PROFILE_TRACKER,
            dest_hash: [0x11; TRUNCATED_HASHBYTES],
            public_key: None,
        };
        let bytes = encode_telemetry_target(&target);
        assert_eq!(
            classify_control_frame(&bytes, ACCEPTED_PRE_236),
            ControlAction::Refuse {
                refused_type: TYPE_TELEMETRY_TARGET,
                reason: REFUSE_UNKNOWN_TYPE
            }
        );
    }

    #[test]
    fn a_telemetry_target_classifies_into_its_action() {
        let target = TelemetryTargetWire {
            profile: TELEMETRY_PROFILE_STATION,
            dest_hash: [0x11; TRUNCATED_HASHBYTES],
            public_key: None,
        };
        assert_eq!(
            classify_control_frame(&encode_telemetry_target(&target), ACCEPTED),
            ControlAction::TelemetryTarget(target)
        );
    }

    #[test]
    fn the_clear_frame_carries_the_off_profile_and_no_destination() {
        let bytes = encode_telemetry_clear();
        let target = match classify_control_frame(&bytes, ACCEPTED) {
            ControlAction::TelemetryTarget(t) => t,
            other => panic!("clear frame classified as {other:?}"),
        };
        assert_eq!(target.profile, TELEMETRY_PROFILE_OFF);
        assert_eq!(target.dest_hash, [0u8; TRUNCATED_HASHBYTES]);
        assert_eq!(target.public_key, None);
    }

    #[test]
    fn a_malformed_telemetry_payload_is_refused_audibly() {
        // Truncated hash: the payload's own codec refuses it, and the
        // classifier turns that into a named refusal rather than an ack
        // for a target it could not read.
        let bytes = encode_frame(TYPE_TELEMETRY_TARGET, &[TELEMETRY_PROFILE_STATION, 0x01]);
        assert_eq!(
            classify_control_frame(&bytes, ACCEPTED),
            ControlAction::Refuse {
                refused_type: TYPE_TELEMETRY_TARGET,
                reason: REFUSE_MALFORMED
            }
        );
    }

    #[test]
    fn the_wall_time_frame_round_trips_and_classifies() {
        let bytes = encode_wall_time(1_790_000_000);
        // Every pre-handshake frame must be shorter than the 19-byte
        // minimum Reticulum packet, so old firmware can never take it
        // for one.
        assert!(bytes.len() < 19);
        assert_eq!(
            classify_control_frame(&bytes, ACCEPTED),
            ControlAction::WallTime(1_790_000_000)
        );
    }

    #[test]
    fn a_wall_time_payload_of_the_wrong_size_is_refused_as_malformed() {
        let bytes = encode_frame(TYPE_WALL_TIME, &[0u8; 4]);
        assert_eq!(
            classify_control_frame(&bytes, ACCEPTED),
            ControlAction::Refuse {
                refused_type: TYPE_WALL_TIME,
                reason: REFUSE_MALFORMED
            }
        );
    }

    #[test]
    fn the_capability_query_and_report_round_trip() {
        let query = encode_capability_query();
        assert!(query.len() < 19);
        assert_eq!(
            classify_control_frame(&query, ACCEPTED),
            ControlAction::CapabilityQuery
        );
        let report = encode_capability_report(ACCEPTED);
        let frame = decode_frame(&report).unwrap();
        assert_eq!(frame.frame_type, TYPE_CAPABILITY_REPORT);
        let (version, accepted) = decode_capability_report_payload(frame.payload).unwrap();
        assert_eq!(version, ENVELOPE_VERSION);
        assert_eq!(accepted, ACCEPTED);
    }

    #[test]
    fn ack_and_refusal_payloads_round_trip() {
        let ack = encode_ack(TYPE_WALL_TIME);
        let frame = decode_frame(&ack).unwrap();
        assert_eq!(frame.frame_type, TYPE_ACK);
        assert_eq!(decode_ack_payload(frame.payload), Some(TYPE_WALL_TIME));
        assert_eq!(decode_ack_payload(&[]), None);

        let refusal = encode_refusal(TYPE_WALL_TIME, REFUSE_VALUE);
        let frame = decode_frame(&refusal).unwrap();
        assert_eq!(frame.frame_type, TYPE_REFUSAL);
        assert_eq!(
            decode_refusal_payload(frame.payload),
            Some((TYPE_WALL_TIME, REFUSE_VALUE))
        );
        assert_eq!(decode_refusal_payload(&[1]), None);
    }

    #[test]
    fn the_envelope_radio_config_and_the_legacy_magic_parse_to_the_same_config() {
        let cfg = sample_config();
        let enveloped = encode_radio_config(&cfg);
        let legacy = crate::rnode::build_radio_config_frame(&cfg);
        let from_envelope = match classify_control_frame(&enveloped, ACCEPTED) {
            ControlAction::RadioConfig(parsed) => parsed,
            other => panic!("envelope config classified as {other:?}"),
        };
        let from_legacy = match classify_control_frame(&legacy, ACCEPTED) {
            ControlAction::LegacyRadioConfig(parsed) => parsed,
            other => panic!("legacy config classified as {other:?}"),
        };
        assert_eq!(from_envelope, from_legacy);
        assert_eq!(from_envelope, cfg);
    }

    #[test]
    fn the_legacy_reset_magic_still_classifies_as_reset() {
        assert_eq!(
            classify_control_frame(&RADIO_RESET_FRAME, ACCEPTED),
            ControlAction::LegacyReset
        );
        let enveloped = encode_reset();
        assert!(enveloped.len() < 19);
        assert_eq!(
            classify_control_frame(&enveloped, ACCEPTED),
            ControlAction::Reset
        );
    }

    #[test]
    fn an_invalid_legacy_config_keeps_its_legacy_silence() {
        // 21 bytes, right magic, impossible spreading factor.
        let mut bytes = crate::rnode::build_radio_config_frame(&sample_config());
        bytes[10] = 42; // sf byte
        assert_eq!(bytes.len(), RADIO_CONFIG_FRAME_LEN);
        assert_eq!(
            classify_control_frame(&bytes, ACCEPTED),
            ControlAction::LegacyRadioConfigInvalid
        );
    }

    #[test]
    fn an_invalid_envelope_config_is_refused_audibly_unlike_the_legacy_path() {
        let mut bytes = encode_radio_config(&sample_config());
        bytes[ENVELOPE_HEADER_LEN + 8] = 42; // sf byte inside the payload
        assert_eq!(
            classify_control_frame(&bytes, ACCEPTED),
            ControlAction::Refuse {
                refused_type: TYPE_RADIO_CONFIG,
                reason: REFUSE_MALFORMED
            }
        );
    }

    #[test]
    fn a_telemetry_target_without_a_key_round_trips() {
        let target = TelemetryTargetWire {
            profile: TELEMETRY_PROFILE_STATION,
            dest_hash: [0xAB; TRUNCATED_HASHBYTES],
            public_key: None,
        };
        let bytes = encode_telemetry_target(&target);
        let frame = decode_frame(&bytes).unwrap();
        assert_eq!(frame.frame_type, TYPE_TELEMETRY_TARGET);
        assert_eq!(frame.payload.len(), 1 + TRUNCATED_HASHBYTES + 1);
        assert_eq!(decode_telemetry_target_payload(frame.payload), Some(target));
    }

    #[test]
    fn a_telemetry_target_with_a_key_round_trips() {
        let target = TelemetryTargetWire {
            profile: TELEMETRY_PROFILE_TRACKER,
            dest_hash: [0xCD; TRUNCATED_HASHBYTES],
            public_key: Some([0x42; IDENTITY_KEY_SIZE]),
        };
        let bytes = encode_telemetry_target(&target);
        let frame = decode_frame(&bytes).unwrap();
        assert_eq!(
            frame.payload.len(),
            1 + TRUNCATED_HASHBYTES + 1 + IDENTITY_KEY_SIZE
        );
        assert_eq!(decode_telemetry_target_payload(frame.payload), Some(target));
    }

    #[test]
    fn the_key_present_flag_is_explicit_never_length_guessing() {
        let target = TelemetryTargetWire {
            profile: TELEMETRY_PROFILE_TRACKER,
            dest_hash: [0x01; TRUNCATED_HASHBYTES],
            public_key: Some([0x02; IDENTITY_KEY_SIZE]),
        };
        let bytes = encode_telemetry_target(&target);
        let mut payload = bytes[ENVELOPE_HEADER_LEN..].to_vec();

        // Flag says absent but a key follows: malformed.
        payload[1 + TRUNCATED_HASHBYTES] = 0x00;
        assert_eq!(decode_telemetry_target_payload(&payload), None);

        // Flag says present but the key is truncated: malformed.
        payload[1 + TRUNCATED_HASHBYTES] = 0x01;
        payload.pop();
        assert_eq!(decode_telemetry_target_payload(&payload), None);

        // A flag value that is neither 0 nor 1: malformed.
        let mut hash_only = vec![TELEMETRY_PROFILE_TRACKER];
        hash_only.extend_from_slice(&[0x01; TRUNCATED_HASHBYTES]);
        hash_only.push(0x02);
        assert_eq!(decode_telemetry_target_payload(&hash_only), None);
    }

    #[test]
    fn a_reader_skips_an_unknown_frame_without_losing_the_stream() {
        // Two frames back to back through the HDLC deframer: an unknown
        // type, then a wall time. The reader refuses the first by name and
        // still decodes the second — nothing about the unknown frame
        // desynchronised the stream.
        use crate::framing::hdlc::{frame as hdlc_frame, DeframeResult, Deframer};
        let mut stream = Vec::new();
        hdlc_frame(&encode_frame(0x5A, &[9, 9, 9]), &mut stream);
        let mut second = Vec::new();
        hdlc_frame(&encode_wall_time(1_790_000_000), &mut second);
        stream.extend_from_slice(&second);

        let mut deframer = Deframer::new();
        let actions: Vec<ControlAction> = deframer
            .process(&stream)
            .into_iter()
            .filter_map(|r| match r {
                DeframeResult::Frame(data) => Some(classify_control_frame(&data, ACCEPTED)),
                _ => None,
            })
            .collect();
        assert_eq!(
            actions,
            vec![
                ControlAction::Refuse {
                    refused_type: 0x5A,
                    reason: REFUSE_UNKNOWN_TYPE
                },
                ControlAction::WallTime(1_790_000_000)
            ]
        );
    }

    #[test]
    fn the_envelope_magic_is_disjoint_from_the_legacy_magic_and_packet_space() {
        // The legacy classifier keys on [0xA4, 0xA4]; the envelope must
        // never alias it, or a 21-byte envelope frame could be read as a
        // legacy config.
        assert_ne!(ENVELOPE_MAGIC, RADIO_CONFIG_MAGIC);
        assert_eq!(ENVELOPE_MAGIC[0], RADIO_CONFIG_MAGIC[0]);
        // First byte keeps the IFAC bit set — the property the legacy
        // magics rely on to stay out of packet space on a no-IFAC channel.
        assert_eq!(ENVELOPE_MAGIC[0] & 0x80, 0x80);
    }
}
