#!/usr/bin/env bash
# Fixture test for UF2 volume discovery and selection (tools/uf2-volumes.sh).
#
# Same apparatus as tools/test-softdevice-guard.sh: real INFO_UF2.TXT text read
# off the rig boards, written into directories that stand in for mounted
# bootloader volumes. What is added here is the layer below the guard — WHICH
# volume the runner is about to write to — so the mount and umount calls are
# driven through stubs and recorded, and no block device or sudo is needed.
#
# The scenario every case here descends from is Codeberg #341, measured on the
# rig 2026-08-23: a T114 parked in its bootloader with the volume mounted at
# /mnt, while a RAK4631 is flashed and its own volume sits unmounted on
# /dev/sdb. Three attempts, three refusals of the same foreign volume, and a
# give-up message naming a symptom that never happened.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=leviculum-nrf/tools/uf2-volumes.sh
. "$SCRIPT_DIR/uf2-volumes.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

PASS=0
FAIL=0

ok() {
    PASS=$((PASS + 1))
    printf 'ok    %s\n' "$1"
}
bad() {
    FAIL=$((FAIL + 1))
    printf 'FAIL  %s\n' "$1"
}

# Args: $1 = what, $2 = want, $3 = got
check_eq() {
    if [ "$2" = "$3" ]; then
        ok "$1"
    else
        bad "$1"
        printf '        want %q\n        got  %q\n' "$2" "$3"
    fi
}

# Args: $1 = what, $2 = required substring, $3 = got
check_contains() {
    if [[ "$3" == *"$2"* ]]; then
        ok "$1"
    else
        bad "$1"
        printf '        want substring %q\n        got  %q\n' "$2" "$3"
    fi
}

# --- Fixture volumes --------------------------------------------------------

# Read off the rig boards, CRLF included (the same text
# tools/test-softdevice-guard.sh uses).
T114_INFO=$'UF2 Bootloader 0.9.0-2-g836c8dc-dirty\r\nModel: HT-n5262\r\nBoard-ID: HT-n5262\r\nDate: Jul  9 2024\r\nSoftDevice: S140 7.3.0\r\n'
RAK_INFO=$'UF2 Bootloader 0.4.3\r\nModel: WisBlock RAK4631 Board\r\nBoard-ID: WisBlock-RAK4631-Board\r\nDate: May 20 2023\r\nVer: 0.4.3\r\nSoftDevice: S140 7.3.0\r\n'

# A directory that stands in for one mounted bootloader volume.
# Args: $1 = name, $2 = INFO_UF2.TXT text
volume() {
    local dir="$WORK/vol/$1"
    mkdir -p "$dir"
    printf '%s' "$2" >"$dir/INFO_UF2.TXT"
    printf '%s' "$dir"
}

# --- Stubs ------------------------------------------------------------------
# Everything that touches the machine is a seam. SEARCH_DIRS is what is already
# mounted; DEVMAP maps a fake block device to the fixture directory that
# "mounting" it makes visible. Every mount/umount/mkdir lands in $CALLS, which
# is what the unmount-obligation cases assert on.

CALLS="$WORK/calls"
SEARCH_DIRS=""
DEVMAP=""

uf2_search_dirs() {
    [ -n "$SEARCH_DIRS" ] && printf '%s' "$SEARCH_DIRS"
    return 0
}

uf2_block_devices() {
    local d _s
    [ -n "$DEVMAP" ] || return 0
    while IFS=$'\t' read -r d _s; do
        [ -n "$d" ] && printf '%s\n' "$d"
    done <<<"$DEVMAP"
    return 0
}

uf2_have_udisks() { return 1; }

uf2_mkdir() {
    printf 'mkdir %s\n' "$1" >>"$CALLS"
    mkdir -p "$1"
    return 0
}

uf2_mount() {
    local dev="$1" mp="$2" d s src=""
    printf 'mount %s %s\n' "$dev" "$mp" >>"$CALLS"
    while IFS=$'\t' read -r d s; do
        [ "$d" = "$dev" ] && src="$s"
    done <<<"$DEVMAP"
    [ -n "$src" ] || return 1
    mkdir -p "$mp"
    cp -a "$src/." "$mp/" 2>/dev/null || true
    return 0
}

