//! Where each core was when the chip last stopped (1534): a record in RTC
//! slow memory that every turn of both loops writes its stage into, a
//! hardware watchdog per core that resets a chip whose loop stopped, and
//! the line the next boot says it with:
//!
//! ```text
//! [PREV_RUN] reason=<wdt0|wdt1|panic|power|sw|usb|other:0x..> core0_stage=.. core0_lock=.. core0_isr=.. core1_stage=.. core1_lock=.. peer=<8 hex|none> turns=<c0>/<c1> [panic_core=<n> at=<file>:<line>]
//! ```
//!
//! The M9 hung whole on 2026-11-07 (Lew opened a chat, both cores went
//! silent, nothing said where); this is what would have said it.
//!
//! **What survives what.** RTC slow memory keeps its bytes across a
//! watchdog's system reset and a software reset, which reset the digital
//! core and leave the RTC domain alone; esp-hal zeroes the
//! `.rtc_slow.persistent` section only on a power-on reset
//! (`esp-hal-1.2.1/src/soc/mod.rs`, `__init_persistent`). A power cycle,
//! and the EN line espflash or lnsd toggle over the USB bridge, are
//! power-on resets: they wipe the record, and `[PREV_RUN]` then says
//! `reason=power record=none`. To read a hang, let the watchdog reset the
//! chip (5 s) instead of resetting it by hand.
//!
//! **The watchdogs.** The two timer groups' main watchdogs (MWDT0 in TG0,
//! MWDT1 in TG1), one per core's loop, each fed once per turn of its
//! loop, stage 0 resetting the system after [`TIMEOUT_MS`]. They are
//! hardware counters on the APB clock: a core spinning with its
//! interrupts off on a lock it will never get (the hang's hypothesis) is
//! exactly what they see, so no separate interrupt watchdog is needed; a
//! core whose loop stops for any reason stops feeding its own. The reset
//! reason then names which one fired (`CoreMwdt0`/`CoreMwdt1`), which is
//! which core's loop stopped first.
//!
//! **Cost.** Every write is one 32-bit volatile store to RTC slow memory
//! (`0x5000_0000`, both cores reach it); the M9's core-0 loop does about
//! a dozen per turn, core 1's about eight. Not timed on the board.

use core::ptr::{addr_of, addr_of_mut};

/// How long a loop may go without feeding its watchdog.
pub const TIMEOUT_MS: u64 = 5_000;

/// The record's words.
const WORDS: usize = 24;

/// What [`MAGIC`] marks: a record this image's layout wrote.
const MAGIC: u32 = 0x5354_4c31; // "STL1"

// The record's layout, word by word.
const W_MAGIC: usize = 0;
const W_STAGE: usize = 1; // + core
const W_LOCK: usize = 3; // + core
const W_TURNS: usize = 5; // + core
const W_ISR: usize = 7; // core 0's interrupts
const W_PEER: usize = 8; // four words
const W_PANIC: usize = 12; // 0 none, 1 + the core that panicked
const W_PANIC_LINE: usize = 13;
const W_PANIC_FILE: usize = 14; // ten words, the file's tail
const FILE_BYTES: usize = 4 * (WORDS - W_PANIC_FILE);

#[esp_hal::ram(unstable(rtc_slow, persistent))]
static mut RECORD: [u32; WORDS] = [0; WORDS];

fn put(word: usize, value: u32) {
    if word < WORDS {
        // SAFETY: an aligned word inside the static; every access is
        // volatile and word-sized, and each word has one writer at a time
        // (a core writes its own stage, lock and turns; core 0 writes the
        // peer and the interrupt word; the panic words are written by the
        // core that panics). A torn read across words is a record that
        // mixes two turns, never an unsound access.
        unsafe { core::ptr::write_volatile(addr_of_mut!(RECORD[word]), value) }
    }
}

fn get(word: usize) -> u32 {
    if word < WORDS {
        // SAFETY: as in `put`; read only.
        unsafe { core::ptr::read_volatile(addr_of!(RECORD[word])) }
    } else {
        0
    }
}

/// Which loop a write is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Core {
    /// Core 0 (PRO): the line, the node, the client, the flash.
    Io = 0,
    /// Core 1 (APP): the model, the scene, the panel.
    Ui = 1,
}

