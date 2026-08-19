#!/bin/bash
# run-tier3-hw.sh — Tier-3 nightly runner, all boards always powered.
#
# Since the retirement of the in-repo `reticulum-integ` crate this script no
# longer discovers, groups or judges scenarios: **periculum does that**. What
# survives here is exactly the set of capabilities periculum has no home for,
# because they are about keeping THIS rig and THIS project honest rather than
# about running a scenario:
#
#   1. the CI bookkeeping — per-run log file, the append-only
#      `last-results.txt` ledger, log rotation, the auto-bug bundle on RED;
#   2. the VM-mode repo sync;
#   3. the binary-mtime fix (touch the bin-crate sources) that no amount of
#      cargo fingerprinting replaces, plus the `periculum check-freshness`
#      preflight it feeds;
#   4. LNode flash-from-HEAD and [FW_BUILD] firmware-identity verification
#      (scripts/flash-lnodes-from-head.sh) — periculum deliberately leaves
#      board preparation outside its scope;
#   5. the USB device-vanish watchdog and its always-RED attribution.
#
# Everything else — scenario discovery, rig profiles and board pinning, the
# governed `[marginal]` carve-outs (formerly the EXPECTED_MARGINAL allowlist
# that lived in this file), the structured skip tally, the verdict and exit-code
# contract — is periculum's and is not duplicated here.
#
# Usage:
#   bash scripts/run-tier3-hw.sh                    # full nightly (~2-6 h)
#   bash scripts/run-tier3-hw.sh --smoke <pattern>  # subset matching pattern
#
# The `--smoke <pattern>` form selects scenario FILES whose basename contains
# any of the given patterns; same preflight, flash and watchdog handling,
# smaller scenario set. Used by Lew for ad-hoc verification.
#
# NO USB-hub power switching. Every attached board stays powered on and
# passed through to the VM for the entire run. RF isolation of the
# non-participating LNodes is done by the runner, which pushes `radio_silent`
# to every discovered board the scenario did not bind. This replaced the old
# per-profile `uhubctl`/usbhub-helper power cycling, which correlated with
# hamster hardware-watchdog freezes (proven 2026-06-15) and is gone for good.
#
# DEVICE-VANISH HONESTY. The rig boards are passed through to this VM via VFIO
# controller passthrough: the guest owns the xHCI host controller natively, so
# the host cannot inject a phantom VM-side USB disconnect. The old qemu usb-host
# passthrough could drop a board VM-side under load as a pure infrastructure
# artefact; that class is now impossible. A board that vanishes mid-run is
# therefore ALWAYS a real device/firmware failure (a self-reset under sustained
# load, suspected heap exhaustion, Codeberg 65), not an infra glitch, and must
# read as RED. There is no INFRA_INVALID class: a vanish is never absorbed.
#
# CHANGED WITH THE PERICULUM MOVE: the watchdog now covers the WHOLE run
# instead of one profile group, and the vanish is no longer followed by a
# settle + retry-once. periculum runs the corpus in a single process with a
# single device discovery, so there are no groups to retry, and re-running the
# corpus would cost the full 2-6 h. The retry never changed the verdict — a
# confirmed vanish was RED whether it persisted or recovered — so what is lost
# is the diagnostic persisted-vs-recovered note and the "untrusted group's
# results are discarded" rule. Results after a vanish are still reported, and
# the banner says from when on they are untrusted.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
LOG_DIR=~/.local/state/leviculum-ci
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
JSON="$LOG_DIR/nightly-hw-$(date +%Y%m%d-%H%M%S)-$$.json"

# The .log files are rotated by run-tier3.sh's `nightly-*.log` sweep; the
# results documents are new with the periculum move and rotate on the same
# 60-day nightly policy so they cannot fill the disk unattended.
find "$LOG_DIR" -name 'nightly-hw-*.json' -mtime +60 -delete 2>/dev/null || true

