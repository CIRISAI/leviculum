# Fuzz harness for the leviculum-std parsers (Codeberg #108)

Coverage-guided fuzzing (cargo-fuzz / libFuzzer) for the leviculum-std
functions that parse UNTRUSTED bytes. Companion to `leviculum-core/fuzz/`
(Codeberg #23); a separate crate because the parser under test lives in
leviculum-std and pulling that into the core fuzz crate would drag the whole
daemon stack into a crate named for core.

A parser that panics, overflows (in debug), infinite-loops, or OOMs on
malformed input is a remote DoS, so every target must return `Err`/`None`
gracefully on any input.

## Requirements

- Rust **nightly**: `rustup toolchain install nightly`
- **cargo-fuzz**: `cargo install cargo-fuzz`
- Build/run against the **glibc** host target, NOT the workspace musl default
  (`.cargo/config.toml` sets musl; ASan wants glibc):
  `--target x86_64-unknown-linux-gnu`

This crate is detached from the repo workspace (its own `[workspace]` table)
and is excluded from `just standard`; regression tests for any crash it finds
live in the normal `leviculum-std` unit suite instead.

## Targets (ranked by exposure)

| target      | parser                                              | reachability |
|-------------|-----------------------------------------------------|--------------|
| `sam_parse` | `interfaces::i2p::sam::Message::parse` + `i2p_b64decode` + `Destination::from_{public,private}_base64` | every reply line from the I2P SAM bridge socket |

`Message::parse` splits a SAM reply line into a command/action verb and
`KEY=VALUE` options; a `DESTINATION=` value is then routed through the I2P
base64 decoder and the destination decoders (which read a big-endian
certificate length and slice the key blob). The base64 decoder does bit-level
shift/accumulate arithmetic on attacker-controlled length, so it is the most
arithmetic-heavy path and is fuzzed both via a `DESTINATION=` option and
directly on the whole input.

## Run it

```sh
just fuzz                # every target in both fuzz crates, 60 s each
just fuzz sam_parse      # this one
```

`scripts/run-fuzz.sh` is what that recipe calls; see
`leviculum-core/fuzz/README.md` for the exit-code contract. The working corpus
and any crash input live OUTSIDE this checkout, under
`~/.local/state/leviculum-fuzz/{corpus,findings}/leviculum-std/sam_parse/`, so
coverage accumulates across runs (Codeberg #290). `fuzz/seeds/sam_parse/` holds
the committed seed corpus and is fed in as read-only input.

The raw invocation, for a one-off outside the runner:

```sh
cargo +nightly fuzz run sam_parse --fuzz-dir leviculum-std/fuzz \
    --target x86_64-unknown-linux-gnu \
    leviculum-std/fuzz/seeds/sam_parse -- -max_total_time=30 -max_len=8192
```

Note the ASan build of leviculum-std is the slow one: 95 s cold against ~2 s
for a warm leviculum-core target (measured 2026-09-19).

## Reproduce a specific input

```sh
cargo +nightly fuzz run sam_parse --target x86_64-unknown-linux-gnu <file>
```

## CI / nightly

Same as the core crate's: no tier runs the fuzzing itself, the push path runs
`just fuzz-selftest` over the runner, and a scheduled run is
`bash scripts/run-fuzz.sh --seconds <budget>`.
