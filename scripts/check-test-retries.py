#!/usr/bin/env python3
"""A test waits for a state; it never retries the request under test.

Orders 1159 and 1162 shipped loquax facade tests that pulled "up to three
times" until the conversation list showed an import. The retry hid a real
gap: an import beside a running daemon woke no watch, and the second pull
papered over it until 1164 fixed the watch. A test may poll a STATE until a
deadline. It may not re-issue the ACTION under test until that succeeds,
because then it asserts "eventually, after enough nudges" where the product
promises "once".

The rule, a text scan, sub-second, no build: inside the body of a `for`,
`loop` or `while` (nested bodies included, so the "three attempts, each with
its own wait loop" shape is caught on the outer loop), a call from REQUESTS
together with a call from WAITS is a finding. A loop that only reads state
(`conversations()`, `Query::`, `tile_stats`, `read_dir`, a channel
`try_recv`) has no REQUESTS call and passes. Only the innermost loop holding
both is reported. Comments and string literals are blanked before the scan,
and loops are matched by brace depth, not by a regex over the file.

A call to a fn or closure counts as a request (or a wait) when its body
issues one, transitively: a fn or closure of the same file by a bare call,
a top-level `pub fn` of a `tests/support` module also behind its path.
1162 hid `request(Request::Pull)`
and its wait for the sync in a `pulled` closure and called that in a
`for _ in 0..3` loop.

A walk is not a retry: a `for` over data (not a range) whose every request
call takes a name its pattern binds issues a different request each turn,
once each (`for (body, fields) in messages(..) { run_send(.., body ..) }`).
A `for attempt in 0..3` is a counter and stays a finding whatever it passes.

Scope: `loquax/tests/**`, `loquax-android/tests/**`,
`leviculum-std/tests/mvr/**`, and the `#[cfg(test)]` items of
`loquax/src/**` (a `#[cfg(test)] mod name;` pulls in its whole file).

An explicit allow: a line comment `// retry-guard: allow <reason>` directly
above the loop (attribute and comment lines may sit between). Every allow is
printed with its reason so the reviewer sees each one; an allow with no
reason is a finding.

Usage:
    python3 scripts/check-test-retries.py              # the gate
    python3 scripts/check-test-retries.py --selftest   # the positive control
"""

from __future__ import annotations

import re
import shutil
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Calls that issue a request or a send. Extend here; each entry is a regex
# matched against code with comments and strings blanked.
REQUESTS = [
    r"\bRequest::Pull\b",
    r"\bRequest::Send\b",
    r"\.pull\s*\(",
    r"\brun_send\s*\(",
    r"\bsend_home\s*\(",
    r"\bfacade::request\s*\(",
]

# A sleep or a deadline check: what makes a loop a wait rather than a walk.
WAITS = [
    r"\bsleep\s*\(",
    r"\bdeadline\b",
    r"\bInstant::now\s*\(",
    r"\.elapsed\s*\(",
    r"\btimeout\s*\(",
]

# Trees read whole, and the tree whose #[cfg(test)] items are read.
WHOLE = ["loquax/tests", "loquax-android/tests", "leviculum-std/tests/mvr"]
CFG_TEST = ["loquax/src"]

ALLOW = re.compile(r"^\s*//\s*retry-guard:\s*allow\b\s*(.*)$")
LOOP_KW = re.compile(r"\b(for|loop|while)\b")
CFG_TEST_ATTR = re.compile(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]")
HELPER_FN = re.compile(r"\bfn\s+(\w+)")
SUPPORT_FN = re.compile(r"^pub(?:\([^)]*\))?\s+(?:async\s+)?fn\s+(\w+)", re.M)
HELPER_CLOSURE = re.compile(r"\blet\s+(?:mut\s+)?(\w+)\s*(?::[^=;]*)?=\s*(?:move\s+)?\|")
MOD_DECL = re.compile(r"\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;")
REQUEST_RES = [re.compile(p) for p in REQUESTS]
WAIT_RES = [re.compile(p) for p in WAITS]


