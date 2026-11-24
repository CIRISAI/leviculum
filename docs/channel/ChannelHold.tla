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
(* MaxScale is how far the learned waits can stretch: srtt_cap_ms and the *)
(* queue term together (review 6 finding 3: up to (2.5 + max/min) x SRTT  *)
(* on a tier, so ANY factor). What bounds the span is Share: each        *)
(* ordinary wait is at most its share of the bound (wait_share_ms), the   *)
(* shares summing to at most Bound (the ASSUME).                          *)
(*                                                                         *)
(* Peer evidence (form 1 and form 2): an authenticated packet on another *)
(* link whose far end proved the same identity key (Link::               *)
(* proven_peer_key), or the host vouching for this link's far end on its  *)
(* own cryptographic check (NodeCore::note_peer_evidence), counts as      *)
(* evidence of life exactly like AuthIn. PeerElsewhere says it can come.  *)
(* It teaches no RTT (a sample is a proof of THIS link's attempt).        *)
(*                                                                         *)
(* Review 6 finding 4: the learned waits apply only to a message with    *)
(* evidence since its first send (lastAuth > 0); without, the cold ladder *)
(* (scale 1: today's waits), whatever was learned before.                *)
(*                                                                         *)
(* The receipts (ReceiptsPerAttempt): a proof of an EARLIER attempt (the  *)
(* original that queued, after retransmits) confirms the message and is   *)
(* evidence only when every attempt's receipt is kept; with the receipt   *)
(* replaced on retransmit it matches nothing. The late proofs of other    *)
(* messages' earlier attempts (Sample) likewise. The rule above needs     *)
(* them: DeliveredNeverTorn fails without them (TLC).                     *)
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
    MaxScale,     \* how far a learned wait may stretch (any value)
    Share,        \* each ordinary wait's share of the bound (wait_share_ms)
    PeerSpeaks,   \* authenticated inbound may arrive on this link
    PeerElsewhere, \* evidence of the same peer may come from elsewhere
    ReceiptsPerAttempt, \* every attempt's receipt kept (af1c9067b)
    AckComes      \* the late ack may arrive at all

VARIABLES now, tries, sent, lastAuth, state, scale, scale0, proofIn

vars == <<now, tries, sent, lastAuth, state, scale, scale0, proofIn>>

\* The time an unheld message is torn down today, nothing learned: the sum
\* of its waits.
TodayTeardown == LET RECURSIVE S(_) S(k) == IF k = 0 THEN 0 ELSE Ord[k] + S(k - 1)
                 IN S(Max)

SumShare == LET RECURSIVE S(_) S(k) == IF k = 0 THEN 0 ELSE Share[k] + S(k - 1)
            IN S(Max)

\* The shares fit the bound, and never shorten today's cold waits.
ASSUME MaxScale >= 1 /\ SumShare <= Bound /\ \A k \in 1..Max : Share[k] >= Ord[k]

Min(a, b) == IF a < b THEN a ELSE b

\* An ordinary wait: the learned one, capped at its share.
OrdWait(k, s) == Min(s * Ord[k], Share[k])

\* The held wait: max(CHANNEL_HELD_RETRY_MAX_MS, the last ordinary wait).
HeldWait(s) == IF Cap > OrdWait(Max, s) THEN Cap ELSE OrdWait(Max, s)

Wait(k, s) == IF k <= Max THEN OrdWait(k, s) ELSE HeldWait(s)

\* Each due is recomputed from the current estimate (retransmit_due_ms):
\* the learned one only with evidence since the first send.
Due == sent + Wait(tries, IF lastAuth > 0 THEN scale ELSE 1)

Init == /\ now = 0 /\ tries = 1 /\ sent = 0 /\ lastAuth = -1
        /\ state = "pending"
        /\ scale \in 1..MaxScale /\ scale0 = scale
        /\ proofIn = FALSE

Alive == lastAuth > 0 /\ now < Bound

Tick ==
    /\ state = "pending" /\ now < Due
    /\ now' = now + 1
    /\ UNCHANGED <<tries, sent, lastAuth, state, scale, scale0, proofIn>>

\* Evidence from the peer on this link: a packet that passed the link key
\* AND advanced state (a new channel sequence, a fresh datagram, a proof
\* of an outstanding receipt). Unauthenticated packets and REPLAYS (review
\* 6 finding 1: authentic ciphertext, but they advance nothing) are not
\* modelled because the code counts neither: they move nothing this rule
\* reads, which is what lets PeerSpeaks = FALSE mean a dead peer.
AuthIn ==
    /\ PeerSpeaks /\ state = "pending" /\ lastAuth /= now
    /\ lastAuth' = now
    /\ UNCHANGED <<now, tries, sent, state, scale, scale0, proofIn>>

\* Evidence of the same peer from elsewhere: another link to the same
\* proven identity, or the host's vouching. Tied to the peer: only when the
\* peer is alive somewhere (PeerElsewhere), at most one a tick.
PeerEvidence ==
    /\ PeerElsewhere /\ state = "pending" /\ lastAuth /= now
    /\ lastAuth' = now
    /\ UNCHANGED <<now, tries, sent, state, scale, scale0, proofIn>>

\* A verified proof of an attempt of some other message (this one's own is
\* Ack): an RTT sample, moving the learned timeout anywhere within the cap,
\* up as a queue builds or down as it drains. It is authenticated inbound,
\* and like AuthIn at most one a tick (a proof is a packet: no Zeno run of
\* samples at one instant).
Sample ==
    /\ PeerSpeaks /\ ReceiptsPerAttempt /\ state = "pending" /\ lastAuth /= now
    /\ scale' \in 1..MaxScale /\ scale' /= scale
    /\ lastAuth' = now
    /\ UNCHANGED <<now, tries, sent, state, scale0, proofIn>>

\* A timer fires at its due, or at once when a smaller estimate moved the
\* due into the past (the next poll).
Timeout ==
    /\ state = "pending" /\ now >= Due
    /\ IF tries >= Max /\ ~Alive
       THEN /\ state' = "torn" /\ UNCHANGED <<tries, sent>>
       ELSE /\ tries' = tries + 1
            /\ sent' = now
            /\ UNCHANGED state
    /\ UNCHANGED <<now, lastAuth, scale, scale0, proofIn>>

\* The proof of the latest attempt: its receipt is there either way.
Ack ==
    /\ AckComes /\ state = "pending"
    /\ state' = "acked" /\ proofIn' = TRUE
    /\ UNCHANGED <<now, tries, sent, lastAuth, scale, scale0>>

\* The late proof of an EARLIER attempt (the original, queued): it confirms
\* the message only if that attempt's receipt was kept.
AckEarlier ==
    /\ AckComes /\ state = "pending" /\ tries > 1
    /\ proofIn' = TRUE
    /\ IF ReceiptsPerAttempt
       THEN state' = "acked" /\ lastAuth' = now
       ELSE UNCHANGED <<state, lastAuth>>
    /\ UNCHANGED <<now, tries, sent, scale, scale0>>

Next == Tick \/ AuthIn \/ PeerEvidence \/ Sample \/ Timeout \/ Ack \/ AckEarlier

Spec == Init /\ [][Next]_vars /\ WF_vars(Tick) /\ WF_vars(Timeout)

-----------------------------------------------------------------------------
\* Nothing waits forever on a link this keeps alive.
EventuallyDone == <>(state \in {"acked", "torn"})

\* A held message never outlives the stale bound by more than one held wait.
\* (One held wait at its largest: Cap or the last share, a constant: what
\* finding 3's share cap bounds, however far the learned waits stretch.)
HeldMax == IF Cap > Share[Max] THEN Cap ELSE Share[Max]

HeldBounded == state \in {"pending", "torn"} => now <= Bound + HeldMax

Silent == ~PeerSpeaks /\ ~PeerElsewhere

\* A silent link is torn down exactly when it always was, WHATEVER it
\* learned before (finding 4: no evidence, the cold ladder).
SilentAsToday == (Silent /\ state = "torn") => now = TodayTeardown

\* A silent link is torn down by the stale bound, whatever its timeout
\* learned before (the cap): a dead peer is not noticed later than the
\* link's own stale check would notice it (CIRISEdge#1030).
SilentByBound == (Silent /\ state = "torn") => now <= Bound

\* Without inbound the learned timeout never moves.
NoInboundNoLearning == ~PeerSpeaks => scale = scale0

\* A message whose proof reached the node is never torn down for it: the
\* coupling to the per-attempt receipts.
DeliveredNeverTorn == proofIn => state /= "torn"

\* Torn only when it was out of tries and not alive.
TornOnlyWhenDead == state = "torn" => (tries >= Max /\ ~Alive)
=============================================================================
