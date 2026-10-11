//! The panic and HardFault post-mortems: what a boot replays, what the
//! debug-port query reads, how an overlong panic message is kept, and how a
//! fault leaves the board (leviculum#413, the main-loop section of
//! `docs/src/firmware/host-test-seams.md`).
//!
//! Both records live in `.retained` RAM next to the breadcrumb record of
//! this crate's root, written by the panic handler and the HardFault
//! handler in `leviculum-nrf/src/lib.rs` and read on the next boot. A test
//! that drives one on the rig needs a fault at a chosen instant and the
//! reset after it; every decision about the record is here instead, and the
//! handlers keep only the volatile reads and writes and the register pokes.
//!
//! # Replay once, query always
//!
//! A record carries one of two magics: fresh, written by the fault, and
//! seen, left behind by the boot that replayed it. [`RecordMagic::take`] is
//! the boot's read: it returns a fresh record once and downgrades it, so a
//! boot does not replay the same fault twice. [`RecordMagic::peek`] is the
//! `p` query on the debug port: it accepts both, because the boot's replay
//! goes into an 8 KiB log ring that runtime output laps before a late host
//! reads it. Any other word is no record: power-on RAM, a record an older
//! image left with a magic this one does not know, or the other record's.
//!
//! # The message ring
//!
//! The panic writer keeps the LAST [`PANIC_MSG_MAX`] bytes of the message
//! ([`ring_slot`]), because a SoftDevice fault panic carries its PC and
//! PREGION at the end, behind a source path that can be dropped.
//! [`ring_window`] linearises it on the way out.
//!
//! # The way out
//!
//! [`fault_exit`]: a board with no LED armed resets at once, so the next
//! boot logs the post-mortem without delay; one with an LED blinks it
//! [`BLINK_CYCLES`] times first. [`led_registers`] names the GPIO registers
//! the blink writes, one function for both handlers.

/// One record's two magics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordMagic {
    /// Written by the fault handler, last, so a partial write is no record.
    pub fresh: u32,
    /// Left behind by the boot that replayed the record.
    pub seen: u32,
}

/// The panic message record. `0xBADD_CAF1` since `len` came to mean "total
/// bytes produced" with the bytes a ring; the older meaning ("bytes stored,
/// clamped") read the new way linearises at the wrong offset, so the magic
/// was bumped and an older record reads as none.
pub const PANIC_RECORD: RecordMagic = RecordMagic {
    fresh: 0xBADD_CAF1,
    seen: 0xBADD_CAF2,
};

/// The HardFault register snapshot record. Distinct from the stack canary
/// `0xDEADBEEF`, so a stray canary word cannot read as a record.
pub const HARDFAULT_RECORD: RecordMagic = RecordMagic {
    fresh: 0xC0FF_EE12,
    seen: 0xC0FF_EE13,
};

impl RecordMagic {
    /// The boot's read. `Some(word)` when the record is returned, with the
    /// magic to write back; `None` when there is nothing to replay, which
    /// includes a record an earlier boot already replayed.
    pub fn take(&self, magic: u32) -> Option<u32> {
        (magic == self.fresh).then_some(self.seen)
    }

    /// The debug-port query's read: a fresh record or a replayed one,
    /// nothing written back.
    pub fn peek(&self, magic: u32) -> bool {
        magic == self.fresh || magic == self.seen
    }
}

/// Bytes of panic message the record keeps.
pub const PANIC_MSG_MAX: usize = 1024;

/// Where the writer puts the byte at position `pos` of the message in a
/// ring of `cap` bytes. `cap` zero is answered with slot zero, not divided
/// by.
pub fn ring_slot(pos: usize, cap: usize) -> usize {
    pos.checked_rem(cap).unwrap_or(0)
}

/// Where the oldest surviving byte of a `total`-byte message sits in a ring
/// of `cap` bytes, and how many bytes survive. A message that fit starts at
/// zero; an overlong one keeps its last `cap` bytes, the oldest of them at
/// `total % cap`.
pub fn ring_window(total: usize, cap: usize) -> (usize, usize) {
    if total > cap {
        (ring_slot(total, cap), cap)
    } else {
        (0, total)
    }
}

