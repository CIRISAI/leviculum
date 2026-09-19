# Fuzz harness for the wire-format parsers (Codeberg #23)

Coverage-guided fuzzing (cargo-fuzz / libFuzzer) for the functions that parse
UNTRUSTED bytes off the wire. A parser that panics, overflows (in debug),
infinite-loops, or OOMs on malformed input is a remote DoS, so every target
must return `Err`/`None` gracefully on any input.

## Requirements

- Rust **nightly** (libFuzzer needs `-Z` sanitizer flags): `rustup toolchain install nightly`
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

The msgpack readers in `resource/msgpack.rs` (including the recursive
`skip_msgpack_value`) are exercised transitively by
`resource_advertisement_unpack` and `discovery_announce`.

## Run them

```sh
just fuzz                    # every target in both fuzz crates, 60 s each
just fuzz hdlc_deframe       # one target
just fuzz --seconds 900      # the budget a scheduled run wants
```

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

## Known finding

`resource_advertisement_unpack` originally aborted with an ASan
**stack-overflow** on a deeply nested msgpack container (a chain of
fixarray-len-1 tags routed through `skip_msgpack_value`'s unknown-key skip
path). Fixed by a nesting-depth cap (`MAX_SKIP_DEPTH`) in
`resource/msgpack.rs`; the reproducer is kept at
`seeds/resource_advertisement_unpack/recursion_reproducer` and as unit
regressions (`skip_rejects_deeply_nested_container`,
`test_advertisement_unpack_rejects_deeply_nested_value`).

## CI / nightly

A short run catches shallow crashes only; deep continuous fuzzing (hours per
target) belongs in a scheduled job, not in `just standard`, which is why no
tier runs this. What the push path does carry is `just fuzz-selftest` — the
fixture test for the runner (`scripts/test-run-fuzz.sh`), which injects a
crashing target, an unregistered target and a missing cargo-fuzz and asserts
the runner's verdict on each.

A scheduled run is `bash scripts/run-fuzz.sh --seconds <budget>`: exit 1 on a
crash, the input kept under the state dir, one `FUZZ_SUMMARY` line to report.
