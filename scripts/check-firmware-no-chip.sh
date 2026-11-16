#!/usr/bin/env bash
#
# The orchestration stratum names no chip (Codeberg #469, gate
# `firmware-no-chip-cfg` of docs/src/concepts/firmware-layers.md, "Gates the
# new stratum brings").
#
# `firmware-no-hal` holds the dependency graph; this holds the text. A
# `cfg(feature = "bsp-...")` or a chip name in `leviculum-nrf/firmware/src`
# is a board decision taken in the one layer every board shares. Doc
# comments (`///`, `//!`) are exempt, because each seam's doc names today's
# implementer and the fork's, and that is the point of them; an ordinary
# `//` comment is code's company and is not exempt. Case-insensitive, so
# `SoftDevice` and `SX1262` count.
#
# A script rather than a source-invariant test: those are root-workspace
# targets (scripts/source-invariant-targets.txt), and leviculum-nrf is
# excluded from the root workspace.
#
# Positive control first, on a temp copy of the sources: the unmodified copy
# passes, a BSP `cfg` and a chip name in an ordinary comment are each
# refused, and a chip name in a doc comment is not.
#
# Usage: check-firmware-no-chip.sh [SRC_DIR]

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="${1:-$ROOT/leviculum-nrf/firmware/src}"

check() {
    python3 - "$1" <<'PY'
import pathlib, re, sys

src = pathlib.Path(sys.argv[1])
files = sorted(src.rglob("*.rs"))
if not files:
    print(f"[firmware-no-chip] ERROR no .rs files under {src}")
    sys.exit(2)
PATTERNS = [
    ("a BSP cfg", re.compile(r'feature\s*=\s*"bsp-')),
    ("a chip name", re.compile(r"nrf52|sx1262|lr1110|esp32|rp2350|softdevice", re.I)),
]
hits = []
for f in files:
    for n, line in enumerate(f.read_text().splitlines(), 1):
        if line.lstrip().startswith(("///", "//!")):
            continue
        for what, pat in PATTERNS:
            m = pat.search(line)
            if m:
                hits.append(f"{f.relative_to(src.parent)}:{n}: {what} `{m.group(0)}`: {line.strip()}")
for h in hits:
    print(f"[firmware-no-chip] FAIL {h}")
if hits:
    print("[firmware-no-chip] Stratum 2 takes no board decision; a chip belongs in a")
    print("[firmware-no-chip] bind crate (docs/src/concepts/firmware-layers.md).")
    sys.exit(1)
print(f"[firmware-no-chip] ok   no BSP cfg and no chip name outside doc comments in {len(files)} file(s)")
PY
}

# refused <dir> <expected text>: exit 1 naming it, nothing else.
refused() {
    local out rc=0
    out="$(check "$1")" || rc=$?
    [ "$rc" -eq 1 ] && grep -qF "$2" <<<"$out"
}

selftest() {
    local tmp rc=0
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' RETURN
    mkdir "$tmp/src"
    cp -r "$SRC/." "$tmp/src/"
    if ! check "$tmp/src" >/dev/null; then
        echo "[firmware-no-chip] SELFTEST FAIL: the unmodified copy was refused"
        return 1
    fi
    printf '\n#[cfg(feature = "bsp-t114")]\nconst _INJECTED: u8 = 0;\n' >> "$tmp/src/lib.rs"
    if ! refused "$tmp/src" "a BSP cfg \`feature = \"bsp-\`"; then
        echo "[firmware-no-chip] SELFTEST FAIL: an injected BSP cfg was not refused"
        rc=1
    fi
    cp "$SRC/lib.rs" "$tmp/src/lib.rs"
    printf '\n// waits on the SX1262 busy line\n' >> "$tmp/src/lib.rs"
    if ! refused "$tmp/src" "a chip name \`SX1262\`"; then
        echo "[firmware-no-chip] SELFTEST FAIL: a chip name in an ordinary comment was not refused"
        rc=1
    fi
    cp "$SRC/lib.rs" "$tmp/src/lib.rs"
    printf '\n/// Implemented by the SoftDevice backend on the nRF52840.\npub struct Injected;\n' \
        >> "$tmp/src/lib.rs"
    if ! check "$tmp/src" >/dev/null; then
        echo "[firmware-no-chip] SELFTEST FAIL: a chip name in a doc comment was refused"
        rc=1
    fi
    [ "$rc" -eq 0 ] && echo "[firmware-no-chip] selftest ok: a BSP cfg and a chip name in code are refused, a doc comment is not"
    return "$rc"
}

selftest
check "$SRC"
