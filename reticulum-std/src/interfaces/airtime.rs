//! Airtime credit bucket for LoRa-Serial interfaces.
//!
//! Models the per-interface airtime budget as a signed credit that
//! decreases by `airtime_ms(len)` on every `try_send` and regenerates
//! at wall-clock rate. When a charge would push the credit below
//! `threshold_ms`, the interface signals `BufferFull` instead of
//! flooding the host-side serial queue (which on SF10 would absorb
//! ~14 minutes of backlog before the mpsc itself fills).
//!
//! Lives host-side in `reticulum-std` and never leaks into
//! `reticulum-core`, so the `no_std` core stays free of host-side
//! backpressure concerns.

use reticulum_core::rnode::airtime_ms;

/// Leaky-bucket airtime accountant for a single LoRa interface.
///
/// Lazily evaluated: no background task, no timer. `current()`
/// computes the bucket state from `now_ms - last_update_ms` on each
/// call. Ownership and mutation live behind an `Arc<Mutex<…>>`
/// provided by the interface layer.
pub(crate) struct AirtimeCredit {
    credit_ms: i64,
    last_update_ms: u64,
    threshold_ms: i64,
    max_credit_ms: i64,
    max_payload_bytes: u32,
    bw_hz: u32,
    sf: u8,
    cr: u8,
}

impl AirtimeCredit {
    /// Build a credit bucket for a given radio profile.
    ///
    /// `max_payload_bytes` is the largest frame the interface can
    /// emit; the threshold is derived as `-airtime_ms(max_payload)`
    /// so one full-MTU packet in flight saturates the bucket.
    /// `max_credit_ms` is twice that, allowing a small idle-bank of
    /// ~2 packets of headroom after a quiet period.
    pub(crate) fn new(bw_hz: u32, sf: u8, cr: u8, max_payload_bytes: u32) -> Self {
        let per_packet = airtime_ms(max_payload_bytes, bw_hz, sf, cr) as i64;
        Self {
            credit_ms: 0,
            last_update_ms: 0,
            threshold_ms: -per_packet,
            max_credit_ms: 2 * per_packet,
            max_payload_bytes,
            bw_hz,
            sf,
            cr,
        }
    }

    /// Read the effective credit at wall-clock `now_ms` without
    /// mutating state. Clamps at `max_credit_ms` to prevent idle
    /// interfaces from accumulating unbounded credit.
    pub(crate) fn current(&self, now_ms: u64) -> i64 {
        let elapsed = now_ms.saturating_sub(self.last_update_ms) as i64;
        (self.credit_ms + elapsed).min(self.max_credit_ms)
    }

    /// Deduct `airtime_ms(packet_bytes)` from the bucket if the
    /// result stays at or above `threshold_ms`. Returns `Err(())` on
    /// rejection; state is unchanged on `Err`.
    pub(crate) fn try_charge(&mut self, packet_bytes: u32, now_ms: u64) -> Result<(), ()> {
        let cost = airtime_ms(packet_bytes, self.bw_hz, self.sf, self.cr) as i64;
        let current = self.current(now_ms);
        let new_credit = current - cost;
        if new_credit < self.threshold_ms {
            return Err(());
        }
        self.credit_ms = new_credit;
        self.last_update_ms = now_ms;
        Ok(())
    }

    /// Earliest wall-clock time at which a `packet_bytes` charge
    /// would succeed. Returns `now_ms` when the bucket already has
    /// enough headroom.
    pub(crate) fn earliest_fit_time(&self, packet_bytes: u32, now_ms: u64) -> u64 {
        let cost = airtime_ms(packet_bytes, self.bw_hz, self.sf, self.cr) as i64;
        let current = self.current(now_ms);
        // Charge succeeds at time T when current(T) - cost >= threshold_ms.
        // With 1:1 regen, current(T) = current(now) + (T - now). Solve:
        //     current + (T - now) - cost >= threshold_ms
        //   => T - now >= cost - |threshold| - current    (threshold is negative)
        let wait = cost - self.threshold_ms.abs() - current;
        if wait <= 0 {
            now_ms
        } else {
            now_ms.saturating_add(wait as u64)
        }
    }

    /// Worst-case airtime in milliseconds for one full-MTU transmit
    /// under the bucket's current radio params. Used by the driver to
    /// populate Transport's `interface_max_airtime_ms` backchannel,
    /// which sizes the announce-retry jitter window.
    pub(crate) fn max_airtime_ms(&self) -> u64 {
        airtime_ms(self.max_payload_bytes, self.bw_hz, self.sf, self.cr)
    }

    /// Swap the radio params used to price subsequent charges.
    /// Preserves `credit_ms` and `last_update_ms`, the in-flight
    /// packet was already charged under the old params and drains
    /// under them; subsequent packets pay the new price. Threshold
    /// and max_credit are recomputed from `max_payload_bytes` under
    /// the new radio profile so "one MTU in flight" stays the
    /// invariant across reconfig.
    pub(crate) fn update_radio_params(&mut self, bw_hz: u32, sf: u8, cr: u8) {
        self.bw_hz = bw_hz;
        self.sf = sf;
        self.cr = cr;
        let per_packet = airtime_ms(self.max_payload_bytes, bw_hz, sf, cr) as i64;
        self.threshold_ms = -per_packet;
        self.max_credit_ms = 2 * per_packet;
    }

    /// Wall-clock timestamp (ms) of the most recent charge. Used by tests to
    /// observe the bucket at the exact instant of a charge, with no regen term.
    #[cfg(test)]
    pub(crate) fn last_update_ms(&self) -> u64 {
        self.last_update_ms
    }

