//! When does an LNode answer the radio query (#349), and when does it refuse
//! it as busy?
//!
//! The field bring-up of 2026-09-26 stopped here: `lnsd` pushed the legacy
//! radio config at a T114, read the `RADIO_CONFIG_ACK`, asked the board what
//! it was running 0.3 ms later and got `REFUSE_BUSY` — which
//! `leviculum-std/src/interfaces/serial.rs` reads as "this boot never brought
//! its radio up" and answers by dropping the interface. The driver cannot be
//! fixed on a guess about which of those two frames is the lie, so this tool
//! asks the board directly and prints a table.
//!
//! An example and not a test: it needs a real board on a real port, and its
//! output is a measurement, not an assertion. Everything on the wire comes
//! from `leviculum_core` — the same `build_radio_config_frame`,
//! `encode_radio_query` and HDLC framing the driver and `lnflash` use — so
//! what the board answers here is what it would answer them. No frame is
//! hand-rolled.
//!
//! The one thing deliberately NOT shared with `lnflash::envelope::transact` is
//! its `drain_input()` before each write. The driver had none when this was
//! written, and the field ACK arrived 0.7 ms after the write — too fast for a
//! USB round trip to look like — so leftover bytes in the input buffer were
//! the first suspect, and a tool that always drains cannot see the thing it is
//! looking for. `--drain` and `--peek` turn the two halves of that suspicion
//! on separately. (The driver drains now —
//! `leviculum-std/src/interfaces/serial.rs::drain_stale_input` — which is why
//! this tool keeps the arms: it is the only way left to observe an undrained
//! port.)
//!
//! What it answered, run on 2026-09-26 with no drain and no peek (so: exactly
//! the driver's sequence). The last two rows are the same afternoon's
//! re-measurement of the two boards the field `--set-tx-power` could not read,
//! `--peek` and no `--config`, so no field board's stored profile was touched:
//!
//! | board                    | config ACK | query, every delay asked |
//! |--------------------------|------------|--------------------------|
//! | `DEC9947DAD9D2869`       | 0.425 ms   | report                   |
//! | `183004F712B4A7FE`       | 0.630 ms   | `REFUSE_BUSY`            |
//! | `ABFAB3F1807E459B` (RAK) | not sent   | `REFUSE_BUSY` in 0.31 ms |
//! | `183004F712B4A7FE`       | not sent   | `REFUSE_BUSY` in 0.30 ms |
//!
//! So a sub-millisecond ACK is ordinary on this link and was never evidence of
//! stale bytes; and the refusal is a state and not a race — it lands in about
//! 0.3 ms, at every delay, ten seconds apart. `--peek` said `input buffer
//! empty` on both boards, once more against stale bytes.
//!
//! **The media report is what turns the refusal into an action, and only it.**
//! Both refusing boards answered `running_lora=0 configured_lora=1` at 14:41
//! and 14:45 CEST: the carrier IS on their stored page, this boot just never
//! started its LoRa task, so a reset is the entire remedy and a flash would
//! change nothing. Their debug ports say `[MEDIA] lora=off ble=on src=flash`
//! — which is the *running* half by construction
//! (`leviculum-nrf/src/media.rs::log_banner`), so reading a stored `lora=off`
//! out of that line is reading a field the line does not carry. Ask the media
//! query.
//!
//! ```text
//! radio_query_timing <port> [--config] [--reset] [--drain] [--peek]
//!                           [--delays 0,200,1000,3000,10000] [--window-ms 2000]
//! ```

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use leviculum_core::envelope::{
    decode_frame, decode_media_report_payload, decode_radio_report_payload, decode_refusal_payload,
    encode_media_query, encode_radio_query, encode_reset, REFUSE_BUSY, REFUSE_NOT_RUNNING,
    TYPE_MEDIA_REPORT, TYPE_RADIO_QUERY, TYPE_RADIO_REPORT, TYPE_REFUSAL,
};
use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use leviculum_core::rnode::{build_radio_config_frame, RADIO_CONFIG_ACK};
use lnflash::radio::{RadioSettings, EU868};
use lnflash::sys::Fd;

