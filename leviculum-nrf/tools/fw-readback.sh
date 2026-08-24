# shellcheck shell=bash
# Which board is actually running the image we just wrote.
#
# Sourced by uf2-runner.sh. Defines constants and functions only and runs
# nothing, so tools/test-fw-readback.sh can drive attribution against stubbed
# boards with no hardware, the same way tools/uf2-volumes.sh is driven by
# tools/test-uf2-volumes.sh.
#
# Why it exists. A UF2 mass-storage volume carries no board serial. The runner
# used to pair the volume it found with a candidate from its own enumeration,
# and that candidate was simply a board of the right type — not the board that
# owns the volume. With two T114s attached, one in DFU and one running, it
# wrote the image to the one in DFU and reported the other; measured twice on
# the rig 2026-08-23/24, once in each direction, so it is not a fixed bias but
# whichever candidate enumeration reaches first (Codeberg #343). The same
# missing step made `flash CONFIRMED` mean "a board of this type re-enumerated"
# rather than "the named board runs the named image".
#
# The step that was missing is a read-back. Our firmware prints
# `[FW_BUILD] <stamp>` on the debug CDC at boot and every 5 s, where <stamp> is
# `leviculum_nrf::FW_BUILD_STAMP`; the runner greps the same stamp out of the
# flat image it is about to write. Attribution then follows from what a board
# says about itself, not from the order the bus enumerated it.
#
# Three properties, and the fixture test holds each of them:
#   1. The expected stamp comes from the IMAGE, never from `git rev-parse`.
#      A working tree moves under a cached build; the image does not.
#   2. A board that cannot be read is UNCONFIRMED, which is a different
#      outcome from a board that answers with a different image. Silence must
#      never be reported as the wrong firmware, or as the right one.
#   3. When nothing can be bound to a board, the runner says exactly that.
#      A guess is what produced #343.
#
# The machine-touching calls (the serial read, the debug-port lookup, the
# candidate enumeration) are one-line functions rather than inline commands
# purely so the test can replace them.

# Seconds of debug-serial listening per read attempt. The firmware re-emits the
# banner every 5 s, so one window catches a healthy board; the extra margin
# covers a boot that lands mid-window.
FW_READ_WINDOW="${LEVICULUM_FW_READ_WINDOW:-8}"

# Read windows before a silent board is called UNCONFIRMED. A board that has
# just re-enumerated can lag udev, and a slow boot can miss the first window
# entirely, so the named board gets more than one chance — this is the budget
# that keeps a legitimately slow board from being reported as unreadable.
FW_READ_ATTEMPTS="${LEVICULUM_FW_READ_ATTEMPTS:-3}"

# Read windows spent on the OTHER candidates when the named board is not
# carrying the image. These are boards we are only asking about, not waiting
# for, and each one costs a window; one apiece keeps the worst case bounded.
FW_READ_OTHER_ATTEMPTS="${LEVICULUM_FW_READ_OTHER_ATTEMPTS:-1}"

# --- The image's own build stamp --------------------------------------------

# The stamp carried by an image file, or nothing.
#
# Read out of the image because the alternative — asking git — is wrong in
# exactly the cases that matter. `git rev-parse HEAD` describes the working
# tree at the moment of the question, not the bytes on the way to the board:
# it is right only as long as the build is fresh, and it cannot express a
# dirty tree at all, so two different images built from one commit both answer
# with that commit. `FW_BUILD_STAMP` is compiled into the image, so this grep
# yields the identity of the thing being written and of nothing else.
#
# Args: $1 = image path (the flat binary, not the UF2 — a UF2 interleaves
#            256-byte payloads with block headers, so a literal can straddle
#            a boundary and would not match)
fw_image_stamp() {
    local img="${1:-}"
    [ -n "$img" ] && [ -f "$img" ] || return 0
    LC_ALL=C grep -a -o -E 'git_sha=[0-9A-Za-z_.-]+ dirty=(true|false)' "$img" 2>/dev/null |
        head -1
    return 0
}

# The sha half of a stamp, for messages that talk about shas.
# Args: $1 = stamp
fw_stamp_sha() {
    local s="${1:-}"
    s="${s#git_sha=}"
    printf '%s' "${s%% *}"
}

# --- Machine-touching primitives (the test seams) ---------------------------

