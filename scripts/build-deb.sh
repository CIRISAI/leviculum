#!/usr/bin/env bash
# Build the Debian packages for one architecture: musl-static, but for
# loquax on amd64 and arm64, which links against glibc to carry its window.
#
# Why a script rather than inline commands: this sequence used to be
# spelled out twice, in the Justfile's build-deb* recipes and in
# .woodpecker/nightly.yml, and the two drifted — the same failure
# deb-stamp.sh was extracted to prevent, one layer up. Commit 4c580cc5
# added manual pages to the three .deb manifests and the rendering step
# that produces them to the Justfile only, so every nightly from
# 2026-07-29 on died in `cargo deb` with
#
#   error: Can't resolve asset: leviculum-cli/../target/man/lnsd.1
#
# and the rolling release went stale for eight days. The nightly also
# never called deb-finalize.sh, which the Justfile recipes did: once the
# assets resolved it would have gone on publishing packages without
# md5sums, with a mis-named changelog and an empty Depends field. Both
# gaps are drift between two copies of one procedure, so there is now
# one implementation and two callers.
#
# Usage:
#   scripts/build-deb.sh amd64
#   scripts/build-deb.sh arm64
#   scripts/build-deb.sh armhf
#   LEVICULUM_ALLOC=system scripts/build-deb.sh amd64
#   scripts/build-deb.sh --check [amd64|arm64|armhf]   (default: the host's)
#   scripts/build-deb.sh --floor [amd64|arm64]          (default: the host's)
#
# --floor (1674) links what the packages link against glibc, at the floor
# GLIBC_FLOOR below, and stops there: the desktop loquax with DESKTOP_ARGS,
# and lnsd on the same gnu triple. A check (--check) links nothing, so a
# call to a glibc symbol newer than the floor compiles there and fails only
# in the package build: 42cae4a2 put `mallinfo2` (glibc 2.33) into the
# mailbox's census path and every nightly from 2026-11-08 on died in
# `ld.lld: error: undefined symbol: mallinfo2`. lnsd ships musl-static, but
# its gnu link at the floor is the cheap witness for the daemon code that a
# desktop binary may pull in next. `just deb-floor-check` calls it.
#
# --check (1593) runs `cargo check --release` over exactly the target triples
# and feature sets the packages are built with, and stops there: no stamps, no
# manual pages, no clean, no link, no package. `just deb-features-check` calls
# it for the host's architecture. It exists because loquax under
# `tui gui` on the gnu triple did not compile on master from 902bc129 to
# cbbd870d (1570 added a field to `GuiArgs` and not to two initialisers), and
# no coder gate builds that pair on that triple: `just loquax-gui` builds
# `gui` alone and `just loquax-tui` `tui` alone. The sets are read by both
# modes from the *_ARGS arrays below, so the check cannot drift from the build.
#
# lnsd runs on mimalloc, leviculum-cli's default feature (#437, 766), and
# lnpnd too, lnpnd's default feature (1592). LEVICULUM_ALLOC=system builds
# both daemons, in their plain and heapacct packages, on the libc allocator
# instead (`--no-default-features`, lnpnd keeping its interface families),
# for a node that has to be compared against the old build; anything else is
# refused.
#
# Expects scripts/deb-stamp.sh to have run. The versions are read from
# its stamp files rather than derived here, so an amd64 + arm64 pair
# from one build carries identical version strings even when the two
# builds straddle midnight UTC.
#
# Requires cargo-deb, cargo-zigbuild and a ziglang (`just _deb-prereqs`
# installs both; cargo-zigbuild finds a pip ziglang by itself), on amd64 as
# well since loquax's desktop build (1342), and python3 for the manual
# pages. Produces target/debian/*_<arch>.deb, which cargo-deb
# also hardlinks under target/<triple>/debian/: one per crate, plus
# leviculum-heapacct, the instrumented variant of the leviculum package, and
# lnpnd-heapacct, lnpnd's.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Where cargo-deb drops the finished packages. "$ROOT/target/debian" only holds
# while CARGO_TARGET_DIR is unset (scripts/cargo-target-dir.sh). The roff pages
# below are the other case and deliberately stay repo-relative: the three
# Cargo.toml manifests declare them as `../target/man/<tool>.1`, resolved
# against the manifest directory, so that path is pinned wherever cargo builds.
# shellcheck source-path=SCRIPTDIR/..
# shellcheck source=scripts/cargo-target-dir.sh
source "$ROOT/scripts/cargo-target-dir.sh"
TARGET="$(cargo_target_dir "$ROOT")"

