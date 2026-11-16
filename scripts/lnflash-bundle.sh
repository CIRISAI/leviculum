#!/usr/bin/env bash
# Build the lnflash tarball: a stranger unpacks it on any Linux, runs one
# binary, and their board ends up running our firmware.
#
#     tar xzf lnflash-<stamp>.tar.gz
#     cd lnflash-<stamp>
#     sudo ./lnflash
#
# Nothing installed, no repo, no network. This script is the build side of
# that promise, so everything it puts in the tarball comes from this checkout
# and nowhere else — a bundle built from a Meshtastic checkout is exactly the
# hidden dependency our clone-and-deploy policy forbids.
#
# Output: target/lnflash/lnflash-<stamp>.tar.gz, where <stamp> is the Debian
# version the leviculum .deb of the same build carries (see STAMP below), and
# lnflash-<stamp>+<feature>... when NRF_FEATURES is set.
#
#   OUT_DIR    where to write (default target/lnflash)
#   SKIP_FIRMWARE=1  reuse an already-built firmware ELF instead of building
#                    it. For iterating on the bundle itself; a release build
#                    must not use it.
#   NRF_FEATURES     comma-separated leviculum-nrf cargo features every board
#                    image is built with on top of its own bsp-* set, for a
#                    rig A/B arm such as `subslot-offset` (#438). Empty by
#                    default, and empty changes nothing. Set, it suffixes the
#                    bundle, its tarball and every UF2 with `+<feature>` and
#                    the manifest records the list, so a flashed bundle says
#                    which arm it is.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

NRF_DIR="$ROOT/leviculum-nrf"
# Both workspaces are asked where cargo writes rather than told: with
# CARGO_TARGET_DIR set (the nightly wrapper sets it) every artefact below lands
# outside the tree and every "$ROOT/target" path here would miss it. See
# scripts/cargo-target-dir.sh.
# shellcheck source-path=SCRIPTDIR/..
# shellcheck source=scripts/cargo-target-dir.sh
source "$ROOT/scripts/cargo-target-dir.sh"
# The firmware is built with the checkout's paths remapped, the same way in
# every script that builds it, at one fixed path (scripts/nrf-rustflags.sh,
# orders 1368 and 1460): every build below goes through nrf_cargo.
# shellcheck source=scripts/nrf-rustflags.sh
source "$ROOT/scripts/nrf-rustflags.sh"
HOST_TARGET="$(cargo_target_dir "$ROOT")"
NRF_TARGET_DIR="$(cargo_target_dir "$NRF_DIR")"
NRF_TARGET="$NRF_TARGET_DIR/thumbv7em-none-eabihf/release"
# OUT_DIR stays repo-relative: the tarball is this script's own product, not a
# cargo artefact, and the release steps that collect it look here.
OUT_DIR="${OUT_DIR:-$ROOT/target/lnflash}"

# The boards this bundle carries. One record per board, four fields:
#
#   <name>|<cargo bin>|<cargo features>|<vendored SoftDevice stem, or empty>
#
# `name` is the catalogue key in lnflash/catalogue.toml, and it is the same
# name the manifest sections and the staging directories use — that is what
# lets everything below be a loop instead of a branch. The fourth field names
# the pair under lnflash/payload/<name>/ that a SoftDevice remedy ships as,
# `<stem>_softdevice.hex` beside `<stem>_license-agreement.txt`; a board that
# carries no remedy leaves it empty and simply gets no remedy section.
#
# A third board is one more line here. Nothing below this list mentions a
# board by name: the builds, the UF2 conversion, the staging, the manifest and
# the licence assertions against the finished tarball all walk it.
#
# Not every board lnflash knows may be listed. A board whose catalogue entry
# has no `flashing` section is control-only — its Board-ID names a module
# rather than a product, so no manifest may key a write on it — and lnflash
# refuses at load time to read a bundle that carries an image for one. The
# SenseCAP Solar Node is the first (Codeberg #233); it is flashed by
# `just flash-solarnode`, by a person who can see which board is on the bench.
#
# The SenseCAP T1000-E carries no SoftDevice remedy: its factory bootloader
# already runs S140 7.3.0, and its catalogue level is `expected` until the rig
# has run it. Its first image goes on by serial DFU (`just
# t1000e-dfu-package`), because the unit we have publishes no UF2 volume.
#
# Which image a board gets is settled in docs/src/concepts/board-support-scope.md:
# one build serves a pinout family, so the RAK4631 ships the baseboard build
# that also runs on a bare module rather than a second, stripped one.
BOARDS=(
    "t114|t114|bsp-t114|s140_nrf52_7.3.0"
    "rak4631|rak4631|bsp-rak4631,rak-baseboard|"
    "t1000e|t1000e|bsp-t1000e|"
)

