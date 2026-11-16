//! NAT lab node (+ciris, leviculum#100): the real Rust stack, one role per
//! process, for `scripts/natlab/nat_type.py` to run inside network
//! namespaces behind simulated NATs.
//!
//! ```sh
//! natlab_node relay <listen ip> <tcp port> <facilitator port>
//! natlab_node facilitator <udp port>
//! natlab_node peer dest|propose <relay ip:tcp> <facilitator ip:port> \
//!     <second probe server ip:port> <exchange dir> <timeout secs>
//! ```
//!
//! `relay` is a transport node with a TCP server and a facilitator; a
//! `facilitator` answers probes only. A `dest` peer registers a destination,
//! writes its hash and signing key to `<exchange dir>/dest`, announces it
//! until the upgrade ends and accepts the proposal; a `propose` peer reads
//! the file, links to the destination through the relay and proposes the
//! direct link. Both probe the facilitator and the second server from the
//! session socket. Each peer prints one JSON line: its NAT report, how the
//! upgrade ended, and its outcome counters.

use std::net::SocketAddr;
use std::time::Duration;

use leviculum_core::direct_link::{NatMapping, NatReport};
use leviculum_core::node::DirectLinkPolicy;
use leviculum_core::{Destination, DestinationType, Direction, Identity};
use leviculum_std::direct_link::DirectLinkSettings;
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use leviculum_std::NodeEvent;

type BoxError = Box<dyn std::error::Error>;

fn arg(args: &[String], i: usize) -> Result<&str, BoxError> {
    args.get(i)
        .map(String::as_str)
        .ok_or_else(|| format!("missing argument {i}; see the file's doc comment").into())
}

