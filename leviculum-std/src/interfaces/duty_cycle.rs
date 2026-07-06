//! Regulatory duty-cycle enforcement for LoRa interfaces (Codeberg #55).
//!
//! LoRa runs in licence-exempt sub-GHz bands whose regulators cap the
//! fraction of time a transmitter may occupy the channel over a rolling
//! observation window. In Europe the caps come from ETSI EN 300 220-2 /
//! CEPT ERC Recommendation 70-03: the 863-870 MHz range is split into
//! sub-bands, each with its own duty-cycle limit (0.1%, 1% or 10%) measured
//! over a one-hour window.
//!
//! This module keeps a rolling-window sum of transmitted airtime per
//! sub-band (chosen by TX frequency) and answers "may I transmit this
//! packet now, and if not, when?". The LoRa interface consults it just
//! before writing a frame and holds the packet back when the sub-band's
//! budget is spent, rather than transmitting over the limit.
//!
//! Interface-isolated on purpose: only the LoRa interface knows its
//! `RadioSettings` (frequency / SF / BW / CR), computes per-packet airtime
//! via [`leviculum_core::rnode::airtime_ms`], and defers TX. The core,
//! transport and daemon stay unaware -- holding a packet back is invisible
//! to the receiver, so wire and semantic compatibility are untouched. This
//! is a regulatory deviation from a thin serial writer that improves
//! Priority 1 (lawful operation) with no compatibility cost.
//!
//! Nothing here is EU-only: [`Region`] is a data table and the policy has
//! an explicit off/unlimited mode for unregulated bands. `etsi-eu868` is
//! the documented default set, not a hardcoded assumption.

use std::collections::VecDeque;

/// ETSI rolling observation window: one hour, in milliseconds.
///
/// Duty cycle under EN 300 220-2 is "transmitter on-time within any
/// one-hour period", so the window is 3_600_000 ms.
pub(crate) const ETSI_WINDOW_MS: u64 = 3_600_000;

/// Basis-point denominator: a cap of `cap_bp` basis points means
/// `cap_bp / 10_000` of the window may be occupied (100 bp = 1%).
const BP_DENOMINATOR: u64 = 10_000;

/// A contiguous frequency sub-band `[low_hz, high_hz)` with its
/// duty-cycle cap in basis points (1% = 100 bp, 0.1% = 10 bp, 10% = 1000 bp).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SubBand {
    /// Inclusive lower bound, Hz.
    pub low_hz: u32,
    /// Exclusive upper bound, Hz.
    pub high_hz: u32,
    /// Duty-cycle cap in basis points (1/10000).
    pub cap_bp: u32,
}

impl SubBand {
    fn contains(&self, freq_hz: u32) -> bool {
        freq_hz >= self.low_hz && freq_hz < self.high_hz
    }
}

/// A named regulatory region: its observation window, the ordered sub-band
/// table, and the cap applied to any frequency that falls in a guard gap or
/// outside every listed sub-band.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Region {
    /// Region identifier, e.g. `"etsi-eu868"`. Documents the table and shows
    /// up in `Debug`; not read on the hot path.
    #[allow(dead_code)]
    pub name: &'static str,
    /// Rolling observation window in milliseconds.
    pub window_ms: u64,
    /// Ordered, non-overlapping sub-bands.
    pub sub_bands: &'static [SubBand],
    /// Cap (basis points) for frequencies matching no sub-band. Set to the
    /// most restrictive listed cap so an out-of-table frequency is never
    /// treated more permissively than the regulator allows.
    pub default_cap_bp: u32,
}

