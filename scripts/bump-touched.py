#!/usr/bin/env python3
"""Versions move with every landing (1226).

The workspace said `0.10.0-dev` from 2026-07-01 for 1255 commits, loquax
`0.1.0`, the Android app's versionCode `1`: nobody remembered to move them,
because nothing asked. This script is what asks.

The rule: every versioned unit whose files changed since the base (the last
landing, `origin/master`) carries a version newer than the base's. A unit
that changed and still carries the base's version is OWED. Several passes
between two landings share one bump, so the numbers stay small.

What moves, and only this (minor and major are Lew's, never a script's):

  * a plain version bumps its patch: 0.1.0 -> 0.1.1;
  * the workspace sits in a development window (the comment above its
    version line in Cargo.toml) and its moving number is the dev counter:
    0.10.0-dev -> 0.10.0-dev.1 -> 0.10.0-dev.2. Debian gets 0.10.0~dev.2,
    above an installed 0.10.0~dev (scripts/test-deb-stamp.sh compares it);
  * loquax-android's `[package.metadata.android] version-code` grows by
    one, because Android refuses an update whose versionCode did not grow.
    The Makefile reads it from that Cargo.toml and the versionName from
    loquax's, the version the app shows (1667).

A unit's files are its crate directory, and for loquax-android loquax's
too: the APK carries loquax and is named by its version, so a loquax
change is a new APK whose versionCode must grow (1667). The workspace unit is every crate
whose version is the workspace's, taken from `cargo metadata` rather than
typed here, so a new crate with `version.workspace = true` joins it without
an edit to this file. `docs/`, `scripts/`, the Justfile and the root
Cargo.toml move nothing on their own.

Usage:
  python3 -I scripts/bump-touched.py [--base REV]   bump every owed unit
  python3 -I scripts/bump-touched.py --check [--json] [--base REV]
      list the owed units, exit 1 if there are any, change nothing
  python3 -I scripts/bump-touched.py --selftest
      run the fixtures (a scripted git repository in a tempdir)

Both `--check` and the bump print the base first, `base=<sha12> (<rev>,
<date>)`, and every OWED line names the unit's version at the base and at
HEAD. A gate tree whose `origin/master` had not been fetched compared
against a landing days old and reported owed versions the coder tree did
not see (1247); the base line is what lets a reader tell the two apart.

A base that is not an ancestor of HEAD is no landing this tree builds on:
the guard says `base is not an ancestor of HEAD; fetch origin` and exits 2,
whatever the mode. A stale base that IS an ancestor cannot be told from a
fresh one by git alone; its date on the base line is the witness.

Exit 0 = nothing owed (or everything owed was bumped). Exit 1 = `--check`
found owed units, or a fixture misbehaved. Exit 2 = the script could not
work (no git, unresolvable base in bump mode, a base that is not an
ancestor of HEAD, a version it cannot spell); a gate reads it as
infrastructure, not as owed versions.
"""

import argparse
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# The units with a version of their own: name, crate directory. The
# workspace unit is not listed; `workspace_unit` derives it.
OWN_UNITS = [
    ("loquax", "loquax"),
    ("loquax-android", "loquax-android"),
    ("lnomad", "lnomad"),
    ("lnpnd", "lnpnd"),
    ("lblogd", "lblogd"),
    ("leviculum-nrf", "leviculum-nrf"),
    ("leviculum-esp", "leviculum-esp"),
    ("leviculum-lr11xx", "leviculum-lr11xx"),
]

WORKSPACE = "workspace"
ANDROID_CODE_UNIT = "loquax-android"

# Directories a unit's version answers for beyond its own: the APK is
# loquax inside loquax-android, named by loquax's version (1667).
EXTRA_PATHS = {"loquax-android": ["loquax"]}


class Failure(Exception):
    """The script cannot do its work; exit 2 with the message."""


def git(root, *args, check=True):
    proc = subprocess.run(["git", *args], cwd=root, capture_output=True, text=True)
    if check and proc.returncode != 0:
        raise Failure(f"git {' '.join(args)}: {proc.stderr.strip()}")
    return proc


