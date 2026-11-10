#!/usr/bin/env bash
#
# Read an LNode's bootloader, and lift a RAK4631's to a new build (order 1609,
# Codeberg #235). The procedure and every number it checks are in
# docs/src/concepts/ota-stage-1-ble-dfu.md, "Bootloader update".
#
#   bash scripts/lnode-bootloader.sh info <port|->
#   bash scripts/lnode-bootloader.sh update-rak <uf2> <port|->
#
# <port> is any CDC port of the board running our firmware (the 1200-baud
# touch works on either); "-" means nobody can touch it and a person
# double-taps reset when asked. `info` prints INFO_UF2.TXT and leaves the
# board in its bootloader: the board's own flash recipe (or a reset, where
# there is a button) brings the application back, and on a Pocket V2 a power
# cycle does not, the battery keeps it up.
#
# `update-rak`, in order, stopping at the first thing that is not as expected:
#   1. lnflash's planner reads the UF2 against leviculum-nrf/memory.x and
#      refuses one that would stage into the SoftDevice, the application, the
#      boot record, the calendar ring or the identity/radio/telemetry band
#      (lnflash/examples/bootloader_plan.rs). It names the record-log pages
#      the staging overwrites; those records are lost, and the run asks for
#      that to be accepted (LEVICULUM_BL_ACCEPT_STORE_LOSS=1 says yes ahead).
#   2. With a port: the board's [IDENTITY] line, read off its debug port.
#   3. Into the current bootloader; INFO must say WisBlock-RAK4631-Board and
#      S140 7.3.0, or nothing is written.
#   4. The UF2 is copied onto the volume mounted `-o flush`, no `sync`: the
#      bootloader resets under the last block, and a global sync would block
#      on a device that is gone.
#   5. Back into the (new) bootloader; INFO must name the planner's version
#      and the same SoftDevice.
#   6. Our application, through the same runner `just flash-rak4631-one`
#      uses, which proves [FW_BUILD]; then [IDENTITY] must be unchanged.
#
# Needs sudo -n for mount/umount/cp onto the bootloader volume, as the runner.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOOLS="$ROOT/leviculum-nrf/tools"

# The application's USB ID, for the debug-port lookup in fw-readback.sh. The
# RAK4631 recipes export LEVICULUM_USB_PID=0002; `info` on a T114 leaves the
# default, which is the T114's.
BOARD_VID="${LEVICULUM_USB_VID:-1209}"
BOARD_PID="${LEVICULUM_USB_PID:-0001}"
BOOTLOADER_BOARD_ID="${LEVICULUM_UF2_BOARD_ID:-}"
export BOARD_VID BOARD_PID BOOTLOADER_BOARD_ID

# shellcheck source=leviculum-nrf/tools/uf2-volumes.sh
source "$TOOLS/uf2-volumes.sh"
# shellcheck source=leviculum-nrf/tools/fw-readback.sh
source "$TOOLS/fw-readback.sh"

RAK_BOARD_ID="WisBlock-RAK4631-Board"
# The SoftDevice our RAK4631 images link against (leviculum-nrf/memory.x).
WANT_SOFTDEVICE="S140 7.3.0"
# USER_FLASH_END of the bootloader our flash map is laid out against
# (leviculum-nrf/memory.x: the band above it survives every UF2).
WANT_WINDOW_END="0xEA000"
MOUNT_ROOT="${LEVICULUM_UF2_MOUNT_ROOT:-/run/leviculum-uf2}"
MOUNTED=""

say() { printf '%s\n' "$*"; }
die() {
    printf 'lnode-bootloader: %s\n' "$*" >&2
    exit 1
}

cleanup() {
    if [ -n "$MOUNTED" ]; then
        sudo -n umount "$MOUNTED" 2>/dev/null || sudo -n umount -l "$MOUNTED" 2>/dev/null || true
        sudo -n rmdir "$MOUNTED" 2>/dev/null || true
        MOUNTED=""
    fi
}
trap cleanup EXIT

# Small block devices right now, one per line (uf2-volumes.sh's notion of a
# candidate bootloader volume).
bl_devices() { uf2_block_devices | sort; }

