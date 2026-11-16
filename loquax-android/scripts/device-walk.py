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
import shutil
import signal
import socket
import sqlite3
import struct
import subprocess
import sys
import tempfile
import threading
import time
import xml.etree.ElementTree as ET
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

PACKAGE = "network.leviculum.loquax.debug"
ACTIVITY = "network.leviculum.loquax.MainActivity"
REPO = Path(__file__).resolve().parents[2]
TARGET = "x86_64-unknown-linux-musl"

STEPS = [
    "fixture",
    "install",
    "join_error_words",
    "first_start",
    "list",
    "notification_text",
    "open_imported",
    "info",
    "contact_info",
    "marks",
    "reply_send",
    "compose_bar",
    "image",
    "picture_zoom",
    "file",
    "voice",
    "voice_hold_release",
    "voice_slide_cancel",
    "voice_lock_send",
    "voice_silence_hint",
    "camera_photo",
    "share_position_from_bar",
    "share_for_15_min",
    "share_until_stopped",
    "receive_wake",
    "receive_report",
    "position_pin",
    "draft",
    "group_create",
    "group_join",
    "group_message",
    "group_card_join",
    "network_wifi_toggle",
    "network_mobile_switch",
    "flight_mode",
    "map",
    "fling",
    "friends",
    "offgrid_positions",
    "settings",
    "receipts",
    "profile",
    "explore",
    "first_start_qr",
    "pencil_scan",
    "share_in_contact",
    "buildings",
    "pan_into_empty",
    "region_fills_in",
    "first_start_name",
    "link_device",
    "move_phone",
    "teardown",
]

# The texts the screens show (android/res/values/strings.xml). The walk
# runs the phone in English; a German one shows values-de's.
WHERE_TITLE = "Where should your mailbox be?"
WHERE_COMPUTER = "On a computer at home"
WHERE_PHONE = "On this phone"
DONE = "Done"
# Link a device (1264, finding 7): the profile's row, the sheet's ids, the
# line once the other device came, and the name the walk's second device
# (the host's loquax) brings in its join.
LINK_ROW = "profile_link_device"
LINK_QR = "link_qr"
LINK_CODE = "link_code"
LINK_STATUS = "link_status"
LINKED = "Device linked: {}"
LINKED_NAME = "walk laptop"
JOIN_CODE_PREFIX = "lqxh1"
# Moving to a new phone (1281, walk finding 24): the profile's row, the
# sheet's ids, its line once the new phone (the host's walk_move) took the
# mailbox, and the first screen of the old phone afterwards.
MOVE_ROW = "profile_move_phone"
MOVE_QR = "move_qr"
MOVE_CODE = "move_code"
MOVED = "Moved to {}. This phone has stopped."
MOVED_NAME = "walk new phone"
MOVED_TITLE = "This phone has moved"
TAKE_BACK = "Use this phone again"
TAKE_BACK_ID = "join_retry"
# The first start's name (walk finding 9): the address page's one field,
# and the name the walk types into it on a phone mailbox.
WELCOME_NAME = "welcome_name"
WELCOME_QR = "welcome_qr"
PHONE_NAME = "Walk Phone"
# The always-on notification (walk finding 19): its line names the
# mailbox, never the server it dials, and carries no unread count, which
# would post it again on every new message and undo a swipe (1345).
SESSION_LINE = re.compile(r"^Connected to your mailbox$")
NOT_CONNECTED = "Not connected, trying again"
# A friend's code from another app (walk finding 12): the name dialog, the
# name the walk gives, and the chat's banner that must not show right
# after the add (walk finding 15).
ADD_CONTACT = "Add contact"
ADD_CONTACT_ASK = "What do you call this contact?"
FRIEND_NAME = "Walk Friend"
BOOK_BANNER = "book_banner"
CONTINUE = "Continue"
PASTE_CODE = "Paste a code"
JOIN = "Join"
ACTION_INFO = "Info"
ACTION_REPLY = "Reply"
INFO_TITLE = "Message details"
# The chat menu and the contact screen (1283, walk findings 28 and 29):
# Refresh left the menu; the identity and the network sit under a closed
# Advanced, whose header and body have their ids (ids.xml).
CONTACT_INFO = "Contact info"
CHAT_MENU_GONE = ["Refresh"]
CONTACT_SCROLL = "contact_scroll"
CONTACT_ADVANCED = "contact_advanced"
CONTACT_ADVANCED_BODY = "contact_advanced_body"
CONTACT_FOLDED = ["Identity", "Network"]
# Words the contact screen's main rows no longer say (finding 29).
CONTACT_MAIN_WORDS = ["hash", "the name it announces"]
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
# The tab bar's tabs (activity_conversations.xml): Chats, Map and
# Explore (1578), three and no fourth.
TABS = ["tab_chats", "tab_map", "tab_explore"]
# Explore's list (1578): its field, its order's toggle, its list, and the
# section titles the library words: "Pages", "Rooms" and, under it,
# "Servers" (1621). The titles are the catalogue's (`explore_pages`,
# `explore_rooms`, `explore_servers`) in the phone's language, read from
# the generated strings_scene.xml: English literals failed on the German
# Pixel before they checked anything (1618, walk 15).
EXPLORE_IDS = ["explore_search", "explore_sort", "explore_list"]
EXPLORE_TITLE_KEYS = ["explore_pages", "explore_rooms", "explore_servers"]
# One scroll step of Explore's list (1621, walk 2): at most three quarters
# of the list's height, dragged slowly enough that the list does not fling
# on, so no section title passes unseen between two dumps.
EXPLORE_SCROLL_SHARE = 0.75
EXPLORE_SCROLL_MS = 1500
ANDROID_RES = Path(__file__).resolve().parent.parent / "android" / "res"


def catalogue_words(lang: str, keys: list[str]) -> list[str]:
    """The catalogue's `keys` in `lang` (a locale tag, `de-DE` or `en`), as
    the app shows them: the language's strings_scene.xml, English where
    the phone speaks one the app does not."""
    base = lang.replace("_", "-").split("-")[0].lower()
    path = ANDROID_RES / ("values" if base in ("", "en") else f"values-{base}") / "strings_scene.xml"
    if not path.exists():
        path = ANDROID_RES / "values" / "strings_scene.xml"
    found = {}
    for item in ET.parse(path).getroot().iter("string"):
        found[item.get("name")] = (item.text or "").replace("\\'", "'").replace('\\"', '"')
    return [found[key] for key in keys]


def phone_lang(dev) -> str:
    """The phone's language as a locale tag: the user's choice, else the
    build's."""
    for prop in ("persist.sys.locale", "ro.product.locale"):
        said = dev.shell(f"getprop {prop}", check=False).strip()
        if said:
            return said
    return "en"
NOT_DECODED = "did not decode"
# The settings' entries in the list's order (1199, SettingsActivity.PAGES).
SETTINGS_PAGES = [
    "Profile", "Mailbox", "Appearance", "Chats", "Notifications", "Privacy", "Maps", "Backup",
    "Advanced",
]
# Each page's mark that it is that page and not a copy of another (1201):
# an id or a text only it shows. Pages with no mark of their own say none.
SETTINGS_PAGE_MARKS = {
    "Profile": ("rid", "profile_qr"),
    "Appearance": ("text", THEME_AUTO),
    "Chats": ("rid", "photo_size_phone"),
    "Privacy": ("text", "Positions you shared"),
    "Maps": ("rid", "maps_auto"),
}
# The Privacy page's first row (1233): "Send read receipts", its line,
# the switch's id.
RECEIPTS_ROW = "Send read receipts"
RECEIPTS_LINE = "Others see when you have read their messages."
RECEIPTS_SWITCH = "privacy_receipts"
BLOCKED_HEADER = "Blocked contacts"
# A photo (1220): the repository's fixture, what the paperclip's entry
# says, what a bubble's photo is called by a screen reader (the
# catalogue's scene_photo), and the whole photo's view id.
FIXTURE_PHOTO = REPO / "loquax" / "tests" / "fixtures" / "photo-portrait.jpg"
ATTACH_PHOTO = "Gallery"
PHOTO_WORDS = re.compile(r"^Photo, \d+(?:\.\d)? (?:B|KB|MB)$")
PHOTO_FULL = "photo_full"
# The compose bar (1270, after Signal): the buttons inside the field
# left to right, and the round one outside it, by the scene's ids
# (scene::NodeId::resource); five with an empty field, four once a letter
# is typed (the plus is Send, the microphone hides). The plus sheet's and
# the position sheet's rows, the staged photo's words, the fixtures the
# debug build takes for the camera (ChatActivity.CAMERA_FIXTURE) and the
# phone's fix (Locator.FIXTURE), and the walk's seeded fix.
BAR_EMPTY = ["emoji", "locate", "camera", "record", "plus"]
BAR_TYPED = ["emoji", "locate", "camera", "send"]
BAR_IDS = ["emoji", "locate", "camera", "record", "plus", "send"]
SHEET_GALLERY = "sheet_gallery"
SHEET_FILE = "sheet_file"
SHEET_SHARE_NOW = "sheet_share_now"
SHEET_FIX_WORDS = "sheet_fix_words"
STAGED_WORDS = re.compile(r"^Photo to send, \d+(?:\.\d)? (?:B|KB|MB)$")
CAMERA_FIXTURE = "camera-fixture.jpg"
FIX_FIXTURE = "fix-fixture.txt"
SEEDED_FIX = "43.7731,11.2560"
# A share for a while (1273): the sheet's rows, the chat's chip, its
# Stop, its words; a second seeded fix 44 m north of the first, a move by
# the share's 25 m; and the end report's mark as the wire carries it, the
# msgpack of `"ended": true` in the location's eighth element.
SHEET_SHARE_15_MIN = "sheet_share_15_min"
SHEET_SHARE_UNTIL = "sheet_share_until_stopped"
SHARE_CHIP = "share_chip"
SHARE_WORDS = "share_words"
SHARE_STOP = "share_stop"
SHARE_LEFT = re.compile(r"^Sharing your position, \d+ (?:min|h) left$")
SHARE_UNTIL = "Sharing your position until you stop"
MOVED_FIX = "43.7735,11.2560"
SHARE_ENDED = b"\xa5ended\xc3"
# A file (1221): what the paperclip's entry says, what a bubble's file is
# called by a screen reader (the catalogue's scene_file), and the line the
# chat shows where no app on the phone opens the kind.
ATTACH_FILE = "File"
FILE_WORDS = re.compile(r"^File (?P<name>.+), \d+(?:\.\d)? (?:B|KB|MB)$")
FILE_NO_APP = "No app on this device opens this file."
# A voice message (1222): what a screen reader says for a bubble's player
# (the catalogue's scene_voice), its two buttons, and the fixture the
# debug build records instead of the microphone (Voice.FIXTURE).
VOICE_WORDS = re.compile(r"^Voice message, (?P<m>\d+):(?P<s>\d\d)$")
VOICE_PLAY = "Play"
VOICE_PAUSE = "Pause"
VOICE_FIXTURE = "voice-fixture.pcm"
# Recording a voice message (1276): the recording bar's parts by the
# scene's ids (rec_dot the pulsing dot, selected while it pulses, rec_held
# the microphone under the finger), its words (the catalogue's rec_*, the
# hint with RecordingBar's chevron), the timer's form, and the parts each
# phase shows and does not.
REC_TIMER = re.compile(r"^REC (?P<m>\d\d):(?P<s>\d\d)$")
REC_SLIDE = "\u2039 Slide left to cancel"
REC_RELEASE = "\u2039 Release to cancel"
REC_HELD = "Release to send"
REC_LOCK = "Lock the recording"
REC_LOCKED = "Recording locked"
REC_SILENT = "Nothing heard. Is the microphone allowed?"
REC_ALLOW = "Allow the microphone"
REC_PARTS = {
    "held": (
        ["rec_bar", "rec_dot", "rec_timer", "rec_wave", "rec_cancel", "rec_held", "rec_lock"],
        ["compose", "rec_send", "rec_pause", "rec_discard", "rec_resume"],
    ),
    "locked": (
        ["rec_bar", "rec_dot", "rec_timer", "rec_wave", "rec_lock", "rec_discard", "rec_pause", "rec_send"],
        ["compose", "rec_held", "rec_cancel", "rec_resume", "rec_play"],
    ),
    "paused": (
        ["rec_bar", "rec_dot", "rec_timer", "rec_wave", "rec_discard", "rec_play", "rec_resume", "rec_send"],
        ["compose", "rec_held", "rec_cancel", "rec_pause", "rec_lock"],
    ),
}
# RecordingBar.log's line in a debuggable build, under the tag `loquax`,
# whenever the phase, the dot, the chip, the hint's arming, the buttons or
# the line change.
REC_LINE = re.compile(
    r"REC_BAR phase=(?P<phase>\w+) pulsing=(?P<pulsing>true|false) locked=(?P<locked>true|false) "
    r"armed=(?P<armed>true|false) buttons=(?P<buttons>[\w,]*) notice=(?P<notice>[^\r\n]*)"
)
# One contact code everywhere (1263, finding 1): the first start's
# address page, opened in the debug APK by `am start` with this extra, and
# the start of every contact code its QR and the profile's hold.
WELCOME_ACTIVITY = "network.leviculum.loquax.WelcomeActivity"
LXMA_PREFIX = "lxma://"
# Group chats (1307): the pencil's "New group" row, the picker's Next and
# the name step's Create (the catalogue's group_next, group_create), the
# mailbox's line when it hosts a group, and the line a member who joined
# by writing makes in the group's chat (1307 G2).
GROUP_NEW = "new_group"
GROUP_NEXT = "Next"
GROUP_CREATE = "Create"
GROUP_JOINED = "{} joined."
# The invitation card's Join (the catalogue's group_join), 1307 pass four.
GROUP_JOIN = "Join"


