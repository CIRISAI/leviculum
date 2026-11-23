# The punch strategy: choosing the technique from evidence

`DirectLinkPunch.tla` models HOW the punch works: sockets, mappings,
filters, tokens, the commit. This document and `Strategy.tla` cover WHICH
technique to run, chosen from the evidence (`EVIDENCE.md`). They also
cover how both ends agree on it and on the pair, and how one attempt's
outcome becomes the next attempt's evidence.

Eric's direction is the specification:

- Use the technique the situation calls for. Spamming techniques is not
  effective.
- Never dismiss evidence that arrives through secondary channels:
  durations, partial results, the peer's verdict.
- Rule out halting and race states.

The model is where everyone's work integrates. A change to the punch
changes `Row` in `Strategy.tla` first (`CONFORMANCE.md`).

## 1. Who decides

**One end decides: the responder, called the controller.** It is the first
end to hold both profiles: the initiator's profile arrives in REQUEST, and
its own comes from its probe. So it plans epoch 1 and sends the plan in
READY, which costs no extra round trip. Mid-attempt it re-plans in the step
a fact arrives and sends `PLAN(epoch, technique, why)`. The initiator, the
controlled side, runs whatever plan it last received. It reports its own
partials and its verdict in `OBS`. The controller also nominates the pair
(below).

Why not "both compute identically from shared evidence"? The decisive
secondary evidence is asymmetric. Only the side that heard without being
acked knows it did, and only the side that saw the peer's port move knows
that. The two ends also learn these facts at different moments. Two
planners on two evidence sets at two moments diverge.

The model showed exactly this before the two-sided stop (section 5) was
added. The controller re-planned on its own silence while the peer's
heard-but-not-acked report was in flight:

- `MapA=EDMrand, MapB=EDMrand, FiltB=EIF`, unclassified: it opened birthday
  sockets on a pair the next report proved hopeless;
- `MapB=EDMseq, FiltB=EIF`, unclassified: it declared no direct path
  although prediction would have worked.

With one decider, monotone evidence, and plans tagged by epoch, Agreement
holds by construction, and TLC confirms it.

ICE makes the offerer the controlling agent. Choosing the responder
instead saves one relay round trip. That is open decision D1.

## 2. The techniques, as DirectLinkPunch configurations

A technique is one way to run the mechanism DirectLinkPunch models. The
planner picks one at a time. A technique never runs beside another, except
that the cheap plain punch keeps running under prediction and birthday,
because those are built on it.

| technique | what runs | DirectLinkPunch configuration | holds | cost bound per attempt |
|---|---|---|---|---|
| `lan` | tokened punches to the peer's host candidates (same LAN), or carrier-side candidates (same carrier NAT), only | `targets = {Lan(o)}`, no public target | nothing | 10 pps x candidates (at most 4) x stop |
| `v6` | tokened punches to the peer's global v6 address only | targets = the v6 candidate (dual-stack; not in DLP, gap G6) | nothing | 10 pps x 1 x stop |
| `hair` | tokened punches to the peer's reflexive address through the shared NAT | `SameNAT`, `targets = {padv}`, Hairpin | nothing | 10 pps x 1 x stop |
| `plain` | tokened punches to the reflexive address, plus every peer-reflexive source that proves the token | `HostTrust = deny` (no knocks, spray or birthday) | nothing | 10 pps x (1 + at most 4 peer-reflexive) x stop |
| `mapX` | X asks its router (PCP, then NAT-PMP, then UPnP-IGD) to map the SESSION socket, advertises the mapped address in PLAN, then both run plain | not in DLP: X becomes EIM with open filtering | the router lease (deleted on release) | at most 3 router transactions, plus plain |
| `predX` | plain, plus knocks from the other side to X's predicted ports (at most 16) | DLP `held`/`knocking` from tick 0, not after a silence timer | the pair's budget | 16 knocks per tick x stop; 1 escalation |
| `bdayX` | plain; X opens K birthday sockets toward the other side, which sprays S knocks on X's IP | DLP `bday`/`spray` from tick 0 | K sockets and permits on X; the spray budget on the other side | K x 2 pps + S knocks; 1 escalation |

