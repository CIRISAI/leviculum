//! Loquax on the ThinkNode M9: the first image that boots the renderer
//! (order 1361, slice four of `docs/src/concepts/handheld-messenger.md`).
//!
//! What it does, and it is the whole of what this step claims: bring the
//! SoC up, map the 8 MB PSRAM and give it to the heap, light the ST7789,
//! draw the chats list of a fixture model (`loquax_m9::fixture`, no
//! network) with the renderer the host pins its goldens with, and let the
//! keyboard move through it: open a chat, scroll, type, go back, and work
//! a group's screens with the keys of `loquax_m9::group` (slice six, 1366;
//! the fixture's groups are reached through `FRAME_DUMP`, nothing is asked
//! of a home), and open the map of the fixture's positions over the map
//! core's Florence region, read in place from the image (slice seven,
//! 1375: `M`, the arrows, `+` and `-`, Space).
//!
//! Since 1414 the image is also a Reticulum node and a client of its home
//! (`leviculum_esp::handheld`): its identity in flash, one interface, the
//! host's serial line, and the client record that makes it a device of a
//! home. Reticulum packets are HDLC frames on the same UART as the log
//! lines and the control envelope, under the rules of
//! `leviculum-nrf/serial-mux`: lines whole between frames and free of the
//! four special bytes, frames with XON and XOFF escaped, and the capability
//! report stating `serial_mux=1` so lnsd does its half. A frame that is not
//! the envelope is a packet for the node.
//!
//! Since 1415 the board becomes that device: a board without a client
//! record opens on the pairing screen, where the home's admission code is
//! typed on the keys (Back leaves it for the fixture), or the code comes
//! over the line as a `TYPE_HOME_CODE` frame (`lnflash --home <code>
//! --port <tty>`). Either way the record is written, the board joins, and
//! from then on it draws its home's chats from what it pulls, says the
//! home in the top bar (joining, fetched N of M, unreachable) and sends a
//! line typed in a chat through the home.
//!
//! **Nothing goes on the air.** The LR1110's chip select is parked high so
//! it ignores the panel's traffic on the shared bus; no opcode reaches it.
//! The radio is the node's second interface and the next step.
//!
//! Over the debug UART (the CH340K, 115200 8N1), in boot order:
//!
//! ```text
//! [FW_BUILD] git_sha=<sha> dirty=<b> version=<v> t=<ms>
//! [BOARD] id=M9 name=Elecrow ThinkNode M9 heap=<psram bytes> t=<ms>
//! [HEAP] region=psram start=<addr> bytes=<n> t=<ms>
//! [STACK] bytes=<n> t=<ms>
//! [FLASH] identity=<offset|none/why> client=<offset|none/why> messages=<offset|none/why> t=<ms>  (1414, 1476)
//! [IDENTITY] identity=<hash> probe=none lxmf=none source=<flash|generated> stored=<yes|no> t=<ms>
//! [CENSUS] node held=<n> box=<n> links=8 max_answer=262144 t=<ms>
//! [NODE] state=up iface=serial hw_mtu=564 serial_mux=1 t=<ms>
//! [DISP] ST7789 320x240 madctl=0xb0 spi_mhz=40 state=up t=<ms>
//! [DISP] splash rows=0..239 ms=<ms> backlight=on t=<ms>   (1485; or splash=failed err=..., backlight=off)
//! [KB] addr=0x6d found=yes ... protocol=codes reg=0x01 len=8 poll_ms=10 t=<ms>
//! [CENSUS] renderer held=<n> budget=220000 verdict=ok t=<ms>
//! [CENSUS] screen=chats scene_held=... verdict=ok t=<ms>
//! [CENSUS] screen=chat scene_held=... verdict=ok t=<ms>
//! [CENSUS] screen=<groups|invite|members|admin|ask|pick|name> ...  (1366)
//! [CENSUS] map held=<n> budget=230000 verdict=ok t=<ms>         (1375)
//! [CENSUS] screen=<map|pan> ... verdict=ok t=<ms>               (1375)
//! [CENSUS] map_kept held=153600 t=<ms>   the map's picture kept between
//!                           draws from here on (1507), after the census
//! [MAP] zoom=<z> picture_ms=.. names_ms=.. names=<n> dropped=<n> kept=<0|1>
//!                           every map draw (1531); kept=1 copied the kept
//!                           picture back, a page switch or a key that moved
//!                           no view and no pin (1507), and its ms are 0
//! [CENSUS] session=pull held=<n> peak=<n> stack=<n> verdict=ok pages=<n> entries=30 t=<ms>
//! [HOME] client=none page=<n> t=<ms>        or client=yes admitted=... cursor=...
//! [STORE] sectors=896 used=<n> entries=<n> cursor=<seq> torn=<n> foreign=<n> asks=<n> restore=<resumed|rebuilt|whole> restore_ms=<ms>  (1476, 1521)
//! [STORE] restore.a <how> decoded=<n> read_us=.. crc_us=.. decode_us=.. unpack_us=.. step_us=.. finish_us=..  (1521, where the boot's time went)
//! [STORE] restore.b <how> drain_us=.. rows_us=.. total_us=.. unnamed_us=.. per_1000_ms read=.. crc=.. decode=.. unpack=.. step=..  (1521c, the rest of it)
//! [STORE] index=summaries records=<n> conversations=<n>   or index=rebuilt why=<no-state|ring-moved> ...  (1521)
//! [HOME] store=empty cursor=reset was=<n> cursor=0   (1476, a record whose store is gone; or store=behind/ahead)
//! [HOME] page ... taken=<n> of=<m> / pulled ... saved=store / joined=yes ... / failed ...   (as the client asks; the record is not written for a pull, 1463)
//! [HOME] code=typed / code=taken saved=yes / code=refused ...   (1415, a code taken in)
//! [STORE] kept=<n> writes=<n> erased=<n> cursor=<seq> used=<n> entries=<n>   (1476, every page pulled)
//! [STORE] summary records=<n> writes=<n> erased=<n>   (1521, the rows the folds changed, and the ring's state)
//! [STORE] marks=<n> writes=<n> erased=<n>   (1542, the read marks a fold kept in the ring)
//! [STORE] page_read entries=<n> earlier=<n> ms=<ms>   (1521, a conversation opened or scrolled to its top)
//! [HOME] sent id=<8 hex> waiting=<n> / send=failed ...         (1415, a line typed in a chat)
//! [DISP] flush rows=0..239 ms=<ms> runs=<n> bytes=<n> t=<ms>
//!                           when a flush's last byte is out (1507): rows
//!                           from the first run's first to the last's
//!                           last, each run its own window, sent by DMA
//!                           straight out of the frame in PSRAM; ms from
//!                           its start to its end, the loop running on
//! [DISP] dma=bounce reason=<e>   the descriptors would not align: every
//!                           flush goes through the driver's bounce buffer
//! [DISP] flush=failed err=<spi:..|dma:..>   the frame is owed whole
//! [DISP] pace=<te|clock> edges=<n> merged=<n> t=<ms>
//!                           when what paces the draws changes (1507,
//!                           `loquax_m9::pace`): te is the panel's blank on
//!                           GPIO19, clock the 60 Hz fallback of a silent
//!                           line; merged counts the turns whose change
//!                           waited for the next frame's one draw
//! [PAGE] name=<page> cursor=<n> top=<n> [zoom=<z>] t=<ms>
//!                           after the draw, when the page or its cursor
//!                           moved (1533, `loquax_m9::page`): what a bench
//!                           walk asserts without a dump
//! [UART] rx_lost=<n> overrun=<n> deframe_drop=<n> rx_err=<n> loop_gap_max_ms=<n> stack_free=<n> tx_drop=<n> tx_drop_bytes=<n> t=<ms>
//!                           on every loss and every five seconds (1440);
//!                           stack_free is core 0's stack never touched
//!                           since the boot's census, above the guard (1521b),
//!                           `unset` before the first measure (1521c); the
//!                           build's STACK_HEADROOM stack= is the same
//!                           section's whole size, need= a floor borrowed
//!                           from the LNode's node peak (#50), not a measure;
//!                           tx_drop the records the send ring dropped, its
//!                           oldest first, while the loop wrote faster than
//!                           the line took (1507)
//! [IDLE] io_pct=<n> ui_pct=<n> window_ms=<n> t=<ms>
//!                           every five seconds (1507, `leviculum_esp::idle`):
//!                           the share of the window each core waited for
//!                           its next interrupt (WAITI) instead of spinning;
//!                           core 0 waits on a quiet turn when the keyboard's
//!                           tick runs, core 1 when no flush, no fold and no
//!                           due draw is left, woken by TE or its 16 ms tick
//! [PREV_RUN] reason=<wdt0|wdt1|panic|power|..> core0_stage=.. core0_lock=.. core0_isr=.. core1_stage=.. core1_lock=.. peer=.. turns=<c0>/<c1>
//!                           at boot: where each core was when the last run
//!                           stopped (1534, `leviculum_esp::stall`); a power
//!                           cycle or an EN reset says record=none
//! [WATCHDOG] core0=mwdt0 core1=mwdt1 timeout_ms=5000 record=rtc-slow
//!                           once, as core 0's loop arms its watchdog (1534)
//! [BENCH] inject=on        at boot and with the banner, only in the bench
//!                           image (`LEVICULUM_M9_BENCH=1`, 1533): a
//!                           `TYPE_KEY_INJECT` frame presses a key, the
//!                           module `bench` at the end of this file
//! [BENCH] owe=<armed|refused|ring-closed|fired> flags=<0x..>
//!                           the bench image's `TYPE_BENCH_OWE` (1540): the
//!                           next open writes every row again, `flags` says
//!                           which store's write erases a sector first; it
//!                           fires on the open's Load, read mark or read
//!                           row, whichever comes first (1542)
//! [BENCH] arrive=<sent|refused|folded> row=<n> of=<n> peer=<8 hex> [unread=<n>]
//!                           `TYPE_BENCH_ARRIVE` (1540): a message came into
//!                           the chat at that row, kept and folded as a
//!                           pulled one, its chat selected
//! [OPEN] peer=<8 hex> entries=<n> wait_ms=.. page_read_ms=.. decode_ms=.. fold_ms=.. scene_ms=.. draw_ms=.. flush_ms=..
//!                           bench image only (1540): where an opened
//!                           chat's page went, from the load asked to the
//!                           frame flushed; wait is the load's way through
//!                           core 0, page_read the ring read inside it
//! [CORES] census=done waited_ms=<n>   core 0 held still while the UI
//!                           core's census ran (1521b), or census=late
//! KBRAW <ms> <hex>          one per answer that is no idle one: a timer
//!                           polls every 10 ms into a queue the loop takes
//!                           (1463); <ms> is the poll's time, not the line's
//! KEY <ms> code=0x6b name=k after a KBRAW line that carries a press (1427)
//! [KB] answer=mixed t=<ms>  after one whose bytes disagree
//! [KB] queue_lost=<n> read_failed=<n>   when either count moved (1463)
//! [LOOP] turn_ms=<n> stage=<host|node|take|draw|keys> host=.. node=.. take=.. draw=.. keys=.. entries=<n>
//!                           a turn of the loop longer than 100 ms (1463)
//! ```
//!
//! and the banner pair at boot and every five seconds for the first two
//! minutes, the window `lnflash` reads it in, never after (1521b: it
//! stood beside every `[UART]` line for good). A `FRAME_DUMP` control frame
//! (`7e a4 a5 18 00 00 7e`, or `7e a4 a5 18 00 01 <screen> 7e`) is acked
//! and answered with the `PBM` dump of `loquax_m9::dump`, its lines written
//! in one go so no HDLC frame lands between two of them (1533); `<screen>` is a
//! byte of `fixture::SCREENS`, 0 to 10, or `pair::DUMP_BYTE`, 11, the
//! pairing screen (1440).
//!
//! The pin handles are moved out of `Peripherals` through the board
//! module's type aliases, as `heltec_v4.rs` does: a compile-time check that
//! this binary and the board file agree about every net.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::ptr::addr_of_mut;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use critical_section::Mutex;

use esp_hal::peripherals::{TIMG0, TIMG1};
use esp_hal::rng::{Trng, TrngSource};
use esp_hal::system::{CpuControl, Stack};
use esp_hal::{
    delay::Delay,
    dma::{aligned::DmaAlignedMut, DmaDescriptor, DmaTxBuf},
    dma_descriptors_chunk_size, dma_rx_buffer, dma_tx_buffer,
    gpio::{Event, Input, InputConfig, Io, Level, Output, OutputConfig},
    i2c::master::{Config as I2cConfig, I2c},
    main,
    psram::{Psram, PsramConfig, PsramMode, PsramSize, SpiRamFreq},
    spi::{
        master::{Config as SpiConfig, Spi, SpiDma, SpiDmaTransfer},
        Mode,
    },
    time::{Duration, Rate},
    timer::{
        timg::{TimerGroup, Wdt},
        PeriodicTimer,
    },
    uart::Uart,
    Blocking,
};
use leviculum_core::envelope::{self, EnvelopeError};
use leviculum_core::framing::hdlc::{DeframeResult, Deframer};
use leviculum_esp::flash::Stores;
use leviculum_esp::handheld::{self, CodeRefusal, Handheld};
use leviculum_esp::stall::{self, Core, Isr, Lock, Stage, Watchdog};
use leviculum_esp::{boards::thinknode_m9 as m9, uptime_ms, UartLog};
use leviculum_ui::input::{KeyCode, KeyEventKind};
use loquax::home::ask::Verb;
use loquax::home::board::Word;
use loquax::home::wire::{Entry, Mark};
use loquax::ui::listing::Row;
use loquax::ui::strings::{StrId, Text};
use loquax::ui::UiModel;
use loquax_m9::census::{self, Probe, Step};
use loquax_m9::field::Field;
use loquax_m9::fixture::{self, Screen};
use loquax_m9::font::Size;
use loquax_m9::frame::{Frame, HEIGHT, WIDTH};
use loquax_m9::home::Answer;
use loquax_m9::house::{self, Did, Overview};
use loquax_m9::kbraw::{self, Presses, Protocol, Read};
use loquax_m9::keyboard::{self, Key, POLL_MS};
use loquax_m9::map::{self, Map};
use loquax_m9::pace::{self, Pace};
use loquax_m9::page;
use loquax_m9::pair::{self, Pairing};
use loquax_m9::read::Reader;
use loquax_m9::render::Renderer;
use loquax_m9::splash::{self, Panel as _};
use loquax_m9::{dump, group, home, screen};