/// How long one write may take before the port counts as gone. The value
/// `lnflash::envelope` uses; a control frame is a few dozen bytes.
const WRITE_WITHIN: Duration = Duration::from_secs(2);

/// How long to keep trying to reopen the port after a reset before giving up.
/// The CDC device re-enumerates, so the old descriptor is dead and the new one
/// does not exist for a second or two.
const REOPEN_WITHIN: Duration = Duration::from_secs(20);

/// How often to retry the reopen. Small on purpose: the interesting question
/// after a reset is what the board answers to the EARLIEST attach a host can
/// make, and a long sleep would hide a window the firmware closes in the
/// meantime.
const REOPEN_EVERY: Duration = Duration::from_millis(100);

struct Args {
    port: String,
    config: bool,
    reset: bool,
    drain: bool,
    peek: bool,
    delays_ms: Vec<u64>,
    window: Duration,
}

fn parse_args() -> Result<Args, String> {
    let mut port = None;
    let mut out = Args {
        port: String::new(),
        config: false,
        reset: false,
        drain: false,
        peek: false,
        delays_ms: vec![0, 200, 1_000, 3_000, 10_000],
        window: Duration::from_millis(2_000),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => out.config = true,
            "--reset" => out.reset = true,
            "--drain" => out.drain = true,
            "--peek" => out.peek = true,
            "--delays" => {
                let list = args.next().ok_or("--delays needs a comma-separated list")?;
                let mut parsed = Vec::new();
                for part in list.split(',') {
                    parsed.push(
                        part.trim()
                            .parse::<u64>()
                            .map_err(|e| format!("--delays {part}: {e}"))?,
                    );
                }
                out.delays_ms = parsed;
            }
            "--window-ms" => {
                let ms = args.next().ok_or("--window-ms needs a value")?;
                out.window =
                    Duration::from_millis(ms.parse().map_err(|e| format!("--window-ms: {e}"))?);
            }
            other if other.starts_with("--") => return Err(format!("unknown flag {other}")),
            other => port = Some(other.to_string()),
        }
    }
    out.port = port.ok_or("no port given")?;
    Ok(out)
}

/// The profile the field base asks for: the ReticulumNet EU868 consensus at
/// 20 dBm, which is verbatim what `/home/lew/feld/config` carries.
fn field_settings() -> RadioSettings {
    RadioSettings {
        tx_power_dbm: 20,
        ..EU868
    }
}

/// What one frame off the board is, in one line. Every frame is named, not
/// just the answer being waited for: a stale `RADIO_CONFIG_ACK` sitting in the
/// input buffer is the whole question here, and a classifier that only looks
/// for reports would drop it silently.
fn describe(data: &[u8]) -> String {
    if data == RADIO_CONFIG_ACK {
        return "legacy-radio-config-ack".to_string();
    }
    let Ok(f) = decode_frame(data) else {
        return format!("unparsable {} bytes: {:02x?}", data.len(), data);
    };
    match f.frame_type {
        TYPE_RADIO_REPORT => match decode_radio_report_payload(f.payload) {
            Some(w) => format!(
                "radio-report freq={} bw={} sf={} cr={} txp={} preamble={} csma={}",
                w.frequency_hz,
                w.bandwidth_hz,
                w.sf,
                w.cr,
                w.tx_power_dbm,
                w.preamble_len,
                w.csma_enabled
            ),
            None => "radio-report (payload is not a radio config block)".to_string(),
        },
        TYPE_REFUSAL => match decode_refusal_payload(f.payload) {
            Some((refused, reason)) => {
                let name = match reason {
                    REFUSE_BUSY => "BUSY",
                    REFUSE_NOT_RUNNING => "NOT_RUNNING",
                    _ => "other",
                };
                format!("refusal of type=0x{refused:02x} reason=0x{reason:02x} ({name})")
            }
            None => "refusal (payload does not parse)".to_string(),
        },
        TYPE_MEDIA_REPORT => match decode_media_report_payload(f.payload) {
            Some((running, configured)) => format!(
                "media-report running_lora={} running_ble={} configured_lora={} \
                 configured_ble={}",
                u8::from(running.lora_enabled),
                u8::from(running.ble_enabled),
                u8::from(configured.lora_enabled),
                u8::from(configured.ble_enabled)
            ),
            None => "media-report (payload is not two known flag bytes)".to_string(),
        },
        t => format!("envelope type=0x{t:02x} payload={} bytes", f.payload.len()),
    }
}

