//! `lndecode` — read Reticulum wire bytes, print every parsed field as JSON.
//!
//! One frame per input line, hex or base64, from a file or stdin; one JSON
//! object per line on stdout, so a capture becomes greppable and `jq`-able
//! without adding a debug print to the daemon. See the crate docs for why the
//! decoder shares no code with the writer.

use std::io::{Read, Write};

use clap::Parser;

#[derive(Parser)]
#[command(
    name = "lndecode",
    about = "Decode Reticulum wire bytes to structured JSON",
    long_about = "Reads one frame per line as hex or base64 and writes one JSON object \
per line. Lines that are empty or start with '#' are skipped, so a capture file \
may carry comments. The decoder is independent of the packet writer: an \
announce's signature and destination hash are recomputed here, not taken on \
trust, and anomalies (hop count above the ceiling, an uptime-seconds emission \
timestamp, a hostile link MTU) are reported in each object's `warnings` array \
rather than refused."
)]
struct Args {
    /// Capture file to read; stdin when absent.
    file: Option<std::path::PathBuf>,

    /// Indent the JSON instead of one object per line.
    #[arg(long)]
    pretty: bool,

    /// Treat the whole input as one frame, newlines and all.
    ///
    /// For a hex dump wrapped across lines. Without it every line is its own
    /// frame, which is what a capture file holds.
    #[arg(long)]
    single: bool,

    /// Unix seconds the emission-timestamp plausibility check compares
    /// against; the host clock when absent. Set it to make a decode of an
    /// archived capture reproducible.
    #[arg(long, value_name = "SECS")]
    now: Option<u64>,
}

fn main() {
    let args = Args::parse();
    let now = args.now.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    });

    let mut text = String::new();
    let read = match &args.file {
        Some(path) => std::fs::File::open(path)
            .and_then(|mut f| f.read_to_string(&mut text))
            .map_err(|e| format!("{}: {e}", path.display())),
        None => std::io::stdin()
            .lock()
            .read_to_string(&mut text)
            .map_err(|e| format!("stdin: {e}")),
    };
    if let Err(e) = read {
        eprintln!("lndecode: {e}");
        std::process::exit(2);
    }

    let frames: Vec<&str> = if args.single {
        vec![text.as_str()]
    } else {
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut failures = 0usize;
    for (index, frame) in frames.iter().enumerate() {
        match decode_one(frame, now) {
            Ok(value) => {
                let rendered = if args.pretty {
                    serde_json::to_string_pretty(&value)
                } else {
                    serde_json::to_string(&value)
                };
                match rendered {
                    Ok(s) => {
                        let _ = writeln!(out, "{s}");
                    }
                    Err(e) => {
                        eprintln!("lndecode: frame {}: {e}", index + 1);
                        failures += 1;
                    }
                }
            }
            Err(e) => {
                eprintln!("lndecode: frame {}: {e}", index + 1);
                failures += 1;
            }
        }
    }
    let _ = out.flush();

    if failures > 0 {
        std::process::exit(1);
    }
    // An empty input is not a decode failure, but a caller that piped the
    // wrong thing should hear about it rather than read an empty success.
    if frames.is_empty() {
        eprintln!("lndecode: no frames on input");
        std::process::exit(1);
    }
}

fn decode_one(text: &str, now: u64) -> Result<serde_json::Value, lndecode::DecodeError> {
    let bytes = lndecode::bytes_from_text(text)?;
    Ok(lndecode::decode(&bytes, now)?.value)
}
