#!/usr/bin/env bash
#
# Composes the esptool command lines that read Mark's signed RNode firmware
# off an ESP32 board and write it back. The Justfile's flash-rnode-* recipes
# are wrappers around this; it exists as a script because the chip and the
# flash offsets have to be DERIVED, and a just recipe cannot derive anything.
#
# WHAT WENT WRONG (2026-09-17)
#
#   Every recipe passed `--chip esp32` and read the bootloader at 0x1000,
#   both constants, and both wrong for half the rig. The Heltec V4 is an
#   ESP32-S3, where the second-stage bootloader starts at 0x0, and esptool
#   refuses the wrong --chip outright:
#
#     A fatal error occurred: This chip is ESP32-S3, not ESP32.
#
#   So the V4 could never be extracted or restored, and nobody found out
#   until a V4 needed restoring. The T-Beams are plain ESP32 and worked,
#   which is exactly why the constants looked like facts.
#
# HOW THE CHIP IS ESTABLISHED
#
#   From the device when we are allowed to talk to it (`--board auto`, the
#   default: esptool's own detection answers it), else from the board name
#   via the table below. A chip name may also be passed straight through.
#   The offsets then follow the chip family, never a global default.
#
# BOARD TABLE
#
#   heltec-v4   esp32s3   Heltec WiFi LoRa 32 V4. Read off the board:
#                         "Connected to ESP32-S3", QFN56 rev v0.2
#                         (.rnode-fw/extract.log, 2026-09-17).
#   tbeam       esp32     LilyGO T-Beam, ESP32 + SX1276. The board the
#                         extract/write pair has been run against since
#                         June, with --chip esp32, successfully.
#   auto        probed    ask the device (the default)
#
#   A Heltec V3 row is deliberately NOT here. Nobody has read that board's
#   chip on this rig, it is not in scripts/usbhub-helper's device list, and
#   the vendor sells "WiFi LoRa 32 V3" as an ESP32-S3 — so the one thing a
#   guessed row could do is put 0x1000 in front of an S3 again, which is
#   the defect this script exists to end. Run it with `auto`, or add the
#   row once `rnode-flash.sh chip --port ...` has answered.
#
# REGIONS
#
#   esp32:   the layout the T-Beam path has been flashing since June, left
#            byte for byte as it was — it is verified on hardware and a
#            "tidier" number here is a re-verification nobody asked for.
#   esp32s3: bootloader at 0x0 (the S3's ROM loads it from there; on the
#            ESP32 0x0..0x1000 is not flash-mapped for the loader), read up
#            to the partition table at 0x8000. The rest is read out of the
#            V4's own partition table in .rnode-fw/v4-full-16mb.bin
#            (2026-09-17): nvs 0x9000+0x5000, otadata 0xe000+0x2000,
#            app0 0x10000+0x200000, spiffs 0x210000+0x1e0000, coredump
#            0x3f0000+0x10000. Same RNode layout as the 4 MB parts, on a
#            16 MB chip with the tail unused.
#
#   A per-region set is a backup of the firmware. It is NOT a restore of a
#   board in trouble: it presumes the partition table it was cut with. That
#   is what read-image/write-image below are for — one image, the whole
#   flash, no assumptions.
#
# Usage:
#   rnode-flash.sh extract     --port P [--board B|--chip C] [--fw-dir D]
#   rnode-flash.sh write       --port P [--board B|--chip C] [--fw-dir D]
#   rnode-flash.sh write-app   --port P [--board B|--chip C] --image F
#   rnode-flash.sh hash        --image F                      # app image hash
#   rnode-flash.sh read-image  --port P [--board B|--chip C] --image F
#   rnode-flash.sh write-image --port P [--board B|--chip C] --image F
#   rnode-flash.sh chip        --port P                       # print the chip
#
# RNODE_DRY_RUN=1 prints the composed commands instead of running them and
# touches neither the port nor the filesystem; scripts/check-rnode-chip-
# offsets.sh gates the composition that way. A dry run cannot probe a
# device, so it needs --board or --chip.
#
# ESPTOOL points at the binary (default: the venv
# scripts/install-esptool.sh writes). esptool 5 spellings are composed here;
# see that script for why 4.x is refused rather than accommodated.

