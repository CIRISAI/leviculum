//! mvr: serving one `/get` fetch costs the heap several times the
//! response that a cap was supposed to bound.
//!
//! **The named failure mode:** a board accepts mail, a peer collects it,
//! and the board panics in `alloc` while building the answer. Its own
//! record from the boot after, 2026-09-22:
//!
//! ```text
//! PANIC_PMRT … alloc.rs "memory allocation of 5446 bytes failed"
//! RESET_REASON sreq=1        BOOT_TRACE prev_phase=main-loop
//! ```
//!
//! with `PN_GET dst=… form=fetch count=24 bytes=5376 purged=0` as the last
//! line of the boot before. The request arrived, reassembled and was
//! served; the board died between the serve and the response, and the
//! Python peer saw only `PR_TRANSFER_FAILED` — a timeout, not a crash.
//!
//! **What this test covers and what it cannot.** It cannot reproduce the
//! panic: only a 96 KiB heap can refuse 5 446 bytes, and this runs on a
//! host with gigabytes. What it reproduces is the *arithmetic* — how
//! many response-sized blocks the serve path holds live at once — by
//! walking the real path (`MessageGetResponse::encode`, `send_response`,
//! `send_response_resource`, `OutgoingResource::new_with_flags`) over a
//! real established link with a counting allocator underneath. The
//! measurement is compared against [`serve_peak_bytes`], the model the
//! board's heap budget carries, in both directions — the model must
//! bound reality, and must not be twice too loose to be worth sizing a
//! 96 KiB heap with.
//!
//! **It is also the instrument that priced the fix.** Measured on this
//! host, before and after #384 B1 (the four copies the path made for
//! nothing):
//!
//! ```text
//! response 5 427 B      before        after
//!   single-packet refusal  5 465 B       0 B
//!   resource-path peak    51 662 B  29 842 B
//!   modelled at the 8 KB cap
//!                         97 036 B  48 922 B
//! ```
//!
//! The refusal is the headline: `send_response` compares a computed
//! length against the MDU now, so the allocation the board actually died
//! in does not happen at all. The rest is the peak, down 42 %, which is
//! what a heap plan has to carry.
//!
//! **Only a board can prove the rest**: that the failing allocation is
//! the one named above rather than a later one on the same path, and
//! that whatever cap the heap plan ends up funding survives a real
//! collection over LoRa. What this proves is the size of the hole.

use leviculum_core::node::request::{RequestError, RequestPolicy};
use leviculum_core::transport::TickOutput;
use leviculum_core::{
    Action, Clock, Destination, DestinationType, Direction, Identity, InterfaceId, MemoryStorage,
    NodeCore, NodeCoreBuilder, NodeEvent,
};
use leviculum_lxmf::propagation::{
    MessageGetRequest, PropagationUpload, TransferLimit, TransientId,
};
use leviculum_lxmf::propagation_node::{
    serve_peak_bytes, GetOutcome, PropagationNode, PropagationNodeConfig, RESPONSE_FRAME_BYTES,
};
use leviculum_lxmf::propagation_store::MemoryPropagationStore;
use rand_core::OsRng;

use crate::alloc_probe;

/// The path both engines serve a mailbox fetch on
/// (`reference/LXMF/LXMF/LXMRouter.py:1489`).
const GET_REQUEST_PATH: &str = "/get";

/// The board announces this per-sync limit and caps its own fetch
/// responses at it (`BOARD_SYNC_LIMIT_KB`, `leviculum-nrf/src/pn.rs`).
const BOARD_SYNC_LIMIT_KB: u64 = 8;

/// The field shape: 24 stored messages, 224 B of body each.
const FIELD_MESSAGES: u8 = 24;
const FIELD_BODY_BYTES: usize = 224;

#[derive(Clone, Copy)]
pub(crate) struct FixedClock;

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        1_700_000_000_000
    }

    fn wall_unix_secs(&self) -> Option<u64> {
        Some(1_700_000_000)
    }
}

pub(crate) type TestNode = NodeCore<OsRng, FixedClock, MemoryStorage>;

fn one_packet(output: &TickOutput) -> Vec<u8> {
    let mut all: Vec<Vec<u8>> = output
        .actions
        .iter()
        .map(|action| match action {
            Action::SendPacket { data, .. } | Action::Broadcast { data, .. } => data.clone(),
        })
        .collect();
    assert_eq!(all.len(), 1, "expected exactly one packet on the wire");
    all.remove(0)
}

/// The serving node and the client that fetched from it, with one
/// established link and `/get` served on the serving node's destination.
///
/// `pub(crate)` for `pn_serve_cap_survives_a_shrinking_heap`, which
/// measures the same serve path from the other end — what it costs
/// while the heap moves under it. One builder, so the two measurements
/// cannot drift onto different links.
pub(crate) struct Pair {
    pub(crate) serving: TestNode,
    pub(crate) link_id: leviculum_core::LinkId,
}

