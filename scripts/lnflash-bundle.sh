#!/usr/bin/env bash
# Build the lnflash tarball: a stranger unpacks it on any Linux, runs one
# binary, and their T114 ends up running our firmware.
#
#     tar xzf lnflash-<version>.tar.gz
#     cd lnflash-<version>
#     sudo ./lnflash
#
# Nothing installed, no repo, no network. This script is the build side of
# that promise, so everything it puts in the tarball comes from this checkout
# and nowhere else — a bundle built from a Meshtastic checkout is exactly the
# hidden dependency our clone-and-deploy policy forbids.
#
# Output: target/lnflash/lnflash-<version>.tar.gz
#
#   OUT_DIR    where to write (default target/lnflash)
#   SKIP_FIRMWARE=1  reuse an already-built firmware ELF instead of building
#                    it. For iterating on the bundle itself; a release build
#                    must not use it.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

NRF_DIR="$ROOT/leviculum-nrf"
NRF_TARGET="$NRF_DIR/target/thumbv7em-none-eabihf/release"
OUT_DIR="${OUT_DIR:-$ROOT/target/lnflash}"

# Fixed properties of the Adafruit nRF52 UF2 family, the same two constants
# leviculum-nrf/tools/uf2-runner.sh writes into every flashed image.
FLASH_BASE=0x27000
FAMILY_ID=0xADA52840

VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
GIT_SHA="$(git rev-parse --short HEAD)"
BUILT="$(git log -1 --format=%cs)"
STAGE="$OUT_DIR/lnflash-$VERSION"
TARBALL="$OUT_DIR/lnflash-$VERSION.tar.gz"

say() { printf '[lnflash-bundle] %s\n' "$*"; }

if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
    say "WARNING: tracked files are modified. The firmware will report"
    say "         dirty=true and the manifest records git_sha=$GIT_SHA anyway."
fi

# --- The application image ---------------------------------------------------
# Built here rather than vendored, so a bundle always carries the firmware the
# commit it was built from produces.
if [ "${SKIP_FIRMWARE:-0}" != 1 ]; then
    say "building the T114 firmware"
    (cd "$NRF_DIR" && cargo build --release --bin t114 --features bsp-t114)
fi

ELF="$NRF_TARGET/t114"
[ -f "$ELF" ] || { echo "no firmware ELF at $ELF" >&2; exit 1; }

# ELF -> flat binary. -R .bss -R .uninit excludes NOBITS sections, without
# which a pre-2020 llvm-objcopy emits a ~500 MB binary at the wrong addresses.
find_objcopy() {
    local sysroot candidate
    sysroot="$(rustc --print sysroot 2>/dev/null || true)"
    if [ -n "$sysroot" ]; then
        candidate="$(find "$sysroot/lib/rustlib" -name llvm-objcopy -type f 2>/dev/null | head -1)"
        [ -n "$candidate" ] && { echo "$candidate"; return; }
    fi
    command -v llvm-objcopy || command -v arm-none-eabi-objcopy || true
}
OBJCOPY="$(find_objcopy)"
[ -n "$OBJCOPY" ] || { echo "no objcopy found (llvm-tools or arm-none-eabi-binutils)" >&2; exit 1; }

# bin2uf2 is leviculum-nrf's, already the producer of every image we flash.
# Reusing it keeps one UF2 writer on the build side rather than two.
BIN2UF2="$NRF_DIR/target/bin2uf2"
if [ ! -x "$BIN2UF2" ] || [ "$NRF_DIR/tools/bin2uf2.rs" -nt "$BIN2UF2" ]; then
    say "building bin2uf2"
    rustc -O -o "$BIN2UF2" "$NRF_DIR/tools/bin2uf2.rs"
fi

say "converting the firmware to UF2"
"$OBJCOPY" -O binary -R .bss -R .uninit "$ELF" "$OUT_DIR/t114.bin.tmp" 2>/dev/null \
    || { mkdir -p "$OUT_DIR"; "$OBJCOPY" -O binary -R .bss -R .uninit "$ELF" "$OUT_DIR/t114.bin.tmp"; }
"$BIN2UF2" --base "$FLASH_BASE" --family "$FAMILY_ID" "$OUT_DIR/t114.bin.tmp" "$OUT_DIR/t114.uf2.tmp"

# --- The binary --------------------------------------------------------------
# musl-static by workspace default, so it runs on any Linux with no libc of
# its own to find.
say "building lnflash"
cargo build -p lnflash --release
LNFLASH="$ROOT/target/x86_64-unknown-linux-musl/release/lnflash"
[ -f "$LNFLASH" ] || { echo "no lnflash binary at $LNFLASH" >&2; exit 1; }

