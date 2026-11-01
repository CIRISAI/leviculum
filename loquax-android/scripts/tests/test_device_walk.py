"""The device walk's parsers (1175): the UI dump to texts and bounds, the
screencap's pixels, the WALK line format. Run by
`loquax-android/tests/device_walk_script.rs` under `cargo test -p
loquax-android`, or alone:

    python3 -I -m unittest discover -s loquax-android/scripts/tests
"""

import importlib.util
import struct
import sys
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "device-walk.py"
_spec = importlib.util.spec_from_file_location("device_walk", SCRIPT)
walk = importlib.util.module_from_spec(_spec)
# Registered before it runs: its dataclasses look their module up there.
sys.modules["device_walk"] = walk
_spec.loader.exec_module(walk)

# A dump as `uiautomator dump` writes it for the conversation list, cut to
# three rows, with the line the tool prints when it writes to a tty.
DUMP = """<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation="0">\
<node index="0" text="" resource-id="" class="android.widget.FrameLayout" package="network.leviculum.loquax.debug" content-desc="" clickable="false" long-clickable="false" scrollable="false" bounds="[0,0][1080,2400]">\
<node index="0" text="Loquax" resource-id="network.leviculum.loquax.debug:id/title" class="android.widget.TextView" content-desc="" clickable="false" bounds="[160,80][600,160]" />\
<node index="1" text="" resource-id="network.leviculum.loquax.debug:id/menu" class="android.widget.ImageButton" content-desc="More" clickable="true" bounds="[960,80][1060,160]" />\
<node index="2" text="" resource-id="network.leviculum.loquax.debug:id/list" class="android.widget.ListView" content-desc="" clickable="false" scrollable="true" bounds="[0,200][1080,2300]">\
<node index="0" text="Zo&#235; &#55357;&#56492; &#198;r&#248;" resource-id="network.leviculum.loquax.debug:id/name" class="android.widget.TextView" content-desc="" clickable="false" bounds="[180,420][800,480]" />\
<node index="1" text="J&#252;rgen &#55357;&#57010;" resource-id="network.leviculum.loquax.debug:id/name" class="android.widget.TextView" content-desc="" clickable="false" bounds="[180,220][800,280]" />\
<node index="2" text="&lt;5ab3c2d1&#8230;&gt;" resource-id="network.leviculum.loquax.debug:id/name" class="android.widget.TextView" content-desc="" long-clickable="true" bounds="[180,620][800,680]" />\
</node></node></hierarchy>
UI hierchary dumped to: /dev/tty
"""


def screencap(width, height, pixels, header=16, fmt=1):
    """A raw screencap of `pixels`, a list of (r, g, b) rows by rows."""
    head = struct.pack("<III", width, height, fmt)
    if header == 16:
        head += struct.pack("<I", 0)
    body = b"".join(bytes((r, g, b, 255)) for r, g, b in pixels)
    return head + body


def filled(width, height, colour):
    return [colour] * (width * height)


class Bounds(unittest.TestCase):
    def test_reads_uiautomators_form(self):
        self.assertEqual(walk.parse_bounds("[0,200][1080,2300]"), (0, 200, 1080, 2300))
        self.assertEqual(walk.parse_bounds(" [-5,0][10,20] "), (-5, 0, 10, 20))

    def test_refuses_anything_else(self):
        for text in ["", "[0,0]", "0,0,10,10", "[0,0][10,10]x", "[a,0][1,1]"]:
            self.assertIsNone(walk.parse_bounds(text), text)


def node(rid="", text="", desc=""):
    """One node of a dump, its bounds of no matter."""
    pkg = "network.leviculum.loquax.debug:id/"
    return walk.Node(text=text, desc=desc, rid=pkg + rid if rid else "", cls="", bounds=(0, 0, 1, 1))


# The Map tab as 1199 builds it: the bar, the map, the credit, the
# crosshair of 1201, two tabs with their icons and labels.
MAP_SCREEN = [
    node("title", "Map"),
    node("menu", desc="More"),
    node("map"),
    node("credit", "© OpenStreetMap contributors"),
    node("locate", desc="Show where I am"),
    node("tab_chats", desc="Chats"),
    node("tab_chats_icon"),
    node("tab_chats_label", "Chats"),
    node("tab_map", desc="Map"),
    node("tab_map_icon"),
    node("tab_map_label", "Map"),
]


class MapScreen(unittest.TestCase):
    def test_the_map_tab_as_built_has_no_problem(self):
        self.assertEqual(walk.tab_ids(MAP_SCREEN), ["tab_chats", "tab_map"])
        self.assertEqual(walk.map_screen_problems(MAP_SCREEN), [])

    def test_the_old_header_is_found_by_text_and_by_id(self):
        problems = walk.map_screen_problems(MAP_SCREEN + [node("load", "Load area")])
        self.assertEqual(problems, ["text 'Load area' on the map screen", "id 'load' on the map screen"])
        self.assertEqual(walk.map_screen_problems(MAP_SCREEN + [node("theme", "\u263e")]),
                         ["text '\u263e' on the map screen", "id 'theme' on the map screen"])

    def test_a_tab_missing_or_a_third_one_is_a_problem(self):
        self.assertEqual(walk.map_screen_problems(MAP_SCREEN[:7] + [node("map_x")]),
                         ["tabs ['tab_chats'], want ['tab_chats', 'tab_map']"])
        calls = MAP_SCREEN + [node("tab_calls", desc="Calls")]
        self.assertEqual(walk.map_screen_problems(calls),
                         ["tabs ['tab_chats', 'tab_map', 'tab_calls'], want ['tab_chats', 'tab_map']"])

    def test_no_map_view_is_a_problem(self):
        self.assertEqual(walk.map_screen_problems([n for n in MAP_SCREEN if not n.is_id("map")]),
                         ["no map view"])

    def test_no_crosshair_is_a_problem(self):
        self.assertEqual(walk.map_screen_problems([n for n in MAP_SCREEN if not n.is_id("locate")]),
                         ["no crosshair"])


def at(rid, text, top):
    """A node of a dump `top` pixels down."""
    pkg = "network.leviculum.loquax.debug:id/"
    return walk.Node(text=text, desc="", rid=pkg + rid, cls="", bounds=(0, top, 100, top + 40))


class FriendsSheet(unittest.TestCase):
    ALLOWED = {"Anna", "Bob", "Jürgen 🚲"}

    def test_the_names_from_the_top_with_no_problem(self):
        nodes = [at("friend_name", "Bob", 300), at("friends_older", "Older than a day", 200),
                 at("friend_name", "Jürgen 🚲", 250)]
        names, problems = walk.friends_sheet_problems(nodes, self.ALLOWED)
        self.assertEqual(names, ["Jürgen 🚲", "Bob"])
        self.assertEqual(problems, [])
        self.assertTrue(walk.divider_above(nodes))

    def test_an_empty_sheet_a_stranger_and_a_name_twice(self):
        self.assertEqual(walk.friends_sheet_problems([], self.ALLOWED), ([], ["no friend in the sheet"]))
        nodes = [at("friend_name", "Carla", 100), at("friend_name", "Bob", 150), at("friend_name", "Bob", 200)]
        _, problems = walk.friends_sheet_problems(nodes, self.ALLOWED)
        self.assertEqual(problems, ["names without a position in the fixture: ['Carla']",
                                    "listed twice: ['Bob']"])

    def test_the_divider_below_a_row_or_missing_is_not_above(self):
        rows = [at("friend_name", "Bob", 100)]
        self.assertFalse(walk.divider_above(rows))
        self.assertFalse(walk.divider_above(rows + [at("friends_older", "Older than a day", 160)]))


def settings_list(titles=None, summaries=None):
    """The settings list as SettingsActivity builds it, one entry a 100
    pixels down: its title and its one line under it."""
    titles = walk.SETTINGS_PAGES if titles is None else titles
    summaries = ["what it holds"] * len(titles) if summaries is None else summaries
    nodes = [node("title", "Settings")]
    for i, title in enumerate(titles):
        nodes.append(at("settings_entry_title", title, 200 + 100 * i))
    for i, summary in enumerate(summaries):
        nodes.append(at("settings_entry_summary", summary, 240 + 100 * i))
    return nodes


class Settings(unittest.TestCase):
    def test_the_nine_entries_as_built_have_no_problem(self):
        titles, problems = walk.settings_list_problems(settings_list())
        self.assertEqual(titles, walk.SETTINGS_PAGES)
        self.assertEqual(problems, [])

    def test_a_missing_entry_a_missing_line_and_the_wrong_order(self):
        _, problems = walk.settings_list_problems(settings_list(walk.SETTINGS_PAGES[:-1]))
        self.assertEqual(problems, [f"entries {walk.SETTINGS_PAGES[:-1]}, want {walk.SETTINGS_PAGES}"])
        _, problems = walk.settings_list_problems(settings_list(summaries=["x"] * 8 + [""]))
        self.assertEqual(problems, ["9 entries with 8 descriptions"])
        swapped = ["Mailbox", "Profile"] + walk.SETTINGS_PAGES[2:]
        _, problems = walk.settings_list_problems(settings_list(swapped))
        self.assertEqual(len(problems), 1)

    def test_a_raw_hash_on_a_main_row_and_a_line_that_did_not_decode(self):
        lines = ["what it holds"] * len(walk.SETTINGS_PAGES)
        lines[1] = "Mailbox 3e35f10c9a7b4d2e8f1a0b6c5d4e3f21"
        _, problems = walk.settings_list_problems(settings_list(summaries=lines))
        self.assertEqual(problems, [f"a raw hash on a main row: {lines[1]!r}"])
        lines[1] = "on <3e35f10c…>"
        _, problems = walk.settings_list_problems(settings_list(summaries=lines))
        self.assertEqual(problems, ["a raw hash on a main row: 'on <3e35f10c…>'"])
        broken = settings_list() + [node("status", "the request did not decode: x")]
        _, problems = walk.settings_list_problems(broken)
        self.assertEqual(problems, ["on screen: 'the request did not decode: x'"])

    def test_a_page_shows_its_own_mark(self):
        self.assertIsNone(walk.page_mark_missing("Maps", [node("maps_auto")]))
        self.assertEqual(walk.page_mark_missing("Maps", [node("title", "Maps")]),
                         "Maps without its rid 'maps_auto'")
        self.assertIsNone(walk.page_mark_missing("Privacy", [node("", "Positions you shared")]))
        self.assertIsNone(walk.page_mark_missing("Backup", []))


