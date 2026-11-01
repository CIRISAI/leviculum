#!/usr/bin/env python3
"""NAT lab (+ciris): can a direct-link punch be forced through a NAT that
normally defeats it?

Builds two NAT'd hosts and a public relay in Linux network namespaces, runs
two peers through them with a chosen punch strategy, and reports, per
scenario, how often both sides succeed, how often only one does, and whether
data then flows both ways over the chosen path. Needs root (a CI runner).

    sudo python3 natlab.py matrix --trials 5

Topology (all addresses private to the lab):

    hA 10.0.1.2 -- rA [NAT] 203.0.113.1 --+
                                          br (inet ns) -- relay 203.0.113.10:3478/3479
    hB 10.0.2.2 -- rB [NAT] 203.0.113.2 --+

NAT kinds, as Linux implements them (filtering is always address+port
dependent: conntrack only lets replies in):
    cone  MASQUERADE: endpoint-independent, port-preserving mapping
    sym   MASQUERADE --random-fully: a fresh random port per destination
          (endpoint-dependent mapping), the case that defeats a plain punch

The punch frames and the success rule of `baseline` mirror leviculum-std's
`direct_link::punch` (every 100 ms for 10 s, success = heard a punch and got
an ack, from any address, path = where the punch came from).
"""

import argparse
import hashlib
import json
import os
import random
import selectors
import socket
import struct
import subprocess
import sys
import time

RELAY_IP = "203.0.113.10"
RELAY_PORTS = (3478, 3479)
NS = ["inet", "rA", "rB", "hA", "hB"]
PUNCH_INTERVAL = 0.1
SELF = os.path.abspath(__file__)


# --- topology -------------------------------------------------------------


def sh(cmd, check=True):
    r = subprocess.run(cmd, shell=True, capture_output=True, text=True)
    if check and r.returncode != 0:
        raise SystemExit(f"failed: {cmd}\n{r.stdout}{r.stderr}")
    return r.stdout


def nsx(ns, cmd, check=True):
    return sh(f"ip netns exec {ns} {cmd}", check)


def teardown():
    for ns in NS:
        sh(f"ip netns del {ns}", check=False)


def setup(kind_a, kind_b):
    teardown()
    for ns in NS:
        sh(f"ip netns add {ns}")
        nsx(ns, "ip link set lo up")
    nsx("inet", "ip link add br type bridge")
    nsx("inet", "ip link set br up")
    nsx("inet", f"ip addr add {RELAY_IP}/24 dev br")
    for side, wan_ip, lan, kind in (("A", "203.0.113.1", "10.0.1", kind_a),
                                    ("B", "203.0.113.2", "10.0.2", kind_b)):
        r, h = f"r{side}", f"h{side}"
        # WAN: router <-> bridge
        sh(f"ip link add wan{side} netns {r} type veth peer name p{side} netns inet")
        nsx("inet", f"ip link set p{side} master br up")
        nsx(r, f"ip addr add {wan_ip}/24 dev wan{side}")
        nsx(r, f"ip link set wan{side} up")
        # LAN: router <-> host
        sh(f"ip link add lan{side} netns {r} type veth peer name eth0 netns {h}")
        nsx(r, f"ip addr add {lan}.1/24 dev lan{side}")
        nsx(r, f"ip link set lan{side} up")
        nsx(h, f"ip addr add {lan}.2/24 dev eth0")
        nsx(h, "ip link set eth0 up")
        nsx(h, f"ip route add default via {lan}.1")
        nsx(r, "sysctl -qw net.ipv4.ip_forward=1")
        masq = "MASQUERADE --random-fully" if kind == "sym" else "MASQUERADE"
        nsx(r, f"iptables -t nat -A POSTROUTING -o wan{side} -j {masq}")
        # A home router drops unsolicited inbound silently (no ICMP).
        nsx(r, f"iptables -A INPUT -i wan{side} -p udp -j DROP")
        nsx(r, f"iptables -A FORWARD -i wan{side} -m conntrack --ctstate NEW -j DROP")


