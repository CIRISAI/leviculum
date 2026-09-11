#!/usr/bin/env bash
# Push one commit from a clean checkout that the gates actually run in.
#
# .githooks/pre-push refuses a push its gates would not be about: a tree with
# uncommitted tracked changes, or a master push of a sha that is not HEAD. Both
# refusals used to print
#
#     git clone . /tmp/push-tree && git -C /tmp/push-tree checkout <sha>
#
# and that recipe is wrong three times over. All three were met at once in
# /home/lew/ci/push-tree on 2026-09-11:
#
#   - a clone does not inherit `core.hooksPath`, so the tree created to BE
#     gated pushes with no gate running at all — the remedy quietly undid the
#     guard that printed it;
#   - nothing initialises the submodules, so `just fast` stops at
#     check-submodules before it tests anything;
#   - the clone's `origin` is the local repository, so the push lands next
#     door instead of on the forge.
#
# This script is that remedy done properly. Usage:
#
#     scripts/push-clean.sh <sha> [<remote>]        # remote defaults to origin
#
# <sha> is anything `git rev-parse` resolves in the source repository, so
# `push-clean.sh HEAD` and `push-clean.sh 0f6dad8c` both work; the resolved
# commit is what gets pushed, never a branch name that could move underneath.
#
# The clone lives at $LEV_PUSH_TREE (default ~/.cache/leviculum/push-tree) and
# is reused across runs: it keeps its object store and its build cache, so the
# second push is minutes cheaper than the first. It is deliberately OUTSIDE the
# source tree — a clone inside it would show up as untracked files here and as
# a second copy of the workspace to every gate that walks the directory.
#
# Nothing here sets CARGO_TARGET_DIR. Left alone, the clone builds into its own
# `target/`; set it and the gates in the clone honour it, because since 0f6dad8c
# every script asks cargo where it writes instead of assuming <tree>/target.
set -euo pipefail

die() {
    echo "push-clean: $*" >&2
    exit 1
}

SHA_ARG="${1:-}"
REMOTE="${2:-origin}"
[ -n "$SHA_ARG" ] || die "usage: scripts/push-clean.sh <sha> [<remote>]"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)" ||
    die "not inside a git repository: $SCRIPT_DIR"

SHA="$(git -C "$SRC" rev-parse --verify --quiet "${SHA_ARG}^{commit}")" ||
    die "$SRC does not have a commit '$SHA_ARG'"

# The URL the SOURCE uses for <remote>, not whatever the clone's own remote
# happens to point at. The clone is made from the source, so without this its
# `origin` is the local repository — the third defect of the old recipe.
PUSH_URL="$(git -C "$SRC" remote get-url "$REMOTE" 2>/dev/null)" ||
    die "$SRC has no remote '$REMOTE'"

