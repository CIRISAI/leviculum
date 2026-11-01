#!/usr/bin/env bash
#
# Builds Mark's RNode firmware from reference/RNode_Firmware with our patches
# from scripts/patches/rnode-firmware-*.patch applied, for the one board we
# have a reason to patch: the Heltec V4. Everywhere else the rig runs the
# signed release images that `just flash-rnode` writes back.
#
# WHY WE BUILD AT ALL (1260, 2026-11-01)
#
#   The V4 carries an external LNA in front of its SX1262 (GC1109, 17 dB; or
#   KCT8103L, 21 dB, from v4.3 on). Upstream subtracts that gain in
#   sx126x::packetRssi() and currentRssi(), but not in the overload
#   packetRssi(snr_raw), and the ESP32 receive path calls exactly that
#   overload (RNode_Firmware.ino:470, then :1662 into CMD_STAT_RSSI). So the
#   V4 reports every packet 21 dB hot on our rig, and `rig levels` marked it
#   "not trusted" in every round. Upstream HEAD still has it. We do not
#   report upstream; we carry the one-line fix here and document it in
#   docs/src/rnode-protocol.md.
#
# WHAT IS BUILT
#
#   The submodule is copied, never patched in place (the pin stays the pin):
#   target/rnode-firmware/RNode_Firmware/ gets the sources, every patch is
#   applied with `patch -p1`, and arduino-cli compiles it with the exact
#   command line of the upstream Makefile's firmware-heltec32_v4 target
#   (fqbn esp32:esp32:esp32s3:CDCOnBoot=cdc, no_ota partitions, BOARD_MODEL
#   0x3F) against the core version that Makefile pins (2.0.17). Output:
#   target/rnode-firmware/heltec-v4/RNode_Firmware.ino.bin, the app image
#   that `just flash-rnode-built` writes to 0x10000.
#
#   The Makefile's check_bt_buffers step is not run: it checks the classic
#   Bluetooth queue sizes in the core's BluetoothSerial, and the S3 has no
#   classic Bluetooth (its firmware-heltec32_v4 target skips it too).
#
# PINS
#
#   arduino-cli 1.5.1 (sha256 checked), esp32 core 2.0.17, and the six
#   libraries the Makefile's prep-esp32 names plus their dependencies, at
#   the versions the first build used (2026-11-01). The upstream Makefile
#   installs the libraries unpinned; we pin because this writes to a
#   board, the same reason scripts/install-esptool.sh gives.
#
# DIAG BUILDS (1271)
#
#   `build heltec-v4 --diag` also applies scripts/patches/diag-rnode-firmware-
#   *.patch on top of the product patches and writes to heltec-v4-diag/, so
#   the product image is never overwritten by an instrumented one. A diag
#   patch is a measuring tool for one investigation, never flashed by
#   default: `just flash-rnode-built` only ever takes heltec-v4/.
#
# Usage:
#   scripts/build-rnode-firmware.sh setup        # install the toolchain
#   scripts/build-rnode-firmware.sh build heltec-v4 [--diag]
#
# The toolchain lives under LEVICULUM_RNODE_TOOLS (default ~/.rnode-tools,
# beside the esptool venv); the core and libraries in arduino-cli's own data
# directory under it, so nothing touches ~/.arduino15.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOOLS="${LEVICULUM_RNODE_TOOLS_DIR:-$HOME/.rnode-tools}"
SRC="$REPO_DIR/reference/RNode_Firmware"
OUT="$REPO_DIR/target/rnode-firmware"

ARDUINO_CLI_VERSION=1.5.1
ARDUINO_CLI_SHA256=28a8e119c498a25607821c36cb2dc49e8463941b261a0d99091baa7bc692dd2b
ARDUINO_CLI="$TOOLS/arduino-cli/arduino-cli"
ESP32_CORE=2.0.17
LIBS=(
    "Adafruit SSD1306@2.5.16"
    "Adafruit SH110X@2.1.14"
    "Adafruit ST7735 and ST7789 Library@1.11.0"
    "Adafruit NeoPixel@1.15.4"
    "XPowersLib@0.3.3"
    "Crypto@0.4.0"
    # What those pull in, pinned too and installed with --no-deps, so
    # a dependency release cannot change the image either.
    "Adafruit GFX Library@1.13.0"
    "Adafruit BusIO@1.17.4"
    "Adafruit seesaw Library@1.7.9"
    "SD@1.3.0"
)

die() { echo "build-rnode-firmware: $*" >&2; exit 1; }
say() { echo "[build-rnode-firmware] $*"; }

# One config for every call: the board index URL comes from the submodule's
# own arduino-cli.yaml, the data and download directories are ours.
CONFIG="$TOOLS/arduino-cli/arduino-cli.yaml"
ac() { "$ARDUINO_CLI" --config-file "$CONFIG" "$@"; }

