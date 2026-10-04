#!/usr/bin/env python3
"""`just standard` runs every integration-test target in the tree (#408).

`standard` named its integration suites one at a time until 408, and 94 of
the workspace's 139 `tests/` targets were in no line of it. lnmsg's
python_interop among them went red on master for eight commits while the land
gate's `just standard` passed it twice (401). The repair is
scripts/standard-integ.sh, which computes its selection from the tree and
leaves out only what scripts/standard-integ-elsewhere.txt says another line of
`standard` already runs. A computed selection cannot forget a new file, so
what remains to drift is the three things it stands on, and this gate holds
each of them:

  1. The enumeration. scripts/integ-targets.sh finds targets by cargo's
     autodiscovery rule for `tests/`, which is complete only while no member
     declares a `[[test]]` elsewhere or sets `autotests = false`. Its result
     is compared, both directions, against `cargo metadata`'s own list of
     test targets.
  2. The claims. Every line of standard-integ-elsewhere.txt must name a
     target that exists, cite a recipe `standard` reaches, and quote text
     that recipe's body holds. A claim whose line was deleted is a target
     `standard` silently stopped running: the synthetic omission the
     fixtures below plant.
  3. The call. `standard` itself must run scripts/standard-integ.sh.

Plus the prerequisite file both runners read (scripts/integ-prerequisites.txt):
every target it names must exist, so a renamed suite cannot keep a skip
that describes nothing.

The checker's own fixtures run first, every time: a guard that has never been
seen to fire is not known to work.

Exit 0 = all of the above hold. Exit 1 = one does not, or a fixture did not
behave.

Usage:
  python3 scripts/check-standard-integ.py [--justfile PATH]
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile

REPO_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ROOT = "standard"
RUNNER = "scripts/standard-integ.sh"
ELSEWHERE = "scripts/standard-integ-elsewhere.txt"
PREREQUISITES = "scripts/integ-prerequisites.txt"


def dump_recipes(justfile):
    """just's own view of a justfile: {name: recipe dict}."""
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


def reached(recipes, root):
    """Every recipe `just <root>` runs, the root included."""
    seen = set()
    stack = [root]
    while stack:
        name = stack.pop()
        if name in seen:
            continue
        seen.add(name)
        for dep in recipes.get(name, {}).get("dependencies", []):
            stack.append(dep["recipe"])
    return seen


def recipe_body(source, name):
    """The recipe's body lines as written, `{{...}}` left in place."""
    header = re.compile(r"^" + re.escape(name) + r"(\s|:)")
    lines = source.splitlines()
    for index, line in enumerate(lines):
        if header.match(line):
            body = []
            for text in lines[index + 1 :]:
                if text.strip() and not text[0].isspace():
                    break
                body.append(text)
            return "\n".join(body)
    return None


def data_lines(path):
    """[(line number, fields)] of a declaration file, comments dropped."""
    out = []
    with open(path, encoding="utf-8") as handle:
        for number, line in enumerate(handle, start=1):
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            out.append((number, line.split()))
    return out


def enumerated_targets():
    """scripts/integ-targets.sh's answer: {`<dir>/<name>`}."""
    proc = subprocess.run(
        ["bash", "-c", ". scripts/integ-targets.sh && integ_enumerate"],
        cwd=REPO_DIR,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or "integ_enumerate failed")
    return set(proc.stdout.split())


def metadata_targets():
    """cargo's answer: {`<dir>/<name>`} for every target of kind `test`."""
    proc = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=REPO_DIR,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or "cargo metadata failed")
    out = set()
    for package in json.loads(proc.stdout)["packages"]:
        member = os.path.relpath(os.path.dirname(package["manifest_path"]), REPO_DIR)
        for target in package["targets"]:
            if "test" in target["kind"]:
                out.add(f"{member}/{target['name']}")
    return out


def claim_matches(claim, target):
    return claim == target or (
        claim.endswith("/*") and target.rsplit("/", 1)[0] == claim[:-2]
    )


def findings(justfile, enumerated, metadata, elsewhere, prerequisites):
    """One line per way `standard` can miss a target.

    `elsewhere` and `prerequisites` are (path shown in messages, data_lines).
    """
    recipes = dump_recipes(justfile)
    with open(justfile, encoding="utf-8") as handle:
        source = handle.read()
    if ROOT not in recipes:
        raise RuntimeError(f"no `{ROOT}` recipe in {justfile}")
    reach = reached(recipes, ROOT)
    out = []

    # 1. The enumeration against cargo's own list.
    for target in sorted(metadata - enumerated):
        out.append(
            f"cargo knows the test target {target}, scripts/integ-targets.sh "
            f"does not find it, so `{ROOT}` would never run it. It is declared "
            f"outside `tests/*.rs` and `tests/<dir>/main.rs`, or its member is "
            f"missing from Cargo.toml's `members`: teach integ_enumerate the "
            f"rule it broke."
        )
    for target in sorted(enumerated - metadata):
        out.append(
            f"scripts/integ-targets.sh finds {target}, cargo has no such test "
            f"target (`autotests = false`, or a `[[test]]` that renames it), so "
            f"`cargo test --test` on it would fail or select something else."
        )

    # 2. Every claim is a target another line of `standard` really runs.
    shown, lines = elsewhere
    for number, fields in lines:
        where = f"{shown}:{number}"
        if len(fields) < 3:
            out.append(f"{where}: needs `<dir>/<target> <recipe> <text>`.")
            continue
        claim, recipe, text = fields[0], fields[1], " ".join(fields[2:])
        if not any(claim_matches(claim, t) for t in enumerated):
            out.append(
                f"{where}: claims {claim}, which matches no test target in "
                f"this tree. Drop the line, or follow the rename."
            )
        if recipe not in reach:
            out.append(
                f"{where}: cites recipe `{recipe}`, which `{ROOT}` does not "
                f"run, so {claim} is in neither `{ROOT}` nor its computed run."
            )
            continue
        body = recipe_body(source, recipe)
        if body is None or text not in body:
            out.append(
                f"{where}: recipe `{recipe}` no longer holds `{text}`, so "
                f"{claim} is in neither that line nor {RUNNER}'s computed run. "
                f"Restore the line, or delete this claim so the computed run "
                f"takes the target."
            )

    # 3. The call itself.
    body = recipe_body(source, ROOT) or ""
    if RUNNER not in body:
        out.append(
            f"`{ROOT}` does not run {RUNNER}, so every target "
            f"{ELSEWHERE} does not claim runs in no line of it."
        )

    # 4. The prerequisites name real targets.
    shown, lines = prerequisites
    for number, fields in lines:
        where = f"{shown}:{number}"
        if len(fields) < 2:
            out.append(f"{where}: needs `<path> <dir>/<target> [<test>]`.")
            continue
        if fields[1] not in enumerated:
            out.append(
                f"{where}: names {fields[1]}, which is not a test target in "
                f"this tree. Drop the line, or follow the rename."
            )
    return out


