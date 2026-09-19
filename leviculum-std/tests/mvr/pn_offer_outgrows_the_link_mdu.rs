//! mvr: a propagation node's `/offer` has to fit the link it is handed to.
//!
//! **The named failure mode:** a sync round whose store holds more offerable
//! records than one link-MDU request can name is refused *locally* by
//! [`NodeCore::send_request`] with `PayloadTooLarge`
//! (`packed.len() > link.mdu()`, `leviculum-core/src/node/mod.rs`), so not a
//! byte reaches the peer. On the board the only trace is one
//! `PN_SYNC … result=request_failed` line, and because a store only grows
//! between purges the next round is refused the same way: the peer stops
//! syncing silently and for good. Observed on 2026-09-18, the same board
//! passing `ble_pn_board_upload` with eight records offered and failing
//! `lora_pn_board_sync` with twelve in the store.
//!
//! This reproduces it host-side on a real link between two `NodeCore`s: same
//! offer codec ([`PeerOffer`]), same planner ([`build_offer`]), same
//! `send_request` path, same MDU arithmetic. No daemon, no LoRa, no Docker.
//!
//! **Ruling out the other three ways `send_request` can fail**, since the
//! symptom is one opaque `Err`: `LinkNotFound` and `LinkNotActive` are
//! excluded by the link being the one this test just established and drove a
//! successful request over; `EncryptionFailed` is excluded by the smaller
//! offer on the *same link object* encrypting and going out. Each test below
//! therefore reads a refusal as the MDU check and nothing else — and asserts
//! the error variant on top of that.
//!
//! The store sizes are derived from the link's own MDU rather than written
//! down, so the test keeps its meaning if the negotiated MTU ever changes.

use leviculum_core::node::request::{RequestError, RequestPolicy};
use leviculum_core::transport::TickOutput;
use leviculum_core::{
    Action, Clock, Destination, DestinationType, Direction, Identity, InterfaceId, MemoryStorage,
    NodeCore, NodeCoreBuilder, NodeEvent,
};
use leviculum_lxmf::peering::{
    answer_offer, build_offer, offer_budget_for_mdu, offer_encoded_len, response_action,
    OfferResponse, Peer, PeerOffer, PeerRecord, ResponseAction, OFFER_BYTES_LIMIT,
    OFFER_REQUEST_PATH, REQUEST_ENVELOPE_BYTES,
};
use leviculum_lxmf::propagation::TransientId;
use leviculum_lxmf::propagation_store::StoredMessage;
use rand_core::OsRng;

/// A standing clock: nothing here waits for a timer, and a deterministic
/// mvr must not depend on one.
#[derive(Clone, Copy)]
struct FixedClock;

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        1_700_000_000_000
    }

    fn wall_unix_secs(&self) -> Option<u64> {
        Some(1_700_000_000)
    }
}

type TestNode = NodeCore<OsRng, FixedClock, MemoryStorage>;

/// One store record, the shape [`build_offer`] scans. Small enough that
/// neither the peer's transfer limit nor its sync limit can be the thing
/// that bounds a round here — the offer's own byte budget has to be.
fn record(sequence: u64) -> StoredMessage {
    let mut transient_id = [0u8; 32];
    transient_id[..8].copy_from_slice(&sequence.to_be_bytes());
    StoredMessage {
        transient_id,
        destination_hash: [7; 16],
        size: 64,
        received_at: 1_700_000_000 + sequence,
        stamp_value: 16,
        sequence,
    }
}

/// A peer we hold a mined key for, with announce limits out of the way.
fn peer_at(cursor: u64) -> Peer {
    PeerRecord {
        destination_hash: [1; 16],
        identity_hash: Some([2; 16]),
        public_keys: None,
        peering_key: Some(([5; 32], 0)),
        transfer_limit_kb: 1_000,
        sync_limit_kb: 10_240,
        stamp_cost: 0,
        stamp_cost_flexibility: 0,
        peering_cost: 0,
        peering_timebase: 1,
        last_heard: 1,
        cursor,
        is_static: false,
    }
    .into_peer()
}

