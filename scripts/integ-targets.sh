# shellcheck shell=bash
# Sourced, not run: the workspace's integration-test targets and their
# run-time prerequisites, for every runner that selects tests from the tree
# (scripts/ci-gate-integ.sh, scripts/standard-integ.sh). One copy, so the two
# gates cannot disagree about what a target is or what it needs.
#
# Expects the caller to have cd'd to the repository root.

# The workspace members, as paths, out of the root manifest's `members = [...]`
# array. Read rather than asked of `cargo metadata` because this also runs in
# the forge container, where jq is absent and the answer is one sed away.
# scripts/check-standard-integ.py compares the result against `cargo metadata`.
integ_members() {
    sed -n '/^members = \[/,/^]/p' Cargo.toml |
        sed -n 's/^[[:space:]]*"\([^"]*\)".*/\1/p'
}

# Every integration-test target in the workspace as `<dir>/<name>`, by cargo's
# own autodiscovery rule for `tests/`: each `*.rs` file is a target, and each
# subdirectory holding a `main.rs` is a target named after the directory (that
# is how `tests/mvr/main.rs` becomes the target `mvr`). No member sets
# `autotests = false` or declares a `[[test]]` elsewhere, which is what makes
# the rule complete here; scripts/check-standard-integ.py holds it to that.
integ_enumerate() {
    local m f d
    while read -r m; do
        [ -n "$m" ] || continue
        [ -d "$m/tests" ] || continue
        for f in "$m"/tests/*.rs; do
            [ -f "$f" ] || continue
            echo "$m/$(basename "$f" .rs)"
        done
        for d in "$m"/tests/*/; do
            [ -f "${d}main.rs" ] || continue
            echo "$m/$(basename "$d")"
        done
    done <<EOF
$(integ_members)
EOF
}

INTEG_PREREQUISITES="scripts/integ-prerequisites.txt"

# One line per declared prerequisite that is ABSENT on this host:
# `<dir>/<target><TAB><exact test name, or ->><TAB><missing path>`.
# Format and reasons: scripts/integ-prerequisites.txt.
integ_unmet_prerequisites() {
    local path target test
    while read -r path target test; do
        case "$path" in '' | '#'*) continue ;; esac
        [ -e "$path" ] && continue
        printf '%s\t%s\t%s\n' "$target" "${test:--}" "$path"
    done <"$INTEG_PREREQUISITES"
}