esp_bootloader_esp_idf::esp_app_desc!();

/// How often the board re-states what it is: the five seconds every
/// leviculum board uses, so a reader that attaches late still reads it.
const BANNER_INTERVAL_MS: u64 = 5_000;

/// The banner pair repeats only this long after boot (1521b): the window
/// `lnflash`'s verify reads `[FW_BUILD]` in, as the nRF boards keep it.
const BANNER_WINDOW_MS: u64 = 120_000;

/// While the UI core's census runs (1521b), core 0 holds still: no node,
/// no flash (an erase parks the UI core), no allocation in the census's
/// count. Window 7 measured the census beside a running I/O loop: ten
/// times slower, and `draw_held` counted core 0's allocations.
static CENSUS_RUNNING: AtomicBool = AtomicBool::new(true);

/// How long core 0 waits for the census before it runs anyway: a UI core
/// that stalled must not take the line down with it.
const CENSUS_WAIT_MS: u64 = 30_000;

/// The least PSRAM the UI heap wants: the plan's 1.1 MB, model, scene,
/// renderer and two frames on the 64-bit host
/// (`handheld-messenger.md`, "Memory budgets, measured").
const HEAP_MIN: usize = 1_100_000 + (1 << 20);

/// The keyboard's poll, the interrupt's once the boot handed it over
/// (1463): the controller's bus, the timer that ticks every
/// [`POLL_MS`], and the protocol at the address the probe found (1493:
/// a V1.0 board answers at 0x6C, and polling the measured 0x6D there
/// left its keyboard dead). The loop no longer reads the controller; it
/// takes what the interrupt kept in [`KB_PRESSES`], so a key pressed
/// while the loop is busy drawing or storing is read all the same.
struct KbPoll {
    bus: I2c<'static, Blocking>,
    timer: PeriodicTimer<'static, Blocking>,
    protocol: Protocol,
}

static KB_POLL: Mutex<RefCell<Option<KbPoll>>> = Mutex::new(RefCell::new(None));
/// The answers the polls took that were no idle ones.
static KB_PRESSES: Mutex<RefCell<Presses>> = Mutex::new(RefCell::new(Presses::new()));
/// Polls whose I2C transfer failed.
static KB_FAILS: AtomicU32 = AtomicU32::new(0);
/// The keyboard's timer runs: core 0 has an interrupt every
/// [`POLL_MS`] and may wait for it (1507, `leviculum_esp::idle`).
static KB_TICKING: AtomicBool = AtomicBool::new(false);

/// The UI core's own tick (1507): a timer on that core, so it always has
/// an interrupt coming within a frame when it waits, TE line or none.
static UI_TICK: Mutex<RefCell<Option<PeriodicTimer<'static, Blocking>>>> =
    Mutex::new(RefCell::new(None));

/// The UI core's tick period: just under the panel's frame.
const UI_TICK_MS: u64 = 16;

/// The UI core's tick: only that the core wakes.
#[esp_hal::handler]
fn ui_tick() {
    critical_section::with(|cs| {
        if let Some(timer) = UI_TICK.borrow_ref_mut(cs).as_mut() {
            timer.clear_interrupt();
        }
    });
}

/// The panel's TE line (1507), the UI core's interrupt once `ui_main`
/// listens on it: every rising edge is a vertical blank.
static TE_LINE: Mutex<RefCell<Option<Input<'static>>>> = Mutex::new(RefCell::new(None));
/// The blanks counted since boot.
static TE_EDGES: AtomicU32 = AtomicU32::new(0);

/// The GPIO interrupt, bound on the UI core: a TE edge counted. Nothing
/// else on this board listens on a GPIO.
#[esp_hal::handler]
fn te_edge() {
    critical_section::with(|cs| {
        if let Some(te) = TE_LINE.borrow_ref_mut(cs).as_mut() {
            if te.is_interrupt_set() {
                te.clear_interrupt();
                TE_EDGES.fetch_add(1, Ordering::Relaxed);
            }
        }
    });
}

/// The UI core's clock for the pace: microseconds, truncated to the
/// wrapping `u32` [`Pace`] compares.
fn pace_now() -> u32 {
    leviculum_esp::uptime_us() as u32
}

/// The timer's tick: one read of the controller, its answer queued. The
/// transfer is about a millisecond at 100 kHz, inside the critical
/// section; the UART's interrupt has 5.5 ms of slack behind it.
#[esp_hal::handler]
fn kb_tick() {
    stall::isr(Isr::Keyboard);
    critical_section::with(|cs| {
        let mut poll = KB_POLL.borrow_ref_mut(cs);
        let Some(poll) = poll.as_mut() else {
            return;
        };
        poll.timer.clear_interrupt();
        let kb = poll.protocol;
        let mut answer = [0u8; kbraw::MAX_ANSWER];
        let answer = &mut answer[..kb.len];
        let read = match kb.register {
            Some(register) => poll.bus.write_read(kb.address, &[register], answer),
            None => poll.bus.read(kb.address, answer),
        };
        match read {
            Ok(()) => KB_PRESSES
                .borrow_ref_mut(cs)
                .poll(&kb, uptime_ms() as u32, answer),
            Err(_) => {
                KB_FAILS.fetch_add(1, Ordering::Relaxed);
            }
        }
    });
    stall::isr(Isr::None);
}

/// The answers kept since the last call, oldest first, and how many were
/// dropped and failed so far.
fn kb_drain() -> (Vec<kbraw::Press>, u32, u32) {
    stall::locked(Core::Ui, Lock::Keys, || {
        critical_section::with(|cs| {
            let mut presses = KB_PRESSES.borrow_ref_mut(cs);
            let drained = core::iter::from_fn(|| presses.pop()).collect();
            (drained, presses.lost(), KB_FAILS.load(Ordering::Relaxed))
        })
    })
}

// ---------------------------------------------------------------------
// The two cores and the queues between them (1493)
// ---------------------------------------------------------------------
//
// The I/O side keeps core 0 (PRO): the UART and its RX interrupt, the
// keyboard's timer interrupt, the node, the client and every flash
// write. The UI side runs on core 1 (APP): the model, the scene, the
// renderer and the panel's SPI. Between them stand two queues under the
// cross-core critical section, and the keyboard queue [`KB_PRESSES`],
// which the UI side drains directly.
//
// The executor is NO executor: each core runs one blocking loop, and
// the queues are plain `VecDeque`s under `critical_section` (esp-hal's
// multicore implementation is a cross-core spinlock). Why: the
// community port's interrupt executor crashed on this board
// (`thinknode-m9.md`, "The loop"), esp-rtos would add a scheduler and a
// stack per task against a measured stack budget, and after 1463 the
// data flow is already message-shaped — pages, answers and key presses
// in, asks out — so two loops with queues give the decoupling without a
// new machine to trust. Flash stays on core 0; esp-storage's AutoPark
// parks the UI core for each write or erase (the flash routines run
// with the cache off, which core 1 executes through), and core 0 holds
// the critical section across each one, so the UI core is never parked
// holding it and no interrupt here waits on it while the UI core is
// parked (1534, `leviculum_esp::flash`, `parked`).

/// What the I/O core hands the UI core.
enum ToUi {
    /// A fresh model: the restore after a code was taken.
    Model(Box<UiModel<Field>>),
    /// A pulled page's entries for the model.
    Entries(Vec<Entry>, [u8; 16]),
    /// How an ask ended, for the model.
    Answer(Answer),
    /// The top bar's word moved.
    Word(Option<Word>),
    /// An error that is a state (1493), or its end.
    Fault(Option<StrId>),
    /// The pairing screen could not keep the code ([`CodeRefusal`]).
    CodeNote(&'static str),
    /// A `FRAME_DUMP` for the renderer: the screen byte, as the control
    /// frame carried it.
    Dump(Option<u8>),
    /// A conversation's page read from the ring (1521): its entries, how
    /// many older wait, and the home's address they fold under.
    Page {
        peer: [u8; 16],
        entries: Vec<Entry>,
        earlier: usize,
        address: [u8; 16],
    },
    /// The bench's arrival (1540): the row of the list it goes to.
    #[cfg(m9_bench)]
    BenchArrive(u8),
}

/// What the UI core hands the I/O core.
enum FromUi {
    /// A fold's output for the home: the verbs for the ask queue and
    /// the marks among them for the store ([`Handheld::ask`]).
    Asks { verbs: Vec<Verb>, marks: Vec<Mark> },
    /// A formatted log line: prefix and body, printed by the one UART
    /// writer.
    Line(&'static str, String),
    /// A raw line (`KBRAW`, `KEY`).
    Raw(String),
    /// A frame dump's `PBM` lines, whole (1533): written in one go, so no
    /// HDLC frame of the node lands between two of its lines.
    Dump(Vec<String>),
    /// A complete home code typed on the pairing screen.
    Code(String),
    /// The rows of the conversations the folds changed, those deleted,
    /// and how many pulled pages the UI folded since (1521): for the
    /// summary log.
    Rows {
        rows: Vec<Row>,
        gone: Vec<[u8; 16]>,
        pages: u32,
    },
    /// The model asks a conversation's next page (1521).
    Load([u8; 16]),
    /// The reader switched the type (1532): the client record keeps it.
    Type(Size),
    /// The bench's arrival (1540): the chat's peer and the time it is
    /// written at, for the I/O core to keep and hand back.
    #[cfg(m9_bench)]
    BenchArrive { peer: [u8; 16], at: i64 },
}

static TO_UI: Mutex<RefCell<VecDeque<ToUi>>> = Mutex::new(RefCell::new(VecDeque::new()));
static FROM_UI: Mutex<RefCell<VecDeque<FromUi>>> = Mutex::new(RefCell::new(VecDeque::new()));

fn to_ui(message: ToUi) {
    stall::locked(Core::Io, Lock::ToUi, || {
        critical_section::with(|cs| TO_UI.borrow_ref_mut(cs).push_back(message))
    });
}

fn ui_drain() -> Vec<ToUi> {
    stall::locked(Core::Ui, Lock::ToUi, || {
        critical_section::with(|cs| TO_UI.borrow_ref_mut(cs).drain(..).collect())
    })
}

fn io_drain() -> Vec<FromUi> {
    stall::locked(Core::Io, Lock::FromUi, || {
        critical_section::with(|cs| FROM_UI.borrow_ref_mut(cs).drain(..).collect())
    })
}

/// The UI core's way to the log: every line goes through the queue to
/// the one UART writer on core 0.
struct UiLog;

impl UiLog {
    fn line(&mut self, prefix: &'static str, body: String) {
        self.send(FromUi::Line(prefix, body));
    }

    fn raw(&mut self, body: String) {
        self.send(FromUi::Raw(body));
    }

    fn send(&mut self, message: FromUi) {
        stall::locked(Core::Ui, Lock::FromUi, || {
            critical_section::with(|cs| FROM_UI.borrow_ref_mut(cs).push_back(message))
        });
    }
}

/// The UI core's stack: the open chat's census ceilings, scene and draw
/// added up (`census::CHAT`, the 184,000 B the build script holds the
/// one stack to before 1493), rounded up to whole KiB.
const UI_STACK: usize = 184 * 1024;
const _: () = assert!(UI_STACK >= 184_000);

static mut APP_CORE_STACK: Stack<UI_STACK> = Stack::new();

/// A turn of the loop longer than this says where its time went (1463):
/// the bar a key is answered within.
const SLOW_TURN_MS: u64 = 100;

/// What one turn of the loop spent, by stage, in milliseconds.
#[derive(Default)]
struct Turn {
    host: u64,
    node: u64,
    take: u64,
    draw: u64,
    keys: u64,
}

impl Turn {
    /// Say the turn as a `[LOOP]` line when it took longer than
    /// [`SLOW_TURN_MS`].
    fn say(&self, log: &mut UartLog<'_>, entries: u64) {
        let total = self.host + self.node + self.take + self.draw + self.keys;
        if total <= SLOW_TURN_MS {
            return;
        }
        let stages = [
            ("host", self.host),
            ("node", self.node),
            ("take", self.take),
            ("draw", self.draw),
            ("keys", self.keys),
        ];
        let slowest = stages
            .iter()
            .max_by_key(|(_, ms)| *ms)
            .map_or("none", |(name, _)| name);
        log.line(
            "[LOOP] ",
            format_args!(
                "turn_ms={total} stage={slowest} host={} node={} take={} draw={} keys={} entries={entries}",
                self.host, self.node, self.take, self.draw, self.keys
            ),
        );
    }
}

/// The ST7789 opcodes this image sends (Sitronix ST7789V datasheet, 9.1).
const SWRESET: u8 = 0x01;
const SLPOUT: u8 = 0x11;
const NORON: u8 = 0x13;
const INVON: u8 = 0x21;
const DISPON: u8 = 0x29;
const CASET: u8 = 0x2A;
const RASET: u8 = 0x2B;
const RAMWR: u8 = 0x2C;
const MADCTL: u8 = 0x36;
/// Tearing effect line on (9.1.37); its one byte 0: the V-blank only.
const TEON: u8 = 0x35;
const COLMOD: u8 = 0x3A;
const COLMOD_RGB565: u8 = 0x55;

/// The most rows one DMA transfer out of the frame carries: esp-hal's
/// 32,736 bytes a transfer, in whole rows of 640 (1507).
const ROWS_PER_DMA: u32 = 32_736 / (WIDTH * 2);
/// Their bytes.
const DMA_BYTES: usize = (ROWS_PER_DMA * WIDTH * 2) as usize;
/// A descriptor's bytes over PSRAM: 4,095 less the 64-byte alignment.
const DMA_CHUNK: usize = 4032;

/// Why a write did not reach the panel.
#[derive(Debug)]
enum PanelError {
    Spi(esp_hal::spi::Error),
    Dma(esp_hal::dma::DmaBufError),
}

impl core::fmt::Display for PanelError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PanelError::Spi(e) => write!(f, "spi:{e:?}"),
            PanelError::Dma(e) => write!(f, "dma:{e:?}"),
        }
    }
}

