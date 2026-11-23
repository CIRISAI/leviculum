# Direct-link evidence catalogue

Each observable is cited to the standard or paper that defines it, in the
standard's own vocabulary: RFC 4787 for mapping and filtering, RFC 5780 for
behaviour discovery, RFC 8445 for candidates. Where no source exists, the
entry says "ours". REFERENCES.md has the full references.

Every observable the two ends, and the parties they talk to, can have about
the path between them: where it comes from in the code, what it tells the
planner, how far to trust it, and how long it stays valid. `Strategy.tla`
models the ones marked **[model]**. The planner (`STRATEGY.md`, `Row` in
`Strategy.tla`) reads them. Everything else is listed so that a change to
the planner states which of these it uses.

Code references:

- **LTS** is `diag/punch-telemetry` at `acb9363` (same `leviculum-std` and `leviculum-core` as `634f6f4`). Its `leviculum-std` and
  `leviculum-core` are identical to `2cf992a`, the head the literature
  review read.
- **main** is `origin/main` at `247d8c6` (v0.35.0+ciris.2).
- **edge** is CIRISEdge `origin/main`.
- `std` is `leviculum-std/src/direct_link.rs`, `core` is
  `leviculum-core/src/direct_link/session.rs`, `wire` is
  `leviculum-core/src/direct_link/wire.rs`, `node` is
  `leviculum-core/src/node/direct_link.rs`, and `udp` is
  `leviculum-std/src/interfaces/direct_udp.rs`.

## Validity classes

A fact is never deleted. A fresher fact of the same kind supersedes it, and
each fact carries the attempt it was observed in (`Fact.n` in the model).
Its validity is one of these:

| class | valid for | model |
|---|---|---|
| **attempt** | this attempt only: raw per-attempt observations | `AttemptScoped` (heard, acked, silent, peerDone) |
| **history(H)** | this attempt and the next H attempts with this peer on this network, then expired (not deleted) | `Valid`, `Hist` (inferences and outcomes: failed, ok, heardOnly, otherPort) |
| **network epoch** | until this side's network changes: a new interface or default route, a new gateway, or a new reflexive IP | the profile (`knows`); the model has no network change (gap M-N1) |

"This network" means the pair of network epochs, this side's and the
peer's. History is about technique names and NAT classes, never addresses.
It lives in memory only (CC 2.6.8; the `NoLearnedAddressPersisted` model
of DirectLinkPunch).

## Primary evidence (the profile)

Each side learns these about itself when it probes. Today the REQUEST and
READY carry only part of them, and the derived hints `q` and `b`. The
planner needs the profile itself; see "Wire" below.

