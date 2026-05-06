#!/usr/bin/env bash
# Collects the .deb packages produced by cargo-deb in the build-amd64
# and build-arm64 steps, and produces a source tarball from HEAD.
# Stages everything under dist/ with stable filenames so the rolling
# release URLs stay valid across nightly runs.
#
# Expects:
#   target/debian/leviculum_*_amd64.deb
#   target/debian/leviculum_*_arm64.deb
#   git available on PATH
#
# Produces:
#   dist/leviculum-nightly-amd64.deb       + .sha256
#   dist/leviculum-nightly-arm64.deb       + .sha256
#   dist/leviculum-nightly-source.tar.gz   + .sha256
# The .deb version lives in the control metadata and the embedded
# --version string, not in the filename. The source tarball is a
# git archive of HEAD (tracked files only, no vendor/ submodules,
# no target/ build output).

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

DIST="dist"
rm -rf "$DIST"
mkdir -p "$DIST"

collect_deb() {
    local arch_dash="$1"  # amd64 | arm64
    local stable="leviculum-nightly-${arch_dash}.deb"

    # cargo-deb emits one .deb per arch under target/debian/. The
    # filename embeds the full nightly version, which changes each
    # run — glob to the unique file for this arch.
    local src
    src=$(ls -1 target/debian/leviculum_*_"${arch_dash}".deb 2>/dev/null | head -n1)
    if [ -z "${src:-}" ]; then
        echo "error: no .deb found for ${arch_dash} under target/debian/" >&2
        exit 1
    fi

    cp "$src" "$DIST/$stable"
    (cd "$DIST" && sha256sum "$stable" >"$stable.sha256")
}

collect_deb amd64
collect_deb arm64

# Source tarball at the same commit as the binaries. git archive
# emits only tracked files, so vendor/ submodules and target/ never
# enter the tarball. The --prefix gives `tar xzf` a clean directory
# layout: leviculum-nightly/{Cargo.toml, reticulum-*, …}.
git archive --format=tar.gz --prefix=leviculum-nightly/ \
    -o "$DIST/leviculum-nightly-source.tar.gz" HEAD
(cd "$DIST" && sha256sum leviculum-nightly-source.tar.gz \
    >leviculum-nightly-source.tar.gz.sha256)

echo "=== dist/ ==="
ls -la "$DIST"
