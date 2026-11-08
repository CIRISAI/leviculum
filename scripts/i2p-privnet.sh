#!/usr/bin/env bash
# A private I2P network on loopback, for tests that must not need the internet
# (1576, measured by 1568: docs/src/concepts/i2p-interface.md, "The test bed").
#
# Starts N i2pd routers on loopback that can reach nothing but each other:
# their own netid (97), so no public router would talk to them even if one
# were reachable; `reservedrange = false`, so they accept loopback addresses
# for each other; `nat = false`, `floodfill = true` on every router, SSU2 off,
# NTCP2 published on a port of its own.
#
# Each router has its own loopback address, 127.97.0.<i>, published and bound
# (`--host`, `--address4`); SAM stays on 127.0.0.1. One shared address is not
# enough: i2pd refuses an inbound NTCP2 handshake while another from the same
# IP is pending ("Incoming session from 127.0.0.1 is already pending"), so
# routers on one IP lose links to each other at the start, when they all dial
# at once. 1576 measured it: on one address, 1 of 15 A/B runs never got a
# packet across and the slow ones carried the most refusals. No reseed server: every router starts
# once to write its router.info, stops, and all of them restart with a zip of
# those router.infos as `reseed.zipfile`. No root, no docker, no netns.
#
# Data directories go under ~/.i2pd/ and not under $TMPDIR: Debian's AppArmor
# profile for /usr/bin/i2pd (/etc/apparmor.d/usr.bin.i2pd) lets the router
# write only /var/lib/i2pd and the invoking user's ~/.i2pd, and a data
# directory in /tmp aborts it at startup with "Permission denied".
#
# Then it runs the given command with the bed in its environment and tears
# every router down on any exit. setpriv --pdeathsig ties each router to this
# script, so a SIGKILLed run leaves none behind.
#
# Usage: scripts/i2p-privnet.sh [N] -- <command> [args...]
#   N        routers, at least 2, default 5
# Environment given to the command:
#   I2P_BED       the bed's directory (r<i>/ per router, each with i2pd.log)
#   SAM_1..SAM_N  each router's SAM bridge, host:port
#   SAM_A, SAM_B  SAM_1 and SAM_2, the two the tests use
#   I2P_BED_T0    epoch seconds of the restart, for timing from it
# Knobs: LOGLEVEL (i2pd's, default warn), KEEP=1 keeps the bed directory
# (and prints it), NETID (default 97).
#
# Measured bring-up on i2pd 2.56 (1568): SAM answers 0.1 s after the restart;
# a first SESSION CREATE takes 9 to 39 s and a stream across 18 to 54 s, in
# multiples of i2pd's 3 s tunnel tick. Allow 120 s before calling a link dead.
#
# Exit status: the command's; 1 if the bed did not come up.

set -euo pipefail
export LC_ALL=C

