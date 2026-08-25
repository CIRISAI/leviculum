#!/usr/bin/env bash
#
# Print what an LNode is saying on its debug CDC port.
#
# Usage: tools/debug-read.sh <debug-port> [seconds]
#   e.g. tools/debug-read.sh /dev/leviculum-debug 20
#        tools/debug-read.sh /dev/leviculum-rak-debug 20 | grep SX_REG
#
# Exit 0 if anything was received, 1 on timeout or an unopenable port.
#
# Why this exists next to lnode-panic-query.sh: that one sends the post-mortem
# trigger, flushes the ring backlog so a stale reply cannot be mistaken for
# its own, and filters to the four panic tags. All three make it the wrong
# tool for a BOOT line — the backlog it drops is exactly where the boot lines
# are. This one asks nothing and filters nothing.
#
# DTR and RTS are asserted on open because the debug CDC transmits only with
# DTR raised (docs/src/concepts/lnode-flashing.md). A port that opens but
# stays silent is almost always this, not a dead board.
#
# Boot lines and the 30-second window: the firmware's boot-critical output
# goes into the 8 KiB LOG_RING whether or not a host is attached, and the ring
# drains once DTR is asserted. Runtime output stays gated for up to 30 s after
# boot (`RUNTIME_DRAIN_OPEN`, leviculum-nrf/src/log.rs) precisely so it cannot
# lap the boot lines before somebody attaches. So: attach within ~30 s of the
# board booting and the boot lines are still there. Later than that, press
# reset with this already running.
#
# Pure python3 stdlib (termios/fcntl), no pyserial — same reason as
# lnode-panic-query.sh: this runs on hosts where the rig venv is not active.

set -euo pipefail

if [ $# -lt 1 ]; then
    echo "usage: $0 <debug-port> [seconds]" >&2
    exit 1
fi

PORT="$1"
SECS="${2:-20}"

python3 - "$PORT" "$SECS" <<'PY'
import sys, os, fcntl, termios, struct, select, time

port, secs = sys.argv[1], float(sys.argv[2])
try:
    fd = os.open(port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
except OSError as e:
    print(f"cannot open {port}: {e}", file=sys.stderr)
    sys.exit(1)

try:
    # 8N1 at 115200, raw. The rate is nominal on a CDC-ACM port, but the raw
    # flags are not: without them the tty layer rewrites CR/LF and echoes.
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
        # No modem-control lines on this fd (a pty, a pipe). Read anyway,
        # same call and same reason as fw-readback.sh: a port that cannot be
        # told to raise DTR is not a reason to report a board as silent, and
        # on a real CDC-ACM this call does not fail.
        pass

    deadline = time.monotonic() + secs
    got = False
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
        got = True
        sys.stdout.write(chunk.decode('utf-8', 'replace').replace('\r', ''))
        sys.stdout.flush()
finally:
    os.close(fd)

if not got:
    print(f"nothing received on {port} in {secs}s", file=sys.stderr)
    sys.exit(1)
PY
