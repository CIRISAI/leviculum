//! The UI core parks itself for a flash op and polls the receive FIFOs
//! while it waits (1613, #474): no byte the host or the GNSS receiver
//! sends is lost to the op's masked window.
//!
//! # The mechanism
//!
//! A flash write or erase runs with core 0's interrupts off — esp-storage
//! wraps every ROM call in its own interrupt-masking lock
//! (`esp-storage-0.10.0/src/lib.rs`, `maybe_with_critical_section`), so
//! the mask is not this crate's to lift — and with the cache off, which
//! is why the other core must not run from flash while the op does. One
//! sector erase is 35 to 54 ms on the board (window 23, `park_max_ms`),
//! the design bound 57 ms; the UART FIFO holds 128 bytes, 11 ms of the
//! line. Until 1613 the other core was *hardware-stalled* (esp-storage's
//! AutoPark, an RTC clock stall): no CPU anywhere could take the FIFO,
//! and what the line carried past 128 bytes was dropped — 1 to 2 of 100
//! injected key frames in windows 22 and 23, every one inside an erase.
//!
//! Now core 0 asks the UI core to park itself: a software interrupt
//! ([`arm`] binds it on core 1) whose handler acknowledges, clears its
//! own INTENABLE, and spins in internal RAM polling UART0's and UART1's
//! receive FIFOs into two [`ParkRing`]s until core 0 says the op is
//! done. One popper is always live: core 0's interrupt whenever core 0's
//! interrupts are on, the parked spin exactly while they are off. The
//! handoffs keep the byte order — core 0 masks its own interrupts and
//! sweeps the FIFO before the park, harvests the rings before it unmasks
//! after — and `leviculum_serial_mux::park`'s host model pins the whole
//! timeline at line rate across a modelled 57 ms window.
//!
//! # Why the spin is safe where AutoPark had to stall
//!
//! The handler is *entered* while the cache is still on (core 0 waits
//! for the ack before it starts the op), so esp-hal's interrupt dispatch
//! — which reads tables in flash — runs on readable flash. From the ack
//! to the done store the spin touches only its own code and literals
//! (placed in internal RAM, `#[esp_hal::ram]`, and checked call-free by
//! `scripts/build-m9-image.sh`), the two rings and the handshake words
//! (internal `.bss`), and peripheral registers. INTENABLE is zero for
//! the whole spin, so no other interrupt can dispatch through flash
//! while the cache is off; it is restored, and the handler returns, only
//! after core 0 flipped the request word back — which it does after the
//! op, when esp-storage has restored the cache.
//!
//! # Why this cannot deadlock where the 2026-11-07 hang could
//!
//! The hang's shape (1534) was a core hardware-stalled while holding the
//! cross-core critical section. Cooperative parking removes the shape:
//! `critical_section`'s acquire disables core 1's interrupts first, so
//! the park interrupt cannot land inside a held section — the ack comes
//! after the section ends, and core 0 holds no lock while it waits for
//! the ack (its own interrupts are off, but the global section stays
//! free). A core 1 that never acks — not armed yet, or wedged — is
//! hardware-stalled after `ACK_WAIT_US` exactly as AutoPark stalled
//! it, under the global critical section so it cannot be holding it,
//! and the fallback is counted ([`stats`]).

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use esp_hal::interrupt::software::SoftwareInterrupt;
use esp_hal::peripherals::CPU_CTRL;
use esp_hal::system::{is_running, Cpu, CpuControl};
use esp_sync::raw::{RawLock, SingleCoreInterruptLock};
use leviculum_serial_mux::park::{ParkRing, GNSS_RING, HOST_RING};

/// The software interrupt the park rides on: FROM_CPU_INTR2, bound on
/// core 1 by [`arm`], raised by [`hold`] on core 0. 0, 1 and 3 stay
/// free for whatever esp-rtos-style executors claim first.
pub const PARK_INTR: u8 = 2;

/// How long core 0 waits for the ack before it falls back to the
/// hardware stall: 20 ms. The ack waits at most for core 1's longest
/// stretch with interrupts off — a critical section or a pool lock,
/// microseconds — so a miss means core 1 is not armed or not well.
const ACK_WAIT_US: u64 = 20_000;

/// How long core 0 waits for the spin to confirm it stopped after the
/// unpark: it is spinning on the request word in internal RAM, so this
/// is generous; a miss is counted, never waited out again.
const DONE_WAIT_US: u64 = 10_000;

