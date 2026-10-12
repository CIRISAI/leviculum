//! The boot heap budget and the live serve cap, asserted on the host
//! (Codeberg #388, #413).
//!
//! Until this crate existed the whole check on this arithmetic was one
//! compile-time assert on `SERVE_MARGIN_BYTES`, and every other number in it
//! could only be read off a board's boot line. Here each function is pinned
//! at its boundaries, the derived constants are printed with their values so
//! a change to the heap size or any term shows up as a diff, and the #388
//! field figures are asked the question the board asks at every `/get`.
//!
//! The boot line is pinned below at the measured node box,
//! [`NODE_BOX_BYTES`], which the firmware asserts against `size_of` of its
//! `NodeCore`. Until #661 this file stated a node box of its own, 31 040 B,
//! and pinned `slack=656 serve_cap=170` from it while every board built from
//! the tree printed `total=98304 slack=0 serve_cap=0`: the node box had grown
//! 656 B past the figure and nothing compared the two.
//!
//! Run: `cargo test -p leviculum-heap-budget --test budget -- --nocapture`

use leviculum_heap_budget::{
    budget_per_link, budget_reserve, budget_serve_cap, budget_slack, budget_total,
    held_announce_bytes, live_serve_cap, max_endpoint_links, BOARD_HOLDING_INTERFACES,
    BOARD_INGRESS_CONTROL, BOARD_INTERFACES, BOARD_MAX_HELD_ANNOUNCES, BOARD_SYNC_LIMIT_KB,
    BOARD_TRANSFER_LIMIT_KB, CHANNEL_BUDGET_BYTES, HEAP_SIZE, HELD_ANNOUNCE_SLICE_BYTES,
    INTERFACE_MTU, LINK_BYTES, MAX_INCOMING_RESOURCE_BYTES, MAX_LINKS, NODE_BOX_BYTES,
    OFFER_RESPONSE_RESERVE_BYTES, QUEUE_DEPTH, ROLE_BUDGET_BYTES, SERVE_MARGIN_BYTES,
    SERVE_PEAK_BYTES, SERVE_RESERVE_BYTES, SERVE_RESOURCE_SDU, SESSION_BUDGET_BYTES,
    SYNC_SLICE_BYTES,
};
use leviculum_lxmf::propagation_node::{serve_largest_block_bytes, serve_peak_bytes};

/// The T114's boxed `NodeCore`, as its boot line reports it
/// (`size_of_val(&*node)`, `src/bin/t114.rs`). The crate's pinned
/// [`NODE_BOX_BYTES`], which `heap_census.rs` asserts on the target; the
/// RAK4631 and the solar node box the same type and measure the same.
const T114_NODE_BOX: usize = NODE_BOX_BYTES;

/// Every fixed term of [`max_endpoint_links`] except the node box.
fn fixed_without_node_box() -> usize {
    ROLE_BUDGET_BYTES + budget_reserve() + MAX_LINKS * SESSION_BUDGET_BYTES
}

/// The node box at which the fixed terms fill the heap exactly.
fn node_box_filling_the_heap() -> usize {
    HEAP_SIZE - fixed_without_node_box()
}

