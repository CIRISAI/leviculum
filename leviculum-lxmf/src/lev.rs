//! The `lev` namespace: Leviculum's own LXMF extension, carried in the
//! custom field pair the reference keeps for client protocols
//! (`reference/LXMF/LXMF/LXMF.py:34-45`): `FIELD_CUSTOM_TYPE` (0xFB) holds
//! the type `b"lev"` and `FIELD_CUSTOM_DATA` (0xFC) a msgpack map, written
//! straight into the field, not wrapped in a `bin`.
//!
//! | key | value | meaning |
//! |---|---|---|
//! | `0x00` | uint | capabilities; bit 0: "I send read receipts and want them" |
//! | `0x01` | `[uint32 read_at, [bin32 message id, ...]]` | a read receipt |
//! | `0x02` | `[bin16 sender, uint64 sent_ms, uint epoch]` | a group copy: who wrote it, when by their clock, the membership it was sent under |
//! | `0x04` | `[bin16 group, bin64 public key, str name, str admin's name, uint epoch]` | an invitation to a group |
//! | `0x05` | `[uint epoch, [[bin16 address, str name, uint flags], ...]]` | a group's membership; a member leaving sends itself alone, flagged [`MEMBER_LEFT`] |
//!
//! Capability bit 1 says "I understand groups": a group's home sends such a
//! member its copies with key `0x02` and the bare text, and every other
//! member the text with the sender's name in front
//! (`docs/drafts/group-chats-study-2026-11.md`, "The fan-out field" and
//! "Interop for stock clients"). Keys `0x03` and `0x06` are the study's
//! group status and digest, kept free for them.
//!
//! Versions move by keys, never by the type: a reader skips a key it does
//! not know, and a later key never changes what an earlier one means. The
//! keys above 0xFF that `LXMF.py:29-32` suggests for experiments are no
//! option: rsLXMF, under Ratspeak, truncates every field key to a `u8`, so
//! 0x1FB would land on that client's 0xFB. Every client checked shows a
//! normal message with this pair in it as it shows one without
//! (`docs/src/concepts/loquax-read-receipts.md`, which is also this
//! namespace's public description).
//!
//! This codec produces and reads the two fields and nothing more. When a
//! receipt is sent, and what it does to a message's state, is the
//! client's business.

use crate::constants::{FIELD_CUSTOM_DATA, FIELD_CUSTOM_TYPE};
use crate::message::Field;
use crate::msgpack::{self, Kind};
use alloc::vec::Vec;

/// The type `FIELD_CUSTOM_TYPE` names the namespace by.
pub const LEV_TYPE: &[u8] = b"lev";

/// The map key of the capabilities.
pub const KEY_CAPABILITIES: i64 = 0x00;

/// The map key of a read receipt.
pub const KEY_RECEIPT: i64 = 0x01;

/// The map key of a group copy's sender and time.
pub const KEY_GROUP_COPY: i64 = 0x02;

/// The map key of a group invitation.
pub const KEY_GROUP_INVITE: i64 = 0x04;

/// The map key of a group's membership.
pub const KEY_GROUP_MEMBERS: i64 = 0x05;

/// Capability bit 0: the sender sends read receipts and wants them.
pub const CAP_READ_RECEIPTS: u64 = 1;

/// Capability bit 1: the sender understands groups.
pub const CAP_GROUPS: u64 = 2;

/// A member's flag in [`Members`]: it has left, or was removed.
pub const MEMBER_LEFT: u64 = 1;

/// A member's flag in [`Members`]: it is the group's admin.
pub const MEMBER_ADMIN: u64 = 2;

/// The length of an identity's public key, X25519 then Ed25519, as an
/// invitation carries it (the `lxma://` code carries the same 64 bytes).
pub const PUBLIC_KEY_BYTES: usize = 64;

/// The most message ids one receipt lists: what fits a single-packet
/// (opportunistic) message, 295 bytes of content
/// (`crate::constants::ENCRYPTED_PACKET_MAX_CONTENT`), at 34 bytes an id
/// beside the namespace's own bytes.
pub const RECEIPT_IDS_MAX: usize = 7;