/// The park request: odd while a hold wants core 1 parked, even
/// otherwise, increasing. The spin acks and exits by value, so a stale
/// interrupt (a fallback's late delivery) sees an even word and leaves.
static REQ: AtomicU32 = AtomicU32::new(0);
/// The spin's "I am in internal RAM, polling": the request's value.
static ACK: AtomicU32 = AtomicU32::new(0);
/// The spin's "I stopped, the rings are yours": the request's value.
static DONE: AtomicU32 = AtomicU32::new(0);
/// Whether [`arm`] ran on core 1.
static ARMED: AtomicBool = AtomicBool::new(false);

/// Cooperative parks, fallback stalls, and done-waits that timed out.
static COOP: AtomicU32 = AtomicU32::new(0);
static FALLBACK: AtomicU32 = AtomicU32::new(0);
static LATE: AtomicU32 = AtomicU32::new(0);

/// What the host sent while core 0's interrupts were off: harvested
/// into the UART keep by [`hold`] before the interrupt pops anything
/// newer.
static HOST: ParkRing<HOST_RING> = ParkRing::new();
/// The GNSS receiver's bytes of the same window.
static GNSS: ParkRing<GNSS_RING> = ParkRing::new();

/// (cooperative parks, fallback stalls, late done-waits) so far.
pub fn stats() -> (u32, u32, u32) {
    (
        COOP.load(Ordering::Relaxed),
        FALLBACK.load(Ordering::Relaxed),
        LATE.load(Ordering::Relaxed),
    )
}

/// Whether [`arm`] ran: the boot waits for it before its restore's
/// first flash op (1622).
pub fn armed() -> bool {
    ARMED.load(Ordering::Acquire)
}

/// Bind the park interrupt on the core that calls this — the UI core,
/// first thing after its start, before it waits for the boot's restore
/// (1622). From here on every flash op parks core 1
/// cooperatively instead of stalling it.
pub fn arm(mut park: SoftwareInterrupt<'static, 2>) {
    // The handler resets the latch through its own register writes;
    // the driver object has nothing left to do (its type has no Drop
    // to unbind anything), so it ends here.
    park.set_interrupt_handler(park_ipi);
    ARMED.store(true, Ordering::Release);
}

/// Run the flash op `f` with core 0's interrupts off and core 1 parked:
/// cooperatively when [`arm`] answered, by the hardware stall when not.
/// Returns the op's result and the microseconds spent waiting for the
/// ack (0 for a fallback or a single-core run).
///
/// The FIFOs should be swept into their keeps right before this
/// (`drain_rx_fifo`, `gnss_uart::drain`), so the window starts empty;
/// the rings are harvested into the same keeps before the interrupts
/// come back, which keeps the byte streams in order.
pub fn hold<R>(f: impl FnOnce() -> R) -> (R, u64) {
    if !is_running(Cpu::AppCpu) {
        // Boot, before the UI core starts: nothing else runs from
        // flash, esp-storage's own lock is all the op needs. Since 1622
        // only a new board's first identity save comes here; the
        // restore runs after the UI core armed the park.
        return (f(), 0);
    }
    // Core 0's interrupts off for the whole hold: the UART interrupt
    // stops popping before the spin starts (one popper at a time), and
    // stays off until the rings are harvested (order).
    // SAFETY: exited below, same core, the one token in flight here.
    let token = unsafe { SingleCoreInterruptLock.enter() };
    let gen = REQ.load(Ordering::Relaxed).wrapping_add(1) | 1;
    REQ.store(gen, Ordering::Release);
    let mut acked = false;
    let mut ack_wait = 0u64;
    if ARMED.load(Ordering::Acquire) {
        raise_park_intr();
        let t0 = crate::uptime_us();
        loop {
            if ACK.load(Ordering::Acquire) == gen {
                acked = true;
                ack_wait = crate::uptime_us().saturating_sub(t0);
                break;
            }
            if crate::uptime_us().saturating_sub(t0) > ACK_WAIT_US {
                break;
            }
            core::hint::spin_loop();
        }
    }
    let r = if acked {
        COOP.fetch_add(1, Ordering::Relaxed);
        f()
    } else {
        // Not armed, or core 1 did not answer: stall it as AutoPark
        // would, under the global critical section so it cannot be
        // holding that section while stalled (1534's hang shape).
        FALLBACK.fetch_add(1, Ordering::Relaxed);
        critical_section::with(|_| {
            // SAFETY: CPU_CTRL is only otherwise held by the boot's
            // `start_app_core` guard, which no longer touches it; the
            // stall registers are written from this core only.
            let mut ctrl = CpuControl::new(unsafe { CPU_CTRL::steal() });
            // SAFETY: this core is ProCpu; the parked core is the other.
            unsafe { ctrl.park_core(Cpu::AppCpu) };
            let r = f();
            ctrl.unpark_core(Cpu::AppCpu);
            r
        })
    };
    REQ.store(gen.wrapping_add(1), Ordering::Release);
    // The spin may have entered late, during a fallback: wait for its
    // done word whenever its ack is there.
    if ACK.load(Ordering::Acquire) == gen {
        let t0 = crate::uptime_us();
        loop {
            if DONE.load(Ordering::Acquire) == gen {
                break;
            }
            if crate::uptime_us().saturating_sub(t0) > DONE_WAIT_US {
                LATE.fetch_add(1, Ordering::Relaxed);
                break;
            }
            core::hint::spin_loop();
        }
    }
    // The window's bytes, ahead of whatever the FIFO holds now: the
    // UART interrupts are still off, so nothing newer can pass these.
    HOST.drain(crate::absorb_parked);
    GNSS.drain(crate::gnss_uart::absorb_parked);
    // SAFETY: the token `enter` gave above, on this core.
    unsafe { SingleCoreInterruptLock.exit(token) };
    (r, ack_wait)
}

