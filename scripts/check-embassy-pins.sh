#!/usr/bin/env bash
#
# The embassy pin rule for the firmware workspace (Codeberg #469, step 0 of
# docs/src/concepts/firmware-layers.md). Rule and pin table:
# docs/src/firmware/embassy-pins.md.
#
# One version set per workspace: every manifest under leviculum-nrf/ (and the
# LR11xx driver, which the firmware pulls by path from its own workspace) that
# names an `embassy-*` crate names it at one semver-compatible version, and
# never from a git source. Nor may any `[patch.*]` table carry a git
# revision. Two versions of one embassy crate build two executors or two
# time drivers that do not see each other; a git revision in master is a
# build nobody can reproduce once the branch moves.
#
# Reads the manifest TEXT (tomllib), not cargo's resolution: the subject is
# what a member declares, so the guard needs no network and no lock, and it
# names the file that broke the rule. A transitive copy a dependency brings
# (nrf-softdevice's own embassy-sync 0.6) is that dependency's pin, not ours.
#
# Self-tests first, on a temp copy of the real manifests: an injected second
# version and an injected git patch must each be refused, or the guard fails.
#
# Usage: check-embassy-pins.sh [ROOT]   (ROOT defaults to the repo root)

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="${1:-$(cd "$HERE/.." && pwd)}"

check() {
    python3 - "$1" <<'PY'
import pathlib, re, sys, tomllib

root = pathlib.Path(sys.argv[1])
manifests = sorted(p for p in (root / "leviculum-nrf").rglob("Cargo.toml")
                   if "target" not in p.relative_to(root).parts)
lr = root / "leviculum-lr11xx" / "Cargo.toml"
if lr.exists():
    manifests.append(lr)
if not manifests:
    print(f"[embassy-pins] FAIL no manifests under {root}/leviculum-nrf")
    sys.exit(1)

def compat(req):
    # The semver-compatible class cargo unifies on: 0.x for 0.x.y, else the major.
    m = re.match(r"^[\^=~]?\s*(\d+)(?:\.(\d+))?", req.strip())
    if not m:
        return req.strip()
    major, minor = m.group(1), m.group(2) or "0"
    return f"{major}.{minor}" if major == "0" else major

def tables(doc):
    for key in ("dependencies", "dev-dependencies", "build-dependencies"):
        if key in doc:
            yield key, doc[key]
    for tgt, body in doc.get("target", {}).items():
        for key in ("dependencies", "dev-dependencies", "build-dependencies"):
            if key in body:
                yield f"target.{tgt}.{key}", body[key]
    ws = doc.get("workspace", {})
    if "dependencies" in ws:
        yield "workspace.dependencies", ws["dependencies"]

seen = {}   # crate -> {class: [where, ...]}
errors = []
for path in manifests:
    rel = path.relative_to(root)
    try:
        doc = tomllib.loads(path.read_text())
    except tomllib.TOMLDecodeError as e:
        print(f"[embassy-pins] ERROR {rel} does not parse: {e}")
        sys.exit(2)
    for src, body in doc.get("patch", {}).items():
        for name, spec in body.items():
            if isinstance(spec, dict) and "git" in spec:
                errors.append(f"{rel}: [patch.{src}] {name} is a git revision ({spec['git']})")
    for table, deps in tables(doc):
        for name, spec in deps.items():
            crate = spec.get("package", name) if isinstance(spec, dict) else name
            if not crate.startswith("embassy-"):
                continue
            if isinstance(spec, dict) and "git" in spec:
                errors.append(f"{rel}: [{table}] {crate} comes from git ({spec['git']})")
                continue
            ver = spec if isinstance(spec, str) else spec.get("version")
            if ver is None:
                continue  # `workspace = true` or a bare path: the source of truth is elsewhere
            seen.setdefault(crate, {}).setdefault(compat(ver), []).append(f"{rel} ({ver})")

for crate in sorted(seen):
    classes = seen[crate]
    if len(classes) > 1:
        detail = "; ".join(f"{c}: {', '.join(w)}" for c, w in sorted(classes.items()))
        errors.append(f"{crate} is pinned at {len(classes)} versions: {detail}")
    else:
        (c, where), = classes.items()
        print(f"[embassy-pins] ok   {crate} {c} ({len(where)} manifest(s))")

for e in errors:
    print(f"[embassy-pins] FAIL {e}")
if errors:
    print("[embassy-pins] One version set per workspace, bumped in one commit across")
    print("[embassy-pins] every member, no git revisions: docs/src/firmware/embassy-pins.md")
    sys.exit(1)
PY
}

# Exit 1 with the expected line is a refusal; anything else (a parse error is
# exit 2) means the injection did not test what it claims to.
refused() {
    local out rc=0
    out="$(check "$1")" || rc=$?
    [ "$rc" -eq 1 ] && grep -qF "$2" <<<"$out"
}

selftest() {
    local tmp rc=0
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' RETURN
    (cd "$ROOT" && find leviculum-nrf leviculum-lr11xx -name Cargo.toml -not -path '*/target/*' \
        -exec cp --parents {} "$tmp/" \;)
    if ! check "$tmp" >/dev/null; then
        echo "[embassy-pins] SELFTEST FAIL: the unmodified copy was refused"
        return 1
    fi
    mkdir "$tmp/leviculum-nrf/injected"
    printf '[package]\nname = "injected"\n[dependencies]\nembassy-time = "0.4"\n' \
        > "$tmp/leviculum-nrf/injected/Cargo.toml"
    if ! refused "$tmp" "embassy-time is pinned at 2 versions"; then
        echo "[embassy-pins] SELFTEST FAIL: a second embassy-time version was accepted"
        rc=1
    fi
    printf '[package]\nname = "injected"\n[patch.crates-io]\nembassy-sync = { git = "https://github.com/embassy-rs/embassy", rev = "0" }\n' \
        > "$tmp/leviculum-nrf/injected/Cargo.toml"
    if ! refused "$tmp" "[patch.crates-io] embassy-sync is a git revision"; then
        echo "[embassy-pins] SELFTEST FAIL: a git patch was accepted"
        rc=1
    fi
    [ "$rc" -eq 0 ] && echo "[embassy-pins] selftest ok: a second version and a git patch are refused"
    return "$rc"
}

selftest
check "$ROOT"
