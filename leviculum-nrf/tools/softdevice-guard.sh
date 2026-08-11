# shellcheck shell=bash
# The SoftDevice precondition — the one check that stops a flash from
# soft-bricking a board.
#
# Sourced by uf2-runner.sh. Defines constants and functions only and runs
# nothing, so tools/test-softdevice-guard.sh can drive the parsing and the
# decision against fixture files, with no board attached.
#
# Why it exists. Our application is linked at 0x27000 (leviculum-nrf/memory.x,
# and --base in uf2-runner.sh), the application base S140 7.x forwards to. A
# board still carrying the factory S140 6.1.1 forwards to 0x26000, finds no
# vector table a page below where our image starts, and crashes before USB
# initialises: no CDC ports, no bootloader drive, nothing on the bus. It looks
# like dead hardware and is not — an application flash never touches the
# bootloader at 0xF4000, so a physical double-tap RESET always brings the UF2
# drive back. A T114 was written off as bricked for weeks over exactly this.
# See docs/src/concepts/lnode-flashing.md, "The mismatch is a soft brick, not
# a dead board".
#
# What it reads. The `SoftDevice:` line the bootloader publishes in
# INFO_UF2.TXT — the same source lnflash reads (lnflash/src/infouf2.rs). The
# bootloader generates that line at runtime from the SoftDevice actually
# installed, so it answers the version question rather than inferring it from
# a USB ID or a board model.

# The constraint lnflash enforces for the identical image at the identical
# base (`requires.softdevice = ">=7.0.1, <8.0.0"` in the bundle manifest,
# checked by lnflash/src/flow.rs). Deliberately the same constraint and not a
# stricter "must be exactly 7.3.0": two tools that write the same bytes to the
# same address must not disagree about which boards may receive them.
SOFTDEVICE_NAME="S140"
SOFTDEVICE_MIN_WORD=7000001   # >=7.0.1, the version the nrf-softdevice bindings target
SOFTDEVICE_MAX_WORD=8000000   # <8.0.0, exclusive — nothing is promised past major 7

# What the remedy installs, i.e. what lnflash/payload/t114/ vendors.
SOFTDEVICE_REMEDY_VERSION="7.3.0"

# --- Parsing ----------------------------------------------------------------

# Print the value of the `SoftDevice:` line of an INFO_UF2.TXT, e.g.
# "S140 7.3.0". Prints nothing when the file is absent or carries no such
# line — bootloaders older than the one that started emitting it simply do
# not have it, which is data rather than an error (lnflash/src/infouf2.rs
# takes the same view). The key is matched case-insensitively and the CRLF
# the real files use is stripped.
# Args: $1 = path to INFO_UF2.TXT
info_uf2_softdevice() {
    local file="$1" line
    [ -f "$file" ] || return 0
    line="$(grep -i -m1 '^SoftDevice:' "$file" 2>/dev/null || true)"
    [ -n "$line" ] || return 0
    line="${line#*:}"
    line="$(printf '%s' "$line" | tr -d '\r')"
    # Trim leading and trailing whitespace.
    line="${line#"${line%%[![:space:]]*}"}"
    line="${line%"${line##*[![:space:]]}"}"
    printf '%s' "$line"
}

# Print Nordic's packed encoding of a major.minor.bugfix version:
# major * 1000000 + minor * 1000 + bugfix, the same word the SoftDevice keeps
# at 0x3014 and lnflash/src/softdevice.rs decodes. Prints nothing for anything
# that is not exactly three decimal components, so "unknown" and "7.3.0.1" are
# both unreadable rather than silently truncated.
# Args: $1 = version text
softdevice_word() {
    local v="$1" rest major minor bugfix
    case "$v" in
        *.*.*) ;;
        *) return 0 ;;
    esac
    major="${v%%.*}"
    rest="${v#*.}"
    minor="${rest%%.*}"
    bugfix="${rest#*.}"
    case "$major$minor$bugfix" in
        '' | *[!0-9]*) return 0 ;;
    esac
    # Leading zeros would be read as octal by $(( )).
    printf '%s' "$((10#$major * 1000000 + 10#$minor * 1000 + 10#$bugfix))"
}

