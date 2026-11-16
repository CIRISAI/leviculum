//! `/lev/status`: what became of a message a propagation node held
//! (leviculum#450, order 1394; the concept is
//! `docs/src/concepts/loquax-propagation-receipts.md`, section B).
//!
//! A sender's home asks the propagation node it uploaded to, by
//! `transient_id`, whether the recipient collected the message. Only the
//! sender and the propagation nodes know a `transient_id`
//! (`reference/LXMF/LXMF/LXMessage.py:433-434`), and the node never learns
//! who uploaded (an unidentified link, `reference/LXMF/LXMF/LXMRouter.py:2889`),
//! so the query is a pull over an unidentified link: the node answers
//! whoever holds the ids and learns nothing about the asker.
//!
//! A stock node does not answer a request path it has no handler for
//! (`reference/Reticulum/RNS/Link.py:859-905`), so the capability is
//! announced: `PN_META_CUSTOM` 0xFF in the announce's metadata map carries
//! `{"lev": {"status": 1}}`. The reference checks only that the metadata is
//! a dict (`reference/LXMF/LXMF/LXMF.py:244`), so a stock client keeps
//! taking the announce and ignores the key.
//!
//! Wire, all MessagePack: the request is an array of 32-byte bins, at most
//! [`LEV_STATUS_MAX_IDS`]; the answer is a map from each asked id (bin) to
//! a word ([`MessageStatus::word`]). A refused request is answered with the
//! bare error code a propagation node answers any refusal with
//! ([`PeerError`]).

use alloc::vec::Vec;

use crate::msgpack;
use crate::propagation::{
    MetadataEntry, PeerError, PropagationError, PropagationNodeAnnounce, TransientId,
};

/// The request path, served on the `lxmf.propagation` destination.
pub const LEV_STATUS_PATH: &str = "/lev/status";

/// The most ids one request may ask for; a larger one is refused with
/// [`PeerError::InvalidData`]. 64 ids are a 2.2 KB request and a 2.8 KB
/// answer at most, a small resource; the home asks only for its open ids
/// and splits a longer list across requests.
pub const LEV_STATUS_MAX_IDS: usize = 64;

/// The announce metadata key for custom data (`PN_META_CUSTOM`,
/// `reference/LXMF/LXMF/LXMF.py:138`).
pub const PN_META_CUSTOM: u64 = 0xFF;

/// The version of `/lev/status` this node answers, the value under
/// `{"lev": {"status": _}}`.
pub const LEV_STATUS_VERSION: u64 = 1;

/// Why a message left a propagation node's store, as the node remembers it
/// for [`crate::MESSAGE_EXPIRY_SECS`] after it left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepartureReason {
    /// Left before the reason was recorded (a departures file written by
    /// an older node). Answered as [`MessageStatus::Unknown`].
    Unrecorded,
    /// The recipient's client confirmed it in its `haves` and the node
    /// deleted it (`reference/LXMF/LXMF/LXMRouter.py:1508-1519`).
    Collected,
    /// Displaced to make room for a newer message (`get_weight`,
    /// `reference/LXMF/LXMF/LXMRouter.py:1056-1067`).
    Evicted,
    /// Older than the message expiry and never collected here.
    Expired,
    /// Addressed to the node's own mailbox and delivered to it.
    Delivered,
}

impl DepartureReason {
    /// The persisted byte.
    pub const fn code(self) -> u8 {
        match self {
            Self::Unrecorded => 0,
            Self::Collected => 1,
            Self::Evicted => 2,
            Self::Expired => 3,
            Self::Delivered => 4,
        }
    }

