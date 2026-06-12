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

// RPC transport. Python `multiprocessing.connection` runs over Unix sockets on
// Unix and over TCP loopback (AF_INET, default local_control_port 37429) on
// Windows; we mirror that so `rnstatus`/`rnpath` interop on each platform. The
// framing + HMAC auth (connection.rs) are transport-agnostic.
#[cfg(windows)]
use tokio::net::TcpListener as RpcListener;
#[cfg(windows)]
use tokio::net::TcpStream as RpcStream;
#[cfg(unix)]
use tokio::net::UnixListener as RpcListener;
#[cfg(unix)]
use tokio::net::UnixStream as RpcStream;
use tokio::sync::watch;

use crate::driver::StdNodeCore;
use crate::interfaces::{InterfaceOnlineMap, InterfaceStatsMap};
use connection::{read_message, server_handshake, write_message};
use error::RpcError;
use handlers::handle_request;
use pickle::parse_request;

/// Bind the RPC listener for the given abstract name.
///
/// On Linux, uses abstract Unix sockets; on other Unix systems, filesystem
/// sockets in the temp directory.
#[cfg(unix)]
fn bind_rpc_listener(
    abstract_name: &str,
) -> Result<std::os::unix::net::UnixListener, std::io::Error> {
    #[cfg(target_os = "linux")]
    {
        use std::os::linux::net::SocketAddrExt;
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(abstract_name.as_bytes())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        std::os::unix::net::UnixListener::bind_addr(&addr)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let path =
            std::env::temp_dir().join(format!("leviculum-{}", abstract_name.replace('/', "-")));
        let _ = std::fs::remove_file(&path);
        std::os::unix::net::UnixListener::bind(&path)
    }
}

/// Windows: bind the RPC listener on TCP loopback (Python-RNS AF_INET fallback).
#[cfg(windows)]
fn bind_rpc_listener(abstract_name: &str) -> Result<std::net::TcpListener, std::io::Error> {
    std::net::TcpListener::bind(crate::interfaces::local::loopback_addr(abstract_name))
}

/// Connect to an RPC socket by abstract name.
#[cfg(unix)]
fn connect_rpc(abstract_name: &str) -> Result<std::os::unix::net::UnixStream, std::io::Error> {
    #[cfg(target_os = "linux")]
    {
        use std::os::linux::net::SocketAddrExt;
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(abstract_name.as_bytes())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        std::os::unix::net::UnixStream::connect_addr(&addr)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let path =
            std::env::temp_dir().join(format!("leviculum-{}", abstract_name.replace('/', "-")));
        std::os::unix::net::UnixStream::connect(&path)
    }
}

/// Windows: connect to the TCP loopback RPC socket.
#[cfg(windows)]
fn connect_rpc(abstract_name: &str) -> Result<std::net::TcpStream, std::io::Error> {
    std::net::TcpStream::connect(crate::interfaces::local::loopback_addr(abstract_name))
}

/// Spawn the RPC server on abstract Unix socket `\0rns/{instance_name}/rpc`.
///
/// Accepts connections concurrently (each in its own task).
/// Each connection: handshake -> read request -> dispatch -> write response -> close.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_rpc_server(
    instance_name: &str,
    core: Arc<Mutex<StdNodeCore>>,
    authkey: [u8; 32],
    start_time: std::time::Instant,
    iface_stats_map: InterfaceStatsMap,
    iface_online_map: InterfaceOnlineMap,
    auto_peer_count_rx: Option<watch::Receiver<usize>>,
) -> Result<(), std::io::Error> {
    let abstract_name = format!("rns/{}/rpc", instance_name);

    let std_listener = bind_rpc_listener(&abstract_name)?;
    std_listener.set_nonblocking(true)?;
    let listener = RpcListener::from_std(std_listener)?;

    tracing::info!("RPC server listening on socket {}", abstract_name);

    tokio::spawn(async move {
        rpc_accept_loop(
            listener,
            core,
            authkey,
            start_time,
            iface_stats_map,
            iface_online_map,
            auto_peer_count_rx,
        )
        .await;
    });

    Ok(())
}

