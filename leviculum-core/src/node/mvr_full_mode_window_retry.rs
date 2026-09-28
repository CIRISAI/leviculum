//! mvr: a Full-mode discovery window is opened once and retried never
//! (Codeberg #433, the stream that outlived the 375 suppression).
//!
//! ## The field failure this reproduces
//!
//! Measured live on 2026-09-28 (05:35Z, the base T114's if02 read
//! directly, order 385): with both field helpers dead since the
//! 2026-09-27 field day, a phone kept retrying path requests for their
//! two LXMF destinations over BLE. The board held a cached announce for
//! each (the helpers had announced all day) but no path (the daemon
//! carrying them was gone), so every request took the #117
//! cached-announce-no-path arm of `handle_path_request` and opened a
//! pending discovery window — on a **Full-mode** interface, where the
//! reference opens nothing at all. `retry_pending_discoveries` then
//! re-emitted each window every `DISCOVERY_RETRY_INTERVAL_MS` = 5 s
//! with a FRESH random tag onto LoRa and serial, and the requester's
//! next retry re-opened the window the moment it expired: 34 frames in
//! 90 s on the serial capture, every one built by the board itself
//! (requestor transport id = its own identity hash), tags all distinct,
//! timestamps on an exact 5-second grid — for two destinations that no
//! longer exist, for hours. The neighbour boards cross-heard the fresh
//! tags (dedup never bites a fresh tag) and re-keyed them onto THEIR
//! other interfaces at the same cadence.
//!
//! The 375 suppression does not touch this: it gates what an INCOMING
//! request re-originates, and the retry loop is not an incoming
//! request.
//!
//! ## The reference
//!
//! Neither reference line retries a pending discovery at all — the
//! entry is written once and only ever culled by timeout:
//!
//! - 1.3.5 (`reference/Reticulum/RNS/Transport.py:3031`): the entry is
//!   inserted with a 15 s timeout, read again only when the answer
//!   arrives (`:1984`) and popped by the jobs cull (`:905-907`).
//!   Nothing re-sends it.
//! - 1.5.2 (`Transport.py:3561`, wheel under
//!   `~/.local/state/leviculum-ci/rns-1.5.2`): same shape, culled at
//!   `:1005-1011`.
//!
//! And neither opens a discovery on a Full interface in the first
//! place: `should_search_for_unknown` requires the mode in
//! `DISCOVER_PATHS_FOR` (1.3.5 `Transport.py:2912-2918`,
//! `Interface.py:54`; 1.5.2 `:3421-3433` adds `recursive_prs`,
//! `MODE_INTERNAL` and a filtered boundary arm — still never Full).
//! Our Full-mode window is the #117 deviation (a destination we have
//! provably seen but hold no current path to is re-originated instead
//! of served stale); the deviation's argument covers the single
//! re-origination, not a 0.4 Hz forever-stream on the shared medium.
//!
//! ## The rule
//!
//! A window whose REQUESTING interface is in a `DISCOVER_PATHS_FOR`
//! mode keeps the retry cadence (the operator opted into active
//! discovery; the reference at least re-originates there). A window
//! whose requesting interface does not discover paths — the #117 arm's
//! windows — gets its one re-origination at open and no retries. The
//! withheld retries are counted (`path_request_retry_withheld`, on the
//! PKT_DROP_SUMMARY line beside `path_request_pending_suppressed`,
//! outside `total` for the same reason: airtime saved is not loss).
//!
//! ## Shape
//!
//! The field shape at 1 Hz: Full-mode peer ingress (the phone's BLE
//! link), Full-mode LoRa egress, one requester retrying every second
//! with a fresh tag, two relays. Deterministic, single named failure
//! mode: the count of re-originated path requests per pending window.

extern crate std;

use std::boxed::Box;
use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{MTU, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction};
use crate::embedded_storage::EmbeddedStorage;
use crate::identity::Identity;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{Packet, PacketType};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Clock, InterfaceMode, Storage};
use crate::transport::{Action, InterfaceId, TickOutput};

/// Both boards' exact shape: `EmbeddedStorage`, transport enabled.
type EmbeddedNode = NodeCore<OsRng, MockClock, EmbeddedStorage>;

/// The phone's link and the shared medium, in the boards' index order.
const BLE: usize = 0;
const LORA: usize = 1;

