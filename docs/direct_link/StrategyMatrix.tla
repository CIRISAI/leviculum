--------------------------- MODULE StrategyMatrix ---------------------------
(* Probe for run_strategy.py. Instead of stopping at (or, with -continue, *)
(* printing a full trace for) every violation, it prints the NAME of each *)
(* property a reachable state or step violates, and the outcome of every  *)
(* final state. Traces for chosen counterexamples come from separate runs *)
(* of Strategy.tla on that one property.                                   *)
EXTENDS Strategy, TLC
Final == Over /\ Quiet /\ (BothDirect \/ retry = "no")
OutcomeOf ==
    IF BothDirect THEN "direct:" \o commit["A"]
    ELSE IF SettledRelayed THEN "relayed"
    ELSE "BUG"
TriedAll == {f.t : f \in {g \in ev[Ctl] \cup ev[ini] : g.k \in {"failed", "ok"}}}
RecordOutcome ==
    Final => PrintT(<<"OUTCOME", OutcomeOf, att, tried, TriedAll, luck>>)

InvNames == <<"TypeOK", "OneTechnique", "PlanIsCheapest", "EscPerAttempt", "AttemptCost",
              "CapsHeld", "PlanFollowsEvidence", "DirectWhenPossible", "NoFalseAbort",
              "SamePlan", "Agreement", "NotStuck", "NoRace", "NotRetriedSameWay",
              "RetryIsEvidenceFunction", "StartsWhereItWorked", "EscalationNeedsTrust",
              "AbortLeavesLinkUnchanged", "NoLoneMove", "FacilitatorKnowledge",
              "FacilitatorCarriesNothing", "NoTokenToStranger", "StrangerKnowledge",
              "NoPathToStranger", "NoReplay", "NoRawToCapablePeer", "NoLearnedAddressPersisted",
              "PlanObsToPeerOnly", "ShapedHidesSignals", "AnonymousNeverUpgrades">>
Inv(n) ==
    CASE n = "TypeOK" -> TypeOK [] n = "OneTechnique" -> OneTechnique
      [] n = "PlanIsCheapest" -> PlanIsCheapest [] n = "EscPerAttempt" -> EscPerAttempt
      [] n = "AttemptCost" -> AttemptCost [] n = "CapsHeld" -> CapsHeld
      [] n = "PlanFollowsEvidence" -> PlanFollowsEvidence
      [] n = "DirectWhenPossible" -> DirectWhenPossible [] n = "NoFalseAbort" -> NoFalseAbort
      [] n = "SamePlan" -> SamePlan [] n = "Agreement" -> Agreement
      [] n = "NotStuck" -> NotStuck [] n = "NoRace" -> NoRace
      [] n = "NotRetriedSameWay" -> NotRetriedSameWay
      [] n = "RetryIsEvidenceFunction" -> RetryIsEvidenceFunction
      [] n = "StartsWhereItWorked" -> StartsWhereItWorked
      [] n = "EscalationNeedsTrust" -> EscalationNeedsTrust
      [] n = "AbortLeavesLinkUnchanged" -> AbortLeavesLinkUnchanged
      [] n = "NoLoneMove" -> NoLoneMove [] n = "FacilitatorKnowledge" -> FacilitatorKnowledge
      [] n = "FacilitatorCarriesNothing" -> FacilitatorCarriesNothing
      [] n = "NoTokenToStranger" -> NoTokenToStranger
      [] n = "StrangerKnowledge" -> StrangerKnowledge [] n = "NoPathToStranger" -> NoPathToStranger
      [] n = "NoReplay" -> NoReplay [] n = "NoRawToCapablePeer" -> NoRawToCapablePeer
      [] n = "NoLearnedAddressPersisted" -> NoLearnedAddressPersisted
      [] n = "PlanObsToPeerOnly" -> PlanObsToPeerOnly
      [] n = "ShapedHidesSignals" -> ShapedHidesSignals
      [] n = "AnonymousNeverUpgrades" -> AnonymousNeverUpgrades
InvProbe == \A i \in 1..Len(InvNames) : ~Inv(InvNames[i]) => PrintT(<<"VIOLATED", InvNames[i]>>)
\* Evidence and knowledge only grow (MonotoneEvidence).
LinkStep == \A s \in Sides : ev[s] \subseteq ev'[s] /\ knows[s] \subseteq knows'[s]
\* An always-true action constraint: names the action properties a step breaks.
ActProbe ==
    /\ (~NoSpamStep => PrintT(<<"VIOLATED", "NoSpam">>))
    /\ (~UsesEvidenceStep => PrintT(<<"VIOLATED", "UsesEvidence">>))
    /\ (~VariantStep => PrintT(<<"VIOLATED", "Variant">>))
    /\ (~NoNegligibleStep => PrintT(<<"VIOLATED", "NoNegligibleRow">>))
    /\ (~LinkStep => PrintT(<<"VIOLATED", "MonotoneEvidence">>))
=============================================================================
