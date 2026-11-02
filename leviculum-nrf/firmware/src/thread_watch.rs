//! Where thread mode was when it stopped, said by the executor that
//! outlives it (order 1297, Refs #359).
//!
//! On 2026-10-31 a board froze four seconds into a boot with its LoRa task
//! still receiving for another 55 s. The LoRa task runs on an interrupt
//! executor above thread mode; everything else, the USB tasks and the main
//! loop included, runs on the thread executor. The dying boot's persistent
//! tail held LoRa lines only, and no `[STACK] tag=tick`, which a task that
//! does nothing but wait two seconds on a timer prints. A task parked on an
//! `await` (a CDC write the host no longer reads, a full channel) cannot
//! silence that tick; only thread mode spinning in a call that never
//! returns can. The tail could not say which call, because nothing that
//! still ran knew.
//!
//! This is the witness. Thread mode [`Witness::beat`]s from that same tick
//! task, and names the blocking calls it is about to make with
//! [`Witness::enter`] / [`Witness::leave`] (the panel's flush and power
//! writes, which are synchronous bus transfers on some boards). The LoRa
//! task, when its hand-off to the main loop finds the channel full, asks
//! [`Witness::stall`] and writes one `THREAD_STALL` line into the
//! persistent tail: how long thread mode has not beaten, and the site it
//! was in. One line per stall: a second report needs a beat in between.
//!
//! All state is in atomics, read and written from either executor. The
//! millisecond stamps are `u32` and compared with wrapping arithmetic, so
//! the 49-day wrap costs at most one wrong age, never a panic.

use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Sender, TrySendError};

/// How long thread mode may go without a beat before a full channel is
/// reported as its stall. The beat comes every 2 s; 5 s is two missed
/// beats and a half, well past any scheduling delay a busy main loop
/// causes, and short enough that the four-slot LoRa channel of a board
/// receiving every few seconds fills well after it.
pub const STALL_AFTER_MS: u32 = 5_000;

/// A blocking call thread mode announces before it makes it.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Site {
    /// No named call in progress: thread mode was somewhere else.
    None = 0,
    /// The status panel's frame push.
    PanelFlush = 1,
    /// The status panel's power write.
    PanelPower = 2,
}

impl Site {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::PanelFlush,
            2 => Self::PanelPower,
            _ => Self::None,
        }
    }

    /// The `site=` value of the `THREAD_STALL` line.
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::PanelFlush => "panel-flush",
            Self::PanelPower => "panel-power",
        }
    }
}

/// One stall, as the line reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stall {
    /// Milliseconds since thread mode's last beat (since boot if it never
    /// beat).
    pub beat_age_ms: u32,
    /// The site thread mode was in.
    pub site: Site,
    /// Milliseconds since it entered that site; 0 for [`Site::None`].
    pub site_age_ms: u32,
}

/// Thread mode's last beat and current site.
pub struct Witness {
    beat_ms: AtomicU32,
    site: AtomicU8,
    site_since_ms: AtomicU32,
    /// The beat a stall was last reported against, plus one so that 0
    /// means "none reported" even for a stall before the first beat.
    reported: AtomicU32,
}

impl Default for Witness {
    fn default() -> Self {
        Self::new()
    }
}

impl Witness {
    /// A witness that has seen no beat and no site.
    pub const fn new() -> Self {
        Self {
            beat_ms: AtomicU32::new(0),
            site: AtomicU8::new(Site::None as u8),
            site_since_ms: AtomicU32::new(0),
            reported: AtomicU32::new(0),
        }
    }

    /// Thread mode ran at `now_ms`.
    pub fn beat(&self, now_ms: u32) {
        self.beat_ms.store(now_ms, Ordering::Release);
    }

    /// Thread mode is about to make the blocking call `site`.
    pub fn enter(&self, site: Site, now_ms: u32) {
        self.site_since_ms.store(now_ms, Ordering::Relaxed);
        self.site.store(site as u8, Ordering::Release);
    }

    /// The call named by the last [`enter`](Self::enter) returned.
    pub fn leave(&self) {
        self.site.store(Site::None as u8, Ordering::Release);
    }

    /// Whether thread mode has stalled as of `now_ms`, once per stall:
    /// `Some` when no beat came for [`STALL_AFTER_MS`] and this stall was
    /// not reported yet.
    pub fn stall(&self, now_ms: u32) -> Option<Stall> {
        let beat = self.beat_ms.load(Ordering::Acquire);
        let beat_age_ms = now_ms.wrapping_sub(beat);
        if beat_age_ms < STALL_AFTER_MS {
            return None;
        }
        let key = beat.wrapping_add(1);
        if self.reported.swap(key, Ordering::AcqRel) == key {
            return None;
        }
        let site = Site::from_u8(self.site.load(Ordering::Acquire));
        let site_age_ms = match site {
            Site::None => 0,
            _ => now_ms.wrapping_sub(self.site_since_ms.load(Ordering::Relaxed)),
        };
        Some(Stall {
            beat_age_ms,
            site,
            site_age_ms,
        })
    }
}

