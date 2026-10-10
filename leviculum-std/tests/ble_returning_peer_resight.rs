//! A peer that returns on its old address is sighted again (leviculum#443).
//!
//! ## The failure this reproduces
//!
//! Two lnsd on one btvirt form a BLE link (the lower address dials). The
//! dialled daemon is killed and later restarted on the same controller,
//! same address, same identity — an lnsd restart, the ordinary life of a
//! PC node. The survivor must dial it again; before the #443 fix it never
//! did, however long the room ran.
//!
//! The mechanism, measured on btvirt (2026-10-10, three runs, btmon +
//! full D-Bus monitor): the advertisements arrive at the survivor's
//! controller every scan window and bluetoothd re-offers the stored
//! `Device1` object (RSSI `PropertiesChanged` per window, fresh
//! `ManufacturerData`), the scan task probes it every window — but
//! bluetoothd resolved the peer's GATT services into the device's
//! `UUIDs` property while the link was up and dropped them from it again
//! at the unpaired disconnect, so every probe reads a UUID list without
//! the Columba service ([1800, 1801, 180a]), `decide_from_scan` discards
//! the sighting as a non-peer, and nothing is ever dialled. The fix
//! removes the device object when a link goes down, so the returning
//! peer's first advertisement recreates it from the air.
//!
//! ## Running it
//!
//! Needs the patched `btvirt`, `/dev/vhci`, the system `bluetoothd`, and
//! a fresh release `lnsd` — host-global Bluetooth state, the `ble_room`
//! lock class — so it is `#[ignore]`d and run by name:
//!
//! ```text
//! cargo build --release --bin lnsd
//! cargo test -p leviculum-std --test ble_returning_peer_resight -- --ignored --nocapture
//! ```

#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use leviculum_std::process::spawn_supervised;

const BTVIRT: &str = "/usr/local/bin/btvirt";

/// How long two fresh lnsd get to find each other and handshake.
const LINK_WAIT: Duration = Duration::from_secs(60);

/// How long the survivor gets to notice the departure. The release is
/// event-driven (dc35f43f) but the kill here is a SIGKILL, so the notice
/// rides BlueZ's own deferred disconnect, measured 2.3-2.5 s after the
/// kill; 15 s holds comfortably without waiting out the 45 s sweep.
const NOTICE_WAIT: Duration = Duration::from_secs(15);

/// How long the returned daemon gets to be re-dialled. The survivor
/// scans every 7 s and the departed address's dial backoff is 5 s, so a
/// healthy resight-and-dial lands within two windows plus the handshake;
/// 90 s is several fallback rounds. Before the fix, three 120 s
/// reproductions never saw one.
const RESIGHT_WAIT: Duration = Duration::from_secs(90);

/// The release `lnsd`, resolved the way the mvrs resolve it, and refused
/// when it is older than the BLE interface under test (same rule and
/// reason as `ble_link_gap_floor`).
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
/// same addresses every time, and a device object left from a previous
/// room is exactly the stale state this test must create itself, inside
/// the run, to mean anything.
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

fn config(k: usize, adapter: &str) -> String {
    // Node 1 may dial nobody (`initiate_only` on an address that exists
    // nowhere): the rejoin this test times must come from node 0's own
    // re-sighting and dial. Without the key a 2-node room self-heals —
    // the returned node 1 sights node 0 and its #375 fallback dials it,
    // a rescue the 3-node cell does not get because node 1's one
    // outgoing slot goes to the third node (565 run 2).
    let no_dials = if k == 1 {
        "    initiate_only = 00:00:00:00:00:00\n"
    } else {
        ""
    };
    format!(
        "[reticulum]\n  enable_transport = no\n  share_instance = yes\n  \
         instance_name = resight_{pid}_{k}\n\n[logging]\n  loglevel = 4\n\n\
         [interfaces]\n  [[BLE Room]]\n    type = BLEInterface\n    enabled = yes\n    \
         device = {adapter}\n{no_dials}",
        pid = std::process::id()
    )
}

/// btvirt and the two daemons; killed in reverse on drop. The departed
/// daemon's slot holds its config dir so the restart is the same daemon.
struct Room {
    btvirt: Child,
    daemons: Vec<Option<Child>>,
    adapters: Vec<String>,
    dir: tempfile::TempDir,
}