# --- Assemble ----------------------------------------------------------------
rm -rf "$STAGE"
mkdir -p "$STAGE/firmware/t114"
install -m 0755 "$LNFLASH" "$STAGE/lnflash"
install -m 0644 "$OUT_DIR/t114.uf2.tmp" "$STAGE/firmware/t114/leviculum-t114-$VERSION.uf2"
# The SoftDevice travels as a file with its licence beside it — never linked
# into the binary, because Nordic's clauses 4 and 5 cannot become part of one
# combined work with AGPL code.
install -m 0644 "$ROOT/lnflash/payload/t114/s140_nrf52_7.3.0_softdevice.hex" "$STAGE/firmware/t114/"
install -m 0644 "$ROOT/lnflash/payload/t114/s140_nrf52_7.3.0_license-agreement.txt" "$STAGE/firmware/t114/"
install -m 0644 "$ROOT/lnflash/payload/README-bundle.md" "$STAGE/README.md"
install -m 0644 "$ROOT/LICENSE" "$STAGE/LICENSE"
# Codeberg #288. Two things in this bundle are statically linked Rust — the
# lnflash binary and the t114 UF2 — and both carry MIT- and BSD-licensed
# crates whose licences require the notice to travel with the binary.
# THIRD-PARTY-NOTICES covers both: it is generated from the two lockfiles,
# host binaries in part 1 and the firmware image in part 2. The SoftDevice
# beside it keeps its own licence file; that blob is never linked in, so its
# terms are a separate matter (see the manifest's remedy section).
install -m 0644 "$ROOT/THIRD-PARTY-NOTICES" "$STAGE/THIRD-PARTY-NOTICES"
rm -f "$OUT_DIR/t114.bin.tmp" "$OUT_DIR/t114.uf2.tmp"

sha() { sha256sum "$STAGE/firmware/$1" | cut -d' ' -f1; }

cat > "$STAGE/firmware/manifest.toml" <<EOF
# What this bundle carries: the release it is, and one image per board.
#
# The board facts — USB IDs, flash geometry, Board-ID, SoftDevice constraint —
# are NOT here. They are hardware properties, they do not change when a release
# is cut, and the sessions that only configure a running board need them with
# no bundle on disk at all, so they live in lnflash/catalogue.toml, compiled
# into the binary (Codeberg #342). Keeping a second copy here would be a second
# copy to drift.
#
# Generated by scripts/lnflash-bundle.sh. Do not edit by hand — the checksums
# are what stand between a truncated download and somebody's flash.

[bundle]
version = "$VERSION"
built   = "$BUILT"

[board.t114.app]
file    = "t114/leviculum-t114-$VERSION.uf2"
sha256  = "$(sha "t114/leviculum-t114-$VERSION.uf2")"
# Checked back off the [FW_BUILD] banner on the debug port after the flash.
git_sha = "$GIT_SHA"

[board.t114.remedy.softdevice]
file    = "t114/s140_nrf52_7.3.0_softdevice.hex"
sha256  = "$(sha t114/s140_nrf52_7.3.0_softdevice.hex)"
# Mandatory, and lnflash refuses to load a bundle whose licence file is
# missing. Nordic's clause 2 requires the notice to travel with the blob.
license = "t114/s140_nrf52_7.3.0_license-agreement.txt"
# The hex is distributed untouched and converted at run time, which avoids the
# question of whether repacking counts as the modification clause 5 forbids.
convert = "hex-to-uf2"
EOF

# --- Check it before shipping it ---------------------------------------------
say "checking the bundle"
"$STAGE/lnflash" --bundle "$STAGE" --check-bundle

tar -czf "$TARBALL" -C "$OUT_DIR" "lnflash-$VERSION"

# The licence files are asserted against the TARBALL rather than the stage
# directory, because the tarball is what ships and a `tar` that quietly leaves
# a file out is precisely the failure worth catching here. --check-bundle above
# cannot do it: it walks the manifest, and the manifest describes what lnflash
# writes to a board, not what the archive must carry for the distribution to be
# lawful (Codeberg #288).
listing="$(tar -tzf "$TARBALL")"
for want in \
    "lnflash-$VERSION/LICENSE" \
    "lnflash-$VERSION/THIRD-PARTY-NOTICES" \
    "lnflash-$VERSION/firmware/t114/s140_nrf52_7.3.0_license-agreement.txt"; do
    printf '%s\n' "$listing" | grep -qxF "$want" \
        || { echo "bundle is missing $want" >&2; exit 1; }
done
# Not just present: both halves of the notice file have to be in it. A
# truncated or half-generated file passes a name check and fails the obligation
# — the firmware section is the one that is easy to lose, because it comes from
# the second, separate workspace.
for marker in "PART 1 — host binaries" "PART 2 — LNode firmware image"; do
    grep -qF "$marker" "$STAGE/THIRD-PARTY-NOTICES" \
        || { echo "THIRD-PARTY-NOTICES is missing the '$marker' section" >&2; exit 1; }
done
say "licence files present in the tarball (LICENSE, THIRD-PARTY-NOTICES, SoftDevice agreement)"

say "wrote $TARBALL ($(du -h "$TARBALL" | cut -f1))"
say "contents:"
printf '%s\n' "$listing" | sed 's/^/  /'