# The pencil (1263, finding 3): its two rows, top to bottom, the fold
# that hides the typed address, and the sentence for a text that is no
# code (the catalogue's code_foreign).
# "New contact" (1448, walk finding 17): the pencil's screen is a contact
# list, and the ways to add one unfold under this row.
PENCIL_CONTACT = "new_contact"
PENCIL_SCAN = "new_scan"
PENCIL_PASTE = "new_paste"
PENCIL_ADVANCED = "new_advanced"
CODE_FOREIGN = "That is not a Loquax code."
# Join errors in plain words (1263, finding 8): what a code that is no
# mailbox's says (the catalogue's join_not_mailbox), the parser's words
# behind Details, and the ids of Retry and the fold.
JOIN_NOT_MAILBOX = "That code is not a mailbox code."
JOIN_DETAILS_SAY = "That is not a mailbox code"
JOIN_RETRY = "join_retry"
JOIN_DETAILS_TOGGLE = "join_details_toggle"
JOIN_DETAILS = "join_details"
BAD_MAILBOX_CODE = "lqxh1nonsense"
# Words the 90 % never see on a main row (1263): Reticulum's and the
# daemon's.
DAEMON_WORDS = ["LXMF", "hex digit", "lnsd", "TCP", "host:port", "loquax home", "Reticulum"]
# The region the walk puts on the phone for the Maps page (1207): the
# repository's Florence fixture, and the middle the page names it by
# (Ui.place, two decimals, either decimal mark).
FIXTURE_REGION = REPO / "leviculum-map" / "fixtures" / "firenze.lmr"
FIXTURE_REGION_PLACE = re.compile(r"43[.,]\d\d° N, 11[.,]\d\d° E")
# A size as Formatter.formatShortFileSize writes it: "1.2 MB", "345 kB".
FILE_SIZE = re.compile(r"\d+(?:[.,]\d+)?\s?(?:B|kB|KB|MB|GB)\b")
# The Maps page's last row (1290, the core's catalogue): its id, its label
# with the total, its one question with the count while the automatic
# download is on, and the question's yes.
DELETE_ALL = "maps_delete_all"
DELETE_ALL_LABEL = re.compile(
    r"(?:Delete all downloaded maps|Alle geladenen Karten löschen) \((.+)\)"
)
DELETE_ALL_ASK = re.compile(
    r"Delete all (\d+) maps\? Areas you visit will be downloaded again when you look at them\."
    r"|Alle (\d+) Karten löschen\? Gebiete, die du besuchst, werden wieder geladen,"
    r" wenn du sie anschaust\."
)
DELETE_ALL_YES = "Delete all"
# The buttons of the dialogs a first start may raise: Android's
# notification question, its battery exemption, the app's own (Asks.java).
# "Not now" first: the battery question's "Allow" (1283, finding 27) opens a
# second, system dialog; Android's own questions have no "Not now".
DIALOG_BUTTONS = ["Not now", "Allow", "ALLOW"]
# The list's last line for a peer whose position is live (1209, Ui.preview).
LIVE_LINE = "\U0001f4cd live"
# A row's last line while a message is typed there and not sent (1219,
# the catalogue's draft_row).
DRAFT_PREFIX = "Draft: "
# The liveness ring's width as a share of the avatar (AvatarView.RING).
RING = 0.06
# The list row's map pin (1224): its content description begins so
# (the catalogue's position_of), and its tap target is at least 48dp.
PIN_LABEL = "Position of "
PIN_DP = 48
# The ladder's five marks (1232, loquax-read-receipts.md section 3): the
# view id each rung gives its mark (chat::Mark::resource, Glyph.resource)
# and the start of what a screen reader says for it (the catalogue's
# state_* words in English); the clock has two, sending and sent with no
# proof.
MARK_WORDS = {
    "state_clock": ("Sending", "Sent, not confirmed yet"),
    "state_tick": ("Waiting in the mailbox",),
    "state_ticks": ("Delivered",),
    "state_ticks_read": ("Read at ",),
    "state_failed": ("Not sent. Double tap to send again",),
}
# A mark of ours the walk counts as a tick: anyone confirmed it.
TICKS = {"state_tick", "state_ticks", "state_ticks_read"}
# The buildings step (1193): the walk's region of central Frankfurt, z15
# with its building sections, and the place the map opens on, degrees
# times a million; the screen the debug APK exports for it.
FRANKFURT_REGION = REPO / "leviculum-map" / "fixtures" / "frankfurt" / "roemer.lmr"
FRANKFURT_E6 = (50107750, 8682000)
MAP_ACTIVITY = "network.leviculum.loquax.MapActivity"
# The building fill as the phone's frame holds it: the table's colour
# through RGB565 (leviculum_mapfill::frame::rgb888). tests/map.rs holds
# these to the renderer, and the shares below to its frame of this view.
BUILDING_FILL_LIGHT = (222, 215, 206)
BUILDING_FILL_DARK = (57, 60, 66)
# Samples across the middle half of the screen, columns by rows.
BUILDINGS_GRID = (32, 64)
# At z16 at least this share of the samples is fill; at z15, where the
# map draws no building, at most this.
BUILDINGS_LEAST = 0.05
BUILDINGS_Z15_MOST = 0.005
# The frame time proxy: the time from the screen asked for to the map at
# rest on screen, z16 with its buildings at most this many times z15's
# plus the slack (a screencap's own while is the proxy's grain).
SETTLE_FACTOR = 1.5
SETTLE_SLACK_MS = 1000
SETTLE_TIMEOUT_S = 20


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
    selected: bool = False
    enabled: bool = True

    @property
    def center(self) -> tuple[int, int]:
        x1, y1, x2, y2 = self.bounds
        return ((x1 + x2) // 2, (y1 + y2) // 2)

    def is_id(self, name: str) -> bool:
        """Whether the node's resource id is `name` of any package."""
        return self.rid.endswith(":id/" + name)


BOUNDS = re.compile(r"\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]")


def marks_on(nodes: list[Node]) -> list[Node]:
    """The delivery marks on screen: nodes whose id is a rung's."""
    return [n for n in nodes if any(n.is_id(rid) for rid in MARK_WORDS)]


def mark_rid(node: Node) -> str:
    """The rung a mark node's id names, or ""."""
    return next((rid for rid in MARK_WORDS if node.is_id(rid)), "")


def marks_problems(seen: dict[str, str]) -> list[str]:
    """What is wrong with the marks seen, rung to its spoken words: a rung
    never seen, or one that says another rung's words."""
    problems = [f"no {rid}" for rid in MARK_WORDS if rid not in seen]
    for rid, desc in seen.items():
        if not desc.startswith(MARK_WORDS[rid]):
            problems.append(f"{rid} says {desc!r}, not {' or '.join(MARK_WORDS[rid])!r}")
    return problems


def bar_on(nodes: list[Node]) -> list[Node]:
    """The compose bar's buttons on screen (1270), left to right."""
    found = [n for n in nodes if any(n.is_id(rid) for rid in BAR_IDS)]
    return sorted(found, key=lambda n: (n.bounds[0], n.bounds[1]))


def bar_id(node: Node) -> str:
    """The scene's id of a compose bar button node, or ""."""
    return next((rid for rid in BAR_IDS if node.is_id(rid)), "")


def bar_problems(nodes: list[Node], typed: bool) -> list[str]:
    """What is wrong with the compose bar (1270): not the buttons the
    scene lists for an empty or a typed field, in their order; a button
    a screen reader cannot name; the emoji not left of the field, the
    others not right of it; the round button inside the field's pill."""
    want = BAR_TYPED if typed else BAR_EMPTY
    buttons = bar_on(nodes)
    have = [bar_id(n) for n in buttons]
    problems = []
    if have != want:
        problems.append(f"the bar has {have}, want {want}")
    for node in buttons:
        if not node.desc:
            problems.append(f"{bar_id(node)} has no words for a screen reader")
    field = find(nodes, rid="compose")
    if not field:
        return problems + ["no compose field"]
    fx1, _, fx2, _ = field[0].bounds
    for node in buttons:
        x1, _, x2, _ = node.bounds
        rid = bar_id(node)
        if rid == "emoji" and x2 > fx1:
            problems.append(f"the emoji is not left of the field: {node.bounds} vs {field[0].bounds}")
        if rid != "emoji" and x1 < fx2:
            problems.append(f"{rid} is not right of the field: {node.bounds} vs {field[0].bounds}")
    inside = [n for n in buttons if bar_id(n) in ("locate", "camera", "record")]
    rounds = [n for n in buttons if bar_id(n) in ("plus", "send")]
    if rounds and inside and rounds[0].bounds[0] < max(n.bounds[2] for n in inside):
        problems.append("the round button is not outside the field")
    return problems


def caption_bubble_problems(nodes: list[Node], photo: Node, caption: str) -> list[str]:
    """What is wrong with the bubble of a photo sent with a caption
    (1270, 1268's one bubble): the photo in no bubble or more than one,
    its bubble without the caption as its text row, or without a
    delivery mark."""
    around = [b for b in find(nodes, rid="bubble") if inside(photo, b)]
    if len(around) != 1:
        return [f"the photo is in {len(around)} bubbles, want one"]
    bubble = around[0]
    problems = []
    words = [n.text for n in find(nodes, rid="body") if inside(n, bubble)]
    if words != [caption]:
        problems.append(f"its bubble says {words}, want [{caption!r}]")
    if not [n for n in marks_on(nodes) if inside(n, bubble)]:
        problems.append("its bubble has no delivery mark")
    return problems


def accepted_to(log: str, peer: str) -> int:
    """How many sends the mailbox accepted for `peer` (`HOME send accepted
    ... to=<peer>`)."""
    return sum(
        1
        for line in log.splitlines()
        if "HOME send accepted" in line and f"to={peer}" in line
    )


def group_hosted(log: str, name: str) -> str | None:
    """The address the mailbox hosts the group `name` at (`GROUP hosted
    name=<name> address=<hex>`, 1307), the newest such line's."""
    found = None
    prefix = f"GROUP hosted name={name} address="
    for line in log.splitlines():
        at = line.find(prefix)
        if at >= 0:
            found = line[at + len(prefix):].split()[0]
    return found


def chip_problems(nodes: list[Node], words) -> list[str]:
    """What is wrong with the share's chip (1273): not there, its words
    not `words` (a string, or a pattern), or a Stop without words for a
    screen reader."""
    if not find(nodes, rid=SHARE_CHIP):
        return ["no share chip"]
    said = [n.text for n in find(nodes, rid=SHARE_WORDS)]
    problems = []
    fits = (lambda t: words.match(t)) if hasattr(words, "match") else (lambda t: t == words)
    if len(said) != 1 or not fits(said[0]):
        want = words.pattern if hasattr(words, "match") else words
        problems.append(f"the chip says {said}, want {want!r}")
    stop = find(nodes, rid=SHARE_STOP)
    if not stop:
        problems.append("the chip has no Stop")
    elif not (stop[0].desc or stop[0].text):
        problems.append("the Stop has no words for a screen reader")
    return problems


# The map chip's words for a load for the screen (strings.xml map_loading,
# 1199; since 1292 only for the area on screen).
MAP_LOADING = "Loading the map of this area"
# How long after the pan's last swipe ended the load for the screen may
# reach the source: the scheduler's bound (300 ms from the uncovered
# frame, 1292) and a frame's draw and the network's way on top.
PAN_LOAD_SLACK_MS = 700


def map_chip(nodes: list[Node]) -> str:
    """The map chip's words, empty when none shows."""
    chips = find(nodes, rid="chip")
    return chips[0].text if chips else ""


def pan_into_empty_problems(
    before_chip: str,
    after_chip: str,
    connects_before: int,
    connect_ms: list[int],
    pan_end_ms: int,
) -> list[str]:
    """What is wrong with a pan from the walk's region into the empty
    (1292): the chip said a load for the screen before the pan, no load
    reached the source after it, the first one only later than
    `PAN_LOAD_SLACK_MS` after the pan ended, or the chip did not say it
    loads here afterwards. `connect_ms` are the source's connections since
    the pan began, in milliseconds from it; `connects_before` those before,
    the loads next to the screen."""
    problems = []
    if before_chip.startswith(MAP_LOADING):
        problems.append(f"the chip said {before_chip!r} before the pan, over a held view")
    if not connect_ms:
        problems.append(f"no load reached the source after the pan ({connects_before} before it)")
    elif connect_ms[0] > pan_end_ms + PAN_LOAD_SLACK_MS:
        problems.append(
            f"the first load reached the source {connect_ms[0]} ms after the pan began, "
            f"later than {pan_end_ms} + {PAN_LOAD_SLACK_MS}"
        )
    if not after_chip.startswith(MAP_LOADING):
        problems.append(f"the chip says {after_chip!r} after the pan, want {MAP_LOADING!r}")
    return problems


class StallingSource:
    """A map source on the walk's host that takes every connection and
    never answers (1292): a load that starts stays running, and its
    connection's time is the load's start as the host sees it."""

    def __init__(self, host: str) -> None:
        self.server = socket.create_server((host, 0))
        self.port = self.server.getsockname()[1]
        self.times: list[float] = []
        self.held: list[socket.socket] = []
        self.thread = threading.Thread(target=self.accept, daemon=True)
        self.thread.start()

    def accept(self) -> None:
        while True:
            try:
                conn, _ = self.server.accept()
            except OSError:
                return
            self.times.append(time.monotonic())
            self.held.append(conn)

    def close(self) -> None:
        self.server.close()
        for conn in self.held:
            conn.close()


# A region arriving over a slow link (1291): the Grunewald archive (z12
# to z15, leviculum-mapcut/fixtures/README.md) served from this host at
# FILL_RATE, about ten seconds of it, the map opened over it at z15. The
# map must show at least FILL_STATES_LEAST distinct pictures before the
# source sent its last byte: the outline and the coarser zooms standing in
# for the finer ones as they arrive.
GRUNEWALD_ARCHIVE = REPO / "leviculum-mapcut" / "fixtures" / "grunewald.pmtiles"
GRUNEWALD_E6 = (52471500, 13228000)
FILL_RATE = 8000
FILL_STATES_LEAST = 3
# The share of the samples that must differ for a grab to be a new picture.
FILL_CHANGED = 0.02
FILL_TIMEOUT_S = 90
# How long the map must stay the same after the last byte to be at rest.
FILL_REST_S = 3.0


def fill_states(grabs: list[tuple[float, list[tuple[int, int, int]]]]) -> list[float]:
    """The times of the grabs that start a new picture: the first, and each
    that differs from the last new one in more than `FILL_CHANGED` of its
    samples."""
    times: list[float] = []
    last = None
    for at, samples in grabs:
        if last is None or changed_fraction(samples, last) > FILL_CHANGED:
            times.append(at)
            last = samples
    return times


def region_fills_problems(states: list[float], served: int, size: int, last_byte: float | None) -> list[str]:
    """What is wrong with a region arriving (1291): the source did not send
    the whole archive, or the map showed fewer than `FILL_STATES_LEAST`
    distinct pictures up to its last byte. `states` are the new pictures'
    times, `last_byte` the time the source sent the last byte."""
    problems = []
    if served < size or last_byte is None:
        problems.append(f"the source sent {served} of {size} bytes")
        return problems
    before = [t for t in states if t <= last_byte]
    if len(before) < FILL_STATES_LEAST:
        problems.append(
            f"{len(before)} distinct pictures before the region was whole, "
            f"want at least {FILL_STATES_LEAST}"
        )
    return problems


class ThrottledSource:
    """A map source on the walk's host that answers range requests for one
    archive at `rate` bytes a second over all its connections together
    (1291): a slow link the region arrives over. `served` counts the
    bytes sent of the answers' bodies, `last_byte` when the whole archive
    had gone out."""

    def __init__(self, host: str, data: bytes, rate: int) -> None:
        self.data = data
        self.rate = max(1, rate)
        self.lock = threading.Lock()
        self.free = 0.0
        self.sent: set[int] = set()
        self.served = 0
        self.last_byte: float | None = None
        source = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *args) -> None:
                pass

            def do_GET(self) -> None:
                source.answer(self)

        self.server = ThreadingHTTPServer((host, 0), Handler)
        self.server.daemon_threads = True
        self.port = self.server.server_address[1]
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def answer(self, h: BaseHTTPRequestHandler) -> None:
        size = len(self.data)
        m = re.fullmatch(r"bytes=(\d+)-(\d*)", h.headers.get("Range", ""))
        start, end = 0, size - 1
        if m:
            start = int(m.group(1))
            end = min(int(m.group(2)) if m.group(2) else size - 1, size - 1)
        if start >= size:
            h.send_response(416)
            h.send_header("Content-Range", f"bytes */{size}")
            h.send_header("Content-Length", "0")
            h.end_headers()
            return
        body = self.data[start : end + 1]
        h.send_response(206 if m else 200)
        if m:
            h.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        h.send_header("Content-Length", str(len(body)))
        h.send_header("Connection", "close")
        h.end_headers()
        for at in range(0, len(body), 4096):
            chunk = body[at : at + 4096]
            with self.lock:
                now = time.monotonic()
                begin = max(now, self.free)
                self.free = begin + len(chunk) / self.rate
                due = self.free
            time.sleep(max(0.0, due - time.monotonic()))
            h.wfile.write(chunk)
            with self.lock:
                self.served += len(chunk)
                self.sent.update(range(start + at, start + at + len(chunk)))
                if self.last_byte is None and len(self.sent) >= size:
                    self.last_byte = time.monotonic()

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()


def end_reports(store: Path, peer: str) -> int:
    """How many messages the mailbox's outbox holds for `peer` that carry
    a share's end mark (1273): the packed LXMF is plaintext at rest, its
    telemetry field with it."""
    import sqlite3

    if not store.exists():
        return 0
    conn = sqlite3.connect(f"file:{store}?mode=ro", uri=True)
    try:
        rows = conn.execute(
            "SELECT packed FROM home_outbox WHERE lower(hex(destination)) = ?",
            (peer.lower(),),
        ).fetchall()
    finally:
        conn.close()
    return sum(1 for (packed,) in rows if SHARE_ENDED in bytes(packed))


def photos_on(nodes: list[Node]) -> list[Node]:
    """The bubbles' photos on screen (1220), top first."""
    return by_top(find(nodes, rid="photo"))


def photo_problems(photos: list[Node], before: int) -> list[str]:
    """What is wrong with the photos on screen after one was sent, when
    `before` were there: none new, one a screen reader cannot name, or
    one drawn as a sliver (its thumbnail is at least 48 px a side)."""
    problems = []
    if len(photos) <= before:
        problems.append(f"{len(photos)} photos on screen, {before} before the send")
    for node in photos:
        if not PHOTO_WORDS.match(node.desc):
            problems.append(f"a photo says {node.desc!r}")
        x1, y1, x2, y2 = node.bounds
        if min(x2 - x1, y2 - y1) < 48:
            problems.append(f"a photo is {x2 - x1}x{y2 - y1}")
    return problems


def photo_bubble_problems(nodes: list[Node], photo: Node) -> list[str]:
    """What is wrong with the bubble of a photo sent without words (1268):
    it is in no bubble or in more than one, its bubble holds a text row,
    a bubble on screen holds nothing, or its bubble has no delivery
    mark."""
    bubbles = find(nodes, rid="bubble")
    around = [b for b in bubbles if inside(photo, b)]
    if len(around) != 1:
        return [f"the photo is in {len(around)} bubbles, want one"]
    bubble = around[0]
    problems = []
    texts = [n for n in find(nodes, rid="body") if inside(n, bubble)]
    if texts:
        problems.append(f"its bubble has a text row {texts[0].text!r}")
    # Every bubble holds something a screen shows or a reader says: a
    # text, a photo, a file, a voice message.
    for other in bubbles:
        if not any(inside(n, other) for n in nodes if n.text.strip() or n.desc):
            problems.append(f"an empty bubble at {other.bounds}")
    if not [n for n in marks_on(nodes) if inside(n, bubble)]:
        problems.append("its bubble has no mark")
    return problems


def files_on(nodes: list[Node]) -> list[Node]:
    """The bubbles' file rows on screen (1221), top first: what a screen
    reader names as a file."""
    return by_top([n for n in nodes if FILE_WORDS.match(n.desc)])


def file_problems(files: list[Node], before: int, name: str) -> list[str]:
    """What is wrong with the file rows on screen after `name` was sent,
    when `before` were there: none new, or the newest not named `name`."""
    if len(files) <= before:
        return [f"{len(files)} files on screen, {before} before the send"]
    match = FILE_WORDS.match(files[-1].desc)
    if match is None or match.group("name") != name:
        return [f"the newest file says {files[-1].desc!r}, want {name!r}"]
    return []


def voices_on(nodes: list[Node]) -> list[Node]:
    """The bubbles' voice players on screen (1222), top first: what a
    screen reader names as a voice message."""
    return by_top([n for n in nodes if VOICE_WORDS.match(n.desc)])


def voice_problems(voices: list[Node], before: int, least_s: int) -> list[str]:
    """What is wrong with the players on screen after a hold of the
    microphone, when `before` were there: none new, or the newest shorter
    than `least_s` seconds."""
    if len(voices) <= before:
        return [f"{len(voices)} voice messages on screen, {before} before the hold"]
    match = VOICE_WORDS.match(voices[-1].desc)
    if match is None:
        return [f"the newest says {voices[-1].desc!r}"]
    secs = int(match.group("m")) * 60 + int(match.group("s"))
    if secs < least_s:
        return [f"the newest is {secs} s long, want at least {least_s}"]
    return []


def rec_bar_problems(nodes: list[Node], phase: str) -> list[str]:
    """What is wrong with the recording bar on screen for `phase` (1276,
    `held`, `locked` or `paused`): a part it shows missing, one it does
    not show there, a timer not of the form `REC 00:07`, the hint, the
    held microphone or the chip with other words, a button without words
    for a screen reader."""
    want, gone = REC_PARTS[phase]
    problems = [f"no {rid}" for rid in want if not find(nodes, rid=rid)]
    problems += [f"{rid} on screen while {phase}" for rid in gone if find(nodes, rid=rid)]
    timer = find(nodes, rid="rec_timer")
    if timer and not REC_TIMER.match(timer[0].text):
        problems.append(f"the timer says {timer[0].text!r}")
    lock = find(nodes, rid="rec_lock")
    if phase == "held":
        hint = find(nodes, rid="rec_cancel")
        if hint and hint[0].text not in (REC_SLIDE, REC_RELEASE):
            problems.append(f"the hint says {hint[0].text!r}")
        held = find(nodes, rid="rec_held")
        if held and held[0].desc != REC_HELD:
            problems.append(f"the held microphone says {held[0].desc!r}")
        if lock and (lock[0].desc != REC_LOCK or lock[0].selected):
            problems.append(f"the chip says {lock[0].desc!r}, closed={lock[0].selected}, want open")
    if phase == "locked" and lock and (lock[0].desc != REC_LOCKED or not lock[0].selected):
        problems.append(f"the chip says {lock[0].desc!r}, closed={lock[0].selected}, want closed")
    for rid in ("rec_discard", "rec_pause", "rec_resume", "rec_play", "rec_send"):
        for button in find(nodes, rid=rid):
            if not button.desc:
                problems.append(f"{rid} has no words for a screen reader")
    return problems


def rec_seconds(nodes: list[Node]) -> int | None:
    """The recording bar's timer in seconds; None without one."""
    timer = find(nodes, rid="rec_timer")
    m = REC_TIMER.match(timer[0].text) if timer else None
    return int(m.group("m")) * 60 + int(m.group("s")) if m else None


def rec_pulsing(nodes: list[Node]) -> bool | None:
    """Whether the bar's dot pulses (selected); None without a dot."""
    dot = find(nodes, rid="rec_dot")
    return dot[0].selected if dot else None


def rec_notice_problems(nodes: list[Node]) -> list[str]:
    """What is wrong with the line of a microphone that hears nothing:
    not there, other words, no way to the permission, a dot that still
    pulses."""
    problems = []
    said = [n.text for n in find(nodes, rid="rec_notice")]
    if said != [REC_SILENT]:
        problems.append(f"the line says {said}, want [{REC_SILENT!r}]")
    allow = find(nodes, rid="rec_allow")
    if not allow or allow[0].text != REC_ALLOW:
        problems.append(f"no {REC_ALLOW!r} button: {[n.text for n in allow]}")
    if rec_pulsing(nodes) is not False:
        problems.append(f"the dot pulses={rec_pulsing(nodes)}, want False")
    return problems


def parse_rec_lines(logcat: str) -> list[dict]:
    """RecordingBar's lines in a logcat, oldest first: the phase, the dot,
    the chip, the hint's arming as booleans, the buttons as a list."""
    out = []
    for m in REC_LINE.finditer(logcat):
        line = m.groupdict()
        for key in ("pulsing", "locked", "armed"):
            line[key] = line[key] == "true"
        line["buttons"] = [b for b in line["buttons"].split(",") if b]
        line["notice"] = line["notice"].strip()
        out.append(line)
    return out


def rec_went(lines: list[dict], *path: str) -> list[str]:
    """What is missing from the phases `lines` went through for them to
    have gone `path` in that order (`held`, then `cancelled`; a phase
    word, or `armed` for a held line armed to cancel)."""
    at = 0
    for line in lines:
        if at == len(path):
            break
        want = path[at]
        if want == "armed" and line["phase"] == "held" and line["armed"]:
            at += 1
        elif want == line["phase"]:
            at += 1
    return [] if at == len(path) else [f"no {path[at]!r} after {list(path[:at])} in {[l['phase'] for l in lines]}"]


