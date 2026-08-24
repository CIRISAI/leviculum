#!/usr/bin/env bash
# Cargo runner — builds UF2 from ELF and deploys to bootloader.
# Invoked automatically by `cargo run` via .cargo/config.toml.
#
# Default behaviour: flash EVERY attached device matching the configured
# USB VID/PID, sequentially. Touch-free for healthy firmware (1200-baud-touch
# triggers the Adafruit bootloader); falls back to manual double-tap RESET
# prompt per device if touch fails (crashed firmware, missing handler).
#
# Per-board parameters (defaults match the T114):
#   LEVICULUM_USB_VID         USB Vendor ID hex w/o 0x prefix (default: 1209)
#   LEVICULUM_USB_PID         USB Product ID hex w/o 0x prefix (default: 0001)
#   LEVICULUM_BOARD_NAME      Human-readable board name in messages
#                             (default: T114)
#   LEVICULUM_UF2_BOARD_ID    Board-ID string in INFO_UF2.TXT, used to confirm
#                             the right bootloader is mounted
#                             (default: HT-n5262)
#   LEVICULUM_DOUBLE_TAP_HINT One extra line under the manual double-tap
#                             prompt, for a board where "double-tap RESET" is
#                             not enough to act on. The Pocket V2 has no
#                             externally accessible RESET at all, so its
#                             owner is sent looking for a button that does not
#                             exist (Codeberg #261). Empty on a board whose
#                             RESET is a button on the outside.
#
# Selective flashing: set LEVICULUM_FLASH_ONLY=<port-or-symlink> to target
# exactly one device. Useful for A/B firmware testing.
#
# Pipeline: ELF → flat binary (objcopy) → UF2 (bin2uf2) → copy to UF2 drive
#
# NOTE: --base must match FLASH ORIGIN in memory.x (currently 0x27000 for S140 v7.3.0,
# bumped from 0x26000 for v6.1.1 in bug32-softdevice-spike Day 3).
# Both T114 and RAK4631 share this layout, so FLASH_BASE / FAMILY_ID are not
# parameterized — they are fixed properties of the Adafruit nRF52 UF2 family.
#
# Because that base assumes a SoftDevice, no write happens before
# guard_softdevice (tools/softdevice-guard.sh) has read the board's own
# `SoftDevice:` line. Everything about the SoftDevice — why the mismatch is a
# soft brick, how to remedy one, why we cannot simply link the blob in — is in
# docs/src/concepts/lnode-flashing.md.

set -euo pipefail

ELF="${1:?Usage: uf2-runner.sh <ELF>}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
NRF_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
TARGET_DIR="$NRF_DIR/target"
BIN_FILE="$ELF.bin"
UF2_FILE="$ELF.uf2"
UF2_TIMEOUT="${UF2_TIMEOUT:-30}"

# Reliability knobs (see flash_one_device). A UF2 copy returning 0 does NOT
# prove the flash took — the bootloader flashes + reboots asynchronously — so we
# verify the application re-enumerated and retry a bounded number of times.
#   FLASH_ATTEMPTS      number of automatic detect→copy→verify cycles per board
#   UF2_VERIFY_TIMEOUT  seconds to wait for the app PID to reappear after a copy
FLASH_ATTEMPTS="${LEVICULUM_FLASH_ATTEMPTS:-3}"
UF2_VERIFY_TIMEOUT="${LEVICULUM_UF2_VERIFY_TIMEOUT:-20}"

# Non-interactive mode: no human is present to satisfy a manual double-tap
# prompt (the automated VFIO tier3 run). Detected via an explicit flag the
# harness can set, or the absence of a controlling TTY on stdin. In this mode
# flash_one_device retries then FAILS instead of blocking on a prompt.
noninteractive() {
    [ -n "${LEVICULUM_NONINTERACTIVE:-}" ] && return 0
    [ ! -t 0 ]
}

FLASH_BASE=0x27000
FAMILY_ID=0xADA52840

# The SoftDevice guard. Sourced rather than inlined so its parsing and its
# decision can be driven from tools/test-softdevice-guard.sh without a board.
if [ ! -f "$SCRIPT_DIR/softdevice-guard.sh" ]; then
    echo "Error: $SCRIPT_DIR/softdevice-guard.sh is missing; refusing to flash" >&2
    echo "       without the SoftDevice check that keeps a 6.1.1 board alive." >&2
    exit 1
fi
# shellcheck source=leviculum-nrf/tools/softdevice-guard.sh
. "$SCRIPT_DIR/softdevice-guard.sh"

# Which volume a write goes to. Sourced for the same reason as the guard: it
# decides where the image lands, and tools/test-uf2-volumes.sh drives that
# decision against fixtures without a board.
if [ ! -f "$SCRIPT_DIR/uf2-volumes.sh" ]; then
    echo "Error: $SCRIPT_DIR/uf2-volumes.sh is missing; refusing to flash without" >&2
    echo "       the volume selection that keeps the image off the wrong board." >&2
    exit 1
fi
# shellcheck source=leviculum-nrf/tools/uf2-volumes.sh
. "$SCRIPT_DIR/uf2-volumes.sh"

# Which board ended up with the image. Sourced for the same reason as the two
# above: it decides what the summary claims, and tools/test-fw-readback.sh
# drives that decision against stubbed boards with no hardware.
if [ ! -f "$SCRIPT_DIR/fw-readback.sh" ]; then
    echo "Error: $SCRIPT_DIR/fw-readback.sh is missing; refusing to flash without" >&2
    echo "       the read-back that binds the image to the board it landed on." >&2
    exit 1
fi
# shellcheck source=leviculum-nrf/tools/fw-readback.sh
. "$SCRIPT_DIR/fw-readback.sh"

# Per-board parameters (default to T114 values for backward compatibility).
BOARD_VID="${LEVICULUM_USB_VID:-1209}"
BOARD_PID="${LEVICULUM_USB_PID:-0001}"
BOARD_NAME="${LEVICULUM_BOARD_NAME:-T114}"
BOOTLOADER_BOARD_ID="${LEVICULUM_UF2_BOARD_ID:-HT-n5262}"
DOUBLE_TAP_HINT="${LEVICULUM_DOUBLE_TAP_HINT:-}"

# A mount we make is a mount we give back. The registry records ownership and
# the EXIT trap drains it, so no path out of this script — an early exit, a
# failed gate, a Ctrl-C — can leave a volume behind. A left-behind volume is
# not a cosmetic leak: it shadows every other board in the search path until
# somebody unmounts by hand (Codeberg #341).
uf2_registry_init
trap 'release_unclaimed_uf2_volumes ""; rm -f "$UF2_MOUNT_REGISTRY" "$UF2_SEEN_REGISTRY"' EXIT