# DESKTOP_TRIPLE is loquax's for the package (1342): one binary with the
# terminal view and the window, and the window needs glibc, because winit
# opens X11 and Wayland with dlopen, which a musl-static binary cannot. Both
# go through cargo zigbuild at the glibc floor GLIBC_FLOOR, so the package
# runs on Debian 11 and Ubuntu 20.04 on whatever this host's glibc is; the
# arm64 cross link needs no sysroot of its own, zig carries the glibc stubs
# (measured 2026-11-02 with ziglang 0.16.0: links, highest symbol
# GLIBC_2.30, runs `--version` under qemu-aarch64). armhf, where no desktop
# runs, keeps the musl-static terminal build and its `terminal` variant.
GLIBC_FLOOR=2.31
DESKTOP_TRIPLE=
CHECK_ONLY=
FLOOR_ONLY=
if [ "${1:-}" = --check ] || [ "${1:-}" = --floor ]; then
    if [ "$1" = --check ]; then CHECK_ONLY=1; else FLOOR_ONLY=1; fi
    shift
    # Without an architecture, --check takes the host's: the coder gate
    # checks what this machine's nightly would package.
    if [ -z "${1:-}" ]; then
        case "$(uname -m)" in
        x86_64) set -- amd64 ;;
        aarch64) set -- arm64 ;;
        armv7l) set -- armhf ;;
        *)
            echo "error: no package architecture for host $(uname -m)" >&2
            exit 2
            ;;
        esac
    fi
fi
ARCH="${1:-}"
case "$ARCH" in
amd64)
    TRIPLE=x86_64-unknown-linux-musl
    DESKTOP_TRIPLE=x86_64-unknown-linux-gnu
    ;;
arm64)
    TRIPLE=aarch64-unknown-linux-musl
    DESKTOP_TRIPLE=aarch64-unknown-linux-gnu
    ;;
# armv7 with hard float: Raspberry Pi 2, 3 and Zero 2 under a 32-bit OS
# (1153). Zig links it as it links arm64; built and run under qemu-arm on
# 2026-10-30, lnsd on mimalloc included.
armhf) TRIPLE=armv7-unknown-linux-musleabihf ;;
*)
    echo "usage: $0 [--check|--floor] <amd64|arm64|armhf>" >&2
    exit 2
    ;;
esac

# Every crate this script packages, and every binary they install between
# them. Kept in the same order as deb-stamp.sh's CRATES, which writes the
# version files read below; its last entry, lnflash, is packaged from the
# firmware bundle by scripts/build-lnflash-deb.sh instead.
CRATES=(leviculum-cli lnomad lblogd lnpnd loquax)
BINS=(lnsd lnstest lncp lnstatus lnprobe lnpath lnmgmt lnomad lblogd lnpnd loquax)
# loquax's terminal frontend is off by default (loquax/Cargo.toml); the package
# is what a person installs, so plain `loquax` opens the messenger. The musl
# loquax is armhf's package and every architecture's static tarball
# (collect-nightly-debs.sh); amd64's and arm64's packages take the desktop
# build below.
FEATURES=(--features loquax/tui)
DESKTOP_FEATURES=(--features "loquax/tui loquax/gui")

# The .deb is not always named after its crate: leviculum-cli ships as
# "leviculum". Same mapping as deb-stamp.sh's pkg_name; it is spelled
# out in both places rather than shared, because a two-line case is
# cheaper to read twice than an extra sourced file is to follow.
pkg_name() {
    case "$1" in
    leviculum-cli) echo leviculum ;;
    *) echo "$1" ;;
    esac
}

LEVICULUM_ALLOC="${LEVICULUM_ALLOC:-mimalloc}"
case "$LEVICULUM_ALLOC" in
mimalloc)
    alloc_args=()
    lnpnd_alloc_args=()
    ;;