impl From<esp_hal::spi::Error> for PanelError {
    fn from(e: esp_hal::spi::Error) -> Self {
        PanelError::Spi(e)
    }
}

/// A flush on its way (1507): the runs of rows still to send, the next row
/// of the one whose window is open, and the frame they come out of.
#[derive(Default)]
struct Flush {
    runs: VecDeque<(u32, u32)>,
    /// The open window's next row; `None` once its last chunk went out.
    at: Option<u32>,
    /// A window is open: chip select low, RAMWR sent.
    open: bool,
    /// The frame's first word, the DMA's source.
    base: usize,
    /// What the `[DISP] flush` line says once all of it is out: the rows
    /// from the first run's first to the last run's last, the runs, the
    /// bytes, and when it began, microseconds.
    report: Option<Sent>,
}

/// A flush, for its `[DISP] flush` line.
#[derive(Clone, Copy)]
struct Sent {
    first: u32,
    last: u32,
    runs: usize,
    bytes: usize,
    began_us: u64,
    /// Microseconds from begin to the last byte on the panel; 0 until then.
    us: u64,
}

/// The panel on SPI2 with DMA, its chip select and data/command line,
/// and its backlight, on when low, lit only over a written panel (1485).
///
/// Since 1507 a flush sends each run of changed rows in its own window,
/// straight out of the frame by DMA (the frame keeps the panel's byte
/// order on the DMA's boundary, `loquax_m9::frame`), and does not wait:
/// [`Panel::begin`] starts it, [`Panel::pump`] moves it on between the
/// turn's other work, [`Panel::settle`] waits it out before the frame is
/// drawn into again.
struct Panel<'d> {
    /// The bus, `None` while a transfer owns it.
    spi: Option<SpiDma<'d, Blocking>>,
    inflight: Option<SpiDmaTransfer<'d, Blocking, DmaTxBuf>>,
    /// The transfer's descriptors, `None` while one runs.
    descriptors: Option<DmaAlignedMut<'static, [DmaDescriptor]>>,
    flush: Flush,
    /// The last flush that reached the panel, until the loop says it.
    done: Option<Sent>,
    /// The frame's memory refused a DMA ([`PanelError::Dma`]): the rows
    /// go through the driver's bounce buffer, as before 1507.
    bounce: bool,
    cs: Output<'d>,
    dc: Output<'d>,
    backlight: Output<'d>,
    lit: bool,
}

/// The boot order of `loquax_m9::splash`: the board's half.
impl splash::Panel for Panel<'_> {
    type Error = PanelError;

    fn write(&mut self, frame: &Frame, first: u32, last: u32) -> Result<(), Self::Error> {
        self.write_rows(frame, first, last)
    }

    fn light(&mut self, on: bool) {
        match on {
            true => self.backlight.set_low(),
            false => self.backlight.set_high(),
        }
        self.lit = on;
    }
}

impl Panel<'_> {
    fn cmd(&mut self, op: u8, data: &[u8]) -> Result<(), PanelError> {
        self.cs.set_low();
        let r = self.send(op, data);
        self.cs.set_high();
        r
    }

    fn send(&mut self, op: u8, data: &[u8]) -> Result<(), PanelError> {
        let Panel { spi, dc, .. } = self;
        let Some(spi) = spi.as_mut() else {
            return Err(PanelError::Spi(esp_hal::spi::Error::Unsupported));
        };
        dc.set_low();
        spi.write(&[op])?;
        dc.set_high();
        if !data.is_empty() {
            spi.write(data)?;
        }
        Ok(())
    }

    /// The datasheet's power-on sequence with its delays, as the map
    /// firmware ran it on this unit, landscape.
    fn init(&mut self, rst: &mut Output<'_>, delay: &Delay) -> Result<(), PanelError> {
        rst.set_low();
        delay.delay_millis(1);
        rst.set_high();
        delay.delay_millis(120);
        self.cmd(SWRESET, &[])?;
        delay.delay_millis(150);
        self.cmd(SLPOUT, &[])?;
        delay.delay_millis(120);
        self.cmd(COLMOD, &[COLMOD_RGB565])?;
        self.cmd(MADCTL, &[m9::LCD_MADCTL])?;
        self.cmd(INVON, &[])?;
        self.cmd(NORON, &[])?;
        // The blank on TE (1507): the draws are paced by it.
        self.cmd(TEON, &[0x00])?;
        delay.delay_millis(10);
        self.cmd(DISPON, &[])
    }

    /// Rows `first..=last` of `frame`, written before it returns: the
    /// splash and the fault screen, which nothing overlaps.
    fn write_rows(&mut self, frame: &Frame, first: u32, last: u32) -> Result<(), PanelError> {
        self.settle()?;
        self.begin(frame, [(first, last)].into_iter().collect())?;
        let r = self.settle();
        self.done = None;
        r
    }

    /// The flush that reached the panel since the last call, once.
    fn take_done(&mut self) -> Option<Sent> {
        self.done.take()
    }

    /// Whether a flush is still on its way.
    fn busy(&self) -> bool {
        self.inflight.is_some() || !self.flush.runs.is_empty() || self.flush.open
    }

    /// Start sending `runs` of `frame`, each in its own window. The frame
    /// must not be drawn into until [`Panel::settle`] (or a [`Panel::pump`]
    /// that said done): the DMA reads it as it goes.
    fn begin(&mut self, frame: &Frame, runs: VecDeque<(u32, u32)>) -> Result<(), PanelError> {
        self.settle()?;
        let report = match (runs.front(), runs.back()) {
            (Some(&(first, _)), Some(&(_, last))) => Some(Sent {
                first,
                last,
                runs: runs.len(),
                bytes: runs
                    .iter()
                    .map(|(a, b)| ((b + 1 - a) * WIDTH * 2) as usize)
                    .sum(),
                began_us: leviculum_esp::uptime_us(),
                us: 0,
            }),
            _ => None,
        };
        self.flush = Flush {
            runs,
            at: None,
            open: false,
            base: frame.pixels().as_ptr() as usize,
            report,
        };
        self.pump().map(|_| ())
    }

    /// Wait for the flush on its way to reach the panel.
    fn settle(&mut self) -> Result<(), PanelError> {
        while !self.pump()? {
            core::hint::spin_loop();
        }
        Ok(())
    }

    /// Move the flush on as far as it goes without waiting: the transfer
    /// that ended is taken back, the next chunk or window started. `true`
    /// once all of it reached the panel. An error leaves no window open
    /// and nothing owed; the caller owes the frame whole.
    fn pump(&mut self) -> Result<bool, PanelError> {
        let r = self.pump_inner();
        match r {
            Err(_) => {
                self.flush = Flush::default();
                self.cs.set_high();
            }
            Ok(true) => {
                if let Some(mut sent) = self.flush.report.take() {
                    sent.us = leviculum_esp::uptime_us().saturating_sub(sent.began_us);
                    self.done = Some(sent);
                }
            }
            Ok(false) => {}
        }
        r
    }

    fn pump_inner(&mut self) -> Result<bool, PanelError> {
        loop {
            if let Some(transfer) = self.inflight.take() {
                if !transfer.is_done() {
                    self.inflight = Some(transfer);
                    return Ok(false);
                }
                let (spi, buf) = transfer.wait();
                leviculum_esp::mspi::dma_done();
                self.spi = Some(spi);
                self.descriptors = Some(buf.split().0);
            }
            if self.flush.open && self.flush.at.is_none() {
                self.cs.set_high();
                self.flush.open = false;
                self.flush.runs.pop_front();
            }
            let Some(&(first, last)) = self.flush.runs.front() else {
                return Ok(true);
            };
            if !self.flush.open {
                self.window(first, last)?;
                self.flush.at = Some(first);
            }
            let Some(row) = self.flush.at else {
                // An open window always has a next row here: the close
                // above took the one whose last chunk went.
                return Ok(true);
            };
            let end = (row + ROWS_PER_DMA - 1).min(last);
            let bytes = ((end + 1 - row) * WIDTH * 2) as usize;
            let at = self.flush.base + (row * WIDTH * 2) as usize;
            // SAFETY: rows `row..=end` of the frame `begin` was given,
            // which lives as long as the UI core and is not drawn into
            // until the flush settled; the DMA only reads them.
            let rows: &'static mut [u8] =
                unsafe { core::slice::from_raw_parts_mut(at as *mut u8, bytes) };
            if self.bounce {
                self.bounced(rows)?;
            } else if !self.dma(rows)? {
                return Ok(false);
            }
            self.flush.at = (end < last).then_some(end + 1);
        }
    }

    /// Open the window of rows `first..=last`, full width, and say RAMWR:
    /// the ST7789's row window, so a moved cursor sends its rows and not
    /// the screen.
    fn window(&mut self, first: u32, last: u32) -> Result<(), PanelError> {
        let x1 = (WIDTH - 1) as u16;
        let (y0, y1) = (first as u16, last as u16);
        self.cs.set_low();
        self.flush.open = true;
        self.send(CASET, &[0, 0, (x1 >> 8) as u8, x1 as u8])?;
        self.send(
            RASET,
            &[(y0 >> 8) as u8, y0 as u8, (y1 >> 8) as u8, y1 as u8],
        )?;
        self.send(RAMWR, &[])
    }

    /// Start a DMA of `rows` straight out of PSRAM. `false` when the flash
    /// is busy (`leviculum_esp::mspi`): the chunk goes on a later pump.
    fn dma(&mut self, rows: &'static mut [u8]) -> Result<bool, PanelError> {
        let len = rows.len();
        let buffer = match DmaAlignedMut::new(rows) {
            Ok(buffer) => buffer,
            Err(e) => {
                // The frame's memory is no DMA's: through the bounce
                // buffer from here on, said once by the caller's error.
                self.bounce = true;
                return Err(PanelError::Dma(e));
            }
        };
        let (Some(descriptors), Some(spi)) = (self.descriptors.take(), self.spi.take()) else {
            return Err(PanelError::Spi(esp_hal::spi::Error::Unsupported));
        };
        let buf = match DmaTxBuf::new(descriptors, buffer) {
            Ok(buf) => buf,
            Err(e) => {
                self.spi = Some(spi);
                self.bounce = true;
                return Err(PanelError::Dma(e));
            }
        };
        if !leviculum_esp::mspi::dma_begin(leviculum_esp::mspi::transfer_us(len, m9::LCD_SPI_MHZ)) {
            self.spi = Some(spi);
            self.descriptors = Some(buf.split().0);
            return Ok(false);
        }
        self.dc.set_high();
        match spi.write_buffer(len, buf) {
            Ok(transfer) => {
                self.inflight = Some(transfer);
                Ok(true)
            }
            Err((e, spi, buf)) => {
                leviculum_esp::mspi::dma_done();
                self.spi = Some(spi);
                self.descriptors = Some(buf.split().0);
                Err(PanelError::Spi(e))
            }
        }
    }

    /// `rows` through the driver's own buffer in internal RAM, a chunk at
    /// a time and waiting: the way before 1507, for a frame no DMA reads.
    fn bounced(&mut self, rows: &[u8]) -> Result<(), PanelError> {
        let Some(spi) = self.spi.as_mut() else {
            return Err(PanelError::Spi(esp_hal::spi::Error::Unsupported));
        };
        self.dc.set_high();
        for part in rows.chunks(4096) {
            spi.write(part)?;
        }
        Ok(())
    }
}

// The stack's bounds, from esp-hal's linker script (`ld/sections/stack.x`):
// it grows down from `_stack_start_cpu0` towards `_stack_end_cpu0`.
extern "C" {
    static _stack_start_cpu0: u8;
    static _stack_end_cpu0: u8;
}

/// The stack's low and high end: linker symbols, only their addresses
/// taken.
fn stack_bounds() -> (usize, usize) {
    (
        core::ptr::addr_of!(_stack_end_cpu0) as usize,
        core::ptr::addr_of!(_stack_start_cpu0) as usize,
    )
}

/// The paint the census reads its stack off.
const PAINT: u8 = 0xA5;
/// Bytes above the stack's low end left alone: the stack-protector guard
/// word and esp-hal's guard watchpoint sit there.
const GUARD_SKIP: usize = 512;
/// Bytes below the probe's own frame left alone.
const FRAME_SKIP: usize = 512;

/// The board's half of the census: the counted allocator and the painted
/// stack, from just below the probe's frame down to the guard. `floor`
/// is the measuring core's stack low end: core 0's linker symbol, or
/// the UI core's static stack (1493).
struct BoardProbe {
    before: usize,
    low: usize,
    high: usize,
    floor: usize,
}

impl BoardProbe {
    fn over(floor: usize) -> Self {
        Self {
            before: 0,
            low: 0,
            high: 0,
            floor,
        }
    }
}

impl Probe for BoardProbe {
    #[inline(never)]
    fn begin(&mut self) {
        leviculum_esp::heap_reset_peak();
        self.before = leviculum_esp::heap_live();
        let here = 0u8;
        let sp = core::ptr::addr_of!(here) as usize;
        self.low = self.floor + GUARD_SKIP;
        self.high = sp.saturating_sub(FRAME_SKIP).max(self.low);
        for at in self.low..self.high {
            // SAFETY: the unused stack below the live frames, which this
            // core owns and nothing else writes.
            unsafe { core::ptr::write_volatile(at as *mut u8, PAINT) };
        }
    }

