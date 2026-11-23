# The punch strategy: choosing the technique from evidence

This is a **formalization of established practice**:

- the IETF's NAT behaviour requirements and traversal techniques: RFC 4787,
  RFC 5128, RFC 5780, RFC 6887;
- ICE's roles and nomination (RFC 8445);
- what shipping systems do (SURVEY.md);
- the one prior pair → technique matrix, NATCracker (Roverso et al., ICCCN
  2009).

Our additions are few and marked **ours**. REFERENCES.md has every
source, and the traceability table classes each row and property as one of:

- **implements**: implements the source as written;
- **formalizes**: formalizes practice the source describes;
- **ours**: no source found.

`DirectLinkPunch.tla` models HOW the punch works: sockets, mappings,
filters, tokens, the commit. This document and `Strategy.tla` cover WHICH
technique runs, chosen from the evidence (EVIDENCE.md), how both ends agree
on it and on the pair, and how an attempt's outcome becomes the next
attempt's evidence.

Eric's direction is the specification:

- Use the technique the situation calls for. Spamming techniques is not
  effective.
- Never dismiss evidence that arrives through secondary channels:
  durations, partial results, the peer's verdict.
- Rule out halting and race states.

A change to the punch changes `Row` in `Strategy.tla` first
(CONFORMANCE.md).

Vocabulary is RFC 4787's:

- Mapping: EIM / ADM / APDM, §4.1. Our `EDM` is ADM or APDM. `EDMseq` and
  `EDMrand` add the port assignment, §4.2.1.
- Filtering: EIF / ADF / APDF, §5.
- Candidates are ICE's (RFC 8445 §5.1.1).
- NATCracker's names are mapped onto these in `natcracker.md`.

## 1. Who decides

**The initiator, as ICE's controlling agent**, decides and nominates. The
responder is the controlled agent, which we call the follower. This
implements RFC 8445 §6.1.1 and §8.1.1, regular nomination in-band on the
checked pair.

Epoch 1 costs no extra round trip. It is `Row` over the two profiles plus
the history the REQUEST and READY carry. Both ends hold exactly that set
once READY has been sent and received, so both compute epoch 1 themselves:
the responder when it sends READY, the initiator when it receives it.
`SamePlan` checks that they agree (**ours**).

After epoch 1, only the controller re-plans, in the step a fact arrives. It
sends `PLAN(epoch, technique, why)`. The follower reports its own partials
and its verdict in `OBS`.

Why not have both ends re-plan from shared evidence? The decisive
secondary evidence is asymmetric. Only the side that heard without being
acked knows it, and only the side that saw the peer's port move knows that.
The two ends learn these at different moments. The model showed the
divergence before the two-sided stop (section 5) was added; see section 10.

## 2. The techniques, as DirectLinkPunch configurations

Our caps follow HyperDHT's: **256 birthday sockets and 1,750 knocks** (Eric's decision). The
table decides how much of that a situation needs.

| technique | what runs | source | DirectLinkPunch configuration | holds | cost bound per attempt |
|---|---|---|---|---|---|
| `lan` | tokened punches to host candidates (same LAN), or carrier-side candidates (same CGN), only | RFC 8445 §5.1.1.1; RFC 5128 §3.3.2 | `targets = {Lan(o)}` | nothing | 10 pps x candidates (≤ 4) x stop |
| `v6` | punches to the peer's global v6 address only | RFC 8445; RFC 8305; RFC 6092 REC-11/14 | not in DLP (gap G6) | nothing | 10 pps x stop |
| `hair` | punches to the reflexive address through the shared NAT | RFC 4787 §6 REQ-9; RFC 5128 §3.3.3 | `SameNAT`, Hairpin | nothing | 10 pps x stop |
| `rev` | **connection reversal**: the NATed side sends to the reachable side; no simultaneous punch | RFC 5128 §3.2; Nebula punch_back; BEP 55 | one side EIM+EIF | nothing | 10 pps x stop, one side |
| `plain` | tokened punches to the reflexive address, plus proven peer-reflexive sources | RFC 5128 §3.3; RFC 8445 §7.2.5.3.1 | `HostTrust = deny` | nothing | 10 pps x (1 + ≤ 4) x stop |
| `mapX` | X maps its SESSION socket (PCP, then NAT-PMP, then UPnP-IGD), advertises the mapping in PLAN, then plain; plus the static local-port guess (Tailscale, iroh) as one extra candidate | RFC 6887 §11; RFC 6886; RFC 6970 | X becomes EIM+EIF | the router lease (deleted on release) | ≤ 3 router transactions + plain |
| `predX` | plain, plus knocks from the other side to X's predicted ports (≤ 16) | RFC 5128 §3.5; draft-takeda §6.1; NATCracker PRC | DLP `held`/`knocking` from tick 0 | the pair's budget | 16 knocks/tick x stop; 1 escalation |
| `predAB` | both sequential: **authenticated** punches to each other's predicted ports | NATCracker §5.8 interleaved | DLP `MacFrames` | the pair's budget | 2 x 16 MAC'd frames/tick x stop; 1 escalation |
| `bdayX` | plain; X opens a TRANCHE of birthday sockets toward the other side, each refreshed so its mapping lives (RFC 4787 §4.3), first sends at low TTL (HyperDHT TTL 5; Guha 2005 §2.2); the other side sprays knocks on X's IP, inside a known port block when there is one | Tailscale blog; HyperDHT; Keller 2022 | DLP `bday`/`spray` from tick 0 | the tranche's sockets and permits on X; the spray budget on the other | tranche x 2 pps + ≤ 1,750 knocks; 1 escalation |