#[test]
fn the_derived_constants_have_these_values() {
    let rows = [
        ("HEAP_SIZE", HEAP_SIZE, 98_304),
        (
            "MAX_INCOMING_RESOURCE_BYTES",
            MAX_INCOMING_RESOURCE_BYTES,
            8_192,
        ),
        ("MAX_LINKS", MAX_LINKS, 4),
        ("QUEUE_DEPTH", QUEUE_DEPTH, 4),
        ("INTERFACE_MTU", INTERFACE_MTU, 564),
        ("SESSION_BUDGET_BYTES", SESSION_BUDGET_BYTES, 3_948),
        ("CHANNEL_BUDGET_BYTES", CHANNEL_BUDGET_BYTES, 4_512),
        // Derived since order 511 (was a stated 8): the largest answer the
        // role's sync slice funds beside two queued uploads.
        ("BOARD_SYNC_LIMIT_KB", BOARD_SYNC_LIMIT_KB as usize, 5),
        ("SYNC_SLICE_BYTES", SYNC_SLICE_BYTES, 8_000),
        (
            "OFFER_RESPONSE_RESERVE_BYTES",
            OFFER_RESPONSE_RESERVE_BYTES,
            1_024,
        ),
        (
            "BOARD_TRANSFER_LIMIT_KB",
            BOARD_TRANSFER_LIMIT_KB as usize,
            4,
        ),
        ("SERVE_RESOURCE_SDU", SERVE_RESOURCE_SDU, 464),
        ("SERVE_RESERVE_BYTES", SERVE_RESERVE_BYTES, 0),
        ("SERVE_PEAK_BYTES", SERVE_PEAK_BYTES, 17_264),
        ("ROLE_BUDGET_BYTES", ROLE_BUDGET_BYTES, 21_120),
        ("LINK_BYTES", LINK_BYTES, 2_632),
        ("budget_per_link", budget_per_link(), 2_696),
        ("budget_reserve", budget_reserve(), 18_848),
        ("SERVE_MARGIN_BYTES", SERVE_MARGIN_BYTES, 14_696),
    ];
    for (name, value, _) in rows {
        println!("HEAP_BUDGET_CONST {name}={value}");
    }
    for (name, value, expected) in rows {
        assert_eq!(value, expected, "{name}");
    }
}

/// The T114 boot line at its node box.
#[test]
fn the_t114_boot_line_has_these_values() {
    let nb = T114_NODE_BOX;
    println!(
        "HEAP_BUDGET node_box={nb} links={} total={} slack={} serve_cap={}",
        max_endpoint_links(nb),
        budget_total(nb),
        budget_slack(nb),
        budget_serve_cap(nb)
    );
    assert_eq!(max_endpoint_links(nb), 4);
    // Four links fit with 56 B left: the heap to the byte at #661,
    // 4 × 16 B since the RTT retry state left `Link` (#662), less the 8 B
    // the boxed split request/response state takes in `NodeCore` (#666).
    assert_eq!(nb, 31_704);
    assert_eq!(budget_total(nb), HEAP_SIZE - 56);
    assert_eq!(budget_slack(nb), 56);
    assert_eq!(budget_serve_cap(nb), 0);
    // The binaries' two boot assertions hold on it.
    assert!(budget_total(nb) <= HEAP_SIZE);
    assert!(max_endpoint_links(nb) >= MAX_LINKS);
}

#[test]
fn max_endpoint_links_at_its_boundaries() {
    let full = node_box_filling_the_heap();
    let per_link = budget_per_link();
    // Zero node box: the whole remainder goes to links.
    assert_eq!(
        max_endpoint_links(0),
        (HEAP_SIZE - fixed_without_node_box()) / per_link
    );
    assert_eq!(max_endpoint_links(0), 15);
    // The node box at which exactly MAX_LINKS links fit with nothing left:
    // the cap is reached, slack is zero, the total is the heap to the byte.
    let exact = full - MAX_LINKS * per_link;
    assert_eq!(exact, 31_760);
    // The boards are 56 B below it: not one byte to spare at #661, then
    // 16 B a link freed when the RTT retry state left `Link` (#662), then
    // 8 B of that spent on the boxed split state in `NodeCore` (#666).
    assert_eq!(exact - NODE_BOX_BYTES, 56);
    assert_eq!(max_endpoint_links(exact), MAX_LINKS);
    assert_eq!(budget_slack(exact), 0);
    assert_eq!(budget_total(exact), HEAP_SIZE);
    // One byte more and a link is lost, with one link's cost less a byte
    // of slack: the binaries' `links >= MAX_LINKS` assertion trips here.
    assert_eq!(max_endpoint_links(exact + 1), MAX_LINKS - 1);
    assert_eq!(budget_slack(exact + 1), per_link - 1);
    // One byte less and nothing changes but a byte of slack.
    assert_eq!(max_endpoint_links(exact - 1), MAX_LINKS);
    assert_eq!(budget_slack(exact - 1), 1);
    // Fixed terms filling the heap exactly: no link, no slack.
    assert_eq!(full, 42_544);
    assert_eq!(max_endpoint_links(full - 1), 0);
    assert_eq!(budget_slack(full - 1), 1);
    assert_eq!(max_endpoint_links(full), 0);
    assert_eq!(budget_total(full), HEAP_SIZE);
    // Past it the division does not underflow, and the total exceeds the
    // heap, which is what the boot `total <= HEAP_SIZE` assertion catches.
    assert_eq!(max_endpoint_links(full + 1), 0);
    assert_eq!(budget_total(full + 1), HEAP_SIZE + 1);
    assert_eq!(budget_slack(full + 1), 0);
    assert_eq!(max_endpoint_links(HEAP_SIZE), 0);
}

