#!/usr/bin/env bash
#
# Record-store gap gate for the nRF firmware (Codeberg #384).
#
# The store's 64 KiB region sits directly above the firmware image
# (`leviculum-nrf/memory.x`: FLASH ends at 0xDA000, STORE runs 0xDA000-0xEA000).
# An image that grows into it would erase the board's message store on the next
# UF2 flash.
#
# WHAT THE LINKER ALREADY REFUSES, without this gate:
#   * an image whose sections do not fit FLASH - "section `.text' will not fit
#     in region `FLASH'" - because FLASH now stops where STORE starts;
#   * FLASH enlarged over STORE, or STORE pushed into the persistence pages at
#     USER_FLASH_END, or STORE sized off the 4 KiB page grid: the three ASSERTs
#     at the bottom of memory.x.
# All three are link errors, so they cannot reach a board.
#
# WHAT THIS GATE ADDS:
#   1. The NUMBER, printed for both bins on every run. A link error is a cliff
#      with no warning track: it says "does not fit" the first time it does not
#      fit, and nothing before that says the gap is down to 8 KiB. The trend is
#      the useful part, and only a printed number has one. Pass a minimum gap to
#      turn the trend into a gate: `check-nrf-store-gap.sh 65536`.
#   2. It measures the image AS FLASHED - the PT_LOAD segments' physical
#      addresses and file sizes, which is what `objcopy -O binary` and the .uf2
#      are built from - rather than the sections the linker charged to FLASH. A
#      section placed at an absolute address by a future `SECTIONS` edit lands
#      in the .bin and in no region's accounting.
#   3. It reads the region's bounds from `__srecord_store`/`__erecord_store` in
#      the linked ELF: the values the FIRMWARE mounts, not the ones memory.x
#      appears to say. An edit that moves the region but not the symbols (or the
#      other way round) is invisible to every ASSERT written in terms of
#      ORIGIN(STORE).
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
OUT="$(cargo_target_dir "$NRF")/thumbv7em-none-eabihf/release"

# The bootloader's USER_FLASH_END: the three persistence pages at and above it
# survive a UF2 because the bootloader declines every block there
# (docs/src/concepts/lnode-flashing.md). The store must stop at it.
USER_FLASH_END=$((0xEA000))

find_tool() { # find_tool <suffix> -> prints a readelf/nm binary
    local want="$1"
    if command -v "arm-none-eabi-$want" >/dev/null 2>&1; then
        echo "arm-none-eabi-$want"
        return 0
    fi
    local d
    for d in "$(rustc --print sysroot)/lib/rustlib/"*/bin; do
        if [ -x "$d/llvm-$want" ]; then
            echo "$d/llvm-$want"
            return 0
        fi
    done
    echo "[store-gap] no $want found. Install binutils-arm-none-eabi," >&2
    echo "[store-gap] or add the rustup llvm-tools component." >&2
    return 1
}

READELF="$(find_tool readelf)"
NM="$(find_tool nm)"

# stdin: `readelf -lW` output. stdout: the highest flash byte the image
# occupies, decimal. Segments whose physical address is in RAM are the .bss/
# .uninit kind and carry no file content; a segment with FileSiz 0 carries none
# either.
image_end() {
    python3 -c '
import re, sys
end = 0
for line in sys.stdin:
    f = line.split()
    if len(f) < 6 or f[0] != "LOAD":
        continue
    try:
        phys, filesz = int(f[3], 16), int(f[4], 16)
    except ValueError:
        continue
    if filesz == 0 or phys >= 0x100000:   # 1 MiB of flash on the nRF52840
        continue
    end = max(end, phys + filesz)
if end == 0:
    print("no flash PT_LOAD segment found: readelf output not understood",
          file=sys.stderr)
    sys.exit(2)
print(end)
'
}

symbol() { # symbol <elf> <name> -> prints its value, decimal
    local value
    value="$("$NM" "$2" | awk -v s="$3" '$3 == s { print $1 }' | head -1)"
    if [ -z "$value" ]; then
        echo "[store-gap] $1: no symbol $3 in the linked ELF." >&2
        echo "[store-gap] memory.x must define it; see its STORE region." >&2
        return 1
    fi
    echo $((0x$value))
}

build() {
    # shellcheck disable=SC2086 # feature list is intentionally word-split
    (cd "$NRF" && cargo build --release --bin "$1" --features "$2")
}

check() {
    local bin="$1" elf="$OUT/$1" end base store_end gap
    [ -f "$elf" ] || {
        echo "[store-gap] missing ELF: $elf" >&2
        exit 1
    }

    end="$("$READELF" -lW "$elf" | image_end)"
    base="$(symbol "$bin" "$elf" __srecord_store)"
    store_end="$(symbol "$bin" "$elf" __erecord_store)"
    gap=$((base - end))

    printf '[store-gap] %-8s image ends %#x, store %#x..%#x (%d pages), gap %d B (%d KiB)\n' \
        "$bin" "$end" "$base" "$store_end" "$(((store_end - base) / 4096))" \
        "$gap" "$((gap / 1024))"

    if [ "$end" -gt "$base" ]; then
        echo "[store-gap] FAIL $bin: the image reaches $((end - base)) B into the record store."
        echo "[store-gap] The store's records are inside the UF2's writable window, so the"
        echo "[store-gap] next flash would erase them. Move STORE up (impossible past"
        echo "[store-gap] $(printf '%#x' "$USER_FLASH_END")), shrink it, or shrink the image — memory.x."
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
check t114 || rc=1
check rak4631 || rc=1
exit "$rc"
