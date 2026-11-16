#!/usr/bin/env bash
#
# Record-store gap gate for the nRF firmware (Codeberg #384).
#
# The store's 64 KiB region sits above the firmware image, with the calendar
# checkpoint ring (#166 item 4) and the boot-record page (#380) between them
# (`leviculum-nrf/memory.x`: FLASH ends at 0xD7000, TIME is 0xD7000-0xD9000,
# BOOT is 0xD9000-0xDA000, STORE runs 0xDA000-0xEA000). An image that grows
# into any of them would erase the board's message store, its restart count or
# its learned time on the next UF2 flash.
#
# The GAP this gate prints is therefore measured to the TIME ring, not to the
# store: the ring is what the image runs into first, and a gap measured past it
# would overstate the headroom by its pages.
#
# WHAT THE LINKER ALREADY REFUSES, without this gate:
#   * an image whose sections do not fit FLASH - "section `.text' will not fit
#     in region `FLASH'" - because FLASH now stops where BOOT starts;
#   * FLASH enlarged over BOOT, BOOT over STORE, STORE pushed into the
#     persistence pages at USER_FLASH_END, or either region sized off the 4 KiB
#     page grid: the ASSERTs at the bottom of memory.x.
# All three are link errors, so they cannot reach a board.
#
# WHAT THIS GATE ADDS:
#   1. The NUMBER, printed for every bin on every run. A link error is a cliff
#      with no warning track: it says "does not fit" the first time it does not
#      fit, and nothing before that says the gap is down to 8 KiB. The trend is
#      the useful part, and only a printed number has one. Pass a minimum gap to
#      turn the trend into a gate: `check-nrf-store-gap.sh 65536`.
#   2. It measures the image AS FLASHED - the PT_LOAD segments' physical
#      addresses and file sizes, which is what `objcopy -O binary` and the .uf2
#      are built from - rather than the sections the linker charged to FLASH. A
#      section placed at an absolute address by a future `SECTIONS` edit lands
#      in the .bin and in no region's accounting.
#   3. It reads the bounds from `__srecord_store`/`__erecord_store` and
#      `__sboot_record` in the linked ELF: the values the FIRMWARE mounts and
#      writes, not the ones memory.x appears to say. An edit that moves a region
#      but not its symbol (or the other way round) is invisible to every ASSERT
#      written in terms of ORIGIN(STORE) or ORIGIN(BOOT).
#
# Positive control for the comparison itself: a minimum gap nothing could
# satisfy must fail.
#
#   bash scripts/check-nrf-store-gap.sh 999999999   # must report FAIL
#
# Usage: check-nrf-store-gap.sh [min_gap_bytes]

set -euo pipefail

MIN_GAP="${1:-0}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NRF="$ROOT/leviculum-nrf"
# cargo decides where the ELFs land; see scripts/cargo-target-dir.sh for the
# incident that taught us not to assume `$NRF/target`.
# shellcheck source-path=SCRIPTDIR/..
# shellcheck source=scripts/cargo-target-dir.sh
source "$ROOT/scripts/cargo-target-dir.sh"
# The firmware is built with the checkout's paths remapped, the same way in
# every script that builds it, at one fixed path (scripts/nrf-rustflags.sh,
# orders 1368 and 1460): every build below goes through nrf_cargo.
# shellcheck source=scripts/nrf-rustflags.sh
source "$ROOT/scripts/nrf-rustflags.sh"
OUT="$(cargo_target_dir "$NRF")/thumbv7em-none-eabihf/release"

# The bootloader's USER_FLASH_END: the three persistence pages at and above it
# survive a UF2 because the bootloader declines every block there
# (docs/src/concepts/lnode-flashing.md). The store must stop at it.
USER_FLASH_END=$((0xEA000))

# The tool lookup and the image-end measurement are shared with
# check-nrf-flash-budget.sh, which reads the same ELFs for a different edge.
# shellcheck source=scripts/nrf-elf.sh
source "$ROOT/scripts/nrf-elf.sh"
READELF="$(nrf_elf_tool store-gap readelf)"
NM="$(nrf_elf_tool store-gap nm)"

