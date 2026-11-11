# Loquax: what a Loquax Mailbox is, at home and on an expedition

Decided by Lew on 2026-11-11 (order 1677, Refs #232). Part of the
[Loquax design record](loquax.md), beside [the home node](loquax-home-node.md)
(the Mailbox as built), [N homes under one identity](loquax-homes.md),
[reaching the mailbox from anywhere](loquax-reachability.md) and
[the propagation node](loquax-mailbox.md).

This page says how far a Loquax Mailbox is a Reticulum node. Each rule is
an invariant with a number, **M1** to **M6**, so later orders cite it; each
says what enforces it today and what is not built yet. The open points
carry numbers too, **O1** to **O6**.

## M1. The Mailbox is an LXMF endpoint with a device sync, not a node

The Mailbox holds the user's identity, store and ratchets and talks to two
sides: inward to its paired devices (the client protocol of
[the home node](loquax-home-node.md)), and outward as an ordinary LXMF
participant: it announces, takes and opens links, collects from
propagation nodes. It never forwards another person's packets and never
acts as a propagation node. Everything that makes a node lives in the
`lnsd` beside it, as lxmd lives beside rnsd. The Mailbox has no network
access of its own: it runs behind an lnsd's shared instance. On a phone in
full-node mode the embedded core plays that lnsd, the same split inside
one process.

What enforces it today:

- The Mailbox's node is built with transport off, for every way it can
  reach the network: `ReticulumNodeBuilder` (`loquax/src/engine/shell.rs:902`)
  takes `enable_transport(false)` before the shared instance, the TCP leaf
  and the own-node mode are chosen.
- The Mailbox is a propagation client and nothing more:
  `enable_propagation_client` (`loquax/src/engine.rs:1888`). Loquax does not
  link the propagation node's crate, `leviculum-lxmf-node`, except as a
  dev-dependency for its tests (`loquax/Cargo.toml`, under
  `[dev-dependencies]`); the propagation node on a home host is `lnpnd`,
  under an identity of its own.
- Beside a running lnsd the Mailbox is a client of its shared instance;
  `decide` (`loquax/src/home/upstream.rs:39`) picks that way whenever the
  daemon answers.

**O1, the own-node mode.** Without an answering lnsd, or with
`mailbox_upstream = mesh`, the Mailbox runs on a node of its own: it dials
the entry servers and serves the phones' port itself
(`Upstream::Mesh`, `loquax/src/engine/shell.rs:929`). Transport stays off,
so M1 holds, but nothing is a full node beside it and M2 does not. It is
the phone's mailbox case on a computer. Whether a computer keeps that mode
or always starts an lnsd beside its Mailbox is open.

## M2. The lnsd beside it is a full node, on purpose

A home node that runs all the time with internet is the most valuable
relay the mesh has. The lnsd beside a Mailbox therefore runs the
zero-config defaults ([zero-config lnsd](zero-config.md)): AutoInterface,
the two servers, discovery, BLE, and transport on. The user who wants
Bluetooth only, or the mailbox only, opts out per medium.

What enforces it today: lnsd's transport default is on when the config does
not name the key, `enable_transport` (`leviculum-std/src/config.rs:75`); the
other defaults are the zero-config page's.

**O2, the cost of transport on a metered link.** Before the transport
default is nailed down for the home, the reviewer measures what a transport
node at the backbone pulls per day (on workorse and miauhaus).
Unmeasured as of 2026-11-11.

## M3. Devices are admitted by pairing only, never by port

The devices' port, today the access point listener a Mailbox adds to its
lnsd's config or a BLE interface, is a door to the shared instance, not a
security boundary. A device that is not paired gets nothing from the
Mailbox, whatever port it came in on: no entry, no list, no name, not even
the word whether its request was well formed, and nothing it sends is
kept.

What enforces it today:

- Every request on `loquax.home` but `/join` is refused with status 2
  (`NotAdmitted`) before it is read when the link's identity is not an
  admitted device: the gate in `route` (`loquax/src/home/server.rs:151`),
  through `refuse_unless_admitted` (`loquax/src/home/server.rs:222`), which
  asks `is_admitted` (`loquax/src/store.rs:2946`). A link that was not
  identified gets status 1 before that. Each handler keeps its own check
  as a second line.
- Every refusal says so in the log with the device's hash: the free-text
  `HOME <ask> refused device=<hash> reason=not-admitted` and the structured
  event `home_refuse` (`loquax/src/events.rs:242`), `HOME_REFUSE device=
  ask= reason=`, in the catalogue. A path the Mailbox does not serve is
  logged as `ask=unknown`, so a stranger's text never reaches the log. A
  join with an unknown or expired token is `HOME_REFUSE ask=join
  reason=token` or `reason=expired`.
- The test `an_unpaired_device_gets_nothing_whatever_it_asks`
  (`loquax/src/home/server.rs:2600`) asks every path the Mailbox serves,
  well formed, malformed and empty, and one it does not, as a device that
  never paired: every answer is `NotAdmitted`, nothing is accepted,
  watched or learned, and the archive, the device list and the outbox are
  the same after. Its control, the paired device, gets its pull, `/about`,
  `/pages` and `/send` answered from the same store. Before 1677 a
  malformed request from a stranger was answered `Malformed` and left no
  refusal in the log: the handlers read the request before they checked
  the device.
- The phones' port is an access point listener, `block`
  (`loquax/src/home/phone_server.rs:30`), on port 4243 unless setup names
  another: it keeps the mesh's announces from the phones and is no part of
  the admission.

**O3, the cell.** The end-to-end proof is a periculum cell,
`loquax_mailbox_unpaired_device_gets_nothing`: a mailbox on a Rust node
with its devices' port, a client that never paired asking `/about`,
`/pages`, `/send` and a pull over a real link, the refusals in the
mailbox's log with its hash, the store unchanged, and the paired device's
answers in the same cell. Owed by order 1677.

## M4. Rendezvous is the mesh's job

A device away from home does not dial its home directly: that needs NAT
help, an open port and widens the attack surface. It joins the mesh as a
leaf anywhere, port 4343 of leviculum.network being one such door (an access
point that floods no announces), and reaches its Mailbox by path and link.
The home's own access point port serves the LAN. The ladder and the
rendezvous are [reaching the mailbox from anywhere](loquax-reachability.md);
the shipped entry list is `DEFAULT` (`loquax/src/home/rendezvous.rs:56`).

## M5. The expedition is the same software without TCP

A car node is an lnsd with LoRa on the big antenna and transport on,
`lnpnd` when the group wants a propagation node, and the Mailbox behind
it. A handheld syncs with it over BLE or WiFi near the car. A handheld
with its own LoRa (the M9, an LNode) stays a full node and reaches its
Mailbox over LoRa hops like any destination. A phone without LoRa waits
for the car.

Two rules depend on the medium. They belong to the Mailbox and its
clients, not to the core (the core passes packets and knows nothing of
mail):

- **Sync by cost.** Over BLE or WiFi everything; over LoRa only text,
  receipts and positions. Pictures and files wait for a fast medium, and
  the client says so ("3 Bilder warten aufs Auto"). The core knows the
  path's next-hop bitrate, `next_hop_interface_bitrate`
  (`leviculum-core/src/node/mod.rs:4384`).
- **The Mailbox's announces over LoRa follow the slow cadence**: 5 minutes
  while moving, an hour while still ([announce cadence](announce-cadence.md)).

**O4, sync by cost, not built.** The client protocol has a page bound for
a narrow client (1363) and a board's fold (1439), but nothing chooses by
the medium, and a client of a shared instance sees only its local
interface: the next-hop bitrate it would ask is the daemon's, which it
cannot read yet.

**O5, the LoRa cadence for the Mailbox's announces, not built.** The
Mailbox announces at start and every 360 minutes on every interface,
`ANNOUNCE_EVERY` (`loquax/src/home/daemon.rs:74`), whatever moves.

## M6. Every Loquax is a home; an external Mailbox is a second one

Decided by Lew on 2026-11-11 ([the expedition map](loquax-expedition-map.md),
section 11). The mailbox-first decision of 2026-11-10 had two modes the
user picks at the first start on Android: the Mailbox on the phone, or an
external one the phone is a client of. They become two behaviours of one
app:

- Every Loquax is always a home of its own, holding the identity, its
  store and its ratchets, as the Mailbox on the phone does today
  (`loquax-android/src/mailbox.rs`).
- An external Mailbox, once set up, is a second home of the same
  identity, the N homes of [N homes under one identity](loquax-homes.md).
- While the external Mailbox is in reach, the device rests as its
  client: it takes the directory, the names and the pages from it and
  hears no mesh of its own ([the propagation node](loquax-mailbox.md),
  section 5). When the Mailbox is out of reach, the device is the node:
  its own home fans out, collects and hears the mesh, and the homes
  reconcile when they see each other again.
- The app switches by reachability; the user picks nothing. The
  first-start question on Android retires. Setting up an external
  Mailbox stays the one step a user takes.

What enforces it today: nothing. It depends on the N homes, of which
nothing is built, and on the phone as a full node
([the phone as a full node](loquax-phone-node.md)), a concept with nothing
built. Until both land the two modes run as built.

**O6, the battery.** What the node behaviour costs a phone's battery has
not been measured on a real phone. A bad number means the switch needs a
brake (for instance a longer wait before the device becomes the node);
the measurement comes before the switch is built.