set -euo pipefail

DRY_RUN="${RNODE_DRY_RUN:-0}"
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RNODECONF=(env "PYTHONPATH=$REPO_DIR/reference/Reticulum" python3
    "$REPO_DIR/reference/Reticulum/RNS/Utilities/rnodeconf.py")
ESPTOOL="${ESPTOOL:-${LEVICULUM_RNODE_TOOLS:-$HOME/.rnode-tools/venv}/bin/esptool}"
BAUD=921600
PORT=""
BOARD=""
CHIP=""
FW_DIR="$REPO_DIR/.rnode-fw"
IMAGE=""

die() { echo "rnode-flash: $*" >&2; exit 1; }

run() {
    if [[ "$DRY_RUN" == "1" ]]; then
        printf '%s\n' "$*"
    else
        "$@"
    fi
}

# ESP32-S3 (QFN56) -> esp32s3, ESP32-D0WD-V3 -> esp32, i.e. the family, not
# the package. Longest family first, because every one of them starts with
# the string "esp32".
normalize_chip() {
    local raw="${1,,}"
    raw="${raw//-/}"
    case "$raw" in
        esp32s3*) echo esp32s3 ;;
        esp32s2*) echo esp32s2 ;;
        esp32c2*) echo esp32c2 ;;
        esp32c3*) echo esp32c3 ;;
        esp32c6*) echo esp32c6 ;;
        esp32h2*) echo esp32h2 ;;
        esp32p4*) echo esp32p4 ;;
        esp32*)   echo esp32 ;;
        *) return 1 ;;
    esac
}

chip_for_board() {
    case "$1" in
        heltec-v4) echo esp32s3 ;;
        tbeam) echo esp32 ;;
        auto) return 1 ;;
        *)
            if normalize_chip "$1" >/dev/null 2>&1 && [[ "$1" == esp32* ]]; then
                normalize_chip "$1"
            else
                die "unknown board '$1' (heltec-v4, tbeam, auto, or a chip name)"
            fi
            ;;
    esac
}

require_esptool5() {
    [[ -x "$ESPTOOL" ]] || die "no esptool at $ESPTOOL (run: just flash-rnode-setup)"
    local v
    v="$("$ESPTOOL" version 2>/dev/null | tail -1 || true)"
    case "$v" in
        5.*) ;;
        *) die "esptool $ESPTOOL is version '${v:-unknown}'; these recipes compose esptool 5 syntax (run: just flash-rnode-setup)" ;;
    esac
}

probe_chip() {
    require_esptool5
    local out detected
    out="$("$ESPTOOL" --port "$PORT" chip-id 2>&1)" \
        || die "could not talk to $PORT:"$'\n'"$out"
    # esptool 5 says "Connected to ESP32-S3 on <port>"; it also prints a
    # "Chip type: ESP32-S3 (QFN56)" line, and 4.x said "Chip is ...". Any of
    # the three answers the question, so read whichever appears first.
    detected="$(printf '%s\n' "$out" | sed -n \
        -e 's/^Connected to \([A-Za-z0-9-]*\) on .*/\1/p' \
        -e 's/^Chip type: *\([A-Za-z0-9-]*\).*/\1/p' \
        -e 's/^Chip is \([A-Za-z0-9-]*\).*/\1/p' | head -1)"
    [[ -n "$detected" ]] || die "could not read a chip name out of:"$'\n'"$out"
    normalize_chip "$detected" || die "unrecognised chip '$detected' on $PORT"
}

# name offset size, one per line. The bootloader row is the one that moves.
regions_for_chip() {
    case "$1" in
        esp32)
            cat <<'EOF'
bootloader 0x1000 0x4650
partitions 0x8000 0xc00
boot_app0 0xe000 0x2000
app 0x10000 0x200000
console 0x210000 0x1f0000
EOF
            ;;
        esp32s3)
            cat <<'EOF'
