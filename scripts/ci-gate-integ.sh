#!/bin/bash
# The rest of the workspace's tests, which the forge gate never ran (#312).
#
# `just ci-gate` is the recipe BOTH forge pipelines run — `.woodpecker/ci.yml`
# on every push and pull request (Codeberg #299) and `.woodpecker/nightly.yml`
# before it builds anything it publishes (#266) — and until this script it ran
# `cargo test --workspace --lib` and nothing else. `--lib` selects one target
# per crate, so every target under `*/tests/**` gated NOTHING there, and
# neither did the unit tests that live inside bin targets, nor the doctests.
#
# MEASURED on the coder host (10 cores, warm target dir) with `cargo test
# --workspace`, 2026-09-26: 596 s wall, green, 5916 tests passed. What the
# gate ran of those: 4279, in 16 lib targets, 45.5 s. What it did not, and
# this script adds:
#
#   134 integration-test targets   1370 tests   461.3 s
#    23 bin unittest targets        234 tests     0.1 s
#    16 doctest units                33 tests     1.1 s
#
# Less the exclusions below (359 tests, 182 s of it the interop suite),
# that is 1278 tests the push path had no opinion on, on a gate whose whole
# point is to have one before a stranger's commit lands.
#
# THE EXCLUSIONS, ONE REASON BETWEEN THEM: three targets (rnsd_interop,
# reference_lock, python_interop) and seven tests inside the mvr target read a
# `reference/` submodule at run time, and both pipelines clone with
# `submodules: false` on purpose — a plain clone must build (#300) and
# github.com stays out of the release path's dependency set. They do not skip
# when it is absent, they FAIL, which is deliberate (see
# scripts/check-plain-clone.sh's header) and is exactly why they cannot be
# left in. Which ones, and why each, is declared once in
# scripts/integ-prerequisites.txt, which `just standard` reads too; this
# script excludes exactly the entries whose prerequisite is absent HERE, so on
# a host that has the submodules it runs them. The interop suite is a third
# of the integration runtime (353 tests, ~190 s).
#
# Each excluded test name is checked against the target's own `--list` before
# the run, so a renamed test is a red gate naming the line, never a skip that
# matches nothing while the line's reason outlives the test it described.
#
# WHAT COVERS THEM INSTEAD, because "nothing" would be the #312 bug one layer
# down: the tier-2 nightly runs `just complete` — `cargo test --workspace
# --all-targets` with the submodules present — `scripts/nightly-green-ref.sh`
# signs that commit only if the rnsd_interop unit ran and every test in it
# passed, and `scripts/check-nightly-green.sh` refuses to publish a commit no
# such signature covers. The interop suites therefore gate a RELEASE and not a
# push. Making them gate the push as well means fetching a submodule into
# every push pipeline, which spends the property #300 bought to re-derive a
# verdict the nightly already imports.
#
# THE LIST OF WHAT RUNS IS COMPUTED, NOT WRITTEN DOWN. A second copy of the
# workspace's test inventory in this file is a list that goes stale the first
# week somebody adds a suite, and it goes stale SILENTLY — the gate stays
# green while the new suite runs nowhere, which is #312's own shape. So the
# targets are enumerated from the tree by cargo's own autodiscovery rule
# (`tests/*.rs`, and `tests/<dir>/main.rs` as target `<dir>`), and only the
# prerequisites are written. The failure direction is therefore loud: a new
# suite that needs a submodule turns this gate red on the push that adds it,
# and the remedy is one line in scripts/integ-prerequisites.txt with its reason.
#
# Exit 0 = everything selected passed. Exit 1 = a test failed, or the
# enumeration and the prerequisite file disagree.
#
# Usage:
#   bash scripts/ci-gate-integ.sh
#   bash scripts/ci-gate-integ.sh --list   # print the selection, run nothing
set -uo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_DIR" || exit 1

# shellcheck source=scripts/integ-targets.sh
. scripts/integ-targets.sh

# The prerequisites this host lacks, split into whole targets and single tests
# inside a target. Reasons: scripts/integ-prerequisites.txt.
UNMET="$(integ_unmet_prerequisites)"
EXCLUDED="$(printf '%s\n' "$UNMET" | awk -F'\t' '$2 == "-" { print $1 }' | sort -u)"
EXCLUDED_TESTS="$(printf '%s\n' "$UNMET" | awk -F'\t' '$2 != "-" { print $1 " " $2 }' | sort -u)"

say() { echo "[ci-gate-integ] $*" >&2; }

die() {
    echo "" >&2
    echo "ci-gate-integ: FAILED — $*" >&2
    echo "" >&2
    echo "The list of test targets is computed from the tree and only the" >&2
    echo "prerequisites are written down (scripts/integ-prerequisites.txt)." >&2
    echo "Fix whichever of the two moved; do not silence this by dropping a" >&2
    echo "target from the run." >&2
    exit 1
}

ALL="$(integ_enumerate | sort)"
[ -n "$ALL" ] || die "the enumeration found no integration-test target at all.
A gate that measures nothing reads green forever, so this is a refusal
rather than an empty run: either the repo root moved or 'members = [...]'
in Cargo.toml is no longer parsable by the sed in scripts/integ-targets.sh."

EXCLUDED_LIST="$(printf '%s\n' "$EXCLUDED" | sed '/^[[:space:]]*$/d')"

