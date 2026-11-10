#!/usr/bin/env bash
# shellcheck shell=bash
#
# Path remapping for the firmware images: the nRF boards (order 1368, #359)
# and the ThinkNode M9's xtensa image (order 1458, #474).
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
#   the rustc sysroot         -> /rustc    (asked in the firmware directory, so
#                                            the M9 gets the esp toolchain's,
#                                            whose rust-src build-std compiles)
#   the firmware target dir   -> /target   (build-script output, OUT_DIR)
#
# and, more specific than the first two and so after them, each registry index
# directory and git checkout the firmware's Cargo.lock uses -> /r and /g
# (`dependency_dir_remaps`, order 1620).
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
# without this file still links, with the long paths in it. The M9 takes the
# same flags through CARGO_TARGET_XTENSA_ESP32S3_NONE_ELF_RUSTFLAGS, appended
# the same way to leviculum-esp/.cargo/config.toml (`esp_rustflags_export`,
# called by scripts/build-m9-image.sh).
#
# The remap is not enough on its own (order 1460, #474). leviculum-nrf is a
# workspace of its own and leviculum-core, leviculum-lxmf, ... are path
# dependencies outside it, and cargo hashes the ABSOLUTE path of such a
# dependency into its `-C metadata`. That hash is the crate's StableCrateId,
# it lands in every mangled symbol, and LTO folds differently on different
# names: on 2026-11-06 the T114 image of one commit, remapped, was 704,736 B
# in one checkout and 704,776 B in a clone. So the wrapper form below runs its
# command in a private user and mount namespace (`unshare`, util-linux) that
# binds the repository at one fixed path, $NRF_CANON, whatever its real path
# is, and starts in the same place relative to it. The namespace's /mnt is a
# fresh tmpfs: nothing on the host is touched, concurrent builds do not meet,
# and a host directory under /mnt is not visible in there. RUSTC_WRAPPER is
# emptied in there, because an sccache server started outside cannot see
# $NRF_CANON. scripts/build-m9-image.sh makes the same hop for the M9 (1458).
#
# A host that refuses unprivileged user namespaces builds in place, and the
# wrapper says on stderr that this image is not the reproducible one.
#
# The path is not the only input that differs between a checkout and a clone
# (order 1467, #474). leviculum-core's build script stamps the image with a
# build floor (LEVICULUM_BUILD_UNIX_SECS: SOURCE_DATE_EPOCH, else HEAD's
# committer date) and on purpose does not rerun when HEAD moves, so a target
# directory keeps the date of the commit it was FIRST built at. On 2026-11-07
# the T114 image of d4a425e7 differed by 14 bytes between the land gate's
# push-tree and a clone: two literal-pool words in leviculum_core's
# `emission_secs` and the calendar code, 21:17:21 (b07e759b, where that target
# had first been built at $NRF_CANON) against 00:22:31 (d4a425e7). So the
# wrapper exports SOURCE_DATE_EPOCH as HEAD's committer date unless the caller
# set one, as scripts/build-m9-image.sh does for the M9; the build script
# reruns when it changes, so leviculum-core and what links it rebuild once per
# commit in a firmware target dir, which a change to core costs anyway.
#
# `cargo run` is refused through the wrapper: the runner flashes, and a UF2
# volume udisks mounts after the 1200-baud touch never shows up inside a
# private mount namespace. Flash with scripts/nrf-flash.sh, which builds
# through here and runs the runner outside on the image it built.
#
# Two ways in:
#
#   source "$ROOT/scripts/nrf-rustflags.sh"; nrf_cargo build --release ...
#   bash scripts/nrf-rustflags.sh cargo build --release --bin t114 ...  # wrapper
#
# The wrapper form is for the Justfile, whose recipes run under sh. `nrf_cargo`
# is the wrapper form for a script that sourced this file. Every nRF build goes
# through one of the two; `nrf_rustflags_export` alone gives the remap without
# the fixed path, which is what the wrapper calls once it is inside.

