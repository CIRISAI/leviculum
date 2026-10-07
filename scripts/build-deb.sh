#!/usr/bin/env bash
# Build the musl-static Debian packages for one architecture.
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
#
# Expects scripts/deb-stamp.sh to have run. The versions are read from
# its stamp files rather than derived here, so an amd64 + arm64 pair
# from one build carries identical version strings even when the two
# builds straddle midnight UTC.
#
# Requires cargo-deb; arm64 additionally needs cargo-zigbuild and a
# ziglang on PATH (`just _deb-prereqs` installs both), and python3 for
# the manual pages. Produces target/debian/*_<arch>.deb, which cargo-deb
# also hardlinks under target/<triple>/debian/: one per crate, plus
# leviculum-heapacct and leviculum-heapacct-mimalloc, the instrumented
# variants of the leviculum package.

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

ARCH="${1:-}"
case "$ARCH" in
amd64) TRIPLE=x86_64-unknown-linux-musl ;;
arm64) TRIPLE=aarch64-unknown-linux-musl ;;
*)
    echo "usage: $0 <amd64|arm64>" >&2
    exit 2
    ;;
esac

# Every crate that ships a Debian package, and every binary the three of
# them install between them. Kept in the same order as deb-stamp.sh's
# CRATES, which writes the version files read below.
CRATES=(leviculum-cli lnomad lblogd lnpnd)
BINS=(lnsd lnstest lncp lnstatus lnprobe lnpath lnomad lblogd lnpnd)

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
echo "[build-deb] ${ARCH} (${TRIPLE}) build-id=${LEVICULUM_BUILD_ID}"

# Render docs/src/man/*.1.md to roff under target/man/, which the three
# .deb manifests install from. The Markdown is the single source: mdbook
# publishes the same files, so the shipped manual pages and the online
# documentation cannot drift apart.
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

# cargo-zigbuild uses Zig as the C compiler/linker — the only way to
# reach aarch64-musl from an amd64 host without docker-in-docker or a
# dedicated arm64 runner.
build_cmd=(cargo build)
[ "$ARCH" = arm64 ] && build_cmd=(cargo zigbuild)
bin_args=()
for bin in "${BINS[@]}"; do
    bin_args+=(--bin "$bin")
done
"${build_cmd[@]}" --release --target "$TRIPLE" "${bin_args[@]}"

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
    cargo deb -p "$crate" --target "$TRIPLE" --no-build --no-strip \
        --deb-version "$version"
    debs+=("$TARGET/debian/$(pkg_name "$crate")_${version}_${ARCH}.deb")
done

# The heap-accounting variant of the leviculum package (Codeberg #437): the
# same six binaries, with lnsd rebuilt under `--features heap-accounting` so
# its SIGUSR1 census subtracts MEASURED live bytes instead of the model. The
# feature puts leviculum_std's CountingAllocator in front of the system
# allocator, which only counts and forwards every call, so the CPU cost is
# bounded and fixed: three relaxed atomic adds per allocation and one per
# free, independent of heap size or uptime (heap_accounting.rs module docs;
# `heap-gap-bench --alloc-overhead` measures it per allocation).
#
# Only lnsd is rebuilt, in the same target directory so only leviculum-std
# and leviculum-cli recompile. Cargo uplifts the instrumented binary over
# release/lnsd, which the plain tarballs read afterwards, so the plain one is
# put back once the variant is packaged. `rm` before `cp`, never `cp` over:
# release/lnsd is a hardlink into deps/, and writing through it would put
# plain bytes into the cached instrumented artefact.
release_dir="$TARGET/$TRIPLE/release"
plain_lnsd="$(mktemp)"
cp "$release_dir/lnsd" "$plain_lnsd"
restore_plain_lnsd() {
    rm -f "$release_dir/lnsd"
    cp "$plain_lnsd" "$release_dir/lnsd"
    chmod 755 "$release_dir/lnsd"
    rm -f "$plain_lnsd"
}
trap restore_plain_lnsd EXIT