/// ETSI EN 300 220-2 V3.2.1 sub-bands for the EU 863-870 MHz range.
///
/// Ranges and caps per ETSI EN 300 220-2 / ERC Recommendation 70-03
/// (sub-band letters K, L, M, N, P, Q as commonly tabulated):
///
/// | Sub-band | Range (MHz)     | Duty cycle | cap_bp |
/// |----------|-----------------|------------|--------|
/// | K        | 863.0 - 865.0   | 0.1%       | 10     |
/// | L        | 865.0 - 868.0   | 1%         | 100    |
/// | M        | 868.0 - 868.6   | 1%         | 100    |
/// | N        | 868.7 - 869.2   | 0.1%       | 10     |
/// | P        | 869.4 - 869.65  | 10%        | 1000   |
/// | Q        | 869.7 - 870.0   | 1%         | 100    |
///
/// The gaps (868.6-868.7, 869.2-869.4, 869.65-869.7) and any frequency
/// outside 863-870 MHz fall through to `default_cap_bp`.
const EU868_SUBBANDS: &[SubBand] = &[
    SubBand {
        low_hz: 863_000_000,
        high_hz: 865_000_000,
        cap_bp: 10,
    }, // K 0.1%
    SubBand {
        low_hz: 865_000_000,
        high_hz: 868_000_000,
        cap_bp: 100,
    }, // L 1%
    SubBand {
        low_hz: 868_000_000,
        high_hz: 868_600_000,
        cap_bp: 100,
    }, // M 1%
    SubBand {
        low_hz: 868_700_000,
        high_hz: 869_200_000,
        cap_bp: 10,
    }, // N 0.1%
    SubBand {
        low_hz: 869_400_000,
        high_hz: 869_650_000,
        cap_bp: 1000,
    }, // P 10%
    SubBand {
        low_hz: 869_700_000,
        high_hz: 870_000_000,
        cap_bp: 100,
    }, // Q 1%
];

/// ETSI EU 863-870 MHz region with the sub-band table above and a
/// conservative 0.1% default for guard gaps / out-of-band frequencies.
pub(crate) const REGION_ETSI_EU868: Region = Region {
    name: "etsi-eu868",
    window_ms: ETSI_WINDOW_MS,
    sub_bands: EU868_SUBBANDS,
    default_cap_bp: 10,
};

/// Duty-cycle policy for a LoRa interface: either off (unregulated band) or
/// enforcing a named region's caps.
#[derive(Clone, Copy, Debug)]
pub(crate) enum DutyCyclePolicy {
    /// No duty-cycle limit. For unregulated bands or lab/off configs.
    Unlimited,
    /// Enforce the given region's per-sub-band caps.
    Enforced(Region),
}

impl Default for DutyCyclePolicy {
    /// Off by default: the band/region is deployment-specific and we do not
    /// hardcode an EU-only assumption. Operators opt into their region via
    /// config (`duty_cycle = eu868`).
    fn default() -> Self {
        DutyCyclePolicy::Unlimited
    }
}

impl DutyCyclePolicy {
    /// Parse a config value into a policy. Case-insensitive.
    ///
    /// Off:      `off`, `none`, `unlimited`, `false`, `0`, `disabled`
    /// EU 868:   `eu868`, `eu-868`, `etsi-eu868`, `etsi_eu868`, `etsi`, `eu`
    ///
    /// Returns `None` for an unrecognised value so the caller can surface a
    /// config error rather than silently defaulting.
    pub(crate) fn from_config_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "unlimited" | "false" | "0" | "disabled" => {
                Some(DutyCyclePolicy::Unlimited)
            }
            "eu868" | "eu-868" | "etsi-eu868" | "etsi_eu868" | "etsi" | "eu" => {
                Some(DutyCyclePolicy::Enforced(REGION_ETSI_EU868))
            }
            _ => None,
        }
    }

    /// Whether this policy enforces any cap.
    pub(crate) fn is_enforced(&self) -> bool {
        matches!(self, DutyCyclePolicy::Enforced(_))
    }
}

/// One recorded transmission: when it started and how long it occupied the
/// channel. Evicted once `start_ms + window_ms <= now`.
#[derive(Clone, Copy, Debug)]
struct TxEvent {
    start_ms: u64,
    airtime_ms: u64,
}

/// Rolling-window airtime accountant, one event queue per sub-band.
///
/// Lazily evaluated: no background task, no timer. Old events are evicted on
/// each query from `now_ms`. Sub-band identity is the index into
/// `region.sub_bands`; the extra trailing bucket (index == `sub_bands.len()`)
/// holds transmissions on guard-gap / out-of-band frequencies priced at
/// `default_cap_bp`.
pub(crate) struct DutyCycleAccountant {
    policy: DutyCyclePolicy,
    events: Vec<VecDeque<TxEvent>>,
}

impl DutyCycleAccountant {
    /// Build an accountant for the given policy. `Unlimited` allocates no
    /// per-band state and admits every TX.
    pub(crate) fn new(policy: DutyCyclePolicy) -> Self {
        let buckets = match policy {
            DutyCyclePolicy::Unlimited => 0,
            // One queue per sub-band plus one for out-of-band frequencies.
            DutyCyclePolicy::Enforced(region) => region.sub_bands.len() + 1,
        };
        Self {
            policy,
            events: (0..buckets).map(|_| VecDeque::new()).collect(),
        }
    }

