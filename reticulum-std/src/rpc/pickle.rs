//! Pickle serialization for RPC request/response dicts
//!
//! Uses `serde_pickle` to parse Python pickle dicts into typed Rust enums
//! and serialize response dicts back to pickle bytes.

use serde_pickle::value::{HashableValue, Value};
use std::collections::BTreeMap;

use super::error::RpcError;

/// Parsed RPC request.
///
/// Fields are parsed from pickle dicts and logged via `Debug`.
/// Some stub fields (blackhole params) are not yet read by handlers.
#[derive(Debug)]
#[allow(dead_code)] // blackhole fields not yet used — see Codeberg issues
pub(crate) enum RpcRequest {
    // GET commands
    GetInterfaceStats,
    GetLinkCount,
    GetPathTable {
        max_hops: Option<i64>,
    },
    GetRateTable,
    GetNextHop {
        destination_hash: Vec<u8>,
    },
    GetNextHopIfName {
        destination_hash: Vec<u8>,
    },
    GetFirstHopTimeout {
        destination_hash: Vec<u8>,
    },
    GetPacketRssi {
        packet_hash: Vec<u8>,
    },
    GetPacketSnr {
        packet_hash: Vec<u8>,
    },
    GetPacketQ {
        packet_hash: Vec<u8>,
    },
    GetBlackholedIdentities,
    // DROP commands
    DropPath {
        destination_hash: Vec<u8>,
    },
    DropAllVia {
        destination_hash: Vec<u8>,
    },
    DropAnnounceQueues,
    // BLACKHOLE commands
    BlackholeIdentity {
        identity_hash: Vec<u8>,
        until: Option<f64>,
        reason: Option<String>,
    },
    UnblackholeIdentity {
        identity_hash: Vec<u8>,
    },
    // destination_data lifecycle commands (stubs — see handlers.rs)
    DestinationDataUsed {
        destination_hash: Vec<u8>,
    },
    DestinationDataRetain {
        destination_hash: Vec<u8>,
    },
    DestinationDataUnretain {
        destination_hash: Vec<u8>,
    },
}

