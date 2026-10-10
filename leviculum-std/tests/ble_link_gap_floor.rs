//! The BLE link gap floor (leviculum#379): at what inter-packet gap does a
//! BLE link between two lnsd first lose a packet to its receiver's
//! reassembly, as a function of how many fragments a packet takes?
//!
//! ## Why this is a measurement and not a gate
//!
//! The pacer (`interfaces/ble/links.rs`, `LinkPacer`) holds the next
//! packet's first fragment back until `DEFAULT_TX_GAP_MS` (100 ms) after
//! the previous packet's last one. The receiver keeps ONE reassembly per
//! link and the 5-byte fragment header carries no packet id
//! (`leviculum-core/src/framing/ble.rs`, `BleDefragmenter::process`), so
//! two packets whose fragments interleave can only end in a discard. The
//! gap is therefore not a tuning knob but a distance, and before anyone
//! lowers it the floor has to be read off the real BlueZ path. That takes
//! the patched `btvirt` and a running `bluetoothd`, and a grid of minutes,
//! so the test is `#[ignore]`d and run by name:
//!
//! ```text
//! cargo build --release --bin lnsd
//! cargo test -p leviculum-std --test ble_link_gap_floor -- --ignored --nocapture
//! ```
//!
//! ## What one cell is
//!
//! A room the way periculum's `ble_room` cells build one: `btvirt -L -l2`,
//! two release `lnsd`, each bound to its own virtual controller, both
//! with the interface's `tx_gap_ms = G`. Once the link is up, a shared-
//! instance client on node 0 writes `N` PLAIN broadcast packets of `L`
//! wire bytes, each carrying its sequence number; lnsd forwards a local
//! client's PLAIN broadcast onto every interface, so they all leave over
//! the one BLE link. A client on node 1 receives what the far lnsd
//! reassembles and forwards to its local clients, and counts distinct
//! sequence numbers. The receiver's `BLE_RX_ABANDON` lines (the
//! defragmenter's discard count) and its `bad fragment` lines are read
//! from its log afterwards.
//!
//! The packets go out in bursts of [`BURST`], each burst after the
//! previous one stopped arriving: a central link's queue holds 32 packets
//! (`LINK_QUEUE_DEPTH`) and drops the rest, which would be a queue
//! overrun and not the reassembly loss this measures. Within a burst the
//! interface queue is never empty, so the pacer's gap IS the spacing.
//!
//! ## The grid
//!
//! Environment, all optional (test-only, read here and nowhere else):
//!
//! - `GAP_FLOOR_GAPS` — comma-separated gaps in ms, default `100,50,25,10,5,0`
//! - `GAP_FLOOR_LENS` — wire lengths in bytes, default `60,200,300,500`
//!   (one, one, two and three fragments at [`FRAGMENT_MTU`]; 200 is the
//!   length #379's plan named for two fragments, which it is only below
//!   an ATT MTU of 208)
//! - `GAP_FLOOR_N` — packets per cell, default 200
//! - `GAP_FLOOR_REPS` — rooms per gap, default 1
//! - `GAP_FLOOR_SENDER` — the node that sends, `0` (default) or `1`.
//!   btvirt numbers its controllers in order and the lower address
//!   dials, so node 0 sends on the central's write path and node 1 on
//!   the peripheral's notify path; each has its own pacer
//! - `GAP_FLOOR_KEEP` — a directory the rooms' logs are copied into
//! - `GAP_FLOOR_BTMON` — an HCI capture file: the first room runs
//!   `sudo -n timeout 60 btmon -w <file>` beside it, the wire evidence
//!   that a cell's packets went out in the fragments its `frags=` says.
//!   Keep such a run under a minute; btmon stops itself at 60 s
//!
//! One room serves every length of one gap: the gap is a config key, so
//! a new gap is a restart, a new length is not. Every cell prints one
//! `GAP_FLOOR_CELL` line; the table in
//! `docs/measurements/2026-10-10-ble-link-gap-floor.md` is those lines.

#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use leviculum_core::framing::hdlc;
use leviculum_core::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use leviculum_core::DestinationType;
use leviculum_std::process::spawn_supervised;

const BTVIRT: &str = "/usr/local/bin/btvirt";

/// Packets per burst; see the module docs.
const BURST: usize = 16;

