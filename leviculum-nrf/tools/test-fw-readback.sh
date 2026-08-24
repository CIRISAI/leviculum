#!/usr/bin/env bash
# Fixture test for firmware read-back attribution (tools/fw-readback.sh).
#
# Same apparatus as tools/test-uf2-volumes.sh: everything that touches the
# machine is a stub, so the decision under test runs with no board, no udev and
# no sudo. What is stubbed here is one layer further along than volume
# selection — WHICH BOARD ended up with the image the runner wrote — so the
# stubs are a set of fake boards, each with a serial and a firmware stamp it
# will report when its debug port is read.
#
# The scenario every case here descends from is Codeberg #343, measured on the
# rig 2026-08-23 and again 2026-08-24: two T114s attached, one parked in its
# UF2 bootloader and one running. The volume took the image, the runner named
# the other board, and `flash CONFIRMED` was printed on the strength of "a
# board of this type re-enumerated". Both nights the summary named the board
# that had NOT been flashed, and the two nights named opposite boards — the
# attribution follows enumeration order, which is neither stable nor related
# to which board was in the bootloader.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
if [ ! -f "$SCRIPT_DIR/fw-readback.sh" ]; then
    printf 'FAIL  tools/fw-readback.sh is missing; there is no read-back to test\n'
    printf '\n0 passed, 1 failed\n'
    exit 1
fi
# shellcheck source=leviculum-nrf/tools/fw-readback.sh
. "$SCRIPT_DIR/fw-readback.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

PASS=0
FAIL=0

ok() {
    PASS=$((PASS + 1))
    printf 'ok    %s\n' "$1"
}
bad() {
    FAIL=$((FAIL + 1))
    printf 'FAIL  %s\n' "$1"
}

# Args: $1 = what, $2 = want, $3 = got
check_eq() {
    if [ "$2" = "$3" ]; then
        ok "$1"
    else
        bad "$1"
        printf '        want %q\n        got  %q\n' "$2" "$3"
    fi
}

# Args: $1 = what, $2 = required substring, $3 = got
check_contains() {
    if [[ "$3" == *"$2"* ]]; then
        ok "$1"
    else
        bad "$1"
        printf '        want substring %q\n        got  %q\n' "$2" "$3"
    fi
}

# Args: $1 = what, $2 = forbidden substring, $3 = got
check_lacks() {
    if [[ "$3" != *"$2"* ]]; then
        ok "$1"
    else
        bad "$1"
        printf '        forbidden substring %q\n        got  %q\n' "$2" "$3"
    fi
}

# --- The fake rig -----------------------------------------------------------
# BOARDS is one "<serial>\t<stamp>\t<silent-windows>" per line:
#   stamp           what its [FW_BUILD] line carries ("" = it has no debug port)
#   silent-windows  how many read windows pass before it answers, so a slow
#                   board can be told apart from a mute one
# READS counts read windows, which is what the timing control asserts on.

BOARDS=""
READS="$WORK/reads"
: >"$READS"

board() { BOARDS="$BOARDS$1	$2	${3:-0}"$'\n'; }

board_field() {
    local want="$1" col="$2" s st sw
    while IFS=$'\t' read -r s st sw; do
        if [ "$s" = "$want" ]; then
            case "$col" in
            stamp) printf '%s' "$st" ;;
            silent) printf '%s' "$sw" ;;
            esac
            return 0
        fi
    done <<<"$BOARDS"
    return 0
}

fw_debug_ports() {
    local s st sw
    while IFS=$'\t' read -r s st sw; do
        [ -n "$s" ] || continue
        # A board with no debug port is enumerable but unreadable: the port
        # never appears. Distinct from a board whose port is there and mute.
        [ "$st" = "(noport)" ] && continue
        printf '%s\t%s\n' "$s" "$WORK/port-$s"
    done <<<"$BOARDS" | sort
    return 0
}

# One read window against one fake board. Silence for the first
# <silent-windows> windows, then the board's banner — the same shape a board
# that boots slowly presents to the real reader.
fw_read_banner() {
    local port="$1" serial="${1##*/port-}" stamp silent n
    printf '%s\n' "$port" >>"$READS"
    stamp="$(board_field "$serial" stamp)"
    silent="$(board_field "$serial" silent)"
    [ -n "$stamp" ] || return 0
    [ "$stamp" = "(mute)" ] && return 0
    n="$(grep -c "^$port\$" "$READS")"
    [ "$n" -le "${silent:-0}" ] && return 0
    printf '[INFO!] [FW_BUILD] %s\n' "$stamp"
    return 0
}

