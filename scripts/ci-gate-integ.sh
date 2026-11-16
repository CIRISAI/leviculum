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
# A SECOND KIND OF EXCLUSION: a target that declares `required-features`.
# The run below is one `cargo test --workspace --test ...` with no
# `--features`, and cargo refuses the WHOLE invocation when one of its targets
# needs a feature it does not enable (forge pipeline 573, 2026-10-26: red on
# loquax/tui_snapshot since 994 added it). scripts/standard-integ.sh had the
# same wall and the same remedy: such a target is run by the recipe that
# passes the feature, named in scripts/standard-integ-elsewhere.txt. The rule
# is read from `cargo metadata` by scripts/check-standard-integ.py `--gated`,
# the parser the standard gate already trusts, and a gated target with no
# recipe is a refusal here rather than a target no gate runs. `ci-gate` runs
# those recipes by name after this script, and check-standard-integ holds it
# to that.
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
#   bash scripts/ci-gate-integ.sh --list      # print the selection, run nothing
#   bash scripts/ci-gate-integ.sh --dry-run   # every check, the excluded tests'
#                                             # listings included, then print the
#                                             # cargo invocations instead of them
set -uo pipefail

DRY_RUN=0
[ "${1:-}" != "--dry-run" ] || DRY_RUN=1

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

# Is the line $1 in the text $2? A here-string and never `printf | grep -q`:
# under `pipefail` that pipeline is RED whenever grep matches and exits before
# printf has finished writing, because printf then dies of SIGPIPE (141) and
# pipefail reports it. On a loaded runner that happens: forge pipeline 536 went
# red on f78ce9a6 claiming target mvr "no longer has" a test that 537 found in
# the same listing an hour later (#843). Measured on the coder host under
# twenty busy loops: the pipe form 3 red of 10500 checks, this form 0.
has_line() { grep -qxF -- "$1" <<<"$2"; }

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

# `<dir>/<target><TAB><features><TAB><recipe or ->`, one per target with
# required features.
GATED="$(python3 scripts/check-standard-integ.py --gated)" ||
    die "could not read the targets with required-features from cargo metadata
(python3 scripts/check-standard-integ.py --gated)."
GATED_LIST="$(printf '%s\n' "$GATED" | awk -F'\t' 'NF { print $1 }')"
while IFS=$'\t' read -r target features recipe; do
    [ -n "$target" ] || continue
    [ "$recipe" != "-" ] ||
        die "$target requires the feature(s) $features, which this run does not enable,
and scripts/standard-integ-elsewhere.txt names no recipe that runs it. Claim
it there from a recipe that passes the feature, and run that recipe in
'ci-gate', or no gate tests it."
done <<EOF
$GATED
EOF

# `<dir>/<target><TAB><recipe>`, one per gated target whose recipe runs it
# through scripts/with-prerequisites.sh naming it. A by-name line of
# scripts/integ-prerequisites.txt for such a target is not stale although
# this run excludes the target: the recipe applies the line (1203). Forge
# land gate on e6dd4eaf: red on the four loquax/home_node lines 1167 added,
# which the coder host never saw because its submodules exclude nothing.
WRAPPED="$(python3 scripts/check-standard-integ.py --wrapped)" ||
    die "could not read which gated targets run through scripts/with-prerequisites.sh
(python3 scripts/check-standard-integ.py --wrapped)."

# Both directions of drift. A stale exclusion is not cosmetic: the entry is
# what carries the reason a suite is out of the push gate, so an entry naming
# a target that no longer exists means the reason is describing nothing.
#
# An entry is `<dir>/<target>`. A bare `<target>` that a second package also
# has is refused by name: it does not say which one it means, and
# `cargo test --test <name>` would take every one of them. A shared name in a
# QUALIFIED entry is fine: the run below gives the surviving target its own
# package's invocation (1306; until then the forge refused the qualified
# `loquax/scene_golden` once lnomad had a `scene_golden` too, land gate on
# ee9541af).
while read -r entry; do
    [ -n "$entry" ] || continue
    if [ "${entry#*/}" = "$entry" ]; then
        dupes="$(grep -c "/${entry}\$" <<<"$ALL")"
        [ "$dupes" -le 1 ] ||
            die "scripts/integ-prerequisites.txt names the bare target '$entry', which exists in $dupes packages:
