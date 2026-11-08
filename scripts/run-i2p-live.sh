#!/usr/bin/env bash
# Run the two #[ignore]d live I2P tests on a private I2P network (T-0009, 1576).
#
# leviculum-std/src/interfaces/i2p/i2pd_live.rs holds the only coverage of
# I2PInterface against a real SAM bridge: that i2pd accepts our exact SESSION
# CREATE dialect and makes the Ed25519 destination we ask for, and that an
# HDLC frame crosses two live sessions over real tunnels. The mock-SAM tests in
# tier 0 reply with whatever they were told to, so a SAM reply i2pd changes is
# invisible to them.
#
# Until 1576 this started one fresh i2pd that reseeded from the public
# network, so the run needed the internet and took minutes. Now it runs on
# scripts/i2p-privnet.sh: five routers on loopback with their own netid, no
# reseed server, no internet. The dialect test runs on router 1; the loopback
# test's accepting side is on router 1 and its connecting side on router 2, so
# the frame crosses the private net.
#
# Not a tier: the reviewer runs it beside `emulated-corpus`.
#
# Usage: scripts/run-i2p-live.sh
# Exit status is cargo test's; 1 if the bed could not be brought up.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Bounds. TEST_BUDGET bounds the cargo run on the bed. Measured by 1568, a
# stream across the private net is usable 18 to 54 s after the routers
# restart; the slow test allows itself 240 s to connect once the sessions
# exist, and each SAM command 120 s.
TEST_BUDGET_S=600

say() { echo "[i2p-live] $*"; }

# Build first, so the routers' clock starts with the tests and the measured
# wall is the bed's, not the compiler's.
say "building the leviculum-std lib tests"
cargo test -p leviculum-std --lib --no-run

# shellcheck disable=SC2016  # expanded by the inner shell, inside the bed
exec bash scripts/i2p-privnet.sh 5 -- bash -c '
    echo "[i2p-live] running the live tests on $SAM_A and $SAM_B (budget '"$TEST_BUDGET_S"' s)"
    LEVICULUM_I2P_SAM="$SAM_A" LEVICULUM_I2P_SAM_PEER="$SAM_B" timeout '"$TEST_BUDGET_S"' \
        cargo test -p leviculum-std --lib interfaces::i2p::i2pd_live -- --ignored --nocapture --test-threads=1
'
