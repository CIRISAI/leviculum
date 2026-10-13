//! Proof-of-work stamp primitives for on-network interface discovery.
//!
//! This is a byte-for-byte port of the stamp scheme used by Python
//! `LXMF.LXStamper` (`reference/LXMF/LXMF/LXStamper.py`), restricted to the pieces
//! that interface discovery needs. A stamp is a random 32-byte value whose
//! `full_hash(workblock + stamp)` has at least `cost` leading zero bits, where
//! the `workblock` is deterministically expanded from the announce material via
//! HKDF.
//!
//! Discovery mints at `cost = 16` ([`DEFAULT_STAMP_VALUE`]) and `rounds = 20`
//! ([`WORKBLOCK_EXPAND_ROUNDS`]); other LXMF call sites use different expansion
//! rounds, which is why the round count is a parameter here. What we *require*
//! of a received stamp is a separate, lower number
//! ([`MIN_REQUIRED_STAMP_VALUE`]) — see its docs for why the two differ.

use alloc::vec::Vec;
use rand_core::CryptoRngCore;

use crate::crypto::{derive_key, full_hash, full_hash_parts, StreamHasher};
use crate::msgpack;

/// Size of a stamp in bytes (`RNS.Identity.HASHLENGTH // 8`).
pub const STAMP_SIZE: usize = 32;

/// Stamp value (leading zero bits) we MINT discovery announces at.
///
/// Tracks Python `RNS.Discovery.InterfaceAnnouncer.DEFAULT_STAMP_VALUE`, which
/// RNS 1.5.0 raised from 14 to 16 (Codeberg #328). A 1.5.0 listener runs with
/// `required_value = 16` and silently discards anything weaker, so minting at
/// 14 made us invisible to every 1.5.0 node. Minting at 16 is accepted by 1.5.0
/// and 1.3.5 listeners alike — a stamp is checked with `>=`, so an
/// over-strength stamp is never a problem for an older peer.
pub const DEFAULT_STAMP_VALUE: u32 = 16;

/// Stamp value we REQUIRE of a received discovery announce.
///
/// Deliberately the *old* Python default (14), not [`DEFAULT_STAMP_VALUE`].
/// Python couples the two constants, so a 1.5.0 node requires 16 and therefore
/// stops seeing 1.3.5 neighbours; adopting that coupling would have turned
/// #328's one-sided break into a two-sided one, with us newly deaf to every
/// 1.3.5 announce on the air. Splitting the constants is a deviation in
/// internals only: the wire format is untouched, the accepted set is a strict
/// superset of Python's (a 16-value stamp still passes a 14 requirement), and
/// hearing more neighbours is squarely Priority 1. Python itself treats the
/// receive threshold as configuration — `InterfaceDiscovery(required_value=...)`
/// — so this is a config choice, not a protocol divergence.
pub const MIN_REQUIRED_STAMP_VALUE: u32 = 14;

/// The receive gate must never rise above the mint cost, or we would reject our
/// own announces on a loopback or a two-node lab mesh. Asserted at compile time
/// rather than in a test: the pair is a design invariant of the split above, and
/// a build that violates it should not exist for a test to catch.
const _: () = assert!(MIN_REQUIRED_STAMP_VALUE <= DEFAULT_STAMP_VALUE);

/// Number of HKDF expansion rounds for the discovery workblock.
pub const WORKBLOCK_EXPAND_ROUNDS: usize = 20;

/// Length (bytes) of one HKDF expansion round output.
const ROUND_OUTPUT_LEN: usize = 256;

/// Expand `material` into the proof-of-work workblock.
///
/// Mirrors `LXStamper.stamp_workblock`: for each round `n` in `0..rounds`,
/// append `hkdf(length=256, derive_from=material, salt=full_hash(material +
/// msgpack(n)), context=None)`. The resulting workblock is `rounds * 256`
/// bytes.
pub fn stamp_workblock(material: &[u8], rounds: usize) -> Vec<u8> {
    let mut workblock = Vec::with_capacity(rounds * ROUND_OUTPUT_LEN);
    let mut salt_input = Vec::with_capacity(material.len() + 9);
    for n in 0..rounds {
        // salt = full_hash(material + msgpack.packb(n))
        salt_input.clear();
        salt_input.extend_from_slice(material);
        msgpack::write_uint(&mut salt_input, n as u64);
        let salt = full_hash(&salt_input);

        let mut block = [0u8; ROUND_OUTPUT_LEN];
        derive_key(material, Some(&salt), None, &mut block);
        workblock.extend_from_slice(&block);
    }
    workblock
}