# --- Step 1: Find objcopy ---------------------------------------------------

find_objcopy() {
    # Try llvm-objcopy from rustup first
    local sysroot
    sysroot="$(rustc --print sysroot 2>/dev/null || true)"
    if [ -n "$sysroot" ]; then
        local candidate
        candidate="$(find "$sysroot/lib/rustlib" -name llvm-objcopy -type f 2>/dev/null | head -1)"
        if [ -n "$candidate" ] && [ -x "$candidate" ]; then
            echo "$candidate"
            return
        fi
    fi
    # Try llvm-objcopy in PATH
    if command -v llvm-objcopy >/dev/null 2>&1; then
        echo "llvm-objcopy"
        return
    fi
    # Try arm-none-eabi-objcopy in PATH
    if command -v arm-none-eabi-objcopy >/dev/null 2>&1; then
        echo "arm-none-eabi-objcopy"
        return
    fi
    echo ""
}

OBJCOPY="$(find_objcopy)"
if [ -z "$OBJCOPY" ]; then
    echo "Error: No objcopy found. Install one of:" >&2
    echo "  rustup component add llvm-tools   (recommended)" >&2
    echo "  apt install gcc-arm-none-eabi" >&2
    exit 1
fi

# --- Step 2: Build bin2uf2 (cached) -----------------------------------------

BIN2UF2="$TARGET_DIR/bin2uf2"
BIN2UF2_SRC="$SCRIPT_DIR/bin2uf2.rs"

if [ ! -f "$BIN2UF2" ] || [ "$BIN2UF2_SRC" -nt "$BIN2UF2" ]; then
    echo "==> Building bin2uf2 tool"
    mkdir -p "$TARGET_DIR"
    rustc "$BIN2UF2_SRC" -o "$BIN2UF2" --edition 2021
fi

# --- Step 3: ELF → flat binary ----------------------------------------------

echo "==> Converting ELF to binary ($(basename "$OBJCOPY"))"
# -R .bss -R .uninit: exclude NOBITS sections so the binary only contains FLASH
# content. Without this, a pre-2020 llvm-objcopy bug could include .bss (VMA in
# RAM at 0x20003000), producing a ~500 MB binary and wrong UF2 target addresses.
"$OBJCOPY" -O binary -R .bss -R .uninit "$ELF" "$BIN_FILE"

# Sanity check: firmware must fit in application region (824K = 0xCE000 bytes).
# If the binary is larger, something went wrong (e.g. NOBITS leak or wrong ELF).
BIN_SIZE="$(stat -c%s "$BIN_FILE")"
MAX_SIZE=$((0xCE000))
if [ "$BIN_SIZE" -gt "$MAX_SIZE" ]; then
    echo "Error: Binary is ${BIN_SIZE} bytes, exceeds application region (${MAX_SIZE} bytes)." >&2
    echo "       This would overwrite the bootloader. Aborting." >&2
    rm -f "$BIN_FILE"
    exit 1
fi

# --- Step 4: Binary → UF2 ---------------------------------------------------

echo "==> Converting binary to UF2 (base: $FLASH_BASE, family: nRF52840)"
"$BIN2UF2" --base "$FLASH_BASE" --family "$FAMILY_ID" "$BIN_FILE" "$UF2_FILE"

# The identity of the image we are about to write, taken from the image. Not
# from `git rev-parse`: that answers about the working tree at the moment of
# the question, which is a different thing from the bytes on their way to a
# board, and it has no way to say "dirty" at all — so two different images
# built from one commit would both answer with that commit. The firmware
# prints this same stamp on its debug port, which is what makes the read-back
# a comparison rather than an inference.
BUILT_STAMP="$(fw_image_stamp "$BIN_FILE")"
if [ -n "$BUILT_STAMP" ]; then
    echo "==> Image build stamp: $BUILT_STAMP (read from $(basename "$BIN_FILE"))"
else
    echo "[uf2-runner] WARNING: this image carries no [FW_BUILD] stamp; the flash" >&2
    echo "             can be performed but not confirmed against any board." >&2
fi

# UF2 volume discovery (find_uf2_drive), selection by Board-ID
# (poll_matching_drive) and the unmount obligation that goes with mounting
# them live in tools/uf2-volumes.sh, sourced above.

# --- Helper: enumerate all attached transport ports for the current board --
# Prints one path per line, sorted by ID_SERIAL_SHORT (deterministic order).
# Empty output means no device matching $BOARD_VID:$BOARD_PID with interface
# 02 was found.  Single udevadm call per port (cached output grepped four
# times) — saves ~75% of subprocess calls vs. a four-call form.

find_all_t114_transport_ports() {
    local port props vid pid iface serial out=""
    for port in /dev/ttyACM*; do
        [ -c "$port" ] || continue
        props="$(udevadm info -q property "$port" 2>/dev/null || true)"
        vid="$(   echo "$props" | grep '^ID_VENDOR_ID='         | cut -d= -f2)"
        pid="$(   echo "$props" | grep '^ID_MODEL_ID='          | cut -d= -f2)"
        iface="$( echo "$props" | grep '^ID_USB_INTERFACE_NUM=' | cut -d= -f2)"
        if [ "$vid" = "$BOARD_VID" ] && [ "$pid" = "$BOARD_PID" ] && [ "$iface" = "02" ]; then
            serial="$(echo "$props" | grep '^ID_SERIAL_SHORT='  | cut -d= -f2)"
            out="$out$serial $port"$'\n'
        fi
    done
    echo -n "$out" | sort | awk '{print $2}'
}

