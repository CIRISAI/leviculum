#![no_std]
//! The LoRa transmit path's channel-access policy: when a packet that is
//! ready to go may actually key the radio.
//!
//! Two decisions live here, extracted from `lora_task` so a host test can
//! drive them against scripted radios and seeded randomness:
//!
//! 1. **Acquisition jitter** — a randomised pre-TX wait, spent listening,
//!    before the first CAD of a channel acquisition. Two senders whose
//!    transmissions are triggered by the same event (a co-started probe
//!    announce, a rebroadcast of the same received frame) reach their
//!    radios phase-locked; CAD alone cannot separate them, because both
//!    probe a channel on which neither has keyed yet. The jitter de-tiles
//!    them so at most one is inside the other's CAD window.
//!
//! 2. **The CAD retry gate** — the bounded listen-before-talk state
//!    machine around the SX1262's channel-activity detection: clear
//!    transmits, busy backs off a random contention window that doubles
//!    per retry, and after [`CAD_MAX_RETRIES`] attempts the frame is
//!    transmitted anyway. The give-up is deliberate: the layers above
//!    assume the interface eventually transmits, so a frame must not
//!    starve behind a busy channel forever. (The reference firmware
//!    instead waits indefinitely and lets its airtime lock be the only
//!    bound; our bounded forced TX is a deviation, justified because a
//!    silent starvation is indistinguishable from packet loss to every
//!    layer above.)
//!
//! # Reference
//!
//! The jitter's parameters are the RNode firmware's CSMA band-1 (idle
//! channel) draw, taken from the vendored source
//! (`reference/RNode_Firmware`):
//!
//! * every transmission waits DIFS = `CSMA_SIFS_MS + 2 * csma_slot_ms`
//!   with SIFS = 0, i.e. two slots (Config.h:102, Config.h:119), then a
//!   contention window of `random(cw_min, cw_max)` slots
//!   (RNode_Firmware.ino:1625-1627, Arduino `random` upper-exclusive);
//!   at ≤ 7 % channel airtime the band-1 window is `cw_min = 0`,
//!   `cw_max = band * CSMA_CW_PER_BAND_WINDOWS - 1` = 14
//!   (Config.h:108-111, RNode_Firmware.ino:1614-1617), so the
//!   idle-channel draw is uniform over 0..=13 slots. The reference
//!   reaches that value only by transiting a band: `cw_max` is
//!   *declared* as `CSMA_CW_PER_BAND_WINDOWS` itself, 15
//!   (Config.h:127), and `update_csma_parameters` rewrites it only when
//!   the band CHANGES (RNode_Firmware.ino:1614), which band 1 never
//!   does from boot. A reference board therefore draws over 15 slots
//!   until its first excursion above 7 % airtime and over 14 after it.
//!   We mirror the post-excursion window; the difference is one slot,
//!   and which of the two is the right target is open on Codeberg #347;
//! * the slot is 12 symbol times, clamped to at most 100 ms and at least
//!   24 ms — or 6 ms when the modulation runs faster than 30 kbps
//!   (Config.h:104-107, Utilities.h:1244-1252, bitrate Utilities.h:1237);
//! * there is **no host-visible way to disable this**: `tx_queue_handler`
//!   (RNode_Firmware.ino:1623) runs it for every queued packet.
//!
//! Only the band-1 draw is mirrored, not the airtime-scaled band
//! escalation: the escalation widens the window under sustained load,
//! which is what the retry gate's doubling contention window already does
//! here, reacting to observed CAD-busy instead of to an airtime average.

/// Maximum CAD attempts before a frame is transmitted even though the
/// channel appears busy — the documented give-up that keeps a frame from
/// starving forever (see the crate docs for why this deviates from the
/// reference's unbounded wait).
pub const CAD_MAX_RETRIES: u8 = 8;
/// Initial contention window (slots) for the CAD backoff. Starting at 2
/// guarantees a non-zero-slot random choice on the first retry so two
/// nodes that simultaneously detect traffic desynchronize meaningfully.
pub const CAD_CW_INITIAL: u8 = 2;
/// Maximum contention window (slots) after exponential back-off.
///
/// The reference has no doubling window; it widens a fixed one by airtime
/// band, `cw_max = band * CSMA_CW_PER_BAND_WINDOWS - 1`, so its widest draw
/// is band 4's 45..=58 slots (Config.h:108-111,
/// RNode_Firmware.ino:1614-1617). Ours is 0..=63, one band-width above that
/// ceiling and reached by observed CAD-busy rather than by an airtime
/// average — the substitution the crate docs above describe. In slot times
/// the two ceilings are 5.8 s and 6.3 s at the 100 ms slot, which is what
/// makes the escalation comparable rather than merely analogous.
pub const CAD_CW_MAX: u8 = 64;

