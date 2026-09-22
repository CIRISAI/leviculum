#!/bin/bash
# Resolved before the cd below; see run-tier2.sh.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
LOG_DIR=~/.local/state/leviculum-ci
mkdir -p "$LOG_DIR"
# Per-execution log: timestamp + PID guarantees no overlap if two
# instances ever collide (CLAUDE.md: failure logs must always survive).
LOG="$LOG_DIR/nightly-$(date +%Y%m%d-%H%M%S)-$$.log"
RESULTS="$LOG_DIR/last-results.txt"

cd "$(dirname "$0")/.." || exit 1

# Rotate logs (keep nightly logs longer: 60 days)
find "$LOG_DIR" -name 'nightly-*.log' -mtime +60 -delete 2>/dev/null || true

MARKER="$LOG_DIR/lock-contention"
# shellcheck source=scripts/lock-contention.sh
. "$SCRIPT_DIR/lock-contention.sh"

# Test seam (scripts/test-lock-contention.sh only): see run-tier2.sh.
TIER_CMD=( just nightly )
if [ -n "${LEVICULUM_SELFTEST_TIER_CMD:-}" ]; then
    TIER_CMD=( bash -c "$LEVICULUM_SELFTEST_TIER_CMD" )
fi

if CARGO_TARGET_DIR=~/.cache/leviculum-ci-target CARGO_INCREMENTAL=0 "${TIER_CMD[@]}" > "$LOG" 2>&1; then
    # Parse pass/skip counts for hardware-availability awareness.
    # Format may vary across cargo versions; adjust if parsing fails.
    PASSED=$(grep -oP 'test result: ok\. \K\d+' "$LOG" | awk '{s+=$1} END{print s}')
    SKIPPED=$(grep -oP 'test result: ok\..+?\K\d+(?= ignored)' "$LOG" | awk '{s+=$1} END{print s}')
    notify-send -u normal "Leviculum CI" "Nightly: GREEN (passed: ${PASSED:-?}, skipped: ${SKIPPED:-?})"
    echo "$(date -Iseconds) tier3 GREEN passed=${PASSED:-?} skipped=${SKIPPED:-?} $LOG" >> "$RESULTS"
elif lock_contention_take "$MARKER"; then
    # Another run held the scenario-runner lock when nightly tried to
    # start — e.g. a manual LoRa bench was running past 02:00.
    # Not a failure; deferred. See periculum/src/lock.rs.
    #
    # Unless the holder does not look legitimate. That verdict is the whole
    # point of the marker's contents, and this notification is where a human
    # meets it: a 24-hour holder has already starved a nightly, and saying
    # "another test held the lock" about it buries the one message worth
    # waking up to.
    if lock_contention_is_suspect; then
        notify-send -u critical "Leviculum CI" \
            "Nightly: SKIPPED — SUSPECTED WEDGE on the runner lock. ${LOCK_DETAIL:-no detail recorded}"
    else
        notify-send -u normal "Leviculum CI" \
            "Nightly: SKIPPED — another test held the lock ($(lock_contention_fields))"
    fi
    echo "$(date -Iseconds) tier3 SKIPPED $(lock_contention_token) $(lock_contention_fields) $LOG" >> "$RESULTS"
else
    notify-send -u critical "Leviculum CI" "Nightly: RED — see $LOG"
    echo "$(date -Iseconds) tier3 RED $LOG" >> "$RESULTS"
fi
