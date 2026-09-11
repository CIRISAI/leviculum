#!/usr/bin/env bash
# Positive control for the guards in .githooks/pre-push, and for the remedy
# their refusals print.
#
# Those guards are cold code: they fire on the rare push that is wrong, and
# between firings nothing exercises them. A guard nobody runs is a guard that
# rots, and a tree-guard defect could sit in this hook unnoticed until a push
# was refused for the wrong reason (2026-09-11).
#
# A selftest for the ref guard did exist before this file and had run daily for
# weeks: `~/.local/bin/lev-selftest` on hamster, driven by project-sanity.sh.
# It lives outside the repository and on one host, so it says nothing about the
# hook in any other clone, and nothing about the tree guard. This file is the
# in-repository version — `just fast` runs it, so every host that pushes runs
# it — and lev-selftest now calls this script instead of its own copy.
#
# Every case below drives the real hook, with LEV_PREPUSH_GUARD_ONLY=1 so it
# stops before the multi-minute gates, against a scratch repository built here.
# No case inspects the hook's source; each one pushes something and reads the
# verdict. The whole file runs in about a second.
#
# Usage, from any working directory:
#
#   bash <path>/scripts/check-prepush-guard.sh                 # this repo's hook
#   bash <path>/scripts/check-prepush-guard.sh <hook>          # a given hook
#   LEV_PREPUSH_HOOK=<hook> bash <path>/scripts/check-prepush-guard.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Argument first, environment second, this repo's hook last. Callers outside
# the tree — lev-selftest drives the hook of whichever checkout it is auditing
# — name the hook; everything here is absolute afterwards, because the cases
# below run with the working directory inside a scratch repository.
HOOK="${1:-${LEV_PREPUSH_HOOK:-$ROOT/.githooks/pre-push}}"
[ -f "$HOOK" ] || { echo "[prepush-guard] no such hook: $HOOK" >&2; exit 1; }
HOOK="$(cd "$(dirname "$HOOK")" && pwd)/$(basename "$HOOK")"
[ -x "$HOOK" ] || { echo "[prepush-guard] not executable: $HOOK" >&2; exit 1; }

# The remedy that hook's refusals print, taken from the same checkout as the
# hook rather than from this script's own tree: the two are a pair, and a run
# against someone else's hook must test the script that hook names.
PUSH_CLEAN="$(cd "$(dirname "$HOOK")/.." && pwd)/scripts/push-clean.sh"
[ -r "$PUSH_CLEAN" ] ||
    { echo "[prepush-guard] no such script: $PUSH_CLEAN" >&2; exit 1; }

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
        bad "$what: exited 0, expected a refusal"
    elif ! printf '%s' "$out" | grep -qF "$phrase"; then
        bad "$what: refused, but the message never says '$phrase'"
    else
        ok "$what"
    fi
}