    /// The reason a persisted byte names; an unknown byte, from a newer
    /// node, is [`Self::Unrecorded`].
    pub const fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Collected,
            2 => Self::Evicted,
            3 => Self::Expired,
            4 => Self::Delivered,
            _ => Self::Unrecorded,
        }
    }

    /// The word of the `PN_DEPART` log line.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Unrecorded => "unrecorded",
            Self::Collected => "collected",
            Self::Evicted => "evicted",
            Self::Expired => "expired",
            Self::Delivered => "delivered",
        }
    }

    /// Whether a restored departure seeds the duplicate cache. An expired
    /// one does not: past the expiry window a peer re-offers what it
    /// received after us, and it is a new copy for us too.
    pub const fn blocks_reoffer(self) -> bool {
        !matches!(self, Self::Expired)
    }
}

/// What `/lev/status` answers for one id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageStatus {
    /// In the store, not yet collected.
    Held,
    /// The recipient's LXMF router took it from this node.
    Collected,
    /// Displaced before anyone collected it here.
    Evicted,
    /// Expired here uncollected. Not "lost": every peer gets a copy
    /// (`reference/LXMF/LXMF/LXMRouter.py:2519`), so the recipient may
    /// have collected it at another node.
    Expired,
    /// Delivered to the node's own mailbox.
    Delivered,
    /// Never held here, left more than the expiry window ago, or left
    /// before the node recorded reasons.
    Unknown,
}

impl MessageStatus {
    /// The word on the wire.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Collected => "collected",
            Self::Evicted => "evicted",
            Self::Expired => "expired",
            Self::Delivered => "delivered",
            Self::Unknown => "unknown",
        }
    }

    /// The status a word names; a word this build does not know, from a
    /// newer node, is [`Self::Unknown`].
    pub fn from_word(word: &str) -> Self {
        match word {
            "held" => Self::Held,
            "collected" => Self::Collected,
            "evicted" => Self::Evicted,
            "expired" => Self::Expired,
            "delivered" => Self::Delivered,
            _ => Self::Unknown,
        }
    }

    /// What the node answers for an id that left for `reason`.
    pub const fn of_departure(reason: DepartureReason) -> Self {
        match reason {
            DepartureReason::Unrecorded => Self::Unknown,
            DepartureReason::Collected => Self::Collected,
            DepartureReason::Evicted => Self::Evicted,
            DepartureReason::Expired => Self::Expired,
            DepartureReason::Delivered => Self::Delivered,
        }
    }
}

/// The request for `ids`.
pub fn encode_request(ids: &[TransientId]) -> Vec<u8> {
    let mut out = Vec::with_capacity(3 + ids.len() * 34);
    msgpack::array(&mut out, ids.len());
    for id in ids {
        msgpack::bin(&mut out, id);
    }
    out
}

/// The ids a request asks for, or the error code to answer with.
pub fn decode_request(data: &[u8]) -> Result<Vec<TransientId>, PeerError> {
    let mut position = 0;
    let count = msgpack::array_len(data, &mut position).map_err(|_| PeerError::InvalidData)?;
    if count > LEV_STATUS_MAX_IDS {
        return Err(PeerError::InvalidData);
    }
    let mut ids = Vec::with_capacity(count);
    for _ in 0..count {
        let raw = msgpack::read_bin(data, &mut position).map_err(|_| PeerError::InvalidData)?;
        let id: TransientId = raw.try_into().map_err(|_| PeerError::InvalidData)?;
        ids.push(id);
    }
    if position != data.len() {
        return Err(PeerError::InvalidData);
    }
    Ok(ids)
}

