//! What one link and one incoming resource cost a board's heap, measured
//! against a real transfer rather than read off the receive path.
//!
//! # Why this file exists
//!
//! `leviculum-esp`'s `heltec_v4` binary accepts links
//! (`MAX_LINKS`) and caps what a peer may advertise to it
//! (`MAX_INCOMING_RESOURCE_BYTES`), and it asserts at COMPILE time that both
//! fit its share of a 160 KiB pool. The sum it asserts is
//! [`leviculum_core::resource::incoming_peak_bytes`] — an enumeration of the
//! buffers `IncomingResource::assemble` has live at the same instant. An
//! enumeration is a claim about code, and a claim about code is exactly the
//! kind of thing that is true when it is written and false four commits
//! later, silently, in a crate that cross-compiles and runs no test.
//!
//! So this file does not restate the sum. It drives a real resource transfer
//! between two real nodes, counts the bytes the RECEIVER has live at its
//! worst instant, and asserts the formula covers it. A change to the assembly
//! path that adds a buffer makes this red; a change that removes one makes
//! the margin visible in the printed line and nothing else.
//!
//! The second test is the reason the cap has to be set at all, as a
//! measurement rather than an argument: with the builder default the same
//! receiver allocates tens of kilobytes off ONE advertisement packet, before
//! any part has arrived and with nothing it can do about it — the peer chose
//! the number. With the board's cap set it allocates nothing and refuses.
//!
//! Both tests run host-side because neither firmware crate can run a test at
//! all; both cross-compile. The figures they produce are the host's — a
//! 64-bit `Option<Vec<u8>>` is 24 bytes where the board's is 12 — which is
//! why the assertion is `formula >= measured` on whatever target it runs,
//! and never a byte count written out.
//!
//! Run: `cargo test -p leviculum-core --test board_link_budget -- --nocapture`

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use rand_core::OsRng;

use leviculum_core::constants::{MTU, RESOURCE_HASHMAP_LEN, RESOURCE_WINDOW_MAX_FAST};
use leviculum_core::resource::{
    incoming_peak_bytes, resource_sdu, ResourceStrategy, ASSEMBLY_LIVE_COPIES,
};
use leviculum_core::traits::{Clock, NoStorage};
use leviculum_core::{
    Action, Destination, DestinationHash, DestinationType, Direction, Identity, InterfaceId,
    LinkId, NodeCore, NodeCoreBuilder, ProofStrategy, TickOutput,
};

// ---------------------------------------------------------------------------
// The instrument: bytes live, attributed to the receiver
// ---------------------------------------------------------------------------
//
// A global allocator that forwards to System and, while ARMED, remembers the
// size of every block it hands out. A free is accounted whether or not the
// allocator is armed, because a buffer the receiver allocated inside a window
// is routinely dropped by the harness outside one — the packets it hands
// back. Counting the alloc and dropping the free on the floor would make the
// instrument drift upwards for the length of the run, which is the failure
// mode that makes a heap instrument worse than none.
//
// Blocks allocated while disarmed are never entered in the map, so the
// sender's and the harness's own allocations are invisible: `peak` is the
// high-water mark of what the RECEIVER has live, and nothing else.

struct AttributingAlloc;

static ARMED: AtomicBool = AtomicBool::new(false);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static BLOCKS: Mutex<Option<HashMap<usize, usize>>> = Mutex::new(None);

thread_local! {
    // Set while we are inside the bookkeeping, so the map's own allocations
    // cannot recurse into it. const-init: no allocation on first touch.
    static IN_TRACKER: Cell<bool> = const { Cell::new(false) };
}

fn with_map<T>(f: impl FnOnce(&mut HashMap<usize, usize>) -> T) -> Option<T> {
    IN_TRACKER.with(|flag| {
        if flag.get() {
            return None;
        }
        flag.set(true);
        let mut guard = BLOCKS.lock().ok()?;
        let out = f(guard.get_or_insert_with(HashMap::new));
        drop(guard);
        flag.set(false);
        Some(out)
    })
}