def blank(source: str) -> str:
    """Comments and string/char literals replaced by spaces, newlines kept.

    Offsets and line numbers of the result match the source, so a match in
    the blanked text names the same place in the file.
    """
    out = list(source)
    i, n = 0, len(source)

    def wipe(start: int, end: int) -> None:
        for k in range(start, min(end, n)):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        c = source[i]
        if source.startswith("//", i):
            end = source.find("\n", i)
            end = n if end < 0 else end
            wipe(i, end)
            i = end
        elif source.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if source.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif source.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            wipe(i, j)
            i = j
        elif (m := re.compile(r"b?r(#*)\"").match(source, i)) and (
            i == 0 or not (source[i - 1].isalnum() or source[i - 1] == "_")
        ):
            close = '"' + m.group(1)
            end = source.find(close, m.end())
            end = n if end < 0 else end + len(close)
            wipe(i, end)
            i = end
        elif c == '"':
            j = i + 1
            while j < n and source[j] != '"':
                j += 2 if source[j] == "\\" else 1
            wipe(i, j + 1)
            i = j + 1
        elif c == "'":
            # A char literal ('a', '\n', '\u{..}'), or a lifetime / label.
            m = re.compile(r"'(?:\\.[^']*|[^'\\\n])'").match(source, i)
            if m:
                wipe(i, m.end())
                i = m.end()
            else:
                i += 1
        else:
            i += 1
    return "".join(out)


def match_brace(code: str, open_at: int) -> int:
    """Offset of the `}` closing the `{` at open_at (len(code) if none)."""
    depth = 0
    for k in range(open_at, len(code)):
        if code[k] == "{":
            depth += 1
        elif code[k] == "}":
            depth -= 1
            if depth == 0:
                return k
    return len(code)


def group_after(code: str, at: int) -> str:
    """The (...) or {...} group starting at the first such opener at/after at."""
    k = at - 1 if at > 0 and code[at - 1] in "({" else at
    while k < len(code) and code[k] in " \t\n":
        k += 1
    if k >= len(code) or code[k] not in "({":
        return ""
    opener, closer = code[k], ")" if code[k] == "(" else "}"
    depth = 0
    for j in range(k, len(code)):
        if code[j] == opener:
            depth += 1
        elif code[j] == closer:
            depth -= 1
            if depth == 0:
                return code[k + 1 : j]
    return code[k + 1 :]


def walk_bindings(header: str) -> set[str]:
    """Names a `for` pattern binds, empty for a range (a counter, not data)."""
    m = re.match(r"\s*(.*?)\bin\b(.*)$", header, re.S)
    if not m or ".." in m.group(2):
        return set()
    names = set(re.findall(r"\b[a-z_][A-Za-z0-9_]*\b", m.group(1)))
    return names - {"_", "mut", "ref"}


def body_open(code: str, after: int, keyword: str) -> int | None:
    """The `{` opening the body of the loop whose keyword ends at `after`.

    The first `{` outside parentheses and brackets. For `for`, the header
    must contain `in`: `impl Trait for Type {` and `for<'a>` are not loops.
    """
    depth = 0
    for k in range(after, len(code)):
        c = code[k]
        if c in "([":
            depth += 1
        elif c in ")]":
            depth -= 1
        elif c == ";" and depth == 0:
            return None
        elif c == "{" and depth == 0:
            if keyword == "for" and not re.search(r"\bin\b", code[after:k]):
                return None
            return k
    return None


def line_of(code: str, offset: int) -> int:
    return code.count("\n", 0, offset) + 1


def cfg_test_spans(code: str) -> list[tuple[int, int]]:
    """(start, end) of every item a `#[cfg(test)]` attribute gates."""
    spans = []
    for m in CFG_TEST_ATTR.finditer(code):
        k = m.end()
        while k < len(code) and code[k] not in "{;":
            k += 1
        if k < len(code) and code[k] == "{":
            spans.append((m.start(), match_brace(code, k)))
    return spans


def cfg_test_mod_files(path: Path, code: str) -> list[Path]:
    """Files named by `#[cfg(test)] mod name;` in path."""
    found = []
    for m in CFG_TEST_ATTR.finditer(code):
        decl = MOD_DECL.match(code, m.end())
        if not decl:
            continue
        name = decl.group(1)
        base = path.parent if path.name in ("mod.rs", "lib.rs", "main.rs") else path.with_suffix("")
        for candidate in (base / f"{name}.rs", base / name / "mod.rs"):
            if candidate.exists():
                found.append(candidate)
                break
    return found


