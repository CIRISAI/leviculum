//! The board's message store: [`leviculum_record_log`] mounted on the region
//! `memory.x` reserves behind the firmware image (Codeberg #384).
//!
//! **Nothing is stored in it yet.** No LXMF, no propagation node, nothing
//! announced. What this module adds is the region, the mount, and one bench
//! instrument that writes synthetic records so the erase storm can be measured
//! under BLE and LoRa load. A board that mounts the store behaves exactly as
//! before on every other path, which is the whole requirement for shipping it:
//! the store is a consumer of flash and of the SoftDevice's flash scheduler,
//! and neither is touched until somebody asks it to append.
//!
//! # Where the region comes from
//!
//! Not from a constant here. `memory.x` carves `STORE` out of the top of the
//! application window and exports `__srecord_store` / `__erecord_store`;
//! [`region`] reads those two symbols and nothing else in the tree knows the
//! addresses. Moving or resizing the store is one edit in one file, and an
//! image that grows into it is a link error rather than a store quietly
//! sitting underneath the firmware (the `ASSERT`s in `memory.x`, plus
//! `scripts/check-nrf-store-gap.sh`, which prints the remaining gap for both
//! bins on every `just fast`).
//!
//! # Why the log lives in a task and is reached by channel
//!
//! Two reasons, one of them fatal.
//!
//! The fatal one: `nrf_softdevice::Flash`'s `write` and `erase` futures arm a
//! `DropBomb` (nrf-softdevice 5949a5b, `nrf-softdevice/src/flash.rs`) and
//! **panic** if they are dropped in flight. A caller that could cancel an
//! append — a `select!` with a timeout, a connection task that goes away — is
//! therefore a caller that could panic the board. Nothing that can be
//! cancelled may hold an append, so the log is owned by a task that does
//! nothing else and requests arrive on [`REQUESTS`]; the sender never waits
//! for the flash.
//!
//! The second: `Flash::take` is a singleton and the radio config (#349) and
//! the telemetry target (#236) already write through it. All three go through
//! the one [`crate::flash::SharedFlash`] mutex, which [`Device`] below takes
//! for the duration of a single flash operation — not for a whole append. A
//! store that held the lock across an append would stall a radio-config save
//! behind its erases, and an append that waited for the whole log would be an
//! append during which nothing else could persist anything.
//!
//! # What is logged
//!
//! One line at mount, ungated so it reaches a host that attaches late:
//!
//! ```text
//! STORE mount state=<ours|formatted> pages=<n> live=<n> free_bytes=<n> t=<ms>
//! ```
//!
//! then, at runtime, one line per failed flash operation and a summary
//! whenever a counter moved since the last one:
//!
//! ```text
//! STORE op_fail op=<erase|write> attempt=<n> t=<ms>
//! STORE stats appends=<n> fails=<n> sealed_pages=<n> t=<ms>
//! ```
//!
//! `fails` counts flash operations the SoftDevice refused (the `op_fail`
//! lines); `sealed_pages` counts pages given up because an append failed part
//! way through and the rest of that page can no longer be programmed. The
//! `t=<ms>` stamp is appended by [`crate::log`] to every line, so neither
//! format string carries one.

extern crate alloc;

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embedded_storage_async::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};
use leviculum_record_log::{Error as LogError, RecordLog, KEY_LEN, MAX_BODY};

/// Tag every record the bench instrument writes carries.
///
/// The record log reads no tag — it is one opaque byte (see that crate's
/// module docs) — so this is a convention between the instrument that writes
/// synthetic records and whatever purges them later. It exists so a measuring
/// run's records are distinguishable from a real message's without reading a
/// body: a later batch can sweep the region for `tag == TAG_BENCH` and purge
/// exactly those.
pub const TAG_BENCH: u8 = 0xB0;

/// The propagation role's tag map (`leviculum-pn-store`) must keep bench
/// records invisible: above the message clamp and distinct from the peer
/// tag, or a storm would masquerade as messages or peers.
const _: () = assert!(
    TAG_BENCH > leviculum_pn_store::MESSAGE_TAG_MAX && TAG_BENCH != leviculum_pn_store::TAG_PEER,
    "TAG_BENCH collides with the propagation store's tag map"
);

