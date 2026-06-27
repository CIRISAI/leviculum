#!/bin/bash
# tier3-hw-selftest.sh — rig-free selftest for run-tier3-hw.sh verdict logic.
#
# Drives run-tier3-hw.sh through its selftest seams (LEVICULUM_SELFTEST plus the
# stubbed profile / fn-list / cargo) and the simulated-vanish hook, with NO rig,
# NO build and NO real cargo. Asserts the board-vanish verdict policy:
#
#   - a vanish that PERSISTS across the retry -> tier3 RED with the vanished
#     board named (board_vanish=<vid:pid> firmware_self_reset_suspected), and
#     NOTHING marked INFRA_INVALID (the class is retired);
#   - a clean run -> tier3 GREEN, no vanish tokens;
#   - a vanish that RECOVERS on the retry -> tier3 RED with the SAME attribution
#     as the persistent path (board_vanish=<vid:pid> firmware_self_reset_
#     suspected): post-VFIO a vanish is always a real self-reset, so recovery on
#     the retry does not make it acceptable;
#   - a genuine test failure with no vanish -> tier3 RED (regression unaffected).
#
# Each case runs with HOME pointed at a throwaway dir so the script's state
# (~/.local/state/leviculum-ci) lands in the sandbox and never pollutes real CI.
#
# Usage: bash scripts/tier3-hw-selftest.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TARGET="$SCRIPT_DIR/run-tier3-hw.sh"

# Stub cargo: emit one libtest result line per fn. $1=profile $2=attempt, the
# rest are fn-names. RESULT (ok|FAILED) and exit code are chosen per case.
STUB_OK='prof="$1"; att="$2"; shift 2; for f in "$@"; do echo "test reticulum_integ::$f ... ok"; done; exit 0'
STUB_FAIL='prof="$1"; att="$2"; shift 2; for f in "$@"; do echo "test reticulum_integ::$f ... FAILED"; done; exit 101'

FAILED=0

# run_case <name> <expect_rc> -- <env assignments...>
# Captures combined output + exit code of one stubbed run. Echoes nothing;
# leaves $OUT and $RC set for the caller's assertions.
run_case() {
    local sandbox
    sandbox=$(mktemp -d)
    OUT=$(env HOME="$sandbox" \
        LEVICULUM_SELFTEST=1 \
        LEVICULUM_SELFTEST_PROFILE=default \
        LEVICULUM_SELFTEST_FNS="lora_smoke" \
        LEVICULUM_SETTLE_TIMEOUT=0 \
        "$@" \
        bash "$TARGET" 2>&1)
    RC=$?
    rm -rf "$sandbox"
}

assert_contains() {
    local hay="$1" needle="$2" label="$3"
    if grep -qF -- "$needle" <<<"$hay"; then
        echo "  PASS: $label"
    else
        echo "  FAIL: $label (missing: $needle)"
        FAILED=1
    fi
}

assert_absent() {
    local hay="$1" needle="$2" label="$3"
    if grep -qF -- "$needle" <<<"$hay"; then
        echo "  FAIL: $label (unexpected: $needle)"
        FAILED=1
    else
        echo "  PASS: $label"
    fi
}

assert_rc() {
    local got="$1" want="$2" label="$3"
    if [[ "$got" == "$want" ]]; then
        echo "  PASS: $label (rc=$got)"
    else
        echo "  FAIL: $label (rc=$got want=$want)"
        FAILED=1
    fi
}

echo "== Case 1: persistent vanish -> RED with board attribution, no INFRA_INVALID =="
run_case \
    LEVICULUM_SELFTEST_CARGO="$STUB_OK" \
    LEVICULUM_SIMULATE_VANISH=default \
    LEVICULUM_SIMULATE_VANISH_PERSIST=1 \
    LEVICULUM_SIMULATE_VANISH_VIDPID=1209:0001
assert_rc "$RC" 1 "persistent vanish exits non-zero (RED)"
assert_contains "$OUT" "tier3 RED (expected_marginal=0 skipped=0 board_vanish=1209:0001 firmware_self_reset_suspected)" "verdict line names the board + suspected cause"
assert_contains "$OUT" "BOARD VANISH (RED)" "loud board-vanish banner emitted"
assert_absent  "$OUT" "INFRA_INVALID" "no INFRA_INVALID class anywhere"
assert_absent  "$OUT" "infra_invalid=" "no infra_invalid verdict counter"

echo "== Case 2: clean run -> GREEN, no vanish tokens =="
run_case LEVICULUM_SELFTEST_CARGO="$STUB_OK"
assert_rc "$RC" 0 "clean run exits zero (GREEN)"
assert_contains "$OUT" "tier3 GREEN (expected_marginal=0 skipped=0)" "plain GREEN verdict, no vanish fields"
assert_absent  "$OUT" "board_vanish=" "no board_vanish token on a clean run"

echo "== Case 3: vanish recovers on retry -> RED with same attribution as persistent =="
run_case \
    LEVICULUM_SELFTEST_CARGO="$STUB_OK" \
    LEVICULUM_SIMULATE_VANISH=default \
    LEVICULUM_SIMULATE_VANISH_VIDPID=1a86:55d4
assert_rc "$RC" 1 "recovered vanish exits non-zero (RED)"
assert_contains "$OUT" "tier3 RED (expected_marginal=0 skipped=0 board_vanish=1a86:55d4 firmware_self_reset_suspected)" "recovered vanish RED with board attribution + suspected cause"
assert_contains "$OUT" "BOARD VANISH (RED)" "loud board-vanish banner emitted"
assert_contains "$OUT" "recovered on retry" "diagnostic note distinguishes recovered from persisted"
assert_absent  "$OUT" "tier3 GREEN" "recovered vanish is not GREEN"
assert_absent  "$OUT" "transient_board_vanish=" "no transient GREEN verdict token"

echo "== Case 4: genuine test failure, no vanish -> RED (regression path intact) =="
run_case LEVICULUM_SELFTEST_CARGO="$STUB_FAIL"
assert_rc "$RC" 1 "real test failure exits non-zero (RED)"
assert_contains "$OUT" "tier3 RED (expected_marginal=0 skipped=0)" "plain RED verdict, no vanish fields"
assert_absent  "$OUT" "board_vanish=" "no board_vanish token for a plain test failure"

echo
if (( FAILED == 0 )); then
    echo "tier3-hw-selftest: ALL PASS"
    exit 0
else
    echo "tier3-hw-selftest: FAILURES"
    exit 1
fi