cmd_setup() {
    mkdir -p "$TOOLS/arduino-cli"
    if [[ ! -x "$ARDUINO_CLI" ]] \
        || [[ "$("$ARDUINO_CLI" version 2>/dev/null)" != *"Version: $ARDUINO_CLI_VERSION "* ]]; then
        local tgz="$TOOLS/arduino-cli/arduino-cli_${ARDUINO_CLI_VERSION}_Linux_64bit.tar.gz"
        say "fetching arduino-cli $ARDUINO_CLI_VERSION"
        curl -sSfL -o "$tgz" \
            "https://github.com/arduino/arduino-cli/releases/download/v${ARDUINO_CLI_VERSION}/arduino-cli_${ARDUINO_CLI_VERSION}_Linux_64bit.tar.gz"
        echo "$ARDUINO_CLI_SHA256  $tgz" | sha256sum -c --quiet - \
            || die "arduino-cli tarball does not match its pinned sha256"
        tar -xzf "$tgz" -C "$TOOLS/arduino-cli" arduino-cli
        rm -f "$tgz"
    fi
    cat > "$CONFIG" <<EOF
board_manager:
  additional_urls:
    - https://raw.githubusercontent.com/espressif/arduino-esp32/gh-pages/package_esp32_index.json
directories:
  data: $TOOLS/arduino-cli/data
  downloads: $TOOLS/arduino-cli/staging
  user: $TOOLS/arduino-cli/user
EOF
    ac core update-index
    ac core install "esp32:esp32@$ESP32_CORE"
    local lib
    for lib in "${LIBS[@]}"; do
        ac lib install --no-deps "$lib"
    done
    say "arduino-cli $ARDUINO_CLI_VERSION, esp32 core $ESP32_CORE ready under $TOOLS/arduino-cli"
}

# board -> fqbn and BOARD_MODEL, copied from the upstream Makefile's
# firmware-<board> target. Only boards we patch for get a row.
board_row() {
    case "$1" in
        heltec-v4) echo "esp32:esp32:esp32s3:CDCOnBoot=cdc 0x3F" ;;
        *) die "no build row for board '$1' (heltec-v4)" ;;
    esac
}

cmd_build() {
    local board="${1:-}" diag="${2:-}" row fqbn model suffix=""
    [[ -n "$board" ]] || die "build needs a board (heltec-v4)"
    case "$diag" in
        "") ;;
        --diag) suffix="-diag" ;;
        *) die "unknown build option '$diag' (--diag)" ;;
    esac
    # Resolved into a variable first: a die() inside a here-string's command
    # substitution would leave the caller compiling with an empty fqbn.
    row="$(board_row "$board")" || exit 1
    read -r fqbn model <<< "$row"
    [[ -x "$ARDUINO_CLI" && -f "$CONFIG" ]] || die "no toolchain (run: just rnode-firmware-setup)"
    [[ -f "$SRC/RNode_Firmware.ino" ]] || die "no sources at $SRC (git submodule update --init reference/RNode_Firmware)"

    local sketch="$OUT/RNode_Firmware" build="$OUT/build-$board$suffix" dest="$OUT/$board$suffix"
    rm -rf "$sketch" "$build" "$dest"
    mkdir -p "$sketch" "$dest"
    # The tracked sources only; .git and any local build products stay behind.
    (cd "$SRC" && git ls-files -z | tar --null -T - -cf -) | tar -xf - -C "$sketch"

    local p
    for p in "$REPO_DIR"/scripts/patches/rnode-firmware-*.patch; do
        [[ -e "$p" ]] || continue
        say "applying $(basename "$p")"
        patch -p1 --forward --batch -d "$sketch" < "$p"
    done
    if [[ -n "$suffix" ]]; then
        for p in "$REPO_DIR"/scripts/patches/diag-rnode-firmware-*.patch; do
            [[ -e "$p" ]] || continue
            say "applying diag $(basename "$p")"
            patch -p1 --forward --batch -d "$sketch" < "$p"
        done
    fi

    ac compile --fqbn "$fqbn" \
        --build-path "$build" --output-dir "$dest" \
        --build-property "build.partitions=no_ota" \
        --build-property "upload.maximum_size=2097152" \
        --build-property "compiler.cpp.extra_flags=\"-DBOARD_MODEL=$model\"" \
        "$sketch"

    [[ -f "$dest/RNode_Firmware.ino.bin" ]] || die "compile produced no RNode_Firmware.ino.bin"
    # The hash the board will validate against; scripts/rnode-flash.sh
    # write-app stores it in the EEPROM. Refuses an image without one.
    bash "$REPO_DIR/scripts/rnode-flash.sh" hash --image "$dest/RNode_Firmware.ino.bin" \
        > "$dest/firmware-hash"
    say "built $dest/RNode_Firmware.ino.bin hash $(cat "$dest/firmware-hash")"
}

[[ $# -ge 1 ]] || die "usage: $0 <setup|build BOARD>"
case "$1" in
    setup) cmd_setup ;;
    build) shift; cmd_build "$@" ;;
    *) die "unknown action '$1'" ;;
esac