/// "These messages of yours were read": the reader's clock when the first
/// of them came on screen, unix seconds, and their full LXMF message ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub read_at: u32,
    pub ids: Vec<[u8; 32]>,
}

impl Receipt {
    /// The receipts for `ids`, at most [`RECEIPT_IDS_MAX`] each, in order.
    pub fn batches(read_at: u32, ids: &[[u8; 32]]) -> Vec<Self> {
        ids.chunks(RECEIPT_IDS_MAX)
            .map(|chunk| Self {
                read_at,
                ids: chunk.to_vec(),
            })
            .collect()
    }
}

/// What a group's home adds to the copy of a member's message it sends a
/// member that understands groups: the copy's LXMF source is the group, so
/// the writer goes here, with the writer's own timestamp, which together
/// with the writer is the copy's dedup key (study 6.2), and the membership
/// epoch the home sent it under ("your list is stale" when it is newer
/// than the reader's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupCopy {
    /// The writer's LXMF address.
    pub sender: [u8; 16],
    /// The writer's message timestamp, milliseconds since the epoch.
    pub sent_ms: u64,
    /// The membership epoch.
    pub epoch: u64,
}

/// "Join this group": its address and key, so the reader can write to it
/// before any announce of it was heard, its name, the admin's name for the
/// trust sentence, and the membership epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    pub group: [u8; 16],
    pub public_key: [u8; PUBLIC_KEY_BYTES],
    pub name: alloc::string::String,
    pub admin_name: alloc::string::String,
    pub epoch: u64,
}

/// One member as [`Members`] lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub address: [u8; 16],
    pub name: alloc::string::String,
    /// [`MEMBER_LEFT`], [`MEMBER_ADMIN`]; unknown bits are kept.
    pub flags: u64,
}

/// A group's membership at an epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Members {
    pub epoch: u64,
    pub members: Vec<Member>,
}

/// What the `lev` namespace of one message says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lev {
    /// Key `0x00`; `None` when the map has none.
    pub capabilities: Option<u64>,
    /// Key `0x01`.
    pub receipt: Option<Receipt>,
    /// Key `0x02`.
    pub group_copy: Option<GroupCopy>,
    /// Key `0x04`.
    pub invite: Option<Invite>,
    /// Key `0x05`.
    pub members: Option<Members>,
}

impl Lev {
    /// The capability mark every message of a client that sends and wants
    /// read receipts carries: `{0x00: 1}`.
    pub fn mark() -> Self {
        Self {
            capabilities: Some(CAP_READ_RECEIPTS),
            ..Self::default()
        }
    }

    /// The mark with a receipt in it.
    pub fn mark_with(receipt: Receipt) -> Self {
        Self {
            receipt: Some(receipt),
            ..Self::mark()
        }
    }

    /// Whether the sender said it sends and wants read receipts.
    pub fn supports_receipts(&self) -> bool {
        self.capabilities
            .is_some_and(|caps| caps & CAP_READ_RECEIPTS != 0)
    }

    /// Whether the sender said it understands groups.
    pub fn supports_groups(&self) -> bool {
        self.capabilities.is_some_and(|caps| caps & CAP_GROUPS != 0)
    }

