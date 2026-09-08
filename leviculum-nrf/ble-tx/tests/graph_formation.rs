//! The #375 simulation as a host test: N boards with random addresses,
//! random arrival orders, the REAL initiation rule
//! ([`leviculum_ble_tx::should_initiate`]) and the real slot limits —
//! three incoming links, one outgoing, a full board stops advertising
//! (#372), a board whose outgoing link is up stops scanning, waiting
//! boards rescan every round, and the fallback switches a board's scan
//! mode after a bounded number of empty rounds, exactly as the
//! firmware's clock does after `SCAN_FALLBACK_AFTER_MS`.
//!
//! The harness is calibrated against the issue's own Monte Carlo: under
//! the strict rule it reproduces the published disconnection rates
//! exactly (21 % of orders at 10 boards, 40 % at 20 — see the control
//! below). What it then shows about the fallback is sharper than the
//! issue's "0 %" row:
//!
//! 1. **No board is ever left without a BLE link.** The issue's
//!    headline failure — a board that "sits scanning forever with no
//!    BLE link at all" — is gone: 0 of 1 000 orders at both sizes,
//!    against 84/1000 under the strict rule at n=10.
//! 2. **The residual disconnection is one specific, rarer mechanism:
//!    the fully saturated cycle lock.** A fallback dial can land inside
//!    the dialler's own component and close a cycle there, spending the
//!    component's last free central; if every component saturates this
//!    way simultaneously, nobody is scanning and disjoint components
//!    never merge (2/1000 orders at n=10, 48/1000 at n=20). The tests
//!    assert that every disconnected order shows exactly this
//!    signature — all outgoing links in use — so a disconnected board
//!    that is still SEARCHING would fail them: the fallback rule itself
//!    has no remaining gap, the single-central topology does. Closing
//!    the lock needs a smarter target choice (collecting a scan window
//!    and dialling the lowest-addressed candidate yields 0/1000 at both
//!    sizes in this harness), which belongs to the free-slot-record
//!    batch (#375 item 3), not to the rule.
//! 3. The strict-only control proves the fallback is what changed the
//!    outcome, not the harness: same seeds, same loop, 210/1000
//!    disconnected orders.

use leviculum_ble_tx::{should_initiate, ScanMode};

/// The firmware's incoming-slot count (`PERIPH_LINKS`, #372).
const PERIPH_SLOTS: usize = 3;

/// The fallback bound, in scan rounds. The firmware bounds the strict
/// search in time (`SCAN_FALLBACK_AFTER_MS` = 30 s over 5 s retry
/// cycles, so about six passes); one sim round is one scan pass, hence
/// six empty rounds before a board's mode flips. The lock rate barely
/// moves with this bound (checked from 6 to 50 rounds), so its exact
/// value is not what the assertions lean on.
const FALLBACK_AFTER_ROUNDS: u32 = 6;

/// Seeded orders per claim. The strict rule's failure rate at 10 boards
/// is about one order in five, so a thousand orders leaves a vanishing
/// chance of the control finding nothing.
const ORDERS: u64 = 1_000;

