#!/usr/bin/env bash
#
# Stack-frame gate for the nRF firmware.
#
# The T114 runs on a 128 KB stack that grows DOWN into the SoftDevice's RAM
# floor (flip-link layout): an overflow past `_stack_end` corrupts SD state
# and surfaces as an SD internal assertion, not as a clean fault. A single
# oversized frame therefore silently eats the whole margin.
#
# That is exactly what happened: `Box::new(builder.build(..))` materialised a
# by-value `NodeCore` (>40 KB with the inline `EmbeddedStorage`) twice in
# `main`'s poll frame — 94 720 B, 74 % of the stack, leaving ~13 KB of margin.
# `NodeCoreBuilder::build_boxed` removed it. This gate keeps it removed.
#
# Reads the frame-allocating `sub sp` immediates straight out of the linked
# ELF, so it measures the shipped binary rather than a source-level proxy.
#
# It also asserts the layout that makes the limit mean anything at all: that
# `_stack_end` really is a floor with the statics ABOVE it, i.e. that
# flip-link is in effect. See `check_layout` below.
#
# Usage: check-nrf-stack-frames.sh [max_frame_bytes]

set -euo pipefail

LIMIT="${1:-16384}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NRF="$ROOT/leviculum-nrf"
# Not "$NRF/target": cargo puts the ELF wherever CARGO_TARGET_DIR says, and a
# gate that looks in the wrong place reports "missing ELF" after a build that
# worked. See scripts/cargo-target-dir.sh.
# shellcheck source-path=SCRIPTDIR/..
# shellcheck source=scripts/cargo-target-dir.sh
source "$ROOT/scripts/cargo-target-dir.sh"
OUT="$(cargo_target_dir "$NRF")/thumbv7em-none-eabihf/release"

# GNU objdump prints the immediate in decimal, llvm-objdump in hex; the
# parser below accepts both, so either tool is fine.
OBJDUMP=""
if command -v arm-none-eabi-objdump >/dev/null 2>&1; then
    OBJDUMP="arm-none-eabi-objdump"
else
    for d in "$(rustc --print sysroot)/lib/rustlib/"*/bin; do
        if [ -x "$d/llvm-objdump" ]; then
            OBJDUMP="$d/llvm-objdump"
            break
        fi
    done
fi
if [ -z "$OBJDUMP" ]; then
    echo "[stack-frames] no objdump found. Install binutils-arm-none-eabi," >&2
    echo "[stack-frames] or add the rustup llvm-tools component." >&2
    exit 1
fi

# Same two sources, for the layout check's symbol table.
NM=""
if command -v arm-none-eabi-nm >/dev/null 2>&1; then
    NM="arm-none-eabi-nm"
else
    for d in "$(rustc --print sysroot)/lib/rustlib/"*/bin; do
        if [ -x "$d/llvm-nm" ]; then
            NM="$d/llvm-nm"
            break
        fi
    done
fi
if [ -z "$NM" ]; then
    echo "[nrf-layout] no nm found. Install binutils-arm-none-eabi," >&2
    echo "[nrf-layout] or add the rustup llvm-tools component." >&2
    exit 1
fi

# stdin: disassembly. stdout: frame sizes in bytes, descending.
parse_frames() {
    # shellcheck disable=SC2016 # python source, quoted literally on purpose:
    # the backticks in its comment and error text are prose, not command
    # substitution, and `$` would have to reach python unexpanded too.
    python3 -c '
import re, sys
# `sub sp, #N` (narrow) and `sub.w sp, sp, #N` / `subw sp, sp, #N` (wide) are
# the frame-allocating forms LLVM emits for thumbv7em.
pat = re.compile(r"\bsubw?(?:\.w)?\s+sp,\s+(?:sp,\s+)?#(0x[0-9a-fA-F]+|[0-9]+)")
sizes = set()
for line in sys.stdin:
    m = pat.search(line)
    if m:
        sizes.add(int(m.group(1), 0))
if not sizes:
    print("no `sub sp` frames found: objdump output not understood",
          file=sys.stderr)
    sys.exit(2)
for s in sorted(sizes, reverse=True):
    print(s)
'
}