**The birthday tranches** are 64, then 128, then 256 sockets, one per
attempt, within the pair's 3 escalations (DCUtR's 3 attempts).

- A tranche never takes more than half of a CGN subscriber's port quota.
  RFC 6888 REQ-4 makes the quota configurable, and Richter 2016 saw as few
  as 512 ports per subscriber. When P13 (EVIDENCE.md) shows the quota, the
  tranche is capped at half of it.
- Opening 256 sockets at once would take half of a 512-port quota. That is
  why the ramp is never a flat 256.
- The ramp is **ours**.

**Candidate techniques, not in the table:**

- **R5b PCP PEER** (RFC 6887 §12) learns the external port toward this
  peer. That makes an EDM side exactly predictable, but no surveyed system
  ships it (SURVEY.md §3b.1). Precondition: a PCP server answers, it is the
  outermost NAT (not `CgnatDetected`), and it supports PEER.
- **R6c** prediction on a sequential side against a random side with
  non-strict filtering. See `natcracker.md` and open decision D10.

## 3. The decision table

The table is a total, deterministic decision list over the controller's
evidence. Rows are tried in order. A technique that has failed (both
verdicts in, section 5) is skipped while that failure is valid (history H).
A birthday is the exception: it may run again at its next tranche.

`Row` in `Strategy.tla` is this table, word for word. `Cheapest` computes
the same answer from the physics: each technique's sufficiency
precondition (`Pre`, which captures reachability), the **likelihood floor**
(`Likely`), and the cost order. TLC checks that the two agree on every view
either side reaches (`PlanIsCheapest`).

**Reachability is not likelihood.** TLC's nondeterministic port draws show
only that an outcome is reachable. The column "expected" gives the
expected success from the cited arithmetic, as a class:

| class | expected success |
|---|---|
| certain | ≥ 95% |
| high | 50–95% |
| low | 1–50% |
| negligible | < 1% |

**The floor: no plan uses a negligible row, however reachable**
(`NoNegligibleRow`). Example: with MAC'd frames, the mechanism model's NP=4
"opens" EDMrand against EDMrand. In 16 bits that is the squared model,
1−exp(−N·M/65536²) ≈ 0.003% at 256 x 1,750 (SURVEY.md §4).

"Auth" below means the HMAC wire is negotiated (section 9). The class
column refers to REFERENCES.md §2.

