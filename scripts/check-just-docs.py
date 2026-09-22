#!/usr/bin/env python3
"""Every listed recipe describes itself in a `[doc(...)]`, not by accident.

`just --list` is the first thing anyone who clones this repository reads, and
until Codeberg #301 most of its lines were sentence fragments: `ci-gate`
introduced itself as "day one it would only teach people to skip the gate" and
`fast` as "sources, proof against the kernel".

The mechanism is not truncation. `take_doc_comment` in just's parser walks the
item list backwards from a recipe, requires a newline and then exactly ONE
`Item::Comment`, and keeps that single line -- so the LAST line of a comment
block becomes the whole description, `just --explain` prints the same line, and
a blank line between block and recipe suppresses the description entirely
rather than reaching further up (measured against just 1.40.0). Our comment
blocks are long and worth keeping, so nothing about writing one tells the
author which of its lines a reader will be shown.

`[doc('...')]` takes precedence over the preceding comment, which makes the
description something the author writes on purpose. This gate is what keeps it
that way for the next recipe: the attribute must be there, it must say
something, and it must stay short enough that the list still reads as a list.

The recipe list comes from `just --dump --dump-format json`, i.e. from just's
own parser, so the gate cannot drift from the grammar it is checking -- and
`attributes` in that dump is the explicit attribute, while `doc` is whatever
`--list` would show. Comparing them is exactly the distinction the issue is
about. Private recipes (`_`-prefixed or `[private]`) are skipped: they are not
listed, so they have no description to get wrong.

Exit 0 = every listed recipe carries an explicit, short doc attribute.
Exit 1 = one does not, or the checker's own fixtures did not behave.

Usage:
  python3 scripts/check-just-docs.py [--justfile PATH]
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile

# A description longer than this stops being a description. `just --list` puts
# it after the longest recipe signature in the file (currently 47 columns for
# `flash-rnode-write-image PORT IMAGE BOARD="auto"`), so the limit is what
# keeps the right-hand column readable on an 80-120 column terminal rather
# than an arbitrary style rule.
MAX_DOC_LEN = 72

REPO_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


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


def doc_attribute(recipe):
    """The string of the recipe's `[doc(...)]` attribute, or None.

    just renders an attribute either as a bare string (`[private]`) or as a
    one-key object (`[doc('x')]` -> {"doc": "x"}), so both shapes are read.
    `[doc]` with no argument carries no string and counts as absent.
    """
    for attr in recipe.get("attributes", []):
        if isinstance(attr, dict) and "doc" in attr:
            value = attr["doc"]
            return value if isinstance(value, str) else None
    return None


def recipe_line(source, name):
    """1-based line of the recipe's definition, so a failure names a place."""
    pattern = re.compile(r"^" + re.escape(name) + r"(\s|:)")
    for number, line in enumerate(source.splitlines(), start=1):
        if pattern.match(line):
            return number
    return None


def findings(justfile):
    """One line per listed recipe whose description is not explicit."""
    recipes = dump_recipes(justfile)
    with open(justfile, encoding="utf-8") as handle:
        source = handle.read()

    out = []
    for name in sorted(recipes):
        recipe = recipes[name]
        if recipe.get("private") or name.startswith("_"):
            continue
        where = recipe_line(source, name)
        shown_path = os.path.relpath(os.path.abspath(justfile), REPO_DIR)
        if shown_path.startswith(".."):
            shown_path = justfile
        place = f"{shown_path}:{where}" if where else shown_path
        doc = doc_attribute(recipe)
        shown = recipe.get("doc")
        if doc is None:
            if shown is None:
                out.append(
                    f"{place}: {name} has no description at all -- a blank line "
                    f"between its comment block and the recipe suppresses one. "
                    f"Add [doc('...')] directly above the recipe line."
                )
            else:
                out.append(
                    f"{place}: {name} is described by the tail of its comment "
                    f"block, {shown!r}. Add [doc('...')] directly above the "
                    f"recipe line; it takes precedence and leaves the block alone."
                )
        elif not doc.strip():
            out.append(
                f"{place}: {name} has an empty [doc('')], which suppresses the "
                f"description instead of writing one."
            )
        elif len(doc) > MAX_DOC_LEN:
            out.append(
                f"{place}: {name} has a {len(doc)}-character description, over "
                f"the {MAX_DOC_LEN}-character limit. The block below it is where "
                f"the detail belongs: {doc!r}"
            )
    return out


