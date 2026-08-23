# shellcheck shell=bash
# Which bootloader volume the flash is about to be written to.
#
# Sourced by uf2-runner.sh. Defines constants and functions only and runs
# nothing, so tools/test-uf2-volumes.sh can drive discovery and selection
# against fixture directories with no board and no sudo, the same way
# tools/softdevice-guard.sh is driven by tools/test-softdevice-guard.sh.
#
# Why it is its own file. Every write in uf2-runner.sh lands on whatever this
# code hands back, and it used to hand back the FIRST UF2 volume in the search
# path and stop looking. With a T114 parked in its bootloader and mounted at
# /mnt, every RAK4631 flash was offered that one foreign volume, refused it
# correctly, asked again, and was offered it again — three attempts, then
# "app never re-enumerated", a symptom that had not occurred, while the RAK's
# own volume sat unmounted on /dev/sdb (Codeberg #341, measured 2026-08-23).
#
# Three properties follow, and the fixture test holds each of them:
#   1. Discovery ENUMERATES. Selection by Board-ID happens once, over the whole
#      set, never by taking the first thing seen.
#   2. A mount this file makes is a mount this file owns. Anything the caller
#      does not take goes back immediately, because the leaked /mnt mount is
#      what turned a transient shadow into a permanent one.
#   3. Giving up names the volumes that were actually seen and their Board-IDs.
#
# The machine-touching calls (mount, umount, mkdir, udisksctl) are one-line
# functions rather than inline commands purely so the test can replace them.

# Where volumes are mounted when we have to mount them ourselves. Deliberately
# NOT /mnt: the volume that shadows everything else is typically the one
# already sitting there, and a mount point under an occupied /mnt would be a
# path inside that foreign filesystem. One subdirectory per device, so a
# foreign volume can never occupy the only slot.
UF2_MOUNT_ROOT="${LEVICULUM_UF2_MOUNT_ROOT:-/run/leviculum-uf2}"

# Volumes THIS run mounted: one "<path>\t<method>\t<device>" per line.
# A file rather than a shell variable because find_uf2_drive is called inside a
# command substitution, and an assignment made there dies with the subshell —
# the ownership record has to outlive it or the unmount can never happen.
UF2_MOUNT_REGISTRY="${LEVICULUM_UF2_MOUNT_REGISTRY:-${TMPDIR:-/tmp}/leviculum-uf2-mounts.$$}"

# Volumes seen by the last scan: one "<path>\t<board-id>" per line, so the
# give-up message can report observation instead of assertion. Same reason for
# being a file.
UF2_SEEN_REGISTRY="${LEVICULUM_UF2_SEEN_REGISTRY:-${TMPDIR:-/tmp}/leviculum-uf2-seen.$$}"

# --- Machine-touching primitives (the test seams) ---------------------------

uf2_have_udisks() { command -v udisksctl >/dev/null 2>&1; }
uf2_udisks_mount() { udisksctl mount -b "$1" 2>/dev/null; }
uf2_udisks_unmount() { udisksctl unmount -b "$1" 2>/dev/null; }
uf2_mkdir() { sudo -n mkdir -p "$1" 2>/dev/null; }
uf2_mount() { sudo -n mount "$1" "$2" 2>/dev/null; }
uf2_umount() { sudo -n umount "$1" 2>/dev/null; }

# Directories an already-mounted bootloader volume can turn up in. $UF2_MOUNT_ROOT
# is included so a volume left behind by a killed run is found and re-owned
# rather than shadowing forever.
uf2_search_dirs() {
    local d seen=""
    for d in "/media/${USER:-}" "/run/media/${USER:-}" "$UF2_MOUNT_ROOT" /mnt; do
        case "$d" in */) d="${d%/}" ;; esac
        # An unset $USER would leave "/media" and "/run/media" themselves,
        # which are other users' mount roots and not ours to scan.
        if [ -z "$d" ] || [ "$d" = "/media" ] || [ "$d" = "/run/media" ]; then
            continue
        fi
        [ -d "$d" ] || continue
        case "$seen" in
        *"|$d|"*) continue ;;
        esac
        seen="$seen|$d|"
        printf '%s\n' "$d"
    done
    return 0
}