# --- Helper: copy the UF2 image onto a (validated) bootloader drive ---------
# UF2 bootloaders intercept FAT writes and scan each 512-byte sector for UF2
# magic. Write via cp (same approach as uf2conv.py / the Heltec toolchain).
# sync may return I/O errors because the bootloader resets after the final UF2
# block — normal and tolerated. Returns 0 on a successful copy, 2 on a write
# failure (write-protect, FS error, mid-flight unplug), 3 when the SoftDevice
# guard refused.
#
# The guard runs here rather than at each call site because this is the single
# place the application image reaches a board: a refusal here cannot be routed
# around. 3 is separate from 2 on purpose — a write failure is worth retrying,
# a wrong SoftDevice is not, and it will still be wrong three attempts later.
# Args: $1 = drive path, $2 = hint
copy_uf2_to_drive() {
    local drive="$1" hint="$2"
    local uf2_size uf2_blocks

    if ! guard_softdevice "$drive" "$hint"; then
        return 3
    fi
    uf2_size="$(stat -c%s "$UF2_FILE")"
    uf2_blocks=$((uf2_size / 512))
    echo "==> $hint: deploying firmware.uf2 (${uf2_size} bytes, ${uf2_blocks} blocks) to $drive"

    # Every exit below gives the volume back if we were the ones who mounted
    # it, and leaves it alone if we were not. Hard-coding /mnt here was half of
    # why a foreign volume mounted at /mnt could never be cleaned up (#341).
    if [ -w "$drive" ]; then
        if ! cp "$UF2_FILE" "$drive/NEW.UF2" 2>/dev/null; then
            echo "[uf2-runner] $hint: UF2 drive mounted at $drive but cp failed" >&2
            release_uf2_volume "$drive"
            return 2
        fi
        sync 2>/dev/null || true
    else
        if ! sudo cp "$UF2_FILE" "$drive/NEW.UF2" 2>/dev/null; then
            echo "[uf2-runner] $hint: UF2 drive mounted at $drive but sudo cp failed" >&2
            release_uf2_volume "$drive"
            return 2
        fi
        sudo sync 2>/dev/null || true
    fi

    release_uf2_volume "$drive"
    return 0
}

# --- Helper: is this board's APPLICATION firmware back on the bus? ----------
# The Adafruit/Nordic UF2 bootloader enumerates a DIFFERENT VID/PID than the
# application, so an app-PID match ($BOARD_VID:$BOARD_PID) is only ever true
# once the freshly-flashed firmware has booted and re-enumerated — never while
# the board sits in the bootloader. When the pre-flash USB serial is known we
# match it precisely via the transport CDC port (interface 02), so a sibling
# board of the same PID already in app mode is never mistaken for this one;
# otherwise we fall back to a plain lsusb VID:PID presence match.
# Args: $1 = pre-flash serial ("" if unknown)
board_app_returned() {
    local serial="${1:-}"
    if [ -n "$serial" ]; then
        local p s
        while IFS= read -r p; do
            [ -n "$p" ] || continue
            s="$(udevadm info -q property "$p" 2>/dev/null | grep '^ID_SERIAL_SHORT=' | cut -d= -f2 || true)"
            [ "$s" = "$serial" ] && return 0
        done <<< "$(find_all_t114_transport_ports)"
        return 1
    fi
    lsusb 2>/dev/null | grep -qiE "[[:space:]]${BOARD_VID}:${BOARD_PID}[[:space:]]"
}

# --- Helper: verify the board rebooted into the APP after a UF2 copy --------
# A copy returning 0 does NOT mean the flash took: the bootloader flashes and
# reboots asynchronously. Poll (bounded, UF2_VERIFY_TIMEOUT s) for BOTH the app
# to re-enumerate (board_app_returned) AND our board's UF2 drive to be gone (a
# drive that lingers/reappears == the app crashed straight back to DFU). Return
# 0 the moment both hold; non-zero on timeout (== the flash did not take).
# Args: $1 = hint, $2 = pre-flash serial ("" if unknown)
verify_app_return() {
    local hint="$1" serial="${2:-}"
    local ticks=$((UF2_VERIFY_TIMEOUT * 2))
    local tick=0
    while [ "$tick" -lt "$ticks" ]; do
        sleep 0.5
        tick=$((tick + 1))
        if board_app_returned "$serial"; then
            if ! matching_uf2_volume_present; then
                echo "[uf2-runner] $hint: app-returned (${BOARD_VID}:${BOARD_PID} present, bootloader drive gone) after $((tick / 2))s"
                return 0
            fi
        fi
    done
    echo "[uf2-runner] $hint: app-NOT-returned (no ${BOARD_VID}:${BOARD_PID} within ${UF2_VERIFY_TIMEOUT}s of copy)" >&2
    return 1
}

# --- Read-back bookkeeping for the summary ----------------------------------
# One formatted line per board, filled in by fw_attribute's verdict. These are
# what Step 7 prints: the summary states what boards said about themselves,
# never what the enumeration order suggested.
FW_CONFIRMED_SERIALS=""    # serials proven to carry the image
FW_UNCONFIRMED_LINES=""    # a board that answered wrong, or did not answer
FW_UNATTRIBUTED_LINES=""   # a write nothing could be bound to

# File the verdict of the last fw_attribute and echo it. A confirmed flash and
# a corrected attribution are ordinary progress and go to stdout; everything
# else is a thing that went wrong and goes to stderr.
record_attribution() {
    case "$FW_ATTR_OUTCOME" in
    match | rebound)
        echo "[uf2-runner] $FW_ATTR_MESSAGE"
        FW_CONFIRMED_SERIALS="$FW_CONFIRMED_SERIALS"$'\n'"$FW_ATTR_SERIAL"
        ;;
    ambiguous)
        echo "[uf2-runner] $FW_ATTR_MESSAGE" >&2
        FW_UNATTRIBUTED_LINES="$FW_UNATTRIBUTED_LINES"$'\n'"$FW_ATTR_MESSAGE"
        ;;
    *)
        echo "[uf2-runner] $FW_ATTR_MESSAGE" >&2
        FW_UNCONFIRMED_LINES="$FW_UNCONFIRMED_LINES"$'\n'"$FW_ATTR_MESSAGE"
        ;;
    esac
}

