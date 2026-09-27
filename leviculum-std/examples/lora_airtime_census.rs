//! Charge a board capture's LoRa frames with the firmware's own airtime cost.
//!
//! Reads a debug capture from an LNode (the `[T114_TX_FRAME]` and `[LORA] RX`
//! lines) and prints the channel occupancy it implies, grouped by direction,
//! by Reticulum packet type, by frame length and by wall-clock hour.
//!
//! Every millisecond it prints comes from the two functions the firmware
//! itself charges its regulatory ledger with, so a write-up quoting this tool
//! cannot drift from what the board would have counted:
//!
//! * TX: [`frame_airtime_cost_ms`] per keyed frame. `[T114_TX_FRAME] len=` is
//!   the full on-air frame length, the same number `add_airtime` is fed after
//!   a successful `transmit()` (`leviculum-nrf/src/lora.rs:1015-1028`).
//! * RX: [`packet_airtime_ms`] per demodulated packet. `[LORA] RX N bytes`
//!   prints the REASSEMBLED packet length, not the frame length, so the
//!   per-frame header byte and — above `MAX_SINGLE_PAYLOAD` — the second
//!   frame's whole preamble have to be added back. That is exactly what
//!   `packet_airtime_ms` does.
//!
//! The RX side is also totalled a second way, over `[T114_SX_RX] len=`, which
//! is every frame the modem demodulated whether or not it reassembled into a
//! packet the host ever saw. That total is the honest upper bound on what the
//! receiver's ear was busy with; the packet total is what the capture can
//! attribute to a packet type. They differ by the frames that never became a
//! packet, and the gap is worth printing rather than choosing between.
//!
//! Pricing an RX packet with the TX function understates a split packet by a
//! full preamble plus a header byte; pricing either with a hand-rolled copy of
//! the Semtech formula is how the 2026-09-27 field write-up came to charge a
//! 119 B frame 126 ms instead of 380 ms (its transcription lost the `+4` in
//! the `(CR-4)+4` coding-rate factor, so every code group cost one symbol
//! instead of five).
//!
//! # Usage
//!
//! ```sh
//! cargo run -p leviculum-std --example lora_airtime_census -- \
//!     <capture.log> [<date> <from-utc> <to-utc>] [--sf N] [--bw HZ] \
//!     [--cr N] [--preamble N]
//! ```
//!
//! The window bounds are matched against the capture's own
//! `YYYY-MM-DDTHH:MM:SS` line prefix and are inclusive/exclusive, e.g.
//! `2026-09-27 09:20:00 14:15:00`. Without them the whole file is charged.
//! The PHY defaults to the field default (869.463 MHz band: BW 125 kHz, SF8,
//! CR 4/5) with the preamble the RNode derivation yields there, 18 symbols.

use std::collections::BTreeMap;

use leviculum_core::rnode::{frame_airtime_cost_ms, packet_airtime_ms};

/// Reticulum packet type, bits 0-1 of the flags byte
/// (`leviculum-core/src/packet.rs:15,37-46`).
const TYPE_NAMES: [&str; 4] = ["DATA", "ANNOUNCE", "LINKREQUEST", "PROOF"];

#[derive(Default, Clone)]
struct Bucket {
    frames: u64,
    bytes: u64,
    airtime_ms: u64,
}

impl Bucket {
    fn add(&mut self, bytes: u64, airtime_ms: u64) {
        self.frames += 1;
        self.bytes += bytes;
        self.airtime_ms += airtime_ms;
    }
}

struct Phy {
    bw_hz: u32,
    sf: u8,
    cr: u8,
    preamble: u16,
}

/// One charged event: wall-clock second within the window, and its cost.
struct Event {
    secs: u64,
    airtime_ms: u64,
}