/// The answer: each asked id with its status, in the order asked.
pub fn encode_answer(entries: &[(TransientId, MessageStatus)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(3 + entries.len() * 44);
    msgpack::map(&mut out, entries.len());
    for (id, status) in entries {
        msgpack::bin(&mut out, id);
        msgpack::string(&mut out, status.word());
    }
    out
}

/// The answer to a refused request.
pub fn encode_refusal(error: PeerError) -> Vec<u8> {
    let mut out = Vec::with_capacity(2);
    msgpack::uint(&mut out, u64::from(error.code()));
    out
}

/// What an answer says: the statuses, or the node's refusal code.
pub fn decode_answer(
    data: &[u8],
) -> Result<Result<Vec<(TransientId, MessageStatus)>, u8>, PropagationError> {
    let mut position = 0;
    if msgpack::peek_kind(data, position)? != msgpack::Kind::Map {
        let code = msgpack::read_uint(data, &mut position)?;
        return u8::try_from(code)
            .map(Err)
            .map_err(|_| PropagationError::InvalidValue);
    }
    let count = msgpack::map_len(data, &mut position)?;
    let mut entries = Vec::with_capacity(count.min(LEV_STATUS_MAX_IDS));
    for _ in 0..count {
        let id: TransientId = msgpack::read_bin(data, &mut position)?
            .try_into()
            .map_err(|_| PropagationError::InvalidLength)?;
        let word = msgpack::read_str(data, &mut position)?;
        entries.push((id, MessageStatus::from_word(word)));
    }
    Ok(Ok(entries))
}

/// The announce metadata entry that says this node answers `/lev/status`:
/// key [`PN_META_CUSTOM`], value `{"lev": {"status": 1}}`, 14 bytes.
pub fn capability_metadata() -> MetadataEntry {
    let mut raw = Vec::with_capacity(14);
    msgpack::map(&mut raw, 1);
    msgpack::string(&mut raw, "lev");
    msgpack::map(&mut raw, 1);
    msgpack::string(&mut raw, "status");
    msgpack::uint(&mut raw, LEV_STATUS_VERSION);
    (PN_META_CUSTOM, raw)
}

/// The `/lev/status` version a propagation node announces, if any: the
/// integer under `{"lev": {"status": _}}` at [`PN_META_CUSTOM`]. Other
/// keys in either map are skipped, so the custom map can carry more.
pub fn announced_version(announce: &PropagationNodeAnnounce) -> Option<u64> {
    let (_, raw) = announce
        .metadata
        .iter()
        .find(|(key, _)| *key == PN_META_CUSTOM)?;
    let mut position = 0;
    if !find_str_key(raw, &mut position, "lev")? || !find_str_key(raw, &mut position, "status")? {
        return None;
    }
    msgpack::read_uint(raw, &mut position).ok()
}

/// Read a map at `position` and leave the cursor on the value under `key`:
/// `Some(true)` when found, `Some(false)` when the map has no such key,
/// `None` when the bytes are not a map.
fn find_str_key(raw: &[u8], position: &mut usize, key: &str) -> Option<bool> {
    let count = msgpack::map_len(raw, position).ok()?;
    for _ in 0..count {
        let matches = if msgpack::peek_kind(raw, *position).ok()? == msgpack::Kind::Str {
            msgpack::read_str(raw, position).ok()? == key
        } else {
            msgpack::skip(raw, position).ok()?;
            false
        };
        if matches {
            return Some(true);
        }
        msgpack::skip(raw, position).ok()?;
    }
    Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn a_request_round_trips_and_its_bytes_are_an_array_of_bins() {
        let ids = [[0x11u8; 32], [0x22; 32]];
        let request = encode_request(&ids);
        assert_eq!(request.len(), 1 + 2 * 34);
        assert_eq!(&request[..3], &[0x92, 0xc4, 0x20]);
        assert_eq!(decode_request(&request).unwrap(), ids.to_vec());
    }

    #[test]
    fn a_malformed_or_oversized_request_is_refused_with_invalid_data() {
        let short = {
            let mut out = Vec::new();
            msgpack::array(&mut out, 1);
            msgpack::bin(&mut out, &[0u8; 16]);
            out
        };
        assert_eq!(decode_request(&short), Err(PeerError::InvalidData));
        assert_eq!(decode_request(&[0xc0]), Err(PeerError::InvalidData));
        let mut trailing = encode_request(&[[1u8; 32]]);
        trailing.push(0xc0);
        assert_eq!(decode_request(&trailing), Err(PeerError::InvalidData));
        let at_limit = vec![[3u8; 32]; LEV_STATUS_MAX_IDS];
        assert_eq!(
            decode_request(&encode_request(&at_limit)).unwrap(),
            at_limit
        );
        let over = vec![[3u8; 32]; LEV_STATUS_MAX_IDS + 1];
        assert_eq!(
            decode_request(&encode_request(&over)),
            Err(PeerError::InvalidData)
        );
        assert!(decode_request(&encode_request(&[])).unwrap().is_empty());
    }

    #[test]
    fn an_answer_round_trips_and_a_refusal_is_its_code() {
        let entries = [
            ([1u8; 32], MessageStatus::Held),
            ([2u8; 32], MessageStatus::Collected),
            ([3u8; 32], MessageStatus::Evicted),
            ([4u8; 32], MessageStatus::Expired),
            ([5u8; 32], MessageStatus::Delivered),
            ([6u8; 32], MessageStatus::Unknown),
        ];
        let answer = encode_answer(&entries);
        assert_eq!(decode_answer(&answer).unwrap().unwrap(), entries.to_vec());
        let refusal = encode_refusal(PeerError::Throttled);
        assert_eq!(refusal, vec![0xcc, 0xf6]);
        assert_eq!(decode_answer(&refusal).unwrap(), Err(0xf6));
    }

    /// A newer node may answer a word this build does not know; it reads
    /// as unknown rather than failing the whole answer.
    #[test]
    fn an_unknown_word_reads_as_unknown() {
        let mut answer = Vec::new();
        msgpack::map(&mut answer, 1);
        msgpack::bin(&mut answer, &[9u8; 32]);
        msgpack::string(&mut answer, "forwarded");
        assert_eq!(
            decode_answer(&answer).unwrap().unwrap(),
            vec![([9u8; 32], MessageStatus::Unknown)]
        );
    }

    #[test]
    fn every_reason_byte_round_trips_and_an_unknown_one_is_unrecorded() {
        for reason in [
            DepartureReason::Unrecorded,
            DepartureReason::Collected,
            DepartureReason::Evicted,
            DepartureReason::Expired,
            DepartureReason::Delivered,
        ] {
            assert_eq!(DepartureReason::from_code(reason.code()), reason);
        }
        assert_eq!(DepartureReason::from_code(200), DepartureReason::Unrecorded);
    }

    /// The capability lookup skips other keys in either map, so the custom
    /// map can carry more than ours.
    #[test]
    fn the_capability_is_found_among_other_custom_keys() {
        let mut raw = Vec::new();
        msgpack::map(&mut raw, 2);
        msgpack::uint(&mut raw, 7);
        msgpack::string(&mut raw, "x");
        msgpack::string(&mut raw, "lev");
        msgpack::map(&mut raw, 2);
        msgpack::string(&mut raw, "other");
        msgpack::array(&mut raw, 0);
        msgpack::string(&mut raw, "status");
        msgpack::uint(&mut raw, 3);
        let announce = |metadata| PropagationNodeAnnounce {
            legacy_support: false,
            timebase: 0,
            enabled: true,
            transfer_limit_kb: 256,
            sync_limit_kb: 10240,
            stamp_cost: 16,
            stamp_cost_flexibility: 3,
            peering_cost: 18,
            metadata,
        };
        assert_eq!(
            announced_version(&announce(vec![(PN_META_CUSTOM, raw)])),
            Some(3)
        );
        let mut not_ours = Vec::new();
        msgpack::map(&mut not_ours, 1);
        msgpack::string(&mut not_ours, "sideband");
        msgpack::uint(&mut not_ours, 1);
        assert_eq!(
            announced_version(&announce(vec![(PN_META_CUSTOM, not_ours)])),
            None
        );
        assert_eq!(
            announced_version(&announce(vec![(PN_META_CUSTOM, vec![0x01])])),
            None
        );
        assert_eq!(
            announced_version(&announce(vec![capability_metadata()])),
            Some(LEV_STATUS_VERSION)
        );
    }
}
