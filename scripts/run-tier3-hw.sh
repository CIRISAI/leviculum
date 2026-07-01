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
#
# DEVICE-VANISH HONESTY. The rig boards are passed through to this VM via VFIO
# controller passthrough: the guest owns the xHCI host controller natively, so
# the host cannot inject a phantom VM-side USB disconnect. The old qemu usb-host
# passthrough could drop a board VM-side under load as a pure infrastructure
# artefact; that class is now impossible. A board that vanishes mid-run is
# therefore ALWAYS a real device/firmware failure (a self-reset under sustained
# load, suspected heap exhaustion, Codeberg 65), not an infra glitch, and must
# read as RED.
#
# So each profile group runs under a device watchdog. If any rig board vanishes
# during the window the group's results are UNTRUSTED; the group is re-attached
# and retried ONCE only to rule out a one-frame sampling blip. A confirmed
# vanish makes the run RED with the vanished board(s) named in the verdict line,
# whether it PERSISTS across the retry or RECOVERS on it: post-VFIO a vanish is
# always a real firmware self-reset, and recovery on the retry does not make it
# acceptable. The retry result is a diagnostic note only. There is no
# INFRA_INVALID class any more: a vanish is never absorbed.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
LOG_DIR=~/.local/state/leviculum-ci
DEVICES_TOML="$REPO_DIR/reticulum-integ/profiles/devices.toml"
TESTS_DIR="$REPO_DIR/reticulum-integ/tests"
RESULTS="$LOG_DIR/last-results.txt"
MARKER="$LOG_DIR/lock-contention"

mkdir -p "$LOG_DIR"

# --- Expected-marginal carve-out (Bug 1 disposition, decision A) ---
#
# SF10/CR8 mixed-chip SX1262->SX127x is a characterised chip-interop margin at
# the longest packet (frequency refuted by direct FEI ~275Hz/0.32ppm; the
# residual is a TX-modulation/spectral interop effect). SF10/CR5 is the robust
# mixed-chip limit and same-chip pairs pass SF10/CR8; both stay gating.
#
# All four SF10/CR8 mixed-chip slow benches are listed: the two `_ca` variants
# (bench_single_pair_slow_ca / bench_dual_pair_slow_ca) and their non-`_ca`
# siblings (bench_single_pair_slow / bench_dual_pair_slow), which run the same
# SF10/CR8 over the same mixed profiles (rnode_lnode_pair / dual_pair_mixed) and
# share the deafness. The same-chip rnode_only SF10/CR8 bench is NOT listed and
# stays gating.
#
# These four benches KEEP RUNNING and stay visible, but a genuine FAILURE of a
# listed test does NOT flip the tier3 verdict to RED: it is reported separately
# and counted in the verdict line (expected_marginal=N), so GREEN is never
# silently carved out. An unexpected PASS is logged loudly so a recovery is
# noticed. This list is the single source of truth for the mechanism; the
# documentary comments on the test fns and scenarios (reticulum-integ/src/
# executor.rs, reticulum-integ/tests/bench_*_slow*.toml) reference it.
#
# A SECOND, distinct mechanism is also carved out: lora_link_python. This is a
# python-relay to python-relay LoRa link test (both nodes type = "python"; the
# only rust part is the `lnstest selftest` driver, whose Client A/B are the link
# endpoints). The python daemon does not recover a single lost LinkRequest /
# LRPROOF packet in-window over slow LoRa (SF7, 2.73 kbps); our rust transport
# relays it at 100%. Measured same driver/radio/hardware, only the relaying
# stack differs (2026-06-25, rnode_pair): lora_link_rust green, lora_link_interop
# 5/5 green, lora_link_python 2/5 green (60% red). This is a Python-RNS reference
# characteristic, not a libreticulum bug, and we do not patch vendor. Its
# scenario is reticulum-integ/tests/lora_link_python.toml and its test fn is in
# reticulum-integ/src/executor.rs.
EXPECTED_MARGINAL=(
    bench_single_pair_slow_ca bench_dual_pair_slow_ca
    bench_single_pair_slow bench_dual_pair_slow
    lora_link_python
)

