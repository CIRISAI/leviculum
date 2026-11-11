//! The board in the sim (1530): the ThinkNode M9 image's two loops
//! (`leviculum-esp/src/bin/thinknode_m9.rs`) over the host's stand-ins.
//!
//! The UI side is `ui_main`'s turn, step for step and through the same
//! library calls: the key presses drained from the poll queue
//! (`kbraw::Presses`), each read by `Protocol::read` and `Key::of`, a
//! command run by `keyboard::run`, an event by the pairing or
//! `group::press`, what the fold left routed by `home::route`, the map's
//! chrome by its clock, and one redraw per turn by the renderer the board
//! draws with (`Renderer::show`, `pair::draw`), its `[PAGE]` line by
//! `page::of`. The I/O side is `handheld.rs`'s store half: the restore at
//! boot (`home::restore_timed`), the asks and marks kept (`keep_ask`,
//! `keep_marks`), the summary log (`Shelf::said`, `Shelf::flush`), a
//! conversation's page read (`Shelf::page`), the idle erase
//! (`Shelf::quiet`), and a pulled page kept and handed over
//! (`home::keep`, `ToUi::Entries`).
//!
//! The two cores are one thread here: a turn of the UI side, then one of
//! the I/O side, until neither queue holds anything. The board runs them
//! side by side; what the sim cannot show is any race between them.
//!
//! Stubbed, and what that cannot reproduce: the panel is the frame the
//! renderer draws into (no SPI, no flush timing: `[DISP] flush` says
//! `ms=0.000`); the keyboard is the code the script or the window names,
//! put in the queue as the controller's answer for it (no I2C, no
//! missed polls); the flash is [`crate::flash::Flash`] (no erase timing,
//! no power cut); there is no node and no UART, so nothing is pulled from
//! a home and an ask stays queued (the top bar's word is the one a board
//! shows before its first ask ends); the heap is the host's (no PSRAM, no
//! census of its bytes); the boot's census of every screen is not run.

use std::cell::Cell;
use std::collections::VecDeque;

use leviculum_ui::input::{KeyCode, KeyEventKind};
use loquax::home::ask::Verb;
use loquax::home::board::{self, Session, Word};
use loquax::home::wire::{Entry, Mark};
use loquax::ui::listing::Row;
use loquax::ui::strings::StrId;
use loquax::ui::UiModel;
use loquax_m9::field::Field;
use loquax_m9::font::Size;
use loquax_m9::frame::Frame;
use loquax_m9::home::{self, Folding, How, Outgoing, Shelf};
use loquax_m9::house::{self, Did, Overview};
use loquax_m9::kbraw::{self, Presses, Protocol, Read};
use loquax_m9::keyboard::{self, Key};
use loquax_m9::map::{self, Map};
use loquax_m9::pair::{self, Pairing};
use loquax_m9::read::Reader;
use loquax_m9::render::Renderer;
use loquax_m9::screen::{self, Look, COLS, ROWS};

use loquax_m9::bench::{self, Arrivals};
use loquax_m9::{fixture, group, page};

use crate::flash::Flash;

thread_local! {
    /// The board's clock in the sim, microseconds since boot: the script's
    /// time in a headless run, the wall's in the window. Not a field
    /// because the renderer takes a `fn` for its map timing, as the
    /// board's does; the thread's own (1546), so the parity tests, each a
    /// board on a thread of the harness, do not set each other's clocks
    /// (a test that booted reset the map test's clock to 0, and the map's
    /// words stood on its golden).
    static CLOCK_US: Cell<u64> = const { Cell::new(0) };
}

/// Microseconds since boot.
pub fn uptime_us() -> u64 {
    CLOCK_US.with(Cell::get)
}

/// Milliseconds since boot.
pub fn uptime_ms() -> u64 {
    uptime_us() / 1000
}

/// Set the clock.
pub fn set_clock_ms(ms: u64) {
    CLOCK_US.with(|clock| clock.set(ms.saturating_mul(1000)));
}

/// Move the clock on to `ms`, never back (1575): a board whose turns
/// ran past the script's time keeps its own.
pub fn clock_to_ms(ms: u64) {
    CLOCK_US.with(|clock| clock.set(clock.get().max(ms.saturating_mul(1000))));
}

/// Move the clock on by `us`: the board's cost of an entry folded
/// ([`Board::fold_cost_us`]).
fn advance_us(us: u64) {
    CLOCK_US.with(|clock| clock.set(clock.get() + us));
}

/// A turn longer than this says where its time went, as the board's
/// `[LOOP]` line does (`SLOW_TURN_MS`).
const SLOW_TURN_MS: u64 = 100;

/// The board's debug line: `<prefix><body> t=<ms>` in the firmware's own
/// shape (`leviculum-log-line`), on stderr.
#[derive(Default)]
pub struct Log {
    /// Every line, kept for the window's title and the tests.
    pub kept: Vec<String>,
}

impl Log {
    pub fn line(&mut self, prefix: &str, body: &str) {
        let mut buf = vec![0u8; prefix.len() + body.len() + 32];
        let line =
            leviculum_log_line::format_line(&mut buf, prefix, format_args!("{body}"), uptime_ms());
        let text = String::from_utf8_lossy(line);
        let text = text.trim_end_matches(['\r', '\n']);
        eprintln!("{text}");
        self.kept.push(text.to_string());
    }

    pub fn raw(&mut self, text: &str) {
        eprintln!("{text}");
        self.kept.push(text.to_string());
    }
}

/// What the screen shows besides the model, as the board's `Shown`.
#[derive(Default)]
struct Shown {
    pairing: Option<Pairing>,
    word: Option<Word>,
    fault: Option<StrId>,
    map: map::Overlay,
}

/// What the I/O side hands the UI side (`ToUi`), those the sim has.
enum ToUi {
    Entries(Vec<Entry>, [u8; 16]),
    Fault(Option<StrId>),
    Page {
        peer: [u8; 16],
        entries: Vec<Entry>,
        earlier: usize,
        address: [u8; 16],
    },
}

