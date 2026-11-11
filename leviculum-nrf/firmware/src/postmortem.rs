//! The post-mortem store (#65, #469 step 1): what a panic or a HardFault
//! leaves in retained RAM for the next boot to replay.
//!
//! Three records sit in the `.retained` link section: the panic counter,
//! the panic message and the HardFault register snapshot. The section is
//! the seam: a bind crate's linker script places it in RAM its reset leaves
//! alone (on the nRF52840 `memory.x`'s `RETAINED`, below the stack and below
//! every address the Adafruit bootloader touches, [`crate::boot_trace`]'s
//! `BOOT_TRACE` for the full story). The handlers themselves are the
//! bind crate's, because catching a fault and resetting the core are an
//! architecture's: they call [`bump_panic_count`] and then [`record_panic`]
//! or [`record_hardfault`], and reset. Everything a boot or the debug-port
//! query reads back, and the lines it prints, is here.
//!
//! The decisions inside (which magic a read accepts, the message ring's
//! arithmetic) are `leviculum_boot_trace::postmortem`, host-tested.

use leviculum_boot_trace::postmortem::{self, RecordMagic};

use crate::log;

/// Persistent panic counter (Codeberg #65): lives in `.retained` next to
/// the post-mortems and — unlike them — is NOT cleared by the boot-time
/// read, so it accumulates across panic/HardFault soft-reset cycles
/// until power loss. Cold boots start at 0 via the magic check.
#[repr(C)]
struct PanicCountRaw {
    magic: u32,
    count: u32,
}

const PANIC_COUNT_MAGIC: u32 = 0xC01D_FACE;

#[link_section = ".retained"]
static mut PANIC_COUNT: core::mem::MaybeUninit<PanicCountRaw> = core::mem::MaybeUninit::uninit();

/// Increment the persistent panic counter and return the new value.
/// Called on every panic and HardFault path before the soft-reset, by the
/// bind crate's handlers. Volatile raw-pointer access only — runs in fault
/// context.
pub fn bump_panic_count() -> u32 {
    unsafe {
        let p = core::ptr::addr_of_mut!(PANIC_COUNT).cast::<PanicCountRaw>();
        let magic = core::ptr::read_volatile(core::ptr::addr_of!((*p).magic));
        let prev = if magic == PANIC_COUNT_MAGIC {
            core::ptr::read_volatile(core::ptr::addr_of!((*p).count))
        } else {
            0
        };
        let next = prev.wrapping_add(1);
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).count), next);
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).magic), PANIC_COUNT_MAGIC);
        next
    }
}

/// Read the persistent panic counter WITHOUT clearing it (boot banner,
/// debug captures). 0 on cold boot or never-panicked.
pub fn panic_count() -> u32 {
    unsafe {
        let p = core::ptr::addr_of!(PANIC_COUNT).cast::<PanicCountRaw>();
        if core::ptr::read_volatile(core::ptr::addr_of!((*p).magic)) == PANIC_COUNT_MAGIC {
            core::ptr::read_volatile(core::ptr::addr_of!((*p).count))
        } else {
            0
        }
    }
}

/// Maximum bytes of panic message preserved across the post-mortem soft-reset.
/// Bumped to 1024 (was 256) so the SoftDevice's "memory access violation"
/// message — which encodes PC + PREGION at the END of the string — survives
/// the buffer cap. The 256-char limit was cutting off precisely the
/// diagnostic data we need to pinpoint the offending instruction. The value
/// and the ring arithmetic around it are host-tested in
/// `leviculum_boot_trace::postmortem`.
pub const PANIC_MSG_MAX: usize = leviculum_boot_trace::postmortem::PANIC_MSG_MAX;

/// Snapshot returned by `take_panic_postmortem()`.
#[derive(Clone, Copy)]
pub struct PanicPostMortem {
    /// Bytes valid in `bytes` (== `total` unless the message overflowed
    /// the buffer, in which case these are the LAST `PANIC_MSG_MAX`
    /// bytes of it).
    pub len: usize,
    /// Total bytes the panic formatter produced. `total > len` means
    /// the front of the message was dropped — never the tail, where SD
    /// fault panics carry PC/PREGION.
    pub total: usize,
    pub bytes: [u8; PANIC_MSG_MAX],
}

#[repr(C)]
struct PanicPmRaw {
    magic: u32,
    len: u32,
    bytes: [u8; PANIC_MSG_MAX],
}

