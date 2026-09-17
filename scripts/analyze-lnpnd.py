#!/usr/bin/env python3
"""Summarise an `lnpnd` event log in one streaming pass, constant memory.

    python3 scripts/analyze-lnpnd.py /var/log/lnpnd/events.log
    zcat -f /var/log/lnpnd/events.log* | python3 scripts/analyze-lnpnd.py -

Built in the shape of the miauhaus `analyze.py`, and for the same reason: a
public node's event log runs to tens of gigabytes, so anything that holds
the log — or a set keyed by message — in memory does not finish. Every counter
below is O(1) in the number of lines. The only per-key state is per peer,
and that is capped at MAX_TRACKED_PEERS: a node peers with `max_peers` (20
by default) at a time, but a log spanning months carries every peer that
ever announced, so "bounded by the peer table" would be wrong. Peers past
the cap are counted, not tracked, and the report says how many.

Phases, in the order an operator asks the questions:

  1. Volume and span      -- how much log, over what window, what is in it
  2. Uploads              -- accepted and rejected, rejections by reason
  3. Store                -- utilisation against the limit, and evictions
  4. Peers                -- offer and sync rounds per peer, with outcome
  5. Clients              -- mailbox requests served, own mail received
  6. Liveness             -- gaps in the PN_STORE heartbeat
  7. Format health        -- lines that do not parse, events not in the list

The log format is `NAME key=value ... t=<ms>` (docs/src/structured-event-logs.md),
one event per line, scalar values. Lines from other subsystems are counted
and skipped: an lnpnd log also carries the stack's own events.
"""

import sys
from collections import defaultdict

# The events lnpnd emits. An event outside this list is reported in phase 7
# rather than ignored: a new event nobody told the analysis about is the
# thing most likely to carry the next finding.
KNOWN = {
    "PN_ACCEPT",
    "PN_REJECT",
    "PN_EVICT",
    "PN_GET",
    "PN_PEER",
    "PN_OFFER",
    "PN_SYNC",
    "PN_MAILBOX",
    "PN_STORE",
}

# A heartbeat gap worth naming. PN_STORE fires every STORE_MAINTENANCE_SECS
# (480 s, lnpnd/src/engine.rs:69), so anything past twice that is either a
# daemon that was down or a tick that took minutes.
HEARTBEAT_PERIOD_MS = 480_000
HEARTBEAT_GAP_MS = 2 * HEARTBEAT_PERIOD_MS

# The memory bound. A node holds at most `max_peers` peers at once, but a
# log that spans months names every peer that ever announced within reach,
# and an uncapped dict makes the constant-memory claim false exactly on the
# logs that need it. Peers past the cap are counted in `peers_untracked`.
MAX_TRACKED_PEERS = 512

# How many peers the report lists, most active first. The rest are summed.
PEERS_LISTED = 30


class Stats:
    """Every counter the report needs, and nothing per-message."""

    def __init__(self):
        self.lines = 0
        self.parsed = 0
        self.unparsed = 0
        self.by_event = defaultdict(int)
        self.unknown_events = defaultdict(int)
        self.first_t = None
        self.last_t = None

        # Phase 2
        self.accepted = 0
        self.accepted_dup = 0
        self.accepted_bytes = 0
        self.accepted_via = defaultdict(int)
        self.rejected_by_reason = defaultdict(int)
        self.rejected_via = defaultdict(int)
        self.stamp_value_min = None
        self.stamp_value_max = None

        # Phase 3
        self.store_used = None
        self.store_limit = None
        self.store_count = None
        self.store_used_max = 0
        self.evicted = defaultdict(int)
        self.evicted_bytes = 0

        # Phase 4: capped at MAX_TRACKED_PEERS, never by traffic.
        self.peer_offers = defaultdict(lambda: defaultdict(int))
        self.peer_syncs = defaultdict(lambda: defaultdict(int))
        self.peer_sync_bytes = defaultdict(int)
        self.peer_actions = defaultdict(lambda: defaultdict(int))
        self.peers_seen = set()
        self.peers_untracked = 0
        self.offered = 0
        self.wanted = 0

        # Phase 5
        self.get_forms = defaultdict(int)
        self.get_served = 0
        self.get_bytes = 0
        self.get_purged = 0
        self.mailbox_in = 0
        self.mailbox_bytes = 0

        # Phase 6: only the running gap state, never the timestamps.
        self.last_heartbeat_t = None
        self.heartbeat_gaps = 0
        self.heartbeat_gap_max = 0

    def track(self, peer):
        """Whether this peer is one of the tracked ones; counts it if not."""
        if peer in self.peers_seen:
            return True
        if len(self.peers_seen) >= MAX_TRACKED_PEERS:
            self.peers_untracked += 1
            return False
        self.peers_seen.add(peer)
        return True


