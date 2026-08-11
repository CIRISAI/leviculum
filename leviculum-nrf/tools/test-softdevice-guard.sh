#!/usr/bin/env bash
# Fixture test for the SoftDevice guard (tools/softdevice-guard.sh).
#
# The guard's whole job happens on a mounted bootloader drive, which is the
# one thing CI does not have. What it decides from, though, is a text file, so
# the decision is checkable against fixtures: real INFO_UF2.TXT content read
# off the rig boards, plus the variants that matter (the factory 6.1.1 that
# soft-bricks, the old bootloader that publishes no line at all).
#
# This covers the logic, not the plumbing. That the guard is reached before
# every write, and that a real 6.1.1 board is actually refused, is a rig check.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=leviculum-nrf/tools/softdevice-guard.sh
. "$SCRIPT_DIR/softdevice-guard.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

PASS=0
FAIL=0

# Write an INFO_UF2.TXT into its own directory (the guard takes a drive path)
# and print that directory.
fixture() {
    local name="$1" text="$2" dir
    dir="$WORK/$name"
    mkdir -p "$dir"
    printf '%s' "$text" >"$dir/INFO_UF2.TXT"
    printf '%s' "$dir"
}

# Args: $1 = what is being checked, $2 = expected rc, $3 = drive path
check_verdict() {
    local what="$1" want="$2" drive="$3"
    local rc=0 out
    out="$(softdevice_verdict "$drive/INFO_UF2.TXT")" || rc=$?
    if [ "$rc" -eq "$want" ]; then
        PASS=$((PASS + 1))
        printf 'ok    verdict %-44s rc=%s (%s)\n' "$what" "$rc" "$out"
    else
        FAIL=$((FAIL + 1))
        printf 'FAIL  verdict %-44s rc=%s want=%s (%s)\n' "$what" "$rc" "$want" "$out"
    fi
}

# Args: $1 = what, $2 = expected rc, $3 = drive path, $4 = text the output must
# contain ("" for no requirement)
check_guard() {
    local what="$1" want="$2" drive="$3" needle="${4:-}"
    local rc=0 out
    out="$(guard_softdevice "$drive" "(test)" 2>&1)" || rc=$?
    if [ "$rc" -ne "$want" ]; then
        FAIL=$((FAIL + 1))
        printf 'FAIL  guard   %-44s rc=%s want=%s\n' "$what" "$rc" "$want"
        return
    fi
    if [ -n "$needle" ] && [[ "$out" != *"$needle"* ]]; then
        FAIL=$((FAIL + 1))
        printf 'FAIL  guard   %-44s rc=%s but output lacks %q\n' "$what" "$rc" "$needle"
        return
    fi
    PASS=$((PASS + 1))
    printf 'ok    guard   %-44s rc=%s\n' "$what" "$rc"
}

# --- Fixtures ---------------------------------------------------------------

# Read off the rig T114, verbatim, CRLF included
# (docs/src/concepts/lnode-flashing.md; the same text lnflash/src/infouf2.rs
# tests against).
T114=$'UF2 Bootloader 0.9.0-2-g836c8dc-dirty lib/nrfx (v2.0.0) lib/tinyusb (0.12.0-145-g9775e7691) lib/uf2 (remotes/origin/configupdate-9-gadbb8c7)\r\nModel: HT-n5262\r\nBoard-ID: HT-n5262\r\nDate: Jul  9 2024\r\nSoftDevice: S140 7.3.0\r\n'

# The RAK4631's file: same shape plus a `Ver:` key.
RAK=$'UF2 Bootloader 0.4.3\r\nModel: WisBlock RAK4631 Board\r\nBoard-ID: WisBlock-RAK4631-Board\r\nDate: May 20 2023\r\nVer: 0.4.3\r\nSoftDevice: S140 7.3.0\r\n'

# The factory board that was written off as bricked for weeks, serial
# 183004F712B4A7FE (docs/src/concepts/lnode-flashing.md).
FACTORY="${T114//S140 7.3.0/S140 6.1.1}"

# A bootloader too old to publish the line at all.
NO_LINE="${T114//$'SoftDevice: S140 7.3.0\r\n'/}"

