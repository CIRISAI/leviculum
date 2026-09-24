//! One client upload, from the packet landing to the record being durable,
//! priced against the uploader's receipt window (leviculum#397).
//!
//! The sequence below is `leviculum_nrf::pn::Engine`'s own: `on_event` sees a
//! `LinkProofRequested` and then a `LinkDataReceived`, queues a `Work::Upload`,
//! and `settle` grinds one [`GRIND_SLICE_ROUNDS`]-round slice of the stamp
//! workblock per pass until the judgement is in, then flushes the store. The
//! only thing the fix moves is WHERE in that sequence the proof is released,
//! so that is the only thing these tests vary.
//!
//! [`Release::AfterVerdict`] is fd4fdff5 and is a positive control: it exists
//! nowhere but in this file, and it is what makes the same test body red.

use leviculum_settle_budget as budget;
use leviculum_upload_proof::{self as up, UploadProofs};

const LINK: [u8; 2] = [0x7a, 0x0f];
const HASH: up::PacketHash = [0x5e; 32];

/// The board's flush of one accepted record through the record-store task:
/// one channel round trip per op, milliseconds. Small next to the workblock
/// and not the term under test — it is here so the old ordering is priced
/// honestly rather than flatteringly.
const FLUSH_MS: u64 = 12;

/// Where the engine releases the packet proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Release {
    /// The fix: the moment the payload is in hand, before it is decoded.
    OnReceipt,
    /// fd4fdff5: after the stamp verdict and after the flush reported durable.
    AfterVerdict,
}

/// One wire- or CPU-visible step of the upload, with the milliseconds since
/// the client's packet landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// The link data proof goes out.
    Prove,
    /// One slice of the 1000-round propagation workblock is expanded.
    GrindSlice,
    /// The accepted record reaches flash.
    Flush,
}

/// Drive the engine's sequence for one upload and return its timeline.
fn timeline(release: Release) -> Vec<(u64, Step)> {
    let policy = budget::SettlePolicy::Sliced {
        rounds: budget::GRIND_SLICE_ROUNDS,
    };
    let pass = budget::price_pass(
        policy,
        budget::WORKBLOCK_ROUNDS_PN,
        budget::WORKBLOCK_US_NRF52840,
    );
    let slices = policy.passes_per_workblock(budget::WORKBLOCK_ROUNDS_PN);
    let slice_ms = pass.absence_us.div_ceil(1_000);

    let mut proofs: UploadProofs<[u8; 2]> = UploadProofs::new();
    let mut steps = Vec::new();
    let mut now = 0;

    // `on_event`: the core asks whether this packet gets a proof, then hands
    // the decrypted payload up.
    proofs.requested(LINK, HASH);
    let owed = proofs.on_receipt(&LINK).expect("a proof was requested");
    assert_eq!(owed, HASH);
    if release == Release::OnReceipt {
        steps.push((now, Step::Prove));
    }

    // `settle`: one slice per pass until the workblock is expanded.
    for _ in 0..slices {
        now += slice_ms;
        steps.push((now, Step::GrindSlice));
    }

    // The accepted record goes to flash.
    now += FLUSH_MS;
    steps.push((now, Step::Flush));

    if release == Release::AfterVerdict {
        steps.push((now, Step::Prove));
    }
    steps
}

fn proved_at(steps: &[(u64, Step)]) -> u64 {
    steps
        .iter()
        .find(|(_, step)| *step == Step::Prove)
        .map(|(at, _)| *at)
        .expect("every upload with a requested proof is proved")
}

#[test]
fn the_proof_goes_out_before_the_first_stamp_round() {
    let steps = timeline(Release::OnReceipt);

    assert_eq!(
        steps.first().map(|(_, step)| *step),
        Some(Step::Prove),
        "the proof states that the bytes arrived; nothing is allowed in front of it"
    );
    assert_eq!(proved_at(&steps), 0);
    assert!(
        steps.iter().any(|(_, step)| *step == Step::GrindSlice),
        "the grind still happens, it just happens after"
    );
}

#[test]
fn the_fixed_order_lands_inside_every_measured_ble_window() {
    let at = proved_at(&timeline(Release::OnReceipt));
    for rtt in [up::BLE_RTT_MIN_MS, up::BLE_RTT_MAX_MS] {
        assert!(
            up::proof_lands(rtt, at),
            "proof at {at} ms must reach a receipt open for {} ms",
            up::receipt_window_ms(rtt)
        );
    }
}

/// The positive control: the same body with fd4fdff5's release point, against
/// the workblock cost `leviculum-settle-budget` pins from the rig.
#[test]
fn the_old_order_misses_every_ble_window() {
    let at = proved_at(&timeline(Release::AfterVerdict));

    assert!(
        at >= u64::from(budget::WORKBLOCK_US_NRF52840) / 1_000,
        "the old order pays the whole workblock before it proves"
    );
    for rtt in [up::BLE_RTT_MIN_MS, up::BLE_RTT_MAX_MS] {
        assert!(
            !up::proof_lands(rtt, at),
            "proof at {at} ms cannot reach a receipt open for only {} ms",
            up::receipt_window_ms(rtt)
        );
    }
}

/// Why the same code looked healthy on the rig's LoRa cells: at SF7/BW125 a
/// propagation link's round trip is seconds, and `rtt * 6` is wider than the
/// walk. The carrier hid the defect; it did not fix it.
#[test]
fn the_old_order_survives_a_lora_round_trip() {
    let at = proved_at(&timeline(Release::AfterVerdict));
    let lora_rtt_ms = 1_200;

    assert!(up::proof_lands(lora_rtt_ms, at));
    assert!(
        up::receipt_window_ms(lora_rtt_ms) > u64::from(budget::WORKBLOCK_US_NRF52840) / 1_000,
        "a seconds-wide window is the only reason the LoRa cells pass"
    );
}
