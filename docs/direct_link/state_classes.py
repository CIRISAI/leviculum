#!/usr/bin/env python3
"""Group the reachable states of a TLC `-dump dot,actionlabels` graph into
classes and write the state x event table (markdown) for DirectLinkPunch.

    python3 docs/direct_link/state_classes.py GRAPH.dot SCENARIO > table.md

A class is: both sides' phase and linkPath, what each believes its path is,
whether each has heard / been acked (the one-way and asymmetric cases), the
signals in flight to each, and the escalation / spray / birthday flags. For
each class: the number of states, the transitions in and out (by action,
to which class), whether every state in it can still reach a terminal state
(no stuck class), and its verdict: LEGAL-TERMINAL (DirectPathUp /
RelayedOnly / LinkClosed), LEGAL-TRANSIENT, or BUG (a terminal state the
invariants reject, or a class with a state that cannot reach a terminal).
"""
import collections
import re
import sys

NODE = re.compile(r'^(-?\d+) \[label="((?:[^"\\]|\\.)*)"', re.S)
EDGE = re.compile(r'^(-?\d+) -> (-?\d+) \[label="([^"]*)"')


def unescape(label):
    return label.replace('\\"', '"').replace("\\n", "\n").replace("\\\\", "\\")


def fields(state):
    out, cur = {}, None
    for line in state.split("\n"):
        m = re.match(r"/\\ (\w+) = (.*)", line)
        if m:
            cur = m.group(1)
            out[cur] = m.group(2)
        elif cur:
            out[cur] += " " + line.strip()
    return out


def field(state, name):
    return state.get(name, "")


def per_side(value, side):
    m = re.search(side + r' \|-> ("[^"]*"|\{[^}]*\}|<<[^>]*>>[^,\]]*|TRUE|FALSE|\d+)', value)
    return m.group(1) if m else "?"


def phase_to_path(phase, link_up, fell):
    if link_up == "FALSE" or phase == '"Gone"':
        return "none"
    if phase == '"Connected"':
        return "relayed" if fell == "TRUE" else "direct"
    if phase in ('"Idle"', '"Failed"'):
        return "relayed"
    return "upgrading"


def signals(chan, side):
    m = re.search(side + r" \|-> <<(.*?)>>(?=,\s*[AB] \|->|\s*\])", chan)
    if not m:
        return "-"
    return ",".join(sorted(set(re.findall(r't \|-> "(\w+)"', m.group(1))))) or "-"


def klass(state):
    phase = field(state, "phase")
    link = field(state, "linkUp")
    fell = field(state, "fellBack")
    heard, acked = field(state, "heard"), field(state, "acked")
    chan = field(state, "chan")
    esc, spr, act = field(state, "escalated"), field(state, "sprayed"), field(state, "active")
    path = field(state, "path")
    key = []
    for s in "AB":
        ph = per_side(phase, s).strip('"')
        lp = phase_to_path(per_side(phase, s), link, per_side(fell, s))
        h = per_side(heard, s) not in ("{}", "?")
        a = per_side(acked, s) not in ("{}", "?")
        p = "path" if '<<"none">>' not in per_side(path, s) and per_side(path, s) != "?" else "-"
        flags = []
        if per_side(esc, s) == "TRUE":
            flags.append("esc")
        if per_side(spr, s) not in ("{}", "?"):
            flags.append("spray")
        if per_side(act, s) not in ("0", "?"):
            flags.append("bday-switched")
        key.append(f"{s}:{ph}/{lp} {'H' if h else '-'}{'A' if a else '-'} {p} "
                   f"in[{signals(chan, s)}] {'+'.join(flags) or '-'}")
    return " | ".join(key)


