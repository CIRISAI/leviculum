#!/usr/bin/env python3
"""A scripted walk through Loquax on one adb device (1175, Refs #232).

The walk is what Lew's thumb did when it found the three device bugs of
2026-10-30/31, made a script the reviewer runs every night in Waydroid on
hamster: a mailbox on this host with the 1173 ugly store in its archive,
the x86_64 debug APK installed fresh, the first start, the list, the
imported conversation, Info, a reply, a wake, the map. Every assertion
reads the screen, through `uiautomator dump` texts or the pixels of a
`screencap`, never a timing alone.

Each step prints one line, to stdout and to OUT/walk.log:

    WALK step=<name> ok=<0|1> ms=<n> detail=<...>

A failing step leaves OUT/<step>.png and OUT/<step>.xml (the screen and
its UI dump) and the walk goes on where it can. The last line is

    WALK_SUMMARY ok=<n> failed=<n> ms=<total>

and the exit code is 1 when any step failed.

Usage, on hamster with Waydroid's session up (`adb devices` lists it):

    cargo build -p loquax --features tui --bin loquax
    cargo build -p leviculum-cli --bin lnsd
    (cd loquax-android && make waydroid)
    python3 -I loquax-android/scripts/device-walk.py \\
        --serial 192.168.240.112:5555 --server 192.168.240.1:4242 \\
        --out ~/scratch/walk

docs/src/concepts/loquax-device-walk.md says what it proves and what it
does not, and how to add a step. Standard library only, Python 3.11+.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import re
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import time
import xml.etree.ElementTree as ET
from dataclasses import dataclass, field
from pathlib import Path

PACKAGE = "network.leviculum.loquax.debug"
ACTIVITY = "network.leviculum.loquax.MainActivity"
REPO = Path(__file__).resolve().parents[2]
TARGET = "x86_64-unknown-linux-musl"

STEPS = [
    "fixture",
    "install",
    "first_start",
    "list",
    "open_imported",
    "info",
    "reply_send",
    "receive_wake",
    "receive_report",
    "map",
    "fling",
    "friends",
    "settings",
    "profile",
    "teardown",
]

# The texts the screens show (android/res/values/strings.xml). The walk
# runs the phone in English; a German one shows values-de's.
WHERE_TITLE = "Where should your mailbox be?"
WHERE_COMPUTER = "On a computer at home"
CONTINUE = "Continue"
PASTE_CODE = "Paste a code"
JOIN = "Join"
ACTION_INFO = "Info"
ACTION_REPLY = "Reply"
INFO_TITLE = "Message details"
MAP = "Map"
MORE = "More"
SEND = "Send"
SETTINGS = "Settings"
APPEARANCE = "Appearance"
THEME_LIGHT = "Light"
THEME_DARK = "Dark"
THEME_AUTO = "Like the phone"
# What the map screen lost in 1199: the header's buttons. Their texts and
# their old ids; neither may be on screen.
MAP_CHROME_TEXTS = ["Load area", "All", "\u263e", "\u2600"]
MAP_CHROME_IDS = ["load", "fit", "theme"]
# The tab bar's tabs (activity_conversations.xml): Chats and Map.
TABS = ["tab_chats", "tab_map"]
NOT_DECODED = "did not decode"
# The settings' entries in the list's order (1199, SettingsActivity.PAGES).
SETTINGS_PAGES = [
    "Profile", "Mailbox", "Appearance", "Notifications", "Privacy", "Maps", "Backup", "Advanced",
]
# Each page's mark that it is that page and not a copy of another (1201):
# an id or a text only it shows. Pages with no mark of their own say none.
SETTINGS_PAGE_MARKS = {
    "Profile": ("rid", "profile_qr"),
    "Appearance": ("text", THEME_AUTO),
    "Privacy": ("text", "Positions you shared"),
    "Maps": ("rid", "maps_auto"),
}
# The region the walk puts on the phone for the Maps page (1207): the
# repository's Florence fixture, and the middle the page names it by
# (Ui.place, two decimals, either decimal mark).
FIXTURE_REGION = REPO / "leviculum-map" / "fixtures" / "firenze.lmr"
FIXTURE_REGION_PLACE = re.compile(r"43[.,]\d\d° N, 11[.,]\d\d° E")
# A size as Formatter.formatShortFileSize writes it: "1.2 MB", "345 kB".
FILE_SIZE = re.compile(r"\d+(?:[.,]\d+)?\s?(?:B|kB|KB|MB|GB)\b")
# The buttons of the dialogs a first start may raise: Android's
# notification question, its battery exemption, the app's own (Asks.java).
# "Not now" before "Leave it out": the latter opens a second, system dialog.
DIALOG_BUTTONS = ["Allow", "ALLOW", "Not now"]
# The list's last line for a peer whose position is live (1209, Ui.preview).
LIVE_LINE = "\U0001f4cd live"
# The liveness ring's width as a share of the avatar (AvatarView.RING).
RING = 0.06
# A delivery mark of ours the walk counts as a tick (Ui.state's words).
TICKS = {"sent", "delivered ✓"}


# --- Parsers: what the unit tests under scripts/tests/ pin. ---


@dataclass(frozen=True)
class Node:
    """One node of a UI dump."""

    text: str
    desc: str
    rid: str
    cls: str
    bounds: tuple[int, int, int, int]
    clickable: bool = False
    long_clickable: bool = False
    scrollable: bool = False
    checked: bool = False

    @property
    def center(self) -> tuple[int, int]:
        x1, y1, x2, y2 = self.bounds
        return ((x1 + x2) // 2, (y1 + y2) // 2)

    def is_id(self, name: str) -> bool:
        """Whether the node's resource id is `name` of any package."""
        return self.rid.endswith(":id/" + name)


BOUNDS = re.compile(r"\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]")


def parse_bounds(text: str) -> tuple[int, int, int, int] | None:
    """`[x1,y1][x2,y2]` as uiautomator writes it; None for anything else."""
    m = BOUNDS.fullmatch(text.strip())
    if not m:
        return None
    return tuple(int(g) for g in m.groups())  # type: ignore[return-value]


SURROGATE_PAIR = re.compile(r"&#(5[5-6]\d{3});&#(5[6-7]\d{3});")
SURROGATE = re.compile(r"&#(5[5-7]\d{3});")


def join_surrogates(xml_text: str) -> str:
    """Character references to UTF-16 surrogates made one reference each.

    Android's serializer (KXmlSerializer, under uiautomator dump) writes a
    character outside the BMP, an emoji of a name, as two references to
    its surrogates, which XML forbids and ElementTree refuses. A pair
    becomes the character's reference, a lone one U+FFFD."""

    def pair(m: re.Match) -> str:
        hi, lo = int(m.group(1)), int(m.group(2))
        if 0xD800 <= hi <= 0xDBFF and 0xDC00 <= lo <= 0xDFFF:
            return f"&#{0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)};"
        return m.group(0)

    def lone(m: re.Match) -> str:
        return "&#65533;" if 0xD800 <= int(m.group(1)) <= 0xDFFF else m.group(0)

    return SURROGATE.sub(lone, SURROGATE_PAIR.sub(pair, xml_text))


def parse_dump(xml_text: str) -> list[Node]:
    """The nodes of a `uiautomator dump`, in document order.

    The dump may come with text around the XML (the tool's "UI hierchary
    dumped to" line when it writes to a tty); that is cut away.
    """
    start = xml_text.find("<?xml")
    if start < 0:
        start = xml_text.find("<hierarchy")
    end = xml_text.rfind(">")
    if start < 0 or end < 0:
        return []
    root = ET.fromstring(join_surrogates(xml_text[start : end + 1]))
    nodes = []
    for el in root.iter("node"):
        bounds = parse_bounds(el.get("bounds", "")) or (0, 0, 0, 0)
        nodes.append(
            Node(
                text=el.get("text", ""),
                desc=el.get("content-desc", ""),
                rid=el.get("resource-id", ""),
                cls=el.get("class", ""),
                bounds=bounds,
                clickable=el.get("clickable") == "true",
                long_clickable=el.get("long-clickable") == "true",
                scrollable=el.get("scrollable") == "true",
                checked=el.get("checked") == "true",
            )
        )
    return nodes


