//! The remote-management client: `lnpnd --status`, `--peers`,
//! `--sync <peer>` and `--break <peer>` (Codeberg #384 part 4,
//! deliverable 1).
//!
//! The exact client flow `lxmd` runs (`query_status` / `_request_sync` /
//! `_request_unpeer`, `reference/LXMF/LXMF/Utilities/lxmd.py:649-680`,
//! `:509-544`, `:580-613`): resolve the remote identity from the
//! propagation destination hash, derive the `lxmf.propagation.control`
//! destination, link to it, identify, request one of the three control
//! paths and read the MessagePack answer back. The output mirrors
//! `get_status`'s (`lxmd.py:682-809`) line for line — the point is that
//! an operator's eyes and scripts see one tool, whichever daemon
//! answers — and the exit codes are the reference's.

use std::path::PathBuf;
use std::time::Duration;

use leviculum_core::{Destination, DestinationHash, Identity};
use leviculum_lxmf::control::{
    ControlNodeStats, ControlPeerStats, ControlResponse, CONTROL_ASPECTS, HOPS_UNKNOWN,
    STATS_GET_PATH, SYNC_REQUEST_PATH, UNPEER_REQUEST_PATH,
};
use leviculum_lxmf::node::APP_NAME;
use leviculum_lxmf::PeerError;
use leviculum_std::driver::ReticulumNodeBuilder;

/// What the client is asked to do.
pub enum ClientAction {
    Status { show_status: bool, show_peers: bool },
    Sync([u8; 16]),
    Unpeer([u8; 16]),
}

pub struct ClientOptions {
    /// Shared-instance name to attach to.
    pub instance: String,
    /// Where this client keeps its node state (learned identities, paths).
    pub storage_dir: PathBuf,
    /// The identity the request identifies as; must be on the remote's
    /// `control_allowed` list (or be the remote's own).
    pub identity: Identity,
    /// The remote node's `lxmf.propagation` destination hash; `None`
    /// queries our own daemon's node (the identity's own control
    /// destination, `get_status` with `remote=None`, `lxmd.py:650`).
    pub remote: Option<[u8; 16]>,
    pub timeout: Duration,
}

/// The shipped budget for `--status` / `--peers`, matching the reference's
/// `get_status(..., timeout=5)` (`reference/LXMF/LXMF/Utilities/lxmd.py:682`).
pub const DEFAULT_STATUS_TIMEOUT: Duration = Duration::from_secs(5);

/// The shipped budget for `--sync` / `--break`. The reference passes no
/// timeout to `_request_sync` / `_request_unpeer` (`lxmd.py:509`, `:580`),
/// whose own default is 10 s.
pub const DEFAULT_ACTION_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the client re-asks for the control path while waiting for it.
///
/// The reference asks exactly once and then polls (`lxmd.py:663-667`); the
/// repeat is ours, for the case Codeberg #44 describes. It must stay well
/// under the budget, and the budget must never be so small that the *first*
/// request is the one that falls outside it — see the
/// `status_resolves_the_control_path_at_the_shipped_default_budget` test.
pub const PATH_RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// The reference's exit codes (`lxmd.py:516-578`, `:689-697`).
const EXIT_TIMEOUT: u8 = 200;
const EXIT_NO_IDENTITY: u8 = 203;
const EXIT_NO_ACCESS: u8 = 204;
const EXIT_INVALID_DATA: u8 = 205;
const EXIT_NOT_FOUND: u8 = 206;
const EXIT_EMPTY: u8 = 207;

