#!/bin/bash
# lnomad end-to-end acceptance against a REAL NomadNet node.
#
# Proves that lnomad can fetch and render a live NomadNet micron page over the
# shared-instance path: NomadNet runs as the shared Reticulum instance AND as a
# node server hosting a known index.mu; lnomad connects as a client, fetches the
# page, and renders it. The rendered output is asserted to contain the known
# page content.
#
# This is an ON-DEMAND acceptance, not part of any CI tier. It needs a working
# `nomadnet` (and its Python `RNS`) on PATH or pointed at by the env vars below.
#
# Configuration (all overridable via the environment):
#   PY               python3 with RNS installed        (default: `python3`)
#   NOMADNET         nomadnet executable               (default: `nomadnet`)
#   LNOMAD           lnomad binary                     (default: the musl
#                    release build under the cargo target dir)
#   CARGO_TARGET_DIR cargo target dir used to locate LNOMAD and to build
#                    lnomad if it is missing            (default: `target`)
#   BASE             scratch dir for configs/logs      (default: mktemp)
#   NN_SETTLE        seconds to let nomadnet come up    (default: 25)
#   LNOMAD_TIMEOUT   seconds for the lnomad fetch       (default: 40)
#
# Exit status: 0 on ACCEPT-PASS, non-zero on any failure.

set -u

PY="${PY:-python3}"
NOMADNET="${NOMADNET:-nomadnet}"
NN_SETTLE="${NN_SETTLE:-25}"
LNOMAD_TIMEOUT="${LNOMAD_TIMEOUT:-40}"

# Locate the repo root so we can find (and, if needed, build) the binary
# without hardcoding a developer home directory.
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
LNOMAD="${LNOMAD:-$TARGET_DIR/x86_64-unknown-linux-musl/release/lnomad}"

fail() { echo "ACCEPT-FAIL: $*" >&2; exit 1; }

command -v "$PY" >/dev/null 2>&1 || fail "python3 ('$PY') not found on PATH"
"$PY" -c 'import RNS' 2>/dev/null || fail "Python RNS not importable via '$PY' (pip install rns)"
command -v "$NOMADNET" >/dev/null 2>&1 || [ -x "$NOMADNET" ] || fail "nomadnet ('$NOMADNET') not found (pip install nomadnet)"

BASE="${BASE:-$(mktemp -d /tmp/lnomad-accept.XXXXXX)}"
RNSCFG="$BASE/rns"
NNCFG="$BASE/nn"
mkdir -p "$RNSCFG" "$NNCFG/storage/pages"

NNPID=""
cleanup() {
    [ -n "$NNPID" ] && kill "$NNPID" 2>/dev/null
    [ -n "$NNPID" ] && wait "$NNPID" 2>/dev/null
    rm -rf "$BASE"
}
trap cleanup EXIT INT TERM

# Build lnomad (musl release) if the binary is missing.
if [ ! -x "$LNOMAD" ]; then
    echo "=== lnomad binary missing, building release ==="
    ( cd "$REPO_ROOT" && CARGO_TARGET_DIR="$TARGET_DIR" cargo build --release -p lnomad 2>&1 | tail -3 )
    [ -x "$LNOMAD" ] || fail "lnomad binary not found after build: $LNOMAD"
fi

cat > "$RNSCFG/config" <<EOF
[reticulum]
  enable_transport = Yes
  share_instance = Yes
[interfaces]
  [[Default Interface]]
    type = AutoInterface
    interface_enabled = Yes
EOF

cat > "$NNCFG/config" <<EOF
[node]
  enable_node = yes
  node_name = lnomad-acceptance
EOF

# Distinctive pages so the render is unambiguous to verify: heading, bold,
# an inline colour tag and a same-destination link.
cat > "$NNCFG/storage/pages/index.mu" <<'MU'
>Welcome to the lnomad acceptance node

This page is served by `!real NomadNet`!. Colour: `F00fblue`f done.

`[Read the about page`:/page/about.mu]
MU
cat > "$NNCFG/storage/pages/about.mu" <<'MU'
>About
Served by real NomadNet over the shared instance.
MU

echo "=== start nomadnet (shared instance + node) ==="
"$NOMADNET" --daemon --console --rnsconfig "$RNSCFG" --config "$NNCFG" > "$BASE/nn.log" 2>&1 &
NNPID=$!
sleep "$NN_SETTLE"   # come up, generate identity, enable node, announce

kill -0 "$NNPID" 2>/dev/null || { tail -20 "$BASE/nn.log"; fail "nomadnet exited during startup"; }

echo "=== compute node dest from identity ==="
[ -f "$NNCFG/storage/identity" ] || { tail -20 "$BASE/nn.log"; fail "nomadnet identity not generated"; }
DEST=$("$PY" - <<PY 2>"$BASE/dest-err.txt"
import RNS
RNS.Reticulum("$RNSCFG")
identity = RNS.Identity.from_file("$NNCFG/storage/identity")
dest = RNS.Destination(identity, RNS.Destination.OUT, RNS.Destination.SINGLE, "nomadnetwork", "node")
print(dest.hash.hex())
PY
)
if [ -z "$DEST" ]; then
    tail -5 "$BASE/dest-err.txt"
    fail "could not derive node destination from identity"
fi
echo "node dest: [$DEST]"

echo "=== lnomad --print fetch + render index.mu ==="
timeout "$LNOMAD_TIMEOUT" "$LNOMAD" --print --config "$RNSCFG" "${DEST}:/page/index.mu" \
    > "$BASE/out.txt" 2>"$BASE/err.txt"
LN_RC=$?
echo "lnomad-rc: $LN_RC"
echo "--- STDOUT ---"; cat "$BASE/out.txt"
echo "--- STDERR (tail) ---"; tail -5 "$BASE/err.txt"

echo "=== verify ==="
if [ "$LN_RC" -ne 0 ]; then
    fail "lnomad exited non-zero ($LN_RC)"
fi
if grep -qi "acceptance node" "$BASE/out.txt" && grep -qi "real NomadNet" "$BASE/out.txt"; then
    echo "ACCEPT-PASS: lnomad fetched and rendered the real NomadNet index page"
    echo "LNOMAD-ACCEPT-COMPLETE"
    exit 0
fi
fail "expected content not found in rendered output"
