#!/usr/bin/env bash
#
# The ThinkNode M9 walked on the bench without hands (order 1533): flash a
# named image, boot it twice, run a walk file's keys through the bench
# image's key injection, and hold what the board says to the bars. The
# walk itself, its grammar and its bars are scripts/m9-walk.py's (its
# docstring); this wrapper finds the rig folder's proxy, flashes, and hands
# over.
#
# The rig folder is the reviewer's (`/home/lew/rig-run/m9/<date>/` on the
# bench host, but any folder works): the proxy `m9-proxy.py` running on it
# owns the M9's CH340, appends the board's bytes to raw.bin, writes
# tx.hex's hex lines to the board and pulses EN for a `reset` file. To
# flash, the proxy is stopped through its STOP file, espflash writes the
# image as `just flash-m9` does, and the proxy is started again with the
# command line it had, detached, its output in <rig>/proxy.out. An lnsd on
# the proxy's pty sees its pty go away and must be restarted by whoever
# runs it.
#
#   VERDICT <PASS|FAIL> steps=.. boots=.. key_ms n=.. median=.. max=.. ...
#
# Usage:
#   bash scripts/m9-walk.sh --rig DIR --walk FILE [--image ELF] [--out DIR] [--boots N]
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
    M9_WALK_TMP="$ROOT/target/tmp" exec python3 -I "$ROOT/scripts/m9-walk.py" --selftest
fi

rig="" walk="" image="" out="" boots=2
while [ "$#" -gt 0 ]; do
    case "$1" in
        --rig) rig="${2:-}"; shift 2 ;;
        --walk) walk="${2:-}"; shift 2 ;;
        --image) image="${2:-}"; shift 2 ;;
        --out) out="${2:-}"; shift 2 ;;
        --boots) boots="${2:-}"; shift 2 ;;
        *) echo "usage: $0 --rig DIR --walk FILE [--image ELF] [--out DIR] [--boots N] | --selftest" >&2; exit 2 ;;
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

pid="$(proxy_pid)" || { echo "$TAG FAIL no m9-proxy.py runs on $rig"; exit 1; }

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

python3 -I "$ROOT/scripts/m9-walk.py" --rig "$rig" --walk "$walk" --out "$out" --boots "$boots"
rc=$?
echo "$TAG out: $out (walk.md for the agent's eye, board.log, dumps/)"
exit "$rc"