/// How many ids one request on a link with this MDU can name, derived from
/// the wire arithmetic rather than written down.
fn ids_that_fit(mdu: usize) -> usize {
    (1..)
        .find(|count| REQUEST_ENVELOPE_BYTES + offer_encoded_len(*count) > mdu)
        .expect("some id count exceeds any MDU")
        - 1
}

fn packets(output: &TickOutput) -> Vec<Vec<u8>> {
    output
        .actions
        .iter()
        .map(|action| match action {
            Action::SendPacket { data, .. } | Action::Broadcast { data, .. } => data.clone(),
        })
        .collect()
}

fn one_packet(output: &TickOutput) -> Vec<u8> {
    let mut all = packets(output);
    assert_eq!(all.len(), 1, "expected exactly one packet on the wire");
    all.remove(0)
}

/// The offering node and the peer it syncs toward, with one established
/// link between them and `/offer` served on the peer's destination.
struct Pair {
    offering: TestNode,
    peer: TestNode,
    link_id: leviculum_core::LinkId,
}

fn established_pair() -> Pair {
    let peer_identity = Identity::generate(&mut OsRng);
    let signing_key = peer_identity.ed25519_verifying().to_bytes();
    let mut peer = NodeCoreBuilder::new().build(OsRng, FixedClock, MemoryStorage::with_defaults());
    let mut destination = Destination::new(
        Some(peer_identity),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["propagation"],
    )
    .expect("propagation destination");
    destination.set_accepts_links(true);
    let destination_hash = *destination.hash();
    peer.register_destination(destination);
    peer.register_request_handler(
        destination_hash,
        OFFER_REQUEST_PATH,
        RequestPolicy::AllowAll,
    );

    let mut offering =
        NodeCoreBuilder::new().build(OsRng, FixedClock, MemoryStorage::with_defaults());
    let (link_id, _, output) = offering
        .connect(destination_hash, &signing_key)
        .expect("connect to the peer");

    let proof = peer.handle_packet(InterfaceId(0), &one_packet(&output));
    let established = offering.handle_packet(InterfaceId(0), &one_packet(&proof));
    assert!(
        established
            .events
            .iter()
            .any(|event| matches!(event, NodeEvent::LinkEstablished { .. })),
        "the offering node's link must come up before anything is offered"
    );
    let _ = peer.handle_packet(InterfaceId(0), &one_packet(&established));

    Pair {
        offering,
        peer,
        link_id,
    }
}

impl Pair {
    fn link_mdu(&self) -> usize {
        self.offering
            .link(&self.link_id)
            .expect("the link this test established")
            .mdu()
    }

    /// Place one `/offer` on the link, exactly as both engines do.
    fn offer(&mut self, ids: &[TransientId]) -> Result<(TickOutput, [u8; 16]), RequestError> {
        let offer = PeerOffer {
            peering_key: [5; 32],
            transient_ids: ids.to_vec(),
        };
        self.offering
            .send_request(
                &self.link_id,
                OFFER_REQUEST_PATH,
                Some(&offer.encode()),
                Some(60_000),
            )
            .map(|(request_id, output)| (output, request_id))
    }

    /// Carry the offer to the peer, answer it out of the peer's (empty)
    /// store, and carry the answer back. Returns the decoded response.
    fn round_trip(&mut self, output: TickOutput) -> OfferResponse {
        let inbound = self
            .peer
            .handle_packet(InterfaceId(0), &one_packet(&output));
        let (request_id, data) = inbound
            .events
            .iter()
            .find_map(|event| match event {
                NodeEvent::RequestReceived {
                    request_id,
                    path,
                    data,
                    ..
                } if path == OFFER_REQUEST_PATH => Some((*request_id, data.clone())),
                _ => None,
            })
            .expect("the peer must see the /offer request");
        let offer = PeerOffer::decode(&data).expect("the peer decodes the offer body");
        let answer = answer_offer(&offer.transient_ids, |_| false);
        let sent = self
            .peer
            .send_response(&self.link_id, &request_id, &answer.encode())
            .expect("the answer fits the same link the offer came in on");
        let back = self
            .offering
            .handle_packet(InterfaceId(0), &one_packet(&sent));
        back.events
            .iter()
            .find_map(|event| match event {
                NodeEvent::ResponseReceived { response_data, .. } => {
                    Some(OfferResponse::decode(response_data).expect("decodable answer"))
                }
                _ => None,
            })
            .expect("the offering node must see the answer")
    }
}

