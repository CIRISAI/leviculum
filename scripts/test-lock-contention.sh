#!/bin/bash
# test-lock-contention.sh — the tier runners' reading of periculum's
# lock-contention marker (Codeberg #309).
#
# The marker stopped being an empty flag file in periculum #30/#31: it now
# carries `verdict=`, `suspect=`, `holder_pid=`, `holder_age_secs=` and a
# `detail=` sentence, and a contender writes `verdict=suspected_wedge` (or
# `misrecorded`/`unattributable`) when the process holding the rig lock does
# not look legitimate — a holder past 24 h, or one the kernel disagrees with.
# All three runners branched on the file's EXISTENCE and deleted it before
# reading a byte, so the loudest thing periculum can say arrived as the same
# `SKIPPED lock-held` as a plain overlap.
#
# What is asserted here is the consumer half of that: the helper's parse, and
# the two ledger lines each runner writes. No build, no docker, no rig; the
# tier command is stubbed through the runners' LEVICULUM_SELFTEST_TIER_CMD
# seam and `notify-send` is a fixture on PATH. ~1 s.
#
# run-tier3-hw.sh has its own harness and its cases live there
# (scripts/tier3-hw-selftest.sh): it is the only one of the three that sees
# periculum's exit code directly, so its case is about routing 2 AND 4 to the
# marker rather than about parsing it.
#
# Usage: bash scripts/test-lock-contention.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
FAILED=0

pass() { echo "  PASS: $1"; }
fail() { echo "  FAIL: $1"; FAILED=1; }

assert_contains() {
    local hay="$1" needle="$2" label="$3"
    if grep -qF -- "$needle" <<<"$hay"; then pass "$label"
    else fail "$label (missing: $needle)"; echo "--- haystack ---"; echo "$hay"; fi
}

assert_absent() {
    local hay="$1" needle="$2" label="$3"
    if grep -qF -- "$needle" <<<"$hay"; then fail "$label (unexpected: $needle)"
    else pass "$label"; fi
}

assert_eq() {
    local got="$1" want="$2" label="$3"
    if [[ "$got" == "$want" ]]; then pass "$label (= $got)"
    else fail "$label (got '$got' want '$want')"; fi
}

# A marker exactly as periculum's `marker_contents` writes it
# (periculum/src/lock.rs). Args: path, verdict tag, suspect, holder pid, age.
write_marker() {
    local path="$1" verdict="$2" suspect="$3" pid="$4" age="$5"
    mkdir -p "$(dirname "$path")"
    cat > "$path" <<EOF
run=4242-1758500000000
contender_pid=4242
at=2026-09-22T03:37:00
at_epoch_ms=1758500000000
verdict=$verdict
suspect=$suspect
holder_run=$pid-1758400000000
holder_pid=$pid
holder_started=2026-09-21T03:37:00
holder_age_secs=$age
detail=pid $pid is alive and has held the lock for $age s
EOF
}

# ---------------------------------------------------------------------------
# The helper, on its own
# ---------------------------------------------------------------------------

echo "== The marker parse =="

# shellcheck source=scripts/lock-contention.sh
. "$SCRIPT_DIR/lock-contention.sh"

SANDBOX=$(mktemp -d)
M="$SANDBOX/lock-contention"

write_marker "$M" running false 1234 600
if lock_contention_take "$M"; then pass "a present marker is taken"
else fail "a present marker is taken"; fi
assert_eq "$LOCK_VERDICT" "running" "verdict parsed"
assert_eq "$LOCK_HOLDER_PID" "1234" "holder pid parsed"
assert_eq "$(lock_contention_token)" "lock-held" "a live recent holder is a plain overlap"
if lock_contention_is_suspect; then fail "running is not suspect"; else pass "running is not suspect"; fi
if [ -e "$M" ]; then fail "the marker is removed once read"; else pass "the marker is removed once read"; fi

write_marker "$M" suspected_wedge true 4321 90000
lock_contention_take "$M"
assert_eq "$(lock_contention_token)" "lock-suspect" "a wedge-shaped holder gets its own token"
if lock_contention_is_suspect; then pass "suspected_wedge is suspect"; else fail "suspected_wedge is suspect"; fi
assert_contains "$(lock_contention_fields)" "holder_age_secs=90000" "the age reaches the ledger"
assert_contains "$LOCK_DETAIL" "pid 4321" "the detail sentence survives the parse"

# The pre-#30 artefact: an empty file. It must still read as a contention,
# and as the benign kind — inventing a suspicion from an absent field would
# put a false wedge accusation in the ledger of every old marker.
: > "$M"
if lock_contention_take "$M"; then pass "an empty pre-#30 marker still counts as contention"
else fail "an empty pre-#30 marker still counts as contention"; fi
assert_eq "$(lock_contention_token)" "lock-held" "an unrecorded verdict is not an accusation"
assert_contains "$(lock_contention_fields)" "verdict=unrecorded" "the ledger says the verdict was absent"

if lock_contention_take "$SANDBOX/absent"; then fail "no marker is not a contention"
else pass "no marker is not a contention"; fi

rm -rf "$SANDBOX"

# ---------------------------------------------------------------------------
# The two runners that never see periculum's exit code
# ---------------------------------------------------------------------------

