#!/usr/bin/env bash
# The Android half of loquax-android's toolchain: build-tools 35.0.1,
# platform android-35 and the NDK r27d, from Google's repository, each
# checked against the SHA-1 Google's repository index (repository2-3.xml)
# gives for it, and unpacked into the layout Debian's
# google-android-*-installer packages produce under /usr/lib/android-sdk.
#
# trixie's contrib has no such packages (checked 2026-10-29). A host that
# has them already (android-make-template's apt line) lacks only the NDK,
# and the script skips what is in place.
#
# The rest comes from Debian and rustup:
#   sudo apt install default-jdk-headless unzip
#   rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
#
# Usage: loquax-android/install-toolchain.sh [SDK]   (default /usr/lib/android-sdk)
# Writes SDK with sudo when it is not writable. About 2.3 GB, of which the
# NDK is 2.0 GB.
set -euo pipefail

SDK=${1:-/usr/lib/android-sdk}
BASE=https://dl.google.com/android/repository

# archive, sha1, directory in the archive, place under SDK
PIECES=(
    "build-tools_r35.0.1_linux.zip e009a9b188cfeb1d2b4c318ab5cb4f1ddc368861 android-15 build-tools/35.0.1"
    "platform-35_r02.zip 0bb560a90a7a2cbd0dd8348224d518b638fe7949 android-35 platforms/android-35"
    "android-ndk-r27d-linux.zip 22105e410cf29afcf163760cc95522b9fb981121 android-ndk-r27d ndk/27.3.13750724"
)

SUDO=
mkdir -p "$SDK" 2>/dev/null || true
[[ -w "$SDK" ]] || SUDO=sudo
$SUDO mkdir -p "$SDK"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

for piece in "${PIECES[@]}"; do
    read -r archive sha1 inner place <<<"$piece"
    if [[ -d "$SDK/$place" ]]; then
        echo "have $SDK/$place"
        continue
    fi
    echo "fetching $archive"
    curl -sSfo "$WORK/$archive" "$BASE/$archive"
    echo "$sha1  $WORK/$archive" | sha1sum -c --quiet
    unzip -q "$WORK/$archive" -d "$WORK/x"
    $SUDO mkdir -p "$(dirname "$SDK/$place")"
    $SUDO mv "$WORK/x/$inner" "$SDK/$place"
    rm -rf "$WORK/x" "$WORK/$archive"
    echo "installed $SDK/$place"
done
