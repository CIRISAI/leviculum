#!/bin/bash
# run-tier3-hw.sh — Tier-3 nightly runner, all boards always powered.
#
# Wraps the per-profile cargo invocation. Tests are grouped by their
# `profile = "..."` field in `reticulum-integ/tests/<fn-name>.toml`;
# tests without an explicit profile fall back to `default`.
#
# Usage:
#   bash scripts/run-tier3-hw.sh                    # full nightly (~2-6 h)
#   bash scripts/run-tier3-hw.sh --smoke <pattern>  # subset matching pattern
#
# The `--smoke <pattern>` form passes `<pattern>` as positional cargo-test
# filters; same per-group handling, smaller test set. Used by Lew for
# ad-hoc verification and by acceptance check 4.
#
# NO USB-hub power switching. Every attached board stays powered on and
# passed through to the VM for the entire run. RF isolation of the
# non-participating LNodes is done by the Rust test itself, which serially
# pushes `radio_silent` to every discovered LNode the scenario did not
# bind (silence_unused_lnode in runner.rs). This replaces the old
# per-profile `uhubctl`/usbhub-helper power cycling, which correlated with
# hamster hardware-watchdog freezes (proven 2026-06-15) and is gone for
# good. The profile `required`/`exclude` lists now only document which
# boards participate vs. get silenced; they no longer touch power.

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

# No EXIT trap to restore hub state: with all boards permanently powered
# on there is nothing to switch back, so no restore is needed.

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

# No hub-helper reachability gate: the run no longer orchestrates USB
# power, so the usbhub-helper is not required. The only remaining use of
# `ssh hamster` is the best-effort flash-failure desktop notification,
# which already tolerates an unreachable host.

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

# True if a single USB ID is currently enumerated.
lnode_present() {
    lsusb -d "$1" >/dev/null 2>&1
}

