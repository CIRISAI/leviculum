//! The M9 as a client of its home, on the host (slice five, host half,
//! 1363; `docs/src/concepts/handheld-messenger.md`, "Slice five, host
//! half"): the `no_std` client (`loquax::home::ask`) performed by the
//! host's executor (`client::run_ask`) against a real mailbox on its own
//! node, through a transport node that carries the device link, the way
//! the board reaches its home over the mesh. The pairing starts at the
//! keyboard: the controller's answers through the board's measured read
//! and its driver to key events, the typed code, the join, `HOME admitted`.
//!
//! Between the board's node and the transport node sits a proxy that
//! reads the HDLC frames of the TCP link both ways and keeps each
//! Reticulum packet's length and kind: what the board would put on the
//! air, packet by packet, since a LoRa interface sends each packet as one
//! or two frames of its own (`packet_airtime_ms`). The byte and airtime
//! tables of the page are what this test prints.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use leviculum_core::framing::hdlc::{DeframeResult, Deframer};
use leviculum_core::packet::{Packet, PacketContext, PacketType};
use leviculum_core::rnode::{derive_preamble_symbols, packet_airtime_ms};
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use leviculum_ui::input::KeyEvent;
use loquax::home::ask::{Ask, GroupVerb, Outcome, Verb, LORA_PAGE_BYTES};
use loquax::home::client;
use loquax::home::code::{HomeCode, Typing};
use loquax::home::watch::{self, WatchEvent};
use loquax::home::wire;
use loquax::outbox::OutboxEvent;
use loquax::ui::{update, Pane, UiEffect, UiEvent, UiModel};
use loquax_m9::field::Field;
use loquax_m9::group;
use loquax_m9::kbraw::{Read, MEASURED};
use loquax_m9::keyboard::Key;
use loquax_m9::keys::{event, M9Key};
use loquax_m9::pair::Pairing;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "../../loquax/tests/support/reach.rs"]
mod reach;

use reach::{entry, logs, next_watch, Mailbox, Relay};

/// The packet MTU on a LoRa link, `leviculum_core::constants::MTU`.
const MTU: usize = leviculum_core::constants::MTU;

/// One packet the proxy saw.
#[derive(Clone, Debug)]
struct Seen {
    /// From the board towards its home.
    up: bool,
    len: usize,
    kind: PacketType,
    context: PacketContext,
}

impl Seen {
    fn label(&self) -> String {
        format!(
            "{} {:?}/{:?} {}",
            if self.up { "up" } else { "down" },
            self.kind,
            self.context,
            self.len
        )
    }
}

/// The proxy between the board's node and the transport node.
struct Proxy {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Proxy {
    async fn start(upstream: SocketAddr) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the proxy's port");
        let addr = listener.local_addr().expect("addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let kept = seen.clone();
        tokio::spawn(async move {
            while let Ok((board, _)) = listener.accept().await {
                let Ok(relay) = tokio::net::TcpStream::connect(upstream).await else {
                    continue;
                };
                let (board_read, board_write) = board.into_split();
                let (relay_read, relay_write) = relay.into_split();
                tokio::spawn(pump(board_read, relay_write, true, kept.clone()));
                tokio::spawn(pump(relay_read, board_write, false, kept.clone()));
            }
        });
        Self { addr, seen }
    }

    fn mark(&self) -> usize {
        self.seen.lock().expect("seen").len()
    }

