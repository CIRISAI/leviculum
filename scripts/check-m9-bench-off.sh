#!/usr/bin/env bash
#
# The M9's bench hook is compiled out of the release image (order 1533):
# the release image of a commit that carries the hook is the same code as
# the release image of a commit without it.
#
# Builds the release image (scripts/build-m9-image.sh, no --bench) in a
# clone of <base> and in a clone of <head>, with one SOURCE_DATE_EPOCH for
# both, and compares what the board loads: the bytes of every PT_LOAD
# segment of the two ELFs, each at its address. The one thing that
# differs by design is the [FW_BUILD] stamp, `git_sha=<short sha>`, which
# is masked to one value in both. And both are built with
# `-Zlocation-detail=none`: the hook's lines move every line below them,
# and a panic's `Location` (file, line, column) is a constant in the image,
# so every `unwrap`, `borrow_mut` and index check below the hook would
# differ by its line number alone (2026-11-07, the first run of this check:
# 44 B of code and the rodata apart). Without locations what remains is the
# code. The packaged .bin is not compared: its
# checksum and digest cover the stamp, and espflash writes the ELF's
# sha256 into the application descriptor, and the ELF's DWARF carries the
# stamp too.
#
#   M9_BENCH_OFF base=<sha> head=<sha> segments=<n> bytes=<n> verdict=<same|DIFFERENT>
#
# Red when either build fails, when the stamps' lengths differ (the mask
# cannot hold the layout then; the line says so), or when any loaded byte
# differs. The clones and their target directories go under
# ${XDG_CACHE_HOME:-$HOME/.cache}/leviculum-m9-bench-off, never /tmp, and
# are removed on exit. Without the esp toolchain it says SKIP.
#
# Usage:
#   bash scripts/check-m9-bench-off.sh <base> [<head>]   # head: HEAD
#   bash scripts/check-m9-bench-off.sh --compare <base.elf> <head.elf> <base-sha> <head-sha>
#   bash scripts/check-m9-bench-off.sh --selftest

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAG="[m9-bench-off]"

# compare <base elf> <head elf> <base short sha> <head short sha>: the
# M9_BENCH_OFF line; 1 unless every loaded byte is the same.
compare() {
    python3 -I - "$@" <<'PY'
import struct, sys
a_path, b_path, a_sha, b_sha = sys.argv[1:5]

def loads(path):
    data = open(path, "rb").read()
    if data[:4] != b"\x7fELF" or data[4] != 1 or data[5] != 1:
        sys.exit(f"{path}: not a 32-bit little-endian ELF")
    phoff, = struct.unpack_from("<I", data, 28)
    phentsize, phnum = struct.unpack_from("<HH", data, 42)
    out = []
    for i in range(phnum):
        p_type, p_offset, p_vaddr, _, p_filesz = struct.unpack_from("<IIIII", data, phoff + i * phentsize)
        if p_type == 1 and p_filesz:
            out.append((p_vaddr, data[p_offset:p_offset + p_filesz]))
    return sorted(out)

stamp = lambda sha: b"git_sha=" + sha.encode()
if len(a_sha) != len(b_sha):
    print(f"M9_BENCH_OFF base={a_sha} head={b_sha} verdict=DIFFERENT why=stamp-lengths")
    sys.exit(1)
a = [(v, d.replace(stamp(a_sha), stamp(b_sha))) for v, d in loads(a_path)]
b = loads(b_path)
same = a == b
size = sum(len(d) for _, d in b)
print(f"M9_BENCH_OFF base={a_sha} head={b_sha} segments={len(b)} bytes={size} verdict={'same' if same else 'DIFFERENT'}")
if not same:
    for (va, da), (vb, db) in zip(a, b):
        if va != vb or len(da) != len(db):
            print(f"  segment at {va:#x} ({len(da)} B) vs {vb:#x} ({len(db)} B)")
        elif da != db:
            first = next(i for i in range(len(da)) if da[i] != db[i])
            print(f"  segment at {va:#x}: first difference at {va + first:#x}")
    if len(a) != len(b):
        print(f"  {len(a)} vs {len(b)} segments")
sys.exit(0 if same else 1)
PY
}

# fake_elf <path> <payload>: a 32-bit ELF with one PT_LOAD segment holding
# <payload>, for the selftest.
fake_elf() {
    python3 -I - "$1" "$2" <<'PY'
import struct, sys
path, payload = sys.argv[1], sys.argv[2].encode()
header = bytearray(52)
header[:6] = b"\x7fELF\x01\x01"
struct.pack_into("<I", header, 28, 52)
struct.pack_into("<HH", header, 42, 32, 1)
ph = struct.pack("<IIIIIIII", 1, 84, 0x42000000, 0x42000000, len(payload), len(payload), 5, 4)
open(path, "wb").write(bytes(header) + ph + payload)
PY
}