# Partitions small enough to be a UF2 bootloader volume (< 64 MB), one per line.
uf2_block_devices() {
    local dev size part
    for dev in /dev/sd?; do
        [ -b "$dev" ] || continue
        size="$(cat "/sys/block/$(basename "$dev")/size" 2>/dev/null || echo 0)"
        # size is in 512-byte sectors; 64 MB = 131072 sectors.
        if [ "$size" -le 0 ] || [ "$size" -ge 131072 ]; then
            continue
        fi
        part="${dev}1"
        [ -b "$part" ] || part="$dev"
        printf '%s\n' "$part"
    done
    return 0
}

uf2_device_is_mounted() { grep -q "^$1 " /proc/mounts 2>/dev/null; }

# --- The ownership registry -------------------------------------------------

uf2_register_mount() {
    printf '%s\t%s\t%s\n' "$1" "$2" "$3" >>"$UF2_MOUNT_REGISTRY" 2>/dev/null || true
}

# Unmount registered volumes and forget them.
#   uf2_drain_registry only   <path>   unmount exactly that one
#   uf2_drain_registry except <path>   unmount all but that one ("" = all)
# The registry is rewritten from what survives, so draining is idempotent: a
# second call for the same path finds nothing to do and unmounts nothing.
# Args: $1 = mode, $2 = path ("" allowed in except mode)
uf2_drain_registry() {
    local mode="$1" path="${2:-}" p method dev kept="" drop
    [ -f "$UF2_MOUNT_REGISTRY" ] || return 0
    while IFS=$'\t' read -r p method dev; do
        [ -n "$p" ] || continue
        drop=0
        case "$mode" in
        only)
            [ "$p" = "$path" ] && drop=1
            ;;
        except)
            if [ -z "$path" ] || [ "$p" != "$path" ]; then drop=1; fi
            ;;
        esac
        if [ "$drop" -eq 0 ]; then
            kept="$kept$p"$'\t'"$method"$'\t'"$dev"$'\n'
            continue
        fi
        case "$method" in
        udisks) uf2_udisks_unmount "$dev" ;;
        mount) uf2_umount "$p" ;;
        esac
    done <"$UF2_MOUNT_REGISTRY"
    printf '%s' "$kept" >"$UF2_MOUNT_REGISTRY" 2>/dev/null || true
    return 0
}

# Give one volume back. A no-op for a volume somebody else mounted — we undo
# our own mounts and nobody else's.
# Args: $1 = volume path
release_uf2_volume() {
    [ -n "${1:-}" ] || return 0
    uf2_drain_registry only "$1"
}

# Give back everything except the volume the caller took ("" = everything).
# Args: $1 = volume path to keep, or ""
release_unclaimed_uf2_volumes() { uf2_drain_registry except "${1:-}"; }

# Start a run with an empty registry and no inherited mounts. Anything still
# mounted under $UF2_MOUNT_ROOT belongs to a run that died without draining;
# it is ours by construction, so take it back.
uf2_registry_init() {
    local mp
    : >"$UF2_MOUNT_REGISTRY" 2>/dev/null || true
    : >"$UF2_SEEN_REGISTRY" 2>/dev/null || true
    while IFS= read -r mp; do
        [ -n "$mp" ] || continue
        uf2_umount "$mp"
    done <<<"$(awk -v root="$UF2_MOUNT_ROOT/" '$2 ~ "^"root {print $2}' /proc/mounts 2>/dev/null)"
    return 0
}

# --- Discovery --------------------------------------------------------------