K and S come from the evidence: a known port block narrows the range (S9,
S10). The defaults are today's: K = 128 (32 on own evidence), S = 960, with
odds of about 85% (38%) over a full 16-bit pool. Tailscale's 256 x 2048
gives 99.9%.

The cost units in `Strategy.tla` (`Cost`) rank them: lan 1, v6 1, hair 2,
plain 2, map 3, pred 5, bday 12.

## 3. The decision table

The table is a total, deterministic decision list over the controller's
evidence. Rows are tried in order. A technique already "failed" (both
verdicts in, section 5) is skipped, and a failure stays valid for history
H. R8 is the only otherwise.

`Row` in `Strategy.tla` is this table, word for word. `Cheapest` is the
same answer computed from the physics: each technique's sufficiency
precondition (`Pre`) and the cost order. TLC checks that the two agree on
every view either side ever reaches (`PlanIsCheapest`).

| row | situation (evidence) | technique | expected success | stop condition | moves mid-attempt |
|---|---|---|---|---|---|
| **R1** same network | the same public IP (P8), with LAN or carrier-side candidates (P8) | `lan` only, **never the hairpin** | high on one LAN (host path); a carrier NAT only with carrier-side candidates | silent past `Stop` | silent: R2, else R3 |
| **R2** both global v6 | both have a global v6 address (P6) | `v6` only (a stateful firewall opens on a simultaneous send) | high unless a firewall drops unsolicited inbound UDP; unmeasured | silent past `Stop`, or ICMP no-route (S8) at once | silent or unreachable: R3/R4 |
| **R3** one public IP, no usable candidates | the same public IP, no candidates (or R1 failed), both reflexive | `hair` | as the NAT hairpins (P4; RFC 6888 requires it of a CGN) | silent past `Stop` | silent: R5 |
| **R4** apart, classes allow plain | different public IPs, both reflexive, and NOT (both EDM and neither may filter openly) and NOT (a closed EDM facing a strict filter) | `plain` | EIM pairs: 100% in the live runs; one EDM against an address-filtering EIM: by peer-reflexive learning | silent past `Stop` | heard-not-acked (S4): this side is EDM, the peer strict: R6/R7/R8 at once. Same IP, other port (S7): reclassify the peer as EDM; with both EDM: R8 at once |
| **R5** a router maps | P7 on either side (not `CgnatDetected`) | `mapX` (A first) | high when the router honours it and is outermost; `Mapped` is not proof | the router refuses; or plain silent past `Stop` | refused: the other side's map, or R6/R7 |
| **R6** one sequential EDM | X is EDM-sequential (P1 + P1b), the other EIM or unknown, the knocker's gate allows (P11), budget left | `predX` | when the step held between the measurement and the punch | silent past `Stop` after the knocks begin | silent: R7 or R8 |
| **R7** one random EDM | X is EDM-random (P1), or own evidence (random steps on exactly ONE side, class unknown, and plain failed), the other EIM or unknown, both gates allow, budget left | `bdayX`, K and S from the evidence | 85% at 128 x 960, 38% at 32 x 960 (full pool); more on a port block | the spray spent and one refresh later, or a hit | heard on the session socket: stay (no switch, DLP review 5) |
| **R8** nothing can work | otherwise: e.g. both EDM with filtering known closed, no v6, no mapping, no candidates; or every row failed or is gated | **none**: "no direct path" at once, the link unchanged, handed to the reflector fallback (CIRISEdge#1024) | none | at once | none |

Notes on the rows:

- **R4 keeps plain for an unclassified pair**, and for a pair whose
  filtering is unknown. Plain is the cheapest technique that may work.
  Classes only narrow the table, and an EDM NAT with endpoint-independent
  filtering punches plain against any EDM peer. The DirectLinkPunch
  cross-check found this (section 7).
- Eric's example "both random-EDM, no v6, no mapping: report no direct
  path at once" holds as stated once filtering is known closed (P3). With
  filtering unknown, R4 spends one plain stop first: no knocks, no
  sockets. The partials (S4, S7) usually end it before `Stop`, at R8.
  This is open decision D3.
- **R7's own-evidence fallback needs random steps on exactly one side.**
  An EIM NAT that allocates sockets at random reads exactly like a random
  EDM (P1b). With random steps on both sides and no class, the table can
  name no hard side. It answers R8 rather than spraying both ways, which
  is today's behaviour at twice the cost. TLC reports this as a
  DirectWhenPossible violation (scenario `hard-unclassified-eimrandalloc`):
  it is an evidence gap, not a planner bug (D4).
- Today a facilitator's silence ends the session at once
  (`core probe_failed`). Here it removes only R3, R4, R6 and R7. R1, R2 and
  R5 stand (scenario `fac-silent-a-v6`).

## 4. Mid-attempt moves (secondary evidence)

Every fact enters the controller's evidence in the step it arrives, and
the plan becomes `Row(evidence)` in that same step
(`PlanFollowsEvidence`).

| evidence | seen by | effect |
|---|---|---|
| S4 heard, never acked, on plain | the EDM side | that side is EDM (`MapV`); the peer filters strictly (`FiltV`); plain is impossible: R6, R7 or R8, without the silence timer |
| S7 the peer on its IP from another port | the peer of an EDM side | the peer is EDM; with the steps (P1b), sequential or random; both EDM: R8 at once |
| S8 host or net unreachable on v6 | either side | v6 fails at once |
| S11 REJECT PUNCH_FAILED, or OBS "silent" | the other side's verdict | completes the two-sided stop (section 5) |
| S13 the link lost | either side | the attempt ends; no technique failure is recorded |
| S14 facilitator silence | the prober | rows needing a reflexive address drop out |
| S3 first heard late (beyond the usual 7 to 567 ms) | either side | widens `Stop` for the next technique (a prior, not a row) |
| S9, S10 the knocks or the socket at the first hit | the easy side, the hard side | the port block: K and S for the next attempt |

## 5. Stop conditions, and the two-sided stop

`Stop(t) = max(600 ms, 2 x relay RTT)` for lan, v6, hair and plain. Live
clean pairs are heard within 567 ms. For pred and bday the clock starts
when the knocks begin; for bday the stop is the spray plus one
`BIRTHDAY_REFRESH`. Each side's own stop gives its **verdict**: "silent",
and "heard-only" if it heard but was never acked.

The controlled side sends its verdict in OBS. **A technique has FAILED only
when the controller holds both verdicts** (`TwoSided`). Otherwise, under
loss, it fails at the controller's deadline (`LimT`, the phase timeouts
of 10 to 12 s).

