#!/usr/bin/env bash
# The window bench's steps (1441, 1470), which `just window-bench` runs in
# turn: prepare, ROUNDS rounds, compare. Apart, so each can be run on its
# own (a round is about 5 min on schneckenschreck, the whole recipe 20).
#
#   window-bench.sh prepare BASE   build both benches, decide A/B or A/A
#   window-bench.sh prepare-v3     this tree's bench as BASE, the same built
#                                  for x86-64-v3 as this (1469)
#   window-bench.sh round I        round I: BASE's bench, then this tree's
#   window-bench.sh compare        the table and the verdict
#   window-bench.sh clean          remove BASE's worktree
#
# BASE is built in a worktree under target/window-bench/base-src into its
# OWN target directory, target/window-bench/base-target. With one target
# directory for both, the dep-info named the sources relative to the
# workspace root, BASE's and this tree's units hashed the same, and cargo
# took BASE's artefacts as fresh for this tree whenever this tree's files
# were older than BASE's build: on 2026-11-07 the A/B compared one binary
# with itself (`Finished in 0.31s`, both exes the same bytes). The exes'
# sha256 are checked: the same bytes in an A/B is red.
#
# BASE naming this tree's HEAD in a tree with no tracked change is the A/A
# selftest: one exe, run as both sides; every row must be held or noise.
#
# The load: the 1-minute load average is recorded at the start of each side
# of each round; above the bound the compare refuses a verdict (exit 3,
# SKIP). The bound is WINDOW_BENCH_MAX_LOAD, else a fifth of the host's
# CPUs (2.0 on schneckenschreck's 10): the A/A of 2026-11-07 at loads 0.4
# to 4.9 moved rows from -25.8 % to +11.8 %. The load average counts the
# whole host, so the bound divides the host's CPUs, not the ones this
# process may run on (a coder pass is pinned to 4 of the 10).
set -euo pipefail

out="$(realpath -m target/window-bench)"
src="$out/base-src"

exe() {
    python3 -c 'import json,sys
for l in sys.stdin:
    m = json.loads(l)
    if m.get("reason") == "compiler-artifact" and m.get("executable") and m["target"]["name"] == "loquax" and m["profile"]["test"]:
        print(m["executable"])'
}

build() {
    local host
    host="$(rustc -vV | sed -n 's/^host: //p')"
    cargo test --release -p loquax --features gui --bins --target "$host" --no-run --message-format=json-render-diagnostics | exe
}

clean() {
    git worktree remove --force "$src" 2>/dev/null || rm -rf "$src"
    git worktree prune
}

case "${1:-}" in
prepare)
    base="${2:-}"
    [ -n "$base" ] || base="$(git log -1 --format=%H -- loquax/tests/window_bench.baseline)"
    base="$(git rev-parse --verify "$base^{commit}")"
    mkdir -p "$out"
    rm -f "$out"/base-*.txt "$out"/this-*.txt "$out"/base-*.log "$out"/this-*.log "$out/load.txt" "$out/mode" "$out/exes.txt"
    clean
    if [ "$base" = "$(git rev-parse HEAD)" ] && git diff --quiet HEAD; then
        mode=aa
    else
        mode=ab
    fi
    cp "$(build)" "$out/this-bench"
    if [ "$mode" = aa ]; then
        cp "$out/this-bench" "$out/base-bench"
    else
        git worktree add -q --detach "$src" "$base"
        # The base's bench reads its fixture from its own tree: the worktree
        # stays until `clean`.
        (cd "$src" && CARGO_TARGET_DIR="$out/base-target" build) > "$out/base-exe"
        cp "$(cat "$out/base-exe")" "$out/base-bench"
        rm "$out/base-exe"
    fi
    echo "$mode" > "$out/mode"
    echo "window-bench: $mode, BASE $base, this $(git rev-parse HEAD)$(git diff --quiet HEAD || echo ' with changes')" | tee "$out/exes.txt"
    python3 -I scripts/window-bench-compare.py --check-exes "$mode" "$out/base-bench" "$out/this-bench" | tee -a "$out/exes.txt"
    ;;
prepare-v3)
    # The package's two loquax builds compared (1469): this tree's bench,
    # baseline, as BASE, and the same sources built with
    # -C target-cpu=x86-64-v3 into build-deb.sh's v3 target directory as
    # this. An A/B: the exes must differ. Both read this tree's fixtures.
    mkdir -p "$out"
    rm -f "$out"/base-*.txt "$out"/this-*.txt "$out"/base-*.log "$out"/this-*.log "$out/load.txt" "$out/mode" "$out/exes.txt"
    clean
    cp "$(build)" "$out/base-bench"
    v3_exe="$(CARGO_TARGET_DIR="$(realpath -m target/x86-64-v3)" CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C target-cpu=x86-64-v3" build)"
    cp "$v3_exe" "$out/this-bench"
    echo ab > "$out/mode"
    echo "window-bench: v3, base = this tree's baseline bench, this = the same built for x86-64-v3, $(git rev-parse HEAD)$(git diff --quiet HEAD || echo ' with changes')" | tee "$out/exes.txt"
    python3 -I scripts/window-bench-compare.py --check-exes ab "$out/base-bench" "$out/this-bench" | tee -a "$out/exes.txt"
    ;;
round)
    i="${2:?round number}"
    for side in base this; do
        read -r load _ < /proc/loadavg
        echo "$side $i $load" >> "$out/load.txt"
        LOQUAX_WINDOW_BENCH="$out/$side-$i.txt" "$out/$side-bench" --exact gui::bench::window_bench --nocapture > "$out/$side-$i.log" 2>&1
    done
    ;;
compare)
    bound="${WINDOW_BENCH_MAX_LOAD:-$(python3 -c 'import os; print(os.cpu_count() / 5)')}"
    status=0
    python3 -I scripts/window-bench-compare.py --base "$out"/base-*.txt --this "$out"/this-*.txt \
        --load "$out/load.txt" --max-load "$bound" --mode "$(cat "$out/mode")" > "$out/table.md" || status=$?
    cat "$out/exes.txt" "$out/table.md"
    exit "$status"
    ;;
clean)
    clean
    ;;
*)
    sed -n '2,14p' "$0" >&2
    exit 2
    ;;
esac
