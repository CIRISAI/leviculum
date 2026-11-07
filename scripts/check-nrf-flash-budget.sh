#!/usr/bin/env bash
#
# Flash headroom gate for the nRF firmware images (order 1300, #359).
#
# The image window is FLASH in leviculum-nrf/memory.x, 0x27000 + 0xB0000. On
# 2026-11-01 the Pocket image (rak4631 with rak-baseboard) had 32 bytes of it
# left, so a 768 B diagnostic (the thread witness, order 1297) could not be
# linked into the one board it was written for. A day later the Pocket image
# did not link at all: FLASH overflowed by 56 B on master, and no gate a coder
# pass runs had said so. The linker is a cliff with no warning track; this is
# the warning track.
#
# It links every image the tree ships or flashes (t114, rak4631 as the
# Pocket, solarnode, t1000e) and prints one line per image:
#
#   FLASH_HEADROOM board=<b> used=<bytes> free=<bytes>
#
# `used` is the image as flashed (the PT_LOAD segments, scripts/nrf-elf.sh)
# minus the FLASH origin, `free` the bytes left below the end of FLASH. An
# image that does not link prints a negative `free`, read from lld's
# "overflowed by N bytes", instead of a cargo error with no number in it.
#
# Red when any image has less than the floor free. The floor is 8 KiB, two
# 4 KiB flash pages, because that is the growth one ordinary firmware batch
# brings: 1297's witness alone was 768 B, and the Pocket went from 32 B free
# to 56 B short in the eight firmware commits between ee9541af and 29aa5b2f.
# Below 8 KiB the next feature batch is the one that breaks the build, and
# the decision what the board gives up for it would be made under a red land
# gate instead of in advance.
#
# Usage:
#   bash scripts/check-nrf-flash-budget.sh              # link all four, floor 8192
#   bash scripts/check-nrf-flash-budget.sh <floor>      # another floor, bytes
#   bash scripts/check-nrf-flash-budget.sh --selftest   # fake images, no build
#
# The images are built with scripts/nrf-rustflags.sh, which remaps the
# checkout's, the registry's and the toolchain's paths (order 1368), and
# builds at one fixed path in a private mount namespace (order 1460), so the
# bytes do not depend on where the tree is checked out
# (scripts/check-nrf-reproducible.sh compares two). The gate holds the first
# half too: an image whose .rodata still carries `/home/` or the fixed build
# path is red, a host path that reached the board through a root the remap
# does not know.
#
# Exit 0 = every image links, keeps the floor free and carries no host path.
# Exit 1 = one does not, or a selftest case did not behave.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NRF="$ROOT/leviculum-nrf"
DEFAULT_FLOOR=8192

# shellcheck source=scripts/nrf-elf.sh
source "$ROOT/scripts/nrf-elf.sh"

# The images: bin name and the feature set it ships with. The rak4631 line is
# the Pocket build, the one the lnflash bundle carries
# (scripts/lnflash-bundle.sh BOARDS) and the tightest of the four.
IMAGES=(
    "t114|bsp-t114"
    "rak4631|bsp-rak4631,rak-baseboard"
    "solarnode|bsp-solarnode"
    "t1000e|bsp-t1000e"
)

# flash_region <memory.x>: prints "<origin> <length>" in decimal, from the
# FLASH line of the linker script the images are linked with.
flash_region() {
    python3 -I -c '
import re, sys
text = open(sys.argv[1]).read()
m = re.search(r"^\s*FLASH\s*:\s*ORIGIN\s*=\s*(0x[0-9A-Fa-f]+|\d+)\s*,\s*LENGTH\s*=\s*(0x[0-9A-Fa-f]+|\d+)",
              text, re.M)
if not m:
    print("no FLASH region line in " + sys.argv[1], file=sys.stderr)
    sys.exit(2)
print(int(m.group(1), 0), int(m.group(2), 0))
' "$1"
}

