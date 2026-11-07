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
//! [DISP] splash rows=0..239 ms=<ms> backlight=on t=<ms>   (1485; or splash=failed spi=..., backlight=off)
//! [KB] addr=0x6d found=yes ... protocol=codes reg=0x01 len=8 poll_ms=10 t=<ms>
//! [CENSUS] renderer held=<n> budget=220000 verdict=ok t=<ms>
//! [CENSUS] screen=chats scene_held=... verdict=ok t=<ms>
//! [CENSUS] screen=chat scene_held=... verdict=ok t=<ms>
//! [CENSUS] screen=<groups|invite|members|admin|ask|pick|name> ...  (1366)
//! [CENSUS] map held=<n> budget=230000 verdict=ok t=<ms>         (1375)
//! [CENSUS] screen=<map|pan> ... verdict=ok t=<ms>               (1375)
//! [CENSUS] session=pull held=<n> peak=<n> stack=<n> verdict=ok pages=<n> entries=30 t=<ms>
//! [HOME] client=none page=<n> t=<ms>        or client=yes admitted=... cursor=...
//! [STORE] sectors=896 used=<n> entries=<n> cursor=<seq> torn=<n> foreign=<n> asks=<n> restore=<resumed|rebuilt|whole> restore_ms=<ms>  (1476, 1521)
//! [STORE] restore <how> decoded=<n> read_us=.. crc_us=.. decode_us=.. unpack_us=.. step_us=.. finish_us=.. rows_us=.. per_1000_ms ...  (1521, where the boot's time went)
//! [STORE] index=summaries records=<n> conversations=<n>   or index=rebuilt why=<no-state|ring-moved> ...  (1521)
//! [HOME] store=empty cursor=reset was=<n> cursor=0   (1476, a record whose store is gone; or store=behind/ahead)
//! [HOME] page ... taken=<n> of=<m> / pulled ... saved=store / joined=yes ... / failed ...   (as the client asks; the record is not written for a pull, 1463)
//! [HOME] code=typed / code=taken saved=yes / code=refused ...   (1415, a code taken in)
//! [STORE] kept=<n> writes=<n> erased=<n> cursor=<seq> used=<n> entries=<n>   (1476, every page pulled)
//! [STORE] summary records=<n> writes=<n> erased=<n>   (1521, the rows the folds changed, and the ring's state)
//! [STORE] page_read entries=<n> earlier=<n> ms=<ms>   (1521, a conversation opened or scrolled to its top)
//! [HOME] sent id=<8 hex> waiting=<n> / send=failed ...         (1415, a line typed in a chat)
//! [DISP] flush rows=0..239 ms=<ms> t=<ms>
//! [UART] rx_lost=<n> overrun=<n> deframe_drop=<n> rx_err=<n> loop_gap_max_ms=<n> t=<ms>
//!                           on every loss and with every banner (1440)
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
//! and the banner pair every five seconds. A `FRAME_DUMP` control frame
//! (`7e a4 a5 18 00 00 7e`, or `7e a4 a5 18 00 01 <screen> 7e`) is acked
//! and answered with the `PBM` dump of `loquax_m9::dump`; `<screen>` is a
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
use core::sync::atomic::{AtomicU32, Ordering};

use critical_section::Mutex;

use esp_hal::rng::{Trng, TrngSource};
use esp_hal::system::{CpuControl, Stack};
use esp_hal::{
    delay::Delay,
    dma_rx_buffer, dma_tx_buffer,
    gpio::{Level, Output, OutputConfig},
    i2c::master::{Config as I2cConfig, I2c},
    main,
    psram::{Psram, PsramConfig, PsramMode, PsramSize, SpiRamFreq},
    spi::{
        master::{Config as SpiConfig, Spi, SpiDma},
        Mode,
    },
    time::{Duration, Rate},
    timer::{timg::TimerGroup, PeriodicTimer},
    uart::Uart,
    Blocking,
};
use leviculum_core::envelope::{self, EnvelopeError};
use leviculum_core::framing::hdlc::{DeframeResult, Deframer};
use leviculum_esp::flash::Stores;
use leviculum_esp::handheld::{self, CodeRefusal, Handheld};
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
use loquax_m9::frame::{Frame, HEIGHT, WIDTH};
use loquax_m9::home::Answer;
use loquax_m9::kbraw::{self, Presses, Protocol, Read};
use loquax_m9::keyboard::{self, Key, POLL_MS};
use loquax_m9::map::{self, Map};
use loquax_m9::pair::{self, Pairing};
use loquax_m9::render::Renderer;
use loquax_m9::splash::{self, Panel as _};
use loquax_m9::{dump, group, home, screen};