/// Record version, encoded in the magic. `.uninit` survives a reflash,
/// so a record written by an older image is still there on the first
/// boot of a new one. `len` used to mean "bytes stored, clamped"; it now
/// means "total bytes produced", with `bytes` a ring keeping the last
/// `PANIC_MSG_MAX` of them — the same word read the old way linearises
/// an overlong message at the wrong offset. Bump this whenever the
/// record's layout or the meaning of a field changes, so stale records
/// are rejected instead of misparsed.
///
/// The fresh magic has a twin for a record already replayed once by a
/// boot: `take_panic_postmortem` downgrades a fresh record to it instead of
/// erasing it, so the debug-port query ([`postmortem_query`]) can still
/// retrieve the evidence after the boot replay was lost to log-ring
/// lapping. The next panic overwrites the record with a fresh magic; only
/// power loss or a reflash wipes it. Both, and which read accepts which,
/// are `leviculum_boot_trace::postmortem::PANIC_RECORD`.
const PANIC_PM: RecordMagic = postmortem::PANIC_RECORD;

#[link_section = ".retained"]
static mut PANIC_PM_RECORD: core::mem::MaybeUninit<PanicPmRaw> = core::mem::MaybeUninit::uninit();

/// Decode the ring-stored record at `p`. Caller has validated the magic.
///
/// The panic handler writes the buffer as a ring keeping the LAST
/// `PANIC_MSG_MAX` bytes; `postmortem::ring_window` says where the oldest
/// surviving byte sits. Linearise on the way out.
unsafe fn read_panic_pm(p: *const PanicPmRaw) -> PanicPostMortem {
    let total = core::ptr::read_volatile(core::ptr::addr_of!((*p).len)) as usize;
    let (start, len) = postmortem::ring_window(total, PANIC_MSG_MAX);
    let mut bytes = [0u8; PANIC_MSG_MAX];
    let src = core::ptr::addr_of!((*p).bytes).cast::<u8>();
    for (i, slot) in bytes.iter_mut().enumerate().take(len) {
        *slot = core::ptr::read_volatile(src.add(postmortem::ring_slot(start + i, PANIC_MSG_MAX)));
    }
    PanicPostMortem { len, total, bytes }
}

/// Read the panic message captured before the last soft-reset and mark
/// it seen. Returns `Some(_)` exactly once after a panic, `None`
/// otherwise — but the record itself stays in `.uninit` (downgraded to
/// its seen magic) so [`peek_panic_postmortem`] can re-read it.
pub fn take_panic_postmortem() -> Option<PanicPostMortem> {
    unsafe {
        let p = core::ptr::addr_of_mut!(PANIC_PM_RECORD).cast::<PanicPmRaw>();
        let magic = core::ptr::read_volatile(core::ptr::addr_of!((*p).magic));
        let seen = PANIC_PM.take(magic)?;
        let pm = read_panic_pm(p);
        // Mark seen (not erased) so boots don't re-log but the query can.
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).magic), seen);
        Some(pm)
    }
}

/// Read the stored panic post-mortem WITHOUT consuming it. Accepts both
/// a fresh record and one already replayed at boot. Debug-port query
/// path; safe to call any number of times.
pub fn peek_panic_postmortem() -> Option<PanicPostMortem> {
    unsafe {
        let p = core::ptr::addr_of!(PANIC_PM_RECORD).cast::<PanicPmRaw>();
        let magic = core::ptr::read_volatile(core::ptr::addr_of!((*p).magic));
        if !PANIC_PM.peek(magic) {
            return None;
        }
        Some(read_panic_pm(p))
    }
}

/// Ring writer for `core::fmt::write` — keeps the LAST `buf.len()`
/// bytes of everything written (`pos` counts total bytes, storage
/// wraps). On overflow the front of the message is sacrificed, not
/// the tail: SD fault panics put PC/PREGION at the END, behind an
/// expendable source-path prefix. Used inside the panic handler
/// where allocation must not happen.
struct ByteWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}
impl core::fmt::Write for ByteWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let cap = self.buf.len();
        for &b in s.as_bytes() {
            if let Some(slot) = self.buf.get_mut(postmortem::ring_slot(self.pos, cap)) {
                *slot = b;
            }
            self.pos += 1;
        }
        Ok(())
    }
}

/// Between a panic's message and the allocation post-mortem appended to it
/// ([`record_panic`]): a control character no panic message or census word
/// contains, so the replay's split is unambiguous.
pub const CENSUS_SEPARATOR: char = '\u{1e}';

