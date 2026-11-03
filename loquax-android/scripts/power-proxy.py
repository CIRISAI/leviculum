#!/usr/bin/env python3
"""What Loquax costs a phone, tier P: proxies in Waydroid (1177, Refs #232).

The research note of 2026-10-31 (docs/src/concepts/loquax-phone-power.md)
ranks where a phone's battery goes while Loquax waits for mail. Waydroid
on hamster has no battery and never suspends, so this script measures
what stands for the battery: what goes over the app's TCP connection and
when, how long a modem would stay on for that, what CPU the app burns and
how often it wakes, and which wakelocks it holds. Tier R, a real phone at
the bench, gives the real mAh.

Per mode, one after the other on the same host clock, each on a fresh
fixture of the device walk (scripts/device-walk.py: lnsd with a TCP
server, the mailbox over the ugly store, the APK installed and joined):

    B       today's defaults
    Bprime  the levers the host can pull today: the TCP server in
            access_point mode, a long keepalive_interval for the host's
            links (--bprime-link-keepalive); the line says which levers
            of the note are still missing
    A       a standalone node, AutoInterface only, no mailbox
    ref     the reference app (--ref-apk, --ref-package), connected to
            the same TCP server

each mode settles, then idles (--idle, 2 h), then gets one message every
--message-every seconds from the host for --traffic (1 h). Every --sample
seconds (30) it reads the app's TCP connection with `ss -tin` on this
host, the app's processes through `adb shell` (/proc, else dumpsys
cpuinfo) and `dumpsys power`; every --tick seconds (1) it reads `ss`
alone for the packet timeline the modelled radio-on time comes from.
Each phase ends in one line, to stdout and OUT/power.log:

    POWER mode=<m> phase=<idle|traffic> ok=1 hours=<h> segs_in_h=<n> ...

a mode that could not run in

    POWER mode=<m> phase=<setup|idle|traffic> ok=0 detail=<why>

and the run in a table and

    POWER_SUMMARY modes=<n> ok=<n> failed=<n> ms=<total>

OUT/power.json holds every phase's numbers, OUT/<mode>/samples.jsonl the
raw samples. The exit code is 1 when a mode failed to run, never because
of a number. Usage, on hamster with Waydroid's session up:

    cargo build -p loquax --features tui --bin loquax
    cargo build -p leviculum-cli --bin lnsd
    cargo build -p loquax-android --example walk_mailbox
    (cd loquax-android && make waydroid)
    python3 -I loquax-android/scripts/power-proxy.py \\
        --serial 192.168.240.112:5555 --server 192.168.240.1:4242 \\
        --walk-mailbox target/debug/examples/walk_mailbox \\
        --modes B,Bprime,ref --ref-apk ~/apk/columba.apk \\
        --ref-package <package> --ref-address <its LXMF address> \\
        --out ~/scratch/power

Standard library only, Python 3.11+.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
MODES = ["B", "Bprime", "A", "ref"]
# The note's levers (section 1) and whether this tree has a knob for them.
BPRIME_LEVERS = ["server_access_point", "host_link_keepalive"]
# Both former gaps are defaults since 1346: the device's TCP idle and its
# link keepalive (docs/src/concepts/loquax-phone-power.md, "The device link's
# keepalives").
BPRIME_MISSING: list[str] = []


def load_walk():
    """scripts/device-walk.py as a module: its device, fixture and steps."""
    spec = importlib.util.spec_from_file_location("device_walk", HERE / "device-walk.py")
    module = importlib.util.module_from_spec(spec)
    # Registered before it runs: its dataclasses look their module up there.
    sys.modules["device_walk"] = module
    spec.loader.exec_module(module)
    return module


# --- Parsers: what the unit tests under scripts/tests/ pin. ---


@dataclass(frozen=True)
class Conn:
    """One TCP connection as `ss -tin` shows it."""

    local: str
    peer: str
    counters: dict

    @property
    def key(self) -> tuple[str, str]:
        return (self.local, self.peer)

    def get(self, name: str) -> int:
        return int(self.counters.get(name, 0))


SS_INT_KEYS = {
    "bytes_sent",
    "bytes_acked",
    "bytes_received",
    "bytes_retrans",
    "segs_out",
    "segs_in",
    "data_segs_out",
    "data_segs_in",
    "lastsnd",
    "lastrcv",
    "lastack",
    "retrans",
}


def parse_ss(text: str) -> list[Conn]:
    """`ss -tin` (with or without -H, with or without a state filter): a
    line with the addresses, then an indented line of `key:value` tokens.

    The address line is the last two `host:port` columns; tokens without a
    colon (`cubic`, `sack`) and ones that are not integers (`rtt:0.1/0.05`)
    are kept as strings, `retrans:0/2` as its second number, the total."""
    conns = []
    current: tuple[str, str] | None = None
    for line in text.splitlines():
        if not line.strip():
            continue
        if line[0].isspace():
            if current is None:
                continue
            counters: dict = {}
            for token in line.split():
                key, sep, value = token.partition(":")
                if not sep:
                    continue
                if key in SS_INT_KEYS:
                    number = value.rpartition("/")[2] if key == "retrans" else value
                    try:
                        counters[key] = int(number)
                    except ValueError:
                        counters[key] = value
                else:
                    counters[key] = value
            conns.append(Conn(current[0], current[1], counters))
            current = None
            continue
        cols = line.split()
        if cols[0] in ("State", "Recv-Q", "Netid"):
            continue
        addrs = [c for c in cols if ":" in c and c.rpartition(":")[2].isdigit()]
        current = (addrs[-2], addrs[-1]) if len(addrs) >= 2 else None
    return conns


def host_of(address: str) -> str:
    """The host part of `host:port`, `[v6]:port` or `::ffff:v4:port`."""
    host = address.rpartition(":")[0].strip("[]")
    return host.removeprefix("::ffff:")


def device_conns(conns: list[Conn], port: int, device_ip: str | None) -> list[Conn]:
    """The connections to the server's `port` from the device."""
    out = []
    for c in conns:
        if c.local.rpartition(":")[2] != str(port):
            continue
        if device_ip and host_of(c.peer) != device_ip:
            continue
        out.append(c)
    return out