# firmware_remap_flags <firmware dir name>: prints the remap flags for the
# firmware crate in that directory of the repository (leviculum-nrf,
# leviculum-esp), space separated.
firmware_remap_flags() {
    local here root fw target sysroot cargo_home p
    here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || return 1
    fw="$here/../${1:?firmware_remap_flags: need the firmware directory}"
    root="$(cd "$here/.." && cargo metadata --format-version 1 --no-deps |
        python3 -I -c 'import json, sys; print(json.load(sys.stdin)["workspace_root"])')" || {
        echo "nrf-rustflags: no workspace_root from cargo metadata" >&2
        return 1
    }
    # shellcheck source=scripts/cargo-target-dir.sh
    source "$here/cargo-target-dir.sh"
    target="$(cargo_target_dir "$fw")" || return 1
    # In the firmware directory, because its rust-toolchain.toml picks the
    # compiler: leviculum-esp builds with the esp fork, whose sysroot differs.
    sysroot="$(cd "$fw" && rustc --print sysroot)" || return 1
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
    # After the two roots above, because rustc applies the last match.
    dependency_dir_remaps "$fw" "$cargo_home"
}

# dependency_dir_remaps <firmware dir> <cargo home>: prints one remap flag per
# dependency source directory the firmware's Cargo.lock resolves to below the
# cargo home, to a short name (order 1620, #235):
#
#   $CARGO_HOME/registry/src/index.crates.io-1949cf8c6b5b557f -> /r
#   $CARGO_HOME/git/checkouts/nrf-softdevice-c026ac046cd0977f/1d813cb -> /g
#
# Each prefix is in every panic location of the crates below it, and the
# board images carry them as strings: on the RAK4631 of 7b1ca161, 43
# registry paths and 17 git paths, 2440 B of a FLASH window that had 8240 B
# left. What a panic line names stays enough to find the source, the crate
# and its version (`/r/embassy-usb-0.5.1/src/lib.rs`, then line and column)
# or the checkout's crate directory
# (`/g/nrf-softdevice/src/ble/gatt_client.rs`); the index hash and the
# checkout revision are Cargo.lock's to say. The list comes from the lock
# rather than a glob over the cargo home, so two hosts with different
# leftovers there still hand rustc the same flags. Std's own paths
# (`/rustc/<commit>/library/...`) are baked into the shipped std and no flag
# reaches them.
dependency_dir_remaps() {
    local fw="$1" cargo_home="$2" meta
    meta="$(cd "$fw" && cargo metadata --format-version 1 --all-features)" || {
        echo "nrf-rustflags: 'cargo metadata' failed in $fw" >&2
        return 1
    }
    printf '%s' "$meta" | python3 -I -c '
import json, sys
home = sys.argv[1].rstrip("/")
reg, git = home + "/registry/src/", home + "/git/checkouts/"
dirs = set()
for pkg in json.load(sys.stdin)["packages"]:
    path = pkg["manifest_path"]
    if path.startswith(reg):
        dirs.add((reg + path[len(reg):].split("/")[0], "/r"))
    elif path.startswith(git):
        name, rev = path[len(git):].split("/")[:2]
        dirs.add((git + name + "/" + rev, "/g"))
for src, dst in sorted(dirs):
    print("--remap-path-prefix=%s=%s" % (src, dst), end=" ")
' "$cargo_home" || {
        echo "nrf-rustflags: could not read the dependency directories of $fw" >&2
        return 1
    }
}

# nrf_remap_flags: the flags for the nRF firmware workspace.
nrf_remap_flags() {
    firmware_remap_flags leviculum-nrf
}

# nrf_rustflags_export: puts the flags where cargo reads them for the firmware
# triple, on top of whatever the caller already had there.
nrf_rustflags_export() {
    local flags
    flags="$(nrf_remap_flags)" || return 1
    export CARGO_TARGET_THUMBV7EM_NONE_EABIHF_RUSTFLAGS="${CARGO_TARGET_THUMBV7EM_NONE_EABIHF_RUSTFLAGS:+$CARGO_TARGET_THUMBV7EM_NONE_EABIHF_RUSTFLAGS }${flags% }"
}