uf2_umount() {
    printf 'umount %s\n' "$1" >>"$CALLS"
    find "$1" -mindepth 1 -delete 2>/dev/null || true
    return 0
}

umount_count() { awk '/^umount /{n++} END{print n+0}' "$CALLS"; }

reset_scenario() {
    : >"$CALLS"
    SEARCH_DIRS=""
    DEVMAP=""
    rm -rf "${WORK:?}/mnt"
    mkdir -p "$WORK/mnt"
    UF2_MOUNT_ROOT="$WORK/mnt"
    UF2_MOUNT_REGISTRY="$WORK/registry"
    UF2_SEEN_REGISTRY="$WORK/seen"
    : >"$UF2_MOUNT_REGISTRY"
    : >"$UF2_SEEN_REGISTRY"
}

# --- 1. A foreign volume must not shadow the board being flashed ------------
# The red test. Two volumes are already mounted, the FIRST belonging to another
# board. Pre-fix, find_uf2_drive returns that first one and nothing else, so
# poll_matching_drive refuses it and never sees the second.

reset_scenario
BOOTLOADER_BOARD_ID="WisBlock-RAK4631-Board"
S1_T114="$(volume s1-t114 "$T114_INFO")"
S1_RAK="$(volume s1-rak "$RAK_INFO")"
SEARCH_DIRS="$S1_T114"$'\n'"$S1_RAK"$'\n'
S1_RC=0
S1_GOT="$(poll_matching_drive "(test)" 0 2>/dev/null)" || S1_RC=$?
check_eq "a foreign volume listed first does not hide the matching one" "$S1_RAK" "$S1_GOT"
check_eq "...and selection succeeds" "0" "$S1_RC"

# --- 2. The rig scenario itself ---------------------------------------------
# The foreign volume is the one already mounted (this is /mnt on the rig) and
# the board being flashed is still an unmounted block device. Pre-fix the
# search path short-circuits on the foreign volume and the device is never
# examined; the mount point being occupied is the second half of the failure.

reset_scenario
BOOTLOADER_BOARD_ID="WisBlock-RAK4631-Board"
S2_MNT="$(volume s2-mnt "$T114_INFO")"
S2_RAK="$(volume s2-rak "$RAK_INFO")"
SEARCH_DIRS="$S2_MNT"$'\n'
DEVMAP="/dev/fake-sdb1"$'\t'"$S2_RAK"
S2_GOT="$(poll_matching_drive "(test)" 0 2>/dev/null)"
check_eq "an occupied mount point does not hide an unmounted board" "$WORK/mnt/fake-sdb1" "$S2_GOT"

# --- 3. Nothing matches: no write, and the message names what was seen ------

reset_scenario
BOOTLOADER_BOARD_ID="WisBlock-RAK4631-Board"
S3_T114="$(volume s3-t114 "$T114_INFO")"
SEARCH_DIRS="$S3_T114"$'\n'
S3_RC=0
S3_GOT="$(poll_matching_drive "(test)" 0 2>/dev/null)" || S3_RC=$?
check_eq "a foreign-only search path yields no volume" "" "$S3_GOT"
check_eq "...and reports failure, so no drive reaches the copy" "1" "$S3_RC"
check_eq "nothing was written to the foreign volume" "" \
    "$(find "$S3_T114" -name 'NEW.UF2' 2>/dev/null)"
if declare -F uf2_no_match_message >/dev/null; then
    S3_MSG="$(uf2_no_match_message)"
else
    S3_MSG="(uf2_no_match_message is not defined)"
fi
check_contains "the give-up message says what was actually wrong" \
    "no matching UF2 volume" "$S3_MSG"
check_contains "the give-up message names the volume and its Board-ID" \
    "$S3_T114 (HT-n5262)" "$S3_MSG"
