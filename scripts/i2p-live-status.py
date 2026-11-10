#!/usr/bin/env python3
"""The I2P status surface, discovery and stale mark on the private bed (1588).

Run inside scripts/i2p-privnet.sh (`just i2p-live` does), which gives SAM_1..
SAM_N and I2P_BED. One driver for both stacks: the same config text, the same
client tools (Python's rnstatus and rnprobe, and lnstatus where lnsd answers),
only the daemon differs, as the drop-in rule asks.

Modes:

  rows     Two runs, one per server stack. In each, a connectable, discoverable
           server S on router 2 and two `peers =` clients on router 1, one lnsd
           (C_l) and one rnsd (C_r, `discover_interfaces = yes`). Once both
           clients carry a probe to S, prints every I2P row of rnstatus -a
           against S and both clients, normalised (S's b32 as <S>, traffic
           lines out), and requires the rows of the lnsd run to equal the rows
           of the rnsd run, the lnsd client's peer row to equal the rnsd
           client's, and lnstatus to print what rnstatus prints against lnsd.
           Then waits for C_r to have discovered S and requires the stored
           record's `reachable_on` to be S's b32. In the lnsd run it then
           freezes router 2 (SIGSTOP) for FREEZE_S seconds, requires lnsd's
           `I2P_TUNNEL_STALE` and `Tunnel Unresponsive` 20 s in and no teardown
           before the thaw, and `I2P_TUNNEL_ACTIVE` after it.

  restart  For each server stack: S on router 2 and a client of each stack on
           router 1; once both carry a probe, S is stopped and started again on
           the same key. The old clients are probed for RESTART_WALL_S seconds;
           a fresh client of each stack, started with the restart on router 3,
           is probed over the same wall.
           A fresh client that reaches the restarted S where the old one does
           not names a reconnect gap in the old client's tunnel; neither
           reaching it names the bed (the restarted destination's LeaseSet).

Prints `I2P_ROWS`, `I2P_DISCOVERY`, `I2P_STALE`, `I2P_RESTART` lines; exit 0
when every requirement held, 1 otherwise.

Environment: SAM_1, SAM_2 (from the bed), I2P_BED, I2P_STATUS_WORK (where the
daemons' directories and logs go; default $I2P_BED/status), LNSD and LNSTATUS
(default the release binaries of this tree), FREEZE_S (default 45),
RESTART_WALL_S (default 180), RESTART_GAP_S (seconds between S's stop and its
start, default 2), RESTART_STACKS (the server stacks `restart`
runs, default `rnsd lnsd`).
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
import re
import signal
import struct
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RELEASE = ROOT / "target" / "x86_64-unknown-linux-musl" / "release"
LNSD = os.environ.get("LNSD", str(RELEASE / "lnsd"))
LNSTATUS = os.environ.get("LNSTATUS", str(RELEASE / "lnstatus"))
PYPATH = f"{ROOT / 'reference' / 'Reticulum'}:{ROOT / 'reference' / 'LXMF'}"
LINK_WALL_S = 150.0
DISCOVERY_WALL_S = 180.0
FREEZE_S = float(os.environ.get("FREEZE_S", "45"))
RESTART_WALL_S = float(os.environ.get("RESTART_WALL_S", "180"))
RESTART_GAP_S = float(os.environ.get("RESTART_GAP_S", "2"))
T0 = time.monotonic()
FAILED: list[str] = []


def say(msg: str) -> None:
    print(f"[i2p-status {time.monotonic() - T0:6.1f}] {msg}", flush=True)


def require(ok: bool, what: str) -> None:
    if not ok:
        FAILED.append(what)
        say(f"FAILED: {what}")


def pyenv() -> dict[str, str]:
    env = dict(os.environ)
    env["PYTHONPATH"] = PYPATH
    env["PYTHONUNBUFFERED"] = "1"
    return env


def b32_of_key(path: Path) -> str:
    """The .b32.i2p name of a stored I2P private key, without the suffix."""
    raw = base64.b64decode(path.read_text().strip(), altchars=b"-~")
    cert_len = struct.unpack("!H", raw[385:387])[0]
    public = raw[: 387 + cert_len]
    return base64.b32encode(hashlib.sha256(public).digest()).decode()[:52].lower()


class Daemon:
    def __init__(self, work: Path, name: str, stack: str, sam: str, iface: str,
                 reticulum: str = "") -> None:
        self.name, self.stack, self.sam = name, stack, sam
        self.dir = work / name
        self.dir.mkdir(parents=True, exist_ok=True)
        self.log_path = self.dir / "out.log"
        self.proc: subprocess.Popen | None = None
        (self.dir / "config").write_text(
            "[reticulum]\n"
            "  enable_transport = yes\n"
            "  share_instance = yes\n"
            f"  instance_name = st{os.getpid()}{name}\n"
            "  respond_to_probes = yes\n"
            f"{reticulum}"
            "[logging]\n"
            "  loglevel = 6\n"
            "[auto]\n"
            "  lan = no\n"
            "  servers = no\n"
            "  bluetooth = no\n"
            "  serial = no\n"
            "[interfaces]\n"
            "  [[I2P]]\n"
            "    type = I2PInterface\n"
            "    enabled = yes\n"
            f"{iface}"
        )

    def start(self) -> None:
        env = pyenv()
        env["I2P_SAM_ADDRESS"] = self.sam
        if self.stack == "lnsd":
            env["RUST_LOG"] = "info"
            argv = [LNSD, "--config", str(self.dir), "-vv"]
        else:
            argv = [sys.executable, "-m", "RNS.Utilities.rnsd", "--config", str(self.dir), "-vv"]
        log = open(self.log_path, "ab")
        self.proc = subprocess.Popen(
            ["setpriv", "--pdeathsig", "TERM", "--", *argv],
            stdout=log, stderr=subprocess.STDOUT, env=env,
        )
        say(f"{self.name}: {self.stack} started on {self.sam}")

    def stop(self) -> None:
        if self.proc is None:
            return
        self.proc.send_signal(signal.SIGTERM)
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        self.proc = None

    def log(self) -> str:
        try:
            return re.sub(r"\x1b\[[0-9;]*m", "", self.log_path.read_text(errors="replace"))
        except FileNotFoundError:
            return ""

    def keyfile(self) -> Path | None:
        keys = [p for p in (self.dir / "storage" / "i2p").glob("*.i2p") if p.stat().st_size > 0] \
            if (self.dir / "storage" / "i2p").is_dir() else []
        return keys[0] if keys else None

    def wait_b32(self, wall: float = 120.0) -> str | None:
        deadline = time.monotonic() + wall
        while time.monotonic() < deadline:
            key = self.keyfile()
            if key is not None:
                return b32_of_key(key)
            time.sleep(0.5)
        return None

    def probe_hash(self) -> str:
        out = subprocess.run(
            [sys.executable, "-c",
             "import sys, RNS\n"
             "i = RNS.Identity.from_file(sys.argv[1])\n"
             "print(RNS.Destination.hash_from_name_and_identity('rnstransport.probe', i).hex())",
             str(self.dir / "storage" / "transport_identity")],
            capture_output=True, text=True, env=pyenv(), timeout=30,
        )
        return out.stdout.strip()

    def tool(self, argv: list[str], timeout: float = 25.0) -> str:
        try:
            out = subprocess.run(argv, capture_output=True, text=True, env=pyenv(), timeout=timeout)
            return out.stdout + out.stderr
        except subprocess.TimeoutExpired as e:
            return (e.stdout or b"").decode(errors="replace") if isinstance(e.stdout, bytes) else (e.stdout or "")

    def rnstatus(self, *args: str) -> str:
        return self.tool([sys.executable, "-m", "RNS.Utilities.rnstatus", "--config", str(self.dir), *args])

    def lnstatus(self, *args: str) -> str:
        return self.tool([LNSTATUS, "--config", str(self.dir), *args])

    def status_json(self) -> dict:
        try:
            return json.loads(self.rnstatus("-a", "-j"))
        except ValueError:
            return {}

    def tunnel_states(self) -> list[str]:
        return [str(i.get("tunnelstate")) for i in self.status_json().get("interfaces", [])
                if i.get("tunnelstate")]

    def probe(self, dest: str, timeout: int = 15) -> bool:
        text = self.tool(
            [sys.executable, "-m", "RNS.Utilities.rnprobe", "--config", str(self.dir),
             "-t", str(timeout), "rnstransport.probe", dest],
            timeout=timeout + 30,
        ).replace("\r", "\n")
        # rnprobe exits 0 on SIGTERM, so the verdict is its text.
        return "Sent 1, received 1" in text

    def wait_probe(self, dest: str, wall: float) -> float | None:
        start = time.monotonic()
        while time.monotonic() - start < wall:
            if self.probe(dest):
                return time.monotonic() - start
            time.sleep(2)
        return None


def i2p_blocks(text: str, b32: str) -> list[str]:
    """The I2P rows of an rnstatus/lnstatus text, normalised for comparison:
    S's b32 as <S>, and only the lines that describe the interface rather than
    its traffic."""
    keep = ("Status", "Mode", "Rate", "Peers", "I2P", "I2P B32")
    blocks, cur = [], None
    for line in text.replace(b32, "<S>").splitlines():
        if re.match(r"^ I2PInterface(Peer)?\[", line):
            cur = [line.strip()]
            blocks.append(cur)
        elif cur is not None and line.strip() == "":
            cur = None
        elif cur is not None:
            key = line.split(":", 1)[0].strip()
            if key in keep:
                cur.append(re.sub(r"\s+", " ", line.strip()))
    return sorted("\n".join(b) for b in blocks)


def shape(blocks: list[str]) -> list[str]:
    """The rows with their tunnel state blanked. rnsd's own state text lags its
    link: on 1588's bed its client read `Creating Tunnel` on a peer that had
    just carried a probe, and one of its accepted peers did the same, so the
    state value is compared on lnsd alone (`lnsd_states`), and the stacks on
    everything else."""
    return sorted(re.sub(r"^I2P : .*$", "I2P : <state>", b, flags=re.M) for b in blocks)


def states(blocks: list[str]) -> list[str]:
    return [m for b in blocks for m in re.findall(r"^I2P : (.*)$", b, flags=re.M)]


def show(title: str, blocks: list[str]) -> None:
    print(f"--- {title}")
    for b in blocks:
        print(b)
        print()


def discovered_reachable(c: Daemon, b32: str, wall: float) -> tuple[bool, str]:
    deadline = time.monotonic() + wall
    text = ""
    while time.monotonic() < deadline:
        text = c.rnstatus("-D")
        if b32 in text:
            return True, text
        time.sleep(5)
    return False, text


def bed_router_pid(i: int) -> int | None:
    bed = os.environ.get("I2P_BED", "")
    out = subprocess.run(["pgrep", "-f", "--", f"--datadir={bed}/r{i}( |$)"],
                         capture_output=True, text=True)
    pids = [int(p) for p in out.stdout.split()]
    return pids[0] if pids else None


def first_line_time(daemon: Daemon, needle: str, since_len: int) -> bool:
    return needle in daemon.log()[since_len:]


def rows_run(work: Path, s_stack: str) -> dict:
    """One server stack; returns the normalised rows and the daemons."""
    tag = f"rows-{s_stack}"
    s = Daemon(work / tag, "s", s_stack, os.environ["SAM_2"],
               "    connectable = yes\n    discoverable = yes\n")
    s.start()
    b32 = s.wait_b32()
    require(b32 is not None, f"{tag}: S wrote an I2P key")
    if b32 is None:
        s.stop()
        return {}
    say(f"{tag}: S is {b32}.b32.i2p")
    c_l = Daemon(work / tag, "c_l", "lnsd", os.environ["SAM_1"], f"    peers = {b32}.b32.i2p\n")
    c_r = Daemon(work / tag, "c_r", "rnsd", os.environ["SAM_1"], f"    peers = {b32}.b32.i2p\n",
                 reticulum="  discover_interfaces = yes\n")
    t_c = time.monotonic()
    c_l.start()
    c_r.start()
    probe = s.probe_hash()
    up = {c.name: c.wait_probe(probe, LINK_WALL_S) for c in (c_l, c_r)}
    say(f"{tag}: first probe after {up}")
    require(all(v is not None for v in up.values()), f"{tag}: both clients reach S")

    texts = {
        "s": s.rnstatus("-a"),
        "c_l": c_l.rnstatus("-a"),
        "c_r": c_r.rnstatus("-a"),
    }
    if s_stack == "lnsd":
        texts["s_lnstatus"] = s.lnstatus("-a")
        texts["c_l_lnstatus"] = c_l.lnstatus("-a")
    rows = {}
    for k, text in texts.items():
        (work / tag / f"{k}.status.txt").write_text(text)
        rows[k] = i2p_blocks(text, b32)
        show(f"{tag} {k}", rows[k])

    found, text = discovered_reachable(c_r, b32, DISCOVERY_WALL_S - (time.monotonic() - t_c))
    line = next((l.strip() for l in text.splitlines() if b32 in l), "none")
    print(f"I2P_DISCOVERY s={s_stack} found={'yes' if found else 'no'} line={line!r}", flush=True)
    # Required of lnsd. rnsd's own announce is recorded, not required: on
    # 1588's first bed run rnsd S sent it ("Sending interface discovery
    # announce for I2P with 134B payload", 60 s in) and its rnsd client,
    # which stored lnsd's, logged nothing of it.
    if s_stack == "lnsd":
        require(found, f"{tag}: rnsd discovered S with reachable_on = its b32")
    return {"tag": tag, "rows": rows, "s": s, "c_l": c_l, "c_r": c_r, "b32": b32, "probe": probe}


def stale_run(run: dict) -> None:
    s, c_l, c_r = run["s"], run["c_l"], run["c_r"]
    pid = bed_router_pid(2)
    require(pid is not None, "router 2's pid")
    if pid is None:
        return
    mark = len(c_l.log())
    os.kill(pid, signal.SIGSTOP)
    t_freeze = time.monotonic()
    say(f"router 2 (pid {pid}) frozen for {FREEZE_S:.0f} s")
    stale_event = stale_status_l = stale_status_r = None
    while time.monotonic() - t_freeze < FREEZE_S:
        el = time.monotonic() - t_freeze
        if stale_event is None and "I2P_TUNNEL_STALE" in c_l.log()[mark:]:
            stale_event = el
        if stale_status_l is None and any("Unresponsive" in st for st in c_l.tunnel_states()):
            stale_status_l = el
        if stale_status_r is None and any("Unresponsive" in st for st in c_r.tunnel_states()):
            stale_status_r = el
        time.sleep(1)
    frozen_log = c_l.log()[mark:]
    teardown = "tearing down the stream" in frozen_log
    os.kill(pid, signal.SIGCONT)
    t_thaw = time.monotonic()
    say("router 2 thawed")
    active_event = None
    while time.monotonic() - t_thaw < 90:
        if "I2P_TUNNEL_ACTIVE" in c_l.log()[mark:]:
            active_event = time.monotonic() - t_thaw
            break
        time.sleep(1)
    fmt = lambda v: "none" if v is None else f"{v:.1f}"
    event_lines = [l for l in c_l.log()[mark:].splitlines() if "I2P_TUNNEL_" in l]
    for l in event_lines:
        print(f"  c_l: {l.strip()[:300]}")
    print(
        f"I2P_STALE s=lnsd frozen_s={FREEZE_S:.0f} lnsd_event_s={fmt(stale_event)} "
        f"lnsd_status_s={fmt(stale_status_l)} rnsd_status_s={fmt(stale_status_r)} "
        f"lnsd_teardown_while_frozen={'yes' if teardown else 'no'} "
        f"lnsd_active_after_thaw_s={fmt(active_event)}",
        flush=True,
    )
    # The mark counts from the last byte read, not from the freeze: a
    # keepalive arrives every 10 s, so the freeze lands 0 to 10 s after one
    # and the mark 10 to 21 s into it; the event's `idle_ms` is the 20 s.
    idle = [int(m) for l in event_lines for m in re.findall(r"I2P_TUNNEL_STALE.*idle_ms=(\d+)", l)]
    print(f"  lnsd stale idle_ms={idle}", flush=True)
    require(stale_event is not None and stale_event <= 31 and bool(idle)
            and 20_000 < idle[0] <= 22_000,
            "lnsd's I2P_TUNNEL_STALE after 20 s without a byte")
    require(stale_status_l is not None, "rnstatus against lnsd shows Tunnel Unresponsive")
    require(not teardown, "no teardown before the 110 s read timeout")
    require(active_event is not None, "lnsd's I2P_TUNNEL_ACTIVE after the thaw")


def mode_rows(work: Path) -> None:
    runs = {}
    for stack in ("rnsd", "lnsd"):
        run = rows_run(work, stack)
        runs[stack] = run
        if not run:
            continue
        if stack == "lnsd":
            stale_run(run)
        for k in ("c_l", "c_r", "s"):
            run[k].stop()
    if not all(runs.values()):
        return
    r, l = runs["rnsd"]["rows"], runs["lnsd"]["rows"]
    same_s = shape(r["s"]) == shape(l["s"])
    same_peer = shape(l["c_l"]) == shape(l["c_r"]) and shape(r["c_l"]) == shape(r["c_r"])
    same_ln = l["s_lnstatus"] == l["s"] and l["c_l_lnstatus"] == l["c_l"]
    lnsd_states = states(l["s"]) + states(l["c_l"]) + states(r["c_l"])
    rnsd_states = states(r["s"]) + states(r["c_r"]) + states(l["c_r"])
    active = bool(lnsd_states) and all(st == "Tunnel Active" for st in lnsd_states)
    print(
        f"I2P_ROWS s_rows={'same' if same_s else 'differ'} "
        f"peer_rows={'same' if same_peer else 'differ'} "
        f"lnstatus={'same' if same_ln else 'differ'} "
        f"lnsd_states={'active' if active else lnsd_states} rnsd_states={rnsd_states}",
        flush=True,
    )
    require(active, "every lnsd tunnel that carried a probe reads Tunnel Active")
    require(same_s, "rnstatus against lnsd S prints the rows rnstatus against rnsd S prints")
    require(same_peer, "the lnsd client's peer row is the rnsd client's")
    require(same_ln, "lnstatus against lnsd prints what rnstatus prints")


def mode_restart(work: Path) -> None:
    for s_stack in os.environ.get("RESTART_STACKS", "rnsd lnsd").split():
        tag = f"restart-{s_stack}"
        s = Daemon(work / tag, "s", s_stack, os.environ["SAM_2"], "    connectable = yes\n")
        s.start()
        b32 = s.wait_b32()
        if b32 is None:
            require(False, f"{tag}: S wrote an I2P key")
            s.stop()
            continue
        peers = f"    peers = {b32}.b32.i2p\n"
        olds = [Daemon(work / tag, f"old_{st}", st, os.environ["SAM_1"], peers) for st in ("lnsd", "rnsd")]
        for c in olds:
            c.start()
        probe = s.probe_hash()
        before = {c.name: c.wait_probe(probe, LINK_WALL_S) for c in olds}
        say(f"{tag}: before the restart {before}")
        bed = os.environ.get("I2P_BED", "")
        r2 = Path(bed) / "r2" / "i2pd.log"
        r2_mark = len(r2.read_text(errors="replace")) if r2.is_file() else 0
        s.stop()
        time.sleep(RESTART_GAP_S)
        s.start()
        t_restart = time.monotonic()
        # Fresh clients start with the restart, on a router of their own, so
        # old and fresh are probed over the same wall and the same seconds.
        fresh = [Daemon(work / tag, f"fresh_{st}", st, os.environ["SAM_3"], peers) for st in ("lnsd", "rnsd")]
        for c in fresh:
            c.start()
        after: dict[str, float | None] = {}

        def reach(c: Daemon) -> None:
            after[c.name] = c.wait_probe(probe, RESTART_WALL_S)

        threads = [threading.Thread(target=reach, args=(c,)) for c in (*olds, *fresh)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        after_old = {c.name: after[c.name] for c in olds}
        after_fresh = {c.name: after[c.name] for c in fresh}
        say(f"{tag}: after the restart, old {after_old}, fresh {after_fresh}")
        # The router's own account of the restart (LOGLEVEL=info): whether
        # SESSION CREATE was handed the destination the closed session left
        # behind (`Local destination <S> exists`, ClientContext.cpp:393/408 in
        # i2pd 2.56) and how often it then failed to publish its LeaseSet.
        r2_text = r2.read_text(errors="replace")[r2_mark:] if r2.is_file() else ""
        publish_misses = r2_text.count("Publish confirmation was not received") if r2.is_file() else -1
        reused = f"{b32}.b32.i2p exists" in r2_text
        not_ready = r2_text.count("destination is not ready")
        fmt = lambda d: " ".join(f"{k}={'no' if v is None else f'{v:.1f}'}" for k, v in d.items())
        print(
            f"I2P_RESTART s={s_stack} gap_s={RESTART_GAP_S:g} reused={'yes' if reused else 'no'} "
            f"not_ready={not_ready} since_restart_s={time.monotonic() - t_restart:.0f} "
            f"{fmt(before)} after_{fmt(after_old).replace(' ', ' after_')} "
            f"after_{fmt(after_fresh).replace(' ', ' after_')} r2_publish_misses={publish_misses}",
            flush=True,
        )
        for c in [*olds, *fresh, s]:
            c.stop()


def main() -> int:
    mode = sys.argv[1] if len(sys.argv) > 1 else "rows"
    work = Path(os.environ.get("I2P_STATUS_WORK") or Path(os.environ["I2P_BED"]) / "status")
    work.mkdir(parents=True, exist_ok=True)
    say(f"mode {mode}, work {work}, lnsd {LNSD}")
    try:
        if mode == "rows":
            mode_rows(work)
        elif mode == "restart":
            mode_restart(work)
        else:
            print(f"unknown mode {mode}", file=sys.stderr)
            return 2
    finally:
        # A frozen router must not outlive the run.
        pid = bed_router_pid(2)
        if pid is not None:
            os.kill(pid, signal.SIGCONT)
    say(f"done, {len(FAILED)} requirement(s) failed: {FAILED}")
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