pub async fn run(options: ClientOptions, action: ClientAction) -> u8 {
    let timeout_name = match &action {
        ClientAction::Status { .. } => "Getting lnpnd statistics",
        ClientAction::Sync(_) => "Requesting lnpnd peer sync",
        ClientAction::Unpeer(_) => "Requesting lnpnd peering break",
    };
    let fail = |code: u8, message: &str| -> u8 {
        println!("{message}");
        code
    };

    if std::fs::create_dir_all(&options.storage_dir).is_err() {
        return fail(1, "Could not create client storage directory");
    }
    let mut node = match ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(&options.instance)
        .storage_path(options.storage_dir.clone())
        .build()
        .await
    {
        Ok(node) => node,
        Err(error) => {
            eprintln!(
                "lnpnd: could not join the Reticulum shared instance named \
                 '{}'.\n  Start a daemon with `lnsd` (or Python's `rnsd`), or name \
                 another instance with --rnsconfig / --instance.\n  The stack said: {error}",
                options.instance
            );
            return 1;
        }
    };
    if let Err(error) = node.start().await {
        eprintln!("lnpnd: node start: {error}");
        return 1;
    }
    let code = query(&node, &options, &action, timeout_name).await;
    let _ = node.stop().await;
    code
}

/// Resolve a path to the control destination, exactly as every client
/// action resolves it.
///
/// Public because it is what the shipped defaults have to survive, and a
/// test that re-types the budget instead of calling this proves nothing
/// about the binary an operator runs.
pub async fn resolve_control_path(
    node: &leviculum_std::ReticulumNode,
    control_hash: &DestinationHash,
    timeout: Duration,
) -> bool {
    if node.has_path(control_hash) {
        return true;
    }
    node.wait_for_path(control_hash, timeout, PATH_RETRY_INTERVAL)
        .await
        .unwrap_or(false)
}