impl Drop for Room {
    fn drop(&mut self) {
        for d in self.daemons.iter_mut().flatten() {
            let _ = d.kill();
            let _ = d.wait();
        }
        for adapter in &self.adapters {
            forget_devices(adapter);
        }
        let _ = self.btvirt.kill();
        let _ = self.btvirt.wait();
        // `RESIGHT_KEEP=<dir>` copies the room's logs out before the
        // tempdir goes, green and red alike.
        if let Ok(keep) = std::env::var("RESIGHT_KEEP") {
            let _ = std::fs::create_dir_all(&keep);
            for k in 0..self.daemons.len() {
                let name = format!("n{k}.log");
                let _ = std::fs::copy(self.dir.path().join(&name), Path::new(&keep).join(&name));
            }
        }
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

    fn spawn_daemon(&mut self, lnsd: &Path, k: usize) {
        let cdir = self.dir.path().join(format!("c{k}"));
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.path().join(format!("n{k}.log")))
            .expect("log");
        let mut cmd = Command::new(lnsd);
        cmd.arg("--config")
            .arg(&cdir)
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(log.try_clone().expect("clone"))
            .stderr(log);
        self.daemons[k] = Some(spawn_supervised(cmd).expect("spawn lnsd"));
    }
}

fn open_room(lnsd: &Path) -> Room {
    assert!(
        !Command::new("pgrep")
            .args(["-x", "btvirt"])
            .output()
            .is_ok_and(|o| o.status.success()),
        "another btvirt is running; this test needs the controllers to itself"
    );
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
        daemons: vec![None, None],
        adapters: fresh.clone(),
        dir,
    };
    for (k, adapter) in fresh.iter().enumerate() {
        forget_devices(adapter);
        let cdir = room.dir.path().join(format!("c{k}"));
        std::fs::create_dir_all(&cdir).expect("config dir");
        std::fs::write(cdir.join("config"), config(k, adapter)).expect("config");
        room.spawn_daemon(lnsd, k);
    }
    room
}

/// Wait until `log_of` contains at least `n` lines matching `pred`, or
/// panic with both logs after `wait`.
fn wait_for_lines(
    room: &Room,
    node: usize,
    wait: Duration,
    n: usize,
    what: &str,
    pred: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + wait;
    loop {
        let log = room.log(node);
        if log.lines().filter(|l| pred(l)).count() >= n {
            return log;
        }
        assert!(
            Instant::now() < deadline,
            "node {node}: no {what} within {wait:?}\n--- node 0 ---\n{}\n--- node 1 ---\n{}",
            room.log(0),
            room.log(1),
        );
        thread::sleep(Duration::from_millis(500));
    }
}

/// The `key=value` field of the first line of `log` matching `pred`.
fn field_of(log: &str, pred: impl Fn(&str) -> bool, key: &str) -> String {
    log.lines()
        .find(|l| pred(l))
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|w| w.strip_prefix(key))
                .map(|v| v.trim_matches('"').to_string())
        })
        .unwrap_or_default()
}

#[test]
#[ignore = "needs btvirt + bluetoothd + a fresh release lnsd (host-global Bluetooth state); run by name"]
fn a_peer_back_on_its_old_address_is_sighted_again() {
    let lnsd = release_lnsd();
    let mut room = open_room(&lnsd);

    // The room forms: node 0 (lower address) dials node 1.
    let up = |l: &str| l.contains("BLE_LINK_UP");
    let log0 = wait_for_lines(&room, 0, LINK_WAIT, 1, "BLE_LINK_UP", up);
    wait_for_lines(&room, 1, LINK_WAIT, 1, "BLE_LINK_UP", up);
    let peer_addr = field_of(&log0, up, "addr=");
    assert!(!peer_addr.is_empty(), "BLE_LINK_UP without addr= in {log0}");

    // Node 1 vanishes without a word — SIGKILL, the harshest departure,
    // so nothing on its side can clean up and the survivor's BlueZ keeps
    // the stored device object.
    if let Some(mut child) = room.daemons[1].take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let down = |l: &str| l.contains("BLE_LINK_DOWN");
    wait_for_lines(&room, 0, NOTICE_WAIT, 1, "BLE_LINK_DOWN", down);

    // A short absence, then the same daemon returns on the same
    // controller from the same config dir: same address, same identity.
    thread::sleep(Duration::from_secs(3));
    room.spawn_daemon(&lnsd, 1);

    // The survivor must link it again. Before the #443 fix the sighting
    // never came back: the stored device's UUIDs no longer carry the
    // service, so every window's probe discards the peer.
    let re_up = |l: &str| l.contains("BLE_LINK_UP") && l.contains(&format!("addr={peer_addr}"));
    wait_for_lines(
        &room,
        0,
        RESIGHT_WAIT,
        2,
        "second BLE_LINK_UP for the returned peer",
        re_up,
    );
}