def show(root, rev, path):
    """The text of `path` at `rev`, or None when it did not exist there."""
    proc = git(root, "show", f"{rev}:{path}", check=False)
    return proc.stdout if proc.returncode == 0 else None


def section_version(text, section):
    """`version` under `[section]` in a manifest's text, or None."""
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError as error:
        raise Failure(f"manifest does not parse: {error}") from error
    node = data
    for key in section.split("."):
        node = node.get(key, {}) if isinstance(node, dict) else {}
    version = node.get("version") if isinstance(node, dict) else None
    return version if isinstance(version, str) else None


def android_code(text):
    """`[package.metadata.android] version-code`, or None."""
    if text is None:
        return None
    data = tomllib.loads(text)
    code = data.get("package", {}).get("metadata", {}).get("android", {}).get("version-code")
    return code if isinstance(code, int) else None


def workspace_unit(root):
    """The workspace unit's crate directories: every member whose version is
    `[workspace.package] version`, by value, from `cargo metadata`."""
    manifest = (root / "Cargo.toml").read_text()
    ws_version = section_version(manifest, "workspace.package")
    if ws_version is None:
        return []
    proc = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps", "--offline"],
        cwd=root,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise Failure(f"cargo metadata: {proc.stderr.strip()}")
    meta = json.loads(proc.stdout)
    dirs = []
    for package in meta["packages"]:
        if package["version"] != ws_version:
            continue
        crate_dir = Path(package["manifest_path"]).parent.resolve()
        dirs.append(crate_dir.relative_to(root.resolve()).as_posix())
    return sorted(dirs)


def units(root, own_units, extra_paths=EXTRA_PATHS):
    """[(name, manifest path, section, [paths])], the workspace first."""
    table = [(WORKSPACE, "Cargo.toml", "workspace.package", workspace_unit(root))]
    for name, crate_dir in own_units:
        if (root / crate_dir / "Cargo.toml").exists():
            paths = [crate_dir, *extra_paths.get(name, [])]
            table.append((name, f"{crate_dir}/Cargo.toml", "package", paths))
    return table


def sourced_packages(text):
    """The registry and git packages a lock file pins, as a set."""
    if text is None:
        return None
    packages = tomllib.loads(text).get("package", [])
    return {(p["name"], p["version"], p["source"]) for p in packages if "source" in p}


def changed(root, base, paths):
    """Whether any file under `paths` changed since `base`.

    A Cargo.lock counts only when a registry or git package in it moved.
    The bump itself rewrites the version of every path crate a lock names
    (the firmware's lock names leviculum-core), and counting that would
    make a core change move the firmware's version too, then the next
    landing's lock refresh move it again."""
    if not paths:
        return False
    # A trailing slash: `loquax` must not take `loquax-android/` with it.
    specs = [p.rstrip("/") + "/" for p in paths]
    proc = git(root, "diff", "--name-only", f"{base}..HEAD", "--", *specs)
    for name in proc.stdout.split():
        if Path(name).name != "Cargo.lock":
            return True
        head = (root / name).read_text() if (root / name).exists() else None
        if sourced_packages(show(root, base, name)) != sourced_packages(head):
            return True
    return False


def survey(root, base, own_units, code_unit=ANDROID_CODE_UNIT, extra_paths=EXTRA_PATHS):
    """One record per unit: changed, versions at base and HEAD, owed."""
    records = []
    for name, manifest, section, paths in units(root, own_units, extra_paths):
        head_text = (root / manifest).read_text()
        base_text = show(root, base, manifest)
        head_version = section_version(head_text, section)
        base_version = section_version(base_text, section) if base_text else None
        touched = changed(root, base, paths)
        # A unit new since the base has no older version to be newer than.
        owed = touched and base_version is not None and head_version == base_version
        record = {
            "unit": name,
            "manifest": manifest,
            "section": section,
            "changed": touched,
            "base": base_version,
            "head": head_version,
            "owed": owed,
        }
        if name == code_unit:
            base_code = android_code(base_text)
            head_code = android_code(head_text)
            record["base_code"] = base_code
            record["head_code"] = head_code
            if touched and base_code is not None and (head_code or 0) <= base_code:
                record["owed"] = True
        records.append(record)
    return records


