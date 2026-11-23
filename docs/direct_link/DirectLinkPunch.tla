--------------------------- MODULE DirectLinkPunch ---------------------------
(***************************************************************************)
(* The direct-link punch of leviculum (+ciris), as the code does it at the *)
(* head of diag/punch-telemetry after the third review (2cf992a): two     *)
(* nodes A and B share a relayed RNS link; A proposes; each probes ITS OWN *)
(* facilitator; both punch; the link moves only when both punches         *)
(* succeeded (the two-sided commit, #89).                                 *)
(*                                                                         *)
(* Names agreed with the Constitution (CC): linkPath per side is one of   *)
(* none / relayed / upgrading / direct; the initial state is              *)
(* RelayedLinkUp, the terminal ones DirectPathUp and RelayedOnly.         *)
(*                                                                         *)
(* Two PLANTS put back behaviour the review removed, so TLC can show each *)
(* named property catching it:                                            *)
(*   HijackPlant            72be0f9's spray: tokened punches to sprayed   *)
(*                          and predicted ports, and an ack from a        *)
(*                          sprayed port counted as ours;                 *)
(*   NamedFacilitatorPlant  the responder probes the facilitator the      *)
(*                          REQUEST names ("f"), not its own.             *)
(*                                                                         *)
(* Code references (file:function) are in DirectLinkPunch.md, per action. *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, Sequences, TLC

CONSTANTS
    MapA, MapB,          \* "EIM" | "EDMseq" | "EDMrand"   (RFC 4787 4.1)
    FiltA, FiltB,        \* "EIF" | "ADF" | "APDF"          (RFC 4787 5)
    SameNAT, Hairpin,    \* one NAT for both; it loops public->public back
    FacA, FacB,          \* each side's OWN facilitator: "F1" | "F2" | "R"
                         \* ("R": the relay carrying the link runs one)
    FacAnswers,          \* facilitators answer probes
    ForgedProbe,         \* A's facilitator answers with a forged mapping
    Observers,           \* 1..4 observers a side's session socket probes;
                         \* >= 2 classifies endpoint-dependent (review item 6)
    Pooling,             \* "paired" | "arbitrary" (RFC 4787 4.1)
    Stranger,            \* another host holds a port on the hard side's IP
    PolicyB,             \* "ask_host" | "identified_only" | "accept_all" | "reject"
                         \* (A, the proposer, runs ask_host)
    HostTrustA, HostTrustB, \* the host's escalation decision for the link
                         \* (core set_direct_link_escalation): "unset" |
                         \* "allow" | "deny"
    PeerIdentified,      \* the link's remote identity is known (LINKIDENTIFY)
    KF,                  \* birthday sockets on own evidence alone (the
                         \* fallback; 32 in the code against K's 128)
    Glare, BInitiatedLink,
    RelayLossy, RelayDup, NetLossy, Teardown, KeepaliveOK,
    HijackPlant, NamedFacilitatorPlant,
    MacFrames,           \* EXPLORATION, not the code: frame-auth-note.md's MAC'd
                         \* punch frames, sent to knocked and sprayed ports
                         \* too; a stranger that gets one cannot forge a reply
    SprayHits,           \* "any" | "hit" | "miss": oracle on the spray
    SprayBudget,         \* the process budget allows the spray (review 2)
    NP, W, EscAt, BdayAt, K, S, Q, MaxSkew, Latency

Sides == {"A", "B"}
Other(s) == IF s = "A" THEN "B" ELSE "A"
Map(s)  == IF s = "A" THEN MapA ELSE MapB
Filt(s) == IF s = "A" THEN FiltA ELSE FiltB
OwnFac(s) == IF s = "A" THEN FacA ELSE FacB
Ports == 1..NP
None == <<"none">>
Facs == {"F1", "F2", "R"}
FacAddr(f) == <<f, 0>>
IsFac(a) == a[1] \in Facs
\* The stranger's port on each side's IP (only the hard side's matters).
StrangerPort == NP
PubIP(s) == s
PeerIP(s) == IF Pooling = "arbitrary" /\ Map(s) /= "EIM" THEN s \o "2" ELSE s
Lan(s) == <<"lan", s>>
Socks == 0..K
\* Birthday sockets a side may hold: K (MAX_BIRTHDAY_SOCKETS, 128) on
\* two-observer evidence, KF (FALLBACK_BIRTHDAY_SOCKETS, 32) on own
\* evidence alone (std start_punch, third review N1c).
BdayK == IF Observers >= 2 THEN K ELSE KF
\* Ports a side's NAT may allocate: never the stranger's.
Alloc(s) == IF Stranger THEN Ports \ {StrangerPort} ELSE Ports

Phases == {"Idle", "Discovering", "Proposing", "AwaitingReady", "Punching",
           "Committing", "Connected", "Failed", "Gone"}

VARIABLES
    phase, role, sid, failure, chan,
    adv, pred, bday, peerAdv, peerPred, peerB,
    nat, ctr, open, active,
    targets, held, knocking, sprayed, punched, heard, acked,
    tick, escalated, punch, path, peerDone, fellBack,
    net, linkUp,
    duped,         \* signals the relay has duplicated (each at most once)
    \* knowledge (what each party has been given)
    facSaw,        \* facSaw[f]: <<side, mapped>> pairs facilitator f answered
    facSessions,   \* session ids any facilitator ever saw (probes carry none)
    strangerSaw,   \* kinds of frame the stranger received: "K" | "tokened"
    relaySaw,      \* signal types the relay carried
    persisted      \* observed mappings written to any record (CC 2.6.8)

vars == <<phase, role, sid, failure, chan, adv, pred, bday, peerAdv, peerPred,
          peerB, nat, ctr, open, active, targets, held, knocking, sprayed,
          punched, heard, acked, tick, escalated, punch, path, peerDone,
          fellBack, net, linkUp, duped, facSaw, facSessions, strangerSaw,
          relaySaw, persisted>>

punchVars == <<open, active, targets, held, knocking, sprayed, punched,
               heard, acked, tick, escalated, punch, path>>
knowVars == <<facSaw, facSessions, strangerSaw, relaySaw, persisted, duped>>

-----------------------------------------------------------------------------
(* linkPath (CC names), derived from the session state.                    *)
linkPath(s) ==
    IF ~linkUp \/ phase[s] = "Gone" THEN "none"
    ELSE IF phase[s] = "Connected" THEN (IF fellBack[s] THEN "relayed" ELSE "direct")
    ELSE IF phase[s] \in {"Idle", "Failed"} THEN "relayed"
    ELSE "upgrading"

RelayedLinkUp == \A s \in Sides : linkPath(s) = "relayed" /\ phase[s] = "Idle"
DirectPathUp == \A s \in Sides : linkPath(s) = "direct"
RelayedOnly == /\ \A s \in Sides : linkPath(s) = "relayed" /\ phase[s] \in {"Failed", "Idle"}
               /\ \E t \in Sides : phase[t] = "Failed"

-----------------------------------------------------------------------------
(* NAT.                                                                     *)
Used(s) == {m[3] : m \in nat[s]}
MapOf(s, k, d) == {m[3] : m \in {x \in nat[s] : x[1] = k /\ x[2] = d}}
SrcIP(s, d) == IF d[1] = "lan" THEN "lan" ELSE IF IsFac(d) THEN PubIP(s) ELSE PeerIP(s)
NewPorts(s, k, d) ==
    IF Map(s) = "EIM" THEN
        LET have == {m[3] : m \in {x \in nat[s] : x[1] = k}}
        IN IF have /= {} THEN have ELSE {k + 1}
    ELSE IF Map(s) = "EDMseq" THEN {ctr[s] + 1}
    ELSE Alloc(s) \ Used(s)
WithMapping(s, k, d, port) ==
    IF d[1] = "lan" \/ MapOf(s, k, d) /= {} THEN nat[s] ELSE nat[s] \cup {<<k, d, port>>}
\* RFC 4787 section 5: filtering is a property of the MAPPING the datagram
\* arrives at (its external address), judged against the destinations that
\* mapping was made toward. (An earlier draft judged it against every
\* mapping of the same internal socket, which on an EDM NAT let a port made
\* toward the facilitator admit the peer: the SamePath counterexamples of
\* the d4ff59e run were that, not the code.)
Admit(s, src, dstAddr) ==
    LET ms == {m \in nat[s] : <<SrcIP(s, m[2]), m[3]>> = dstAddr}
        socks == {m[1] : m \in ms}
    IN IF socks = {} THEN -1
       ELSE LET k == CHOOSE x \in socks : TRUE
                ok == CASE Filt(s) = "EIF"  -> TRUE
                        [] Filt(s) = "ADF"  -> \E m \in ms : m[2][1] = src[1]
                        [] Filt(s) = "APDF" -> \E m \in ms : m[2] = src
            IN IF ok /\ k \in open[s] THEN k ELSE -1

-----------------------------------------------------------------------------
Init ==
    /\ phase = [s \in Sides |-> "Idle"]
    /\ role = [s \in Sides |-> "-"]
    /\ sid = [s \in Sides |-> "-"]
    /\ failure = [s \in Sides |-> "-"]
    /\ chan = [s \in Sides |-> <<>>]
    /\ adv = [s \in Sides |-> None]
    /\ pred = [s \in Sides |-> {}]
    /\ bday = [s \in Sides |-> FALSE]
    /\ peerAdv = [s \in Sides |-> None]
    /\ peerPred = [s \in Sides |-> {}]
    /\ peerB = [s \in Sides |-> FALSE]
    /\ nat = [s \in Sides |-> {}]
    /\ ctr = [s \in Sides |-> 0]
    /\ open = [s \in Sides |-> {0}]
    /\ active = [s \in Sides |-> 0]
    /\ targets = [s \in Sides |-> {}]
    /\ held = [s \in Sides |-> {}]
    /\ knocking = [s \in Sides |-> {}]
    /\ sprayed = [s \in Sides |-> {}]
    /\ punched = [s \in Sides |-> {}]
    /\ heard = [s \in Sides |-> {}]
    /\ acked = [s \in Sides |-> {}]
    /\ tick = [s \in Sides |-> 0]
    /\ escalated = [s \in Sides |-> FALSE]
    /\ punch = [s \in Sides |-> "none"]
    /\ path = [s \in Sides |-> None]
    /\ peerDone = [s \in Sides |-> FALSE]
    /\ fellBack = [s \in Sides |-> FALSE]
    /\ net = {}
    /\ linkUp = TRUE
    /\ duped = {}
    /\ facSaw = [f \in Facs |-> {}]
    /\ facSessions = {}
    /\ strangerSaw = {}
    /\ relaySaw = {}
    /\ persisted = {}

\* A signal on the relayed link (wire::Signal over the RNS Channel). The
\* relay sees its type travel by; its content is the link's encrypted
\* payload (RelaySaw records only the type).
Send(s, m) ==
    /\ chan' = [chan EXCEPT ![Other(s)] = Append(@, m)]
    /\ relaySaw' = relaySaw \cup {m.t}
Front(s) == chan[s][1]

-----------------------------------------------------------------------------
(* Proposing: node propose_direct_link -> core Session::initiate.          *)
Propose(s) ==
    /\ linkUp /\ phase[s] = "Idle" /\ (s = "A" \/ Glare)
    /\ phase' = [phase EXCEPT ![s] = "Discovering"]
    /\ role' = [role EXCEPT ![s] = "I"]
    /\ sid' = [sid EXCEPT ![s] = s]
    /\ UNCHANGED <<failure, chan, adv, pred, bday, peerAdv, peerPred, peerB,
                   nat, ctr, punchVars, peerDone, fellBack, net, linkUp, knowVars>>

(* Which facilitator a side probes. The initiator: its own. The responder:
   its own too (CC 5.4.6, node accept_direct_request), or, with the plant,
   the one the REQUEST named. *)
ProbeTarget(s) ==
    IF role[s] = "R" /\ NamedFacilitatorPlant THEN OwnFac(Other(s)) ELSE OwnFac(s)

StartPunch(s, padv, ppred) ==
    /\ targets' = [targets EXCEPT ![s] = {padv} \cup (IF SameNAT THEN {Lan(Other(s))} ELSE {})]
    /\ held' = [held EXCEPT ![s] = {<<padv[1], p>> : p \in ppred} \ {padv}]
    /\ punch' = [punch EXCEPT ![s] = "running"]
    /\ tick' = [tick EXCEPT ![s] = 0]
    /\ UNCHANGED <<open, active, knocking, sprayed, punched, heard, acked, escalated, path>>

(* The probe (std start_probe). The session socket maps toward the chosen
   facilitator; it learns its address (or, ForgedProbe, a wrong port). With
   Observers >= 2 the same socket toward a second observer says whether the
   mapping is endpoint-dependent (classify_nat); only then does the side
   advertise "q" (steady steps) or "b" (random) (advertise). A probe carries
   a random nonce, never the session (facSessions stays empty). *)
ProbeAnswer(s, p) ==
    /\ linkUp /\ phase[s] = "Discovering" /\ FacAnswers
    /\ p \in NewPorts(s, 0, FacAddr(ProbeTarget(s)))
    /\ LET f == ProbeTarget(s)
           seen == IF ForgedProbe /\ s = "A" THEN (IF p = NP THEN 1 ELSE p + 1) ELSE p
           \* Two-observer evidence (std classify_nat), or, with one observer,
           \* the own-evidence fallback (std decide): random steps alone.
           edm == Map(s) /= "EIM" /\ Observers >= 2
           c2 == IF Map(s) = "EDMseq" THEN p + 2 ELSE ctr[s]
           q  == IF edm /\ Map(s) = "EDMseq" THEN {c2 + i : i \in 1..Q} \cap Ports ELSE {}
           b  == Map(s) = "EDMrand"
           me == <<PubIP(s), seen>>
       IN
       /\ nat' = [nat EXCEPT ![s] = @ \cup {<<0, FacAddr(f), p>>}]
       /\ ctr' = [ctr EXCEPT ![s] = c2]
       /\ adv' = [adv EXCEPT ![s] = me]
       /\ pred' = [pred EXCEPT ![s] = q]
       /\ bday' = [bday EXCEPT ![s] = b]
       /\ facSaw' = [facSaw EXCEPT ![f] = @ \cup {<<s, p>>}]
       /\ IF role[s] = "I"
          THEN /\ phase' = [phase EXCEPT ![s] = "Proposing"]
               /\ Send(s, [t |-> "REQUEST", sess |-> sid[s], adv |-> me, q |-> q, b |-> b])
               /\ UNCHANGED punchVars
          ELSE /\ phase' = [phase EXCEPT ![s] = "Punching"]
               /\ Send(s, [t |-> "READY", sess |-> sid[s], adv |-> me, q |-> q, b |-> b])
               /\ StartPunch(s, peerAdv[s], peerPred[s])
    /\ UNCHANGED <<role, sid, failure, peerAdv, peerPred, peerB, peerDone, fellBack,
                   net, linkUp, facSessions, strangerSaw, persisted, duped>>

ProbeFail(s) ==
    /\ linkUp /\ phase[s] = "Discovering" /\ ~FacAnswers
    /\ phase' = [phase EXCEPT ![s] = "Failed"]
    /\ failure' = [failure EXCEPT ![s] = "ProbeFailed"]
    /\ IF role[s] = "R"
       THEN Send(s, [t |-> "REJECT", sess |-> sid[s], r |-> "UNSUPPORTED"])
       ELSE UNCHANGED <<chan, relaySaw>>
    /\ UNCHANGED <<role, sid, adv, pred, bday, peerAdv, peerPred, peerB, nat,
                   ctr, punchVars, peerDone, fellBack, net, linkUp,
                   facSaw, facSessions, strangerSaw, persisted, duped>>

-----------------------------------------------------------------------------
(* Signals (node direct_link_signal, core Session::signal).                *)
RecvRequest(s) ==
    /\ linkUp /\ chan[s] /= <<>> /\ Front(s).t = "REQUEST"
    /\ LET m == Front(s)
           crossable == phase[s] \in {"Discovering", "Proposing", "AwaitingReady"} /\ role[s] = "I"
           linkInit == IF s = "B" THEN BInitiatedLink ELSE ~BInitiatedLink
           keepOurs == crossable /\ linkInit
           giveWay  == crossable /\ ~linkInit
           busy == phase[s] \notin {"Idle", "Failed"} /\ ~giveWay
           policy == IF s = "B" THEN PolicyB ELSE "ask_host"
           reject == IF policy = "reject" THEN "POLICY"
                     ELSE IF keepOurs THEN "GLARE"
                     ELSE IF busy THEN "BUSY" ELSE "-"
       IN
       IF reject /= "-"
       THEN /\ chan' = [chan EXCEPT ![s] = Tail(@), ![Other(s)] = Append(@,
                   [t |-> "REJECT", sess |-> m.sess, r |-> reject])]
            /\ relaySaw' = relaySaw \cup {"REJECT"}
            /\ UNCHANGED <<phase, role, sid, failure, peerAdv, peerPred, peerB, punchVars>>
       ELSE /\ chan' = [chan EXCEPT ![s] = Tail(@), ![Other(s)] = Append(@,
                   [t |-> "ACCEPT", sess |-> m.sess])]
            /\ relaySaw' = relaySaw \cup {"ACCEPT"}
            /\ phase' = [phase EXCEPT ![s] = "Discovering"]
            /\ role' = [role EXCEPT ![s] = "R"]
            /\ sid' = [sid EXCEPT ![s] = m.sess]
            /\ failure' = [failure EXCEPT ![s] = "-"]
            /\ peerAdv' = [peerAdv EXCEPT ![s] = m.adv]
            /\ peerPred' = [peerPred EXCEPT ![s] = m.q]
            \* The peer's "b" is kept as sent (core Session::set_peer_birthday);
            \* whether this side may act on it is the trust gate, Escalates.
            /\ peerB' = [peerB EXCEPT ![s] = m.b]
            /\ UNCHANGED punchVars
    /\ UNCHANGED <<adv, pred, bday, nat, ctr, peerDone, fellBack, net, linkUp,
                   facSaw, facSessions, strangerSaw, persisted, duped>>

RecvOther(s) ==
    /\ linkUp /\ chan[s] /= <<>> /\ Front(s).t /= "REQUEST"
    /\ chan' = [chan EXCEPT ![s] = Tail(@)]
    /\ LET m == Front(s) IN
       IF m.sess /= sid[s] THEN
            UNCHANGED <<phase, failure, peerAdv, peerPred, peerB, peerDone, punchVars>>
       ELSE CASE
         m.t = "ACCEPT" /\ phase[s] = "Proposing" ->
            /\ phase' = [phase EXCEPT ![s] = "AwaitingReady"]
            /\ UNCHANGED <<failure, peerAdv, peerPred, peerB, peerDone, punchVars>>
      [] m.t = "REJECT" /\ phase[s] \in {"Proposing", "AwaitingReady"} ->
            /\ phase' = [phase EXCEPT ![s] = "Failed"]
            /\ failure' = [failure EXCEPT ![s] =
                  IF m.r = "GLARE" THEN "Glare" ELSE "Rejected(" \o m.r \o ")"]
            /\ UNCHANGED <<peerAdv, peerPred, peerB, peerDone, punchVars>>
      [] m.t = "READY" /\ phase[s] = "AwaitingReady" ->
            /\ phase' = [phase EXCEPT ![s] = "Punching"]
            /\ peerAdv' = [peerAdv EXCEPT ![s] = m.adv]
            /\ peerPred' = [peerPred EXCEPT ![s] = m.q]
            /\ peerB' = [peerB EXCEPT ![s] = m.b]
            /\ StartPunch(s, m.adv, m.q)
            /\ UNCHANGED <<failure, peerDone>>
      [] m.t = "COMPLETE" /\ phase[s] = "Punching" ->
            /\ peerDone' = [peerDone EXCEPT ![s] = TRUE]
            /\ UNCHANGED <<phase, failure, peerAdv, peerPred, peerB, punchVars>>
      [] m.t = "COMPLETE" /\ phase[s] = "Committing" ->
            /\ phase' = [phase EXCEPT ![s] = "Connected"]
            /\ UNCHANGED <<failure, peerAdv, peerPred, peerB, peerDone, punchVars>>
      [] m.t = "REJECT" /\ phase[s] \in {"Punching", "Committing"} ->
            /\ phase' = [phase EXCEPT ![s] = "Failed"]
            /\ failure' = [failure EXCEPT ![s] = "Rejected(" \o m.r \o ")"]
            /\ punch' = [punch EXCEPT ![s] = IF @ = "running" THEN "aborted" ELSE @]
            /\ UNCHANGED <<peerAdv, peerPred, peerB, peerDone, open, active, targets,
                           held, knocking, sprayed, punched, heard, acked, tick,
                           escalated, path>>
      [] OTHER ->
            UNCHANGED <<phase, failure, peerAdv, peerPred, peerB, peerDone, punchVars>>
    /\ UNCHANGED <<role, sid, adv, pred, bday, nat, ctr, fellBack, net, linkUp, knowVars>>

-----------------------------------------------------------------------------
(* The punch (std punch_inner / punch_birthday).                            *)
Punching(s) == phase[s] = "Punching" /\ punch[s] = "running"
Listening(s) == Punching(s) \/ phase[s] \in {"Committing", "Connected"}
PeerPub(s) == peerAdv[s]
ToSide(d, s) == d.dst[1] \in {PubIP(s), PeerIP(s)} \/ d.dst = Lan(s)
StrangerAddr(d) == Stranger /\ d.dst[1] \in Sides \cup {"A2", "B2"} /\ d.dst[2] = StrangerPort
                   /\ \A m \in nat[IF d.dst[1] \in {"A", "A2"} THEN "A" ELSE "B"] : m[3] /= StrangerPort
\* Model choice M1: the network delivers within Latency ticks of GLOBAL
\* time: no side ticks past a datagram (to anyone) that has been in flight
\* that long, and every in-flight datagram ages on every tick. (A per-side
\* bound let one side run its whole window out while the other's answers
\* sat in flight: an unbounded one-way delay the code never meets, its
\* 10 s window being far longer than a round trip.)
DueTo(s) == \E d \in net : ~StrangerAddr(d) /\ d.w >= Latency
Aged(s) == {[d EXCEPT !.w = @ + 1] : d \in net}
SkewOK(s) == phase[Other(s)] \in {"Punching", "AwaitingReady", "Discovering", "Proposing"}
             => tick[s] < tick[Other(s)] + MaxSkew + 1
SilentPrimary(s) == heard[s] = {} /\ acked[s] = {}
\* The trust gate (core direct_link_escalation_allowed, third review): may
\* this side's punch escalate at all (knocks, spray, birthday sockets)? The
\* host's decision for the link when it gave one; else never under
\* accept_all; yes on a link this side initiated; under identified_only, yes
\* for an identified peer; under ask_host, yes as the responder (the session
\* exists only once the host accepted); never under reject.
Policy(s) == IF s = "B" THEN PolicyB ELSE "ask_host"
HostTrust(s) == IF s = "A" THEN HostTrustA ELSE HostTrustB
LinkInitiator(s) == IF s = "B" THEN BInitiatedLink ELSE ~BInitiatedLink
Escalates(s) ==
    IF HostTrust(s) /= "unset" THEN HostTrust(s) = "allow"
    ELSE CASE Policy(s) = "accept_all"      -> FALSE
           [] LinkInitiator(s)              -> TRUE
           [] Policy(s) = "identified_only" -> PeerIdentified
           [] Policy(s) = "ask_host"        -> role[s] = "R"
           [] OTHER                         -> FALSE

\* The peer's birthday mappings toward this side, for the spray oracle.
PeerBdayPorts(s) == {m[3] : m \in {x \in nat[Other(s)] : x[1] # 0}}
SprayDraws(s) ==
    LET cand == {<<PeerPub(s)[1], p>> : p \in Ports} \ (targets[s] \cup sprayed[s] \cup knocking[s])
        hits == {a \in cand : a[2] \in PeerBdayPorts(s)}
        pick == CASE SprayHits = "hit"  -> IF hits /= {} THEN hits ELSE cand
                  [] SprayHits = "miss" -> cand \ hits
                  [] OTHER -> cand
    IN IF peerB[s] /\ escalated[s] /\ SprayBudget /\ Escalates(s) /\ heard[s] = {}
          /\ Cardinality(sprayed[s]) < S
       THEN {{a} : a \in pick} \cup (IF pick = {} THEN {{}} ELSE {})
       ELSE {{}}

RECURSIVE SeqPorts(_, _)
SeqPorts(D, c) ==
    IF D = {} THEN [x \in {} |-> 0]
    ELSE LET x == CHOOSE y \in D : TRUE IN (x :> (c + 1)) @@ SeqPorts(D \ {x}, c + 1)
Allocations(s, k, D) ==
    IF D = {} THEN {[x \in {} |-> 0]}
    ELSE IF Map(s) = "EIM" THEN {[x \in D |-> CHOOSE p \in NewPorts(s, k, x) : TRUE]}
    ELSE IF Map(s) = "EDMseq" THEN {SeqPorts(D, ctr[s])}
    ELSE {f \in [D -> Alloc(s) \ Used(s)] : \A x, y \in D : x /= y => f[x] /= f[y]}

\* Whether a datagram toward address x carries the token: a punch to a
\* target does; a knock to a predicted or sprayed port does not, unless the
\* HijackPlant puts 72be0f9's tokened spray back.
Tokened(kind) == kind \in {"P", "A"}

Tick(s) ==
    /\ linkUp /\ Punching(s) /\ tick[s] < W /\ ~DueTo(s) /\ SkewOK(s)
    /\ LET esc  == ~escalated[s] /\ tick[s] + 1 >= EscAt /\ heard[s] = {}
           drop == ~escalated[s] /\ tick[s] + 1 >= EscAt /\ heard[s] /= {}
           kn   == IF ~Escalates(s) THEN {}
                   ELSE IF esc THEN held[s] ELSE IF heard[s] /= {} THEN {} ELSE knocking[s]
           \* BIRTHDAY_AFTER on two-observer evidence; the full silence gate
           \* (ESCALATE_AFTER) on own evidence alone (Eric's fallback).
           gate == IF Observers >= 2 THEN BdayAt ELSE EscAt
           openB == bday[s] /\ Escalates(s) /\ SprayBudget /\ active[s] = 0 /\ tick[s] + 1 >= gate
                    /\ SilentPrimary(s) /\ open[s] = {0}
           k == active[s]
       IN
       \E spray \in SprayDraws(s) :
       LET knockTo == kn \cup spray
           tokTo == targets[s] \cup (IF HijackPlant \/ MacFrames THEN knockTo ELSE {})
           dests == targets[s] \cup knockTo
           newD == {x \in dests : x[1] /= "lan" /\ MapOf(s, k, x) = {}}
       IN
       \* Model choice M7 (the scale's port pool): a NAT with no port left for
       \* a new destination maps none and the datagram is dropped; likewise
       \* the birthday phase opens nothing if the pool cannot map BdayK more.
       \* (At NP=4 a random NAT runs out; the code's 64k pool does not.)
       LET allocs == Allocations(s, k, newD)
           exhausted == allocs = {}
       IN
       \E f \in (IF exhausted THEN {[x \in {} |-> 0]} ELSE allocs) :
       LET mapped == IF exhausted THEN {} ELSE newD
           nat1 == IF exhausted THEN nat[s] ELSE nat[s] \cup {<<k, x, f[x]>> : x \in newD}
           freeB == Alloc(s) \ {m[3] : m \in nat1}
           gs == IF openB THEN {h \in [1..BdayK -> freeB] : \A i, j \in 1..BdayK : i /= j => h[i] /= h[j]}
                 ELSE {}
           openB2 == openB /\ gs /= {}
       IN
       \E g \in (IF openB2 THEN gs ELSE {[x \in {} |-> 0]}) :
       LET nat2 == IF openB2 THEN nat1 \cup {<<j, PeerPub(s), g[j]>> : j \in 1..BdayK} ELSE nat1
           opened == IF openB2 THEN 0..BdayK ELSE open[s]
           sendable(x) == x[1] = "lan" \/ x \notin newD \/ x \in mapped
           srcOf(x) == IF x[1] = "lan" THEN Lan(s)
                       ELSE <<SrcIP(s, x), CHOOSE p \in {m[3] : m \in {y \in nat2 : y[1] = k /\ y[2] = x}} : TRUE>>
           pk == {[src |-> srcOf(x), dst |-> x, k |-> "P", w |-> 0] : x \in {y \in tokTo : sendable(y)}}
           kk == {[src |-> srcOf(x), dst |-> x, k |-> "K", w |-> 0] : x \in {y \in knockTo \ tokTo : sendable(y)}}
           bsrc(j) == <<PeerIP(s), CHOOSE p \in {m[3] : m \in {y \in nat2 : y[1] = j /\ y[2] = PeerPub(s)}} : TRUE>>
           bk == {[src |-> bsrc(j), dst |-> PeerPub(s), k |-> "P", w |-> 0] : j \in (opened \ {0}) \ {k}}
       IN
       /\ nat' = [nat EXCEPT ![s] = nat2]
       /\ ctr' = [ctr EXCEPT ![s] = IF Map(s) = "EDMseq" THEN @ + Cardinality(mapped) ELSE @]
       /\ held' = [held EXCEPT ![s] = IF esc \/ drop THEN {} ELSE @]
       /\ knocking' = [knocking EXCEPT ![s] = kn]
       /\ escalated' = [escalated EXCEPT ![s] = @ \/ esc \/ drop]
       /\ sprayed' = [sprayed EXCEPT ![s] = @ \cup spray]
       /\ punched' = [punched EXCEPT ![s] = @ \cup {y \in tokTo : sendable(y)}]
       /\ open' = [open EXCEPT ![s] = opened]
       /\ tick' = [tick EXCEPT ![s] = @ + 1]
       /\ net' = Aged(s) \cup pk \cup kk \cup bk
    /\ UNCHANGED <<phase, role, sid, failure, chan, adv, pred, bday, peerAdv,
                   peerPred, peerB, active, targets, heard, acked, punch, path,
                   peerDone, fellBack, linkUp, knowVars>>

DstSide(d) == IF d.dst = Lan("A") \/ d.dst[1] \in {"A", "A2"} THEN "A" ELSE "B"
HairOK(d) == ~SameNAT \/ d.dst[1] = "lan" \/ Hairpin
Reaches(d) == LET s == DstSide(d) IN
              IF d.dst[1] = "lan" THEN active[s] ELSE Admit(s, d.src, d.dst)
Ack(d) == [src |-> d.dst, dst |-> d.src, k |-> "A", w |-> 0]

\* The stranger: a host on the hard side's IP holding StrangerPort. It sees
\* what lands there; it can answer with a valid tokened frame only if a
\* tokened frame ever reached it (it cannot forge the token otherwise).
StrangerGets(d) ==
    /\ d \in net /\ StrangerAddr(d)
    /\ net' = (net \ {d}) \cup
              (IF (Tokened(d.k) /\ ~MacFrames) \/ "tokened" \in strangerSaw
               THEN {[src |-> d.dst, dst |-> d.src, k |-> "P", w |-> 0],
                     [src |-> d.dst, dst |-> d.src, k |-> "A", w |-> 0]}
               ELSE {})
    /\ strangerSaw' = strangerSaw \cup {IF ~Tokened(d.k) THEN "K" ELSE IF MacFrames THEN "mac" ELSE "tokened"}
    /\ UNCHANGED <<phase, role, sid, failure, chan, adv, pred, bday, peerAdv,
                   peerPred, peerB, nat, ctr, punchVars, peerDone, fellBack,
                   linkUp, facSaw, facSessions, relaySaw, persisted, duped>>

Deliver(d) ==
    /\ d \in net /\ ~StrangerAddr(d)
    /\ LET s == DstSide(d)
           k == Reaches(d)
           taken == linkUp /\ Listening(s) /\ HairOK(d) /\ k /= -1
           onActive == taken /\ k = active[s]
           \* std punch_birthday: switch to birthday socket k only while the
           \* session socket has heard and been acked by nothing (review 5).
           switch == taken /\ k /= active[s] /\ k /= 0 /\ active[s] = 0 /\ Punching(s)
                     /\ Tokened(d.k) /\ SilentPrimary(s)
           reply == (onActive \/ switch) /\ d.k = "P"
       IN
       /\ net' = (net \ {d}) \cup (IF reply THEN {Ack(d)} ELSE {})
       /\ nat' = IF reply /\ d.dst[1] /= "lan"
                 THEN [nat EXCEPT ![s] = WithMapping(s, k, d.src,
                          CHOOSE p \in {m[3] : m \in {x \in nat[s] : <<SrcIP(s, x[2]), x[3]>> = d.dst}} : TRUE)]
                 ELSE nat
       /\ IF onActive /\ Punching(s) /\ d.k = "P"
          THEN /\ heard' = [heard EXCEPT ![s] = @ \cup {<<k, d.src>>}]
               \* A source that sent a valid punch proved the token.
               /\ targets' = [targets EXCEPT ![s] = @ \cup {d.src}]
               /\ UNCHANGED <<acked, open, active>>
          ELSE IF onActive /\ Punching(s) /\ d.k = "A"
          THEN \* Ours only if this side sent a tokened punch there (review
               \* 1); with the plant, a sprayed port counts too (72be0f9).
               /\ acked' = [acked EXCEPT ![s] =
                     IF d.src \in punched[s] \/ (HijackPlant /\ d.src \in sprayed[s])
                     THEN @ \cup {<<k, d.src>>} ELSE @]
               /\ UNCHANGED <<heard, targets, open, active>>
          ELSE IF switch
          THEN /\ active' = [active EXCEPT ![s] = k]
               /\ open' = [open EXCEPT ![s] = {k}]
               /\ heard' = [heard EXCEPT ![s] = IF d.k = "P" THEN {<<k, d.src>>} ELSE {}]
               /\ acked' = [acked EXCEPT ![s] = {}]
               /\ targets' = [targets EXCEPT ![s] = @ \cup {d.src}]
          ELSE UNCHANGED <<heard, acked, targets, open, active>>
    /\ UNCHANGED <<phase, role, sid, failure, chan, adv, pred, bday, peerAdv,
                   peerPred, peerB, ctr, held, knocking, sprayed, punched, tick,
                   escalated, punch, path, peerDone, fellBack, linkUp, knowVars>>

Lose(d) ==
    /\ NetLossy /\ d \in net
    /\ net' = net \ {d}
    /\ UNCHANGED <<phase, role, sid, failure, chan, adv, pred, bday, peerAdv,
                   peerPred, peerB, nat, ctr, punchVars, peerDone, fellBack,
                   linkUp, knowVars>>

PunchOK(s) ==
    /\ linkUp /\ Punching(s)
    /\ \E h \in heard[s] : h \in acked[s] /\ h[1] = active[s]
    /\ LET h == CHOOSE x \in heard[s] : x \in acked[s] /\ x[1] = active[s] IN
       /\ path' = [path EXCEPT ![s] = h]
       /\ punch' = [punch EXCEPT ![s] = "ok"]
       /\ phase' = [phase EXCEPT ![s] = IF peerDone[s] THEN "Connected" ELSE "Committing"]
       /\ Send(s, [t |-> "COMPLETE", sess |-> sid[s]])
    /\ UNCHANGED <<role, sid, failure, adv, pred, bday, peerAdv, peerPred, peerB,
                   nat, ctr, open, active, targets, held, knocking, sprayed,
                   punched, heard, acked, tick, escalated, peerDone, fellBack,
                   net, linkUp, facSaw, facSessions, strangerSaw, persisted, duped>>

PunchFail(s) ==
    /\ linkUp /\ Punching(s) /\ tick[s] = W
    /\ ~(\E d \in net : ToSide(d, s) /\ ~StrangerAddr(d))
    /\ ~(\E h \in heard[s] : h \in acked[s] /\ h[1] = active[s])
    /\ punch' = [punch EXCEPT ![s] = "fail"]
    /\ phase' = [phase EXCEPT ![s] = "Failed"]
    /\ failure' = [failure EXCEPT ![s] = "PunchFailed"]
    /\ Send(s, [t |-> "REJECT", sess |-> sid[s], r |-> "PUNCH_FAILED"])
    /\ UNCHANGED <<role, sid, adv, pred, bday, peerAdv, peerPred, peerB, nat, ctr,
                   open, active, targets, held, knocking, sprayed, punched,
                   heard, acked, tick, escalated, path, peerDone, fellBack, net,
                   linkUp, facSaw, facSessions, strangerSaw, persisted, duped>>

-----------------------------------------------------------------------------
Waiting(s) == phase[s] \in {"Proposing", "AwaitingReady", "Committing"}
\* The peer will send this side nothing more: it ended, or sits Idle with
\* nothing on its way to it, or both wait in Committing (each lost the
\* other's COMPLETE). Model choice M3: a phase timeout fires only then; the
\* code's 10-12 s budgets are far longer than a relay round trip.
PeerEnded(s) == \/ phase[Other(s)] \in {"Failed", "Gone", "Connected"}
                \/ (phase[Other(s)] = "Idle" /\ chan[Other(s)] = <<>>)
                \/ (phase[Other(s)] = "Committing" /\ phase[s] = "Committing")
Timeout(s) ==
    /\ linkUp /\ Waiting(s) /\ chan[s] = <<>> /\ (PeerEnded(s) \/ RelayLossy)
    /\ phase' = [phase EXCEPT ![s] = "Failed"]
    /\ failure' = [failure EXCEPT ![s] = "Timeout"]
    /\ UNCHANGED <<role, sid, chan, adv, pred, bday, peerAdv, peerPred, peerB, nat,
                   ctr, punchVars, peerDone, fellBack, net, linkUp, knowVars>>

LoseSignal(s) ==
    /\ RelayLossy /\ chan[s] /= <<>>
    /\ chan' = [chan EXCEPT ![s] = Tail(@)]
    /\ UNCHANGED <<phase, role, sid, failure, adv, pred, bday, peerAdv, peerPred,
                   peerB, nat, ctr, punchVars, peerDone, fellBack, net, linkUp, knowVars>>

\* Model choice M6: a duplicating relay duplicates each signal at most once
\* (an adversary duplicating forever could starve every later signal).
DupSignal(s) ==
    /\ RelayDup /\ chan[s] /= <<>> /\ Front(s) \notin duped
    /\ chan' = [chan EXCEPT ![s] = <<Front(s)>> \o @]
    /\ duped' = duped \cup {Front(s)}
    /\ UNCHANGED <<phase, role, sid, failure, adv, pred, bday, peerAdv, peerPred,
                   peerB, nat, ctr, punchVars, peerDone, fellBack, net, linkUp,
                   facSaw, facSessions, strangerSaw, relaySaw, persisted, duped>>

\* node direct_link_link_removed: any -> none, both ends.
TearDown ==
    /\ Teardown /\ linkUp
    /\ linkUp' = FALSE
    /\ phase' = [s \in Sides |-> IF phase[s] = "Idle" THEN "Idle" ELSE "Gone"]
    /\ chan' = [s \in Sides |-> <<>>]
    /\ punch' = [s \in Sides |-> IF punch[s] = "running" THEN "aborted" ELSE punch[s]]
    /\ UNCHANGED <<role, sid, failure, adv, pred, bday, peerAdv, peerPred, peerB,
                   nat, ctr, open, active, targets, held, knocking, sprayed,
                   punched, heard, acked, tick, escalated, path, peerDone,
                   fellBack, net, knowVars>>

\* direct -> relayed: the direct path goes stale (a mapping the keepalive
\* does not refresh expires) and the link falls back to its relayed path
\* (node direct_link_link_stale / release_direct_attachment). Both ends: the
\* peer stops hearing too and falls back on its own interface timer.
\* Also the recovery of a lone move (gap G2): a direct side whose peer has
\* failed hears nothing on the direct interface and falls back too.
FallBack(s) ==
    /\ (~KeepaliveOK \/ phase[Other(s)] \in {"Failed", "Gone"})
    /\ linkUp /\ phase[s] = "Connected" /\ ~fellBack[s]
    /\ fellBack' = [fellBack EXCEPT ![s] = TRUE]
    /\ UNCHANGED <<phase, role, sid, failure, chan, adv, pred, bday, peerAdv,
                   peerPred, peerB, nat, ctr, punchVars, peerDone, net, linkUp, knowVars>>

-----------------------------------------------------------------------------
\* Named, so the state graph labels each delivery by what it was.
DeliverAny == \E d \in net : Deliver(d)
LoseAny == \E d \in net : Lose(d)
StrangerGetsAny == \E d \in net : StrangerGets(d)

Next ==
    \/ \E s \in Sides :
        \/ Propose(s)
        \/ \E p \in Ports : ProbeAnswer(s, p)
        \/ ProbeFail(s)
        \/ RecvRequest(s) \/ RecvOther(s)
        \/ Tick(s) \/ PunchOK(s) \/ PunchFail(s)
        \/ Timeout(s) \/ LoseSignal(s) \/ DupSignal(s) \/ FallBack(s)
    \/ DeliverAny \/ LoseAny \/ StrangerGetsAny
    \/ TearDown

Fairness ==
    /\ \A s \in Sides :
        /\ WF_vars(Propose(s))
        /\ WF_vars(\E p \in Ports : ProbeAnswer(s, p))
        /\ WF_vars(ProbeFail(s))
        /\ WF_vars(RecvRequest(s)) /\ WF_vars(RecvOther(s))
        /\ WF_vars(Tick(s)) /\ WF_vars(PunchOK(s)) /\ WF_vars(PunchFail(s))
        /\ WF_vars(Timeout(s))
        /\ WF_vars(FallBack(s))
    /\ WF_vars(DeliverAny \/ StrangerGetsAny)

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
(* Properties (names agreed with CC).                                       *)
Over == \A s \in Sides : phase[s] \in {"Connected", "Failed", "Gone", "Idle"}
Established == \A s \in Sides : phase[s] = "Connected"

TypeOK == phase \in [Sides -> Phases]

CapsHeld ==
    /\ \A s \in Sides : Cardinality(sprayed[s]) <= S
    /\ \A s \in Sides : Cardinality(open[s] \ {0}) <= BdayK
    /\ \A s \in Sides : Cardinality(pred[s]) <= Q

\* Where socket k of s, sending to x, appears to the peer.
Shows(s, k, x) ==
    IF x[1] = "lan" THEN {Lan(s)}
    ELSE {<<SrcIP(s, m[2]), m[3]>> : m \in {y \in nat[s] : y[1] = k /\ y[2] = x}}

\* The trust gate holds: no knock, spray or birthday socket from a side the
\* gate does not let escalate (core direct_link_escalation_allowed, std
\* punch_with: predicted, spray and extras all need plan.escalate).
EscalationNeedsTrust ==
    \A s \in Sides : (knocking[s] /= {} \/ sprayed[s] /= {} \/ open[s] /= {0}) => Escalates(s)

\* A side goes direct only on an address both heard and acked.
PathHeardAndAcked ==
    \A s \in Sides : path[s] /= None => (path[s] \in heard[s] /\ path[s] \in acked[s])

\* Both direct, on one pair: each side's path address is where the other's
\* chosen socket, toward its chosen address, appears. This is also
\* ForgedReflexiveNeverWrongDirect: a direct path always joins A and B, never
\* a stranger or an address a forged probe named.
SamePath ==
    (phase["A"] = "Connected" /\ phase["B"] = "Connected") =>
        /\ path["A"][2] \in Shows("B", path["B"][1], path["B"][2])
        /\ path["B"][2] \in Shows("A", path["A"][1], path["A"][2])
ForgedReflexiveNeverWrongDirect ==
    \A s \in Sides : phase[s] = "Connected" =>
        \E m \in nat[Other(s)] : <<SrcIP(Other(s), m[2]), m[3]>> = path[s][2] \/ path[s][2] = Lan(Other(s))

\* The hijack the review found (item 1): a side settles, even only into the
\* commit, on the stranger's address. Unreachable on the fixed code;
\* reachable with the HijackPlant.
NoPathToStranger ==
    \A s \in Sides : path[s] = None \/ ~Stranger \/ path[s][2][2] /= StrangerPort
                                  \/ path[s][2][1] \notin {Other(s), Other(s) \o "2"}

\* No side direct while the other has failed.
NoLoneMove == ~(\E s \in Sides : phase[s] = "Connected" /\ phase[Other(s)] = "Failed")

\* A clean abort leaves the link as it was: relayed, on both sides.
AbortLeavesLinkUnchanged ==
    (Over /\ \E s \in Sides : phase[s] = "Failed") => \A s \in Sides : linkPath(s) = "relayed"

\* Nothing but probes goes to or comes from a facilitator; the relay carries
\* signals only (it never sees a punch, knock or payload).
FacilitatorCarriesNothing ==
    /\ \A d \in net : ~IsFac(d.dst) /\ ~IsFac(d.src)
    /\ relaySaw \subseteq {"REQUEST", "ACCEPT", "READY", "COMPLETE", "REJECT"}

\* CC 5.4.6. A facilitator learns only the mapping of a node that chose to
\* ask it: a REQUEST naming a facilitator the responder does not itself use
\* never causes a packet from the responder to that party. No identifier
\* links two probes (they carry a nonce, never the session); where both
\* sides chose one facilitator, it can at most correlate them by timing.
FacilitatorKnowledge ==
    /\ \A f \in Facs : \A x \in facSaw[f] : OwnFac(x[1]) = f
    /\ facSessions = {}

\* A stranger on a sprayed IP learns only that a knock arrived.
\* (With MacFrames a stranger may also see a MAC'd frame: the session id
\* travels in clear, the key never; "mac" records that exposure.)
StrangerKnowledge == strangerSaw \subseteq {"K", "mac"}
NoTokenToStranger == "tokened" \notin strangerSaw

\* CC 2.6.8: no observed mapping is ever written into a nodecode, a signed
\* dial hint or any persisted record. The code has no such write; the model
\* has no action that writes `persisted`, so this holds by construction and
\* is listed as such in DirectLinkPunch.md.
NoLearnedAddressPersisted == persisted = {}

NotStuck == Over \/ ENABLED Next

\* linkPath moves only along the six agreed transitions (or stays).
LinkPathStep(s) ==
    \/ linkPath(s)' = linkPath(s)
    \/ <<linkPath(s), linkPath(s)'>> \in
        {<<"none", "relayed">>, <<"relayed", "upgrading">>, <<"upgrading", "direct">>,
         <<"upgrading", "relayed">>, <<"direct", "relayed">>,
         <<"relayed", "none">>, <<"upgrading", "none">>, <<"direct", "none">>}
LinkPathTransitions == [][\A s \in Sides : LinkPathStep(s)]_vars

EventuallyOver == <>[]Over
EventuallyEstablished == <>Established
=============================================================================
