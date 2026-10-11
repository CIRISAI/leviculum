//! The board's channel ledgers: what share of the air is in use, by us and
//! by everyone, and how loud the channel is when nobody talks (leviculum#636).
//!
//! An RNode keeps four numbers and prints them once a second as
//! `CMD_STAT_CHTM` (`kiss_indicate_channel_stats`,
//! `reference/RNode_Firmware/Utilities.h:959-985`): its own short and long
//! airtime, a short and a long channel load, and beside them the current
//! RSSI, a noise floor and an interference reading. Before this module an
//! LNode kept the airtime pair (`leviculum_core::rnode::AirtimeTracker`, the
//! duty ledger) and nothing else, so a board on a loaded channel could not
//! say it was loaded (audit 2026-10-11, D6c).
//!
//! # What the reference measures, and how this mirrors it
//!
//! Every `STATUS_INTERVAL_MS`, 3 ms (`reference/RNode_Firmware/Config.h:176`),
//! `check_modem_status` (`reference/RNode_Firmware/RNode_Firmware.ino:1445`)
//! takes one sample: the carrier-detect bit and the instantaneous RSSI.
//!
//! * **Carrier detect** on an SX126x is the latched PreambleDetected or
//!   HeaderValid IRQ, read back, with a preamble that has stood longer than
//!   a preamble plus a header without a header treated as false
//!   (`sx126x::dcd`, `reference/RNode_Firmware/sx126x.cpp:495-521`).
//!   [`Carrier`] carries the two bits and [`ChannelLedger::sample`] makes the
//!   same call against `dcd_hold_ms`, which the firmware sizes with
//!   `leviculum_core::sx126x::false_preamble_ms`, the same two terms. The
//!   reference then clears the bit and re-enters receive; a ledger only
//!   measures, so here the stale latch simply stops counting as busy.
//! * **Short channel load.** The last `DCD_SAMPLES` = 2500 carrier bits
//!   (`reference/RNode_Firmware/Config.h:178`), 7.5 s of listening, counted
//!   and divided by the window: `local_channel_util`. Added to the short
//!   airtime and clamped at 1 it is `total_channel_util`
//!   (`reference/RNode_Firmware/RNode_Firmware.ino:1453-1460`), the
//!   `channel_load_short` of the stat frame. Our own keyed time counts in
//!   it through the airtime term, because a transmitting modem does not
//!   sample.
//! * **Long channel load.** Once per update the short load is folded into
//!   the hour's 480 bins of 7.5 s (`AIRTIME_BINS`,
//!   `reference/RNode_Firmware/Config.h:184`) as a per-bin maximum, and the
//!   long load is their mean (`reference/RNode_Firmware/RNode_Firmware.ino:1462-1465`,
//!   `:704-706`).
//! * **Airtime.** The duty ledger's own short and long figures, untouched
//!   (`update_airtime`, `reference/RNode_Firmware/RNode_Firmware.ino:693-702`).
//! * **Noise floor.** A sample is a noise sample when no carrier is detected
//!   and, once a floor exists, it sits less than `CSMA_INFR_THRESHOLD_DB`,
//!   11 dB, above it. Every `NOISE_FLOOR_SAMPLES`, 128, of them the floor
//!   becomes their mean (`update_noise_floor`,
//!   `reference/RNode_Firmware/RNode_Firmware.ino:1358-1388`). The reference
//!   keeps a 128-entry ring and sums it only when the ring wraps, so the
//!   mean it takes is always of the last 128 accepted samples: a block mean,
//!   which a running sum and a count give exactly, with no ring.
//! * **Interference** is a sample with no carrier more than 11 dB above the
//!   floor; standing below `CSMA_RFENV_RECAL_LIMIT_DB`, -83 dBm, for
//!   `CSMA_RFENV_RECAL_MS`, 2.5 s, it marks the floor unsampled so the next
//!   block measures the new environment
//!   (`update_modem_status`, `reference/RNode_Firmware/RNode_Firmware.ino:1394-1431`,
//!   `reference/RNode_Firmware/Config.h:113-115`).
//!
//! # Where this departs from the reference, and why
//!
//! * **A sample every [`SAMPLE_MS`], 30 ms, not every 3 ms**, and therefore
//!   a carrier window of [`DCD_SAMPLES`], 250, not 2500, so the window
//!   still spans 7.5 s of listening. The reference samples in its main
//!   loop, which spins anyway; our receive path sleeps on the DIO1 edge, and
//!   ten times the wake-ups would buy resolution the 1 s report does not
//!   show. A periodic sample of a frame-shaped carrier is an unbiased
//!   estimate of its share at either rate.
//! * **Updates are timed, not counted.** The reference recomputes every
//!   `UTIL_UPDATE_INTERVAL` samples, which is one second only while it
//!   samples uninterrupted (`reference/RNode_Firmware/Config.h:179-180`);
//!   [`ChannelLedger::update`] recomputes when [`UPDATE_MS`] has passed,
//!   whoever asks, so a board that spends a second transmitting still
//!   reports once a second.
//! * **Skipped bins are retired**, as `AirtimeTracker` does for the airtime
//!   bins (leviculum#495): the reference clears only the next bin, which
//!   double counts an hour-old bin whenever an update is skipped.
//! * **No noise floor is reported until one was sampled.** The reference
//!   reports its initial `-292` (`reference/RNode_Firmware/Config.h:155`)
//!   wrapped into a byte, and counts every sample as interference against
//!   it. Here the floor is `None`, and nothing is interference, until the
//!   first block of 128 is in.
//! * **No LNA gain-variance filter.** `LORA_LNA_GVT` and the
//!   `LNA_GD_*` thresholds are compiled only for boards with an external
//!   LNA (`reference/RNode_Firmware/Boards.h:425-429`); none of ours has
//!   one.
//!
//! Nothing here decides anything on the air. The CSMA band escalation the
//! reference drives from the short airtime (D3,
//! `reference/RNode_Firmware/RNode_Firmware.ino:1603-1619`) reads
//! [`ChannelLedger::airtime_short_bp`] and lives in the crate root
//! (leviculum#637).

