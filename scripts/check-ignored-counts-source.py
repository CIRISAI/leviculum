#!/usr/bin/env python3
"""The #[ignore]d census, counted from the SOURCES, for the push gate.

scripts/check-ignored-counts.py answers the same question from the built
artefacts: it asks every test binary `--ignored --list` and every package's
doc-test set the same. That is the authoritative count, and it is why that
script runs in `just standard` and not here -- it needs every workspace test
binary linked, which Tier 0 does not do and must not start doing.

The cost of that is what happened on 2026-09-23. Commit 82425837 added an
#[ignore]d reference arm to rnsd_interop; its gate was `just fast`, which runs
no census at all; the bucket grew by one unobserved, and the land gate on
9b9725ab went red in its last step with 30 commits already queued behind it.
The author could not have seen it. This script is so that the next one can:
same pin file, same selectors, same failure message, ~0.2 s, no build.

HOW IT COUNTS

Every `.rs` file inside a workspace package is attributed to the test unit that
compiles it, by matching it against each target's module root (`src/` for a
lib or bin crate root, `tests/<name>/` for a `tests/<name>/main.rs`, and so on;
longest root wins, an exact `src_path` match beats all). In an attributed file:

  * a line whose first non-space token is `#[ignore` counts one ignored test
    against that unit -- prose mentions of the attribute inside `///`, `//!`
    and `//` comments do not match, which is the whole reason the check is
    anchored at the start of the line;
  * an opening ```-fence in a `///` or `//!` comment whose info string carries
    `ignore` counts one ignored doc-test against `-p <pkg> --doc`, for the
    packages whose lib target has doctests enabled.

Files outside every workspace package are out of scope exactly as they are for
the binary census: vendor/crossterm, vendor/termbg, leviculum-nrf and
leviculum-esp sit outside the workspace (Cargo.toml `exclude`), and nothing in
this repository's pin file describes them.

THE ONE WAY IT CAN DIFFER FROM THE BINARY COUNT

A cfg-gated test. `#[cfg(feature = "x")] #[test] #[ignore]` is one line of
source either way, so this script always counts it; the binary census counts
it only when the feature is on in the build it inspects. The same holds for
`#[cfg(target_os = ...)]` and for a whole `#[cfg(...)] mod tests`. When the two
disagree for that reason the binary census in `just standard` is right and this
one is not, and the pin file follows the binary census. That is accepted:
this gate exists to catch the ordinary case -- a test gaining an #[ignore] --
minutes after it is written rather than an hour into the land gate, and the
workspace has no cfg-gated ignored test today (checked 2026-09-23, the two
censuses agree unit for unit).

A whole TARGET behind `required-features` is the same disagreement one level
up, and that one this script does follow: the binary census builds
`cargo test --workspace` with default features, so a target whose required
features that build does not switch on is never linked and has no unit there.
`enabled_features` works out, from `cargo metadata`, which features of each
workspace package that build turns on (its defaults, what workspace dependents
ask of it, and everything those reach through `dep/feature`), and a target
whose required features are not all among them is skipped, its files with it.
Where the arithmetic cannot tell (a `dep?/feature`, an optional dependency) it
counts the feature as on: a wrong guess then shows as a red mismatch here,
never as a silently skipped test. Order 1658: the mailbox growth soak
(`-p loquax --test mailbox_growth_soak`, required-features = ["tui"]) was
pinned to satisfy this gate, and the binary census, which never sees the
unit, sent the land gate on dcb99b75 red on the pin.

Usage:
    python3 scripts/check-ignored-counts-source.py           # the gate
    python3 scripts/check-ignored-counts-source.py --print   # the census
"""

from __future__ import annotations

import importlib.util
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PIN_FILE_REL = Path("scripts") / "ignored-counts.txt"

# The pin format and the selector spelling have exactly one definition, in the
# binary census, and this script borrows it rather than keeping a second copy
# that drifts the first time either is sharpened.
_SPEC = importlib.util.spec_from_file_location(
    "ignored_counts_binary", ROOT / "scripts" / "check-ignored-counts.py"
)
_BINARY = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(_BINARY)
read_pins = _BINARY.read_pins
selector_for = _BINARY.selector_for

