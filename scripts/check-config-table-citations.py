#!/usr/bin/env python3
"""Config-reference table citations land on the key they document (676).

The config reference (`docs/src/guide/configuration.md`) gives every key a
row, and every row cites two places: the struct member the key fills,
spelled `` `InterfaceConfig::min_rssi` (`config.rs:838`) ``, and the INI
parser arm that reads it, spelled `` `min_rssi` (`ini_config.rs:893`) ``.
Both are named citations, and the citation guard
(`leviculum-std/tests/doc_citations.rs`) holds a named citation to "the name
occurs within 8 lines of the span". In a struct whose members are three lines
apart, each with a doc comment that repeats the key, that window spans three
neighbouring members: 674 found every `config.rs` citation in the BLE table
pointing at the member above the one it named, and the guard green.

This check is strict where the guard is lenient, and only for these two
shapes, in table rows of `docs/src/**/*.md`:

  * `` `Type::member` (`...config.rs:A[-B]`) `` -- the span must contain the
    member's definition (`pub member:` inside `struct Type`, or
    `fn member(`), and must not start above the `///` doc comment and
    attributes directly over it, nor end past the definition (a field) or its closing
    brace (a fn);
  * `` `key` (`...ini_config.rs:A[-B]`) `` -- the span must contain the line
    holding the string literal `"key"`, and must stay inside that match arm
    and the comment directly above it.

A failure prints the span that would satisfy it. `--fix` writes those spans
into the documents, which is safe only because the rule is exact: the
suggestion is derived from the definition the citation names, not from the
nearest occurrence of a word.

Exit 0 = every such citation holds (and the fixtures behaved).
Exit 1 = a citation does not, or the checker's own fixtures did not behave.

Usage:
  python3 scripts/check-config-table-citations.py [--fix]
"""

import argparse
import os
import re
import subprocess
import sys
import tempfile

REPO_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

MEMBER_RE = re.compile(
    r"`(\w+)::(\w+)` \(`((?:[\w.-]+/)*config\.rs):(\d+)(?:-(\d+))?`\)"
)
KEY_RE = re.compile(r"`(\w+)` \(`((?:[\w.-]+/)*ini_config\.rs):(\d+)(?:-(\d+))?`\)")
# Over a struct member: its doc comment and attributes. Over a match arm,
# where nothing carries a doc comment: any comment.
MEMBER_PREAMBLE_RE = re.compile(r"^\s*(///|#\[)")
ARM_PREAMBLE_RE = re.compile(r"^\s*(//|#\[)")


def table_rows(text):
    """[(line number, line)] of markdown table rows outside code fences."""
    rows = []
    fenced = False
    for number, line in enumerate(text.splitlines(), start=1):
        if line.lstrip().startswith("```"):
            fenced = not fenced
            continue
        if not fenced and line.startswith("|"):
            rows.append((number, line))
    return rows


def preamble_start(lines, at, preamble):
    """First line (1-based) of the `preamble` lines directly over `at`."""
    start = at
    while start > 1 and preamble.match(lines[start - 2]):
        start -= 1
    return start


def block_end(lines, at):
    """Last line (1-based) of the item opened at `at`: its closing brace at the
    same indentation when the line opens a block, else `at` itself."""
    if not lines[at - 1].rstrip().endswith("{"):
        return at
    indent = len(lines[at - 1]) - len(lines[at - 1].lstrip())
    for number in range(at + 1, len(lines) + 1):
        text = lines[number - 1]
        if text.strip().startswith("}") and len(text) - len(text.lstrip()) == indent:
            return number
    return at


def member_target(lines, type_name, member):
    """(start, definition, end) of `type_name::member`, or None."""
    struct_re = re.compile(r"^\s*pub(\([\w:]+\))? struct " + re.escape(type_name) + r"\b.*\{\s*$")
    field_re = re.compile(r"^\s*pub(\([\w:]+\))? " + re.escape(member) + r"\s*:")
    for number, text in enumerate(lines, start=1):
        if struct_re.match(text):
            end = block_end(lines, number)
            for field in range(number + 1, end):
                if field_re.match(lines[field - 1]):
                    return preamble_start(lines, field, MEMBER_PREAMBLE_RE), field, field
    fn_re = re.compile(r"^\s*(pub(\([\w:]+\))? )?(const )?fn " + re.escape(member) + r"\s*[(<]")
    for number, text in enumerate(lines, start=1):
        if fn_re.match(text):
            return preamble_start(lines, number, MEMBER_PREAMBLE_RE), number, block_end(lines, number)
    return None


def key_targets(lines, key):
    """[(start, literal line, end)] of every match arm that reads `key`. The
    interface keys are read twice, by the interface's own arms and by the
    sub-interface's, so a citation may land on either."""
    arm = re.compile(r'^\s*(\| )?("\w+" \| )*"' + re.escape(key) + r'"( \| "\w+")* =>')
    hits = [n for n, text in enumerate(lines, start=1) if arm.match(text)]
    return [(preamble_start(lines, at, ARM_PREAMBLE_RE), at, block_end(lines, at)) for at in hits] or None


def resolve(root, files, cited, want):
    """The one repo file `cited` names that `want(lines)` accepts."""
    suffix = "/" + cited
    found = []
    for rel in files:
        if rel == cited or rel.endswith(suffix):
            with open(os.path.join(root, rel), encoding="utf-8") as handle:
                lines = handle.read().splitlines()
            if want(lines) is not None:
                found.append((rel, lines))
    return found


def spell(start, end):
    return str(start) if start == end else f"{start}-{end}"


