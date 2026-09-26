#!/usr/bin/env bash
# Fixture test for scripts/check-tree-clean.sh (Codeberg #295).
#
# The check exists because a gate that dirties its own tree refuses the NEXT
# gate, in a run whose logs say nothing about it. A check for that failure
# which has never been watched fire is worth nothing, so every case here
# INJECTS the damage into a throwaway checkout and asserts what the check
# concluded from it.
#
#   1. A RECIPE THAT REWRITES A TRACKED LOCKFILE. The positive control, and the
#      #295 shape exactly: a fixture justfile whose middle step appends to a
#      tracked Cargo.lock, with the snapshot as its first dependency and the
#      verify in its body -- so this also tests the WIRING `just fast` uses,
#      that the snapshot runs before the recipes and the verify after them.
#      Asserts exit 1 and the file named.
#   2. THE SAME TIER WITHOUT THE DIRTYING STEP. Counterweight to 1: green, so
#      case 1 is not "this always fails".
#   3. A TREE THAT WAS ALREADY DIRTY. The check's whole reason for taking a
#      baseline: uncommitted work of the author's is NOT a finding, and the
#      tier stays green over it.
#   4. ALREADY DIRTY, AND DIRTIED AGAIN. What a status code alone cannot see:
#      the path keeps its ` M` line while its content changes, which is the
#      lockfile case one edit later. Caught by the content hash.
#   5. NO SNAPSHOT MEANS THE BASELINE IS "CLEAN", OUT LOUD. What a bare
#      `just check-tree-clean` does in a gate's tree, and it must say which
#      baseline it used rather than implying one.
#   6. A SNAPSHOT FROM BEFORE A COMMIT IS NOT TRUSTED. A run that died half-way
#      leaves a baseline behind; the tree is committed to; the next verify must
#      not report the commit's own paths as damage.
#   7. THE CHECK CANNOT RUN. Not a git checkout, or no mode: exit 2, never a
#      green verdict over a question it did not ask.
#
# Needs `just` (case 1 and 2 drive a real justfile). ~2 s.
#
# Usage: bash scripts/test-tree-clean.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CHECK="$SCRIPT_DIR/check-tree-clean.sh"
REPO="$(cd "$SCRIPT_DIR/.." && pwd)"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

FAILED=0
pass() { echo "  PASS: $1"; }
fail() { echo "  FAIL: $1" >&2; FAILED=1; }
assert() { if [ "$1" = 0 ]; then pass "$2"; else fail "$2"; fi; }
expect_eq() { if [ "$1" = "$2" ]; then pass "$3"; else fail "$3 -- got '$1', wanted '$2'"; fi; }

# A throwaway checkout with one tracked file that looks like the real victim.
# $1 = directory name.
make_repo() {
    local dir="$WORK/$1"
    mkdir -p "$dir"
    git -C "$dir" init --quiet
    git -C "$dir" config user.email fixture@example.invalid
    git -C "$dir" config user.name "Fixture"
    printf '# generated\nversion = 4\n' > "$dir/Cargo.lock"
    echo 'fn main() {}' > "$dir/main.rs"
    git -C "$dir" add -A
    git -C "$dir" commit --quiet -m "fixture"
    echo "$dir"
}

# A fixture tier with the same wiring `just fast` has: snapshot first, the
# recipes after it, verify last. $2 is the shell line the middle step runs.
write_fixture_tier() {
    local dir="$1" step="$2"
    cat > "$dir/Justfile" <<EOF
fixture-tier: _snapshot _work
    bash $CHECK --verify --repo $dir --state $dir/snapshot.txt

_snapshot:
    bash $CHECK --snapshot --repo $dir --state $dir/snapshot.txt

_work:
    $step
EOF
}

echo "=== case 1: a step that rewrites a tracked lockfile is caught and named ==="
if ! command -v just >/dev/null 2>&1; then
    echo "SKIP: just is not on PATH"
    exit 0
fi
R="$(make_repo dirty)"
write_fixture_tier "$R" "printf 'dependencies = [\"leviculum-core\"]\\\\n' >> $R/Cargo.lock"
OUT="$(cd "$R" && just fixture-tier 2>&1)"
RC=$?
expect_eq "$RC" 1 "the tier goes red when a step rewrote a tracked file"
grep -q "TREE_DIRTIED path=Cargo.lock" <<< "$OUT"
assert $? "the finding names the file, not just the fact"
grep -q "TREE_CLEAN_CHECK status=RED .*dirtied=1" <<< "$OUT"
assert $? "the verdict line counts it"
grep -q "refuses the NEXT gate" <<< "$OUT"
assert $? "the message says what this breaks next"
[ "$FAILED" = 0 ] || echo "$OUT" >&2