system)
    alloc_args=(--no-default-features)
    lnpnd_alloc_args=(--no-default-features --features "ble serial i2p")
    ;;
*)
    echo "error: LEVICULUM_ALLOC=${LEVICULUM_ALLOC}; want mimalloc or system" >&2
    exit 2
    ;;
esac

# Every cargo compile this script runs, as the arguments after `--target`:
# the one source both modes read (1593). The heap-accounting pair is the
# packages' instrumented variants; the SYSTEM pair is the libc-allocator
# rebuild of the two daemons and is empty under mimalloc.
bin_args=()
for bin in "${BINS[@]}"; do
    bin_args+=(--bin "$bin")
done
MAIN_ARGS=("${bin_args[@]}" "${FEATURES[@]}")
DESKTOP_ARGS=(-p loquax --bin loquax "${DESKTOP_FEATURES[@]}")
LNSD_HEAPACCT_ARGS=(-p leviculum-cli --bin lnsd "${alloc_args[@]}" --features heap-accounting)
LNPND_HEAPACCT_ARGS=(-p lnpnd --bin lnpnd "${lnpnd_alloc_args[@]}" --features heap-accounting)
LNSD_SYSTEM_ARGS=(-p leviculum-cli --bin lnsd --no-default-features)
LNPND_SYSTEM_ARGS=(-p lnpnd --bin lnpnd "${lnpnd_alloc_args[@]}")

# cargo-zigbuild uses Zig as the C compiler/linker — the only way to
# reach aarch64-musl from an amd64 host without docker-in-docker or a
# dedicated arm64 runner. The check is plain cargo on every architecture:
# cargo-zigbuild 0.x has no `check` subcommand (`cargo zigbuild check` is
# refused, measured 2026-11-09), and the recipe checks the host's own
# architecture, whose C build scripts (mimalloc) the host compiler serves.
build_cmd=(cargo build)
[ "$ARCH" != amd64 ] && build_cmd=(cargo zigbuild)
check_cmd=(cargo check)

if [ -n "$FLOOR_ONLY" ]; then
    if [ -z "$DESKTOP_TRIPLE" ]; then
        echo "error: --floor ${ARCH}: no glibc build on this architecture" >&2
        exit 2
    fi
    echo "[build-deb] --floor ${ARCH} (${DESKTOP_TRIPLE}.${GLIBC_FLOOR})"
    cargo zigbuild --release --target "${DESKTOP_TRIPLE}.${GLIBC_FLOOR}" "${DESKTOP_ARGS[@]}"
    cargo zigbuild --release --target "${DESKTOP_TRIPLE}.${GLIBC_FLOOR}" -p leviculum-cli --bin lnsd
    echo "[build-deb] --floor ${ARCH}: loquax and lnsd link against glibc ${GLIBC_FLOOR}"
    exit 0
fi

if [ -n "$CHECK_ONLY" ]; then
    echo "[build-deb] --check ${ARCH} (${TRIPLE}${DESKTOP_TRIPLE:+, ${DESKTOP_TRIPLE}}) allocator=${LEVICULUM_ALLOC}"
    "${check_cmd[@]}" --release --target "$TRIPLE" "${MAIN_ARGS[@]}"
    if [ -n "$DESKTOP_TRIPLE" ]; then
        # The glibc floor is a link-time suffix; a check has nothing to link.
        "${check_cmd[@]}" --release --target "$DESKTOP_TRIPLE" "${DESKTOP_ARGS[@]}"
    fi
    if [ "$LEVICULUM_ALLOC" = system ]; then
        "${check_cmd[@]}" --release --target "$TRIPLE" "${LNSD_SYSTEM_ARGS[@]}"
        "${check_cmd[@]}" --release --target "$TRIPLE" "${LNPND_SYSTEM_ARGS[@]}"
    fi
    "${check_cmd[@]}" --release --target "$TRIPLE" "${LNSD_HEAPACCT_ARGS[@]}"
    "${check_cmd[@]}" --release --target "$TRIPLE" "${LNPND_HEAPACCT_ARGS[@]}"
    echo "[build-deb] --check ${ARCH}: every package's feature set compiles"
    exit 0
fi

