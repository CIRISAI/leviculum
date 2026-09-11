#!/usr/bin/env bash
# Where the flash goes, by crate: the firmware-diet trend number.
#
# Builds both firmware bins, then groups every flash-resident symbol
# (text, rodata, weak, initialised data) by its crate and prints one
# table per bin, largest group first. The store-gap gate says how much
# room is LEFT (scripts/check-nrf-store-gap.sh); this says who TOOK it,
# so a growth trend has a name before the gap becomes a link error.
#
# Grouping: the first path segment of the demangled name. For impls on
# primitives (`<f32 as core::str::traits::FromStr>::from_str`) the
# defining crate of the trait is charged, `core` when there is none.
# Anonymous rodata (`.Lanon.*`) has no owner in the symbol table and is
# reported as its own line, as is the residue of symbolless flash
# (vector table, padding, literal pools): the table's TOTAL is the
# symbol-accounted part, the `image` header line is what the UF2 flashes.
#
# Run from leviculum-nrf/:
#   bash tools/flash-attribution.sh                 # both bins, crate table
#   bash tools/flash-attribution.sh --top alloc     # largest symbols of one group
#   bash tools/flash-attribution.sh --no-build      # reuse existing ELFs
set -euo pipefail

cd "$(dirname "$0")/.."
NRF="$PWD"
# cargo decides where the ELFs land; CARGO_TARGET_DIR moves them out of the
# workspace entirely (scripts/cargo-target-dir.sh in the repo root).
# shellcheck source-path=SCRIPTDIR/../..
# shellcheck source=scripts/cargo-target-dir.sh
source "$NRF/../scripts/cargo-target-dir.sh"
elfdir="$(cargo_target_dir "$NRF")/thumbv7em-none-eabihf/release"

build=1
top=""
while [ $# -gt 0 ]; do
    case "$1" in
    --no-build) build=0 ;;
    --top)
        top="${2:?--top needs a group name}"
        shift
        ;;
    *)
        echo "usage: flash-attribution.sh [--no-build] [--top <group>]" >&2
        exit 2
        ;;
    esac
    shift
done

if [ "$build" -eq 1 ]; then
    cargo build --release --bin t114 --features bsp-t114 --quiet
    cargo build --release --bin rak4631 --features bsp-rak4631,rak-baseboard --quiet
fi

image_bytes() { # PT_LOAD file bytes below the 1 MiB flash line, like the store gate
    arm-none-eabi-readelf -lW "$1" | python3 -c '
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
    if filesz and phys < 0x100000:
        end += filesz
print(end)
'
}

attribute() { # attribute <elf> <top-group-or-empty>
    arm-none-eabi-nm -S --size-sort -C "$1" | TOP="$2" python3 -c '
import os, re, sys

def crate_of(name: str) -> str:
    if name.startswith(".Lanon"):
        return "(anon rodata)"
    if name.startswith("<"):
        inner = re.sub(r"^<[&* ]*(dyn |mut )*", "", name)
        m = re.match(r"([A-Za-z_][A-Za-z0-9_]*)::", inner)
        if m:
            return m.group(1)
        # impl on a primitive: charge the trait crate, core by default
        m = re.search(r" as ([A-Za-z_][A-Za-z0-9_]*)::", name)
        return m.group(1) if m else "core"
    m = re.match(r"([A-Za-z_][A-Za-z0-9_]*)::", name)
    return m.group(1) if m else "(no crate)"

groups: dict[str, int] = {}
members: dict[str, list[tuple[int, str]]] = {}
total = 0
for line in sys.stdin:
    parts = line.split(None, 3)
    if len(parts) < 4 or parts[2] not in "TtRrWwDd":
        continue
    size = int(parts[1], 16)
    name = re.sub(r"::h[0-9a-f]{16}$", "", parts[3].strip())
    crate = crate_of(name)
    groups[crate] = groups.get(crate, 0) + size
    members.setdefault(crate, []).append((size, name))
    total += size

top = os.environ["TOP"]
if top:
    for size, name in sorted(members.get(top, []), reverse=True):
        print(f"{size:9d}  {name}")
else:
    for crate, size in sorted(groups.items(), key=lambda kv: -kv[1]):
        print(f"{size:9d}  {crate}")
print(f"{total:9d}  TOTAL (symbol-accounted)")
'
}

for bin in t114 rak4631; do
    elf="$elfdir/$bin"
    [ -f "$elf" ] || {
        echo "[flash-attribution] missing ELF: $elf" >&2
        exit 1
    }
    img="$(image_bytes "$elf")"
    printf '\n== %s  image %d B (%d KiB) ==\n' "$bin" "$img" "$((img / 1024))"
    attribute "$elf" "$top"
done