/// DIFS in slots: `CSMA_SIFS_MS + 2 * csma_slot_ms` with SIFS = 0
/// (reference Config.h:102, Config.h:119).
pub const JITTER_DIFS_SLOTS: u64 = 2;
/// Number of equally likely jitter draws: band-1 `random(0, 14)` is
/// uniform over 0..=13 slots (reference Config.h:108-111,
/// RNode_Firmware.ino:1626, Arduino `random` upper-exclusive).
pub const JITTER_CW_SLOTS: u32 = 14;
/// One jitter slot is 12 symbol times (reference Config.h:107).
pub const JITTER_SLOT_SYMBOLS: u64 = leviculum_core::rnode::CSMA_SLOT_SYMBOLS;
/// Slot ceiling in ms (reference Config.h:104).
pub const JITTER_SLOT_MAX_MS: u64 = leviculum_core::rnode::CSMA_SLOT_MAX_MS;
/// Slot floor in ms (reference Config.h:105).
pub const JITTER_SLOT_MIN_MS: u64 = leviculum_core::rnode::CSMA_SLOT_MIN_MS;
/// Slot floor at fast rates: `CSMA_SLOT_MIN_MS - CSMA_SLOT_MIN_FAST_DELTA`
/// = 24 - 18 (reference Config.h:105-106, Utilities.h:1246).
pub const JITTER_SLOT_MIN_FAST_MS: u64 =
    leviculum_core::rnode::CSMA_SLOT_MIN_MS - leviculum_core::rnode::CSMA_SLOT_MIN_FAST_DELTA;
/// Rates above this count as fast (reference Config.h:87,
/// `LORA_FAST_THRESHOLD_BPS`).
pub const JITTER_FAST_THRESHOLD_BPS: u64 = leviculum_core::rnode::LORA_FAST_THRESHOLD_BPS as u64;

/// The reference's CSMA slot for a modulation: 12 symbol times clamped to
/// `[24, 100]` ms, floor 6 ms at rates above 30 kbps (Utilities.h:
/// 1244-1252).
///
/// The derivation itself is [`leviculum_core::rnode::csma_slot_ms`], where it
/// sits beside the airtime and preamble arithmetic it shares its reference
/// lines with, is pinned against a literal float transcription of those
/// lines over every PHY the reference admits, and is mirrored into
/// `periculum-wire`. It is reached through this name because the slot is a
/// channel-access quantity to every caller here, and because there must be
/// exactly one of it: the CAD backoff in the firmware's transmit path used
/// to derive a second, unclamped slot of its own from a 500-byte airtime,
/// 27 times this one at SF12 (Codeberg #147).
pub fn jitter_slot_ms(bw_hz: u32, sf: u8, cr_denom: u8) -> u64 {
    leviculum_core::rnode::csma_slot_ms(bw_hz, sf, cr_denom)
}

/// The widest wait a node can owe before it keys up on a fresh channel
/// acquisition: DIFS plus the top of the contention window, at its
/// modulation ([`JITTER_DIFS_SLOTS`], [`JITTER_CW_SLOTS`],
/// [`jitter_slot_ms`]).
///
/// This is the quantity anyone sizing a *listening* window against a peer
/// has to budget, because it is the latest the peer can key up — not the
/// latest it can finish a frame. The draw is uniform over the window, so
/// this is a ceiling reached once per `JITTER_CW_SLOTS` acquisitions and
/// not a typical wait; the mean is DIFS plus half the window.
pub fn widest_acquisition_wait_ms(bw_hz: u32, sf: u8, cr_denom: u8) -> u64 {
    (JITTER_DIFS_SLOTS + JITTER_CW_SLOTS as u64 - 1) * jitter_slot_ms(bw_hz, sf, cr_denom)
}

/// Ceiling on the post-TX receive window, in ms. A bound on how long one
/// yield may keep the transmit path off its queue when the channel is
/// genuinely silent; it binds only at SF12/125 kHz, where one reply
/// airtime alone is 9.3 s.
pub const POST_TX_WINDOW_MAX_MS: u64 = 10_000;

/// How long the transmit path listens after a transmission before it
/// drains the next outgoing frame: one reply airtime plus the peer's whole
/// turnaround.
///
/// `reply_airtime_ms` is a full single-frame reply on the wire at the live
/// modulation, and `host_margin_ms` the peer's host-side processing before
/// it starts contending (`PACING_MARGIN_MS`); the caller supplies both
/// because the airtime formula and that constant live in `leviculum-core`.
/// The modulation is taken rather than a slot, because the slot the peer
/// draws in is this crate's [`jitter_slot_ms`] and no other — passing one
/// in is how the term below came to be wrong.
///
/// **The turnaround is the peer's widest time to KEY UP, not to finish its
/// frame** ([`widest_acquisition_wait_ms`]): the receiver stops its
/// timeout on preamble detect and then runs to packet completion whatever
/// the length (`SET_STOP_RX_TIMER_ON_PREAMBLE`,
/// `leviculum-nrf/src/sx1262.rs`), so a reply that starts inside the
/// window is heard in full even when it ends outside it.
///
/// Until Codeberg #423 the turnaround budgeted the peer's DIFS and nothing
/// else — two slots of the *backoff* slot the CAD gate uses, written
/// before the peer had a contention window to draw at all. It was covered
/// at every slow PHY only by the reply-airtime term's slack, and at
/// SF7/500 kHz there was not enough of that slack: 270 ms of window
/// against a peer that can wait 360 ms, so a peer drawing the top of its
/// window keyed up after the board had already left the window. The
/// coverage is now structural — the term the window must cover is a
/// summand of the window — instead of an accident of how much airtime the
/// modulation happens to charge.
pub fn post_tx_rx_window_ms(
    reply_airtime_ms: u64,
    host_margin_ms: u64,
    bw_hz: u32,
    sf: u8,
    cr_denom: u8,
) -> u32 {
    let turnaround = host_margin_ms + widest_acquisition_wait_ms(bw_hz, sf, cr_denom);
    // Clamp to >= 1 ms: the SX1262 reads a zero timeout as "no timeout".
    reply_airtime_ms
        .saturating_add(turnaround)
        .clamp(1, POST_TX_WINDOW_MAX_MS) as u32
}