@dataclass(frozen=True)
class ProcStat:
    pid: int
    comm: str
    utime: int
    stime: int
    starttime: int


def parse_proc_stat(text: str) -> ProcStat:
    """`/proc/<pid>/stat`: the name in parentheses may hold spaces and
    parentheses itself, so the fields count from the last `)`."""
    open_, close = text.find("("), text.rfind(")")
    if open_ < 0 or close < open_:
        raise ValueError(f"not a stat line: {text[:80]!r}")
    pid = int(text[:open_].strip())
    rest = text[close + 1 :].split()
    # rest[0] is field 3 (state); utime 14, stime 15, starttime 22.
    return ProcStat(pid, text[open_ + 1 : close], int(rest[11]), int(rest[12]), int(rest[19]))


def parse_task_status(text: str) -> dict[int, tuple[int, int]]:
    """Concatenated `/proc/<pid>/task/*/status`: per thread id, its
    voluntary and nonvoluntary context switches."""
    out: dict[int, tuple[int, int]] = {}
    tid = None
    vol = nonvol = None
    for line in text.splitlines():
        key, _, value = line.partition(":")
        value = value.strip()
        if key == "Name":
            tid, vol, nonvol = None, None, None
        elif key == "Pid":
            tid = int(value)
        elif key == "voluntary_ctxt_switches":
            vol = int(value)
        elif key == "nonvoluntary_ctxt_switches":
            nonvol = int(value)
        if tid is not None and vol is not None and nonvol is not None:
            out[tid] = (vol, nonvol)
            tid = vol = nonvol = None
    return out


def parse_ps(text: str, package: str) -> list[int]:
    """The pids of `package`'s processes (`name` and `name:service`) in
    `ps -A -o PID,NAME`."""
    pids = []
    for line in text.splitlines():
        cols = line.split()
        if len(cols) < 2 or not cols[0].isdigit():
            continue
        name = cols[-1]
        if name == package or name.startswith(package + ":"):
            pids.append(int(cols[0]))
    return pids


def parse_pm_uid(text: str, package: str) -> int | None:
    """The uid in `pm list packages -U <package>`; pm matches substrings,
    so only the exact name counts."""
    for line in text.splitlines():
        m = re.fullmatch(r"package:(\S+)\s+uid:(\d+)(?:,\d+)*", line.strip())
        if m and m.group(1) == package:
            return int(m.group(2))
    return None


CPUINFO_WINDOW = re.compile(r"CPU usage from (-?\d+)ms to (-?\d+)ms ago")
CPUINFO_LINE = re.compile(r"^\s*([\d.]+)% (\d+)/(\S+): ([\d.]+)% user \+ ([\d.]+)% kernel")


def parse_cpuinfo(text: str, package: str) -> dict:
    """`dumpsys cpuinfo`: the window in seconds and the summed percent of
    `package`'s processes over it."""
    window = None
    m = CPUINFO_WINDOW.search(text)
    if m:
        window = abs(int(m.group(1)) - int(m.group(2))) / 1000
    pct = 0.0
    found = 0
    for line in text.splitlines():
        m = CPUINFO_LINE.match(line)
        if not m:
            continue
        name = m.group(3)
        if name == package or name.startswith(package + ":"):
            pct += float(m.group(1))
            found += 1
    return {"window_s": window, "pct": pct, "processes": found}


