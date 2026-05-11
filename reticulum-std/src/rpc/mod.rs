//! RPC server for Python CLI tool compatibility
//!
//! Implements the `multiprocessing.connection` wire protocol so that Python
//! tools (`rnstatus`, `rnpath`, `rnprobe`) can query the running Rust daemon
//! as if it were a Python shared instance.
//!
//! Three layers:
//! - `connection`: Wire protocol (length-prefixed framing, HMAC handshake)
//! - `pickle`: Request parsing and response building (pickle ser/de)
//! - `handlers`: RPC command dispatch and state queries

pub(crate) mod connection;
mod error;
mod handlers;
pub(crate) mod pickle;

use std::sync::{Arc, Mutex};

use tokio::net::UnixListener;
use tokio::sync::watch;

use crate::driver::StdNodeCore;
use crate::interfaces::InterfaceStatsMap;
use connection::{read_message, server_handshake, write_message};
use error::RpcError;
use handlers::handle_request;
use pickle::parse_request;

/// Spawn the RPC server on abstract Unix socket `\0rns/{instance_name}/rpc`.
///
/// Accepts connections concurrently (each in its own task).
/// Each connection: handshake -> read request -> dispatch -> write response -> close.
pub(crate) fn spawn_rpc_server(
    instance_name: &str,
    core: Arc<Mutex<StdNodeCore>>,
    authkey: [u8; 32],
    start_time: std::time::Instant,
    iface_stats_map: InterfaceStatsMap,
    auto_peer_count_rx: Option<watch::Receiver<usize>>,
) -> Result<(), std::io::Error> {
    let abstract_name = format!("rns/{}/rpc", instance_name);

    use std::os::linux::net::SocketAddrExt;
    let addr = std::os::unix::net::SocketAddr::from_abstract_name(abstract_name.as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let std_listener = std::os::unix::net::UnixListener::bind_addr(&addr)?;
    std_listener.set_nonblocking(true)?;
    let listener = UnixListener::from_std(std_listener)?;

    tracing::info!(
        "RPC server listening on abstract socket \\0{}",
        abstract_name
    );

    tokio::spawn(async move {
        rpc_accept_loop(
            listener,
            core,
            authkey,
            start_time,
            iface_stats_map,
            auto_peer_count_rx,
        )
        .await;
    });

    Ok(())
}

/// Accept loop: spawns a task per connection.
async fn rpc_accept_loop(
    listener: UnixListener,
    core: Arc<Mutex<StdNodeCore>>,
    authkey: [u8; 32],
    start_time: std::time::Instant,
    iface_stats_map: InterfaceStatsMap,
    auto_peer_count_rx: Option<watch::Receiver<usize>>,
) {
    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                tracing::warn!("RPC accept error: {}", e);
                continue;
            }
        };

        let core = Arc::clone(&core);
        let stats_map = Arc::clone(&iface_stats_map);
        let peer_count_rx = auto_peer_count_rx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_rpc_connection(
                stream,
                &core,
                &authkey,
                start_time,
                &stats_map,
                &peer_count_rx,
            )
            .await
            {
                tracing::debug!("RPC connection error: {}", e);
            }
        });
    }
}

/// Handle a single RPC connection: handshake -> read -> dispatch -> write -> close.
async fn handle_rpc_connection(
    mut stream: tokio::net::UnixStream,
    core: &Arc<Mutex<StdNodeCore>>,
    authkey: &[u8; 32],
    start_time: std::time::Instant,
    iface_stats_map: &InterfaceStatsMap,
    auto_peer_count_rx: &Option<watch::Receiver<usize>>,
) -> Result<(), RpcError> {
    server_handshake(&mut stream, authkey).await?;

    let request_bytes = read_message(&mut stream).await?;
    let request = parse_request(&request_bytes)?;

    tracing::debug!("RPC request: {:?}", request);

    let response_bytes = {
        let mut core = core.lock().unwrap();
        let peer_count = auto_peer_count_rx
            .as_ref()
            .map(|rx| *rx.borrow())
            .unwrap_or(0);
        handle_request(&request, &mut core, start_time, iface_stats_map, peer_count)?
    };

    write_message(&mut stream, &response_bytes).await?;

    Ok(())
}

