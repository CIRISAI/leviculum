//! The frame-aware TCP proxy the identify-loss tests share, in both directions.
//!
//! A LINKIDENTIFY frame is not acknowledged by anything in the protocol: the
//! sender emits it and the advertisement back to back, so losing the first
//! silently changes what the receiver knows about the second. To test either
//! side of that, the loss has to be exact — one named frame gone, every other
//! byte of the session identical.
//!
//! This proxy sits between two Reticulum peers on loopback TCP, deframes the
//! HDLC stream, classifies each packet by its context byte
//! ([`leviculum_core::packet::peek_wire_class`]) and swallows the first
//! `identify_drop_budget` LINKIDENTIFY frames travelling **towards the
//! upstream**, re-framing everything else onward untouched. The gate is the
//! context byte, not a time window: the byte gate in
//! `tests/mvr/link_failure_recovery_silent_resume.rs:64-110` drops everything
//! inside a window, which here would take the advertisement with it and prove
//! nothing.
//!
//! Who "upstream" is depends on the direction under test. In
//! `lncp_identify_loss_interop_tests` the upstream is a Python `rncp -l` and
//! our sender is downstream; in `lncp_listener_identify_loss_interop_tests`
//! the upstream is our `lncp -l` and the sender (Python or ours) is
//! downstream. The drop is always on the leg carrying the identify, so the
//! same budget argument means the same thing in every test.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use leviculum_core::packet::peek_wire_class;
use leviculum_core::{Destination, DestinationHash, DestinationType, Direction, Identity};
use leviculum_std::process::spawn_supervised;
use rand_core::OsRng;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub const RNCP_PY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../reference/Reticulum/RNS/Utilities/rncp.py"
);
pub const VENDOR_RNS_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../reference/Reticulum");

/// `PacketContext::LinkIdentify` as it appears on the wire
/// (`leviculum-core/src/packet.rs:175`). The proxy reads bytes, so it names
/// the byte.
pub const CTX_LINK_IDENTIFY: u8 = 0xFB;
/// `PacketContext::ResourceAdv` (`packet.rs:161`).
pub const CTX_RESOURCE_ADV: u8 = 0x02;
/// `PacketContext::ResourceRcl` (`packet.rs:166`) — the receiver's rejection.
pub const CTX_RESOURCE_RCL: u8 = 0x07;

/// True when `python3` can import the vendored `RNS` package. Same skip
/// condition the rest of the suite uses.
pub fn python_rns_available() -> bool {
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
pub struct RncpListener {
    pub config_dir: PathBuf,
    pub save_dir: PathBuf,
    child: Child,
}

impl RncpListener {
    /// What `rncp` logged, for an assertion message.
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.config_dir.join("rncp.log")).unwrap_or_default()
    }
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
pub fn spawn_rncp_listener(
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

/// Write the config and identity a Python `rncp -S` sender needs, and return
/// `(config_dir, identity_path)`.
///
/// The identity is seeded from Rust so a listener's `-a` can name its hash
/// before either process starts.
pub fn prepare_rncp_sender(
    tmp: &std::path::Path,
    identity: &Identity,
    target_port: u16,
) -> (PathBuf, PathBuf) {
    let config_dir = tmp.join("rncp-sender-config");
    std::fs::create_dir_all(&config_dir).expect("create sender config dir");
    let identity_path = config_dir.join("rncp_identity");
    std::fs::write(
        &identity_path,
        identity.private_key_bytes().expect("private key bytes"),
    )
    .expect("write sender identity");

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
         \x20 [[Identify Loss TCP Client]]\n\
         \x20   type = TCPClientInterface\n\
         \x20   enabled = yes\n\
         \x20   target_host = 127.0.0.1\n\
         \x20   target_port = {target_port}\n"
    );
    std::fs::write(config_dir.join("config"), config).expect("write sender config");

    (config_dir, identity_path)
}

/// Run `rncp -S -w <wait_secs>` to completion, pushing `file_path` to
/// `dest_hash_hex`, and return its `Output`.
///
/// Hard-capped by `timeout(1)`: with `-S` the sender's path and status waits
/// (`rncp.py:659-664`, `:724-729`) spin without sleeping, so a sender that
/// never concludes would block the calling test forever.
///
/// `PYTHONUNBUFFERED` because `RNS.exit` is `os._exit`
/// (`reference/Reticulum/RNS/__init__.py:350-355`): it skips the interpreter's
/// flush, so rncp's last line -- the one that says what happened -- never
/// reaches a pipe otherwise.
pub async fn run_rncp_sender(
    config_dir: PathBuf,
    identity_path: PathBuf,
    file_path: PathBuf,
    dest_hash_hex: String,
    wait_secs: u32,
    hard_cap_secs: u32,
) -> std::process::Output {
    tokio::task::spawn_blocking(move || {
        Command::new("timeout")
            .arg(hard_cap_secs.to_string())
            .arg("python3")
            .arg(RNCP_PY)
            .arg("--config")
            .arg(&config_dir)
            .arg("-i")
            .arg(&identity_path)
            .arg("-S")
            .arg("-w")
            .arg(wait_secs.to_string())
            .arg(&file_path)
            .arg(&dest_hash_hex)
            .env("PYTHONPATH", VENDOR_RNS_ROOT)
            .env("PYTHONUNBUFFERED", "1")
            .output()
            .expect("run rncp sender")
    })
    .await
    .expect("rncp sender join")
}

