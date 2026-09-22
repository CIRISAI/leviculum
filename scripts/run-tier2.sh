#!/bin/bash
# Tier-2 runner (on demand since 2026-06-12; the nightly stays scheduled).
#
# Skip-guards: a run silently no-ops if tier2 was already GREEN today or
# if there are no commits today. For an INTENTIONAL repeat run (e.g.
# re-validating after an environment change), bypass both guards with
# either:
#     bash scripts/run-tier2.sh --force
#     LEVICULUM_TIER2_FORCE=1 systemctl --user start leviculum-ci-tier2.service
# (for the systemd form, set the env var via
#  `systemctl --user set-environment LEVICULUM_TIER2_FORCE=1`, and
#  unset-environment afterwards).
FORCE=0
if [ "${1:-}" = "--force" ] || [ "${LEVICULUM_TIER2_FORCE:-0}" = "1" ]; then
    FORCE=1
fi
# Resolved before the cd below: every later "$(dirname "$0")" would be
# relative to the NEW working directory, which is only accidentally the same
# one when the script was started by an absolute path.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
LOG_DIR=~/.local/state/leviculum-ci
mkdir -p "$LOG_DIR"
# Per-execution log: timestamp + PID guarantees no overlap if two
# instances ever collide (CLAUDE.md: failure logs must always survive).
LOG="$LOG_DIR/tier2-$(date +%Y%m%d-%H%M%S)-$$.log"
RESULTS="$LOG_DIR/last-results.txt"

cd "$(dirname "$0")/.." || exit 1

# Repo-sync at head of every run when the install was --vm-mode
# (worktree-scoped marker inside .git/).  Brings this worktree to
# origin/master before any test work.  Skipped on developer-machine
# installs where the marker is absent.
if [ -f "$(git rev-parse --git-dir)/leviculum-ci-vm-mode-marker" ]; then
    bash "$SCRIPT_DIR/_repo-sync.sh"
    echo "$(date -Iseconds) tier2 sync HEAD=$(git rev-parse --short HEAD)" >> "$RESULTS"
fi

# Rotate logs
find "$LOG_DIR" -name 'tier*.log' -mtime +14 -delete 2>/dev/null || true

if [ "$FORCE" = "1" ]; then
    echo "$(date -Iseconds) tier2 FORCED (skip-guards bypassed)" >> "$RESULTS"
else
    # Skip if already ran successfully today
    if grep -q "$(date +%Y-%m-%d).*tier2 GREEN" "$RESULTS" 2>/dev/null; then
        exit 0
    fi

    # Skip if no commits today
    if [ -z "$(git log --since=midnight --oneline 2>/dev/null)" ]; then
        exit 0
    fi
fi

MARKER="$LOG_DIR/lock-contention"
# shellcheck source=scripts/lock-contention.sh
. "$SCRIPT_DIR/lock-contention.sh"

# Test seam (scripts/test-lock-contention.sh only): replace the tier command
# with a stub that writes a contention marker and fails, so the branch below
# can be driven without a two-hour corpus run. Empty in production.
TIER_CMD=( just extensive )
if [ -n "${LEVICULUM_SELFTEST_TIER_CMD:-}" ]; then
    TIER_CMD=( bash -c "$LEVICULUM_SELFTEST_TIER_CMD" )
fi

if CARGO_TARGET_DIR=~/.cache/leviculum-ci-target CARGO_INCREMENTAL=0 "${TIER_CMD[@]}" > "$LOG" 2>&1; then
    echo "$(date -Iseconds) tier2 GREEN $LOG" >> "$RESULTS"
elif lock_contention_take "$MARKER"; then
    # Another run held the scenario-runner lock when Tier 2 tried to
    # start. Not a failure — deferred. See periculum/src/lock.rs.
    # Lock-contention is NOT a RED — no bundle emit.
    #
    # Which KIND of contention comes out of the marker, not out of its mere
    # existence: a holder periculum could not call legitimate gets its own
    # ledger token, so the one case worth acting on is greppable.
    echo "[CI] lock contention: ${LOCK_DETAIL:-(no detail recorded)}" >> "$LOG"
    echo "$(date -Iseconds) tier2 SKIPPED $(lock_contention_token) $(lock_contention_fields) $LOG" >> "$RESULTS"
else
    echo "$(date -Iseconds) tier2 RED $LOG" >> "$RESULTS"
    bash "$SCRIPT_DIR/_emit-auto-bug-bundle.sh" tier2 "$LOG" || true
fi