symbol() { # symbol <elf> <name> -> prints its value, decimal
    local value
    value="$("$NM" "$2" | awk -v s="$3" '$3 == s { print $1 }' | head -1)"
    if [ -z "$value" ]; then
        echo "[store-gap] $1: no symbol $3 in the linked ELF." >&2
        echo "[store-gap] memory.x must define it; see its STORE, BOOT and TIME regions." >&2
        return 1
    fi
    echo $((0x$value))
}

build() {
    # shellcheck disable=SC2086 # feature list is intentionally word-split
    (cd "$NRF" && nrf_cargo build --release --bin "$1" --features "$2")
}

check() {
    local bin="$1" elf="$OUT/$1" end base store_end boot time time_end gap
    [ -f "$elf" ] || {
        echo "[store-gap] missing ELF: $elf" >&2
        exit 1
    }

    end="$("$READELF" -lW "$elf" | nrf_image_end)"
    base="$(symbol "$bin" "$elf" __srecord_store)"
    store_end="$(symbol "$bin" "$elf" __erecord_store)"
    boot="$(symbol "$bin" "$elf" __sboot_record)"
    time="$(symbol "$bin" "$elf" __stime_floor)"
    time_end="$(symbol "$bin" "$elf" __etime_floor)"
    gap=$((time - end))

    printf '[store-gap] %-8s image ends %#x, time ring %#x..%#x, boot record %#x, store %#x..%#x (%d pages), gap %d B (%d KiB)\n' \
        "$bin" "$end" "$time" "$time_end" "$boot" "$base" "$store_end" "$(((store_end - base) / 4096))" \
        "$gap" "$((gap / 1024))"

    if [ "$end" -gt "$time" ]; then
        echo "[store-gap] FAIL $bin: the image reaches $((end - time)) B into the time ring."
        echo "[store-gap] The ring, the boot page and the store are inside the UF2's writable window,"
        echo "[store-gap] so the next flash would erase them. Move the regions up (impossible"
        echo "[store-gap] past $(printf '%#x' "$USER_FLASH_END")), shrink them, or shrink the image — memory.x."
        return 1
    fi
    if [ "$time_end" -ne "$boot" ]; then
        echo "[store-gap] FAIL $bin: the time ring ends at $(printf '%#x' "$time_end") and the boot page"
        echo "[store-gap] starts at $(printf '%#x' "$boot") — the two must be adjacent,"
        echo "[store-gap] or the gap printed above is not the headroom the image actually has."
        return 1
    fi
    if [ $((boot + 4096)) -ne "$base" ]; then
        echo "[store-gap] FAIL $bin: the boot-record page is at $(printf '%#x' "$boot") and the store"
        echo "[store-gap] starts at $(printf '%#x' "$base") — the two must be adjacent, one page apart,"
        echo "[store-gap] or the gap printed above is not the headroom the image actually has."
        return 1
    fi
    if [ "$store_end" -gt "$USER_FLASH_END" ]; then
        echo "[store-gap] FAIL $bin: the store ends at $(printf '%#x' "$store_end"), above"
        echo "[store-gap] USER_FLASH_END $(printf '%#x' "$USER_FLASH_END") — it would overlap the identity,"
        echo "[store-gap] radio-config or telemetry page."
        return 1
    fi
    if [ $(((store_end - base) % 4096)) -ne 0 ] || [ $((base % 4096)) -ne 0 ]; then
        echo "[store-gap] FAIL $bin: the store is not a whole number of 4 KiB pages."
        return 1
    fi
    if [ "$gap" -lt "$MIN_GAP" ]; then
        echo "[store-gap] FAIL $bin: gap $gap B is below the required minimum $MIN_GAP B."
        return 1
    fi
}

rc=0
build t114 bsp-t114
build rak4631 bsp-rak4631,rak-baseboard
build solarnode bsp-solarnode
build t1000e bsp-t1000e
check t114 || rc=1
check rak4631 || rc=1
check solarnode || rc=1
check t1000e || rc=1
exit "$rc"
