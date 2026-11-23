#!/usr/bin/env python3
"""Run TLC on Strategy.tla per scenario, and cross-check its oracle.

    python3 docs/direct_link/run_strategy.py OUTDIR [--only SUBSTR]
    python3 docs/direct_link/run_strategy.py OUTDIR --oracle --dlp PATH/DirectLinkPunch.tla
    python3 docs/direct_link/run_strategy.py OUTDIR --vectors

Scenarios: each environment (NAT pair, topology, v6, router mapping,
classification) is run twice, once per planner ("strategy", the design;
"today", the code at diag/punch-telemetry), plus fault, plant and learning
scenarios. TLC runs StrategyMatrix.tla: Strategy plus probes that print
the name of every violated invariant (InvProbe) and action property
(ActProbe, an action constraint), and the outcome of every final state,
without stopping (-continue would print a full trace per violating state).
Termination is checked as a liveness property. Results go to
OUTDIR/strategy.json and OUTDIR/strategy-matrix.md.

--vectors: the planner's (input, expected plan) pairs from every strategy
scenario, via StrategyVectors.tla, to OUTDIR/plan-vectors.jsonl
(CONFORMANCE.md).

--oracle: for all 81 (mapping, filtering) pairs, runs DirectLinkPunch with
escalation denied on both sides (the "plain" technique) through
OracleProbe.tla and compares what each side heard / was acked by / saw on
another port with Strategy.tla's HPub / APub / OtherPortSeen. --dlp names
the DirectLinkPunch.tla to use (it must take HostTrustA/HostTrustB).

TLC: nice -n 19 ionice -c3, -Xmx3g, -workers 2 (shared box).
"""
import collections
import itertools
import json
import os
import re
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
JAR = os.path.expanduser("~/.cache/ciris-kernel/tla2tools-1.7.4.jar")
CAP_SECS = 900

# The probe names every violated invariant (InvProbe) and action property
# (ActProbe, an always-true action constraint) without stopping; the
# liveness property is checked as such.
INVS = "InvProbe RecordOutcome"
ACTC = "ACTION_CONSTRAINT ActProbe"
PROPS = "Termination"

BASE = dict(
    MAPA="EIM", MAPB="EIM", FILTA="APDF", FILTB="APDF", EIMSTEPS="seq",
    V6A="FALSE", V6B="FALSE", V6OPEN="TRUE", TOPO="apart", HAIRPIN="FALSE",
    CGNCAND="FALSE", UPNPA="FALSE", UPNPB="FALSE", FACA="TRUE", FACB="TRUE",
    CLASSIFY="TRUE", FILTKNOWN="FALSE", LUCK="any", ESCA="TRUE", ESCB="TRUE",
    BUDGET="3", STRANGER="FALSE", HIJACK="FALSE", NAMED="FALSE", RLOSS="FALSE",
    NLOSS="FALSE", PATHLOSS="FALSE", PLANNER="strategy", UNTAGGED="FALSE",
    MAXATT="2", INVS=INVS, PROPS=PROPS, ACTC=ACTC,
    HIST="3", ANONA="FALSE", ANONB="FALSE", SHAPED="FALSE", HELLOA="FALSE", HELLOB="FALSE", DOWNGRADE="FALSE", ONPATH="FALSE",
    LANSTRANGER="FALSE", PORTBLOCK="FALSE", TRANCHES="1",
)
MAPS = ["EIM", "EDMseq", "EDMrand"]
FILTS = ["EIF", "ADF", "APDF"]


