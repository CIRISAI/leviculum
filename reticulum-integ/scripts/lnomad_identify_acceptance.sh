#!/bin/bash
# lnomad identify (fingerprint) acceptance against real Python RNS.
#
# Proves the #115 identify feature end to end over Python interop: when lnomad
# is configured to identify to a node (its persistent per-node identify.toml
# opt-in), a real Python RNS request handler receives lnomad's own fingerprint
# as remote_identity; when lnomad is anonymous (the default) the handler sees
# nothing. Uses a minimal Python RNS responder (lnomad_identify_responder.py)
# rather than a full NomadNet so the observed remote_identity is logged directly
# and there is no executable-page env-var layer to confuse the result; the RNS
# request/identify mechanism exercised is exactly the one NomadNet uses.
#
# CRUCIAL: a unique shared-instance instance_name. On a host already running a
# shared instance (an lnsd/rnsd on the default @rns/default socket), leaving the
# default instance_name makes lnomad connect to THAT instance, not the responder
# under test, and identify does not survive the relay. Both the responder config
# and lnomad's --config point at the same config dir, so they share instance_name.
# enable_transport=No and NO interface: a local client to a local node needs no
# mesh, and AutoInterface can hang RNS.Reticulum() from multicast contention.
#
# On-demand acceptance, not a CI tier (the deterministic Rust test
# identified_fetch_reveals_fingerprint_to_server guards CI). Needs Python RNS.
#
# Env overrides: PY (python3), LNOMAD (musl release), CARGO_TARGET_DIR, BASE,
# INSTANCE (unique instance_name), SETTLE.
#
# Exit status: 0 on ACCEPT-PASS, non-zero on any failure.

set -u

PY="${PY:-python3}"
INSTANCE="${INSTANCE:-lnid_accept}"
SETTLE="${SETTLE:-8}"

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
LNOMAD="${LNOMAD:-$TARGET_DIR/x86_64-unknown-linux-musl/release/lnomad}"
RESPONDER="$(dirname "$0")/lnomad_identify_responder.py"

fail() { echo "ACCEPT-FAIL: $*" >&2; exit 1; }

command -v "$PY" >/dev/null 2>&1 || fail "python3 ('$PY') not found"
"$PY" -c 'import RNS' 2>/dev/null || fail "Python RNS not importable via '$PY'"
[ -f "$RESPONDER" ] || fail "responder not found: $RESPONDER"

if [ ! -x "$LNOMAD" ]; then
    echo "=== building lnomad release ==="
    ( cd "$REPO_ROOT" && CARGO_TARGET_DIR="$TARGET_DIR" cargo build --release -p lnomad 2>&1 | tail -3 )
    [ -x "$LNOMAD" ] || fail "lnomad binary not found after build: $LNOMAD"
fi

BASE="${BASE:-$(mktemp -d /tmp/lnomad-identify.XXXXXX)}"
CFG="$BASE/cfg"; LNCFG="$BASE/lnomad-config"
mkdir -p "$CFG" "$LNCFG/lnomad"

RPID=""
cleanup() { [ -n "$RPID" ] && kill "$RPID" 2>/dev/null; wait 2>/dev/null; rm -rf "$BASE"; }
trap cleanup EXIT INT TERM

cat > "$CFG/config" <<EOF
[reticulum]
  enable_transport = No
  share_instance = Yes
  instance_name = $INSTANCE
[interfaces]
EOF

echo "=== start Python responder (shared instance, instance_name=$INSTANCE) ==="
"$PY" "$RESPONDER" "$CFG" > "$BASE/resp.out" 2>"$BASE/resp.err" &
RPID=$!
sleep "$SETTLE"
kill -0 "$RPID" 2>/dev/null || { cat "$BASE/resp.err"; fail "responder exited during startup"; }

DEST=$(grep "^DEST " "$BASE/resp.out" | awk '{print $2}')
[ -n "$DEST" ] || { cat "$BASE/resp.err"; fail "responder did not report a destination"; }
echo "node dest: [$DEST]"

fetch() { XDG_CONFIG_HOME="$LNCFG" timeout 30 "$LNOMAD" --print --config "$CFG" "${DEST}:/page/whoami.mu"; }

echo "=== ARM A: anonymous (default, no identify.toml) ==="
rm -f "$LNCFG/lnomad/identify.toml"
A=$(fetch 2>/dev/null)
echo "$A"
echo "$A" | grep -qi "anonymous" || fail "anonymous arm: expected 'anonymous'"
echo "$A" | grep -qiE "identified as [0-9a-f]{32}" && fail "anonymous arm LEAKED an identity"

echo "=== ARM B: identified (identify.toml opts the node in) ==="
printf 'dest = ["%s"]\n' "$DEST" > "$LNCFG/lnomad/identify.toml"
B=$(fetch 2>/dev/null)
echo "$B"
FP=$(echo "$B" | grep -oiE "identified as [0-9a-f]{32}" | grep -oiE "[0-9a-f]{32}" | head -1)
[ -n "$FP" ] || fail "identified arm: responder did not observe a remote_identity"

echo "=== ARM B2: stability (persistent identity -> same fingerprint) ==="
FP2=$(fetch 2>/dev/null | grep -oiE "[0-9a-f]{32}" | head -1)
[ "$FP2" = "$FP" ] || fail "fingerprint not stable: '$FP' vs '$FP2'"

echo "=== cross-check: the observed fingerprint IS lnomad's own identity hash ==="
OWN=$("$PY" - "$LNCFG/lnomad/identity" <<'PYEOF'
import sys, hashlib
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization
key = open(sys.argv[1], "rb").read()[5:69]  # RTIC: magic4 ver1 key64 checksum2
xp = X25519PrivateKey.from_private_bytes(key[:32]).public_key().public_bytes(
    serialization.Encoding.Raw, serialization.PublicFormat.Raw)
ep = Ed25519PrivateKey.from_private_bytes(key[32:64]).public_key().public_bytes(
    serialization.Encoding.Raw, serialization.PublicFormat.Raw)
print(hashlib.sha256(xp + ep).digest()[:16].hex())
PYEOF
)
[ "$OWN" = "$FP" ] || fail "observed fingerprint $FP != lnomad's own identity hash $OWN"

echo "ACCEPT-PASS: real Python RNS receives remote_identity iff lnomad identifies; fp $FP is lnomad's own"
echo "LNOMAD-IDENTIFY-ACCEPT-COMPLETE"
exit 0