/// Parse an RPC request from pickle bytes.
pub(crate) fn parse_request(data: &[u8]) -> Result<RpcRequest, RpcError> {
    let value: Value = serde_pickle::value_from_slice(data, Default::default())
        .map_err(|e| RpcError::Pickle(format!("deserialize: {}", e)))?;

    let dict = match value {
        Value::Dict(d) => d,
        _ => return Err(RpcError::InvalidFormat("expected dict".into())),
    };

    // Check for "get" key
    if let Some(get_val) = dict_get_str(&dict, "get") {
        return match get_val.as_str() {
            "interface_stats" => Ok(RpcRequest::GetInterfaceStats),
            "link_count" => Ok(RpcRequest::GetLinkCount),
            "path_table" => {
                let max_hops = dict_get_int(&dict, "max_hops");
                Ok(RpcRequest::GetPathTable { max_hops })
            }
            "rate_table" => Ok(RpcRequest::GetRateTable),
            "next_hop" => {
                let destination_hash = dict_get_bytes(&dict, "destination_hash")
                    .ok_or_else(|| RpcError::InvalidFormat("missing destination_hash".into()))?;
                Ok(RpcRequest::GetNextHop { destination_hash })
            }
            "next_hop_if_name" => {
                let destination_hash = dict_get_bytes(&dict, "destination_hash")
                    .ok_or_else(|| RpcError::InvalidFormat("missing destination_hash".into()))?;
                Ok(RpcRequest::GetNextHopIfName { destination_hash })
            }
            "first_hop_timeout" => {
                let destination_hash = dict_get_bytes(&dict, "destination_hash")
                    .ok_or_else(|| RpcError::InvalidFormat("missing destination_hash".into()))?;
                Ok(RpcRequest::GetFirstHopTimeout { destination_hash })
            }
            "packet_rssi" => {
                let packet_hash = dict_get_bytes(&dict, "packet_hash")
                    .ok_or_else(|| RpcError::InvalidFormat("missing packet_hash".into()))?;
                Ok(RpcRequest::GetPacketRssi { packet_hash })
            }
            "packet_snr" => {
                let packet_hash = dict_get_bytes(&dict, "packet_hash")
                    .ok_or_else(|| RpcError::InvalidFormat("missing packet_hash".into()))?;
                Ok(RpcRequest::GetPacketSnr { packet_hash })
            }
            "packet_q" => {
                let packet_hash = dict_get_bytes(&dict, "packet_hash")
                    .ok_or_else(|| RpcError::InvalidFormat("missing packet_hash".into()))?;
                Ok(RpcRequest::GetPacketQ { packet_hash })
            }
            "blackholed_identities" => Ok(RpcRequest::GetBlackholedIdentities),
            other => Err(RpcError::InvalidFormat(format!(
                "unknown get command: {}",
                other
            ))),
        };
    }

    // Check for "drop" key
    if let Some(drop_val) = dict_get_str(&dict, "drop") {
        return match drop_val.as_str() {
            "path" => {
                let destination_hash = dict_get_bytes(&dict, "destination_hash")
                    .ok_or_else(|| RpcError::InvalidFormat("missing destination_hash".into()))?;
                Ok(RpcRequest::DropPath { destination_hash })
            }
            "all_via" => {
                let destination_hash = dict_get_bytes(&dict, "destination_hash")
                    .ok_or_else(|| RpcError::InvalidFormat("missing destination_hash".into()))?;
                Ok(RpcRequest::DropAllVia { destination_hash })
            }
            "announce_queues" => Ok(RpcRequest::DropAnnounceQueues),
            other => Err(RpcError::InvalidFormat(format!(
                "unknown drop command: {}",
                other
            ))),
        };
    }

    // Check for "blackhole_identity" key
    if let Some(identity_bytes) = dict_get_bytes(&dict, "blackhole_identity") {
        let until = dict_get_float(&dict, "until");
        let reason = dict_get_str(&dict, "reason");
        return Ok(RpcRequest::BlackholeIdentity {
            identity_hash: identity_bytes,
            until,
            reason,
        });
    }

    // Check for "unblackhole_identity" key
    if let Some(identity_bytes) = dict_get_bytes(&dict, "unblackhole_identity") {
        return Ok(RpcRequest::UnblackholeIdentity {
            identity_hash: identity_bytes,
        });
    }

    // Check for "destination_data" key (used / retain / unretain operations)
    if let Some(op) = dict_get_str(&dict, "destination_data") {
        let destination_hash = dict_get_bytes(&dict, "destination_hash")
            .ok_or_else(|| RpcError::InvalidFormat("missing destination_hash".into()))?;
        return match op.as_str() {
            "used" => Ok(RpcRequest::DestinationDataUsed { destination_hash }),
            "retain" => Ok(RpcRequest::DestinationDataRetain { destination_hash }),
            "unretain" => Ok(RpcRequest::DestinationDataUnretain { destination_hash }),
            other => Err(RpcError::InvalidFormat(format!(
                "unknown destination_data operation: {}",
                other
            ))),
        };
    }

    Err(RpcError::InvalidFormat("unrecognized request".into()))
}

/// Serialize an RPC response value to pickle bytes.
pub(crate) fn serialize_response(value: &Value) -> Result<Vec<u8>, RpcError> {
    serde_pickle::value_to_vec(value, Default::default())
        .map_err(|e| RpcError::Pickle(format!("serialize: {}", e)))
}

// Pickle dict helpers
/// Create a pickle string key
pub(crate) fn pickle_str_key(s: &str) -> HashableValue {
    HashableValue::String(s.into())
}

/// Create a pickle string value
pub(crate) fn pickle_str(s: &str) -> Value {
    Value::String(s.into())
}

/// Create a pickle bytes value
pub(crate) fn pickle_bytes(b: &[u8]) -> Value {
    Value::Bytes(b.to_vec())
}

/// Create a pickle int value
pub(crate) fn pickle_int(n: i64) -> Value {
    Value::I64(n)
}

/// Create a pickle float value
pub(crate) fn pickle_float(f: f64) -> Value {
    Value::F64(f)
}

/// Create a pickle bool value
pub(crate) fn pickle_bool(b: bool) -> Value {
    Value::Bool(b)
}

/// Create a pickle None value
pub(crate) fn pickle_none() -> Value {
    Value::None
}

/// Build a pickle dict from key-value pairs
pub(crate) fn pickle_dict(entries: Vec<(HashableValue, Value)>) -> Value {
    Value::Dict(BTreeMap::from_iter(entries))
}

/// Build a pickle list
pub(crate) fn pickle_list(items: Vec<Value>) -> Value {
    Value::List(items)
}

