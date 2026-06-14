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
#
# Fresh-binary guarantee (2026-06-13 nightly: 12 setup aborts).
# cargo decides "up to date" by source mtime. After _repo-sync.sh pulls
# newer commits whose checked-out files keep their old mtimes, cargo can
# skip the relink ("Finished in 0.07s") so the binary mtime stays old —
# while `check_binary_freshness` (paths.rs) compares that mtime against
# the git COMMIT time of the last production-source change. The two
# truths diverge and every binary-mounting test aborts in setup.
#
# Robustness: TOUCH the bin-crate sources so cargo recompiles the final
# crates and relinks, stamping a fresh mtime on every binary. (Deleting
# only the top-level binary does NOT work: cargo re-hardlinks it from
# target/release/deps without relinking, preserving the old mtime —
# verified 2026-06-13.) Touching every .rs under the two bin crates is
# robust regardless of which file is a given bin's entry point. Then
# PRE-FLIGHT with the EXACT same check the tests run
# (`reticulum-integ check-freshness`, one source of truth) and abort the
# whole run on a single clear failure rather than letting N scenarios
# die one by one.
CACHE_TARGET=~/.cache/leviculum-ci-target
log "[CI_HW] building integ binaries (lnsd / lns / lncp / lora-proxy)"
RELEASE_DIR="$CACHE_TARGET/x86_64-unknown-linux-musl/release"
find "$REPO_DIR/reticulum-cli/src" "$REPO_DIR/reticulum-proxy/src" \
  -name '*.rs' -exec touch {} +
CARGO_TARGET_DIR="$CACHE_TARGET" CARGO_INCREMENTAL=0 \
  cargo build --release --bin lnsd --bin lns --bin lncp --bin lora-proxy

# Build the preflight checker itself, then run it. It resolves the same
# binaries via the same paths:: code TestRunner::new uses, so it cannot
# drift from the per-test assertion.
CARGO_TARGET_DIR="$CACHE_TARGET" CARGO_INCREMENTAL=0 \
  cargo build --release --bin reticulum-integ -p reticulum-integ
if ! CARGO_TARGET_DIR="$CACHE_TARGET" "$RELEASE_DIR/reticulum-integ" check-freshness; then
    log "[CI_HW] FATAL: integ binaries still stale after forced rebuild — aborting run"
    echo "$(date -Iseconds) tier3 RED stale-binaries-preflight $LOG" >> "$RESULTS"
    exit 1
fi

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

# --- LNode auto-flash (Teil A) ---
#
# Flash the attached LNodes (T114 + Pocket-V2) from the current tree
# BEFORE any LoRa scenario runs, so the host code under test runs against
# firmware built from the SAME commit. Without this we would test current
# host code against whatever stale firmware happens to be on the boards,
# which makes every LNode result meaningless.
#
# The flash mechanic already exists (just flash / just flash-rak4631);
# both use the 1200-baud touch reset in reticulum-nrf/src/usb.rs, so no
# manual button press is needed on a board already running our firmware.
#
# Non-fatal by contract: a board that is not touch-flashable (stuck in
# stock app firmware with no touch handler, USB timeout, no bootloader)
# must NOT abort the whole run. We WARN, fire a desktop notification
# asking for the physical RESET double-tap, and continue; the LNode
# profiles then skip visibly via the device-count preflight.

LNODE_USB_IDS=( "1209:0001" "1209:0002" )   # T114, Pocket-V2

# True only when every expected LNode USB ID is currently enumerated.
lnodes_enumerated() {
    local id
    for id in "${LNODE_USB_IDS[@]}"; do
        lsusb -d "$id" >/dev/null 2>&1 || return 1
    done
    return 0
}

# Desktop notification for a flash failure that needs a manual RESET
# double-tap. Routed through hamster because the rig's display lives
# there (same channel run-tier3.sh notifies on). Best-effort.
notify_flash_failed() {
    local board="$1"
    ssh hamster "notify-send -u critical 'Leviculum CI' 'LNode flash FAILED ($board): physical RESET double-tap needed'" \
        2>/dev/null || log "[CI_HW] WARN: notify-send for $board flash failure did not reach hamster"
}