# Fixed properties of the Adafruit nRF52 UF2 family, the same two constants
# leviculum-nrf/tools/uf2-runner.sh writes into every flashed image and the
# same two lnflash/catalogue.toml records per board. They are not per-board
# fields here because every board in the list above is an nRF52840 running
# that bootloader; a board outside the family needs a new transport in
# lnflash, not a new column (docs/src/concepts/lnode-flashing.md, "Four axes").
FLASH_BASE=0x27000
FAMILY_ID=0xADA52840

VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
GIT_SHA="$(git rev-parse --short HEAD)"
BUILT="$(git log -1 --format=%cs)"

# Which build this bundle is. The bare Cargo version alone made every nightly
# of a development window the same "0.10.0-dev" on disk, so two bundles a week
# apart could not be told apart and nobody could say which firmware they had
# flashed (PUB-0022). The stamp is the leviculum .deb's own version,
# <version>~nightly.<UTCdate>.<sha7>, and it is READ from the file
# scripts/deb-stamp.sh wrote rather than derived again: the nightly runs that
# script in an earlier step, so a derivation here could cross midnight UTC and
# disagree with the .deb, and collect-nightly-debs.sh finds the tarball by the
# same file. It names the tarball, the directory inside it, every UF2, the
# manifest's [bundle] version and the README.
#
# A stamp file from another commit is stale (a developer tree keeps it between
# runs) and a tree that never ran deb-stamp has none; both get the same shape
# derived here, by deb-stamp's own rules: the commit from CI_COMMIT_SHA or git,
# the date in UTC, and the pre-release `-` spelled `~`.
STAMP_SHA="${CI_COMMIT_SHA:-$(git rev-parse HEAD)}"
STAMP_SHA7="$(printf '%.7s' "$STAMP_SHA")"
STAMP=""
if [ -r .deb-version-leviculum-cli ]; then
    IFS= read -r STAMP <.deb-version-leviculum-cli || true
    case "$STAMP" in
    *"~nightly."*".$STAMP_SHA7") ;;
    *)
        printf '[lnflash-bundle] %s\n' \
            "ignoring .deb-version-leviculum-cli ('$STAMP'): it is not from commit $STAMP_SHA7" >&2
        STAMP=""
        ;;
    esac
fi
if [ -z "$STAMP" ]; then
    STAMP="$(printf '%s' "$VERSION" | sed 's/-/~/')~nightly.$(date -u +%Y%m%d).$STAMP_SHA7"
fi

# Which compiler produced everything in this bundle. A UF2 in somebody's hands
# could otherwise only be traced back to a compiler by inferring one from a git
# tag, and the firmware is exactly where that matters: a measured 3264-byte
# stack-frame margin, and codegen differences between compiler versions move
# frame sizes (Codeberg #305).
#
# One value for both halves, and that is checked rather than assumed. The host
# binary is built here and the firmware is built in leviculum-nrf, which has no
# rust-toolchain.toml of its own — rustup walks up to the root pin — so the two
# agree today. A per-directory override or a toolchain file added there would
# silently make this key describe only one of the two images, so ask both.
RUSTC_VERSION="$(rustc --version)"
NRF_RUSTC_VERSION="$(cd "$NRF_DIR" && rustc --version)"
if [ "$RUSTC_VERSION" != "$NRF_RUSTC_VERSION" ]; then
    echo "the firmware and the flasher would be built by different compilers:" >&2
    echo "  $ROOT:    $RUSTC_VERSION" >&2
    echo "  $NRF_DIR: $NRF_RUSTC_VERSION" >&2
    echo "the manifest records one compiler for the whole bundle, so record" >&2
    echo "both here before shipping a bundle built by two" >&2
    exit 1
fi

# The extra firmware features. Checked for shape rather than trusted: the list
# goes into file names and into a TOML string, and a bsp-* feature here would
# give a board a second pinout on top of its own.
NRF_FEATURES="${NRF_FEATURES:-}"
case "$NRF_FEATURES" in
*[!a-z0-9,-]* | ,* | *, | *,,*)
    echo "NRF_FEATURES='$NRF_FEATURES' is not a comma-separated list of cargo features" >&2
    exit 1
    ;;
esac
case ",$NRF_FEATURES," in
*,bsp-*)
    echo "NRF_FEATURES='$NRF_FEATURES' names a bsp-* feature; every board brings its own" >&2
    exit 1
    ;;
esac
# `+a+b` for features a,b; nothing at all without them, so the default bundle
# keeps the name the nightly collector and publish-nightly.sh look for.
FEATURE_SUFFIX=""
[ -z "$NRF_FEATURES" ] || FEATURE_SUFFIX="+${NRF_FEATURES//,/+}"
BUNDLE_NAME="$STAMP$FEATURE_SUFFIX"