// SAFETY: every op forwards to System with the layout it was handed; the
// bookkeeping only reads the returned address as an integer and never touches
// the memory. `realloc` is deliberately not overridden, so the default impl
// decomposes it into a recorded alloc plus a recorded dealloc.
unsafe impl GlobalAlloc for AttributingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() && ARMED.load(Ordering::Relaxed) {
            let size = layout.size();
            with_map(|map| {
                if map.insert(ptr as usize, size).is_none() {
                    let live = LIVE.fetch_add(size, Ordering::Relaxed) + size;
                    PEAK.fetch_max(live, Ordering::Relaxed);
                }
            });
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        with_map(|map| {
            if let Some(size) = map.remove(&(ptr as usize)) {
                LIVE.fetch_sub(size, Ordering::Relaxed);
            }
        });
        System.dealloc(ptr, layout);
    }
}

#[global_allocator]
static ALLOC: AttributingAlloc = AttributingAlloc;

/// The instrument is one global, and `cargo test` runs the tests in this
/// binary on parallel threads. A second test resetting `PEAK` while the first
/// is measuring is not a flake to be lived with, it is a zero reported as a
/// measurement — which is exactly what happened while this file was being
/// written, and it is the reason `heap_fragmentation.rs` next door is a single
/// test. Two tests here, each holding this for its whole body, so the other
/// one has not allocated yet when this one arms.
static INSTRUMENT: Mutex<()> = Mutex::new(());

/// Take the instrument. Poisoning is ignored deliberately: a panicking test
/// has already failed and the next one wants a working instrument, not a
/// second failure about the first one.
fn take_instrument() -> std::sync::MutexGuard<'static, ()> {
    INSTRUMENT.lock().unwrap_or_else(|e| e.into_inner())
}

/// Forget everything the instrument holds and start from zero.
fn tracker_reset() {
    ARMED.store(false, Ordering::Relaxed);
    LIVE.store(0, Ordering::Relaxed);
    PEAK.store(0, Ordering::Relaxed);
    with_map(|map| map.clear());
}

/// Run `f` with the receiver's allocations attributed to it.
fn armed<T>(f: impl FnOnce() -> T) -> T {
    ARMED.store(true, Ordering::Relaxed);
    let out = f();
    ARMED.store(false, Ordering::Relaxed);
    out
}

fn peak() -> usize {
    PEAK.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// The fixture: two nodes and a link, the way a board's peer would build one
// ---------------------------------------------------------------------------

const IFACE: InterfaceId = InterfaceId(0);
const START_MS: u64 = 1_000_000;

/// A clock that does not move. Both tests run on a lossless in-process wire,
/// where nothing the transfer does is driven by a timer: every REQ is answered
/// in the same call chain that produced it, so advancing time would only
/// expire things. A clock that stands still makes a retry impossible rather
/// than unlikely, which is what keeps the measured peak the peak of ONE
/// transfer.
#[derive(Clone, Copy)]
struct StillClock;

impl Clock for StillClock {
    fn now_ms(&self) -> u64 {
        START_MS
    }
}

type Node = NodeCore<OsRng, StillClock, NoStorage>;

fn outbound(out: &TickOutput) -> Vec<Vec<u8>> {
    out.actions
        .iter()
        .map(|a| match a {
            Action::SendPacket { data, .. } | Action::Broadcast { data, .. } => data.clone(),
        })
        .collect()
}

/// The receiving side: one destination that accepts links, and the cap under
/// test. `max_incoming` is `None` for the builder default — the state
/// `heltec_v4` shipped in before the link step, and the control the second
/// test needs.
fn make_receiver(max_incoming: Option<usize>) -> (Node, DestinationHash, [u8; 32]) {
    let identity = Identity::generate(&mut OsRng);
    let signing_key = identity.ed25519_verifying().to_bytes();
    let mut builder = NodeCoreBuilder::new().max_links(Some(1));
    if let Some(cap) = max_incoming {
        builder = builder.max_incoming_resource_size(cap);
    }
    let mut node = builder.build(OsRng, StillClock, NoStorage);

    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "leviculum",
        &["esp32", "board"],
    )
    .expect("destination");
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    let dest_hash = *dest.hash();
    node.register_destination(dest);
    (node, dest_hash, signing_key)
}