/// Number of leading zero bits of `full_hash(workblock + stamp)`.
///
/// Mirrors `LXStamper.stamp_value`.
pub fn stamp_value(workblock: &[u8], stamp: &[u8]) -> u32 {
    digest_value(&full_hash_parts(&[workblock, stamp]))
}

/// Number of leading zero bits of a stamp digest.
fn digest_value(digest: &[u8; 32]) -> u32 {
    let mut value = 0u32;
    for &byte in digest.iter() {
        if byte == 0 {
            value += 8;
        } else {
            value += byte.leading_zeros();
            break;
        }
    }
    value
}

/// Whether `stamp` satisfies `target_cost` against `workblock`.
///
/// Mirrors `LXStamper.stamp_valid`: `full_hash(workblock + stamp)`, read as a
/// big-endian 256-bit integer, must be `<= 2^(256 - target_cost)`. A
/// `target_cost` of 0 is always valid (the target exceeds the 256-bit range).
pub fn stamp_valid(stamp: &[u8], target_cost: u32, workblock: &[u8]) -> bool {
    if target_cost == 0 {
        return true;
    }
    if target_cost > 256 {
        return false;
    }
    digest_meets_cost(&full_hash_parts(&[workblock, stamp]), target_cost)
}

/// The acceptance rule, once for the mint and the verifiers alike: `digest`,
/// read as a big-endian 256-bit integer, is `<= 2^(256 - target_cost)`.
///
/// `target_cost` must be in `1..=256`; its callers answer the cases outside
/// that range before they get here.
fn digest_meets_cost(digest: &[u8; 32], target_cost: u32) -> bool {
    // target = 1 << (256 - target_cost), as a 32-byte big-endian value.
    let p = 256 - target_cost as usize; // 0..=255
    let mut target = [0u8; STAMP_SIZE];
    let byte_from_lsb = p / 8;
    let bit = p % 8;
    target[STAMP_SIZE - 1 - byte_from_lsb] = 1u8 << bit;

    // digest <= target, compared as unsigned big-endian integers.
    *digest <= target
}

/// Why a stamp could not be generated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StampError {
    /// `cost` exceeds the 256-bit width of the hash the target is compared
    /// against, so no stamp can ever satisfy it.
    CostExceedsHashWidth {
        /// The rejected cost.
        cost: u32,
    },
}

impl core::fmt::Display for StampError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            StampError::CostExceedsHashWidth { cost } => write!(
                f,
                "stamp cost {cost} exceeds the 256-bit hash width; no stamp can satisfy it"
            ),
        }
    }
}

