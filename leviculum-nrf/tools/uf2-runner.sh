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

set -euo pipefail

ELF="${1:?Usage: uf2-runner.sh <ELF>}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
NRF_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
TARGET_DIR="$NRF_DIR/target"
BIN_FILE="$ELF.bin"
UF2_FILE="$ELF.uf2"
UF2_TIMEOUT="${UF2_TIMEOUT:-30}"

FLASH_BASE=0x27000
FAMILY_ID=0xADA52840

# Per-board parameters (default to T114 values for backward compatibility).
BOARD_VID="${LEVICULUM_USB_VID:-1209}"
BOARD_PID="${LEVICULUM_USB_PID:-0001}"
BOARD_NAME="${LEVICULUM_BOARD_NAME:-T114}"
BOOTLOADER_BOARD_ID="${LEVICULUM_UF2_BOARD_ID:-HT-n5262}"

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

# --- Helper: find UF2 drive (polled by flash_one_uf2) -----------------------

find_uf2_drive() {
    local search_dirs=()
    # Standard Linux automount locations
    if [ -d "/media/$USER" ]; then
        search_dirs+=("/media/$USER")
    fi
    if [ -d "/run/media/$USER" ]; then
        search_dirs+=("/run/media/$USER")
    fi
    if [ -d "/mnt" ]; then
        search_dirs+=("/mnt")
    fi

    for dir in "${search_dirs[@]}"; do
        local info
        info="$(find "$dir" -maxdepth 2 -name INFO_UF2.TXT -type f 2>/dev/null | head -1)"
        if [ -n "$info" ]; then
            dirname "$info"
            return
        fi
    done

    # Try mounting unmounted small removable block devices
    for dev in /dev/sd?; do
        [ -b "$dev" ] || continue
        # Only consider small devices (< 64MB, typical for UF2 bootloaders)
        local size
        size="$(cat "/sys/block/$(basename "$dev")/size" 2>/dev/null || echo 0)"
        # size is in 512-byte sectors; 64MB = 131072 sectors
        if [ "$size" -gt 0 ] && [ "$size" -lt 131072 ]; then
            local part="${dev}1"
            [ -b "$part" ] || part="$dev"

            # Try udisksctl first (user-level, no sudo)
            if command -v udisksctl >/dev/null 2>&1; then
                local mount_output
                mount_output="$(udisksctl mount -b "$part" 2>/dev/null || true)"
                if [ -n "$mount_output" ]; then
                    local mount_point
                    mount_point="$(echo "$mount_output" | grep -oP 'at \K/.*' || true)"
                    if [ -n "$mount_point" ] && [ -f "$mount_point/INFO_UF2.TXT" ]; then
                        echo "$mount_point"
                        return
                    fi
                fi
            fi

            # Fallback: sudo mount to /mnt
            if sudo -n mount "$part" /mnt 2>/dev/null; then
                if [ -f /mnt/INFO_UF2.TXT ]; then
                    echo "/mnt"
                    return
                fi
                sudo -n umount /mnt 2>/dev/null
            fi
        fi
    done

    echo ""
}

# Validate that the given UF2 drive belongs to the configured board.
# Without this check `flash_one_uf2` would clobber whatever bootloader
# happens to be mounted — if a T114 sits in UF2 mode while we are flashing
# a RAK4631, the wrong UF2 lands on the wrong silicon.
uf2_drive_matches_board() {
    local d="$1"
    [ -n "$d" ] || return 1
    [ -f "$d/INFO_UF2.TXT" ] || return 1
    grep -q "$BOOTLOADER_BOARD_ID" "$d/INFO_UF2.TXT" 2>/dev/null
}

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

# --- Helper: flash one UF2 to whichever bootloader drive appears ------------
# Args: $1 = hint string (port path or "(unknown device)") for log/prompt
# Returns:
#   0 = UF2 successfully copied to the drive
#   1 = UF2 drive never appeared within UF2_TIMEOUT (manual prompt was
#       displayed but ignored, or no T114 in bootloader mode)
#   2 = drive appeared but cp failed (write-protect, FS error, mid-flight unplug)
# The helper prints a precise per-failure diagnostic line BEFORE returning
# non-zero, so the caller's summary need only list the port without
# re-explaining the cause.

