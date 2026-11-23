# Conformance: how the code refines Strategy.tla

The code is to refine a model that formalizes established practice (RFC
4787, RFC 5128, RFC 8445 roles and nomination, the rns-rs wire). The
conformance tests trace back to those sources through REFERENCES.md §2.

## The rule

**Any change to the punch's behaviour changes `Strategy.tla` first.** That
covers which technique runs, when it stops, what is escalated, how the pair
is chosen, and when to retry.

The PR that changes the code also carries:

- the spec change;
- a regenerated `strategy-matrix.md`, `oracle.md` and `plan-vectors.jsonl`
  from `run_strategy.py`;
- the diff of the violated-properties column, which must show no new
  violation in a `strategy--` row.

Mechanism changes (sockets, tokens, filters, the facilitator) go to
`DirectLinkPunch.tla` first, as before. Then `run_strategy.py --oracle`
reruns, because the strategy's oracle rests on that mechanism.

## The single planner function

```rust
// leviculum-core/src/direct_link/strategy.rs: no_std, no I/O, no clock, no RNG
pub fn plan(view: &View, attempt: u32) -> Plan;   // Row(kn, F, tried, n)
pub fn likelihood(view: &View, attempt: u32, t: Tech) -> Likely; // Likely: the floor

pub struct View<'a> {
    pub own: Profile,             // EVIDENCE P1..P11 for this side
    pub peer: Option<Profile>,    // from REQUEST / READY
    pub facts: &'a [Fact],        // valid facts only (the caller applies validity)
    pub tried: TechSet,           // started this attempt
    pub budget_left: u8,          // escalations left for the pair
    pub auth_wire: bool,          // both answered HELLO (EVIDENCE P14)
    pub port_block: bool,         // the hard side's allocator block is known (P12)
}
pub struct Profile { mapping: Mapping, filtering: Filtering, steps: Steps,
                     global_v6: bool, router_maps: bool, reflexive: bool,
                     same_public_ip: bool, candidates: bool, may_escalate: bool }
pub enum Fact { Silent(Side, Tech), HeardOnly(Side, Tech), OtherPort(Side),
                Failed(Tech), Ok(Tech) /* .. each with its attempt */ }
pub enum Plan { Lan, V6, Hairpin, Reversal, Plain, Map(Side), Predict(Side), PredictBoth,
                Birthday(Side /* tranche from the facts */), NoDirect }
```

The controller is the INITIATOR (RFC 8445 §6.1.1), and both ends call
`plan` for epoch 1 on the same inputs.

`plan` is `Row` in `Strategy.tla`, with `MapV` and `FiltV` as its private
helpers. It must not read anything `Row` does not read. Validity
(`Valid`) and the two-sided stop (`TwoSided`) are separate pure functions
beside it. The sizing of a technique (K, S, the port block) is a second
pure function, `size(plan, view) -> Params`, and it never changes which
row is chosen.

## Model traces become test vectors

1. **Planner vectors.** `run_strategy.py OUT --vectors` runs every
   `strategy--` scenario through `StrategyVectors.tla`. For every reachable
   state where the controller holds both profiles, it prints the planner's
   inputs exactly as `View` holds them (classes, flags, failed set, budget)
   and `Row`'s answer. The deduplicated set is `plan-vectors.jsonl`, one
   JSON object per line. A unit test in `strategy.rs` parses the file and
   asserts `plan(view) == expected` for every line. The vectors are
   generated from the model, never written by hand.
2. **Scenario traces.** A counterexample or a chosen behaviour (TLC
   `-dump` or the trace of a violated property) becomes a sans-I/O test of
   `core::Session`. Each action in the trace maps to one `Session` input:

   | model action | `Session` input |
   |---|---|
   | `Probe` | `probed_with_candidates` |
   | `RecvReq`, `RecvReady`, `RecvPlan`, `RecvObs`, `RecvComplete`, `RecvAbort` | `signal` |
   | `Hear`, `Ack`, `Tick` | a new `observed(Fact)` input |
   | `Nominate`, `RecvNom` | `punched` / a new `nominated` input |

   After each input the test asserts the `Step`s against the next state:
   `Send(Signal)`, `Probe`, `Punch{plan}`, `Established`, `Failed`. The
   session is already sans-I/O (`core` :1-22), so a trace replays
   deterministically.
3. **Refinement mapping**, for review rather than TLC:

   | model | code |
   |---|---|
   | `ph` | `Phase` |
   | `ts` | the driver's running technique |
   | `ev` | the session's fact list |
   | `msgs` | the channel outbox and inbox |
   | `res` | the sockets, permits and router leases the driver holds per session |
   | `commit` | the interface's peer address |

   `NoRace` becomes a debug assertion in `DirectLinkRuntime::release` and
   on each re-plan: nothing outlives its technique.

## Code changes, in order: leviculum main (0.35) first

**Delivery target: leviculum main, the 0.35 line, first.** The mechanism
(prediction, birthday, the trust gate, the budgets) is being ported there
by `lev-port35` (branch `feat/punch-on-035`, which so far carries the
one-line punch and probe diagnostics, `9a036df5e`). That port is the
prerequisite (step 0); it is not part of this list.

Main already has three things the order relies on:

- #100, the distinct-IP NAT verdict, with peers as probe servers (`core
  nat.rs` :173; `std direct_link/nat.rs`);
- #101, the port-mapping engine, with gateway-change detection;
- a NAT probe that tolerates ICMP errors (`aac90a091`).

Each step is shippable alone, and the gate stays green between steps.
Sizes are S, M or L.

| # | change (on 0.35) | size | depends on | evidence it adds | rows / properties it serves |
|---|---|---|---|---|---|
| 0 | the mechanism port (`lev-port35`) | — | — | — | — |
| 1 | **Expose the evidence live**, with no behaviour change: probe attempt and RTT (S1); first heard / first acked, heard-only and acked-only (S3–S5), `HeardRelation` per source (S6, S7), knocks at the first hit (S9) and the winner's relative port (S10), handed to the node as they happen | M | 0 | S1, S3–S7, S9, S10 | §4 moves |
| 2 | **`plan()`, `likelihood()`, `size()`** in core, tested by `plan-vectors.jsonl`; nothing calls them yet | M | — | — | Row, Likely |
| 3 | **Classification into the profile**: #100's verdict (P1), port preservation (P2), pooling (P5), global v6 (P6) from main's `NatReport` | S | 2 | P1, P2, P5, P6 | R4, R6, R7, R3b |
| 4 | **Wire**: the profile and history in REQUEST and READY, PLAN, OBS, ABORT `NODIRECT`, attempt and epoch tags, negotiated by a REQUEST flag; an old peer keeps today's flow and its class is logged (D6) | M | 2 | P11 | SamePlan, NoRace |
| 5 | **Session roles**: the initiator controls (D1), both compute epoch 1, epochs, the two-sided stop, re-plan on every fact, freeze on nomination; **R0** (an Anonymous-Tier identity never proposes, probes or answers with READY, PLAN or OBS); **D7**: on a shaped link PLAN and OBS ride inside the shaped frames | L | 4 | — | R0, Agreement, AnonymousNeverUpgrades, ShapedHidesSignals |
| 6 | **Driver: one technique at a time.** Technique runners for lan, v6, hair, plain, predict, predict-both, birthday, symmetric birthday. Birthday in tranches of 64 → 128 → 256 sockets within the caps of 256 / 1,750, each socket refreshed, first sends at low TTL (HyperDHT TTL 5). Remove the silence-timer escalation and the all-targets-at-once punch | L | 5 | S18 | R1–R7b, NoSpam, NoNegligibleRow |
| 7 | **Nomination**: the initiator's NOMINATE frame kind; `COMPLETE(epoch, technique)`; the interface accepts only the nominated pair | M | 5, 6 | — | Agreement (samenat-hairpin, dual stack) |
| 8 | **Learning**: per-(peer, network-epoch) facts in memory, H = 3 (D5); retry = `plan(next view) != NoDirect`; remove "any rejection but BUSY clears the retry" (`node` :1417-1421); edge drops its 1 h hold | S | 5 | S12 | Learning |
| 9 | **Session-socket port mapping** (R5) on #101's engine; PCP PEER (R5b) later on the same engine (D9) | M | 6 | P7 per session | R5 |
| 10 | **Connection reversal** (R3b) and the **static local-port guess** | S | 6 | P15, P16 | R3b |
| 11 | **Hairpin test** (P4), **carrier-side candidates and subnet comparison** (P8) | S | 4 | P4, P8 | R1, R3 |
| 12 | **Filtering test** (P3): a facilitator or #100 peer answers from another port and IP on request (RFC 5780 §4.4); RNSP grows a flag | M | 4 | P3, P15 | R3b, R4, D3 |
| 13 | **ICMP** (S8) on the session socket (`IP_RECVERR` and platform equivalents), building on main's probe handling | S | 1 | S8 | R2 |
| 14 | **Network-epoch detection** on #101's gateway-change signal: expire the profile | S | 8 | — | validity |
| 15 | **The authenticated wire** (rns-rs ≥ 0.14): HELLO in link DATA context 0x80, 52-byte HMAC-tagged frames with random nonces, RNSK keepalives; the legacy wire toward non-answering peers (leviculum#112) | L | 4 | P14 | R6b, NoReplay, NoRawToCapablePeer, the LAN leak |
| 16 | **Adaptive keepalive and refresh** from the measured binding lifetime (RFC 5780 §4.6) | S | 15 | S17 | R7 refresh, liveness |
| 17 | **Port-block and quota sizing** for the birthday spray and tranche | S | 6, 1 | P12, P13 | R7 |

Steps 1–3 can go in parallel, as can 9–14 once 6 lands. Step 15 can start
after 4; R6b waits for it.

### The LTS line (diag/punch-telemetry, for calls)

**Only step 7, nomination,** is needed there. It is being built now in
`punch3`: initiator nomination in the two-sided commit, which fixes the
samenat-hairpin split (`std` :1313, `udp` :220) and the dual-stack split.

The model also shows three LTS bugs that do not block calls:

- the retry depends on arrival order (`node` :1417-1421);
- a heard-but-not-acked random side suppresses its own birthday phase
  (`std` :964, :1170);
- a facilitator's silence aborts the session even when v6 would work
  (`core probe_failed`).

They are fixed on 0.35 by steps 5, 6 and 8; backporting them is a separate
decision.

## What the conformance does not cover

The model abstracts the mechanism into `Works`, `Hears` and `Acks`, which
are cross-checked against DirectLinkPunch for `plain`. Three things are
NOT cross-checked mechanically:

- the techniques DLP does not model (v6, map) and the lan/hair split. They
  are plain RFC 4787 reasoning;
- the timing between the two ends: `SkewOK` is lockstep, DLP's MaxSkew 0;
- that `Row`'s order is the right one for success rates. That needs the
  measurements (EVIDENCE S12, and the punch-probe runs), not TLC.