def flush_conntrack():
    for r in ("rA", "rB"):
        nsx(r, "conntrack -F", check=False)


# --- relay (rendezvous + reflexive address, two ports) ---------------------


def relay(args):
    sel = selectors.DefaultSelector()
    socks = []
    for port in RELAY_PORTS:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.bind((RELAY_IP, port))
        s.setblocking(False)
        sel.register(s, selectors.EVENT_READ)
        socks.append(s)
    runs = {}  # run -> name -> {"addr", "hard", "src"}
    starts = {}
    while True:
        for key, _ in sel.select(timeout=1.0):
            s = key.fileobj
            try:
                data, src = s.recvfrom(2048)
                msg = json.loads(data)
            except (OSError, ValueError):
                continue
            addr = f"{src[0]}:{src[1]}"
            if msg["t"] in ("reg", "probe"):
                s.sendto(json.dumps({"t": "you", "addr": addr}).encode(), src)
            if msg["t"] == "ready":
                run = runs.setdefault(msg["run"], {})
                run[msg["name"]] = {"addr": addr, "hard": msg["hard"], "src": src}
                if len(run) == 2:
                    start = starts.setdefault(msg["run"], time.time() + 1.0)
                    for name, me in run.items():
                        other = next(v for k, v in run.items() if k != name)
                        go = {"t": "go", "peer": other["addr"], "peer_hard": other["hard"],
                              "start": start}
                        socks[0].sendto(json.dumps(go).encode(), me["src"])


# --- peer -----------------------------------------------------------------


def parse_addr(text):
    host, port = text.rsplit(":", 1)
    return (host, int(port))


def rpc(sock, dest, msg, want, timeout=30.0):
    end = time.time() + timeout
    while time.time() < end:
        sock.sendto(json.dumps(msg).encode(), dest)
        wait = time.time() + 0.5
        while time.time() < wait:
            sock.settimeout(max(0.01, wait - time.time()))
            try:
                data, src = sock.recvfrom(2048)
                reply = json.loads(data)
            except (socket.timeout, ValueError):
                continue
            if reply.get("t") == want:
                return reply
    raise SystemExit(f"relay: no {want}")


def frame(kind, tag, seq, idx=0):
    return b"LVP" + kind + tag + struct.pack(">IH", seq, idx)


def parse(data, tag):
    if len(data) != 3 + 1 + 8 + 6 or data[:3] != b"LVP" or data[4:12] != tag:
        return None
    seq, idx = struct.unpack(">IH", data[12:18])
    return data[3:4], seq, idx


class Punch:
    """One or many sockets punching toward targets; reports the first socket
    whose success rule holds."""

    def __init__(self, socks, tag, strict):
        self.socks = socks
        self.tag = tag
        self.strict = strict
        self.sel = selectors.DefaultSelector()
        for i, s in enumerate(socks):
            s.setblocking(False)
            self.sel.register(s, selectors.EVENT_READ, i)
        self.heard = {}   # sock idx -> src
        self.acked = {}   # sock idx -> src
        self.sent_to = [set() for _ in socks]
        self.first_heard = None
        self.first_ack = None
        self.seq = 0

    def send(self, i, dest):
        try:
            self.socks[i].sendto(frame(b"P", self.tag, self.seq, i), dest)
            self.sent_to[i].add(dest)
        except OSError:
            pass

    def poll(self, timeout, t0):
        for key, _ in self.sel.select(timeout=timeout):
            i = key.data
            while True:
                try:
                    data, src = self.socks[i].recvfrom(2048)
                except (BlockingIOError, OSError):
                    break
                p = parse(data, self.tag)
                if not p:
                    continue
                kind, seq, _ = p
                if kind == b"P":
                    try:
                        self.socks[i].sendto(frame(b"A", self.tag, seq, i), src)
                    except OSError:
                        pass
                    self.heard.setdefault(i, src)
                    self.first_heard = self.first_heard or round(time.time() - t0, 3)
                elif kind == b"A":
                    if not self.strict or src in self.sent_to[i]:
                        self.acked.setdefault(i, src)
                        self.first_ack = self.first_ack or round(time.time() - t0, 3)

    def winner(self):
        for i in self.acked:
            if self.strict:
                return i, self.acked[i]
            if i in self.heard:
                return i, self.heard[i]
        return None