# Read the firmware [FW_BUILD] banner back over the debug serial and check
# its git_sha against the expected HEAD sha. Defense-in-depth: a silent
# touch-flash that did not actually take (board re-enumerated but old
# firmware still resident) is caught here. Non-fatal — WARN only.
verify_lnode_banner() {
    local board="$1" expect_sha="$2"
    local dev_key sym_prefix
    case "$board" in
        t114)    dev_key=t114;      sym_prefix=/dev/leviculum-debug ;;
        rak4631) dev_key=pocket-v2; sym_prefix=/dev/leviculum-rak-debug ;;
        *)       return 0 ;;
    esac
    local serial; serial=$(device_serial "$dev_key")
    local port="$sym_prefix"
    [[ -n "$serial" && -e "${sym_prefix}-${serial}" ]] && port="${sym_prefix}-${serial}"
    if [[ ! -e "$port" ]]; then
        log "[CI_HW] WARN: $board debug serial ($port) absent; cannot verify firmware sha"
        return 0
    fi
    # The banner is logged via log_critical! at boot and held in the
    # firmware ring buffer; opening the port asserts DTR and drains it.
    # Read a short window and take the most recent [FW_BUILD] line.
    local banner
    banner=$(timeout 8 cat "$port" 2>/dev/null | tr -d '\r' | grep -a 'FW_BUILD' | tail -1 || true)
    if [[ -z "$banner" ]]; then
        log "[CI_HW] WARN: $board no [FW_BUILD] banner seen on $port within window"
        return 0
    fi
    log "[CI_HW] $board banner: $banner"
    if [[ "$banner" == *"git_sha=$expect_sha"* ]]; then
        log "[CI_HW] $board firmware sha matches HEAD ($expect_sha)"
    else
        log "[CI_HW] WARN: LNode firmware sha mismatch ($board): expected $expect_sha, banner='$banner'"
    fi
}

flash_lnodes() {
    local head_sha; head_sha=$(cd "$REPO_DIR" && git rev-parse --short HEAD)
    log "[CI_HW] flashing LNodes from HEAD $head_sha"

    # T114 fleet, then Pocket-V2 fleet. Each just-target builds the
    # embedded firmware (cargo run) and touch-flashes every attached board
    # of that kind. A failing fleet warns + notifies but does not abort.
    if ( cd "$REPO_DIR" && just flash ); then
        log "[CI_HW] T114 flash invocation completed"
    else
        log "[CI_HW] WARN: LNode flash failed (t114)"
        notify_flash_failed t114
    fi
    if ( cd "$REPO_DIR" && just flash-rak4631 ); then
        log "[CI_HW] Pocket-V2 flash invocation completed"
    else
        log "[CI_HW] WARN: LNode flash failed (rak4631)"
        notify_flash_failed rak4631
    fi

    # Settle: LNodes re-enumerate as fresh ttyACM after the touch reset.
    # Bounded poll until both expected USB IDs are back (or 30 s timeout)
    # so the first scenario does not open a half-enumerated port.
    log "[CI_HW] waiting for LNodes to re-enumerate (USB IDs ${LNODE_USB_IDS[*]})"
    local waited=0
    while ! lnodes_enumerated; do
        if (( waited >= 30 )); then
            log "[CI_HW] WARN: LNodes not fully re-enumerated after ${waited}s; proceeding (profiles gate on device-count)"
            break
        fi
        sleep 2
        waited=$(( waited + 2 ))
    done
    if lnodes_enumerated; then
        log "[CI_HW] LNodes re-enumerated after ${waited}s"
        # Give udev a beat to (re)create the by-serial debug symlinks.
        sleep 2
        verify_lnode_banner t114    "$head_sha"
        verify_lnode_banner rak4631 "$head_sha"
    fi
    return 0
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
    # Helper API: `enable-only <b>...` takes SPACE-SEPARATED board args,
    # one per board. The old comma-joined single arg
    # ("t-beam-1,t-beam-2") was rejected as an unknown board, so power
    # isolation silently no-op'd on the 2026-06-13 nightly (fail-safe:
    # all boards stayed on, but the intended RF isolation never happened).
    # $required is intentionally unquoted so each board becomes its own
    # ssh argument.
    if [[ -n "$required" ]]; then
        # shellcheck disable=SC2029,SC2086  # word-split $required: one arg per board
        ssh hamster enable-only $required
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

# --- Flash LNodes from HEAD, then discover, group, run ---

# Additive step: bring the attached LNodes to the tested commit before any
# profile group runs. Smoke runs flash too — a smoke pass against stale
# firmware is just as misleading as a full one.
flash_lnodes

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
