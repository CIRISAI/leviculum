#!/usr/bin/env python3
"""Run TLC on DirectLinkPunch.tla for each NAT pair and scenario, and tabulate
the outcomes the model can reach.

    python3 docs/direct_link/run_matrix.py OUTDIR [--only NAME]

For each scenario this writes OUTDIR/<name>.cfg, runs TLC with `-dump` (every
reachable state), and classifies the states where the attempt is over:
Established (both Connected), a clean abort (both Failed / Gone, with each
side's failure), or BUG (anything else: one side Connected while the other
failed, or Connected on different paths, which the invariants also flag).
The summary goes to OUTDIR/matrix.json and OUTDIR/matrix.md.

TLC: nice -n 19 ionice -c3, -Xmx4g, -workers 2 (shared box).
"""
import itertools
import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
JAR = os.path.expanduser("~/.cache/ciris-kernel/tla2tools-1.7.4.jar")

BASE = dict(
    MAPA="EIM", MAPB="EIM", FILTA="APDF", FILTB="APDF", SAMENAT="FALSE",
    HAIRPIN="FALSE", FAC="TRUE", POOL="paired", OBS="1", POLICY="accept",
    GLARE="FALSE", BINIT="FALSE", RLOSS="FALSE", RDUP="FALSE", NLOSS="FALSE",
    TEAR="FALSE", KEEP="TRUE", HITS="any", NP="3", W="3", SKEW="1", K="1",
    LAT="0", PROPS="EventuallyOver",
)

MAPS = ["EIM", "EDMseq", "EDMrand"]
FILTS = ["EIF", "ADF", "APDF"]


def scenarios():
    # The NAT-pair matrix: A (initiator) x B (responder).
    for ma, fa, mb, fb in itertools.product(MAPS, FILTS, MAPS, FILTS):
        yield f"pair-{ma}-{fa}--{mb}-{fb}", dict(MAPA=ma, FILTA=fa, MAPB=mb, FILTB=fb)
    # Edge scenarios on the hard pair (Eric's NAT against a random EDM one)
    # and on an easy one.
    hard = dict(MAPA="EIM", FILTA="APDF", MAPB="EDMrand", FILTB="APDF")
    easy = dict(MAPA="EIM", FILTA="APDF", MAPB="EIM", FILTB="APDF")
    yield "hard-spray-hit", dict(hard, HITS="hit")
    yield "hard-spray-miss", dict(hard, HITS="miss")
    yield "hard-latency", dict(hard, LAT="1", W="4")
    yield "easy-glare-a-wins", dict(easy, GLARE="TRUE", BINIT="FALSE")
    yield "easy-glare-b-wins", dict(easy, GLARE="TRUE", BINIT="TRUE")
    yield "easy-policy-reject", dict(easy, POLICY="reject")
    yield "easy-no-facilitator", dict(easy, FAC="FALSE")
    yield "easy-relay-lossy", dict(easy, RLOSS="TRUE")
    yield "easy-relay-dup", dict(easy, RDUP="TRUE")
    yield "easy-net-lossy", dict(easy, NLOSS="TRUE")
    yield "easy-teardown", dict(easy, TEAR="TRUE")
    yield "easy-keepalive-lost", dict(easy, KEEP="FALSE")
    yield "samenat-hairpin", dict(easy, SAMENAT="TRUE", HAIRPIN="TRUE")
    yield "samenat-no-hairpin", dict(easy, SAMENAT="TRUE", HAIRPIN="FALSE")
    yield "samenat-hard-no-hairpin", dict(hard, SAMENAT="TRUE", HAIRPIN="FALSE")
    yield "hard-pooling-arbitrary", dict(hard, POOL="arbitrary")
    yield "seq-pooling-arbitrary", dict(MAPA="EIM", FILTA="APDF", MAPB="EDMseq", FILTB="APDF", POOL="arbitrary")
    yield "skew-2", dict(hard, SKEW="2")


def cfg_text(params):
    p = dict(BASE, **params)
    text = open(os.path.join(HERE, "base.cfg.tmpl")).read()
    for k, v in p.items():
        text = text.replace(f"@{k}@", v)
    assert "@" not in text.replace("\\*", ""), text
    return text


PHASE = re.compile(r'phase = \[A \|-> "(\w+)", B \|-> "(\w+)"\]')
FAIL = re.compile(r'failure = \[A \|-> "([^"]+)", B \|-> "([^"]+)"\]')


def states(dump):
    block = []
    for line in open(dump):
        if line.startswith("State ") and block:
            yield "".join(block)
            block = []
        block.append(line)
    if block:
        yield "".join(block)


def classify(text):
    m = PHASE.search(text)
    if not m:
        return None
    a, b = m.groups()
    over = {"Connected", "Failed", "Gone", "Idle"}
    if a not in over or b not in over:
        return None
    if a == "Idle" and b == "Idle":
        return None
    f = FAIL.search(text)
    fa, fb = f.groups() if f else ("-", "-")
    if a == b == "Connected":
        return "Established"
    if "Connected" not in (a, b):
        return f"abort(A:{fa if a != 'Gone' else 'Gone'}, B:{fb if b != 'Gone' else 'Gone'})"
    return f"BUG({a}/{b})"


def run(outdir, name, params):
    cfg = os.path.join(outdir, name + ".cfg")
    open(cfg, "w").write(cfg_text(params))
    dump = os.path.join(outdir, name + ".dump")
    log = os.path.join(outdir, name + ".log")
    cmd = ["timeout", "1500", "nice", "-n", "19", "ionice", "-c3", "java",
           "-Xmx4g", "-XX:+UseParallelGC", "-cp", JAR, "tlc2.TLC", "-workers", "2",
           "-config", cfg, "-dump", dump, "-metadir", os.path.join(outdir, name + ".states"),
           os.path.join(outdir, "DirectLinkPunch.tla")]
    with open(log, "w") as fh:
        subprocess.run(cmd, stdout=fh, stderr=subprocess.STDOUT, cwd=outdir)
    logtext = open(log).read()
    errors = re.findall(r"Error: (.*)", logtext)
    distinct = re.findall(r"(\d+) distinct states found", logtext)
    outcomes = {}
    if os.path.exists(dump + ".dump") or os.path.exists(dump):
        path = dump + ".dump" if os.path.exists(dump + ".dump") else dump
        for st in states(path):
            c = classify(st)
            if c:
                outcomes[c] = outcomes.get(c, 0) + 1
        os.remove(path)
    return dict(errors=errors, distinct=int(distinct[-1]) if distinct else None,
                outcomes=outcomes, params=params)


def main():
    outdir = sys.argv[1]
    only = sys.argv[3] if len(sys.argv) > 3 and sys.argv[2] == "--only" else None
    os.makedirs(outdir, exist_ok=True)
    subprocess.run(["cp", os.path.join(HERE, "DirectLinkPunch.tla"), outdir], check=True)
    resfile = os.path.join(outdir, "matrix.json")
    results = json.load(open(resfile)) if os.path.exists(resfile) else {}
    for name, params in scenarios():
        if only and not re.fullmatch(only, name):
            continue
        if name in results and not only:
            continue
        results[name] = run(outdir, name, params)
        json.dump(results, open(resfile, "w"), indent=1)
        r = results[name]
        print(name, r["distinct"], r["outcomes"], r["errors"][:2], flush=True)


if __name__ == "__main__":
    main()
