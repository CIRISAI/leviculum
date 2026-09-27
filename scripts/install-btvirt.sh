#!/bin/bash
# Build and install the bluez `btvirt` the periculum `ble_room_*` cells need.
#
# WHY THIS IS NOT "apt install bluez-test-tools" AND NOT A STOCK BUILD
#
# The emulator IS the medium under those cells, so which btvirt a bench holds
# decides their verdict. bluez 5.82 and older give each side of a link its own
# connection handle but report the CENTRAL's handle to the peripheral and
# forward ACL data under the SENDER's handle. Both sides start counting at the
# same number, so the two errors are invisible while every device holds at most
# one link (N = 2 green) and certain from the second concurrent link: measured
# 2026-09-27, the room stalled from N >= 3 and the stall count grew faster than
# N, because the misrouted MTU response also killed healthy ATT channels on
# third nodes. Upstream `4ff7deaf8c` fixes exactly that, and with it the ladder
# is green to N = 16 -- the emulator's own `MAX_BTDEV_ENTRIES` ceiling, not
# ours. Debian packages no btvirt at all, and a stock build reproduces the
# stall, so a bench provisioned either way measures the emulator's bug and
# calls it a mesh finding. (periculum #49.)
#
# WHAT IT LEAVES BEHIND, AND WHY THE SIDECAR IS NOT OPTIONAL
#
#   /usr/local/bin/btvirt             the patched emulator
#   /usr/local/bin/btvirt.provenance  what it was built from, one line
#
# periculum's ble_room runner prints the sidecar's first non-comment line as
# `origin=` in every run's `BLE_ROOM_BTVIRT` preamble (periculum's
# `periculum/src/ble_room.rs`), and `origin=unrecorded` when there is none. A
# room without it cannot say what it measured on, which after the above is the
# difference between a result and an anecdote. Keep the line's shape: the
# runner quotes at most 160 characters of it.
#
# Idempotent: a host whose installed binary already carries the patch -- by
# md5 against the recorded one, or by the sidecar naming the commit -- is left
# untouched, so `just install-ci` stays a one-command re-run.
#
# Usage:
#   bash scripts/install-btvirt.sh             build + install if needed
#   bash scripts/install-btvirt.sh --check     verify only, build nothing
#   bash scripts/install-btvirt.sh --self-test drive --check against injected
#                                              damage (no network, no build)
#
# Exit 0 = the patched binary is in place (or, in install mode, a prerequisite
# this script may not conjure is missing and it said which). Exit 1 = --check
# found no patched binary, or a build was attempted and failed.
set -uo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"

# Where the binary and its sidecar live. Overridable only so --self-test can
# point the same checks at a scratch directory; nothing in production sets it.
BTVIRT_BIN="${LEVICULUM_BTVIRT_BIN:-/usr/local/bin/btvirt}"
PROVENANCE="$BTVIRT_BIN.provenance"

# The patch, its identity, and the phrase the sidecar must carry. The sha
# lives here rather than being parsed out of the patch file so that a
# hand-edited patch cannot quietly rename what the provenance claims.
PATCH_SHA=4ff7deaf8c908626f71b72880289e51bd4fe081d
PATCH_SHORT=4ff7deaf8c
PATCH_SUBJECT="emulator: use the handle of the receiving side"
PATCH_FILE="$REPO_DIR/scripts/patches/bluez-${PATCH_SHORT}-emulator-receiving-side-handle.patch"

say() { echo "[install-btvirt] $*"; }

# The sidecar's `origin=`: first non-empty, non-comment line, exactly the line
# periculum reads.
origin_line() {
    [ -r "$PROVENANCE" ] || return 1
    local line
    line=$(grep -v '^[[:space:]]*#' "$PROVENANCE" | grep -m1 '[^[:space:]]')
    [ -n "$line" ] || return 1
    echo "$line"
}

# Does this host already hold the patched emulator? Two independent answers,
# either sufficient: the sidecar names the commit, or the binary is
# byte-identical to one a previous run recorded.
provenance_names_patch() {
    local line
    line=$(origin_line) || return 1
    case "$line" in
        *"$PATCH_SHORT"*) return 0 ;;
        *) return 1 ;;
    esac
}

# The md5 the sidecar recorded for the binary it describes, if any.
recorded_md5() {
    local line
    line=$(origin_line) || return 1
    # `..., md5 <32 hex>` -- the tail of the line this script writes.
    echo "$line" | grep -oE 'md5 [0-9a-f]{32}' | head -1 | cut -d' ' -f2
}

installed_md5() {
    [ -x "$BTVIRT_BIN" ] || return 1
    md5sum "$BTVIRT_BIN" | cut -d' ' -f1
}