/// What the transmit path must do next, after asking the gate about a CAD
/// outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Key the radio. `forced` is true when the retry budget ran out with
    /// the channel still busy; `retries` is the attempt count for the
    /// `[LORA_CSMA_TX]` line.
    Transmit { forced: bool, retries: u8 },
    /// Listen for this many backoff slots (caller multiplies by the CSMA
    /// slot, [`ChannelAccess::jitter_slot`]), then CAD again.
    Backoff { slots: u64 },
    /// CAD itself failed; retry it immediately. Bounded by the same
    /// retry budget as busy, so a radio that cannot CAD still transmits.
    Retry,
}

/// The channel-access state one LoRa transmit path owns: the seeded
/// randomness, the owed acquisition jitter, and the CAD retry gate.
///
/// Everything is a function of the seed and the fed-in CAD outcomes, so a
/// host test scripts a radio ("busy, busy, clear") and asserts the exact
/// decisions.
#[derive(Debug, Clone)]
pub struct ChannelAccess {
    rng: u32,
    jitter_slot_ms: u64,
    jitter_owed: bool,
    jitter_remaining_ms: u64,
    jitter_was_drawn: bool,
    attempt: u8,
    cw: u8,
}

/// xorshift32 step. The all-zero state is a fixed point, which is why the
/// constructor refuses a zero seed.
fn xorshift32(state: &mut u32) -> u32 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    *state
}

impl ChannelAccess {
    /// A fresh access state. `seed` must come from per-board entropy (the
    /// hardware RNG): two boards seeded identically draw identical jitter
    /// and identical backoffs at identical draw counts, which re-creates
    /// exactly the phase lock the jitter exists to break. A zero seed is
    /// replaced (xorshift32's zero state is a fixed point).
    ///
    /// Boot counts as a released channel: the very first transmission owes
    /// jitter, because co-started boards are the canonical phase-locked
    /// senders.
    pub const fn new(seed: u32) -> Self {
        Self {
            rng: if seed == 0 { 0x9E37_79B9 } else { seed },
            jitter_slot_ms: JITTER_SLOT_MIN_MS,
            jitter_owed: true,
            jitter_remaining_ms: 0,
            jitter_was_drawn: false,
            attempt: 0,
            cw: CAD_CW_INITIAL,
        }
    }

    /// Adopt a modulation. Called at radio bring-up and on every runtime
    /// reconfiguration, beside the backoff slot recomputation.
    pub fn set_phy(&mut self, bw_hz: u32, sf: u8, cr_denom: u8) {
        self.jitter_slot_ms = jitter_slot_ms(bw_hz, sf, cr_denom);
    }

    /// The jitter slot in force, for the `[LORA_JITTER]` line.
    pub const fn jitter_slot(&self) -> u64 {
        self.jitter_slot_ms
    }

    /// The channel was yielded back (a post-TX listening window ran, or is
    /// about to): the next acquisition owes jitter again. Any remainder
    /// of a previous, abandoned draw is dropped — a new acquisition draws
    /// afresh rather than inheriting a leftover.
    pub fn channel_released(&mut self) {
        self.jitter_owed = true;
        self.jitter_remaining_ms = 0;
    }

    /// A new packet starts contending: reset the retry gate. Does not
    /// touch the owed jitter — a burst continuation is a new packet but
    /// not a new acquisition.
    pub fn begin_packet(&mut self) {
        self.attempt = 0;
        self.cw = CAD_CW_INITIAL;
    }

    /// The randomised pre-TX wait this acquisition still owes, in ms: on
    /// the first ask after a channel release, DIFS plus a uniform draw of
    /// 0..=13 slots ([`JITTER_DIFS_SLOTS`], [`JITTER_CW_SLOTS`]); on every
    /// later ask, whatever is LEFT of that draw after the caller has
    /// reported what it actually listened through ([`Self::jitter_spent`]),
    /// and `0` once the whole wait has been served.
    ///
    /// Asking is not serving. The caller spends the wait listening, so a
    /// peer that keys during it is heard rather than talked over — but the
    /// listen returns early on a reception, and a wait that ends that way
    /// has de-tiled nothing: the frame that ended it releases every other
    /// waiting node at the same instant, which is the phase lock the draw
    /// exists to break. Handing the value out therefore cannot discharge
    /// the debt; only listening it through can.
    pub fn acquisition_jitter_ms(&mut self) -> u64 {
        if self.jitter_owed {
            self.jitter_owed = false;
            let draw = (xorshift32(&mut self.rng) % JITTER_CW_SLOTS) as u64;
            self.jitter_remaining_ms = (JITTER_DIFS_SLOTS + draw) * self.jitter_slot_ms;
            self.jitter_was_drawn = true;
        } else {
            self.jitter_was_drawn = false;
        }
        self.jitter_remaining_ms
    }

    /// Report how long the caller actually listened through the wait
    /// [`Self::acquisition_jitter_ms`] last handed it. Wall-clock elapsed
    /// is the right figure and may overshoot the window; the debt
    /// saturates at zero.
    pub fn jitter_spent(&mut self, ms: u64) {
        self.jitter_remaining_ms = self.jitter_remaining_ms.saturating_sub(ms);
    }

    /// Whether the last [`Self::acquisition_jitter_ms`] was a fresh draw
    /// rather than the remainder of one already part-served. Only the log
    /// line reads this, so a run's records distinguish an acquisition's
    /// one draw from the windows that resume it.
    pub const fn jitter_was_drawn(&self) -> bool {
        self.jitter_was_drawn
    }