# --- Flash one device: touch → copy → verify-app-return, with retries -------
# Wraps a full detect→copy→verify cycle in a bounded retry loop
# ($FLASH_ATTEMPTS). Only returns 0 once the application firmware is CONFIRMED
# back on the bus; a copy that never boots the app is a FAILED flash (non-zero),
# NOT a false success. Self-heals a board stuck in the bootloader from a prior
# attempt by re-copying directly (no touch needed). After the retries are
# exhausted the manual double-tap prompt is shown ONLY in interactive mode; a
# non-interactive run (VFIO tier3, no human) skips the prompt and fails.
# Args: $1 = port path ("" if none, e.g. a crashed board already in DFU)
#       $2 = pre-flash USB serial ("" if unknown)
# Returns 0 = the image is on a board and confirmed there (or the board could
#             not be read, which is reported but not retried);
#         4 = an image was written that could not be bound to any board;
#         other non-zero = flash failed after all attempts.
flash_one_device() {
    local port="$1" serial="${2:-}"
    local hint="${port:-(unknown device)}"
    [ -n "$serial" ] && hint="$hint (serial=$serial)"

    local grace_ticks=$((UF2_TIMEOUT * 2))
    # Whether a volume for THIS board was ever found. It decides which failure
    # the give-up line reports: a copy that did not boot, or no volume at all.
    local saw_drive=0
    # The last verdict that did not end the loop, so the give-up line can name
    # what was actually observed instead of asserting a symptom.
    local pending_outcome="" pending_message="" pending_stamp=""
    local attempt
    for (( attempt = 1; attempt <= FLASH_ATTEMPTS; attempt++ )); do
        echo ""
        echo "==> $hint: flash attempt $attempt/$FLASH_ATTEMPTS"

        # Is the board already sitting in ITS bootloader? (stuck from a prior
        # attempt, or crashed-and-double-tapped) → re-copy directly, no touch.
        local drive
        drive="$(poll_matching_drive "$hint" 0 || true)"
        if [ -n "$drive" ]; then
            echo "[uf2-runner] $hint: bootloader-persisted (UF2 drive at $drive already present) — copying without touch"
        else
            # Not in bootloader: issue the 1200-baud touch to trigger it, then
            # wait (up to UF2_TIMEOUT) for the UF2 drive. Touch is best-effort —
            # the port may have renumbered/vanished across a prior attempt, and
            # old firmware ignores it entirely.
            if [ -n "$port" ]; then
                echo "[uf2-runner] $hint: issuing 1200-baud touch"
                stty -F "$port" 1200 2>/dev/null || true
            fi
            drive="$(poll_matching_drive "$hint" "$grace_ticks" || true)"
        fi

        if [ -z "$drive" ]; then
            echo "[uf2-runner] $hint: attempt $attempt — $(uf2_no_match_message) (waited ${UF2_TIMEOUT}s)" >&2
            continue
        fi

        saw_drive=1
        echo "==> $hint: found UF2 drive at $drive ($(basename "$drive"))"
        local copy_rc=0
        copy_uf2_to_drive "$drive" "$hint" || copy_rc=$?
        if [ "$copy_rc" -eq 3 ]; then
            # Refused by the SoftDevice guard, which already said why. Retrying
            # or prompting for a double-tap would only repeat the refusal.
            return 3
        fi
        if [ "$copy_rc" -ne 0 ]; then
            echo "[uf2-runner] $hint: attempt $attempt — copy failed" >&2
            continue
        fi

        if verify_app_return "$hint" "$serial"; then
            # A board of this type is back on the bus. That is NOT the same as
            # "this board runs this image" — a UF2 volume carries no serial, so
            # the pairing that got us here was enumeration order and nothing
            # more (#343). Ask the boards themselves.
            fw_attribute "$hint (attempt $attempt)" "$BUILT_STAMP" "$serial"
            case "$FW_ATTR_OUTCOME" in
            match | noanswer | nostamp)
                # Confirmed, or unreadable and therefore not retryable: a mute
                # board says nothing more on the second attempt than on the
                # first, and re-flashing it would spend flash cycles to learn
                # the same nothing.
                record_attribution
                return 0
                ;;
            rebound)
                # The image is real and on another board — file that now, it
                # will not be found again once that board is bound. This board
                # still has not been flashed, so the loop goes round: the next
                # attempt touches it and finds a volume that is its own.
                record_attribution
                pending_outcome="rebound"
                pending_stamp="$FW_ATTR_SERIAL"
                continue
                ;;
            ambiguous)
                record_attribution
                return 4
                ;;
            mismatch)
                # The copy did not take. Worth another attempt; only the last
                # verdict is filed, so retries do not multiply summary lines.
                pending_outcome="mismatch"
                pending_message="$FW_ATTR_MESSAGE"
                pending_stamp="$FW_ATTR_STAMP"
                echo "[uf2-runner] $hint: attempt $attempt — $serial still reports" \
                    "$FW_ATTR_STAMP, not $BUILT_STAMP; retrying" >&2
                continue
                ;;
            esac
        fi
        echo "[uf2-runner] $hint: attempt $attempt — app did not return; retrying" >&2
    done

    # Retries exhausted. Interactive: one last human-driven double-tap round.
    # Non-interactive: no human to answer the prompt, so fail straight through.
    if noninteractive; then
        echo "[uf2-runner] $hint: non-interactive — skipping manual double-tap prompt" >&2
    else
        echo "==> $hint: automatic flash failed after $FLASH_ATTEMPTS attempts."
        echo "    ┌───────────────────────────────────────────────────┐"
        printf  "    │  Double-tap RESET on %-7s to enter bootloader. │\n" "$BOARD_NAME"
        echo "    └───────────────────────────────────────────────────┘"
        # Per board, because on a Pocket V2 the line above is not something a
        # person can act on: there is no RESET button on the outside of that
        # case (Codeberg #261). One prompt, board-specific wording — not a
        # second prompt path.
        if [ -n "$DOUBLE_TAP_HINT" ]; then echo "    $DOUBLE_TAP_HINT"; fi
        echo "==> Waiting for UF2 drive (${UF2_TIMEOUT}s)..."
        local drive
        drive="$(poll_matching_drive "$hint" "$grace_ticks" || true)"
        if [ -n "$drive" ]; then
            saw_drive=1
            if copy_uf2_to_drive "$drive" "$hint" && verify_app_return "$hint" "$serial"; then
                # Same rule as the automatic path: a human double-tap does not
                # make the enumeration order true either.
                fw_attribute "$hint (manual double-tap)" "$BUILT_STAMP" "$serial"
                record_attribution
                case "$FW_ATTR_OUTCOME" in
                match | noanswer | nostamp) return 0 ;;
                ambiguous) return 4 ;;
                rebound)
                    pending_outcome="rebound"
                    pending_stamp="$FW_ATTR_SERIAL"
                    ;;
                mismatch)
                    pending_outcome="mismatch"
                    pending_message=""
                    pending_stamp="$FW_ATTR_STAMP"
                    ;;
                esac
            fi
        fi
    fi

    # Report what was observed. The old line asserted "app never re-enumerated"
    # unconditionally, which is false whenever no volume for this board was ever
    # found — the case #341 is about, where the message named a symptom that had
    # not been reached and sent the diagnosis an hour in the wrong direction.
    # The read-back adds two more observations that are not "never
    # re-enumerated" either: every write was taken by a different board, and
    # the board took a write and still reports something else.
    if [ -n "$pending_message" ]; then
        FW_UNCONFIRMED_LINES="$FW_UNCONFIRMED_LINES"$'\n'"$pending_message"
    fi
    if [ "$pending_outcome" = "rebound" ]; then
        echo "[uf2-runner] $hint: FLASH FAILED after $FLASH_ATTEMPTS attempts" \
            "(every write was received by serial=$pending_stamp, not by this board;" \
            "the volume could not be bound to it)" >&2
        return 1
    fi
    if [ "$pending_outcome" = "mismatch" ]; then
        echo "[uf2-runner] $hint: FLASH FAILED after $FLASH_ATTEMPTS attempts" \
            "(UF2 copied, board still reports $pending_stamp, not $BUILT_STAMP)" >&2
        return 1
    fi
    if [ "$saw_drive" -eq 1 ]; then
        echo "[uf2-runner] $hint: FLASH FAILED after $FLASH_ATTEMPTS attempts" \
            "(UF2 copied, app never re-enumerated)" >&2
    else
        echo "[uf2-runner] $hint: FLASH FAILED after $FLASH_ATTEMPTS attempts —" \
            "$(uf2_no_match_message)" >&2
    fi
    return 1
}