/// What the UI side hands the I/O side (`FromUi`), those the sim has; the
/// log lines are written where they are said.
enum FromUi {
    Asks {
        verbs: Vec<Verb>,
        marks: Vec<Mark>,
    },
    Code(String),
    Rows {
        rows: Vec<Row>,
        gone: Vec<[u8; 16]>,
        pages: u32,
    },
    Load([u8; 16]),
    /// The reader switched the type (1532): the record keeps it.
    Type(Size),
}

/// What a turn's folds owe the I/O side (`Wants`).
#[derive(Default)]
struct Wants {
    verbs: Vec<Verb>,
    marks: Vec<Mark>,
    rows: Vec<Row>,
    gone: Vec<[u8; 16]>,
    loads: Vec<[u8; 16]>,
    pages: u32,
}

/// The board: its flash, its I/O side, its UI side.
pub struct Board {
    pub flash: Flash,
    pub log: Log,
    // The I/O side.
    shelf: Option<Shelf>,
    outgoing: Outgoing,
    unsaved: u32,
    faulted: bool,
    // The UI side.
    renderer: Renderer<'static, Frame>,
    map: Map<'static>,
    model: Box<UiModel<Field>>,
    shown: Shown,
    /// The house key's overview and the reader's type size (1532).
    overview: Overview,
    row: usize,
    said: page::Said,
    drawn: screen::Drawn,
    chrome: map::Chrome,
    kb: Protocol,
    presses: Presses,
    seed: [u8; 32],
    counter: u64,
    to_ui: VecDeque<ToUi>,
    from_ui: VecDeque<FromUi>,
    /// The last `KEY` line, for the window's title.
    pub last_key: Option<String>,
    /// The last `[PAGE]` line's body.
    pub last_page: Option<String>,
    /// Frames drawn that changed the panel: the flushes.
    pub frames: u64,
    /// A redraw the next turn owes (a frame dump drew over the screen).
    owed: bool,
    /// What a size set asked of the model (1546), routed by the next
    /// turn as the board's loop routes it.
    asked: Vec<loquax::ui::UiEffect>,
    /// What the reader saw, said to the model, and its clocks (1541).
    reader: Reader,
    /// The image: bench or release (1562).
    pub arrivals: Arrivals,
    /// The bench's arrivals since boot, each its own number.
    arrived: u32,
    /// The pulled page folded a budget at a time (1575), as `ui_main`.
    folding: Folding,
    /// What one entry folded costs the board's UI core, in the sim's
    /// microseconds (`--fold-cost-us`): 0, the default, folds in no time,
    /// so the budget never runs out and a page folds in one turn.
    pub fold_cost_us: u64,
    /// The drain's budget (`--fold-budget-us`), `None` for none: every
    /// page whole in the turn that took it, keys or not, as before 1575.
    pub fold_budget_us: Option<u64>,
    /// Presses the script made, by time, not yet in the queue (1575).
    scheduled: VecDeque<(u64, u8)>,
    /// The presses the keys stage took this turn, by their press time,
    /// for the `[SIM] key_ms=` line of the frame that answers them.
    keys_owed: Vec<u64>,
    /// The archive a `--store pulling` board pulls by `pull <pages>`
    /// steps, and how much of it is pulled.
    pub to_pull: Vec<Entry>,
    pulled: usize,
}

impl Board {
    /// Power on over `flash`: the store restored for its home, or the
    /// pairing over the fixture on a board without one, and the first
    /// frame drawn. `boots` makes the client ids of each boot its own.
    #[cfg(test)]
    pub fn boot(flash: Flash, log: Log, boots: u64) -> Result<Self, String> {
        Self::boot_as(flash, log, boots, Arrivals::Dropped)
    }

    /// [`Board::boot`] as the image `arrivals` names (1562): the bench
    /// image ([`Arrivals::Shown`]) lists "Bench" and takes `bench arrive`
    /// and `bench forget`, a release image ([`Arrivals::Dropped`]) does
    /// neither.
    pub fn boot_as(
        mut flash: Flash,
        mut log: Log,
        boots: u64,
        arrivals: Arrivals,
    ) -> Result<Self, String> {
        set_clock_ms(0);
        log.line(
            "[SIM] ",
            &format!(
                "board=M9 loquax-m9={} sectors={} home={} boot={boots} image={}",
                env!("CARGO_PKG_VERSION"),
                flash.messages.sectors.len(),
                flash
                    .home
                    .map_or_else(|| String::from("none"), |a| hex8(&a)),
                match arrivals {
                    Arrivals::Shown => "bench",
                    Arrivals::Dropped => "release",
                },
            ),
        );
        let mut outgoing = Outgoing::default();
        let (model, shelf, shown) = match flash.home {
            None => {
                log.line("[HOME] ", "client=none");
                let shown = Shown {
                    pairing: Some(Pairing::new()),
                    ..Shown::default()
                };
                (fixture::model(), None, shown)
            }
            Some(address) => {
                let (model, shelf) =
                    open_store(&mut flash, address, arrivals, &mut log, &mut outgoing)?;
                let shown = Shown {
                    word: Some(Word::of(&Session::new(), true, None)),
                    ..Shown::default()
                };
                (model, Some(shelf), shown)
            }
        };
        // The panel's frame lives as long as the run: the renderer borrows
        // it, as the board's borrows its static one.
        let frame: &'static mut Frame = Box::leak(Box::new(Frame::new()));
        let mut renderer =
            Renderer::new(frame).map_err(|_| String::from("the renderer did not start"))?;
        renderer.set_clock(uptime_us);
        let map = Map::board(map::FIXTURE, &[])
            .map_err(|_| String::from("the map region did not read"))?;
        let mut seed = [0u8; 32];
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        seed[..16].copy_from_slice(&nanos.to_le_bytes());
        seed[16..24].copy_from_slice(&boots.to_le_bytes());
        let mut board = Self {
            flash,
            log,
            shelf,
            outgoing,
            unsaved: 0,
            faulted: false,
            renderer,
            map,
            model: Box::new(model),
            shown,
            overview: Overview::new(Size::DEFAULT),
            row: 0,
            said: page::Said::default(),
            drawn: screen::Drawn::default(),
            chrome: map::Chrome::default(),
            kb: kbraw::MEASURED,
            presses: Presses::new(),
            seed,
            counter: 0,
            to_ui: VecDeque::new(),
            from_ui: VecDeque::new(),
            last_key: None,
            last_page: None,
            frames: 0,
            owed: false,
            asked: Vec::new(),
            reader: Reader::default(),
            arrivals,
            arrived: 0,
            folding: Folding::default(),
            fold_cost_us: 0,
            fold_budget_us: Some(home::FOLD_BUDGET_US),
            keys_owed: Vec::new(),
            scheduled: VecDeque::new(),
            to_pull: Vec::new(),
            pulled: 0,
        };
        // The record's type (1532), as the board reads it at boot; a board
        // without a record shows the default.
        let size = match board.flash.home {
            Some(_) => Size::of_record(board.flash.type_size),
            None => Size::DEFAULT,
        };
        board.overview.restore(size);
        board.asked = house::set_size(size, &mut board.model, &mut board.renderer);
        board
            .log
            .line("[TYPE] ", &format!("size={} via=boot", size.name()));
        board.redraw();
        board.drawn.drawn(&board.model);
        Ok(board)
    }