use core::fmt;

use leviculum_core::rnode::{
    AirtimeTracker, ChannelStats, AIRTIME_BINLEN_MS, AIRTIME_BINS, AIRTIME_LONGTERM_MS,
};

/// How often the receive path samples the carrier bit and the RSSI while it
/// listens: ten times the reference's `STATUS_INTERVAL_MS`
/// (`reference/RNode_Firmware/Config.h:176`). See the module docs.
pub const SAMPLE_MS: u64 = 30;

/// Carrier samples in the short window: 7.5 s of listening at
/// [`SAMPLE_MS`], the span of the reference's 2500 samples at 3 ms
/// (`reference/RNode_Firmware/Config.h:176-178`).
pub const DCD_SAMPLES: usize = (AIRTIME_BINLEN_MS / SAMPLE_MS) as usize;

/// How often the ledgers are recomputed and reported:
/// `UTIL_UPDATE_INTERVAL_MS` (`reference/RNode_Firmware/Config.h:179`).
pub const UPDATE_MS: u64 = 1000;

/// Accepted noise samples per floor estimate (`NOISE_FLOOR_SAMPLES`,
/// `reference/RNode_Firmware/Config.h:154`).
pub const NOISE_FLOOR_SAMPLES: u16 = 128;

/// How far above the floor a carrier-free sample is interference rather
/// than noise (`CSMA_INFR_THRESHOLD_DB`, `reference/RNode_Firmware/Config.h:113`).
pub const INFR_THRESHOLD_DB: i16 = 11;

/// How long interference below [`RFENV_RECAL_LIMIT_DBM`] has to persist
/// before the floor is resampled (`CSMA_RFENV_RECAL_MS`,
/// `reference/RNode_Firmware/Config.h:114`).
pub const RFENV_RECAL_MS: u64 = 2500;

/// The level below which persistent interference is taken for a changed
/// environment rather than a transmitter (`CSMA_RFENV_RECAL_LIMIT_DB`,
/// `reference/RNode_Firmware/Config.h:115`).
pub const RFENV_RECAL_LIMIT_DBM: i16 = -83;

