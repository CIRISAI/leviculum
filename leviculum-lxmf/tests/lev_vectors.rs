//! The `lev` namespace against the reference (`VEC-MSG-LEV-MARK`,
//! `VEC-MSG-LEV-RECEIPT` in `docs/src/appendix/lxmf/vectors/vectors.json`,
//! written by `gen_vectors.py` importing `reference/LXMF`).
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
use leviculum_lxmf::lev::{Lev, Receipt};
use leviculum_lxmf::{DeliveryMethod, Message, Verification};

const IDS: [&str; 2] = ["VEC-MSG-LEV-MARK", "VEC-MSG-LEV-RECEIPT"];

fn hex(id: &str, field: &str) -> Vec<u8> {
    hex::decode(common::fixture(id, field)).unwrap()
}

fn expected(id: &str) -> Lev {
    match id {
        "VEC-MSG-LEV-RECEIPT" => Lev::mark_with(Receipt {
            read_at: common::fixture(id, "recovered_read_at").parse().unwrap(),
            ids: vec![hex("VEC-MSG-1", "message_id_hex").try_into().unwrap()],
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
        assert_eq!(common::fixture(id, "recovered_capabilities"), "1", "{id}");
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
