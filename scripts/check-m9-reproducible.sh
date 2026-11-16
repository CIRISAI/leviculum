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
# absolute source paths, a build time, and, once both were fixed, by cargo's
# crate hashes of the path dependencies, which carry the tree's absolute
# path. build-m9-image.sh now remaps the paths (scripts/nrf-rustflags.sh),
# pins SOURCE_DATE_EPOCH to the commit and builds at one fixed path in a
# private mount namespace; its header says why each is needed.
#
#   M9_REPRO sha_tree=<sha256> sha_clone=<sha256> verdict=<same|DIFFERENT>
#
# Red when either build fails, when the bytes differ, when an image names
# a host path, or when an image's [FW_BUILD] stamp is not HEAD's, clean
# (1521b). A tree with tracked changes is not HEAD (its embedded dirty
# flag alone differs), so then both builds are clones of HEAD at two paths.
# Without the esp toolchain it says SKIP, as build-m9-image.sh does, unless
# M9_IMAGE_REQUIRE_ESP=1; without unprivileged user namespaces too, because
# build-m9-image.sh then cannot build at its fixed path.
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

# stamp_is_head <image> <head sha>: 1 unless the image's [FW_BUILD] stamp
# names <head> and a clean tree (1521b). Window 7's image said
# git_sha=c1003ed9, the parent of the commit it was flashed for: it was
# built before that commit and nothing re-ran the build script.
stamp_is_head() {
    local img="$1" head="$2" stamp sha
    stamp="$(grep -aoE 'git_sha=[0-9a-z]+ dirty=(true|false)' "$img" | head -n 1)"
    sha="${stamp#git_sha=}"
    sha="${sha%% *}"
    if [ -z "$stamp" ]; then
        echo "$TAG FAIL $img carries no git_sha stamp"
        return 1
    fi
    if [ "${#sha}" -lt 7 ] || [ "${head#"$sha"}" = "$head" ]; then
        echo "$TAG FAIL $img says $stamp, HEAD is $head"
        return 1
    fi
    if [ "${stamp##* }" != "dirty=false" ]; then
        echo "$TAG FAIL $img says $stamp: built from a tree with tracked changes"
        return 1
    fi
    echo "$TAG stamp $stamp is HEAD"
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
    # The stamp (1521b): HEAD's passes, its parent's and a dirty one do not.
    printf 'x\0git_sha=1234abcd dirty=false version=0.1.5\0' >"$dir/a"
    stamp_is_head "$dir/a" 1234abcdef0123456789 >/dev/null ||
        { echo "$TAG selftest FAIL: HEAD's own stamp was refused"; rc=1; }
    stamp_is_head "$dir/a" 9999abcdef0123456789 >/dev/null &&
        { echo "$TAG selftest FAIL: another commit's stamp passed"; rc=1; }
    printf 'x\0git_sha=1234abcd dirty=true version=0.1.5\0' >"$dir/a"
    stamp_is_head "$dir/a" 1234abcdef0123456789 >/dev/null &&
        { echo "$TAG selftest FAIL: a dirty stamp passed"; rc=1; }
    rm -rf "$dir"
    [ "$rc" -eq 0 ] && echo "$TAG selftest ok: equal images pass, a differing or path-naming image is refused, a stamp not HEAD's is refused"
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

# build-m9-image.sh builds in place without one, and that image is not the
# reproducible one by design; red here would only restate it.
if ! unshare --user --map-root-user --mount true 2>/dev/null; then
    echo "$TAG SKIP no unprivileged user namespace on this host; build-m9-image.sh cannot build at its fixed path"
    exit 0
fi

# shellcheck source=scripts/cargo-target-dir.sh
source "$ROOT/scripts/cargo-target-dir.sh"
start=$(date +%s)
head="$(git -C "$ROOT" rev-parse HEAD)" || exit 1
base="${XDG_CACHE_HOME:-$HOME/.cache}/leviculum-m9-repro"
mkdir -p "$base" || exit 1
work="$(mktemp -d "$base/run.XXXXXX")" || exit 1
trap 'rm -rf "$work"' EXIT

# clone_build <dir> <name>: clone HEAD into <dir>, build there, copy the image
# to $work/<name>.bin. Its own target directory: a CARGO_TARGET_DIR shared
# with the tree build would hand the clone the tree's artefacts.
clone_build() {
    local dir="$1" name="$2"
    if ! { git clone -q --no-checkout "$ROOT" "$dir" && git -C "$dir" checkout -q --detach "$head"; }; then
        echo "$TAG FAIL could not clone $ROOT at $head"
        return 1
    fi
    if ! env -u CARGO_TARGET_DIR bash "$dir/scripts/build-m9-image.sh" >"$work/$name.log" 2>&1; then
        cat "$work/$name.log"
        echo "$TAG FAIL the image did not build in $dir"
        return 1
    fi
    cp "$dir/leviculum-esp/target/xtensa-esp32s3-none-elf/release/thinknode_m9.bin" "$work/$name.bin"
}

# A tree with tracked changes is not HEAD, and its embedded dirty flag alone
# would differ; then the first build is a second clone, at a longer path.
if [ -z "$(git -C "$ROOT" status --porcelain --untracked-files=no)" ]; then
    first="$ROOT"
    echo "$TAG build 1 of 2: this tree ($ROOT)"
    if ! bash "$ROOT/scripts/build-m9-image.sh" >"$work/tree.log" 2>&1; then
        cat "$work/tree.log"
        echo "$TAG FAIL the image did not build in this tree"
        exit 1
    fi
    tree_target="$(cargo_target_dir "$ROOT/leviculum-esp")" || exit 1
    cp "$tree_target/xtensa-esp32s3-none-elf/release/thinknode_m9.bin" "$work/tree.bin" || exit 1
else
    first="$work/first/another-clone-of-${head:0:12}"
    echo "$TAG the tree has tracked changes, so build 1 of 2 is a clone of $head too ($first)"
    clone_build "$first" tree || exit 1
fi

clone="$work/clone-of-${head:0:12}"
echo "$TAG build 2 of 2: a clone of $head ($clone)"
clone_build "$clone" clone || exit 1

verdict "$work/tree.bin" "$work/clone.bin" "$first/" "$clone/" "$HOME/" /mnt/leviculum/
rc=$?
stamp_is_head "$work/tree.bin" "$head" || rc=1
stamp_is_head "$work/clone.bin" "$head" || rc=1
echo "$TAG $(($(date +%s) - start)) s for both builds"
exit "$rc"