/// One whole, in the stat frame's unit: hundredths of a percent
/// (`(uint16_t)(airtime*100*100)`, `reference/RNode_Firmware/Utilities.h:961`).
pub const FULL_BP: u32 = 10_000;

const DCD_WORDS: usize = DCD_SAMPLES.div_ceil(32);

/// The two latched receive IRQ bits one carrier sample reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Carrier {
    /// PreambleDetected is latched.
    pub preamble: bool,
    /// HeaderValid is latched.
    pub header: bool,
}

/// The ledgers as of one update: the values of one `CMD_STAT_CHTM` frame
/// and of one `[LORA_CHTM]` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelSnapshot {
    /// Our keyed airtime over the current and previous 7.5 s bins, in
    /// hundredths of a percent.
    pub airtime_short: u16,
    /// Our keyed airtime over the rolling hour, in hundredths of a percent.
    pub airtime_long: u16,
    /// Carrier share of the last 7.5 s of listening plus our short
    /// airtime, clamped at 100 %, in hundredths of a percent.
    pub channel_load_short: u16,
    /// Mean over the hour's bins of each bin's highest short load, in
    /// hundredths of a percent.
    pub channel_load_long: u16,
    /// The last RSSI sample in dBm, `None` before the first.
    pub current_rssi: Option<i16>,
    /// The noise floor in dBm, `None` until the first block is averaged.
    pub noise_floor: Option<i16>,
    /// The last sample's RSSI in dBm when it was interference, else `None`.
    pub interference: Option<i16>,
}

impl ChannelSnapshot {
    /// The same values as the core's `CMD_STAT_CHTM` type, for the envelope
    /// report.
    pub fn stats(&self) -> ChannelStats {
        ChannelStats {
            airtime_short: self.airtime_short,
            airtime_long: self.airtime_long,
            channel_load_short: self.channel_load_short,
            channel_load_long: self.channel_load_long,
            current_rssi: self.current_rssi,
            noise_floor: self.noise_floor,
            interference: self.interference,
        }
    }
}

/// Hundredths of a percent as a percent with two decimals, the format the
/// host's `LORA_CHTM` line prints (`{:.2}` of `raw / 100.0`).
struct Percent(u16);

impl fmt::Display for Percent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:02}", self.0 / 100, self.0 % 100)
    }
}

/// A dBm reading, or `none`.
struct Dbm(Option<i16>);

impl fmt::Display for Dbm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(dbm) => write!(f, "{dbm}"),
            None => f.write_str("none"),
        }
    }
}

/// The body of the board's `[LORA_CHTM]` line. The first four keys and
/// their format are the host's `LORA_CHTM` line's
/// (`leviculum-std/src/interfaces/rnode.rs`, `apply_radio_stat`), so one
/// reader takes either; the three RSSI keys are the stat frame's other
/// bytes, which the host line does not print.
impl fmt::Display for ChannelSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "airtime_short={} airtime_long={} channel_load_short={} channel_load_long={} \
             current_rssi={} noise_floor={} interference={}",
            Percent(self.airtime_short),
            Percent(self.airtime_long),
            Percent(self.channel_load_short),
            Percent(self.channel_load_long),
            Dbm(self.current_rssi),
            Dbm(self.noise_floor),
            Dbm(self.interference),
        )
    }
}

/// The duty ledger and the channel-load ledgers of one LoRa radio.
///
/// Owns the [`AirtimeTracker`] rather than borrowing it, because the two
/// are read together at every update and fed from the same task: the
/// receive path samples while it waits, the transmit path charges airtime,
/// and either may be the one that finds an update due. Fixed size, no heap:
/// the tracker's 960 bytes plus [`AIRTIME_BINS`] `u16` load bins (960) and a
/// [`DCD_SAMPLES`]-bit carrier ring (32).
#[derive(Clone)]
pub struct ChannelLedger {
    airtime: AirtimeTracker,
    dcd_ring: [u32; DCD_WORDS],
    dcd_next: u16,
    load_bins: [u16; AIRTIME_BINS],
    load_last_bin: Option<u64>,
    dcd_hold_ms: Option<u64>,
    preamble_since: Option<u64>,
    nf_sum: i32,
    nf_count: u16,
    nf_sampled: bool,
    noise_floor: Option<i16>,
    current_rssi: Option<i16>,
    interference: bool,
    interference_since: Option<u64>,
    last_update_ms: Option<u64>,
    /// The last snapshot, meaningful once `last_update_ms` is `Some`. Not an
    /// `Option<ChannelSnapshot>`: that type's `None` is a non-zero niche in
    /// the snapshot's own `Option<i16>` tags, which would move a `static`
    /// ledger from `.bss` into `.data` and copy its 2 KB out of flash at
    /// every boot. Every field here is zero in [`new`](Self::new).
    last: ChannelSnapshot,
}