# --- Step 5: Determine target list ------------------------------------------

if [ -n "${LEVICULUM_FLASH_ONLY:-}" ]; then
    PORTS="$LEVICULUM_FLASH_ONLY"
    echo "==> LEVICULUM_FLASH_ONLY set; targeting only $PORTS"
else
    PORTS="$(find_all_t114_transport_ports)"
fi

# Newline-separated lists of ports that flashed successfully / failed. A write
# that could not be bound to a board is not tracked here: it has no port to
# name, which is the whole of what is wrong with it, so it is carried as the
# read-back verdict itself in FW_UNATTRIBUTED_LINES.
FLASHED_PORTS=""
FAILED_PORTS=""

# --- Step 6: Flash loop -----------------------------------------------------

# Tracks serials whose UF2 was successfully copied (one per line). Captured
# in-loop just before the touch so we can identify devices across renumber.
FLASHED_SERIALS=""

if [ -z "$PORTS" ]; then
    # No board visible on VID/PID — either none attached, all already in
    # bootloader mode (UF2 drive only), or all crashed. Run one round of the
    # legacy fallback (manual prompt + UF2-drive polling).
    echo "[uf2-runner] no $BOARD_NAME transport port detected; awaiting manual double-tap"
    FLASH_RC=0
    flash_one_device "" "" || FLASH_RC=$?
    case "$FLASH_RC" in
    0) FLASHED_PORTS="(unknown)" ;;
    4) : ;; # the verdict is already filed under FW_UNATTRIBUTED_LINES
    *) FAILED_PORTS="(unknown)" ;;
    esac
else
    NUM=$(echo "$PORTS" | wc -l)
    echo "==> Flashing $NUM $BOARD_NAME(s)"
    INDEX=0
    while IFS= read -r PORT; do
        [ -n "$PORT" ] || continue
        INDEX=$((INDEX + 1))
        # Capture this port's serial BEFORE the touch so we can recognise
        # the device after it renumbers.
        PORT_SERIAL="$(udevadm info -q property "$PORT" 2>/dev/null | grep '^ID_SERIAL_SHORT=' | cut -d= -f2 || true)"
        echo ""
        if [ -n "$PORT_SERIAL" ]; then
            echo "==> ($INDEX/$NUM) trying $BOARD_NAME at $PORT (serial=$PORT_SERIAL)"
        else
            echo "==> ($INDEX/$NUM) trying $BOARD_NAME at $PORT"
        fi
        # flash_one_device owns the 1200-baud touch, the copy, the app-return
        # verification and the bounded retry loop. Old firmware ignores the
        # touch; new firmware writes the GPREGRET magic and resets into the UF2
        # bootloader. It only returns 0 once the app is CONFIRMED back on USB.
        FLASH_RC=0
        flash_one_device "$PORT" "$PORT_SERIAL" || FLASH_RC=$?
        case "$FLASH_RC" in
        0)
            FLASHED_PORTS="$FLASHED_PORTS"$'\n'"$PORT"
            [ -n "$PORT_SERIAL" ] && FLASHED_SERIALS="$FLASHED_SERIALS"$'\n'"$PORT_SERIAL"
            ;;
        4)
            echo "[uf2-runner] ($INDEX/$NUM) UNATTRIBUTED — continuing with next $BOARD_NAME" >&2
            ;;
        *)
            echo "[uf2-runner] ($INDEX/$NUM) FAILED — continuing with next $BOARD_NAME" >&2
            FAILED_PORTS="$FAILED_PORTS"$'\n'"$PORT"
            ;;
        esac
    done <<< "$PORTS"
fi

# Strip leading newlines.
FLASHED_SERIALS="$(echo -n "$FLASHED_SERIALS" | sed '/^$/d')"

