#!/usr/bin/env bash
#
# Commit-message authorship guard (Codeberg #205).
#
# CONTRIBUTING.md: "no AI trailers. Commit under your real name and a
# reachable e-mail address." Until this script nothing enforced that in
# either repository, and on 2026-08-07 a commit carrying
# `Co-Authored-By: <model> <noreply@…>` landed in periculum, in a pass whose
# author had cited that exact rule, correctly, hours earlier. It was caught
# only because a human read the message before pushing, and amended it.
#
# This script is byte-identical in leviculum and periculum. The rule is
# stated in both CONTRIBUTING.md files in the same words, so the two
# repositories agree rather than one relying on the other.
#
# The rule is not unclear and it is not forgotten. It is unenforced *against
# a default*: the assistant harnesses used here instruct their agents to
# append that trailer to every commit. A rule that has to win against a
# default, with nothing behind it, stays perpetually half-kept, and its
# violation is invisible unless a person reads every message before every
# push. That is what this closes.
#
# WHAT COUNTS AS A VIOLATION: a matching line at COLUMN 0.
#
#   An indented line is prose, not a trailer. This is the whole of the
#   quoting rule, and it is deliberate:
#
#   * Column 0 is where the default writes. A harness emitting a trailer is
#     emitting a real trailer -- git's `interpret-trailers`, Codeberg,
#     GitHub and GitLab all harvest from column 0 and nowhere else. Matching
#     there is matching the thing being enforced against, not a paraphrase
#     of it.
#   * It leaves a deterministic escape for a message that must *discuss* a
#     trailer -- this guard's own commit message does -- namely the
#     indentation git already uses for quoted material. One keystroke, and
#     visible in review.
#   * The alternative considered was git's own trailer block: the last
#     paragraph, which is the only place a trailer is semantically a
#     trailer. Rejected, because the default's `Generated with <tool>` line
#     sits in its own paragraph *above* the trailer block, so that rule
#     would cover half the default and read as covering all of it.
#
#   A leading space defeats this check. That is not a gap: this guards
#   against a tool's default, not against a person who has decided to lie
#   about authorship, and no message-shaped check reaches the second.
#
# Usage:
#   check-commit-trailers.sh                # baseline..HEAD  (the CI mode)
#   check-commit-trailers.sh <git-range>    # e.g. HEAD~5..HEAD, or a sha
#   check-commit-trailers.sh --message FILE # one message file (commit-msg)
#
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

BASELINE_FILE="scripts/commit-trailer-baseline.txt"

# The vendor list is from the issue. It is matched case-insensitively and is
# not meant to be exhaustive -- a name nobody has heard of yet is caught by
# review, not here.
VENDORS='claude|anthropic|gpt|copilot|gemini'

# Three shapes, all anchored at column 0, all matched against a lowercased
# line:
#   1. the trailer itself, whoever it names;
#   2. `Generated with <tool>` -- required to name a vendor, because
#      "Generated with scripts/generate_test_vectors.py" is a sentence this
#      repository could legitimately write;
#   3. a line opening with the robot emoji, which is the harness's own
#      marker and has no legitimate use at column 0.
PATTERN="^(co-authored-by:.*(${VENDORS})|(🤖[[:space:]]*)?generated with.*(${VENDORS})|🤖)"

# Scans a stream of `\036<sha>\n<message>` records; prints one line per
# offending message line as `<sha>\t<line-no>\t<text>`. Exits 1 if any.
scan_records() {
    awk -v re="$PATTERN" '
        BEGIN { RS = "\036"; FS = "\n"; bad = 0 }
        NF == 0 || $1 == "" { next }
        {
            for (i = 2; i <= NF; i++) {
                if (tolower($i) ~ re) {
                    printf "%s\t%d\t%s\n", $1, i - 1, $i
                    bad = 1
                }
            }
        }
        END { exit bad }
    '
}

# Prints the offending lines for `git log`-selectable revisions.
scan_revs() {
    git log --format='%x1e%H%n%B' "$@" | scan_records
}

# --- standing canary ------------------------------------------------------
#
# The concept page requires a permanent pair, not a one-time demonstration:
# a gate that stops matching -- a pattern that no longer compiles the way it
# used to, an awk that returns nothing -- is green forever, which is the
# defect the page exists to remove. This runs before the guard reports
# anything else, on every invocation, and costs nothing measurable.
#
# leviculum's pinned `legacy` count is a second and stronger canary: 16 of
# the 1275 commits below its baseline must match and 1259 must not,
# recomputed every run, so a pattern that stopped matching or started
# matching everything goes red there without this. periculum's count is 0
# and covers only one direction, which is why the pair is here rather than
# only in the baseline file.
canary() {
    local bad good
    bad=$(printf 'subject\n\nbody\n\nCo-Authored-By: Claude Opus 5 <noreply@anthropic.com>\n')
    good=$(printf 'subject\n\nthe commit carried\n  Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>\nand I amended it.\n\nCloses: #205\n')
    if printf '\036canary\n%s\n' "$bad" | scan_records >/dev/null; then
        echo "CANARY: a machine-authorship trailer was NOT matched. This guard" >&2
        echo "        cannot see the defect it exists to catch, and every green" >&2
        echo "        run since it broke means nothing." >&2
        exit 3
    fi
    if ! printf '\036canary\n%s\n' "$good" | scan_records >/dev/null; then
        echo "CANARY: a message that only *quotes* a trailer, indented, was" >&2
        echo "        matched. A guard with false positives gets switched off." >&2
        exit 3
    fi
}
canary

