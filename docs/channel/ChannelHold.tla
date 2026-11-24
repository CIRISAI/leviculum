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
(* Part 2, the learned timeout (Channel::mark_delivered_attempt): every    *)
(* proof names its attempt, so every proof is an RTT sample, and the      *)
(* waits scale with the delay the link measured. Scale is that factor:    *)
(* whatever earlier messages taught it (Init: any value up to MaxScale),  *)
(* changed only by a sample, so only when the peer speaks, and re-read on *)
(* every timer, as the code recomputes each due from the current SRTT.    *)
(* MaxScale stands for srtt_cap_ms: at the cap the ordinary span is at    *)
(* most the stale bound (the ASSUME).                                     *)
(*                                                                         *)
(* Peer evidence (form 1 and form 2): an authenticated packet on another *)
(* link whose far end proved the same identity key (Link::               *)
(* proven_peer_key), or the host vouching for this link's far end on its  *)
(* own cryptographic check (NodeCore::note_peer_evidence), counts as      *)
(* evidence of life exactly like AuthIn. PeerElsewhere says it can come.  *)
(* It teaches no RTT (a sample is a proof of THIS link's attempt).        *)
(*                                                                         *)
(* Time is whole ticks. Waits: Scale * Ord[k] for ordinary try k, then the *)
(* held wait for every held try. The message is first sent at 0.          *)
(***************************************************************************)
EXTENDS Integers, Sequences

CONSTANTS
    Max,          \* CHANNEL_MAX_TRIES
    Ord,          \* the ordinary waits, one per try 1..Max, nothing learned
    Cap,          \* CHANNEL_HELD_RETRY_MAX_MS at the model's scale
    Bound,        \* the link's stale bound, from the first send
    MaxScale,     \* the cap on the learned timeout (srtt_cap_ms)
    PeerSpeaks,   \* authenticated inbound may arrive on this link
    PeerElsewhere, \* evidence of the same peer may come from elsewhere
    AckComes      \* the late ack may arrive at all

VARIABLES now, tries, sent, lastAuth, state, scale, scale0

vars == <<now, tries, sent, lastAuth, state, scale, scale0>>

\* The time an unheld message is torn down today, nothing learned: the sum
\* of its waits.
TodayTeardown == LET RECURSIVE S(_) S(k) == IF k = 0 THEN 0 ELSE Ord[k] + S(k - 1)
                 IN S(Max)

\* The cap: at most MaxScale, the ordinary span stays within the bound.
ASSUME MaxScale >= 1 /\ MaxScale * TodayTeardown <= Bound

\* The held wait: max(CHANNEL_HELD_RETRY_MAX_MS, the last ordinary wait).
HeldWait(s) == IF Cap > s * Ord[Max] THEN Cap ELSE s * Ord[Max]

Wait(k, s) == IF k <= Max THEN s * Ord[k] ELSE HeldWait(s)

\* Each due is recomputed from the current estimate (retransmit_due_ms).
Due == sent + Wait(tries, scale)

Init == /\ now = 0 /\ tries = 1 /\ sent = 0 /\ lastAuth = -1
        /\ state = "pending"
        /\ scale \in 1..MaxScale /\ scale0 = scale

Alive == lastAuth > 0 /\ now < Bound

Tick ==
    /\ state = "pending" /\ now < Due
    /\ now' = now + 1
    /\ UNCHANGED <<tries, sent, lastAuth, state, scale, scale0>>

\* An authenticated packet from the peer (a packet that decrypted, or a
\* verified proof). Unauthenticated packets are not modelled: they move
\* nothing this rule reads.
AuthIn ==
    /\ PeerSpeaks /\ state = "pending" /\ lastAuth /= now
    /\ lastAuth' = now
    /\ UNCHANGED <<now, tries, sent, state, scale, scale0>>

\* Evidence of the same peer from elsewhere: another link to the same
\* proven identity, or the host's vouching. Tied to the peer: only when the
\* peer is alive somewhere (PeerElsewhere), at most one a tick.
PeerEvidence ==
    /\ PeerElsewhere /\ state = "pending" /\ lastAuth /= now
    /\ lastAuth' = now
    /\ UNCHANGED <<now, tries, sent, state, scale, scale0>>

\* A verified proof of an attempt of some other message (this one's own is
\* Ack): an RTT sample, moving the learned timeout anywhere within the cap,
\* up as a queue builds or down as it drains. It is authenticated inbound,
\* and like AuthIn at most one a tick (a proof is a packet: no Zeno run of
\* samples at one instant).
Sample ==
    /\ PeerSpeaks /\ state = "pending" /\ lastAuth /= now
    /\ scale' \in 1..MaxScale /\ scale' /= scale
    /\ lastAuth' = now
    /\ UNCHANGED <<now, tries, sent, state, scale0>>

\* A timer fires at its due, or at once when a smaller estimate moved the
\* due into the past (the next poll).
Timeout ==
    /\ state = "pending" /\ now >= Due
    /\ IF tries >= Max /\ ~Alive
       THEN /\ state' = "torn" /\ UNCHANGED <<tries, sent>>
       ELSE /\ tries' = tries + 1
            /\ sent' = now
            /\ UNCHANGED state
    /\ UNCHANGED <<now, lastAuth, scale, scale0>>

Ack ==
    /\ AckComes /\ state = "pending"
    /\ state' = "acked"
    /\ UNCHANGED <<now, tries, sent, lastAuth, scale, scale0>>

Next == Tick \/ AuthIn \/ PeerEvidence \/ Sample \/ Timeout \/ Ack

Spec == Init /\ [][Next]_vars /\ WF_vars(Tick) /\ WF_vars(Timeout)

-----------------------------------------------------------------------------
\* Nothing waits forever on a link this keeps alive.
EventuallyDone == <>(state \in {"acked", "torn"})

\* A held message never outlives the stale bound by more than one held wait.
HeldBounded == state = "pending" => now <= Bound + HeldWait(MaxScale)

\* A silent link that learned nothing is torn down exactly when it always
\* was.
Silent == ~PeerSpeaks /\ ~PeerElsewhere

SilentAsToday == (Silent /\ scale0 = 1 /\ state = "torn") => now = TodayTeardown

\* A silent link is torn down by the stale bound, whatever its timeout
\* learned before (the cap): a dead peer is not noticed later than the
\* link's own stale check would notice it (CIRISEdge#1030).
SilentByBound == (Silent /\ state = "torn") => now <= Bound

\* Without inbound the learned timeout never moves.
NoInboundNoLearning == ~PeerSpeaks => scale = scale0

\* Torn only when it was out of tries and not alive.
TornOnlyWhenDead == state = "torn" => (tries >= Max /\ ~Alive)
=============================================================================
