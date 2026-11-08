#!/usr/bin/env python3
"""The M9 image's stack frames and its deepest static call chains (1582).

The nRF's frame gate (scripts/check-nrf-stack-frames.sh) reads `sub sp`
immediates; the ESP32-S3 is Xtensa with the windowed ABI, where every
function opens with `entry a1, <frame>` and a frame past entry's reach
grows by `l32r aX, <literal> (<bytes>); sub aY, a1, aX; movsp a1, aY`.
This reads both out of `objdump -d -C` of the linked ELF, so it measures
the shipped image, then walks the static call graph:

  - edges are `call8 <target>` and `callx8 aN` after an `l32r aN, ...
    (<target> <name>)` in the same function, which is how LLVM's Xtensa
    backend calls a function it cannot reach with call8;
  - calls through a vtable, a `dyn Fn` or a fn pointer are not edges, so
    a chain is a lower bound on what such a path can reach, not an upper
    bound on the core;
  - a cycle is cut where the chain would repeat a function;
  - the panic and allocation-failure paths are cut (`--with-panics` keeps
    them): they end in a reset.

Usage:
  m9-stack-frames.py <elf> [--top N] [--with-panics] [root ...]
  m9-stack-frames.py --selftest

Roots default to core 0's `main`, the UI core's `thinknode_m9::ui_main`
and the image's interrupt handlers. Needs xtensa-esp32s3-elf-objdump on
PATH (~/export-esp.sh puts it there).
"""

import os
import re
import subprocess
import sys

sys.setrecursionlimit(100000)

LABEL = re.compile(r"^([0-9a-f]{8}) <(.*)>:$")
INSN = re.compile(r"^\s*([0-9a-f]{8}):\t(\S+)\t?(.*)$")
ENTRY = re.compile(r"a1, (0x[0-9a-f]+|\d+)")
L32R = re.compile(r"(a\d+), [0-9a-f]+ <.*> \(([0-9a-f]+)(?: <.*>)?\)$")
SUB_SP = re.compile(r"(a\d+), a1, (a\d+)")
MOVSP = re.compile(r"a1, (a\d+)")
CALL = re.compile(r"([0-9a-f]+) ")
PANICS = re.compile(
    r"panic|begin_unwind|unwrap_failed|expect_failed|slice_end_index|"
    r"slice_start_index|slice_index_order|handle_alloc_error|alloc_error|"
    r"assert_failed|ord_violation"
)
ROOTS = [
    "main",
    "thinknode_m9::ui_main",
    "__level_1_interrupt",
    "__level_2_interrupt",
    "__level_3_interrupt",
    "leviculum_esp::__esp_hal_internal_uart0_rx",
    "thinknode_m9::__esp_hal_internal_kb_tick",
    "thinknode_m9::__esp_hal_internal_te_edge",
    "thinknode_m9::__esp_hal_internal_ui_tick",
    "esp_hal::gpio::interrupt::user_gpio_interrupt_handler",
]


def parse(lines):
    """The functions of a disassembly: address -> name, frame, callees."""
    funcs = {}
    cur = None
    regs = {}
    for line in lines:
        line = line.rstrip("\n")
        m = LABEL.match(line)
        if m:
            cur = {"name": m.group(2), "entry": None, "grow": 0, "calls": set()}
            funcs[int(m.group(1), 16)] = cur
            regs = {}
            continue
        if cur is None:
            continue
        m = INSN.match(line)
        if not m:
            continue
        op, args = m.group(2), m.group(3)
        if op == "entry":
            e = ENTRY.match(args)
            if e and cur["entry"] is None:
                cur["entry"] = int(e.group(1), 0)
        elif op == "l32r":
            e = L32R.match(args)
            if e:
                regs[e.group(1)] = int(e.group(2), 16)
        elif op == "sub":
            e = SUB_SP.match(args)
            if e and e.group(2) in regs:
                regs["sp-" + e.group(1)] = regs[e.group(2)]
        elif op == "movsp":
            e = MOVSP.match(args)
            if e and "sp-" + e.group(1) in regs:
                cur["grow"] += regs["sp-" + e.group(1)]
        elif op in ("call0", "call4", "call8", "call12"):
            e = CALL.match(args)
            if e:
                cur["calls"].add(int(e.group(1), 16))
        elif op in ("callx0", "callx4", "callx8", "callx12"):
            if args.strip() in regs:
                cur["calls"].add(regs[args.strip()])
    return funcs


def frame(funcs, addr):
    f = funcs.get(addr)
    if f is None or f["entry"] is None:
        return 0
    return f["entry"] + f["grow"]