N=5
if [ $# -gt 0 ] && [ "$1" != "--" ]; then N=$1; shift; fi
[ "${1:-}" = "--" ] && shift
[ $# -gt 0 ] || { echo "usage: $0 [N] -- <command> [args...]" >&2; exit 2; }
[ "$N" -ge 2 ] 2>/dev/null || { echo "[i2p-privnet] N must be a number >= 2, got '$N'" >&2; exit 2; }
NETID=${NETID:-97}
LOGLEVEL=${LOGLEVEL:-warn}

command -v i2pd >/dev/null || { echo "[i2p-privnet] i2pd not installed (sudo apt install i2pd)" >&2; exit 1; }
command -v setpriv >/dev/null || { echo "[i2p-privnet] setpriv not found (util-linux)" >&2; exit 1; }

say() { echo "[i2p-privnet] $*"; }
now() { date +%s.%N; }
since() { awk -v a="$1" -v b="$(now)" 'BEGIN { printf "%.1f", b - a }'; }

mkdir -p "$HOME/.i2pd"
BED="$(mktemp -d "$HOME/.i2pd/privnet.XXXXXX")"
pids=()
# shellcheck disable=SC2317  # reached through the EXIT trap below
stop_routers() {
    # SIGTERM stops i2pd at once; SIGINT would wait out its transit tunnels.
    for p in "${pids[@]}"; do kill -TERM "$p" 2>/dev/null || true; done
    for p in "${pids[@]}"; do
        for _ in $(seq 1 50); do kill -0 "$p" 2>/dev/null || break; sleep 0.1; done
        kill -KILL "$p" 2>/dev/null || true
        wait "$p" 2>/dev/null || true
    done
    pids=()
}
# shellcheck disable=SC2317
cleanup() {
    stop_routers
    if [ -n "${KEEP:-}" ]; then say "bed kept at $BED"; else rm -rf "$BED"; fi
}
trap cleanup EXIT

# Two ports per router, picked free by the kernel so parallel beds and a
# system i2pd on the default ports never collide.
free_port() { # address
    python3 -I -c 'import socket,sys; s=socket.socket(); s.bind((sys.argv[1],0)); print(s.getsockname()[1])' "$1"
}
declare -a ADDR NET_PORT SAM_PORT
for i in $(seq 1 "$N"); do
    ADDR[i]=127.97.0.$i
    NET_PORT[i]=$(free_port "${ADDR[i]}")
    SAM_PORT[i]=$(free_port 127.0.0.1)
done

CERTS=()
[ -d /usr/share/i2pd/certificates ] && CERTS=(--certsdir=/usr/share/i2pd/certificates)

start_router() { # index extra-args...
    local i=$1; shift
    local d="$BED/r$i"
    mkdir -p "$d/tunnels.d"
    printf 'reservedrange = false\nnat = false\nfloodfill = true\nipv4 = true\nipv6 = false\n' >"$d/i2pd.conf"
    : >"$d/tunnels.conf"
    setpriv --pdeathsig TERM -- i2pd --datadir="$d" --conf="$d/i2pd.conf" \
        --tunconf="$d/tunnels.conf" --tunnelsdir="$d/tunnels.d" --pidfile="$d/pid" \
        "${CERTS[@]}" \
        --log=file --logfile="$d/i2pd.log" --loglevel="$LOGLEVEL" \
        --netid="$NETID" --host="${ADDR[i]}" --address4="${ADDR[i]}" \
        --ssu2.enabled=false \
        --ntcp2.enabled=true --ntcp2.published=true --port="${NET_PORT[i]}" \
        --upnp.enabled=false \
        --http.enabled=false --httpproxy.enabled=false --socksproxy.enabled=false \
        --bob.enabled=false --i2cp.enabled=false --i2pcontrol.enabled=false \
        --sam.enabled=true --sam.address=127.0.0.1 --sam.port="${SAM_PORT[i]}" \
        "$@" >"$d/stdout" 2>&1 &
    pids+=($!)
}

T0=$(now)
# Phase 1: each router writes its router.info, then stops. The unreachable
# reseed URL keeps a router from reaching for the public reseed servers.
for i in $(seq 1 "$N"); do start_router "$i" --reseed.urls=http://127.0.0.1:1/; done
for i in $(seq 1 "$N"); do
    for _ in $(seq 1 100); do [ -s "$BED/r$i/router.info" ] && break; sleep 0.1; done
    if ! [ -s "$BED/r$i/router.info" ]; then
        echo "[i2p-privnet] router $i wrote no router.info within 10 s:" >&2
        cat "$BED/r$i/stdout" "$BED/r$i/i2pd.log" >&2 2>/dev/null || true
        exit 1
    fi
done
stop_routers

# Phase 2: every router's netDb starts empty and reseeds from the zip of all
# router.infos.
for i in $(seq 1 "$N"); do
    cp "$BED/r$i/router.info" "$BED/routerInfo-r$i.dat"
    rm -rf "$BED/r$i/netDb"
done
(cd "$BED" && python3 -I -m zipfile -c seed.zip routerInfo-r*.dat)
PHASE1=$(since "$T0")
T1=$(now)
for i in $(seq 1 "$N"); do
    start_router "$i" --reseed.zipfile="$BED/seed.zip" --reseed.threshold=1 \
        --reseed.urls=http://127.0.0.1:1/
done
for i in $(seq 1 "$N"); do
    up=0
    for _ in $(seq 1 300); do
        if (exec 3<>"/dev/tcp/127.0.0.1/${SAM_PORT[i]}") 2>/dev/null; then up=1; break; fi
        sleep 0.1
    done
    if [ "$up" != 1 ]; then
        echo "[i2p-privnet] router $i opened no SAM on ${SAM_PORT[i]} within 30 s:" >&2
        cat "$BED/r$i/stdout" "$BED/r$i/i2pd.log" >&2 2>/dev/null || true
        exit 1
    fi
    export "SAM_$i=127.0.0.1:${SAM_PORT[i]}"
done
say "$N routers on 127.97.0.1-$N, netid $NETID, bed $BED: first start ${PHASE1} s, SAM up $(since "$T1") s after the restart"

export I2P_BED="$BED" I2P_BED_T0="$T1" SAM_A="127.0.0.1:${SAM_PORT[1]}" SAM_B="127.0.0.1:${SAM_PORT[2]}"
rc=0
"$@" || rc=$?
say "command exit $rc, $(since "$T1") s after the restart"
if [ "$rc" != 0 ]; then
    for i in $(seq 1 "$N"); do
        echo "== router $i log tail" >&2
        tail -n 15 "$BED/r$i/i2pd.log" >&2 2>/dev/null || true
    done
fi
exit "$rc"
