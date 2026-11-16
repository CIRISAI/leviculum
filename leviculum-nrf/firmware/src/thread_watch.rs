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
//! Besides the site, thread mode records its PHASE ([`Phase`], order 1641):
//! the main-loop arm or propagation-engine step it last entered, set on
//! entry and never cleared until the loop goes back to its `select`. A site
//! names a blocking call; a phase names the work a stall or a failed
//! allocation happened inside, which on 2026-11-10 was the one thing the
//! T114's four `memory allocation of 3456 bytes failed` panics and its
//! `THREAD_STALL ... site=none` could not say. The phase rides the stall
//! line as `phase=` and the panic record's message as a trailing
//! ` phase=<name>` (`crate::postmortem::record_panic`).
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

/// The work thread mode last entered: a main-loop arm or a step of the
/// propagation engine. Set on entry with [`set_phase`] (or [`phase_scope`]
/// around a synchronous section of another task, which restores the phase
/// it found), reset to [`Phase::Idle`] when the main loop goes back to its
/// `select`. Unlike a [`Site`] it is not cleared on the way out of the
/// work, so a panic or a stall after the work returned still names the last
/// arm thread mode was in, which is what a post-mortem needs.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The main loop is parked in its `select`; whatever ran was another
    /// task of the thread executor.
    Idle = 0,
    /// The serial (USB) reception arm: `handle_packet` on interface 0.
    SerRx,
    /// The LoRa reception arm.
    LoraRx,
    /// The BLE reception arm.
    BleRx,
    /// A BLE peer transition.
    BlePeer,
    /// The core's timer arm (`handle_timeout`).
    Timer,
    /// A host control envelope (wall time, telemetry target, position,
    /// announce request).
    HostCtl,
    /// The telemetry tick.
    Telemetry,
    /// The periodic or button announce.
    Announce,
    /// The heap census walk.
    Census,
    /// The propagation engine digesting one dispatch's core events that
    /// none of the named engine phases below covers.
    PnEvents,
    /// The engine handling an announce: a propagation node's (peer table,
    /// recall) or any other (the active-delivery trigger).
    PnAnnounce,
    /// The engine handling link data, a request or a response on a link.
    PnLinkData,
    /// The engine handling a resource advertisement, completion or failure:
    /// the sync intake and the client upload.
    PnResource,
    /// The engine's periodic jobs at the top of `settle` (announce,
    /// maintenance, idle links, stats, path requests).
    PnTicks,
    /// The engine's sync scheduler: picking a peer, opening a round,
    /// planning an offer.
    PnSyncRound,
    /// One queued work item (`perform`: upload, `/get`, `/offer`).
    PnWork,
    /// One step of an inbound sync batch's drain.
    PnSyncIntake,
    /// The engine flushing its store adapters' queued writes.
    PnFlush,
    /// The USB serial task deframing what the host sent.
    SerialIngress,
    /// Inside the BLE peer arm (order 1665): the core's peer-up or peer-lost
    /// handling (path cull, peer-up pulls, the stored-announce re-offer).
    BlePeerCore,
    /// Inside the BLE peer arm: our own announce to the peer that came up.
    BlePeerAnnounce,
    /// Inside the BLE peer arm: the propagation role's peer-up (its
    /// announce and the stored-mail solicit, which walks the store).
    BlePeerSolicit,
    /// Inside the BLE peer arm: handing its actions to the interfaces.
    BlePeerDispatch,
    /// A main-loop arm's core events before the propagation engine sees
    /// them: the event lines, the `/status` and `/dfu` responders.
    CoreEvents,
}

impl Phase {
    /// Every phase, in discriminant order.
    pub const ALL: [Phase; 25] = [
        Self::Idle,
        Self::SerRx,
        Self::LoraRx,
        Self::BleRx,
        Self::BlePeer,
        Self::Timer,
        Self::HostCtl,
        Self::Telemetry,
        Self::Announce,
        Self::Census,
        Self::PnEvents,
        Self::PnAnnounce,
        Self::PnLinkData,
        Self::PnResource,
        Self::PnTicks,
        Self::PnSyncRound,
        Self::PnWork,
        Self::PnSyncIntake,
        Self::PnFlush,
        Self::SerialIngress,
        Self::BlePeerCore,
        Self::BlePeerAnnounce,
        Self::BlePeerSolicit,
        Self::BlePeerDispatch,
        Self::CoreEvents,
    ];

    fn from_u8(v: u8) -> Self {
        Self::ALL.get(usize::from(v)).copied().unwrap_or(Self::Idle)
    }

