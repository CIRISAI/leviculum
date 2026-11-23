-------------------------- MODULE StrategyVectors --------------------------
(* Test vectors for the code's planner (CONFORMANCE.md): for every state  *)
(* TLC reaches in which the controller holds both profiles, print the     *)
(* planner's INPUTS as the code will hold them (classes, flags, the set   *)
(* of failed techniques, the budget left) and Row's answer. The inputs    *)
(* are exactly what Row reads, so each line is one (input, expected plan) *)
(* pair; run_strategy.py --vectors collects and deduplicates them.        *)
EXTENDS Strategy, TLC
VectorOf(c) ==
    LET kn == knows[c]  F == Now(c) IN
    [mapA |-> MapV(kn, F, "A"), mapB |-> MapV(kn, F, "B"),
     filtA |-> FiltV(kn, F, "A"), filtB |-> FiltV(kn, F, "B"),
     stepsA |-> Steps("A"), stepsB |-> Steps("B"),
     v6A |-> V6A, v6B |-> V6B, upnpA |-> UpnpA, upnpB |-> UpnpB,
     facA |-> FacA, facB |-> FacB, samePub |-> SamePub, cands |-> LanWorks,
     gateA |-> EscA, gateB |-> EscB, authWire |-> AuthV(kn), portBlock |-> PortBlock,
     trancheA |-> Tranche("bdayA", F, att), trancheB |-> Tranche("bdayB", F, att),
     failed |-> Failed(F, att), tried |-> tried, budgetLeft |-> PairBudget - escUsed,
     plan |-> Row(kn, F, tried, att), likelihood |-> Likely(kn, F, att, Row(kn, F, tried, att))]
RecordVector ==
    (KAB(knows[Ctl]) /\ ph[Ctl] \in {"gather", "run"}) => PrintT(<<"VECTOR", VectorOf(Ctl)>>)
=============================================================================