/// The largest synthetic record the bench instrument will write, and the bound
/// the control envelope refuses above
/// ([`leviculum_core::envelope::STORE_STORM_MAX_BYTES`]).
///
/// Tied to the envelope's number here rather than restated: the envelope is
/// what refuses a host's value, and this assertion is what stops that bound
/// from outgrowing the page a record has to fit in.
const _: () = assert!(
    leviculum_core::envelope::STORE_STORM_MAX_BYTES as usize <= MAX_BODY,
    "the control envelope would accept a storm record larger than a page can hold"
);

/// What a caller may ask the store task to do.
///
/// One variant, because nothing stores messages yet: the bench instrument is
/// the only producer of records on a board today. A message producer arrives
/// as another variant, not as another owner of the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Append `records` synthetic records of `size` body bytes each, tagged
    /// [`TAG_BENCH`] (#384 bench instrument, `--store-storm`). Bounded and
    /// refused while one is running — see [`request_storm`].
    Storm { records: u16, size: u16 },
}

/// Pending requests. Depth 1: a second storm while one runs is refused by
/// [`request_storm`] rather than queued, because a measurement that silently
/// ran twice is worse than one that was told no.
static REQUESTS: Channel<CriticalSectionRawMutex, Request, 1> = Channel::new();

/// One write the propagation engine owes the log (Codeberg #384 part 3):
/// the flush half of `leviculum-pn-store`'s queued ops. Bodies ride the
/// heap because a message body is up to a page.
pub enum PnOp {
    Append {
        key: [u8; KEY_LEN],
        time: u32,
        tag: u8,
        body: alloc::vec::Vec<u8>,
    },
    /// Purge the record at this **region-relative** offset iff its key
    /// still matches — the page may have been reclaimed since the caller
    /// scanned it, and purging whatever now sits there would corrupt a
    /// stranger record's commit word.
    Purge { offset: u32, key: [u8; KEY_LEN] },
}

/// What a completed [`PnOp`] reports back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PnDone {
    /// The log's `free_bytes` after the op — the engine's fill mirror.
    pub free_bytes: u32,
}

/// Depth 1 and one caller: the propagation engine flushes one op at a
/// time and awaits [`PN_DONE`] before the next, which is what keeps the
/// reply unambiguous without a ticket.
static PN_OPS: Channel<CriticalSectionRawMutex, PnOp, 1> = Channel::new();
static PN_DONE: Signal<CriticalSectionRawMutex, Result<PnDone, ()>> = Signal::new();

/// The log's `free_bytes` as of the mount / the last completed operation,
/// for callers that need the number without a round trip (the boot line,
/// the display).
static FREE_BYTES: AtomicU32 = AtomicU32::new(0);

/// Execute one propagation-store write on the log task.
///
/// `Err(())` means the op did not land: the store is not mounted, or the
/// flash refused it past the retry budget. The engine's contract with
/// `leviculum-pn-store` is exactly this bool — on an append failure it
/// un-remembers the id from the role's duplicate cache and sends no
/// proof; on a purge failure it drops the mask and the record reappears.
///
/// Must not be called concurrently with itself (single engine, main
/// loop); the await is safe to hold across the flash's own retries
/// because the caller runs in an arm of the main loop, never inside a
/// `select` that could drop it — and even a drop would only lose the
/// *reply*: the op itself completes on this task, which owns the log.
pub async fn pn_execute(op: PnOp) -> Result<PnDone, ()> {
    if !MOUNTED.load(Ordering::Relaxed) {
        return Err(());
    }
    PN_DONE.reset();
    PN_OPS.send(op).await;
    PN_DONE.wait().await
}

/// The log's `free_bytes` as of the last operation (see [`FREE_BYTES`]).
pub fn free_bytes_hint() -> u32 {
    FREE_BYTES.load(Ordering::Relaxed)
}

/// Set once the log is mounted. Until then there is nothing to append to and
/// [`request_storm`] refuses.
static MOUNTED: AtomicBool = AtomicBool::new(false);
/// Set while a storm is being executed.
static STORM_RUNNING: AtomicBool = AtomicBool::new(false);

/// Records successfully appended since boot.
static APPENDS: AtomicU32 = AtomicU32::new(0);
/// Flash operations the SoftDevice refused, retried or not.
static OP_FAILS: AtomicU32 = AtomicU32::new(0);
/// Pages given up because an append failed after its first program run.
static SEALED_PAGES: AtomicU32 = AtomicU32::new(0);

