//! `punch-probe` (+ciris): measure the direct-link punch between two real
//! hosts over a real relay, and say why it fails.
//!
//! ```text
//! punch-probe --relay HOST:PORT listen
//! punch-probe --relay HOST:PORT dial --to DEST_HEX --key SIGNING_KEY_HEX --attempts 5
//! ```
//!
//! Both roles attach a TCP client interface to the relay (whose TCP port is
//! also its UDP facilitator), check this host's NAT mapping against the
//! facilitator (and optionally a public STUN server), and then:
//! - `listen` announces `ciris.punchprobe` with `direct_connect_policy =
//!   accept_all` and prints its destination hash and signing key;
//! - `dial` finds a path, opens a link and proposes a direct link N times,
//!   spaced past the proposal cooldown, printing one JSON line per attempt.
//!
//! Output is JSON lines on stdout. No line carries an IP address or a port:
//! leviculum's own address-bearing log lines are not printed (only the
//! `direct_link_diag` summaries, which hold counts and classes), unless
//! `--verbose` sends every log line to stderr for a local run.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use leviculum_core::direct_link::{wire, ProbeProtocol};
use leviculum_core::node::DirectLinkPolicy;
use leviculum_core::{Destination, DestinationHash, DestinationType, Direction, Identity};
use leviculum_std::direct_link::{DirectLinkSettings, DIAG_TARGET};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::{EventReceiver, NodeEvent};
use rand_core::RngCore;
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

#[derive(Parser)]
#[command(
    name = "punch-probe",
    about = "Measure a direct-link punch over a relay"
)]
struct Cli {
    /// The relay's TCP dial, which is also its UDP facilitator unless
    /// `--facilitator` names another.
    #[arg(long)]
    relay: String,
    /// The facilitator this node probes (`host:port`), when it is not the
    /// relay: a public STUN server with `--protocol stun`, so a run over a
    /// third-party relay needs no facilitator of anyone's.
    #[arg(long)]
    facilitator: Option<String>,
    /// How to probe the facilitator: rnsp or stun.
    #[arg(long, default_value = "rnsp")]
    protocol: String,
    /// A public STUN server (`host:port`) for the mapping-behaviour check.
    #[arg(long)]
    stun: Option<String>,
    /// A second observer the node's session socket also probes, so it can
    /// tell an endpoint-dependent NAT (and only then advertise predicted
    /// ports or "b"). TEST ONLY: a production node has none unless its
    /// operator configures one (`probe_addr2`).
    #[arg(long)]
    facilitator2: Option<String>,
    /// How to probe `--facilitator2`: rnsp or stun.
    #[arg(long, default_value = "stun")]
    protocol2: String,
    /// Print every log line to stderr (local runs only: they carry addresses).
    #[arg(long)]
    verbose: bool,
    #[command(subcommand)]
    role: Role,
}

#[derive(Subcommand)]
enum Role {
    /// Announce a destination and accept direct links.
    Listen {
        /// Stop after this many seconds (0: run until killed).
        #[arg(long, default_value_t = 0)]
        duration_secs: u64,
        /// Re-announce this often.
        #[arg(long, default_value_t = 120)]
        announce_secs: u64,
        /// This listener's throwaway identity, a private key `keygen` wrote
        /// (never derived from a seed: third review C). Without one, a random
        /// identity. Its destination and public key: `whois`.
        #[arg(long)]
        identity_file: Option<std::path::PathBuf>,
        /// Accept a direct link only from the dialer with this identity hash
        /// (hex, from `keygen`): ask_host with an allowlist, never accept_all
        /// on a public destination. Only the public hash is ever handed out.
        #[arg(long)]
        allow: Option<String>,
        /// A file of allowed identity hashes (hex, one a line), re-read on
        /// every request: a dialing runner's hash is appended once it has
        /// published it. Starts empty: nobody is admitted until then.
        #[arg(long)]
        allow_file: Option<std::path::PathBuf>,
    },
    /// Make a random throwaway identity for one run: its private key goes to
    /// `--out` (keep it off the run log and out of git), its public hash to
    /// stdout.
    Keygen {
        #[arg(long)]
        out: std::path::PathBuf,
    },
    /// Print the destination and public signing key `listen
    /// --identity-file` will use (both public); no network.
    Whois {
        #[arg(long)]
        identity_file: std::path::PathBuf,
    },
    /// Link to a listener and propose a direct link repeatedly.
    Dial {
        /// The listener's destination hash (hex).
        #[arg(long)]
        to: String,
        /// The listener's signing key (hex, 32 bytes).
        #[arg(long)]
        key: String,
        #[arg(long, default_value_t = 3)]
        attempts: u32,
        /// Seconds between proposals; leviculum refuses under 60.
        #[arg(long, default_value_t = 70)]
        spacing_secs: u64,
        /// This dialer's throwaway identity, a private key `keygen` wrote; it
        /// identifies itself on the link, so the listener's allowlist
        /// (`listen --allow`) admits it.
        #[arg(long)]
        identity_file: std::path::PathBuf,
    },
}