# Always log to both stdout (for interactive feedback) and the per-run
# log (for forensic).  exec the redirection so every subsequent command
# is captured automatically.
exec > >(tee -a "$LOG") 2>&1

log() { echo "$@"; }

# No EXIT trap to restore hub state: with all boards permanently powered
# on there is nothing to switch back, so no restore is needed.

# --- Periculum resolution ---
#
# periculum is a sibling checkout (its `periculum` crate has a hard Cargo path
# dependency on ../libreticulum). PERICULUM_ROOT overrides the checkout,
# PERICULUM_BIN the binary. Built on demand with a pinned target dir so the
# default PERICULUM_BIN path holds even though this script exports a global
# CARGO_TARGET_DIR.
PERICULUM_ROOT="${PERICULUM_ROOT:-$REPO_DIR/../periculum}"
PERICULUM_BIN="${PERICULUM_BIN:-$PERICULUM_ROOT/target/release/periculum}"

# The corpora a tier-3 nightly runs. All three, because the pre-periculum
# nightly ran `cargo test -- --include-ignored`, i.e. the hardware scenarios
# ON TOP OF the docker ones, and dropping the docker half here would silently
# narrow the nightly.
CORPORA=( conformance regression hardware )

# --- Argument parsing ---

SMOKE_MODE=false
SMOKE_PATTERN=""
if [[ "${1:-}" == "--smoke" ]]; then
    SMOKE_MODE=true
    SMOKE_PATTERN="${2:-}"
    if [[ -z "$SMOKE_PATTERN" ]]; then
        log "ERROR: --smoke needs a scenario-name pattern."
        log "Usage: $0 --smoke '<pattern> [<pattern> ...]'"
        exit 1
    fi
    log "[CI_HW] mode=smoke pattern='$SMOKE_PATTERN'"
else
    log "[CI_HW] mode=full-nightly"
fi

# --- Build the binaries periculum mounts into the node containers ---
#
# Fresh-binary guarantee (2026-06-13 nightly: 12 setup aborts).
# cargo decides "up to date" by source mtime. After _repo-sync.sh pulls
# newer commits whose checked-out files keep their old mtimes, cargo can
# skip the relink ("Finished in 0.07s") so the binary mtime stays old —
# while `check_binary_freshness` compares that mtime against the git COMMIT
# time of the last production-source change. The two truths diverge and every
# binary-mounting scenario aborts in setup.
#
# Robustness: TOUCH the bin-crate sources so cargo recompiles the final
# crates and relinks, stamping a fresh mtime on every binary. (Deleting
# only the top-level binary does NOT work: cargo re-hardlinks it from
# target/release/deps without relinking, preserving the old mtime —
# verified 2026-06-13.) periculum's own force-rebuild does not do this, so the
# touch has to happen on the way in — it lives in `build-integ-bins`
# (Justfile), together with the list of binaries, which is why this script
# calls the recipe instead of carrying a cargo line of its own: two lists
# drift, and this one had (the recipe grew --bin lxmf-node, the script did
# not, and every run after a source change died in the preflight below
# pointing at lxmf-node — 2026-08-19). Then PRE-FLIGHT with the EXACT same
# check each scenario's setup runs (`periculum check-freshness`, one source
# of truth) and abort the whole run on a single clear failure rather than
# letting N scenarios die one by one.
#
# The whole build + freshness preflight is skipped in selftest mode
# (LEVICULUM_SELFTEST=1), which exercises only the watchdog/verdict logic with
# a stubbed periculum and needs no binaries and no rig.
CACHE_TARGET=~/.cache/leviculum-ci-target
if [[ -z "${LEVICULUM_SELFTEST:-}" ]]; then
log "[CI_HW] building node binaries (just build-integ-bins)"
( cd "$REPO_DIR" && CARGO_TARGET_DIR="$CACHE_TARGET" CARGO_INCREMENTAL=0 just build-integ-bins )