def maps_page(on=True, used="Storage used: 1.4 MB", rows=(("Around 43.77° N, 11.25° E",
              "1.4 MB · Oct 31 · zoom 0 to 16"),)):
    """The Maps page as SettingsActivity.showMaps builds it (1207)."""
    switch = walk.Node(text="", desc="", rid="network.leviculum.loquax.debug:id/maps_auto",
                       cls="android.widget.Switch", bounds=(900, 200, 1000, 260), checked=on)
    nodes = [switch, at("maps_used", used, 300)]
    for i, (name, line) in enumerate(rows):
        nodes.append(at("maps_region_name", name, 400 + 100 * i))
        nodes.append(at("maps_region_line", line, 440 + 100 * i))
    return nodes


class MapsPage(unittest.TestCase):
    def test_the_page_as_built_has_no_problem(self):
        detail, problems = walk.maps_page_problems(maps_page())
        self.assertEqual(problems, [])
        self.assertEqual(detail, "auto=on used=1.4 MB regions=1")
        german = maps_page(used="Speicher belegt: 1,4 MB",
                           rows=(("Rund um 43,77° N, 11,25° E", "1,4 MB · 31. Okt. · Zoom 0 bis 16"),))
        self.assertEqual(walk.maps_page_problems(german)[1], [])

    def test_lews_phone_on_2026_10_31(self):
        """Switch off, 0 B, no maps, the toast's line in the dump."""
        nodes = maps_page(on=False, used="Storage used: 0 B", rows=()) + [
            node("", "No maps on this phone yet."),
            node("", "the request did not decode: missing field `w`"),
        ]
        detail, problems = walk.maps_page_problems(nodes)
        self.assertEqual(detail, "auto=off used=0 B regions=0")
        self.assertEqual(problems, [
            "the automatic download shows off",
            "the storage line says 'Storage used: 0 B'",
            "no row of the fixture region among []",
            "on screen: 'the request did not decode: missing field `w`'",
        ])

    def test_a_region_without_its_size_and_no_switch(self):
        rows = (("Around 43.77° N, 11.25° E", " · Oct 31 · zoom 0 to 16"),)
        _, problems = walk.maps_page_problems(maps_page(rows=rows))
        self.assertEqual(problems, [f"a region without its size: {[rows[0][1]]}"])
        _, problems = walk.maps_page_problems(maps_page()[1:])
        self.assertEqual(problems, ["no automatic download switch"])

    def test_delete_all_row_as_built_and_after(self):
        """1290: the row below the regions with the storage line's total;
        after the yes no region, 0 B and the row off."""
        row = at("maps_delete_all", "Delete all downloaded maps (1.4 MB)", 600)
        total, problems = walk.delete_all_row_problems(maps_page() + [row])
        self.assertEqual((total, problems), ("1.4 MB", []))
        off = walk.Node(text="Delete all downloaded maps (0 B)", desc="",
                        rid=row.rid, cls="", bounds=row.bounds, enabled=False)
        empty = maps_page(used="Storage used: 0 B", rows=()) + [off]
        self.assertEqual(walk.delete_all_row_problems(empty), ("0 B", []))

    def test_delete_all_row_problems(self):
        self.assertEqual(walk.delete_all_row_problems(maps_page()), ("?", ["no Delete all row"]))
        wrong = at("maps_delete_all", "Delete all downloaded maps (2.8 MB)", 300)
        off = walk.Node(text=wrong.text, desc="", rid=wrong.rid, cls="", bounds=wrong.bounds,
                        enabled=False)
        _, problems = walk.delete_all_row_problems(maps_page() + [off])
        self.assertEqual(problems, [
            "the row's total '2.8 MB' is not the storage line's 'Storage used: 1.4 MB'",
            "the row is not below every region",
            "the row is off with 1 regions",
        ])
        _, problems = walk.delete_all_row_problems(
            maps_page(rows=()) + [at("maps_delete_all", "Delete all", 600)])
        self.assertEqual(problems, ["the row says 'Delete all'", "the row is on with 0 regions"])

    def test_delete_all_question(self):
        ask = node("", "Delete all 1 maps? Areas you visit will be downloaded again when you "
                       "look at them.")
        yes = node("", walk.DELETE_ALL_YES)
        self.assertEqual(walk.delete_all_ask_problems([ask, yes], 1), [])
        german = node("", "Alle 3 Karten löschen? Gebiete, die du besuchst, werden wieder "
                          "geladen, wenn du sie anschaust.")
        self.assertEqual(walk.delete_all_ask_problems([german, yes], 3), [])
        self.assertEqual(walk.delete_all_ask_problems([ask], 2), [
            "the question counts 1 maps, the page listed 2",
            "no 'Delete all' button",
        ])
        manual = node("", "Delete all 1 maps? They come back only when you load an area again.")
        self.assertTrue(walk.delete_all_ask_problems([manual, yes], 1)[0].startswith("no question"))

    def test_the_dump_reads_a_buttons_enabled(self):
        xml = ('<hierarchy rotation="0"><node index="0" text="x" resource-id="x:id/maps_delete_all" '
               'class="android.widget.Button" enabled="false" bounds="[0,0][1,1]" /></hierarchy>')
        self.assertFalse(walk.parse_dump(xml)[0].enabled)
        self.assertTrue(walk.parse_dump(xml.replace('enabled="false" ', ""))[0].enabled)

    def test_the_dump_reads_a_switchs_state(self):
        xml = ('<hierarchy rotation="0"><node index="0" text="" resource-id="x:id/maps_auto" '
               'class="android.widget.Switch" checked="true" bounds="[0,0][1,1]" /></hierarchy>')
        self.assertTrue(walk.parse_dump(xml)[0].checked)
        self.assertFalse(walk.parse_dump(xml.replace('checked="true"', 'checked="false"'))[0].checked)

    def test_the_fixture_region_exists(self):
        self.assertTrue(walk.FIXTURE_REGION.is_file(), walk.FIXTURE_REGION)


ME = "3e35f10c9a7b4d2e8f1a0b6c5d4e3f21"


class Profile(unittest.TestCase):
    def test_the_profile_as_built_has_no_problem(self):
        nodes = [node("profile_address", ME), node("profile_qr"), node("profile_share", "Share")]
        self.assertEqual(walk.profile_problems(nodes, ME), (ME, []))
        self.assertEqual(walk.profile_problems(nodes, None), (ME, []))

    def test_a_missing_or_wrong_address_a_missing_code_and_share(self):
        self.assertEqual(walk.profile_problems([], None),
                         ("", ["no address row", "no QR code", "no Share"]))
        other = "0" * 32
        _, problems = walk.profile_problems(
            [node("profile_address", other), node("profile_qr"), node("profile_share")], ME)
        self.assertEqual(problems, [f"the address row shows {other}, the mailbox is {ME}"])
        _, problems = walk.profile_problems(
            [node("profile_address", "<3e35f10c…>"), node("profile_qr"), node("profile_share")], ME)
        self.assertEqual(problems, ["the address row shows '<3e35f10c…>'"])

    def test_the_settings_profile_entry_shows_the_code(self):
        self.assertIsNone(walk.page_mark_missing("Profile", [node("profile_qr")]))
        self.assertEqual(walk.page_mark_missing("Profile", []),
                         "Profile without its rid 'profile_qr'")


class Dump(unittest.TestCase):
    def setUp(self):
        self.nodes = walk.parse_dump(DUMP)

    def test_every_node_in_document_order_without_the_tty_line(self):
        self.assertEqual(len(self.nodes), 7)
        self.assertEqual(self.nodes[1].text, "Loquax")
        self.assertEqual(self.nodes[1].bounds, (160, 80, 600, 160))

    def test_entities_decode_to_the_names_a_screen_shows(self):
        names = [n.text for n in walk.find(self.nodes, rid="name")]
        self.assertEqual(names, ["Zoë 💬 Ærø", "Jürgen 🚲", "<5ab3c2d1…>"])

    def test_texts_take_text_and_content_description(self):
        self.assertEqual(walk.texts(self.nodes)[:2], ["Loquax", "More"])

    def test_find_by_text_desc_and_id(self):
        self.assertEqual(walk.find(self.nodes, desc="More")[0].center, (1010, 120))
        self.assertTrue(walk.find(self.nodes, desc="More")[0].clickable)
        self.assertEqual(len(walk.find(self.nodes, text="Jürgen", contains=True)), 1)
        self.assertEqual(walk.find(self.nodes, text="Jürgen"), [])
        self.assertTrue(walk.find(self.nodes, rid="list")[0].scrollable)
        # The id's own name, not a suffix of a longer one.
        self.assertEqual(walk.find(self.nodes, rid="ame"), [])

    def test_by_top_orders_the_rows_as_they_stand(self):
        rows = walk.by_top(walk.find(self.nodes, rid="name"))
        self.assertEqual([n.text for n in rows], ["Jürgen 🚲", "Zoë 💬 Ærø", "<5ab3c2d1…>"])
        self.assertTrue(rows[2].long_clickable)

    def test_no_xml_is_no_nodes(self):
        self.assertEqual(walk.parse_dump("ERROR: null root node returned by UiTestAutomationBridge."), [])

    def test_raw_utf8_and_lone_surrogates(self):
        # A newer serializer writes the emoji itself; a lone surrogate
        # reference is no character and reads as U+FFFD.
        dump = (
            '<?xml version="1.0"?><hierarchy><node text="Ratte Größe 🐀" bounds="[0,0][1,1]"/>'
            '<node text="a&#55357;b" bounds="[0,0][1,1]"/></hierarchy>'
        )
        self.assertEqual(walk.texts(walk.parse_dump(dump)), ["Ratte Größe 🐀", "a\ufffdb"])

    def test_join_surrogates(self):
        self.assertEqual(walk.join_surrogates("&#55357;&#56320;"), "&#128000;")
        self.assertEqual(walk.join_surrogates("&#56320;&#55357;"), "&#65533;&#65533;")
        self.assertEqual(walk.join_surrogates("&#252;&#59999;"), "&#252;&#59999;")

    def test_hash_names(self):
        self.assertTrue(walk.is_hash_name("<5ab3c2d1…>"))
        for text in ["<5ab3c2d1>", "<5AB3C2D1…>", "Peer 5", "<5ab3c2d1…> x"]:
            self.assertFalse(walk.is_hash_name(text), text)