/// A board as the field ran it: transport on, every interface Full —
/// `leviculum-nrf` never names `InterfaceMode`, so the boards run the
/// trait default (`traits.rs::InterfaceMode`), and lnsd's BLE and
/// serial interfaces default the same way.
fn make_board() -> Box<EmbeddedNode> {
    let mut node = NodeCoreBuilder::new().enable_transport(true).build_boxed(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        EmbeddedStorage::new(),
    );
    add_iface(&mut node, "ble_peer", 0);
    add_iface(&mut node, "lora_sx1262", 1);
    node.set_interface_mode(BLE, InterfaceMode::Full);
    node.set_interface_mode(LORA, InterfaceMode::Full);
    node
}

fn add_iface(node: &mut EmbeddedNode, name: &'static str, id: u8) -> usize {
    let idx = node
        .transport
        .register_interface(Box::new(MockInterface::new(name, id)));
    node.set_interface_name(idx, String::from(name));
    idx
}

/// The dead helper's destination: announced while it lived, gone now.
fn make_dest() -> Destination {
    Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["delivery"],
    )
    .unwrap()
}

/// Arm the #117 state on the board: a cached announce for `dest` with
/// no path entry. The announce is ingested for real over `iface` (which
/// caches it and installs the path), then the path alone is dropped —
/// the state a path expiry or an interface loss leaves behind while the
/// announce cache's longer retention keeps the raw announce.
fn arm_cached_announce_no_path(board: &mut EmbeddedNode, dest: &mut Destination, iface: usize) {
    let ts = board.transport().clock().now_ms();
    let announce = dest.announce(None, &mut OsRng, ts, ts / 1000).unwrap();
    let mut buf = [0u8; MTU];
    let len = announce.pack(&mut buf).unwrap();
    let _ = board.handle_packet(InterfaceId(iface), &buf[..len]);

    let d = *dest.hash().as_bytes();
    assert!(
        board.transport.storage_mut().remove_path(&d).is_some(),
        "premise: the announce installed a path to drop"
    );
    assert!(
        board.transport().storage().get_announce_cache(&d).is_some(),
        "premise: the announce cache survives the dropped path"
    );
}

/// Path requests naming `dest` in this output, each as the set of
/// interfaces the frame reaches: a `SendPacket` reaches its target, a
/// `Broadcast` reaches every registered interface except its excluded
/// one.
fn path_requests_for(
    node: &EmbeddedNode,
    out: &TickOutput,
    dest: &[u8; TRUNCATED_HASHBYTES],
) -> Vec<Vec<usize>> {
    let pr_hash = *node.transport().path_request_hash();
    out.actions
        .iter()
        .filter_map(|action| {
            let (reach, data) = match action {
                Action::SendPacket { iface, data, .. } => (std::vec![iface.0], data),
                Action::Broadcast {
                    data,
                    exclude_iface,
                    ..
                } => (
                    [BLE, LORA]
                        .into_iter()
                        .filter(|&i| Some(InterfaceId(i)) != *exclude_iface)
                        .collect(),
                    data,
                ),
            };
            let p = Packet::unpack(data).ok()?;
            (p.flags.packet_type == PacketType::Data
                && p.destination_hash == pr_hash
                && p.data.as_slice().len() >= TRUNCATED_HASHBYTES
                && &p.data.as_slice()[..TRUNCATED_HASHBYTES] == dest)
                .then_some(reach)
        })
        .collect()
}

/// One retry as the phone puts it on the wire: a 48-byte transport-form
/// path request with a FRESH random tag, built from a throwaway node so
/// the tag is new every call — exactly as `Transport.request_path`
/// draws one per call, which is why tag dedup never bites.
fn fresh_retry(dest: &[u8; TRUNCATED_HASHBYTES]) -> Vec<u8> {
    let mut requester = make_board();
    let pr_hash = *requester.transport().path_request_hash();
    let out = requester.request_path(&crate::DestinationHash::new(*dest));
    out.actions
        .into_iter()
        .map(|action| match action {
            Action::SendPacket { data, .. } => data,
            Action::Broadcast { data, .. } => data,
        })
        .find(|data| {
            Packet::unpack(data)
                .map(|p| p.destination_hash == pr_hash)
                .unwrap_or(false)
        })
        .expect("the requester emits a path request")
}