# Crashed-firmware recovery pass: a board with crashed app firmware never
# enumerates as a transport CDC port — invisible to the main touch loop.
# If the user double-taps the crashed device BEFORE running this script (or
# between flashes), its Adafruit bootloader appears as a UF2 mass-storage
# drive. Flash whatever's still mounted after the touch loop. Filtered by
# the board-specific INFO_UF2.TXT Board-ID so only the configured bootloader
# is touched.
# The loop ends when the volume goes away, which is what a bootloader does
# once it has taken the image. A volume that is STILL there afterwards did not
# take it, so writing it again would only repeat the failure and inflate the
# summary — hence the already-written set, plus a round cap as a backstop.
RECOVERY_DONE=""
RECOVERY_ROUNDS=0
while [ "$RECOVERY_ROUNDS" -lt 4 ]; do
    RECOVERY_ROUNDS=$((RECOVERY_ROUNDS + 1))
    # poll_matching_drive rather than the first volume in the search path: this
    # loop used to `break` the moment it met a foreign volume, so a T114 parked
    # in its bootloader disabled crashed-firmware recovery for every other
    # board just as it disabled the main loop (#341).
    EXTRA_DRIVE="$(poll_matching_drive "(crashed-recovery)" 0 || true)"
    [ -n "$EXTRA_DRIVE" ] || break
    case "$RECOVERY_DONE" in
    *"|$EXTRA_DRIVE|"*) break ;;
    esac
    RECOVERY_DONE="$RECOVERY_DONE|$EXTRA_DRIVE|"
    echo ""
    echo "==> Extra UF2 drive at $EXTRA_DRIVE — flashing crashed-firmware $BOARD_NAME (no transport port)"
    # This path writes without going through copy_uf2_to_drive, so it needs
    # the guard of its own. `break` rather than `continue`: looping would
    # refuse the same board forever.
    if ! guard_softdevice "$EXTRA_DRIVE" "(crashed-recovery)"; then
        FAILED_PORTS="$FAILED_PORTS"$'\n'"(crashed-recovery)"
        release_uf2_volume "$EXTRA_DRIVE"
        break
    fi
    RECOVERY_COPIED=0
    if [ -w "$EXTRA_DRIVE" ]; then
        if cp "$UF2_FILE" "$EXTRA_DRIVE/NEW.UF2" 2>/dev/null; then
            sync 2>/dev/null || true
            RECOVERY_COPIED=1
        else
            echo "[uf2-runner] (crashed-recovery): cp to $EXTRA_DRIVE failed" >&2
        fi
    else
        if sudo cp "$UF2_FILE" "$EXTRA_DRIVE/NEW.UF2" 2>/dev/null; then
            sudo sync 2>/dev/null || true
            RECOVERY_COPIED=1
        else
            echo "[uf2-runner] (crashed-recovery): sudo cp to $EXTRA_DRIVE failed" >&2
        fi
    fi
    release_uf2_volume "$EXTRA_DRIVE"
    if [ "$RECOVERY_COPIED" -eq 0 ]; then
        # A copy that failed will fail the same way on the same volume next
        # round, and the volume is still there — stop instead of spinning.
        FAILED_PORTS="$FAILED_PORTS"$'\n'"(crashed-recovery)"
        break
    fi
    # Wait for the bootloader to process the file and disappear before
    # checking for more drives. Without this the same drive could be picked
    # up twice in quick succession.
    sleep 3
    # This pass has no candidate at all: it wrote to a volume it found lying
    # there. Asking every board which one now carries the image is the only way
    # it can name a recipient truthfully — and when none or several answer, the
    # honest report is that it does not know which board it wrote.
    fw_attribute "(crashed-recovery)" "$BUILT_STAMP" ""
    record_attribution
    case "$FW_ATTR_OUTCOME" in
    rebound | match)
        FLASHED_PORTS="$FLASHED_PORTS"$'\n'"(crashed-recovery serial=$FW_ATTR_SERIAL)"
        FLASHED_SERIALS="$FLASHED_SERIALS"$'\n'"$FW_ATTR_SERIAL"
        ;;
    *)
        # Either nothing to compare against (no stamp in the image) or nothing
        # that answered with it. record_attribution has already filed which.
        ;;
    esac
done

# Strip leading newlines from accumulated lists.
FLASHED_PORTS="$(echo -n "$FLASHED_PORTS" | sed '/^$/d')"
FAILED_PORTS="$(echo -n "$FAILED_PORTS"   | sed '/^$/d')"

# --- Step 7: Boot wait + summary --------------------------------------------

NUM_FLASHED=0
if [ -n "$FLASHED_PORTS" ]; then
    NUM_FLASHED=$(echo "$FLASHED_PORTS" | wc -l)
fi

# Wait up to 10 s for the flashed devices to re-enumerate as T114 transport
# ports. We poll until every serial in FLASHED_SERIALS is present, or until
# we time out. This is stricter than "any N ports visible": with
# LEVICULUM_FLASH_ONLY targeting one specific T114, an unrelated already-
# present T114 must not satisfy the count.
echo ""
echo "==> Waiting for flashed devices to re-enumerate..."
BOOT_TIMEOUT=20  # ticks at 0.5s = 10 s
boot_tick=0
CURRENT_PORTS=""
while [ "$boot_tick" -lt "$BOOT_TIMEOUT" ]; do
    CURRENT_PORTS="$(find_all_t114_transport_ports)"
    # Build current-serial set.
    cur_serial_set=""
    if [ -n "$CURRENT_PORTS" ]; then
        while IFS= read -r p; do
            [ -n "$p" ] || continue
            s="$(udevadm info -q property "$p" 2>/dev/null | grep '^ID_SERIAL_SHORT=' | cut -d= -f2 || true)"
            [ -n "$s" ] && cur_serial_set="$cur_serial_set $s"
        done <<< "$CURRENT_PORTS"
    fi
    # Count how many FLASHED_SERIALS are present.
    matched=0
    expected=0
    if [ -n "$FLASHED_SERIALS" ]; then
        expected=$(echo "$FLASHED_SERIALS" | wc -l)
        while IFS= read -r fs; do
            [ -n "$fs" ] || continue
            case " $cur_serial_set " in
                *" $fs "*) matched=$((matched + 1)) ;;
            esac
        done <<< "$FLASHED_SERIALS"
    fi
    if [ "$expected" -gt 0 ] && [ "$matched" -ge "$expected" ]; then
        sleep 0.5  # let udev settle symlinks
        CURRENT_PORTS="$(find_all_t114_transport_ports)"
        break
    fi
    # Fallback for the legacy "(unknown)" path with no captured serial: any
    # port reappearing within timeout is good enough.
    if [ "$expected" -eq 0 ] && [ -n "$CURRENT_PORTS" ]; then
        sleep 0.5
        CURRENT_PORTS="$(find_all_t114_transport_ports)"
        break
    fi
    sleep 0.5
    boot_tick=$((boot_tick + 1))
done

# Per-port lookup: print serial + transport + matching debug port.
print_device_line() {
    local transport="$1"
    local props serial debug_port
    props="$(udevadm info -q property "$transport" 2>/dev/null || true)"
    serial="$(echo "$props" | grep '^ID_SERIAL_SHORT=' | cut -d= -f2)"
    # Find matching debug port (interface 00, same serial).
    local p p_props p_iface p_serial
    debug_port=""
    for p in /dev/ttyACM*; do
        [ -c "$p" ] || continue
        p_props="$(udevadm info -q property "$p" 2>/dev/null || true)"
        p_iface="$( echo "$p_props" | grep '^ID_USB_INTERFACE_NUM=' | cut -d= -f2)"
        p_serial="$(echo "$p_props" | grep '^ID_SERIAL_SHORT='      | cut -d= -f2)"
        if [ "$p_iface" = "00" ] && [ "$p_serial" = "$serial" ]; then
            debug_port="$p"
            break
        fi
    done
    if [ -n "$debug_port" ]; then
        printf "      serial=%s  transport=%s  debug=%s\n" "$serial" "$transport" "$debug_port"
    else
        printf "      serial=%s  transport=%s  debug=(not found)\n" "$serial" "$transport"
    fi
}

