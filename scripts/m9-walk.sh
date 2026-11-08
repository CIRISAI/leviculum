#!/usr/bin/env bash
#
# The ThinkNode M9 walked on the bench without hands (order 1533): flash a
# named image, boot it twice, run a walk file's keys through the bench
# image's key injection, and hold what the board says to the bars. The
# walk itself, its grammar and its bars are scripts/m9-walk.py's (its
# docstring); this wrapper finds the rig folder's proxy (or starts one),
# flashes, and hands over.
#
# The rig folder is the reviewer's (`/home/lew/rig-run/m9/<date>/` on the
# bench host, but any folder works): the proxy scripts/m9-proxy.py running
# on it (its docstring has the file protocol) owns the M9's CH340, appends the board's bytes to raw.bin, writes
# tx.hex's hex lines to the board and pulses EN for a `reset` file. To
# flash, the proxy is stopped through its STOP file, espflash writes the
# image as `just flash-m9` does, and the proxy is started again with the
# command line it had, detached, its output in <rig>/proxy.out. An lnsd on
# the proxy's pty re-attaches to the new pty by itself: leave it running.
# Do not start a second one; it refuses with "shared-instance name already
# served" while the first holds the name (window 13 of order 1554). With --start-proxy <dev> a bench without a proxy gets one: when
# none runs on the rig folder, scripts/m9-proxy.py is started detached on
# <dev> at 115200 baud, its link at <rig>/m9pty, its output in
# <rig>/proxy.out, and the walk waits for its OPEN note; it keeps running
# after the walk, as the reviewer's would. A proxy of another folder on the
# same <dev> is refused: two readers would split the board's bytes.
#
#   VERDICT <PASS|FAIL> steps=.. boots=.. key_ms n=.. median=.. max=.. ...
#
# A walk that opens chats runs only on a board joined to the bench home
# (order 1562, scripts/m9-bench-home.sh): its address is read from
# --bench-home (32 hex or a file), else from the bench home's folder,
# ${XDG_STATE_HOME:-$HOME/.local/state}/leviculum/m9-bench-home/address, and
# m9-walk.py refuses the walk before its first key when the board's client
# record names another home.
#
# Usage:
#   bash scripts/m9-walk.sh --rig DIR --walk FILE [--image ELF] [--out DIR] [--boots N] [--start-proxy DEV] [--bench-home HEX|FILE]
#   bash scripts/m9-walk.sh --selftest
#
# --image is the ELF to flash, the bench one being
# leviculum-esp/target/bench/xtensa-esp32s3-none-elf/release/thinknode_m9
# (scripts/build-m9-image.sh --bench). Without it the walk runs on what
# the board has. The out folder defaults to <rig>/walk-<time>.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAG="[m9-walk]"

if [ "${1:-}" = "--selftest" ]; then
    mkdir -p "$ROOT/target/tmp"
    M9_WALK_TMP="$ROOT/target/tmp" python3 -I "$ROOT/scripts/m9-proxy.py" --selftest || exit 1
    M9_WALK_TMP="$ROOT/target/tmp" exec python3 -I "$ROOT/scripts/m9-walk.py" --selftest
fi

rig="" walk="" image="" out="" boots=2 start_dev=""
bench_home="${XDG_STATE_HOME:-$HOME/.local/state}/leviculum/m9-bench-home/address"
while [ "$#" -gt 0 ]; do
    case "$1" in
        --rig) rig="${2:-}"; shift 2 ;;
        --walk) walk="${2:-}"; shift 2 ;;
        --image) image="${2:-}"; shift 2 ;;
        --out) out="${2:-}"; shift 2 ;;
        --boots) boots="${2:-}"; shift 2 ;;
        --start-proxy) start_dev="${2:-}"; shift 2 ;;
        --bench-home) bench_home="${2:-}"; shift 2 ;;
        *) echo "usage: $0 --rig DIR --walk FILE [--image ELF] [--out DIR] [--boots N] [--start-proxy DEV] [--bench-home HEX|FILE] | --selftest" >&2; exit 2 ;;
    esac
done
if [ ! -d "$rig" ] || [ ! -f "$walk" ]; then
    echo "$TAG FAIL need --rig <folder> and --walk <file>" >&2
    exit 2
fi
python3 -I "$ROOT/scripts/m9-walk.py" --check "$walk" || exit 2
rig="$(cd "$rig" && pwd)"
out="${out:-$rig/walk-$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$out" || exit 1