    /// The model the board shows, for the tests.
    #[cfg(test)]
    pub fn model(&self) -> &UiModel<Field> {
        &self.model
    }

    /// Power off: the flash and the log, for the next boot.
    pub fn power_off(self) -> (Flash, Log) {
        (self.flash, self.log)
    }

    /// A key pressed now: the controller's answer for `code`, queued as
    /// the keyboard's timer queues a polled one.
    #[cfg(any(test, feature = "sim-window"))]
    pub fn press(&mut self, code: u8) {
        let (answer, len) = kbraw::answer_of(&self.kb, code);
        self.presses
            .poll(&self.kb, uptime_ms() as u32, &answer[..len]);
    }

    /// A key pressed at `ms` (1575): it reaches the queue when the
    /// board's clock passes `ms`, inside whatever turn runs then, as the
    /// keyboard's timer puts it there on the board. Scheduled before the
    /// turns run, so a press falls into the turn that spans it.
    pub fn press_at(&mut self, code: u8, ms: u64) {
        self.scheduled.push_back((ms, code));
    }

    /// The scheduled presses whose time the clock passed, into the queue.
    fn release_presses(scheduled: &mut VecDeque<(u64, u8)>, presses: &mut Presses, kb: &Protocol) {
        while let Some(&(ms, code)) = scheduled.front() {
            if ms > uptime_ms() {
                break;
            }
            scheduled.pop_front();
            let (answer, len) = kbraw::answer_of(kb, code);
            presses.poll(kb, ms as u32, &answer[..len]);
        }
    }

    /// `pull <pages> <entries>` (1575): the next pages of the archive a
    /// `--store pulling` board was given, kept and handed to the UI side
    /// as the client hands a pull's pages, without a turn between them.
    /// How many pages went.
    pub fn pull_next(&mut self, pages: usize, entries: usize) -> usize {
        let mut went = 0;
        for _ in 0..pages {
            let end = (self.pulled + entries.max(1)).min(self.to_pull.len());
            if end == self.pulled {
                break;
            }
            let page = self.to_pull[self.pulled..end].to_vec();
            self.pulled = end;
            self.pull(&page);
            went += 1;
        }
        went
    }