# Wait up to $2 s for exactly one device not in the list $1, print it.
bl_wait_new_device() {
    local before="$1" secs="$2" deadline now new
    deadline=$(($(date +%s) + secs))
    while :; do
        new="$(comm -13 <(printf '%s\n' "$before" | sed '/^$/d') <(bl_devices))"
        if [ -n "$new" ]; then
            [ "$(printf '%s\n' "$new" | wc -l)" -eq 1 ] ||
                die "more than one bootloader volume appeared: $(printf "%s " "$new" | tr "\n" " ")"
            sleep 1
            printf '%s\n' "$new"
            return 0
        fi
        now=$(date +%s)
        [ "$now" -lt "$deadline" ] || return 1
        sleep 0.5
    done
}

# Wait up to $2 s for device $1 to go away.
bl_wait_gone() {
    local dev="$1" secs="$2" i
    for ((i = 0; i < secs * 2; i++)); do
        [ -b "$dev" ] || return 0
        sleep 0.5
    done
    return 1
}

# Mount $1 at our own mount point with options $2, set MOUNTED.
bl_mount() {
    local dev="$1" opts="$2" mp
    mp="$MOUNT_ROOT/bl-$(basename "$dev")"
    sudo -n mkdir -p "$mp" || die "sudo -n mkdir $mp failed"
    sudo -n mount -o "$opts" "$dev" "$mp" || die "sudo -n mount -o $opts $dev failed"
    MOUNTED="$mp"
}

# INFO_UF2.TXT of device $1, CR stripped. Mounted read-only and given back.
bl_read_info() {
    local dev="$1" text
    bl_mount "$dev" ro
    [ -f "$MOUNTED/INFO_UF2.TXT" ] || {
        cleanup
        die "$dev carries no INFO_UF2.TXT: not a UF2 bootloader volume"
    }
    text="$(tr -d '\r' <"$MOUNTED/INFO_UF2.TXT")"
    # CURRENT.UF2 is generated from exactly the window the bootloader writes
    # applications to, 256 bytes per 512-byte block from USER_FLASH_START
    # (docs/src/concepts/lnode-flashing.md, "What a UF2 is allowed to write"),
    # so its size is that window's top, read without a constant.
    if [ -f "$MOUNTED/CURRENT.UF2" ]; then
        text="$text"$'\n'"Window-End: $(printf '0x%X' $((0x1000 + $(stat -c %s "$MOUNTED/CURRENT.UF2") * 256 / 512)))"
    fi
    cleanup
    printf '%s\n' "$text"
}

# Field $2 of INFO text $1 ("Board-ID", "SoftDevice", ...).
bl_field() { printf '%s\n' "$1" | sed -n "s/^$2:[[:space:]]*//p" | head -n1; }
# The version word of the first line, "UF2 Bootloader <version> ...".
bl_version() { printf '%s\n' "$1" | sed -n '1s/^UF2 Bootloader \([^ ]*\).*/\1/p'; }

bl_print_info() {
    local info="$1"
    printf '%s\n' "$info" | sed 's/^/  /'
    say "BL_INFO version=$(bl_version "$info") board_id=$(bl_field "$info" Board-ID)" \
        "softdevice=\"$(bl_field "$info" SoftDevice)\" date=\"$(bl_field "$info" Date)\"" \
        "window_end=$(bl_field "$info" Window-End)"
}

bl_serial() { udevadm info -q property "$1" 2>/dev/null | sed -n 's/^ID_SERIAL_SHORT=//p' || true; }

# The identity hash a board prints in [IDENTITY], read off its debug port.
# Args: $1 = USB serial. Three 8 s windows, as the runner's read-back.
bl_identity() {
    local serial="$1" port log id
    log="$(mktemp)"
    for _ in 1 2 3; do
        port="$(fw_debug_port_for_serial "$serial")"
        if [ -n "$port" ]; then
            fw_read_banner "$port" 8 "$log" >/dev/null || true
            id="$(grep -o '\[IDENTITY\] identity=[0-9a-f]*' "$log" | tail -n1 | sed 's/.*identity=//' || true)"
            if [ -n "$id" ]; then
                rm -f "$log"
                printf '%s\n' "$id"
                return 0
            fi
        fi
        sleep 2
    done
    rm -f "$log"
    return 1
}

