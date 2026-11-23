<!--
SURVEY.md: the NAT traversal implementation survey, 2026-11-22.
Author: the internal survey agent (nat-survey), working for the CIRIS team; imported by the
strategy agent (nat-strategy). Unchanged from the agent's report except this header and one
line in section 7: a local scratch path (it named a user's directory) is replaced by
"(a session scratchpad, not durable)". Provenance tags ([code], [spec], [V], [V-meta], [NV])
are the survey's own. REFERENCES.md is built from its section 6.
-->

# NAT traversal survey: source code, specs, literature (2026-11-22)

Provenance tags: **[code]** read in source; **[spec]** read in RFC / I-D / paper; **[doc]** documentation or blog only; **[V]** verified from the primary document this session; **[V-meta]** title/status/date verified, body not opened; **[NV]** not verified.

Shallow clones read on 2026-11-22. Links pin these commits:
tailscale `87b3a5ff26`, iroh `40833b58fb`, noq (n0 quinn fork) `b36dc4de8e`, go-libp2p `8582b880e8`, rust-libp2p `78f6c206c2`, libp2p/specs `af185b5`, hyperdht `ce23dfd62d`, pion/ice `fb6b5670f7`, libnice `340219ebae`, ZeroTierOne `899352e384`, nebula `7876efaaf4`, netbird `9ba41ecd61`, syncthing `0f77b91539`, i2pd `04a20a0d05`, libtorrent `bd105a2620`, innernet `fac1a657b3`, tinc `211e3dfaef`, n2n `a6f94564f2`, yggdrasil-go `2c7f050fc5`, quic-go `098776f`, Reticulum (Python) `f55c0c6`, rns-rs (lelloman) `f672d64b0f`.

---

## 1. Comparison table

Y yes, P partial, N no, ? unknown. The "Ours" row restates the brief; it was not verified against code. Source links are in §2.

| System | Reflexive | Classify (map/filter) | Sim punch | Port prediction | Birthday / multi-socket | UPnP/PMP/PCP | IPv6 | TCP punch | Same-NAT / LAN | Sync start | Keepalive / consent | Relay fallback | Abuse controls | Formal |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Ours (brief) | Y 21 B probe / STUN | map Y (2 observers) / filter ? | Y two-sided commit | Y sequential | Y 128 sockets × 960 knocks | Y (newer line) | ? | N | Y host + prflx | Y | ? | N (policy) | Y rate, trust gate | ? |
| Tailscale | Y STUN [code] | map Y / filter N [code] | Y [code] | P local-port guess [code] | **N** [code+doc] | Y/Y/Y [code] | Y [code] | N | Y local endpoints [code] | Y CallMeMaybe [code] | Y heartbeat 3 s; lifetime probe gated [code] | Y DERP + peer relays [code] | P [code] | test harness (natlab) |
| iroh | Y QAD [code] | map Y / filter N [code] | Y QUIC frames [code] | P same guess [code] | N [code] | Y/Y/Y [code] | Y [code] | N | Y [code] | Y rounds [code] | Y 5 s / 15 s [code] | Y iroh-relay [code] | Y caps [code] | test harness (patchbay) |
| go-libp2p DCUtR | Y identify + AutoNAT v2 [code] | map Y (informational) / N [code] | Y [code] | N | N | P UPnP+PMP [code] | Y | Y TCP sim-open [code] | ? | Y RTT/2 [code] | ? | P circuit v2, 2 min / 128 KiB [code] | Y AutoNAT [code] | N |
| rust-libp2p DCUtR | Y | N [code] | Y [code] | N | N | P UPnP only [code] | Y | Y | ? | Y RTT/2 [code] | ? | P circuit v2 | P | N |
| HyperDHT | Y DHT samples [code] | map Y / N [code] | Y [code] | N | **Y 256 sockets / 1,750 probes** [code] | N | P/? (host cands IPv4 only) | N | Y LAN prefix match [code] | Y DHT rounds [code] | ? | Y blind-relay [code] | Y global random-punch limit [code] | N |
| pion/ice | Y | N | Y ICE | N | N | N | Y | Y active/passive/so [code] | Y + mDNS [code] | trickle [code] | Y RFC 7675 [code] | Y TURN | Y ICE pacing | N |
| libnice | Y | N | Y ICE | N | N | P UPnP (gupnp-igd) [code] | Y | Y incl. so [code] | Y | Y trickle [code] | Y RFC 7675 [code] | Y TURN | Y Ta, 100-check cap [code] | N |
| libwebrtc | Y | N | Y ICE | N | N | N | Y | Y | Y + mDNS [code] | Y | P [code] | Y TURN | Y | N |
| ZeroTier (now) | Y peers report [code] | P symmetric detect, historical only | Y RENDEZVOUS [code] | **removed (was in 1.2.x)** [code] | P secondary random port [code] | Y/Y/N [code] | Y | N | Y [code] | Y [code] | Y 14 s [code] | Y roots + TCP fallback | Y caps [code] | N |
| Nebula | Y lighthouse | N | Y [code] | N | N | N | Y | N | P calculated_remotes [code] | Y + delay [code] | Y punchy [code] | Y relays [code] | ? | N |
| NetBird | Y (pion fork) | N | Y ICE | N | N | Y via go-nat (PCP ?) [code] | Y | ? | Y | Y | Y | Y | ? | N |
| innernet | P server-reported | N | P WireGuard candidate cycling [code] | N | N | N | Y | N | Y ≤10 candidates [code] | N | WireGuard | N | ? | N |
| tinc | Y via meta | N | Y UDP discovery [code] | N | N | P UPnP [code] | Y | N | ? | ? | Y [code] | Y meta / TCP | ? | N |
| n2n | Y supernode | N | Y | N | N | P UPnP/PMP [code] | ? | N | ? | ? | ? | Y supernode | ? | N |
| Yggdrasil | N | N | N | N | N | N | Y | N | multicast LAN | N | N | N | n/a | N |
| Syncthing | Y STUN [code] | Y RFC 3489-style [code] | P implicit | N | N | Y/Y/N [code] | Y | N | Y | N | Y adaptive 20–180 s [code] | Y relays | ? | N |
| i2pd SSU2 | Y peer test [code] | Y incl. symmetric [code] | Y relay + HolePunch [code] | N | N | ? | Y | N (NTCP2) | ? | Y via introducer [code] | Y | P (introducers signal only) | Y nonce, resend caps [code] | N |
| libtorrent | Y peer-reported | N | Y BEP 55 over uTP [code] | N | N | Y/Y/Y [code] | Y | uTP is UDP | ? | Y rendezvous peer [code] | ? | N | ? | N |
| QUIC NAT draft-04 | out of scope | N | Y [spec] | N | N | N | Y | N | n/a | Y PUNCH_REQUEST [spec] | RFC 9000 | proxied path assumed [spec] | grants; §6.4 "TODO" [spec] | N |
| rns-rs DirectLink | Y RNSP/STUN [code] | N | Y [code] | N | N | N | Y [code] | N | Y host ≤2, prflx ≤2 [code] | Y [code] | Y 15 s / 45 s [code] | N (transport path stays) | Y extensive [code] | fuzz target only |

---

## 2. Mechanisms beyond plain simultaneous punching, with parameters

### Tailscale [code]
- **No birthday or multi-socket code**: zero hits for `birthday` across the Go tree. Docs table says hard + easy → "Relayed" ([device-connectivity](https://tailscale.com/docs/reference/device-connectivity)) [doc].
- Mapping classification: two STUN replies differ → `MappingVariesByDestIP=true` ([netcheck.go#L717-L731](https://github.com/tailscale/tailscale/blob/87b3a5ff26/net/netcheck/netcheck.go#L717-L731)). No IPv6 mapping check (code TODO), no filtering test. Hairpin checks removed ("We no longer send hairpin checks", netcheck.go#L334).
- **Local-port guess** (`EndpointSTUN4LocalPort`, tailcfg.go#L1421 "hard NAT: STUN'ed IPv4 address + local fixed port"): if mapping varies by destination **and** tailscaled runs on a fixed local port, it advertises STUN IP + that local port, assuming the user added a static forward. Comment: "Worst case it's an invalid candidate mapping." ([magicsock.go#L1364-L1373](https://github.com/tailscale/tailscale/blob/87b3a5ff26/wgengine/magicsock/magicsock.go#L1364-L1373)).
- Timers ([magicsock.go#L4288-L4317](https://github.com/tailscale/tailscale/blob/87b3a5ff26/wgengine/magicsock/magicsock.go#L4288)): heartbeat 3 s; sessionActiveTimeout 45 s; trustUDPAddrDuration 6.5 s; endpointsFreshEnough 27 s; upgrade direct/relay every 1 min; disco ping min interval 5 s, timeout 5 s (tsconst/ping.go).
- NAT-lifetime probing `probeUDPLifetime`: pings at preset timeout "cliffs", 2 s slack, gated by `NodeAttrProbeUDPLifetime` ([endpoint.go#L160-L200](https://github.com/tailscale/tailscale/blob/87b3a5ff26/wgengine/magicsock/endpoint.go#L160)).
- Disco messages: Ping, Pong, CallMeMaybe, CallMeMaybeVia, BindUDPRelayEndpoint(+Challenge/Answer), AllocateUDPRelayEndpoint Request/Response (disco/disco.go#L45-L53).
- Port mapping `net/portmapper/{upnp,pcp,legacy_upnp}.go`; PCP opcodes ANNOUNCE and MAP only (pcp.go#L41-L43).
- Relay: DERP, plus Tailscale Peer Relays (`net/udprelay`).

### iroh [code]
- NAT traversal inside QUIC, n0 variant: ADD_ADDRESS v4/v6, REACH_OUT v4/v6, REMOVE_ADDRESS, frame types `0x3d7f90`–`0x3d7f94` ([noq frame.rs#L127-L137](https://github.com/n0-computer/noq/blob/b36dc4de8e/noq-proto/src/frame.rs#L127)). Specified as draft-bruynooghe-n0-quic-nat-traversal-00; not the draft-seemann-03/04 frames.
- Probing: `MAX_NAT_PROBE_ATTEMPTS = 9` per address per round; backoff base initial_rtt/10 (~33 ms), exponential, capped 2 s; ~4 s total ([n0_nat_traversal.rs#L19-L32, #L330-L344](https://github.com/n0-computer/noq/blob/b36dc4de8e/noq-proto/src/n0_nat_traversal.rs#L19)). (The draft says probes are abandoned at a new round or "a fixed timeout of 30s" — code and draft differ.)
- Limits/timers: 32 remote candidates (`MAX_QNT_ADDRESSES`), 8 multipath paths, heartbeat 5 s, path idle 15 s ([socket.rs#L109-L145](https://github.com/n0-computer/iroh/blob/40833b58fb/iroh/src/socket.rs#L109)); re-holepunch every 5 s (`remote_state.rs#L52`).
- **Local-port guess**, copied from Tailscale including the comment: if `mapping_varies_by_dest()` and the socket is bound to a non-zero port, advertise QAD IPv4 + that port ([socket.rs#L1853-L1872](https://github.com/n0-computer/iroh/blob/40833b58fb/iroh/src/socket.rs#L1853); type at socket.rs#L2108 "Hard NAT: QAD'ed IPv4 address + local fixed port").
- Reflexive address via QUIC Address Discovery to relays (net_report.rs).
- Port mapping crate `portmapper` 0.20.2: UPnP, NAT-PMP, PCP; PCP `Opcode {Announce=0, Map=1}` only.

### go-libp2p [code]
- DCUtR: `maxRetries = 3`; SYNC at rtt/2 ([holepuncher.go#L23, #L150](https://github.com/libp2p/go-libp2p/blob/8582b880e8/p2p/protocol/holepunch/holepuncher.go#L23)); TCP via `WithSimultaneousConnect` (util.go#L51-L60).
- QUIC punch: 64 random bytes, sleep 10 ms + rand(min(10·(i+1)², 200)) ms, up to `HolePunchTimeout` 5 s ([quic/transport.go#L230-L250](https://github.com/libp2p/go-libp2p/blob/8582b880e8/p2p/transport/quic/transport.go#L230)).
- NAT type: `observedaddrs` classifies EIM vs EDM for TCP and UDP once ≥3×max observations ([manager.go#L518-L575](https://github.com/libp2p/go-libp2p/blob/8582b880e8/p2p/host/observedaddrs/manager.go#L518)); emitted as an event, no consumer in holepunch.
- AutoNAT v2 amplification guard: dial-back to a non-observed IP requires 30–100 kB client data (autonat.go#L32-L33), 3 s wait (options.go#L27), per-peer RPM limit, 1 concurrent request per peer.
- Port mapping: UPnP + NAT-PMP (`p2p/net/nat/internal/{upnp,natpmp}.go`); no PCP.
- Circuit relay v2 default limit: 2 min, 128 KiB (relay/resources.go#L64-L66).

### rust-libp2p [code]
- DCUtR `MAX_NUMBER_OF_UPGRADE_ATTEMPTS = 3` (behaviour.rs#L46); dial waits rtt/2 (outbound.rs#L93).
- QUIC punch: 64 random bytes every rand(10..=200) ms ([hole_punching.rs](https://github.com/libp2p/rust-libp2p/blob/78f6c206c2/transports/quic/src/hole_punching.rs)).
- `protocols/upnp` (IGD only).

### HyperDHT — the only shipping birthday implementation [code]
File: [lib/holepuncher.js](https://github.com/holepunchto/hyperdht/blob/ce23dfd62d/lib/holepuncher.js).
- **Classification** ([nat.js#L98-L160](https://github.com/holepunchto/hyperdht/blob/ce23dfd62d/lib/nat.js#L98)): samples its own address as reported by DHT nodes. One ip:port seen ≥3 times → CONSISTENT; max hits 1 → RANDOM; 2-hit rules; ≥4 samples with no decision → RANDOM. Unfirewalled → OPEN (coerced to CONSISTENT for punching).
- **Dispatch** (`_punch`, L165-L212): CONSISTENT×CONSISTENT → `_consistentProbe`; local CONSISTENT × remote RANDOM → `_randomProbes`; local RANDOM × remote CONSISTENT → `_openBirthdaySockets` then `_keepAliveRandomNat`; anything else → return false. Random modes require a *verified* remote address.
- **Hard (RANDOM) side opens the sockets**: `BIRTHDAY_SOCKETS = 256` (L6). `_openBirthdaySockets` (L272-L279) acquires sockets from the pool until 256 are held, sending one 1-byte packet (0x00) from each to the easy side's verified address with `HOLEPUNCH_TTL = 5` (L8) — low TTL, to create its own mapping without (likely) reaching the far NAT.
- Then `_keepAliveRandomNat` (L247-L270): pause 100 ms, then `tries = 1750` (~35 s), one send every 20 ms, round-robin across the 256 sockets (each socket re-sent every ~5.1 s); the first full cycle still at low TTL (`lowTTLRounds = 1`), then TTL 64.
- **Easy (CONSISTENT) side**: `_randomProbes` (L234-L243): `tries = 1750`, one 1-byte probe every 20 ms (50 pps, ~35 s) from its single socket to `randomPort()` = `(1000 + Math.random()*64536) | 0` at the hard side's verified IP. Probes are token-free.
- **Stop conditions**: `this.punching` goes false on connect or destroy; tries exhausted → `_autoDestroy()` (destroys if not connected); destroy releases all held sockets.
- **Both RANDOM**: `_unstable()` (L95-L101) is true when both are ≥RANDOM (or local UNKNOWN) → `analyze()` attempts a socket reopen/resample (`MAX_REOPENS = 3`) or returns false; `_punch` has no branch for random×random → no punch; relay if configured.
- **Consistent probe**: 10 rounds at 1 s; unverified addresses only every 4th tick; responder sleeps 1 s first.
- **Global limit** (index.js#L22, #L54-L56; connect.js#L681-L690; server.js#L608): `_randomPunchLimit = 1` ("set to one for extra safety for now") concurrent random punch per DHT node; `randomPunchInterval` default 20,000 ms between random punches; if over the limit and no relay token → `HOLEPUNCH_ABORTED`; otherwise delays with a round update.
- Relay: `relayThrough` via `blind-relay` (server.js#L381-L430). Same-NAT: `matchAddress` octet-prefix match; `localAddresses` skips non-IPv4.

### pion/ice [code]
[agent_options.go#L22-L56](https://github.com/pion/ice/blob/fb6b5670f7/agent_options.go#L22): check interval 200 ms; keepalive 2 s; disconnected 5 s; failed 25 s; consent freshness 30 s; nomination waits srflx 500 ms, prflx 1 s, relay 2 s; max 7 binding requests per pair. TCP types active/passive/so; mDNS; renomination; address rewrite rules; SPED (draft-hancke-webrtc-sped).

### libnice [code]
[agent-priv.h#L112-L118](https://gitlab.freedesktop.org/libnice/libnice/-/blob/340219ebae/agent/agent-priv.h#L112): Ta 20 ms; Tr 25 s; consent 5 s / 30 s (RFC 7675); min consent interval 4 s; max 100 checks. Properties: UPnP, ICE-TCP (incl. S-O), keepalive-conncheck, nomination mode, trickle, renomination, idle timeout, consent freshness. No mDNS found.

### libwebrtc [code]
[ice_transport_internal.h](https://webrtc.googlesource.com/src/+/refs/heads/main/p2p/base/ice_transport_internal.h): `NominationMode {REGULAR, AGGRESSIVE, SEMI_AGGRESSIVE}`, default SEMI_AGGRESSIVE; renomination; STUN keepalive interval configurable. mDNS obfuscation (basic_port_allocator.cc#L546). No explicit RFC 7675 timer found in connection.cc → P.

### ZeroTier [code]
- Historical prediction `getSymmetricNatPredictions()` present in 1.2.12 ([SelfAwareness.cpp@1.2.12#L127](https://github.com/zerotier/ZeroTierOne/blob/1.2.12/node/SelfAwareness.cpp#L127)), absent in 1.4.0, 1.4.6, 1.6.6, 1.8.10, 1.10.6 and HEAD. Method: cites draft-takeda; only trusted (upstream) reporters count (security note on cache poisoning); symmetric if trusted reports disagree; predicts max-seen-port+1 per external IP, plus one random port (1024 + prng % 64511).
- Today: secondary random port; tertiary port for miniupnpc/libnatpmp ([OneService.cpp#L1353-L1380](https://github.com/zerotier/ZeroTierOne/blob/899352e384/service/OneService.cpp#L1353)); RENDEZVOUS from relays at most every 30 s (`ZT_MIN_UNITE_INTERVAL`), randomised hint order "for black magickal NAT-t reasons" (Peer.cpp#L384); PUSH_DIRECT_PATHS ≤8 per scope/family, ≤8 responses per 30 s (Constants.hpp#L574-L609); path heartbeat 14 s (Constants.hpp#L374).

### Nebula [code]
[punchy.go](https://github.com/slackhq/nebula/blob/7876efaaf4/punchy.go): 1-byte punch packets; `punchy.punch` periodic to keep mappings; `punchy.delay` default 1 s; `respond` (punch_back, connection reversal) default off, `respond_delay` 5 s; `target_all_remotes`; `calculated_remotes` subnet mapping (examples/config.yml#L118); relays (config.yml#L294).

### Syncthing [code]
RFC 3489-style NAT typing via ccding/go-stun; symmetric → not punchable, stops keepalives ([stun.go#L271](https://github.com/syncthing/syncthing/blob/0f77b91539/lib/stun/stun.go#L271)); adaptive STUN keepalive start 180 s, min 20 s (optionsconfiguration.go#L61-L62); UPnP (`lib/upnp`), NAT-PMP (`lib/pmp`); relays.

### i2pd SSU2 [code]
Peer test sets `eRouterErrorSymmetricNAT` (SSU2Session.cpp#L2124); full-cone detection has a "TODO … isn't working" comment (L2127). Peer test resend 3 s ± 2 s, max 3; HolePunch resend 1 s ± 0.5 s, max 3 (SSU2OutOfSession.h#L19-L59); ≤3 introducers (SSU2.h#L46).

### libtorrent [code]
BEP 55 `ut_holepunch`: rendezvous / connect / failed with error codes (bt_peer_connection.cpp#L1489-L1600); on connect adds the peer with `supports_utp = true`. NAT-PMP and PCP (natpmp.cpp#L111), UPnP (upnp.cpp).

### innernet [code]
Candidate cycling, ≤10 candidates, one per `STEP_INTERVAL` 5 s (client-core/src/nat.rs#L16).

### tinc / n2n / Yggdrasil [code, light]
tinc: `UDPDiscovery*` options (net_setup.c#L395-L398), UPnP via miniupnpc (src/upnp.c). n2n: supernode registration, UPnP/NAT-PMP (src/n2n_port_mapping.c). Yggdrasil: no NAT traversal code.

### QUIC NAT draft [spec]
[draft-seemann-quic-nat-traversal-04](https://datatracker.ietf.org/doc/html/draft-seemann-quic-nat-traversal-04): asymmetric (only the server advertises); client requests attempts with PUNCH_REQUEST against PUNCH_GRANT credits; §6.4 amplification "TODO". -00..-02 used ADD_ADDRESS / PUNCH_ME_NOW. No implementation of -03/-04 found (quic-go: none).

### rns-rs DirectLink [code+doc]
[docs/direct-link-protocol.md](https://github.com/lelloman/rns-rs/blob/f672d64b0f/docs/direct-link-protocol.md). Same wire design as ours (21-byte RNSP probe or STUN; 52-byte HMAC-tagged punches; ≤4 candidates; ≤2 host; ≤2 prflx). Punch 100 ms for 10 s; cap 128 punches + 128 acks; unanswered target: burst 16 then 1/s, ≤25 (~1.3 KB). Facilitator rate limit: per-source bucket (IPv6 /64) 10 burst, 1/s, 1000/s global, 4096 sources. No classification, prediction, birthday or port mapping; two hard NATs fail by design. Python Reticulum and `rete`: no NAT traversal.

---

## 3. Gaps

### 3a. Shipped elsewhere, absent from ours (ranked by likely success gain)
1. Per-socket refresh during the birthday window (HyperDHT: each of 256 sockets re-sent ~every 5 s). Richter: CGN UDP timeouts 10–200 s, median 35 s non-cellular — early hard-side mappings can expire mid-window.
2. Low-TTL priming of hard-side sockets (HyperDHT TTL 5).
3. Connection reversal when one side is reachable (RFC 5128 §3.2; Nebula punch_back; BEP 55; DCUtR unilateral upgrade).
4. NAT-lifetime measurement / adaptive keepalive (Tailscale cliffs; Syncthing 180→20 s).
5. Local-port static-forward guess (Tailscale, iroh): cheap, one extra candidate.
6. Filtering classification (Syncthing, i2pd peer test).
7. mDNS host candidates / RFC 7675 consent (ICE stacks) — privacy and liveness rather than success.
(Ours already has things no other surveyed shipping system combines: two-observer classification + sequential prediction + birthday + PCP + prflx + host candidates.)

### 3b. In the literature, shipped by nobody
1. PCP PEER opcode (RFC 6887 §12): learn the external port toward a specific peer. All PCP clients read implement ANNOUNCE + MAP only.
2. Chunk-aware birthday: target knocks at the CGN port block (Richter: chunks ≤1K–16K in 17 ASes; ≥512 ports/subscriber; RFC 7422/7768). Keller: "random" mobile allocators cluster at +1/+2 from the smallest seen port.
3. Traffic-rate-aware sequential prediction (Huang PSM/EVM 2019; Keller DEVM/DPSM 2022: 76–78%; 93% cone×progressing).
4. Hard–hard birthday (needs ~170k probes per side for 99.9%; impractical).
5. TCP traversal beyond plain simultaneous open (STUNT; NATBlaster low-TTL SYN + raw SYNACK).
6. ICMP rendezvous (pwnat) — history only.
7. Formal verification — none.

---

## 4. Birthday arithmetic and the "0.01%"

The figure is real, in both Trautwein papers §7.1 ("When both peers operate behind EDM NATs … 0.01% when probing 2,048 ports on one side and 256 on the other"), citing Tailscale's 2020 blog. It is the **both-sides-hard** case: each probe also gets a random source port, so the collision is on a (src, dst) pair; space 65536². P ≈ 1−exp(−N·M/65536²) = 0.0122% for 256×2048. The one-hard formula 1−exp(−N·M/65536) does not apply. The blog's 170k → 99.88% and 54k → 49.3% also fit the squared model.

| Case | Result |
|---|---|
| Tailscale one-hard: 256 × 174 / 256 / 1024 / 2048 | 49% / 63% / 98% / 99.97% (blog: 50/64/98/99.9) |
| Ours: 128 × 960 over 64,512 ports | 85.1% (85.4% without replacement) |
| Ours, both hard | 0.0029% |
| HyperDHT 256 × 1,750 over 64,536 | 99.9% |
| Keller 3,000 × 30 | 74.7% model vs 73–83% measured |
| Ours inside a 1K CGN chunk | ~100% |

Caveats: uniformity assumption fails for chunked or clustered allocators; on a CGNAT IP, random knocks reach other subscribers' mappings; CGN per-subscriber port caps (as low as 512) make 128 sockets a quarter of the quota.

Paper caution (2604.12484 only): many open ports raise fingerprinting exposure; probing can trigger IDS / denylisting; failed TCP sim-open looks like a SYN flood; can exhaust NAT session tables.

Corrections: their "CGNAT EDM in 40% of cases [Richter]" is the cellular bimodal figure (Richter: <2% CPE, 11% non-cellular CGN ASes symmetric). Their ~11% EDM prevalence is from Halkes 2011 (not verified).

---

## 5. Measured success rates

| Source | Population | Result | Caveats |
|---|---|---|---|
| Ford, Srisuresh, Kegel 2005 [spec] | 380 NAT Check reports, 68 vendors | UDP 82% (310/380), TCP 64% (184/286); hairpin UDP 24%, TCP 13% | self-selected; §6.2 counts via fetch summary |
| Guha & Francis 2005 STUNT [spec] | 16 lab + 93 home NATs | TCP avg 88%; 29.9% symmetric mapping; 13.6% reject sim-open | 2005 market weighting; per-method via summary |
| Halkes & Pouwelse 2011 | Tribler | abstract only: ~64% punchable, >80% success | NV |
| Richter et al. 2016 [spec] | Netalyzr + BitTorrent DHT | CGN in >17% eyeball, >90% cellular ASes; port alloc non-cell/cell: preservation 41.2/27.9%, sequential 22.2/26.0%, random 35.6/44.7%; UDP timeout 74% ≤1 min, range 10–200 s | per AS, most-permissive STUN type |
| Keller et al. 2022 [spec] | German mobile (Telekom, Vodafone, Telefónica) | progressing DEVM/DPSM 76–78%; cone×progressing 93%; random with 3,000 ports 73–83%; random×progressing 14–17.5% (6–10% baseline); random×random "insufficient" | small field test |
| Trautwein et al. 2025/26 [spec] | 4.4M attempts, 85k+ networks, 167 countries, 2022-12-01 → 2023-01-01 | 70% ± 7.1%; TCP ≈ QUIC; 97.6% of successes on attempt 1 | conditional on relay reservation + address discovery (fail ~29%); honeypot pre-filter; no NAT typing |
| Tailscale blog 2020 [doc] | — | ">90%" basic techniques | estimate |

---

## 6. RFC and Internet-Draft reference list

### 6a. RFCs (all [V]: rfc-editor .json + .txt opened; URL form `https://www.rfc-editor.org/rfc/rfcNNNN#section-X`)

- **RFC 4787** NAT Behavioral Requirements for Unicast UDP (BCP 2007; upd. 6888, 7857): §4.1 mapping; §4.2.1 port assignment; §4.2.3 contiguity; §4.3 refresh REQ-5 (≥2 min)/REQ-6; §5 filtering REQ-8; §6 hairpinning REQ-9; §8 REQ-11; §12 REQ-1–14.
- **RFC 7857** Updates to NAT Behavioral Requirements (BCP 2016): §3 port overlapping; §4 APP; §5–7 EIM/EIF; §8 parity; §9 port randomization (follow RFC 6056 §4).
- **RFC 5382** NAT Behavioral Requirements for TCP (BCP 2008): §4.1 REQ-1; §4.2 REQ-2 (all valid sequences incl. sim-open); §4.3 REQ-3.
- **RFC 5780** NAT Behavior Discovery Using STUN (Exp 2010): §3.1–3.4; §4.3 mapping test; §4.4 filtering test; §4.6 lifetime; §7.2 CHANGE-REQUEST; §7.4 OTHER-ADDRESS; §8.
- **RFC 5128** State of P2P across NATs (Info 2008): §2.2–2.6; §2.10; §3.1 relaying; §3.2 connection reversal; §3.3 (3.3.1–3.3.3) UDP hole punching; §3.4 TCP; §3.5 UDP port prediction; §3.6 TCP port prediction; §5.2; §5.4; §6.1–6.4.
- **RFC 6056** Port Randomization (BCP 2011): §3.3.1–3.3.5.
- **RFC 6888** CGN requirements (BCP 2013): §3 REQ-1, 2, 4, 7, 9; §5 REQ-13/14/15 (hard to guess).
- **RFC 6598** Shared Address Space (BCP 2012): §4; §7 100.64.0.0/10.
- **RFC 7422** deterministic CGN mapping (Info 2014); **RFC 7768** port blocks (Info 2016) §2; **RFC 7753** PCP port sets (PS 2016) §1.1. RFC 6269, 7021 [V-meta].
- **RFC 6886** NAT-PMP (Info 2013): §3.2; §3.3.
- **RFC 6887** PCP (PS 2013; upd. 7488, 7652, 7843): §8.1.1; §8.5 epoch; §11 MAP; §12 PEER; §13.1 THIRD_PARTY; §13.3 FILTER; §14.1 ANNOUNCE; §15; §18.
- **RFC 6970** UPnP IGD–PCP IWF (PS 2013): §4; §5.6; §5.9.
- RFC 7488, 7652, 7843 [V-meta].
- **RFC 8489** STUN (PS 2020; obsoletes 5389 ← 3489): §6.2.1 RTO ≥500 ms; §7; §9 (§9.2.1); §14.2 XOR-MAPPED-ADDRESS; §16.2.1 DDoS; §19.
- **RFC 8445** ICE (PS 2018; upd. 8863): §5.1.1.1; §5.1.1.2; §5.1.1.4; §6.1.2.5 (cap 100); §6.1.4; §7.2.5.3.1 / §7.3.1.3 prflx; §8.1.1 (aggressive nomination deprecated); §11 Tr 15 s; §14.2 Ta 50 ms; §19.2–19.4; §19.5.1 STUN amplification.
- **RFC 8838** Trickle ICE (PS 2021): §9–14; §16.
- **RFC 8863** ICE PAC (PS 2021): §4 PAC timer 39.5 s.
- **RFC 7675** Consent Freshness (PS 2015): §5.1 (5 s × 0.8–1.2; expiry 30 s).
- **RFC 6263** RTP keepalive (PS 2011): §4; §5; §7 (min Tr 15 s for UDP).
- **RFC 8085** UDP Usage Guidelines (BCP 2017): §3.5.
- **RFC 6544** ICE-TCP (PS 2012): §3; §4.2; §5.3; §11.
- **RFC 8656** TURN (PS 2020): §3.2; §3.3; §3.5; §7–10. RFC 6062, 8016 [V-meta]. RFC 8421 [V-meta].
- **RFC 8828** WebRTC IP handling (PS 2021): §5.2. RFC 6762 mDNS [V-meta].
- **RFC 8305** Happy Eyeballs v2 (PS 2017): §4; §5 (250 ms); §7.
- **RFC 8899** DPLPMTUD (PS 2020): §4.1; §4.3; §5.2; §5.3; §6.1; §6.3.
- **RFC 9000** QUIC (PS 2021): §8 (3× limit); §8.2.1–8.2.4; §9.1; §9.3.1–9.3.3; §9.6; §21.3; §21.5 (§21.5.4).
- **RFC 10069** Multiple Paths for a QUIC Connection (PS, Nov 2026; was draft-ietf-quic-multipath-21): §3.1 (§3.1.2); §3.3; §3.4.
- RFC 9298, 9443 [V-meta].
- **RFC 6092** IPv6 CPE simple security: §3.2.2 REC-11; §3.2.3 REC-14. RFC 6146, 7050, 6052, 6877 [V-meta].

### 6b. Internet-Drafts (all [V]: datatracker rev/state + archive text read, 2026-11-22)

- **draft-seemann-quic-nat-traversal-04** (active, 2026-11-02) — https://datatracker.ietf.org/doc/html/draft-seemann-quic-nat-traversal-04 — §4; §5.1–5.3; §6 (PUNCH_GRANT / PUNCH_REQUEST / PUNCH_DONE); §6.1; §6.4 "TODO"; §8; §9.
- **draft-bruynooghe-n0-quic-nat-traversal-00** (active, 2026-07-06) — iroh's protocol — §3.4; §3.5.1 (unpadded PATH_CHALLENGE, PTO backoff, 30 s); §4.1–4.3; §5 "TODO".
- **draft-kozuka-quic-stateless-punch-00** (active, 2026-11-15) — §3 one-sided punch (server one punch, client PATH_CHALLENGE after ~SRTT); §5.1 limits; §10; App. B (fails through EDM).
- **draft-ietf-quic-address-discovery-01** (QUIC WG doc, 2026-08-15; replaces draft-seemann-quic-address-discovery-04) — §4.1 OBSERVED_ADDRESS; §5; §6.1–6.2.
- **draft-ietf-masque-connect-udp-listen-16** (MASQUE, RFC Editor queue, 2026-10-12) — §2; §7 Proxy-Public-Address; §8.1; §9.
- **draft-seemann-masque-connect-udp-rendezvous-00** (active, 2026-08-16) — §3; §3.2; §4.
- **draft-munizaga-quic-alternative-server-address-02** (active, 2026-09-25) — §5; §5.1; §8.1; §8.2.
- **draft-hancke-webrtc-sped-01** (active, 2026-11-02) — §3.3.
- draft-ietf-mmusic-mdns-ice-candidates-03 (**expired** 2022) — §3.1; §3.2; §3.3.
- draft-takeda-symmetric-nat-traversal-00 (expired 2003) — §6.1; §6.3; §7.2; §8.1–8.4.
- Expired, [V-meta] only: draft-duke-quic-natsupp-03; draft-li-quic-nat-optimization-00. Not relevant: draft-ietf-lisp-nat-traversal-04.
- No current tsvwg / tram / ice draft on NAT behaviour, symmetric traversal or port prediction (name + title search; not full text).

### 6c. Non-IETF specs
- libp2p DCUtR r1 2021-11-20 (Recommendation) — https://github.com/libp2p/specs/blob/af185b5/relay/DCUtR.md — "The Protocol" steps 1–6 ("observed (and possibly predicted) addresses", undefined; RTT/2; TCP sim-connect; QUIC random packets 10–200 ms; 3 attempts). [V]
- libp2p AutoNAT v2 r2 2023-04-15 (Working Draft) — autonat/autonat-v2.md "Amplification Attack Prevention" (30k–100k bytes). [V]
- libp2p Circuit Relay v2 r3 2023-02-28 — reservation, connection initiation, vouchers. [V]
- I2P SSU2 (Status Completed, Prop159; page updated 2026-03) — https://i2p.net/en/docs/specs/ssu2/ — #peer-test-process, #peer-test-type-7, #relay-process, #relayrequest, #relayresponse, #relayintro, #hole-punch-type-11, #introducers, #peer-address-spoofing. [V]
- BEP 55 Holepunch (Accepted, 2019-02-28) https://www.bittorrent.org/beps/bep_0055.html; BEP 29 uTP. [V]
- rns-rs DirectLink doc [V]. UPnP IGD:2 (OCF) [NV, 404].

### 6d. Column → defining source
| Column | Normative / defining | Literature |
|---|---|---|
| Reflexive | RFC 8489 §14.2; RFC 8445 §5.1.1.2; quic-address-discovery-01 §4.1; connect-udp-listen-16 §7 | — |
| Classification | RFC 4787 §4.1, §5; RFC 5128 §2.2–2.6; RFC 5780 §4.3–4.4; Takeda §6.3, §7.2 | Guha 2005; Richter 2016 |
| Simultaneous punch | RFC 5128 §3.3; RFC 8445 §7; DCUtR; seemann-04 §6; bruynooghe-00 §3.5 | Ford 2005; Trautwein |
| Port prediction | RFC 5128 §3.5–3.6; Takeda §6.1, §8; RFC 4787 §4.2.1/4.2.3; against: RFC 7857 §9, RFC 6888 REQ-15, RFC 6056; blocks: RFC 7422/7768/7753 | Keller 2022; Huang/Wei/Wang [NV] |
| Birthday | none in any RFC/I-D | Tailscale blog [doc]; Keller 2022; Trautwein §7.1; NATBlaster [NV] |
| Port mapping | RFC 6886; RFC 6887 §11/§12/§14.1; RFC 6970; RFC 6888 REQ-9 | Trautwein |
| IPv6 | RFC 6092; RFC 8445/8421; RFC 8305; NAT64 RFCs | — |
| TCP punch | RFC 5128 §3.4/3.6; RFC 5382 REQ-2; RFC 6544; DCUtR step 5 | Ford; Guha |
| Hairpin / same-NAT / CGNAT | RFC 4787 §6; RFC 5128 §2.10, §3.3.2–3.3.3, §5.4; RFC 5780 §3.4; RFC 6598; RFC 6888; RFC 8445 §5.1.1.1; mDNS draft-03 | Ford |
| Sync start | DCUtR step 4; seemann-04 §6; kozuka-00 §3; RFC 8445 §6.1.4, §14.2; RFC 8838; RFC 8863 | Trautwein |
| Keepalive / consent | RFC 4787 §4.3; RFC 7675 §5.1; RFC 8445 §11; RFC 6263 §7; RFC 8085 §3.5; RFC 5780 §3.3/§4.6 | Richter |
| Peer-reflexive | RFC 8445 §7.2.5.3.1, §7.3.1.3 | — |
| Relay fallback | RFC 5128 §3.1; RFC 8656; RFC 9298; connect-udp-listen-16; rendezvous-00; circuit v2 | — |
| Connection reversal | RFC 5128 §3.2 | — |
| Low-TTL priming | none in RFC/I-D | Guha 2005 §2.2 |
| Abuse controls | RFC 8445 §19.5.1; RFC 8489 §16.2.1; RFC 9000 §8, §21.3, §21.5; RFC 5128 §6; AutoNAT v2; kozuka §5.1; seemann §6.4/§9; SSU2 #peer-address-spoofing | Trautwein 2026 §6 |
| Path validation | RFC 9000 §8.2, §9; RFC 10069 §3.1 | — |
| PMTU | RFC 8899 §4–6 | — |
| Formal model | none | NATCracker (ICCCN 2009) abstract only [NV] |

### 6e. Papers
[V]: Trautwein et al. arXiv 2510.27500 and 2604.12484 (IMC '26, doi 10.1145/3777912.3809138); Richter et al. IMC 2016 (icir.org/vern/papers/cgn-imc16.pdf); Keller, Hoßfeld, von Mammen ICCE 2022; Ford, Srisuresh, Kegel USENIX 2005 (arXiv cs/0603074); Guha & Francis IMC 2005; Takeda draft. [doc]: Tailscale "How NAT traversal works" (2020). Technique history: pwnat, chownat, NAT Slipstreaming (samy.pl), Natter (GitHub README/source, via fetch agent).

---

## 7. Not verified from primary sources
- Halkes & Pouwelse 2011 full text (Springer login wall, HAL bot wall).
- NATCracker full text (venue/DOI verified only).
- Wei et al. 2008; Wang, Lu, Gu 2006; Huang et al. 2019; Klinec & Matyáš 2014 (only via Keller).
- NATBlaster original paper; "439 probes for 95%" figure.
- Takeda §8 (fetch-agent summary; §6–7.2 read).
- Ford §6.2 counts and STUNT per-method rates (fetch-agent summary).
- libwebrtc RFC 7675 timers; NetBird go-nat PCP; HyperDHT / dht-rpc IPv6; n2n and tinc timers; libp2p same-NAT handling.
- Tailscale ProbeUDPLifetime production enablement; Tailscale history (shallow clone).
- Java I2P reference implementation; UPnP IGD:2 spec (404).
- Everything about our own implementation (row restates the brief).

Scratch copies of clones and documents: (a session scratchpad, not durable)

---

## 8. Addendum: leviculum direct_link wire vs rns-rs DirectLink

Sources read: ours = `git -C /home/emoore/Leviculum-punchdiag2 show origin/diag/punch-telemetry:leviculum-core/src/direct_link/wire.rs` (read-only). Theirs = lelloman/rns-rs HEAD `f672d64b0f` (2026-11-22), clone unshallowed for history.

**Verdict: a current rns-rs node (rns-net ≥ v0.14.0 / rns-core ≥ v0.9.0, 2026-11-18) and ours cannot upgrade a link between them.** Ours is byte-compatible with rns-net ≤ v0.13.0 / rns-core ≤ v0.8.0 (rns-net v0.13.0 = `9aa6de76`, 2026-11-13). The facilitator probe is still compatible, so the two can share a facilitator.

| Item | Ours (leviculum) | rns-rs now | rns-rs ≤ v0.13.0 |
|---|---|---|---|
| Signal transport | channel messages, MSGTYPE 0xFE00–0xFE04 | link DATA packet, context 0x80, `[version u8][kind u8][msgpack]`, never proven; kinds 0x00 HELLO … 0x07 CHANNEL_PART_ACK | channel 0xFE00–0xFE04 (= ours) |
| Capability check | none | HELLO `{v, r}` sent first, repeated every 2.5 s, 8 s timeout; no answer → fail 0x03 | none |
| msgpack keys | s f a p r, +ciris l (local cands ≤4), c (commit), q (predicted ports ≤16), b (birthday), **h = host payload (bin ≤16 KiB)** | s f a p r v m h x; **"m" (largest packet) required** in REQUEST/READY (`map_get("m")?`); **h = host-candidate endpoint list ≤2** | s f a p r |
| Punch / ack | 56 B: `RNSH`/`RNSA` + session 16 + **raw 32-byte token** + seq u32 BE | 52 B: magic 4 + session 16 + random nonce 16 + tag 16 = HMAC-SHA256(token, preceding 36 B)[..16]; ack echoes the punch nonce | 56 B (= ours) |
| Token | HKDF-SHA256(ikm = link key, salt = session id, info `"rns-holepunch-v1"`), 32 B | same derivation: `hkdf(32, derived_key, Some(session_id), Some(b"rns-holepunch-v1"))` (engine.rs) — but it is a MAC key, "never appears on the wire" | same, sent raw |
| Keepalive | punch frame with seq `u32::MAX` | `RNSK` + session 16 + counter u64 BE + HMAC-SHA256(keepalive key, preceding 28 B)[..16]; key = HKDF(…, info `"rns-holepunch-keepalive-v1"`); every 15 s, down after 45 s | punch frame seq u32::MAX, every 30 s (= ours) |
| MTU probe | `RNSM` + session + raw token | `RNSM` + session + nonce 8 + size u16 + zeros + tag 16 | — |
| Probe (facilitator) | RNSP 21 B (magic, ver 1, nonce 16); reply + family 4/6 + addr + port (24/36 B); or STUN Binding | identical | identical |

Breaking commits (all 2026-11-16/17, first released in rns-net v0.14.0 / rns-core v0.9.0 on 2026-11-18):
- `6d21c606` "direct-link: ask the peer before signalling, off the channel" (channel 0xFE00 → context 0x80 + HELLO; the reason given: Python peers raised ME_NOT_REGISTERED and stalled the channel).
- `f5595aaa` RNSK authenticated keepalives (interval later 15 s in `3046408c`).
- `01f7be6c` "random probe and punch nonces, checked against their source" (56-byte raw-token frames → 52-byte HMAC frames).
- Also `fad7129d` host candidates "h", `e46e48b1` "m".

Our module doc's compatibility claim held up to rns-net v0.13.0 and is stale now. Even after re-framing, the "h" key collides: ours is a bin payload, theirs an endpoint list.

**Does rns-rs's HMAC frame give the authentication we listed as a gap?** For the control datagrams, yes. Ours puts the 32-byte token on the wire in every punch, ack, keepalive and MTU probe. So any on-path observer (the NAT/CGN, the Wi-Fi, a facilitator-side tap) learns a bearer secret. They can then forge or replay acks, keepalives and MTU probes for that session from the peer's IP:port (keepalive seq u32::MAX is constant, so it replays as-is).

rns-rs never sends the token:
- Punches carry a fresh random nonce plus a truncated HMAC.
- An ack counts only if it echoes a nonce this side sent to that IP; replays of a seen nonce are rejected.
- Keepalives use a separate key and a monotonic counter checked for freshness.

It does not authenticate the carried Reticulum packets, which carry their own link encryption. Our tokenless 16-byte knocks are a separate, deliberate design and do not need it.
