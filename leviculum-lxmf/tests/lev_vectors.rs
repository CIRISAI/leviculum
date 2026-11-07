//! The `lev` namespace against the reference (`VEC-MSG-LEV-MARK`,
//! `VEC-MSG-LEV-RECEIPT` and the group keys `VEC-MSG-LEV-GROUP-*` in
//! `docs/src/appendix/lxmf/vectors/vectors.json`, written by
//! `gen_vectors.py` importing `reference/LXMF`).
//!
//! Both directions, as for reactions and replies: the message the reference
//! built is read by our codec with the mark and the receipt it holds, and
//! the message built from our fields is the reference's byte for byte. What
//! the reference's `LXMessage.unpack_from_bytes` read back out of those
//! bytes, packed again, is the field we wrote: the pair survives a Python
//! client untouched.

mod common;

use leviculum_core::Identity;
use leviculum_lxmf::constants::{FIELD_CUSTOM_DATA, FIELD_CUSTOM_TYPE};
use leviculum_lxmf::lev::{
    DigestLine, GroupCopy, GroupStatus, Invite, Lev, Member, Members, Receipt, CAP_GROUPS,
    CAP_READ_RECEIPTS, MEMBER_ADMIN, MEMBER_JOINED, MEMBER_LEFT,
};
use leviculum_lxmf::{DeliveryMethod, Message, Verification};

const IDS: [&str; 9] = [
    "VEC-MSG-LEV-MARK",
    "VEC-MSG-LEV-RECEIPT",
    "VEC-MSG-LEV-GROUP-COPY",
    "VEC-MSG-LEV-GROUP-INVITE",
    "VEC-MSG-LEV-GROUP-MEMBERS",
    "VEC-MSG-LEV-GROUP-LEAVE",
    "VEC-MSG-LEV-GROUP-DIGEST",
    "VEC-MSG-LEV-GROUP-STATUS",
    "VEC-MSG-LEV-GROUP-ENDED",
];

const SENDER: [u8; 16] = [0x11; 16];
const MEMBER: [u8; 16] = [0x55; 16];

/// What a Loquax that understands groups says: bits 0 and 1.
fn groups(lev: Lev) -> Lev {
    Lev {
        capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
        ..lev
    }
}

fn hex(id: &str, field: &str) -> Vec<u8> {
    hex::decode(common::fixture(id, field)).unwrap()
}

fn expected(id: &str) -> Lev {
    match id {
        "VEC-MSG-LEV-RECEIPT" => Lev::mark_with(Receipt {
            read_at: common::fixture(id, "recovered_read_at").parse().unwrap(),
            ids: vec![hex("VEC-MSG-1", "message_id_hex").try_into().unwrap()],
        }),
        "VEC-MSG-LEV-GROUP-COPY" => groups(Lev {
            group_copy: Some(GroupCopy {
                sender: SENDER,
                sent_ms: 1_700_000_000_123,
                epoch: 1,
            }),
            ..Lev::default()
        }),
        "VEC-MSG-LEV-GROUP-INVITE" => groups(Lev {
            invite: Some(Invite {
                group: [0x22; 16],
                public_key: std::array::from_fn(|i| 0x80 + i as u8),
                name: "Familie".into(),
                admin_name: "Lew".into(),
                epoch: 1,
            }),
            ..Lev::default()
        }),
        "VEC-MSG-LEV-GROUP-MEMBERS" => groups(Lev {
            members: Some(Members {
                epoch: 1,
                members: vec![
                    Member {
                        address: SENDER,
                        name: "Lew".into(),
                        flags: MEMBER_ADMIN,
                    },
                    Member {
                        address: MEMBER,
                        name: "Anna".into(),
                        flags: 0,
                    },
                ],
                ended: false,
            }),
            ..Lev::default()
        }),
        "VEC-MSG-LEV-GROUP-LEAVE" => groups(Lev {
            members: Some(Members {
                epoch: 1,
                members: vec![Member {
                    address: MEMBER,
                    name: "Anna".into(),
                    flags: MEMBER_LEFT,
                }],
                ended: false,
            }),
            ..Lev::default()
        }),
        "VEC-MSG-LEV-GROUP-DIGEST" => groups(Lev {
            digest: Some(vec![
                DigestLine {
                    sender: SENDER,
                    sent_ms: 1_700_000_000_123,
                    text: "Brot ist da".into(),
                    left_out: None,
                },
                DigestLine {
                    sender: MEMBER,
                    sent_ms: 1_700_000_001_123,
                    text: "Danke".into(),
                    left_out: None,
                },
            ]),
            ..Lev::default()
        }),
        "VEC-MSG-LEV-GROUP-STATUS" => groups(Lev {
            status: Some(GroupStatus {
                original: [0x77; 32],
                delivered: 2,
                read: 1,
                members: 2,
                readers: 1,
                on_device: None,
                rungs: Vec::new(),
            }),
            ..Lev::default()
        }),
        "VEC-MSG-LEV-GROUP-ENDED" => groups(Lev {
            members: Some(Members {
                epoch: 2,
                members: vec![
                    Member {
                        address: SENDER,
                        name: "Lew".into(),
                        flags: MEMBER_ADMIN | MEMBER_LEFT,
                    },
                    Member {
                        address: MEMBER,
                        name: "Anna".into(),
                        flags: MEMBER_JOINED | MEMBER_LEFT,
                    },
                ],
                ended: true,
            }),
            ..Lev::default()
        }),
        _ => Lev::mark(),
    }
}

