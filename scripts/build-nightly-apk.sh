#!/usr/bin/env bash
# The Android app for the nightly (1267, Refs #232): the debug APK for
# phones and the x86_64 one for emulators, built by loquax-android's
# Makefile and staged into dist/ beside the .debs with a .sha256 each.
#
# Why: until 1267 nothing published an APK. `make waydroid` existed for the
# device walk on hamster (loquax-android/scripts/device-walk.py), and the
# download page offered phones nothing, so a first user could not install
# the app (the ninety percent walk, finding 2).
#
# Run by scripts/nightly-publish.sh AFTER scripts/collect-nightly-debs.sh,
# which empties dist/ first. The site publish (scripts/publish-site.sh) sends
# whatever dist/ holds, so the APKs go to leviculum.network with everything
# else. The forge's rolling release does not carry them: its build steps run
# in rust:bookworm without the Android SDK and NDK (2.3 GB,
# loquax-android/install-toolchain.sh), and scripts/publish-nightly.sh uploads
# only .deb, .tar.gz and .sha256.
#
# Staged, one `stage_apk <make target> <built name> <published name>` line
# each (scripts/site-check.py reads the published names off those lines):
#
#   loquax-nightly-android.apk         arm64-v8a and armeabi-v7a, for phones
#   loquax-nightly-android-x86_64.apk  x86_64, for Waydroid and the emulator
#
# Debug builds, package network.leviculum.loquax.debug: what the device walk
# installs, and installable beside a later signed release. The signed release
# build and F-Droid come later.
#
# VERSIONS. versionName and versionCode are the ones scripts/bump-touched.py
# moves in loquax-android/Cargo.toml at every landing that touches the crate
# (1226); the Makefile reads both from there (`make print-version`, checked by
# `bump-touched.py --selftest`). This script reads them back out of each
# built APK with aapt2 (aapt's badging cannot resolve the manifest's
# framework icon and fails) and refuses an APK that carries anything else.
#
# THE SIGNING KEY outlives the tree. Android installs an update only when it
# is signed with the key of the installed app, and the nightly builds in a
# fresh clone every night, where the Makefile's tree-local debug.keystore
# would be a new key each time and every update would be refused. So the key
# lives at LEVICULUM_APK_KEYSTORE (default
# ~/.config/leviculum-android/nightly-debug.keystore), with the debug
# convention's password `android`; the file's mode 600 is what protects it.
# A missing key is created, and said so loudly: from then on phones that
# installed an APK signed with the old key have to uninstall before the
# next one installs.
#
# Usage:
#   bash scripts/build-nightly-apk.sh [--dist DIR]   build and stage (default dist/)
#   bash scripts/build-nightly-apk.sh --check-tools  name every missing tool, exit 1 if any
#   bash scripts/build-nightly-apk.sh --print-names  the published names, one a line
#
# Environment, all optional: LEVICULUM_APK_KEYSTORE as above; SDK (default
# /usr/lib/android-sdk) and CARGO_TARGET_DIR are the Makefile's own.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/loquax-android"
BIN="$APP/android/bin"

say() { printf '[nightly-apk] %s\n' "$*"; }
die() { printf '[nightly-apk] %s\n' "$*" >&2; exit 1; }

MODE=build
DIST="$ROOT/dist"
while [ $# -gt 0 ]; do
    case "$1" in
    --check-tools | --print-names) MODE="${1#--}"; shift ;;
    --dist)
        [ $# -ge 2 ] || die "--dist needs a directory"
        DIST="$2"
        shift 2
        ;;
    *) die "unknown argument '$1'" ;;
    esac
done

KEYSTORE="${LEVICULUM_APK_KEYSTORE:-${HOME:-}/.config/leviculum-android/nightly-debug.keystore}"
# Absolute, because make runs in loquax-android/.
KEYSTORE="$(realpath -m -- "$KEYSTORE")"
DIST="$(realpath -m -- "$DIST")"
# The Makefile's defaults, so a check here looks where the build will.
SDK="${SDK:-/usr/lib/android-sdk}"
BUILDTOOLS="$SDK/build-tools/35.0.1"