#[test]
fn budget_serve_cap_at_its_boundaries() {
    let exact = node_box_filling_the_heap() - MAX_LINKS * budget_per_link();
    // No slack funds no serve.
    assert_eq!(budget_serve_cap(exact), 0);
    assert_eq!(budget_serve_cap(node_box_filling_the_heap()), 0);
    // One byte of slack funds nothing either.
    assert_eq!(budget_serve_cap(exact - 1), 0);
    // The serve cap is the largest cap whose peak fits the slack: the
    // T114's 56 B fund nothing, not even a one-byte serve (#661, #662,
    // #666).
    let slack = budget_slack(T114_NODE_BOX);
    assert_eq!(slack, 56);
    assert_eq!(budget_serve_cap(T114_NODE_BOX), 0);
    assert!(serve_peak_bytes(1, SERVE_RESOURCE_SDU) > slack);
    // A link fewer frees almost a whole link's cost for the serve.
    assert_eq!(budget_serve_cap(exact + 1), 1_016);
    // Zero node box: 2 104 B of slack.
    assert_eq!(budget_slack(0), 2_104);
    assert_eq!(budget_serve_cap(0), 744);
}

#[test]
fn live_serve_cap_takes_the_margin_off_free_to_the_byte() {
    let m = SERVE_MARGIN_BYTES;
    // Boot cap zero so the margin's edge is visible on its own: the first
    // byte served needs the margin plus the peak of a one-byte serve.
    let first = m + serve_peak_bytes(1, SERVE_RESOURCE_SDU);
    println!("LIVE_SERVE first_free_for_one_byte={first}");
    assert_eq!(live_serve_cap(0, 0, usize::MAX), 0);
    assert_eq!(live_serve_cap(0, m - 1, usize::MAX), 0);
    assert_eq!(live_serve_cap(0, m, usize::MAX), 0);
    assert_eq!(live_serve_cap(0, m + 1, usize::MAX), 0);
    assert_eq!(live_serve_cap(0, first - 1, usize::MAX), 0);
    assert_eq!(live_serve_cap(0, first, usize::MAX), 1);
}

