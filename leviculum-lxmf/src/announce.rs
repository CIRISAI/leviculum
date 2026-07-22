//! LXMF delivery announce application data.

use alloc::{string::String, vec::Vec};

use crate::msgpack;

pub const SF_COMPRESSION: u64 = 0x00;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryAnnounce {
    pub display_name: Option<Vec<u8>>,
    pub stamp_cost: Option<u8>,
    pub compression_supported: bool,
}

impl DeliveryAnnounce {
    /// Encode the current LXMF 1.0.1 three-element announce format.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        msgpack::array(&mut out, 3);
        if let Some(name) = &self.display_name {
            msgpack::bin(&mut out, name);
        } else {
            msgpack::nil(&mut out);
        }
        if let Some(cost) = self.stamp_cost {
            msgpack::uint(&mut out, cost as u64);
        } else {
            msgpack::nil(&mut out);
        }
        msgpack::array(&mut out, usize::from(self.compression_supported));
        if self.compression_supported {
            msgpack::uint(&mut out, SF_COMPRESSION);
        }
        out
    }

    /// Decode both the original raw-name format and all list formats since
    /// LXMF 0.5. Missing capability lists imply compression support, matching
    /// the Python reference's backwards-compatibility rule.
    pub fn decode(data: &[u8]) -> Result<Self, msgpack::Error> {
        if data.is_empty() {
            return Ok(Self {
                display_name: None,
                stamp_cost: None,
                compression_supported: true,
            });
        }
        if !matches!(data[0], 0x90..=0x9f | 0xdc | 0xdd) {
            return Ok(Self {
                display_name: Some(data.to_vec()),
                stamp_cost: None,
                compression_supported: true,
            });
        }

        let mut pos = 0;
        let len = msgpack::array_len(data, &mut pos)?;
        let display_name = if len >= 1 {
            if data.get(pos) == Some(&0xc0) {
                msgpack::read_nil(data, &mut pos)?;
                None
            } else {
                Some(msgpack::read_bin(data, &mut pos)?.to_vec())
            }
        } else {
            None
        };
        let stamp_cost = if len >= 2 {
            if data.get(pos) == Some(&0xc0) {
                msgpack::read_nil(data, &mut pos)?;
                None
            } else {
                Some(
                    u8::try_from(msgpack::read_uint(data, &mut pos)?)
                        .map_err(|_| msgpack::Error::Overflow)?,
                )
            }
        } else {
            None
        };
        let compression_supported = if len < 3 {
            true
        } else if !matches!(data.get(pos), Some(0x90..=0x9f | 0xdc | 0xdd)) {
            msgpack::skip(data, &mut pos)?;
            true
        } else {
            let functions = msgpack::array_len(data, &mut pos)?;
            let mut compression = false;
            for _ in 0..functions {
                if msgpack::read_uint(data, &mut pos)? == SF_COMPRESSION {
                    compression = true;
                }
            }
            compression
        };
        for _ in 3..len {
            msgpack::skip(data, &mut pos)?;
        }
        if pos != data.len() {
            return Err(msgpack::Error::Trailing);
        }
        Ok(Self {
            display_name,
            stamp_cost,
            compression_supported,
        })
    }

    pub fn display_name(&self) -> Option<String> {
        let name = core::str::from_utf8(self.display_name.as_deref()?).ok()?;
        Some(name.replace('\0', "").trim().into())
    }
}

/// Compatibility wrapper retaining the original small API.
pub fn delivery(display_name: Option<&[u8]>, stamp_cost: Option<u8>) -> Vec<u8> {
    DeliveryAnnounce {
        display_name: display_name.map(<[u8]>::to_vec),
        stamp_cost,
        compression_supported: true,
    }
    .encode()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_delivery_announce_round_trips() {
        let encoded = delivery(Some(b"Alice"), Some(8));
        let decoded = DeliveryAnnounce::decode(&encoded).unwrap();
        assert_eq!(decoded.display_name(), Some("Alice".into()));
        assert!(decoded.compression_supported);
    }

    #[test]
    fn old_two_element_announce_remains_accepted() {
        let old = hex::decode("92c405416c69636508").unwrap();
        let decoded = DeliveryAnnounce::decode(&old).unwrap();
        assert!(decoded.compression_supported);
        assert_eq!(decoded.stamp_cost, Some(8));
    }
}