# regression/c_api_restart_recovery mounts c-lnsd, which is a C build, not a
# cargo bin, so periculum's force-rebuild cannot produce it.
log "[CI_HW] building c-lnsd"
( cd "$REPO_DIR" && CARGO_TARGET_DIR="$CACHE_TARGET" CARGO_INCREMENTAL=0 just build-c-lnsd )

if [[ ! -x "$PERICULUM_BIN" ]]; then
    log "[CI_HW] periculum binary missing — building in $PERICULUM_ROOT"
    ( cd "$PERICULUM_ROOT" && CARGO_TARGET_DIR=target cargo build --release )
fi

# The preflight resolves the same binaries via the same paths:: code each
# scenario's setup uses, so it cannot drift from the per-scenario assertion.
if ! CARGO_TARGET_DIR="$CACHE_TARGET" "$PERICULUM_BIN" check-freshness; then
    # periculum names WHICH binary is stale but cannot say WHY, and the two
    # causes want opposite responses: either the build step built it and it is
    # genuinely older than the last source change (a cargo/mtime problem), or
    # the build step never built it at all (a list problem — the binary is in
    # periculum's freshness list and not in the recipe's). periculum cannot
    # tell them apart because it does not know what its caller builds. This
    # script is the only party that knows both, so it prints the recipe body
    # beside the failure and names the missing-from-the-recipe case first —
    # that is the one that has actually happened (lxmf-node, 2026-08-19).
    log "[CI_HW] the build step above ran this recipe:"
    ( cd "$REPO_DIR" && just --show build-integ-bins ) || true
    log "[CI_HW] SUSPECT: a binary the preflight checks but that recipe does not"
    log "[CI_HW]          build was never built by this run — compare the two lists."
    log "[CI_HW] FATAL: node binaries still stale after forced rebuild — aborting run"
    echo "$(date -Iseconds) tier3 RED stale-binaries-preflight $LOG" >> "$RESULTS"
    exit 1
fi
fi

# --- Scenario selection ---
#
# periculum takes files and directories, so the full nightly is the three
# corpus directories and a smoke run is the subset of their scenario files
# whose basename matches any supplied pattern.
select_targets() {
    if ! $SMOKE_MODE; then
        local c
        for c in "${CORPORA[@]}"; do
            echo "$PERICULUM_ROOT/$c"
        done
        return
    fi
    local c pat f
    for c in "${CORPORA[@]}"; do
        for pat in $SMOKE_PATTERN; do
            for f in "$PERICULUM_ROOT/$c/"*"$pat"*.toml; do
                [[ -e "$f" ]] && echo "$f"
            done
        done
    done | sort -u
}

# --- Device-vanish watchdog ---
#
# The four distinct USB IDs of the five rig boards. The two T-Beams share
# 1a86:55d4, so that ID's baseline count is 2 and a single T-Beam vanish drops
# it to 1. Every board, including ones the active scenario silenced, counts:
# a silenced board that vanishes and returns un-silenced can still interfere
# with the running scenario, so ANY rig-board disconnect poisons the run.
RIG_USB_IDS=( "1a86:55d4" "1209:0001" "1209:0002" "303a:1001" )

# Number of currently enumerated USB devices for one vid:pid.
rig_id_count() { lsusb -d "$1" 2>/dev/null | wc -l | tr -d ' '; }

WATCHDOG_PID=""

# Start a background watchdog for the run's execution window. It snapshots a
# per-vid:pid baseline count, then polls once a second; the first time any ID's
# count drops below its baseline it appends one timestamped line to $poison
# (latched per ID so a long outage does not spam). Non-empty $poison after the
# window == a rig-board vanished during the run. Pure lsusb poll: no root, no
# dmesg privilege, robust to ttyACM renumbering (keyed by device identity, not
# node).
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
                    echo "vanish at=$(date -Iseconds) vid_pid=$id baseline=${base[$id]} now=$cur" >> "$poison"
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

