#!/usr/bin/env python3
"""No test picks a port number that something else may already hold.

Codeberg nightly #509 (2026-10-11, 3032da10) went red on
`link_failure_recovery_silent_resume`: the mvr drew 61455 from the shared
allocator, whose probe bind found it free, and its own proxy's bind of the
same number a few instructions later was refused with EADDRINUSE. Pipelines
#473 and #486 had failed the same way. A number chosen first and bound later
is a handoff, and in the window anything on the host can take it. A literal
number is the same handoff with the window stretched to the whole run.

Two rules, both a text scan, sub-second, no build:

  1. LITERALS, over every Rust file under leviculum-std/tests/: a numeric
     port in a socket or config position (a `bind((host, N))`, a
     `"127.0.0.1:N"`, a `listen_port = N` / `target_port = N` / `port = N`,
     a `SocketAddr::from(([..], N))`, a `const ..PORT..: u16 = N`, a
     `let ..port..: u16 = N`). Port 0 is not a literal port, it is the
     kernel's pick. A literal that names no socket this suite binds or dials
     (a config text that is only parsed, a number carried in an announce)
     is admitted by a line of scripts/test-port-literals.txt,
     `<file> <port> <reason>`; a line that matches nothing fails too, so the
     list cannot outlive what it excuses.

  2. ADVANCE PICKS, over leviculum-std/tests/mvr/: every listener the test
     process binds itself takes port 0 (`crate::ports::LOOPBACK_ANY_PORT`,
     `bind_loopback_tcp()`, `"127.0.0.1:0"`) and reads the port back. A
     number drawn ahead of its bind (`free_port()`, the allocator) is allowed
     only for a consumer that cannot report back, and the draw must say so
     in a `port-handoff:` comment directly above it; a `bind(...)` or
     `add_tcp_server(...)` that is not port 0 is allowed only in a function
     that has such a draw. `ports.rs` defines the helpers and is exempt.

`--selftest` runs both rules over fixtures with each fault injected and
fails unless every one fires and the clean fixture passes.

Usage:
    python3 scripts/check-test-ports.py              # the gate
    python3 scripts/check-test-ports.py --selftest   # the positive control
"""

from __future__ import annotations

import re
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TESTS_REL = Path("leviculum-std/tests")
MVR_REL = TESTS_REL / "mvr"
ALLOW_REL = Path("scripts/test-port-literals.txt")
MARKER = "port-handoff:"

# A port is 1..65535 written as a decimal literal; 0 is the kernel's pick.
_N = r"([1-9][0-9]{0,4})\b"
LITERAL_PATTERNS = [
    re.compile(r"bind\(\s*\(\s*\"[^\"]*\"\s*,\s*" + _N),
    re.compile(r"\"(?:127\.0\.0\.1|0\.0\.0\.0|localhost|\[::1?\]):" + _N),
    re.compile(r"\b(?:listen_port|target_port|forward_port|port)\s*=\s*" + _N),
    re.compile(r"SocketAddr::from\(\(\s*\[[0-9,\s]+\]\s*,\s*" + _N),
    re.compile(r"\bconst\s+\w*PORT\w*\s*:\s*u16\s*=\s*" + _N),
    re.compile(r"\blet\s+(?:mut\s+)?\w*port\w*\s*:\s*u16\s*=\s*" + _N),
]

DRAW = re.compile(
    r"\b(?:free_port|free_tcp_port|free_udp_port|free_udp_port_pair|"
    r"try_free_tcp_port|try_free_udp_port|pick_free_tcp_port|pick_free_udp_port|"
    r"find_available_ports|next_port_candidate)\b\s*(?:::<[^>]*>)?\("
)
BIND = re.compile(r"(?:::bind|\.add_tcp_server|\.add_udp_interface)\(")
PORT_ZERO = re.compile(
    r"LOOPBACK_ANY_PORT|bind_loopback_tcp\(|\"[^\"]*:0\"|,\s*0\s*\)"
)
FN = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+\w+")


def is_comment(line: str) -> bool:
    return line.lstrip().startswith("//")


def literal_findings(root: Path) -> list[tuple[str, int, int, str]]:
    """(file, line, port, text) for every literal port under the tests tree."""
    found = []
    for path in sorted((root / TESTS_REL).rglob("*.rs")):
        rel = str(path.relative_to(root))
        for number, line in enumerate(path.read_text().splitlines(), start=1):
            if is_comment(line):
                continue
            for pattern in LITERAL_PATTERNS:
                for match in pattern.finditer(line):
                    port = int(match.group(1))
                    if port <= 65535:
                        found.append((rel, number, port, line.strip()))
    return found