bootloader 0x0 0x8000
partitions 0x8000 0xc00
boot_app0 0xe000 0x2000
app 0x10000 0x200000
console 0x210000 0x1e0000
EOF
            ;;
        *)
            die "no region map recorded for $1 — only esp32 and esp32s3 have been measured; read a full image instead (read-image)"
            ;;
    esac
}

# The region map is resolved into a variable before anything runs: inside a
# `< <(...)` process substitution a die() would exit the subshell only, and
# the caller would carry on to flash with whatever it had.
cmd_extract() {
    local regions name offset size
    regions="$(regions_for_chip "$CHIP")" || exit 1
    run mkdir -p "$FW_DIR"
    while read -r name offset size; do
        run "$ESPTOOL" --chip "$CHIP" --port "$PORT" --baud "$BAUD" \
            read-flash "$offset" "$size" "$FW_DIR/$name.bin"
    done <<< "$regions"
}

cmd_write() {
    local regions name offset size args=()
    regions="$(regions_for_chip "$CHIP")" || exit 1
    while read -r name offset size; do
        [[ "$DRY_RUN" == "1" || -f "$FW_DIR/$name.bin" ]] \
            || die "no $name.bin in $FW_DIR — run the extract first"
        args+=("$offset" "$FW_DIR/$name.bin")
    done <<< "$regions"
    run "$ESPTOOL" --chip "$CHIP" --port "$PORT" --baud "$BAUD" \
        --before default-reset --after hard-reset \
        write-flash --flash-mode dio --flash-freq 80m --flash-size detect \
        "${args[@]}"
}

cmd_read_image() {
    [[ -n "$IMAGE" ]] || die "read-image needs --image <file>"
    run "$ESPTOOL" --chip "$CHIP" --port "$PORT" --baud "$BAUD" \
        read-flash 0x0 ALL "$IMAGE"
}

# --flash-size keep, not detect: a full-flash image already carries the
# board's own header and a restore must put back exactly what was read.
cmd_write_image() {
    [[ -n "$IMAGE" ]] || die "write-image needs --image <file>"
    [[ "$DRY_RUN" == "1" || -f "$IMAGE" ]] || die "no image at $IMAGE"
    run "$ESPTOOL" --chip "$CHIP" --port "$PORT" --baud "$BAUD" \
        --before default-reset --after hard-reset \
        write-flash --flash-size keep 0x0 "$IMAGE"
}

# The SHA-256 an ESP-IDF app image carries in its last 32 bytes, checked
# against the bytes before it. The firmware compares this against the target
# hash in its EEPROM on every boot (Device.h device_validate_partitions) and
# keeps the radio off on a mismatch; upstream's partition_hashes prints the
# same value.
app_hash() {
    python3 -I - "$1" <<'PY'
import hashlib, sys
d = open(sys.argv[1], "rb").read()
if len(d) < 64 or hashlib.sha256(d[:-32]).digest() != d[-32:]:
    sys.exit("rnode-flash: %s carries no appended SHA-256" % sys.argv[1])
print(d[-32:].hex())
PY
}

# Wait for the port to come back after a reset re-enumerated it.
wait_port() {
    [[ "$DRY_RUN" == "1" ]] && return 0
    local _
    for _ in $(seq 1 30); do
        [[ -e "$PORT" ]] && { sleep 3; return 0; }
        sleep 1
    done
    die "$PORT did not come back within 30 s"
}