def environments():
    """(name, params) per environment; each runs under both planners."""
    # NAT pairs: every mapping pair, behind the commonest filtering (APDF)
    # and with the EIM side's filter loosened, classified and not.
    for ma, mb in itertools.product(MAPS, MAPS):
        for fa, fb in [("APDF", "APDF"), ("ADF", "APDF"), ("APDF", "EIF")]:
            for cl in ["TRUE", "FALSE"]:
                yield (f"pair-{ma}-{fa}--{mb}-{fb}-cls{cl[0]}",
                       dict(MAPA=ma, FILTA=fa, MAPB=mb, FILTB=fb, CLASSIFY=cl))
    hard = dict(MAPA="EIM", MAPB="EDMrand")
    both = dict(MAPA="EDMrand", MAPB="EDMrand")
    seq = dict(MAPA="EIM", MAPB="EDMseq")
    # Same network.
    yield "samelan-hairpin", dict(TOPO="sameLAN", HAIRPIN="TRUE")
    yield "samelan-nohairpin", dict(TOPO="sameLAN")
    yield "samecgn-nocand-hairpin", dict(TOPO="sameCGN", HAIRPIN="TRUE")
    yield "samecgn-nocand-nohairpin", dict(TOPO="sameCGN")
    yield "samecgn-cand", dict(TOPO="sameCGN", CGNCAND="TRUE", HAIRPIN="TRUE")
    # IPv6.
    yield "v6-both", dict(V6A="TRUE", V6B="TRUE")
    yield "v6-both-hard", dict(hard, V6A="TRUE", V6B="TRUE")
    yield "v6-both-closed-hard", dict(hard, V6A="TRUE", V6B="TRUE", V6OPEN="FALSE")
    yield "v6-one-side", dict(V6B="TRUE")
    yield "v6-both-bothrandom", dict(both, V6A="TRUE", V6B="TRUE")
    # Router mapping.
    yield "upnp-hard-side", dict(hard, UPNPB="TRUE")
    yield "upnp-easy-side", dict(hard, UPNPA="TRUE")
    yield "upnp-bothrandom", dict(both, UPNPA="TRUE")
    # Both hard, nothing else: no direct path, at once.
    yield "bothrandom-nothing", dict(both)
    yield "seq-vs-random", dict(MAPA="EDMseq", MAPB="EDMrand")
    # Classification and secondary evidence.
    yield "hard-filtknown", dict(hard, FILTKNOWN="TRUE")
    yield "hard-unclassified-eif", dict(hard, CLASSIFY="FALSE", FILTB="EIF")
    yield "hard-unclassified-eimrandalloc", dict(hard, CLASSIFY="FALSE", EIMSTEPS="rand")
    yield "seq-unclassified-eif", dict(seq, CLASSIFY="FALSE", FILTB="EIF")
    # Facilitator silence.
    yield "fac-silent-a-v6", dict(FACA="FALSE", V6A="TRUE", V6B="TRUE")
    yield "fac-silent-a-upnp", dict(hard, FACA="FALSE", UPNPB="TRUE")
    yield "fac-silent-b", dict(FACB="FALSE")
    # The gate and the budget.
    yield "gate-deny-b-hard", dict(hard, ESCB="FALSE")
    yield "gate-deny-a-seq", dict(seq, ESCA="FALSE")
    yield "budget-one-hard", dict(hard, BUDGET="1", MAXATT="2")
    yield "budget-zero-hard", dict(hard, BUDGET="0")
    # Connection reversal (RFC 5128 3.2): one side reachable by anyone.
    yield "reversal-eim-eif-known", dict(MAPA="EIM", FILTA="EIF", MAPB="EDMrand", FILTKNOWN="TRUE")
    # Frame authentication (rns-rs >= 0.14 wire, negotiated by HELLO).
    mac = dict(HELLOA="TRUE", HELLOB="TRUE")
    yield "seqseq-raw", dict(MAPA="EDMseq", MAPB="EDMseq", FILTA="APDF", FILTB="ADF")
    yield "seqseq-mac", dict(mac, MAPA="EDMseq", MAPB="EDMseq", FILTA="APDF", FILTB="ADF")
    yield "seqseq-mac-bothstrict", dict(mac, MAPA="EDMseq", MAPB="EDMseq")
    yield "bothrandom-mac-reachable", dict(mac, MAPA="EDMrand", MAPB="EDMrand")
    yield "hello-one-side", dict(HELLOA="TRUE", MAPB="EDMrand")
    yield "lan-leak-cgn-raw", dict(TOPO="sameCGN", HAIRPIN="TRUE", LANSTRANGER="TRUE")
    yield "lan-leak-cgn-mac", dict(mac, TOPO="sameCGN", HAIRPIN="TRUE", LANSTRANGER="TRUE")
    yield "replay-onpath-raw", dict(ONPATH="TRUE", PATHLOSS="TRUE", MAXATT="1")
    yield "replay-onpath-mac", dict(mac, ONPATH="TRUE", PATHLOSS="TRUE", MAXATT="1")
    # Birthday tranches across attempts (64 -> 128 -> 256 sockets x 1,750).
    yield "tranches-hard", dict(hard, TRANCHES="3", MAXATT="3", BUDGET="3")
    yield "tranches-hard-budget2", dict(hard, TRANCHES="3", MAXATT="3", BUDGET="2")
    # D4: ambiguous own evidence, trusted (both gates) and untrusted.
    yield "ambiguous-trusted", dict(hard, CLASSIFY="FALSE", EIMSTEPS="rand")
    yield "ambiguous-untrusted", dict(hard, CLASSIFY="FALSE", EIMSTEPS="rand", ESCA="FALSE")
    # R0 and D7 (CC 5.4.6, CC 1.13.3.1).
    yield "anon-initiator", dict(hard, ANONA="TRUE")
    yield "anon-responder", dict(ANONB="TRUE", V6A="TRUE", V6B="TRUE")
    yield "shaped-hard", dict(hard, SHAPED="TRUE")
    yield "portblock-hard", dict(hard, PORTBLOCK="TRUE")