    /// Re-anchor the bucket's regen baseline to `now_ms` without changing the
    /// stored credit. Lets tests align a fresh bucket to the global clock so it
    /// carries zero idle credit when subsequently charged, making the post-
    /// charge balance deterministic independent of process age. Test-only.
    #[cfg(test)]
    pub(crate) fn seed_last_update_ms(&mut self, now_ms: u64) {
        self.last_update_ms = now_ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reticulum_core::rnode::airtime_ms;

    // SF10/BW125/CR4/8, max_payload=500: cost derived by rnode helper.
    const BW: u32 = 125_000;
    const SF: u8 = 10;
    const CR: u8 = 8;
    const MAX_PAYLOAD: u32 = 500;

    fn fresh() -> AirtimeCredit {
        AirtimeCredit::new(BW, SF, CR, MAX_PAYLOAD)
    }

    #[test]
    fn new_threshold_matches_airtime_formula() {
        let b = fresh();
        let expected = -(airtime_ms(MAX_PAYLOAD, BW, SF, CR) as i64);
        assert_eq!(b.threshold_ms, expected);
        assert_eq!(b.max_credit_ms, -2 * expected);
    }

    #[test]
    fn current_returns_credit_at_same_timestamp() {
        let b = fresh();
        assert_eq!(b.current(0), 0);
    }

    #[test]
    fn current_adds_elapsed_since_last_update() {
        let mut b = fresh();
        // Put credit in a deficit so growth is observable below the cap.
        b.credit_ms = -500;
        b.last_update_ms = 1000;
        assert_eq!(b.current(1300), -200);
    }

    #[test]
    fn current_caps_at_max_credit() {
        let b = fresh();
        // After a long idle, credit saturates at max_credit_ms.
        let huge_future = 1_000_000_000u64;
        assert_eq!(b.current(huge_future), b.max_credit_ms);
    }

    #[test]
    fn try_charge_succeeds_on_fresh_bucket() {
        let mut b = fresh();
        // Charge at t=0 so no regen has occurred; credit starts at 0,
        // drops to -airtime(50) after the charge.
        let cost_50 = airtime_ms(50, BW, SF, CR) as i64;
        assert!(b.try_charge(50, 0).is_ok());
        assert_eq!(b.credit_ms, -cost_50);
        assert_eq!(b.last_update_ms, 0);
    }

    #[test]
    fn try_charge_rejects_when_over_threshold() {
        let mut b = fresh();
        // Charge one full-MTU packet at t=0; credit = -airtime(MTU) = threshold.
        assert!(b.try_charge(MAX_PAYLOAD, 0).is_ok());
        let credit_after_first = b.credit_ms;
        let last_update_after_first = b.last_update_ms;
        // Immediate second charge of any size must fail (current == threshold,
        // any non-zero cost pushes below).
        assert!(b.try_charge(50, 0).is_err());
        assert_eq!(b.credit_ms, credit_after_first);
        assert_eq!(b.last_update_ms, last_update_after_first);
    }

    #[test]
    fn try_charge_succeeds_after_regen_wait() {
        let mut b = fresh();
        assert!(b.try_charge(MAX_PAYLOAD, 0).is_ok());
        // Wait long enough for the bucket to regenerate past threshold.
        let wait_ms = airtime_ms(MAX_PAYLOAD, BW, SF, CR);
        assert!(b.try_charge(50, wait_ms).is_ok());
    }

    #[test]
    fn earliest_fit_time_returns_now_when_ready() {
        let b = fresh();
        assert_eq!(b.earliest_fit_time(50, 1_000), 1_000);
    }

    #[test]
    fn earliest_fit_time_accounts_for_deficit() {
        let mut b = fresh();
        b.try_charge(MAX_PAYLOAD, 0).unwrap();
        // At t=0 after a full-MTU charge: credit = -X, threshold = -X.
        // A 50-byte follow-up costs Y, needs current(T) - Y >= -X, i.e.
        // T - 0 >= Y (since current(0) = -X and regen is 1 ms/ms).
        // So earliest_fit_time(50, 0) == Y.
        let t = 0u64;
        let cost_small = airtime_ms(50, BW, SF, CR);
        assert_eq!(b.earliest_fit_time(50, t), t + cost_small);
        assert!(b.earliest_fit_time(50, t) > t);
    }

    #[test]
    fn update_radio_params_changes_subsequent_cost() {
        let mut b = fresh();
        // Spend everything at SF=10.
        b.try_charge(MAX_PAYLOAD, 0).unwrap();
        let credit_before = b.credit_ms;
        let last_before = b.last_update_ms;
        // Reconfig to SF=7 (much smaller per-byte airtime).
        b.update_radio_params(BW, 7, CR);
        // Credit + timestamp preserved.
        assert_eq!(b.credit_ms, credit_before);
        assert_eq!(b.last_update_ms, last_before);
        // A subsequent earliest_fit_time query uses the new (smaller) cost.
        let fit_sf7 = b.earliest_fit_time(50, 0);
        // Sanity: cost-per-byte at SF7 << SF10, so a 50-byte packet
        // prices much cheaper under the new params.
        let cost_sf10 = airtime_ms(50, BW, SF, CR) as i64;
        let cost_sf7 = airtime_ms(50, BW, 7, CR) as i64;
        assert!(cost_sf7 < cost_sf10);
        // After reconfig, threshold was recomputed at SF7 (smaller |threshold|),
        // but credit_ms still carries the old SF10 charge. The bucket is
        // deeper in deficit than the new threshold, so regen must catch up.
        let current = b.current(0);
        let expected_wait_sf7 = cost_sf7 - b.threshold_ms.abs() - current;
        let expected_fit_sf7 = expected_wait_sf7.max(0) as u64;
        assert_eq!(fit_sf7, expected_fit_sf7);
    }
}
