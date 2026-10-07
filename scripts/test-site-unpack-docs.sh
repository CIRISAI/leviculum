#!/usr/bin/env bash
# Fixture test for packaging/site/lev-unpack-docs (Codeberg #260).
#
# That script turns the nightly's book tarball, as lev-receive-nightly
# published it under releases/nightly/latest/, into what
# https://leviculum.network/docs/ serves. It runs on the VPS, on a file that
# arrived from the network, and writes under the web root, so its refusals
# are the feature, and each one is made to fire here against the real
# script with the real tar.
#
# The cases, each against a releases root shaped like lev-receive-nightly
# leaves it:
#
#   good-book         a well-formed book: docs -> docs.<digest12>, readable
#   unchanged         the same book again: a no-op that says so
#   second-book       a new book swaps in, the old directory goes
#   bad-checksum      the tarball does not match its .sha256
#   wrong-name        the .sha256 names another file
#   no-tarball        nothing under nightly/latest/
#   outside-docs      a member not under docs/
#   dotdot            a member docs/../x
#   absolute          a member /x
#   symlink-member    a symlink member
#   dir-member        a directory member (the builder writes files only)
#   hardlink-member   a hard-link member
#   no-index          a book without docs/index.html
#   real-docs-dir     a hand-made docs/ directory is not replaced
#
# Every refusal asserts afterwards that it failed with one line on stderr,
# that `docs` still resolves to the previous book, that no new docs.* tree
# appeared and that no docs.new was left behind.
#
# UNPACK overrides the script under test, which is how a pre-fix copy is
# checked to be red.
#
# Usage: bash scripts/test-site-unpack-docs.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
UNPACK="${UNPACK:-$REPO_ROOT/packaging/site/lev-unpack-docs}"
[ -f "$UNPACK" ] || { echo "no script under test at $UNPACK"; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

failures=0
fail() { echo "  FAIL: $*"; failures=$((failures + 1)); }

NAME="leviculum-docs-nightly.tar.gz"
WEB="$WORK/www"
REL="$WEB/releases"
mkdir -p "$REL/nightly"

# Publishes <tarball> as build <id> and points latest at it, the way
# lev-receive-nightly leaves the tree. <sha-mode>: ok | bad | wrong-name.
publish() { # <id> <tarball> [sha-mode]
    local id="$1" tarball="$2" mode="${3:-ok}"
    local dir="$REL/nightly/$id"
    mkdir -p "$dir"
    cp "$tarball" "$dir/$NAME"
    (cd "$dir" && sha256sum "$NAME" >"$NAME.sha256")
    case "$mode" in
    bad) echo "tampered" >>"$dir/$NAME" ;;
    wrong-name) sed -i "s/ ${NAME}\$/ other.tar.gz/" "$dir/$NAME.sha256" ;;
    esac
    ln -sfn "$id" "$REL/nightly/latest"
}

# A book in the builder's layout: files only, under docs/, sorted.
make_book() { # <out.tar.gz> <marker>
    local out="$1" marker="$2" src="$WORK/src.$RANDOM"
    mkdir -p "$src/docs/css" "$src/docs/concepts"
    echo "<html>${marker}</html>" >"$src/docs/index.html"
    echo "body{}" >"$src/docs/css/general.css"
    echo "<html>concept ${marker}</html>" >"$src/docs/concepts/a.html"
    : >"$src/docs/.nojekyll"
    (cd "$src" && printf '%s\n' docs/.nojekyll docs/concepts/a.html docs/css/general.css docs/index.html |
        tar --no-recursion -czf "$out" -T -)
    rm -rf "$src"
}

run() {
    RC=0
    RELEASES_ROOT="$REL" bash "$UNPACK" >"$WORK/out" 2>"$WORK/err" || RC=$?
}