# esp_rustflags_export: the same for the M9's xtensa triple. Call it with the
# esp toolchain's environment loaded (~/export-esp.sh), as the build is.
esp_rustflags_export() {
    local flags
    flags="$(firmware_remap_flags leviculum-esp)" || return 1
    export CARGO_TARGET_XTENSA_ESP32S3_NONE_ELF_RUSTFLAGS="${CARGO_TARGET_XTENSA_ESP32S3_NONE_ELF_RUSTFLAGS:+$CARGO_TARGET_XTENSA_ESP32S3_NONE_ELF_RUSTFLAGS }${flags% }"
}

# firmware_epoch_export <repo root>: exports SOURCE_DATE_EPOCH as the
# committer date of HEAD in <repo root>, unless the caller already set one.
# Returns 1, with SOURCE_DATE_EPOCH unset, when there is no commit to ask
# (a source tarball); the build then carries the build clock.
firmware_epoch_export() {
    local secs
    if [ -n "${SOURCE_DATE_EPOCH:-}" ]; then
        export SOURCE_DATE_EPOCH
        return 0
    fi
    secs="$(git -C "${1:?firmware_epoch_export: need the repository root}" log -1 --format=%ct HEAD 2>/dev/null)" || secs=""
    if [ -z "$secs" ]; then
        unset SOURCE_DATE_EPOCH
        return 1
    fi
    export SOURCE_DATE_EPOCH="$secs"
}

# The fixed path every nRF and M9 image is built at, inside the namespace.
NRF_CANON=/mnt/leviculum

# nrf_canon_available: true when this host lets an unprivileged user open a
# user and mount namespace, which the hop to $NRF_CANON needs.
nrf_canon_available() {
    unshare --user --map-root-user --mount true 2>/dev/null
}

# nrf_cargo <cargo args...>: cargo for the nRF firmware workspace, at
# $NRF_CANON when the host allows it, with the remap in place. Same as the
# wrapper form below, for a script that sourced this file.
nrf_cargo() {
    local here
    here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || return 1
    bash "$here/nrf-rustflags.sh" cargo "$@"
}

# nrf_canon_exec <command...>: the wrapper. Runs the command with the remap
# exported, at $NRF_CANON when it can, else in place with a note on stderr.
nrf_canon_exec() {
    local root here inner
    root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)" || return 1
    if [ "${1:-}" = cargo ] && [ "${2:-}" = run ]; then
        echo "nrf-rustflags: 'cargo run' would run the flash runner inside a private" >&2
        echo "nrf-rustflags: mount namespace, where no UF2 volume appears; use" >&2
        echo "nrf-rustflags: scripts/nrf-flash.sh <bin> <features> (order 1460)" >&2
        return 2
    fi
    # Before the hop: the namespace inherits it, and git is asked on the
    # host's own path. Inside, the caller's value is kept as it is.
    firmware_epoch_export "$root" ||
        echo "nrf-rustflags: note: no git commit date at $root, the image carries the build clock" >&2
    if [ "$root" = "$NRF_CANON" ]; then
        nrf_rustflags_export || return 1
        exec "$@"
    fi
    if ! nrf_canon_available; then
        echo "nrf-rustflags: note: no unprivileged user namespace here; building at $root," >&2
        echo "nrf-rustflags: note: not at $NRF_CANON, so this image is not the reproducible one" >&2
        nrf_rustflags_export || return 1
        exec "$@"
    fi
    here="$(pwd -P)" || return 1
    case "$here" in
        "$root" | "$root"/*) inner="$NRF_CANON${here#"$root"}" ;;
        *) inner="$here" ;;
    esac
    # shellcheck disable=SC2016 # expanded by the inner shell
    RUSTC_WRAPPER="" CARGO_BUILD_RUSTC_WRAPPER="" \
        exec unshare --user --map-root-user --mount bash -c '
        mount -t tmpfs nrf-build "${2%/*}" && mkdir "$2" && mount --bind "$1" "$2" &&
            cd "$3" || exit 1
        shift 3
        exec bash "$0" "$@"' \
        "$NRF_CANON/scripts/nrf-rustflags.sh" "$root" "$NRF_CANON" "$inner" "$@"
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    [ "$#" -gt 0 ] || {
        echo "usage: $0 <command> [args...]   (or source it)" >&2
        exit 2
    }
    nrf_canon_exec "$@"
    exit $?
fi