// Internal helpers
fn dict_get_str(dict: &BTreeMap<HashableValue, Value>, key: &str) -> Option<String> {
    let k = HashableValue::String(key.into());
    match dict.get(&k) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn dict_get_bytes(dict: &BTreeMap<HashableValue, Value>, key: &str) -> Option<Vec<u8>> {
    let k = HashableValue::String(key.into());
    match dict.get(&k) {
        Some(Value::Bytes(b)) => Some(b.clone()),
        _ => None,
    }
}

fn dict_get_int(dict: &BTreeMap<HashableValue, Value>, key: &str) -> Option<i64> {
    let k = HashableValue::String(key.into());
    match dict.get(&k) {
        Some(Value::I64(n)) => Some(*n),
        Some(Value::None) => None,
        _ => None,
    }
}

fn dict_get_float(dict: &BTreeMap<HashableValue, Value>, key: &str) -> Option<f64> {
    let k = HashableValue::String(key.into());
    match dict.get(&k) {
        Some(Value::F64(f)) => Some(*f),
        Some(Value::None) => None,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_get_request(command: &str) -> Vec<u8> {
        let dict = pickle_dict(vec![(pickle_str_key("get"), pickle_str(command))]);
        serde_pickle::value_to_vec(&dict, Default::default()).unwrap()
    }

    fn build_get_request_with_bytes(command: &str, key: &str, value: &[u8]) -> Vec<u8> {
        let dict = pickle_dict(vec![
            (pickle_str_key("get"), pickle_str(command)),
            (pickle_str_key(key), pickle_bytes(value)),
        ]);
        serde_pickle::value_to_vec(&dict, Default::default()).unwrap()
    }

    #[test]
    fn test_parse_get_interface_stats() {
        let data = build_get_request("interface_stats");
        let req = parse_request(&data).unwrap();
        assert!(matches!(req, RpcRequest::GetInterfaceStats));
    }

    #[test]
    fn test_parse_get_link_count() {
        let data = build_get_request("link_count");
        let req = parse_request(&data).unwrap();
        assert!(matches!(req, RpcRequest::GetLinkCount));
    }

    #[test]
    fn test_parse_get_path_table() {
        let dict = pickle_dict(vec![
            (pickle_str_key("get"), pickle_str("path_table")),
            (pickle_str_key("max_hops"), pickle_int(5)),
        ]);
        let data = serde_pickle::value_to_vec(&dict, Default::default()).unwrap();
        let req = parse_request(&data).unwrap();
        match req {
            RpcRequest::GetPathTable { max_hops } => assert_eq!(max_hops, Some(5)),
            other => panic!("expected GetPathTable, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_get_next_hop() {
        let hash = vec![0xAB; 16];
        let data = build_get_request_with_bytes("next_hop", "destination_hash", &hash);
        let req = parse_request(&data).unwrap();
        match req {
            RpcRequest::GetNextHop { destination_hash } => assert_eq!(destination_hash, hash),
            other => panic!("expected GetNextHop, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_drop_path() {
        let hash = vec![0xCD; 16];
        let dict = pickle_dict(vec![
            (pickle_str_key("drop"), pickle_str("path")),
            (pickle_str_key("destination_hash"), pickle_bytes(&hash)),
        ]);
        let data = serde_pickle::value_to_vec(&dict, Default::default()).unwrap();
        let req = parse_request(&data).unwrap();
        match req {
            RpcRequest::DropPath { destination_hash } => assert_eq!(destination_hash, hash),
            other => panic!("expected DropPath, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_blackhole_identity() {
        let hash = vec![0xEF; 16];
        let dict = pickle_dict(vec![
            (pickle_str_key("blackhole_identity"), pickle_bytes(&hash)),
            (pickle_str_key("until"), pickle_float(1234567.0)),
            (pickle_str_key("reason"), pickle_str("testing")),
        ]);
        let data = serde_pickle::value_to_vec(&dict, Default::default()).unwrap();
        let req = parse_request(&data).unwrap();
        match req {
            RpcRequest::BlackholeIdentity {
                identity_hash,
                until,
                reason,
            } => {
                assert_eq!(identity_hash, hash);
                assert_eq!(until, Some(1234567.0));
                assert_eq!(reason, Some("testing".into()));
            }
            other => panic!("expected BlackholeIdentity, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_unknown_get_command() {
        let data = build_get_request("nonexistent");
        let result = parse_request(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_serialize_response_round_trip() {
        let response = pickle_dict(vec![
            (pickle_str_key("transport_id"), pickle_bytes(&[0x42; 16])),
            (pickle_str_key("transport_uptime"), pickle_float(123.456)),
            (pickle_str_key("interfaces"), pickle_list(vec![])),
        ]);
        let bytes = serialize_response(&response).unwrap();
        let parsed: Value = serde_pickle::value_from_slice(&bytes, Default::default()).unwrap();
        match parsed {
            Value::Dict(d) => {
                assert!(d.contains_key(&pickle_str_key("transport_id")));
                assert!(d.contains_key(&pickle_str_key("transport_uptime")));
            }
            _ => panic!("expected dict"),
        }
    }
}
