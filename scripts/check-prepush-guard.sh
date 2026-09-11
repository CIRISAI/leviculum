#!/usr/bin/env bash
# Positive control for the guards in .githooks/pre-push.
#
# Those guards are cold code: they fire on the rare push that is wrong, and
# between firings nothing exercises them. A guard nobody runs is a guard that
# rots — .githooks/pre-push has advertised a selftest in a comment since
# 2026-08-17 and none existed, which is why a tree-guard defect could sit in it
# unnoticed until a push was refused for the wrong reason (2026-09-11).
#
# Every case below drives the real hook, with LEV_PREPUSH_GUARD_ONLY=1 so it
# stops before the multi-minute gates, against a scratch repository built here.
# No case inspects the hook's source; each one pushes something and reads the
# verdict. The whole file runs in about a second.
#
# Usage: bash scripts/check-prepush-guard.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOOK="$ROOT/.githooks/pre-push"
[ -x "$HOOK" ] || { echo "[prepush-guard] not executable: $HOOK" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

FAILURES=0
CHECKS=0

# A scratch repository with two commits, clean tree, no remotes. Echoes its
# path; the caller dirties it or not.
scratch_repo() {
    local dir="$WORK/$1"
    git init -q --initial-branch=master "$dir"
    git -C "$dir" config user.email "gate@example.invalid"
    git -C "$dir" config user.name "Gate"
    echo one >"$dir/file.txt"
    git -C "$dir" add file.txt
    git -C "$dir" commit -qm "one"
    echo two >>"$dir/file.txt"
    git -C "$dir" commit -qam "two"
    printf '%s' "$dir"
}

# run <repo> <remote> <url> <ref-line>...   -> exit code in $rc, output in $out
run_hook() {
    local repo="$1" remote="$2" url="$3"
    shift 3
    local lines=""
    local line
    for line in "$@"; do lines+="$line"$'\n'; done
    set +e
    out="$(cd "$repo" && printf '%s' "$lines" |
        LEV_PREPUSH_GUARD_ONLY=1 bash "$HOOK" "$remote" "$url" 2>&1)"
    rc=$?
    set -e
}

ok() {
    CHECKS=$((CHECKS + 1))
    echo "[prepush-guard] ok      $1"
}

bad() {
    CHECKS=$((CHECKS + 1))
    FAILURES=$((FAILURES + 1))
    echo "[prepush-guard] FAIL    $1" >&2
    printf '%s\n' "${out:-}" | sed 's|^|[prepush-guard]         |' >&2
}

# expect_refusal <description> <phrase the refusal must name>
expect_refusal() {
    local what="$1" phrase="$2"
    if [ "$rc" -eq 0 ]; then
        bad "$what: hook exited 0, expected a refusal"
    elif ! printf '%s' "$out" | grep -qF "$phrase"; then
        bad "$what: refused, but the message never says '$phrase'"
    else
        ok "$what"
    fi
}

expect_pass() {
    local what="$1"
    if [ "$rc" -ne 0 ]; then
        bad "$what: hook exited $rc, expected it to pass"
    else
        ok "$what"
    fi
}

PUBLIC_URL="ssh://git@codeberg.org/lew/leviculum.git"
HOST_URL="ssh://hamster/home/lew/coding/libreticulum"
ZERO="0000000000000000000000000000000000000000"

# --- ref guard ---------------------------------------------------------------
repo="$(scratch_repo refguard)"
head="$(git -C "$repo" rev-parse HEAD)"

run_hook "$repo" origin "$PUBLIC_URL" \
    "refs/heads/feature $head refs/heads/feature $ZERO"
expect_refusal "feature branch to the public forge is refused" "is not master"

run_hook "$repo" origin "$PUBLIC_URL" \
    "refs/heads/master $head refs/heads/master $ZERO"
expect_pass "master to the public forge passes"

run_hook "$repo" origin "$PUBLIC_URL" \
    "refs/tags/v1.2.3 $head refs/tags/v1.2.3 $ZERO"
expect_pass "a tag to the public forge passes"

# Deleting a stray branch from the forge is the remedy, not the harm.
run_hook "$repo" origin "$PUBLIC_URL" \
    "(delete) $ZERO refs/heads/feature $head"
expect_pass "deleting a branch on the public forge passes"

# Host-to-host is how feature branches legitimately move between machines.
run_hook "$repo" hamster "$HOST_URL" \
    "refs/heads/feature $head refs/heads/feature $ZERO"