IGNORE_ATTR = re.compile(r"^\s*#\[\s*ignore\b")
DOC_LINE = re.compile(r"^\s*//[/!](.*)$")
FENCE = re.compile(r"^\s*(?P<ticks>`{3,})(?P<info>.*)$")


def cargo_metadata() -> dict:
    proc = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        sys.exit("[ignored-source] `cargo metadata` failed")
    return json.loads(proc.stdout)


# The selector of a target the default-features workspace build never links.
# Its files are claimed (so the lib's `src/` does not swallow a skipped
# `src/bin/x.rs`) and then not counted.
SKIPPED = ""


def enabled_features(meta: dict) -> dict[str, set[str]]:
    """The features of each workspace package that `cargo test --workspace`
    with default features switches on, unified across the workspace."""
    packages = {p["name"]: p for p in meta["packages"]}
    enabled: dict[str, set[str]] = {name: set() for name in packages}
    queue: list[tuple[str, str]] = [(name, "default") for name in packages]

    for pkg in packages.values():
        for dep in pkg["dependencies"]:
            if dep["name"] not in packages or dep.get("optional"):
                continue
            queue += [(dep["name"], f) for f in dep.get("features", [])]
            if dep.get("uses_default_features", True):
                queue.append((dep["name"], "default"))

    while queue:
        name, feature = queue.pop()
        if feature in enabled[name]:
            continue
        enabled[name].add(feature)
        pkg = packages[name]
        # A dependency is named in a feature by its rename if it has one.
        local = {d.get("rename") or d["name"]: d["name"] for d in pkg["dependencies"]}
        for entry in pkg["features"].get(feature, []):
            if entry.startswith("dep:"):
                continue
            if "/" in entry:
                dep, _, sub = entry.partition("/")
                dep = local.get(dep.rstrip("?"))
                if dep in packages:
                    queue.append((dep, sub))
                continue
            if entry in pkg["features"]:
                queue.append((name, entry))
    return enabled


def targets() -> tuple[list[tuple[Path, Path, bool, str]], dict[Path, str]]:
    """(module root, src_path, is-crate-root, selector) per target, and the
    package directory each doc-test unit belongs to. A target the default
    workspace build does not link carries the selector SKIPPED."""
    meta = cargo_metadata()
    enabled = enabled_features(meta)
    units: list[tuple[Path, Path, bool, str]] = []
    doc_units: dict[Path, str] = {}
    for pkg in meta["packages"]:
        pkg_dir = Path(pkg["manifest_path"]).parent
        for target in pkg["targets"]:
            if "custom-build" in target["kind"]:
                continue
            src = Path(target["src_path"])
            root_is_crate = src.stem in ("lib", "main")
            root = src.parent if root_is_crate else src.parent / src.stem
            required = set(target.get("required-features", []))
            selector = (
                selector_for(pkg["name"], target)
                if required <= enabled[pkg["name"]]
                else SKIPPED
            )
            units.append((root, src, root_is_crate, selector))
            is_lib = "lib" in target["kind"] or "proc-macro" in target["kind"]
            if is_lib and target.get("doctest", True):
                doc_units[pkg_dir] = f"-p {pkg['name']} --doc"
    return units, doc_units


def owner(path: Path, units: list[tuple[Path, Path, bool, str]]) -> str | None:
    """The test unit that compiles `path`, or None if no target reaches it."""
    best: tuple[tuple[int, int, int], str] | None = None
    for root, src, root_is_crate, selector in units:
        if path == src:
            return selector
        if root not in path.parents:
            continue
        # Longest module root wins; a crate root's whole `src/` is the weakest
        # claim, so `src/bin/foo/helper.rs` goes to the bin and not to the lib.
        # Between a lib and a bin sharing `src/`, the lib holds the modules.
        key = (len(root.parts), 0 if root_is_crate else 1, 1 if "--lib" in selector else 0)
        if best is None or key > best[0]:
            best = (key, selector)
    return best[1] if best else None


