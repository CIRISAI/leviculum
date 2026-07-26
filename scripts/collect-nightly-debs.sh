#!/usr/bin/env bash
# Collects the .deb packages produced by cargo-deb, packs the raw
# binaries into per-arch userspace tarballs, and emits a source
# tarball from HEAD. Stages everything under dist/ with stable
# filenames so the rolling release URLs stay valid across nightly
# runs.
#
# Expects:
#   target/x86_64-unknown-linux-musl/release/{lnsd,lnstest,lncp,lnstatus,lnomad}
#   target/aarch64-unknown-linux-musl/release/{lnsd,lnstest,lncp,lnstatus,lnomad}
#   target/debian/leviculum_*_amd64.deb
#   target/debian/leviculum_*_arm64.deb
#   target/debian/lnomad_*_amd64.deb
#   target/debian/lnomad_*_arm64.deb
#   git available on PATH
#   LEVICULUM_BUILD_ID env var (embedded in the per-arch VERSION file)
#
# Produces:
#   dist/leviculum-nightly-amd64.deb       + .sha256
#   dist/leviculum-nightly-arm64.deb       + .sha256
#   dist/leviculum-nightly-amd64.tar.gz    + .sha256   (just the binaries)
#   dist/leviculum-nightly-arm64.tar.gz    + .sha256   (just the binaries)
#   dist/lnomad-nightly-amd64.deb          + .sha256
#   dist/lnomad-nightly-arm64.deb          + .sha256
#   dist/lnomad-nightly-amd64.tar.gz       + .sha256   (just the binary)
#   dist/lnomad-nightly-arm64.tar.gz       + .sha256   (just the binary)
#   dist/leviculum-nightly-source.tar.gz   + .sha256
# The .deb version lives in the control metadata and the embedded
# --version string, not in the filename. Binaries are pre-stripped
# at link time via [profile.release] strip = "debuginfo" in the
# workspace Cargo.toml — no extra strip step here. The source
# tarball is a git archive of HEAD (tracked files only).

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

DIST="dist"
rm -rf "$DIST"
mkdir -p "$DIST"

collect_deb() {
    local pkg="$1"        # leviculum | lnomad
    local arch_dash="$2"  # amd64 | arm64
    local stable="${pkg}-nightly-${arch_dash}.deb"

    # cargo-deb emits one .deb per package and arch under
    # target/debian/. The filename embeds the full nightly version,
    # which changes each run — glob to the unique file.
    local src
    src=$(ls -1 target/debian/"${pkg}"_*_"${arch_dash}".deb 2>/dev/null | head -n1)
    if [ -z "${src:-}" ]; then
        echo "error: no ${pkg} .deb found for ${arch_dash} under target/debian/" >&2
        exit 1
    fi

    cp "$src" "$DIST/$stable"
    (cd "$DIST" && sha256sum "$stable" >"$stable.sha256")
}

collect_deb leviculum amd64
collect_deb leviculum arm64
collect_deb lnomad amd64
collect_deb lnomad arm64

# Per-arch userspace binary tarball: the package's binaries plus
# README/LICENSE/CHANGELOG and a VERSION pointer. Drop-in for users
# who want the tools without root, system service, or .deb tooling.
pack_bin_tarball() {
    local pkg="$1"          # leviculum | lnomad
    local arch_dash="$2"    # amd64 | arm64
    local rust_triple="$3"  # x86_64-unknown-linux-musl | aarch64-unknown-linux-musl
    local readme_src="$4"   # per-package README, path relative to repo root
    shift 4                 # remaining args: binaries to include

    local name="${pkg}-nightly-${arch_dash}"
    local stage="$DIST/$name"
    local src="target/${rust_triple}/release"

    mkdir -p "$stage/bin" "$stage/doc"
    for bin in "$@"; do
        cp "$src/$bin" "$stage/bin/$bin"
    done
    cp "$readme_src" "$stage/doc/README.md"
    cp LICENSE CHANGELOG.md "$stage/doc/"
    cat >"$stage/VERSION" <<EOF
${pkg} nightly build
build-id: ${LEVICULUM_BUILD_ID:-unknown}
arch: linux-${arch_dash}
EOF

    tar -C "$DIST" -czf "$DIST/$name.tar.gz" "$name"
    rm -rf "$stage"
    (cd "$DIST" && sha256sum "$name.tar.gz" >"$name.tar.gz.sha256")
}

pack_bin_tarball leviculum amd64 x86_64-unknown-linux-musl README.md lnsd lnstest lncp lnstatus
pack_bin_tarball leviculum arm64 aarch64-unknown-linux-musl README.md lnsd lnstest lncp lnstatus
pack_bin_tarball lnomad amd64 x86_64-unknown-linux-musl lnomad/README.md lnomad
pack_bin_tarball lnomad arm64 aarch64-unknown-linux-musl lnomad/README.md lnomad

# Source tarball at the same commit as the binaries. git archive
# emits only tracked files, so vendor/ submodules and target/ never
# enter the tarball. The --prefix gives `tar xzf` a clean directory
# layout: leviculum-nightly-source/{Cargo.toml, reticulum-*, …}.
git archive --format=tar.gz --prefix=leviculum-nightly-source/ \
    -o "$DIST/leviculum-nightly-source.tar.gz" HEAD
(cd "$DIST" && sha256sum leviculum-nightly-source.tar.gz \
    >leviculum-nightly-source.tar.gz.sha256)

echo "=== dist/ ==="
ls -la "$DIST"