# Drive one runner in a sandbox HOME. $1 = script, $2.. = env assignments.
# Leaves $OUT (the run's stdout+stderr), $LEDGER (last-results.txt) and
# $NOTIFY (what notify-send was asked to say) set.
run_case() {
    local script="$1"; shift
    SANDBOX=$(mktemp -d)
    mkdir -p "$SANDBOX/bin"
    # notify-send is a real dependency of run-tier3.sh and must not reach a
    # real desktop from a test; the fixture records the call instead.
    cat > "$SANDBOX/bin/notify-send" <<EOF
#!/bin/bash
echo "notify-send \$*" >> "$SANDBOX/notify.log"
EOF
    chmod +x "$SANDBOX/bin/notify-send"
    OUT=$(env HOME="$SANDBOX" \
        PATH="$SANDBOX/bin:$PATH" \
        LEVICULUM_BRIDGE="$SANDBOX/bridge" \
        "$@" \
        bash "$SCRIPT_DIR/$script" --force 2>&1)
    LEDGER=$(cat "$SANDBOX/.local/state/leviculum-ci/last-results.txt" 2>/dev/null)
    NOTIFY=$(cat "$SANDBOX/notify.log" 2>/dev/null)
    rm -rf "$SANDBOX"
}

# A stub tier command: writes the marker periculum would have written, then
# fails the way `just` fails when the run underneath it was refused.
stub_contending() {
    local verdict="$1" suspect="$2" pid="$3" age="$4"
    cat <<EOF
m="\$HOME/.local/state/leviculum-ci/lock-contention"
mkdir -p "\$(dirname "\$m")"
cat > "\$m" <<MARKER
run=4242-1758500000000
contender_pid=4242
at=2026-09-22T03:37:00
at_epoch_ms=1758500000000
verdict=$verdict
suspect=$suspect
holder_run=$pid-1758400000000
holder_pid=$pid
holder_started=2026-09-21T03:37:00
holder_age_secs=$age
detail=pid $pid is alive but has held the lock for 25h -- SUSPECTED WEDGE
MARKER
echo "[leviculum] the rig lock is held"
exit 2
EOF
}

for runner in run-tier2.sh run-tier3.sh; do
    tier=tier2; [[ "$runner" == run-tier3.sh ]] && tier=tier3

    echo "== $runner: a legitimate overlap =="
    run_case "$runner" LEVICULUM_SELFTEST_TIER_CMD="$(stub_contending running false 1234 600)"
    assert_eq "$OUT" "" "the contention path stays silent (cron mails on output)"
    assert_contains "$LEDGER" "$tier SKIPPED lock-held" "an overlap keeps the historic token"
    assert_contains "$LEDGER" "holder_pid=1234" "the ledger names the holder"
    assert_absent "$LEDGER" "$tier RED" "an overlap is not a RED"

    echo "== $runner: a suspected wedge =="
    run_case "$runner" LEVICULUM_SELFTEST_TIER_CMD="$(stub_contending suspected_wedge true 4321 90000)"
    assert_contains "$LEDGER" "$tier SKIPPED lock-suspect" "a wedge is distinguishable in the ledger"
    assert_contains "$LEDGER" "verdict=suspected_wedge" "the verdict itself is recorded"
    assert_contains "$LEDGER" "holder_pid=4321" "the ledger names the suspected holder"
    assert_absent "$LEDGER" "$tier SKIPPED lock-held " "a wedge is not filed as a plain overlap"

    echo "== $runner: a real failure with no marker is still RED =="
    run_case "$runner" LEVICULUM_SELFTEST_TIER_CMD='echo "boom"; exit 1'
    assert_contains "$LEDGER" "$tier RED" "no marker, no skip"
    assert_absent "$LEDGER" "SKIPPED lock" "a failure is never reclassified as contention"
done

echo "== run-tier3.sh: the human is told which of the two it is =="
run_case run-tier3.sh LEVICULUM_SELFTEST_TIER_CMD="$(stub_contending running false 1234 600)"
assert_contains "$NOTIFY" "-u normal" "an overlap notifies at normal urgency"
run_case run-tier3.sh LEVICULUM_SELFTEST_TIER_CMD="$(stub_contending suspected_wedge true 4321 90000)"
assert_contains "$NOTIFY" "-u critical" "a suspected wedge is not a normal-urgency skip"
assert_contains "$NOTIFY" "SUSPECTED WEDGE" "the notification says what is suspected"

# ---------------------------------------------------------------------------
# The producer half
# ---------------------------------------------------------------------------
#
# Every case above feeds the parser a marker this file wrote. That proves the
# consumer and nothing about the contract: the bug being fixed here IS a
# producer that grew fields and a consumer that never read them, so a fixture
# agreeing with itself is exactly the failure mode to guard against.
#
# Producing a real marker needs a real contention -- a second radiating run,
# or a second docker-bench run -- which is a rig/docker job and not this
# script's. What can be checked without either is that every key this parser
# consumes is still a key periculum's `marker_contents` emits. Skipped, not
# failed, where the sibling checkout is absent: it is a sibling by convention,
# not a submodule.
echo "== The contract with periculum's writer =="
PERICULUM_LOCK="${PERICULUM_ROOT:-$SCRIPT_DIR/../../periculum}/periculum/src/lock.rs"
if [ -r "$PERICULUM_LOCK" ]; then
    for key in verdict suspect holder_pid holder_age_secs detail; do
        if grep -qE "writeln!\(out, \"$key=" "$PERICULUM_LOCK"; then
            pass "periculum still writes $key="
        else
            fail "periculum no longer writes $key= (this parser reads it)"
        fi
    done
else
    echo "  SKIP: no periculum checkout at $PERICULUM_LOCK"
fi

echo
if (( FAILED == 0 )); then
    echo "lock-contention selftest: ALL PASS"
    exit 0
else
    echo "lock-contention selftest: FAILURES"
    exit 1
fi
