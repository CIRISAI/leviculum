# Four things RNS 1.5.x changed, and what each one costs us

## Why this document exists

Codeberg #331 listed four differences read out of the 1.5.0 source diff. None of them was an
observed break; each was an assumption about our side that nobody had checked. This page records
the check. One of the four was a real defect and is fixed; two were already covered and now have
tests saying so; one is latent and this page is the whole of the answer.

The reading was done against `/home/lew/coding/Reticulum` at 1.5.2 — `reference/Reticulum` in this
tree is pinned at 1.3.5 and does not contain any of the four changes. Line numbers below name the
generation they belong to, because on all four points the two generations disagree with each
other.

## 1. Keepalives on `last_outbound` — not our gap

**What changed.** 1.5.2 `Link.py:749` widened the watchdog gate:

```python
if now >= last_inbound + self.keepalive or now >= self.last_outbound + self.keepalive:
```

1.3.5 `Link.py:792` has only the first half.

**What it is actually for.** The stale test inside that branch is `now >= last_inbound +
self.stale_time` in both generations, and it is `last_inbound`-only in both. So the wider gate does
not make a 1.5.x destination tear a link down sooner — it cannot; entering the branch earlier with
fresh inbound just sends a keepalive and sleeps. What it fixes is the opposite end. A 1.3.5
INITIATOR that receives continuously and transmits nothing keeps its own `last_inbound` fresh,
never enters the branch, and never sends a keepalive; meanwhile the destination's `last_inbound`
ages out and the destination — of either generation — declares the link stale. 1.5.x closed that by
making the initiator's own silence a trigger.

**Where we stand.** `Link::should_send_keepalive` (`leviculum-core/src/link/mod.rs`) gates on
`last_keepalive` alone: an active initiator emits one every interval regardless of traffic in
either direction. That is a superset of both Python gates, so neither the 1.3.5 bug nor the 1.5.x
change can reach us. It is a superset by accident of a simpler rule, though, so
`silent_initiator_keeps_sending_keepalives_while_inbound_is_fresh` now pins it: a future "only
keepalive when idle" optimisation turns that test red before it reaches a peer.

## 2. Stream chunks six bytes larger — already our size

**What changed.** 1.3.5 `Buffer.py:229` sizes a chunk `channel.mdu - StreamDataMessage.OVERHEAD`
with `OVERHEAD = 2 + 6` (`:56`). The six is the channel envelope header, which `channel.mdu`
(`Channel.py:652`) has already subtracted — so 1.3.5 charged it twice. 1.5.x subtracts the
two-byte stream header only, and its chunks are six bytes larger, landing on the link MDU exactly.

**Where we stand.** `max_data_len` (`leviculum-core/src/link/channel/buffer.rs:63`) is
`channel_mdu - STREAM_DATA_HEADER_SIZE`. We already write 1.5.x-sized chunks and always have; the
larger chunk is not new traffic to us, only newly observable. On the read side
`RawChannelReader::receive` has no length gate at all — it appends whatever arrives — so the only
place a size could be refused is the `len > mdu` guard in `Channel::send_raw`, which is inclusive
at the boundary. `a_1_5_x_sized_stream_chunk_fits_the_channel_and_reads_back_whole` drives the
largest chunk 1.5.x can build through send, receive, unpack and reassembly and compares the bytes.

## 3. `hops >= 128` rejected as malformed — we could emit it

**What changed.** 1.5.2 rejects the value at both ends. `Packet.py:248`:

```python
if self.hops >= RNS.Transport.PATHFINDER_M:
    raise ValueError(f"Invalid hop count {self.hops}")
```

That runs in `unpack`, before the header type is read, so the packet is not over-ranged — it is
unreadable, and nothing downstream sees it. `Transport.py:1356` declines to emit one:
`if packet.hops > Transport.PATHFINDER_M-1: return False`. Neither check exists in 1.3.5, which
both accepts and forwards such a packet.

