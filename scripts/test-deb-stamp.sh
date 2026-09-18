#!/usr/bin/env bash
#
# Selftest for scripts/deb-stamp.sh, driven against a fixture workspace.
#
# The script states two things that are only true if they are measured, and
# both of them are invisible until a package is already on somebody's disk:
#
#   1. A pre-release version has to change spelling on the way into a Debian
#      version. Cargo writes `0.10.0-dev`; Debian reads the `-` as the start
#      of a Debian revision and sorts the result ABOVE `0.10.0`, so a tree in
#      the development window would advertise itself as NEWER than the release
#      it is working towards, and an upgrade path would lie. The translation
#      to `~` is what makes it sort below. Both comparisons are run here, the
#      wrong spelling included, so the guard has a positive control rather
#      than a belief.
#
#   2. The build id carries the commit distance from the last `v*` tag as
#      semver build metadata. Derived from `git describe`, which means it
#      quietly disappears in a clone that has no tags — so the tagless case
#      is a case here, not a hope.
#
# ~2 s, no network, no build: four empty crates and `cargo pkgid`.
#
# Usage: bash scripts/test-deb-stamp.sh

set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

failures=0
fail() { echo "  FAIL: $*"; failures=$((failures + 1)); }
eq() { # <what> <got> <want>
    [ "$2" = "$3" ] || fail "$1 is '$2', expected '$3'"
}

# Offline throughout: the fixture crates have no dependencies, so nothing
# should reach for the network, and a run that does is a bug in this test.
export CARGO_NET_OFFLINE=true

# --- The fixture ----------------------------------------------------------
#
# A workspace holding the four crates deb-stamp asks cargo about, with
# leviculum-cli in a development window and the other three on plain
# release versions. deb-stamp resolves ROOT from its own path, so it is
# copied in beside the helper it sources, exactly as the real tree has them.
setup() { # <tag-mode: tagged | tagless>
    TREE="$WORK/${1}"
    mkdir -p "$TREE/scripts"
    cp "$REPO/scripts/deb-stamp.sh" "$REPO/scripts/cargo-target-dir.sh" "$TREE/scripts/"

    cat > "$TREE/Cargo.toml" <<'EOF'
[workspace]
resolver = "2"
members = ["leviculum-cli", "lnomad", "lblogd", "lnpnd"]
EOF
    local crate version
    for crate in leviculum-cli:0.10.0-dev lnomad:1.2.3 lblogd:0.4.0 lnpnd:0.2.0; do
        version="${crate##*:}"
        crate="${crate%%:*}"
        mkdir -p "$TREE/$crate/src"
        cat > "$TREE/$crate/Cargo.toml" <<EOF
[package]
name = "$crate"
version = "$version"
edition = "2021"
EOF
        : > "$TREE/$crate/src/lib.rs"
    done
    printf '/target/\n' > "$TREE/.gitignore"

    # `cargo pkgid` refuses to answer without a lockfile. The crates have no
    # dependencies, so resolving one touches nothing outside the fixture.
    ( cd "$TREE" && cargo generate-lockfile ) >/dev/null 2>&1 \
        || { echo "could not resolve the fixture workspace"; exit 1; }

    (
        cd "$TREE" || exit 1
        git init -q .
        git config user.name fixture
        git config user.email fixture@example.invalid
        git add -A
        git commit -q -m "fixture"
        if [ "$1" = tagged ]; then
            git tag v0.9.0
            # Three commits past the tag, which is the number the build id
            # must carry.
            local i
            for i in 1 2 3; do
                git commit -q --allow-empty -m "past the tag $i"
            done
        fi
    ) >/dev/null 2>&1 || { echo "could not build the fixture repo"; exit 1; }
}

run_stamp() {
    ( cd "$TREE" && bash scripts/deb-stamp.sh ) > "$WORK/stamp.out" 2>&1
}

dump() { echo "--- deb-stamp output ---"; cat "$WORK/stamp.out"; echo "---"; }

# --- Case: a tagged tree in a development window ---------------------------
echo "[case] tagged"
before=$failures
setup tagged
run_stamp || { echo "deb-stamp.sh exited non-zero"; dump; failures=$((failures + 1)); }