fn make_sender() -> Node {
    NodeCoreBuilder::new().build(OsRng, StillClock, NoStorage)
}

/// Drive a link to Active on both sides, with every receiver call attributed.
/// Returns the link id both ends know it by.
fn establish(
    sender: &mut Node,
    receiver: &mut Node,
    dest: DestinationHash,
    key: &[u8; 32],
) -> LinkId {
    let (link_id, _routed, out) = sender.connect(dest, key).expect("connect");
    let mut to_receiver = outbound(&out);
    for _ in 0..8 {
        if to_receiver.is_empty() {
            break;
        }
        let mut back = Vec::new();
        for pkt in std::mem::take(&mut to_receiver) {
            let out = armed(|| receiver.handle_packet(IFACE, &pkt));
            back.extend(outbound(&out));
        }
        for pkt in back {
            to_receiver.extend(outbound(&sender.handle_packet(IFACE, &pkt)));
        }
    }
    assert_eq!(sender.active_link_count(), 1, "sender link active");
    assert_eq!(receiver.active_link_count(), 1, "receiver link active");
    link_id
}

// ---------------------------------------------------------------------------
// 1. The formula covers every term the peer's number moves
// ---------------------------------------------------------------------------

/// Drive one whole resource of `cap` encrypted bytes through a fresh pair and
/// return the receiver's peak live bytes.
///
/// The instrument is reset AFTER the link is up, so the node boxes and the
/// `Link` are already allocated and invisible: what it returns is the cost of
/// the transfer plus whatever `handle_packet` holds while it runs.
fn peak_for_transfer(cap: usize) -> usize {
    // What the sender's plaintext must be for the ciphertext to land exactly
    // at `cap`: the receive path bounds the ENCRYPTED size (`t`), which
    // carries a 4-byte random hash, the token overhead and one AES block of
    // padding.
    let plaintext = cap - 4 - 48 - 16;

    let (mut receiver, dest, key) = make_receiver(Some(cap));
    let mut sender = make_sender();
    let link_id = establish(&mut sender, &mut receiver, dest, &key);
    receiver
        .set_resource_strategy(&link_id, ResourceStrategy::AcceptAll)
        .expect("accept resources");

    tracker_reset();

    let data: Vec<u8> = (0..plaintext).map(|i| (i % 251) as u8).collect();
    let (_hash, out) = sender
        .send_resource(&link_id, &data, None, false)
        .expect("send resource");

    let mut to_receiver = outbound(&out);
    let mut completed = false;
    for _ in 0..512 {
        if to_receiver.is_empty() {
            break;
        }
        let mut back = Vec::new();
        for pkt in std::mem::take(&mut to_receiver) {
            let out = armed(|| receiver.handle_packet(IFACE, &pkt));
            completed |= out
                .events
                .iter()
                .any(|e| matches!(e, leviculum_core::NodeEvent::ResourceCompleted { .. }));
            back.extend(outbound(&out));
        }
        for pkt in back {
            to_receiver.extend(outbound(&sender.handle_packet(IFACE, &pkt)));
        }
    }

    // A transfer that never assembled never reached the peak this measures, so
    // a green run on a broken fixture would be a green run that measured
    // nothing. This is the control that makes the number mean something.
    assert!(
        completed,
        "the fixture never completed the {cap} B transfer, so nothing was measured"
    );
    peak()
}