/// A Python `rncp -S` sender left running, killed on drop.
///
/// The run-to-completion form above cannot be used where the sender may not
/// conclude at all: RNS 1.5.2 defines `Resource.REJECTED = 0x00`, the same
/// value as `Resource.NONE` (`reference/Reticulum/RNS/Resource.py:142-152`),
/// so a rejected sender's `while resource.status < RNS.Resource.TRANSFERRING`
/// spin (`rncp.py:703-708`) never ends. A test that waits for such a sender
/// waits for its hard cap; this one watches the wire instead and kills the
/// process when it has seen what it came for.
pub struct RncpSender {
    log_path: PathBuf,
    child: Child,
}

impl RncpSender {
    /// The sender's exit status if it has concluded, `None` while it runs.
    pub fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// The tail of what the sender logged, for an assertion message.
    pub fn log_tail(&self, bytes: usize) -> String {
        let log = std::fs::read_to_string(&self.log_path).unwrap_or_default();
        let start = log.len().saturating_sub(bytes);
        log[start..].replace('\n', " | ")
    }
}

impl Drop for RncpSender {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start `rncp -S -w <wait_secs>` pushing `file_path` to `dest_hash_hex`, and
/// leave it running.
pub fn spawn_rncp_sender(
    config_dir: &std::path::Path,
    identity_path: &std::path::Path,
    file_path: &std::path::Path,
    dest_hash_hex: &str,
    wait_secs: u32,
) -> RncpSender {
    let log_path = config_dir.join("rncp-sender.log");
    let log = std::fs::File::create(&log_path).expect("create rncp sender log");
    let log_err = log.try_clone().expect("clone log handle");
    let mut cmd = Command::new("python3");
    cmd.arg(RNCP_PY)
        .arg("--config")
        .arg(config_dir)
        .arg("-i")
        .arg(identity_path)
        .arg("-S")
        .arg("-w")
        .arg(wait_secs.to_string())
        .arg(file_path)
        .arg(dest_hash_hex)
        .arg("-vvv")
        .env("PYTHONPATH", VENDOR_RNS_ROOT)
        .env("PYTHONUNBUFFERED", "1")
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    let child = spawn_supervised(cmd).expect("spawn rncp -S");

    RncpSender { log_path, child }
}

/// Every frame the proxy carried, by context byte and direction, plus the
/// frames it swallowed and when the two frames this pair of tests turns on
/// went past. This is the tests' evidence: their assertions quote it, so a red
/// run names the chain instead of pointing at a log.
#[derive(Default)]
pub struct ProxyLog {
    /// Context bytes travelling towards the upstream peer (the proxy's
    /// connect side), in order.
    pub to_upstream: Vec<u8>,
    /// Context bytes travelling back towards the downstream peer (the one that
    /// connected to the proxy), in order.
    pub to_downstream: Vec<u8>,
    pub dropped_identify: usize,
    /// When the first resource advertisement reached the upstream, and when
    /// the first rejection came back. The gap between them is how long a
    /// sender waits for an answer it can act on.
    pub first_adv_to_upstream: Option<Instant>,
    pub first_rcl_to_downstream: Option<Instant>,
}

impl ProxyLog {
    pub fn saw_to_upstream(&self, ctx: u8) -> bool {
        self.to_upstream.contains(&ctx)
    }

    pub fn saw_to_downstream(&self, ctx: u8) -> bool {
        self.to_downstream.contains(&ctx)
    }

    pub fn count_to_upstream(&self, ctx: u8) -> usize {
        self.to_upstream.iter().filter(|c| **c == ctx).count()
    }

    pub fn count_to_downstream(&self, ctx: u8) -> usize {
        self.to_downstream.iter().filter(|c| **c == ctx).count()
    }

    /// How long the downstream sender waited between its advertisement and the
    /// upstream's rejection, when both were seen.
    pub fn adv_to_rcl(&self) -> Option<std::time::Duration> {
        let adv = self.first_adv_to_upstream?;
        let rcl = self.first_rcl_to_downstream?;
        rcl.checked_duration_since(adv)
    }

    /// One line naming the whole exchange, quoted by every assertion.
    pub fn evidence(&self) -> String {
        format!(
            "proxy: {} identify frame(s) dropped, advertisement sent={}, RCL received={}, \
             adv->rcl={:?}; contexts to upstream={:02x?}, to downstream={:02x?}",
            self.dropped_identify,
            self.saw_to_upstream(CTX_RESOURCE_ADV),
            self.saw_to_downstream(CTX_RESOURCE_RCL),
            self.adv_to_rcl(),
            self.to_upstream,
            self.to_downstream,
        )
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
    to_upstream: bool,
    identify_drop_budget: usize,
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
                    if to_upstream {
                        log.to_upstream.push(ctx);
                        if ctx == CTX_RESOURCE_ADV && log.first_adv_to_upstream.is_none() {
                            log.first_adv_to_upstream = Some(Instant::now());
                        }
                    } else {
                        log.to_downstream.push(ctx);
                        if ctx == CTX_RESOURCE_RCL && log.first_rcl_to_downstream.is_none() {
                            log.first_rcl_to_downstream = Some(Instant::now());
                        }
                    }
                    if to_upstream
                        && ctx == CTX_LINK_IDENTIFY
                        && log.dropped_identify < identify_drop_budget
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
/// first `identify_drop_budget` LINKIDENTIFY frames travelling towards the
/// upstream.
///
/// Bound before the call returns so a peer's TCP client cannot race the
/// accept loop into a refused connect (Codeberg #221).
pub async fn spawn_identify_dropping_proxy(
    listen_port: u16,
    upstream_port: u16,
    identify_drop_budget: usize,
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
            tokio::spawn(pump(
                down_rx,
                up_tx,
                true,
                identify_drop_budget,
                Arc::clone(&log),
            ));
            tokio::spawn(pump(up_rx, down_tx, false, 0, Arc::clone(&log)));
        }
    });
}