DEV = re.compile(r"^(\d+\.\d+\.\d+)-dev(?:\.(\d+))?$")
PLAIN = re.compile(r"^(\d+)\.(\d+)\.(\d+)$")


def next_version(version):
    """The next version a landing gives `version`: the dev counter inside a
    development window, the patch otherwise. Anything else is not ours to
    guess at."""
    dev = DEV.match(version)
    if dev:
        counter = int(dev.group(2) or 0) + 1
        return f"{dev.group(1)}-dev.{counter}"
    plain = PLAIN.match(version)
    if plain:
        major, minor, patch = plain.groups()
        return f"{major}.{minor}.{int(patch) + 1}"
    raise Failure(f"cannot spell the next version after {version!r}")


def set_section_version(text, section, new):
    """Replace the first `version = "..."` under `[section]`, keeping every
    other byte of the manifest, its comments included."""
    lines = text.splitlines(keepends=True)
    inside = False
    for i, line in enumerate(lines):
        header = re.match(r"^\s*\[([^\]]+)\]\s*(#.*)?$", line)
        if header:
            inside = header.group(1).strip() == section
            continue
        if inside and re.match(r'^version\s*=\s*"', line):
            lines[i] = re.sub(r'"[^"]*"', f'"{new}"', line, count=1)
            return "".join(lines)
    raise Failure(f"no version line under [{section}]")


def set_android_code(text, code):
    new, count = re.subn(
        r"^(version-code\s*=\s*)\d+", rf"\g<1>{code}", text, count=1, flags=re.M
    )
    if count != 1:
        raise Failure("no version-code line under [package.metadata.android]")
    return new


def lock_roots(root):
    """Every directory holding a tracked Cargo.lock."""
    proc = git(root, "ls-files", "--", "Cargo.lock", "*/Cargo.lock")
    return sorted({str(Path(p).parent) for p in proc.stdout.split()})


def refresh_locks(root, bumped_packages):
    """Bring every Cargo.lock that names a bumped package up to its version.

    `cargo update --workspace --offline` and not `-p <crate>`: `-w` re-resolves
    the lock's own workspace members and every path dependency (which is how
    leviculum-core reaches the firmware's and the M9's locks) and leaves
    every registry package where it is, so the diff is the versions this
    script moved and nothing else. `-p` would need one call per package and
    per lock, and refuses a lock that does not contain the package. Offline,
    because a version bump fetches nothing."""
    for lock_dir in lock_roots(root):
        lock = (root / lock_dir / "Cargo.lock").read_text()
        if not any(f'name = "{name}"' in lock for name in bumped_packages):
            continue
        proc = subprocess.run(
            ["cargo", "update", "--workspace", "--offline"],
            cwd=root / lock_dir,
            capture_output=True,
            text=True,
        )
        if proc.returncode != 0:
            raise Failure(f"cargo update in {lock_dir}: {proc.stderr.strip()}")


def package_names(root, record):
    if record["unit"] == WORKSPACE:
        names = []
        for crate_dir in workspace_unit(root):
            data = tomllib.loads((root / crate_dir / "Cargo.toml").read_text())
            names.append(data["package"]["name"])
        return names
    data = tomllib.loads((root / record["manifest"]).read_text())
    return [data["package"]["name"]]