# --check: the sidecar exists, names the patch, and describes the binary that
# is actually there. A sidecar older than the binary beside it is the case
# periculum reports as `origin_stale=yes` -- somebody reinstalled btvirt and
# left the note behind -- and it is a failure here, because the whole point of
# the file is that it can be believed.
check() {
    if ! [ -x "$BTVIRT_BIN" ]; then
        say "MISSING: no executable $BTVIRT_BIN"
        say "         run: bash scripts/install-btvirt.sh"
        return 1
    fi
    if ! [ -r "$PROVENANCE" ]; then
        say "MISSING: no $PROVENANCE"
        say "         the ble_room cells would run with origin=unrecorded"
        return 1
    fi
    local line
    line=$(origin_line) || {
        say "EMPTY: $PROVENANCE has no non-comment line"
        return 1
    }
    if ! provenance_names_patch; then
        say "UNPATCHED: $PROVENANCE does not name $PATCH_SHORT"
        say "           origin=\"$line\""
        say "           expected: bluez <version> + upstream $PATCH_SHORT ($PATCH_SUBJECT); ..."
        return 1
    fi
    if [ "$PROVENANCE" -ot "$BTVIRT_BIN" ]; then
        say "STALE: $PROVENANCE is older than the binary it describes"
        say "       origin=\"$line\""
        return 1
    fi
    local want have
    want=$(recorded_md5)
    have=$(installed_md5)
    if [ -n "$want" ] && [ "$want" != "$have" ]; then
        say "MISMATCH: $BTVIRT_BIN is md5 $have, the sidecar records $want"
        return 1
    fi
    say "OK: $BTVIRT_BIN"
    say "    origin=\"$line\""
    return 0
}

# Everything below is the install half.

SUDO=""
need_sudo() {
    if [ "$(id -u)" -eq 0 ]; then
        SUDO=""
        return 0
    fi
    if command -v sudo >/dev/null 2>&1; then
        SUDO="sudo"
        return 0
    fi
    return 1
}

install_btvirt() {
    # Idempotence, checked before anything is fetched or compiled.
    if [ -x "$BTVIRT_BIN" ] && provenance_names_patch; then
        local line
        line=$(origin_line)
        say "already patched, nothing to do"
        say "    origin=\"$line\""
        return 0
    fi

    if ! [ -r "$PATCH_FILE" ]; then
        say "ERROR: vendored patch missing: $PATCH_FILE"
        return 1
    fi

    # Prerequisites this script may not conjure. Each one warns with the exact
    # command and returns 0: btvirt is an optional test dependency, and an
    # installer that aborts a whole CI provisioning run over the BLE bench's
    # emulator would be the worse failure.
    if ! need_sudo; then
        say "SKIPPED: not root and no sudo; cannot install into $BTVIRT_BIN"
        return 0
    fi
    for tool in patch make gcc dpkg-parsechangelog; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            say "SKIPPED: '$tool' not found; cannot build btvirt here"
            say "         sudo apt install build-essential dpkg-dev patch"
            return 0
        fi
    done

    local workdir
    workdir=$(mktemp -d /tmp/install-btvirt.XXXXXX) || return 1
    # shellcheck disable=SC2064  # $workdir is fixed now; expand it now too.
    trap "rm -rf '$workdir'" RETURN

    say "fetching the bluez source that matches this host's bluez"
    if ! (cd "$workdir" && apt-get source bluez >"$workdir/apt-source.log" 2>&1); then
        say "SKIPPED: 'apt-get source bluez' failed; see below"
        sed 's/^/[install-btvirt]   /' "$workdir/apt-source.log"
        say "         needs a deb-src line in /etc/apt/sources.list*, then apt-get update"
        return 0
    fi

    local srcdir
    srcdir=$(find "$workdir" -maxdepth 1 -type d -name 'bluez-*' | head -1)
    if [ -z "$srcdir" ]; then
        say "ERROR: apt-get source bluez left no bluez-* directory in $workdir"
        return 1
    fi

    local version
    version=$(cd "$srcdir" && dpkg-parsechangelog -S Version 2>/dev/null)
    [ -n "$version" ] || version="unknown"
    say "source: $(basename "$srcdir") (bluez $version)"

    say "build dependencies (apt, may take a while)"
    DEBIAN_FRONTEND=noninteractive $SUDO apt-get build-dep -y bluez \
        >"$workdir/build-dep.log" 2>&1 || {
        say "WARNING: 'apt-get build-dep bluez' failed; trying the build anyway"
        sed 's/^/[install-btvirt]   /' "$workdir/build-dep.log" | tail -5
    }
    DEBIAN_FRONTEND=noninteractive $SUDO apt-get install -y \
        libreadline-dev python3-docutils >"$workdir/apt-install.log" 2>&1 || true

    say "applying $PATCH_SHORT ($PATCH_SUBJECT)"
    if ! (cd "$srcdir" && patch -p1 --forward <"$PATCH_FILE" >"$workdir/patch.log" 2>&1); then
        say "ERROR: the vendored patch does not apply to bluez $version"
        sed 's/^/[install-btvirt]   /' "$workdir/patch.log"
        say "       refresh scripts/patches/ against this bluez, or pin the source"
        return 1
    fi
    sed 's/^/[install-btvirt]   /' "$workdir/patch.log"

    # Only emulator/btvirt is wanted, so everything that pulls in a daemon,
    # a udev rule or a D-Bus service is configured out.
    say "configure + make emulator/btvirt (minutes)"
    if ! (cd "$srcdir" && ./configure --enable-testing --disable-systemd \
            --disable-cups --disable-obex --disable-hid2hci --disable-mesh \
            --disable-udev >"$workdir/configure.log" 2>&1); then
        say "ERROR: configure failed"
        tail -20 "$workdir/configure.log" | sed 's/^/[install-btvirt]   /'
        return 1
    fi
    if ! (cd "$srcdir" && make -j"$(nproc)" emulator/btvirt >"$workdir/make.log" 2>&1); then
        say "ERROR: make emulator/btvirt failed"
        tail -20 "$workdir/make.log" | sed 's/^/[install-btvirt]   /'
        return 1
    fi

    # Keep whatever was there, once: a bench that wants to reproduce the stall
    # (or bisect it) needs the unpatched binary, and the second run must not
    # overwrite that keepsake with a patched copy.
    if [ -x "$BTVIRT_BIN" ] && [ ! -e "$BTVIRT_BIN.stock.bak" ]; then
        $SUDO cp -p "$BTVIRT_BIN" "$BTVIRT_BIN.stock.bak"
        say "kept the previous binary at $BTVIRT_BIN.stock.bak"
    fi

    $SUDO install -m 755 "$srcdir/emulator/btvirt" "$BTVIRT_BIN" || return 1

    local md5 today
    md5=$(installed_md5)
    today=$(date +%Y-%m-%d)
    # The second line is the one periculum quotes. It stays inside the
    # runner's 160-character window as long as it keeps this shape.
    printf '%s\n%s\n%s\n' \
        "# written by scripts/install-btvirt.sh on $today" \
        "# source: apt-get source bluez ($version), patch -p1 < $(basename "$PATCH_FILE") ($PATCH_SHA)" \
        "bluez $version + upstream $PATCH_SHORT ($PATCH_SUBJECT); built $today, md5 $md5" \
        | $SUDO tee "$PROVENANCE" >/dev/null || return 1
    $SUDO chmod 644 "$PROVENANCE"

    say "installed $BTVIRT_BIN (md5 $md5)"
    check
}