def run_peer(args):
    tag = hashlib.sha256(args.run.encode()).digest()[:8]
    relay1 = (RELAY_IP, RELAY_PORTS[0])
    relay2 = (RELAY_IP, RELAY_PORTS[1])
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("0.0.0.0", 0))
    mapped = rpc(s, relay1, {"t": "reg", "name": args.name, "run": args.run}, "you")["addr"]
    mapped2 = rpc(s, relay2, {"t": "probe"}, "you")["addr"]
    hard = mapped != mapped2
    go = rpc(s, relay1, {"t": "ready", "name": args.name, "run": args.run, "hard": hard}, "go")
    peer = parse_addr(go["peer"])
    peer_hard = go["peer_hard"]
    t0 = go["start"]
    start = t0 + args.delay
    while time.time() < start:
        time.sleep(min(0.01, start - time.time()))

    strategy = args.strategy
    strict = strategy != "baseline"
    socks = [s]
    role = "single"
    if strategy == "birthday" and hard:
        role = "hard"
        for _ in range(args.sockets - 1):
            extra = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            extra.bind(("0.0.0.0", 0))
            socks.append(extra)
    elif strategy == "birthday" and peer_hard:
        role = "spray"
    p = Punch(socks, tag, strict)
    end = start + args.window
    next_tick = start
    next_spray = start + 0.2
    spray_used = set()
    win = None
    win_at = None
    while time.time() < end:
        now = time.time()
        if win is None and now >= next_tick:
            if role == "hard":
                # Each socket opens its own mapping toward the peer; the
                # spray side is guessing which ports those landed on.
                for i in range(len(socks)):
                    p.send(i, peer)
                next_tick += 0.5
            else:
                p.send(0, peer)
                for src in list(p.heard.values()):  # peer-reflexive
                    if strict and src != peer:
                        p.send(0, src)
                next_tick += PUNCH_INTERVAL
            p.seq += 1
        if win is None and role == "spray" and now >= next_spray:
            for _ in range(args.spray_burst):
                port = random.randint(1024, 65535)
                if port in spray_used:
                    continue
                spray_used.add(port)
                p.send(0, (peer[0], port))
            next_spray += 0.01
        p.poll(0.005, t0)
        if win is None:
            win = p.winner()
            if win is not None:
                win_at = round(time.time() - t0, 3)
                # Like the interface after a punch: stop punching, keep acking
                # (poll still acks), only from here on.

    # Data phase, at an absolute time both sides share: send over the path
    # each side chose, count what arrives from the chosen path.
    data_start = t0 + max(args.window + args.delay, args.window) + 0.5
    while time.time() < data_start:
        time.sleep(0.01)
    rx = 0
    tx = 0
    if win is not None:
        i, path = win
        sock = socks[i]
        sock.setblocking(False)
        dend = time.time() + 2.0
        nxt = time.time()
        while time.time() < dend:
            if time.time() >= nxt and tx < 10:
                try:
                    sock.sendto(b"DATA" + tag, path)
                    tx += 1
                except OSError:
                    pass
                nxt += 0.1
            try:
                data, src = sock.recvfrom(2048)
                if data == b"DATA" + tag and src == path:
                    rx += 1
            except (BlockingIOError, OSError):
                time.sleep(0.005)
    print(json.dumps({
        "name": args.name, "mapped": mapped, "hard": hard, "role": role,
        "ok": win is not None, "at": win_at, "path": f"{win[1][0]}:{win[1][1]}" if win else None,
        "first_heard": p.first_heard, "first_ack": p.first_ack,
        "sprayed": len(spray_used), "sockets": len(socks), "data_rx": rx,
    }))