# What one APK of the nightly is. In build mode: build it, check its
# versions, stage it with its checksum. The other modes only collect the
# published name.
NAMES=()
stage_apk() {  # <make target> <name the Makefile builds> <published name>
    local target="$1" built="$BIN/$2" stable="$3"
    NAMES+=("$stable")
    [ "$MODE" = build ] || return 0

    rm -f "$built"
    say "make ${target} (signing with ${KEYSTORE})"
    make -C "$APP" "$target" KEYSTORE="$KEYSTORE"
    [ -f "$built" ] || die "make ${target} left no ${built}"

    local badging code name
    badging="$("$BUILDTOOLS/aapt2" dump badging "$built")"
    code="$(sed -n "s/^package: .* versionCode='\([0-9]*\)'.*/\1/p" <<<"$badging")"
    name="$(sed -n "s/^package: .* versionName='\([^']*\)'.*/\1/p" <<<"$badging")"
    if [ "$code" != "$WANT_CODE" ] || [ "$name" != "$WANT_NAME" ]; then
        die "${2} carries versionCode '${code}' versionName '${name}', loquax-android/Cargo.toml says '${WANT_CODE}' '${WANT_NAME}'"
    fi

    cp "$built" "$DIST/$stable"
    (cd "$DIST" && sha256sum "$stable" >"$stable.sha256")
    say "staged ${stable}: versionName ${name}, versionCode ${code}, $(wc -c <"$DIST/$stable") bytes"
}

apks() {
    stage_apk debug loquax-debug.apk loquax-nightly-android.apk
    stage_apk waydroid loquax-debug-x86_64.apk loquax-nightly-android-x86_64.apk
}

if [ "$MODE" = print-names ]; then
    apks
    printf '%s\n' "${NAMES[@]}"
    exit 0
fi

# --- Tools ------------------------------------------------------------------
missing=0
need() {  # <what> <present: 0|1>
    if [ "$2" != 1 ]; then
        say "missing: $1 (loquax-android/README.md says how to install it)"
        missing=1
    fi
}
has() { command -v "$1" >/dev/null 2>&1 && echo 1 || echo 0; }
for tool in make javac keytool cargo rustup sha256sum; do
    need "$tool" "$(has "$tool")"
done
for piece in build-tools/35.0.1/aapt build-tools/35.0.1/aapt2 build-tools/35.0.1/d8 build-tools/35.0.1/apksigner \
    build-tools/35.0.1/zipalign platforms/android-35/android.jar \
    ndk/27.3.13750724/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-ar; do
    need "${SDK}/${piece} (loquax-android/install-toolchain.sh)" "$([ -e "$SDK/$piece" ] && echo 1 || echo 0)"
done
if [ "$(has rustup)" = 1 ]; then
    installed="$(rustup target list --installed 2>/dev/null || true)"
    for triple in aarch64-linux-android armv7-linux-androideabi x86_64-linux-android; do
        need "rustup target ${triple}" "$(grep -qx "$triple" <<<"$installed" && echo 1 || echo 0)"
    done
fi
if [ "$MODE" = check-tools ]; then
    exit "$missing"
fi
[ "$missing" = 0 ] || die "refusing: tools missing (above); no APK was built"

# --- Build ------------------------------------------------------------------
[ -d "$DIST" ] || die "no ${DIST}: run scripts/collect-nightly-debs.sh first"

read -r WANT_CODE WANT_NAME < <(make -s --no-print-directory -C "$APP" print-version |
    sed -n 's/^VERSION_CODE=\([0-9]*\) VERSION_NAME=\(.*\)$/\1 \2/p')
if [ -z "${WANT_CODE:-}" ] || [ -z "${WANT_NAME:-}" ]; then
    die "make print-version in loquax-android named no versionCode and versionName"
fi

if [ ! -f "$KEYSTORE" ]; then
    say "=================================================================="
    say "NO NIGHTLY SIGNING KEY at ${KEYSTORE}: creating a new one."
    say "A phone holding an APK signed with an older key refuses this one"
    say "as an update and has to uninstall first. Keep this file: it is"
    say "what makes tomorrow's nightly an update of today's."
    say "=================================================================="
    (
        umask 077
        mkdir -p "$(dirname "$KEYSTORE")"
        keytool -genkeypair -keystore "$KEYSTORE" -storepass android -keypass android \
            -alias androiddebugkey -dname "CN=Leviculum nightly,O=Leviculum,C=DE" \
            -keyalg RSA -keysize 2048 -validity 10000
    )
fi

apks
