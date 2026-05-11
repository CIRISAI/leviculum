#!/bin/sh
# Used by reticulum-integ/tests/lns_diag.toml.
#
# Verifies that `lns diag` never emits the raw transport-identity bytes
# (which it opens only to derive the shared-instance RPC authkey). The
# integ `exec` step splits its command on whitespace and runs it via
# `docker exec` without a shell, so the redirection / pipeline / `$(…)`
# this needs has to live in a script file; invoke it as
# `sh /opt/integ-scripts/lns_diag_no_secret.sh`.
#
# Exit 0 + "ok: identity bytes absent" on success; non-zero otherwise.
set -e

id_file=/root/.reticulum/storage/transport_identity
if [ ! -s "$id_file" ]; then
    echo "FAIL: $id_file missing or empty" >&2
    exit 2
fi

# Lowercase hex of the 64 identity bytes, the way `lns diag` would render
# any byte field. tr -dc keeps only hex digits (drops od's spaces/newlines).
h=$(od -An -tx1 -v "$id_file" | tr -dc 0-9a-f)
if [ -z "$h" ]; then
    echo "FAIL: could not hex-encode $id_file" >&2
    exit 2
fi

lns diag -c /root/.reticulum > /tmp/lns-diag-out.txt
if grep -qiF "$h" /tmp/lns-diag-out.txt; then
    echo "FAIL: identity bytes leaked into lns diag output" >&2
    exit 1
fi

echo "ok: identity bytes absent from bundle"
