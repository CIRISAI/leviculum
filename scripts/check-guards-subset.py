#!/usr/bin/env python3
"""`just guards` is a strict subset of `just fast`, checked and not promised.

`guards` (Codeberg #323) is the coder pass's standing gate: the members of
Tier 0 that cost under ten seconds each, named in a dependency list with no
body so that `guards` and `fast` can be diffed against each other by eye.
Being a subset was verified once, by hand, by the person who wrote the list.
Nothing kept it true afterwards, and the two ways it rots both cost an hour:

  * a guard added to `fast` and not to `guards` is invisible to every coder
    pass until a land gate goes red on it -- which is the shape #310 and #316
    had, and the reason `guards` exists at all;
  * a guard in `guards` and not in `fast` is a gate the landing never runs,
    so a coder pass sees a verdict nobody else will.

This gate closes both, plus the order: `guards` must run its members in the
order `fast` runs them, because "diff them by eye" is the only thing keeping
the two lists readable and a reordered list defeats it.

WHAT IT CANNOT DECIDE. The membership rule's first clause is a COST -- under
ten seconds standing alone -- and no static check can read a stopwatch. So the
third property is a forced decision rather than a measurement: every recipe
`fast` reaches must be either reached by `guards` or named in the ledger of
`# not-in-guards: NAME -- reason` lines in the Justfile comment above
`guards`. A new recipe on the push path cannot be silently absent from the
coder gate; it is either in it, or somebody wrote down why not. The ledger is
checked back: an entry for a recipe `fast` does not reach, or for one that IS
in `guards`, or with no reason, is itself a failure.

Reachability is over the dependency graph as just reports it, not over the two
lines of text: `check-source-invariant-census` is in `guards` directly and in
`fast` only as a dependency of `source-invariant-tests`, which is the split
#323 made on purpose.

Exit 0 = `guards` is a subset of `fast`, in order, and every other member of
         `fast` is accounted for.
Exit 1 = one of those is false, or the checker's own fixtures did not behave.

Usage:
  python3 scripts/check-guards-subset.py [--justfile PATH]
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile

REPO_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# The coder gate and the tier it is a subset of. Both names are what the
# failure messages talk about, so they are spelled once.
SUBSET = "guards"
SUPERSET = "fast"

# One line per member of `fast` that `guards` does not run, with the reason.
# `--` rather than an em dash so the line is greppable from a terminal.
LEDGER_RE = re.compile(r"^#\s*not-in-guards:\s*([A-Za-z0-9_.-]+)\s*--\s*(.*)$")


def dump_recipes(justfile):
    """Return just's own view of a justfile: {name: recipe dict}.

    Raises RuntimeError when just cannot parse it -- a checker that treats an
    unparseable justfile as "no recipes to complain about" passes forever.
    """
    proc = subprocess.run(
        [
            "just",
            "--justfile",
            justfile,
            "--working-directory",
            os.path.dirname(os.path.abspath(justfile)) or ".",
            "--dump",
            "--dump-format",
            "json",
        ],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or "just --dump failed")
    return json.loads(proc.stdout)["recipes"]


def execution_order(recipes, root):
    """The recipes `just <root>` would run, in the order it would run them.

    just runs a recipe's dependencies left to right, each one's own
    dependencies first, and each recipe at most once. The root itself is not
    part of the result: neither `fast` nor `guards` is a guard.
    """
    order = []
    seen = set()

    def walk(name):
        if name in seen:
            return
        seen.add(name)
        for dep in recipes.get(name, {}).get("dependencies", []):
            walk(dep["recipe"])
        order.append(name)

    walk(root)
    return [name for name in order if name != root]


def recipe_line(source, name):
    """1-based line of the recipe's definition, so a failure names a place."""
    pattern = re.compile(r"^" + re.escape(name) + r"(\s|:)")
    for number, line in enumerate(source.splitlines(), start=1):
        if pattern.match(line):
            return number
    return None


def ledger_entries(source):
    """[(recipe, reason, line)] for every `# not-in-guards:` line."""
    out = []
    for number, line in enumerate(source.splitlines(), start=1):
        match = LEDGER_RE.match(line.strip())
        if match:
            out.append((match.group(1), match.group(2).strip(), number))
    return out