def terminal_verdict(state):
    phase = field(state, "phase")
    link = field(state, "linkUp")
    fell = field(state, "fellBack")
    phases = {s: per_side(phase, s).strip('"') for s in "AB"}
    over = all(p in ("Connected", "Failed", "Gone", "Idle") for p in phases.values())
    if link == "FALSE" and over:
        return "LEGAL-TERMINAL LinkClosed"
    if not over or all(p == "Idle" for p in phases.values()):
        return None
    paths = {s: phase_to_path(per_side(phase, s), link, per_side(fell, s)) for s in "AB"}
    if all(p == "direct" for p in paths.values()):
        return "LEGAL-TERMINAL DirectPathUp"
    if all(p == "relayed" for p in paths.values()):
        return "LEGAL-TERMINAL RelayedOnly"
    if all(p == "none" for p in paths.values()):
        return "LEGAL-TERMINAL LinkClosed"
    return "BUG terminal " + "/".join(paths[s] for s in "AB")


def main():
    path, scenario = sys.argv[1], sys.argv[2]
    nodes, edges = {}, []
    text = open(path).read()
    # One record per line: labels escape their newlines.
    for chunk in text.split("\n"):
        chunk = chunk.strip()
        m = EDGE.match(chunk)
        if m:
            edges.append(m.groups())
            continue
        m = NODE.match(chunk)
        if m:
            nodes[m.group(1)] = fields(unescape(m.group(2)))
    cls = {n: klass(s) for n, s in nodes.items()}
    succ = collections.defaultdict(set)
    pred = collections.defaultdict(set)
    for a, b, lab in edges:
        if a in nodes and b in nodes and a != b:
            succ[a].add(b)
            pred[b].add(a)
    # Terminal: the attempt is over AND nothing more can happen (a state with
    # a FallBack or a delivery still to come is not terminal yet).
    term = {n: (terminal_verdict(s) if not succ[n] else None) for n, s in nodes.items()}
    # States that can reach a terminal state.
    can = {n for n, t in term.items() if t}
    stack = list(can)
    while stack:
        n = stack.pop()
        for p in pred[n]:
            if p not in can:
                can.add(p)
                stack.append(p)
    count = collections.Counter(cls.values())
    out_e = collections.defaultdict(collections.Counter)
    in_e = collections.defaultdict(collections.Counter)
    for a, b, lab in edges:
        if a in cls and b in cls and cls[a] != cls[b]:
            act = re.sub(r"\(.*", "", lab).strip() or "?"
            out_e[cls[a]][f"{act} -> #{{{cls[b]}}}"] += 1
            in_e[cls[b]][act] += 1
    verdict = {}
    for c in count:
        members = [n for n in cls if cls[n] == c]
        stuck = [n for n in members if n not in can]
        terms = {term[n] for n in members if term[n]}
        if stuck:
            verdict[c] = f"BUG ({len(stuck)} state(s) with no path to a terminal state)"
        elif any(t.startswith("BUG") for t in terms):
            verdict[c] = "BUG " + ", ".join(sorted(terms))
        elif terms and len(terms) == 1 and all(not succ[n] or all(cls[m] == c for m in succ[n]) for n in members):
            verdict[c] = sorted(terms)[0]
        else:
            verdict[c] = "LEGAL-TRANSIENT" + (" (some states terminal: " + ", ".join(sorted(terms)) + ")" if terms else "")
    ids = {c: i for i, c in enumerate(sorted(count, key=lambda c: (-count[c], c)), 1)}
    print(f"### State classes: {scenario}\n")
    print(f"{len(nodes)} states, {len(edges)} transitions, {len(count)} classes. "
          f"Class: per side `phase/linkPath heard-acked path in[signals in flight] flags`.\n")
    print("| # | class (A \\| B) | states | in (by action) | out (action -> class) | verdict |")
    print("|---|---|---|---|---|---|")
    for c in sorted(count, key=lambda c: ids[c]):
        ins = ", ".join(f"{a}×{n}" for a, n in in_e[c].most_common(4))
        outs = ", ".join(f"{a.split(' -> ')[0]}→#{ids[a.split('#{', 1)[1][:-1]]}×{n}"
                         for a, n in out_e[c].most_common(5))
        print(f"| {ids[c]} | `{c}` | {count[c]} | {ins or '-'} | {outs or '-'} | {verdict[c]} |")
    bugs = [c for c in count if verdict[c].startswith("BUG")]
    print(f"\nClasses with no path to a terminal state, or a rejected terminal: {len(bugs)}.")


if __name__ == "__main__":
    main()
