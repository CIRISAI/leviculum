#!/usr/bin/env python3
"""Rustdoc over the crates this branch touched -- the cheap half of `doc-gate`.

`doc-gate` runs `cargo doc --workspace --no-deps` under `-D warnings`, so a
broken or private intra-doc link is a land gate failure. It costs ~21 s over
the whole workspace and is therefore excused from `guards`, and that excusal
is the hole Codeberg #359 fell into: pass 344 wrote `[`Column`]` and
`[`json_string`]` into leviculum-lxmf-node/src/telemetry.rs, gated on
`cargo test`, fmt, clippy and `just guards`, and not one of those four runs
rustdoc. The red surfaced a day later on the land gate with sixty-three
commits queued behind it.

Docing only what changed is the part of that gate a coder pass can afford. A
pass touches one or two crates; `cargo doc --no-deps -p <crate>` for those is
seconds rather than twenty.

WHAT COUNTS AS TOUCHED -- three sources, unioned:

  * `git diff --name-only <merge-base with origin/master> HEAD`: every commit
    this branch has that the remote does not, so a pass sees its own landed
    work and that of every earlier pass still unpushed on the same lane.
  * `git diff --name-only HEAD`: staged and unstaged work. A pass gates
    BEFORE it commits, so without this the recipe would be blind to the very
    change it is being run about.
  * `git ls-files --others --exclude-standard`: a brand-new file in a crate
    is a change to that crate.

With no `origin/master` (a plain clone, a detached CI checkout) the first
source is skipped rather than fatal; the other two still apply.

WHAT IT SKIPS. Paths outside every workspace member -- docs/, scripts/, the
Justfile. Nothing touched inside the workspace is a pass, not an error: there
is no documentation to break.

leviculum-nrf and leviculum-esp are their own cargo workspaces, so
`cargo metadata` here does not list them and `cargo doc -p` from this root
cannot reach them. This recipe therefore cannot document them, and until
2026-09-28 the paragraph above said it did not have to, because `lint-nrf`
and `build-esp-image` carried their own rustdoc gate. Half of that was
false: `build-esp32` (the recipe's real name) has always run
`RUSTDOCFLAGS="-D warnings" cargo doc`, `lint-nrf` never did. `just
doc-touched` was green on a batch it could not see, and when 362 measured
the gap it was 18 broken intra-doc links across the nrf host members and 50
across 11 modules of the firmware crate (Codeberg #367). `lint-nrf` now
carries the line for both halves of its workspace, per bundle feature set.

So the skip stands -- and is now announced. A branch that touched either
foreign workspace gets a line naming the recipe that docs it, because the
cost of the hole was not the skip but its silence.

MEMORY (#1423). Twice on 2026-11-04 the kernel killed rustdoc in this
recipe on the coder host (anon-rss 3.6 GB and 4.0 GB), each time ending a
pass. Measured per crate under `/usr/bin/time` the next evening: every crate
cost the same 4.4 to 5.1 GB and 10 s, nmea0183 (a few hundred lines) as much
as leviculum-core. The crate was never the cost. The doc output directory
was: cargo writes every crate it ever documented into one shared
`<target>/<triple>/doc`, and rustdoc reads the merged search index of all of
them back on every run to write its own part. On that tree it held 86 MB of
index in 1458 files, forty-odd crates and bins, the stale ones included, and
a crate with seven bins (leviculum-std, leviculum-cli) paid it eight times.
With that directory removed first the same crates cost 0.18 to 0.73 GB and
0.3 to 2.9 s (table in the report of 1423). `--no-deps` was already there;
the job cap is explained at `JOBS` below.

So the recipe now starts from an empty doc directory: it is a checker, not a
documentation build, and nothing reads that output (the docs tarball is built
by its own script into target/docs). Cargo notices the missing output and
reruns rustdoc for the touched crates, so the gate is not made vacuous.

It also refuses to start when `MemAvailable` is below `NEED_MB`, printing the
numbers, rather than letting the kernel pick a victim among the processes
of every lane on the host, and it prints the peak RSS of the whole cargo
process tree at the end, summed over concurrent processes, so the next
change to `NEED_MB` is a reading and not a guess.

Usage:
  python3 scripts/doc-touched.py
"""

