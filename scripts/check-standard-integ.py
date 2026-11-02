#!/usr/bin/env python3
"""`just standard` runs every integration-test target in the tree (#408).

`standard` named its integration suites one at a time until 408, and 94 of
the workspace's 139 `tests/` targets were in no line of it. loquax's
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
  4. The features. The computed run is one plain `cargo test --test ...`
     list with no `--features`, and cargo refuses the whole invocation when
     one of its targets declares `required-features`. Every such target must
     be claimed by a recipe that passes the feature (land gate 1f9036f8,
     2026-10-26: loquax/tui_snapshot, 0 tests executed).
  5. The forge gate. scripts/ci-gate-integ.sh computes its own selection
     and refuses the same targets for the same reason (forge pipeline 573,
     2026-10-26: red on loquax/tui_snapshot since 994). It must exclude them
     by the rule this script serves it (`--gated`), and `ci-gate` must run
     the recipe each one is claimed by, or the forge tests it nowhere.

Plus the prerequisite file both runners read (scripts/integ-prerequisites.txt):
every target it names must exist, so a renamed suite cannot keep a skip
that describes nothing, and must be named `<dir>/<target>`: a bare name two
packages share is refused with that reason (1306). The selftest also holds
scripts/integ-targets.sh's integ_run_groups to running a target whose name an
excluded one shares from its own package (land gate on ee9541af). And a target with required features that the file
names runs in its recipe, not in either runner, so that recipe must run it
through scripts/with-prerequisites.sh naming it, or the forge runs the tests
the file excludes everywhere else (forge pipeline 598, 2026-10-31, 1167).

The checker's own fixtures run first, every time: a guard that has never been
seen to fire is not known to work.

Exit 0 = all of the above hold. Exit 1 = one does not, or a fixture did not
behave.

Usage:
  python3 scripts/check-standard-integ.py [--justfile PATH]
  python3 scripts/check-standard-integ.py --gated
      print `<dir>/<target><TAB><features><TAB><recipe or ->` for every
      test target with required features, and check nothing: the rule
      scripts/ci-gate-integ.sh excludes by
  python3 scripts/check-standard-integ.py --wrapped [--justfile PATH]
      print `<dir>/<target><TAB><recipe>` for every test target with
      required features whose recipe runs it through
      scripts/with-prerequisites.sh naming it (rule 4b), and check nothing:
      scripts/ci-gate-integ.sh accepts a by-name entry of
      scripts/integ-prerequisites.txt for such a target, which its own run
      excludes, because that recipe applies it (1203)
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
FORGE = "ci-gate"
FORGE_RUNNER = "scripts/ci-gate-integ.sh"
# The line of FORGE_RUNNER that excludes the targets with required features.
FORGE_RULE = "check-standard-integ.py --gated"
ELSEWHERE = "scripts/standard-integ-elsewhere.txt"
PREREQUISITES = "scripts/integ-prerequisites.txt"
# The wrapper a recipe runs a gated target's tests through so that file's
# declarations reach them.
PREREQ_RUNNER = "scripts/with-prerequisites.sh"


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
    """cargo's answer: {`<dir>/<name>`: [required features]} for every target
    of kind `test`."""
    proc = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=REPO_DIR,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or "cargo metadata failed")
    out = {}
    for package in json.loads(proc.stdout)["packages"]:
        member = os.path.relpath(os.path.dirname(package["manifest_path"]), REPO_DIR)
        for target in package["targets"]:
            if "test" in target["kind"]:
                out[f"{member}/{target['name']}"] = target.get("required-features") or []
    return out


def claim_matches(claim, target):
    return claim == target or (
        claim.endswith("/*") and target.rsplit("/", 1)[0] == claim[:-2]
    )


def claimed_by(claims, target):
    """The recipe of the first claim covering `target`, or None."""
    for _, fields in claims:
        if len(fields) >= 2 and claim_matches(fields[0], target):
            return fields[1]
    return None


def runs_through_wrapper(source, recipe, target):
    """Does `recipe`'s body run `target` through PREREQ_RUNNER, naming it?"""
    body = recipe_body(source, recipe) or ""
    wrapped = re.compile(
        re.escape(PREREQ_RUNNER) + r"[^\n]*\s" + re.escape(target) + r"(\s|$)", re.M
    )
    return wrapped.search(body) is not None


def wrapped_targets(source, metadata, claims):
    """[(target, recipe)] of every target with required features whose
    claimed recipe runs it through PREREQ_RUNNER: what `--wrapped` prints."""
    out = []
    for target, features in sorted(metadata.items()):
        recipe = claimed_by(claims, target) if features else None
        if recipe is not None and runs_through_wrapper(source, recipe, target):
            out.append((target, recipe))
    return out