def mark_node(rid, desc, top):
    """A delivery mark as the chat's dump has it (1232)."""
    return (
        f'<node index="0" text="" resource-id="network.leviculum.loquax.debug:id/{rid}" '
        f'class="android.widget.ImageView" content-desc="{desc}" clickable="false" '
        f'bounds="[900,{top}][948,{top + 48}]" />'
    )


class Marks(unittest.TestCase):
    """The ladder's step: five rungs by id, each with its own words."""

    ALL = [
        ("state_clock", "Sending"),
        ("state_tick", "Waiting in the mailbox"),
        ("state_ticks", "Delivered"),
        ("state_ticks_read", "Read at 10:32"),
        ("state_failed", "Not sent. Double tap to send again"),
    ]

    def dump(self, marks):
        body = "".join(mark_node(rid, desc, 200 + 100 * i) for i, (rid, desc) in enumerate(marks))
        return walk.parse_dump(f'<hierarchy rotation="0">{body}</hierarchy>')

    def test_five_rungs_with_their_words_pass(self):
        nodes = self.dump(self.ALL)
        seen = {walk.mark_rid(n): n.desc for n in walk.marks_on(nodes)}
        self.assertEqual(walk.marks_problems(seen), [])

    def test_a_missing_rung_and_wrong_words_are_named(self):
        marks = [m for m in self.ALL if m[0] != "state_ticks_read"]
        marks[1] = ("state_tick", "Delivered")
        nodes = self.dump(marks)
        seen = {walk.mark_rid(n): n.desc for n in walk.marks_on(nodes)}
        problems = walk.marks_problems(seen)
        self.assertIn("no state_ticks_read", problems)
        self.assertTrue(any(p.startswith("state_tick says 'Delivered'") for p in problems), problems)

    def test_the_old_mark_id_is_no_rung(self):
        nodes = self.dump([("mark", "delivered ✓")])
        self.assertEqual(walk.marks_on(nodes), [])

    def test_a_tick_is_any_confirmed_rung(self):
        self.assertEqual(walk.TICKS, {"state_tick", "state_ticks", "state_ticks_read"})


def privacy_page(on=True, row_top=200, blocked_top=400):
    """The Privacy page as SettingsActivity.privacy builds it (1233)."""
    switch = walk.Node(text="", desc="Send read receipts",
                       rid="network.leviculum.loquax.debug:id/privacy_receipts",
                       cls="android.widget.Switch", bounds=(900, row_top, 1000, row_top + 60),
                       checked=on)
    return [
        at("", "Send read receipts", row_top),
        at("", "Others see when you have read their messages. Off, you do not see it "
               "from others either.", row_top + 30),
        switch,
        at("", "Blocked contacts", blocked_top),
        at("", "Positions you shared", blocked_top + 200),
    ]


class PrivacyPage(unittest.TestCase):
    def test_the_page_as_built_has_no_problem(self):
        self.assertEqual(walk.privacy_page_problems(privacy_page()), (True, []))
        self.assertEqual(walk.privacy_page_problems(privacy_page(on=False)), (False, []))

    def test_no_switch_no_line_and_the_row_below_the_blocked(self):
        state, problems = walk.privacy_page_problems(privacy_page()[3:])
        self.assertIsNone(state)
        self.assertIn("no read receipts switch", problems)
        self.assertIn("no 'Send read receipts'", problems)
        self.assertIn("no line under the read receipts row", problems)
        _, problems = walk.privacy_page_problems(privacy_page(row_top=600))
        self.assertEqual(problems, ["the read receipts row is not above the blocked contacts"])

    def test_the_rungs_with_the_switch_on_and_off(self):
        everything = set(walk.MARK_WORDS)
        self.assertEqual(walk.receipts_rungs_problems(everything, True), [])
        self.assertEqual(walk.receipts_rungs_problems(everything - {"state_ticks_read"}, True),
                         ["on: no read mark"])
        self.assertEqual(walk.receipts_rungs_problems(everything - {"state_ticks_read"}, False), [])
        self.assertEqual(walk.receipts_rungs_problems(everything, False),
                         ["off: a read mark still shows"])
        self.assertEqual(walk.receipts_rungs_problems({"state_clock"}, False),
                         ["off: no delivered mark"])

    def test_the_walk_runs_the_step_after_settings(self):
        self.assertEqual(walk.STEPS.index("receipts"), walk.STEPS.index("settings") + 1)


class Code(unittest.TestCase):
    ADMIT = (
        'Admission code for "walk", good for one device, once:\n\n'
        "lqxh1abc234@192.168.240.1:4242\n\n"
        "On a desktop: loquax mailbox join lqxh1abc234\n"
    )

    def test_the_code_with_its_server(self):
        self.assertEqual(walk.parse_code(self.ADMIT), "lqxh1abc234@192.168.240.1:4242")

    def test_a_code_without_server(self):
        self.assertEqual(walk.parse_code("join lqxh1abc234\n"), "lqxh1abc234")

    def test_none(self):
        self.assertIsNone(walk.parse_code("loquax: no mailbox here"))


class LinkDevice(unittest.TestCase):
    """The sheet of "Link a device" (1264)."""

    CODE = "lqxh1abcdefgh234567@192.168.240.1:4242"

    def test_a_code_with_its_server_and_its_qr_passes(self):
        self.assertEqual(
            walk.link_code_problems(self.CODE, [self.CODE.upper()], "192.168.240.1:4242"), []
        )

    def test_a_contact_code_or_another_server_is_named(self):
        problems = walk.link_code_problems("lxma://00", ["lxma://00"], "192.168.240.1:4242")
        self.assertIn("no join code", problems[0])
        problems = walk.link_code_problems(self.CODE, [self.CODE], "10.0.0.2:4242")
        self.assertIn("does not carry 10.0.0.2:4242", problems[0])

    def test_a_qr_that_reads_otherwise_or_twice_is_named(self):
        problems = walk.link_code_problems(self.CODE, ["lqxh1other"], "192.168.240.1:4242")
        self.assertIn("the QR reads", problems[0])
        problems = walk.link_code_problems(self.CODE, [], "192.168.240.1:4242")
        self.assertIn("holds 0 codes", problems[0])

    def test_the_step_comes_before_the_move(self):
        self.assertEqual(walk.STEPS[-3:], ["link_device", "move_phone", "teardown"])


class Screencap(unittest.TestCase):
    def test_both_headers(self):
        for header in (12, 16):
            data = screencap(2, 2, [(1, 2, 3), (4, 5, 6), (7, 8, 9), (10, 11, 12)], header)
            screen = walk.parse_screencap(data)
            self.assertEqual((screen.width, screen.height), (2, 2))
            self.assertEqual(screen.pixel(1, 0), (4, 5, 6))
            self.assertEqual(screen.pixel(0, 1), (7, 8, 9))
            # Outside clamps to the edge.
            self.assertEqual(screen.pixel(5, 5), (10, 11, 12))

    def test_refuses_a_wrong_format_or_size(self):
        with self.assertRaises(ValueError):
            walk.parse_screencap(screencap(2, 2, filled(2, 2, (0, 0, 0)), fmt=4))
        with self.assertRaises(ValueError):
            walk.parse_screencap(screencap(2, 2, filled(2, 2, (0, 0, 0)))[:-1])
        with self.assertRaises(ValueError):
            walk.parse_screencap(b"\x00" * 8)


