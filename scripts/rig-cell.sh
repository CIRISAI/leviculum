#!/usr/bin/env bash
# rig-cell.sh <cell> [cell...] — run single hardware cells on the bench with
# the discipline of the full tier-3 runner: the bench lock, the standing debug
# readers stopped for the run and brought back after it, the pinned periculum
# snapshot as executor, the rig worktree as the tree under test, and the
# airtime lock waived (the bench is a shielded room; a cell that is ABOUT duty
# is run by other means).
#
# It replaces a reviewer helper that lived in the bench directory and knew its
# host by literal paths and its boards by three literal serials. Here every
# host path comes from the environment with a default derived from $HOME, and
# the boards come from the bench's own rig.toml.
#
# Environment (default in brackets):
#   LEV_RIG_RUN               bench directory [$HOME/rig-run]
#   LEV_RIG_TOML              board inventory [$LEV_RIG_RUN/rig.toml]
#   LEV_RIG_PROOF             proved firmware sha file [$LEV_RIG_RUN/proof/lnode]
#   PERICULUM_REFERENCE_ROOT  periculum checkout with the reference submodules
#                             [$HOME/coding/periculum]
#   PERICULUM_REGISTER_REF    known-reds register
#                             [$PERICULUM_REFERENCE_ROOT/hardware/KNOWN-REDS.toml]
#   LEV_RIG_PERICULUM_SHA     executor snapshot sha
#                             [short origin/master of PERICULUM_REFERENCE_ROOT]
#   LEV_RIG_SNAPSHOT          executor snapshot
#                             [$LEV_RIG_RUN/snap-periculum-$LEV_RIG_PERICULUM_SHA]
#   PERICULUM_LEVICULUM_ROOT  rig worktree [$HOME/coding/libreticulum-rig]
#   LEV_RIG_LOG_DIR           cell logs [$HOME/ci]
#   LEV_RIG_RUNLOG            this runner's own log [$LEV_RIG_LOG_DIR/rig-cell.log]
#   LEV_RIG_STATE             bench lock directory
#                             [${XDG_STATE_HOME:-$HOME/.local/state}/leviculum-ci]
#   LEV_RIG_BY_ID             serial by-id directory [/dev/serial/by-id]
#   LEV_RIG_READER            reader program [scripts/debug-witness-reader.py]
#   LEV_RIG_READER_WAIT       seconds to wait for a board's link after the run [30]
#
# THE READERS
#
# Every `firmware = "lnode"` device in rig.toml gets one reader on its if00
# debug port, found by serial alone (`usb-*_<serial>-if00`: the product part
# of the name differs per board, the serial does not). The reader is
# scripts/debug-witness-reader.py with --attach-now: it attaches at once,
# timestamps every line, and re-attaches across every reset. Its default mode
# waits for a vanish before attaching, which is right during a run and would
# record nothing between runs. Logs go to $LEV_RIG_RUN/readers/<board>.log.
#
# The cell resets every board it binds, so right after the run a board's link
# is often missing for a few seconds. readers_back waits for it, per board, and
# says either way; a board that is not back is named, never skipped silently.

set -uo pipefail

RIG_CELL_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

LEV_RIG_RUN="${LEV_RIG_RUN:-$HOME/rig-run}"
LEV_RIG_TOML="${LEV_RIG_TOML:-$LEV_RIG_RUN/rig.toml}"
LEV_RIG_PROOF="${LEV_RIG_PROOF:-$LEV_RIG_RUN/proof/lnode}"
LEV_RIG_LOG_DIR="${LEV_RIG_LOG_DIR:-$HOME/ci}"
LEV_RIG_RUNLOG="${LEV_RIG_RUNLOG:-$LEV_RIG_LOG_DIR/rig-cell.log}"
LEV_RIG_STATE="${LEV_RIG_STATE:-${XDG_STATE_HOME:-$HOME/.local/state}/leviculum-ci}"
LEV_RIG_BY_ID="${LEV_RIG_BY_ID:-/dev/serial/by-id}"
LEV_RIG_READER="${LEV_RIG_READER:-$RIG_CELL_REPO/scripts/debug-witness-reader.py}"
LEV_RIG_READER_WAIT="${LEV_RIG_READER_WAIT:-30}"
RIG_READERS_DIR="$LEV_RIG_RUN/readers"

