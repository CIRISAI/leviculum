#!/usr/bin/env bash
#
# Gate: the RNode flash commands name the chip the board actually carries,
# and the flash offsets that belong to that chip.
#
# Until 2026-09-17 every recipe said `--chip esp32` and read the bootloader
# at 0x1000. The Heltec V4 is an ESP32-S3, whose bootloader lives at 0x0 and
# whose esptool refuses the wrong --chip outright, so the V4 could be neither
# extracted nor restored. The recipes looked correct because the two T-Beams
# are plain ESP32 and a constant that is right for the boards you happen to
# use reads exactly like a fact.
#
# What is asserted is the COMPOSED COMMAND LINE, not that some variable
# exists. A `--chip {{chip}}` that resolves to esp32 for every board would
# satisfy any check that only looked for the variable, and would leave the
# V4 exactly as unrecoverable as before.
#
# The commands are obtained by running the real `just` recipes with
# RNODE_DRY_RUN=1, which prints what would be run and touches neither a port
# nor the filesystem. So the gate covers the whole chain the bench uses —
# recipe, board argument, region table — and not a re-implementation of it.
#
# Usage:
#   check-rnode-chip-offsets.sh              # gate
#   check-rnode-chip-offsets.sh --self-test  # positive control
#
# The positive control patches the S3 bootloader offset back to 0x1000 in a
# throwaway copy of scripts/rnode-flash.sh and requires the assertions to go
# red. A gate nobody has seen fail is not known to be a gate.

set -uo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_DIR" || exit 1

PORT=/dev/ttyACM-nonexistent
FW=/tmp/rnode-fw-gate
RNODE_FLASH="${RNODE_FLASH:-$REPO_DIR/scripts/rnode-flash.sh}"
FAILURES=0
QUIET="${QUIET:-0}"

note() { [[ "$QUIET" == "1" ]] || echo "$@"; }