$(grep "/${entry}\$" <<<"$ALL" | sed 's/^/  /')
'cargo test --test $entry' selects all of them, so excluding by that name
would drop the others without a word. Qualify the line as <dir>/$entry."
    fi
    has_line "$entry" "$ALL" ||
        die "scripts/integ-prerequisites.txt names '$entry', which is not a test target in this tree.
Either the target was renamed or removed — drop the line — or the path is
wrong, in which case a suite everyone believes is excluded is being run.
A line names its target as <dir>/<target>."
done <<EOF
$EXCLUDED_LIST
$GATED_LIST
$(awk '{ print $1 }' <<<"$EXCLUDED_TESTS" | sort -u)
EOF

SELECTED="$(printf '%s\n' "$ALL" | grep -vxF -f <(printf '%s\n' "$EXCLUDED_LIST" "$GATED_LIST" | sed '/^$/d'))"
# `-<TAB><name>` for the workspace run, `<dir><TAB><name>` for a selected
# target whose name an excluded one shares (scripts/integ-targets.sh).
RUN_GROUPS="$(integ_run_groups "$ALL" "$SELECTED")"
# Names, deduplicated: two packages may legitimately both have a `cli` target,
# and `--test cli` selects both, which is what we want.
NAMES="$(awk -F'\t' '$1 == "-" { print $2 }' <<<"$RUN_GROUPS")"
# `<dir>/<name>`, each run from its own package after the workspace run.
SPLIT="$(awk -F'\t' '$1 != "-" { print $1 "/" $2 }' <<<"$RUN_GROUPS")"

say "$(printf '%s\n' "$ALL" | wc -l) integration-test targets in the tree"
say "$(printf '%s\n' "$SELECTED" | wc -l) selected, via $(printf '%s\n' "$NAMES" | sed '/^$/d' | wc -l) target name(s) and $(printf '%s\n' "$SPLIT" | sed '/^$/d' | wc -l) per-package target(s)"
while read -r target; do
    [ -n "$target" ] || continue
    say "per package: $target (an excluded target of another package has its name)"
done <<EOF
$SPLIT
EOF
while read -r entry; do
    [ -n "$entry" ] || continue
    say "excluded: $entry ($(printf '%s\n' "$UNMET" | awk -F'\t' -v t="$entry" '$1 == t && $2 == "-" { print $3 }' | paste -sd' ') absent, scripts/integ-prerequisites.txt)"
done <<EOF
$EXCLUDED_LIST
EOF
while IFS=$'\t' read -r target features recipe; do
    [ -n "$target" ] || continue
    say "excluded: $target (requires features: $features; run by recipe $recipe)"
done <<EOF
$GATED
EOF

ARGS=()
while read -r name; do
    [ -n "$name" ] || continue
    ARGS+=(--test "$name")
done <<EOF
$NAMES
EOF

EXCLUDED_TESTS_LIST="$(printf '%s\n' "$EXCLUDED_TESTS" | sed '/^[[:space:]]*$/d')"

# One selector a line: a bare name for the workspace run, `<dir>/<name>` for
# a target run from its own package.
if [ "${1:-}" = "--list" ]; then
    printf '%s\n' "$NAMES" "$SPLIT" | sed '/^$/d'
    exit 0
fi

# Every excluded test must be a test of a target this run selects, by its
# exact name in that target's own listing. Same two directions as EXCLUDED: a
# line naming nothing is a reason describing nothing, and a skip that silently
# stopped matching puts the test back on a gate that cannot run it.
#
# A missing name is a red gate, so the listing that made it red is evidence
# and is kept: the failure names its line count, first and last lines and
# cargo's stderr, and writes the whole capture under the manifest directory
# (#195). Before calling a name gone, the target is listed ONCE more: a binary
# that lists itself twice with different results is its own finding, and the
# gate stops on it rather than trusting either listing.
LISTING_DIR="$(mktemp -d)"
trap 'rm -rf "$LISTING_DIR"' EXIT

