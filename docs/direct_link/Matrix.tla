------------------------------- MODULE Matrix -------------------------------
(* Outcome probe for run_matrix.py: prints the outcome of every terminal   *)
(* state TLC reaches (an always-true invariant with a PrintT side effect). *)
EXTENDS DirectLinkPunch, TLC
OutcomeOf ==
    IF Established THEN "DirectPathUp"
    ELSE IF \A s \in Sides : linkPath(s) = "relayed"
         THEN "RelayedOnly(A:" \o failure["A"] \o ",B:" \o failure["B"] \o ")"
    ELSE IF \A s \in Sides : linkPath(s) = "none" \/ phase[s] = "Gone" THEN "LinkClosed"
    ELSE "BUG(" \o phase["A"] \o "/" \o phase["B"] \o ")"
RecordOutcome == (Over /\ (\E s \in Sides : phase[s] /= "Idle")) => PrintT(<<"OUTCOME", OutcomeOf>>)
=============================================================================
