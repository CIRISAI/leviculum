//! A lost LINKIDENTIFY turns into a rejected transfer, and lncp gives up.
//!
//! `lora_lncp_auth_to_python` went RED in the full hardware run of
//! 2026-09-23 06:18. Alpha (our `lnsd`, the sender) put three frames on the
//! wire inside 130 ms — LRRTT, LINKIDENTIFY, RESOURCE_ADV — and 2.3 s later
//! got a 115 B answer that was an `RCL` where the green runs carry a
//! `RESOURCE_REQ`. Python 1.5.2 answers an advertisement with `RCL` in exactly
//! one place: `receive_resource_callback` returns False
//! (`reference/Reticulum/RNS/Utilities/rncp.py:254-266`) because
//! `resource.link.get_remote_identity()` is None, and `Link.py:1091-1097` then
//! calls `RNS.Resource.reject`. The allowed hash was alpha's, so the identity
//! was absent: the identify frame never reached Python's link.
//!
//! The link stayed up the whole time. `Resource.reject` sends the RCL and
//! nothing else. The sender had a live link and a peer that had simply not
//! heard who it was, and it quit.
//!
//! This test **models** that loss; it does not reproduce the air. Whether the
//! 06:18 frame died on air or in the receiving modem's turnaround is a
//! third-listener measurement on the rig, owed separately. Here a frame-aware
//! TCP proxy sits between our node and a Python `rncp -l -a <our hash>` and
//! drops exactly the first packet whose context byte is `LINKIDENTIFY` (0xFB),
//! passing every other frame in both directions untouched.
//!
//! The proxy classifies by context byte via
//! [`leviculum_core::packet::peek_wire_class`], not by time window — the byte
//! gate in `tests/mvr/link_failure_recovery_silent_resume.rs:64-110` drops
//! everything inside a window, which here would take the advertisement with it
//! and prove nothing.
//!
//! The assertion is the user-visible outcome: `lncp` delivers the file over a
//! link whose identify was lost once. On the code of 2026-09-23 it does not —
//! the failure message carries the observed chain (ADV sent, RCL received,
//! `ResourceError::Cancelled`) so the red is readable without a log dig.
//!
//! The sender is `leviculum_cli::cp::run_send`, i.e. the push loop the `lncp`
//! binary runs, not a copy of it.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use leviculum_core::packet::peek_wire_class;
use leviculum_core::{Destination, DestinationHash, DestinationType, Direction, Identity};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::process::spawn_supervised;
use rand_core::OsRng;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::harness::find_available_ports;

static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

const RNCP_PY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../reference/Reticulum/RNS/Utilities/rncp.py"
);
const VENDOR_RNS_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../reference/Reticulum");

/// `PacketContext::LinkIdentify` as it appears on the wire
/// (`leviculum-core/src/packet.rs:175`). The proxy reads bytes, so it names
/// the byte.
const CTX_LINK_IDENTIFY: u8 = 0xFB;
/// `PacketContext::ResourceAdv` (`packet.rs:161`).
const CTX_RESOURCE_ADV: u8 = 0x02;
/// `PacketContext::ResourceRcl` (`packet.rs:166`) — the receiver's rejection.
const CTX_RESOURCE_RCL: u8 = 0x07;

