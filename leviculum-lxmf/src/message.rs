use crate::msgpack;
use alloc::vec::Vec;
use leviculum_core::{crypto::full_hash, Identity};

/// An LXMF extension field. Values are retained as one complete MessagePack
/// value so unknown extensions round-trip byte-for-byte. Python accepts signed
/// integer keys, including negative fixints.
pub type Field = (i64, Vec<u8>);
type DecodedPayload = (f64, Vec<u8>, Vec<u8>, Vec<Field>, Option<Vec<u8>>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DeliveryMethod {
    Opportunistic = 1,
    Direct = 2,
    Propagated = 3,
    Paper = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    Unverified,
    Valid,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageError {
    TooShort,
    InvalidFormat,
    InvalidField,
    Identity,
    WrongDestination,
}
impl From<msgpack::Error> for MessageError {
    fn from(_: msgpack::Error) -> Self {
        Self::InvalidFormat
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub destination_hash: [u8; 16],
    pub source_hash: [u8; 16],
    pub signature: [u8; 64],
    pub timestamp: f64,
    pub title: Vec<u8>,
    pub content: Vec<u8>,
    pub fields: Vec<Field>,
    /// Delivery stamp. PoW stamps are 32 bytes; ticket-derived stamps are the
    /// 16-byte Reticulum truncated hash used by the Python implementation.
    pub stamp: Option<Vec<u8>>,
    pub message_id: [u8; 32],
    pub verification: Verification,
    pub method: DeliveryMethod,
}

impl Message {
    /// Construct and sign a complete LXMF message from its protocol fields.
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        destination_hash: [u8; 16],
        source_hash: [u8; 16],
        source: &Identity,
        timestamp: f64,
        title: Vec<u8>,
        content: Vec<u8>,
        fields: Vec<Field>,
        method: DeliveryMethod,
    ) -> Result<Self, MessageError> {
        for (_, v) in &fields {
            let mut p = 0;
            msgpack::skip(v, &mut p).map_err(|_| MessageError::InvalidField)?;
            if p != v.len() {
                return Err(MessageError::InvalidField);
            }
        }
        let payload = encode_payload(timestamp, &title, &content, &fields, None);
        let mut hashed = Vec::with_capacity(32 + payload.len());
        hashed.extend_from_slice(&destination_hash);
        hashed.extend_from_slice(&source_hash);
        hashed.extend_from_slice(&payload);
        let message_id = full_hash(&hashed);
        let mut signed = hashed;
        signed.extend_from_slice(&message_id);
        let signature = source.sign(&signed).map_err(|_| MessageError::Identity)?;
        Ok(Self {
            destination_hash,
            source_hash,
            signature,
            timestamp,
            title,
            content,
            fields,
            stamp: None,
            message_id,
            verification: Verification::Valid,
            method,
        })
    }
    pub fn set_stamp(&mut self, stamp: Vec<u8>) -> Result<(), MessageError> {
        if stamp.len() != 16 && stamp.len() != 32 {
            return Err(MessageError::InvalidFormat);
        }
        self.stamp = Some(stamp);
        Ok(())
    }
    pub fn pack(&self) -> Vec<u8> {
        let payload = encode_payload(
            self.timestamp,
            &self.title,
            &self.content,
            &self.fields,
            self.stamp.as_deref(),
        );
        let mut o = Vec::with_capacity(96 + payload.len());
        o.extend_from_slice(&self.destination_hash);
        o.extend_from_slice(&self.source_hash);
        o.extend_from_slice(&self.signature);
        o.extend_from_slice(&payload);
        o
    }
    pub fn on_air(&self) -> Result<Vec<u8>, MessageError> {
        let p = self.pack();
        match self.method {
            DeliveryMethod::Opportunistic => Ok(p[16..].to_vec()),
            DeliveryMethod::Direct => Ok(p),
            // These methods require destination encryption and dedicated
            // framing; returning plaintext here would be a confidentiality bug.
            DeliveryMethod::Propagated | DeliveryMethod::Paper => Err(MessageError::InvalidFormat),
        }
    }
    pub fn unpack(
        data: &[u8],
        inferred_destination: Option<[u8; 16]>,
        source: Option<&Identity>,
        method: DeliveryMethod,
    ) -> Result<Self, MessageError> {
        let owned;
        let d = if method == DeliveryMethod::Opportunistic {
            let dst = inferred_destination.ok_or(MessageError::WrongDestination)?;
            owned = {
                let mut x = Vec::with_capacity(data.len() + 16);
                x.extend_from_slice(&dst);
                x.extend_from_slice(data);
                x
            };
            owned.as_slice()
        } else {
            data
        };
        if d.len() < 97 {
            return Err(MessageError::TooShort);
        }
        let destination_hash: [u8; 16] = d[0..16].try_into().map_err(|_| MessageError::TooShort)?;
        let source_hash: [u8; 16] = d[16..32].try_into().map_err(|_| MessageError::TooShort)?;
        let signature: [u8; 64] = d[32..96].try_into().map_err(|_| MessageError::TooShort)?;
        let (timestamp, title, content, fields, stamp) = decode_payload(&d[96..])?;
        let clean = encode_payload(timestamp, &title, &content, &fields, None);
        let mut hashed = Vec::new();
        hashed.extend_from_slice(&destination_hash);
        hashed.extend_from_slice(&source_hash);
        hashed.extend_from_slice(&clean);
        let message_id = full_hash(&hashed);
        let mut signed = hashed;
        signed.extend_from_slice(&message_id);
        let verification = match source {
            None => Verification::Unverified,
            Some(i) => {
                if i.verify(&signed, &signature)
                    .map_err(|_| MessageError::Identity)?
                {
                    Verification::Valid
                } else {
                    Verification::Invalid
                }
            }
        };
        Ok(Self {
            destination_hash,
            source_hash,
            signature,
            timestamp,
            title,
            content,
            fields,
            stamp,
            message_id,
            verification,
            method,
        })
    }
}

fn encode_payload(
    ts: f64,
    title: &[u8],
    content: &[u8],
    fields: &[Field],
    stamp: Option<&[u8]>,
) -> Vec<u8> {
    let mut o = Vec::new();
    msgpack::array(&mut o, if stamp.is_some() { 5 } else { 4 });
    msgpack::f64(&mut o, ts);
    msgpack::bin(&mut o, title);
    msgpack::bin(&mut o, content);
    msgpack::map(&mut o, fields.len());
    for (k, v) in fields {
        msgpack::int(&mut o, *k);
        o.extend_from_slice(v)
    }
    if let Some(s) = stamp {
        msgpack::bin(&mut o, s)
    }
    o
}
fn decode_payload(d: &[u8]) -> Result<DecodedPayload, MessageError> {
    let mut p = 0;
    let n = msgpack::array_len(d, &mut p)?;
    if !(4..=5).contains(&n) {
        return Err(MessageError::InvalidFormat);
    }
    let ts = msgpack::read_f64(d, &mut p)?;
    let title = msgpack::read_bin(d, &mut p)?.to_vec();
    let content = msgpack::read_bin(d, &mut p)?.to_vec();
    let count = msgpack::map_len(d, &mut p)?;
    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        let k = msgpack::read_int(d, &mut p)?;
        fields.push((k, msgpack::raw(d, &mut p)?.to_vec()))
    }
    let stamp = if n == 5 {
        let s = msgpack::read_bin(d, &mut p)?;
        if s.len() != 16 && s.len() != 32 {
            return Err(MessageError::InvalidFormat);
        }
        Some(s.to_vec())
    } else {
        None
    };
    if p != d.len() {
        return Err(MessageError::InvalidFormat);
    }
    Ok((ts, title, content, fields, stamp))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opportunistic_message_round_trips() {
        let private: Vec<u8> = (0..64).collect();
        let id = Identity::from_private_key_bytes(&private).unwrap();
        let destination_hash = [1; 16];
        let m = Message::create(
            destination_hash,
            [2; 16],
            &id,
            1_700_000_000.0,
            b"Hi".to_vec(),
            b"Hello".to_vec(),
            Vec::new(),
            DeliveryMethod::Opportunistic,
        )
        .unwrap();
        let u = Message::unpack(
            &m.on_air().unwrap(),
            Some(destination_hash),
            Some(&id),
            DeliveryMethod::Opportunistic,
        )
        .unwrap();
        assert_eq!(u.verification, Verification::Valid);
    }
}