# overflow_bytes <build log>: the largest "overflowed by N bytes" lld printed
# for region FLASH, or nothing when the log has none. lld prints one line per
# section past the end, cumulative, so the largest is the whole shortfall.
overflow_bytes() {
    grep -oE "will not fit in region 'FLASH': overflowed by [0-9]+ bytes" "$1" |
        grep -oE '[0-9]+ bytes' | grep -oE '[0-9]+' | sort -n | tail -1
}

# verdict <board> <used> <free> <floor>: prints the headroom line and, below
# the floor, the failure. Returns 1 below the floor.
verdict() {
    local board="$1" used="$2" free="$3" floor="$4"
    echo "FLASH_HEADROOM board=$board used=$used free=$free"
    if [ "$free" -lt "$floor" ]; then
        if [ "$free" -lt 0 ]; then
            echo "[flash-budget] FAIL $board: the image does not link, FLASH is $((-free)) B short."
        else
            echo "[flash-budget] FAIL $board: $free B free, below the floor of $floor B."
        fi
        echo "[flash-budget] Win the bytes back before the next change lands (the census and"
        echo "[flash-budget] the knobs already taken are in leviculum-nrf/.cargo/config.toml);"
        echo "[flash-budget] dropping a feature from a board is Lew's decision, not a pass's."
        return 1
    fi
    return 0
}

# host_path_verdict <board> <rodata blob>: red when the blob holds `/home/`
# or the fixed build path `/mnt/leviculum/` (order 1460; the remap maps it to
# /src, so finding it means the remap missed the namespace's root).
# Panic locations are the bulk of what lands there; 1300 counted about 5.6 KB
# of them per image before the remap, and with them the image size moved with
# the checkout path (1326).
host_path_verdict() {
    local board="$1" blob="$2" n
    n="$(grep -acE '/home/|/mnt/leviculum/' "$blob")"
    if [ "$n" -ne 0 ]; then
        echo "[flash-budget] FAIL $board: .rodata carries a host path ($n line(s) with /home/ or /mnt/leviculum/), e.g.:"
        grep -aoE '(/home/|/mnt/leviculum/)[[:graph:]]*' "$blob" | head -3 | sed 's/^/    /'
        echo "[flash-budget] Build through scripts/nrf-rustflags.sh, or teach it the missing root."
        return 1
    fi
    return 0
}

# check_host_paths <board> <elf> <scratch dir>
check_host_paths() {
    local board="$1" elf="$2" blob="$3/$1.rodata"
    "$OBJCOPY" -O binary --only-section=.rodata "$elf" "$blob" || {
        echo "[flash-budget] FAIL $board: could not extract .rodata from $elf"
        return 1
    }
    host_path_verdict "$board" "$blob"
}

# measure_elf <board> <elf> <origin> <length> <floor>
measure_elf() {
    local board="$1" elf="$2" origin="$3" length="$4" floor="$5" end
    end="$("$READELF" -lW "$elf" | nrf_image_end)" || {
        echo "[flash-budget] FAIL $board: could not read $elf"
        return 1
    }
    verdict "$board" "$((end - origin))" "$((origin + length - end))" "$floor"
}

# measure_log <board> <build log> <length> <floor>: a build that failed. With
# an overflow it is a number below zero; anything else is a build error and
# the log tail is the evidence.
measure_log() {
    local board="$1" log="$2" length="$3" floor="$4" over
    over="$(overflow_bytes "$log")"
    if [ -n "$over" ]; then
        verdict "$board" "$((length + over))" "$((-over))" "$floor"
        return 1
    fi
    echo "[flash-budget] FAIL $board: the build failed for another reason than flash size:"
    tail -20 "$log"
    return 1
}