reads_for() { grep -c "/port-$1\$" "$READS"; }

reset_scenario() {
    BOARDS=""
    : >"$READS"
    FW_ATTRIBUTED=""
    FW_ATTR_OUTCOME=""
    FW_ATTR_SERIAL=""
    FW_ATTR_STAMP=""
    FW_ATTR_MESSAGE=""
    FW_READ_WINDOW=1
    FW_READ_ATTEMPTS=3
    FW_READ_OTHER_ATTEMPTS=1
}

# The two rig boards, named as they are in #343.
A=183004F712B4A7FE
B=DEC9947DAD9D2869
NEW='git_sha=0269dbf dirty=false'
OLD='git_sha=50d8133 dirty=false'

# --- 0. The expected stamp comes out of the image, not out of git -----------
# `git rev-parse` answers about the working tree; the runner has to ask about
# the bytes it is writing. A flat image built at one commit must still name
# that commit after HEAD has moved, and a dirty build must not read as clean.

reset_scenario
IMG="$WORK/firmware.bin"
head -c 4096 /dev/zero >"$IMG"
printf 'leviculum T114 booting\0[FW_BUILD] %s\0' "$NEW" >>"$IMG"
head -c 4096 /dev/zero >>"$IMG"
check_eq "the stamp is read out of the image" "$NEW" "$(fw_image_stamp "$IMG")"
check_eq "...and its sha half is available for messages" "0269dbf" \
    "$(fw_stamp_sha "$(fw_image_stamp "$IMG")")"

DIRTY_IMG="$WORK/dirty.bin"
printf '\0\0[FW_BUILD] git_sha=0269dbf dirty=true\0\0' >"$DIRTY_IMG"
check_eq "a dirty build is a different stamp from the clean one at that commit" \
    "git_sha=0269dbf dirty=true" "$(fw_image_stamp "$DIRTY_IMG")"
check_eq "an image with no stamp yields nothing rather than a guess" "" \
    "$(fw_image_stamp "/dev/null")"
check_eq "a missing image yields nothing" "" "$(fw_image_stamp "$WORK/absent.bin")"

# --- 1. Two candidates, the image went to the other one ---------------------
# THE RED TEST, and the ticket's own acceptance case. The runner names a board
# it picked out of its own enumeration; the image is on the other one. Nothing
# in the old code reads a board back, so the old answer is whichever candidate
# enumeration reached first and the summary is wrong half the time.

reset_scenario
board "$A" "$OLD"
board "$B" "$NEW"
fw_attribute "(test)" "$NEW" "$A"
check_eq "the board that got the image is the board that is named" "$B" "$FW_ATTR_SERIAL"
check_eq "...and the outcome says the naming was corrected" "rebound" "$FW_ATTR_OUTCOME"
check_contains "the message names the board that actually has it" \
    "the image $NEW is on serial=$B" "$FW_ATTR_MESSAGE"
check_contains "...and says the named board does not" \
    "not on serial=$A" "$FW_ATTR_MESSAGE"
check_lacks "...and does not report this as a confirmed flash of the named board" \
    "CONFIRMED — serial=$A" "$FW_ATTR_MESSAGE"

# The inverse direction, measured the following night. The fix must not have a
# preferred board any more than the defect had a fixed bias.
reset_scenario
board "$A" "$NEW"
board "$B" "$OLD"
fw_attribute "(test)" "$NEW" "$B"
check_eq "the same holds with the two boards swapped" "$A" "$FW_ATTR_SERIAL"

# --- 2. The read-back does not match: a failure, and both shas are named ----

reset_scenario
board "$A" "$OLD"
fw_attribute "(test)" "$NEW" "$A"
check_eq "a board carrying a different image is not a confirmed flash" "mismatch" "$FW_ATTR_OUTCOME"
check_contains "the wording names what the board reports" "reports $OLD" "$FW_ATTR_MESSAGE"
check_contains "...and what was written" "the image that was written is $NEW" "$FW_ATTR_MESSAGE"
check_contains "...and reads as a failure, not a success" "NOT CONFIRMED" "$FW_ATTR_MESSAGE"

# Same commit, dirty tree: a different image, and confirming only the sha
# would call the two the same one.
reset_scenario
board "$A" "git_sha=0269dbf dirty=true"
fw_attribute "(test)" "$NEW" "$A"
check_eq "a dirty build of the same commit is not the clean image" "mismatch" "$FW_ATTR_OUTCOME"

# --- 3. The board does not answer: unknown, and not a mismatch --------------
# Two ways to be silent, and neither may be reported as wrong firmware: a
# board whose debug port never appears, and a board whose port is there and
# says nothing (crashed firmware).