# Same line, but keyed on the identity the read-back established rather than
# on a device path. The serial is what the board answered with; the paths are
# looked up from it, never the other way round.
# Args: $1 = serial, $2 = stamp it reported
print_confirmed_line() {
    local serial="$1" stamp="$2"
    local p props p_iface p_serial transport="" debug_port=""
    for p in /dev/ttyACM*; do
        [ -c "$p" ] || continue
        props="$(udevadm info -q property "$p" 2>/dev/null || true)"
        p_serial="$(echo "$props" | grep '^ID_SERIAL_SHORT=' | cut -d= -f2)"
        [ "$p_serial" = "$serial" ] || continue
        p_iface="$(echo "$props" | grep '^ID_USB_INTERFACE_NUM=' | cut -d= -f2)"
        case "$p_iface" in
        00) debug_port="$p" ;;
        02) transport="$p" ;;
        esac
    done
    printf "      serial=%s  transport=%s  debug=%s  %s\n" \
        "$serial" "${transport:-(not found)}" "${debug_port:-(not found)}" "$stamp"
}

# Classify flashed devices as booted vs flashed-but-not-booted, BY SERIAL.
# After flash a device may renumber to a different /dev/ttyACM*; the
# authoritative identity is its USB serial number. A flashed serial is
# "booted" iff any current transport port has that serial.
#
# Build a quick lookup of currently-visible serials.
CURRENT_SERIALS=""
if [ -n "$CURRENT_PORTS" ]; then
    while IFS= read -r p; do
        [ -n "$p" ] || continue
        s="$(udevadm info -q property "$p" 2>/dev/null | grep '^ID_SERIAL_SHORT=' | cut -d= -f2 || true)"
        [ -n "$s" ] && CURRENT_SERIALS="$CURRENT_SERIALS"$'\n'"$s $p"
    done <<< "$CURRENT_PORTS"
fi

BOOTED_PORTS=""       # transport paths of devices that flashed AND came back
NOT_BOOTED_PORTS=""   # original paths of devices that flashed but didn't reappear

if [ -n "$FLASHED_PORTS" ]; then
    # Map FLASHED entries to serials in the same order as the loop ran.
    # FLASHED_SERIALS is parallel to FLASHED_PORTS for the normal flow; the
    # legacy "(unknown)" placeholder has no entry there.
    serial_lines=0
    [ -n "$FLASHED_SERIALS" ] && serial_lines=$(echo "$FLASHED_SERIALS" | wc -l)

    i=0
    while IFS= read -r fp; do
        [ -n "$fp" ] || continue
        i=$((i + 1))
        if [ "$fp" = "(unknown)" ]; then
            # Legacy fallback path — no serial captured. Best-effort: if any
            # port is currently visible, claim booted with the first one.
            if [ -n "$CURRENT_PORTS" ]; then
                first="$(echo "$CURRENT_PORTS" | head -1)"
                BOOTED_PORTS="$BOOTED_PORTS"$'\n'"$first"
            else
                NOT_BOOTED_PORTS="$NOT_BOOTED_PORTS"$'\n'"(unknown)"
            fi
            continue
        fi
        # Look up the serial captured BEFORE flash (parallel-array index).
        fp_serial=""
        if [ "$i" -le "$serial_lines" ]; then
            fp_serial="$(echo "$FLASHED_SERIALS" | sed -n "${i}p")"
        fi
        if [ -z "$fp_serial" ]; then
            # No serial recorded — fall back to path comparison.
            if echo "$CURRENT_PORTS" | grep -qx "$fp"; then
                BOOTED_PORTS="$BOOTED_PORTS"$'\n'"$fp"
            else
                NOT_BOOTED_PORTS="$NOT_BOOTED_PORTS"$'\n'"$fp"
            fi
            continue
        fi
        # Search current ports for this serial (regardless of which /dev/ttyACM*).
        # `|| true` covers grep-no-match, which is normal when a flashed
        # device hasn't re-enumerated yet.
        cur_path="$(echo "$CURRENT_SERIALS" | grep "^${fp_serial} " | awk '{print $2}' | head -1 || true)"
        if [ -n "$cur_path" ]; then
            BOOTED_PORTS="$BOOTED_PORTS"$'\n'"$cur_path"
        else
            NOT_BOOTED_PORTS="$NOT_BOOTED_PORTS"$'\n'"$fp (serial=$fp_serial)"
        fi
    done <<< "$FLASHED_PORTS"
fi

# Strip leading newlines.
BOOTED_PORTS="$(echo -n "$BOOTED_PORTS"         | sed '/^$/d')"
NOT_BOOTED_PORTS="$(echo -n "$NOT_BOOTED_PORTS" | sed '/^$/d')"

# The read-back verdicts, deduplicated: one board can be confirmed only once
# per run, and a serial reached twice is the same board both times.
FW_CONFIRMED_SERIALS="$(echo -n "$FW_CONFIRMED_SERIALS"   | sed '/^$/d' | awk '!seen[$0]++')"
FW_UNCONFIRMED_LINES="$(echo -n "$FW_UNCONFIRMED_LINES"   | sed '/^$/d')"
FW_UNATTRIBUTED_LINES="$(echo -n "$FW_UNATTRIBUTED_LINES" | sed '/^$/d')"

NUM_BOOTED=0
NUM_NOT_BOOTED=0
NUM_FAILED=0
NUM_CONFIRMED=0
NUM_UNCONFIRMED=0
NUM_UNATTRIBUTED=0
if [ -n "$BOOTED_PORTS"     ]; then NUM_BOOTED=$(    echo "$BOOTED_PORTS"     | wc -l); fi
if [ -n "$NOT_BOOTED_PORTS" ]; then NUM_NOT_BOOTED=$(echo "$NOT_BOOTED_PORTS" | wc -l); fi
if [ -n "$FAILED_PORTS"     ]; then NUM_FAILED=$(    echo "$FAILED_PORTS"     | wc -l); fi
if [ -n "$FW_CONFIRMED_SERIALS"  ]; then NUM_CONFIRMED=$(   echo "$FW_CONFIRMED_SERIALS"  | wc -l); fi
if [ -n "$FW_UNCONFIRMED_LINES"  ]; then NUM_UNCONFIRMED=$( echo "$FW_UNCONFIRMED_LINES"  | wc -l); fi
if [ -n "$FW_UNATTRIBUTED_LINES" ]; then NUM_UNATTRIBUTED=$(echo "$FW_UNATTRIBUTED_LINES" | wc -l); fi