// Client-side functions
/// Connect to the RPC server, perform handshake, send request, receive response.
pub(crate) async fn rpc_client_call(
    abstract_name: &str,
    authkey: &[u8; 32],
    request: &serde_pickle::value::Value,
) -> Result<serde_pickle::value::Value, RpcError> {
    use std::os::linux::net::SocketAddrExt;
    use tokio::net::UnixStream;

    let addr = std::os::unix::net::SocketAddr::from_abstract_name(abstract_name.as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    let std_stream = std::os::unix::net::UnixStream::connect_addr(&addr)?;
    std_stream.set_nonblocking(true)?;
    let mut stream = UnixStream::from_std(std_stream)?;

    connection::client_handshake(&mut stream, authkey).await?;

    let request_bytes = serde_pickle::value_to_vec(request, Default::default())
        .map_err(|e| RpcError::Pickle(format!("serialize request: {}", e)))?;
    write_message(&mut stream, &request_bytes).await?;

    let response_bytes = read_message(&mut stream).await?;
    let response: serde_pickle::value::Value =
        serde_pickle::value_from_slice(&response_bytes, Default::default())
            .map_err(|e| RpcError::Pickle(format!("deserialize response: {}", e)))?;

    Ok(response)
}

/// Issue a parameterless `get` query against a running shared-instance daemon's
/// RPC socket (`\0rns/{instance_name}/rpc`) and return the response decoded into
/// a [`serde_json::Value`].
///
/// `get_key` must be one of the parameterless RPC keys understood by the daemon
/// (and by Python `rnsd`): `"interface_stats"`, `"path_table"`, `"link_count"`,
/// `"rate_table"`, `"blackholed_identities"`. Queries that take parameters
/// (`next_hop`, `packet_rssi`, …) and the mutating `drop`/`blackhole`/
/// `destination_data` ops are intentionally not reachable through this helper.
///
/// `authkey` is `SHA256(transport_identity)` — the daemon derives the same key
/// from its `{config_dir}/storage/transport_identity` file (raw 64 bytes).
///
/// Returns the response as JSON: pickle dicts become objects (non-string keys
/// stringified), `bytes` values become lowercase hex strings, tuples/lists/sets
/// become arrays, `None` becomes `null`, big ints become decimal strings.
pub async fn rpc_query(
    instance_name: &str,
    authkey: &[u8; 32],
    get_key: &str,
) -> Result<serde_json::Value, crate::Error> {
    let abstract_name = format!("rns/{}/rpc", instance_name);
    let request = pickle::pickle_dict(vec![(
        pickle::pickle_str_key("get"),
        pickle::pickle_str(get_key),
    )]);
    let response = rpc_client_call(&abstract_name, authkey, &request)
        .await
        .map_err(|e| match e {
            RpcError::Io(io) => crate::Error::Io(io),
            other => crate::Error::Config(format!("shared-instance RPC error: {other}")),
        })?;
    Ok(pickle_value_to_json(&response))
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn f64_to_json(f: f64) -> serde_json::Value {
    serde_json::Number::from_f64(f)
        .map(serde_json::Value::Number)
        .unwrap_or(serde_json::Value::Null)
}

fn pickle_value_to_json(v: &serde_pickle::value::Value) -> serde_json::Value {
    use serde_json::Value as J;
    use serde_pickle::value::Value as P;
    match v {
        P::None => J::Null,
        P::Bool(b) => J::Bool(*b),
        P::I64(n) => J::from(*n),
        P::Int(n) => J::String(n.to_string()),
        P::F64(f) => f64_to_json(*f),
        P::Bytes(b) => J::String(hex_lower(b)),
        P::String(s) => J::String(s.clone()),
        P::List(items) | P::Tuple(items) => {
            J::Array(items.iter().map(pickle_value_to_json).collect())
        }
        P::Set(items) | P::FrozenSet(items) => {
            J::Array(items.iter().map(pickle_hashable_to_json).collect())
        }
        P::Dict(d) => J::Object(
            d.iter()
                .map(|(k, v)| (pickle_hashable_key_string(k), pickle_value_to_json(v)))
                .collect(),
        ),
    }
}

fn pickle_hashable_to_json(h: &serde_pickle::value::HashableValue) -> serde_json::Value {
    use serde_json::Value as J;
    use serde_pickle::value::HashableValue as H;
    match h {
        H::None => J::Null,
        H::Bool(b) => J::Bool(*b),
        H::I64(n) => J::from(*n),
        H::Int(n) => J::String(n.to_string()),
        H::F64(f) => f64_to_json(*f),
        H::Bytes(b) => J::String(hex_lower(b)),
        H::String(s) => J::String(s.clone()),
        H::Tuple(items) => J::Array(items.iter().map(pickle_hashable_to_json).collect()),
        H::FrozenSet(items) => J::Array(items.iter().map(pickle_hashable_to_json).collect()),
    }
}

fn pickle_hashable_key_string(h: &serde_pickle::value::HashableValue) -> String {
    use serde_pickle::value::HashableValue as H;
    match h {
        H::String(s) => s.clone(),
        H::I64(n) => n.to_string(),
        H::Int(n) => n.to_string(),
        H::Bool(b) => b.to_string(),
        H::Bytes(b) => hex_lower(b),
        H::F64(f) => f.to_string(),
        H::None => "null".to_string(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interfaces::InterfaceStatsMap;
    use pickle::{pickle_dict, pickle_str, pickle_str_key};
    use serde_pickle::value::{HashableValue, Value};

    /// Derive the RPC authkey from a NodeCore identity (same as driver).
    fn derive_authkey(core: &Arc<Mutex<StdNodeCore>>) -> [u8; 32] {
        let core_guard = core.lock().unwrap();
        let prv = core_guard.identity().private_key_bytes().unwrap();
        use sha2::Digest;
        let hash = sha2::Sha256::digest(prv);
        let mut key = [0u8; 32];
        key.copy_from_slice(&hash);
        key
    }

    static RPC_TEST_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    /// Create a minimal ReticulumNode and extract its inner Arc<Mutex<StdNodeCore>>.
    fn make_test_core(transport: bool) -> Arc<Mutex<StdNodeCore>> {
        let id = RPC_TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let storage_path = std::env::temp_dir().join(format!(
            "reticulum_rpc_test_{}_{}_{}",
            std::process::id(),
            id,
            transport
        ));
        let node = crate::driver::ReticulumNodeBuilder::new()
            .storage_path(storage_path)
            .enable_transport(transport)
            .build_sync()
            .expect("build_sync failed");
        node.inner()
    }

    fn empty_stats_map() -> InterfaceStatsMap {
        Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()))
    }

    /// Spawn a minimal RPC server and test it with a Rust client.
    #[tokio::test]
    async fn test_rpc_interface_stats_round_trip() {
        let core = make_test_core(true);
        let start_time = std::time::Instant::now();
        let authkey = derive_authkey(&core);

        let instance_name = format!("rpctest_{}", std::process::id());
        let abstract_name = format!("rns/{}/rpc", instance_name);

        spawn_rpc_server(
            &instance_name,
            Arc::clone(&core),
            authkey,
            start_time,
            empty_stats_map(),
            None,
        )
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let request = pickle_dict(vec![(pickle_str_key("get"), pickle_str("interface_stats"))]);
        let response = rpc_client_call(&abstract_name, &authkey, &request)
            .await
            .unwrap();

        match &response {
            Value::Dict(d) => {
                assert!(
                    d.contains_key(&HashableValue::String("transport_id".into())),
                    "response should contain transport_id"
                );
                assert!(
                    d.contains_key(&HashableValue::String("transport_uptime".into())),
                    "response should contain transport_uptime"
                );
                assert!(
                    d.contains_key(&HashableValue::String("interfaces".into())),
                    "response should contain interfaces"
                );

                if let Some(Value::F64(uptime)) =
                    d.get(&HashableValue::String("transport_uptime".into()))
                {
                    assert!(*uptime >= 0.0, "uptime should be non-negative");
                }
            }
            other => panic!("expected dict response, got: {:?}", other),
        }
    }

    /// Test that wrong authkey is rejected.
    #[tokio::test]
    async fn test_rpc_auth_failure() {
        let core = make_test_core(true);
        let start_time = std::time::Instant::now();
        let authkey = derive_authkey(&core);

        let instance_name = format!("rpctest_auth_{}", std::process::id());
        let abstract_name = format!("rns/{}/rpc", instance_name);

        spawn_rpc_server(
            &instance_name,
            Arc::clone(&core),
            authkey,
            start_time,
            empty_stats_map(),
            None,
        )
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let wrong_key = [0xFFu8; 32];
        let request = pickle_dict(vec![(pickle_str_key("get"), pickle_str("interface_stats"))]);
        let result = rpc_client_call(&abstract_name, &wrong_key, &request).await;
        assert!(result.is_err(), "wrong authkey should cause failure");
    }

    /// Test link_count RPC.
    #[tokio::test]
    async fn test_rpc_link_count() {
        let core = make_test_core(false);
        let start_time = std::time::Instant::now();
        let authkey = derive_authkey(&core);

        let instance_name = format!("rpctest_lc_{}", std::process::id());
        let abstract_name = format!("rns/{}/rpc", instance_name);

        spawn_rpc_server(
            &instance_name,
            Arc::clone(&core),
            authkey,
            start_time,
            empty_stats_map(),
            None,
        )
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let request = pickle_dict(vec![(pickle_str_key("get"), pickle_str("link_count"))]);
        let response = rpc_client_call(&abstract_name, &authkey, &request)
            .await
            .unwrap();

        match response {
            Value::I64(count) => assert_eq!(count, 0, "no links established"),
            other => panic!("expected int, got: {:?}", other),
        }
    }
}
