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
//! [STORE] sectors=1024 used=<n> entries=<n> cursor=<seq> torn=<n> foreign=<n> restore_ms=<ms>  (1476)
//! [HOME] store=empty cursor=reset was=<n> cursor=0   (1476, a record whose store is gone; or store=behind/ahead)
//! [HOME] page ... taken=<n> of=<m> / pulled ... / joined=yes ... / failed ...   (as the client asks)
//! [HOME] code=typed / code=taken saved=yes / code=refused ...   (1415, a code taken in)
//! [STORE] kept=<n> writes=<n> erased=<n> cursor=<seq> used=<n> entries=<n>   (1476, every page pulled)
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

use alloc::vec::Vec;
use core::cell::RefCell;
use core::sync::atomic::{AtomicU32, Ordering};

use critical_section::Mutex;

use esp_hal::rng::{Trng, TrngSource};
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
use leviculum_ui::input::{KeyCode, KeyEvent, KeyEventKind};
use loquax::home::board::Word;
use loquax::ui::UiModel;
use loquax_m9::census::{self, Probe, Step};
use loquax_m9::field::Field;
use loquax_m9::fixture::{self, Screen};
use loquax_m9::frame::{Frame, HEIGHT, WIDTH};
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

/// The keyboard controller's protocol, as the bench capture of 2026-11-04
/// read it: one key code per press (`thinknode-m9.md`, "The keyboard
/// controller, measured").
const KB: Protocol = kbraw::MEASURED;

