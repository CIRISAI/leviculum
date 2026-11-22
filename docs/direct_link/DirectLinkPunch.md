# DirectLinkPunch: a TLA+ model of the direct-link punch

`DirectLinkPunch.tla` models the leviculum (+ciris) direct-link upgrade as
the code does it at `d4ff59e` (diag/punch-telemetry, after the review of
72be0f9): sides A (initiator) and B (responder), each side's own
facilitator, the relay carrying the signalling, each NAT as a parameter, and
a stranger on the hard side's public IP. It was built from the code, not
from intent. Where the code leaves a behaviour undefined, the model's choice
is listed under **Gaps**, not made silently.

Files:

| file | what |
|---|---|
| `DirectLinkPunch.tla` | the spec |
| `Matrix.tla` | the spec plus an outcome probe (prints each terminal state's outcome) |
| `base.cfg.tmpl` | the config template; `run_matrix.py` fills one per scenario |
| `run_matrix.py` | runs TLC per NAT pair and scenario, collects outcomes and violations |
| `state_classes.py` | groups a TLC `-dump dot` graph into state classes (the table below) |
| `matrix.md`, `classes-*.md` | the results, generated |
| `frame-auth-note.md` | the HMAC-in-frames design note (later; not shipped) |

TLC: `tla2tools-1.7.4.jar`, run as `nice -n 19 ionice -c3 java -Xmx4g ...
tlc2.TLC -workers 2`.

## Names (agreed with the Constitution)

`linkPath(s)` is one of `none`, `relayed`, `upgrading`, `direct`, derived from
the session state:

| linkPath | when |
|---|---|
| `none` | the link is gone (`TearDown`, node `direct_link_link_removed`) |
| `relayed` | no session (`Idle`), a session that ended without a path (`Failed`), or a direct path that fell back |
| `upgrading` | a session is running (`Discovering` … `Committing`) |
| `direct` | `Connected`, and not fallen back |

The six transitions, which `LinkPathTransitions` checks as an action
property: none→relayed (link up), relayed→upgrading (proposal accepted),
upgrading→direct (heard∧acked on both sides: `Connected` needs this side's
`PunchOK` and the peer's COMPLETE, which it sends only on its own
`PunchOK`), upgrading→relayed (clean abort), direct→relayed (`FallBack`),
and any→none (`TearDown`).

The initial state is `RelayedLinkUp`. Its preconditions are not modelled,
because they are the host's: A and B may link (same owner, mutual contacts,
or a serve seat), and the relay carries by consent. The terminal states are
`DirectPathUp` (both `direct`) and `RelayedOnly` (both `relayed` after a
failure). `LinkClosed` (both `none`) is the third terminal, reached only
through `TearDown`.

## Model choices

- **M1, network delay:** a datagram is delivered (or filtered, or lost)
  within `Latency` ticks of global time. No side ticks past a datagram that
  has been in flight that long. A per-side bound (an earlier draft) let one
  side run out its window while the other's answers sat in flight, an
  unbounded one-way delay the code never meets.
- **M2, time:** the punch window is `W` ticks (PUNCH_WINDOW 10 s at 100 ms).
  The model keeps the code's proportions: escalation at `EscAt=2` and the
  birthday gate at `BdayAt=1`, against `W=6`. The code's ratios are
  1.5 s : 1 s : 10 s, so W/EscAt is about 6.7.
- **M3, phase timeouts:** a timeout fires only once nothing it waits for
  can still come: the peer has ended, or sits Idle with nothing on its way
  to it. This assumes a relay round trip far shorter than the code's 10 to
  12 s budgets.
- **M4, ports:** each public IP has ports `1..NP`. A random allocation, the
  spray's draws and a birthday socket's mapping are nondeterministic
  choices, so a birthday collision is a branch, not a probability. The
  scale is `NP=4`, `K=1` birthday socket and `S=2` sprayed ports; the
  model's safety does not depend on the counts. `SprayHits` (`hit` / `miss`)
  is an oracle that narrows the draws.
- **M5, datagrams:** the network is a set, so identical repeated punches
  merge. The order of distinct datagrams is free, and losses happen only
  with `NetLossy`.
- **M6, the relay:** one FIFO per direction, reliable, because the RNS
  Channel is reliable and ordered and the core relies on it. `RelayLossy`
  and `RelayDup` break that on purpose.

## Actions and the code they follow

| action | code |
|---|---|
| `Propose` | core `node/direct_link.rs` `propose_direct_link_with` → `Session::initiate` |
| `ProbeAnswer`, `ProbeFail` | std `direct_link.rs` `start_probe` (`probe`, `classify_nat`, `allocation_steps`, `advertise`), core `Session::probed_with_candidates` / `probe_failed` |
| `RecvRequest` | core `direct_link_signal`: policy, crossing (glare: `keep_ours` / `give_way`), busy, `accept_direct_request` (own facilitator, CC 5.4.6) |
| `RecvOther` | core `Session::signal` (ACCEPT, READY, COMPLETE, REJECT per phase) |
| `Tick` | std `punch_inner` tick arm (tokened punches, knocks, spray, escalation) and `punch_birthday` (gate, birthday sockets, refresh) |
| `Deliver` | std `punch_inner` recv arm (ack, heard, peer-reflexive, acked only if punched), `birthday_task` and the switch (only while the session socket is silent) |
| `StrangerGets` | another host's socket on the hard side's IP |
| `PunchOK`, `PunchFail` | std `punch_inner` result → core `Session::punched` (COMPLETE, or REJECT PUNCH_FAILED) |
| `Timeout` | core `Session::poll` |
| `TearDown` | core `direct_link_link_removed` |
| `FallBack` | core `direct_link_link_stale` / `release_direct_attachment` |

