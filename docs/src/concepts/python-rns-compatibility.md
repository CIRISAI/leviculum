# Python-RNS Compatibility

Leviculum is built to live in the same mesh as Python Reticulum
(`rnsd`) and to be a drop-in replacement for the daemon and its
tooling. Compatibility is pursued at two distinct levels, and one
thing that is *not* pursued at all.

## Level 1: wire and semantic compatibility

The protocol the two stacks speak must be identical on the air. The
exact bytes of identities, destinations, announces, packets, and links
are fixed by the
[Reticulum specification](../appendix/reticulum-specification.md);
the message format layered on top is fixed by the
[LXMF specification](../appendix/lxmf-specification.md). Leviculum
implements those formats so that a Python peer cannot tell a Leviculum
neighbour from another Python node.

Semantic compatibility goes beyond byte layout: behaviours a Python
peer *expects* from a neighbour — answering path requests, rebroadcast
decisions, link lifecycle, ratchet handling — must still be delivered.
Where the precise expected behaviour matters and is subtle, it is
captured as a source-of-truth reference; the broadcast path is
documented in
[Broadcast: Python-RNS parity reference](../architecture-broadcast-python-parity.md),
which records what Python does for every broadcast mechanism so the
Rust core can match it.

## Level 2: drop-in daemon and tooling

`lnsd` shares two interfaces with Python's `rnsd`:

- **The shared-instance IPC socket.** A running daemon exposes a local
  control/data channel that client tools connect to. Leviculum speaks
  the same protocol, so Python's `rnstatus`, `rnpath`, `rnprobe`, and
  `rncp` drive a running `lnsd` without modification, and the
  Leviculum tools `lnstest` and `lncp` drive a running `rnsd` just the
  same. The RPC control channel that backs `rnstatus`/`rnpath`/
  `rnprobe` is implemented in `reticulum-std/src/rpc/` (it speaks
  Python's `multiprocessing.connection` framing with pickle payloads,
  see `rpc/connection.rs` and `rpc/pickle.rs`).
- **The config-file format.** `lnsd` parses the same INI-style config
  that `rnsd` uses (`reticulum-std/src/config.rs`,
  `reticulum-std/src/ini_config.rs`). Even keys Leviculum does not act
  on are parsed for compatibility — for example `shared_instance_type`
  and `shared_instance_socket` are read and honoured per RNS 1.3.x
  semantics so an existing `rnsd` config works unchanged
  (`reticulum-std/src/config.rs:47-53`).

This drop-in property is a deliberate design goal, not an accident.
It is also what makes honest A/B testing possible: the test harness
points the *same* client binary (e.g. `lnstest selftest`) at either
daemon, never a parallel per-stack driver. A parallel driver would
smuggle configuration differences into what claims to be a stack
comparison.

## What is explicitly *not* a goal: internal parity

Compatibility is not the same as parity.

- **Compatibility** — our stacks interoperate at the wire and semantic
  level.
- **Parity** — our internals mirror Python's (same algorithms, same
  retry timings, same state-machine structure).

Leviculum needs the first, not the second. The historical parity
documents under `docs/src/architecture-*-python-parity.md` are
reference material for *getting behaviour right*, not commitments to
maintain identical internals.

## The deviation rule

A deviation from Python-RNS's implementation is acceptable if and only
if **all three** of the following hold:

1. Wire-format compatibility is preserved.
2. Semantic compatibility is preserved (behaviours Python peers expect
   from a neighbour are still delivered).
3. The deviation measurably improves robustness or mesh delivery.

"Because Python does it differently" is not, on its own, an objection;
only "this breaks wire or semantic compatibility" is. The
[interface-isolation](interface-isolation.md) design — interfaces
applying their own jitter, CSMA, and airtime budgeting — is a
deliberate deviation that satisfies this rule.