/// The keyboard's poll, the interrupt's once the boot handed it over
/// (1463): the controller's bus and the timer that ticks every
/// [`POLL_MS`]. The loop no longer reads the controller; it takes what the
/// interrupt kept in [`KB_PRESSES`], so a key pressed while the loop is
/// busy drawing or storing is read all the same.
struct KbPoll {
    bus: I2c<'static, Blocking>,
    timer: PeriodicTimer<'static, Blocking>,
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
        let mut answer = [0u8; kbraw::MAX_ANSWER];
        let answer = &mut answer[..KB.len];
        let read = match KB.register {
            Some(register) => poll.bus.write_read(KB.address, &[register], answer),
            None => poll.bus.read(KB.address, answer),
        };
        match read {
            Ok(()) => KB_PRESSES
                .borrow_ref_mut(cs)
                .poll(&KB, uptime_ms() as u32, answer),
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
/// stack, from just below the probe's frame down to the guard.
struct BoardProbe {
    before: usize,
    low: usize,
    high: usize,
}

impl Probe for BoardProbe {
    #[inline(never)]
    fn begin(&mut self) {
        leviculum_esp::heap_reset_peak();
        self.before = leviculum_esp::heap_live();
        let here = 0u8;
        let sp = core::ptr::addr_of!(here) as usize;
        let (end, _) = stack_bounds();
        self.low = end + GUARD_SKIP;
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
/// typed, and the home's word in the top bar (1415).
#[derive(Default)]
struct Shown {
    pairing: Option<Pairing>,
    word: Option<Word>,
}

/// Draw what `model` shows, the M9's cursor over a group's rows on `row`,
/// the map in pixels when it is open, or the pairing over all of it, and
/// send the rows that changed to the panel.
fn redraw(
    renderer: &mut Renderer<'_, Frame>,
    map: &mut Map<'_>,
    panel: &mut Panel<'_>,
    model: &UiModel<Field>,
    row: usize,
    shown: &Shown,
    log: &mut UartLog<'_>,
) {
    let look = screen::Look {
        row,
        home: shown.word,
        ..screen::Look::of(model)
    };
    let drawn = match &shown.pairing {
        Some(pairing) => pair::draw(renderer, pairing, shown.word, &look),
        None => renderer.show(model, &look, map),
    };
    if drawn.is_err() {
        log.line("[DISP] ", format_args!("draw=failed"));
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
                format_args!(
                    "flush rows={first}..{last} ms={}.{:03}",
                    us / 1000,
                    us % 1000
                ),
            );
            // The splash's write failed (1485): the frame was whole again,
            // so this flush wrote every row, and the backlight may light.
            if !panel.lit {
                panel.light(true);
                log.line("[DISP] ", format_args!("backlight=on"));
            }
        }
        Err(e) => log.line("[DISP] ", format_args!("flush=failed spi={e:?}")),
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
    let identity = handheld::identity(&mut stores, &mut rng, &mut log);
    let before = leviculum_esp::heap_live();
    let mut node = Handheld::new(identity, node_rng);
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

    // The keyboard controller, probed at both revisions' addresses.
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
    if let Some(bus) = i2c.as_mut() {
        let mut probe = [0u8; 1];
        let v11 = bus.read(m9::KB_ADDR_V11, &mut probe).is_ok();
        let v10 = bus.read(m9::KB_ADDR_V10, &mut probe).is_ok();
        log.line(
            "[KB] ",
            format_args!(
                "addr={:#04x} found={} v10_addr={:#04x} v10_found={} protocol=codes reg={:#04x} len={} poll_ms={POLL_MS}",
                KB.address,
                if v11 { "yes" } else { "no" },
                m9::KB_ADDR_V10,
                if v10 { "yes" } else { "no" },
                KB.register.unwrap_or(0),
                KB.len,
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
                    KB_POLL.borrow_ref_mut(cs).replace(KbPoll { bus, timer });
                });
                log.line(
                    "[KB] ",
                    format_args!("poll=timer every_ms={POLL_MS} queue={}", kbraw::QUEUE),
                );
            }
            Err(e) => log.line("[KB] ", format_args!("state=down reason=timer err={e:?}")),
        }
    }

    // The renderer over the panel's frame, made once; it clears the frame
    // in memory, the panel keeps the splash.
    let before = leviculum_esp::heap_live();
    let Ok(mut renderer) = Renderer::new(&mut frame) else {
        log.line("[DISP] ", format_args!("state=down reason=renderer"));
        halt_with_banner(&mut log, bytes);
    };
    let renderer_held = leviculum_esp::heap_live() - before;
    log.line(
        "[CENSUS] ",
        format_args!(
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
        log.line("[DISP] ", format_args!("state=down reason=region"));
        halt_with_banner(&mut log, bytes);
    };
    let map_held = leviculum_esp::heap_live() - before;
    log.line(
        "[CENSUS] ",
        format_args!(
            "map held={map_held} budget={} verdict={}",
            census::PAINTER_HELD,
            if map_held < census::PAINTER_HELD {
                "ok"
            } else {
                "over"
            }
        ),
    );

    // The census: every screen, built and drawn as the host's census does,
    // against the same budgets. Each draw leaves the screen in the frame;
    // the start screen is drawn fresh after them.
    let mut probe = BoardProbe {
        before: 0,
        low: 0,
        high: 0,
    };
    for shown in fixture::SCREENS {
        let budget = shown.budget();
        let (model, row) = shown.walk();
        match census::measure_at(&mut probe, &model, row, &mut renderer, &mut map) {
            Ok(got) => log.line(
                "[CENSUS] ",
                format_args!(
                    "{}",
                    census::Line {
                        screen: shown.name(),
                        census: &got,
                        budget: &budget,
                    }
                ),
            ),
            Err(_) => log.line(
                "[CENSUS] ",
                format_args!("screen={} draw=failed", shown.name()),
            ),
        }
    }
    // The client session's pull (1363's census, measured here since 1414):
    // thirty 200-byte messages in LoRa pages, and the batch the board runs
    // with on what it measured.
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
    let mut model = restored.unwrap_or_else(fixture::model);
    let mut shown = Shown {
        pairing: node.address().is_none().then(Pairing::new),
        word: node.word(),
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

    let mut deframer = Deframer::new();
    // The keyboard's losses as the last `[KB]` line said them.
    let mut kb_said = (0, 0);
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
                                        model = taken;
                                        row = 0;
                                    }
                                    envelope::encode_ack(envelope::TYPE_HOME_CODE)
                                }
                                Err(CodeRefusal::Value) => envelope::encode_refusal(
                                    envelope::TYPE_HOME_CODE,
                                    envelope::REFUSE_VALUE,
                                ),
                                Err(CodeRefusal::Persist) => envelope::encode_refusal(
                                    envelope::TYPE_HOME_CODE,
                                    envelope::REFUSE_PERSIST,
                                ),
                            }
                        }
                    };
                    log.frame(&answer);
                    if node.address().is_some() {
                        shown.pairing = None;
                        shown.word = node.word();
                        redraw(
                            &mut renderer,
                            &mut map,
                            &mut panel,
                            &model,
                            row,
                            &shown,
                            &mut log,
                        );
                    }
                    continue;
                }
            }
            control(
                &data,
                &mut model,
                &mut row,
                &mut renderer,
                &mut map,
                &mut panel,
                &mut log,
            );
        }

        turn.host = uptime_ms().saturating_sub(t_host);
        let t_node = uptime_ms();
        node.turn(uptime_ms(), &mut stores, &mut log);
        turn.node = uptime_ms().saturating_sub(t_node);
        let t_take = uptime_ms();

        // What the client brought: pulled entries into the model, ended
        // sends to their bubbles, and the top bar's word when it moved.
        let drained = node.drain();
        let mut dirty = false;
        if let Some(address) = node.address() {
            if !drained.entries.is_empty() {
                home::take(&mut model, &drained.entries, &address);
                dirty = true;
            }
            for result in drained.sent {
                home::sent(&mut model, result);
                dirty = true;
            }
        }
        let word = node.word();
        if word != shown.word {
            shown.word = word;
            dirty = true;
        }
        if dirty {
            let t_draw = uptime_ms();
            redraw(
                &mut renderer,
                &mut map,
                &mut panel,
                &model,
                row,
                &shown,
                &mut log,
            );
            turn.draw = uptime_ms().saturating_sub(t_draw);
        }
        turn.take = uptime_ms().saturating_sub(t_take).saturating_sub(turn.draw);

        // The keys the timer read since the last turn (1463), oldest
        // first: every press, however long the turn that held it was.
        let t_keys = uptime_ms();
        let (presses, kb_lost, kb_fails) = kb_drain();
        if (kb_lost, kb_fails) != kb_said {
            kb_said = (kb_lost, kb_fails);
            log.line(
                "[KB] ",
                format_args!("queue_lost={kb_lost} read_failed={kb_fails}"),
            );
        }
        for press in presses {
            let ms = press.ms;
            log.raw(&kbraw::raw_line(ms, press.answer()));
            let mut events = Vec::new();
            let mut command = None;
            match KB.read(press.answer()) {
                Read::Idle => {}
                Read::Mixed => log.line("[KB] ", format_args!("answer=mixed")),
                Read::Code(code) => {
                    if let Some(key) = Key::of(code) {
                        log.raw(&keyboard::key_line(ms, code, key));
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
                redraw(
                    &mut renderer,
                    &mut map,
                    &mut panel,
                    &model,
                    row,
                    &shown,
                    &mut log,
                );
            }
            if !events.is_empty() {
                for event in events {
                    if let Some(pairing) = shown.pairing.as_mut() {
                        let taken = pairing_key(
                            pairing,
                            &event,
                            &mut node,
                            &mut rng,
                            &mut stores,
                            batch,
                            &mut log,
                        );
                        if event.code == KeyCode::Esc && event.kind != KeyEventKind::Release {
                            shown.pairing = None;
                        }
                        if let Some(taken) = taken {
                            // The code was taken: the home's chats from
                            // here (what the store kept of that home),
                            // the join in the top bar.
                            model = taken;
                            row = 0;
                            shown.pairing = None;
                        }
                        shown.word = node.word();
                        continue;
                    }
                    let effects = group::press(&mut model, &mut row, &event);
                    if node.address().is_some() {
                        let mut ids = || {
                            let mut id = [0u8; 16];
                            rand_core::RngCore::fill_bytes(&mut rng, &mut id);
                            id
                        };
                        node.send(home::sends(effects, &mut ids), uptime_ms());
                    }
                }
                redraw(
                    &mut renderer,
                    &mut map,
                    &mut panel,
                    &model,
                    row,
                    &shown,
                    &mut log,
                );
            }
        }
        turn.keys = uptime_ms().saturating_sub(t_keys);
        turn.say(&mut log, node.entries());
    }
}