    fn since(&self, mark: usize) -> Vec<Seen> {
        self.seen.lock().expect("seen")[mark..].to_vec()
    }
}

async fn pump(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    up: bool,
    seen: Arc<Mutex<Vec<Seen>>>,
) {
    let mut deframer = Deframer::with_max_frame(1 << 20);
    let mut buffer = [0u8; 4096];
    loop {
        let n = match from.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        for result in deframer.process(&buffer[..n]) {
            if let DeframeResult::Frame(frame) = result {
                if let Ok(packet) = Packet::unpack(&frame) {
                    seen.lock().expect("seen").push(Seen {
                        up,
                        len: frame.len(),
                        kind: packet.flags.packet_type,
                        context: packet.context,
                    });
                }
            }
        }
        if to.write_all(&buffer[..n]).await.is_err() {
            return;
        }
    }
}

/// Airtime of one packet at our LoRa defaults (BW 125 kHz, SF8, CR 4/5,
/// the derived preamble), as `announce-cadence.md` reckons it.
fn ms(len: usize) -> u64 {
    packet_airtime_ms(len, 125_000, 8, 5, derive_preamble_symbols(8, 5, 125_000))
}

/// One phase of the walk: its packets, their bytes and airtime.
struct Phase {
    name: &'static str,
    packets: Vec<Seen>,
}

/// What a phase saw, sorted: the ask's own packets, the wakes the
/// watching link carried meanwhile (a channel message down and its proof
/// up), and the announces, which are the nodes' and no client's cost.
fn sorted(seen: Vec<Seen>) -> (Vec<Seen>, Vec<Seen>, Vec<Seen>) {
    let (mut own, mut wakes, mut announces) = (Vec::new(), Vec::new(), Vec::new());
    for packet in seen {
        match (packet.up, packet.kind, packet.context) {
            (_, PacketType::Announce, _) => announces.push(packet),
            (false, PacketType::Data, PacketContext::Channel)
            | (true, PacketType::Proof, PacketContext::None) => wakes.push(packet),
            _ => own.push(packet),
        }
    }
    (own, wakes, announces)
}

impl Phase {
    fn bytes(&self, up: bool) -> usize {
        self.packets
            .iter()
            .filter(|p| p.up == up)
            .map(|p| p.len)
            .sum()
    }

    fn airtime(&self, up: Option<bool>) -> u64 {
        self.packets
            .iter()
            .filter(|p| up.is_none_or(|up| p.up == up))
            .map(|p| ms(p.len))
            .sum()
    }

