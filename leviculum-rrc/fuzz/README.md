# Fuzz harness for the RRC envelope decoder

Coverage-guided fuzzing (cargo-fuzz / libFuzzer) for `leviculum_rrc`'s
decoder. Companion to the other fuzz crates; a separate crate because the
decoder lives in its own crate.

An RRC hub's link packets go straight into `Session::receive`
(`loquax/src/rooms_link.rs`). A decoder that panics on crafted input is a
remote crash of Loquax, from any hub the reader adds.

## Requirements

As `leviculum-micron/fuzz/README.md`: Rust nightly, cargo-fuzz, the glibc
host target. `bash scripts/run-fuzz.sh rrc_envelope` runs it.

## Target

| target         | decoder                                                | reachability |
|----------------|--------------------------------------------------------|--------------|
| `rrc_envelope` | `Value::decode`, `Envelope::decode`, `Session::receive` | every packet a hub sends on a room link |

Besides "never panics", every decoded value and envelope is re-encoded and
decoded again, and the second encoding must equal the first.

## Seeds

`seeds/rrc_envelope/` holds the 21 recorded envelopes of
`../tests/vectors/envelopes.json`, one file per vector name.