echo "=== case 2: the same tier, nothing written, is green ==="
R="$(make_repo clean)"
write_fixture_tier "$R" "true"
OUT="$(cd "$R" && just fixture-tier 2>&1)"
RC=$?
expect_eq "$RC" 0 "a tier that writes nothing passes"
grep -q "TREE_CLEAN_CHECK status=GREEN" <<< "$OUT"
assert $? "the verdict line says GREEN"

echo "=== case 3: work the author had not committed is not a finding ==="
R="$(make_repo predirty)"
echo 'fn main() { /* mid-edit */ }' > "$R/main.rs"
write_fixture_tier "$R" "true"
OUT="$(cd "$R" && just fixture-tier 2>&1)"
RC=$?
expect_eq "$RC" 0 "a dirty tree the tier did not touch stays green"
grep -q "baseline_entries=1" <<< "$OUT"
assert $? "the baseline recorded the author's edit"

echo "=== case 4: a file dirty BEFORE and written again is still caught ==="
R="$(make_repo dirtier)"
printf '# generated\nversion = 4\n# hand edit\n' > "$R/Cargo.lock"
write_fixture_tier "$R" "printf 'dependencies = []\\\\n' >> $R/Cargo.lock"
OUT="$(cd "$R" && just fixture-tier 2>&1)"
RC=$?
expect_eq "$RC" 1 "the status code was unchanged; the content hash caught it"
grep -q "TREE_DIRTIED path=Cargo.lock" <<< "$OUT"
assert $? "the second edit names the same file"
grep -q "TREE_UNDIRTIED path=Cargo.lock" <<< "$OUT"
assert $? "and reports the baseline entry it replaced"

echo "=== case 5: with no snapshot the baseline is 'clean', and says so ==="
R="$(make_repo nosnapshot)"
printf 'dependencies = []\n' >> "$R/Cargo.lock"
OUT="$(bash "$CHECK" --verify --repo "$R" --state "$R/absent.txt" 2>&1)"
RC=$?
expect_eq "$RC" 1 "a dirty tree with no baseline is red"
grep -q "TREE_CLEAN_BASELINE source=clean" <<< "$OUT"
assert $? "the baseline it used is named, not implied"

echo "=== case 6: a baseline from before a commit is not trusted ==="
R="$(make_repo stale)"
printf 'dependencies = []\n' >> "$R/Cargo.lock"
bash "$CHECK" --snapshot --repo "$R" --state "$R/snapshot.txt" >/dev/null
git -C "$R" commit --quiet -am "the author committed the edit"
OUT="$(bash "$CHECK" --verify --repo "$R" --state "$R/snapshot.txt" 2>&1)"
RC=$?
expect_eq "$RC" 0 "the committed tree is clean, not 'missing' the baseline's entry"
grep -q "TREE_BASELINE_STALE" <<< "$OUT"
assert $? "the discarded baseline is reported rather than silently dropped"
LEFTOVER=0; [ -f "$R/snapshot.txt" ] || LEFTOVER=1
expect_eq "$LEFTOVER" 1 "the snapshot is consumed, so it cannot be believed twice"

echo "=== case 7: a check that cannot run is exit 2 ==="
OUT="$(bash "$CHECK" --verify --repo "$WORK" 2>&1)"
RC=$?
expect_eq "$RC" 2 "exit 2 outside a git checkout"
grep -q "not a git checkout" <<< "$OUT"
assert $? "and it says why"
OUT="$(bash "$CHECK" 2>&1)"
RC=$?
expect_eq "$RC" 2 "exit 2 with no mode given"

echo "=== the real checkout: snapshot and verify with nothing in between ==="
# Green on HEAD, whatever state the author's tree is in -- the property the
# push path depends on.
OUT="$(bash "$CHECK" --snapshot --state "$WORK/real.txt" --repo "$REPO" 2>&1
       bash "$CHECK" --verify --state "$WORK/real.txt" --repo "$REPO" 2>&1)"
RC=$?
expect_eq "$RC" 0 "the real checkout is green across a tier that ran nothing"
grep -q "TREE_CLEAN_CHECK status=GREEN baseline=snapshot" <<< "$OUT"
assert $? "against its own snapshot, not against 'clean'"

echo
if [ "$FAILED" = 0 ]; then
    echo "test-tree-clean: ALL CASES PASS"
else
    echo "test-tree-clean: FAILURES ABOVE" >&2
fi
exit "$FAILED"