/// Is this frame the radio query's own answer — the thing the driver decides
/// on — rather than some other traffic that happened to arrive?
fn answers_the_query(data: &[u8]) -> bool {
    let Ok(f) = decode_frame(data) else {
        return false;
    };
    match f.frame_type {
        TYPE_RADIO_REPORT => true,
        TYPE_REFUSAL => {
            matches!(decode_refusal_payload(f.payload), Some((refused, _)) if refused == TYPE_RADIO_QUERY)
        }
        _ => false,
    }
}

/// Read frames until one satisfies `decisive`, or `window` runs out. Returns
/// every frame seen, so the caller can print the ones it did not decide on.
fn frames_until(
    fd: &Fd,
    deframer: &mut Deframer,
    window: Duration,
    decisive: impl Fn(&[u8]) -> bool,
) -> std::io::Result<Vec<Vec<u8>>> {
    let deadline = Instant::now() + window;
    let mut seen = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(seen);
        }
        let Some(chunk) = fd.read_available(remaining)? else {
            // EOF: the board went away (a reset re-enumerating the CDC).
            return Ok(seen);
        };
        let mut done = false;
        for result in deframer.process(&chunk) {
            if let DeframeResult::Frame(data) = result {
                done |= decisive(&data);
                seen.push(data);
            }
        }
        if done {
            return Ok(seen);
        }
    }
}

fn open(port: &str) -> std::io::Result<Fd> {
    let fd = Fd::open_serial(Path::new(port))?;
    fd.set_transport_port()?;
    Ok(fd)
}