def texts(nodes: list[Node]) -> list[str]:
    """Every text and content description on screen, in document order."""
    out = []
    for n in nodes:
        if n.text:
            out.append(n.text)
        if n.desc:
            out.append(n.desc)
    return out


def find(
    nodes: list[Node],
    text: str | None = None,
    desc: str | None = None,
    rid: str | None = None,
    contains: bool = False,
) -> list[Node]:
    """The nodes that match every given criterion."""

    def match(have: str, want: str | None) -> bool:
        if want is None:
            return True
        return want in have if contains else have == want

    return [
        n
        for n in nodes
        if match(n.text, text) and match(n.desc, desc) and (rid is None or n.is_id(rid))
    ]


def by_top(nodes: list[Node]) -> list[Node]:
    """The nodes from the top of the screen down."""
    return sorted(nodes, key=lambda n: (n.bounds[1], n.bounds[0]))


TAB_ID = re.compile(r":id/(tab_[a-z]+)$")


def tab_ids(nodes: list[Node]) -> list[str]:
    """The tab bar's tabs on screen, by id: `tab_chats`, not its icon's
    `tab_chats_icon`."""
    out = []
    for n in nodes:
        m = TAB_ID.search(n.rid)
        if m:
            out.append(m.group(1))
    return out


def map_screen_problems(nodes: list[Node]) -> list[str]:
    """What is wrong with the Map tab's dump (1199): any of the old header
    controls, or a tab bar other than exactly Chats and Map."""
    problems = []
    for text in MAP_CHROME_TEXTS:
        if find(nodes, text=text):
            problems.append(f"text {text!r} on the map screen")
    for rid in MAP_CHROME_IDS:
        if find(nodes, rid=rid):
            problems.append(f"id {rid!r} on the map screen")
    tabs = tab_ids(nodes)
    if tabs != TABS:
        problems.append(f"tabs {tabs}, want {TABS}")
    if not find(nodes, rid="map"):
        problems.append("no map view")
    # The crosshair (1201), the one round button above Friends.
    if not find(nodes, rid="locate"):
        problems.append("no crosshair")
    return problems


def friends_sheet_problems(nodes: list[Node], allowed: set[str]) -> tuple[list[str], list[str]]:
    """The Friends sheet's names from the top down (1200), and what is wrong
    with them: no row at all, a name the fixture gave no position, a name
    twice."""
    names = [n.text for n in by_top(find(nodes, rid="friend_name"))]
    problems = []
    if not names:
        problems.append("no friend in the sheet")
    strangers = [name for name in names if name not in allowed]
    if strangers:
        problems.append(f"names without a position in the fixture: {strangers}")
    twice = sorted({name for name in names if names.count(name) > 1})
    if twice:
        problems.append(f"listed twice: {twice}")
    return names, problems


def divider_above(nodes: list[Node]) -> bool:
    """Whether the sheet's \"Older than a day\" divider stands above its
    first row: the ugly store's positions are all months old (1200)."""
    older = find(nodes, rid="friends_older")
    rows = by_top(find(nodes, rid="friend_name"))
    return bool(older) and bool(rows) and older[0].bounds[1] < rows[0].bounds[1]


HASH_NAME = re.compile(r"<[0-9a-f]{8}…>")
# Reticulum's vocabulary as a main row of the settings must not show it
# (1201): a raw address of 32 hex digits, or one cut to `<8 hex…>`.
RAW_HASH = re.compile(r"[0-9a-f]{32}|<[0-9a-f]{8}…>")


def settings_list_problems(nodes: list[Node]) -> tuple[list[str], list[str]]:
    """The settings list's titles from the top down (1201), and what is
    wrong: not the eight pages in their order, a title without its one
    line description, a raw hash on a main row, a line that did not
    decode."""
    titles = [n.text for n in by_top(find(nodes, rid="settings_entry_title"))]
    summaries = [n.text for n in by_top(find(nodes, rid="settings_entry_summary"))]
    problems = []
    if titles != SETTINGS_PAGES:
        problems.append(f"entries {titles}, want {SETTINGS_PAGES}")
    if len(summaries) != len(titles) or any(not s.strip() for s in summaries):
        problems.append(f"{len(titles)} entries with {len([s for s in summaries if s.strip()])} descriptions")
    for text in titles + summaries:
        if RAW_HASH.search(text):
            problems.append(f"a raw hash on a main row: {text!r}")
    problems += decode_problems(nodes)
    return titles, problems


ADDRESS = re.compile(r"[0-9a-f]{32}")


def profile_problems(nodes: list[Node], want: str | None) -> tuple[str, list[str]]:
    """The profile behind the avatar (1187): the address it shows, and
    what is wrong: no address row or one that is not 32 hex digits, not
    the mailbox's address `want` (when known), no QR code, no Share, a
    line that did not decode."""
    rows = find(nodes, rid="profile_address")
    shown = rows[0].text.strip() if rows else ""
    problems = []
    if not rows:
        problems.append("no address row")
    elif not ADDRESS.fullmatch(shown):
        problems.append(f"the address row shows {shown!r}")
    elif want is not None and shown != want:
        problems.append(f"the address row shows {shown}, the mailbox is {want}")
    if not find(nodes, rid="profile_qr"):
        problems.append("no QR code")
    if not find(nodes, rid="profile_share"):
        problems.append("no Share")
    problems += decode_problems(nodes)
    return shown, problems


def decode_problems(nodes: list[Node]) -> list[str]:
    """A line that says something did not decode, anywhere on screen."""
    return [f"on screen: {t!r}" for t in texts(nodes) if NOT_DECODED in t]


def maps_page_problems(nodes: list[Node]) -> tuple[str, list[str]]:
    """The Maps page (1201, 1207): what it shows, and what is wrong: no
    automatic download switch or one that is off (on by default, 1089,
    and the walk never stores off for good), no storage line or one
    without a size or of 0 B, no region row of the walk's fixture, a
    region without its size, a line that did not decode. Lew's phone
    showed the switch off, "0 B" and no maps, the request having failed
    to decode."""
    switch = find(nodes, rid="maps_auto")
    used = find(nodes, rid="maps_used")
    names = [n.text for n in by_top(find(nodes, rid="maps_region_name"))]
    lines = [n.text for n in by_top(find(nodes, rid="maps_region_line"))]
    problems = []
    if not switch:
        problems.append("no automatic download switch")
    elif not switch[0].checked:
        problems.append("the automatic download shows off")
    used_text = used[0].text if used else ""
    size = FILE_SIZE.search(used_text)
    if not used:
        problems.append("no storage line")
    elif size is None or re.fullmatch(r"0\s?B", size.group(0)):
        problems.append(f"the storage line says {used_text!r}")
    if not any(FIXTURE_REGION_PLACE.search(name) for name in names):
        problems.append(f"no row of the fixture region among {names}")
    if len(lines) != len(names) or any(not FILE_SIZE.search(line) for line in lines):
        problems.append(f"a region without its size: {lines}")
    problems += decode_problems(nodes)
    state = "none" if not switch else "on" if switch[0].checked else "off"
    return f"auto={state} used={size.group(0) if size else '?'} regions={len(names)}", problems


def page_mark_missing(page: str, nodes: list[Node]) -> str | None:
    """What `page` lacks of its own mark (SETTINGS_PAGE_MARKS); None when
    it shows it or has none."""
    mark = SETTINGS_PAGE_MARKS.get(page)
    if mark is None:
        return None
    kind, want = mark
    found = find(nodes, rid=want) if kind == "rid" else find(nodes, text=want)
    return None if found else f"{page} without its {kind} {want!r}"


def is_hash_name(text: str) -> bool:
    """Whether a row's name is the address form `<8 hex…>` (loquax::names)."""
    return HASH_NAME.fullmatch(text) is not None


CODE = re.compile(r"lqxh1[a-z0-9]+(?:@[^\s]+)?")