    /// The two fields, `FIELD_CUSTOM_TYPE` first. The map's keys go in
    /// ascending order, each once, so the same [`Lev`] is always the same
    /// bytes: a group copy must be (study 2.4).
    pub fn fields(&self) -> [Field; 2] {
        let mut kind = Vec::new();
        msgpack::bin(&mut kind, LEV_TYPE);
        let entries = usize::from(self.capabilities.is_some())
            + usize::from(self.receipt.is_some())
            + usize::from(self.group_copy.is_some())
            + usize::from(self.invite.is_some())
            + usize::from(self.members.is_some());
        let mut data = Vec::new();
        msgpack::map(&mut data, entries);
        if let Some(caps) = self.capabilities {
            msgpack::int(&mut data, KEY_CAPABILITIES);
            msgpack::uint(&mut data, caps);
        }
        if let Some(receipt) = &self.receipt {
            msgpack::int(&mut data, KEY_RECEIPT);
            msgpack::array(&mut data, 2);
            msgpack::uint(&mut data, u64::from(receipt.read_at));
            msgpack::array(&mut data, receipt.ids.len());
            for id in &receipt.ids {
                msgpack::bin(&mut data, id);
            }
        }
        if let Some(copy) = &self.group_copy {
            msgpack::int(&mut data, KEY_GROUP_COPY);
            msgpack::array(&mut data, 3);
            msgpack::bin(&mut data, &copy.sender);
            msgpack::uint(&mut data, copy.sent_ms);
            msgpack::uint(&mut data, copy.epoch);
        }
        if let Some(invite) = &self.invite {
            msgpack::int(&mut data, KEY_GROUP_INVITE);
            msgpack::array(&mut data, 5);
            msgpack::bin(&mut data, &invite.group);
            msgpack::bin(&mut data, &invite.public_key);
            msgpack::string(&mut data, &invite.name);
            msgpack::string(&mut data, &invite.admin_name);
            msgpack::uint(&mut data, invite.epoch);
        }
        if let Some(members) = &self.members {
            msgpack::int(&mut data, KEY_GROUP_MEMBERS);
            msgpack::array(&mut data, 2);
            msgpack::uint(&mut data, members.epoch);
            msgpack::array(&mut data, members.members.len());
            for member in &members.members {
                msgpack::array(&mut data, 3);
                msgpack::bin(&mut data, &member.address);
                msgpack::string(&mut data, &member.name);
                msgpack::uint(&mut data, member.flags);
            }
        }
        [(FIELD_CUSTOM_TYPE, kind), (FIELD_CUSTOM_DATA, data)]
    }

    /// `fields` with the namespace's pair put in, replacing a pair already
    /// there: the custom pair holds one type a message.
    pub fn put_into(&self, fields: &mut Vec<Field>) {
        fields.retain(|(key, _)| *key != FIELD_CUSTOM_TYPE && *key != FIELD_CUSTOM_DATA);
        fields.extend(self.fields());
    }

    /// Read a message's fields. `None` when its custom type is not `lev`
    /// (another client's protocol, or none). A `lev` type whose data is not
    /// a map says nothing: `Some` of the empty [`Lev`]. Unknown keys are
    /// skipped, a repeated key resolves to its last occurrence, as a Python
    /// dict does, and a receipt that is not `[uint32, [bin32, ...]]` is
    /// none; an id of another length in it is passed over. A group key of
    /// another shape is none too; elements after the ones listed are
    /// skipped, so a later version may append to an array.
    pub fn from_fields(fields: &[Field]) -> Option<Self> {
        let value = |want: i64| {
            fields
                .iter()
                .rev()
                .find(|(key, _)| *key == want)
                .map(|(_, v)| v.as_slice())
        };
        if bytes(value(FIELD_CUSTOM_TYPE)?)? != LEV_TYPE {
            return None;
        }
        let mut lev = Self::default();
        let Some(d) = value(FIELD_CUSTOM_DATA) else {
            return Some(lev);
        };
        let mut p = 0;
        let Ok(entries) = msgpack::map_len(d, &mut p) else {
            return Some(lev);
        };
        for _ in 0..entries {
            let (Ok(key), Ok(value)) = (msgpack::raw(d, &mut p), msgpack::raw(d, &mut p)) else {
                break;
            };
            let mut kp = 0;
            match msgpack::read_int(key, &mut kp) {
                Ok(KEY_CAPABILITIES) => {
                    let mut vp = 0;
                    lev.capabilities = msgpack::read_uint(value, &mut vp).ok();
                }
                Ok(KEY_RECEIPT) => lev.receipt = receipt(value),
                Ok(KEY_GROUP_COPY) => lev.group_copy = group_copy(value),
                Ok(KEY_GROUP_INVITE) => lev.invite = invite(value),
                Ok(KEY_GROUP_MEMBERS) => lev.members = members(value),
                _ => {}
            }
        }
        Some(lev)
    }
}

fn receipt(d: &[u8]) -> Option<Receipt> {
    let mut p = 0;
    if msgpack::array_len(d, &mut p).ok()? != 2 {
        return None;
    }
    let read_at = u32::try_from(msgpack::read_uint(d, &mut p).ok()?).ok()?;
    let count = msgpack::array_len(d, &mut p).ok()?;
    let mut ids = Vec::new();
    for _ in 0..count {
        let raw = msgpack::raw(d, &mut p).ok()?;
        if let Some(id) = bytes(raw).and_then(|b| <[u8; 32]>::try_from(b).ok()) {
            ids.push(id);
        }
    }
    Some(Receipt { read_at, ids })
}