/// How many times a fault handler blinks its LED before it resets (about
/// five seconds at the handlers' busy-wait).
pub const BLINK_CYCLES: u32 = 25;

/// How a fault handler leaves the board once the record is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultExit {
    /// `sys_reset` at once.
    ResetNow,
    /// Blink the armed LED this many times, then `sys_reset`.
    BlinkThenReset {
        /// [`BLINK_CYCLES`].
        cycles: u32,
    },
}

/// The way out of a panic or a HardFault, for whether the board armed a
/// fault LED.
pub fn fault_exit(led_armed: bool) -> FaultExit {
    if led_armed {
        FaultExit::BlinkThenReset {
            cycles: BLINK_CYCLES,
        }
    } else {
        FaultExit::ResetNow
    }
}

/// GPIO P0's register base on the nRF52840.
pub const P0_BASE: u32 = 0x5000_0000;
/// GPIO P1's register base on the nRF52840.
pub const P1_BASE: u32 = 0x5000_0300;

/// The registers a fault blink writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LedRegisters {
    /// DIRSET, to make the pin an output.
    pub dirset: u32,
    /// The register that lights the LED: OUTSET, or OUTCLR when active low.
    pub on: u32,
    /// The register that darkens it.
    pub off: u32,
    /// The pin's bit.
    pub mask: u32,
}