/// The defect itself: the store outgrows one request, and the round dies
/// before a byte leaves the node.
#[test]
fn an_offer_planned_against_the_ram_ceiling_is_refused_by_the_link() {
    let mut pair = established_pair();
    let mdu = pair.link_mdu();
    let fits = ids_that_fit(mdu);

    // A store two records past what this link can name in one request —
    // the twelve-against-eight shape of the failing hardware run.
    let store: Vec<StoredMessage> = (1..=(fits as u64 + 2)).map(record).collect();
    let plan = build_offer(&peer_at(0), &store, OFFER_BYTES_LIMIT).expect("something to offer");
    assert_eq!(
        plan.ids.len(),
        store.len(),
        "premise: the old bound plans the whole store into one request"
    );
    assert!(matches!(
        pair.offer(&plan.ids),
        Err(RequestError::PayloadTooLarge)
    ));

    // Same link, same moment: an offer sized to the link goes out. That is
    // what excludes LinkNotFound / LinkNotActive / EncryptionFailed as the
    // cause of the refusal above.
    let plan = build_offer(&peer_at(0), &store, offer_budget_for_mdu(mdu)).expect("a bounded plan");
    assert_eq!(plan.ids.len(), fits);
    let (output, _) = pair
        .offer(&plan.ids)
        .expect("the bounded offer is accepted");
    assert_eq!(pair.round_trip(output), OfferResponse::WantAll);
}

/// The budget is exact, not merely conservative: one more id than
/// [`offer_budget_for_mdu`] allows is what the link refuses.
#[test]
fn the_offer_budget_sits_exactly_on_the_link_mdu() {
    let mut pair = established_pair();
    let mdu = pair.link_mdu();
    let store: Vec<StoredMessage> = (1..=200).map(record).collect();
    let plan = build_offer(&peer_at(0), &store, offer_budget_for_mdu(mdu)).expect("a plan");

    let mut one_too_many = plan.ids.clone();
    one_too_many.push(store[plan.ids.len()].transient_id);
    assert!(
        matches!(
            pair.offer(&one_too_many),
            Err(RequestError::PayloadTooLarge)
        ),
        "one id past the budget must be what the link refuses"
    );
    // And the budget itself is not a byte short of the limit.
    assert!(
        pair.offer(&plan.ids).is_ok(),
        "the accounted budget must be the link's own limit, not an arbitrary smaller one"
    );
}

/// The fix must not trade a stalled sync for a lossy one: bounded rounds
/// cover every record, in order, once.
#[test]
fn bounded_rounds_drain_a_store_the_link_cannot_name_at_once() {
    let mut pair = established_pair();
    let budget = offer_budget_for_mdu(pair.link_mdu());
    let store: Vec<StoredMessage> = (1..=50).map(record).collect();

    let fits = ids_that_fit(pair.link_mdu());
    let mut cursor = 0u64;
    let mut offered: Vec<TransientId> = Vec::new();
    let mut rounds = 0;
    while let Some(plan) = build_offer(&peer_at(cursor), &store, budget) {
        rounds += 1;
        assert!(rounds <= store.len(), "the drain did not terminate");
        let (output, _) = pair
            .offer(&plan.ids)
            .expect("every bounded round fits the link");
        let response = pair.round_trip(output);
        assert_eq!(response, OfferResponse::WantAll);
        assert_eq!(
            response_action(&response, &plan),
            ResponseAction::SendMessages(plan.ids.clone())
        );
        offered.extend_from_slice(&plan.ids);
        // What `conclude_round` does once the round's messages are through.
        cursor = plan.cursor_target;
    }

    let all: Vec<TransientId> = store.iter().map(|entry| entry.transient_id).collect();
    assert_eq!(
        offered, all,
        "every record must be offered exactly once, in append order"
    );
    assert_eq!(
        rounds,
        store.len().div_ceil(fits),
        "{} records at {fits} per round over a {}-byte MDU",
        store.len(),
        pair.link_mdu()
    );
}