# list_target NAME N TARGET: listing N of target NAME into
# $LISTING_DIR/NAME.N.{out,err}. The selector of the run below: `--workspace`
# for a name of the workspace run, so the listing compiles what the run reuses
# rather than a second feature resolution, and `-p` for a per-package target,
# so a same-named target of another package cannot answer for it.
list_target() {
    local sel=(--workspace)
    if has_line "$3" "$SPLIT"; then
        sel=(-p "$(integ_package "${3%/*}")")
    fi
    cargo test "${sel[@]}" --test "$1" -- --list \
        >"$LISTING_DIR/$1.$2.out" 2>"$LISTING_DIR/$1.$2.err"
}

# The number of tests a listing names (libtest's `<name>: test` lines).
listed_tests() { grep -c ': test$' "$1"; }

# A floor under which a listing is called truncated rather than believed: the
# test attributes in the target's sources, less a tenth for the cfg-gated and
# the macro-generated ones the grep cannot see either way.
test_floor() {
    local pkg="${1%/*}" name="${1##*/}" attrs
    attrs="$(grep -rhcE '^[[:space:]]*#\[(tokio::)?test\b' \
        "$pkg/tests/$name" "$pkg/tests/$name.rs" 2>/dev/null | awk '{ n += $1 } END { print n + 0 }')"
    echo $(( ${attrs:-0} * 9 / 10 ))
}

# Copy every capture of NAME to a directory the run leaves behind, and say it.
keep_listings() {
    local name="$1" dest
    dest="$(python3 - <<'PY' 2>/dev/null
import importlib.util
spec = importlib.util.spec_from_file_location("rwm", "scripts/run-with-manifest.py")
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
print(mod.manifest_dir())
PY
)"
    dest="${dest:-${XDG_STATE_HOME:-$HOME/.local/state}/leviculum-ci}/ci-gate-integ-listing-$name-$(date -u +%Y%m%dT%H%M%SZ)-$$"
    if mkdir -p "$dest" && cp "$LISTING_DIR/$name".* "$dest"/; then
        echo "$dest"
    else
        echo "(could not write $dest)"
    fi
}

# describe_listing NAME N TARGET: the facts of one capture, for a die message.
describe_listing() {
    local out="$LISTING_DIR/$1.$2.out" err="$LISTING_DIR/$1.$2.err"
    echo "listing $2: $(wc -l <"$out") lines, $(listed_tests "$out") tests, floor $(test_floor "$3")"
    echo "  first line: $(head -n 1 "$out")"
    echo "  last line:  $(tail -n 1 "$out")"
    if [ -s "$err" ]; then
        echo "  cargo stderr:"
        sed 's/^/    /' "$err"
    fi
}

# gate NAME CMD...: CMD through run-with-manifest.py as gate NAME, or under
# `--dry-run` the command it would run, quoted, and nothing else.
gate() {
    local name="$1"
    shift
    if [ "$DRY_RUN" = 1 ]; then
        printf '[ci-gate-integ] would run (gate %s):' "$name" >&2
        printf ' %q' "$@" >&2
        echo >&2
        return 0
    fi
    python3 scripts/run-with-manifest.py --gate "$name" -- "$@"
}

SKIP_ARGS=()
declare -A LISTING=()
while read -r target test; do
    [ -n "$target" ] || continue
    if ! has_line "$target" "$SELECTED"; then
        recipe="$(awk -F'\t' -v t="$target" '$1 == t { print $2 }' <<<"$WRAPPED")"
        if [ -n "$recipe" ]; then
            say "excluded test: $target $test (target runs in recipe $recipe, which applies scripts/integ-prerequisites.txt through scripts/with-prerequisites.sh)"
            continue
        fi
        recipe="$(awk -F'\t' -v t="$target" '$1 == t { print $3 }' <<<"$GATED")"
        [ -z "$recipe" ] ||
            die "scripts/integ-prerequisites.txt names '$test' in '$target', which is not a selected