echo ""
if [ -n "$BUILT_STAMP" ]; then
    echo "==> Flash summary (image $BUILT_STAMP):"
else
    echo "==> Flash summary:"
fi

if [ -n "$BUILT_STAMP" ]; then
    # The authoritative bucket, and the one #343 was about. A board is listed
    # here because it said so on its own debug port — not because a board of
    # this type re-enumerated while a volume that carries no serial was being
    # written. The two are the same thing only when exactly one board of the
    # type is attached, which is precisely the case the rig is not.
    echo "    carrying this image, read back from the board ($NUM_CONFIRMED):"
    if [ "$NUM_CONFIRMED" -gt 0 ]; then
        while IFS= read -r s; do
            [ -n "$s" ] || continue
            print_confirmed_line "$s" "$BUILT_STAMP"
        done <<< "$FW_CONFIRMED_SERIALS"
    else
        printf "      (none — no attached board was shown to be running it)\n"
    fi

    if [ "$NUM_UNCONFIRMED" -gt 0 ]; then
        echo "    written but not confirmed ($NUM_UNCONFIRMED):"
        while IFS= read -r line; do
            [ -n "$line" ] || continue
            printf "      %s\n" "$line"
        done <<< "$FW_UNCONFIRMED_LINES"
    fi

    if [ "$NUM_UNATTRIBUTED" -gt 0 ]; then
        echo "    written, and the runner does not know which board received it ($NUM_UNATTRIBUTED):"
        while IFS= read -r line; do
            [ -n "$line" ] || continue
            printf "      %s\n" "$line"
        done <<< "$FW_UNATTRIBUTED_LINES"
    fi
elif [ "$NUM_BOOTED" -gt 0 ]; then
    # No stamp in the image, so there is nothing to read back and the only
    # available statement is the weak one: a board of this type came back.
    echo "    flashed & booted, NOT read back ($NUM_BOOTED):"
    while IFS= read -r p; do
        [ -n "$p" ] || continue
        print_device_line "$p"
    done <<< "$BOOTED_PORTS"
fi

if [ "$NUM_NOT_BOOTED" -gt 0 ]; then
    echo "    flashed but not booted ($NUM_NOT_BOOTED):"
    while IFS= read -r p; do
        [ -n "$p" ] || continue
        printf "      %s   — UF2 copied but device did not re-enumerate within 10s\n" "$p"
        printf "                       (firmware may be crashed; double-tap RESET to re-flash)\n"
    done <<< "$NOT_BOOTED_PORTS"
fi

if [ "$NUM_FAILED" -gt 0 ]; then
    echo "    failed to flash ($NUM_FAILED):"
    while IFS= read -r p; do
        [ -n "$p" ] || continue
        printf "      %s   — UF2 not copied (see error line above)\n" "$p"
    done <<< "$FAILED_PORTS"
fi

# --- Step 8: Update target/debug-port for tooling ---------------------------
# Tools that consume target/debug-port (e.g. log readers) assume one port.
# Write the first CONFIRMED device's debug path — the board that answered with
# this image, not merely the first that re-enumerated; a log reader pointed at
# the wrong board is the same defect one layer on. Falls back to the booted
# set only when there is no stamp to confirm against. LEVICULUM_FLASH_ONLY
# pins a specific device.
first_serial=""
if [ -n "$FW_CONFIRMED_SERIALS" ]; then
    first_serial="$(echo "$FW_CONFIRMED_SERIALS" | head -1)"
elif [ -z "$BUILT_STAMP" ] && [ -n "$BOOTED_PORTS" ]; then
    first_transport="$(echo "$BOOTED_PORTS" | head -1)"
    first_props="$(udevadm info -q property "$first_transport" 2>/dev/null || true)"
    first_serial="$(echo "$first_props" | grep '^ID_SERIAL_SHORT=' | cut -d= -f2)"
fi
if [ -n "$first_serial" ]; then
    first_debug=""
    for p in /dev/ttyACM*; do
        [ -c "$p" ] || continue
        p_props="$(udevadm info -q property "$p" 2>/dev/null || true)"
        p_iface="$( echo "$p_props" | grep '^ID_USB_INTERFACE_NUM=' | cut -d= -f2)"
        p_serial="$(echo "$p_props" | grep '^ID_SERIAL_SHORT='      | cut -d= -f2)"
        if [ "$p_iface" = "00" ] && [ "$p_serial" = "$first_serial" ]; then
            first_debug="$p"
            break
        fi
    done
    # Prefer udev symlink if present (stable across reboots). The unqualified
    # /dev/leviculum-debug lands on whichever board udev saw first, so it is
    # only safe when this run touched exactly one board.
    if [ -L "/dev/leviculum-debug-$first_serial" ]; then
        first_debug="/dev/leviculum-debug-$first_serial"
    elif [ -L "/dev/leviculum-debug" ] &&
        { { [ -n "$BUILT_STAMP" ] && [ "$NUM_CONFIRMED" -eq 1 ]; } ||
            { [ -z "$BUILT_STAMP" ] && [ "$NUM_BOOTED" -eq 1 ]; }; }; then
        first_debug="/dev/leviculum-debug"
    fi
    if [ -n "$first_debug" ]; then
        mkdir -p "$TARGET_DIR"
        echo "$first_debug" > "$TARGET_DIR/debug-port"
    fi
fi

# --- Step 9: Cleanup --------------------------------------------------------

rm -f "$BIN_FILE"

# Exit code: non-zero if any targeted device ended up "failed to flash".
# "Flashed but not booted" still counts as exit 0 — the bits made it onto
# the device; if the firmware crashes that's a build problem, not a
# tooling problem.
if [ "$NUM_FAILED" -gt 0 ]; then
    echo ""
    echo "==> Exit 1: $NUM_FAILED of $((NUM_FLASHED + NUM_FAILED)) $BOARD_NAME(s) did not get flashed."
    exit 1
fi

# An image that went somewhere unknown is not a success. The caller's whole
# reason for flashing is to know what a board is running afterwards, and a run
# that cannot say which board it wrote has not delivered that — on CI it has
# to go red rather than let the next scenario attribute results to a firmware
# nobody located. A board that could not be READ (mute debug port) is NOT this
# case and stays exit 0: that is a board failing to answer, not a write
# landing somewhere unaccounted for.
if [ "$NUM_UNATTRIBUTED" -gt 0 ]; then
    echo ""
    echo "==> Exit 1: $NUM_UNATTRIBUTED write(s) could not be bound to a $BOARD_NAME."
    exit 1
fi

echo ""
echo "==> Done!"
exit 0