/// What a RECEPTION holds while it runs, over and above the buffers the
/// resource itself owns.
///
/// [`incoming_peak_bytes`] enumerates the resource's own buffers and nothing
/// else, deliberately: the firmware budgets a working reserve for the rest
/// (`leviculum-esp`'s `NODE_WORKING_RESERVE` names "the per-packet copies a
/// reception makes on its way through `handle_packet`" among the things it
/// covers). Measuring a receiver at its peak sees both, so the assertion
/// below needs a term for the second — and the term has to be bounded by
/// something OTHER than the cap, or it would be hiding exactly the growth
/// this file exists to detect.
///
/// It is, and by protocol constants:
///
///  * four MTUs for the packet a part arrived in, the plaintext
///    `Link::decrypt` produces from it, and their copies through
///    `handle_packet` — every one of them bounded by the link MTU, none by
///    the transfer's size;
///  * four REQ-sized buffers for the request the receiver builds in the same
///    call (`build_request`: a flag byte, a map hash, a 32-byte id and at
///    most [`RESOURCE_WINDOW_MAX_FAST`] hashes) — bounded by the WINDOW,
///    which is a protocol maximum and the same at any cap.
///
/// Four of each rather than one: the measurement counts a buffer and its copy
/// and the packet built to answer it, and the point of the allowance is to be
/// generous about the terms that do not grow so the assertion is sharp about
/// the ones that do.
const RECEPTION_ALLOWANCE: usize =
    4 * MTU + 4 * (1 + RESOURCE_HASHMAP_LEN + 32 + RESOURCE_HASHMAP_LEN * RESOURCE_WINDOW_MAX_FAST);

/// Two real transfers at two caps, and the property that binds them.
///
/// # What is asserted
///
/// `incoming_peak_bytes(cap) + RECEPTION_ALLOWANCE >= measured peak`, at two
/// caps four times apart. The allowance is cap-independent by construction
/// (see its own doc), so the assertion is sharp in exactly one direction:
/// anything that grows with the cap has to be inside the formula. A seventh
/// live copy of the payload, or an index entry that got wider, breaks it at
/// the large cap while the small one still passes — which is the shape of
/// failure that would otherwise ship as a heap budget that is quietly short
/// on every board computed from it.
///
/// It is not `formula >= measured`. That would assert one crate's constant
/// covers another crate's term and would go red for reasons that have
/// nothing to do with the cap.
///
/// The small cap is the one `leviculum-esp` ships and the large one is
/// `leviculum-nrf`'s, so the printed line answers "what does that board's cap
/// actually cost" for both families at once.
#[test]
fn incoming_peak_formula_bounds_what_the_cap_buys() {
    let _instrument = take_instrument();
    // Two real values a real sender will send, four times apart, so a
    // per-byte term the formula misses cannot hide inside the allowance.
    const SMALL: usize = 2 * 1024;
    const LARGE: usize = 8 * 1024;

    let sdu = resource_sdu(MTU as u32);
    let measured = [peak_for_transfer(SMALL), peak_for_transfer(LARGE)];
    let formula = [
        incoming_peak_bytes(SMALL, sdu),
        incoming_peak_bytes(LARGE, sdu),
    ];

    for (i, cap) in [SMALL, LARGE].into_iter().enumerate() {
        println!(
            "INCOMING_PEAK cap={cap} sdu={sdu} measured={} formula={} allowance={} \
             residual={}",
            measured[i],
            formula[i],
            RECEPTION_ALLOWANCE,
            measured[i].saturating_sub(formula[i]),
        );
    }

    for (i, cap) in [SMALL, LARGE].into_iter().enumerate() {
        assert!(
            formula[i] + RECEPTION_ALLOWANCE >= measured[i],
            "incoming_peak_bytes({cap}, {sdu}) = {} plus the {RECEPTION_ALLOWANCE} B a \
             reception is allowed does not cover the {} B a real transfer at {cap} B has \
             live: the receive path allocates something the enumeration behind \
             ASSEMBLY_LIVE_COPIES does not name, and every firmware heap budget computed \
             from it is short",
            formula[i],
            measured[i],
        );
    }

    // The slope, which is the same claim with the allowance taken out of it.
    //
    // Both assertions above carry the allowance, so a generous allowance
    // buys slack at both caps at once — and the allowance is a judgement
    // about buffers that do NOT grow with the cap, made in this file. The
    // difference between the two measurements cancels everything
    // cap-independent, allowance included, and leaves exactly the bytes the
    // extra 6 KiB of cap bought. That is the number ASSEMBLY_LIVE_COPIES
    // claims, so it is asserted against the formula's own difference and not
    // against a figure written out here.
    //
    // At six live copies this reads 37 469 B for 6 144 B of extra cap; at
    // two it reads 12 893 B, against a formula difference of 12 665 B.
    let measured_growth = measured[1] - measured[0];
    let formula_growth = formula[1] - formula[0];
    assert!(
        formula_growth + RECEPTION_ALLOWANCE >= measured_growth,
        "{} B more cap cost the receiver {measured_growth} B more heap, where \
         incoming_peak_bytes says {formula_growth} B: the assembly path holds more \
         copies of the transfer than the {} ASSEMBLY_LIVE_COPIES names, and the \
         difference is charged to every board's heap budget",
        LARGE - SMALL,
        ASSEMBLY_LIVE_COPIES,
    );
}

