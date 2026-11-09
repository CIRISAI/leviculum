#!/usr/bin/env bash
#
# The bench M9's own mailbox (order 1562). The bench board is never a
# device of a real home: on 2026-11-08 the walks of windows 11 to 15 ran on
# Lew's M9 joined to his mailbox, opened his chats and sent read marks for
# messages he never read (`HOME mark read device=940b1536...` at 09:06), and
# his contacts saw the ticks. The board walks against a `loquax mailbox` on
# the bench host instead: an identity of its own, the 1489 fixture as its
# archive (300 conversations, 4000 messages; `loquax-m9-sim --seed-home`),
# its admission codes issued here and never Lew's, and a stock Python LXMF
# sender for live arrivals. scripts/m9-walk.sh refuses a walk that opens
# chats while the board's client record names any other home.
#
# Lew's pairing comes back with `lnflash --home` or a code from his own
# mailbox, when he wants his chats on the board again; the two never mix.
#
# Usage:
#   bash scripts/m9-bench-home.sh setup [--dir DIR]
#       the identity and the fixture into DIR (a second setup changes
#       nothing), DIR/address the home's LXMF address; prints
#       BENCH_HOME dir=.. address=.. appended=.. held=..
#   bash scripts/m9-bench-home.sh run [--dir DIR] [--config RNSDIR]
#       `loquax mailbox run` on DIR beside the bench's lnsd (its shared
#       instance; RNSDIR as lnsd's --config), detached, its log in
#       DIR/mailbox.log and its pid in DIR/mailbox.pid; waits for HOME up
#   bash scripts/m9-bench-home.sh stop [--dir DIR]
#   bash scripts/m9-bench-home.sh code [--dir DIR]
#       a fresh single-use admission code, printed and in DIR/code
#   bash scripts/m9-bench-home.sh join --rig RIG [--dir DIR]
#       a fresh code written to the board through the rig folder's proxy
#       (TYPE_HOME_CODE, scripts/m9-walk.py --home-code): waits for
#       `[HOME] code=taken` and the client line naming the bench home; the
#       board then joins over its node (the admission is in mailbox.log)
#   bash scripts/m9-bench-home.sh send [--dir DIR] [--config RNSDIR] <words>
#       a live arrival: the reference LXMF client (reference/LXMF over
#       reference/Reticulum) on the bench's shared instance delivers
#       <words> to the bench home (scripts/m9-bench-send.py)
#   bash scripts/m9-bench-home.sh --selftest
#       setup and code in a scratch folder, offline: the code's home key
#       is DIR/address's, a second setup appends nothing, and another
#       folder's code is not (the control)
#
# DIR defaults to ${XDG_STATE_HOME:-$HOME/.local/state}/leviculum/m9-bench-home,
# the folder scripts/m9-walk.sh reads the bench home's address from.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAG="[m9-bench-home]"
DEFAULT_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/leviculum/m9-bench-home"

# build_bin <cargo args...>: the release binary cargo builds, by its path.
build_bin() {
    (cd "$ROOT" && cargo build --release --message-format=json "$@" 2>"$ROOT/target/m9-bench-home-build.log") |
        python3 -I -c '
import json, sys
path = ""
for line in sys.stdin:
    try:
        m = json.loads(line)
    except ValueError:
        continue
    if m.get("reason") == "compiler-artifact" and m.get("executable"):
        path = m["executable"]
print(path)'
}

sim_bin() { build_bin -p loquax-m9 --features sim --bin loquax-m9-sim; }
loquax_bin() { build_bin -p loquax --features tui --bin loquax; }

need() {
    [ -n "$1" ] && [ -x "$1" ] && return 0
    echo "$TAG FAIL the build did not make $2 (see $ROOT/target/m9-bench-home-build.log)"
    exit 1
}

setup() {
    local dir="$1" sim line
    sim="$(sim_bin)"; need "$sim" loquax-m9-sim
    line="$("$sim" --seed-home "$dir")" || { echo "$TAG FAIL the seed: $line"; exit 1; }
    local address="${line#*address=}"; address="${address%% *}"
    echo "$address" >"$dir/address"
    echo "BENCH_HOME dir=$dir ${line#BENCH_HOME }"
}

code() {
    local dir="$1" loquax out text
    loquax="$(loquax_bin)"; need "$loquax" loquax
    [ -f "$dir/address" ] || { echo "$TAG FAIL $dir is no bench home (setup first)"; exit 1; }
    out="$(LNMSG_HOME="$dir" "$loquax" mailbox admit --name m9-bench 2>&1)" || {
        echo "$out"; echo "$TAG FAIL loquax mailbox admit"; exit 1; }
    text="$(grep -o 'lqxh1[^[:space:]]*' <<<"$out" | head -n 1)"
    [ -n "$text" ] || { echo "$out"; echo "$TAG FAIL no code in admit's answer"; exit 1; }
    echo "$text" >"$dir/code"
    echo "$text"
}

# code_address <code>: the LXMF address of the home a code names
# (lqxh1 + base32 of public key, token, check; the delivery destination
# lxmf.delivery of that identity, as RNS hashes it).
code_address() {
    python3 -I -c '
import base64, hashlib, sys
body = sys.argv[1].split("@", 1)[0][len("lqxh1"):].upper()
raw = base64.b32decode(body + "=" * (-len(body) % 8))
key = raw[:64]
identity = hashlib.sha256(key).digest()[:16]
name = hashlib.sha256(b"lxmf.delivery").digest()[:10]
print(hashlib.sha256(name + identity).digest()[:16].hex())' "$1"
}

