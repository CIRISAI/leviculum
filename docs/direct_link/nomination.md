# Pair agreement by nomination (+ciris, fourth review H)

Each side used to settle on its OWN first address both heard and acked, and
the direct interface accepts only its chosen peer address. With two live
candidates (a hairpinning shared NAT: LAN and public; a dual-stack pair:
IPv4 and IPv6) the two ends could commit to different pairs and drop each
other's frames (TLA+ `samenat-hairpin`, property `SamePath`). Nomination fixes
it as ICE does (RFC 8445 8.1.1): one side chooses, the other takes the
choice on the 5-tuple it arrives on.

## The rule

In force only when both ends run the two-sided commit and both sent `"n"`
(REQUEST offers it, READY agrees). Without it, nothing changes: each end
settles on its own, as rns-rs does.

1. Every punch's sequence number carries the index of the target it was sent
   to, in the sender's own target list (`wire::punch_seq`, low 5 bits), and a
   nominate bit (bit 30; bit 31 stays clear, so a punch is never a keepalive
   or an MTU probe). The ack echoes the number.
2. The initiator (controlling) counts an ack only if it echoes the index of
   the address it came from: the reply came back over the very pair the
   punch went out on (the symmetric check, RFC 8445 7.2.5.2.1).
3. Once it has an address both heard and acked, the initiator sends only
   nominate-flagged punches to it, one every punch tick, until one is acked.
   Then its punch succeeds on that address.
4. The responder (controlled) never settles on its own. It settles on the
   source address a nominate-flagged punch arrived from, on the socket it
   arrived on, and acks it, but only from a source it had already heard and
   acked on that socket (review pass 5): one forged nominate-flagged
   datagram from a new address pins nothing. The token travels in clear in
   every punch frame, so an observer ON the path can still send a punch and
   then a nomination from its own address, exactly as it could already
   become a heard-and-acked pair without nomination; MAC'd frames
   (frame-auth-note.md, rns-rs v0.14's framing below) are what closes that. Both direct interfaces then start on one 5-tuple;
   nothing is re-pointed after the move.

## A lost nomination

Nominations repeat every tick, so one loss costs a tick. The responder
waits for one up to `NOMINATION_GRACE` (2 s) past its punch window, and a
settled initiator nominates as long, so a pair proved in the window's last
moment still gets its name through (the model found that without the grace
an equal window expired first). If none gets through, both waits are bounded:
the initiator's punch ends unacked, the responder's ends unnominated, each
sends REJECT PUNCH_FAILED under the commit, and the link stays relayed,
unchanged, on both sides (the model's `AbortLeavesLinkUnchanged`; its
`*-net-lossy` rows lose nominations too). A responder that took a nomination
whose ack was lost has sent COMPLETE; the initiator's REJECT ends it before
either moves. A miss is retried whichever verdict lands first.

## Nomination and the birthday hand-off: one rule

The birthday sockets open while the session socket's own punches are not
ACKED (heard or not: a random side that hears the peer but is never acked is
exactly one whose mapping is in the way). Whether a birthday hit takes the
punch off the session socket is one rule, `may_switch`:

- without nomination: only while the session socket has heard nothing (else
  the peer may settle on the session socket and the two ends split);
- the initiator: while nothing it sent from the session socket is acked. It
  nominates, so the responder follows wherever it settles;
- the responder: on a nomination only. Its birthday sockets ack every punch
  and keep listening, and report nothing but a nomination; so the initiator
  can prove a birthday socket's pair itself, name it, and the responder
  settles on exactly the socket the nomination arrives on.

So a nomination always lands on a socket the responder is listening on, and
the hard side's winner is, by construction, the socket the nomination
arrived on (responder) or the socket it handed off to and nominated from
(initiator).

## In rns-rs v0.14's framing

rns-rs v0.14 replaced the sequence number with a nonce and an HMAC over the
frame. Nomination needs two things the ack can return and an attacker cannot
forge: which target the punch went to, and whether it nominates. In that
framing both fit as authenticated bytes, with no change to the logic above:
a kind byte (punch, ack, nominate, nominate-ack) inside the MACed header,
and the target index as the first byte of the nonce (the rest stays random).
The ack carries the punch's nonce back under its own MAC, so the initiator
reads the index from it exactly as it reads the low bits of the sequence
number today. Until the fork adopts that framing, the sequence number
carries both.
