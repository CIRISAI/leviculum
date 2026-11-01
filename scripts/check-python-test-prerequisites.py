#!/usr/bin/env python3
"""Every test that spawns Python is declared in scripts/integ-prerequisites.txt.

A test that starts a Python process needs something a clone does not carry by
itself: the vendored reference stacks under reference/ for the Python peers,
and at least a `python3` on PATH for the rest. The runners that select tests
from the tree (scripts/ci-gate-integ.sh, scripts/standard-integ.sh,
scripts/with-prerequisites.sh) leave such a test out exactly when its line in
scripts/integ-prerequisites.txt names a path that is absent. A test with no
line runs anyway and fails there: the land gate on d365c0d2 went red in its
forge stage (no submodules) on 1274's
`home_reach_a_python_entry_holds_none_of_the_mailboxs_announces`, whose
Python peer exited before READY, while every other test that starts that peer
had its line (1282). Writing the line was a convention; this makes it a check.

WHAT COUNTS AS SPAWNING PYTHON. A function whose body holds
`Command::new("python...")`, and every function that calls one, followed
through the target's own modules (`mod x;`, `#[path] mod x;`, inline `mod`):
free functions by name, associated functions by `Type::name`/`Self::name`,
methods by `.name(`. A command line held in a string and run through a shell
(`format!("python3 {BRIDGE} ...")` in rnsd_interop) is not seen; that target
is declared whole. A `#[test]` or `#[tokio::test]` function that reaches a
spawn needs a line naming its target (the whole target) or its exact name in
that target. The call graph is read from the text, not from rustc, so it is
an over-approximation where two items share a name; a false positive is
answered with a `command:python3` line, which is true of the test anyway and
excludes it nowhere a gate runs.

WHAT IT SUGGESTS. The line to add, by what the reached spawn site's file
names: `scripts/test_daemon.py` or `reference/LXMF` -> the RNS and the LXMF
lines, `reference/Reticulum` -> the RNS line, otherwise `command:python3`.
Check the suggestion against what the test really needs; the file's header
says how to measure it.

Exit 0 = every Python-spawning test is declared.
Exit 1 = one is not (file:line, test, the line to add), or the selftest failed.

Usage:
  python3 scripts/check-python-test-prerequisites.py [--root DIR] [--prerequisites FILE]
  python3 scripts/check-python-test-prerequisites.py --selftest
"""

import argparse
import os
import re
import shutil
import sys
import tempfile

REPO_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PREREQUISITES = "scripts/integ-prerequisites.txt"
RNS = "reference/Reticulum/RNS/__init__.py"
LXMF = "reference/LXMF/LXMF/__init__.py"

SPAWN = re.compile(r'Command\s*::\s*new\s*\(\s*"python')
TEST_ATTR = re.compile(r"#\[\s*(?:tokio\s*::\s*)?test\b")
FN = re.compile(r"\bfn\s+(\w+)")
INLINE_MOD = re.compile(r"\bmod\s+(\w+)\s*$")
IMPL = re.compile(r"\bimpl\b")
MOD_DECL = re.compile(r"\bmod\s+(\w+)\s*;")
PATH_ATTR = re.compile(r'#\[\s*path\s*=\s*"([^"]*)"\s*\]')
QUALIFIED_CALL = re.compile(r"\b(\w+)\s*::\s*(\w+)\s*(?:::\s*<[^()]*?>\s*)?\(")
BARE_CALL = re.compile(r"(?<![\w.:])(\w+)\s*(?:::\s*<[^()]*?>\s*)?\(")
METHOD_CALL = re.compile(r"\.\s*(\w+)\s*(?:::\s*<[^()]*?>\s*)?\(")


def members(root):
    """The workspace members, as scripts/integ-targets.sh reads them."""
    with open(os.path.join(root, "Cargo.toml"), encoding="utf-8") as handle:
        text = handle.read()
    block = re.search(r"^members = \[(.*?)^\]", text, re.M | re.S)
    if not block:
        return []
    return re.findall(r'^\s*"([^"]*)"', block.group(1), re.M)


def targets(root):
    """{`<dir>/<name>`: root file}, by cargo's autodiscovery rule."""
    out = {}
    for member in members(root):
        tests = os.path.join(root, member, "tests")
        if not os.path.isdir(tests):
            continue
        for entry in sorted(os.listdir(tests)):
            path = os.path.join(tests, entry)
            if entry.endswith(".rs") and os.path.isfile(path):
                out[f"{member}/{entry[:-3]}"] = path
            elif os.path.isfile(os.path.join(path, "main.rs")):
                out[f"{member}/{entry}"] = os.path.join(path, "main.rs")
    return out