pub(crate) fn established_pair() -> Pair {
    let serving_identity = Identity::generate(&mut OsRng);
    let signing_key = serving_identity.ed25519_verifying().to_bytes();
    let mut serving =
        NodeCoreBuilder::new().build(OsRng, FixedClock, MemoryStorage::with_defaults());
    let mut destination = Destination::new(
        Some(serving_identity),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["propagation"],
    )
    .expect("propagation destination");
    destination.set_accepts_links(true);
    let destination_hash = *destination.hash();
    serving.register_destination(destination);
    serving.register_request_handler(destination_hash, GET_REQUEST_PATH, RequestPolicy::AllowAll);

    let mut client =
        NodeCoreBuilder::new().build(OsRng, FixedClock, MemoryStorage::with_defaults());
    let (_, _, output) = client
        .connect(destination_hash, &signing_key)
        .expect("connect to the serving node");

    let proof = serving.handle_packet(InterfaceId(0), &one_packet(&output));
    let established = client.handle_packet(InterfaceId(0), &one_packet(&proof));
    assert!(
        established
            .events
            .iter()
            .any(|event| matches!(event, NodeEvent::LinkEstablished { .. })),
        "the link must come up before anything is served"
    );
    let inbound = serving.handle_packet(InterfaceId(0), &one_packet(&established));
    let link_id = inbound
        .events
        .iter()
        .find_map(|event| match event {
            NodeEvent::LinkEstablished { link_id, .. } => Some(*link_id),
            _ => None,
        })
        .expect("the serving side must see its link come up");

    Pair { serving, link_id }
}

/// A stored message of exactly `FIELD_BODY_BYTES` servable bytes, whose
/// body does not compress.
///
/// Incompressible on purpose: a real LXMF body is ciphertext, which is
/// why the firmware's dependency on `leviculum-lxmf` drops the bz2
/// feature entirely. A run of constant bytes would let this host's
/// compressor shrink the payload and measure a path no board takes. The
/// filler is a cheap xorshift so the bytes are deterministic and the
/// test stays reproducible.
fn upload(seed: u8) -> Vec<u8> {
    let mut lxmf_data = vec![7u8; 16];
    let mut state = 0x2545_F491_4F6C_DD1Du64 ^ u64::from(seed);
    while lxmf_data.len() < FIELD_BODY_BYTES {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        lxmf_data.extend_from_slice(&state.to_le_bytes());
    }
    lxmf_data.truncate(FIELD_BODY_BYTES);
    PropagationUpload::single(1_700_000_000.0, lxmf_data, [0xEE; 32]).encode()
}

/// The field fetch, served out of a real store through the real codec.
fn field_fetch_response() -> Vec<u8> {
    let mut role = PropagationNode::new(
        MemoryPropagationStore::new(64 * 1024),
        PropagationNodeConfig {
            sync_limit_kb: BOARD_SYNC_LIMIT_KB,
            ..PropagationNodeConfig::default()
        },
    );
    let wants: Vec<TransientId> = (0..FIELD_MESSAGES)
        .map(
            |seed| match role.handle_upload(&upload(seed), 0, |_, _| None) {
                leviculum_lxmf::propagation_node::UploadOutcome::Accepted {
                    transient_id, ..
                } => transient_id,
                other => panic!("the store must accept the field shape: {other:?}"),
            },
        )
        .collect();
    let fetch = MessageGetRequest {
        wants: Some(wants),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(1000)),
    };
    let GetOutcome::Fetch {
        response,
        served,
        served_bytes,
        ..
    } = role
        .handle_get(&fetch.encode().unwrap(), &[7; 16], 0)
        .expect("the fetch is well formed")
    else {
        panic!("a fetch request must produce a fetch outcome");
    };
    assert_eq!(served.len(), usize::from(FIELD_MESSAGES));
    assert_eq!(served_bytes, 5376, "the board's own PN_GET bytes= figure");
    response
}