run() {
    local floor="$1" origin length out logdir rc=0 entry board features log
    read -r origin length < <(flash_region "$NRF/memory.x") || exit 1
    # shellcheck source-path=SCRIPTDIR/..
    # shellcheck source=scripts/cargo-target-dir.sh
    source "$ROOT/scripts/cargo-target-dir.sh"
    out="$(cargo_target_dir "$NRF")" || exit 1
    # shellcheck source=scripts/nrf-rustflags.sh
    source "$ROOT/scripts/nrf-rustflags.sh"
    logdir="$out/flash-budget"
    mkdir -p "$logdir"
    for entry in "${IMAGES[@]}"; do
        board="${entry%%|*}"
        features="${entry#*|}"
        log="$logdir/$board.log"
        if (cd "$NRF" && nrf_cargo build --release --bin "$board" --features "$features") >"$log" 2>&1; then
            measure_elf "$board" "$out/thumbv7em-none-eabihf/release/$board" "$origin" "$length" "$floor" || rc=1
            check_host_paths "$board" "$out/thumbv7em-none-eabihf/release/$board" "$logdir" || rc=1
        else
            measure_log "$board" "$log" "$length" "$floor" || rc=1
        fi
    done
    echo "[flash-budget] floor $floor B, FLASH $(printf '%#x' "$origin") + $(printf '%#x' "$length")"
    exit "$rc"
}

# --- Selftest: fake images through the real measurement ----------------------

# fake_elf <path> <phys> <filesz>: a minimal ELF32 little-endian ARM file with
# one PT_LOAD segment, enough for readelf -lW and nothing else.
fake_elf() {
    python3 -I -c '
import struct, sys
path, phys, filesz = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
ehdr = struct.pack("<16sHHIIIIIHHHHHH",
    b"\x7fELF\x01\x01\x01" + bytes(9), 2, 40, 1, phys, 52, 0, 0x05000400,
    52, 32, 1, 40, 0, 0)
phdr = struct.pack("<IIIIIIII", 1, 0x1000, phys, phys, filesz, filesz, 5, 4)
with open(path, "wb") as f:
    f.write(ehdr + phdr)
    f.write(bytes(0x1000 - len(ehdr) - len(phdr)))
    f.write(bytes(filesz))
' "$@"
}

