#!/bin/bash
set -e

# Ensure Python output is unbuffered so docker logs captures rnsd output
export PYTHONUNBUFFERED=1

# RNS is pre-installed in the Docker image (see Dockerfile).
# No runtime pip install needed.

# AutoInterface tests need IPv6 link-local on eth0 to be out of the
# kernel DAD (Duplicate Address Detection) tentative state before the
# daemon starts; otherwise the unicast bind fails with EADDRNOTAVAIL
# and the AutoInterface orchestrator exits. Polled here, max 5 s.
if [ "${WAIT_FOR_DAD:-0}" = "1" ]; then
    for _ in $(seq 1 50); do
        if ! ip -6 addr show dev eth0 2>/dev/null | grep -q tentative; then
            break
        fi
        sleep 0.1
    done
fi

case "${NODE_TYPE}" in
    rust)
        exec /usr/local/bin/lnsd -v --config /root/.reticulum
        ;;
    python)
        # RNS_REQUIRE_SHARED is set on the container env so that Python tools
        # (rnprobe, rnpath, rncp, rnstatus, …) launched via `docker exec` fail
        # loudly when they cannot reach the daemon. rnsd itself IS the daemon
        # — with the var set, the vendored RNS patch refuses to start
        # ("started as shared instance"). Strip it for the daemon process only.
        unset RNS_REQUIRE_SHARED
        exec python3 -m RNS.Utilities.rnsd -v --config /root/.reticulum
        ;;
    *)
        echo "Unknown NODE_TYPE: ${NODE_TYPE}" >&2
        exit 1
        ;;
esac