class Sampling(unittest.TestCase):
    def setUp(self):
        # 40 x 40: the left half grey, the right half white; one black
        # quarter of the right half's top (x 30..40, y 0..10).
        pixels = []
        for y in range(40):
            for x in range(40):
                if x >= 30 and y < 10:
                    pixels.append((0, 0, 0))
                elif x < 20:
                    pixels.append((120, 120, 120))
                else:
                    pixels.append((250, 250, 250))
        self.screen = walk.parse_screencap(screencap(40, 40, pixels))
        self.box = (0, 0, 40, 40)

    def test_grid_spreads_inside_the_box(self):
        samples = walk.sample_grid(self.screen, (0, 20, 20, 40), 4, 4)
        self.assertEqual(len(samples), 16)
        self.assertTrue(walk.is_uniform(samples))
        self.assertEqual(samples[0], (120, 120, 120))

    def test_uniform_only_when_flat(self):
        self.assertFalse(walk.is_uniform(walk.sample_grid(self.screen, self.box)))
        self.assertTrue(walk.is_uniform([(10, 10, 10), (14, 12, 16)], tolerance=6))
        self.assertFalse(walk.is_uniform([(10, 10, 10), (17, 10, 10)], tolerance=6))
        self.assertTrue(walk.is_uniform([]))

    def test_black_cells_count_the_black_part(self):
        self.assertEqual(walk.black_cells(self.screen, self.box), 1)
        grey = walk.parse_screencap(screencap(8, 8, filled(8, 8, (30, 30, 30))))
        self.assertEqual(walk.black_cells(grey, (0, 0, 8, 8)), 0)
        black = walk.parse_screencap(screencap(8, 8, filled(8, 8, (0, 0, 0))))
        self.assertEqual(walk.black_cells(black, (0, 0, 8, 8)), 16)

    def test_changed_fraction(self):
        a = [(0, 0, 0)] * 4
        b = [(0, 0, 0), (0, 0, 17), (10, 10, 10), (255, 0, 0)]
        self.assertEqual(walk.changed_fraction(a, a), 0.0)
        self.assertEqual(walk.changed_fraction(a, b), 0.5)
        self.assertEqual(walk.changed_fraction(a, b[:3]), 1.0)

    def test_fling_problems_want_three_different_grabs(self):
        a = [(0, 0, 0)] * 10
        b = [(0, 0, 0)] * 5 + [(200, 200, 200)] * 5
        c = [(200, 200, 200)] * 10
        self.assertEqual(walk.fling_problems(a, b, c), [])
        self.assertEqual(walk.fling_problems(a, b, b), ["later=final (0.000 changed)"])
        self.assertEqual(
            walk.fling_problems(a, a, a),
            ["early=later (0.000 changed)", "early=final (0.000 changed)", "later=final (0.000 changed)"],
        )

    def test_mean_luma(self):
        self.assertAlmostEqual(walk.mean_luma([(255, 255, 255)]), 255.0)
        self.assertAlmostEqual(walk.mean_luma([(0, 0, 0), (255, 255, 255)]), 127.5)
        self.assertEqual(walk.mean_luma([]), 0.0)


class Buildings(unittest.TestCase):
    """The buildings step's reading of the screen (1193)."""

    def test_the_middle_half(self):
        self.assertEqual(walk.middle_half(1080, 2400), (270, 600, 810, 1800))

    def test_the_fill_share_counts_either_table_within_the_tolerance(self):
        light, dark = walk.BUILDING_FILL_LIGHT, walk.BUILDING_FILL_DARK
        near = tuple(c + 3 for c in light)
        off = tuple(c + 4 for c in light)
        samples = [light, dark, near, off, (0, 0, 0), (255, 255, 255), (250, 250, 250), (1, 2, 3)]
        self.assertEqual(walk.colour_share(samples, [light, dark]), 3 / 8)
        self.assertEqual(walk.colour_share(samples, [dark]), 1 / 8)
        self.assertEqual(walk.colour_share([], [light]), 0.0)

    def test_the_fill_is_read_off_a_screencap(self):
        fill = walk.BUILDING_FILL_LIGHT
        # A screen whose left quarter of the middle is fill.
        pixels = [fill if x < 10 else (240, 238, 232) for _ in range(40) for x in range(40)]
        screen = walk.parse_screencap(screencap(40, 40, pixels))
        samples = walk.sample_grid(screen, walk.middle_half(40, 40), 10, 10)
        self.assertEqual(walk.colour_share(samples, [fill]), 0.0)
        samples = walk.sample_grid(screen, (0, 0, 40, 40), 8, 8)
        self.assertEqual(walk.colour_share(samples, [fill]), 0.25)

    def test_the_verdict(self):
        least, most = walk.BUILDINGS_LEAST, walk.BUILDINGS_Z15_MOST
        self.assertEqual(walk.buildings_problems(0.2, 0.0, 1500, 1200), [])
        bound = int(1200 * walk.SETTLE_FACTOR) + walk.SETTLE_SLACK_MS
        self.assertEqual(walk.buildings_problems(0.2, 0.0, bound, 1200), [])
        self.assertEqual(
            walk.buildings_problems(least / 2, most * 2, bound + 1, 1200),
            [
                f"z16 fill {least / 2:.3f} under {least}",
                f"z15 fill {most * 2:.3f} over {most}",
                f"z16 at rest after {bound + 1} ms, over {bound} ms (z15 1200 ms)",
            ],
        )


class Lines(unittest.TestCase):
    def test_a_step_line_round_trips(self):
        line = walk.walk_line("list", True, 1234.7, "rows=18 of 18\n named=12")
        self.assertEqual(line, "WALK step=list ok=1 ms=1234 detail=rows=18 of 18 named=12")
        self.assertEqual(
            walk.parse_walk_line(line),
            {"step": "list", "ok": True, "ms": 1234, "detail": "rows=18 of 18 named=12"},
        )

    def test_a_failed_step_and_an_empty_detail(self):
        line = walk.walk_line("map", False, 0, "")
        self.assertEqual(line, "WALK step=map ok=0 ms=0 detail=")
        self.assertFalse(walk.parse_walk_line(line)["ok"])

    def test_other_lines_are_none(self):
        for line in ["", "WALK_SUMMARY ok=1 failed=0 ms=3", "WALK step=x ok=2 ms=1 detail=", "  list rows seen"]:
            self.assertIsNone(walk.parse_walk_line(line), line)

    def test_summary(self):
        self.assertEqual(walk.summary_line(9, 1, 61234.9), "WALK_SUMMARY ok=9 failed=1 ms=61234")

    def test_every_step_has_its_method(self):
        for step in walk.STEPS:
            self.assertTrue(callable(getattr(walk.Walk, "step_" + step, None)), step)


class InputText(unittest.TestCase):
    def test_spaces_and_shell_specials(self):
        self.assertEqual(walk.input_text_arg("walk reply 123"), "walk%sreply%s123")
        self.assertEqual(walk.input_text_arg("lqxh1ab@10.0.0.1:4242"), "lqxh1ab@10.0.0.1:4242")
        self.assertEqual(walk.input_text_arg("a&b;c'd"), "a\\&b\\;c\\'d")

    def test_ascii_only(self):
        with self.assertRaises(ValueError):
            walk.input_text_arg("Grüße")


if __name__ == "__main__":
    unittest.main()


# Two records as `dumpsys notification --noredact` lists them on Android
# 13, cut: the session's own, untagged, and a conversation's, tagged by its
# peer; a record of another app between them.
NOTIFICATIONS = """\
  Notification List:
    NotificationRecord(0x0a1b2c3d: pkg=network.leviculum.loquax.debug user=UserHandle{0} id=1 tag=null importance=2 key=0|network.leviculum.loquax.debug|1|null|10123: Notification(channel=session))
      uid=10123 userId=0
      mUpdateTimeMs=1793480000000
    NotificationRecord(0x0e0f: pkg=com.android.systemui user=UserHandle{0} id=7 tag=abcd importance=3 key=x: Notification(channel=x))
      mUpdateTimeMs=1793480000001
    NotificationRecord(0x0b2c3d4e: pkg=network.leviculum.loquax.debug user=UserHandle{0} id=0 tag=3b5c00112233445566778899aabbccdd importance=4 key=0|network.leviculum.loquax.debug|0|3b5c00112233445566778899aabbccdd|10123: Notification(channel=messages))
      uid=10123 userId=0
      mUpdateTimeMs=1793480123456
"""


class Notifications(unittest.TestCase):
    """The receive_report step's notification check (1209)."""

    def test_the_conversations_records_by_tag_with_their_time(self):
        got = walk.conversation_notifications(NOTIFICATIONS, walk.PACKAGE)
        self.assertEqual(got, {"3b5c00112233445566778899aabbccdd": "1793480123456"})

    def test_none_listed_is_none(self):
        self.assertEqual(walk.conversation_notifications("", walk.PACKAGE), {})

    def test_another_package_with_our_prefix_is_not_ours(self):
        other = NOTIFICATIONS.replace("loquax.debug user", "loquax.debugx user")
        self.assertEqual(walk.conversation_notifications(other, walk.PACKAGE), {})


# The session's record with its extras, as `--noredact` prints them; the
# conversation record below it carries a text of its own.
SESSION_NOTIFICATIONS = """\
    NotificationRecord(0x0a1b2c3d: pkg=network.leviculum.loquax.debug user=UserHandle{0} id=1 tag=null importance=2 key=0|network.leviculum.loquax.debug|1|null|10123: Notification(channel=session))
      uid=10123 userId=0
      extras={
        android.title=String (Loquax)
        android.text=String (Connected to your mailbox, 3 unread)
      }
    NotificationRecord(0x0b2c3d4e: pkg=network.leviculum.loquax.debug user=UserHandle{0} id=0 tag=3b5c00112233445566778899aabbccdd importance=4 key=x: Notification(channel=messages))
      extras={
        android.text=String (hallo)
      }
"""


class SessionLine(unittest.TestCase):
    """The always-on notification's line (1278, walk finding 19)."""

    SERVER = "192.168.240.1:4242"

    def test_the_untagged_records_text_is_the_line(self):
        line = walk.session_line(SESSION_NOTIFICATIONS, walk.PACKAGE)
        self.assertEqual(line, "Connected to your mailbox, 3 unread")
        self.assertEqual(walk.session_line_problems(line, self.SERVER), [])
        self.assertEqual(walk.session_line_problems("Connected to your mailbox", self.SERVER), [])

    def test_none_the_old_words_or_the_server_are_named(self):
        self.assertIsNone(walk.session_line(NOTIFICATIONS, walk.PACKAGE))
        self.assertEqual(walk.session_line_problems(None, self.SERVER), ["no session notification"])
        problems = walk.session_line_problems(f"Connected to {self.SERVER}", self.SERVER)
        self.assertIn("the line says", problems[0])
        self.assertIn("names the server", problems[1])
        problems = walk.session_line_problems(walk.NOT_CONNECTED, self.SERVER)
        self.assertEqual(len(problems), 1)