WAKE_LOCK = re.compile(r"^\s+(\w+_WAKE_LOCK)\s+'(.*)'.*\(uid=(\d+)")


def parse_power_wakelocks(text: str) -> list[dict] | None:
    """The `Wake Locks: size=N` section of `dumpsys power`: type, tag, uid
    of each lock held now. None when the section is missing."""
    lines = text.splitlines()
    for i, line in enumerate(lines):
        if line.strip().startswith("Wake Locks: size="):
            break
    else:
        return None
    locks = []
    for line in lines[i + 1 :]:
        if not line.strip():
            break
        m = WAKE_LOCK.match(line)
        if m:
            locks.append({"type": m.group(1), "tag": m.group(2), "uid": int(m.group(3))})
    return locks


def parse_checkin(text: str, uid: int) -> dict:
    """`dumpsys batterystats --checkin`, the rows of `uid`: the partial
    wakelock time and count (`wl` rows: name, full ms, f, count, partial
    ms, p, count, ...) and the CPU time (`cpu` rows: user ms, system ms).
    `rows` is 0 when the uid has none, which says the stats hold nothing."""
    out = {"rows": 0, "partial_wl_ms": 0, "partial_wl_count": 0, "cpu_user_ms": 0, "cpu_sys_ms": 0}
    for line in text.splitlines():
        cols = line.strip().split(",")
        if len(cols) < 4 or cols[0] != "9" or cols[1] != str(uid) or cols[2] != "l":
            continue
        out["rows"] += 1
        kind = cols[3]
        if kind == "wl":
            # The tag sits at 4; the first lone `p` after the full count marks
            # the partial pair, which sits before and after it.
            for j in range(6, len(cols) - 1):
                if cols[j] == "p" and cols[j - 1].isdigit() and cols[j + 1].isdigit():
                    out["partial_wl_ms"] += int(cols[j - 1])
                    out["partial_wl_count"] += int(cols[j + 1])
                    break
        elif kind == "cpu" and len(cols) >= 6:
            out["cpu_user_ms"] += int(cols[4])
            out["cpu_sys_ms"] += int(cols[5])
    return out


# --- Arithmetic over the samples. ---


class Accumulator:
    """A counter per key (a connection, a pid, a thread) summed over a
    phase: per key its last value less its first, or less 0 for a key that
    appeared after the phase began (a reconnect, a new process). A key
    whose counter went down is a new incarnation and starts again."""

    def __init__(self) -> None:
        self.first: dict = {}
        self.last: dict = {}
        self.carried: dict = {}
        self.started = False

    def observe(self, values: dict) -> None:
        for key, value in values.items():
            if key not in self.first:
                self.first[key] = value if not self.started else 0
            elif value < self.last[key]:
                self.carried[key] = self.carried.get(key, 0) + self.last[key] - self.first[key]
                self.first[key] = 0
            self.last[key] = value
        self.started = True

    def total(self) -> int:
        return sum(self.last[k] - self.first[k] for k in self.last) + sum(self.carried.values())

    def keys(self) -> int:
        return len(self.last)


def radio_on_seconds(events: list[float], start: float, end: float, tail: float) -> float:
    """The modelled seconds a modem stays on: after each packet it stays on
    for `tail` seconds or until the next packet, whichever comes first
    (the note's section 3, `sum min(gap, tail)`). Events outside the
    window are ignored; the last one's tail is cut at the window's end."""
    times = sorted(t for t in events if start <= t <= end)
    on = 0.0
    for a, b in zip(times, times[1:] + [end]):
        on += min(b - a, tail)
    return on


def timeline_events(previous: dict, now: dict, at: float, tick: float) -> list[float]:
    """The packet times one tick of `ss` shows, per connection key: when
    segs_out grew, at `at - lastsnd`; when segs_in grew, at `at - lastrcv`.
    A counter that grew while its `last*` says the newest packet is older
    than the tick (a keepalive probe or a pure ACK does not move lastsnd)
    counts at the tick's middle instead. Packets within one tick are one
    event; with a tail of seconds that loses at most a tick per event."""
    events = []
    for key, conn in now.items():
        before = previous.get(key)
        for segs, last in (("segs_out", "lastsnd"), ("segs_in", "lastrcv")):
            grew = conn.get(segs) > (before.get(segs) if before else 0)
            if not grew:
                continue
            ago = conn.counters.get(last)
            if isinstance(ago, int) and ago <= tick * 1000:
                events.append(at - ago / 1000)
            else:
                events.append(at - tick / 2)
    return events