def parse(line):
    """`NAME k=v ... t=<ms>` -> (name, {k: v}), or None."""
    fields = line.split()
    if not fields:
        return None
    name = fields[0]
    if "=" in name:
        return None
    values = {}
    for field in fields[1:]:
        key, sep, value = field.partition("=")
        if sep:
            values[key] = value
    return name, values


def as_int(values, key, default=0):
    try:
        return int(values.get(key, default))
    except ValueError:
        return default


def consume(stats, line):
    stats.lines += 1
    parsed = parse(line)
    if parsed is None:
        stats.unparsed += 1
        return
    name, values = parsed
    if name not in KNOWN:
        # The stack's own events share this log; count the lnpnd-shaped
        # strangers separately from ordinary stack traffic.
        if name.startswith("PN_"):
            stats.unknown_events[name] += 1
        stats.by_event[name] += 1
        return
    stats.parsed += 1
    stats.by_event[name] += 1

    t = as_int(values, "t", -1)
    if t >= 0:
        if stats.first_t is None:
            stats.first_t = t
        stats.last_t = t

    if name == "PN_ACCEPT":
        if values.get("dup") == "true":
            stats.accepted_dup += 1
        else:
            stats.accepted += 1
            stats.accepted_bytes += as_int(values, "bytes")
        stats.accepted_via[values.get("via", "?")] += 1
        value = as_int(values, "value", -1)
        if value >= 0:
            if stats.stamp_value_min is None or value < stats.stamp_value_min:
                stats.stamp_value_min = value
            if stats.stamp_value_max is None or value > stats.stamp_value_max:
                stats.stamp_value_max = value

    elif name == "PN_REJECT":
        stats.rejected_by_reason[values.get("reason", "?")] += 1
        stats.rejected_via[values.get("via", "?")] += 1

    elif name == "PN_EVICT":
        stats.evicted[values.get("reason", "?")] += 1
        stats.evicted_bytes += as_int(values, "bytes")

    elif name == "PN_STORE":
        stats.store_used = as_int(values, "used")
        stats.store_limit = as_int(values, "limit")
        stats.store_count = as_int(values, "count")
        stats.store_used_max = max(stats.store_used_max, stats.store_used)
        if t >= 0:
            if stats.last_heartbeat_t is not None:
                gap = t - stats.last_heartbeat_t
                if gap > HEARTBEAT_GAP_MS:
                    stats.heartbeat_gaps += 1
                    stats.heartbeat_gap_max = max(stats.heartbeat_gap_max, gap)
            stats.last_heartbeat_t = t

    elif name == "PN_OFFER":
        # The totals are counted for every peer; only the per-peer
        # breakdown is capped.
        stats.offered += as_int(values, "offered")
        stats.wanted += as_int(values, "wanted")
        peer = values.get("peer", "?")
        if stats.track(peer):
            stats.peer_offers[peer][values.get("dir", "?")] += 1

    elif name == "PN_SYNC":
        peer = values.get("peer", "?")
        if stats.track(peer):
            direction = values.get("dir", "?")
            result = values.get("result", "?")
            stats.peer_syncs[peer][f"{direction}:{result}"] += 1
            stats.peer_sync_bytes[peer] += as_int(values, "bytes")

    elif name == "PN_PEER":
        peer = values.get("peer", "?")
        if stats.track(peer):
            stats.peer_actions[peer][values.get("action", "?")] += 1

    elif name == "PN_GET":
        stats.get_forms[values.get("form", "?")] += 1
        stats.get_served += as_int(values, "count")
        stats.get_bytes += as_int(values, "bytes")
        stats.get_purged += as_int(values, "purged")

    elif name == "PN_MAILBOX":
        stats.mailbox_in += 1
        stats.mailbox_bytes += as_int(values, "bytes")