    /// The retry count so far, for the `[LORA_CAD]` log lines.
    pub const fn retries(&self) -> u8 {
        self.attempt
    }

    /// The channel was probed and found clear: transmit.
    pub fn cad_clear(&mut self) -> Verdict {
        Verdict::Transmit {
            forced: false,
            retries: self.attempt,
        }
    }

    /// The channel was probed and found busy: back off a random number of
    /// slots inside the doubling contention window — or transmit anyway
    /// once the retry budget is spent (the documented give-up).
    pub fn cad_busy(&mut self) -> Verdict {
        self.attempt += 1;
        if self.attempt >= CAD_MAX_RETRIES {
            return Verdict::Transmit {
                forced: true,
                retries: self.attempt,
            };
        }
        let slots = (xorshift32(&mut self.rng) as u64) % (self.cw as u64);
        self.cw = core::cmp::min(self.cw.saturating_mul(2), CAD_CW_MAX);
        Verdict::Backoff { slots }
    }

    /// The CAD operation itself errored: count it against the same budget
    /// and retry immediately (no backoff listen — the radio, not the
    /// channel, refused), forcing the TX once the budget is spent so a
    /// radio that cannot CAD still transmits.
    pub fn cad_error(&mut self) -> Verdict {
        self.attempt += 1;
        if self.attempt >= CAD_MAX_RETRIES {
            return Verdict::Transmit {
                forced: true,
                retries: self.attempt,
            };
        }
        Verdict::Retry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- The reference slot, checked against hand-derived values -------

    #[test]
    fn the_slot_matches_the_reference_at_the_bench_phy() {
        // SF7/BW125: symbol 1.024 ms, 12 symbols = 12.288 ms, bitrate
        // 5468 bps (not fast) -> clamped up to the 24 ms floor. This is
        // the corpus-wide bench PHY, so it is the value the acceptance
        // ladder runs under.
        assert_eq!(jitter_slot_ms(125_000, 7, 5), 24);
    }

    #[test]
    fn the_slot_scales_with_the_symbol_time_between_the_clamps() {
        // SF9/BW125: symbol 4.096 ms -> 49.152 ms -> 49.
        assert_eq!(jitter_slot_ms(125_000, 9, 5), 49);
        // SF10/BW125: symbol 8.192 ms -> 98.304 ms -> 98, just under the
        // ceiling.
        assert_eq!(jitter_slot_ms(125_000, 10, 5), 98);
    }

    #[test]
    fn the_slot_ceiling_binds_at_slow_modulations() {
        // SF12/BW125: 12 symbols are 393 ms; the reference caps the slot
        // at 100 ms (Config.h:104).
        assert_eq!(jitter_slot_ms(125_000, 12, 5), 100);
        assert_eq!(jitter_slot_ms(125_000, 11, 8), 100);
    }

    #[test]
    fn the_fast_rate_floor_is_6_ms() {
        // SF5/BW500: bitrate 5*4*500000/(5*32) = 62500 bps > 30 kbps,
        // symbol 0.064 ms, 12 symbols = 0.768 ms -> fast floor 6 ms
        // (Config.h:105-106).
        assert_eq!(jitter_slot_ms(500_000, 5, 5), 6);
    }

    #[test]
    fn the_widest_acquisition_wait_is_a_function_of_the_modulation() {
        // The table `docs/src/concepts/csma-transmit-window.md` quotes for
        // question 1 of Codeberg #347, pinned where it is computed rather
        // than transcribed into prose that cannot go red. Each row is
        // DIFS plus the widest draw, i.e. the ceiling a caller sizes a
        // delivery window with; the mean is DIFS plus half the window.
        //
        // The span the rows cover is the argument: 90 ms at SF5/500 kHz
        // against 1500 ms at SF12/125 kHz, a factor of 16 that no
        // millisecond constant tracks. A change to the slot derivation,
        // the DIFS width or the number of draws moves at least one row.
        let widest = widest_acquisition_wait_ms;
        let narrowest = |bw, sf, cr| JITTER_DIFS_SLOTS * jitter_slot_ms(bw, sf, cr);

        // SF7 and SF8 at 125 kHz sit on the 24 ms slot floor together, so
        // the bench PHY and the project default draw the same window.
        assert_eq!((narrowest(125_000, 7, 5), widest(125_000, 7, 5)), (48, 360));
        assert_eq!((narrowest(125_000, 8, 5), widest(125_000, 8, 5)), (48, 360));
        // Between the clamps the window tracks the symbol time.
        assert_eq!((narrowest(125_000, 9, 5), widest(125_000, 9, 5)), (98, 735));
        assert_eq!(
            (narrowest(125_000, 10, 5), widest(125_000, 10, 5)),
            (196, 1470)
        );
        // At SF12 the slot ceiling binds, so the window stops growing.
        assert_eq!(
            (narrowest(125_000, 12, 5), widest(125_000, 12, 5)),
            (200, 1500)
        );
        // Above 30 kbps the fast floor binds instead.
        assert_eq!((narrowest(500_000, 5, 5), widest(500_000, 5, 5)), (12, 90));

        // The reference's own inter-frame gap, measured off the air during
        // Codeberg #344, had a median of 205 ms over 81 gaps at the bench
        // PHY. The window's median draw is DIFS plus half of 13 slots,
        // which is what that measurement is a measurement OF: a model that
        // missed it by a slot would put this at 180 or 228.
        let median_ms = JITTER_DIFS_SLOTS * jitter_slot_ms(125_000, 7, 5)
            + (JITTER_CW_SLOTS as u64 - 1) * jitter_slot_ms(125_000, 7, 5) / 2;
        assert_eq!(median_ms, 204);
    }

    #[test]
    fn a_degenerate_modulation_cannot_divide_by_zero() {
        // A corrupt flash page decodes to whatever it decodes to; the
        // slot function must stay total.
        assert_eq!(jitter_slot_ms(0, 7, 5), JITTER_SLOT_MAX_MS);
        assert_eq!(jitter_slot_ms(125_000, 0, 5), JITTER_SLOT_MAX_MS);
        assert_eq!(jitter_slot_ms(125_000, 7, 0), JITTER_SLOT_MAX_MS);
        assert_eq!(jitter_slot_ms(125_000, 40, 5), JITTER_SLOT_MAX_MS);
    }

    // --- The post-TX receive window (Codeberg #423) ---------------------

    /// The reply the post-TX window is sized against: a full single-frame
    /// LoRa packet on the wire, header plus the largest unsplit payload,
    /// at the programmed preamble the reference derives for the PHY.
    ///
    /// Computed from `leviculum-core` rather than transcribed, so the rows
    /// below are the numbers the firmware's own call produces and cannot
    /// drift from the airtime formula the radio is charged with. A config
    /// may override the preamble (`preamble_symbols`,
    /// `leviculum-std/src/interfaces/serial.rs`), which moves the rows but
    /// not the coverage: the peer's turnaround is a summand of the window,
    /// so any airtime at all still covers it.
    fn reply_airtime_ms(bw: u32, sf: u8, cr: u8) -> u64 {
        let bytes = (leviculum_core::rnode::MAX_SINGLE_PAYLOAD + 1) as u32;
        let preamble = leviculum_core::rnode::derive_preamble_symbols(sf, cr, bw);
        leviculum_core::rnode::airtime_ms_with_preamble(bytes, bw, sf, cr, preamble)
    }

    /// The window `leviculum-nrf/src/lora.rs` opened before #423: one reply
    /// airtime plus `PACING_MARGIN_MS` plus two slots of the *backoff*
    /// slot, `max(24, airtime(500)/10)`, which was the CAD gate's own slot
    /// and not the one a peer draws its contention window in. Reconstructed
    /// history twice over: that second slot is gone since #147 and the gate
    /// now counts in [`jitter_slot_ms`] like everything else.
    fn window_before_423(bw: u32, sf: u8, cr: u8) -> u64 {
        let backoff_slot = core::cmp::max(
            JITTER_SLOT_MIN_MS,
            leviculum_core::rnode::airtime_ms(500, bw, sf, cr) / 10,
        );
        (reply_airtime_ms(bw, sf, cr) + leviculum_core::rnode::PACING_MARGIN_MS + 2 * backoff_slot)
            .clamp(1, POST_TX_WINDOW_MAX_MS)
    }

    #[test]
    fn budgeting_only_the_peers_difs_closed_the_window_before_it_could_key_up() {
        // The defect of Codeberg #423, as the arithmetic states it. At
        // SF7/500 kHz the pre-#423 window was 270 ms and a peer's widest
        // wait before it keys up is 360 ms, so a peer that drew the top of
        // its contention window keyed after the board had stopped
        // listening. Nothing about the peer is hypothetical here: it draws
        // the window THIS crate hands its own transmit path.
        assert_eq!(window_before_423(500_000, 7, 5), 270);
        assert_eq!(widest_acquisition_wait_ms(500_000, 7, 5), 360);
        assert!(window_before_423(500_000, 7, 5) < widest_acquisition_wait_ms(500_000, 7, 5));

        // And it is the fast PHYs only, which is why the defect survived a
        // corpus that runs at SF7..SF12/125 kHz: there the airtime term is
        // large enough to cover the missing contention window by accident.
        // SF7/250 kHz is the last row that still covers, and by 36 ms.
        assert!(window_before_423(125_000, 7, 5) > widest_acquisition_wait_ms(125_000, 7, 5));
        assert_eq!(
            window_before_423(250_000, 7, 5) - widest_acquisition_wait_ms(250_000, 7, 5),
            36
        );

        // What the term should have been, at the one PHY that broke: the
        // peer's whole wait, not its DIFS.
        assert!(
            post_tx_rx_window_ms(
                reply_airtime_ms(500_000, 7, 5),
                leviculum_core::rnode::PACING_MARGIN_MS,
                500_000,
                7,
                5,
            ) as u64
                >= widest_acquisition_wait_ms(500_000, 7, 5)
        );
    }

    #[test]
    fn the_post_tx_window_is_one_reply_plus_the_peers_whole_turnaround() {
        // The table `docs/src/concepts/csma-transmit-window.md` quotes for
        // question 4, pinned where it is computed. Each row is one reply
        // airtime plus 100 ms of peer host processing plus the peer's
        // widest wait, so a change to any of the three moves a row.
        let window = |bw, sf, cr| {
            post_tx_rx_window_ms(
                reply_airtime_ms(bw, sf, cr),
                leviculum_core::rnode::PACING_MARGIN_MS,
                bw,
                sf,
                cr,
            )
        };
        assert_eq!(window(125_000, 7, 5), 876);
        assert_eq!(window(125_000, 8, 5), 1188);
        assert_eq!(window(125_000, 9, 5), 2127);
        assert_eq!(window(125_000, 10, 5), 3948);
        // At SF12 one reply airtime alone is 9348 ms, so the ceiling binds.
        assert_eq!(window(125_000, 12, 5), POST_TX_WINDOW_MAX_MS as u32);
        assert_eq!(window(250_000, 7, 5), 680);
        assert_eq!(window(500_000, 7, 5), 582);
        assert_eq!(window(500_000, 5, 5), 231);
    }

    #[test]
    fn the_post_tx_window_covers_the_peers_keyup_at_every_modulation() {
        // The property the row-by-row table is one sample of, and the
        // reason the term is a summand rather than a margin that happens
        // to be large enough: the window cannot be shorter than the peer's
        // host turnaround plus the widest wait it can draw, at any
        // modulation a config can ask for.
        let margin = leviculum_core::rnode::PACING_MARGIN_MS;
        for &bw in &[7_800u32, 62_500, 125_000, 250_000, 500_000] {
            for sf in 5..=12u8 {
                for cr in 5..=8u8 {
                    let owed = margin + widest_acquisition_wait_ms(bw, sf, cr);
                    let window =
                        post_tx_rx_window_ms(reply_airtime_ms(bw, sf, cr), margin, bw, sf, cr)
                            as u64;
                    assert!(
                        window >= owed,
                        "bw={bw} sf={sf} cr={cr}: window {window} < owed {owed}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_windows_ceiling_cannot_cut_into_the_peers_turnaround() {
        // The one way the coverage above could stop holding is the clamp,
        // so the ceiling is pinned against the widest turnaround any
        // modulation can produce: DIFS plus the window at the 100 ms slot
        // ceiling, plus the host margin. 1600 ms against 10000 ms.
        let widest_possible = leviculum_core::rnode::PACING_MARGIN_MS
            + (JITTER_DIFS_SLOTS + JITTER_CW_SLOTS as u64 - 1) * JITTER_SLOT_MAX_MS;
        assert_eq!(widest_possible, 1600);
        assert!(POST_TX_WINDOW_MAX_MS >= widest_possible);
        // And a degenerate modulation still yields a window the SX1262
        // accepts: a zero timeout means "listen forever" to the chip.
        assert!(post_tx_rx_window_ms(0, 0, 0, 0, 0) >= 1);
    }

    // --- The acquisition jitter ----------------------------------------

    #[test]
    fn boot_owes_jitter_and_the_draw_is_difs_plus_a_bounded_window() {
        // Every seed must land inside DIFS + 0..=13 slots; run a spread
        // of seeds so the bound is a property, not a lucky draw.
        for seed in 1..=1_000u32 {
            let mut access = ChannelAccess::new(seed);
            access.set_phy(125_000, 7, 5);
            let jitter = access.acquisition_jitter_ms();
            assert!(jitter.is_multiple_of(24), "whole slots only, got {jitter}");
            assert!(
                (48..=48 + 13 * 24).contains(&jitter),
                "seed {seed} drew {jitter}"
            );
        }
    }

    #[test]
    fn jitter_is_owed_once_per_acquisition_not_once_per_packet() {
        let mut access = ChannelAccess::new(7);
        access.set_phy(125_000, 7, 5);
        let drawn = access.acquisition_jitter_ms();
        assert!(drawn > 0);
        // The transmit path listens the wait through; only then does it
        // probe the channel and key up.
        access.jitter_spent(drawn);
        assert_eq!(access.acquisition_jitter_ms(), 0);
        // Burst continuation: more packets, same acquisition, no new wait.
        access.begin_packet();
        assert_eq!(access.acquisition_jitter_ms(), 0);
        access.begin_packet();
        assert_eq!(access.acquisition_jitter_ms(), 0);
        // The channel is yielded back: the next acquisition owes again.
        access.channel_released();
        assert!(access.acquisition_jitter_ms() > 0);
    }

    #[test]
    fn a_jitter_wait_cut_short_by_a_reception_is_still_owed() {
        // The minimal reproducer for the bench_dual_pair_fast red of
        // 2026-09-17. The wait is spent listening, so the transmit path's
        // `rx_window` returns the moment a frame arrives: on lnode_a a
        // 288 ms draw was cut at 143 ms by an incoming proof, and the
        // path then went straight to CAD and keyed up 11 ms after that
        // frame ended — phase-locked with every other node the same frame
        // had just released, which is the exact collision the draw exists
        // to break.
        //
        // No transmission happened, so the acquisition is not over and
        // the debt must survive the interruption. Asking is not serving.
        let mut access = ChannelAccess::new(0x5EED_0001);
        access.set_phy(250_000, 7, 5);
        let drawn = access.acquisition_jitter_ms();
        assert!(drawn > 0, "boot owes jitter");
        assert_ne!(
            access.acquisition_jitter_ms(),
            0,
            "a wait that was never served was forgiven by being asked for"
        );

        // Part-served: the remainder is what is still owed, and it is a
        // resume, not a second draw.
        let served = drawn / 3;
        access.jitter_spent(served);
        assert_eq!(access.acquisition_jitter_ms(), drawn - served);
        assert!(!access.jitter_was_drawn(), "the resume redrew the window");

        // Listened through: only now may the acquisition probe the channel.
        access.jitter_spent(drawn - served);
        assert_eq!(access.acquisition_jitter_ms(), 0);
    }

    #[test]
    fn spending_more_than_is_owed_does_not_wrap_the_debt() {
        // The caller reports wall-clock elapsed, which overshoots the
        // window by the radio's own turnaround; that must land on zero,
        // not on u64::MAX minus epsilon.
        let mut access = ChannelAccess::new(0x5EED_0002);
        access.set_phy(250_000, 7, 5);
        let drawn = access.acquisition_jitter_ms();
        access.jitter_spent(drawn + 10_000);
        assert_eq!(access.acquisition_jitter_ms(), 0);
    }

    #[test]
    fn a_release_redraws_rather_than_resuming_a_part_served_wait() {
        // A yield handed the channel back: that is a new acquisition and
        // owes a full fresh draw, not the leftover of an abandoned one.
        let mut access = ChannelAccess::new(0x5EED_0003);
        access.set_phy(250_000, 7, 5);
        let drawn = access.acquisition_jitter_ms();
        access.jitter_spent(drawn / 2);
        access.channel_released();
        let redrawn = access.acquisition_jitter_ms();
        assert!(access.jitter_was_drawn(), "a release did not redraw");
        assert!(
            redrawn >= JITTER_DIFS_SLOTS * access.jitter_slot(),
            "a fresh acquisition drew only {redrawn}, i.e. a leftover"
        );
    }

    #[test]
    fn the_same_seed_draws_the_same_jitter_and_different_seeds_de_tile() {
        // Determinism is what makes the draw testable; per-board seeds are
        // what make it useful. Both properties in one place: identical
        // seeds reproduce, and across a small population of seeds more
        // than one distinct value appears.
        let draw = |seed: u32| {
            let mut access = ChannelAccess::new(seed);
            access.set_phy(125_000, 7, 5);
            access.acquisition_jitter_ms()
        };
        assert_eq!(draw(42), draw(42));
        let mut distinct = [false; 14];
        for seed in 1..=100u32 {
            distinct[((draw(seed) - 48) / 24) as usize] = true;
        }
        assert!(
            distinct.iter().filter(|d| **d).count() > 1,
            "a hundred boards all drew the same slot"
        );
    }

    #[test]
    fn a_zero_seed_is_replaced_not_a_fixed_point() {
        // xorshift32(0) == 0 forever; the constructor must not let a
        // zeroed RNG turn the jitter into a constant.
        let mut access = ChannelAccess::new(0);
        access.set_phy(125_000, 7, 5);
        let first = access.acquisition_jitter_ms();
        access.channel_released();
        let second = access.acquisition_jitter_ms();
        access.channel_released();
        let third = access.acquisition_jitter_ms();
        // Not all three equal — the RNG is stepping.
        assert!(
            !(first == second && second == third),
            "zero-seeded RNG is stuck at {first}"
        );
    }

    #[test]
    fn the_draw_is_roughly_uniform_over_the_fourteen_slots() {
        // 14_000 draws over the 14 values: each bucket should be near
        // 1000. A crude 3-sigma-ish band suffices to catch a modulo or
        // shift mistake without turning the test statistical.
        let mut access = ChannelAccess::new(0xC0FF_EE11);
        access.set_phy(125_000, 7, 5);
        let mut buckets = [0u32; 14];
        for _ in 0..14_000 {
            access.channel_released();
            let jitter = access.acquisition_jitter_ms();
            buckets[((jitter - 48) / 24) as usize] += 1;
        }
        for (slot, count) in buckets.iter().enumerate() {
            assert!(
                (900..=1100).contains(count),
                "slot {slot} drew {count} of 14000"
            );
        }
    }

    // --- The CAD retry gate --------------------------------------------

    #[test]
    fn a_clear_channel_transmits_unforced() {
        let mut access = ChannelAccess::new(3);
        access.begin_packet();
        assert_eq!(
            access.cad_clear(),
            Verdict::Transmit {
                forced: false,
                retries: 0
            }
        );
    }

    #[test]
    fn busy_backs_off_inside_the_doubling_window_and_then_gives_up() {
        // The scripted radio: busy on every probe. The gate must back off
        // with slot counts inside the advertised window (2, 4, 8, ... 64)
        // and force the TX on the 8th attempt — the frame must not starve.
        let mut access = ChannelAccess::new(0xB00_57ED);
        access.begin_packet();
        let mut cw = CAD_CW_INITIAL as u64;
        for attempt in 1..CAD_MAX_RETRIES {
            match access.cad_busy() {
                Verdict::Backoff { slots } => {
                    assert!(slots < cw, "attempt {attempt}: {slots} >= window {cw}");
                    cw = core::cmp::min(cw * 2, CAD_CW_MAX as u64);
                }
                other => panic!("attempt {attempt}: expected backoff, got {other:?}"),
            }
        }
        assert_eq!(
            access.cad_busy(),
            Verdict::Transmit {
                forced: true,
                retries: CAD_MAX_RETRIES
            }
        );
    }

    #[test]
    fn a_new_packet_resets_the_retry_budget() {
        let mut access = ChannelAccess::new(9);
        access.begin_packet();
        for _ in 0..CAD_MAX_RETRIES {
            access.cad_busy();
        }
        access.begin_packet();
        assert_eq!(access.retries(), 0);
        // With a fresh budget a busy can never force.
        assert!(matches!(access.cad_busy(), Verdict::Backoff { .. }));
    }

    #[test]
    fn cad_errors_spend_the_same_budget_and_still_transmit() {
        // A radio that cannot CAD (SPI fault, chip wedged) must not
        // starve the frame either: errors retry immediately and force at
        // the same bound.
        let mut access = ChannelAccess::new(11);
        access.begin_packet();
        for _ in 1..CAD_MAX_RETRIES {
            assert_eq!(access.cad_error(), Verdict::Retry);
        }
        assert_eq!(
            access.cad_error(),
            Verdict::Transmit {
                forced: true,
                retries: CAD_MAX_RETRIES
            }
        );
    }

    #[test]
    fn busy_and_error_mix_shares_one_budget() {
        // 4 busies and 4 errors together exhaust the 8-attempt budget.
        let mut access = ChannelAccess::new(13);
        access.begin_packet();
        for _ in 0..4 {
            assert!(matches!(access.cad_busy(), Verdict::Backoff { .. }));
        }
        for _ in 0..3 {
            assert_eq!(access.cad_error(), Verdict::Retry);
        }
        assert_eq!(
            access.cad_error(),
            Verdict::Transmit {
                forced: true,
                retries: CAD_MAX_RETRIES
            }
        );
    }

    // --- Per-board seeding (Codeberg #268) ------------------------------

    /// The number of backoffs a board walks before the retry budget forces
    /// the transmission: every attempt but the last yields a `Backoff`.
    const LADDER_LEN: usize = CAD_MAX_RETRIES as usize - 1;

    /// The backoff ladder one board walks when every CAD comes back busy,
    /// driven exactly the way `lora_task` drives this type: construct from
    /// the board's seed, adopt the bench PHY, start a packet, spend the
    /// acquisition jitter (it comes off the same stream, so a ladder taken
    /// without it is a different sequence), then CAD until the gate forces.
    fn busy_backoff_ladder(seed: u32) -> [u64; LADDER_LEN] {
        let mut access = ChannelAccess::new(seed);
        access.set_phy(125_000, 7, 5);
        access.begin_packet();
        let _ = access.acquisition_jitter_ms();
        let mut ladder = [0u64; LADDER_LEN];
        for (attempt, slots) in ladder.iter_mut().enumerate() {
            match access.cad_busy() {
                Verdict::Backoff { slots: drawn } => *slots = drawn,
                other => panic!("attempt {attempt}: expected a backoff, got {other:?}"),
            }
        }
        assert!(
            matches!(access.cad_busy(), Verdict::Transmit { forced: true, .. }),
            "the ladder must end at the forced TX"
        );
        ladder
    }

    #[test]
    fn a_board_walks_the_same_ladder_twice_from_its_own_seed() {
        // Per-board entropy must not cost reproducibility: a seed still
        // determines the whole sequence, which is what lets a host test
        // assert exact decisions and a rig run be replayed.
        assert_eq!(
            busy_backoff_ladder(0x5EED_1234),
            busy_backoff_ladder(0x5EED_1234)
        );
        assert_eq!(busy_backoff_ladder(1), busy_backoff_ladder(1));
    }

    #[test]
    fn two_boards_with_their_own_seeds_do_not_share_a_backoff_ladder() {
        // Codeberg #268: every board seeded this PRNG with one compile-time
        // constant, so two boards with equal draw counts — the normal case
        // early in a scenario, where the rig powers them together and they
        // react to the same third-party frame — drew byte-identical
        // backoffs and collided again on every one of the eight retries.
        //
        // The defect is stated first, so the test reads as a claim about
        // seeding rather than about xorshift32: one shared seed IS one
        // shared ladder, and the fix is that boards no longer share the
        // seed.
        assert_eq!(
            busy_backoff_ladder(0xDEAD_BEEF),
            busy_backoff_ladder(0xDEAD_BEEF)
        );

        // Distinct seeds: no pair of boards may walk the same ladder. The
        // population is deliberately more than a handful — the first
        // backoff is drawn from a window of CAD_CW_INITIAL slots, so two
        // boards agreeing on their FIRST retry is expected roughly half
        // the time and only the full ladder separates them.
        const BOARDS: usize = 64;
        let ladders: [[u64; LADDER_LEN]; BOARDS] = core::array::from_fn(|i| {
            busy_backoff_ladder(0x9E37_79B9u32.wrapping_mul(i as u32 + 1))
        });
        for (i, a) in ladders.iter().enumerate() {
            for (j, b) in ladders.iter().enumerate().skip(i + 1) {
                assert_ne!(a, b, "boards {i} and {j} walk the same ladder {a:?}");
            }
        }
    }

    #[test]
    fn the_jitter_a_board_owes_is_drawn_from_the_same_stream_as_its_backoffs() {
        // #268 is one seed feeding both draws, so a fix that de-tiled the
        // jitter but left the backoff on a second, still-shared stream
        // would look fixed and collide on every retry. Spending the jitter
        // has to move the ladder.
        let mut with_jitter = ChannelAccess::new(0x00C0_FFEE);
        with_jitter.set_phy(125_000, 7, 5);
        with_jitter.begin_packet();
        let _ = with_jitter.acquisition_jitter_ms();

        let mut without_jitter = ChannelAccess::new(0x00C0_FFEE);
        without_jitter.set_phy(125_000, 7, 5);
        without_jitter.begin_packet();

        let mut moved = false;
        for _ in 0..LADDER_LEN {
            match (with_jitter.cad_busy(), without_jitter.cad_busy()) {
                (Verdict::Backoff { slots: a }, Verdict::Backoff { slots: b }) => moved |= a != b,
                (a, b) => panic!("expected two backoffs, got {a:?} and {b:?}"),
            }
        }
        assert!(moved, "the backoff ladder ignored the jitter draw");
    }
}