**Where we stood.** Our emit gates compared `packet.hops > self.config.max_hops` with `max_hops =
PATHFINDER_MAX_HOPS = 128`, and `packet.hops` is the receipt-incremented count
(`incoming_hop_count`, `transport.rs`). A relayed packet arriving with a wire byte of 127 therefore
became `hops = 128`, passed a `> 128` gate, and went back out stamped 128 — the first value a
1.5.x neighbour refuses to parse. Six emit sites were reachable this way: the three forward arms,
the capped announce broadcast, the targeted path response, and the shared-instance hand-off to
local clients (where a 1.5.x client raises on exactly the same byte).

**What changed here.** `Transport::hop_ceiling()` returns
`config.max_hops.min(PATHFINDER_MAX_WIRE_HOPS)` — the operator's reachability limit and the wire
ceiling, whichever binds first — and all six sites ask it.
`forward_never_stamps_a_hop_byte_a_1_5_x_peer_cannot_parse` is the reproduction: pre-fix it
forwards with `fwd[1] == 128`, post-fix the packet is dropped and accounted as `forward-max-hops`.
`forward_still_relays_one_below_the_wire_hop_ceiling` holds the other side, so the fix cannot
degrade into "stop forwarding".

**Receipt is deliberately untouched.** We still accept and deliver a hop byte 1.5.x would refuse.
Being liberal in what we accept costs a 1.3.5 peer nothing; being liberal in what we emit costs a
1.5.x peer the packet. Against the deviation rule: wire compatibility improves, semantic
compatibility is unaffected (no peer can expect delivery along a 128-hop path — every peer drops
announces above `PATHFINDER_M`), and the packets shed are unroutable or looping traffic.

## 4. `local_hops_delta` and the ephemeral transport identity — latent, and what to watch

Two 1.5.x options with no 1.3.5 counterpart.

**`local_hops_delta`.** Off by default (`Reticulum.py:257`); when the config option at
`Reticulum.py:515` is set, `Transport.py:337` draws `(urandom(1) % 6) + 2` once per boot and stamps
it in place of the true hop byte on packets the node originates (`Transport.py:1401`, `:1421`,
`:1435`, `:1596`, gated by `should_apply_delta` at `:1609`, which requires `packet.hops == 0` and
no shared instance). So on a mesh with it enabled, `hops == 0` no longer means "origin" and a
delta-enabled neighbour reads as two to seven hops further away than it is.

What that touches here: nothing that breaks, and nothing that is right either. Our `hops == 0`
tests are all path-table entries meaning "local client behind the shared instance"
(`transport.rs:8595`, `:10539`) or our own locally-created announces (`:10210`) — neither is a
remote node's claim about itself, so neither can be lied to. The cost is metric only: a
delta-enabled peer loses every path race against an honest one, and its
`ESTABLISHMENT_TIMEOUT_PER_HOP` scaling is drawn from a fiction. There is no fix to make, only a
thing not to assume: a mixed-mesh hop-count measurement is meaningless unless
`local_hops_delta` is known to be off on every Python node in it, and it is not observable from
outside.

**Ephemeral transport identity.** 1.5.2 `Transport.py:332-335`: a node with transport disabled and
`static_transport_identity` unset — the default for every non-transport 1.5.x node — replaces its
stored `Transport.identity` with a fresh `RNS.Identity()` at every start. Its transport hash is
therefore per-boot.

What that touches here: we hold no remote transport id anywhere that survives our own restart.
There is no `destination_table` on disk — the path table is in memory and refreshed from announces
— and no code path compares a received `transport_id` against a remembered one; the only equality
test on a carried transport id is against our OWN hash (`transport.rs:3059`), to decide whether a
transport-routed packet is addressed to us. A rebooted peer's stale `via` entries are the ordinary
stale-path case, which announce refresh and `PATHFINDER_EXPIRY_SECS` already cover.

The thing that does break is a test fixture. Any interop or rig scenario that records a Python
peer's transport hash in one run and expects it in the next is now reading a per-boot value; the
symptom is an unplaceable identifier in a log, which is exactly the shape the null-hypothesis rule
in CLAUDE.md was written for. Check the hash against the run's own nodes before treating it as a
stranger.

## What is NOT settled here

The fifth 1.5.x difference — the LRPROOF hop re-balance — is a design decision, not an audit
result, and lives in [Hop counting](../architecture-hop-counting.md) under #330. This page does not
reopen it.