/// The board's one witness: thread mode beats and names its sites here.
pub static THREAD: Witness = Witness::new();

/// [`THREAD`] beats now. Called by the board's two-second stack tick task,
/// which does nothing but wait on a timer, so its silence is thread mode's.
pub fn beat_now() {
    THREAD.beat(embassy_time::Instant::now().as_millis() as u32);
}

/// Hand `value` to `tx`, waiting as `send` does. When the channel is full,
/// first ask [`THREAD`] whether thread mode has stalled and, if so, write
/// the [`log_stall`] line naming `waiting`: a full channel is its consumer
/// not taking, and if thread mode has not beaten either, the caller's
/// executor may be the only one left to say where it stopped. The wait
/// itself is unchanged.
pub async fn send_watched<T, const N: usize>(
    tx: &Sender<'_, CriticalSectionRawMutex, T, N>,
    value: T,
    waiting: &str,
) {
    if let Err(TrySendError::Full(value)) = tx.try_send(value) {
        let now = embassy_time::Instant::now().as_millis() as u32;
        if let Some(stall) = THREAD.stall(now) {
            log_stall(&stall, waiting);
        }
        tx.send(value).await;
    }
}

/// The `THREAD_STALL` line for `stall`, `waiting` naming what the reporter
/// found full (`lora`: the LoRa task's hand-off to the main loop).
///
/// ```text
/// THREAD_STALL beat_age_ms=38114 site=panel-flush site_age_ms=38102 waiting=lora t=42616
/// ```
pub fn log_stall(stall: &Stall, waiting: &str) {
    crate::log::log_fmt(
        "",
        format_args!(
            "THREAD_STALL beat_age_ms={} site={} site_age_ms={} waiting={}",
            stall.beat_age_ms,
            stall.site.name(),
            stall.site_age_ms,
            waiting
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_beating_thread_mode_is_not_a_stall() {
        let w = Witness::new();
        w.beat(10_000);
        assert_eq!(w.stall(10_000 + STALL_AFTER_MS - 1), None);
    }

    #[test]
    fn a_missing_beat_is_reported_once_with_the_site_it_was_in() {
        let w = Witness::new();
        w.beat(4_000);
        w.enter(Site::PanelFlush, 4_100);
        let s = w.stall(42_616);
        assert_eq!(
            s,
            Some(Stall {
                beat_age_ms: 38_616,
                site: Site::PanelFlush,
                site_age_ms: 38_516,
            })
        );
        // The same stall, asked again on the next full channel: silent.
        assert_eq!(w.stall(59_911), None);
    }

    #[test]
    fn a_new_beat_arms_the_next_report() {
        let w = Witness::new();
        w.beat(1_000);
        assert!(w.stall(9_000).is_some());
        w.beat(20_000);
        let s = w.stall(30_000);
        assert_eq!(
            s.map(|s| (s.beat_age_ms, s.site)),
            Some((10_000, Site::None))
        );
    }

    #[test]
    fn a_site_left_is_not_blamed() {
        let w = Witness::new();
        w.beat(0);
        w.enter(Site::PanelPower, 1);
        w.leave();
        let s = w.stall(STALL_AFTER_MS);
        assert_eq!(s.map(|s| (s.site, s.site_age_ms)), Some((Site::None, 0)));
    }

    #[test]
    fn a_stall_before_the_first_beat_is_reported_too() {
        let w = Witness::new();
        let s = w.stall(STALL_AFTER_MS + 3);
        assert_eq!(s.map(|s| s.beat_age_ms), Some(STALL_AFTER_MS + 3));
        assert_eq!(w.stall(STALL_AFTER_MS + 4), None);
    }

    #[test]
    fn the_wrap_gives_an_age_not_a_panic() {
        let w = Witness::new();
        w.beat(u32::MAX - 1_000);
        let s = w.stall(STALL_AFTER_MS);
        assert_eq!(s.map(|s| s.beat_age_ms), Some(STALL_AFTER_MS + 1_001));
    }

    #[test]
    fn every_site_has_its_own_name() {
        let names = [Site::None, Site::PanelFlush, Site::PanelPower].map(Site::name);
        assert_eq!(names, ["none", "panel-flush", "panel-power"]);
        for v in 0..=2u8 {
            assert_eq!(Site::from_u8(v) as u8, v);
        }
    }
}
