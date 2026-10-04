#!/bin/bash
# Every integration-test target `just standard` does not run by a line of its
# own (#408).
#
# `standard` used to name its integration suites one at a time, and a suite
# nobody named ran in no tier below the nightly: on 2026-10-04 lnmsg's
# python_interop was red on master for eight commits, the land gate's
# `just standard` passed the red through twice, and only `just complete` on
# origin/master saw it (401). That is the shape #194 describes, one tier up.
#
# So the selection is computed, the way scripts/ci-gate-integ.sh computes the
# forge's: every target in the tree (scripts/integ-targets.sh), less the ones
# `standard` already runs by name (scripts/standard-integ-elsewhere.txt), less
# the targets and tests whose run-time prerequisite is absent on this host
# (scripts/integ-prerequisites.txt), each printed with the path it lacks. On
# the land host the `reference/` submodules are checked out and nothing is
# skipped. scripts/check-standard-integ.py, in `just guards`, holds the
# declarations to the Justfile and the enumeration to `cargo metadata`.
#
# COST, priced from the per-binary runtimes of the 2026-10-04 nightly's
# `cargo test --workspace --all-targets` on schneckenschreck: the 92 targets
# this selects on the land host hold 189 s of test time, 62 s of it
# leviculum-lxmf-node/peering_loopback, 31 s leviculum-std/discovery_autoconnect,
# 16 s leviculum-lxmf-node/deferred_resource_builds, 11 s each
# leviculum-cli/selftest_ratchet_message_count and lnmsg/python_interop,
# 10 s leviculum-cli/client_tools, and under 9 s each for the other 86.
# Cargo runs the binaries one after another, so that is wall time, plus
# linking the test binaries on the first run after a change.
#
# `--bins` rides along for the unit tests inside bin targets, which `--lib`
# never selects (0.1 s). `--no-fail-fast` so a red run reports every binary,
# not a prefix. Through run-with-manifest.py, like every test gate.
#
# Exit 0 = everything selected passed. Exit 1 = a test failed, or a
# declaration names a target the tree does not have.
#
# Usage:
#   bash scripts/standard-integ.sh
#   bash scripts/standard-integ.sh --list   # print the selected targets, run nothing
set -uo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_DIR" || exit 1

# shellcheck source=scripts/integ-targets.sh
. scripts/integ-targets.sh

ELSEWHERE_FILE="scripts/standard-integ-elsewhere.txt"

say() { echo "[standard-integ] $*" >&2; }

die() {
    echo "" >&2
    echo "standard-integ: FAILED — $*" >&2
    exit 1
}

ALL="$(integ_enumerate | sort)"
[ -n "$ALL" ] || die "the enumeration found no integration-test target at all."

# Targets another line of `standard` runs: `<dir>/<name>`, or `<dir>/*`.
ELSEWHERE="$(sed -e '/^[[:space:]]*#/d' -e '/^[[:space:]]*$/d' "$ELSEWHERE_FILE" | awk '{ print $1 }')"
UNMET="$(integ_unmet_prerequisites)"

SELECTED=""
while read -r target; do
    [ -n "$target" ] || continue
    covered=""
    while read -r claim; do
        [ -n "$claim" ] || continue
        if [ "$claim" = "$target" ] || [ "$claim" = "${target%/*}/*" ]; then
            covered=1
            break
        fi
    done <<EOF
$ELSEWHERE
EOF
    [ -z "$covered" ] || continue
    missing="$(printf '%s\n' "$UNMET" | awk -F'\t' -v t="$target" '$1 == t && $2 == "-" { print $3 }' | paste -sd' ')"
    if [ -n "$missing" ]; then
        say "skipped: $target ($missing absent, $INTEG_PREREQUISITES)"
        continue
    fi
    SELECTED="$SELECTED$target
"
done <<EOF
$ALL
EOF
SELECTED="$(printf '%s' "$SELECTED" | sed '/^$/d')"

if [ "${1:-}" = "--list" ]; then
    printf '%s\n' "$SELECTED"
    exit 0
fi

[ -n "$SELECTED" ] || die "nothing is selected; a gate that measures nothing reads green forever."

ARGS=()
while read -r name; do
    [ -n "$name" ] || continue
    ARGS+=(--test "$name")
done <<EOF
$(printf '%s\n' "$SELECTED" | sed 's|.*/||' | sort -u)
EOF

# Single tests whose prerequisite is absent, inside a target this run selects.
# `--exact` below makes each `--skip` a whole test name.
SKIP_ARGS=()
while IFS=$'\t' read -r target test path; do
    { [ -n "$target" ] && [ "$test" != "-" ]; } || continue
    printf '%s\n' "$SELECTED" | grep -qxF "$target" || continue
    say "skipped test: $target $test ($path absent, $INTEG_PREREQUISITES)"
    SKIP_ARGS+=(--skip "$test")
done <<EOF
$UNMET
EOF

say "$(printf '%s\n' "$ALL" | wc -l) integration-test targets in the tree, $(printf '%s\n' "$SELECTED" | wc -l) selected here"
python3 scripts/run-with-manifest.py --gate standard-integ -- \
    cargo test --workspace --bins "${ARGS[@]}" --no-fail-fast -- --exact "${SKIP_ARGS[@]}"