/// A stage of a loop. The numbers are the record's; the names are the
/// line's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Stage {
    /// Not yet in a loop.
    Boot = 1,
    /// Core 0: reading the host's bytes and the control frames.
    Host = 2,
    /// Core 0: a packet into the node.
    Packet = 3,
    /// Core 0: the node's and the client's turn.
    Node = 4,
    /// Core 0: what the client brought, over to the UI.
    Take = 5,
    /// Core 0: a fold's asks into the queue and the store.
    Asks = 6,
    /// Core 0: the changed rows into the summary log.
    Rows = 7,
    /// Core 0: a conversation's page read for the UI.
    Load = 8,
    /// Core 0: a UI line out on the UART.
    Line = 9,
    /// Core 0: a typed home code.
    Code = 10,
    /// Core 0: the quiet turn's erase ahead.
    Quiet = 11,
    /// Core 0: the keyboard's loss counts.
    KbCount = 12,
    /// Core 0: waiting for its next interrupt (1507, `idle`).
    IoIdle = 13,
    /// Core 0: the bench's screen dump written out (1603): apart from
    /// the line it shared before, as `[LOOP]`'s `dump=` is apart from
    /// `take=`.
    Dump = 14,
    /// Core 0: the LR1110's task polled (1478b): its boot probe, a bus
    /// transaction on SPI2 between the panel's chunks.
    Radio = 15,
    /// Core 1: the I/O core's messages folded.
    Drain = 20,
    /// Core 1: the key presses folded.
    Keys = 21,
    /// Core 1: the scene built and drawn, the panel flushed.
    Draw = 22,
    /// Core 1: the fold's wants handed to core 0.
    Send = 23,
    /// Core 1: the census at boot.
    Census = 24,
    /// Core 1: waiting for the panel's next frame or its tick (1507).
    UiIdle = 25,
    /// Core 1: waiting for the last flush to reach the panel before the
    /// frame is drawn into (1563): apart from the draw, so a record says
    /// which of the two held the core.
    Flush = 26,
}

impl Stage {
    fn name(raw: u32) -> &'static str {
        match raw {
            0 => "none",
            1 => "boot",
            2 => "host",
            3 => "packet",
            4 => "node",
            5 => "take",
            6 => "asks",
            7 => "rows",
            8 => "load",
            9 => "line",
            10 => "code",
            11 => "quiet",
            12 => "kb-count",
            13 => "idle",
            14 => "dump",
            15 => "radio",
            20 => "drain",
            21 => "keys",
            22 => "draw",
            23 => "send",
            24 => "census",
            25 => "idle",
            26 => "flush",
            _ => "?",
        }
    }
}

/// A lock a core is about to take or holds, as far as the record names
/// it: the queues, the key queue, and the flash op (which parks the other
/// core). The heap's lock is the same cross-core critical section but is
/// not named: it is taken too often to write a word for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Lock {
    /// No lock named.
    None = 0,
    /// `TO_UI`, the I/O core's queue to the UI.
    ToUi = 1,
    /// `FROM_UI`, the UI core's queue to the I/O core.
    FromUi = 2,
    /// The key queue.
    Keys = 3,
    /// A flash write or erase: the other core is parked for it.
    Flash = 4,
    /// The last word told the UI.
    Word = 5,
}

impl Lock {
    fn name(raw: u32) -> &'static str {
        match raw {
            0 => "none",
            1 => "to-ui",
            2 => "from-ui",
            3 => "keys",
            4 => "flash",
            5 => "word",
            _ => "?",
        }
    }
}

/// An interrupt on core 0, as the record names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Isr {
    /// None running.
    None = 0,
    /// The keyboard's 10 ms poll.
    Keyboard = 1,
    /// The UART's receive FIFO.
    Uart = 2,
}

impl Isr {
    fn name(raw: u32) -> &'static str {
        match raw {
            0 => "none",
            1 => "kb",
            2 => "uart",
            _ => "?",
        }
    }
}

/// `core` entered `stage`.
pub fn stage(core: Core, stage: Stage) {
    put(W_STAGE + core as usize, stage as u32);
}

/// `core` is about to take `lock`, or ([`Lock::None`]) let it go.
pub fn lock(core: Core, lock: Lock) {
    put(W_LOCK + core as usize, lock as u32);
}

/// Run `f` with `lock` named for `core`.
pub fn locked<R>(core: Core, which: Lock, f: impl FnOnce() -> R) -> R {
    lock(core, which);
    let result = f();
    lock(core, Lock::None);
    result
}

/// Core 0 entered (or, [`Isr::None`], left) an interrupt handler. The
/// interrupts nest only by priority, and both of these run at the same
/// one, so one word holds it.
pub fn isr(which: Isr) {
    put(W_ISR, which as u32);
}

/// `core` finished a turn of its loop.
pub fn turned(core: Core) {
    let word = W_TURNS + core as usize;
    put(word, get(word).wrapping_add(1));
}

/// The conversation core 0 is reading a page of.
pub fn peer(peer: &[u8; 16]) {
    for (i, chunk) in peer.chunks_exact(4).enumerate() {
        let mut word = [0u8; 4];
        word.copy_from_slice(chunk);
        put(W_PEER + i, u32::from_be_bytes(word));
    }
}

/// A panic on the current core at `file:line`: written before the
/// message is said, so a panic whose line never reaches the UART still
/// leaves its site.
pub fn panicked(file: &str, line: u32) {
    let core = esp_hal::system::Cpu::current() as u32;
    put(W_PANIC, 1 + core);
    put(W_PANIC_LINE, line);
    let bytes = file.as_bytes();
    let tail = &bytes[bytes.len().saturating_sub(FILE_BYTES)..];
    for i in 0..FILE_BYTES / 4 {
        let mut word = [0u8; 4];
        for (j, byte) in word.iter_mut().enumerate() {
            *byte = tail.get(i * 4 + j).copied().unwrap_or(0);
        }
        put(W_PANIC_FILE + i, u32::from_le_bytes(word));
    }
}