FIXTURE_JUSTFILE = """\
manifest := "true"

fast: mvr

mvr:
    {{manifest}} mvr -- cargo test -p a --test mvr

orphan:
    cargo test -p a --test lonely

standard: fast
    {{manifest}} x -- cargo test -p a --test interop
    bash scripts/standard-integ.sh
"""


def selftest():
    """Plant each failure on a fixture and require the checker to name it."""
    targets = {"a/mvr", "a/interop", "a/plain", "b/one", "b/two"}
    good_claims = [
        (1, ["a/mvr", "mvr", "cargo", "test", "-p", "a", "--test", "mvr"]),
        (2, ["a/interop", "standard", "cargo", "test", "-p", "a", "--test", "interop"]),
    ]
    prereqs = [(1, ["reference/x", "a/interop"])]
    cases = [
        ("clean fixture", FIXTURE_JUSTFILE, targets, targets, good_claims, prereqs, None),
        (
            "omission: the line a claim stands for is gone",
            FIXTURE_JUSTFILE.replace("    {{manifest}} x -- cargo test -p a --test interop\n", ""),
            targets, targets, good_claims, prereqs, "no longer holds",
        ),
        (
            "standard stops calling the runner",
            FIXTURE_JUSTFILE.replace("    bash scripts/standard-integ.sh\n", ""),
            targets, targets, good_claims, prereqs, "does not run scripts/standard-integ.sh",
        ),
        (
            "claim cites a recipe standard does not reach",
            FIXTURE_JUSTFILE, targets, targets,
            good_claims + [(3, ["b/one", "orphan", "cargo", "test"])],
            prereqs, "does not run, so b/one",
        ),
        (
            "claim names a missing target",
            FIXTURE_JUSTFILE, targets, targets,
            good_claims + [(3, ["c/*", "standard", "standard-integ.sh"])],
            prereqs, "matches no test target",
        ),
        (
            "cargo has a target the enumeration misses",
            FIXTURE_JUSTFILE, targets, targets | {"b/declared"}, good_claims,
            prereqs, "cargo knows the test target b/declared",
        ),
        (
            "prerequisite names a missing target",
            FIXTURE_JUSTFILE, targets, targets, good_claims,
            prereqs + [(2, ["reference/x", "a/gone"])], "names a/gone",
        ),
    ]
    failed = []
    with tempfile.TemporaryDirectory() as tmp:
        for label, text, enumerated, metadata, claims, pre, expect in cases:
            path = os.path.join(tmp, "Justfile")
            with open(path, "w", encoding="utf-8") as handle:
                handle.write(text)
            got = findings(path, enumerated, metadata, ("fixture", claims), ("fixture", pre))
            if expect is None and got:
                failed.append(f"{label}: expected no finding, got {got}")
            elif expect is not None and not any(expect in line for line in got):
                failed.append(f"{label}: expected a finding with `{expect}`, got {got}")
    return failed


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--justfile", default=os.path.join(REPO_DIR, "Justfile"))
    args = parser.parse_args()

    try:
        broken = selftest()
    except RuntimeError as error:
        print(f"check-standard-integ: selftest could not run: {error}", file=sys.stderr)
        return 1
    if broken:
        print("check-standard-integ: FAILED, the checker's own fixtures:", file=sys.stderr)
        for line in broken:
            print(f"  {line}", file=sys.stderr)
        return 1

    try:
        enumerated = enumerated_targets()
        if not enumerated:
            raise RuntimeError("scripts/integ-targets.sh found no test target at all")
        out = findings(
            args.justfile,
            enumerated,
            metadata_targets(),
            (ELSEWHERE, data_lines(os.path.join(REPO_DIR, ELSEWHERE))),
            (PREREQUISITES, data_lines(os.path.join(REPO_DIR, PREREQUISITES))),
        )
    except RuntimeError as error:
        print(f"check-standard-integ: could not check: {error}", file=sys.stderr)
        return 1

    if out:
        print("check-standard-integ: FAILED", file=sys.stderr)
        for line in out:
            print(f"  {line}", file=sys.stderr)
        return 1
    print(
        f"check-standard-integ: {len(enumerated)} test targets, cargo agrees; "
        f"every claim in {ELSEWHERE} is backed by its recipe; `{ROOT}` runs {RUNNER}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