async fn query(
    node: &leviculum_std::ReticulumNode,
    options: &ClientOptions,
    action: &ClientAction,
    timeout_name: &str,
) -> u8 {
    let fail = |code: u8, message: &str| -> u8 {
        println!("{message}");
        code
    };
    let timeout_exit = format!("{timeout_name} timed out, exiting now");

    // The remote identity: ours for the local daemon, recalled from the
    // propagation announce otherwise (`_get_target_identity`,
    // `reference/LXMF/LXMF/Utilities/lxmd.py:811-838`).
    let remote_identity = match options.remote {
        None => options.identity.clone(),
        Some(remote) => {
            let hash = DestinationHash::new(remote);
            let deadline = tokio::time::Instant::now() + options.timeout;
            let mut requested = false;
            loop {
                if let Some(identity) = node.get_identity(&hash) {
                    break identity;
                }
                if tokio::time::Instant::now() >= deadline {
                    return fail(
                        EXIT_TIMEOUT,
                        "Resolving remote identity timed out, exiting now",
                    );
                }
                if !requested {
                    requested = true;
                    let _ = node.request_path(&hash).await;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };

    // The control destination, derived exactly as the client side of the
    // reference derives it (`lxmd.py:651`: `RNS.Destination(remote_identity,
    // OUT, SINGLE, APP_NAME, "propagation", "control")`).
    let name_hash = Destination::compute_name_hash(APP_NAME, &CONTROL_ASPECTS);
    let control_hash = Destination::compute_destination_hash(&name_hash, remote_identity.hash());

    if !resolve_control_path(node, &control_hash, options.timeout).await {
        return fail(EXIT_TIMEOUT, &timeout_exit);
    }

    let signing_key = remote_identity.ed25519_verifying().to_bytes();
    let (link, established) = match node.connect_awaited(&control_hash, &signing_key).await {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("lnpnd: link setup failed: {error:?}");
            return 1;
        }
    };
    if tokio::time::timeout(options.timeout, established)
        .await
        .map(|outcome| outcome.is_err())
        .unwrap_or(true)
    {
        return fail(EXIT_TIMEOUT, &timeout_exit);
    }
    if let Err(error) = node.identify_link(link.link_id(), &options.identity).await {
        eprintln!("lnpnd: identify failed: {error:?}");
        return 1;
    }

    // The request data rides as one msgpack value; the peer hash packs
    // as a 16-byte bin, what `umsgpack.packb(destination_hash)` sends.
    let packed_hash = |peer: &[u8; 16]| {
        let mut out = Vec::new();
        leviculum_lxmf::msgpack::bin(&mut out, peer);
        out
    };
    let (path, data): (&str, Option<Vec<u8>>) = match action {
        ClientAction::Status { .. } => (STATS_GET_PATH, None),
        ClientAction::Sync(peer) => (SYNC_REQUEST_PATH, Some(packed_hash(peer))),
        ClientAction::Unpeer(peer) => (UNPEER_REQUEST_PATH, Some(packed_hash(peer))),
    };
    let timeout_ms = options.timeout.as_millis() as u64;
    let response = match node
        .send_request_awaited(link.link_id(), path, data.as_deref(), Some(timeout_ms))
        .await
    {
        Ok((_, future)) => match tokio::time::timeout(options.timeout, future).await {
            Ok(Ok(info)) => info.response_data,
            // The link stood and the identify went through, so the remote
            // heard us and chose not to answer. On the wire that is
            // indistinguishable from a node that died mid-request, and
            // the client cannot tell the two apart — a refusal behind
            // `RNS.Destination.ALLOW_LIST` is sent as silence
            // (`reference/Reticulum/RNS/Link.py:867-874`). lnpnd answers
            // its refusals (`lnpnd/src/engine.rs`,
            // `register_control_handlers`); lxmd and lnpnd before 0.2.0 do
            // not, so name the possibility instead of letting "timed out"
            // send the reader to the mesh. stdout keeps lxmd's exact line
            // for scripts; the hint goes to stderr.
            _ => {
                eprintln!(
                    "lnpnd: the link was established and identified, so the node heard                      the request.
  A node that refuses a query it is not configured to                      answer sends nothing back, which looks exactly like this.
  Check                      that this identity ({}) is the node's own or is listed under                      control_allowed in its config.",
                    prettyhexrep(options.identity.hash())
                );
                return fail(EXIT_TIMEOUT, &timeout_exit);
            }
        },
        Err(error) => {
            eprintln!("lnpnd: request failed: {error:?}");
            return 1;
        }
    };
    let _ = node.close_link(link.link_id()).await;

    let decoded = match ControlResponse::decode(&response) {
        Ok(decoded) => decoded,
        Err(_) => return fail(EXIT_EMPTY, "Empty response received"),
    };
    // The reference's error dispositions (`get_status` / `request_sync` /
    // `request_unpeer`, `lxmd.py:561-578`, `:630-647`, `:689-697`).
    match (&decoded, action) {
        (ControlResponse::Error(PeerError::NoIdentity), _) => {
            fail(EXIT_NO_IDENTITY, "Remote received no identity")
        }
        (ControlResponse::Error(PeerError::NoAccess), _) => fail(EXIT_NO_ACCESS, "Access denied"),
        (ControlResponse::Error(PeerError::InvalidData), _) => {
            fail(EXIT_INVALID_DATA, "Invalid data received by remote")
        }
        (ControlResponse::Error(PeerError::NotFound), _) => {
            fail(EXIT_NOT_FOUND, "The requested peer was not found")
        }
        (ControlResponse::Error(_), _) | (ControlResponse::Nil, _) => {
            fail(EXIT_EMPTY, "Empty response received")
        }
        (ControlResponse::Ack, ClientAction::Sync(peer)) => {
            println!("Sync requested for peer {}", prettyhexrep(peer));
            0
        }
        (ControlResponse::Ack, ClientAction::Unpeer(peer)) => {
            println!("Broke peering with {}", prettyhexrep(peer));
            0
        }
        (ControlResponse::Ack, ClientAction::Status { .. }) => {
            fail(EXIT_EMPTY, "Empty response received")
        }
        (
            ControlResponse::Stats(stats),
            ClientAction::Status {
                show_status,
                show_peers,
            },
        ) => {
            print_status(stats, *show_status, *show_peers);
            0
        }
        (ControlResponse::Stats(_), _) => fail(EXIT_EMPTY, "Empty response received"),
    }
}

/// `get_status`'s output (`reference/LXMF/LXMF/Utilities/lxmd.py:698-809`),
/// line for line over our decoded stats.
fn print_status(stats: &ControlNodeStats, show_status: bool, show_peers: bool) {
    let ms_util = match stats.messagestore_limit_bytes {
        Some(limit) if limit > 0 => format!(
            "{}%",
            pyfloat(round2(
                stats.messagestore_bytes as f64 / limit as f64 * 100.0
            ))
        ),
        _ => "unknown".to_string(),
    };
    let who_str = if stats.from_static_only {
        "static peers only"
    } else {
        "all nodes"
    };

    let mut available_peers = 0u64;
    let mut unreachable_peers = 0u64;
    let mut peered_incoming = 0u64;
    let mut peered_outgoing = 0u64;
    let mut peered_rx_bytes = 0u64;
    let mut peered_tx_bytes = 0u64;
    for peer in &stats.peers {
        peered_incoming += peer.incoming;
        peered_outgoing += peer.outgoing;
        peered_rx_bytes += peer.rx_bytes;
        peered_tx_bytes += peer.tx_bytes;
        if peer.alive {
            available_peers += 1;
        } else {
            unreachable_peers += 1;
        }
    }
    let total_incoming = peered_incoming
        + stats.unpeered_propagation_incoming
        + stats.client_propagation_messages_received;
    let total_rx_bytes = peered_rx_bytes + stats.unpeered_propagation_rx_bytes;
    // Python's `df` is `round(...)` (a float) or the int 0
    // (`lxmd.py:726-727`), and the f-string prints them differently.
    let df = if total_incoming != 0 {
        pyfloat(round2(peered_outgoing as f64 / total_incoming as f64))
    } else {
        "0".to_string()
    };

    let dhs = prettyhexrep(&stats.destination_hash);
    let uts = prettytime(stats.uptime_secs);
    println!();
    println!("LXMF Propagation Node running on {dhs}, uptime is {uts}");

    if show_status {
        let msb = prettysize(stats.messagestore_bytes as f64);
        let msl = match stats.messagestore_limit_bytes {
            Some(limit) => prettysize(limit as f64),
            None => "unlimited".to_string(),
        };
        let ptl = prettysize(stats.propagation_limit_kb.unwrap_or(0) as f64 * 1000.0);
        let psl = prettysize(stats.sync_limit_kb.unwrap_or(0) as f64 * 1000.0);
        let uprx = prettysize(stats.unpeered_propagation_rx_bytes as f64);
        let mscnt = stats.messagestore_count;
        let stp = stats.total_peers;
        let smp = stats
            .max_peers
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unlimited".to_string());
        let sdp = stats.discovered_peers;
        let ssp = stats.static_peers;
        let cprr = stats.client_propagation_messages_received;
        let cprs = stats.client_propagation_messages_served;
        let upi = stats.unpeered_propagation_incoming;
        let psc = stats.target_stamp_cost;
        let scf = stats.stamp_cost_flexibility;
        let pc = stats.peering_cost;
        let pcm = stats.max_peering_cost;
        println!("Messagestore contains {mscnt} messages, {msb} ({ms_util} utilised of {msl})");
        println!("Required propagation stamp cost is {psc}, flexibility is {scf}");
        println!("Peering cost is {pc}, max remote peering cost is {pcm}");
        println!("Accepting propagated messages from {who_str}");
        println!("{ptl} message limit, {psl} sync limit");
        println!();
        println!("Peers   : {stp} total (peer limit is {smp})");
        println!("          {sdp} discovered, {ssp} static");
        println!("          {available_peers} available, {unreachable_peers} unreachable");
        println!();
        println!(
            "Traffic : {total_incoming} messages received in total ({})",
            prettysize(total_rx_bytes as f64)
        );
        println!(
            "          {peered_incoming} messages received from peered nodes ({})",
            prettysize(peered_rx_bytes as f64)
        );
        println!("          {upi} messages received from unpeered nodes ({uprx})");
        println!(
            "          {peered_outgoing} messages transferred to peered nodes ({})",
            prettysize(peered_tx_bytes as f64)
        );
        println!("          {cprr} propagation messages received directly from clients");
        println!("          {cprs} propagation messages served to clients");
        println!("          Distribution factor is {df}");
        println!();
    }

    if show_peers {
        if !show_status {
            println!();
        }
        for peer in &stats.peers {
            print_peer(peer);
        }
    }
}

fn print_peer(peer: &ControlPeerStats) {
    let ind = "  ";
    let t = if peer.is_static {
        "Static peer     "
    } else {
        "Discovered peer "
    };
    let a = if peer.alive {
        "Available"
    } else {
        "Unreachable"
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let heard_ago = now.saturating_sub(peer.last_heard);
    let hops = peer.network_distance;
    let hs = if hops == HOPS_UNKNOWN {
        "hops unknown".to_string()
    } else if hops == 1 {
        "1 hop away".to_string()
    } else {
        format!("{hops} hops away")
    };
    let display = |value: Option<u64>| {
        value
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    };
    let pk = match peer.peering_key_value {
        None => "Not generated".to_string(),
        Some(value) => format!("Generated, value is {value}"),
    };
    let ls = if peer.last_sync_attempt != 0 {
        format!(
            "last synced {} ago",
            prettytime(now.saturating_sub(peer.last_sync_attempt) as f64)
        )
    } else {
        "never synced".to_string()
    };
    let sstr = prettyspeed(peer.str_rate as f64);
    let sler = prettyspeed(peer.ler as f64);
    let stl = match peer.transfer_limit_kb {
        Some(limit) if limit > 0 => prettysize(limit as f64 * 1000.0),
        _ => "Unknown".to_string(),
    };
    let ssl = match peer.sync_limit_kb {
        Some(limit) if limit > 0 => prettysize(limit as f64 * 1000.0),
        _ => "unknown".to_string(),
    };
    let srxb = prettysize(peer.rx_bytes as f64);
    let stxb = prettysize(peer.tx_bytes as f64);
    let ar = pyfloat(round2(peer.acceptance_rate * 100.0));
    println!("{ind}{t}{}", prettyhexrep(&peer.peer_id));
    if let Some(name) = peer.name.as_deref().filter(|name| !name.is_empty()) {
        let mut shown: String = name.chars().take(45).collect();
        if name.chars().count() > 45 {
            shown.push_str("...");
        }
        println!("{ind}{ind}Name       : {shown}");
    }
    println!(
        "{ind}{ind}Status     : {a}, {hs}, last heard {} ago",
        prettytime(heard_ago as f64)
    );
    println!(
        "{ind}{ind}Costs      : Propagation {} (flex {}), peering {}",
        display(peer.target_stamp_cost),
        display(peer.stamp_cost_flexibility),
        display(peer.peering_cost)
    );
    println!("{ind}{ind}Sync key   : {pk}");
    println!("{ind}{ind}Speeds     : {sstr} STR, {sler} LER");
    println!("{ind}{ind}Limits     : {stl} message limit, {ssl} sync limit");
    println!(
        "{ind}{ind}Messages   : {} offered, {} outgoing, {} incoming, {ar}% acceptance rate",
        peer.offered, peer.outgoing, peer.incoming
    );
    println!("{ind}{ind}Traffic    : {srxb} received, {stxb} sent");
    let ms = if peer.unhandled == 1 { "" } else { "s" };
    println!(
        "{ind}{ind}Sync state : {} unhandled message{ms}, {ls}",
        peer.unhandled
    );
    println!();
}

/// `RNS.prettyhexrep` (`reference/Reticulum/RNS/__init__.py:183-186`).
pub fn prettyhexrep(data: &[u8]) -> String {
    let hex: String = data.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("<{hex}>")
}

/// Python's `round(x, 2)`.
fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Python's `str()` of a float: a whole number keeps one decimal
/// (`str(1.0) == "1.0"`), everything else prints shortest (`"42.5"`,
/// `"1.25"`). The status lines embed rounded floats this way.
fn pyfloat(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

/// `RNS.prettysize` (`reference/Reticulum/RNS/__init__.py:191-206`).
pub fn prettysize(num: f64) -> String {
    let units = ["", "K", "M", "G", "T", "P", "E", "Z"];
    let mut num = num;
    for unit in units {
        if num.abs() < 1000.0 {
            if unit.is_empty() {
                return format!("{num:.0} B");
            }
            return format!("{num:.2} {unit}B");
        }
        num /= 1000.0;
    }
    format!("{num:.2}YB")
}

/// `RNS.prettyspeed` (`reference/Reticulum/RNS/__init__.py:188-189`):
/// `prettysize(num/8, suffix="b") + "ps"` — the byte value re-scaled to
/// bits inside `prettysize`, i.e. the number itself in bits.
pub fn prettyspeed(bits_per_sec: f64) -> String {
    let units = ["", "K", "M", "G", "T", "P", "E", "Z"];
    let mut num = bits_per_sec;
    for unit in units {
        if num.abs() < 1000.0 {
            if unit.is_empty() {
                return format!("{num:.0} bps");
            }
            return format!("{num:.2} {unit}bps");
        }
        num /= 1000.0;
    }
    format!("{num:.2}Ybps")
}

/// `RNS.prettytime` with the defaults `lxmd` calls it with
/// (`reference/Reticulum/RNS/__init__.py:239-290`, `verbose=False`,
/// `compact=False`).
pub fn prettytime(seconds: f64) -> String {
    let neg = seconds < 0.0;
    let mut time = seconds.abs();
    let days = (time / (24.0 * 3600.0)).floor() as u64;
    time %= 24.0 * 3600.0;
    let hours = (time / 3600.0).floor() as u64;
    time %= 3600.0;
    let minutes = (time / 60.0).floor() as u64;
    time %= 60.0;
    let secs = round2(time);

    let mut components: Vec<String> = Vec::new();
    if days > 0 {
        components.push(format!("{days}d"));
    }
    if hours > 0 {
        components.push(format!("{hours}h"));
    }
    if minutes > 0 {
        components.push(format!("{minutes}m"));
    }
    if secs > 0.0 {
        components.push(format!("{}s", pyfloat(secs)));
    }
    let mut tstr = String::new();
    let total = components.len();
    for (index, component) in components.iter().enumerate() {
        if index > 0 {
            if index + 1 < total {
                tstr.push_str(", ");
            } else {
                tstr.push_str(" and ");
            }
        }
        tstr.push_str(component);
    }
    if tstr.is_empty() {
        return "0s".to_string();
    }
    if neg {
        format!("-{tstr}")
    } else {
        tstr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The formatting helpers must match the reference's Python
    /// implementations on representative values, because the status
    /// output is compared across daemons by eye and by script.
    #[test]
    fn pretty_helpers_match_python() {
        // RNS.prettysize: "%.0f B" below 1000, "%.2f KB" above.
        assert_eq!(prettysize(0.0), "0 B");
        assert_eq!(prettysize(999.0), "999 B");
        assert_eq!(prettysize(1000.0), "1.00 KB");
        assert_eq!(prettysize(4096.0), "4.10 KB");
        assert_eq!(prettysize(500_000_000.0), "500.00 MB");
        // RNS.prettyspeed(0) == "0 bps".
        assert_eq!(prettyspeed(0.0), "0 bps");
        assert_eq!(prettyspeed(40_000.0), "40.00 Kbps");
        // RNS.prettytime(42.5) == "42.5s"; 3661.0 == "1h, 1m and 1s".
        assert_eq!(prettytime(42.5), "42.5s");
        assert_eq!(prettytime(3661.0), "1h, 1m and 1.0s");
        assert_eq!(prettytime(61.0), "1m and 1.0s");
        assert_eq!(prettytime(90061.25), "1d, 1h, 1m and 1.25s");
        assert_eq!(prettytime(0.0), "0s");
        assert_eq!(prettytime(86400.0), "1d");
        // RNS.prettyhexrep.
        assert_eq!(prettyhexrep(&[0xab, 0x01]), "<ab01>");
    }
}