Without this rule the controller re-planned on its own silence and missed
the peer's partials. Both counterexamples in section 1 came from that.
DirectLinkPunch's two-sided commit (#89) is the same idea applied to the
commit.

## 6. Commit: the controller nominates the pair

The controller nominates the pair it proved, heard and acked on its
plan's technique. It does so in-band, by repeated NOMINATE frames on that
pair, like ICE's USE-CANDIDATE. From then on the plan is frozen.

The controlled side commits only to the pair a NOMINATE arrived on, and
only after it has itself heard and been acked there. It confirms with
`COMPLETE(epoch, technique)`. The controller moves on that COMPLETE and
echoes it; the controlled side moves on the echo.

Today each side settles on the first address it heard and acked
(`std` :1313), and COMPLETE names no pair. TLC reproduces the
DirectLinkPunch `samenat-hairpin` SamePath counterexample at this layer
(today: A on the LAN pair, B on the hairpin), and also a dual-stack one
(A on v6, B on v4). The design removes both: **FIXED, not excluded**
(Agreement holds in every strategy scenario).

## 7. The plain-punch oracle

`Strategy.tla` abstracts the mechanism into `HPub(s)` ("s hears the
peer's punches") and `APub(s)` ("an ack reaches s from an address s
punched"), after peer-reflexive learning. They follow RFC 4787, plus the
one behaviour the first draft missed:

- **An EDM NAT with endpoint-independent filtering admits a packet from a
  remote it never sent to.** The flow that packet opens carries the
  side's replies to that remote from the port the packet arrived at
  (`Reuse`, `EffEIM`).
- Toward an EDM peer this always happens, because the peer's punches come
  from a port this side never sent to.
- Toward an EIM peer it races with this side's own first punch, which
  makes a new mapping (the `race` variable).

`run_strategy.py --oracle` runs DirectLinkPunch with escalation denied on
both sides (exactly `plain`) for all 81 (mapping, filtering) pairs. It
compares each side's heard, heard-and-acked, acked and other-port flags in
every terminal state. Results:

- 73 pairs agree exactly.
- 4 pairs differ only by a DirectLinkPunch artifact (EVIDENCE.md, "A finding about
  DirectLinkPunch"): an ack sent from the arrival port although the socket
  has a mapping toward its destination.
- 4 pairs (both EDM, both filtering openly) agree on the outcome. Whether
  a side also saw the other's port move is a race.
- The first draft disagreed on 32 pairs, all with an open-filtering EDM
  side. The cross-check is what corrected it.

`oracle.md` has the table. It used the local working copy of
`DirectLinkPunch.tla` (sha256 `e8fe47cd4bfe2bd2…`) from 2026-11-22.

## 8. Learning

- Every technique outcome is a fact: `failed(t)` and `ok(t)`. So are the
  inferences: `heardOnly`, `otherPort`. Their validity is history(H)
  (EVIDENCE.md). REQUEST, READY, PLAN and ABORT carry them, so the next
  attempt's controller holds them whichever side it is.
- The next attempt skips what failed and starts where it worked: a peer
  that needed birthday starts at R7 next time (`StartsWhereItWorked`).
- A peer known unpunchable gets R8 at once until the history expires
  (`NotRetriedSameWay`).
- **Retry is a function of the evidence**: propose again exactly when
  `Row` on the evidence the next attempt will hold is not R8
  (`RetryIsEvidenceFunction`).
- Today the initiator's retry depends on which verdict arrives first. Its
  own PUNCH_FAILED schedules a retry. The peer's REJECT PUNCH_FAILED,
  arriving first, clears it (`node` :1417-1421: any rejection but BUSY).
  TLC finds both orders on the same evidence.
- Edge's one-hour hold after PunchFailed (edge `direct_path.rs` :190) is
  replaced by the same rule.

## 9. Wire

The strategy needs these signals. Everything is negotiated by a REQUEST
flag; with a peer that lacks it, today's behaviour stays.

- **REQUEST and READY carry the profile**: the class (P1, P1b), filtering
  when known (P3), v6 (P6), router mapping (P7), same-network indications
  (P8), the gate (P11) and the history facts. They are classes, never
  learned addresses beyond the candidates already sent.
- **READY carries the plan**: epoch 1, the technique, its parameters (the
  hard or sequential side, K, S, a port block).
- **New `PLAN` and `OBS`** signals.
- **ABORT reason `NODIRECT`** (R8).
- **A NOMINATE punch-frame kind.**
- **`COMPLETE` carries (epoch, technique)**.
- **Every signal carries the attempt tag.** `UntaggedPlant` shows what goes
  wrong without it.

## 10. What TLC checked

### The run

`run_strategy.py` ran 181 scenarios, generated into `strategy-matrix.md`:

- 90 environments, each under both planners;
- fault scenarios: relay loss, datagram loss;
- learning scenarios: a path lost later, three attempts;
- the stranger, and the three plants.

All 181 runs completed with no TLC error and none was capped. In all,
TLC explored 365,401 distinct states under the strategy planner (the
largest run, `plant-untagged`, 282,048) and 144,104 under today's.

The tool was TLC 2.19 (tla2tools 1.7.4), with `-workers 2`, under
`nice -n 19 ionice -c3`.

Every run checks every property:

- the invariants by a probe that names each violation without stopping;
- the action properties (`NoSpam`, `UsesEvidence`, `Variant`,
  `MonotoneEvidence`) by an always-true action constraint that names them;
- `Termination` as liveness, under weak fairness.

### Scale (what TLC did NOT check)

The bounds are:

- two attempts (three in one scenario), `Hist` = 1;
- stop and deadline timers of 2 and 3 ticks; today's window 3, its
  escalation at 1;
- one oracle draw per run for map, pred and bday (hit, miss, or both).

Birthday odds, the sizes of K and S, and real durations are not in the
model: a hit is a branch. The environment is fixed per run; there is no
network change mid-run (gap M-N1). The techniques DirectLinkPunch does not
model (v6, map) rest on RFC 4787 reasoning only. Only the plain punch's
oracle is cross-checked mechanically (section 7: 81 of 81 pairs).

### Results per property, strategy planner (91 runs)

| property | result |
|---|---|
| **Total** (`OneTechnique` + `PlanIsCheapest`) | holds in every run. The table agrees with the physics' cheapest sufficient technique on every view either side reached. Determinism is by construction (a decision list). |
| **NoSpam** (`NoSpamStep`, `EscPerAttempt`, `AttemptCost`, `CapsHeld`) | holds. Nothing starts without its precondition, nothing restarts in an attempt or against a valid failure, there is at most one escalation per attempt, and the cost cap and the pair budget hold. |
| **UsesEvidence** (`UsesEvidenceStep`, `PlanFollowsEvidence`) | holds. Nothing starts while something strictly cheaper is available, and the controller runs `Row(evidence)` in every state (it re-plans in the step a fact arrives). |
| **DirectWhenPossible** (no false hopeless) | holds except in **4 evidence gaps**: the unclassified sequential EDM against an EIM with strict filtering, in 3 filtering variants (`pair-*-EDMseq-*--EIM-APDF-clsF` and its mirror), and `hard-unclassified-eimrandalloc`. No secondary evidence separates them without the classifier (P1); they need the distinct-IP second observer (gap 3). |
| **NoFalseAbort** | holds. Without loss, a technique is only declared failed when it cannot work. |
| **Agreement** (`SamePlan`, `Agreement`) | holds, including the same-LAN-with-hairpin, the carrier NAT and the dual-stack scenarios, where today fails. |
| **NoHalt** (`Variant`, `NotStuck`, `Termination`) | holds in every run, including relay loss and datagram loss. A lexicographic variant falls on every step, so every run ends within the variant's bound. |
| **NoRace** (`NoRace`) | holds. No resource outlives its technique and no commitment outlives its attempt. **`UntaggedPlant` violates it**: without attempt tags, a late PLAN restarts sockets on a side whose attempt ended. |
| **Learning** (`NotRetriedSameWay`, `RetryIsEvidenceFunction`, `StartsWhereItWorked`, `MonotoneEvidence`) | holds, including a path lost after it was established, and three attempts. |
| **Kept from DirectLinkPunch** (`EscalationNeedsTrust`, `AbortLeavesLinkUnchanged`, `NoLoneMove`, `FacilitatorKnowledge`, `FacilitatorCarriesNothing`, `NoTokenToStranger`, `StrangerKnowledge`, `NoPathToStranger`) | hold. **`HijackPlant`** violates `NoTokenToStranger` and `StrangerKnowledge`. `NoPathToStranger` still holds under the plant, because nomination accepts only the plan's technique; today's planner violates it. **`NamedFacilitatorPlant`** violates `FacilitatorKnowledge`. |

### Today's behaviour (90 runs): the counterexamples

Each one is reached by TLC at this layer. "Real" means confirmed against
the code by reading it, beyond the model.

1. **Two pairs, one link (Agreement)**:
   - `samelan-hairpin`: A settles on the LAN pair, B on the hairpin. This
     is DirectLinkPunch's `samenat-hairpin` SamePath counterexample at
     this layer.
   - `samecgn-cand`: the same split, on a carrier NAT.
   - `v6-both`: A settles on v4 and B on v6, the suspected lit-review
     gap 2.

   Real: `std` :1313 picks the first heard-and-acked address; COMPLETE
   names no pair; `udp` :220 drops any other source.
2. **Techniques racing (OneTechnique, 52 runs)**: plain with v6, LAN with
   hairpin, prediction with birthday.
3. **Escalation without the evidence for it (NoSpam, 53 runs)**:
   - both sides random, or seq against random: both sides open birthday
     sockets and both spray, and the 10 s window burns
     (`bothrandom-nothing`: AttemptCost, EscPerAttempt);
   - tokened punches go to a peer's v6 address when this side has no v6
     (`v6-one-side`).
4. **The cheaper technique ignored (UsesEvidence, 48 runs)**:
   - birthday or prediction starts on the 1.5 s silence timer while plain
     has not failed by any verdict;
   - a router mapping is never used (`upnp-*`: today ends relayed where R5
     goes direct);
   - **a facilitator's silence ends the session although v6 or a router
     mapping would work** (`fac-silent-a-v6`, `fac-silent-a-upnp`; real:
     `core probe_failed` :461-474).
5. **A partial result suppresses the escalation that needed it
   (NoFalseAbort, DirectWhenPossible; `pair-EIM-APDF--EDMrand-EIF`,
   `hard-unclassified-eif`)**. The random side hears the peer but is never
   acked. `heard` is then non-empty, so it never opens its birthday
   sockets, while the peer sprays into nothing. Both fail although the
   birthday would succeed. Real: `std` :964 (`primary_heard`) and :1170
   (`heard.is_empty()`). The strategy reads the same heard-not-acked as
   evidence: the side is EDM, so it goes to R7.
6. **Retried the same way (NotRetriedSameWay, 51 runs)**: the next attempt
   repeats every technique that just failed.
7. **The retry depends on arrival order (RetryIsEvidenceFunction, 51
   runs)**. On the same evidence, the initiator retries if its own window
   ended first, and does not if the peer's REJECT PUNCH_FAILED came first.
   Real: `node` :1417-1421.
8. **The plant**: with `HijackPlant`, today settles on the stranger
   (`NoPathToStranger`), as DirectLinkPunch showed for 72be0f9.

Where today does better: `hard-unclassified-eimrandalloc` (an EIM NAT that
allocates sockets at random, against a random EDM, unclassified). Today's
symmetric birthday reaches direct, at twice the cost; the strategy reports
no direct path. That is open decision D4.

### Model choices (Strategy.tla)

- **S-M1, timers.** A timer does not run while a datagram it waits for is
  on its way (unless NetLossy), while a signal of this attempt is in
  flight (unless RelayLossy), or while the peer is still producing its
  verdict or plan (`AwaitsVerdict`, `AwaitsPlan`, the Deadline guard). This
  is DirectLinkPunch's M1 and M3: relay and network are faster than any
  timer.
- **S-M2, skew.** Both ends run one epoch's timers in step (`SkewOK`;
  DirectLinkPunch MaxSkew 0).
- **S-M3, the mechanism as an oracle.** `Works`, `Hears`, `Acks` and
  `race`, checked against DirectLinkPunch for plain (section 7).
- **S-M4, the relay is a set.** Signals are always reordered, and lost
  only with RelayLossy. The real Channel is ordered and reliable, so this
  is stronger than the code needs.
- **S-M5, facts carry no addresses.** The profile is constant per run.

## 11. Open decisions for Eric

- **D1. The controller.** The responder (this design: the plan rides
  READY, saving one relay round trip), or the initiator (ICE's
  controlling side, at one more round trip)?
- **D2. The hairpin.** R3 punches through the shared NAT only when there
  are no LAN or carrier candidates, or after R1 failed. Allow it there, or
  never?
- **D3. Both EDM, filtering unknown.** One plain round first (catches the
  EDM-with-open-filtering pairs, which do punch; partials usually end it
  early), or "no direct path at once" (your example) until a filtering
  test (P3) exists?
- **D4. Ambiguous own evidence** (random allocation on both sides, no
  classifier). R8 (this design), today's symmetric birthday (twice the
  cost, sometimes direct), or require the distinct-IP observer first?
- **D5. History.** H, the number of attempts a failure is remembered (the
  model uses 1), and what counts as a network change. It lives in memory
  only, under CC 2.6.8.
- **D6. Old peers.** A strategy node meeting a today node keeps today's
  flow. Does that include the pair split (gap 2), or does a strategy node
  refuse to commit without a nomination?
- **D7. The relay.** It sees two more signal types (PLAN, OBS) and their
  timing: it learns that a re-plan happened, never why. Acceptable?
- **D8. Thresholds.** The `Stop` floor (600 ms) and its RTT multiple
  (2x); whether R7 on own evidence alone (32 sockets, about 38% on a full
  pool) is worth its cost, or should wait for a port-block estimate.
