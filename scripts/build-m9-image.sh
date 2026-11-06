#!/usr/bin/env bash
#
# The ThinkNode M9's Loquax image (order 1361, slice four of "The M9
# frontend: the plan, measured" in docs/src/concepts/handheld-messenger.md,
# Codeberg #232 and #473): build it for xtensa-esp32s3-none-elf, package it
# as espflash flashes it, and print its headroom the way the nRF flash
# budget prints its own (scripts/check-nrf-flash-budget.sh):
#
#   FLASH_HEADROOM board=thinknode_m9 used=<bytes> free=<bytes>
#   STACK_HEADROOM board=thinknode_m9 stack=<bytes> need=<bytes>
#
# `used` is the application image espflash writes, `free` what is left of
# the application partition of espflash's default table for 16 MB, both read
# off espflash's own "App/part. size: <used>/<partition> bytes" line. The
# stack is the `.stack` section the linker leaves in internal RAM (the UI
# heap is PSRAM); `need` is the open chat's scene and draw ceilings of
# loquax-m9/src/census.rs added up, 184,000 B, the deepest the board's
# census allows one screen to go.
#
# Red when the image does not build, when fewer than 8 KiB of the partition
# are free (the nRF floor, for the nRF reason), when the stack is below the
# need, or when the image names this tree's path or the home directory.
#
# The image is the same bytes from any clone of the same commit (order
# 1458, scripts/check-m9-reproducible.sh guards it): rustc gets the path
# remap of scripts/nrf-rustflags.sh for the xtensa triple, so no absolute
# path of the building tree, the cargo home or the toolchain is compiled in,
# and SOURCE_DATE_EPOCH is the commit's date unless the caller set one,
# because esp-bootloader-esp-idf's build script stamps the application
# descriptor with that time, else with the clock.
#
# The esp toolchain is optional per host (see `build-esp32` in the
# Justfile): without ~/export-esp.sh this says SKIP, unless
# M9_IMAGE_REQUIRE_ESP=1 makes that a failure.
#
# Usage:
#   bash scripts/build-m9-image.sh             # build, package, measure
#   bash scripts/build-m9-image.sh --selftest  # the verdicts on fake numbers

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ESP="$ROOT/leviculum-esp"
TAG="[m9-image]"
FLOOR=8192
STACK_NEED=184000

# verdict <used> <partition> <stack>: the two headroom lines; 1 when either
# is below its floor.
verdict() {
    local used="$1" partition="$2" stack="$3" rc=0
    echo "FLASH_HEADROOM board=thinknode_m9 used=$used free=$((partition - used))"
    echo "STACK_HEADROOM board=thinknode_m9 stack=$stack need=$STACK_NEED"
    if [ $((partition - used)) -lt "$FLOOR" ]; then
        echo "$TAG FAIL $((partition - used)) B of the application partition free, below $FLOOR B."
        rc=1
    fi
    if [ "$stack" -lt "$STACK_NEED" ]; then
        echo "$TAG FAIL $stack B of stack, below the $STACK_NEED B the open chat's census allows."
        rc=1
    fi
    return "$rc"
}

# app_size <espflash log>: "<used> <partition>" from espflash's size line.
app_size() {
    sed -n 's/.*App\/part\. size: *\([0-9,]*\)\/\([0-9,]*\) bytes.*/\1 \2/p' "$1" | tr -d , | head -1
}