/// Accept loop: spawns a task per connection.
#[allow(clippy::too_many_arguments)]
async fn rpc_accept_loop(
    listener: RpcListener,
    core: Arc<Mutex<StdNodeCore>>,
    authkey: [u8; 32],
    start_time: std::time::Instant,
    iface_stats_map: InterfaceStatsMap,
    iface_online_map: InterfaceOnlineMap,
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
        let online_map = Arc::clone(&iface_online_map);
        let peer_count_rx = auto_peer_count_rx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_rpc_connection(
                stream,
                &core,
                &authkey,
                start_time,
                &stats_map,
                &online_map,
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
#[allow(clippy::too_many_arguments)]
async fn handle_rpc_connection(
    mut stream: RpcStream,
    core: &Arc<Mutex<StdNodeCore>>,
    authkey: &[u8; 32],
    start_time: std::time::Instant,
    iface_stats_map: &InterfaceStatsMap,
    iface_online_map: &InterfaceOnlineMap,
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
        handle_request(
            &request,
            &mut core,
            start_time,
            iface_stats_map,
            iface_online_map,
            peer_count,
        )?
    };

    write_message(&mut stream, &response_bytes).await?;

    Ok(())
}

// Client-side functions

/// Wall-clock cap on the shared-instance RPC handshake + request/response.
///
/// The socket is a local abstract Unix socket, so 5 s is generous. Its real
/// job is to keep a client (`lns diag`, future `lns status`/`interfaces`, …)
/// from blocking forever when the daemon is unresponsive or speaks a slightly
/// different RPC dialect — e.g. a Python `rnsd` that hits an error handling an
/// unexpected request and sends no response at all.
const RPC_CLIENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Connect to the RPC server, perform handshake, send request, receive response.
///
/// The whole handshake + round-trip is bounded by [`RPC_CLIENT_TIMEOUT`]; on
/// expiry this returns a `TimedOut` I/O error rather than hanging.
pub(crate) async fn rpc_client_call(
    abstract_name: &str,
    authkey: &[u8; 32],
    request: &serde_pickle::value::Value,
) -> Result<serde_pickle::value::Value, RpcError> {
    // `connect_rpc` either succeeds quickly or fails fast (ECONNREFUSED /
    // connection refused if nothing is listening) on the loopback transports we
    // use, so it stays outside the request/response timeout below.
    let std_stream = connect_rpc(abstract_name)?;
    std_stream.set_nonblocking(true)?;
    let mut stream = RpcStream::from_std(std_stream)?;

    let request_bytes = serde_pickle::value_to_vec(request, Default::default())
        .map_err(|e| RpcError::Pickle(format!("serialize request: {}", e)))?;

    let exchange = async {
        connection::client_handshake(&mut stream, authkey).await?;
        write_message(&mut stream, &request_bytes).await?;
        let response_bytes = read_message(&mut stream).await?;
        let response: serde_pickle::value::Value =
            serde_pickle::value_from_slice(&response_bytes, Default::default())
                .map_err(|e| RpcError::Pickle(format!("deserialize response: {}", e)))?;
        Ok::<_, RpcError>(response)
    };

    match tokio::time::timeout(RPC_CLIENT_TIMEOUT, exchange).await {
        Ok(result) => result,
        Err(_elapsed) => Err(RpcError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
                "shared-instance RPC did not complete within {} s",
                RPC_CLIENT_TIMEOUT.as_secs()
            ),
        ))),
    }
}

/// Issue a parameterless `get` query against a running shared-instance daemon's
/// RPC socket (`\0rns/{instance_name}/rpc`) and return the response decoded into
/// a [`serde_json::Value`].
///
/// `get_key` must be one of the parameterless RPC keys understood by the daemon:
/// `"interface_stats"`, `"path_table"`, `"link_count"`, `"link_table"`,
/// `"rate_table"`, `"blackholed_identities"`. The first five overlap with
/// Python `rnsd` (`"link_table"` is a Leviculum-only extension — Python has
/// only `link_count` — and degrades to `<unavailable>` against an `rnsd` that
/// rejects it). Queries that take parameters (`next_hop`, `packet_rssi`, …)
/// and the mutating `drop`/`blackhole`/`destination_data` ops are
/// intentionally not reachable through this helper.
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
    let mut entries = vec![(pickle::pickle_str_key("get"), pickle::pickle_str(get_key))];
    // Python `rnsd`'s `path_table` RPC handler reads `call["max_hops"]`
    // unconditionally (it `KeyError`s — and then sends no response — when the
    // key is absent). Python's own client always sends the key, value `None`
    // for "no hop limit" (RNS/Reticulum.py:1331). Our server treats absent and
    // `None` identically, so this is harmless against `lnsd` too.
    if get_key == "path_table" {
        entries.push((
            pickle::pickle_str_key("max_hops"),
            serde_pickle::value::Value::None,
        ));
    }
    let request = pickle::pickle_dict(entries);
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

