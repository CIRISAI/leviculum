# Loquax

An LXMF messenger for Reticulum. Plain `loquax` opens the interactive
messenger, with conversations, messages and a compose line: in a window on
a desktop, in the terminal where there is no display. `loquax gui` and
`loquax tui` insist on one of the two. The subcommands are for
scripts: `loquax send` queues one message for an LXMF address; `loquax
fetch` drains the propagation node's mailbox; `loquax address` prints this
machine's own address.

Loquax attaches to a Reticulum shared instance that is already running,
`lnsd` from the `leviculum` package or Python's `rnsd`, the way `lnomad`
does. It runs its own LXMF router but never a Reticulum stack of its own, so
a daemon has to be running.

The messenger keeps its conversations across restarts in
`$LNMSG_HOME/history.sqlite3`, an SQLite file; it refuses to start over one
it cannot read whole rather than begin an empty history on top of it.

The `loquax` package for amd64 and arm64 ships one binary with the `tui`
and `gui` features, built against glibc; the armhf package ships the
musl-static terminal build. Loquax Mailbox comes along as a system service,
installed switched off. Every option is in `loquax(1)`; the design is in
`docs/src/concepts/loquax.md` and the pages beside it.