STAGE="$OUT_DIR/lnflash-$BUNDLE_NAME"
TARBALL="$OUT_DIR/lnflash-$BUNDLE_NAME.tar.gz"

say() { printf '[lnflash-bundle] %s\n' "$*"; }

if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
    say "WARNING: tracked files are modified. The firmware will report"
    say "         dirty=true and the manifest records git_sha=$GIT_SHA anyway."
fi

# --- The application images --------------------------------------------------
# Built here rather than vendored, so a bundle always carries the firmware the
# commit it was built from produces.
mkdir -p "$OUT_DIR"

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
BIN2UF2="$NRF_TARGET_DIR/bin2uf2"
if [ ! -x "$BIN2UF2" ] || [ "$NRF_DIR/tools/bin2uf2.rs" -nt "$BIN2UF2" ]; then
    say "building bin2uf2"
    # rustc, unlike cargo, does not create the directory it writes into, and an
    # external target directory is empty before the first build below runs.
    mkdir -p "$NRF_TARGET_DIR"
    rustc -O -o "$BIN2UF2" "$NRF_DIR/tools/bin2uf2.rs"
fi

for record in "${BOARDS[@]}"; do
    IFS='|' read -r name bin features _stem <<<"$record"
    [ -z "$NRF_FEATURES" ] || features="$features,$NRF_FEATURES"

    if [ "${SKIP_FIRMWARE:-0}" != 1 ]; then
        say "building the $name firmware ($features)"
        (cd "$NRF_DIR" && nrf_cargo build --release --bin "$bin" --features "$features")
    fi

    ELF="$NRF_TARGET/$bin"
    [ -f "$ELF" ] || { echo "no firmware ELF at $ELF" >&2; exit 1; }

    # ELF -> flat binary. -R .bss -R .uninit excludes NOBITS sections, without
    # which a pre-2020 llvm-objcopy emits a ~500 MB binary at the wrong
    # addresses.
    say "converting the $name firmware to UF2"
    "$OBJCOPY" -O binary -R .bss -R .uninit "$ELF" "$OUT_DIR/$name.bin.tmp"
    "$BIN2UF2" --base "$FLASH_BASE" --family "$FAMILY_ID" \
        "$OUT_DIR/$name.bin.tmp" "$OUT_DIR/$name.uf2.tmp"
done

# --- The binary --------------------------------------------------------------
# musl-static by workspace default, so it runs on any Linux with no libc of
# its own to find.
say "building lnflash"
cargo build -p lnflash --release
LNFLASH="$HOST_TARGET/x86_64-unknown-linux-musl/release/lnflash"
[ -f "$LNFLASH" ] || { echo "no lnflash binary at $LNFLASH" >&2; exit 1; }

# --- Assemble ----------------------------------------------------------------
rm -rf "$STAGE"
mkdir -p "$STAGE/firmware"
install -m 0755 "$LNFLASH" "$STAGE/lnflash"
install -m 0644 "$ROOT/lnflash/payload/README-bundle.md" "$STAGE/README.md"
# The README is the one file a person opens, so it names the build too.
cat >>"$STAGE/README.md" <<EOF

## Which build this is

    $STAMP

