//! mvr: two copies of ONE emission, relayed first and direct second
//! (Codeberg #231).
//!
//! ## The clause under test
//!
//! `transport.rs`'s equal-or-fewer-hops acceptance arm has a second
//! condition beside "the emission is newer": `announce_emitted ==
//! path_timebase && accept_same_emission(..)`, today
//! [`SameEmissionRule::FewerHopsWins`]. A second copy of an announce we
//! already installed replaces the path whenever it took strictly fewer
//! hops. Python has no such acceptance: the vendored
//! reference/Reticulum/RNS/Transport.py:1772 (1.3.5) demands an unseen
//! random blob AND `announce_emitted > path_timebase`, and 1.5.2 — the
//! version periculum's `rnsd` arm pins — adds only a `gravity` arm on equal
//! emission, which rejects whenever both interfaces carry the default
//! gravity of 0, i.e. on any default config. Hop count never appears in
//! either version's same-emission test. The 1.5.2 line numbers are in
//! docs/src/protocol-notes/announce-dedup-and-path-replacement.md §5, in
//! prose, because that tree is not vendored.
//!
//! ## Why this is the minimal test and not the soak
//!
//! The instrument #231 was expected to use — the `pathchoice_*` soak's
//! direct share — cannot see the clause. `path_soak` reads the hop count
//! out of the very `path_wait` invocation that resolved
//! (`periculum/src/executor.rs::execute_path_soak`), and `lnpath -w` polls
//! at 100 ms and reports as soon as the path appears
//! (`leviculum-cli/src/lnpath.rs::wait_and_report`). A second copy of one
//! emission is a whole frame behind the first (p332's loss50 arm measured
//! 150071 ms of airtime over 436 frames, ~344 ms per frame), so the swap
//! lands after the read: the share it reports is the hop count of the
//! FIRST install, in which the clause has by definition not fired yet.
//! What the clause does has to be asserted here, and counted in the logs
//! from `reason=`.
//!
//! ## What is pinned
//!
//! 1. Today's behaviour: relayed (hops 2, via bravo) first, direct
//!    (hops 1) second — the entry moves to hops 1 with `next_hop` None,
//!    and `PATH_ADD` names `reason=same_emission_fewer_hops`, the readout
//!    the per-arm count is taken from.
//! 2. The control: direct first, relayed second — the entry stays at
//!    hops 1 under every candidate rule, so a change to #231's decision
//!    cannot regress the ordinary case.
//! 3. Python's rule, expressed but NOT wired in: the same two copies under
//!    [`SameEmissionRule::StrictlyNewerOnly`] keep the relayed 2-hop
//!    entry.
//! 4. Option (C) of #231: fewer hops only on a different interface — the
//!    one-radio room rejects, a genuine second route accepts.
//!
//! Sans-I/O: 1 node, 1-2 mock interfaces, deterministic, sub-second. No
//! behaviour change: the live rule is still `FewerHopsWins`.

extern crate std;

use std::boxed::Box;
use std::string::String;
use std::vec::Vec;

use rand_core::OsRng;

use crate::constants::{MTU, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction};
use crate::identity::Identity;
use crate::memory_storage::MemoryStorage;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{HeaderType, Packet, TransportType};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::transport::{
    accept_same_emission, InterfaceId, SameEmissionCopy, SameEmissionRule, SAME_EMISSION_RULE,
};

type TransportNode = NodeCore<OsRng, MockClock, MemoryStorage>;

/// bravo's transport id: the relay that carries the 2-hop copy.
const BRAVO: [u8; TRUNCATED_HASHBYTES] = [0x42; TRUNCATED_HASHBYTES];

fn make_node() -> Box<TransportNode> {
    NodeCoreBuilder::new().enable_transport(true).build_boxed(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        MemoryStorage::with_defaults(),
    )
}

fn add_iface(node: &mut TransportNode, name: &'static str, id: u8) -> usize {
    let idx = node
        .transport
        .register_interface(Box::new(MockInterface::new(name, id)));
    node.set_interface_name(idx, String::from(name));
    idx
}

/// charlie, the announced destination, and the ONE announce this test has
/// two copies of.
struct Charlie {
    dest_hash: crate::DestinationHash,
    raw: Vec<u8>,
}