selftest() {
    local log rc=0
    mkdir -p "$ROOT/target/tmp"
    log="$(mktemp "$ROOT/target/tmp/m9-image-selftest.XXXXXX")"
    printf 'Chip type:         esp32s3\nApp/part. size:    927,520/16,384,000 bytes, 5.66%%\n' >"$log"
    [ "$(app_size "$log")" = "927520 16384000" ] || { echo "$TAG selftest FAIL: size line not read"; rc=1; }
    rm -f "$log"
    verdict 927520 16384000 312440 >/dev/null || { echo "$TAG selftest FAIL: a roomy image was refused"; rc=1; }
    verdict 16380000 16384000 312440 >/dev/null && { echo "$TAG selftest FAIL: 4000 B free passed"; rc=1; }
    verdict 927520 16384000 100000 >/dev/null && { echo "$TAG selftest FAIL: a 100 kB stack passed"; rc=1; }
    [ "$rc" -eq 0 ] && echo "$TAG selftest ok: the size line reads, both floors refuse"
    return "$rc"
}

if [ "${1:-}" = "--selftest" ]; then
    selftest
    exit $?
fi

if [ ! -f "$HOME/export-esp.sh" ]; then
    if [ "${M9_IMAGE_REQUIRE_ESP:-0}" = 1 ]; then
        echo "$TAG FAIL no esp toolchain (~/export-esp.sh) and M9_IMAGE_REQUIRE_ESP=1"
        exit 1
    fi
    echo "$TAG SKIP no esp toolchain on this host (~/export-esp.sh; scripts/install-ci.sh installs it)"
    exit 0
fi

selftest || exit 1
# shellcheck source=scripts/cargo-target-dir.sh
source "$ROOT/scripts/cargo-target-dir.sh"
# shellcheck source=scripts/nrf-rustflags.sh
source "$ROOT/scripts/nrf-rustflags.sh"
target="$(cargo_target_dir "$ESP")" || { echo "$TAG FAIL no target directory for $ESP"; exit 1; }
OUT="$target/xtensa-esp32s3-none-elf/release"
if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
    SOURCE_DATE_EPOCH="$(git -C "$ROOT" log -1 --format=%ct HEAD 2>/dev/null)" || SOURCE_DATE_EPOCH=""
fi
if [ -n "$SOURCE_DATE_EPOCH" ]; then
    export SOURCE_DATE_EPOCH
else
    unset SOURCE_DATE_EPOCH
    echo "$TAG note: no git commit date here, the image carries the build clock"
fi
mkdir -p "$ROOT/target/tmp"
log="$ROOT/target/tmp/m9-image.log"
# shellcheck disable=SC1091
if ! (source "$HOME/export-esp.sh" >/dev/null && export PATH="$HOME/.cargo/bin:$PATH" \
        && esp_rustflags_export && cd "$ESP" && cargo build --release --locked --bin thinknode_m9 \
        && espflash save-image --chip esp32s3 --flash-size 16mb \
            "$OUT/thinknode_m9" "$OUT/thinknode_m9.bin") >"$log" 2>&1; then
    tail -30 "$log"
    echo "$TAG FAIL the image did not build or package (the full log: $log)"
    exit 1
fi
read -r used partition < <(app_size "$log")
if [ -z "${used:-}" ] || [ -z "${partition:-}" ]; then
    tail -10 "$log"
    echo "$TAG FAIL espflash printed no size line (the full log: $log)"
    exit 1
fi
# shellcheck disable=SC1091
stack="$(source "$HOME/export-esp.sh" >/dev/null && xtensa-esp32s3-elf-size -A "$OUT/thinknode_m9" \
    | awk '$1 == ".stack" { print $2 }')"
echo "$TAG image $OUT/thinknode_m9.bin ($(stat -c %s "$OUT/thinknode_m9.bin") B), elf $OUT/thinknode_m9"
rc=0
verdict "$used" "$partition" "${stack:-0}" || rc=1
# The cheap half of check-m9-reproducible.sh, on every build: the shipped
# image names no path of this host (the remap above, order 1458).
for p in "$ROOT/" "$HOME/"; do
    if grep -qaF -- "$p" "$OUT/thinknode_m9.bin"; then
        echo "$TAG FAIL the image names the host path $p ($(grep -caF -- "$p" "$OUT/thinknode_m9.bin") lines); the path remap did not reach it"
        rc=1
    fi
done
exit "$rc"