import json
import os
import shutil
import subprocess
import sys
import time

REPO_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def git(*args, check=True):
    """Stripped stdout lines of a git command, or [] when it is allowed to fail."""
    proc = subprocess.run(
        ["git", *args], cwd=REPO_DIR, capture_output=True, text=True
    )
    if proc.returncode != 0:
        if check:
            raise RuntimeError(f"git {' '.join(args)}: {proc.stderr.strip()}")
        return []
    return [line for line in proc.stdout.splitlines() if line]


def metadata():
    """`cargo metadata --no-deps` of this workspace, parsed."""
    proc = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=REPO_DIR,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"cargo metadata: {proc.stderr.strip()}")
    return json.loads(proc.stdout)


def members(meta):
    """[(package name, directory relative to the workspace root)]."""
    root = meta["workspace_root"]
    return [
        (pkg["name"], os.path.relpath(os.path.dirname(pkg["manifest_path"]), root))
        for pkg in meta["packages"]
    ]


def changed_paths():
    """Every path this branch has moved, committed or not."""
    paths = set()
    if git("rev-parse", "--verify", "--quiet", "origin/master", check=False):
        base = git("merge-base", "origin/master", "HEAD")[0]
        paths.update(git("diff", "--name-only", base, "HEAD"))
    paths.update(git("diff", "--name-only", "HEAD"))
    paths.update(git("ls-files", "--others", "--exclude-standard"))
    return paths


def touched_crates(member_list, paths):
    """The workspace packages those paths belong to.

    Longest matching directory wins, so a nested member (vendor/nmea0183) is
    attributed to itself rather than to whatever sits above it.
    """
    hit = set()
    for path in paths:
        best = None
        for name, directory in member_list:
            prefix = "" if directory == "." else directory + "/"
            if path.startswith(prefix) and (best is None or len(prefix) > len(best[1])):
                best = (name, prefix)
        if best:
            hit.add(best[0])
    return sorted(hit)


# The foreign cargo workspaces this recipe cannot document, and the recipe that
# does. `cargo doc -p <crate>` from this root fails for both: they are not
# members here, so `cargo metadata --no-deps` never names them.
FOREIGN_WORKSPACES = {
    "leviculum-nrf/": "just lint-nrf",
    "leviculum-esp/": "just build-esp32",
    "leviculum-lr11xx/": "just lint-lr11xx",
}


def foreign_notes(paths):
    """[(workspace prefix, recipe)] for the foreign workspaces this branch touched."""
    return sorted(
        (prefix, recipe)
        for prefix, recipe in FOREIGN_WORKSPACES.items()
        if any(path.startswith(prefix) for path in paths)
    )


# Two rustdoc (or rustc, when a dependency's metadata has to be rebuilt)
# processes at a time. The doc directory is shared within one run too, so a
# crate documented late re-reads the index of every crate before it, and with
# all 26 members touched at once (the worst case a branch can hand this
# recipe) cargo's default ten jobs summed to 5.0 GB, the size of the kills it
# was meant to end. Measured on schneckenschreck on 2026-11-04, warm, empty
# doc directory, summed peak and wall time: ten jobs 5004 MB 7.4 s, four 2455
# MB 11.2 s, two 1776 MB 19.3 s, one 1234 MB 36.7 s. A single crate costs the
# same at any setting (0.18 to 0.73 GB), so the cap only bites on the large
# sets, and two halves the worst case for less than a doubling of its time.
JOBS = 2

# The guard: the worst summed peak at JOBS above, plus a margin for growth
# until the next reading and for a dependency rebuild riding along. The worst
# reading was not the 26-crate set but this lane's own 13 touched crates the
# same evening, 1855 MB; the order cargo picks moves the peak by about 5 %.
MEASURED_MB = 1855
MARGIN_MB = 1024
NEED_MB = MEASURED_MB + MARGIN_MB


def mem_available_mb():
    """MemAvailable in MB, or None where /proc/meminfo has no such line."""
    try:
        with open("/proc/meminfo") as meminfo:
            for line in meminfo:
                if line.startswith("MemAvailable:"):
                    return int(line.split()[1]) // 1024
    except OSError:
        return None
    return None


