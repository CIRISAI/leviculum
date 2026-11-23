<!--
GAP_REVIEW.md: the gap review of the direct-link punch against the RFCs and the literature,
2026-11-22. Author: the internal literature-review agent (nat-litreview), working for the
CIRIS team; imported by the strategy agent (nat-strategy). Unchanged except this header.
Its file:line references are to the heads it names (LTS = 2cf992a8a, main = v0.35.0+ciris.2).
-->

# NAT traversal literature review: leviculum direct links + CIRISEdge embedder

Read-only, from git objects. "LTS" = leviculum `2cf992a8a` (diag/punch-telemetry; head 5316b05 adds only punch-probe CLI/CI). "main" = leviculum origin/main v0.35.0+ciris.2. Edge = CIRISEdge origin/main.

## Headline

Nothing that ships has the escalation work. CIRISEdge main pins `v0.34.0+ciris.3` (Cargo.toml:980-981): no spray, prediction or birthday sockets. leviculum main has #100 and #101 but not the punch branch (2cf992a8a is not an ancestor of origin/main). No line has both.

## Yardstick

### 1. RFC 8445 ICE: partial
- Candidates:
  - Host: one IPv4 (default-route interface) plus one global IPv6, found by route lookup (LTS std/direct_link.rs:2122-2150). A multi-homed device offers only its default interface.
  - Server-reflexive: one, from one facilitator, 3 tries x 2 s (:247-248, :287-338).
  - Peer-reflexive: implemented, at most 4 (:380, :1268-1282).
  - Relayed: none as a UDP candidate; the RNS link is the fallback.
  - Predicted ports: extra, non-ICE candidates (core session.rs:127-143).
- Pairing and priority: absent. All targets are punched every 100 ms (std :349, :1167-1177). The path is the first address both heard and acked (:1313).
- Checks: symmetric, proven both ways on one 5-tuple (:1285-1296, :1313). Triggered checks: yes, an immediate ack and the source is added (:1263-1282).
- Role conflict: the link's initiator wins (node/direct_link.rs:1020-1027).
- Nomination: absent. The two-sided commit (session.rs:554-590) swaps verdicts, not the pair (Gap 2).
- Check list, Ta pacing, pair state machine: absent (flat vectors, std :1139-1144).

### 2. RFC 8838 Trickle: absent
The phases run strictly in sequence (session.rs:404-458). Low cost, since gathering is a single probe.

### 3. Consent and keepalive: partial
- Commit peers: an acked probe every 5 s, dead after 15 s (direct_udp.rs:51-52), so detected at 15-20 s. Faster than RFC 7675's 30 s.
- Others (rns-rs): 30 s keepalive, dead at 120 s (:44-47). 30 s is right at common 30 s mapping timeouts; RFC 8445's Tr is 15 s.
- Not authenticated: any datagram from the peer's address refreshes liveness (:250), and token frames are replayable.
- Fallback paths:
  - link-stale (node/direct_link.rs:1991-2004);
  - the peer arriving over the relay after a 10 s grace (:63, :1918-1946);
  - a nudge;
  - edge's 2 s no-progress cutoff on A/V legs (edge direct_path.rs:200; its "120 s" comment is stale for commit peers).

### 4. RFC 5780 / RFC 4787 behaviour discovery: partial
- LTS: EIM vs EDM from the session socket toward the facilitator and a second observer (std :1447-1465, :2434-2445). The second observer is usually the facilitator's port+1, so it is blind to address-dependent mapping.
- main: needs two distinct IPs (main core nat.rs:173-218) and records port preservation and shared server IPs (:134-140). Telemetry only: it feeds no punch decision (main std direct_link.rs:985-1020).
- Absent: filtering classification (CHANGE-REQUEST / OTHER-ADDRESS), hairpin test, parity, contiguity, IP pooling detection, binding-lifetime discovery.

### 5. RFC 5128: partial
- UDP simultaneous punch: yes.
- Hairpin: implicit, because the public address is always target 0 (std :2106-2114).
- IPv4 LAN candidates: only when both sides report the same public IP (session.rs:106-125).
- TCP simultaneous open: absent.

### 6. Port prediction: partial
- 3 samples: the session socket plus two fresh sockets probed together inside 1 s (std :1381-1424).
- Step: the smaller of two equal-sign steps of at most 64. Predicts the next 16 ports of 1024 or above (:1365, :1547-1567).
- Predicted ports get knocks only.
- Not handled: staleness across the relay round (REQUEST/ACCEPT/READY), contiguity, port-block allocators.