/// The two halves of a recorded panic message: the message itself and, when
/// the panic was an allocation failure, the words
/// [`crate::heap::write_alloc_postmortem`] appended.
pub fn split_census(msg: &str) -> (&str, Option<&str>) {
    match msg.split_once(CENSUS_SEPARATOR) {
        Some((message, census)) => (message, Some(census)),
        None => (msg, None),
    }
}

/// Capture a panic's message into the retained record, magic last, so the
/// next boot can log it, followed by ` phase=<name>`
/// ([`crate::thread_watch::Phase`]). The log ring is unusable here: the executor is
/// dead and would never drain it. For the bind crate's `#[panic_handler]`;
/// no allocation, volatile writes only.
pub fn record_panic(info: &core::panic::PanicInfo) {
    unsafe {
        let p = core::ptr::addr_of_mut!(PANIC_PM_RECORD).cast::<PanicPmRaw>();
        let buf_ptr = core::ptr::addr_of_mut!((*p).bytes).cast::<u8>();
        let buf_slice = core::slice::from_raw_parts_mut(buf_ptr, PANIC_MSG_MAX);
        let mut writer = ByteWriter {
            buf: buf_slice,
            pos: 0,
        };
        // The phase thread mode last entered rides at the END, where the
        // ring keeps it whatever the message's length (order 1641): an
        // allocation failure names only its size, and the phase is what says
        // which work asked for it.
        let _ = core::fmt::write(
            &mut writer,
            format_args!(
                "{} phase={}",
                info,
                crate::thread_watch::current_phase().name()
            ),
        );
        // An allocation failure carries what the allocator froze and the
        // last census behind a separator (order 1665); the boot replay
        // splits it off as `[PANIC_CENSUS]`. Any other panic writes nothing
        // here.
        if crate::heap::alloc_failure().is_some() {
            let _ = core::fmt::Write::write_char(&mut writer, CENSUS_SEPARATOR);
            let _ = crate::heap::write_alloc_postmortem(
                &mut writer,
                embassy_time::Instant::now().as_millis(),
            );
        }
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).len), writer.pos as u32);
        // Magic last — partial write must not appear valid.
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).magic), PANIC_PM.fresh);
    }
}

/// HardFault post-mortem snapshot saved into a `.uninit` static so it
/// survives `sys_reset()`. The next boot reads it via
/// `take_hardfault_postmortem()` to log the faulting PC and the
/// register set, allowing post-hoc address-to-source resolution with
/// `arm-none-eabi-addr2line`.
#[repr(C)]
pub struct HardfaultPostMortem {
    /// `PM_MAGIC` when valid. Set on fault, cleared after one successful read.
    magic: u32,
    pub pc: u32,
    pub lr: u32,
    pub r0: u32,
    pub r1: u32,
    pub r2: u32,
    pub r3: u32,
    pub r12: u32,
    pub xpsr: u32,
}

/// Distinct from `paint_stack`'s `0xDEADBEEF` canary so a stray canary
/// word cannot masquerade as a valid PM. Versioned like [`PANIC_PM`]: bump
/// it if this record's layout ever changes, so a record left in `.uninit`
/// by an older image is rejected rather than misparsed. Its
/// already-replayed twin mirrors the panic record's: the boot read
/// downgrades to it instead of erasing, keeping the record retrievable via
/// the debug-port query until the next fault or power loss.
const HARDFAULT_PM: RecordMagic = postmortem::HARDFAULT_RECORD;

/// Survives `sys_reset` because `.retained` (memory.x) is `NOLOAD` —
/// values in RAM are not zeroed by the runtime startup — and sits below
/// every RAM address the Adafruit bootloader touches. In `.uninit` this
/// record occupied the top 36 bytes of RAM, dead centre of the
/// bootloader's stack; it could not have survived a single reset (see
/// `boot_trace::BOOT_TRACE` for the full story).
#[link_section = ".retained"]
static mut HARDFAULT_PM_RECORD: core::mem::MaybeUninit<HardfaultPostMortem> =
    core::mem::MaybeUninit::uninit();