| row | situation (evidence, EVIDENCE.md) | technique | expected (class: source and assumptions) | class | stop condition | moves mid-attempt |
|---|---|---|---|---|---|---|
| **R1** same network | the same public IP (P8), with LAN or carrier-side candidates (P8) | `lan` only, **never the hairpin** | certain on one LAN (a host path, RFC 8445 §5.1.2 host first); carrier side: high, assumed (unmeasured) | formalizes; carrier candidates are **ours** | silent past `Stop` | silent: R2, else R3 |
| **R2** both global v6 | both have a global v6 address (P6) | `v6` only | high, assumed: a stateful v6 firewall passes a simultaneous punch (RFC 6092 REC-11/14); unmeasured | formalizes (RFC 8305 preference); exclusivity is **ours** | silent past `Stop`, or ICMP no-route at once (S8) | to R3/R3b/R4 |
| **R3** one public IP, no usable candidates | the same public IP, no candidates (or R1 failed), both reflexive | `hair` | low: hairpin UDP on 24% of NATs (Ford 2005); a CGN MUST hairpin (RFC 6888 REQ-9): high there | formalizes | silent past `Stop` | to R5 |
| **R3b** one side reachable | one side known EIM with EIF (P1 + P3), or no NAT | `rev` | certain (RFC 5128 §3.2) | implements | silent past `Stop` | to R4 |
| **R4** apart, the classes allow plain | different public IPs, both reflexive, and NOT (both EDM and neither may filter openly) and NOT (a closed EDM facing a strict filter) | `plain` | high: UDP punching 82% (Ford 2005); 70% ± 7.1% at scale (Trautwein 2025/26); EIM pairs 100% in our live runs | formalizes; the open-EDM race is **ours** | silent past `Stop` | heard-not-acked (S4): this side is EDM, the peer strict: R6/R7/R8 at once. Same IP, other port (S7): reclassify; both EDM: R8 at once |
| **R5** a router maps | P7 on either side, not `CgnatDetected` | `mapX` (A first) | high, assumed (RFC 6886/6887); `Mapped` is not proof | formalizes | refused, or plain silent past `Stop` | the other side's map, or R6/R7 |
| **R6** one sequential EDM | X EDM-sequential (P1 + P1b), the other EIM or unknown, the knocker's gate (P11), budget left | `predX` | high: 76–78% for progressing allocators (Keller 2022, mobile); assumes the step holds between measurement and punch | formalizes | silent past `Stop` after the knocks begin | to R6b/R7/R8 |
| **R6b** both sequential | both EDM-sequential, **Auth**, not both filtering strictly (P3), both gates, budget | `predAB` | low: NATCracker §5.9.3 (PRC-PRC success "drops significantly"); DirectLinkPunch reaches it only with MAC'd frames and not both-APDF | formalizes NATCracker §5.8; the Auth precondition is from DLP | silent past `Stop` | to R8 |
| **R7** one random EDM | X EDM-random (P1); or, on own evidence, random steps on exactly ONE side, class unknown, and plain failed; the other EIM or unknown; both gates; budget | `bdayX` at tranche k | tranche 1 (64 sockets x 1,750): 82%; 2 (128): 97%; 3 (256): 99.9% (over 64,512 ports, uniform). Inside a known port block (S9, S10, P12): ~100% (SURVEY.md §4). Assumes the mappings live through the window (refresh; S17 lifetime) | formalizes; the tranches are **ours** | the knocks spent and one refresh later, or a hit | heard on the session socket: stay. A miss: the next attempt runs tranche k+1 |
| **R8** nothing above the floor | otherwise: e.g. both random (negligible: 0.003%); both EDM with filtering known closed; no v6, mapping or candidates; or every row failed or gated | **none**: "no direct path" at once, the link unchanged, handed to #1024 (multi-reflector relaying; RFC 5128 §3.1) | — | formalizes (HyperDHT; Tailscale; NATCracker §5.8.3) | at once | — |

Notes:

- **R4 keeps plain** for an unclassified pair, and for a pair whose
  filtering is unknown. Plain is the cheapest technique that may work. Our
  DirectLinkPunch cross-check found that an EDM NAT with endpoint-independent filtering
  punches plain against ANY EDM peer (section 7). Eric's "both random,
  report at once" holds as stated once filtering is known closed (P3). With
  filtering unknown, R4 spends one plain stop first, with no knocks and no
  sockets; the partials usually end it early (open decision D3).
- **R7's own-evidence fallback** needs random steps on exactly one side. An
  EIM NAT that allocates SOCKETS at random reads like a random EDM (P1b).
  With both sides random and no class, R8 (D4).
- Today a facilitator's silence ends the session at once. Here it removes
  only the rows that need a reflexive address (R3, R3b, R4, R6, R6b, R7);
  R1, R2 and R5 stand.

## 4. Mid-attempt moves (secondary evidence)

Every fact enters the controller's evidence in the step it arrives. The
plan becomes `Row(evidence)` in that same step (`PlanFollowsEvidence`).
Using partials to reclassify the NAT is **ours**. The nearest practice is
ICE's triggered checks (RFC 8445 §7.3.1.4).