    #[inline(never)]
    fn end(&mut self) -> Step {
        // SAFETY: as in `begin`; read only.
        let touched = (self.low..self.high)
            .find(|at| unsafe { core::ptr::read_volatile(*at as *const u8) } != PAINT)
            .unwrap_or(self.high);
        let live = leviculum_esp::heap_live();
        Step {
            held: live as isize - self.before as isize,
            peak: leviculum_esp::heap_peak().saturating_sub(self.before),
            stack: self.high - touched,
        }
    }
}

/// Core 0's stack high-water mark (1521b): painted once below the
/// caller's frame, read on every `[UART]` line as the bytes above the
/// guard nothing has written since.
struct StackWatch {
    low: usize,
    high: usize,
}

impl StackWatch {
    #[inline(never)]
    fn paint() -> Self {
        let here = 0u8;
        let sp = core::ptr::addr_of!(here) as usize;
        let low = stack_bounds().0 + GUARD_SKIP;
        let high = sp.saturating_sub(FRAME_SKIP).max(low);
        for at in low..high {
            // SAFETY: core 0's unused stack below the live frames, which
            // this core owns; an interrupt that lands here writes over
            // paint, which is what it is there to show.
            unsafe { core::ptr::write_volatile(at as *mut u8, PAINT) };
        }
        Self { low, high }
    }

    /// The bytes above the guard never written since [`StackWatch::paint`].
    fn free(&self) -> usize {
        // SAFETY: as in `paint`; read only.
        let touched = (self.low..self.high)
            .find(|at| unsafe { core::ptr::read_volatile(*at as *const u8) } != PAINT)
            .unwrap_or(self.high);
        touched - self.low
    }
}

fn halt_with_banner(log: &mut UartLog<'_>, heap: usize) -> ! {
    let delay = Delay::new();
    loop {
        delay.delay_millis(BANNER_INTERVAL_MS as u32);
        log.identify(m9::LOG_PREFIX, m9::BOARD_NAME, heap);
    }
}

/// What the screen shows besides the model: the pairing while a code is
/// typed, the home's word in the top bar (1415), and a fault that is a
/// state (1493).
#[derive(Default)]
struct Shown {
    pairing: Option<Pairing>,
    word: Option<Word>,
    fault: Option<StrId>,
    /// The map's key hint and corner credit (1531), as the loop's
    /// [`map::Chrome`] says them.
    map: map::Overlay,
    /// The house key's overview and the reader's type size (1532).
    overview: Overview,
}

/// Draw what `model` shows, the M9's cursor over a group's rows on `row`,
/// the map in pixels when it is open, or the pairing over all of it, and
/// start the rows that changed on their way to the panel. On the UI core
/// (1493); the lines go through the queue. A `[PAGE]` line follows when the
/// page or its cursor moved (1533). Whether a flush began (1507): its
/// `[DISP] flush` line comes when it is out ([`flushed`]).
#[allow(clippy::too_many_arguments)]
fn redraw(
    renderer: &mut Renderer<'_, Frame>,
    map: &mut Map<'_>,
    panel: &mut Panel<'_>,
    model: &UiModel<Field>,
    row: usize,
    shown: &Shown,
    said: &mut page::Said,
    log: &mut UiLog,
) -> bool {
    // The last flush reads the frame until it is out (1507).
    let _ = flushed(panel, renderer.panel(), true, log);
    let look = screen::Look {
        row,
        home: shown.word,
        fault: shown.fault,
        note: shown.overview.note(),
        map: shown.map,
        ..screen::Look::of(model)
    };
    let overview = shown.overview;
    #[cfg(not(m9_bench))]
    let drawn = match &shown.pairing {
        Some(pairing) => pair::draw(renderer, pairing, shown.word, &look),
        None if overview.open => renderer.text(|cols, rows| overview.lines(&look, cols, rows)),
        None => renderer.show(model, &look, map),
    };
    // The bench times the scene and the draw apart (1540).
    #[cfg(m9_bench)]
    let drawn = match &shown.pairing {
        Some(pairing) => pair::draw(renderer, pairing, shown.word, &look),
        None if overview.open => renderer.text(|cols, rows| overview.lines(&look, cols, rows)),
        None => bench::show(renderer, model, &look, map),
    };
    if drawn.is_err() {
        log.line("[DISP] ", alloc::string::String::from("draw=failed"));
        return false;
    }
    // What the map's draw did (1531): its zoom, the picture's and the
    // label pass's time, the names set and dropped.
    if let Some(stats) = renderer.take_map_stats() {
        let ms = |us: u64| alloc::format!("{}.{:03}", us / 1000, us % 1000);
        log.line(
            "[MAP] ",
            alloc::format!(
                "zoom={} picture_ms={} names_ms={} names={} dropped={} kept={}",
                stats.zoom,
                ms(stats.picture_us),
                ms(stats.names_us),
                stats.names.drawn,
                stats.names.dropped(),
                u8::from(stats.kept)
            ),
        );
    }
    let at = match shown.pairing.is_none() && overview.open {
        true => page::house(),
        false => page::of(model, row, shown.pairing.is_some(), &renderer.window()),
    };
    let moved = said.say(at);
    // The changed rows as runs, a window each, sent by DMA while the loop
    // goes on (1507); the `[DISP] flush` line follows when the last byte
    // is out ([`flushed`]).
    let frame = renderer.panel();
    let runs = frame.take_runs();
    let flushing = !runs.is_empty();
    if flushing {
        if let Err(e) = panel.begin(frame, runs.into_iter().collect()) {
            frame.forget_flushed();
            log.line("[DISP] ", alloc::format!("flush=failed err={e}"));
        }
    }
    if let Some(body) = moved {
        log.line("[PAGE] ", body);
    }
    flushing
}

/// Move the panel's flush on, or wait it out when `wait`, and say the one
/// that reached the panel: its `[DISP] flush` line (rows from the first
/// run's first to the last's last, the time from begin to the last byte),
/// and the backlight after the first frame that landed (1485). Its
/// microseconds, when one landed. A flush that failed owes the frame
/// whole: the panel's bytes are unknown now (1493).
fn flushed(panel: &mut Panel<'_>, frame: &Frame, wait: bool, log: &mut UiLog) -> Option<u64> {
    let moved = match wait {
        true => panel.settle(),
        false => panel.pump().map(|_| ()),
    };
    if let Err(e) = moved {
        frame.forget_flushed();
        log.line("[DISP] ", alloc::format!("flush=failed err={e}"));
        return None;
    }
    let sent = panel.take_done()?;
    log.line(
        "[DISP] ",
        alloc::format!(
            "flush rows={}..{} ms={}.{:03} runs={} bytes={}",
            sent.first,
            sent.last,
            sent.us / 1000,
            sent.us % 1000,
            sent.runs,
            sent.bytes
        ),
    );
    if !panel.lit {
        panel.light(true);
        log.line("[DISP] ", alloc::string::String::from("backlight=on"));
    }
    Some(sent.us)
}