fn group_copy(d: &[u8]) -> Option<GroupCopy> {
    let mut p = 0;
    if msgpack::array_len(d, &mut p).ok()? < 3 {
        return None;
    }
    let sender = fixed(d, &mut p)?;
    let sent_ms = msgpack::read_uint(d, &mut p).ok()?;
    let epoch = msgpack::read_uint(d, &mut p).ok()?;
    Some(GroupCopy {
        sender,
        sent_ms,
        epoch,
    })
}

fn invite(d: &[u8]) -> Option<Invite> {
    let mut p = 0;
    if msgpack::array_len(d, &mut p).ok()? < 5 {
        return None;
    }
    Some(Invite {
        group: fixed(d, &mut p)?,
        public_key: fixed(d, &mut p)?,
        name: text(d, &mut p)?,
        admin_name: text(d, &mut p)?,
        epoch: msgpack::read_uint(d, &mut p).ok()?,
    })
}

fn members(d: &[u8]) -> Option<Members> {
    let mut p = 0;
    if msgpack::array_len(d, &mut p).ok()? < 2 {
        return None;
    }
    let epoch = msgpack::read_uint(d, &mut p).ok()?;
    let count = msgpack::array_len(d, &mut p).ok()?;
    let mut members = Vec::new();
    for _ in 0..count {
        let raw = msgpack::raw(d, &mut p).ok()?;
        members.push(member(raw)?);
    }
    Some(Members { epoch, members })
}

fn member(d: &[u8]) -> Option<Member> {
    let mut p = 0;
    if msgpack::array_len(d, &mut p).ok()? < 3 {
        return None;
    }
    Some(Member {
        address: fixed(d, &mut p)?,
        name: text(d, &mut p)?,
        flags: msgpack::read_uint(d, &mut p).ok()?,
    })
}

/// The next value as exactly `N` bytes.
fn fixed<const N: usize>(d: &[u8], p: &mut usize) -> Option<[u8; N]> {
    let raw = msgpack::raw(d, p).ok()?;
    bytes(raw).and_then(|b| <[u8; N]>::try_from(b).ok())
}

/// The next value as text: a `str`, or a `bin` holding UTF-8.
fn text(d: &[u8], p: &mut usize) -> Option<alloc::string::String> {
    let raw = msgpack::raw(d, p).ok()?;
    core::str::from_utf8(bytes(raw)?)
        .ok()
        .map(alloc::string::String::from)
}

