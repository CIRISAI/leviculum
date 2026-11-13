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
//! | `0x03` | `[bin32 original, uint delivered, uint read, uint members, uint readers, uint on_device?, [[bin16 member, uint proven_at, uint on_device_at, uint read_at], ...]?]` | a group's status of one message, home to its writer: how many copies are proven and read, of how many, and how many members send receipts; since 1466 how many are on their member's device, and each member's rungs with their times (0: not yet) |
//! | `0x04` | `[bin16 group, bin64 public key, str name, str admin's name, uint epoch]` | an invitation to a group |
//! | `0x05` | `[uint epoch, [[bin16 address, str name, uint flags], ...], uint standing]` | a group's membership; a member leaving sends itself alone, flagged [`MEMBER_LEFT`]; the third element is there only when the group [`GROUP_ENDED`] |
//! | `0x06` | `[[bin16 sender, uint64 sent_ms, str text, uint left_out?], ...]` | a digest: the lines a group's home held for a slow member, one message for all of them; `left_out` only on a line that stands for what was not sent ([`LEFT_OUT_PHOTO`] and its siblings) |
//! | `0x07` | `[uint32 received_at, [bin32 message id, ...]]` | a device receipt: these messages of yours are on my device (named a propagation receipt when it was built for collected post) |
//! | `0x08` | `bin16 writer` | the address that wrote this map (1482); a reader takes the map as the word of the message's source only when this is the source |
//!
//! Key `0x08` (1482) is against relays. A bot such as LXMF-Tools'
//! distribution group passes a member's fields through unchanged into the
//! copy it signs itself, so a capability mark on the copy is the writer's,
//! not the bot's: a reader that took it as the bot's would send the bot
//! device and read receipts, which the bot hands every member back as
//! empty posts. Every map a Loquax client sends names its writer; a reader
//! takes a map whose writer is another address as no `lev` at all
//! ([`Lev::from_fields_of`]). A map without the key (a writer before 1482)
//! is taken as the source's, as before.
//!
//! Capability bit 2 says "I say when your post is on my device, and want
//! mine said": a recipient's device answers such a sender's message with
//! key `0x07` once it has it, however it came (1393 for propagated post,
//! `docs/src/concepts/loquax-propagation-receipts.md`; 1462 for direct
//! post too, `docs/src/concepts/loquax-read-receipts.md`, section 6). The
//! transport proof is no such word: a mailbox signs it with the recipient's
//! identity as soon as it archived the message.
//!
//! Capability bit 1 says "I understand groups": a group's home sends such a
//! member its copies with key `0x02`. Every member, that one too, gets the
//! text with the sender's name in front, `"Anna: text"`, one wire shape for
//! all; a reader that understands groups takes the writer from `0x02` and
//! strips the name it knows for them, and the prefix is what still names
//! the writer when the field is lost on the way (1388, 2026-11-04; the
//! study, `docs/drafts/group-chats-study-2026-11.md`, "The fan-out field"
//! and "Interop for stock clients", said the bare text). A copy without
//! words says what it carries and from whom instead, `"Photo from Anna"`
//! ([`StandIn`]).
//!
//! Key `0x03` (1307 pass four) is the study's group status (2.3) with one
//! element appended, `readers`: the members whose newest message said bit
//! 0. The writer's "read by N of M" counts only those, so a member with
//! receipts off is never waited for.
//!
//! 1466 appends two more: `on_device`, the copies whose member's device
//! said it has them (its `0x07`, or its read receipt, which includes it),
//! and each counted member's rungs, so the writer's Info names who holds
//! the message at which rung and since when. A proof is no device's word in
//! a group either: a member's mailbox signs it with the member's identity.
//! The writer's two ticks are `on_device` of every copy; a status of five
//! elements (a home before 1466) never says it, so its writer stays at one
//! tick. A reader before 1466 reads the first five and skips the rest.
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

use crate::constants::{
    FIELD_AUDIO, FIELD_CUSTOM_DATA, FIELD_CUSTOM_TYPE, FIELD_FILE_ATTACHMENTS, FIELD_IMAGE,
    FIELD_TELEMETRY,
};
use crate::message::Field;
use crate::msgpack::{self, Kind};
use alloc::string::String;
use alloc::vec::Vec;

/// The type `FIELD_CUSTOM_TYPE` names the namespace by.
pub const LEV_TYPE: &[u8] = b"lev";

/// The map key of the capabilities.
pub const KEY_CAPABILITIES: i64 = 0x00;

/// The map key of a read receipt.
pub const KEY_RECEIPT: i64 = 0x01;

/// The map key of a group copy's sender and time.
pub const KEY_GROUP_COPY: i64 = 0x02;

/// The map key of a group's status of one message (study 2.3).
pub const KEY_GROUP_STATUS: i64 = 0x03;

