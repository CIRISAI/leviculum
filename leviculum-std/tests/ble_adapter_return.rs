//! A BLE interface whose adapter vanishes and returns links again (600).
//!
//! ## The failure this reproduces
//!
//! Two lnsd on one btvirt form a BLE link. Then the controllers' D-Bus
//! objects go away and come back — bluetoothd restarting under a package
//! upgrade, a USB dongle re-enumerating, both ordinary on a PC node. Here
//! it is btvirt itself that is stopped and started again: its vhci
//! controllers unregister, bluetoothd emits `InterfacesRemoved` for
//! `/org/bluez/hciN`, and the restarted btvirt registers controllers at
//! the same indices and addresses, so bluetoothd emits `InterfacesAdded`
//! for the same paths. The host-global alternative, `systemctl restart
//! bluetooth`, would also tear down every other Bluetooth user on the
//! host, a neighbouring room and the system lnsd included.
//!
//! Before the fix the daemons never linked again. The adapter object took
//! the GATT application, the advertisement and the connections with it;
//! the session held a handle to the dead object and kept it, and the scan
//! task retried discovery against it every window with a DEBUG line
//! (589's run 1: `discovery failed: the target object was either not
//! present or removed` every 5 s for two minutes). Nothing advertised,
//! nothing served, so nothing was ever sighted.
//!
//! ## The bound
//!
//! The interface re-resolves a lost adapter once per `discovery_interval`
//! (5 s, the default these daemons run), so `BLE_ADAPTER_BACK` follows
//! the adapters' return within one interval plus the session's setup
//! calls: [`BACK_WAIT`]. From there the room is a fresh room, and gets
//! the budget a fresh room gets: [`LINK_WAIT`].
//!
//! ## Running it
//!
//! Needs the patched `btvirt`, `/dev/vhci`, the system `bluetoothd`, and
//! a fresh release `lnsd` — host-global Bluetooth state, the `ble_room`
//! lock class — so it is `#[ignore]`d and run by name:
//!
//! ```text
//! cargo build --release --bin lnsd
//! cargo test -p leviculum-std --test ble_adapter_return -- --ignored --nocapture
//! ```
//!
//! `ADAPTER_RETURN_KEEP=<dir>` copies both daemons' logs, btvirt's, and a
//! `dbus-monitor` capture of bluetoothd's `ObjectManager` signals out of
//! the room, green or red. `ADAPTER_RETURN_LNSD=<path>` runs a given
//! binary without the freshness check, for the red run against a build
//! from before the fix.

#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use leviculum_std::process::spawn_supervised;

const BTVIRT: &str = "/usr/local/bin/btvirt";

/// How long two fresh lnsd get to find each other and handshake, the
/// budget `ble_returning_peer_resight` gives a fresh room.
const LINK_WAIT: Duration = Duration::from_secs(60);

/// How long the adapter objects get to leave and to come back once
/// btvirt stops or starts; bluetoothd follows the kernel's index events
/// within a second here.
const ADAPTER_WAIT: Duration = Duration::from_secs(15);

/// How long the controllers stay away: longer than a scan window, so the
/// loss is not a blink.
const ABSENCE: Duration = Duration::from_secs(3);

/// From the adapters' return to `BLE_ADAPTER_BACK`: one re-resolution
/// interval (the default `discovery_interval`, 5 s) plus the session's
/// setup calls, with slack for bluetoothd powering the fresh controller.
const BACK_WAIT: Duration = Duration::from_secs(10);

