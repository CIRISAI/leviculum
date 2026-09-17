# lnpnd(1)

## NAME

lnpnd -- LXMF propagation node daemon for Reticulum

## SYNOPSIS

**lnpnd** [**--config** *dir*] [**--rnsconfig** *dir*] [*overrides*]
**lnpnd** **--status** [**--peers**] [**-r** *hash*] [**--identity** *path*] [**--timeout** *secs*]
**lnpnd** **--sync** *peer* [**-r** *hash*]
**lnpnd** **--break** *peer* [**-r** *hash*]
**lnpnd** **--exampleconfig**

## DESCRIPTION

**lnpnd** runs an LXMF propagation node: a store-and-forward mailbox on a Reticulum mesh. Clients (Sideband, MeshChat, **lxmd**-based tools) configure the destination hash it prints at startup as their propagation node; messages for offline recipients wait in its store until the recipient drains its mailbox, and the node peers with other propagation nodes -- lnpnd, lxmd or board-hosted -- and syncs stored messages both ways.

The daemon attaches to a Reticulum shared instance that is already running -- **lnsd**(1), or Python's **rnsd** -- the way **lnstatus**(1) and **lncp**(1) do; it does not start a Reticulum stack of its own, and exits if no daemon answers (the packaged service restarts it until one does).

**lnpnd** is a drop-in counterpart to Python's **lxmd**. It reads lxmd's configuration format and key names from an lxmd-shaped config directory, carries the same remote-management interface (an `lxmf.propagation.control` destination answering `/pn/get/stats`, `/pn/peer/sync` and `/pn/peer/unpeer`), and its query verbs print output in lxmd's shape -- `lxmd --status --remote <hash>` works against an lnpnd node, `lnpnd --status --remote <hash>` against a stock lxmd node, and scripts written for one read the other.

Like lxmd, the daemon also keeps a mailbox of its own: an LXMF delivery destination on the node identity, announced with the configured display name and stamp cost. Each received message is written to the messages directory in the reference's packed-container file format, and the configured `on_inbound` program runs with the file's path as its argument.

## OPTIONS

**--config** *dir*
:   Path to the lnpnd config directory (see FILES). Default: `/etc/lnpnd` if it holds a config, else `~/.config/lnpnd` if it does, else `~/.lnpnd`.

**--rnsconfig** *dir*
:   Path to the Reticulum config directory whose `instance_name` decides which shared instance to join. Default: the platform's Reticulum config directory.

**--instance** *name*
:   Join this shared-instance name directly, overriding the Reticulum config file's.

**--data-dir** *dir*
:   Where the message store, peer table and client state live. Default: `<configdir>/storage`.

**-i**, **--on-inbound** *path*
:   Executable to run for each message received in the daemon's own mailbox; overrides the config file's `on_inbound`. The program receives the full path to the written message file as its argument.

**-s**, **--service**
:   Log to `<configdir>/logfile` instead of the terminal. The packaged systemd unit does not use this -- under systemd the journal captures stderr.

**-p**, **--propagation-node**
:   Accepted for lxmd command-line compatibility; lnpnd always runs the propagation node role.

**-v**, **-q**
:   Raise / lower the log level from the config's `loglevel`.

**--exampleconfig**
:   Print a verbose configuration example to stdout and exit.

Daemon settings can also be given as flags (`--stamp-cost`, `--peering-cost`, `--max-peers`, `--static-peers`, `--autopeer`, `--autopeer-maxdepth`, `--remote-peering-cost-max`, `--max-inbound-syncs`, `--from-static-only`, `--transfer-limit-kb`, `--sync-limit-kb`, `--store-limit-kb`, `--announce-interval-secs`, `--name`); a flag overrides the config file's value for the same setting.

## REMOTE MANAGEMENT

The query verbs run as a client against a node's control destination, identifying with an identity the node has allowed (its own, or one listed under `control_allowed` in its config). Without **--remote** they query the local daemon using the config directory's identity; with **--remote** *hash* (a propagation destination hash) they query that node, with **--identity** *path* naming the identity file to identify with. The counterpart tool's verbs are interchangeable with these.