def findings(justfile, enumerated, metadata, elsewhere, prerequisites, forge_runner):
    """One line per way `standard` or the forge gate can miss a target.

    `metadata` is {`<dir>/<name>`: [required features]}; `elsewhere` and
    `prerequisites` are (path shown in messages, data_lines); `forge_runner`
    is the text of scripts/ci-gate-integ.sh.
    """
    recipes = dump_recipes(justfile)
    with open(justfile, encoding="utf-8") as handle:
        source = handle.read()
    if ROOT not in recipes:
        raise RuntimeError(f"no `{ROOT}` recipe in {justfile}")
    reach = reached(recipes, ROOT)
    gated = {t: f for t, f in metadata.items() if f}
    metadata = set(metadata)
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

    # 2b. A target with required features is claimed, never computed.
    claims = [fields[0] for _, fields in lines if fields]
    for target in sorted(gated):
        if not any(claim_matches(claim, target) for claim in claims):
            out.append(
                f"{target} requires the feature(s) `{', '.join(gated[target])}`; "
                f"the computed run of {RUNNER} enables none, so `cargo test "
                f"--test` refuses the whole invocation (land gate 1f9036f8, "
                f"2026-10-26). Claim it in {ELSEWHERE} from a recipe that "
                f"passes the feature."
            )

    # 2c. The forge gate excludes them by the same rule and runs their recipe.
    if gated and FORGE_RULE not in forge_runner:
        out.append(
            f"{FORGE_RUNNER} no longer runs `{FORGE_RULE}`, so it hands "
            f"{', '.join(sorted(gated))} to a `cargo test` without the feature, "
            f"which refuses the whole forge gate (pipeline 573, 2026-10-26)."
        )
    forge_body = recipe_body(source, FORGE) or ""
    for target in sorted(gated):
        recipe = claimed_by(lines, target)
        if recipe is None:
            continue
        if not re.search(r"(^|\s)" + re.escape(recipe) + r"(\s|$)", forge_body):
            out.append(
                f"{target} requires the feature(s) `{', '.join(gated[target])}`, "
                f"so {FORGE_RUNNER} leaves it to recipe `{recipe}`, and "
                f"`{FORGE}` does not run `{recipe}`: the forge tests {target} "
                f"nowhere. Add `{{{{just_executable()}}}} {recipe}` to `{FORGE}`."
            )

    # 3. The call itself.
    body = recipe_body(source, ROOT) or ""
    if RUNNER not in body:
        out.append(
            f"`{ROOT}` does not run {RUNNER}, so every target "
            f"{ELSEWHERE} does not claim runs in no line of it."
        )

    # 4. The prerequisites name real targets.
    claim_lines = lines
    shown, lines = prerequisites
    for number, fields in lines:
        where = f"{shown}:{number}"
        if len(fields) < 2:
            out.append(f"{where}: needs `<path> <dir>/<target> [<test>]`.")
            continue
        if "/" not in fields[1]:
            same = sorted(t for t in enumerated if t.rsplit("/", 1)[-1] == fields[1])
            if len(same) > 1:
                out.append(
                    f"{where}: names the bare target {fields[1]}, which exists "
                    f"in {len(same)} packages ({', '.join(same)}); `cargo test "
                    f"--test {fields[1]}` selects all of them, so excluding by "
                    f"that name would drop the others without a word. Qualify "
                    f"it as <dir>/{fields[1]} (1306)."
                )
                continue
        if fields[1] not in enumerated:
            out.append(
                f"{where}: names {fields[1]}, which is not a test target in "
                f"this tree. Drop the line, or follow the rename."
            )

    # 4b. A gated target's prerequisites reach the recipe that runs it.
    declared = sorted({fields[1] for _, fields in lines if len(fields) >= 2} & set(gated))
    for target in declared:
        recipe = claimed_by(claim_lines, target)
        if recipe is None:
            continue
        if not runs_through_wrapper(source, recipe, target):
            out.append(
                f"{shown} declares prerequisites of {target}, which requires "
                f"features and runs in recipe `{recipe}`, not in {RUNNER} or "
                f"{FORGE_RUNNER}; `{recipe}` does not run it through "
                f"`{PREREQ_RUNNER} {target}`, so a host without the "
                f"prerequisite runs the tests the file excludes (1167)."
            )
    return out


FIXTURE_JUSTFILE = """\
manifest := "true"

fast: mvr

mvr:
    {{manifest}} mvr -- cargo test -p a --test mvr

orphan:
    cargo test -p a --test lonely

tui:
    {{manifest}} g -- bash scripts/with-prerequisites.sh a/gated -- cargo test -p a --features tui --test gated

standard: fast tui
    {{manifest}} x -- cargo test -p a --test interop
    bash scripts/standard-integ.sh

ci-gate:
    {{just_executable()}} mvr
    bash scripts/ci-gate-integ.sh
    {{just_executable()}} tui
"""