/// Serving one fetch still holds several copies of its own response --
/// and the one the board died in, the framed copy a refusal used to
/// build before it checked the MDU, is no longer among them.
#[test]
fn one_fetch_serve_holds_many_copies_of_its_own_response() {
    let response = field_fetch_response();
    let request_id = [0x5Au8; 16];
    let framed = response.len() + RESPONSE_FRAME_BYTES;

    let mut refusing = established_pair();
    // The response does not fit one packet on this link -- the premise of
    // the whole resource path below, asserted rather than assumed.
    let mdu = refusing
        .serving
        .link(&refusing.link_id)
        .expect("the link this test established")
        .mdu();
    assert!(
        response.len() > mdu,
        "the field response ({}) must outgrow the link MDU ({mdu})",
        response.len()
    );

    // 1. The single-packet path, and the allocation the board died in.
    //    `send_response` used to frame the whole response BEFORE it
    //    compared the result with the MDU, so a response it was going to
    //    refuse still cost a full framed copy: 5 446 B, the number in
    //    the board's PANIC_PMRT line. It died here, before the resource
    //    path it would have fallen back to ever started. The check now
    //    runs on a length computed from `bin_len`, so the refusal must
    //    cost nothing at all -- not "less", zero.
    let peak_refusal = {
        let probe = alloc_probe::Probe::armed();
        let refused = refusing
            .serving
            .send_response(&refusing.link_id, &request_id, &response);
        let peak = probe.peak();
        assert!(
            matches!(refused, Err(RequestError::PayloadTooLarge)),
            "the single-packet path must refuse it: {refused:?}"
        );
        peak
    };
    assert_eq!(
        peak_refusal, 0,
        "a refusal must allocate nothing; it used to allocate the whole \
         {framed} B frame, which is what killed a T114"
    );

    // 2. What this host adds that a board does not: `leviculum-std` links
    //    bzip2 and the firmware does not, so the resource constructor
    //    here runs a compressor that allocates scratch no board pays for.
    //    Calibrated rather than guessed, on a payload small enough that
    //    everything else in the window is under a kilobyte.
    let mut calibrating = established_pair();
    let host_scratch = {
        let probe = alloc_probe::Probe::armed();
        let _ = calibrating
            .serving
            .send_response_resource(&calibrating.link_id, &request_id, &[0xc4, 1, 0x00])
            .expect("a three-byte response is a legal resource");
        let peak = probe.peak();
        let largest = alloc_probe::largest_block();
        // Positive control for the block ceiling: with the compressor's
        // scratch excluded, everything left in a three-byte serve is
        // under a kilobyte. If this ever creeps toward the ceiling the
        // filter has started hiding real work.
        assert!(
            largest < 1024 && peak < 4096,
            "the host allowance is no longer just the compressor: \
             peak {peak} B, largest block {largest} B"
        );
        peak
    };

    // 3. The resource path on the real response.
    let mut serving = established_pair();
    let (peak_resource, largest_block) = {
        let probe = alloc_probe::Probe::armed();
        let _ = serving
            .serving
            .send_response_resource(&serving.link_id, &request_id, &response)
            .expect("the resource path must take it");
        (probe.peak(), alloc_probe::largest_block())
    };
    // Nothing board-side was filtered out: every block the counted path
    // made is under the ceiling that excludes the compressor.
    assert!(
        largest_block < alloc_probe::BLOCK_CEILING_BYTES,
        "a counted allocation ({largest_block} B) reached the ceiling \
         that is supposed to exclude only host-only scratch"
    );

    // The response itself is allocated before the window and stays live
    // through all of it -- the caller owns it -- so its block belongs in
    // the figure.
    let board_peak = peak_resource + response.capacity();

    let cap = (BOARD_SYNC_LIMIT_KB * 1000) as usize;
    let sdu = leviculum_core::resource::resource_sdu(500);
    let modelled_here = serve_peak_bytes(response.len(), sdu);
    let modelled_at_cap = serve_peak_bytes(cap, sdu);

    eprintln!(
        "SERVE_PEAK response={} framed={framed} refusal={peak_refusal} \
board_peak={board_peak} host_scratch={host_scratch} \
modelled_here={modelled_here} modelled_at_cap={modelled_at_cap}",
        response.len()
    );

    // The model must BOUND what runs, or the board's heap budget is
    // describing something other than the code.
    assert!(
        board_peak <= modelled_here,
        "measured {board_peak} B exceeds the modelled {modelled_here} B -- \
         the serve path grew a copy the heap budget does not know about"
    );
    // And it must not be vacuous. A bound twice too loose would pass the
    // line above while telling a 96 KiB budget nothing useful.
    assert!(
        modelled_here <= 2 * board_peak,
        "modelled {modelled_here} B is more than twice the measured \
         {board_peak} B -- too loose to size a 96 KiB heap with"
    );

    // The finding, as one number, and the fix, as the other. A response
    // an 8 KB cap was supposed to bound still costs the heap multiples
    // of its own length -- five copies is what the resource path is
    // structurally worth until it streams (#384 B2). But it must cost
    // well under the nine times it cost before B1, or the copies this
    // batch removed have quietly come back.
    assert!(
        board_peak >= 4 * response.len(),
        "serving a {} B response cost only {board_peak} B of transient; \
         the finding is that it costs multiples of it",
        response.len()
    );
    assert!(
        board_peak <= 6 * response.len(),
        "serving a {} B response cost {board_peak} B -- more than six \
         times it, so a copy B1 removed is back on the path",
        response.len()
    );
}
