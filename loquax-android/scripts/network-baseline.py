#!/usr/bin/env python3
"""How long a phone app takes to be reachable again after a network change
(1298, Refs #232, #471): Loquax against Columba, on the same phone.

Lew's measure for the phone-only user: Columba's background service
chokes when the network changes, and he restarts it by hand. This script
makes the five changes of the order on one adb device and measures, for
the app that holds an LXMF address on that phone (Columba, or Loquax with
the mailbox on the phone):

    wifi_toggle     WLAN off for --off-secs, then on
    mobile_switch   WLAN off with mobile data on for --off-secs, then on
    flight_mode     flight mode for --flight-secs (120), then off
    other_wlan      to --other-ssid (another subnet), then back to --home-ssid
    sleep           screen off and Doze forced for --sleep-secs (600)

From the moment the network is back (or the phone awake), the host's
`loquax send --via direct` to --to is tried again and again, each try
bounded by --try-secs, until one exits 0: a direct send ends 0 only with
the delivery proof, so that moment is when the app on the phone is
reachable again. The app is brought to the front first; meanwhile the
screen is read every --look-secs and every text that was not on it before
the change is kept: what the app shows meanwhile. A change the device cannot make (no SIM, no second
WLAN given) is said as skipped, never measured.

One line per change, to stdout and OUT/network.log:

    NET app=<package> change=<name> ok=<0|1> off_s=<n> kinds=<a>b>c>
        back_ms=<n> tries=<n> shown=<texts|...> detail=<...>

then the report's table (`table`). The reviewer runs it twice on the
Fairphone, once with Columba's service holding the address and once with
Loquax's phone mailbox, both reaching the host's lnsd, the sender's config
and home those of a host `loquax` that reaches the same lnsd:

    python3 -I loquax-android/scripts/network-baseline.py \\
        --serial <fairphone> --package network.columba.app \\
        --to <columba's LXMF address> \\
        --loquax target/debug/loquax --sender-home ~/scratch/sender \\
        --sender-config ~/scratch/sender/rns \\
        --home-ssid <ssid> --other-ssid <ssid> --other-pass <pass> \\
        --out ~/scratch/baseline-columba

Standard library only, Python 3.11+. The adb plumbing and the default
network's parser are the device walk's (scripts/device-walk.py).
"""

from __future__ import annotations

import argparse
import importlib.util
import os
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path

_spec = importlib.util.spec_from_file_location(
    "device_walk", Path(__file__).resolve().parent / "device-walk.py"
)
walk = importlib.util.module_from_spec(_spec)
sys.modules["device_walk"] = walk
_spec.loader.exec_module(walk)

CHANGES = ["wifi_toggle", "mobile_switch", "flight_mode", "other_wlan", "sleep"]


@dataclass
class Measured:
    """One change's numbers."""

    app: str
    change: str
    ok: bool
    off_s: int = 0
    kinds: tuple[str, str, str] = ("-", "-", "-")
    back_ms: int | None = None
    tries: int = 0
    shown: list[str] = field(default_factory=list)
    detail: str = ""


def new_texts(before: set[str], seen: list[str]) -> list[str]:
    """The texts of `seen` that were not on the screen before the change,
    each once, in the order they came."""
    out: list[str] = []
    for text in seen:
        text = text.strip()
        if text and text not in before and text not in out:
            out.append(text)
    return out


def net_line(m: Measured) -> str:
    """The NET line of one change."""
    back = "-" if m.back_ms is None else str(m.back_ms)
    shown = "|".join(t.replace(" ", "_") for t in m.shown) or "-"
    return (
        f"NET app={m.app} change={m.change} ok={int(m.ok)} off_s={m.off_s} "
        f"kinds={'>'.join(m.kinds)} back_ms={back} tries={m.tries} shown={shown} "
        f"detail={m.detail.replace(' ', '_') or '-'}"
    )


def parse_net_line(line: str) -> dict[str, str] | None:
    """A NET line's fields, or None for another line."""
    if not line.startswith("NET "):
        return None
    fields = {}
    for part in line[4:].split(" "):
        key, _, value = part.partition("=")
        fields[key] = value
    return fields


def table(lines: list[str]) -> list[str]:
    """The NET lines as the report's table, one row per app and change:
    `| change | app | off s | kinds | back ms | tries | shown meanwhile |`."""
    rows = [
        "| change | app | off s | kinds | back to a proven delivery, ms | tries | shown meanwhile |",
        "|---|---|---|---|---|---|---|",
    ]
    for line in lines:
        f = parse_net_line(line)
        if f is None:
            continue
        back = f["back_ms"] if f["ok"] == "1" else f"not back ({f['detail'].replace('_', ' ')})"
        rows.append(
            f"| {f['change']} | {f['app']} | {f['off_s']} | {f['kinds']} | {back} "
            f"| {f['tries']} | {f['shown'].replace('_', ' ')} |"
        )
    return rows