#[test]
fn live_serve_cap_takes_the_margin_off_largest_to_the_byte() {
    let m = SERVE_MARGIN_BYTES;
    let first = m + serve_largest_block_bytes(1, SERVE_RESOURCE_SDU);
    println!("LIVE_SERVE first_largest_for_one_byte={first}");
    assert_eq!(live_serve_cap(0, usize::MAX / 2, m - 1), 0);
    assert_eq!(live_serve_cap(0, usize::MAX / 2, m), 0);
    assert_eq!(live_serve_cap(0, usize::MAX / 2, m + 1), 0);
    assert_eq!(live_serve_cap(0, usize::MAX / 2, first - 1), 0);
    // Not 1: for small caps the largest single block is a part or the
    // scratch, not the body, so it is flat in the cap and the block that
    // funds one byte funds every cap up to where the body or the part grows.
    let at_first = live_serve_cap(0, usize::MAX / 2, first);
    assert_eq!(at_first, 8);
    assert_eq!(
        serve_largest_block_bytes(at_first, SERVE_RESOURCE_SDU),
        serve_largest_block_bytes(1, SERVE_RESOURCE_SDU)
    );
    assert!(
        serve_largest_block_bytes(at_first + 1, SERVE_RESOURCE_SDU)
            > serve_largest_block_bytes(1, SERVE_RESOURCE_SDU)
    );
}

#[test]
fn live_serve_cap_never_falls_below_the_boot_cap() {
    let boot = budget_serve_cap(T114_NODE_BOX);
    let m = SERVE_MARGIN_BYTES;
    // At and either side of the margin the live heap funds less than the
    // boot plan did, and the boot cap is what the board serves.
    for free in [0, m - 1, m, m + 1] {
        assert_eq!(live_serve_cap(boot, free, free), boot, "free={free}");
    }
    // The first free figure at which the live heap beats the boot floor.
    let above = m + serve_peak_bytes(boot + 1, SERVE_RESOURCE_SDU);
    println!("LIVE_SERVE boot_cap={boot} first_free_above_boot={above}");
    // A zero floor (#661): the first heap above it is the first heap that
    // funds one byte at all (15 369 B when the stale pin said 170 B).
    assert_eq!(boot, 0);
    assert_eq!(above, 14_837);
    assert_eq!(live_serve_cap(boot, above - 1, usize::MAX), boot);
    assert_eq!(live_serve_cap(boot, above, usize::MAX), boot + 1);
    // Bytes in pieces: plenty free, but the largest block at the margin
    // funds nothing above the floor.
    assert_eq!(live_serve_cap(boot, 60_000, m), boot);
}

/// The #388 question, asked of the figures the boards reported. With the
/// 96 KiB heap, two phones on BLE and the propagation role, what does one
/// `/get` get to serve?
///
/// The 2026-09-12 field rows come from the issue text (`[HEAP]` lines of
/// the field T114 183004F712B4A7FE): the census and its `largest=` did not
/// exist yet, so `largest` is taken as `free`, the most generous reading.
/// The 2026-09-23 row is the census line the issue's comment quotes from
/// `lora_pn_board_offer_past_the_link`.
#[test]
fn issue_388_field_figures() {
    let boot = budget_serve_cap(T114_NODE_BOX);
    // (what, free, largest, expected cap)
    let rows: [(&str, usize, usize, usize); 3] = [
        // Six minutes before the panic, links=2, no traffic.
        ("field 2026-09-12 steady used=90804", 7_500, 7_500, 0),
        // After the upload, 24 s before `allocation of 340 bytes failed`.
        ("field 2026-09-12 pre-panic used=93544", 4_760, 4_760, 0),
        // The rig T114 that died serving 24 messages, under today's model.
        ("rig 2026-09-23 used=67924", 30_380, 30_320, 7_252),
    ];
    for (what, free, largest, expected) in rows {
        let cap = live_serve_cap(boot, free, largest);
        println!(
            "ISSUE_388 {what} free={free} largest={largest} margin={SERVE_MARGIN_BYTES} \
             boot_cap={boot} live_serve_cap={cap}"
        );
        assert_eq!(cap, expected, "{what}");
        // Both field rows are below the margin: the heap they report funds
        // no serve of its own, and the board falls back to the boot floor,
        // which since #661 is honestly 0 B: it serves nothing.
        if free < SERVE_MARGIN_BYTES {
            assert_eq!(cap, boot, "{what}");
        }
    }
}

