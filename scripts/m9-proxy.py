#!/usr/bin/env python3
"""The ThinkNode M9's bench proxy: owns the board's CH340 tty and shares it.

The reviewer's rig reader from the M9 bench (2026-11-06 to 11-07), shipped
in the tree by order 1542 so a bench without one can start it
(`scripts/m9-walk.sh --start-proxy <dev>`, the periculum cells
`hardware/m9_walk*`). Standard library only.

Usage:
  m9-proxy.py <dev> <baud> <dir> <link>
  m9-proxy.py --selftest      the protocol against a pty, no board

  <dev>   the M9's tty, e.g. /dev/serial/by-id/usb-1a86_USB_Serial-if00-port0
  <baud>  115200 or 230400
  <dir>   the rig folder, the file protocol below lives in it
  <link>  a symlink the proxy points at its pty's slave, so lnsd can run
          the node link on it while every board line is still captured

It reopens <dev> every 0.5 s while the port is gone (a reset, a replug),
with DTR and RTS on. The file protocol in <dir>:

  raw.bin     appended: every byte the board sends, unaltered
  host.bin    appended: every byte the host wrote to the pty
  board.log   appended: the board's lines as text, host time first
              (`HH:MM:SS.mmm <line>`, non-printing bytes as \\xNN, a run
              of more than 1024 bytes without a line end as `[nolf] ...`),
              and the proxy's own notes in the same form:
                PTY <link> -> <slave>        at start
                OPEN <dev> <baud> dtr+rts on the port opened (`dtr+rts
                                             err=<why>` on a tty without
                                             modem lines)
                CLOSE err=<why> total=<n>    the port lost, n bytes read
                TX <hex>                     a tx.hex line written
                HOSTTX n=<n>                 bytes from the pty written
                PTYDROP n=<n> total=<n>      board bytes the pty would
                                             not take (nobody reading)
                RESET pulse (...)            a reset file taken
                RESET err=<why>              its pulse failed
                STOP file seen, exiting
  tx.hex      read and truncated: one hex line per write to the board
              (scripts/m9-walk.py writes its frames here)
  reset       taken and deleted: an EN pulse (DTR off, RTS on for
              150 ms, then RTS off), the board reboots
  STOP        seen: the proxy exits and leaves the file; whoever wrote
              it deletes it (scripts/m9-walk.sh, to flash the board)
"""

import fcntl
import os
import select
import struct
import sys
import termios
import time
import tty

BAUDS = {115200: termios.B115200, 230400: termios.B230400}
TIOCM_DTR, TIOCM_RTS = 0x002, 0x004


def ts():
    t = time.time()
    return time.strftime("%H:%M:%S", time.localtime(t)) + ".%03d" % int((t % 1) * 1000)


def render(b):
    return "".join(chr(c) if 32 <= c < 127 else ("\\r" if c == 13 else "\\x%02x" % c) for c in b)