#[derive(Default)]
struct Census {
    /// (direction, packet type) -> bucket
    by_type: BTreeMap<(&'static str, u8), Bucket>,
    /// (direction, packet type, on-air length) -> bucket
    by_len: BTreeMap<(&'static str, u8, u32), Bucket>,
    /// (direction, hour of day) -> bucket
    by_hour: BTreeMap<(&'static str, u32), Bucket>,
    rx_events: Vec<Event>,
    tx_events: Vec<Event>,
    /// Every frame the modem demodulated (`[T114_SX_RX]`), reassembled or not.
    demod: Bucket,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut positional: Vec<String> = Vec::new();
    let mut phy = Phy {
        bw_hz: 125_000,
        sf: 8,
        cr: 5,
        preamble: 18,
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let mut take = |name: &str| -> String {
            i += 1;
            args.get(i)
                .unwrap_or_else(|| panic!("{name} needs a value"))
                .clone()
        };
        match a {
            "--sf" => phy.sf = take("--sf").parse().expect("--sf"),
            "--bw" => phy.bw_hz = take("--bw").parse().expect("--bw"),
            "--cr" => phy.cr = take("--cr").parse().expect("--cr"),
            "--preamble" => phy.preamble = take("--preamble").parse().expect("--preamble"),
            _ => positional.push(a.to_string()),
        }
        i += 1;
    }

    let path = match positional.first() {
        Some(p) => p.clone(),
        None => {
            eprintln!(
                "usage: lora_airtime_census <capture.log> \
                 [<date> <from-utc> <to-utc>] [--sf N] [--bw HZ] [--cr N] [--preamble N]"
            );
            std::process::exit(2);
        }
    };
    let window = match positional.len() {
        1 => None,
        4 => Some((
            positional[1].clone(),
            positional[2].clone(),
            positional[3].clone(),
        )),
        n => {
            eprintln!("expected 1 or 4 positional arguments, got {n}");
            std::process::exit(2);
        }
    };

    let raw = std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
    let text = String::from_utf8_lossy(&raw);

    let mut census = Census::default();
    for line in text.lines() {
        let Some(secs) = line_time(line, window.as_ref()) else {
            continue;
        };
        if let Some((flags, frame_len)) = parse_tx(line) {
            charge(
                &mut census,
                "TX",
                flags,
                frame_len,
                frame_airtime_cost_ms(frame_len, phy.bw_hz, phy.sf, phy.cr, phy.preamble),
                secs,
            );
        } else if let Some(frame_len) = parse_sx_rx(line) {
            census.demod.add(
                frame_len as u64,
                frame_airtime_cost_ms(frame_len, phy.bw_hz, phy.sf, phy.cr, phy.preamble),
            );
        } else if let Some((flags, packet_len)) = parse_rx(line) {
            charge(
                &mut census,
                "RX",
                flags,
                packet_len,
                packet_airtime_ms(packet_len as usize, phy.bw_hz, phy.sf, phy.cr, phy.preamble),
                secs,
            );
        }
    }

    report(&census, &phy, window.as_ref());
}

fn charge(census: &mut Census, dir: &'static str, flags: u8, len: u32, airtime_ms: u64, secs: u64) {
    let ptype = flags & 0x03;
    census
        .by_type
        .entry((dir, ptype))
        .or_default()
        .add(len as u64, airtime_ms);
    census
        .by_len
        .entry((dir, ptype, len))
        .or_default()
        .add(len as u64, airtime_ms);
    census
        .by_hour
        .entry((dir, (secs / 3600) as u32))
        .or_default()
        .add(len as u64, airtime_ms);
    let event = Event { secs, airtime_ms };
    if dir == "TX" {
        census.tx_events.push(event);
    } else {
        census.rx_events.push(event);
    }
}

/// Seconds-of-day of a capture line, if it is inside the window.
fn line_time(line: &str, window: Option<&(String, String, String)>) -> Option<u64> {
    // `2026-09-27T09:23:50.278+00:00 [...]`
    let bytes = line.as_bytes();
    if bytes.len() < 19 || bytes[10] != b'T' {
        return None;
    }
    let date = &line[..10];
    let hms = &line[11..19];
    if let Some((want_date, from, to)) = window {
        if date != want_date || hms < from.as_str() || hms >= to.as_str() {
            return None;
        }
    }
    let h: u64 = hms[0..2].parse().ok()?;
    let m: u64 = hms[3..5].parse().ok()?;
    let s: u64 = hms[6..8].parse().ok()?;
    Some(h * 3600 + m * 60 + s)
}

/// `[T114_TX_FRAME] first8=<16 hex> len=<n>`: flags byte and on-air length.
fn parse_tx(line: &str) -> Option<(u8, u32)> {
    let rest = line.split_once("[T114_TX_FRAME] first8=")?.1;
    let flags = u8::from_str_radix(rest.get(0..2)?, 16).ok()?;
    let len = rest.split_once(" len=")?.1;
    Some((flags, parse_u32(len)?))
}

/// `[LORA] RX <n> bytes ... flags=0x<hh>`: flags byte and reassembled length.
fn parse_rx(line: &str) -> Option<(u8, u32)> {
    let rest = line.split_once("[LORA] RX ")?.1;
    let len = parse_u32(rest)?;
    if !rest[len.to_string().len()..].starts_with(" bytes") {
        return None;
    }
    let flags = rest.split_once(" flags=0x")?.1;
    let flags = u8::from_str_radix(flags.get(0..2)?, 16).ok()?;
    Some((flags, len))
}

/// `[T114_SX_RX] len=<n> first8=...`: on-air length of a demodulated frame.
fn parse_sx_rx(line: &str) -> Option<u32> {
    parse_u32(line.split_once("[T114_SX_RX] len=")?.1)
}

fn parse_u32(s: &str) -> Option<u32> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Busiest rolling hour over a list of charged events, in (start second, ms).
fn busiest_rolling_hour(events: &[Event]) -> (u64, u64) {
    let mut best = (0u64, 0u64);
    let mut sum = 0u64;
    let mut lo = 0usize;
    for hi in 0..events.len() {
        sum += events[hi].airtime_ms;
        while events[hi].secs.saturating_sub(events[lo].secs) >= 3600 {
            sum -= events[lo].airtime_ms;
            lo += 1;
        }
        if sum > best.1 {
            best = (events[lo].secs, sum);
        }
    }
    best
}

fn hhmmss(secs: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

fn report(census: &Census, phy: &Phy, window: Option<&(String, String, String)>) {
    let window_s: f64 = match window {
        Some((_, from, to)) => {
            let sec = |t: &str| -> f64 {
                let p: Vec<f64> = t.split(':').map(|v| v.parse().unwrap_or(0.0)).collect();
                p[0] * 3600.0 + p[1] * 60.0 + p[2]
            };
            sec(to) - sec(from)
        }
        None => f64::NAN,
    };
    println!(
        "PHY: bw={} Hz sf={} cr=4/{} preamble={} symbols   window={} s",
        phy.bw_hz, phy.sf, phy.cr, phy.preamble, window_s
    );

    let mut totals: BTreeMap<&'static str, Bucket> = BTreeMap::new();
    println!("\n== by direction and packet type ==");
    println!("dir  type         frames      bytes    airtime_s   share_of_dir");
    for dir in ["TX", "RX"] {
        let dir_total: u64 = census
            .by_type
            .iter()
            .filter(|((d, _), _)| *d == dir)
            .map(|(_, b)| b.airtime_ms)
            .sum();
        for ((d, t), b) in census.by_type.iter() {
            if *d != dir {
                continue;
            }
            println!(
                "{d}   {:<11} {:>6} {:>10} {:>11.1} {:>13.1}%",
                TYPE_NAMES[*t as usize],
                b.frames,
                b.bytes,
                b.airtime_ms as f64 / 1000.0,
                100.0 * b.airtime_ms as f64 / dir_total.max(1) as f64,
            );
            let e = totals.entry(dir).or_default();
            e.frames += b.frames;
            e.bytes += b.bytes;
            e.airtime_ms += b.airtime_ms;
        }
    }
    let combined: u64 = totals.values().map(|b| b.airtime_ms).sum();
    for (dir, b) in totals.iter() {
        println!(
            "{dir} total: frames={} bytes={} airtime={:.1} s duty={:.2}%",
            b.frames,
            b.bytes,
            b.airtime_ms as f64 / 1000.0,
            100.0 * b.airtime_ms as f64 / 1000.0 / window_s,
        );
    }
    println!(
        "combined occupancy: {:.1} s = {:.2}% of the window",
        combined as f64 / 1000.0,
        100.0 * combined as f64 / 1000.0 / window_s,
    );
    println!(
        "RX demodulated frames (T114_SX_RX, reassembled or not): {} frames \
         {:.1} s duty={:.2}%",
        census.demod.frames,
        census.demod.airtime_ms as f64 / 1000.0,
        100.0 * census.demod.airtime_ms as f64 / 1000.0 / window_s,
    );

    println!("\n== by length (>= 10 frames) ==");
    println!("dir  type          len  frames    airtime_s   per_frame_ms");
    for (d, t) in census.by_type.keys() {
        for ((d2, t2, len), b) in census.by_len.iter() {
            if d2 != d || t2 != t || b.frames < 10 {
                continue;
            }
            println!(
                "{d}   {:<11} {:>4} {:>7} {:>12.1} {:>14}",
                TYPE_NAMES[*t as usize],
                len,
                b.frames,
                b.airtime_ms as f64 / 1000.0,
                b.airtime_ms / b.frames,
            );
        }
    }

    println!("\n== per hour ==");
    for ((dir, hour), b) in census.by_hour.iter() {
        println!(
            "{hour:02}:00 {dir} {:>5} frames {:>8.1} s  {:.2}% duty",
            b.frames,
            b.airtime_ms as f64 / 1000.0,
            100.0 * b.airtime_ms as f64 / 1000.0 / 3600.0,
        );
    }

    println!("\n== busiest rolling hour ==");
    for (label, events) in [("RX", &census.rx_events), ("TX", &census.tx_events)] {
        let (start, ms) = busiest_rolling_hour(events);
        println!(
            "{label}: {:.1} s starting {}Z",
            ms as f64 / 1000.0,
            hhmmss(start)
        );
    }
    let mut both: Vec<Event> = census
        .rx_events
        .iter()
        .chain(census.tx_events.iter())
        .map(|e| Event {
            secs: e.secs,
            airtime_ms: e.airtime_ms,
        })
        .collect();
    both.sort_by_key(|e| e.secs);
    let (start, ms) = busiest_rolling_hour(&both);
    println!(
        "TX+RX: {:.1} s starting {}Z",
        ms as f64 / 1000.0,
        hhmmss(start)
    );
}