def deepest(funcs, cut):
    """A function's deepest chain: (bytes, [addresses])."""
    memo = {}
    visiting = set()

    def deep(addr):
        if addr in memo:
            return memo[addr]
        if addr in visiting:
            return (0, [])
        if addr not in funcs:
            return (0, [addr])
        visiting.add(addr)
        best = (0, [])
        for callee in funcs[addr]["calls"]:
            if callee in funcs and cut and cut.search(funcs[callee]["name"]):
                continue
            got = deep(callee)
            if got[0] > best[0]:
                best = got
        visiting.discard(addr)
        memo[addr] = (frame(funcs, addr) + best[0], [addr] + best[1])
        return memo[addr]

    return deep


def report(funcs, roots, top, cut, out):
    by_frame = sorted(funcs, key=lambda a: frame(funcs, a), reverse=True)
    print(f"M9_FRAMES largest {top}:", file=out)
    for a in by_frame[:top]:
        print(f"  {frame(funcs, a):7d} {funcs[a]['name'][:140]}", file=out)
    deep = deepest(funcs, cut)
    for root in roots:
        found = [a for a in funcs if funcs[a]["name"] == root]
        if not found:
            print(f"M9_CHAIN root={root} missing", file=out)
            continue
        total, chain = deep(found[0])
        print(f"M9_CHAIN root={root} bytes={total} depth={len(chain)}", file=out)
        for a in chain:
            name = funcs[a]["name"] if a in funcs else hex(a)
            print(f"  {frame(funcs, a):7d} {name[:140]}", file=out)


SAMPLE = """\
42000000 <main>:
42000000:\tentry\ta1, 0x100
42000003:\tl32r\ta8, 41ffff00 <x+0x10> (42000100 <big>)
42000006:\tcallx8\ta8
42000009:\tcall8\t42000200 <small>
4200000c:\tl32r\ta8, 41ffff04 <x+0x14> (42000300 <core::panicking::panic_fmt>)
4200000f:\tcallx8\ta8
42000012:\tcallx8\ta9

42000100 <big>:
42000100:\tentry\ta1, 32
42000103:\tl32r\ta8, 41ffff08 <<a as b>::c+0x8> (8020 <RESERVE_ICACHE+0x20>)
42000106:\tsub\ta8, a1, a8
42000109:\tmovsp\ta1, a8
4200010c:\tcall8\t42000200 <small>

42000200 <small>:
42000200:\tentry\ta1, 48
42000203:\tcall8\t42000100 <big>

42000300 <core::panicking::panic_fmt>:
42000300:\tentry\ta1, 0x10000
"""


def selftest():
    funcs = parse(SAMPLE.splitlines())
    fails = []
    if frame(funcs, 0x42000000) != 256:
        fails.append("a hex entry immediate was not read")
    if frame(funcs, 0x42000100) != 32 + 0x8020:
        fails.append("a frame grown by l32r/sub/movsp was not added")
    deep = deepest(funcs, PANICS)
    total, chain = deep(0x42000000)
    if total != 256 + 32 + 0x8020 + 48:
        fails.append(f"the chain through callx8 and a cycle came to {total}")
    if [funcs[a]["name"] for a in chain] != ["main", "big", "small"]:
        fails.append("the chain did not run main -> big -> small, cut at the cycle")
    total, _ = deepest(funcs, None)(0x42000000)
    if total != 256 + 0x10000:
        fails.append("--with-panics did not keep the panic path")
    for f in fails:
        print(f"[m9-stack-frames] selftest FAIL: {f}")
    if not fails:
        print("[m9-stack-frames] selftest ok: frames, growth, l32r calls, cycles and the panic cut read")
    return 1 if fails else 0


def main(argv):
    if argv[:1] == ["--selftest"]:
        return selftest()
    if not argv:
        print(__doc__)
        return 2
    elf = argv[0]
    rest = argv[1:]
    top = 20
    cut = PANICS
    roots = []
    i = 0
    while i < len(rest):
        if rest[i] == "--top":
            top = int(rest[i + 1])
            i += 2
            continue
        if rest[i] == "--with-panics":
            cut = None
        else:
            roots.append(rest[i])
        i += 1
    objdump = os.environ.get("M9_OBJDUMP", "xtensa-esp32s3-elf-objdump")
    try:
        dis = subprocess.run(
            [objdump, "-d", "--no-show-raw-insn", "-C", elf],
            check=True,
            capture_output=True,
            text=True,
            errors="replace",
        ).stdout
    except (OSError, subprocess.CalledProcessError) as e:
        print(f"[m9-stack-frames] FAIL {objdump} on {elf}: {e}")
        return 1
    report(parse(dis.splitlines()), roots or ROOTS, top, cut, sys.stdout)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