# Mount one device and report it iff it carries an INFO_UF2.TXT. A volume kept
# is registered; a volume that turns out to be something else is unmounted in
# the same breath. Prints the mount point, or nothing.
# Args: $1 = device path
uf2_try_mount_device() {
    local dev="$1" out mp
    if uf2_have_udisks; then
        out="$(uf2_udisks_mount "$dev" || true)"
        if [ -n "$out" ]; then
            mp="$(printf '%s' "$out" | grep -oP 'at \K/.*' || true)"
            # udisksctl ends the line with a full stop; a mount point does not.
            mp="${mp%.}"
            if [ -n "$mp" ]; then
                if [ -f "$mp/INFO_UF2.TXT" ]; then
                    uf2_register_mount "$mp" udisks "$dev"
                    printf '%s\n' "$mp"
                else
                    uf2_udisks_unmount "$dev"
                fi
                return 0
            fi
        fi
    fi

    mp="$UF2_MOUNT_ROOT/$(basename "$dev")"
    uf2_mkdir "$mp" || return 0
    if uf2_mount "$dev" "$mp"; then
        if [ -f "$mp/INFO_UF2.TXT" ]; then
            uf2_register_mount "$mp" mount "$dev"
            printf '%s\n' "$mp"
        else
            uf2_umount "$mp"
        fi
    fi
    return 0
}

# Every UF2 volume reachable right now, one path per line. NO selection: a
# volume belonging to another board is still printed, because deciding that is
# poll_matching_drive's job over the whole set, and skipping it here is exactly
# the shortcut that produced #341.
find_uf2_drive() {
    local dir info dev mp seen=""

    while IFS= read -r dir; do
        [ -n "$dir" ] || continue
        while IFS= read -r info; do
            [ -n "$info" ] || continue
            mp="$(dirname "$info")"
            case "$seen" in
            *"|$mp|"*) continue ;;
            esac
            seen="$seen|$mp|"
            printf '%s\n' "$mp"
        done <<<"$(find "$dir" -maxdepth 2 -name INFO_UF2.TXT -type f 2>/dev/null)"
    done <<<"$(uf2_search_dirs)"

    while IFS= read -r dev; do
        [ -n "$dev" ] || continue
        uf2_device_is_mounted "$dev" && continue
        mp="$(uf2_try_mount_device "$dev")"
        [ -n "$mp" ] || continue
        case "$seen" in
        *"|$mp|"*) continue ;;
        esac
        seen="$seen|$mp|"
        printf '%s\n' "$mp"
    done <<<"$(uf2_block_devices)"

    return 0
}

# --- Selection --------------------------------------------------------------

# The Board-ID a volume publishes, or a token saying why there is none.
# Args: $1 = volume path
uf2_volume_board_id() {
    local d="$1" id=""
    if [ ! -f "$d/INFO_UF2.TXT" ]; then
        printf '(missing)'
        return 0
    fi
    id="$(grep -m1 -oE 'Board-ID:[[:space:]]*[^[:space:]]+' "$d/INFO_UF2.TXT" 2>/dev/null |
        awk '{print $2}')"
    printf '%s' "${id:-(unknown)}"
}

# Does this volume belong to the board we are flashing? Without this check the
# copy would clobber whatever bootloader happens to be mounted — a T114 in UF2
# mode taking a RAK4631 image is a wrong UF2 on the wrong silicon.
# Args: $1 = volume path
uf2_drive_matches_board() {
    local d="$1"
    [ -n "$d" ] || return 1
    [ -f "$d/INFO_UF2.TXT" ] || return 1
    grep -q "$BOOTLOADER_BOARD_ID" "$d/INFO_UF2.TXT" 2>/dev/null
}

# The result of the last uf2_scan_once, and which foreign volumes it has
# already complained about. Globals rather than printed values on purpose:
# uf2_scan_once must NOT be called inside a command substitution, because both
# of these have to survive it — a subshell would silently drop the warn-once
# state and flood the log twice a second.
UF2_SCAN_MATCH=""
UF2_SCAN_WARNED=""