#[main]
fn main() -> ! {
    let p = leviculum_esp::init_soc();

    // The log first: every later failure says itself here.
    let tx: m9::UartTx = p.GPIO43;
    let rx: m9::UartRx = p.GPIO44;
    let Ok(uart) = Uart::new(p.UART0, leviculum_esp::uart_log_config(m9::UART_BAUD)) else {
        loop {
            core::hint::spin_loop();
        }
    };
    let mut log = UartLog::new(uart.with_tx(tx).with_rx(rx));
    leviculum_esp::say_panics_on_uart0(m9::UART_BAUD);
    // Where the last run stopped (1534), read before this run's record
    // starts over it.
    let prev = stall::previous();
    stall::begin();
    log.line("[PREV_RUN] ", format_args!("{prev}"));

    // The heap in PSRAM, before anything allocates.
    let psram = Psram::new(
        p.PSRAM,
        PsramConfig {
            mode: PsramMode::OctalSpi,
            size: PsramSize::Size(m9::PSRAM_BYTES),
            ram_frequency: SpiRamFreq::Freq80m,
            ..Default::default()
        },
    );
    let (start, bytes) = psram.raw_parts();
    log.identify(m9::LOG_PREFIX, m9::BOARD_NAME, bytes);
    if bytes < HEAP_MIN {
        log.line(
            "[HEAP] ",
            format_args!("region=psram bytes={bytes} state=down reason=psram-absent"),
        );
        halt_with_banner(&mut log, bytes);
    }
    // SAFETY: once, before any allocation; the driver mapped `bytes` at
    // `start` and nothing else in this image touches PSRAM.
    unsafe { leviculum_esp::init_heap_at(start as usize, bytes) };
    log.line(
        "[HEAP] ",
        format_args!("region=psram start={start:p} bytes={bytes}"),
    );
    // The line's send ring on the heap, now that there is one (1507).
    log.start_ring();
    let (stack_end, stack_start) = stack_bounds();
    log.line(
        "[STACK] ",
        format_args!("bytes={}", stack_start - stack_end),
    );

    // The stores, the identity and the node (1414), before the panel: the
    // flash routines run with the cache off, and nothing else is drawing
    // through it yet.
    let mut stores = Stores::open(p.FLASH);
    log.line(
        "[FLASH] ",
        format_args!(
            "identity={} client={} messages={}",
            stores.identity_slot(),
            stores.client_slot(),
            stores.messages_slot()
        ),
    );
    // The true random source: the SoC's RNG with the ADC's noise. Kept for
    // the life of the image; `Trng` refuses to exist without it.
    let _entropy = TrngSource::new(p.RNG, p.ADC1);
    let (Ok(mut rng), Ok(node_rng)) = (Trng::try_new(), Trng::try_new()) else {
        log.line("[NODE] ", format_args!("state=down reason=no-entropy"));
        halt_with_banner(&mut log, bytes);
    };
    // A flash read error keeps the board without an identity (1493):
    // no node runs, and once the panel is up the screen says why.
    let identity = handheld::identity(&mut stores, &mut rng, &mut log);
    let mut node = match identity {
        Some(identity) => {
            let before = leviculum_esp::heap_live();
            let node = Handheld::new(identity, node_rng);
            log.line(
                "[CENSUS] ",
                format_args!(
                    "node held={} box={} links={} max_answer={}",
                    leviculum_esp::heap_live() - before,
                    core::mem::size_of::<handheld::Core>(),
                    handheld::MAX_LINKS,
                    handheld::MAX_ANSWER
                ),
            );
            log.line(
                "[NODE] ",
                format_args!(
                    "state=up iface=serial hw_mtu={} serial_mux=1",
                    leviculum_esp::serial_iface::SERIAL_HW_MTU
                ),
            );
            Some(node)
        }
        None => {
            log.line("[NODE] ", format_args!("state=down reason=identity-unread"));
            None
        }
    };

    // The bus supply on, and every chip select on SPI2 high before the bus
    // exists, so the radio and the card ignore the panel's traffic.
    let vext: m9::VextEnable = p.GPIO18;
    let lora_nss: m9::LoRaNss = p.GPIO39;
    let sd_cs: m9::SdCs = p.GPIO48;
    let lcd_cs: m9::LcdCs = p.GPIO16;
    let lcd_dc: m9::LcdDc = p.GPIO15;
    let lcd_reset: m9::LcdReset = p.GPIO14;
    let lcd_backlight: m9::LcdBacklight = p.GPIO17;
    let lcd_te: m9::LcdTe = p.GPIO19;
    let io = Io::new(p.IO_MUX);
    let _vext = Output::new(vext, Level::Low, OutputConfig::default());
    let _lora_nss = Output::new(lora_nss, Level::High, OutputConfig::default());
    let _sd_cs = Output::new(sd_cs, Level::High, OutputConfig::default());
    let cs = Output::new(lcd_cs, Level::High, OutputConfig::default());
    let dc = Output::new(lcd_dc, Level::High, OutputConfig::default());
    let mut rst = Output::new(lcd_reset, Level::High, OutputConfig::default());
    // Off until the splash is on the panel (1485): the ST7789's memory is
    // noise after its reset.
    let backlight = Output::new(lcd_backlight, Level::High, OutputConfig::default());
    let delay = Delay::new();

    let (Ok(dma_rx), Ok(dma_tx)) = (dma_rx_buffer!(4096), dma_tx_buffer!(4096)) else {
        log.line("[DISP] ", format_args!("state=down reason=dma-buffers"));
        halt_with_banner(&mut log, bytes);
    };
    let spi_config = SpiConfig::default()
        .with_frequency(Rate::from_mhz(m9::LCD_SPI_MHZ))
        .with_mode(Mode::_0);
    let Ok(spi) = Spi::new(p.SPI2, spi_config) else {
        log.line("[DISP] ", format_args!("state=down reason=spi-config"));
        halt_with_banner(&mut log, bytes);
    };
    let sck: m9::SpiSck = p.GPIO40;
    let mosi: m9::SpiMosi = p.GPIO47;
    let miso: m9::SpiMiso = p.GPIO38;
    let spi = spi
        .with_sck(sck)
        .with_mosi(mosi)
        .with_miso(miso)
        .with_dma(p.DMA_CH0)
        .with_buffers(dma_rx, dma_tx);
    // The descriptors a flush's DMA runs on (1507): one transfer's rows
    // of the frame, in PSRAM-sized chunks.
    let (_, descriptors) = dma_descriptors_chunk_size!(0, DMA_BYTES, DMA_CHUNK);
    let descriptors = match DmaAlignedMut::new(descriptors) {
        Ok(descriptors) => Some(descriptors),
        Err(e) => {
            log.line("[DISP] ", format_args!("dma=bounce reason={e:?}"));
            None
        }
    };
    let mut panel = Panel {
        spi: Some(spi),
        inflight: None,
        bounce: descriptors.is_none(),
        descriptors,
        flush: Flush::default(),
        done: None,
        cs,
        dc,
        backlight,
        lit: false,
    };
    match panel.init(&mut rst, &delay) {
        Ok(()) => log.line(
            "[DISP] ",
            format_args!(
                "ST7789 {WIDTH}x{HEIGHT} madctl={:#04x} spi_mhz={} state=up",
                m9::LCD_MADCTL,
                m9::LCD_SPI_MHZ
            ),
        ),
        Err(e) => log.line("[DISP] ", format_args!("state=down reason=init err={e}")),
    }
    // The splash written whole, then the backlight (1485). It stays on the
    // panel through the census and the store's restore: nothing flushes
    // until the UI's first redraw.
    let mut frame = Frame::new();
    let t0 = esp_hal::time::Instant::now();
    let splashed = splash::power_on(&mut panel, &mut frame);
    let us = t0.elapsed().as_micros();
    match splashed {
        Ok(()) => log.line(
            "[DISP] ",
            format_args!(
                "splash rows=0..{} ms={}.{:03} backlight=on",
                HEIGHT - 1,
                us / 1000,
                us % 1000
            ),
        ),
        Err(e) => log.line(
            "[DISP] ",
            format_args!("splash=failed err={e} backlight=off"),
        ),
    }

    // A board whose identity would not read (1493): the fault on the
    // panel in the reader's words, the banner on the line, no node.
    let Some(mut node) = node.take() else {
        if let Ok(mut renderer) = Renderer::new(&mut frame) {
            let words =
                Text::Key(StrId::M9NoIdentity, alloc::vec::Vec::new()).resolve(loquax_m9::LANG);
            let _ = renderer.text(|cols, rows| fault_lines(&words, cols, rows));
            let shown = renderer.panel();
            if let Some((first, last)) = shown.take_dirty() {
                let _ = panel.write_rows(shown, first, last);
            }
        }
        halt_with_banner(&mut log, bytes);
    };
    drop(frame);

    // The keyboard controller, probed at both revisions' addresses; the
    // probe's answer is the address polled (1493): a V1.0 board answers
    // at 0x6C and its keyboard was dead under the fixed 0x6D.
    let sda: m9::KbSda = p.GPIO20;
    let scl: m9::KbScl = p.GPIO21;
    let i2c_config = I2cConfig::default().with_frequency(Rate::from_khz(m9::KB_I2C_KHZ));
    let mut i2c = match I2c::new(p.I2C0, i2c_config) {
        Ok(i2c) => Some(i2c.with_sda(sda).with_scl(scl)),
        Err(_) => {
            log.line("[KB] ", format_args!("state=down reason=i2c-config"));
            None
        }
    };
    let mut kb = kbraw::MEASURED;
    if let Some(bus) = i2c.as_mut() {
        let mut probe = [0u8; 1];
        let v11 = bus.read(m9::KB_ADDR_V11, &mut probe).is_ok();
        let v10 = bus.read(m9::KB_ADDR_V10, &mut probe).is_ok();
        kb = kbraw::probed(v11, v10);
        log.line(
            "[KB] ",
            format_args!(
                "addr={:#04x} found={} v10_addr={:#04x} v10_found={} protocol=codes reg={:#04x} len={} poll_ms={POLL_MS}",
                kb.address,
                if v11 { "yes" } else { "no" },
                m9::KB_ADDR_V10,
                if v10 { "yes" } else { "no" },
                kb.register.unwrap_or(0),
                kb.len,
            ),
        );
    }

    // The keyboard's poll from here on is the timer's (1463); the same
    // group's watchdog is core 0's loop's (1534), TG1's the UI core's.
    let group0 = TimerGroup::new(p.TIMG0);
    let wdt0 = group0.wdt;
    let group1 = TimerGroup::new(p.TIMG1);
    let wdt1 = group1.wdt;
    // The UI core's tick (1507), started over there.
    let ui_timer = group1.timer0;
    if let Some(bus) = i2c.take() {
        let mut timer = PeriodicTimer::new(group0.timer0);
        timer.set_interrupt_handler(kb_tick);
        match timer.start(Duration::from_millis(u64::from(POLL_MS))) {
            Ok(()) => {
                critical_section::with(|cs| {
                    timer.listen();
                    KB_POLL.borrow_ref_mut(cs).replace(KbPoll {
                        bus,
                        timer,
                        protocol: kb,
                    });
                });
                KB_TICKING.store(true, Ordering::Release);
                log.line(
                    "[KB] ",
                    format_args!("poll=timer every_ms={POLL_MS} queue={}", kbraw::QUEUE),
                );
            }
            Err(e) => log.line("[KB] ", format_args!("state=down reason=timer err={e:?}")),
        }
    }

    // The client session's pull (1363's census, measured here since 1414):
    // thirty 200-byte messages in LoRa pages, and the batch the board runs
    // with on what it measured. On this core's stack: the pull runs here.
    let mut probe = BoardProbe::over(stack_bounds().0);
    let archive = census::archive(30, 200);
    let answers = census::lora_answers(&archive);
    let home_key = node.node.identity().public_key_bytes();
    let batch = match census::session(&mut probe, &home_key, &answers) {
        Ok(got) => {
            log.line(
                "[CENSUS] ",
                format_args!(
                    "{} pages={} entries={}",
                    census::SessionLine(&got),
                    answers.len(),
                    archive.len()
                ),
            );
            census::batch(&got, handheld::MAX_ANSWER)
        }
        Err(error) => {
            log.line("[CENSUS] ", format_args!("session=pull failed={error}"));
            census::batch(&Step::default(), handheld::MAX_ANSWER)
        }
    };
    drop(answers);
    drop(archive);
    // From here on core 0's deepest frame is measured (1521b): the
    // restore, the loop, the flash, after the session census above
    // painted and read its own.
    let watch = StackWatch::paint();
    // A device of a home draws its home's chats: what its message store
    // kept (1476), then every pull; a board that is none opens on the
    // pairing, and Back from there shows the fixture as before (1415).
    let restored = match stores.load_client() {
        // No watchdog is armed yet: nothing to feed.
        Ok(Some(record)) => node.client(record, batch, &mut stores, &mut log, &|| {}),
        Ok(None) => {
            log.line(
                "[HOME] ",
                format_args!("client=none page={}", batch.page_bytes),
            );
            None
        }
        Err(error) => {
            log.line("[HOME] ", format_args!("client=unread error={error}"));
            None
        }
    };
    let model = Box::new(restored.unwrap_or_else(fixture::model));
    let shown = Shown {
        pairing: node.address().is_none().then(Pairing::new),
        word: node.word(),
        fault: None,
        map: map::Overlay::default(),
        // The record's type (1532); a board without one shows the default.
        overview: Overview::new(node.large_type().map_or(Size::DEFAULT, Size::of_large)),
    };

    // The seed the UI core draws client ids from (1493): entropy taken
    // here, hashed with a counter over there. An id must never repeat
    // across reboots — the home reads a repeated id as the same message.
    let mut seed = [0u8; 32];
    rand_core::RngCore::fill_bytes(&mut rng, &mut seed);

    // The UI core (1493): model, scene, renderer, panel. Everything it
    // says goes through the queue to this core's UART writer. The guard
    // must live as long as the image: dropping it parks the core.
    let mut cpu = CpuControl::new(p.CPU_CTRL);
    // SAFETY: the one place this static is taken, before the core starts.
    let ui_stack = unsafe { &mut *addr_of_mut!(APP_CORE_STACK) };
    let started = cpu.start_app_core(ui_stack, move || {
        ui_main(
            panel,
            model,
            shown,
            kb,
            seed,
            wdt1,
            UiWakes {
                io,
                te: lcd_te,
                tick: ui_timer,
            },
        )
    });
    match &started {
        Ok(_) => log.line(
            "[CORES] ",
            format_args!("ui=core1 stack={UI_STACK} io=core0"),
        ),
        Err(e) => log.line("[CORES] ", format_args!("ui=down reason={e:?}")),
    }
    if started.is_err() {
        halt_with_banner(&mut log, bytes);
    }
    // The UI core's census runs alone (1521b): what it counts and how
    // long it takes are its own.
    let waited = uptime_ms();
    while CENSUS_RUNNING.load(Ordering::Acquire)
        && uptime_ms().saturating_sub(waited) < CENSUS_WAIT_MS
    {
        core::hint::spin_loop();
    }
    log.line(
        "[CORES] ",
        format_args!(
            "census={} waited_ms={}",
            if CENSUS_RUNNING.load(Ordering::Acquire) {
                "late"
            } else {
                "done"
            },
            uptime_ms().saturating_sub(waited)
        ),
    );

    // The I/O loop on core 0: the line, the node, the client, the flash.
    let mut deframer = Deframer::new();
    // The keyboard's losses as the last `[KB]` line said them.
    let mut kb_said = (0, 0);
    // The faults the screen shows (1493): sticky until a boot proves the
    // flash again; the word is the state, the log has the detail.
    let mut faulted = false;
    let mut next_banner = uptime_ms() + BANNER_INTERVAL_MS;
    // Where the `[IDLE]` line's window began (1507).
    let mut idle_since = uptime_ms();
    let _ = leviculum_esp::idle::waited_us();
    // The bench's keys (1533), only in an image built for it.
    #[cfg(m9_bench)]
    let mut inject = loquax_m9::kbraw::Inject::new();
    // The bench's arrivals so far (1540): each says its number.
    #[cfg(m9_bench)]
    let mut arrivals: u32 = 0;
    #[cfg(m9_bench)]
    log.line("[BENCH] ", format_args!("inject=on"));
    // From here core 0's loop feeds its watchdog every turn, and between
    // the messages of a long one (1534): a frame dump's lines took 4.6 s
    // of one turn on the bench (2026-11-07f), all of it the UART's.
    let wdt: Watchdog<TIMG0<'static>> = Watchdog::arm(wdt0);
    // After the first turn the loop never waits for the line (1507): a
    // full send ring drops its oldest record, counted on the `[UART]`
    // line. The first turn writes the UI core's census lines, all of them.
    let mut first_turn = true;
    log.line(
        "[WATCHDOG] ",
        format_args!(
            "core0=mwdt0 core1=mwdt1 timeout_ms={} record=rtc-slow",
            stall::TIMEOUT_MS
        ),
    );
    loop {
        wdt.feed();
        stall::turned(Core::Io);
        stall::stage(Core::Io, Stage::Host);
        let now = uptime_ms();
        let mut turn = Turn::default();
        if now >= next_banner {
            next_banner = now + BANNER_INTERVAL_MS;
            if now < BANNER_WINDOW_MS {
                log.identify(m9::LOG_PREFIX, m9::BOARD_NAME, bytes);
                #[cfg(m9_bench)]
                log.line("[BENCH] ", format_args!("inject=on"));
            }
            log.set_stack_free(watch.free());
            log.say_rx_counts(true);
            // How much of the window each core waited (1507).
            let [io, ui] = leviculum_esp::idle::waited_us();
            let window_us = (now.saturating_sub(idle_since) * 1000).max(1);
            idle_since = now;
            log.line(
                "[IDLE] ",
                format_args!(
                    "io_pct={} ui_pct={} window_ms={}",
                    u64::from(io) * 100 / window_us,
                    u64::from(ui) * 100 / window_us,
                    window_us / 1000
                ),
            );
        }

        // The host: control frames between the lines.
        let t_host = uptime_ms();
        let mut incoming = [0u8; 64];
        let n = log.read_ready(&mut incoming);
        let mut requests = Vec::new();
        for result in deframer.process(&incoming[..n]) {
            match result {
                DeframeResult::Frame(data) => requests.push(data),
                DeframeResult::TooShort | DeframeResult::Oversized => log.deframe_dropped(),
                DeframeResult::NeedMore => {}
            }
        }
        // Every loss on the line said when it happens (1440).
        log.say_rx_counts(false);
        let mut busy = n > 0;
        for data in requests {
            if matches!(
                envelope::decode_frame(&data),
                Err(EnvelopeError::NotEnvelope)
            ) {
                // Not the control envelope: a Reticulum packet for the node.
                stall::stage(Core::Io, Stage::Packet);
                node.packet(&data, uptime_ms(), &mut stores, &mut log);
                wdt.feed();
                continue;
            }
            if let Ok(frame) = envelope::decode_frame(&data) {
                if frame.frame_type == envelope::TYPE_HOME_CODE {
                    stall::stage(Core::Io, Stage::Code);
                    // The code written over the control link (1415): the
                    // record before the ack, then the same join as typed.
                    let answer = match envelope::decode_home_code_payload(frame.payload) {
                        None => envelope::encode_refusal(
                            envelope::TYPE_HOME_CODE,
                            envelope::REFUSE_MALFORMED,
                        ),
                        Some(text) => {
                            match node.take_code(
                                text,
                                &mut rng,
                                &mut stores,
                                batch,
                                &mut log,
                                &|| wdt.feed(),
                            ) {
                                Ok(taken) => {
                                    if let Some(taken) = taken {
                                        to_ui(ToUi::Model(Box::new(taken)));
                                        to_ui(ToUi::Word(node.word()));
                                    }
                                    envelope::encode_ack(envelope::TYPE_HOME_CODE)
                                }
                                Err(CodeRefusal::Value) => envelope::encode_refusal(
                                    envelope::TYPE_HOME_CODE,
                                    envelope::REFUSE_VALUE,
                                ),
                                Err(CodeRefusal::Persist) => {
                                    to_ui(ToUi::CodeNote(CODE_NOT_SAVED));
                                    envelope::encode_refusal(
                                        envelope::TYPE_HOME_CODE,
                                        envelope::REFUSE_PERSIST,
                                    )
                                }
                            }
                        }
                    };
                    log.frame(&answer);
                    continue;
                }
                #[cfg(m9_bench)]
                if frame.frame_type == envelope::TYPE_KEY_INJECT {
                    stall::stage(Core::Io, Stage::Line);
                    let answer = bench::take(frame.payload, kb, &mut inject);
                    log.frame(&answer);
                    continue;
                }
                #[cfg(m9_bench)]
                if frame.frame_type == envelope::TYPE_BENCH_OWE {
                    stall::stage(Core::Io, Stage::Line);
                    let answer = bench::owe(frame.payload, &mut node, &mut log);
                    log.frame(&answer);
                    continue;
                }
                #[cfg(m9_bench)]
                if frame.frame_type == envelope::TYPE_BENCH_ARRIVE {
                    stall::stage(Core::Io, Stage::Line);
                    let answer = match envelope::decode_bench_arrive_payload(frame.payload) {
                        None => envelope::encode_refusal(
                            envelope::TYPE_BENCH_ARRIVE,
                            envelope::REFUSE_MALFORMED,
                        ),
                        Some(row) => {
                            to_ui(ToUi::BenchArrive(row));
                            envelope::encode_ack(envelope::TYPE_BENCH_ARRIVE)
                        }
                    };
                    log.frame(&answer);
                    continue;
                }
                if frame.frame_type == envelope::TYPE_FRAME_DUMP {
                    match envelope::decode_frame_dump_payload(frame.payload) {
                        None => log.frame(&envelope::encode_refusal(
                            envelope::TYPE_FRAME_DUMP,
                            envelope::REFUSE_MALFORMED,
                        )),
                        Some(Some(byte))
                            if Screen::of(byte).is_none() && byte != pair::DUMP_BYTE =>
                        {
                            log.frame(&envelope::encode_refusal(
                                envelope::TYPE_FRAME_DUMP,
                                envelope::REFUSE_VALUE,
                            ))
                        }
                        Some(screen) => {
                            log.frame(&envelope::encode_ack(envelope::TYPE_FRAME_DUMP));
                            to_ui(ToUi::Dump(screen));
                        }
                    }
                    continue;
                }
            }
            control(&data, &mut log);
        }

        // A key the bench holds answers again when it is due (1533).
        #[cfg(m9_bench)]
        bench::held(kb, &mut inject);

        turn.host = uptime_ms().saturating_sub(t_host);
        stall::stage(Core::Io, Stage::Node);
        let t_node = uptime_ms();
        node.turn(uptime_ms(), &mut stores, &mut log);
        turn.node = uptime_ms().saturating_sub(t_node);
        stall::stage(Core::Io, Stage::Take);
        let t_take = uptime_ms();

        // What the client brought goes over to the UI core: pulled
        // entries, ended asks, the word, and any write that failed.
        let drained = node.drain();
        if let Some(address) = node.address() {
            if !drained.entries.is_empty() {
                busy = true;
                to_ui(ToUi::Entries(drained.entries, address));
            }
            for answer in drained.answers {
                to_ui(ToUi::Answer(answer));
            }
        }
        if drained.join_unsaved {
            faulted = true;
            to_ui(ToUi::Fault(Some(StrId::M9JoinNotSaved)));
        } else if drained.unsaved > 0 && !faulted {
            faulted = true;
            to_ui(ToUi::Fault(Some(StrId::M9NotSaved)));
        }
        to_ui_word(node.word());

        // What the UI core asked for.
        for message in io_drain() {
            wdt.feed();
            match message {
                FromUi::Asks { verbs, marks } => {
                    stall::stage(Core::Io, Stage::Asks);
                    busy = true;
                    #[cfg(m9_bench)]
                    node.bench_before_ask(&mut log);
                    // An open's read mark (1542): what an open of an
                    // unread chat always sends, Load or not.
                    #[cfg(m9_bench)]
                    let opened = marks
                        .iter()
                        .any(|mark| mark.flag == loquax::home::wire::MarkFlag::Read);
                    let mut ids = || {
                        let mut id = [0u8; 16];
                        rand_core::RngCore::fill_bytes(&mut rng, &mut id);
                        id
                    };
                    node.ask(verbs, &marks, &mut ids, uptime_ms(), &mut stores, &mut log);
                    #[cfg(m9_bench)]
                    if opened {
                        node.bench_fire(&mut stores, &mut log);
                    }
                }
                FromUi::Rows { rows, gone, pages } => {
                    stall::stage(Core::Io, Stage::Rows);
                    // A chat read (1542): the opened one's row. An
                    // arrival's row is unread, so the owe the walk arms
                    // after one waits for the open.
                    #[cfg(m9_bench)]
                    let opened = rows.iter().any(|row| row.unread == 0);
                    node.said(rows, gone, pages, &mut stores, &mut log);
                    #[cfg(m9_bench)]
                    if opened {
                        node.bench_fire(&mut stores, &mut log);
                    }
                }
                FromUi::Load(peer) => {
                    stall::stage(Core::Io, Stage::Load);
                    stall::peer(&peer);
                    busy = true;
                    let want = home::page_entries();
                    #[cfg(m9_bench)]
                    let t_read = esp_hal::time::Instant::now();
                    if let (Some(address), Some((entries, earlier))) =
                        (node.address(), node.load(peer, want, &mut stores, &mut log))
                    {
                        #[cfg(m9_bench)]
                        bench::READ_US
                            .store(t_read.elapsed().as_micros() as u32, Ordering::Relaxed);
                        to_ui(ToUi::Page {
                            peer,
                            entries,
                            earlier,
                            address,
                        });
                    }
                    // The owe the bench armed fires while the UI core
                    // folds the page (1540).
                    #[cfg(m9_bench)]
                    node.bench_fire(&mut stores, &mut log);
                }
                #[cfg(m9_bench)]
                FromUi::BenchArrive { peer, at } => {
                    stall::stage(Core::Io, Stage::Line);
                    arrivals += 1;
                    if !node.bench_arrive(peer, at, arrivals, &mut stores, &mut log) {
                        log.line("[BENCH] ", format_args!("arrive=refused reason=no-home"));
                    }
                }
                // Whole, however long (1521b): the census's screen lines
                // outgrew the fixed line and lost their verdict's reason.
                FromUi::Line(prefix, body) => {
                    stall::stage(Core::Io, Stage::Line);
                    log.text(prefix, &body);
                }
                FromUi::Raw(body) => {
                    stall::stage(Core::Io, Stage::Line);
                    log.raw(&body);
                }
                // Every line before anything else is written (1533): the
                // 4.6 s of a dump fed line by line to the watchdog.
                // Every line of it, waiting for the ring's room (1507).
                FromUi::Dump(lines) => {
                    stall::stage(Core::Io, Stage::Line);
                    log.set_lossy(false);
                    for line in lines {
                        log.raw(&line);
                        wdt.feed();
                    }
                    log.set_lossy(true);
                }
                FromUi::Type(size) => {
                    // The reader's type into the client record (1532).
                    let kept = match node.keep_type(size.large(), &mut stores) {
                        Some(true) => "yes",
                        Some(false) => "no reason=write",
                        None => "no reason=no-record",
                    };
                    log.line("[STORE] ", format_args!("type={} kept={kept}", size.name()));
                }
                FromUi::Code(text) => {
                    stall::stage(Core::Io, Stage::Code);
                    match node.take_code(&text, &mut rng, &mut stores, batch, &mut log, &|| {
                        wdt.feed()
                    }) {
                        Ok(Some(taken)) => {
                            to_ui(ToUi::Model(Box::new(taken)));
                            to_ui(ToUi::Word(node.word()));
                        }
                        Ok(None) => {}
                        Err(CodeRefusal::Value) => {}
                        Err(CodeRefusal::Persist) => to_ui(ToUi::CodeNote(CODE_NOT_SAVED)),
                    }
                }
            }
        }

        // The keyboard's losses, counted by the interrupt, said here:
        // the presses themselves are the UI core's.
        stall::stage(Core::Io, Stage::KbCount);
        let (kb_lost, kb_fails) = stall::locked(Core::Io, Lock::Keys, || {
            critical_section::with(|cs| {
                (
                    KB_PRESSES.borrow_ref(cs).lost(),
                    KB_FAILS.load(Ordering::Relaxed),
                )
            })
        });
        if (kb_lost, kb_fails) != kb_said {
            kb_said = (kb_lost, kb_fails);
            log.line(
                "[KB] ",
                format_args!("queue_lost={kb_lost} read_failed={kb_fails}"),
            );
        }

        // A quiet turn (1493): nothing came over the line and nothing was
        // pulled, so the store may erase its next ring sector now, and a
        // pulled page later writes into erased flash instead of holding
        // the interrupts for the erase.
        if !busy {
            stall::stage(Core::Io, Stage::Quiet);
            node.quiet(&mut stores, &mut log);
        }

        turn.take = uptime_ms().saturating_sub(t_take);
        turn.say(&mut log, node.entries());
        if core::mem::take(&mut first_turn) {
            log.set_lossy(true);
        } else if !busy && KB_TICKING.load(Ordering::Acquire) {
            // A quiet turn waits for the next interrupt (1507): the
            // keyboard's tick within 10 ms, the line's bytes, the send
            // FIFO running dry. A frame under the receive threshold waits
            // for the tick, as it waited for the loop before.
            stall::stage(Core::Io, Stage::IoIdle);
            leviculum_esp::idle::wait(0);
        }
    }
}

