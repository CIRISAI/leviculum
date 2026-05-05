#!/usr/bin/env bash
# Builds the nightly release artefacts (tarballs + .deb packages) from
# the compiled musl binaries. Called from .woodpecker/nightly.yml after
# the two per-arch build steps finish.
#
# Expects:
#   target/x86_64-unknown-linux-musl/release/{lnsd,lns,lncp}
#   target/aarch64-unknown-linux-musl/release/{lnsd,lns,lncp}
#   target/debian/leviculum_*_amd64.deb
#   target/debian/leviculum_*_arm64.deb
#   LEVICULUM_BUILD_ID env var (for the versioned README line)
#
# Produces under dist/ with stable filenames (download URLs stay valid
# across nightly runs):
#   leviculum-nightly-linux-amd64.tar.gz + .sha256
#   leviculum-nightly-linux-arm64.tar.gz + .sha256
#   leviculum-nightly-amd64.deb          + .sha256
#   leviculum-nightly-arm64.deb          + .sha256
# The actual package version lives in the .deb control metadata and
# in the embedded --version string, not in the filename.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

BUILD_ID="${LEVICULUM_BUILD_ID:-unknown}"
DIST="dist"
rm -rf "$DIST"
mkdir -p "$DIST"

pack_tarball() {
    local arch_dash="$1"    # amd64 | arm64
    local rust_triple="$2"  # x86_64-unknown-linux-musl | aarch64-unknown-linux-musl
    local strip_tool="$3"   # strip | aarch64-linux-gnu-strip

    local name="leviculum-nightly-linux-${arch_dash}"
    local stage="$DIST/$name"
    local src="target/${rust_triple}/release"

    mkdir -p "$stage/bin" "$stage/doc"

    for bin in lnsd lns lncp; do
        cp "$src/$bin" "$stage/bin/$bin"
        "$strip_tool" "$stage/bin/$bin"
    done

    cp README.md LICENSE CHANGELOG.md "$stage/doc/"

    cat >"$stage/VERSION" <<EOF
leviculum nightly build
build-id: ${BUILD_ID}
arch: linux-${arch_dash}
EOF

    tar -C "$DIST" -czf "$DIST/$name.tar.gz" "$name"
    rm -rf "$stage"

    (cd "$DIST" && sha256sum "$name.tar.gz" >"$name.tar.gz.sha256")
}

copy_deb() {
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

pack_tarball amd64 x86_64-unknown-linux-musl  strip
pack_tarball arm64 aarch64-unknown-linux-musl aarch64-linux-gnu-strip
copy_deb amd64
copy_deb arm64

echo "=== dist/ ==="
ls -la "$DIST"