fail() {
    FAILURES=$((FAILURES + 1))
    echo "  FAIL: $1" >&2
    [[ $# -lt 2 ]] || printf '    in:\n%s\n' "$2" | sed 's/^/    /' >&2
}

# expect <label> <needle> <haystack>
expect() {
    if [[ "$3" == *"$2"* ]]; then
        note "  ok: $1"
    else
        fail "$1 — missing: $2" "$3"
    fi
}

# refute <label> <needle> <haystack>
refute() {
    if [[ "$3" == *"$2"* ]]; then
        fail "$1 — present but must not be: $2" "$3"
    else
        note "  ok: $1"
    fi
}

compose() { # <action> <board> [extra args...]
    local action="$1" board="$2"
    shift 2
    RNODE_DRY_RUN=1 bash "$RNODE_FLASH" "$action" \
        --port "$PORT" --board "$board" --fw-dir "$FW" "$@" 2>&1
}

# The composition claims, run against whichever rnode-flash.sh $RNODE_FLASH
# points at — the tree's, or the deliberately broken copy the self-test makes.
check_composition() {
    local out

    note "ESP32-S3 board (heltec-v4): chip esp32s3, bootloader at 0x0"
    out="$(compose extract heltec-v4)"
    expect "extract reads the bootloader at 0x0" \
        "--chip esp32s3 --port $PORT --baud 921600 read-flash 0x0 0x8000 $FW/bootloader.bin" "$out"
    expect "extract reads the partition table at 0x8000" \
        "--chip esp32s3 --port $PORT --baud 921600 read-flash 0x8000 0xc00 $FW/partitions.bin" "$out"
    refute "no esp32 chip argument anywhere in an S3 extract" "--chip esp32 " "$out"
    # Trailing space: without it the needle also matches the app region at
    # 0x10000, and the assertion would be red on a correct composition.
    refute "the ESP32 bootloader offset never appears for an S3" "read-flash 0x1000 " "$out"

    out="$(compose write heltec-v4)"
    expect "write puts the bootloader back at 0x0" \
        "--chip esp32s3 --port $PORT --baud 921600 --before default-reset --after hard-reset write-flash --flash-mode dio --flash-freq 80m --flash-size detect 0x0 $FW/bootloader.bin" "$out"
    refute "no esp32 chip argument in an S3 write" "--chip esp32 " "$out"

    note "ESP32 board (tbeam): chip esp32, bootloader at 0x1000"
    out="$(compose extract tbeam)"
    expect "extract reads the bootloader at 0x1000" \
        "--chip esp32 --port $PORT --baud 921600 read-flash 0x1000 0x4650 $FW/bootloader.bin" "$out"
    refute "no esp32s3 chip argument in an ESP32 extract" "--chip esp32s3" "$out"
    refute "the S3 bootloader offset never appears for an ESP32" "read-flash 0x0 " "$out"

    out="$(compose write tbeam)"
    expect "write puts the bootloader back at 0x1000" \
        "--chip esp32 --port $PORT --baud 921600 --before default-reset --after hard-reset write-flash --flash-mode dio --flash-freq 80m --flash-size detect 0x1000 $FW/bootloader.bin" "$out"
    refute "no esp32s3 chip argument in an ESP32 write" "--chip esp32s3" "$out"

    note "full-image restore path"
    out="$(compose read-image heltec-v4 --image /tmp/gate-image.bin)"
    expect "the whole flash goes into one image" \
        "--chip esp32s3 --port $PORT --baud 921600 read-flash 0x0 ALL /tmp/gate-image.bin" "$out"
    out="$(compose write-image tbeam --image /tmp/gate-image.bin)"
    expect "an image is written back verbatim, size kept" \
        "--chip esp32 --port $PORT --baud 921600 --before default-reset --after hard-reset write-flash --flash-size keep 0x0 /tmp/gate-image.bin" "$out"

    note "a chip name passes straight through"
    out="$(compose extract esp32s3)"
    expect "--chip esp32s3 given by name still reads 0x0" \
        "--chip esp32s3 --port $PORT --baud 921600 read-flash 0x0 0x8000 $FW/bootloader.bin" "$out"
}

# Refusals. A wrong answer must be an error, never a default that quietly
# flashes the offsets of some other chip.
check_refusals() {
    local out rc

    out="$(compose extract wisblock)"; rc=$?
    if [[ $rc -ne 0 ]]; then
        note "  ok: an unknown board is refused"
    else
        fail "an unknown board must be refused" "$out"
    fi
    expect "the refusal names the boards it knows" "unknown board 'wisblock'" "$out"

    out="$(RNODE_DRY_RUN=1 bash "$RNODE_FLASH" extract --port "$PORT" 2>&1)"; rc=$?
    if [[ $rc -ne 0 ]]; then
        note "  ok: a dry run without a board is refused"
    else
        fail "a dry run cannot probe and must say so" "$out"
    fi

    out="$(compose extract esp32c3)"; rc=$?
    if [[ $rc -ne 0 ]]; then
        note "  ok: a family with no measured region map is refused"
    else
        fail "an unmeasured chip family must be refused, not guessed at" "$out"
    fi
    refute "an unmeasured family gets no invented offsets" "read-flash 0x" "$out"
}

# The recipes themselves, not just the script under them: a board argument
# that the recipe drops on the floor would pass check_composition and still
# leave `just flash-rnode /dev/ttyACM6 heltec-v4` flashing an ESP32 layout.
check_recipes() {
    local out
    command -v just >/dev/null 2>&1 || { fail "just is not installed"; return; }

    out="$(RNODE_DRY_RUN=1 just flash-rnode-extract "$PORT" heltec-v4 2>&1)"
    expect "just flash-rnode-extract passes the S3 board through" \
        "--chip esp32s3 --port $PORT --baud 921600 read-flash 0x0 0x8000 " "$out"
    out="$(RNODE_DRY_RUN=1 just flash-rnode-extract "$PORT" tbeam 2>&1)"
    expect "just flash-rnode-extract passes the ESP32 board through" \
        "--chip esp32 --port $PORT --baud 921600 read-flash 0x1000 0x4650 " "$out"
    out="$(RNODE_DRY_RUN=1 just flash-rnode "$PORT" heltec-v4 2>&1)"
    expect "just flash-rnode writes an S3 bootloader at 0x0" \
        "--flash-size detect 0x0 " "$out"
    out="$(RNODE_DRY_RUN=1 just flash-rnode "$PORT" tbeam 2>&1)"
    expect "just flash-rnode writes an ESP32 bootloader at 0x1000" \
        "--flash-size detect 0x1000 " "$out"
    out="$(RNODE_DRY_RUN=1 just flash-rnode-write-image "$PORT" /tmp/gate-image.bin heltec-v4 2>&1)"
    expect "just flash-rnode-write-image restores a full image" \
        "--chip esp32s3 --port $PORT --baud 921600 --before default-reset --after hard-reset write-flash --flash-size keep 0x0 /tmp/gate-image.bin" "$out"
}

if [[ "${1:-}" == "--self-test" ]]; then
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    sed 's/^bootloader 0x0 0x8000$/bootloader 0x1000 0x8000/' \
        scripts/rnode-flash.sh > "$tmp/rnode-flash.sh"
    if ! grep -q '^bootloader 0x1000 0x8000$' "$tmp/rnode-flash.sh"; then
        echo "SELF-TEST BROKEN: the S3 bootloader row was not patched" >&2
        exit 1
    fi
    RNODE_FLASH="$tmp/rnode-flash.sh"
    QUIET=1
    FAILURES=0
    check_composition 2>/dev/null
    if [[ $FAILURES -gt 0 ]]; then
        echo "self-test: an S3 bootloader back at 0x1000 is caught ($FAILURES assertions red)"
        exit 0
    fi
    echo "SELF-TEST FAILED: the gate passed a composition it must reject" >&2
    exit 1
elif [[ $# -gt 0 ]]; then
    echo "ERROR: unknown argument '$1'" >&2
    echo "Usage: $0 [--self-test]" >&2
    exit 1
fi

check_composition
check_refusals
check_recipes

if [[ $FAILURES -gt 0 ]]; then
    echo "check-rnode-chip-offsets: $FAILURES assertion(s) failed" >&2
    exit 1
fi
echo "check-rnode-chip-offsets: chip and offsets follow the board"