flash_one_uf2() {
    local hint="${1:-(unknown device)}"
    local drive
    # Track which mismatched drives we have already warned about so the log
    # does not flood while we keep polling for the right one.
    local warned_about=""

    # Inline helper: poll once, returning a drive only if it matches the
    # configured Board-ID. Mismatched drives (e.g. a T114 bootloader while we
    # are flashing a RAK4631) are skipped with a one-shot warning.
    poll_one() {
        local d
        d="$(find_uf2_drive)"
        if [ -n "$d" ] && ! uf2_drive_matches_board "$d"; then
            case "$warned_about" in
                *"|$d|"*) ;;
                *)
                    local seen_id="(missing)"
                    if [ -f "$d/INFO_UF2.TXT" ]; then
                        seen_id="$(grep -m1 -oE 'Board-ID: [^[:space:]]+' "$d/INFO_UF2.TXT" 2>/dev/null | cut -d' ' -f2)"
                        [ -z "$seen_id" ] && seen_id="(unknown)"
                    fi
                    echo "[uf2-runner] $hint: ignoring UF2 drive at $d — Board-ID '$seen_id' does not match expected '$BOOTLOADER_BOARD_ID'" >&2
                    warned_about="$warned_about|$d|"
                    ;;
            esac
            d=""
        fi
        echo "$d"
    }

    # Quick silent grace period: if 1200-baud-touch worked the firmware will
    # have rebooted into the UF2 bootloader within ~1-2 s. Polling silently
    # for QUIET_GRACE seconds before showing the manual-tap prompt avoids
    # misleading the user into thinking they need to tap when touch already
    # succeeded.
    local QUIET_GRACE=4
    local quiet_ticks=$((QUIET_GRACE * 2))   # 0.5s per tick
    drive="$(poll_one)"
    local tick=0
    while [ -z "$drive" ] && [ "$tick" -lt "$quiet_ticks" ]; do
        sleep 0.5
        tick=$((tick + 1))
        drive="$(poll_one)"
    done

    if [ -z "$drive" ]; then
        # Touch didn't bring up a drive in the grace window — show the prompt
        # and continue polling for the remaining UF2_TIMEOUT.
        echo "==> $hint: looking for UF2 drive..."
        echo "    ┌──────────────────────────────────────────────────┐"
        printf  "    │  Double-tap RESET on %-6s to enter bootloader. │\n" "$BOARD_NAME"
        echo "    └──────────────────────────────────────────────────┘"
        echo "==> Waiting for UF2 drive (${UF2_TIMEOUT}s)..."

        local remaining_ticks=$(( (UF2_TIMEOUT - QUIET_GRACE) * 2 ))
        [ "$remaining_ticks" -lt 0 ] && remaining_ticks=0
        tick=0
        while [ "$tick" -lt "$remaining_ticks" ]; do
            sleep 0.5
            tick=$((tick + 1))
            drive="$(poll_one)"
            if [ -n "$drive" ]; then
                break
            fi
        done

        if [ -z "$drive" ]; then
            echo "[uf2-runner] $hint: UF2 drive never appeared (timeout after ${UF2_TIMEOUT}s manual prompt)" >&2
            return 1
        fi
    fi

    local drive_name
    drive_name="$(basename "$drive")"
    echo "==> $hint: found UF2 drive at $drive ($drive_name)"

    # UF2 bootloaders intercept FAT filesystem writes and check each 512-byte
    # sector for UF2 magic. Write the file via cp (same approach as uf2conv.py
    # and the Heltec Arduino toolchain). sync may return I/O errors because the
    # bootloader resets after processing the last UF2 block — this is normal
    # and expected.
    local uf2_size uf2_blocks
    uf2_size="$(stat -c%s "$UF2_FILE")"
    uf2_blocks=$((uf2_size / 512))
    echo "==> $hint: deploying firmware.uf2 (${uf2_size} bytes, ${uf2_blocks} blocks)"

    if [ -w "$drive" ]; then
        if ! cp "$UF2_FILE" "$drive/NEW.UF2" 2>/dev/null; then
            echo "[uf2-runner] $hint: UF2 drive mounted at $drive but cp failed" >&2
            # Best-effort cleanup if we mounted to /mnt
            [ "$drive" = "/mnt" ] && sudo -n umount /mnt 2>/dev/null || true
            return 2
        fi
        sync 2>/dev/null || true
    else
        if ! sudo cp "$UF2_FILE" "$drive/NEW.UF2" 2>/dev/null; then
            echo "[uf2-runner] $hint: UF2 drive mounted at $drive but sudo cp failed" >&2
            [ "$drive" = "/mnt" ] && sudo -n umount /mnt 2>/dev/null || true
            return 2
        fi
        sudo sync 2>/dev/null || true
    fi

    # Best-effort cleanup if we mounted to /mnt
    if [ "$drive" = "/mnt" ]; then
        sudo -n umount /mnt 2>/dev/null || true
    fi

    return 0
}

# --- Step 5: Determine target list ------------------------------------------

if [ -n "${LEVICULUM_FLASH_ONLY:-}" ]; then
    PORTS="$LEVICULUM_FLASH_ONLY"
    echo "==> LEVICULUM_FLASH_ONLY set; targeting only $PORTS"