is_expected_marginal() {
    local needle="$1" m
    for m in "${EXPECTED_MARGINAL[@]}"; do
        [[ "$needle" == "$m" ]] && return 0
    done
    return 1
}

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
# The whole build + freshness preflight is skipped in selftest mode
# (LEVICULUM_SELFTEST=1), which exercises only the watchdog/retry/verdict
# logic with a stubbed cargo and needs no binaries or rig (see the
# simulated-vanish hook and the dry traces in the task).
CACHE_TARGET=~/.cache/leviculum-ci-target
if [[ -z "${LEVICULUM_SELFTEST:-}" ]]; then
log "[CI_HW] building integ binaries (lnsd / lnstest / lncp / lora-proxy)"
RELEASE_DIR="$CACHE_TARGET/x86_64-unknown-linux-musl/release"
find "$REPO_DIR/leviculum-cli/src" "$REPO_DIR/leviculum-proxy/src" \
  -name '*.rs' -exec touch {} +
CARGO_TARGET_DIR="$CACHE_TARGET" CARGO_INCREMENTAL=0 \
  cargo build --release --bin lnsd --bin lnstest --bin lncp --bin lora-proxy

# c_api_restart_recovery is non-ignored, so `cargo test --include-ignored`
# runs it; it resolves c-lnsd via paths::release_bin. Build it here too.
log "[CI_HW] building c-lnsd"
CARGO_TARGET_DIR="$CACHE_TARGET" CARGO_INCREMENTAL=0 \
  just build-c-lnsd

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