# Both directions of drift. A stale exclusion is not cosmetic: the entry is
# what carries the reason a suite is out of the push gate, so an entry naming
# a target that no longer exists means the reason is describing nothing.
while read -r entry; do
    [ -n "$entry" ] || continue
    printf '%s\n' "$ALL" | grep -qxF "$entry" ||
        die "scripts/integ-prerequisites.txt names '$entry', which is not a test target in this tree.
Either the target was renamed or removed — drop the line — or the path is
wrong, in which case a suite everyone believes is excluded is being run."
    # Selection below is BY NAME (`cargo test --test <name>`), which is
    # workspace-wide: excluding a name that a second package also uses would
    # silently take that one out too.
    name="${entry##*/}"
    dupes="$(printf '%s\n' "$ALL" | grep -c "/${name}\$")"
    [ "$dupes" -eq 1 ] ||
        die "the excluded target name '$name' exists in $dupes packages.
'cargo test --test $name' selects all of them, so excluding one would drop
the others without a word. Give the surviving one a different name, or run
it from a second invocation here."
done <<EOF
$EXCLUDED_LIST
EOF

SELECTED="$(printf '%s\n' "$ALL" | grep -vxF -f <(printf '%s\n' "$EXCLUDED_LIST"))"
# Names, deduplicated: two packages may legitimately both have a `cli` target,
# and `--test cli` selects both, which is what we want.
NAMES="$(printf '%s\n' "$SELECTED" | sed 's|.*/||' | sort -u)"

say "$(printf '%s\n' "$ALL" | wc -l) integration-test targets in the tree"
say "$(printf '%s\n' "$SELECTED" | wc -l) selected, via $(printf '%s\n' "$NAMES" | wc -l) target name(s)"
while read -r entry; do
    [ -n "$entry" ] || continue
    say "excluded: $entry ($(printf '%s\n' "$UNMET" | awk -F'\t' -v t="$entry" '$1 == t && $2 == "-" { print $3 }' | paste -sd' ') absent, scripts/integ-prerequisites.txt)"
done <<EOF
$EXCLUDED_LIST
EOF

ARGS=()
while read -r name; do
    [ -n "$name" ] || continue
    ARGS+=(--test "$name")
done <<EOF
$NAMES
EOF

EXCLUDED_TESTS_LIST="$(printf '%s\n' "$EXCLUDED_TESTS" | sed '/^[[:space:]]*$/d')"

if [ "${1:-}" = "--list" ]; then
    printf '%s\n' "$NAMES"
    exit 0
fi

# Every excluded test must be a test of a target this run selects, by its
# exact name in that target's own listing. Same two directions as EXCLUDED: a
# line naming nothing is a reason describing nothing, and a skip that silently
# stopped matching puts the test back on a gate that cannot run it.
SKIP_ARGS=()
declare -A LISTING=()
while read -r target test; do
    [ -n "$target" ] || continue
    printf '%s\n' "$SELECTED" | grep -qxF "$target" ||
        die "scripts/integ-prerequisites.txt names '$test' in '$target', which is not a selected
test target in this tree. Fix the target path, or drop the line if the
target itself went onto EXCLUDED."
    name="${target##*/}"
    if [ -z "${LISTING[$name]+set}" ]; then
        say "listing target $name to check its excluded tests"
        # Same `--workspace` as the run below, so the listing compiles what
        # the run reuses rather than a second feature resolution.
        LISTING[$name]="$(cargo test --workspace --test "$name" -- --list)" ||
            die "could not list the tests of target '$name'."
    fi
    printf '%s\n' "${LISTING[$name]}" | grep -qxF "$test: test" ||
        die "scripts/integ-prerequisites.txt names '$test', which target '$name' no longer has.
It was renamed or removed: follow the rename, or drop the line."
    say "excluded test: $target $test (a prerequisite is absent, scripts/integ-prerequisites.txt)"
    SKIP_ARGS+=(--skip "$test")
done <<EOF
$EXCLUDED_TESTS_LIST
EOF

# `--bins` rides along: those 23 targets hold 234 unit tests that `--lib`
# never selected either, 169 of them in leviculum-cli's seven client
# binaries, and they cost 0.1 s.
#
# `--no-fail-fast` for the reason the `complete` recipe gives: without it
# cargo stops after the first red binary and every later one goes unrun, so a
# red gate would report a prefix of the suite while looking like all of it.
#
# Through run-with-manifest.py, like the `--lib` line in the recipe above this
# one: the manifest is what makes "the gate was green" unable to mean "the
# suite never ran" (Guarantee B), and it is also what keeps the full output of
# a red run on disk instead of in a scrollback.
# `--exact` makes each `--skip` a whole test name rather than a substring, so
# an excluded name cannot take a longer one in another binary with it.
say "running cargo test --workspace --bins with ${#ARGS[@]} target flags, skipping $((${#SKIP_ARGS[@]} / 2)) test(s)"
python3 scripts/run-with-manifest.py --gate ci-gate-integ -- \
    cargo test --workspace --bins "${ARGS[@]}" --no-fail-fast -- --exact "${SKIP_ARGS[@]}"
rc=$?

# Doctests are their own invocation because cargo DROPS them when any other
# target selector is given — the same reason `complete` spells them out
# separately. 33 tests, 1.1 s.
python3 scripts/run-with-manifest.py --gate ci-gate-doc -- \
    cargo test --workspace --doc --no-fail-fast
doc_rc=$?

[ "$rc" -eq 0 ] || exit "$rc"
exit "$doc_rc"