/// How many times one flash operation is attempted before the store gives up
/// on it.
///
/// The SoftDevice schedules flash work between radio events and fails the
/// operation outright when it finds no gap (S140 SDS, Flash API timing), so a
/// refusal is a statement about the next few milliseconds of radio traffic and
/// not about the part. Four attempts with a doubling delay walk off a busy
/// window; hammering at a fixed interval can sit inside one.
const FLASH_ATTEMPTS: u8 = 4;
/// Delay before the second attempt, doubled for each further one: 50, 100,
/// 200 ms.
const FLASH_BACKOFF_MS: u64 = 50;

/// How often the task prints [`stats`](self#what-is-logged), if anything moved.
const STATS_PERIOD_S: u64 = 300;

/// The store's region: `(base, len)` from the linker symbols `memory.x`
/// exports.
///
/// Absolute symbols — their *addresses* are the numbers — which is the same
/// shape `__sretained` is read with in [`crate::ble`].
pub fn region() -> (u32, u32) {
    extern "C" {
        static __srecord_store: u8;
        static __erecord_store: u8;
    }
    let base = core::ptr::addr_of!(__srecord_store) as u32;
    let end = core::ptr::addr_of!(__erecord_store) as u32;
    (base, end.saturating_sub(base))
}

/// Ask the store task to append `records` synthetic records of `size` bytes.
///
/// Never blocks and never touches flash on the caller's stack. `false` means
/// the request was refused and nothing will happen: the log is not mounted, a
/// storm is already running, or the slot is taken. The caller answers the host
/// with a refusal — an instrument that silently dropped a request would make
/// the next measurement a lie.
///
/// The bounds on `records` and `size` belong to
/// [`leviculum_core::envelope::classify_control_frame`], which refuses out-of-
/// range values by name on every binary; this function re-checks `size`
/// against what a page can hold so that a caller which is not the envelope
/// cannot get past it.
pub fn request_storm(records: u16, size: u16) -> bool {
    if !MOUNTED.load(Ordering::Relaxed) || STORM_RUNNING.load(Ordering::Relaxed) {
        return false;
    }
    if records == 0 || size as usize > MAX_BODY {
        return false;
    }
    REQUESTS.try_send(Request::Storm { records, size }).is_ok()
}

/// Whether a storm is running right now.
pub fn storm_running() -> bool {
    STORM_RUNNING.load(Ordering::Relaxed)
}

/// Whether the log is mounted.
pub fn mounted() -> bool {
    MOUNTED.load(Ordering::Relaxed)
}

/// The shared SoftDevice flash handle as a device the record log can own.
///
/// Takes the mutex per operation, which is what lets the log be mounted for
/// the lifetime of the task while the radio config and the telemetry target
/// keep writing their own pages between its operations. Safe because the three
/// stores own disjoint pages: the only thing the lock has to serialise is
/// access to the single `Flash` handle itself.
///
/// Writes and erases are retried with a doubling backoff and counted here,
/// where the kind of the operation is known. A retry is invisible to the log
/// above: by the time it sees an error, the operation has been refused
/// `FLASH_ATTEMPTS` times, which is what makes the log's page-sealing
/// response to a failure proportionate.
#[cfg(feature = "softdevice")]
pub struct Device {
    flash: &'static crate::flash::SharedFlash,
}

#[cfg(feature = "softdevice")]
type SdFlash = nrf_softdevice::Flash;

#[cfg(feature = "softdevice")]
impl Device {
    /// Borrow the shared handle.
    pub fn new(flash: &'static crate::flash::SharedFlash) -> Self {
        Self { flash }
    }
}

/// One operation with its retry loop.
///
/// A macro rather than a function taking a closure: each of the two operations
/// borrows the mutex guard for the duration of its own future, and expressing
/// that as a `FnMut` returning a future that borrows its argument needs
/// lending closures. The loop is the interesting part and it exists once.
#[cfg(feature = "softdevice")]
macro_rules! retried {
    ($self:expr, $op:literal, |$guard:ident| $call:expr) => {{
        let mut outcome = Ok(());
        let mut delay = FLASH_BACKOFF_MS;
        for attempt in 1..=FLASH_ATTEMPTS {
            outcome = {
                let mut $guard = $self.flash.lock().await;
                $call.await
            };
            if outcome.is_ok() {
                break;
            }
            OP_FAILS.fetch_add(1, Ordering::Relaxed);
            crate::log::log_fmt(
                "STORE ",
                format_args!("op_fail op={} attempt={}", $op, attempt),
            );
            if attempt < FLASH_ATTEMPTS {
                embassy_time::Timer::after(embassy_time::Duration::from_millis(delay)).await;
                delay *= 2;
            }
        }
        outcome
    }};
}