### 7. Birthday: implemented for one hard side
- Hard side: up to 128 sockets (32 on own evidence), each punching the peer's advertised address every 500 ms (std :890-899, :1061-1100). Process-wide semaphore of 128 (:799-801).
- Easy side: 16-byte knocks to random ports 1024-65535 on the hard side's IP. 22 per 100 ms, at most 960, stop on first heard, start after 1.5 s of silence (:880, :885, :1210-1245, :1356).
- Odds per attempt:
  - 1-exp(-128*960/64512) ≈ 85% (wire.rs:59-66);
  - fallback arm: 1-exp(-32*960/64512) ≈ 38%;
  - Tailscale's 256x2048: 99.9%.
- Roles match Tailscale's: the hard side holds the sockets, the easy side sprays.

### 8. DCUtR: partial
- Synchronised start: the responder waits RTT/2, at most 1.5 s (node/direct_link.rs:57, :1303-1307). Comparable to DCUtR.
- Retries: DCUtR retries twice at once. Ours backs off 60/120/300/600/900 s (:52), and edge waits 1 h after PunchFailed (edge direct_path.rs:190).
- IMC'26 (arXiv 2604.12484, a revision of 2510.27500): about 70% conditional success, 97.6% of successes on the first attempt.

### 9. Port mapping (main #101): implemented, unused by direct links
- PCP, then NAT-PMP, then UPnP-IGD. Renewal at half-life with a 30 s floor (main port_mapping/engine.rs:164-169), delete on stop, reclaim of stale UPnP entries, CGNAT reported.
- Maps only the listeners and the facilitator port (main driver/port_mapping.rs:163-171). The session socket is never mapped, and the mapped address is never a candidate.
- Double NAT: detected only; no upstream PCP.

### 10. IPv6: partial
- Global v6 candidates always sent, dual-stack socket (std :2082-2104, session.rs:118-121). Stateful-firewall punching works.
- No preference order (the paths race).
- No NAT64/PREF64 awareness beyond an MTU note (wire.rs:833-838).

### 11. Same network: partial
- LAN candidates: only on the same public IP.
- CGNAT: two subscribers behind one carrier IP count as "same NAT". Each sends tokened punches to the other's private address on its own LAN (session.rs:106-125).
- No 100.64/10 candidates.
- No mDNS-style hiding: the private IPv4 is always sent to the peer (session.rs:423, :442).

### 12. Path quality: partial
- MTU: a 2-step probe (wire.rs:838, std :1968-2060). LTS sends it without DF; main fixed this in e7ff30853 (main std :1241-1248). Without DF, Linux can fragment locally and report a false 1400.
- No roaming: the interface accepts only the punched source (direct_udp.rs:220).
- One direct path plus the relay; no other warm paths.

### 13. Security: partial (stronger than ICE on budgets)
- Facilitator amplification:
  - RNSP: 21 bytes in, 28 or 40 out (1.3x / 1.9x);
  - STUN: 20 in, 32 or 44 out (1.6x / 2.2x) (wire.rs:546-585, :644-672);
  - 10 replies per second per source (std :2192).
- main's peer probes are padded so they do not amplify (main nat.rs:232-241).
- Spray abuse: a consented peer can advertise a victim's IP with "b". Bounded by 3 escalations per 5 min per pair, per-target volume (IPv4 address or v6 /64), and 1500 pps for the process (std :556-586). No check that the advertised IP is really the peer's.
- Authentication: the token travels in clear and can be replayed. Only the design note covers it (frame-auth-note.md).

### 14. Evidence
- Live (punch-probe on GitHub-hosted runners):
  - easy pairs: 100%;
  - random vs easy: 7 of 9;
  - fallback arm: 3 of 3, which is about 5% likely at a 38% prior. A plausible reason is that cloud egress NATs often allocate from a narrow port block. Not verified for these runners; if true, the runners overstate the odds for a full-range carrier CGNAT.
- Unit-tested: classify, predict, budgets. main also has a NAT-type test in network namespaces (natlab-nat-type.yml).
- TLA+: safety only, at NP=4, K=1, S=2. The RESULTS section is unfilled (DirectLinkPunch.md:159). Dual-stack is not modelled (G6).
- Not measured: both-hard, ADM, IP pooling, carrier CGNAT, IPv6, roaming.

## Both sides hard