# One scan: record every volume seen with its Board-ID, set UF2_SCAN_MATCH to
# the one that matches (empty if none), warn once per foreign volume, and hand
# back every volume we mounted and are not keeping. One exit path, so the
# unmount obligation cannot be skipped by returning early.
# Args: $1 = hint for log lines
uf2_scan_once() {
    local hint="$1" cand id
    UF2_SCAN_MATCH=""

    : >"$UF2_SEEN_REGISTRY" 2>/dev/null || true
    while IFS= read -r cand; do
        [ -n "$cand" ] || continue
        id="$(uf2_volume_board_id "$cand")"
        printf '%s\t%s\n' "$cand" "$id" >>"$UF2_SEEN_REGISTRY" 2>/dev/null || true
        if uf2_drive_matches_board "$cand"; then
            [ -z "$UF2_SCAN_MATCH" ] && UF2_SCAN_MATCH="$cand"
            continue
        fi
        case "$UF2_SCAN_WARNED" in
        *"|$cand|"*) ;;
        *)
            echo "[uf2-runner] $hint: ignoring UF2 drive at $cand — Board-ID '$id'" \
                "does not match expected '$BOOTLOADER_BOARD_ID'" >&2
            UF2_SCAN_WARNED="$UF2_SCAN_WARNED|$cand|"
            ;;
        esac
    done <<<"$(find_uf2_drive)"

    release_unclaimed_uf2_volumes "$UF2_SCAN_MATCH"
    [ -n "$UF2_SCAN_MATCH" ]
}

# Poll up to $2 ticks (0.5 s each; 0 = check once) for a UF2 volume whose
# Board-ID matches $BOOTLOADER_BOARD_ID. Prints that volume's path and returns
# 0; prints nothing and returns 1 when the budget runs out. The matched volume
# stays mounted and stays in the registry — release_uf2_volume gives it back
# once the copy is done.
# Args: $1 = hint for log lines, $2 = max ticks
poll_matching_drive() {
    local hint="$1" max_ticks="$2" tick=0
    UF2_SCAN_WARNED=""
    while :; do
        uf2_scan_once "$hint" || true
        if [ -n "$UF2_SCAN_MATCH" ]; then
            printf '%s\n' "$UF2_SCAN_MATCH"
            return 0
        fi
        [ "$tick" -ge "$max_ticks" ] && break
        sleep 0.5
        tick=$((tick + 1))
    done
    echo ""
    return 1
}

# Is a volume for OUR board on the machine right now? Quiet — no warnings, no
# polling, and nothing left mounted, because the caller only wants the answer.
matching_uf2_volume_present() {
    local cand rc=1
    while IFS= read -r cand; do
        [ -n "$cand" ] || continue
        uf2_drive_matches_board "$cand" && rc=0
    done <<<"$(find_uf2_drive)"
    release_unclaimed_uf2_volumes ""
    return "$rc"
}

# --- Reporting --------------------------------------------------------------

# The volumes the last scan saw, as "<path> (<board-id>)" joined by commas.
uf2_seen_summary() {
    local p id out=""
    [ -f "$UF2_SEEN_REGISTRY" ] || {
        printf '(none)'
        return 0
    }
    while IFS=$'\t' read -r p id; do
        [ -n "$p" ] || continue
        [ -n "$out" ] && out="$out, "
        out="$out$p ($id)"
    done <"$UF2_SEEN_REGISTRY"
    printf '%s' "${out:-(none)}"
}

# What to say when no volume for this board could be found. It reports what was
# observed rather than asserting a symptom: the old wording claimed the
# application never re-enumerated, which in the #341 failure had not been tried,
# and reading it cost an hour.
uf2_no_match_message() {
    printf "no matching UF2 volume (expected Board-ID '%s'); seen: %s" \
        "$BOOTLOADER_BOARD_ID" "$(uf2_seen_summary)"
}