    /// A `FRAME_DUMP` naming screen `byte` (`ToUi::Dump`): the fixture's
    /// screen walked as the board walks it (`Screen::walk`) and drawn in
    /// place of the model, or the pairing fresh at
    /// [`pair::DUMP_BYTE`], with nothing of the board's own over it.
    /// Returns its name; the frame holds it until the next turn, which
    /// draws what the board shows, the walked model now, as the board's
    /// next turn does.
    pub fn show_screen(&mut self, byte: u8) -> Option<&'static str> {
        // The goldens are the default size's (1532): a dump draws in it,
        // the overview closed, and the reader's size comes back after.
        let kept_overview = self.overview;
        self.overview = Overview::new(Size::DEFAULT);
        self.renderer.set_size(Size::DEFAULT);
        let name = self.dump_screen(byte);
        self.overview = kept_overview;
        let asked = house::set_size(self.overview.size, &mut self.model, &mut self.renderer);
        self.asked.extend(asked);
        name
    }

    fn dump_screen(&mut self, byte: u8) -> Option<&'static str> {
        let name = if let Some(walked) = fixture::Screen::of(byte) {
            let (model, row) = walked.walk();
            *self.model = model;
            self.row = row;
            self.shown.pairing = None;
            let kept = std::mem::take(&mut self.shown);
            self.redraw();
            self.shown = kept;
            walked.name()
        } else if byte == pair::DUMP_BYTE {
            let fresh = Shown {
                pairing: Some(Pairing::new()),
                ..Shown::default()
            };
            let kept = std::mem::replace(&mut self.shown, fresh);
            let row = std::mem::take(&mut self.row);
            self.redraw();
            self.shown = kept;
            self.row = row;
            pair::DUMP_NAME
        } else {
            return None;
        };
        self.log.line("[DUMP] ", &format!("screen={name}"));
        self.owed = true;
        Some(name)
    }

    /// Turns of both sides until neither has anything left. A pull
    /// folded a budget at a time takes a turn per budget (1575).
    pub fn settle(&mut self) {
        self.run_until(u64::MAX);
    }

    /// Turns of both sides until neither has anything left or the clock
    /// reached `until_ms` (1575): a board whose folds cost time
    /// (`fold_cost_us`) is still busy when the script's next step comes,
    /// and the step's key falls into the turn that ran past it. With no
    /// cost the clock stands and this is [`Board::settle`].
    pub fn run_until(&mut self, until_ms: u64) {
        let until_us = until_ms.saturating_mul(1000);
        for _ in 0..100_000 {
            if uptime_us() >= until_us {
                return;
            }
            let (busy_ui, draw_ms, keys_ms) = self.ui_turn();
            let t1 = std::time::Instant::now();
            let busy_io = self.io_turn();
            let take = t1.elapsed().as_millis() as u64;
            self.say_turn(draw_ms, keys_ms, take);
            if !busy_ui
                && !busy_io
                && !self.folding.busy()
                && self.to_ui.is_empty()
                && self.from_ui.is_empty()
                && self.asked.is_empty()
            {
                // Idle until the next press the script made, if it
                // comes before `until_ms`.
                if let Some(&(ms, _)) = self.scheduled.front() {
                    if ms.saturating_mul(1000) < until_us {
                        clock_to_ms(ms);
                        continue;
                    }
                }
                // A quiet turn: the store erases ahead (`node.quiet`).
                self.quiet();
                // A summary backlog goes on a sector at a time in the
                // board's quiet turns after (1591), a second apart; the
                // sim writes it out here a sector a call, its clock
                // left where the script has it (a clock moved on would
                // run the reader's dwell).
                self.drain_backlog();
                return;
            }
        }
    }

    /// Turns until every press the script made is taken (1575), the
    /// clock moved on to a press still ahead when the board is idle.
    pub fn answer_keys(&mut self) {
        while let Some(&(ms, _)) = self.scheduled.front() {
            self.run_until(ms);
            clock_to_ms(ms);
            Self::release_presses(&mut self.scheduled, &mut self.presses, &self.kb);
            while self.presses.waiting() {
                self.ui_turn();
                self.io_turn();
            }
        }
        while self.presses.waiting() {
            self.ui_turn();
            self.io_turn();
        }
    }

    /// The panel's frame.
    pub fn frame(&self) -> &Frame {
        self.renderer.panel()
    }

    /// The screen's text rows: the cells the renderer draws, as the
    /// scene, the pairing or the map's cell layer word them. The map's
    /// picture, pins and names are pixels and not in them.
    pub fn text(&self) -> Vec<String> {
        let look = self.look();
        let (cols, rows) = self.overview.size.grid();
        let lines = match &self.shown.pairing {
            Some(pairing) => pair::screen(pairing, self.shown.word, &look, cols, rows),
            None if self.overview.open => {
                let page = house::page(&self.model, self.overview.size, env!("CARGO_PKG_VERSION"));
                self.overview.lines(&page, &look, cols, rows)
            }
            None if map::shown(&self.model) => {
                let bare = map::view(&self.model)
                    .is_none_or(|(view, _)| self.map.choose(&view) == map::Drawn::Bare);
                map::lines(&look, bare, COLS, ROWS)
            }
            None => screen::lines(&screen::scene(&self.model), &look, cols, rows),
        };
        lines
            .iter()
            .map(|line| line.cells.iter().map(|cell| cell.ch).collect())
            .collect()
    }

    /// Keep `entries` as a pulled page and hand them to the model, as
    /// the client does with each page of a pull (`handheld.rs`, the page
    /// kept before the model takes it).
    pub fn pull(&mut self, entries: &[Entry]) {
        let (Some(address), Some(shelf)) = (self.flash.home, self.shelf.as_mut()) else {
            return;
        };
        match home::keep(shelf, &mut self.flash.messages, entries, &address) {
            Ok(done) => {
                let body = format!(
                    "kept={} writes={} erased={} cursor={} used={} entries={}",
                    done.entries,
                    done.writes,
                    done.erased,
                    shelf.cursor(),
                    shelf.used(),
                    shelf.ring.entries()
                );
                self.log.line("[STORE] ", &body);
            }
            Err(error) => {
                self.unsaved += 1;
                self.log
                    .line("[STORE] ", &format!("kept=no error={error:?}"));
            }
        }
        // `Handheld::drain`: the summary's state waits for the rows.
        shelf.pages_owed += 1;
        self.to_ui
            .push_back(ToUi::Entries(entries.to_vec(), address));
    }

    /// `bench arrive` (1540, 1562) as the bench image does it: the UI
    /// side's arrival, the I/O side's entries kept and handed back as a
    /// page ([`Board::pull`]), "Bench" under the cursor once folded.
    pub fn bench_arrive(&mut self) -> Result<(), String> {
        if self.arrivals != Arrivals::Shown {
            return Err(String::from(
                "bench arrive: a release image refuses TYPE_BENCH_ARRIVE (--bench)",
            ));
        }
        let (Some(address), Some(cursor)) =
            (self.flash.home, self.shelf.as_ref().map(Shelf::cursor))
        else {
            self.log.line("[BENCH] ", "arrive=refused reason=no-home");
            return Err(String::from("bench arrive: the board is no home's device"));
        };
        self.arrived += 1;
        let arrival = bench::arrival(&self.model);
        self.log.line(
            "[BENCH] ",
            &format!(
                "arrive=sent peer={} named={}",
                hex8(&bench::BENCH_PEER),
                if arrival.named { "yes" } else { "no" }
            ),
        );
        let entries = bench::entries(address, arrival, self.arrived, cursor);
        self.pull(&entries);
        self.settle();
        bench::select(&mut self.model);
        let unread = bench::index(&self.model)
            .and_then(|at| self.model.conversations.get(at))
            .map_or(0, |c| c.unread);
        self.log.line(
            "[BENCH] ",
            &format!(
                "arrive=folded peer={} unread={unread}",
                hex8(&bench::BENCH_PEER)
            ),
        );
        self.owed = true;
        self.settle();
        Ok(())
    }

    /// `bench forget` (1562) as the bench image does it: the arrivals
    /// out of the store (`home::forget`), and the store opened again from
    /// the flash, its model in place of the old one
    /// (`Handheld::bench_reopen`).
    pub fn bench_forget(&mut self) -> Result<(), String> {
        if self.arrivals != Arrivals::Shown {
            return Err(String::from(
                "bench forget: a release image refuses TYPE_BENCH_FORGET (--bench)",
            ));
        }
        let Some(address) = self.flash.home else {
            self.log.line("[BENCH] ", "forget=refused reason=no-home");
            return Err(String::from("bench forget: the board is no home's device"));
        };
        let done = home::forget(&mut self.flash.messages, address, &|| ())
            .map_err(|e| format!("bench forget: {e:?}"))?;
        self.log.line(
            "[BENCH] ",
            &format!(
                "forget=done flagged={} unflagged={} marks={} entries={} frames={} sectors={} summary_erased={}",
                done.flagged,
                done.unflagged,
                done.marks,
                done.store.entries,
                done.store.frames,
                done.store.sectors,
                done.summary_erased
            ),
        );
        // The old shelf and model go before the reopen builds the next,
        // as the bench image frees them (1603, `Handheld::bench_release`).
        self.shelf = None;
        *self.model = home::model(address);
        let mut outgoing = Outgoing::default();
        let (model, shelf) = open_store(
            &mut self.flash,
            address,
            self.arrivals,
            &mut self.log,
            &mut outgoing,
        )?;
        *self.model = model;
        self.shelf = Some(shelf);
        self.outgoing = outgoing;
        self.row = 0;
        self.asked.extend(house::set_size(
            self.overview.size,
            &mut self.model,
            &mut self.renderer,
        ));
        self.log.line("[BENCH] ", "forget=reopened");
        self.owed = true;
        self.settle();
        Ok(())
    }

    // -----------------------------------------------------------------
    // The UI side (`ui_main`)
    // -----------------------------------------------------------------

    fn look(&self) -> Look {
        Look {
            row: self.row,
            home: self.shown.word,
            fault: self.shown.fault,
            map: self.shown.map,
            ..Look::of(&self.model)
        }
    }

    fn next_id(&mut self) -> [u8; 16] {
        self.counter += 1;
        id_of(&self.seed, self.counter)
    }

    /// `route_out`: what a fold left, routed, its asks into `wants`.
    /// `held`: a pulled page's chunk that is not its last, whose rows
    /// stay owed to the route after the last (1591, `home::route_held`).
    fn route_out(
        &mut self,
        effects: Vec<loquax::ui::UiEffect>,
        wants: &mut Wants,
        held: bool,
    ) -> bool {
        // The ids as the board draws them: its boot's seed hashed with a
        // counter, one per verb the route asks.
        let seed = self.seed;
        let mut counter = self.counter;
        let mut take = || {
            counter += 1;
            id_of(&seed, counter)
        };
        let routed = match held {
            true => home::route_held(&mut self.model, effects, &mut take),
            false => home::route(&mut self.model, effects, &mut take),
        };
        self.counter = counter;
        for line in routed.notices {
            self.log.line("[UI] ", &format!("notice=\"{line}\""));
        }
        for name in routed.unrouted {
            self.log.line("[UI] ", &format!("unrouted={name}"));
        }
        wants.verbs.extend(routed.verbs);
        wants.marks.extend(routed.marks);
        wants
            .rows
            .retain(|row| !routed.rows.iter().any(|new| new.peer == row.peer));
        wants.rows.extend(routed.rows);
        wants.gone.extend(routed.gone);
        for peer in routed.loads {
            if !wants.loads.contains(&peer) {
                wants.loads.push(peer);
            }
        }
        routed.redraw
    }

    /// One turn of the UI side; whether it did anything, and the
    /// milliseconds of its draw and its keys.
    fn ui_turn(&mut self) -> (bool, u64, u64) {
        let mut dirty = false;
        let mut busy = false;
        let mut wants = Wants::default();
        Self::release_presses(&mut self.scheduled, &mut self.presses, &self.kb);
        // What the last size set asked (1546): the boot's among them.
        let asked = std::mem::take(&mut self.asked);
        if !asked.is_empty() {
            busy = true;
            dirty |= self.route_out(asked, &mut wants, false);
        }
        // The model's clocks once a second (1541), as the board's loop
        // tells them. A turn that only told the time is not busy.
        let (ticked, first_day) = self.reader.tick(&mut self.model, uptime_ms());
        dirty |= self.route_out(ticked, &mut wants, false) || first_day;
        // The drain as `ui_main`'s (1575): what the I/O side handed over,
        // in order; a pulled page folded until the budget is spent or a
        // press waits, its rest the next turn's, and the turn that folds
        // a page's last entry ends its drain there.
        let start = uptime_us();
        let deadline = self.fold_budget_us.map_or(u64::MAX, |b| start + b);
        let budgeted = self.fold_budget_us.is_some();
        loop {
            if self.folding.busy() {
                busy = true;
                let cost = self.fold_cost_us;
                let (scheduled, presses, kb) = (&mut self.scheduled, &mut self.presses, &self.kb);
                let chunk = self
                    .folding
                    .fold(&mut self.model, &uptime_us, deadline, &mut || {
                        // The entry's cost on the board, then the press
                        // asked, as the board asks its queue.
                        advance_us(cost);
                        Self::release_presses(scheduled, presses, kb);
                        budgeted && presses.waiting()
                    });
                if self.route_out(chunk.effects, &mut wants, chunk.done.is_none()) {
                    dirty = true;
                }
                if chunk.done.is_some() {
                    wants.pages += 1;
                    break;
                }
                if self.folding.busy() {
                    break;
                }
            }
            Self::release_presses(&mut self.scheduled, &mut self.presses, &self.kb);
            if uptime_us() >= deadline || (budgeted && self.presses.waiting()) {
                break;
            }
            let Some(message) = self.to_ui.pop_front() else {
                break;
            };
            busy = true;
            match message {
                ToUi::Entries(entries, address) => {
                    self.reader.saw(&entries, uptime_ms());
                    self.folding.begin(entries, address);
                }
                ToUi::Page {
                    peer,
                    entries,
                    earlier,
                    address,
                } => {
                    self.reader.saw(&entries, uptime_ms());
                    let effects = home::page_in(&mut self.model, peer, &entries, earlier, &address);
                    self.route_out(effects, &mut wants, false);
                    dirty = true;
                }
                ToUi::Fault(fault) => {
                    if fault != self.shown.fault {
                        self.shown.fault = fault;
                        dirty = true;
                    }
                }
            }
        }
        let drain_ms = (uptime_us() - start) / 1000;
        if drain_ms > SLOW_TURN_MS {
            self.log.line(
                "[LOOP] ",
                &format!("core=ui turn_ms={drain_ms} stage=drain drain={drain_ms}"),
            );
        }

        Self::release_presses(&mut self.scheduled, &mut self.presses, &self.kb);
        let t_keys = std::time::Instant::now();
        while let Some(press) = self.presses.pop() {
            busy = true;
            let ms = press.ms;
            self.keys_owed.push(u64::from(ms));
            self.log.raw(&kbraw::raw_line(ms, press.answer()));
            let mut events = Vec::new();
            let mut command = None;
            match self.kb.read(press.answer()) {
                Read::Idle => {}
                Read::Mixed => self.log.line("[KB] ", "answer=mixed"),
                Read::Code(code) => {
                    if let Some(key) = Key::of(code) {
                        let line = keyboard::key_line(ms, code, key);
                        self.log.raw(&line);
                        self.last_key = Some(line);
                        // The house key and its settings page (1532,
                        // 1561); the pairing has the keys while it shows.
                        let took = match self.shown.pairing {
                            None => {
                                let page = house::page(
                                    &self.model,
                                    self.overview.size,
                                    env!("CARGO_PKG_VERSION"),
                                );
                                self.overview.press(key, &page)
                            }
                            Some(_) => None,
                        };
                        if let Some(did) = took {
                            dirty |= self.did(did);
                            continue;
                        }
                        events.extend(key.event());
                        command = key.command();
                    }
                }
            }
            if let (Some(command), None) = (command, self.shown.pairing.as_ref()) {
                keyboard::run(&mut self.model, &mut self.row, command);
                dirty = true;
            }
            for event in events {
                dirty = true;
                if let Some(pairing) = self.shown.pairing.as_mut() {
                    if let Some(code) = pairing.key(&event) {
                        self.from_ui.push_back(FromUi::Code(code.encode()));
                    }
                    if event.code == KeyCode::Esc && event.kind != KeyEventKind::Release {
                        self.shown.pairing = None;
                    }
                    continue;
                }
                if !map::bound(&self.model, &event) {
                    self.chrome.nudge(uptime_ms());
                }
                let effects = group::press(&mut self.model, &mut self.row, &event);
                if self.route_out(effects, &mut wants, false) {
                    self.renderer.panel().forget_flushed();
                    dirty = true;
                }
            }
        }
        let keys_ms = t_keys.elapsed().as_millis() as u64;

        if !wants.verbs.is_empty() || !wants.marks.is_empty() {
            self.from_ui.push_back(FromUi::Asks {
                verbs: std::mem::take(&mut wants.verbs),
                marks: std::mem::take(&mut wants.marks),
            });
        }
        // Held while a page is part-folded (1575), as the board holds them.
        if self.folding.rows_due(&mut wants.rows, &mut wants.gone)
            && (!wants.rows.is_empty() || !wants.gone.is_empty() || wants.pages > 0)
        {
            self.from_ui.push_back(FromUi::Rows {
                rows: std::mem::take(&mut wants.rows),
                gone: std::mem::take(&mut wants.gone),
                pages: std::mem::take(&mut wants.pages),
            });
        }
        for peer in std::mem::take(&mut wants.loads) {
            self.from_ui.push_back(FromUi::Load(peer));
        }

        let t_draw = std::time::Instant::now();
        dirty |= std::mem::take(&mut self.owed);
        dirty |= self.drawn.due(&self.model);
        let now = uptime_ms();
        self.chrome.seen(map::shown(&self.model), now);
        let overlay = self
            .chrome
            .overlay(now, || !self.model.map_pins().is_empty());
        if overlay != self.shown.map {
            self.shown.map = overlay;
            dirty = true;
        }
        // The keys this turn took are answered by its frame (1575): the
        // walker's key_ms, a press's time to the flush after it.
        for at in std::mem::take(&mut self.keys_owed) {
            self.log.line(
                "[SIM] ",
                &format!("key_ms={}", uptime_ms().saturating_sub(at)),
            );
        }
        if dirty {
            self.redraw();
            // What the frame showed of the open chat (1541); its asks
            // route on the next turn, as the board's loop routes them.
            let said = self
                .reader
                .say(&mut self.model, self.renderer.seen(), uptime_ms());
            self.asked.extend(said);
            self.drawn.drawn(&self.model);
        }
        (busy || dirty, t_draw.elapsed().as_millis() as u64, keys_ms)
    }

    /// What the overview did (1532): the size set on the renderer and
    /// the model, said in a `[TYPE]` line and sent for the record.
    /// Whether the screen changed.
    fn did(&mut self, did: Did) -> bool {
        if let Some(size) = did.size {
            let asked = house::set_size(size, &mut self.model, &mut self.renderer);
            self.asked.extend(asked);
            self.log
                .line("[TYPE] ", &format!("size={} via={}", size.name(), did.via));
            self.from_ui.push_back(FromUi::Type(size));
        }
        did.redraw
    }

    /// `redraw`: the model, the pairing over it or the map, the rows that
    /// changed "flushed", the `[MAP]` and `[PAGE]` lines.
    fn redraw(&mut self) {
        let look = self.look();
        let overview = self.overview;
        let drawn = match &self.shown.pairing {
            Some(pairing) => pair::draw(&mut self.renderer, pairing, self.shown.word, &look),
            None if overview.open => {
                let page = house::page(&self.model, overview.size, env!("CARGO_PKG_VERSION"));
                self.renderer
                    .text(|cols, rows| overview.lines(&page, &look, cols, rows))
            }
            None => self.renderer.show(&self.model, &look, &mut self.map),
        };
        if drawn.is_err() {
            self.log.line("[DISP] ", "draw=failed");
            return;
        }
        if let Some(stats) = self.renderer.take_map_stats() {
            let ms = |us: u64| format!("{}.{:03}", us / 1000, us % 1000);
            self.log.line(
                "[MAP] ",
                &format!(
                    "zoom={} picture_ms={} names_ms={} names={} dropped={}",
                    stats.zoom,
                    ms(stats.picture_us),
                    ms(stats.names_us),
                    stats.names.drawn,
                    stats.names.dropped()
                ),
            );
        }
        let at = match self.shown.pairing.is_none() && overview.open {
            true => page::house(overview.at),
            false => page::of(
                &self.model,
                self.row,
                self.shown.pairing.is_some(),
                &self.renderer.window(),
            ),
        };
        let moved = self.said.say(at);
        if let Some((first, last)) = self.renderer.panel().take_dirty() {
            // No SPI: the flush is the frame itself, and takes no time.
            self.frames += 1;
            self.log
                .line("[DISP] ", &format!("flush rows={first}..{last} ms=0.000"));
        }
        if let Some(body) = moved {
            self.log.line("[PAGE] ", &body);
            self.last_page = Some(body);
        }
    }

    // -----------------------------------------------------------------
    // The I/O side (`main`'s loop, `handheld.rs`'s store half)
    // -----------------------------------------------------------------

    fn io_turn(&mut self) -> bool {
        let mut busy = false;
        if self.unsaved > 0 && !self.faulted {
            self.faulted = true;
            self.to_ui.push_back(ToUi::Fault(Some(StrId::M9NotSaved)));
        }
        while let Some(message) = self.from_ui.pop_front() {
            busy = true;
            match message {
                FromUi::Asks { verbs, marks } => self.ask(verbs, &marks),
                FromUi::Rows { rows, gone, pages } => {
                    if let Some(shelf) = self.shelf.as_mut() {
                        shelf.said(rows, gone);
                        shelf.pages_owed = shelf.pages_owed.saturating_sub(pages);
                    }
                    self.flush();
                }
                FromUi::Load(peer) => {
                    let want = home::page_entries();
                    if let (Some(address), Some((entries, earlier))) =
                        (self.flash.home, self.load(peer, want))
                    {
                        self.to_ui.push_back(ToUi::Page {
                            peer,
                            entries,
                            earlier,
                            address,
                        });
                    }
                }
                FromUi::Type(size) => {
                    // The record's flag (1532): kept with a home, as the
                    // board's record keeps it; a board without one keeps
                    // it until the power goes.
                    let kept = match self.flash.home {
                        Some(_) => {
                            self.flash.type_size = size.record();
                            "yes"
                        }
                        None => "no reason=no-record",
                    };
                    self.log
                        .line("[STORE] ", &format!("type={} kept={kept}", size.name()));
                }
                FromUi::Code(code) => {
                    // The join needs the node and a home: none in the sim.
                    let body = format!(
                        "code=typed chars={} taken=no reason=sim-has-no-link",
                        code.chars().count()
                    );
                    self.log.line("[HOME] ", &body);
                }
            }
        }
        busy
    }

    /// `Handheld::ask`: the marks kept, each verb kept under a fresh id
    /// and queued.
    fn ask(&mut self, verbs: Vec<Verb>, marks: &[Mark]) {
        if self.flash.home.is_none() {
            return;
        }
        if let (false, Some(shelf)) = (marks.is_empty(), self.shelf.as_mut()) {
            match home::keep_marks(shelf, &mut self.flash.messages, marks) {
                Ok(done) => self.log.line(
                    "[STORE] ",
                    &format!(
                        "marks={} writes={} erased={}",
                        done.entries, done.writes, done.erased
                    ),
                ),
                Err(error) => {
                    self.unsaved += 1;
                    self.log
                        .line("[STORE] ", &format!("marks=unsaved error={error:?}"));
                }
            }
        }
        for verb in verbs {
            let id = self.next_id();
            if let (Some(shelf), Some((tag, bytes))) =
                (self.shelf.as_mut(), board::ask_bytes(&verb))
            {
                if let Err(error) = shelf.keep_ask(&mut self.flash.messages, &id, tag, &bytes) {
                    self.unsaved += 1;
                    self.log
                        .line("[STORE] ", &format!("ask=unsaved error={error:?}"));
                }
            }
            self.outgoing.push(id, verb);
        }
    }

    /// What the board's quiet turns would write of a summary backlog
    /// (1591), a sector a call, until none is owed or a call wrote
    /// nothing.
    fn drain_backlog(&mut self) {
        while self.shelf.as_ref().is_some_and(|s| s.owes()) {
            let kept = self.log.kept.len();
            self.flush();
            if self.log.kept.len() == kept {
                return;
            }
        }
    }

    /// `Client::flush`: the summary log's records and state, one bounded
    /// write a call (1591, 1597).
    fn flush(&mut self) {
        self.flush_with(None);
    }

    /// `Client::flush_with`: in a quiet turn (`Some` of the clock), a
    /// backlog's next sector not before `home::BACKLOG_GAP_MS` later.
    fn flush_with(&mut self, quiet: Option<u64>) {
        let Some(shelf) = self.shelf.as_mut() else {
            return;
        };
        let outgoing = &self.outgoing;
        let written = match quiet {
            Some(now_ms) => {
                shelf.quiet_flush(&mut self.flash.messages, || outgoing.pending(), now_ms)
            }
            None => shelf.flush(&mut self.flash.messages, || outgoing.pending()),
        };
        match written {
            Ok(wrote) if wrote.appended().writes > 0 => {
                let done = wrote.appended();
                self.log.line(
                    "[STORE] ",
                    &format!(
                        "summary records={} writes={} erased={} left={}",
                        done.entries,
                        done.writes,
                        done.erased,
                        u8::from(wrote.partial())
                    ),
                )
            }
            Ok(_) => {}
            Err(error) => self
                .log
                .line("[STORE] ", &format!("summary=unsaved error={error:?}")),
        }
    }

    /// `Handheld::load`: a conversation's next page from the ring.
    fn load(&mut self, peer: [u8; 16], want: usize) -> Option<(Vec<Entry>, usize)> {
        let address = self.flash.home?;
        let shelf = self.shelf.as_mut()?;
        let t0 = uptime_ms();
        match shelf.page(&mut self.flash.messages, peer, want, &address) {
            Ok((entries, earlier)) => {
                let cost = shelf.index.last_page;
                self.log.line(
                    "[STORE] ",
                    &format!(
                        "page_read entries={} messages={} marks={} sectors={} earlier={earlier} ms={}",
                        entries.len(),
                        cost.messages,
                        cost.marks,
                        cost.sectors,
                        uptime_ms().saturating_sub(t0)
                    ),
                );
                Some((entries, earlier))
            }
            Err(error) => {
                self.log
                    .line("[STORE] ", &format!("page_read=failed error={error:?}"));
                None
            }
        }
    }

    /// `Handheld::quiet`: the summary flushed, then one erase ahead.
    fn quiet(&mut self) {
        self.flush_with(Some(uptime_ms()));
        let Some(shelf) = self.shelf.as_mut() else {
            return;
        };
        let erases = shelf.ring.erases();
        match shelf.quiet(&mut self.flash.messages) {
            Ok(true) => {
                let body = format!(
                    "pre_erased={} erases={}",
                    shelf.ring.erases() - erases,
                    shelf.ring.erases()
                );
                self.log.line("[STORE] ", &body);
            }
            Ok(false) => {}
            Err(error) => self
                .log
                .line("[STORE] ", &format!("pre_erase=failed error={error:?}")),
        }
    }

    /// The `[LOOP]` line of a slow turn: the host's own milliseconds.
    fn say_turn(&mut self, draw: u64, keys: u64, take: u64) {
        let total = draw + keys + take;
        if total <= SLOW_TURN_MS {
            return;
        }
        let stages = [("take", take), ("draw", draw), ("keys", keys)];
        let slowest = stages
            .iter()
            .max_by_key(|(_, ms)| *ms)
            .map_or("none", |(name, _)| name);
        let entries = self.shelf.as_ref().map_or(0, |shelf| shelf.ring.entries());
        self.log.line(
            "[LOOP] ",
            &format!(
                "turn_ms={total} stage={slowest} host=0 read=0 pkt=0 ctl=0 node=0 take={take} draw={draw} keys={keys} entries={entries}"
            ),
        );
    }
}