def allow_above(lines: list[str], line: int) -> tuple[bool, str] | None:
    """(found, reason) for a `retry-guard: allow` above 1-based `line`."""
    k = line - 2
    while k >= 0:
        text = lines[k].strip()
        m = ALLOW.match(lines[k])
        if m:
            return True, m.group(1).strip()
        if text.startswith("//") or text.startswith("#[") or text.endswith(":"):
            k -= 1
            continue
        return None
    return None


class Unit:
    """One scanned file: its blanked code and the spans that are in scope."""

    def __init__(self, path: Path, rel: str, spans: list[tuple[int, int]] | None):
        source = path.read_text()
        self.rel = rel
        self.code = blank(source)
        self.lines = source.splitlines()
        self.spans = spans

    def in_scope(self, offset: int) -> bool:
        return self.spans is None or any(a <= offset <= b for a, b in self.spans)


def helper_bodies(unit: Unit) -> list[tuple[str, str]]:
    """(name, body) of every fn and closure the unit defines in scope.

    A test that hides `request(Request::Pull)` and its wait in a `pulled`
    closure and calls that in a loop retries all the same (the 1162 shape).
    """
    code, out = unit.code, []
    for m in HELPER_FN.finditer(code):
        if not unit.in_scope(m.start()):
            continue
        open_at = body_open(code, m.end(), "fn")
        if open_at is not None:
            out.append((m.group(1), code[open_at : match_brace(code, open_at)]))
    for m in HELPER_CLOSURE.finditer(code):
        if not unit.in_scope(m.start()):
            continue
        params_end = code.find("|", m.end())
        if params_end < 0:
            continue
        open_at = body_open(code, params_end + 1, "closure")
        semi = code.find(";", params_end)
        if open_at is not None and (semi < 0 or open_at < semi):
            out.append((m.group(1), code[open_at : match_brace(code, open_at)]))
        elif semi >= 0:
            out.append((m.group(1), code[params_end + 1 : semi]))
    return out


def call_re(names: set[str], qualified: bool) -> list[re.Pattern[str]]:
    """Calls of names: bare only, or also behind a `module::` path."""
    before = r"(?<![\w.])" if qualified else r"(?<![\w.:])"
    return [re.compile(before + re.escape(n) + r"\s*\(") for n in sorted(names)]


def grow(bodies, req_res, wait_res, qualified: bool):
    """Fixpoint: a helper whose body requests (waits), or calls one that does."""
    requesters: set[str] = set()
    waiters: set[str] = set()
    while True:
        reqs = req_res + call_re(requesters, qualified)
        waits = wait_res + call_re(waiters, qualified)
        grown = False
        for name, body in bodies:
            if name not in requesters and any(r.search(body) for r in reqs):
                requesters.add(name)
                grown = True
            if name not in waiters and any(r.search(body) for r in waits):
                waiters.add(name)
                grown = True
        if not grown:
            return reqs, waits


def support_bodies(unit: Unit) -> list[tuple[str, str]]:
    """(name, body) of the top-level `pub fn`s of a `support/` module."""
    if "/support/" not in "/" + unit.rel:
        return []
    out = []
    for m in SUPPORT_FN.finditer(unit.code):
        open_at = body_open(unit.code, m.end(), "fn")
        if open_at is not None:
            out.append((m.group(1), unit.code[open_at : match_brace(unit.code, open_at)]))
    return out