expect_pass() {
    local what="$1"
    if [ "$rc" -ne 0 ]; then
        bad "$what: exited $rc, expected it to pass"
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

# --- the remedy the refusals print -------------------------------------------
# A refusal is only as good as the recipe it hands you. The one these guards
# printed until 2026-09-11 — `git clone . /tmp/push-tree && checkout <sha>` —
# built a tree that pushes with NO hook running at all, because a clone
# inherits no `core.hooksPath`. The push then arrives, which is exactly what
# makes the defect invisible: the operator sees success.
#
# So these cases must not ask whether the push arrived. The commit the clone
# pushes carries a CLAUDE.md to a remote named `origin`, i.e. something the
# hook MUST refuse; the refusal sentence is produced by nothing else and can
# only have come from inside the clone. The negative control below removes
# `core.hooksPath` from the same clone and pushes the identical commit, which
# then goes straight through — the proof fails exactly when the hook is gone.

# A scratch source repository: the real hook, the real script, a policy file
# the hook must refuse, and a submodule, since initialising them was the second
# thing the old recipe left undone.
sub="$WORK/pushclean-sub"
git init -q --initial-branch=master "$sub"
git -C "$sub" config user.email "gate@example.invalid"
git -C "$sub" config user.name "Gate"
echo vendored >"$sub/file.txt"
git -C "$sub" add file.txt
git -C "$sub" commit -qm "vendored"

push_src="$(scratch_repo pushclean-src)"
mkdir -p "$push_src/.githooks" "$push_src/scripts"
cp "$HOOK" "$push_src/.githooks/pre-push"
cp "$PUSH_CLEAN" "$push_src/scripts/push-clean.sh"
chmod +x "$push_src/.githooks/pre-push" "$push_src/scripts/push-clean.sh"
echo "policy" >"$push_src/CLAUDE.md"
git -C "$push_src" -c protocol.file.allow=always \
    submodule add -q "$sub" vendor/sub
git -C "$push_src" add -f .githooks/pre-push scripts/push-clean.sh CLAUDE.md
git -C "$push_src" commit -qm "hook, remedy, policy file and a submodule"
push_head="$(git -C "$push_src" rev-parse HEAD)"

# An empty bare repository standing in for the forge, reached under the name
# `origin` so the hook classifies the push as public exactly as it would.
forge="$WORK/pushclean-forge.git"
git init -q --bare "$forge"
git -C "$push_src" remote add origin "$forge"

forge_master() {
    git -C "$forge" rev-parse --verify --quiet refs/heads/master || true
}

push_tree="$WORK/pushclean-tree"
run_push_clean() {
    set +e
    out="$(LEV_PREPUSH_GUARD_ONLY=1 LEV_PUSH_TREE="$push_tree" \
        bash "$push_src/scripts/push-clean.sh" "$@" 2>&1)"
    rc=$?
    set -e
}

run_push_clean "$push_head"
expect_refusal "push-clean.sh: the hook runs inside the clone" \
    "carries Claude-specific files"

CHECKS=$((CHECKS + 1))
if [ -n "$(forge_master)" ]; then
    FAILURES=$((FAILURES + 1))
    echo "[prepush-guard] FAIL    the refused push reached the forge anyway" >&2
else
    echo "[prepush-guard] ok      the refused push published nothing"
fi

CHECKS=$((CHECKS + 1))
if [ -r "$push_tree/vendor/sub/file.txt" ]; then
    echo "[prepush-guard] ok      push-clean.sh initialises the submodules"
else
    FAILURES=$((FAILURES + 1))
    echo "[prepush-guard] FAIL    push-clean.sh left vendor/sub uninitialised;" >&2
    echo "[prepush-guard]         check-submodules would stop \`just fast\` here" >&2
fi

CHECKS=$((CHECKS + 1))
clone_url="$(git -C "$push_tree" remote get-url origin 2>/dev/null || true)"
if [ "$clone_url" = "$forge" ]; then
    echo "[prepush-guard] ok      the clone's origin is the forge, not the source"
else
    FAILURES=$((FAILURES + 1))
    echo "[prepush-guard] FAIL    the clone's origin is '$clone_url'," >&2
    echo "[prepush-guard]         expected the source's own origin URL $forge" >&2
fi

# Negative control. Same clone, same commit, same remote — only core.hooksPath
# is gone, which is precisely what the old recipe never set. If this push were
# also refused, the refusal above would be evidence of nothing.
git -C "$push_tree" config --unset core.hooksPath
set +e
out="$(git -C "$push_tree" push origin "$push_head:refs/heads/master" 2>&1)"
rc=$?
set -e
expect_pass "without core.hooksPath the identical push is not refused"

CHECKS=$((CHECKS + 1))
if printf '%s' "$out" | grep -qF "carries Claude-specific files"; then
    FAILURES=$((FAILURES + 1))
    echo "[prepush-guard] FAIL    the hookless push was refused too; the proof" >&2
    echo "[prepush-guard]         above does not distinguish hook from no hook" >&2
elif [ "$(forge_master)" = "$push_head" ]; then
    echo "[prepush-guard] ok      ... and the forge took what the hook refused"
else
    FAILURES=$((FAILURES + 1))
    echo "[prepush-guard] FAIL    the hookless push neither refused nor landed;" >&2
    echo "[prepush-guard]         forge master is '$(forge_master)'" >&2
fi

# Reusing the push tree must never clobber what is in it. The phrase is
# push-clean's own wording, not the hook's ("the WORKING tree has ..."): with
# this check removed the hook still refuses the push a moment later, so the
# looser phrase would pass while push-clean was free to check out over the
# edits it was supposed to protect.
git -C "$push_tree" config core.hooksPath .githooks
echo "half-finished port" >>"$push_tree/CLAUDE.md"
run_push_clean "$push_head"
expect_refusal "push-clean.sh: a dirty push tree is refused" \
    "the push tree has uncommitted tracked changes"

echo "[prepush-guard] $((CHECKS - FAILURES))/$CHECKS checks passed"
[ "$FAILURES" -eq 0 ] || exit 1