# The stamp files are inputs to this script, not something it derives.
# Failing loudly beats building packages whose version strings silently
# disagree with the ones the other architecture shipped.
for crate in "${CRATES[@]}"; do
    [ -r ".deb-version-${crate}" ] || {
        echo "error: .deb-version-${crate} missing — run scripts/deb-stamp.sh first" >&2
        exit 1
    }
done
[ -r .build-id ] || {
    echo "error: .build-id missing — run scripts/deb-stamp.sh first" >&2
    exit 1
}
LEVICULUM_BUILD_ID="$(cat .build-id)"
export LEVICULUM_BUILD_ID
echo "[build-deb] ${ARCH} (${TRIPLE}) build-id=${LEVICULUM_BUILD_ID} lnsd and lnpnd allocator=${LEVICULUM_ALLOC}"

# Render docs/src/man/*.1.md to roff under target/man/, which the three
# .deb manifests install from. The Markdown is the single source: the
# nightly renders the same files with mdbook into the handbook at
# https://leviculum.network/docs/ and the leviculum-docs-nightly.tar.gz
# asset (scripts/build-docs-tarball.sh), so the shipped manual pages and
# the online documentation cannot drift apart.
#
# The selftest runs first because nothing downstream reads the roff: a
# mangled conversion does not fail the build, it ships. It cost a silently
# wrong lnomad(1) once already (a code span holding a backtick shifted
# every span after it), so the conversions have a standing guard.
python3 scripts/md2man.py --selftest
python3 scripts/md2man.py --outdir target/man docs/src/man/*.1.md

# Incremental-relink insurance: a repeated build with an unchanged
# LEVICULUM_BUILD_ID can skip relinking some binaries and leave them
# with a stale version string. Fresh CI containers rarely hit this, but
# the clean is cheap.
clean_args=()
for crate in "${CRATES[@]}"; do
    clean_args+=(-p "$crate")
done
cargo clean "${clean_args[@]}"
# `-p` cleans the configured target alone, the musl one; the desktop
# loquax is its own.
if [ -n "$DESKTOP_TRIPLE" ]; then
    cargo clean -p loquax --target "$DESKTOP_TRIPLE"
fi

"${build_cmd[@]}" --release --target "$TRIPLE" "${MAIN_ARGS[@]}"
if [ -n "$DESKTOP_TRIPLE" ]; then
    cargo zigbuild --release --target "${DESKTOP_TRIPLE}.${GLIBC_FLOOR}" "${DESKTOP_ARGS[@]}"
fi

# amd64's loquax a second time, for x86-64-v3 (1469, #474): the window draws
# through tiny-skia, whose wide types are picked by cfg(target_feature) at
# compile time, so only a build with AVX2 reaches them (1442 measured the map's
# wheel 23 to 29 % faster at 1527x2099). The package ships it as
# /usr/lib/loquax/loquax-v3 (loquax/Cargo.toml, variant amd64), and the
# baseline binary hands itself over to it on a CPU that runs it
# (loquax/src/cpu.rs). Its own target directory, so neither build takes the
# other's artefacts as fresh; pinned under this repository's target/ because
# the asset path is read relative to the manifest, as the manual pages' is.
# Only the target's flags: the build scripts and proc macros run here and
# stay baseline. arm64 has no second build, NEON is its baseline; the static
# tarball (collect-nightly-debs.sh) stays baseline too.
V3_TARGET="$ROOT/target/x86-64-v3"
v3_marker='(x86-64-v3)'
if [ "$ARCH" = amd64 ]; then
    CARGO_TARGET_DIR="$V3_TARGET" cargo clean -p loquax --target "$DESKTOP_TRIPLE"
    CARGO_TARGET_DIR="$V3_TARGET" CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C target-cpu=x86-64-v3" \
        cargo zigbuild --release --target "${DESKTOP_TRIPLE}.${GLIBC_FLOOR}" -p loquax --bin loquax \
        "${DESKTOP_FEATURES[@]}"
    # Read off the bytes, as the allocator below: `--version` of the v3
    # build ends in the marker (loquax/src/main.rs, cpu_suffix), the
    # baseline's does not, so a flag that did not reach rustc fails here.
    grep -qaF "$v3_marker" "$V3_TARGET/$DESKTOP_TRIPLE/release/loquax" || {
        echo "error: the x86-64-v3 loquax was not built for x86-64-v3 (no '$v3_marker')" >&2
        exit 1
    }
    if grep -qaF "$v3_marker" "$TARGET/$DESKTOP_TRIPLE/release/loquax"; then
        echo "error: the baseline loquax carries '$v3_marker'" >&2
        exit 1
    fi
fi

# The allocator is read off the bytes, like the heap-accounting marker below:
# an arm64 binary cannot be run here. lnsd and lnpnd log `Allocator: mimalloc`
# at startup only when they were built on mimalloc, so a package that would
# ship either daemon on the wrong allocator fails here instead of on a field
# node a day later, as resident memory nobody can account for.
mimalloc_marker='Allocator: mimalloc'
release_dir="$TARGET/$TRIPLE/release"
if [ "$LEVICULUM_ALLOC" = system ]; then
    "${build_cmd[@]}" --release --target "$TRIPLE" "${LNSD_SYSTEM_ARGS[@]}"
    "${build_cmd[@]}" --release --target "$TRIPLE" "${LNPND_SYSTEM_ARGS[@]}"
fi
require_allocator() {
    local bin="$release_dir/$1"
    if [ "$LEVICULUM_ALLOC" = mimalloc ]; then
        grep -qaF "$mimalloc_marker" "$bin" || {
            echo "error: $bin is not built on mimalloc (no '$mimalloc_marker')" >&2
            exit 1
        }
    elif grep -qaF "$mimalloc_marker" "$bin"; then
        echo "error: $bin carries '$mimalloc_marker' under LEVICULUM_ALLOC=system" >&2
        exit 1
    fi
}
require_allocator lnsd
require_allocator lnpnd

# One --deb-version per package: the three are versioned independently,
# so a single shared version string would be wrong for at least two of
# them.
#
# --no-strip: rust already strips debuginfo at link time (see workspace
# Cargo.toml [profile.release] strip = "debuginfo"). cargo-deb's default
# strip --strip-all corrupted x86_64 musl-static binaries (SIGSEGV at
# startup), so it is skipped entirely.
debs=()
for crate in "${CRATES[@]}"; do
    version="$(cat ".deb-version-${crate}")"
    deb_args=(--target "$TRIPLE")
    if [ "$crate" = loquax ]; then
        if [ "$ARCH" = amd64 ]; then
            deb_args=(--target "$DESKTOP_TRIPLE" --variant amd64)
        elif [ -n "$DESKTOP_TRIPLE" ]; then
            deb_args=(--target "$DESKTOP_TRIPLE")
        else
            deb_args+=(--variant terminal)
        fi
    fi
    cargo deb -p "$crate" "${deb_args[@]}" --no-build --no-strip \
        --deb-version "$version"
    debs+=("$TARGET/debian/$(pkg_name "$crate")_${version}_${ARCH}.deb")
done

# The heap-accounting variant of the leviculum package (Codeberg #437): the
# same six binaries, with lnsd rebuilt under `--features heap-accounting` so
# its SIGUSR1 census subtracts MEASURED live bytes instead of the model. The
# feature puts leviculum_std's counting shim in front of the allocator the
# plain lnsd runs (LEVICULUM_ALLOC), which only counts and forwards every
# call, so the CPU cost is bounded and fixed: three relaxed atomic adds per
# allocation and one per free, independent of heap size or uptime
# (heap_accounting.rs module docs; `heap-gap-bench --alloc-overhead` measures
# it per allocation).
#
# Only lnsd is rebuilt, in the same target directory so only leviculum-std
# and leviculum-cli recompile. Cargo uplifts the instrumented binary over
# release/lnsd, which the plain tarballs read afterwards, so the plain one is
# put back once the variant is packaged. `rm` before `cp`, never `cp` over:
# release/lnsd is a hardlink into deps/, and writing through it would put
# plain bytes into the cached instrumented artefact.
#
# lnpnd gets the same treatment for its own heapacct variant (550), below the
# two lnsd ones, so both plain binaries are saved and put back together.
instrumented_bins=(lnsd lnpnd)
plain_dir="$(mktemp -d)"
for bin in "${instrumented_bins[@]}"; do
    cp "$release_dir/$bin" "$plain_dir/$bin"
done
restore_plain_bins() {
    for bin in "${instrumented_bins[@]}"; do
        rm -f "$release_dir/$bin"
        cp "$plain_dir/$bin" "$release_dir/$bin"
        chmod 755 "$release_dir/$bin"
    done
    rm -rf "$plain_dir"
}
trap restore_plain_bins EXIT

# The instrumented lnsd has to keep its symbol table: the census's call-site
# sampler (LEVICULUM_HEAP_SAMPLE, 508) names frames from it at run time, and
# without it every stack prints as bare addresses. rust-lld keeps .symtab
# under the profile's `strip = "debuginfo"`, but zig, which links arm64 (and
# armhf), strips everything for it, so there the instrumented builds link
# unstripped: 23.5 MB with the standard library's DWARF instead of 9.7 MB
# (measured 2026-10-07). Stripping only the DWARF afterwards needs an objcopy
# this build does not otherwise require: the pinned toolchain's rust-objcopy
# cannot load its libLLVM, and zig's objcopy has no ELF strip. The DWARF is
# in no loaded segment, so it costs disk and not resident memory.
instrumented_env=()
if [ "$ARCH" != amd64 ]; then
    instrumented_env=(CARGO_PROFILE_RELEASE_STRIP=none)
fi
require_symtab() {
    local bin="${1:-lnsd}"
    grep -qaF '.symtab' "$release_dir/$bin" || {
        echo "error: the instrumented $bin has no symbol table; the heap sampler could not name a frame" >&2
        exit 1
    }
}

# Read off the bytes, not the exit code: an arm64 binary cannot be run here,
# and the startup line exists only in a heap-accounting build.
heapacct_marker='Heap accounting: on'
env "${instrumented_env[@]}" "${build_cmd[@]}" --release --target "$TRIPLE" "${LNSD_HEAPACCT_ARGS[@]}"
require_symtab
grep -qaF "$heapacct_marker" "$release_dir/lnsd" || {
    echo "error: lnsd built with --features heap-accounting does not carry the feature" >&2
    exit 1
}
require_allocator lnsd
heapacct_version="$(cat .deb-version-leviculum-cli)+heapacct"
cargo deb -p leviculum-cli --variant heapacct --target "$TRIPLE" --no-build --no-strip \
    --deb-version "$heapacct_version"
debs+=("$TARGET/debian/leviculum-heapacct_${heapacct_version}_${ARCH}.deb")

# lnpnd under `--features heap-accounting` (550), so the soak node's
# propagation daemon reports measured live bytes the way its lnsd does. Its
# own package, a variant of lnpnd's, because the daemon ships with its own
# unit and service user; lnpnd's build carries the same startup line.
env "${instrumented_env[@]}" "${build_cmd[@]}" --release --target "$TRIPLE" "${LNPND_HEAPACCT_ARGS[@]}"
require_symtab lnpnd
grep -qaF "$heapacct_marker" "$release_dir/lnpnd" || {
    echo "error: lnpnd built with --features heap-accounting does not carry the feature" >&2
    exit 1
}
require_allocator lnpnd
lnpnd_heapacct_version="$(cat .deb-version-lnpnd)+heapacct"
cargo deb -p lnpnd --variant heapacct --target "$TRIPLE" --no-build --no-strip \
    --deb-version "$lnpnd_heapacct_version"
debs+=("$TARGET/debian/lnpnd-heapacct_${lnpnd_heapacct_version}_${ARCH}.deb")

restore_plain_bins
trap - EXIT
if grep -qaF "$heapacct_marker" "$release_dir/lnsd"; then
    echo "error: the plain lnsd was not restored after the heap-accounting builds" >&2
    exit 1
fi
require_allocator lnsd
if grep -qaF "$heapacct_marker" "$release_dir/lnpnd"; then
    echo "error: the plain lnpnd was not restored after the heap-accounting build" >&2
    exit 1
fi
require_allocator lnpnd

# The things cargo-deb gets wrong or cannot produce: native changelog
# name, md5sums, empty Depends. Idempotent, in place.
bash scripts/deb-finalize.sh "${debs[@]}"

for deb in "${debs[@]}"; do
    echo "[build-deb] produced: ${deb}"
done
