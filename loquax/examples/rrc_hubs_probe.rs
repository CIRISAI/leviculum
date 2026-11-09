//! The public rooms' hubs, measured (1577, #232): which of the published
//! `rrc.hub` destinations answer, through the shipped entry servers.
//!
//! A node of its own (no transport, a fresh identity) dials each
//! `--server` as a TCP client, listens for `rrc.hub` announces for
//! `--listen` seconds, then opens every hub of `--hubs` (the shipped list,
//! `loquax/assets/hubs.toml`, and its `[[unreached]]` entries) and every
//! hub it heard besides with Loquax's own hub link
//! ([`loquax::rooms_link`]): path, link, Identify, HELLO until WELCOME,
//! a few seconds for the hub's notices, then it leaves. It joins no room
//! and says nothing.
//!
//! One line a hub on stdout, stable keys, for `just rrc-hubs-check`:
//!
//! ```text
//! HUB hash=<32 hex> listed=<name|-> announce=<yes|no> hops=<n|-> welcome=<yes|no|why> name=<hub's|-> notice=<first line|->
//! ```
//!
//! and a `HEARD` line for each `rrc.hub` announce, with its app data's
//! hub name. `--open no` listens only, `--open listed` opens the list's
//! hubs alone. Each hub's notices go whole to
//! stderr as `NOTICE hash=<hex> <line>`, where a hub names its rooms.

use std::collections::BTreeMap;
use std::net::ToSocketAddrs;
use std::sync::Arc;
use std::time::Duration;

use leviculum_rrc::Value;
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::{Destination, DestinationHash, NodeEvent};
use loquax::engine::Lent;
use loquax::rooms_link::{self, Ask, Options, Told};
use tokio::sync::{mpsc, Notify};

/// How long a hub may take from Open to WELCOME or a refusal: the link's
/// own budgets (path 30 s, link 30 s, five HELLOs 5 s apart) and a margin.
const OPEN_BUDGET: Duration = Duration::from_secs(100);
/// How long the hub's notices after WELCOME are collected.
const NOTICE_WINDOW: Duration = Duration::from_secs(6);

struct Args {
    servers: Vec<String>,
    listen: Duration,
    hubs: Option<String>,
    /// `yes`: the list and every hub heard; `listed`: the list alone;
    /// `no`: listen only.
    open: String,
}

fn args() -> Result<Args, String> {
    let mut servers = Vec::new();
    let mut listen = Duration::from_secs(1800);
    let mut hubs = None;
    let mut open = "yes".to_string();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let value = it.next().ok_or(format!("{arg} needs a value"))?;
        match arg.as_str() {
            "--server" => servers.push(value),
            "--listen" => {
                listen = Duration::from_secs(value.parse().map_err(|_| "--listen <seconds>")?)
            }
            "--hubs" => hubs = Some(value),
            "--open" => open = value,
            _ => return Err(format!("unknown argument {arg}")),
        }
    }
    if servers.is_empty() {
        servers = vec![
            "leviculum.network:4242".to_string(),
            "38.242.217.238:4242".to_string(),
        ];
    }
    Ok(Args {
        servers,
        listen,
        hubs,
        open,
    })
}

/// The hubs a hub list names: `hash = "..."` lines, each with the
/// `name = "..."` line of its table, shipped and unreached alike.
fn listed(text: &str) -> Vec<([u8; 16], String)> {
    let mut out = Vec::new();
    let mut name = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("[[") {
            name.clear();
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        match key.trim() {
            "name" => name = value.to_string(),
            "hash" => {
                if let Some(hash) = hex16(value) {
                    out.push((hash, name.clone()));
                }
            }
            _ => {}
        }
    }
    out
}

fn hex16(text: &str) -> Option<[u8; 16]> {
    let bytes = hex::decode(text).ok()?;
    bytes.try_into().ok()
}

/// One line's value: no spaces, `-` for none.
fn word(text: Option<&str>) -> String {
    match text.map(str::trim).filter(|t| !t.is_empty()) {
        Some(text) => text
            .chars()
            .map(|c| if c.is_whitespace() { '_' } else { c })
            .take(60)
            .collect(),
        None => "-".to_string(),
    }
}

/// The hub's name in its announce: rrcd's `{"proto": "rrc", "v": 1, "hub": name}`.
fn announced_name(app_data: &[u8]) -> Option<String> {
    let Ok(Value::Map(pairs)) = Value::decode(app_data) else {
        return None;
    };
    pairs.iter().find_map(|(k, v)| match (k, v) {
        (Value::Text(k), Value::Text(v)) if k == "hub" => Some(v.clone()),
        _ => None,
    })
}

#[derive(Default)]
struct Heard {
    name: Option<String>,
    hops: Option<u8>,
}