# Debug (interface 00) ports of every attached board of the configured
# VID/PID, one "<serial>\t<port>" per line, sorted by serial.
#
# /dev/serial/by-id is preferred over /dev/ttyACM* because the by-id name
# carries the board serial and the interface number, and a ttyACM number
# carries neither — it is a position, and the whole point of this file is not
# to trust a position for an identity. The ttyACM walk stays as a fallback for
# a host without by-id links.
fw_debug_ports() {
    local link port props vid pid iface serial out=""
    for link in /dev/serial/by-id/*-if00; do
        [ -e "$link" ] || continue
        props="$(udevadm info -q property "$link" 2>/dev/null || true)"
        vid="$(printf '%s\n' "$props" | grep '^ID_VENDOR_ID=' | cut -d= -f2)"
        pid="$(printf '%s\n' "$props" | grep '^ID_MODEL_ID=' | cut -d= -f2)"
        [ "$vid" = "$BOARD_VID" ] || continue
        [ "$pid" = "$BOARD_PID" ] || continue
        serial="$(printf '%s\n' "$props" | grep '^ID_SERIAL_SHORT=' | cut -d= -f2)"
        [ -n "$serial" ] || continue
        out="$out$serial	$link"$'\n'
    done
    if [ -z "$out" ]; then
        for port in /dev/ttyACM*; do
            [ -c "$port" ] || continue
            props="$(udevadm info -q property "$port" 2>/dev/null || true)"
            vid="$(printf '%s\n' "$props" | grep '^ID_VENDOR_ID=' | cut -d= -f2)"
            pid="$(printf '%s\n' "$props" | grep '^ID_MODEL_ID=' | cut -d= -f2)"
            iface="$(printf '%s\n' "$props" | grep '^ID_USB_INTERFACE_NUM=' | cut -d= -f2)"
            [ "$vid" = "$BOARD_VID" ] || continue
            [ "$pid" = "$BOARD_PID" ] || continue
            [ "$iface" = "00" ] || continue
            serial="$(printf '%s\n' "$props" | grep '^ID_SERIAL_SHORT=' | cut -d= -f2)"
            [ -n "$serial" ] || continue
            out="$out$serial	$port"$'\n'
        done
    fi
    printf '%s' "$out" | sed '/^$/d' | sort
    return 0
}

# The serials of every board we could read from right now, one per line.
fw_candidate_serials() { fw_debug_ports | cut -f1; }

# This board's debug port, or nothing.
# Args: $1 = serial
fw_debug_port_for_serial() {
    local want="$1" s p
    [ -n "$want" ] || return 0
    while IFS=$'\t' read -r s p; do
        if [ "$s" = "$want" ]; then
            printf '%s' "$p"
            return 0
        fi
    done <<<"$(fw_debug_ports)"
    return 0
}

# The last `[FW_BUILD]` line seen on a debug port within <secs>, or nothing.
#
# DTR and RTS are asserted on open because the debug CDC transmits only with
# them raised: opening the port without them yields silence, and silence read
# as "wrong firmware" would reintroduce the guess this file exists to remove.
# Pure stdlib (termios/fcntl) so no pyserial install is required on the rig.
# Args: $1 = port, $2 = seconds
fw_read_banner() {
    local port="$1" secs="$2"
    python3 - "$port" "$secs" <<'PY'
import sys, os, time, fcntl, termios, struct, select
port, secs = sys.argv[1], float(sys.argv[2])
try:
    fd = os.open(port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
except OSError:
    sys.exit(0)
try:
    iflag, oflag, cflag, lflag, ispeed, ospeed, cc = termios.tcgetattr(fd)
    iflag = oflag = lflag = 0
    cflag = termios.CLOCAL | termios.CREAD | termios.CS8
    ispeed = ospeed = termios.B115200
    termios.tcsetattr(fd, termios.TCSANOW,
                      [iflag, oflag, cflag, lflag, ispeed, ospeed, cc])
    dtr = getattr(termios, 'TIOCM_DTR', 0x002)
    rts = getattr(termios, 'TIOCM_RTS', 0x004)
    try:
        fcntl.ioctl(fd, termios.TIOCMBIS, struct.pack('I', dtr | rts))
    except OSError:
        # No modem-control lines on this fd (a pty, a pipe). Read anyway: a
        # port that cannot be told to raise DTR is not a reason to report a
        # board as silent, and on a real CDC-ACM this call does not fail.
        pass
    deadline = time.monotonic() + secs
    buf, last = b'', ''
    while time.monotonic() < deadline:
        r, _, _ = select.select([fd], [], [], deadline - time.monotonic())
        if not r:
            continue
        try:
            chunk = os.read(fd, 4096)
        except OSError:
            break
        if not chunk:
            continue
        buf += chunk
        while b'\n' in buf:
            line, buf = buf.split(b'\n', 1)
            text = line.decode('utf-8', 'replace').replace('\r', '').strip()
            if 'FW_BUILD' in text:
                last = text
    print(last)
finally:
    os.close(fd)
PY
}

# --- One board --------------------------------------------------------------

# What one board says it is running, as "<state>\t<detail>":
#   match     <stamp>   it carries the image that was written
#   mismatch  <stamp>   it answered, with a different image
#   noanswer  ""        no debug port, or nothing on it
#
# `mismatch` and `noanswer` stay apart all the way to the summary: a crashed
# board and a wrongly-flashed board need different things done to them.
# Args: $1 = serial, $2 = expected stamp, $3 = read attempts
fw_board_state() {
    local serial="$1" want="$2" attempts="${3:-$FW_READ_ATTEMPTS}"
    local port banner="" got n=0

    port="$(fw_debug_port_for_serial "$serial")"
    if [ -z "$port" ]; then
        printf 'noanswer\t\n'
        return 0
    fi
    while [ "$n" -lt "$attempts" ]; do
        banner="$(fw_read_banner "$port" "$FW_READ_WINDOW")"
        [ -n "$banner" ] && break
        n=$((n + 1))
    done
    if [ -z "$banner" ]; then
        printf 'noanswer\t\n'
        return 0
    fi
    got="$(printf '%s\n' "$banner" |
        grep -o -E 'git_sha=[0-9A-Za-z_.-]+ dirty=(true|false)' | head -1)"
    if [ -z "$got" ]; then
        # It spoke, but not a stamp we can compare. That is not silence, and
        # it is not the image we wrote either; report what it actually said.
        printf 'mismatch\t%s\n' "$banner"
        return 0
    fi
    if [ "$got" = "$want" ]; then
        printf 'match\t%s\n' "$got"
    else
        printf 'mismatch\t%s\n' "$got"
    fi
    return 0
}

# --- Attribution ------------------------------------------------------------

# Result of the last fw_attribute. Globals rather than printed values because
# FW_ATTRIBUTED has to survive from one call to the next, and a command
# substitution would drop it. This block is the module's output contract: it
# is written here and read by the caller (uf2-runner.sh, tools/test-*), which
# is why shellcheck sees only assignments.
# shellcheck disable=SC2034
FW_ATTR_OUTCOME=""  # match|rebound|mismatch|noanswer|ambiguous|nostamp
FW_ATTR_SERIAL=""   # the board the image is on, when that is known
FW_ATTR_STAMP=""    # what that board reports
FW_ATTR_MESSAGE=""  # the line to print, already worded

# Serials already bound to this image, "|serial|" joined. A board that has
# been confirmed for an earlier write must not be offered as the recipient of
# a later one — flashing two boards with one image would otherwise make every
# write after the first look ambiguous.
FW_ATTRIBUTED=""

fw_already_attributed() {
    case "$FW_ATTRIBUTED" in
    *"|$1|"*) return 0 ;;
    esac
    return 1
}

fw_mark_attributed() { FW_ATTRIBUTED="$FW_ATTRIBUTED|$1|"; }

# How to describe a board that is not carrying the image.
# Args: $1 = state, $2 = serial, $3 = detail
fw_state_phrase() {
    case "$1" in
    mismatch) printf 'serial=%s reports %s' "$2" "$3" ;;
    *) printf 'serial=%s did not answer on its debug port' "$2" ;;
    esac
}

# Bind a write to a board by reading firmware back, and word the outcome.
#
# The named board is asked first, because in the ordinary single-board flash
# it is the answer and one read ends the matter. Only when it is not carrying
# the image do the other candidates get asked — which is the #343 case, where
# the volume that took the write belonged to a board the runner never named.
#
# Sets FW_ATTR_*. Always returns 0; the outcome is in FW_ATTR_OUTCOME.
# Args: $1 = hint for log lines, $2 = expected stamp, $3 = named serial ("" if
#       the write cannot be associated with any candidate, e.g. the
#       crashed-firmware recovery pass)
fw_attribute() {
    local hint="$1" want="$2" named="${3:-}"
    local state="noanswer" detail="" s st hits="" nhits=0 seen=""

    FW_ATTR_OUTCOME=""
    FW_ATTR_SERIAL=""
    FW_ATTR_STAMP=""
    FW_ATTR_MESSAGE=""

    if [ -z "$want" ]; then
        FW_ATTR_OUTCOME="nostamp"
        FW_ATTR_MESSAGE="$hint: flash UNCONFIRMED — the image carries no [FW_BUILD] stamp, so there is nothing to read back and no board can be shown to have received it"
        return 0
    fi

    if [ -n "$named" ]; then
        IFS=$'\t' read -r state detail <<<"$(fw_board_state "$named" "$want" "$FW_READ_ATTEMPTS")"
        if [ "$state" = "match" ]; then
            fw_mark_attributed "$named"
            FW_ATTR_OUTCOME="match"
            FW_ATTR_SERIAL="$named"
            FW_ATTR_STAMP="$detail"
            FW_ATTR_MESSAGE="$hint: flash CONFIRMED — serial=$named reports $detail, read back from the board, which is the image that was written"
            return 0
        fi
    fi

    # The named board is not carrying it. Ask everyone else: the volume was
    # never bound to a board, so the recipient is whoever says so.
    while IFS= read -r s; do
        [ -n "$s" ] || continue
        [ "$s" = "$named" ] && continue
        fw_already_attributed "$s" && continue
        seen="$seen $s"
        # Only the verdict matters here: we are asking who has the image, not
        # cataloguing what everyone else is running.
        IFS=$'\t' read -r st _ <<<"$(fw_board_state "$s" "$want" "$FW_READ_OTHER_ATTEMPTS")"
        [ "$st" = "match" ] && hits="$hits$s"$'\n'
    done <<<"$(fw_candidate_serials)"

    hits="$(printf '%s' "$hits" | sed '/^$/d')"
    [ -n "$hits" ] && nhits="$(printf '%s\n' "$hits" | wc -l)"

    if [ "$nhits" -eq 1 ]; then
        fw_mark_attributed "$hits"
        FW_ATTR_OUTCOME="rebound"
        FW_ATTR_SERIAL="$hits"
        FW_ATTR_STAMP="$want"
        if [ -n "$named" ]; then
            FW_ATTR_MESSAGE="$hint: flash MIS-ATTRIBUTED — the image $want is on serial=$hits, not on serial=$named ($(fw_state_phrase "$state" "$named" "$detail")); bound by read-back, not by enumeration order"
        else
            FW_ATTR_MESSAGE="$hint: the image $want was received by serial=$hits, bound by read-back"
        fi
        return 0
    fi

    if [ "$nhits" -gt 1 ]; then
        FW_ATTR_OUTCOME="ambiguous"
        FW_ATTR_MESSAGE="$hint: flash UNATTRIBUTED — the image $want was written, and more than one board reports it (serials: $(printf '%s' "$hits" | tr '\n' ' ' | sed 's/ *$//')); the runner does not know which board it wrote"
        return 0
    fi

    if [ -z "$named" ]; then
        FW_ATTR_OUTCOME="ambiguous"
        FW_ATTR_MESSAGE="$hint: flash UNATTRIBUTED — the image $want was written, and no attached board reports it; the runner does not know which board it wrote (asked:${seen:- none})"
        return 0
    fi

    # shellcheck disable=SC2034  # the FW_ATTR_* block is this module's output
    # contract; every read of it is in the caller (uf2-runner.sh, tools/test-*).
    case "$state" in
    mismatch)
        FW_ATTR_OUTCOME="mismatch"
        FW_ATTR_SERIAL="$named"
        FW_ATTR_STAMP="$detail"
        FW_ATTR_MESSAGE="$hint: flash NOT CONFIRMED — serial=$named reports $detail, the image that was written is $want; no other attached board reports it either"
        ;;
    *)
        FW_ATTR_OUTCOME="noanswer"
        FW_ATTR_SERIAL="$named"
        FW_ATTR_STAMP=""
        FW_ATTR_MESSAGE="$hint: flash UNCONFIRMED — serial=$named did not answer on its debug port within $((FW_READ_WINDOW * FW_READ_ATTEMPTS))s, so its firmware could not be read; that is not the same as carrying the wrong image"
        ;;
    esac
    return 0
}