/// Drive one simulated second: an optional ingress frame, then the tick
/// that runs `retry_pending_discoveries`. Returns the path requests for
/// `dest` this second put on the wire, with targets.
fn one_second(
    board: &mut EmbeddedNode,
    dest: &[u8; TRUNCATED_HASHBYTES],
    ingress: Option<(usize, Vec<u8>)>,
) -> Vec<Vec<usize>> {
    let mut sent = Vec::new();
    if let Some((iface, frame)) = ingress {
        let out = board.handle_packet(InterfaceId(iface), &frame);
        sent.extend(path_requests_for(board, &out, dest));
    }
    let out = board.handle_timeout();
    sent.extend(path_requests_for(board, &out, dest));
    board.transport().clock().advance(1_000);
    sent
}

/// THE pin: the phone retrying every second for half a minute costs the
/// board ONE re-origination, not one per 5-second retry tick. Before
/// the rule the count was 7 (the open plus six fresh-tag retries), and
/// the field capture showed exactly that stream, sustained for hours.
#[test]
fn a_full_mode_window_is_not_retried() {
    let mut dest = make_dest();
    let d = *dest.hash().as_bytes();
    let mut board = make_board();
    arm_cached_announce_no_path(&mut board, &mut dest, LORA);

    let mut onto_lora = 0;
    for _second in 0..30 {
        onto_lora += one_second(&mut board, &d, Some((BLE, fresh_retry(&d))))
            .iter()
            .filter(|reach| reach.contains(&LORA))
            .count();
    }

    assert_eq!(
        onto_lora, 1,
        "one pending window on a Full-mode ingress must cost ONE \
         re-origination — the #117 one-shot — and no retry cadence \
         (the reference retries nothing: 1.3.5 Transport.py:3031/:905-907)"
    );

    // Positive control for the counter: the withheld retry ticks are
    // counted and named, not silently skipped. Six 5-second ticks fall
    // inside the 30-second window (t=0,5,10,15,20,25).
    assert_eq!(
        board.transport().stats().path_request_retry_withholds(),
        6,
        "each withheld retry tick moves the counter"
    );
}

/// The neighbour's side of the field loop: a board that cross-hears a
/// fresh-tag stream for the same destination on the shared medium
/// re-keys it ONCE onto its other interfaces per window — it does not
/// answer a 5-second stream with a 5-second stream of its own. (Nothing
/// ever goes back onto the shared medium itself: ingress is excluded
/// from both the open rebroadcast and the retries.)
#[test]
fn a_cross_heard_stream_is_rekeyed_once_not_echoed_as_a_cadence() {
    let mut dest = make_dest();
    let d = *dest.hash().as_bytes();
    let mut board = make_board();
    arm_cached_announce_no_path(&mut board, &mut dest, BLE);

    let mut to_ble = 0;
    let mut to_lora = 0;
    for second in 0..30 {
        // The neighbour retries on the 5-second grid, fresh tag each.
        let ingress = (second % 5 == 0).then(|| (LORA, fresh_retry(&d)));
        for reach in one_second(&mut board, &d, ingress) {
            if reach.contains(&LORA) {
                to_lora += 1;
            }
            if reach.contains(&BLE) {
                to_ble += 1;
            }
        }
    }

    assert_eq!(
        to_lora, 0,
        "nothing is keyed back onto the shared medium the stream came from"
    );
    assert_eq!(
        to_ble, 1,
        "the cross-heard window is re-keyed once onto the other \
         interfaces, not echoed as a retry cadence"
    );
}

/// The rule is a mode gate, not a retry removal: a window opened from a
/// DISCOVER_PATHS_FOR interface keeps its retry cadence — the operator
/// opted into active discovery there, and the retries are the
/// deliberate lossy-medium deviation `retry_pending_discoveries`
/// documents.
#[test]
fn a_discovering_window_keeps_its_retry_cadence() {
    let dest = make_dest();
    let d = *dest.hash().as_bytes();
    let mut board = make_board();
    board.set_interface_mode(BLE, InterfaceMode::Gateway);

    // Unknown destination, no cache needed: Gateway ingress discovers.
    let mut onto_lora = 0;
    for second in 0..11 {
        let ingress = (second == 0).then(|| (BLE, fresh_retry(&d)));
        onto_lora += one_second(&mut board, &d, ingress)
            .iter()
            .filter(|reach| reach.contains(&LORA))
            .count();
    }

    assert_eq!(
        onto_lora, 4,
        "a discovering window keeps its retry cadence: the open plus the \
         retry ticks at t=0, t=5 and t=10 inside an 11-second drive"
    );
    assert_eq!(
        board.transport().stats().path_request_retry_withholds(),
        0,
        "nothing was withheld on a discovering window"
    );
}
