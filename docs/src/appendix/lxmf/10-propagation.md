# Propagation

Propagation lets a sender deposit a message at an always-reachable node for an
offline recipient to collect later. This section specifies the normative
client-facing wire surfaces and proves the envelope with `[VEC-PROP-ENVELOPE]`.
The node-internal store, peer selection, rotation, and sync scheduling are
informative ([Router internals](11-router-informative.md)).

## Propagation transfer envelope

A propagated message is wrapped as follows (`LXMessage.py:423-433`):

```
pn_encrypted_data = destination.encrypt(packed[16:])
lxmf_data         = packed[:16] || pn_encrypted_data
transient_id      = full_hash(lxmf_data)
if propagation_stamp: lxmf_data || = propagation_stamp     # 32 bytes
propagation_packed = msgpack([ wall_clock_timestamp, [ lxmf_data ] ])
```

Normative points:

- The destination hash (`packed[:16]`) stays in cleartext; the rest of the packed
  message (`packed[16:]`, i.e. source hash, signature, payload) is encrypted to
  the recipient (`LXMessage.py:427,430`).
- `transient_id = full_hash(lxmf_data)` and is computed **before** any
  propagation stamp is appended (`LXMessage.py:431-432`).
- The envelope is `msgpack([timestamp, [lxmf_data, ...]])`: a timestamp followed
  by a list of one or more `lxmf_data` blobs (`LXMessage.py:433`). The peer-sync
  path reuses the same shape with many blobs (`LXMPeer.py:462`).

### Proof: `[VEC-PROP-ENVELOPE]`

Because `destination.encrypt` uses a fresh ephemeral key per call, the ciphertext
is not reproducible; the vector is a round-trip proof. It records the inner
packed bytes, the cleartext `dest_hash_prefix`, the structure
`destination_hash(16) || destination.encrypt(packed[16:])`, the derived
`transient_id`, and proves `destination.decrypt(pn_encrypted) == packed[16:]`
(`decrypt_recovers_inner_tail = true`). An implementation MUST reproduce the
framing and the `transient_id` derivation; the ciphertext itself is
non-deterministic by construction.

## Propagation-node announce

See [Announce application data](09-announce-appdata.md) for the 7-element node
announce that advertises the node's limits and stamp costs.

## `/offer` (push to a node)

A syncing party requests `OFFER_REQUEST_PATH = "/offer"` (`LXMPeer.py:14`) over a
Link with the payload (`LXMPeer.py:381,385`):

```
offer = [ peering_key, [ transient_id, ... ] ]
```

where `peering_key` is the party's proof-of-work peering key (see below) and the
list is the transient-ids it offers. The node replies via `offer_response`
(`LXMPeer.py:396`); the reply is one of: `False` (node already has all),
`True` (node wants all), or a list (the subset the node wants). The wanted
messages are then pushed as one Resource carrying
`msgpack([timestamp, [lxmf_data, ...]])` (`LXMPeer.py:462-464`).

## `/get` (collect from a node)

A recipient requests `MESSAGE_GET_PATH = "/get"` (`LXMPeer.py:15`) with the
payload (`LXMRouter.py:1427-1449`):

```
[ want, have ]
```

- if both `want` and `have` are `None`, the node returns a list of the recipient's
  available `transient_id`s, sorted by size (`LXMRouter.py:1436-1449`);
- otherwise `have` lists transient-ids the client already holds (so the node can
  drop them) and `want` lists the ones to send (`LXMRouter.py:1451-`).

## Error codes (`LXMPeer.py:24-31`)

Returned by the node's request handlers:

| Code | Name | Meaning |
|------|------|---------|
| 0xF0 | `ERROR_NO_IDENTITY` | requester did not identify on the link |
| 0xF1 | `ERROR_NO_ACCESS` | requester not allowed |
| 0xF3 | `ERROR_INVALID_KEY` | invalid peering key |
| 0xF4 | `ERROR_INVALID_DATA` | malformed request |
| 0xF5 | `ERROR_INVALID_STAMP` | propagation stamp invalid |
| 0xF6 | `ERROR_THROTTLED` | rate limited (`PN_STAMP_THROTTLE` = 180 s) |
| 0xFD | `ERROR_NOT_FOUND` | requested message not found |
| 0xFE | `ERROR_TIMEOUT` | request timed out |

## Peering key

A peering key is a proof-of-work over `peer_identity_hash || node_identity_hash`
with `WORKBLOCK_EXPAND_ROUNDS_PEERING` = 25 rounds against the node's advertised
`peering_cost` (`LXMPeer.py:242-265`; validated by `validate_peering_key`,
`LXStamper.py:48-51`). It authorizes a party to offer messages to the node.

## Transient ingest and expiry (informative)

A node stores each accepted message keyed by `transient_id`, validates its
propagation stamp in batches (`LXStamper.py:87-90`), and expires entries after
`MESSAGE_EXPIRY` = 30 days (`LXMRouter.py:38`). The on-disk layout and peer
bookkeeping are informative.