# --self-test: every way the sidecar can lie, injected, with --check asked for
# a verdict on each. A checker nobody has ever made fail is a checker nobody
# knows is wired up -- and this one guards a file a human writes by hand on a
# bench, which is exactly where the lies come from.
self_test() {
    local scratch fails=0
    scratch=$(mktemp -d /tmp/install-btvirt-selftest.XXXXXX) || return 1
    # shellcheck disable=SC2064  # $scratch is fixed now; expand it now too.
    trap "rm -rf '$scratch'" RETURN

    local bin="$scratch/btvirt"
    local note="$scratch/btvirt.provenance"

    # `--check` under this script's own name, pointed at the scratch pair.
    probe() { LEVICULUM_BTVIRT_BIN="$bin" bash "$0" --check >/dev/null 2>&1; }

    expect() {  # expect <want-exit> <case name>
        local want="$1" name="$2" got
        probe
        got=$?
        if [ "$got" -eq "$want" ]; then
            say "  ok   $name (exit $got)"
        else
            say "  FAIL $name: wanted exit $want, got $got"
            fails=$((fails + 1))
        fi
    }

    write_good_note() {
        printf '# a comment the reader must skip\n\nbluez 5.82-1.1 + upstream %s (%s); built 2026-09-27, md5 %s\n' \
            "$PATCH_SHORT" "$PATCH_SUBJECT" "$(md5sum "$bin" | cut -d' ' -f1)" >"$note"
    }

    say "self-test: injecting each way the sidecar can lie"

    expect 1 "no binary at all"

    printf 'not really an emulator\n' >"$bin"
    chmod 755 "$bin"
    expect 1 "binary, no sidecar"

    printf '# only comments\n#\n' >"$note"
    expect 1 "sidecar with no origin line"

    printf 'bluez 5.82 stock\n' >"$note"
    expect 1 "origin line that does not name the patch"

    write_good_note
    expect 0 "patched sidecar describing this binary"

    # Same sidecar, binary replaced underneath it: what periculum reports as
    # origin_stale=yes, and what a reinstall-without-rewrite leaves behind.
    printf 'a different build entirely\n' >"$bin"
    touch "$bin"
    expect 1 "sidecar older than the binary it describes"

    # Freshen the sidecar's mtime but not its content: the recorded md5 is now
    # the only thing left that can tell -- and it must.
    touch "$note"
    expect 1 "sidecar newer than the binary but recording another md5"

    write_good_note
    expect 0 "rewritten sidecar after the reinstall"

    if [ "$fails" -ne 0 ]; then
        say "self-test: $fails case(s) failed"
        return 1
    fi
    say "self-test: all cases behaved"
    return 0
}

case "${1-}" in
    --check)
        check
        exit $?
        ;;
    --self-test)
        self_test
        exit $?
        ;;
    "")
        install_btvirt
        exit $?
        ;;
    *)
        echo "ERROR: unknown flag '$1'" >&2
        echo "Usage: $0 [--check|--self-test]" >&2
        exit 1
        ;;
esac