/// The map key of a group invitation.
pub const KEY_GROUP_INVITE: i64 = 0x04;

/// The map key of a group's membership.
pub const KEY_GROUP_MEMBERS: i64 = 0x05;

/// The map key of a group's digest for a slow member (study 5.3).
pub const KEY_GROUP_DIGEST: i64 = 0x06;

/// The map key of a propagation receipt (1393).
pub const KEY_PROPAGATION_RECEIPT: i64 = 0x07;

/// The map key of the map's writer (1482).
pub const KEY_WRITER: i64 = 0x08;

/// What a digest line stands for when the home left its message out
/// (1348): a photo, a voice message or a file too large for a slow member,
/// or a message no longer there. The line's text is then the home's
/// English sentence for it, and a reader that knows the value says it in
/// its own language instead.
pub const LEFT_OUT_PHOTO: u64 = 1;
/// See [`LEFT_OUT_PHOTO`].
pub const LEFT_OUT_VOICE: u64 = 2;
/// See [`LEFT_OUT_PHOTO`].
pub const LEFT_OUT_FILE: u64 = 3;
/// See [`LEFT_OUT_PHOTO`].
pub const LEFT_OUT_GONE: u64 = 4;

/// What a group's copy without words carries (1388): its text is then the
/// home's English sentence naming the kind and the writer, `"Photo from
/// Anna"`, where a copy with words says `"Anna: text"`. A stock client
/// reads the sentence; a reader that understands groups knows it by the
/// copy's `0x02` and the writer's name ([`StandIn::read`]) and draws the
/// bubble without it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StandIn {
    /// `FIELD_IMAGE`.
    Photo,
    /// `FIELD_AUDIO`.
    Voice,
    /// `FIELD_FILE_ATTACHMENTS`.
    File,
    /// `FIELD_TELEMETRY`, a position.
    Position,
}

impl StandIn {
    /// Every kind, in the order [`StandIn::of`] picks one.
    pub const ALL: [StandIn; 4] = [
        StandIn::Photo,
        StandIn::Voice,
        StandIn::File,
        StandIn::Position,
    ];

    /// The sentence's words before the name, as they go on the wire.
    pub fn lead(self) -> &'static str {
        match self {
            StandIn::Photo => "Photo from ",
            StandIn::Voice => "Voice message from ",
            StandIn::File => "File from ",
            StandIn::Position => "Position from ",
        }
    }

    /// The field that makes a message this kind.
    pub fn field(self) -> i64 {
        match self {
            StandIn::Photo => FIELD_IMAGE,
            StandIn::Voice => FIELD_AUDIO,
            StandIn::File => FIELD_FILE_ATTACHMENTS,
            StandIn::Position => FIELD_TELEMETRY,
        }
    }

    /// The kind a message of `fields` stands in as: a photo before a voice
    /// message before a file before a position, as a bubble draws the
    /// first. `None` for a message that carries none of them.
    pub fn of(fields: &[Field]) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| fields.iter().any(|(key, _)| *key == kind.field()))
    }

    /// The sentence for `name`'s message: `"Photo from Anna"`.
    pub fn wire(self, name: &str) -> String {
        let mut out = String::from(self.lead());
        out.push_str(name);
        out
    }

    /// Whether `body` of a copy with `fields` is the sentence for its kind
    /// naming `name`: what a reader that knows the writer's name checks.
    pub fn read(body: &str, fields: &[Field], name: &str) -> bool {
        Self::of(fields).is_some_and(|kind| {
            body.strip_prefix(kind.lead())
                .is_some_and(|rest| rest == name)
        })
    }

    /// The name in `body` when it is the sentence for the kind of `fields`:
    /// the reader's way when the membership names no one for the writer.
    pub fn name_in<'a>(body: &'a str, fields: &[Field]) -> Option<&'a str> {
        body.strip_prefix(Self::of(fields)?.lead())
    }
}

/// Capability bit 0: the sender sends read receipts and wants them.
pub const CAP_READ_RECEIPTS: u64 = 1;

/// Capability bit 1: the sender understands groups.
pub const CAP_GROUPS: u64 = 2;

/// Capability bit 2: the sender acknowledges propagated post and wants its
/// own acknowledged (1393).
pub const CAP_PROPAGATION_RECEIPTS: u64 = 4;

/// A member's flag in [`Members`]: it has left, or was removed.
pub const MEMBER_LEFT: u64 = 1;

/// A member's flag in [`Members`]: it is the group's admin.
pub const MEMBER_ADMIN: u64 = 2;

/// A member's flag in [`Members`]: it joined, by the invitation's Join or
/// by writing to the group (1307 pass two); an invited member without it
/// has not answered yet.
pub const MEMBER_JOINED: u64 = 4;

/// A membership's standing (1307 pass four): the admin deleted the group.
/// Nobody is in it any more and the home refuses every later message.
pub const GROUP_ENDED: u64 = 1;

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

