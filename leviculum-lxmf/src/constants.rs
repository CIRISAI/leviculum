/// Per-transfer limit for message propagation, in kilobytes: Python
/// `PROPAGATION_LIMIT` (`reference/LXMF/LXMF/LXMRouter.py:55`).
///
/// No consumer in this crate yet. It binds in the propagation-node hosting
/// paths this crate does not implement: announce field 3
/// (`reference/LXMF/LXMF/LXMRouter.py:331`) and the incoming sync-resource
/// refusal in `propagation_resource_advertised`
/// (`reference/LXMF/LXMF/LXMRouter.py:2206`). The client-side `/get` request
/// limit is the delivery limit instead, see
/// [`DELIVERY_LIMIT_KB`].
pub const PROPAGATION_LIMIT_KB: u64 = 256;

/// Per-transfer limit for one LXMF delivery, in kilobytes: Python
/// `DELIVERY_LIMIT` (`reference/LXMF/LXMF/LXMRouter.py:60`).
pub const DELIVERY_LIMIT_KB: u64 = 1000;

/// The delivery limit in bytes, converted exactly as the reference applies
/// it: a receiver refuses an advertised delivery Resource strictly above
/// `delivery_per_transfer_limit*1000` uncompressed bytes
/// (`reference/LXMF/LXMF/LXMRouter.py:1979`). Exactly at the limit is
/// accepted; the comparison is `size > limit`.
pub const DELIVERY_LIMIT_BYTES: u64 = DELIVERY_LIMIT_KB * 1000;

pub const DESTINATION_LENGTH: usize = 16;
pub const SIGNATURE_LENGTH: usize = 64;
pub const TICKET_LENGTH: usize = 16;
pub const STAMP_SIZE: usize = 32;
/// Bytes the msgpack payload spends on the message timestamp:
/// `TIMESTAMP_SIZE` (`reference/LXMF/LXMF/LXMessage.py:61`).
pub const TIMESTAMP_SIZE: usize = 8;
/// Bytes the msgpack payload spends on its own framing:
/// `STRUCT_OVERHEAD` (`reference/LXMF/LXMF/LXMessage.py:62`).
pub const STRUCT_OVERHEAD: usize = 8;
pub const LXMF_OVERHEAD: usize = 112;
pub const ENCRYPTED_PACKET_MAX_CONTENT: usize = 295;
pub const LINK_PACKET_MAX_CONTENT: usize = 319;
pub const PLAIN_PACKET_MAX_CONTENT: usize = 368;
pub const PAPER_MDU: usize = 2210;
pub const COST_TICKET: u16 = 256;
pub const TICKET_EXPIRY: u64 = 1_814_400;
pub const TICKET_GRACE: u64 = 432_000;
pub const TICKET_RENEW: u64 = 1_209_600;
pub const TICKET_INTERVAL: u64 = 86_400;
pub const STAMP_COST_EXPIRY: u64 = 45 * 24 * 60 * 60;
pub const WORKBLOCK_EXPAND_ROUNDS: usize = 3000;
/// Workblock expansion rounds for the outer propagation-node stamp.
pub const WORKBLOCK_EXPAND_ROUNDS_PN: usize = 1000;
/// Workblock expansion rounds for a node-to-node peering key
/// (`WORKBLOCK_EXPAND_ROUNDS_PEERING`, `reference/LXMF/LXMF/LXStamper.py:14`).
pub const WORKBLOCK_EXPAND_ROUNDS_PEERING: usize = 25;

pub const FIELD_EMBEDDED_LXMS: i64 = 0x01;
pub const FIELD_TELEMETRY: i64 = 0x02;
pub const FIELD_TELEMETRY_STREAM: i64 = 0x03;
pub const FIELD_ICON_APPEARANCE: i64 = 0x04;
pub const FIELD_FILE_ATTACHMENTS: i64 = 0x05;
pub const FIELD_IMAGE: i64 = 0x06;
pub const FIELD_AUDIO: i64 = 0x07;
pub const AUDIO_MODE_CUSTOM: u8 = 0xFF;
pub const FIELD_THREAD: i64 = 0x08;
pub const FIELD_COMMANDS: i64 = 0x09;
pub const FIELD_RESULTS: i64 = 0x0A;
pub const FIELD_COMMANDS_EXECUTED: i64 = FIELD_RESULTS;
pub const FIELD_GROUP: i64 = 0x0B;
pub const FIELD_TICKET: i64 = 0x0C;
pub const FIELD_EVENT: i64 = 0x0D;
pub const FIELD_RNR_REFS: i64 = 0x0E;
pub const FIELD_RENDERER: i64 = 0x0F;
pub const FIELD_CUSTOM_TYPE: i64 = 0xFB;
pub const FIELD_CUSTOM_DATA: i64 = 0xFC;
pub const FIELD_CUSTOM_META: i64 = 0xFD;
pub const FIELD_NON_SPECIFIC: i64 = 0xFE;
pub const FIELD_DEBUG: i64 = 0xFF;
pub const FIELD_REPLY_TO: i64 = 0x30;
pub const FIELD_REPLY_QUOTE: i64 = 0x31;
pub const FIELD_REACTION: i64 = 0x40;
pub const FIELD_COMMENT: i64 = 0x41;
pub const FIELD_CONTINUATION: i64 = 0x42;