/// Host bytes the full ring counted instead of keeping, ever.
pub fn host_ring_lost() -> u32 {
    HOST.lost()
}

/// GNSS bytes the full ring counted instead of keeping, ever.
pub fn gnss_ring_lost() -> u32 {
    GNSS.lost()
}

/// Raise FROM_CPU_INTR2 on whatever core enabled it (core 1, [`arm`]).
fn raise_park_intr() {
    // SAFETY: the one writer of this latch's set side; the handler is
    // the one writer of its clear side.
    let system = unsafe { &*esp32s3::SYSTEM::ptr() };
    system
        .cpu_intr_from_cpu(PARK_INTR as usize)
        .write(|w| w.cpu_intr().set_bit());
}

/// The park interrupt's handler, on the UI core: ack, then poll the
/// receive FIFOs from internal RAM until the op is over.
///
/// `#[esp_hal::ram]` and call-free between the ack and the done store
/// (checked against the linked ELF by `scripts/build-m9-image.sh`): the
/// cache is off for most of that stretch, so a single call or constant
/// read into flash would hang or fault the core. INTENABLE is zero for
/// the same stretch, so no other handler can dispatch through flash
/// tables either. Entry and return run with the cache on — core 0 waits
/// for the ack before the op and flips the request only after it.
#[esp_hal::ram]
#[esp_hal::handler(priority = esp_hal::interrupt::Priority::Priority2)]
fn park_ipi() {
    // SAFETY: the clear side of the latch, this handler alone.
    let system = unsafe { &*esp32s3::SYSTEM::ptr() };
    system
        .cpu_intr_from_cpu(PARK_INTR as usize)
        .write(|w| w.cpu_intr().clear_bit());
    let gen = REQ.load(Ordering::Acquire);
    if gen & 1 == 0 {
        // A stale delivery: the hold fell back and finished already.
        return;
    }
    // Nothing may dispatch while the cache is off: everything, this
    // level included, masked until the spin is over. In-service
    // execution continues regardless of INTENABLE.
    let saved = esp_hal::xtensa_lx::interrupt::disable();
    // SAFETY: sole readers of these FIFOs while parked — core 0 masked
    // its interrupts before raising this one.
    let uart0 = unsafe { &*esp32s3::UART0::ptr() };
    let uart1 = unsafe { &*esp32s3::UART1::ptr() };
    ACK.store(gen, Ordering::Release);
    while REQ.load(Ordering::Acquire) == gen {
        while uart0.status().read().rxfifo_cnt().bits() > 0 {
            let byte = uart0.fifo().read().rxfifo_rd_byte().bits();
            // SAFETY: the parked spin is the ring's one writer, between
            // this ack and the done store below.
            unsafe { HOST.push(byte) };
        }
        while uart1.status().read().rxfifo_cnt().bits() > 0 {
            let byte = uart1.fifo().read().rxfifo_rd_byte().bits();
            // SAFETY: as above.
            unsafe { GNSS.push(byte) };
        }
    }
    DONE.store(gen, Ordering::Release);
    // SAFETY: restoring what `disable` saved, in the same handler.
    unsafe { esp_hal::xtensa_lx::interrupt::enable_mask(saved) };
}
