#!/bin/bash
# run-tier3-hw.sh — Tier-3 nightly runner with hub-power orchestration.
#
# Wraps the existing `just nightly` cargo invocation with per-profile
# USB-hub power switching via `ssh hamster...`.  Tests are
# grouped by their `profile = "..."` field in
# `reticulum-integ/tests/<fn-name>.toml`; tests without an explicit
# profile fall back to `default` (= all devices on).
#
# Usage:
#   bash scripts/run-tier3-hw.sh                    # full nightly (~2-6 h)
#   bash scripts/run-tier3-hw.sh --smoke <pattern>  # subset matching pattern
#
# The `--smoke <pattern>` form passes `<pattern>` as positional cargo-test
# filters; same per-group orchestration, smaller test set.  Used by Lew
# for ad-hoc verification and by acceptance check 4.
#
# Always-restore on EXIT (clean, fail, SIGINT alike) puts the helper back
# in the all-on default state via `ssh hamster restore-default`.
#
# Source-of-truth note: since the remove-event fix the VM DOES see USB
# disconnects after a hub-port flip and the `/dev/serial/by-id/...`
# symlinks come and go correctly. `ssh hamster status` remains the
# authoritative view of hub-port POWER state (the VM only sees
# enumeration), and is still emitted into the per-test log as
# `[CI_HW] hamster_status=...`.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
LOG_DIR=~/.local/state/leviculum-ci
DEVICES_TOML="$REPO_DIR/reticulum-integ/profiles/devices.toml"
TESTS_DIR="$REPO_DIR/reticulum-integ/tests"
RESULTS="$LOG_DIR/last-results.txt"
MARKER="$LOG_DIR/lock-contention"

mkdir -p "$LOG_DIR"

# Repo-sync at head of every run when the install was --vm-mode
# (worktree-scoped marker inside .git/).  Brings this worktree to
# origin/master before any test work.  Skipped on developer-machine
# installs where the marker is absent.  Runs from $REPO_DIR (already
# computed above) to ensure git rev-parse / fetch operate on the
# right worktree.
if [ -f "$(cd "$REPO_DIR" && git rev-parse --git-dir)/leviculum-ci-vm-mode-marker" ]; then
    (cd "$REPO_DIR" && bash "$REPO_DIR/scripts/_repo-sync.sh")
    echo "$(date -Iseconds) tier3-hw sync HEAD=$(cd "$REPO_DIR" && git rev-parse --short HEAD)" >> "$RESULTS"
fi

LOG="$LOG_DIR/nightly-hw-$(date +%Y%m%d-%H%M%S)-$$.log"
# Side-channel for structured SCENARIO_SKIPPED lines from the integ
# runner's device-count preflight (see require_runner! in executor.rs).
SKIP_LOG="$LOG_DIR/nightly-hw-skips-$$.log"
: > "$SKIP_LOG"

# Always log to both stdout (for interactive feedback) and the per-run
# log (for forensic).  exec the redirection so every subsequent command
# is captured automatically.
exec > >(tee -a "$LOG") 2>&1

log() { echo "$@"; }

# Restore hub state on any exit path so manual interventions or scheduled
# runs leave the rig in a predictable state for the next operator.
trap 'log "[CI_HW] EXIT trap: restoring default hub state"; ssh hamster restore-default || log "[CI_HW] WARN: restore-default failed"' EXIT

# --- Argument parsing ---

SMOKE_MODE=false
SMOKE_PATTERN=""
if [[ "${1:-}" == "--smoke" ]]; then
    SMOKE_MODE=true
    SMOKE_PATTERN="${2:-}"
    if [[ -z "$SMOKE_PATTERN" ]]; then
        log "ERROR: --smoke needs a test-filter pattern."
        log "Usage: $0 --smoke '<pattern> [<pattern> ...]'"
        exit 1
    fi
    log "[CI_HW] mode=smoke pattern='$SMOKE_PATTERN'"
else
    log "[CI_HW] mode=full-nightly"
fi

# --- Pre-step: hamster helper reachable? ---

if ! ssh hamster status >/dev/null 2>&1; then
    cat <<'EOF'
ERROR: ssh hamster status failed; cannot orchestrate hardware.

