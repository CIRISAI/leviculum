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