// ---------------------------------------------------------------------------
// 2. The unset default is an allocation the peer chooses
// ---------------------------------------------------------------------------

/// One advertisement, two receivers, and the difference the cap makes.
///
/// Nothing is transferred: the advertisement alone sizes `parts` and
/// `hashmap` from the peer's `t`. The receiver with the builder default
/// allocates the index for a megabyte off that one packet; the receiver with
/// a board's cap refuses it before allocating. The first figure is the
/// positive control — a run where the default receiver allocated nothing
/// would mean the fixture never delivered an advertisement, and would prove
/// nothing about the cap.
#[test]
fn an_unset_cap_lets_a_peer_size_the_allocation() {
    let _instrument = take_instrument();
    // Big enough that the index is unmistakable and still one segment: the
    // reference spills above `RESOURCE_MAX_EFFICIENT_SIZE`, and a peer that
    // segments sends several advertisements, each bounded the same way.
    const PLAINTEXT: usize = 900_000;
    // A board-sized cap. Any value far below the advertisement works; this
    // one is the order of magnitude an ESP32-S3's unclaimed node share
    // actually affords.
    const BOARD_CAP: usize = 2 * 1024;

    let data: Vec<u8> = (0..PLAINTEXT).map(|i| (i % 251) as u8).collect();

    let mut peaks = Vec::new();
    for cap in [None, Some(BOARD_CAP)] {
        let (mut receiver, dest, key) = make_receiver(cap);
        let mut sender = make_sender();
        let link_id = establish(&mut sender, &mut receiver, dest, &key);
        receiver
            .set_resource_strategy(&link_id, ResourceStrategy::AcceptAll)
            .expect("accept resources");

        tracker_reset();
        let (_hash, out) = sender
            .send_resource(&link_id, &data, None, false)
            .expect("send resource");
        // The advertisement only. Everything after it is the sender waiting
        // for a REQ that a refusing receiver never sends.
        let adv = outbound(&out).into_iter().next().expect("advertisement");
        let out = armed(|| receiver.handle_packet(IFACE, &adv));
        drop(out);
        peaks.push(peak());
    }

    let (unset, capped) = (peaks[0], peaks[1]);
    let sdu = resource_sdu(MTU as u32);
    println!(
        "ADVERTISEMENT_ONLY plaintext={PLAINTEXT} sdu={sdu} unset_cap_peak={unset} \
         board_cap_peak={capped} board_cap={BOARD_CAP}"
    );

    // The control: the default really does let the peer pick a large number.
    // The bound is the index term alone — no part has arrived — so it is not
    // a byte count written out but the same arithmetic the firmware budgets
    // with, at the advertised size.
    let index_only = incoming_peak_bytes(PLAINTEXT, sdu) - ASSEMBLY_LIVE_COPIES * PLAINTEXT;
    assert!(
        unset >= index_only / 2,
        "the default-capped receiver allocated only {unset} B off an advertisement whose \
         index alone is {index_only} B: the fixture did not deliver an advertisement, so \
         the comparison below proves nothing"
    );
    // And the finding: the cap moves that from the peer's choice to ours.
    assert!(
        capped < unset / 10,
        "a receiver capped at {BOARD_CAP} B still allocated {capped} B off an \
         advertisement the uncapped one spent {unset} B on: the cap is not being \
         enforced before the allocation"
    );
}
