use super::*;
use crate::peering::{PeerTable, PeeringConfig, SyncPhase, SYNC_BACKOFF_STEP_SECS};
use crate::propagation::PropagationNodeAnnounce;

const LINK: LinkId = LinkId::new([0x51; 16]);
const OTHER_LINK: LinkId = LinkId::new([0x52; 16]);
const REQUEST: [u8; 16] = [0x61; 16];
const KEY: [u8; 32] = [0x7c; 32];
/// The MDU a LoRa link reports, the one the board sizes its offer against.
const LORA_MDU: usize = 431;
const PEER: [u8; 16] = [0x3a; 16];
/// A monotonic millisecond some hours into a board's uptime.
const DIALLED_MS: u64 = 7_200_000;
/// The emission clock, in seconds, at the same moment.
const NOW: u64 = 1_760_000_000;

fn round(request_id: Option<[u8; 16]>) -> RoundView {
    RoundView {
        link_id: LINK,
        request_id,
        deadline_ms: round_deadline_ms(DIALLED_MS),
    }
}

fn link_up(identified: bool, peering_key: Option<[u8; 32]>, link_mdu: Option<usize>) -> RoundEvent {
    RoundEvent::LinkUp {
        identified,
        peering_key,
        link_mdu,
    }
}

/// A table holding one announced peer with something above its cursor,
/// booked `attempts` times at `NOW` as `start_round` books it, mid-dial.
fn booked_table(attempts: u64) -> PeerTable {
    let mut table = PeerTable::new(PeeringConfig::default());
    let announce = PropagationNodeAnnounce {
        legacy_support: false,
        timebase: NOW,
        enabled: true,
        transfer_limit_kb: 256,
        sync_limit_kb: 10_240,
        stamp_cost: 13,
        stamp_cost_flexibility: 3,
        peering_cost: 1,
        metadata: alloc::vec![],
    };
    table.handle_announce(PEER, &announce, Some(1), NOW, false);
    let Some(peer) = table.get_mut(&PEER) else {
        panic!("the announce created no peer");
    };
    for _ in 0..attempts {
        peer.book_sync_attempt(NOW);
    }
    peer.state = SyncPhase::LinkEstablishing;
    table
}

/// What the caller does with an end: the peer back to Idle
/// (`finish_round`). Returns when the peer is next due, by
/// `PeerTable::next_due`, scanning forward one second at a time.
fn ended_and_next_due(table: &mut PeerTable) -> u64 {
    if let Some(peer) = table.get_mut(&PEER) {
        peer.state = SyncPhase::Idle;
    }
    (NOW..NOW + 10 * SYNC_BACKOFF_STEP_SECS)
        .find(|now| table.next_due(*now, 1, None).is_some())
        .unwrap_or(u64::MAX)
}

/// The happy path pins the function's shape: an identified link, a peer
/// with a key and a live link offer against that link's MDU.
#[test]
fn an_identified_link_with_a_key_offers_against_its_own_mdu() {
    assert_eq!(
        round_step(Some(&round(None)), link_up(true, Some(KEY), Some(LORA_MDU))),
        RoundStep::Offer {
            peering_key: KEY,
            link_mdu: LORA_MDU,
        }
    );
}

/// The identify could not be sent on the fresh link. The peer never
/// answered as a peer, so `note_link_established` is not called and the
/// peer stays deferred by its whole accumulator: on its third attempt, 36
/// minutes. That holds even when the key and the link are missing too,
/// because the identify is checked first.
#[test]
fn a_link_that_cannot_identify_ends_the_round_and_keeps_the_peer_deferred() {
    for (key, mdu) in [(Some(KEY), Some(LORA_MDU)), (None, None)] {
        assert_eq!(
            round_step(Some(&round(None)), link_up(false, key, mdu)),
            RoundStep::End(RoundEnd::IdentifyFailed)
        );
    }
    assert_eq!(RoundEnd::IdentifyFailed.as_str(), "identify_failed");
    let mut table = booked_table(3);
    assert_eq!(
        ended_and_next_due(&mut table),
        NOW + 3 * SYNC_BACKOFF_STEP_SECS
    );
}