def bump(root, records, code_unit=ANDROID_CODE_UNIT, out=sys.stdout):
    """Bump every owed unit; print one VERSION line per unit moved."""
    bumped = []
    owed = [r for r in records if r["owed"]]
    # The workspace's package names before its version moves: afterwards
    # `workspace_unit` matches by the new version, which is the same set.
    names = {r["unit"]: package_names(root, r) for r in owed}
    for record in owed:
        path = root / record["manifest"]
        text = path.read_text()
        old = record["head"]
        new = old if old != record["base"] else next_version(old)
        if new != old:
            text = set_section_version(text, record["section"], new)
        line = f"VERSION unit={record['unit']} from={old} to={new}"
        if record["unit"] == code_unit:
            # The code moves with the version unless it already grew since
            # the base. A base without the field (before 1226 the Makefile
            # held it) cannot show growth, so the code moves.
            head_code = record["head_code"] or 0
            base_code = record["base_code"]
            code = head_code
            if base_code is None or head_code <= base_code:
                code = max(head_code, base_code or 0) + 1
            if code != head_code:
                text = set_android_code(text, code)
            line += f" code_from={head_code} code_to={code}"
        path.write_text(text)
        bumped.extend(names[record["unit"]])
        print(line, file=out)
    if bumped:
        refresh_locks(root, bumped)
    return bumped


def base_resolves(root, base):
    return git(root, "rev-parse", "--verify", "--quiet", f"{base}^{{commit}}", check=False).returncode == 0


NOT_ANCESTOR = "base is not an ancestor of HEAD; fetch origin"


def base_line(root, base):
    """`base=<sha12> (<rev>, <committer date>)`: what the guard compared
    against, so a stale base shows on the first line of the log."""
    proc = git(root, "log", "-1", "--format=%H %cI", f"{base}^{{commit}}")
    sha, date = proc.stdout.split()
    return sha[:12], f"base={sha[:12]} ({base}, {date})"


def is_ancestor(root, base):
    return git(root, "merge-base", "--is-ancestor", base, "HEAD", check=False).returncode == 0


def run(root, base, mode, own_units=OWN_UNITS, code_unit=ANDROID_CODE_UNIT, out=sys.stdout):
    if not base_resolves(root, base):
        if mode == "check":
            # A tarball or a clone without the remote has no landing to be
            # newer than. Say so rather than pass silently.
            print(f"version-guard: base {base} does not resolve here, nothing to compare", file=out)
            return 0
        raise Failure(f"base {base} does not resolve")
    sha12, line = base_line(root, base)
    if mode != "json":
        print(line, file=out)
    if not is_ancestor(root, base):
        print(f"version-guard: {NOT_ANCESTOR} (base {sha12})", file=out if mode != "json" else sys.stderr)
        return 2
    records = survey(root, base, own_units, code_unit)
    if mode == "json":
        print(json.dumps({"base": base, "base_sha": sha12, "units": records}, indent=2), file=out)
        return 1 if any(r["owed"] for r in records) else 0
    if mode == "check":
        owed = [r for r in records if r["owed"]]
        for r in owed:
            extra = ""
            if r["unit"] == code_unit:
                extra = f" code={r['head_code']} base_code={r['base_code']}"
            print(
                f"OWED unit={r['unit']} base_version={r['base']} head_version={r['head']}{extra}"
                f" (changed since {base})",
                file=out,
            )
        if owed:
            print(
                f"version-guard: {len(owed)} unit(s) changed since {base} without a newer version;"
                " run `python3 -I scripts/bump-touched.py` and commit the result",
                file=out,
            )
            return 1
        print(f"version-guard: every unit changed since {base} carries a newer version", file=out)
        return 0
    bump(root, records, code_unit, out=out)
    return 0


# ---------------------------------------------------------------- selftest


def _write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


_LOCK = """version = 4

[[package]]
name = "fx-a"
version = "{path_version}"

[[package]]
name = "fx-reg"
version = "{reg_version}"
source = "registry+https://github.com/rust-lang/crates.io-index"
"""


