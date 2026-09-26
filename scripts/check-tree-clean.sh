#!/usr/bin/env bash
# A gate must not dirty the tree it runs in (Codeberg #295).
#
# What happened: `just fast` -> `fuzz-regress` built a fuzz crate whose
# committed Cargo.lock no longer matched its dependency graph, cargo wrote the
# three missing lines to disk, and the run stayed green. The tree it wrote into
# was the landing gate's shared push tree, and a gate refuses to run in a dirty
# one: the next one stopped with rc=5 on 3b1bf00e, and nothing in either gate's
# log said which run had modified the file -- the gate that broke it was green,
# and the gate that reported it had not run anything yet.
#
# The lock is fixed at its source and `run-fuzz.sh` now refuses a stale one by
# name. This script is the layer BELOW that: whatever else in the tier learns
# the same habit -- a build script writing a generated file, a test leaving a
# fixture behind, a formatter run without --check -- is caught here, in the run
# that caused it, naming the path.
#
# WHY A SNAPSHOT AND NOT SIMPLY "THE TREE IS CLEAN": the gate's push tree is
# clean before it starts, so there the two are the same question. A developer's
# checkout is not, and a tier that went red because its author had uncommitted
# work would be turned off within a week. So the subject is the DELTA the tier
# produced: `--snapshot` before the first recipe, `--verify` after the last,
# and only paths that changed in between are a finding.
#
# A path's content is hashed, not just its status code: a file already modified
# before the tier and modified AGAIN by a recipe keeps the same ` M` line, and
# that is exactly the lockfile case one edit later.
#
# Usage:
#   scripts/check-tree-clean.sh --snapshot     # before the tier's first recipe
#   scripts/check-tree-clean.sh --verify       # after its last
#
#   --repo DIR    the checkout to watch          (default: this script's repo)
#   --state FILE  where the snapshot is kept     (default: under
#                 ~/.local/state/leviculum-ci/, keyed by the checkout's path so
#                 the coder tree, the push tree and the rig tree cannot read
#                 each other's snapshot)
#
# `--verify` CONSUMES the snapshot: a leftover baseline from a run that died
# half-way would otherwise be compared against a tree that has been committed
# to since, and report a fix as damage. Without a snapshot the baseline is
# "clean", said out loud on the BASELINE line, which is what a bare
# `just check-tree-clean` in a gate's tree should mean anyway.
#
# Exit 0 = the tier changed no tracked file. Exit 1 = it did, and they are
# named. Exit 2 = the check could not run (not a git checkout, bad arguments).

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$SCRIPT_DIR/.." && pwd)"
MODE=""
STATE=""

die() { echo "[check-tree-clean] ERROR: $*" >&2; exit 2; }

while [ $# -gt 0 ]; do
    case "$1" in
        --snapshot) MODE=snapshot; shift ;;
        --verify)   MODE=verify; shift ;;
        --repo)     REPO="${2:?--repo needs a directory}"; shift 2 ;;
        --state)    STATE="${2:?--state needs a file}"; shift 2 ;;
        -h|--help)  sed -n '2,47p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *)          die "unknown argument '$1' (want --snapshot or --verify)" ;;
    esac
done

[ -n "$MODE" ] || die "say which half: --snapshot or --verify"
REPO="$(cd "$REPO" 2>/dev/null && pwd)" || die "no such directory"
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || die "$REPO is not a git checkout"

if [ -z "$STATE" ]; then
    # Keyed by the checkout's absolute path: this host runs three checkouts of
    # this repository (coder, push-tree, rig) and a shared state file would
    # hand one tree's baseline to another.
    key="$(printf '%s' "$REPO" | sha256sum | cut -c1-12)"
    STATE="${LEVICULUM_CI_STATE:-$HOME/.local/state/leviculum-ci}/tree-clean-$key.txt"
fi

HEAD_SHA="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || echo unborn)"