D_OK="$(fixture ok "$T114")"
D_RAK="$(fixture rak "$RAK")"
D_FACTORY="$(fixture factory "$FACTORY")"
D_NOLINE="$(fixture noline "$NO_LINE")"
D_LF="$(fixture lf "${T114//$'\r\n'/$'\n'}")"
D_LOWER="$(fixture lower "${T114//SoftDevice:/softdevice:}")"
D_FLOOR="$(fixture floor "${T114//S140 7.3.0/S140 7.0.1}")"
D_BELOW="$(fixture below "${T114//S140 7.3.0/S140 7.0.0}")"
D_NEXTMAJOR="$(fixture nextmajor "${T114//S140 7.3.0/S140 8.0.0}")"
D_UNREADABLE="$(fixture unreadable "${T114//S140 7.3.0/S140 unknown}")"
D_WRONGNAME="$(fixture wrongname "${T114//S140 7.3.0/S112 7.3.0}")"
D_EMPTY="$(fixture empty "")"
D_ABSENT="$WORK/absent" # no INFO_UF2.TXT at all
mkdir -p "$D_ABSENT"

# --- Parsing ----------------------------------------------------------------

if [ "$(info_uf2_softdevice "$D_OK/INFO_UF2.TXT")" = "S140 7.3.0" ]; then
    PASS=$((PASS + 1)); printf 'ok    parse   the CRLF line yields the bare value\n'
else
    FAIL=$((FAIL + 1)); printf 'FAIL  parse   the CRLF line yields the bare value: %q\n' \
        "$(info_uf2_softdevice "$D_OK/INFO_UF2.TXT")"
fi

if [ "$(softdevice_word 7.3.0)" = "7003000" ] &&
    [ "$(softdevice_word 6.1.1)" = "6001001" ] &&
    [ "$(softdevice_word 7.0.1)" = "7000001" ] &&
    [ -z "$(softdevice_word unknown)" ] &&
    [ -z "$(softdevice_word 7.3)" ] &&
    [ -z "$(softdevice_word 7.3.0.1)" ]; then
    PASS=$((PASS + 1)); printf 'ok    parse   versions pack the way Nordic packs them\n'
else
    FAIL=$((FAIL + 1)); printf 'FAIL  parse   versions pack the way Nordic packs them\n'
fi

# --- The three cases the guard exists for -----------------------------------

check_verdict "the rig T114 at 7.3.0 is allowed"        0 "$D_OK"
check_verdict "the factory board at 6.1.1 is refused"   1 "$D_FACTORY"
check_verdict "a bootloader with no line is unknown"    2 "$D_NOLINE"

# --- Everything around them -------------------------------------------------

check_verdict "the RAK file, extra key and all"         0 "$D_RAK"
check_verdict "LF-only line endings read the same"      0 "$D_LF"
check_verdict "the key is matched case-insensitively"   0 "$D_LOWER"
check_verdict "7.0.1, the floor lnflash accepts"        0 "$D_FLOOR"
check_verdict "7.0.0, one bugfix below the floor"       1 "$D_BELOW"
check_verdict "8.0.0, the next major"                   1 "$D_NEXTMAJOR"
check_verdict "a present but unreadable version"        1 "$D_UNREADABLE"
check_verdict "S112, the right version on the wrong SD" 1 "$D_WRONGNAME"
check_verdict "an empty file"                           2 "$D_EMPTY"
check_verdict "no INFO_UF2.TXT at all"                  2 "$D_ABSENT"

# --- The decision, including what it tells the operator ---------------------

check_guard "7.3.0 proceeds"           0 "$D_OK"      "proceeding"
check_guard "6.1.1 refuses"            1 "$D_FACTORY" "REFUSING to flash"
check_guard "the refusal names what it found" 1 "$D_FACTORY" "S140 6.1.1"
check_guard "the refusal names the remedy"    1 "$D_FACTORY" "lnflash-bundle"
check_guard "the refusal names the override"  1 "$D_FACTORY" "LEVICULUM_SKIP_SD_CHECK=1"
check_guard "no line warns but proceeds" 0 "$D_NOLINE" "WARNING"

# The override, and that taking it is visible in the log of the flash it
# allowed.
LEVICULUM_SKIP_SD_CHECK=1 check_guard "the override lets 6.1.1 through" 0 "$D_FACTORY" "BYPASSED"
LEVICULUM_SKIP_SD_CHECK=1 check_guard "the bypass still names the version" 0 "$D_FACTORY" "S140 6.1.1"

# --- Summary ----------------------------------------------------------------

printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