/// Brute-force a valid stamp for `material` at the given `cost` and `rounds`.
///
/// Returns the stamp and its realised value (`>= cost`). Mirrors
/// `LXStamper.generate_stamp` (single-worker form): draw random 32-byte
/// candidates until one is valid.
///
/// # Termination
///
/// [`stamp_valid`] answers `false` unconditionally for `target_cost > 256` —
/// the target would need more bits than the hash has — so the search loop
/// below would spin forever rather than merely for a long time. A caller that
/// let a cost of 257 through would therefore not get a slow node, it would get
/// a permanently dead one, and on the discovery path that cost is one
/// off-by-one away from the legal maximum. The guard turns that into an error
/// at the door.
///
/// This bounds nothing else: the search is exponential in `cost`, so a cost in
/// the high tens is already unfinishable in practice. Keeping *peer-chosen*
/// costs off this function entirely is a separate invariant, held by
/// `docs/src/concepts/core-lock-budget.md`.
pub fn generate_stamp(
    material: &[u8],
    cost: u32,
    rounds: usize,
    rng: &mut impl CryptoRngCore,
) -> Result<([u8; STAMP_SIZE], u32), StampError> {
    if cost > 256 {
        return Err(StampError::CostExceedsHashWidth { cost });
    }
    // The workblock is the same for every attempt, so its SHA-256 state is
    // built once and each attempt only feeds its 32-byte stamp into a clone:
    // a cost-16 mint draws about 65 536 stamps, and hashing the 5 KiB
    // workblock for each of them was nearly all of its work.
    let mut base = StreamHasher::new();
    base.update(&stamp_workblock(material, rounds));
    let mut stamp = [0u8; STAMP_SIZE];
    loop {
        rng.fill_bytes(&mut stamp);
        let digest = base.finish_with(&stamp);
        if cost == 0 || digest_meets_cost(&digest, cost) {
            return Ok((stamp, digest_value(&digest)));
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use rand_core::{OsRng, RngCore};

    // Golden vectors captured from Python `LXMF.LXStamper` over the vendored
    // tree (see `tests/discovery_golden_gen.py`). `material = 0xaa * 32`,
    // `rounds = 20`.
    const PRIM_MATERIAL: [u8; 32] = [0xaa; 32];
    const PRIM_WB_FULLHASH: [u8; 32] = [
        0x0d, 0xc4, 0x2e, 0xe9, 0x32, 0x6a, 0x35, 0x6a, 0xb4, 0xf5, 0x66, 0x7a, 0x3c, 0x88, 0x13,
        0x0e, 0xa0, 0xf7, 0x2c, 0xe4, 0x91, 0xbc, 0x8f, 0x96, 0xaf, 0xfb, 0x91, 0x24, 0xad, 0x0a,
        0xd1, 0x77,
    ];

    #[test]
    fn workblock_matches_python() {
        let wb = stamp_workblock(&PRIM_MATERIAL, WORKBLOCK_EXPAND_ROUNDS);
        assert_eq!(wb.len(), WORKBLOCK_EXPAND_ROUNDS * ROUND_OUTPUT_LEN);
        assert_eq!(full_hash(&wb), PRIM_WB_FULLHASH);
    }

    #[test]
    fn zero_stamp_value_matches_python() {
        // full_hash(workblock + 0x00*32) for the primitive material has 0
        // leading zero bits, per Python.
        let wb = stamp_workblock(&PRIM_MATERIAL, WORKBLOCK_EXPAND_ROUNDS);
        assert_eq!(stamp_value(&wb, &[0u8; 32]), 0);
    }

    #[test]
    fn generate_then_validate_roundtrip() {
        let material = [0x11u8; 32];
        // A low cost keeps the brute force fast and deterministic in runtime.
        let (stamp, value) =
            generate_stamp(&material, 8, WORKBLOCK_EXPAND_ROUNDS, &mut OsRng).expect("cost 8");
        assert!(value >= 8);
        let wb = stamp_workblock(&material, WORKBLOCK_EXPAND_ROUNDS);
        assert!(stamp_valid(&stamp, 8, &wb));
        assert_eq!(stamp_value(&wb, &stamp), value);
    }

    /// RED-before: `stamp_valid` rejects every candidate above 256, so the
    /// search loop never returned. This test could not be written against the
    /// old signature at all — it would have hung the suite.
    #[test]
    fn cost_above_the_hash_width_is_refused_rather_than_ground_forever() {
        let err = generate_stamp(&[0x33u8; 32], 257, WORKBLOCK_EXPAND_ROUNDS, &mut OsRng)
            .expect_err("257 is unsatisfiable, not merely expensive");
        assert_eq!(err, StampError::CostExceedsHashWidth { cost: 257 });
    }

    /// The guard's boundary is exactly `stamp_valid`'s: 256 is the largest cost
    /// for which a target still fits in the hash, so it stays on the accepted
    /// side. Asserted against `stamp_valid` rather than by calling
    /// `generate_stamp(_, 256, ..)`, which would run until the heat death of
    /// the universe — unfinishable is not the same defect as non-terminating,
    /// and only the second one is what this guard is for.
    #[test]
    fn the_guard_boundary_matches_what_stamp_valid_can_answer() {
        let wb = stamp_workblock(&[0x44u8; 32], 1);
        // A cost of 256 targets `result <= 1`: satisfiable in principle, so
        // `stamp_valid` does not short-circuit it the way it does 257.
        assert!(!stamp_valid(&[0u8; 32], 256, &wb) || stamp_value(&wb, &[0u8; 32]) >= 256);
        assert!(
            !stamp_valid(&[0u8; 32], 257, &wb),
            "257 is never satisfiable"
        );
    }

    /// Attempt rate of the discovery mint at a fixed workblock, and of the
    /// standalone verifier for comparison. Ignored because it is a
    /// measurement, not a check; run it under `--release` with
    /// `--ignored --nocapture`.
    #[test]
    #[ignore = "measurement, run manually under --release with --nocapture"]
    fn bench_stamp_attempt_rate() {
        const ATTEMPTS: u32 = 200_000;
        let wb = stamp_workblock(&[0x55u8; 32], WORKBLOCK_EXPAND_ROUNDS);
        let rate = |label: &str, attempt: &mut dyn FnMut(&[u8; STAMP_SIZE]) -> bool| {
            let mut stamp = [0u8; STAMP_SIZE];
            let mut hits = 0u32;
            let start = std::time::Instant::now();
            for n in 0..ATTEMPTS {
                stamp[..4].copy_from_slice(&n.to_le_bytes());
                if attempt(&stamp) {
                    hits += 1;
                }
            }
            let secs = start.elapsed().as_secs_f64();
            std::println!(
                "STAMP_ATTEMPT_RATE path={label} attempts={ATTEMPTS} secs={secs:.3} per_sec={:.0} hits={hits}",
                f64::from(ATTEMPTS) / secs
            );
        };
        rate("verifier", &mut |stamp| {
            stamp_valid(stamp, DEFAULT_STAMP_VALUE, &wb)
        });
        let mut base = StreamHasher::new();
        base.update(&wb);
        rate("mint", &mut |stamp| {
            digest_meets_cost(&base.finish_with(stamp), DEFAULT_STAMP_VALUE)
        });
    }

    /// The pre-696 spelling of the check, concatenating the workblock and the
    /// stamp into one buffer: the reference the incremental mint is held to.
    fn concatenated_digest(workblock: &[u8], stamp: &[u8]) -> [u8; 32] {
        let mut input = Vec::with_capacity(workblock.len() + stamp.len());
        input.extend_from_slice(workblock);
        input.extend_from_slice(stamp);
        full_hash(&input)
    }

    fn concatenated_stamp_valid(stamp: &[u8], target_cost: u32, workblock: &[u8]) -> bool {
        if target_cost == 0 {
            return true;
        }
        let p = 256 - target_cost as usize;
        let mut target = [0u8; STAMP_SIZE];
        target[STAMP_SIZE - 1 - p / 8] = 1u8 << (p % 8);
        concatenated_digest(workblock, stamp) <= target
    }

    /// A stamp from the incremental mint passes the old concatenating check,
    /// and its reported value is the old spelling's value.
    #[test]
    fn a_minted_stamp_passes_the_concatenating_check() {
        let material = [0x66u8; 32];
        let wb = stamp_workblock(&material, WORKBLOCK_EXPAND_ROUNDS);
        for cost in [0u32, 1, 4, 10] {
            let (stamp, value) =
                generate_stamp(&material, cost, WORKBLOCK_EXPAND_ROUNDS, &mut OsRng)
                    .expect("low cost");
            assert!(concatenated_stamp_valid(&stamp, cost, &wb), "cost {cost}");
            assert_eq!(value, digest_value(&concatenated_digest(&wb, &stamp)));
            assert!(value >= cost);
        }
    }

    /// The cloned base state finalised with a stamp is the digest of the
    /// concatenation, for random workblocks and stamps.
    #[test]
    fn the_incremental_digest_is_the_concatenated_digest() {
        for _ in 0..1000 {
            let mut material = [0u8; 32];
            OsRng.fill_bytes(&mut material);
            let mut stamp = [0u8; STAMP_SIZE];
            OsRng.fill_bytes(&mut stamp);
            let wb = stamp_workblock(&material, 1 + (material[0] % 3) as usize);
            let mut base = StreamHasher::new();
            base.update(&wb);
            let expected = concatenated_digest(&wb, &stamp);
            assert_eq!(base.finish_with(&stamp), expected);
            assert_eq!(full_hash_parts(&[&wb, &stamp]), expected);
        }
    }

    #[test]
    fn valid_target_boundary() {
        // Cost 0 always valid; an all-ones hash target is only met by cost 0.
        let wb = stamp_workblock(&[0x22u8; 32], WORKBLOCK_EXPAND_ROUNDS);
        assert!(stamp_valid(&[0u8; 32], 0, &wb));
    }
}