def human_bytes(n):
    for unit in ("B", "kB", "MB", "GB", "TB"):
        if abs(n) < 1000 or unit == "TB":
            return f"{n:.1f} {unit}" if unit != "B" else f"{n} B"
        n /= 1000.0


def human_ms(ms):
    seconds = ms / 1000.0
    if seconds < 90:
        return f"{seconds:.1f} s"
    minutes = seconds / 60
    if minutes < 90:
        return f"{minutes:.1f} min"
    hours = minutes / 60
    if hours < 48:
        return f"{hours:.1f} h"
    return f"{hours / 24:.1f} d"


def report(stats):
    out = sys.stdout.write

    out("== 1. Volume and span ==\n")
    out(f"lines            {stats.lines}\n")
    out(f"lnpnd events     {stats.parsed}\n")
    out(f"other events     {stats.lines - stats.parsed - stats.unparsed}\n")
    out(f"unparsed lines   {stats.unparsed}\n")
    if stats.first_t is not None and stats.last_t is not None:
        span = stats.last_t - stats.first_t
        out(f"span             {human_ms(span)} (t={stats.first_t}..{stats.last_t})\n")
    for name in sorted(KNOWN):
        if stats.by_event.get(name):
            out(f"  {name:<12} {stats.by_event[name]}\n")

    out("\n== 2. Uploads ==\n")
    out(f"accepted         {stats.accepted} ({human_bytes(stats.accepted_bytes)})\n")
    out(f"  duplicates     {stats.accepted_dup} (re-proven, not re-stored)\n")
    for via, count in sorted(stats.accepted_via.items()):
        out(f"  via {via:<10} {count}\n")
    if stats.stamp_value_min is not None:
        out(f"  stamp values   {stats.stamp_value_min}..{stats.stamp_value_max}\n")
    rejected = sum(stats.rejected_by_reason.values())
    out(f"rejected         {rejected}\n")
    for reason, count in sorted(stats.rejected_by_reason.items(), key=lambda kv: -kv[1]):
        out(f"  {reason:<14} {count}\n")
    for via, count in sorted(stats.rejected_via.items()):
        out(f"  via {via:<10} {count}\n")
    offered_total = stats.accepted + stats.accepted_dup + rejected
    if offered_total:
        out(f"accept rate      {100.0 * stats.accepted / offered_total:.1f} %\n")

    out("\n== 3. Store ==\n")
    if stats.store_limit:
        out(
            f"last reported    {human_bytes(stats.store_used)} of "
            f"{human_bytes(stats.store_limit)} "
            f"({100.0 * stats.store_used / stats.store_limit:.1f} %), "
            f"{stats.store_count} message(s)\n"
        )
        out(
            f"high-water       {human_bytes(stats.store_used_max)} "
            f"({100.0 * stats.store_used_max / stats.store_limit:.1f} %)\n"
        )
    else:
        out("no PN_STORE line in this log -- store utilisation unknown\n")
    evicted = sum(stats.evicted.values())
    out(f"evicted          {evicted} ({human_bytes(stats.evicted_bytes)})\n")
    for reason, count in sorted(stats.evicted.items(), key=lambda kv: -kv[1]):
        out(f"  {reason:<14} {count}\n")
    if stats.evicted.get("displaced"):
        out("  note: 'displaced' means the store was full and made room -- if this\n")
        out("        is not near zero the limit is too small for the offered load\n")

    out("\n== 4. Peers ==\n")
    peers = set(stats.peer_offers) | set(stats.peer_syncs) | set(stats.peer_actions)
    out(f"peers tracked    {len(peers)}\n")
    if stats.peers_untracked:
        out(
            f"peers past cap   {stats.peers_untracked} event(s) for peers beyond "
            f"MAX_TRACKED_PEERS={MAX_TRACKED_PEERS}\n"
        )
        out("  a peer count this high on a real node is itself the finding:\n")
        out("  max_peers bounds the LIVE table, not how many ever announced\n")
    out(f"offered/wanted   {stats.offered} offered, {stats.wanted} wanted\n")

    def activity(peer):
        return (
            sum(stats.peer_offers.get(peer, {}).values())
            + sum(stats.peer_syncs.get(peer, {}).values())
            + sum(stats.peer_actions.get(peer, {}).values())
        )

    listed = sorted(peers, key=lambda p: (-activity(p), p))[:PEERS_LISTED]
    if len(peers) > len(listed):
        out(f"  (the {len(listed)} most active of {len(peers)})\n")
    for peer in listed:
        offers = stats.peer_offers.get(peer, {})
        syncs = stats.peer_syncs.get(peer, {})
        actions = stats.peer_actions.get(peer, {})
        parts = []
        if offers:
            parts.append("offers " + " ".join(f"{k}={v}" for k, v in sorted(offers.items())))
        if syncs:
            parts.append("syncs " + " ".join(f"{k}={v}" for k, v in sorted(syncs.items())))
        if actions:
            parts.append(" ".join(f"{k}={v}" for k, v in sorted(actions.items())))
        if stats.peer_sync_bytes.get(peer):
            parts.append(human_bytes(stats.peer_sync_bytes[peer]))
        out(f"  {peer[:16]}  {'; '.join(parts)}\n")
        failed = sum(count for key, count in syncs.items() if not key.endswith(":ok"))
        if failed:
            out(f"      {failed} sync round(s) ended other than ok\n")

    out("\n== 5. Clients ==\n")
    out(f"mailbox requests {sum(stats.get_forms.values())}\n")
    for form, count in sorted(stats.get_forms.items()):
        out(f"  {form:<14} {count}\n")
    out(f"messages served  {stats.get_served} ({human_bytes(stats.get_bytes)})\n")
    out(f"purged on fetch  {stats.get_purged}\n")
    out(f"own mail in      {stats.mailbox_in} ({human_bytes(stats.mailbox_bytes)})\n")

    out("\n== 6. Liveness ==\n")
    if stats.by_event.get("PN_STORE"):
        out(f"heartbeats       {stats.by_event['PN_STORE']} (PN_STORE, every {human_ms(HEARTBEAT_PERIOD_MS)})\n")
        out(f"gaps > {human_ms(HEARTBEAT_GAP_MS):<10} {stats.heartbeat_gaps}\n")
        if stats.heartbeat_gaps:
            out(f"largest gap      {human_ms(stats.heartbeat_gap_max)}\n")
            out("  a gap is a daemon that was down or a tick that took minutes;\n")
            out("  cross-check against the journal for a restart\n")
    else:
        out("no PN_STORE line -- liveness cannot be distinguished from a quiet node\n")

    out("\n== 7. Format health ==\n")
    out(f"unparsed lines   {stats.unparsed}\n")
    if stats.unknown_events:
        out("PN_* events this analysis does not know (add them here):\n")
        for name, count in sorted(stats.unknown_events.items(), key=lambda kv: -kv[1]):
            out(f"  {name:<16} {count}\n")
    else:
        out("every PN_* event in this log is one the analysis accounts for\n")


def main():
    if len(sys.argv) != 2:
        sys.stderr.write(__doc__)
        return 2
    stats = Stats()
    path = sys.argv[1]
    stream = sys.stdin if path == "-" else open(path, "r", errors="replace")
    try:
        for line in stream:
            consume(stats, line)
    finally:
        if stream is not sys.stdin:
            stream.close()
    report(stats)
    return 0


if __name__ == "__main__":
    sys.exit(main())