/// The last word the UI core was told, so the queue carries changes, not
/// a word per turn.
fn to_ui_word(word: Option<Word>) {
    static LAST: Mutex<RefCell<Option<Option<Word>>>> = Mutex::new(RefCell::new(None));
    stall::locked(Core::Io, Lock::Word, || {
        critical_section::with(|cs| {
            let mut last = LAST.borrow_ref_mut(cs);
            if last.as_ref() != Some(&word) {
                *last = Some(word);
                TO_UI.borrow_ref_mut(cs).push_back(ToUi::Word(word));
            }
        })
    });
}

/// What a turn of the UI core's folds owe the I/O core (1493, 1521).
#[derive(Default)]
struct Wants {
    verbs: Vec<Verb>,
    marks: Vec<Mark>,
    rows: Vec<Row>,
    gone: Vec<[u8; 16]>,
    loads: Vec<[u8; 16]>,
    /// Pulled pages folded this turn.
    pages: u32,
}

/// The pairing screen's words are English (1363); so is this state.
const CODE_NOT_SAVED: &str = "The code could not be saved. Try again.";

/// What wakes the UI core (1507): the GPIO block the TE line's interrupt
/// is bound through, the line, and the core's own tick's timer. Bound
/// on the UI core, which is where their interrupts are taken.
struct UiWakes {
    io: Io<'static>,
    te: m9::LcdTe<'static>,
    tick: esp_hal::timer::timg::Timer<'static>,
}