# The instrumented lnsd has to keep its symbol table: the census's call-site
# sampler (LEVICULUM_HEAP_SAMPLE, 508) names frames from it at run time, and
# without it every stack prints as bare addresses. rust-lld keeps .symtab
# under the profile's `strip = "debuginfo"`, but zig, which links arm64,
# strips everything for it, so on arm64 the instrumented builds link
# unstripped: 23.5 MB with the standard library's DWARF instead of 9.7 MB
# (measured 2026-10-07). Stripping only the DWARF afterwards needs an objcopy
# this build does not otherwise require: the pinned toolchain's rust-objcopy
# cannot load its libLLVM, and zig's objcopy has no ELF strip. The DWARF is
# in no loaded segment, so it costs disk and not resident memory.
instrumented_env=()
if [ "$ARCH" = arm64 ]; then
    instrumented_env=(CARGO_PROFILE_RELEASE_STRIP=none)
fi
require_symtab() {
    grep -qaF '.symtab' "$release_dir/lnsd" || {
        echo "error: the instrumented lnsd has no symbol table; the heap sampler could not name a frame" >&2
        exit 1
    }
}

# Read off the bytes, not the exit code: an arm64 binary cannot be run here,
# and the startup line exists only in a heap-accounting build.
heapacct_marker='Heap accounting: on'
env "${instrumented_env[@]}" "${build_cmd[@]}" --release --target "$TRIPLE" -p leviculum-cli --bin lnsd \
    --features heap-accounting
require_symtab
grep -qaF "$heapacct_marker" "$release_dir/lnsd" || {
    echo "error: lnsd built with --features heap-accounting does not carry the feature" >&2
    exit 1
}
heapacct_version="$(cat .deb-version-leviculum-cli)+heapacct"
cargo deb -p leviculum-cli --variant heapacct --target "$TRIPLE" --no-build --no-strip \
    --deb-version "$heapacct_version"
debs+=("$TARGET/debian/leviculum-heapacct_${heapacct_version}_${ARCH}.deb")

# The same instrumented lnsd on mimalloc (508), so the soak node can hold one
# census against the other and tell whether the allocator is what keeps the
# freed memory. `--features heap-accounting,alloc-mimalloc` wraps mimalloc in
# the counting shim; the binary must carry both startup lines.
mimalloc_marker='Allocator: mimalloc'
env "${instrumented_env[@]}" "${build_cmd[@]}" --release --target "$TRIPLE" -p leviculum-cli --bin lnsd \
    --features heap-accounting,alloc-mimalloc
require_symtab
for marker in "$heapacct_marker" "$mimalloc_marker"; do
    grep -qaF "$marker" "$release_dir/lnsd" || {
        echo "error: lnsd built with --features heap-accounting,alloc-mimalloc lacks '$marker'" >&2
        exit 1
    }
done
mimalloc_version="$(cat .deb-version-leviculum-cli)+heapacct+mimalloc"
cargo deb -p leviculum-cli --variant heapacct-mimalloc --target "$TRIPLE" --no-build --no-strip \
    --deb-version "$mimalloc_version"
debs+=("$TARGET/debian/leviculum-heapacct-mimalloc_${mimalloc_version}_${ARCH}.deb")

restore_plain_lnsd
trap - EXIT
if grep -qaF "$heapacct_marker" "$release_dir/lnsd" || grep -qaF "$mimalloc_marker" "$release_dir/lnsd"; then
    echo "error: the plain lnsd was not restored after the heap-accounting builds" >&2
    exit 1
fi

# The things cargo-deb gets wrong or cannot produce: native changelog
# name, md5sums, empty Depends. Idempotent, in place.
bash scripts/deb-finalize.sh "${debs[@]}"

for deb in "${debs[@]}"; do
    echo "[build-deb] produced: ${deb}"
done