Success is about zero by construction, apart from the IPv6, same-NAT and sequential-prediction escapes.
- A birthday socket aims at the peer's advertised port, its mapping toward the facilitator (std :1061-1080). An EDM peer NAT never routes a new source there.
- An EDM side's knocks leave from fresh random ports, and receivers discard knocks (:1253-1256; birthday tasks skip non-token frames).
- Literature: 2048x256 both-EDM ≈ 0.01%. The paper estimates about 1.2% of pairs are both-EDM.
- The relay is the answer, and it already exists.

## ADM and arbitrary IP pooling

No, the second observer does not detect either. The port+1 observer shares the facilitator's IP, so an address-dependent mapping is classified EIM: no "b", no "q" (std :1490-1499).
- Against an address-dependent or open peer filter it can still work: the new port is heard as peer-reflexive.
- Against an address-and-port-dependent filter (Linux conntrack, most home routers) it fails silently.
- Arbitrary pooling puts the mapping on another IP. The spray targets only targets[0].ip() (:1216), and prediction uses peer_public.ip() (session.rs:134).
- main's distinct-IP classifier would flag both, but it is telemetry only.
- Edge sets no second observer (direct_path.rs:451-465), so every hard NAT in production would take the 32-socket fallback (≈38%).

## Gaps, ranked

1. Merge the punch branch onto main and adopt it in edge. Benefit: any escalation at all in production. Medium.
2. Pair agreement or nomination (suspected; needs a dual-stack or hairpin witness). Each side settles on its first heard∧acked address (std :1313). COMPLETE carries no pair, and the interface drops other sources (direct_udp.rs:220). A can settle on v6 while B settles on v4, or LAN against hairpin, giving a path that is one-way dead (the v43 class). Fix: the controlling side nominates, or the interface accepts token-proven sources. Small to medium.
3. Map the session socket with PCP/NAT-PMP/UPnP and advertise the mapped address as a candidate. Benefit: home hard NATs become easy, with no spray. Medium.
4. Drive the punch from main's distinct-IP verdict and configure a second observer in edge. Benefit: catches ADM and pooling, and takes production from 38% to 85%. Small to medium.
5. Quick retry after a birthday miss (the budget allows 3 per 5 min) instead of edge's 1 h hold. Benefit: 85% to ≈98%. Small.
6. Authenticated (HMAC) frames, and liveness refreshed only by authenticated frames. Benefit: no replay or spoofed keep-alive. Medium, a negotiated wire change.
7. Roaming: accept a token-proven new source. Benefit: survives NAT rebinding and wifi/cellular switches, which today means a 15 s loss and possibly an hour's hold. Medium.
8. Learn the port range from allocation samples and spray a port block. Benefit: much higher odds on PBA CGNATs. Small to medium.
9. Candidate priority (v6 and host first) and host candidates from every interface. Small.
10. Cherry-pick DF for the MTU probe (e7ff30853). Benefit: no false 1400 result. Small.
11. CGNAT false same-NAT: send knocks only to host candidates until a token is proven. Benefit: no tokens leaked onto your own LAN. Small.
12. Plain keepalive at 15 s or less. Small.
13. Lower value: 256x2048 counts, Trickle, RFC 5780 filtering discovery, TCP simultaneous open.

## What we do that these sources don't (as far as they go)
- Token-free, content-free knocks, kept separate from the authenticated checks (wire.rs:813-829).
- Escalation gated by trust and consent, not by the answer policy (node/direct_link.rs:470-485, :1291-1296).
- Process-wide budgets per pair, per target (IPv4 or IPv6 /64) and global, with fair shares, plus a socket semaphore (std :556-801).
- A two-sided commit with explicit verdicts (REJECT_PUNCH_FAILED, REJECT_HOLDING) and make-before-break fallback (session.rs:554-590, wire.rs:80-93).
- A responder never probes a facilitator the requester names.
- Own-evidence fallback classification with fewer sockets (std :1501-1520).
- Peers as NAT probe servers with padded, non-amplifying requests (main nat.rs:232).
- Logs that carry no addresses.
- A TLA+ model with planted bugs. It found the birthday-switch race (std :1004).

## Is it the most advanced?

No. For a random hard NAT against an easy one it matches Tailscale's birthday approach with half the sockets. In abuse budgets, trust gating and formal modelling it goes further than ICE, DCUtR or Tailscale's write-up. It lacks:
- ICE's pair machinery and nomination (a likely real bug, Gap 2);
- the mapped address as a candidate;
- ADM and pooling detection that drives the punch;
- roaming;
- authenticated checks and consent.

The live evidence is 12 runs on cloud runners, and none of it ships yet. A careful, safety-first implementation of the easy-vs-hard case, but not the most complete one.