/// The UI core (1493): the model, the scene, the renderer, the panel.
/// It drains the key queue and the I/O core's messages, folds them,
/// routes what the fold asks (`home::route`) back to the I/O core, and
/// draws at most once per turn, whatever arrived.
fn ui_main(
    mut panel: Panel<'static>,
    mut model: Box<UiModel<Field>>,
    mut shown: Shown,
    kb: Protocol,
    seed: [u8; 32],
    wdt: Wdt<TIMG1<'static>>,
    wakes: UiWakes,
) -> ! {
    stall::stage(Core::Ui, Stage::Census);
    let mut log = UiLog;
    // The TE line's interrupt on this core (1507): bound and listened to
    // here, so its edges wake the core that draws.
    let UiWakes {
        mut io,
        te: te_pin,
        tick,
    } = wakes;
    io.set_interrupt_handler(te_edge);
    let mut te_line = Input::new(te_pin, InputConfig::default());
    critical_section::with(|cs| {
        te_line.listen(Event::RisingEdge);
        TE_LINE.borrow_ref_mut(cs).replace(te_line);
    });
    // Its tick, bound here too (1507): the core that waits has a wake
    // within a frame whatever the TE line does.
    let mut timer = PeriodicTimer::new(tick);
    timer.set_interrupt_handler(ui_tick);
    let ticking = match timer.start(Duration::from_millis(UI_TICK_MS)) {
        Ok(()) => {
            critical_section::with(|cs| {
                timer.listen();
                UI_TICK.borrow_ref_mut(cs).replace(timer);
            });
            true
        }
        Err(e) => {
            log.line("[UI] ", alloc::format!("tick=down err={e:?}"));
            false
        }
    };
    let mut frame = Frame::new();
    let before = leviculum_esp::heap_live();
    let Ok(mut renderer) = Renderer::new(&mut frame) else {
        log.line(
            "[DISP] ",
            alloc::string::String::from("state=down reason=renderer"),
        );
        loop {
            core::hint::spin_loop();
        }
    };
    // The map's passes timed for its `[MAP]` line (1531).
    renderer.set_clock(leviculum_esp::uptime_us);
    let renderer_held = leviculum_esp::heap_live() - before;
    log.line(
        "[CENSUS] ",
        alloc::format!(
            "renderer held={renderer_held} budget={} verdict={}",
            census::RENDERER_HELD,
            if renderer_held < census::RENDERER_HELD {
                "ok"
            } else {
                "over"
            }
        ),
    );
    // The map beside it (slice seven): the region read in place from the
    // image, the painter's storage on the heap.
    let before = leviculum_esp::heap_live();
    let Ok(mut map) = Map::new(map::FIXTURE) else {
        log.line(
            "[DISP] ",
            alloc::string::String::from("state=down reason=region"),
        );
        loop {
            core::hint::spin_loop();
        }
    };
    let map_held = leviculum_esp::heap_live() - before;
    log.line(
        "[CENSUS] ",
        alloc::format!(
            "map held={map_held} budget={} verdict={}",
            census::PAINTER_HELD,
            if map_held < census::PAINTER_HELD {
                "ok"
            } else {
                "over"
            }
        ),
    );

    // The census: every screen, built and drawn as the host's census
    // does, against the same budgets — on this core's stack, which is
    // where every scene and draw lives now (1493).
    let stack_floor = core::ptr::addr_of!(APP_CORE_STACK) as usize;
    let mut probe = BoardProbe::over(stack_floor);
    // Both type sizes (1532): the large one's lines say `<screen>-large`.
    for size in Size::ALL {
        for walked in fixture::SCREENS {
            let budget = walked.budget();
            let (mut walk_model, walk_row) = walked.walk();
            // The walk's model is the fixture's, no home's: nothing it
            // asks has a store to go to.
            let _ = house::set_size(size, &mut walk_model, &mut renderer);
            let name = census::screen_name(walked.name(), size);
            match census::measure_at(&mut probe, &walk_model, walk_row, &mut renderer, &mut map) {
                Ok(got) => log.line(
                    "[CENSUS] ",
                    alloc::format!(
                        "{}",
                        census::Line {
                            screen: &name,
                            census: &got,
                            budget: &budget,
                        }
                    ),
                ),
                Err(_) => log.line("[CENSUS] ", alloc::format!("screen={name} draw=failed")),
            }
        }
    }
    // The map's picture kept between draws from here on (1507): after the
    // census, whose screens are measured as drawn without it.
    let kept = map.keep_pictures();
    log.line("[CENSUS] ", alloc::format!("map_kept held={kept}"));
    // Core 0 may run again (1521b).
    CENSUS_RUNNING.store(false, Ordering::Release);

    // The ids a fold's asks carry (1493): the boot's entropy hashed with
    // a counter, unique across reboots, drawn without a Trng over here.
    let mut counter: u64 = 0;
    let mut ids = move || {
        counter += 1;
        let mut buf = [0u8; 40];
        buf[..32].copy_from_slice(&seed);
        buf[32..].copy_from_slice(&counter.to_le_bytes());
        let hash = leviculum_core::crypto::sha256(&buf);
        let mut id = [0u8; 16];
        id.copy_from_slice(&hash[..16]);
        id
    };

    // The reader's type size, the record's (1532).
    let size = shown.overview.size;
    // What a size set asks of the model (1546): the selected chat's page
    // among it, routed by the loop's next turn. Dropped before, the
    // boot's cursor chat never loaded and opened with no read mark.
    let mut asked = house::set_size(size, &mut model, &mut renderer);
    log.line("[TYPE] ", alloc::format!("size={} via=boot", size.name()));

    // The M9's own cursor over a group's sheet or picker (slice six).
    let mut row = 0;
    // The last `[PAGE]` line said (1533).
    let mut said = page::Said::default();
    redraw(
        &mut renderer,
        &mut map,
        &mut panel,
        &model,
        row,
        &shown,
        &mut said,
        &mut log,
    );
    // What the panel shows (1491): a page that changed nothing draws
    // nothing.
    let mut drawn = screen::Drawn::default();
    drawn.drawn(&model);
    // What the reader saw, said to the model, and the clocks its dwell
    // runs on (1541).
    let mut reader = Reader::default();
    // One draw per frame of the panel (1507): what the turns change is
    // owed until the next blank, then drawn once.
    let mut pace = Pace::new();
    pace.drew(TE_EDGES.load(Ordering::Relaxed), pace_now());
    let mut draw_owed = false;
    // What paces the draws, as the last `[DISP] pace=` line said it.
    let mut paced_by: Option<pace::Source> = None;
    // Turns whose change the pace merged into a later frame's draw.
    let mut merged: u32 = 0;

    // An opened chat's page on its way, for the `[OPEN]` line (1540).
    #[cfg(m9_bench)]
    let mut open = bench::Open::default();
    // The peer of the bench's arrival on its way to the fold (1540).
    #[cfg(m9_bench)]
    let mut arriving: Option<[u8; 16]> = None;

    // The map's words by the clock (1531): the key hint after it opens
    // and after a key it does not know, the credit once a boot.
    let mut chrome = map::Chrome::default();

    // From here the UI core's loop feeds its own watchdog (1534).
    let wdt = Watchdog::arm(wdt);
    loop {
        wdt.feed();
        stall::turned(Core::Ui);
        stall::stage(Core::Ui, Stage::Drain);
        let mut dirty = false;
        let mut wants = Wants::default();
        let route_out = |model: &mut UiModel<Field>,
                         effects: Vec<loquax::ui::UiEffect>,
                         mut ids: &mut dyn FnMut() -> [u8; 16],
                         log: &mut UiLog,
                         wants: &mut Wants|
         -> bool {
            let routed = home::route(model, effects, &mut ids);
            for line in routed.notices {
                log.line("[UI] ", alloc::format!("notice=\"{line}\""));
            }
            for name in routed.unrouted {
                log.line("[UI] ", alloc::format!("unrouted={name}"));
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
        };

        // What the last size set asked (1546).
        let owed = core::mem::take(&mut asked);
        if !owed.is_empty() && route_out(&mut model, owed, &mut ids, &mut log, &mut wants) {
            dirty = true;
        }
        // The model's clocks once a second (1541): a bubble on screen for
        // the dwell is seen, and its mark goes to the home.
        let (ticked, first_day) = reader.tick(&mut model, leviculum_esp::uptime_ms());
        if route_out(&mut model, ticked, &mut ids, &mut log, &mut wants) || first_day {
            dirty = true;
        }

        // What the I/O core brought.
        for message in ui_drain() {
            match message {
                ToUi::Model(taken) => {
                    model = taken;
                    row = 0;
                    shown.pairing = None;
                    // The size the reader chose before the code, set on
                    // the new model and kept in the new record (1532).
                    asked.extend(house::set_size(
                        shown.overview.size,
                        &mut model,
                        &mut renderer,
                    ));
                    log.send(FromUi::Type(shown.overview.size));
                    dirty = true;
                }
                ToUi::Entries(entries, address) => {
                    wants.pages += 1;
                    reader.saw(&entries, leviculum_esp::uptime_ms());
                    let (_, effects) = home::take(&mut model, &entries, &address);
                    if route_out(&mut model, effects, &mut ids, &mut log, &mut wants) {
                        dirty = true;
                    }
                    #[cfg(m9_bench)]
                    if let Some(peer) = arriving {
                        if bench::carries(&entries, &peer) {
                            arriving = None;
                            bench::folded(&model, &peer, &mut log);
                            dirty = true;
                        }
                    }
                }
                ToUi::Answer(answer) => {
                    let effects = home::answered(&mut model, answer);
                    if route_out(&mut model, effects, &mut ids, &mut log, &mut wants) {
                        dirty = true;
                    }
                }
                ToUi::Page {
                    peer,
                    entries,
                    earlier,
                    address,
                } => {
                    reader.saw(&entries, leviculum_esp::uptime_ms());
                    #[cfg(not(m9_bench))]
                    let effects = home::page_in(&mut model, peer, &entries, earlier, &address);
                    #[cfg(m9_bench)]
                    let effects = open.page_in(&mut model, peer, &entries, earlier, &address);
                    route_out(&mut model, effects, &mut ids, &mut log, &mut wants);
                    // The page is on the chat now, whatever it asked.
                    dirty = true;
                }
                #[cfg(m9_bench)]
                ToUi::BenchArrive(row) => {
                    if let Some(peer) = bench::arrive(&mut model, row, &mut log) {
                        arriving = Some(peer);
                        dirty = true;
                    }
                }
                ToUi::Word(word) => {
                    if word != shown.word {
                        shown.word = word;
                        dirty = true;
                    }
                }
                ToUi::Fault(fault) => {
                    if fault != shown.fault {
                        shown.fault = fault;
                        dirty = true;
                    }
                }
                ToUi::CodeNote(note) => {
                    if let Some(pairing) = shown.pairing.as_mut() {
                        pairing.note = Some(note);
                        dirty = true;
                    }
                }
                ToUi::Dump(screen) => {
                    // The goldens are the default size's (1532).
                    renderer.set_size(Size::DEFAULT);
                    if let Some(walked) = screen.and_then(Screen::of) {
                        let (walk_model, walk_row) = walked.walk();
                        *model = walk_model;
                        row = walk_row;
                        shown.pairing = None;
                        redraw(
                            &mut renderer,
                            &mut map,
                            &mut panel,
                            &model,
                            row,
                            &Shown::default(),
                            &mut said,
                            &mut log,
                        );
                        log.line("[DUMP] ", alloc::format!("screen={}", walked.name()));
                    }
                    if screen == Some(pair::DUMP_BYTE) {
                        let fresh = Shown {
                            pairing: Some(Pairing::new()),
                            word: None,
                            fault: None,
                            map: map::Overlay::default(),
                            overview: Overview::default(),
                        };
                        redraw(
                            &mut renderer,
                            &mut map,
                            &mut panel,
                            &model,
                            0,
                            &fresh,
                            &mut said,
                            &mut log,
                        );
                        log.line("[DUMP] ", alloc::format!("screen={}", pair::DUMP_NAME));
                    }
                    let dumped = renderer.panel();
                    let mut lines = Vec::new();
                    dump::encode(WIDTH, HEIGHT, |x, y| dumped.pixel(x, y), &mut |line| {
                        lines.push(String::from(line))
                    });
                    log.send(FromUi::Dump(lines));
                    asked.extend(house::set_size(
                        shown.overview.size,
                        &mut model,
                        &mut renderer,
                    ));
                    dirty = true;
                }
            }
        }

        // The keys the timer read, oldest first (1463): every press,
        // however long the turn that held it was; all of them before the
        // one draw.
        stall::stage(Core::Ui, Stage::Keys);
        let (presses, _, _) = kb_drain();
        for press in presses {
            let ms = press.ms;
            log.raw(kbraw::raw_line(ms, press.answer()));
            let mut events = Vec::new();
            let mut command = None;
            match kb.read(press.answer()) {
                Read::Idle => {}
                Read::Mixed => log.line("[KB] ", alloc::string::String::from("answer=mixed")),
                Read::Code(code) => {
                    if let Some(key) = Key::of(code) {
                        log.raw(keyboard::key_line(ms, code, key));
                        // The house key and the open overview (1532);
                        // the pairing has the keys while it shows.
                        let took = match shown.pairing {
                            None => shown.overview.press(key, ms),
                            Some(_) => None,
                        };
                        if let Some(did) = took {
                            dirty |= switched(did, &mut model, &mut renderer, &mut log, &mut asked);
                            continue;
                        }
                        events.extend(key.event());
                        command = key.command();
                    }
                }
            }
            // A control that runs a command (1440): the chats list, the
            // compose line, the map. Not on the pairing screen, which
            // only Back leaves.
            if let (Some(command), None) = (command, shown.pairing.as_ref()) {
                keyboard::run(&mut model, &mut row, command);
                dirty = true;
            }
            for event in events {
                dirty = true;
                if let Some(pairing) = shown.pairing.as_mut() {
                    if let Some(code) = pairing.key(&event) {
                        // The code goes to the I/O core, which owns the
                        // flash and the join; the model comes back.
                        log.send(FromUi::Code(code.encode()));
                    }
                    if event.code == KeyCode::Esc && event.kind != KeyEventKind::Release {
                        shown.pairing = None;
                    }
                    continue;
                }
                if !map::bound(&model, &event) {
                    chrome.nudge(leviculum_esp::uptime_ms());
                }
                let effects = group::press(&mut model, &mut row, &event);
                if route_out(&mut model, effects, &mut ids, &mut log, &mut wants) {
                    frame_whole(&renderer);
                    dirty = true;
                }
            }
        }

        stall::stage(Core::Ui, Stage::Send);
        if !wants.verbs.is_empty() || !wants.marks.is_empty() {
            log.send(FromUi::Asks {
                verbs: core::mem::take(&mut wants.verbs),
                marks: core::mem::take(&mut wants.marks),
            });
        }
        // The rows after the asks: the marks they keep move the ring the
        // summary's state then says (1521).
        if !wants.rows.is_empty() || !wants.gone.is_empty() || wants.pages > 0 {
            log.send(FromUi::Rows {
                rows: core::mem::take(&mut wants.rows),
                gone: core::mem::take(&mut wants.gone),
                pages: core::mem::take(&mut wants.pages),
            });
        }
        for peer in core::mem::take(&mut wants.loads) {
            #[cfg(m9_bench)]
            open.asked(peer);
            log.send(FromUi::Load(peer));
        }

        stall::stage(Core::Ui, Stage::Draw);
        // A house tap's gap, the switch's word (1532).
        let did = shown.overview.tick(leviculum_esp::uptime_ms() as u32);
        dirty |= switched(did, &mut model, &mut renderer, &mut log, &mut asked);
        dirty |= drawn.due(&model);
        // The map's words come and go by the clock (1531): a redraw when
        // they change, none while they stand.
        let now = leviculum_esp::uptime_ms();
        chrome.seen(map::shown(&model), now);
        let overlay = chrome.overlay(now, || !model.map_pins().is_empty());
        if overlay != shown.map {
            shown.map = overlay;
            dirty = true;
        }
        let edges = TE_EDGES.load(Ordering::Relaxed);
        let now = pace_now();
        let source = pace.source(edges, now);
        if paced_by != Some(source) {
            paced_by = Some(source);
            log.line(
                "[DISP] ",
                alloc::format!("pace={} edges={edges} merged={merged}", source.name()),
            );
        }
        draw_owed |= dirty;
        if draw_owed && !pace.due(edges, now) {
            merged = merged.saturating_add(u32::from(dirty));
        } else if draw_owed {
            let flushing = redraw(
                &mut renderer,
                &mut map,
                &mut panel,
                &model,
                row,
                &shown,
                &mut said,
                &mut log,
            );
            pace.drew(edges, now);
            draw_owed = false;
            // What the frame showed of the open chat (1541), said when it
            // moved; its asks route on the next turn, before its sends.
            asked.extend(reader.say(&mut model, renderer.seen(), leviculum_esp::uptime_ms()));
            drawn.drawn(&model);
            #[cfg(not(m9_bench))]
            let _ = flushing;
            // A draw that changed no row is said at once (1540).
            #[cfg(m9_bench)]
            if !flushing {
                bench::FLUSH_US.store(0, Ordering::Relaxed);
                open.say(&mut log);
            }
        }
        // The flush on its way, moved on between the turns (1507); the
        // bench's `[OPEN]` waits for its last byte.
        let landed = flushed(&mut panel, renderer.panel(), false, &mut log);
        #[cfg(m9_bench)]
        if let Some(us) = landed {
            bench::FLUSH_US.store(us as u32, Ordering::Relaxed);
            open.say(&mut log);
        }
        #[cfg(not(m9_bench))]
        let _ = landed;
        // Nothing to do until the next frame or a key (1507): no flush on
        // its way, no fold's effects owed, no draw that may go now. The
        // TE edge or the tick wakes the core; a key waits at most until
        // then, and its draw would have waited for the frame anyway.
        let now = pace_now();
        let edges = TE_EDGES.load(Ordering::Relaxed);
        if ticking && !panel.busy() && asked.is_empty() && !(draw_owed && pace.due(edges, now)) {
            stall::stage(Core::Ui, Stage::UiIdle);
            leviculum_esp::idle::wait(1);
        } else {
            core::hint::spin_loop();
        }
    }
}

/// What the overview did (1532): the size set on the renderer and the
/// model, said in a `[TYPE]` line and sent to the I/O core for the
/// record, what the model asked back kept in `asked` for the next turn's
/// route (1546). Whether the screen changed.
fn switched(
    did: Did,
    model: &mut UiModel<Field>,
    renderer: &mut Renderer<'_, Frame>,
    log: &mut UiLog,
    asked: &mut Vec<loquax::ui::UiEffect>,
) -> bool {
    if let Some(size) = did.size {
        asked.extend(house::set_size(size, model, renderer));
        log.line(
            "[TYPE] ",
            alloc::format!("size={} via={}", size.name(), did.via),
        );
        log.send(FromUi::Type(size));
    }
    did.redraw
}

/// Every row owed again: `Ctrl-l`'s whole redraw.
fn frame_whole(renderer: &Renderer<'_, Frame>) {
    renderer.panel().forget_flushed();
}

/// The fault screen's lines: `words` wrapped to the panel's cells, a
/// margin cell before each line.
fn fault_lines(words: &str, cols: usize, rows: usize) -> Vec<leviculum_ui::style::RLine> {
    use leviculum_ui::style::{RLine, RStyle};
    use loquax_m9::screen::{fill, push};
    let plain = RStyle::default();
    let mut out = Vec::new();
    let width = cols.saturating_sub(2).max(1);
    let mut line = String::new();
    out.push(fill(RLine::default(), cols, plain));
    for word in words.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            let mut row = RLine::default();
            push(&mut row, &alloc::format!(" {line}"), plain);
            out.push(fill(row, cols, plain));
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        let mut row = RLine::default();
        push(&mut row, &alloc::format!(" {line}"), plain);
        out.push(fill(row, cols, plain));
    }
    while out.len() < rows {
        out.push(fill(RLine::default(), cols, plain));
    }
    out.truncate(rows);
    out
}