def check(root, files, docs, fix=False):
    """Failures as strings; with `fix`, rewrite every repairable citation."""
    failures = []
    for doc in docs:
        path = os.path.join(root, doc)
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
        lines = text.splitlines(keepends=True)
        changed = False
        for number, row in table_rows(text):
            new_row = row
            for m in list(MEMBER_RE.finditer(row)) + list(KEY_RE.finditer(row)):
                if m.re is MEMBER_RE:
                    type_name, member, cited = m.group(1), m.group(2), m.group(3)
                    a, b = int(m.group(4)), int(m.group(5) or m.group(4))
                    subject = f"{type_name}::{member}"
                    target = lambda ls, t=type_name, mb=member: (
                        None if (hit := member_target(ls, t, mb)) is None else [hit]
                    )
                else:
                    key, cited = m.group(1), m.group(2)
                    a, b = int(m.group(3)), int(m.group(4) or m.group(3))
                    subject = f'"{key}"'
                    target = lambda ls, k=key: key_targets(ls, k)
                found = resolve(root, files, cited, target)
                where = f"{doc}:{number}: {m.group(0)}"
                if len(found) != 1:
                    failures.append(
                        f"{where}\n    {subject} resolves in {len(found)} file(s) "
                        f"matching `{cited}`; it must resolve in exactly one"
                    )
                    continue
                rel, src = found[0]
                targets = target(src)
                if any(start <= a <= at <= b <= end for start, at, end in targets):
                    continue
                start, at, end = min(targets, key=lambda t: abs(t[1] - a))
                want = spell(start, at) if m.re is MEMBER_RE else spell(at, end)
                failures.append(
                    f"{where}\n    {subject} is defined at {rel}:{at} "
                    f"(its item spans {spell(start, end)}); the citation should read "
                    f"`{cited}:{want}`"
                )
                new_row = new_row.replace(
                    m.group(0), m.group(0).replace(f"{cited}:{spell(a, b)}", f"{cited}:{want}"), 1
                )
            if new_row != row:
                lines[number - 1] = new_row + "\n"
                changed = True
        if fix and changed:
            with open(path, "w", encoding="utf-8") as handle:
                handle.write("".join(lines))
    return failures


FIXTURE_SRC = """\
pub struct InterfaceConfig {
    /// The adapter (`device`).
    pub device: Option<String>,
    /// Link cap (`max_connections`).
    #[serde(default)]
    pub max_connections: Option<usize>,
}
"""

FIXTURE_INI = """\
fn apply(key: &str) {
    match key {
        "device" => {}
        // A comment over the arm.
        "max_connections" => {
            parse();
        }
    }
}
"""


def selftest():
    """The verdicts on a fixture tree, the moved-line case first."""
    cases = [
        # (doc row, expected failures)
        ("| `InterfaceConfig::max_connections` (`config.rs:4-6`) |", 0),
        ("| `InterfaceConfig::max_connections` (`config.rs:6`) |", 0),
        # One line off in either direction: the guard's window holds both.
        ("| `InterfaceConfig::max_connections` (`config.rs:2-3`) |", 1),
        ("| `InterfaceConfig::max_connections` (`config.rs:3-6`) |", 1),
        ("| `InterfaceConfig::nonexistent` (`config.rs:6`) |", 1),
        ("| `max_connections` (`ini_config.rs:5-7`) |", 0),
        ("| `max_connections` (`ini_config.rs:4-5`) |", 0),
        ("| `max_connections` (`ini_config.rs:3`) |", 1),
        ("| `device` (`ini_config.rs:3`) |", 0),
    ]
    with tempfile.TemporaryDirectory() as root:
        os.makedirs(os.path.join(root, "src"))
        os.makedirs(os.path.join(root, "docs"))
        for name, body in (("src/config.rs", FIXTURE_SRC), ("src/ini_config.rs", FIXTURE_INI)):
            with open(os.path.join(root, name), "w", encoding="utf-8") as handle:
                handle.write(body)
        files = ["src/config.rs", "src/ini_config.rs"]
        for row, expected in cases:
            with open(os.path.join(root, "docs/t.md"), "w", encoding="utf-8") as handle:
                handle.write("| Key |\n|-----|\n" + row + "\n")
            got = check(root, files, ["docs/t.md"])
            if len(got) != expected:
                print(f"selftest: {row!r}: expected {expected} failure(s), got {got}")
                return False
        # The repair lands on the definition and then passes.
        with open(os.path.join(root, "docs/t.md"), "w", encoding="utf-8") as handle:
            handle.write("| `InterfaceConfig::max_connections` (`config.rs:1-2`) |\n")
        check(root, files, ["docs/t.md"], fix=True)
        with open(os.path.join(root, "docs/t.md"), encoding="utf-8") as handle:
            repaired = handle.read()
        if "`config.rs:4-6`" not in repaired or check(root, files, ["docs/t.md"]):
            print(f"selftest: repair did not land on the definition: {repaired!r}")
            return False
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--fix", action="store_true", help="rewrite the citations")
    args = parser.parse_args()
    if not selftest():
        print("check-config-table-citations: FIXTURES FAILED")
        return 1
    files = subprocess.run(
        ["git", "ls-files"], cwd=REPO_DIR, capture_output=True, text=True, check=True
    ).stdout.split()
    docs = [f for f in files if f.startswith("docs/src/") and f.endswith(".md")]
    failures = check(REPO_DIR, files, docs, fix=args.fix)
    if failures:
        print("\n\n".join(failures))
        verb = "rewritten" if args.fix else "do not land on what they name"
        print(f"\ncheck-config-table-citations: {len(failures)} table citation(s) {verb}")
        return 0 if args.fix else 1
    print("check-config-table-citations: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
