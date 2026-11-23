---------------------------- MODULE OracleProbe ----------------------------
(* Cross-check of Strategy.tla's plain-punch oracle (HPub / APub) against *)
(* the mechanism. Run DirectLinkPunch with escalation denied on both     *)
(* sides (HostTrustA = HostTrustB = "deny": tokened punches to the        *)
(* advertised address and to proven peer-reflexive sources only, which    *)
(* is the "plain" technique) and print, for every terminal state, the     *)
(* outcome and what each side heard and was acked by (run_strategy.py    *)
(* --oracle compares these with HPub / APub / OtherPortSeen).             *)
EXTENDS DirectLinkPunch, TLC
OtherPort(s) == \E h \in heard[s] : peerAdv[s] /= None /\ h[2][1] = peerAdv[s][1]
                                   /\ h[2] /= peerAdv[s]
Seen(s) == <<heard[s] /= {}, \E h \in heard[s] : h \in acked[s], acked[s] /= {}, OtherPort(s)>>
Terminal == Over /\ (\E s \in Sides : phase[s] /= "Idle")
ProbeOracle ==
    Terminal => PrintT(<<"ORACLE", IF Established THEN "direct" ELSE "relayed",
                         Seen("A"), Seen("B")>>)
=============================================================================