def parse_code(text: str) -> str | None:
    """The admission code `loquax mailbox admit` printed, with its `@host:port`."""
    found = [m.group(0) for m in CODE.finditer(text)]
    with_server = [c for c in found if "@" in c]
    if with_server:
        return with_server[0]
    return found[0] if found else None


@dataclass(frozen=True)
class Screen:
    """A raw `screencap` (no -p): RGBA rows after a small header."""

    width: int
    height: int
    data: bytes
    offset: int

    def pixel(self, x: int, y: int) -> tuple[int, int, int]:
        x = min(max(x, 0), self.width - 1)
        y = min(max(y, 0), self.height - 1)
        i = self.offset + (y * self.width + x) * 4
        return (self.data[i], self.data[i + 1], self.data[i + 2])


def parse_screencap(data: bytes) -> Screen:
    """A raw screencap: width, height, format (1, RGBA_8888) as u32 LE, and
    from Android 9 on a colour space u32; then the pixels."""
    if len(data) < 12:
        raise ValueError(f"a screencap of {len(data)} bytes has no header")
    width, height, fmt = struct.unpack_from("<III", data, 0)
    if fmt != 1:
        raise ValueError(f"screencap format {fmt}, not RGBA_8888")
    pixels = width * height * 4
    header = len(data) - pixels
    if header not in (12, 16):
        raise ValueError(
            f"screencap {width}x{height}: {len(data)} bytes, header would be {header}"
        )
    return Screen(width, height, data, header)


def sample_grid(
    screen: Screen, box: tuple[int, int, int, int], cols: int = 12, rows: int = 12
) -> list[tuple[int, int, int]]:
    """`cols` x `rows` pixels spread evenly inside `box`, row by row."""
    x1, y1, x2, y2 = box
    out = []
    for r in range(rows):
        y = y1 + (2 * r + 1) * (y2 - y1) // (2 * rows)
        for c in range(cols):
            x = x1 + (2 * c + 1) * (x2 - x1) // (2 * cols)
            out.append(screen.pixel(x, y))
    return out


def is_uniform(samples: list[tuple[int, int, int]], tolerance: int = 6) -> bool:
    """Whether every sample lies within `tolerance` of the first, per channel."""
    if not samples:
        return True
    r0, g0, b0 = samples[0]
    return all(
        abs(r - r0) <= tolerance and abs(g - g0) <= tolerance and abs(b - b0) <= tolerance
        for r, g, b in samples
    )


def black_cells(
    screen: Screen, box: tuple[int, int, int, int], cells: int = 4, per: int = 5, level: int = 12
) -> int:
    """How many of `cells` x `cells` parts of `box` are black throughout:
    every one of their `per` x `per` samples darker than `level` in each
    channel. A map frame has no such part; a tile that failed to draw does."""
    x1, y1, x2, y2 = box
    count = 0
    for cy in range(cells):
        for cx in range(cells):
            cell = (
                x1 + cx * (x2 - x1) // cells,
                y1 + cy * (y2 - y1) // cells,
                x1 + (cx + 1) * (x2 - x1) // cells,
                y1 + (cy + 1) * (y2 - y1) // cells,
            )
            samples = sample_grid(screen, cell, per, per)
            if all(max(p) < level for p in samples):
                count += 1
    return count


def changed_fraction(
    a: list[tuple[int, int, int]], b: list[tuple[int, int, int]], tolerance: int = 16
) -> float:
    """The share of sample pairs that differ by more than `tolerance` in a channel."""
    if not a or len(a) != len(b):
        return 1.0
    changed = sum(
        1 for p, q in zip(a, b) if any(abs(x - y) > tolerance for x, y in zip(p, q))
    )
    return changed / len(a)


def fling_problems(
    early: list[tuple[int, int, int]],
    later: list[tuple[int, int, int]],
    final: list[tuple[int, int, int]],
    least: float = 0.02,
) -> list[str]:
    """The fling step's verdict (1188): the map grabbed early in the coast,
    later in it and at rest must differ pairwise by at least `least` of
    the samples, or the map stood still between them."""
    problems = []
    for name, a, b in (
        ("early=later", early, later),
        ("early=final", early, final),
        ("later=final", later, final),
    ):
        share = changed_fraction(a, b)
        if share < least:
            problems.append(f"{name} ({share:.3f} changed)")
    return problems


def mean_luma(samples: list[tuple[int, int, int]]) -> float:
    """The samples' mean brightness, 0 to 255 (Rec. 601 weights)."""
    if not samples:
        return 0.0
    return sum(0.299 * r + 0.587 * g + 0.114 * b for r, g, b in samples) / len(samples)


def ring_points(
    box: tuple[int, int, int, int], ring: float = RING, count: int = 16
) -> list[tuple[int, int]]:
    """`count` points on the middle of an avatar's liveness ring (1209):
    the circle inside `box`, `ring` of its side wide at the edge."""
    x1, y1, x2, y2 = box
    side = min(x2 - x1, y2 - y1)
    cx, cy = (x1 + x2) / 2, (y1 + y2) / 2
    radius = side / 2 - side * ring / 2
    return [
        (
            round(cx + radius * math.cos(2 * math.pi * i / count)),
            round(cy + radius * math.sin(2 * math.pi * i / count)),
        )
        for i in range(count)
    ]


def conversation_notifications(dumpsys: str, package: str) -> dict[str, str]:
    """The conversation notifications `dumpsys notification --noredact`
    lists for `package`: each record's tag (LoquaxService tags one by the
    peer) to the first time the record names (`mUpdateTimeMs`, `postTime`
    or `when`), so a notification raised again shows as a new time. The
    session's own, untagged, is left out."""
    out: dict[str, str] = {}
    blocks = re.split(r"(?=NotificationRecord\()", dumpsys)
    for block in blocks:
        head = re.match(r"NotificationRecord\([^\n]*", block)
        if not head or f"pkg={package} " not in head.group(0) + " ":
            continue
        tag = re.search(r"\btag=(\S+)", head.group(0))
        if not tag or tag.group(1) == "null":
            continue
        when = re.search(r"\b(?:mUpdateTimeMs|postTime|when)=(\d+)", block)
        out[tag.group(1)] = when.group(1) if when else ""
    return out


def clean_detail(detail: str) -> str:
    """A detail fit for one line: whitespace runs to one space."""
    return " ".join(str(detail).split())


def walk_line(step: str, ok: bool, ms: int, detail: str) -> str:
    """One step's line."""
    return f"WALK step={step} ok={1 if ok else 0} ms={int(ms)} detail={clean_detail(detail)}"


WALK_LINE = re.compile(r"WALK step=(\S+) ok=([01]) ms=(\d+) detail=(.*)")


def parse_walk_line(line: str) -> dict | None:
    """A step's line back as a dict; None for any other line."""
    m = WALK_LINE.fullmatch(line.rstrip("\n"))
    if not m:
        return None
    return {
        "step": m.group(1),
        "ok": m.group(2) == "1",
        "ms": int(m.group(3)),
        "detail": m.group(4),
    }


def summary_line(ok: int, failed: int, ms: int) -> str:
    return f"WALK_SUMMARY ok={ok} failed={failed} ms={int(ms)}"


def input_text_arg(text: str) -> str:
    """`text` for `adb shell input text`: spaces as %s, the shell's
    specials escaped. ASCII only; `input text` types nothing else."""
    out = []
    for ch in text:
        if ord(ch) >= 128:
            raise ValueError(f"input text types ASCII only, not {ch!r}")
        if ch == " ":
            out.append("%s")
        elif ch.isalnum() or ch in "._-@:/,+=":
            out.append(ch)
        else:
            out.append("\\" + ch)
    return "".join(out)


# --- The device. ---


class StepFailed(Exception):
    """A step's assertion failed; the message is its detail."""


