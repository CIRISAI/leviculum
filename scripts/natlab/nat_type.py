#!/usr/bin/env python3
"""NAT lab, real nodes (+ciris, leviculum#100): does a two-server probe
classify each NAT correctly, and how does the real Rust punch fare through
it?

Builds two NAT'd hosts, a public relay and a second probe server in Linux
network namespaces, and runs the leviculum-std example `natlab_node` in each
(the real stack: TCP to the relay, a link through it, the direct-link
proposal, the probe of two servers off the session socket, the punch). Needs
root (a CI runner) and the example built:

    cargo build --release -p leviculum-std --example natlab_node
    sudo python3 scripts/natlab/nat_type.py --bin <path to natlab_node> --trials 3

Topology (all addresses private to the lab):

    hA 10.0.1.2 -- rA [NAT] 203.0.113.1 --+
                                          br0 (inet ns) -- relay 203.0.113.10
    hB 10.0.2.2 -- rB [NAT] 203.0.113.2 --+   tcp 4242, facilitator udp 3478
                                          +-- srv2 203.0.113.11, probe udp 3479

NAT kinds, as Linux implements them (filtering is address+port dependent:
conntrack lets only replies in):
    cone  MASQUERADE: endpoint-independent, port-preserving mapping
    sym   MASQUERADE --random-fully: a fresh random port per destination

The WITNESS is the classification: a `cone` host must read
endpoint-independent and a `sym` host endpoint-dependent, every trial. The
punch outcomes are reported beside it; they are lab measurements of these
two Linux NATs, not a model of any real network.
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
import time

RELAY_IP = "203.0.113.10"
SRV2_IP = "203.0.113.11"
TCP_PORT = 4242
FAC_PORT = 3478
SRV2_PORT = 3479
NS = ["inet", "srv2", "rA", "rB", "hA", "hB"]
EXPECTED = {"cone": "endpoint-independent", "sym": "endpoint-dependent"}
SCENARIOS = [("cone", "cone"), ("sym", "cone"), ("cone", "sym"), ("sym", "sym")]


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
    nsx("inet", "ip link add name br0 type bridge")
    nsx("inet", "ip link set br0 up")
    nsx("inet", f"ip addr add {RELAY_IP}/24 dev br0")
    # The second probe server: its own host on the public segment, so the
    # two servers differ in address, not only in port.
    sh("ip link add wan0 netns srv2 type veth peer name pS netns inet")
    nsx("inet", "ip link set pS master br0 up")
    nsx("srv2", f"ip addr add {SRV2_IP}/24 dev wan0")
    nsx("srv2", "ip link set wan0 up")
    for side, wan_ip, lan, kind in (("A", "203.0.113.1", "10.0.1", kind_a),
                                    ("B", "203.0.113.2", "10.0.2", kind_b)):
        r, h = f"r{side}", f"h{side}"
        sh(f"ip link add wan{side} netns {r} type veth peer name p{side} netns inet")
        nsx("inet", f"ip link set p{side} master br0 up")
        nsx(r, f"ip addr add {wan_ip}/24 dev wan{side}")
        nsx(r, f"ip link set wan{side} up")
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


def spawn(ns, args, log):
    return subprocess.Popen(["ip", "netns", "exec", ns] + args, stdout=subprocess.PIPE,
                            stderr=log, text=True)


def trial(binary, timeout, logdir, tag):
    exchange = tempfile.mkdtemp(prefix="natlab-")
    os.chmod(exchange, 0o777)
    common = [f"{RELAY_IP}:{TCP_PORT}", f"{RELAY_IP}:{FAC_PORT}", f"{SRV2_IP}:{SRV2_PORT}",
              exchange, str(timeout)]
    procs = []
    for ns, side in (("hA", "dest"), ("hB", "propose")):
        log = open(os.path.join(logdir, f"{tag}-{side}.log"), "w")
        procs.append(spawn(ns, [binary, "peer", side] + common, log))
    out = []
    for p in procs:
        stdout, _ = p.communicate(timeout=timeout + 30)
        try:
            out.append(json.loads(stdout.strip().splitlines()[-1]))
        except (ValueError, IndexError):
            out.append({"error": f"exit {p.returncode}", "stdout": stdout})
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True, help="the natlab_node example binary")
    ap.add_argument("--trials", type=int, default=3)
    ap.add_argument("--timeout", type=int, default=45)
    ap.add_argument("--out", default="natlab-nat-type.json")
    ap.add_argument("--logs", default="natlab-logs")
    args = ap.parse_args()
    binary = os.path.abspath(args.bin)
    os.makedirs(args.logs, exist_ok=True)
    rows = []
    wrong = 0
    for kind_a, kind_b in SCENARIOS:
        name = f"{kind_a}/{kind_b}"
        setup(kind_a, kind_b)
        servers = [
            spawn("inet", [binary, "relay", RELAY_IP, str(TCP_PORT), str(FAC_PORT)],
                  open(os.path.join(args.logs, f"{kind_a}-{kind_b}-relay.log"), "w")),
            spawn("srv2", [binary, "facilitator", str(SRV2_PORT)],
                  open(os.path.join(args.logs, f"{kind_a}-{kind_b}-srv2.log"), "w")),
        ]
        time.sleep(1.5)
        tally = {"classified": 0, "misclassified": 0, "established": 0, "failed": 0}
        trials = []
        for t in range(args.trials):
            flush_conntrack()
            res = trial(binary, args.timeout, args.logs, f"{kind_a}-{kind_b}-{t}")
            for r, kind in zip(res, (kind_a, kind_b)):
                got = (r.get("nat") or {}).get("mapping")
                if got == EXPECTED[kind]:
                    tally["classified"] += 1
                else:
                    tally["misclassified"] += 1
                    wrong += 1
            ok = [((r.get("outcome") or {}).get("established")) for r in res]
            tally["established" if all(ok) else "failed"] += 1
            trials.append(res)
            print(f"[{name}] trial {t}: {json.dumps(res)}", flush=True)
        for s in servers:
            s.terminate()
            s.wait(timeout=10)
        rows.append({"scenario": name, "trials": args.trials, **tally, "detail": trials})
    teardown()
    with open(args.out, "w") as f:
        json.dump(rows, f, indent=1)
    print("\n| NAT A/B | classified (of 2 per trial) | upgrade established | failed |")
    print("|---|---|---|---|")
    for r in rows:
        print(f"| {r['scenario']} | {r['classified']}/{2 * r['trials']} | "
              f"{r['established']}/{r['trials']} | {r['failed']} |")
    if wrong:
        print(f"\nWITNESS FAILED: {wrong} peer(s) misclassified", file=sys.stderr)
        sys.exit(1)
    print("\nWITNESS: every peer classified as its NAT kind")


if __name__ == "__main__":
    main()