def edit(rid, top):
    return walk.Node(text="", desc="", rid="network.leviculum.loquax.debug:id/" + rid,
                     cls="android.widget.EditText", bounds=(0, top, 100, top + 40))


class FirstStartName(unittest.TestCase):
    """The address page's one field (1278, walk finding 9)."""

    def test_the_name_alone_above_the_qr_passes(self):
        nodes = [edit("welcome_name", 100), boxed("welcome_qr", (0, 200, 240, 440))]
        self.assertEqual(walk.first_start_name_problems(nodes), [])

    def test_no_field_one_below_or_a_second_is_named(self):
        qr = boxed("welcome_qr", (0, 200, 240, 440))
        self.assertIn("no name field", walk.first_start_name_problems([qr]))
        self.assertIn(
            "the name field is below the QR",
            walk.first_start_name_problems([qr, edit("welcome_name", 500)]),
        )
        self.assertIn(
            "2 fields on the page, not the name alone",
            walk.first_start_name_problems([edit("welcome_name", 100), edit("other", 150), qr]),
        )

    def test_the_new_steps_stand_where_they_need_their_state(self):
        steps = walk.STEPS
        self.assertLess(steps.index("first_start"), steps.index("notification_text"))
        self.assertLess(steps.index("pencil_scan"), steps.index("share_in_contact"))
        self.assertEqual(
            steps[-4:], ["first_start_name", "link_device", "move_phone", "teardown"]
        )
        for step in ("notification_text", "share_in_contact", "first_start_name"):
            self.assertTrue(callable(getattr(walk.Walk, "step_" + step)), step)


class MovePhone(unittest.TestCase):
    """The old phone's first screen after the move (1281, walk finding 24)."""

    def moved_text(self):
        return walk.Node(text=walk.MOVED_TITLE + "\n\nThis phone has moved. Your mailbox is on your new phone now.",
                         desc="", rid="network.leviculum.loquax.debug:id/error", cls="", bounds=(0, 0, 1, 1))

    def test_the_moved_page_with_its_button_passes(self):
        nodes = [self.moved_text(), node(walk.TAKE_BACK_ID, walk.TAKE_BACK)]
        self.assertEqual(walk.moved_page_problems(nodes), [])

    def test_no_title_another_button_or_a_join_form_is_named(self):
        self.assertIn(f"no {walk.MOVED_TITLE!r}",
                      walk.moved_page_problems([node(walk.TAKE_BACK_ID, walk.TAKE_BACK)]))
        self.assertIn(f"no {walk.TAKE_BACK!r} button",
                      walk.moved_page_problems([self.moved_text(), node(walk.TAKE_BACK_ID, "Try again")]))
        self.assertIn("1 fields on the moved page",
                      walk.moved_page_problems([self.moved_text(), node(walk.TAKE_BACK_ID, walk.TAKE_BACK),
                                                edit("code", 100)]))

    def test_the_step_is_last_before_the_teardown_and_runs(self):
        self.assertEqual(walk.STEPS[-2:], ["move_phone", "teardown"])
        self.assertTrue(callable(getattr(walk.Walk, "step_move_phone")))
        self.assertEqual(walk.parse_args(["--walk-move", "/x/walk_move"]).walk_move, "/x/walk_move")


def boxed(rid, bounds, desc=""):
    return walk.Node(text="", desc=desc, rid="network.leviculum.loquax.debug:id/" + rid, cls="", bounds=bounds)


class Photo(unittest.TestCase):
    """The image step's reading of the screen (1220)."""

    def node(self, desc: str, bounds=(0, 0, 480, 640)) -> "walk.Node":
        return walk.Node("", desc, "network.leviculum.loquax:id/photo", "android.widget.ImageView", bounds)

    def test_a_new_named_photo_is_right(self):
        photos = [self.node("Photo, 104 KB"), self.node("Photo, 1.4 MB", (0, 700, 360, 1180))]
        self.assertEqual(walk.photo_problems(photos, 1), [])

    def test_none_new_unnamed_or_a_sliver_is_refused(self):
        self.assertEqual(walk.photo_problems([], 0), ["0 photos on screen, 0 before the send"])
        self.assertEqual(
            walk.photo_problems([self.node("")], 0), ["a photo says ''"]
        )
        self.assertEqual(
            walk.photo_problems([self.node("Photo, 9 KB", (0, 0, 480, 20))], 0),
            ["a photo is 480x20"],
        )

    def test_only_photo_ids_are_photos_top_first(self):
        low = self.node("Photo, 1 KB", (0, 900, 100, 1000))
        high = self.node("Photo, 2 KB", (0, 100, 100, 200))
        other = walk.Node("", "Photo, 3 KB", "network.leviculum.loquax:id/body", "", (0, 0, 9, 9))
        self.assertEqual(walk.photos_on([low, other, high]), [high, low])


class PhotoBubble(unittest.TestCase):
    """A photo without words is one bubble with its mark (1268)."""

    photo = walk.Node("", "Photo, 104 KB", "network.leviculum.loquax:id/photo", "", (500, 400, 1040, 1000))
    mark = boxed("state_ticks", (990, 1030, 1030, 1060), "Delivered")
    bubble = boxed("bubble", (480, 380, 1060, 1080))

    def test_the_photo_and_its_mark_in_one_bubble_is_right(self):
        nodes = [self.bubble, self.photo, self.mark]
        self.assertEqual(walk.photo_bubble_problems(nodes, self.photo), [])

    def test_lews_phone_on_2026_11_01(self):
        # The photo above, an empty bubble under it, the mark nowhere.
        empty = boxed("bubble", (980, 1010, 1060, 1100))
        problems = walk.photo_bubble_problems([self.photo, empty], self.photo)
        self.assertEqual(problems, ["the photo is in 0 bubbles, want one"])
        # Its own bubble right, an empty one beside it is still named.
        below = boxed("bubble", (980, 1100, 1060, 1180))
        nodes = [self.bubble, self.photo, self.mark, below]
        self.assertEqual(
            walk.photo_bubble_problems(nodes, self.photo),
            ["an empty bubble at (980, 1100, 1060, 1180)"],
        )

    def test_an_empty_bubble_a_text_row_and_no_mark_are_named(self):
        empty = boxed("bubble", (980, 1100, 1060, 1180))
        body = walk.Node(" ", "", "network.leviculum.loquax:id/body", "", (500, 1010, 900, 1060))
        problems = walk.photo_bubble_problems([self.bubble, self.photo, body, empty], self.photo)
        self.assertEqual(
            problems,
            [
                "its bubble has a text row ' '",
                "an empty bubble at (980, 1100, 1060, 1180)",
                "its bubble has no mark",
            ],
        )


class ComposeBar(unittest.TestCase):
    """The compose bar as Signal has it (1270): its buttons, in order and
    named, the round one outside the field; a caption in the photo's
    bubble; the mailbox's sends to a peer."""

    field = boxed("compose", (120, 2200, 700, 2300))

    def bar(self, ids, desc="words"):
        x = {"emoji": 20, "locate": 720, "camera": 800, "record": 880, "plus": 980, "send": 980}
        return [boxed(rid, (x[rid], 2210, x[rid] + 80, 2290), desc) for rid in ids]

    def test_the_empty_field_has_five_buttons_and_the_typed_four(self):
        empty = [self.field] + self.bar(walk.BAR_EMPTY)
        self.assertEqual(walk.bar_problems(empty, typed=False), [])
        typed = [self.field] + self.bar(walk.BAR_TYPED)
        self.assertEqual(walk.bar_problems(typed, typed=True), [])
        self.assertEqual(len(walk.BAR_EMPTY), 5)

    def test_the_old_bar_and_a_missed_swap_are_named(self):
        # Today's bar before 1270: the paperclip is no button of the
        # scene's, the round one was the microphone named send.
        old = [self.field] + self.bar(["emoji", "send"])
        self.assertEqual(
            walk.bar_problems(old, typed=False),
            [f"the bar has ['emoji', 'send'], want {walk.BAR_EMPTY}"],
        )
        stuck = [self.field] + self.bar(walk.BAR_EMPTY)
        self.assertEqual(
            walk.bar_problems(stuck, typed=True),
            [f"the bar has {walk.BAR_EMPTY}, want {walk.BAR_TYPED}"],
        )

    def test_a_button_without_words_or_on_the_wrong_side_is_named(self):
        nodes = [self.field] + self.bar(walk.BAR_EMPTY)
        nodes[2] = boxed("locate", (720, 2210, 800, 2290), "")
        self.assertEqual(
            walk.bar_problems(nodes, typed=False),
            ["locate has no words for a screen reader"],
        )
        left = [self.field] + self.bar(walk.BAR_EMPTY)
        left[3] = boxed("camera", (300, 2210, 380, 2290), "words")
        problems = walk.bar_problems(left, typed=False)
        self.assertIn("the bar has ['emoji', 'camera', 'locate', 'record', 'plus']", problems[0])
        self.assertTrue(any(p.startswith("camera is not right of the field") for p in problems))

    def test_the_round_button_must_be_outside(self):
        nodes = [self.field] + self.bar(["emoji", "locate", "camera", "record"])
        nodes.append(boxed("plus", (850, 2210, 930, 2290), "words"))
        problems = walk.bar_problems(nodes, typed=False)
        self.assertIn("the round button is not outside the field", problems)

    def test_a_caption_sits_in_the_photos_bubble_with_its_mark(self):
        bubble = boxed("bubble", (480, 380, 1060, 1180))
        photo = walk.Node("", "Photo, 104 KB", "network.leviculum.loquax:id/photo", "", (500, 400, 1040, 1000))
        body = walk.Node("the view", "", "network.leviculum.loquax:id/body", "", (500, 1010, 900, 1060))
        mark = boxed("state_ticks", (990, 1110, 1030, 1140), "Delivered")
        self.assertEqual(walk.caption_bubble_problems([bubble, photo, body, mark], photo, "the view"), [])
        # The caption in a bubble of its own: the old two-bubble shape.
        other = boxed("bubble", (480, 1200, 1060, 1300))
        apart = walk.Node("the view", "", "network.leviculum.loquax:id/body", "", (500, 1210, 900, 1260))
        self.assertEqual(
            walk.caption_bubble_problems([bubble, photo, other, apart, mark], photo, "the view"),
            ["its bubble says [], want ['the view']"],
        )
        self.assertEqual(
            walk.caption_bubble_problems([photo], photo, "the view"),
            ["the photo is in 0 bubbles, want one"],
        )

    def test_the_mailbox_sends_to_a_peer_are_counted(self):
        log = "\n".join(
            [
                "INFO HOME send accepted device=aa message=01 to=0c0c",
                "INFO HOME send accepted device=aa message=02 to=0d0d",
                "INFO HOME send accepted device=aa message=03 to=0c0c",
                "INFO something else to=0c0c",
            ]
        )
        self.assertEqual(walk.accepted_to(log, "0c0c"), 2)
        self.assertEqual(walk.accepted_to(log, "0e0e"), 0)