    /// Resolve `freq_hz` to `(bucket_index, cap_bp, window_ms)`, or `None`
    /// when the policy is `Unlimited`.
    fn resolve(&self, freq_hz: u32) -> Option<(usize, u32, u64)> {
        match self.policy {
            DutyCyclePolicy::Unlimited => None,
            DutyCyclePolicy::Enforced(region) => {
                let idx = region.sub_bands.iter().position(|b| b.contains(freq_hz));
                match idx {
                    Some(i) => Some((i, region.sub_bands[i].cap_bp, region.window_ms)),
                    None => Some((
                        region.sub_bands.len(),
                        region.default_cap_bp,
                        region.window_ms,
                    )),
                }
            }
        }
    }

    /// The duty-cycle cap (basis points) that applies to `freq_hz`, or `None`
    /// under an unlimited policy. Exposed for tests / diagnostics.
    pub(crate) fn cap_bp_for(&self, freq_hz: u32) -> Option<u32> {
        self.resolve(freq_hz).map(|(_, cap_bp, _)| cap_bp)
    }

    /// Allowed airtime (ms) within one window for a given cap.
    fn budget_ms(window_ms: u64, cap_bp: u32) -> u64 {
        window_ms.saturating_mul(cap_bp as u64) / BP_DENOMINATOR
    }

    /// Evict events older than the window and return the airtime still summed
    /// inside it for `bucket`.
    fn used_airtime_ms(&mut self, bucket: usize, window_ms: u64, now_ms: u64) -> u64 {
        let q = &mut self.events[bucket];
        while let Some(front) = q.front() {
            if front.start_ms.saturating_add(window_ms) <= now_ms {
                q.pop_front();
            } else {
                break;
            }
        }
        q.iter().map(|e| e.airtime_ms).sum()
    }

    /// Try to admit a transmission of `airtime_ms` on `freq_hz` at `now_ms`.
    ///
    /// On `Ok(())` the airtime has been recorded and the caller may transmit.
    /// On `Err(defer_until_ms)` the sub-band budget is spent; the caller must
    /// hold the packet back until at least `defer_until_ms` (wall clock in the
    /// same frame as `now_ms`), at which point enough old airtime will have
    /// aged out of the window. Nothing is recorded on `Err`.
    pub(crate) fn try_charge(
        &mut self,
        freq_hz: u32,
        airtime_ms: u64,
        now_ms: u64,
    ) -> Result<(), u64> {
        let Some((bucket, cap_bp, window_ms)) = self.resolve(freq_hz) else {
            return Ok(()); // Unlimited: admit everything.
        };
        let budget = Self::budget_ms(window_ms, cap_bp);
        let used = self.used_airtime_ms(bucket, window_ms, now_ms);

        if used + airtime_ms <= budget {
            self.events[bucket].push_back(TxEvent {
                start_ms: now_ms,
                airtime_ms,
            });
            return Ok(());
        }

        Err(self.defer_until(bucket, window_ms, budget, used, airtime_ms, now_ms))
    }