#[cfg(feature = "softdevice")]
impl ErrorType for Device {
    type Error = <SdFlash as ErrorType>::Error;
}

#[cfg(feature = "softdevice")]
impl ReadNorFlash for Device {
    const READ_SIZE: usize = <SdFlash as ReadNorFlash>::READ_SIZE;

    /// Not retried: internal flash is memory-mapped and the SoftDevice's own
    /// implementation is a `memcpy` that cannot fail or be refused.
    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let mut guard = self.flash.lock().await;
        <SdFlash as ReadNorFlash>::read(&mut guard, offset, bytes).await
    }

    fn capacity(&self) -> usize {
        // 1 MiB, the nRF52840's flash. `nrf_softdevice::Flash::capacity`
        // returns this same compile-time constant under its `nrf52840`
        // feature (nrf-softdevice 5949a5b, `nrf-softdevice/src/flash.rs`);
        // the trait method is synchronous, so the async mutex cannot be taken
        // to ask it, and restating the constant is the lesser evil. Getting it
        // wrong is a safe failure: the record log compares `base + len`
        // against it and refuses the region with `OutOfBounds`, which the
        // mount line reports.
        256 * 4096
    }
}

#[cfg(feature = "softdevice")]
impl NorFlash for Device {
    const WRITE_SIZE: usize = <SdFlash as NorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <SdFlash as NorFlash>::ERASE_SIZE;

    /// The record log erases exactly one page per call, so a retry of a
    /// refused erase cannot re-erase a page that already succeeded — which
    /// would spend an erase cycle the wear budget counts.
    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        debug_assert_eq!(to - from, Self::ERASE_SIZE as u32);
        retried!(self, "erase", |guard| <SdFlash as NorFlash>::erase(
            &mut *guard,
            from,
            to
        ))
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        retried!(self, "write", |guard| <SdFlash as NorFlash>::write(
            &mut *guard,
            offset,
            bytes
        ))
    }
}

/// `nrf_softdevice::Flash` declares it (a word takes a second write as long as
/// it only clears bits), and [`RecordLog::purge`] needs it.
#[cfg(feature = "softdevice")]
impl MultiwriteNorFlash for Device {}

/// Mount the store and serve requests for the lifetime of the board.
///
/// Mounts first, and formats only a region no log of ours wrote: `mount`
/// answers `None` for an unformatted region *and* for somebody else's data,
/// and either way the one erase that follows is this board's decision about
/// its own flash. Nothing above this task can ask for a format.
#[cfg(feature = "softdevice")]
#[embassy_executor::task]
pub async fn store_task(flash: &'static crate::flash::SharedFlash) {
    let (base, len) = region();
    let mut log = match mount(flash, base, len).await {
        Some(log) => log,
        // No store this boot. The task ends rather than spinning, `MOUNTED`
        // stays false, and every request is refused with a reason instead of
        // being dropped into a channel nothing reads.
        None => return,
    };
    FREE_BYTES.store(log.free_bytes(), Ordering::Relaxed);
    MOUNTED.store(true, Ordering::Relaxed);

    let mut ticker = embassy_time::Ticker::every(embassy_time::Duration::from_secs(STATS_PERIOD_S));
    let mut reported = (0u32, 0u32, 0u32);
    loop {
        use embassy_futures::select::{select3, Either3};
        // The only place this task selects, and therefore the only place a
        // future of it is ever dropped: all arms are cheap and cancel-safe,
        // and an append is never inside one. See the module docs on the
        // `DropBomb`.
        match select3(REQUESTS.receive(), PN_OPS.receive(), ticker.next()).await {
            Either3::First(Request::Storm { records, size }) => {
                STORM_RUNNING.store(true, Ordering::Relaxed);
                storm(&mut log, records, size).await;
                STORM_RUNNING.store(false, Ordering::Relaxed);
                FREE_BYTES.store(log.free_bytes(), Ordering::Relaxed);
            }
            Either3::Second(op) => {
                let outcome = pn_perform(&mut log, base, op).await;
                FREE_BYTES.store(log.free_bytes(), Ordering::Relaxed);
                PN_DONE.signal(outcome);
            }
            Either3::Third(()) => {}
        }
        let now = (
            APPENDS.load(Ordering::Relaxed),
            OP_FAILS.load(Ordering::Relaxed),
            SEALED_PAGES.load(Ordering::Relaxed),
        );
        // Silent while nothing happens: a board that never appends has
        // nothing to summarise, and a debug capture of a long run should not
        // have to scroll past it.
        if now != reported {
            crate::log::log_fmt(
                "STORE ",
                format_args!(
                    "stats appends={} fails={} sealed_pages={}",
                    now.0, now.1, now.2
                ),
            );
            reported = now;
        }
    }
}