Either install the usbhub-helper on hamster
(scripts/install-usbhub-helper.sh; needs the authorized_keys entry and
the RSHTECH board registry — see the helper's --help) or fall back to
run-tier3.sh (no-hardware mode).

Aborting.
EOF
    exit 1
fi
log "[CI_HW] hamster helper reachable"

# --- Build the binaries the integ runner mounts into Docker ---
# Mirrors the `build-integ-bins` Just target.  `just nightly` would do
# this transitively, but we drive cargo directly per-group so no Just
# dependency chain runs.
log "[CI_HW] building integ binaries (lnsd / lns / lncp / lora-proxy)"
CARGO_TARGET_DIR=~/.cache/leviculum-ci-target CARGO_INCREMENTAL=0 \
  cargo build --release --bin lnsd --bin lns --bin lncp --bin lora-proxy

# --- TOML helpers (python3 + tomllib) ---

# Fetch a profile's `required` array as space-separated keys.
profile_required() {
    local profile="$1"
    python3 - "$DEVICES_TOML" "$profile" <<'PY'
import sys, tomllib
path, profile = sys.argv[1], sys.argv[2]
d = tomllib.load(open(path, 'rb'))
print(' '.join(d['profiles'][profile].get('required', [])))
PY
}

profile_exclude() {
    local profile="$1"
    python3 - "$DEVICES_TOML" "$profile" <<'PY'
import sys, tomllib
path, profile = sys.argv[1], sys.argv[2]
d = tomllib.load(open(path, 'rb'))
print(' '.join(d['profiles'][profile].get('exclude', [])))
PY
}

# Fetch a device's serial.  Empty string if device-key not in table.
device_serial() {
    local key="$1"
    python3 - "$DEVICES_TOML" "$key" <<'PY'
import sys, tomllib
path, key = sys.argv[1], sys.argv[2]
d = tomllib.load(open(path, 'rb'))
print(d['devices'].get(key, {}).get('serial', ''))
PY
}

# Resolve a test fn-name to its profile.  Order:
#   1. <fn-name>.toml exists and has top-level `profile` field → that.
#   2. <fn-name>.toml exists but no `profile` field → "default".
#   3. <fn-name>.toml does not exist → "default".
test_profile_for() {
    local fn="$1"
    local toml="$TESTS_DIR/$fn.toml"
    if [[ ! -f "$toml" ]]; then
        echo default
        return
    fi
    python3 - "$toml" <<'PY'
import sys, tomllib
d = tomllib.load(open(sys.argv[1], 'rb'))
print(d.get('profile', 'default'))
PY
}

# --- Test discovery ---

# Returns one test fn-name (last `::` segment) per line.  Smoke pattern
# forwarded as positional cargo filters.
discover_tests() {
    local cargo_args=( -p reticulum-integ -- --include-ignored --list )
    if $SMOKE_MODE; then
        # shellcheck disable=SC2206
        cargo_args+=( $SMOKE_PATTERN )
    fi
    cargo test "${cargo_args[@]}" 2>/dev/null \
      | awk -F': ' '$NF == "test" { print $1 }' \
      | awk -F'::' '{ print $NF }' \
      | sort -u
}

# --- Q6 informative warning: FD held against a device's by-id symlink ---

warn_if_fd_held() {
    local key="$1"
    local serial; serial=$(device_serial "$key")
    [[ -n "$serial" ]] || return 0
    local sym
    set +f
    for sym in /dev/serial/by-id/usb-*"${serial}"*; do
        [[ -e "$sym" ]] || continue
        local holders
        holders=$(lsof -t "$sym" 2>/dev/null || true)
        if [[ -n "$holders" ]]; then
            log "[CI_HW] WARN: $key ($(basename "$sym")) held by PID(s) ${holders//$'\n'/,}; proceeding per Tier-3 policy"
        fi
    done
}

# --- Per-profile setup ---

setup_profile() {
    local profile="$1"
    local required exclude
    required=$(profile_required "$profile")
    exclude=$(profile_exclude "$profile")
    log "[CI_HW] ----- profile=$profile required=$required excluded=$exclude -----"
    # Q6 warnings for any process still holding a stale FD against an
    # excluded device.  Tier-3 wins (proceed regardless), per Q6 policy.
    local key
    for key in $exclude; do
        warn_if_fd_held "$key"
    done
    # Helper API: enable-only takes one comma-separated list.
    local active_csv
    active_csv=$(echo "$required" | tr ' ' ',')
    if [[ -n "$active_csv" ]]; then
        # shellcheck disable=SC2029  # local expansion of $active_csv is intentional
        ssh hamster enable-only "$active_csv"
    fi
    # Authoritative state from hamster — the VM-side enumeration is
    # not trustworthy after a libvirt-cached disable.
    local actual
    actual=$(ssh hamster status)
    log "[CI_HW] hamster_status=$actual"
}

# --- Per-group cargo invocation ---

run_group() {
    local profile="$1"
    shift
    local fns=( "$@" )
    setup_profile "$profile"
    log "[CI_HW] running ${#fns[@]} tests for profile=$profile: ${fns[*]}"
    # All tests of this profile in one cargo invocation: cheaper than
    # spawning a fresh test binary per test.  --test-threads=1 keeps
    # the integ-lock contract intact.  LEVICULUM_SKIP_LOG collects
    # structured SCENARIO_SKIPPED lines (device-count preflight) —
    # libtest swallows captured output of green tests, so the in-test
    # eprintln alone would be invisible here.
    CARGO_TARGET_DIR=~/.cache/leviculum-ci-target CARGO_INCREMENTAL=0 \
      LEVICULUM_SKIP_LOG="$SKIP_LOG" \
      cargo test -p reticulum-integ -- --include-ignored --test-threads=1 "${fns[@]}"
}

# --- Discover, group, run ---

mapfile -t ALL_FNS < <(discover_tests)
if [[ ${#ALL_FNS[@]} -eq 0 ]]; then
    log "[CI_HW] no tests matched; nothing to do"
    exit 0
fi
log "[CI_HW] discovered ${#ALL_FNS[@]} tests"

# Bucket fn-names by their resolved profile.  Sorting by profile name
# minimises hub-power cycling between groups (each transition costs
# ~2-3 s).  Within a profile the cargo --test-threads=1 order is
# deterministic.
declare -A PROFILE_BUCKETS=()
for fn in "${ALL_FNS[@]}"; do
    profile=$(test_profile_for "$fn")
    PROFILE_BUCKETS[$profile]+="$fn "
    log "[CI_HW] resolve $fn → profile=$profile"
done

# Process buckets in sorted profile order for predictability.
RC=0
for profile in $(printf '%s\n' "${!PROFILE_BUCKETS[@]}" | sort); do
    # shellcheck disable=SC2206
    fns=( ${PROFILE_BUCKETS[$profile]} )
    if ! run_group "$profile" "${fns[@]}"; then
        RC=1
        if [[ -f "$MARKER" ]]; then
            # Lock-contention path mirrors run-tier3.sh: another cargo
            # held the integ lock when this fired.  Treat as SKIPPED,
            # not RED.  Marker file decouples skip-vs-fail from log
            # text grepping.
            rm -f "$MARKER"
            log "[CI_HW] SKIPPED — another cargo-test held the integ lock"
            echo "$(date -Iseconds) tier3 SKIPPED lock-held $LOG" >> "$RESULTS"
            exit 0
        fi
        log "[CI_HW] profile=$profile RED — see $LOG"
    fi
done

# Device-count skips are neither green nor red — surface them loudly
# in the summary and the results line. A non-empty skip list with the
# 4th RNode back in the rig means a profile/registration gap, not noise.
SKIPPED=0
if [[ -s "$SKIP_LOG" ]]; then
    SKIPPED=$(grep -c '^SCENARIO_SKIPPED' "$SKIP_LOG" || true)
    log "[CI_HW] $SKIPPED scenario(s) skipped on device-count preflight:"
    while IFS= read -r line; do log "[CI_HW]   $line"; done < "$SKIP_LOG"
fi

if [[ $RC -eq 0 ]]; then
    log "[CI_HW] tier3 GREEN (skipped=$SKIPPED)"
    echo "$(date -Iseconds) tier3 GREEN skipped=$SKIPPED $LOG" >> "$RESULTS"
else
    log "[CI_HW] tier3 RED (skipped=$SKIPPED)"
    echo "$(date -Iseconds) tier3 RED skipped=$SKIPPED $LOG" >> "$RESULTS"
    # One bundle per tier-3 run, not per failing profile group;
    # per-profile RED log lines remain informational only.
    bash "$REPO_DIR/scripts/_emit-auto-bug-bundle.sh" tier3-hw "$LOG" || true
fi
exit $RC
