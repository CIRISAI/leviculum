#!/usr/bin/env bash
#
# The ThinkNode M9 image is the same bytes from any clone of the same commit
# (order 1458, #474).
#
# Builds the image twice with scripts/build-m9-image.sh: once in this tree,
# once in a fresh clone of HEAD under a throwaway path, and compares the two
# `thinknode_m9.bin` by sha256. It also looks into both images for the two
# tree paths and the home directory, so a leak is named even when it happens
# to leak the same bytes twice. On 2026-11-06 the two builds differed by 88
# absolute source paths and a build time; build-m9-image.sh now remaps the
# paths (scripts/nrf-rustflags.sh) and pins SOURCE_DATE_EPOCH to the commit.
#
#   M9_REPRO sha_tree=<sha256> sha_clone=<sha256> verdict=<same|DIFFERENT>
#
# Red when either build fails, when the bytes differ, when an image names a
# host path, or when the tree has tracked changes (the clone would build a
# different commit, and the embedded dirty flag alone differs). Without the
# esp toolchain it says SKIP, as build-m9-image.sh does, unless
# M9_IMAGE_REQUIRE_ESP=1.
#
# The clone and its target directory (about 0.9 GB) go under
# ${XDG_CACHE_HOME:-$HOME/.cache}/leviculum-m9-repro, never /tmp (a small
# tmpfs shared with the running gate), and are removed on exit.
#
# Usage:
#   bash scripts/check-m9-reproducible.sh               # two builds, compare
#   bash scripts/check-m9-reproducible.sh --selftest    # the verdict on fakes
#   bash scripts/check-m9-reproducible.sh --compare A B # the verdict on two images

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAG="[m9-repro]"

# verdict <image a> <image b> <path>...: the M9_REPRO line; 1 when the
# images differ or either one contains one of the paths.
verdict() {
    local a="$1" b="$2" sa sb rc=0 img p
    shift 2
    sa="$(sha256sum "$a" | cut -d' ' -f1)" || return 1
    sb="$(sha256sum "$b" | cut -d' ' -f1)" || return 1
    if [ "$sa" = "$sb" ]; then
        echo "M9_REPRO sha_tree=$sa sha_clone=$sb verdict=same"
    else
        echo "M9_REPRO sha_tree=$sa sha_clone=$sb verdict=DIFFERENT"
        echo "$TAG FAIL the two images differ ($(stat -c %s "$a") vs $(stat -c %s "$b") B)"
        rc=1
    fi
    for img in "$a" "$b"; do
        for p in "$@"; do
            [ -n "$p" ] || continue
            if grep -qaF -- "$p" "$img"; then
                echo "$TAG FAIL $img names the host path $p"
                rc=1
            fi
        done
    done
    return "$rc"
}

selftest() {
    local dir rc=0
    mkdir -p "$ROOT/target/tmp"
    dir="$(mktemp -d "$ROOT/target/tmp/m9-repro-selftest.XXXXXX")"
    printf 'image\0/src/leviculum-core/src/lib.rs\0' >"$dir/a"
    cp "$dir/a" "$dir/b"
    verdict "$dir/a" "$dir/b" /home/builder >/dev/null ||
        { echo "$TAG selftest FAIL: two equal images were refused"; rc=1; }
    # The positive control: one injected path makes the bytes differ.
    printf 'image\0/home/builder/tree/leviculum-core/src/lib.rs\0' >"$dir/b"
    verdict "$dir/a" "$dir/b" >/dev/null &&
        { echo "$TAG selftest FAIL: an injected path string passed"; rc=1; }
    # And the same path in both, equal bytes, still red by name.
    cp "$dir/b" "$dir/a"
    verdict "$dir/a" "$dir/b" /home/builder >/dev/null &&
        { echo "$TAG selftest FAIL: a host path leaked twice passed"; rc=1; }
    rm -rf "$dir"
    [ "$rc" -eq 0 ] && echo "$TAG selftest ok: equal images pass, a differing or path-naming image is refused"
    return "$rc"
}

case "${1:-}" in
    --selftest)
        selftest
        exit $?
        ;;
    --compare)
        [ "$#" -eq 3 ] || { echo "usage: $0 --compare <image a> <image b>" >&2; exit 2; }
        verdict "$2" "$3" "$ROOT/" "$HOME/"
        exit $?
        ;;
    "") ;;
    *)
        echo "usage: $0 [--selftest | --compare <image a> <image b>]" >&2
        exit 2
        ;;
esac

if [ ! -f "$HOME/export-esp.sh" ]; then
    if [ "${M9_IMAGE_REQUIRE_ESP:-0}" = 1 ]; then
        echo "$TAG FAIL no esp toolchain (~/export-esp.sh) and M9_IMAGE_REQUIRE_ESP=1"
        exit 1
    fi
    echo "$TAG SKIP no esp toolchain on this host (~/export-esp.sh; scripts/install-ci.sh installs it)"
    exit 0
fi

selftest || exit 1

if [ -n "$(git -C "$ROOT" status --porcelain --untracked-files=no)" ]; then
    git -C "$ROOT" status --short --untracked-files=no
    echo "$TAG FAIL the tree has tracked changes; the clone of HEAD would build a different image. Commit first."
    exit 1
fi

# shellcheck source=scripts/cargo-target-dir.sh
source "$ROOT/scripts/cargo-target-dir.sh"
start=$(date +%s)
head="$(git -C "$ROOT" rev-parse HEAD)" || exit 1
base="${XDG_CACHE_HOME:-$HOME/.cache}/leviculum-m9-repro"
mkdir -p "$base" || exit 1
work="$(mktemp -d "$base/run.XXXXXX")" || exit 1
trap 'rm -rf "$work"' EXIT

echo "$TAG build 1 of 2: this tree ($ROOT)"
if ! bash "$ROOT/scripts/build-m9-image.sh" >"$work/tree.log" 2>&1; then
    cat "$work/tree.log"
    echo "$TAG FAIL the image did not build in this tree"
    exit 1
fi
tree_target="$(cargo_target_dir "$ROOT/leviculum-esp")" || exit 1
cp "$tree_target/xtensa-esp32s3-none-elf/release/thinknode_m9.bin" "$work/tree.bin" || exit 1

clone="$work/clone-of-${head:0:12}"
echo "$TAG build 2 of 2: a clone of $head ($clone)"
if ! { git clone -q --no-checkout "$ROOT" "$clone" && git -C "$clone" checkout -q --detach "$head"; }; then
    echo "$TAG FAIL could not clone $ROOT at $head"
    exit 1
fi
# Its own target directory: a CARGO_TARGET_DIR shared with the tree build
# would hand the clone the tree's artefacts.
if ! env -u CARGO_TARGET_DIR bash "$clone/scripts/build-m9-image.sh" >"$work/clone.log" 2>&1; then
    cat "$work/clone.log"
    echo "$TAG FAIL the image did not build in the clone"
    exit 1
fi
cp "$clone/leviculum-esp/target/xtensa-esp32s3-none-elf/release/thinknode_m9.bin" "$work/clone.bin" || exit 1

verdict "$work/tree.bin" "$work/clone.bin" "$ROOT/" "$clone/" "$HOME/"
rc=$?
echo "$TAG $(($(date +%s) - start)) s for both builds"
exit "$rc"
