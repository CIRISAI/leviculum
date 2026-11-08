#!/usr/bin/env bash
#
# The T114 firmware image is the same bytes from any clone of the same commit
# (order 1460, #474).
#
# Builds the T114 image twice through scripts/nrf-rustflags.sh's wrapper: once
# in this tree, once in a fresh clone of HEAD under a throwaway path, and
# compares the two images as flashed (the flat binary the UF2 runner makes,
# `nrf_flat_image` in scripts/nrf-elf.sh) by sha256. It also looks into both
# for the two tree paths, the home directory and the fixed build path, so a
# leak is named even when it happens to leak the same bytes twice. The T114
# because every pass links it; the other boards go through the same wrapper.
#
# On 2026-11-06, at be7f7ece, the tree and a clone gave 500702b8... (704,736
# B) and b72d975f... (704,776 B) although both were remapped and neither named
# a host path: cargo hashes the absolute path of leviculum-core, a path
# dependency outside the firmware workspace, into its crate hash, and LTO
# folds differently on different names. The wrapper now builds at one fixed
# path in a private mount namespace; its header says why.
#
# On 2026-11-07, at d4a425e7, the land gate's tree and a clone still differed
# by 14 bytes: leviculum-core's build floor, which a target directory keeps
# from the commit it was first built at (b07e759b) while a clone stamps its
# own. The wrapper now pins SOURCE_DATE_EPOCH to HEAD (order 1467). So this
# guard is only as strong as the tree's target is old: a target first built
# at HEAD cannot show a stale stamp.
#
#   NRF_REPRO sha_tree=<sha256> sha_clone=<sha256> verdict=<same|DIFFERENT>
#
# Red when either build fails, when the bytes differ, when an image names
# a host path, or when an image's [FW_BUILD] stamp is not HEAD's, clean
# (1537, the nRF half of 1521b). A tree with tracked changes is not HEAD (its embedded dirty
# flag alone differs), so then both builds are clones of HEAD at two paths.
# Without unprivileged user namespaces it says SKIP: the wrapper then builds
# in place by design, and red here would only restate that.
#
# The clones and their target directories go under
# ${XDG_CACHE_HOME:-$HOME/.cache}/leviculum-nrf-repro, never /tmp (a small
# tmpfs shared with the running gate), and are removed on exit.
#
# Usage:
#   bash scripts/check-nrf-reproducible.sh               # two builds, compare
#   bash scripts/check-nrf-reproducible.sh --selftest    # the verdict on fakes
#   bash scripts/check-nrf-reproducible.sh --compare A B # the verdict on two images

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAG="[nrf-repro]"
BIN=t114
FEATURES=bsp-t114

# verdict <image a> <image b> <path>...: the NRF_REPRO line; 1 when the
# images differ or either one contains one of the paths.
verdict() {
    local a="$1" b="$2" sa sb rc=0 img p
    shift 2
    sa="$(sha256sum "$a" | cut -d' ' -f1)" || return 1
    sb="$(sha256sum "$b" | cut -d' ' -f1)" || return 1
    if [ "$sa" = "$sb" ]; then
        echo "NRF_REPRO sha_tree=$sa sha_clone=$sb verdict=same"
    else
        echo "NRF_REPRO sha_tree=$sa sha_clone=$sb verdict=DIFFERENT"
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
# names <head> and a clean tree (1537). The build script watched .git/HEAD
# and the index only, a commit on a branch moves the branch's ref, and an
# image built just before a commit kept the parent's sha (1521b, on the M9).
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
    dir="$(mktemp -d "$ROOT/target/tmp/nrf-repro-selftest.XXXXXX")"
    printf 'image\0/src/leviculum-core/src/lib.rs\0' >"$dir/a"
    cp "$dir/a" "$dir/b"
    verdict "$dir/a" "$dir/b" /home/builder /mnt/leviculum/ >/dev/null ||
        { echo "$TAG selftest FAIL: two equal images were refused"; rc=1; }
    # The positive control: one injected path makes the bytes differ.
    printf 'image\0/home/builder/tree/leviculum-core/src/lib.rs\0' >"$dir/b"
    verdict "$dir/a" "$dir/b" >/dev/null &&
        { echo "$TAG selftest FAIL: an injected path string passed"; rc=1; }
    # The same path in both, equal bytes, still red by name.
    cp "$dir/b" "$dir/a"
    verdict "$dir/a" "$dir/b" /home/builder >/dev/null &&
        { echo "$TAG selftest FAIL: a host path leaked twice passed"; rc=1; }
    # And the fixed build path, which the remap must map to /src.
    printf 'image\0/mnt/leviculum/leviculum-core/src/lib.rs\0' >"$dir/a"
    cp "$dir/a" "$dir/b"
    verdict "$dir/a" "$dir/b" /mnt/leviculum/ >/dev/null &&
        { echo "$TAG selftest FAIL: the unremapped fixed build path passed"; rc=1; }
    # The stamp (1537): HEAD's passes, another commit's and a dirty one do not.
    printf 'x\0git_sha=1234abcd dirty=false version=0.1.5\0' >"$dir/a"
    stamp_is_head "$dir/a" 1234abcdef0123456789 >/dev/null ||
        { echo "$TAG selftest FAIL: HEAD's own stamp was refused"; rc=1; }
    stamp_is_head "$dir/a" 9999abcdef0123456789 >/dev/null &&
        { echo "$TAG selftest FAIL: another commit's stamp passed"; rc=1; }
    printf 'x\0git_sha=1234abcd dirty=true version=0.1.5\0' >"$dir/a"
    stamp_is_head "$dir/a" 1234abcdef0123456789 >/dev/null &&
        { echo "$TAG selftest FAIL: a dirty stamp passed"; rc=1; }
    printf 'x\0no stamp here\0' >"$dir/a"
    stamp_is_head "$dir/a" 1234abcdef0123456789 >/dev/null &&
        { echo "$TAG selftest FAIL: an image without a stamp passed"; rc=1; }
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
        verdict "$2" "$3" "$ROOT/" "$HOME/" /mnt/leviculum/
        exit $?
        ;;
    "") ;;
    *)
        echo "usage: $0 [--selftest | --compare <image a> <image b>]" >&2
        exit 2
        ;;
