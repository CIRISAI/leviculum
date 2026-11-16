//! How the transport port writes one HDLC frame, and why a started frame is
//! driven to its end inside one poll (LV-1006, order 1671).
//!
//! # The mechanism
//!
//! A full-speed CDC IN endpoint carries 64 bytes a packet, and the nRF52840
//! USBD holds one packet per endpoint. The embassy driver's `write_packet`
//! waits until the host has taken the previous packet, copies the next one
//! in and returns; so a frame of `n` packets is `n` awaits, and every one
//! after the first returns `Pending` while the host collects the packet
//! before it, about a millisecond.
//!
//! The transport task runs on the board's thread-mode executor beside the
//! main loop. A `Pending` hands the CPU back, and the task's next poll waits
//! for whatever main-loop poll is running to end. After a propagation-node
//! deposit those polls run for hundreds of milliseconds (1665 found a 6.6 s
//! one in the same situation). The rest of the frame waits that long.
//!
//! The host does not see the break at the packet where it happens. Linux
//! `cdc_acm` reads with two-packet URBs (128 bytes), and a URB completes when
//! it is full or a short packet ends it, so every break inside a frame
//! reaches the reader at wire offset 128 (or 256, ...). That is the bench
//! window of 2026-11-11 (`t114-ingress/gaps-run.txt`): every split frame
//! split at offset 128, holes of 160 to 401 ms.
//!
//! Python-RNS `SerialInterface.readLoop` (`SerialInterface.py:190-195`,
//! 1.5.2) drops a frame with a hole over 100 ms and, through a state bug,
//! reads every later frame out of phase until rnsd restarts. Our own reader
//! drops the one frame and keeps its phase (the mvr
//! `a_frame_torn_by_a_gap_leaves_the_serial_decoder_in_phase`); the board
//! must still never send a frame with a hole in it.
//!
//! # The writer
//!
//! [`write_frame`] writes the first packet with an ordinary await: the wait
//! before it lies between two frames, where no reader times anything. Each
//! later packet, and the zero-length packet that ends a frame whose length
//! is a multiple of 64, goes through [`eager`]: the write future is polled
//! in a loop, without handing the CPU back, until the host has taken the
//! previous packet. The USBD interrupt that marks the endpoint ready runs at
//! a priority above thread mode, so it lands during the spin. The frame
//! then leaves inside one poll of the transport task, whatever the main loop
//! is doing, and the thread executor pays about one host poll per packet.
//!
//! The spin is bounded by [`PACKET_SPIN_US`] a packet. A host that takes
//! no packet that long has stopped reading (the port is closing, the host
//! controller is stalled), and the write falls back to the plain await,
//! which is the host-gated one in the firmware: it abandons the frame when
//! DTR drops, as before.
//!
//! Moving the writer to an interrupt executor instead, as the LoRa task was
//! moved (leviculum#850), would race the USBD's EasyDMA: the peripheral
//! runs one endpoint transfer at a time, the embassy driver starts and
//! finishes each transfer inside one poll without a lock, and the transport
//! reader, the debug port and the control pipe all run theirs from thread
//! mode, where a preempting writer would start a second one.

use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, Waker};

/// Bytes a full-speed bulk packet carries; the board's CDC endpoints are
/// created with this size (`CdcAcmClass::new(.., 64)` in `usb.rs`).
pub const PACKET: usize = 64;

/// The longest the writer spins for the host to take one packet before it
/// hands the CPU back.
///
/// A host with a read pending (Linux `cdc_acm` keeps its URBs queued
/// whether or not a process reads the tty) takes a packet within a
/// full-speed frame, 1 ms. Five frames is room for a busy bus and still
/// well under the 100 ms the reference's reader allows inside a frame, so
/// even a frame whose every packet ran the bound out would not tear a
/// two-URB frame: a frame of [`PACKET`]-byte packets is torn only by a hole
/// longer than that between two URBs.
pub const PACKET_SPIN_US: u64 = 5_000;

/// The reference reader's partial-frame timeout
/// (`RNS/Interfaces/SerialInterface.py`, `self.timeout = 100`). A hole
/// inside a frame longer than this loses the frame there, and with 1.5.2's
/// state bug every frame after it.
pub const REFERENCE_FRAME_TIMEOUT_US: u64 = 100_000;

// The bound sits well inside the reference's timeout: a packet that runs it
// out still leaves a two-URB frame an order of magnitude short of a tear.
// And it outlasts a full-speed frame twice, so a host that is reading is
// never mistaken for one that stopped.
const _: () = assert!(PACKET_SPIN_US * 10 < REFERENCE_FRAME_TIMEOUT_US);
const _: () = assert!(PACKET_SPIN_US >= 2 * 1_000);