/// A burst is over when nothing new arrived for this long.
const QUIET: Duration = Duration::from_millis(1_500);

/// How long two fresh lnsd get to find each other and handshake.
const LINK_WAIT: Duration = Duration::from_secs(60);

/// The PLAIN destination the probes are addressed to. Nobody registers
/// it: plain broadcasts are forwarded between local clients and network
/// interfaces whether or not anyone does.
const DEST: [u8; 16] = *b"gap-floor-probe!";

/// The first payload bytes of every probe, so the receiver can tell its
/// probes from anything else a shared instance hands a client.
const MAGIC: &[u8; 4] = b"GAPF";

/// The ATT MTU the sender fragments by. Two BlueZ hosts exchange 512,
/// at which every Reticulum packet is one fragment; a board exchanges
/// 253 (payload 245 a fragment), which splits the grid's lengths into
/// one, two and three fragments. The interface's `fragment_mtu` key
/// caps the sizing to it.
const FRAGMENT_MTU: usize = 253;

/// Wire bytes of a header-1 packet before its data.
const HEADER_LEN: usize = 2 + 16 + 1;

fn env_list(key: &str, default: &[u64]) -> Vec<u64> {
    match std::env::var(key) {
        Ok(v) => v
            .split(',')
            .map(|s| s.trim().parse().expect("a number"))
            .collect(),
        Err(_) => default.to_vec(),
    }
}

fn env_num(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().parse().expect("a number"))
        .unwrap_or(default)
}

/// The release `lnsd`, resolved the way the mvrs resolve it, and refused
/// when it is older than the BLE interface it is meant to measure (the
/// `tx_gap_ms` key it is configured with is a recent addition).
fn release_lnsd() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let triple = std::env::var("CARGO_BUILD_TARGET")
        .unwrap_or_else(|_| "x86_64-unknown-linux-musl".to_string());
    let with_triple = target.join(triple).join("release").join("lnsd");
    let bin = if with_triple.exists() {
        with_triple
    } else {
        target.join("release").join("lnsd")
    };
    let built = std::fs::metadata(&bin)
        .and_then(|m| m.modified())
        .unwrap_or_else(|e| panic!("{}: {e}; cargo build --release --bin lnsd", bin.display()));
    let ble = root.join("leviculum-std/src/interfaces/ble");
    for entry in std::fs::read_dir(&ble).expect("ble sources") {
        let entry = entry.expect("entry");
        let touched = entry.metadata().and_then(|m| m.modified()).expect("mtime");
        assert!(
            built >= touched,
            "{} is older than {}; cargo build --release --bin lnsd",
            bin.display(),
            entry.path().display()
        );
    }
    bin
}

fn adapters() -> BTreeSet<String> {
    std::fs::read_dir("/sys/class/bluetooth")
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("hci") && !n.contains(':'))
                .collect()
        })
        .unwrap_or_default()
}