/// Decode the record at `p`. Caller has validated the magic.
unsafe fn read_hardfault_pm(p: *const HardfaultPostMortem) -> HardfaultPostMortem {
    HardfaultPostMortem {
        magic: 0,
        pc: core::ptr::read_volatile(core::ptr::addr_of!((*p).pc)),
        lr: core::ptr::read_volatile(core::ptr::addr_of!((*p).lr)),
        r0: core::ptr::read_volatile(core::ptr::addr_of!((*p).r0)),
        r1: core::ptr::read_volatile(core::ptr::addr_of!((*p).r1)),
        r2: core::ptr::read_volatile(core::ptr::addr_of!((*p).r2)),
        r3: core::ptr::read_volatile(core::ptr::addr_of!((*p).r3)),
        r12: core::ptr::read_volatile(core::ptr::addr_of!((*p).r12)),
        xpsr: core::ptr::read_volatile(core::ptr::addr_of!((*p).xpsr)),
    }
}

/// Read the HardFault post-mortem captured before the last soft-reset
/// and mark it seen. Returns `Some(_)` once after a HardFault, `None`
/// otherwise — the record stays in `.uninit` for
/// [`peek_hardfault_postmortem`].
pub fn take_hardfault_postmortem() -> Option<HardfaultPostMortem> {
    unsafe {
        let p = core::ptr::addr_of_mut!(HARDFAULT_PM_RECORD).cast::<HardfaultPostMortem>();
        let magic = core::ptr::read_volatile(core::ptr::addr_of!((*p).magic));
        let seen = HARDFAULT_PM.take(magic)?;
        let pm = read_hardfault_pm(p);
        // Mark seen (not erased) so boots don't re-log but the query can.
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).magic), seen);
        Some(pm)
    }
}

/// Read the stored HardFault post-mortem WITHOUT consuming it. Accepts
/// both a fresh record and one already replayed at boot. Debug-port
/// query path; safe to call any number of times.
pub fn peek_hardfault_postmortem() -> Option<HardfaultPostMortem> {
    unsafe {
        let p = core::ptr::addr_of!(HARDFAULT_PM_RECORD).cast::<HardfaultPostMortem>();
        let magic = core::ptr::read_volatile(core::ptr::addr_of!((*p).magic));
        if !HARDFAULT_PM.peek(magic) {
            return None;
        }
        Some(read_hardfault_pm(p))
    }
}

/// The registers a HardFault handler hands [`record_hardfault`]: the
/// exception frame the core stacked.
pub struct HardfaultRegisters {
    pub pc: u32,
    pub lr: u32,
    pub r0: u32,
    pub r1: u32,
    pub r2: u32,
    pub r3: u32,
    pub r12: u32,
    pub xpsr: u32,
}

/// Save a HardFault's register snapshot into the retained record, magic
/// last, so a partial write can't masquerade as a valid PM. For the bind
/// crate's HardFault handler; volatile writes only.
pub fn record_hardfault(regs: &HardfaultRegisters) {
    unsafe {
        let p = core::ptr::addr_of_mut!(HARDFAULT_PM_RECORD).cast::<HardfaultPostMortem>();
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).pc), regs.pc);
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).lr), regs.lr);
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).r0), regs.r0);
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).r1), regs.r1);
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).r2), regs.r2);
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).r3), regs.r3);
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).r12), regs.r12);
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).xpsr), regs.xpsr);
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*p).magic), HARDFAULT_PM.fresh);
    }
}

/// Emit the `[PANIC_COUNT]` line. Shared by the boot banner of both
/// BSPs and the debug-port query.
pub fn log_panic_count() {
    log::log_fmt_critical(
        "[INFO!] ",
        format_args!("[PANIC_COUNT] total={}", panic_count()),
    );
}

