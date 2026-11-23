# DirectLinkPunch: a TLA+ model of the direct-link punch

`DirectLinkPunch.tla` models the leviculum (+ciris) direct-link upgrade as
the code does it at `2cf992a` (diag/punch-telemetry, after the third review
of 4ab3b14: the trust gate, fair shares, the own-evidence fallback): sides A (initiator) and B (responder), each side's own
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
- **M7, the port pool:** at `NP=4` a random NAT can run out of ports. A
  datagram toward a new destination that no free port can map is dropped
  (no mapping, nothing sent), and the birthday phase opens nothing if the
  pool cannot map its sockets. The code's 64k pool never runs out; without
  this the model deadlocked (the `NotStuck` counterexample of the d4ff59e
  run on EDMrand↔EDMrand was this, not the code).
- **M8, filtering per mapping:** RFC 4787 section 5 filters at the mapping
  (external address) a datagram arrives at, against the destinations that
  mapping was made toward. The d4ff59e model judged it against every
  mapping of the same internal socket, which on an EDM NAT let a port made
  toward the facilitator admit the peer. Every `SamePath` counterexample on
  an EDM–ADF pair in that run was this artifact; with the fix those pairs
  reach only `RelayedOnly` (see the results).
- **M9, the trust gate and the socket counts:** `Escalates(s)` is core
  `direct_link_escalation_allowed`: the host's decision (`HostTrustA/B`)
  when given; otherwise never under `accept_all`; yes on a link the side
  initiated (`BInitiatedLink`); `identified_only`: no (fourth review B: an
  identity costs nothing; `PeerIdentified` no longer matters); `ask_host`:
  yes as the responder; `reject`: no. A runs `ask_host`. The birthday count
  is `K` (MAX_BIRTHDAY_SOCKETS, 256) on either evidence kind (fourth review
  G removed the 32-socket fallback split), opened one socket a tick while
  the side hears nothing (the code's ramp: 64 every 500 ms); the matrix runs
  `K=1`, the `sockets-ramp-*` rows `K=2`.
- **M10, the reply leaves through the replier's own mapping (fourth
  review):** an ack goes out through the replying socket's mapping toward
  the punch's source (made at the arrival port if there is none), and
  carries `to`, the address the punch was sent to (the code's target index
  in the sequence number). Earlier runs shortcut the ack to come from the
  address the punch arrived at, which no NAT does, and which produced acks
  on EDM pairs that the code never sees.
- **M11, settling is immediate:** a side that has proved a pair does not
  tick again before settling (`CanSettle`), as std `punch_inner` returns
  the moment it sees it. With nomination a responder waits on the
  initiator, so letting the initiator idle past its proof put the
  responder's window out first: a model artifact, not the code.
- **M12, nomination (`Nominate`, fourth review H):** the initiator, once it
  has a pair heard and acked (an ack counts only if `to` is the address it
  came from), sends a nomination (`N`) on it every tick until one is acked
  (`AN`); COMPLETE waits for that. The responder never settles on its own:
  it settles on the address an `N` arrived from, on the socket it arrived
  on. `Nominate=FALSE` is the code before it (each side its own first
  pair), kept for the `-before-nomination` rows.

- **M13, timers during relay delay (`RelaySlow`, strategy review):** a
  Committing wait may run out while the peer's COMPLETE is still on the
  relay. The side that times out sends REJECT (core `Session::poll`), and a
  side that already moved on the late COMPLETE honours an abort for its own
  session (core `direct_link_peer_aborted`): `NoLoneMove` can break while the
  abort crosses the relay, `NoLoneMoveSettled` says it never outlasts it.

## Timeline of an escalated attempt (the code's numbers)

