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

    def test_mean_luma(self):
        self.assertAlmostEqual(walk.mean_luma([(255, 255, 255)]), 255.0)
        self.assertAlmostEqual(walk.mean_luma([(0, 0, 0), (255, 255, 255)]), 127.5)
        self.assertEqual(walk.mean_luma([]), 0.0)


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