/// "These messages of yours are on my device" (the type keeps the name it
/// was built under, for post that came through the propagation network):
/// the recipient's clock when the first of them arrived, unix seconds, and
/// their full LXMF message ids. The shape of a
/// [`Receipt`], under its own key, so a reader never takes arrival for
/// reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropagationReceipt {
    pub received_at: u32,
    pub ids: Vec<[u8; 32]>,
}

impl PropagationReceipt {
    /// The receipts for `ids`, at most [`RECEIPT_IDS_MAX`] each, in order.
    pub fn batches(received_at: u32, ids: &[[u8; 32]]) -> Vec<Self> {
        Receipt::batches(received_at, ids)
            .into_iter()
            .map(|receipt| Self {
                received_at: receipt.read_at,
                ids: receipt.ids,
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
    /// The group is gone ([`GROUP_ENDED`]): its admin deleted it.
    pub ended: bool,
}

/// One member's rungs of one copy in a group's status (1466): when the
/// home booked its proof, its device's word and its read receipt, unix
/// seconds, 0 for not yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberRung {
    pub member: [u8; 16],
    pub proven_at: u32,
    pub on_device_at: u32,
    pub read_at: u32,
}

/// What a group's home tells a writer of one message (study 2.3): its
/// copies' proofs and receipts, counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupStatus {
    /// The LXMF id of the writer's own message.
    pub original: [u8; 32],
    /// Copies proven by their member.
    pub delivered: u64,
    /// Copies a member said it read.
    pub read: u64,
    /// Copies made: every other member present when it came.
    pub members: u64,
    /// Of those, the members that send read receipts.
    pub readers: u64,
    /// Copies whose member's device said it has them (1466); `None` from
    /// a home before 1466, which never says it.
    pub on_device: Option<u64>,
    /// Each counted member's rungs (1466); empty from a home before it.
    pub rungs: Vec<MemberRung>,
}

impl GroupStatus {
    /// Every copy is proven: the writer's one tick, "Arrived" (1466; two
    /// before it). A member's mailbox proves with the member's identity.
    pub fn delivered_to_all(&self) -> bool {
        self.members > 0 && self.delivered >= self.members
    }

    /// Every copy is on its member's device: the writer's two ticks (1466).
    pub fn on_devices_of_all(&self) -> bool {
        self.members > 0 && self.on_device.is_some_and(|n| n >= self.members)
    }

    /// Every member that sends receipts read it: the writer's read mark.
    pub fn read_by_all(&self) -> bool {
        self.readers > 0 && self.read >= self.readers
    }
}

/// One line of a digest: who wrote it, when by their clock, and its text
/// without the home's `"Name: "` prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestLine {
    pub sender: [u8; 16],
    pub sent_ms: u64,
    pub text: alloc::string::String,
    /// What the line stands for when the home left its message out
    /// ([`LEFT_OUT_PHOTO`] and its siblings), written as a fourth element
    /// only then, so every other line keeps its three.
    pub left_out: Option<u64>,
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
    /// Key `0x03`.
    pub status: Option<GroupStatus>,
    /// Key `0x04`.
    pub invite: Option<Invite>,
    /// Key `0x05`.
    pub members: Option<Members>,
    /// Key `0x06`.
    pub digest: Option<Vec<DigestLine>>,
    /// Key `0x07`.
    pub propagation_receipt: Option<PropagationReceipt>,
    /// Key `0x08`: the address that wrote the map (1482).
    pub writer: Option<[u8; 16]>,
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