sha7="$(cd "$TREE" && git rev-parse HEAD | cut -c1-7)"
date_u="$(date -u +%Y%m%d)"

eq ".build-id" "$(cat "$TREE/.build-id" 2>/dev/null)" "nightly.${date_u}-${sha7}+3"
eq ".deb-version-leviculum-cli" \
    "$(cat "$TREE/.deb-version-leviculum-cli" 2>/dev/null)" \
    "0.10.0~dev~nightly.${date_u}.${sha7}"
eq ".deb-version-lnomad" \
    "$(cat "$TREE/.deb-version-lnomad" 2>/dev/null)" \
    "1.2.3~nightly.${date_u}.${sha7}"
eq ".deb-version-lnpnd" \
    "$(cat "$TREE/.deb-version-lnpnd" 2>/dev/null)" \
    "0.2.0~nightly.${date_u}.${sha7}"

# The Debian changelog is stamped from the same string, under the BINARY
# package name (leviculum-cli ships as "leviculum").
changelog="$TREE/target/deb-changelog/leviculum"
if [ -r "$changelog" ]; then
    eq "deb changelog top line" \
        "$(head -1 "$changelog")" \
        "leviculum (0.10.0~dev~nightly.${date_u}.${sha7}) unstable; urgency=medium"
else
    fail "no Debian changelog at target/deb-changelog/leviculum"
fi

# The run says the distance in words, so a human reading CI output sees it.
grep -q "distance=3 commit(s) past v0.9.0" "$WORK/stamp.out" \
    || { fail "deb-stamp does not report the distance and its base tag"; dump; }

[ "$failures" -eq "$before" ] || dump

# --- The ordering claim, run rather than reasoned --------------------------
#
# This is the assertion the whole `~` translation exists for, and the wrong
# spelling is checked alongside the right one: a guard that only ever sees
# the passing case cannot say whether it would notice the failing one.
echo "[case] debian-ordering"
if command -v dpkg >/dev/null 2>&1; then
    stamped="0.10.0~dev~nightly.${date_u}.${sha7}"
    dpkg --compare-versions "$stamped" lt "0.10.0" \
        || fail "the stamped pre-release ${stamped} does not sort below 0.10.0"
    dpkg --compare-versions "$stamped" gt "0.9.0" \
        || fail "the stamped pre-release ${stamped} does not sort above 0.9.0"
    # The positive control: the untranslated spelling really is the bug.
    dpkg --compare-versions "0.10.0-dev~nightly.${date_u}.${sha7}" gt "0.10.0" \
        || fail "the untranslated spelling no longer sorts above 0.10.0 — this test's premise is stale, check it against dpkg before trusting the translation"
    # A release version is untouched and still upgrades out of its nightlies.
    dpkg --compare-versions "1.2.3~nightly.${date_u}.${sha7}" lt "1.2.3" \
        || fail "a release nightly no longer sorts below its release"
else
    echo "  skip: dpkg not installed"
fi

# --- Case: no v* tag reachable --------------------------------------------
#
# A shallow or tagless clone has nothing to measure from. The build id must
# lose the metadata and nothing else, and the script must still succeed:
# the nightly cannot fail because somebody cloned without tags.
echo "[case] tagless"
before=$failures
setup tagless
run_stamp || { fail "deb-stamp.sh exited non-zero on a tagless clone"; dump; }

sha7="$(cd "$TREE" && git rev-parse HEAD | cut -c1-7)"
eq ".build-id (tagless)" "$(cat "$TREE/.build-id" 2>/dev/null)" "nightly.${date_u}-${sha7}"
eq ".deb-version-leviculum-cli (tagless)" \
    "$(cat "$TREE/.deb-version-leviculum-cli" 2>/dev/null)" \
    "0.10.0~dev~nightly.${date_u}.${sha7}"
grep -q "distance=unknown" "$WORK/stamp.out" \
    || { fail "a tagless clone does not say the distance is unknown"; dump; }

[ "$failures" -eq "$before" ] || dump

echo
if [ "$failures" -eq 0 ]; then
    echo "test-deb-stamp: all cases passed"
    exit 0
fi
echo "test-deb-stamp: FAILED (${failures} assertion(s))"
exit 1