# Judge one INFO_UF2.TXT. Prints a verdict token plus what was found, and
# returns:
#   0  ok <name version>          the constraint holds, writing is safe
#   1  mismatch <name version>    it does not hold, writing would soft-brick
#   2  unknown                    no SoftDevice: line, so nothing can be said
# A line that is present but unreadable ("S140 unknown") is a mismatch, not an
# unknown: lnflash takes the remedy rather than the risk in that case, and a
# bootloader modern enough to emit the line emits a parseable version.
# Args: $1 = path to INFO_UF2.TXT
softdevice_verdict() {
    local file="$1" found name version word
    found="$(info_uf2_softdevice "$file")"
    if [ -z "$found" ]; then
        printf 'unknown'
        return 2
    fi
    # "S140 7.3.0" — split at the LAST space, so a name containing one is
    # still kept whole.
    version="${found##* }"
    name="${found% *}"
    word="$(softdevice_word "$version")"
    if [ "$name" = "$SOFTDEVICE_NAME" ] && [ -n "$word" ] &&
        [ "$word" -ge "$SOFTDEVICE_MIN_WORD" ] && [ "$word" -lt "$SOFTDEVICE_MAX_WORD" ]; then
        printf 'ok %s' "$found"
        return 0
    fi
    printf 'mismatch %s' "$found"
    return 1
}

# --- The guard ---------------------------------------------------------------

# Decide whether the application UF2 may be written to a mounted bootloader
# drive. Returns 0 to proceed, 1 to refuse.
#
# Every outcome is logged, including the ones that proceed: a guard that
# passes silently is indistinguishable from a guard that is not running.
# Args: $1 = drive path, $2 = hint for log lines
guard_softdevice() {
    local drive="$1" hint="$2"
    local file="$drive/INFO_UF2.TXT"
    local rc=0 verdict found

    if [ -n "${LEVICULUM_SKIP_SD_CHECK:-}" ]; then
        found="$(info_uf2_softdevice "$file")"
        echo "[uf2-runner] $hint: LEVICULUM_SKIP_SD_CHECK set — SoftDevice check BYPASSED" \
            "(the board reports '${found:-no SoftDevice: line}')" >&2
        return 0
    fi

    verdict="$(softdevice_verdict "$file")" || rc=$?
    found="${verdict#* }"

    if [ "$rc" -eq 0 ]; then
        echo "[uf2-runner] $hint: SoftDevice $found satisfies" \
             ">=7.0.1, <8.0.0 — proceeding"
        return 0
    fi

    if [ "$rc" -eq 2 ]; then
        # A bootloader too old to publish the line. lnflash tolerates the same
        # case rather than refusing on an absent key, and refusing here would
        # block boards that are very probably fine.
        echo "[uf2-runner] $hint: WARNING — $file publishes no 'SoftDevice:' line," \
             "so the version could not be checked. Proceeding." >&2
        echo "[uf2-runner] $hint: WARNING — if the board goes dark after this flash it is" \
             "the SoftDevice mismatch, not dead hardware: double-tap RESET and see" \
             "docs/src/concepts/lnode-flashing.md." >&2
        return 0
    fi

    cat >&2 <<EOF
[uf2-runner] $hint: REFUSING to flash — SoftDevice mismatch.
    the bootloader on $drive reports:  $found
    this image needs:                  $SOFTDEVICE_NAME >=7.0.1, <8.0.0
    It is linked at ${FLASH_BASE:-0x27000}, and S140 6.1.1 forwards to 0x26000, a page
    lower. Writing it would leave the board dark on the bus — a soft brick,
    not a dead board: double-tap RESET always brings the bootloader drive
    back. Nothing was written.

    Remedy — install S140 $SOFTDEVICE_REMEDY_VERSION first, then flash again:
      just lnflash-bundle
      tar xzf target/lnflash/lnflash-*.tar.gz -C target/lnflash
      sudo target/lnflash/lnflash-*/lnflash
    lnflash confirms the board from its own bootloader, installs the
    SoftDevice from lnflash/payload/t114/, and writes the application after.
    Background: docs/src/concepts/lnode-flashing.md, "Installing the SoftDevice".

    To flash anyway (this is how boards get soft-bricked):
      LEVICULUM_SKIP_SD_CHECK=1
EOF
    return 1
}