/// Codeberg #598: what the held-announce hold costs on the board, at the
/// figures `HELD_ANNOUNCE_SLICE_BYTES` states. The boot plan's slack
/// funds no held announce at all, the board's cap fills its slice, one
/// more does not fit, and the core's default would not fit the heap.
#[test]
fn the_held_announce_hold_against_its_slice() {
    let first = held_announce_bytes(1, 1);
    let at_cap = held_announce_bytes(BOARD_MAX_HELD_ANNOUNCES, BOARD_HOLDING_INTERFACES);
    let past_cap = held_announce_bytes(BOARD_MAX_HELD_ANNOUNCES + 1, BOARD_HOLDING_INTERFACES);
    let core_default = held_announce_bytes(256, 1);
    println!(
        "HELD slack={} first={first} cap={BOARD_MAX_HELD_ANNOUNCES} at_cap={at_cap} \
         past_cap={past_cap} slice={HELD_ANNOUNCE_SLICE_BYTES} core_default_one_iface={core_default}",
        budget_slack(T114_NODE_BOX)
    );
    assert_eq!(
        first, 1_220,
        "a 536 B node, the 184 B outer node, 500 B raw"
    );
    assert!(
        budget_slack(T114_NODE_BOX) < first,
        "the boot plan's slack funds no held announce"
    );
    // Codeberg #606: the USB serial interface holds none, so the slice is
    // shared by LoRa and BLE alone, and the cap the three shared (4) is 6.
    assert!(
        !BOARD_INGRESS_CONTROL[0],
        "USB serial runs no ingress limiter"
    );
    assert_eq!(BOARD_HOLDING_INTERFACES, 2);
    assert_eq!(BOARD_MAX_HELD_ANNOUNCES, 6);
    assert_eq!(at_cap, 7_256);
    assert_eq!(held_announce_bytes(4, BOARD_INTERFACES), 7_792);
    assert!(at_cap <= HELD_ANNOUNCE_SLICE_BYTES);
    assert!(
        past_cap > HELD_ANNOUNCE_SLICE_BYTES,
        "the cap is the largest that fits"
    );
    assert_eq!(core_default, 148_016);
    assert!(core_default > HEAP_SIZE);
    assert_eq!(held_announce_bytes(0, BOARD_HOLDING_INTERFACES), 0);
}

/// Audit 2026-10-12 D14: four links (two phones uploading, a peer syncing,
/// a third client) each advertise a resource at the announced sync limit,
/// one after the other while the earlier ones are still in flight. The
/// reserve funds [`INCOMING_RESOURCES_FUNDED`] of them at the binaries' cap;
/// the gate admits exactly that many and refuses the rest, whoever sends
/// them. When the admitted one concludes, the next advertisement is taken.
#[test]
fn four_links_advertising_at_once_get_what_the_reserve_funds() {
    use leviculum_heap_budget::{admits_incoming_resource, INCOMING_RESOURCES_FUNDED};
    let size = BOARD_SYNC_LIMIT_KB * 1000;
    assert!(size as usize <= MAX_INCOMING_RESOURCE_BYTES);
    let within = size <= BOARD_SYNC_LIMIT_KB * 1000;

    let mut in_flight = 0;
    let mut admitted = Vec::new();
    for link in ["phone-a", "phone-b", "peer", "phone-c"] {
        if admits_incoming_resource(within, in_flight) {
            in_flight += 1;
            admitted.push(link);
        }
    }
    assert_eq!(admitted, vec!["phone-a"], "one admitted, three refused");
    assert_eq!(in_flight, INCOMING_RESOURCES_FUNDED);
    // Closed by admission, not by funding: the reserve term did not move.
    assert_eq!(budget_reserve(), 18_848);

    // The admitted transfer concluded: the next advertisement is taken.
    in_flight -= 1;
    assert!(admits_incoming_resource(within, in_flight));
    // Over the sync limit is refused whatever is in flight.
    assert!(!admits_incoming_resource(false, 0));
}