class Share(unittest.TestCase):
    """A share for a while (1273): the chat's chip, its words and its
    Stop; the end reports in the mailbox's outbox."""

    def chip(self, words, stop_desc="Stop"):
        return [
            node("share_chip"),
            node("share_words", text=words),
            node("share_stop", text="Stop", desc=stop_desc),
        ]

    def test_the_chip_with_its_words_and_stop_passes(self):
        self.assertEqual(
            walk.chip_problems(self.chip("Sharing your position, 42 min left"), walk.SHARE_LEFT), []
        )
        self.assertEqual(walk.chip_problems(self.chip("Sharing your position, 8 h left"), walk.SHARE_LEFT), [])
        self.assertEqual(walk.chip_problems(self.chip(walk.SHARE_UNTIL), walk.SHARE_UNTIL), [])

    def test_a_missing_chip_wrong_words_or_a_mute_stop_are_named(self):
        self.assertEqual(walk.chip_problems([node("compose")], walk.SHARE_LEFT), ["no share chip"])
        self.assertEqual(
            walk.chip_problems(self.chip(walk.SHARE_UNTIL), walk.SHARE_LEFT),
            [f"the chip says ['{walk.SHARE_UNTIL}'], want {walk.SHARE_LEFT.pattern!r}"],
        )
        mute = [node("share_chip"), node("share_words", text=walk.SHARE_UNTIL), node("share_stop")]
        self.assertEqual(
            walk.chip_problems(mute, walk.SHARE_UNTIL),
            ["the Stop has no words for a screen reader"],
        )
        self.assertEqual(
            walk.chip_problems(self.chip(walk.SHARE_UNTIL)[:2], walk.SHARE_UNTIL),
            ["the chip has no Stop"],
        )

    def test_end_reports_count_the_marked_sends_to_the_peer(self):
        import sqlite3
        import tempfile

        with tempfile.TemporaryDirectory() as home:
            store = Path(home) / "history.sqlite3"
            self.assertEqual(walk.end_reports(store, "0c" * 16), 0, "no store yet")
            conn = sqlite3.connect(store)
            conn.execute(
                "CREATE TABLE home_outbox(identity BLOB, client_id BLOB, id BLOB, destination BLOB, "
                "packed BLOB, created_at INTEGER, state INTEGER, propagated INTEGER)"
            )
            rows = [
                (b"\x0c" * 16, b"head" + walk.SHARE_ENDED + b"tail"),
                (b"\x0c" * 16, b"a running report \xa5every\xcd\x03\x84"),
                (b"\x0d" * 16, walk.SHARE_ENDED),
            ]
            for n, (destination, packed) in enumerate(rows):
                conn.execute(
                    "INSERT INTO home_outbox VALUES (?, ?, ?, ?, ?, 0, 0, 0)",
                    (b"me", bytes([n]), bytes([n]), destination, packed),
                )
            conn.commit()
            conn.close()
            self.assertEqual(walk.end_reports(store, "0c" * 16), 1)
            self.assertEqual(walk.end_reports(store, "0D" * 16), 1)
            self.assertEqual(walk.end_reports(store, "0e" * 16), 0)


class PictureZoom(unittest.TestCase):
    """The viewer's zoom step (1269): the touch device, the screen size,
    the pinch's events, the viewer's log line and where it put the
    picture."""

    # `getevent -pl` of an emulator: a keyboard, then the touch screen.
    GETEVENT = """add device 1: /dev/input/event1
  name:     "qwerty2"
  events:
    KEY (0001): KEY_ESC KEY_1
add device 2: /dev/input/event2
  name:     "virtio_input_multi_touch_1"
  events:
    KEY (0001): BTN_TOUCH
    ABS (0003): ABS_MT_SLOT           : value 0, min 0, max 9, fuzz 0, flat 0, resolution 0
                ABS_MT_TOUCH_MAJOR    : value 0, min 0, max 2147483647, fuzz 0, flat 0, resolution 0
                ABS_MT_POSITION_X     : value 0, min 0, max 32767, fuzz 0, flat 0, resolution 0
                ABS_MT_POSITION_Y     : value 0, min 0, max 32767, fuzz 0, flat 0, resolution 0
                ABS_MT_TRACKING_ID    : value 0, min 0, max 9, fuzz 0, flat 0, resolution 0
  input props:
    INPUT_PROP_DIRECT
"""

    def test_the_touch_screen_is_found_and_a_keyboard_is_not(self):
        touch = walk.parse_touch_device(self.GETEVENT)
        self.assertEqual(touch.path, "/dev/input/event2")
        self.assertEqual((touch.x_min, touch.x_max, touch.y_max), (0, 32767, 32767))
        self.assertTrue(touch.touch_major)
        self.assertFalse(touch.pressure)
        self.assertIsNone(walk.parse_touch_device(self.GETEVENT.split("add device 2")[0]))

    def test_a_direct_device_wins_over_a_touchpad(self):
        pad = self.GETEVENT.replace("INPUT_PROP_DIRECT", "INPUT_PROP_POINTER").replace(
            "/dev/input/event2", "/dev/input/event7"
        )
        # The pad first, the screen after it.
        both = pad + "add device 3: /dev/input/event2\n" + self.GETEVENT.split(
            "add device 2: /dev/input/event2\n"
        )[1]
        self.assertEqual(walk.parse_touch_device(both).path, "/dev/input/event2")
        self.assertEqual(walk.parse_touch_device(pad).path, "/dev/input/event7")

    def test_the_screen_size_takes_the_override(self):
        self.assertEqual(walk.parse_wm_size("Physical size: 1080x2340\n"), (1080, 2340))
        self.assertEqual(
            walk.parse_wm_size("Physical size: 1080x2340\nOverride size: 720x1560\n"),
            (720, 1560),
        )
        self.assertIsNone(walk.parse_wm_size("no size"))

    def test_the_pinch_spreads_two_fingers_and_lifts_both(self):
        touch = walk.parse_touch_device(self.GETEVENT)
        events = walk.pinch_events(touch, (1000, 2000), (500, 1000), (100, 400), steps=3)
        xs = {0: [], 1: []}
        slot = None
        for t, c, v in events:
            if (t, c) == (walk.EV_ABS, walk.ABS_MT_SLOT):
                slot = v
            elif (t, c) == (walk.EV_ABS, walk.ABS_MT_POSITION_X):
                xs[slot].append(v)
        # Screen x 400 and 600 at first, 100 and 900 at last, on 0..32767.
        scale = 32767 / 999
        self.assertEqual(xs[0][0], round(400 * scale))
        self.assertEqual(xs[1][0], round(600 * scale))
        self.assertEqual(xs[0][-1], round(100 * scale))
        self.assertEqual(xs[1][-1], round(900 * scale))
        self.assertEqual(len(xs[0]), 4)
        self.assertTrue(all(a > b for a, b in zip(xs[0], xs[0][1:])), xs[0])
        ids = [v for t, c, v in events if c == walk.ABS_MT_TRACKING_ID]
        self.assertEqual(ids, [1269, 1270, -1, -1])
        self.assertEqual(events[-1], (walk.EV_SYN, walk.SYN_REPORT, 0))
        self.assertIn((walk.EV_KEY, walk.BTN_TOUCH, 0), events)
        line = walk.sendevent_script(touch.path, events[:2])
        self.assertEqual(line, "sendevent /dev/input/event2 3 47 0; sendevent /dev/input/event2 3 57 1269")

    LOG = (
        "11-01 14:00:01.000  4711  4711 I loquax  : PHOTO_ZOOM scale=1.000 left=0.0 "
        "top=480.0 width=1080.0 height=1440.0 view=1080x2400\n"
        "11-01 14:00:03.000  4711  4711 I loquax  : PHOTO_ZOOM scale=2.500 left=-810.0 "
        "top=-600.0 width=2700.0 height=3600.0 view=1080x2400\n"
    )

    def test_the_viewers_lines_are_read_oldest_first(self):
        states = walk.parse_photo_zoom(self.LOG)
        self.assertEqual([z["scale"] for z in states], [1.0, 2.5])
        self.assertEqual(states[1]["left"], -810.0)
        self.assertEqual((states[1]["vw"], states[1]["vh"]), (1080.0, 2400.0))
        self.assertEqual(walk.parse_photo_zoom("nothing"), [])

    def test_a_picture_in_bounds_passes_and_one_off_them_fails(self):
        rest, zoomed = walk.parse_photo_zoom(self.LOG)
        self.assertEqual(walk.zoom_bounds_problems(rest), [])
        self.assertEqual(walk.zoom_bounds_problems(zoomed), [])
        # Panned past the left edge: a black gap on the right.
        gap = dict(zoomed, left=-1700.0)
        self.assertIn("x: 2700 px from -1700 leaves a gap", walk.zoom_bounds_problems(gap)[0])
        # At 1x not centred.
        off = dict(rest, top=100.0)
        self.assertIn("y: 1440 px at 100 is not centred", walk.zoom_bounds_problems(off)[0])

    def test_the_step_stands_after_the_image_it_zooms(self):
        self.assertEqual(walk.STEPS.index("picture_zoom"), walk.STEPS.index("image") + 1)