/// The registers for an LED on `port` (1 is P1, anything else P0) and `pin`
/// (its low five bits), lit high or, when `active_low`, lit low.
pub fn led_registers(port: u8, pin: u8, active_low: bool) -> LedRegisters {
    let base = if port == 1 { P1_BASE } else { P0_BASE };
    let outset = base + 0x508;
    let outclr = base + 0x50C;
    let (on, off) = if active_low {
        (outclr, outset)
    } else {
        (outset, outclr)
    };
    LedRegisters {
        dirset: base + 0x518,
        on,
        off,
        mask: 1u32 << (pin & 31),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A boot, a second boot, a query, a fresh fault, the next boot: what
    /// the `.retained` word holds and what each read returns.
    fn replay_once_query_always(record: RecordMagic) {
        let mut magic = record.fresh;
        // The first boot after the fault replays it and downgrades it.
        let written = record.take(magic);
        assert_eq!(written, Some(record.seen));
        magic = written.unwrap_or(magic);
        // The next boot finds nothing to replay.
        assert_eq!(record.take(magic), None);
        // The query still reads it, and reading leaves it as it is.
        assert!(record.peek(magic));
        assert!(record.peek(magic));
        assert_eq!(magic, record.seen);
        // A fresh fault overwrites the magic; the next boot replays again.
        magic = record.fresh;
        assert!(record.peek(magic));
        assert_eq!(record.take(magic), Some(record.seen));
    }

    #[test]
    fn a_panic_is_replayed_by_one_boot_and_read_by_every_query() {
        replay_once_query_always(PANIC_RECORD);
    }

    #[test]
    fn a_hardfault_is_replayed_by_one_boot_and_read_by_every_query() {
        replay_once_query_always(HARDFAULT_RECORD);
    }

    /// Words a record can hold without a fault behind it: zeroed and
    /// all-ones power-on RAM, the stack canary, the breadcrumb record's
    /// magic, the panic record's pre-ring magic, and the other record's
    /// magics. None is replayed or queried.
    #[test]
    fn a_word_that_is_not_this_records_magic_is_no_record() {
        let older_panic = 0xBADD_CAF0;
        for (record, other) in [
            (PANIC_RECORD, HARDFAULT_RECORD),
            (HARDFAULT_RECORD, PANIC_RECORD),
        ] {
            for word in [
                0,
                u32::MAX,
                0xDEAD_BEEF,
                crate::MAGIC,
                older_panic,
                other.fresh,
                other.seen,
            ] {
                assert_eq!(record.take(word), None, "{word:#010x}");
                assert!(!record.peek(word), "{word:#010x}");
            }
        }
    }

    /// The writer the panic handler runs, over `ring_slot`.
    fn write_ring(message: &[u8], ring: &mut [u8]) -> usize {
        let cap = ring.len();
        let mut pos = 0;
        for &b in message {
            if let Some(slot) = ring.get_mut(ring_slot(pos, cap)) {
                *slot = b;
            }
            pos += 1;
        }
        pos
    }

    /// The reader `read_panic_pm` runs, over `ring_window`.
    fn read_ring(ring: &[u8], total: usize) -> Vec<u8> {
        let (start, len) = ring_window(total, ring.len());
        (0..len)
            .map(|i| ring[ring_slot(start + i, ring.len())])
            .collect()
    }

    /// The shape of a SoftDevice fault panic: a source path, a raw newline,
    /// and the PC and PREGION at the end. Padded past the ring, the tail
    /// must survive whole and in order.
    #[test]
    fn an_overlong_panic_keeps_its_tail_in_order() {
        let mut message = Vec::new();
        message.extend_from_slice(
            b"panicked at /home/build/.cargo/registry/src/nrf-softdevice/src/softdevice.rs:",
        );
        while message.len() < PANIC_MSG_MAX + 77 {
            message.extend_from_slice(b"0123456789");
        }
        message
            .extend_from_slice(b":\nSoftdevice fault id=1 pc=0x0002a4c8 info=0x00000001 PREGION=1");
        let mut ring = [0u8; PANIC_MSG_MAX];
        let total = write_ring(&message, &mut ring);
        assert_eq!(total, message.len());
        let read = read_ring(&ring, total);
        assert_eq!(read.len(), PANIC_MSG_MAX);
        assert_eq!(read[..], message[message.len() - PANIC_MSG_MAX..]);
        assert!(read.ends_with(b"pc=0x0002a4c8 info=0x00000001 PREGION=1"));
    }

    #[test]
    fn a_message_that_fits_is_read_from_the_start() {
        for len in [0usize, 1, PANIC_MSG_MAX - 1, PANIC_MSG_MAX] {
            let message: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let mut ring = [0u8; PANIC_MSG_MAX];
            let total = write_ring(&message, &mut ring);
            assert_eq!(ring_window(total, PANIC_MSG_MAX), (0, len));
            assert_eq!(read_ring(&ring, total), message);
        }
    }

    #[test]
    fn the_window_wraps_at_whole_multiples_of_the_ring() {
        assert_eq!(
            ring_window(PANIC_MSG_MAX + 1, PANIC_MSG_MAX),
            (1, PANIC_MSG_MAX)
        );
        assert_eq!(
            ring_window(2 * PANIC_MSG_MAX, PANIC_MSG_MAX),
            (0, PANIC_MSG_MAX)
        );
        assert_eq!(ring_window(5, 0), (0, 0));
        assert_eq!(ring_slot(7, 0), 0);
    }

    #[test]
    fn without_an_led_a_fault_resets_at_once() {
        assert_eq!(fault_exit(false), FaultExit::ResetNow);
    }

    #[test]
    fn with_an_led_a_fault_blinks_twenty_five_times_then_resets() {
        assert_eq!(fault_exit(true), FaultExit::BlinkThenReset { cycles: 25 });
    }

    /// The registers of the nRF52840 GPIO (product specification, GPIO
    /// register table): OUTSET at 0x508, OUTCLR at 0x50C, DIRSET at 0x518.
    #[test]
    fn the_blink_writes_the_pins_registers() {
        // The T114's panic LED, P1.03, active low (`boards/t114.rs`): lit
        // through OUTCLR.
        let t114 = led_registers(1, 3, true);
        assert_eq!(
            t114,
            LedRegisters {
                dirset: 0x5000_0818,
                on: 0x5000_080C,
                off: 0x5000_0808,
                mask: 1 << 3,
            }
        );
        // The Solar Node's HardFault LED, P0.15, lit high
        // (`bin/solarnode.rs`): the two swap.
        let solar = led_registers(0, 15, false);
        assert_eq!((solar.on, solar.off), (0x5000_0508, 0x5000_050C));
        assert_eq!(solar.dirset, 0x5000_0518);
        assert_eq!(solar.mask, 1 << 15);
        // Any port but 1 is P0, and the pin keeps its low five bits.
        assert_eq!(led_registers(7, 35, false), led_registers(0, 3, false));
    }
}
