#!/bin/bash
# Run a recipe's own `cargo test` line with the tests whose run-time
# prerequisite is absent HERE left out, by the declaration
# scripts/integ-prerequisites.txt holds for every runner (1167).
#
# scripts/ci-gate-integ.sh and scripts/standard-integ.sh compute their
# selection and read that file. A target with `required-features` is not
# theirs: it runs in the recipe that passes the feature (`loquax-tui` for
# loquax/home_node), and that recipe named its tests outright. So four
# home_node tests that start a stock Python LXMF peer through
# loquax/tests/support/python_peer.rs ran on the forge, which clones without
# the reference submodules, and failed there with "the Python daemon exited
# before saying READY" where every other such test is excluded by name.
# This wrapper is the same rule for those recipes: it reads the same file
# through scripts/integ-targets.sh, prints the same named reason for each
# test it leaves out, and on a host with the submodules runs the command
# untouched. scripts/check-standard-integ.py holds every gated target with a
# prerequisite to a recipe that runs through it.
#
# Only single tests are left out. A target whose whole-target prerequisite is
# absent is a refusal: the `--test` flags are the recipe's, and dropping one
# here would leave the recipe's line claiming a target it no longer runs.
#
# Each name left out is first checked against the command's own listing
# (`-- --list`, which compiles what the run then reuses), so a renamed test is
# a refusal naming the line rather than a `--skip` that matches nothing. The
# listing goes to a file and not to the output: under run-with-manifest.py a
# listed unit with no `test result:` line reads as a run that measured
# nothing.
#
# Usage:
#   bash scripts/with-prerequisites.sh <dir>/<target>... -- cargo test ...
# The command must not carry libtest arguments of its own (no `--` in it):
# `--exact` is added, which would change what a filter of the recipe's means.
set -uo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_DIR" || exit 1

# shellcheck source=scripts/integ-targets.sh
. scripts/integ-targets.sh

say() { echo "[with-prerequisites] $*" >&2; }
die() {
    echo "with-prerequisites: FAILED: $*" >&2
    exit 2
}
has_line() { grep -qxF -- "$1" <<<"$2"; }

TARGETS=()
while [ $# -gt 0 ] && [ "$1" != "--" ]; do
    TARGETS+=("$1")
    shift
done
[ "${1:-}" = "--" ] || die "usage: $0 <dir>/<target>... -- <cargo test command>"
shift
[ $# -gt 0 ] || die "no command after --"
[ "${#TARGETS[@]}" -gt 0 ] || die "no target named before --"
CMD=("$@")
for arg in "${CMD[@]}"; do
    [ "$arg" != "--" ] || die "the command carries libtest arguments (a \`--\`); this wrapper adds \`--exact\`, which would change what they mean"
done

ALL="$(integ_enumerate)"
UNMET="$(integ_unmet_prerequisites)"
SKIPS=()
for target in "${TARGETS[@]}"; do
    has_line "$target" "$ALL" || die "'$target' is not a test target in this tree"
    whole="$(awk -F'\t' -v t="$target" '$1 == t && $2 == "-" { print $3 }' <<<"$UNMET" | paste -sd' ')"
    [ -z "$whole" ] || die "$target needs $whole for the whole target, absent here ($INTEG_PREREQUISITES).
This wrapper leaves out single tests only: declare the tests by name, or take
the target out of the recipe's line on hosts without it."
    while IFS=$'\t' read -r t test path; do
        { [ "$t" = "$target" ] && [ "$test" != "-" ]; } || continue
        say "skipped test: $target $test ($path absent, $INTEG_PREREQUISITES)"
        SKIPS+=("$test")
    done <<<"$UNMET"
done

[ "${#SKIPS[@]}" -gt 0 ] || exec "${CMD[@]}"

LISTING="$(mktemp)"
trap 'rm -f "$LISTING"' EXIT
"${CMD[@]}" -- --list >"$LISTING" 2>&1 ||
    die "could not list the command's tests (exit $?):
$(sed 's/^/  /' "$LISTING")"
for test in "${SKIPS[@]}"; do
    has_line "$test: test" "$(cat "$LISTING")" ||
        die "$INTEG_PREREQUISITES names '$test', which no binary of this command lists.
It was renamed or removed: follow the rename, or drop the line."
done

ARGS=(--exact)
for test in "${SKIPS[@]}"; do
    ARGS+=(--skip "$test")
done
"${CMD[@]}" -- "${ARGS[@]}"