| # | observable | source today | what it tells | reliability | validity |
|---|---|---|---|---|---|
| P1 **[model]** | **Mapping class** per side: EIM / address-dependent / address-and-port-dependent<br>*Defined by:* RFC 4787 §4.1 (EIM / ADM / APDM); RFC 5780 §4.3 (mapping test); RFC 5128 §2.2–2.6; NATCracker §5.3.1 (m ∈ EI/HD/PD). | LTS: `std classify_nat` :1447-1463, run from the session socket toward the facilitator and a second observer (`std` :2434-2443; `second_observer_usable` :1493). `EdmEvidence` :1468-1478. main: `core nat.rs classify` :173 needs answers from two distinct IPs; peers count as probe servers (`std direct_link/nat.rs` :1-28, `PEERS_PROBED_MAX` :105). Telemetry only on main. | Whether the address a facilitator reported is the one the peer's punch must reach. ADM against APDM decides whether two ports on one IP can see it. | High with two observers at distinct IPs. A second observer on the facilitator's IP (LTS's usual port+1) misses address-dependent mapping and classes it EIM. Edge configures no second observer (edge `direct_path.rs` :451-465), so in production the class is `unknown`. | network epoch; main re-probes on new peer addresses, backing off from 30 s to 30 min (`std direct_link/nat.rs` :532-586) |
| P1b **[model]** | **Allocation**: sequential or random, and the measured step<br>*Defined by:* RFC 4787 §4.2.1 (port assignment), §4.2.3; RFC 6056 and RFC 7857 §9 (randomization); NATCracker §5.3.2 (a ∈ PP/PC/RD); HyperDHT `nat.js` CONSISTENT / RANDOM. | LTS: `std allocation_steps` :1381-1405 (two fresh sockets, within 1 s), `steps_from` :1410, `predict_ports` :1547-1565 (step at most `MAX_PREDICT_STEP` 64, :1365) | A sequential EDM side is predictable (row R6). Random steps on an unclassified side are the own-evidence fallback for R7. | Steps measure allocation BETWEEN SOCKETS, not between destinations. An EIM NAT that allocates sockets at random reads exactly like a random EDM; TLC found the planner cannot tell them apart without P1 (scenario `hard-unclassified-eimrandalloc`). A busy CGNAT interleaves other hosts' allocations, so a sequential allocator can read as random. | network epoch (the step); the predicted ports themselves: this attempt, and only if READY comes within the measurement's freshness (gap: no staleness bound today, lit. review 6) |
| P2 | **Port preservation**<br>*Defined by:* RFC 4787 §4.2.1 (port preservation); NATCracker PP. | LTS: `std log_probe_diag` :1907-1913, logged only. main: `NatReport.port_preserved` (`core nat.rs` :102, :134) | A port-preserving EDM maps toward the peer on the local port when it is free, a one-port prediction. | Medium: a port collision breaks it silently. | network epoch |
| P3 **[model]** | **Filtering class**: EIF / ADF / APDF (RFC 4787 5)<br>*Defined by:* RFC 4787 §5 (EIF / ADF / APDF), REQ-8; RFC 5780 §4.4 (filtering test), §7.2 CHANGE-REQUEST, §7.4 OTHER-ADDRESS; NATCracker §5.3.3. | **None** in any code. | Whether a one-EDM pair can punch plain: yes when the EIM side filters by address only. Whether an EDM side is "open" (EDM with EIF). TLC and DirectLinkPunch agree that an open EDM side punches plain against ANY EDM peer, and races against an EIM one. | To add: an RFC 5780-style test. A facilitator (or a main #100 peer observer) answers one probe from another port and another IP; an answer heard means EIF or ADF. RNSP needs a "reply from elsewhere" flag. Until then the planner infers strict filtering from S4. | network epoch |
| P4 | **Hairpin**<br>*Defined by:* RFC 4787 §6 REQ-9; RFC 5780 §3.4; RFC 5128 §3.3.3. | **None.** | Whether two hosts behind one NAT can reach each other on its public address (row R3). | To add: the session socket sends one probe to its own reflexive address. An answer means the NAT hairpins. Cheap; only needed when there are no LAN candidates. | network epoch |
| P5 | **IP pooling** (paired / arbitrary)<br>*Defined by:* RFC 4787 §4.1 (IP address pooling: paired / arbitrary). | main: `classify` compares whole addresses, so a moved IP is EDM (:173-218), and `servers_share_ip` (:106). LTS: `HeardRelation::OtherIp` (`std` :1594-1595), logged only. | Arbitrary pooling puts the mapping toward the peer on another public IP. Prediction and the spray aim at the advertised IP (`std` :1216, `core` :134) and miss. | Medium: OtherIp is also a multi-homed peer. | network epoch |
| P6 **[model]** | **Global IPv6 address**<br>*Defined by:* RFC 8445 §5.1.1.1; RFC 6092 REC-11/14; RFC 8305. | LTS: `std host_candidates` :2122-2149 (one, by route lookup); `core is_global_v6` :146; `same_nat_alternates` sends it always (:106-121). main: `NatReport.global_ipv6` (:109). | A v6 candidate exists (row R2). It does NOT say whether the v6 firewall passes a simultaneous punch; that shows only as S3/S8. | High for "has an address"; unknown for "reachable". Only the default-route address is offered. | network epoch |
| P7 **[model]** | **Router mapping protocol answering** (PCP / NAT-PMP / UPnP-IGD)<br>*Defined by:* RFC 6886; RFC 6887 §11 MAP, §14.1 ANNOUNCE; RFC 6970. | main: `port_mapping` (`mod.rs` `MappingState` :120-170, `CgnatDetected` :147, `ReachabilityReport` :233-257). Maps listeners and the facilitator port only (`driver/port_mapping.rs` `run_keys` :160-180); **the session socket is never mapped** and the mapped address is never a candidate. | Row R5: map the session socket and the side becomes EIM with open filtering toward everyone. | `Mapped` is not proof of reachability (`mod.rs` :124-126). `CgnatDetected` means the mapping opens nothing; its `external_ip` is still a carrier-side candidate (P8). | network epoch; the lease until its expiry (renewed at half-life, engine) |
| P8 **[model]** | **Same LAN / same carrier NAT**<br>*Defined by:* RFC 6598 (shared address space); RFC 6888 (CGN); RFC 5128 §3.3.2. | LTS: the two reflexive IPs are equal (`core` :111) and that is all. main: `netinfo non_public_reason` (:14-19, the RFC 6598 shared range) and `CgnatDetected.external_ip`. | Row R1: punch host or carrier candidates, never through the hairpin. One public IP alone cannot tell a home LAN from a carrier NAT. | To add: compare the host candidates' private subnets (same subnet means same LAN), and exchange carrier-side addresses (the router's CGNAT-side IP from P7, or a host address in the RFC 6598 shared range). Today a carrier-NAT pair punches each other's HOME LAN addresses, which are unreachable and leak tokened frames onto the sender's own LAN (lit. review 11). | network epoch |
| P9 | **Transport kind**: what carries the relayed link, and this host's access (cellular / wifi / wired)<br>*Defined by:* no standard (a prior); Richter 2016 (cellular CGN prevalence). | **None** in the direct-link code. The node's interfaces know the first; the host (edge, mobile OS) knows the second. | Priors only: cellular means likely CGNAT and EDM; a LoRa or serial relay means slow signalling and wider stop conditions. Never a sufficient precondition for any row. | Medium as a prior. | network epoch |
| P10 **[model]** | **The facilitator answered** (a reflexive address exists)<br>*Defined by:* RFC 8489 §14.2 (server-reflexive); RFC 8445 §5.1.1.2. | LTS: `std probe` :288-337; core `probe_failed` :461-474 | Rows R3, R4, R6 and R7 need both reflexive addresses. R1, R2 and R5 do not. | High. | network epoch (see S14) |
| P11 **[model]** | **The trust gate's verdict** per side, and **budget left**<br>*Defined by:* ours (the trust gate); budgets: RFC 8445 §19.5.1, HyperDHT global limit. | LTS: `node direct_link_escalation_allowed` :470-485 (gate); `std KnockBudget` :556-801 (budget). Neither is told to the peer. | The controller must know the PEER may escalate before it plans R6/R7, or a birthday plan meets a peer that may not spray. | High. | gate: per link; budget: its window (`BUDGET_WINDOW` 300 s, `std` :562) |

| P12 **[model: PortBlock]** | **Port block** of the hard side's allocator<br>*Defined by:* RFC 7422; RFC 7768 §2; RFC 6888 REQ-4; Richter 2016 (chunks ≤ 1K–16K in 17 ASes); Keller 2022 ("random" mobile allocators cluster at +1/+2 from the smallest port seen). | **None** today. | Whether R7's spray can target a block instead of 1024–65535: about 100% inside a 1K chunk (SURVEY.md §4). | Medium: from S9 and S10 of earlier attempts, or from P1b samples. | network epoch |
| P13 | **CGN per-subscriber port quota**<br>*Defined by:* RFC 6888 REQ-4 (configurable per-subscriber limit); RFC 7753 (port sets); Richter 2016 (≥ 512 ports per subscriber). | **None** today; to add: PCP port-set responses (RFC 7753), or exhaustion when opening sockets. | Caps the birthday tranche: never more than half the quota. 256 sockets can be half of a 512-port quota. | Low (rarely observable). | network epoch |
| P14 **[model: HelloA/B]** | **The peer speaks the authenticated wire** (HELLO answered)<br>*Defined by:* rns-rs DirectLink protocol, commit `6d21c606` (HELLO, context 0x80), `01f7be6c` (HMAC frames), `f5595aaa` (RNSK). | **None** in ours (leviculum#112). | Whether frames may go to ports that may be anyone's (R6b), and whether the legacy raw token is ever sent (NoRawToCapablePeer). | High. | per link |
| P15 **[model: rev]** | **One side reachable by anyone** (EIM with EIF, or no NAT)<br>*Defined by:* RFC 5128 §3.2 (connection reversal); RFC 4787 §4.1, §5; NATCracker f = EI. | From P1 plus P3 (main #100 `NoNat`; the filtering test). | Row R3b: the other side just sends. | High once P3 exists. | network epoch |
| P16 | **A fixed local port** on the session socket (and the user's static port forward, if any)<br>*Defined by:* Tailscale `EndpointSTUN4LocalPort`, iroh (SURVEY.md §2). | **None**: the session socket binds an ephemeral port (`std bind_session_socket` :2082). | One extra candidate, the reflexive IP plus the local port, nearly free: "worst case it's an invalid candidate mapping" (Tailscale). | Low. | network epoch |
## Secondary evidence (Eric's point)

These come DURING an attempt, mostly to one side only, and today nothing
reads them before the attempt ends. The planner reads every one of them in
the step it arrives (`PlanFollowsEvidence`). The controlled side reports its
own in an OBS signal.

| # | observable | source today | what it tells | reliability | validity |
|---|---|---|---|---|---|
| S1 | **Probe round-trip time, and which attempt answered**<br>*Defined by:* RFC 8489 §6.2.1 (RTO, retransmissions). | `std probe_with` :297-337 draws a fresh nonce per attempt and returns the address only. `log_probe_diag` logs the total elapsed (:1905). | Loss and latency on this side's UDP path. An answer only on the second or third try means a lossy uplink: widen the stop conditions, prefer fewer sockets. | High for this path at this moment. | attempt; a decaying prior for the network epoch |
| S2 | **Relay RTT**<br>*Defined by:* RFC 8445 §14 (Ta, RTO); DCUtR RTT/2. | `node` :1303-1307 (`link.rtt_us()`, used for the responder's start delay) | Scales every stop condition (`STRATEGY.md`, "Stop conditions") and bounds the start skew. | High. | attempt |
| S3 **[model: silent]** | **Time to first heard, and to first acked**<br>*Defined by:* ours as evidence; timings as RFC 8445 §14. | `PunchDiag.first_heard_ms`, `first_acked_ms` (`std` :1694-1695, :1767-1789), logged at the end only. | A clean pair is heard in 7 to 567 ms (`std` :883-884). A technique silent past its stop condition is evidence it fails: the "silent" verdict. | High, except under loss. | attempt |
| S4 **[model: heardOnly]** | **Heard but never acked**: my packets don't arrive<br>*Defined by:* ours (no source names this partial); consistent with RFC 4787 §5. | From the `heard` and `acked` lists (`std` :1143-1144, :1268-1298), logged as `heard_and_acked` (:1825-1828). | With the plain punch: this side is EDM and the peer filters strictly (Strategy.tla `MapV`, `FiltV`). Plain is impossible: move to R6, R7 or R8 at once. | Medium: loss can fake it. The model trusts it only without NetLossy (`NoFalseAbort`). | history(H) for the class inference; attempt for the raw fact |
| S5 | **Acked but never heard**<br>*Defined by:* ours. | The same lists. | Transient: the peer replies but punches elsewhere, because it has not learned this side's peer-reflexive address, or `PEER_REFLEXIVE_MAX` (`std` :380) was reached. Persistent: the peer cannot reach this side where it is. DirectLinkPunch also produces it from an artifact (see below). | Low. | attempt |
| S6 **[model, plain only]** | **Which candidate was heard from**<br>*Defined by:* RFC 8445 §7.2.5.3.1 / §7.3.1.3 (peer-reflexive candidates). | `heard_relation` (`std` :1615-1632, `HeardRelation` :1577-1596), logged as classes (:1860-1861) only. | Same, other port, host candidate, predicted, host IP on another port, or another IP. Each relation is a class of the PEER's NAT. | High: the source proved the token. | history(H), as a class of the peer |
| S7 **[model: otherPort]** | **Same IP, other port**: a symmetric mapping revealed<br>*Defined by:* RFC 4787 §4.1 (a moved mapping is EDM); RFC 5780 §4.3. | `HeardRelation::SameIpOtherPort` (`std` :1583-1584) | The peer's mapping moved: it is EDM. Reclassify, and with both sides EDM go to R8 at once, without waiting for the silence timer. | High. With an open-filtering EDM peer it is a race whether it shows (DirectLinkPunch cross-check). | history(H) |
| S8 | **ICMP errors**<br>*Defined by:* RFC 5128 §6 (security notes); no RFC defines its use as evidence: ours. | `udp` :212-217 (recv error, ignored, "on some platforms"); `std` `recv_errors` :1249-1251 and `send_errors` :1187-1189, counted, not told apart. | Port unreachable: the host answers but nothing listens there (a wrong port; the peer has left). Host or net unreachable: no route, so the v6 row fails at once. | Medium: ICMP is often filtered. Linux reports it on an unconnected socket only with `IP_RECVERR`; to add, per platform. | attempt |
| S9 | **Knocks before the first answer**<br>*Defined by:* RFC 7422, RFC 7768 §2 (port blocks); Keller 2022. | `diag.knocks` (`std` :1210, :1243) counts all knocks. | With S10: how wide the hard side's port pool really is. An early hit means a narrow pool (a port block), which sizes the next attempt's spray. | Medium (one sample). | history(H) |
| S10 | **Which birthday socket won**<br>*Defined by:* as S9. | `diag.birthday_winner` (`std` :1031-1032); `birthday_ignored` (:1004-1006) | Combined with its mapped port (to add: the winner's port relative to the session socket's, never the port itself in a log), it bounds the allocator's block. | Medium. | history(H) |
| S11 **[model: ABORT]** | **The peer's REJECT reason, and when it came**<br>*Defined by:* leviculum wire (REJECT reasons); ICE has no equivalent. | `core` :544-548 (`Failure::Rejected`); `node end_direct_link_session` :1362-1434; reasons in `wire` :43-93 | PUNCH_FAILED is the peer's verdict: the other half of the two-sided stop. BUSY, GLARE and POLICY are not NAT evidence. When it came, against this side's window, measures the skew. **Today a Rejected(PUNCH_FAILED) that arrives first clears the retry (`node` :1417-1421); TLC shows it (RetryIsEvidenceFunction).** | High. | attempt (the verdict); history (the failure) |
| S12 **[model: failed, ok]** | **Outcome and duration of earlier attempts with this peer and on this network**<br>*Defined by:* ours (per-peer history); DCUtR 3 attempts. | Attempt count and backoff only (`node RETRY_BACKOFF_MS` :52, `schedule_direct_link_retry` :560-582). main: `DirectLinkOutcomes` per NAT class, per NODE (`core nat.rs` :355-400). edge holds 1 h after PunchFailed (`direct_path.rs` :190). | Learning: skip what failed, start where it worked. | High. | history(H) |
| S13 | **Link loss during an attempt**<br>*Defined by:* —. | `node direct_link_link_removed` :1681; `direct_link_link_stale` :1991 | The attempt is over. It is NOT evidence about any technique: record no failure. | High. | none |
| S14 **[model]** | **Facilitator silence**<br>*Defined by:* RFC 8489 (no answer); RFC 8445 §5.1.1.2. | `std probe` :333-336; `core probe_failed` :461-474 | No reflexive address on this network. Rows R1, R2 and R5 still stand. **Today a probe failure ends the session at once, even with v6 or a router mapping available** (TLC scenario `fac-silent-a-v6`). | High. | network epoch |
| S15 | **The peer's gate and budget** (P11, told to the peer)<br>*Defined by:* ours. | not exchanged | See P11. | High. | per link |
| S16 | **Path MTU**<br>*Defined by:* RFC 8899 §4–6 (DPLPMTUD). | `std probe_path_mtu` :1968 | Path quality after the commit. Not a strategy input. | Medium (LTS sends without DF; main fixed it in e7ff30853). | the path's lifetime |

| S17 | **Binding lifetime**: how long an idle mapping lives<br>*Defined by:* RFC 5780 §3.3, §4.6; RFC 4787 §4.3 REQ-5 (≥ 2 min recommended); Richter 2016 (CGN UDP timeouts 10–200 s, 74% ≤ 1 min; median 35 s non-cellular, SURVEY.md §3a); Tailscale lifetime "cliffs", Syncthing 180 → 20 s. | **None**: fixed 5 s / 15 s (fast) or 30 s / 120 s (`udp` :44-52). | The keepalive interval and each birthday socket's refresh are FUNCTIONS of it, never constants. A birthday mapping opened early in the window can expire mid-window. | Medium: a probe at increasing intervals. | network epoch |
| S18 | **Low-TTL priming worked**: the hard side's first sends left at a low TTL<br>*Defined by:* HyperDHT TTL 5; Guha 2005 §2.2. | **None**: the TTL is not set (`std birthday_sockets` :1325). | Opens this side's mapping without the packet reaching the far NAT, whose filter would otherwise record a refusal. An input to R7, not a row. | — | attempt |
## What the observers learn

The planner adds no observer. The facilitator still sees only the probes of
the nodes that chose it (`FacilitatorKnowledge`). The relay sees two more
signal types, PLAN and OBS, and their timing: it learns that a re-plan
happened, not why, since the content is the link's encrypted payload
(`FacilitatorCarriesNothing`). Facts carry classes, never addresses.

## A finding about DirectLinkPunch

The cross-check (`run_strategy.py --oracle`) ran all 81 mapping and
filtering pairs. In 4 of them (an EIM side with strict filtering against an
EDM side with open filtering) DirectLinkPunch reaches a state where the EIM
side has been ACKED but has never HEARD the peer. The cause is a modelling
choice: `Deliver` sends the ack from the address the punch arrived at
(`Ack(d)` has `src = d.dst`) even when that socket already holds a mapping
toward the punch's source. A NAT carries one socket's datagrams to one
destination through one mapping. So in the code the ack would leave through
the same mapping as the punches, and the EIM side would see neither. The
outcomes do not change (relayed either way). The partial does, and the
partial is evidence a planner reads. Strategy.tla models the one-mapping
behaviour.

## Gaps (needed by the planner, absent from the code)

Ranked by how many table rows depend on them:

1. **The profile on the wire**: P1, P1b, P3, P6, P7, P8 and P11 as classes in REQUEST and READY. Today only `q`, `b` and the candidates travel.
2. **Live secondary evidence**: S3, S4, S6 and S7 to the planner while the punch runs. Today they go to one log line at the end.
3. **P1 in production**: a distinct-IP second observer in edge, or main #100's peer observers.
4. **P3 filtering test** and **P4 hairpin test**.
5. **P7 for the session socket**: map it, and offer the mapped address as a candidate.
6. **S12 per peer and per network**, in memory.
7. **P8 carrier-side candidates** and a subnet comparison.
8. **S8 ICMP**, per platform.
9. **S9 and S10** port-block evidence.
10. **Network-epoch change detection**, which expires the profile (model gap M-N1).
11. **P14 the authenticated wire** (rns-rs ≥ 0.14, HELLO): the
    precondition of R6b and of NoReplay.
12. **S17 binding lifetime** and an adaptive keepalive; per-socket birthday
    refresh within it.
13. **P12 port block, P13 CGN quota, S18 low-TTL priming**: the inputs that
    size a birthday tranche.
14. **P15 reachability and P16 a fixed local port**: connection reversal
    and the static-forward guess.
