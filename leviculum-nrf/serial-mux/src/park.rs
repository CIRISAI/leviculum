//! The bytes a flash op's masked window would lose, taken by the parked
//! core (1613, #474).
//!
//! The ThinkNode M9's flash writes and erases run on core 0 with that
//! core's interrupts off — esp-storage wraps every ROM call in its own
//! interrupt-masking lock, so the mask is not the firmware's to lift —
//! and the other core hardware-stalled, since it executes through the
//! cache the op turns off. One sector erase is 35 to 54 ms on the board
//! (window 23's pull, `park_max_ms`), the ROM's bound 57 ms; the UART's
//! receive FIFO holds 128 bytes, 11 ms of the line at 115200 baud.
//! Everything past those 128 bytes was dropped by the FIFO with no CPU
//! anywhere to take it: 2 of 100 injected key frames in window 22, 1 of
//! 100 in window 23, each inside an erase. Lew's rule (2026-11-07):
//! input runs decoupled from drawing and is never lost.
//!
//! The design that closes it: the UI core is parked *cooperatively* —
//! an inter-processor interrupt puts it into a spin that runs entirely
//! from internal RAM, so the op's cache-off window cannot touch it —
//! and that spin polls the receive FIFOs into a [`ParkRing`] in internal
//! RAM for as long as the op runs. One core always has the FIFO: core
//! 0's interrupt whenever its interrupts are on, the parked spin exactly
//! while they are off. The ring is handed back to core 0 when the op
//! ends, ahead of whatever the FIFO holds next, so the stream stays in
//! order.
//!
//! The ring is deliberately not a concurrent queue. The park protocol
//! is phase-separated: the spin alone writes while the op runs, core 0
//! alone drains after the spin has confirmed it stopped (an
//! acquire/release handshake orders the two), and both sides never run
//! at once. [`ParkRing`] only has to be a buffer whose writer cannot
//! panic and calls nothing — the spin that calls it runs from internal
//! RAM while the flash is unreadable, so a panic path (a bounds check's
//! abort, a formatting call) would be a jump into unreadable code.
//!
//! The model test below pins the arithmetic and the handoff: a byte
//! stream at line rate across a modelled 57 ms masked window loses
//! nothing and stays in order with the parked poller and a ring of
//! [`HOST_RING`] bytes, and the same stream against the FIFO alone —
//! today's design, and the largest FIFO the shared UART RAM could give
//! (option (d), 1613) — loses bytes. See
//! `docs/src/concepts/handheld-messenger.md`, "A key frame survives a
//! flash erase".

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU32, Ordering};

/// The host UART's ring: 178 ms of the line at 115200 baud, 3.1 times
/// the 57 ms bound of one sector erase, so even an erase the datasheet
/// allows to run long leaves headroom before a byte is counted lost.
pub const HOST_RING: usize = 2048;

/// The GNSS UART's ring: 89 ms of the line, 1.5 times the bound. A lost
/// NMEA sentence is the next second's, so the smaller ring spends less
/// internal RAM, which is the M9's core-0 stack headroom.
pub const GNSS_RING: usize = 1024;

/// Bytes one masked window can carry at most: the 57 ms bound at 115200
/// baud, 8N1 (11520 bytes a second).
pub const MASKED_WINDOW_BYTES: usize = 57 * 11520 / 1000;

// The host ring outlasts the bound with room for a late erase; the GNSS
// ring at least covers it.
const _: () = assert!(HOST_RING >= 3 * MASKED_WINDOW_BYTES);
const _: () = assert!(GNSS_RING >= MASKED_WINDOW_BYTES);

/// A byte sink for one masked window: written by the parked core's spin,
/// drained by the op's core after the handshake, never both at once.
///
/// `push` is written for the spin's constraints: it cannot panic and
/// calls nothing, so a caller placed in internal RAM stays in internal
/// RAM. A full ring counts the byte instead of keeping it.
pub struct ParkRing<const N: usize> {
    buf: UnsafeCell<[u8; N]>,
    /// Bytes written this window; the writer publishes with `Release`,
    /// the drainer reads with `Acquire`, and the park protocol's own
    /// done-handshake orders the phases.
    len: AtomicU32,
    /// Bytes pushed past `N`, cumulative across windows.
    lost: AtomicU32,
}

