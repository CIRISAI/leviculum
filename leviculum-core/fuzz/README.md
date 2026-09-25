# Fuzz harness for the wire-format parsers (Codeberg #23)

Coverage-guided fuzzing (cargo-fuzz / libFuzzer) for the functions that parse
UNTRUSTED bytes off the wire. A parser that panics, overflows (in debug),
infinite-loops, or OOMs on malformed input is a remote DoS, so every target
must return `Err`/`None` gracefully on any input.

## Requirements

- Rust **nightly** (libFuzzer needs `-Z` sanitizer flags): `rustup toolchain install nightly`.
  The repo's pinned toolchain (`rust-toolchain.toml`, stable 1.97.1) cannot build these
  targets, which is why every invocation below is `cargo +nightly`. Last verified
  against `cargo 1.98.0-nightly (598ab48ec 2026-06-17)`; every run prints the resolved
  version as `FUZZ_TOOLCHAIN`, and `LEVICULUM_FUZZ_TOOLCHAIN=nightly-YYYY-MM-DD` pins it
  by date without editing anything (see `scripts/install-ci.sh`).
- **cargo-fuzz**: `cargo install cargo-fuzz`
- Build/run against the **glibc** host target, NOT the workspace musl default
  (`.cargo/config.toml` sets musl; ASan wants glibc):
  `--target x86_64-unknown-linux-gnu`

This crate is detached from the repo workspace (its own `[workspace]` table)
and is excluded from `just standard`; the regression tests for any crash it
finds live in the normal `leviculum-core` unit suite instead.

## Targets (ranked by exposure — network-reachable first)

| target                           | parser                                              | reachability |
|----------------------------------|-----------------------------------------------------|--------------|
| `packet_unpack`                  | `packet::Packet::unpack` (+ `packet_hash`)          | every inbound wire packet, every interface |
| `resource_advertisement_unpack`  | `resource::ResourceAdvertisement::unpack`           | peer resource advertisement over an established link, no PoW |
| `discovery_announce`             | `discovery::parse_announce_app_data`                | discovery announce from any peer (PoW-gated; fuzzed with `required_value = 0`) |
| `ifac_verify`                    | `ifac::IfacConfig::verify_ifac` / `has_ifac_flag`   | every inbound packet on an IFAC-guarded interface |
| `hdlc_deframe`                   | `framing::hdlc::Deframer::process`                  | raw serial/TCP byte stream (KISS/HDLC) |
| `kiss_deframe`                   | `framing::kiss::KissDeframer::process`              | raw serial byte stream (RNode/KISS) |

The msgpack readers in `src/msgpack.rs` (including the recursive
`skip_msgpack_value`) are exercised transitively by
`resource_advertisement_unpack` and `discovery_announce`.

## Run them

```sh
just fuzz                    # every target in both fuzz crates, 60 s each
just fuzz hdlc_deframe       # one target
just fuzz --seconds 900      # the budget a scheduled run wants
just fuzz-nightly            # the scheduled run: FUZZ_SECS (120 s) per target
just fuzz-regress            # replay the corpus and the seeds, no fuzzing
```

`fuzz-regress` is the one that is on the push path (`just fast`). It generates
nothing: it replays every input already in the corpus and every checked-in seed
through its target exactly once (libFuzzer `-runs=0`), which is the check that
the defects those inputs found stay fixed. 2.0 s warm for all eight targets,
92 s from cold (measured 2026-09-25; the cold cost is almost entirely the
leviculum-std ASan build). Without the nightly toolchain it prints one
`FUZZ_SKIPPED` line and exits 0, so it does not make nightly a push-path
dependency.

In that mode `-max_len` follows the corpus instead of the 8192 default: the
flag TRUNCATES an oversized corpus file rather than skipping it, so replaying
the 100 KB #263 reproducer at the default limit would have replayed the first
8192 bytes of it and called that a regression check.

`scripts/run-fuzz.sh` is what that recipe calls, and it is the only invocation
anyone needs to remember: it supplies the nightly toolchain, the glibc target,
the seed path and the libFuzzer budget. Exit 1 is a crash, exit 2 is a run that
could not happen (missing toolchain, or a `fuzz_targets/*.rs` that this
manifest does not register as a `[[bin]]`, which no run reaches).