# Bring the board into its bootloader and print the volume's device.
# Args: $1 = port or "-", $2 = what to tell a person who has to double-tap
bl_enter() {
    local port="$1" why="$2" before dev
    before="$(bl_devices)"
    if [ "$port" = "-" ]; then
        say ">>> $why: double-tap reset now (a Pocket V2: the pinhole beside the USB socket, docs/src/firmware/recovery.md)" >&2
        dev="$(bl_wait_new_device "$before" 120)" || die "no bootloader volume appeared within 120 s"
    else
        [ -e "$port" ] || die "$port does not exist"
        stty -F "$port" 1200 2>/dev/null || true
        dev="$(bl_wait_new_device "$before" 30)" ||
            die "no bootloader volume within 30 s of the 1200-baud touch on $port"
    fi
    printf '%s\n' "$dev"
}

cmd_info() {
    local port="${1:?usage: info <port|->}" dev info
    dev="$(bl_enter "$port" "reading the bootloader")"
    info="$(bl_read_info "$dev")"
    say "==> INFO_UF2.TXT of $dev"
    bl_print_info "$info"
    say "==> the board is in its bootloader. Back to the application: its flash recipe" \
        "(just flash-rak4631-one / flash-one with the port), or reset where there is a button."
}

cmd_update_rak() {
    local uf2="${1:?usage: update-rak <uf2> <port|->}" port="${2:?usage: update-rak <uf2> <port|->}"
    local plan target serial="" id_before="" dev info old new boot_log id_after flash_only

    [ -f "$uf2" ] || die "$uf2: no such file"
    say "==> plan for $uf2 (sha256 $(sha256sum "$uf2" | cut -d' ' -f1))"
    plan="$(cd "$ROOT" && cargo run -q -p lnflash --example bootloader_plan -- "$uf2" leviculum-nrf/memory.x)" || {
        printf '%s\n' "$plan"
        die "the planner refused $uf2; nothing was written"
    }
    printf '%s\n' "$plan"
    target="$(printf '%s\n' "$plan" | sed -n 's/^BL_PLAN version=\([^ ]*\).*/\1/p')"
    if [ -z "$target" ] || [ "$target" = unknown ]; then
        die "the image names no bootloader version"
    fi
    if grep -q '^BL_PLAN store_pages_lost=.' <<<"$plan"; then
        if [ "${LEVICULUM_BL_ACCEPT_STORE_LOSS:-0}" != 1 ]; then
            say ">>> the staging overwrites the record-log pages above; records on them are lost."
            local answer=""
            read -r -p ">>> type yes to accept: " answer || answer=""
            [ "$answer" = yes ] || die "not accepted; nothing was written"
        fi
    fi

    if [ "$port" != "-" ]; then
        serial="$(bl_serial "$port")"
        [ -n "$serial" ] || die "$port has no USB serial"
        id_before="$(bl_identity "$serial")" || die "no [IDENTITY] line from serial $serial; nothing was written"
        say "BL_UPDATE identity_before=$id_before serial=$serial"
    else
        say "BL_UPDATE identity_before=unread (no port): identity kept will be UNVERIFIED"
    fi

    dev="$(bl_enter "$port" "entering the current bootloader")"
    info="$(bl_read_info "$dev")"
    bl_print_info "$info"
    old="$(bl_version "$info")"
    [ "$(bl_field "$info" Board-ID)" = "$RAK_BOARD_ID" ] || die "Board-ID is not $RAK_BOARD_ID; nothing was written"
    [ "$(bl_field "$info" SoftDevice)" = "$WANT_SOFTDEVICE" ] ||
        die "SoftDevice is not $WANT_SOFTDEVICE; nothing was written"
    # The planner modelled the receiver as RAK 0.4.3 (lnflash
    # bootloader_update::RAK4631_0_4_3); its staging offset follows from
    # this window top, so a different one means a different set of pages.
    [ "$(bl_field "$info" Window-End)" = "$WANT_WINDOW_END" ] ||
        die "the current bootloader writes up to $(bl_field "$info" Window-End), the planner assumed" \
            "$WANT_WINDOW_END; nothing was written"

    if [ "$old" = "$target" ]; then
        say "BL_UPDATE skipped: already $target"
    else
        say "==> copying $uf2 onto $dev (bootloader $old -> $target)"
        bl_mount "$dev" flush
        sudo -n cp "$uf2" "$MOUNTED/NEW.UF2" || say "    cp returned non-zero; normal if the bootloader reset under it"
        cleanup
        bl_wait_gone "$dev" 30 || die "$dev is still there 30 s after the copy: the bootloader did not take it"

        # The new bootloader starts the application (it is still valid) or
        # stays in its own drive; either way, enter it once more and read.
        if [ "$port" != "-" ]; then
            local tries=0 cdc=""
            while [ "$tries" -lt 60 ] && [ -z "$cdc" ]; do
                cdc="$(fw_debug_port_for_serial "$serial")"
                [ -n "$cdc" ] && break
                if new="$(bl_wait_new_device "$(bl_devices | grep -vx "$dev" || true)" 1)"; then
                    dev="$new"
                    break
                fi
                tries=$((tries + 1))
            done
            if [ -n "$cdc" ]; then
                sleep 2
                dev="$(bl_enter "$cdc" "reading the new bootloader")"
            elif [ ! -b "$dev" ]; then
                die "neither the application nor a bootloader volume came back within 60 s"
            fi
        else
            dev="$(bl_enter - "reading the new bootloader")"
        fi
        info="$(bl_read_info "$dev")"
        bl_print_info "$info"
        new="$(bl_version "$info")"
        [ "$new" = "$target" ] || die "the bootloader reports $new, expected $target"
        [ "$(bl_field "$info" SoftDevice)" = "$WANT_SOFTDEVICE" ] ||
            die "SoftDevice after the update is not $WANT_SOFTDEVICE"
        # The top of the window our images and persistence pages are laid out
        # against (leviculum-nrf/memory.x). A new bootloader that moved it
        # needs a person's look before anything else is written.
        [ "$(bl_field "$info" Window-End)" = "$WANT_WINDOW_END" ] ||
            die "the new bootloader writes up to $(bl_field "$info" Window-End), not $WANT_WINDOW_END;" \
                "the board is in its bootloader and our application is still in flash"
        say "BL_UPDATE before=$old after=$new softdevice=\"$WANT_SOFTDEVICE\" window_end=$WANT_WINDOW_END"
    fi

    # The board is in its bootloader with the volume present; the runner's
    # flash_one_device finds that volume before it touches anything. Without a
    # port, LEVICULUM_FLASH_ONLY names no device on purpose, so the runner
    # does not go looking for every other RAK4631 on the bus.
    flash_only="$port"
    [ "$port" = "-" ] && flash_only="/dev/leviculum-bootloader-update"
    boot_log="$(mktemp)"
    say "==> flashing our application"
    LEVICULUM_FLASH_ONLY="$flash_only" LEVICULUM_FLASH_BOOT_LOG="$boot_log" \
        bash "$ROOT/scripts/nrf-flash.sh" rak4631 bsp-rak4631,rak-baseboard ||
        die "the application flash failed; the bootloader is $target, the board is in it"

    if [ -n "$id_before" ]; then
        id_after="$(grep -o '\[IDENTITY\] identity=[0-9a-f]*' "$boot_log" | tail -n1 | sed 's/.*identity=//' || true)"
        [ -n "$id_after" ] || id_after="$(bl_identity "$serial" || true)"
        rm -f "$boot_log"
        [ "$id_after" = "$id_before" ] ||
            die "IDENTITY CHANGED: before=$id_before after=${id_after:-unread}"
        say "BL_UPDATE identity_kept=yes identity=$id_after"
    else
        rm -f "$boot_log"
        say "BL_UPDATE identity_kept=unverified (no port)"
    fi
}

case "${1:-}" in
info)
    shift
    cmd_info "$@"
    ;;
update-rak)
    shift
    cmd_update_rak "$@"
    ;;
*)
    die "usage: $0 info <port|-> | update-rak <uf2> <port|->"
    ;;
esac