reset_scenario
board "$A" "(noport)"
fw_attribute "(test)" "$NEW" "$A"
check_eq "a board with no debug port is unconfirmed, not mismatched" "noanswer" "$FW_ATTR_OUTCOME"
check_contains "...and the wording says so in as many words" \
    "did not answer on its debug port" "$FW_ATTR_MESSAGE"
check_contains "...and separates itself from the wrong-image case" \
    "not the same as carrying the wrong image" "$FW_ATTR_MESSAGE"
check_lacks "...and claims nothing about which image it carries" "reports git_sha" "$FW_ATTR_MESSAGE"

reset_scenario
board "$A" "(mute)"
fw_attribute "(test)" "$NEW" "$A"
check_eq "a board with a silent debug port is unconfirmed too" "noanswer" "$FW_ATTR_OUTCOME"
check_eq "...after spending its whole read budget on it" "3" "$(reads_for "$A")"

# A silent NAMED board must still not stop the image being traced to the board
# that has it — silence about A is not evidence about B.
reset_scenario
board "$A" "(mute)"
board "$B" "$NEW"
fw_attribute "(test)" "$NEW" "$A"
check_eq "a silent named board does not hide the board that has the image" "$B" "$FW_ATTR_SERIAL"
check_eq "...and the outcome is the corrected naming" "rebound" "$FW_ATTR_OUTCOME"
check_contains "...and the message says the named board was silent" \
    "serial=$A did not answer" "$FW_ATTR_MESSAGE"

# --- 4. One candidate, everything normal: still green (control) -------------

reset_scenario
board "$A" "$NEW"
fw_attribute "(test)" "$NEW" "$A"
check_eq "the ordinary single-board flash is confirmed" "match" "$FW_ATTR_OUTCOME"
check_eq "...naming the board that was flashed" "$A" "$FW_ATTR_SERIAL"
check_contains "...on the strength of the read-back" "read back from the board" "$FW_ATTR_MESSAGE"
check_eq "...and one read window is enough for a healthy board" "1" "$(reads_for "$A")"

# Two boards, both already carrying the image, one of them just flashed: the
# confirmed board is the named one and the other is never consulted.
reset_scenario
board "$A" "$NEW"
board "$B" "$NEW"
fw_attribute "(test)" "$NEW" "$A"
check_eq "a sibling on the same image does not make the answer ambiguous" "match" "$FW_ATTR_OUTCOME"
check_eq "...and the sibling is not even read" "0" "$(reads_for "$B")"

# --- 5. A slow board still passes inside the existing budget (control) ------
# A board that has just re-enumerated can miss a read window: udev lags, the
# boot lands mid-window. That is why the named board gets more than one. This
# case is the guard against a later change quietly reducing the budget to one
# window and turning every slow boot into "did not answer".

reset_scenario
board "$A" "$NEW" 2 # silent for two windows, answers in the third
fw_attribute "(test)" "$NEW" "$A"
check_eq "a slow board is confirmed, not reported unreadable" "match" "$FW_ATTR_OUTCOME"
check_eq "...naming the right board" "$A" "$FW_ATTR_SERIAL"
check_eq "...within the budget and no further" "3" "$(reads_for "$A")"

# --- 6. Nothing can be bound to a board: say exactly that -------------------
# The crashed-firmware recovery pass writes to a volume with no candidate
# behind it at all. With nobody reporting the image, the honest summary is
# that the runner does not know which board it wrote.

reset_scenario
board "$A" "$OLD"
board "$B" "$OLD"
fw_attribute "(crashed-recovery)" "$NEW" ""
check_eq "an unbindable write is not attributed to anyone" "ambiguous" "$FW_ATTR_OUTCOME"
check_eq "...and names no board" "" "$FW_ATTR_SERIAL"
check_contains "...and says so" "does not know which board it wrote" "$FW_ATTR_MESSAGE"
check_contains "...and names the boards it asked" "$A" "$FW_ATTR_MESSAGE"

# Recovery with exactly one board reporting the image: that IS a binding, and
# it is the only way the recovery pass can ever name a board truthfully.
reset_scenario
board "$A" "$OLD"
board "$B" "$NEW"
fw_attribute "(crashed-recovery)" "$NEW" ""
check_eq "a recovery write with one reporter is bound to that board" "rebound" "$FW_ATTR_OUTCOME"
check_eq "...naming it" "$B" "$FW_ATTR_SERIAL"