impl Default for ChannelLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelLedger {
    /// Empty ledgers, airtime limits unset, no noise floor.
    pub const fn new() -> Self {
        Self {
            airtime: AirtimeTracker::new(),
            dcd_ring: [0; DCD_WORDS],
            dcd_next: 0,
            load_bins: [0; AIRTIME_BINS],
            load_last_bin: None,
            dcd_hold_ms: None,
            preamble_since: None,
            nf_sum: 0,
            nf_count: 0,
            nf_sampled: false,
            noise_floor: None,
            current_rssi: None,
            interference: false,
            interference_since: None,
            last_update_ms: None,
            last: ChannelSnapshot {
                airtime_short: 0,
                airtime_long: 0,
                channel_load_short: 0,
                channel_load_long: 0,
                current_rssi: None,
                noise_floor: None,
                interference: None,
            },
        }
    }

    /// The duty ledger.
    pub fn airtime(&self) -> &AirtimeTracker {
        &self.airtime
    }

    /// The duty ledger, to charge a frame or set a limit.
    pub fn airtime_mut(&mut self) -> &mut AirtimeTracker {
        &mut self.airtime
    }

    /// How long a latched preamble without a header counts as a carrier:
    /// `leviculum_core::sx126x::false_preamble_ms` at the running PHY, or
    /// `None` when that has no answer, in which case a latched preamble
    /// counts for as long as it stands.
    pub fn set_dcd_hold_ms(&mut self, hold_ms: Option<u64>) {
        self.dcd_hold_ms = hold_ms;
        self.preamble_since = None;
    }

    /// The reference's `dcd()` on one reading of the two latched bits.
    fn carrier_detected(&mut self, now_ms: u64, carrier: Carrier) -> bool {
        if !carrier.preamble {
            self.preamble_since = None;
            return carrier.header;
        }
        let since = *self.preamble_since.get_or_insert(now_ms);
        if carrier.header {
            return true;
        }
        match self.dcd_hold_ms {
            Some(hold) => now_ms.saturating_sub(since) <= hold,
            None => true,
        }
    }

    /// Take one sample: the latched IRQ bits and the instantaneous RSSI,
    /// read while the receiver listens.
    pub fn sample(&mut self, now_ms: u64, carrier: Carrier, rssi_dbm: i16) {
        let dcd = self.carrier_detected(now_ms, carrier);
        self.current_rssi = Some(rssi_dbm);

        // update_modem_status: interference, and the recalibration it can
        // trigger. Against a sampled-at-least-once floor only.
        self.interference = match self.noise_floor {
            Some(nf) => !dcd && rssi_dbm > nf + INFR_THRESHOLD_DB,
            None => false,
        };
        if self.interference && rssi_dbm < RFENV_RECAL_LIMIT_DBM {
            let since = *self.interference_since.get_or_insert(now_ms);
            if now_ms.saturating_sub(since) >= RFENV_RECAL_MS {
                self.nf_sampled = false;
                self.interference_since = None;
            }
        } else {
            self.interference_since = None;
        }

        // update_noise_floor.
        if !dcd {
            let accept = match (self.nf_sampled, self.noise_floor) {
                (true, Some(nf)) => rssi_dbm < nf + INFR_THRESHOLD_DB,
                _ => true,
            };
            if accept {
                self.nf_sum += rssi_dbm as i32;
                self.nf_count += 1;
                if self.nf_count >= NOISE_FLOOR_SAMPLES {
                    self.noise_floor = Some((self.nf_sum / NOISE_FLOOR_SAMPLES as i32) as i16);
                    self.nf_sampled = true;
                    self.nf_sum = 0;
                    self.nf_count = 0;
                }
            }
        }

        // util_samples[dcd_sample] = dcd.
        let i = self.dcd_next as usize;
        let (word, bit) = (i / 32, 1u32 << (i % 32));
        if dcd {
            self.dcd_ring[word] |= bit;
        } else {
            self.dcd_ring[word] &= !bit;
        }
        self.dcd_next = ((i + 1) % DCD_SAMPLES) as u16;
    }