    /// Whether the sender said it acknowledges propagated post and wants
    /// its own acknowledged.
    pub fn supports_propagation_receipts(&self) -> bool {
        self.capabilities
            .is_some_and(|caps| caps & CAP_PROPAGATION_RECEIPTS != 0)
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
            + usize::from(self.status.is_some())
            + usize::from(self.invite.is_some())
            + usize::from(self.members.is_some())
            + usize::from(self.digest.is_some())
            + usize::from(self.propagation_receipt.is_some())
            + usize::from(self.writer.is_some());
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
        if let Some(status) = &self.status {
            msgpack::int(&mut data, KEY_GROUP_STATUS);
            msgpack::array(&mut data, if status.on_device.is_some() { 7 } else { 5 });
            msgpack::bin(&mut data, &status.original);
            msgpack::uint(&mut data, status.delivered);
            msgpack::uint(&mut data, status.read);
            msgpack::uint(&mut data, status.members);
            msgpack::uint(&mut data, status.readers);
            if let Some(on_device) = status.on_device {
                msgpack::uint(&mut data, on_device);
                msgpack::array(&mut data, status.rungs.len());
                for rung in &status.rungs {
                    msgpack::array(&mut data, 4);
                    msgpack::bin(&mut data, &rung.member);
                    msgpack::uint(&mut data, u64::from(rung.proven_at));
                    msgpack::uint(&mut data, u64::from(rung.on_device_at));
                    msgpack::uint(&mut data, u64::from(rung.read_at));
                }
            }
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
            msgpack::array(&mut data, if members.ended { 3 } else { 2 });
            msgpack::uint(&mut data, members.epoch);
            msgpack::array(&mut data, members.members.len());
            for member in &members.members {
                msgpack::array(&mut data, 3);
                msgpack::bin(&mut data, &member.address);
                msgpack::string(&mut data, &member.name);
                msgpack::uint(&mut data, member.flags);
            }
            if members.ended {
                msgpack::uint(&mut data, GROUP_ENDED);
            }
        }
        if let Some(digest) = &self.digest {
            msgpack::int(&mut data, KEY_GROUP_DIGEST);
            msgpack::array(&mut data, digest.len());
            for line in digest {
                msgpack::array(&mut data, 3 + usize::from(line.left_out.is_some()));
                msgpack::bin(&mut data, &line.sender);
                msgpack::uint(&mut data, line.sent_ms);
                msgpack::string(&mut data, &line.text);
                if let Some(left_out) = line.left_out {
                    msgpack::uint(&mut data, left_out);
                }
            }
        }
        if let Some(receipt) = &self.propagation_receipt {
            msgpack::int(&mut data, KEY_PROPAGATION_RECEIPT);
            msgpack::array(&mut data, 2);
            msgpack::uint(&mut data, u64::from(receipt.received_at));
            msgpack::array(&mut data, receipt.ids.len());
            for id in &receipt.ids {
                msgpack::bin(&mut data, id);
            }
        }
        if let Some(writer) = &self.writer {
            msgpack::int(&mut data, KEY_WRITER);
            msgpack::bin(&mut data, writer);
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
    /// dict does, and a receipt (`0x01` or `0x07`) that is not `[uint32,
    /// [bin32, ...]]` is none; an id of another length in it is passed over. A group key of
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
                Ok(KEY_GROUP_STATUS) => lev.status = status(value),
                Ok(KEY_GROUP_INVITE) => lev.invite = invite(value),
                Ok(KEY_GROUP_MEMBERS) => lev.members = members(value),
                Ok(KEY_GROUP_DIGEST) => lev.digest = digest(value),
                Ok(KEY_PROPAGATION_RECEIPT) => {
                    lev.propagation_receipt = receipt(value).map(|r| PropagationReceipt {
                        received_at: r.read_at,
                        ids: r.ids,
                    });
                }
                Ok(KEY_WRITER) => {
                    lev.writer = bytes(value).and_then(|b| <[u8; 16]>::try_from(b).ok());
                }
                _ => {}
            }
        }
        Some(lev)
    }

    /// The namespace of a message `source` sent, as `source`'s own word
    /// (1482): [`Self::from_fields`], and none when the map names another
    /// writer, as a relay's copy of a member's message does. A map that
    /// names no writer is the source's, as every map before 1482 was.
    pub fn from_fields_of(fields: &[Field], source: &[u8; 16]) -> Option<Self> {
        Self::from_fields(fields).filter(|lev| lev.said_by(source))
    }

    /// Whether this map is `source`'s own word: it names no writer, or
    /// names `source`.
    pub fn said_by(&self, source: &[u8; 16]) -> bool {
        self.writer.is_none_or(|writer| writer == *source)
    }

    /// Who measured the `FIELD_TELEMETRY` reading of a message `source`
    /// signed (1687, the second audit of 1757): the writer a group copy
    /// names, `group_copy.sender`, when the map is `source`'s own word
    /// ([`Self::from_fields_of`]) and `lists(copy)` holds, which the
    /// reader answers from its own knowledge: `source` is a group it has
    /// joined, and that group's membership at the copy's epoch lists the
    /// writer as a member who has not left. `source` in every other case:
    /// any sender can name any writer, so a name nobody vouches for is the
    /// sender's own reading. The one rule every reader of a copy's
    /// position goes through (Loquax, the node helper).
    pub fn reading_writer(
        fields: &[Field],
        source: &[u8; 16],
        lists: impl FnOnce(&GroupCopy) -> bool,
    ) -> [u8; 16] {
        match Self::from_fields_of(fields, source).and_then(|lev| lev.group_copy) {
            Some(copy) if lists(&copy) => copy.sender,
            _ => *source,
        }
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

fn status(d: &[u8]) -> Option<GroupStatus> {
    let mut p = 0;
    let len = msgpack::array_len(d, &mut p).ok()?;
    if len < 5 {
        return None;
    }
    let mut status = GroupStatus {
        original: fixed(d, &mut p)?,
        delivered: msgpack::read_uint(d, &mut p).ok()?,
        read: msgpack::read_uint(d, &mut p).ok()?,
        members: msgpack::read_uint(d, &mut p).ok()?,
        readers: msgpack::read_uint(d, &mut p).ok()?,
        on_device: None,
        rungs: Vec::new(),
    };
    // 1466: the device count and the rungs, both or neither; a damaged
    // tail leaves the five counts as an older home's.
    if len >= 7 {
        if let Some((on_device, rungs)) = status_tail(d, &mut p) {
            status.on_device = Some(on_device);
            status.rungs = rungs;
        }
    }
    Some(status)
}

fn status_tail(d: &[u8], p: &mut usize) -> Option<(u64, Vec<MemberRung>)> {
    let on_device = msgpack::read_uint(d, p).ok()?;
    let n = msgpack::array_len(d, p).ok()?;
    let mut rungs = Vec::new();
    for _ in 0..n {
        if msgpack::array_len(d, p).ok()? < 4 {
            return None;
        }
        let at =
            |p: &mut usize| -> Option<u32> { u32::try_from(msgpack::read_uint(d, p).ok()?).ok() };
        rungs.push(MemberRung {
            member: fixed(d, p)?,
            proven_at: at(p)?,
            on_device_at: at(p)?,
            read_at: at(p)?,
        });
    }
    Some((on_device, rungs))
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
    let len = msgpack::array_len(d, &mut p).ok()?;
    if len < 2 {
        return None;
    }
    let epoch = msgpack::read_uint(d, &mut p).ok()?;
    let count = msgpack::array_len(d, &mut p).ok()?;
    let mut members = Vec::new();
    for _ in 0..count {
        let raw = msgpack::raw(d, &mut p).ok()?;
        members.push(member(raw)?);
    }
    // The standing, when there is one; a third element that is no uint is
    // a later version's and says nothing of it.
    let ended =
        len > 2 && msgpack::read_uint(d, &mut p).is_ok_and(|standing| standing & GROUP_ENDED != 0);
    Some(Members {
        epoch,
        members,
        ended,
    })
}

fn digest(d: &[u8]) -> Option<Vec<DigestLine>> {
    let mut p = 0;
    let count = msgpack::array_len(d, &mut p).ok()?;
    let mut lines = Vec::new();
    for _ in 0..count {
        let raw = msgpack::raw(d, &mut p).ok()?;
        let mut q = 0;
        let len = msgpack::array_len(raw, &mut q).ok()?;
        if len < 3 {
            return None;
        }
        let sender = fixed(raw, &mut q)?;
        let sent_ms = msgpack::read_uint(raw, &mut q).ok()?;
        let text = text(raw, &mut q)?;
        // A fourth element of another type says nothing; the text stands.
        let left_out = match len {
            3 => None,
            _ => msgpack::read_uint(raw, &mut q).ok(),
        };
        lines.push(DigestLine {
            sender,
            sent_ms,
            text,
            left_out,
        });
    }
    Some(lines)
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

    /// 1482: the writer is `08 c4 10` and its sixteen bytes, after every
    /// other key; a map naming another writer is no word of the source's,
    /// one naming none (before 1482) is.
    #[test]
    fn the_writer_key_names_who_wrote_the_map() {
        let anna = [0xa1; 16];
        let relay = [0xb0; 16];
        let lev = Lev {
            writer: Some(anna),
            ..Lev::mark()
        };
        let [_, (_, data)] = lev.fields();
        assert_eq!(hex(&data), alloc::format!("82000108c410{}", hex(&anna)));
        let fields = lev.fields();
        assert_eq!(Lev::from_fields(&fields), Some(lev.clone()));
        assert_eq!(Lev::from_fields_of(&fields, &anna), Some(lev));
        assert_eq!(Lev::from_fields_of(&fields, &relay), None, "passed through");
        let before = Lev::mark().fields();
        assert_eq!(Lev::from_fields_of(&before, &relay), Some(Lev::mark()));
        // Of another length it names nobody, and the map stays the source's.
        let mut odd = Vec::new();
        msgpack::map(&mut odd, 2);
        msgpack::int(&mut odd, KEY_CAPABILITIES);
        msgpack::uint(&mut odd, 1);
        msgpack::int(&mut odd, KEY_WRITER);
        msgpack::bin(&mut odd, &[0xa1; 8]);
        let odd = alloc::vec![Lev::mark().fields()[0].clone(), (FIELD_CUSTOM_DATA, odd)];
        assert_eq!(Lev::from_fields_of(&odd, &relay), Some(Lev::mark()));
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
                ended: false,
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

    /// 1307 G2: a digest is the study's bytes (5.3, `0x06`): a list of
    /// `[bin16 sender, uint64 ms, str text]`, read back as written; a line
    /// whose sender is not 16 bytes makes the whole digest none (a control:
    /// a digest is never shown with a line missing).
    #[test]
    fn a_digest_is_the_studys_bytes_and_a_bad_line_spoils_it() {
        let lev = Lev {
            capabilities: Some(CAP_GROUPS),
            digest: Some(alloc::vec![
                DigestLine {
                    sender: [0x11; 16],
                    sent_ms: 1_700_000_000_123,
                    text: "Brot".into(),
                    left_out: None,
                },
                DigestLine {
                    sender: [0x55; 16],
                    sent_ms: 7,
                    text: "Danke".into(),
                    left_out: None,
                },
            ]),
            ..Lev::default()
        };
        let [_, (_, data)] = lev.fields();
        let h = hex(&data);
        // map 2; 0: 2; 6: [[bin16, uint64, "Brot"], [bin16, 7, "Danke"]].
        assert!(h.starts_with("8200020692"), "{h}");
        assert!(
            h.contains(&("93c410".to_owned() + &"11".repeat(16) + "cf0000018bcfe5687ba442726f74")),
            "{h}"
        );
        assert!(
            h.ends_with(&("93c410".to_owned() + &"55".repeat(16) + "07a544616e6b65")),
            "{h}"
        );
        assert_eq!(Lev::from_fields(&lev.fields()), Some(lev));

        let mut kind = Vec::new();
        msgpack::bin(&mut kind, LEV_TYPE);
        let mut data = Vec::new();
        msgpack::map(&mut data, 1);
        msgpack::int(&mut data, KEY_GROUP_DIGEST);
        msgpack::array(&mut data, 2);
        msgpack::array(&mut data, 3);
        msgpack::bin(&mut data, &[1; 16]);
        msgpack::uint(&mut data, 1);
        msgpack::string(&mut data, "fine");
        msgpack::array(&mut data, 3);
        msgpack::bin(&mut data, &[1; 8]);
        msgpack::uint(&mut data, 2);
        msgpack::string(&mut data, "short sender");
        let read = Lev::from_fields(&[(FIELD_CUSTOM_TYPE, kind), (FIELD_CUSTOM_DATA, data)])
            .expect("ours");
        assert_eq!(read.digest, None);
    }

    /// 1348: a line the home left out carries what it stands for as a
    /// fourth element, and only that line; a reader before 1348 reads the
    /// first three (it takes a longer line, see above) and keeps the text.
    #[test]
    fn a_left_out_line_says_what_it_stands_for_in_a_fourth_element() {
        let line = |left_out| DigestLine {
            sender: [0x11; 16],
            sent_ms: 7,
            text: "sent a photo (too large for the radio)".into(),
            left_out,
        };
        let lev = Lev {
            digest: Some(alloc::vec![line(Some(LEFT_OUT_PHOTO)), line(None)]),
            ..Lev::default()
        };
        let [_, (_, data)] = lev.fields();
        let h = hex(&data);
        // [[bin16, 7, str, 1], [bin16, 7, str]].
        let said = hex(b"sent a photo (too large for the radio)");
        let head = "c410".to_owned() + &"11".repeat(16) + "07" + "d926" + &said;
        assert!(
            h.contains(&("0692".to_owned() + "94" + &head + "01")),
            "{h}"
        );
        assert!(
            h.ends_with(&("93".to_owned() + &head)),
            "the second line keeps three: {h}"
        );
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
                ended: false,
            })
        );
    }

    /// 1307 pass four: the status is the study's `0x03` with `readers`
    /// appended, and reads back as written; a status of four elements (the
    /// study's, without `readers`) is none, a control that the reader does
    /// not guess a count.
    #[test]
    fn a_status_is_five_counts_and_four_are_none() {
        let status = GroupStatus {
            original: [0x77; 32],
            delivered: 3,
            read: 1,
            members: 3,
            readers: 2,
            on_device: None,
            rungs: Vec::new(),
        };
        let lev = Lev {
            capabilities: Some(CAP_GROUPS),
            status: Some(status.clone()),
            ..Lev::default()
        };
        let [_, (_, data)] = lev.fields();
        assert_eq!(
            hex(&data),
            "8200020395c420".to_owned() + &"77".repeat(32) + "03010302"
        );
        assert_eq!(Lev::from_fields(&lev.fields()), Some(lev));
        assert!(status.delivered_to_all() && !status.read_by_all());
        assert!(!status.on_devices_of_all(), "five counts never say device");
        let none = GroupStatus {
            readers: 0,
            read: 0,
            ..status
        };
        assert!(!none.read_by_all(), "nobody sends receipts: never read");

        let mut kind = Vec::new();
        msgpack::bin(&mut kind, LEV_TYPE);
        let mut data = Vec::new();
        msgpack::map(&mut data, 1);
        msgpack::int(&mut data, KEY_GROUP_STATUS);
        msgpack::array(&mut data, 4);
        msgpack::bin(&mut data, &[0x77; 32]);
        msgpack::uint(&mut data, 3);
        msgpack::uint(&mut data, 1);
        msgpack::uint(&mut data, 3);
        let read = Lev::from_fields(&[(FIELD_CUSTOM_TYPE, kind), (FIELD_CUSTOM_DATA, data)])
            .expect("ours");
        assert_eq!(read.status, None);
    }

    /// 1466: the status with the device count and one member's rungs, by
    /// hand: `97` an array of seven, the five counts, `02` on_device, `91`
    /// one rung, `94` its four elements, `c4 10` and the member's 16
    /// bytes, `ce ..` proven_at, `ce ..` on_device_at, `00` read_at not
    /// yet. It reads back as written; a reader that knows only five reads
    /// the five (the first five elements are the bytes of before 1466);
    /// two ticks only once every copy is on a device.
    #[test]
    fn a_status_since_1466_carries_the_device_count_and_each_members_rungs() {
        let status = GroupStatus {
            original: [0x77; 32],
            delivered: 3,
            read: 0,
            members: 3,
            readers: 2,
            on_device: Some(2),
            rungs: alloc::vec![MemberRung {
                member: [0x11; 16],
                proven_at: 1_700_000_100,
                on_device_at: 1_700_000_160,
                read_at: 0,
            }],
        };
        let lev = Lev {
            capabilities: Some(CAP_GROUPS),
            status: Some(status.clone()),
            ..Lev::default()
        };
        let [_, (_, data)] = lev.fields();
        assert_eq!(
            hex(&data),
            "8200020397c420".to_owned()
                + &"77".repeat(32)
                + "0300030202"
                + "9194c410"
                + &"11".repeat(16)
                + "ce6553f164ce6553f1a000"
        );
        assert_eq!(Lev::from_fields(&lev.fields()), Some(lev));
        assert!(status.delivered_to_all());
        assert!(!status.on_devices_of_all(), "two of three on a device");
        let all = GroupStatus {
            on_device: Some(3),
            ..status.clone()
        };
        assert!(all.on_devices_of_all());
        // An empty group is never on every device.
        let empty = GroupStatus {
            members: 0,
            on_device: Some(0),
            ..status
        };
        assert!(!empty.on_devices_of_all());
    }

    /// 1393: a propagation receipt by hand. `82` a map of two, `00 05` the
    /// capabilities (bit 0 read receipts, bit 2 propagation receipts, still
    /// one fixint, so asking costs nothing), `07` the key, `92` an array of
    /// two, `ce 65 53 f1 64` received_at 1700000100 as uint32, `91` one id,
    /// `c4 20` and its 32 bytes.
    #[test]
    fn a_propagation_receipt_is_the_hand_built_bytes() {
        let lev = Lev {
            capabilities: Some(CAP_READ_RECEIPTS | CAP_PROPAGATION_RECEIPTS),
            propagation_receipt: Some(PropagationReceipt {
                received_at: 1_700_000_100,
                ids: alloc::vec![[0xab; 32]],
            }),
            ..Lev::default()
        };
        let [(_, kind), (_, data)] = lev.fields();
        assert_eq!(
            hex(&data),
            "8200050792ce6553f16491c420".to_owned() + &"ab".repeat(32)
        );
        // Alone, without the mark: the pair with its keys `cc fb`, `cc fc`
        // is 18 bytes, and each id 34 more.
        let alone = Lev {
            propagation_receipt: lev.propagation_receipt.clone(),
            ..Lev::default()
        };
        let [(_, k), (_, d)] = alone.fields();
        assert_eq!(2 + k.len() + 2 + d.len(), 18 + 34);
        assert_eq!(kind, k);
        let read = Lev::from_fields(&lev.fields()).expect("lev");
        assert_eq!(read, lev);
        assert!(read.supports_propagation_receipts() && read.supports_receipts());
        assert_eq!(read.receipt, None, "arrival is never taken for reading");
        assert!(!Lev::mark().supports_propagation_receipts());
        let batches = PropagationReceipt::batches(9, &[[1; 32]; 9]);
        assert_eq!(
            batches.iter().map(|r| r.ids.len()).collect::<Vec<_>>(),
            [7, 2]
        );
    }

    /// 1393: a decoder from before `0x07` takes the key for one it does not
    /// know. The same map with the key renamed to one no decoder knows
    /// (`0x7e`), put first, is what such a decoder sees: the capabilities
    /// and the read receipt behind it still parse. Today's decoder reads the
    /// original; a `0x07` of another shape is none and spoils nothing.
    #[test]
    fn a_decoder_without_0x07_skips_it_and_reads_the_rest() {
        let map = |key: i64, shape_ok: bool| {
            let mut data = Vec::new();
            msgpack::map(&mut data, 3);
            msgpack::int(&mut data, key);
            msgpack::array(&mut data, 2);
            msgpack::uint(&mut data, 1_700_000_100);
            if shape_ok {
                msgpack::array(&mut data, 2);
                msgpack::bin(&mut data, &[0xab; 32]);
                msgpack::bin(&mut data, &[0xcd; 32]);
            } else {
                msgpack::string(&mut data, "not a list");
            }
            msgpack::int(&mut data, KEY_CAPABILITIES);
            msgpack::uint(&mut data, CAP_READ_RECEIPTS | CAP_PROPAGATION_RECEIPTS);
            msgpack::int(&mut data, KEY_RECEIPT);
            msgpack::array(&mut data, 2);
            msgpack::uint(&mut data, 7);
            msgpack::array(&mut data, 1);
            msgpack::bin(&mut data, &[0xef; 32]);
            let mut kind = Vec::new();
            msgpack::bin(&mut kind, LEV_TYPE);
            Lev::from_fields(&[(FIELD_CUSTOM_TYPE, kind), (FIELD_CUSTOM_DATA, data)]).expect("lev")
        };
        let read_receipt = Some(Receipt {
            read_at: 7,
            ids: alloc::vec![[0xef; 32]],
        });

        let older = map(0x7e, true);
        assert_eq!(older.propagation_receipt, None);
        assert_eq!(older.receipt, read_receipt);
        assert_eq!(
            older.capabilities,
            Some(CAP_READ_RECEIPTS | CAP_PROPAGATION_RECEIPTS)
        );

        let today = map(KEY_PROPAGATION_RECEIPT, true);
        assert_eq!(
            today.propagation_receipt,
            Some(PropagationReceipt {
                received_at: 1_700_000_100,
                ids: alloc::vec![[0xab; 32], [0xcd; 32]],
            })
        );
        assert_eq!(today.receipt, read_receipt);

        let bad = map(KEY_PROPAGATION_RECEIPT, false);
        assert_eq!(bad.propagation_receipt, None);
        assert_eq!(bad.receipt, read_receipt);
    }

    /// 1307 pass four: an ended group's membership carries the standing as
    /// a third element; a membership that is not ended has none, so every
    /// earlier one keeps its bytes (control).
    #[test]
    fn an_ended_membership_appends_its_standing_and_a_live_one_does_not() {
        let members = |ended| Members {
            epoch: 4,
            members: alloc::vec![Member {
                address: [0x44; 16],
                name: "Lew".into(),
                flags: MEMBER_ADMIN | MEMBER_LEFT,
            }],
            ended,
        };
        let data = |ended| {
            let [_, (_, data)] = Lev {
                members: Some(members(ended)),
                ..Lev::default()
            }
            .fields();
            hex(&data)
        };
        assert!(data(true).starts_with("8105930491"), "{}", data(true));
        assert!(data(true).ends_with("0301"), "{}", data(true));
        assert!(data(false).starts_with("8105920491"), "{}", data(false));
        for ended in [true, false] {
            let lev = Lev {
                members: Some(members(ended)),
                ..Lev::default()
            };
            assert_eq!(Lev::from_fields(&lev.fields()), Some(lev));
        }
    }

    /// 1388: a copy without words is the kind's sentence naming the
    /// writer, a reader knows it by the name it holds for them, and a
    /// sentence of another kind, or naming someone else, is text.
    #[test]
    fn a_stand_in_names_the_kind_and_the_writer() {
        let photo: [Field; 1] = [(FIELD_IMAGE, alloc::vec![0x90])];
        let voice_and_file: [Field; 2] = [
            (FIELD_FILE_ATTACHMENTS, alloc::vec![0x90]),
            (FIELD_AUDIO, alloc::vec![0x90]),
        ];
        let position: [Field; 1] = [(FIELD_TELEMETRY, alloc::vec![0x80])];
        assert_eq!(StandIn::of(&photo), Some(StandIn::Photo));
        assert_eq!(StandIn::of(&voice_and_file), Some(StandIn::Voice));
        assert_eq!(StandIn::of(&position), Some(StandIn::Position));
        assert_eq!(StandIn::of(&[]), None);
        let wire: alloc::vec::Vec<_> = StandIn::ALL.iter().map(|k| k.wire("lew")).collect();
        assert_eq!(
            wire,
            [
                "Photo from lew",
                "Voice message from lew",
                "File from lew",
                "Position from lew"
            ]
        );
        assert!(StandIn::read("Photo from a:b", &photo, "a:b"));
        assert!(!StandIn::read("Photo from lew", &photo, "anna"));
        assert!(!StandIn::read("File from lew", &photo, "lew"));
        assert!(!StandIn::read("Photo from lew", &[], "lew"));
        assert_eq!(StandIn::name_in("Photo from lew", &photo), Some("lew"));
        assert_eq!(StandIn::name_in("lew: Photo from lew", &photo), None);
    }
}
