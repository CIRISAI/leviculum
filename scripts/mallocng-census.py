#!/usr/bin/env python3
"""Decode the meta-area dumps written by scripts/mallocng-census.gdb.

mallocng keeps one `struct meta` per allocation group, and the groups of
one size class are only returned to the OS when a group is ENTIRELY free
(`okay_to_free`, musl src/malloc/mallocng/free.c:38-70, reached from
`nontrivial_free`, free.c:78). So a class whose live objects drain away
without any group emptying keeps its pages resident, and neither
`live_bytes` nor `/proc/self/statm` can say that happened. This script
says it: per size class, how many groups exist, how many slots they hold,
and how many of those slots are live.

  scripts/mallocng-census.py <dump-dir> <musl-static-binary>

The binary is read only for `nm -S`, to pin the two layout constants this
decoder hardcodes. If musl ever changes `struct malloc_context` or
`size_classes`, the sizes move and the script refuses rather than
printing plausible nonsense.
"""

import collections
import glob
import os
import re
import struct
import subprocess
import sys

# x86_64 layout of musl 1.2.5's struct malloc_context (meta.h:39-56) and
# struct meta_area (meta.h:33-37) / struct meta (meta.h:24-31).
MALLOC_CONTEXT_SIZE = 0x3A0
SIZE_CLASSES_SIZE = 48 * 2
META_AREA_SLOTS_OFFSET = 24
META_SIZE = 40
UNIT = 16


def symbol_sizes(binary):
    out = subprocess.run(
        ["nm", "-S", binary], capture_output=True, text=True, check=True
    ).stdout
    sizes = {}
    for line in out.splitlines():
        parts = line.split()
        if len(parts) == 4:
            sizes[parts[3]] = int(parts[1], 16)
    return sizes


def check_layout(binary):
    sizes = symbol_sizes(binary)
    for name, want in (
        ("__malloc_context", MALLOC_CONTEXT_SIZE),
        ("__malloc_size_classes", SIZE_CLASSES_SIZE),
    ):
        got = sizes.get(name)
        if got is None:
            sys.exit(f"{binary}: no {name} symbol -- not a musl-static build?")
        if got != want:
            sys.exit(
                f"{binary}: {name} is {got:#x} bytes, this decoder assumes "
                f"{want:#x}. musl's layout moved; re-derive the offsets in "
                "scripts/mallocng-census.gdb before trusting any number here."
            )


def size_classes(binary):
    """musl's own size_classes[], read out of the binary's .rodata."""
    out = subprocess.run(
        ["nm", binary], capture_output=True, text=True, check=True
    ).stdout
    addr = None
    for line in out.splitlines():
        parts = line.split()
        if len(parts) == 3 and parts[2] == "__malloc_size_classes":
            addr = int(parts[0], 16)
    if addr is None:
        sys.exit(f"{binary}: no __malloc_size_classes symbol")
    # objcopy the section out rather than guessing the file offset.
    raw = subprocess.run(
        ["objdump", "-s", "-j", ".rodata", binary], capture_output=True, text=True,
        check=True,
    ).stdout
    blob = {}
    for line in raw.splitlines():
        m = re.match(r"\s*([0-9a-f]+)\s+((?:[0-9a-f]{2,8}\s){1,4})", line)
        if m:
            base = int(m.group(1), 16)
            data = bytes.fromhex(m.group(2).replace(" ", ""))
            for i, b in enumerate(data):
                blob[base + i] = b
    try:
        data = bytes(blob[addr + i] for i in range(SIZE_CLASSES_SIZE))
    except KeyError:
        sys.exit("could not read __malloc_size_classes out of .rodata")
    return list(struct.unpack("<48H", data))


def stages(dump_dir):
    found = collections.defaultdict(list)
    for path in glob.glob(os.path.join(dump_dir, "s*_*.bin")):
        stage = int(os.path.basename(path).split("_")[0][1:])
        found[stage].append(path)
    return found


def census(paths):
    """(groups, slots, free_slots) per size class, over all meta areas."""
    agg = collections.defaultdict(lambda: [0, 0, 0])
    for path in sorted(paths):
        blob = open(path, "rb").read()
        nslots = struct.unpack_from("<i", blob, 16)[0]
        if not 0 < nslots <= (len(blob) - META_AREA_SLOTS_OFFSET) // META_SIZE:
            sys.exit(f"{path}: nslots={nslots} is not a meta area")
        for i in range(nslots):
            off = META_AREA_SLOTS_OFFSET + META_SIZE * i
            _prev, _next, mem, avail, freed, word = struct.unpack_from(
                "<QQQiiQ", blob, off
            )
            # A meta with no group is either never-used or on the free
            # list; free_meta() zeroes it (meta.h:106-110).
            if mem == 0:
                continue
            last_idx = word & 31
            sizeclass = (word >> 6) & 63
            slots = last_idx + 1
            entry = agg[sizeclass]
            entry[0] += 1
            entry[1] += slots
            entry[2] += bin((avail | freed) & ((2 << last_idx) - 1)).count("1")
    return agg


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__.strip().splitlines()[-3].strip())
    dump_dir, binary = sys.argv[1], sys.argv[2]
    check_layout(binary)
    classes = size_classes(binary)
    found = stages(dump_dir)
    if not found:
        sys.exit(f"no s<N>_<M>.bin dumps in {dump_dir}")
    for stage in sorted(found):
        agg = census(found[stage])
        total_cap = sum(
            s * classes[c] * UNIT for c, (_g, s, _f) in agg.items() if c < 48
        )
        print(f"=== stage {stage}  capacity {total_cap / 1e6:.2f} MB")
        print("  class  slot      groups     slots      live      free   free MB")
        for c in sorted(agg):
            groups, slots, free = agg[c]
            slot = classes[c] * UNIT if c < 48 else 0
            print(
                f"  {c:5d} {slot:5d}B {groups:10d} {slots:9d} {slots - free:9d} "
                f"{free:9d} {free * slot / 1e6:9.2f}"
            )


if __name__ == "__main__":
    main()
