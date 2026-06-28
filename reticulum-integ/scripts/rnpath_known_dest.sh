#!/bin/sh
# Exercise rnpath RPC commands that need a real, daemon-known destination hash.
# The destination is discovered from the live path table (get_path_table RPC),
# so the same script works against either daemon without host-side log access.
#
# Modes:
#   nexthop -> bare `rnpath <hash>` path lookup, exercises get_next_hop +
#              get_next_hop_if_name; prints NEXTHOP_OK on success.
#   drop    -> `rnpath -d <hash>` against an existing path, exercises drop_path
#              with a real hit (must return success); prints DROP_OK on success.

CFG=/root/.reticulum
MODE="$1"

JSON=$(rnpath -t -j --config "$CFG")
echo "path_table=$JSON"

HASH=$(printf '%s' "$JSON" | python3 -c 'import sys, json
t = json.load(sys.stdin)
print(t[0]["hash"] if t else "")')

if [ -z "$HASH" ]; then
    echo "NO_KNOWN_PATH"
    exit 3
fi
echo "selected_hash=$HASH"

case "$MODE" in
    nexthop)
        OUT=$(rnpath "$HASH" --config "$CFG" -w 12 2>&1)
        echo "$OUT"
        echo "$OUT" | grep -q "Path found" && echo "NEXTHOP_OK"
        ;;
    drop)
        OUT=$(rnpath -d "$HASH" --config "$CFG" 2>&1)
        echo "$OUT"
        echo "$OUT" | grep -q "Dropped path" && echo "DROP_OK"
        ;;
    *)
        echo "unknown mode: $MODE"
        exit 2
        ;;
esac