expect_pass "feature branch host-to-host passes"

# A push of several refs is refused for the one bad ref among them.
run_hook "$repo" origin "$PUBLIC_URL" \
    "refs/heads/master $head refs/heads/master $ZERO" \
    "refs/heads/feature $head refs/heads/feature $ZERO"
expect_refusal "one bad ref refuses a multi-ref push" "is not master"

# --- Claude guard ------------------------------------------------------------
# Same standing: committed Claude-specific files must not reach a public forge,
# and the ignore rule that should have stopped them has already failed by the
# time this hook sees the commit.
claude_repo="$(scratch_repo claudeguard)"
echo "policy" >"$claude_repo/CLAUDE.md"
git -C "$claude_repo" add -f CLAUDE.md
git -C "$claude_repo" commit -qm "policy file"
claude_head="$(git -C "$claude_repo" rev-parse HEAD)"
run_hook "$claude_repo" origin "$PUBLIC_URL" \
    "refs/heads/master $claude_head refs/heads/master $ZERO"
expect_refusal "a committed CLAUDE.md is refused to the public forge" "CLAUDE.md"

run_hook "$claude_repo" hamster "$HOST_URL" \
    "refs/heads/master $claude_head refs/heads/master $ZERO"
expect_pass "the same commit passes host-to-host"

# --- tree guard --------------------------------------------------------------
# The gates below the guards test the WORKING TREE. These three cases are the
# whole claim: what the gates test and what the push publishes must be the
# same thing, whatever the remote.
dirty="$(scratch_repo dirty)"
dirty_head="$(git -C "$dirty" rev-parse HEAD)"
echo "half-finished port" >>"$dirty/file.txt"
run_hook "$dirty" origin "$PUBLIC_URL" \
    "refs/heads/master $dirty_head refs/heads/master $ZERO"
expect_refusal "a dirty tree is refused" "uncommitted tracked changes"

# A staged-but-uncommitted change is the same defect one step further along.
git -C "$dirty" add file.txt
run_hook "$dirty" origin "$PUBLIC_URL" \
    "refs/heads/master $dirty_head refs/heads/master $ZERO"
expect_refusal "a staged change is refused" "uncommitted tracked changes"

# The documented carve-out: untracked files are in no commit and in every
# working tree, so they must NOT refuse. Without this case the guard could be
# tightened into one that fires on the normal state and nobody would notice
# until people started pushing with --no-verify.
untracked="$(scratch_repo untracked)"
untracked_head="$(git -C "$untracked" rev-parse HEAD)"
echo scratch >"$untracked/notes.log"
run_hook "$untracked" origin "$PUBLIC_URL" \
    "refs/heads/master $untracked_head refs/heads/master $ZERO"
expect_pass "an untracked file does not refuse"

# HEAD elsewhere: the gates would describe HEAD, the push would publish
# something else.
elsewhere="$(scratch_repo elsewhere)"
elsewhere_head="$(git -C "$elsewhere" rev-parse HEAD)"
elsewhere_prev="$(git -C "$elsewhere" rev-parse HEAD~1)"
run_hook "$elsewhere" origin "$PUBLIC_URL" \
    "refs/heads/master $elsewhere_prev refs/heads/master $ZERO"
expect_refusal "a pushed master sha that is not HEAD is refused" \
    "but HEAD, the tree the gates below test, is $elsewhere_head"

# Host-to-host too: a gate verdict about the wrong commit is worthless
# wherever that commit is going.
run_hook "$elsewhere" hamster "$HOST_URL" \
    "refs/heads/master $elsewhere_prev refs/heads/master $ZERO"
expect_refusal "the same mismatch is refused host-to-host" "would get $elsewhere_prev"

# ... but a non-master ref is exempt on purpose: a branch fetched from the
# other host is routinely pushed from a tree standing somewhere else.
run_hook "$elsewhere" hamster "$HOST_URL" \
    "refs/heads/feature $elsewhere_prev refs/heads/feature $ZERO"
expect_pass "a non-master ref at another sha passes"

run_hook "$elsewhere" origin "$PUBLIC_URL" \
    "refs/heads/master $elsewhere_head refs/heads/master $ZERO"
expect_pass "a clean tree pushing HEAD passes the guard"

echo "[prepush-guard] $((CHECKS - FAILURES))/$CHECKS checks passed"
[ "$FAILURES" -eq 0 ] || exit 1
