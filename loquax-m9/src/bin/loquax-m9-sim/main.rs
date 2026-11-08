//! `loquax-m9-sim` (1530): the ThinkNode M9's Loquax on the host, drawn
//! as the board draws it, driven by its keys, in a window or headless.
//!
//! ```text
//! loquax-m9-sim [--store fixture|real|<image>] [--save <image>]
//!               [--keys "down down enter" | --script <file>]
//!               [--reboot-after <n>] [--dump <png>] [--dump-every <n>]
//!               [--text] [--window [--scale 2|3]]
//! ```
//!
//! - `--store`: what the board's flash holds at power-on. `fixture` (the
//!   default) is a board without a client record: the pairing, and Back
//!   leaves it for the fixture model, as on the board. `real` is a device
//!   of a home whose store a first pull of the 1489 fixture filled (300
//!   conversations, 4000 messages). Anything else is a flash image a run
//!   saved with `--save`.
//! - `--save`: write the flash image at the end (and at each reboot), so
//!   the next run's `--store <image>` is a power cycle.
//! - `--keys`, `--script`: the presses ([`script`]).
//! - `--reboot-after <n>`: power-cycle the board on its flash after the
//!   `n`th press, once.
//! - `--dump <png>`: the panel after the script, 320 x 240 as the board's
//!   `FRAME_DUMP` would show it. `--dump-every <n>`: also every `n`th
//!   frame drawn, beside it as `<stem>-f<frame>.png`.
//! - `--text`: the screen's text rows on stdout after the script.
//! - `--window`: the panel in a window at `--scale` (built with the
//!   `sim-window` feature, for the host's own triple).
//!
//! The board's lines (`KBRAW`, `KEY`, `[PAGE]`, `[STORE]`, `[HOME]`,
//! `[DISP]`, `[MAP]`, `[LOOP]`) go to stderr in the firmware's shape. Exit
//! 0 when the script ran, 2 on a usage error, 1 on any other.

mod board;
mod flash;
#[cfg(test)]
mod parity;
mod real;
mod script;
#[cfg(feature = "sim-window")]
mod window;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use board::{Board, Log};
use flash::Flash;
use loquax_m9::frame::{Frame, HEIGHT, WIDTH};
use script::Step;

/// What the command line asked.
#[derive(Debug, Default)]
struct Args {
    store: Option<String>,
    save: Option<PathBuf>,
    steps: Vec<Step>,
    reboot_after: Option<usize>,
    dump: Option<PathBuf>,
    dump_every: Option<u64>,
    text: bool,
    window: bool,
    scale: u32,
}

fn usage() -> &'static str {
    "usage: loquax-m9-sim [--store fixture|real|<image>] [--save <image>] \
     [--keys \"down enter\" | --script <file>] [--reboot-after <n>] \
     [--dump <png>] [--dump-every <n>] [--text] [--window [--scale 2|3]]"
}

fn parse(argv: &[String]) -> Result<Args, String> {
    let mut args = Args {
        scale: 2,
        ..Args::default()
    };
    let mut it = argv.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} wants a value"))
        };
        match arg.as_str() {
            "--store" => args.store = Some(value("--store")?),
            "--save" => args.save = Some(PathBuf::from(value("--save")?)),
            "--keys" => args.steps.extend(script::keys(&value("--keys")?)?),
            "--script" => {
                let path = value("--script")?;
                let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
                args.steps
                    .extend(script::file(&text).map_err(|e| format!("{path}: {e}"))?);
            }
            "--reboot-after" => {
                let n = value("--reboot-after")?;
                args.reboot_after = Some(n.parse().map_err(|_| format!("--reboot-after {n}"))?);
            }
            "--dump" => args.dump = Some(PathBuf::from(value("--dump")?)),
            "--dump-every" => {
                let n = value("--dump-every")?;
                let n: u64 = n.parse().map_err(|_| format!("--dump-every {n}"))?;
                if n == 0 {
                    return Err(String::from("--dump-every wants a count over 0"));
                }
                args.dump_every = Some(n);
            }
            "--text" => args.text = true,
            "--window" => args.window = true,
            "--scale" => {
                let n = value("--scale")?;
                args.scale = match n.as_str() {
                    "1" => 1,
                    "2" => 2,
                    "3" => 3,
                    _ => return Err(format!("--scale {n}: 1, 2 or 3")),
                };
            }
            "--mailbox" => {
                return Err(String::from(
                    "--mailbox: this sim has no node yet; the board's client \
                     (leviculum-esp/src/handheld.rs) is built on the ESP's types",
                ))
            }
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("{other}: no such option")),
        }
    }
    if args.dump_every.is_some() && args.dump.is_none() {
        return Err(String::from("--dump-every names its files after --dump"));
    }
    Ok(args)
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse(&argv) {
        Ok(args) => args,
        Err(error) => {
            if !error.is_empty() {
                eprintln!("loquax-m9-sim: {error}");
            }
            eprintln!("{}", usage());
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("loquax-m9-sim: {error}");
            ExitCode::from(1)
        }
    }
}