/// Perform one propagation-store write (Codeberg #384 part 3).
///
/// The append counts into the same [`APPENDS`] the storms count into; a
/// failure counts a sealed page exactly as a storm's does. The purge
/// re-validates the record at its offset before touching its commit word
/// — a reclaimed page answers "no record" or a different key, and the op
/// then reports success with nothing to do, because a record that is
/// already gone is what the purge wanted.
#[cfg(feature = "softdevice")]
async fn pn_perform(log: &mut RecordLog<Device>, base: u32, op: PnOp) -> Result<PnDone, ()> {
    match op {
        PnOp::Append {
            key,
            time,
            tag,
            body,
        } => {
            let room_before = log.sector_room();
            match log.append(&key, time, tag, &body).await {
                Ok(_) => {
                    APPENDS.fetch_add(1, Ordering::Relaxed);
                    Ok(PnDone {
                        free_bytes: log.free_bytes(),
                    })
                }
                Err(err) => {
                    if room_before > 0 && log.sector_room() == 0 {
                        SEALED_PAGES.fetch_add(1, Ordering::Relaxed);
                    }
                    crate::log::log_fmt(
                        "STORE ",
                        format_args!("pn_append_failed reason={}", reason(&err)),
                    );
                    Err(())
                }
            }
        }
        PnOp::Purge { offset, key } => {
            let at = base.saturating_add(offset);
            match log.record_at(at).await {
                Ok(Some(record)) if record.key == key => match log.purge(&record).await {
                    Ok(()) => Ok(PnDone {
                        free_bytes: log.free_bytes(),
                    }),
                    Err(err) => {
                        crate::log::log_fmt(
                            "STORE ",
                            format_args!("pn_purge_failed reason={}", reason(&err)),
                        );
                        Err(())
                    }
                },
                Ok(_) => Ok(PnDone {
                    free_bytes: log.free_bytes(),
                }),
                Err(err) => {
                    crate::log::log_fmt(
                        "STORE ",
                        format_args!("pn_purge_failed reason={}", reason(&err)),
                    );
                    Err(())
                }
            }
        }
    }
}

/// Mount the region, formatting it once if it is not ours, and say which of
/// the two happened.
///
/// `None` means the store is unavailable for this boot and is the one outcome
/// that leaves [`MOUNTED`] false — every control request is then refused
/// rather than silently dropped.
#[cfg(feature = "softdevice")]
async fn mount(
    flash: &'static crate::flash::SharedFlash,
    base: u32,
    len: u32,
) -> Option<RecordLog<Device>> {
    let state;
    let mut log = match RecordLog::mount(Device::new(flash), base, len).await {
        Ok(Some(log)) => {
            state = "ours";
            log
        }
        Ok(None) => {
            state = "formatted";
            // `mount` took the device by value and dropped it along with its
            // `None`, so the format builds a second one — free, because a
            // `Device` is a borrowed reference. It costs one erase and one
            // 12-byte page header. A fresh board and a board whose region
            // holds a stranger's bytes take the same path on purpose: the
            // region is inside our own image window, so there is no stranger
            // whose data we would be protecting, and `mount` is still the
            // first call so the decision to write is this module's rather
            // than a side effect of opening.
            match RecordLog::open(Device::new(flash), base, len).await {
                Ok(log) => log,
                Err(err) => {
                    log_mount_failure(&err);
                    return None;
                }
            }
        }
        Err(err) => {
            log_mount_failure(&err);
            return None;
        }
    };

    // One scan of the region, reads only: what is already on the part after a
    // reboot, which is the number that says whether the mount found anything.
    let (live, _purged) = match log.count().await {
        Ok(counts) => counts,
        Err(err) => {
            log_mount_failure(&err);
            return None;
        }
    };
    crate::log::log_fmt_critical(
        "STORE ",
        format_args!(
            "mount state={} pages={} live={} free_bytes={}",
            state,
            log.sectors(),
            live,
            log.free_bytes()
        ),
    );
    Some(log)
}

