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

The daemon attaches to a Reticulum shared instance that is already running -- **lnsd**(1), or Python's **rnsd** -- the way **lnstatus**(1) and **lncp**(1) do; it does not start a Reticulum stack of its own. The instance need not be running *yet*: at startup **lnpnd** waits up to a minute for the daemon's IPC socket, which is what gets it through a boot where both start together and the daemon binds its socket a few seconds later. After that minute it exits, and the packaged service restarts it until a daemon answers.

**lnpnd** is a drop-in counterpart to Python's **lxmd**. It reads lxmd's configuration format and key names from an lxmd-shaped config directory, carries the same remote-management interface (an `lxmf.propagation.control` destination answering `/pn/get/stats`, `/pn/peer/sync` and `/pn/peer/unpeer`), and its query verbs print output in lxmd's shape -- `lxmd --status --remote <hash>` works against an lnpnd node, `lnpnd --status --remote <hash>` against a stock lxmd node, and scripts written for one read the other.

As lxmd does, the node tears down a link a client or peer opened to it once it has carried no payload for 180 seconds (`P_LINK_MAX_INACTIVITY`); keepalives do not count, so a client that holds a silent link open does not hold a link-table slot with it.

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

The query verbs run as a client against a node's control destination, identifying with an identity the node has allowed. A node always allows its own identity, so the local verbs work on a fresh installation with nothing configured; `control_allowed` in the config adds other people's identity hashes. Without **--remote** they query the local daemon using the config directory's identity; with **--remote** *hash* (a propagation destination hash) they query that node, with **--identity** *path* naming the identity file to identify with. The counterpart tool's verbs are interchangeable with these.

A node that refuses a query answers the refusal, and the verb exits 204. Python's lxmd registers its control paths behind `ALLOW_LIST`, which sends nothing at all to an identity it refuses; against such a node a refusal is indistinguishable from silence and exits 200, so lnpnd names the possibility on stderr rather than leaving "timed out" as the only account.

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

The three announced costs default to `propagation_stamp_cost_target = 13`, `propagation_stamp_cost_flexibility = 3` and `peering_cost = 1`, the same as a board's propagation node. 13 is the reference's own floor (`PROPAGATION_COST_MIN`; lxmd's default is 16), so a store accepted here travels through every default peer. Peering cost 1 rather than 0, because a stock lxmd peer never syncs toward a node announcing 0: its `peering_key_ready` returns false on a falsy cost (`LXMPeer.py:228`). The reference's default peering cost is 18. The mailbox's `[lxmf] stamp_cost` is a separate knob and keeps lxmd's default of 12.

`enable_node = no` is refused: the propagation node is what lnpnd is. For a mailbox-only daemon use lxmd; for a client, **lnmsg**.

## FILES

The config directory keeps lxmd's layout. lnpnd started on an lxmd config directory takes over its identity and its message store, so the messages lxmd held for its clients are served from the first start on.

*config*
:   The configuration file. Created from the example on first daemon start if missing.

*identity*
:   The node's identity -- its mesh address. Never replaced automatically; the same file format Python's RNS uses, so an identity can move between daemons. A start that finds no identity here creates one and says so before it joins the shared instance, logging a `PN_IDENTITY_CREATED` line with the new propagation destination hash. That line is the only notice a new address exists, so a node that is meant to continue an existing one must have that node's identity file copied here *before* its first start.

*allowed*
:   With `auth_required = yes`: identity hashes (one hex hash per line) allowed to drain mailboxes from this node.

*ignored*
:   Destination hashes (one per line) whose messages the daemon's own mailbox drops.

*storage/*
:   The data directory (unless **--data-dir** moves it): `lxmf/messagestore/` (the propagation store, at the path lxmd keeps it; a store found at the older `messagestore/` is moved there on start), `peers/` (the peer table), `messages/` (the daemon's own received mail, one packed-container file per message, readable by the reference's `LXMessage.unpack_from_file`), and the node's client state.

The Debian package installs the config directory at */etc/lnpnd* and the data directory at */var/lib/lnpnd*, both owned by the `lnpnd` service user. It enables the unit but does not start it: at install time neither an identity nor a configuration is in place, and a daemon started then would mint an address of its own and announce it. Place the identity, then `systemctl start lnpnd`. An upgrade over a running node restarts it and mints nothing; an upgrade over a stopped one leaves it stopped.

## SIGNALS

**SIGINT**, **SIGTERM**
:   Graceful shutdown: the node storage is flushed before the daemon exits.

**SIGUSR1**
:   Print the memory census to stderr, the same dump **lnsd**(1) prints on the same signal (see DIAGNOSTIC DUMP there). In a build with the `heap-accounting` feature (the `lnpnd-heapacct` package, whose `--version` ends in `+heapacct`) the `gap:` line subtracts the measured live heap bytes and a `=== Measured live ===` line follows; with `LEVICULUM_HEAP_SAMPLE=<n>` set at startup it also lists the top allocation call sites. A plain build prints `live n/a` there.

## EVENTS

With `LEVICULUM_EVENT_LOG=<path>` set, the daemon appends one structured line per accepted upload (`PN_ACCEPT`), rejected upload (`PN_REJECT`, with a fixed `reason` word), mailbox request (`PN_GET`), eviction (`PN_EVICT`), peer-table change (`PN_PEER`), offer round (`PN_OFFER`), sync round (`PN_SYNC`) and own-mailbox delivery (`PN_MAILBOX`), plus a `PN_STORE` line every 8 minutes carrying store size against the limit. `PN_STORE` fires whether or not traffic arrived, so it is also the log's liveness heartbeat: a quiet node and a dead one are otherwise the same absence of lines.

`scripts/analyze-lnpnd.py` summarises such a log in one streaming pass with constant memory, which matters because a public node's event log grows to tens of gigabytes. `packaging/logrotate/` caps it: a logrotate config, a oneshot and a 15-minute timer, with `packaging/logrotate/README.md` on why the rotation has to be `copytruncate` and what the ceiling comes to.

## SEE ALSO

**lnsd**(1), **lnstatus**(1), **lncp**(1)

The Python counterpart: `lxmd` from the LXMF distribution.