# --- The photo viewer's zoom (1269). ---

# `PhotoView.logState`'s line, under the tag `loquax`, once a gesture
# settled.
PHOTO_ZOOM = re.compile(
    r"PHOTO_ZOOM scale=(?P<scale>[\d.]+) left=(?P<left>-?[\d.]+) top=(?P<top>-?[\d.]+) "
    r"width=(?P<width>[\d.]+) height=(?P<height>[\d.]+) view=(?P<vw>\d+)x(?P<vh>\d+)"
)


@dataclass
class TouchDevice:
    """A multi-touch screen of `getevent -pl`: its node and the range of
    its two position axes, and whether it takes a pressure and a touch
    size, which some drivers want above 0 for a contact."""

    path: str
    x_min: int
    x_max: int
    y_min: int
    y_max: int
    pressure: bool = False
    touch_major: bool = False


AXIS = re.compile(r"(ABS_MT_[A-Z_]+)\s*:\s*value -?\d+, min (-?\d+), max (-?\d+)")


def parse_touch_device(getevent: str) -> TouchDevice | None:
    """The first direct (screen, not pad) multi-touch device with slots
    in `getevent -pl`'s listing; any with slots when none says direct."""
    found: list[tuple[bool, TouchDevice]] = []
    for block in re.split(r"(?m)^add device \d+: ", getevent)[1:]:
        path = block.split("\n", 1)[0].strip()
        axes = {name: (int(lo), int(hi)) for name, lo, hi in AXIS.findall(block)}
        if not {"ABS_MT_SLOT", "ABS_MT_POSITION_X", "ABS_MT_POSITION_Y"} <= axes.keys():
            continue
        (x_min, x_max), (y_min, y_max) = axes["ABS_MT_POSITION_X"], axes["ABS_MT_POSITION_Y"]
        device = TouchDevice(
            path,
            x_min,
            x_max,
            y_min,
            y_max,
            pressure="ABS_MT_PRESSURE" in axes,
            touch_major="ABS_MT_TOUCH_MAJOR" in axes,
        )
        found.append(("INPUT_PROP_DIRECT" in block, device))
    direct = [d for is_direct, d in found if is_direct]
    return (direct or [d for _, d in found] or [None])[0]


def parse_wm_size(text: str) -> tuple[int, int] | None:
    """The screen's size of `wm size`: the override when one is set."""
    sizes = re.findall(r"(Physical|Override) size: (\d+)x(\d+)", text)
    if not sizes:
        return None
    by = {kind: (int(w), int(h)) for kind, w, h in sizes}
    return by.get("Override", by.get("Physical"))


# The input event codes a pinch writes (linux/input-event-codes.h).
EV_SYN, EV_KEY, EV_ABS = 0, 1, 3
SYN_REPORT, BTN_TOUCH = 0, 0x14A
ABS_MT_SLOT, ABS_MT_TOUCH_MAJOR, ABS_MT_POSITION_X = 0x2F, 0x30, 0x35
ABS_MT_POSITION_Y, ABS_MT_TRACKING_ID, ABS_MT_PRESSURE = 0x36, 0x39, 0x3A


def pinch_events(
    touch: TouchDevice,
    screen: tuple[int, int],
    centre: tuple[int, int],
    radii: tuple[int, int],
    steps: int = 12,
) -> list[tuple[int, int, int]]:
    """Two fingers on a horizontal line through `centre`, `radii[0]`
    screen pixels either side of it at first and `radii[1]` at last, in
    `steps` moves: the (type, code, value) events of multi-touch protocol
    B, the screen's pixels mapped onto the device's axes."""
    sw, sh = screen
    cx, cy = centre

    def to_device(x: float, y: float) -> tuple[int, int]:
        dx = touch.x_min + round(max(0, min(sw - 1, x)) * (touch.x_max - touch.x_min) / max(1, sw - 1))
        dy = touch.y_min + round(max(0, min(sh - 1, y)) * (touch.y_max - touch.y_min) / max(1, sh - 1))
        return dx, dy

    events: list[tuple[int, int, int]] = []

    def fingers(r: float, first: bool) -> None:
        for slot, side in ((0, -1), (1, 1)):
            x, y = to_device(cx + side * r, cy)
            events.append((EV_ABS, ABS_MT_SLOT, slot))
            if first:
                events.append((EV_ABS, ABS_MT_TRACKING_ID, 1269 + slot))
                if touch.touch_major:
                    events.append((EV_ABS, ABS_MT_TOUCH_MAJOR, 6))
                if touch.pressure:
                    events.append((EV_ABS, ABS_MT_PRESSURE, 60))
            events.append((EV_ABS, ABS_MT_POSITION_X, x))
            events.append((EV_ABS, ABS_MT_POSITION_Y, y))
        if first:
            events.append((EV_KEY, BTN_TOUCH, 1))
        events.append((EV_SYN, SYN_REPORT, 0))

    r0, r1 = radii
    fingers(r0, True)
    for i in range(1, steps + 1):
        fingers(r0 + (r1 - r0) * i / steps, False)
    for slot in (0, 1):
        events.append((EV_ABS, ABS_MT_SLOT, slot))
        events.append((EV_ABS, ABS_MT_TRACKING_ID, -1))
    events.append((EV_KEY, BTN_TOUCH, 0))
    events.append((EV_SYN, SYN_REPORT, 0))
    return events


def sendevent_script(path: str, events: list[tuple[int, int, int]]) -> str:
    """One shell line writing `events` to the device at `path`."""
    return "; ".join(f"sendevent {path} {t} {c} {v}" for t, c, v in events)


def parse_photo_zoom(logcat: str) -> list[dict]:
    """The viewer's settled states in a logcat, oldest first."""
    out = []
    for m in PHOTO_ZOOM.finditer(logcat):
        out.append({k: float(v) for k, v in m.groupdict().items()})
    return out


def zoom_bounds_problems(state: dict) -> list[str]:
    """What is wrong with where the viewer put the picture: an axis it is
    wider than the screen on whose edge has come inside the screen, or one
    it is narrower on where it is not centred."""
    problems = []
    for axis, start, size, screen in (
        ("x", state["left"], state["width"], state["vw"]),
        ("y", state["top"], state["height"], state["vh"]),
    ):
        if size >= screen - 1:
            if start > 1 or start + size < screen - 1:
                problems.append(f"{axis}: {size:.0f} px from {start:.0f} leaves a gap on a {screen:.0f} px screen")
        elif abs(start - (screen - size) / 2) > 1:
            problems.append(f"{axis}: {size:.0f} px at {start:.0f} is not centred on {screen:.0f} px")
    return problems


def inside(node: Node, box: Node) -> bool:
    """Whether `node`'s middle is within `box`."""
    x, y = node.center
    x1, y1, x2, y2 = box.bounds
    return x1 <= x <= x2 and y1 <= y <= y2


