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
//! Nothing here is EU-only: [`Region`] is a data table and the policy has an
//! explicit off/unlimited mode for unregulated bands. The default is
//! frequency-aware (`Auto`): an EU 863-870 MHz TX frequency enforces the ETSI
//! caps lawfully-by-default, a US 902-928 MHz or out-of-band frequency stays
//! off. Operators can also force `eu868`, force `off`, or set a custom flat
//! percentage cap. No band is hardcoded as an unconditional assumption.

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

/// Inclusive lower / exclusive upper bounds (Hz) of the EU 863-870 MHz
/// licence-exempt range, used by the frequency-aware `Auto` default to decide
/// whether a TX frequency falls under ETSI duty-cycle regulation.
const EU_863_870_LOW_HZ: u32 = 863_000_000;
const EU_863_870_HIGH_HZ: u32 = 870_000_000;

/// Build a flat-cap region with no sub-band table: every frequency prices at
/// `cap_bp` over the ETSI one-hour window. Used for a custom numeric
/// `duty_cycle` value (e.g. `5%`), a single cap applied to all TX regardless
/// of sub-band.
fn custom_region(cap_bp: u32) -> Region {
    Region {
        name: "custom",
        window_ms: ETSI_WINDOW_MS,
        // Empty table: `resolve` falls every frequency through to the default.
        sub_bands: &[],
        default_cap_bp: cap_bp,
    }
}

/// Parse a numeric duty-cycle value into a cap in basis points, or `None` if
/// it is not a valid percentage.
///
/// A bare or `%`-suffixed number is always read as a **percentage**:
/// `5%`, `5` and `5.0` all mean 5% (500 bp); `0.5` means 0.5% (50 bp);
/// `10%` means 10% (1000 bp). The value must be in `(0, 100]`.
fn parse_custom_percent(s: &str) -> Option<u32> {
    let num = s.strip_suffix('%').unwrap_or(s).trim();
    let pct: f64 = num.parse().ok()?;
    if !(pct.is_finite() && pct > 0.0 && pct <= 100.0) {
        return None;
    }
    let bp = (pct * 100.0).round() as u32;
    (bp > 0).then_some(bp)
}

/// Duty-cycle policy for a LoRa interface.
///
/// `Auto` is the resolved-later default: the concrete policy depends on the TX
/// frequency, which the interface knows but the config parser does not, so it
/// is resolved via `resolve_for_frequency` once the radio is up. `Unlimited`
/// and `Enforced` are fully determined.
#[derive(Clone, Copy, Debug)]
pub(crate) enum DutyCyclePolicy {
    /// No duty-cycle limit. For unregulated bands or lab/off configs.
    Unlimited,
    /// Frequency-aware default (lawful-by-default). Resolved from the TX
    /// frequency: an EU 863-870 MHz frequency enforces the ETSI sub-band caps,
    /// anything else (US 902-928 MHz, out-of-band) stays off. Never reaches the
    /// accountant directly; `resolve_for_frequency` turns it into `Unlimited`
    /// or `Enforced` first.
    Auto,
    /// Enforce the given region's per-sub-band caps. A custom numeric config
    /// value produces an `Enforced` with a synthetic flat-cap region.
    Enforced(Region),
}

impl Default for DutyCyclePolicy {
    /// Off by default for programmatic construction (phone-attached channel
    /// radios, tests) where no region is implied. The config path uses `Auto`
    /// (frequency-aware) for an absent `duty_cycle`; see the driver's
    /// `parse_duty_cycle`.
    fn default() -> Self {
        DutyCyclePolicy::Unlimited
    }
}