async fn run(builder: ReticulumNodeBuilder) -> Result<ReticulumNode, BoxError> {
    let storage = std::env::temp_dir().join(format!("natlab-{}", std::process::id()));
    std::fs::create_dir_all(&storage)?;
    let mut node = builder.storage_path(storage).build().await?;
    node.start().await?;
    Ok(node)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Result<Vec<u8>, BoxError> {
    (0..s.len())
        .step_by(2)
        .map(|i| Ok(u8::from_str_radix(s.get(i..i + 2).ok_or("odd hex")?, 16)?))
        .collect()
}

fn nat_json(report: Option<&NatReport>) -> serde_json::Value {
    let Some(r) = report else {
        return serde_json::Value::Null;
    };
    serde_json::json!({
        "mapping": r.mapping.name(),
        "probes": r.probes.iter().map(|p| serde_json::json!({
            "server": p.server.to_string(),
            "mapped": p.mapped.map(|m| m.to_string()),
        })).collect::<Vec<_>>(),
        "local_port": r.local_port,
        "port_preserved": r.port_preserved,
        "servers_share_ip": r.servers_share_ip,
        "global_ipv6": r.global_ipv6,
    })
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let args: Vec<String> = std::env::args().collect();
    match arg(&args, 1)? {
        "relay" => {
            let ip = arg(&args, 2)?.to_string();
            let tcp: u16 = arg(&args, 3)?.parse()?;
            let probe: u16 = arg(&args, 4)?.parse()?;
            let _node = run(ReticulumNodeBuilder::new()
                .enable_transport(true)
                .add_interface_config(leviculum_std::config::InterfaceConfig {
                    name: "Lab TCP Server".to_string(),
                    interface_type: "TCPServerInterface".to_string(),
                    listen_ip: Some(ip),
                    listen_port: Some(tcp),
                    ingress_control: Some(false),
                    ..Default::default()
                })
                .direct_link(DirectLinkSettings {
                    facilitator_port: Some(probe),
                    ..Default::default()
                }))
            .await?;
            tokio::signal::ctrl_c().await?;
        }
        "facilitator" => {
            let port: u16 = arg(&args, 2)?.parse()?;
            let _node = run(ReticulumNodeBuilder::new().direct_link(DirectLinkSettings {
                facilitator_port: Some(port),
                ..Default::default()
            }))
            .await?;
            tokio::signal::ctrl_c().await?;
        }
        "peer" => peer(&args).await?,
        other => return Err(format!("unknown role {other:?}").into()),
    }
    Ok(())
}

async fn peer(args: &[String]) -> Result<(), BoxError> {
    let side = arg(args, 2)?;
    let relay: SocketAddr = arg(args, 3)?.parse()?;
    let facilitator = arg(args, 4)?.to_string();
    let second = arg(args, 5)?.to_string();
    let exchange = std::path::PathBuf::from(arg(args, 6)?);
    let timeout = Duration::from_secs(arg(args, 7)?.parse()?);
    let deadline = tokio::time::Instant::now() + timeout;
    let mut node = run(ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(relay)
        .direct_link(DirectLinkSettings {
            policy: DirectLinkPolicy::AcceptAll,
            facilitator: Some(facilitator),
            ..Default::default()
        })
        .nat_probe_servers(vec![second]))
    .await?;
    let mut rx = node.take_event_receiver().ok_or("no event receiver")?;
    let file = exchange.join("dest");

    let mut dest_hash = None;
    let started = tokio::time::Instant::now();
    if side == "dest" {
        let identity = Identity::generate(&mut rand_core::OsRng);
        let signing_key: [u8; 32] = identity.public_key_bytes()[32..64].try_into()?;
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "natlab",
            &["peer"],
        )
        .map_err(|e| format!("destination: {e:?}"))?;
        dest.set_accepts_links(true);
        let hash = *dest.hash();
        node.register_destination(dest);
        let tmp = exchange.join("dest.tmp");
        std::fs::write(
            &tmp,
            format!("{} {}", hex(hash.as_bytes()), hex(&signing_key)),
        )?;
        std::fs::rename(tmp, &file)?;
        dest_hash = Some(hash);
    } else {
        let text = loop {
            if let Ok(t) = std::fs::read_to_string(&file) {
                break t;
            }
            if tokio::time::Instant::now() > deadline {
                return Err("no destination file".into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let (h, k) = text.split_once(' ').ok_or("bad destination file")?;
        let hash = leviculum_core::DestinationHash::new(unhex(h)?.as_slice().try_into()?);
        let key: [u8; 32] = unhex(k)?.as_slice().try_into()?;
        // Wait for the announce through the relay, then link and propose.
        let mut proposed = false;
        while tokio::time::Instant::now() < deadline && !proposed {
            tokio::select! {
                ev = rx.recv() => if ev.is_none() { break },
                _ = tokio::time::sleep(Duration::from_millis(200)) => {}
            }
            if node.hops_to(&hash).is_none() {
                continue;
            }
            let handle = node.connect(&hash, &key).await?;
            let link_id = *handle.link_id();
            node.await_link_established(&link_id).await?;
            node.propose_direct_link(&link_id).await?;
            proposed = true;
            std::mem::forget(handle);
        }
        if !proposed {
            return Err("never linked to the destination".into());
        }
    }

    let mut next_announce = tokio::time::Instant::now();
    let mut outcome = serde_json::Value::Null;
    let mut nat_events: Vec<serde_json::Value> = Vec::new();
    while tokio::time::Instant::now() < deadline {
        if let Some(hash) = &dest_hash {
            if tokio::time::Instant::now() >= next_announce {
                node.announce_destination(hash, Some(b"natlab")).await?;
                next_announce = tokio::time::Instant::now() + Duration::from_secs(2);
            }
        }
        let ev = tokio::time::timeout(Duration::from_millis(250), rx.recv()).await;
        match ev {
            Ok(Some(NodeEvent::NatTypeChanged { previous, report })) => {
                nat_events.push(serde_json::json!({
                    "previous": previous.map(|m: NatMapping| m.name()),
                    "mapping": report.mapping.name(),
                }));
            }
            Ok(Some(NodeEvent::DirectLinkEstablished { .. })) => {
                outcome = serde_json::json!({"established": true});
                break;
            }
            Ok(Some(NodeEvent::DirectLinkFailed { failure, .. })) => {
                outcome =
                    serde_json::json!({"established": false, "failure": format!("{failure:?}")});
                break;
            }
            Ok(None) => break,
            _ => {}
        }
    }
    let outcomes = node.direct_link_outcomes();
    println!(
        "{}",
        serde_json::json!({
            "side": side,
            "nat": nat_json(node.nat_report().as_ref()),
            "nat_events": nat_events,
            "outcome": outcome,
            "after_ms": started.elapsed().as_millis() as u64,
            "counts": {
                "established": outcomes.established,
                "punch_failed": outcomes.punch_failed,
                "timed_out": outcomes.timed_out,
                "refused": outcomes.refused,
                "probe_failed": outcomes.probe_failed,
            },
            "records": outcomes.recent.iter().map(|r| serde_json::json!({
                "outcome": r.outcome.name(),
                "nat": r.nat.map(|m| m.name()),
                "proposed": r.proposed,
            })).collect::<Vec<_>>(),
        })
    );
    // A short linger, so the peer's last signals and acks are answered.
    tokio::time::sleep(Duration::from_secs(1)).await;
    node.stop().await?;
    Ok(())
}
