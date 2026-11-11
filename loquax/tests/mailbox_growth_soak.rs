//! The mailbox's growth soak (order 1585): does `loquax mailbox run` grow
//! with uptime under the load a field mailbox sees?
//!
//! loquax-mailbox held about 630 MiB (RSS and swap) on miauhaus after 28 h
//! and grew about 22 MB/h, 366 kB/min. What a home on a transport hears
//! all day is the network's LXMF announces: the lnsd beside it knew 80 000
//! destinations. This soak is that load on one host:
//!
//! ```text
//!   loquax mailbox run (the binary under test, its own process)
//!        │  \0rns/<name>
//!   shared instance (in process)
//!        │  \0rns/<name>
//!   announcer (in process)   every second MAILBOX_SOAK_RATE new lxmf.delivery
//!                            destinations, each with a name, never seen before
//! ```
//!
//! Every `MAILBOX_SOAK_SAMPLE_SECS` the soak sends SIGUSR1 and reads the
//! dump's `MAILBOX_CENSUS` line, the nodes' `=== Node total estimated ===`
//! and the process's `VmRSS` plus `VmSwap`, and prints one
//! `MAILBOX_SOAK_SAMPLE` line. At the end it prints the least-squares slope
//! of each column, the node tables and the anonymous footprint over the
//! samples after the first third (the warm-up, where every table fills to
//! its ceiling), as `MAILBOX_GROWTH table=<name> kb_per_min=<slope>`.
//!
//! **The verdict:** red when the anonymous footprint or the tracked total
//! (mailbox tables plus node tables) rises faster than
//! `MAILBOX_SOAK_MAX_KB_PER_MIN` (default 20 kB/min, 1.2 MB/h, a
//! twentieth of the field's rate). Red too with fewer than four samples
//! after the warm-up: then the soak proved nothing.
//!
//! Run it with `just mailbox-growth-soak` (release, two hours, a sample
//! every ten minutes). Knobs: `MAILBOX_SOAK_SECS`,
//! `MAILBOX_SOAK_SAMPLE_SECS`, `MAILBOX_SOAK_RATE`,
//! `MAILBOX_SOAK_MAX_KB_PER_MIN`. What it does not load: a device syncing
//! (the 1489 fixture and the bench board of 1562), which the field host had
//! too; the census names its tables (`wakes`, `costs`), so a soak with a
//! board attached reads the same lines.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use leviculum_core::{Destination, DestinationType, Direction};
use leviculum_std::driver::ReticulumNodeBuilder;

mod support;

const LOQUAX: &str = env!("CARGO_BIN_EXE_loquax");

