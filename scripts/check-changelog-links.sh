#!/usr/bin/env bash
#
# CHANGELOG.md holds two lists of versions, and they must be the same list
# (Codeberg #287).
#
# Keep a Changelog headings are reference links: `## [0.8.1] - 2026-08-16`
# renders as a link to the release diff only if a matching definition
# `[0.8.1]: https://…` exists at the foot of the file. Without one, Markdown
# has nothing to resolve and emits the literal text `[0.8.1]`, brackets and
# all. Nothing about writing the heading produces the definition, so the two
# lists drift by default, and they drift at the TOP of the file: the heading
# is written in the release commit, the definition is remembered later or
# never. At master 752baa4 seven headings had no definition and the four
# newest releases were among them, so every reader who opened the changelog
# saw the broken ones first. `## [0.1.0] - 2025-XX-XX` had shipped in every
# release since with its placeholder date still in it.
#
# What this refuses, all three found in that state:
#
#   UNDEFINED   a heading with no link definition — renders as bracket text.
#   STALE       a definition with no heading — the heading was renamed or
#               removed and its definition was left behind, so the next
#               reader adding that version finds a link that points at the
#               wrong range.
#   BAD DATE    a heading whose date is not a real YYYY-MM-DD. `2025-XX-XX`
#               is the case that shipped; so is a heading that lost its date
#               entirely, and so is `Unreleased` carrying one (it has no
#               release date by definition, and a date there means somebody
#               released it and forgot to rename the heading).
#
# It deliberately does not check that a URL resolves. A gate that needs the
# network is a gate that fails on a train, and the definitions here are
# compare links whose ends are tags and commit SHAs this repository already
# has. Whether every version ALSO has a tag is a separate question with a
# separate answer — 0.6.1 and 0.6.3 were released and never tagged, so their
# definitions name commits — and it is not this file's business.
#
# In a gate rather than a `#[test]` for the reason the whole check-* family
# is: it reads a file no test binary compiles, and it must run on the push
# path where the author still has the changelog open.
#
# Exit 0 = the two lists are the same list and every date is real.
# Exit 1 = drift, a placeholder date, or the checker's own self-test failed.
#
# Usage:
#   bash scripts/check-changelog-links.sh [path/to/CHANGELOG.md]
set -uo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
CHANGELOG="${1:-$REPO_DIR/CHANGELOG.md}"

# Print one line per problem in the changelog at $1, and nothing when it is
# clean. Returns 1 if anything was printed.
#
# Headings are `## [name]` optionally followed by ` - date`; definitions are
# `[name]: url` at the start of a line. Both are read with awk in one pass so
# the line numbers in a failure are the file's own.
check_changelog() {
    local file="$1" problems
    if [ ! -f "$file" ]; then
        echo "  MISSING FILE $file — nothing to check."
        return 1
    fi
    problems="$(awk '
        # `## [name]` or `## [name] - date`. Anything else starting with ##
        # is an ordinary section heading and not our business.
        /^## \[/ {
            line = $0
            name = line
            sub(/^## \[/, "", name)
            if (name !~ /\]/) next
            sub(/\].*$/, "", name)
            heading_line[name] = FNR
            order[++n] = name

            rest = line
            sub(/^## \[[^]]*\]/, "", rest)
            sub(/^[ \t]*-[ \t]*/, "", rest)
            sub(/[ \t]+$/, "", rest)

            if (name == "Unreleased") {
                if (rest != "") {
                    printf "  BAD DATE  %s:%d: [Unreleased] carries a date (%s).\n", FILENAME, FNR, rest
                    printf "            Unreleased has no release date. If it was released, rename\n"
                    printf "            the heading to the version and open a fresh Unreleased.\n"
                    bad++
                }
            } else if (rest == "") {
                printf "  BAD DATE  %s:%d: [%s] has no date.\n", FILENAME, FNR, name
                printf "            Every released heading is `## [version] - YYYY-MM-DD`.\n"
                bad++
            } else if (rest !~ /^[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]$/) {
                printf "  BAD DATE  %s:%d: [%s] has date %s, which is not YYYY-MM-DD.\n", FILENAME, FNR, name, rest
                printf "            A placeholder here ships in every release after it. Use the\n"
                printf "            date of the tag or of the release commit.\n"
                bad++
            }
            next
        }
        # `[name]: url` in column 1 — the link definitions at the foot.
        /^\[[^]]+\]:[ \t]/ {
            name = $0
            sub(/^\[/, "", name)
            sub(/\]:.*$/, "", name)
            def_line[name] = FNR
            next
        }
        END {
            for (i = 1; i <= n; i++) {
                name = order[i]
                if (!(name in def_line)) {
                    printf "  UNDEFINED %s:%d: [%s] has no link definition.\n", FILENAME, heading_line[name], name
                    printf "            It renders as the literal text [%s]. Add a definition at the\n", name
                    printf "            foot of the file, in the same descending order as the rest.\n"
                    bad++
                }
            }
            for (name in def_line) {
                if (!(name in heading_line)) {
                    printf "  STALE     %s:%d: [%s] is defined and no heading uses it.\n", FILENAME, def_line[name], name
                    printf "            The heading was renamed or removed; remove the definition\n"
                    printf "            too, or restore the heading.\n"
                    bad++
                }
            }
            if (bad > 0) exit 1
        }
    ' "$file")"
    if [ -n "$problems" ]; then
        printf '%s\n' "$problems"
        return 1
    fi
    return 0
}