| when (from the responder's READY; the initiator starts on its arrival) | what happens |
|---|---|
| before: the proposal | each side probes ITS OWN facilitator: one round trip when it answers, at most 6 s (3 attempts of 2 s); the second observer and the allocation probes add at most 1.5 s and are skipped once the main probe took over 3 s |
| 0 to 1.5 s | the responder starts punching RTT/2 after its READY (at most 1.5 s), so both ends start together |
| +1.0 s (two-observer evidence) or +1.5 s (own evidence) | the random side, if none of its punches is acked yet: the first 64 birthday sockets |
| +1.5 s | if nothing is heard: knocks to predicted ports, and the spray (320 knocks a second); the random side's second tranche (128 sockets) |
| +2.0 s, +2.5 s | third and fourth tranches (192, then all 256), while nothing is acked |
| about +7.0 s | the spray's 1,750 knocks are spent |
| +13 s | the escalated window ends (a plain punch: +10 s, as rns-rs) |
| +15 s | the nomination grace ends: the responder stops waiting for a nomination, the settled initiator stops nominating (plain with nomination: +12 s) |
| +18 s after the session began punching | the core's punch and commit waits: no verdict means `DirectLinkFailed` (plain: 12 s, 14 s with nomination; an older peer without `"n"` sees exactly the 10 s window and 12 s waits it always did) |
| after both COMPLETEs | the link moves, one relay trip later |

## Actions and the code they follow

| action | code |
|---|---|
| `Propose` | core `node/direct_link.rs` `propose_direct_link_with` → `Session::initiate` |
| `ProbeAnswer`, `ProbeFail` | std `direct_link.rs` `start_probe` (`probe`, `classify_nat`, `allocation_steps`, `advertise`), core `Session::probed_with_candidates` / `probe_failed` |
| `RecvRequest` | core `direct_link_signal`: policy, crossing (glare: `keep_ours` / `give_way`), busy, `accept_direct_request` (own facilitator, CC 5.4.6) |
| `RecvOther` | core `Session::signal` (ACCEPT, READY, COMPLETE, REJECT per phase) |
| `Tick` | std `punch_inner` tick arm (tokened punches, each naming its target, knocks, spray, escalation) and `punch_birthday` (gate: BIRTHDAY_AFTER on two-observer evidence, ESCALATE_AFTER on own; the ramp, tranche by tranche while silent, up to 256; the pair's allowance asked before anything binds), all behind core `direct_link_escalation_allowed` (`Escalates`) |
| `NomTick` | std `punch_inner`, `nominating`: the settled initiator's nomination every tick until acked |
| `Deliver` | std `punch_inner` recv arm (ack, heard, peer-reflexive, acked only if punched and, under nomination, only for a punch to that address; a responder settles on a nomination, an initiator on its ack), `birthday_task` and the switch (only while the session socket is silent) |
| `StrangerGets` | another host's socket on the hard side's IP |
| `PunchOK`, `PunchFail` | std `punch_inner` result → core `Session::punched` (COMPLETE, or REJECT PUNCH_FAILED); under nomination `PunchOK` starts the initiator nominating, and the responder never takes it |
| `Timeout` | core `Session::poll` |
| `TearDown` | core `direct_link_link_removed` |
| `FallBack` | core `direct_link_link_stale` / `release_direct_attachment` |

## Properties

| property | what it says |
|---|---|
| `PathHeardAndAcked` | a side settles on an address only if it both heard and was acked there, on its punching socket (under nomination, the responder: heard there, the nomination's source, which the initiator proved both ways) |
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
| `EscalationNeedsTrust` | no knock, spray or birthday socket from a side the trust gate does not let escalate |
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

- **G8, path choice is per side (found by the 2cf992a run, `samenat-hairpin`;
  FIXED by nomination in the fourth review, see the results):**
  each side settles on the FIRST address both heard and acked (std
  `direct_link.rs:1313`), independently. With two live candidates (same NAT
  with hairpin: LAN and hairpinned public; on EDM pairs, two mappings) A can
  settle on one pair and B on another. The direct interface accepts only its
  own path peer (`interfaces/direct_udp.rs:220`), so each side drops the
  other's frames and both fall back after the liveness timeout: a wasted
  upgrade, not a wrong one. Fix candidates: the initiator nominates (COMPLETE
  carries its chosen pair, the responder adopts it, as ICE does), or the
  direct interface accepts any source that proved the session token.
- **G9, EDM on both sides with non-EIF filtering is structurally relayed
  only.** A tokened punch goes only to an advertised address or a source
  that already sent one (review item 1); knocks are token-free and earn no
  reply. When both NATs are endpoint-dependent and filter, no tokened frame
  ever reaches the address the peer's NAT allocated toward us, so no pair is
  ever heard and acked. Exhaustive at this scale, not a probability. The
  exploration constant `MacFrames` (frame-auth-note.md's MAC'd frames sent to
  predicted and sprayed ports too, no forgeable reply) opens five of the
  eight such pairs; see the `mac-*` rows.
- **G10, an EIM NAT that allocates new sockets at random** would show random
  own steps and, with one observer, take the fallback ("b") though its
  mapping is endpoint-independent. The model's EIM allocates in order. The
  live arms saw no such NAT (every EIM runner: evidence none).

## Results (2cf992a, `matrix.md`)

TLC 1.7.4, 103 scenarios, `-continue`; two runs reached the 420 s cap
(`pair-EDMrand-APDF--EDMrand-EIF`, `hard-latency`) and are PARTIAL: what they
reached is real, "no violation" covers the states explored. The outcome class
is over every reachable terminal state (the draws are nondeterministic, so
"direct or relayed" means some draws close and some do not).

| A \ B | EIM-EIF | EIM-ADF | EIM-APDF | EDMseq-EIF | EDMseq-ADF | EDMseq-APDF | EDMrand-EIF | EDMrand-ADF | EDMrand-APDF |
|---|---|---|---|---|---|---|---|---|---|
| EIM-EIF | direct | direct | direct | direct | direct | direct | direct | direct | direct |
| EIM-ADF | direct | direct | direct | direct | direct | direct | direct | direct | direct |
| EIM-APDF | direct | direct | direct | direct | direct | direct | direct or relayed | direct or relayed | direct or relayed |
| EDMseq-APDF | direct | direct | direct | direct | relayed only | relayed only | direct | relayed only | relayed only |
| EDMrand-APDF | direct | direct | direct or relayed | direct | relayed only | relayed only | direct [capped] | relayed only | relayed only |

**Invariant violations and what they are in the code:**

| scenario | property | cause | code |
|---|---|---|---|
| `samenat-hairpin` | `SamePath` | G8: A settles on B's LAN candidate, B on A's hairpinned public mapping; each drops the other's frames | std `direct_link.rs:1313` (first heard∧acked, per side), `interfaces/direct_udp.rs:220` (only the path peer is accepted) |
| `mac-EDMseq-APDF--EDMrand-ADF` (exploration) | `SamePath` | G8 again, on two EDM mappings both heard and acked | same lines |
| `easy-relay-lossy` | `NoLoneMove` | G2: a lost COMPLETE; the side that got the peer's COMPLETE moves, the other times out of Committing | core `direct_link/session.rs:571-578` (Connected on the peer's COMPLETE), `:599-601` (Committing → Timeout); recovery `interfaces/direct_udp.rs:283` (silence → path down) |
| `plant-hijack` | `StrangerKnowledge` (and liveness) | the plant: tokened spray and acks from sprayed ports | fixed by std `direct_link.rs:1207,1240` (token-free `knock_frame`) and `:1290` (an ack counts only from a punched address) |
| `plant-named-facilitator` | `FacilitatorKnowledge` | the plant: the responder probes the REQUEST's facilitator | fixed by core `node/direct_link.rs:1115-1143` (`accept_direct_request` probes its own) |

None of `NoLearnedAddressPersisted` (by construction), `FacilitatorCarriesNothing`,
`NoTokenToStranger`, `NoPathToStranger`, `ForgedReflexiveNeverWrongDirect`,
`AbortLeavesLinkUnchanged`, `EscalationNeedsTrust`, `CapsHeld` or `NotStuck`
is violated anywhere outside the plants; `EventuallyOver` holds outside
`plant-hijack`.

The d4ff59e run's other counterexamples were the model's: the `SamePath`
rows on EDM–ADF pairs (M8) and `NotStuck` on EDMrand↔EDMrand (M7).

**The trust gate (no violation; liveness consequences):**

- `gate-b-initiated-link` reaches **no** `DirectPathUp`: on a link B (the
  random side) opened, A proposes under ask_host as the session initiator,
  so the default rule says no (core `node/direct_link.rs:480-482`: link
  initiator yes, else ask_host only as responder). A cannot spray, B's
  birthday sockets never get in. `gate-b-initiated-link-a-allowed` (the host
  says yes) restores the hard pair's outcomes. A node that proposes chose
  its peer as much as one that opened the link: candidate change, `Role::
  Initiator` counts as trusted under ask_host.
- `gate-host-deny-a` and `gate-host-deny-both`: relayed only (by design).
  `gate-host-deny-b`, `hard-accept-all`, `gate-identified-only-anonymous`:
  fewer closing draws (B's birthday sockets off). `gate-identified-only` and
  `gate-accept-all-host-allow`: the same as ask_host.

**Fallback sockets:** `sockets-two-observer-K2` and `sockets-fallback-KF2`
close in every draw; `sockets-fallback-KF1` (fewer sockets on own evidence)
is the only one of the three with relayed outcomes. The live arms say the
same at full scale: fallback at 32 sockets closed 3/12, two-observer at 128
closed 9/12 (the odds 1 − exp(−K·S/R), S = 960, R ≈ 64.5k: 38% and 85%).

`classes-*.md` are from the d4ff59e model and were not regenerated.