fn out(v: Value) {
    println!("{v}");
}

/// Forwards `direct_link_diag` events, as JSON objects, to a channel.
struct DiagLayer(mpsc::UnboundedSender<Map<String, Value>>);

struct Fields(Map<String, Value>);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
        self.0
            .insert(f.name().into(), Value::String(format!("{v:?}")));
    }
    fn record_str(&mut self, f: &tracing::field::Field, v: &str) {
        self.0.insert(f.name().into(), Value::String(v.into()));
    }
    fn record_u64(&mut self, f: &tracing::field::Field, v: u64) {
        self.0.insert(f.name().into(), json!(v));
    }
    fn record_i64(&mut self, f: &tracing::field::Field, v: i64) {
        self.0.insert(f.name().into(), json!(v));
    }
    fn record_bool(&mut self, f: &tracing::field::Field, v: bool) {
        self.0.insert(f.name().into(), json!(v));
    }
}

impl<S: tracing::Subscriber> Layer<S> for DiagLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != DIAG_TARGET {
            return;
        }
        let mut fields = Fields(Map::new());
        event.record(&mut fields);
        fields.0.remove("message");
        let _ = self.0.send(fields.0);
    }
}

/// An identity from a key file `keygen` wrote, or a clear error.
fn identity_from_file(path: &std::path::Path) -> Result<Identity, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("cannot read the key file: {e}"))?;
    let bytes = unhex::<64>(&text).map_err(|e| format!("the key file is not a key: {e}"))?;
    Identity::from_private_key_bytes(&bytes).map_err(|_| "the key file is not a key".to_string())
}

/// Exit with a one-line error (no panic, no address).
fn fail(role: &str, error: &str) -> ! {
    out(json!({"role": role, "kind": "error", "error": error}));
    std::process::exit(2);
}