dump() {
    echo "  --- rc=$RC"
    echo "  --- stdout ---"; sed 's/^/    /' "$WORK/out"
    echo "  --- stderr ---"; sed 's/^/    /' "$WORK/err"
    echo "  --- web root ---"; (cd "$WEB" && ls -la .) | sed 's/^/    /'
}

served() { cat "$WEB/docs/index.html" 2>/dev/null; }
trees() { (cd "$WEB" && for d in docs.*; do [ -d "$d" ] && [ ! -L "$d" ] && echo "$d"; done) | tr '\n' ' '; }

assert_refused() { # <label> <expected-served> <expected-trees>
    local label="$1" want="$2" want_trees="$3"
    [ "$RC" -ne 0 ] || fail "$label: exit 0, expected a refusal"
    local lines
    lines="$(wc -l <"$WORK/err")"
    [ "$lines" -eq 1 ] || fail "$label: expected one line on stderr, got ${lines}"
    [ "$(served)" = "$want" ] || fail "$label: docs serves '$(served)', expected '${want}'"
    [ "$(trees)" = "$want_trees" ] || fail "$label: trees are '$(trees)', expected '${want_trees}'"
    [ ! -e "$WEB/docs.new" ] || fail "$label: left docs.new behind"
}

# Builds a tarball from a src dir with arbitrary extra tar options.
raw_tar() { # <out> <src-dir> <tar args...>
    local out="$1" src="$2"
    shift 2
    (cd "$src" && tar -czf "$out" "$@") 2>/dev/null
}

# --- good-book ------------------------------------------------------------
echo "[case] good-book"
before=$failures
make_book "$WORK/book1.tgz" one
publish b1 "$WORK/book1.tgz"
run
[ "$RC" -eq 0 ] || fail "good-book: exit $RC, expected 0"
[ -L "$WEB/docs" ] || fail "good-book: docs is not a symlink"
[[ "$(readlink "$WEB/docs" 2>/dev/null)" =~ ^docs\.[0-9a-f]{12}$ ]] ||
    fail "good-book: docs points at '$(readlink "$WEB/docs" 2>/dev/null)'"
[ "$(served)" = "<html>one</html>" ] || fail "good-book: docs serves '$(served)'"
[ -f "$WEB/docs/.nojekyll" ] || fail "good-book: the dot file did not land"
perms="$(stat -c '%a' "$WEB/docs/css/general.css" 2>/dev/null)"
[ "$perms" = "644" ] || fail "good-book: a file has mode ${perms}, expected 644"
[ ! -e "$WEB/docs.new" ] || fail "good-book: left docs.new behind"
TREES1="$(trees)"
[ "$(echo "$TREES1" | wc -w)" -eq 1 ] || fail "good-book: trees are '${TREES1}'"
[ "$failures" -eq "$before" ] || dump

# --- unchanged ------------------------------------------------------------
echo "[case] unchanged"
before=$failures
run
[ "$RC" -eq 0 ] || fail "unchanged: exit $RC, expected 0"
grep -q 'unchanged' "$WORK/out" || fail "unchanged: did not say the book is unchanged"
[ "$(trees)" = "$TREES1" ] || fail "unchanged: trees are '$(trees)', expected '${TREES1}'"
[ "$failures" -eq "$before" ] || dump

# --- second-book ----------------------------------------------------------
echo "[case] second-book"
before=$failures
make_book "$WORK/book2.tgz" two
publish b2 "$WORK/book2.tgz"
run
[ "$RC" -eq 0 ] || fail "second-book: exit $RC, expected 0"
[ "$(served)" = "<html>two</html>" ] || fail "second-book: docs serves '$(served)'"
TREES2="$(trees)"
if [ "$(echo "$TREES2" | wc -w)" -ne 1 ] || [ "$TREES2" = "$TREES1" ]; then
    fail "second-book: trees are '${TREES2}', expected one new tree in place of '${TREES1}'"
fi
[ "$failures" -eq "$before" ] || dump

GOOD="<html>two</html>"