def scenarios():
    for name, env in environments():
        for planner in ["strategy", "today"]:
            yield f"{planner}--{name}", dict(env, PLANNER=planner)
    hard = dict(MAPA="EIM", MAPB="EDMrand")
    # Faults (strategy; today's fault behaviour is DirectLinkPunch's).
    for planner in ["strategy", "today"]:
        yield f"{planner}--fault-relay-lossy", dict(PLANNER=planner, RLOSS="TRUE", MAXATT="1")
        yield f"{planner}--fault-relay-lossy-hard", dict(hard, PLANNER=planner, RLOSS="TRUE", MAXATT="1")
        yield f"{planner}--fault-net-lossy", dict(PLANNER=planner, NLOSS="TRUE", MAXATT="1")
        yield f"{planner}--fault-net-lossy-hard", dict(hard, PLANNER=planner, NLOSS="TRUE", MAXATT="1")
        yield f"{planner}--learn-pathloss", dict(PLANNER=planner, PATHLOSS="TRUE", MAXATT="2")
        yield f"{planner}--learn-pathloss-hard", dict(hard, PLANNER=planner, PATHLOSS="TRUE", MAXATT="2")
        yield f"{planner}--learn-three-attempts-hard", dict(hard, PLANNER=planner, MAXATT="3")
        yield f"{planner}--stranger-hard", dict(hard, PLANNER=planner, STRANGER="TRUE")
        # Plants: each must violate its property.
        yield f"{planner}--plant-hijack", dict(hard, PLANNER=planner, STRANGER="TRUE", HIJACK="TRUE", LUCK="miss")
        yield f"{planner}--plant-named-facilitator", dict(PLANNER=planner, NAMED="TRUE")
    yield "strategy--plant-untagged", dict(hard, UNTAGGED="TRUE", MAXATT="2", RLOSS="TRUE")
    yield "strategy--plant-downgrade", dict(HELLOA="TRUE", HELLOB="TRUE", DOWNGRADE="TRUE", ONPATH="TRUE", PATHLOSS="TRUE", MAXATT="1")