esac

selftest || exit 1

# shellcheck source=scripts/nrf-rustflags.sh
source "$ROOT/scripts/nrf-rustflags.sh"
if ! nrf_canon_available; then
    echo "$TAG SKIP no unprivileged user namespace on this host; the nRF build cannot reach its fixed path"
    exit 0
fi

# shellcheck source=scripts/cargo-target-dir.sh
source "$ROOT/scripts/cargo-target-dir.sh"
# shellcheck source=scripts/nrf-elf.sh
source "$ROOT/scripts/nrf-elf.sh"
start=$(date +%s)
head="$(git -C "$ROOT" rev-parse HEAD)" || exit 1
base="${XDG_CACHE_HOME:-$HOME/.cache}/leviculum-nrf-repro"
mkdir -p "$base" || exit 1
work="$(mktemp -d "$base/run.XXXXXX")" || exit 1
trap 'rm -rf "$work"' EXIT

# tree_build <tree> <name>: build the image in <tree> through that tree's own
# wrapper, and write the image as flashed to $work/<name>.bin.
tree_build() {
    local tree="$1" name="$2" elf
    if ! (cd "$tree/leviculum-nrf" &&
        bash "$tree/scripts/nrf-rustflags.sh" cargo build --release --bin "$BIN" --features "$FEATURES") \
        >"$work/$name.log" 2>&1; then
        cat "$work/$name.log"
        echo "$TAG FAIL the $BIN image did not build in $tree"
        return 1
    fi
    elf="$(cargo_target_dir "$tree/leviculum-nrf")/thumbv7em-none-eabihf/release/$BIN" || return 1
    nrf_flat_image "$elf" "$work/$name.bin" || {
        echo "$TAG FAIL could not flatten $elf"
        return 1
    }
}

# clone_build <dir> <name>: clone HEAD into <dir> and build there, with its
# own target directory: a CARGO_TARGET_DIR shared with the tree build would
# hand the clone the tree's artefacts, and at the fixed path cargo would take
# them as its own.
clone_build() {
    local dir="$1" name="$2"
    if ! { git clone -q --no-checkout "$ROOT" "$dir" && git -C "$dir" checkout -q --detach "$head"; }; then
        echo "$TAG FAIL could not clone $ROOT at $head"
        return 1
    fi
    (unset CARGO_TARGET_DIR && tree_build "$dir" "$name")
}

# A tree with tracked changes is not HEAD, and its embedded dirty flag alone
# would differ; then the first build is a second clone, at a longer path.
if [ -z "$(git -C "$ROOT" status --porcelain --untracked-files=no)" ]; then
    first="$ROOT"
    echo "$TAG build 1 of 2: this tree ($ROOT)"
    tree_build "$ROOT" tree || exit 1
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
