--------------------------- MODULE DirectLinkPunch ---------------------------
(***************************************************************************)
(* The direct-link punch of leviculum (+ciris), as the code does it at     *)
(* 72be0f9 on diag/punch-telemetry: two nodes A and B share a relayed      *)
(* link, A proposes, both probe a facilitator, both punch, and the link    *)
(* moves only when both punches succeeded (the two-sided commit, #89).     *)
(*                                                                         *)
(* Built from the code, not from intent. Each action names the code it     *)
(* follows:                                                                *)
(*   core  = leviculum-core/src/direct_link/session.rs (Session phases)    *)
(*   node  = leviculum-core/src/node/direct_link.rs   (admission, steps)   *)
(*   wire  = leviculum-core/src/direct_link/wire.rs   (signals, caps)      *)
(*   std   = leviculum-std/src/direct_link.rs         (probe, punch)       *)
(*                                                                         *)
(* Abstractions (each listed in DirectLinkPunch.md, "Model choices"):      *)
(* - time is a tick counter per side inside the punch window; one tick is *)
(*   one PUNCH_INTERVAL round, and every datagram already sent to a side  *)
(*   is delivered (or filtered, or lost) before that side ticks again;    *)
(* - the port space of each public IP is 1..NP; a random allocation and   *)
(*   the spray's port draws are nondeterministic choices, so a birthday   *)
(*   collision is a branch, not a probability;                            *)
(* - the relay carries signals as one FIFO per direction (the RNS Channel *)
(*   is reliable and ordered); RelayLossy / RelayDup break that on        *)
(*   purpose, to show what the code relies on.                            *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, Sequences, TLC

CONSTANTS
    MapA, MapB,          \* "EIM" | "EDMseq" | "EDMrand"   (RFC 4787 4.1)
    FiltA, FiltB,        \* "EIF" | "ADF" | "APDF"          (RFC 4787 5)
    SameNAT,             \* both behind one NAT: host candidates exchanged
    Hairpin,             \* that NAT loops public-to-public back inside
    FacAnswers,          \* the facilitator answers probes
    Pooling,             \* "paired" | "arbitrary" (RFC 4787 4.1, IP pooling)
    Observers,           \* 1..4: facilitators/observing peers (classification only)
    PolicyB,             \* "accept" | "reject"   (B's direct_connect_policy)
    Glare,               \* B proposes too, crossing A's REQUEST
    BInitiatedLink,      \* B initiated the RNS link (wins a crossing)
    RelayLossy,          \* signals may be lost
    RelayDup,            \* signals may be duplicated
    NetLossy,            \* UDP datagrams may be lost (not just filtered)
    Teardown,            \* the link may be torn down mid-attempt
    KeepaliveOK,         \* the direct interface's keepalive beats the NAT's idle timer
    SprayHits,           \* "any" | "hit" | "miss": oracle on the spray's draws
    NP,                  \* ports per public IP
    W,                   \* punch window, in ticks (PUNCH_WINDOW)
    EscAt,               \* ESCALATE_AFTER, in ticks
    BdayAt,              \* BIRTHDAY_AFTER, in ticks
    K,                   \* MAX_BIRTHDAY_SOCKETS (scaled)
    S,                   \* MAX_SPRAY_PORTS (scaled)
    Q,                   \* MAX_PREDICTED_PORTS (scaled)
    MaxSkew,             \* how many ticks one side may run ahead of the other
    Latency              \* receiver ticks a datagram may stay in flight (0: none)

Sides == {"A", "B"}
Other(s) == IF s = "A" THEN "B" ELSE "A"
Map(s)  == IF s = "A" THEN MapA ELSE MapB
Filt(s) == IF s = "A" THEN FiltA ELSE FiltB
Ports == 1..NP
None == <<"none">>

\* An address is <<ip, port>>. A side's public IP is its name; with
\* arbitrary pooling a side's mapping toward the PEER leaves from a second
\* IP (name \o "2") the facilitator never saw. LAN addresses are
\* <<"lan", side>> (port 0), reachable only with SameNAT.
Fac == <<"F", 0>>
PubIP(s) == s
PeerIP(s) == IF Pooling = "arbitrary" /\ Map(s) /= "EIM" THEN s \o "2" ELSE s
Lan(s) == <<"lan", s>>

\* Sockets: 0 is the session socket (probe + punch); 1..K are birthday
\* sockets (std birthday_sockets), opened only by a "b" side.
Socks == 0..K

\* Signals on the relay (wire::Signal), tagged with the session that sent them.
\*   REQUEST carries the initiator's advertised address, predicted ports, "b".
\*   READY   the responder's.  ACCEPT / COMPLETE / REJECT(reason).
Reasons == {"POLICY", "BUSY", "UNSUPPORTED", "PUNCH_FAILED", "GLARE"}

Phases == {"Idle", "Discovering", "Proposing", "AwaitingReady", "Punching",
           "Committing", "Connected", "Failed", "Gone"}

VARIABLES
    phase,      \* core Phase, plus Idle / Failed / Gone (link removed)
    role,       \* "I" (initiator) | "R" (responder) | "-"
    sid,        \* the session this side runs: "A" or "B" (whose proposal)
    failure,    \* why it failed (core Failure), or "-"
    chan,       \* chan[s]: signals on their way TO s (Seq)
    adv,        \* adv[s]: s's own facilitator-observed address, or None
    pred,       \* pred[s]: ports s advertises as predicted ("q")
    bday,       \* bday[s]: s advertises "b"
    peerAdv, peerPred, peerB, \* what s learned from the peer's REQUEST/READY
    nat,        \* nat[s]: set of <<sock, dst, port>> mappings on s's NAT
    ctr,        \* ctr[s]: last sequentially allocated port (EDMseq)
    open,       \* open[s]: sockets s holds open
    active,     \* active[s]: the socket s punches from (0, or the winner)
    targets,    \* punch targets (std punch_inner `targets`)
    held,       \* predicted targets held back until ESCALATE_AFTER
    sprayed,    \* addresses sprayed (std `sprayed`)
    punched,    \* addresses a punch left for (std `punched`)
    heard,      \* <<sock, addr>> a valid punch arrived on/from
    acked,      \* <<sock, addr>> a valid ack arrived on/from, addr punched
    tick,       \* ticks into the window
    escalated,  \* past ESCALATE_AFTER with nothing heard
    punch,      \* "none" | "running" | "ok" | "fail"  (driver's verdict)
    path,       \* <<sock, addr>> the punch settled on, or None
    peerDone,   \* core peer_committed: the peer's COMPLETE arrived
    net,        \* datagrams in flight: [src |-> addr, dst |-> addr, k |-> "P"|"A"]
    expired,    \* a Connected side's mapping went idle (keepalive lost)
    linkUp

vars == <<phase, role, sid, failure, chan, adv, pred, bday, peerAdv, peerPred,
          peerB, nat, ctr, open, active, targets, held, sprayed, punched,
          heard, acked, tick, escalated, punch, path, peerDone, net, expired,
          linkUp>>

punchVars == <<open, active, targets, held, sprayed, punched, heard, acked,
               tick, escalated, punch, path>>

-----------------------------------------------------------------------------
(* NAT: which public port a socket's datagram to dst leaves from, and who   *)
(* may come back in.                                                        *)

Used(s) == {m[3] : m \in nat[s]}
MapOf(s, k, d) == {m[3] : m \in {x \in nat[s] : x[1] = k /\ x[2] = d}}
\* The IP a datagram from s to d leaves with.
SrcIP(s, d) == IF d[1] = "lan" THEN "lan"
               ELSE IF d = Fac THEN PubIP(s) ELSE PeerIP(s)

\* Ports a new mapping of socket k toward d may take (a set: several means
\* the NAT's choice is free). EIM keeps one port per socket whatever the
\* destination; EDMseq takes the next; EDMrand any unused one.
NewPorts(s, k, d) ==
    IF Map(s) = "EIM" THEN
        LET have == {m[3] : m \in {x \in nat[s] : x[1] = k}}
        IN IF have /= {} THEN have ELSE {k + 1}
    ELSE IF Map(s) = "EDMseq" THEN {ctr[s] + 1}
    ELSE Ports \ Used(s)

\* The public address socket k of s sends to d from, if mapped already.
Src(s, k, d) ==
    IF d[1] = "lan" THEN Lan(s)
    ELSE LET p == MapOf(s, k, d) IN
         IF p = {} THEN None ELSE <<SrcIP(s, d), CHOOSE x \in p : TRUE>>

\* Sending creates the mapping if needed: the new nat and ctr.
WithMapping(s, k, d, port) ==
    IF d[1] = "lan" \/ MapOf(s, k, d) /= {} THEN nat[s]
    ELSE nat[s] \cup {<<k, d, port>>}

\* Does s's NAT let a datagram from src in to its public port? Returns the
\* socket it reaches, or -1. (RFC 4787 5: filtering.)
Admit(s, src, dstAddr) ==
    LET ms == {m \in nat[s] : <<SrcIP(s, m[2]), m[3]>> = dstAddr}
        socks == {m[1] : m \in ms}
    IN IF socks = {} THEN -1
       ELSE LET k == CHOOSE x \in socks : TRUE
                ok == CASE Filt(s) = "EIF"  -> TRUE
                        [] Filt(s) = "ADF"  -> \E m \in nat[s] : m[1] = k /\ m[2][1] = src[1]
                        [] Filt(s) = "APDF" -> \E m \in nat[s] : m[1] = k /\ m[2] = src
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
    /\ sprayed = [s \in Sides |-> {}]
    /\ punched = [s \in Sides |-> {}]
    /\ heard = [s \in Sides |-> {}]
    /\ acked = [s \in Sides |-> {}]
    /\ tick = [s \in Sides |-> 0]
    /\ escalated = [s \in Sides |-> FALSE]
    /\ punch = [s \in Sides |-> "none"]
    /\ path = [s \in Sides |-> None]
    /\ peerDone = [s \in Sides |-> FALSE]
    /\ net = {}
    /\ expired = [s \in Sides |-> FALSE]
    /\ linkUp = TRUE

Send(s, m) == chan' = [chan EXCEPT ![Other(s)] = Append(@, m)]

-----------------------------------------------------------------------------
(* Proposing (node propose_direct_link -> core Session::initiate).          *)

\* A proposes; with Glare, B proposes too. The initiator probes first.
Propose(s) ==
    /\ linkUp /\ phase[s] = "Idle"
    /\ (s = "A" \/ Glare)
    /\ phase' = [phase EXCEPT ![s] = "Discovering"]
    /\ role' = [role EXCEPT ![s] = "I"]
    /\ sid' = [sid EXCEPT ![s] = s]
    /\ UNCHANGED <<failure, chan, adv, pred, bday, peerAdv, peerPred, peerB,
                   nat, ctr, punchVars, peerDone, net, expired, linkUp>>

(* The punch starts (std start_punch -> punch_with). Targets: the peer's
   advertised address, its LAN candidate when one NAT is shared (core
   same_nat_alternates), and its predicted ports held back (std punch_inner
   `held`). *)
StartPunch(s, padv, ppred) ==
    /\ targets' = [targets EXCEPT ![s] = {padv} \cup (IF SameNAT THEN {Lan(Other(s))} ELSE {})]
    /\ held' = [held EXCEPT ![s] = {<<padv[1], p>> : p \in ppred} \ {padv}]
    /\ punch' = [punch EXCEPT ![s] = "running"]
    /\ tick' = [tick EXCEPT ![s] = 0]
    /\ UNCHANGED <<open, active, sprayed, punched, heard, acked, escalated, path>>

(* The facilitator probe (std start_probe + allocation_steps + predict_ports).
   The session socket maps toward the facilitator; a side whose port moved
   measures two more allocations: steady (EDMseq) -> predicted ports, random
   (EDMrand) -> "b". EIM keeps its port (port-preserving in the model): it
   advertises neither. Gap G1: an EIM NAT that does not keep the port also
   measures, and advertises "q" (steady) or "b" (random) on 0.34; no traffic
   comes of it unless the punch is silent for ESCALATE_AFTER. *)
ProbeAnswer(s, p) ==
    /\ linkUp /\ phase[s] = "Discovering" /\ FacAnswers
    /\ p \in NewPorts(s, 0, Fac)
    /\ LET c2 == IF Map(s) = "EDMseq" THEN p + 2 ELSE ctr[s]
           q  == IF Map(s) = "EDMseq" THEN {c2 + i : i \in 1..Q} \cap Ports ELSE {}
           b  == Map(s) = "EDMrand"
       IN
       /\ nat' = [nat EXCEPT ![s] = @ \cup {<<0, Fac, p>>}]
       /\ ctr' = [ctr EXCEPT ![s] = c2]
       /\ adv' = [adv EXCEPT ![s] = <<PubIP(s), p>>]
       /\ pred' = [pred EXCEPT ![s] = q]
       /\ bday' = [bday EXCEPT ![s] = b]
       /\ IF role[s] = "I"
          THEN \* core probed_with_candidates, Initiator: send REQUEST.
               /\ phase' = [phase EXCEPT ![s] = "Proposing"]
               /\ Send(s, [t |-> "REQUEST", sess |-> sid[s], adv |-> <<PubIP(s), p>>,
                           q |-> q, b |-> b])
               /\ UNCHANGED punchVars
          ELSE \* Responder: send READY and punch.
               /\ phase' = [phase EXCEPT ![s] = "Punching"]
               /\ Send(s, [t |-> "READY", sess |-> sid[s], adv |-> <<PubIP(s), p>>,
                           q |-> q, b |-> b])
               /\ StartPunch(s, peerAdv[s], peerPred[s])
    /\ UNCHANGED <<role, sid, failure, peerAdv, peerPred, peerB, peerDone, net,
                   expired, linkUp>>

\* core probe_failed: a responder REJECTs UNSUPPORTED; both fail ProbeFailed.
ProbeFail(s) ==
    /\ linkUp /\ phase[s] = "Discovering" /\ ~FacAnswers
    /\ phase' = [phase EXCEPT ![s] = "Failed"]
    /\ failure' = [failure EXCEPT ![s] = "ProbeFailed"]
    /\ IF role[s] = "R"
       THEN Send(s, [t |-> "REJECT", sess |-> sid[s], r |-> "UNSUPPORTED"])
       ELSE UNCHANGED chan
    /\ UNCHANGED <<role, sid, adv, pred, bday, peerAdv, peerPred, peerB, nat,
                   ctr, punchVars, peerDone, net, expired, linkUp>>

-----------------------------------------------------------------------------
(* A signal arrives (node direct_link_signal, core Session::signal).        *)

Front(s) == chan[s][1]
Pop(s) == chan' = [chan EXCEPT ![s] = Tail(@)]

\* REQUEST admission (node: policy, crossing/glare, busy).
RecvRequest(s) ==
    /\ linkUp /\ chan[s] /= <<>> /\ Front(s).t = "REQUEST"
    /\ LET m == Front(s)
           crossable == phase[s] \in {"Discovering", "Proposing", "AwaitingReady"} /\ role[s] = "I"
           linkInit == IF s = "B" THEN BInitiatedLink ELSE ~BInitiatedLink
           keepOurs == crossable /\ linkInit
           giveWay  == crossable /\ ~linkInit
           busy == phase[s] \notin {"Idle", "Failed"} /\ ~giveWay
           reject == IF s = "B" /\ PolicyB = "reject" THEN "POLICY"
                     ELSE IF keepOurs THEN "GLARE"
                     ELSE IF busy THEN "BUSY" ELSE "-"
       IN
       IF reject /= "-"
       THEN /\ chan' = [chan EXCEPT ![s] = Tail(@), ![Other(s)] = Append(@,
                   [t |-> "REJECT", sess |-> m.sess, r |-> reject])]
            /\ UNCHANGED <<phase, role, sid, failure, peerAdv, peerPred, peerB,
                           punchVars>>
       ELSE \* Session::respond: ACCEPT, then probe the facilitator. A node
            \* giving way ends its own proposal (Failure::Glare) first.
            /\ chan' = [chan EXCEPT ![s] = Tail(@), ![Other(s)] = Append(@,
                   [t |-> "ACCEPT", sess |-> m.sess])]
            /\ phase' = [phase EXCEPT ![s] = "Discovering"]
            /\ role' = [role EXCEPT ![s] = "R"]
            /\ sid' = [sid EXCEPT ![s] = m.sess]
            /\ failure' = [failure EXCEPT ![s] = "-"]
            /\ peerAdv' = [peerAdv EXCEPT ![s] = m.adv]
            /\ peerPred' = [peerPred EXCEPT ![s] = m.q]
            /\ peerB' = [peerB EXCEPT ![s] = m.b]
            /\ UNCHANGED punchVars
    /\ UNCHANGED <<adv, pred, bday, nat, ctr, peerDone, net, expired, linkUp>>

\* Every other signal: for this side's session, in a phase that takes it.
RecvOther(s) ==
    /\ linkUp /\ chan[s] /= <<>> /\ Front(s).t /= "REQUEST"
    /\ Pop(s)
    /\ LET m == Front(s) IN
       IF m.sess /= sid[s] THEN
            \* For no session of ours (a stale or given-way one): ignored.
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
            \* The peer's punch failed (0x10): this side aborts, its punch
            \* task dropped (std PunchDiag logs "aborted").
            /\ phase' = [phase EXCEPT ![s] = "Failed"]
            /\ failure' = [failure EXCEPT ![s] = "Rejected(" \o m.r \o ")"]
            /\ punch' = [punch EXCEPT ![s] = IF @ = "running" THEN "aborted" ELSE @]
            /\ UNCHANGED <<peerAdv, peerPred, peerB, peerDone, open, active, targets,
                           held, sprayed, punched, heard, acked, tick, escalated, path>>
      [] OTHER ->
            \* Out of phase (a duplicate READY, a late ACCEPT): ignored.
            UNCHANGED <<phase, failure, peerAdv, peerPred, peerB, peerDone, punchVars>>
    /\ UNCHANGED <<role, sid, adv, pred, bday, nat, ctr, net, expired, linkUp>>

-----------------------------------------------------------------------------
(* The punch (std punch_inner / punch_birthday), one tick at a time.        *)

Punching(s) == phase[s] \in {"Punching"} /\ punch[s] = "running"
\* The direct interface (Committing / Connected) keeps acking late punches.
Listening(s) == Punching(s) \/ phase[s] \in {"Committing", "Connected"}
PeerPub(s) == peerAdv[s]

\* A datagram to s, and one s must take before it ticks again: in flight
\* for Latency of s's ticks already (one tick is PUNCH_INTERVAL, 100 ms; a
\* relay-free round trip spans one or more).
ToSide(d, s) == d.dst[1] \in {PubIP(s), PeerIP(s)} \/ d.dst = Lan(s)
DueTo(s) == \E d \in net : ToSide(d, s) /\ d.w >= Latency
\* s ticks: every datagram still on its way to s has waited one tick more.
Aged(s) == {IF ToSide(d, s) THEN [d EXCEPT !.w = @ + 1] ELSE d : d \in net}

\* Skew: a side may not run more than MaxSkew ticks ahead of a peer that is
\* punching or about to (READY in flight). Gap G2: the code bounds the skew
\* only by RESPONDER_START_DELAY (RTT/2, at most 1.5 s).
SkewOK(s) == phase[Other(s)] \in {"Punching", "AwaitingReady", "Discovering", "Proposing"}
             => tick[s] < tick[Other(s)] + MaxSkew + 1

\* The spray draws for one tick: up to 1 fresh port on the peer's IP
\* (SPRAY_PER_TICK, scaled), within S in all. The oracle SprayHits narrows
\* the draws to the peer's birthday mappings ("hit") or away from them
\* ("miss"); "any" leaves every draw.
PeerBdayPorts(s) == {m[3] : m \in {x \in nat[Other(s)] : x[1] # 0}}
SprayDraws(s) ==
    LET cand == {<<PeerPub(s)[1], p>> : p \in Ports} \ (targets[s] \cup sprayed[s])
        hits == {a \in cand : a[2] \in PeerBdayPorts(s)}
        pick == CASE SprayHits = "hit"  -> IF hits /= {} THEN hits ELSE cand
                  [] SprayHits = "miss" -> cand \ hits
                  [] OTHER -> cand
    IN IF peerB[s] /\ escalated[s] /\ heard[s] = {} /\ Cardinality(sprayed[s]) < S
       THEN {{a} : a \in pick} \cup (IF pick = {} THEN {{}} ELSE {})
       ELSE {{}}

\* Sequential ports for new destinations, in a fixed order (EDMseq).
RECURSIVE SeqPorts(_, _)
SeqPorts(D, c) ==
    IF D = {} THEN [x \in {} |-> 0]
    ELSE LET x == CHOOSE y \in D : TRUE
         IN (x :> (c + 1)) @@ SeqPorts(D \ {x}, c + 1)

\* The ways s's NAT may map the new destinations D of socket k: one
\* function D -> port each. EIM: the socket's one port; EDMseq: the next
\* ones; EDMrand: any injective choice of unused ports.
Allocations(s, k, D) ==
    IF D = {} THEN {[x \in {} |-> 0]}
    ELSE IF Map(s) = "EIM" THEN {[x \in D |-> CHOOSE p \in NewPorts(s, k, x) : TRUE]}
    ELSE IF Map(s) = "EDMseq" THEN {SeqPorts(D, ctr[s])}
    ELSE {f \in [D -> Ports \ Used(s)] :
            \A x, y \in D : x /= y => f[x] /= f[y]}

\* One tick: punch every target from the active socket, spray, keep the
\* birthday sockets' refresh going, and escalate once silent past EscAt
\* (std punch_inner's tick arm; punch_birthday's BIRTHDAY_AFTER).
Tick(s) ==
    /\ linkUp /\ Punching(s) /\ tick[s] < W
    /\ ~DueTo(s) /\ SkewOK(s)
    /\ LET esc == ~escalated[s] /\ tick[s] + 1 >= EscAt /\ heard[s] = {}
           \* Heard already when the escalation point came: drop them (std).
           drop == ~escalated[s] /\ tick[s] + 1 >= EscAt /\ heard[s] /= {}
           tg == targets[s] \cup (IF esc THEN held[s] ELSE {})
           openB == bday[s] /\ active[s] = 0 /\ tick[s] + 1 >= BdayAt
                    /\ heard[s] = {} /\ open[s] = {0}
           k == active[s]
       IN
       \E spray \in SprayDraws(s) :
       LET dests == tg \cup spray
           newD == {x \in dests : x[1] /= "lan" /\ MapOf(s, k, x) = {}}
       IN
       \E f \in Allocations(s, k, newD) :
       LET nat1 == nat[s] \cup {<<k, x, f[x]>> : x \in newD}
           freeB == Ports \ {m[3] : m \in nat1}
       IN
       \E g \in (IF openB THEN {h \in [1..K -> freeB] :
                                  \A i, j \in 1..K : i /= j => h[i] /= h[j]}
                 ELSE {[x \in {} |-> 0]}) :
       LET nat2 == IF openB THEN nat1 \cup {<<j, PeerPub(s), g[j]>> : j \in 1..K} ELSE nat1
           opened == IF openB THEN 0..K ELSE open[s]
           srcOf(x) == IF x[1] = "lan" THEN Lan(s)
                       ELSE <<SrcIP(s, x), CHOOSE p \in {m[3] : m \in {y \in nat2 : y[1] = k /\ y[2] = x}} : TRUE>>
           pk == {[src |-> srcOf(x), dst |-> x, k |-> "P", w |-> 0] : x \in dests}
           bsrc(j) == <<PeerIP(s), CHOOSE p \in {m[3] : m \in {y \in nat2 : y[1] = j /\ y[2] = PeerPub(s)}} : TRUE>>
           bk == {[src |-> bsrc(j), dst |-> PeerPub(s), k |-> "P", w |-> 0] : j \in (opened \ {0}) \ {k}}
       IN
       /\ nat' = [nat EXCEPT ![s] = nat2]
       /\ ctr' = [ctr EXCEPT ![s] = IF Map(s) = "EDMseq" THEN @ + Cardinality(newD) ELSE @]
       /\ targets' = [targets EXCEPT ![s] = tg]
       /\ held' = [held EXCEPT ![s] = IF esc \/ drop THEN {} ELSE @]
       /\ escalated' = [escalated EXCEPT ![s] = @ \/ esc \/ drop]
       /\ sprayed' = [sprayed EXCEPT ![s] = @ \cup spray]
       /\ punched' = [punched EXCEPT ![s] = @ \cup tg]
       /\ open' = [open EXCEPT ![s] = opened]
       /\ tick' = [tick EXCEPT ![s] = @ + 1]
       /\ net' = Aged(s) \cup pk \cup bk
    /\ UNCHANGED <<phase, role, sid, failure, chan, adv, pred, bday, peerAdv,
                   peerPred, peerB, active, heard, acked, punch, path, peerDone,
                   expired, linkUp>>

\* Deliver a datagram (or the NAT drops it). d.dst is an address on one
\* side: its public IP (or pooled IP), or its LAN address.
DstSide(d) == IF d.dst = Lan("A") \/ d.dst[1] \in {"A", "A2"} THEN "A" ELSE "B"
\* Hairpin: a datagram to the public address of a peer behind the same NAT
\* reaches it only if that NAT loops it back.
HairOK(d) == ~SameNAT \/ d.dst[1] = "lan" \/ Hairpin
\* The socket d reaches on its side, or -1 (NAT filter, closed socket).
Reaches(d) == LET s == DstSide(d) IN
              IF d.dst[1] = "lan" THEN active[s] ELSE Admit(s, d.src, d.dst)
Ack(d) == [src |-> d.dst, dst |-> d.src, k |-> "A", w |-> 0]

Deliver(d) ==
    /\ d \in net
    /\ LET s == DstSide(d)
           k == Reaches(d)
           taken == linkUp /\ Listening(s) /\ HairOK(d) /\ k /= -1
           onActive == taken /\ k = active[s]
           \* std birthday_task: the first valid frame on a birthday socket
           \* switches the punch onto it; every other birthday socket and the
           \* session socket close (std interface_for). AS CODED at 72be0f9:
           \* nothing checks whether the session socket had already heard or
           \* been acked (property SamePath).
           switch == taken /\ k /= active[s] /\ k /= 0 /\ active[s] = 0 /\ Punching(s)
           reply == (onActive \/ switch) /\ d.k = "P"
       IN
       /\ net' = (net \ {d}) \cup (IF reply THEN {Ack(d)} ELSE {})
       /\ nat' = IF reply /\ d.dst[1] /= "lan"
                 THEN [nat EXCEPT ![s] = WithMapping(s, k, d.src,
                          CHOOSE p \in {m[3] : m \in {x \in nat[s] : <<SrcIP(s, x[2]), x[3]>> = d.dst}} : TRUE)]
                 ELSE nat
       /\ IF onActive /\ Punching(s) /\ d.k = "P"
          THEN \* Heard: ack it, punch it too (peer-reflexive), and once heard
               \* at the advertised address drop the predicted targets.
               /\ heard' = [heard EXCEPT ![s] = @ \cup {<<k, d.src>>}]
               /\ targets' = [targets EXCEPT ![s] =
                     IF d.src = PeerPub(s)
                     THEN {t \in @ : t = d.src \/ t[1] /= d.src[1] \/ <<k, t>> \in heard[s]} \cup {d.src}
                     ELSE @ \cup {d.src}]
               /\ UNCHANGED <<acked, open, active>>
          ELSE IF onActive /\ Punching(s) /\ d.k = "A"
          THEN \* An ack counts if this side punched or sprayed there.
               /\ acked' = [acked EXCEPT ![s] =
                     IF d.src \in punched[s] \cup sprayed[s] \cup targets[s]
                     THEN @ \cup {<<k, d.src>>} ELSE @]
               /\ UNCHANGED <<heard, targets, open, active>>
          ELSE IF switch
          THEN /\ active' = [active EXCEPT ![s] = k]
               /\ open' = [open EXCEPT ![s] = {k}]
               \* The punch that got through counts as heard (d19ebf3 fix).
               /\ heard' = [heard EXCEPT ![s] = IF d.k = "P" THEN {<<k, d.src>>} ELSE {}]
               /\ acked' = [acked EXCEPT ![s] = {}]
               /\ targets' = [targets EXCEPT ![s] = @ \cup {d.src}]
          ELSE UNCHANGED <<heard, acked, targets, open, active>>
    /\ UNCHANGED <<phase, role, sid, failure, chan, adv, pred, bday, peerAdv,
                   peerPred, peerB, ctr, held, sprayed, punched, tick, escalated,
                   punch, path, peerDone, expired, linkUp>>

\* The network loses a datagram (always possible: UDP).
Lose(d) ==
    /\ NetLossy /\ d \in net
    /\ net' = net \ {d}
    /\ UNCHANGED <<phase, role, sid, failure, chan, adv, pred, bday, peerAdv,
                   peerPred, peerB, nat, ctr, punchVars, peerDone, expired, linkUp>>

\* The punch succeeds: the first address both heard and acked on the active
\* socket (std punch_inner: `acked.iter().find(|a| heard.contains(a))`).
PunchOK(s) ==
    /\ linkUp /\ Punching(s)
    /\ \E h \in heard[s] : h \in acked[s] /\ h[1] = active[s]
    /\ LET h == CHOOSE x \in heard[s] : x \in acked[s] /\ x[1] = active[s] IN
       /\ path' = [path EXCEPT ![s] = h]
       /\ punch' = [punch EXCEPT ![s] = "ok"]
       \* core punched(true), two-sided: COMPLETE; Connected if the peer's
       \* COMPLETE came already, else Committing.
       /\ phase' = [phase EXCEPT ![s] = IF peerDone[s] THEN "Connected" ELSE "Committing"]
       /\ Send(s, [t |-> "COMPLETE", sess |-> sid[s]])
    /\ UNCHANGED <<role, sid, failure, adv, pred, bday, peerAdv, peerPred, peerB,
                   nat, ctr, open, active, targets, held, sprayed, punched, heard,
                   acked, tick, escalated, peerDone, net, expired, linkUp>>

\* The window closes with no path: REJECT(PUNCH_FAILED), Failed(PunchFailed).
PunchFail(s) ==
    /\ linkUp /\ Punching(s) /\ tick[s] = W /\ ~(\E d \in net : ToSide(d, s))
    /\ ~(\E h \in heard[s] : h \in acked[s] /\ h[1] = active[s])
    /\ punch' = [punch EXCEPT ![s] = "fail"]
    /\ phase' = [phase EXCEPT ![s] = "Failed"]
    /\ failure' = [failure EXCEPT ![s] = "PunchFailed"]
    /\ Send(s, [t |-> "REJECT", sess |-> sid[s], r |-> "PUNCH_FAILED"])
    /\ UNCHANGED <<role, sid, adv, pred, bday, peerAdv, peerPred, peerB, nat, ctr,
                   open, active, targets, held, sprayed, punched, heard, acked,
                   tick, escalated, path, peerDone, net, expired, linkUp>>

-----------------------------------------------------------------------------
(* Timeouts (core Session::poll), the relay's faults, teardown, expiry.     *)

\* A phase that waits on the peer times out once nothing it waits for can
\* still come: the peer has ended, and nothing is on its way here.
Waiting(s) == phase[s] \in {"Proposing", "AwaitingReady", "Committing"}
PeerEnded(s) == phase[Other(s)] \in {"Idle", "Failed", "Gone", "Connected"}
             \/ (phase[Other(s)] = "Committing" /\ phase[s] = "Committing")
Timeout(s) ==
    /\ linkUp /\ Waiting(s) /\ chan[s] = <<>>
    /\ PeerEnded(s) \/ RelayLossy
    /\ phase' = [phase EXCEPT ![s] = "Failed"]
    /\ failure' = [failure EXCEPT ![s] = "Timeout"]
    /\ UNCHANGED <<role, sid, chan, adv, pred, bday, peerAdv, peerPred, peerB, nat,
                   ctr, punchVars, peerDone, net, expired, linkUp>>

LoseSignal(s) ==
    /\ RelayLossy /\ chan[s] /= <<>>
    /\ chan' = [chan EXCEPT ![s] = Tail(@)]
    /\ UNCHANGED <<phase, role, sid, failure, adv, pred, bday, peerAdv, peerPred,
                   peerB, nat, ctr, punchVars, peerDone, net, expired, linkUp>>

DupSignal(s) ==
    /\ RelayDup /\ chan[s] /= <<>> /\ Len(chan[s]) < 4
    /\ chan' = [chan EXCEPT ![s] = <<Front(s)>> \o @]
    /\ UNCHANGED <<phase, role, sid, failure, adv, pred, bday, peerAdv, peerPred,
                   peerB, nat, ctr, punchVars, peerDone, net, expired, linkUp>>

\* node direct_link_link_removed: every session and interface goes, both ends.
TearDown ==
    /\ Teardown /\ linkUp
    /\ linkUp' = FALSE
    /\ phase' = [s \in Sides |-> IF phase[s] = "Idle" THEN "Idle" ELSE "Gone"]
    /\ chan' = [s \in Sides |-> <<>>]
    /\ punch' = [s \in Sides |-> IF punch[s] = "running" THEN "aborted" ELSE punch[s]]
    /\ UNCHANGED <<role, sid, failure, adv, pred, bday, peerAdv, peerPred, peerB,
                   nat, ctr, open, active, targets, held, sprayed, punched, heard,
                   acked, tick, escalated, path, peerDone, net, expired>>

\* After the move, a mapping the keepalive does not refresh expires (the
\* direct interface then goes stale: DirectLinkLost, back to the relay).
Expire(s) ==
    /\ ~KeepaliveOK /\ phase[s] = "Connected" /\ ~expired[s]
    /\ expired' = [expired EXCEPT ![s] = TRUE]
    /\ UNCHANGED <<phase, role, sid, failure, chan, adv, pred, bday, peerAdv,
                   peerPred, peerB, nat, ctr, punchVars, peerDone, net, linkUp>>

-----------------------------------------------------------------------------
Next ==
    \/ \E s \in Sides :
        \/ Propose(s)
        \/ \E p \in Ports : ProbeAnswer(s, p)
        \/ ProbeFail(s)
        \/ RecvRequest(s) \/ RecvOther(s)
        \/ Tick(s) \/ PunchOK(s) \/ PunchFail(s)
        \/ Timeout(s) \/ LoseSignal(s) \/ DupSignal(s) \/ Expire(s)
    \/ \E d \in net : Deliver(d) \/ Lose(d)
    \/ TearDown

\* Fairness: every side keeps acting, the relay and the network deliver.
\* Losses are not fair (they may happen, never must).
Fairness ==
    /\ \A s \in Sides :
        /\ WF_vars(Propose(s))
        /\ WF_vars(\E p \in Ports : ProbeAnswer(s, p))
        /\ WF_vars(ProbeFail(s))
        /\ WF_vars(RecvRequest(s)) /\ WF_vars(RecvOther(s))
        /\ WF_vars(Tick(s)) /\ WF_vars(PunchOK(s)) /\ WF_vars(PunchFail(s))
        /\ WF_vars(Timeout(s))
    /\ WF_vars(\E d \in net : Deliver(d))

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
(* Properties.                                                              *)

Terminal(s) == phase[s] \in {"Connected", "Failed", "Gone"}
\* An attempt is over when neither side runs a session (Idle never proposed).
Over == \A s \in Sides : phase[s] \in {"Connected", "Failed", "Gone", "Idle"}
Established == \A s \in Sides : phase[s] = "Connected"
CleanAbort == \A s \in Sides : phase[s] \in {"Failed", "Gone", "Idle"}

TypeOK ==
    /\ phase \in [Sides -> Phases]
    /\ \A s \in Sides : Cardinality(sprayed[s]) <= S
    /\ \A s \in Sides : open[s] \subseteq Socks

\* Caps (wire MAX_SPRAY_PORTS, MAX_BIRTHDAY_SOCKETS, MAX_PREDICTED_PORTS).
CapsHeld ==
    /\ \A s \in Sides : Cardinality(sprayed[s]) <= S
    /\ \A s \in Sides : Cardinality(open[s] \ {0}) <= K
    /\ \A s \in Sides : Cardinality(pred[s]) <= Q

\* Only probes ever go to the facilitator; the relay carries only signals
\* (the net never addresses the facilitator with a punch).
FacilitatorProbesOnly == \A d \in net : d.dst /= Fac /\ d.src /= Fac

\* A side settles on a path only if that address was both heard and acked,
\* on the socket it punches from.
PathHeardAndAcked ==
    \A s \in Sides : path[s] /= None => (path[s] \in heard[s] /\ path[s] \in acked[s])

\* Where socket k of s, sending to x, appears to the peer.
Shows(s, k, x) ==
    IF x[1] = "lan" THEN {Lan(s)}
    ELSE {<<SrcIP(s, m[2]), m[3]>> : m \in {y \in nat[s] : y[1] = k /\ y[2] = x}}

\* Both sides on ONE pair: each side's path address is where the other's
\* chosen socket, sending to its chosen address, appears.
SamePath ==
    (phase["A"] = "Connected" /\ phase["B"] = "Connected") =>
        /\ path["A"][2] \in Shows("B", path["B"][1], path["B"][2])
        /\ path["B"][2] \in Shows("A", path["A"][1], path["A"][2])

\* No side is Connected while the other has failed (it would send into a
\* socket the other released). The code's recovery when it happens anyway:
\* the direct interface goes stale and the link falls back to the relay.
NoLoneMove ==
    ~(\E s \in Sides : phase[s] = "Connected" /\ phase[Other(s)] = "Failed")

\* No stuck state: something can always happen until the attempt is over.
NotStuck == Over \/ ENABLED Next

\* Liveness: every attempt ends; and, per scenario, how.
EventuallyOver == <>[]Over
EventuallyEstablished == <>Established
EventuallyCleanAbort == <>[](Over /\ CleanAbort)
=============================================================================