/// One key on the pairing screen: the code edited, and on Enter with a
/// whole code that checks, the code taken as `lnflash --home` would give
/// it; the model the board shows from then on.
fn pairing_key(
    pairing: &mut Pairing,
    event: &KeyEvent,
    node: &mut Handheld,
    rng: &mut Trng,
    stores: &mut Stores<'_>,
    batch: loquax::home::board::Batch,
    log: &mut UartLog<'_>,
) -> Option<UiModel<Field>> {
    let code = pairing.key(event)?;
    log.line("[HOME] ", format_args!("code=typed"));
    // Refused only when flash would not take it: the typed code checked.
    node.take_code(&code.encode(), rng, stores, batch, log)
        .ok()
        .flatten()
}

/// One control frame from the host: the envelope's capability query and
/// `FRAME_DUMP`; everything else envelope-shaped is refused by name.
/// Frames that are not the envelope are the node's and never come here.
fn control(
    data: &[u8],
    model: &mut UiModel<Field>,
    row: &mut usize,
    renderer: &mut Renderer<'_, Frame>,
    map: &mut Map<'_>,
    panel: &mut Panel<'_>,
    log: &mut UartLog<'_>,
) {
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
        envelope::TYPE_FRAME_DUMP => match envelope::decode_frame_dump_payload(frame.payload) {
            None => log.frame(&envelope::encode_refusal(
                envelope::TYPE_FRAME_DUMP,
                envelope::REFUSE_MALFORMED,
            )),
            Some(Some(byte)) if Screen::of(byte).is_none() && byte != pair::DUMP_BYTE => log.frame(
                &envelope::encode_refusal(envelope::TYPE_FRAME_DUMP, envelope::REFUSE_VALUE),
            ),
            Some(screen) => {
                log.frame(&envelope::encode_ack(envelope::TYPE_FRAME_DUMP));
                if let Some(shown) = screen.and_then(Screen::of) {
                    (*model, *row) = shown.walk();
                    redraw(renderer, map, panel, model, *row, &Shown::default(), log);
                    log.line("[DUMP] ", format_args!("screen={}", shown.name()));
                }
                // The pairing a board without a record opens on (1440),
                // over the model as it is.
                if screen == Some(pair::DUMP_BYTE) {
                    let fresh = Shown {
                        pairing: Some(Pairing::new()),
                        word: None,
                    };
                    redraw(renderer, map, panel, model, 0, &fresh, log);
                    log.line("[DUMP] ", format_args!("screen={}", pair::DUMP_NAME));
                }
                let frame = renderer.panel();
                dump::encode(WIDTH, HEIGHT, |x, y| frame.pixel(x, y), &mut |line| {
                    log.raw(line)
                });
            }
        },
        other => {
            let reason = match other {
                envelope::TYPE_CAPABILITIES => envelope::REFUSE_MALFORMED,
                _ => envelope::REFUSE_UNKNOWN_TYPE,
            };
            log.frame(&envelope::encode_refusal(other, reason));
        }
    }
}