def scan(unit: Unit, req_res, wait_res) -> tuple[list[str], list[str]]:
    """(findings, allows) for one file."""
    code, lines, rel = unit.code, unit.lines, unit.rel
    loops = []  # (start, open, close, names the pattern binds)
    for m in LOOP_KW.finditer(code):
        if not unit.in_scope(m.start()):
            continue
        open_at = body_open(code, m.end(), m.group(1))
        if open_at is None:
            continue
        names = walk_bindings(code[m.end() : open_at]) if m.group(1) == "for" else set()
        loops.append((m.start(), open_at, match_brace(code, open_at), names))

    def holds_both(open_at: int, close: int, names: set[str]) -> bool:
        body = code[open_at:close]
        requests = [x for r in req_res for x in r.finditer(body)]
        if not requests or not any(r.search(body) for r in wait_res):
            return False
        if not names:
            return True
        # A walk: every request carries something this turn's pattern bound.
        for x in requests:
            args = set(re.findall(r"\b\w+\b", group_after(body, x.end())))
            if not args & names:
                return True
        return False

    hits = [lp[:3] for lp in loops if holds_both(lp[1], lp[2], lp[3])]
    findings, allows = [], []
    for start, open_at, close in hits:
        if any(o > open_at and c <= close for _, o, c in hits):
            continue  # an inner loop holds both; that one is reported
        line = line_of(code, start)
        first = lines[line - 1].strip()
        allow = allow_above(lines, line)
        if allow and allow[1]:
            allows.append(f"{rel}:{line}: allowed ({allow[1]}): {first}")
        elif allow:
            findings.append(f"{rel}:{line}: `retry-guard: allow` without a reason: {first}")
        else:
            findings.append(
                f"{rel}:{line}: loop re-issues a request and waits: {first}. "
                f"Issue the request once, then poll the state until a deadline."
            )
    return findings, allows


def check(root: Path) -> tuple[list[str], list[str]]:
    units: list[Unit] = []
    for tree in WHOLE:
        for path in sorted((root / tree).rglob("*.rs")):
            units.append(Unit(path, str(path.relative_to(root)), None))
    whole_test_files: set[Path] = set()
    for tree in CFG_TEST:
        for path in sorted((root / tree).rglob("*.rs")):
            code = blank(path.read_text())
            whole_test_files.update(cfg_test_mod_files(path, code))
            spans = cfg_test_spans(code)
            if spans:
                units.append(Unit(path, str(path.relative_to(root)), spans))
    for path in sorted(whole_test_files):
        units.append(Unit(path, str(path.relative_to(root)), None))

    # Shared helpers first: a `tests/support` fn counts in every caller,
    # also behind its module path. Then each file's own fns and closures,
    # by bare call only, so a `Foo::new(` never matches a local `new`.
    shared = [hb for unit in units for hb in support_bodies(unit)]
    req_res, wait_res = grow(shared, REQUEST_RES, WAIT_RES, qualified=True)
    findings, allows = [], []
    for unit in units:
        f, a = scan(unit, *grow(helper_bodies(unit), req_res, wait_res, qualified=False))
        findings.extend(f)
        allows.extend(a)
    return findings, allows


RETRY = """\
#[test]
fn import_shows() {
    let deadline = Instant::now() + Duration::from_secs(5);
    for _attempt in 0..3 {
        facade::request(Request::Pull { home: h }); // "pull" in a comment
        if conversations().len() == 1 { return; }
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = deadline;
}
"""

ALLOWED = RETRY.replace(
    "    for _attempt", "    // retry-guard: allow the home node drops the first pull by design\n    for _attempt"
)

STATE_POLL = """\
#[test]
fn import_shows() {
    facade::request(Request::Pull { home: h });
    let deadline = Instant::now() + Duration::from_secs(5);
    while conversations().is_empty() {
        assert!(Instant::now() < deadline, "no import; Request::Pull sent once");
        let s = "facade::request(Request::Pull)";
        std::thread::sleep(Duration::from_millis(50));
    }
}
"""

NESTED = """\
fn f() {
    for _ in 0..3 {
        run_send(&msg);
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if let Ok(x) = rx.try_recv() { return; }
        }
    }
}
"""

CFG_SRC = """\
fn product() {
    loop { send_home(x); std::thread::sleep(d); }
}
#[cfg(test)]
mod tests {
    #[test]
    fn t() { loop { send_home(x); std::thread::sleep(d); } }
}
"""

