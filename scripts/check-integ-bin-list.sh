#!/bin/bash
# One list of integration binaries, and it lives in `build-integ-bins`.
#
# The Justfile recipe `build-integ-bins` names every binary periculum mounts
# into its node containers, and `periculum check-freshness` asserts each of
# them is newer than the last production-source change. Two ways that one
# list stops being one, and this file refuses both.
#
# A SECOND COPY. A caller that spells out its own `cargo build --bin ...`
# line instead of calling the recipe has made a second copy of that list, and
# the two drift silently: the recipe grew `--bin lxmf-node`,
# scripts/run-tier3-hw.sh did not, and from then on every hardware nightly
# that followed a source change died in the freshness preflight naming a
# binary nothing had built (2026-08-19).
#
# THE OTHER SIDE MOVING. The set the recipe builds and the set the preflight
# grades are two lists in two repositories, and only their being equal makes
# the preflight a preflight. When they diverge the run does not merely lose a
# check, it ABORTS: a binary periculum grades and nothing builds is "stale
# after forced rebuild", FATAL, whole nightly gone (Codeberg #229 — lnstatus
# was absent from the build set and survived in ~/.cache/leviculum-ci-target
# from other jobs, so the divergence stayed invisible until a disk cleanup
# emptied that cache). A binary the recipe builds that periculum grades for
# nothing is the harmless direction, but it is still drift and it is still
# reported: it means somebody's list moved and nobody said so.
#
# Scope is deliberately narrow — for the first part, two named files, not a
# tree sweep. Any other `cargo build --bin` in the tree (a developer
# convenience, a doc example) is nobody's second list, and a guard that
# shouts about those gets disabled.
#
# `cargo build --release` with no `--bin` is fine and appears in
# run-tier3-hw.sh: that one builds periculum itself, in periculum's own
# checkout, which has nothing to do with this list.
#
# In a gate rather than a `#[test]` for the reason the whole check-* family
# is: it reads files that no test binary compiles, and it must run on the
# push path where the author still has the file open.
#
# Exit 0 = one list, and it is the set periculum grades. Exit 1 = a banned
# line, a drifted set, or the checker's own self-test failed.
#
# Usage:
#   bash scripts/check-integ-bin-list.sh
set -uo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_DIR" || exit 1

# The callers that mount the integ binaries. Scoped by name: this is a ban on
# a specific duplication, not a style rule about cargo invocations.
GUARDED_FILES=(
    scripts/run-tier3-hw.sh
    .woodpecker/nightly.yml
)

# Where the graded set is read from, resolved exactly as run-tier3-hw.sh
# resolves it so this gate and the nightly cannot mean different checkouts.
PERICULUM_ROOT="${PERICULUM_ROOT:-$REPO_DIR/../periculum}"
# Normalised so the "read from" line in a failure names a path a reader can
# paste, not one with a `/../` in the middle of it.
[ -d "$PERICULUM_ROOT" ] && PERICULUM_ROOT="$(cd "$PERICULUM_ROOT" && pwd)"
PERICULUM_BIN="${PERICULUM_BIN:-$PERICULUM_ROOT/target/release/periculum}"
PERICULUM_SOURCE="$PERICULUM_ROOT/periculum/src/main.rs"

# Graded by the preflight but deliberately not in `build-integ-bins`: c-lnsd
# is a `cc` build of leviculum-ffi/examples/c/lnsd.c, not a cargo bin, so it
# has its own recipe (`build-c-lnsd`, which run-tier3-hw.sh calls right after
# `build-integ-bins`). The carve-out is checked below rather than trusted —
# an excluded name whose own recipe disappeared would be excluded from
# nothing.
NOT_CARGO_BINS=( c-lnsd )

# Print `file:line: text` for every banned line in $1, and nothing otherwise.
#
# Backslash continuations are joined first, so a `cargo build --release \`
# split across two lines is read as the one command it is; the reported line
# number is where the command starts. Comment lines are skipped after the
# join — a commented-out cargo line builds nothing. The leading `- ` of a
# YAML sequence entry is not a comment marker and is stripped before the
# comment test.
banned_lines() {
    awk '
    {
        line = $0
        start = FNR
        while (line ~ /\\$/ && (getline nxt) > 0) {
            sub(/\\$/, "", line)
            line = line nxt
        }
        head = line
        sub(/^[ \t]*(-[ \t]+)?/, "", head)
        if (substr(head, 1, 1) == "#") next
        if (line ~ /cargo[ \t]+build/ && line ~ /--bins?([ \t=]|$)/) {
            sub(/^[ \t]+/, "", line)
            printf "%s:%d: %s\n", FILENAME, start, line
        }
    }
    ' "$1"
}