/// True when `python3` can import the vendored `RNS` package. Same skip
/// condition the rest of the suite uses.
fn python_rns_available() -> bool {
    Command::new("python3")
        .args(["-c", "import RNS"])
        .env("PYTHONPATH", VENDOR_RNS_ROOT)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A spawned Python `rncp -l`, killed on drop.
struct RncpListener {
    config_dir: PathBuf,
    save_dir: PathBuf,
    child: Child,
}

impl Drop for RncpListener {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Boot `rncp -l -a <allowed_hash>` on a pre-seeded identity, listening on one
/// `TCPServerInterface`.
///
/// The identity is generated here rather than by `rncp` so the destination hash
/// is known before the process starts: `RNS.Identity.to_file` writes the raw
/// 64-byte private key our `private_key_bytes()` produces, and
/// `prepare_identity` (`rncp.py:56-68`) loads any file already at `-i`.
fn spawn_rncp_listener(
    tcp_port: u16,
    allowed_hash_hex: &str,
    announce_interval_secs: u32,
    tmp: &std::path::Path,
) -> (RncpListener, DestinationHash) {
    let config_dir = tmp.join("rncp-config");
    let save_dir = tmp.join("rncp-received");
    std::fs::create_dir_all(&config_dir).expect("create rncp config dir");
    std::fs::create_dir_all(&save_dir).expect("create rncp save dir");

    let identity = Identity::generate(&mut OsRng);
    let identity_path = config_dir.join("rncp_identity");
    std::fs::write(
        &identity_path,
        identity.private_key_bytes().expect("private key bytes"),
    )
    .expect("write rncp identity");

    // What rncp itself builds at rncp.py:112.
    let destination = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "rncp",
        &["receive"],
    )
    .expect("build rncp destination");
    let dest_hash = *destination.hash();

    let config = format!(
        "[reticulum]\n\
         \x20 enable_transport = no\n\
         \x20 share_instance = no\n\
         \x20 panic_on_interface_error = no\n\
         \n\
         [logging]\n\
         \x20 loglevel = 5\n\
         \n\
         [interfaces]\n\
         \x20 [[Identify Loss TCP Server]]\n\
         \x20   type = TCPServerInterface\n\
         \x20   enabled = yes\n\
         \x20   listen_ip = 127.0.0.1\n\
         \x20   listen_port = {tcp_port}\n"
    );
    std::fs::write(config_dir.join("config"), config).expect("write rncp config");

    let log = std::fs::File::create(config_dir.join("rncp.log")).expect("create rncp log");
    let log_err = log.try_clone().expect("clone log handle");
    let mut cmd = Command::new("python3");
    cmd.arg(RNCP_PY)
        .arg("--config")
        .arg(&config_dir)
        .arg("-i")
        .arg(&identity_path)
        .arg("-l")
        .arg("-a")
        .arg(allowed_hash_hex)
        .arg("-s")
        .arg(&save_dir)
        .arg("-O")
        .arg("-b")
        .arg(announce_interval_secs.to_string())
        .arg("-vvv")
        .env("PYTHONPATH", VENDOR_RNS_ROOT)
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    let child = spawn_supervised(cmd).expect("spawn rncp -l");

    (
        RncpListener {
            config_dir,
            save_dir,
            child,
        },
        dest_hash,
    )
}

/// Every frame the proxy carried, by context byte and direction, plus the
/// frames it swallowed. This is the test's evidence: the assertion message
/// quotes it, so a red run names the chain instead of pointing at a log.
#[derive(Default)]
struct ProxyLog {
    to_python: Vec<u8>,
    to_node: Vec<u8>,
    dropped_identify: usize,
}

impl ProxyLog {
    fn saw_to_python(&self, ctx: u8) -> bool {
        self.to_python.contains(&ctx)
    }

    fn saw_to_node(&self, ctx: u8) -> bool {
        self.to_node.contains(&ctx)
    }
}

/// One direction of the proxy. Deframes HDLC, classifies each packet by its
/// context byte, optionally swallows the first LINKIDENTIFY, and re-frames the
/// rest onward.
///
/// Re-framing rather than byte-copying is what makes the gate exact: the drop
/// removes one packet and leaves the stream otherwise identical, because
/// `hdlc::frame` is the same encoder both ends use
/// (`leviculum-std/src/interfaces/tcp.rs:1035`, Python's `TCPInterface`).
async fn pump(
    mut src: tokio::net::tcp::OwnedReadHalf,
    mut dst: tokio::net::tcp::OwnedWriteHalf,
    drop_first_identify: bool,
    log: Arc<Mutex<ProxyLog>>,
) {
    let mut deframer = Deframer::new();
    let mut buf = vec![0u8; 8192];
    let mut framed = Vec::new();
    loop {
        let n = match src.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for result in deframer.process(&buf[..n]) {
            let DeframeResult::Frame(packet) = result else {
                continue;
            };
            let ctx = peek_wire_class(&packet).map(|w| w.context);
            let mut swallow = false;
            {
                let mut log = log.lock().expect("proxy log");
                if let Some(ctx) = ctx {
                    if drop_first_identify {
                        log.to_python.push(ctx);
                    } else {
                        log.to_node.push(ctx);
                    }
                    if drop_first_identify && ctx == CTX_LINK_IDENTIFY && log.dropped_identify == 0
                    {
                        log.dropped_identify += 1;
                        swallow = true;
                    }
                }
            }
            if swallow {
                continue;
            }
            frame(&packet, &mut framed);
            if dst.write_all(&framed).await.is_err() {
                return;
            }
        }
    }
}

/// Listen on `listen_port`, forward to `127.0.0.1:upstream_port`, dropping the
/// first LINKIDENTIFY travelling towards the upstream.
///
/// Bound before the call returns so the node's TCP client cannot race the
/// accept loop into a refused connect (Codeberg #221).
async fn spawn_identify_dropping_proxy(
    listen_port: u16,
    upstream_port: u16,
    log: Arc<Mutex<ProxyLog>>,
) {
    let listener = TcpListener::bind(("127.0.0.1", listen_port))
        .await
        .expect("bind identify-dropping proxy");
    tokio::spawn(async move {
        loop {
            let Ok((downstream, _)) = listener.accept().await else {
                return;
            };
            let Ok(upstream) = tokio::net::TcpStream::connect(("127.0.0.1", upstream_port)).await
            else {
                continue;
            };
            let _ = downstream.set_nodelay(true);
            let _ = upstream.set_nodelay(true);
            let (down_rx, down_tx) = downstream.into_split();
            let (up_rx, up_tx) = upstream.into_split();
            tokio::spawn(pump(down_rx, up_tx, true, Arc::clone(&log)));
            tokio::spawn(pump(up_rx, down_tx, false, Arc::clone(&log)));
        }
    });
}

/// A single lost identify must not cost the transfer: `lncp` has a live link
/// and a peer that never heard who it was, and one more identify plus one more
/// advertisement is all the protocol needs.
///
/// Red before the remote-rejection fix: the transfer concludes with
/// `ResourceError::Cancelled` and `lncp` returns the error.
#[tokio::test]
async fn lncp_recovers_from_a_dropped_link_identify() {
    if !python_rns_available() {
        eprintln!("skipping lncp identify-loss interop: python3 + vendored RNS unavailable");
        return;
    }
    crate::common::init_tracing();

    let test_id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = crate::common::temp_storage("lncp_identify_loss", &format!("run{test_id}"));

    let sender_identity = Identity::generate(&mut OsRng);
    let sender_hash_hex = hex::encode(sender_identity.hash());

    let (ports, _alloc) = find_available_ports::<2>().await.expect("allocate ports");
    let (python_port, proxy_port) = (ports[0], ports[1]);

    let (rncp, dest_hash) = spawn_rncp_listener(python_port, &sender_hash_hex, 5, tmp.path());
    let dest_hash_hex = hex::encode(dest_hash.as_bytes());

    let log = Arc::new(Mutex::new(ProxyLog::default()));
    spawn_identify_dropping_proxy(proxy_port, python_port, Arc::clone(&log)).await;

    let proxy_addr: SocketAddr = format!("127.0.0.1:{proxy_port}")
        .parse()
        .expect("proxy addr");
    let storage = tmp.path().join("sender-storage");
    std::fs::create_dir_all(&storage).expect("create sender storage");
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(proxy_addr)
        .storage_path(storage)
        .build()
        .await
        .expect("build sender node");
    let mut events = node.take_event_receiver().expect("event receiver");
    node.start().await.expect("start sender node");

    // rncp announces every 5 s (-b 5); the path and the peer's public key both
    // arrive with the announce, and there is no transport node to answer a
    // path request, so waiting for the announce is the only readiness signal.
    let deadline = Instant::now() + Duration::from_secs(60);
    while !node.has_path(&dest_hash) {
        assert!(
            Instant::now() < deadline,
            "no announce from rncp within 60 s; rncp log: {}",
            std::fs::read_to_string(rncp.config_dir.join("rncp.log")).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let payload: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
    let file_path = tmp.path().join("identify-loss.bin");
    std::fs::write(&file_path, &payload).expect("write payload");

    let outcome = leviculum_cli::cp::run_send(
        &node,
        &mut events,
        file_path.to_str().expect("utf-8 path"),
        &dest_hash_hex,
        Some(90.0),
        0,
        true,
        true,
        Some(&sender_identity),
        false,
    )
    .await;

    let evidence = {
        let log = log.lock().expect("proxy log");
        format!(
            "proxy: {} identify frame(s) dropped, advertisement sent={}, RCL received={}; \
             contexts to python={:02x?}, to node={:02x?}",
            log.dropped_identify,
            log.saw_to_python(CTX_RESOURCE_ADV),
            log.saw_to_node(CTX_RESOURCE_RCL),
            log.to_python,
            log.to_node,
        )
    };

    assert!(
        outcome.is_ok(),
        "lncp must deliver the file after one lost identify, got {:?}. {evidence}",
        outcome.err().map(|e| e.to_string()),
    );

    {
        let log = log.lock().expect("proxy log");
        assert_eq!(
            log.dropped_identify, 1,
            "the modelled loss must have fired exactly once. {evidence}"
        );
    }

    let received = rncp.save_dir.join("identify-loss.bin");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !received.is_file() {
        assert!(
            Instant::now() < deadline,
            "rncp never wrote the file. {evidence}\nrncp log: {}",
            std::fs::read_to_string(rncp.config_dir.join("rncp.log")).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(
        std::fs::read(&received).expect("read received file"),
        payload,
        "rncp must reassemble the payload byte for byte. {evidence}"
    );
}