Built from commit $STAMP_SHA7 (committed $BUILT). The leviculum .deb of the
same nightly carries the same version, \`lnflash\` names it in its first line,
and \`firmware/manifest.toml\` records it as the bundle version.
EOF
if [ -n "$NRF_FEATURES" ]; then
    cat >>"$STAGE/README.md" <<EOF

This is not the default firmware: every image was built with the extra
leviculum-nrf features \`$NRF_FEATURES\`, which the manifest records and
\`lnflash\` names in its first line.
EOF
fi
install -m 0644 "$ROOT/LICENSE" "$STAGE/LICENSE"
# Codeberg #288. The statically linked Rust in this bundle — the lnflash binary
# and every firmware UF2 — carries MIT- and BSD-licensed crates whose licences
# require the notice to travel with the binary. THIRD-PARTY-NOTICES covers all
# of it: it is generated from the two lockfiles, host binaries in part 1 and
# the firmware in part 2, and both firmware binaries are built from the same
# leviculum-nrf lockfile that part 2 describes. The SoftDevice beside it keeps
# its own licence file; that blob is never linked in, so its terms are a
# separate matter (see the manifest's remedy section).
install -m 0644 "$ROOT/THIRD-PARTY-NOTICES" "$STAGE/THIRD-PARTY-NOTICES"

for record in "${BOARDS[@]}"; do
    IFS='|' read -r name _bin _features stem <<<"$record"
    mkdir -p "$STAGE/firmware/$name"
    install -m 0644 "$OUT_DIR/$name.uf2.tmp" \
        "$STAGE/firmware/$name/leviculum-$name-$BUNDLE_NAME.uf2"
    rm -f "$OUT_DIR/$name.bin.tmp" "$OUT_DIR/$name.uf2.tmp"
    [ -n "$stem" ] || continue
    # The SoftDevice travels as a file with its licence beside it — never
    # linked into the binary, because Nordic's clauses 4 and 5 cannot become
    # part of one combined work with AGPL code.
    install -m 0644 "$ROOT/lnflash/payload/$name/${stem}_softdevice.hex" \
        "$STAGE/firmware/$name/"
    install -m 0644 "$ROOT/lnflash/payload/$name/${stem}_license-agreement.txt" \
        "$STAGE/firmware/$name/"
done

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
version = "$STAMP"
built   = "$BUILT"
# The compiler that produced every image below and the flasher beside them.
rustc   = "$RUSTC_VERSION"
EOF
# Only when set: a default bundle's manifest stays byte for byte what it was.
if [ -n "$NRF_FEATURES" ]; then
    IFS=, read -ra extra <<<"$NRF_FEATURES"
    cat >> "$STAGE/firmware/manifest.toml" <<EOF
# Extra leviculum-nrf features every image below was built with, on top of
# its board's own bsp-* set. lnflash names them when it flashes.
features = [$(printf '"%s", ' "${extra[@]}" | sed 's/, $//')]
EOF
fi

for record in "${BOARDS[@]}"; do
    IFS='|' read -r name _bin _features stem <<<"$record"
    image="$name/leviculum-$name-$BUNDLE_NAME.uf2"
    cat >> "$STAGE/firmware/manifest.toml" <<EOF

[board.$name.app]
file    = "$image"
sha256  = "$(sha "$image")"
# Checked back off the [FW_BUILD] banner on the debug port after the flash.
git_sha = "$GIT_SHA"
EOF
    [ -n "$stem" ] || continue
    cat >> "$STAGE/firmware/manifest.toml" <<EOF

[board.$name.remedy.softdevice]
file    = "$name/${stem}_softdevice.hex"
sha256  = "$(sha "$name/${stem}_softdevice.hex")"
# Mandatory, and lnflash refuses to load a bundle whose licence file is
# missing. Nordic's clause 2 requires the notice to travel with the blob.
license = "$name/${stem}_license-agreement.txt"
# The hex is distributed untouched and converted at run time, which avoids the
# question of whether repacking counts as the modification clause 5 forbids.
convert = "hex-to-uf2"
EOF
done

# --- Check it before shipping it ---------------------------------------------
say "checking the bundle"
"$STAGE/lnflash" --bundle "$STAGE" --check-bundle

tar -czf "$TARBALL" -C "$OUT_DIR" "lnflash-$BUNDLE_NAME"

# The licence files are asserted against the TARBALL rather than the stage
# directory, because the tarball is what ships and a `tar` that quietly leaves
# a file out is precisely the failure worth catching here. --check-bundle above
# cannot do it: it walks the manifest, and the manifest describes what lnflash
# writes to a board, not what the archive must carry for the distribution to be
# lawful (Codeberg #288).
listing="$(tar -tzf "$TARBALL")"
wanted=(
    "lnflash-$BUNDLE_NAME/LICENSE"
    "lnflash-$BUNDLE_NAME/THIRD-PARTY-NOTICES"
)
# Every board's image, and every vendored blob's licence beside it. Derived
# from the board list rather than written out, so a board added above cannot
# ship with its image silently missing from the archive.
for record in "${BOARDS[@]}"; do
    IFS='|' read -r name _bin _features stem <<<"$record"
    wanted+=("lnflash-$BUNDLE_NAME/firmware/$name/leviculum-$name-$BUNDLE_NAME.uf2")
    [ -n "$stem" ] || continue
    wanted+=("lnflash-$BUNDLE_NAME/firmware/$name/${stem}_license-agreement.txt")
done
# A here-string and never `printf | grep -q`: bash's printf writes a line
# buffer at a time, so under `pipefail` that pipeline is red whenever grep
# matches and exits before printf has written the rest, printf dying of SIGPIPE
# (141) with the name right there in the listing (#843). It reported a present
# THIRD-PARTY-NOTICES missing once on a loaded host (1314: the pipe form 14 red
# of 20000 checks under `stress-ng --cpu 40`). The listing goes with the
# message, so a red names what it compared against.
for want in "${wanted[@]}"; do
    grep -qxF -- "$want" <<<"$listing" || {
        echo "bundle is missing $want; the tarball lists:" >&2
        printf '%s\n' "$listing" | sed 's/^/  /' >&2
        exit 1
    }
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