check_contains "the give-up message names the Board-ID it wanted" \
    "WisBlock-RAK4631-Board" "$S3_MSG"

# --- 4. A volume we mounted and the caller rejected is unmounted again ------
# The leak that made #341 permanent: the foreign volume stays mounted, so it
# shadows every later board and survives everything short of a hand umount.

reset_scenario
BOOTLOADER_BOARD_ID="WisBlock-RAK4631-Board"
S4_T114="$(volume s4-t114 "$T114_INFO")"
DEVMAP="/dev/fake-sdb1"$'\t'"$S4_T114"
poll_matching_drive "(test)" 0 >/dev/null 2>&1 || true
S4_CALLS="$(cat "$CALLS")"
check_contains "the foreign device was mounted to be examined" \
    "mount /dev/fake-sdb1 " "$S4_CALLS"
check_contains "...and unmounted again once it was refused" \
    "umount $WORK/mnt" "$S4_CALLS"

# --- 5. A volume we mounted and the caller TOOK stays until released --------

reset_scenario
BOOTLOADER_BOARD_ID="WisBlock-RAK4631-Board"
S5_RAK="$(volume s5-rak "$RAK_INFO")"
DEVMAP="/dev/fake-sdb1"$'\t'"$S5_RAK"
S5_GOT="$(poll_matching_drive "(test)" 0 2>/dev/null)"
check_eq "a device we mounted and took is handed back" "$WORK/mnt/fake-sdb1" "$S5_GOT"
check_eq "...and is still mounted, because the copy has not run yet" "0" "$(umount_count)"
if declare -F release_uf2_volume >/dev/null; then
    release_uf2_volume "$S5_GOT"
else
    printf 'note  release_uf2_volume is not defined\n'
fi
check_eq "release_uf2_volume unmounts it exactly once" "1" "$(umount_count)"
if declare -F release_uf2_volume >/dev/null; then
    release_uf2_volume "$S5_GOT"
fi
check_eq "...and releasing twice does not unmount twice" "1" "$(umount_count)"

# --- 6. The single-matching-volume path stays exactly as it was -------------

reset_scenario
BOOTLOADER_BOARD_ID="HT-n5262"
S6_T114="$(volume s6-t114 "$T114_INFO")"
SEARCH_DIRS="$S6_T114"$'\n'
S6_RC=0
S6_GOT="$(poll_matching_drive "(test)" 0 2>/dev/null)" || S6_RC=$?
check_eq "the single matching volume is selected" "$S6_T114" "$S6_GOT"
check_eq "...with success" "0" "$S6_RC"
check_eq "a volume we did not mount is never unmounted" "0" "$(umount_count)"

# --- 7. A block device that is not a bootloader is released at once ---------

reset_scenario
BOOTLOADER_BOARD_ID="HT-n5262"
S7_PLAIN="$WORK/vol/s7-plain"
mkdir -p "$S7_PLAIN"
: >"$S7_PLAIN/readme.txt"
DEVMAP="/dev/fake-sdc1"$'\t'"$S7_PLAIN"
poll_matching_drive "(test)" 0 >/dev/null 2>&1 || true
check_eq "a device with no INFO_UF2.TXT is unmounted immediately" "1" "$(umount_count)"

# --- 8. A foreign volume is reported once per poll, not once per tick -------
# The poll runs twice a second. The "ignoring ..." line has to be one-shot, and
# the state that makes it one-shot has to survive from tick to tick — which it
# does not if the scan is run inside a command substitution.

reset_scenario
BOOTLOADER_BOARD_ID="WisBlock-RAK4631-Board"
S8_T114="$(volume s8-t114 "$T114_INFO")"
SEARCH_DIRS="$S8_T114"$'\n'
S8_WARNINGS="$(poll_matching_drive "(test)" 2 2>&1 >/dev/null | grep -c 'ignoring UF2 drive')"
check_eq "a foreign volume is warned about once across three ticks" "1" "$S8_WARNINGS"

# --- Summary ----------------------------------------------------------------

printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