class Device:
    """One adb device, by serial."""

    def __init__(self, adb: str, serial: str):
        self.adb = adb
        self.serial = serial

    def run(self, *args: str, timeout: float = 60, check: bool = True) -> bytes:
        cmd = [self.adb, "-s", self.serial, *args]
        proc = subprocess.run(cmd, capture_output=True, timeout=timeout)
        if check and proc.returncode != 0:
            raise StepFailed(
                f"{' '.join(args[:3])} exit={proc.returncode} "
                f"{proc.stderr.decode(errors='replace').strip()[:300]}"
            )
        return proc.stdout

    def shell(self, command: str, timeout: float = 60, check: bool = True) -> str:
        return self.run("shell", command, timeout=timeout, check=check).decode(errors="replace")

    def dump(self) -> list[Node]:
        out = self.shell(
            "uiautomator dump /sdcard/walk-dump.xml >/dev/null && cat /sdcard/walk-dump.xml",
            timeout=30,
        )
        return parse_dump(out)

    def dump_xml(self) -> str:
        return self.shell(
            "uiautomator dump /sdcard/walk-dump.xml >/dev/null && cat /sdcard/walk-dump.xml",
            timeout=30,
            check=False,
        )

    def screen(self) -> Screen:
        return parse_screencap(self.run("exec-out", "screencap", timeout=30))

    def screenshot_png(self, path: Path) -> None:
        path.write_bytes(self.run("exec-out", "screencap", "-p", timeout=30, check=False))

    def tap(self, x: int, y: int) -> None:
        self.shell(f"input tap {x} {y}")

    def tap_node(self, node: Node) -> None:
        self.tap(*node.center)

    def long_press(self, node: Node, ms: int = 900) -> None:
        x, y = node.center
        self.shell(f"input swipe {x} {y} {x} {y} {ms}")

    def swipe(self, x1: int, y1: int, x2: int, y2: int, ms: int = 300) -> None:
        self.shell(f"input swipe {x1} {y1} {x2} {y2} {ms}")

    def double_tap(self, x: int, y: int) -> None:
        # One shell, the two taps back to back: the map zooms on a double
        # tap (MapView.onDoubleTap), the gesture `input` can make; a pinch
        # needs two pointers, which `input` has not.
        self.shell(f"input tap {x} {y}; input tap {x} {y}")

    def type_text(self, text: str) -> None:
        self.shell(f"input text {input_text_arg(text)}")

    def key(self, name: str) -> None:
        self.shell(f"input keyevent {name}")


# --- The host's mailbox. ---


@dataclass
class Fixture:
    """lnsd with a TCP server, `loquax mailbox run` over the ugly store,
    and the admission code for one device."""

    root: Path
    cfg: Path
    mailbox: Path
    sender: Path
    manifest: dict
    code: str
    procs: list = field(default_factory=list)
    logs: dict = field(default_factory=dict)

    def mailbox_log(self) -> str:
        path = self.logs.get("mailbox")
        return path.read_text(errors="replace") if path and path.exists() else ""

    def stop(self) -> None:
        for proc in reversed(self.procs):
            if proc.poll() is None:
                proc.send_signal(signal.SIGTERM)
        for proc in reversed(self.procs):
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=5)


def lnsd_config(
    instance: str,
    listen_ip: str,
    port: int,
    mode: str | None = None,
    link_keepalive: int | None = None,
) -> str:
    """The fixture's config. `mode` is the TCP server's interface mode
    (`access_point` keeps announces off the device's connection) and
    `link_keepalive` the `keepalive_interval` in seconds of every link
    the host's side holds; both are the power proxy's levers (1177)."""
    keepalive = f"  keepalive_interval = {link_keepalive}\n" if link_keepalive else ""
    server_mode = f"    mode = {mode}\n" if mode else ""
    return f"""[reticulum]
  enable_transport = True
  share_instance = Yes
  instance_name = {instance}
  panic_on_interface_error = No
{keepalive}
[logging]
  loglevel = 4

[interfaces]
  [[Walk TCP Server]]
    type = TCPServerInterface
    enabled = yes
    listen_ip = {listen_ip}
    listen_port = {port}
{server_mode}"""


def wait_for(predicate, timeout: float, every: float = 0.25):
    """`predicate()`'s first truthy value within `timeout` seconds, else None."""
    deadline = time.monotonic() + timeout
    while True:
        value = predicate()
        if value:
            return value
        if time.monotonic() >= deadline:
            return None
        time.sleep(every)


def port_open(port: int) -> bool:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=0.5):
            return True
    except OSError:
        return False


# --- The walk. ---