# --- checksum refusals ----------------------------------------------------
echo "[case] bad-checksum"
before=$failures
make_book "$WORK/book3.tgz" three
publish b3 "$WORK/book3.tgz" bad
run
assert_refused bad-checksum "$GOOD" "$TREES2"
grep -q 'checksum mismatch' "$WORK/err" || fail "bad-checksum: wrong reason: $(cat "$WORK/err")"
[ "$failures" -eq "$before" ] || dump

echo "[case] wrong-name"
before=$failures
publish b4 "$WORK/book3.tgz" wrong-name
run
assert_refused wrong-name "$GOOD" "$TREES2"
grep -q "names 'other.tar.gz'" "$WORK/err" || fail "wrong-name: wrong reason: $(cat "$WORK/err")"
[ "$failures" -eq "$before" ] || dump

echo "[case] no-tarball"
before=$failures
mkdir -p "$REL/nightly/b5"
ln -sfn b5 "$REL/nightly/latest"
run
assert_refused no-tarball "$GOOD" "$TREES2"
[ "$failures" -eq "$before" ] || dump

# --- member refusals ------------------------------------------------------
SRC="$WORK/raw"
mkdir -p "$SRC/docs" "$SRC/other"
echo "<html>evil</html>" >"$SRC/docs/index.html"
echo "x" >"$SRC/other/x"
echo "x" >"$SRC/x"
ln -s /etc/passwd "$SRC/docs/evil"
ln "$SRC/docs/index.html" "$SRC/docs/hard.html"

refuse_case() { # <label> <id> <reason-substring> <tar args...>
    local label="$1" id="$2" reason="$3"
    shift 3
    echo "[case] $label"
    before=$failures
    raw_tar "$WORK/$label.tgz" "$SRC" "$@" ||
        { fail "$label: could not build the fixture tarball"; return; }
    publish "$id" "$WORK/$label.tgz"
    run
    assert_refused "$label" "$GOOD" "$TREES2"
    grep -q -- "$reason" "$WORK/err" || fail "$label: wrong reason: $(cat "$WORK/err")"
    [ "$failures" -eq "$before" ] || dump
}

refuse_case outside-docs r1 'outside docs/' --no-recursion docs/index.html other/x
refuse_case dotdot r2 'outside docs/' --no-recursion --transform 's,^x$,docs/../x,' docs/index.html x
refuse_case absolute r3 'outside docs/' --no-recursion -P --transform 's,^x$,/x,' docs/index.html x
refuse_case symlink-member r4 'not a regular file' --no-recursion docs/index.html docs/evil
refuse_case dir-member r5 'not a regular file' docs/index.html docs
refuse_case hardlink-member r6 'not a regular file' --no-recursion docs/index.html docs/hard.html
rm -f "$SRC/docs/evil"
mv "$SRC/docs/index.html" "$SRC/docs/other.html"
refuse_case no-index r7 'no docs/index.html' --no-recursion docs/other.html

# --- real-docs-dir --------------------------------------------------------
echo "[case] real-docs-dir"
before=$failures
publish b8 "$WORK/book3.tgz"
saved="$(readlink "$WEB/docs")"
rm "$WEB/docs"
mkdir "$WEB/docs"
echo "hand-made" >"$WEB/docs/index.html"
run
[ "$RC" -ne 0 ] || fail "real-docs-dir: exit 0, expected a refusal"
[ "$(served)" = "hand-made" ] || fail "real-docs-dir: the hand-made directory was touched"
grep -q 'not a symlink' "$WORK/err" || fail "real-docs-dir: wrong reason: $(cat "$WORK/err")"
rm -rf "$WEB/docs"
ln -s "$saved" "$WEB/docs"
[ "$failures" -eq "$before" ] || dump

echo
if [ "$failures" -ne 0 ]; then
    echo "test-site-unpack-docs: FAILED ($failures assertion(s))"
    exit 1
fi
echo "test-site-unpack-docs: all cases passed"