impl DutyCyclePolicy {
    /// Parse a config value into a policy. Case-insensitive.
    ///
    /// Off:      `off`, `none`, `unlimited`, `false`, `0`, `disabled`
    /// Auto:     `auto`, `default` (frequency-aware, lawful-by-default)
    /// EU 868:   `eu868`, `eu-868`, `etsi-eu868`, `etsi_eu868`, `etsi`, `eu`
    /// Custom:   any percentage `5%` / `5` / `0.5` (flat cap, all sub-bands)
    ///
    /// Returns `None` for an unrecognised value so the caller can surface a
    /// config error rather than silently defaulting.
    pub(crate) fn from_config_str(s: &str) -> Option<Self> {
        let t = s.trim().to_ascii_lowercase();
        match t.as_str() {
            "off" | "none" | "unlimited" | "false" | "0" | "disabled" => {
                Some(DutyCyclePolicy::Unlimited)
            }
            "auto" | "default" => Some(DutyCyclePolicy::Auto),
            "eu868" | "eu-868" | "etsi-eu868" | "etsi_eu868" | "etsi" | "eu" => {
                Some(DutyCyclePolicy::Enforced(REGION_ETSI_EU868))
            }
            // Fall through to a numeric percentage -> flat custom cap.
            _ => parse_custom_percent(&t).map(|bp| DutyCyclePolicy::Enforced(custom_region(bp))),
        }
    }

    /// Resolve the frequency-aware `Auto` default against a concrete TX
    /// frequency. `Unlimited` and `Enforced` pass through unchanged.
    ///
    /// EU 863-870 MHz -> enforce the ETSI sub-band caps (so 869.525 MHz lands
    /// in sub-band P at 10%). US 902-928 MHz (FCC has no duty-cycle percentage)
    /// and any other frequency -> off; we do not invent a cap for a band we do
    /// not have a table for.
    pub(crate) fn resolve_for_frequency(self, freq_hz: u32) -> DutyCyclePolicy {
        match self {
            DutyCyclePolicy::Auto => {
                if (EU_863_870_LOW_HZ..EU_863_870_HIGH_HZ).contains(&freq_hz) {
                    DutyCyclePolicy::Enforced(REGION_ETSI_EU868)
                } else {
                    DutyCyclePolicy::Unlimited
                }
            }
            other => other,
        }
    }

    /// Whether this policy enforces any cap. `Auto` is not (yet) enforcing; it
    /// must be resolved to a concrete policy first.
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
            // `Auto` should be resolved to a concrete policy before it reaches
            // the accountant; treat any stray `Auto` as off rather than panic.
            DutyCyclePolicy::Unlimited | DutyCyclePolicy::Auto => 0,
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
            // Unresolved `Auto` prices as off (see `new`).
            DutyCyclePolicy::Unlimited | DutyCyclePolicy::Auto => None,
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

    // --- frequency-aware Auto default -------------------------------------

    #[test]
    fn auto_enforces_at_eu_frequency() {
        // Absent config (Auto) at 869.525 MHz -> ETSI enforcement, sub-band P
        // (10%). This is the lawful-by-default case.
        let resolved = DutyCyclePolicy::Auto.resolve_for_frequency(F_P_869_5);
        assert!(resolved.is_enforced());
        let acct = DutyCycleAccountant::new(resolved);
        assert_eq!(acct.cap_bp_for(F_P_869_5), Some(1000)); // P = 10%
    }

    #[test]
    fn auto_is_off_at_us_frequency() {
        // 915 MHz is US ISM (FCC: no duty-cycle %); Auto must NOT throttle.
        let resolved = DutyCyclePolicy::Auto.resolve_for_frequency(F_OUT_915);
        assert!(!resolved.is_enforced());
    }

    #[test]
    fn auto_is_off_out_of_known_bands() {
        // 433 MHz and 2.4 GHz have no table here -> off, no invented cap.
        assert!(!DutyCyclePolicy::Auto
            .resolve_for_frequency(433_000_000)
            .is_enforced());
        // u32 caps at ~4.29 GHz; use a high in-range value for 2.4 GHz.
        assert!(!DutyCyclePolicy::Auto
            .resolve_for_frequency(2_400_000_000)
            .is_enforced());
    }