def _fixture(tmp):
    """Three units: workspace crate `a`, own-versioned `b`, and `c` with an
    Android version code. The base commit is tagged `base`."""
    root = Path(tmp) / "repo"
    _write(
        root / "Cargo.toml",
        '[workspace]\nresolver = "2"\nmembers = ["a", "b", "c"]\n\n'
        "[workspace.package]\n# the window\nversion = \"0.3.0-dev\"\nedition = \"2021\"\n",
    )
    _write(
        root / "a" / "Cargo.toml",
        '[package]\nname = "fx-a"\nversion.workspace = true\nedition.workspace = true\n',
    )
    _write(root / "a" / "src" / "lib.rs", "")
    _write(root / "b" / "Cargo.toml", '[package]\nname = "fx-b"\nversion = "0.1.0"\nedition = "2021"\n')
    _write(root / "b" / "src" / "lib.rs", "")
    _write(
        root / "c" / "Cargo.toml",
        '[package]\nname = "fx-c"\nversion = "0.2.0"\nedition = "2021"\n\n'
        "[package.metadata.android]\nversion-code = 4\n",
    )
    _write(root / "c" / "src" / "lib.rs", "")
    env_git = ["-c", "user.name=fx", "-c", "user.email=fx@example.invalid", "-c", "commit.gpgsign=false"]
    subprocess.run(["git", "init", "-q", "-b", "master"], cwd=root, check=True)
    subprocess.run(["cargo", "generate-lockfile", "--offline"], cwd=root, check=True, capture_output=True)
    subprocess.run(["git", "add", "-A"], cwd=root, check=True)
    subprocess.run(["git", *env_git, "commit", "-q", "-m", "base"], cwd=root, check=True)
    subprocess.run(["git", "tag", "base"], cwd=root, check=True)
    return root, env_git


def _commit(root, env_git, message):
    subprocess.run(["git", "add", "-A"], cwd=root, check=True)
    subprocess.run(["git", *env_git, "commit", "-q", "--allow-empty", "-m", message], cwd=root, check=True)