// SAFETY: the phase discipline above — one writer (the parked spin),
// then one drainer (the op's core), ordered by the protocol's
// acquire/release handshake, never concurrent. The atomics carry the
// publication; the buffer is only touched by the side the phase names.
unsafe impl<const N: usize> Sync for ParkRing<N> {}

impl<const N: usize> Default for ParkRing<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> ParkRing<N> {
    /// An empty ring; `const` so it can be a static.
    pub const fn new() -> Self {
        Self {
            buf: UnsafeCell::new([0; N]),
            len: AtomicU32::new(0),
            lost: AtomicU32::new(0),
        }
    }

    /// Keep one byte, or count it lost. Panic-free and call-free: the
    /// parked spin runs this from internal RAM while the flash — and so
    /// every panic path — is unreadable.
    ///
    /// # Safety
    ///
    /// Only the parked spin calls this, and only while the protocol says
    /// it is the writer (between its ack and its done stores).
    #[inline(always)]
    pub unsafe fn push(&self, byte: u8) {
        let len = self.len.load(Ordering::Relaxed) as usize;
        if len < N {
            // SAFETY: `len < N` bounds the write; the phase discipline
            // (type doc) makes this the only access to the buffer now.
            unsafe { (self.buf.get() as *mut u8).add(len).write(byte) };
            self.len.store(len as u32 + 1, Ordering::Release);
        } else {
            let lost = self.lost.load(Ordering::Relaxed);
            self.lost.store(lost.saturating_add(1), Ordering::Release);
        }
    }

    /// Hand this window's bytes to `take` and start the next window:
    /// the op's core, after the done-handshake.
    pub fn drain(&self, take: impl FnOnce(&[u8])) {
        let len = (self.len.load(Ordering::Acquire) as usize).min(N);
        // SAFETY: the phase discipline (type doc): the writer has
        // confirmed it stopped before the drainer runs.
        let buf: &[u8; N] = unsafe { &*self.buf.get() };
        take(&buf[..len]);
        self.len.store(0, Ordering::Release);
    }