# Simulated-vanish hook (test-only, no rig). LEVICULUM_SIMULATE_VANISH=1
# injects ONE synthetic disconnect into the watchdog so the RED attribution
# path is exercised without a rig. LEVICULUM_SIMULATE_VANISH_VIDPID overrides
# the injected board id (default 1a86:55d4) so a selftest can assert the
# attribution names a specific board, e.g. an LNode 1209:0001.
maybe_simulate_vanish() {
    local poison="$1"
    [[ -n "${LEVICULUM_SIMULATE_VANISH:-}" ]] || return 0
    local vidpid="${LEVICULUM_SIMULATE_VANISH_VIDPID:-1a86:55d4}"
    echo "SIMULATED vanish at=$(date -Iseconds) vid_pid=$vidpid (LEVICULUM_SIMULATE_VANISH)" >> "$poison"
    log "[CI_HW] WATCHDOG: simulated rig-board vanish injected (vid_pid=$vidpid)"
}

# --- Flash the LNodes from HEAD, then verify they really run it ---
#
# Additive step: bring the attached LNodes to the tested commit before any
# scenario runs. Smoke runs flash too — a smoke pass against stale firmware is
# just as misleading as a full one. Skipped in selftest mode (no rig).
#
# LNODE_FW_UNVERIFIED_IDS collects the USB id of every enumerated LNode we
# could not confirm runs HEAD (hard flash failure or unconfirmed [FW_BUILD]
# banner). Non-empty -> the run is RED and the verdict line names the board(s)
# via firmware_unverified=<ids>, mirroring board_vanish.
LNODE_FW_UNVERIFIED_IDS=()
if [[ -z "${LEVICULUM_SELFTEST:-}" ]]; then
    while IFS=' ' read -r tag id; do
        [[ "$tag" == "FW_UNVERIFIED" && -n "$id" ]] || continue
        LNODE_FW_UNVERIFIED_IDS+=( "$id" )
    done < <(bash "$REPO_DIR/scripts/flash-lnodes-from-head.sh")
fi
# Test-only: force the firmware-unverified path for a given board id without a
# rig/flash (selftest skips the real flash above).
if [[ -n "${LEVICULUM_SIMULATE_FW_STALE:-}" ]]; then
    LNODE_FW_UNVERIFIED_IDS+=( "${LEVICULUM_SIMULATE_FW_STALE}" )
    log "[CI_HW] FW_VERIFY: simulated firmware-unverified injected (board=${LEVICULUM_SIMULATE_FW_STALE}, LEVICULUM_SIMULATE_FW_STALE)"
fi

# --- The run ---