| evidence | seen by | effect |
|---|---|---|
| S4 heard, never acked, on plain | the EDM side | that side is EDM; the peer filters strictly; plain is impossible: R6, R7 or R8 without the silence timer |
| S7 the peer on its IP from another port (RFC 4787 §4.1) | the peer of an EDM side | the peer is EDM; with P1b, sequential or random; both EDM: R8 at once |
| S8 host or net unreachable on v6 | either | v6 fails at once |
| S11 REJECT PUNCH_FAILED, or OBS "silent" | the other side's verdict | completes the two-sided stop (section 5) |
| S13 the link lost | either | the attempt ends; no technique failure is recorded |
| S14 facilitator silence | the prober | rows needing a reflexive address drop out |
| S3 first heard late (beyond 7–567 ms) | either | widens `Stop` for the next technique |
| S9, S10 knocks / socket at the first hit | the easy / hard side | port block: R7's spray range and tranche next time |
| S17 binding lifetime (RFC 5780 §4.6) | either | refresh interval for birthday sockets and the keepalive: a function of the measured lifetime |

## 5. Stop conditions, and the two-sided stop

`Stop(t) = max(600 ms, 2 x relay RTT)` for lan, v6, hair, rev and plain.
Live clean pairs are heard within 567 ms. The RTT scaling formalizes DCUtR's
RTT/2 synchronization and RFC 8445 §14. For pred, predAB and bday the clock
starts when the knocks begin; for bday the stop is the spray plus one
refresh.

Each side's own stop gives its **verdict**: "silent", and "heard-only" if it
heard but was never acked. The follower sends its verdict in OBS.

**A technique has FAILED only when the controller holds both verdicts**
(`TwoSided`). Under loss, it fails at the controller's deadline instead.
This is **ours**: it generalizes leviculum's two-sided commit (#89). ICE
marks pairs failed per agent (RFC 8445 §6.1.2.6).

Without the two-sided stop, the controller re-planned on its own silence
and missed the peer's partials (section 10).

## 6. Commit: the controller nominates the pair

The controller nominates the pair it proved, heard and acked on its plan's
technique. It does so in-band, by repeated NOMINATE frames on that pair,
like ICE's USE-CANDIDATE (RFC 8445 §8.1.1). From then on the plan is frozen.

The follower commits only to the pair a NOMINATE arrived on, and only after
it has itself heard and been acked there. It confirms with
`COMPLETE(epoch, technique)`. The controller moves on that COMPLETE and
echoes it; the follower moves on the echo. This is make-before-break, as in
RFC 9000 §9 (migration keeps the old path until the new one is validated).

Today each side settles on the first address it heard and acked
(`std` :1313), and COMPLETE names no pair. That is DirectLinkPunch's
`samenat-hairpin` SamePath counterexample, real in the code. The design
removes it.

## 7. The plain-punch oracle

