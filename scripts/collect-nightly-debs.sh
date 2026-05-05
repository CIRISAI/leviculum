#!/usr/bin/env bash
# Collects the .deb packages produced by cargo-deb in the build-amd64
# and build-arm64 steps and stages them under dist/ with stable
# filenames, so the rolling release URL stays valid across nightly
# runs.
#
# Expects:
#   target/debian/leviculum_*_amd64.deb
#   target/debian/leviculum_*_arm64.deb
#
# Produces:
#   dist/leviculum-nightly-amd64.deb       + .sha256
#   dist/leviculum-nightly-arm64.deb       + .sha256
# The actual package version lives in the .deb control metadata and
# the embedded --version string, not in the filename.

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

echo "=== dist/ ==="
ls -la "$DIST"
