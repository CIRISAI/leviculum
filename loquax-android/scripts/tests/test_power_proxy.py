"""The power proxy's parsers and arithmetic (1177): `ss -tin`, /proc stat
and task status, ps and pm, dumpsys cpuinfo, power and batterystats
--checkin, the radio model and the POWER line. Run by
`loquax-android/tests/device_walk_script.rs` under `cargo test -p
loquax-android`, or alone:

    python3 -I -m unittest discover -s loquax-android/scripts/tests
"""

import importlib.util
import sys
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]
_spec = importlib.util.spec_from_file_location("power_proxy", SCRIPTS / "power-proxy.py")
power = importlib.util.module_from_spec(_spec)
sys.modules["power_proxy"] = power
_spec.loader.exec_module(power)
walk = sys.modules.get("device_walk") or power.load_walk()

PKG = "network.leviculum.loquax.debug"

# `ss -tinH state established '( sport = :4242 )'` as iproute2 6.x writes it:
# the state column dropped by the filter, the info line tab indented. The
# second connection is another host's, the third the device over v4-mapped v6.
SS = """\
0      0      192.168.240.1:4242      192.168.240.112:39874
\t cubic wscale:7,7 rto:204 rtt:0.512/0.25 ato:40 mss:1448 pmtu:1500 rcvmss:536 advmss:1448 cwnd:10 bytes_sent:1520 bytes_acked:1521 bytes_received:3044 segs_out:40 segs_in:61 data_segs_out:12 data_segs_in:30 send 226Mbps lastsnd:2044 lastrcv:1044 lastack:1044 pacing_rate 452Mbps delivery_rate 52Mbps delivered:13 app_limited busy:12ms retrans:0/2 rcv_space:14480 minrtt:0.1 snd_wnd:64256
0      0      192.168.240.1:4242      10.0.0.7:50000
\t cubic rtt:9/4 bytes_acked:1 bytes_received:1 segs_out:1 segs_in:1 lastsnd:10 lastrcv:10
0      0      [::ffff:192.168.240.1]:4242      [::ffff:192.168.240.112]:40000
\t cubic rtt:1/1 bytes_acked:5 bytes_received:6 segs_out:7 segs_in:8 lastsnd:300 lastrcv:400
"""

# Without -H and the state filter: a header and the state column.
SS_FULL = """\
State Recv-Q Send-Q Local Address:Port  Peer Address:Port Process
ESTAB 0      0      192.168.240.1:4242 192.168.240.112:39874 users:(("lnsd",pid=77,fd=9))
\t cubic bytes_acked:9 bytes_received:10 segs_out:11 segs_in:12
"""

STAT = (
    "1234 (loquax) (x) S 300 300 0 0 -1 1077952832 12000 0 30 0 523 211 0 0 20 0 45 0 98765 "
    "1000 200 18446744073709551615 0 0 0 0 0 0 0 4612 0 0 0 0 17 3 0 0 0 0 0\n"
)

TASKS = """\
Name:\tloquax.debug
Umask:\t0077
State:\tS (sleeping)
Tgid:\t1234
Pid:\t1234
voluntary_ctxt_switches:\t500
nonvoluntary_ctxt_switches:\t20
Name:\tRenderThread
State:\tS (sleeping)
Tgid:\t1234
Pid:\t1250
voluntary_ctxt_switches:\t70
nonvoluntary_ctxt_switches:\t3
"""

PS = f"""\
  PID NAME
    1 init
 1234 {PKG}
 1240 {PKG}:service
 1300 {PKG}.other
"""

PM = f"""\
package:{PKG}.extra uid:10200
package:{PKG} uid:10123
"""

CPUINFO = f"""\
Load: 1.2 / 1.0 / 0.9
CPU usage from 61234ms to 1234ms ago (2026-10-31 10:00:00.000 to 2026-10-31 10:01:00.000):
  12% 456/system_server: 8% user + 4% kernel / faults: 100 minor
  1.5% 1234/{PKG}: 1% user + 0.5% kernel / faults: 3 minor
  0.2% 1240/{PKG}:service: 0.1% user + 0.1% kernel
  0.3% 1300/{PKG}.other: 0.2% user + 0.1% kernel
3.1% TOTAL: 2% user + 1.1% kernel
"""

POWER_DUMP = f"""\
POWER MANAGER (dumpsys power)

Wake Locks: size=3
  PARTIAL_WAKE_LOCK              'NetworkStats' ACQ=-2s5ms LONG (uid=1000 pid=500)
  PARTIAL_WAKE_LOCK              '*job*/{PKG}/.Sync' ACQ=-1s (uid=10123 pid=1234 ws=WorkSource{{10123}})
  SCREEN_BRIGHT_WAKE_LOCK        'WindowManager' ON_AFTER_RELEASE ACQ=-9m (uid=1000 pid=500)

Suspend Blockers: size=4
  PowerManagerService.WakeLocks: ref count=1
"""

