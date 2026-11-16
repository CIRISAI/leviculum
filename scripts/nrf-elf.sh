# shellcheck shell=bash
#
# Reading a linked nRF firmware ELF, shared by the two gates that measure one:
# scripts/check-nrf-store-gap.sh and scripts/check-nrf-flash-budget.sh. Both
# need the same two answers, which binutils to read it with and where the
# image as flashed ends, and two copies of either would drift (order 1300).
#
# Source it; it defines functions and runs nothing.

# nrf_elf_tool <tag> <suffix>: prints a readelf/nm/objdump binary, preferring
# binutils-arm-none-eabi and falling back to the rustup llvm-tools component.
# <tag> prefixes the error so the calling gate names itself.
nrf_elf_tool() {
    local tag="$1" want="$2"
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
    echo "[$tag] no $want found. Install binutils-arm-none-eabi," >&2
    echo "[$tag] or add the rustup llvm-tools component." >&2
    return 1
}

# stdin: `readelf -lW` output. stdout: the highest flash byte the image
# occupies, decimal. It reads the PT_LOAD segments' physical addresses and
# file sizes, which is what `objcopy -O binary` and the .uf2 are built from,
# rather than the sections the linker charged to FLASH: a section placed at an
# absolute address lands in the .bin and in no region's accounting. Segments
# whose physical address is in RAM are the .bss/.uninit kind and carry no file
# content; a segment with FileSiz 0 carries none either.
nrf_image_end() {
    python3 -I -c '
import sys
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

# nrf_flat_image <elf> <out>: the image as flashed, the flat binary
# leviculum-nrf/tools/uf2-runner.sh makes the UF2 from, with the same three
# NOBITS-or-RAM sections left out. scripts/nrf-flash.sh prints its sha256 and
# scripts/check-nrf-reproducible.sh compares two of them (order 1460).
nrf_flat_image() {
    local objcopy
    objcopy="$(nrf_elf_tool nrf-image objcopy)" || return 1
    "$objcopy" -O binary -R .bss -R .uninit -R .retained "$1" "$2"
}