selftest() {
    local dir rc=0
    mkdir -p "$ROOT/target/tmp"
    dir="$(mktemp -d "$ROOT/target/tmp/m9-bench-off-selftest.XXXXXX")"
    fake_elf "$dir/a" "code git_sha=1111aaaa dirty=false"
    fake_elf "$dir/b" "code git_sha=2222bbbb dirty=false"
    compare "$dir/a" "$dir/b" 1111aaaa 2222bbbb >/dev/null ||
        { echo "$TAG selftest FAIL: two images differing only by their stamp were refused"; rc=1; }
    # The positive control: one byte of code differs.
    fake_elf "$dir/b" "cide git_sha=2222bbbb dirty=false"
    compare "$dir/a" "$dir/b" 1111aaaa 2222bbbb >/dev/null &&
        { echo "$TAG selftest FAIL: a changed byte of code passed"; rc=1; }
    fake_elf "$dir/b" "code git_sha=2222bbbbc dirty=false"
    compare "$dir/a" "$dir/b" 1111aaaa 2222bbbbc >/dev/null &&
        { echo "$TAG selftest FAIL: stamps of two lengths passed"; rc=1; }
    rm -rf "$dir"
    [ "$rc" -eq 0 ] && echo "$TAG selftest ok: a stamp alone passes, a changed byte and a longer stamp are refused"
    return "$rc"
}

case "${1:-}" in
    --selftest)
        selftest
        exit $?
        ;;
    --compare)
        [ "$#" -eq 5 ] || { echo "usage: $0 --compare <base.elf> <head.elf> <base-sha> <head-sha>" >&2; exit 2; }
        compare "$2" "$3" "$4" "$5"
        exit $?
        ;;
    "" | -*)
        echo "usage: $0 <base> [<head>] | --compare ... | --selftest" >&2
        exit 2
        ;;
esac

if [ ! -f "$HOME/export-esp.sh" ]; then
    echo "$TAG SKIP no esp toolchain on this host (~/export-esp.sh)"
    exit 0
fi
selftest || exit 1

base="$(git -C "$ROOT" rev-parse --verify "$1^{commit}")" || exit 1
head="$(git -C "$ROOT" rev-parse --verify "${2:-HEAD}^{commit}")" || exit 1
# One date for both: build-m9-image.sh keeps a SOURCE_DATE_EPOCH it is
# given, and the application descriptor carries it.
SOURCE_DATE_EPOCH="$(git -C "$ROOT" log -1 --format=%ct "$head")" || exit 1
export SOURCE_DATE_EPOCH
cache="${XDG_CACHE_HOME:-$HOME/.cache}/leviculum-m9-bench-off"
mkdir -p "$cache" || exit 1
work="$(mktemp -d "$cache/run.XXXXXX")" || exit 1
[ -n "${M9_BENCH_OFF_KEEP:-}" ] || trap 'rm -rf "$work"' EXIT
start=$(date +%s)

# build_at <commit> <name>: the release image of <commit> built in a clone;
# its ELF copied to $work/<name>.elf, its short sha to $work/<name>.sha.
build_at() {
    local commit="$1" name="$2" dir="$work/$2"
    if ! { git clone -q --no-checkout "$ROOT" "$dir" && git -C "$dir" checkout -q --detach "$commit"; }; then
        echo "$TAG FAIL could not clone $ROOT at $commit"
        return 1
    fi
    echo "$TAG building the release image of $commit"
    if ! env -u CARGO_TARGET_DIR -u M9_BENCH -u LEVICULUM_M9_BENCH \
        CARGO_TARGET_XTENSA_ESP32S3_NONE_ELF_RUSTFLAGS="-Zlocation-detail=none" \
        bash "$dir/scripts/build-m9-image.sh" >"$work/$name.log" 2>&1; then
        cat "$work/$name.log"
        echo "$TAG FAIL the image of $commit did not build"
        return 1
    fi
    cp "$dir/leviculum-esp/target/xtensa-esp32s3-none-elf/release/thinknode_m9" "$work/$name.elf" &&
        git -C "$dir" rev-parse --short HEAD >"$work/$name.sha"
}

build_at "$base" base || exit 1
build_at "$head" head || exit 1
compare "$work/base.elf" "$work/head.elf" "$(cat "$work/base.sha")" "$(cat "$work/head.sha")"
rc=$?
echo "$TAG $(($(date +%s) - start)) s for both builds"
exit "$rc"