**--status**
:   Print the node's status: store utilisation, costs, peer counts, traffic counters.

**--peers**
:   Print the peered nodes with their state, costs, sync keys and traffic.

**--sync** *peer*
:   Ask the node to sync with *peer* (a destination hash) now.

**-b**, **--break** *peer*
:   Break the node's peering with *peer*.

**--timeout** *secs*
:   Timeout for query operations (default 5, sync/break 10).

Exit codes follow lxmd's: 200 timeout, 203 no identity / bad hash, 204 access denied, 205 invalid data, 206 peer not found, 207 empty response.

## CONFIGURATION

The config file is lxmd's format and keys (`lxmd --exampleconfig` and `lnpnd --exampleconfig` describe the same file). All keys of the reference's `[propagation]`, `[lxmf]` and `[logging]` sections are accepted. Most are honoured identically: announce intervals and costs, autopeering and its depth, static peers, `max_peers`, `from_static_only`, `max_inbound_syncs`, `auth_required` (with the `allowed` file), `control_allowed`, storage and transfer limits, `display_name`, `stamp_cost`, `delivery_transfer_max_accepted_size`, `on_inbound`, `loglevel`, and the `ignored` file.

Keys accepted but not acted on, so a config file shared with lxmd parses cleanly (each is warned about at startup):

`announce_at_start` (in `[propagation]`)
:   lnpnd always announces the propagation node shortly after start, so `yes` is already the case and `no` has nothing to switch off. The `[lxmf]` key of the same name *is* honoured: it governs the daemon's own delivery destination.

`prioritise_destinations`
:   lnpnd's store eviction is size- and age-driven only. The reference uses this list to keep favoured destinations when the store overflows; lnpnd's store design (shared with the board-hosted node, where the list would not fit) does not carry per-destination priority.

`sequential_pn_stamp_validation`, `static_peers_bypass_sequential`
:   Stamp validation in lnpnd always runs sequentially on one worker thread, in arrival order, for every peer. The reference's toggles choose between parallel and sequential validation; lnpnd's single worker is the sequential behaviour, so `yes` is already the case and `no` has nothing to switch on.

Two defaults differ deliberately from the reference and are wire-legal: `propagation_stamp_cost_target` defaults to 0 (accept uploads without proof-of-work; lxmd never announces below 13) and `peering_cost` defaults to 0. Note that a stock lxmd peer can never sync *toward* a node announcing peering cost 0 -- its `peering_key_ready` short-circuits on a falsy cost -- so nodes that want stock peers to push messages to them should announce at least 1.

`enable_node = no` is refused: the propagation node is what lnpnd is. For a mailbox-only daemon use lxmd; for a client, **lnmsg**.

## FILES

The config directory keeps lxmd's layout:

*config*
:   The configuration file. Created from the example on first daemon start if missing.

*identity*
:   The node's identity -- its mesh address. Created on first start, never replaced automatically; the same file format Python's RNS uses, so an identity can move between daemons.

*allowed*
:   With `auth_required = yes`: identity hashes (one hex hash per line) allowed to drain mailboxes from this node.

*ignored*
:   Destination hashes (one per line) whose messages the daemon's own mailbox drops.

*storage/*
:   The data directory (unless **--data-dir** moves it): `messagestore/` (the propagation store), `peers/` (the peer table), `messages/` (the daemon's own received mail, one packed-container file per message, readable by the reference's `LXMessage.unpack_from_file`), and the node's client state.

The Debian package installs the config directory at */etc/lnpnd* and the data directory at */var/lib/lnpnd*, both owned by the `lnpnd` service user.

## EVENTS

With `LEVICULUM_EVENT_LOG=<path>` set, the daemon appends one structured line per accepted upload (`PN_ACCEPT`), mailbox request (`PN_GET`), eviction (`PN_EVICT`), peer-table change (`PN_PEER`), offer round (`PN_OFFER`), sync round (`PN_SYNC`) and own-mailbox delivery (`PN_MAILBOX`).

## SEE ALSO

**lnsd**(1), **lnstatus**(1), **lncp**(1)

The Python counterpart: `lxmd` from the LXMF distribution.