mapfile -t TARGETS < <(select_targets)
if (( ${#TARGETS[@]} == 0 )); then
    log "[CI_HW] no scenarios matched; nothing to do"
    exit 0
fi
log "[CI_HW] running periculum over ${#TARGETS[@]} target(s): ${TARGETS[*]}"

POISON=$(mktemp)
STOP=$(mktemp)
start_device_watchdog "$POISON" "$STOP"
maybe_simulate_vanish "$POISON"

# periculum prints its own per-scenario VERDICT lines and the SUMMARY; the
# machine-readable document goes to $JSON for the verdict block below. The
# exec redirection at the top already tees everything into $LOG.
PERICULUM_RC=0
if [[ -n "${LEVICULUM_SELFTEST_PERICULUM:-}" ]]; then
    # Test seam (selftest only): stub periculum. The command emits
    # periculum-style output and exits with a chosen code, so the
    # watchdog/verdict logic is exercised without a rig or a build.
    bash -c "$LEVICULUM_SELFTEST_PERICULUM" _ "$JSON" "${TARGETS[@]}" || PERICULUM_RC=$?
else
    CARGO_TARGET_DIR="$CACHE_TARGET" \
      "$PERICULUM_BIN" run --json-out "$JSON" "${TARGETS[@]}" || PERICULUM_RC=$?
fi

stop_device_watchdog "$STOP"

VANISHED_BOARDS=()
if [[ -s "$POISON" ]]; then
    log "[CI_HW] WATCHDOG: rig-board vanish during the run:"
    while IFS= read -r line; do log "[CI_HW]   $line"; done < "$POISON"
    while IFS= read -r vp; do
        [[ -n "$vp" ]] || continue
        VANISHED_BOARDS+=( "$vp" )
    done < <(grep -oE 'vid_pid=[0-9a-fA-F]{4}:[0-9a-fA-F]{4}' "$POISON" \
             | sed 's/^vid_pid=//' | sort -u)
fi
rm -f "$POISON"

# --- Verdict ---
#
# periculum owns the scenario verdicts, the governed `[marginal]` carve-outs
# and the skip tally; this block only reads its exit code and summary and adds
# the two rig-honesty causes periculum knows nothing about.
#
# periculum exit codes (its CI contract):
#   0  something ran, nothing RED
#   1  at least one scenario RED
#   2  usage error, malformed scenario, internal runner error, refused preflight
#   3  nothing ran (only SKIPPED_INFRA / UNSUPPORTED)

# Summary counters out of the JSON document, empty when it was never written
# (exit 2 produces none).
read_summary() {
    local key="$1"
    [[ -s "$JSON" ]] || { echo 0; return; }
    python3 - "$JSON" "$key" <<'PY'
import json, sys
try:
    with open(sys.argv[1]) as fh:
        print(json.load(fh).get('summary', {}).get(sys.argv[2], 0))
except Exception:
    print(0)
PY
}

MARGINAL=$(read_summary marginal)
SKIPPED=$(read_summary skipped_infra)

RC=0
case "$PERICULUM_RC" in
    0) ;;
    1) RC=1 ;;
    2)
        if [[ -f "$MARKER" ]]; then
            # Lock-contention path mirrors run-tier3.sh: another process held
            # the runner lock when this fired. Treat as SKIPPED, not RED. The
            # marker file decouples skip-vs-fail from log-text grepping;
            # periculum drops it before printing, precisely for this consumer.
            rm -f "$MARKER"
            log "[CI_HW] SKIPPED — another run held the runner lock"
            echo "$(date -Iseconds) tier3 SKIPPED lock-held $LOG" >> "$RESULTS"
            exit 0
        fi
        RC=1
        log "[CI_HW] RED — periculum exited 2 (usage, malformed scenario, internal runner error or refused preflight) — see $LOG"
        ;;
    3)
        # Nothing ran: every scenario was SKIPPED_INFRA or UNSUPPORTED. That is
        # a rig statement, not a protocol one, so it is never RED — but it is
        # not GREEN either, and the ledger must not read as a passing nightly.
        log "[CI_HW] SKIPPED — the whole corpus reported SKIPPED_INFRA/UNSUPPORTED; nothing ran"
        ;;
    *)
        RC=1
        log "[CI_HW] RED — periculum exited $PERICULUM_RC (unknown code) — see $LOG"
        ;;
esac

dedup_ids() { printf '%s\n' "$@" | awk 'NF' | sort -u | paste -sd, -; }

# Board-vanish attribution. Any confirmed rig-board vanish is a real
# device/firmware failure (suspected firmware self-reset under load, Codeberg
# 65) and forces RED. The VFIO controller passthrough cannot produce a
# host-side phantom disconnect, so a vanish is never an infrastructure
# artefact: it is never absorbed.
BOARD_VANISH_IDS=$(dedup_ids "${VANISHED_BOARDS[@]:-}")
if [[ -n "$BOARD_VANISH_IDS" ]]; then
    RC=1
    log "[CI_HW] ===================================================================="
    log "[CI_HW] BOARD VANISH (RED): rig board(s) $BOARD_VANISH_IDS vanished mid-run."
    log "[CI_HW] Real device/firmware failure, suspected firmware self-reset under"
    log "[CI_HW] load (Codeberg 65). Forces tier3 RED. Every scenario verdict from"
    log "[CI_HW] the vanish timestamp above onwards is UNTRUSTED: the rig it ran on"
    log "[CI_HW] was not the rig the corpus assumes."
    log "[CI_HW] ===================================================================="