esp_bootloader_esp_idf::esp_app_desc!();

/// How often the board re-states what it is: the five seconds every
/// leviculum board uses, so a reader that attaches late still reads it.
const BANNER_INTERVAL_MS: u64 = 5_000;

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

/// The timer's tick: one read of the controller, its answer queued. The
/// transfer is about a millisecond at 100 kHz, inside the critical
/// section; the UART's interrupt has 5.5 ms of slack behind it.
#[esp_hal::handler]
fn kb_tick() {
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
}

/// The answers kept since the last call, oldest first, and how many were
/// dropped and failed so far.
fn kb_drain() -> (Vec<kbraw::Press>, u32, u32) {
    critical_section::with(|cs| {
        let mut presses = KB_PRESSES.borrow_ref_mut(cs);
        let drained = core::iter::from_fn(|| presses.pop()).collect();
        (drained, presses.lost(), KB_FAILS.load(Ordering::Relaxed))
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
// with the cache off, which core 1 executes through).

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
}

/// What the UI core hands the I/O core.
enum FromUi {
    /// A fold's output for the home: the verbs for the ask queue and
    /// the marks among them for the store ([`Handheld::ask`]).
    Asks { verbs: Vec<Verb>, marks: Vec<Mark> },
    /// A formatted log line: prefix and body, printed by the one UART
    /// writer.
    Line(&'static str, String),
    /// A raw line (`KBRAW`, `KEY`, the `PBM` dump).
    Raw(String),
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
}

static TO_UI: Mutex<RefCell<VecDeque<ToUi>>> = Mutex::new(RefCell::new(VecDeque::new()));
static FROM_UI: Mutex<RefCell<VecDeque<FromUi>>> = Mutex::new(RefCell::new(VecDeque::new()));

fn to_ui(message: ToUi) {
    critical_section::with(|cs| TO_UI.borrow_ref_mut(cs).push_back(message));
}

fn ui_drain() -> Vec<ToUi> {
    critical_section::with(|cs| TO_UI.borrow_ref_mut(cs).drain(..).collect())
}

fn io_drain() -> Vec<FromUi> {
    critical_section::with(|cs| FROM_UI.borrow_ref_mut(cs).drain(..).collect())
}

/// The UI core's way to the log: every line goes through the queue to
/// the one UART writer on core 0.
struct UiLog;

impl UiLog {
    fn line(&mut self, prefix: &'static str, body: String) {
        critical_section::with(|cs| {
            FROM_UI
                .borrow_ref_mut(cs)
                .push_back(FromUi::Line(prefix, body))
        });
    }

    fn raw(&mut self, body: String) {
        critical_section::with(|cs| FROM_UI.borrow_ref_mut(cs).push_back(FromUi::Raw(body)));
    }

    fn send(&mut self, message: FromUi) {
        critical_section::with(|cs| FROM_UI.borrow_ref_mut(cs).push_back(message));
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
const COLMOD: u8 = 0x3A;
const COLMOD_RGB565: u8 = 0x55;

/// The panel on SPI2 with DMA, its chip select and data/command line,
/// and its backlight, on when low, lit only over a written panel (1485).
struct Panel<'d> {
    spi: SpiDma<'d, Blocking>,
    cs: Output<'d>,
    dc: Output<'d>,
    backlight: Output<'d>,
    lit: bool,
}

/// The boot order of `loquax_m9::splash`: the board's half.
impl splash::Panel for Panel<'_> {
    type Error = esp_hal::spi::Error;

    fn write(&mut self, frame: &Frame, first: u32, last: u32) -> Result<(), Self::Error> {
        self.flush(frame, first, last)
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
    fn cmd(&mut self, op: u8, data: &[u8]) -> Result<(), esp_hal::spi::Error> {
        self.cs.set_low();
        let r = self.send(op, data);
        self.cs.set_high();
        r
    }

    fn send(&mut self, op: u8, data: &[u8]) -> Result<(), esp_hal::spi::Error> {
        self.dc.set_low();
        self.spi.write(&[op])?;
        self.dc.set_high();
        if !data.is_empty() {
            self.spi.write(data)?;
        }
        Ok(())
    }

    /// The datasheet's power-on sequence with its delays, as the map
    /// firmware ran it on this unit, landscape.
    fn init(&mut self, rst: &mut Output<'_>, delay: &Delay) -> Result<(), esp_hal::spi::Error> {
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
        delay.delay_millis(10);
        self.cmd(DISPON, &[])
    }

    /// Rows `first..=last` of `frame`, full width, in one chip-select
    /// window: the ST7789's row window, so a moved cursor sends its band
    /// and not the screen. The frame holds native RGB565 words; the panel
    /// takes them high byte first, swapped here a DMA chunk at a time.
    fn flush(&mut self, frame: &Frame, first: u32, last: u32) -> Result<(), esp_hal::spi::Error> {
        let x1 = (WIDTH - 1) as u16;
        let (y0, y1) = (first as u16, last as u16);
        let words = &frame.pixels()[(first * WIDTH) as usize..((last + 1) * WIDTH) as usize];
        self.cs.set_low();
        let r = (|| {
            self.send(CASET, &[0, 0, (x1 >> 8) as u8, x1 as u8])?;
            self.send(
                RASET,
                &[(y0 >> 8) as u8, y0 as u8, (y1 >> 8) as u8, y1 as u8],
            )?;
            self.dc.set_low();
            self.spi.write(&[RAMWR])?;
            self.dc.set_high();
            let mut chunk = [0u8; 4096];
            for part in words.chunks(chunk.len() / 2) {
                for (i, word) in part.iter().enumerate() {
                    chunk[2 * i..2 * i + 2].copy_from_slice(&word.to_be_bytes());
                }
                self.spi.write(&chunk[..2 * part.len()])?;
            }
            Ok(())
        })();
        self.cs.set_high();
        r
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
}

/// Draw what `model` shows, the M9's cursor over a group's rows on `row`,
/// the map in pixels when it is open, or the pairing over all of it, and
/// send the rows that changed to the panel. On the UI core (1493); the
/// lines go through the queue.
fn redraw(
    renderer: &mut Renderer<'_, Frame>,
    map: &mut Map<'_>,
    panel: &mut Panel<'_>,
    model: &UiModel<Field>,
    row: usize,
    shown: &Shown,
    log: &mut UiLog,
) {
    let look = screen::Look {
        row,
        home: shown.word,
        fault: shown.fault,
        ..screen::Look::of(model)
    };
    let drawn = match &shown.pairing {
        Some(pairing) => pair::draw(renderer, pairing, shown.word, &look),
        None => renderer.show(model, &look, map),
    };
    if drawn.is_err() {
        log.line("[DISP] ", alloc::string::String::from("draw=failed"));
        return;
    }
    let frame = renderer.panel();
    let Some((first, last)) = frame.take_dirty() else {
        return;
    };
    let t0 = esp_hal::time::Instant::now();
    let flushed = panel.flush(frame, first, last);
    let us = t0.elapsed().as_micros();
    match flushed {
        Ok(()) => {
            log.line(
                "[DISP] ",
                alloc::format!(
                    "flush rows={first}..{last} ms={}.{:03}",
                    us / 1000,
                    us % 1000
                ),
            );
            // The splash's write failed (1485): the frame was whole again,
            // so this flush wrote every row, and the backlight may light.
            if !panel.lit {
                panel.light(true);
                log.line("[DISP] ", alloc::string::String::from("backlight=on"));
            }
        }
        Err(e) => {
            // The panel's bytes are unknown now: every row is owed, so
            // the next flush that lands is whole — without this the
            // failed band stayed stale for ever (1493).
            frame.forget_flushed();
            log.line("[DISP] ", alloc::format!("flush=failed spi={e:?}"));
        }
    }
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
    let mut panel = Panel {
        spi,
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
        Err(e) => log.line("[DISP] ", format_args!("state=down reason=init spi={e:?}")),
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
            format_args!("splash=failed spi={e:?} backlight=off"),
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
                let _ = panel.flush(shown, first, last);
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

    // The keyboard's poll from here on is the timer's (1463).
    if let Some(bus) = i2c.take() {
        let group = TimerGroup::new(p.TIMG0);
        let mut timer = PeriodicTimer::new(group.timer0);
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
    // A device of a home draws its home's chats: what its message store
    // kept (1476), then every pull; a board that is none opens on the
    // pairing, and Back from there shows the fixture as before (1415).
    let restored = match stores.load_client() {
        Ok(Some(record)) => node.client(record, batch, &mut stores, &mut log),
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
    let started = cpu.start_app_core(ui_stack, move || ui_main(panel, model, shown, kb, seed));
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

    // The I/O loop on core 0: the line, the node, the client, the flash.
    let mut deframer = Deframer::new();
    // The keyboard's losses as the last `[KB]` line said them.
    let mut kb_said = (0, 0);
    // The faults the screen shows (1493): sticky until a boot proves the
    // flash again; the word is the state, the log has the detail.
    let mut faulted = false;
    let mut next_banner = uptime_ms() + BANNER_INTERVAL_MS;
    loop {
        let now = uptime_ms();
        let mut turn = Turn::default();
        if now >= next_banner {
            next_banner = now + BANNER_INTERVAL_MS;
            log.identify(m9::LOG_PREFIX, m9::BOARD_NAME, bytes);
            log.say_rx_counts(true);
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
                node.packet(&data, uptime_ms(), &mut stores, &mut log);
                continue;
            }
            if let Ok(frame) = envelope::decode_frame(&data) {
                if frame.frame_type == envelope::TYPE_HOME_CODE {
                    // The code written over the control link (1415): the
                    // record before the ack, then the same join as typed.
                    let answer = match envelope::decode_home_code_payload(frame.payload) {
                        None => envelope::encode_refusal(
                            envelope::TYPE_HOME_CODE,
                            envelope::REFUSE_MALFORMED,
                        ),
                        Some(text) => {
                            match node.take_code(text, &mut rng, &mut stores, batch, &mut log) {
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

        turn.host = uptime_ms().saturating_sub(t_host);
        let t_node = uptime_ms();
        node.turn(uptime_ms(), &mut stores, &mut log);
        turn.node = uptime_ms().saturating_sub(t_node);
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
            match message {
                FromUi::Asks { verbs, marks } => {
                    busy = true;
                    let mut ids = || {
                        let mut id = [0u8; 16];
                        rand_core::RngCore::fill_bytes(&mut rng, &mut id);
                        id
                    };
                    node.ask(verbs, &marks, &mut ids, uptime_ms(), &mut stores, &mut log);
                }
                FromUi::Rows { rows, gone, pages } => {
                    node.said(rows, gone, pages, &mut stores, &mut log);
                }
                FromUi::Load(peer) => {
                    busy = true;
                    let want = home::page_entries();
                    if let (Some(address), Some((entries, earlier))) =
                        (node.address(), node.load(peer, want, &mut stores, &mut log))
                    {
                        to_ui(ToUi::Page {
                            peer,
                            entries,
                            earlier,
                            address,
                        });
                    }
                }
                FromUi::Line(prefix, body) => log.line(prefix, format_args!("{body}")),
                FromUi::Raw(body) => log.raw(&body),
                FromUi::Code(text) => {
                    match node.take_code(&text, &mut rng, &mut stores, batch, &mut log) {
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
        let (kb_lost, kb_fails) = critical_section::with(|cs| {
            (
                KB_PRESSES.borrow_ref(cs).lost(),
                KB_FAILS.load(Ordering::Relaxed),
            )
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
            node.quiet(&mut stores, &mut log);
        }

        turn.take = uptime_ms().saturating_sub(t_take);
        turn.say(&mut log, node.entries());
    }
}

/// The last word the UI core was told, so the queue carries changes, not
/// a word per turn.
fn to_ui_word(word: Option<Word>) {
    static LAST: Mutex<RefCell<Option<Option<Word>>>> = Mutex::new(RefCell::new(None));
    critical_section::with(|cs| {
        let mut last = LAST.borrow_ref_mut(cs);
        if last.as_ref() != Some(&word) {
            *last = Some(word);
            TO_UI.borrow_ref_mut(cs).push_back(ToUi::Word(word));
        }
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
) -> ! {
    let mut log = UiLog;
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
    for walked in fixture::SCREENS {
        let budget = walked.budget();
        let (walk_model, walk_row) = walked.walk();
        match census::measure_at(&mut probe, &walk_model, walk_row, &mut renderer, &mut map) {
            Ok(got) => log.line(
                "[CENSUS] ",
                alloc::format!(
                    "{}",
                    census::Line {
                        screen: walked.name(),
                        census: &got,
                        budget: &budget,
                    }
                ),
            ),
            Err(_) => log.line(
                "[CENSUS] ",
                alloc::format!("screen={} draw=failed", walked.name()),
            ),
        }
    }

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

    // The M9's own cursor over a group's sheet or picker (slice six).
    let mut row = 0;
    redraw(
        &mut renderer,
        &mut map,
        &mut panel,
        &model,
        row,
        &shown,
        &mut log,
    );
    // What the panel shows (1491): a page that changed nothing draws
    // nothing.
    let mut drawn = screen::Drawn::default();
    drawn.drawn(&model);

    loop {
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

        // What the I/O core brought.
        for message in ui_drain() {
            match message {
                ToUi::Model(taken) => {
                    model = taken;
                    row = 0;
                    shown.pairing = None;
                    dirty = true;
                }
                ToUi::Entries(entries, address) => {
                    wants.pages += 1;
                    let (_, effects) = home::take(&mut model, &entries, &address);
                    if route_out(&mut model, effects, &mut ids, &mut log, &mut wants) {
                        dirty = true;
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
                    let effects = home::page_in(&mut model, peer, &entries, earlier, &address);
                    route_out(&mut model, effects, &mut ids, &mut log, &mut wants);
                    // The page is on the chat now, whatever it asked.
                    dirty = true;
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
                            &mut log,
                        );
                        log.line("[DUMP] ", alloc::format!("screen={}", walked.name()));
                    }
                    if screen == Some(pair::DUMP_BYTE) {
                        let fresh = Shown {
                            pairing: Some(Pairing::new()),
                            word: None,
                            fault: None,
                        };
                        redraw(
                            &mut renderer,
                            &mut map,
                            &mut panel,
                            &model,
                            0,
                            &fresh,
                            &mut log,
                        );
                        log.line("[DUMP] ", alloc::format!("screen={}", pair::DUMP_NAME));
                    }
                    let dumped = renderer.panel();
                    dump::encode(WIDTH, HEIGHT, |x, y| dumped.pixel(x, y), &mut |line| {
                        log.raw(String::from(line))
                    });
                    dirty = true;
                }
            }
        }

        // The keys the timer read, oldest first (1463): every press,
        // however long the turn that held it was; all of them before the
        // one draw.
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
                let effects = group::press(&mut model, &mut row, &event);
                if route_out(&mut model, effects, &mut ids, &mut log, &mut wants) {
                    frame_whole(&renderer);
                    dirty = true;
                }
            }
        }

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
            log.send(FromUi::Load(peer));
        }

        dirty |= drawn.due(&model);
        if dirty {
            redraw(
                &mut renderer,
                &mut map,
                &mut panel,
                &model,
                row,
                &shown,
                &mut log,
            );
            drawn.drawn(&model);
        }
        core::hint::spin_loop();
    }
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