#[test]
fn the_references_mark_and_receipt_are_read() {
    for id in IDS {
        let packed = hex(id, "packed_hex");
        let identity = Identity::from_private_key_bytes(&hex(id, "src_identity_prv_hex")).unwrap();
        let message = Message::unpack(&packed, None, Some(&identity), DeliveryMethod::Direct)
            .unwrap_or_else(|e| panic!("{id}: {e}"));
        assert_eq!(message.verification, Verification::Valid, "{id}");
        assert_eq!(message.message_id.to_vec(), hex(id, "message_id_hex"));
        assert_eq!(message.content, hex(id, "content_hex"), "{id}");
        let lev = Lev::from_fields(&message.fields).unwrap_or_else(|| panic!("{id}: no lev"));
        assert_eq!(lev, expected(id), "{id}");
        assert!(lev.supports_receipts(), "{id}");
    }
}

#[test]
fn our_fields_are_the_references_bytes_and_survive_its_unpack() {
    let identity =
        Identity::from_private_key_bytes(&hex("VEC-MSG-LEV-MARK", "src_identity_prv_hex")).unwrap();
    for id in IDS {
        let packed = hex(id, "packed_hex");
        let fields = expected(id).fields();
        assert_eq!(fields[0].0, FIELD_CUSTOM_TYPE);
        assert_eq!(fields[0].1, hex(id, "field_fb_msgpack_hex"), "{id}: type");
        assert_eq!(fields[1].0, FIELD_CUSTOM_DATA);
        assert_eq!(fields[1].1, hex(id, "field_fc_msgpack_hex"), "{id}: data");
        // What the reference read out of the message, packed again.
        assert_eq!(fields[1].1, hex(id, "recovered_fc_msgpack_hex"), "{id}");
        assert_eq!(hex(id, "recovered_type_hex"), b"lev", "{id}");
        let caps = expected(id).capabilities.unwrap_or_default().to_string();
        assert_eq!(common::fixture(id, "recovered_capabilities"), caps, "{id}");
        let ours = Message::create(
            packed[0..16].try_into().unwrap(),
            packed[16..32].try_into().unwrap(),
            &identity,
            common::fixture(id, "timestamp").parse().unwrap(),
            Vec::new(),
            hex(id, "content_hex"),
            fields.to_vec(),
            DeliveryMethod::Opportunistic,
        )
        .unwrap();
        assert_eq!(ours.pack(), packed, "{id}: packed");
        assert_eq!(common::fixture(id, "signature_validated"), "true");
    }
}

/// What the reference read out of the group keys is what our codec wrote:
/// the writer, its clock and the epoch; the group, its key and names; each
/// member with its flags.
#[test]
fn the_reference_reads_the_group_keys_as_we_mean_them() {
    let id = "VEC-MSG-LEV-GROUP-COPY";
    assert_eq!(
        common::fixture(id, "recovered_sender_hex"),
        hex::encode(SENDER)
    );
    assert_eq!(common::fixture(id, "recovered_sent_ms"), "1700000000123");
    assert_eq!(common::fixture(id, "recovered_epoch"), "1");
    let id = "VEC-MSG-LEV-GROUP-INVITE";
    let invite = expected(id).invite.unwrap();
    assert_eq!(
        common::fixture(id, "recovered_group_hex"),
        hex::encode(invite.group)
    );
    assert_eq!(
        common::fixture(id, "recovered_group_key_hex"),
        hex::encode(invite.public_key)
    );
    assert_eq!(common::fixture(id, "recovered_group_name"), "Familie");
    assert_eq!(common::fixture(id, "recovered_admin_name"), "Lew");
    assert!(expected("VEC-MSG-LEV-GROUP-COPY").supports_groups());
    // The digest (1307 G2): each line's writer, clock and text.
    let all = include_str!("../../docs/src/appendix/lxmf/vectors/vectors.json");
    let at = all
        .find("\"id\": \"VEC-MSG-LEV-GROUP-DIGEST\"")
        .expect("the digest vector");
    let record = &all[at..];
    let start = record.find("\"recovered_digest\"").expect("its digest");
    let digest = &record[start..start + record[start..].find(']').expect("a line") + 300];
    for line in expected("VEC-MSG-LEV-GROUP-DIGEST").digest.unwrap() {
        assert!(digest.contains(&hex::encode(line.sender)), "{digest}");
        assert!(digest.contains(&line.sent_ms.to_string()), "{digest}");
        assert!(digest.contains(&line.text), "{digest}");
    }
}