fn knob(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// `VmRSS` plus `VmSwap` of `pid`, in kB.
fn anon_kb(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let field = |name: &str| -> u64 {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    };
    field("VmRSS:") + field("VmSwap:")
}

/// `key=<n>` out of `line`.
fn value(line: &str, key: &str) -> Option<u64> {
    line.split_whitespace()
        .find_map(|word| word.strip_prefix(key)?.strip_prefix('='))
        .and_then(|n| n.parse().ok())
}

/// The least-squares slope of `ys` (bytes or kB) against `xs` (minutes).
fn slope(xs: &[f64], ys: &[f64]) -> f64 {
    let n = xs.len() as f64;
    let mx = xs.iter().sum::<f64>() / n;
    let my = ys.iter().sum::<f64>() / n;
    let num: f64 = xs.iter().zip(ys).map(|(x, y)| (x - mx) * (y - my)).sum();
    let den: f64 = xs.iter().map(|x| (x - mx).powi(2)).sum();
    if den == 0.0 {
        0.0
    } else {
        num / den
    }
}

struct Sample {
    minute: f64,
    anon_kb: u64,
    columns: Vec<(String, u64)>,
    node_bytes: u64,
    /// Every node table's estimate, summed over the nodes, by name.
    node_tables: std::collections::BTreeMap<String, u64>,
}

/// A node census line, `<table>: <n> entries, raw <r> bytes, estimated
/// <e> bytes (...)`, as its table's name and estimate.
fn node_table(line: &str) -> Option<(String, u64)> {
    let (name, rest) = line.split_once(": ")?;
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let estimated = rest.split("estimated ").nth(1)?;
    let bytes = estimated.split_whitespace().next()?.parse().ok()?;
    Some((name.to_string(), bytes))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "a soak of two hours by default: just mailbox-growth-soak"]
async fn the_mailbox_does_not_grow_with_uptime() {
    let secs = knob("MAILBOX_SOAK_SECS", 7200);
    let sample_secs = knob("MAILBOX_SOAK_SAMPLE_SECS", 600);
    let rate = knob("MAILBOX_SOAK_RATE", 2);
    let max_kb_per_min = knob("MAILBOX_SOAK_MAX_KB_PER_MIN", 20) as f64;

    let daemon = support::daemon("soak", |b| b).await;
    let home = tempfile::tempdir().expect("loquax home");
    let mut command = Command::new(LOQUAX);
    command
        .args(["mailbox", "run", "--instance", &daemon.instance, "--config"])
        .arg(daemon.config_dir.path())
        .args(["--sync-interval", "off"])
        .env("LNMSG_HOME", home.path())
        .env_remove("LEVICULUM_EVENT_LOG")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = leviculum_std::process::spawn_supervised(command).expect("spawn the mailbox");
    let pid = child.id();
    // Only the dump's lines are kept: the log of a two-hour run is not.
    let dumps = Arc::new(Mutex::new(Vec::<String>::new()));
    let up = Arc::new(Mutex::new(false));
    let reader = {
        let dumps = Arc::clone(&dumps);
        let up = Arc::clone(&up);
        let pipe = child.stderr.take().expect("stderr pipe");
        std::thread::spawn(move || {
            let mut current = String::new();
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                // SIGUSR1 is handled from here (1691); `HOME up` matched
                // `HOME upstream`, logged before the handler.
                if line.contains("HOME signals SIGUSR1=census") {
                    *up.lock().expect("up") = true;
                }
                if line.starts_with("MAILBOX_CENSUS ")
                    || line.starts_with("=== Node total estimated: ")
                    || node_table(&line).is_some()
                {
                    current.push_str(&line);
                    current.push('\n');
                }
                if line.starts_with("gap:") {
                    dumps
                        .lock()
                        .expect("dumps")
                        .push(std::mem::take(&mut current));
                }
            }
        })
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while !*up.lock().expect("up") {
        assert!(Instant::now() < deadline, "the mailbox never came up");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // The network: new named destinations every second.
    let storage = tempfile::tempdir().expect("announcer storage");
    let mut announcer = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(&daemon.instance)
        .storage_path(storage.path().to_path_buf())
        .build_sync()
        .expect("build the announcer");
    announcer.start().await.expect("attach the announcer");

    let started = Instant::now();
    let mut next_sample = started;
    let mut samples: Vec<Sample> = Vec::new();
    let mut announced = 0u64;
    while started.elapsed() < Duration::from_secs(secs) {
        for _ in 0..rate {
            let destination = Destination::new(
                Some(leviculum_std::generate_identity()),
                Direction::In,
                DestinationType::Single,
                "lxmf",
                &["delivery"],
            )
            .expect("a delivery destination");
            let hash = *destination.hash();
            announcer.register_destination(destination);
            let name = format!("soak peer {announced}");
            let app_data = leviculum_lxmf::announce::delivery(Some(name.as_bytes()), None);
            let _ = announcer.announce_destination(&hash, Some(&app_data)).await;
            // The announcer keeps nothing it does not need.
            announcer.unregister_destination(&hash);
            announced += 1;
        }
        if Instant::now() >= next_sample {
            next_sample += Duration::from_secs(sample_secs);
            let before = dumps.lock().expect("dumps").len();
            let _ = Command::new("kill")
                .args(["-s", "USR1", &pid.to_string()])
                .status();
            let wait = Instant::now() + Duration::from_secs(10);
            while dumps.lock().expect("dumps").len() == before && Instant::now() < wait {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let dump = dumps
                .lock()
                .expect("dumps")
                .last()
                .cloned()
                .unwrap_or_default();
            let census = dump
                .lines()
                .find(|l| l.starts_with("MAILBOX_CENSUS "))
                .unwrap_or("")
                .to_string();
            let columns = loquax::home::census::COLUMNS
                .iter()
                .map(|c| (c.to_string(), value(&census, c).unwrap_or(0)))
                .collect();
            let node_bytes = dump
                .lines()
                .filter_map(|l| l.strip_prefix("=== Node total estimated: "))
                .filter_map(|r| r.split_whitespace().next()?.parse::<u64>().ok())
                .sum();
            let mut node_tables = std::collections::BTreeMap::new();
            for (name, bytes) in dump.lines().filter_map(node_table) {
                *node_tables.entry(name).or_insert(0) += bytes;
            }
            let sample = Sample {
                minute: started.elapsed().as_secs_f64() / 60.0,
                anon_kb: anon_kb(pid),
                columns,
                node_bytes,
                node_tables,
            };
            println!(
                "MAILBOX_SOAK_SAMPLE minute={:.1} announced={announced} anon_kb={} node_bytes={} {}",
                sample.minute,
                sample.anon_kb,
                sample.node_bytes,
                census.trim_start_matches("MAILBOX_CENSUS ")
            );
            samples.push(sample);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let _ = Command::new("kill")
        .args(["-s", "TERM", &pid.to_string()])
        .status();
    let _ = tokio::task::spawn_blocking(move || child.wait()).await;
    reader.join().expect("reader");

    let warm = samples.len() / 3;
    let window = &samples[warm..];
    assert!(
        window.len() >= 4,
        "{} samples after the warm-up: the soak proved nothing",
        window.len()
    );
    let xs: Vec<f64> = window.iter().map(|s| s.minute).collect();
    let kb = |f: &dyn Fn(&Sample) -> f64| -> f64 {
        slope(&xs, &window.iter().map(f).collect::<Vec<_>>())
    };
    for (i, (name, _)) in window[0].columns.iter().enumerate() {
        println!(
            "MAILBOX_GROWTH table={name} kb_per_min={:.2}",
            kb(&|s: &Sample| s.columns[i].1 as f64 / 1024.0)
        );
    }
    for name in window[0].node_tables.keys() {
        let slope = kb(&|s: &Sample| s.node_tables.get(name).copied().unwrap_or(0) as f64 / 1024.0);
        let last = window[window.len() - 1]
            .node_tables
            .get(name)
            .copied()
            .unwrap_or(0);
        if slope.abs() >= 0.01 || last > 0 {
            println!(
                "MAILBOX_GROWTH table=node:{name} kb_per_min={slope:.2} last_kb={}",
                last / 1024
            );
        }
    }
    let nodes = kb(&|s: &Sample| s.node_bytes as f64 / 1024.0);
    let tracked = kb(&|s: &Sample| {
        (s.node_bytes + s.columns.iter().map(|(_, b)| b).sum::<u64>()) as f64 / 1024.0
    });
    let anon = kb(&|s: &Sample| s.anon_kb as f64);
    println!("MAILBOX_GROWTH table=nodes kb_per_min={nodes:.2}");
    println!("MAILBOX_GROWTH table=tracked kb_per_min={tracked:.2}");
    println!("MAILBOX_GROWTH table=anon kb_per_min={anon:.2}");
    println!(
        "MAILBOX_SOAK_VERDICT anon_kb_per_min={anon:.2} tracked_kb_per_min={tracked:.2} \
         limit={max_kb_per_min} samples={} announced={announced}",
        window.len()
    );
    assert!(
        anon <= max_kb_per_min && tracked <= max_kb_per_min,
        "the mailbox grew with uptime: anon {anon:.2} kB/min, tracked {tracked:.2} kB/min, \
         limit {max_kb_per_min} kB/min"
    );
}