# True only when every USB ID passed as an argument is currently enumerated.
ids_enumerated() {
    local id
    for id in "$@"; do
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

# Resolve an LNode's debug ttyACM via udev properties. Each LNode exposes
# two CDC-ACM interfaces; interface 00 is the ASCII text debug console
# (interface 02 is the binary HDLC data link). Match ID_VENDOR_ID=1209, the
# board's product id and ID_USB_INTERFACE_NUM=00. The /dev/leviculum-*-debug by-serial
# symlinks an earlier version assumed do NOT exist on the rig, and serials
# are volatile, so we resolve dynamically rather than hardcode either.
# Echoes the matching /dev/ttyACM* on success; returns 1 if none found.
resolve_lnode_debug_port() {
    local pid="$1"   # 0001 (t114) | 0002 (rak4631 / Pocket-V2)
    local dev props
    set +f
    for dev in /dev/ttyACM*; do
        [[ -e "$dev" ]] || continue
        props=$(udevadm info -q property -n "$dev" 2>/dev/null) || continue
        grep -q '^ID_VENDOR_ID=1209$'       <<<"$props" || continue
        grep -q "^ID_MODEL_ID=${pid}\$"     <<<"$props" || continue
        grep -q '^ID_USB_INTERFACE_NUM=00$' <<<"$props" || continue
        echo "$dev"
        return 0
    done
    return 1
}

# Read the periodic firmware [FW_BUILD] banner from a CDC-ACM debug port,
# returning the last such line seen within <secs> (empty if none). DTR+RTS
# are asserted on open because CDC-ACM transmits only with DTR raised.
# Pure stdlib (termios/fcntl) so no pyserial install is required on the rig.
read_fw_build_banner() {
    local port="$1" secs="$2"
    python3 - "$port" "$secs" <<'PY'
import sys, os, time, fcntl, termios, struct, select
port, secs = sys.argv[1], float(sys.argv[2])
try:
    fd = os.open(port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
except OSError:
    sys.exit(0)
try:
    iflag, oflag, cflag, lflag, ispeed, ospeed, cc = termios.tcgetattr(fd)
    iflag = oflag = lflag = 0
    cflag = termios.CLOCAL | termios.CREAD | termios.CS8
    ispeed = ospeed = termios.B115200
    termios.tcsetattr(fd, termios.TCSANOW,
                      [iflag, oflag, cflag, lflag, ispeed, ospeed, cc])
    dtr = getattr(termios, 'TIOCM_DTR', 0x002)
    rts = getattr(termios, 'TIOCM_RTS', 0x004)
    fcntl.ioctl(fd, termios.TIOCMBIS, struct.pack('I', dtr | rts))
    deadline = time.monotonic() + secs
    buf, last = b'', ''
    while time.monotonic() < deadline:
        r, _, _ = select.select([fd], [], [], deadline - time.monotonic())
        if not r:
            continue
        try:
            chunk = os.read(fd, 4096)
        except OSError:
            break
        if not chunk:
            continue
        buf += chunk
        while b'\n' in buf:
            line, buf = buf.split(b'\n', 1)
            text = line.decode('utf-8', 'replace').replace('\r', '').strip()
            if 'FW_BUILD' in text:
                last = text
    print(last)
finally:
    os.close(fd)
PY
}

# Read the firmware [FW_BUILD] banner back over the debug serial and check
# its git_sha against the expected HEAD sha. Defense-in-depth: a silent
# touch-flash that did not actually take (board re-enumerated but old
# firmware still resident) is caught here. Non-fatal — WARN only.
verify_lnode_banner() {
    local board="$1" expect_sha="$2"
    local pid
    case "$board" in
        t114)    pid=0001 ;;
        rak4631) pid=0002 ;;
        *)       return 0 ;;
    esac
    local port
    if ! port=$(resolve_lnode_debug_port "$pid"); then
        log "[CI_HW] WARN: $board debug serial (VID 1209 PID $pid intf 00) not found; cannot verify firmware sha"
        return 0
    fi
    log "[CI_HW] $board debug serial resolved to $port"
    # The firmware re-emits the banner every ~5 s, so an 8 s window always
    # catches at least one even though the boot-time banner is long gone.
    local banner
    banner=$(read_fw_build_banner "$port" 8)
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

    # Flash only the boards currently enumerated. A board physically removed
    # from the rig (Pocket-V2 unplugged → 1209:0002 absent) must NOT stall
    # the flash waiting for a UF2 drive that will never appear; skip it.
    #
    # UF2_TIMEOUT=120 (vs the uf2-runner.sh default of 30) on the VM flash
    # path: the libvirt USB-attach chain (1200-baud touch → nRF52 UF2
    # bootloader → udev → virsh attach → VM enumeration → /dev/sda) has
    # highly variable latency (~6 s typical, >30 s on a stalling run). The
    # desktop-flash default on hamster keeps 30.
    #
    # T114 fleet, then Pocket-V2 fleet. Each just-target builds the embedded
    # firmware (cargo run) and touch-flashes every attached board of that
    # kind. A failing fleet warns + notifies but does not abort.
    local flashed_ids=()

    if lnode_present 1209:0001; then
        if ( cd "$REPO_DIR" && UF2_TIMEOUT=120 just flash ); then
            log "[CI_HW] T114 flash invocation completed"
        else
            log "[CI_HW] WARN: LNode flash failed (t114)"
            notify_flash_failed t114
        fi
        flashed_ids+=( "1209:0001" )
    else
        log "[CI_HW] t114 (1209:0001) not enumerated; skipping flash"
    fi

    if lnode_present 1209:0002; then
        if ( cd "$REPO_DIR" && UF2_TIMEOUT=120 just flash-rak4631 ); then
            log "[CI_HW] Pocket-V2 flash invocation completed"
        else
            log "[CI_HW] WARN: LNode flash failed (rak4631)"
            notify_flash_failed rak4631
        fi
        flashed_ids+=( "1209:0002" )
    else
        log "[CI_HW] rak4631/Pocket-V2 (1209:0002) not enumerated; skipping flash"
    fi

    if (( ${#flashed_ids[@]} == 0 )); then
        log "[CI_HW] WARN: no LNodes enumerated; nothing flashed"
        return 0
    fi

    # Settle: LNodes re-enumerate as fresh ttyACM after the touch reset.
    # Bounded poll until the boards we actually flashed are back (or 30 s
    # timeout) so the first scenario does not open a half-enumerated port.
    log "[CI_HW] waiting for LNodes to re-enumerate (USB IDs ${flashed_ids[*]})"
    local waited=0
    while ! ids_enumerated "${flashed_ids[@]}"; do
        if (( waited >= 30 )); then
            log "[CI_HW] WARN: LNodes not fully re-enumerated after ${waited}s; proceeding (profiles gate on device-count)"
            break
        fi
        sleep 2
        waited=$(( waited + 2 ))
    done
    if ids_enumerated "${flashed_ids[@]}"; then
        log "[CI_HW] LNodes re-enumerated after ${waited}s"
        # Give udev a beat to settle the fresh ttyACM nodes + properties
        # before resolve_lnode_debug_port iterates them.
        sleep 2
        local id
        for id in "${flashed_ids[@]}"; do
            case "$id" in
                1209:0001) verify_lnode_banner t114    "$head_sha" ;;
                1209:0002) verify_lnode_banner rak4631 "$head_sha" ;;
            esac
        done
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
    # `required` = boards that participate (active); `exclude` = boards to
    # be silenced. No power is switched here: all boards stay on and the
    # Rust test silences every discovered LNode it does not bind
    # (silence_unused_lnode). This log line is the forensic record of
    # which boards the profile expects active vs. silenced.
    log "[CI_HW] ----- profile=$profile active=$required silenced=$exclude -----"
    # Q6 warnings for any process still holding a stale FD against a
    # to-be-silenced device.  Tier-3 wins (proceed regardless), per Q6
    # policy.
    local key
    for key in $exclude; do
        warn_if_fd_held "$key"
    done
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

# Bucket fn-names by their resolved profile.  Grouping by profile keeps
# the per-profile setup/log lines together and runs each profile's tests
# in one cargo invocation.  Within a profile the cargo --test-threads=1
# order is deterministic.
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