#[cfg(all(test, unix))]
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

    fn empty_online_map() -> InterfaceOnlineMap {
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
            empty_online_map(),
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
            empty_online_map(),
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
            empty_online_map(),
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

    /// `rpc_query("path_table")` must send `{"get":"path_table","max_hops":None}`
    /// (Python `rnsd` `KeyError`s on a missing `max_hops`). Verify the request
    /// round-trips against our own server and decodes to a JSON array, and that
    /// an explicit `max_hops: None` request is accepted directly.
    #[tokio::test]
    async fn test_rpc_query_path_table_sends_max_hops() {
        let core = make_test_core(true);
        let start_time = std::time::Instant::now();
        let authkey = derive_authkey(&core);

        let instance_name = format!("rpctest_pt_{}", std::process::id());

        spawn_rpc_server(
            &instance_name,
            Arc::clone(&core),
            authkey,
            start_time,
            empty_stats_map(),
            empty_online_map(),
            None,
        )
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // rpc_query builds `rns/{instance_name}/rpc` internally and, for
        // path_table, appends `max_hops: None`.
        let json = rpc_query(&instance_name, &authkey, "path_table")
            .await
            .expect("path_table query should succeed");
        assert!(
            json.is_array(),
            "path_table should decode to a JSON array, got: {json:?}"
        );

        // The explicit `max_hops: None` request shape (what rpc_query sends, and
        // what Python's client sends) is accepted by our server.
        let abstract_name = format!("rns/{}/rpc", instance_name);
        let req = pickle_dict(vec![
            (pickle_str_key("get"), pickle_str("path_table")),
            (pickle_str_key("max_hops"), Value::None),
        ]);
        let resp = rpc_client_call(&abstract_name, &authkey, &req)
            .await
            .expect("explicit max_hops:None request should round-trip");
        assert!(
            matches!(resp, Value::List(_)),
            "expected a list, got: {resp:?}"
        );
    }

    /// `rpc_client_call` must not hang forever when the peer accepts the
    /// connection but never speaks (e.g. a daemon that errored handling the
    /// request and sent no response). It should fail with a `TimedOut` error
    /// within the [`RPC_CLIENT_TIMEOUT`] window.
    #[tokio::test]
    async fn test_rpc_client_call_times_out_on_mute_peer() {
        use std::os::linux::net::SocketAddrExt;

        let abstract_name = format!("rns/rpc-mute-{}/rpc", std::process::id());
        let addr =
            std::os::unix::net::SocketAddr::from_abstract_name(abstract_name.as_bytes()).unwrap();
        let std_listener = std::os::unix::net::UnixListener::bind_addr(&addr).unwrap();
        std_listener.set_nonblocking(true).unwrap();
        let listener = tokio::net::UnixListener::from_std(std_listener).unwrap();

        // Accept connections and hold them open without ever writing a byte.
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                // Keep the stream alive but mute.
                tokio::spawn(async move {
                    let _held = stream;
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                });
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let request = pickle_dict(vec![(pickle_str_key("get"), pickle_str("interface_stats"))]);
        let start = std::time::Instant::now();
        let result = rpc_client_call(&abstract_name, &[0u8; 32], &request).await;
        let elapsed = start.elapsed();

        match result {
            Err(RpcError::Io(ref e)) if e.kind() == std::io::ErrorKind::TimedOut => {}
            other => panic!("expected TimedOut I/O error, got: {other:?}"),
        }
        assert!(
            elapsed >= RPC_CLIENT_TIMEOUT.saturating_sub(std::time::Duration::from_millis(500)),
            "should have waited ~{:?}, only waited {elapsed:?}",
            RPC_CLIENT_TIMEOUT
        );
        assert!(
            elapsed < RPC_CLIENT_TIMEOUT + std::time::Duration::from_secs(3),
            "took far longer than the timeout: {elapsed:?}"
        );
    }

    /// `rpc_query("link_table")` round-trips against our own server and
    /// decodes to a JSON array. Empty for a freshly-built test core that has
    /// no links; the per-row response shape is exercised in the diag-side
    /// rendering test (`reticulum-cli::diag::tests`).
    ///
    /// `link_table` is a Leviculum-only extension (Python `rnsd` has no
    /// equivalent — it exposes `link_count` only). This test guards against
    /// regressions in the request → handler → response wiring.
    #[tokio::test]
    async fn test_rpc_query_link_table_round_trip() {
        let core = make_test_core(true);
        let start_time = std::time::Instant::now();
        let authkey = derive_authkey(&core);

        let instance_name = format!("rpctest_lt_{}", std::process::id());

        spawn_rpc_server(
            &instance_name,
            Arc::clone(&core),
            authkey,
            start_time,
            empty_stats_map(),
            empty_online_map(),
            None,
        )
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let json = rpc_query(&instance_name, &authkey, "link_table")
            .await
            .expect("link_table query should succeed");
        let arr = json
            .as_array()
            .expect("link_table response should be a JSON array");
        assert!(arr.is_empty(), "fresh test core has no links: {arr:?}");
    }

    /// Codeberg #56: the `status` field of each per-interface dict must
    /// reflect the real `Interface::is_online()` value (sourced from
    /// `iface_online_map`), not the hardcoded `true` it used to be.
    ///
    /// Sets up a core with one named interface, marks it offline in the
    /// online map, queries `interface_stats`, and asserts the entry
    /// reports `status: false`. Inverse: a second core+name with the
    /// online map set to `true` reports `status: true`.
    #[tokio::test]
    async fn test_rpc_interface_stats_status_reflects_is_online() {
        for (case, expected_status) in [("offline", false), ("online", true)] {
            let core = make_test_core(true);
            let start_time = std::time::Instant::now();
            let authkey = derive_authkey(&core);

            // Register a fake interface in the core so `core.interface_stats()`
            // returns one entry. The driver does this via `core.set_interface_name()`
            // after registering the handle — here we do it directly.
            let iface_id: usize = 4242;
            let iface_name = format!("TCPInterface[lns-#56-{case}/fake:0000]");
            {
                let mut c = core.lock().unwrap();
                c.set_interface_name(iface_id, iface_name.clone());
            }

            let online_map: InterfaceOnlineMap =
                Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
            {
                let mut m = online_map.lock().unwrap();
                m.insert(iface_id, expected_status);
            }

            let instance_name = format!("rpctest_status_{}_{case}", std::process::id());
            let abstract_name = format!("rns/{instance_name}/rpc");

            spawn_rpc_server(
                &instance_name,
                Arc::clone(&core),
                authkey,
                start_time,
                empty_stats_map(),
                Arc::clone(&online_map),
                None,
            )
            .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;

            let request = pickle_dict(vec![(pickle_str_key("get"), pickle_str("interface_stats"))]);
            let response = rpc_client_call(&abstract_name, &authkey, &request)
                .await
                .unwrap();

            let Value::Dict(d) = &response else {
                panic!("expected dict response, got: {response:?}");
            };
            let Some(Value::List(ifaces)) = d.get(&HashableValue::String("interfaces".into()))
            else {
                panic!("response missing `interfaces` list");
            };
            assert_eq!(
                ifaces.len(),
                1,
                "expected exactly one interface in the response ({case} case), got {ifaces:?}"
            );
            let Value::Dict(iface) = &ifaces[0] else {
                panic!("interface entry not a dict: {:?}", ifaces[0]);
            };
            let status = iface
                .get(&HashableValue::String("status".into()))
                .expect("interface entry missing `status`");
            assert_eq!(
                status,
                &Value::Bool(expected_status),
                "interface {case}: status should be Bool({expected_status})"
            );
            // Also confirm the entry actually corresponds to the interface we
            // registered (the name flows through unchanged).
            assert_eq!(
                iface.get(&HashableValue::String("name".into())),
                Some(&Value::String(iface_name)),
                "interface {case}: name mismatch"
            );
        }
    }
}