/// Emit the post-mortem block: `[HARDFAULT_PMRT]` and/or `[PANIC_PMRT]`
/// plus `[PANIC_PMRT_TAIL]`. One formatter for the boot replay of both
/// BSPs and the debug-port query — capture parsers key on these exact
/// tags.
pub fn log_postmortems(
    hardfault_pm: Option<&HardfaultPostMortem>,
    panic_pm: Option<&PanicPostMortem>,
) {
    if let Some(pm) = hardfault_pm {
        log::log_fmt_critical(
            "[INFO!] ",
            format_args!(
                "[HARDFAULT_PMRT] pc=0x{:08x} lr=0x{:08x} r0=0x{:08x} r1=0x{:08x} r2=0x{:08x} r3=0x{:08x} r12=0x{:08x} xpsr=0x{:08x}",
                pm.pc, pm.lr, pm.r0, pm.r1, pm.r2, pm.r3, pm.r12, pm.xpsr
            ),
        );
    }
    if let Some(pm) = panic_pm {
        let whole = core::str::from_utf8(&pm.bytes[..pm.len]).unwrap_or("<non-utf8 panic msg>");
        let (msg, census) = split_census(whole);
        // EscapeCtrl keeps this ONE line: the SD fault panic embeds a
        // raw '\n' after the source location, which split the line and
        // cost us the PC/PREGION tail in capture.
        log::log_fmt_critical(
            "[INFO!] ",
            format_args!(
                "[PANIC_PMRT] len={} total={} msg={}",
                pm.len,
                pm.total,
                log::EscapeCtrl(msg)
            ),
        );
        // Tail again on its own short line — PC/PREGION ride at the END
        // of SD fault messages, and a short line survives any reader-
        // side line-length cap.
        let mut tail_start = msg.len().saturating_sub(96);
        while !msg.is_char_boundary(tail_start) {
            tail_start += 1;
        }
        log::log_fmt_critical(
            "[INFO!] ",
            format_args!("[PANIC_PMRT_TAIL] {}", log::EscapeCtrl(&msg[tail_start..])),
        );
        // What the allocator froze at the refusal and the last census before
        // it (order 1665): the holder the message's size and phase cannot
        // name.
        if let Some(census) = census {
            log::log_fmt_critical(
                "[INFO!] ",
                format_args!("[PANIC_CENSUS] {}", log::EscapeCtrl(census)),
            );
        }
    }
}

/// Debug-port query: replay the persistent panic evidence on demand.
///
/// Triggered by a `p` byte on the debug CDC port
/// (`usb::debug_reader_task`). The boot-time replay is emitted exactly
/// once into the 8 KiB log ring and is lapped by runtime output before
/// a late-attaching host can read it; this query re-emits the same
/// block whenever asked. `[PM_QUERY]` begin/done markers bracket the
/// response so `scripts/lnode-panic-query.sh` knows when it is
/// complete.
pub fn postmortem_query() {
    log::log_fmt_critical("[INFO!] ", format_args!("[PM_QUERY] begin"));
    log_panic_count();
    let hardfault_pm = peek_hardfault_postmortem();
    let panic_pm = peek_panic_postmortem();
    log_postmortems(hardfault_pm.as_ref(), panic_pm.as_ref());
    log::log_fmt_critical(
        "[INFO!] ",
        format_args!(
            "[PM_QUERY] done hardfault={} panic={}",
            hardfault_pm.is_some() as u8,
            panic_pm.is_some() as u8
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panic_count_accumulates_and_a_read_leaves_it() {
        let first = bump_panic_count();
        let second = bump_panic_count();
        assert_eq!(second, first.wrapping_add(1));
        assert_eq!(panic_count(), second);
        assert_eq!(panic_count(), second);
    }

    #[test]
    fn a_hardfault_record_is_taken_once_and_peeked_after() {
        record_hardfault(&HardfaultRegisters {
            pc: 0x0002_7f3c,
            lr: 0x0002_7e01,
            r0: 1,
            r1: 2,
            r2: 3,
            r3: 4,
            r12: 12,
            xpsr: 0x6100_0000,
        });
        let taken = take_hardfault_postmortem().expect("a fresh record");
        assert_eq!(
            (taken.pc, taken.lr, taken.r12),
            (0x0002_7f3c, 0x0002_7e01, 12)
        );
        assert!(take_hardfault_postmortem().is_none(), "taken twice");
        let peeked = peek_hardfault_postmortem().expect("a seen record stays readable");
        assert_eq!(peeked.xpsr, 0x6100_0000);
    }

    /// Order 1665: the allocation post-mortem rides behind the separator and
    /// the replay splits it off; a panic without it keeps its whole message.
    #[test]
    fn the_census_splits_off_an_allocation_failure_only() {
        let alloc = "panicked at alloc.rs:573:9:\nmemory allocation of 3456 bytes failed phase=ble-peer\u{1e}alloc size=3456 free=2100";
        assert_eq!(
            split_census(alloc),
            (
                "panicked at alloc.rs:573:9:\nmemory allocation of 3456 bytes failed phase=ble-peer",
                Some("alloc size=3456 free=2100")
            )
        );
        let other = "panicked at src/x.rs:1:1:\nindex out of bounds phase=timer";
        assert_eq!(split_census(other), (other, None));
    }
}