# Print, one per line and sorted, the binaries a recipe body on stdin builds.
# Both `--bin name` and `--bin=name` count; `--bins` names nothing in
# particular and is part 1's business, not this one's.
recipe_bin_names() {
    awk '
    {
        for (i = 1; i <= NF; i++) {
            if ($i == "--bin" && i < NF) {
                print $(i + 1)
            } else if ($i ~ /^--bin=/) {
                name = $i
                sub(/^--bin=/, "", name)
                print name
            }
        }
    }
    ' | sort -u
}

# Print the binaries periculum's preflight grades, from the binary's own
# `check-freshness --list` — the interface periculum offers for exactly this
# question (periculum #42), so the answer comes from the same constants the
# check iterates. Exit 1 without output when there is no usable binary; the
# caller falls back to the source.
#
# Only `required` and `optional` are read, which are the two kinds periculum
# prints. A third kind would not be read here — but a binary in it that the
# recipe builds then shows up as UNGRADED below, which names this file, so
# the blind spot reports itself instead of hiding.
graded_from_binary() {
    local bin="$1" out names
    [ -x "$bin" ] || return 1
    out="$("$bin" check-freshness --list 2>/dev/null)" || return 1
    names="$(awk '$1 == "required" || $1 == "optional" { print $2 }' <<<"$out" | sort -u)"
    [ -n "$names" ] || return 1
    printf '%s\n' "$names"
}

# Same list, read from the two const arrays in periculum's checked-out
# source, so a clone whose periculum has never been built still gates. Exit 1
# without output when either constant is not where this expects it: a checker
# that quietly compares against an empty list passes the tree forever, which
# is the failure this whole file exists to prevent.
graded_from_source() {
    local src="$1" names
    [ -f "$src" ] || return 1
    names="$(awk '
        /const[ \t]+PREFLIGHT_(REQUIRED|OPTIONAL)_BINARIES/ { seen++; grab = 1 }
        grab {
            n = split($0, parts, "\"")
            for (i = 2; i <= n; i += 2) print parts[i]
            if ($0 ~ /\];/) grab = 0
        }
        END { if (seen < 2) exit 1 }
    ' "$src")" || return 1
    [ -n "$names" ] || return 1
    printf '%s\n' "$names" | sort -u
}

# Drop the names that are graded but are nobody's cargo bin.
drop_not_cargo_bins() {
    grep -vxF "$(printf '%s\n' "${NOT_CARGO_BINS[@]}")" || true
}

# Compare the graded set ($1) with the built set ($2), both newline-
# separated. Prints one line per drifted binary, naming the direction and the
# edit that fixes it; returns 1 on any drift, 0 when the two are identical.
compare_sets() {
    local graded_f built_f rc=0 name
    graded_f="$(mktemp)"
    built_f="$(mktemp)"
    printf '%s\n' "$1" | drop_not_cargo_bins | sort -u > "$graded_f"
    printf '%s\n' "$2" | drop_not_cargo_bins | sort -u > "$built_f"
    while read -r name; do
        [ -n "$name" ] || continue
        echo "  MISSING  $name — the preflight grades it, build-integ-bins does not build it."
        echo "           A run that needs it aborts: 'node binaries still stale after"
        echo "           forced rebuild'. Add '--bin $name' to the recipe."
        rc=1
    done < <(comm -23 "$graded_f" "$built_f")
    while read -r name; do
        [ -n "$name" ] || continue
        echo "  UNGRADED $name — build-integ-bins builds it, the preflight grades nothing"
        echo "           by that name. Either periculum dropped it and the recipe should"
        echo "           too, or it belongs in NOT_CARGO_BINS with its own recipe named."
        rc=1
    done < <(comm -13 "$graded_f" "$built_f")
    rm -f "$graded_f" "$built_f"
    return "$rc"
}

# --- Self-test ------------------------------------------------------------
#
# "No banned line found" and "the two sets agree" are both satisfied forever
# by a checker that stopped looking, so every classifier here is exercised on
# fixtures before it is allowed to say anything about the tree. A checker
# that gets any of them wrong fails the gate here rather than passing the
# tree silently.
SELFTEST_DIR="$(mktemp -d)"
trap 'rm -rf "$SELFTEST_DIR"' EXIT