    /// Whether [`update`](Self::update) has anything new to say at `now_ms`:
    /// never updated, or [`UPDATE_MS`] since the last.
    pub fn update_due(&self, now_ms: u64) -> bool {
        match self.last_update_ms {
            None => true,
            Some(last) => now_ms.saturating_sub(last) >= UPDATE_MS,
        }
    }

    /// Recompute every ledger at `now_ms` and return the snapshot, if an
    /// update is due; `None` otherwise, and nothing changes. Also brings
    /// the duty ledger up to `now_ms`, which is the same call its own users
    /// make and changes nothing they rely on.
    pub fn update(&mut self, now_ms: u64) -> Option<ChannelSnapshot> {
        if !self.update_due(now_ms) {
            return None;
        }
        self.last_update_ms = Some(now_ms);

        self.airtime.update(now_ms);
        let airtime_short = self.airtime_short_bp();
        let airtime_long = bp(
            self.airtime.long_term_airtime_ms() as u64,
            AIRTIME_LONGTERM_MS,
        );

        let busy: u32 = self.dcd_ring.iter().map(|w| w.count_ones()).sum();
        let local = bp(busy as u64, DCD_SAMPLES as u64);
        let load_short = (local as u32 + airtime_short as u32).min(FULL_BP) as u16;

        self.retire_skipped_load_bins(now_ms);
        let cb = ((now_ms % AIRTIME_LONGTERM_MS) / AIRTIME_BINLEN_MS) as usize;
        self.load_bins[cb] = self.load_bins[cb].max(load_short);
        let sum: u32 = self.load_bins.iter().map(|&b| b as u32).sum();
        let load_long = (sum / AIRTIME_BINS as u32) as u16;

        let snapshot = ChannelSnapshot {
            airtime_short,
            airtime_long,
            channel_load_short: load_short,
            channel_load_long: load_long,
            current_rssi: self.current_rssi,
            noise_floor: self.noise_floor,
            interference: if self.interference {
                self.current_rssi
            } else {
                None
            },
        };
        self.last = snapshot;
        Some(snapshot)
    }

    /// Our keyed airtime over the current and previous 7.5 s bins as of the
    /// duty ledger's last update, in hundredths of a percent: the
    /// reference's `airtime`, which selects its CSMA band
    /// ([`crate::ChannelAccess::set_airtime_short`]).
    pub fn airtime_short_bp(&self) -> u16 {
        bp(
            self.airtime.short_term_airtime_ms() as u64,
            2 * AIRTIME_BINLEN_MS,
        )
    }

    /// The snapshot of the last update, `None` before the first.
    pub fn last(&self) -> Option<ChannelSnapshot> {
        self.last_update_ms.map(|_| self.last)
    }

    /// Clear the load bins time has rolled past since the last update,
    /// through the one after the current one: `longterm_bins[nb] = 0.0`
    /// (`reference/RNode_Firmware/RNode_Firmware.ino:1465`) extended over
    /// every bin a gap skipped, the `AirtimeTracker` rule (leviculum#495).
    fn retire_skipped_load_bins(&mut self, now_ms: u64) {
        let now_bin = now_ms / AIRTIME_BINLEN_MS;
        let first = match self.load_last_bin {
            None => now_bin + 1,
            Some(last) if now_bin < last => return,
            Some(last) => last + 1,
        };
        let last = now_bin + 1;
        let count = (last + 1 - first).min(AIRTIME_BINS as u64);
        for abs in (last + 1 - count)..=last {
            self.load_bins[(abs % AIRTIME_BINS as u64) as usize] = 0;
        }
        self.load_last_bin = Some(now_bin);
    }
}