def cfg_text(params):
    p = dict(BASE, **params)
    text = open(os.path.join(HERE, "strategy.cfg.tmpl")).read()
    for k, v in p.items():
        text = text.replace(f"@{k}@", v)
    assert "@" not in text.replace("\\*", ""), text
    return text


def tlc(out, name, cfg_body, module):
    cfg = os.path.join(out, f"{name}.cfg")
    log = os.path.join(out, f"{name}.log")
    with open(cfg, "w") as f:
        f.write(cfg_body)
    meta = os.path.join(out, f"st_{name}")
    cmd = ["timeout", str(CAP_SECS), "nice", "-n", "19", "ionice", "-c3", "java", "-Xmx3g",
           "-XX:+UseParallelGC", "-cp", JAR, "tlc2.TLC", "-workers", "2",
           "-metadir", meta, "-config", cfg, module]
    with open(log, "w") as f:
        rc = subprocess.run(cmd, cwd=out, stdout=f, stderr=subprocess.STDOUT).returncode
    shutil.rmtree(meta, ignore_errors=True)
    return rc, open(log).read()


def parse(text, tag):
    viol = set(re.findall(r'<<\s*"VIOLATED",\s*"(\w+)"\s*>>', text))
    viol |= set(re.findall(r"(?:Invariant|Action property) (\w+) is violated", text))
    if re.search(r"Temporal properties were violated", text):
        viol.add("Termination")
    viol = sorted(viol)
    m = re.findall(r"([\d,]+) states generated, ([\d,]+) distinct states found", text)
    states = int(m[-1][1].replace(",", "")) if m else 0
    complete = "Model checking completed" in text or "No error has been found" in text \
        or (bool(m) and re.search(r"\b0 states left on queue", text) is not None)
    outs = collections.Counter(" ".join(x.split()) for x in
                               re.findall(r'<<\s*"' + tag + r'",(.*?)>>\s*\n(?=<<|\S)', text, re.S))
    errors = re.findall(r"^Error: (?!Invariant|Action property|The behavior|The error|Temporal|The following)(.*)$",
                        text, re.M)
    return viol, states, complete, outs, errors


def outcome_summary(outs):
    agg = collections.Counter()
    for k, v in outs.items():
        first = k.split(",")[0].strip().strip('"')
        agg[first] += v
    return dict(agg)


def run_matrix(out, only):
    shutil.copy(os.path.join(HERE, "Strategy.tla"), out)
    shutil.copy(os.path.join(HERE, "StrategyMatrix.tla"), out)
    results = {}
    path = os.path.join(out, "strategy.json")
    if os.path.exists(path):
        results = json.load(open(path))
    for name, params in scenarios():
        if only and only not in name:
            continue
        if name in results and not only:
            continue
        rc, text = tlc(out, name, cfg_text(params), "StrategyMatrix.tla")
        viol, states, complete, outs, errors = parse(text, "OUTCOME")
        results[name] = dict(params=params, violated=viol, states=states, complete=complete,
                             outcomes=outcome_summary(outs), errors=errors[:3], rc=rc)
        print(f"{name}: {states} states, {'complete' if complete else 'PARTIAL'}, "
              f"violated={viol or '-'} outcomes={outcome_summary(outs)} {errors[:1]}", flush=True)
        json.dump(results, open(path, "w"), indent=1, sort_keys=True)
    write_md(out, results)


def write_md(out, results):
    lines = ["| scenario | outcomes (final states) | properties violated | states |",
             "|---|---|---|---|"]
    for name in sorted(results):
        r = results[name]
        oc = ", ".join(f"{k} x{v}" for k, v in sorted(r["outcomes"].items())) or "-"
        lines.append(f"| {name} | {oc} | {', '.join(r['violated']) or 'none'} | "
                     f"{r['states']}{'' if r['complete'] else ' PARTIAL'} |")
    open(os.path.join(out, "strategy-matrix.md"), "w").write("\n".join(lines) + "\n")


# ---- the oracle cross-check ----

def eim(m):
    return m == "EIM"


def loose(f):
    return f in ("EIF", "ADF")