    fn row(&self) -> String {
        format!(
            "| {} | {} | {} | {} | {} | {} | {} |",
            self.name,
            self.packets.len(),
            self.bytes(true),
            self.bytes(false),
            self.airtime(Some(true)),
            self.airtime(Some(false)),
            self.packets
                .iter()
                .map(Seen::label)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// The board's node: a plain node with one TCP client, to the proxy.
async fn board_node(proxy: SocketAddr, dir: &std::path::Path) -> Arc<ReticulumNode> {
    let mut node = ReticulumNodeBuilder::new()
        .add_tcp_client(proxy)
        .storage_path(dir.to_path_buf())
        .build()
        .await
        .expect("the board's node");
    node.start().await.expect("start the board's node");
    Arc::new(node)
}

/// The controller's answers for pressing the key of `code` once: the
/// code in all eight bytes, then an idle read, as the bench capture of
/// 2026-11-04 read them (`loquax-m9/tests/keyboard/capture-20261104.txt`).
fn tap(answers: &mut Vec<[u8; 8]>, code: u8) {
    answers.push([code; 8]);
    answers.push([0; 8]);
}

/// The answers that type `text`: the controller sends a character as its
/// ASCII code (the capture read letters and `0`; the digits 2 to 7 of a
/// code are taken to follow `0`, which the reviewer's walk confirms).
fn typing_answers(text: &str) -> Vec<[u8; 8]> {
    let mut answers = Vec::new();
    for c in text.chars() {
        tap(&mut answers, u8::try_from(c).expect("an ASCII character"));
    }
    answers
}

/// The board's read and driver over `answers`.
fn keys(answers: &[[u8; 8]]) -> Vec<KeyEvent> {
    answers
        .iter()
        .filter_map(|answer| match MEASURED.read(answer) {
            Read::Code(code) => Key::of(code).and_then(Key::event),
            Read::Idle | Read::Mixed => None,
        })
        .collect()
}

/// The pairing walk: the code typed on the board's keys with one stray
/// character and its Backspace, then Enter. What the board joins with.
fn typed_code(code: &HomeCode) -> HomeCode {
    let mut pairing = Pairing::new();
    let text = code.encode();
    let feed = |pairing: &mut Pairing, answers: Vec<[u8; 8]>| {
        keys(&answers)
            .iter()
            .filter_map(|event| pairing.key(event))
            .last()
    };
    assert_eq!(feed(&mut pairing, typing_answers(&text[..20])), None);
    assert_eq!(pairing.typing(), Typing::Short { typed: 20 });
    // '9' is no base32 character, named where it stands.
    feed(&mut pairing, typing_answers("9"));
    assert_eq!(pairing.typing(), Typing::Stray { at: 20 });
    let mut answers = Vec::new();
    tap(&mut answers, 0x08);
    feed(&mut pairing, answers);
    feed(&mut pairing, typing_answers(&text[20..]));
    println!("pairing screen: {:?}", pairing.lines());
    let mut answers = Vec::new();
    tap(&mut answers, 0x0d);
    feed(&mut pairing, answers).expect("Enter on the complete code joins")
}

async fn ask(
    node: &ReticulumNode,
    device: &leviculum_core::Identity,
    home: &[u8; 64],
    verb: Verb,
) -> Outcome {
    let ask = Ask::new(home, verb, 0, 30_000).expect("ask");
    client::run_ask(node, device, ask, |_| {})
        .await
        .unwrap_or_else(|error| panic!("ask: {error}"))
}

fn submission(content: &[u8]) -> wire::Submission {
    wire::Submission {
        client_id: client::client_id(),
        destination: [0x5a; 16],
        title: Vec::new(),
        content: content.to_vec(),
        fields: Vec::new(),
    }
}

/// Let the link closes and proofs of a finished ask cross the proxy.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(400)).await;
}

/// The whole walk over one transport node: pairing from the keys to
/// `HOME admitted`, a watching link, a send and the wake it brings, a pull
/// of that one message in LoRa pages, a mark; then the page bound against
/// its control. Prints the byte table and the airtime table.
#[tokio::test(flavor = "multi_thread")]
async fn the_board_pairs_by_its_keys_and_pulls_sends_and_marks_in_lora_pages() {
    logs();
    let relay = Relay::start(&[]).await;
    let mailbox = Mailbox::start(vec![entry(relay.peer)]).await;
    let proxy = Proxy::start(relay.peer).await;
    let dir = tempfile::tempdir().expect("board dir");
    std::fs::create_dir_all(dir.path().join("storage")).expect("storage");
    let node = board_node(proxy.addr, &dir.path().join("storage")).await;
    let device = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    let home = mailbox.public_key;
    let mut phases = Vec::new();

    // Pairing: the code the phone shows, typed on the board's keys.
    let shown = HomeCode {
        public_key: home,
        token: mailbox.token,
    };
    let code = typed_code(&shown);
    assert_eq!(code, shown);
    let mark = proxy.mark();
    let join = Pairing::join(&code, 0, 30_000).expect("join");
    let joined = client::run_ask(&node, &device, join, |_| {}).await;
    assert_eq!(joined, Ok(Outcome::Joined));
    settle().await;
    let (own, _, announces) = sorted(proxy.since(mark));
    println!(
        "announces meanwhile, not counted: {:?}",
        announces.iter().map(Seen::label).collect::<Vec<_>>()
    );
    phases.push(Phase {
        name: "join (path request, link, identify, /join, close)",
        packets: own,
    });
    let address = loquax::home::lxmf_address(&home).expect("address");
    let devices = loquax::store::Store::open_in(mailbox.dir.path(), address)
        .expect("the mailbox's store")
        .devices()
        .expect("devices");
    let admitted = devices
        .iter()
        .find(|d| d.device == *device.hash())
        .expect("HOME admitted: the board is a device of the home");
    // The home keeps the name its owner gave the code (the harness issues
    // it as "phone"), not the one the board brings (`DEVICE_NAME`).
    assert_eq!(admitted.name, "phone");

    // The watching link the idle board keeps.
    let mark = proxy.mark();
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let watching = tokio::spawn(watch::run(
        watch::Watch {
            node: node.clone(),
            public_key: home,
            device: device.clone(),
            home: dir.path().to_path_buf(),
        },
        tx,
    ));
    next_watch(&mut events, 30, |e| *e == WatchEvent::Watching(true)).await;
    let WatchEvent::Wake(before) =
        next_watch(&mut events, 5, |e| matches!(e, WatchEvent::Wake(_))).await
    else {
        unreachable!()
    };
    settle().await;
    phases.push(Phase {
        name: "watch (link, identify, /watch, kept)",
        packets: sorted(proxy.since(mark)).0,
    });

    // One message out.
    let mark = proxy.mark();
    let Outcome::Sent(id) = ask(
        &node,
        &device,
        &home,
        Verb::Send(submission(b"Gruss vom M9")),
    )
    .await
    else {
        panic!("not sent");
    };
    next_watch(
        &mut events,
        30,
        |e| matches!(e, WatchEvent::Wake(n) if *n > before),
    )
    .await;
    settle().await;
    // The wake the send caused crosses while the send's link closes.
    let (own, wakes, _) = sorted(proxy.since(mark));
    phases.push(Phase {
        name: "send 12 characters (link, identify, /send, close)",
        packets: own,
    });
    assert_eq!(wakes.len(), 2, "one wake and its proof: {wakes:?}");
    phases.push(Phase {
        name: "wake on the watching link (channel message, proof)",
        packets: wakes,
    });

    // The one message pulled, in LoRa pages.
    let mark = proxy.mark();
    let page_bytes = Some(LORA_PAGE_BYTES as u64);
    let pulled = ask(
        &node,
        &device,
        &home,
        Verb::Pull {
            cursor: before,
            page_bytes,
        },
    )
    .await;
    settle().await;
    let Outcome::Pulled {
        cursor, entries, ..
    } = pulled
    else {
        panic!("not pulled");
    };
    assert_eq!(entries, 1, "the sent message is the one entry");
    phases.push(Phase {
        name: "pull one message (link, identify, /sync, close)",
        packets: sorted(proxy.since(mark)).0,
    });

    let mark = proxy.mark();
    let marked = ask(
        &node,
        &device,
        &home,
        Verb::Mark(wire::Mark {
            flag: wire::MarkFlag::Read,
            ids: vec![id],
        }),
    )
    .await;
    assert_eq!(marked, Outcome::Marked);
    settle().await;
    phases.push(Phase {
        name: "mark one read (link, identify, /mark, close)",
        packets: sorted(proxy.since(mark)).0,
    });

    // The bound against its control: four more messages, pulled in LoRa
    // pages and then in the home's own page.
    for n in 0..4 {
        let text = format!("Nachricht {n} an den Briefkasten, etwas laenger als ein Gruss");
        ask(
            &node,
            &device,
            &home,
            Verb::Send(submission(text.as_bytes())),
        )
        .await;
    }
    settle().await;
    let mark = proxy.mark();
    let mut pages = Vec::new();
    let ask_paged = Ask::new(&home, Verb::Pull { cursor, page_bytes }, 0, 30_000).expect("ask");
    let paged = client::run_ask(&node, &device, ask_paged, |page| {
        pages.push(page.entries.len())
    })
    .await
    .expect("paged pull");
    settle().await;
    let paged_seen = proxy.since(mark);
    let mark = proxy.mark();
    let whole = ask(
        &node,
        &device,
        &home,
        Verb::Pull {
            cursor,
            page_bytes: None,
        },
    )
    .await;
    settle().await;
    let whole_seen = proxy.since(mark);
    let responses = |seen: &[Seen]| -> Vec<usize> {
        seen.iter()
            .filter(|p| !p.up && p.context == PacketContext::Response)
            .map(|p| p.len)
            .collect()
    };
    let resource = |seen: &[Seen]| {
        seen.iter().any(|p| {
            matches!(
                p.context,
                PacketContext::Resource | PacketContext::ResourceAdv
            )
        })
    };
    println!(
        "paged pull: {paged:?}, pages {pages:?}, responses {:?}",
        responses(&paged_seen)
    );
    println!(
        "whole pull: {whole:?}, responses {:?}",
        responses(&whole_seen)
    );
    let (Outcome::Pulled { entries: a, .. }, Outcome::Pulled { entries: b, .. }) = (&paged, &whole)
    else {
        panic!("not pulled");
    };
    // The mark is an archive entry too: it and the four messages.
    assert_eq!(
        (*a, *b),
        (5, 5),
        "both pulls bring the mark and the four messages"
    );
    assert!(pages.len() > 1, "the bound split the archive: {pages:?}");
    assert!(
        responses(&paged_seen).iter().all(|len| *len <= MTU) && !resource(&paged_seen),
        "every LoRa page is one packet of the default MTU"
    );
    assert!(
        responses(&whole_seen).iter().any(|len| *len > MTU) || resource(&whole_seen),
        "control: the home's own page does not fit one LoRa packet"
    );

    println!(
        "| phase | packets | bytes up | bytes down | airtime up ms | airtime down ms | packets |"
    );
    println!("|---|---:|---:|---:|---:|---:|---|");
    for phase in &phases {
        println!("{}", phase.row());
    }
    for p in paged_seen.iter().chain(&whole_seen) {
        assert!(
            p.len <= MTU || !p.up,
            "the board sent a packet over the MTU: {p:?}"
        );
    }

    // The airtime per hour. A link keepalive is 20 bytes each way
    // (`Link::build_keepalive_packet`: flags, hops, link id, context, one
    // byte), every LINK_KEEPALIVE_SECS on the watching link.
    let keepalive = 20;
    let per_hour = 3600.0 / loquax::home::keepalive::LINK_KEEPALIVE_SECS as f64;
    let idle = per_hour * 2.0 * ms(keepalive) as f64;
    let named = |name: &str| {
        phases
            .iter()
            .find(|p| p.name.starts_with(name))
            .map(|p| p.airtime(None))
            .unwrap_or_default()
    };
    // Every entry the archive gains wakes the watching link, the board's
    // own too, and every wake past the cursor is a pull: a message in is
    // its wake, the pull, the mark read and the mark's wake; a message out
    // is the send, its wake and the pull of its own entry.
    let wake = named("wake");
    let incoming = wake + named("pull one") + named("mark") + wake;
    let outgoing = named("send") + wake + named("pull one");
    let link = |phase: &str| {
        phases
            .iter()
            .find(|p| p.name.starts_with(phase))
            .map(|p| {
                p.packets
                    .iter()
                    .filter(|p| {
                        matches!(
                            (p.kind, p.context),
                            (PacketType::LinkRequest, _)
                                | (PacketType::Proof, PacketContext::Lrproof)
                                | (_, PacketContext::Lrrtt)
                                | (_, PacketContext::LinkIdentify)
                                | (_, PacketContext::LinkClose)
                        )
                    })
                    .map(|p| ms(p.len))
                    .sum::<u64>()
            })
            .unwrap_or_default()
    };
    let hour = |ms: f64| format!("{ms:.0} ms/h, {:.3} %", ms / 36_000.0);
    println!("| per hour, at SF8 BW125 CR4/5 | airtime |");
    println!("|---|---:|");
    println!(
        "| one keepalive exchange, 2 x {keepalive} B | {} ms |",
        2 * ms(keepalive)
    );
    println!("| idle client, {per_hour:.2} exchanges | {} |", hour(idle));
    println!("| one message in (wake, pull, mark read, its wake) | {incoming} ms |");
    println!("| one message out (send, its wake, the pull of it) | {outgoing} ms |");
    println!(
        "| of which a link of its own per ask (request, proof, rtt, identify, close) | {} ms |",
        link("pull one")
    );
    println!(
        "| idle + one message in | {} |",
        hour(idle + incoming as f64)
    );
    println!(
        "| idle + one in + one out | {} |",
        hour(idle + (incoming + outgoing) as f64)
    );
    // The board's own share, what its duty budget is charged.
    let own = |name: &str| {
        phases
            .iter()
            .find(|p| p.name.starts_with(name))
            .map(|p| p.airtime(Some(true)))
            .unwrap_or_default()
    };
    let up = 3 * own("wake") + 2 * own("pull one") + own("mark") + own("send");
    println!(
        "| the board's own, idle + one in + one out | {} |",
        hour(idle / 2.0 + up as f64)
    );

    drop(events);
    watching.abort();
    mailbox.stop().await;
    relay.stop().await;
}

/// Press `keys` on the board's model as the board presses them, and keep
/// what the model hands out.
fn press(model: &mut UiModel<Field>, row: &mut usize, keys: &[M9Key]) -> Vec<UiEffect> {
    keys.iter()
        .flat_map(|key| group::press(model, row, &event(*key)))
        .collect()
}

/// The board's ask for the one group effect among `effects`, run against
/// its home: what the home answered.
async fn ask_group(
    node: &ReticulumNode,
    device: &leviculum_core::Identity,
    home: &[u8; 64],
    effects: &[UiEffect],
) -> Outcome {
    let verbs: Vec<GroupVerb> = effects
        .iter()
        .filter_map(|effect| group::ask_of(effect, client::client_id()))
        .collect();
    let [verb] = verbs.as_slice() else {
        panic!("one group ask among {effects:?}");
    };
    ask(node, device, home, Verb::Group(verb.clone())).await
}

/// Move the picker's cursor to the friend called `name` and tick it.
fn tick(model: &mut UiModel<Field>, row: &mut usize, name: &str) {
    let scene = loquax_m9::screen::scene(model);
    let rows = &scene.new_group.as_ref().expect("New group").rows;
    let at = rows
        .iter()
        .position(|r| r.name.starts_with(name))
        .unwrap_or_else(|| panic!("no {name} among the friends"));
    while *row > at {
        press(model, row, &[M9Key::Up]);
    }
    while *row < at {
        press(model, row, &[M9Key::Down]);
    }
    press(model, row, &[M9Key::Char(' ')]);
}

/// The members a home keeps of a group it hosts: each one's flags, and
/// whether it left.
fn hosted_members(mailbox: &Mailbox, group: &[u8; 16]) -> Vec<([u8; 16], i64, bool)> {
    let address = loquax::home::lxmf_address(&mailbox.public_key).expect("address");
    loquax::store::Store::open_in(mailbox.dir.path(), address)
        .expect("the store")
        .group_members(group)
        .expect("members")
        .iter()
        .map(|m| (m.address, m.flags, m.left_at.is_some()))
        .collect()
}

/// Wait until `mailbox` keeps `member` of `group` as `wanted` says, by
/// its flags and whether it left.
async fn wait_member(
    mailbox: &Mailbox,
    group: &[u8; 16],
    member: &[u8; 16],
    what: &str,
    wanted: impl Fn(i64, bool) -> bool,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        let members = hosted_members(mailbox, group);
        if members
            .iter()
            .any(|(address, flags, left)| address == member && wanted(*flags, *left))
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the home never kept the member {what}: {members:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Pull everything after `*cursor` in LoRa pages and hand each received
/// message to the board's model, as the engine hands what it ingests.
async fn pull_into_model(
    node: &ReticulumNode,
    device: &leviculum_core::Identity,
    home: &[u8; 64],
    cursor: &mut u64,
    model: &mut UiModel<Field>,
) {
    let mut entries = Vec::new();
    let pull = Ask::new(
        home,
        Verb::Pull {
            cursor: *cursor,
            page_bytes: Some(LORA_PAGE_BYTES as u64),
        },
        0,
        30_000,
    )
    .expect("ask");
    let pulled = client::run_ask(node, device, pull, |page| {
        entries.extend(page.entries.iter().cloned())
    })
    .await
    .expect("pull");
    if let Outcome::Pulled { cursor: at, .. } = pulled {
        *cursor = at;
    }
    for entry in entries {
        if entry.kind != loquax::home::KIND_RECEIVED {
            continue;
        }
        let Ok(message) = leviculum_lxmf::Message::unpack(
            &entry.packed,
            None,
            None,
            leviculum_lxmf::DeliveryMethod::Direct,
        ) else {
            continue;
        };
        update(
            model,
            UiEvent::Engine(OutboxEvent::Received {
                sent_at: 0,
                message_id: message.message_id,
                source: message.source_hash,
                title: message.title,
                body: message.content,
                fields: message.fields,
                verified: true,
            }),
        );
    }
}

/// Slice six, host half (1366): every group verb the board's keys reach,
/// asked of a real home over the mesh by the `no_std` ask
/// (`Verb::Group`). The board's home is Lew's. As its admin the board
/// makes "Seilschaft" with Anna (`/group`), adds Bob (`/groupadd`) and
/// deletes it (`/groupdelete`), each a walk of keys on the member sheet
/// and the picker; Lew's home hosts it, keeps Bob, and hosts nothing after
/// the delete. Then Anna, on a home of her own, makes "Chor" with Lew: the
/// invitation reaches Lew's home, the board pulls it in LoRa pages, Enter
/// on the card joins and `l`, `y` on the sheet leaves; Anna's home keeps
/// Lew joined, then left.
#[tokio::test(flavor = "multi_thread")]
async fn the_boards_group_keys_make_join_add_leave_and_delete_on_real_homes() {
    use loquax::store::groups::MEMBER_JOINED;
    use M9Key::{Char, Enter};
    logs();
    let relay = Relay::start(&[]).await;
    let lew = Mailbox::start(vec![entry(relay.peer)]).await;
    let anna = Mailbox::start(vec![entry(relay.peer)]).await;
    let dir = tempfile::tempdir().expect("board dir");
    let node = board_node(relay.peer, dir.path()).await;
    let device = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    let anna_dir = tempfile::tempdir().expect("Anna's phone dir");
    let anna_node = board_node(relay.peer, anna_dir.path()).await;
    let anna_device = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    for (node, device, mailbox) in [(&node, &device, &lew), (&anna_node, &anna_device, &anna)] {
        let code = HomeCode {
            public_key: mailbox.public_key,
            token: mailbox.token,
        };
        let join = Pairing::join(&code, 0, 30_000).expect("join");
        let joined = client::run_ask(node, device, join, |_| {}).await;
        assert_eq!(joined, Ok(Outcome::Joined));
    }
    let lew_address = loquax::home::lxmf_address(&lew.public_key).expect("address");
    let anna_address = loquax::home::lxmf_address(&anna.public_key).expect("address");
    let bob = [0xb0; 16];

    // The board: Lew's model, Anna and Bob among its friends.
    let mut model = UiModel::<Field>::new(&[anna_address, bob]);
    model.address = Some(lew_address);
    model.names.remember(anna_address, "Anna");
    model.names.remember(bob, "Bob");
    model.focus = Pane::Conversations;
    let mut row = 0;

    // Create: n, Anna ticked, Enter, the name, Enter.
    press(&mut model, &mut row, &[Char('n')]);
    tick(&mut model, &mut row, "Anna");
    let mut keys = vec![Enter];
    keys.extend("Seilschaft".chars().map(Char));
    keys.push(Enter);
    let created = press(&mut model, &mut row, &keys);
    let Outcome::Group(seil) = ask_group(&node, &device, &lew.public_key, &created).await else {
        panic!("/group made nothing");
    };
    update(&mut model, UiEvent::GroupMade(Ok(seil)));
    let members = hosted_members(&lew, &seil);
    assert!(
        members.iter().any(|(a, ..)| *a == anna_address),
        "Lew's home hosts Seilschaft with Anna: {members:?}"
    );
    assert!(
        !members.iter().any(|(a, ..)| *a == bob),
        "control: not Bob yet"
    );

    // Add: the sheet's a, Bob ticked, Enter.
    update(&mut model, UiEvent::Open { peer: seil });
    model.focus = Pane::Messages;
    press(&mut model, &mut row, &[Char('m'), Char('a')]);
    tick(&mut model, &mut row, "Bob");
    let added = press(&mut model, &mut row, &[Enter]);
    let outcome = ask_group(&node, &device, &lew.public_key, &added).await;
    assert_eq!(outcome, Outcome::Group(seil));
    update(&mut model, UiEvent::MembersAdded(Ok(seil)));
    assert!(
        hosted_members(&lew, &seil).iter().any(|(a, ..)| *a == bob),
        "Lew's home keeps Bob after the add"
    );

    // Delete: the sheet's d and y.
    model.focus = Pane::Messages;
    press(&mut model, &mut row, &[Char('m')]);
    let deleted = press(&mut model, &mut row, &[Char('d'), Char('y')]);
    let outcome = ask_group(&node, &device, &lew.public_key, &deleted).await;
    assert_eq!(outcome, Outcome::Group(seil));
    let store = loquax::store::Store::open_in(lew.dir.path(), lew_address).expect("store");
    let hosted = loquax::home::group::hosted(lew.dir.path(), &store).expect("hosted");
    assert!(
        hosted.iter().all(|h| h.group.address != seil),
        "Lew's home hosts no Seilschaft after the delete"
    );

    // Anna's Chor, with Lew: made on Anna's home by her phone's ask.
    let chor = wire::GroupRequest {
        client_id: client::client_id(),
        name: "Chor".into(),
        members: vec![(lew_address, "Lew".into())],
    };
    let Outcome::Group(chor) = ask(
        &anna_node,
        &anna_device,
        &anna.public_key,
        Verb::Group(GroupVerb::Create(chor)),
    )
    .await
    else {
        panic!("Anna's /group made nothing");
    };

    // The invitation reaches Lew's home; the board pulls it in LoRa pages.
    let mut cursor = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    while model.group(&chor).is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "Anna's invitation never reached the board"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
        pull_into_model(&node, &device, &lew.public_key, &mut cursor, &mut model).await;
    }

    // The control: Anna's home keeps Lew invited, not joined, until the
    // board's Join.
    let lew_in_chor = || {
        hosted_members(&anna, &chor)
            .into_iter()
            .find(|(address, ..)| *address == lew_address)
            .expect("Lew is a member of Chor")
    };
    let (_, flags, left) = lew_in_chor();
    assert!(flags & MEMBER_JOINED == 0 && !left, "{flags:#x} {left}");

    // Join: Enter on Chor's card.
    update(&mut model, UiEvent::Open { peer: chor });
    model.focus = Pane::Messages;
    let joined = press(&mut model, &mut row, &[Enter]);
    let Outcome::Sent(_) = ask_group(&node, &device, &lew.public_key, &joined).await else {
        panic!("the Join was not sent");
    };
    wait_member(&anna, &chor, &lew_address, "joined", |flags, left| {
        flags & MEMBER_JOINED != 0 && !left
    })
    .await;

    assert!(!lew_in_chor().2, "control: not left before the Leave");

    // Leave: the sheet's l and y.
    let left = press(&mut model, &mut row, &[Char('m'), Char('l'), Char('y')]);
    let Outcome::Sent(_) = ask_group(&node, &device, &lew.public_key, &left).await else {
        panic!("the Leave was not sent");
    };
    wait_member(&anna, &chor, &lew_address, "left", |_, left| left).await;

    lew.stop().await;
    anna.stop().await;
    relay.stop().await;
}
