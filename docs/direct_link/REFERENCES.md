# References and traceability

The strategy layer **formalizes established practice**: the IETF's NAT
behaviour requirements and traversal techniques, ICE's controlling agent
and nomination, and what shipping systems do. The few things that are
ours are marked as ours in the traceability table (section 2).

Vocabulary follows the sources.

- **Mapping and filtering** are RFC 4787's: EIM / ADM / APDM and EIF / ADF / APDF.
  - Our `EDMseq` / `EDMrand` are an EDM (ADM or APDM) NAT whose port
    assignment (RFC 4787 §4.2.1) is sequential or random.
  - NATCracker writes these as m ∈ {EI, HD, PD}, a ∈ {PP, PC, RD}, f ∈ {EI, HD, PD}.
- **Candidate types** are ICE's (RFC 8445 §5.1.1): host, server-reflexive,
  peer-reflexive, relayed.
- **"Plain punch"** is RFC 5128 §3.3's UDP hole punching.
- **"Knock"** is ours: a tokenless datagram sent only to open our own filter.

Where our usage differs from a source, the row says so.

## 1. Sources

Tags:

- **[V]**: the primary document was opened and the cited section read. Unless noted
  "(me)", this was done by the survey agent on 2026-11-22: SURVEY.md §6, with the URLs
  and its own tags.
- **[V-meta]**: title, status and date only.
- **[NV]**: not verified.

My own checks (nat-strategy, 2026-11-22):

- **NATCracker [V (me)]**: read in full, chapter 5 of the thesis below.
- **draft-seemann-quic-nat-traversal**: a web search confirmed the name and
  revisions -00 to -02. The search index does not show -04. The survey read
  -04 on datatracker, so its [V] stands.
