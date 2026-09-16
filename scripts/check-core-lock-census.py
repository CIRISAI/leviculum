#!/usr/bin/env python3
"""Census gate for the public methods that take the core lock (Codeberg #199).

`93ba351` shipped a seam (`leviculum_std::driver::CoreProcessor`) that runs
consumer code while the driver holds the core mutex. That mutex is a
non-reentrant `std::sync::Mutex`, so any handle the consumer smuggled in that
re-locks it hangs the node -- first call, no load required, in safe synchronous
code with nothing for a compiler to catch.

The seam documented that residue as a phrase: "roughly forty plain synchronous
`pub fn`s". A prose phrase cannot be wrong loudly. Nothing anywhere noticed
when the set grew, and nothing asked the author of the forty-first whether it
was safe to hand a processor. This script turns the phrase into a list and a
number, pinned in scripts/core-lock-census.txt, so adding one is a diff.

What it counts
--------------

A `pub fn`, reachable from outside the crate (every enclosing module `pub`,
the `impl`'s type `pub`), whose own body acquires the core mutex --
`Arc<Mutex<StdNodeCore>>` -- through `MutexRecover::lock_recover`.

Receivers are resolved two ways, which is every spelling the crate uses:

  * `self.<field>` inside `impl T` resolves against T's declared fields. This
    is what keeps `CompletionRegistry::self.inner` (a `Mutex<RegistryInner>`,
    not the core) out of the census while `ReticulumNode::self.inner` is in it
    -- a name-only rule would count all twenty of them.
  * a bare local resolves against the enclosing function's parameter and `let`
    type annotations.

DIRECT acquisition only. A `pub fn` that reaches the lock through a private
helper is just as deadly to a processor and is NOT in this census; making it so
needs a call graph, which is a different piece of work (#199 asks for the
census, not the call graph). The limit is stated here rather than left to be
discovered, because a check that has quietly stopped checking is the failure
mode docs/src/concepts/checks-and-citations.md exists to remove. What guards
the gap instead is `unresolved()`: a lock acquisition inside a public method
whose receiver this script cannot type is a hard failure, not a silent pass.

The prose sites
---------------

Three places described the set in words. They now cite the pinned total, and
this gate checks they still say the same number, so the census cannot drift
away from the sentence that sends a reader looking for it.

Usage:
    python3 scripts/check-core-lock-census.py            # the gate
    python3 scripts/check-core-lock-census.py --print    # current census,
                                                         # in pin-file format
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rust_source import line_of, receiver_words, strip_noise  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
CRATE = ROOT / "leviculum-std"
SRC = CRATE / "src"
PIN_FILE = ROOT / "scripts" / "core-lock-census.txt"
PIN_FILE_REL = PIN_FILE.relative_to(ROOT)

# The core handle's type, as the crate spells it. `Arc<Mutex<StdNodeCore>>`
# with any amount of whitespace, behind any number of `&`/`&mut`.
CORE_TYPE = re.compile(r"&?\s*(?:mut\s+)?Arc\s*<\s*Mutex\s*<\s*StdNodeCore\s*>\s*>")
LOCK_CALL = re.compile(r"\.lock_recover\s*\(")
IDENT_RE = r"[A-Za-z0-9_]+"

# Prose that describes the size of this set. Each entry says: this file must
# contain the pinned total, and must not contain any of the vague phrasings the
# total replaced.
PROSE_SITES = (
    "docs/src/concepts/core-lock-budget.md",
    "leviculum-std/src/driver/processor.rs",
    "leviculum-std/src/sync_ext.rs",
)
VAGUE = re.compile(r"\b(?:roughly|about|around|some|nearly|approximately)\s+forty\b", re.I)


# ---------------------------------------------------------------------------
# Lexical item scanning
# ---------------------------------------------------------------------------


def body_of(code: str, brace: int) -> tuple[int, int]:
    """Span of the block whose opening brace is at `brace`, inclusive."""
    depth, j = 0, brace
    while j < len(code):
        if code[j] == "{":
            depth += 1
        elif code[j] == "}":
            depth -= 1
            if depth == 0:
                return (brace, j)
        j += 1
    return (brace, len(code) - 1)


def signature_end(code: str, after: int) -> int | None:
    """Index of the `{` opening the body of the item whose header ends at `after`.

    Returns None for a declaration without a body (a trait method's `fn f();`).
    Angle brackets are deliberately not tracked: no `{` or `;` can appear
    inside a generic argument list, so parentheses and brackets are enough.
    """
    depth, j = 0, after
    while j < len(code):
        c = code[j]
        if c in "([":
            depth += 1
        elif c in ")]":
            depth -= 1
        elif depth == 0 and c == "{":
            return j
        elif depth == 0 and c == ";":
            return None
        j += 1
    return None


def preceding_modifiers(code: str, start: int) -> str:
    """The `pub`/`pub(crate)`/`async`/`const`/`unsafe` run in front of an item."""
    j = start
    words: list[str] = []
    while True:
        k = j - 1
        while k >= 0 and code[k] in " \t\n\r":
            k -= 1
        if k >= 0 and code[k] == ")":
            # `pub(crate)` / `pub(super)` / `pub(in path)`
            depth, m = 1, k - 1
            while m >= 0 and depth:
                if code[m] == ")":
                    depth += 1
                elif code[m] == "(":
                    depth -= 1
                m -= 1
            inner = code[m + 2 : k]
            n = m
            while n >= 0 and code[n] in " \t\n\r":
                n -= 1
            end = n + 1
            while n >= 0 and IDENT_RE and re.match(r"[A-Za-z0-9_]", code[n]):
                n -= 1
            word = code[n + 1 : end]
            if word != "pub":
                break
            words.append(f"pub({inner.strip()})")
            j = n + 1
            continue
        end = k + 1
        while k >= 0 and re.match(r"[A-Za-z0-9_]", code[k]):
            k -= 1
        word = code[k + 1 : end]
        if word in ("pub", "async", "const", "unsafe", "extern", "default"):
            words.append(word)
            j = k + 1
            continue
        break
    return " ".join(reversed(words))


class Item:
    __slots__ = ("kind", "name", "vis", "head", "open", "close")

    def __init__(self, kind: str, name: str, vis: str, head: int, open_: int, close: int):
        self.kind, self.name, self.vis = kind, name, vis
        self.head, self.open, self.close = head, open_, close


def scan_items(code: str) -> list[Item]:
    """Every `fn`, `impl` and inline `mod` with a body, in source order."""
    items: list[Item] = []
    for m in re.finditer(r"\b(fn|impl|mod)\b", code):
        kw = m.group(1)
        if kw == "fn":
            nm = re.match(r"\s*([A-Za-z0-9_]+)", code[m.end() :])
            if not nm:
                continue  # `Fn`/`fn()` pointer type, not an item
            name, after = nm.group(1), m.end() + nm.end()
        elif kw == "mod":
            nm = re.match(r"\s*([A-Za-z0-9_]+)", code[m.end() :])
            if not nm:
                continue
            name, after = nm.group(1), m.end() + nm.end()
        else:
            name, after = "", m.end()
        brace = signature_end(code, after)
        if brace is None:
            continue
        if kw == "impl":
            head = code[m.end() : brace]
            target = head.split(" for ")[-1] if " for " in head else head
            name = re.sub(r"<.*", "", target.strip(), flags=re.S).strip().split("::")[-1]
            if not name:
                continue
        open_, close = body_of(code, brace)
        items.append(Item(kw, name, preceding_modifiers(code, m.start()), m.start(), open_, close))
    return items


def innermost(items: list[Item], kind: str, off: int) -> Item | None:
    best = None
    for it in items:
        if it.kind == kind and it.open < off < it.close:
            if best is None or it.open > best.open:
                best = it
    return best


def attributes_before(code: str, head: int) -> list[str]:
    """The attribute list directly above the item whose keyword starts at `head`.

    Walks backwards over whitespace and balanced `#[..]` groups, so an item
    carrying several attributes is read whole. Doc comments are already blanked
    by `strip_noise`, so they do not interrupt the walk.
    """
    attrs: list[str] = []
    j = head
    while True:
        k = j - 1
        while k >= 0 and code[k] in " \t\n\r":
            k -= 1
        if k < 0 or code[k] != "]":
            break
        depth, m = 1, k - 1
        while m >= 0 and depth:
            if code[m] == "]":
                depth += 1
            elif code[m] == "[":
                depth -= 1
            m -= 1
        if m < 0 or code[m] != "#":
            break
        attrs.append(code[m : k + 1])
        j = m
    return attrs


def in_test_code(code: str, items: list[Item], off: int) -> bool:
    """Is `off` inside a `#[cfg(test)]` module or a `#[test]`/`#[tokio::test]` fn?"""
    for it in items:
        if it.open < off < it.close and it.kind in ("mod", "fn"):
            for attr in attributes_before(code, it.head):
                flat = re.sub(r"\s+", "", attr)
                if "cfg(test)" in flat or flat in ("#[test]", "#[tokio::test]"):
                    return True
    return False


# ---------------------------------------------------------------------------
# Resolving a receiver to the core mutex
# ---------------------------------------------------------------------------


def core_fields(sources: dict[Path, str]) -> set[tuple[str, str]]:
    """(type name, field name) pairs declared `Arc<Mutex<StdNodeCore>>`."""
    fields: set[tuple[str, str]] = set()
    for code in sources.values():
        for m in re.finditer(r"\b(?:struct|union)\s+([A-Za-z0-9_]+)", code):
            brace = signature_end(code, m.end())
            if brace is None:
                continue
            open_, close = body_of(code, brace)
            for f in re.finditer(
                rf"\b({IDENT_RE})\s*:\s*({CORE_TYPE.pattern})", code[open_:close]
            ):
                fields.add((m.group(1), f.group(1)))
    return fields


def core_locals(fn_code: str) -> set[str]:
    """Names bound to the core handle inside one function body-plus-signature.

    Parameter annotations and `let` annotations only. An unannotated `let` is
    not resolved, and `unresolved()` reports it rather than guessing.
    """
    names = set()
    for m in re.finditer(rf"\b({IDENT_RE})\s*:\s*({CORE_TYPE.pattern})", fn_code):
        names.add(m.group(1))
    return names


def is_core_lock(code: str, items: list[Item], off: int) -> tuple[bool, str]:
    """Does the `.lock_recover()` at `off` take the core mutex?

    Returns (verdict, why). `why` is "unresolved" when the receiver could not be
    typed at all, which the caller treats as a failure inside a public method.
    """
    chain = receiver_words(code, off)
    if not chain:
        return (False, "unresolved: no receiver")
    fn = innermost(items, "fn", off)
    if len(chain) == 2 and chain[1] == "self":
        imp = innermost(items, "impl", off)
        if imp is None:
            return (False, "unresolved: `self` outside an impl")
        if (imp.name, chain[0]) in CORE_FIELDS:
            return (True, f"{imp.name}.{chain[0]}")
        if any(t == imp.name for t, _ in ALL_FIELDS) or (imp.name, chain[0]) in ALL_FIELDS:
            return (False, f"{imp.name}.{chain[0]} is not the core")
        return (False, f"unresolved: no field {chain[0]} on {imp.name}")
    if len(chain) == 1:
        if fn is None:
            return (False, "unresolved: bare receiver outside a fn")
        if chain[0] in core_locals(code[fn.head : fn.close]):
            return (True, f"local {chain[0]}")
        return (False, f"local {chain[0]} is not the core")
    return (False, f"unresolved: receiver chain {'.'.join(reversed(chain))}")


CORE_FIELDS: set[tuple[str, str]] = set()
ALL_FIELDS: set[tuple[str, str]] = set()


def all_fields(sources: dict[Path, str]) -> set[tuple[str, str]]:
    fields: set[tuple[str, str]] = set()
    for code in sources.values():
        for m in re.finditer(r"\b(?:struct|union)\s+([A-Za-z0-9_]+)", code):
            brace = signature_end(code, m.end())
            if brace is None:
                continue
            open_, close = body_of(code, brace)
            for f in re.finditer(rf"\b({IDENT_RE})\s*:", code[open_:close]):
                fields.add((m.group(1), f.group(1)))
    return fields


# ---------------------------------------------------------------------------
# Effective visibility
# ---------------------------------------------------------------------------


def module_path(path: Path) -> str:
    """`driver::sender` for leviculum-std/src/driver/sender.rs."""
    rel = path.relative_to(SRC)
    parts = list(rel.parts)
    if parts[-1] in ("lib.rs", "mod.rs"):
        parts.pop()
    else:
        parts[-1] = parts[-1][: -len(".rs")]
    return "::".join(parts)


def public_module_files() -> set[Path]:
    """Files whose whole `mod` chain from lib.rs is `pub`."""
    reachable: set[Path] = set()

    def visit(path: Path) -> None:
        if path in reachable or not path.exists():
            return
        reachable.add(path)
        code = strip_noise(path.read_text())
        base = path.parent if path.name in ("lib.rs", "mod.rs") else path.with_suffix("")
        for m in re.finditer(r"(?<![A-Za-z0-9_])pub\s+mod\s+([A-Za-z0-9_]+)\s*;", code):
            name = m.group(1)
            for cand in (base / f"{name}.rs", base / name / "mod.rs"):
                if cand.exists():
                    visit(cand)
                    break

    visit(SRC / "lib.rs")
    return reachable


def reexported_names(sources: dict[Path, str], public_files: set[Path]) -> set[str]:
    """Leaf identifiers a `pub use` lifts into a publicly reachable module.

    `mod sender;` + `pub use sender::PacketSender;` is how this crate exposes
    most of the driver's handle types, so a module-chain-only rule would miss
    `PacketSender` and `LinkHandle` -- the two handles the seam's own docs name
    as the way a processor smuggles the core lock into a hook.
    """
    names: set[str] = set()
    for path, code in sources.items():
        if path not in public_files:
            continue
        for m in re.finditer(r"(?<![A-Za-z0-9_])pub\s+use\s+([^;]+);", code):
            body = m.group(1)
            if " as " in body:
                names.update(re.findall(r"as\s+([A-Za-z0-9_]+)", body))
            for leaf in re.findall(r"([A-Za-z0-9_]+)\s*(?=,|\}|$)", body):
                names.add(leaf)
            tail = body.split("::")[-1].strip()
            if re.fullmatch(r"[A-Za-z0-9_]+", tail):
                names.add(tail)
    return names


def public_types(sources: dict[Path, str], public_files: set[Path], exported: set[str]) -> set[str]:
    """Types a consumer of the crate can name, and therefore call methods on."""
    names = set()
    for path, code in sources.items():
        for m in re.finditer(
            r"(?<![A-Za-z0-9_])pub\s+(?:struct|enum|trait|union)\s+([A-Za-z0-9_]+)", code
        ):
            if path in public_files or m.group(1) in exported:
                names.add(m.group(1))
    return names


# ---------------------------------------------------------------------------
# The canary. Checked before anything is reported about the tree, because a
# classifier that has stopped matching reports a clean surface forever.
# ---------------------------------------------------------------------------

CANARY_SRC = """
pub struct ReticulumNode { inner: Arc<Mutex<StdNodeCore>>, seen: Mutex<u8> }
pub struct CompletionRegistry { inner: Mutex<RegistryInner> }

impl ReticulumNode {
    pub fn has_path(&self, d: &DestinationHash) -> bool {
        self.inner.lock_recover().has_path(d)
    }
    pub fn seen_count(&self) -> u8 { *self.seen.lock_recover() }
    pub(crate) fn hidden(&self) -> bool { self.inner.lock_recover().x() }
    fn private(&self) -> bool { self.inner.lock_recover().x() }
    pub fn declared_only(&self) -> bool;
}

impl CompletionRegistry {
    pub fn established(&self) -> usize { self.inner.lock_recover().established.len() }
}

pub fn free_fn(inner: &Arc<Mutex<StdNodeCore>>) -> bool {
    inner.lock_recover().x()
}

#[cfg(test)]
mod tests {
    #[test]
    fn t() {
        let core: Arc<Mutex<StdNodeCore>> = mk();
        core.lock_recover().x();
    }
}
"""
# `has_path` and `free_fn` only. `seen_count` locks a different mutex,
# `hidden`/`private` are not `pub`, `CompletionRegistry::established` locks
# `Mutex<RegistryInner>` under the same field name, `declared_only` has no
# body, and the test module is not API.
CANARY_EXPECT = {"ReticulumNode::has_path", "free_fn"}


def census_of(
    code: str,
    module: str,
    pub_types: set[str],
    free_fns_exported: bool = True,
    exported_names: frozenset[str] = frozenset(),
) -> tuple[dict[str, int], list[str]]:
    """Public methods in one file that take the core lock, plus unresolved sites.

    `free_fns_exported` says whether a bare `pub fn` in this file is reachable
    from outside the crate: true when the file's `mod` chain is public. A free
    function in a private module is reachable only if it is `pub use`d, which is
    what `exported_names` carries."""
    items = scan_items(code)
    found: dict[str, int] = {}
    unresolved: list[str] = []
    for m in LOCK_CALL.finditer(code):
        off = m.start()
        if in_test_code(code, items, off):
            continue
        fn = innermost(items, "fn", off)
        if fn is None:
            continue
        imp = innermost(items, "impl", off)
        if imp is not None and imp.open > fn.open:
            imp = None
        qualified = f"{imp.name}::{fn.name}" if imp else fn.name
        if module:
            qualified = f"{module}::{qualified}"
        is_pub = fn.vis.split()[0] == "pub" if fn.vis.split() else False
        if imp is not None:
            exported = is_pub and imp.name in pub_types
        else:
            exported = is_pub and (free_fns_exported or fn.name in exported_names)
        core, why = is_core_lock(code, items, off)
        if core and exported:
            found[qualified] = found.get(qualified, 0) + 1
        elif why.startswith("unresolved") and exported:
            unresolved.append(f"{qualified} ({why})")
    return found, unresolved


def canary() -> None:
    global CORE_FIELDS, ALL_FIELDS
    code = strip_noise(CANARY_SRC)
    saved_core, saved_all = CORE_FIELDS, ALL_FIELDS
    CORE_FIELDS = core_fields({Path("canary"): code})
    ALL_FIELDS = all_fields({Path("canary"): code})
    try:
        srcs = {Path("canary"): code}
        files = set(srcs)
        found, unresolved = census_of(
            code, "", public_types(srcs, files, set()), True, frozenset()
        )
    finally:
        CORE_FIELDS, ALL_FIELDS = saved_core, saved_all
    if set(found) != CANARY_EXPECT:
        sys.exit(
            "[core-lock-census] CANARY FAILED: the classifier read the fixture as\n"
            f"    {sorted(found)}\n"
            f"  but it holds exactly {sorted(CANARY_EXPECT)}. It has stopped\n"
            "  recognising the shape it exists to find, so every number it\n"
            "  reports about the tree is meaningless."
        )
    if unresolved:
        sys.exit(
            f"[core-lock-census] CANARY FAILED: {unresolved} left unresolved in a "
            "fixture whose every receiver is resolvable."
        )


# ---------------------------------------------------------------------------
# The pin file
# ---------------------------------------------------------------------------


def read_pins() -> tuple[int, set[str]]:
    total = None
    methods: set[str] = set()
    for lineno, raw in enumerate(PIN_FILE.read_text().splitlines(), 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if line.startswith("TOTAL "):
            rest = line[len("TOTAL ") :].strip()
            if not rest.isdigit():
                sys.exit(f"[core-lock-census] {PIN_FILE_REL}:{lineno}: malformed TOTAL: {raw!r}")
            total = int(rest)
            continue
        if " " in line:
            sys.exit(f"[core-lock-census] {PIN_FILE_REL}:{lineno}: malformed entry: {raw!r}")
        methods.add(line)
    if total is None:
        sys.exit(f"[core-lock-census] {PIN_FILE_REL}: no TOTAL line")
    return total, methods


def check_prose(total: int) -> list[str]:
    failures = []
    for rel in PROSE_SITES:
        text = (ROOT / rel).read_text()
        if str(total) not in text:
            failures.append(
                f"{rel} does not state the pinned total {total}. It describes this\n"
                f"      set, so it has to carry the number the set actually has --\n"
                f"      that is the whole point of counting it."
            )
        if (m := VAGUE.search(text)) is not None:
            failures.append(
                f"{rel} still says {m.group(0)!r}. The census exists so that\n"
                f"      sentence can say {total}; a phrase cannot be wrong loudly."
            )
    return failures


def main() -> int:
    global CORE_FIELDS, ALL_FIELDS
    canary()

    sources = {p: strip_noise(p.read_text()) for p in sorted(SRC.rglob("*.rs"))}
    public_files = public_module_files()
    exported = reexported_names(sources, public_files)
    CORE_FIELDS = core_fields(sources)
    ALL_FIELDS = all_fields(sources)
    if not CORE_FIELDS:
        sys.exit(
            "[core-lock-census] no field of type Arc<Mutex<StdNodeCore>> found in "
            f"{SRC.relative_to(ROOT)}. The core handle has been renamed or "
            "restyled; teach CORE_TYPE about it before trusting this gate."
        )
    pub_types = public_types(sources, public_files, exported)

    census: dict[str, int] = {}
    unresolved: list[str] = []
    for path, code in sources.items():
        found, unres = census_of(
            code,
            module_path(path),
            pub_types,
            path in public_files,
            frozenset(exported),
        )
        for k, v in found.items():
            census[k] = census.get(k, 0) + v
        unresolved.extend(f"{path.relative_to(ROOT)}: {u}" for u in unres)

    if "--print" in sys.argv[1:]:
        print(f"TOTAL {len(census)}")
        for name in sorted(census):
            print(name)
        return 0

    pinned_total, pinned = read_pins()
    failures: list[str] = []

    if unresolved:
        failures.append(
            "a public method takes a lock this script could not type:\n      "
            + "\n      ".join(sorted(unresolved))
            + "\n      Teach is_core_lock() about the receiver. An unresolved site is\n"
            "      neither in nor out of the census, which is the one answer a\n"
            "      census may not give."
        )

    added = sorted(set(census) - pinned)
    removed = sorted(pinned - set(census))
    if added:
        failures.append(
            "new public method(s) taking the core lock:\n        "
            + "\n        ".join(added)
            + f"""

      Each of these is a way for a `CoreProcessor` hook holding a node handle
      to deadlock the node on its first call -- synchronously, with no
      `.await` and nothing for a compile-fail fixture to catch. Before adding
      the line to {PIN_FILE_REL}, answer the question this gate exists to ask:
      does this method need to be reachable by a consumer at all, and if it
      does, is its doc comment honest about what it locks?

      Then add it (and bump TOTAL), in the same commit."""
        )
    if removed:
        failures.append(
            "pinned method(s) that no longer take the core lock (renamed, made\n"
            "      private, or moved off the lock -- the good direction):\n        "
            + "\n        ".join(removed)
            + f"\n      Drop them from {PIN_FILE_REL} and lower TOTAL in the same commit."
        )
    if not added and not removed and pinned_total != len(census):
        failures.append(
            f"the list in {PIN_FILE_REL} matches, but its TOTAL says {pinned_total} "
            f"and the list is {len(census)} long."
        )

    if not failures:
        failures.extend(check_prose(len(census)))

    if failures:
        print("[core-lock-census] FAIL: the public core-locking surface has moved")
        for f in failures:
            print(f"  {f}")
        return 1

    print(
        f"[core-lock-census] OK: {len(census)} public method(s) take the core lock, "
        f"all pinned in {PIN_FILE_REL}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