test target in this tree. It requires features and runs in recipe '$recipe',
which does not run it through 'scripts/with-prerequisites.sh $target', so
nothing applies the line there. Route that recipe through the wrapper."
        die "scripts/integ-prerequisites.txt names '$test' in '$target', which is not a selected
test target in this tree. Fix the target path, or drop the line if the
target itself went onto EXCLUDED."
    fi
    name="${target##*/}"
    if [ -z "${LISTING[$target]+set}" ]; then
        say "listing target $name to check its excluded tests"
        list_target "$name" 1 "$target" ||
            die "could not list the tests of target '$name' (cargo exited $?).
$(describe_listing "$name" 1 "$target")
Kept: $(keep_listings "$name")"
        LISTING[$target]="$(cat "$LISTING_DIR/$name.1.out")"
    fi
    # Every branch of a miss ends in die, so a target is relisted at most once.
    if ! has_line "$test: test" "${LISTING[$target]}"; then
        say "target $name does not list '$test'; listing it once more before believing that"
        list_target "$name" 2 "$target"
        relist_rc=$?
        if [ "$relist_rc" -eq 0 ] && has_line "$test: test" "$(cat "$LISTING_DIR/$name.2.out")"; then
            die "the test binary of target '$name' listed itself twice with different results:
the first listing lacks '$test', the second has it.
$(describe_listing "$name" 1 "$target")
$(describe_listing "$name" 2 "$target")
diff of listing 1 against listing 2:
$(diff "$LISTING_DIR/$name.1.out" "$LISTING_DIR/$name.2.out" | sed 's/^/  /')
Kept: $(keep_listings "$name")"
        fi
        floor="$(test_floor "$target")"
        count="$(listed_tests "$LISTING_DIR/$name.1.out")"
        if [ "$count" -lt "$floor" ]; then
            verdict="the listing of target '$name' is truncated: it names $count tests where the
tree's test attributes put the floor at $floor, so its lack of '$test'
says nothing about the test. The second listing (exit $relist_rc) lacks it too."
        else
            verdict="scripts/integ-prerequisites.txt names '$test', which target '$name' no longer has
(two listings agree; the second exited $relist_rc).
It was renamed or removed: follow the rename, or drop the line."
        fi
        die "$verdict
$(describe_listing "$name" 1 "$target")
$(describe_listing "$name" 2 "$target")
Kept: $(keep_listings "$name")"
    fi
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
gate ci-gate-integ \
    cargo test --workspace --bins "${ARGS[@]}" --no-fail-fast -- --exact "${SKIP_ARGS[@]}"
rc=$?

# A target whose name an excluded target of another package shares, from its
# own package: the workspace run above cannot name it without taking the
# other along (1306). Same skips, one manifest per package. `-p` resolves
# features for that package alone, which may compile a few crates again.
while read -r target; do
    [ -n "$target" ] || continue
    pkg="$(integ_package "${target%/*}")"
    [ -n "$pkg" ] || die "no package name in ${target%/*}/Cargo.toml, for $target."
    say "running cargo test -p $pkg --test ${target##*/}"
    gate "ci-gate-integ-$pkg" \
        cargo test -p "$pkg" --test "${target##*/}" --no-fail-fast -- --exact "${SKIP_ARGS[@]}"
    split_rc=$?
    [ "$split_rc" -eq 0 ] || rc="$split_rc"
done <<EOF
$SPLIT
EOF

# Doctests are their own invocation because cargo DROPS them when any other
# target selector is given — the same reason `complete` spells them out
# separately. 33 tests, 1.1 s.
gate ci-gate-doc \
    cargo test --workspace --doc --no-fail-fast
doc_rc=$?

[ "$rc" -eq 0 ] || exit "$rc"
exit "$doc_rc"