def findings(justfile):
    """One line per way the two lists have come apart."""
    recipes = dump_recipes(justfile)
    with open(justfile, encoding="utf-8") as handle:
        source = handle.read()

    shown_path = os.path.relpath(os.path.abspath(justfile), REPO_DIR)
    if shown_path.startswith(".."):
        shown_path = justfile

    def place(name):
        where = recipe_line(source, name)
        return f"{shown_path}:{where}" if where else shown_path

    for name in (SUBSET, SUPERSET):
        if name not in recipes:
            raise RuntimeError(f"no `{name}` recipe in {justfile}")

    subset = execution_order(recipes, SUBSET)
    superset = execution_order(recipes, SUPERSET)
    superset_set = set(superset)
    subset_set = set(subset)

    out = []

    # 1. Subset. A guard the landing gate never runs.
    for name in subset:
        if name not in superset_set:
            out.append(
                f"{place(SUBSET)}: `{SUBSET}` runs {name}, which `{SUPERSET}` "
                f"never reaches -- so the landing gate never runs it and a "
                f"coder pass is the only place its verdict exists. Add it to "
                f"`{SUPERSET}`, or drop it from `{SUBSET}`."
            )

    # 2. Order. The lists are diffed by eye; that only works in one order.
    index = 0
    previous = None
    for name in subset:
        if name not in superset_set:
            continue
        try:
            index = superset.index(name, index) + 1
        except ValueError:
            after = f" after {previous}" if previous else ""
            out.append(
                f"{place(SUBSET)}: `{SUBSET}` runs {name}{after}, `{SUPERSET}` "
                f"runs it earlier. The two dependency lists are read against "
                f"each other by eye, which needs them in the same order."
            )
            index = superset.index(name) + 1
        previous = name

    # 3. Every other member of `fast` is a written-down decision, not an
    #    oversight. This is the clause that catches the guard somebody adds to
    #    the push path and forgets here.
    entries = ledger_entries(source)
    excused = {name for name, _, _ in entries}
    for name in superset:
        if name in subset_set or name in excused:
            continue
        out.append(
            f"{place(SUPERSET)}: `{SUPERSET}` runs {name} and `{SUBSET}` does "
            f"not, with no `# not-in-guards: {name} -- <reason>` line above "
            f"`{SUBSET}` saying why. Put it in `{SUBSET}` if it costs under "
            f"ten seconds standing alone, builds no part of the workspace for "
            f"the host and drives no test target of its own; otherwise write "
            f"that line."
        )

    # 4. The ledger itself, so it cannot outlive what it excuses.
    for name, reason, line in entries:
        where = f"{shown_path}:{line}"
        if name not in superset_set:
            out.append(
                f"{where}: `not-in-guards: {name}` names a recipe `{SUPERSET}` "
                f"never reaches. Delete the line; there is nothing to excuse."
            )
        elif name in subset_set:
            out.append(
                f"{where}: `not-in-guards: {name}` names a recipe that IS in "
                f"`{SUBSET}`. One of the two is wrong."
            )
        elif not reason:
            out.append(
                f"{where}: `not-in-guards: {name}` gives no reason. The reason "
                f"is the whole point of the line."
            )
    return out


# --- Self-test ------------------------------------------------------------
#
# "Found nothing" is what both a working gate and a broken one print, so every
# verdict is exercised before the real Justfile is read. The two verdicts the
# gate exists for are driven against a scratch copy of THIS repository's
# Justfile with one name moved, because a synthetic fixture proves the
# algorithm and not that it still fits the file it is pointed at.

CLEAN_FIXTURE = """
# not-in-guards: slow -- a minute of cargo
[doc('Cheap')]
alpha:
    @true

[doc('Cheap')]
beta:
    @true

[doc('Expensive')]
slow:
    @true

[doc('Coder gate')]
guards: alpha beta

[doc('Tier 0')]
fast: alpha beta slow
    @true
"""

FIXTURES = {
    # A guard in `guards` that the landing gate never runs.
    "guards-only": (
        CLEAN_FIXTURE.replace("guards: alpha beta", "guards: alpha beta orphan")
        + "\n[doc('Orphan')]\norphan:\n    @true\n",
        "orphan",
    ),
    # A guard on the push path that no coder pass runs and nothing excuses.
    "unexcused": (CLEAN_FIXTURE.replace("# not-in-guards: slow -- a minute of cargo\n", ""), "slow"),
    # The same two lists, read in different orders.
    "out-of-order": (CLEAN_FIXTURE.replace("guards: alpha beta", "guards: beta alpha"), "alpha"),
    # A ledger line that outlived the recipe it excused.
    "stale-ledger": (
        CLEAN_FIXTURE.replace("not-in-guards: slow --", "not-in-guards: ghost --")
        + "\n# not-in-guards: slow -- a minute of cargo\n[doc('x')]\nlater:\n    @true\n",
        "ghost",
    ),
    # A ledger line for a recipe that is in `guards` after all.
    "contradictory-ledger": (
        CLEAN_FIXTURE.replace(
            "# not-in-guards: slow --", "# not-in-guards: alpha -- stale\n# not-in-guards: slow --"
        ),
        "alpha",
    ),
    # A ledger line with no reason on it.
    "reasonless-ledger": (
        CLEAN_FIXTURE.replace("not-in-guards: slow -- a minute of cargo", "not-in-guards: slow --"),
        "slow",
    ),
    # Reachability is over the graph, not the text: `slow` is excused, and the
    # transitive dependency it drags onto the push path is not.
    "transitive-unexcused": (
        CLEAN_FIXTURE.replace("slow:\n", "slow: hidden\n") + "\n[doc('Hidden')]\nhidden:\n    @true\n",
        "hidden",
    ),
}

