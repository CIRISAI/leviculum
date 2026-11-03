//! Loquax on the ThinkNode M9: the first image that boots the renderer
//! (order 1361, slice four of `docs/src/concepts/handheld-messenger.md`).
//!
//! What it does, and it is the whole of what this step claims: bring the
//! SoC up, map the 8 MB PSRAM and give it to the heap, light the ST7789,
//! draw the chats list of a fixture model (`loquax_m9::fixture`, no
//! network) with the renderer the host pins its goldens with, and let the
//! keyboard move through it: open a chat, scroll, type, go back.
//!
//! **Nothing is transmitted.** The LR1110's chip select is parked high so
//! it ignores the panel's traffic on the shared bus; no opcode reaches it.
//!
//! Over the debug UART (the CH340K, 115200 8N1), in boot order:
//!
//! ```text
//! [FW_BUILD] git_sha=<sha> dirty=<b> version=<v> t=<ms>
//! [BOARD] id=M9 name=Elecrow ThinkNode M9 heap=<psram bytes> t=<ms>
//! [HEAP] region=psram start=<addr> bytes=<n> t=<ms>
//! [STACK] bytes=<n> t=<ms>
//! [DISP] ST7789 320x240 madctl=0xb0 spi_mhz=40 state=up t=<ms>
//! [KB] addr=0x6d found=yes ... protocol=provisional reg=0x01 len=8 t=<ms>
//! [CENSUS] renderer held=<n> budget=220000 verdict=ok t=<ms>
//! [CENSUS] screen=chats scene_held=... verdict=ok t=<ms>
//! [CENSUS] screen=chat scene_held=... verdict=ok t=<ms>
//! [DISP] flush rows=0..239 ms=<ms> t=<ms>
//! KBRAW <ms> <hex>          one per poll, every 10 ms
//! KB <ms> <14 hex>          one per poll, the scan the table made of it
//! ```
//!
//! and the banner pair every five seconds. A `FRAME_DUMP` control frame
//! (`7e a4 a5 18 00 00 7e`, or `7e a4 a5 18 00 01 <screen> 7e`) is acked
//! and answered with the `PBM` dump of `loquax_m9::dump`.
//!
//! The pin handles are moved out of `Peripherals` through the board
//! module's type aliases, as `heltec_v4.rs` does: a compile-time check that
//! this binary and the board file agree about every net.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

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
    time::Rate,
    uart::{Config as UartConfig, Uart},
    Blocking,
};
use leviculum_core::envelope::{self, EnvelopeError};
use leviculum_core::framing::hdlc::{DeframeResult, Deframer};
use leviculum_esp::{boards::thinknode_m9 as m9, uptime_ms, UartLog};
use loquax::ui::{update, UiEvent, UiModel};
use loquax_m9::census::{self, Probe, Step};
use loquax_m9::field::Field;
use loquax_m9::fixture::{self, Screen};
use loquax_m9::frame::{Frame, HEIGHT, WIDTH};
use loquax_m9::kbraw::{self, Protocol, MAX_ANSWER};
use loquax_m9::keyboard::{Keyboard, POLL_MS};
use loquax_m9::render::Renderer;
use loquax_m9::{dump, screen};

esp_bootloader_esp_idf::esp_app_desc!();

/// How often the board re-states what it is: the five seconds every
/// leviculum board uses, so a reader that attaches late still reads it.
const BANNER_INTERVAL_MS: u64 = 5_000;

/// The least PSRAM the UI heap wants: the plan's 1.1 MB, model, scene,
/// renderer and two frames on the 64-bit host
/// (`handheld-messenger.md`, "Memory budgets, measured").
const HEAP_MIN: usize = 1_100_000 + (1 << 20);

/// The keyboard controller's protocol until a capture reads it.
const KB: Protocol = kbraw::PROVISIONAL;

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

/// The panel on SPI2 with DMA, its chip select and data/command line.
struct Panel<'d> {
    spi: SpiDma<'d, Blocking>,
    cs: Output<'d>,
    dc: Output<'d>,
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

/// Draw what `model` shows and send the rows that changed to the panel.
fn redraw(
    renderer: &mut Renderer<'_, Frame>,
    panel: &mut Panel<'_>,
    model: &UiModel<Field>,
    log: &mut UartLog<'_>,
) {
    if renderer
        .draw(&screen::scene(model), &screen::Look::of(model))
        .is_err()
    {
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
        Ok(()) => log.line(
            "[DISP] ",
            format_args!(
                "flush rows={first}..{last} ms={}.{:03}",
                us / 1000,
                us % 1000
            ),
        ),
        Err(e) => log.line("[DISP] ", format_args!("flush=failed spi={e:?}")),
    }
}