HIDDEN = """\
fn t() {
    let pulled = |session: &Session, what: &str| {
        session.request(Request::Pull).expect("pull");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            match session.next_event(Duration::from_millis(500)) {
                Some(Event::Sync { state, .. }) if state == "done" => break,
                _ => assert!(std::time::Instant::now() < deadline, "{what}"),
            }
        }
    };
    pulled(&session, "the pull on open");
    let mut list = listed(&session);
    for _ in 0..3 {
        if list.conversations.iter().any(|row| row.peer == peer_hex) { break; }
        pulled(&session, "the pull after the import");
        list = listed(&session);
    }
}
"""

WALK = """\
async fn f() {
    for (body, fields) in messages(addr) {
        let send = run_send(&mut tap, SendRequest { body: body.into(), fields }, &o);
        let sent = loop {
            tokio::select! { s = &mut send => break s, _ = tokio::time::sleep(d) => {} }
        };
    }
}
"""

IMPL_FOR = """\
impl Drop for Guard {
    fn drop(&mut self) { let _ = self.home.pull(); std::thread::sleep(d); }
}
"""

# (name, file, text, expected finding count)
CASES = [
    ("a retried Request::Pull", "loquax/tests/t.rs", RETRY, 1),
    ("the same loop with an allow", "loquax/tests/t.rs", ALLOWED, 0),
    ("an allow with no reason", "loquax/tests/t.rs", RETRY.replace("    for _a", "    // retry-guard: allow\n    for _a"), 1),
    ("a state-polling loop", "loquax-android/tests/t.rs", STATE_POLL, 0),
    ("three attempts, each its own wait loop", "leviculum-std/tests/mvr/t.rs", NESTED, 1),
    ("cfg(test) only, product loop ignored", "loquax/src/x.rs", CFG_SRC, 1),
    ("the 1162 shape: request and wait behind a closure", "loquax-android/tests/t.rs", HIDDEN, 1),
    ("a support helper that requests and waits", "loquax/tests/support/s.rs",
     "pub fn pull_and_wait(s: &S) { s.pull(); std::thread::sleep(d); }\n"
     "fn t() { loop { if done() { break; } support::pull_and_wait(&s); } }\n", 1),
    ("a walk over distinct messages", "loquax/tests/t.rs", WALK, 0),
    ("a counter passed to the request is still a retry", "loquax/tests/t.rs",
     WALK.replace("(body, fields) in messages(addr)", "body in 0..3"), 1),
    ("impl .. for is not a loop", "loquax/tests/t.rs", IMPL_FOR, 0),
    ("outside the scope", "loquax/examples/t.rs", RETRY, 0),
]


def selftest() -> int:
    failures = []
    for name, rel, text, want in CASES:
        root = Path(tempfile.mkdtemp(prefix="check-test-retries-"))
        for tree in WHOLE + CFG_TEST:
            (root / tree).mkdir(parents=True)
        (root / rel).parent.mkdir(parents=True, exist_ok=True)
        (root / rel).write_text(text)
        findings, allows = check(root)
        before = len(failures)
        if len(findings) != want:
            failures.append(f"{name}: {len(findings)} finding(s), expected {want}: {findings}")
        if name == "the same loop with an allow" and len(allows) != 1:
            failures.append(f"{name}: the allow was not printed: {allows}")
        # Kept only when it explains a failure: one directory per fixture per
        # guards run had piled up in /tmp by the hundred (1314).
        if len(failures) == before:
            shutil.rmtree(root)
        else:
            failures.append(f"{name}: fixture kept at {root}")
    for failure in failures:
        print(f"check-test-retries selftest: {failure}", file=sys.stderr)
    if not failures:
        print(f"check-test-retries selftest: {len(CASES)} fixtures behave")
    return 1 if failures else 0


def main() -> int:
    if "--selftest" in sys.argv[1:]:
        return selftest()
    findings, allows = check(ROOT)
    for allow in allows:
        print(f"check-test-retries: {allow}")
    for finding in findings:
        print(f"check-test-retries: {finding}", file=sys.stderr)
    if findings:
        print(f"check-test-retries: {len(findings)} finding(s); see scripts/check-test-retries.py", file=sys.stderr)
        return 1
    print(f"check-test-retries: no test retries a request ({len(allows)} allow(s))")
    return 0


if __name__ == "__main__":
    sys.exit(main())