# proxy_pid: the proxy whose folder is $rig, by its command line.
proxy_pid() {
    local pid args
    for pid in $(pgrep -f 'm9-proxy\.py'); do
        args="$(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null)" || continue
        case " $args " in
            *" $rig "*) echo "$pid"; return 0 ;;
        esac
    done
    return 1
}

# start_proxy: scripts/m9-proxy.py on $start_dev for $rig, detached, and
# its OPEN note in board.log within 10 s.
start_proxy() {
    local pid args lines
    for pid in $(pgrep -f 'm9-proxy\.py'); do
        args="$(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null)" || continue
        case " $args " in
            *" $start_dev "*) echo "$TAG FAIL m9-proxy.py $pid already owns $start_dev for another folder: $args"; return 1 ;;
        esac
    done
    lines=0
    [ -f "$rig/board.log" ] && lines="$(wc -l <"$rig/board.log")"
    echo "$TAG starting scripts/m9-proxy.py on $start_dev for $rig"
    setsid -f python3 -I "$ROOT/scripts/m9-proxy.py" "$start_dev" 115200 "$rig" "$rig/m9pty" \
        >>"$rig/proxy.out" 2>&1 </dev/null
    for _ in $(seq 100); do
        if tail -n +"$((lines + 1))" "$rig/board.log" 2>/dev/null | grep -q " OPEN $start_dev "; then
            return 0
        fi
        sleep 0.1
    done
    echo "$TAG FAIL the proxy started on $start_dev did not open it within 10 s (see $rig/board.log, $rig/proxy.out)"
    return 1
}

if ! pid="$(proxy_pid)"; then
    if [ -z "$start_dev" ]; then
        echo "$TAG FAIL no m9-proxy.py runs on $rig (--start-proxy <dev> starts one)"
        exit 1
    fi
    start_proxy || exit 1
    pid="$(proxy_pid)" || { echo "$TAG FAIL the proxy started on $start_dev is gone"; exit 1; }
elif [ -n "$start_dev" ]; then
    echo "$TAG m9-proxy.py $pid already runs on $rig, using it"
fi

if [ -n "$image" ]; then
    [ -f "$image" ] || { echo "$TAG FAIL no image $image"; exit 1; }
    mapfile -d '' cmd <"/proc/$pid/cmdline"
    # m9-proxy.py <dev> <baud> <dir> <link>, after the interpreter's words.
    dev=""
    for i in "${!cmd[@]}"; do
        case "${cmd[$i]}" in
            *m9-proxy.py) dev="${cmd[$((i + 1))]:-}" ;;
        esac
    done
    [ -n "$dev" ] || { echo "$TAG FAIL cannot read the proxy's device from its command line"; exit 1; }
    sha256sum "$image" >"$out/image.sha256"
    echo "$TAG flashing $image ($(cut -d' ' -f1 "$out/image.sha256")) to $dev"
    touch "$rig/STOP"
    for _ in $(seq 50); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.1
    done
    if kill -0 "$pid" 2>/dev/null; then
        rm -f "$rig/STOP"
        echo "$TAG FAIL the proxy $pid did not stop"
        exit 1
    fi
    rm -f "$rig/STOP"
    PATH="$HOME/.cargo/bin:$PATH" espflash flash --chip esp32s3 --port "$dev" --baud 230400 \
        --non-interactive --flash-size 16mb --flash-mode dio --flash-freq 80mhz \
        --partition-table "$ROOT/leviculum-esp/partitions-m9.csv" "$image" >"$out/flash.log" 2>&1
    flashed=$?
    # The proxy again, whatever the flash did: the rig is the reviewer's.
    setsid nohup "${cmd[@]}" >>"$rig/proxy.out" 2>&1 </dev/null &
    sleep 1
    if [ "$flashed" -ne 0 ]; then
        cat "$out/flash.log"
        echo "$TAG FAIL espflash rc=$flashed (log: $out/flash.log)"
        exit 1
    fi
    proxy_pid >/dev/null || { echo "$TAG FAIL the proxy did not come back"; exit 1; }
fi

python3 -I "$ROOT/scripts/m9-walk.py" --rig "$rig" --walk "$walk" --out "$out" --boots "$boots" \
    --bench-home "$bench_home"
rc=$?
echo "$TAG out: $out (walk.md for the agent's eye, board.log, dumps/)"
exit "$rc"