CHECKIN = """\
9,0,i,vers,36,214,UP1A,UP1A
9,10123,l,wl,LoquaxSync,0,f,0,5230,p,4,0,w,0
9,10123,l,wl,Other p tag,0,f,0,770,p,1,0,w,0
9,10123,l,cpu,820,410,0
9,10124,l,wl,Foreign,0,f,0,99,p,1,0,w,0
9,10124,l,cpu,1,1,0
"""


class SsTest(unittest.TestCase):
    def test_the_counters_of_each_connection(self):
        conns = power.parse_ss(SS)
        self.assertEqual(len(conns), 3)
        c = conns[0]
        self.assertEqual(c.key, ("192.168.240.1:4242", "192.168.240.112:39874"))
        self.assertEqual(c.get("segs_in"), 61)
        self.assertEqual(c.get("segs_out"), 40)
        self.assertEqual(c.get("bytes_received"), 3044)
        self.assertEqual(c.get("bytes_acked"), 1521)
        self.assertEqual(c.get("lastsnd"), 2044)
        self.assertEqual(c.get("retrans"), 2)
        self.assertEqual(c.counters["rtt"], "0.512/0.25")

    def test_only_the_devices_connections_on_the_port(self):
        conns = power.device_conns(power.parse_ss(SS), 4242, "192.168.240.112")
        self.assertEqual([c.get("segs_in") for c in conns], [61, 8])
        self.assertEqual(power.device_conns(power.parse_ss(SS), 4243, None), [])
        self.assertEqual(len(power.device_conns(power.parse_ss(SS), 4242, None)), 3)

    def test_a_header_and_a_state_column_and_a_process(self):
        conns = power.parse_ss(SS_FULL)
        self.assertEqual(len(conns), 1)
        self.assertEqual(conns[0].peer, "192.168.240.112:39874")
        self.assertEqual(conns[0].get("segs_in"), 12)

    def test_nothing_established_is_nothing(self):
        self.assertEqual(power.parse_ss(""), [])

    def test_the_host_of_an_address(self):
        self.assertEqual(power.host_of("[::ffff:10.1.2.3]:4"), "10.1.2.3")
        self.assertEqual(power.host_of("[fe80::1]:4"), "fe80::1")
        self.assertEqual(power.host_of("10.1.2.3:4"), "10.1.2.3")


class ProcTest(unittest.TestCase):
    def test_stat_counts_from_the_last_parenthesis(self):
        st = power.parse_proc_stat(STAT)
        self.assertEqual((st.pid, st.comm), (1234, "loquax) (x"))
        self.assertEqual((st.utime, st.stime, st.starttime), (523, 211, 98765))

    def test_a_stat_that_is_not_one(self):
        with self.assertRaises(ValueError):
            power.parse_proc_stat("cat: /proc/9/stat: No such file or directory")

    def test_switches_per_thread(self):
        self.assertEqual(power.parse_task_status(TASKS), {1234: (500, 20), 1250: (70, 3)})
        self.assertEqual(power.parse_task_status("Permission denied"), {})

    def test_the_packages_processes_and_not_its_namesakes(self):
        self.assertEqual(power.parse_ps(PS, PKG), [1234, 1240])

    def test_the_exact_packages_uid(self):
        self.assertEqual(power.parse_pm_uid(PM, PKG), 10123)
        self.assertIsNone(power.parse_pm_uid("", PKG))


class DumpsysTest(unittest.TestCase):
    def test_cpuinfo_sums_the_packages_processes_over_its_window(self):
        info = power.parse_cpuinfo(CPUINFO, PKG)
        self.assertEqual(info["processes"], 2)
        self.assertAlmostEqual(info["pct"], 1.7)
        self.assertAlmostEqual(info["window_s"], 60.0)
        self.assertEqual(power.parse_cpuinfo("", PKG)["processes"], 0)

    def test_the_wakelocks_held_now(self):
        locks = power.parse_power_wakelocks(POWER_DUMP)
        self.assertEqual(len(locks), 3)
        self.assertEqual(
            [l for l in locks if l["uid"] == 10123],
            [{"type": "PARTIAL_WAKE_LOCK", "tag": f"*job*/{PKG}/.Sync", "uid": 10123}],
        )

    def test_no_wakelock_section_is_none_and_an_empty_one_is_empty(self):
        self.assertIsNone(power.parse_power_wakelocks("Can't find service: power"))
        self.assertEqual(power.parse_power_wakelocks("Wake Locks: size=0\n\nSuspend"), [])

    def test_checkin_sums_the_uids_partial_wakelocks_and_cpu(self):
        stats = power.parse_checkin(CHECKIN, 10123)
        self.assertEqual(stats["rows"], 3)
        self.assertEqual((stats["partial_wl_ms"], stats["partial_wl_count"]), (6000, 5))
        self.assertEqual((stats["cpu_user_ms"], stats["cpu_sys_ms"]), (820, 410))

    def test_checkin_without_the_uid_says_so(self):
        self.assertEqual(power.parse_checkin(CHECKIN, 99999)["rows"], 0)


