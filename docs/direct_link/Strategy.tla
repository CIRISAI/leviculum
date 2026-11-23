------------------------------ MODULE Strategy ------------------------------
(***************************************************************************)
(* The STRATEGY layer of the direct-link punch: which technique to run,   *)
(* chosen from evidence, and how both ends agree on it and on the pair.   *)
(*                                                                         *)
(* DirectLinkPunch.tla models the MECHANISM (sockets, NAT mappings,       *)
(* filters, tokens, the facilitator, a stranger). This module sits above *)
(* it: a technique is one configuration of that mechanism (see            *)
(* STRATEGY.md, "Techniques as DirectLinkPunch configurations"), and its  *)
(* outcome per NAT pair is the oracle `Works`/`Hears`/`Acks` below,       *)
(* derived from RFC 4787 and cross-checked against DirectLinkPunch runs   *)
(* (run_strategy.py --oracle).                                             *)
(*                                                                         *)
(* It formalizes established practice: RFC 4787 behaviours, RFC 5128     *)
(* techniques, RFC 8445's controlling agent and nomination, the shipped   *)
(* systems in SURVEY.md and NATCracker's pair matrix; REFERENCES.md      *)
(* classes every row and property by its source.                          *)
(*                                                                         *)
(* Two planners share one environment, one network and one evidence       *)
(* store:                                                                  *)
(*   Planner = "strategy"  the design: the initiator (ICE's controlling   *)
(*                         agent) runs Row(evidence), one technique at a  *)
(*                         time, above a likelihood floor; both ends     *)
(*                         compute epoch 1 from the exchanged profiles;   *)
(*                         the controller re-plans in the step a fact     *)
(*                         arrives and nominates the pair in-band;        *)
(*   Planner = "today"     the code at diag/punch-telemetry: every target *)
(*                         at once, escalation on the silence timer, and  *)
(*                         each side settles on the first heard+acked.   *)
(* The frame wire is a parameter: raw-token frames (ours today, rns-rs    *)
(* <= 0.13) or HMAC frames negotiated by HELLO (rns-rs >= 0.14).           *)
(*                                                                         *)
(* Evidence only grows: `ev[s]` is a set of facts, each stamped with the  *)
(* attempt it was seen in; a fact is superseded (never deleted) and its   *)
(* validity is `Valid` (this attempt only, or Hist attempts more).        *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, Sequences, TLC

CONSTANTS
    \* ---- the environment: the truth, which neither end sees directly ----
    MapA, MapB,          \* "EIM" | "EDMseq" | "EDMrand"      (RFC 4787 4.1)
    FiltA, FiltB,        \* "EIF" | "ADF" | "APDF"             (RFC 4787 5)
    EimSteps,            \* "seq" | "rand": how an EIM NAT allocates between
                         \* SOCKETS (what own allocation probes see)
    V6A, V6B,            \* the side has a global IPv6 host address
    V6Open,              \* both v6 stateful firewalls pass a simultaneous punch
    Topo,                \* "apart" | "sameLAN" | "sameCGN" (one carrier NAT)
    Hairpin,             \* the shared NAT loops public -> public back
    CgnCand,             \* sameCGN: both know carrier-side candidates
    UpnpA, UpnpB,        \* the side's router maps the session socket (PCP /
                         \* NAT-PMP / UPnP-IGD) and is the outermost NAT
    FacA, FacB,          \* the side's own facilitator answers its probe
    Classify,            \* two observers at DISTINCT IPs answer (main #100)
    FiltKnown,           \* a filtering test exists (not in any code today)
    Luck,                \* "any" | "hit" | "miss": oracle for map/pred/bday
    \* ---- parameters kept from DirectLinkPunch ----
    EscA, EscB,          \* the trust gate's verdict per side (DLP Escalates)
    PairBudget,          \* escalations one pair may start, all attempts
    Stranger,            \* a stranger holds a port on a knocked IP
    HijackPlant,         \* knocks carry the token (72be0f9)
    NamedFacilitatorPlant, \* the responder probes the facilitator the
                         \* initiator named
    \* ---- faults ----
    RelayLossy,          \* signals may be lost (the relay is a set: always
                         \* reordered)
    NetLossy,            \* datagrams may be lost
    PathLoss,            \* an established direct path may die later
    \* ---- the planner, a plant of this layer, bounds ----
    Planner,             \* "strategy" | "today"
    UntaggedPlant,       \* signals carry no attempt tag
    MaxAtt, Hist, StopT, LimT, W, EscAt, CostCap,
    \* ---- frame authentication and capability (rns-rs >= 0.14 wire) ----
    HelloA, HelloB,      \* the side speaks the HMAC wire and answers HELLO
                         \* (rns-rs docs/direct-link-protocol.md; survey 8)
    DowngradePlant,      \* sends raw-token frames to a peer that showed HELLO
    OnPath,              \* an observer on the path sees every frame (RFC 8445
                         \* 19.1 threat model; RFC 7675 3)
    LanStranger,         \* a host on this side's LAN holds the private address
                         \* a peer behind ANOTHER NAT advertised (lit. review 11)
    \* ---- evidence about the hard side's allocator, and the ramp ----
    PortBlock,           \* its ports cluster in a known block (RFC 7422, RFC 7768;
                         \* Richter 2016; Keller 2022)
    Tranches             \* birthday tranches (64, 128, 256 sockets x 1,750 knocks)

Sides == {"A", "B"}
Other(s) == IF s = "A" THEN "B" ELSE "A"
Map(s)  == IF s = "A" THEN MapA ELSE MapB
Filt(s) == IF s = "A" THEN FiltA ELSE FiltB
V6(s)   == IF s = "A" THEN V6A ELSE V6B
Upnp(s) == IF s = "A" THEN UpnpA ELSE UpnpB
Fac(s)  == IF s = "A" THEN FacA ELSE FacB
Esc(s)  == IF s = "A" THEN EscA ELSE EscB
OwnFac(s) == "F" \o s
Facs == {"FA", "FB"}
\* What own allocation probes (std allocation_steps) see between sockets.
Steps(s) == CASE Map(s) = "EDMrand" -> "rand"
              [] Map(s) = "EDMseq"  -> "seq"
              [] OTHER              -> EimSteps

-----------------------------------------------------------------------------
(* Techniques. The parameter in a name is the side it is about: the side  *)
(* that maps (map), whose ports are predicted (pred), that is random and  *)
(* holds the birthday sockets (bday).                                      *)
Techs == {"lan", "v6", "hair", "rev", "plain", "mapA", "mapB",
          "predA", "predB", "predAB", "bdayA", "bdayB"}
BdayTechs == {"bdayA", "bdayB"}
Paths == Techs \cup {"stray"}        \* "stray": an answer from the stranger
Rank(t) == CASE t = "lan" -> 1 [] t = "v6" -> 2 [] t = "hair" -> 3 [] t = "rev" -> 4
             [] t = "plain" -> 5 [] t = "mapA" -> 6 [] t = "mapB" -> 7
             [] t = "predA" -> 8 [] t = "predB" -> 9 [] t = "predAB" -> 10
             [] t = "bdayA" -> 11 [] t = "bdayB" -> 12
\* Cost: datagrams and held state per attempt, in units (STRATEGY.md).
Cost(t) == CASE t \in {"lan", "v6", "rev"} -> 1 [] t \in {"hair", "plain"} -> 2
             [] t \in {"mapA", "mapB"} -> 3 [] t \in {"predA", "predB", "predAB"} -> 5
             [] OTHER -> 12
Side(t) == IF t \in {"mapA", "predA", "bdayA"} THEN "A" ELSE "B"
Escalated == {"predA", "predB", "predAB", "bdayA", "bdayB"}
HoldsRes == {"mapA", "mapB", "bdayA", "bdayB"}  \* a router lease; sockets+permits
\* The resources side s holds while technique t runs.
Acquire(s, t) == IF t \in {"mapA", "mapB"} THEN (IF s = Side(t) THEN {t} ELSE {})
                 ELSE IF t \in {"bdayA", "bdayB"} THEN {t} ELSE {}
\* Which side sends knocks (to ports that may be anyone's) for t.
Knocks(s, t) == t \in Escalated /\ (t = "predAB" \/ s /= Side(t))

RECURSIVE SumCost(_)
SumCost(S) == IF S = {} THEN 0
              ELSE LET t == CHOOSE x \in S : TRUE IN Cost(t) + SumCost(S \ {t})

-----------------------------------------------------------------------------
(* The physics: what a technique can do between this pair of NATs.        *)
SamePub == Topo /= "apart"
LanWorks == Topo = "sameLAN" \/ (Topo = "sameCGN" /\ CgnCand)
FacBoth == FacA /\ FacB
EIM(s) == Map(s) = "EIM"
Loose(s) == Filt(s) \in {"EIF", "ADF"}
\* An EDM NAT with endpoint-independent filtering admits a packet from a
\* remote it never sent to, and the flow that packet opens carries this
\* side's replies to that remote from the port it arrived at (the
\* DirectLinkPunch cross-check found this; STRATEGY.md, "The plain-punch
\* oracle"). Toward an EDM peer that always happens (its punches come
\* from a port this side never sent to); toward an EIM peer it is a race
\* with this side's own first punch, which makes a new mapping: `race`.
VARIABLE race
OpenEDM(e) == ~EIM(e) /\ Filt(e) = "EIF"
Reuse(e) == OpenEDM(e) /\ (~EIM(Other(e)) \/ race = "reuse")
EffEIM(s) == EIM(s) \/ Reuse(s)
RaceMatters == \E e \in Sides : OpenEDM(e) /\ EIM(Other(e))
\* "plain": tokened punches at the reflexive address, plus every
\* peer-reflexive source that proved the token (std punch_inner), after
\* that learning has run. HPub(s): s hears the peer's punches; APub(s): an
\* ack reaches s from an address s punched (only those count). Derived in
\* STRATEGY.md "The plain-punch oracle" and checked against
\* DirectLinkPunch with escalation denied (run_strategy.py --oracle).
HPub(s) == LET o == Other(s) IN
    CASE EffEIM(s) /\ EffEIM(o) -> TRUE
      [] EffEIM(s)              -> Loose(s)
      [] EffEIM(o)              -> Loose(o) \/ Filt(s) = "EIF"
      [] OTHER                  -> FALSE
APub(s) == LET o == Other(s) IN
    CASE EffEIM(o) -> HPub(o)
      [] EffEIM(s) -> HPub(o) /\ HPub(s)
      [] OTHER     -> FALSE
\* s hears the peer from the peer's public IP on a port it never
\* advertised: the peer's mapping moved (std heard_relation
\* SameIpOtherPort).
OtherPortSeen(s) == HPub(s) /\ ~EffEIM(Other(s))

VARIABLE luck   \* the oracle's draw for map / pred / bday, fixed at Init
\* The wire in use: HMAC frames when both sides showed the capability (HELLO)
\* and nothing downgrades; else the raw-token frames of rns-rs <= 0.13.
Wire == IF HelloA /\ HelloB /\ ~DowngradePlant THEN "mac" ELSE "raw"

VARIABLES
    att,        \* the attempt, 1..MaxAtt
    ini,        \* this attempt's initiator: the controller
    ph,         \* per side: idle gather wait run nom cwait direct relayed
    knows,      \* per side: whose probe profile it holds (monotone)
    ev,         \* per side: the facts it holds (monotone)
    ep,         \* per side: the plan epoch it runs
    ts,         \* per side: the techniques it is running (a set)
    el,         \* per side: ticks in the current epoch / phase
    res,        \* per side: techniques whose resources it holds
    commit,     \* per side: "-" or the path it committed to
    msgs,       \* signals in flight on the relay (a set: reordered)
    escUsed,    \* escalations started, all attempts
    tried,      \* techniques started this attempt
    first,      \* the first technique of this attempt
    ifail,      \* the initiator's first failure this attempt (today's retry)
    retry,      \* "-" | "yes" | "no": propose again after this attempt
    facSaw,     \* per facilitator: who probed it
    strangerSaw,\* what reached the stranger: "K" | "tokened"
    relaySaw,   \* signal types the relay carried
    fooled      \* per side: its liveness was refreshed by a replayed frame

vars == <<att, ini, ph, knows, ev, ep, ts, el, res, commit, msgs, escUsed,
          tried, first, ifail, retry, luck, race, facSaw, strangerSaw, relaySaw, fooled>>

\* The initiator controls (decided: initiator nomination in the two-sided
\* commit, ICE-style, RFC 8445 6.1.1 "controlling agent" and 8.1.1); the
\* responder follows.
Ctl == ini
Fol == Other(Ctl)
Deciders == IF Planner = "strategy" THEN {Ctl} ELSE Sides
\* The tranche a birthday runs at in attempt n (STRATEGY.md R7), capped at
\* Tranches: one more than the earlier attempts it failed in.
TrancheAt(t, n) ==
    LET k == 1 + Cardinality({f.n : f \in {g \in ev[Ctl] \cup ev[Fol] : g.k = "failed" /\ g.t = t /\ g.n < n}})
    IN IF k > Tranches THEN Tranches ELSE k

WorksAt(t, n) ==
    CASE t = "lan"   -> SamePub /\ LanWorks
      [] t = "v6"    -> V6A /\ V6B /\ V6Open
      [] t = "hair"  -> SamePub /\ Hairpin /\ FacBoth
      [] t = "plain" -> ~SamePub /\ FacBoth /\ \A s \in Sides : HPub(s) /\ APub(s)
      [] t \in {"mapA", "mapB"} -> Upnp(Side(t)) /\ luck["map"]
      [] t = "rev"   -> ~SamePub /\ FacBoth /\ \E x \in Sides : EIM(x) /\ Filt(x) = "EIF"
      [] t \in {"predA", "predB"} ->
            ~SamePub /\ FacBoth /\ Map(Side(t)) = "EDMseq" /\ EIM(Other(Side(t))) /\ luck["pred"]
      \* DirectLinkPunch matrix.md mac-EDMseq-*: punchable only with MAC'd
      \* frames, and not with strict filtering on both sides
      [] t = "predAB" ->
            ~SamePub /\ FacBoth /\ MapA = "EDMseq" /\ MapB = "EDMseq" /\ Wire = "mac"
            /\ ~(FiltA = "APDF" /\ FiltB = "APDF") /\ luck["pred"]
      [] t \in {"bdayA", "bdayB"} ->
            /\ ~SamePub /\ FacBoth /\ Map(Side(t)) = "EDMrand"
            /\ \/ EIM(Other(Side(t))) /\ luck["bday"] <= TrancheAt(t, n)
               \* both random: reachable at the mechanism model's NP=4 with
               \* MAC'd frames (matrix.md mac-EDMrand-*), negligible in 16 bits
               \/ Map(Other(Side(t))) = "EDMrand" /\ Wire = "mac" /\ luck["hh"]
      [] OTHER -> FALSE
Works(t) == WorksAt(t, att)
Hears(s, t) == IF t = "plain" THEN ~SamePub /\ FacBoth /\ HPub(s) ELSE Works(t)
Acks(s, t)  == IF t = "plain" THEN ~SamePub /\ FacBoth /\ APub(s) ELSE Works(t)

-----------------------------------------------------------------------------

-----------------------------------------------------------------------------
(* Evidence.                                                               *)
Fact(k, x, t, n) == [k |-> k, x |-> x, t |-> t, n |-> n]
\* Raw observations live one attempt; inferences and outcomes are history
\* for Hist attempts more (EVIDENCE.md gives each kind's validity).
AttemptScoped == {"heard", "acked", "peerDone", "silent"}
Valid(f, n) == IF f.k \in AttemptScoped THEN f.n = n ELSE f.n <= n /\ n - f.n <= Hist
ValidAt(F, n) == {f \in F : Valid(f, n)}
HasF(F, k, x, t) == \E f \in F : f.k = k /\ (x = "*" \/ f.x = x) /\ (t = "*" \/ f.t = t)
Now(s) == ValidAt(ev[s], att)
Heard(s, t) == HasF(Now(s), "heard", s, t)
AckedF(s, t) == HasF(Now(s), "acked", s, t)
HA(s, t) == Heard(s, t) /\ AckedF(s, t)
\* What the controlled side reports (OBS): its asymmetric partials and its
\* verdict on the technique ("silent": its stop condition fired).
Decisive == {"otherPort", "heardOnly", "silent"}
HistKinds == {"otherPort", "heardOnly", "failed", "ok"}
Carry(F) == {f \in ValidAt(F, att) : f.k \in HistKinds}

\* A view is (kn, F): the profiles held and the VALID facts held.
Kn(kn, x) == x \in kn
KAB(kn) == Sides \subseteq kn
EDMs == {"EDMseq", "EDMrand", "EDM"}
\* The mapping class as the evidence says: the classifier (two observers at
\* distinct IPs) when it ran; else a secondary fact that the side's mapping
\* moved (a peer saw it on another port, or it heard but was never acked),
\* refined by its own allocation steps; else unknown.
MapV(kn, F, x) ==
    IF ~Kn(kn, x) THEN "unk"
    ELSE IF Classify THEN Map(x)
    ELSE IF HasF(F, "otherPort", x, "*") \/ HasF(F, "heardOnly", x, "*")
         THEN CASE Steps(x) = "seq" -> "EDMseq" [] Steps(x) = "rand" -> "EDMrand"
                [] OTHER -> "EDM"
    ELSE "unk"
\* Filtering as the evidence says: a filtering test (none in code), or the
\* inference from the PEER having heard but never been acked on the plain
\* punch: its acks die at this side's filter.
FiltV(kn, F, x) ==
    IF FiltKnown /\ Kn(kn, x) THEN Filt(x)
    ELSE IF HasF(F, "heardOnly", Other(x), "plain") THEN "APDF"
    ELSE "unk"
Gate(kn, x) == Kn(kn, x) /\ Esc(x)
\* The frame wire both ends have shown they speak (rns-rs >= 0.14 HELLO and
\* HMAC frames; survey section 8). Learned with the profile: AuthV.
AuthV(kn) == KAB(kn) /\ HelloA /\ HelloB
\* Birthday tranches (Eric: 256 sockets / 1,750 knocks are CAPS, ramped in
\* tranches, retries escalating within 3 per pair): the tranche a birthday
\* runs at in attempt n is one more than the earlier attempts it failed in.
Tranche(t, F, n) == 1 + Cardinality({f.n : f \in {g \in F : g.k = "failed" /\ g.t = t /\ g.n < n}})
\* A technique has failed for attempt n: in attempt n itself; or, within
\* its validity, in an earlier attempt, except that a birthday may run
\* again at its next tranche until Tranches have failed.
Failed(F, n) ==
    {t \in Techs : \E f \in F : f.k = "failed" /\ f.t = t
                      /\ (f.n = n \/ t \notin BdayTechs \/ Tranche(t, F, n) > Tranches)}
\* Own-evidence fallback for "random": exactly one side shows random
\* allocation steps, its class is unknown, and the plain punch has failed.
RandSide(kn, F, n, e) ==
    \/ MapV(kn, F, e) = "EDMrand"
    \/ /\ MapV(kn, F, e) = "unk" /\ Kn(kn, e) /\ Steps(e) = "rand"
       /\ "plain" \in Failed(F, n)
       /\ ~(Kn(kn, Other(e)) /\ Steps(Other(e)) = "rand" /\ MapV(kn, F, Other(e)) = "unk")

\* Pre(t): given the view, t MAY succeed (its sufficiency precondition,
\* from the physics: reachability), and the gate and the budget allow it.
Pre(kn, F, tr, n, t) ==
    LET K == KAB(kn)
        m(x) == MapV(kn, F, x)
        f(x) == FiltV(kn, F, x)
        facb == K /\ FacBoth
        bud == escUsed < PairBudget \/ t \in tr
    IN CASE t = "lan"   -> K /\ SamePub /\ LanWorks
         [] t = "v6"    -> K /\ V6A /\ V6B
         [] t = "hair"  -> K /\ SamePub /\ facb
         \* RFC 5128 3.2 connection reversal: one side is reachable by anyone
         \* (endpoint-independent mapping AND filtering, RFC 4787 4.1, 5)
         [] t = "rev"   -> K /\ ~SamePub /\ facb /\ \E x \in Sides : m(x) = "EIM" /\ f(x) = "EIF"
         [] t = "plain" -> K /\ ~SamePub /\ facb
                           \* impossible only if both are EDM and neither may
                           \* filter openly, or an EDM that may not filter
                           \* openly faces a strict filter
                           /\ ~(\A e \in Sides : m(e) \in EDMs /\ f(e) \notin {"EIF", "unk"})
                           /\ ~(\E e \in Sides : m(e) \in EDMs /\ f(e) \notin {"EIF", "unk"}
                                               /\ f(Other(e)) = "APDF")
         [] t \in {"mapA", "mapB"} -> K /\ Upnp(Side(t))
         [] t \in {"predA", "predB"} ->
               LET e == Side(t) IN
               K /\ ~SamePub /\ facb /\ m(e) = "EDMseq" /\ m(Other(e)) \in {"EIM", "unk"}
               /\ Gate(kn, Other(e)) /\ bud
         \* both sequential: authenticated punches to each other's predicted
         \* ports (NATCracker 5.8 "interleaved prediction"); tokens may not go
         \* to ports that may be anyone's, so only on the authenticated wire
         [] t = "predAB" ->
               K /\ ~SamePub /\ facb /\ m("A") = "EDMseq" /\ m("B") = "EDMseq" /\ AuthV(kn)
               /\ ~(f("A") = "APDF" /\ f("B") = "APDF")
               /\ Gate(kn, "A") /\ Gate(kn, "B") /\ bud
         [] t \in {"bdayA", "bdayB"} ->
               LET e == Side(t) IN
               K /\ ~SamePub /\ facb /\ RandSide(kn, F, n, e)
               \* the peer EIM or unknown; or (reachable only, on the
               \* authenticated wire) random too: the squared model
               /\ (m(Other(e)) \in {"EIM", "unk"} \/ (AuthV(kn) /\ m(Other(e)) = "EDMrand"))
               /\ Gate(kn, e) /\ Gate(kn, Other(e)) /\ bud
         [] OTHER -> FALSE

(***************************************************************************)
(* Likelihood: reachability is not likelihood. TLC's nondeterministic port *)
(* draws show that an outcome is REACHABLE; the expected success comes    *)
(* from the cited arithmetic (STRATEGY.md section 3, the quantitative     *)
(* column). Classes: certain (>= 95%), high (50-95%), low (1-50%),        *)
(* negligible (< 1%). The FLOOR: no plan uses a negligible technique.     *)
(***************************************************************************)
Likely(kn, F, n, t) ==
    LET m(x) == MapV(kn, F, x) IN
    CASE t = "lan"   -> "certain"      \* RFC 8445 5.1.1.1 host candidates, one LAN
      [] t = "v6"    -> "high"         \* RFC 6092 REC-11/14; unmeasured here
      [] t = "hair"  -> "low"          \* Ford 2005: hairpin UDP 24% of NATs
      [] t = "rev"   -> "certain"      \* RFC 5128 3.2: one side reachable
      [] t = "plain" -> "high"         \* Ford 2005 82%; Trautwein 2025/26 70%
      [] t \in {"mapA", "mapB"} -> "high"   \* RFC 6886/6887; unmeasured here
      [] t \in {"predA", "predB"} -> "high" \* Keller 2022 DEVM/DPSM 76-78%
      [] t = "predAB" -> "low"         \* NATCracker 5.9.3: PRC-PRC "drops significantly"
      [] t \in {"bdayA", "bdayB"} ->
            IF m(Other(Side(t))) \in EDMs THEN "negligible"
                \* both hard: 1-exp(-N*M/65536^2) = 0.003% at 256 x 1,750 (survey 4)
            ELSE IF PortBlock THEN "certain"   \* inside a 1K CGN chunk: ~100% (survey 4)
            ELSE IF Tranche(t, F, n) = 1 THEN "high"
                \* 64 sockets x 1,750 knocks over 64,512 ports: 82%
            ELSE "certain"                     \* 128 x 1,750: 97%; 256 x 1,750: 99.9%
      [] OTHER -> "negligible"
AboveFloor(kn, F, n, t) == Likely(kn, F, n, t) /= "negligible"
Avail(kn, F, tr, n, t) == Pre(kn, F, tr, n, t) /\ t \notin Failed(F, n) /\ AboveFloor(kn, F, n, t)
\* The physics' answer: the cheapest technique that may still work and is
\* above the floor.
Cheapest(kn, F, tr, n) ==
    LET av == {t \in Techs : Avail(kn, F, tr, n, t)} IN
    IF av = {} THEN "none" ELSE CHOOSE t \in av : \A u \in av : Rank(t) <= Rank(u)

(***************************************************************************)
(* THE DECISION TABLE (STRATEGY.md), row by row, in its own words. This   *)
(* is the planner the code is to implement (CONFORMANCE.md: `plan()`).     *)
(* TLC checks it against Cheapest (PlanIsCheapest) on every view reached. *)
(***************************************************************************)
Row(kn, F, tr, n) ==
    LET K         == KAB(kn)
        fl        == Failed(F, n)
        m(x)      == MapV(kn, F, x)
        f(x)      == FiltV(kn, F, x)
        untried(t) == t \notin fl
        sameNet   == K /\ SamePub                   \* R1, R3: one public IP
        haveCands == K /\ LanWorks                  \* R1: host / carrier candidates
        bothV6    == K /\ V6A /\ V6B                \* R2
        reflexive == K /\ FacA /\ FacB              \* both probes answered
        reachable == \E x \in Sides : m(x) = "EIM" /\ f(x) = "EIF"   \* R3b
        edm(x)    == m(x) \in EDMs
        strict(x) == f(x) = "APDF"
        mayOpen(x) == f(x) \in {"EIF", "unk"}   \* endpoint-independent filtering not ruled out
        closedEdm(x) == edm(x) /\ ~mayOpen(x)
        plainCan  == ~(closedEdm("A") /\ closedEdm("B"))
                     /\ ~(closedEdm("A") /\ strict("B")) /\ ~(closedEdm("B") /\ strict("A"))
        budget(t) == escUsed < PairBudget \/ t \in tr
        seqSide   == {e \in Sides : m(e) = "EDMseq" /\ m(Other(e)) \in {"EIM", "unk"}
                                    /\ Gate(kn, Other(e)) /\ untried("pred" \o e)
                                    /\ budget("pred" \o e)}
        bothSeq   == m("A") = "EDMseq" /\ m("B") = "EDMseq" /\ AuthV(kn)
                     /\ ~(strict("A") /\ strict("B")) /\ Gate(kn, "A") /\ Gate(kn, "B")
                     /\ untried("predAB") /\ budget("predAB")
        \* one random side against an EIM or unknown one; never both random
        \* (negligible: the floor), even where the authenticated wire makes
        \* it reachable
        randSide  == {e \in Sides : RandSide(kn, F, n, e) /\ m(Other(e)) \in {"EIM", "unk"}
                                    /\ Gate(kn, e) /\ Gate(kn, Other(e))
                                    /\ untried("bday" \o e) /\ budget("bday" \o e)}
        pick(S)   == IF "A" \in S THEN "A" ELSE "B"
    IN
    \* R1 same network: host or carrier candidates only, never the hairpin
    IF sameNet /\ haveCands /\ untried("lan") THEN "lan"
    \* R2 both global v6: a stateful-firewall punch on v6 only
    ELSE IF bothV6 /\ untried("v6") THEN "v6"
    \* R3 one public IP, no candidates (or they failed): the hairpin
    ELSE IF sameNet /\ reflexive /\ untried("hair") THEN "hair"
    \* R3b one side reachable by anyone: connection reversal (RFC 5128 3.2)
    ELSE IF ~sameNet /\ reflexive /\ reachable /\ untried("rev") THEN "rev"
    \* R4 apart, and the classes allow it: a plain simultaneous punch
    ELSE IF ~sameNet /\ reflexive /\ plainCan /\ untried("plain") THEN "plain"
    \* R5 a router maps the session socket: map it, then punch plain
    ELSE IF K /\ UpnpA /\ untried("mapA") THEN "mapA"
    ELSE IF K /\ UpnpB /\ untried("mapB") THEN "mapB"
    \* R6 one side sequential EDM: knock its predicted ports
    ELSE IF ~sameNet /\ reflexive /\ seqSide /= {} THEN "pred" \o pick(seqSide)
    \* R6b both sequential, authenticated wire, not both strict: interleaved
    ELSE IF ~sameNet /\ reflexive /\ bothSeq THEN "predAB"
    \* R7 one side random EDM: birthday (sockets there, spray here), by tranche
    ELSE IF ~sameNet /\ reflexive /\ randSide /= {} THEN "bday" \o pick(randSide)
    \* R8 nothing can work above the floor: no direct path, at once (#1024)
    ELSE "none"

-----------------------------------------------------------------------------
(* Today's code, as a planner: every target at once, then escalation on   *)
(* the silence timer (std punch_inner / punch_birthday, core session).    *)
TodayB(x) == IF Classify THEN Map(x) = "EDMrand" ELSE Steps(x) = "rand"  \* "b" (std decide)
TodayQ(x) == Classify /\ Map(x) = "EDMseq"                              \* "q" (std advertise)
TodayBase(s) ==
    (IF SamePub THEN {"lan", "hair"} ELSE {"plain"})       \* core same_nat_alternates
    \cup (IF V6(Other(s)) THEN {"v6"} ELSE {})             \* global v6 always offered
TodayEsc(s) ==
    LET o == Other(s) IN
    IF ~Esc(s) \/ escUsed >= PairBudget THEN {}
    ELSE (IF TodayQ(o) THEN {"pred" \o o} ELSE {})          \* knock its predicted ports
         \cup (IF TodayB(s) THEN {"bday" \o s} ELSE {})     \* hold birthday sockets
         \cup (IF TodayB(o) THEN {"bday" \o o} ELSE {})     \* spray its IP

\* The paths side s takes part in: what it runs, and (today) the predicted
\* path a peer knocks open toward it, which its own punches then use.
Lit(s) == ts[s] \cup {t \in ts[Other(s)] : t \in {"predA", "predB"} /\ Side(t) = s}
\* The peer still punches t (or keeps it alive) / still acks on t.
PeerSends(s, t) ==
    LET o == Other(s) IN
    \/ ph[o] = "run" /\ t \in Lit(o)
    \/ ph[o] \in {"nom", "cwait", "direct"} /\ commit[o] = t
CommitTech(s) == commit[s]

-----------------------------------------------------------------------------
Msg(ty, fr, e, t, f) == [ty |-> ty, fr |-> fr, n |-> att, e |-> e, t |-> t, f |-> f]
Tag(m) == UntaggedPlant \/ m.n = att
ProbeTarget(s) == IF s /= ini /\ NamedFacilitatorPlant THEN OwnFac(ini) ELSE OwnFac(s)
\* luck.bday is the tranche at which the birthday hits (Tranches + 1: never).
LuckSet ==
    {l \in [{"map", "pred", "hh"} -> BOOLEAN] \X (1..(Tranches + 1)) :
        CASE Luck = "hit"  -> (\A k \in DOMAIN l[1] : l[1][k]) /\ l[2] = 1
          [] Luck = "miss" -> (\A k \in DOMAIN l[1] : ~l[1][k]) /\ l[2] = Tranches + 1
          [] OTHER         -> TRUE}
LuckOf(l) == [map |-> l[1]["map"], pred |-> l[1]["pred"], hh |-> l[1]["hh"], bday |-> l[2]]

Init ==
    /\ att = 1
    /\ ini \in Sides
    /\ ph = [s \in Sides |-> IF s = ini THEN "gather" ELSE "idle"]
    /\ knows = [s \in Sides |-> {}]
    /\ ev = [s \in Sides |-> {}]
    /\ ep = [s \in Sides |-> 0]
    /\ ts = [s \in Sides |-> {}]
    /\ el = [s \in Sides |-> 0]
    /\ res = [s \in Sides |-> {}]
    /\ commit = [s \in Sides |-> "-"]
    /\ msgs = {}
    /\ escUsed = 0
    /\ tried = {}
    /\ first = "-"
    /\ ifail = "-"
    /\ retry = "-"
    /\ luck \in {LuckOf(l) : l \in LuckSet}
    /\ race \in (IF RaceMatters THEN {"reuse", "new"} ELSE {"new"})
    /\ facSaw = [f \in Facs |-> {}]
    /\ strangerSaw = {}
    /\ relaySaw = {}
    /\ fooled = [s \in Sides |-> FALSE]

-----------------------------------------------------------------------------
\* The two-sided stop: the current technique has FAILED once the controller
\* holds both sides' verdicts on it ("silent" from each), so its re-plan
\* sees the peer's partials too (the peer heard but was never acked, the
\* peer saw this side's port move).
TwoSided(c, F) ==
    {Fact("failed", c, t, att) :
        t \in {u \in ts[c] : HasF(ValidAt(F, att), "silent", c, u)
                             /\ HasF(ValidAt(F, att), "silent", Other(c), u)}}

(* The controller re-plans on its fact set F2 (after this step), sending  *)
(* `base` plus whatever the new plan needs. In the step the fact arrives: *)
(* PlanFollowsEvidence.                                                    *)
Decide(c, F2, base) ==
    LET p == Row(knows[c], ValidAt(F2, att), tried, att) IN
    IF ts[c] = {p} THEN
        /\ msgs' = base
        /\ UNCHANGED <<ph, ep, ts, el, res, tried, first, escUsed, relaySaw, fooled>>
    ELSE IF p = "none" THEN
        /\ ph' = [ph EXCEPT ![c] = "relayed"]
        /\ ts' = [ts EXCEPT ![c] = {}]
        /\ res' = [res EXCEPT ![c] = {}]
        /\ msgs' = base \cup {Msg("ABORT", c, ep[c], "NODIRECT", Carry(F2))}
        /\ relaySaw' = relaySaw \cup {"ABORT"}
        /\ UNCHANGED <<ep, el, tried, first, escUsed, fooled>>
    ELSE
        /\ ep' = [ep EXCEPT ![c] = @ + 1]
        /\ ts' = [ts EXCEPT ![c] = {p}]
        /\ el' = [el EXCEPT ![c] = 0]
        /\ res' = [res EXCEPT ![c] = Acquire(c, p)]
        /\ tried' = tried \cup {p}
        /\ escUsed' = escUsed + (IF p \in Escalated THEN 1 ELSE 0)
        /\ msgs' = base \cup {Msg("PLAN", c, ep[c] + 1, p,
                                  {f \in F2 : f.n = att /\ f.k \in HistKinds})}
        /\ relaySaw' = relaySaw \cup {"PLAN"}
        /\ UNCHANGED <<ph, first, fooled>>

\* The probe: own facilitator only (CC 5.4.6), which teaches the side its
\* own profile. The initiator then sends REQUEST (its profile, its history);
\* the responder, holding both, plans epoch 1 and sends it in READY.
Probe(s) ==
    /\ ph[s] = "gather"
    /\ LET fz == ProbeTarget(s)
           kn == knows[s] \cup {s}
       IN
       /\ knows' = [knows EXCEPT ![s] = kn]
       /\ facSaw' = [facSaw EXCEPT ![fz] = @ \cup {s}]
       /\ IF s = ini THEN
             /\ IF Planner = "today" /\ ~Fac(s)
                THEN \* core probe_failed: the initiator never proposes
                     /\ ph' = [ph EXCEPT ![s] = "relayed"]
                     /\ ifail' = "probe"
                     /\ UNCHANGED <<msgs, relaySaw, fooled>>
                ELSE /\ ph' = [ph EXCEPT ![s] = "wait"]
                     /\ msgs' = msgs \cup {Msg("REQ", s, 0, "-", Carry(ev[s]))}
                     /\ relaySaw' = relaySaw \cup {"REQ"}
                     /\ UNCHANGED ifail
             /\ UNCHANGED <<ep, ts, el, res, tried, first, escUsed, fooled>>
          ELSE IF Planner = "today" THEN
             IF ~Fac(s) THEN \* REJECT UNSUPPORTED
                /\ ph' = [ph EXCEPT ![s] = "relayed"]
                /\ msgs' = msgs \cup {Msg("ABORT", s, 0, "UNSUP", Carry(ev[s]))}
                /\ relaySaw' = relaySaw \cup {"ABORT"}
                /\ UNCHANGED <<ep, ts, el, res, tried, first, escUsed, ifail, fooled>>
             ELSE
                /\ ph' = [ph EXCEPT ![s] = "run"]
                /\ ts' = [ts EXCEPT ![s] = TodayBase(s)]
                /\ el' = [el EXCEPT ![s] = 0]
                /\ tried' = tried \cup TodayBase(s)
                /\ msgs' = msgs \cup {Msg("READY", s, 0, "-", Carry(ev[s]))}
                /\ relaySaw' = relaySaw \cup {"READY"}
                /\ UNCHANGED <<ep, res, escUsed, ifail, first, fooled>>
          ELSE
             LET p == Row(kn, Now(s), {}, att) IN
             IF p = "none" THEN \* "no direct path", at once
                /\ ph' = [ph EXCEPT ![s] = "relayed"]
                /\ msgs' = msgs \cup {Msg("ABORT", s, 0, "NODIRECT", Carry(ev[s]))}
                /\ relaySaw' = relaySaw \cup {"ABORT"}
                /\ UNCHANGED <<ep, ts, el, res, tried, first, escUsed, ifail, fooled>>
             ELSE
                \* the follower starts the plan it computed; the controller,
                \* computing the same, accounts for it (tried, budget)
                /\ ph' = [ph EXCEPT ![s] = "run"]
                /\ ep' = [ep EXCEPT ![s] = 1]
                /\ ts' = [ts EXCEPT ![s] = {p}]
                /\ el' = [el EXCEPT ![s] = 0]
                /\ res' = [res EXCEPT ![s] = Acquire(s, p)]
                /\ msgs' = msgs \cup {Msg("READY", s, 1, p, Carry(ev[s]))}
                /\ relaySaw' = relaySaw \cup {"READY"}
                /\ UNCHANGED <<ifail, tried, first, escUsed>>
    /\ UNCHANGED <<att, ini, ev, commit, retry, luck, race, strangerSaw, fooled>>

RecvReqOK(s, m) == s /= ini /\ ph[s] = "idle" /\ m.ty = "REQ" /\ m.fr = ini /\ Tag(m)
RecvReq(s, m) ==
    /\ m \in msgs /\ RecvReqOK(s, m)
    /\ msgs' = msgs \ {m}
    /\ knows' = [knows EXCEPT ![s] = @ \cup {m.fr}]
    /\ ev' = [ev EXCEPT ![s] = @ \cup m.f]
    /\ ph' = [ph EXCEPT ![s] = "gather"]
    /\ UNCHANGED <<att, ini, ep, ts, el, res, commit, escUsed, tried, first, ifail,
                   retry, luck, race, facSaw, strangerSaw, relaySaw, fooled>>

RecvReadyOK(s, m) == s = ini /\ ph[s] = "wait" /\ m.ty = "READY" /\ m.fr = Other(s) /\ Tag(m)
RecvReady(s, m) ==
    /\ m \in msgs /\ RecvReadyOK(s, m)
    /\ knows' = [knows EXCEPT ![s] = @ \cup {m.fr}]
    /\ ev' = [ev EXCEPT ![s] = @ \cup m.f]
    /\ el' = [el EXCEPT ![s] = 0]
    \* epoch 1 is Row on the two profiles and the carried history, which both
    \* ends now hold: the initiator computes it too (SamePlan checks both got
    \* the same answer; READY's "t" is the responder's)
    /\ LET p == Row(knows[s] \cup {m.fr}, ValidAt(ev[s] \cup m.f, att), {}, att) IN
       IF Planner = "today"
       THEN /\ ph' = [ph EXCEPT ![s] = "run"]
            /\ ts' = [ts EXCEPT ![s] = TodayBase(s)]
            /\ tried' = tried \cup TodayBase(s)
            /\ msgs' = msgs \ {m}
            /\ UNCHANGED <<ep, res, relaySaw>>
       ELSE IF p = "none"
       THEN /\ ph' = [ph EXCEPT ![s] = "relayed"]
            /\ msgs' = (msgs \ {m}) \cup {Msg("ABORT", s, 0, "NODIRECT", Carry(ev[s] \cup m.f))}
            /\ relaySaw' = relaySaw \cup {"ABORT"}
            /\ UNCHANGED <<ep, ts, res, tried>>
       ELSE /\ ph' = [ph EXCEPT ![s] = "run"]
            /\ ep' = [ep EXCEPT ![s] = 1]
            /\ ts' = [ts EXCEPT ![s] = {p}]
            /\ res' = [res EXCEPT ![s] = Acquire(s, p)]
            /\ tried' = tried \cup {p}
            /\ msgs' = msgs \ {m}
            /\ UNCHANGED relaySaw
    /\ IF Planner = "strategy" /\ Row(knows[s] \cup {m.fr}, ValidAt(ev[s] \cup m.f, att), {}, att) /= "none"
       THEN LET p == Row(knows[s] \cup {m.fr}, ValidAt(ev[s] \cup m.f, att), {}, att) IN
            /\ first' = p
            /\ escUsed' = escUsed + (IF p \in Escalated THEN 1 ELSE 0)
       ELSE UNCHANGED <<first, escUsed>>
    /\ UNCHANGED <<att, ini, commit, ifail, retry, luck, race, facSaw,
                   strangerSaw, fooled>>

\* The controlled side takes a newer plan. With UntaggedPlant it also takes
\* one that arrives after its attempt ended.
RecvPlanOK(s, m) ==
    /\ Planner = "strategy" /\ s = Fol /\ m.ty = "PLAN" /\ m.fr = Other(s)
    /\ \/ Tag(m) /\ ph[s] = "run" /\ m.e > ep[s]
       \/ UntaggedPlant /\ ph[s] = "relayed" /\ m.e > ep[s]
RecvPlan(s, m) ==
    /\ m \in msgs /\ RecvPlanOK(s, m)
    /\ msgs' = msgs \ {m}
    /\ ev' = [ev EXCEPT ![s] = @ \cup m.f]
    /\ ep' = [ep EXCEPT ![s] = m.e]
    /\ ts' = [ts EXCEPT ![s] = {m.t}]
    /\ res' = [res EXCEPT ![s] = Acquire(s, m.t)]
    /\ el' = [el EXCEPT ![s] = 0]
    /\ UNCHANGED <<att, ini, ph, knows, commit, escUsed, tried, first, ifail, retry,
                   luck, race, facSaw, strangerSaw, relaySaw, fooled>>

\* The controller takes the controlled side's report (the asymmetric
\* partials only it can see), and re-plans on it.
RecvObsOK(s, m) == Planner = "strategy" /\ s = Ctl /\ m.ty = "OBS" /\ m.fr = Fol /\ Tag(m)
RecvObs(s, m) ==
    /\ m \in msgs /\ RecvObsOK(s, m)
    /\ LET F1 == ev[s] \cup m.f
           F2 == F1 \cup TwoSided(s, F1)
       IN
       /\ ev' = [ev EXCEPT ![s] = F2]
       /\ IF ph[s] = "run" THEN Decide(s, F2, msgs \ {m})
          ELSE /\ msgs' = msgs \ {m}
               /\ UNCHANGED <<ph, ep, ts, el, res, tried, first, escUsed, relaySaw, fooled>>
    /\ UNCHANGED <<att, ini, knows, commit, ifail, retry, luck, race, facSaw, strangerSaw, fooled>>

-----------------------------------------------------------------------------
(* The network: hearing and acks on a path both ends take part in.        *)
HearEn(s, t) ==
    /\ ph[s] = "run"
    /\ \/ t \in Techs /\ t \in Lit(s) /\ PeerSends(s, t) /\ Hears(s, t)
       \/ t = "stray" /\ "tokened" \in strangerSaw /\ \E u \in ts[s] : Knocks(s, u)
    /\ ~Heard(s, t)
AckEn(s, t) ==
    /\ ph[s] = "run"
    /\ \/ t \in Techs /\ t \in Lit(s) /\ PeerSends(s, t) /\ Acks(s, t) /\ Heard(Other(s), t)
       \/ t = "stray" /\ "tokened" \in strangerSaw /\ \E u \in ts[s] : Knocks(s, u)
    /\ ~AckedF(s, t)
NomEn(s) ==
    /\ Planner = "strategy" /\ s = Fol /\ ph[s] = "run" /\ ph[Ctl] = "nom"
    /\ commit[Ctl] \in ts[s] /\ HA(s, commit[Ctl]) /\ Hears(s, commit[Ctl])
NetPending(s) == (\E t \in Paths : HearEn(s, t) \/ AckEn(s, t)) \/ NomEn(s)
                 \/ (Planner = "strategy" /\ s = Ctl /\ ph[s] = "run" /\ \E t \in ts[s] : HA(s, t))
                 \/ (Planner = "today" /\ ph[s] = "run" /\ \E t \in Lit(s) : HA(s, t))

Hear(s, t) ==
    /\ HearEn(s, t)
    /\ LET nf == {Fact("heard", s, t, att)}
                 \cup (IF t = "plain" /\ OtherPortSeen(s) THEN {Fact("otherPort", Other(s), "-", att)} ELSE {})
           F2 == ev[s] \cup nf
           news == {f \in nf : f.k \in Decisive /\ f \notin ev[s]}
       IN
       /\ ev' = [ev EXCEPT ![s] = F2]
       /\ IF Planner = "strategy" /\ s = Ctl
          THEN Decide(s, F2, msgs)
          ELSE IF Planner = "strategy" /\ news /= {}
          THEN /\ msgs' = msgs \cup {Msg("OBS", s, ep[s], "-", news)}
               /\ relaySaw' = relaySaw \cup {"OBS"}
               /\ UNCHANGED <<ph, ep, ts, el, res, tried, first, escUsed, fooled>>
          ELSE UNCHANGED <<ph, ep, ts, el, res, tried, first, escUsed, msgs, relaySaw, fooled>>
    /\ UNCHANGED <<att, ini, knows, commit, ifail, retry, luck, race, facSaw, strangerSaw, fooled>>

Ack(s, t) ==
    /\ AckEn(s, t)
    /\ ev' = [ev EXCEPT ![s] = @ \cup {Fact("acked", s, t, att)}]
    /\ UNCHANGED <<att, ini, ph, knows, ep, ts, el, res, commit, msgs, escUsed, tried,
                   first, ifail, retry, luck, race, facSaw, strangerSaw, relaySaw, fooled>>

\* A knock lands on the stranger's port: it learns a knock arrived (or,
\* with the plant, gets the token and can answer as "stray").
\* What a datagram to a port that may be anyone's carries: a tokenless
\* knock; a session frame when the plant tokens the knocks, or when the
\* technique sends session frames there (predAB); and a session frame is
\* the raw token on the legacy wire, a MAC with a fresh nonce on the new one.
StrayKind(s) ==
    IF HijackPlant \/ "predAB" \in ts[s]
    THEN (IF Wire = "mac" THEN "mac" ELSE "tokened")
    ELSE "K"
StrangerGets(s) ==
    /\ Stranger /\ ph[s] = "run" /\ \E u \in ts[s] : Knocks(s, u)
    /\ LET kind == StrayKind(s) IN
       /\ kind \notin strangerSaw
       /\ strangerSaw' = strangerSaw \cup {kind}
    /\ UNCHANGED <<att, ini, ph, knows, ev, ep, ts, el, res, commit, msgs, escUsed, tried,
                   first, ifail, retry, luck, race, facSaw, relaySaw, fooled>>

\* The LAN host-candidate leak (lit. review 11; leviculum#112 item 2): a
\* peer behind ANOTHER NAT that shares this side's public IP (a carrier
\* NAT) advertised a private address; punching it sends session frames
\* onto this side's own LAN, where a stranger may hold that address.
StrangerOnLan(s) ==
    /\ LanStranger /\ ph[s] = "run" /\ "lan" \in ts[s] /\ ~LanWorks
    /\ LET kind == IF Wire = "mac" THEN "mac" ELSE "tokened" IN
       /\ kind \notin strangerSaw
       /\ strangerSaw' = strangerSaw \cup {kind}
    /\ UNCHANGED <<att, ini, ph, knows, ev, ep, ts, el, res, commit, msgs, escUsed, tried,
                   first, ifail, retry, luck, race, facSaw, relaySaw, fooled>>

\* Keepalive replay (survey 8): an observer that holds a captured frame
\* (on the path, or a stranger that was sent the raw token) replays the
\* legacy keepalive (constant seq u32::MAX) to a side whose peer has gone;
\* its liveness is refreshed by a frame the peer never sent. The HMAC wire's
\* counter (RNSK, checked for freshness) rejects it.
EveHasFrame == Wire = "raw" /\ (OnPath \/ "tokened" \in strangerSaw)
Replay(s) ==
    /\ EveHasFrame /\ ph[s] = "direct" /\ ph[Other(s)] = "relayed" /\ ~fooled[s]
    /\ fooled' = [fooled EXCEPT ![s] = TRUE]
    /\ UNCHANGED <<att, ini, ph, knows, ev, ep, ts, el, res, commit, msgs, escUsed, tried,
                   first, ifail, retry, luck, race, facSaw, strangerSaw, relaySaw>>

-----------------------------------------------------------------------------
(* Time. A timer never runs while a datagram it waits for is on its way   *)
(* (unless NetLossy) or while a signal of this attempt is in flight       *)
(* (unless RelayLossy): the relay and the network are faster than every   *)
(* timer (DirectLinkPunch M1, M3).                                         *)
Lim(s) ==
    IF ph[s] = "run"
    THEN (IF Planner = "today" THEN W ELSE LimT)
    ELSE LimT
\* The controller holds its own verdict and waits for the peer's, which the
\* peer (still running the same plan) will send.
AwaitsVerdict(s) ==
    /\ Planner = "strategy" /\ s = Ctl /\ ph[s] = "run" /\ ph[Fol] = "run"
    /\ \E t \in ts[s] : HasF(Now(s), "silent", s, t) /\ ~HasF(Now(s), "silent", Fol, t)
                        /\ ~HA(Fol, t)   \* it will send one: its own stop is still to fire
\* The controlled side has given its verdict and waits for the plan the
\* controller (still on the same epoch) will send.
AwaitsPlan(s) ==
    /\ Planner = "strategy" /\ s = Fol /\ ph[s] = "run" /\ ph[Ctl] = "run" /\ ep[Ctl] = ep[s]
    /\ \E t \in ts[s] : HasF(Now(s), "silent", s, t)
TickOK(s) ==
    /\ NetLossy \/ ~\E x \in Sides : NetPending(x)
    /\ RelayLossy \/ /\ ~\E m \in msgs : m.n = att
                     /\ ph[Other(s)] /= "gather"
                     /\ ~AwaitsVerdict(s) /\ ~AwaitsPlan(s)

\* Both ends run one epoch's timers in step (DirectLinkPunch MaxSkew = 0:
\* the responder's start delay; follow-up 3, synchronised start).
SkewOK(s) == (ph[s] = "run" /\ ph[Other(s)] = "run" /\ ep[s] = ep[Other(s)]) => el[s] <= el[Other(s)]
Tick(s) ==
    /\ ph[s] \in {"wait", "run", "nom", "cwait"}
    /\ el[s] < Lim(s)
    /\ TickOK(s)
    /\ SkewOK(s)
    /\ \/ \* strategy: the technique's stop condition (silence, or heard
          \* but never acked) fires on this side: its verdict ("silent")
          \* and partial; the controlled side reports both, the controller
          \* re-plans once it holds the peer's verdict too (TwoSided)
          /\ Planner = "strategy" /\ ph[s] = "run" /\ el[s] + 1 = StopT
          /\ \A t \in ts[s] : ~HA(s, t)
          /\ LET t == CHOOSE x \in ts[s] : TRUE
                 nf == {Fact("silent", s, t, att)}
                       \cup (IF Heard(s, t) THEN {Fact("heardOnly", s, t, att)} ELSE {})
                 F1 == ev[s] \cup nf
                 F2 == IF s = Ctl THEN F1 \cup TwoSided(s, F1) ELSE F1
                 news == {f \in nf : f \notin ev[s]}
             IN
             /\ ev' = [ev EXCEPT ![s] = F2]
             /\ IF s = Ctl /\ (F2 /= F1 \/ ts[s] /= {Row(knows[s], ValidAt(F2, att), tried, att)})
                THEN Decide(s, F2, msgs)
                ELSE IF s = Ctl
                THEN /\ el' = [el EXCEPT ![s] = @ + 1]
                     /\ UNCHANGED <<ph, ep, ts, res, tried, first, escUsed, msgs, relaySaw, fooled>>
                ELSE /\ el' = [el EXCEPT ![s] = @ + 1]
                     /\ msgs' = msgs \cup {Msg("OBS", s, ep[s], "-", news)}
                     /\ relaySaw' = relaySaw \cup {"OBS"}
                     /\ UNCHANGED <<ph, ep, ts, res, tried, first, escUsed, fooled>>
          /\ UNCHANGED <<att, ini, knows, commit, ifail, retry, luck, race, facSaw, strangerSaw, fooled>>
       \/ \* strategy: the peer's verdict never came (a lost signal): the
          \* controller's own verdict decides, at its deadline
          /\ Planner = "strategy" /\ s = Ctl /\ ph[s] = "run" /\ el[s] + 1 = LimT
          /\ \A t \in ts[s] : ~HA(s, t) /\ HasF(Now(s), "silent", s, t)
          /\ LET t == CHOOSE x \in ts[s] : TRUE
                 F2 == ev[s] \cup {Fact("failed", s, t, att)}
             IN /\ ev' = [ev EXCEPT ![s] = F2]
                /\ Decide(s, F2, msgs)
          /\ UNCHANGED <<att, ini, knows, commit, ifail, retry, luck, race, facSaw, strangerSaw, fooled>>
       \/ \* today: ESCALATE_AFTER with nothing heard: predicted ports,
          \* birthday sockets and the spray, all at once
          /\ Planner = "today" /\ ph[s] = "run" /\ el[s] + 1 = EscAt
          /\ ~\E t \in Paths : Heard(s, t)
          /\ LET x == TodayEsc(s) IN
             /\ ts' = [ts EXCEPT ![s] = @ \cup x]
             /\ res' = [res EXCEPT ![s] = @ \cup {t \in x : t \in HoldsRes}]
             /\ tried' = tried \cup x
             /\ escUsed' = escUsed + (IF x /= {} THEN 1 ELSE 0)
          /\ el' = [el EXCEPT ![s] = @ + 1]
          /\ UNCHANGED <<att, ini, ph, knows, ev, ep, commit, msgs, first, ifail, retry,
                         luck, race, facSaw, strangerSaw, relaySaw, fooled>>
       \/ \* any other tick
          /\ ~(Planner = "strategy" /\ ph[s] = "run" /\ el[s] + 1 = StopT
               /\ \A t \in ts[s] : ~HA(s, t))
          /\ ~(Planner = "strategy" /\ s = Ctl /\ ph[s] = "run" /\ el[s] + 1 = LimT
               /\ \A t \in ts[s] : ~HA(s, t) /\ HasF(Now(s), "silent", s, t))
          /\ ~(Planner = "today" /\ ph[s] = "run" /\ el[s] + 1 = EscAt
               /\ ~\E t \in Paths : Heard(s, t))
          /\ el' = [el EXCEPT ![s] = @ + 1]
          /\ UNCHANGED <<att, ini, ph, knows, ev, ep, ts, res, commit, msgs, escUsed,
                         tried, first, ifail, retry, luck, race, facSaw, strangerSaw, relaySaw, fooled>>

-----------------------------------------------------------------------------
(* Committing.                                                              *)
\* strategy: the controller nominates the pair it proved (heard+acked on
\* its plan's technique), in-band, like ICE's USE-CANDIDATE; the plan is
\* frozen from here.
Nominate(c) ==
    /\ Planner = "strategy" /\ c = Ctl /\ ph[c] = "run"
    /\ \E t \in ts[c] :
       /\ HA(c, t)
       /\ ph' = [ph EXCEPT ![c] = "nom"]
       /\ commit' = [commit EXCEPT ![c] = t]
    /\ el' = [el EXCEPT ![c] = 0]
    /\ UNCHANGED <<att, ini, knows, ev, ep, ts, res, msgs, escUsed, tried, first, ifail,
                   retry, luck, race, facSaw, strangerSaw, relaySaw, fooled>>

\* The controlled side commits to the nominated pair, on which it has
\* itself heard and been acked, and confirms over the relay.
RecvNom(s) ==
    /\ NomEn(s)
    /\ ph' = [ph EXCEPT ![s] = "cwait"]
    /\ commit' = [commit EXCEPT ![s] = commit[Ctl]]
    /\ res' = [res EXCEPT ![s] = @ \cap {commit[Ctl]}]
    /\ el' = [el EXCEPT ![s] = 0]
    /\ msgs' = msgs \cup {Msg("COMPLETE", s, ep[s], commit[Ctl], {})}
    /\ relaySaw' = relaySaw \cup {"COMPLETE"}
    /\ UNCHANGED <<att, ini, knows, ev, ep, ts, escUsed, tried, first, ifail, retry,
                   luck, race, facSaw, strangerSaw, fooled>>

\* today: each side settles on the FIRST address it both heard and was
\* acked on (std punch_inner :1313), and sends COMPLETE (no pair in it).
PunchOK(s) ==
    /\ Planner = "today" /\ ph[s] = "run"
    /\ \E t \in Lit(s) \cup (IF HijackPlant THEN {"stray"} ELSE {}) :
       /\ HA(s, t)
       /\ commit' = [commit EXCEPT ![s] = t]
       /\ res' = [res EXCEPT ![s] = @ \cap {t}]
       /\ LET done == HasF(Now(s), "peerDone", s, "*") IN
          /\ ph' = [ph EXCEPT ![s] = IF done THEN "direct" ELSE "cwait"]
          /\ ev' = [ev EXCEPT ![s] = @ \cup (IF done THEN {Fact("ok", s, t, att)} ELSE {})]
    /\ el' = [el EXCEPT ![s] = 0]
    /\ msgs' = msgs \cup {Msg("COMPLETE", s, 0, "-", {})}
    /\ relaySaw' = relaySaw \cup {"COMPLETE"}
    /\ UNCHANGED <<att, ini, knows, ep, ts, escUsed, tried, first, ifail, retry, luck, race,
                   facSaw, strangerSaw, fooled>>

RecvCompleteOK(s, m) ==
    /\ m.ty = "COMPLETE" /\ m.fr = Other(s) /\ Tag(m)
    /\ IF Planner = "strategy"
       THEN \/ s = Ctl /\ ph[s] = "nom" /\ m.t = commit[s]
            \/ s = Fol /\ ph[s] = "cwait" /\ m.t = commit[s]
       ELSE ph[s] \in {"run", "cwait"}
RecvComplete(s, m) ==
    /\ m \in msgs /\ RecvCompleteOK(s, m)
    /\ IF ph[s] = "run"   \* today: the peer's half, while still punching
       THEN /\ ev' = [ev EXCEPT ![s] = @ \cup {Fact("peerDone", s, "-", att)}]
            /\ msgs' = msgs \ {m}
            /\ UNCHANGED <<ph, res, relaySaw, fooled>>
       ELSE /\ ph' = [ph EXCEPT ![s] = "direct"]
            /\ ev' = [ev EXCEPT ![s] = @ \cup {Fact("ok", s, commit[s], att)}]
            /\ res' = [res EXCEPT ![s] = @ \cap {commit[s]}]
            /\ IF Planner = "strategy" /\ s = Ctl
               THEN /\ msgs' = (msgs \ {m}) \cup {Msg("COMPLETE", s, ep[s], commit[s], {})}
                    /\ relaySaw' = relaySaw \cup {"COMPLETE"}
               ELSE /\ msgs' = msgs \ {m}
                    /\ UNCHANGED relaySaw
    /\ UNCHANGED <<att, ini, knows, ep, ts, el, commit, escUsed, tried, first, ifail,
                   retry, luck, race, facSaw, strangerSaw, fooled>>

-----------------------------------------------------------------------------
(* Ending without a path.                                                   *)
\* The facts a side's last word carries, for the peer's history.
Release(s) ==
    /\ ts' = [ts EXCEPT ![s] = {}]
    /\ res' = [res EXCEPT ![s] = {}]
    /\ commit' = [commit EXCEPT ![s] = "-"]

\* today: the punch window ends with nothing heard+acked: REJECT
\* PUNCH_FAILED; every technique it ran is recorded as failed.
PunchFail(s) ==
    /\ Planner = "today" /\ ph[s] = "run" /\ el[s] = W
    /\ ~\E t \in Lit(s) : HA(s, t)
    /\ RelayLossy \/ ~\E m \in msgs : m.n = att /\ m.fr = Other(s)
    /\ LET nf == {Fact("failed", s, t, att) : t \in Lit(s)}
                 \cup {Fact("heardOnly", s, t, att) : t \in {u \in Lit(s) : Heard(s, u)}}
       IN /\ ev' = [ev EXCEPT ![s] = @ \cup nf]
          /\ msgs' = msgs \cup {Msg("ABORT", s, 0, "PF", Carry(ev[s] \cup nf))}
    /\ ph' = [ph EXCEPT ![s] = "relayed"]
    /\ Release(s)
    /\ relaySaw' = relaySaw \cup {"ABORT"}
    /\ ifail' = IF s = ini /\ ifail = "-" THEN "own" ELSE ifail
    /\ UNCHANGED <<att, ini, knows, ep, el, escUsed, tried, first, retry, luck, race, facSaw,
                   strangerSaw, fooled>>

\* A phase deadline (core Session::poll): only once nothing it waits for is
\* in flight to it (M3), unless the relay loses signals.
Deadline(s) ==
    /\ \/ ph[s] \in {"wait", "nom", "cwait"}
       \/ ph[s] = "run" /\ Planner = "strategy" /\ s /= Ctl
    /\ el[s] = Lim(s)
    /\ RelayLossy \/ ~\E m \in msgs : m.n = att /\ m.fr = Other(s)
    \* M3: not while the peer still runs the attempt and will send its
    \* verdict (today: COMMIT_TIMEOUT outlasts the peer's punch window)
    /\ RelayLossy \/ ph[Other(s)] \notin {"gather", "run"}
    /\ NetLossy \/ ~\E x \in Sides : NetPending(x)
    /\ ph' = [ph EXCEPT ![s] = "relayed"]
    /\ Release(s)
    /\ msgs' = msgs \cup {Msg("ABORT", s, ep[s], "TIMEOUT", Carry(ev[s]))}
    /\ relaySaw' = relaySaw \cup {"ABORT"}
    /\ ifail' = IF s = ini /\ ifail = "-" THEN "timeout" ELSE ifail
    /\ UNCHANGED <<att, ini, knows, ev, ep, el, escUsed, tried, first, retry, luck, race,
                   facSaw, strangerSaw, fooled>>

RecvAbortOK(s, m) ==
    m.ty = "ABORT" /\ m.fr = Other(s) /\ Tag(m) /\ ph[s] \in {"gather", "wait", "run", "nom", "cwait"}
RecvAbort(s, m) ==
    /\ m \in msgs /\ RecvAbortOK(s, m)
    /\ msgs' = msgs \ {m}
    /\ ev' = [ev EXCEPT ![s] = @ \cup m.f]
    /\ ph' = [ph EXCEPT ![s] = "relayed"]
    /\ Release(s)
    /\ ifail' = IF s = ini /\ ifail = "-" THEN "peer:" \o m.t ELSE ifail
    /\ UNCHANGED <<att, ini, knows, ep, el, escUsed, tried, first, retry, luck, race, facSaw,
                   strangerSaw, relaySaw, fooled>>

Accepted(s, m) == RecvReqOK(s, m) \/ RecvReadyOK(s, m) \/ RecvPlanOK(s, m) \/ RecvObsOK(s, m)
                  \/ RecvCompleteOK(s, m) \/ RecvAbortOK(s, m)
\* A signal no handler takes is dropped (a stale one, or one for a phase
\* that is over): the session ignores it (core Session::signal).
Discard(m) ==
    /\ m \in msgs /\ ~Accepted(Other(m.fr), m)
    /\ msgs' = msgs \ {m}
    /\ UNCHANGED <<att, ini, ph, knows, ev, ep, ts, el, res, commit, escUsed, tried, first,
                   ifail, retry, luck, race, facSaw, strangerSaw, relaySaw, fooled>>
LoseSignal(m) ==
    /\ RelayLossy /\ m \in msgs
    /\ msgs' = msgs \ {m}
    /\ UNCHANGED <<att, ini, ph, knows, ev, ep, ts, el, res, commit, escUsed, tried, first,
                   ifail, retry, luck, race, facSaw, strangerSaw, relaySaw, fooled>>

\* A lone direct side hears nothing on the direct path and falls back
\* (direct_udp fast liveness, node direct_link_link_stale).
FallBack(s) ==
    /\ ph[s] = "direct" /\ ph[Other(s)] = "relayed"
    /\ ph' = [ph EXCEPT ![s] = "relayed"]
    /\ Release(s)
    /\ UNCHANGED <<att, ini, knows, ev, ep, el, msgs, escUsed, tried, first, ifail, retry,
                   luck, race, facSaw, strangerSaw, relaySaw, fooled>>

\* One side's end of an established path goes (it roams, its mapping
\* expires): it falls back; the other learns only from silence (FallBack),
\* unless a replayed keepalive keeps it believing (Replay).
LoneLoss(s) ==
    /\ PathLoss /\ \A x \in Sides : ph[x] = "direct"
    /\ ph' = [ph EXCEPT ![s] = "relayed"]
    /\ Release(s)
    /\ ifail' = "lost"
    /\ UNCHANGED <<att, ini, knows, ev, ep, el, msgs, escUsed, tried, first, retry, luck, race,
                   facSaw, strangerSaw, relaySaw, fooled>>

\* An established path dies later (a mapping expires): both fall back.
PathLost ==
    /\ PathLoss /\ \A s \in Sides : ph[s] = "direct"
    /\ ph' = [s \in Sides |-> "relayed"]
    /\ ts' = [s \in Sides |-> {}]
    /\ res' = [s \in Sides |-> {}]
    /\ commit' = [s \in Sides |-> "-"]
    /\ ifail' = "lost"
    /\ UNCHANGED <<att, ini, knows, ev, ep, el, msgs, escUsed, tried, first, retry, luck, race,
                   facSaw, strangerSaw, relaySaw, fooled>>

-----------------------------------------------------------------------------
(* Learning: whether, and how, to propose again.                            *)
\* A side the attempt never reached (its REQUEST lost) is on the relay too.
Untouched(s) == ph[s] = "idle" /\ ~\E m \in msgs : m.ty = "REQ" /\ m.n = att
SettledRelayed == \A s \in Sides : ph[s] = "relayed" \/ Untouched(s)
\* today (core end_direct_link_session): the initiator retries unless it
\* was REJECTed for a reason other than BUSY, whichever verdict came first.
TodayRetry == ifail \in {"own", "timeout", "probe", "lost"}
\* What today's code means to do (its comment: "fails for a reason worth
\* trying again (the punch, a timeout, the probe...)"): the peer's
\* PUNCH_FAILED is the punch failing.
TodayIntent == ifail \in {"own", "timeout", "probe", "lost", "peer:PF"}
\* strategy: propose again exactly when the evidence the next attempt will
\* hold still leaves a technique to try.
StrategyRetry == Row(knows[ini], ValidAt(ev[ini], att + 1), {}, att + 1) /= "none"
RetryRule == IF Planner = "today" THEN TodayRetry ELSE StrategyRetry
JustifiedRetry == IF Planner = "today" THEN TodayIntent ELSE StrategyRetry

DecideRetry ==
    /\ SettledRelayed /\ retry = "-"
    /\ ~\E m \in msgs : m.n = att
    /\ retry' = IF att < MaxAtt /\ RetryRule THEN "yes" ELSE "no"
    /\ UNCHANGED <<att, ini, ph, knows, ev, ep, ts, el, res, commit, msgs, escUsed, tried,
                   first, ifail, luck, race, facSaw, strangerSaw, relaySaw, fooled>>

NewAttempt ==
    /\ SettledRelayed /\ retry = "yes"
    /\ att' = att + 1
    /\ \E i \in Sides :
       /\ ini' = i
       /\ ph' = [s \in Sides |-> IF s = i THEN "gather" ELSE "idle"]
    /\ ep' = [s \in Sides |-> 0]
    /\ ts' = [s \in Sides |-> {}]
    /\ el' = [s \in Sides |-> 0]
    /\ res' = [s \in Sides |-> {}]
    /\ commit' = [s \in Sides |-> "-"]
    /\ tried' = {}
    /\ first' = "-"
    /\ ifail' = "-"
    /\ retry' = "-"
    /\ UNCHANGED <<knows, ev, msgs, escUsed, luck, race, facSaw, strangerSaw, relaySaw, fooled>>

-----------------------------------------------------------------------------
Next ==
    \/ \E s \in Sides :
        \/ Probe(s) \/ Tick(s) \/ Nominate(s) \/ RecvNom(s) \/ PunchOK(s)
        \/ PunchFail(s) \/ Deadline(s) \/ FallBack(s) \/ StrangerGets(s)
        \/ StrangerOnLan(s) \/ Replay(s) \/ LoneLoss(s)
        \/ \E t \in Paths : Hear(s, t) \/ Ack(s, t)
        \/ \E m \in msgs : RecvReq(s, m) \/ RecvReady(s, m) \/ RecvPlan(s, m)
                           \/ RecvObs(s, m) \/ RecvComplete(s, m) \/ RecvAbort(s, m)
    \/ \E m \in msgs : Discard(m) \/ LoseSignal(m)
    \/ PathLost \/ DecideRetry \/ NewAttempt

\* Weak fairness on every action except the faults (a lossy relay or
\* network may lose; it need not).
Fair(s) ==
    /\ WF_vars(Probe(s)) /\ WF_vars(Tick(s)) /\ WF_vars(Nominate(s))
    /\ WF_vars(PunchOK(s)) /\ WF_vars(PunchFail(s)) /\ WF_vars(Deadline(s))
    /\ WF_vars(FallBack(s))
    /\ WF_vars(\E m \in msgs : RecvReq(s, m) \/ RecvReady(s, m) \/ RecvPlan(s, m)
                               \/ RecvObs(s, m) \/ RecvComplete(s, m) \/ RecvAbort(s, m))
    /\ (~NetLossy => /\ WF_vars(RecvNom(s))
                     /\ WF_vars(\E t \in Paths : Hear(s, t) \/ Ack(s, t)))
Fairness ==
    /\ \A s \in Sides : Fair(s)
    /\ WF_vars(\E m \in msgs : Discard(m))
    /\ WF_vars(DecideRetry) /\ WF_vars(NewAttempt)

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
(***************************************************************************)
(* PROPERTIES                                                              *)
(***************************************************************************)
Over == \A s \in Sides : ph[s] \in {"direct", "relayed"} \/ Untouched(s)
BothDirect == \A s \in Sides : ph[s] = "direct"
Faults == RelayLossy \/ NetLossy
Quiet == ~\E m \in msgs : m.n = att
InFlight(ty) == \E m \in msgs : m.ty = ty /\ m.n = att
Started(s) == ts'[s] \ ts[s]
Phases == {"idle", "gather", "wait", "run", "nom", "cwait", "direct", "relayed"}

TypeOK ==
    /\ att \in 1..MaxAtt /\ ini \in Sides
    /\ ph \in [Sides -> Phases]
    /\ \A s \in Sides : ts[s] \subseteq Techs /\ res[s] \subseteq HoldsRes
    /\ \A s \in Sides : commit[s] \in Paths \cup {"-"}

(* Total: one technique at a time, and the table (Row) is the physics'    *)
(* cheapest sufficient technique on EVERY view either side reaches. Row   *)
(* is a decision list, so it is a total function by construction; what    *)
(* TLC checks is that its rows, written in the table's own words, never   *)
(* disagree with Cheapest (from Pre, Cost, Failed).                       *)
OneTechnique == \A s \in Sides : ph[s] = "run" => Cardinality(ts[s]) = 1
PlanIsCheapest == \A s \in Sides : Row(knows[s], Now(s), tried, att) = Cheapest(knows[s], Now(s), tried, att)
Total == OneTechnique /\ PlanIsCheapest

(* NoSpam: a technique starts only when its precondition holds on the     *)
(* starter's evidence, never twice in one attempt or against a valid      *)
(* failure, and the escalation and cost bounds hold.                      *)
NoSpamStep ==
    \A s \in Deciders : \A t \in Started(s) : att' = att =>
         /\ Pre(knows'[s], ValidAt(ev'[s], att), tried, att, t)
         /\ t \notin Failed(ValidAt(ev'[s], att), att)
         /\ (Planner = "strategy" => t \notin tried)
NoSpam == [][NoSpamStep]_vars
EscPerAttempt == Cardinality(tried \cap Escalated) <= 1
AttemptCost == SumCost(tried) <= CostCap
CapsHeld == escUsed <= PairBudget

(* UsesEvidence: nothing started while a strictly cheaper technique was   *)
(* available; the controller runs exactly Row(its evidence) in every      *)
(* state (re-plans in the same step a fact arrives); and a pair ends      *)
(* relayed only if no technique could have worked.                        *)
UsesEvidenceStep ==
    \A s \in Deciders : \A t \in Started(s) : att' = att =>
         ~\E u \in Techs : Cost(u) < Cost(t) /\ Avail(knows'[s], ValidAt(ev'[s], att), tried, att, u)
UsesEvidence == [][UsesEvidenceStep]_vars
\* The floor: no decider starts a technique whose expected success, from
\* the cited arithmetic, is negligible, however reachable it is.
NoNegligibleStep ==
    \A s \in Deciders : \A t \in Started(s) : att' = att =>
        Likely(knows'[s], ValidAt(ev'[s], att), att, t) /= "negligible"
NoNegligibleRow == [][NoNegligibleStep]_vars
PlanFollowsEvidence ==
    Planner = "strategy" => (ph[Ctl] = "run" => ts[Ctl] = {Row(knows[Ctl], Now(Ctl), tried, att)})
\* The environment allows t: its gates, the budget, and the floor (a
\* both-random birthday is negligible however reachable).
EnvAllows(t) ==
    CASE t \in {"predA", "predB"} -> Esc(Other(Side(t))) /\ (escUsed < PairBudget \/ t \in tried)
      [] t = "predAB" -> EscA /\ EscB /\ (escUsed < PairBudget \/ t \in tried)
      [] t \in {"bdayA", "bdayB"} -> EscA /\ EscB /\ (escUsed < PairBudget \/ t \in tried)
                                    /\ EIM(Other(Side(t)))
      [] OTHER -> TRUE
DirectWhenPossible ==
    (SettledRelayed /\ ~Faults /\ retry = "no" /\ ifail /= "lost")
        => \A t \in Techs : Works(t) => ~EnvAllows(t)
GateAllows(t) ==
    CASE t \in {"predA", "predB"} -> Esc(Other(Side(t)))
      [] t \in {"bdayA", "bdayB", "predAB"} -> EscA /\ EscB
      [] OTHER -> TRUE
\* A technique is recorded failed only if it cannot work (no faults).
NoFalseAbort ==
    ~Faults => \A s \in Sides : \A f \in ev[s] : f.k = "failed" => ~(WorksAt(f.t, f.n) /\ GateAllows(f.t))

(* Agreement: both ends run the same plan once its signal has landed, and *)
(* both direct means one pair.                                            *)
SamePlan ==
    (Planner = "strategy" /\ ~RelayLossy /\ \A s \in Sides : ph[s] = "run" /\ ~InFlight("PLAN"))
        => ts["A"] = ts["B"]
Agreement == BothDirect => commit["A"] = commit["B"]

(* NoHalt: a variant strictly decreases on every step (so every attempt,  *)
(* and the whole run, ends within a bounded number of steps); nothing is  *)
(* stuck before the end; and the end is reached.                          *)
PhRank(p) == CASE p = "idle" -> 6 [] p = "gather" -> 5 [] p = "wait" -> 4 [] p = "run" -> 3
              [] p \in {"nom", "cwait"} -> 2 [] p = "direct" -> 1 [] OTHER -> 0
ElMax == IF W > LimT THEN W ELSE LimT
FactCount == Cardinality(ev["A"]) + Cardinality(ev["B"]) + Cardinality(knows["A"])
             + Cardinality(knows["B"]) + Cardinality(strangerSaw)
             + Cardinality({s \in Sides : fooled[s]})
Measure == << MaxAtt - att,
              PhRank(ph["A"]) + PhRank(ph["B"]),
              IF retry = "-" THEN 1 ELSE 0,
              (20 - ep["A"]) + (20 - ep["B"]),
              (ElMax - el["A"]) + (ElMax - el["B"]),
              10000 - FactCount,
              Cardinality(msgs) >>
RECURSIVE LexLessI(_, _, _)
LexLessI(a, b, i) == IF i > Len(a) THEN FALSE
                     ELSE IF a[i] < b[i] THEN TRUE
                     ELSE IF a[i] > b[i] THEN FALSE
                     ELSE LexLessI(a, b, i + 1)
VariantStep == vars' = vars \/ LexLessI(Measure', Measure, 1)
Variant == [][VariantStep]_vars
NotStuck == Over \/ ENABLED Next
Termination == <>[]Over

(* NoRace: no resource outlives its technique, no commitment outlives its *)
(* attempt, and nothing runs on a side that has ended.                    *)
NoRace ==
    \A s \in Sides :
        /\ res[s] \subseteq (CASE ph[s] = "run" -> ts[s]
                               [] ph[s] \in {"nom", "cwait", "direct"} -> {commit[s]}
                               [] OTHER -> {})
        /\ ph[s] \in {"idle", "gather", "wait", "run", "relayed"} => commit[s] = "-"
        /\ ph[s] \in {"idle", "gather", "wait", "relayed"} => ts[s] = {}

(* Learning: an attempt's outcome is evidence for the next.               *)
\* (A birthday at its next tranche is not "the same way": Failed encodes it.)
NotRetriedSameWay ==
    \A s \in Deciders : \A t \in ts[s] :
        t \notin Failed({f \in Now(s) : f.n < att}, att)
RetryIsEvidenceFunction ==
    (SettledRelayed /\ retry /= "-" /\ att < MaxAtt) => ((retry = "yes") = JustifiedRetry)
StartsWhereItWorked ==
    (Planner = "strategy" /\ att > 1 /\ first /= "-") =>
        \A f \in Now(Ctl) : (f.k = "ok" /\ f.n = att - 1) => first = f.t

(* Kept from DirectLinkPunch, at this layer.                              *)
EscalationNeedsTrust ==
    \A s \in Sides : \A t \in ts[s] \cap Escalated :
        (Knocks(s, t) \/ (s = Side(t) /\ t \in {"bdayA", "bdayB"})) => Esc(s)
AbortLeavesLinkUnchanged ==
    (Over /\ ~RelayLossy /\ Quiet /\ ifail /= "lost" /\ \E s \in Sides : ph[s] /= "direct") => SettledRelayed
NoLoneMove == (~RelayLossy /\ ifail /= "lost") => ~\E s \in Sides : ph[s] = "direct" /\ ph[Other(s)] = "relayed"
FacilitatorKnowledge == \A f \in Facs : \A x \in facSaw[f] : OwnFac(x) = f
FacilitatorCarriesNothing == relaySaw \subseteq {"REQ", "READY", "PLAN", "OBS", "COMPLETE", "ABORT"}
NoTokenToStranger == "tokened" \notin strangerSaw
\* A stranger learns only that a knock (or an authenticated frame it cannot
\* verify, forge or replay) arrived.
StrangerKnowledge == strangerSaw \subseteq {"K", "mac"}
\* No liveness from a frame the peer did not send (RFC 7675 consent: only
\* an authenticated, fresh response counts).
NoReplay == \A s \in Sides : ~fooled[s]
\* The negotiation: never a raw-token frame to a peer that has shown the
\* authenticated wire (HELLO); the legacy wire only toward one that has not.
NoRawToCapablePeer ==
    \A s \in Sides : ph[s] \in {"run", "nom", "cwait", "direct"} =>
        ~(Wire = "raw" /\ HelloA /\ HelloB)
NoPathToStranger == \A s \in Sides : commit[s] /= "stray"

\* Evidence is monotone: a fact is never discarded, only superseded.
MonotoneEvidence == [][\A s \in Sides : ev[s] \subseteq ev'[s] /\ knows[s] \subseteq knows'[s]]_vars

=============================================================================