CLONE="${LEV_PUSH_TREE:-${XDG_CACHE_HOME:-$HOME/.cache}/leviculum/push-tree}"
case "$CLONE" in
    "$SRC" | "$SRC"/*) die "LEV_PUSH_TREE must be outside $SRC, got $CLONE" ;;
esac

if [ -e "$CLONE" ]; then
    git -C "$CLONE" rev-parse --git-dir >/dev/null 2>&1 ||
        die "$CLONE exists and is not a git repository; move it aside"
    # Refuse rather than clobber. The clone is ours to reuse, but a checkout
    # over someone's edits would destroy work with no way back, and a tree with
    # local changes is the exact state this whole guard exists to keep out of a
    # gate run.
    dirty="$(git -C "$CLONE" status --porcelain --untracked-files=no \
        --ignore-submodules=untracked)"
    if [ -n "$dirty" ]; then
        echo "push-clean: REFUSED — the push tree has uncommitted tracked changes:" >&2
        printf '%s\n' "$dirty" | sed 's|^|push-clean:   |' >&2
        echo "push-clean: $CLONE" >&2
        echo "push-clean: keep them or discard them, then run this again." >&2
        exit 1
    fi
else
    echo "push-clean: cloning $SRC -> $CLONE"
    mkdir -p "$(dirname "$CLONE")"
    # Cloned, not init+fetch: a clone from a local path hardlinks the object
    # store, which is 920 MB here and would otherwise be repacked and copied.
    #
    # The remote it creates is then REMOVED rather than renamed, because it
    # points at the source repository, and its refs/remotes/* entries would
    # answer the Claude guard's `--not --remotes` with the source's branches —
    # i.e. with commits the forge has never seen. Removing the remote takes
    # those refs (and the origin/HEAD symref) with it, so refs/remotes/ holds
    # only what the fetch from the forge below puts there.
    git clone --quiet --origin lev-push-clean-source "$SRC" "$CLONE"
    git -C "$CLONE" remote remove lev-push-clean-source
fi

git -C "$CLONE" remote get-url "$REMOTE" >/dev/null 2>&1 ||
    git -C "$CLONE" remote add "$REMOTE" "$PUSH_URL"
git -C "$CLONE" remote set-url "$REMOTE" "$PUSH_URL"

# The first defect, and the only one that made the tree LOOK gated: a clone
# takes no configuration from the repository it came from, so the hook in
# .githooks sat there unread. Set every run, not only on the fresh clone, so a
# push tree created before this script existed is repaired by using it.
git -C "$CLONE" config core.hooksPath .githooks

# Objects for <sha>, from the source. Into a private ref namespace on purpose:
# anything under refs/remotes/ is what the Claude guard means by `--not
# --remotes` when the forge does not have the branch yet, and filling it with
# the source's branches would narrow that range to nothing.
git -C "$CLONE" fetch --quiet --no-tags --force "$SRC" \
    "+refs/heads/*:refs/lev-push-clean/source/*" "+HEAD:refs/lev-push-clean/head"
git -C "$CLONE" cat-file -e "${SHA}^{commit}" 2>/dev/null ||
    die "$SHA is on no branch of $SRC and could not be fetched"

# And the forge's side, so refs/remotes/<remote>/* says what the forge really
# has. The Claude guard diffs against the remote sha git reports for the ref;
# stale remote-tracking refs make it judge the wrong range.
git -C "$CLONE" fetch --quiet --no-tags --prune "$REMOTE" ||
    die "could not fetch $REMOTE ($PUSH_URL) into $CLONE"

git -C "$CLONE" checkout --quiet --detach "$SHA"

# Second defect: `just fast` starts with check-submodules, which fails on a
# clone where nothing initialised them.
#
# Each submodule is pointed at the source's checkout first, where the objects
# already are — the vendored references are hundreds of megabytes over the
# network and the same commits sit one directory away. A submodule the source
# has not initialised keeps its .gitmodules URL and is cloned normally.
if [ -f "$CLONE/.gitmodules" ]; then
    while read -r key path; do
        name="${key#submodule.}"
        name="${name%.path}"
        [ -e "$SRC/$path/.git" ] || continue
        git -C "$CLONE" config "submodule.$name.url" "$SRC/$path"
    done < <(git -C "$CLONE" config -f "$CLONE/.gitmodules" \
        --get-regexp '^submodule\..*\.path$' || true)
fi
#
# `protocol.file.allow=always` is needed for exactly that rewrite: git has
# refused the file transport for submodules since CVE-2022-39253, where the
# URL comes from a .gitmodules an attacker may have written. Here the path is
# one this script computed from the repository it is already reading, so the
# file it protects against does not get a vote.
# (`init.defaultBranch`: each submodule clone otherwise prints the twelve-line
#  "you are using 'master' as the name for the initial branch" notice, twice as
#  much output as everything else this script says put together. The value is
#  irrelevant — every submodule ends up detached at its gitlink — only the
#  variable being set at all is, which is what silences the notice.)
git -C "$CLONE" -c protocol.file.allow=always -c init.defaultBranch=master \
    submodule update --init --recursive --quiet

echo "push-clean: pushing $SHA to $PUSH_URL (as $REMOTE) from $CLONE"
git -C "$CLONE" push "$REMOTE" "$SHA:refs/heads/master"