class ArithmeticTest(unittest.TestCase):
    def test_the_accumulator_counts_new_keys_from_zero_and_survives_a_reset(self):
        acc = power.Accumulator()
        acc.observe({"a": 100})
        acc.observe({"a": 130, "b": 5})
        self.assertEqual(acc.total(), 35)
        # `a` restarts (a pid reused, a counter reset): its 30 stay counted.
        acc.observe({"a": 4, "b": 9})
        self.assertEqual(acc.total(), 30 + 4 + 9)
        self.assertEqual(acc.keys(), 2)

    def test_the_radio_stays_on_for_the_gap_or_the_tail(self):
        # Packets at 0, 3 and 50 in a window to 55: 3 + 10 + 5.
        self.assertEqual(power.radio_on_seconds([3, 0, 50], 0, 55, 10), 18)
        # Every 5 s, below a 10 s tail: on all the time.
        self.assertEqual(power.radio_on_seconds(list(range(0, 60, 5)), 0, 60, 10), 60)
        # Every 30 s: 10 s of each 30.
        self.assertEqual(power.radio_on_seconds([0, 30, 60, 90], 0, 120, 10), 40)
        self.assertEqual(power.radio_on_seconds([-5, 200], 0, 100, 10), 0)

    def test_the_timeline_takes_the_last_packets_time_or_the_ticks_middle(self):
        before = {k: c for k, c in ((c.key, c) for c in power.parse_ss(SS))}
        grown = power.parse_ss(SS.replace("segs_out:40", "segs_out:42").replace("lastsnd:2044", "lastsnd:250"))
        now = {c.key: c for c in grown}
        self.assertEqual(power.timeline_events(before, now, 100.0, 1.0), [99.75])
        # segs_in grew but lastrcv says 1044 ms ago, older than the tick.
        grown = power.parse_ss(SS.replace("segs_in:61", "segs_in:62"))
        now = {c.key: c for c in grown}
        self.assertEqual(power.timeline_events(before, now, 100.0, 1.0), [99.5])
        self.assertEqual(power.timeline_events(before, before, 100.0, 1.0), [])


class LineTest(unittest.TestCase):
    def test_a_phase_line_round_trips(self):
        line = power.power_line(
            "Bprime",
            "idle",
            True,
            {"hours": 2.0, "segs_in_h": 1440.0, "cpu_src": "proc", "wakelock_held_frac": None,
             "levers": ["server_access_point"], "sends": "0/0"},
        )
        self.assertEqual(
            line,
            "POWER mode=Bprime phase=idle ok=1 hours=2.000 segs_in_h=1440.0 cpu_src=proc "
            "wakelock_held_frac=na sends=0/0 levers=server_access_point",
        )
        parsed = power.parse_power_line(line)
        self.assertEqual(parsed["mode"], "Bprime")
        self.assertTrue(parsed["ok"])
        self.assertEqual(parsed["segs_in_h"], "1440.0")

    def test_a_failed_mode_carries_its_detail(self):
        line = power.power_line("A", "setup", False, {}, "no AutoInterface\n mode")
        self.assertEqual(line, "POWER mode=A phase=setup ok=0 detail=no AutoInterface mode")
        self.assertEqual(power.parse_power_line(line)["detail"], "no AutoInterface mode")
        self.assertIsNone(power.parse_power_line("WALK step=x ok=1 ms=1 detail="))

    def test_the_summary_and_the_table(self):
        self.assertEqual(power.summary_line(2, 1, 5), "POWER_SUMMARY modes=3 ok=2 failed=1 ms=5")
        rows = [
            {"mode": "B", "phase": "idle", "ok": True, "segs_in_h": 700.0, "segs_out_h": 720.0,
             "bytes_in_h": 1.0, "bytes_out_h": 2.0, "radio_on_frac": 1.0, "cpu_s_h": 3.5,
             "vcsw_h": 9000.0, "wakelock_held_frac": 0.0, "bs_partial_wl_s_h": None, "conns": 1},
            {"mode": "A", "phase": "setup", "ok": False, "detail": "no AutoInterface"},
        ]
        lines = power.table(rows).splitlines()
        self.assertEqual(len(lines), 3)
        self.assertTrue(lines[0].startswith("mode"))
        self.assertIn("1420.0", lines[1])
        self.assertIn("na", lines[1])
        self.assertIn("failed: no AutoInterface", lines[2])


class FixtureConfigTest(unittest.TestCase):
    def test_the_levers_reach_the_config_and_stay_out_by_default(self):
        plain = walk.lnsd_config("w", "0.0.0.0", 4242)
        self.assertNotIn("mode =", plain)
        self.assertNotIn("keepalive_interval", plain)
        levered = walk.lnsd_config("w", "0.0.0.0", 4242, "access_point", 600)
        self.assertIn("\n  keepalive_interval = 600\n", levered)
        self.assertTrue(levered.endswith("    listen_port = 4242\n    mode = access_point\n"))
        # The keepalive belongs to [reticulum], above [logging].
        self.assertLess(levered.index("keepalive_interval"), levered.index("[logging]"))


if __name__ == "__main__":
    unittest.main()