def allowlist(root: Path) -> tuple[dict[tuple[str, int], str], list[str]]:
    entries: dict[tuple[str, int], str] = {}
    errors: list[str] = []
    path = root / ALLOW_REL
    if not path.exists():
        return entries, [f"{ALLOW_REL} is missing"]
    for number, raw in enumerate(path.read_text().splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split(None, 2)
        if len(parts) < 3 or not parts[1].isdigit():
            errors.append(f"{ALLOW_REL}:{number}: expected `<file> <port> <reason>`")
            continue
        entries[(parts[0], int(parts[1]))] = parts[2]
    return entries, errors


def check_literals(root: Path) -> list[str]:
    entries, errors = allowlist(root)
    used: set[tuple[str, int]] = set()
    for rel, number, port, text in literal_findings(root):
        key = (rel, port)
        if key in entries:
            used.add(key)
            continue
        errors.append(
            f"{rel}:{number}: literal port {port} ({text}). Bind port 0 and read "
            f"the port back, or draw one at run time; if no socket binds or "
            f"dials it, admit it in {ALLOW_REL} with the reason"
        )
    for rel, port in sorted(set(entries) - used):
        errors.append(
            f"{ALLOW_REL}: `{rel} {port}` admits a literal that no longer exists; "
            f"drop the line"
        )
    return errors


def handoff_comment_above(lines: list[str], index: int) -> bool:
    """A `port-handoff:` line in the comment block directly above `index`."""
    i = index - 1
    while i >= 0 and is_comment(lines[i]):
        if MARKER in lines[i]:
            return True
        i -= 1
    return False


def check_mvr(root: Path) -> list[str]:
    errors: list[str] = []
    for path in sorted((root / MVR_REL).glob("*.rs")):
        if path.name == "ports.rs":
            continue
        rel = path.relative_to(root)
        lines = path.read_text().splitlines()
        fn_has_handoff = False
        for index, line in enumerate(lines):
            if FN.match(line):
                fn_has_handoff = False
            if is_comment(line):
                continue
            if DRAW.search(line):
                if handoff_comment_above(lines, index):
                    fn_has_handoff = True
                else:
                    errors.append(
                        f"{rel}:{index + 1}: a port drawn ahead of its bind with no "
                        f"`// {MARKER} <why>` comment directly above. Bind "
                        f"crate::ports::LOOPBACK_ANY_PORT and read the port back "
                        f"instead, or say why the consumer cannot"
                    )
            if BIND.search(line) and not PORT_ZERO.search(line) and not fn_has_handoff:
                errors.append(
                    f"{rel}:{index + 1}: a listener this process binds on a port "
                    f"chosen before the bind ({line.strip()}). Bind "
                    f"crate::ports::LOOPBACK_ANY_PORT and read the port back "
                    f"(crate::ports::bound_tcp_addr / local_addr)"
                )
    return errors


def check(root: Path) -> list[str]:
    return check_literals(root) + check_mvr(root)


CLEAN_MVR = """\
fn proxy() {
    let listener = crate::ports::bind_loopback_tcp();
    let other = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
}

async fn node() {
    let b = Builder::new().add_tcp_server(crate::ports::LOOPBACK_ANY_PORT);
}

fn daemon() {
    // port-handoff: the daemon reads it from a config file.
    let port = crate::ports::free_port();
    let b = Builder::new().add_tcp_server(addr_from(port));
}
"""

FAULTS = {
    "literal bind": (TESTS_REL / "a.rs", 'fn f() { TcpListener::bind(("127.0.0.1", 61455)); }\n'),
    "literal address": (TESTS_REL / "a.rs", 'fn f() { let a = "127.0.0.1:4242"; }\n'),
    "literal config": (TESTS_REL / "a.rs", 'const C: &str = "listen_port = 37428";\n'),
    "literal const": (TESTS_REL / "a.rs", "const PROXY_PORT: u16 = 61455;\n"),
    "draw without reason": (
        MVR_REL / "m.rs",
        "fn f() {\n    let p = crate::ports::free_port();\n}\n",
    ),
    "in-process bind of a drawn port": (
        MVR_REL / "m.rs",
        'fn f(p: u16) {\n    let l = StdTcpListener::bind(("127.0.0.1", p));\n}\n',
    ),
    "node server on a drawn port": (
        MVR_REL / "m.rs",
        "async fn f(addr: SocketAddr) {\n    let b = B::new().add_tcp_server(addr);\n}\n",
    ),
    "reason in another fn": (
        MVR_REL / "m.rs",
        "fn a() {\n    // port-handoff: daemon config.\n    let p = crate::ports::free_port();\n}\n"
        "fn b(addr: SocketAddr) {\n    let s = B::new().add_tcp_server(addr);\n}\n",
    ),
    "stale allowlist line": (ALLOW_REL, "leviculum-std/tests/gone.rs 4242 parsed only\n"),
}


def fixture(files: dict[Path, str]) -> Path:
    root = Path(tempfile.mkdtemp(prefix="check-test-ports-"))
    (root / MVR_REL).mkdir(parents=True)
    (root / ALLOW_REL).parent.mkdir(parents=True)
    (root / ALLOW_REL).write_text("# empty\n")
    (root / MVR_REL / "clean.rs").write_text(CLEAN_MVR)
    for rel, text in files.items():
        (root / rel).write_text(text)
    return root


def selftest() -> int:
    failures = []
    clean = check(fixture({}))
    if clean:
        failures.append(f"the clean fixture fails: {clean}")
    for name, (rel, text) in FAULTS.items():
        if not check(fixture({rel: text})):
            failures.append(f"injected fault did not fire: {name}")
    for failure in failures:
        print(f"check-test-ports selftest: {failure}", file=sys.stderr)
    if not failures:
        print(f"check-test-ports selftest: clean fixture passes, {len(FAULTS)} faults fire")
    return 1 if failures else 0


def main() -> int:
    if "--selftest" in sys.argv[1:]:
        return selftest()
    errors = check(ROOT)
    for error in errors:
        print(f"check-test-ports: {error}", file=sys.stderr)
    if errors:
        print(
            f"check-test-ports: {len(errors)} finding(s); see the docstring of "
            f"scripts/check-test-ports.py and leviculum-std/tests/mvr/ports.rs",
            file=sys.stderr,
        )
        return 1
    print("check-test-ports: no literal port, no advance pick without a reason")
    return 0


if __name__ == "__main__":
    sys.exit(main())
