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
//! The firmware's numbers before and after the move are the same: the
//! T114's boot line (`HEAP_BUDGET links=4 … slack=720 serve_cap=194`, the
//! values `heap_census.rs` documented before the move) is pinned below at
//! the T114's node box.
//!
//! Run: `cargo test -p leviculum-heap-budget --test budget -- --nocapture`

use leviculum_heap_budget::{
    budget_per_link, budget_reserve, budget_serve_cap, budget_slack, budget_total, live_serve_cap,
    max_endpoint_links, BOARD_SYNC_LIMIT_KB, BOARD_TRANSFER_LIMIT_KB, CHANNEL_BUDGET_BYTES,
    HEAP_SIZE, INTERFACE_MTU, LINK_BYTES, MAX_INCOMING_RESOURCE_BYTES, MAX_LINKS,
    OFFER_RESPONSE_RESERVE_BYTES, QUEUE_DEPTH, ROLE_BUDGET_BYTES, SERVE_MARGIN_BYTES,
    SERVE_PEAK_BYTES, SERVE_RESERVE_BYTES, SERVE_RESOURCE_SDU, SESSION_BUDGET_BYTES,
    SYNC_SLICE_BYTES,
};
use leviculum_lxmf::propagation_node::{serve_largest_block_bytes, serve_peak_bytes};

/// The T114's boxed `NodeCore`, as its boot line reports it
/// (`size_of_val(&*node)`, `src/bin/t114.rs`), and as `heap_census.rs`
/// stated it beside the arithmetic before the move.
const T114_NODE_BOX: usize = 31_040;

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

/// The T114 boot line, unchanged by the move.
#[test]
fn the_t114_boot_line_is_what_it_was_before_the_move() {
    let nb = T114_NODE_BOX;
    println!(
        "HEAP_BUDGET node_box={nb} links={} total={} slack={} serve_cap={}",
        max_endpoint_links(nb),
        budget_total(nb),
        budget_slack(nb),
        budget_serve_cap(nb)
    );
    assert_eq!(max_endpoint_links(nb), 4);
    assert_eq!(budget_total(nb), 97_584);
    assert_eq!(budget_slack(nb), 720);
    assert_eq!(budget_serve_cap(nb), 194);
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
    // T114's 720 B fund 194 B, and 195 B would need more than 720.
    let slack = budget_slack(T114_NODE_BOX);
    assert!(serve_peak_bytes(194, SERVE_RESOURCE_SDU) <= slack);
    assert!(serve_peak_bytes(195, SERVE_RESOURCE_SDU) > slack);
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
    assert_eq!(above, 15_417);
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
        ("field 2026-09-12 steady used=90804", 7_500, 7_500, 194),
        // After the upload, 24 s before `allocation of 340 bytes failed`.
        ("field 2026-09-12 pre-panic used=93544", 4_760, 4_760, 194),
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
        // about one minimal message.
        if free < SERVE_MARGIN_BYTES {
            assert_eq!(cap, boot, "{what}");
        }
    }
}
