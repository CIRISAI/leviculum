#!/bin/sh
# Exercise `rnid -i <hash>` against the shared instance. rnid recalls the
# identity for a destination hash and, on a successful recall, calls
# reticulum._retain_identity(), which issues the identity_data:retain RPC to
# the daemon. The destination hash is discovered from the live path table so
# the script is daemon-agnostic and needs no host-side log access.
#
# Exit contract (the scenario asserts exit 0):
#   0  recall reached the retain path AND the retain RPC was accepted (clean)
#   3  no known path yet (cannot drive recall) -> scenario fails honestly
#   4  retain RPC was rejected: the daemon dropped the local client connection
#      so rnid logged "Shared instance RPC failed while retaining identity" and
#      still exited 0. That swallowed failure is exactly the pre-fix Rust gap
#      (request parser had no identity_data arm -> unrecognized request -> EOF).
#      Failing here makes the scenario HONEST: rnid's own exit 0 cannot mask it.
# The captured rnid stdout+stderr is still printed for the conformance report.

CFG=/root/.reticulum

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

# -R requests the identity from the network (drives recall to completion), -p
# forces a definite recall outcome (allow_none=False) so the result is visible
# instead of silently no-op. On a successful recall rnid calls _retain_identity,
# which issues the identity_data:retain RPC to the shared instance.
echo "--- rnid -i begin ---"
RNID_OUT=$(rnid -i "$HASH" -R -p -t 15 --config "$CFG" 2>&1)
RNID_EXIT=$?
printf '%s\n' "$RNID_OUT"
echo "rnid_exit=$RNID_EXIT"
echo "--- rnid -i end ---"

# Honesty gate. The daemon dropping the retain RPC manifests, client-side, as
# this single rnid log line (recv_bytes raised on the EOF). Its presence means
# the identity_data:retain RPC was NOT accepted. Python rnsd and the fixed Rust
# daemon never emit it. rnid itself still exits 0, so the line is the only
# client-observable signal of the gap; fail loudly on it.
if printf '%s' "$RNID_OUT" | grep -qF "Shared instance RPC failed while retaining identity"; then
    echo "RETAIN_RPC_REJECTED"
    exit 4
fi

echo "RETAIN_RPC_CLEAN"
exit 0
