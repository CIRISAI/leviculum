#!/usr/bin/env python3
"""Walk the ThinkNode M9 on the bench without hands (orders 1533, 1540).

The board runs the bench image (`scripts/build-m9-image.sh --bench`, boot
line `[BENCH] inject=on`). The rig folder's proxy (scripts/m9-proxy.py,
its docstring the file protocol; `m9-walk.sh --start-proxy` starts one)
owns the M9's CH340: it appends every byte the board sends to
`<rig>/raw.bin`, writes the hex lines it finds in `<rig>/tx.hex` to the
board, and pulses EN when `<rig>/reset` appears. This walker speaks
through those three files only, so lnsd can keep the node link on the
proxy's pty while a walk runs.

A walk is a text file, one step a line, `#` to the line's end a comment:

    key <key>                 press a key: a name (ring_up ring_down
                              ring_left ring_right enter back chats home
                              map new_message pin space backspace), one
                              character, or a code (0xb6); waits for its
                              KEY line and the frame after it
    hold <key> <ms>           hold a key, answers again every 200 ms
    wait page=<name> [<ms>]   until a [PAGE] line names the page (5000)
    expect <field><op><value> page, cursor, top, zoom against the last
                              [PAGE] line; op one of = != < >
    expect <field> changed    the field moved since the last action
                              (key, hold, dump, reboot, idle, sleep);
                              waits for a [PAGE] line that moves it up to
                              1 s after the action began (1554: the map's
                              zoom is said 397 ms after T, behind the hint
                              row's flush)
    expect log "<words>"      the words in a board line since the last
                              action began
    dump <name>               a FRAME_DUMP of what is shown, decoded to
                              <out>/dumps/<name>.png
    reboot                    EN pulse, wait for the UI, note restore_ms
    idle <s>                  watch the line <s> seconds: rx_lost must
                              not grow
    sleep <ms>                wait
    measure                   the key latencies and the [OPEN] lines so
                              far into walk.md
    bench owe [erase|erase-log|erase-ring]
                              TYPE_BENCH_OWE (1540): the next open writes
                              every chat's row again while the UI core
                              folds; `erase-log` closes the summary log's
                              sector first so the write erases one,
                              `erase-ring` the message ring's (the open's
                              read mark), `erase` both; waits for
                              `[BENCH] owe=armed`
    bench arrive <row>        TYPE_BENCH_ARRIVE (1540): a message comes
                              into the chat at that row of the list
                              (modulo the rows there are), unread, and its
                              chat is selected; waits for
                              `[BENCH] arrive=folded`
    open longest [<rows>]     open each of the list's first rows (8) in
                              turn, back out, and open the one whose
                              [PAGE] top is largest: the chat with the
                              most lines above its screen, whatever the
                              list's order (1542); fails when none has
                              more than one screen (every top 0)
    allow key_ms <ms>         the key bar for the rest of the walk, when
                              the walk asks another question than the
                              latency (hang.walk: Enter on a long chat
                              took 1.7 s in window 10, 1507's to fix)

The walk's verdict is held to the bars of order 1533: key to frame under
100 ms (a KEY line's board time to the next `[DISP] flush` line's t=;
walk.md names that flush's rows=, the first after the key whatever it
draws, 1554; the house key's bar is HOUSE_GAP_MS longer, the time its tap
waits to tell itself from a hold before the overview opens),
a resumed restore under 2 s on every boot after the first, no `[LOOP]`
turn over 250 ms (a dump step's own turn, core 0 writing the dump's lines,
is exempt and named `dump_ms=` in walk.md instead), no `[PREV_RUN]` line that carries a record (a watchdog
or a panic; a power-on reset says `record=none`), rx_lost not growing in
an idle step, a `[BENCH] inject=on` line on every boot (whatever else
the boot says), and every expect and wait met. Output, in --out:

    board.log   every board line the walk saw, host time first
    walk.md     one line per step: page, cursor, top, latency, dump path,
                for an agent to read and judge the dumps by eye
    dumps/      the frame dumps as PNG
    VERDICT     the verdict line, also the last line of walk.md

Usage:
  m9-walk.py --rig DIR --walk FILE [--out DIR] [--boots N]
  m9-walk.py --selftest
"""

import argparse
import os
import re
import shlex
import struct
import sys
import tempfile
import threading
import time
import zlib

FLAG, ESC, XON, XOFF = 0x7E, 0x7D, 0x11, 0x13
TYPE_FRAME_DUMP, TYPE_KEY_INJECT = 0x18, 0x1A
TYPE_BENCH_OWE, TYPE_BENCH_ARRIVE = 0x1B, 0x1C
# leviculum-core/src/envelope.rs, BENCH_OWE_*.
OWE_FLAGS = {None: 0x00, "erase-log": 0x01, "erase-ring": 0x02, "erase": 0x03}
# The steps that do something; `expect ... changed` and `expect log` look
# back to the last of them.
ACTIONS = ("key", "hold", "dump", "reboot", "idle", "sleep", "bench", "open")
KEY_MS_BAR = 100
# loquax-m9/src/house.rs, GAP_MS: a house tap opens the overview only once
# this long passed without another press (1532), so its frame comes after.
HOUSE_GAP_MS = 300
# How long `expect <field> changed` waits for a [PAGE] line that moves the
# field, from the action's start (1554).
CHANGED_WAIT_S = 1.0
RESTORE_MS_BAR = 2000
LOOP_MS_BAR = 250
KEY_WAIT_S = 2.0
READY_WAIT_S = 90.0
DUMP_WAIT_S = 20.0
DUMP_LOOP_WAIT_S = 1.0
BENCH_WAIT_S = 5.0
# The [OPEN] line's fields (1540; marks, sectors, unpack and alloc 1556),
# in its order.
OPEN_FIELDS = ("marks", "sectors", "wait_ms", "page_read_ms", "decode_ms", "unpack_ms", "alloc_ms",
               "fold_ms", "scene_ms", "draw_ms", "flush_ms")

# loquax-m9/src/keyboard.rs, SPECIALS and Key::of.
NAMES = {
    "chats": 0x81, "home": 0x82, "new_message": 0x83, "pin": 0x84,
    "map": 0x85, "back": 0x86, "ring_left": 0xB4, "ring_up": 0xB5,
    "ring_down": 0xB6, "ring_right": 0xB7, "enter": 0x0D,
    "backspace": 0x08, "space": 0x20,
}