say() {
    local line
    line="$(date -u +%FT%TZ) $*"
    mkdir -p "$(dirname "$LEV_RIG_RUNLOG")" 2>/dev/null || true
    printf '%s\n' "$line" >>"$LEV_RIG_RUNLOG"
    printf '%s\n' "$line"
}

# Every LNode in rig.toml as `<board>\t<serial>`, in file order.
rig_lnode_boards() {
    python3 - "$LEV_RIG_TOML" <<'PY'
import sys, tomllib
try:
    with open(sys.argv[1], "rb") as fh:
        rig = tomllib.load(fh)
except (OSError, tomllib.TOMLDecodeError) as exc:
    print(f"rig.toml unreadable: {exc}", file=sys.stderr)
    sys.exit(1)
for name, dev in rig.get("devices", {}).items():
    if dev.get("firmware") == "lnode" and dev.get("serial"):
        print(f"{name}\t{dev['serial']}")
PY
}

# The board's if00 debug link, or nothing if it is not there.
# Args: $1 = serial
rig_debug_link() {
    local link
    for link in "$LEV_RIG_BY_ID"/usb-*_"$1"-if00; do
        [ -e "$link" ] && { printf '%s\n' "$link"; return 0; }
    done
    return 1
}

rig_reader_pidfile() { printf '%s/%s.pid' "$RIG_READERS_DIR" "$1"; }

# True if this board's reader from an earlier start is still alive.
# Args: $1 = board
rig_reader_alive() {
    local pid
    pid="$(cat "$(rig_reader_pidfile "$1")" 2>/dev/null)" || return 1
    [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null
}

# Close every open fd above 2 in the calling (sub)shell. For a child that
# outlives its caller, before it execs: whatever the caller holds open, a flock
# above all, would otherwise be held by the child for as long as it runs. Not
# only our own bench lock on fd 7: rig-cell is run from tools that hold their
# own locks on other fds, and on 2026-11-08 a reader kept erase.sh's fd 9 lock
# for 42 minutes, so lander and cadence read the rig as held. Closing a list
# of known fds would miss the next caller's; this closes whatever is there.
rig_close_inherited_fds() {
    local fd n
    for fd in /proc/self/fd/*; do
        n="${fd##*/}"
        [[ "$n" =~ ^[0-9]+$ ]] || continue
        [ "$n" -gt 2 ] || continue
        { eval "exec $n>&-"; } 2>/dev/null
    done
}

# Start one reader, detached from this shell so it outlives the run.
# Args: $1 = board, $2 = serial, $3 = link
rig_reader_start() {
    local board="$1" serial="$2" link="$3"
    # The subshell closes every inherited fd above 2 (rig_close_inherited_fds)
    # and then execs, so $! is the reader's own pid. --capture is a year: the
    # reader's default window is 90 s, sized for one post-mortem, and a standing
    # reader that went deaf 90 s after the cell would record nothing until the
    # next reset.
    (
        rig_close_inherited_fds
        exec setsid python3 "$LEV_RIG_READER" --port "$link" \
            --out "$RIG_READERS_DIR/$board.log" --label "$board/$serial" \
            --attach-now --capture 31536000
    ) </dev/null >>"$RIG_READERS_DIR/$board.err" 2>&1 &
    printf '%s\n' "$!" >"$(rig_reader_pidfile "$board")"
}