    /// The `phase=` value.
    pub fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::SerRx => "ser-rx",
            Self::LoraRx => "lora-rx",
            Self::BleRx => "ble-rx",
            Self::BlePeer => "ble-peer",
            Self::Timer => "timer",
            Self::HostCtl => "host-ctl",
            Self::Telemetry => "telemetry",
            Self::Announce => "announce",
            Self::Census => "census",
            Self::PnEvents => "pn-events",
            Self::PnAnnounce => "pn-announce",
            Self::PnLinkData => "pn-link-data",
            Self::PnResource => "pn-resource",
            Self::PnTicks => "pn-ticks",
            Self::PnSyncRound => "pn-sync-round",
            Self::PnWork => "pn-work",
            Self::PnSyncIntake => "pn-sync-intake",
            Self::PnFlush => "pn-flush",
            Self::SerialIngress => "serial-ingress",
            Self::BlePeerCore => "ble-peer-core",
            Self::BlePeerAnnounce => "ble-peer-announce",
            Self::BlePeerSolicit => "ble-peer-solicit",
            Self::BlePeerDispatch => "ble-peer-dispatch",
            Self::CoreEvents => "core-events",
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
    /// The phase thread mode was in.
    pub phase: Phase,
}

/// Thread mode's last beat and current site.
pub struct Witness {
    beat_ms: AtomicU32,
    site: AtomicU8,
    site_since_ms: AtomicU32,
    phase: AtomicU8,
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
            phase: AtomicU8::new(Phase::Idle as u8),
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

    /// Thread mode entered `phase`.
    pub fn set_phase(&self, phase: Phase) {
        self.phase.store(phase as u8, Ordering::Relaxed);
    }

    /// Thread mode entered `phase`; returns the phase it was in, for a
    /// caller that restores it ([`PhaseScope`]).
    pub fn swap_phase(&self, phase: Phase) -> Phase {
        Phase::from_u8(self.phase.swap(phase as u8, Ordering::Relaxed))
    }

    /// The phase thread mode last entered.
    pub fn phase(&self) -> Phase {
        Phase::from_u8(self.phase.load(Ordering::Relaxed))
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
            phase: self.phase(),
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

/// [`THREAD`] entered `phase`. Also restarts the allocator's per-phase
/// counters ([`crate::heap::phase_entered`], order 1665), so a refused
/// allocation names what the phase it died in had taken.
pub fn set_phase(phase: Phase) {
    THREAD.set_phase(phase);
    crate::heap::phase_entered();
}

/// The phase [`THREAD`] last entered; the panic handler's read.
pub fn current_phase() -> Phase {
    THREAD.phase()
}

/// A phase held for one synchronous section and restored when dropped.
/// For a task other than the main loop: it must not leave the main loop's
/// phase overwritten when it yields back. Never hold one across an
/// `await`.
#[must_use = "the phase is restored when the scope drops"]
pub struct PhaseScope {
    previous: Phase,
}

/// Enter `phase` until the returned scope drops.
pub fn phase_scope(phase: Phase) -> PhaseScope {
    PhaseScope {
        previous: THREAD.swap_phase(phase),
    }
}

impl Drop for PhaseScope {
    fn drop(&mut self) {
        THREAD.set_phase(self.previous);
    }
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
/// THREAD_STALL beat_age_ms=38114 site=panel-flush site_age_ms=38102 phase=pn-work waiting=lora t=42616
/// ```
pub fn log_stall(stall: &Stall, waiting: &str) {
    crate::log::log_fmt(
        "",
        format_args!(
            "THREAD_STALL beat_age_ms={} site={} site_age_ms={} phase={} waiting={}",
            stall.beat_age_ms,
            stall.site.name(),
            stall.site_age_ms,
            stall.phase.name(),
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
                phase: Phase::Idle,
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

    #[test]
    fn a_stall_names_the_phase_thread_mode_last_entered() {
        let w = Witness::new();
        w.beat(0);
        w.set_phase(Phase::PnWork);
        let s = w.stall(STALL_AFTER_MS);
        assert_eq!(s.map(|s| s.phase), Some(Phase::PnWork));
    }

    #[test]
    fn a_swapped_phase_hands_back_the_one_it_replaced() {
        let w = Witness::new();
        w.set_phase(Phase::PnWork);
        assert_eq!(w.swap_phase(Phase::SerialIngress), Phase::PnWork);
        assert_eq!(w.phase(), Phase::SerialIngress);
    }

    #[test]
    fn every_phase_has_its_own_name_and_round_trips() {
        for (i, p) in Phase::ALL.iter().enumerate() {
            assert_eq!(*p as u8 as usize, i);
            assert_eq!(Phase::from_u8(i as u8), *p);
        }
        let mut names: alloc::vec::Vec<&str> = Phase::ALL.iter().map(|p| p.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Phase::ALL.len());
        assert_eq!(Phase::from_u8(200), Phase::Idle);
    }
}