/// The single-packet size limits are written here as literals; Python
/// computes them. This module proves the two agree.
///
/// Python derives every one of them from `RNS.Reticulum.MTU`
/// (`reference/Reticulum/RNS/Reticulum.py:93`), so if upstream moves the MTU
/// its thresholds move with it and ours do not. Both stacks would keep
/// delivering, each choosing a different carrier for the same message: a
/// semantic divergence with no symptom on the air. The day an MTU constant
/// moves — ours, or an upstream change we track — this is what fails instead
/// of the literal drifting quietly.
///
/// The derivations are Python's, term for term, against our own
/// `leviculum-core` constants:
///
/// * `LXMF_OVERHEAD` (`reference/LXMF/LXMF/LXMessage.py:63`)
/// * `ENCRYPTED_PACKET_MDU` (`reference/LXMF/LXMF/LXMessage.py:68`), from
///   `ENCRYPTED_MDU` (`reference/Reticulum/RNS/Packet.py:106`)
/// * `ENCRYPTED_PACKET_MAX_CONTENT`
///   (`reference/LXMF/LXMF/LXMessage.py:79`) — note the `+ DESTINATION_LENGTH`
///   term: an opportunistic packet infers the destination hash from its own
///   header instead of carrying it.
/// * `LINK_PACKET_MAX_CONTENT` (`reference/LXMF/LXMF/LXMessage.py:90`), from
///   `MDU` (`reference/Reticulum/RNS/Link.py:73`)
/// * `PLAIN_PACKET_MAX_CONTENT` (`reference/LXMF/LXMF/LXMessage.py:95`), from
///   `PLAIN_MDU` (`reference/Reticulum/RNS/Packet.py:110`)
#[cfg(test)]
mod derivation {
    use super::{
        DESTINATION_LENGTH, ENCRYPTED_PACKET_MAX_CONTENT, LINK_PACKET_MAX_CONTENT, LXMF_OVERHEAD,
        PLAIN_PACKET_MAX_CONTENT, SIGNATURE_LENGTH, STRUCT_OVERHEAD, TICKET_LENGTH, TIMESTAMP_SIZE,
    };
    use leviculum_core::constants::{
        AES_BLOCK_SIZE, ED25519_SIGNATURE_SIZE, MDU, MTU, TOKEN_OVERHEAD, TRUNCATED_HASHBYTES,
        X25519_KEY_SIZE,
    };
    use leviculum_core::resource::STANDARD_LINK_MDU;

    /// `RNS.Packet.ENCRYPTED_MDU` (`reference/Reticulum/RNS/Packet.py:106`).
    ///
    /// Python spells the third term `RNS.Identity.KEYSIZE//16` — 512 bits of
    /// identity key over 16, i.e. 32 — which is the length of the X25519
    /// ephemeral public key the encrypted payload carries ahead of the token.
    /// That is what our [`X25519_KEY_SIZE`] names directly.
    fn encrypted_mdu() -> usize {
        ((MDU - TOKEN_OVERHEAD - X25519_KEY_SIZE) / AES_BLOCK_SIZE) * AES_BLOCK_SIZE - 1
    }

    /// The three lengths `LXMF_OVERHEAD` is built from are the transport's,
    /// not LXMF's own: a drift in either crate's idea of a truncated hash or
    /// an Ed25519 signature would otherwise pass the overhead check below by
    /// moving both sides of it at once.
    #[test]
    fn lxmf_field_lengths_are_the_transports() {
        assert_eq!(DESTINATION_LENGTH, TRUNCATED_HASHBYTES);
        assert_eq!(TICKET_LENGTH, TRUNCATED_HASHBYTES);
        assert_eq!(SIGNATURE_LENGTH, ED25519_SIGNATURE_SIZE);
    }

    #[test]
    fn lxmf_overhead_is_the_sum_of_its_parts() {
        assert_eq!(
            LXMF_OVERHEAD,
            2 * DESTINATION_LENGTH + SIGNATURE_LENGTH + TIMESTAMP_SIZE + STRUCT_OVERHEAD
        );
    }

    /// The threshold the opportunistic-to-direct fallback turns on. Bending
    /// the MTU here and leaving the literal alone is exactly the drift that
    /// would otherwise send a message over a link while a Python peer sent
    /// the same message in one packet.
    #[test]
    fn encrypted_packet_max_content_is_derived_from_the_mtu() {
        let encrypted_packet_mdu = encrypted_mdu() + TIMESTAMP_SIZE;
        assert_eq!(
            ENCRYPTED_PACKET_MAX_CONTENT,
            encrypted_packet_mdu - LXMF_OVERHEAD + DESTINATION_LENGTH,
            "the opportunistic single-packet limit no longer matches an MTU of {MTU}"
        );
    }

    /// `STANDARD_LINK_MDU` is our `RNS.Link.MDU`: a class constant derived
    /// from the default MTU, which link MTU discovery does not move.
    #[test]
    fn link_packet_max_content_is_derived_from_the_link_mdu() {
        assert_eq!(
            LINK_PACKET_MAX_CONTENT,
            STANDARD_LINK_MDU - LXMF_OVERHEAD,
            "the single-packet-over-a-link limit no longer matches an MTU of {MTU}"
        );
    }

    #[test]
    fn plain_packet_max_content_is_derived_from_the_mtu() {
        assert_eq!(
            PLAIN_PACKET_MAX_CONTENT,
            MDU - LXMF_OVERHEAD + DESTINATION_LENGTH
        );
    }
}