VERBS = ("key", "hold", "wait", "expect", "dump", "reboot", "idle", "sleep", "measure", "bench", "allow", "open")
OPEN_ROWS = 8


def parse(steps):
    """The walk's steps as (line, verb, args); a ValueError naming the line
    for one that does not read, before the board is touched."""
    out = []
    for number, raw in enumerate(steps, 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        try:
            verb, *args = shlex.split(line)
            if verb not in VERBS:
                raise ValueError(f"no step {verb!r}")
            if verb in ("key", "hold"):
                key_code(args[0])
            if verb == "hold":
                if not 0 < int(args[1]) <= 10_000:
                    raise ValueError("a hold is 1 to 10000 ms")
            if verb in ("idle", "sleep"):
                float(args[0])
            if verb in ("dump", "wait", "expect") and not args:
                raise ValueError(f"{verb} what?")
            if verb == "bench":
                bench_frame(args)
            if verb == "allow":
                if args[0] != "key_ms" or int(args[1]) <= 0:
                    raise ValueError("allow key_ms <ms>")
            if verb == "open":
                if args[:1] != ["longest"] or len(args) > 2 or (len(args) == 2 and not 0 < int(args[1]) <= 64):
                    raise ValueError("open longest [<rows>], rows 1 to 64")
        except (IndexError, ValueError) as e:
            raise ValueError(f"line {number}: `{line}`: {e}") from None
        out.append((line, verb, args))
    return out


def key_code(word):
    if word in NAMES:
        return NAMES[word]
    if re.fullmatch(r"0x[0-9a-fA-F]{2}", word):
        code = int(word, 16)
    elif len(word) == 1 and 0x20 < ord(word) < 0x7F:
        code = ord(word)
    else:
        raise ValueError(f"no key named {word!r}")
    if code == 0:
        raise ValueError("0 is the idle answer, no key")
    return code


def envelope(frame_type, payload):
    return b"\xa4\xa5" + bytes([frame_type]) + struct.pack(">H", len(payload)) + payload


def wire(data):
    """HDLC with 0x7E/0x7D escaped, then XON/XOFF escaped as the M9's
    one UART asks of every sender (leviculum-nrf/serial-mux)."""
    out = bytearray([FLAG])
    for b in data:
        if b in (FLAG, ESC, XON, XOFF):
            out += bytes([ESC, b ^ 0x20])
        else:
            out.append(b)
    out.append(FLAG)
    return bytes(out)


def key_frame(code, hold_ms=None):
    payload = bytes([code]) if hold_ms is None else bytes([code]) + struct.pack(">H", hold_ms)
    return wire(envelope(TYPE_KEY_INJECT, payload))


def dump_frame():
    return wire(envelope(TYPE_FRAME_DUMP, b""))


def bench_frame(args):
    """A `bench` step's frame; a ValueError for one that does not read."""
    if args[:1] == ["owe"] and len(args) <= 2:
        flag = args[1] if len(args) == 2 else None
        if flag not in OWE_FLAGS:
            raise ValueError(f"bench owe takes erase, erase-log or erase-ring, not {flag!r}")
        flags = OWE_FLAGS[flag]
        return wire(envelope(TYPE_BENCH_OWE, bytes([flags]) if flags else b""))
    if args[:1] == ["arrive"] and len(args) == 2:
        row = int(args[1])
        if not 0 <= row <= 255:
            raise ValueError("a row is 0 to 255")
        return wire(envelope(TYPE_BENCH_ARRIVE, bytes([row])))
    raise ValueError("bench owe [erase|erase-log|erase-ring] or bench arrive <row>")


def fields(line):
    """The key=value words of a board line."""
    return dict(m.groups() for m in re.finditer(r"(\w+)=(\S+)", line))


def png(path, width, height, pixels):
    """RGB565 pixels as an 8-bit RGB PNG, stdlib only."""
    rows = bytearray()
    for y in range(height):
        rows.append(0)
        for v in pixels[y * width:(y + 1) * width]:
            r5, g6, b5 = v >> 11, (v >> 5) & 0x3F, v & 0x1F
            rows += bytes(((r5 << 3) | (r5 >> 2), (g6 << 2) | (g6 >> 4), (b5 << 3) | (b5 >> 2)))

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)))
        f.write(chunk(b"IDAT", zlib.compress(bytes(rows), 6)))
        f.write(chunk(b"IEND", b""))


def dump_parts(lines):
    """The dump's lines out of a capture's: a line the board's ring tore
    (window 13 of 2026-11-08: `PBR 169 0 f79d*320PBR 188 0 f79d*320`, a
    line end dropped and the lines between gone) splits where the next
    PBM/PBR/PBE starts, so the rows that came are kept."""
    return [p for l in lines for p in re.split(r"(?=PB[MRE] )", l) if p[:4] in ("PBM ", "PBR ", "PBE ")]


def torn_lines(w, h, seen):
    """The dump lines a decode lacks, at least: per row, the spans of
    pixels no PBR line carried (a row takes one line or a few, so two lost
    lines of the same row count as one)."""
    n = 0
    for y in range(h):
        row = seen[y * w:(y + 1) * w]
        n += sum(1 for x in range(w) if not row[x] and (x == 0 or row[x - 1]))
    return n