#[cfg(feature = "softdevice")]
fn log_mount_failure(err: &LogError<<SdFlash as ErrorType>::Error>) {
    crate::log::log_fmt_critical(
        "STORE ",
        format_args!("mount failed reason={}", reason(err)),
    );
}

/// Why a store operation failed, as one stable token per cause.
#[cfg(feature = "softdevice")]
fn reason(err: &LogError<<SdFlash as ErrorType>::Error>) -> &'static str {
    match err {
        LogError::Flash(_) => "flash",
        LogError::BodyTooLarge => "body-too-large",
        LogError::BadRegion => "bad-region",
        LogError::OutOfBounds => "out-of-bounds",
        LogError::UnsupportedGeometry => "geometry",
    }
}

/// Append `records` synthetic records of `size` bytes, tagged [`TAG_BENCH`].
///
/// The instrument for the erase-storm measurement: N records of a known size
/// is N × `stride` bytes of programming and one erase per page it fills, under
/// whatever BLE and LoRa load the board is carrying at the time. Nothing is
/// persisted as configuration and nothing is repeated — a storm happens once,
/// when asked.
#[cfg(feature = "softdevice")]
async fn storm(log: &mut RecordLog<Device>, records: u16, size: u16) {
    let start = embassy_time::Instant::now();
    let mut body = [0xA5u8; leviculum_core::envelope::STORE_STORM_MAX_BYTES as usize];
    let size = core::cmp::min(size as usize, body.len());
    let mut appended = 0u16;
    let mut failed = 0u16;

    for n in 0..records {
        // The key is the record's place in the storm, so a later reader can
        // order the synthetic records without parsing a body; the log itself
        // reads neither field.
        let mut key = [0u8; KEY_LEN];
        key[0..2].copy_from_slice(&n.to_be_bytes());
        key[2..4].copy_from_slice(&(size as u16).to_be_bytes());
        // Bytes that depend on the record index, so two synthetic records are
        // distinguishable and a CRC failure cannot hide behind every record
        // being identical.
        if size > 0 {
            body[0] = n as u8;
            body[size - 1] = (n >> 8) as u8;
        }

        let room_before = log.sector_room();
        // Uptime, not wall time: the timestamp is opaque to the log, a bench
        // record is only ever read back against the same boot, and a board
        // with no GNSS fix has no calendar to offer.
        let time = embassy_time::Instant::now().as_millis() as u32;
        match log.append(&key, time, TAG_BENCH, &body[..size]).await {
            Ok(_) => {
                APPENDS.fetch_add(1, Ordering::Relaxed);
                appended += 1;
            }
            Err(err) => {
                failed += 1;
                if room_before > 0 && log.sector_room() == 0 {
                    SEALED_PAGES.fetch_add(1, Ordering::Relaxed);
                }
                crate::log::log_fmt(
                    "STORE ",
                    format_args!("append_failed record={} reason={}", n, reason(&err)),
                );
            }
        }
    }

    crate::log::log_fmt(
        "STORE ",
        format_args!(
            "storm records={} size={} appended={} failed={} seq={} ms={}",
            records,
            size,
            appended,
            failed,
            log.sequence(),
            start.elapsed().as_millis()
        ),
    );
}

/// Spawn the store task, borrowing the shared SoftDevice flash handle.
///
/// Call after `Softdevice::enable`, beside the radio-config and telemetry
/// store tasks: writing internal flash with the SoftDevice live is only legal
/// through its own syscalls.
#[cfg(feature = "softdevice")]
pub fn spawn_store_task(
    spawner: &embassy_executor::Spawner,
    flash: &'static crate::flash::SharedFlash,
) {
    spawner.must_spawn(store_task(flash));
}
