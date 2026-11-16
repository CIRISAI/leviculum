#!/usr/bin/env python3
"""Send one control frame to a single-UART board and log every byte (1238).

The ThinkNode M9 carries debug text and HDLC on one UART behind a CH340
(`leviculum-nrf/serial-mux/src/lib.rs`). This probe opens the tty
exclusively, optionally resets the board first (`espflash reset`, the line
the rig notes use), captures the boot banner, sends exactly the frames it
is told to, and reports what came back and whether the board still talks.

Usage:
  m9-frame-probe.py --selftest
  m9-frame-probe.py --port /dev/ttyUSB1 --log FILE [--reset]
                    [--send cap|env|env-escaped|query|xon ...]
                    [--phy FREQ,BW,SF,CR,TXP,PREAMBLE,CSMA,ST,LT]
                    [--wait 5] [--alive 5]

Every --send is one step: the frame goes out, then the probe waits --wait
seconds and lists the answers. `env` is the enveloped radio config byte for
byte as periculum frames it (HDLC only); `env-escaped` is the same frame
with XON/XOFF escaped as the mux contract asks of both senders; `xon` is a
single raw 0x11. After the last step the probe listens --alive seconds,
asks for the banner (`b`), and says whether any byte arrived.

Every byte read is logged with host time in --log, as hex and as text.
"""

import argparse
import fcntl
import os
import select
import struct
import subprocess
import sys
import termios
import time

FLAG, ESC, XON, XOFF = 0x7E, 0x7D, 0x11, 0x13
MAGIC = b"\xa4\xa5"
TYPE_RADIO_CONFIG, TYPE_CAPABILITIES, TYPE_RADIO_QUERY = 0x01, 0x04, 0x07
NAMES = {0x81: "ACK", 0x82: "REFUSAL", 0x83: "CAPABILITY_REPORT", 0x84: "RADIO_REPORT"}


def envelope(frame_type, payload):
    return MAGIC + bytes([frame_type]) + struct.pack(">H", len(payload)) + payload


def radio_payload(phy):
    # periculum-wire/src/rnode.rs build_radio_config_frame without its magic.
    f, bw, sf, cr, txp, pre, csma, st, lt = phy
    return (struct.pack(">II", f, bw) + bytes([sf, cr, txp & 0xFF])
            + struct.pack(">H", pre) + bytes([csma, 0]) + struct.pack(">HH", st, lt))


def hdlc(data):
    # periculum-wire/src/hdlc.rs frame(): FLAG, 0x7E/0x7D escaped, FLAG.
    out = bytearray([FLAG])
    for b in data:
        if b in (FLAG, ESC):
            out += bytes([ESC, b ^ 0x20])
        else:
            out.append(b)
    out.append(FLAG)
    return bytes(out)


def escape_flow(wire):
    # leviculum-nrf/serial-mux/src/lib.rs escape_flow().
    out = bytearray()
    for b in wire:
        if b in (XON, XOFF):
            out += bytes([ESC, b ^ 0x20])
        else:
            out.append(b)
    return bytes(out)