    /// Compute when enough oldest events will have expired for `airtime_ms` to
    /// fit. `used` is the current in-window airtime sum for `bucket`.
    fn defer_until(
        &self,
        bucket: usize,
        window_ms: u64,
        budget: u64,
        used: u64,
        airtime_ms: u64,
        now_ms: u64,
    ) -> u64 {
        // A single packet longer than the entire window budget can never fit;
        // re-evaluate after a full window rather than spin.
        if airtime_ms > budget {
            return now_ms.saturating_add(window_ms);
        }
        // We must free at least `need` ms of in-window airtime. Expiring the
        // oldest events first, the event whose expiry crosses `need` sets the
        // defer time (it leaves the window at start_ms + window_ms).
        let need = (used + airtime_ms).saturating_sub(budget);
        let mut freed = 0u64;
        for e in self.events[bucket].iter() {
            freed += e.airtime_ms;
            if freed >= need {
                return e.start_ms.saturating_add(window_ms);
            }
        }
        // Should be unreachable when airtime_ms <= budget (freeing all events
        // leaves only airtime_ms <= budget), but stay safe.
        now_ms.saturating_add(window_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_core::rnode::airtime_ms;

    // A representative 868.1 MHz channel (sub-band M, 1%) and 868.3 (also M).
    const F_M_868_1: u32 = 868_100_000;
    const F_L_866: u32 = 866_000_000;
    const F_K_864: u32 = 864_000_000;
    const F_N_869: u32 = 869_000_000;
    const F_P_869_5: u32 = 869_500_000;
    const F_Q_869_8: u32 = 869_800_000;
    const F_GAP_868_65: u32 = 868_650_000; // guard gap between M and N
    const F_OUT_915: u32 = 915_000_000; // outside 863-870 entirely

    fn eu868() -> DutyCycleAccountant {
        DutyCycleAccountant::new(DutyCyclePolicy::Enforced(REGION_ETSI_EU868))
    }

    // --- ETSI airtime formula vs known reference values -------------------
    //
    // Reference airtimes from the Semtech SX1272/76 LoRa airtime formula
    // (AN1200.13), explicit header + CRC on, 8-symbol preamble. Each value
    // is hand-derived below and cross-checks against the widely-used online
    // calculators (e.g. avbentem/loratools, The Things Network). Airtime is
    // duty-cycle's charged quantity, so getting it right is the regulatory
    // crux -- these assert exact ms, not a tolerance band.

    #[test]
    fn airtime_sf7_bw125_cr5_20b_matches_reference() {
        // T_sym = 128/125000 = 1.024 ms; preamble = 12.25*T_sym = 12.544 ms.
        // payload symbols = 8 + ceil((160-28+44)/28)*5 = 8 + 7*5 = 43.
        // total = 12.544 + 43*1.024 = 56.576 ms -> 57 ms (round up).
        assert_eq!(airtime_ms(20, 125_000, 7, 5), 57);
    }

    #[test]
    fn airtime_sf12_bw125_cr5_20b_matches_reference() {
        // T_sym = 4096/125000 = 32.768 ms; preamble = 401.408 ms; DE=1.
        // payload symbols = 8 + ceil((160-48+44)/40)*5 = 8 + 4*5 = 28.
        // total = 401.408 + 28*32.768 = 1318.912 ms -> 1319 ms.
        // Matches loratools (1318.91 ms).
        assert_eq!(airtime_ms(20, 125_000, 12, 5), 1319);
    }

    #[test]
    fn airtime_sf10_bw125_cr8_50b_matches_reference() {
        // T_sym = 1024/125000 = 8.192 ms; preamble = 100.352 ms; DE=0.
        // payload symbols = 8 + ceil((400-40+44)/40)*8 = 8 + 11*8 = 96.
        // total = 100.352 + 96*8.192 = 886.784 ms -> 887 ms.
        assert_eq!(airtime_ms(50, 125_000, 10, 8), 887);
    }

    // --- Sub-band selection by frequency ----------------------------------

    #[test]
    fn sub_band_selection_by_frequency() {
        let acct = eu868();
        assert_eq!(acct.cap_bp_for(F_K_864), Some(10)); // K 0.1%
        assert_eq!(acct.cap_bp_for(F_L_866), Some(100)); // L 1%
        assert_eq!(acct.cap_bp_for(F_M_868_1), Some(100)); // M 1%
        assert_eq!(acct.cap_bp_for(F_N_869), Some(10)); // N 0.1%
        assert_eq!(acct.cap_bp_for(F_P_869_5), Some(1000)); // P 10%
        assert_eq!(acct.cap_bp_for(F_Q_869_8), Some(100)); // Q 1%
    }

    #[test]
    fn sub_band_boundaries_are_half_open() {
        let acct = eu868();
        // 865.0 MHz is the K/L boundary: belongs to L (1%), not K (0.1%).
        assert_eq!(acct.cap_bp_for(865_000_000), Some(100));
        // 868.0 MHz is the L/M boundary: belongs to M.
        assert_eq!(acct.cap_bp_for(868_000_000), Some(100));
        // 863.0 MHz is the very bottom of K (inclusive).
        assert_eq!(acct.cap_bp_for(863_000_000), Some(10));
    }

    #[test]
    fn guard_gap_and_out_of_band_use_default_cap() {
        let acct = eu868();
        // Guard gap 868.6-868.7 and out-of-band 915 both price at the
        // conservative default (0.1%).
        assert_eq!(acct.cap_bp_for(F_GAP_868_65), Some(10));
        assert_eq!(acct.cap_bp_for(F_OUT_915), Some(10));
    }

    // --- Rolling-window accountant: admit up to cap, then defer -----------

    #[test]
    fn admits_up_to_cap_then_defers() {
        let mut acct = eu868();
        // Sub-band M is 1% over 3_600_000 ms => 36_000 ms of airtime budget.
        let budget = DutyCycleAccountant::budget_ms(ETSI_WINDOW_MS, 100);
        assert_eq!(budget, 36_000);

        // Charge fixed 1000 ms chunks at t=0. 36 chunks exactly fill the cap.
        for i in 0..36 {
            assert!(
                acct.try_charge(F_M_868_1, 1000, 0).is_ok(),
                "chunk {i} should be admitted"
            );
        }
        // The 37th chunk would exceed 36_000 ms -> deferred.
        let deferred = acct.try_charge(F_M_868_1, 1000, 0);
        assert!(deferred.is_err(), "over-cap TX must be deferred");
        // All events started at t=0, so the oldest expires at t=window.
        // Freeing one 1000 ms event suffices for a 1000 ms charge.
        assert_eq!(deferred.unwrap_err(), ETSI_WINDOW_MS);
    }

    #[test]
    fn admits_again_after_window_rolls() {
        let mut acct = eu868();
        // Fill the 0.1% N sub-band (budget 3_600 ms) with one big event.
        let budget = DutyCycleAccountant::budget_ms(ETSI_WINDOW_MS, 10);
        assert_eq!(budget, 3_600);
        assert!(acct.try_charge(F_N_869, 3_600, 0).is_ok());
        // Immediately after, no room.
        assert!(acct.try_charge(F_N_869, 1, 0).is_err());
        // Just before the window rolls, still no room.
        assert!(acct.try_charge(F_N_869, 1, ETSI_WINDOW_MS - 1).is_err());
        // Once the first event ages out at t=window, budget is free again.
        assert!(acct.try_charge(F_N_869, 3_600, ETSI_WINDOW_MS).is_ok());
    }

    #[test]
    fn defer_time_reflects_partial_expiry() {
        let mut acct = eu868();
        // N sub-band, budget 3_600 ms. Two 1_800 ms events at t=0 and
        // t=1_000 fill it exactly.
        assert!(acct.try_charge(F_N_869, 1_800, 0).is_ok());
        assert!(acct.try_charge(F_N_869, 1_800, 1_000).is_ok());
        // A third 1_800 ms charge at t=2_000 needs 1_800 ms freed. Only the
        // first event (1_800 ms, started at t=0) needs to expire, at
        // t = 0 + window.
        let r = acct.try_charge(F_N_869, 1_800, 2_000);
        assert_eq!(r.unwrap_err(), ETSI_WINDOW_MS);
    }

    #[test]
    fn distinct_sub_bands_have_independent_budgets() {
        let mut acct = eu868();
        // Saturate N (0.1%). M (1%) must remain wide open: separate bucket.
        assert!(acct.try_charge(F_N_869, 3_600, 0).is_ok());
        assert!(acct.try_charge(F_N_869, 1, 0).is_err());
        assert!(acct.try_charge(F_M_868_1, 1_000, 0).is_ok());
    }

    // --- off / unlimited disables gating ----------------------------------

    #[test]
    fn unlimited_never_defers() {
        let mut acct = DutyCycleAccountant::new(DutyCyclePolicy::Unlimited);
        // Charge far beyond any cap: always admitted.
        for _ in 0..10_000 {
            assert!(acct.try_charge(F_N_869, 5_000, 0).is_ok());
        }
        // No sub-band cap under an unlimited policy.
        assert_eq!(acct.cap_bp_for(F_N_869), None);
    }

    #[test]
    fn oversized_single_packet_defers_a_full_window() {
        let mut acct = eu868();
        // A single packet longer than the whole N budget (3_600 ms) can never
        // fit; defer a full window and re-evaluate rather than spin.
        let r = acct.try_charge(F_N_869, 3_601, 10_000);
        assert_eq!(r.unwrap_err(), 10_000 + ETSI_WINDOW_MS);
    }

    // --- config parsing ---------------------------------------------------

    #[test]
    fn policy_from_config_str() {
        assert!(matches!(
            DutyCyclePolicy::from_config_str("off"),
            Some(DutyCyclePolicy::Unlimited)
        ));
        assert!(matches!(
            DutyCyclePolicy::from_config_str("UNLIMITED"),
            Some(DutyCyclePolicy::Unlimited)
        ));
        assert!(matches!(
            DutyCyclePolicy::from_config_str(" eu868 "),
            Some(DutyCyclePolicy::Enforced(_))
        ));
        assert!(matches!(
            DutyCyclePolicy::from_config_str("etsi-eu868"),
            Some(DutyCyclePolicy::Enforced(_))
        ));
        assert!(DutyCyclePolicy::from_config_str("mars").is_none());
    }

    #[test]
    fn default_policy_is_unlimited() {
        assert!(!DutyCyclePolicy::default().is_enforced());
    }
}