/// The link identified but the peer holds no peering key (it was cleared
/// by a `RemineKey` answer between scheduling and link-up). The link came
/// up, so the booking is cleared and the peer is due on the next pass,
/// where `start_round` hands it to the miner.
#[test]
fn an_identified_link_to_a_peer_without_a_key_ends_no_key_and_leaves_it_due() {
    assert_eq!(
        round_step(Some(&round(None)), link_up(true, None, Some(LORA_MDU))),
        RoundStep::End(RoundEnd::NoKey)
    );
    assert_eq!(RoundEnd::NoKey.as_str(), "no_key");
    let mut table = booked_table(2);
    if let Some(peer) = table.get_mut(&PEER) {
        peer.note_link_established();
    }
    assert_eq!(ended_and_next_due(&mut table), NOW);
}

/// The peer identified and then vanished: the core dropped the link before
/// the offer could be sized against it.
#[test]
fn a_link_gone_before_the_offer_is_sized_ends_link_gone() {
    assert_eq!(
        round_step(Some(&round(None)), link_up(true, Some(KEY), None)),
        RoundStep::End(RoundEnd::LinkGone)
    );
    assert_eq!(RoundEnd::LinkGone.as_str(), "link_gone");
}

/// The round's own `/offer` timed out on the round's own link (the core's
/// 60 s request timeout, well inside the round's deadline). A timeout of
/// another request, of the same id on another link, or before the offer
/// was sent at all is not this round's.
#[test]
fn only_the_rounds_own_offer_timing_out_ends_it() {
    let sent = round(Some(REQUEST));
    let timed_out = |link_id, request_id| RoundEvent::RequestTimedOut {
        link_id,
        request_id,
    };
    assert_eq!(
        round_step(Some(&sent), timed_out(LINK, REQUEST)),
        RoundStep::End(RoundEnd::OfferTimeout)
    );
    assert_eq!(RoundEnd::OfferTimeout.as_str(), "offer_timeout");
    assert_eq!(
        round_step(Some(&sent), timed_out(LINK, [0x62; 16])),
        RoundStep::Unchanged
    );
    assert_eq!(
        round_step(Some(&sent), timed_out(OTHER_LINK, REQUEST)),
        RoundStep::Unchanged
    );
    assert_eq!(
        round_step(Some(&round(None)), timed_out(LINK, REQUEST)),
        RoundStep::Unchanged
    );
    assert_eq!(
        round_step(None, timed_out(LINK, REQUEST)),
        RoundStep::Unchanged
    );
}

/// A round whose link never comes up, or comes up and stalls, ends 180 s
/// after the dial and not a millisecond before. Ended before link-up, it
/// leaves the booking standing: the unreachable peer is retried 12, then
/// 24 minutes on.
#[test]
fn a_round_past_its_deadline_times_out_and_one_at_it_does_not() {
    let dialled = round(None);
    assert_eq!(dialled.deadline_ms, DIALLED_MS + 180_000);
    let tick = |now_ms| RoundEvent::Tick { now_ms };
    assert_eq!(
        round_step(Some(&dialled), tick(DIALLED_MS + 179_999)),
        RoundStep::Unchanged
    );
    assert_eq!(
        round_step(Some(&dialled), tick(DIALLED_MS + 180_000)),
        RoundStep::Unchanged
    );
    assert_eq!(
        round_step(Some(&dialled), tick(DIALLED_MS + 180_001)),
        RoundStep::End(RoundEnd::Timeout)
    );
    assert_eq!(RoundEnd::Timeout.as_str(), "timeout");
    assert_eq!(round_step(None, tick(u64::MAX)), RoundStep::Unchanged);
    for attempts in [1, 2] {
        let mut table = booked_table(attempts);
        assert_eq!(
            ended_and_next_due(&mut table),
            NOW + attempts * SYNC_BACKOFF_STEP_SECS
        );
    }
}

/// The core's link cap refused the dial: no round opens, and the booking
/// `start_round` made before dialling stands, so a table of peers behind a
/// full link cap is not redialled on every sync pass.
#[test]
fn a_dial_the_link_cap_refuses_defers_the_peer_by_its_booking() {
    assert_eq!(round_step(None, RoundEvent::DialRefused), RoundStep::Defer);
    let mut table = booked_table(1);
    assert_eq!(ended_and_next_due(&mut table), NOW + SYNC_BACKOFF_STEP_SECS);
}

/// A link-up with no round in flight (the round already ended on its
/// deadline) is not acted on.
#[test]
fn a_link_up_after_the_round_ended_changes_nothing() {
    assert_eq!(
        round_step(None, link_up(true, Some(KEY), Some(LORA_MDU))),
        RoundStep::Unchanged
    );
}