build() {
    # shellcheck disable=SC2086 # feature list is intentionally word-split
    (cd "$NRF" && cargo build --release --bin "$1" --features "$2")
}

check() {
    local bin="$1" elf="$OUT/$1" frames max
    [ -f "$elf" ] || { echo "[stack-frames] missing ELF: $elf" >&2; exit 1; }

    frames="$("$OBJDUMP" -d "$elf" | parse_frames)"
    max="$(printf '%s\n' "$frames" | head -1)"

    if [ "$max" -gt "$LIMIT" ]; then
        echo "[stack-frames] FAIL $bin: largest frame ${max} B > limit ${LIMIT} B"
        echo "[stack-frames] largest frames:"
        printf '%s\n' "$frames" | head -5 | sed 's/^/  /'
        echo "[stack-frames] the owning function is the last symbol header before"
        echo "[stack-frames] the matching 'sub sp' in: $OBJDUMP -d $elf"
        return 1
    fi
    echo "[stack-frames] ok   $bin: largest frame ${max} B (limit ${LIMIT} B)"
}

# The flip-link layout, asserted on the linked ELF because nothing in the
# build declares it: `linker = "flip-link"` in leviculum-nrf/.cargo/config.toml
# is the only thing that puts the statics ABOVE the stack and turns
# ORIGIN(RAM) into `_stack_end`. Drop that line and the layout inverts back to
# cortex-m-rt's — statics at ORIGIN(RAM), stack at the top of RAM growing down
# into them, which is #50's original overflow — and it inverts SILENTLY: the
# image links, boots, and only misbehaves once a deep frame runs.
#
# Three consequences ride on the layout and are what this asserts:
#   * the frame limit above is a margin against `_stack_end` as a FLOOR;
#   * `assert_sd_fits_below_retained` (src/ble/mod.rs) exists only because
#     nrf-softdevice's own check compares against `__sdata`, the stack's TOP;
#   * ORIGIN(RAM) in memory.x does NOT set the SoftDevice's `app_ram_base`,
#     which is why the old "stay below 0x20010000" note there is retired
#     (#46). All three are false under the inverted layout.
check_layout() {
    local bin="$1" elf="$OUT/$1"
    [ -f "$elf" ] || { echo "[nrf-layout] missing ELF: $elf" >&2; exit 1; }

    "$NM" "$elf" | python3 -c '
import sys

# No single quotes anywhere below: this block is a single-quoted bash string.
want = ("__sdata", "_stack_start", "_stack_end", "__sretained", "__eretained")
sym = {}
for line in sys.stdin:
    f = line.split()
    if len(f) == 3 and f[2] in want:
        sym[f[2]] = int(f[0], 16)

bin_name = sys.argv[1]
missing = [n for n in want if n not in sym]
if missing:
    names = " ".join(missing)
    print(f"[nrf-layout] FAIL {bin_name}: symbols absent: {names}")
    sys.exit(1)

sdata = sym["__sdata"]
sstart = sym["_stack_start"]
send = sym["_stack_end"]
eret = sym["__eretained"]

problems = []
if sdata != sstart:
    problems.append(f"__sdata {sdata:#010x} != _stack_start {sstart:#010x}")
if sdata <= send:
    problems.append(
        f"statics are not above the stack: __sdata {sdata:#010x} <= "
        f"_stack_end {send:#010x} - flip-link is not in effect "
        f"(leviculum-nrf/.cargo/config.toml)")
if eret > send:
    problems.append(
        f"the retained records are inside the stack: __eretained "
        f"{eret:#010x} > _stack_end {send:#010x}")

if problems:
    print(f"[nrf-layout] FAIL {bin_name}:")
    for p in problems:
        print(f"  {p}")
    sys.exit(1)

print(f"[nrf-layout] ok   {bin_name}: stack [{send:#010x}, {sstart:#010x}) = "
      f"{sstart - send} B, statics above it, retained below")
' "$bin"
}

rc=0
build t114 bsp-t114
build rak4631 bsp-rak4631,rak-baseboard
build solarnode bsp-solarnode
check t114 || rc=1
check rak4631 || rc=1
check solarnode || rc=1
check_layout t114 || rc=1
check_layout rak4631 || rc=1
check_layout solarnode || rc=1
exit "$rc"