# Two boards reporting the image and no named candidate: which one took THIS
# write is unknowable, and a guess is what produced the ticket.
reset_scenario
board "$A" "$NEW"
board "$B" "$NEW"
fw_attribute "(crashed-recovery)" "$NEW" ""
check_eq "two reporters and no named board is unattributed" "ambiguous" "$FW_ATTR_OUTCOME"
check_contains "...and the message names both" "$A" "$FW_ATTR_MESSAGE"
check_contains "...and both" "$B" "$FW_ATTR_MESSAGE"

# --- 7. A board already bound to this image is not offered again ------------
# Flashing two boards with one image: after A is confirmed, B's write must not
# be able to point back at A. Without the exclusion every write after the first
# would look ambiguous, which reads as "we do not know" about a run that went
# perfectly.

reset_scenario
board "$A" "$NEW"
board "$B" "$NEW"
fw_attribute "(test)" "$NEW" "$A"
check_eq "the first board is confirmed" "match" "$FW_ATTR_OUTCOME"
fw_attribute "(test)" "$NEW" "$B"
check_eq "the second board is confirmed on its own read-back" "match" "$FW_ATTR_OUTCOME"
check_eq "...and named as itself" "$B" "$FW_ATTR_SERIAL"

# And with the second board silent, the already-bound first one is not
# offered up as the recipient of the second write.
reset_scenario
board "$A" "$NEW"
board "$B" "(mute)"
fw_attribute "(test)" "$NEW" "$A"
fw_attribute "(test)" "$NEW" "$B"
check_eq "an already-bound board is not re-used to explain a later write" \
    "noanswer" "$FW_ATTR_OUTCOME"

# --- 8. An image with no stamp confirms nothing, and says why ---------------
# Not a failure of the board: a failure of the question. Reporting it as a
# mismatch would blame hardware for a missing build stamp.

reset_scenario
board "$A" "$NEW"
fw_attribute "(test)" "" "$A"
check_eq "an unstamped image cannot confirm anything" "nostamp" "$FW_ATTR_OUTCOME"
check_contains "...and says the image is what is missing" \
    "the image carries no [FW_BUILD] stamp" "$FW_ATTR_MESSAGE"
check_eq "...without reading a board to find that out" "0" "$(reads_for "$A")"

# --- 9. The reader really does read a serial port ---------------------------
# Everything above stubs fw_read_banner. This case runs the real one against a
# pty, so the parsing (CRLF, the last banner winning, the timeout) is covered
# rather than assumed. What a pty cannot cover is the DTR/RTS assertion, which
# stays a rig property; the ioctl is exercised, its effect is not.

reset_scenario
PTY_OUT="$WORK/pty-name"
python3 - "$PTY_OUT" <<'PY' &
import os, pty, sys, time
master, slave = pty.openpty()
open(sys.argv[1], 'w').write(os.ttyname(slave) + '\n')
deadline = time.monotonic() + 8
os.write(master, b'[INFO!] [TIME_SOURCE] source=uptime-only\r\n')
os.write(master, b'[INFO!] [FW_BUILD] git_sha=deadbee dirty=false\r\n')
time.sleep(0.3)
os.write(master, b'[INFO!] [FW_BUILD] git_sha=0269dbf dirty=false\r\n')
while time.monotonic() < deadline:
    time.sleep(0.1)
PY
PTY_PID=$!
PTY_DEV=""
for _ in 1 2 3 4 5 6 7 8 9 10; do
    [ -s "$PTY_OUT" ] && PTY_DEV="$(cat "$PTY_OUT")" && break
    sleep 0.2
done
# Put the real reader back over the stub. Re-sourcing rather than `unset -f`:
# the stub replaced the definition, so unsetting would leave no reader at all
# and the case would pass on an empty result from a command that is not there.
# shellcheck source=leviculum-nrf/tools/fw-readback.sh
. "$SCRIPT_DIR/fw-readback.sh"
if [ -n "$PTY_DEV" ]; then
    PTY_BANNER="$(fw_read_banner "$PTY_DEV" 2)"
    check_eq "the real reader returns the LAST banner seen, CR stripped" \
        "[INFO!] [FW_BUILD] git_sha=0269dbf dirty=false" "$PTY_BANNER"
    check_eq "a port that says nothing in the window yields nothing" "" \
        "$(fw_read_banner "$WORK/absent-port" 1)"
else
    bad "a pty could not be opened, so the real reader was not exercised"
fi
kill "$PTY_PID" 2>/dev/null
wait "$PTY_PID" 2>/dev/null

# --- Summary ----------------------------------------------------------------

printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