/// The bytes of a `bin` or a `str` value.
fn bytes(value: &[u8]) -> Option<&[u8]> {
    let mut p = 0;
    match msgpack::peek_kind(value, 0).ok()? {
        Kind::Str => msgpack::read_str(value, &mut p).ok().map(str::as_bytes),
        _ => msgpack::read_bin(value, &mut p).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::borrow::ToOwned;

    fn hex(bytes: &[u8]) -> alloc::string::String {
        bytes.iter().map(|b| alloc::format!("{b:02x}")).collect()
    }

    #[test]
    fn the_mark_is_twelve_bytes_on_the_wire() {
        let [(tk, tv), (dk, dv)] = Lev::mark().fields();
        assert_eq!((tk, hex(&tv)), (FIELD_CUSTOM_TYPE, "c4036c6576".into()));
        assert_eq!((dk, hex(&dv)), (FIELD_CUSTOM_DATA, "810001".into()));
        // `cc fb` and `cc fc` are the keys of the message's field map.
        assert_eq!(2 + tv.len() + 2 + dv.len(), 12);
        let read = Lev::from_fields(&Lev::mark().fields()).expect("lev");
        assert_eq!(read, Lev::mark());
        assert!(read.supports_receipts());
    }

    #[test]
    fn a_receipt_reads_back_as_written() {
        let receipt = Receipt {
            read_at: 1_800_000_000,
            ids: alloc::vec![[0xab; 32], [0xcd; 32]],
        };
        let lev = Lev::mark_with(receipt.clone());
        let [_, (_, data)] = lev.fields();
        assert!(hex(&data).starts_with("82000101" /* map 2, 0:1, key 1 */));
        assert!(hex(&data).contains("92ce6b49d20092c420"));
        assert_eq!(Lev::from_fields(&lev.fields()), Some(lev));
    }

    #[test]
    fn unknown_keys_are_skipped_and_another_type_is_not_ours() {
        let mut data = Vec::new();
        msgpack::map(&mut data, 3);
        msgpack::int(&mut data, 0x7f);
        msgpack::string(&mut data, "later");
        msgpack::int(&mut data, KEY_CAPABILITIES);
        msgpack::uint(&mut data, CAP_READ_RECEIPTS | 0b100);
        msgpack::string(&mut data, "x");
        msgpack::nil(&mut data);
        let mut kind = Vec::new();
        msgpack::string(&mut kind, "lev");
        let fields = alloc::vec![(FIELD_CUSTOM_TYPE, kind), (FIELD_CUSTOM_DATA, data.clone())];
        let lev = Lev::from_fields(&fields).expect("a str type is read too");
        assert!(lev.supports_receipts());
        assert_eq!(lev.receipt, None);

        let mut other = Vec::new();
        msgpack::bin(&mut other, b"ratspeak.chat.v2");
        assert_eq!(
            Lev::from_fields(&[(FIELD_CUSTOM_TYPE, other), (FIELD_CUSTOM_DATA, data)]),
            None
        );
        assert_eq!(Lev::from_fields(&[]), None);
    }

    #[test]
    fn what_says_nothing_is_the_empty_namespace() {
        let mut kind = Vec::new();
        msgpack::bin(&mut kind, LEV_TYPE);
        let alone = alloc::vec![(FIELD_CUSTOM_TYPE, kind.clone())];
        assert_eq!(Lev::from_fields(&alone), Some(Lev::default()));
        assert!(!Lev::default().supports_receipts());
        let bin_data = alloc::vec![
            (FIELD_CUSTOM_TYPE, kind.clone()),
            (FIELD_CUSTOM_DATA, alloc::vec![0xc4, 0x01, 0x00])
        ];
        assert_eq!(Lev::from_fields(&bin_data), Some(Lev::default()));
        // Capabilities without bit 0, and a receipt of the wrong shape.
        let mut data = Vec::new();
        msgpack::map(&mut data, 2);
        msgpack::int(&mut data, KEY_CAPABILITIES);
        msgpack::uint(&mut data, 0);
        msgpack::int(&mut data, KEY_RECEIPT);
        msgpack::array(&mut data, 1);
        msgpack::uint(&mut data, 5);
        let lev =
            Lev::from_fields(&[(FIELD_CUSTOM_TYPE, kind), (FIELD_CUSTOM_DATA, data)]).expect("lev");
        assert!(!lev.supports_receipts());
        assert_eq!(lev.receipt, None);
    }

    #[test]
    fn receipts_go_seven_ids_at_most() {
        let ids: Vec<[u8; 32]> = (0..16u8).map(|i| [i; 32]).collect();
        let batches = Receipt::batches(9, &ids);
        assert_eq!(
            batches.iter().map(|r| r.ids.len()).collect::<Vec<_>>(),
            [7, 7, 2]
        );
        let again: Vec<[u8; 32]> = batches.iter().flat_map(|r| r.ids.clone()).collect();
        assert_eq!(again, ids);
        assert!(Receipt::batches(9, &[]).is_empty());
    }

    #[test]
    fn put_into_replaces_another_custom_pair() {
        let mut fields = alloc::vec![
            (0x30, alloc::vec![0xc4, 0x00]),
            (FIELD_CUSTOM_TYPE, alloc::vec![0xc4, 0x01, b'x']),
            (FIELD_CUSTOM_DATA, alloc::vec![0x80]),
        ];
        Lev::mark().put_into(&mut fields);
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].0, 0x30);
        assert_eq!(Lev::from_fields(&fields), Some(Lev::mark()));
    }

    fn copy() -> GroupCopy {
        GroupCopy {
            sender: [0x11; 16],
            sent_ms: 1_700_000_000_123,
            epoch: 3,
        }
    }

    /// The study's 0x02 as bytes: `02` the key, `93` an array of three,
    /// `c4 10` and the sixteen bytes of the writer, `cf` and the eight of
    /// its timestamp in milliseconds, `03` the epoch.
    #[test]
    fn a_group_copy_is_the_studys_bytes() {
        let lev = Lev {
            capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
            group_copy: Some(copy()),
            ..Lev::default()
        };
        let [_, (_, data)] = lev.fields();
        assert_eq!(
            hex(&data),
            alloc::format!(
                "82{}{}{}",
                "0003",
                "0293c410".to_owned() + &"11".repeat(16),
                "cf0000018bcfe5687b03"
            )
        );
        let read = Lev::from_fields(&lev.fields()).expect("lev");
        assert_eq!(read, lev);
        assert!(read.supports_groups() && read.supports_receipts());
        assert!(!Lev::mark().supports_groups());
    }

    #[test]
    fn an_invitation_and_a_membership_read_back_as_written() {
        let lev = Lev {
            capabilities: Some(CAP_GROUPS),
            invite: Some(Invite {
                group: [0x22; 16],
                public_key: [0x33; PUBLIC_KEY_BYTES],
                name: "Familie".into(),
                admin_name: "Lew".into(),
                epoch: 1,
            }),
            members: Some(Members {
                epoch: 1,
                members: alloc::vec![
                    Member {
                        address: [0x44; 16],
                        name: "Lew".into(),
                        flags: MEMBER_ADMIN,
                    },
                    Member {
                        address: [0x55; 16],
                        name: "Anna".into(),
                        flags: 0,
                    },
                ],
            }),
            ..Lev::default()
        };
        let [_, (_, data)] = lev.fields();
        let h = hex(&data);
        // map 3; 0: 2; 4: [bin16, bin64 (`c4 40`), "Familie", "Lew", 1].
        assert!(h.starts_with("83000204" /* map 3, 0:2, key 4 */), "{h}");
        assert!(
            h.contains(&("95c410".to_owned() + &"22".repeat(16) + "c440")),
            "{h}"
        );
        assert!(h.contains("a746616d696c6965a34c657701"), "{h}");
        // 5: [1, [[bin16, "Lew", 2], [bin16, "Anna", 0]]].
        assert!(h.contains(&("0592019293c410".to_owned() + &"44".repeat(16) + "a34c657702")));
        assert!(h.ends_with(&("93c410".to_owned() + &"55".repeat(16) + "a4416e6e6100")));
        assert_eq!(Lev::from_fields(&lev.fields()), Some(lev));
    }

    #[test]
    fn a_group_key_of_another_shape_says_nothing_and_a_longer_one_is_read() {
        let mut kind = Vec::new();
        msgpack::bin(&mut kind, LEV_TYPE);
        let mut data = Vec::new();
        msgpack::map(&mut data, 2);
        // A writer of 8 bytes: not a copy.
        msgpack::int(&mut data, KEY_GROUP_COPY);
        msgpack::array(&mut data, 3);
        msgpack::bin(&mut data, &[1; 8]);
        msgpack::uint(&mut data, 5);
        msgpack::uint(&mut data, 1);
        // A membership whose arrays carry a fourth element a later version
        // added.
        msgpack::int(&mut data, KEY_GROUP_MEMBERS);
        msgpack::array(&mut data, 3);
        msgpack::uint(&mut data, 7);
        msgpack::array(&mut data, 1);
        msgpack::array(&mut data, 4);
        msgpack::bin(&mut data, &[9; 16]);
        msgpack::string(&mut data, "Anna");
        msgpack::uint(&mut data, MEMBER_LEFT);
        msgpack::string(&mut data, "later");
        msgpack::string(&mut data, "later too");
        let lev =
            Lev::from_fields(&[(FIELD_CUSTOM_TYPE, kind), (FIELD_CUSTOM_DATA, data)]).expect("lev");
        assert_eq!(lev.group_copy, None);
        assert_eq!(
            lev.members,
            Some(Members {
                epoch: 7,
                members: alloc::vec![Member {
                    address: [9; 16],
                    name: "Anna".into(),
                    flags: MEMBER_LEFT,
                }],
            })
        );
    }
}