def speech_pcm(rate: int, ms: int) -> bytes:
    """Mono 16-bit speech-like sound, as loquax::voice::test_speech makes
    it for the tests: a voice of moving pitch in syllables with pauses."""
    out = bytearray()
    phase = 0.0
    for i in range(rate * ms // 1000):
        t = i / rate
        syllable = int(t / 0.25)
        within = t / 0.25 - syllable
        envelope = 0.0 if syllable % 4 == 3 else math.sin(math.pi * within) ** 2
        pitch = 140.0 + 40.0 * math.sin(t * 2.1)
        phase += 2 * math.pi * pitch / rate
        s = envelope * sum(math.sin(phase * h) / h for h in range(1, 8))
        out += struct.pack("<h", max(-32000, min(32000, int(s * 6000))))
    return bytes(out)


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
                selected=el.get("selected") == "true",
                enabled=el.get("enabled", "true") == "true",
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


def explore_scroll(box: tuple[int, int, int, int]) -> tuple[int, int, int, int, int]:
    """One slow step down Explore's list in `box` (its bounds): the
    swipe's x, from y, to y and its milliseconds, at most
    [`EXPLORE_SCROLL_SHARE`] of the list's height (1621: a 250 ms swipe
    over the whole list flung a section title past)."""
    x = (box[0] + box[2]) // 2
    reach = int((box[3] - box[1]) * EXPLORE_SCROLL_SHARE)
    start = box[3] - (box[3] - box[1] - reach) // 2
    return x, start, start - reach, EXPLORE_SCROLL_MS


def explore_problems(
    nodes: list[Node], titles: list[str] | None = None, first: list[Node] | None = None
) -> list[str]:
    """What is wrong with Explore's dumps (1578): a missing field, toggle or
    list, a section without its title (`titles`, the catalogue's in the
    phone's language; English without), or a tab bar other than exactly
    Chats, Map and Explore. `nodes` may be several dumps of the scrolled
    list; the tabs are counted on `first`, the first dump alone (1621: over
    13 dumps the bar counted 13 times), or on `nodes` without it."""
    problems = []
    for rid in EXPLORE_IDS:
        if not find(nodes, rid=rid):
            problems.append(f"no {rid}")
    if titles is None:
        titles = catalogue_words("en", EXPLORE_TITLE_KEYS)
    for title in titles:
        if not find(nodes, text=title):
            problems.append(f"no section {title!r}")
    tabs = tab_ids(nodes if first is None else first)
    if tabs != TABS:
        problems.append(f"tabs {tabs}, want {TABS}")
    return problems


def map_screen_problems(nodes: list[Node]) -> list[str]:
    """What is wrong with the Map tab's dump (1199): any of the old header
    controls, or a tab bar other than exactly Chats, Map and Explore."""
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


# The pins the map drew (1302, MapPane.refresh's line): all, and how many
# are nodes nearby.
MAP_PINS = re.compile(r"map pins=(?P<pins>\d+) nearby=(?P<nearby>\d+)")
# The card of a node nearby (strings_scene.xml, nearby_card).
NEARBY_CARD = "Nearby, not in your contacts"
# Two places near the Florence fixture, the nodes nearby announce.
OFFGRID_PLACES = ["43.7741,11.2551", "43.7768,11.2589"]


def map_pins(logcat: str) -> tuple[int, int] | None:
    """The newest `map pins=N nearby=M` line of a logcat; None without one."""
    found = list(MAP_PINS.finditer(logcat))
    if not found:
        return None
    last = found[-1]
    return int(last.group("pins")), int(last.group("nearby"))


def offgrid_problems(
    pins: tuple[int, int] | None, before: tuple[int, int] | None, names: list[str], wanted: list[str]
) -> list[str]:
    """What is wrong after two nodes announced (1302): fewer than two more
    pins, not two more nearby ones, a seeded name missing from the sheet."""
    problems = []
    if pins is None:
        problems.append("no 'map pins=' line in the logcat")
    else:
        was = before or (0, 0)
        if pins[0] - was[0] < len(wanted):
            problems.append(f"pins {was[0]} -> {pins[0]}, want {len(wanted)} more")
        if pins[1] - was[1] < len(wanted):
            problems.append(f"nearby pins {was[1]} -> {pins[1]}, want {len(wanted)} more")
    missing = [w for w in wanted if not any(n.startswith(w) for n in names)]
    if missing:
        problems.append(f"not in the Friends sheet: {missing}; the sheet has {names}")
    return problems


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
    wrong: not the nine pages in their order, a title without its one
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


def qr_texts(zbar: str) -> list[str]:
    """What `zbarimg --quiet --raw` read: one code a line, blank lines
    dropped."""
    return [line.strip() for line in zbar.splitlines() if line.strip()]


def first_start_qr_problems(welcome: list[str], profile: list[str], address: str | None) -> list[str]:
    """The first start's QR and the profile's (1263, finding 1): each one
    code, a contact code, of the mailbox's address when known, and the
    same."""
    problems = []
    for where, read in (("first start", welcome), ("profile", profile)):
        if len(read) != 1:
            problems.append(f"the {where} screen holds {len(read)} codes: {read}")
        elif not read[0].startswith(LXMA_PREFIX):
            problems.append(f"the {where} QR is {read[0]!r}, no contact code")
        elif address is not None and not read[0].startswith(f"{LXMA_PREFIX}{address}:"):
            problems.append(f"the {where} QR is {read[0]!r}, not of {address}")
    if len(welcome) == 1 and len(profile) == 1 and welcome != profile:
        problems.append(f"the first start's QR {welcome[0]!r} is not the profile's {profile[0]!r}")
    return problems


def link_code_problems(shown: str, scanned: list[str], server: str) -> list[str]:
    """The sheet of "Link a device" (1264): the code as text is a join
    code with the phone mailbox's server list behind it, which the walk
    set to `server`, and the QR on screen reads as the same code (a join
    code's QR raises it to capitals for the alphanumeric mode, so the case
    is not compared)."""
    problems = []
    if not shown.startswith(JOIN_CODE_PREFIX):
        problems.append(f"the code {shown!r} is no join code")
    elif not shown.endswith(f"@{server}"):
        problems.append(f"the code {shown!r} does not carry {server}")
    if len(scanned) != 1:
        problems.append(f"the sheet holds {len(scanned)} codes: {scanned}")
    elif scanned[0].lower() != shown.lower():
        problems.append(f"the QR reads {scanned[0]!r}, the text says {shown!r}")
    return problems


def moved_page_problems(nodes: list[Node]) -> list[str]:
    """The old phone's first screen after the move (1281): it says the
    phone moved, offers to take the mailbox back, and shows no join form
    (no field to type a code into)."""
    problems = []
    if not any(MOVED_TITLE in n.text for n in nodes):
        problems.append(f"no {MOVED_TITLE!r}")
    button = find(nodes, rid=TAKE_BACK_ID)
    if not button or button[0].text != TAKE_BACK:
        problems.append(f"no {TAKE_BACK!r} button")
    edits = [n for n in nodes if n.cls.endswith("EditText")]
    if edits:
        problems.append(f"{len(edits)} fields on the moved page")
    return problems


def first_start_name_problems(nodes: list[Node]) -> list[str]:
    """The address page of a phone mailbox's first start (walk finding 9):
    the name is its one field, above the QR."""
    problems = []
    field = find(nodes, rid=WELCOME_NAME)
    qr = find(nodes, rid=WELCOME_QR)
    if not field:
        problems.append("no name field")
    elif qr and field[0].bounds[1] >= qr[0].bounds[1]:
        problems.append("the name field is below the QR")
    edits = [n for n in nodes if n.cls.endswith("EditText")]
    if len(edits) != 1:
        problems.append(f"{len(edits)} fields on the page, not the name alone")
    return problems


def session_line(dumpsys: str, package: str) -> str | None:
    """The text of `package`'s session notification, the untagged one, in
    `dumpsys notification --noredact`; None when there is none."""
    for block in re.split(r"(?=NotificationRecord\()", dumpsys):
        head = re.match(r"NotificationRecord\([^\n]*", block)
        if not head or f"pkg={package} " not in head.group(0) + " ":
            continue
        tag = re.search(r"\btag=(\S+)", head.group(0))
        if tag and tag.group(1) != "null":
            continue
        text = re.search(r"android\.text=\S+ \((.*)\)\s*$", block, re.M)
        if text:
            return text.group(1)
    return None


def session_line_problems(line: str | None, server: str) -> list[str]:
    """What is wrong with the session notification's line (walk finding
    19): none, not "Connected to your mailbox" (no unread count since
    1345), or the server's address on it."""
    if line is None:
        return ["no session notification"]
    problems = []
    if not SESSION_LINE.fullmatch(line):
        problems.append(f"the line says {line!r}")
    host = server.rpartition(":")[0]
    if server in line or (host and host in line):
        problems.append(f"the line names the server: {line!r}")
    return problems


def daemon_word_problems(nodes: list[Node]) -> list[str]:
    """A word of Reticulum or the daemon anywhere on screen."""
    return [
        f"on screen: {t!r} says {word!r}"
        for t in texts(nodes)
        for word in DAEMON_WORDS
        if word in t
    ]


def pencil_problems(nodes: list[Node], camera: bool) -> list[str]:
    """The pencil's screen (1263, finding 3): "Scan a code" first where
    there is a camera, "Paste a code" next, the typed address folded
    away under Advanced, no daemon word."""
    scan = find(nodes, rid=PENCIL_SCAN)
    paste = find(nodes, rid=PENCIL_PASTE)
    problems = []
    if not paste:
        problems.append("no Paste a code")
    if camera and not scan:
        problems.append("a camera and no Scan a code")
    if scan and paste and scan[0].bounds[1] >= paste[0].bounds[1]:
        problems.append("Scan a code is not above Paste a code")
    if not find(nodes, rid=PENCIL_ADVANCED):
        problems.append("no Advanced")
    if find(nodes, rid="address"):
        problems.append("the address field shows before Advanced is opened")
    return problems + daemon_word_problems(nodes)


def chat_menu_problems(menu: list[str]) -> list[str]:
    """The chat bar's menu (1283, finding 28): Contact info is there,
    Refresh is gone."""
    problems = [] if CONTACT_INFO in menu else ["no Contact info in the menu"]
    return problems + [f"the menu still offers {gone!r}" for gone in CHAT_MENU_GONE if gone in menu]


def contact_fold_problems(closed: list[Node], opened: list[Node]) -> list[str]:
    """The contact screen's Advanced fold (1283, finding 29): `closed` is
    the screen as it opens, `opened` every node seen after the header's
    tap, scrolled down. Closed: the header shows, its body does not, and
    neither do the identity, the network or a word of Reticulum's. Opened:
    the body shows, and the identity and the network are in it."""
    problems = []
    if not find(closed, rid=CONTACT_ADVANCED):
        problems.append("no Advanced")
    if find(closed, rid=CONTACT_ADVANCED_BODY):
        problems.append("Advanced is open before the tap")
    shown = texts(closed)
    for header in CONTACT_FOLDED:
        if header in shown:
            problems.append(f"{header} on the main rows")
    for text in shown:
        for word in CONTACT_MAIN_WORDS:
            if word in text:
                problems.append(f"a main row says {word!r}: {text!r}")
    if not find(opened, rid=CONTACT_ADVANCED_BODY):
        problems.append("Advanced did not open")
    seen = texts(opened)
    for header in CONTACT_FOLDED:
        if header not in seen:
            problems.append(f"no {header} under Advanced")
    return problems + decode_problems(closed + opened)


def join_error_problems(nodes: list[Node], said: str) -> list[str]:
    """A failed join (1263, finding 8): the one sentence `said`, Retry,
    the Details fold, no daemon word in the sentence."""
    error = find(nodes, rid="error")
    problems = []
    shown = error[0].text if error else None
    if shown != said:
        problems.append(f"the join says {shown!r}, not {said!r}")
    if not find(nodes, rid=JOIN_RETRY):
        problems.append("no Retry")
    if not find(nodes, rid=JOIN_DETAILS_TOGGLE):
        problems.append("no Details")
    if shown is not None:
        problems += [f"the sentence says {word!r}" for word in DAEMON_WORDS if word in shown]
    return problems


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


def delete_all_row_problems(nodes: list[Node]) -> tuple[str, list[str]]:
    """The Maps page's last row (1290): its total, and what is wrong: no
    row, a label that is not "Delete all downloaded maps (size)", a total
    other than the storage line's, the row above a region, the row off
    while regions are listed or on while none are."""
    row = find(nodes, rid=DELETE_ALL)
    if not row:
        return "?", ["no Delete all row"]
    label = DELETE_ALL_LABEL.fullmatch(row[0].text)
    regions = find(nodes, rid="maps_region_name")
    used = find(nodes, rid="maps_used")
    problems = []
    total = label.group(1) if label else "?"
    if label is None:
        problems.append(f"the row says {row[0].text!r}")
    else:
        size = FILE_SIZE.search(used[0].text) if used else None
        if size is None or size.group(0) != total:
            problems.append(
                f"the row's total {total!r} is not the storage line's "
                f"{used[0].text if used else None!r}"
            )
    if any(r.bounds[1] > row[0].bounds[1] for r in regions):
        problems.append("the row is not below every region")
    if row[0].enabled != bool(regions):
        state = "on" if row[0].enabled else "off"
        problems.append(f"the row is {state} with {len(regions)} regions")
    return total, problems


def delete_all_ask_problems(nodes: list[Node], regions: int) -> list[str]:
    """The row's question (1290): what is wrong: no question in the
    automatic download's words, or one with a count other than the
    regions listed, or no Delete all to say yes with."""
    asked = [m for t in texts(nodes) if (m := DELETE_ALL_ASK.fullmatch(t))]
    if not asked:
        return [f"no question; screen: {texts(nodes)[:8]}"]
    problems = []
    count = int(asked[0].group(1) or asked[0].group(2))
    if count != regions:
        problems.append(f"the question counts {count} maps, the page listed {regions}")
    if not find(nodes, text=DELETE_ALL_YES):
        problems.append(f"no {DELETE_ALL_YES!r} button")
    return problems


def privacy_page_problems(nodes: list[Node]) -> tuple[bool | None, list[str]]:
    """The Privacy page's read receipts row (1233): the switch's state,
    None without one, and what is wrong: no switch, no row title or no
    line under it, the row not above the blocked contacts, a line that
    did not decode."""
    switch = find(nodes, rid=RECEIPTS_SWITCH)
    shown = texts(nodes)
    problems = []
    if not switch:
        problems.append("no read receipts switch")
    if RECEIPTS_ROW not in shown:
        problems.append(f"no {RECEIPTS_ROW!r}")
    if not any(t.startswith(RECEIPTS_LINE) for t in shown):
        problems.append("no line under the read receipts row")
    blocked = find(nodes, text=BLOCKED_HEADER)
    if switch and blocked and switch[0].bounds[1] > blocked[0].bounds[1]:
        problems.append("the read receipts row is not above the blocked contacts")
    problems += decode_problems(nodes)
    return (switch[0].checked if switch else None), problems


def receipts_rungs_problems(seen: set[str], on: bool) -> list[str]:
    """The imported chat's rungs as seen with read receipts `on` or off
    (1233): on, the read rung is there; off, it is not, and its message
    shows two grey ticks instead."""
    if on:
        return [] if "state_ticks_read" in seen else ["on: no read mark"]
    problems = []
    if "state_ticks_read" in seen:
        problems.append("off: a read mark still shows")
    if "state_ticks" not in seen:
        problems.append("off: no delivered mark")
    return problems


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


def middle_half(width: int, height: int) -> tuple[int, int, int, int]:
    """The middle half of a screen in each direction: inside the map
    whatever bars the screen has, away from the attribution."""
    return (width // 4, height // 4, width * 3 // 4, height * 3 // 4)


def colour_share(
    samples: list[tuple[int, int, int]],
    colours: list[tuple[int, int, int]],
    tolerance: int = 3,
) -> float:
    """The share of `samples` within `tolerance` of any of `colours`, per channel."""
    if not samples:
        return 0.0
    hits = sum(
        1
        for p in samples
        if any(all(abs(a - b) <= tolerance for a, b in zip(p, c)) for c in colours)
    )
    return hits / len(samples)


def buildings_problems(z16_share: float, z15_share: float, z16_ms: int, z15_ms: int) -> list[str]:
    """The buildings step's verdict (1193): the fill on screen at z16 and
    not at z15, and z16's time to rest within the bound of z15's."""
    problems = []
    if z16_share < BUILDINGS_LEAST:
        problems.append(f"z16 fill {z16_share:.3f} under {BUILDINGS_LEAST}")
    if z15_share > BUILDINGS_Z15_MOST:
        problems.append(f"z15 fill {z15_share:.3f} over {BUILDINGS_Z15_MOST}")
    bound = int(z15_ms * SETTLE_FACTOR) + SETTLE_SLACK_MS
    if z16_ms > bound:
        problems.append(f"z16 at rest after {z16_ms} ms, over {bound} ms (z15 {z15_ms} ms)")
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


def parse_density(text: str) -> int | None:
    """The screen's density from `wm density`: the override when one is
    set, else the physical one; None when neither is said."""
    found = {}
    for line in text.splitlines():
        m = re.match(r"\s*(Physical|Override) density:\s*(\d+)", line)
        if m:
            found[m.group(1)] = int(m.group(2))
    return found.get("Override", found.get("Physical"))


def pin_in_row(nodes: list[Node], avatar: Node) -> Node | None:
    """The map pin of the list row whose avatar is `avatar` (1224): the pin
    whose middle lies within the avatar's height."""
    top, bottom = avatar.bounds[1], avatar.bounds[3]
    for node in find(nodes, rid="pin"):
        if top <= node.center[1] <= bottom:
            return node
    return None


def pin_problems(pin: Node | None, density: int | None) -> list[str]:
    """What is wrong with a row's map pin: none there, no label of the
    catalogue's, a tap target under 48dp."""
    if pin is None:
        return ["no map pin in the row"]
    problems = []
    if not pin.desc.startswith(PIN_LABEL):
        problems.append(f"the pin says {pin.desc!r}, not {PIN_LABEL!r}...")
    if density:
        least = PIN_DP * density / 160 - 1
        x1, y1, x2, y2 = pin.bounds
        if x2 - x1 < least or y2 - y1 < least:
            problems.append(f"the pin's target is {x2 - x1}x{y2 - y1} px, under {PIN_DP}dp at {density} dpi")
    return problems


def last_in_row(nodes: list[Node], avatar: Node) -> Node | None:
    """The last line of the list row whose avatar is `avatar`: the `last`
    whose middle lies within the avatar's height."""
    top, bottom = avatar.bounds[1], avatar.bounds[3]
    for node in find(nodes, rid="last"):
        if top <= node.center[1] <= bottom:
            return node
    return None


def draft_problems(last: Node | None, typed: str) -> list[str]:
    """What is wrong with a row's draft line (1219): none there, or not
    `Draft: ` and what was typed."""
    if last is None:
        return ["no last line in the row"]
    want = DRAFT_PREFIX + typed
    if last.text != want:
        return [f"the row says {last.text!r}, not {want!r}"]
    return []


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


# A network change (1298): the line the screens show while a connection
# the change dropped is not back (strings.xml `reconnecting`), and how long
# the three walk steps keep the network away. The order's flight mode is
# two minutes; `--flight-secs` shortens it for a quick run.
RECONNECTING = "Reconnecting\u2026"
WIFI_OFF_SECS = 10
MOBILE_SECS = 20
# How long after the network is back the host's message may take to show.
BACK_BUDGET_SECS = 90

ACTIVE_DEFAULT = re.compile(r"Active default network:\s*(\S+)")


def default_network(dumpsys: str) -> str:
    """The kind of the default network in `dumpsys connectivity`: `none`,
    `wifi`, `mobile` or `other`. The agent line of the active network names
    its transports (`Transports: WIFI`, `CELLULAR`); one that cannot be
    found is `other`, never a guess at wifi."""
    m = ACTIVE_DEFAULT.search(dumpsys)
    if not m or m.group(1) in ("none", "null"):
        return "none"
    netid = m.group(1)
    for line in dumpsys.splitlines():
        if f"network{{{netid}}}" not in line or "Transports:" not in line:
            continue
        transports = line.split("Transports:", 1)[1].split("Capabilities:", 1)[0]
        if "WIFI" in transports:
            return "wifi"
        if "CELLULAR" in transports:
            return "mobile"
        return "other"
    return "other"


def network_detail(
    change: str,
    off_secs: int,
    kinds: tuple[str, str, str],
    shown: list[str],
    line_gone_ms: int | None,
    bubble_ms: int,
) -> str:
    """One network step's WALK detail: the change, how long the network was
    away, the kinds before, during and after, the lines the screen showed
    meanwhile (each once, in order), when the "Reconnecting…" line was
    gone and when the host's message showed, both counted from the moment
    the network came back."""
    seen: list[str] = []
    for line in shown:
        if line and line not in seen:
            seen.append(line)
    before, during, after = kinds
    gone = "never_shown" if line_gone_ms is None else str(line_gone_ms)
    return (
        f"change={change} off_s={off_secs} kinds={before}>{during}>{after} "
        f"shown={'|'.join(seen) or '-'} line_gone_ms={gone} bubble_ms={bubble_ms}"
    )


NETWORK_DETAIL = re.compile(
    r"change=(?P<change>\S+) off_s=(?P<off>\d+) kinds=(?P<kinds>\S+) "
    r"shown=(?P<shown>.*?) line_gone_ms=(?P<gone>\S+) bubble_ms=(?P<bubble>\d+)"
)


def network_rows(walk_log: str) -> list[str]:
    """The network steps of a walk.log as the report's table rows:
    `| change | off s | kinds | shown | line gone ms | message ms |`."""
    rows = []
    for line in walk_log.splitlines():
        m = NETWORK_DETAIL.search(line)
        if m:
            rows.append(
                f"| {m['change']} | {m['off']} | {m['kinds']} | {m['shown']} "
                f"| {m['gone']} | {m['bubble']} |"
            )
    return rows


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

    def swipe_async(self, x1: int, y1: int, x2: int, y2: int, ms: int) -> subprocess.Popen:
        """A swipe that runs while the walk dumps the screen: a hold of the
        microphone is a swipe that does not move (1276)."""
        return subprocess.Popen(
            [self.adb, "-s", self.serial, "shell", f"input swipe {x1} {y1} {x2} {y2} {ms}"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

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
        # The phone holds the mailbox since first_start_name (1278).
        self.on_phone = False
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
        # The join leads to the address page once (walk finding 13), its
        # Done to the list, whose title is the app's name.
        address_page = False
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            tapped += self.dismiss_dialogs(1)
            nodes = dev.dump()
            if find(nodes, rid=WELCOME_QR):
                address_page = True
                self.tap_done(nodes)
            elif find(nodes, rid="list") and find(nodes, rid="write"):
                if not address_page:
                    raise StepFailed("the list came without the address page after the join")
                return f"joined address_page=shown dialogs={tapped or 'none'}"
            time.sleep(0.5)
        raise StepFailed(f"no conversation list 30s after Join; screen: {texts(nodes)[:12]}")

    def tap_done(self, nodes: list[Node]) -> None:
        """Done on the address page, scrolled to when below the fold."""
        dev = self.need_device()
        done = find(nodes, text=DONE)
        if done:
            dev.tap_node(done[0])
            return
        x1, y1, x2, y2 = find(nodes, rid=WELCOME_QR)[0].bounds
        x = (x1 + x2) // 2
        dev.swipe(x, y2, x, y1, 300)
        time.sleep(0.5)

    def step_notification_text(self) -> str:
        """The always-on notification (walk finding 19): joined to the
        host's mailbox, its line says "Connected to your mailbox", with no
        unread count (1345), and never the server's address."""
        dev = self.need_device()
        seen: list[str | None] = []

        def look():
            line = session_line(dev.shell("dumpsys notification --noredact"), PACKAGE)
            seen.append(line)
            return not session_line_problems(line, self.args.server)

        if not wait_for(look, 30, every=1.0):
            problems = session_line_problems(seen[-1] if seen else None, self.args.server)
            raise StepFailed("; ".join(problems) + f"; lines seen: {sorted(set(map(str, seen)))}")
        return f"line={seen[-1]!r}"

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

    def step_contact_info(self) -> str:
        """The chat menu and the contact screen (1283, findings 28 and 29):
        the menu has Contact info and no Refresh; the screen opens with
        Advanced closed and no identity, network or hash on it; a tap opens
        the fold, and down it the identity and the network show; back to
        the chat."""
        dev = self.need_device()
        dev.tap_node(self.wait_id("menu", 10)[0])
        self.wait_text(CONTACT_INFO, 10)
        menu = texts(dev.dump())
        problems = chat_menu_problems(menu)
        self.tap_text(CONTACT_INFO)
        header = self.wait_id(CONTACT_ADVANCED, 10)
        closed = dev.dump()
        dev.tap_node(header[0])
        self.wait_id(CONTACT_ADVANCED_BODY, 10)
        opened: list[Node] = []
        for _ in range(6):
            nodes = dev.dump()
            opened += nodes
            if all(h in texts(opened) for h in CONTACT_FOLDED):
                break
            self.scroll_list(nodes, CONTACT_SCROLL, up=False)
        problems += contact_fold_problems(closed, opened)
        self.note(f"contact info: menu={menu} closed={texts(closed)[:16]}")
        dev.key("KEYCODE_BACK")
        self.wait_id("compose", 10)
        if problems:
            raise StepFailed("; ".join(problems))
        return f"menu=no_refresh advanced=closed_then_open folded={CONTACT_FOLDED}"

    def step_marks(self) -> str:
        """The imported chat holds one sent message on each rung of the
        ladder (ugly_store's delivery_of): down from the top until all five
        marks were on screen, each with its own id and its own words."""
        dev = self.need_device()
        seen: dict[str, str] = {}
        last: list[str] = []
        for scrolls in range(60):
            nodes = dev.dump()
            for node in marks_on(nodes):
                seen.setdefault(mark_rid(node), node.desc)
            if len(seen) == len(MARK_WORDS):
                break
            bodies = [n.text for n in by_top(find(nodes, rid="body"))]
            if bodies and bodies == last:
                break
            last = bodies
            self.scroll_list(nodes, "list", up=False)
        problems = marks_problems(seen)
        self.note(f"marks seen: {seen}")
        if problems:
            raise StepFailed("; ".join(problems))
        return f"scrolls={scrolls} rungs={sorted(seen)}"

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
            marks = [n for n in by_top(marks_on(nodes)) if n.bounds[1] >= y]
            mark = mark_rid(marks[0]) if marks else ""
            return mark in TICKS

        shown = wait_for(ticked, 5, every=0.2)
        ms = int((time.monotonic() - start) * 1000)
        if not shown:
            raise StepFailed(f"no bubble {body!r} with a tick within 5s; mark={mark!r}")
        accepted = wait_for(lambda: "HOME send accepted" in fixture.mailbox_log(), 10)
        if not accepted:
            raise StepFailed("the mailbox log has no 'HOME send accepted'")
        return f"bubble_ms={ms} mark={mark!r} mailbox=accepted"

    def clear_compose(self) -> None:
        """Empty the compose field, as the voice step does."""
        dev = self.need_device()
        compose = self.wait_id("compose", 5)[0]
        if compose.text.strip():
            dev.tap_node(compose)
            dev.shell("input keyevent KEYCODE_MOVE_END")
            for _ in range(len(compose.text) + 2):
                dev.key("KEYCODE_DEL")

    def step_compose_bar(self) -> str:
        """The compose bar as Signal has it (1270): with an empty field
        the emoji left of it, the position button, the camera and the
        microphone right of it and the round plus outside it, five, each
        named for a screen reader; a typed letter turns the plus into Send
        and hides the microphone; the letter deleted turns it back."""
        dev = self.need_device()
        self.clear_compose()
        problems: list[str] = []

        def bar_is(typed: bool) -> bool:
            nonlocal problems
            problems = bar_problems(dev.dump(), typed)
            return not problems

        if not wait_for(lambda: bar_is(False), 5, every=0.3):
            raise StepFailed("empty field: " + "; ".join(problems))
        empty = [bar_id(n) for n in bar_on(dev.dump())]
        dev.tap_node(self.wait_id("compose", 5)[0])
        start = time.monotonic()
        dev.type_text("x")
        if not wait_for(lambda: bar_is(True), 5, every=0.2):
            raise StepFailed("typed: " + "; ".join(problems))
        swap_ms = int((time.monotonic() - start) * 1000)
        dev.key("KEYCODE_DEL")
        if not wait_for(lambda: bar_is(False), 5, every=0.3):
            raise StepFailed("deleted again: " + "; ".join(problems))
        return f"empty={','.join(empty)} typed=send swap_ms={swap_ms} back=plus"

    def step_camera_photo(self) -> str:
        """A photo through the compose bar's camera (1270): the fixture put
        where the debug build takes its capture from (an emulator has no
        camera app), the camera tapped; the staged photo shows with its
        words and the round button is Send; a caption typed and Send make
        one bubble with the picture, the caption and a mark, the mailbox's
        send; the bar is the plus again."""
        dev = self.need_device()
        fixture = self.need_fixture()
        if not FIXTURE_PHOTO.is_file():
            raise StepFailed(f"{FIXTURE_PHOTO} missing")
        dev.run("push", str(FIXTURE_PHOTO), f"/data/local/tmp/{CAMERA_FIXTURE}", timeout=60)
        dev.shell(
            f"run-as {PACKAGE} sh -c 'cat /data/local/tmp/{CAMERA_FIXTURE} > files/{CAMERA_FIXTURE}'"
        )
        caption = f"walk caption {self.nonce}"
        try:
            self.clear_compose()
            sent_before = fixture.mailbox_log().count("HOME send accepted")
            before = len(photos_on(dev.dump()))
            dev.tap_node(self.wait_id("camera", 5)[0])
            staged = self.wait_id("staged_words", 15)[0]
            if not STAGED_WORDS.match(staged.text):
                raise StepFailed(f"the staged photo says {staged.text!r}")
            nodes = dev.dump()
            if not find(nodes, rid="send") or find(nodes, rid="plus"):
                raise StepFailed(f"with a staged photo the round button is {[bar_id(n) for n in bar_on(nodes)]}")
            dev.tap_node(self.wait_id("compose", 5)[0])
            dev.type_text(caption)
            start = time.monotonic()
            dev.tap_node(self.wait_id("send", 5)[0])
            problems: list[str] = []

            def shown():
                nonlocal problems
                nodes = dev.dump()
                problems = photo_problems(photos_on(nodes), before)
                if not problems:
                    problems = caption_bubble_problems(nodes, photos_on(nodes)[-1], caption)
                return not problems

            if not wait_for(shown, 15, every=0.3):
                raise StepFailed("; ".join(problems))
            ms = int((time.monotonic() - start) * 1000)
            accepted = wait_for(
                lambda: fixture.mailbox_log().count("HOME send accepted") > sent_before, 30
            )
            if not accepted:
                raise StepFailed("the mailbox log has no new 'HOME send accepted'")
            nodes = dev.dump()
            if find(nodes, rid="staged_words"):
                raise StepFailed("the staged photo stayed after the send")
            problems = bar_problems(nodes, typed=False)
            if problems:
                raise StepFailed("after the send: " + "; ".join(problems))
            return f"staged={staged.text!r} sent_ms={ms} bubbles=1 caption=in mark=in mailbox=accepted"
        finally:
            dev.shell(f"run-as {PACKAGE} rm -f files/{CAMERA_FIXTURE}", check=False)
            dev.shell(f"rm -f /data/local/tmp/{CAMERA_FIXTURE}", check=False)

    def step_share_position_from_bar(self) -> str:
        """The position one tap away (1270, Lew 2026-11-01 13:30): a fix
        seeded where the debug build reads the phone's from, the bar's
        position button opens the sheet with the fix's age and "Send my
        position now"; its tap sends the position report to this chat:
        the mailbox accepts a send for the open conversation's peer and
        the sheet closes. The peer's own session is not run by the walk,
        so the mailbox's acceptance for it is where the walk can see the
        report go."""
        dev = self.need_device()
        fixture = self.need_fixture()
        peer = fixture.manifest["sender_peer"]
        dev.shell(f"run-as {PACKAGE} sh -c 'echo {SEEDED_FIX} > files/{FIX_FIXTURE}'")
        try:
            before = accepted_to(fixture.mailbox_log(), peer)
            dev.tap_node(self.wait_id("locate", 5)[0])
            words = self.wait_id(SHEET_FIX_WORDS, 10)[0]
            if not words.text.startswith("Found"):
                raise StepFailed(f"the sheet says {words.text!r}, not the fix's age")
            start = time.monotonic()
            dev.tap_node(self.wait_id(SHEET_SHARE_NOW, 5)[0])
            if not wait_for(lambda: accepted_to(fixture.mailbox_log(), peer) > before, 30):
                raise StepFailed(f"the mailbox accepted no send to {peer[:8]}")
            ms = int((time.monotonic() - start) * 1000)
            if not wait_for(lambda: not find(dev.dump(), rid="sheet"), 5, every=0.3):
                raise StepFailed("the sheet stayed open after the share")
            return f"sheet={words.text!r} accepted_ms={ms} to={peer[:8]} sheet=closed"
        finally:
            dev.shell(f"run-as {PACKAGE} rm -f files/{FIX_FIXTURE}", check=False)

    def share_from_sheet(self, row: str, words) -> tuple[Device, Fixture, str, int]:
        """Seed a fix, open the position sheet, tap `row`: the chip says
        `words` within 10 s and the mailbox accepts the first report for
        the open chat's peer within 30 s. The device, the fixture, the
        peer and the acceptances before."""
        dev = self.need_device()
        fixture = self.need_fixture()
        peer = fixture.manifest["sender_peer"]
        dev.shell(f"run-as {PACKAGE} sh -c 'echo {SEEDED_FIX} > files/{FIX_FIXTURE}'")
        before = accepted_to(fixture.mailbox_log(), peer)
        dev.tap_node(self.wait_id("locate", 5)[0])
        dev.tap_node(self.wait_id(row, 10)[0])
        problems: list[str] = []

        def chip():
            nonlocal problems
            problems = chip_problems(dev.dump(), words)
            return not problems

        if not wait_for(chip, 10, every=0.3):
            raise StepFailed("; ".join(problems))
        if not wait_for(lambda: accepted_to(fixture.mailbox_log(), peer) > before, 30):
            raise StepFailed(f"the share's first report did not reach the mailbox for {peer[:8]}")
        return dev, fixture, peer, before

    def stop_share(self, dev: Device, fixture: Fixture, peer: str) -> int:
        """Tap the chip's Stop: the chip goes within 5 s, and the end
        report, one more acceptance carrying the end mark, reaches the
        mailbox's outbox for the peer within 30 s. The milliseconds from
        the tap to the chip's going."""
        store = fixture.mailbox / "history.sqlite3"
        ended = end_reports(store, peer)
        start = time.monotonic()
        dev.tap_node(self.wait_id(SHARE_STOP, 5)[0])
        if not wait_for(lambda: not find(dev.dump(), rid=SHARE_CHIP), 5, every=0.3):
            raise StepFailed("the chip stayed after Stop")
        ms = int((time.monotonic() - start) * 1000)
        if not wait_for(lambda: end_reports(store, peer) > ended, 30):
            raise StepFailed(f"no end report for {peer[:8]} in the mailbox's outbox")
        return ms

    def step_share_for_15_min(self) -> str:
        """A share for 15 minutes (1273): the sheet's row starts it, the
        chip says the time left, the first report goes at once; a second
        seeded fix 44 m away is a move and goes once the share's minute
        has passed (the share service reads the seeded fix every 15 s);
        Stop takes the chip away and sends the end report, which the
        mailbox's outbox holds for the peer with its end mark. The peer's
        own session is not run by the walk (1270's gap): its mailbox leg
        is where the walk sees the end report go."""
        dev, fixture, peer, before = self.share_from_sheet(SHEET_SHARE_15_MIN, SHARE_LEFT)
        try:
            dev.shell(f"run-as {PACKAGE} sh -c 'echo {MOVED_FIX} > files/{FIX_FIXTURE}'")
            moved = wait_for(lambda: accepted_to(fixture.mailbox_log(), peer) > before + 1, 120)
            if not moved:
                raise StepFailed("the move of 44 m sent no second report within 2 min")
            ms = self.stop_share(dev, fixture, peer)
            return f"chip=left reports=2 moved=sent stop_to_gone_ms={ms} end=in_outbox to={peer[:8]}"
        finally:
            dev.shell(f"run-as {PACKAGE} rm -f files/{FIX_FIXTURE}", check=False)

    def step_share_until_stopped(self) -> str:
        """A share until stopped (1273): the chip says so, and Stop ends
        it with the end report as in share_for_15_min."""
        dev, fixture, peer, _ = self.share_from_sheet(SHEET_SHARE_UNTIL, SHARE_UNTIL)
        try:
            ms = self.stop_share(dev, fixture, peer)
            return f"chip=until reports=1 stop_to_gone_ms={ms} end=in_outbox to={peer[:8]}"
        finally:
            dev.shell(f"run-as {PACKAGE} rm -f files/{FIX_FIXTURE}", check=False)

    def step_image(self) -> str:
        """A photo through the paperclip (1220): the fixture onto the
        phone's pictures, the system's picker, the bubble's thumbnail
        with its words and a tick, one bubble with its mark and no text
        row (1268), the mailbox's send, the whole photo on
        a tap and Back to the chat."""
        dev = self.need_device()
        fixture = self.need_fixture()
        if not FIXTURE_PHOTO.is_file():
            raise StepFailed(f"{FIXTURE_PHOTO} missing")
        name = f"walk-photo-{self.nonce}.jpg"
        dev.run("push", str(FIXTURE_PHOTO), f"/sdcard/Pictures/{name}", timeout=60)
        dev.shell(
            "am broadcast -a android.intent.action.MEDIA_SCANNER_SCAN_FILE "
            f"-d file:///sdcard/Pictures/{name}",
            check=False,
        )
        sent_before = fixture.mailbox_log().count("HOME send accepted")
        before = len(photos_on(dev.dump()))
        dev.tap_node(self.wait_id("plus", 5)[0])
        dev.tap_node(self.wait_id(SHEET_GALLERY, 5)[0])
        # The system's picker: the file by its name, in its recent or its
        # pictures list.
        start = time.monotonic()
        dev.tap_node(self.wait_text(name, 20, contains=True)[0])
        problems: list[str] = []

        def shown():
            nonlocal problems
            nodes = dev.dump()
            problems = photo_problems(photos_on(nodes), before)
            return not problems

        if not wait_for(shown, 15, every=0.3):
            raise StepFailed("; ".join(problems))
        ms = int((time.monotonic() - start) * 1000)
        accepted = wait_for(
            lambda: fixture.mailbox_log().count("HOME send accepted") > sent_before, 30
        )
        if not accepted:
            raise StepFailed("the mailbox log has no new 'HOME send accepted'")
        nodes = dev.dump()
        photo = photos_on(nodes)[-1]
        # One bubble, the photo and its mark in it, no empty one (1268).
        problems = photo_bubble_problems(nodes, photo)
        if problems:
            raise StepFailed("; ".join(problems))
        dev.tap_node(photo)
        self.wait_id(PHOTO_FULL, 10)
        dev.key("KEYCODE_BACK")
        self.wait_id("compose", 10)
        dev.shell(f"rm -f /sdcard/Pictures/{name}", check=False)
        return (
            f"picked_to_bubble_ms={ms} desc={photo.desc!r} bubbles=1 mark=in "
            "mailbox=accepted full=shown"
        )

    def step_picture_zoom(self) -> str:
        """The big picture zooms under two fingers (1269): the image
        step's photo opened whole, a pinch out about its centre written
        to the screen's multi-touch device with `sendevent` (`input` has
        one pointer only), then a pan, a double tap back to 1x and Back
        to the chat. The zoom's numbers are the viewer's own `PHOTO_ZOOM`
        line in the log; that the picture moved is read off the screen."""
        dev = self.need_device()
        photos = photos_on(dev.dump())
        if not photos:
            raise StepFailed("no photo in the open chat: the image step leaves one")
        touch = parse_touch_device(dev.shell("getevent -pl", timeout=30, check=False))
        if touch is None:
            raise StepFailed("getevent -pl lists no multi-touch device to pinch on")
        size = parse_wm_size(dev.shell("wm size"))
        if size is None:
            raise StepFailed("wm size says no screen size")
        dev.shell("logcat -c", check=False)
        dev.tap_node(photos[-1])
        box = self.wait_id(PHOTO_FULL, 10)[0].bounds
        x1, y1, x2, y2 = box
        cx, cy = (x1 + x2) // 2, (y1 + y2) // 2
        states: list[dict] = []

        def settled(test) -> bool:
            nonlocal states
            states = parse_photo_zoom(dev.shell("logcat -d -s loquax:I", check=False))
            return bool(states) and test(states[-1])

        def expect(what: str, test, timeout: float = 8) -> dict:
            if not wait_for(lambda: settled(test), timeout, every=0.3):
                last = states[-1] if states else "no PHOTO_ZOOM line"
                raise StepFailed(f"{what}: {last}")
            problems = zoom_bounds_problems(states[-1])
            if problems:
                raise StepFailed(f"{what}: {'; '.join(problems)}")
            return states[-1]

        expect("the viewer shows the photo at 1x", lambda z: z["scale"] == 1.0, 10)
        time.sleep(0.5)
        before = sample_grid(dev.screen(), box)
        width = x2 - x1
        events = pinch_events(touch, size, (cx, cy), (width // 10, width * 4 // 10))
        dev.shell(sendevent_script(touch.path, events), timeout=120)
        pinched = expect("the pinch zoomed", lambda z: z["scale"] > 1.2)
        time.sleep(0.5)
        share = changed_fraction(before, sample_grid(dev.screen(), box))
        if share < 0.05:
            raise StepFailed(f"the screen did not change with the pinch ({share:.2f})")
        dev.swipe(cx + width // 6, cy, cx - width // 6, cy, 500)
        panned = expect(
            "the pan moved the zoomed picture",
            lambda z: abs(z["left"] - pinched["left"]) > 10 or abs(z["top"] - pinched["top"]) > 10,
        )
        dev.double_tap(cx, cy)
        expect("the double tap went back to 1x", lambda z: z["scale"] == 1.0)
        dev.key("KEYCODE_BACK")
        self.wait_id("compose", 10)
        return (
            f"pinch=1.000->{pinched['scale']:.3f} changed={share:.2f} "
            f"pan_dx={panned['left'] - pinched['left']:.0f} doubletap=1x back=chat "
            f"touch={touch.path}"
        )

    def step_file(self) -> str:
        """A file through the paperclip (1221): a text file onto the
        phone's downloads, the system's picker, the bubble's file row with
        its name and size, the mailbox's send; a tap saves it under the
        app's files, where run-as finds its bytes, and hands it to an app
        that opens it or says none does; Back to the chat."""
        dev = self.need_device()
        fixture = self.need_fixture()
        name = f"walk-file-{self.nonce}.txt"
        content = f"loquax device walk {self.nonce}\n".encode() * 64
        local = Path(tempfile.mkdtemp(prefix="walk-file-")) / name
        local.write_bytes(content)
        dev.run("push", str(local), f"/sdcard/Download/{name}", timeout=60)
        local.unlink()
        local.parent.rmdir()
        dev.shell(
            "am broadcast -a android.intent.action.MEDIA_SCANNER_SCAN_FILE "
            f"-d file:///sdcard/Download/{name}",
            check=False,
        )
        sent_before = fixture.mailbox_log().count("HOME send accepted")
        before = len(files_on(dev.dump()))
        dev.tap_node(self.wait_id("plus", 5)[0])
        dev.tap_node(self.wait_id(SHEET_FILE, 5)[0])
        start = time.monotonic()
        dev.tap_node(self.wait_text(name, 20, contains=True)[0])
        problems: list[str] = []

        def shown():
            nonlocal problems
            problems = file_problems(files_on(dev.dump()), before, name)
            return not problems

        if not wait_for(shown, 15, every=0.3):
            raise StepFailed("; ".join(problems))
        ms = int((time.monotonic() - start) * 1000)
        accepted = wait_for(
            lambda: fixture.mailbox_log().count("HOME send accepted") > sent_before, 30
        )
        if not accepted:
            raise StepFailed("the mailbox log has no new 'HOME send accepted'")
        row = files_on(dev.dump())[-1]
        dev.tap_node(row)
        saved = ""

        def stored():
            nonlocal saved
            saved = dev.shell(
                f"run-as {PACKAGE} sh -c 'cat files/files/*/{name}' 2>/dev/null", check=False
            )
            return saved.encode() == content

        if not wait_for(stored, 10, every=0.5):
            raise StepFailed(f"files/files/*/{name} holds {len(saved)} bytes, want {len(content)}")
        time.sleep(2)
        nodes = dev.dump()
        if find(nodes, rid="compose") and not any(n.text == FILE_NO_APP for n in nodes):
            opened = "the chat stayed and said nothing"
            raise StepFailed(f"a tap on the file opened nothing: {opened}")
        handed = "no_app" if any(n.text == FILE_NO_APP for n in nodes) else "another_app"
        if handed == "another_app":
            dev.key("KEYCODE_BACK")
            self.wait_id("compose", 10)
        dev.shell(f"rm -f /sdcard/Download/{name}", check=False)
        return f"picked_to_bubble_ms={ms} desc={row.desc!r} mailbox=accepted saved=ok open={handed}"

    def step_voice(self) -> str:
        """A voice message through the microphone (1222): the fixture PCM
        put where the debug build records it from, the permission granted
        as the first press would ask for it, the send button held for three
        seconds while the compose field is empty; the bubble's player with
        its length, the mailbox's send; its play button turns to pause and
        back at the end; a hold slid left sends nothing."""
        dev = self.need_device()
        fixture = self.need_fixture()
        local = Path(tempfile.mkdtemp(prefix="walk-voice-")) / VOICE_FIXTURE
        local.write_bytes(speech_pcm(16_000, 8_000))
        dev.run("push", str(local), f"/data/local/tmp/{VOICE_FIXTURE}", timeout=60)
        local.unlink()
        local.parent.rmdir()
        dev.shell(
            f"run-as {PACKAGE} sh -c 'cat /data/local/tmp/{VOICE_FIXTURE} > files/{VOICE_FIXTURE}'"
        )
        dev.shell(f"pm grant {PACKAGE} android.permission.RECORD_AUDIO", check=False)
        try:
            compose = self.wait_id("compose", 5)[0]
            if compose.text.strip():
                dev.tap_node(compose)
                dev.shell("input keyevent KEYCODE_MOVE_END")
                for _ in range(len(compose.text) + 2):
                    dev.key("KEYCODE_DEL")
            sent_before = fixture.mailbox_log().count("HOME send accepted")
            before = len(voices_on(dev.dump()))
            mic = self.wait_id("record", 5)[0]
            dev.long_press(mic, 3_000)
            start = time.monotonic()
            problems: list[str] = []

            def shown():
                nonlocal problems
                problems = voice_problems(voices_on(dev.dump()), before, 2)
                return not problems

            if not wait_for(shown, 15, every=0.3):
                raise StepFailed("; ".join(problems))
            ms = int((time.monotonic() - start) * 1000)
            accepted = wait_for(
                lambda: fixture.mailbox_log().count("HOME send accepted") > sent_before, 30
            )
            if not accepted:
                raise StepFailed("the mailbox log has no new 'HOME send accepted'")
            nodes = dev.dump()
            player = voices_on(nodes)[-1]
            play = [n for n in find(nodes, desc=VOICE_PLAY) if inside(n, player)]
            if not play:
                raise StepFailed(f"no {VOICE_PLAY!r} button in {player.desc!r}")
            dev.tap_node(play[0])
            if not wait_for(lambda: any(inside(n, player) for n in find(dev.dump(), desc=VOICE_PAUSE)), 5, every=0.3):
                raise StepFailed(f"a tap on {VOICE_PLAY!r} did not turn it to {VOICE_PAUSE!r}")
            if not wait_for(lambda: any(inside(n, player) for n in find(dev.dump(), desc=VOICE_PLAY)), 15, every=0.5):
                raise StepFailed(f"the player never came back to {VOICE_PLAY!r} at its end")
            # Slid left: gone, nothing sent.
            count = len(voices_on(dev.dump()))
            x, y = mic.center
            dev.swipe(x, y, max(0, x - 400), y, 2_000)
            time.sleep(4)
            if len(voices_on(dev.dump())) != count:
                raise StepFailed("a hold slid left sent a voice message")
        finally:
            dev.shell(f"run-as {PACKAGE} rm -f files/{VOICE_FIXTURE}", check=False)
            dev.shell(f"rm -f /data/local/tmp/{VOICE_FIXTURE}", check=False)
        return f"held_to_bubble_ms={ms} desc={player.desc!r} mailbox=accepted played=ok cancel=ok"

    # Recording a voice message (1276): the recording bar.

    def seed_voice(self, pcm: bytes) -> None:
        """`pcm` where the debug build records it from (Voice.FIXTURE),
        the permission granted, the compose field empty."""
        dev = self.need_device()
        local = Path(tempfile.mkdtemp(prefix="walk-voice-")) / VOICE_FIXTURE
        local.write_bytes(pcm)
        dev.run("push", str(local), f"/data/local/tmp/{VOICE_FIXTURE}", timeout=60)
        local.unlink()
        local.parent.rmdir()
        dev.shell(
            f"run-as {PACKAGE} sh -c 'cat /data/local/tmp/{VOICE_FIXTURE} > files/{VOICE_FIXTURE}'"
        )
        dev.shell(f"pm grant {PACKAGE} android.permission.RECORD_AUDIO", check=False)
        self.clear_compose()

    def unseed_voice(self) -> None:
        dev = self.need_device()
        dev.shell(f"run-as {PACKAGE} rm -f files/{VOICE_FIXTURE}", check=False)
        dev.shell(f"rm -f /data/local/tmp/{VOICE_FIXTURE}", check=False)

    def bar_gone(self, timeout: float) -> bool:
        """Whether the recording bar gave way to the compose field."""
        dev = self.need_device()

        def gone():
            nodes = dev.dump()
            return not find(nodes, rid="rec_bar") and bool(find(nodes, rid="compose"))

        return wait_for(gone, timeout, every=0.3)

    def wait_bar(self, phase: str, timeout: float) -> list[Node]:
        """The recording bar in `phase` on screen within `timeout`."""
        dev = self.need_device()
        nodes: list[Node] = []
        problems: list[str] = []

        def shows():
            nonlocal nodes, problems
            nodes = dev.dump()
            problems = rec_bar_problems(nodes, phase)
            return not problems

        if not wait_for(shows, timeout, every=0.3):
            raise StepFailed(f"{phase}: " + "; ".join(problems))
        return nodes

    def lock_by_swipe(self) -> None:
        """A swipe up from the microphone, 400 px (past 72 dp at any
        density up to 5.5), and the finger lifts: locked, it records on."""
        dev = self.need_device()
        mic = self.wait_id("record", 5)[0]
        x, y = mic.center
        dev.swipe(x, y, x, max(0, y - 400), 900)

    def step_voice_hold_release(self) -> str:
        """Held, the compose row is the recording bar at once (1276): the
        dot pulsing, `REC 00:0N`, the wave, "Slide left to cancel", the
        held microphone saying "Release to send", the open lock chip;
        released after six seconds, the bar gives way to the field, the
        bubble is at least five seconds long and the mailbox took it."""
        dev = self.need_device()
        fixture = self.need_fixture()
        self.seed_voice(speech_pcm(16_000, 10_000))
        try:
            sent_before = fixture.mailbox_log().count("HOME send accepted")
            before = len(voices_on(dev.dump()))
            x, y = self.wait_id("record", 5)[0].center
            hold = dev.swipe_async(x, y, x, y, 6_000)
            time.sleep(1.5)
            nodes = dev.dump()
            problems = rec_bar_problems(nodes, "held")
            if rec_pulsing(nodes) is not True:
                problems.append(f"the dot pulses={rec_pulsing(nodes)}, want True")
            seconds = rec_seconds(nodes)
            hold.wait(timeout=30)
            if problems:
                raise StepFailed("held: " + "; ".join(problems))
            if not self.bar_gone(10):
                raise StepFailed("the bar did not give way to the field after the release")
            shown: list[str] = []

            def bubble():
                nonlocal shown
                shown = voice_problems(voices_on(dev.dump()), before, 5)
                return not shown

            if not wait_for(bubble, 15, every=0.3):
                raise StepFailed("; ".join(shown))
            if not wait_for(lambda: fixture.mailbox_log().count("HOME send accepted") > sent_before, 30):
                raise StepFailed("the mailbox log has no new 'HOME send accepted'")
        finally:
            self.unseed_voice()
        return f"held timer={seconds}s bar=ok bubble=ok mailbox=accepted"

    def step_voice_slide_cancel(self) -> str:
        """Held and slid left 500 px (past 96 dp at any density up to 5),
        the hint arms ("Release to cancel", RecordingBar's line) and the
        release discards: the bar goes, no bubble, nothing sent."""
        dev = self.need_device()
        fixture = self.need_fixture()
        self.seed_voice(speech_pcm(16_000, 10_000))
        try:
            sent_before = fixture.mailbox_log().count("HOME send accepted")
            count = len(voices_on(dev.dump()))
            x, y = self.wait_id("record", 5)[0].center
            dev.shell("logcat -c", check=False)
            dev.swipe(x, y, max(0, x - 500), y, 3_000)
            lines = parse_rec_lines(dev.shell("logcat -d -s loquax:I", check=False))
            went = rec_went(lines, "held", "armed", "cancelled")
            if went:
                raise StepFailed("; ".join(went))
            if not self.bar_gone(10):
                raise StepFailed("the bar stayed after the cancelling release")
            time.sleep(4)
            if len(voices_on(dev.dump())) != count:
                raise StepFailed("a hold slid left made a voice message")
            if fixture.mailbox_log().count("HOME send accepted") != sent_before:
                raise StepFailed("a hold slid left reached the mailbox")
        finally:
            self.unseed_voice()
        return f"phases={[l['phase'] for l in lines]} armed=ok sent=none"

    def step_voice_lock_send(self) -> str:
        """Swiped up, it locks: the chip closed ("Recording locked"), the
        bin, the pause and the big Send, no held microphone, and the
        finger is free. Pause stands it still (play, resume, the dot
        steady), resume goes on, Send sends: the bar goes, one bubble at
        least three seconds long, the mailbox took it."""
        dev = self.need_device()
        fixture = self.need_fixture()
        self.seed_voice(speech_pcm(16_000, 12_000))
        try:
            sent_before = fixture.mailbox_log().count("HOME send accepted")
            before = len(voices_on(dev.dump()))
            self.lock_by_swipe()
            locked = self.wait_bar("locked", 8)
            time.sleep(1.5)
            dev.tap_node(find(locked, rid="rec_pause")[0])
            paused = self.wait_bar("paused", 8)
            if rec_pulsing(paused) is not False:
                raise StepFailed(f"paused, the dot pulses={rec_pulsing(paused)}")
            dev.tap_node(find(paused, rid="rec_resume")[0])
            resumed = self.wait_bar("locked", 8)
            time.sleep(1.5)
            dev.tap_node(find(resumed, rid="rec_send")[0])
            if not self.bar_gone(10):
                raise StepFailed("the bar stayed after Send")
            shown: list[str] = []

            def bubble():
                nonlocal shown
                shown = voice_problems(voices_on(dev.dump()), before, 3)
                return not shown

            if not wait_for(bubble, 15, every=0.3):
                raise StepFailed("; ".join(shown))
            if not wait_for(lambda: fixture.mailbox_log().count("HOME send accepted") > sent_before, 30):
                raise StepFailed("the mailbox log has no new 'HOME send accepted'")
        finally:
            self.unseed_voice()
        return "locked=ok paused=ok resumed=ok sent=ok mailbox=accepted"

    def step_voice_silence_hint(self) -> str:
        """A microphone that hears nothing (a fixture of digital silence,
        the blocked microphone's zeros): locked and two seconds in, the
        line under the wave says "Nothing heard. Is the microphone
        allowed?" with "Allow the microphone", and the dot stops pulsing;
        the bin then discards and nothing is sent."""
        dev = self.need_device()
        fixture = self.need_fixture()
        self.seed_voice(bytes(16_000 * 2 * 8))
        try:
            sent_before = fixture.mailbox_log().count("HOME send accepted")
            self.lock_by_swipe()
            self.wait_bar("locked", 8)
            problems: list[str] = []
            nodes: list[Node] = []

            def hint():
                nonlocal problems, nodes
                nodes = dev.dump()
                problems = rec_notice_problems(nodes)
                return not problems

            if not wait_for(hint, 10, every=0.3):
                raise StepFailed("; ".join(problems))
            seconds = rec_seconds(nodes)
            if seconds is None or seconds < 2:
                raise StepFailed(f"the line came at {seconds} s, before 2")
            dev.tap_node(find(nodes, rid="rec_discard")[0])
            if not self.bar_gone(10):
                raise StepFailed("the bin left the bar up")
            time.sleep(3)
            if fixture.mailbox_log().count("HOME send accepted") != sent_before:
                raise StepFailed("the discarded silence reached the mailbox")
        finally:
            self.unseed_voice()
        return f"notice_at={seconds}s dot=steady discarded=ok"

    def host_send(self, body: str) -> list[str]:
        """The host sender's `loquax send` of `body` to the phone's address,
        and its environment, for `subprocess`."""
        fixture = self.need_fixture()
        return [
            self.args.loquax,
            "send",
            "--config",
            str(fixture.cfg),
            "--via",
            "direct",
            fixture.manifest["address"],
            body,
        ]

    def host_env(self) -> dict:
        return {**os.environ, "LNMSG_HOME": str(self.need_fixture().sender)}

    def step_receive_wake(self) -> str:
        dev = self.need_device()
        body = f"walk wake {self.nonce}"
        send = subprocess.run(
            self.host_send(body),
            env=self.host_env(),
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
        line says `📍 live`, and the row gains its map pin (1224), which
        stands in the place of the avatar's ring: the avatar stays as it
        was."""
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
        nodes = dev.dump()
        avatar = self.row_avatar(nodes, shown)
        problems = pin_problems(pin_in_row(nodes, avatar), self.density())
        if problems:
            raise StepFailed("; ".join(problems))
        after = [dev.screen().pixel(x, y) for x, y in ring_points(avatar.bounds)]
        ring = changed_fraction(before, after)
        if ring >= 0.5:
            raise StepFailed(f"the avatar gained a ring in the list: {ring:.2f} of its points changed")
        return f"live_line_ms={seen_ms} notified=no pin=shown ring_changed={ring:.2f}"

    def density(self) -> int | None:
        return parse_density(self.need_device().shell("wm density"))

    def step_position_pin(self) -> str:
        """The list row's map pin (1224), after receive_report gave the
        peer a fresh position: a tap lands on the Map tab with that
        friend's card open; Back returns to the chats, the card closed."""
        dev = self.need_device()
        shown = self.need_fixture().manifest["open"]["shown"]
        for _ in range(3):
            if find(dev.dump(), rid="write"):
                break
            dev.key("KEYCODE_BACK")
            time.sleep(0.6)
        nodes = dev.dump()
        pin = pin_in_row(nodes, self.row_avatar(nodes, shown))
        problems = pin_problems(pin, self.density())
        if problems or pin is None:
            raise StepFailed("; ".join(problems))
        start = time.monotonic()
        dev.tap_node(pin)
        card = self.wait_id("card_name", 10)
        card_ms = int((time.monotonic() - start) * 1000)
        if card[0].text != shown:
            raise StepFailed(f"the card says {card[0].text!r}, the row {shown!r}")
        nodes = dev.dump()
        if find(nodes, rid="write") or find(nodes, rid="compose"):
            raise StepFailed(f"not on the Map tab: {texts(nodes)[:12]}")
        dev.key("KEYCODE_BACK")
        self.wait_id("write", 10)
        if find(dev.dump(), rid="card_name"):
            raise StepFailed("back left the card open")
        return f"pin={pin.desc!r} card_ms={card_ms} card={card[0].text!r} back=chats"

    def step_draft(self) -> str:
        """A message typed and not sent (1219): Back to the list shows it
        as the row's `Draft: …`; the app stopped and started again shows it
        there still, and the chat opens with it in the field. The field is
        emptied again at the end, which takes the draft away."""
        dev = self.need_device()
        shown = self.need_fixture().manifest["open"]["shown"]
        typed = f"walk draft {self.nonce}"

        def to_list():
            for _ in range(3):
                if find(dev.dump(), rid="write"):
                    return
                dev.key("KEYCODE_BACK")
                time.sleep(0.6)
            self.wait_id("write", 10)

        def row_says():
            nodes = dev.dump()
            return draft_problems(last_in_row(nodes, self.row_avatar(nodes, shown)), typed)

        to_list()
        self.open_conversation(shown)
        dev.tap_node(self.wait_id("compose", 5)[0])
        dev.type_text(typed)
        to_list()
        problems = row_says()
        if problems:
            raise StepFailed("in the list: " + "; ".join(problems))
        # Typing paused long ago: the store has it. The app goes whole.
        dev.shell(f"am force-stop {PACKAGE}")
        start = time.monotonic()
        dev.shell(f"am start -W -n {PACKAGE}/{ACTIVITY}", timeout=60)
        self.wait_id("write", 30)
        if not wait_for(lambda: not row_says(), 10, every=0.5):
            raise StepFailed("after the restart: " + "; ".join(row_says()))
        back_ms = int((time.monotonic() - start) * 1000)
        self.open_conversation(shown)
        field = self.wait_id("compose", 5)[0]
        if field.text != typed:
            raise StepFailed(f"the field holds {field.text!r}, not {typed!r}")
        dev.tap_node(field)
        dev.shell("input keyevent KEYCODE_MOVE_END " + " ".join(["KEYCODE_DEL"] * (len(typed) + 2)))
        to_list()
        nodes = dev.dump()
        last = last_in_row(nodes, self.row_avatar(nodes, shown))
        if last is not None and last.text.startswith(DRAFT_PREFIX):
            raise StepFailed(f"emptied, the row still says {last.text!r}")
        return f"row={DRAFT_PREFIX + typed!r} restart_ms={back_ms} field=restored cleared=yes"

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

    def step_group_create(self) -> str:
        """A group from the phone (1307, study 4.1): the pencil, "New
        group", the host's sender ticked, Next, the name, Create; 4 + k
        taps and the name with k = 1. The mailbox makes the group and hosts
        its engine, and the list shows the group's row; its address stays
        for the next two steps."""
        dev = self.need_device()
        fixture = self.need_fixture()
        friend = fixture.manifest["open"]["shown"]
        name = f"Walk Group {self.nonce}"
        self.back_to_chats()
        taps = 0
        dev.tap_node(self.wait_id("write", 10)[0])
        taps += 1
        dev.tap_node(self.wait_id(GROUP_NEW, 10)[0])
        taps += 1
        # The picker is the same list with check marks (1448): its button
        # shows once it is the picker, so the friend's row is ticked, not
        # opened.
        self.wait_text(GROUP_NEXT, 10)
        self.tap_text(friend)
        taps += 1
        self.tap_text(GROUP_NEXT)
        taps += 1
        fields = []

        def field_up():
            nonlocal fields
            fields = [n for n in dev.dump() if "EditText" in n.cls]
            return bool(fields)

        if not wait_for(field_up, 10, every=0.3):
            raise StepFailed("no name field after Next")
        dev.tap_node(fields[0])
        dev.type_text(name)
        self.tap_text(GROUP_CREATE)
        taps += 1
        address = None

        def hosted():
            nonlocal address
            address = group_hosted(fixture.mailbox_log(), name)
            return address is not None

        if not wait_for(hosted, 30, every=0.5):
            raise StepFailed(f"the mailbox hosts no {name!r}: {fixture.mailbox_log()[-400:]}")
        self.group = (name, address)
        self.back_to_chats()
        self.wait_text(name, 30)
        if taps != 5:
            raise StepFailed(f"{taps} taps, not 4 + 1")
        return f"taps={taps}+name group={address[:8]} row=shown"

    def need_group(self) -> tuple[str, str]:
        group = getattr(self, "group", None)
        if group is None:
            raise StepFailed("no group: run group_create first")
        return group

    def step_group_join(self) -> str:
        """A member who joins by writing (1307 G2): the host's sender, a
        member the phone ticked, writes to the group's address. The group
        copies the line to the phone as "<name>: <text>" with the writer
        above the bubble, and tells the phone the new list, which the
        group's chat shows as the line "<name> joined."."""
        dev = self.need_device()
        fixture = self.need_fixture()
        name, address = self.need_group()
        friend = fixture.manifest["open"]["shown"]
        body = f"walk group hello {self.nonce}"
        send = subprocess.run(
            [self.args.loquax, "send", "--config", str(fixture.cfg), "--via", "direct", address, body],
            env=self.host_env(),
            capture_output=True,
            timeout=90,
        )
        if send.returncode != 0:
            raise StepFailed(f"loquax send exit={send.returncode}: {send.stderr.decode()[-300:]}")
        self.back_to_chats()
        self.open_conversation(name)
        start = time.monotonic()
        self.wait_text(body, 30)
        line_ms = int((time.monotonic() - start) * 1000)
        nodes = dev.dump()
        joined = GROUP_JOINED.format(friend)
        if not find(nodes, text=joined, contains=True):
            # The admin's name for the member is the phone's word at
            # Create time; a phone without one names it by its address.
            said = [t for t in texts(nodes) if t.endswith(" joined.")]
            if not said:
                raise StepFailed(f"no line '... joined.' in the chat: {texts(nodes)[-8:]}")
            joined = said[0]
        return f"line_ms={line_ms} note={joined!r}"

    def step_group_message(self) -> str:
        """The phone writes in its group (1307): the bubble gets its tick
        from the group's proof, which the mailbox gives once it archived
        the line ("the group has it"); the mailbox books a copy for the
        member."""
        dev = self.need_device()
        fixture = self.need_fixture()
        name, _ = self.need_group()
        body = f"walk group line {self.nonce}"
        self.back_to_chats()
        self.open_conversation(name)
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
            marks = [n for n in by_top(marks_on(nodes)) if n.bounds[1] >= y]
            mark = mark_rid(marks[0]) if marks else ""
            return mark in TICKS

        if not wait_for(ticked, 30, every=0.3):
            raise StepFailed(f"no tick on {body!r} within 30s; mark={mark!r}")
        tick_ms = int((time.monotonic() - start) * 1000)
        return f"tick_ms={tick_ms} mark={mark!r}"

    def step_group_card_join(self) -> str:
        """A Loquax member joins by the invitation's card (1307 pass four,
        study 4.2): a second home on the host ("card", its owner a scripted
        terminal client) makes a group with the phone in it; the phone
        lists the group as an invitation, its chat shows the card, and
        Join is the one tap after opening it: 2 taps. The card home's store
        must then hold the phone as joined and understanding groups (flags
        8 and 1 of `group_members`)."""
        dev = self.need_device()
        fixture = self.need_fixture()
        loquax = str(self.args.loquax)
        phone = fixture.manifest["address"]
        name = f"Card Group {self.nonce}"
        card, laptop = fixture.root / "card", fixture.root / "card-laptop"
        card.mkdir(exist_ok=True)
        laptop.mkdir(exist_ok=True)
        env_card = {**os.environ, "LNMSG_HOME": str(card), "NO_COLOR": "1"}
        env_laptop = {
            **os.environ,
            "LNMSG_HOME": str(laptop),
            "LNMSG_DISPLAY_NAME": f"Card {self.nonce}",
            "NO_COLOR": "1",
        }
        admitted = subprocess.run(
            [loquax, "home", "admit", "--name", "laptop"],
            env=env_card,
            capture_output=True,
            timeout=30,
        )
        code = parse_code(admitted.stdout.decode(errors="replace"))
        if admitted.returncode != 0 or not code:
            raise StepFailed(f"card admit exit={admitted.returncode}: {admitted.stderr.decode()[-300:]}")
        fixture.logs["card"] = fixture.root / "card.log"
        fixture.procs.append(
            subprocess.Popen(
                [loquax, "mailbox", "run", "--config", str(fixture.cfg), "--sync-interval", "off"],
                env=env_card,
                stdout=fixture.logs["card"].open("wb"),
                stderr=subprocess.STDOUT,
            )
        )

        def card_log() -> str:
            return fixture.logs["card"].read_text(errors="replace")

        if not wait_for(lambda: "HOME up address=" in card_log(), 30):
            raise StepFailed(f"the card home did not come up: {card_log()[-400:]}")
        joined = subprocess.run(
            [loquax, "home", "join", code, "--timeout", "40", "--config", str(fixture.cfg)],
            env=env_laptop,
            capture_output=True,
            timeout=90,
        )
        if joined.returncode != 0:
            raise StepFailed(f"card laptop join exit={joined.returncode}: {joined.stderr.decode()[-300:]}")
        script = laptop / "session.script"
        script.write_text(
            "until ready 20\nkey :\ntype new group\nkey enter\n"
            f"type {phone}\nkey enter\nkey enter\ntype {name}\nkey enter\n"
            "until groups 1 40\nkey q\n",
            encoding="utf-8",
        )
        made = subprocess.run(
            [loquax, "tui", "--config", str(fixture.cfg), "--to", phone, "--sync-interval", "15m",
             "--script", str(script), "--size", "80x24"],
            env=env_laptop,
            capture_output=True,
            timeout=120,
        )
        if made.returncode != 0:
            raise StepFailed(f"card tui exit={made.returncode}: {made.stderr.decode()[-300:]}")
        address = None

        def hosted():
            nonlocal address
            address = group_hosted(card_log(), name)
            return address is not None

        if not wait_for(hosted, 30, every=0.5):
            raise StepFailed(f"the card home hosts no {name!r}: {card_log()[-400:]}")
        self.back_to_chats()
        self.wait_text(name, 90)
        taps = 0
        self.open_conversation(name)
        taps += 1
        self.tap_text(GROUP_JOIN)
        taps += 1
        store = card / "history.sqlite3"
        flags = 0

        def member_joined():
            nonlocal flags
            try:
                with sqlite3.connect(f"file:{store}?mode=ro", uri=True) as db:
                    row = db.execute(
                        "SELECT flags FROM group_members WHERE grp = ? AND address = ?",
                        (bytes.fromhex(address), bytes.fromhex(phone)),
                    ).fetchone()
            except sqlite3.Error:
                return False
            flags = row[0] if row else 0
            return flags & 8 != 0 and flags & 1 != 0

        if not wait_for(member_joined, 60, every=0.5):
            raise StepFailed(f"the card home never saw the phone join: flags={flags}")
        if taps != 2:
            raise StepFailed(f"{taps} taps, not 2")
        return f"taps={taps} group={address[:8]} flags={flags}"

    def network_kind(self) -> str:
        return default_network(self.need_device().shell("dumpsys connectivity", check=False))

    def network_change(self, change: str, off: list[str], on: list[str], off_secs: int) -> str:
        """One network change (1298) on the list screen: `off` takes the
        network away (or swaps it), the screen's line is read every second
        for `off_secs`, `on` brings it back, and the host sends a message
        at once. Measured from the moment `on` ran: when the
        "Reconnecting…" line was gone, and when the message showed in the
        list. No tap: the app must come back by itself."""
        dev = self.need_device()
        for _ in range(3):
            if find(dev.dump(), rid="write"):
                break
            dev.key("KEYCODE_BACK")
            time.sleep(0.6)
        self.wait_id("write", 10)
        before = self.network_kind()
        shown: list[str] = []

        def line() -> str:
            status = find(dev.dump(), rid="status")
            return status[0].text if status else ""

        for command in off:
            dev.shell(command, check=False)
        away_until = time.monotonic() + off_secs
        # The platform takes a moment to let the network go.
        during = wait_for(lambda: (k := self.network_kind()) != before and k, 10, every=0.5)
        during = during or before
        while time.monotonic() < away_until:
            shown.append(line())
            time.sleep(1.0)
        if during == before:
            for command in on:
                dev.shell(command, check=False)
            raise StepFailed(
                f"the change did nothing on this device: the default network stayed {before}"
            )
        body = f"walk {change} {self.nonce}"
        for command in on:
            dev.shell(command, check=False)
        back = time.monotonic()
        send = subprocess.Popen(
            self.host_send(body),
            env=self.host_env(),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        line_gone_ms: int | None = None
        bubble_ms: int | None = None
        try:
            while time.monotonic() - back < BACK_BUDGET_SECS:
                nodes = dev.dump()
                now_ms = int((time.monotonic() - back) * 1000)
                status = find(nodes, rid="status")
                text = status[0].text if status and status[0].bounds[3] > status[0].bounds[1] else ""
                shown.append(text)
                if RECONNECTING in shown and RECONNECTING not in text and line_gone_ms is None:
                    line_gone_ms = now_ms
                if find(nodes, text=body, contains=True):
                    bubble_ms = now_ms
                    break
                time.sleep(0.3)
        finally:
            try:
                send.wait(timeout=60)
            except subprocess.TimeoutExpired:
                send.kill()
        after = self.network_kind()
        if bubble_ms is None:
            raise StepFailed(
                f"no {body!r} within {BACK_BUDGET_SECS}s of the network's return "
                f"(kinds {before}>{during}>{after}, lines {shown[-4:]})"
            )
        return network_detail(change, off_secs, (before, during, after), shown, line_gone_ms, bubble_ms)

    def step_network_wifi_toggle(self) -> str:
        return self.network_change(
            "wifi_toggle", ["svc wifi disable"], ["svc wifi enable"], WIFI_OFF_SECS
        )

    def step_network_mobile_switch(self) -> str:
        """WLAN off with mobile data on, then the WLAN back. A device
        without mobile data (Waydroid, a phone without a SIM) has none to
        switch to: the step says so in its detail and measures nothing."""
        dev = self.need_device()
        dev.shell("svc data enable", check=False)
        dev.shell("svc wifi disable", check=False)
        has_mobile = wait_for(lambda: self.network_kind() == "mobile", 15, every=1.0)
        dev.shell("svc wifi enable", check=False)
        if not has_mobile:
            wait_for(lambda: self.network_kind() != "none", 30, every=1.0)
            return "change=mobile_switch skipped=no_mobile_network"
        wait_for(lambda: self.network_kind() == "wifi", 30, every=1.0)
        return self.network_change(
            "mobile_switch", ["svc wifi disable"], ["svc wifi enable"], MOBILE_SECS
        )

    def step_flight_mode(self) -> str:
        return self.network_change(
            "flight_mode",
            ["cmd connectivity airplane-mode enable"],
            ["cmd connectivity airplane-mode disable"],
            self.args.flight_secs,
        )

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

    def step_offgrid_positions(self) -> str:
        """Positions the mesh announces need no mailbox (1302): with the
        host's mailbox stopped, two nodes announce on `lev.position` over
        the phone's node; the map draws two more pins, both nearby, and the
        Friends sheet lists both by their announced names, the card of one
        saying "Nearby, not in your contacts" with Add and no Chat."""
        dev = self.need_device()
        fixture = self.need_fixture()
        mailbox = fixture.procs[1] if len(fixture.procs) > 1 else None
        for _ in range(3):
            if find(dev.dump(), rid="tab_map"):
                break
            dev.key("KEYCODE_BACK")
            time.sleep(0.6)
        dev.tap_node(self.wait_id("tab_map", 10)[0])
        time.sleep(1.0)
        before = map_pins(dev.shell("logcat -d -s loquax:I", check=False))
        wanted = [f"Walk Nearby {tag}{self.nonce}" for tag in ("A", "B")]
        stopped = mailbox is not None and mailbox.poll() is None and not self.on_phone
        if stopped:
            mailbox.send_signal(signal.SIGSTOP)
        try:
            for name, place in zip(wanted, OFFGRID_PLACES):
                home = fixture.root / ("nearby-" + name[-7:].replace(" ", "-"))
                home.mkdir(parents=True, exist_ok=True)
                said = subprocess.run(
                    [
                        self.args.loquax,
                        "announce",
                        "--config",
                        str(fixture.cfg),
                        "--position",
                        place,
                        "--from",
                        name,
                    ],
                    env={**os.environ, "LNMSG_HOME": str(home)},
                    capture_output=True,
                    timeout=60,
                )
                if said.returncode != 0:
                    raise StepFailed(
                        f"loquax announce exit={said.returncode}: {said.stderr.decode()[-300:]}"
                    )
            seen = wait_for(
                lambda: (lambda p: p if p and p[1] - (before or (0, 0))[1] >= 2 else None)(
                    map_pins(dev.shell("logcat -d -s loquax:I", check=False))
                ),
                20,
                every=1.0,
            )
            pins = seen or map_pins(dev.shell("logcat -d -s loquax:I", check=False))
            dev.tap_node(self.wait_id("friends", 10)[0])
            self.wait_id("friend_name", 10)
            names = [n.text for n in by_top(find(dev.dump(), rid="friend_name"))]
            self.note(f"offgrid: pins {before} -> {pins}, sheet {names}")
            problems = offgrid_problems(pins, before, names, wanted)
            if problems:
                raise StepFailed("; ".join(problems))
            sheet = dev.dump()
            named = [n for n in find(sheet, rid="friend_name") if n.text.startswith(wanted[0])]
            rows = [r for r in by_top(find(sheet, rid="friend_row")) if any(inside(n, r) for n in named)]
            if not rows:
                raise StepFailed(f"no row for {wanted[0]!r}")
            dev.tap_node(rows[0])
            self.wait_id("card_name", 10)
            nodes = dev.dump()
            line = " ".join(n.text for n in find(nodes, rid="card_line"))
            if NEARBY_CARD not in line:
                raise StepFailed(f"the card says {line!r}, want {NEARBY_CARD!r}")
            if not find(nodes, rid="card_add") or find(nodes, rid="card_chat"):
                raise StepFailed(f"the card's buttons: {texts(nodes)[-8:]}")
            dev.key("KEYCODE_BACK")
            dev.key("KEYCODE_BACK")
        finally:
            if stopped:
                mailbox.send_signal(signal.SIGCONT)
        return (
            f"mailbox={'stopped' if stopped else 'not on the host'} pins={before}->{pins} "
            f"sheet={len(names)} card=nearby,add"
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

    def step_receipts(self) -> str:
        """The read receipts switch (1233): More, Settings, Privacy; the
        row first on the page, on; switched off, the imported chat shows
        no read mark, its message two grey ticks; Privacy again shows it
        off (stored), switched on, the read mark back."""
        dev = self.need_device()
        rungs = {}
        for on in (False, True):
            self.open_privacy()
            state = self.privacy_switch(not on)
            dev.tap_node(find(state, rid=RECEIPTS_SWITCH)[0])
            self.privacy_switch(on)
            for _ in range(3):
                dev.key("KEYCODE_BACK")
                time.sleep(0.6)
                if find(dev.dump(), rid="write"):
                    break
            self.wait_id("write", 10)
            seen = self.chat_rungs()
            self.note(f"receipts {'on' if on else 'off'}: rungs {sorted(seen)}")
            problems = receipts_rungs_problems(seen, on)
            if problems:
                raise StepFailed("; ".join(problems))
            rungs["on" if on else "off"] = len(seen)
        return f"switch=on,off,stored,on rungs_off={rungs['off']} rungs_on={rungs['on']}"

    def open_privacy(self) -> None:
        """From the open chat: More, Settings, Privacy."""
        dev = self.need_device()
        for _ in range(3):
            if find(dev.dump(), rid="write"):
                break
            dev.key("KEYCODE_BACK")
            time.sleep(0.6)
        self.tap_text(MORE)
        self.tap_text(SETTINGS)
        self.wait_id("settings_entry_title", 10)
        self.tap_text("Privacy")
        self.wait_id(RECEIPTS_SWITCH, 10)

    def privacy_switch(self, on: bool) -> list[Node]:
        """The Privacy page once its read receipts switch shows `on`, the
        row as `privacy_page_problems` wants it."""
        dev = self.need_device()
        seen: list[Node] = []

        def shows():
            nonlocal seen
            seen = dev.dump()
            state, problems = privacy_page_problems(seen)
            if problems and state is not None:
                raise StepFailed("Privacy: " + "; ".join(problems))
            return seen if state == on else None

        if not wait_for(shows, 10, every=0.3):
            raise StepFailed(f"Privacy: the switch did not show {'on' if on else 'off'}")
        return seen

    def chat_rungs(self) -> set[str]:
        """Every rung the open chat shows: up to its top, then down to its
        end, collecting the marks on the way."""
        dev = self.need_device()
        seen: set[str] = set()
        for up in (True, False):
            last: list[str] = []
            for _ in range(60):
                nodes = dev.dump()
                seen.update(mark_rid(n) for n in marks_on(nodes))
                bodies = [n.text for n in by_top(find(nodes, rid="body"))]
                if bodies and bodies == last:
                    break
                last = bodies
                self.scroll_list(nodes, "list", up=up)
        return seen

    def put_fixture_region(self) -> None:
        """The walk's region into the app's maps directory, through
        run-as (a debug APK), for the Maps page to list (1207)."""
        self.put_region(FIXTURE_REGION, "firenze.lmr")

    def put_region(self, region: Path, name: str) -> None:
        """`region` into the app's maps directory as `name`, through run-as."""
        dev = self.need_device()
        if not region.is_file():
            raise StepFailed(f"{region} missing")
        dev.run("push", str(region), f"/data/local/tmp/walk-{name}", timeout=60)
        dev.shell(
            f"cat /data/local/tmp/walk-{name} | run-as {PACKAGE} sh -c "
            f"'mkdir -p files/maps && cat > files/maps/{name}'"
        )
        dev.shell(f"rm -f /data/local/tmp/walk-{name}", check=False)

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
        that did not decode. Then Delete all (1290): the row at the list's
        foot with the storage line's total, its one question with the
        regions' count, and after the yes no region, 0 B and the row off.
        The page's line for the detail."""
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
        nodes = self.maps_switch(True)
        return f"{detail} switch=off,stored,on {self.delete_all_maps(nodes)}"

    def delete_all_maps(self, nodes: list[Node]) -> str:
        """The Maps page's Delete all (1290) on the open page `nodes`."""
        dev = self.need_device()
        total, problems = delete_all_row_problems(nodes)
        if problems:
            raise StepFailed("Delete all: " + "; ".join(problems))
        regions = len(find(nodes, rid="maps_region_name"))
        dev.tap_node(find(nodes, rid=DELETE_ALL)[0])
        asked: list[Node] = []

        def question():
            nonlocal asked
            asked = dev.dump()
            return any(DELETE_ALL_ASK.fullmatch(t) for t in texts(asked))

        wait_for(question, 10, every=0.3)
        problems = delete_all_ask_problems(asked, regions)
        if problems:
            raise StepFailed("Delete all: " + "; ".join(problems))
        dev.tap_node(find(asked, text=DELETE_ALL_YES)[0])
        after: list[Node] = []

        def emptied():
            nonlocal after
            after = dev.dump()
            return not find(after, rid="maps_region_name") and find(after, rid=DELETE_ALL)

        if not wait_for(emptied, 10, every=0.3):
            raise StepFailed(f"Delete all: regions left; screen: {texts(after)[:8]}")
        left, problems = delete_all_row_problems(after)
        problems += decode_problems(after)
        if not re.fullmatch(r"0\s?B", left):
            problems.append(f"the row says {left!r} with no maps")
        if problems:
            raise StepFailed("Delete all: " + "; ".join(problems))
        return f"delete_all={total},asked={regions},left={left}"

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

    def step_explore(self) -> str:
        """Explore (1578): the third tab opens the pages and the public
        rooms on one screen, its field, its order's toggle and both
        sections there whatever the mesh holds; the toggle turns the order
        and back; Back goes to the chats."""
        self.back_to_chats()
        dev = self.need_device()
        dev.tap_node(self.wait_id("tab_explore", 10)[0])
        self.wait_id("explore_list", 10)
        titles = catalogue_words(phone_lang(dev), EXPLORE_TITLE_KEYS)
        nodes = dev.dump()
        # The list draws only what is visible (1618): the rooms' titles may
        # be below; scroll for them slowly, a step at most three quarters
        # of the list (1621), keeping every row seen.
        seen = list(nodes)
        for _ in range(24):
            if all(find(seen, text=t) for t in titles):
                break
            box = find(dev.dump(), rid="explore_list")[0]
            x, y1, y2, ms = explore_scroll(box.bounds)
            dev.swipe(x, y1, x, y2, ms)
            time.sleep(0.5)
            seen.extend(dev.dump())
        problems = explore_problems(seen, titles, first=nodes)
        if problems:
            raise StepFailed("; ".join(problems))
        order = find(nodes, rid="explore_sort")[0].text
        dev.tap_node(find(nodes, rid="explore_sort")[0])
        time.sleep(0.8)
        turned = find(dev.dump(), rid="explore_sort")[0].text
        if turned == order:
            raise StepFailed(f"the order's toggle stayed {order!r}")
        dev.tap_node(find(dev.dump(), rid="explore_sort")[0])
        dev.key("KEYCODE_BACK")
        self.wait_id("write", 10)
        return f"sections={len(titles)} order={order!r}->{turned!r} back=chats"

    def screen_codes(self, name: str) -> list[str]:
        """The QR codes on screen, read by zbarimg from a screenshot kept
        beside the log as `name`.png."""
        path = self.out / f"{name}.png"
        self.need_device().screenshot_png(path)
        if shutil.which("zbarimg") is None:
            raise StepFailed("zbarimg missing: apt install zbar-tools")
        read = subprocess.run(
            ["zbarimg", "--quiet", "--raw", str(path)], capture_output=True, timeout=30
        )
        return qr_texts(read.stdout.decode("utf-8", "replace"))

    def back_to_chats(self) -> None:
        dev = self.need_device()
        for _ in range(4):
            if find(dev.dump(), rid="write"):
                return
            dev.key("KEYCODE_BACK")
            time.sleep(0.6)
        self.wait_id("write", 10)

    def step_join_error_words(self) -> str:
        """Join errors in plain words (1263, finding 8), before the first
        start joins: a code that is no mailbox's, typed into the join form,
        says one sentence with Retry and a Details fold that opens on the
        parser's words; the app is stopped again, nothing joined."""
        dev = self.need_device()
        dev.shell(f"am start -W -n {PACKAGE}/{ACTIVITY}", timeout=60)
        self.wait_text(WHERE_TITLE, 20)
        self.tap_text(WHERE_COMPUTER)
        self.tap_text(CONTINUE)
        self.tap_text(PASTE_CODE)
        dev.tap_node(self.wait_id("code", 10)[0])
        dev.type_text(BAD_MAILBOX_CODE)
        if not find(dev.dump(), text=JOIN):
            dev.key("KEYCODE_BACK")
        self.tap_text(JOIN)
        self.wait_id(JOIN_RETRY, 15)
        nodes = dev.dump()
        problems = join_error_problems(nodes, JOIN_NOT_MAILBOX)
        if find(nodes, rid=JOIN_DETAILS):
            problems.append("Details open before the tap")
        toggle = find(nodes, rid=JOIN_DETAILS_TOGGLE)
        if toggle:
            dev.tap_node(toggle[0])
            details = self.wait_id(JOIN_DETAILS, 5)
            if JOIN_DETAILS_SAY not in details[0].text:
                problems.append(f"Details say {details[0].text!r}")
        dev.shell(f"am force-stop {PACKAGE}", check=False)
        if problems:
            raise StepFailed("; ".join(problems))
        return f"said={JOIN_NOT_MAILBOX!r} retry=shown details=folded,opens"

    def step_first_start_qr(self) -> str:
        """One contact code everywhere (1263, finding 1): the first start's
        address page, opened on the running session through the debug
        APK's exported screen, shows a QR that zbarimg reads as a contact
        code, `lxma://` and the mailbox's address, and the profile's QR
        reads as the same code."""
        dev = self.need_device()
        self.back_to_chats()
        dev.shell(
            f"am start -W -n {PACKAGE}/{WELCOME_ACTIVITY} --es show address", timeout=60
        )
        self.wait_id("welcome_qr", 20)
        welcome = self.screen_codes("first_start_qr-welcome")
        dev.key("KEYCODE_BACK")
        self.back_to_chats()
        dev.tap_node(self.wait_id("avatar", 10)[0])
        self.wait_id("profile_qr", 10)
        profile = self.screen_codes("first_start_qr-profile")
        dev.key("KEYCODE_BACK")
        self.wait_id("write", 10)
        want = self.fixture.manifest["address"] if self.fixture is not None else None
        problems = first_start_qr_problems(welcome, profile, want)
        if problems:
            raise StepFailed("; ".join(problems))
        return f"code={welcome[0][:48]}… same=profile"

    def step_pencil_scan(self) -> str:
        """The pencil (1263, finding 3): "Scan a code" first where the
        phone has a camera, "Paste a code" next, no daemon word on screen,
        the address field under a closed Advanced; Scan opens the scanner;
        a typed text that is no code says so in one sentence."""
        dev = self.need_device()
        self.back_to_chats()
        dev.tap_node(self.wait_id("write", 10)[0])
        dev.tap_node(self.wait_id(PENCIL_CONTACT, 10)[0])
        self.wait_id(PENCIL_PASTE, 10)
        camera = "feature:android.hardware.camera" in dev.shell("pm list features", check=False)
        nodes = dev.dump()
        problems = pencil_problems(nodes, camera)
        scanned = "no camera"
        if camera and find(nodes, rid=PENCIL_SCAN):
            dev.tap_node(find(nodes, rid=PENCIL_SCAN)[0])
            time.sleep(2)
            self.dismiss_dialogs(1)
            seen = texts(dev.dump())
            scanned = "opened" if any("camera" in t for t in seen) else f"screen {seen[:4]}"
            dev.key("KEYCODE_BACK")
            self.wait_id(PENCIL_PASTE, 10)
        dev.tap_node(self.wait_id(PENCIL_ADVANCED, 10)[0])
        dev.tap_node(self.wait_id("address", 5)[0])
        dev.type_text("hello")
        if not find(dev.dump(), rid="open"):
            dev.key("KEYCODE_BACK")
        dev.tap_node(self.wait_id("open", 5)[0])
        # Raises with the screen when the sentence is not there.
        self.wait_text(CODE_FOREIGN, 5)
        self.back_to_chats()
        if problems:
            raise StepFailed("; ".join(problems))
        return f"camera={camera} scan={scanned} refusal={CODE_FOREIGN!r}"

    def step_share_in_contact(self) -> str:
        """A friend's code from another app (walk finding 12): a text with
        an lxma:// code shared to Loquax opens "Add contact"; named, the
        friend's chat opens without the banner that asks to save them to
        the phone's contacts (walk finding 15). The same code as a tapped
        lxma:// link opens the dialog again, the name known."""
        dev = self.need_device()
        friend = self.need_fixture().manifest["friend"]
        code = friend["code"]
        self.back_to_chats()
        dev.shell(
            f"am start -W -a android.intent.action.SEND -t text/plain "
            f"--es android.intent.extra.TEXT 'Add me on Loquax: {code}' "
            f"-n {PACKAGE}/{ACTIVITY}",
            timeout=60,
        )
        self.wait_text(ADD_CONTACT_ASK, 20)
        nodes = dev.dump()
        edits = [n for n in nodes if n.cls.endswith("EditText")]
        if len(edits) != 1:
            raise StepFailed(f"the dialog holds {len(edits)} fields: {texts(nodes)[:12]}")
        dev.tap_node(edits[0])
        dev.type_text(FRIEND_NAME)
        add = [n for n in find(dev.dump(), text=ADD_CONTACT) if n.clickable or "Button" in n.cls]
        if not add:
            dev.key("KEYCODE_BACK")
            add = [
                n for n in find(dev.dump(), text=ADD_CONTACT) if n.clickable or "Button" in n.cls
            ]
        if not add:
            raise StepFailed(f"no {ADD_CONTACT!r} button: {texts(dev.dump())[:12]}")
        dev.tap_node(add[0])
        self.wait_text(FRIEND_NAME, 15)
        problems = []
        if find(dev.dump(), rid=BOOK_BANNER):
            problems.append("the chat asks to save the friend right after the add")
        self.back_to_chats()
        dev.shell(f"am start -W -a android.intent.action.VIEW -d '{code}'", timeout=60)
        self.wait_text(ADD_CONTACT_ASK, 20)
        known = [n.text for n in dev.dump() if n.cls.endswith("EditText")]
        if known != [FRIEND_NAME]:
            problems.append(f"the link's dialog holds {known}, not {FRIEND_NAME!r}")
        dev.key("KEYCODE_BACK")
        self.back_to_chats()
        if problems:
            raise StepFailed("; ".join(problems))
        return f"shared=added {FRIEND_NAME!r} banner=none link=dialog address={friend['address']}"

    def step_buildings(self) -> str:
        """The buildings (1193): the walk's Frankfurt region on the phone
        and the autoload off, so nothing else lands; the map opened on the
        Römer at z15, then at z16, each through the debug APK's exported
        map screen. At z16 the building fill covers at least a share of
        the middle of the screen, at z15 next to none; the time from the
        screen asked for to the map at rest at z16 is within the bound of
        z15's (the frame time proxy)."""
        dev = self.need_device()
        self.put_region(FRANKFURT_REGION, "frankfurt-roemer.lmr")
        for _ in range(3):
            if find(dev.dump(), rid="write"):
                break
            dev.key("KEYCODE_BACK")
            time.sleep(0.6)
        self.tap_text(MORE)
        self.tap_text(SETTINGS)
        self.wait_id("settings_entry_title", 10)
        self.tap_text("Maps")
        nodes = self.maps_switch(True)
        dev.tap_node(find(nodes, rid="maps_auto")[0])
        self.maps_switch(False)
        for _ in range(2):
            dev.key("KEYCODE_BACK")
            time.sleep(0.6)
        fills = [BUILDING_FILL_LIGHT, BUILDING_FILL_DARK]
        seen = {}
        for zoom in (15, 16):
            ms, screen = self.map_at_rest(zoom * 1000)
            nodes = dev.dump()
            if not find(nodes, rid="map"):
                raise StepFailed(f"z{zoom}: no map on screen: {texts(nodes)[:12]}")
            cols, rows = BUILDINGS_GRID
            samples = sample_grid(screen, middle_half(screen.width, screen.height), cols, rows)
            seen[zoom] = (ms, colour_share(samples, fills))
            dev.key("KEYCODE_BACK")
            time.sleep(0.6)
        (z15_ms, z15_share), (z16_ms, z16_share) = seen[15], seen[16]
        detail = (
            f"fill z15={z15_share:.3f} z16={z16_share:.3f} "
            f"at_rest_ms z15={z15_ms} z16={z16_ms} autoload=off"
        )
        problems = buildings_problems(z16_share, z15_share, z16_ms, z15_ms)
        if problems:
            raise StepFailed("; ".join(problems) + "; " + detail)
        return detail

    def step_pan_into_empty(self) -> str:
        """A pan out of the walk's region into the empty starts the load
        for the screen at once and the chip says so (1292). The walk's
        Frankfurt region, the autoload on, the source a socket on this
        host that never answers; the map opened on the Römer at z18,
        where the region holds the screen, then panned east a screen and
        a half in two swipes of 400 ms. The source's connections are the
        loads' starts: the first after the pan must come within
        `PAN_LOAD_SLACK_MS` of its end, and the chip must say
        "Loading the map of this area". The config is put back after."""
        dev = self.need_device()
        host = self.args.server.rpartition(":")[0]
        self.put_region(FRANKFURT_REGION, "frankfurt-roemer.lmr")
        source = StallingSource(host)
        url = f"http://{host}:{source.port}/planet.pmtiles"
        keep = "grep -v -e '^map_source' -e '^map_autoload' files/config 2>/dev/null"
        dev.shell(
            f"run-as {PACKAGE} sh -c \"{keep} > files/config.walk; "
            f"echo 'map_autoload = on' >> files/config.walk; "
            f"echo 'map_source = \\\"{url}\\\"' >> files/config.walk; "
            f"cp files/config files/config.before 2>/dev/null; "
            f"mv files/config.walk files/config\""
        )
        try:
            # A new process reads the config afresh.
            dev.shell(f"am force-stop {PACKAGE}", check=False)
            self.map_at_rest(18_000)
            time.sleep(1.0)
            nodes = dev.dump()
            box = self.map_box(nodes)
            before_chip = map_chip(nodes)
            connects_before = len(source.times)
            x1, y1, x2, y2 = box
            cy = (y1 + y2) // 2
            reach = (x2 - x1) * 3 // 4
            began = time.monotonic()
            for _ in range(2):
                dev.swipe(x2 - (x2 - x1) // 8, cy, x2 - (x2 - x1) // 8 - reach, cy, 400)
            pan_end_ms = int((time.monotonic() - began) * 1000)
            after_chip = ""

            def loading():
                nonlocal after_chip
                after_chip = map_chip(dev.dump())
                return after_chip.startswith(MAP_LOADING) or None

            wait_for(loading, 10, every=0.5)
            connect_ms = [int((t - began) * 1000) for t in source.times if t >= began]
        finally:
            source.close()
            dev.shell(
                f"run-as {PACKAGE} sh -c 'if [ -f files/config.before ]; then "
                f"mv files/config.before files/config; else rm -f files/config; fi'",
                check=False,
            )
            dev.shell(f"am force-stop {PACKAGE}", check=False)
        detail = (
            f"connects before={connects_before} after_ms={connect_ms} pan_end_ms={pan_end_ms} "
            f"chip before={before_chip!r} after={after_chip!r}"
        )
        problems = pan_into_empty_problems(
            before_chip, after_chip, connects_before, connect_ms, pan_end_ms
        )
        if problems:
            raise StepFailed("; ".join(problems) + "; " + detail)
        return detail

    def step_region_fills_in(self) -> str:
        """A region shows itself while it downloads (1291): the autoload
        on, its source the Grunewald archive on this host at `FILL_RATE`,
        no region over Grunewald on the phone; the map opened there at
        z15 and grabbed until it rests after the source's last byte. The
        grabs before that byte must hold at least `FILL_STATES_LEAST`
        distinct pictures. The config is put back and the region removed
        after."""
        dev = self.need_device()
        host = self.args.server.rpartition(":")[0]
        data = GRUNEWALD_ARCHIVE.read_bytes()
        source = ThrottledSource(host, data, FILL_RATE)
        url = f"http://{host}:{source.port}/grunewald.pmtiles"
        keep = "grep -v -e '^map_source' -e '^map_autoload' files/config 2>/dev/null"
        dev.shell(
            f"run-as {PACKAGE} sh -c \"rm -f files/maps/auto-*.lmr; {keep} > files/config.walk; "
            f"echo 'map_autoload = on' >> files/config.walk; "
            f"echo 'map_source = \\\"{url}\\\"' >> files/config.walk; "
            f"cp files/config files/config.before 2>/dev/null; "
            f"mv files/config.walk files/config\""
        )
        grabs: list[tuple[float, list[tuple[int, int, int]]]] = []
        try:
            dev.shell(f"am force-stop {PACKAGE}", check=False)
            lat, lon = GRUNEWALD_E6
            began = time.monotonic()
            dev.shell(
                f"am start -W -n {PACKAGE}/{MAP_ACTIVITY} "
                f"--el lat_e6 {lat} --el lon_e6 {lon} --el zoom_milli 15000"
            )
            while time.monotonic() - began < FILL_TIMEOUT_S:
                screen = dev.screen()
                box = middle_half(screen.width, screen.height)
                grabs.append((time.monotonic(), sample_grid(screen, box, 16, 16)))
                states = fill_states(grabs)
                last = source.last_byte
                now = time.monotonic()
                if last is not None and now - max(last, states[-1]) > FILL_REST_S:
                    break
        finally:
            source.close()
            dev.shell(
                f"run-as {PACKAGE} sh -c 'rm -f files/maps/auto-*.lmr; "
                f"if [ -f files/config.before ]; then "
                f"mv files/config.before files/config; else rm -f files/config; fi'",
                check=False,
            )
            dev.shell(f"am force-stop {PACKAGE}", check=False)
        states = fill_states(grabs)
        last = source.last_byte
        ms = [int((t - began) * 1000) for t in states]
        detail = (
            f"pictures_ms={ms} last_byte_ms={int((last - began) * 1000) if last else None} "
            f"served={source.served}/{len(data)} grabs={len(grabs)} rate={FILL_RATE}"
        )
        problems = region_fills_problems(states, len(source.sent), len(data), last)
        if problems:
            raise StepFailed("; ".join(problems) + "; " + detail)
        return detail

    def map_at_rest(self, zoom_milli: int) -> tuple[int, Screen]:
        """The map screen asked for on the walk's Frankfurt place at
        `zoom_milli`; the milliseconds until two grabs in a row are the
        same, the map at rest and not the screen before it, and a grab a
        second later, which the colours are read from."""
        dev = self.need_device()
        before = dev.screen()
        box = middle_half(before.width, before.height)
        was = sample_grid(before, box)
        lat, lon = FRANKFURT_E6
        started = time.monotonic()
        dev.shell(
            f"am start -W -n {PACKAGE}/{MAP_ACTIVITY} "
            f"--el lat_e6 {lat} --el lon_e6 {lon} --el zoom_milli {zoom_milli}"
        )
        last = None
        while time.monotonic() - started < SETTLE_TIMEOUT_S:
            now = sample_grid(dev.screen(), box)
            if (
                last is not None
                and changed_fraction(last, now, tolerance=0) == 0
                and changed_fraction(was, now) > 0.2
                and not is_uniform(now)
            ):
                ms = int((time.monotonic() - started) * 1000)
                time.sleep(1.0)
                return ms, dev.screen()
            last = now
        raise StepFailed(f"z{zoom_milli / 1000:g}: the map not at rest within {SETTLE_TIMEOUT_S}s")

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

    def start_on_phone(self, name: str | None) -> list[str]:
        """A first start with the mailbox on this phone, its node dialling
        the walk's lnsd (the servers file, put in before the first start,
        stands in for the public rendezvous), to the chats. On the address
        page `name` is typed into the name field when given; what is wrong
        with that page (walk finding 9) comes back."""
        dev = self.need_device()
        dev.shell(f"am force-stop {PACKAGE}", check=False)
        dev.shell(f"pm clear {PACKAGE}")
        dev.shell(
            f"run-as {PACKAGE} sh -c 'mkdir -p files/mailbox && "
            f"echo {self.args.server} > files/mailbox/servers'"
        )
        dev.shell(f"am start -W -n {PACKAGE}/{ACTIVITY}", timeout=60)
        self.wait_text(WHERE_TITLE, 20)
        self.tap_text(WHERE_PHONE)
        # One primary button whatever is chosen (1448, walk finding 15).
        self.tap_text(CONTINUE)
        problems: list[str] | None = None
        deadline = time.monotonic() + 90
        while True:
            nodes = dev.dump()
            if find(nodes, rid="write") and find(nodes, rid="avatar"):
                break
            if time.monotonic() > deadline:
                raise StepFailed(f"no chats 90s after Continue on this phone; screen: {texts(nodes)[:12]}")
            if find(nodes, rid=WELCOME_QR):
                if problems is None:
                    problems = first_start_name_problems(nodes)
                    field = find(nodes, rid=WELCOME_NAME)
                    if name is not None and field:
                        dev.tap_node(field[0])
                        # Whatever the owner's card put there goes first.
                        dev.shell("input keyevent KEYCODE_MOVE_END " + "KEYCODE_DEL " * 40)
                        dev.type_text(name)
                        # The keyboard closed, so Done is no key under it.
                        dev.key("KEYCODE_BACK")
                        nodes = dev.dump()
                self.tap_done(nodes)
            elif find(nodes, text=CONTINUE):
                dev.tap_node(find(nodes, text=CONTINUE)[0])
            else:
                self.dismiss_dialogs(1)
            time.sleep(0.5)
        self.on_phone = True
        return ["no address page"] if problems is None else problems

    def step_first_start_name(self) -> str:
        """The first start's name (walk finding 9): with the mailbox on
        this phone the address page asks the name as its one field, above
        the QR; the name typed there is the profile's afterwards."""
        dev = self.need_device()
        problems = self.start_on_phone(PHONE_NAME)
        dev.tap_node(self.wait_id("avatar", 10)[0])
        shown = ""

        def named():
            nonlocal shown
            field = find(dev.dump(), rid="profile_name")
            shown = field[0].text.strip() if field else ""
            return shown == PHONE_NAME

        if not wait_for(named, 15, every=0.5):
            problems.append(f"the profile's name is {shown!r}, not {PHONE_NAME!r}")
        dev.key("KEYCODE_BACK")
        self.wait_id("write", 10)
        if problems:
            raise StepFailed("; ".join(problems))
        return f"name field=first, alone profile={PHONE_NAME!r}"

    def step_link_device(self) -> str:
        """Link a device (1264, finding 7), last before the teardown since
        the app starts afresh before it: the phone holds the mailbox ("On
        this phone", from first_start_name, or started here when that step
        did not run), its node dialling the walk's lnsd; the profile's "Link a device" shows a join code whose QR zbarimg
        reads as the code's text, the server behind it; the host's loquax
        joins with that code over lnsd as a second device, under its own
        name; the sheet then says "Device linked: <that name>"."""
        dev = self.need_device()
        fixture = self.need_fixture()
        if not self.on_phone:
            self.start_on_phone(None)
        dev.tap_node(self.wait_id("avatar", 10)[0])
        dev.tap_node(self.wait_id(LINK_ROW, 10)[0])
        self.wait_id(LINK_QR, 15)
        shown = find(dev.dump(), rid=LINK_CODE)
        code = shown[0].text.strip() if shown else ""
        problems = link_code_problems(code, self.screen_codes("link_device"), self.args.server)
        if problems:
            raise StepFailed("; ".join(problems))
        second = fixture.root / "linked"
        second.mkdir(exist_ok=True)
        joined = subprocess.run(
            [self.args.loquax, "mailbox", "join", "--config", str(fixture.cfg), code],
            env={**os.environ, "LNMSG_HOME": str(second), "LNMSG_DISPLAY_NAME": LINKED_NAME},
            capture_output=True,
            timeout=120,
        )
        if joined.returncode != 0:
            raise StepFailed(
                f"loquax mailbox join exit={joined.returncode}: {joined.stderr.decode()[-300:]}"
            )
        self.wait_text(LINKED.format(LINKED_NAME), 15)
        done = find(dev.dump(), text=DONE)
        if done:
            dev.tap_node(done[0])
        else:
            dev.key("KEYCODE_BACK")
        return f"code={code[:24]}… server={self.args.server} linked={LINKED_NAME!r}"

    def step_move_phone(self) -> str:
        """Move to a new phone (1281, walk finding 24), after link_device
        and last before the teardown, since it ends the phone's mailbox:
        the profile's "Move to a new phone" shows a join code whose QR
        zbarimg reads as its text, the walk's server behind it; the host's
        walk_move takes the mailbox over with it as the new phone; the sheet
        says "Moved to <that name>"; Done opens the first screen, which says
        the phone moved and offers to use it again; that takes it back and
        the chats come again."""
        dev = self.need_device()
        fixture = self.need_fixture()
        if not self.on_phone:
            self.start_on_phone(None)
        dev.tap_node(self.wait_id("avatar", 10)[0])
        dev.tap_node(self.wait_id(MOVE_ROW, 10)[0])
        self.wait_id(MOVE_QR, 15)
        shown = find(dev.dump(), rid=MOVE_CODE)
        code = shown[0].text.strip() if shown else ""
        problems = link_code_problems(code, self.screen_codes("move_phone"), self.args.server)
        if problems:
            raise StepFailed("; ".join(problems))
        new = fixture.root / "new-phone"
        mover = (
            [self.args.walk_move]
            if self.args.walk_move
            else [
                "cargo",
                "run",
                "--quiet",
                "--manifest-path",
                str(REPO / "Cargo.toml"),
                "-p",
                "loquax-android",
                "--example",
                "walk_move",
                "--",
            ]
        )
        moved = subprocess.run(
            [*mover, str(new), code, MOVED_NAME], capture_output=True, timeout=480
        )
        if moved.returncode != 0:
            raise StepFailed(f"walk_move exit={moved.returncode}: {moved.stderr.decode()[-300:]}")
        if not (new / "mailbox" / "identity").exists() or not (new / "mailbox-here").exists():
            raise StepFailed(f"{new} holds no mailbox after the move")
        self.wait_text(MOVED.format(MOVED_NAME), 15)
        dev.tap_node(self.wait_text(DONE, 5)[0])
        self.wait_text(MOVED_TITLE, 30, contains=True)
        problems = moved_page_problems(dev.dump())
        if problems:
            raise StepFailed("; ".join(problems))
        dev.tap_node(self.wait_id(TAKE_BACK_ID, 5)[0])
        # The confirmation names the risk; its positive button takes it back.
        confirm = []

        def asked():
            confirm[:] = [n for n in dev.dump() if n.rid == "android:id/button1"]
            return bool(confirm)

        if not wait_for(asked, 10, every=0.5):
            raise StepFailed("no confirmation for Use this phone again")
        dev.tap_node(confirm[0])
        self.wait_id("write", 60)
        return f"code={code[:24]}… moved to={MOVED_NAME!r} taken back"

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
    p.add_argument(
        "--walk-move",
        help="a built walk_move example; default: cargo run it",
    )
    p.add_argument("--adb", default="adb", help="the adb to use")
    p.add_argument("--keep", action="store_true", help="no teardown")
    p.add_argument(
        "--flight-secs",
        type=int,
        default=120,
        help="how long flight_mode keeps the network away (the order's two minutes)",
    )
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
