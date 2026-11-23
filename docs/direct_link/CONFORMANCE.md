# Conformance: how the code refines Strategy.tla

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
pub fn plan(view: &View) -> Plan;

pub struct View<'a> {
    pub own: Profile,             // EVIDENCE P1..P11 for this side
    pub peer: Option<Profile>,    // from REQUEST / READY
    pub facts: &'a [Fact],        // valid facts only (the caller applies validity)
    pub tried: TechSet,           // started this attempt
    pub budget_left: u8,          // escalations left for the pair
}
pub struct Profile { mapping: Mapping, filtering: Filtering, steps: Steps,
                     global_v6: bool, router_maps: bool, reflexive: bool,
                     same_public_ip: bool, candidates: bool, may_escalate: bool }
pub enum Fact { Silent(Side, Tech), HeardOnly(Side, Tech), OtherPort(Side),
                Failed(Tech), Ok(Tech) /* .. each with its attempt */ }
pub enum Plan { Lan, V6, Hairpin, Plain, Map(Side), Predict(Side), Birthday(Side), NoDirect }
```

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

## Code changes, in order

Each step is shippable alone, and the gate stays green between steps.
Sizes are S, M or L.

| # | change | size | depends on | new instrumentation |
|---|---|---|---|---|
| 1 | **Expose the evidence live**, with no behaviour change: probe attempt and RTT (S1); `first_heard`, `first_acked`, heard-only and acked-only (S3 to S5), `HeardRelation` per source (S6, S7), knocks at the first hit (S9) and the winner's port relative to the session socket's (S10), all handed to the node as they happen, not only in the end-of-punch log | M | - | S1, S3 to S7, S9, S10 |
| 2 | **`plan()` and `size()`** in core, with `plan-vectors.jsonl` as its test; nothing calls it yet | M | - | - |
| 3 | **Wire**: the profile in REQUEST and READY, the plan in READY, PLAN and OBS, ABORT `NODIRECT`, the attempt and epoch tags, negotiated by a REQUEST flag; old peers keep today's flow | M | 2 | P11 (the gate told to the peer) |
| 4 | **Session roles**: controller and follower, epochs, the two-sided stop (`TwoSided`), re-plan on every fact, freeze on nomination; replace `Session::punched`'s single verdict | L | 2, 3 | - |
| 5 | **Driver: one technique at a time.** Split `punch_with` into technique runners (`lan`, `v6`, `hair`, `plain`, `predict`, `birthday`), started and stopped by the session's `Punch{plan}` and its successors. Remove the silence-timer escalation (`ESCALATE_AFTER`, `BIRTHDAY_AFTER` as triggers) and the all-targets-at-once punch | L | 4 | - |
| 6 | **Nomination**: a NOMINATE frame kind; `COMPLETE(epoch, technique)`; the interface accepts only the nominated pair. This fixes lit-review gap 2 and the samenat-hairpin and dual-stack splits | M | 4, 5 | - |
| 7 | **Learning**: per-(peer, network-epoch) facts in memory (never addresses, CC 2.6.8); retry = `plan(next view) != NoDirect`; remove "any rejection but BUSY clears the retry" (`node` :1417-1421); edge drops its 1 h hold for the same rule | S | 4 | S12 |
| 8 | **Router mapping of the session socket** (R5): reuse main #101's engine for the session's socket, advertise the mapped address, delete it on release | M | 5, main merge | P7 per session |
| 9 | **Classification in production**: drive P1 from main #100's distinct-IP verdict, and configure a second observer at a distinct IP in edge (edge `direct_path.rs` :451-465) | S | main merge | P1 |
| 10 | **Hairpin test** (P4) and **carrier-side candidates, subnet comparison** (P8) | S | 3 | P4, P8 |
| 11 | **Filtering test** (P3): the facilitator (or a #100 peer) answers from another port and IP on request; RNSP grows a flag | M | 3 | P3 |
| 12 | **ICMP** (S8): `IP_RECVERR` on Linux (and the platform equivalents) on the session socket | S | 1 | S8 |
| 13 | **Network-epoch detection**: route and interface changes expire the profile | S | 7 | - |

Steps 1 and 2 can go in parallel, as can 8 to 12 once 5 lands. The punch
branch must reach main first (lit-review gap 1): steps 8 and 9 need main's
#100 and #101.

## What the conformance does not cover

The model abstracts the mechanism into `Works`, `Hears` and `Acks`, which
are cross-checked against DirectLinkPunch for `plain`. Three things are
NOT cross-checked mechanically:

- the techniques DLP does not model (v6, map) and the lan/hair split. They
  are plain RFC 4787 reasoning;
- the timing between the two ends: `SkewOK` is lockstep, DLP's MaxSkew 0;
- that `Row`'s order is the right one for success rates. That needs the
  measurements (EVIDENCE S12, and the punch-probe runs), not TLC.