class File(unittest.TestCase):
    """The file step's reading of the screen (1221)."""

    def node(self, desc: str, top: int = 0) -> "walk.Node":
        return walk.Node("\u25a4  x\n2 KB", desc, "", "android.widget.TextView", (0, top, 400, top + 80))

    def test_a_new_file_with_its_name_is_right(self):
        files = [self.node("File a.pdf, 104 KB"), self.node("File walk-file-1.txt, 2 KB", 900)]
        self.assertEqual(walk.file_problems(files, 1, "walk-file-1.txt"), [])

    def test_none_new_or_another_name_is_refused(self):
        self.assertEqual(walk.file_problems([], 0, "a"), ["0 files on screen, 0 before the send"])
        self.assertEqual(
            walk.file_problems([self.node("File b, 1 B")], 0, "a"),
            ["the newest file says 'File b, 1 B', want 'a'"],
        )

    def test_only_what_is_named_a_file_is_a_file_top_first(self):
        low = self.node("File a, 1.4 MB", 900)
        high = self.node("File b, 2 KB", 100)
        other = self.node("Photo, 3 KB", 0)
        self.assertEqual(walk.files_on([low, other, high]), [high, low])
        self.assertEqual(walk.files_on([self.node("File a")]), [])


class Voice(unittest.TestCase):
    """The voice step's reading of the screen and its fixture (1222)."""

    def node(self, desc: str, top: int = 0) -> "walk.Node":
        return walk.Node("", desc, "", "android.widget.LinearLayout", (0, top, 400, top + 80))

    def test_a_new_long_enough_voice_message_is_right(self):
        voices = [self.node("Voice message, 0:07"), self.node("Voice message, 0:03", 900)]
        self.assertEqual(walk.voice_problems(voices, 1, 2), [])

    def test_none_new_or_too_short_is_refused(self):
        self.assertEqual(
            walk.voice_problems([], 0, 2), ["0 voice messages on screen, 0 before the hold"]
        )
        self.assertEqual(
            walk.voice_problems([self.node("Voice message, 0:01")], 0, 2),
            ["the newest is 1 s long, want at least 2"],
        )

    def test_only_what_is_named_a_voice_message_is_one_top_first(self):
        low = self.node("Voice message, 1:02", 900)
        high = self.node("Voice message, 0:02", 100)
        other = self.node("File a, 1 B", 0)
        old = self.node("Voice message, 0:02, which cannot be played here", 50)
        self.assertEqual(walk.voices_on([low, other, high, old]), [high, low])

    def test_a_button_inside_its_player(self):
        player = self.node("Voice message, 0:03", 100)
        button = walk.Node("", "Play", "", "android.widget.ImageButton", (10, 110, 50, 150))
        elsewhere = walk.Node("", "Play", "", "android.widget.ImageButton", (10, 510, 50, 550))
        self.assertTrue(walk.inside(button, player))
        self.assertFalse(walk.inside(elsewhere, player))

    def test_the_fixture_is_sixteen_bit_speech_of_its_length(self):
        pcm = walk.speech_pcm(16_000, 500)
        self.assertEqual(len(pcm), 16_000)
        self.assertTrue(any(b != 0 for b in pcm[2000:4000]), "it sounds")


class Recording(unittest.TestCase):
    """The recording bar's reading (1276): its parts by phase, the timer,
    the dot, the silence line, RecordingBar's log lines."""

    def bar(self, phase, **say):
        def part(rid, text="", desc="", selected=False):
            pkg = "network.leviculum.loquax.debug:id/"
            return walk.Node(text, desc, pkg + rid, "", (0, 0, 1, 1), selected=selected)

        timer = say.get("timer", "REC 00:07")
        common = [
            part("rec_bar", desc=timer),
            part("rec_dot", selected=say.get("pulsing", phase != "paused")),
            part("rec_timer", text=timer),
            part("rec_wave", desc="Sound level"),
        ]
        if phase == "held":
            return common + [
                part("rec_cancel", text=say.get("hint", walk.REC_SLIDE)),
                part("rec_held", desc=say.get("held", walk.REC_HELD)),
                part("rec_lock", desc=say.get("lock", walk.REC_LOCK), selected=say.get("closed", False)),
            ]
        buttons = {
            "locked": [("rec_discard", "Discard recording"), ("rec_pause", "Pause recording")],
            "paused": [
                ("rec_discard", "Discard recording"),
                ("rec_play", "Listen"),
                ("rec_resume", "Continue recording"),
            ],
        }[phase]
        out = common + [part(rid, desc=desc) for rid, desc in buttons]
        out.append(part("rec_send", desc=say.get("send", "Send voice message")))
        if phase == "locked":
            out.append(part("rec_lock", desc=say.get("lock", walk.REC_LOCKED), selected=say.get("closed", True)))
        return out

    def test_each_phase_as_drawn_passes(self):
        for phase in ("held", "locked", "paused"):
            self.assertEqual(walk.rec_bar_problems(self.bar(phase), phase), [], phase)
        self.assertEqual(walk.rec_bar_problems(self.bar("held", hint=walk.REC_RELEASE), "held"), [])

    def test_a_missing_part_one_out_of_place_and_wrong_words_are_named(self):
        held = [n for n in self.bar("held") if not n.is_id("rec_wave")] + [node("compose")]
        self.assertEqual(
            walk.rec_bar_problems(held, "held"), ["no rec_wave", "compose on screen while held"]
        )
        self.assertEqual(
            walk.rec_bar_problems(self.bar("held", timer="0:07"), "held"), ["the timer says '0:07'"]
        )
        self.assertEqual(
            walk.rec_bar_problems(self.bar("held", closed=True), "held"),
            [f"the chip says {walk.REC_LOCK!r}, closed=True, want open"],
        )
        self.assertEqual(
            walk.rec_bar_problems(self.bar("locked", lock=walk.REC_LOCK, closed=False), "locked"),
            [f"the chip says {walk.REC_LOCK!r}, closed=False, want closed"],
        )
        self.assertEqual(
            walk.rec_bar_problems(self.bar("paused", send=""), "paused"),
            ["rec_send has no words for a screen reader"],
        )
        self.assertIn("rec_held on screen while locked", walk.rec_bar_problems(self.bar("locked") + self.bar("held")[-2:-1], "locked"))

    def test_the_timer_and_the_dot_are_read(self):
        self.assertEqual(walk.rec_seconds(self.bar("held", timer="REC 01:12")), 72)
        self.assertIsNone(walk.rec_seconds([node("compose")]))
        self.assertTrue(walk.rec_pulsing(self.bar("held")))
        self.assertFalse(walk.rec_pulsing(self.bar("paused")))
        self.assertIsNone(walk.rec_pulsing([node("compose")]))

    def test_the_silence_line_with_its_button_and_a_steady_dot(self):
        line = [
            walk.Node(walk.REC_SILENT, "", "x:id/rec_notice", "", (0, 0, 1, 1)),
            walk.Node(walk.REC_ALLOW, "", "x:id/rec_allow", "", (0, 0, 1, 1)),
        ]
        self.assertEqual(walk.rec_notice_problems(self.bar("locked", pulsing=False) + line), [])
        self.assertEqual(
            walk.rec_notice_problems(self.bar("locked") + line[:1]),
            [f"no {walk.REC_ALLOW!r} button: []", "the dot pulses=True, want False"],
        )
        self.assertEqual(
            walk.rec_notice_problems(self.bar("locked", pulsing=False) + line[1:]),
            [f"the line says [], want [{walk.REC_SILENT!r}]"],
        )

    def test_the_dump_says_what_is_selected(self):
        xml = (
            "<hierarchy><node text=\"\" resource-id=\"p:id/rec_dot\" selected=\"true\" "
            "bounds=\"[0,0][1,1]\" /><node text=\"\" resource-id=\"p:id/rec_lock\" "
            "selected=\"false\" bounds=\"[0,0][1,1]\" /></hierarchy>"
        )
        dot, chip = walk.parse_dump(xml)
        self.assertTrue(dot.selected)
        self.assertFalse(chip.selected)

    LOGCAT = (
        "11-01 17:00:00.100  4242  4242 I loquax  : REC_BAR phase=held pulsing=true locked=false "
        "armed=false buttons= notice=\n"
        "11-01 17:00:01.200  4242  4242 I loquax  : PHOTO_ZOOM scale=1.0 left=0 top=0 width=1 height=1 view=1x1\n"
        "11-01 17:00:02.300  4242  4242 I loquax  : REC_BAR phase=held pulsing=true locked=false "
        "armed=true buttons= notice=\n"
        "11-01 17:00:03.000  4242  4242 I loquax  : REC_BAR phase=cancelled pulsing=false "
        "locked=false armed=false buttons= notice=\n"
        "11-01 17:00:09.000  4242  4242 I loquax  : REC_BAR phase=locked pulsing=false "
        "locked=true armed=false buttons=rec_discard,rec_pause,rec_send "
        "notice=Nothing heard. Is the microphone allowed?\r\n"
    )

    def test_the_bars_log_lines_are_read_in_order(self):
        lines = walk.parse_rec_lines(self.LOGCAT)
        self.assertEqual([l["phase"] for l in lines], ["held", "held", "cancelled", "locked"])
        self.assertEqual([l["armed"] for l in lines], [False, True, False, False])
        self.assertEqual(lines[3]["buttons"], ["rec_discard", "rec_pause", "rec_send"])
        self.assertEqual(lines[0]["buttons"], [])
        self.assertEqual(lines[3]["notice"], walk.REC_SILENT)
        self.assertTrue(lines[3]["locked"])

    def test_the_phases_gone_through_in_order(self):
        lines = walk.parse_rec_lines(self.LOGCAT)
        self.assertEqual(walk.rec_went(lines, "held", "armed", "cancelled"), [])
        self.assertEqual(
            walk.rec_went(lines[:1] + lines[2:3], "held", "armed", "cancelled"),
            ["no 'armed' after ['held'] in ['held', 'cancelled']"],
        )
        self.assertEqual(
            walk.rec_went(lines, "cancelled", "held"),
            ["no 'held' after ['cancelled'] in ['held', 'held', 'cancelled', 'locked']"],
        )

    def test_the_four_steps_follow_the_voice_step(self):
        at = walk.STEPS.index("voice")
        self.assertEqual(
            walk.STEPS[at + 1 : at + 5],
            ["voice_hold_release", "voice_slide_cancel", "voice_lock_send", "voice_silence_hint"],
        )