# Fetch the USB serials of a profile's required LNode boards
# (required device-keys whose firmware="lnode"), space-separated. Empty
# when the profile needs no specific LNode. Drives the runner's
# identity-stable LNode binding via LEVICULUM_REQUIRED_LNODE_SERIALS.
profile_required_lnode_serials() {
    local profile="$1"
    python3 - "$DEVICES_TOML" "$profile" <<'PY'
import sys, tomllib
path, profile = sys.argv[1], sys.argv[2]
d = tomllib.load(open(path, 'rb'))
devices = d.get('devices', {})
required = d['profiles'][profile].get('required', [])
serials = []
for key in required:
    dev = devices.get(key, {})
    if dev.get('firmware') == 'lnode':
        serial = dev.get('serial', '')
        if serial:
            serials.append(serial)
print(' '.join(serials))
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
    # Selftest seam: bucket every supplied fn under one profile so the
    # dry traces drive a single group deterministically.
    if [[ -n "${LEVICULUM_SELFTEST:-}" ]]; then
        echo "${LEVICULUM_SELFTEST_PROFILE:-default}"
        return
    fi
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
# both use the 1200-baud touch reset in leviculum-nrf/src/usb.rs, so no
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
    # Selftest seam: the dry-trace verification supplies the fn-names
    # directly so no rig and no `cargo test --list` is needed.
    if [[ -n "${LEVICULUM_SELFTEST_FNS:-}" ]]; then
        # shellcheck disable=SC2086
        printf '%s\n' $LEVICULUM_SELFTEST_FNS | sort -u
        return
    fi
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

# --- Device-vanish watchdog (Garantie B) ---
#
# The four distinct USB IDs of the five rig boards. The two T-Beams share
# 1a86:55d4, so that ID's baseline count is 2 and a single T-Beam vanish drops
# it to 1. Every board, including ones the active scenario silenced, counts:
# a silenced board that vanishes and returns un-silenced can still interfere
# with the running test, so ANY rig-board disconnect poisons the group.
RIG_USB_IDS=( "1a86:55d4" "1209:0001" "1209:0002" "303a:1001" )

# Number of currently enumerated USB devices for one vid:pid.
rig_id_count() { lsusb -d "$1" 2>/dev/null | wc -l | tr -d ' '; }

WATCHDOG_PID=""

# Start a background watchdog for one group's execution window. It snapshots a
# per-vid:pid baseline count, then polls once a second; the first time any ID's
# count drops below its baseline it appends one line to $poison (latched per ID
# so a long outage does not spam). Non-empty $poison after the window == a
# rig-board vanished during the run. Pure lsusb poll: no root, no dmesg
# privilege, robust to ttyACM renumbering (keyed by device identity, not node).
start_device_watchdog() {
    local poison="$1" stop="$2"
    rm -f "$stop"
    : > "$poison"
    (
        set +e
        declare -A base reported
        local id
        for id in "${RIG_USB_IDS[@]}"; do
            base[$id]=$(rig_id_count "$id")
            reported[$id]=0
        done
        while [[ ! -e "$stop" ]]; do
            for id in "${RIG_USB_IDS[@]}"; do
                local cur
                cur=$(rig_id_count "$id")
                if (( cur < ${base[$id]} )) && (( reported[$id] == 0 )); then
                    echo "vanish vid_pid=$id baseline=${base[$id]} now=$cur" >> "$poison"
                    reported[$id]=1
                fi
            done
            sleep 1
        done
        exit 0
    ) &
    WATCHDOG_PID=$!
}

# Stop the watchdog (create the stop sentinel, reap the process).
stop_device_watchdog() {
    local stop="$1"
    : > "$stop"
    if [[ -n "$WATCHDOG_PID" ]]; then
        wait "$WATCHDOG_PID" 2>/dev/null || true
    fi
    WATCHDOG_PID=""
    rm -f "$stop"
}

# Per-vid:pid count of a profile's required boards ("vid:pid count" lines).
profile_required_vidpid_counts() {
    local profile="$1"
    python3 - "$DEVICES_TOML" "$profile" <<'PY'
import sys, tomllib
from collections import Counter
path, profile = sys.argv[1], sys.argv[2]
d = tomllib.load(open(path, 'rb'))
devices = d.get('devices', {})
req = d['profiles'].get(profile, {}).get('required', [])
c = Counter()
for k in req:
    vp = devices.get(k, {}).get('vid_pid', '')
    if vp:
        c[vp] += 1
for vp, n in sorted(c.items()):
    print(f"{vp} {n}")
PY
}

# After a vanish, wait for the profile's required boards to re-enumerate
# (udev lora-vm-attach restores them) before retrying the group. Bounded and
# best-effort: on timeout we retry anyway and the runner's device-count
# preflight skips cleanly if a board is still absent. LEVICULUM_SETTLE_TIMEOUT
# overrides the wait (used short by the dry traces).
settle_rig() {
    local profile="$1"
    local timeout="${LEVICULUM_SETTLE_TIMEOUT:-45}"
    log "[CI_HW] settling rig for profile=$profile (up to ${timeout}s for required boards to re-attach)"
    local waited=0 ok vp need have
    while :; do
        ok=1
        while read -r vp need; do
            [[ -n "$vp" ]] || continue
            have=$(rig_id_count "$vp")
            (( have >= need )) || ok=0
        done < <(profile_required_vidpid_counts "$profile")
        if (( ok == 1 )); then
            log "[CI_HW] required boards present after ${waited}s"
            sleep 2   # let udev settle the fresh nodes/properties
            return 0
        fi
        if (( waited >= timeout )); then
            log "[CI_HW] WARN: required boards not all back after ${waited}s; retrying anyway (preflight will skip if absent)"
            return 0
        fi
        sleep 3
        waited=$(( waited + 3 ))
    done
}

# Simulated-vanish hook (test-only, no rig). LEVICULUM_SIMULATE_VANISH=<group_
# or_test> injects ONE synthetic disconnect into the watchdog for the matching
# group on its FIRST run, so the recovers-on-retry path is exercised (now also
# RED). LEVICULUM_SIMULATE_VANISH_PERSIST=1 also injects on the retry,
# exercising the persisted-vanish path. LEVICULUM_SIMULATE_VANISH_VIDPID overrides
# the injected board id (default 1a86:55d4) so a selftest can assert the
# attribution names a specific board, e.g. an LNode 1209:0001. Matches when the
# value equals the profile name or any fn-name in the group.
maybe_simulate_vanish() {
    local profile="$1" attempt="$2" poison="$3"; shift 3
    local fns=( "$@" )
    local want="${LEVICULUM_SIMULATE_VANISH:-}"
    [[ -n "$want" ]] || return 0
    local match=0 fn
    [[ "$want" == "$profile" ]] && match=1
    for fn in "${fns[@]}"; do
        [[ "$want" == "$fn" ]] && match=1
    done
    (( match == 1 )) || return 0
    local vidpid="${LEVICULUM_SIMULATE_VANISH_VIDPID:-1a86:55d4}"
    if (( attempt == 1 )) || [[ -n "${LEVICULUM_SIMULATE_VANISH_PERSIST:-}" ]]; then
        echo "SIMULATED vanish vid_pid=$vidpid attempt=$attempt profile=$profile (LEVICULUM_SIMULATE_VANISH)" >> "$poison"
        log "[CI_HW] WATCHDOG: simulated rig-board vanish injected (profile=$profile attempt=$attempt vid_pid=$vidpid)"
    fi
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

# Run ONE cargo attempt of a profile group under the device watchdog. Parses
# libtest result lines into the ATTEMPT_* group-scoped arrays and reports
# whether a rig-board vanish was seen during the window (ATTEMPT_POISONED).
# Returns cargo's real exit code. The orchestrator (run_group) decides, from
# ATTEMPT_POISONED, whether to trust this attempt or retry.
#
# Sets: ATTEMPT_FAILED_TESTS ATTEMPT_PASSED_TESTS ATTEMPT_FAILED_COUNT
#       ATTEMPT_POISONED
run_group_attempt() {
    local profile="$1" attempt="$2"
    shift 2
    local fns=( "$@" )
    # Bind LNodes by profile identity, not discovery sort order. The
    # runner reads LEVICULUM_REQUIRED_LNODE_SERIALS and assigns serial
    # nodes to the LNodes whose USB serial is in this set (silencing the
    # rest regardless of sort position). Empty when the profile needs no
    # specific LNode (RNode-only profiles), in which case the runner keeps
    # its sorted-discovery fallback.
    local required_lnode_serials
    required_lnode_serials=$(profile_required_lnode_serials "$profile")
    if [[ -n "$required_lnode_serials" ]]; then
        log "[CI_HW] required LNode serials for profile=$profile: $required_lnode_serials"
    fi

    # Watchdog covers exactly this cargo window. poison non-empty afterwards
    # == a rig board vanished while the tests ran (real or simulated).
    local poison stop
    poison=$(mktemp)
    stop=$(mktemp)
    start_device_watchdog "$poison" "$stop"
    maybe_simulate_vanish "$profile" "$attempt" "$poison" "${fns[@]}"

    # All tests of this profile in one cargo invocation: cheaper than
    # spawning a fresh test binary per test.  --test-threads=1 keeps
    # the integ-lock contract intact.  LEVICULUM_SKIP_LOG collects
    # structured SCENARIO_SKIPPED lines (device-count preflight) —
    # libtest swallows captured output of green tests, so the in-test
    # eprintln alone would be invisible here.
    # Capture this group's cargo output so the final verdict can classify
    # per-test FAILED/ok lines: the EXPECTED_MARGINAL carve-out is per test,
    # but cargo runs a whole profile group in one invocation. The stream is
    # still forwarded to stdout (-> main $LOG) by tee; PIPESTATUS[0] preserves
    # cargo's real exit code through the pipe.
    log "[CI_HW] cargo attempt=$attempt for profile=$profile: ${fns[*]}"
    local group_log cargo_rc
    group_log=$(mktemp)
    if [[ -n "${LEVICULUM_SELFTEST_CARGO:-}" ]]; then
        # Test seam (dry traces only): stub cargo. The command emits
        # libtest-style result lines and exits with a chosen code, so the
        # watchdog/retry/verdict logic is exercised without a rig or a build.
        bash -c "$LEVICULUM_SELFTEST_CARGO" _ "$profile" "$attempt" "${fns[@]}" \
          2>&1 | tee "$group_log"
    else
        CARGO_TARGET_DIR=~/.cache/leviculum-ci-target CARGO_INCREMENTAL=0 \
          LEVICULUM_SKIP_LOG="$SKIP_LOG" \
          LEVICULUM_REQUIRED_LNODE_SERIALS="$required_lnode_serials" \
          cargo test -p reticulum-integ -- --include-ignored --test-threads=1 "${fns[@]}" \
          2>&1 | tee "$group_log"
    fi
    cargo_rc=${PIPESTATUS[0]}

    stop_device_watchdog "$stop"

    # Parse libtest per-test result lines. With --test-threads=1 the result is
    # printed on the running line (`test <path> ... FAILED` / `... ok`). Last
    # `::` segment = fn-name, matching discover_tests and EXPECTED_MARGINAL.
    # ATTEMPT_FAILED_COUNT lets the caller tell a parseable test failure apart
    # from a non-zero cargo exit with no test failure (build/harness error).
    ATTEMPT_FAILED_TESTS=()
    ATTEMPT_PASSED_TESTS=()
    ATTEMPT_FAILED_COUNT=0
    local fn
    while IFS= read -r fn; do
        [[ -n "$fn" ]] || continue
        ATTEMPT_FAILED_TESTS+=( "$fn" )
        ATTEMPT_FAILED_COUNT=$(( ATTEMPT_FAILED_COUNT + 1 ))
    done < <(awk '/^test .* \.\.\. FAILED$/ { print $2 }' "$group_log" \
             | awk -F'::' '{ print $NF }' | sort -u)
    while IFS= read -r fn; do
        [[ -n "$fn" ]] || continue
        ATTEMPT_PASSED_TESTS+=( "$fn" )
    done < <(awk '/^test .* \.\.\. ok$/ { print $2 }' "$group_log" \
             | awk -F'::' '{ print $NF }' | sort -u)
    rm -f "$group_log"

    ATTEMPT_VANISHED_IDS=()
    if [[ -s "$poison" ]]; then
        ATTEMPT_POISONED=1
        log "[CI_HW] WATCHDOG: rig-board vanish during profile=$profile attempt=$attempt:"
        while IFS= read -r fn; do log "[CI_HW]   $fn"; done < "$poison"
        # Pull the vanished board id(s) out of the poison lines so the verdict
        # can name the board(s) (both real "vanish vid_pid=.." and SIMULATED
        # lines carry vid_pid=XXXX:XXXX).
        local vp
        while IFS= read -r vp; do
            [[ -n "$vp" ]] || continue
            ATTEMPT_VANISHED_IDS+=( "$vp" )
        done < <(grep -oE 'vid_pid=[0-9a-fA-F]{4}:[0-9a-fA-F]{4}' "$poison" \
                 | sed 's/^vid_pid=//' | sort -u)
    else
        ATTEMPT_POISONED=0
    fi
    rm -f "$poison"
    return "$cargo_rc"
}

# Commit one attempt's parsed results into the run-global verdict arrays:
# failures gate (FAILED_TESTS), passes are recorded (PASSED_TESTS). Only ever
# called for a TRUSTED attempt (a first run that saw no vanish). Any confirmed
# vanish never commits: its results are untrusted and the vanish itself is the
# RED cause, whether it persisted across the retry or recovered on it.
commit_attempt_results() {
    local fn
    for fn in "${ATTEMPT_FAILED_TESTS[@]:-}"; do
        [[ -n "$fn" ]] || continue
        FAILED_TESTS+=( "$fn" )
    done
    for fn in "${ATTEMPT_PASSED_TESTS[@]:-}"; do
        [[ -n "$fn" ]] || continue
        PASSED_TESTS+=( "$fn" )
    done
}

# Orchestrate a profile group: run once under the watchdog; if the window saw a
# vanish, re-attach + retry ONCE only to rule out a one-frame sampling blip. A
# confirmed vanish -> the group is RED with the board(s) named
# (GROUP_BOARD_VANISH / VANISHED_BOARDS_PERSIST), whether it persisted across the
# retry or recovered on it; the retry result is diagnostic only. Sets
# GROUP_BOARD_VANISH, GROUP_FAILED_COUNT and LAST_GROUP_RC for the main loop;
# commits TRUSTED results into the verdict arrays only (never on a vanish).
run_group() {
    local profile="$1"
    shift
    local fns=( "$@" )
    setup_profile "$profile"
    log "[CI_HW] running ${#fns[@]} tests for profile=$profile: ${fns[*]}"

    GROUP_BOARD_VANISH=0
    GROUP_FAILED_COUNT=0
    LAST_GROUP_RC=0

    local rc=0
    run_group_attempt "$profile" 1 "${fns[@]}" && rc=0 || rc=$?

    if (( ATTEMPT_POISONED == 0 )); then
        commit_attempt_results
        GROUP_FAILED_COUNT=$ATTEMPT_FAILED_COUNT
        LAST_GROUP_RC=$rc
        return 0
    fi

    # A rig board vanished mid-run. Capture which board(s) before the retry
    # overwrites ATTEMPT_VANISHED_IDS. Re-attach/settle and retry ONCE only to
    # rule out a one-frame sampling blip.
    local first_vanish_ids=( "${ATTEMPT_VANISHED_IDS[@]:-}" )
    log "[CI_HW] profile=$profile UNTRUSTED — rig-board vanish during run (${first_vanish_ids[*]}); re-attaching and retrying ONCE"
    settle_rig "$profile"

    local rc2=0
    run_group_attempt "$profile" 2 "${fns[@]}" && rc2=0 || rc2=$?

    # A rig-board vanish was confirmed on the first run; the retry only rules
    # out a one-frame watchdog sampling blip. Post-VFIO the host cannot drop a
    # board VM-side, so a confirmed vanish is ALWAYS a real firmware self-reset
    # (suspected heap exhaustion under load, Codeberg 65) and is RED with the
    # board(s) named, whether it persisted across the retry or recovered on it:
    # recovery does not make a self-reset acceptable. The group's own test
    # results are untrusted and NOT committed; the vanish itself is the RED
    # cause. The persisted-vs-recovered distinction is a diagnostic log note
    # only and does not change the verdict.
    local id
    for id in "${first_vanish_ids[@]}" "${ATTEMPT_VANISHED_IDS[@]:-}"; do
        [[ -n "$id" ]] || continue
        VANISHED_BOARDS_PERSIST+=( "$id" )
    done
    GROUP_BOARD_VANISH=1
    GROUP_FAILED_COUNT=$ATTEMPT_FAILED_COUNT
    LAST_GROUP_RC=$rc2
    if (( ATTEMPT_POISONED == 0 )); then
        log "[CI_HW] profile=$profile RED — board vanish (${first_vanish_ids[*]}) recovered on retry but is a real firmware self-reset (Codeberg 65); RED with board(s) named"
    else
        log "[CI_HW] profile=$profile RED — board vanish persisted across retry (${VANISHED_BOARDS_PERSIST[*]}); real device/firmware failure, suspected firmware self-reset (Codeberg 65)"
    fi
    return 0
}

# --- Flash LNodes from HEAD, then discover, group, run ---

# Additive step: bring the attached LNodes to the tested commit before any
# profile group runs. Smoke runs flash too — a smoke pass against stale
# firmware is just as misleading as a full one. Skipped in selftest mode
# (no rig, watchdog/verdict-only verification).
if [[ -z "${LEVICULUM_SELFTEST:-}" ]]; then
    flash_lnodes
fi

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
# Per-test failure accounting for the EXPECTED_MARGINAL carve-out. run_group
# appends fn-names to these and sets GROUP_FAILED_COUNT / GROUP_BOARD_VANISH /
# LAST_GROUP_RC for the group it just ran. VANISHED_BOARDS_PERSIST collects the
# vid:pid of every confirmed board vanish (forces RED), whether it persisted
# across the retry or recovered on it. Initialised here (set -u) before the
# first call.
FAILED_TESTS=()
PASSED_TESTS=()
VANISHED_BOARDS_PERSIST=()
GROUP_FAILED_COUNT=0
GROUP_BOARD_VANISH=0
LAST_GROUP_RC=0

RC=0
for profile in $(printf '%s\n' "${!PROFILE_BUCKETS[@]}" | sort); do
    # shellcheck disable=SC2206
    fns=( ${PROFILE_BUCKETS[$profile]} )
    # run_group always returns 0 (it absorbs the cargo rc into LAST_GROUP_RC and
    # the verdict arrays); the non-zero-cargo handling keys off LAST_GROUP_RC.
    run_group "$profile" "${fns[@]}"
    if (( GROUP_BOARD_VANISH == 1 )); then
        # A confirmed rig-board vanish (persisted across the retry or recovered
        # on it). Under VFIO controller passthrough the host cannot drop a board
        # VM-side, so this is a real device/firmware failure (suspected
        # self-reset, Codeberg 65), not an infra glitch. It forces RED with the
        # board(s) named in the verdict block. Handled there; do NOT fall through
        # to the build/harness-error branch, which would mis-attribute the
        # vanish-induced cargo abort as a build bug.
        log "[CI_HW] profile=$profile RED — confirmed rig-board vanish; attributed in verdict"
    elif (( LAST_GROUP_RC != 0 )); then
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
        if (( GROUP_FAILED_COUNT == 0 )); then
            # cargo failed but no per-test FAILED line was parsed (and no
            # vanish): build error, harness abort or crash. Cannot be carved
            # out — hard RED.
            RC=1
            log "[CI_HW] profile=$profile RED — cargo failed with no parseable test failure (build/harness error) — see $LOG"
        else
            # Genuine test failure(s); the GREEN/RED decision and the
            # EXPECTED_MARGINAL carve-out are applied once, after the loop.
            log "[CI_HW] profile=$profile had ${GROUP_FAILED_COUNT} test failure(s) — classified below"
        fi
    fi
done

# --- Verdict: apply the EXPECTED_MARGINAL carve-out per failed test ---
#
# A failed test on EXPECTED_MARGINAL is reported separately and counted, but
# does NOT flip the verdict to RED. Any other failed test does. Only genuine
# test FAILURES are absorbed here; device-count skips and lock-skips keep their
# own separate reporting and never reach this list.
EXPECTED_MARGINAL_FAILED=0
for fn in "${FAILED_TESTS[@]:-}"; do
    [[ -n "$fn" ]] || continue
    if is_expected_marginal "$fn"; then
        EXPECTED_MARGINAL_FAILED=$(( EXPECTED_MARGINAL_FAILED + 1 ))
        # Mechanism per test is documented at the EXPECTED_MARGINAL array
        # (SF10/CR8 mixed-chip for the benches, python-stack link establishment
        # for lora_link_python); not repeated here so the line stays accurate.
        log "[CI_HW] EXPECTED_MARGINAL $fn failed as expected"
    else
        RC=1
    fi
done

# Loud notice if a carved-out bench unexpectedly PASSED, so a recovery is
# noticed and the carve-out can be revisited. Excludes the case where the
# bench was device-count skipped (a skip also exits the test green).
for m in "${EXPECTED_MARGINAL[@]}"; do
    if printf '%s\n' "${PASSED_TESTS[@]:-}" | grep -qx "$m" \
       && ! grep -q "^SCENARIO_SKIPPED scenario=$m " "$SKIP_LOG" 2>/dev/null; then
        log "[CI_HW] EXPECTED_MARGINAL $m PASSED unexpectedly: revisit carve-out"
    fi
done

# Board-vanish attribution (replaces the retired INFRA_INVALID class). Any
# confirmed rig-board vanish is a real device/firmware failure (suspected
# firmware self-reset under load, Codeberg 65) and forces RED, whether the board
# stayed gone across the retry or recovered on it. The VFIO controller
# passthrough cannot produce a host-side phantom disconnect, so a vanish is
# never an infrastructure artefact: it is never absorbed.
dedup_ids() { printf '%s\n' "$@" | awk 'NF' | sort -u | paste -sd, -; }
BOARD_VANISH_IDS=$(dedup_ids "${VANISHED_BOARDS_PERSIST[@]:-}")
if [[ -n "$BOARD_VANISH_IDS" ]]; then
    RC=1
    log "[CI_HW] ===================================================================="
    log "[CI_HW] BOARD VANISH (RED): rig board(s) $BOARD_VANISH_IDS vanished mid-run."
    log "[CI_HW] Real device/firmware failure, suspected firmware self-reset under"
    log "[CI_HW] load (Codeberg 65). Forces tier3 RED; the affected group's own"
    log "[CI_HW] results are untrusted. A vanish that recovered on the retry is RED"
    log "[CI_HW] too: post-VFIO a vanish is always a real self-reset."
    log "[CI_HW] ===================================================================="
fi

# Device-count skips are neither green nor red — surface them loudly
# in the summary and the results line. A non-empty skip list with the
# 4th RNode back in the rig means a profile/registration gap, not noise.
SKIPPED=0
if [[ -s "$SKIP_LOG" ]]; then
    SKIPPED=$(grep -c '^SCENARIO_SKIPPED' "$SKIP_LOG" || true)
    log "[CI_HW] $SKIPPED scenario(s) skipped on device-count preflight:"
    while IFS= read -r line; do log "[CI_HW]   $line"; done < "$SKIP_LOG"
fi

# Verdict fields. A confirmed board vanish names the board(s) and the suspected
# cause so the line is unmissable, e.g.
#   tier3 RED (expected_marginal=0 skipped=0 board_vanish=1209:0001 firmware_self_reset_suspected)
# This holds whether the board stayed gone across the retry or recovered on it.
VERDICT_FIELDS="expected_marginal=$EXPECTED_MARGINAL_FAILED skipped=$SKIPPED"
if [[ -n "$BOARD_VANISH_IDS" ]]; then
    VERDICT_FIELDS="$VERDICT_FIELDS board_vanish=$BOARD_VANISH_IDS firmware_self_reset_suspected"
fi

if [[ $RC -eq 0 ]]; then
    log "[CI_HW] tier3 GREEN ($VERDICT_FIELDS)"
    echo "$(date -Iseconds) tier3 GREEN $VERDICT_FIELDS $LOG" >> "$RESULTS"
else
    log "[CI_HW] tier3 RED ($VERDICT_FIELDS)"
    echo "$(date -Iseconds) tier3 RED $VERDICT_FIELDS $LOG" >> "$RESULTS"
    # One bundle per tier-3 run, not per failing profile group;
    # per-profile RED log lines remain informational only.
    bash "$REPO_DIR/scripts/_emit-auto-bug-bundle.sh" tier3-hw "$LOG" || true
fi
exit $RC