`Strategy.tla` abstracts the mechanism into `HPub(s)` ("s hears the
peer's punches") and `APub(s)` ("an ack reaches s from an address s
punched"), after peer-reflexive learning (RFC 8445 §7.2.5.3.1). They follow
RFC 4787 §4.1 and §5, plus one behaviour the first draft missed:

- **An EDM NAT with endpoint-independent filtering admits a packet from a
  remote it never sent to.** The flow that packet opens carries the side's
  replies to that remote from the port it arrived at (`Reuse`, `EffEIM`).
- Toward an EDM peer this always happens, because the peer's punches come
  from a port this side never sent to.
- Toward an EIM peer it races with this side's own first punch, which makes
  a new mapping (the `race` variable).

`run_strategy.py --oracle` runs DirectLinkPunch (pushed version
`acb936328`) with escalation denied on both sides (exactly `plain`) for all
81 (mapping, filtering) pairs. It compares each side's heard,
heard-and-acked, acked and other-port flags in every terminal state:

- **81/81 pairs agree.**
  - 73 agree exactly.
  - 4 differ only by a DirectLinkPunch artifact (EVIDENCE.md): an ack sent
    from the arrival port although the socket has a mapping toward its
    destination.
  - 4 (both EDM, both filtering openly) agree on the outcome; whether a
    side also saw the other's port move is a race.
- The first draft of the oracle disagreed on 32 pairs, all with an
  open-filtering EDM side. The cross-check is what corrected it.
- `oracle.md` has the table.

NATCracker's Thm 5.6.1 is a SEQUENTIAL hole punch: one side first, then
the other. That explains its stricter condition for plain (`natcracker.md`).

## 8. Learning

- Every technique outcome is a fact: `failed(t)` and `ok(t)`. So are the
  inferences: `heardOnly`, `otherPort`. Their validity is history(H).
  REQUEST, READY, PLAN and ABORT carry them, so the next attempt's
  controller holds them whichever side it is.
- The next attempt skips what failed and starts where it worked
  (`StartsWhereItWorked`). A failed birthday runs again at its next
  tranche, up to 3 tranches within the pair's 3 escalations.
- A peer known unpunchable gets R8 at once until the history expires
  (`NotRetriedSameWay`).
- **Retry is a function of the evidence**: propose again exactly when `Row`
  on the evidence the next attempt will hold is not R8
  (`RetryIsEvidenceFunction`).
- Per-peer technique history is **ours**: no surveyed system keeps one.
  DCUtR retries 3 times without learning.

## 9. Wire, and frame authentication

The planner's inputs travel in REQUEST and READY: the profile
(EVIDENCE.md P1–P13), as classes, never learned addresses. The other
signals:

- `PLAN(epoch, technique, why)` and `OBS(facts)`;
- ABORT reason `NODIRECT`;
- a NOMINATE frame kind;
- `COMPLETE(epoch, technique)`;
- the attempt tag on every signal. `UntaggedPlant` shows what goes wrong
  without it.

**Frame authentication is a precondition, not only a security property.**
We adopt rns-rs's wire (rns-net ≥ 0.14), negotiated per link by HELLO. It
**implements** the rns-rs DirectLink protocol document and commits:

- signals in link DATA context 0x80, preceded by HELLO;
- 52-byte punch and ack frames with a random nonce and
  HMAC-SHA256(token, …)[..16]. The token never appears on the wire;
- RNSK keepalives with a counter and a separate key, every 15 s, down after
  45 s.

Toward a peer that does not answer HELLO, the legacy raw-token wire stays
(leviculum#112).

`Strategy.tla` models the wire as a parameter: `HelloA`, `HelloB`, and
`Wire` ∈ {raw, mac}. Under it:

- **NoTokenToStranger, StrangerKnowledge.** Hold under either wire for the
  strategy. Under raw, today's LAN host-candidate punch leaks the token to a
  stranger on our own LAN (`lan-leak-cgn-raw`). Under mac the stranger
  receives an authenticated frame it can neither verify nor replay.
- **NoReplay.** Holds under mac. FAILS under raw, for either planner: an
  on-path observer replays the legacy keepalive (constant seq u32::MAX) to
  a side whose peer has gone (`replay-onpath-raw`). This implements rns-rs's
  RNSK counter freshness (RFC 7675 §5: consent only from an authenticated,
  fresh response).
- **NoRawToCapablePeer.** Never a raw frame to a peer that has shown HELLO.
  `DowngradePlant` violates it.
- **R6b exists only under mac.** Authenticated frames may go to predicted
  ports, which may be anyone's; raw-token frames may not. This is the
  mechanism model's exhaustive finding: without MAC, EDM on both sides is
  relayed-only.


## 10. What TLC checked

### The run

`run_strategy.py` ran 208 scenarios, generated into `strategy-matrix.md`:

- 94 environments, each under both planners;
- fault, learning, stranger, wire and plant scenarios.

All 208 runs completed with no TLC error and none was capped. In all, TLC
explored 1,041,809 distinct states under the strategy planner (the largest
run, `plant-untagged`, 847,232) and 344,480 under today's. The tool was
TLC 2.19, with `-workers 2`, under `nice -n 19 ionice -c3`.

Every run checks every property:

- the invariants by a probe that names each violation without stopping;
- the action properties (`NoSpam`, `UsesEvidence`, `NoNegligibleRow`,
  `Variant`, `MonotoneEvidence`) by an always-true action constraint;
- `Termination` as liveness, under weak fairness.

### Scale (what TLC did NOT check)

The bounds are:

- 1 to 3 attempts, `Hist` 1 (2 for the tranche runs), up to 3 tranches;
- stop and deadline timers of 2 and 3 ticks; today's window 3, its
  escalation at 1;
- one oracle draw per run: the tranche at which a birthday hits, and hit
  or miss for map, pred and both-random.

The success RATES are not in TLC. They are the quantitative column of
section 3, from the cited arithmetic and measurements. The likelihood
classes are checked only as a FLOOR (`NoNegligibleRow`). The environment
is fixed per run (no network change: gap M-N1).

v6, map, rev and the lan/hair split rest on the cited RFC reasoning. Only
the plain punch's oracle is cross-checked mechanically: 81/81 against
DirectLinkPunch `acb936328`.

### Results per property, strategy planner (105 runs)

| property | result |
|---|---|
| **Total** (`OneTechnique` + `PlanIsCheapest`) | holds in every run. The table agrees with the physics' cheapest sufficient technique above the floor, on every view either side reached. Determinism is by construction. |
| **NoSpam** (`NoSpamStep`, `EscPerAttempt`, `AttemptCost`, `CapsHeld`) | holds |
| **UsesEvidence** (`UsesEvidenceStep`, `PlanFollowsEvidence`) | holds |
| **NoNegligibleRow** (the floor) | holds, including `bothrandom-mac-reachable`. There the authenticated wire makes the both-random birthday REACHABLE; the strategy never plans it, and today's code does. |
| **DirectWhenPossible** | holds except in **4 evidence gaps**: the unclassified sequential EDM against a strict-filtering EIM (3 runs), and `hard-unclassified-eimrandalloc`. No secondary evidence separates them without the distinct-IP classifier (P1). |
| **NoFalseAbort** | holds, per tranche: a birthday failed at tranche k is not a false abort if only tranche k+1 would hit |
| **Agreement**, **SamePlan** | hold. Both ends compute the same epoch 1 in every run, and samenat-hairpin, the carrier NAT and dual-stack all agree, where today fails. |
| **NoHalt** (`Variant`, `NotStuck`, `Termination`) | holds in every run, including relay loss, datagram loss and three attempts |
| **NoRace** | holds. **`UntaggedPlant`** violates it. |
| **Learning** (`NotRetriedSameWay`, `RetryIsEvidenceFunction`, `StartsWhereItWorked`) | holds, including the tranche ramp (`tranches-hard`: a miss at 64 sockets runs 128, then 256, within the budget; `tranches-hard-budget2` stops at the budget) |
| **NoTokenToStranger**, **StrangerKnowledge**, **NoPathToStranger** | hold, under both wires. **`HijackPlant`** violates the first two; nomination still keeps the path off the stranger. |
| **NoReplay** | holds under the HMAC wire. **FAILS under the raw wire** (`replay-onpath-raw`), for either planner: the property needs the rns-rs wire, not a better planner. |
| **NoRawToCapablePeer** | holds. **`DowngradePlant`** violates it, and NoReplay with it. |
| **FacilitatorKnowledge** and the rest kept from DirectLinkPunch | hold. **`NamedFacilitatorPlant`** violates FacilitatorKnowledge. |

### Today's behaviour (103 runs): the counterexamples

"Real" means confirmed against the code by reading it.

1. **Two pairs on one link (Agreement)**:
   - `samelan-hairpin`: LAN on one side, hairpin on the other. This is
     DirectLinkPunch's samenat-hairpin; real (`std` :1313, `udp` :220).
   - `samecgn-cand`: the same split on a carrier NAT.
   - `v6-both`: v4 on one side, v6 on the other.
2. **Techniques racing (OneTechnique, 62 runs)** and **escalation without
   evidence (NoSpam, 63 runs)**. For example: both random, so both sides
   open sockets and spray (AttemptCost, EscPerAttempt).
3. **Below the floor (NoNegligibleRow, 10 runs)**: today runs the birthday
   against a random or sequential EDM peer, the squared model. With MAC'd
   frames it is even reachable (`bothrandom-mac-reachable` reaches direct),
   but at about 0.003%.
4. **The cheaper technique ignored (UsesEvidence, 57 runs)**:
   - a facilitator's silence ends the session although v6 or a router
     mapping would work; real (`core probe_failed` :461-474);
   - router mapping is never used;
   - reversal is never used.
5. **A partial result suppresses the escalation it calls for (NoFalseAbort
   and DirectWhenPossible)**. A random side that hears without being acked
   never opens its birthday sockets, while the peer sprays into nothing.
   Real (`std` :964, :1170).
6. **Retried the same way (59 runs)**, and **the retry depends on arrival
   order (59 runs)**: the peer's REJECT PUNCH_FAILED, arriving first,
   clears the retry. Real (`node` :1417-1421).
7. **The raw wire.** Two failures:
   - `lan-leak-cgn-raw`: the LAN host-candidate punch sends the token to a
     stranger on our own LAN (NoTokenToStranger). Real (`core
     same_nat_alternates` :106-121: one public IP is taken for one LAN).
   - `replay-onpath-raw`: keepalive replay (NoReplay). Real: keepalives
     carry seq u32::MAX and the raw token (SURVEY.md §8).

   **Both disappear under the HMAC wire** (`lan-leak-cgn-mac`,
   `replay-onpath-mac`: no violation).

Where today does better:
- `hard-unclassified-eimrandalloc`: its symmetric birthday sometimes
  connects, at twice the cost (D4).
- `bothrandom-mac-reachable`: it reaches direct only by drawing a
  negligible outcome.

### What the design fixed on the way (the model's own findings)

- The **two-sided stop** (section 5). Without it the controller re-planned
  on its own silence while the peer's partial was in flight, and either
  sprayed a hopeless pair or gave up on a predictable one.
- The **open-EDM reuse race**, found by the cross-check (section 7).
- **Initiator as controller** with a shared epoch 1: SamePlan holds.
- A DirectLinkPunch **ack artifact** (EVIDENCE.md), reported to its owner.

### Model choices (Strategy.tla)

- **S-M1, timers.** A timer does not run while a datagram it waits for is
  in flight (unless NetLossy), while a signal of this attempt is in flight
  (unless RelayLossy), or while the peer is still producing its verdict or
  plan. This is DirectLinkPunch's M1 and M3.
- **S-M2, skew.** Both ends run one epoch's timers in step (`SkewOK`;
  DirectLinkPunch MaxSkew 0).
- **S-M3, the mechanism as an oracle**, checked for plain.
- **S-M4, the relay is a set**: always reordered, lost only with
  RelayLossy.
- **S-M5, facts carry no addresses.** The profile is constant per run.
- **S-M6, the wire is a constant per run.** Negotiation failure modes
  (HELLO timeout, a stripped HELLO) are not modelled. Signals ride the
  encrypted link, so a third party cannot strip them.

## 11. Open decisions for Eric

Already decided, and in the model: initiator nomination; caps of 256 / 1,750
in tranches; retries within 3 per pair; the HMAC wire later via HELLO; the
host decides escalation under ask_host. The trust gate is a parameter here
(`EscA`, `EscB`); proposing is not trust.

- **D2. The hairpin.** R3 punches through the shared NAT only when there
  are no LAN or carrier candidates, or after R1 failed (RFC 6888 REQ-9
  makes a CGN hairpin). Allow it there, or never?
- **D3. Both EDM, filtering unknown.** One plain round first (catches the
  open-EDM pairs, which DirectLinkPunch shows do punch), or "no direct path
  at once" until the filtering test (P3) exists?
- **D4. Ambiguous own evidence** (random allocation on both sides, no
  classifier). R8 (this design), today's symmetric birthday (twice the
  cost), or require the distinct-IP observer first?