def selftest():
    """Returns a list of broken expectations; empty means every fixture held."""
    broken = []

    def expect(what, got, want):
        if got != want:
            broken.append(f"{what}: got {got!r}, want {want!r}")

    for old, new in [
        ("0.10.0-dev", "0.10.0-dev.1"),
        ("0.10.0-dev.1", "0.10.0-dev.2"),
        ("0.10.0-dev.9", "0.10.0-dev.10"),
        ("0.1.0", "0.1.1"),
        ("0.4.9", "0.4.10"),
    ]:
        expect(f"next_version({old})", next_version(old), new)
    try:
        next_version("1.0.0-rc.1")
        broken.append("next_version(1.0.0-rc.1) guessed instead of refusing")
    except Failure:
        pass

    units_fx = [("b", "b"), ("c", "c")]
    with tempfile.TemporaryDirectory(prefix="bump-touched-") as tmp:
        root, env_git = _fixture(tmp)
        expect("workspace unit from cargo metadata", workspace_unit(root), ["a"])

        # a changed; b changed and moved its version; c untouched.
        _write(root / "a" / "src" / "lib.rs", "pub fn a() {}\n")
        _write(root / "b" / "src" / "lib.rs", "pub fn b() {}\n")
        text = (root / "b" / "Cargo.toml").read_text().replace('"0.1.0"', '"0.1.5"')
        _write(root / "b" / "Cargo.toml", text)
        # A file outside every unit moves nothing.
        _write(root / "docs" / "x.md", "x\n")
        _commit(root, env_git, "work")

        owed = {r["unit"]: r["owed"] for r in survey(root, "base", units_fx)}
        expect("owed after work", owed, {WORKSPACE: True, "b": False, "c": False})
        sink = open(os.devnull, "w")
        expect("--check exit with one owed", run(root, "base", "check", units_fx, out=sink), 1)

        run(root, "base", "bump", units_fx, out=sink)
        manifest = (root / "Cargo.toml").read_text()
        expect("workspace version bumped", section_version(manifest, "workspace.package"), "0.3.0-dev.1")
        expect("window comment kept", "# the window\n" in manifest, True)
        lock = (root / "Cargo.lock").read_text()
        expect("lock follows the workspace", 'name = "fx-a"\nversion = "0.3.0-dev.1"' in lock, True)
        expect("b untouched by the bump", section_version((root / "b" / "Cargo.toml").read_text(), "package"), "0.1.5")
        _commit(root, env_git, "versions")
        expect("--check exit after the bump", run(root, "base", "check", units_fx, out=sink), 0)

        # A unit's own lock file that moved only a path crate's version (what
        # the bump does to every lock naming a workspace crate) is not a
        # change of that unit; a registry package moving in it is.
        _write(root / "c" / "Cargo.lock", _LOCK.format(path_version="0.3.0-dev", reg_version="1.0.0"))
        _commit(root, env_git, "c lock")
        subprocess.run(["git", "tag", "-f", "lockbase"], cwd=root, check=True, capture_output=True)
        _write(root / "c" / "Cargo.lock", _LOCK.format(path_version="0.3.0-dev.1", reg_version="1.0.0"))
        _commit(root, env_git, "c lock path version")
        expect("path-only lock move", changed(root, "lockbase", ["c"]), False)
        _write(root / "c" / "Cargo.lock", _LOCK.format(path_version="0.3.0-dev.1", reg_version="1.0.1"))
        _commit(root, env_git, "c lock registry version")
        expect("registry lock move", changed(root, "lockbase", ["c"]), True)
        (root / "c" / "Cargo.lock").unlink()
        _commit(root, env_git, "c lock gone, c as at base")

        # A second pass before the landing shares the bump.
        _write(root / "a" / "src" / "lib.rs", "pub fn a2() {}\n")
        _commit(root, env_git, "more work")
        expect("a second pass shares one bump", run(root, "base", "check", units_fx, out=sink), 0)
        subprocess.run(["git", "tag", "-f", "extrabase"], cwd=root, check=True, capture_output=True)

        # The code unit: c changes, its version and code both move.
        _write(root / "c" / "src" / "lib.rs", "pub fn c() {}\n")
        _commit(root, env_git, "c work")
        # A unit that answers for another's directory (loquax-android for
        # loquax, 1667) is owed when only that directory moved.
        owed_b = {r["unit"]: r["owed"] for r in survey(root, "extrabase", units_fx, "c", {"b": ["c"]})}["b"]
        expect("b owed by c's change through its extra path", owed_b, True)
        plain_b = {r["unit"]: r["owed"] for r in survey(root, "extrabase", units_fx, "c", {})}["b"]
        expect("b not owed by c's change without one", plain_b, False)
        expect("c owed", {r["unit"]: r["owed"] for r in survey(root, "base", units_fx, "c")}["c"], True)
        run(root, "base", "bump", units_fx, "c", out=sink)
        c_text = (root / "c" / "Cargo.toml").read_text()
        expect("c version", section_version(c_text, "package"), "0.2.1")
        expect("c code", android_code(c_text), 5)
        _commit(root, env_git, "c versions")
        expect("c settled", run(root, "base", "check", units_fx, "c", out=sink), 0)
        # A version moved without the code is still owed.
        _write(root / "c" / "Cargo.toml", c_text.replace("version-code = 5", "version-code = 4"))
        _commit(root, env_git, "code back")
        expect("code not grown is owed", {r["unit"]: r["owed"] for r in survey(root, "base", units_fx, "c")}["c"], True)

        expect("unresolvable base in check mode", run(root, "no-such-rev", "check", units_fx, out=sink), 0)

        # The base line comes first, and an OWED line names both versions.
        _write(root / "b" / "src" / "lib.rs", "pub fn b2() {}\n")
        _commit(root, env_git, "b work after its bump")
        subprocess.run(["git", "tag", "-f", "bbase"], cwd=root, check=True, capture_output=True)
        _write(root / "b" / "src" / "lib.rs", "pub fn b3() {}\n")
        _commit(root, env_git, "b work past bbase")
        buf = io.StringIO()
        expect("owed b against bbase", run(root, "bbase", "check", units_fx, out=buf), 1)
        lines = buf.getvalue().splitlines()
        sha12 = git(root, "rev-parse", "bbase^{commit}").stdout.strip()[:12]
        expect("base line first", bool(lines) and lines[0].startswith(f"base={sha12} (bbase, "), True)
        expect(
            "OWED line names both versions",
            any(line.startswith("OWED unit=b base_version=0.1.5 head_version=0.1.5 ") for line in lines),
            True,
        )
        buf = io.StringIO()
        expect("clean case exit", run(root, "HEAD", "check", units_fx, out=buf), 0)
        head12 = git(root, "rev-parse", "HEAD").stdout.strip()[:12]
        head_date = git(root, "log", "-1", "--format=%cI", "HEAD").stdout.strip()
        expect("clean case base line", buf.getvalue().splitlines()[:1], [f"base={head12} (HEAD, {head_date})"])

        # A base off HEAD's history (a side branch, or a fetch from
        # somewhere else) is no landing to compare against: exit 2, in
        # check and bump mode alike, with nothing bumped.
        subprocess.run(["git", "checkout", "-q", "-b", "side", "base"], cwd=root, check=True)
        _write(root / "a" / "src" / "lib.rs", "pub fn side() {}\n")
        _commit(root, env_git, "side")
        subprocess.run(["git", "tag", "-f", "stale"], cwd=root, check=True, capture_output=True)
        subprocess.run(["git", "checkout", "-q", "master"], cwd=root, check=True)
        before = (root / "Cargo.toml").read_text()
        buf = io.StringIO()
        expect("non-ancestor base in check mode", run(root, "stale", "check", units_fx, out=buf), 2)
        expect("non-ancestor message", NOT_ANCESTOR in buf.getvalue(), True)
        expect("non-ancestor base in bump mode", run(root, "stale", "bump", units_fx, out=sink), 2)
        expect("non-ancestor bump changed nothing", (root / "Cargo.toml").read_text(), before)
        sink.close()

    broken.extend(makefile_selftest())
    return broken


