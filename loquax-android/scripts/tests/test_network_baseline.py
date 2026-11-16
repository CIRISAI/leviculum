"""The network baseline's lines and table (1298): what
`scripts/network-baseline.py` writes for each change, and the report's
table made from it. Run with the walk's tests by
`loquax-android/tests/device_walk_script.rs`."""

import importlib.util
import sys
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "network-baseline.py"
_spec = importlib.util.spec_from_file_location("network_baseline", SCRIPT)
base = importlib.util.module_from_spec(_spec)
sys.modules["network_baseline"] = base
_spec.loader.exec_module(base)


class Lines(unittest.TestCase):
    def test_only_what_came_new_is_shown_once_in_order(self):
        before = {"Chats", "Anna"}
        seen = ["Chats", "Reconnecting…", "Anna", "Offline", "Reconnecting…", " "]
        self.assertEqual(base.new_texts(before, seen), ["Reconnecting…", "Offline"])

    def test_a_line_reads_back_into_the_table(self):
        measured = base.Measured(
            app="network.columba.app",
            change="wifi_toggle",
            ok=True,
            off_s=20,
            kinds=("wifi", "none", "wifi"),
            back_ms=41250,
            tries=3,
            shown=["No network", "Connecting"],
        )
        line = base.net_line(measured)
        self.assertEqual(
            line,
            "NET app=network.columba.app change=wifi_toggle ok=1 off_s=20 kinds=wifi>none>wifi "
            "back_ms=41250 tries=3 shown=No_network|Connecting detail=-",
        )
        self.assertEqual(base.parse_net_line(line)["back_ms"], "41250")
        self.assertIsNone(base.parse_net_line("WALK step=x ok=1"))
        skipped = base.net_line(
            base.Measured(app="a", change="other_wlan", ok=False, detail="skipped: no --other-ssid")
        )
        rows = base.table([line, skipped, "something else"])
        self.assertEqual(len(rows), 4, rows)
        self.assertEqual(
            rows[2],
            "| wifi_toggle | network.columba.app | 20 | wifi>none>wifi | 41250 | 3 | No network|Connecting |",
        )
        self.assertIn("not back (skipped: no --other-ssid)", rows[3])

    def test_the_five_changes_of_the_order(self):
        self.assertEqual(
            base.CHANGES, ["wifi_toggle", "mobile_switch", "flight_mode", "other_wlan", "sleep"]
        )


if __name__ == "__main__":
    unittest.main()
