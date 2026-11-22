#!/usr/bin/env python3
"""Run TLC on DirectLinkPunch.tla for each NAT pair and scenario, and tabulate
the outcomes the model can reach.

    python3 docs/direct_link/run_matrix.py OUTDIR [--only NAME]

For each scenario this writes OUTDIR/<name>.cfg and runs TLC on Matrix.tla
(DirectLinkPunch plus an outcome probe) with -continue, so every violated
invariant is listed, not just the first. The outcome probe prints the
outcome of each terminal state: DirectPathUp, RelayedOnly(A:..,B:..) with
each side's failure, LinkClosed, or BUG (anything else). The summary goes
to OUTDIR/matrix.json.

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
    HAIRPIN="FALSE", FACA="F1", FACB="F1", FAC="TRUE", FORGED="FALSE", OBS="2",
    POOL="paired", STRANGER="FALSE", POLICY="ask_host", GLARE="FALSE",
    BINIT="FALSE", RLOSS="FALSE", RDUP="FALSE", NLOSS="FALSE", TEAR="FALSE",
    KEEP="TRUE", HIJACK="FALSE", NAMED="FALSE", HITS="any", BUDGET="TRUE",
    NP="4", W="6", K="1", SKEW="0", LAT="0",
    INVS="TypeOK CapsHeld PathHeardAndAcked SamePath ForgedReflexiveNeverWrongDirect "
         "NoLoneMove AbortLeavesLinkUnchanged FacilitatorCarriesNothing FacilitatorKnowledge "
         "StrangerKnowledge NoTokenToStranger NoPathToStranger NoLearnedAddressPersisted "
         "NotStuck RecordOutcome",
    PROPS="EventuallyOver LinkPathTransitions",
)

MAPS = ["EIM", "EDMseq", "EDMrand"]
# A run stopped at the cap is PARTIAL: the outcomes it reached and any
# violation it found are real; "no violation" covers the states explored.
CAP_SECS = 420
FILTS = ["EIF", "ADF", "APDF"]


def scenarios():
    # The NAT-pair matrix: A (initiator) x B (responder), two observers.
    # A: the three EIM filterings, and EDM-seq / EDM-random behind Eric's
    # filtering (APDF); B: all nine. (The full 81 at this scale take more
    # than a day on a shared box; see "Matrix scope" in DirectLinkPunch.md.)
    a_sides = [("EIM", f) for f in FILTS] + [("EDMseq", "APDF"), ("EDMrand", "APDF")]
    for (ma, fa), (mb, fb) in itertools.product(a_sides, itertools.product(MAPS, FILTS)):
        yield f"pair-{ma}-{fa}--{mb}-{fb}", dict(MAPA=ma, FILTA=fa, MAPB=mb, FILTB=fb)
    hard = dict(MAPA="EIM", FILTA="APDF", MAPB="EDMrand", FILTB="APDF")
    seq = dict(MAPA="EIM", FILTA="APDF", MAPB="EDMseq", FILTB="APDF")
    easy = dict(MAPA="EIM", FILTA="APDF", MAPB="EIM", FILTB="APDF")
    # Observers 1..4 (review item 6: one observer cannot classify).
    for n in ["1", "2", "3", "4"]:
        yield f"observers-{n}-hard", dict(hard, OBS=n)
        yield f"observers-{n}-seq", dict(seq, OBS=n)
    yield "hard-spray-hit", dict(hard, HITS="hit")
    yield "hard-spray-miss", dict(hard, HITS="miss")
    yield "hard-budget-refused", dict(hard, BUDGET="FALSE")
    yield "hard-stranger", dict(hard, STRANGER="TRUE")
    yield "hard-latency", dict(hard, LAT="1")
    yield "hard-pooling-arbitrary", dict(hard, POOL="arbitrary")
    yield "seq-pooling-arbitrary", dict(seq, POOL="arbitrary")
    yield "hard-accept-all", dict(hard, POLICY="accept_all")
    yield "forged-probe", dict(easy, FORGED="TRUE")
    yield "forged-probe-hard", dict(hard, FORGED="TRUE")
    yield "easy-glare-a-wins", dict(easy, GLARE="TRUE", BINIT="FALSE")
    yield "easy-glare-b-wins", dict(easy, GLARE="TRUE", BINIT="TRUE")
    yield "easy-policy-reject", dict(easy, POLICY="reject")
    yield "easy-no-facilitator-answer", dict(easy, FAC="FALSE")
    yield "easy-relay-lossy", dict(easy, RLOSS="TRUE")
    yield "easy-relay-dup", dict(easy, RDUP="TRUE")
    yield "easy-net-lossy", dict(easy, NLOSS="TRUE")
    yield "easy-teardown", dict(easy, TEAR="TRUE")
    yield "easy-keepalive-lost", dict(easy, KEEP="FALSE")
    yield "easy-own-facilitators-differ", dict(easy, FACB="F2")
    yield "samenat-hairpin", dict(easy, SAMENAT="TRUE", HAIRPIN="TRUE")
    yield "samenat-no-hairpin", dict(easy, SAMENAT="TRUE", HAIRPIN="FALSE")
    yield "samenat-hard-no-hairpin", dict(hard, SAMENAT="TRUE", HAIRPIN="FALSE")
    yield "skew-1", dict(hard, SKEW="1")
    yield "skew-1-seq", dict(seq, SKEW="1")
    # The plants: each must violate its property.
    yield "plant-hijack", dict(hard, STRANGER="TRUE", HIJACK="TRUE")
    yield "plant-named-facilitator", dict(easy, FACB="F2", NAMED="TRUE")


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
    log = os.path.join(outdir, name + ".log")
    cmd = ["timeout", str(CAP_SECS), "nice", "-n", "19", "ionice", "-c3", "java",
           "-Xmx4g", "-XX:+UseParallelGC", "-cp", JAR, "tlc2.TLC", "-workers", "2",
           "-continue", "-config", cfg,
           "-metadir", os.path.join(outdir, name + ".states"),
           os.path.join(outdir, "Matrix.tla")]
    with open(log, "w") as fh:
        subprocess.run(cmd, stdout=fh, stderr=subprocess.STDOUT, cwd=outdir)
    subprocess.run(["rm", "-rf", os.path.join(outdir, name + ".states")])
    return parse_log(log, params)


def parse_log(log, params):
    logtext = open(log).read()
    violated = sorted(set(re.findall(r"Invariant (\w+) is violated", logtext)))
    temporal = "Temporal properties were violated" in logtext
    distinct = [d.replace(",", "") for d in re.findall(r"([\d,]+) distinct states found", logtext)]
    outcomes = {}
    for m in re.findall(r'<<"OUTCOME", "([^"]+)">>', logtext):
        outcomes[m] = outcomes.get(m, 0) + 1
    finished = "Finished in" in logtext
    return dict(violated=violated, temporal=temporal, finished=finished,
                distinct=int(distinct[-1]) if distinct else None,
                outcomes=outcomes, params=params)


def main():
    outdir = sys.argv[1]
    if len(sys.argv) > 2 and sys.argv[2] == "--reparse":
        # Rebuild matrix.json / matrix.md from the TLC logs alone.
        params = dict(scenarios())
        results = {}
        for name, p in scenarios():
            log = os.path.join(outdir, name + ".log")
            if os.path.exists(log):
                results[name] = parse_log(log, p)
        json.dump(results, open(os.path.join(outdir, "matrix.json"), "w"), indent=1)
        open(os.path.join(outdir, "matrix.md"), "w").write(table(results))
        print(len(results), "scenarios reparsed")
        return
    only = sys.argv[3] if len(sys.argv) > 3 and sys.argv[2] == "--only" else None
    os.makedirs(outdir, exist_ok=True)
    for f in ["DirectLinkPunch.tla", "Matrix.tla"]:
        subprocess.run(["cp", os.path.join(HERE, f), outdir], check=True)
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
        print(name, r["distinct"], r["outcomes"], r["violated"], "temporal" if r["temporal"] else "", flush=True)
    open(os.path.join(outdir, "matrix.md"), "w").write(table(results))


def table(results):
    """matrix.md: one row per scenario."""
    out = ["| scenario | outcomes reached (terminal states) | invariants violated | liveness | run |",
           "|---|---|---|---|---|"]
    for name, r in results.items():
        oc = ", ".join(f"{k} ×{v}" for k, v in sorted(r["outcomes"].items())) or "-"
        out.append(f"| {name} | {oc} | {', '.join(r['violated']) or 'none'} | "
                   f"{'VIOLATED' if r['temporal'] else 'ok'} | "
                   f"{'complete' if r['finished'] else 'PARTIAL (capped)'}, {r['distinct']} states |")
    return "\n".join(out) + "\n"


if __name__ == "__main__":
    main()
