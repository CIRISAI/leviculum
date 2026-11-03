# RRC envelope vectors

Every RRC (Reticulum Relay Chat) envelope shape the pinned reference hub
and the bench's oracle client exchanged in the netns RRC cells, as CBOR
hex with its decoded form, for a clean-room codec (leviculum 1379) to
decode and re-encode byte for byte. Recorded by periculum 1378 (Refs #232).

| | |
|---|---|
| hub | rrcd 0.3.2, `kc1awv/rrcd` `f6d7e9d72bf83c70d7a9373ae8c5edaa052a7bf2`, with cbor2 5.6.5 (`../../rrcd/PIN.md`) |
| client | `../../scripts/rrc_client.py`, periculum's own, encoding with the same cbor2 |
| transport | RNS 1.5.2, one link per client, `--runner netns`, run 2026-11-03 |
| cells | `regression/rrc_room_two_clients`, `rrc_room_key_join`, `rrc_room_key_refused` |
| regenerate | run the three cells, then `python3 -I scripts/rrc-vectors.py <run>/logs` |

`envelopes.json` follows the `vectors/` corpus format (`schema`,
`generator`, `category`, `vectors`). Each vector:

- `cbor`: the envelope as it travelled, the plaintext of one link packet;
- `decoded`: the map with its integer keys as strings, byte strings as
  `{"bytes": "<hex>"}`, everything else as JSON;
- `keys`: the envelope keys present (0 `v`, 1 `t`, 2 `id`, 3 `ts`,
  4 `src`, 5 `room`, 6 `body`, 7 `nick`, 8 `dst`), `body` the CBOR type of
  key 6;
- `direction`, `type`, `code`; `cbor_len`; `wire_len`, the raw length of
  the RNS link packet that carried it (header, link id, context, token);
- `cell` and `client`, where it was seen.

One vector per distinct shape (direction, type, keys present, body type);
a `_2` suffix is the second shape of the same type and direction.

What the run showed, for a codec author:

- **rrcd re-sends the sender's MSG unchanged.** `msg_down` and `msg_up`
  are the same bytes: the hub sets key 4 to the link's identity, which a
  well-behaved client already put there, and forwards the envelope, its
  id and timestamp included, to every member, the sender too.
- **Map order is insertion order, minimal-length heads.** Keys come
  ascending as cbor2 writes a Python dict built in that order; `ts` is a
  `uint64` head (`1b`) for millisecond times, `id` an 8-byte string.
- **Bodies vary by type**: HELLO and WELCOME carry maps (WELCOME's key 3 is
  the limits map: nick 32, room name 64, body 350, rooms 32, 240 per
  minute), JOIN a text key only under rrcd's `+k` extension (`join_up_2`),
  the hub's PING a CBOR float (its monotonic clock) that a PONG returns
  as is, a client's PING whatever it chose (here a uint).
- **JOINED and PARTED come in two shapes**: to the joiner or leaver
  without key 7, to the other members with the nick of who moved.
- **Wire sizes step by the 16-byte AES block**: 131, 147, 163, 179 B for
  CBOR of 49 to 102 B. The 31-byte line is 90 B of CBOR in a 163 B link
  packet each way.

The `+k` key is an rrcd extension, not RRC: `join_up_2` and `error_down`
("bad key (+k)") come from the two key cells; the hub's "mode for #lab is
now: +k" NOTICE has the shape of `notice_down` and is not repeated.

Licensed MIT (`LICENSE`), like the `vectors/` corpus: the files are
recorded data, vendorable anywhere.