fn run(args: &Args) -> Result<(), String> {
    let settings = field_settings();
    println!("port      {}", args.port);
    println!(
        "arms      config={} reset={} drain={} peek={} window={:?}",
        args.config, args.reset, args.drain, args.peek, args.window
    );
    println!("requested {}", settings.describe());

    let mut fd = open(&args.port).map_err(|e| format!("open {}: {e}", args.port))?;

    if args.reset {
        let mut framed = Vec::new();
        frame(&encode_reset(), &mut framed);
        fd.write_all(&framed, Instant::now() + WRITE_WITHIN)
            .map_err(|e| format!("reset write: {e}"))?;
        let sent = Instant::now();
        drop(fd);
        // Reattach as early as the port allows, which is the earliest a
        // daemon's reconnect loop could: whether the LoRa task has published
        // a profile by then is the whole question a retry would answer.
        let give_up = sent + REOPEN_WITHIN;
        // First wait for the device to GO: the by-id symlink still resolves
        // for a few ms after the reset frame, so an open that succeeds
        // immediately has caught the dying descriptor and every write on it
        // fails with EIO once the board actually drops off the bus.
        while Path::new(&args.port).exists() {
            if Instant::now() >= give_up {
                return Err(format!("{} never went away after the reset", args.port));
            }
            std::thread::sleep(REOPEN_EVERY);
        }
        loop {
            match open(&args.port) {
                Ok(open_fd) => {
                    fd = open_fd;
                    break;
                }
                Err(e) if Instant::now() >= give_up => {
                    return Err(format!("reopen {} after reset: {e}", args.port))
                }
                Err(_) => std::thread::sleep(REOPEN_EVERY),
            }
        }
        println!(
            "reset     envelope TYPE_RESET sent; port reopened {:.0} ms later",
            sent.elapsed().as_secs_f64() * 1e3
        );
    }

    // One deframer for the whole session, like `transact`: a late answer to
    // an earlier frame is still bytes on the same stream, and starting over
    // would cut a frame in half.
    let mut deframer = Deframer::new();

    if args.peek {
        // What the board had already written before this tool sent anything.
        // Non-empty here is the stale-bytes hypothesis proven.
        let pending = fd
            .read_available(Duration::from_millis(300))
            .map_err(|e| format!("peek: {e}"))?;
        match pending {
            None => println!("peek      EOF"),
            Some(bytes) if bytes.is_empty() => println!("peek      input buffer empty"),
            Some(bytes) => {
                println!("peek      {} stale bytes already queued", bytes.len());
                for r in deframer.process(&bytes) {
                    if let DeframeResult::Frame(data) = r {
                        println!("peek      stale frame: {}", describe(&data));
                    }
                }
            }
        }
    }

    // t0 — what every delay below is measured from. The config's ACK when
    // there is a config, otherwise the moment the port was ready.
    let mut t0 = Instant::now();

    if args.config {
        if args.drain {
            fd.drain_input().map_err(|e| format!("drain: {e}"))?;
        }
        let payload = build_radio_config_frame(&settings.to_wire());
        let mut framed = Vec::new();
        frame(&payload, &mut framed);
        let sent = Instant::now();
        fd.write_all(&framed, Instant::now() + WRITE_WITHIN)
            .map_err(|e| format!("config write: {e}"))?;
        let seen = frames_until(&fd, &mut deframer, args.window, |d| d == RADIO_CONFIG_ACK)
            .map_err(|e| format!("config ACK wait: {e}"))?;
        t0 = Instant::now();
        let acked = seen.iter().any(|d| d.as_slice() == RADIO_CONFIG_ACK);
        println!(
            "config    legacy radio config sent; ack={} after {:.3} ms",
            acked,
            (t0 - sent).as_secs_f64() * 1e3
        );
        for data in &seen {
            println!("config    saw: {}", describe(data));
        }
    }

    println!();
    println!("{:>9}  {:>9}  answer", "asked_at", "answer_in");
    for delay in &args.delays_ms {
        let due = t0 + Duration::from_millis(*delay);
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        }
        if args.drain {
            fd.drain_input().map_err(|e| format!("drain: {e}"))?;
        }
        let mut framed = Vec::new();
        frame(&encode_radio_query(), &mut framed);
        let asked = Instant::now();
        fd.write_all(&framed, Instant::now() + WRITE_WITHIN)
            .map_err(|e| format!("query write: {e}"))?;
        let seen = frames_until(&fd, &mut deframer, args.window, answers_the_query)
            .map_err(|e| format!("query wait: {e}"))?;
        let elapsed = asked.elapsed();
        let answer = seen.iter().find(|d| answers_the_query(d));
        println!(
            "{:>8.0}   {:>8.3}   {}",
            (asked - t0).as_secs_f64() * 1e3,
            elapsed.as_secs_f64() * 1e3,
            match answer {
                Some(data) => describe(data),
                None => format!("SILENCE (nothing in {:?})", args.window),
            }
        );
        for data in seen.iter().filter(|d| !answers_the_query(d)) {
            println!("{:>8}   {:>8}   also: {}", "", "", describe(data));
        }
    }

    // The follow-up question, always asked, because it is the one that turns a
    // refused radio query into something to do about it — and because the
    // driver now asks it too (`serial.rs::ask_media_profile`), so this is the
    // exchange it will have with this board.
    println!();
    let mut framed = Vec::new();
    frame(&encode_media_query(), &mut framed);
    let asked = Instant::now();
    fd.write_all(&framed, Instant::now() + WRITE_WITHIN)
        .map_err(|e| format!("media query write: {e}"))?;
    let seen = frames_until(
        &fd,
        &mut deframer,
        args.window,
        |d| matches!(decode_frame(d), Ok(f) if f.frame_type == TYPE_MEDIA_REPORT),
    )
    .map_err(|e| format!("media query wait: {e}"))?;
    match seen
        .iter()
        .find(|d| matches!(decode_frame(d), Ok(f) if f.frame_type == TYPE_MEDIA_REPORT))
    {
        Some(data) => println!(
            "media     {} (in {:.3} ms)",
            describe(data),
            asked.elapsed().as_secs_f64() * 1e3
        ),
        None => println!("media     no media report within {:?}", args.window),
    }
    Ok(())
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("radio_query_timing: {e}");
            eprintln!(
                "usage: radio_query_timing <port> [--config] [--reset] [--drain] [--peek] \
                 [--delays 0,200,1000] [--window-ms 2000]"
            );
            return ExitCode::FAILURE;
        }
    };
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("radio_query_timing: {e}");
            ExitCode::FAILURE
        }
    }
}