running_pid() {
    local dir="$1" pid
    pid="$(cat "$dir/mailbox.pid" 2>/dev/null)" || return 1
    kill -0 "$pid" 2>/dev/null || return 1
    echo "$pid"
}

run() {
    local dir="$1" config="$2" loquax pid lines
    loquax="$(loquax_bin)"; need "$loquax" loquax
    [ -f "$dir/address" ] || { echo "$TAG FAIL $dir is no bench home (setup first)"; exit 1; }
    if pid="$(running_pid "$dir")"; then
        echo "$TAG the bench home already runs, pid $pid"
        return 0
    fi
    lines=0
    [ -f "$dir/mailbox.log" ] && lines="$(wc -l <"$dir/mailbox.log")"
    local args=(mailbox run)
    [ -n "$config" ] && args+=(--config "$config")
    LNMSG_HOME="$dir" setsid "$loquax" "${args[@]}" >>"$dir/mailbox.log" 2>&1 </dev/null &
    echo $! >"$dir/mailbox.pid"
    for _ in $(seq 300); do
        if tail -n +"$((lines + 1))" "$dir/mailbox.log" | grep -q "HOME up"; then
            echo "$TAG the bench home runs, pid $(cat "$dir/mailbox.pid"), address $(cat "$dir/address"), log $dir/mailbox.log"
            return 0
        fi
        running_pid "$dir" >/dev/null || break
        sleep 0.1
    done
    tail -n 20 "$dir/mailbox.log"
    echo "$TAG FAIL the bench home did not say HOME up within 30 s"
    exit 1
}

stop() {
    local dir="$1" pid
    if pid="$(running_pid "$dir")"; then
        kill "$pid"
        echo "$TAG stopped pid $pid"
    else
        echo "$TAG not running"
    fi
}

join() {
    local dir="$1" rig="$2" text
    [ -d "$rig" ] || { echo "$TAG FAIL join needs --rig <folder> with the board's proxy"; exit 2; }
    text="$(code "$dir")" || { echo "$text"; exit 1; }
    python3 -I "$ROOT/scripts/m9-walk.py" --rig "$rig" --home-code "$text" --bench-home "$dir/address"
}

send() {
    local dir="$1" config="$2"; shift 2
    [ -f "$dir/address" ] || { echo "$TAG FAIL $dir is no bench home (setup first)"; exit 1; }
    local args=(--to "$(cat "$dir/address")" --text "$*" --identity "$dir/sender-identity"
                --address-out "$dir/sender-address")
    [ -n "$config" ] && args+=(--rnsconfig "$config")
    PYTHONPATH="$ROOT/reference/Reticulum:$ROOT/reference/LXMF" \
        python3 "$ROOT/scripts/m9-bench-send.py" "${args[@]}"
}

selftest() {
    local base a b ca cb
    mkdir -p "$ROOT/target/tmp"
    base="$(mktemp -d "$ROOT/target/tmp/m9-bench-home.XXXXXX")"
    # shellcheck disable=SC2064 # the folder now, not when the trap fires
    trap "rm -rf '$base'" EXIT
    setup "$base/a" >"$base/a.setup" || { cat "$base/a.setup"; echo "$TAG selftest FAIL setup"; exit 1; }
    cat "$base/a.setup"
    grep -q "appended=[1-9]" "$base/a.setup" || { echo "$TAG selftest FAIL the fixture was not appended"; exit 1; }
    setup "$base/a" >"$base/a.again"
    grep -q "appended=0 " "$base/a.again" || { cat "$base/a.again"; echo "$TAG selftest FAIL a second setup appended"; exit 1; }
    a="$(cat "$base/a/address")"
    ca="$(code "$base/a")" || { echo "$ca"; echo "$TAG selftest FAIL code"; exit 1; }
    [ "$(code_address "$ca")" = "$a" ] || {
        echo "$TAG selftest FAIL the code names $(code_address "$ca"), the home is $a"; exit 1; }
    # The control: another bench home's code names another address.
    setup "$base/b" >/dev/null || { echo "$TAG selftest FAIL setup b"; exit 1; }
    cb="$(code "$base/b")" || { echo "$TAG selftest FAIL code b"; exit 1; }
    b="$(cat "$base/b/address")"
    if [ "$(code_address "$cb")" != "$b" ] || [ "$b" = "$a" ]; then
        echo "$TAG selftest FAIL the control: home b $b, its code $(code_address "$cb"), home a $a"; exit 1
    fi
    echo "$TAG selftest ok: setup seeded home $a, a second setup appended nothing, its code names it, another home's code names $b"
}

if [ "${1:-}" = "--selftest" ]; then
    selftest
    exit 0
fi

verb="${1:-}"; shift || true
dir="$DEFAULT_DIR" rig="" config=""
words=()
while [ "$#" -gt 0 ]; do
    case "$1" in
        --dir) dir="${2:-}"; shift 2 ;;
        --rig) rig="${2:-}"; shift 2 ;;
        --config) config="${2:-}"; shift 2 ;;
        *) words+=("$1"); shift ;;
    esac
done
mkdir -p "$dir" || exit 1
dir="$(cd "$dir" && pwd)"
case "$verb" in
    setup) setup "$dir" ;;
    run) run "$dir" "$config" ;;
    stop) stop "$dir" ;;
    code) code "$dir" ;;
    join) join "$dir" "$rig" ;;
    send)
        [ "${#words[@]}" -gt 0 ] || { echo "$TAG send what?"; exit 2; }
        send "$dir" "$config" "${words[@]}" ;;
    *) sed -n '/^# Usage:/,/^# DIR defaults/p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