/// A client id: the boot's seed and a counter, hashed (`ui_main`'s `ids`).
/// The store restored for the home at `address` (the board's
/// `Handheld::client`): the restore's lines said, the asks it kept queued
/// again in `outgoing`, the model and the shelf. At boot, and again after
/// a `bench forget` (1562, `Handheld::bench_reopen`).
fn open_store(
    flash: &mut Flash,
    address: [u8; 16],
    arrivals: Arrivals,
    log: &mut Log,
    outgoing: &mut Outgoing,
) -> Result<(UiModel<Field>, Shelf), String> {
    let t0 = uptime_ms();
    let boot = home::restore_shown(&mut flash.messages, address, &uptime_us, arrivals)
        .map_err(|e| format!("the store did not restore: {e:?}"))?;
    let how = boot.how.word();
    log.line(
        "[STORE] ",
        &format!(
            "sectors={} used={} entries={} cursor={} torn={} foreign={} asks={} restore={how} restore_ms={}",
            boot.shelf.ring.sectors(),
            boot.shelf.used(),
            boot.restored.entries,
            boot.shelf.cursor(),
            boot.restored.torn,
            boot.restored.foreign,
            boot.asks.len(),
            uptime_ms().saturating_sub(t0)
        ),
    );
    for line in boot.spent.lines(boot.how, boot.decoded) {
        log.line("[STORE] ", &line);
    }
    match boot.how {
        How::Resumed { records, tail } => log.line(
            "[STORE] ",
            &format!(
                "index=summaries records={records} tail={tail} conversations={}",
                boot.model.conversations.len()
            ),
        ),
        How::Rebuilt { why } => log.line(
            "[STORE] ",
            &format!(
                "index=rebuilt why={why} conversations={}",
                boot.model.conversations.len()
            ),
        ),
        How::Whole => {}
    }
    for ask in boot.asks {
        match board::ask_verb(ask.tag, &ask.request) {
            Some(verb) => outgoing.push(ask.id, verb),
            None => log.line("[HOME] ", &format!("ask=unread tag={}", ask.tag)),
        }
    }
    log.line(
        "[HOME] ",
        &format!("client=sim link=none waiting={}", outgoing.len()),
    );
    Ok((boot.model, boot.shelf))
}

fn id_of(seed: &[u8; 32], counter: u64) -> [u8; 16] {
    let mut buf = [0u8; 40];
    buf[..32].copy_from_slice(seed);
    buf[32..].copy_from_slice(&counter.to_le_bytes());
    let hash = leviculum_core::crypto::sha256(&buf);
    let mut id = [0u8; 16];
    id.copy_from_slice(&hash[..16]);
    id
}

fn hex8(bytes: &[u8; 16]) -> String {
    bytes[..4].iter().map(|b| format!("{b:02x}")).collect()
}