/// The `ciris.punchprobe` destination and signing key for an identity.
fn listener_destination(identity: Identity) -> (DestinationHash, [u8; 32]) {
    let key: [u8; 32] = identity.public_key_bytes()[32..64].try_into().unwrap();
    let dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "ciris",
        &["punchprobe"],
    )
    .expect("destination");
    (*dest.hash(), key)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex<const N: usize>(s: &str) -> Result<[u8; N], String> {
    let s = s.trim();
    if s.len() != N * 2 {
        return Err(format!("expected {} hex digits, got {}", N * 2, s.len()));
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// One RNSP or STUN probe of `server` from `socket`: the mapped address.
async fn probe_once(
    socket: &tokio::net::UdpSocket,
    server: SocketAddr,
    protocol: ProbeProtocol,
) -> Option<SocketAddr> {
    let mut buf = [0u8; 576];
    for _ in 0..3 {
        let (request, nonce, txn) = match protocol {
            ProbeProtocol::Rnsp => {
                let mut n = [0u8; 16];
                rand_core::OsRng.fill_bytes(&mut n);
                (wire::rnsp_request(&n).to_vec(), n, [0u8; 12])
            }
            ProbeProtocol::Stun => {
                let mut t = [0u8; 12];
                rand_core::OsRng.fill_bytes(&mut t);
                (wire::stun_binding_request(&t).to_vec(), [0u8; 16], t)
            }
        };
        socket.send_to(&request, server).await.ok()?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(1500);
        while let Ok(Ok((len, src))) =
            tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await
        {
            if src != server {
                continue;
            }
            let got = match protocol {
                ProbeProtocol::Rnsp => wire::parse_rnsp_response(&buf[..len], &nonce),
                ProbeProtocol::Stun => wire::parse_stun_binding_response(&buf[..len], &txn),
            };
            if got.is_some() {
                return got;
            }
        }
    }
    None
}

async fn resolve_v4(addr: &str) -> Option<SocketAddr> {
    tokio::net::lookup_host(addr)
        .await
        .ok()?
        .find(SocketAddr::is_ipv4)
}

/// This host's NAT, in classes: one socket probes the facilitator twice
/// (is the mapping stable?) and, with `--stun`, a second server (does the
/// mapping depend on the destination? that is a symmetric NAT on THIS side).
async fn nat_check(role: &str, relay: SocketAddr, protocol: ProbeProtocol, stun: Option<&str>) {
    let Ok(socket) = tokio::net::UdpSocket::bind("0.0.0.0:0").await else {
        out(json!({"role": role, "kind": "nat_check", "error": "bind"}));
        return;
    };
    let local_port = socket.local_addr().map(|a| a.port()).unwrap_or(0);
    let host_ip = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| s.connect(relay).and_then(|_| s.local_addr()))
        .map(|a| a.ip())
        .ok();
    let first = probe_once(&socket, relay, protocol).await;
    let second = probe_once(&socket, relay, protocol).await;
    let stun_addr = match stun {
        Some(s) => resolve_v4(s).await,
        None => None,
    };
    let via_stun = match stun_addr {
        Some(s) => probe_once(&socket, s, ProbeProtocol::Stun).await,
        None => None,
    };
    let yn = |b: Option<bool>| match b {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    };
    // A fresh socket: does the NAT keep a mapping's port across sockets
    // (port-preserving) or allocate sequentially / randomly?
    let other = tokio::net::UdpSocket::bind("0.0.0.0:0").await.ok();
    let other_mapped = match &other {
        Some(o) => probe_once(o, relay, protocol).await,
        None => None,
    };
    let other_preserved = match (&other, other_mapped) {
        (Some(o), Some(m)) => Some(o.local_addr().map(|a| a.port()).ok() == Some(m.port())),
        _ => None,
    };
    out(json!({
        "role": role,
        "kind": "nat_check",
        "facilitator_answered": first.is_some(),
        "no_nat": yn(first.zip(host_ip).map(|(m, h)| m.ip() == h)),
        "port_preserved": yn(first.map(|m| m.port() == local_port)),
        "second_socket_port_preserved": yn(other_preserved),
        "mapping_stable_same_dest": yn(first.zip(second).map(|(a, b)| a == b)),
        "stun_answered": via_stun.is_some(),
        "mapping_same_across_dests": yn(first.zip(via_stun).map(|(a, b)| a == b)),
        "mapping_same_ip_across_dests": yn(first.zip(via_stun).map(|(a, b)| a.ip() == b.ip())),
    }));
}

fn event_json(role: &str, ev: &NodeEvent) -> Option<Value> {
    Some(match ev {
        NodeEvent::DirectLinkEstablished { proposed, .. } => {
            json!({"role": role, "kind": "event", "event": "DirectLinkEstablished", "proposed": proposed})
        }
        NodeEvent::DirectLinkFailed {
            failure, proposed, ..
        } => {
            json!({"role": role, "kind": "event", "event": "DirectLinkFailed", "failure": format!("{failure:?}"), "proposed": proposed})
        }
        NodeEvent::DirectLinkLost { .. } => {
            json!({"role": role, "kind": "event", "event": "DirectLinkLost"})
        }
        NodeEvent::LinkEstablished { .. } => {
            json!({"role": role, "kind": "event", "event": "LinkEstablished"})
        }
        NodeEvent::LinkClosed {
            reason,
            is_initiator,
            ..
        } => {
            json!({"role": role, "kind": "event", "event": "LinkClosed",
                   "reason": format!("{reason:?}"), "initiator": is_initiator})
        }
        _ => return None,
    })
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let cli = Cli::parse();
    let (diag_tx, mut diag_rx) = mpsc::unbounded_channel();
    let registry = tracing_subscriber::registry().with(DiagLayer(diag_tx));
    if cli.verbose {
        registry
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_filter(tracing_subscriber::EnvFilter::new(
                        std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
                    )),
            )
            .init();
    } else {
        registry.init();
    }

    if let Role::Keygen { out: path } = &cli.role {
        let identity = Identity::generate(&mut rand_core::OsRng);
        let key = identity.private_key_bytes().expect("a private key");
        // 0600 from creation (third review C): never readable by others.
        use std::io::Write;
        #[cfg(unix)]
        let file = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
        };
        #[cfg(not(unix))]
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path);
        match file.and_then(|mut f| f.write_all(hex(&key).as_bytes())) {
            Ok(()) => {}
            Err(e) => fail("keygen", &format!("cannot write the key file: {e}")),
        }
        out(json!({"kind": "keygen", "hash": hex(identity.hash())}));
        return;
    }
    if let Role::Whois { identity_file } = &cli.role {
        let identity = identity_from_file(identity_file).unwrap_or_else(|e| fail("whois", &e));
        let (hash, key) = listener_destination(identity);
        out(json!({"kind": "whois", "dest": hex(hash.as_bytes()), "key": hex(&key)}));
        return;
    }
    let role = match cli.role {
        Role::Listen { .. } => "listen",
        Role::Dial { .. } => "dial",
        Role::Whois { .. } | Role::Keygen { .. } => unreachable!(),
    };
    let Some(relay) = resolve_v4(&cli.relay).await else {
        out(json!({"role": role, "kind": "error", "error": "relay does not resolve"}));
        std::process::exit(2);
    };
    let protocol = if cli.protocol == "stun" {
        ProbeProtocol::Stun
    } else {
        ProbeProtocol::Rnsp
    };
    let facilitator_name = cli.facilitator.clone().unwrap_or_else(|| cli.relay.clone());
    let Some(facilitator) = resolve_v4(&facilitator_name).await else {
        out(json!({"role": role, "kind": "error", "error": "facilitator does not resolve"}));
        std::process::exit(2);
    };
    nat_check(role, facilitator, protocol, cli.stun.as_deref()).await;

    let storage = tempfile::tempdir().expect("tempdir");
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(relay)
        .direct_link({
            // ask_host: the listener admits only its allowlisted dialer, and
            // only a node answering identified peers may spray.
            let mut settings = DirectLinkSettings::default();
            settings.policy = DirectLinkPolicy::AskHost;
            settings.facilitator = Some(facilitator_name.clone());
            settings.protocol = protocol;
            settings.facilitator2 = cli.facilitator2.clone();
            settings.protocol2 = if cli.protocol2 == "rnsp" {
                ProbeProtocol::Rnsp
            } else {
                ProbeProtocol::Stun
            };
            settings
        })
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build node");
    node.start().await.expect("start node");
    node.set_direct_link_retry(false);
    let mut rx: EventReceiver = node.take_event_receiver().expect("event receiver");
    tokio::time::sleep(Duration::from_secs(2)).await;

    match cli.role {
        Role::Whois { .. } | Role::Keygen { .. } => unreachable!(),
        Role::Listen {
            duration_secs,
            announce_secs,
            identity_file,
            allow,
            allow_file,
        } => {
            // Somebody must be admissible, and what is named must parse.
            if allow.is_none() && allow_file.is_none() {
                fail(role, "listen needs --allow HASH or --allow-file PATH");
            }
            if let Some(a) = &allow {
                if unhex::<16>(a).is_err() {
                    fail(role, "--allow must be a 16-byte identity hash in hex");
                }
            }
            // Read on every request (a dialer's hash may arrive mid-run).
            let allowed = move || -> Vec<[u8; 16]> {
                let mut list: Vec<String> = allow.iter().cloned().collect();
                if let Some(f) = &allow_file {
                    list.extend(
                        std::fs::read_to_string(f)
                            .unwrap_or_default()
                            .lines()
                            .map(|l| l.trim().to_string())
                            .filter(|l| !l.is_empty()),
                    );
                }
                list.iter().filter_map(|h| unhex::<16>(h).ok()).collect()
            };
            let identity = match &identity_file {
                Some(f) => identity_from_file(f).unwrap_or_else(|e| fail(role, &e)),
                None => Identity::generate(&mut rand_core::OsRng),
            };
            let key: [u8; 32] = identity.public_key_bytes()[32..64].try_into().unwrap();
            let mut dest = Destination::new(
                Some(identity),
                Direction::In,
                DestinationType::Single,
                "ciris",
                &["punchprobe"],
            )
            .expect("destination");
            dest.set_accepts_links(true);
            let hash = *dest.hash();
            node.register_destination(dest);
            out(
                json!({"role": role, "kind": "ready", "dest": hex(hash.as_bytes()), "key": hex(&key)}),
            );
            let stop_at =
                (duration_secs > 0).then(|| Instant::now() + Duration::from_secs(duration_secs));
            let mut announce = tokio::time::interval(Duration::from_secs(announce_secs.max(10)));
            loop {
                if stop_at.is_some_and(|t| Instant::now() >= t) {
                    break;
                }
                tokio::select! {
                    _ = announce.tick() => {
                        let ok = node.announce_destination(&hash, Some(b"punchprobe")).await.is_ok();
                        out(json!({"role": role, "kind": "announce", "ok": ok}));
                    }
                    ev = rx.recv() => match ev {
                        Some(NodeEvent::DirectLinkRequested { link_id, peer_identity, .. }) => {
                            let ok = peer_identity.is_some_and(|p| allowed().contains(&p));
                            let r = if ok {
                                node.accept_direct_link(&link_id).await
                            } else {
                                node.reject_direct_link(&link_id).await
                            };
                            out(json!({"role": role, "kind": "event", "event": "DirectLinkRequested",
                                       "allowlisted": ok, "answered": r.is_ok()}));
                        }
                        Some(ev) => if let Some(v) = event_json(role, &ev) { out(v) },
                        None => break,
                    },
                    d = diag_rx.recv() => if let Some(d) = d {
                        out(json!({"role": role, "kind": "diag", "diag": d}));
                    },
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
        }
        Role::Dial {
            to,
            key,
            attempts,
            spacing_secs,
            identity_file,
        } => {
            let me = identity_from_file(&identity_file).unwrap_or_else(|e| fail(role, &e));
            let hash = DestinationHash::new(unhex::<16>(&to).expect("--to"));
            let key = unhex::<32>(&key).expect("--key");
            // Up to three 60 s waits, each one logged: a miss is a finding
            // (it is reported per wait), not a reason to lose the job.
            let mut found = false;
            for wait in 1..=3u32 {
                let t = Instant::now();
                found = node
                    .wait_for_path(&hash, Duration::from_secs(60), Duration::from_secs(5))
                    .await
                    .unwrap_or(false);
                out(
                    json!({"role": role, "kind": "path", "wait": wait, "found": found, "ms": t.elapsed().as_millis() as u64, "hops": node.hops_to(&hash)}),
                );
                if found {
                    break;
                }
            }
            if !found {
                std::process::exit(3);
            }
            let mut link = None;
            let mut attempt = 1;
            // Proposals refused by policy (the listener has not admitted this
            // dialer's hash yet): retried, not counted, at most this often.
            let mut not_admitted = 0u32;
            // Wait until `$next`, logging every event and diag line meanwhile;
            // a close of our link is logged and drops it, so the next attempt
            // reopens it instead of proposing on a link that is gone.
            macro_rules! idle_until {
                ($next:expr) => {{
                    let until = tokio::time::Instant::from_std($next);
                    loop {
                        tokio::select! {
                            _ = tokio::time::sleep_until(until) => break,
                            d = diag_rx.recv() => if let Some(d) = d {
                                out(json!({"role": role, "kind": "diag", "diag": d}));
                            },
                            ev = rx.recv() => match ev {
                                Some(NodeEvent::LinkClosed { link_id: l, reason, .. })
                                    if link.as_ref().is_some_and(|(id, _)| *id == l) =>
                                {
                                    out(json!({"role": role, "kind": "link_lost_idle",
                                               "reason": format!("{reason:?}")}));
                                    link = None;
                                }
                                Some(ev) => if let Some(v) = event_json(role, &ev) { out(v) },
                                None => break,
                            },
                        }
                    }
                }};
            }
            while attempt <= attempts {
                // (Re)open the link when there is none.
                if link.is_none() {
                    let t = Instant::now();
                    let opened = match node.connect(&hash, &key).await {
                        Ok(h) => {
                            let id = *h.link_id();
                            match tokio::time::timeout(
                                Duration::from_secs(30),
                                node.await_link_established(&id),
                            )
                            .await
                            {
                                Ok(Ok(_)) => {
                                    // Identify, so the listener's allowlist
                                    // can admit this dialer.
                                    let _ = node.identify_link(&id, &me).await;
                                    Some((id, h))
                                }
                                _ => None,
                            }
                        }
                        Err(_) => None,
                    };
                    out(
                        json!({"role": role, "kind": "link", "established": opened.is_some(), "ms": t.elapsed().as_millis() as u64}),
                    );
                    link = opened;
                    // Let the link settle (identify, first keepalive).
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
                let Some((link_id, _handle)) = &link else {
                    // No link this round: the attempt is spent (a while loop
                    // must advance here, or it would spin).
                    attempt += 1;
                    continue;
                };
                let link_id = *link_id;
                while diag_rx.try_recv().is_ok() {}
                let proposed_at = Instant::now();
                let proposal = node.propose_direct_link(&link_id).await;
                let mut outcome = Value::Null;
                let mut failure = Value::Null;
                let mut diags = Vec::new();
                if let Err(e) = &proposal {
                    outcome = json!("refused_locally");
                    failure = json!(e.to_string());
                } else {
                    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
                    loop {
                        tokio::select! {
                            _ = tokio::time::sleep_until(deadline) => {
                                outcome = json!("no_outcome_45s");
                                break;
                            }
                            d = diag_rx.recv() => if let Some(d) = d { diags.push(Value::Object(d)) },
                            ev = rx.recv() => match ev {
                                Some(NodeEvent::DirectLinkEstablished { link_id: l, .. }) if l == link_id => {
                                    outcome = json!("established");
                                    break;
                                }
                                Some(NodeEvent::DirectLinkFailed { link_id: l, failure: f, .. }) if l == link_id => {
                                    outcome = json!("failed");
                                    failure = json!(format!("{f:?}"));
                                    break;
                                }
                                Some(NodeEvent::LinkClosed { link_id: l, .. }) if l == link_id => {
                                    outcome = json!("link_closed");
                                    link = None;
                                    break;
                                }
                                Some(_) => {}
                                None => break,
                            },
                        }
                    }
                }
                // Diag lines logged just before the event.
                tokio::time::sleep(Duration::from_millis(200)).await;
                while let Ok(d) = diag_rx.try_recv() {
                    diags.push(Value::Object(d));
                }
                let refused_by_policy = failure == json!("Rejected(1)");
                if refused_by_policy && not_admitted < 10 {
                    not_admitted += 1;
                    out(json!({"role": role, "kind": "not_admitted", "tries": not_admitted}));
                    idle_until!(proposed_at + Duration::from_secs(61));
                    continue;
                }
                out(json!({
                    "role": role, "kind": "attempt", "attempt": attempt,
                    "outcome": outcome, "failure": failure,
                    "ms": proposed_at.elapsed().as_millis() as u64,
                    "diag": diags,
                }));
                if outcome == json!("established") {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    let _ = node.retire_direct_link(&link_id).await;
                }
                if attempt < attempts {
                    idle_until!(proposed_at + Duration::from_secs(spacing_secs.max(61)));
                }
                attempt += 1;
            }
        }
    }
    let _ = node.stop().await;
}