def decode_dump(lines):
    """PBM/PBR/PBE lines (loquax-m9/src/dump.rs) to (w, h, pixels) and
    None, or with why it does not read: `TORN lines=<n>` when rows are
    missing (the pixels then come with gaps, drawn black), the hash's
    words when every pixel came and still does not hash; None for the
    image when there is no PBM or no PBE at all (1553)."""
    w = h = None
    px = seen = None
    for line in dump_parts(lines):
        if line.startswith("PBM "):
            w, h = map(int, line[4:].split()[:2])
            px, seen = [0] * (w * h), [False] * (w * h)
        elif line.startswith("PBR ") and px is not None:
            f = line[4:].split()
            try:
                y, x = int(f[0]), int(f[1])
                runs = [(int(v, 16), int(n or 1)) for v, _, n in (r.partition("*") for r in f[2:])]
            except (ValueError, IndexError):
                continue
            if not 0 <= y < h:
                continue
            for value, n in runs:
                for _ in range(n):
                    if 0 <= x < w:
                        px[y * w + x], seen[y * w + x] = value, True
                    x += 1
        elif line.startswith("PBE ") and px is not None:
            said = line[4:].split()[0]
            lost = torn_lines(w, h, seen)
            if lost:
                return (w, h, px), f"TORN lines={lost}"
            h32 = 0x811C9DC5
            for v in px:
                for b in (v & 0xFF, v >> 8):
                    h32 = ((h32 ^ b) * 0x01000193) & 0xFFFFFFFF
            if said != "%08x" % h32:
                return (w, h, px), f"fnv said {said}, read {h32:08x}"
            return (w, h, px), None
    return None, "no PBE line"


class Rig:
    """The proxy's three files."""

    def __init__(self, rig):
        self.rig = rig
        self.raw = os.path.join(rig, "raw.bin")
        self.tx = os.path.join(rig, "tx.hex")
        self.offset = os.path.getsize(self.raw) if os.path.exists(self.raw) else 0
        self.pending = b""

    def read_lines(self):
        """The board's complete lines since the last read; frames dropped."""
        if not os.path.exists(self.raw):
            return []
        with open(self.raw, "rb") as f:
            f.seek(self.offset)
            data = f.read()
        self.offset += len(data)
        self.pending += data
        *whole, self.pending = self.pending.split(b"\n")
        out = []
        for raw in whole:
            # A frame is followed by a line end before the next line
            # (UartLog::frame); a text line has its 0x7E escaped.
            if FLAG in raw:
                raw = raw[raw.rfind(bytes([FLAG])) + 1:]
            text = raw.strip(b"\r").decode("ascii", "replace")
            if text:
                out.append(text)
        return out

    def send(self, frame):
        """One frame to the board, once the proxy took what it had."""
        deadline = time.monotonic() + 2.0
        while os.path.exists(self.tx) and os.path.getsize(self.tx) > 0 and time.monotonic() < deadline:
            time.sleep(0.02)
        with open(self.tx, "a") as f:
            f.write(frame.hex() + "\n")

    def reset(self):
        open(os.path.join(self.rig, "reset"), "w").close()