# --- matrix ---------------------------------------------------------------

SCENARIOS = [
    # name, nat A, nat B, strategy, delay A, delay B
    ("cone/cone baseline", "cone", "cone", "baseline", 0.0, 0.0),
    ("cone/cone baseline, B 1.5 s late", "cone", "cone", "baseline", 0.0, 1.5),
    ("cone/cone strict, B 1.5 s late", "cone", "cone", "strict", 0.0, 1.5),
    ("sym/cone baseline", "sym", "cone", "baseline", 0.0, 0.0),
    ("sym/cone strict", "sym", "cone", "strict", 0.0, 0.0),
    ("sym/cone birthday", "sym", "cone", "birthday", 0.0, 0.0),
    ("sym/sym baseline", "sym", "sym", "baseline", 0.0, 0.0),
    ("sym/sym birthday", "sym", "sym", "birthday", 0.0, 0.0),
]


def matrix(args):
    rows = []
    for name, ka, kb, strategy, da, db in SCENARIOS:
        if args.only and args.only not in name:
            continue
        setup(ka, kb)
        rel = subprocess.Popen(["ip", "netns", "exec", "inet", sys.executable, SELF, "relay"])
        time.sleep(0.3)
        tally = {"both": 0, "one-sided": 0, "none": 0, "data both ways": 0}
        trials = []
        for t in range(args.trials):
            flush_conntrack()
            run = f"{name}#{t}#{random.getrandbits(32)}"
            window = 15.0 if strategy == "birthday" else 10.0
            procs = []
            for side, delay in (("a", da), ("b", db)):
                cmd = ["ip", "netns", "exec", f"h{side.upper()}", sys.executable, SELF, "peer",
                       "--name", side, "--run", run, "--strategy", strategy,
                       "--delay", str(delay), "--window", str(window)]
                procs.append(subprocess.Popen(cmd, stdout=subprocess.PIPE, text=True))
            res = []
            for pr in procs:
                out, _ = pr.communicate(timeout=120)
                try:
                    res.append(json.loads(out.strip().splitlines()[-1]))
                except (ValueError, IndexError):
                    res.append({"ok": False, "error": out})
            oks = [r.get("ok") for r in res]
            verdict = "both" if all(oks) else "one-sided" if any(oks) else "none"
            tally[verdict] += 1
            if all(r.get("data_rx", 0) > 0 for r in res):
                tally["data both ways"] += 1
            trials.append(res)
            print(f"[{name}] trial {t}: {verdict} {json.dumps(res)}", flush=True)
        rel.terminate()
        rows.append({"scenario": name, "trials": args.trials, **tally, "detail": trials})
    teardown()
    with open(args.out, "w") as f:
        json.dump(rows, f, indent=1)
    print("\n| scenario | both | one-sided | none | data both ways |")
    print("|---|---|---|---|---|")
    for r in rows:
        print(f"| {r['scenario']} | {r['both']}/{r['trials']} | {r['one-sided']} | {r['none']} "
              f"| {r['data both ways']}/{r['trials']} |")


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("relay")
    pp = sub.add_parser("peer")
    pp.add_argument("--name", required=True)
    pp.add_argument("--run", required=True)
    pp.add_argument("--strategy", default="baseline",
                    choices=["baseline", "strict", "birthday"])
    pp.add_argument("--delay", type=float, default=0.0)
    pp.add_argument("--window", type=float, default=10.0)
    pp.add_argument("--sockets", type=int, default=256)
    pp.add_argument("--spray-burst", type=int, default=2)
    mp = sub.add_parser("matrix")
    mp.add_argument("--trials", type=int, default=5)
    mp.add_argument("--only", default="")
    mp.add_argument("--out", default="natlab-results.json")
    sub.add_parser("teardown")
    args = ap.parse_args()
    {"relay": relay, "peer": run_peer, "matrix": matrix,
     "teardown": lambda _a: teardown()}[args.cmd](args)


if __name__ == "__main__":
    main()