# One line per dirty tracked path: "<path>\t<XY>\t<sha256-of-worktree-content>",
# path first so the file sorts, and reads, by the name a person is looking for.
# `--untracked-files=no` is deliberate, and is the same question the landing
# gate's own precondition asks: a build that leaves a log or a target/ artefact
# behind is untidy, a build that edits a TRACKED file is what refuses the next
# gate.
#
# core.quotepath=off keeps a non-ASCII path readable rather than escaped, and
# the path is everything after the two status columns and one space. A rename
# reports "old -> new"; the new name is the one that exists to hash.
tree_state() {
    git -C "$REPO" -c core.quotepath=off status --porcelain --untracked-files=no \
    | while IFS= read -r line; do
        code="${line:0:2}"
        path="${line:3}"
        case "$path" in *" -> "*) path="${path##* -> }" ;; esac
        if [ -f "$REPO/$path" ]; then
            sum="$(sha256sum -- "$REPO/$path" | cut -d' ' -f1)"
        else
            sum="-"
        fi
        printf '%s\t%s\t%s\n' "$path" "$code" "$sum"
    done | LC_ALL=C sort
}

if [ "$MODE" = snapshot ]; then
    mkdir -p "$(dirname "$STATE")" || die "cannot create $(dirname "$STATE")"
    {
        echo "# check-tree-clean baseline, taken $(date -Is)"
        echo "head $HEAD_SHA"
        tree_state
    } > "$STATE" || die "cannot write $STATE"
    entries="$(grep -cv '^\(#\|head \)' "$STATE")"
    echo "TREE_SNAPSHOT head=$HEAD_SHA dirty_entries=$entries state=$STATE"
    exit 0
fi

BASELINE="$(mktemp)"
CURRENT="$(mktemp)"
trap 'rm -f "$BASELINE" "$CURRENT"' EXIT

source_kind=clean
if [ -f "$STATE" ]; then
    snap_head="$(sed -n 's/^head //p' "$STATE" | head -1)"
    if [ "$snap_head" = "$HEAD_SHA" ]; then
        grep -v '^\(#\|head \)' "$STATE" > "$BASELINE"
        source_kind=snapshot
    else
        # A baseline from before a commit describes a tree that no longer
        # exists. Compare against clean instead, and say so: silently trusting
        # it would report the commit's own paths as damage.
        echo "TREE_BASELINE_STALE snapshot_head=$snap_head head=$HEAD_SHA state=$STATE" >&2
    fi
    rm -f "$STATE"
fi
tree_state > "$CURRENT"

base_n="$(wc -l < "$BASELINE")"
echo "TREE_CLEAN_BASELINE source=$source_kind head=$HEAD_SHA baseline_entries=$base_n"

# Set difference both ways, on the whole line, so a content change on an
# already-dirty path is a difference too.
dirtied=0
reverted=0
while IFS=$'\t' read -r path code sum; do
    [ -n "${sum:-}" ] || continue
    echo "TREE_DIRTIED path=$path status=\"$code\" sha256=$sum"
    dirtied=$((dirtied + 1))
done < <(LC_ALL=C comm -13 "$BASELINE" "$CURRENT")
while IFS=$'\t' read -r path code sum; do
    [ -n "${sum:-}" ] || continue
    # The other direction: a recipe that REVERTED somebody's edit, or staged
    # one, changed a tracked file just as much.
    echo "TREE_UNDIRTIED path=$path was_status=\"$code\" was_sha256=$sum"
    reverted=$((reverted + 1))
done < <(LC_ALL=C comm -23 "$BASELINE" "$CURRENT")

if [ "$dirtied" = 0 ] && [ "$reverted" = 0 ]; then
    echo "TREE_CLEAN_CHECK status=GREEN baseline=$source_kind dirtied=0 undirtied=0"
    exit 0
fi

echo "TREE_CLEAN_CHECK status=RED baseline=$source_kind dirtied=$dirtied undirtied=$reverted" >&2
{
    echo
    echo "A recipe in this run modified the tracked file(s) named above."
    echo "That is what refuses the NEXT gate in a shared tree: its clean-tree"
    echo "precondition fails with rc=5, and landing stops until somebody reverts"
    echo "the file by hand."
    echo "Find the recipe that writes it and make the file an input:"
    echo "  * a Cargo.lock: the build resolves a graph the lock does not match."
    echo "    Regenerate and COMMIT it, and build --locked so the next stale"
    echo "    lock fails instead of being rewritten (run-fuzz.sh does)."
    echo "  * a generated source or fixture: generate it into target/, or make"
    echo "    the recipe compare instead of write (cargo fmt --check, not fmt)."
} >&2
exit 1