`seeds/<target>/` holds a small hand-written seed corpus (committed) and is fed
in as read-only input. The working corpus and any crash input live OUTSIDE this
checkout, under `~/.local/state/leviculum-fuzz/{corpus,findings}/leviculum-core/<target>/`,
so coverage accumulates across runs instead of restarting from the seeds
(Codeberg #290). The in-tree `corpus/` and `artifacts/` dirs stay gitignored
for hand-driven `cargo fuzz` runs.

The raw invocation, for a one-off outside the runner:

```sh
cargo +nightly fuzz run <target> --fuzz-dir leviculum-core/fuzz \
    --target x86_64-unknown-linux-gnu \
    leviculum-core/fuzz/seeds/<target> -- -max_total_time=30 -max_len=8192
```

## Reproduce a specific input

```sh
cargo +nightly fuzz run <target> --target x86_64-unknown-linux-gnu <file>
```

## Known findings, and the seed each one left behind

Every defect these parsers have had keeps a named seed, so `just fuzz-regress`
replays it on every push. What each seed proves differs, and saying so is the
point — a seed that never crashed anything is a coverage anchor, not a
regression test, and calling it one would be the same false confidence #290 is
about.

| seed | defect | what it did before the fix |
|------|--------|----------------------------|
| `resource_advertisement_unpack/recursion_reproducer` | #263 | **Aborted this build.** ASan stack-overflow on a chain of fixarray-len-1 tags routed through `skip_msgpack_value`'s unknown-key path. Fixed by `MAX_SKIP_DEPTH` in `msgpack.rs`. |
| `resource_advertisement_unpack/bin32_len_wrap` | #267 | `81 a1 68 c6 ff ff ff ff` — fixmap(1) `{"h": bin32(len=u32::MAX)}`, straight into `read_msgpack_bin`. Panicked where `usize` is 32 bits (`thumbv7em-none-eabihf`), because `*pos + len` wrapped below `data.len()` and the guard passed. Fixed by `checked_add` in `msgpack::take`. |
| `resource_advertisement_unpack/ext32_len_wrap` | #267 | `81 a1 7a c9 ff ff ff ff` — the same length through the unknown-key skip path's ext32 arm, which advances by `1 + len` and so wraps to 0. |
| `hdlc_deframe/oversized_frame` | #271 | One frame whose unescaped payload is `DEFAULT_MAX_FRAME + 1`, then a well-formed `AB` frame: the discard AND the resynchronisation. |

Two caveats, because a green replay means less than it looks:

- The #267 seeds **cannot crash this build.** The wrap needs a 32-bit `usize`
  and these targets are built for `x86_64-unknown-linux-gnu`. Here they only
  drive the guard. The width-dependent claim is held by the unit regression
  `bin32_length_near_usize_max_is_rejected` (`leviculum-core/src/msgpack.rs`),
  which was verified red against pre-fix code for `i686-unknown-linux-musl`.
- The #271 seed **never crashed either.** The pre-fix failure was unbounded
  growth — a peer streaming bytes that never contain a FLAG until the process
  is out of memory — which no single corpus-sized input reproduces. It pins the
  cap and the resync behaviour instead.

The one of the three that a fuzz run would genuinely have caught in minutes is
#263, and it is also the only one whose seed aborts a pre-fix build.

## CI / nightly

A short run catches shallow crashes only; deep continuous fuzzing (hours per
target) belongs in a scheduled job, not in `just standard`. The push path
carries the two cheap halves instead:

- `just fuzz-selftest` — the fixture test for the runner
  (`scripts/test-run-fuzz.sh`), which injects a crashing target, an
  unregistered target, a missing cargo-fuzz, and a corpus replay that must go
  red when a target starts panicking on an input it holds.
- `just fuzz-regress` — the replay described above.

The scheduled run is `just fuzz-nightly` (`bash scripts/run-fuzz.sh
--nightly`): `FUZZ_SECS` seconds per target, 120 by default, exit 1 on a crash
or a per-input timeout, the input kept under the state dir, one `FUZZ_SUMMARY`
line to report.

Where the corpus lives is `LEVICULUM_FUZZ_CORPUS` and where crash inputs go is
`LEVICULUM_FUZZ_ARTIFACTS`; both default under `~/.local/state/leviculum-fuzz`.
Deliberately NOT the cargo target directory: on the nightly host that is a
shared build cache which `nightly-fresh-tree.sh` deletes wholesale
(`rm -rf "$TARGET"`) once it passes its hard size cap, and a corpus on a cache
eviction schedule is a corpus that silently restarts from the seeds.