cat > "$SELFTEST_DIR/bad-plain" <<'EOF'
cargo build --release --bin lnsd --bin lnstest --bin lncp
EOF

cat > "$SELFTEST_DIR/bad-continued" <<'EOF'
CARGO_TARGET_DIR="$CACHE_TARGET" CARGO_INCREMENTAL=0 \
  cargo build --release --bin lnsd
EOF

cat > "$SELFTEST_DIR/bad-yaml" <<'EOF'
steps:
  build:
    commands:
      - cargo build --release --bins
EOF

cat > "$SELFTEST_DIR/good" <<'EOF'
# cargo build --release --bin lnsd   (what this used to be)
      - # cargo build --release --bin lncp
( cd "$REPO_DIR" && cargo build --release )
just build-integ-bins
EOF

# periculum's constants in the shape its main.rs actually has them: one
# single-line array, one wrapped after the `=`.
cat > "$SELFTEST_DIR/periculum-source.rs" <<'EOF'
/// The binaries `TestRunner::new` mounts for EVERY scenario.
const PREFLIGHT_REQUIRED_BINARIES: [&str; 3] = ["lnsd", "lnstest", "lnstatus"];

/// Mounted only by the scenarios that need them.
const PREFLIGHT_OPTIONAL_BINARIES: [&str; 2] =
    ["lora-proxy", "c-lnsd"];
EOF

# The same file after somebody renamed the constants: the parser must report
# that it found nothing, not report an empty graded set.
sed 's/PREFLIGHT_/MOUNTED_/' "$SELFTEST_DIR/periculum-source.rs" \
    > "$SELFTEST_DIR/periculum-source-renamed.rs"

selftest_failed=0
selftest_fail() {
    echo "check-integ-bin-list: SELF-TEST FAILED — $1"
    selftest_failed=1
}

for fixture in bad-plain bad-continued bad-yaml; do
    if [ -z "$(banned_lines "$SELFTEST_DIR/$fixture")" ]; then
        selftest_fail "fixture '$fixture' was not caught."
    fi
done
if [ -n "$(banned_lines "$SELFTEST_DIR/good")" ]; then
    selftest_fail "clean fixture was flagged:"
    banned_lines "$SELFTEST_DIR/good"
fi

got="$(printf '%s\n' '    cargo build --release --bin lnsd --bin=lncp --bins' | recipe_bin_names | tr '\n' ' ')"
if [ "$got" != "lncp lnsd " ]; then
    selftest_fail "recipe parser read '$got', expected 'lncp lnsd '."
fi
if [ -n "$(printf '%s\n' 'cargo build --release' | recipe_bin_names)" ]; then
    selftest_fail "recipe parser invented a binary in a line that names none."
fi

got="$(graded_from_source "$SELFTEST_DIR/periculum-source.rs" | tr '\n' ' ')"
if [ "$got" != "c-lnsd lnsd lnstatus lnstest lora-proxy " ]; then
    selftest_fail "source parser read '$got' from the fixture constants."
fi
if graded_from_source "$SELFTEST_DIR/periculum-source-renamed.rs" >/dev/null 2>&1; then
    selftest_fail "source parser reported success on a file whose constants are gone."
fi

# The Codeberg #229 shape itself: a graded binary the recipe does not build.
if drift="$(compare_sets "$(printf 'lnsd\nlnstatus\nc-lnsd\n')" "$(printf 'lnsd\n')")"; then
    selftest_fail "a graded-but-unbuilt binary was called no drift at all."
elif [[ "$drift" != *"MISSING  lnstatus"* ]]; then
    selftest_fail "a graded-but-unbuilt binary was misreported: '$drift'"
fi
# And the other direction.
if drift="$(compare_sets "$(printf 'lnsd\n')" "$(printf 'lnsd\nlnghost\n')")"; then
    selftest_fail "a built-but-ungraded binary was called no drift at all."
elif [[ "$drift" != *"UNGRADED lnghost"* ]]; then
    selftest_fail "a built-but-ungraded binary was misreported: '$drift'"
fi
# Identical sets, c-lnsd excluded on the graded side only, must be silent.
if ! compare_sets "$(printf 'lnsd\nc-lnsd\n')" "$(printf 'lnsd\n')" >/dev/null; then
    selftest_fail "identical sets were reported as drift."
fi

if [ "$selftest_failed" -ne 0 ]; then
    echo "The checker itself is broken; its verdict on the tree means nothing."
    exit 1
fi

# --- The gate: one list ---------------------------------------------------