class Walk:
    def __init__(self, rig, out, boots):
        self.rig = rig
        self.out = out
        self.boots_asked = boots
        os.makedirs(os.path.join(out, "dumps"), exist_ok=True)
        self.log = open(os.path.join(out, "board.log"), "a")
        self.md = []
        self.fails = []
        self.notes = []
        self.page = {}
        self.page_before = {}
        # Every board line of the walk, and where the last action began:
        # `expect log` and `changed` look back to it.
        self.step_lines = []
        self.action_at = 0
        self.action_t = time.monotonic()
        self.keys = []
        self.restores = []
        self.boots = 0
        self.rx_lost = None
        self.loop_max = 0
        self.key_bar = KEY_MS_BAR
        self.opens = []
        # A dump step's own [LOOP] turn (1542): exempt from the bar while
        # `dumping`, its turn_ms kept here for walk.md.
        self.dumping = False
        self.dump_turn = None

    # ----- the line -----

    def pump(self, timeout=0.0):
        """Read what came, at least once, for up to `timeout` seconds."""
        deadline = time.monotonic() + timeout
        got = []
        while True:
            lines = self.rig.read_lines()
            for line in lines:
                self.seen(line)
            got += lines
            if time.monotonic() >= deadline:
                return got
            time.sleep(0.02)

    def seen(self, line):
        self.log.write("%.3f %s\n" % (time.time(), line))
        self.log.flush()
        self.step_lines.append(line)
        if line.startswith("[PAGE] "):
            self.page = {k: v for k, v in fields(line).items() if k != "t"}
        elif line.startswith("[LOOP] "):
            f = fields(line)
            ms = int(f.get("turn_ms", 0))
            if self.dumping and self.dump_turn is None and f.get("stage") == "take":
                # The dump's turn: core 0 wrote its lines (4.4 to 5 s in
                # window 11a), a cost of the bench's dump, not of the UI.
                self.dump_turn = ms
                return
            self.loop_max = max(self.loop_max, ms)
            if ms > LOOP_MS_BAR:
                self.fail(f"[LOOP] turn_ms={ms} over {LOOP_MS_BAR}: {line}")
        elif line.startswith("[PREV_RUN] ") and "record=none" not in line:
            self.fail(f"the board stopped and restarted: {line}")
        elif line.startswith("[OPEN] "):
            f = fields(line)
            try:
                self.opens.append({k: float(f[k]) for k in OPEN_FIELDS})
            except (KeyError, ValueError):
                self.fail(f"an [OPEN] line that does not read: {line}")
        elif line.startswith("[UART] "):
            self.rx_lost = int(fields(line).get("rx_lost", 0))
        elif line.startswith("[STORE] ") and "restore_ms=" in line:
            f = fields(line)
            self.restores.append((self.boots, f.get("restore", "?"), int(f["restore_ms"])))

    def mark(self):
        """Read what is there; the place in the step's lines after it."""
        self.pump(0.0)
        return len(self.step_lines)

    def until(self, test, timeout, start):
        """The first line from `start` on in the step's lines for which
        `test` holds, pumping for up to `timeout` seconds; with its index
        after it, or (None, start)."""
        deadline = time.monotonic() + timeout
        at = start
        while True:
            while at < len(self.step_lines):
                line = self.step_lines[at]
                at += 1
                if test(line):
                    return line, at
            if time.monotonic() >= deadline:
                return None, at
            self.pump(0.0)
            time.sleep(0.02)

    def fail(self, why):
        self.fails.append(why)

    def row(self, step, extra=""):
        p = self.page
        self.md.append(
            f"| {len(self.md) + 1} | `{step}` | {p.get('name', '-')} | {p.get('cursor', '-')} "
            f"| {p.get('top', '-')} | {p.get('zoom', '')} | {extra} |"
        )

    # ----- the steps -----

    def boot(self, why):
        """An EN pulse and the UI up: the core-0 loop armed and a page."""
        start = self.mark()
        self.rig.reset()
        self.page = {}
        self.boots += 1
        # The UI up: a [PAGE] line, with or without the [WATCHDOG] line
        # before it (an image older than 1534 says none).
        page, _ = self.until(lambda l: l.startswith("[PAGE] "), READY_WAIT_S, start)
        if not page:
            self.fail(f"boot {self.boots} ({why}): no UI within {READY_WAIT_S:.0f} s")
        # On every boot, whatever else it said (1542): the bench image
        # says it before its loop arms, so it is read by now.
        bench = any(l.startswith("[BENCH] inject=on") for l in self.step_lines[start:])
        if not bench:
            self.fail(f"boot {self.boots}: no [BENCH] inject=on line, not the bench image")
        restore = [r for r in self.restores if r[0] == self.boots]
        said = "restore=none"
        if restore:
            _, how, ms = restore[-1]
            said = f"restore={how} restore_ms={ms}"
            if self.boots > 1 and (how != "resumed" or ms >= RESTORE_MS_BAR):
                self.fail(f"boot {self.boots}: {said}, the bar is resumed under {RESTORE_MS_BAR} ms")
        return said

    def key(self, code, hold_ms=None):
        start = self.mark()
        self.rig.send(key_frame(code, hold_ms))
        line, at = self.until(lambda l: l.startswith("KEY ") and f"code={code:#04x}" in l, KEY_WAIT_S, start)
        if not line:
            self.fail(f"key {code:#04x}: no KEY line within {KEY_WAIT_S} s")
            return "no KEY line"
        pressed = int(line.split()[1])
        flush, _ = self.until(lambda l: l.startswith("[DISP] flush rows="), KEY_WAIT_S, at)
        if hold_ms:
            self.pump(hold_ms / 1000 + 0.3)
        self.pump(0.15)
        if not flush:
            self.keys.append((code, None))
            return "no frame"
        ms = int(fields(flush)["t"]) - pressed
        rows = fields(flush).get("rows", "?")
        self.keys.append((code, ms))
        bar = self.key_bar + (HOUSE_GAP_MS if code == NAMES["home"] else 0)
        if ms >= bar:
            self.fail(f"key {code:#04x}: {ms} ms to its frame (rows={rows}), the bar is under {bar}")
        return f"key_ms={ms} rows={rows}"

    def dump(self, name):
        self.dumping, self.dump_turn = True, None
        try:
            said = self.dump_lines(name)
            # The dump's [LOOP] line ends the turn that wrote it.
            if self.dump_turn is None:
                self.until(lambda l: self.dump_turn is not None, DUMP_LOOP_WAIT_S, len(self.step_lines))
        finally:
            self.dumping = False
        if self.dump_turn is not None:
            said += f" dump_ms={self.dump_turn}"
        return said

    def dump_lines(self, name):
        start = self.mark()
        self.rig.send(dump_frame())
        end, _ = self.until(lambda l: l.startswith("PBE "), DUMP_WAIT_S, start)
        if not end:
            self.fail(f"dump {name}: no PBE line within {DUMP_WAIT_S:.0f} s")
            return "no dump"
        image, why = decode_dump(self.step_lines[start:])
        if not image:
            self.fail(f"dump {name}: {why}")
            return f"dump unread: {why}"
        path = os.path.join("dumps", f"{name}.png")
        png(os.path.join(self.out, path), *image)
        self.pump(0.2)
        if why:
            # A torn dump is a named failure, never a crash (1553): the PNG
            # is written with its gaps for the eye.
            self.fail(f"dump {name}: {why}")
            return f"dump {path} {why}"
        return f"dump {path}"

    def bench(self, args):
        """A bench frame (1540) and the board's line that says it took."""
        start = self.mark()
        self.rig.send(bench_frame(args))
        if args[0] == "owe":
            done, refused = "[BENCH] owe=armed", "[BENCH] owe=refused"
        else:
            done, refused = "[BENCH] arrive=folded", "[BENCH] arrive=refused"
        line, _ = self.until(lambda l: l.startswith(done) or l.startswith(refused), BENCH_WAIT_S, start)
        step = " ".join(["bench"] + args)
        if not line:
            self.fail(f"{step}: no {done} line within {BENCH_WAIT_S:.0f} s")
            return f"no {done} line"
        if line.startswith(refused):
            self.fail(f"{step}: {line}")
            return line
        f = fields(line)
        return f"{done.split()[1]} " + " ".join(f"{k}={f[k]}" for k in ("flags", "peer", "unread") if k in f)

    def wait_page(self, name, ms):
        """Until the last [PAGE] line names `name`, for up to `ms`."""
        self.pump(0.0)
        if self.page.get("name") == name:
            return True
        got, _ = self.until(lambda l: l.startswith("[PAGE] ") and fields(l).get("name") == name,
                            ms / 1000, len(self.step_lines))
        return got is not None or self.page.get("name") == name

    def cursor_to(self, row):
        """The list's cursor to `row` by ring presses; False at the
        list's end (a press that moved nothing) or off the list."""
        for _ in range(2 * OPEN_ROWS * 8):
            if self.page.get("name") != "chats":
                return False
            cursor = int(self.page.get("cursor", -1))
            if cursor == row:
                return True
            self.key(NAMES["ring_down"] if cursor < row else NAMES["ring_up"])
            if int(self.page.get("cursor", -1)) == cursor:
                return False
        return False

    def open_longest(self, rows):
        """Open the list's first `rows` chats in turn and the one whose
        [PAGE] top is largest last: the held scrolls after it test
        scrolling, not the list's order (1542)."""
        if not self.wait_page("chats", 5000):
            self.fail(f"open longest: not on the list ({self.page.get('name')})")
            return "not on the list"
        tops = {}
        for row in range(rows):
            if not self.cursor_to(row):
                break
            self.key(NAMES["enter"])
            if not self.wait_page("chat", 5000):
                self.fail(f"open longest: row {row} did not open")
                return f"row {row} did not open"
            self.pump(0.3)
            tops[row] = int(self.page.get("top", 0))
            self.key(NAMES["back"])
            if not self.wait_page("chats", 5000):
                self.fail(f"open longest: no way back to the list from row {row}")
                return f"no way back from row {row}"
        if not tops:
            self.fail("open longest: the list has no row to open")
            return "no rows"
        pick = max(tops, key=lambda r: (tops[r], -r))
        said = "tops " + " ".join(f"{r}:{t}" for r, t in sorted(tops.items()))
        if tops[pick] == 0:
            self.fail(f"open longest: no chat of {len(tops)} has more than one screen ({said})")
            return said
        if not self.cursor_to(pick):
            self.fail(f"open longest: the cursor did not reach row {pick} again")
            return said
        self.page_before = dict(self.page)
        self.key(NAMES["enter"])
        if not self.wait_page("chat", 5000):
            self.fail(f"open longest: row {pick} did not open the second time")
        return f"opened row {pick} top={self.page.get('top')}; {said}"

    def expect(self, words):
        if words[0] == "log":
            text = " ".join(words[1:])
            ok = any(text in l for l in self.step_lines[self.action_at:])
            return ok, f"log has {text!r}"
        if len(words) == 2 and words[1] == "changed":
            name = words[0]
            before = self.page_before.get(name)
            if self.page.get(name) == before:
                # The [PAGE] line that moves it may come after the key's
                # first flush (1554): wait for it, from the action's start.
                left = CHANGED_WAIT_S - (time.monotonic() - self.action_t)
                self.until(lambda l: l.startswith("[PAGE] ") and fields(l).get(name) != before,
                           max(0.0, left), self.action_at)
            ok = self.page.get(name) != before
            return ok, f"{name} {before} -> {self.page.get(name)}"
        m = re.fullmatch(r"(page|name|cursor|top|zoom)(=|!=|<|>)(\S+)", "".join(words))
        if not m:
            raise ValueError(f"expect what? {' '.join(words)}")
        field, op, want = m.groups()
        field = "name" if field == "page" else field
        got = self.page.get(field)
        if op in "<>":
            ok = got is not None and (int(got) < int(want) if op == "<" else int(got) > int(want))
        else:
            ok = (got == want) == (op == "=")
        return ok, f"{field}={got}"

    def run(self, steps):
        self.notes.append(self.boot("before the walk"))
        for n in range(1, self.boots_asked):
            self.notes.append(self.boot(f"boot {n + 1} of {self.boots_asked}"))
        self.row("boot", "; ".join(self.notes))
        for line, verb, args in parse(steps):
            extra = ""
            if verb in ACTIONS:
                self.pump(0.0)
                self.page_before = dict(self.page)
                self.action_at = len(self.step_lines)
                self.action_t = time.monotonic()
            if verb == "key":
                extra = self.key(key_code(args[0]))
            elif verb == "hold":
                extra = self.key(key_code(args[0]), int(args[1]))
            elif verb == "wait":
                name = args[0].removeprefix("page=")
                ms = int(args[1]) if len(args) > 1 else 5000
                self.pump(0.0)
                if self.page.get("name") != name:
                    got, _ = self.until(lambda l: l.startswith("[PAGE] ") and fields(l).get("name") == name,
                                        ms / 1000, self.action_at)
                    if not got:
                        self.fail(f"wait page={name}: still {self.page.get('name')} after {ms} ms")
                extra = f"waited for {name}"
            elif verb == "expect":
                self.pump(0.1)
                ok, said = self.expect(args)
                extra = ("ok " if ok else "FAILED ") + said
                if not ok:
                    self.fail(f"step `{line}`: {said}")
            elif verb == "dump":
                extra = self.dump(args[0])
            elif verb == "reboot":
                extra = self.boot("reboot step")
            elif verb == "idle":
                self.pump(0.1)
                before = self.rx_lost
                self.pump(float(args[0]))
                extra = f"rx_lost {before} -> {self.rx_lost}"
                if before is not None and self.rx_lost is not None and self.rx_lost > before:
                    self.fail(f"idle {args[0]} s: rx_lost grew {before} -> {self.rx_lost}")
                if before is None or self.rx_lost is None:
                    extra += " (no [UART] line to compare)"
            elif verb == "sleep":
                self.pump(int(args[0]) / 1000)
            elif verb == "measure":
                extra = self.latencies() + "; " + self.open_stats()
            elif verb == "bench":
                extra = self.bench(args)
            elif verb == "allow":
                self.key_bar = int(args[1])
                extra = f"key bar {self.key_bar} ms from here"
            elif verb == "open":
                extra = self.open_longest(int(args[1]) if len(args) == 2 else OPEN_ROWS)
            else:
                raise ValueError(f"no step {verb!r}")
            self.row(line, extra)
        return self.verdict()

    def latencies(self):
        ms = sorted(m for _, m in self.keys if m is not None)
        if not ms:
            return "key_ms none"
        return f"key_ms n={len(ms)} median={ms[len(ms) // 2]} max={ms[-1]} no_frame={sum(1 for _, m in self.keys if m is None)}"

    def open_stats(self):
        """The [OPEN] lines so far (1540): each field's median and max."""
        if not self.opens:
            return "opens none"
        out = [f"opens n={len(self.opens)}"]
        for k in OPEN_FIELDS:
            v = sorted(o[k] for o in self.opens)
            out.append(f"{k} median={v[len(v) // 2]:.1f} max={v[-1]:.1f}")
        return " ".join(out)

    def verdict(self):
        verdict = "PASS" if not self.fails else "FAIL"
        line = (f"VERDICT {verdict} steps={len(self.md)} boots={self.boots} {self.latencies()} "
                f"loop_max_ms={self.loop_max} rx_lost={self.rx_lost} opens={len(self.opens)} fails={len(self.fails)}")
        with open(os.path.join(self.out, "walk.md"), "w") as f:
            f.write("# M9 bench walk\n\n")
            f.write("Each dump is for an agent's eye: judge it against the step it follows.\n\n")
            f.write("| # | step | page | cursor | top | zoom | what happened |\n")
            f.write("|---|---|---|---|---|---|---|\n")
            f.write("\n".join(self.md) + "\n\n")
            if self.opens:
                f.write("## Opens\n\n" + self.open_stats() + "\n\n")
            if self.fails:
                f.write("## Failed\n\n" + "".join(f"- {why}\n" for why in self.fails) + "\n")
            f.write(line + "\n")
        with open(os.path.join(self.out, "VERDICT"), "w") as f:
            f.write(line + "\n")
        print(line)
        for why in self.fails:
            print("  " + why)
        return 0 if not self.fails else 1


