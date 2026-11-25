---------------------------- MODULE HeadOfLine ----------------------------
(***************************************************************************)
(* leviculum#113, RACK while held: the head-of-line rule, the one part of  *)
(* the held channel that needs two messages (ChannelHold.tla has one).     *)
(*                                                                         *)
(* Code: core link/channel/mod.rs poll_with_inbound (behind_oldest,       *)
(* oldest_overdue). On a FIFO path a message B sent after an older message *)
(* A cannot be proved before A is. So while the peer is shown alive       *)
(* (Fresh: evidence within the bound) and A is still out and NOT OVERDUE  *)
(* (A's proof is not yet later than 2 x the latest measured RTT since its *)
(* first send), B's own timer does not end the link. A keeps today's rule *)
(* (out of tries and not alive: torn), and once A is overdue B's does too. *)
(*                                                                         *)
(* The peer gives evidence and proves only until it dies (Death: the last *)
(* tick it can). A's proof is the original, queued: QA ticks after A's    *)
(* send; B's QB after B's, and never before A's. The cold ladder is Ord.  *)
(***************************************************************************)
EXTENDS Integers, Sequences

CONSTANTS
    Max,      \* CHANNEL_MAX_TRIES
    Ord,      \* the cold waits, try 1..Max
    Cap,      \* the held wait's floor (CHANNEL_HELD_RETRY_MAX_MS)
    Bound,    \* the stale bound
    BSend,    \* when B is first sent (A at 0)
    Rtt,      \* the latest measured RTT (A is overdue at 2 x Rtt)
    QA, QB,   \* the queue delays of A's and B's proofs
    Death     \* the peer's last tick of life

VARIABLES now, lastAuth, aState, aTries, aSent, bState, bTries, bSent,
          bTornLive

vars == <<now, lastAuth, aState, aTries, aSent, bState, bTries, bSent, bTornLive>>

Sum(f) == LET RECURSIVE S(_) S(k) == IF k = 0 THEN 0 ELSE f[k] + S(k - 1)
          IN S(Max)
Today == Sum(Ord)
Maxi(a, b) == IF a > b THEN a ELSE b
HeldWait == Maxi(Cap, Ord[Max])
Wait(k) == IF k <= Max THEN Ord[k] ELSE HeldWait

ASSUME BSend > 0 /\ Rtt >= 1 /\ QA >= 1 /\ QB >= 1

Init == /\ now = 0 /\ lastAuth = -1
        /\ aState = "pending" /\ aTries = 1 /\ aSent = 0
        /\ bState = "unsent" /\ bTries = 0 /\ bSent = 0
        /\ bTornLive = FALSE

Fresh == lastAuth >= 0 /\ now < lastAuth + Bound
AliveA == lastAuth > 0 /\ now < Bound
AliveB == lastAuth > BSend /\ now < BSend + Bound
Overdue == now >= 2 * Rtt
Queued == Fresh /\ aState = "pending" /\ ~Overdue
\* The ground truth the rule (Queued) only estimates: the peer is alive,
\* showed itself within the bound, and A's proof is still in the queue.
TrulyQueued == /\ Fresh /\ now <= Death
               /\ aState = "pending" /\ now < QA
LinkTorn == aState = "torn" \/ bState = "torn"
Live == ~LinkTorn

ADue == aSent + Wait(aTries)
BDue == bSent + Wait(bTries)

Tick ==
    /\ Live /\ (aState = "pending" \/ bState \in {"unsent", "pending"})
    /\ aState = "pending" => now < ADue
    /\ bState = "pending" => now < BDue
    /\ bState = "unsent" => now < BSend
    /\ now' = now + 1
    /\ UNCHANGED <<lastAuth, aState, aTries, aSent, bState, bTries, bSent, bTornLive>>

SendB ==
    /\ Live /\ bState = "unsent" /\ now = BSend
    /\ bState' = "pending" /\ bTries' = 1 /\ bSent' = now
    /\ UNCHANGED <<now, lastAuth, aState, aTries, aSent, bTornLive>>

\* Evidence while the peer lives (its frames, other links, the host).
AuthIn ==
    /\ Live /\ now <= Death /\ lastAuth /= now
    /\ lastAuth' = now
    /\ UNCHANGED <<now, aState, aTries, aSent, bState, bTries, bSent, bTornLive>>

\* A's original is proved after its queue delay, if the peer was alive to
\* prove it (it is evidence too).
AckA ==
    /\ Live /\ aState = "pending" /\ now >= QA /\ QA <= Death
    /\ aState' = "acked" /\ lastAuth' = now
    /\ UNCHANGED <<now, aTries, aSent, bState, bTries, bSent, bTornLive>>

\* B's, never before A's (FIFO).
AckB ==
    /\ Live /\ bState = "pending" /\ aState = "acked"
    /\ now >= BSend + QB /\ BSend + QB <= Death
    /\ bState' = "acked" /\ lastAuth' = now
    /\ UNCHANGED <<now, aState, aTries, aSent, bTries, bSent, bTornLive>>

TimeoutA ==
    /\ Live /\ aState = "pending" /\ now >= ADue
    /\ IF aTries >= Max /\ ~AliveA
       THEN aState' = "torn" /\ UNCHANGED <<aTries, aSent>>
       ELSE aTries' = aTries + 1 /\ aSent' = now /\ UNCHANGED aState
    /\ UNCHANGED <<now, lastAuth, bState, bTries, bSent, bTornLive>>

\* B's timer: its own rule ends the link only when it is not queued
\* behind a held, not overdue A (the head-of-line rule).
TimeoutB ==
    /\ Live /\ bState = "pending" /\ now >= BDue
    /\ IF bTries >= Max /\ ~AliveB /\ ~Queued
       THEN /\ bState' = "torn" /\ bTornLive' = TrulyQueued
            /\ UNCHANGED <<bTries, bSent>>
       ELSE bTries' = bTries + 1 /\ bSent' = now
            /\ UNCHANGED <<bState, bTornLive>>
    /\ UNCHANGED <<now, lastAuth, aState, aTries, aSent>>

Next == Tick \/ SendB \/ AuthIn \/ AckA \/ AckB \/ TimeoutA \/ TimeoutB

Spec == Init /\ [][Next]_vars /\ WF_vars(Tick) /\ WF_vars(SendB)
        /\ WF_vars(TimeoutA) /\ WF_vars(TimeoutB)

-----------------------------------------------------------------------------
\* Nothing waits forever.
EventuallyDone == <>(LinkTorn \/ (aState = "acked" /\ bState = "acked"))

\* B is never torn down while A's proof is truly still coming, PROVIDED the
\* queue is not deeper than the rule allows for: A's delay QA at most 2 x the
\* latest measured RTT. Stated against the ground truth, not against the
\* rule's own condition, so it can fail. Without the proviso it does (QA = 9,
\* Rtt = 2, HeadOfLineDeep.cfg): a queue that more than doubled since the
\* last proof, as at its very onset, is not covered, and B ends the link at
\* its own cold ladder unless evidence newer than B's send holds it.
QueuedNotTornAtAll == ~bTornLive
QueuedNotTorn == (QA <= 2 * Rtt) => ~bTornLive

\* Never past the stale bound: every message out ends by the later of the
\* bound and today's cold span after its send, plus one held wait.
NeverPastBound == /\ aState = "pending" => now <= Maxi(Bound, Today) + HeldWait
                  /\ bState = "pending" => now <= BSend + Maxi(Bound, Today) + HeldWait

\* A peer silent from before B's send is found by the later of B's cold
\* ladder and A's overdue point (plus one held wait), never at the bound.
DeadFound == (Live /\ lastAuth < BSend
              /\ now > Maxi(BSend + Today, 2 * Rtt) + HeldWait)
                 => bState /= "pending"
=============================================================================