- **D5. History.** H, how many attempts a failure is remembered (1 here,
  2 for the ramp), and what counts as a network change. It lives in memory
  only (CC 2.6.8).
- **D6. Old peers.** Toward a raw-wire peer, keep today's pair split, or
  refuse to commit without a nomination?
- **D7. The relay** sees PLAN and OBS timing: it learns that a re-plan
  happened, never why. Acceptable?
- **D8. Thresholds.**
  - The `Stop` floor (600 ms) and its RTT multiple (2x).
  - The likelihood classes' bounds (certain ≥ 95%, high ≥ 50%, low ≥ 1%).
  - Whether the floor should be "low" rather than "negligible": that would
    drop R3 hairpin on home NATs (Ford: 24%) and R6b.
- **D9. R5b PCP PEER** (RFC 6887 §12): shipped by nobody, but it makes an
  EDM side exactly predictable. Build it?
- **D10. Where the sources disagree** (`natcracker.md`):
  - Sequential against random with non-strict filtering: NATCracker
    Thm 5.7.1 says feasible, and Keller measured 14–17.5%. Our table says
    R8. Add R6c?
  - Sequential against sequential, both strict: NATCracker Thm 5.8.1 says
    feasible; DirectLinkPunch says relayed even with MAC. Which model of
    prediction is right needs a live witness.