    #[test]
    fn auto_covers_whole_eu_band_edges() {
        // Lower edge inclusive, upper edge exclusive.
        assert!(DutyCyclePolicy::Auto
            .resolve_for_frequency(863_000_000)
            .is_enforced());
        assert!(DutyCyclePolicy::Auto
            .resolve_for_frequency(869_999_999)
            .is_enforced());
        assert!(!DutyCyclePolicy::Auto
            .resolve_for_frequency(870_000_000)
            .is_enforced());
        assert!(!DutyCyclePolicy::Auto
            .resolve_for_frequency(862_999_999)
            .is_enforced());
    }

    #[test]
    fn resolve_passes_through_non_auto() {
        // Unlimited and Enforced are frequency-independent.
        assert!(!DutyCyclePolicy::Unlimited
            .resolve_for_frequency(F_P_869_5)
            .is_enforced());
        assert!(DutyCyclePolicy::Enforced(REGION_ETSI_EU868)
            .resolve_for_frequency(F_OUT_915)
            .is_enforced());
    }

    #[test]
    fn config_str_parses_auto() {
        assert!(matches!(
            DutyCyclePolicy::from_config_str("auto"),
            Some(DutyCyclePolicy::Auto)
        ));
        assert!(matches!(
            DutyCyclePolicy::from_config_str(" DEFAULT "),
            Some(DutyCyclePolicy::Auto)
        ));
    }

    // --- custom numeric percentage cap ------------------------------------

    /// A custom percentage flattens to one cap over every frequency.
    fn custom_cap_bp(s: &str, freq: u32) -> Option<u32> {
        match DutyCyclePolicy::from_config_str(s)? {
            DutyCyclePolicy::Enforced(region) => {
                DutyCycleAccountant::new(DutyCyclePolicy::Enforced(region)).cap_bp_for(freq)
            }
            _ => None,
        }
    }

    #[test]
    fn custom_percent_parses_to_flat_cap() {
        // 5% -> 500 bp, applied flat regardless of sub-band.
        assert_eq!(custom_cap_bp("5%", F_P_869_5), Some(500));
        assert_eq!(custom_cap_bp("5%", F_OUT_915), Some(500));
        // Bare number is a percentage too.
        assert_eq!(custom_cap_bp("5", F_M_868_1), Some(500));
        // Fractional percent.
        assert_eq!(custom_cap_bp("0.5", F_M_868_1), Some(50)); // 0.5% -> 50 bp
        assert_eq!(custom_cap_bp("0.1%", F_M_868_1), Some(10)); // 0.1% -> 10 bp
        assert_eq!(custom_cap_bp("10%", F_M_868_1), Some(1000)); // 10% -> 1000 bp
        assert_eq!(custom_cap_bp("100", F_M_868_1), Some(10_000)); // 100% -> 10000 bp
    }

    #[test]
    fn custom_percent_budget_is_correct() {
        // A 5% custom cap admits 5% * 3_600_000 = 180_000 ms per hour on any
        // frequency, then defers.
        let policy = DutyCyclePolicy::from_config_str("5%").unwrap();
        let mut acct = DutyCycleAccountant::new(policy);
        assert!(acct.try_charge(F_OUT_915, 180_000, 0).is_ok());
        assert!(acct.try_charge(F_OUT_915, 1, 0).is_err());
    }

    #[test]
    fn invalid_custom_percent_is_rejected() {
        // Out of (0, 100], non-numeric, or empty -> config error (None).
        assert!(DutyCyclePolicy::from_config_str("0%").is_none());
        assert!(DutyCyclePolicy::from_config_str("-5").is_none());
        assert!(DutyCyclePolicy::from_config_str("101%").is_none());
        assert!(DutyCyclePolicy::from_config_str("mars").is_none());
        assert!(DutyCyclePolicy::from_config_str("5x").is_none());
    }
}
