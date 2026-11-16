//! Every envelope shape periculum 1378 recorded between the reference hub
//! (rrcd 0.3.2) and its oracle client (`vectors/README.md`): decoded, each
//! key checked against the recorded decoded form, and re-encoded byte for
//! byte, both as a bare CBOR value and through the typed envelope.

use leviculum_rrc::{Envelope, Float, Kind, Value};
use serde_json::Value as Json;

fn vectors() -> Vec<Json> {
    let text = include_str!("vectors/envelopes.json");
    let file: Json = serde_json::from_str(text).expect("the vectors are JSON");
    assert_eq!(file["category"], "rrc_envelope");
    file["vectors"].as_array().expect("a vector list").clone()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

/// Whether the decoded CBOR is the recorded JSON form: integer keys as
/// strings, byte strings as `{"bytes": hex}`.
fn same(value: &Value, json: &Json) -> bool {
    match (value, json) {
        (Value::Uint(n), Json::Number(j)) => j.as_u64() == Some(*n),
        (Value::Float(f), Json::Number(j)) => j.as_f64() == Some(f.to_f64()),
        (Value::Text(s), Json::String(j)) => s == j,
        (Value::Bool(b), Json::Bool(j)) => b == j,
        (Value::Null, Json::Null) => true,
        (Value::Bytes(b), Json::Object(o)) if o.len() == 1 && o.contains_key("bytes") => {
            o["bytes"].as_str().map(unhex).as_deref() == Some(&b[..])
        }
        (Value::Array(items), Json::Array(j)) => {
            items.len() == j.len() && items.iter().zip(j).all(|(a, b)| same(a, b))
        }
        (Value::Map(pairs), Json::Object(o)) => {
            pairs.len() == o.len()
                && pairs.iter().all(|(k, v)| {
                    let key = match k {
                        Value::Uint(n) => n.to_string(),
                        Value::Text(s) => s.clone(),
                        _ => return false,
                    };
                    o.get(&key).is_some_and(|j| same(v, j))
                })
        }
        _ => false,
    }
}

#[test]
fn every_vector_decodes_and_re_encodes_byte_for_byte() {
    let vectors = vectors();
    assert_eq!(vectors.len(), 21, "the recorded shapes");
    for vector in &vectors {
        let name = vector["name"].as_str().expect("a name");
        let cbor = unhex(vector["cbor"].as_str().expect("cbor"));
        assert_eq!(
            cbor.len() as u64,
            vector["cbor_len"].as_u64().unwrap(),
            "{name}"
        );

        let value = Value::decode(&cbor).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(same(&value, &vector["decoded"]), "{name}: {value:?}");
        assert_eq!(value.encode(), cbor, "{name}: value round trip");

        let envelope = Envelope::decode(&cbor).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            envelope.kind.code(),
            vector["code"].as_u64().unwrap(),
            "{name}"
        );
        assert!(
            !matches!(envelope.kind, Kind::Other(_)),
            "{name}: a known type"
        );
        assert_eq!(envelope.encode(), cbor, "{name}: envelope round trip");

        // The keys the recording lists are the keys the envelope carries.
        let names = ["v", "t", "id", "ts", "src", "room", "body", "nick", "dst"];
        let present: Vec<&str> = names
            .iter()
            .enumerate()
            .filter(|(k, _)| value.get(*k as u64).is_some())
            .map(|(_, n)| *n)
            .collect();
        let listed: Vec<&str> = vector["keys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap())
            .collect();
        assert_eq!(present, listed, "{name}");
    }
}

#[test]
fn the_hubs_ping_is_a_float_and_a_pong_hands_it_back_unchanged() {
    let find = |name: &str| {
        let v = vectors().into_iter().find(|v| v["name"] == name).unwrap();
        unhex(v["cbor"].as_str().unwrap())
    };
    let ping = Envelope::decode(&find("ping_down")).unwrap();
    assert!(matches!(ping.body, Some(Value::Float(Float::F64(_)))));
    let pong = Envelope::decode(&find("pong_up")).unwrap();
    assert_eq!(pong.body, ping.body, "the recorded client's PONG");
}

#[test]
fn a_changed_byte_is_not_the_same_envelope() {
    // The control for the round trip: the comparison sees one byte.
    let v = &vectors()[0];
    let mut cbor = unhex(v["cbor"].as_str().unwrap());
    let last = cbor.len() - 1;
    cbor[last] ^= 0x01;
    let value = Value::decode(&cbor).unwrap();
    assert!(!same(&value, &v["decoded"]));
}
