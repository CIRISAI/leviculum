#!/usr/bin/env bash
# What `just sweep` promises, asserted without deleting a single artefact.
#
# The recipe's whole value is WHICH directories it sweeps and with which
# budget, and that is precisely what an eyeball on the Justfile gets wrong:
# this repository has two workspaces, and a sweep of the root leaves the
# firmware workspace's target directory untouched. Codeberg #381.
#
# The assertion is made against a stub `cargo` on PATH, so no build runs, no
# file is removed, and the test costs about a second. Three cases:
#
#   1. defaults    -- both workspaces, the documented budgets
#   2. overrides   -- the two parameters reach cargo-sweep unchanged
#   3. no sweeper  -- the recipe refuses and names the install line, and
#                     sweeps NOTHING (a guard that runs the sweep anyway is
#                     worse than no guard)
#
# Positive control, i.e. proof the test can fail: point it at a Justfile
# without the recipe and case 1 must go red.
#
#   SWEEP_JUSTFILE=/tmp/old/Justfile bash scripts/test-just-sweep.sh
#
# Usage: bash scripts/test-just-sweep.sh
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
JUSTFILE="${SWEEP_JUSTFILE:-$ROOT/Justfile}"

command -v just >/dev/null 2>&1 || {
    echo "test-just-sweep: just not on PATH"
    exit 1
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin"

# The stub answers the guard's version probe and records every sweep it is
# asked for. Any OTHER cargo subcommand is an error: the recipe must not
# build anything on the way to a sweep.
cat > "$WORK/bin/cargo" <<'STUB'
#!/bin/sh
if [ "${1:-}" != sweep ]; then
    echo "unexpected cargo subcommand: $*" >> "$CARGO_STUB_LOG"
    exit 1
fi
shift
if [ "${1:-}" = --version ]; then
    if [ "${STUB_HAS_SWEEP:-1}" = 1 ]; then
        echo "cargo-sweep-sweep 0.0.0-stub"
        exit 0
    fi
    echo "error: no such command: \`sweep\`" >&2
    exit 101
fi
echo "sweep $*" >> "$CARGO_STUB_LOG"
STUB
chmod +x "$WORK/bin/cargo"

failures=0
fail() {
    echo "test-just-sweep: FAIL: $*"
    failures=$((failures + 1))
}

run_sweep() { # run_sweep <log> [args...]; prints nothing, sets SWEEP_STATUS
    local log="$1"
    shift
    : > "$log"
    CARGO_STUB_LOG="$log" \
    STUB_HAS_SWEEP="${STUB_HAS_SWEEP:-1}" \
    PATH="$WORK/bin:$PATH" \
        just --justfile "$JUSTFILE" --working-directory "$ROOT" sweep "$@" \
        > "$log.out" 2>&1
    SWEEP_STATUS=$?
}

# --- 1. defaults: both workspaces, both documented budgets ------------------
run_sweep "$WORK/case1.log"
[ "$SWEEP_STATUS" -eq 0 ] || fail "default sweep exited $SWEEP_STATUS: $(cat "$WORK/case1.log.out")"
grep -qx 'sweep --maxsize 30GB \.' "$WORK/case1.log" \
    || fail "host workspace not swept to 30GB: $(cat "$WORK/case1.log")"
grep -qx 'sweep --maxsize 4GB leviculum-nrf' "$WORK/case1.log" \
    || fail "firmware workspace not swept to 4GB: $(cat "$WORK/case1.log")"

# --- 2. overrides: the parameters reach the sweeper unchanged ---------------
run_sweep "$WORK/case2.log" 7GB 1500
[ "$SWEEP_STATUS" -eq 0 ] || fail "overridden sweep exited $SWEEP_STATUS: $(cat "$WORK/case2.log.out")"
grep -qx 'sweep --maxsize 7GB \.' "$WORK/case2.log" \
    || fail "budget override did not reach the host workspace: $(cat "$WORK/case2.log")"
grep -qx 'sweep --maxsize 1500 leviculum-nrf' "$WORK/case2.log" \
    || fail "budget override did not reach the firmware workspace: $(cat "$WORK/case2.log")"

# --- 3. no sweeper installed: refuse, say how, sweep nothing ----------------
STUB_HAS_SWEEP=0 run_sweep "$WORK/case3.log"
[ "$SWEEP_STATUS" -ne 0 ] || fail "missing cargo-sweep was not refused"
grep -q 'cargo install --locked cargo-sweep' "$WORK/case3.log.out" \
    || fail "refusal does not name the install line: $(cat "$WORK/case3.log.out")"
grep -q '^sweep ' "$WORK/case3.log" \
    && fail "swept despite the missing sweeper: $(cat "$WORK/case3.log")"

if [ "$failures" -gt 0 ]; then
    echo "test-just-sweep: FAILED ($failures assertion(s))"
    exit 1
fi
echo "test-just-sweep: all cases passed"