## Properties

| property | what it says |
|---|---|
| `PathHeardAndAcked` | a side settles on an address only if it both heard and was acked there, on its punching socket |
| `SamePath` | both direct, on ONE socket pair |
| `ForgedReflexiveNeverWrongDirect` | a direct path always joins A and B; a forged probe answer leads to `relayed`, never a wrong `direct` |
| `NoPathToStranger` | no side settles, even only into the commit, on a stranger's address (the review's hijack) |
| `NoLoneMove` | no side direct while the other has failed |
| `AbortLeavesLinkUnchanged` | after a clean abort both sides are `relayed` |
| `FacilitatorCarriesNothing` | no punch, knock or payload to or from a facilitator; the relay carries signals only |
| `FacilitatorKnowledge` | a facilitator learns only the mapping of a node that chose to ask it; a REQUEST naming a facilitator the responder does not use never makes the responder send it anything; no identifier links two probes (they carry a nonce, never the session), so where both sides chose one facilitator it can at most correlate them by timing |
| `StrangerKnowledge` | a stranger on a sprayed IP learns only that a knock arrived |
| `NoTokenToStranger` | no tokened frame reaches a stranger |
| `NoLearnedAddressPersisted` | CC 2.6.8: no observed mapping is written into a nodecode, a signed dial hint or any record. **By construction:** the code has no such write, and the model has no action that writes `persisted` |
| `CapsHeld` | spray ≤ S, birthday sockets ≤ K, predicted ≤ Q |
| `NotStuck` | until the attempt is over, some action is enabled |
| `EventuallyOver` (liveness, weak fairness) | every attempt ends |
| `LinkPathTransitions` (action) | linkPath moves only along the six transitions |

**Knowledge, per party:**

- **The facilitator:** the reflexive mapping of each node that probed it (`facSaw`); no session id (`facSessions` stays empty); and, if both sides chose the same one, two probes close in time.
- **A and B:** each other's mappings and candidates, only through the signalling on the encrypted link.
- **A stranger on a sprayed IP:** "a knock arrived" (`strangerSaw ⊆ {K}`).
- **The relay:** the signal types travelling by (`relaySaw`), and nothing it carries is a punch or a knock.

## The plants

| plant | puts back | the property that catches it | fixed code |
|---|---|---|---|
| `HijackPlant` | 72be0f9's spray: tokened punches to sprayed and predicted ports, and an ack from a sprayed port counted as ours | `NoTokenToStranger`, `StrangerKnowledge`, `NoPathToStranger` | hold |
| `NamedFacilitatorPlant` | the responder probes the `"f"` the REQUEST names | `FacilitatorKnowledge` (with A and B on different facilitators) | holds |

## Gaps (behaviour the code does not define; the model's choice)

- **G1, start skew:** the code bounds the skew between the two punch
  windows only through RESPONDER_START_DELAY (RTT/2, at most 1.5 s). The
  model bounds it by `MaxSkew` ticks. With a larger skew a pair the design
  handles can fail (scenario `skew-2`). This is follow-up 3 (synchronised
  start).
- **G2, the relay is assumed reliable and ordered:** with `RelayLossy`, a
  lost COMPLETE leaves one side `direct` and the other `relayed` (`NoLoneMove`
  violated). The code relies on the Channel never losing a signal while the
  link lives. If it did, the recovery is liveness: the lone side's direct
  interface hears nothing and falls back (fast liveness, 15 s).
- **G3, "the relay as facilitator":** CC says a responder with no own
  facilitator may use "the relay carrying the link". The code has no way to
  learn the relay's facilitator endpoint. It refuses (REJECT UNSUPPORTED)
  unless one is configured. The model's `"R"` is a configured one.
- **G4, teardown:** the model removes both sessions at once; in the code
  each side notices the link close on its own.
- **G5, mapping expiry:** during the punch every tick refreshes the
  mappings, so expiry mid-punch is not modelled. After the move, a mapping
  the keepalive does not refresh expires (`KeepaliveOK=FALSE`, `FallBack`).
  The keepalive check is follow-up 1.
- **G6, same NAT:** `SameNAT` gives each side the other's LAN candidate. The
  code includes IPv4 host candidates only when both report the same public
  IP; dual-stack is not modelled.
- **G7, the spray budget across attempts:** the model's `SprayBudget` is
  per attempt. The code's per-IP cooldown spans attempts, which the live
  re-proof showed refusing the 2nd and 3rd attempts.

RESULTS (filled from `matrix.md` and `classes-*.md` when the runs end).