# ----- the selftest: a board in a thread, behind the proxy's files -----

class FakeBoard(threading.Thread):
    """The proxy and the bench image in one: takes tx.hex, answers into
    raw.bin as the M9 would. `slow_ms` delays every flush, `refuse_owe`
    refuses the bench's owe as a board without a store does, `hang_on`
    is the open after which the board hangs and its watchdog restarts it
    (1540: what hang.walk exists to catch). 1542: `release` boots without
    the bench's and the watchdog's lines, `slow_turn` says a 400 ms
    [LOOP] turn after every key, `tops` is each chat's top when opened
    (the list has as many rows), and every dump ends with its 4400 ms
    [LOOP] turn as the board's does. 1554: the map key opens the map, T
    flushes the hint row at once and says the new zoom 400 ms later (as
    window 13's board did; `stuck_zoom` never moves it), and the house
    key opens the house page after its tap's 300 ms gap."""

    def __init__(self, rig, slow_ms=0, grow_rx=False, refuse_owe=False, hang_on=None,
                 release=False, slow_turn=False, tops=(0, 5, 2), torn=False, stuck_zoom=False):
        super().__init__(daemon=True)
        self.rig, self.slow_ms, self.grow_rx = rig, slow_ms, grow_rx
        self.refuse_owe, self.hang_on = refuse_owe, hang_on
        self.release, self.slow_turn, self.tops = release, slow_turn, tops
        self.torn, self.stuck_zoom = torn, stuck_zoom
        self.zoom = 13
        self.row = 0
        self.page = "chats"
        self.opens = 0
        self.t0 = time.monotonic()
        self.stop = False
        self.cursor = 0
        self.rx_lost = 0
        self.boots = 0

    def ms(self):
        return int((time.monotonic() - self.t0) * 1000)

    def say(self, *lines):
        with open(os.path.join(self.rig, "raw.bin"), "ab") as f:
            for line in lines:
                f.write(line.encode() + b"\r\n")

    def page_line(self):
        top = self.tops[self.row] if self.page == "chat" else max(0, self.cursor - 4)
        zoom = f" zoom={self.zoom}" if self.page == "map" else ""
        self.say(f"[PAGE] name={self.page} cursor={self.cursor} top={top}{zoom} t={self.ms()}")

    def run(self):
        tx, rst = os.path.join(self.rig, "tx.hex"), os.path.join(self.rig, "reset")
        while not self.stop:
            if os.path.exists(rst):
                os.unlink(rst)
                self.boots += 1
                self.cursor = 0
                self.page = "chats"
                restore = "whole" if self.boots == 1 else "resumed"
                self.say("[PREV_RUN] reason=power record=none t=1",
                         f"[STORE] sectors=896 used=3 entries=40 restore={restore} restore_ms=640 t=900")
                if not self.release:
                    self.say("[BENCH] inject=on t=2",
                             "[WATCHDOG] core0=mwdt0 core1=mwdt1 timeout_ms=5000 record=rtc-slow t=950")
                self.page_line()
            if os.path.exists(tx) and os.path.getsize(tx) > 0:
                with open(tx, "r+") as f:
                    lines = f.read().split()
                    f.seek(0)
                    f.truncate()
                for hexline in lines:
                    self.frame(bytes.fromhex(hexline))
            if self.grow_rx:
                self.rx_lost += 1
            self.say(f"[UART] rx_lost={self.rx_lost} overrun=0 t={self.ms()}")
            time.sleep(0.05)

    def frame(self, data):
        body, esc = bytearray(), False
        for b in data[1:-1]:
            if esc:
                body.append(b ^ 0x20)
                esc = False
            elif b == ESC:
                esc = True
            else:
                body.append(b)
        kind, payload = body[2], bytes(body[5:])
        if kind == TYPE_KEY_INJECT:
            at = self.ms()
            code = payload[0]
            self.say(f"KBRAW {at} {code:02x}", f"KEY {at} code={code:#04x} name=x")
            if code == 0xB6 and self.page == "chats":
                self.cursor = min(self.cursor + 1, len(self.tops) - 1)
            elif code == 0xB5 and self.page == "chats":
                self.cursor = max(self.cursor - 1, 0)
            if code == 0x0D and self.page == "chats":
                self.page = "chat"
                self.row = self.cursor
                self.opens += 1
            elif code == 0x86:
                self.page = "chats"
            elif code == 0x85:
                self.page = "map"
            elif code == 0x74 and self.page == "map":
                # The hint row's flush first, the zoom's [PAGE] after.
                self.say(f"[DISP] flush rows=220..239 ms=3.000 t={self.ms() + 20}")
                time.sleep(0.4)
                if not self.stuck_zoom:
                    self.zoom += 1
                self.say(f"[DISP] flush rows=0..239 ms=14.000 t={self.ms()}")
                self.page_line()
                return
            elif code == 0x82:
                # The tap waits out its gap before the overview opens.
                time.sleep(0.3)
                self.page = "house"
            time.sleep(self.slow_ms / 1000)
            self.say(f"[DISP] flush rows=0..239 ms=4.000 t={self.ms() + 20}")
            self.page_line()
            if self.slow_turn:
                self.say("[LOOP] turn_ms=400 stage=draw host=0 node=0 take=0 draw=400 keys=0 entries=40")
            if code == 0x0D and self.page == "chat":
                if self.opens == self.hang_on:
                    # Silent, then the watchdog's reset and its record.
                    time.sleep(0.3)
                    self.page = "chats"
                    self.say("[PREV_RUN] reason=wdt0 core0_stage=rows core0_lock=flash core0_isr=kb "
                             "core1_stage=draw core1_lock=none peer=0a0b0c0d turns=10/12 t=1",
                             "[WATCHDOG] core0=mwdt0 core1=mwdt1 timeout_ms=5000 record=rtc-slow t=950")
                    self.page_line()
                    return
                self.say("[OPEN] peer=0a0b0c0d entries=24 messages=23 marks=1 sectors=6 wait_ms=12.500 "
                         "page_read_ms=9.000 decode_ms=30.250 unpack_ms=10.000 alloc_ms=5.000 "
                         "fold_ms=40.000 scene_ms=20.000 draw_ms=35.000 flush_ms=14.000",
                         "[OPEN_HEAP] peer=0a0b0c0d heap_free=8000000/7990000 heap_largest=7000000/6990000",
                         "[DISP] flush rows=24..239 ms=14.000 t=%d" % self.ms())
        elif kind == TYPE_BENCH_OWE:
            flags = payload[0] if payload else 0
            self.say(f"[BENCH] owe={'refused' if self.refuse_owe else 'armed'} flags={flags:#04x}")
        elif kind == TYPE_BENCH_ARRIVE:
            self.say(f"[BENCH] arrive=sent row={payload[0] % 3} of=3 peer=0a0b0c0d",
                     "[STORE] kept=1 writes=1 erased=0 cursor=40 used=3 entries=41",
                     "[BENCH] arrive=folded peer=0a0b0c0d unread=1")
        elif kind == TYPE_FRAME_DUMP:
            w, h = 4, 4
            rows = [[0xF800, 0x07E0, 0x001F, 0xFFFF], [0, 0, 0x1234, 0x1234],
                    [0xF79D] * 4, [0x0001] * 4]
            h32 = 0x811C9DC5
            for v in (v for row in rows for v in row):
                for b in (v & 0xFF, v >> 8):
                    h32 = ((h32 ^ b) * 0x01000193) & 0xFFFFFFFF
            lines = ["PBR 0 0 f800 07e0 001f ffff", "PBR 1 0 0000*2 1234*2", "PBR 2 0 f79d*4",
                     "PBR 3 0 0001*4"]
            if self.torn:
                # Window 13's tear: row 1's line end dropped, row 2 gone,
                # row 3 glued to row 1's line.
                lines = [lines[0], lines[1] + lines[3]]
            self.say(f"PBM {w} {h}", *lines, f"PBE {h32:08x}",
                     "[LOOP] turn_ms=4400 stage=take host=0 node=0 take=4400 draw=0 keys=0 entries=40")