    /// Bytes pushed past the ring's end so far, wrapping never (saturates).
    pub fn lost(&self) -> u32 {
        self.lost.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::*;
    use crate::rx::RxKeep;

    /// Bytes a second at 115200 baud, 8N1.
    const LINE: usize = 11520;
    /// The masked window's bound (1613): one sector erase, 57 ms.
    const MASK_US: usize = 57_000;
    /// The M9's UART0 receive FIFO.
    const FIFO: usize = 128;

    /// One injected stream: key frames (10 bytes on the wire, the
    /// walker's `key_frame` selftest) between node frames (64 bytes),
    /// back to back — the line never idles, as under a pull.
    fn stream() -> Vec<u8> {
        let mut out = Vec::new();
        let mut key = 0u8;
        while out.len() < 2300 {
            // A key frame's shape: flag, four header bytes, code, hold,
            // flag — the bytes only matter for order checking.
            out.extend_from_slice(&[0x7E, 0xA4, 0xA5, 0x1A, 0x00, key, 0xB5, 0x01, 0xF4, 0x7E]);
            key = key.wrapping_add(1);
            for i in 0..64u8 {
                out.push(i ^ key);
            }
        }
        out
    }

    /// Today's design and option (d), modelled: during the mask no CPU
    /// takes the FIFO, so a window carries at most the FIFO's bytes.
    /// Returns the bytes lost.
    fn fifo_only_loss(fifo: usize) -> usize {
        let masked_bytes = MASK_US * LINE / 1_000_000;
        masked_bytes.saturating_sub(fifo)
    }

    #[test]
    fn the_fifo_alone_loses_at_every_size_the_uart_ram_allows() {
        // 128 today, 384 and 512 the most rx_size could grow without
        // taking the other UARTs' share of the 1 KiB UART RAM (option
        // (d), 1613). Positive control for the model: today's design
        // must lose, or the model could not say the fix holds.
        assert_eq!(fifo_only_loss(FIFO), 528);
        assert_eq!(fifo_only_loss(384), 272);
        assert_eq!(fifo_only_loss(512), 144);
        // Only past 657 bytes would a FIFO cover the bound — and the
        // S3's UART RAM is 1 KiB shared by three UARTs, so it cannot.
        assert_eq!(fifo_only_loss(MASKED_WINDOW_BYTES), 0);
    }

    /// The chosen design, modelled byte by byte: a popper is always
    /// live. Outside the mask the interrupt empties the FIFO at its
    /// threshold; the op's preamble sweeps the FIFO, the parked spin
    /// polls every byte into the ring, the harvest appends the ring to
    /// the keep before the interrupt resumes. Nothing is lost and the
    /// stream stays in order.
    #[test]
    fn a_parked_poller_rides_a_57ms_mask_at_line_rate_without_loss() {
        let sent = stream();
        let byte_us = 1_000_000 / LINE; // 86 us within a byte's true 86.8
        let mask_from = 70_000; // the op starts mid-stream
        let mask_to = mask_from + MASK_US;

        let ring: ParkRing<HOST_RING> = ParkRing::new();
        let mut keep: RxKeep<8192> = RxKeep::new();
        let mut fifo: Vec<u8> = Vec::new();
        let mut got: Vec<u8> = Vec::new();
        let mut swept = false;

        for (i, &byte) in sent.iter().enumerate() {
            let at_us = i * byte_us;
            if at_us >= mask_to && swept {
                // The op ended: harvest the ring into the keep before
                // the interrupt pops anything newer.
                ring.drain(|bytes| keep.push(bytes));
                swept = false;
            }
            if at_us < mask_from || at_us >= mask_to {
                // Interrupt territory: the FIFO takes the byte and the
                // handler empties it past the threshold (64, half the
                // FIFO), well inside its 5.5 ms budget.
                fifo.push(byte);
                assert!(fifo.len() <= FIFO, "the live interrupt never lets it fill");
                if fifo.len() >= 64 {
                    keep.push(&fifo);
                    fifo.clear();
                }
            } else {
                if !swept {
                    // The op's preamble: drain_rx_fifo, then the park.
                    keep.push(&fifo);
                    fifo.clear();
                    swept = true;
                }
                // The parked spin polls far faster than the line; the
                // FIFO never holds more than the byte in flight.
                // SAFETY (test): this loop is the modelled spin, the
                // sole writer of its phase.
                unsafe { ring.push(byte) };
            }
            let mut buf = [0u8; 256];
            loop {
                // The loop reads between everything; during the mask it
                // is parked (core 1) or masked (core 0), so reading here
                // too only makes the ordering check stricter.
                let n = keep.pop_into(&mut buf);
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
            }
        }
        if swept {
            ring.drain(|bytes| keep.push(bytes));
        }
        keep.push(&fifo);
        let mut buf = [0u8; 256];
        loop {
            let n = keep.pop_into(&mut buf);
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
        }

        assert_eq!(ring.lost(), 0, "the ring outlasts the window");
        assert_eq!(keep.lost(), 0);
        assert_eq!(got, sent, "every byte, in order");
    }

    #[test]
    fn a_full_ring_counts_instead_of_keeping() {
        let ring: ParkRing<4> = ParkRing::new();
        for byte in 0..6u8 {
            // SAFETY (test): sole writer.
            unsafe { ring.push(byte) };
        }
        assert_eq!(ring.lost(), 2);
        ring.drain(|bytes| assert_eq!(bytes, &[0, 1, 2, 3]));
        // The next window starts empty; the lost count stands.
        ring.drain(|bytes| assert!(bytes.is_empty()));
        assert_eq!(ring.lost(), 2);
    }
}