selftest() {
    local dir fails=0 origin length out
    dir="$(mktemp -d "${TMPDIR:-/tmp}/flash-budget-selftest.XXXXXX")"
    trap 'rm -rf "$dir"' RETURN
    read -r origin length < <(flash_region "$NRF/memory.x") || return 1
    local end=$((origin + length))

    expect() { # expect <want rc> <want line> <name> <cmd...>
        local want_rc="$1" want_line="$2" name="$3" got_rc
        shift 3
        out="$("$@" 2>&1)"
        got_rc=$?
        if [ "$got_rc" -ne "$want_rc" ] || ! grep -qxF "$want_line" <<<"$out"; then
            echo "[flash-budget] selftest FAIL $name: rc $got_rc (want $want_rc)"
            printf '    %s\n' "${out//$'\n'/$'\n'    }"
            fails=$((fails + 1))
        else
            echo "[flash-budget] selftest ok   $name"
        fi
    }

    # An image with 16 KiB free passes the 8 KiB floor.
    fake_elf "$dir/roomy" "$origin" "$((length - 16384))"
    expect 0 "FLASH_HEADROOM board=roomy used=$((length - 16384)) free=16384" \
        "16 KiB free passes" measure_elf roomy "$dir/roomy" "$origin" "$length" "$DEFAULT_FLOOR"

    # The oversized one: it still links, with 100 B left, and is red. This is
    # the Pocket on 2026-11-01 within a few bytes.
    fake_elf "$dir/oversized" "$origin" "$((length - 100))"
    expect 1 "FLASH_HEADROOM board=oversized used=$((length - 100)) free=100" \
        "100 B free is red" measure_elf oversized "$dir/oversized" "$origin" "$length" "$DEFAULT_FLOOR"

    # One byte under the floor is red, the floor itself is not: the edge.
    fake_elf "$dir/edge" "$origin" "$((length - DEFAULT_FLOOR))"
    expect 0 "FLASH_HEADROOM board=edge used=$((length - DEFAULT_FLOOR)) free=$DEFAULT_FLOOR" \
        "exactly the floor passes" measure_elf edge "$dir/edge" "$origin" "$length" "$DEFAULT_FLOOR"
    fake_elf "$dir/edge1" "$origin" "$((length - DEFAULT_FLOOR + 1))"
    expect 1 "FLASH_HEADROOM board=edge1 used=$((length - DEFAULT_FLOOR + 1)) free=$((DEFAULT_FLOOR - 1))" \
        "one byte under the floor is red" measure_elf edge1 "$dir/edge1" "$origin" "$length" "$DEFAULT_FLOOR"

    # A segment placed above FLASH by a future SECTIONS edit counts: the image
    # as flashed ends where the highest flash segment ends.
    fake_elf "$dir/past" "$((end + 4096))" 16
    expect 1 "FLASH_HEADROOM board=past used=$((length + 4096 + 16)) free=$((-4096 - 16))" \
        "a segment past FLASH is red" measure_elf past "$dir/past" "$origin" "$length" "$DEFAULT_FLOOR"

    # An image that did not link: the number comes out of lld's message, the
    # largest of the cumulative lines.
    cat >"$dir/overflow.log" <<'LOG'
  = note: rust-lld: error: section '.data' will not fit in region 'FLASH': overflowed by 56 bytes
          rust-lld: error: section '.gnu.sgstubs' will not fit in region 'FLASH': overflowed by 64 bytes
LOG
    expect 1 "FLASH_HEADROOM board=overflow used=$((length + 64)) free=-64" \
        "a link overflow is a negative number" measure_log overflow "$dir/overflow.log" "$length" "$DEFAULT_FLOOR"

    # A build that failed for another reason is red and says so, not a number.
    echo "error[E0425]: cannot find value \`x\` in this scope" >"$dir/broken.log"
    expect 1 "[flash-budget] FAIL broken: the build failed for another reason than flash size:" \
        "a compile error is red without a number" measure_log broken "$dir/broken.log" "$length" "$DEFAULT_FLOOR"

    # The host-path guard: a .rodata with a registry path in it is red, one
    # with only remapped paths is not.
    printf 'panicked at /home/u/.cargo/registry/src/x/lib' >"$dir/home.rodata"
    expect 1 "[flash-budget] FAIL home: .rodata carries a host path (1 line(s) with /home/ or /mnt/leviculum/), e.g.:" \
        "a /home/ path in .rodata is red" host_path_verdict home "$dir/home.rodata"
    printf 'panicked at /mnt/leviculum/leviculum-core/src/lib' >"$dir/canon.rodata"
    expect 1 "[flash-budget] FAIL canon: .rodata carries a host path (1 line(s) with /home/ or /mnt/leviculum/), e.g.:" \
        "the unremapped fixed build path is red" host_path_verdict canon "$dir/canon.rodata"
    printf 'panicked at /registry/x/lib\0/src/leviculum-core/src' >"$dir/clean.rodata"
    expect 0 "" "remapped paths pass" host_path_verdict clean "$dir/clean.rodata"

    if [ "$fails" -ne 0 ]; then
        echo "[flash-budget] selftest: $fails case(s) failed"
        return 1
    fi
    echo "[flash-budget] selftest: all cases behaved"
}

READELF="$(nrf_elf_tool flash-budget readelf)" || exit 1
OBJCOPY="$(nrf_elf_tool flash-budget objcopy)" || exit 1

case "${1:-}" in
    --selftest) selftest; exit $? ;;
    "") run "$DEFAULT_FLOOR" ;;
    *[!0-9]*) echo "usage: $0 [floor_bytes|--selftest]" >&2; exit 2 ;;
    *) run "$1" ;;
esac