FIXTURE_FORGE_RUNNER = 'GATED="$(python3 scripts/check-standard-integ.py --gated)"\n'


def selftest():
    """Plant each failure on a fixture and require the checker to name it."""
    targets = {"a/mvr", "a/interop", "a/plain", "b/one", "b/two", "a/gated"}
    meta = {t: [] for t in targets}
    gated = dict(meta, **{"a/gated": ["tui"]})
    good_claims = [
        (1, ["a/mvr", "mvr", "cargo", "test", "-p", "a", "--test", "mvr"]),
        (2, ["a/interop", "standard", "cargo", "test", "-p", "a", "--test", "interop"]),
    ]
    gated_claim = (3, ["a/gated", "tui", "cargo", "test", "-p", "a", "--features", "tui", "--test", "gated"])
    prereqs = [(1, ["reference/x", "a/interop"])]
    forge = FIXTURE_FORGE_RUNNER
    cases = [
        ("clean fixture", FIXTURE_JUSTFILE, targets, meta, good_claims, prereqs, None),
        (
            "omission: the line a claim stands for is gone",
            FIXTURE_JUSTFILE.replace("    {{manifest}} x -- cargo test -p a --test interop\n", ""),
            targets, meta, good_claims, prereqs, "no longer holds",
        ),
        (
            "standard stops calling the runner",
            FIXTURE_JUSTFILE.replace("    bash scripts/standard-integ.sh\n", ""),
            targets, meta, good_claims, prereqs, "does not run scripts/standard-integ.sh",
        ),
        (
            "claim cites a recipe standard does not reach",
            FIXTURE_JUSTFILE, targets, meta,
            good_claims + [(3, ["b/one", "orphan", "cargo", "test"])],
            prereqs, "does not run, so b/one",
        ),
        (
            "claim names a missing target",
            FIXTURE_JUSTFILE, targets, meta,
            good_claims + [(3, ["c/*", "standard", "standard-integ.sh"])],
            prereqs, "matches no test target",
        ),
        (
            "cargo has a target the enumeration misses",
            FIXTURE_JUSTFILE, targets, dict(meta, **{"b/declared": []}), good_claims,
            prereqs, "cargo knows the test target b/declared",
        ),
        (
            "prerequisite names a missing target",
            FIXTURE_JUSTFILE, targets, meta, good_claims,
            prereqs + [(2, ["reference/x", "a/gone"])], "names a/gone",
        ),
        (
            "a target with required features is left to the computed run",
            FIXTURE_JUSTFILE, targets, gated, good_claims, prereqs,
            "a/gated requires the feature(s) `tui`",
        ),
        (
            "the same target claimed by the recipe that passes the feature",
            FIXTURE_JUSTFILE, targets, gated, good_claims + [gated_claim], prereqs, None,
        ),
        (
            "ci-gate stops running the recipe of a gated target",
            FIXTURE_JUSTFILE.replace("    {{just_executable()}} tui\n", ""),
            targets, gated, good_claims + [gated_claim], prereqs,
            "`ci-gate` does not run `tui`: the forge tests a/gated nowhere",
        ),
        (
            "a gated target with a prerequisite, its recipe through the wrapper",
            FIXTURE_JUSTFILE, targets, gated, good_claims + [gated_claim],
            prereqs + [(2, ["reference/x", "a/gated", "one_test"])], None,
        ),
        (
            "a gated target with a prerequisite, its recipe without the wrapper",
            FIXTURE_JUSTFILE.replace("bash scripts/with-prerequisites.sh a/gated -- ", ""),
            targets, gated, good_claims + [gated_claim],
            prereqs + [(2, ["reference/x", "a/gated", "one_test"])],
            "does not run it through `scripts/with-prerequisites.sh a/gated`",
        ),
        (
            "the forge runner drops the required-features rule",
            FIXTURE_JUSTFILE, targets, gated, good_claims + [gated_claim], prereqs,
            "scripts/ci-gate-integ.sh no longer runs",
        ),
    ]
    failed = []
    with tempfile.TemporaryDirectory() as tmp:
        for label, text, enumerated, metadata, claims, pre, expect in cases:
            path = os.path.join(tmp, "Justfile")
            with open(path, "w", encoding="utf-8") as handle:
                handle.write(text)
            runner = "" if "drops the required-features rule" in label else forge
            got = findings(path, enumerated, metadata, ("fixture", claims), ("fixture", pre), runner)
            if expect is None and got:
                failed.append(f"{label}: expected no finding, got {got}")
            elif expect is not None and not any(expect in line for line in got):
                failed.append(f"{label}: expected a finding with `{expect}`, got {got}")
    # `--wrapped`, which scripts/ci-gate-integ.sh trusts to call a by-name
    # line for a target it excludes applied elsewhere (1203): the wrapped
    # recipe is listed, the same recipe without the wrapper is not.
    claims = good_claims + [gated_claim]
    got = wrapped_targets(FIXTURE_JUSTFILE, gated, claims)
    if got != [("a/gated", "tui")]:
        failed.append(f"--wrapped on the clean fixture: expected [('a/gated', 'tui')], got {got}")
    bare = FIXTURE_JUSTFILE.replace("bash scripts/with-prerequisites.sh a/gated -- ", "")
    got = wrapped_targets(bare, gated, claims)
    if got:
        failed.append(f"--wrapped with the recipe unwrapped: expected nothing, got {got}")
    failed += run_groups_selftest()
    return failed