/// xorshift64* — deterministic, seedable, no dependency.
fn next_rand(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

struct Board {
    /// 48-bit static-random address, distinct per board.
    addr: u64,
    arrived: bool,
    /// The one central link: index of the dialled board.
    outgoing: Option<usize>,
    /// Peripheral links, at most [`PERIPH_SLOTS`].
    incoming: Vec<usize>,
    /// Empty scan rounds since the last link-up or permitted peer —
    /// the sim's copy of the firmware's fallback clock.
    strict_rounds: u32,
}

/// Whether the two boards already hold a link in either direction: the
/// registry's churn rule — a second link to a linked identity is
/// refused (`BLE_LINK_DUP`), so the sim never forms one.
fn linked(boards: &[Board], a: usize, b: usize) -> bool {
    boards[a].outgoing == Some(b) || boards[b].outgoing == Some(a)
}

/// Replay one arrival order to quiescence and return the final boards.
fn run_sim(n: usize, seed: u64, with_fallback: bool) -> Vec<Board> {
    let mut rng = seed | 1;
    let mut boards: Vec<Board> = Vec::with_capacity(n);
    while boards.len() < n {
        let addr = (next_rand(&mut rng) & 0xFFFF_FFFF_FFFF) | 0xC000_0000_0000;
        if boards.iter().any(|b| b.addr == addr) {
            continue;
        }
        boards.push(Board {
            addr,
            arrived: false,
            outgoing: None,
            incoming: Vec::new(),
            strict_rounds: 0,
        });
    }

    // The arrival order under test: a seeded shuffle, one per round.
    let mut arrival: Vec<usize> = (0..n).collect();
    for i in (1..n).rev() {
        arrival.swap(i, (next_rand(&mut rng) as usize) % (i + 1));
    }

    let mut linkless_streak: u32 = 0;
    for round in 0..10_000 {
        if round < n {
            boards[arrival[round]].arrived = true;
        }

        // Scan order within the round is part of the replayed
        // randomness: which searching board wins a contended slot.
        let mut scan_order: Vec<usize> = (0..n).collect();
        for i in (1..n).rev() {
            scan_order.swap(i, (next_rand(&mut rng) as usize) % (i + 1));
        }

        let mut any_link = false;
        for i in scan_order {
            // Linked centrals do not scan; unarrived boards are absent.
            if !boards[i].arrived || boards[i].outgoing.is_some() {
                continue;
            }
            let mode = if with_fallback && boards[i].strict_rounds >= FALLBACK_AFTER_ROUNDS {
                ScanMode::Fallback
            } else {
                ScanMode::Strict
            };
            // Visible: arrived, advertising (a full board is not), not
            // ourselves, not already linked to us; then the real rule.
            let candidates: Vec<usize> = (0..n)
                .filter(|&p| {
                    p != i
                        && boards[p].arrived
                        && boards[p].incoming.len() < PERIPH_SLOTS
                        && !linked(&boards, i, p)
                        && should_initiate(0, boards[i].addr, Some(0), boards[p].addr, mode)
                            .initiate()
                })
                .collect();
            match candidates.as_slice() {
                [] => boards[i].strict_rounds += 1,
                found => {
                    // The firmware dials whichever eligible PDU it saw
                    // first, so the pick is arbitrary: seeded random.
                    let target = found[(next_rand(&mut rng) as usize) % found.len()];
                    boards[i].outgoing = Some(target);
                    boards[target].incoming.push(i);
                    // A link up resets the clock in either role, as
                    // `peer_link_up` does on the firmware.
                    boards[i].strict_rounds = 0;
                    boards[target].strict_rounds = 0;
                    any_link = true;
                }
            }
        }

        linkless_streak = if any_link { 0 } else { linkless_streak + 1 };
        // Quiescent: everyone has arrived and even the boards that
        // reached fallback during the streak found nobody. Visibility
        // only changes when a link forms, so nothing changes hereafter.
        if round >= n && linkless_streak > FALLBACK_AFTER_ROUNDS {
            break;
        }
    }

    // The churn rule held: no pair ever holds two links.
    for a in 0..n {
        if let Some(b) = boards[a].outgoing {
            assert_ne!(boards[b].outgoing, Some(a), "duplicate pair link");
        }
        assert!(boards[a].incoming.len() <= PERIPH_SLOTS);
    }
    boards
}

/// Connectivity over the undirected link graph.
fn is_connected(boards: &[Board]) -> bool {
    let n = boards.len();
    let mut seen = vec![false; n];
    let mut queue = vec![0usize];
    seen[0] = true;
    while let Some(at) = queue.pop() {
        for (next, seen_next) in seen.iter_mut().enumerate() {
            if !*seen_next && linked(boards, at, next) {
                *seen_next = true;
                queue.push(next);
            }
        }
    }
    seen.iter().all(|&s| s)
}

/// The two fallback claims at one size: no board ends linkless, and
/// every disconnected order carries the saturated-cycle-lock signature
/// (all outgoing links in use, so nobody was still searching), with the
/// lock rarer than `max_locked` orders in [`ORDERS`].
fn assert_fallback_properties(n: usize, max_locked: usize) {
    let mut locked = Vec::new();
    for seed in 0..ORDERS {
        let boards = run_sim(n, 0xB1E5_0000 + seed, true);
        for (i, b) in boards.iter().enumerate() {
            assert!(
                b.outgoing.is_some() || !b.incoming.is_empty(),
                "board {i} ended with no BLE link at n={n}, seed {seed}"
            );
        }
        if !is_connected(&boards) {
            assert!(
                boards.iter().all(|b| b.outgoing.is_some()),
                "disconnected with a board still searching at n={n}, seed {seed}: \
                 the fallback rule itself failed, not the saturation lock"
            );
            locked.push(seed);
        }
    }
    assert!(
        locked.len() <= max_locked,
        "saturated-cycle locks at n={n} grew past {max_locked}/{ORDERS}: {locked:?}"
    );
}

#[test]
fn ten_boards_connect_or_rarely_lock_saturated_and_nobody_is_linkless() {
    // Measured on this seed stream: 2/1000 locked orders (the strict
    // control below loses 210/1000).
    assert_fallback_properties(10, 5);
}

#[test]
fn twenty_boards_connect_or_rarely_lock_saturated_and_nobody_is_linkless() {
    // Measured on this seed stream: 48/1000 locked orders (the strict
    // rule loses 400/1000).
    assert_fallback_properties(20, 60);
}

/// The control: identical harness, identical seeds, fallback off — the
/// strict sort alone must strand boards for a large share of orders, or
/// the fallback tests above prove nothing about the fallback. The
/// measured rate is 210/1000 disconnected orders (the issue's Monte
/// Carlo says 21 %), 84 of which leave some board with no link at all.
#[test]
fn control_the_strict_rule_alone_disconnects_a_fifth_of_the_orders() {
    let mut disconnected = 0usize;
    let mut some_board_linkless = 0usize;
    for seed in 0..ORDERS {
        let boards = run_sim(10, 0xB1E5_0000 + seed, false);
        if !is_connected(&boards) {
            disconnected += 1;
        }
        if boards
            .iter()
            .any(|b| b.outgoing.is_none() && b.incoming.is_empty())
        {
            some_board_linkless += 1;
        }
    }
    assert!(
        disconnected >= 100,
        "the strict rule connected almost every order ({disconnected}/{ORDERS} lost); \
         the control lost its teeth"
    );
    assert!(
        some_board_linkless >= 50,
        "strict orders with a fully linkless board: {some_board_linkless}/{ORDERS}"
    );
}