# Bring every board's reader back. Idempotent: a live reader is left alone.
readers_back() {
    local board serial link waited
    mkdir -p "$RIG_READERS_DIR"
    while IFS=$'\t' read -r board serial; do
        [ -n "$board" ] || continue
        if rig_reader_alive "$board"; then
            say "reader $board still running"
            continue
        fi
        waited=0
        while ! link="$(rig_debug_link "$serial")"; do
            if [ "$waited" -ge "$LEV_RIG_READER_WAIT" ]; then
                link=""
                break
            fi
            sleep 1
            waited=$((waited + 1))
        done
        if [ -z "$link" ]; then
            say "reader $board NOT back: link missing after $waited s"
            continue
        fi
        rig_reader_start "$board" "$serial" "$link"
        say "reader $board back after $waited s"
    done < <(rig_lnode_boards)
}

# Stop every board's reader, and anything else holding a board's debug port:
# the cell needs the ports, and with the bench lock held nothing else may.
readers_stop() {
    local board serial pid link i holders
    while IFS=$'\t' read -r board serial; do
        [ -n "$board" ] || continue
        if rig_reader_alive "$board"; then
            pid="$(cat "$(rig_reader_pidfile "$board")")"
            kill -TERM "$pid" 2>/dev/null
            for ((i = 0; i < 50; i++)); do
                kill -0 "$pid" 2>/dev/null || break
                sleep 0.1
            done
        fi
        rm -f "$(rig_reader_pidfile "$board")"
        link="$(rig_debug_link "$serial")" || continue
        holders="$(lsof -t "$link" 2>/dev/null | tr '\n' ' ')"
        for pid in $holders; do
            say "stopping pid $pid holding $board: $(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null)"
            kill -TERM "$pid" 2>/dev/null
        done
    done < <(rig_lnode_boards)
    say "readers stopped"
}

rig_cell_main() {
    if [ "$#" -eq 0 ]; then
        echo "usage: $0 <cell> [cell...]" >&2
        return 2
    fi
    export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
    export PERICULUM_REFERENCE_ROOT="${PERICULUM_REFERENCE_ROOT:-$HOME/coding/periculum}"
    export PERICULUM_REGISTER_REF="${PERICULUM_REGISTER_REF:-$PERICULUM_REFERENCE_ROOT/hardware/KNOWN-REDS.toml}"
    export PERICULUM_LEVICULUM_ROOT="${PERICULUM_LEVICULUM_ROOT:-$HOME/coding/libreticulum-rig}"
    unset CARGO_TARGET_DIR
    local psha snap sha cell log rc worst=0
    psha="${LEV_RIG_PERICULUM_SHA:-$(git -C "$PERICULUM_REFERENCE_ROOT" rev-parse --short origin/master)}"
    snap="${LEV_RIG_SNAPSHOT:-$LEV_RIG_RUN/snap-periculum-$psha}"
    sha="$(tr -dc 0-9a-f <"$LEV_RIG_PROOF" 2>/dev/null)"
    if [ ! -x "$snap/periculum-bin" ]; then
        say "STOP: no executor at $snap/periculum-bin"
        return 1
    fi
    if ! rig_lnode_boards >/dev/null; then
        say "STOP: cannot read the boards from $LEV_RIG_TOML"
        return 1
    fi
    mkdir -p "$LEV_RIG_STATE" "$LEV_RIG_LOG_DIR"
    exec 7>"$LEV_RIG_STATE/bench.lock"
    if ! flock -n 7; then
        say "STOP: bench held"
        return 1
    fi
    # Only once the bench is ours: a refused run must not restart readers
    # that the holder of the bench stopped for its own run.
    trap readers_back EXIT
    readers_stop
    for cell in "$@"; do
        log="$LEV_RIG_LOG_DIR/hw-cell-$cell-${sha:-unproved}-$(date -u +%H%M%S).log"
        say "cell=$cell firmware=${sha:-unproved} executor=periculum@$psha log=$log"
        (cd "$snap" && ./periculum-bin run --no-duty-wait-even-when-required \
            "$snap/hardware/$cell.toml") >"$log" 2>&1
        rc=$?
        [ "$rc" -ne 0 ] && worst=$rc
        say "rc=$rc $(grep -hoE 'VERDICT [a-z0-9_]+ [A-Z_]+.*' "$log" | tail -1 | cut -c1-300)"
    done
    return "$worst"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    rig_cell_main "$@"
fi