def count_file(text: str) -> tuple[int, int]:
    """(ignored test attributes, ignored doc-test fences) in one file."""
    attrs = 0
    fences = 0
    in_fence = False
    for line in text.splitlines():
        if IGNORE_ATTR.match(line):
            attrs += 1
            continue
        doc = DOC_LINE.match(line)
        if not doc:
            continue
        fence = FENCE.match(doc.group(1))
        if not fence:
            continue
        if in_fence:
            in_fence = False
            continue
        in_fence = True
        tokens = fence.group("info").replace(",", " ").split()
        if any(t == "ignore" or t.startswith("ignore-") for t in tokens):
            fences += 1
    return attrs, fences


def census() -> tuple[dict[str, int], list[Path]]:
    """The source count per unit, and the ignored tests it could not place."""
    units, doc_units = targets()

    files: set[Path] = set()
    for root, src, _, _ in units:
        files.add(src)
        if root.is_dir():
            files.update(root.rglob("*.rs"))

    counts: dict[str, int] = {}
    unattributed: list[Path] = []
    for path in sorted(files):
        if not path.is_file():
            continue
        attrs, fences = count_file(path.read_text(errors="replace"))
        if not attrs and not fences:
            continue
        selector = owner(path, units)
        if selector is None:
            unattributed.append(path)
            continue
        if selector == SKIPPED:
            continue
        if attrs:
            counts[selector] = counts.get(selector, 0) + attrs
        if not fences or "--lib" not in selector:
            continue
        # A ```ignore fence is a doc-test, and doc-tests are their own unit.
        pkg_dir = next((d for d in doc_units if d in path.parents), None)
        if pkg_dir is not None:
            doc = doc_units[pkg_dir]
            counts[doc] = counts.get(doc, 0) + fences
    return counts, unattributed


def mismatch(selector: str, pinned: int, found: int) -> str:
    direction = "grew" if found > pinned else "shrank"
    return f"""
  {selector}
      pinned {pinned}, the sources hold {found} -- the bucket {direction}.

An #[ignore]d test is run by nothing: not by `just fast`, not by
`just standard`, not by any tier (Codeberg #189, #191c). Adding one is a
decision, so it lands as a diff on the pin file with the reason in the commit
message, or the #[ignore] comes off and the test rides its suite.

  grew:    raise the number, and say in the COMMIT MESSAGE why the test
           cannot be routed into a tier and where it is run instead.
  shrank:  lower it in the same commit as the routing or the deletion. A
           pin above the real count is that many free slots for the next
           ignored test to arrive in unseen.

`python3 scripts/check-ignored-counts.py` in `just standard` asks the built
test binaries the same question and prints the fuller guidance. This gate is
that one moved forward to the push, counted from the sources so it needs no
build; where the two disagree, the binary census is the one the pin follows
(see this script's header -- a cfg-gated test is the way it can happen).
"""


def main() -> int:
    counts, unattributed = census()
    pins = read_pins()

    if "--print" in sys.argv[1:]:
        for selector, count in sorted(counts.items()):
            print(f"{count:<4} {selector}")
        return 0

    failures = [
        mismatch(selector, pins.get(selector, 0), counts.get(selector, 0))
        for selector in sorted(set(counts) | set(pins))
        if counts.get(selector, 0) != pins.get(selector, 0)
    ]
    failures += [
        f"""
  {path.relative_to(ROOT)}
      carries an #[ignore]d test but no cargo target compiles it, so this
      gate cannot attribute it to a unit in {PIN_FILE_REL}. A helper module
      pulled in by `#[path]` from several targets is the usual cause; move
      the test to the target that owns it.
"""
        for path in unattributed
    ]

    if failures:
        print("[ignored-source] FAIL: the #[ignore]d tests in the sources no longer")
        print(f"[ignored-source] match the pinned census in {PIN_FILE_REL}.")
        for failure in failures:
            print(failure)
        return 1

    total = sum(counts.values())
    print(
        f"[ignored-source] OK: {total} ignored tests across {len(counts)} units, "
        f"counted from the sources, all matching {PIN_FILE_REL}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
