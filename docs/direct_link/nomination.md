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
   arrived on, and acks it, but only a nomination that proves the
   nomination secret, and only from a source it had already heard and acked
   on that socket.
5. The nomination secret: 16 random bytes the initiator draws for the
   session and sends only in REQUEST (`"k"`, beside `"n"`), inside the
   encrypted link. A nomination is the 56-byte punch frame (its clear
   token, its nominate-flagged sequence number) followed by a 16-byte HKDF
   proof of the secret over those 56 bytes (`wire::nomination_frame`,
   72 bytes), checked in constant time. Every leviculum parser still reads
   it as a punch of the session, so a direct interface acks a late one; only
   a peer that negotiated `"n"` AND answered `"v"` in READY (it holds the
   secret, review 6 finding 8) is ever sent one: a build that knew `"n"`
   but not `"k"` gets a plain punch instead of frames it would drop. The secret is a newtype
   whose `Debug` prints nothing of it: no log line, diagnostic event or
   panic message can carry it, nor the proof.

## Threat: a passive observer on the punch path

The punch token travels in clear in every punch frame, so anyone who sees
the path's datagrams has it.

- Before the secret: such an observer could send a punch from its own
  address (to become a source the responder acked) and then a
  nominate-flagged punch, and the responder would settle its direct path
  on the observer's address.
- After: a valid nomination needs the secret, which never crossed the
  punch path. The observer cannot produce one before the genuine
  nomination, and replaying the genuine one from another address after it
  changes nothing: the responder took the first valid nomination and
  stopped punching. One forged datagram from a new address pins nothing
  either way (the acked-source rule).
- Filling the acked-source list (review 6 finding 7): the list a
  nomination is taken from holds at most `PUNCH_TARGETS_MAX` sources, and
  an observer with the token can fill it from distinct ports before the
  genuine peer's first punch arrives. A source from one of the peer's own
  IPs (its advertised candidates; the peer's IP on a birthday socket) now
  takes the place of the oldest source that is not, and an unpreferred
  source never displaces anything (`remember_acked`). Limit: an observer
  BEHIND THE PEER'S OWN IP (its NAT, or a host sharing that public
  address) is preferred too and can still crowd the list; the outcome is a
  refused nomination, so the punch fails and the link stays relayed.
- A race won by replay: the proof binds the session, the token, the
  sequence number (target index and nominate bit) and the secret, but not
  the source address. It cannot: the initiator does not know the address
  its own socket's mapping shows the responder (an EDM NAT gives each
  destination its own), so there is nothing it could sign. An acked
  observer that receives the genuine nomination and re-sends it from its
  own address before the genuine copy arrives wins the race; the responder
  then settles on the observer, which relays or drops the link's
  end-to-end-encrypted traffic (it learns nothing, and could already
  delay or drop). A path that carries nothing back fails the direct link's
  liveness (silence: path down) and the link falls back to the relay.
- What remains: an ACTIVE attacker on the path that drops the genuine
  nomination and replays it from its own (already acked) address. It ends
  up relaying the link's end-to-end-encrypted traffic, which it could
  already black-hole or delay; it learns nothing it could not see before. Both direct interfaces then start on one 5-tuple;
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

## The link MTU after an aborted commit

A side lowers its link MTU exactly when it holds both COMPLETEs, its own
and the peer's: on its move, or when the peer's COMPLETE arrives after its
own commit wait ran out. The signals ride the ordered, reliable channel, so
the two ends agree once it has delivered what it carries; if the link dies
first there is nothing to reconcile. Against an OLD peer they agree whenever
the side whose wait runs out runs this code; when the old side times out
silently, they disagree, as they do today: no new side can see that.

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