/// One control frame from the host: the envelope's capability query;
/// everything else envelope-shaped is refused by name (`TYPE_HOME_CODE`
/// and `TYPE_FRAME_DUMP` are the loop's). Frames that are not the
/// envelope are the node's and never come here.
fn control(data: &[u8], log: &mut UartLog<'_>) {
    let frame = match envelope::decode_frame(data) {
        Ok(frame) => frame,
        Err(EnvelopeError::NotEnvelope) => return,
        Err(EnvelopeError::Malformed { frame_type }) => {
            log.frame(&envelope::encode_refusal(
                frame_type,
                envelope::REFUSE_MALFORMED,
            ));
            return;
        }
    };
    match frame.frame_type {
        envelope::TYPE_CAPABILITIES if frame.payload.is_empty() => {
            // `serial_mux=1`: the one UART carries text and frames, so lnsd
            // escapes its flow bytes and strips the line's (1414).
            log.frame(&envelope::encode_capability_report_with_facts(
                &[
                    envelope::TYPE_CAPABILITIES,
                    envelope::TYPE_FRAME_DUMP,
                    envelope::TYPE_HOME_CODE,
                    #[cfg(m9_bench)]
                    envelope::TYPE_KEY_INJECT,
                    #[cfg(m9_bench)]
                    envelope::TYPE_BENCH_OWE,
                    #[cfg(m9_bench)]
                    envelope::TYPE_BENCH_ARRIVE,
                ],
                &[envelope::FACT_SERIAL_MUX],
            ));
        }
        other => {
            let reason = match other {
                envelope::TYPE_CAPABILITIES => envelope::REFUSE_MALFORMED,
                _ => envelope::REFUSE_UNKNOWN_TYPE,
            };
            log.frame(&envelope::encode_refusal(other, reason));
        }
    }
}

/// The bench's key injection (1533): a test hook, compiled only into an
/// image built with `LEVICULUM_M9_BENCH=1` (`scripts/build-m9-image.sh
/// --bench`, `cfg(m9_bench)` from `build.rs`), whose boot says
/// `[BENCH] inject=on`. A `TYPE_KEY_INJECT`
/// frame puts the key's answer into [`KB_PRESSES`] where the keyboard's
/// timer interrupt puts a polled one, so the UI core reads it as a finger's
/// press: the `KBRAW` and `KEY` lines, the fold, the draw, the flush and
/// the `[PAGE]` line are the real path's.
#[cfg(m9_bench)]
mod bench {
    use super::{
        envelope, kbraw, map, screen, stall, uptime_ms, Core, Entry, Field, FromUi, Handheld, Lock,
        Map, Protocol, Renderer, UartLog, UiLog, UiModel, KB_PRESSES,
    };
    use alloc::format;
    use alloc::vec::Vec;
    use core::sync::atomic::{AtomicU32, Ordering};
    use esp_hal::time::Instant;
    use loquax::home::board::rows;
    use loquax::ui::{update, Pane, UiEffect, UiEvent};
    use loquax_m9::frame::Frame;
    use loquax_m9::kbraw::Inject;

    /// The last open's ring read on core 0, microseconds (1540).
    pub static READ_US: AtomicU32 = AtomicU32::new(0);
    /// The last draw's scene build, its cells drawn, and its flush, each
    /// on the UI core, microseconds.
    static SCENE_US: AtomicU32 = AtomicU32::new(0);
    static DRAW_US: AtomicU32 = AtomicU32::new(0);
    pub static FLUSH_US: AtomicU32 = AtomicU32::new(0);

    /// One answer for `code` into the keyboard's queue, at the uptime now.
    fn queue(kb: Protocol, code: u8) {
        let (answer, len) = kbraw::answer_of(&kb, code);
        stall::locked(Core::Io, Lock::Keys, || {
            critical_section::with(|cs| {
                KB_PRESSES
                    .borrow_ref_mut(cs)
                    .poll(&kb, uptime_ms() as u32, &answer[..len])
            })
        });
    }

    /// A `TYPE_KEY_INJECT` payload taken: the press queued and the hold
    /// kept, and the frame that answers it, an ack or a named refusal.
    pub fn take(payload: &[u8], kb: Protocol, inject: &mut Inject) -> Vec<u8> {
        match envelope::decode_key_inject_payload(payload) {
            None => envelope::encode_refusal(envelope::TYPE_KEY_INJECT, envelope::REFUSE_MALFORMED),
            Some(asked) if !asked.valid() => {
                envelope::encode_refusal(envelope::TYPE_KEY_INJECT, envelope::REFUSE_VALUE)
            }
            Some(asked) => {
                let code = inject.press(asked.code, uptime_ms() as u32, asked.hold_ms);
                queue(kb, code);
                envelope::encode_ack(envelope::TYPE_KEY_INJECT)
            }
        }
    }

    /// The held key's next answer, when it is due.
    pub fn held(kb: Protocol, inject: &mut Inject) {
        if let Some(code) = inject.due(uptime_ms() as u32) {
            queue(kb, code);
        }
    }

    /// A `TYPE_BENCH_OWE` payload taken (1540): the owe armed for the
    /// next open, and the frame that answers it.
    pub fn owe(payload: &[u8], node: &mut Handheld, log: &mut UartLog<'_>) -> Vec<u8> {
        let refuse = |reason| envelope::encode_refusal(envelope::TYPE_BENCH_OWE, reason);
        match envelope::decode_bench_owe_payload(payload) {
            None => refuse(envelope::REFUSE_MALFORMED),
            Some(flags) if flags & !envelope::BENCH_OWE_FLAGS != 0 => {
                refuse(envelope::REFUSE_VALUE)
            }
            Some(flags) => {
                if node.bench_owe(flags, log) {
                    envelope::encode_ack(envelope::TYPE_BENCH_OWE)
                } else {
                    refuse(envelope::REFUSE_UNSUPPORTED)
                }
            }
        }
    }

    /// The first four bytes of a peer, as the stall record says one.
    fn short(peer: &[u8; 16]) -> alloc::string::String {
        format!(
            "{:02x}{:02x}{:02x}{:02x}",
            peer[0], peer[1], peer[2], peer[3]
        )
    }

    /// A `TYPE_BENCH_ARRIVE` on the UI core (1540): the chat at `row`
    /// selected while the list shows, and its peer and time sent to the
    /// I/O core, which keeps the message and hands it back as a page.
    pub fn arrive(model: &mut UiModel<Field>, row: u8, log: &mut UiLog) -> Option<[u8; 16]> {
        let Some(arrival) = loquax_m9::bench::arrival(model, row) else {
            log.line(
                "[BENCH] ",
                format!("arrive=refused row={row} of=0 reason=no-chats"),
            );
            return None;
        };
        if model.focus == Pane::Conversations {
            model.selected = arrival.index;
        }
        log.line(
            "[BENCH] ",
            format!(
                "arrive=sent row={} of={} peer={}",
                arrival.row,
                arrival.of,
                short(&arrival.peer)
            ),
        );
        log.send(FromUi::BenchArrive {
            peer: arrival.peer,
            at: arrival.at,
        });
        Some(arrival.peer)
    }

    /// Whether a pulled page carries a message from `peer`: the packed
    /// LXMF message's source, bytes 16 to 32.
    pub fn carries(entries: &[Entry], peer: &[u8; 16]) -> bool {
        entries
            .iter()
            .any(|entry| entry.packed.get(16..32) == Some(&peer[..]))
    }

    /// The arrival folded: its chat's unread, which the walk's open marks.
    pub fn folded(model: &UiModel<Field>, peer: &[u8; 16], log: &mut UiLog) {
        let unread = model
            .conversations
            .iter()
            .find(|c| c.peer == *peer)
            .map_or(0, |c| c.unread);
        log.line(
            "[BENCH] ",
            format!("arrive=folded peer={} unread={unread}", short(peer)),
        );
    }

    /// [`Renderer::show`], the scene's build and its cells' draw timed
    /// apart where the screen is a scene (the map is drawn whole).
    pub fn show(
        renderer: &mut Renderer<'_, Frame>,
        model: &UiModel<Field>,
        look: &screen::Look,
        map: &mut Map<'_>,
    ) -> Result<(), loquax_m9::render::DrawError> {
        if map::shown(model) {
            SCENE_US.store(0, Ordering::Relaxed);
            let t0 = Instant::now();
            let drawn = renderer.show(model, look, map);
            DRAW_US.store(t0.elapsed().as_micros() as u32, Ordering::Relaxed);
            return drawn;
        }
        let t0 = Instant::now();
        let scene = screen::scene(model);
        let t1 = Instant::now();
        let drawn = renderer.draw(&scene, look);
        SCENE_US.store((t1 - t0).as_micros() as u32, Ordering::Relaxed);
        DRAW_US.store(t1.elapsed().as_micros() as u32, Ordering::Relaxed);
        drawn
    }

    /// An opened chat's page on its way (1540): asked when, folded how
    /// long, said once the frame after it is flushed.
    #[derive(Default)]
    pub struct Open {
        asked: Option<([u8; 16], Instant)>,
        /// The page folded and not said yet: peer, entries, wait,
        /// decode and fold, microseconds.
        folded: Option<([u8; 16], usize, u64, u64, u64)>,
    }

    impl Open {
        /// The UI core asked `peer`'s page.
        pub fn asked(&mut self, peer: [u8; 16]) {
            self.asked = Some((peer, Instant::now()));
        }

        /// `home::page_in`, its two halves timed: the entries decoded
        /// into messages (`rows::messages`) and the model's fold of them.
        pub fn page_in(
            &mut self,
            model: &mut UiModel<Field>,
            peer: [u8; 16],
            entries: &[Entry],
            earlier: usize,
            address: &[u8; 16],
        ) -> Vec<UiEffect> {
            let wait = match self.asked.take() {
                Some((asked, at)) if asked == peer => at.elapsed().as_micros(),
                _ => 0,
            };
            let t0 = Instant::now();
            let messages = rows::messages::<Field>(entries, address, peer);
            let t1 = Instant::now();
            let effects = update(
                model,
                UiEvent::Earlier {
                    peer,
                    messages,
                    earlier,
                },
            );
            let decode = (t1 - t0).as_micros();
            let fold = t1.elapsed().as_micros();
            self.folded = Some((peer, entries.len(), wait, decode, fold));
            effects
        }

        /// The `[OPEN]` line, after the draw that showed the page.
        pub fn say(&mut self, log: &mut UiLog) {
            let Some((peer, entries, wait, decode, fold)) = self.folded.take() else {
                return;
            };
            let ms = |us: u64| format!("{}.{:03}", us / 1000, us % 1000);
            let load = |at: &AtomicU32| u64::from(at.load(Ordering::Relaxed));
            log.line(
                "[OPEN] ",
                format!(
                    "peer={} entries={entries} wait_ms={} page_read_ms={} decode_ms={} fold_ms={} scene_ms={} draw_ms={} flush_ms={}",
                    short(&peer),
                    ms(wait),
                    ms(load(&READ_US)),
                    ms(decode),
                    ms(fold),
                    ms(load(&SCENE_US)),
                    ms(load(&DRAW_US)),
                    ms(load(&FLUSH_US)),
                ),
            );
        }
    }
}
