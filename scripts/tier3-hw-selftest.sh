#!/bin/bash
# tier3-hw-selftest.sh — rig-free selftest for run-tier3-hw.sh verdict logic.
#
# Drives run-tier3-hw.sh through its selftest seams (LEVICULUM_SELFTEST plus a
# stubbed periculum) and the simulated-vanish / simulated-stale-firmware hooks,
# with NO rig, NO build and NO real periculum. Asserts the rig-honesty verdict
# policy that is this script's whole remaining reason to exist:
#
#   - a board vanish -> tier3 RED with the vanished board named
#     (board_vanish=<vid:pid> firmware_self_reset_suspected), and NOTHING
#     marked INFRA_INVALID (the class is retired);
#   - a clean run -> tier3 GREEN, no vanish tokens;
#   - a genuine scenario failure with no vanish -> tier3 RED (periculum's own
#     exit-1 contract, passed through unchanged);
#   - unverified LNode firmware -> tier3 RED naming the board;
#   - a corpus where nothing ran (periculum exit 3) -> tier3 SKIPPED, never
#     GREEN and never RED.
#
# Each case runs with HOME pointed at a throwaway dir so the script's state
# (~/.local/state/leviculum-ci) lands in the sandbox and never pollutes real CI.
#
# Usage: bash scripts/tier3-hw-selftest.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TARGET="$SCRIPT_DIR/run-tier3-hw.sh"

# Stub periculum: $1 is the --json-out path, the rest are scenario targets. It
# writes a schema-1-shaped results document with the chosen summary counters
# and exits with the chosen code, so the verdict block reads real JSON.
stub() {
    local rc="$1" marginal="$2" skipped="$3"
    cat <<EOF
json="\$1"; shift
cat > "\$json" <<JSON
{"schema":1,"summary":{"green":1,"red":0,"marginal":$marginal,"unsupported":0,"skipped_infra":$skipped,"total":1},"exit_code":$rc}
JSON
echo "SUMMARY green=1 red=0 marginal=$marginal unsupported=0 skipped_infra=$skipped total=1"
exit $rc
EOF
}

# Exit 2 and exit 3 write no usable document (periculum writes none on 2), so
# those stubs emit output only.
stub_no_json() {
    local rc="$1"
    printf 'echo "stub periculum exit %s"; exit %s\n' "$rc" "$rc"
}

FAILED=0

# run_case -- <env assignments...>
# Captures combined output + exit code of one stubbed run. Echoes nothing;
# leaves $OUT and $RC set for the caller's assertions.
run_case() {
    local sandbox
    sandbox=$(mktemp -d)
    OUT=$(env HOME="$sandbox" \
        LEVICULUM_SELFTEST=1 \
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

echo "== Case 1: board vanish -> RED with board attribution, no INFRA_INVALID =="
run_case \
    LEVICULUM_SELFTEST_PERICULUM="$(stub 0 0 0)" \
    LEVICULUM_SIMULATE_VANISH=1 \
    LEVICULUM_SIMULATE_VANISH_VIDPID=1209:0001
assert_rc "$RC" 1 "vanish exits non-zero (RED)"
assert_contains "$OUT" "tier3 RED (expected_marginal=0 skipped=0 board_vanish=1209:0001 firmware_self_reset_suspected)" "verdict line names the board + suspected cause"
assert_contains "$OUT" "BOARD VANISH (RED)" "loud board-vanish banner emitted"
assert_contains "$OUT" "UNTRUSTED" "banner says the post-vanish verdicts are untrusted"
assert_absent  "$OUT" "INFRA_INVALID" "no INFRA_INVALID class anywhere"
assert_absent  "$OUT" "infra_invalid=" "no infra_invalid verdict counter"

echo "== Case 2: clean run -> GREEN, no vanish tokens =="
run_case LEVICULUM_SELFTEST_PERICULUM="$(stub 0 0 0)"
assert_rc "$RC" 0 "clean run exits zero (GREEN)"
assert_contains "$OUT" "tier3 GREEN (expected_marginal=0 skipped=0)" "plain GREEN verdict, no vanish fields"
assert_absent  "$OUT" "board_vanish=" "no board_vanish token on a clean run"
assert_absent  "$OUT" "firmware_unverified=" "no firmware_unverified token on a clean run"

echo "== Case 3: periculum reports RED -> tier3 RED (contract passed through) =="
run_case LEVICULUM_SELFTEST_PERICULUM="$(stub 1 0 0)"
assert_rc "$RC" 1 "scenario failure exits non-zero (RED)"
assert_contains "$OUT" "tier3 RED (expected_marginal=0 skipped=0)" "plain RED verdict, no vanish fields"
assert_absent  "$OUT" "board_vanish=" "no board_vanish token for a plain scenario failure"

echo "== Case 4: governed carve-outs and skips are surfaced, not folded into the verdict =="
run_case LEVICULUM_SELFTEST_PERICULUM="$(stub 0 3 7)"
assert_rc "$RC" 0 "a run whose only non-green verdicts are marginal/skipped is GREEN"
assert_contains "$OUT" "tier3 GREEN (expected_marginal=3 skipped=7)" "counters come from periculum's summary"

echo "== Case 5: stale/unverified LNode firmware -> RED with firmware_unverified attribution =="
run_case \
    LEVICULUM_SELFTEST_PERICULUM="$(stub 0 0 0)" \
    LEVICULUM_SIMULATE_FW_STALE=1209:0001
assert_rc "$RC" 1 "unverified firmware exits non-zero (RED)"
assert_contains "$OUT" "tier3 RED (expected_marginal=0 skipped=0 firmware_unverified=1209:0001)" "verdict line names the unverified board"
assert_contains "$OUT" "FIRMWARE UNVERIFIED (RED)" "loud firmware-unverified banner emitted"
assert_absent  "$OUT" "tier3 GREEN" "unverified firmware is not GREEN"

echo "== Case 6: nothing ran (periculum exit 3) -> SKIPPED, neither GREEN nor RED =="
run_case LEVICULUM_SELFTEST_PERICULUM="$(stub_no_json 3)"
assert_rc "$RC" 0 "nothing-ran does not fail the tier"
assert_contains "$OUT" "tier3 SKIPPED" "verdict line says SKIPPED"
assert_absent  "$OUT" "tier3 GREEN" "nothing-ran must not read as a passing nightly"
assert_absent  "$OUT" "tier3 RED" "nothing-ran is a rig statement, never a protocol one"

echo "== Case 7: periculum exit 2 without a lock marker -> RED (harness error) =="
run_case LEVICULUM_SELFTEST_PERICULUM="$(stub_no_json 2)"
assert_rc "$RC" 1 "harness error exits non-zero (RED)"
assert_contains "$OUT" "periculum exited 2" "the cause is named in the log"

echo
if (( FAILED == 0 )); then
    echo "tier3-hw-selftest: ALL PASS"
    exit 0
else
    echo "tier3-hw-selftest: FAILURES"
    exit 1
fi
