"""Rust source scanning shared by the census gates in this directory.

These helpers are lexical, not semantic: they blank out comments and literals
so a regex over the result cannot match inside one, and they walk a receiver
chain backwards from a method call. That is enough for a census gate and
nothing like a parser -- every user of this module carries a canary fixture
that fails loudly if the classifier built on top of it has stopped matching
the shape it exists to find.

Extracted from scripts/check-supervised-spawns.py (Codeberg #191d) when
scripts/check-core-lock-census.py (#199) needed the same two functions. One
copy: a second one drifts the first time either gate is sharpened.
"""

from __future__ import annotations

import re

IDENT = re.compile(r"[A-Za-z0-9_]")


def strip_noise(src: str) -> str:
    """Blank out comments and literal contents, preserving every offset.

    Offsets are preserved so a hit's position still maps to a real line number,
    and so the backward walk in `receiver_words` sees the same layout the source
    has. Replacement is by space, except newlines, which are kept so line
    numbering survives.
    """
    out = list(src)
    i, n = 0, len(src)

    def blank(start: int, end: int) -> None:
        for k in range(start, min(end, n)):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        c = src[i]
        if c == "/" and i + 1 < n and src[i + 1] == "/":
            j = src.find("\n", i)
            j = n if j < 0 else j
            blank(i, j)
            i = j
        elif c == "/" and i + 1 < n and src[i + 1] == "*":
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith("/*", j):
                    depth += 1
                    j += 2
                elif src.startswith("*/", j):
                    depth -= 1
                    j += 2
                else:
                    j += 1
            blank(i, j)
            i = j
        elif c == "r" and (m := re.match(r'r(#*)"', src[i:])):
            close = '"' + m.group(1)
            j = src.find(close, i + len(m.group(0)))
            j = n if j < 0 else j + len(close)
            blank(i, j)
            i = j
        elif c == '"':
            j = i + 1
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            blank(i, min(j + 1, n))
            i = j + 1
        elif c == "'":
            # A char literal, or a lifetime. `'a` is not a literal and must not
            # swallow the rest of the line.
            m = re.match(r"'(\\.|[^\\'])'", src[i:])
            if m:
                blank(i, i + len(m.group(0)))
                i += len(m.group(0))
            else:
                i += 1
        else:
            i += 1
    return "".join(out)


def receiver_words(code: str, dot: int) -> list[str]:
    """Every identifier in the receiver chain whose method call sits at `dot`.

    Walks backwards over call groups (`(..)`, `[..]`), method names and path
    separators, collecting each identifier it crosses, innermost first.
    `std::process::Command::new(x).args(y).spawn()` yields
    `["new", "Command", "process", "std"]` -- the fully-qualified spelling is
    why this returns the whole chain and not just its head; a single-token head
    would read `std` there and miss the site.
    `cmd.spawn()` yields `["cmd"]`, `self.inner.lock_recover()` yields
    `["inner", "self"]`.
    """
    words: list[str] = []
    i = dot - 1
    closers = {")": "(", "]": "["}
    while i >= 0:
        while i >= 0 and code[i] in " \t\n\r":
            i -= 1
        if i < 0:
            break
        if code[i] in closers:
            close, openc = code[i], closers[code[i]]
            depth, i = 1, i - 1
            while i >= 0 and depth:
                if code[i] == close:
                    depth += 1
                elif code[i] == openc:
                    depth -= 1
                i -= 1
            continue
        if IDENT.match(code[i]):
            end = i + 1
            while i >= 0 and IDENT.match(code[i]):
                i -= 1
            words.append(code[i + 1 : end])
            j = i
            while j >= 0 and code[j] in " \t\n\r":
                j -= 1
            if j >= 1 and code[j] == ":" and code[j - 1] == ":":
                i = j - 2
                continue
            if j >= 0 and code[j] == ".":
                i = j - 1
                continue
            break
        break
    return words


def line_of(src: str, offset: int) -> int:
    return src.count("\n", 0, offset) + 1