found=0
verdict_suffix="."
for f in "${GUARDED_FILES[@]}"; do
    if [ ! -f "$f" ]; then
        # A guarded file that vanished or was renamed silently disables half
        # this check, so it is a failure and not a skip.
        echo "check-integ-bin-list: FAILED — guarded file '$f' does not exist."
        echo "It was renamed or removed: point GUARDED_FILES at its replacement,"
        echo "or drop the entry deliberately."
        found=1
        continue
    fi
    hits="$(banned_lines "$f")"
    if [ -n "$hits" ]; then
        echo "$hits"
        found=1
    fi
done

if [ "$found" -ne 0 ]; then
    echo
    echo "check-integ-bin-list: FAILED — a second list of integration binaries."
    echo "The list lives in the Justfile recipe 'build-integ-bins' and nowhere"
    echo "else. Call the recipe:"
    echo
    echo "    ( cd \"\$REPO_DIR\" && CARGO_TARGET_DIR=\"\$CACHE_TARGET\" \\"
    echo "      CARGO_INCREMENTAL=0 just build-integ-bins )"
    echo
    echo "A copied list drifts from the one 'periculum check-freshness' asserts"
    echo "against, and the drift only shows up as a hardware nightly aborting in"
    echo "its preflight (2026-08-19)."
fi

# --- The gate: and it is the set the preflight grades ---------------------

# The recipe body comes from `just --show` rather than from grepping the
# Justfile, so what is compared is the recipe as just resolves it. The two
# ways that can fail want different words: just refusing to run at all (a
# renamed recipe, a Justfile that no longer parses) is not the same as a
# recipe that runs and names no binary.
if ! recipe_body="$(just --show build-integ-bins 2>&1)"; then
    echo "check-integ-bin-list: FAILED — 'just --show build-integ-bins' did not run,"
    echo "so there is no build set to compare. just said:"
    printf '%s\n' "$recipe_body" | sed 's/^/  /'
    found=1
elif built_set="$(printf '%s\n' "$recipe_body" | recipe_bin_names)"; [ -z "$built_set" ]; then
    echo "check-integ-bin-list: FAILED — 'just --show build-integ-bins' named no"
    echo "binary at all. The recipe was emptied, or builds its list some way this"
    echo "checker cannot read; either way nothing is being compared."
    found=1
else
    for excluded in "${NOT_CARGO_BINS[@]}"; do
        if ! just --show "build-$excluded" >/dev/null 2>&1; then
            echo "check-integ-bin-list: FAILED — '$excluded' is excluded from the"
            echo "comparison because recipe 'build-$excluded' builds it, and there is no"
            echo "such recipe. The carve-out now excludes it from being built at all."
            found=1
        fi
    done

    if graded_set="$(graded_from_binary "$PERICULUM_BIN")"; then
        graded_from="$PERICULUM_BIN (check-freshness --list)"
    elif graded_set="$(graded_from_source "$PERICULUM_SOURCE")"; then
        graded_from="$PERICULUM_SOURCE (PREFLIGHT_*_BINARIES)"
    else
        graded_set=""
        graded_from=""
    fi

    if [ -z "$graded_from" ]; then
        # Not a failure: this tree must gate from a clone that has no
        # periculum checkout beside it. Named, because a silent skip is how a
        # check stops existing.
        echo "check-integ-bin-list: SKIPPED the set comparison — no periculum to ask."
        echo "Looked for a built binary at $PERICULUM_BIN and for the constants in"
        echo "$PERICULUM_SOURCE. Set PERICULUM_ROOT if the checkout is elsewhere."
        verdict_suffix="; set comparison skipped."
    elif drift="$(compare_sets "$graded_set" "$built_set")"; then
        verdict_suffix=", and it is exactly the set the preflight grades"
        verdict_suffix="$verdict_suffix (per $graded_from)."
    else
        echo
        echo "check-integ-bin-list: FAILED — the build set and the preflight's set differ."
        echo "Graded set read from: $graded_from"
        echo "$drift"
        echo
        echo "The preflight is only a preflight while the two sets are equal. When they"
        echo "are not, a hardware run does not lose a check, it aborts at 03:37 naming a"
        echo "binary nothing built (Codeberg #229, #310)."
        found=1
    fi
fi

# One verdict for both halves. Printed here and nowhere earlier, because a
# half that passed must not announce an OK while the other half is printing
# its failure above it.
[ "$found" -eq 0 ] || exit 1
echo "check-integ-bin-list: OK — ${#GUARDED_FILES[@]} files build via build-integ-bins$verdict_suffix"
exit 0