/// What the last run left, read once at boot before [`begin`] clears it.
pub struct PrevRun {
    reason: &'static str,
    raw_reason: u32,
    words: [u32; WORDS],
}

/// The record as the last run left it, with the reset's reason.
pub fn previous() -> PrevRun {
    use esp_hal::rtc_cntl::SocResetReason as R;
    let reset = esp_hal::system::reset_reason();
    let raw_reason = reset.map_or(0, |r| r as u32);
    let mut words = [0u32; WORDS];
    for (i, word) in words.iter_mut().enumerate() {
        *word = get(i);
    }
    let reason = if words[W_MAGIC] == MAGIC && words[W_PANIC] != 0 {
        "panic"
    } else {
        match reset {
            Some(R::CoreMwdt0 | R::CpuMwdt0) => "wdt0",
            Some(R::CoreMwdt1 | R::CpuMwdt1) => "wdt1",
            Some(R::CoreRtcWdt | R::CpuRtcWdt | R::SysRtcWdt | R::SysSuperWdt) => "rtc-wdt",
            Some(R::ChipPowerOn) => "power",
            Some(R::CoreSw | R::CpuSw) => "sw",
            Some(R::CoreUsbUart | R::CoreUsbJtag) => "usb",
            Some(R::SysBrownOut) => "brownout",
            _ => "other",
        }
    };
    PrevRun {
        reason,
        raw_reason,
        words,
    }
}

impl core::fmt::Display for PrevRun {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let w = &self.words;
        write!(f, "reason={}", self.reason)?;
        if self.reason == "other" {
            write!(f, ":{:#04x}", self.raw_reason)?;
        }
        if w[W_MAGIC] != MAGIC {
            return f.write_str(" record=none");
        }
        write!(
            f,
            " core0_stage={} core0_lock={} core0_isr={} core1_stage={} core1_lock={}",
            Stage::name(w[W_STAGE]),
            Lock::name(w[W_LOCK]),
            Isr::name(w[W_ISR]),
            Stage::name(w[W_STAGE + 1]),
            Lock::name(w[W_LOCK + 1]),
        )?;
        f.write_str(" peer=")?;
        if w[W_PEER..W_PEER + 4].iter().all(|word| *word == 0) {
            f.write_str("none")?;
        } else {
            // The first four bytes, as the other lines shorten an address.
            write!(f, "{:08x}", w[W_PEER])?;
        }
        write!(f, " turns={}/{}", w[W_TURNS], w[W_TURNS + 1])?;
        if w[W_PANIC] != 0 {
            let mut file = [0u8; FILE_BYTES];
            for i in 0..FILE_BYTES / 4 {
                file[i * 4..i * 4 + 4].copy_from_slice(&w[W_PANIC_FILE + i].to_le_bytes());
            }
            let len = file.iter().position(|b| *b == 0).unwrap_or(FILE_BYTES);
            let file = core::str::from_utf8(&file[..len]).unwrap_or("?");
            write!(
                f,
                " panic_core={} at={file}:{}",
                w[W_PANIC] - 1,
                w[W_PANIC_LINE]
            )?;
        }
        Ok(())
    }
}

/// Start this run's record: everything zero, the magic set, both cores at
/// [`Stage::Boot`]. After [`previous`].
pub fn begin() {
    for word in 0..WORDS {
        put(word, 0);
    }
    stage(Core::Io, Stage::Boot);
    stage(Core::Ui, Stage::Boot);
    put(W_MAGIC, MAGIC);
}

/// A loop's watchdog: a timer group's MWDT, armed with stage 0 resetting
/// the system after [`TIMEOUT_MS`], fed once per turn, and from inside a
/// long store operation through a shared borrow (1521c, [`Watchdog::feed`]).
pub struct Watchdog<TG: esp_hal::timer::timg::TimerGroupInstance> {
    wdt: core::cell::RefCell<esp_hal::timer::timg::Wdt<TG>>,
}

impl<TG: esp_hal::timer::timg::TimerGroupInstance> Watchdog<TG> {
    /// Arm `wdt`: from here on the loop must [`Watchdog::feed`] it at
    /// least every [`TIMEOUT_MS`].
    pub fn arm(mut wdt: esp_hal::timer::timg::Wdt<TG>) -> Self {
        use esp_hal::timer::timg::MwdtStage;
        wdt.set_timeout(
            MwdtStage::Stage0,
            esp_hal::time::Duration::from_millis(TIMEOUT_MS),
        );
        // `enable` sets stage 0 to reset the system, the others off
        // (`esp-hal-1.2.1/src/timer/timg.rs`, `set_wdt_enabled`).
        wdt.enable();
        wdt.feed();
        Self {
            wdt: core::cell::RefCell::new(wdt),
        }
    }

    /// The loop is alive. A shared borrow (1521c): a store operation
    /// longer than [`TIMEOUT_MS`] (a restore that folds the whole ring
    /// after a code was taken) feeds it from its clock, a `Fn` hook.
    pub fn feed(&self) {
        self.wdt.borrow_mut().feed();
    }
}
