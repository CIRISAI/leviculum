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
# A prerequisite written `command:<name>` is a program looked up on PATH
# rather than a path in the tree (1256: javac). Format and reasons:
# scripts/integ-prerequisites.txt.
integ_unmet_prerequisites() {
    local path target test
    while read -r path target test; do
        case "$path" in
            '' | '#'*) continue ;;
            command:*) command -v "${path#command:}" >/dev/null 2>&1 && continue ;;
            *) [ -e "$path" ] && continue ;;
        esac
        printf '%s\t%s\t%s\n' "$target" "${test:--}" "$path"
    done <"$INTEG_PREREQUISITES"
}

# The cargo invocations that run the targets in $2 (`<dir>/<name>` lines, a
# subset of $1, every target in the tree) and no other target, one line each:
# `-<TAB><name>` for a name to pass to the workspace-wide run, and
# `<dir><TAB><name>` for a target to run from its own package.
#
# `cargo test --workspace --test <name>` selects EVERY package's target of that
# name. Two packages may share a name (lnomad and loquax both have
# `scene_golden` since 1255), and when one of them is left out, by a
# prerequisite or because a recipe with its features runs it, the name cannot
# go to the workspace run without taking it along: that one goes to `-p` of
# the package the other lives in. A name all of whose targets are selected
# stays in the workspace run (two `cli` targets, one flag). Before 1306 the
# forge refused the shared name outright, and the land gate on ee9541af went
# red on it.
integ_run_groups() {
    local all="$1" selected="$2" target name dir left
    while read -r target; do
        [ -n "$target" ] || continue
        name="${target##*/}"
        dir="${target%/*}"
        # Two steps and no `grep | grep -q`: under a caller's `pipefail` the
        # first grep can die of SIGPIPE when -q exits early (#843).
        left="$(grep "/${name}\$" <<<"$all" | grep -vxF -f <(printf '%s\n' "$selected") || true)"
        if [ -n "$left" ]; then
            printf '%s\t%s\n' "$dir" "$name"
        else
            printf -- '-\t%s\n' "$name"
        fi
    done <<<"$selected" | sort -u
}

# The package name of the workspace member in directory $1: the first `name`
# of its manifest's `[package]` table, which is what `cargo test -p` takes.
integ_package() {
    awk '
        /^\[/ { in_pkg = ($0 == "[package]"); next }
        in_pkg && /^name[[:space:]]*=/ {
            sub(/^name[[:space:]]*=[[:space:]]*"/, ""); sub(/".*/, ""); print; exit
        }
    ' "$1/Cargo.toml"
}