fn bluez_has(adapter: &str) -> bool {
    Command::new("busctl")
        .args(["introspect", "org.bluez", &format!("/org/bluez/{adapter}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Forget every device BlueZ cached on `adapter`: btvirt hands out the
/// same addresses every time, and a device object left from the previous
/// room would carry its GATT cache into this one (periculum's
/// `remove_devices_on`, same reason).
fn forget_devices(adapter: &str) {
    let Ok(out) = Command::new("busctl")
        .args(["--list", "tree", "org.bluez"])
        .output()
    else {
        return;
    };
    let prefix = format!("/org/bluez/{adapter}/dev_");
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let path = line.trim();
        if path.starts_with(&prefix) && path[prefix.len()..].len() == 17 {
            let _ = Command::new("busctl")
                .args([
                    "call",
                    "org.bluez",
                    &format!("/org/bluez/{adapter}"),
                    "org.bluez.Adapter1",
                    "RemoveDevice",
                    "o",
                    path,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn config(k: usize, adapter: &str, gap_ms: u64) -> String {
    format!(
        "[reticulum]\n  enable_transport = no\n  share_instance = yes\n  \
         instance_name = gapfloor_{pid}_{k}\n\n[logging]\n  loglevel = 4\n\n\
         [interfaces]\n  [[BLE Room]]\n    type = BLEInterface\n    enabled = yes\n    \
         device = {adapter}\n    tx_gap_ms = {gap_ms}\n    fragment_mtu = {FRAGMENT_MTU}\n",
        pid = std::process::id()
    )
}

/// btvirt and the two daemons; killed in reverse on drop.
struct Room {
    btvirt: Child,
    daemons: Vec<Child>,
    dir: tempfile::TempDir,
}

impl Drop for Room {
    fn drop(&mut self) {
        for d in &mut self.daemons {
            let _ = d.kill();
            let _ = d.wait();
        }
        let _ = self.btvirt.kill();
        let _ = self.btvirt.wait();
    }
}

impl Room {
    /// Node `k`'s log so far, ANSI colour codes stripped.
    fn log(&self, k: usize) -> String {
        let raw =
            std::fs::read_to_string(self.dir.path().join(format!("n{k}.log"))).unwrap_or_default();
        let mut out = String::with_capacity(raw.len());
        let mut chars = raw.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn instance(k: usize) -> String {
        format!("gapfloor_{}_{k}", std::process::id())
    }
}

fn open_room(lnsd: &Path, gap_ms: u64) -> Room {
    let before = adapters();
    let dir = tempfile::tempdir().expect("room dir");
    let log = std::fs::File::create(dir.path().join("btvirt.log")).expect("btvirt log");
    let mut cmd = Command::new(BTVIRT);
    cmd.args(["-L", "-l2"])
        .stdin(Stdio::null())
        .stdout(log.try_clone().expect("clone"))
        .stderr(log);
    let btvirt = spawn_supervised(cmd).expect("spawn btvirt");
    let deadline = Instant::now() + Duration::from_secs(15);
    let fresh: Vec<String> = loop {
        let mut fresh: Vec<(u32, String)> = adapters()
            .difference(&before)
            .filter_map(|n| n[3..].parse().ok().map(|i| (i, n.clone())))
            .collect();
        fresh.sort();
        if fresh.len() >= 2 && fresh.iter().take(2).all(|(_, n)| bluez_has(n)) {
            break fresh.into_iter().take(2).map(|(_, n)| n).collect();
        }
        assert!(
            Instant::now() < deadline,
            "btvirt brought up {} of 2 controllers bluetoothd owns",
            fresh.len()
        );
        thread::sleep(Duration::from_millis(250));
    };
    let mut room = Room {
        btvirt,
        daemons: Vec::new(),
        dir,
    };
    for (k, adapter) in fresh.iter().enumerate() {
        forget_devices(adapter);
        let cdir = room.dir.path().join(format!("c{k}"));
        std::fs::create_dir_all(&cdir).expect("config dir");
        std::fs::write(cdir.join("config"), config(k, adapter, gap_ms)).expect("config");
        let log = std::fs::File::create(room.dir.path().join(format!("n{k}.log"))).expect("log");
        let mut cmd = Command::new(lnsd);
        cmd.arg("--config")
            .arg(&cdir)
            // The receiver's `bad fragment` line is a debug line.
            .env("RUST_LOG", "info,leviculum_std::interfaces::ble=debug")
            .stdin(Stdio::null())
            .stdout(log.try_clone().expect("clone"))
            .stderr(log);
        let child = spawn_supervised(cmd).expect("spawn lnsd");
        room.daemons.push(child);
    }
    room
}

/// The link's sizing MTU and the sender's role on it, from the sender's
/// `BLE_LINK_UP` line, once both daemons have logged one.
fn wait_link(room: &Room, sender: usize) -> (usize, String) {
    let deadline = Instant::now() + LINK_WAIT;
    loop {
        let (a, b) = (room.log(sender), room.log(1 - sender));
        if let (Some(line), true) = (
            a.lines().find(|l| l.contains("BLE_LINK_UP")),
            b.contains("BLE_LINK_UP"),
        ) {
            let field = |key: &str| {
                line.split_whitespace()
                    .find_map(|w| w.strip_prefix(key))
                    .map(|v| v.trim_matches('"').to_string())
                    .unwrap_or_default()
            };
            let mtu = field("mtu=")
                .parse()
                .unwrap_or_else(|e| panic!("mtu in {line:?}: {e}"));
            return (mtu, field("role="));
        }
        assert!(
            Instant::now() < deadline,
            "no BLE link within {LINK_WAIT:?}\n--- sender ---\n{a}\n--- receiver ---\n{b}"
        );
        thread::sleep(Duration::from_millis(500));
    }
}

fn connect(instance: &str) -> UnixStream {
    let addr = SocketAddr::from_abstract_name(format!("rns/{instance}").as_bytes())
        .expect("abstract addr");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match UnixStream::connect_addr(&addr) {
            Ok(s) => return s,
            Err(e) => {
                assert!(Instant::now() < deadline, "attach to {instance}: {e}");
                thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

fn probe(cell: u32, seq: u32, wire_len: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(wire_len - HEADER_LEN);
    data.extend_from_slice(MAGIC);
    data.extend_from_slice(&cell.to_be_bytes());
    data.extend_from_slice(&seq.to_be_bytes());
    data.resize(wire_len - HEADER_LEN, (seq & 0xff) as u8);
    let packet = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Plain,
            packet_type: PacketType::Data,
        },
        hops: 0,
        transport_id: None,
        destination_hash: DEST,
        context: PacketContext::None,
        data: PacketData::Owned(data),
    };
    let mut raw = vec![0u8; packet.packed_size()];
    let n = packet.pack(&mut raw).expect("pack");
    raw.truncate(n);
    assert_eq!(raw.len(), wire_len, "probe has the cell's wire length");
    let mut framed = Vec::new();
    hdlc::frame(&raw, &mut framed);
    framed
}

/// Distinct sequence numbers of `cell` the receiving client has seen.
struct Received {
    seen: Mutex<BTreeSet<(u32, u32)>>,
}

impl Received {
    fn count(&self, cell: u32) -> usize {
        self.seen
            .lock()
            .expect("lock")
            .iter()
            .filter(|(c, _)| *c == cell)
            .count()
    }
}

fn spawn_receiver(mut stream: UnixStream, got: Arc<Received>) {
    thread::spawn(move || {
        let mut deframer = hdlc::Deframer::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = match stream.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            for result in deframer.process(&buf[..n]) {
                let hdlc::DeframeResult::Frame(raw) = result else {
                    continue;
                };
                let data = raw.get(HEADER_LEN..).unwrap_or_default();
                if raw.get(2..18) != Some(&DEST[..]) || data.get(..4) != Some(&MAGIC[..]) {
                    continue;
                }
                let word = |at: usize| u32::from_be_bytes(data[at..at + 4].try_into().unwrap());
                got.seen.lock().expect("lock").insert((word(4), word(8)));
            }
        }
    });
}

fn count_lines(log: &str, needle: &str) -> usize {
    log.lines().filter(|l| l.contains(needle)).count()
}

/// The sum of `lost=` over the receiver's `BLE_RX_ABANDON` lines.
fn abandoned(log: &str) -> u64 {
    log.lines()
        .filter(|l| l.contains("BLE_RX_ABANDON"))
        .filter_map(|l| {
            l.split_whitespace()
                .find_map(|w| w.strip_prefix("lost="))
                .and_then(|v| v.parse::<u64>().ok())
        })
        .sum()
}

#[test]
#[ignore = "needs the patched btvirt, /dev/vhci and bluetoothd; a measurement grid, run by name"]
fn ble_link_gap_floor() {
    assert!(Path::new(BTVIRT).is_file(), "{BTVIRT} missing");
    assert!(Path::new("/dev/vhci").exists(), "/dev/vhci missing");
    let lnsd = release_lnsd();
    let gaps = env_list("GAP_FLOOR_GAPS", &[100, 50, 25, 10, 5, 0]);
    let lens = env_list("GAP_FLOOR_LENS", &[60, 200, 300, 500]);
    let n = env_num("GAP_FLOOR_N", 200) as u32;
    let reps = env_num("GAP_FLOOR_REPS", 1);
    let keep = std::env::var_os("GAP_FLOOR_KEEP").map(PathBuf::from);
    let sender = env_num("GAP_FLOOR_SENDER", 0) as usize;
    assert!(sender < 2, "GAP_FLOOR_SENDER is 0 or 1");
    let receiver = 1 - sender;
    let mut btmon = std::env::var_os("GAP_FLOOR_BTMON").map(PathBuf::from);

    let mut cell = 0u32;
    for &gap in &gaps {
        for rep in 0..reps {
            let room = open_room(&lnsd, gap);
            // Attached while the controllers exist, before the link.
            let capture = btmon.take().map(|file| {
                let mut cmd = Command::new("sudo");
                cmd.args(["-n", "timeout", "60", "btmon", "-w"])
                    .arg(file)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                spawn_supervised(cmd).expect("spawn btmon")
            });
            let (mtu, role) = wait_link(&room, sender);
            assert!(
                room.log(sender)
                    .contains(&format!("tx_gap_ms={}", gap.min(u64::from(u16::MAX)))),
                "the sender's bring-up line names the gap it was configured with"
            );
            assert!(
                room.log(sender)
                    .contains(&format!("fragment_mtu={FRAGMENT_MTU}")),
                "the sender's bring-up line names the fragment MTU cap"
            );
            // Let the peers' announce-free link settle; nothing else
            // crosses it in this room.
            thread::sleep(Duration::from_secs(2));
            let got = Arc::new(Received {
                seen: Mutex::new(BTreeSet::new()),
            });
            spawn_receiver(connect(&Room::instance(receiver)), got.clone());
            let mut tx = connect(&Room::instance(sender));
            thread::sleep(Duration::from_millis(500));

            for &len in &lens {
                cell += 1;
                let len = len as usize;
                let abandon_before = abandoned(&room.log(receiver));
                let bad_before = count_lines(&room.log(receiver), "bad fragment");
                let drops_before = count_lines(&room.log(sender), "BLE_TX_FANOUT_DROP");
                let started = Instant::now();
                let mut seq = 0u32;
                while seq < n {
                    let end = (seq + BURST as u32).min(n);
                    for s in seq..end {
                        tx.write_all(&probe(cell, s, len)).expect("client write");
                    }
                    seq = end;
                    let mut last = got.count(cell);
                    let mut quiet_since = Instant::now();
                    while quiet_since.elapsed() < QUIET && last < seq as usize {
                        thread::sleep(Duration::from_millis(50));
                        let now = got.count(cell);
                        if now != last {
                            last = now;
                            quiet_since = Instant::now();
                        }
                    }
                }
                let delivered = got.count(cell);
                let (rx_log, tx_log) = (room.log(receiver), room.log(sender));
                let frags = leviculum_core::framing::ble::fragment_count(len, mtu);
                println!(
                    "GAP_FLOOR_CELL sender=n{sender} gap_ms={gap} len={len} frags={frags} mtu={mtu} \
                     sender_role={role} rep={rep} sent={n} delivered={delivered} \
                     abandon={} bad_fragment={} fanout_drop={} secs={:.1}",
                    abandoned(&rx_log) - abandon_before,
                    count_lines(&rx_log, "bad fragment") - bad_before,
                    count_lines(&tx_log, "BLE_TX_FANOUT_DROP") - drops_before,
                    started.elapsed().as_secs_f64()
                );
            }
            let logs = [room.log(0), room.log(1)];
            drop(room);
            if let Some(mut capture) = capture {
                let _ = capture.wait();
            }
            if let Some(dir) = &keep {
                let dest = dir.join(format!("gap{gap}_tx{sender}_rep{rep}"));
                std::fs::create_dir_all(&dest).expect("keep dir");
                for (k, log) in logs.iter().enumerate() {
                    std::fs::write(dest.join(format!("n{k}.log")), log).expect("keep");
                }
            }
        }
    }
}

/// The counters the table is made of, read off real line shapes (ANSI
/// already stripped, as [`Room::log`] hands them over). The grid found no
/// discard at all, so this is the evidence the reader would have counted
/// one.
#[test]
fn the_log_readers_count_what_the_receiver_logs() {
    let log = "2026-10-10T10:22:51Z  WARN leviculum_std::interfaces::ble: \
               event=\"BLE_RX_ABANDON\" iface=ble_0 peer=73bbf0db lost=1 total=1\n\
               2026-10-10T10:22:52Z  WARN leviculum_std::interfaces::ble: \
               event=\"BLE_RX_ABANDON\" iface=ble_0 peer=73bbf0db lost=2 total=3\n\
               2026-10-10T10:22:53Z DEBUG leviculum_std::interfaces::ble: \
               BLE ble_0: bad fragment from 00:AA:01:00:00:01 dropped\n";
    assert_eq!(abandoned(log), 3);
    assert_eq!(count_lines(log, "bad fragment"), 1);
}