fn make_charlie() -> Charlie {
    let identity = Identity::generate(&mut OsRng);
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvrapp",
        &["probe"],
    )
    .unwrap();
    let dest_hash = *dest.hash();
    let ann = dest
        .announce(None, &mut OsRng, TEST_TIME_MS, TEST_TIME_MS / 1000)
        .unwrap();
    let mut buf = [0u8; MTU];
    let len = ann.pack(&mut buf).unwrap();
    Charlie {
        dest_hash,
        raw: buf[..len].to_vec(),
    }
}

impl Charlie {
    /// The copy charlie's own radio put on the air: wire hops 0, no
    /// transport id. The transport increments on ingress, so it is
    /// processed at hops 1.
    fn direct(&self) -> Vec<u8> {
        self.raw.clone()
    }

    /// The same announce after bravo forwarded it: wire hops 1 and bravo's
    /// id, processed at hops 2. Only the header changes — the signed body,
    /// and with it the random blob and its emission timebase, is byte-for-
    /// byte the direct copy's (see
    /// `docs/src/protocol-notes/announce-dedup-and-path-replacement.md` §1:
    /// both copies even hash the same).
    fn relayed(&self) -> Vec<u8> {
        let mut p = Packet::unpack(&self.raw).unwrap();
        p.flags.header_type = HeaderType::Type2;
        p.flags.transport_type = TransportType::Transport;
        p.hops = 1;
        p.transport_id = Some(BRAVO);
        let mut buf = [0u8; MTU];
        let len = p.pack(&mut buf).unwrap();
        buf[..len].to_vec()
    }
}

/// Assertion 1 + 3: the relayed copy arrives first, the direct copy of the
/// SAME emission arrives second on the same radio. Today's rule swaps the
/// route; Python's rule, expressed against the pure function, keeps it.
#[test]
fn relayed_first_then_direct_swaps_to_the_one_hop_route() {
    let mut node = make_node();
    let lora = add_iface(&mut node, "lora_sx1262", 0);
    let charlie = make_charlie();

    // bravo's copy wins the race: 2 hops, next hop bravo.
    let _ = node.handle_packet(InterfaceId(lora), &charlie.relayed());
    let installed = node
        .transport
        .get_path_clone(charlie.dest_hash.as_bytes())
        .expect("the relayed copy installs the first path");
    assert_eq!(installed.hops, 2, "one relay in between");
    assert_eq!(
        installed.next_hop,
        Some(BRAVO),
        "the 2-hop entry routes through bravo"
    );

    // charlie's own copy of the SAME announce arrives second.
    let ((), logs) = crate::test_log_capture::with_captured_logs(|| {
        let _ = node.handle_packet(InterfaceId(lora), &charlie.direct());
    });
    let after = node
        .transport
        .get_path_clone(charlie.dest_hash.as_bytes())
        .expect("path still present");
    assert_eq!(
        after.hops, 1,
        "today's rule (SameEmissionRule::FewerHopsWins) replaces the \
         installed 2-hop entry with the direct copy of the same emission \
         — Codeberg #231"
    );
    assert_eq!(
        after.next_hop, None,
        "a 1-hop path is reached without a relay, so next_hop is cleared"
    );
    // With the quotes: `tracing` renders a `&'static str` field quoted, so
    // the recipe that counts these across a corpus run has to grep
    // `reason="same_emission_fewer_hops"` and not the bare token. Pinned
    // here because the count is the whole measurement #231 is waiting on.
    assert!(
        logs.contains("event=\"PATH_ADD\"") && logs.contains("reason=\"same_emission_fewer_hops\""),
        "the swap must be attributable in the logs: `reason=` on PATH_ADD is \
         the readout #231's per-arm count is taken from, and a soak's direct \
         share cannot substitute for it; logs={logs:?}"
    );

    // Assertion 3 — Python's rule on the very copies this run swapped on,
    // via the pure function, NOT wired in.
    let existing = SameEmissionCopy {
        hops: 2,
        interface_index: lora,
    };
    let incoming = SameEmissionCopy {
        hops: 1,
        interface_index: lora,
    };
    assert!(
        !accept_same_emission(SameEmissionRule::StrictlyNewerOnly, existing, incoming),
        "Python 1.3.5 (reference/Reticulum/RNS/Transport.py:1772) requires a \
         strictly newer emission \
         and an unseen random blob; a second copy of one emission is \
         neither, however few hops it saved"
    );
    let hops_under_pythons_rule =
        if accept_same_emission(SameEmissionRule::StrictlyNewerOnly, existing, incoming) {
            incoming.hops
        } else {
            existing.hops
        };
    assert_eq!(
        hops_under_pythons_rule, 2,
        "under Python's rule alpha keeps bravo's 2-hop route until a fresh \
         emission arrives — this is the behaviour difference #231 decides on"
    );
}

