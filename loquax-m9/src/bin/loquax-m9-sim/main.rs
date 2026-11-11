//! `loquax-m9-sim` (1530): the ThinkNode M9's Loquax on the host, drawn
//! as the board draws it, driven by its keys, in a window or headless.
//!
//! ```text
//! loquax-m9-sim [--store fixture|real|<image>] [--save <image>] [--bench]
//!               [--keys "down down enter" | --script <file>]
//!               [--reboot-after <n>] [--dump <png>] [--dump-every <n>]
//!               [--text] [--window [--scale 2|3]]
//!               [--fold-cost-us <us>] [--fold-budget-us <us>]
//! ```
//!
//! - `--store`: what the board's flash holds at power-on. `fixture` (the
//!   default) is a board without a client record: the pairing, and Back
//!   leaves it for the fixture model, as on the board. `real` is a device
//!   of a home whose store a first pull of the 1489 fixture filled (300
//!   conversations, 4000 messages). Anything else is a flash image a run
//!   saved with `--save`. `pulling` (1575) is a device of that home
//!   with nothing pulled yet: the script's `pull <pages>` steps hand it
//!   the fixture's pages while its keys are pressed.
//! - `--fold-cost-us`: what one pulled entry's fold costs the board's UI
//!   core, on the sim's clock (1575; 0, the default, costs nothing, and a
//!   page folds in one turn). `--fold-budget-us`: the drain's budget
//!   (`home::FOLD_BUDGET_US` by default; 0 for none, a page whole in its
//!   turn whatever the keys, as before 1575). Each key's frame says
//!   `[SIM] key_ms=<n>`, the press to the flush on the sim's clock.
//! - `--bench`: the bench image (1562): "Bench" listed, `bench arrive`
//!   and `bench forget` in a script taken. Without it the board is a
//!   release image, which restores no "Bench" and refuses both.
//! - `--seed-home <dir>`: fill a `loquax mailbox` state directory with
//!   the 1489 fixture as its archive, under its own identity, print
//!   `BENCH_HOME address=<hex> appended=<n> held=<n>` and exit (1562,
//!   [`seed`]; `scripts/m9-bench-home.sh setup`).
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
mod seed;
#[cfg(feature = "sim-window")]
mod window;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use board::{Board, Log};
use flash::Flash;
use loquax_m9::bench::Arrivals;
use loquax_m9::frame::{Frame, HEIGHT, WIDTH};
use script::Step;

/// What the command line asked.
#[derive(Debug, Default)]
struct Args {
    store: Option<String>,
    save: Option<PathBuf>,
    bench: bool,
    seed_home: Option<PathBuf>,
    steps: Vec<Step>,
    reboot_after: Option<usize>,
    dump: Option<PathBuf>,
    dump_every: Option<u64>,
    text: bool,
    window: bool,
    scale: u32,
    fold_cost_us: u64,
    /// `None`: the default budget; `Some(0)`: no budget, as before 1575.
    fold_budget_us: Option<u64>,
}

fn usage() -> &'static str {
    "usage: loquax-m9-sim --seed-home <dir> | [--store fixture|real|<image>] [--save <image>] [--bench] \
     [--keys \"down enter\" | --script <file>] [--reboot-after <n>] \
     [--dump <png>] [--dump-every <n>] [--text] [--window [--scale 2|3]] \
     [--fold-cost-us <us>] [--fold-budget-us <us>]"
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
            "--fold-cost-us" => {
                let n = value("--fold-cost-us")?;
                args.fold_cost_us = n.parse().map_err(|_| format!("--fold-cost-us {n}"))?;
            }
            "--fold-budget-us" => {
                let n = value("--fold-budget-us")?;
                args.fold_budget_us = Some(n.parse().map_err(|_| format!("--fold-budget-us {n}"))?);
            }
            "--bench" => args.bench = true,
            "--seed-home" => args.seed_home = Some(PathBuf::from(value("--seed-home")?)),
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