explain() {
    cat >&2 <<'EOF'

CONTRIBUTING.md: no AI trailers. Commit under your real name and a
reachable e-mail address.

To fix the most recent commit:

    git commit --amend        # delete the offending line, save

For older commits in the range:

    git rebase -i <baseline>  # `reword` each one the report names

If the line above is prose that *quotes* a trailer rather than being one,
indent it by one space. This guard only matches at column 0, because that
is the only place a trailer is a trailer.
EOF
}

# --- one message file (the commit-msg hook) ------------------------------

if [ "${1:-}" = "--message" ]; then
    [ -n "${2:-}" ] || { echo "ERROR: --message needs a file" >&2; exit 2; }
    # Cut git's `--verbose` diff at the scissors line before scanning, so a
    # diff hunk of this very script cannot trip the guard. Comment lines
    # cannot match anyway -- they start with `#`, not with a trailer.
    hits=$(
        sed '/^#\{0,1\} \{0,1\}-\{8,\} >8 -\{8,\}/,$d' "$2" \
            | { printf '\036(message)\n'; cat; } \
            | scan_records
    ) || {
        echo "[check-commit-trailers] REJECTED: machine-authorship line in the commit message" >&2
        echo "$hits" | while IFS=$'\t' read -r _ no text; do
            printf '    line %s: %s\n' "$no" "$text" >&2
        done
        explain
        exit 1
    }
    exit 0
fi

# --- a range, or the pinned baseline -------------------------------------

if [ $# -gt 0 ]; then
    RANGE_DESC="$1"
    if hits=$(scan_revs "$1"); then
        echo "[check-commit-trailers] OK: no machine-authorship lines in $RANGE_DESC"
        exit 0
    fi
    echo "[check-commit-trailers] REJECTED: machine-authorship line(s) in $RANGE_DESC" >&2
    echo "$hits" | while IFS=$'\t' read -r sha no text; do
        printf '    %s  line %s: %s\n' "${sha:0:12}" "$no" "$text" >&2
    done
    explain
    exit 1
fi

# No argument: the CI mode. Two halves.
#
# `baseline..HEAD` is the guard. `legacy` is what stops the baseline being
# used as an off switch. Some commits below the baseline may carry the
# trailer -- they predate the rule having anything behind it, and rewriting
# published history to remove them is the larger harm -- so the baseline
# file pins how many, and this recomputes it every run. Bumping the baseline
# forward to silence a fresh failure moves a violation across that line in
# one direction or the other, and either way the count changes and a
# reviewer sees it. Same objection the concept page makes to expiry dates: a
# number a one-line diff can bump is not a bound.

[ -f "$BASELINE_FILE" ] || {
    echo "ERROR: $BASELINE_FILE is missing" >&2
    exit 2
}
BASELINE=$(awk '$1 == "baseline" { print $2 }' "$BASELINE_FILE")
LEGACY=$(awk '$1 == "legacy" { print $2 }' "$BASELINE_FILE")
[ -n "$BASELINE" ] && [ -n "$LEGACY" ] || {
    echo "ERROR: $BASELINE_FILE must set both 'baseline' and 'legacy'" >&2
    exit 2
}

# A shallow CI clone has to be deepened before either half means anything.
# Depth 1 contains the baseline when the baseline *is* HEAD, so testing for
# the object alone is not enough -- the legacy count would come out 0 and the
# range would come out empty, and both would look like an answer. Deepen on
# the shallow flag, and refuse to proceed if it did not take: a push of five
# commits with the trailer in the third must not read green.
if [ "$(git rev-parse --is-shallow-repository)" = "true" ]; then
    git fetch --unshallow >/dev/null 2>&1 \
        || git fetch --deepen=100000 >/dev/null 2>&1 \
        || true
fi
if [ "$(git rev-parse --is-shallow-repository)" = "true" ] \
    || ! git cat-file -e "${BASELINE}^{commit}" 2>/dev/null; then
    echo "ERROR: this clone is shallow, or does not contain the baseline commit" >&2
    echo "       $BASELINE, so the checked range cannot be" >&2
    echo "       established and 'git fetch --unshallow' did not fix it." >&2
    echo "       Deepen the clone (woodpecker: clone settings 'partial: false')." >&2
    echo "       Failing closed: an undetermined range is not a green range." >&2
    exit 2
fi

legacy_now=$(scan_revs "$BASELINE" | cut -f1 | sort -u | wc -l | tr -d '[:space:]') || true
if [ "$legacy_now" -ne "$LEGACY" ]; then
    echo "ERROR: $LEGACY commits at or below the baseline carry a machine-authorship" >&2
    echo "       line; $legacy_now do now. The baseline moved, or history was" >&2
    echo "       rewritten. Both are things a reviewer has to see." >&2
    echo "       Baseline: $BASELINE ($BASELINE_FILE)" >&2
    exit 1
fi

if hits=$(scan_revs "${BASELINE}..HEAD"); then
    n=$(git rev-list --count "${BASELINE}..HEAD")
    echo "[check-commit-trailers] OK: $n commit(s) since ${BASELINE:0:12}, none carrying a machine-authorship line ($LEGACY known below the baseline)"
    exit 0
fi
echo "[check-commit-trailers] REJECTED: machine-authorship line(s) since ${BASELINE:0:12}" >&2
echo "$hits" | while IFS=$'\t' read -r sha no text; do
    printf '    %s  line %s: %s\n' "${sha:0:12}" "$no" "$text" >&2
done
explain
exit 1