class Line:
    def __init__(self, port, log):
        self.log = log
        self.fd = os.open(port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
        fcntl.ioctl(self.fd, termios.TIOCEXCL)
        a = termios.tcgetattr(self.fd)
        # Raw, and no host-side IXON/IXOFF: the board's flow bytes are data here.
        a[0] = termios.IGNBRK
        a[1] = 0
        a[2] = termios.CS8 | termios.CREAD | termios.CLOCAL
        a[3] = 0
        a[4] = a[5] = termios.B115200
        a[6][termios.VMIN] = 0
        a[6][termios.VTIME] = 0
        termios.tcsetattr(self.fd, termios.TCSAFLUSH, a)
        self.inframe = False
        self.esc = False
        self.cur = bytearray()
        self.frames = []
        self.text = bytearray()
        self.flow = []
        self.total = 0

    def stamp(self):
        t = time.time()
        return time.strftime("%H:%M:%S", time.localtime(t)) + ".%03d" % int((t % 1) * 1000)

    def note(self, msg):
        line = "%s %s" % (self.stamp(), msg)
        self.log.write(line + "\n")
        self.log.flush()
        print(line)

    def write(self, data, what):
        self.note("TX %s %s" % (what, data.hex(" ")))
        os.write(self.fd, data)

    def pump(self, secs):
        end = time.time() + secs
        got = 0
        while True:
            left = end - time.time()
            if left <= 0:
                return got
            r, _, _ = select.select([self.fd], [], [], min(left, 0.2))
            if not r:
                continue
            try:
                buf = os.read(self.fd, 4096)
            except BlockingIOError:
                continue
            if not buf:
                continue
            got += len(buf)
            self.total += len(buf)
            txt = "".join(chr(c) if 32 <= c < 127 else "." for c in buf)
            self.log.write("%s RX %d %s | %s\n" % (self.stamp(), len(buf), buf.hex(" "), txt))
            self.feed(buf)

    def feed(self, buf):
        for b in buf:
            if b in (XON, XOFF):
                self.flow.append(b)
                continue
            if b == FLAG:
                # The board writes each frame with both flags and text between
                # frames, so a closing flag leaves the frame; a flag right
                # after an opening one is the opening again.
                if self.inframe and self.cur:
                    self.frames.append(bytes(self.cur))
                    self.on_frame(bytes(self.cur))
                    self.inframe = False
                else:
                    self.inframe = True
                self.cur = bytearray()
                self.esc = False
                continue
            if not self.inframe:
                self.text.append(b)
                if b == 10:
                    self.on_text(bytes(self.text))
                    self.text = bytearray()
                continue
            if b == ESC:
                self.esc = True
                continue
            self.cur.append(b ^ 0x20 if self.esc else b)
            self.esc = False

    def on_text(self, line):
        s = line.decode("ascii", "replace").strip()
        for key in ("BOOT_COUNT", "PANIC_COUNT", "FW_BUILD", "[IDENTITY]", "booting", "BOOT_TRACE", "SER:"):
            if key in s:
                self.note("TEXT %s" % s)
                break

    def on_frame(self, data):
        if data[:2] == MAGIC and len(data) >= 5:
            t = data[2]
            n = struct.unpack(">H", data[3:5])[0]
            self.note("FRAME envelope type=0x%02x(%s) len=%d payload=%s"
                      % (t, NAMES.get(t, "?"), n, data[5:].hex(" ")))
        else:
            self.note("FRAME packet len=%d" % len(data))


def reset(port, line_log):
    t0 = time.time()
    r = subprocess.run(["espflash", "reset", "--chip", "esp32s3", "--port", port],
                       capture_output=True, text=True, timeout=60)
    line_log.write("espflash reset rc=%d wall=%.2fs\n%s%s\n"
                   % (r.returncode, time.time() - t0, r.stdout, r.stderr))
    return r.returncode


def open_fast(port, log, within=3.0):
    end = time.time() + within
    while True:
        try:
            return Line(port, log)
        except OSError:
            if time.time() > end:
                raise
            time.sleep(0.002)


def selftest():
    # The levels PHY (periculum runner.rs, the levels round's wire): its
    # 19-byte payload puts a raw XOFF in the length field.
    env = envelope(TYPE_RADIO_CONFIG, radio_payload([869463000, 125000, 8, 5, 22, 18, 1, 0, 0]))
    wire = hdlc(env)
    assert wire[:6] == bytes([FLAG, 0xA4, 0xA5, 0x01, 0x00, XOFF]), wire.hex()
    escaped = escape_flow(wire)
    assert XOFF not in escaped and XON not in escaped
    assert escaped[:7] == bytes([FLAG, 0xA4, 0xA5, 0x01, 0x00, ESC, XOFF ^ 0x20])
    assert len(escaped) == len(wire) + 1
    print("selftest ok")
    return 0


def main():
    if sys.argv[1:] == ["--selftest"]:
        return selftest()
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", default="/dev/ttyUSB1")
    ap.add_argument("--log", required=True)
    ap.add_argument("--reset", action="store_true")
    ap.add_argument("--boot-wait", type=float, default=4.0)
    ap.add_argument("--send", action="append", default=[],
                    choices=["cap", "env", "env-escaped", "query", "xon"])
    ap.add_argument("--phy", default="869463000,125000,8,5,14,18,1,0,1000")
    ap.add_argument("--wait", type=float, default=5.0)
    ap.add_argument("--alive", type=float, default=5.0)
    a = ap.parse_args()
    phy = [int(x) for x in a.phy.split(",")]
    log = open(a.log, "a")
    log.write("=== %s sequence=%s reset=%s phy=%s\n" % (time.ctime(), a.send, a.reset, phy))
    if a.reset:
        if reset(a.port, log) != 0:
            print("RESULT reset_failed")
            return 2
    line = open_fast(a.port, log)
    line.note("OPEN %s" % a.port)
    line.pump(a.boot_wait if a.reset else 1.5)
    env = envelope(TYPE_RADIO_CONFIG, radio_payload(phy))
    for step in a.send:
        frames_before = len(line.frames)
        if step == "cap":
            wire, what = hdlc(envelope(TYPE_CAPABILITIES, b"")), "capability query"
        elif step == "query":
            wire, what = hdlc(envelope(TYPE_RADIO_QUERY, b"")), "radio query"
        elif step == "env":
            wire, what = hdlc(env), "enveloped radio config (as periculum frames it)"
        elif step == "env-escaped":
            wire, what = escape_flow(hdlc(env)), "enveloped radio config, XON/XOFF escaped"
        else:
            wire, what = bytes([XON]), "raw XON"
        raw_flow = [hex(b) for b in wire if b in (XON, XOFF)]
        line.write(wire, "%s raw_flow_bytes=%s" % (what, raw_flow))
        got = line.pump(a.wait)
        line.note("STEP %s bytes_back=%d frames_back=%d" % (step, got, len(line.frames) - frames_before))
    quiet = line.pump(a.alive)
    line.write(b"b", "banner ask")
    asked = line.pump(3.0)
    alive = quiet > 0 or asked > 0
    line.note("RESULT sequence=%s alive=%s listen_bytes=%d banner_bytes=%d board_flow=%s"
              % ("+".join(a.send) or "none", alive, quiet, asked,
                 [hex(b) for b in line.flow]))
    os.close(line.fd)
    return 0


if __name__ == "__main__":
    sys.exit(main())
