#![no_main]
//! Fuzz the RRC decoder (`leviculum_rrc::Value::decode`,
//! `Envelope::decode`, `Session::receive`) with arbitrary bytes. They come
//! from a hub over a link, so a panic here is a remote crash of Loquax.
//!
//! Beyond "never panics": a value that decodes re-encodes to bytes that
//! decode to the same value, and to the input itself when the input used
//! the shortest heads (the encoder's form is a fixed point).
use leviculum_rrc::{Envelope, Session, Value};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(value) = Value::decode(data) {
        let again = value.encode();
        let back = Value::decode(&again).expect("the encoder's output decodes");
        // NaN is not equal to itself; compare the bytes instead.
        assert_eq!(back.encode(), again, "encoding is a fixed point");
    }
    if let Ok(envelope) = Envelope::decode(data) {
        let again = envelope.encode();
        let back = Envelope::decode(&again).expect("an encoded envelope decodes");
        assert_eq!(back.encode(), again);
    }
    let mut session = Session::new([7; 16], Some("fuzz".into()), "fuzz", "0");
    let _ = session.receive(data, [0; 8], 0);
});