else
    PORTS="$(find_all_t114_transport_ports)"
fi

# Newline-separated lists of ports that flashed successfully / failed.
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
    if flash_one_uf2 "(unknown device)"; then
        FLASHED_PORTS="(unknown)"
    else
        FAILED_PORTS="(unknown)"
    fi
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
        # 1200-baud-touch. Old firmware ignores; new firmware writes the
        # GPREGRET magic and resets into the UF2 bootloader. stty errors are
        # tolerated (port may have already disappeared mid-loop).
        stty -F "$PORT" 1200 2>/dev/null || true
        if flash_one_uf2 "$PORT"; then
            FLASHED_PORTS="$FLASHED_PORTS"$'\n'"$PORT"
            [ -n "$PORT_SERIAL" ] && FLASHED_SERIALS="$FLASHED_SERIALS"$'\n'"$PORT_SERIAL"
        else
            echo "[uf2-runner] ($INDEX/$NUM) FAILED — continuing with next $BOARD_NAME" >&2
            FAILED_PORTS="$FAILED_PORTS"$'\n'"$PORT"
        fi
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
while true; do
    EXTRA_DRIVE="$(find_uf2_drive)"
    [ -n "$EXTRA_DRIVE" ] || break
    if [ ! -f "$EXTRA_DRIVE/INFO_UF2.TXT" ] || ! grep -q "$BOOTLOADER_BOARD_ID" "$EXTRA_DRIVE/INFO_UF2.TXT" 2>/dev/null; then
        break
    fi
    echo ""
    echo "==> Extra UF2 drive at $EXTRA_DRIVE — flashing crashed-firmware $BOARD_NAME (no transport port)"
    if [ -w "$EXTRA_DRIVE" ]; then
        if cp "$UF2_FILE" "$EXTRA_DRIVE/NEW.UF2" 2>/dev/null; then
            sync 2>/dev/null || true
            FLASHED_PORTS="$FLASHED_PORTS"$'\n'"(crashed-recovery)"
        else
            echo "[uf2-runner] (crashed-recovery): cp to $EXTRA_DRIVE failed" >&2
            FAILED_PORTS="$FAILED_PORTS"$'\n'"(crashed-recovery)"
        fi
    else
        if sudo cp "$UF2_FILE" "$EXTRA_DRIVE/NEW.UF2" 2>/dev/null; then
            sudo sync 2>/dev/null || true
            FLASHED_PORTS="$FLASHED_PORTS"$'\n'"(crashed-recovery)"
        else
            echo "[uf2-runner] (crashed-recovery): sudo cp to $EXTRA_DRIVE failed" >&2
            FAILED_PORTS="$FAILED_PORTS"$'\n'"(crashed-recovery)"
        fi
    fi
    [ "$EXTRA_DRIVE" = "/mnt" ] && sudo -n umount /mnt 2>/dev/null || true
    # Wait for the bootloader to process the file and disappear before
    # checking for more drives. Without this the same drive could be picked
    # up twice in quick succession.
    sleep 1
    sleep 1
    sleep 1
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
    flashed_lines=$(echo "$FLASHED_PORTS" | wc -l)
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

NUM_BOOTED=0
NUM_NOT_BOOTED=0
NUM_FAILED=0
if [ -n "$BOOTED_PORTS"     ]; then NUM_BOOTED=$(    echo "$BOOTED_PORTS"     | wc -l); fi
if [ -n "$NOT_BOOTED_PORTS" ]; then NUM_NOT_BOOTED=$(echo "$NOT_BOOTED_PORTS" | wc -l); fi
if [ -n "$FAILED_PORTS"     ]; then NUM_FAILED=$(    echo "$FAILED_PORTS"     | wc -l); fi

echo ""
echo "==> Flash summary:"

if [ "$NUM_BOOTED" -gt 0 ]; then
    echo "    flashed & booted ($NUM_BOOTED):"
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
# Write the first booted device's debug path; LEVICULUM_FLASH_ONLY pins a
# specific device.
if [ -n "$BOOTED_PORTS" ]; then
    first_transport="$(echo "$BOOTED_PORTS" | head -1)"
    first_props="$(udevadm info -q property "$first_transport" 2>/dev/null || true)"
    first_serial="$(echo "$first_props" | grep '^ID_SERIAL_SHORT=' | cut -d= -f2)"
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
    # Prefer udev symlink if present (stable across reboots).
    if [ -L "/dev/leviculum-debug-$first_serial" ]; then
        first_debug="/dev/leviculum-debug-$first_serial"
    elif [ -L "/dev/leviculum-debug" ] && [ "$NUM_BOOTED" -eq 1 ]; then
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

echo ""
echo "==> Done!"
exit 0