def doc_dirs(target_directory):
    """The rustdoc output directories under the target dir.

    `<target>/doc` for a host build and `<target>/<triple>/doc` for each
    triple a build named (this workspace pins x86_64-unknown-linux-musl in
    .cargo/config.toml, and CARGO_BUILD_TARGET can name another). Only a
    directory that rustdoc wrote is returned: it holds `crates.js`.
    """
    candidates = [os.path.join(target_directory, "doc")]
    try:
        entries = sorted(os.listdir(target_directory))
    except OSError:
        entries = []
    candidates += [os.path.join(target_directory, entry, "doc") for entry in entries]
    return [
        path
        for path in candidates
        if os.path.isfile(os.path.join(path, "crates.js"))
    ]


def tree_rss_kb(root_pid):
    """(summed RSS, largest single RSS) in kB of root_pid and its descendants."""
    children = {}
    rss = {}
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open(f"/proc/{entry}/stat") as stat:
                # The comm field may hold spaces and parentheses; the ppid is
                # the second field after its closing one.
                ppid = int(stat.read().rsplit(")", 1)[1].split()[1])
            with open(f"/proc/{entry}/status") as status:
                kb = next(
                    (int(line.split()[1]) for line in status if line.startswith("VmRSS:")),
                    0,
                )
        except (OSError, ValueError, IndexError):
            continue
        children.setdefault(ppid, []).append(int(entry))
        rss[int(entry)] = kb
    total = largest = 0
    stack = [root_pid]
    while stack:
        pid = stack.pop()
        total += rss.get(pid, 0)
        largest = max(largest, rss.get(pid, 0))
        stack.extend(children.get(pid, []))
    return total, largest


def watch_peak_rss(proc, interval=0.1):
    """Wait for proc, sampling its process tree; (peak summed, peak single) kB.

    A sampled sum, not getrusage: RUSAGE_CHILDREN reports the largest single
    descendant, and what the kernel runs out of is the sum of the rustdoc and
    rustc processes cargo runs at once.
    """
    peak_tree = peak_single = 0
    while proc.poll() is None:
        total, largest = tree_rss_kb(proc.pid)
        peak_tree = max(peak_tree, total)
        peak_single = max(peak_single, largest)
        time.sleep(interval)
    return peak_tree, peak_single


def main():
    paths = changed_paths()
    for prefix, recipe in foreign_notes(paths):
        print(
            f"doc-touched: {prefix.rstrip('/')} is its own workspace and is not "
            f"documented here; `{recipe}` carries its rustdoc gate.",
            flush=True,
        )
    meta = metadata()
    crates = touched_crates(members(meta), paths)
    if not crates:
        print("doc-touched: no workspace crate touched; nothing to document.")
        return 0

    available = mem_available_mb()
    if available is not None and available < NEED_MB:
        print(
            f"doc-touched: refusing to start: MemAvailable is {available} MB, "
            f"this recipe needs {NEED_MB} MB ({MEASURED_MB} MB measured peak "
            f"plus {MARGIN_MB} MB margin). Free memory or wait for the other "
            "lanes, rather than let the kernel choose which process dies.",
            file=sys.stderr,
            flush=True,
        )
        return 2

    for directory in doc_dirs(meta["target_directory"]):
        shutil.rmtree(directory)

    print(f"doc-touched: {' '.join(crates)}", flush=True)
    args = []
    for crate in crates:
        args += ["-p", crate]
    env = dict(os.environ, RUSTDOCFLAGS="-D warnings")
    proc = subprocess.Popen(["cargo", "doc", "--no-deps", "-j", str(JOBS), *args], cwd=REPO_DIR, env=env)
    peak_tree, peak_single = watch_peak_rss(proc)
    print(
        f"doc-touched: peak RSS {peak_tree // 1024} MB summed over the cargo "
        f"process tree, {peak_single // 1024} MB in its largest process "
        f"(MemAvailable was {available} MB, guard {NEED_MB} MB)",
        flush=True,
    )
    return proc.returncode


if __name__ == "__main__":
    sys.exit(main())
