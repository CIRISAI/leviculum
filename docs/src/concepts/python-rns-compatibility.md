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

Semantic compatibility is decided field by field: what a peer
*decides* from a value we generate is part of the contract, even when
the byte layout is right. The audit method and testing rule for that
are in [Wire Field Semantics](wire-field-semantics.md).

It cuts both ways. The reference's **accept set** is as much of the
contract as its output set, and it is wider — Python's decoder takes
forms Python's encoder never emits. A mesh has more than two
implementations in it (reticulum-kt, microReticulum, hand-rolled
senders), and any of them may pick a legal encoding Python happens not
to use. Testing only against `rnsd` cannot see this: it never produces
the form we refuse. Being stricter than the reference on the read
path is a compatibility defect of the same standing as emitting a
wrong value, and it fails silently — see
[Wire Field Semantics](wire-field-semantics.md#the-mirror-question-what-do-we-refuse-to-read).

## Level 2: drop-in daemon and tooling

`lnsd` shares two interfaces with Python's `rnsd`:

- **The shared-instance IPC socket.** A running daemon exposes a local
  control/data channel that client tools connect to. Leviculum speaks
  the same protocol, so Python's `rnstatus`, `rnpath`, `rnprobe`, and
  `rncp` drive a running `lnsd` without modification, and the
  Leviculum tools `lnstest` and `lncp` drive a running `rnsd` just the
  same. The RPC control channel that backs `rnstatus`/`rnpath`/
  `rnprobe` is implemented in `leviculum-std/src/rpc/` (it speaks
  Python's `multiprocessing.connection` framing with pickle payloads,
  see `rpc/connection.rs` and `rpc/pickle.rs`).
- **The config-file format.** `lnsd` parses the same INI-style config
  that `rnsd` uses (`leviculum-std/src/config.rs`,
  `leviculum-std/src/ini_config.rs`). Even keys Leviculum does not act
  on are parsed for compatibility — for example `shared_instance_type`
  and `shared_instance_socket` are read and honoured per RNS 1.3.x
  semantics so an existing `rnsd` config works unchanged
  (`leviculum-std/src/config.rs:47-53`).

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

A deviation that is not written down is indistinguishable from a bug.
Each one is pinned here with the reference line it departs from, so
the next reader can check the claim instead of re-deriving it.

### Pinned deviation: ingress-control default on dial-out links

The reference gives every interface `ingress_control = True`
(`Interface.py:112`), overridable per interface by the config key of
the same name (`Reticulum.py:768-769`, applied at `Reticulum.py:910`).
Leviculum defaults it **off** on dial-out point-to-point links —
`TCPClientInterface`, `BackboneClientInterface`, `UDPInterface`, and an
`I2PInterface` without `connectable` — and leaves it **on** everywhere
else, including every listener
(`ingress_control_default_for_type`, `leviculum-std/src/config.rs:640`).

Against the rule: the flag decides only whether *we* hold incoming
announces, so no wire byte and no behaviour a peer observes changes
(1 and 2). It gains us the announces the limiter would otherwise hold
silently on a link carrying one known peer's startup burst — the
mechanism behind the Codeberg #44 flake, on our receive side (3).
An operator who wants the reference behaviour writes
`ingress_control = yes` on the interface.

The default is a *role* distinction, not a medium one: an interface
that accepts connections from arbitrary unknown peers is exactly the
announce-storm surface the limiter exists for, so a listener keeps the
reference default. What a listener resolves is inherited by every
connection it accepts, as in the reference
(`TCPInterface.py:582`, `I2PInterface.py:951`,
`BackboneInterface.py:409`). Shared-instance IPC clients are never
ingress-limited on either stack — the reference hard-wires
`should_ingress_limit` to `False` for them
(`LocalInterface.py:137-138`) — so that is not a deviation.