fn main() {
    let args = match args() {
        Ok(args) => args,
        Err(why) => {
            eprintln!("rrc_hubs_probe: {why}");
            std::process::exit(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("rrc_hubs_probe: no runtime: {e}");
            std::process::exit(1);
        }
    };
    if let Err(why) = runtime.block_on(run(args)) {
        eprintln!("rrc_hubs_probe: {why}");
        std::process::exit(1);
    }
}

async fn run(args: Args) -> Result<(), String> {
    let mut wanted = match &args.hubs {
        Some(path) => listed(&std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?),
        None => Vec::new(),
    };
    let storage = tempfile::tempdir().map_err(|e| e.to_string())?;
    let mut builder = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .storage_path(storage.path().to_path_buf());
    for server in &args.servers {
        let addr = server
            .to_socket_addrs()
            .map_err(|e| format!("{server}: {e}"))?
            .next()
            .ok_or(format!("{server}: no address"))?;
        eprintln!("SERVER {server} addr={addr}");
        builder = builder.add_tcp_client(addr);
    }
    let mut node = builder.build_sync().map_err(|e| e.to_string())?;
    let mut events = node.take_event_receiver().ok_or("no event receiver")?;
    node.start().await.map_err(|e| e.to_string())?;
    let node = Arc::new(node);
    let hub_name =
        Destination::compute_name_hash(leviculum_rrc::HUB_APP, &[leviculum_rrc::HUB_ASPECT]);

    // Listen.
    let mut heard: BTreeMap<[u8; 16], Heard> = BTreeMap::new();
    let deadline = tokio::time::Instant::now() + args.listen;
    loop {
        let event = tokio::select! {
            event = events.recv() => event,
            () = tokio::time::sleep_until(deadline) => break,
        };
        let Some(event) = event else {
            return Err("the node stopped".into());
        };
        if let NodeEvent::AnnounceReceived { announce, .. } = event {
            if announce.name_hash() != &hub_name {
                continue;
            }
            let dest = *announce.destination_hash().as_bytes();
            let name = announced_name(announce.app_data());
            let hops = node.hops_to(announce.destination_hash());
            let first = !heard.contains_key(&dest);
            let entry = heard.entry(dest).or_default();
            entry.name = name.or(entry.name.take());
            entry.hops = hops.or(entry.hops);
            if first {
                println!(
                    "HEARD hash={} name={} hops={}",
                    hex::encode(dest),
                    word(entry.name.as_deref()),
                    entry.hops.map_or("-".to_string(), |h| h.to_string())
                );
            }
        }
    }
    if args.open == "no" {
        return Ok(());
    }
    for dest in heard.keys().filter(|_| args.open == "yes") {
        if !wanted.iter().any(|(h, _)| h == dest) {
            wanted.push((*dest, String::new()));
        }
    }

    // Open each hub with Loquax's own link, one at a time.
    let (lend_tx, lends) = mpsc::unbounded_channel();
    let (forward, forwarded) = mpsc::unbounded_channel();
    let (_end, ended) = tokio::sync::oneshot::channel();
    lend_tx
        .send(Lent {
            node: Arc::downgrade(&node),
            events: forwarded,
            ended,
        })
        .map_err(|_| "the lend")?;
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if forward.send(event).is_err() {
                return;
            }
        }
    });
    let mut linking = rooms_link::start(
        &tokio::runtime::Handle::current(),
        Options {
            lends,
            identity: leviculum_std::generate_identity(),
            nick: None,
        },
        Arc::new(Notify::new()),
    );
    for (dest, listed_name) in wanted {
        let _ = linking.asks.send(Ask::Open { hub: dest });
        let mut welcome = "no".to_string();
        let mut name = None;
        let mut notice = None;
        let open_deadline = tokio::time::Instant::now() + OPEN_BUDGET;
        let mut notice_deadline = None;
        loop {
            let until = notice_deadline.unwrap_or(open_deadline);
            let told = tokio::select! {
                told = linking.told.recv() => told,
                () = tokio::time::sleep_until(until) => break,
            };
            match told {
                Some(Told::Welcomed { name: n, .. }) => {
                    welcome = "yes".into();
                    name = n;
                    notice_deadline = Some(tokio::time::Instant::now() + NOTICE_WINDOW);
                }
                Some(Told::HubNotice { text, .. }) => {
                    for line in text.lines() {
                        eprintln!("NOTICE hash={} {line}", hex::encode(dest));
                    }
                    notice.get_or_insert(text);
                }
                Some(Told::Down(down)) => {
                    if welcome != "yes" {
                        welcome = format!("{down:?}");
                    }
                    break;
                }
                Some(_) => {}
                None => return Err("the hub link stopped".into()),
            }
        }
        let _ = linking.asks.send(Ask::Close);
        // Let the close go out and its Down(Left) pass before the next Open.
        tokio::time::sleep(Duration::from_secs(1)).await;
        while linking.told.try_recv().is_ok() {}
        let hash = DestinationHash::new(dest);
        let seen = heard.get(&dest);
        let hops = seen
            .and_then(|h| h.hops)
            .or_else(|| node.hops_to(&hash))
            .map_or("-".to_string(), |h| h.to_string());
        let listed_name = (!listed_name.is_empty()).then_some(listed_name);
        println!(
            "HUB hash={} listed={} announce={} hops={} welcome={} name={} notice={}",
            hex::encode(dest),
            word(listed_name.as_deref()),
            if seen.is_some() { "yes" } else { "no" },
            hops,
            welcome,
            word(name.as_deref().or(seen.and_then(|h| h.name.as_deref()))),
            word(notice.as_deref().and_then(|n| n.lines().next())),
        );
    }
    Ok(())
}