def per_hour(value: float, seconds: float) -> float:
    return value * 3600 / seconds if seconds > 0 else 0.0


def clean(detail: str) -> str:
    return " ".join(str(detail).split())


def fmt_value(value) -> str:
    if isinstance(value, float):
        return f"{value:.3f}" if abs(value) < 10 else f"{value:.1f}"
    if isinstance(value, (list, tuple)):
        return ",".join(str(v) for v in value) or "none"
    if value is None:
        return "na"
    return str(value)


# The fields of a phase's line, in order; the JSON has them all.
LINE_FIELDS = [
    "hours",
    "segs_in_h",
    "segs_out_h",
    "bytes_in_h",
    "bytes_out_h",
    "conns",
    "radio_on_frac",
    "tail_s",
    "cpu_s_h",
    "cpu_src",
    "vcsw_h",
    "nvcsw_h",
    "csw_src",
    "wakelock_held_frac",
    "wakelock_src",
    "bs_partial_wl_s_h",
    "bs_cpu_s_h",
    "bs_src",
    "sends",
    "levers",
    "missing",
]


def power_line(mode: str, phase: str, ok: bool, numbers: dict, detail: str = "") -> str:
    parts = [f"POWER mode={mode} phase={phase} ok={1 if ok else 0}"]
    for name in LINE_FIELDS:
        if name in numbers:
            parts.append(f"{name}={fmt_value(numbers[name])}")
    if detail:
        parts.append(f"detail={clean(detail)}")
    return " ".join(parts)


POWER_LINE = re.compile(r"POWER mode=(\S+) phase=(\S+) ok=([01])((?: \w+=\S*)*)(?: detail=(.*))?")


def parse_power_line(line: str) -> dict | None:
    m = POWER_LINE.fullmatch(line.rstrip("\n"))
    if not m:
        return None
    out = {"mode": m.group(1), "phase": m.group(2), "ok": m.group(3) == "1"}
    for pair in m.group(4).split():
        key, _, value = pair.partition("=")
        out[key] = value
    if m.group(5) is not None:
        out["detail"] = m.group(5)
    return out


def summary_line(ok: int, failed: int, ms: int) -> str:
    return f"POWER_SUMMARY modes={ok + failed} ok={ok} failed={failed} ms={int(ms)}"


TABLE_COLUMNS = [
    ("mode", "mode"),
    ("phase", "phase"),
    ("segs/h", "segs_h"),
    ("bytes/h", "bytes_h"),
    ("radio on", "radio_on_frac"),
    ("cpu s/h", "cpu_s_h"),
    ("vcsw/h", "vcsw_h"),
    ("wakelock", "wakelock_held_frac"),
    ("bs wl s/h", "bs_partial_wl_s_h"),
    ("conns", "conns"),
]


def table(rows: list[dict]) -> str:
    """The phases side by side; a failed mode shows its detail."""
    cells = [[title for title, _ in TABLE_COLUMNS]]
    for r in rows:
        if not r.get("ok"):
            cells.append([r["mode"], r["phase"], "failed: " + clean(r.get("detail", ""))[:60]])
            continue
        values = dict(r)
        values["segs_h"] = r.get("segs_in_h", 0) + r.get("segs_out_h", 0)
        values["bytes_h"] = r.get("bytes_in_h", 0) + r.get("bytes_out_h", 0)
        cells.append([fmt_value(values.get(key)) for _, key in TABLE_COLUMNS])
    full = [row for row in cells if len(row) == len(TABLE_COLUMNS)]
    widths = [max(len(row[i]) for row in full) for i in range(len(TABLE_COLUMNS))]
    lines = []
    for row in cells:
        if len(row) < len(TABLE_COLUMNS):
            lines.append("  ".join(c.ljust(widths[i]) for i, c in enumerate(row[:2])) + "  " + row[2])
        else:
            lines.append("  ".join(c.ljust(widths[i]) for i, c in enumerate(row)).rstrip())
    return "\n".join(lines)


# --- The run. ---


class ModeFailed(Exception):
    """The mode could not run; the message says why."""


@dataclass
class Phase:
    """The counters of one phase of one mode."""

    name: str
    start: float
    end: float = 0.0
    cpu: Accumulator = field(default_factory=Accumulator)
    vcsw: Accumulator = field(default_factory=Accumulator)
    nvcsw: Accumulator = field(default_factory=Accumulator)
    bytes_in: Accumulator = field(default_factory=Accumulator)
    bytes_out: Accumulator = field(default_factory=Accumulator)
    segs_in: Accumulator = field(default_factory=Accumulator)
    segs_out: Accumulator = field(default_factory=Accumulator)
    events: list = field(default_factory=list)
    cpuinfo_pct: list = field(default_factory=list)
    wakelock_samples: int = 0
    wakelock_held: int = 0
    wakelock_tags: set = field(default_factory=set)
    sends_ok: int = 0
    sends: int = 0
    cpu_src: str = "none"
    csw_src: str = "none"
    wakelock_src: str = "none"