def open_edm(s, M, F):
    return not eim(M[s]) and F[s] == "EIF"


def eff_eim(s, M, F, race):
    # Strategy.tla EffEIM: an EDM side with endpoint-independent filtering
    # answers from the port a peer's packet arrived at (always toward an EDM
    # peer; toward an EIM peer only if that packet won the race).
    o = 1 - s
    return eim(M[s]) or (open_edm(s, M, F) and (not eim(M[o]) or race == "reuse"))


def hpub(s, M, F, race):
    o = 1 - s
    es, eo = eff_eim(s, M, F, race), eff_eim(o, M, F, race)
    if es and eo:
        return True
    if es:
        return loose(F[s])
    if eo:
        return loose(F[o]) or F[s] == "EIF"
    return False


def apub(s, M, F, race):
    o = 1 - s
    if eff_eim(o, M, F, race):
        return hpub(o, M, F, race)
    if eff_eim(s, M, F, race):
        return hpub(o, M, F, race) and hpub(s, M, F, race)
    return False


def oracle_states(M, F):
    races = ["reuse", "new"] if any(open_edm(s, M, F) and eim(M[1 - s]) for s in (0, 1)) else ["new"]
    out = set()
    for r in races:
        works = all(hpub(s, M, F, r) and apub(s, M, F, r) for s in (0, 1))
        side = lambda s: (hpub(s, M, F, r), hpub(s, M, F, r) and apub(s, M, F, r), apub(s, M, F, r),
                          hpub(s, M, F, r) and not eff_eim(1 - s, M, F, r))
        out.add(("direct" if works else "relayed", side(0), side(1)))
    return out


def dlp_ack_artifact(state):
    """DirectLinkPunch sends an ack from the address the punch ARRIVED at
    (Deliver: Ack(d) has src = d.dst) even when the socket already holds a
    mapping toward that peer; a NAT sends one socket's datagrams to one
    destination through one mapping. Where that differs, a side is 'acked
    but never heard' in the model only. Normalise it away for comparison."""
    o, a, b = state
    fix = lambda x: (x[0], x[1], x[2] and x[0], x[3]) if o == "relayed" else x
    return (o, fix(a), fix(b))