def mask(text):
    """`text` with comments and the insides of string and char literals blanked,
    offsets and newlines kept, so braces and calls can be read by position."""
    out = list(text)
    i, n = 0, len(text)

    def blank(a, b):
        for k in range(a, b):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        c = text[i]
        if text.startswith("//", i):
            j = text.find("\n", i)
            j = n if j < 0 else j
            blank(i, j)
            i = j
        elif text.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif text.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            blank(i, j)
            i = j
        elif c == "r" and re.match(r'r#*"', text[i : i + 300]) and not (
            i and (text[i - 1].isalnum() or text[i - 1] == "_")
        ):
            hashes = re.match(r"r(#*)\"", text[i:]).group(1)
            start = i + 2 + len(hashes)
            end = text.find('"' + hashes, start)
            end = n if end < 0 else end
            blank(start, end)
            i = end + 1 + len(hashes)
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            blank(i + 1, min(j, n))
            i = j + 1
        elif c == "'":
            m = re.match(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'])'", text[i : i + 16])
            if m:
                blank(i + 1, i + len(m.group(0)) - 1)
                i += len(m.group(0))
            else:
                i += 1
        else:
            i += 1
    return "".join(out)


def header_start(masked, brace):
    """Where the item that opens at `brace` begins: back to a `;`, `{` or `}`
    outside parentheses and brackets."""
    depth, j = 0, brace - 1
    while j >= 0:
        c = masked[j]
        if c in ")]":
            depth += 1
        elif c in "([":
            depth -= 1
        elif depth <= 0 and c in ";{}":
            return j + 1
        j -= 1
    return 0


def impl_type(header):
    """The self type of an `impl` header: the last path segment before `{`,
    after `for` when the header has one."""
    body = header[IMPL.search(header).end() :]
    body = re.split(r"\bwhere\b", body)[0]
    previous = None
    while previous != body:
        previous, body = body, re.sub(r"<[^<>]*>", "", body)
    if re.search(r"\bfor\b", body):
        body = re.split(r"\bfor\b", body)[-1]
    names = re.findall(r"\w+", body)
    return names[-1] if names else "?"


class Fn:
    def __init__(self, name, qual, impl, test, path, line, body, start, end):
        self.name, self.qual, self.impl, self.test = name, qual, impl, test
        self.path, self.line, self.body = path, line, body
        self.start, self.end = start, end
        self.method = bool(re.search(r"\(\s*&?\s*(?:'\w+\s+)?(?:mut\s+)?self\b", body))
        self.spawns = False
        self.via = None


def scan(path, modpath, child_dir, seen, fns):
    """Read one file of a target at module path `modpath`; recurse into its
    `mod` declarations. `child_dir` is where a plain `mod x;` here looks."""
    real = os.path.realpath(path)
    if real in seen or not os.path.isfile(path):
        return
    seen.add(real)
    with open(path, encoding="utf-8") as handle:
        text = handle.read()
    masked = mask(text)
    spawns = [m.start() for m in SPAWN.finditer(text)]
    line_of = lambda off: text.count("\n", 0, off) + 1  # noqa: E731

    # Every brace pair, classified by the header in front of it.
    stack, scopes = [], []
    for i, c in enumerate(masked):
        if c == "{":
            h0 = header_start(masked, i)
            header = masked[h0:i]
            m = FN.search(header)
            if m:
                kind, name = "fn", m.group(1)
            elif INLINE_MOD.search(header.rstrip()):
                kind, name = "mod", INLINE_MOD.search(header.rstrip()).group(1)
            elif IMPL.search(header):
                kind, name = "impl", impl_type(header)
            else:
                kind, name = "block", None
            stack.append([kind, name, h0, i, None])
        elif c == "}" and stack:
            entry = stack.pop()
            entry[4] = i
            scopes.append((entry, [tuple(s[:2]) for s in stack]))

    def enclosing(off):
        """The scope chain (outermost first) around offset `off`."""
        chain = []
        for (kind, name, h0, start, end), _ in scopes:
            if start < off < end:
                chain.append((start, kind, name))
        return [(k, n) for _, k, n in sorted(chain)]

    for (kind, name, h0, start, end), outer in scopes:
        if kind != "fn":
            continue
        mods = [n for k, n in outer if k == "mod"]
        impls = [n for k, n in outer if k == "impl"]
        fn = Fn(
            name,
            "::".join(modpath + mods + impls[-1:] + [name]),
            impls[-1] if impls else None,
            bool(TEST_ATTR.search(text[h0:start])) and not impls,
            path,
            line_of(h0 + FN.search(masked[h0:start]).start()),
            masked[h0 : end + 1],
            start,
            end,
        )
        fns.append(fn)
    # A spawn belongs to the innermost function around it.
    mine = [f for f in fns if f.path == path]
    for off in spawns:
        inner = [f for f in mine if f.start < off < f.end]
        if inner:
            f = min(inner, key=lambda f: f.end - f.start)
            f.spawns = True
            f.via = f"{os.path.relpath(path, REPO_ROOT[0])}:{line_of(off)}"

    for m in MOD_DECL.finditer(masked):
        chain = enclosing(m.start())
        if any(k in ("fn", "impl", "block") for k, _ in chain):
            continue
        inline = [n for k, n in chain if k == "mod"]
        name = m.group(1)
        attrs = text[header_start(masked, m.start()) : m.start()]
        explicit = PATH_ATTR.search(attrs)
        base = os.path.join(child_dir, *inline)
        if explicit:
            if inline:
                target = os.path.join(base, explicit.group(1))
            else:
                target = os.path.join(os.path.dirname(path), explicit.group(1))
            target = os.path.normpath(target)
            if os.path.basename(target) == "mod.rs":
                below = os.path.dirname(target)
            else:
                below = target[:-3]
        else:
            flat = os.path.join(base, name + ".rs")
            nested = os.path.join(base, name, "mod.rs")
            target = flat if os.path.isfile(flat) else nested
            below = os.path.join(base, name)
        scan(target, modpath + inline + [name], below, seen, fns)


REPO_ROOT = [REPO_DIR]


def reaching(fns):
    """Mark every function that reaches a spawn; fixed point over the calls."""
    free, assoc, methods = {}, {}, {}
    for f in fns:
        if f.impl is None:
            free.setdefault(f.name, []).append(f)
        else:
            assoc.setdefault((f.impl, f.name), []).append(f)
            if f.method:
                methods.setdefault(f.name, []).append(f)
    mods = {}
    for f in fns:
        if f.impl is None and "::" in f.qual:
            mods.setdefault((f.qual.split("::")[-2], f.name), []).append(f)
    calls = {}
    for f in fns:
        body = f.body[f.body.find("{") :]
        out = []
        for m in QUALIFIED_CALL.finditer(body):
            qual, name = m.group(1), m.group(2)
            owner = f.impl if qual == "Self" else qual
            out += assoc.get((owner, name), []) + mods.get((qual, name), [])
        for m in BARE_CALL.finditer(body):
            out += free.get(m.group(1), [])
        for m in METHOD_CALL.finditer(body):
            out += methods.get(m.group(1), [])
        calls[id(f)] = [g for g in out if g is not f]
    changed = True
    while changed:
        changed = False
        for f in fns:
            if f.spawns:
                continue
            for g in calls[id(f)]:
                if g.spawns:
                    f.spawns, f.via = True, f"{g.qual} ({g.via})"
                    changed = True
                    break


def spawn_sites(f, fns):
    """The files of the spawn sites `f` reaches, through its `via` chain."""
    files = set()
    for g in fns:
        if g.spawns and g.via and re.match(r"[^ ]+:\d+$", g.via):
            if g.via in (f.via or "") or g is f:
                files.add(g.path)
    return files


def suggestion(files):
    """The prerequisite paths a spawn site's file points at."""
    text = ""
    for path in files:
        with open(path, encoding="utf-8") as handle:
            text += handle.read()
    if "test_daemon.py" in text or "reference/LXMF" in text:
        return [RNS, LXMF]
    if "reference/Reticulum" in text:
        return [RNS]
    return ["command:python3"]


def declared(path):
    """({whole targets}, {(target, test)}) of the prerequisite file."""
    whole, named = set(), set()
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            fields = line.split()
            if not fields or fields[0].startswith("#") or len(fields) < 2:
                continue
            if len(fields) == 2:
                whole.add(fields[1])
            else:
                named.add((fields[1], fields[2]))
    return whole, named


def findings(root, prerequisites=None):
    REPO_ROOT[0] = root
    whole, named = declared(prerequisites or os.path.join(root, PREREQUISITES))
    out = []
    for target, path in sorted(targets(root).items()):
        if target in whole:
            continue
        fns = []
        child_dir = os.path.dirname(path)
        scan(path, [], child_dir, set(), fns)
        if not any(f.spawns for f in fns):
            continue
        reaching(fns)
        for f in fns:
            if not (f.test and f.spawns) or (target, f.qual) in named:
                continue
            lines = "\n".join(
                f"    {p} {target} {f.qual}" for p in suggestion(spawn_sites(f, fns))
            )
            out.append(
                f"{os.path.relpath(f.path, root)}:{f.line}: test {f.qual} of "
                f"{target} spawns Python (via {f.via}), and {PREREQUISITES} "
                f"has no line for it, so a runner without the prerequisite "
                f"runs it and it fails there (1282). Add, with its reason:\n{lines}"
            )
    return out


FIXTURE_CARGO = 'members = [\n    "a",\n]\n'
FIXTURE_PEER = """\
pub struct Peer;
impl Peer {
    pub fn start() -> Peer {
        // reference/Reticulum and scripts/test_daemon.py
        let _ = std::process::Command::new("python3").arg("x").spawn();
        Peer
    }
}
"""
FIXTURE_TEST = """\
#[path = "support/peer.rs"]
mod peer;

fn helper() -> peer::Peer {
    peer::Peer::start()
}

#[tokio::test(flavor = "multi_thread")]
async fn direct() {
    let _p = peer::Peer::start();
}

#[test]
fn through_a_helper() {
    let _p = helper();
}

#[test]
fn no_python() {
    let s = "Command::new(\\"python3\\")"; // a string, not a call
    assert!(!s.is_empty());
}
"""
FIXTURE_SCRIPT = """\
use std::process::Command;
#[test]
fn runs_a_script() {
    Command::new("python3").arg("-c").arg("pass").status().unwrap();
}
"""
FIXTURE_MVR = """\
mod inner;
"""
FIXTURE_MVR_INNER = """\
mod tests {
    #[test]
    fn spawns() {
        let _ = std::process::Command::new("python3").status();
    }
}
"""
FIXTURE_PREREQ_FULL = f"""\
# fixture
{RNS} a/interop direct
{LXMF} a/interop direct
{RNS} a/interop through_a_helper
command:python3 a/script
command:python3 a/mvr inner::tests::spawns
"""


def selftest():
    failures = []
    with tempfile.TemporaryDirectory() as tmp:
        files = {
            "Cargo.toml": FIXTURE_CARGO,
            "a/tests/interop.rs": FIXTURE_TEST,
            "a/tests/support/peer.rs": FIXTURE_PEER,
            "a/tests/script.rs": FIXTURE_SCRIPT,
            "a/tests/mvr/main.rs": FIXTURE_MVR,
            "a/tests/mvr/inner.rs": FIXTURE_MVR_INNER,
            PREREQUISITES: FIXTURE_PREREQ_FULL,
        }
        for rel, body in files.items():
            os.makedirs(os.path.dirname(os.path.join(tmp, rel)), exist_ok=True)
            with open(os.path.join(tmp, rel), "w", encoding="utf-8") as handle:
                handle.write(body)

        def expect(label, want, count=1):
            got = findings(tmp)
            joined = "\n".join(got)
            if len(got) != count or not all(w in joined for w in want):
                failures.append(f"{label}: want {want}, got:\n{joined or '(none)'}")

        expect("fully declared", [], 0)
        for line, want in [
            (
                f"{RNS} a/interop through_a_helper",
                ["a/tests/interop.rs:14: test through_a_helper", f"{RNS} a/interop through_a_helper"],
            ),
            (
                f"{RNS} a/interop direct\n{LXMF} a/interop direct",
                ["a/tests/interop.rs:9: test direct", f"{LXMF} a/interop direct"],
            ),
            ("command:python3 a/script", ["test runs_a_script", "command:python3 a/script runs_a_script"]),
            ("command:python3 a/mvr inner::tests::spawns", ["test inner::tests::spawns of a/mvr"]),
        ]:
            with open(os.path.join(tmp, PREREQUISITES), "w", encoding="utf-8") as handle:
                handle.write(FIXTURE_PREREQ_FULL.replace(line + "\n", ""))
            expect(f"without `{line}`", want)
        # A whole-target line covers every test of the target.
        with open(os.path.join(tmp, PREREQUISITES), "w", encoding="utf-8") as handle:
            handle.write(f"{RNS} a/interop\ncommand:python3 a/script\ncommand:python3 a/mvr\n")
        expect("whole targets", [], 0)
    for failure in failures:
        print(f"check-python-test-prerequisites selftest: {failure}", file=sys.stderr)
    return not failures


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", default=REPO_DIR)
    parser.add_argument(
        "--prerequisites",
        help=f"read this file instead of the tree's {PREREQUISITES} (a positive control)",
    )
    parser.add_argument("--selftest", action="store_true")
    args = parser.parse_args()
    if not selftest():
        return 1
    if args.selftest:
        print("check-python-test-prerequisites: selftest ok")
        return 0
    out = findings(os.path.abspath(args.root), args.prerequisites)
    for finding in out:
        print(f"check-python-test-prerequisites: {finding}", file=sys.stderr)
    if out:
        print(
            f"check-python-test-prerequisites: FAILED, {len(out)} test(s) spawn "
            f"Python without a line in {PREREQUISITES}.",
            file=sys.stderr,
        )
        return 1
    print("check-python-test-prerequisites: every Python-spawning test is declared")
    return 0


if __name__ == "__main__":
    sys.exit(main())