/// The board at power-on over the flash `--store` names.
fn power_on(store: Option<&str>) -> Result<Board, String> {
    match store.unwrap_or("fixture") {
        "fixture" => Board::boot(Flash::blank(), Log::default(), 1),
        "real" => {
            let mut flash = Flash::blank();
            flash.home = Some(real::address());
            let mut board = Board::boot(flash, Log::default(), 1)?;
            let archive = real::archive()?;
            // The pull's pages, as many entries as a LoRa page's census
            // (`restore_counts.rs` keeps 72 a page).
            for page in archive.chunks(72) {
                board.pull(page);
                board.settle();
            }
            board.log.line(
                "[SIM] ",
                &format!(
                    "store=real entries={} pages={}",
                    archive.len(),
                    archive.len().div_ceil(72)
                ),
            );
            Ok(board)
        }
        image => Board::boot(Flash::load(Path::new(image))?, Log::default(), 1),
    }
}

fn run(args: &Args) -> Result<(), String> {
    let board = power_on(args.store.as_deref())?;
    if args.window {
        #[cfg(feature = "sim-window")]
        return window::run(board, args.scale, args.save.as_deref());
        #[cfg(not(feature = "sim-window"))]
        return Err(String::from(
            "--window: built without the `sim-window` feature (just m9-sim)",
        ));
    }
    headless(board, args)
}

/// The script, run against the board's clock.
fn headless(mut board: Board, args: &Args) -> Result<(), String> {
    let mut now = 0u64;
    let mut boots = 1;
    let mut presses = 0usize;
    let mut rebooted = false;
    let mut frames_dumped = board.frames;
    board.settle();
    for step in &args.steps {
        match step {
            Step::Key(code) => {
                now += script::STEP_MS;
                board::set_clock_ms(now);
                board.press(*code);
                board.settle();
                presses += 1;
            }
            Step::Wait(ms) => {
                // The board turns all the while: the map's words come
                // and go by the clock.
                let end = now + ms;
                while now < end {
                    now = (now + 50).min(end);
                    board::set_clock_ms(now);
                    board.settle();
                }
            }
            Step::Reboot => {
                boots += 1;
                board = reboot(board, boots, args.save.as_deref())?;
                now = 0;
            }
            Step::Dump(path) => write_png(board.frame(), Path::new(path))?,
            Step::Screen(byte, path) => {
                board.show_screen(*byte);
                write_png(board.frame(), Path::new(path))?;
                board.settle();
            }
        }
        if let (Some(every), Some(dump)) = (args.dump_every, args.dump.as_ref()) {
            if board.frames / every > frames_dumped / every {
                frames_dumped = board.frames;
                write_png(board.frame(), &numbered(dump, board.frames))?;
            }
        }
        if args.reboot_after == Some(presses) && !rebooted && matches!(step, Step::Key(_)) {
            rebooted = true;
            boots += 1;
            board = reboot(board, boots, args.save.as_deref())?;
            now = 0;
        }
    }
    board.settle();
    if let Some(dump) = &args.dump {
        write_png(board.frame(), dump)?;
        board
            .log
            .line("[SIM] ", &format!("dump={}", dump.display()));
    }
    if args.text {
        println!("PAGE {}", board.last_page.as_deref().unwrap_or("none"));
        for (i, row) in board.text().iter().enumerate() {
            println!("ROW {i:02} |{row}|");
        }
    }
    let (flash, _) = board.power_off();
    if let Some(save) = &args.save {
        flash.save(save)?;
    }
    Ok(())
}

/// Power the board off and on again over the flash it wrote.
fn reboot(board: Board, boots: u64, save: Option<&Path>) -> Result<Board, String> {
    let (flash, log) = board.power_off();
    if let Some(save) = save {
        flash.save(save)?;
    }
    let mut board = Board::boot(flash, log, boots)?;
    // The loop turns before a key, as after the first power-on: what the
    // boot asked (1546, the cursor chat's page) is read at t=0.
    board.settle();
    Ok(board)
}

/// `<stem>-f<frame>.png` beside `dump`.
fn numbered(dump: &Path, frame: u64) -> PathBuf {
    let stem = dump
        .file_stem()
        .map_or_else(|| String::from("m9"), |s| s.to_string_lossy().into_owned());
    dump.with_file_name(format!("{stem}-f{frame:05}.png"))
}

/// The panel as an RGB PNG, as the goldens are written.
pub fn write_png(frame: &Frame, path: &Path) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), WIDTH, HEIGHT);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    writer
        .write_image_data(&frame.to_rgb888())
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_options_read() {
        let argv: Vec<String> = ["--store", "real", "--keys", "down enter", "--text"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let args = parse(&argv).expect("args");
        assert_eq!(args.store.as_deref(), Some("real"));
        assert_eq!(args.steps, [Step::Key(0xb6), Step::Key(0x0d)]);
        assert!(args.text);
        let bad: Vec<String> = vec!["--frob".into()];
        assert!(parse(&bad).is_err(), "the negative: an unknown option");
        let every: Vec<String> = vec!["--dump-every".into(), "2".into()];
        assert!(parse(&every).is_err(), "--dump-every without --dump");
    }
}
