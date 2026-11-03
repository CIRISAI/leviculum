#!/usr/bin/env bash
# shellcheck shell=bash
#
# Path remapping for the nRF firmware images (order 1368, #359).
#
# Every panic location rustc compiles into an image is the path it was handed
# for the source file, and for anything outside the firmware workspace that is
# absolute: the cargo registry, the git checkouts, leviculum-core beside
# leviculum-nrf. Order 1300 counted about 5.6 KB of such strings in every
# image, and 1326 measured the image size moving by 288 B between two
# checkouts. Stable cargo has no `trim-paths`, so this hands rustc
# `--remap-path-prefix` for each of those roots instead:
#
#   $CARGO_HOME/registry/src  -> /registry
#   $CARGO_HOME/git/checkouts -> /git
#   the repository root       -> /src      (leviculum-core, leviculum-lxmf, ...)
#   the rustc sysroot         -> /rustc
#   the firmware target dir   -> /target   (build-script output, OUT_DIR)
#
# All five are asked at build time (`cargo metadata`, `rustc --print sysroot`),
# so the result is the same image on every host and in every checkout. The
# firmware crate's own files were relative already (`src/lora.rs`) and stay so.
# rustc applies the LAST matching flag, so the target directory, which usually
# lies inside the repository, comes after the repository root.
#
# The flags go into CARGO_TARGET_THUMBV7EM_NONE_EABIHF_RUSTFLAGS, which cargo
# appends to `target.thumbv7em-none-eabihf.rustflags` in
# leviculum-nrf/.cargo/config.toml rather than replacing it (RUSTFLAGS would
# replace it and drop `-Tlink.x`). A plain `cargo build` in leviculum-nrf
# without this file still links, with the long paths in it.
#
# Two ways in:
#
#   source "$ROOT/scripts/nrf-rustflags.sh"; nrf_rustflags_export || exit 1
#   bash scripts/nrf-rustflags.sh cargo run --release --bin t114 ...   # wrapper
#
# The wrapper form is for the Justfile, whose recipes run under sh.

# nrf_remap_flags: prints the remap flags, space separated.
nrf_remap_flags() {
    local here root nrf target sysroot cargo_home p
    here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || return 1
    nrf="$here/../leviculum-nrf"
    root="$(cd "$here/.." && cargo metadata --format-version 1 --no-deps |
        python3 -I -c 'import json, sys; print(json.load(sys.stdin)["workspace_root"])')" || {
        echo "nrf-rustflags: no workspace_root from cargo metadata" >&2
        return 1
    }
    # shellcheck source=scripts/cargo-target-dir.sh
    source "$here/cargo-target-dir.sh"
    target="$(cargo_target_dir "$nrf")" || return 1
    sysroot="$(rustc --print sysroot)" || return 1
    # cargo's own rule for its home: CARGO_HOME, else ~/.cargo.
    cargo_home="${CARGO_HOME:-$HOME/.cargo}"
    for p in "$root" "$target" "$sysroot" "$cargo_home"; do
        case "$p" in
            *[[:space:]]*)
                echo "nrf-rustflags: '$p' contains whitespace, which the" >&2
                echo "nrf-rustflags: space-separated RUSTFLAGS form cannot carry" >&2
                return 1
                ;;
        esac
    done
    printf '%s ' \
        "--remap-path-prefix=$cargo_home/registry/src=/registry" \
        "--remap-path-prefix=$cargo_home/git/checkouts=/git" \
        "--remap-path-prefix=$sysroot=/rustc" \
        "--remap-path-prefix=$root=/src" \
        "--remap-path-prefix=$target=/target"
}

# nrf_rustflags_export: puts the flags where cargo reads them for the firmware
# triple, on top of whatever the caller already had there.
nrf_rustflags_export() {
    local flags
    flags="$(nrf_remap_flags)" || return 1
    export CARGO_TARGET_THUMBV7EM_NONE_EABIHF_RUSTFLAGS="${CARGO_TARGET_THUMBV7EM_NONE_EABIHF_RUSTFLAGS:+$CARGO_TARGET_THUMBV7EM_NONE_EABIHF_RUSTFLAGS }${flags% }"
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    [ "$#" -gt 0 ] || {
        echo "usage: $0 <command> [args...]   (or source it)" >&2
        exit 2
    }
    nrf_rustflags_export || exit 1
    exec "$@"
fi
