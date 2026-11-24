---------------------------- MODULE ChannelHold ----------------------------
(***************************************************************************)
(* leviculum#113: a Channel message out of tries is held, not torn down,  *)
(* while the link is visibly alive (evidence arrived after the message    *)
(* was first sent), up to the link's stale bound.                         *)
(*                                                                         *)
(* Code: core link/channel/mod.rs poll_with_inbound, retransmit_due_ms,   *)
(* mark_delivered_attempt; node/link_management.rs check_channel_timeouts *)
(* and the record_authenticated_inbound call sites; the ReceiptTracker.   *)
(*                                                                         *)
(* Evidence. A packet that passed the link key AND advanced state (a new  *)
(* channel sequence, a fresh datagram, a proof of a receipt, which it     *)
(* consumes); replays within one sequence lap advance nothing and are not *)
(* modelled (past a lap see leviculum#118: bounded by Bound like any      *)
(* evidence). Three sources:                                              *)
(*   AuthIn       the peer's own channel frames / datagrams (PeerSpeaks); *)
(*   PeerEvidence another link to the same proven identity, or the host's *)
(*                note_peer_evidence (PeerElsewhere);                     *)
(*   LateProof    the late proofs of this link's own messages' EARLIER    *)
(*                attempts, queued behind traffic, and of the spent       *)
(*                receipts of messages already confirmed (PeerProves).    *)
(*                They match a receipt, so count, ONLY with the           *)
(*                per-attempt receipts (ReceiptsPerAttempt); without, they *)
(*                arrive and count for nothing.                           *)
(*                                                                         *)
(* The learned timeout. Every attempt-tagged proof is an RTT sample;      *)
(* scale is how far the learned waits stretch (srtt_cap_ms and the queue  *)
(* term together: any factor up to MaxScale). It applies only to a        *)
(* message with evidence since its first send (finding 4): otherwise the  *)
(* cold ladder, Ord, today's waits, whatever they are. A learned ordinary *)
(* wait is max(cold, min(scale x Ord[k], Share[k])) (retransmit_due_ms):  *)
(* capped at its share of the bound, never below the cold wait. Nothing   *)
(* is ASSUMEd between Share and Ord: the code's cold (Karn) SRTT is       *)
(* uncapped. The learned estimate decays to nothing one Bound after its   *)
(* last sample.                                                           *)
(*                                                                         *)
(* RACK while held (RFC 8985). The one message here is the OLDEST out,    *)
(* which is resent at every one of its own timers, held or not (the RTO   *)
(* backstop, re-review 8 R1): only messages BEHIND it are suppressed, and *)
(* those, with RACK's loss detection and the head-of-line rule, are in    *)
(* HeadOfLine.tla and the code's tests. A transmission may be LOST (Lose); *)
(* the next timer resends it, so a loss is repaired within one held wait. *)
(*                                                                         *)
(* Time is whole ticks. The message is first sent at 0.                   *)
(***************************************************************************)
EXTENDS Integers, Sequences

CONSTANTS
    Max,          \* CHANNEL_MAX_TRIES
    Ord,          \* the cold (today's) ordinary waits, one per try 1..Max
    Cap,          \* CHANNEL_HELD_RETRY_MAX_MS at the model's scale
    Bound,        \* the link's stale bound, from the first send
    MaxScale,     \* how far a learned wait may stretch (any value)
    Share,        \* each ordinary wait's share of the bound (wait_share_ms)
    PeerSpeaks,   \* the peer's own frames / datagrams may arrive here
    PeerElsewhere, \* evidence of the same peer may come from elsewhere
    PeerProves,   \* the peer proves this link's messages, late, while pending
    ProofGap,     \* ... at least once every ProofGap ticks
    ReceiptsPerAttempt, \* every attempt's receipt kept (af1c9067b)
    MaxLosses,    \* how many transmissions may be lost (keeps TLC finite)
    AckComes      \* this message's own (late) ack may arrive at all

VARIABLES now, tries, sent, lastAuth, state, scale, scale0, proofIn,
          lastProof, lastSample, lost, lostAt, losses

vars == <<now, tries, sent, lastAuth, state, scale, scale0, proofIn,
          lastProof, lastSample, lost, lostAt, losses>>

Sum(f) == LET RECURSIVE S(_) S(k) == IF k = 0 THEN 0 ELSE f[k] + S(k - 1)
          IN S(Max)

\* The time an unheld message is torn down today: the sum of its cold waits.
TodayTeardown == Sum(Ord)

\* The shares fit the bound (wait_share_ms divides it exactly).
ASSUME MaxScale >= 1 /\ Sum(Share) <= Bound /\ ProofGap >= 1

Min(a, b) == IF a < b THEN a ELSE b
Maxi(a, b) == IF a > b THEN a ELSE b

\* An ordinary wait. s = 1 is the cold ladder (no evidence, or nothing
\* learned): today's wait exactly. Otherwise the learned one, capped at its
\* share, never below the cold one.
OrdWait(k, s) == IF s = 1 THEN Ord[k] ELSE Maxi(Ord[k], Min(s * Ord[k], Share[k]))

\* The held wait: max(CHANNEL_HELD_RETRY_MAX_MS, the last ordinary wait).
HeldWait(s) == Maxi(Cap, OrdWait(Max, s))

Wait(k, s) == IF k <= Max THEN OrdWait(k, s) ELSE HeldWait(s)

\* Each due is recomputed from the current estimate, the learned one only
\* with evidence since the first send.
Due == sent + Wait(tries, IF lastAuth > 0 THEN scale ELSE 1)

Init == /\ now = 0 /\ tries = 1 /\ sent = 0 /\ lastAuth = -1
        /\ state = "pending"
        /\ scale \in 1..MaxScale /\ scale0 = scale
        /\ proofIn = FALSE /\ lastProof = 0 /\ lastSample = 0
        /\ lost = FALSE /\ lostAt = 0 /\ losses = 0

Alive == lastAuth > 0 /\ now < Bound

\* The peer shown alive by evidence within the bound (RACK while held).
Fresh == lastAuth >= 0 /\ now < lastAuth + Bound

\* Time passes; a peer that proves does so within ProofGap; the learned
\* estimate is dropped one Bound after its last sample.
Tick ==
    /\ state = "pending" /\ now < Due
    /\ PeerProves => now - lastProof < ProofGap
    /\ now' = now + 1
    /\ scale' = IF now + 1 >= lastSample + Bound THEN 1 ELSE scale
    /\ UNCHANGED <<tries, sent, lastAuth, state, scale0, proofIn, lastProof, lastSample, lost, lostAt, losses>>

AuthIn ==
    /\ PeerSpeaks /\ state = "pending" /\ lastAuth /= now
    /\ lastAuth' = now
    /\ UNCHANGED <<now, tries, sent, state, scale, scale0, proofIn, lastProof, lastSample, lost, lostAt, losses>>

PeerEvidence ==
    /\ PeerElsewhere /\ state = "pending" /\ lastAuth /= now
    /\ lastAuth' = now
    /\ UNCHANGED <<now, tries, sent, state, scale, scale0, proofIn, lastProof, lastSample, lost, lostAt, losses>>

\* A proof of another message's LATEST attempt: evidence and an RTT sample;
\* its receipt is there either way.
Sample ==
    /\ PeerSpeaks /\ state = "pending" /\ lastAuth /= now
    /\ scale' \in 1..MaxScale /\ scale' /= scale
    /\ lastAuth' = now /\ lastSample' = now
    /\ UNCHANGED <<now, tries, sent, state, scale0, proofIn, lastProof, lost, lostAt, losses>>

\* The late proof of an EARLIER attempt of another message, or of a spent
\* receipt: evidence (and a sample) only with the per-attempt receipts.
LateProof ==
    /\ PeerProves /\ state = "pending" /\ lastProof /= now
    /\ lastProof' = now
    /\ IF ReceiptsPerAttempt
       THEN /\ lastAuth' = now /\ lastSample' = now
            /\ scale' \in 1..MaxScale
       ELSE UNCHANGED <<lastAuth, lastSample, scale>>
    /\ UNCHANGED <<now, tries, sent, state, scale0, proofIn, lost, lostAt, losses>>

\* A timer fires at its due, or at once when a smaller estimate moved the
\* due into the past (the next poll).
Timeout ==
    /\ state = "pending" /\ now >= Due
    /\ IF tries >= Max /\ ~Alive
       THEN /\ state' = "torn" /\ UNCHANGED <<tries, sent, lost, lostAt, losses>>
       ELSE /\ tries' = tries + 1
            /\ sent' = now
            /\ UNCHANGED state
            \* The oldest: resent at every timer, held or not.
            /\ lost' = FALSE /\ UNCHANGED <<lostAt, losses>>
    /\ UNCHANGED <<now, lastAuth, scale, scale0, proofIn, lastProof, lastSample>>

\* The latest transmission is dropped on the way.
Lose ==
    /\ state = "pending" /\ ~lost /\ losses < MaxLosses
    /\ lost' = TRUE /\ lostAt' = now /\ losses' = losses + 1
    /\ UNCHANGED <<now, tries, sent, lastAuth, state, scale, scale0, proofIn, lastProof, lastSample>>

\* The proof of this message's latest attempt (not if it was lost).
Ack ==
    /\ AckComes /\ state = "pending" /\ ~lost
    /\ state' = "acked" /\ proofIn' = TRUE
    /\ UNCHANGED <<now, tries, sent, lastAuth, scale, scale0, lastProof, lastSample, lost, lostAt, losses>>

\* The late proof of an EARLIER attempt of this message (the original,
\* queued): it confirms the message only if that attempt's receipt was kept.
AckEarlier ==
    /\ AckComes /\ state = "pending" /\ tries > 1
    /\ proofIn' = TRUE
    /\ IF ReceiptsPerAttempt
       THEN state' = "acked" /\ lastAuth' = now
       ELSE UNCHANGED <<state, lastAuth>>
    /\ UNCHANGED <<now, tries, sent, scale, scale0, lastProof, lastSample, lost, lostAt, losses>>

Next == Tick \/ AuthIn \/ PeerEvidence \/ Sample \/ LateProof \/ Timeout
        \/ Ack \/ AckEarlier \/ Lose

Spec == Init /\ [][Next]_vars /\ WF_vars(Tick) /\ WF_vars(Timeout)
        /\ WF_vars(LateProof)

-----------------------------------------------------------------------------
\* Nothing waits forever.
EventuallyDone == <>(state \in {"acked", "torn"})

\* One held wait at its largest.
HeldMax == Maxi(Cap, Maxi(Share[Max], Ord[Max]))

\* A message never outlives the later of the stale bound and today's own
\* cold span by more than one held wait (the share caps what was learned).
HeldBounded == state \in {"pending", "torn"} =>
                   now <= Maxi(Bound, TodayTeardown) + HeldMax

Silent == ~PeerSpeaks /\ ~PeerElsewhere /\ ~PeerProves

\* A silent link is torn down exactly when it always was, WHATEVER it
\* learned before (finding 4: no evidence, the cold ladder).
SilentAsToday == (Silent /\ state = "torn") => now = TodayTeardown

\* Without inbound nothing is learned (decay may still drop what was).
NoInboundNoLearning == (~PeerSpeaks /\ ~PeerProves) => scale \in {scale0, 1}

\* The learned estimate is gone one Bound after its last sample.
LearnedDecays == now >= lastSample + Bound => scale = 1

\* A message whose own proof reached the node is never torn down for it.
DeliveredNeverTorn == proofIn => state /= "torn"

\* A link whose only evidence is the late proofs of its own messages
\* (nothing else arrives) is not given up while they keep coming: torn, if
\* at all, only at the stale bound. The coupling to the per-attempt
\* receipts: without them those proofs count for nothing.
HeldWhileProving == (PeerProves /\ ~PeerSpeaks /\ ~PeerElsewhere /\ ProofGap <= Ord[1]
                     /\ state = "torn") => now >= Bound

\* A transmission lost while the message is out is resent within one held
\* wait at the most (the oldest's RTO backstop, re-review 8 R1), held or not.
ResentWithinHeldWait == (lost /\ state = "pending") => now <= lostAt + HeldMax
\* (A lost transmission is so repaired, or the message ends, within one
\* held wait: the bounded, safety form of "eventually resent or the link
\* ends". The temporal form ran TLC out of memory with Lose in the state.)

\* Torn only when it was out of tries and not alive.
TornOnlyWhenDead == state = "torn" => (tries >= Max /\ ~Alive)
=============================================================================