def run_groups(all_targets, selected):
    """scripts/integ-targets.sh's integ_run_groups: [(dir or "-", name)]."""
    proc = subprocess.run(
        [
            "bash",
            "-c",
            "set -euo pipefail; . scripts/integ-targets.sh && integ_run_groups \"$1\" \"$2\"",
            "run_groups",
            "\n".join(sorted(all_targets)),
            "\n".join(sorted(selected)),
        ],
        cwd=REPO_DIR,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or "integ_run_groups failed")
    return [tuple(line.split("\t")) for line in proc.stdout.splitlines() if line]


def run_groups_selftest():
    """A second package with a target of the same name as an excluded one
    (lnomad and loquax `scene_golden`, land gate on ee9541af): the survivor
    runs from its own package, never as a workspace-wide name that would take
    the excluded one along, and is never dropped."""
    failed = []
    cases = [
        (
            "two packages share a name and one is excluded",
            {"a/same", "b/same", "a/plain"},
            {"b/same", "a/plain"},
            [("-", "plain"), ("b", "same")],
        ),
        (
            "two packages share a name and both run",
            {"a/same", "b/same", "a/plain"},
            {"a/same", "b/same", "a/plain"},
            [("-", "plain"), ("-", "same")],
        ),
        (
            "both of a shared name are excluded",
            {"a/same", "b/same", "a/plain"},
            {"a/plain"},
            [("-", "plain")],
        ),
    ]
    for label, all_targets, selected, want in cases:
        got = sorted(run_groups(all_targets, selected))
        if got != sorted(want):
            failed.append(f"integ_run_groups, {label}: expected {want}, got {got}")
    # The bare name of the same fixture in the prerequisite file is refused
    # with the reason; its qualified form is accepted.
    enumerated = {"a/same", "b/same", "a/plain"}
    meta = {t: [] for t in enumerated}
    claims = [(1, ["a/plain", "standard", "standard-integ.sh"])]
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "Justfile")
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(FIXTURE_JUSTFILE)
        for label, line, expect in [
            ("a bare name two packages share", "same", "exists in 2 packages (a/same, b/same)"),
            ("the same name qualified", "a/same", None),
        ]:
            pre = [(1, ["reference/x", line])]
            got = findings(path, enumerated, meta, ("fixture", claims), ("fixture", pre), FIXTURE_FORGE_RUNNER)
            if expect is None and got:
                failed.append(f"{label}: expected no finding, got {got}")
            elif expect is not None and not any(expect in g for g in got):
                failed.append(f"{label}: expected a finding with `{expect}`, got {got}")
    return failed


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--justfile", default=os.path.join(REPO_DIR, "Justfile"))
    parser.add_argument("--gated", action="store_true")
    parser.add_argument("--wrapped", action="store_true")
    args = parser.parse_args()

    if args.wrapped:
        try:
            claims = data_lines(os.path.join(REPO_DIR, ELSEWHERE))
            with open(args.justfile, encoding="utf-8") as handle:
                source = handle.read()
            for target, recipe in wrapped_targets(source, metadata_targets(), claims):
                print(f"{target}\t{recipe}")
        except (RuntimeError, OSError) as error:
            print(f"check-standard-integ: could not list wrapped targets: {error}", file=sys.stderr)
            return 1
        return 0

    if args.gated:
        try:
            claims = data_lines(os.path.join(REPO_DIR, ELSEWHERE))
            for target, features in sorted(metadata_targets().items()):
                if features:
                    print(f"{target}\t{','.join(features)}\t{claimed_by(claims, target) or '-'}")
        except RuntimeError as error:
            print(f"check-standard-integ: could not list gated targets: {error}", file=sys.stderr)
            return 1
        return 0

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
            open(os.path.join(REPO_DIR, FORGE_RUNNER), encoding="utf-8").read(),
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
        f"every claim in {ELSEWHERE} is backed by its recipe; `{ROOT}` runs {RUNNER}; "
        f"`{FORGE}` runs the recipe of every target with required features"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