/// The release `lnsd`, resolved the way the mvrs resolve it, and refused
/// when it is older than the BLE interface under test (same rule and
/// reason as `ble_link_gap_floor`).
fn release_lnsd() -> PathBuf {
    if let Some(bin) = std::env::var_os("ADAPTER_RETURN_LNSD") {
        return PathBuf::from(bin);
    }
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

/// Whether bluetoothd holds `adapter`'s `Adapter1` object. A property
/// read, not `busctl introspect`: bluetoothd answers an introspection of
/// a path it no longer holds, so that check never sees an adapter leave.
fn bluez_has(adapter: &str) -> bool {
    Command::new("busctl")
        .args([
            "get-property",
            "org.bluez",
            &format!("/org/bluez/{adapter}"),
            "org.bluez.Adapter1",
            "Address",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Forget every device BlueZ cached on `adapter`: btvirt hands out the
/// same addresses every time, and a device object left from a previous
/// room is stale state this test did not make.
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
    format!(
        "[reticulum]\n  enable_transport = no\n  share_instance = yes\n  \
         instance_name = adapter_return_{pid}_{k}\n\n[logging]\n  loglevel = 4\n\n\
         [interfaces]\n  [[BLE Room]]\n    type = BLEInterface\n    enabled = yes\n    \
         device = {adapter}\n",
        pid = std::process::id()
    )
}

/// btvirt, the two daemons and the D-Bus capture; killed in reverse on
/// drop.
struct Room {
    btvirt: Option<Child>,
    daemons: Vec<Child>,
    monitor: Option<Child>,
    adapters: Vec<String>,
    dir: tempfile::TempDir,
}

impl Drop for Room {
    fn drop(&mut self) {
        for d in &mut self.daemons {
            let _ = d.kill();
            let _ = d.wait();
        }
        for adapter in &self.adapters {
            forget_devices(adapter);
        }
        if let Some(mut btvirt) = self.btvirt.take() {
            let _ = btvirt.kill();
            let _ = btvirt.wait();
        }
        if let Some(mut monitor) = self.monitor.take() {
            let _ = monitor.kill();
            let _ = monitor.wait();
        }
        if let Ok(keep) = std::env::var("ADAPTER_RETURN_KEEP") {
            let _ = std::fs::create_dir_all(&keep);
            for name in ["n0.log", "n1.log", "btvirt.log", "dbus.log"] {
                let _ = std::fs::copy(self.dir.path().join(name), Path::new(&keep).join(name));
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

    fn start_btvirt(&mut self) {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.path().join("btvirt.log"))
            .expect("btvirt log");
        let mut cmd = Command::new(BTVIRT);
        cmd.args(["-L", "-l2"])
            .stdin(Stdio::null())
            .stdout(log.try_clone().expect("clone"))
            .stderr(log);
        self.btvirt = Some(spawn_supervised(cmd).expect("spawn btvirt"));
    }

    fn stop_btvirt(&mut self) {
        if let Some(mut btvirt) = self.btvirt.take() {
            let _ = btvirt.kill();
            let _ = btvirt.wait();
        }
    }

    /// Wait until bluetoothd holds (`present`) or no longer holds every
    /// room adapter.
    fn wait_adapters(&self, present: bool) {
        let deadline = Instant::now() + ADAPTER_WAIT;
        while !self.adapters.iter().all(|a| bluez_has(a) == present) {
            assert!(
                Instant::now() < deadline,
                "bluetoothd did not {} {:?} within {ADAPTER_WAIT:?}",
                if present { "re-register" } else { "drop" },
                self.adapters
            );
            thread::sleep(Duration::from_millis(100));
        }
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
    // bluetoothd's ObjectManager signals, the adapters' leaving and
    // returning as D-Bus shows them. Broadcast signals, so a plain match
    // rule receives them without eavesdropping rights.
    let monitor = std::fs::File::create(dir.path().join("dbus.log"))
        .ok()
        .and_then(|log| {
            let mut cmd = Command::new("dbus-monitor");
            cmd.args([
                "--system",
                "type='signal',sender='org.bluez',interface='org.freedesktop.DBus.ObjectManager'",
            ])
            .stdin(Stdio::null())
            .stdout(log.try_clone().ok()?)
            .stderr(log);
            spawn_supervised(cmd).ok()
        });
    let mut room = Room {
        btvirt: None,
        daemons: Vec::new(),
        monitor,
        adapters: Vec::new(),
        dir,
    };
    room.start_btvirt();
    let deadline = Instant::now() + ADAPTER_WAIT;
    room.adapters = loop {
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
    for k in 0..2 {
        let adapter = room.adapters[k].clone();
        forget_devices(&adapter);
        let cdir = room.dir.path().join(format!("c{k}"));
        std::fs::create_dir_all(&cdir).expect("config dir");
        std::fs::write(cdir.join("config"), config(k, &adapter)).expect("config");
        let log = std::fs::File::create(room.dir.path().join(format!("n{k}.log"))).expect("log");
        let mut cmd = Command::new(lnsd);
        cmd.arg("--config")
            .arg(&cdir)
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(log.try_clone().expect("clone"))
            .stderr(log);
        room.daemons
            .push(spawn_supervised(cmd).expect("spawn lnsd"));
    }
    room
}

/// Wait until node `node`'s log holds at least `n` lines matching `pred`,
/// or panic with both logs after `wait`.
fn wait_for_lines(
    room: &Room,
    node: usize,
    wait: Duration,
    n: usize,
    what: &str,
    pred: impl Fn(&str) -> bool,
) {
    let deadline = Instant::now() + wait;
    loop {
        if room.log(node).lines().filter(|l| pred(l)).count() >= n {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "node {node}: no {what} within {wait:?}\n--- node 0 ---\n{}\n--- node 1 ---\n{}",
            room.log(0),
            room.log(1),
        );
        thread::sleep(Duration::from_millis(250));
    }
}

#[test]
#[ignore = "needs btvirt + bluetoothd + a fresh release lnsd (host-global Bluetooth state); run by name"]
fn a_lost_adapter_is_found_again_and_links_return() {
    let lnsd = release_lnsd();
    let mut room = open_room(&lnsd);

    let up = |l: &str| l.contains("BLE_LINK_UP");
    for k in 0..2 {
        wait_for_lines(&room, k, LINK_WAIT, 1, "BLE_LINK_UP", up);
    }

    // The controllers leave: bluetoothd removes /org/bluez/hciN. No
    // daemon can see the loss before this instant.
    let stopped = Instant::now();
    room.stop_btvirt();
    room.wait_adapters(false);
    thread::sleep(ABSENCE);

    // And return at the same indices and addresses.
    room.start_btvirt();
    room.wait_adapters(true);
    let away = stopped.elapsed();

    // The behaviour first: the room links again, within the bound a lost
    // adapter is re-resolved in plus the budget of a fresh room. Before
    // the fix neither daemon ever did.
    for k in 0..2 {
        wait_for_lines(
            &room,
            k,
            BACK_WAIT + LINK_WAIT,
            2,
            "BLE_LINK_UP after the adapter returned",
            up,
        );
    }

    // Then the mechanism, as the logs name it: the loss, the links
    // released through the link-down path, and the re-resolution. All of
    // them are in the log by now; the waits only read.
    for k in 0..2 {
        wait_for_lines(&room, k, BACK_WAIT, 1, "BLE_ADAPTER_LOST", |l| {
            l.contains("BLE_ADAPTER_LOST")
        });
        // Each link went down, by whichever path saw it first: the loss
        // releases it as `reason=adapter_lost`, unless BlueZ closed the
        // peripheral's write socket a few milliseconds before bluetoothd
        // announced the adapter's removal (`reason=disconnected`).
        wait_for_lines(&room, k, BACK_WAIT, 1, "BLE_LINK_DOWN", |l| {
            l.contains("BLE_LINK_DOWN")
        });
        wait_for_lines(&room, k, BACK_WAIT, 1, "BLE_ADAPTER_BACK", |l| {
            l.contains("BLE_ADAPTER_BACK")
        });
    }

    // The re-resolution's own bound: `down_ms` counts from the loss,
    // which came after `stopped`, so it is at most the time the adapters
    // were away plus one re-resolution interval and the setup.
    let absence_and_back = away + BACK_WAIT;
    for k in 0..2 {
        let log = room.log(k);
        let back = |key: &str| -> u64 {
            log.lines()
                .find(|l| l.contains("BLE_ADAPTER_BACK"))
                .and_then(|l| {
                    l.split_whitespace()
                        .find_map(|w| w.strip_prefix(key))
                        .and_then(|v| v.trim_matches('"').parse().ok())
                })
                .unwrap_or_else(|| panic!("node {k}: BLE_ADAPTER_BACK without {key}\n{log}"))
        };
        let down_ms = back("down_ms=");
        assert!(
            Duration::from_millis(down_ms) <= absence_and_back,
            "node {k}: adapter back after {down_ms} ms, bound {absence_and_back:?}"
        );
        // Each node held a link to the other, so each owes its peer's
        // stored device object a removal, whichever path took the link
        // down: released at the loss, or asked for at the departure a
        // moment before it, inside bluetoothd's deferred disconnect.
        // Unpaid, bluetoothd restores the object with the adapter and a
        // dial at it finds no Columba layout.
        assert!(
            back("forgot=") >= 1,
            "node {k}: the returning session removed no stored peer object\n{log}"
        );
    }
}