def run_oracle(out, dlp):
    shutil.copy(dlp, os.path.join(out, "DirectLinkPunch.tla"))
    shutil.copy(os.path.join(HERE, "OracleProbe.tla"), out)
    tmpl = os.path.join(os.path.dirname(os.path.abspath(dlp)), "base.cfg.tmpl")
    base = open(tmpl).read()
    p0 = dict(MAPA="EIM", MAPB="EIM", FILTA="APDF", FILTB="APDF", SAMENAT="FALSE",
              HAIRPIN="FALSE", FACA="F1", FACB="F1", FAC="TRUE", FORGED="FALSE", OBS="2",
              POOL="paired", STRANGER="FALSE", POLICY="ask_host", HOSTA="deny", HOSTB="deny",
              IDENT="TRUE", GLARE="FALSE", BINIT="FALSE", RLOSS="FALSE", RDUP="FALSE",
              NLOSS="FALSE", TEAR="FALSE", KEEP="TRUE", HIJACK="FALSE", NAMED="FALSE",
              HITS="any", BUDGET="TRUE", NP="4", W="6", K="1", KF="1", SKEW="0", LAT="0", MAC="FALSE",
              INVS="ProbeOracle", PROPS="")
    rows, bad = [], 0
    for ma, fa, mb, fb in itertools.product(MAPS, FILTS, MAPS, FILTS):
        p = dict(p0, MAPA=ma, FILTA=fa, MAPB=mb, FILTB=fb)
        text = base
        for k, v in p.items():
            text = text.replace(f"@{k}@", v)
        name = f"oracle-{ma}-{fa}--{mb}-{fb}"
        rc, log = tlc(out, name, text, "OracleProbe.tla")
        seen = set()
        for m in re.finditer(r'<<\s*"ORACLE",\s*"(\w+)",\s*<<(.*?)>>,\s*<<(.*?)>>\s*>>', log, re.S):
            a = tuple(x.strip() == "TRUE" for x in m.group(2).split(","))
            b = tuple(x.strip() == "TRUE" for x in m.group(3).split(","))
            seen.add((m.group(1), a, b))
        M, F = (ma, mb), (fa, fb)
        pred = oracle_states(M, F)
        ok = seen == pred
        norm = {dlp_ack_artifact(x) for x in seen} == pred
        # Both sides EDM with open filtering: the outcome is the oracle's;
        # whether a side ALSO saw the other's port move is a race (each
        # side's first punch may make a new mapping the peer admits).
        both_open = all(open_edm(s, M, F) for s in (0, 1))
        drop_op = lambda st: (st[0], st[1][:3], st[2][:3])
        race_obs = both_open and {drop_op(x) for x in seen} == {drop_op(x) for x in pred}
        bad += not (norm or race_obs)
        rows.append((name, sorted(seen), sorted(pred),
                     "yes" if ok else ("yes, after the ack artifact" if norm else
                                       ("outcome yes; other-port seen is a race" if race_obs else "NO"))))
        print(f"{name}: {rows[-1][3]} dlp={sorted(seen)} oracle={sorted(pred)}", flush=True)
    with open(os.path.join(out, "oracle.md"), "w") as f:
        f.write("| pair (A--B) | DirectLinkPunch terminal states (outcome, A seen, B seen) | oracle | agree |\n|---|---|---|---|\n")
        for name, seen, pred, ok in rows:
            f.write(f"| {name[7:]} | {seen} | {pred} | {ok} |\n")
    exact = sum(1 for r in rows if r[3] == "yes")
    print(f"{len(rows) - bad}/{len(rows)} pairs agree ({exact} exactly)")


def parse_tla_value(v):
    v = v.strip()
    if v in ("TRUE", "FALSE"):
        return v == "TRUE"
    if v.startswith('"'):
        return v.strip('"')
    if v.startswith("{"):
        inner = v[1:-1].strip()
        return sorted(x.strip().strip('"') for x in inner.split(",")) if inner else []
    try:
        return int(v)
    except ValueError:
        return v


def run_vectors(out):
    """Every distinct (planner input, expected plan) the strategy scenarios
    reach, as JSON lines (CONFORMANCE.md)."""
    for f in ("Strategy.tla", "StrategyVectors.tla"):
        shutil.copy(os.path.join(HERE, f), out)
    vectors = {}
    for name, params in scenarios():
        if not name.startswith("strategy--"):
            continue
        p = dict(params, INVS="RecordVector", PROPS="", ACTC="")
        rc, text = tlc(out, "vec-" + name, cfg_text(p), "StrategyVectors.tla")
        for m in re.finditer(r'<<\s*"VECTOR",\s*\[(.*?)\]\s*>>', text, re.S):
            body = " ".join(m.group(1).split())
            rec = {}
            for part in re.finditer(r'(\w+) \|-> ("[^"]*"|\{[^}]*\}|\w+)', body):
                rec[part.group(1)] = parse_tla_value(part.group(2))
            vectors[json.dumps(rec, sort_keys=True)] = rec
        print(f"{name}: {len(vectors)} distinct vectors so far", flush=True)
    with open(os.path.join(out, "plan-vectors.jsonl"), "w") as f:
        for k in sorted(vectors):
            f.write(k + "\n")
    print(f"{len(vectors)} distinct vectors")


def main():
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    if "--oracle" in sys.argv:
        run_oracle(out, sys.argv[sys.argv.index("--dlp") + 1])
        return
    if "--vectors" in sys.argv:
        run_vectors(out)
        return
    only = sys.argv[sys.argv.index("--only") + 1] if "--only" in sys.argv else None
    run_matrix(out, only)


if __name__ == "__main__":
    main()
