# lnpnd

An LXMF propagation node daemon: a store-and-forward mailbox on a Reticulum
mesh. Clients (Sideband, MeshChat, `lxmd`-based tools) configure the
destination hash it prints at startup as their propagation node; messages for
offline recipients wait in its store until the recipient drains its mailbox,
and the node peers with other propagation nodes — lnpnd, lxmd or
board-hosted — and syncs stored messages both ways.

The daemon attaches to a Reticulum shared instance that is already running —
`lnsd`, or Python's `rnsd` — the way `lnstatus` and `lncp` do. It does not
start a Reticulum stack of its own, and exits if no daemon answers (the
packaged service restarts it until one does).

## Drop-in counterpart to lxmd

lnpnd reads lxmd's configuration format and key names from an lxmd-shaped
config directory, carries the same remote-management interface, and its query
verbs print output in lxmd's shape: `lxmd --status --remote <hash>` works
against an lnpnd node, `lnpnd --status --remote <hash>` against a stock lxmd
node, and scripts written for one read the other.

Like lxmd, the daemon also keeps a mailbox of its own: an LXMF delivery
destination on the node identity, announced with the configured display name
and stamp cost. Each received message is written to the messages directory in
the reference's packed-container file format, and the configured `on_inbound`
program runs with the file's path as its argument.

## Running

```sh
lnpnd                        # config from /etc/lnpnd, ~/.config/lnpnd or ~/.lnpnd
lnpnd --config DIR           # an explicit lxmd-shaped config directory
lnpnd --exampleconfig        # print a verbose configuration example
```

The Debian package installs lnpnd as a systemd service under a dedicated
`lnpnd` user, with the config directory at `/etc/lnpnd` and the data
directory at `/var/lib/lnpnd`. The service orders itself after `lnsd.service`
without requiring it: lnpnd needs *a* Reticulum shared instance, and the
Python `rnsd` serves just as well as `lnsd`. Once the daemon and a Reticulum
instance both run:

```sh
sudo -u lnpnd lnpnd --status --config /etc/lnpnd
```

prints the node's destination hash, store utilisation, costs, peer counts and
traffic counters.

## Remote management

The query verbs run as a client against a node's `lxmf.propagation.control`
destination, identifying with an identity the node has allowed (its own, or
one listed under `control_allowed` in its config):

```sh
lnpnd --status  [--peers] [-r HASH] [--identity PATH]
lnpnd --sync  PEER [-r HASH]      # ask the node to sync with PEER now
lnpnd --break PEER [-r HASH]      # break the node's peering with PEER
```

Without `--remote` they query the local daemon using the config directory's
identity. Exit codes follow lxmd's: 200 timeout, 203 no identity / bad hash,
204 access denied, 205 invalid data, 206 peer not found, 207 empty response.

## Configuration

The config file is lxmd's format and keys (`lxmd --exampleconfig` and
`lnpnd --exampleconfig` describe the same file); it is created from the
example on first daemon start if missing. All keys of the reference's
`[propagation]`, `[lxmf]` and `[logging]` sections are accepted, and most are
honoured identically. The keys accepted but not acted on
(`prioritise_destinations`, `sequential_pn_stamp_validation`,
`static_peers_bypass_sequential`) are warned about at startup; lnpnd(1)
explains each.

Two defaults differ deliberately from the reference and are wire-legal:
`propagation_stamp_cost_target` defaults to 0 (accept uploads without
proof-of-work) and `peering_cost` defaults to 0. A stock lxmd peer can never
sync *toward* a node announcing peering cost 0, so nodes that want stock
peers to push messages to them should announce at least 1.

The config directory keeps lxmd's layout: `config`, `identity` (the node's
mesh address, the same file format Python's RNS uses), the optional `allowed`
and `ignored` hash lists, and `storage/` with the message store, peer table
and the daemon's own received mail.

## Documentation

The manual page, lnpnd(1), is the complete reference: every option, the
remote-management interface, the configuration keys and their lxmd
compatibility notes, the file layout, and the structured event log.
