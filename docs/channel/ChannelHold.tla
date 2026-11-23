---------------------------- MODULE ChannelHold ----------------------------
(***************************************************************************)
(* leviculum#113: a Channel message out of tries is held, not torn down,  *)
(* while the link is visibly alive (an AUTHENTICATED packet arrived after *)
(* the message was first sent), up to the link's stale bound.             *)
(*                                                                         *)
(* Code: core link/channel/mod.rs poll_with_inbound and                    *)
(* retransmit_due_ms (the held wait capped at CHANNEL_HELD_RETRY_MAX_MS),  *)
(* node/link_management.rs check_channel_timeouts (authenticated time and  *)
(* stale_time_ms passed in), the record_authenticated_inbound call sites.  *)
(*                                                                         *)
(* Time is whole ticks. Waits: Ord[k] for ordinary try k, then Cap for     *)
(* every held try. The message is first sent at 0.                        *)
(***************************************************************************)
EXTENDS Integers, Sequences

CONSTANTS
    Max,          \* CHANNEL_MAX_TRIES
    Ord,          \* the ordinary waits, one per try 1..Max
    Cap,          \* the held wait (min(growing backoff, CAP) at the model's scale)
    Bound,        \* the link's stale bound, from the first send
    PeerSpeaks,   \* authenticated inbound may arrive (FALSE: a silent link)
    AckComes      \* the late ack may arrive at all

VARIABLES now, tries, due, lastAuth, state

vars == <<now, tries, due, lastAuth, state>>

\* The time an unheld message is torn down today: the sum of its waits.
TodayTeardown == LET RECURSIVE S(_) S(k) == IF k = 0 THEN 0 ELSE Ord[k] + S(k - 1)
                 IN S(Max)

Init == now = 0 /\ tries = 1 /\ due = Ord[1] /\ lastAuth = -1 /\ state = "pending"

Alive == lastAuth > 0 /\ now < Bound

Tick ==
    /\ state = "pending" /\ now < due
    /\ now' = now + 1
    /\ UNCHANGED <<tries, due, lastAuth, state>>

\* An authenticated packet from the peer (a packet that decrypted, or a
\* verified proof). Unauthenticated packets are not modelled: they move
\* nothing this rule reads.
AuthIn ==
    /\ PeerSpeaks /\ state = "pending" /\ lastAuth /= now
    /\ lastAuth' = now
    /\ UNCHANGED <<now, tries, due, state>>

Timeout ==
    /\ state = "pending" /\ now = due
    /\ IF tries >= Max /\ ~Alive
       THEN /\ state' = "torn" /\ UNCHANGED <<tries, due>>
       ELSE /\ tries' = tries + 1
            /\ due' = now + (IF tries + 1 <= Max THEN Ord[tries + 1] ELSE Cap)
            /\ UNCHANGED state
    /\ UNCHANGED <<now, lastAuth>>

Ack ==
    /\ AckComes /\ state = "pending"
    /\ state' = "acked"
    /\ UNCHANGED <<now, tries, due, lastAuth>>

Next == Tick \/ AuthIn \/ Timeout \/ Ack

Spec == Init /\ [][Next]_vars /\ WF_vars(Tick) /\ WF_vars(Timeout)

-----------------------------------------------------------------------------
\* Nothing waits forever on a link this keeps alive.
EventuallyDone == <>(state \in {"acked", "torn"})

\* A held message never outlives the stale bound by more than one held wait.
HeldBounded == state = "pending" => now <= Bound + Cap

\* A silent link is torn down exactly when it always was.
SilentAsToday == (~PeerSpeaks /\ state = "torn") => now = TodayTeardown

\* Torn only when it was out of tries and not alive.
TornOnlyWhenDead == state = "torn" => (tries >= Max /\ ~Alive)
=============================================================================