class Proxy:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.walk_module = load_walk()
        self.out = Path(args.out).resolve()
        self.out.mkdir(parents=True, exist_ok=True)
        self.log = (self.out / "power.log").open("w", encoding="utf-8")
        self.device = self.walk_module.Device(args.adb, args.serial)
        self.port = int(args.server.rpartition(":")[2])
        self.device_ip = args.device_ip or (
            args.serial.rpartition(":")[0] if re.match(r"\d+\.\d+\.\d+\.\d+:", args.serial) else None
        )
        self.clk_tck = 100
        self.rows: list[dict] = []
        self.failed_modes: list[str] = []
        # The mode's walk, once made, so a setup that fails stops its fixture.
        self.walk = None

    def emit(self, line: str) -> None:
        print(line, flush=True)
        self.log.write(line + "\n")
        self.log.flush()

    def note(self, text: str) -> None:
        self.log.write("  " + clean(text) + "\n")
        self.log.flush()

    # Readers.

    def ss(self) -> dict:
        proc = subprocess.run(
            ["ss", "-tinH", "state", "established", f"( sport = :{self.port} )"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        if proc.returncode != 0:
            raise ModeFailed(f"ss exit={proc.returncode}: {proc.stderr.strip()[:200]}")
        return {c.key: c for c in device_conns(parse_ss(proc.stdout), self.port, self.device_ip)}

    def pids(self, package: str) -> list[int]:
        return parse_ps(self.device.shell("ps -A -o PID,NAME", timeout=20), package)

    def read_procs(self, package: str, phase: Phase) -> None:
        """CPU ticks per process and switches per thread from /proc; when
        /proc is closed, dumpsys cpuinfo's percent."""
        cpu, vol, nonvol = {}, {}, {}
        for pid in self.pids(package):
            stat = self.device.shell(f"cat /proc/{pid}/stat", timeout=20, check=False)
            try:
                st = parse_proc_stat(stat)
            except (ValueError, IndexError):
                continue
            cpu[(pid, st.starttime)] = st.utime + st.stime
            tasks = parse_task_status(
                self.device.shell(f"cat /proc/{pid}/task/*/status", timeout=20, check=False)
            )
            for tid, (v, n) in tasks.items():
                vol[(pid, st.starttime, tid)] = v
                nonvol[(pid, st.starttime, tid)] = n
        if cpu:
            phase.cpu.observe(cpu)
            phase.cpu_src = "proc"
        else:
            info = parse_cpuinfo(self.device.shell("dumpsys cpuinfo", timeout=30, check=False), package)
            if info["processes"]:
                phase.cpuinfo_pct.append(info["pct"])
                phase.cpu_src = "cpuinfo"
        if vol:
            phase.vcsw.observe(vol)
            phase.nvcsw.observe(nonvol)
            phase.csw_src = "proc"

    def read_wakelocks(self, uid: int | None, phase: Phase) -> None:
        locks = parse_power_wakelocks(self.device.shell("dumpsys power", timeout=30, check=False))
        if locks is None:
            phase.wakelock_src = "no section"
            return
        phase.wakelock_src = "dumpsys power"
        phase.wakelock_samples += 1
        mine = [lock for lock in locks if uid is not None and lock["uid"] == uid]
        if mine:
            phase.wakelock_held += 1
            phase.wakelock_tags.update(f"{lock['type']}:{lock['tag']}" for lock in mine)

    def observe_tcp(self, phase: Phase, conns: dict) -> None:
        phase.segs_in.observe({k: c.get("segs_in") for k, c in conns.items()})
        phase.segs_out.observe({k: c.get("segs_out") for k, c in conns.items()})
        phase.bytes_in.observe({k: c.get("bytes_received") for k, c in conns.items()})
        phase.bytes_out.observe({k: c.get("bytes_acked") for k, c in conns.items()})

    # One mode.

    def run_mode(self, mode: str) -> None:
        out = self.out / mode
        out.mkdir(parents=True, exist_ok=True)
        samples = (out / "samples.jsonl").open("w", encoding="utf-8")
        self.walk = None
        phase_name = "setup"
        try:
            walk, package, address, numbers = self.setup(mode, out)
            uid = parse_pm_uid(self.device.shell(f"pm list packages -U {package}"), package)
            self.note(f"{mode}: package={package} uid={uid} pids={self.pids(package)}")
            time.sleep(self.args.settle)
            self.battery_reset()
            for phase_name, seconds, every in (
                ("idle", self.args.idle, 0),
                ("traffic", self.args.traffic, self.args.message_every),
            ):
                phase = self.measure(mode, phase_name, seconds, every, package, uid, walk, address, samples)
                row = self.phase_numbers(mode, phase, uid, numbers)
                if mode != "A" and row["conns"] == 0:
                    raise ModeFailed(f"no TCP connection from the device on :{self.port} all phase")
                self.rows.append(row)
                self.emit(power_line(mode, phase_name, True, row))
        except ModeFailed as e:
            self.fail(mode, phase_name, str(e))
        except (subprocess.SubprocessError, OSError, ValueError) as e:
            self.fail(mode, phase_name, f"{type(e).__name__}: {e}")
        except self.walk_module.StepFailed as e:
            self.fail(mode, phase_name, str(e))
        finally:
            samples.close()
            self.teardown(mode, self.walk)

    def fail(self, mode: str, phase: str, detail: str) -> None:
        self.failed_modes.append(mode)
        self.rows.append({"mode": mode, "phase": phase, "ok": False, "detail": detail})
        self.emit(power_line(mode, phase, False, {}, detail))

    def walk_args(self, out: Path, server_mode: str | None, keepalive: int | None) -> argparse.Namespace:
        a = self.args
        return argparse.Namespace(
            out=str(out),
            adb=a.adb,
            serial=a.serial,
            apk=a.apk,
            server=a.server,
            listen=a.listen,
            loquax=a.loquax,
            lnsd=a.lnsd,
            walk_mailbox=a.walk_mailbox,
            server_mode=server_mode,
            link_keepalive=keepalive,
        )

    def setup(self, mode: str, out: Path):
        """The fixture and the app for `mode`: the walk, the package to
        measure, the address the traffic phase sends to, the mode's fields."""
        self.stop_apps()
        if mode == "A":
            # The note's mode (A), section 4: the app would run its own node
            # with AutoInterface only. loquax-android builds no AutoInterface
            # and its first start offers a mailbox at home or on the phone,
            # both over TCP, so there is nothing to start yet.
            raise ModeFailed(
                "the app has no AutoInterface-only mode: loquax-android starts no "
                "AutoInterface and its first start offers only TCP mailboxes"
            )
        numbers: dict = {}
        server_mode = keepalive = None
        if mode == "Bprime":
            server_mode, keepalive = "access_point", self.args.bprime_link_keepalive
            numbers = {"levers": BPRIME_LEVERS, "missing": BPRIME_MISSING}
        walk = self.walk_module.Walk(self.walk_args(out, server_mode, keepalive))
        self.walk = walk
        steps = ["fixture"] if mode == "ref" else ["fixture", "install", "first_start"]
        for step in steps:
            if not walk.run_step(step):
                raise ModeFailed(f"walk step {step} failed: see {out}/walk.log")
        if mode == "ref":
            return (walk, self.start_ref(), self.args.ref_address, numbers)
        return (walk, self.walk_module.PACKAGE, walk.fixture.manifest["address"], numbers)

    def start_ref(self) -> str:
        """The reference app installed over its old self (its data, the
        TCP interface the reviewer set up once, stays), started, and
        connected to the server within --ref-connect-wait."""
        a = self.args
        if not a.ref_package or not a.ref_address:
            raise ModeFailed("ref needs --ref-package and --ref-address")
        if a.ref_apk:
            self.device.run("install", "-r", "-g", a.ref_apk, timeout=300)
        listed = self.device.shell(f"pm list packages {a.ref_package}")
        if f"package:{a.ref_package}" not in listed.split():
            raise ModeFailed(f"{a.ref_package} not installed; give --ref-apk")
        self.device.shell(f"monkey -p {a.ref_package} -c android.intent.category.LAUNCHER 1", check=False)
        walk = self.walk_module
        if not walk.wait_for(lambda: self.ss(), a.ref_connect_wait, every=1.0):
            raise ModeFailed(
                f"{a.ref_package} not connected to {a.server} within {a.ref_connect_wait}s: "
                "set its TCP client interface to that server once"
            )
        return a.ref_package

    def stop_apps(self) -> None:
        for package in (self.walk_module.PACKAGE, self.args.ref_package):
            if package:
                self.device.shell(f"am force-stop {package}", check=False)

    def teardown(self, mode: str, walk) -> None:
        try:
            self.device.shell("dumpsys battery reset", check=False)
            if walk is None:
                return
            if mode == "ref":
                self.stop_apps()
                if walk.fixture is not None:
                    walk.fixture.stop()
            else:
                walk.run_step("teardown")
        except (subprocess.SubprocessError, OSError, self.walk_module.StepFailed) as e:
            self.note(f"{mode}: teardown: {e}")

    def battery_reset(self) -> None:
        """batterystats from zero with the battery as if unplugged, so the
        stats count; whether Waydroid's do is what the bs_src field says."""
        self.device.shell("dumpsys battery unplug", check=False)
        self.device.shell("dumpsys batterystats --reset", check=False)

    def checkin(self, uid: int | None) -> tuple[dict | None, str]:
        if uid is None:
            return None, "no uid"
        text = self.device.shell("dumpsys batterystats --checkin", timeout=60, check=False)
        stats = parse_checkin(text, uid)
        return stats, ("checkin" if stats["rows"] else "no rows for the uid")

    def measure(self, mode, name, seconds, every, package, uid, walk, address, samples) -> Phase:
        phase = Phase(name, time.time())
        tick, sample = self.args.tick, self.args.sample
        previous = self.ss()
        next_sample = next_send = time.monotonic()
        end = time.monotonic() + seconds
        sent = 0
        while True:
            now_mono = time.monotonic()
            if now_mono >= end:
                break
            conns = self.ss()
            phase.events += timeline_events(previous, conns, time.time(), tick)
            previous = conns
            if every and now_mono >= next_send:
                sent += 1
                phase.sends += 1
                if self.send(walk, address, f"power {mode} {sent} {int(time.time())}"):
                    phase.sends_ok += 1
                next_send += every
            if now_mono >= next_sample:
                self.observe_tcp(phase, conns)
                self.read_procs(package, phase)
                self.read_wakelocks(uid, phase)
                samples.write(
                    json.dumps(
                        {
                            "t": time.time(),
                            "phase": name,
                            "conns": {
                                f"{c.local}>{c.peer}": {k: c.counters.get(k) for k in SS_INT_KEYS}
                                for c in conns.values()
                            },
                            "cpu": phase.cpu.total(),
                            "vcsw": phase.vcsw.total(),
                            "wakelock_held": phase.wakelock_held,
                        }
                    )
                    + "\n"
                )
                samples.flush()
                next_sample += sample
            time.sleep(max(0.0, min(tick, end - time.monotonic())))
        # The last state, so the phase ends at its end.
        conns = self.ss()
        phase.events += timeline_events(previous, conns, time.time(), tick)
        self.observe_tcp(phase, conns)
        self.read_procs(package, phase)
        phase.end = time.time()
        return phase

    def send(self, walk, address: str | None, body: str) -> bool:
        if not address or walk is None or walk.fixture is None:
            return False
        proc = subprocess.run(
            [self.args.loquax, "send", "--config", str(walk.fixture.cfg), "--via", "direct", address, body],
            env={**os.environ, "LNMSG_HOME": str(walk.fixture.sender)},
            capture_output=True,
            timeout=60,
        )
        if proc.returncode != 0:
            self.note(f"send exit={proc.returncode}: {proc.stderr.decode(errors='replace')[-200:]}")
        return proc.returncode == 0

    def phase_numbers(self, mode: str, phase: Phase, uid, base: dict) -> dict:
        span = phase.end - phase.start
        tail = self.args.tail
        stats, bs_src = self.checkin(uid)
        self.device.shell("dumpsys batterystats --reset", check=False)
        if phase.cpu_src == "proc":
            cpu_s = phase.cpu.total() / self.clk_tck
            cpu_s_h = per_hour(cpu_s, span)
        elif phase.cpuinfo_pct:
            cpu_s_h = sum(phase.cpuinfo_pct) / len(phase.cpuinfo_pct) / 100 * 3600
        else:
            cpu_s_h = None
        row = {
            "mode": mode,
            "phase": phase.name,
            "ok": True,
            "start": phase.start,
            "end": phase.end,
            "hours": span / 3600,
            "segs_in_h": per_hour(phase.segs_in.total(), span),
            "segs_out_h": per_hour(phase.segs_out.total(), span),
            "bytes_in_h": per_hour(phase.bytes_in.total(), span),
            "bytes_out_h": per_hour(phase.bytes_out.total(), span),
            "conns": phase.segs_in.keys(),
            "events": len(phase.events),
            "radio_on_frac": radio_on_seconds(phase.events, phase.start, phase.end, tail) / span
            if span > 0
            else 0.0,
            "tail_s": tail,
            "cpu_s_h": cpu_s_h,
            "cpu_src": phase.cpu_src,
            "vcsw_h": per_hour(phase.vcsw.total(), span) if phase.csw_src == "proc" else None,
            "nvcsw_h": per_hour(phase.nvcsw.total(), span) if phase.csw_src == "proc" else None,
            "csw_src": phase.csw_src,
            "wakelock_held_frac": phase.wakelock_held / phase.wakelock_samples
            if phase.wakelock_samples
            else None,
            "wakelock_tags": sorted(phase.wakelock_tags),
            "wakelock_src": phase.wakelock_src,
            "bs_partial_wl_s_h": per_hour(stats["partial_wl_ms"] / 1000, span)
            if stats and stats["rows"]
            else None,
            "bs_cpu_s_h": per_hour((stats["cpu_user_ms"] + stats["cpu_sys_ms"]) / 1000, span)
            if stats and stats["rows"]
            else None,
            "bs_src": bs_src,
            "sends": f"{phase.sends_ok}/{phase.sends}",
            **base,
        }
        return row

    def probe(self) -> None:
        """Once, before the modes: the device answers, and CLK_TCK."""
        state = self.device.run("get-state", timeout=20).decode().strip()
        if state != "device":
            raise ModeFailed(f"adb get-state says {state!r}")
        tck = self.device.shell("getconf CLK_TCK", check=False).strip()
        if tck.isdigit():
            self.clk_tck = int(tck)
        self.note(f"probe: device={self.args.serial} ip={self.device_ip} clk_tck={self.clk_tck}")


def parse_args(argv: list[str]) -> argparse.Namespace:
    walk = load_walk()
    debug = walk.REPO / "target" / walk.TARGET / "debug"
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--serial", required=True, help="adb serial of the device (adb devices)")
    p.add_argument("--device-ip", help="the device's address as the server sees it; default: the serial's")
    p.add_argument("--modes", default=",".join(MODES), help=f"comma separated, of {MODES}")
    p.add_argument("--idle", type=float, default=7200, help="seconds of the idle phase")
    p.add_argument("--traffic", type=float, default=3600, help="seconds of the traffic phase")
    p.add_argument("--message-every", type=float, default=600, help="seconds between messages")
    p.add_argument("--settle", type=float, default=120, help="seconds after the join before measuring")
    p.add_argument("--sample", type=float, default=30, help="seconds between the counter samples")
    p.add_argument("--tick", type=float, default=1, help="seconds between the timeline's ss reads")
    p.add_argument("--tail", type=float, default=10, help="the modem's tail in seconds")
    p.add_argument(
        "--bprime-link-keepalive",
        type=int,
        default=600,
        help="keepalive_interval of the host's links in Bprime, seconds",
    )
    p.add_argument("--ref-apk", help="the reference app's APK, installed over its old self")
    p.add_argument("--ref-package", help="the reference app's package name")
    p.add_argument("--ref-address", help="the reference app's LXMF address, for the traffic phase")
    p.add_argument("--ref-connect-wait", type=float, default=120, help="seconds for ref to connect")
    p.add_argument(
        "--apk",
        default=str(walk.REPO / "loquax-android/android/bin/loquax-debug-x86_64.apk"),
        help="the Loquax APK (make waydroid)",
    )
    p.add_argument("--server", default="192.168.240.1:4242", help="host:port of the TCP server")
    p.add_argument("--listen", default="0.0.0.0", help="the TCP server's listen address")
    p.add_argument("--loquax", default=str(debug / "loquax"), help="host loquax (--features tui)")
    p.add_argument("--lnsd", default=str(debug / "lnsd"), help="host lnsd")
    p.add_argument("--walk-mailbox", help="a built walk_mailbox example; default: cargo run it")
    p.add_argument("--adb", default="adb", help="the adb to use")
    p.add_argument("--out", default=str(walk.REPO / "target/tmp/power-proxy"), help="log directory")
    return p.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    chosen = [m.strip() for m in args.modes.split(",") if m.strip()]
    unknown = [m for m in chosen if m not in MODES]
    if unknown:
        print(f"power-proxy: unknown modes {unknown}; known: {MODES}", file=sys.stderr)
        return 2
    proxy = Proxy(args)
    start = time.monotonic()
    try:
        proxy.probe()
    except (ModeFailed, subprocess.SubprocessError, OSError, proxy.walk_module.StepFailed) as e:
        for mode in chosen:
            proxy.fail(mode, "setup", f"probe: {e}")
        chosen = []
    for mode in chosen:
        proxy.run_mode(mode)
    ms = int((time.monotonic() - start) * 1000)
    proxy.emit(table(proxy.rows))
    failed = sorted(set(proxy.failed_modes))
    ok = len({r["mode"] for r in proxy.rows} - set(failed))
    proxy.emit(summary_line(ok, len(failed), ms))
    (proxy.out / "power.json").write_text(
        json.dumps({"args": vars(args), "phases": proxy.rows}, indent=2, default=str), encoding="utf-8"
    )
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