fi

# Firmware-unverified attribution. Same hardware-honesty class as a board
# vanish: an enumerated LNode whose firmware we could NOT confirm == HEAD (hard
# flash failure or no matching [FW_BUILD] banner) means the run tested UNKNOWN
# firmware, so its results cannot be trusted. Forces RED with the board(s) named
# in the verdict line, exactly as board_vanish does. It does NOT abort the run
# (non-fatal-flash contract): the RAK + RNodes may still yield valid results, so
# only the verdict flips. An absent board never reaches here (it was never
# flashed) and stays a clean device-count skip.
FW_UNVERIFIED_IDS=$(dedup_ids "${LNODE_FW_UNVERIFIED_IDS[@]:-}")
if [[ -n "$FW_UNVERIFIED_IDS" ]]; then
    RC=1
    log "[CI_HW] ===================================================================="
    log "[CI_HW] FIRMWARE UNVERIFIED (RED): LNode(s) $FW_UNVERIFIED_IDS could not be"
    log "[CI_HW] confirmed to run HEAD (hard flash failure or no matching [FW_BUILD]"
    log "[CI_HW] banner). The run tested UNKNOWN firmware; forces tier3 RED so a"
    log "[CI_HW] stale/failed flash is never silently trusted."
    log "[CI_HW] ===================================================================="
fi

# Verdict fields. `expected_marginal` and `skipped` keep their historic
# spelling and come from periculum's summary (`marginal`, `skipped_infra`); the
# two rig-honesty causes are added here. A confirmed board vanish names the
# board(s) and the suspected cause so the line is unmissable, e.g.
#   tier3 RED (expected_marginal=0 skipped=0 board_vanish=1209:0001 firmware_self_reset_suspected)
# A firmware-unverified LNode (stale/failed flash) is the same honesty class and
# names its board too, e.g.
#   tier3 RED (expected_marginal=0 skipped=0 firmware_unverified=1209:0001)
VERDICT_FIELDS="expected_marginal=$MARGINAL skipped=$SKIPPED"
if [[ -n "$BOARD_VANISH_IDS" ]]; then
    VERDICT_FIELDS="$VERDICT_FIELDS board_vanish=$BOARD_VANISH_IDS firmware_self_reset_suspected"
fi
if [[ -n "$FW_UNVERIFIED_IDS" ]]; then
    VERDICT_FIELDS="$VERDICT_FIELDS firmware_unverified=$FW_UNVERIFIED_IDS"
fi

if (( RC == 0 )) && [[ "$PERICULUM_RC" == "3" ]]; then
    log "[CI_HW] tier3 SKIPPED ($VERDICT_FIELDS)"
    echo "$(date -Iseconds) tier3 SKIPPED nothing-ran $VERDICT_FIELDS $LOG" >> "$RESULTS"
    exit 0
fi

if [[ $RC -eq 0 ]]; then
    log "[CI_HW] tier3 GREEN ($VERDICT_FIELDS)"
    echo "$(date -Iseconds) tier3 GREEN $VERDICT_FIELDS $LOG" >> "$RESULTS"
else
    log "[CI_HW] tier3 RED ($VERDICT_FIELDS)"
    echo "$(date -Iseconds) tier3 RED $VERDICT_FIELDS $LOG" >> "$RESULTS"
    # One bundle per tier-3 run, not per failing scenario.
    bash "$REPO_DIR/scripts/_emit-auto-bug-bundle.sh" tier3-hw "$LOG" || true
fi
exit $RC