/// Assertion 2, the control: the ordinary order. The direct copy installs
/// first, and the relayed copy of the same emission must not drag the entry
/// back out to 2 hops — which is the worse-hops arm's job, and is true
/// under all three candidate rules.
#[test]
fn direct_first_then_relayed_keeps_the_one_hop_route() {
    let mut node = make_node();
    let lora = add_iface(&mut node, "lora_sx1262", 0);
    let charlie = make_charlie();

    let _ = node.handle_packet(InterfaceId(lora), &charlie.direct());
    let installed = node
        .transport
        .get_path_clone(charlie.dest_hash.as_bytes())
        .expect("the direct copy installs the first path");
    assert_eq!(installed.hops, 1, "heard straight from charlie");
    assert_eq!(installed.next_hop, None, "no relay in between");

    let _ = node.handle_packet(InterfaceId(lora), &charlie.relayed());
    let after = node
        .transport
        .get_path_clone(charlie.dest_hash.as_bytes())
        .expect("path still present");
    assert_eq!(
        after.hops, 1,
        "a worse-hops copy of an emission already installed is ignored \
         unless the path expired or went unresponsive \
         (Transport.py:1793-1826)"
    );
    assert_eq!(after.next_hop, None, "still the direct route");

    // The control holds for every rule #231 could pick, because none of
    // them is even consulted: this is the worse-hops arm, not the
    // equal-or-fewer-hops one.
    for rule in [
        SameEmissionRule::FewerHopsWins,
        SameEmissionRule::StrictlyNewerOnly,
        SameEmissionRule::FewerHopsOnOtherInterface,
    ] {
        assert!(
            !accept_same_emission(
                rule,
                SameEmissionCopy {
                    hops: 1,
                    interface_index: lora,
                },
                SameEmissionCopy {
                    hops: 2,
                    interface_index: lora,
                },
            ),
            "no candidate rule lets a MORE-hops copy of the same emission \
             through: {rule:?}"
        );
    }
}

/// Assertion 4: option (C) of #231 — the clause restricted to a genuine
/// second route. One radio hearing both copies is a retry of one route and
/// is rejected; the same hop counts across two interfaces are two routes
/// and are accepted.
#[test]
fn option_c_accepts_only_across_interfaces() {
    let mut node = make_node();
    let lora = add_iface(&mut node, "lora_sx1262", 0);
    let ble = add_iface(&mut node, "ble_nrf", 1);
    assert_ne!(lora, ble, "two distinct interfaces");

    let same_radio = accept_same_emission(
        SameEmissionRule::FewerHopsOnOtherInterface,
        SameEmissionCopy {
            hops: 2,
            interface_index: lora,
        },
        SameEmissionCopy {
            hops: 1,
            interface_index: lora,
        },
    );
    assert!(
        !same_radio,
        "both copies on one radio: option (C) reads that as the same route \
         heard twice, not a shorter second one"
    );

    let other_radio = accept_same_emission(
        SameEmissionRule::FewerHopsOnOtherInterface,
        SameEmissionCopy {
            hops: 2,
            interface_index: lora,
        },
        SameEmissionCopy {
            hops: 1,
            interface_index: ble,
        },
    );
    assert!(
        other_radio,
        "a shorter copy on a second interface is a real alternative route, \
         which is the case the deviation was presumably written for"
    );
}

/// The live rule is still today's. This pass changed no behaviour, and
/// this is the assertion that says so: flipping
/// [`SAME_EMISSION_RULE`] is #231's decision, not this file's.
#[test]
fn the_live_rule_is_still_todays() {
    assert_eq!(
        SAME_EMISSION_RULE,
        SameEmissionRule::FewerHopsWins,
        "changing the live rule is Lew's call on #231; a pass that flips it \
         must land the pathchoice evidence with it"
    );
}