def makefile_selftest():
    """The Android Makefile takes the versionCode from its Cargo.toml and the
    versionName from loquax's (1667): the real Makefile beside fixture
    manifests whose values match none of the old defaults, and its own
    crate's version differs from loquax's so a wrong read shows."""
    makefile = ROOT / "loquax-android" / "Makefile"
    if shutil.which("make") is None:
        return ["make is not installed; the Makefile's version read cannot be checked"]
    with tempfile.TemporaryDirectory(prefix="bump-touched-make-") as tmp:
        android = Path(tmp) / "loquax-android"
        android.mkdir()
        shutil.copy(makefile, android / "Makefile")
        _write(
            android / "Cargo.toml",
            '[package]\nname = "x"\n# a comment\nversion = "1.2.3"\n\n'
            '[dependencies]\nfoo = { version = "1" }\n\n'
            "[package.metadata.android]\nversion-code = 4242\n",
        )
        _write(
            Path(tmp) / "loquax" / "Cargo.toml",
            '[package]\nname = "loquax"\n# a comment\nversion = "9.8.7"\n\n'
            '[dependencies]\nfoo = { version = "1" }\n',
        )
        proc = subprocess.run(
            ["make", "-s", "--no-print-directory", "-C", str(android), "print-version"],
            capture_output=True,
            text=True,
        )
        got = proc.stdout.strip()
        want = "VERSION_CODE=4242 VERSION_NAME=9.8.7"
        if got != want:
            return [f"Makefile print-version: got {got!r} ({proc.stderr.strip()}), want {want!r}"]
    return []


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--base", default="origin/master")
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--selftest", action="store_true")
    args = parser.parse_args()
    try:
        if args.selftest:
            broken = selftest()
            for line in broken:
                print(f"bump-touched selftest: {line}", file=sys.stderr)
            if broken:
                return 1
            print("bump-touched selftest: every fixture held")
            return 0
        mode = "json" if args.json else "check" if args.check else "bump"
        return run(ROOT, args.base, mode)
    except Failure as error:
        print(f"bump-touched: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