- **Section titles [V (me)]**, checked against the tables of contents in the
  rfc-editor text:
  - RFC 8445: §5.1.2 Prioritizing Candidates, §6.1.1 Determining Role,
    §6.1.2.6 Computing Candidate Pair States, §7.3.1.4 Triggered Checks,
    §8 Concluding ICE Processing, §8.1.1 Nominating Pairs, §14 Setting Ta
    and RTO.
  - RFC 8085: §3.1 Congestion Control Guidelines ("it SHOULD control the
    rate at which it sends UDP datagrams to a destination host"), §3.5
    Middlebox Traversal Guidelines.
  - The body of RFC 8445 §6.1.1 ("The initiating agent that started the ICE
    processing MUST take the controlling role") was reported from a
    summary, not read verbatim: [NV] as a quotation, [V] as a section.

### RFCs

URL form: `https://www.rfc-editor.org/rfc/rfcNNNN#section-X`.

| RFC | title (status) | sections used | tag |
|---|---|---|---|
| 4787 | NAT Behavioral Requirements for Unicast UDP (BCP 127; updated by 6888, 7857) | §4.1 mapping; §4.2.1 port assignment; §4.2.3 parity/contiguity; §4.3 refresh REQ-5, REQ-6; §5 filtering REQ-8; §6 hairpinning REQ-9; §12 REQ-1..14 | [V] |
| 7857 | Updates to NAT Behavioral Requirements (BCP) | §5–7 EIM/EIF; §9 port randomization | [V] |
| 5382 | NAT Behavioral Requirements for TCP (BCP) | §4.2 REQ-2 | [V] |
| 5780 | NAT Behavior Discovery Using STUN (Exp.) | §3.1–3.4; §4.3 mapping test; §4.4 filtering test; §4.6 binding lifetime; §7.2 CHANGE-REQUEST; §7.4 OTHER-ADDRESS | [V] |
| 5128 | State of P2P Communication across NATs (Info.) | §2.2–2.6, §2.10; §3.1 relaying; §3.2 connection reversal; §3.3 UDP hole punching (§3.3.1–3.3.3); §3.5 UDP port prediction; §6 security | [V] |
| 6056 | Transport-Protocol Port Randomization (BCP) | §3.3 | [V] |
| 6888 | Common Requirements for CGNs (BCP) | §3 REQ-1, 2, 4, 7, 9; §5 REQ-13/14/15 | [V] |
| 6598 | Shared Address Space (BCP) | §4, §7 | [V] |
| 7422 | Deterministic Address Mapping for CGN (Info.) | port blocks | [V] |
| 7768 | Port Management to Reduce Logging in Large-Scale NATs (Info.) | §2 | [V] |
| 7753 | PCP Extension for Port-Set Allocation (PS) | §1.1 | [V] |
| 6886 | NAT-PMP (Info.) | §3.2, §3.3 | [V] |
| 6887 | Port Control Protocol (PS) | §8.5 epoch; §11 MAP; §12 PEER; §13.3 FILTER; §14.1 ANNOUNCE | [V] |
| 6970 | UPnP IGD–PCP Interworking Function (PS) | §4, §5.6, §5.9 | [V] |
| 8489 | STUN (PS; obsoletes 5389) | §6.2.1; §14.2 XOR-MAPPED-ADDRESS; §16.2.1 | [V] |
| 8445 | ICE (PS; updated by 8863) | §5.1.1 candidates; §5.1.2 priorities; §6.1.1 roles (controlling/controlled); §6.1.4 checks; §7.2.5.3.1, §7.3.1.3 peer-reflexive; §8, §8.1.1 nomination (aggressive deprecated); §11 keepalive Tr 15 s; §14.2 Ta; §19 security; §19.5.1 amplification | [V] |
| 8838 | Trickle ICE (PS) | §9–14 | [V] |
| 8863 | ICE PAC (PS) | §4 | [V] |
| 7675 | STUN Usage for Consent Freshness (PS) | §5.1 | [V] |
| 6263 | RTP keepalive (PS) | §4, §5, §7 | [V] |
| 8085 | UDP Usage Guidelines (BCP) | §3.5 | [V] |
| 8656 | TURN (PS) | §3 | [V] |
| 8305 | Happy Eyeballs v2 (PS) | §4, §5 | [V] |
| 8899 | DPLPMTUD (PS) | §4–6 | [V] |
| 9000 | QUIC (PS) | §8 address validation; §9 migration | [V] |
| 10069 | Multiple Paths for a QUIC Connection (PS) | §3 | [V] |
| 6092 | IPv6 CPE simple security (Info.) | §3.2.2 REC-11; §3.2.3 REC-14 | [V] |
| 6146, 6877, 7050, 6052 | NAT64, 464XLAT, PREF64 discovery, translation | — | [V-meta] |
| 4380 | Teredo (PS) | prior art on qualification and on choosing a technique from a test | [NV] (not opened by either agent) |

### Internet-Drafts

Datatracker URL form:
`https://datatracker.ietf.org/doc/html/<name>-<rev>`.

| draft | state (date) | sections | tag |
|---|---|---|---|
| draft-seemann-quic-nat-traversal-04 | active (2026-11-02) | §4; §5.1–5.3; §6 PUNCH_GRANT / PUNCH_REQUEST / PUNCH_DONE; §6.4 "TODO"; §8; §9 | [V] |
| draft-bruynooghe-n0-quic-nat-traversal-00 | active (2026-07-06) | §3.4; §3.5.1; §4.1–4.3 | [V] |
| draft-kozuka-quic-stateless-punch-00 | active (2026-11-15) | §3 one-sided punch; §5.1 limits; App. B (fails through EDM) | [V] |
| draft-ietf-quic-address-discovery-01 | QUIC WG (2026-08-15) | §4.1 OBSERVED_ADDRESS; §5; §6 | [V] |
| draft-ietf-masque-connect-udp-listen-16 | RFC Editor queue (2026-10-12) | §2; §7 Proxy-Public-Address; §8.1; §9 | [V] |
| draft-seemann-masque-connect-udp-rendezvous-00 | active (2026-08-16) | §3, §4 | [V] |
| draft-munizaga-quic-alternative-server-address-02 | active (2026-09-25) | §5, §8 | [V] |
| draft-hancke-webrtc-sped-01 | active (2026-11-02) | §3.3 | [V] |
| draft-ietf-mmusic-mdns-ice-candidates-03 | expired (2022) | §3 | [V] |
| draft-takeda-symmetric-nat-traversal-00 | expired (2003) | §6.1, §6.3, §7.2 (§8 [NV]) | [V] |
| draft-duke-quic-natsupp-03, draft-li-quic-nat-optimization-00 | expired | — | [V-meta] |

The survey found no current tsvwg, tram, ice or mmusic draft on NAT
behaviour, symmetric NAT traversal or port prediction. It searched names
and titles, not full text.

### Non-IETF specifications

| spec | where | tag |
|---|---|---|
| libp2p DCUtR r1 (2021-11-20) | https://github.com/libp2p/specs/blob/af185b5/relay/DCUtR.md (RTT/2 sync; 3 attempts) | [V] |
| libp2p AutoNAT v2 r2 (2023-04-15) | libp2p/specs autonat/autonat-v2.md ("Amplification Attack Prevention") | [V] |
| libp2p Circuit Relay v2 r3 | libp2p/specs relay/circuit-v2.md | [V] |
| I2P SSU2 | https://i2p.net/en/docs/specs/ssu2/ (peer test, relay, hole punch, address spoofing) | [V] |
| BEP 55 Holepunch | https://www.bittorrent.org/beps/bep_0055.html | [V] |
| **rns-rs DirectLink protocol** (the same protocol's other implementation) | https://github.com/lelloman/rns-rs/blob/f672d64b0f/docs/direct-link-protocol.md; commits `6d21c606` (HELLO, context 0x80), `01f7be6c` (random nonces, HMAC-tagged 52-byte frames), `f5595aaa` + `3046408c` (RNSK authenticated keepalives, 15 s / 45 s), `fad7129d` ("h"), `e46e48b1` ("m"); first released in rns-net v0.14.0 / rns-core v0.9.0 (2026-11-18) | [V] (SURVEY.md §8) |

### Papers and other documents

| work | used for | tag |
|---|---|---|
| **R. Roverso, S. El-Ansary, S. Haridi, "NATCracker: NAT Combinations Matter", ICCCN 2009, doi 10.1109/ICCCN.2009.5235278**. Full text read in R. Roverso, *A System, Tools and Algorithms for Adaptive HTTP-live Streaming on Peer-to-peer Overlays*, KTH doctoral thesis, Dec 2013, ch. 5 (https://dcatkth.github.io/thesis/roberto_thesis.pdf) | the pair → technique matrix: §5.3 the three policies, 27 types; Thm 5.6.1 (SHP), 5.7.1 (prediction on one side), 5.8.1 (interleaved prediction); coverage 49.2% / +18% / +11.9%, 20.6% untraversable (§5.8.3); birthday "ignored due to low success probability" (§5.8.3); SHP 96% success, PRC-PRC "drops significantly" (§5.9.3). Compared row by row in `natcracker.md`. | [V (me)] |
| B. Ford, P. Srisuresh, D. Kegel, "Peer-to-Peer Communication Across NATs", USENIX ATC 2005 (arXiv cs/0603074) | UDP punching 82% (310/380); hairpin UDP 24% | [V] (§6.2 counts via a summary) |
| S. Guha, P. Francis, "Characterization and Measurement of TCP Traversal through NATs and Firewalls", IMC 2005 | low-TTL priming (§2.2); 29.9% symmetric | [V] |
| Y. Takeda, draft-takeda-symmetric-nat-traversal-00 (2003) | port prediction | [V] |
| P. Richter et al., "A Multi-perspective Analysis of Carrier-Grade NAT Deployment", IMC 2016 | CGN prevalence; port allocation shares; UDP binding timeouts 10–200 s, 74% ≤ 1 min; port chunks | [V] |
| M. Keller, T. Hoßfeld, J. von Mammen, ICCE 2022 (German mobile carriers) | progressing prediction 76–78%; random×progressing 14–17.5%; random clusters at +1/+2 | [V] |
| D. Trautwein et al., arXiv 2510.27500 and 2604.12484 (IMC '26, doi 10.1145/3777912.3809138) | 70% ± 7.1% at scale; 97.6% of successes on attempt 1; §7.1 0.01% both-hard; probing cautions | [V] |
| L. D'Acunto / G. Halkes & J. Pouwelse 2011 (Tribler) | ~64% punchable | [NV] (abstract only) |
| Tailscale, "How NAT traversal works" (2020) | birthday arithmetic; hard+hard → relay | [doc] |
| HyperDHT `lib/holepuncher.js` at `ce23dfd62d` | birthday: 256 sockets / 1,750 probes, TTL 5 priming, per-socket refresh, random×random → no punch, global random-punch limit | [code] |
| Tailscale `87b3a5ff26`, iroh `40833b58fb`, go-libp2p `8582b880e8`, Nebula `7876efaaf4`, Syncthing `0f77b91539`, ZeroTier `899352e384` (and 1.2.12) | local-port guess; lifetime probing; DCUtR retries; punch_back; adaptive keepalive; historical prediction | [code] |
| leviculum `DirectLinkPunch.tla` at `acb936328` (`matrix.md`) | the mechanism; `MacFrames` exploration | this repository |

## 2. Traceability

Each row of the decision table (STRATEGY.md §3) and each property of
`Strategy.tla` is in one of three classes:

- **implements**: implements the source as written;
- **formalizes**: formalizes practice the source describes;
- **ours**: no source found.

The "ours" entries were re-checked against SURVEY.md (21 implementations
read from source). Where a system ships something close, the entry says
so.

### Decision-table rows

| row | class | source |
|---|---|---|
| R1 same network: host candidates | formalizes | RFC 8445 §5.1.1.1 (host candidates), §5.1.2 (host preferred by type preference); RFC 5128 §3.3.2 (peers behind a common NAT); HyperDHT LAN prefix match |
| R1 carrier-side candidates (same CGN) | **ours** | RFC 6598 §4 defines the range; RFC 6887 §8 (ANNOUNCE/MAP) reveals an external address. No surveyed system offers the CGN-side address as a candidate. |
| R1 "never the hairpin when host candidates exist" | formalizes | RFC 8445 §5.1.2 (host type preference above server-reflexive); RFC 5128 §3.3.2 |
| R2 both global v6, v6 only | formalizes the preference; **ours** for the exclusivity | RFC 8305 §4–5 (prefer IPv6, but race); RFC 6092 REC-11/14 (stateful v6 firewall). ICE pairs all candidates (RFC 8445 §6.1.2). Running v6 ALONE, falling back only on its stop condition, is ours. |
| R3 hairpin | formalizes | RFC 4787 §6 REQ-9; RFC 5128 §3.3.3; RFC 6888 REQ-9 |
| R3b connection reversal | implements | RFC 5128 §3.2, as written; Nebula punch_back; BEP 55 |
| R4 plain simultaneous punch | formalizes | RFC 5128 §3.3; RFC 8445 §7 with peer-reflexive learning (§7.2.5.3.1, §7.3.1.3); DCUtR; Ford 2005. Its precondition follows RFC 4787 §4.1, §5; NATCracker Thm 5.6.1 is a sequential variant (`natcracker.md`). |
| R4 the open-EDM reuse race | **ours** | Found by our DirectLinkPunch cross-check (STRATEGY.md §7); consistent with RFC 4787 §5 EIF semantics, but not described in any source we read |
| R5 router mapping of the session socket | formalizes | RFC 6886; RFC 6887 §11 MAP; RFC 6970; Tailscale, iroh, libtorrent (they map their own port, not a per-session socket) |
| R5 the static local-port guess | formalizes | Tailscale `EndpointSTUN4LocalPort`, iroh (SURVEY.md §2): one extra candidate |
| R5b PCP PEER (candidate row) | implements, shipped by nobody | RFC 6887 §12 (learn the external port toward this peer); SURVEY.md §3b.1 |
| R6 one sequential EDM: prediction | formalizes | RFC 5128 §3.5; draft-takeda §6.1; NATCracker PRC (§5.7.1, Thm 5.7.1); Keller 2022; ZeroTier ≤ 1.2.12. **Differs:** NATCracker requires the predicted side's filter < PD; our version needs no such relaxation, because the EIM side knocks and peer-reflexive learning does the rest (`natcracker.md`). |
| R6b both sequential (interleaved) | formalizes | NATCracker §5.8 interleaved PRC-PRC (Thm 5.8.1). The precondition "the authenticated wire, and not both filtering strictly" comes from DirectLinkPunch (`matrix.md` mac-EDMseq-*), which disagrees with NATCracker for both-strict (`natcracker.md`) |
| R6c sequential against random with non-strict filtering (candidate row, not in the table) | formalizes (candidate) | NATCracker Thm 5.7.1 marks it feasible; Keller 2022 measured 14–17.5%; DirectLinkPunch reaches it only with MAC'd frames. Open decision D10. |
| R7 one random EDM: birthday | formalizes | No RFC or I-D. Tailscale blog (arithmetic); HyperDHT (256 sockets, 1,750 probes, random×consistent dispatch); Keller 2022; Trautwein §7.1 |
| R7 per-socket refresh, low-TTL priming | formalizes | HyperDHT (refresh about every 5.1 s; TTL 5); Guha 2005 §2.2; RFC 4787 §4.3 (binding refresh) |
| R7 port-block targeting | formalizes (described, shipped by nobody) | RFC 7422; RFC 7768 §2; RFC 6888 REQ-4; Richter 2016; Keller 2022 (+1/+2 clustering); SURVEY.md §3b.2 |
| R7 tranches 64 → 128 → 256, within 3 per pair; caps 256 / 1,750 | **ours** for the ramp | Caps: HyperDHT's numbers (Eric's decision). Retry count: DCUtR's 3 attempts. The sizing by CGN per-subscriber quota (RFC 6888 REQ-4; Richter: ≥ 512 ports/subscriber) is ours. |
| R8 both random, or nothing above the floor: no direct path | formalizes | HyperDHT `_punch` (no branch for random×random); Tailscale docs (hard+hard → relayed); NATCracker §5.8.3 (birthday ignored); Trautwein §7.1 (0.01%) |
| Hand-off to multi-reflector relaying (#1024) | formalizes | RFC 5128 §3.1 relaying; RFC 8656; circuit relay v2 |

### The protocol around the table

| element | class | source |
|---|---|---|
| The initiator controls and nominates; the responder follows | implements | RFC 8445 §6.1.1 (controlling agent), §8.1.1 regular nomination. Done in-band on the checked pair, like USE-CANDIDATE. |
| Epoch 1 computed identically by both ends from exchanged profiles | **ours** | No source; ICE's controlling agent alone decides nomination |
| Re-plan in the step a fact arrives; heard-not-acked and same-IP-other-port as reclassification | **ours** | ICE triggered checks (RFC 8445 §7.3.1.4) react to an inbound check, the nearest practice; using partials to RECLASSIFY the NAT has no source |
| The two-sided stop (a technique fails on both verdicts) | **ours** | Generalizes leviculum's two-sided commit (#89); ICE marks pairs failed per agent (RFC 8445 §6.1.2.6) |
| Stop conditions scaled by RTT | formalizes | DCUtR RTT/2 synchronisation; RFC 8445 §14 (Ta, RTO); RFC 8489 §6.2.1 |
| Adaptive keepalive from the measured binding lifetime | formalizes | RFC 5780 §4.6; RFC 4787 §4.3 REQ-5; RFC 7675 §5.1; RFC 6263 §4; Tailscale lifetime probe; Syncthing 180 → 20 s |
| History and learning (skip what failed, start where it worked) | **ours** | DCUtR retries up to 3 times without learning; edge holds 1 h. No surveyed system keeps per-peer technique history. |
| The HMAC wire and HELLO negotiation | implements | rns-rs docs/direct-link-protocol.md and commits (section 1); keepalive freshness as RFC 7675 consent |
| The trust gate on escalation; budgets | **ours**, with partial precedent | Precedent: RFC 8445 §19.5.1; RFC 5128 §6; HyperDHT global random-punch limit; rns-rs abuse controls (SURVEY.md §2). The gate's derivation from trust in the peer is ours (leviculum review). |

### Properties of Strategy.tla

| property | class | source |
|---|---|---|
| Total (OneTechnique, PlanIsCheapest) | formalizes | NATCracker §5.8.3: "for each combination we know which technique to use". The cheapest-sufficient order formalizes RFC 8445 §5.1.2's type preference; that the table and the physics agree is checked here. |
| NoSpam (NoSpamStep, EscPerAttempt, AttemptCost, CapsHeld) | formalizes | RFC 8085 §3.1 (applications SHOULD limit traffic); RFC 8445 §14 pacing; HyperDHT limits; NATCracker §5.10 (avoid looking like a scan). The formal statement is ours. |
| UsesEvidence (UsesEvidenceStep, PlanFollowsEvidence) | **ours** | Eric's direction; no source states it as a requirement |
| NoNegligibleRow (the floor) | formalizes | HyperDHT dispatch; NATCracker §5.8.3; Trautwein §7.1; the squared model (SURVEY.md §4). "Reachability is not likelihood" as a model property is ours. |
| DirectWhenPossible, NoFalseAbort | **ours** | — |
| Agreement, SamePlan | implements | RFC 8445 §8 (one nominated pair per component) |
| NoHalt (Variant, NotStuck, Termination) | formalizes | RFC 8445 §6.1.2.6 (Computing Candidate Pair States) and §8 (Concluding ICE Processing); the variant proof is ours |
| NoRace | **ours** | — |
| Learning (NotRetriedSameWay, RetryIsEvidenceFunction, StartsWhereItWorked) | **ours** | DCUtR's 3 retries are the nearest practice |
| MonotoneEvidence | **ours** | — |
| NoReplay | implements | rns-rs RNSK keepalives (counter + HMAC); RFC 7675 §5 (consent only from an authenticated response) |
| NoRawToCapablePeer | implements | rns-rs HELLO capability check (`6d21c606`) |
| NoTokenToStranger, StrangerKnowledge, NoPathToStranger | formalizes | RFC 8445 §19 (attacks on connectivity checks); rns-rs nonces checked against the source (`01f7be6c`); the tokenless knock is ours |
| FacilitatorKnowledge, FacilitatorCarriesNothing | **ours** | CC 5.4.6 (CIRIS); RFC 8489 §16 is the nearest |
| EscalationNeedsTrust | **ours** | the leviculum review's trust gate |
| AbortLeavesLinkUnchanged, NoLoneMove | formalizes | leviculum two-sided commit (#89); make-before-break as in RFC 9000 §9 (migration keeps the old path until the new one is validated) |

### What nobody has (SURVEY.md §3b.7)

No surveyed system has a formal model of its traversal. NATCracker calls
for "some kind of formalism" (§5.1) and gives semi-formal theorems. Ours
is the first we found that model-checks the technique choice, the pair
agreement and the abuse properties together.