class Draft(unittest.TestCase):
    """Where the draft step finds a row's last line (1219), and what it
    refuses."""

    def test_the_last_line_is_the_one_beside_the_rows_avatar(self):
        avatar = boxed("avatar", (42, 300, 189, 447))
        mine = boxed("last", (210, 390, 800, 430), "")
        other = boxed("last", (210, 600, 800, 640), "")
        self.assertEqual(walk.last_in_row([other, mine], avatar), mine)
        self.assertIsNone(walk.last_in_row([other], avatar))

    def test_a_row_without_the_draft_is_refused(self):
        row = walk.Node(text="Draft: walk draft 7", desc="", rid="x:id/last", cls="", bounds=(0, 0, 1, 1))
        self.assertEqual(walk.draft_problems(row, "walk draft 7"), [])
        last = walk.Node(text="walk draft 7", desc="", rid="x:id/last", cls="", bounds=(0, 0, 1, 1))
        self.assertEqual(len(walk.draft_problems(last, "walk draft 7")), 1)
        self.assertEqual(walk.draft_problems(None, "x"), ["no last line in the row"])


class Pin(unittest.TestCase):
    """Where the receive_report and position_pin steps find a row's map pin
    (1224), and what they refuse."""

    def test_the_density_is_the_override_when_set(self):
        self.assertEqual(walk.parse_density("Physical density: 420\n"), 420)
        self.assertEqual(walk.parse_density("Physical density: 420\nOverride density: 480\n"), 480)
        self.assertIsNone(walk.parse_density(""))

    def test_the_pin_is_the_one_beside_the_rows_avatar(self):
        avatar = boxed("avatar", (42, 300, 189, 447))
        mine = boxed("pin", (900, 380, 1026, 506), "Position of Anna, now")
        other = boxed("pin", (900, 590, 1026, 716), "Position of Bo, now")
        self.assertEqual(walk.pin_in_row([other, mine], avatar), mine)
        self.assertIsNone(walk.pin_in_row([other], avatar))

    def test_a_pin_under_48dp_or_without_its_label_is_refused(self):
        good = boxed("pin", (900, 380, 1026, 506), "Position of Anna, now")
        self.assertEqual(walk.pin_problems(good, 420), [])
        small = boxed("pin", (900, 380, 947, 427), "Position of Anna, now")
        self.assertEqual(len(walk.pin_problems(small, 420)), 1)
        bare = boxed("pin", (900, 380, 1026, 506))
        self.assertEqual(len(walk.pin_problems(bare, 420)), 1)
        self.assertEqual(walk.pin_problems(None, 420), ["no map pin in the row"])


class Ring(unittest.TestCase):
    """Where the receive_report step looks for the avatar's ring (1209)."""

    def test_the_points_lie_on_the_rings_middle_inside_the_box(self):
        box = (100, 200, 200, 300)
        points = walk.ring_points(box)
        self.assertEqual(len(points), 16)
        self.assertEqual(points[0], (197, 250))
        for x, y in points:
            self.assertTrue(100 <= x <= 200 and 200 <= y <= 300, (x, y))
            self.assertAlmostEqual(((x - 150) ** 2 + (y - 250) ** 2) ** 0.5, 47, delta=1)

    def test_a_ring_drawn_into_a_screen_is_seen_changed(self):
        side = 100
        plain = filled(side, side, (255, 255, 255))
        ringed = list(plain)
        for x, y in walk.ring_points((0, 0, side, side)):
            ringed[y * side + x] = (0, 90, 200)
        a = walk.parse_screencap(screencap(side, side, plain))
        b = walk.parse_screencap(screencap(side, side, ringed))
        points = walk.ring_points((0, 0, side, side))
        before = [a.pixel(x, y) for x, y in points]
        after = [b.pixel(x, y) for x, y in points]
        self.assertEqual(walk.changed_fraction(before, after), 1.0)


def row_at(rid, top, text=""):
    """A node `rid` at `top`, as the pencil's rows stand."""
    pkg = "network.leviculum.loquax.debug:id/"
    return walk.Node(text=text, desc="", rid=pkg + rid, cls="", bounds=(0, top, 100, top + 40))


ADDRESS = "0c749148" * 4
CODE = f"lxma://{ADDRESS}:" + "ab" * 64


class FirstStartQr(unittest.TestCase):
    """1263, finding 1: the first start's QR is the profile's code."""

    def test_zbar_lines(self):
        self.assertEqual(walk.qr_texts(f"{CODE}\n\n"), [CODE])
        self.assertEqual(walk.qr_texts(""), [])

    def test_the_same_contact_code_passes(self):
        self.assertEqual(walk.first_start_qr_problems([CODE], [CODE], ADDRESS), [])
        self.assertEqual(walk.first_start_qr_problems([CODE], [CODE], None), [])

    def test_the_old_bare_address_fails(self):
        problems = walk.first_start_qr_problems([ADDRESS.upper()], [CODE], ADDRESS)
        self.assertTrue(any("no contact code" in p for p in problems), problems)
        self.assertTrue(any("is not the profile's" in p for p in problems), problems)

    def test_none_or_two_codes_or_another_address_fail(self):
        self.assertTrue(walk.first_start_qr_problems([], [CODE], ADDRESS))
        self.assertTrue(walk.first_start_qr_problems([CODE, CODE], [CODE], ADDRESS))
        other = CODE.replace(ADDRESS, "1" * 32)
        problems = walk.first_start_qr_problems([other], [other], ADDRESS)
        self.assertTrue(any("not of" in p for p in problems), problems)


class Pencil(unittest.TestCase):
    """1263, finding 3: scan first, paste second, typing folded away."""

    def test_scan_above_paste_passes(self):
        nodes = [row_at("new_scan", 200, "Scan a code"), row_at("new_paste", 300, "Paste a code"),
                 row_at("new_advanced", 500, "Advanced  ▾")]
        self.assertEqual(walk.pencil_problems(nodes, camera=True), [])

    def test_no_camera_needs_no_scan(self):
        nodes = [row_at("new_paste", 300), row_at("new_advanced", 500)]
        self.assertEqual(walk.pencil_problems(nodes, camera=False), [])
        self.assertEqual(walk.pencil_problems(nodes, camera=True), ["a camera and no Scan a code"])

    def test_the_old_screen_fails(self):
        nodes = [
            row_at("address", 200),
            row_at("x", 100, "The LXMF address to write to: 32 hex digits, as the other side's app shows it."),
            row_at("new_paste", 100), row_at("new_scan", 300), row_at("new_advanced", 500),
        ]
        problems = walk.pencil_problems(nodes, camera=True)
        self.assertIn("Scan a code is not above Paste a code", problems)
        self.assertIn("the address field shows before Advanced is opened", problems)
        self.assertTrue(any("'LXMF'" in p for p in problems), problems)


class JoinErrorWords(unittest.TestCase):
    """1263, finding 8: one sentence, Retry, Details."""

    SAID = "That code is not a mailbox code."

    def test_the_plain_sentence_passes(self):
        nodes = [row_at("error", 400, self.SAID), row_at("join_retry", 450), row_at("join_details_toggle", 500)]
        self.assertEqual(walk.join_error_problems(nodes, self.SAID), [])

    def test_the_daemon_sentence_fails(self):
        old = ("Your mailbox did not answer through 10.0.0.1:4242. Check the address, "
               "that its lnsd runs a TCP server there.")
        problems = walk.join_error_problems([row_at("error", 400, old)], self.SAID)
        self.assertIn("no Retry", problems)
        self.assertIn("no Details", problems)
        self.assertIn("the sentence says 'lnsd'", problems)
        self.assertIn("the sentence says 'TCP'", problems)

    def test_the_steps_stand_where_they_can_run(self):
        steps = walk.STEPS
        self.assertLess(steps.index("join_error_words"), steps.index("first_start"))
        self.assertGreater(steps.index("first_start_qr"), steps.index("first_start"))
        self.assertGreater(steps.index("pencil_scan"), steps.index("first_start"))