/// The board at power-on over the flash `--store` names, as the image
/// `arrivals` says.
fn power_on(store: Option<&str>, arrivals: Arrivals) -> Result<Board, String> {
    match store.unwrap_or("fixture") {
        "fixture" => Board::boot_as(Flash::blank(), Log::default(), 1, arrivals),
        "real" => {
            let mut flash = Flash::blank();
            flash.home = Some(real::address());
            let mut board = Board::boot_as(flash, Log::default(), 1, arrivals)?;
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
        // 1575: a device of the home with nothing pulled yet; the 1489
        // fixture is the archive `pull <pages>` steps hand it, while the
        // script presses keys.
        "pulling" => {
            let mut flash = Flash::blank();
            flash.home = Some(real::address());
            let mut board = Board::boot_as(flash, Log::default(), 1, arrivals)?;
            board.to_pull = real::archive()?;
            Ok(board)
        }
        image => Board::boot_as(Flash::load(Path::new(image))?, Log::default(), 1, arrivals),
    }
}

/// The image `--bench` names.
fn arrivals(args: &Args) -> Arrivals {
    match args.bench {
        true => Arrivals::Shown,
        false => Arrivals::Dropped,
    }
}

fn run(args: &Args) -> Result<(), String> {
    if let Some(dir) = &args.seed_home {
        let seeded = seed::seed(dir)?;
        println!(
            "BENCH_HOME address={} appended={} held={}",
            seeded
                .address
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            seeded.appended,
            seeded.held
        );
        return Ok(());
    }
    let mut board = power_on(args.store.as_deref(), arrivals(args))?;
    board.fold_cost_us = args.fold_cost_us;
    match args.fold_budget_us {
        Some(0) => board.fold_budget_us = None,
        Some(us) => board.fold_budget_us = Some(us),
        None => {}
    }
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
    for (n, step) in args.steps.iter().enumerate() {
        match step {
            Step::Key(code) => {
                // The press at the script's time (1575): scheduled, and
                // the turns run once the run of presses ends, so a board
                // still busy takes each after the turn it fell into.
                now += script::STEP_MS;
                board.press_at(*code, now);
                presses += 1;
                // A pull's pages join the queue without a turn, so
                // presses after it are scheduled before the turns run too.
                let more = matches!(args.steps.get(n + 1), Some(Step::Key(_) | Step::Pull(..)));
                if !more || args.reboot_after == Some(presses) {
                    board.answer_keys();
                    board.run_until(now + script::STEP_MS);
                }
            }
            Step::Wait(ms) => {
                // The board turns all the while: the map's words come
                // and go by the clock.
                let end = now + ms;
                while now < end {
                    now = (now + 50).min(end);
                    board::clock_to_ms(now);
                    board.run_until(now);
                }
            }
            Step::Pull(pages, entries) => {
                if board.to_pull.is_empty() {
                    return Err(String::from(
                        "pull: only a --store pulling board has an archive to pull",
                    ));
                }
                let went = board.pull_next(*pages, *entries);
                board
                    .log
                    .line("[SIM] ", &format!("pull pages={went} entries={entries}"));
            }
            Step::Reboot => {
                boots += 1;
                board = reboot(board, boots, args.save.as_deref())?;
                now = 0;
            }
            Step::Dump(path) => write_png(board.frame(), Path::new(path))?,
            Step::Bar => {
                // The turns the clock owes first: a house tap's gap that
                // ran out in the wait opens the overview in its next turn.
                board.settle();
                let top = board.text().first().cloned().unwrap_or_default();
                board.log.line("[SIM] ", &format!("bar=\"{}\"", top.trim()));
            }
            Step::Screen(byte, path) => {
                board.show_screen(*byte);
                write_png(board.frame(), Path::new(path))?;
                board.settle();
            }
            Step::BenchArrive => board.bench_arrive()?,
            Step::BenchForget => board.bench_forget()?,
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
    let arrivals = board.arrivals;
    let (cost, budget, to_pull) = (
        board.fold_cost_us,
        board.fold_budget_us,
        board.to_pull.clone(),
    );
    let (flash, log) = board.power_off();
    if let Some(save) = save {
        flash.save(save)?;
    }
    let mut board = Board::boot_as(flash, log, boots, arrivals)?;
    board.fold_cost_us = cost;
    board.fold_budget_us = budget;
    board.to_pull = to_pull;
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

/// The bench never touches a real chat (1562), on the 1489 fixture as a
/// first pull leaves it (`--store real`).
#[cfg(test)]
mod bench_tests {
    use super::*;
    use leviculum_lxmf::msgpack;
    use loquax::home::wire::Entry;
    use loquax::home::KIND_RECEIVED;
    use loquax::ui::UiModel;
    use loquax_m9::bench::{self, BENCH_PEER};
    use loquax_m9::field::Field;

    /// Every real conversation's list row as the reader sees it: the
    /// peer, the unread, the preview and its time.
    fn rows(model: &UiModel<Field>) -> Vec<String> {
        let mut rows: Vec<String> = model
            .conversations
            .iter()
            .filter(|c| c.peer != BENCH_PEER)
            .filter_map(|c| model.row(&c.peer))
            .map(|row| format!("{:02x?} {} {:?}", row.peer, row.unread, row.listing))
            .collect();
        rows.sort();
        rows
    }

    /// A 1540 arrival: into the real chat of `peer`, its signature zero.
    fn unflagged(peer: [u8; 16], address: [u8; 16], at: i64, n: u32, seq: u64) -> Entry {
        let mut packed = Vec::new();
        packed.extend_from_slice(&address);
        packed.extend_from_slice(&peer);
        packed.extend_from_slice(&[0u8; 64]);
        msgpack::array(&mut packed, 4);
        msgpack::f64(&mut packed, at as f64);
        msgpack::bin(&mut packed, b"");
        msgpack::bin(&mut packed, format!("bench arrival {n}").as_bytes());
        msgpack::map(&mut packed, 0);
        Entry {
            seq,
            kind: KIND_RECEIVED,
            at,
            packed,
        }
    }

    /// `bench arrive` three times on the bench image: "Bench" at the
    /// list's top under the cursor with three unread, every real row as
    /// it was. The same flash booted as a release image lists no
    /// "Bench" and the same real rows; and a release image refuses the
    /// arrival outright.
    #[test]
    fn an_arrival_lands_in_bench_only_and_a_release_boot_shows_none() {
        let mut board = power_on(Some("real"), Arrivals::Shown).expect("power on");
        board.settle();
        let before = rows(board.model());
        assert!(before.len() >= 299, "{}", before.len());
        for _ in 0..3 {
            board.bench_arrive().expect("arrive");
        }
        assert_eq!(rows(board.model()), before, "no real row moved");
        let at = bench::index(board.model()).expect("Bench");
        assert_eq!(board.model().conversations[at].unread, 3);
        assert_eq!(board.model().order()[0], at);
        let text = board.text().join("\n");
        assert!(text.contains("Bench"), "{text}");
        assert!(text.contains("bench arrival 3"), "{text}");

        let (flash, log) = board.power_off();
        let mut release = Board::boot_as(flash, log, 2, Arrivals::Dropped).expect("release");
        release.settle();
        assert_eq!(bench::index(release.model()), None);
        let text = release.text().join("\n");
        assert!(!text.contains("Bench"), "{text}");
        assert_eq!(rows(release.model()), before, "the same real rows");
        assert!(
            release.bench_arrive().is_err(),
            "a release image refuses it"
        );
        // The control: the same flash on the bench image lists it.
        let (flash, log) = release.power_off();
        let shown = Board::boot_as(flash, log, 3, Arrivals::Shown).expect("bench");
        assert!(bench::index(shown.model()).is_some());
    }

    /// The clean-up on the host fixture: a board that ran 1540's image
    /// (unflagged arrivals in five real chats, one of them opened and
    /// read) and then this one (three in "Bench"). `bench forget` leaves
    /// no arrival anywhere and every real row as it was before 1540
    /// wrote into it, after the reopen and after a reboot.
    #[test]
    fn bench_forget_cleans_a_store_1540_wrote_into() {
        let mut board = power_on(Some("real"), Arrivals::Shown).expect("power on");
        board.settle();
        let before = rows(board.model());
        let address = real::address();
        let peers: Vec<[u8; 16]> = board
            .model()
            .order()
            .iter()
            .take(5)
            .map(|&i| board.model().conversations[i].peer)
            .collect();
        // 1540 kept an arrival under the store's cursor: the pull's last.
        let cursor = real::archive().expect("archive").len() as u64;
        for (n, peer) in peers.iter().enumerate() {
            let at = 1_800_000_000 + n as i64;
            board.pull(&[unflagged(*peer, address, at, n as u32 + 1, cursor)]);
            board.settle();
        }
        assert_ne!(
            rows(board.model()),
            before,
            "the control: 1540 moved real rows"
        );
        for _ in 0..3 {
            board.bench_arrive().expect("arrive");
        }
        board.bench_forget().expect("forget");
        let forgot = board
            .log
            .kept
            .iter()
            .find(|line| line.contains("forget=done"))
            .cloned()
            .unwrap_or_default();
        assert!(forgot.contains("flagged=4 unflagged=5"), "{forgot}");
        assert_eq!(bench::index(board.model()), None);
        assert_eq!(rows(board.model()), before, "every real row as before 1540");
        let (flash, log) = board.power_off();
        let again = Board::boot_as(flash, log, 2, Arrivals::Shown).expect("reboot");
        assert_eq!(rows(again.model()), before, "and after a reboot");
        assert!(!again.text().join("\n").contains("bench arrival"));
    }
}

/// 1564: a boot reads nothing. The board comes up on the list with the
/// first row under the cursor; that row's unread stay unread, and no read
/// mark is kept, until the reader opens the chat, after a power cycle
/// (the boot resumes from the summary log) and after a reset in the
/// middle of a pull (the ring went on past the summary: the watchdog's
/// case, `[PREV_RUN] reason=wdt1` on the board), with the clock running
/// past the read dwell.
#[cfg(test)]
mod boot_reads_nothing {
    use super::*;

    /// The top row's unread, and whether the boot's log since `from`
    /// said the list and kept a read mark.
    fn after_boot(board: &mut Board, from: usize) -> (usize, bool, Vec<String>) {
        // Past the dwell (`loquax::ui::receipts::SEEN_MS`), the board
        // turning all the while.
        let mut now = 0;
        while now < 6000 {
            now += 50;
            board::clock_to_ms(now);
            board.run_until(now);
        }
        let model = board.model();
        let top = model.order()[0];
        let lines: Vec<String> = board.log.kept[from..].to_vec();
        let listed = lines.iter().any(|l| l.starts_with("[PAGE] name=chats "));
        let marks = lines
            .iter()
            .filter(|l| l.starts_with("[STORE] marks="))
            .cloned()
            .collect();
        (model.conversations[top].unread, listed, marks)
    }

    #[test]
    fn a_power_cycle_keeps_the_top_rows_unread() {
        let board = power_on(Some("real"), Arrivals::Dropped).expect("power on");
        let top = board.model().order()[0];
        let unread = board.model().conversations[top].unread;
        assert!(unread > 0, "the store's first row is unread");
        let (flash, log) = board.power_off();
        let from = log.kept.len();
        let mut again = Board::boot_as(flash, log, 2, Arrivals::Dropped).expect("boot");
        assert!(again.log.kept[from..]
            .iter()
            .any(|l| l.contains("restore=resumed")));
        let (after, listed, marks) = after_boot(&mut again, from);
        assert!(listed, "the boot shows the list");
        assert_eq!(after, unread, "the top row's unread");
        assert!(marks.is_empty(), "{marks:?}");
    }

    #[test]
    fn a_reset_mid_pull_keeps_the_top_rows_unread() {
        let mut board = power_on(Some("pulling"), Arrivals::Dropped).expect("power on");
        board.fold_cost_us = 3600;
        board.pull_next(20, 72);
        board.settle();
        // The next pages kept, their fold cut by the reset.
        board.pull_next(20, 72);
        board.run_until(200);
        let (flash, log) = board.power_off();
        let from = log.kept.len();
        let mut again = Board::boot_as(flash, log, 2, Arrivals::Dropped).expect("boot");
        let index = again.log.kept[from..]
            .iter()
            .find(|l| l.starts_with("[STORE] index="))
            .cloned()
            .unwrap_or_default();
        assert!(
            index.contains("why=ring-moved") || !index.contains("tail=0"),
            "the ring went on past the summary: {index}"
        );
        let top = again.model().order()[0];
        let unread = again.model().conversations[top].unread;
        assert!(unread > 0, "the restored first row is unread");
        let (after, listed, marks) = after_boot(&mut again, from);
        assert!(listed, "the boot shows the list");
        assert_eq!(after, unread, "the top row's unread");
        assert!(marks.is_empty(), "{marks:?}");
        // The control: opened, the chat is read and its mark kept.
        let enter = script::code("enter").expect("enter");
        again.press_at(enter, 6100);
        again.answer_keys();
        let mut now = 6100;
        while now < 12_000 {
            now += 50;
            board::clock_to_ms(now);
            again.run_until(now);
        }
        assert_eq!(again.model().conversations[top].unread, 0, "opened");
        assert!(
            again
                .log
                .kept
                .iter()
                .any(|l| l.starts_with("[STORE] marks=")),
            "the open keeps its mark"
        );
    }
}