/// One packet out of the transport endpoint. In the firmware this is the
/// host-gated write, which answers `false` when no host is reading.
pub trait PacketOut {
    /// Write one packet of at most [`PACKET`] bytes; an empty chunk is the
    /// zero-length packet.
    fn write_packet(&mut self, chunk: &[u8]) -> impl Future<Output = bool>;
}

/// Poll `fut` without yielding until it completes or `spin_us` have passed
/// on `clock`, then await it like any other future.
///
/// The spin polls with a waker that does nothing, which is safe here
/// because nothing needs waking: the loop polls again at once. If the bound
/// runs out the same pinned future is awaited, so its first real poll
/// registers the task's waker and no state is lost.
pub async fn eager<F: Future>(fut: F, clock: &impl Fn() -> u64, spin_us: u64) -> F::Output {
    let mut fut = pin!(fut);
    let start = clock();
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(out) = fut.as_mut().poll(&mut cx) {
            return out;
        }
        if clock().saturating_sub(start) >= spin_us {
            break;
        }
        core::hint::spin_loop();
    }
    fut.await
}

/// Write an already framed `frame` as [`PACKET`]-byte packets, with a
/// zero-length packet when its length is a multiple of [`PACKET`], so the
/// host's read completes at the frame's end.
///
/// Every packet after the first is written through [`eager`] with
/// `spin_us` (the firmware passes [`PACKET_SPIN_US`]). Returns `false` at
/// the first packet the port refused, and the frame stops there.
pub async fn write_frame<P: PacketOut>(
    port: &mut P,
    clock: &impl Fn() -> u64,
    spin_us: u64,
    frame: &[u8],
) -> bool {
    for (i, chunk) in frame.chunks(PACKET).enumerate() {
        let ok = if i == 0 {
            port.write_packet(chunk).await
        } else {
            eager(port.write_packet(chunk), clock, spin_us).await
        };
        if !ok {
            return false;
        }
    }
    if !frame.is_empty() && frame.len().is_multiple_of(PACKET) {
        return eager(port.write_packet(&[]), clock, spin_us).await;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use core::cell::{Cell, RefCell};
    use core::pin::Pin;

    /// How long after the endpoint holds a packet the host has taken it: a
    /// host with a read queued polls a full-speed bulk endpoint within a
    /// frame.
    const HOST_TAKE_US: u64 = 1_000;
    /// The host's read size: Linux `cdc_acm` reads two packets at a time.
    const URB: usize = 2 * PACKET;
    /// The longest hole the bench window measured inside a frame.
    const BENCH_HOLE_US: u64 = 401_000;
    /// What one spin iteration costs on the virtual clock.
    const SPIN_STEP_US: u64 = 1;

    /// The virtual clock, the one-packet endpoint and what the host read.
    struct Bench {
        now_us: Cell<u64>,
        /// When the packet in the endpoint is taken by the host; `None` is
        /// an empty endpoint.
        taken_at: Cell<Option<u64>>,
        /// Whether the host takes packets at all.
        host_reading: Cell<bool>,
        /// Every packet the host took: (when, its length).
        packets: RefCell<Vec<(u64, usize)>>,
        /// The instant the writer becomes runnable again after `Pending`.
        due_us: Cell<Option<u64>>,
    }

    impl Bench {
        fn new() -> Self {
            Self {
                now_us: Cell::new(0),
                taken_at: Cell::new(None),
                host_reading: Cell::new(true),
                packets: RefCell::new(Vec::new()),
                due_us: Cell::new(None),
            }
        }

        /// The spin's clock: reading it costs a spin step.
        fn clock(&self) -> u64 {
            let now = self.now_us.get() + SPIN_STEP_US;
            self.now_us.set(now);
            now
        }

        /// The endpoint is free again once the host took what it held.
        fn endpoint_free(&self) -> bool {
            match self.taken_at.get() {
                None => true,
                Some(at) => self.host_reading.get() && at <= self.now_us.get(),
            }
        }

        /// The instants at which the host's reads completed, with the wire
        /// offset each completed at: a read completes when it holds [`URB`]
        /// bytes or a short packet ends it.
        fn reads(&self) -> Vec<(u64, usize)> {
            let mut out = Vec::new();
            let (mut offset, mut in_urb) = (0usize, 0usize);
            for &(at, len) in self.packets.borrow().iter() {
                offset += len;
                in_urb += len;
                if in_urb == URB || len < PACKET {
                    out.push((at, offset));
                    in_urb = 0;
                }
            }
            out
        }

        /// The longest hole between two host reads inside the frame, and the
        /// wire offset the frame was torn at.
        fn worst_hole(&self) -> (u64, usize) {
            let reads = self.reads();
            reads
                .windows(2)
                .map(|w| (w[1].0 - w[0].0, w[0].1))
                .max_by_key(|&(hole, _)| hole)
                .unwrap_or((0, 0))
        }
    }

    /// One packet into the bench endpoint.
    struct Write<'a> {
        bench: &'a Bench,
        len: usize,
    }

    impl Future for Write<'_> {
        type Output = bool;
        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<bool> {
            let b = self.bench;
            if !b.endpoint_free() {
                // Due when the host takes the packet it holds; a host that
                // does not read never makes it due.
                b.due_us
                    .set(b.taken_at.get().filter(|_| b.host_reading.get()));
                return Poll::Pending;
            }
            let at = b.now_us.get() + HOST_TAKE_US;
            b.taken_at.set(Some(at));
            b.packets.borrow_mut().push((at, self.len));
            b.due_us.set(None);
            Poll::Ready(true)
        }
    }

    struct Port<'a>(&'a Bench);

    impl PacketOut for Port<'_> {
        fn write_packet(&mut self, chunk: &[u8]) -> impl Future<Output = bool> {
            Write {
                bench: self.0,
                len: chunk.len(),
            }
        }
    }

    /// What the main loop does on the thread-mode executor beside the writer.
    #[derive(Clone, Copy)]
    enum Foreign {
        /// One long poll that becomes runnable at `start_us` (the deposit's
        /// work) and lasts `len_us`, then nothing.
        OnePoll { start_us: u64, len_us: u64 },
        /// Runnable always, every poll `len_us` long.
        Grind { len_us: u64 },
    }

    impl Foreign {
        /// The writer returned `Pending` at `now_us` and is runnable again at
        /// `due_us`: when the executor polls it next. A main-loop poll the
        /// executor has entered runs to its end.
        fn next_writer_poll(&mut self, now_us: u64, due_us: u64) -> u64 {
            match *self {
                Foreign::OnePoll { start_us, len_us } if start_us <= due_us => {
                    *self = Foreign::OnePoll {
                        start_us: u64::MAX,
                        len_us: 0,
                    };
                    now_us.max(start_us) + len_us
                }
                Foreign::OnePoll { .. } => due_us,
                Foreign::Grind { len_us } => now_us + len_us,
            }
            .max(due_us)
        }
    }

    /// Run the writer on the thread-mode executor beside `main`. Returns the
    /// thread-mode time the writer's own polls took.
    fn run_beside_main_loop<F: Future<Output = bool>>(
        bench: &Bench,
        mut main: Foreign,
        writer: F,
    ) -> u64 {
        let mut writer = pin!(writer);
        let mut cx = Context::from_waker(Waker::noop());
        let mut writer_us = 0;
        for _ in 0..10_000 {
            let before = bench.now_us.get();
            let polled = writer.as_mut().poll(&mut cx);
            writer_us += bench.now_us.get() - before;
            if let Poll::Ready(ok) = polled {
                assert!(ok, "the bench port refuses nothing");
                return writer_us;
            }
            let now = bench.now_us.get();
            let due = bench.due_us.get().unwrap_or(now);
            bench.now_us.set(main.next_writer_poll(now, due));
        }
        panic!("the writer never finished on the bench");
    }

    /// The deposit's work becomes runnable while the frame is going out,
    /// after the host has its first two packets, and holds the executor
    /// 400 ms: the bench window's case.
    const DEPOSIT: Foreign = Foreign::OnePoll {
        start_us: HOST_TAKE_US + HOST_TAKE_US / 2,
        len_us: 400_000,
    };
    /// The worst case: the main loop is runnable at every turn.
    const GRIND: Foreign = Foreign::Grind { len_us: 400_000 };

    /// A framed packet of `len` wire bytes; the content does not matter to
    /// the endpoint.
    fn wire(len: usize) -> Vec<u8> {
        alloc::vec![0x55; len]
    }

    /// The bench window's frames: 187 to 237 wire bytes, after a deposit
    /// whose main loop holds the executor for 400 ms a poll.
    #[test]
    fn a_frame_awaited_packet_by_packet_tears_at_offset_128_behind_a_busy_loop() {
        let bench = Bench::new();
        let clock = || bench.clock();
        let frame = wire(237);
        run_beside_main_loop(
            &bench,
            DEPOSIT,
            write_frame(&mut Port(&bench), &clock, 0, &frame),
        );
        let (hole, at) = bench.worst_hole();
        assert!(
            hole >= REFERENCE_FRAME_TIMEOUT_US,
            "the model must reproduce the bench's torn frame, hole={hole} us"
        );
        assert_eq!(at, 128, "and tear it where the bench saw it");
        assert!(hole <= BENCH_HOLE_US + 2 * HOST_TAKE_US, "hole={hole} us");
    }

    /// **The fix, on the firmware's bound.** The same frame beside the same
    /// main loop reaches the host without a hole the reference times out
    /// on; the holes are the host's own polls.
    #[test]
    fn a_frame_written_eagerly_reaches_the_host_whole_behind_a_busy_loop() {
        for (len, main) in [118, 128, 187, 237, 564, 1_130]
            .into_iter()
            .flat_map(|len| [(len, DEPOSIT), (len, GRIND)])
        {
            let bench = Bench::new();
            let clock = || bench.clock();
            let frame = wire(len);
            let spent = run_beside_main_loop(
                &bench,
                main,
                write_frame(&mut Port(&bench), &clock, PACKET_SPIN_US, &frame),
            );
            let (hole, _) = bench.worst_hole();
            assert!(
                hole <= 2 * HOST_TAKE_US + 2 * SPIN_STEP_US,
                "len={len}: hole={hole} us inside the frame"
            );
            assert!(hole < REFERENCE_FRAME_TIMEOUT_US);
            // What thread mode pays: about one host poll a packet.
            let packets = len.div_ceil(PACKET) as u64 + u64::from(len % PACKET == 0);
            assert!(
                spent <= packets * (HOST_TAKE_US + SPIN_STEP_US),
                "len={len}: the writer held thread mode {spent} us"
            );
            let sent: usize = bench.packets.borrow().iter().map(|&(_, l)| l).sum();
            assert_eq!(sent, len, "every byte goes out once");
        }
    }

    /// A frame of a multiple of 64 ends with a zero-length packet, which is
    /// written eagerly too: without it the host's read would wait for the
    /// next frame.
    #[test]
    fn a_frame_of_whole_packets_ends_with_a_zero_length_packet() {
        let bench = Bench::new();
        let clock = || bench.clock();
        let frame = wire(128);
        run_beside_main_loop(
            &bench,
            GRIND,
            write_frame(&mut Port(&bench), &clock, PACKET_SPIN_US, &frame),
        );
        let lens: Vec<usize> = bench.packets.borrow().iter().map(|&(_, l)| l).collect();
        assert_eq!(lens, [64, 64, 0]);
        let (hole, _) = bench.worst_hole();
        assert!(hole < REFERENCE_FRAME_TIMEOUT_US, "hole={hole} us");
    }

    /// A host that takes nothing costs thread mode the bound and no more:
    /// the writer hands the CPU back and finishes the frame when the host
    /// reads again, with nothing lost or sent twice.
    #[test]
    fn a_host_that_stops_reading_costs_the_bound_and_loses_nothing() {
        let bench = Bench::new();
        let clock = || bench.clock();
        let frame = wire(200);
        let mut port = Port(&bench);
        let mut writer = pin!(write_frame(&mut port, &clock, PACKET_SPIN_US, &frame));
        let mut cx = Context::from_waker(Waker::noop());
        // The host stops reading: the first packet goes into the empty
        // endpoint, the second spins the bound out and hands the CPU back.
        bench.host_reading.set(false);
        let before = bench.now_us.get();
        assert!(writer.as_mut().poll(&mut cx).is_pending());
        let spun = bench.now_us.get() - before;
        assert!(
            (PACKET_SPIN_US..=PACKET_SPIN_US + 2 * SPIN_STEP_US).contains(&spun),
            "spun {spun} us"
        );
        // Polled again while the host still reads nothing: no second spin,
        // the plain await waits.
        let before = bench.now_us.get();
        assert!(writer.as_mut().poll(&mut cx).is_pending());
        assert_eq!(bench.now_us.get(), before);
        // The host is back.
        bench.host_reading.set(true);
        for _ in 0..100 {
            if writer.as_mut().poll(&mut cx).is_ready() {
                break;
            }
            bench.now_us.set(bench.now_us.get() + HOST_TAKE_US);
        }
        let lens: Vec<usize> = bench.packets.borrow().iter().map(|&(_, l)| l).collect();
        assert_eq!(lens, [64, 64, 64, 8]);
    }
}