# --- Self-test ------------------------------------------------------------
#
# "Found nothing" is what both a working gate and a broken one print, so every
# verdict is exercised on a fixture justfile before the real one is read.

FIXTURES = {
    # The #301 shape itself: the tail of a block becomes the description.
    "tail-of-block": (
        "# A long explanation.\n"
        "# Whose last line is not a description.\n"
        "alpha:\n"
        "    @true\n",
        "alpha",
    ),
    # A blank line before the recipe suppresses the description entirely.
    "blank-line": (
        "# A block that reaches nothing.\n"
        "\n"
        "alpha:\n"
        "    @true\n",
        "alpha",
    ),
    "empty-doc": ("[doc('')]\nalpha:\n    @true\n", "alpha"),
    "too-long": (
        "[doc('" + "x" * (MAX_DOC_LEN + 1) + "')]\nalpha:\n    @true\n",
        "alpha",
    ),
}

CLEAN_FIXTURE = (
    "# A long explanation that stays exactly where it is.\n"
    "# Several lines of it, none of them a description.\n"
    "[doc('Do the thing, briefly.')]\n"
    "alpha:\n"
    "    @true\n"
    "\n"
    "# A private recipe is not listed, so it needs no description.\n"
    "_helper:\n"
    "    @true\n"
    "\n"
    "# Nor is one marked private by attribute.\n"
    "[private]\n"
    "beta:\n"
    "    @true\n"
)


def self_test():
    failures = []
    with tempfile.TemporaryDirectory() as tmp:
        for label, (text, expected_name) in FIXTURES.items():
            path = os.path.join(tmp, f"{label}.just")
            with open(path, "w", encoding="utf-8") as handle:
                handle.write(text)
            try:
                got = findings(path)
            except RuntimeError as exc:
                failures.append(f"fixture '{label}' did not parse: {exc}")
                continue
            if not any(expected_name in line for line in got):
                failures.append(
                    f"fixture '{label}' was not caught: {got!r}"
                )

        path = os.path.join(tmp, "clean.just")
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(CLEAN_FIXTURE)
        try:
            got = findings(path)
        except RuntimeError as exc:
            got = None
            failures.append(f"clean fixture did not parse: {exc}")
        if got:
            failures.append(f"clean fixture was flagged: {got!r}")

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
    return failures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--justfile",
        default=os.path.join(REPO_DIR, "Justfile"),
        help="the justfile to check (default: this repository's)",
    )
    args = parser.parse_args()

    broken = self_test()
    if broken:
        print("check-just-docs: SELF-TEST FAILED")
        for line in broken:
            print(f"  {line}")
        print("The checker is broken; its verdict on the tree means nothing.")
        return 1

    try:
        bad = findings(args.justfile)
    except RuntimeError as exc:
        print(f"check-just-docs: FAILED -- could not read {args.justfile}: {exc}")
        return 1

    if bad:
        print("check-just-docs: FAILED -- `just --list` would show these by accident.")
        for line in bad:
            print(f"  {line}")
        print()
        print("just keeps ONE comment line as a recipe's description: the last one")
        print("before the recipe. A [doc('...')] attribute on the line directly above")
        print("the recipe takes precedence over it and leaves the comment block")
        print("untouched (Codeberg #301).")
        return 1

    recipes = dump_recipes(args.justfile)
    listed = sum(
        1 for n, r in recipes.items() if not r.get("private") and not n.startswith("_")
    )
    print(f"check-just-docs: OK -- {listed} listed recipes describe themselves.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