class Walk:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.out = Path(args.out).resolve()
        self.out.mkdir(parents=True, exist_ok=True)
        self.log = (self.out / "walk.log").open("w", encoding="utf-8")
        self.device = Device(args.adb, args.serial) if args.serial else None
        self.fixture: Fixture | None = None
        self.installed = False
        self.joined = False
        self.ok = 0
        self.failed = 0
        self.nonce = str(int(time.time()))[-6:]

    def emit(self, line: str) -> None:
        print(line, flush=True)
        self.log.write(line + "\n")
        self.log.flush()

    def note(self, text: str) -> None:
        """A line for the log only: what a step saw, for the reviewer."""
        self.log.write("  " + clean_detail(text) + "\n")
        self.log.flush()

    def need_device(self) -> Device:
        if self.device is None:
            raise StepFailed("no --serial: this step drives a device")
        return self.device

    def need_fixture(self) -> Fixture:
        if self.fixture is None:
            raise StepFailed("no fixture: its step failed or did not run")
        return self.fixture

    def run_step(self, name: str) -> bool:
        start = time.monotonic()
        try:
            detail = getattr(self, "step_" + name)()
            ok = True
        except StepFailed as e:
            detail, ok = str(e), False
        except (subprocess.SubprocessError, OSError, ValueError, ET.ParseError) as e:
            detail, ok = f"{type(e).__name__}: {e}", False
        ms = int((time.monotonic() - start) * 1000)
        if not ok:
            self.evidence(name)
        self.emit(walk_line(name, ok, ms, detail or ""))
        if ok:
            self.ok += 1
        else:
            self.failed += 1
        return ok

    def evidence(self, name: str) -> None:
        """The screen and its UI dump beside the log, for a failed step."""
        if self.device is None:
            return
        try:
            self.device.screenshot_png(self.out / f"{name}.png")
            (self.out / f"{name}.xml").write_text(self.device.dump_xml(), encoding="utf-8")
        except (subprocess.SubprocessError, OSError, StepFailed) as e:
            self.note(f"{name}: no evidence: {e}")

    # The helpers that read the screen.

    def wait_text(self, want: str, timeout: float, contains: bool = False) -> list[Node]:
        dev = self.need_device()
        seen: list[Node] = []

        def look():
            nonlocal seen
            seen = dev.dump()
            return find(seen, text=want, contains=contains) or find(
                seen, desc=want, contains=contains
            )

        found = wait_for(look, timeout, every=0.2)
        if not found:
            raise StepFailed(f"no {want!r} within {timeout:.0f}s; screen: {texts(seen)[:12]}")
        return found

    def wait_id(self, rid: str, timeout: float) -> list[Node]:
        dev = self.need_device()
        seen: list[Node] = []

        def look():
            nonlocal seen
            seen = dev.dump()
            return find(seen, rid=rid)

        found = wait_for(look, timeout, every=0.2)
        if not found:
            raise StepFailed(f"no view {rid!r} within {timeout:.0f}s; screen: {texts(seen)[:12]}")
        return found

    def tap_text(self, want: str, timeout: float = 10) -> None:
        self.need_device().tap_node(self.wait_text(want, timeout)[0])

    def dismiss_dialogs(self, rounds: int = 3) -> list[str]:
        """Tap away the notification and battery questions, if they show."""
        dev = self.need_device()
        tapped = []
        for _ in range(rounds):
            nodes = dev.dump()
            hit = None
            for label in DIALOG_BUTTONS:
                buttons = [n for n in find(nodes, text=label) if n.clickable or "Button" in n.cls]
                if buttons:
                    hit = (label, buttons[0])
                    break
            if hit is None:
                break
            tapped.append(hit[0])
            dev.tap_node(hit[1])
            time.sleep(0.8)
        return tapped

    def scroll_list(self, nodes: list[Node], rid: str, up: bool) -> None:
        dev = self.need_device()
        lists = find(nodes, rid=rid)
        if not lists:
            raise StepFailed(f"no list {rid!r} on screen")
        x1, y1, x2, y2 = lists[0].bounds
        x = (x1 + x2) // 2
        top, bottom = y1 + (y2 - y1) // 5, y2 - (y2 - y1) // 5
        if up:
            dev.swipe(x, top, x, bottom, 250)
        else:
            dev.swipe(x, bottom, x, top, 250)
        time.sleep(0.5)

    def map_box(self, nodes: list[Node]) -> tuple[int, int, int, int]:
        maps = find(nodes, rid="map")
        if not maps:
            raise StepFailed(f"no map view on screen: {texts(nodes)[:12]}")
        return maps[0].bounds

    # The steps.

    def step_fixture(self) -> str:
        args = self.args
        loquax = Path(args.loquax)
        lnsd = Path(args.lnsd)
        for binary in (loquax, lnsd):
            if not binary.is_file():
                raise StepFailed(f"{binary} missing; see the docstring's cargo build lines")
        probe = subprocess.run(
            [str(loquax), "mailbox", "--help"], capture_output=True, timeout=30
        )
        if probe.returncode != 0:
            raise StepFailed(f"{loquax} has no mailbox: build it with --features tui")
        host, _, port_text = args.server.rpartition(":")
        if not host or not port_text.isdigit():
            raise StepFailed(f"--server {args.server!r} is not host:port")
        port = int(port_text)
        root = Path(tempfile.mkdtemp(prefix="fixture-", dir=self.out))
        cfg, mailbox, sender = root / "cfg", root / "mailbox", root / "sender"
        cfg.mkdir()
        manifest_path = root / "manifest.json"
        fill = (
            [args.walk_mailbox]
            if args.walk_mailbox
            else [
                "cargo",
                "run",
                "--quiet",
                "--manifest-path",
                str(REPO / "Cargo.toml"),
                "-p",
                "loquax-android",
                "--example",
                "walk_mailbox",
                "--",
            ]
        )
        filled = subprocess.run(
            [*fill, str(mailbox), str(sender), str(manifest_path)],
            capture_output=True,
            timeout=480,
        )
        if filled.returncode != 0:
            raise StepFailed(f"walk_mailbox: {filled.stderr.decode(errors='replace')[-400:]}")
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        (cfg / "config").write_text(
            lnsd_config(
                f"walk{os.getpid()}", args.listen, port, args.server_mode, args.link_keepalive
            ),
            encoding="utf-8",
        )
        fixture = Fixture(root, cfg, mailbox, sender, manifest, code="")
        self.fixture = fixture
        env_mailbox = {**os.environ, "LNMSG_HOME": str(mailbox)}
        fixture.logs["lnsd"] = root / "lnsd.log"
        fixture.procs.append(
            subprocess.Popen(
                [str(lnsd), "--config", str(cfg)],
                stdout=fixture.logs["lnsd"].open("wb"),
                stderr=subprocess.STDOUT,
            )
        )
        if not wait_for(lambda: port_open(port), 20):
            raise StepFailed(f"lnsd's TCP server on {port} did not open (lnsd.log)")
        fixture.logs["mailbox"] = root / "mailbox.log"
        fixture.procs.append(
            subprocess.Popen(
                [str(loquax), "mailbox", "run", "--config", str(cfg), "--sync-interval", "off"],
                env={**env_mailbox, "NO_COLOR": "1"},
                stdout=fixture.logs["mailbox"].open("wb"),
                stderr=subprocess.STDOUT,
            )
        )
        if not wait_for(lambda: "HOME up address=" in fixture.mailbox_log(), 30):
            raise StepFailed(f"the mailbox did not come up: {fixture.mailbox_log()[-400:]}")
        admit = subprocess.run(
            [str(loquax), "mailbox", "admit", "--name", "walk", "--server", args.server],
            env=env_mailbox,
            capture_output=True,
            timeout=30,
        )
        code = parse_code(admit.stdout.decode(errors="replace"))
        if admit.returncode != 0 or not code:
            raise StepFailed(f"admit exit={admit.returncode}: {admit.stderr.decode()[-300:]}")
        fixture.code = code
        (root / "code.txt").write_text(code + "\n", encoding="utf-8")
        return (
            f"address={manifest['address']} messages={manifest['messages']} "
            f"imported={manifest['imported']} server={args.server} code={code[:12]}…"
        )

    def step_install(self) -> str:
        dev = self.need_device()
        apk = Path(self.args.apk)
        if not apk.is_file():
            raise StepFailed(f"{apk} missing: make waydroid in loquax-android/")
        # Cleared first, so the first start is a first start; then -r -g:
        # replace, and grant the run time permissions, which are not under
        # test here (the dialogs are dismissed should they still show).
        dev.shell(f"pm clear {PACKAGE}", check=False)
        dev.run("install", "-r", "-g", str(apk), timeout=180)
        self.installed = True
        dev.shell(f"pm clear {PACKAGE}", check=False)
        listed = dev.shell(f"pm list packages {PACKAGE}")
        if f"package:{PACKAGE}" not in listed.split():
            raise StepFailed(f"{PACKAGE} not installed: {listed.strip()}")
        return f"apk={apk.name} bytes={apk.stat().st_size}"

    def step_first_start(self) -> str:
        dev = self.need_device()
        fixture = self.need_fixture()
        dev.shell(f"am start -W -n {PACKAGE}/{ACTIVITY}", timeout=60)
        self.wait_text(WHERE_TITLE, 20)
        self.tap_text(WHERE_COMPUTER)
        self.tap_text(CONTINUE)
        self.tap_text(PASTE_CODE)
        dev.tap_node(self.wait_id("code", 10)[0])
        dev.type_text(fixture.code)
        nodes = dev.dump()
        typed = find(nodes, rid="code")
        if not typed or typed[0].text.strip() != fixture.code:
            got = typed[0].text if typed else None
            raise StepFailed(f"the code field holds {got!r}, not the code")
        if not find(nodes, text=JOIN):
            # The keyboard covers the button: Back closes the keyboard only.
            dev.key("KEYCODE_BACK")
        self.tap_text(JOIN)
        self.joined = True
        tapped = []
        # The list's title is the app's name; the join leads there.
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            tapped += self.dismiss_dialogs(1)
            nodes = dev.dump()
            if find(nodes, rid="list") and find(nodes, rid="write"):
                return f"joined dialogs={tapped or 'none'}"
            time.sleep(0.5)
        raise StepFailed(f"no conversation list 30s after Join; screen: {texts(nodes)[:12]}")

    def expected_rows(self) -> list[dict]:
        """The conversations the list shows: neither archived nor blocked."""
        convs = self.need_fixture().manifest["conversations"]
        return [c for c in convs if not c["archived"] and not c["blocked"]]

    def step_list(self) -> str:
        dev = self.need_device()
        expected = self.expected_rows()
        want = {c["shown"] for c in expected}
        named = {c["shown"] for c in expected if c["named"]}
        # Within 10 s and without a restart: the rows' names come with the
        # pull, not with the next start (one of the 2026-10-30/31 bugs).
        start = time.monotonic()

        def first_names():
            return [n.text for n in find(dev.dump(), rid="name") if n.text in named]

        if not wait_for(first_names, 10, every=0.3):
            nodes = dev.dump()
            raise StepFailed(
                f"no named row within 10s; names: {[n.text for n in find(nodes, rid='name')]}"
            )
        first_ms = int((time.monotonic() - start) * 1000)
        seen: list[str] = []
        stale = 0
        for _ in range(12):
            nodes = dev.dump()
            new = [n.text for n in by_top(find(nodes, rid="name")) if n.text not in seen]
            seen += new
            if want <= set(seen):
                break
            stale = stale + 1 if not new else 0
            if stale >= 2:
                break
            self.scroll_list(nodes, "list", up=False)
        self.note(f"list rows seen: {seen}")
        missing = sorted(want - set(seen))
        hashed_named = [
            c["shown"] for c in expected if c["named"] and f"<{c['peer'][:8]}…>" in seen
        ]
        if missing or hashed_named:
            raise StepFailed(
                f"rows={len(seen)} missing={missing} shown as address though named={hashed_named}"
            )
        # Back to the top for the next step.
        for _ in range(6):
            self.scroll_list(dev.dump(), "list", up=True)
        unnamed = sum(1 for name in seen if is_hash_name(name))
        return (
            f"rows={len(seen)} of {len(expected)} expected (of {len(self.need_fixture().manifest['conversations'])}, "
            f"archived and blocked hidden) named={len(seen) - unnamed} address-only={unnamed} "
            f"first_name_ms={first_ms}"
        )

    def open_conversation(self, shown: str) -> None:
        dev = self.need_device()
        for _ in range(12):
            nodes = dev.dump()
            rows = find(nodes, rid="name", text=shown)
            if rows:
                dev.tap_node(rows[0])
                self.wait_id("compose", 10)
                return
            self.scroll_list(nodes, "list", up=False)
        raise StepFailed(f"no row {shown!r} in the list")

    def step_open_imported(self) -> str:
        dev = self.need_device()
        opened = self.need_fixture().manifest["open"]
        self.open_conversation(opened["shown"])
        # Up until the screen stops changing: the top of the conversation.
        last_top: list[str] = []
        for scrolls in range(40):
            nodes = dev.dump()
            bodies = [n.text for n in by_top(find(nodes, rid="body"))]
            if bodies and bodies == last_top:
                break
            last_top = bodies
            self.scroll_list(nodes, "list", up=True)
        nodes = dev.dump()
        bodies = [n.text for n in by_top(find(nodes, rid="body")) if n.text]
        days = [n.text for n in find(nodes, rid="day") if n.text]
        self.note(f"top of the imported conversation: bodies={bodies[:4]} days={days}")
        if not bodies or bodies[0] != opened["oldest_body"]:
            raise StepFailed(
                f"top body {bodies[:1]!r}, not the oldest imported {opened['oldest_body']!r}"
            )
        if not days:
            raise StepFailed("no day separator above the oldest imported row")
        return f"scrolls={scrolls} oldest={opened['oldest_body']!r} days={days}"

    def step_info(self) -> str:
        dev = self.need_device()
        nodes = dev.dump()
        bubbles = by_top(find(nodes, rid="bubble"))
        if not bubbles:
            raise StepFailed("no message bubble on screen")
        dev.long_press(bubbles[0])
        self.tap_text(ACTION_INFO)
        found = self.wait_text(INFO_TITLE, 10)
        nodes = dev.dump()
        shown = texts(nodes)
        bad = [t for t in shown if NOT_DECODED in t]
        if bad:
            raise StepFailed(f"the sheet says {bad[0]!r}")
        body = " ".join(t for t in shown if t != INFO_TITLE)
        self.note(f"info sheet: {body[:400]}")
        ok_buttons = find(nodes, text="OK")
        if ok_buttons:
            dev.tap_node(ok_buttons[0])
        else:
            dev.key("KEYCODE_BACK")
        return f"sheet={found[0].text!r} lines={len(shown)}"

    def step_reply_send(self) -> str:
        dev = self.need_device()
        fixture = self.need_fixture()
        body = f"walk reply {self.nonce}"
        nodes = dev.dump()
        bubbles = by_top(find(nodes, rid="bubble"))
        if not bubbles:
            raise StepFailed("no message bubble on screen")
        dev.long_press(bubbles[-1])
        self.tap_text(ACTION_REPLY)
        self.wait_id("reply_who", 5)
        dev.tap_node(self.wait_id("compose", 5)[0])
        dev.type_text(body)
        sends = find(dev.dump(), rid="send")
        if not sends:
            raise StepFailed("no send button")
        start = time.monotonic()
        dev.tap_node(sends[0])
        mark = ""

        def ticked():
            nonlocal mark
            nodes = dev.dump()
            mine = [n for n in by_top(find(nodes, rid="body")) if n.text == body]
            if not mine:
                return False
            y = mine[-1].bounds[1]
            marks = [n for n in find(nodes, rid="mark") if n.bounds[1] >= y]
            mark = marks[0].desc if marks else ""
            return mark in TICKS

        shown = wait_for(ticked, 5, every=0.2)
        ms = int((time.monotonic() - start) * 1000)
        if not shown:
            raise StepFailed(f"no bubble {body!r} with a tick within 5s; mark={mark!r}")
        accepted = wait_for(lambda: "HOME send accepted" in fixture.mailbox_log(), 10)
        if not accepted:
            raise StepFailed("the mailbox log has no 'HOME send accepted'")
        return f"bubble_ms={ms} mark={mark!r} mailbox=accepted"

    def step_receive_wake(self) -> str:
        dev = self.need_device()
        fixture = self.need_fixture()
        body = f"walk wake {self.nonce}"
        send = subprocess.run(
            [
                self.args.loquax,
                "send",
                "--config",
                str(fixture.cfg),
                "--via",
                "direct",
                fixture.manifest["address"],
                body,
            ],
            env={**os.environ, "LNMSG_HOME": str(fixture.sender)},
            capture_output=True,
            timeout=60,
        )
        sent = time.monotonic()
        if send.returncode != 0:
            raise StepFailed(f"loquax send exit={send.returncode}: {send.stderr.decode()[-300:]}")
        # No tap: the wake (1123/1164) must bring it. A dump takes a while
        # itself, so a dump begun within 3 s that shows the bubble passes;
        # the detail says when it ended too.
        while True:
            begun = time.monotonic() - sent
            nodes = dev.dump()
            ended = time.monotonic() - sent
            if find(nodes, rid="body", text=body):
                if begun <= 3.0:
                    return f"seen_by_dump_begun_ms={int(begun * 1000)} ended_ms={int(ended * 1000)}"
                raise StepFailed(f"the bubble came after {begun:.1f}s, not within 3s")
            if begun > 3.0:
                raise StepFailed(f"no bubble {body!r} within 3s of the send; {texts(nodes)[-6:]}")

    def step_receive_report(self) -> str:
        """A position report from the open conversation's peer, with the
        conversation closed (1209): it raises no notification, the list's
        line says `📍 live`, and the row's avatar gains its ring."""
        dev = self.need_device()
        fixture = self.need_fixture()
        peer = fixture.manifest["sender_peer"]
        shown = fixture.manifest["open"]["shown"]
        for _ in range(3):
            if find(dev.dump(), rid="write"):
                break
            dev.key("KEYCODE_BACK")
        nodes = dev.dump()
        avatar = self.row_avatar(nodes, shown)
        before = [dev.screen().pixel(x, y) for x, y in ring_points(avatar.bounds)]
        told = conversation_notifications(
            dev.shell("dumpsys notification --noredact"), PACKAGE
        )
        send = subprocess.run(
            [
                self.args.loquax,
                "send",
                "--config",
                str(fixture.cfg),
                "--via",
                "direct",
                "--position",
                "43.7731,11.2560",
                fixture.manifest["address"],
            ],
            env={**os.environ, "LNMSG_HOME": str(fixture.sender)},
            capture_output=True,
            timeout=60,
        )
        sent = time.monotonic()
        if send.returncode != 0:
            raise StepFailed(f"loquax send exit={send.returncode}: {send.stderr.decode()[-300:]}")
        if not wait_for(lambda: find(dev.dump(), text=LIVE_LINE), 5, every=0.3):
            raise StepFailed(f"no {LIVE_LINE!r} line within 5s; {texts(dev.dump())[:12]}")
        seen_ms = int((time.monotonic() - sent) * 1000)
        # The notification would come with the row; a moment more for it.
        time.sleep(1.0)
        after_told = conversation_notifications(
            dev.shell("dumpsys notification --noredact"), PACKAGE
        )
        if after_told.get(peer) != told.get(peer):
            raise StepFailed(
                f"a notification for {peer[:8]}: before={told.get(peer)!r} after={after_told.get(peer)!r}"
            )
        avatar = self.row_avatar(dev.dump(), shown)
        after = [dev.screen().pixel(x, y) for x, y in ring_points(avatar.bounds)]
        ring = changed_fraction(before, after)
        if ring < 0.5:
            raise StepFailed(f"the avatar's ring did not change: {ring:.2f} of its points")
        return f"live_line_ms={seen_ms} notified=no ring_changed={ring:.2f}"

    def row_avatar(self, nodes: list[Node], shown: str) -> Node:
        """The list row's avatar beside the name `shown`."""
        names = find(nodes, rid="name", text=shown)
        if not names:
            raise StepFailed(f"no row {shown!r} in the list")
        middle = (names[0].bounds[1] + names[0].bounds[3]) // 2
        for node in find(nodes, rid="avatar"):
            if node.bounds[1] <= middle <= node.bounds[3]:
                return node
        raise StepFailed(f"no avatar beside {shown!r}")

    def step_map(self) -> str:
        dev = self.need_device()
        # Back to the list, then the Map tab (1199).
        box = self.open_map()
        first = sample_grid(dev.screen(), box)
        problems = map_screen_problems(dev.dump())
        if problems:
            raise StepFailed("; ".join(problems))
        screen = dev.screen()
        black = black_cells(screen, box)
        if black:
            raise StepFailed(f"{black} black parts of 16 in the first frame")
        x1, y1, x2, y2 = box
        cx, cy = (x1 + x2) // 2, (y1 + y2) // 2
        changes = []
        previous = first
        moves = [
            ("zoom", lambda: dev.double_tap(cx, cy)),
            ("zoom", lambda: dev.double_tap(cx, cy)),
            ("pan", lambda: dev.swipe(cx + (x2 - x1) // 4, cy, cx - (x2 - x1) // 4, cy, 400)),
        ]
        for name, move in moves:
            move()
            time.sleep(1.5)
            screen = dev.screen()
            now = sample_grid(screen, box)
            share = changed_fraction(previous, now)
            black = black_cells(screen, box)
            changes.append(f"{name}={share:.2f}/black={black}")
            if share < 0.05:
                raise StepFailed(f"the frame did not change after {name}: {changes}")
            if black:
                raise StepFailed(f"{black} black parts after {name}: {changes}")
            previous = now
        # The map's theme lives in Settings, Appearance now: light, then
        # dark, each judged on the map tab it returns to.
        lumas = []
        for choice in (THEME_LIGHT, THEME_DARK):
            self.map_theme(choice)
            time.sleep(1.5)
            lumas.append(mean_luma(sample_grid(dev.screen(), box)))
        self.map_theme(THEME_AUTO)
        if lumas[0] - lumas[1] < 20:
            raise StepFailed(f"Settings' map theme left the brightness at {lumas[0]:.0f} -> {lumas[1]:.0f}")
        dev.key("KEYCODE_BACK")
        return f"{' '.join(changes)} tabs=2 chrome=none theme_luma={lumas[0]:.0f}->{lumas[1]:.0f}"

    def open_map(self) -> tuple[int, int, int, int]:
        """The Map tab from wherever the walk is, once a frame shows: the
        map view's box."""
        dev = self.need_device()
        for _ in range(3):
            if find(dev.dump(), rid="write"):
                break
            dev.key("KEYCODE_BACK")
            time.sleep(0.6)
        tabs = find(dev.dump(), rid="tab_map")
        if not tabs:
            raise StepFailed(f"no Map tab on the list: {texts(dev.dump())[:12]}")
        dev.tap_node(tabs[0])
        box = None

        def framed():
            nonlocal box
            nodes = dev.dump()
            if not find(nodes, rid="map"):
                return None
            box = self.map_box(nodes)
            return box if not is_uniform(sample_grid(dev.screen(), box)) else None

        if not wait_for(framed, 20, every=0.5) or box is None:
            raise StepFailed("no map frame within 20s: the map view is uniform or missing")
        return box

    def step_fling(self) -> str:
        """A fast pan let go of coasts on (1188): the screen grabbed about
        100 ms and 400 ms after the finger lifted and once at rest differ
        pairwise. The grabs' times are measured from the swipe's return and
        reported, a screencap takes its own while."""
        dev = self.need_device()
        box = self.open_map()
        x1, y1, x2, y2 = box
        cx, cy = (x1 + x2) // 2, (y1 + y2) // 2
        time.sleep(1.5)
        reach = (x2 - x1) * 3 // 8
        # Half the width in 60 ms: thousands of pixels a second, far past
        # ViewConfiguration's minimum fling velocity.
        dev.swipe(cx + reach, cy, cx - reach, cy, 60)
        lifted = time.monotonic()

        def grab_at(ms: int) -> tuple[int, list[tuple[int, int, int]]]:
            wait = lifted + ms / 1000 - time.monotonic()
            if wait > 0:
                time.sleep(wait)
            took = int((time.monotonic() - lifted) * 1000)
            return took, sample_grid(dev.screen(), box)

        early_ms, early = grab_at(100)
        later_ms, later = grab_at(400)
        final_ms, final = grab_at(4000)
        problems = fling_problems(early, later, final)
        detail = (
            f"grabs at +{early_ms}/+{later_ms}/+{final_ms} ms "
            f"early-later={changed_fraction(early, later):.2f} "
            f"later-final={changed_fraction(later, final):.2f}"
        )
        if problems:
            raise StepFailed(f"the map stood still: {'; '.join(problems)}; {detail}")
        black = black_cells(dev.screen(), box)
        if black:
            raise StepFailed(f"{black} black parts at rest after the fling; {detail}")
        dev.key("KEYCODE_BACK")
        return detail

    def step_friends(self) -> str:
        """The Map tab's Friends button (1200): the sheet lists the fixture's
        peers with a position, under the divider of the older ones; a tap
        on one closes it and opens that friend's card; Chat on the card
        opens the chat, and back returns to the map with the card; back
        closes the card, then goes to the chats."""
        dev = self.need_device()
        allowed = set(self.need_fixture().manifest.get("friends", []))
        for _ in range(3):
            if find(dev.dump(), rid="tab_map"):
                break
            dev.key("KEYCODE_BACK")
            time.sleep(0.6)
        dev.tap_node(self.wait_id("tab_map", 10)[0])
        dev.tap_node(self.wait_id("friends", 10)[0])
        self.wait_id("friend_name", 10)
        nodes = dev.dump()
        names, problems = friends_sheet_problems(nodes, allowed)
        if names and not divider_above(nodes):
            problems.append("no 'Older than a day' divider above the rows")
        self.note(f"friends sheet: {names}")
        if problems:
            raise StepFailed("; ".join(problems))
        first = by_top(find(nodes, rid="friend_row"))
        if not first:
            raise StepFailed("no friend row to tap")
        dev.tap_node(first[0])
        card = self.wait_id("card_name", 10)
        if card[0].text != names[0]:
            raise StepFailed(f"the card says {card[0].text!r}, the row {names[0]!r}")
        nodes = dev.dump()
        navigate = bool(find(nodes, rid="card_navigate"))
        chats = find(nodes, rid="card_chat")
        if not chats:
            raise StepFailed(f"no Chat on the card: {texts(nodes)[:12]}")
        dev.tap_node(chats[0])
        self.wait_id("compose", 10)
        dev.key("KEYCODE_BACK")
        back = self.wait_id("card_name", 10)
        if back[0].text != names[0]:
            raise StepFailed(f"back from the chat the card says {back[0].text!r}")
        dev.key("KEYCODE_BACK")
        if not wait_for(lambda: not find(dev.dump(), rid="card_name"), 5):
            raise StepFailed("back did not close the card")
        dev.key("KEYCODE_BACK")
        self.wait_id("write", 10)
        return (
            f"friends={len(names)} of {len(allowed)} with a position, first={names[0]!r} "
            f"card=ok navigate={'shown' if navigate else 'hidden (no app takes geo:)'} chat=ok back=card,chats"
        )

    def step_settings(self) -> str:
        """The settings (1201): More, Settings; the eight entries in their
        order, each with a one line description and no raw hash; each page
        opened and left with back, none with a line that did not decode,
        each with its own mark where it has one; the Maps page as
        `maps_page` walks it (1207); back to the chats."""
        dev = self.need_device()
        self.put_fixture_region()
        for _ in range(3):
            if find(dev.dump(), rid="write"):
                break
            dev.key("KEYCODE_BACK")
            time.sleep(0.6)
        self.tap_text(MORE)
        self.tap_text(SETTINGS)
        self.wait_id("settings_entry_title", 10)
        titles, problems = settings_list_problems(dev.dump())
        if problems:
            raise StepFailed("; ".join(problems))
        walked = []
        for page in SETTINGS_PAGES:
            self.tap_text(page)
            time.sleep(1.0)
            nodes = dev.dump()
            if find(nodes, rid="settings_entry_title"):
                raise StepFailed(f"{page} did not open: still the list")
            trouble = decode_problems(nodes)
            missing = page_mark_missing(page, nodes)
            if missing:
                trouble.append(missing)
            if trouble:
                raise StepFailed(f"{page}: " + "; ".join(trouble))
            if page == "Maps":
                walked.append(f"Maps({self.maps_page()})")
            else:
                walked.append(page)
            dev.key("KEYCODE_BACK")
            self.wait_id("settings_entry_title", 10)
        dev.key("KEYCODE_BACK")
        self.wait_id("write", 10)
        return f"entries={len(titles)} walked={','.join(walked)} back=chats"

    def put_fixture_region(self) -> None:
        """The walk's region into the app's maps directory, through
        run-as (a debug APK), for the Maps page to list (1207)."""
        dev = self.need_device()
        if not FIXTURE_REGION.is_file():
            raise StepFailed(f"{FIXTURE_REGION} missing")
        dev.run("push", str(FIXTURE_REGION), "/data/local/tmp/walk-firenze.lmr", timeout=60)
        dev.shell(
            f"cat /data/local/tmp/walk-firenze.lmr | run-as {PACKAGE} sh -c "
            "'mkdir -p files/maps && cat > files/maps/firenze.lmr'"
        )
        dev.shell("rm -f /data/local/tmp/walk-firenze.lmr", check=False)

    def maps_switch(self, on: bool) -> list[Node]:
        """The Maps page's switch once it shows `on`; a line that did not
        decode on the way fails (1207)."""
        dev = self.need_device()
        seen: list[Node] = []

        def shows():
            nonlocal seen
            seen = dev.dump()
            if decode_problems(seen):
                raise StepFailed("Maps: " + "; ".join(decode_problems(seen)))
            switch = find(seen, rid="maps_auto")
            return seen if switch and switch[0].checked == on else None

        if not wait_for(shows, 10, every=0.3):
            raise StepFailed(f"Maps: the switch did not show {'on' if on else 'off'}")
        return seen

    def maps_page(self) -> str:
        """The Maps page (1207), open: the switch on, the storage used,
        the fixture's region with its size; the switch off, the page left
        and opened again still off (stored), then on again; never a line
        that did not decode. The page's line for the detail."""
        dev = self.need_device()
        nodes = self.maps_switch(True)
        detail, problems = maps_page_problems(nodes)
        if problems:
            raise StepFailed("Maps: " + "; ".join(problems))
        dev.tap_node(find(nodes, rid="maps_auto")[0])
        self.maps_switch(False)
        dev.key("KEYCODE_BACK")
        self.wait_id("settings_entry_title", 10)
        self.tap_text("Maps")
        self.wait_id("maps_auto", 10)
        nodes = self.maps_switch(False)
        dev.tap_node(find(nodes, rid="maps_auto")[0])
        self.maps_switch(True)
        return f"{detail} switch=off,stored,on"

    def step_profile(self) -> str:
        """The profile (1187): our avatar at the top left of the chats
        opens it; the address row shows the mailbox's address, the QR code
        and Share are there; back to the chats."""
        dev = self.need_device()
        for _ in range(3):
            if find(dev.dump(), rid="write"):
                break
            dev.key("KEYCODE_BACK")
            time.sleep(0.6)
        dev.tap_node(self.wait_id("avatar", 10)[0])
        self.wait_id("profile_address", 10)
        want = self.fixture.manifest["address"] if self.fixture is not None else None
        shown, problems = profile_problems(dev.dump(), want)
        if problems:
            raise StepFailed("; ".join(problems))
        dev.key("KEYCODE_BACK")
        self.wait_id("write", 10)
        return f"address={shown} qr=shown share=shown back=chats"

    def map_theme(self, choice: str) -> None:
        """From the Map tab: menu, Settings, Appearance, `choice`, and back
        to the map."""
        dev = self.need_device()
        self.tap_text(MORE)
        self.tap_text(SETTINGS)
        self.tap_text(APPEARANCE)
        self.tap_text(choice)
        time.sleep(0.5)
        for _ in range(2):
            dev.key("KEYCODE_BACK")
            time.sleep(0.6)
        if not find(dev.dump(), rid="map"):
            raise StepFailed(f"not back on the map after the theme {choice!r}")

    def step_teardown(self) -> str:
        done = []
        problems = []
        fixture = self.fixture
        if fixture is not None:
            revoke = subprocess.run(
                [self.args.loquax, "mailbox", "revoke", "walk"],
                env={**os.environ, "LNMSG_HOME": str(fixture.mailbox)},
                capture_output=True,
                timeout=30,
            )
            if revoke.returncode == 0:
                done.append("revoked")
            elif self.joined:
                problems.append(f"revoke exit={revoke.returncode} {revoke.stderr.decode()[-200:]}")
            fixture.stop()
            done.append("fixture stopped")
        if self.installed and self.device is not None:
            self.device.run("uninstall", PACKAGE, timeout=60, check=False)
            done.append("uninstalled")
        if problems:
            raise StepFailed("; ".join(problems + done))
        return ", ".join(done) or "nothing to tear down"


def parse_args(argv: list[str]) -> argparse.Namespace:
    debug = REPO / "target" / TARGET / "debug"
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--serial", help="adb serial of the device (adb devices)")
    p.add_argument(
        "--apk",
        default=str(REPO / "loquax-android/android/bin/loquax-debug-x86_64.apk"),
        help="the APK to install (make waydroid)",
    )
    p.add_argument(
        "--server",
        default="192.168.240.1:4242",
        help="host:port of the mailbox's TCP server as the device reaches it",
    )
    p.add_argument("--listen", default="0.0.0.0", help="the TCP server's listen address")
    p.add_argument(
        "--server-mode",
        help="the TCP server's interface mode, e.g. access_point; default: full",
    )
    p.add_argument(
        "--link-keepalive",
        type=int,
        help="keepalive_interval in seconds of the host's links; default: from the RTT",
    )
    p.add_argument("--loquax", default=str(debug / "loquax"), help="host loquax (--features tui)")
    p.add_argument("--lnsd", default=str(debug / "lnsd"), help="host lnsd")
    p.add_argument(
        "--walk-mailbox",
        help="a built walk_mailbox example; default: cargo run it",
    )
    p.add_argument("--adb", default="adb", help="the adb to use")
    p.add_argument("--keep", action="store_true", help="no teardown")
    p.add_argument("--out", default=str(REPO / "target/tmp/device-walk"), help="log directory")
    p.add_argument(
        "--steps",
        default=",".join(STEPS),
        help="the steps to run, comma separated, in the walk's order",
    )
    return p.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    chosen = [s.strip() for s in args.steps.split(",") if s.strip()]
    unknown = [s for s in chosen if s not in STEPS]
    if unknown:
        print(f"device-walk: unknown steps {unknown}; known: {STEPS}", file=sys.stderr)
        return 2
    if args.keep and "teardown" in chosen:
        chosen.remove("teardown")
    walk = Walk(args)
    start = time.monotonic()
    try:
        for step in STEPS:
            if step in chosen:
                walk.run_step(step)
    finally:
        if walk.fixture is not None and "teardown" not in chosen and not args.keep:
            walk.fixture.stop()
    ms = int((time.monotonic() - start) * 1000)
    walk.emit(summary_line(walk.ok, walk.failed, ms))
    if args.keep and walk.fixture is not None:
        walk.note(f"kept: fixture in {walk.fixture.root}, code {walk.fixture.code}")
    return 1 if walk.failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