# The reverse of that last case: a `guards` member that `fast` reaches only
# through another recipe is fine, which is the #323 census/run split.
TRANSITIVE_CLEAN = CLEAN_FIXTURE.replace("slow:\n", "slow: census\n").replace(
    "guards: alpha beta", "guards: alpha beta census"
) + "\n[doc('Census')]\ncensus:\n    @true\n"

# The two controls #328 asks for, applied to the real file: (name, edit).
REAL_CONTROLS = {
    "a fast guard dropped from guards": (
        lambda text: re.sub(r"(?m)^(guards:.*?) check-trailers\b", r"\1", text),
        "check-trailers",
    ),
    "a guards member absent from fast": (
        lambda text: re.sub(r"(?m)^(guards:)", r"\1 notices", text),
        "notices",
    ),
}


def _findings_for(tmp, label, text):
    path = os.path.join(tmp, f"{label}.just")
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(text)
    return findings(path)


def self_test(justfile):
    failures = []
    with tempfile.TemporaryDirectory() as tmp:
        for label, (text, expected) in FIXTURES.items():
            try:
                got = _findings_for(tmp, label, text)
            except RuntimeError as exc:
                failures.append(f"fixture '{label}' did not parse: {exc}")
                continue
            if not any(expected in line for line in got):
                failures.append(f"fixture '{label}' was not caught: {got!r}")

        for label, text in (("clean", CLEAN_FIXTURE), ("transitive-clean", TRANSITIVE_CLEAN)):
            try:
                got = _findings_for(tmp, label, text)
            except RuntimeError as exc:
                got = None
                failures.append(f"fixture '{label}' did not parse: {exc}")
            if got:
                failures.append(f"fixture '{label}' was flagged: {got!r}")

        # An unparseable justfile must be an error, never an empty result.
        path = os.path.join(tmp, "broken.just")
        with open(path, "w", encoding="utf-8") as handle:
            handle.write("alpha\n    @true\n")
        try:
            findings(path)
        except RuntimeError:
            pass
        else:
            failures.append("an unparseable justfile was read as zero findings")

        # The two controls, against a copy of the file this gate protects.
        with open(justfile, encoding="utf-8") as handle:
            original = handle.read()
        for label, (edit, expected) in REAL_CONTROLS.items():
            damaged = edit(original)
            if damaged == original:
                failures.append(f"control '{label}' did not change the Justfile")
                continue
            try:
                got = _findings_for(tmp, label.replace(" ", "-"), damaged)
            except RuntimeError as exc:
                failures.append(f"control '{label}' did not parse: {exc}")
                continue
            if not any(expected in line for line in got):
                failures.append(
                    f"control '{label}' did not name {expected}: {got!r}"
                )
    return failures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--justfile",
        default=os.path.join(REPO_DIR, "Justfile"),
        help="the justfile to check (default: this repository's)",
    )
    args = parser.parse_args()

    broken = self_test(args.justfile)
    if broken:
        print("check-guards-subset: SELF-TEST FAILED")
        for line in broken:
            print(f"  {line}")
        print("The checker is broken; its verdict on the tree means nothing.")
        return 1

    try:
        bad = findings(args.justfile)
    except RuntimeError as exc:
        print(f"check-guards-subset: FAILED -- could not read {args.justfile}: {exc}")
        return 1

    if bad:
        print(
            f"check-guards-subset: FAILED -- `{SUBSET}` and `{SUPERSET}` have "
            f"come apart."
        )
        for line in bad:
            print(f"  {line}")
        print()
        print(f"`{SUBSET}` is the subset of `{SUPERSET}` a coder pass can afford")
        print("(Codeberg #323). A guard on only one of the two lists is a guard")
        print("either the landing gate or every coder pass will never run.")
        return 1

    recipes = dump_recipes(args.justfile)
    subset = execution_order(recipes, SUBSET)
    superset = execution_order(recipes, SUPERSET)
    with open(args.justfile, encoding="utf-8") as handle:
        excused = len(ledger_entries(handle.read()))
    print(
        f"check-guards-subset: OK -- {len(subset)} of `{SUPERSET}`'s "
        f"{len(superset)} recipes are in `{SUBSET}`, in order; the other "
        f"{len(superset) - len(subset)} are excused by name ({excused} ledger "
        f"lines)."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