class Baseline:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.dev = walk.Device(args.adb, args.serial)
        self.out = Path(args.out)
        self.out.mkdir(parents=True, exist_ok=True)
        self.lines: list[str] = []

    def kind(self) -> str:
        return walk.default_network(self.dev.shell("dumpsys connectivity", check=False))

    def texts(self) -> set[str]:
        nodes = walk.parse_dump(
            self.dev.shell(
                "uiautomator dump /sdcard/net-dump.xml >/dev/null && cat /sdcard/net-dump.xml",
                timeout=30,
                check=False,
            )
        )
        return {n.text for n in nodes if n.text}

    def shell(self, *commands: str) -> None:
        for command in commands:
            self.dev.shell(command, check=False)

    def send_once(self, body: str) -> bool:
        env = {**os.environ, "LNMSG_HOME": self.args.sender_home}
        cmd = [self.args.loquax, "send", "--via", "direct", self.args.to, body]
        if self.args.sender_config:
            cmd[2:2] = ["--config", self.args.sender_config]
        try:
            return (
                subprocess.run(cmd, env=env, capture_output=True, timeout=self.args.try_secs).returncode
                == 0
            )
        except subprocess.TimeoutExpired:
            return False

    def measure(self, change: str, off: list[str], on: list[str], off_secs: int) -> Measured:
        before_texts = self.texts()
        before = self.kind()
        self.shell(*off)
        during = walk.wait_for(lambda: (k := self.kind()) != before and k, 15, every=1.0) or before
        seen: list[str] = []
        until = time.monotonic() + off_secs
        while time.monotonic() < until:
            seen.extend(self.texts())
            time.sleep(self.args.look_secs)
        self.shell(*on)
        back = time.monotonic()
        tries = 0
        back_ms = None
        while time.monotonic() - back < self.args.budget_secs:
            tries += 1
            if self.send_once(f"baseline {change} {tries} {int(time.time())}"):
                back_ms = int((time.monotonic() - back) * 1000)
                break
            seen.extend(self.texts())
        after = self.kind()
        m = Measured(
            app=self.args.package,
            change=change,
            ok=back_ms is not None,
            off_s=off_secs,
            kinds=(before, during, after),
            back_ms=back_ms,
            tries=tries,
            shown=new_texts(before_texts, seen),
        )
        if change != "sleep" and during == before:
            m.ok, m.detail = False, f"the change did nothing: the default network stayed {before}"
        elif back_ms is None:
            m.detail = f"no proven delivery within {self.args.budget_secs}s"
        return m

    def skipped(self, change: str, why: str) -> Measured:
        return Measured(app=self.args.package, change=change, ok=False, detail=f"skipped: {why}")

    def change(self, name: str) -> Measured:
        a = self.args
        if name == "wifi_toggle":
            return self.measure(name, ["svc wifi disable"], ["svc wifi enable"], a.off_secs)
        if name == "mobile_switch":
            self.shell("svc data enable")
            return self.measure(name, ["svc wifi disable"], ["svc wifi enable"], a.off_secs)
        if name == "flight_mode":
            return self.measure(
                name,
                ["cmd connectivity airplane-mode enable"],
                ["cmd connectivity airplane-mode disable"],
                a.flight_secs,
            )
        if name == "other_wlan":
            if not (a.other_ssid and a.home_ssid):
                return self.skipped(name, "no --other-ssid and --home-ssid")
            other = f"cmd wifi connect-network {a.other_ssid} wpa2 {a.other_pass}"
            home = f"cmd wifi connect-network {a.home_ssid} wpa2 {a.home_pass}"
            return self.measure(name, [other], [home], a.off_secs)
        if name == "sleep":
            return self.measure(
                name,
                ["input keyevent KEYCODE_SLEEP", "dumpsys deviceidle force-idle"],
                ["dumpsys deviceidle unforce", "input keyevent KEYCODE_WAKEUP"],
                a.sleep_secs,
            )
        raise ValueError(name)

    def emit(self, line: str) -> None:
        print(line, flush=True)
        self.lines.append(line)
        with open(self.out / "network.log", "a", encoding="utf-8") as log:
            log.write(line + "\n")


def parse_args(argv: list[str]) -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--adb", default="adb")
    p.add_argument("--serial", required=True)
    p.add_argument("--package", required=True, help="the app that holds the address")
    p.add_argument("--to", required=True, help="the LXMF address the app holds, hex")
    p.add_argument("--loquax", required=True, help="the host's loquax binary")
    p.add_argument("--sender-home", required=True, help="LNMSG_HOME of the host's sender")
    p.add_argument("--sender-config", default="", help="its Reticulum config directory")
    p.add_argument("--changes", default=",".join(CHANGES))
    p.add_argument("--off-secs", type=int, default=20)
    p.add_argument("--flight-secs", type=int, default=120)
    p.add_argument("--sleep-secs", type=int, default=600)
    p.add_argument("--budget-secs", type=int, default=300, help="how long to wait for a delivery")
    p.add_argument("--try-secs", type=int, default=30, help="one send's bound")
    p.add_argument("--look-secs", type=float, default=2.0)
    p.add_argument("--home-ssid", default="")
    p.add_argument("--home-pass", default="")
    p.add_argument("--other-ssid", default="")
    p.add_argument("--other-pass", default="")
    p.add_argument("--out", default="target/tmp/network-baseline")
    return p.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    chosen = [c.strip() for c in args.changes.split(",") if c.strip()]
    unknown = [c for c in chosen if c not in CHANGES]
    if unknown:
        print(f"network-baseline: unknown changes {unknown}; known: {CHANGES}", file=sys.stderr)
        return 2
    run = Baseline(args)
    run.shell(f"monkey -p {args.package} -c android.intent.category.LAUNCHER 1")
    time.sleep(3)
    for name in CHANGES:
        if name in chosen:
            run.emit(net_line(run.change(name)))
    for row in table(run.lines):
        run.emit(row)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