/// `part / whole` in hundredths of a percent, truncated as the reference's
/// `(uint16_t)(fraction*100*100)` truncates, and clamped at one whole.
fn bp(part: u64, whole: u64) -> u16 {
    if whole == 0 {
        return 0;
    }
    (part * FULL_BP as u64 / whole).min(FULL_BP as u64) as u16
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::format;

    const CLEAR: Carrier = Carrier {
        preamble: false,
        header: false,
    };
    const FRAME: Carrier = Carrier {
        preamble: true,
        header: true,
    };

    /// Feed `n` samples one [`SAMPLE_MS`] apart from `t`, `busy` of every
    /// `period` of them carrying a frame, at `rssi` when clear and
    /// `rssi + 30` on a frame. Returns the time after the last sample.
    fn feed(
        ledger: &mut ChannelLedger,
        mut t: u64,
        n: usize,
        busy: usize,
        period: usize,
        rssi: i16,
    ) -> u64 {
        for i in 0..n {
            let on = i % period < busy;
            ledger.sample(
                t,
                if on { FRAME } else { CLEAR },
                if on { rssi + 30 } else { rssi },
            );
            t += SAMPLE_MS;
        }
        t
    }

    #[test]
    fn the_windows_are_the_references() {
        // 2500 samples at 3 ms in the reference, 250 at 30 ms here: the
        // same 7.5 s, which is also one airtime bin.
        assert_eq!(DCD_SAMPLES, 250);
        assert_eq!(DCD_SAMPLES as u64 * SAMPLE_MS, 3 * 2500);
        assert_eq!(AIRTIME_BINS, 480);
    }

    #[test]
    fn an_idle_channel_reports_zero_load_and_its_noise_floor() {
        let mut ledger = ChannelLedger::new();
        let t = feed(&mut ledger, 0, 300, 0, 1, -118);
        let snap = ledger.update(t).unwrap();
        assert_eq!(snap.channel_load_short, 0);
        assert_eq!(snap.channel_load_long, 0);
        assert_eq!(snap.airtime_short, 0);
        assert_eq!(snap.noise_floor, Some(-118));
        assert_eq!(snap.current_rssi, Some(-118));
        assert_eq!(snap.interference, None);
    }

    #[test]
    fn no_noise_floor_is_reported_before_the_first_block() {
        let mut ledger = ChannelLedger::new();
        let t = feed(
            &mut ledger,
            0,
            (NOISE_FLOOR_SAMPLES - 1) as usize,
            0,
            1,
            -118,
        );
        let snap = ledger.update(t).unwrap();
        assert_eq!(snap.noise_floor, None);
        // The envelope then carries the 8-byte form.
        let (_, len) = leviculum_core::rnode::encode_channel_stats(&snap.stats());
        assert_eq!(len, leviculum_core::rnode::CHANNEL_STATS_SHORT_LEN);
    }

    #[test]
    fn a_quarter_busy_channel_reads_twenty_five_percent() {
        let mut ledger = ChannelLedger::new();
        // A full window, one sample in four on a frame.
        let t = feed(&mut ledger, 0, DCD_SAMPLES, 1, 4, -118);
        let snap = ledger.update(t).unwrap();
        // 63 of 250 samples (indices 0, 4, ..., 248): 25.20 %.
        assert_eq!(snap.channel_load_short, 2520);
        // Frames are not noise: the floor is the clear samples alone.
        assert_eq!(snap.noise_floor, Some(-118));
    }

    #[test]
    fn the_short_window_forgets_after_seven_and_a_half_seconds_of_listening() {
        let mut ledger = ChannelLedger::new();
        let t = feed(&mut ledger, 0, DCD_SAMPLES, 1, 1, -118);
        assert_eq!(ledger.update(t).unwrap().channel_load_short, FULL_BP as u16);
        let t = feed(&mut ledger, t, DCD_SAMPLES, 0, 1, -118);
        assert_eq!(ledger.update(t).unwrap().channel_load_short, 0);
    }

    #[test]
    fn our_own_airtime_counts_in_the_load_and_the_sum_clamps() {
        let mut ledger = ChannelLedger::new();
        // 3 s keyed in this bin: 3000 / 15000 = 20 % short airtime.
        ledger.airtime_mut().add_airtime(100, 3000);
        let t = feed(&mut ledger, 100, DCD_SAMPLES, 1, 2, -118);
        let snap = ledger.update(t).unwrap();
        assert_eq!(snap.airtime_short, 2000);
        // 3000 ms of the hour: 0.08 %.
        assert_eq!(snap.airtime_long, 8);
        // 50 % heard + 20 % keyed.
        assert_eq!(snap.channel_load_short, 5000 + 2000);

        // 90 % heard + 20 % keyed clamps at 100 %.
        let mut busy = ChannelLedger::new();
        busy.airtime_mut().add_airtime(100, 3000);
        let t = feed(&mut busy, 100, DCD_SAMPLES, 9, 10, -118);
        assert_eq!(busy.update(t).unwrap().channel_load_short, FULL_BP as u16);
    }

    #[test]
    fn the_long_load_is_the_hours_mean_of_bin_maxima() {
        let mut ledger = ChannelLedger::new();
        // One bin at 100 %, then quiet: 1/480 of the hour.
        let t = feed(&mut ledger, 0, DCD_SAMPLES, 1, 1, -118);
        let snap = ledger.update(t).unwrap();
        assert_eq!(snap.channel_load_long, (FULL_BP / 480) as u16);
        // Two more updates in the same bin at lower load leave the maximum.
        let t2 = feed(&mut ledger, t, 10, 0, 1, -118);
        let later = ledger.update(t2 + UPDATE_MS).unwrap();
        assert!(later.channel_load_long >= snap.channel_load_long);
    }

    #[test]
    fn an_hour_of_silence_empties_the_long_load() {
        let mut ledger = ChannelLedger::new();
        let t = feed(&mut ledger, 0, DCD_SAMPLES, 1, 1, -118);
        assert!(ledger.update(t).unwrap().channel_load_long > 0);
        // A gap of an hour with no update at all: every bin is retired,
        // none is counted twice (the #495 rule).
        let t = feed(
            &mut ledger,
            t + AIRTIME_LONGTERM_MS,
            DCD_SAMPLES,
            0,
            1,
            -118,
        );
        assert_eq!(ledger.update(t).unwrap().channel_load_long, 0);
    }

    #[test]
    fn a_false_preamble_stops_counting_after_its_hold() {
        let mut ledger = ChannelLedger::new();
        ledger.set_dcd_hold_ms(Some(90));
        let preamble_only = Carrier {
            preamble: true,
            header: false,
        };
        // 250 samples of a latch that never decodes a header: busy for the
        // first 90 ms (samples at 0, 30, 60, 90), clear after.
        let mut t = 0;
        for _ in 0..DCD_SAMPLES {
            ledger.sample(t, preamble_only, -118);
            t += SAMPLE_MS;
        }
        assert_eq!(ledger.update(t).unwrap().channel_load_short, 4 * 40);
        // A header makes it a frame however long it stood.
        let mut held = ChannelLedger::new();
        held.set_dcd_hold_ms(Some(90));
        for i in 0..DCD_SAMPLES as u64 {
            held.sample(
                i * SAMPLE_MS,
                Carrier {
                    preamble: true,
                    header: i > 0,
                },
                -90,
            );
        }
        assert_eq!(
            held.update(DCD_SAMPLES as u64 * SAMPLE_MS)
                .unwrap()
                .channel_load_short,
            FULL_BP as u16
        );
    }

    #[test]
    fn a_carrier_without_a_preamble_is_interference_not_noise() {
        let mut ledger = ChannelLedger::new();
        let t = feed(&mut ledger, 0, NOISE_FLOOR_SAMPLES as usize, 0, 1, -118);
        // 12 dB above the floor, no LoRa preamble: interference, and the
        // floor does not move toward it.
        ledger.sample(t, CLEAR, -106);
        let snap = ledger.update(t).unwrap();
        assert_eq!(snap.interference, Some(-106));
        assert_eq!(snap.noise_floor, Some(-118));
        assert_eq!(snap.channel_load_short, 0, "interference is not a carrier");
        // 10 dB above is still noise.
        ledger.sample(t + SAMPLE_MS, CLEAR, -108);
        assert_eq!(ledger.update(t + UPDATE_MS).unwrap().interference, None);
    }

    #[test]
    fn persistent_weak_interference_resamples_the_floor() {
        let mut ledger = ChannelLedger::new();
        let t = feed(&mut ledger, 0, NOISE_FLOOR_SAMPLES as usize, 0, 1, -118);
        // The environment moved: -100 dBm, below the -83 recal limit,
        // for longer than 2.5 s.
        // The first sample starts the clock, the 85th is 2520 ms after it.
        let t = feed(
            &mut ledger,
            t,
            (RFENV_RECAL_MS / SAMPLE_MS) as usize + 2,
            0,
            1,
            -100,
        );
        // Unsampled now: the next block takes every sample, and the floor
        // lands on the new level.
        let t = feed(&mut ledger, t, NOISE_FLOOR_SAMPLES as usize, 0, 1, -100);
        assert_eq!(ledger.update(t).unwrap().noise_floor, Some(-100));
    }

    #[test]
    fn a_strong_carrier_never_resamples_the_floor() {
        let mut ledger = ChannelLedger::new();
        let t = feed(&mut ledger, 0, NOISE_FLOOR_SAMPLES as usize, 0, 1, -118);
        // -70 dBm is above the recal limit: a transmitter, not a new floor.
        let t = feed(&mut ledger, t, 1000, 0, 1, -70);
        assert_eq!(ledger.update(t).unwrap().noise_floor, Some(-118));
    }

    #[test]
    fn the_noise_floor_is_a_truncated_block_mean() {
        let mut ledger = ChannelLedger::new();
        let mut t = 0;
        for i in 0..NOISE_FLOOR_SAMPLES {
            // Alternating -117 and -118: the sum / 128 is -117.5, which C's
            // integer division truncates toward zero, as Rust's does.
            ledger.sample(t, CLEAR, if i % 2 == 0 { -117 } else { -118 });
            t += SAMPLE_MS;
        }
        assert_eq!(ledger.update(t).unwrap().noise_floor, Some(-117));
    }

    #[test]
    fn updates_come_once_a_second() {
        let mut ledger = ChannelLedger::new();
        assert_eq!(ledger.last(), None, "no snapshot before the first update");
        assert!(ledger.update(0).is_some(), "the first is due at once");
        assert!(ledger.update(999).is_none());
        assert!(ledger.update(1000).is_some());
        assert_eq!(ledger.last().map(|s| s.channel_load_short), Some(0));
    }

    #[test]
    fn the_line_carries_the_hosts_keys_in_the_hosts_format() {
        let mut ledger = ChannelLedger::new();
        ledger.airtime_mut().add_airtime(100, 450);
        let t = feed(&mut ledger, 100, DCD_SAMPLES, 1, 4, -118);
        let snap = ledger.update(t).unwrap();
        assert_eq!(
            format!("{snap}"),
            "airtime_short=3.00 airtime_long=0.01 channel_load_short=28.20 \
             channel_load_long=0.05 current_rssi=-118 noise_floor=-118 interference=none"
        );
        let empty = ChannelLedger::new().clone().update(0).unwrap();
        assert_eq!(
            format!("{empty}"),
            "airtime_short=0.00 airtime_long=0.00 channel_load_short=0.00 \
             channel_load_long=0.00 current_rssi=none noise_floor=none interference=none"
        );
    }

    #[test]
    fn the_snapshot_is_the_stat_frame() {
        let mut ledger = ChannelLedger::new();
        let t = feed(&mut ledger, 0, DCD_SAMPLES, 1, 4, -118);
        let stats = ledger.update(t).unwrap().stats();
        let (buf, len) = leviculum_core::rnode::encode_channel_stats(&stats);
        assert_eq!(len, leviculum_core::rnode::CHANNEL_STATS_LEN);
        assert_eq!(
            leviculum_core::rnode::decode_channel_stats(&buf[..len]),
            Some(stats)
        );
    }
}