def open_port(dev, baud):
    fd = os.open(dev, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
    a = termios.tcgetattr(fd)
    a[0] = termios.IGNBRK
    a[1] = 0
    a[2] = termios.CS8 | termios.CREAD | termios.CLOCAL
    a[3] = 0
    a[4] = a[5] = BAUDS[baud]
    a[6][termios.VMIN] = 0
    a[6][termios.VTIME] = 0
    termios.tcsetattr(fd, termios.TCSANOW, a)
    return fd


def modem(fd, bits, on):
    """DTR/RTS on or off; the error's words when the tty has no modem
    lines (a pty, as in the selftest), which the caller notes."""
    try:
        fcntl.ioctl(fd, termios.TIOCMBIS if on else termios.TIOCMBIC, struct.pack("I", bits))
    except OSError as e:
        return str(e)
    return None


def main(argv):
    if len(argv) != 5:
        print("usage: m9-proxy.py <dev> <baud> <dir> <link>", file=sys.stderr)
        return 2
    dev, baud, d, link = argv[1], int(argv[2]), argv[3], argv[4]
    if baud not in BAUDS:
        print(f"m9-proxy.py: baud {baud} is not one of {sorted(BAUDS)}", file=sys.stderr)
        return 2
    raw = open(os.path.join(d, "raw.bin"), "ab")
    hostraw = open(os.path.join(d, "host.bin"), "ab")
    txt = open(os.path.join(d, "board.log"), "a")
    txp = os.path.join(d, "tx.hex")
    rstp = os.path.join(d, "reset")
    stopp = os.path.join(d, "STOP")

    def note(s):
        txt.write("%s %s\n" % (ts(), s))
        txt.flush()

    m, s = os.openpty()
    tty.setraw(s)
    fl = fcntl.fcntl(m, fcntl.F_GETFL)
    fcntl.fcntl(m, fcntl.F_SETFL, fl | os.O_NONBLOCK)
    sname = os.ttyname(s)
    try:
        os.unlink(link)
    except FileNotFoundError:
        pass
    os.symlink(sname, link)
    note("PTY %s -> %s" % (link, sname))
    dropped = 0
    while True:
        try:
            fd = open_port(dev, baud)
        except OSError:
            time.sleep(0.5)
            continue
        err = modem(fd, TIOCM_DTR | TIOCM_RTS, True)
        note("OPEN %s %d dtr+rts %s" % (dev, baud, "on" if err is None else "err=" + err))
        pending = bytearray()
        total = 0
        try:
            while True:
                if os.path.exists(stopp):
                    note("STOP file seen, exiting")
                    os.close(fd)
                    return 0
                if os.path.exists(rstp):
                    os.unlink(rstp)
                    note("RESET pulse (DTR off, RTS on -> EN low; then RTS off)")
                    errs = [modem(fd, TIOCM_DTR, False), modem(fd, TIOCM_RTS, True)]
                    time.sleep(0.15)
                    errs.append(modem(fd, TIOCM_RTS, False))
                    err = next((e for e in errs if e is not None), None)
                    if err is not None:
                        note("RESET err=%s" % err)
                if os.path.exists(txp) and os.path.getsize(txp) > 0:
                    with open(txp, "r+") as f:
                        lines = f.read().split("\n")
                        f.seek(0)
                        f.truncate()
                    for line in lines:
                        line = line.strip()
                        if line:
                            os.write(fd, bytes.fromhex(line))
                            note("TX %s" % line)
                r, _, _ = select.select([fd, m], [], [], 0.2)
                if m in r:
                    try:
                        hb = os.read(m, 4096)
                    except OSError:
                        hb = b""
                    if hb:
                        hostraw.write(hb)
                        hostraw.flush()
                        off = 0
                        while off < len(hb):
                            try:
                                off += os.write(fd, hb[off:])
                            except BlockingIOError:
                                time.sleep(0.002)
                        note("HOSTTX n=%d" % len(hb))
                if fd not in r:
                    continue
                chunk = os.read(fd, 4096)
                if not chunk:
                    raise OSError("eof")
                total += len(chunk)
                raw.write(chunk)
                raw.flush()
                pending += chunk
                try:
                    os.write(m, chunk)
                except OSError:
                    dropped += len(chunk)
                    if dropped < 65536 or dropped % 65536 < len(chunk):
                        note("PTYDROP n=%d total=%d" % (len(chunk), dropped))
                while True:
                    i = pending.find(b"\n")
                    if i < 0:
                        break
                    line = bytes(pending[:i])
                    del pending[:i + 1]
                    txt.write("%s %s\n" % (ts(), render(line)))
                if len(pending) > 1024:
                    txt.write("%s [nolf] %s\n" % (ts(), render(bytes(pending))))
                    pending.clear()
                txt.flush()
        except OSError as e:
            note("CLOSE err=%s total=%d" % (e, total))
            try:
                os.close(fd)
            except OSError:
                pass
            time.sleep(0.5)


def selftest():
    """The proxy as a child on a pty standing in for the CH340: the
    board's bytes reach raw.bin, board.log and the link, tx.hex and the
    link's bytes reach the board, STOP ends it; a wrong command line is
    refused. Under $M9_WALK_TMP when set (the tree's target/tmp)."""
    import subprocess
    import tempfile

    fails = []
    me = os.path.abspath(__file__)

    def wait_for(test, timeout=5.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if test():
                return True
            time.sleep(0.02)
        return False

    def read(path):
        try:
            with open(path, "rb") as f:
                return f.read()
        except FileNotFoundError:
            return b""

    def drain(fd, want, timeout=3.0):
        got = b""
        deadline = time.monotonic() + timeout
        while want not in got and time.monotonic() < deadline:
            r, _, _ = select.select([fd], [], [], 0.05)
            if fd in r:
                try:
                    got += os.read(fd, 4096)
                except OSError:
                    break
        return got

    for bad in ([], ["/dev/null", "9600", ".", "x"]):
        rc = subprocess.run([sys.executable, "-I", me, *bad], capture_output=True).returncode
        if rc != 2:
            fails.append(f"the command line {bad} was not refused with 2 (rc={rc})")

    with tempfile.TemporaryDirectory(dir=os.environ.get("M9_WALK_TMP")) as d:
        board, dev = os.openpty()
        tty.setraw(dev)
        link = os.path.join(d, "m9pty")
        log = os.path.join(d, "board.log")
        child = subprocess.Popen([sys.executable, "-I", me, os.ttyname(dev), "115200", d, link])
        try:
            if not wait_for(lambda: b" OPEN " in read(log)):
                fails.append("no OPEN note in board.log")
            host = os.open(link, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
            tty.setraw(host)
            line = b"[PAGE] name=chats cursor=0 top=0 t=1\r\n"
            os.write(board, line)
            if not wait_for(lambda: line in read(os.path.join(d, "raw.bin"))):
                fails.append("the board's bytes did not reach raw.bin")
            if not wait_for(lambda: b"[PAGE] name=chats cursor=0 top=0 t=1\\r\n" in read(log)):
                fails.append("the board's line did not reach board.log as text")
            if line not in drain(host, line):
                fails.append("the board's bytes did not reach the link")
            with open(os.path.join(d, "tx.hex"), "w") as f:
                f.write("7ea4a51a0001b67e\n")
            if b"\x7e\xa4\xa5\x1a\x00\x01\xb6\x7e" not in drain(board, b"\xb6\x7e"):
                fails.append("a tx.hex line did not reach the board")
            if not wait_for(lambda: b"TX 7ea4a51a0001b67e" in read(log)) or read(os.path.join(d, "tx.hex")):
                fails.append("a tx.hex line was not noted and taken")
            os.write(host, b"lnsd")
            if b"lnsd" not in drain(board, b"lnsd"):
                fails.append("the link's bytes did not reach the board")
            if not wait_for(lambda: b"HOSTTX n=4" in read(log)) or read(os.path.join(d, "host.bin")) != b"lnsd":
                fails.append("the link's bytes were not kept in host.bin and noted")
            open(os.path.join(d, "STOP"), "w").close()
            try:
                rc = child.wait(timeout=3)
                if rc != 0 or b"STOP file seen" not in read(log):
                    fails.append(f"STOP ended the proxy with rc={rc}")
            except subprocess.TimeoutExpired:
                fails.append("the proxy did not stop on STOP")
            os.close(host)
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
            os.close(board)
            os.close(dev)
    for why in fails:
        print("[m9-proxy] selftest FAIL: " + why)
    if not fails:
        print("[m9-proxy] selftest ok: board bytes reach raw.bin, board.log and the link; tx.hex and the "
              "link reach the board; STOP ends it; a wrong command line is refused")
    return 1 if fails else 0


if __name__ == "__main__":
    if sys.argv[1:] == ["--selftest"]:
        sys.exit(selftest())
    sys.exit(main(sys.argv))