# Count what the gate is allowed to claim afterwards, so "OK" cannot be said
# about a file the parser read nothing out of.
count_headings() { grep -c '^## \[' "$1" || true; }

# --- Self-test ------------------------------------------------------------
#
# "No drift found" is satisfied forever by a checker that stopped looking, so
# every class it claims to catch is injected into a fixture and asserted to
# fire, and a clean fixture is asserted to stay silent.
SELFTEST_DIR="$(mktemp -d)"
trap 'rm -rf "$SELFTEST_DIR"' EXIT

cat > "$SELFTEST_DIR/good.md" <<'EOF'
# Changelog

## [Unreleased]

### Added

Something.

## [0.2.0] - 2026-01-30

## [0.1.0] - 2026-01-28

### Notes

Not a version heading: ## not a heading at all

[Unreleased]: https://example.invalid/compare/v0.2.0...master
[0.2.0]: https://example.invalid/compare/v0.1.0...v0.2.0
[0.1.0]: https://example.invalid/src/tag/v0.1.0
EOF

sed '/^\[0\.2\.0\]:/d' "$SELFTEST_DIR/good.md" > "$SELFTEST_DIR/undefined.md"
sed 's|^\[0\.1\.0\]:|[0.0.9]:|' "$SELFTEST_DIR/good.md" > "$SELFTEST_DIR/stale.md"
sed 's/^## \[0\.1\.0\] - 2026-01-28/## [0.1.0] - 2025-XX-XX/' "$SELFTEST_DIR/good.md" > "$SELFTEST_DIR/placeholder.md"
sed 's/^## \[0\.2\.0\] - 2026-01-30/## [0.2.0]/' "$SELFTEST_DIR/good.md" > "$SELFTEST_DIR/undated.md"
sed 's/^## \[Unreleased\]$/## [Unreleased] - 2026-02-01/' "$SELFTEST_DIR/good.md" > "$SELFTEST_DIR/dated-unreleased.md"

selftest_failed=0
selftest_fail() {
    echo "check-changelog-links: SELF-TEST FAILED — $1"
    selftest_failed=1
}

if ! out="$(check_changelog "$SELFTEST_DIR/good.md")"; then
    selftest_fail "the clean fixture was flagged:"
    printf '%s\n' "$out"
fi

expect_catch() {
    local fixture="$1" want="$2" out
    if out="$(check_changelog "$SELFTEST_DIR/$fixture")"; then
        selftest_fail "fixture '$fixture' was not caught at all."
    elif [[ "$out" != *"$want"* ]]; then
        selftest_fail "fixture '$fixture' was caught, but not as '$want':"
        printf '%s\n' "$out"
    fi
}

expect_catch undefined.md         "UNDEFINED"
expect_catch stale.md             "STALE"
expect_catch placeholder.md       "2025-XX-XX"
expect_catch undated.md           "has no date"
expect_catch dated-unreleased.md  "[Unreleased] carries a date"

# A file the parser reads nothing out of must not be able to pass as clean.
: > "$SELFTEST_DIR/empty.md"
if [ "$(count_headings "$SELFTEST_DIR/empty.md")" != "0" ]; then
    selftest_fail "the heading count is not the number of headings."
fi

if [ "$selftest_failed" -ne 0 ]; then
    echo "The checker itself is broken; its verdict on the changelog means nothing."
    exit 1
fi

# --- The gate -------------------------------------------------------------

headings="$(count_headings "$CHANGELOG")"
if ! check_changelog "$CHANGELOG"; then
    echo
    echo "check-changelog-links: FAILED — $CHANGELOG."
    echo "A heading without a definition renders as bracket text, and it is always"
    echo "the newest entries that break, because the definition is the half nothing"
    echo "forces anyone to write (Codeberg #287)."
    exit 1
fi

if [ "$headings" -eq 0 ]; then
    echo "check-changelog-links: FAILED — no '## [version]' heading in $CHANGELOG."
    echo "Either the file lost its headings or this checker can no longer read them;"
    echo "either way it is grading nothing."
    exit 1
fi

echo "check-changelog-links: OK — $headings headings, each with a link definition and a real date."
exit 0
