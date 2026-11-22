# Design note (not shipped): authenticated punch frames

Status: recorded for later, per the review of 72be0f9 (item 1). Not
implemented, because it changes the punch frame and so breaks wire
compatibility with rns-rs direct links.

## Today

A punch or ack frame is `magic(4) | session(16) | token(32) | seq(4)`
(`wire::punch_frame`). The token is derived from the link key and the
session (`wire::punch_token`), and it travels in clear in every frame. Anyone
who sees one frame can replay or forge frames for that session. Since the
review fixes, a tokened frame goes only to an address the peer advertised over
the encrypted link, or to a source that has already sent one: strangers on a
sprayed or predicted port only ever receive a knock (`wire::knock_frame`:
magic and random bytes, no session, no token). That closes the hijack the
review found. It does not stop an on-path observer of a real punch from
replaying it.

## Later

Replace the clear token with a MAC over the frame:

    magic(4) | session(16) | seq(4) | nonce(8) | tag(16)
    tag = HMAC-SHA256(punch_key, magic | session | seq | nonce | src_hint)[..16]

- `punch_key` is derived from the link key as the token is today. It never
  leaves either node.
- A fresh nonce per frame, plus a per-session replay window on
  `(seq, nonce)`, makes a captured frame useless.
- An optional `src_hint` (the sender's view of its own mapping, hashed) would
  let the receiver notice a frame relayed from another address.
- The two-sided commit is unchanged.

Rollout: advertise support in REQUEST/READY (a new optional key, as `"q"` and
`"b"` were added); use MAC frames only when both ends advertise it, and keep
today's frames with rns-rs peers.