# A firmware we built ourselves (scripts/build-rnode-firmware.sh), not one we
# extracted. Only the app region at 0x10000 is written: bootloader,
# partition table, console and EEPROM stay as Mark's release left them, so
# the device signature and provisioning survive. The new image's hash is
# then stored as the EEPROM target, which is what the upstream Makefile's
# upload-<board> targets do with `rnodeconf --firmware-hash`, and the board
# is reset so it validates against it.
cmd_write_app() {
    [[ -n "$IMAGE" ]] || die "write-app needs --image <file>"
    local hash
    if [[ "$DRY_RUN" == "1" ]]; then
        hash="<sha256 of $IMAGE>"
    else
        [[ -f "$IMAGE" ]] || die "no image at $IMAGE"
        hash="$(app_hash "$IMAGE")" || exit 1
    fi
    run "$ESPTOOL" --chip "$CHIP" --port "$PORT" --baud "$BAUD" \
        --before default-reset --after hard-reset \
        write-flash --flash-mode dio --flash-freq 80m --flash-size detect \
        0x10000 "$IMAGE"
    wait_port
    run "${RNODECONF[@]}" "$PORT" --firmware-hash "$hash"
    wait_target_hash "$hash"
    run "$ESPTOOL" --chip "$CHIP" --port "$PORT" chip-id
    wait_port
}

# The firmware commits the EEPROM one byte at a time, each a flash sector
# write (Utilities.h eeprom_update), and rnodeconf returns as soon as the
# frame is sent. Measured 2026-11-01 on the V4: a reset straight after it
# left 17 of the 32 bytes new and the rest of the old hash, so the board
# rebooted into a firmware it no longer validated. Read the target back
# until it is the whole hash, and only then reset.
wait_target_hash() {
    [[ "$DRY_RUN" == "1" ]] && { printf '%s\n' "${RNODECONF[*]} $PORT --get-target-firmware-hash (until $1)"; return 0; }
    local _ got
    for _ in $(seq 1 10); do
        sleep 2
        got="$("${RNODECONF[@]}" "$PORT" --get-target-firmware-hash 2>&1 \
            | sed -n 's/.*The target firmware hash is: \([0-9a-f]*\).*/\1/p' | tail -1)"
        [[ "$got" == "$1" ]] && { echo "rnode-flash: target hash stored: $got" >&2; return 0; }
    done
    die "the board's target hash is '${got:-unreadable}', not $1; do not reset it, run write-app again"
}

if [[ "${1:-}" == "hash" ]]; then
    [[ "${2:-}" == "--image" && -n "${3:-}" ]] || die "usage: $0 hash --image F"
    app_hash "$3"
    exit $?
fi

[[ $# -ge 1 ]] || die "usage: $0 <extract|write|write-app|read-image|write-image|chip> --port P [--board B|--chip C]"
ACTION="$1"
shift

while [[ $# -gt 0 ]]; do
    case "$1" in
        --port) PORT="${2:-}"; shift 2 ;;
        --board) BOARD="${2:-}"; shift 2 ;;
        --chip) CHIP="${2:-}"; shift 2 ;;
        --baud) BAUD="${2:-}"; shift 2 ;;
        --fw-dir) FW_DIR="${2:-}"; shift 2 ;;
        --image) IMAGE="${2:-}"; shift 2 ;;
        --esptool) ESPTOOL="${2:-}"; shift 2 ;;
        *) die "unknown argument '$1'" ;;
    esac
done

[[ -n "$PORT" ]] || die "no --port given"

if [[ -z "$CHIP" ]]; then
    if [[ -n "$BOARD" && "$BOARD" != "auto" ]]; then
        CHIP="$(chip_for_board "$BOARD")"
    elif [[ "$DRY_RUN" == "1" ]]; then
        die "a dry run cannot probe the device — pass --board or --chip"
    else
        CHIP="$(probe_chip)"
        echo "rnode-flash: detected $CHIP on $PORT" >&2
    fi
else
    CHIP="$(normalize_chip "$CHIP")" || die "unrecognised chip name"
fi

[[ "$DRY_RUN" == "1" || "$ACTION" == "chip" ]] || require_esptool5

case "$ACTION" in
    extract) cmd_extract ;;
    write) cmd_write ;;
    write-app) cmd_write_app ;;
    read-image) cmd_read_image ;;
    write-image) cmd_write_image ;;
    chip) echo "$CHIP" ;;
    *) die "unknown action '$ACTION'" ;;
esac