SELFTEST_WALK = """
# the selftest's walk: a press, a wait, expects, a dump, a reboot
key ring_down
wait page=chats
expect cursor=1
expect cursor changed
expect log "KEY"
dump list
reboot
expect cursor=0
idle 0.4
measure
"""

# 1542: the longest chat opened whatever its row, then a dump of it.
SELFTEST_OPEN_WALK = """
open longest
expect page=chat
expect top=5
dump chat
key back
wait page=chats
"""

# 1554: bench.walk's map and house steps: the zoom said behind the hint
# row's flush, the house page behind its tap's gap.
SELFTEST_MAP_WALK = """
key map
wait page=map
key t
expect zoom changed
key home
wait page=house
key back
wait page=chats
"""

# The bench steps' selftest walk (1540): hang.walk's block, twice.
SELFTEST_HANG_WALK = """
allow key_ms 2000
bench arrive 0
bench owe erase
key enter
wait page=chat
key back
wait page=chats
bench arrive 4
bench owe
key enter
wait page=chat
key back
measure
"""


def selftest():
    rc = 0
    with tempfile.TemporaryDirectory(dir=os.environ.get("M9_WALK_TMP")) as base:
        def walk(name, steps=SELFTEST_WALK, **board):
            rig = os.path.join(base, name)
            os.makedirs(rig)
            open(os.path.join(rig, "raw.bin"), "wb").close()
            fake = FakeBoard(rig, **board)
            fake.start()
            out = os.path.join(rig, "out")
            w = Walk(Rig(rig), out, boots=2)
            got = w.run(steps.splitlines())
            fake.stop = True
            return got, w, out
        got, w, out = walk("good")
        if got != 0:
            print("[m9-walk] selftest FAIL: a good board's walk failed:", w.fails)
            rc = 1
        if not os.path.exists(os.path.join(out, "dumps", "list.png")):
            print("[m9-walk] selftest FAIL: the dump was not written as a PNG")
            rc = 1
        if w.boots != 3 or not any(r[1] == "resumed" for r in w.restores):
            print("[m9-walk] selftest FAIL: the boots and restores were not read", w.boots, w.restores)
            rc = 1
        # The positive controls: a slow frame and a growing rx_lost fail.
        got, w, _ = walk("slow", slow_ms=150)
        if got == 0 or not any("to its frame" in f for f in w.fails):
            print("[m9-walk] selftest FAIL: a 150 ms frame passed the 100 ms bar")
            rc = 1
        got, w, _ = walk("lossy", grow_rx=True)
        if got == 0 or not any("rx_lost grew" in f for f in w.fails):
            print("[m9-walk] selftest FAIL: a growing rx_lost passed the idle step")
            rc = 1
        # 1540: the bench steps pass on a good board and count its opens;
        # a refused owe and a hang on an open fail by name.
        got, w, out = walk("bench", steps=SELFTEST_HANG_WALK)
        if got != 0 or len(w.opens) != 2:
            print("[m9-walk] selftest FAIL: a good board's bench walk failed:", w.fails, len(w.opens))
            rc = 1
        with open(os.path.join(out, "walk.md")) as f:
            if "decode_ms median=30.2" not in f.read():
                print("[m9-walk] selftest FAIL: walk.md has no [OPEN] figures")
                rc = 1
        got, w, _ = walk("noowe", steps=SELFTEST_HANG_WALK, refuse_owe=True)
        if got == 0 or not any("owe=refused" in f for f in w.fails):
            print("[m9-walk] selftest FAIL: a refused owe passed")
            rc = 1
        got, w, _ = walk("hang", steps=SELFTEST_HANG_WALK, hang_on=2)
        if got == 0 or not any("reason=wdt0" in f for f in w.fails):
            print("[m9-walk] selftest FAIL: a hang on an open passed the [PREV_RUN] bar", w.fails)
            rc = 1
        # 1542 (a): a board without the bench's and the watchdog's lines
        # (a release image) fails by name on every boot.
        got, w, _ = walk("release", release=True)
        if got == 0 or sum("no [BENCH] inject=on line" in f for f in w.fails) != 3:
            print("[m9-walk] selftest FAIL: a release boot passed or was not named on every boot", w.fails)
            rc = 1
        # 1542 (b): the dump's own 4400 ms turn is named, not failed (the
        # good walk above passed with it); any other slow turn fails.
        with open(os.path.join(base, "good", "out", "walk.md")) as f:
            if "dump_ms=4400" not in f.read():
                print("[m9-walk] selftest FAIL: walk.md does not name the dump's turn")
                rc = 1
        # 1553: a torn dump fails by name with its lost lines, the walk
        # goes on and the PNG is written with its gaps.
        got, w, out = walk("torn", torn=True)
        if got == 0 or not any("dump list: TORN lines=1" in f for f in w.fails):
            print("[m9-walk] selftest FAIL: a torn dump was not named TORN lines=1", w.fails)
            rc = 1
        if not os.path.exists(os.path.join(out, "dumps", "list.png")):
            print("[m9-walk] selftest FAIL: a torn dump wrote no PNG")
            rc = 1
        if any(f for f in w.fails if "TORN" not in f):
            print("[m9-walk] selftest FAIL: a torn dump failed more than its dump", w.fails)
            rc = 1
        got, w, _ = walk("slowturn", slow_turn=True)
        if got == 0 or not any("[LOOP] turn_ms=400 over" in f for f in w.fails):
            print("[m9-walk] selftest FAIL: a 400 ms turn outside a dump passed the 250 ms bar")
            rc = 1
        # 1542 (c): the longest chat is opened whatever its row; a list
        # of one-screen chats fails by name.
        got, w, out = walk("longest", steps=SELFTEST_OPEN_WALK)
        if got != 0 or len(w.opens) != 4:
            print("[m9-walk] selftest FAIL: open longest did not open row 1 after the three", w.fails, len(w.opens))
            rc = 1
        got, w, _ = walk("short", steps=SELFTEST_OPEN_WALK, tops=(0, 0, 0))
        if got == 0 or not any("more than one screen" in f for f in w.fails):
            print("[m9-walk] selftest FAIL: a list of one-screen chats passed open longest", w.fails)
            rc = 1
        # 1554: the zoom's [PAGE] line 400 ms after the key's first flush
        # is waited for, walk.md names that flush's rows, and the house
        # key's frame after its gap passes; a zoom that never moves fails.
        got, w, out = walk("late", steps=SELFTEST_MAP_WALK)
        if got != 0:
            print("[m9-walk] selftest FAIL: a zoom said behind the hint row's flush failed", w.fails)
            rc = 1
        with open(os.path.join(out, "walk.md")) as f:
            if "rows=220..239" not in f.read():
                print("[m9-walk] selftest FAIL: walk.md does not name the key's flush rows")
                rc = 1
        got, w, _ = walk("stuck", steps=SELFTEST_MAP_WALK, stuck_zoom=True)
        if got == 0 or not any("expect zoom changed" in f for f in w.fails):
            print("[m9-walk] selftest FAIL: a zoom that never moved passed", w.fails)
            rc = 1
    for name in ("bench.walk", "hang.walk"):
        shipped = os.path.join(os.path.dirname(os.path.abspath(__file__)), "m9-walks", name)
        try:
            with open(shipped) as f:
                parse(f.read().splitlines())
        except (OSError, ValueError) as e:
            print(f"[m9-walk] selftest FAIL: the shipped {name} does not read: {e}")
            rc = 1
    for bad in (["bench owe now"], ["bench arrive 256"], ["bench nap"], ["allow key_ms -1"],
                ["open shortest"], ["open longest 0"]):
        try:
            parse(bad)
            print(f"[m9-walk] selftest FAIL: `{bad[0]}` was read")
            rc = 1
        except ValueError:
            pass
    if bench_frame(["owe", "erase-log"]).hex() != "7ea4a51b0001017e":
        print("[m9-walk] selftest FAIL: the owe frame is not the envelope's", bench_frame(["owe", "erase-log"]).hex())
        rc = 1
    if bench_frame(["arrive", "3"]).hex() != "7ea4a51c0001037e":
        print("[m9-walk] selftest FAIL: the arrive frame is not the envelope's")
        rc = 1
    try:
        parse(["key nosuchkey"])
        print("[m9-walk] selftest FAIL: a key nobody names was read")
        rc = 1
    except ValueError:
        pass
    if wire(b"\x11\x7e\x00") != b"\x7e\x7d\x31\x7d\x5e\x00\x7e":
        print("[m9-walk] selftest FAIL: the flow and flag bytes are not escaped")
        rc = 1
    if key_frame(0xB5, 500).hex() != "7ea4a51a0003b501f47e":
        print("[m9-walk] selftest FAIL: the key frame is not the envelope's", key_frame(0xB5, 500).hex())
        rc = 1
    if rc == 0:
        print("[m9-walk] selftest ok: a good walk passes and writes its dump, a slow frame and a growing rx_lost fail; "
              "the bench steps pass and count opens, a refused owe and a hang on an open fail; "
              "a release boot fails by name, the dump's turn is named and another slow turn fails, "
              "open longest picks the longest chat and fails on a list of one-screen chats, "
              "a torn dump is named TORN with its lost lines and still drawn, "
              "a zoom said behind the hint row's flush is waited for and one that never moves fails")
    return rc


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--rig")
    ap.add_argument("--walk")
    ap.add_argument("--out")
    ap.add_argument("--boots", type=int, default=2)
    ap.add_argument("--selftest", action="store_true")
    ap.add_argument("--check", metavar="WALK", help="only read the walk file")
    a = ap.parse_args()
    if a.selftest:
        return selftest()
    if a.check:
        try:
            with open(a.check) as f:
                n = len(parse(f.read().splitlines()))
        except ValueError as e:
            print(f"[m9-walk] {a.check}: {e}")
            return 2
        print(f"[m9-walk] {a.check}: {n} steps read")
        return 0
    if not a.rig or not a.walk:
        ap.error("--rig and --walk, or --selftest")
    out = a.out or os.path.join(a.rig, "walk-" + time.strftime("%Y%m%d-%H%M%S"))
    with open(a.walk) as f:
        steps = f.read().splitlines()
    return Walk(Rig(a.rig), out, a.boots).run(steps)


if __name__ == "__main__":
    sys.exit(main())