#[main]
fn main() -> ! {
    let p = leviculum_esp::init_soc();

    // The log first: every later failure says itself here.
    let tx: m9::UartTx = p.GPIO43;
    let rx: m9::UartRx = p.GPIO44;
    let Ok(uart) = Uart::new(p.UART0, UartConfig::default().with_baudrate(m9::UART_BAUD)) else {
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
    let mut backlight = Output::new(lcd_backlight, Level::High, OutputConfig::default());
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
    let mut panel = Panel { spi, cs, dc };
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
    backlight.set_low();

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
                "addr={:#04x} found={} v10_addr={:#04x} v10_found={} protocol=provisional reg={:#04x} len={} poll_ms={POLL_MS}",
                KB.address,
                if v11 { "yes" } else { "no" },
                m9::KB_ADDR_V10,
                if v10 { "yes" } else { "no" },
                KB.register.unwrap_or(0),
                KB.len,
            ),
        );
    }
    let layout = kbraw::provisional_layout();
    let mut keyboard = Keyboard::new(&layout);

    // The renderer over the panel's frame, made once.
    let mut frame = Frame::new();
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

    // The census: both screens, built and drawn as the host's census does,
    // against the same budgets. Each draw leaves the screen in the frame;
    // the start screen is drawn fresh after them.
    let mut probe = BoardProbe {
        before: 0,
        low: 0,
        high: 0,
    };
    for (shown, budget) in [(Screen::Chats, census::CHATS), (Screen::Chat, census::CHAT)] {
        let model = shown.model();
        match census::measure(&mut probe, &model, &mut renderer) {
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
    let mut model = fixture::model();
    redraw(&mut renderer, &mut panel, &model, &mut log);

    let mut deframer = Deframer::new();
    let mut answer = [0u8; MAX_ANSWER];
    let mut i2c_failing = false;
    let mut next_banner = uptime_ms() + BANNER_INTERVAL_MS;
    let mut next_poll = uptime_ms();
    loop {
        let now = uptime_ms();
        if now >= next_banner {
            next_banner = now + BANNER_INTERVAL_MS;
            log.identify(m9::LOG_PREFIX, m9::BOARD_NAME, bytes);
        }

        // The host: control frames between the lines.
        let mut incoming = [0u8; 64];
        let n = log.read_ready(&mut incoming);
        let mut requests = Vec::new();
        for result in deframer.process(&incoming[..n]) {
            if let DeframeResult::Frame(data) = result {
                requests.push(data);
            }
        }
        for data in requests {
            control(&data, &mut model, &mut renderer, &mut panel, &mut log);
        }

        if now < next_poll {
            continue;
        }
        next_poll = now + u64::from(POLL_MS);
        let Some(bus) = i2c.as_mut() else {
            continue;
        };
        let ms = now as u32;
        let read = match KB.register {
            Some(register) => bus.write_read(KB.address, &[register], &mut answer[..KB.len]),
            None => bus.read(KB.address, &mut answer[..KB.len]),
        };
        match read {
            Ok(()) => {
                if i2c_failing {
                    i2c_failing = false;
                    log.line("[KB] ", format_args!("read=ok again"));
                }
                log.raw(&kbraw::raw_line(ms, &answer[..KB.len]));
                let scan = KB.scan(&answer[..KB.len]);
                log.raw(&kbraw::scan_line(ms, scan));
                let mut events = Vec::new();
                keyboard.poll(ms, scan, &mut |event| events.push(event));
                if !events.is_empty() {
                    for event in events {
                        update(&mut model, UiEvent::Key(event));
                    }
                    redraw(&mut renderer, &mut panel, &model, &mut log);
                }
            }
            Err(e) => {
                // Said once per run of failures, not once per poll.
                if !i2c_failing {
                    i2c_failing = true;
                    log.line("[KB] ", format_args!("read=failed err={e:?}"));
                }
            }
        }
    }
}

/// One control frame from the host: the envelope's capability query and
/// `FRAME_DUMP`; everything else envelope-shaped is refused by name, and
/// anything else (this image carries no transport) is dropped.
fn control(
    data: &[u8],
    model: &mut UiModel<Field>,
    renderer: &mut Renderer<'_, Frame>,
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
            log.frame(&envelope::encode_capability_report(&[
                envelope::TYPE_CAPABILITIES,
                envelope::TYPE_FRAME_DUMP,
            ]));
        }
        envelope::TYPE_FRAME_DUMP => match envelope::decode_frame_dump_payload(frame.payload) {
            None => log.frame(&envelope::encode_refusal(
                envelope::TYPE_FRAME_DUMP,
                envelope::REFUSE_MALFORMED,
            )),
            Some(Some(byte)) if Screen::of(byte).is_none() => log.frame(&envelope::encode_refusal(
                envelope::TYPE_FRAME_DUMP,
                envelope::REFUSE_VALUE,
            )),
            Some(screen) => {
                log.frame(&envelope::encode_ack(envelope::TYPE_FRAME_DUMP));
                if let Some(shown) = screen.and_then(Screen::of) {
                    *model = shown.model();
                    redraw(renderer, panel, model, log);
                    log.line("[DUMP] ", format_args!("screen={}", shown.name()));
                }
                let shown = renderer.panel();
                dump::encode(WIDTH, HEIGHT, |x, y| shown.pixel(x, y), &mut |line| {
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
