#!/usr/bin/env bash
# Fixture test for the nightly package step: scripts/collect-nightly-debs.sh
# run in an environment shaped like the container it actually runs in.
#
# The step has had no coverage at all, and that is why the rolling nightly
# release stood still from 2026-08-24 to 2026-09-18 without anything saying
# so. `package` and `publish` carry `when: event: cron` in
# .woodpecker/nightly.yml, so no push gate reaches them; a commit that breaks
# one is green everywhere a human looks and red only at 02:00, in a log
# nobody reads. Commit 03e2cb95 replaced a hardcoded target/ with a
# `cargo metadata` call — right where cargo exists, impossible in the
# debian:bookworm-slim image `package` runs in — and every nightly since
# died with `cargo: command not found`.
#
# So the environment is the assertion. The script runs with a PATH holding
# ONLY the tools bookworm-slim ships plus the git that step apt-installs:
#
#   bash coreutils(cat cp head ls mkdir rm sha256sum) dirname sed tar gzip git
#
# There is no cargo on it, no python3, no curl and no jq, and a symlink farm
# is how that is proven rather than asserted — a future commit that reaches
# for any of them fails here, not in five weeks. The tree the script is
# copied into deliberately has NO scripts/cargo-target-dir.sh either: that
# helper's answer is written forward by scripts/deb-stamp.sh, in the
# rust:bookworm step, and a `source` line reintroduced here cannot survive.
#
# The cargo target directory the fixture points at lies OUTSIDE the tree, as
# it does whenever CARGO_TARGET_DIR is set (the nightly's fresh-tree wrapper
# sets it so the cache outlives the clone). A tree-local decoy .deb sits at
# <tree>/target/debian/ so that "read the stamp" and "assume $ROOT/target"
# produce different bytes in dist/, and the assertion can tell them apart.
#
# COLLECT_SH overrides the script under test, which is how the pre-fix
# version is checked to be red:
#   COLLECT_SH=<old copy> bash scripts/test-collect-nightly-debs.sh
#
# Usage: bash scripts/test-collect-nightly-debs.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
COLLECT_SH="${COLLECT_SH:-$SCRIPT_DIR/collect-nightly-debs.sh}"
[ -f "$COLLECT_SH" ] || { echo "no script under test at $COLLECT_SH"; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

failures=0
fail() { echo "  FAIL: $*"; failures=$((failures + 1)); }

# --- The PATH the slim image gives it -------------------------------------
#
# Every tool the script may use, and nothing else. `command -v` resolves
# against the real PATH here; the farm is what the script under test sees.
TOOLS=(bash cat cp dirname git gzip head ls mkdir rm sed sha256sum tar)
BIN="$WORK/bin"
mkdir -p "$BIN"
for t in "${TOOLS[@]}"; do
    real="$(command -v "$t")" || { echo "missing tool on this host: $t"; exit 1; }
    ln -s "$real" "$BIN/$t"
done

# The premise of the whole file: nothing the fix removed is reachable.
for forbidden in cargo python3 curl jq; do
    if PATH="$BIN" command -v "$forbidden" >/dev/null 2>&1; then
        fail "sandbox PATH still resolves $forbidden — the test proves nothing"
    fi
done

# --- The fixture tree -----------------------------------------------------
#
# A repo-shaped directory holding the script under test, the files it copies
# into the tarballs, the stamp files scripts/deb-stamp.sh would have left,
# and a git repo for `git archive HEAD`.
VER_CLI="0.9.1"
VER_LNOMAD="1.2.3"
VER_LBLOGD="0.4.0"
# lnpnd stands in for a package built inside a development window. Cargo
# spells that version `0.2.0-dev`; deb-stamp stamps it in Debian's spelling
# (`0.2.0~dev`, see the note there), and the tarball has to read it back as
# the semver it came from. Against a strip that cuts at the first `~` this
# comes out "0.2.0" — a tarball claiming a release that was never cut.
VER_LNPND="0.2.0-dev"
DEB_LNPND="0.2.0~dev"
NIGHTLY_SUFFIX="~nightly.20260918.abc1234"
LNFLASH_VERSION="7.7.7"
# A compiler version that exists nowhere: the VERSION file must carry what the
# stamp says, not what the host running this test happens to have installed.
FIXTURE_RUSTC="rustc 9.9.9 (fixturec0de 2026-01-01)"

BINS_AMD=(lnsd lnstest lncp lnstatus lnprobe lnpath lnomad lblogd lnpnd)

setup() {  # <case> [stamp-mode: ok | missing | bogus | empty-target] [rustc-stamp: yes | no]
    local case_name="$1" stamp_mode="${2:-ok}" rustc_stamp="${3:-yes}"
    TREE="$WORK/$case_name/tree"
    EXT_TARGET="$WORK/$case_name/cargo-target"
    mkdir -p "$TREE/scripts" "$TREE/lnomad" "$TREE/lblogd" \
        "$TREE/target/lnflash" "$TREE/target/debian" \
        "$EXT_TARGET/debian"

    cp "$COLLECT_SH" "$TREE/scripts/collect-nightly-debs.sh"

    # Files the tarballs carry, each with content naming itself so a mixed-up
    # copy is visible rather than merely present.
    echo "fixture README" > "$TREE/README.md"
    echo "fixture lnomad README" > "$TREE/lnomad/README.md"
    echo "fixture lblogd README" > "$TREE/lblogd/README.md"
    echo "fixture LICENSE" > "$TREE/LICENSE"
    echo "fixture THIRD-PARTY-NOTICES" > "$TREE/THIRD-PARTY-NOTICES"
    echo "fixture CHANGELOG" > "$TREE/CHANGELOG.md"
    cat > "$TREE/Cargo.toml" <<EOF
[workspace.package]
version = "${LNFLASH_VERSION}"
EOF
    printf '/target/\n/dist/\n' > "$TREE/.gitignore"

    # What scripts/deb-stamp.sh leaves behind.
    echo "${VER_CLI}${NIGHTLY_SUFFIX}" > "$TREE/.deb-version-leviculum-cli"
    echo "${VER_LNOMAD}${NIGHTLY_SUFFIX}" > "$TREE/.deb-version-lnomad"
    echo "${VER_LBLOGD}${NIGHTLY_SUFFIX}" > "$TREE/.deb-version-lblogd"
    echo "${DEB_LNPND}${NIGHTLY_SUFFIX}" > "$TREE/.deb-version-lnpnd"
    echo "nightly.20260918-abc1234" > "$TREE/.build-id"
    # Which compiler built the binaries, stamped in the rust:bookworm step
    # because this one has no rustc to ask (Codeberg #305).
    [ "$rustc_stamp" = yes ] && echo "$FIXTURE_RUSTC" > "$TREE/.rustc-version"
    case "$stamp_mode" in
    ok | empty-target) echo "$EXT_TARGET" > "$TREE/.cargo-target-dir" ;;
    missing) : ;;
    bogus) echo "$WORK/$case_name/nowhere" > "$TREE/.cargo-target-dir" ;;
    esac

    # The artefacts cargo and cargo-deb wrote, in the external target dir.
    if [ "$stamp_mode" != empty-target ]; then
        local pkg arch triple
        for pkg in leviculum lnomad lblogd lnpnd; do
            for arch in amd64 arm64; do
                echo "real ${pkg} ${arch} deb" \
                    > "$EXT_TARGET/debian/${pkg}_1.0.0${NIGHTLY_SUFFIX}_${arch}.deb"
            done
        done
        for triple in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
            mkdir -p "$EXT_TARGET/$triple/release"
            for b in "${BINS_AMD[@]}"; do
                echo "real ${b} ${triple}" > "$EXT_TARGET/$triple/release/$b"
            done
        done
    fi

    # The decoys: a tree-local target/ of the shape the pre-03e2cb95 script
    # assumed. Nothing in dist/ may come from here.
    local arch
    for arch in amd64 arm64; do
        echo "DECOY leviculum ${arch} deb" \
            > "$TREE/target/debian/leviculum_0.0.0_${arch}.deb"
    done
    mkdir -p "$TREE/target/x86_64-unknown-linux-musl/release"
    echo "DECOY lnsd" > "$TREE/target/x86_64-unknown-linux-musl/release/lnsd"

    # The lnflash bundle is the one artefact that is NOT under the cargo
    # target directory — it is the bundle script's own product and stays
    # repo-relative, so the tree-local copy here is the real one.
    echo "real lnflash bundle" \
        > "$TREE/target/lnflash/lnflash-${LNFLASH_VERSION}.tar.gz"

    ( cd "$TREE" \
        && git init -q . \
        && git add -A \
        && git -c user.name=fixture -c user.email=fixture@example.invalid \
            commit -q -m "fixture" ) >/dev/null 2>&1 \
        || { echo "could not build the fixture git repo"; exit 1; }
}

run_collect() {
    ( cd "$TREE" && PATH="$BIN" LEVICULUM_BUILD_ID="fixture-build" \
        bash "$TREE/scripts/collect-nightly-debs.sh" ) > "$TREE/../out" 2>&1
    echo $? > "$TREE/../rc"
}

rc() { cat "$TREE/../rc"; }
out() { cat "$TREE/../out"; }
dumpout() { echo "  --- script output ---"; sed 's/^/    /' "$TREE/../out"; }

# --- Case: the packaging step has no cargo --------------------------------
echo "[case] no-cargo"
before=$failures
setup no-cargo
run_collect
[ "$(rc)" = "0" ] || fail "exit $(rc) with cargo off the PATH, expected 0"

# Exactly these files, no more and no fewer: a silently dropped asset is a
# 404 under a URL the README hardcodes.
expected="$WORK/expected"
: > "$expected"
for pkg in leviculum lnomad lblogd lnpnd; do
    for arch in amd64 arm64; do
        printf '%s\n' "${pkg}-nightly-${arch}.deb" "${pkg}-nightly-${arch}.deb.sha256" \
            "${pkg}-nightly-${arch}.tar.gz" "${pkg}-nightly-${arch}.tar.gz.sha256" >> "$expected"
    done
done
printf '%s\n' lnflash-nightly-amd64.tar.gz lnflash-nightly-amd64.tar.gz.sha256 \
    leviculum-nightly-source.tar.gz leviculum-nightly-source.tar.gz.sha256 >> "$expected"
sort -o "$expected" "$expected"
actual="$(find "$TREE/dist" -maxdepth 1 -type f -printf '%f\n' 2>/dev/null | sort)"
if [ "$actual" != "$(cat "$expected")" ]; then
    fail "dist/ holds:"
    diff <(echo "$actual") "$expected" | sed 's/^/    /'
fi

# The stamp was followed, not the assumption: the bytes in dist/ are the ones
# from the external target directory, not from the tree-local decoy.
for arch in amd64 arm64; do
    got="$(cat "$TREE/dist/leviculum-nightly-${arch}.deb" 2>/dev/null)"
    [ "$got" = "real leviculum ${arch} deb" ] \
        || fail "dist/leviculum-nightly-${arch}.deb holds '${got}' — the decoy under the tree's own target/ was collected"
done

# Every .sha256 describes the file beside it.
( cd "$TREE/dist" && sha256sum -c ./*.sha256 ) >/dev/null 2>&1 \
    || fail "a .sha256 does not match its asset"

# The leviculum tarball: the binaries from the external target dir, the docs
# from the tree, and a VERSION naming the stamped version and build id.
stage="$WORK/unpack"
rm -rf "$stage"; mkdir -p "$stage"
tar -C "$stage" -xzf "$TREE/dist/leviculum-nightly-amd64.tar.gz" 2>/dev/null \
    || fail "leviculum-nightly-amd64.tar.gz is not a readable gzip tarball"
lev="$stage/leviculum-nightly-amd64"
for b in lnsd lnstest lncp lnstatus lnprobe lnpath; do
    [ -f "$lev/bin/$b" ] || fail "leviculum tarball is missing bin/$b"
done
got="$(cat "$lev/bin/lnsd" 2>/dev/null)"
[ "$got" = "real lnsd x86_64-unknown-linux-musl" ] \
    || fail "leviculum tarball carries '${got}' as bin/lnsd — the tree-local decoy"
for d in README.md LICENSE THIRD-PARTY-NOTICES CHANGELOG.md; do
    [ -f "$lev/doc/$d" ] || fail "leviculum tarball is missing doc/$d"
done
grep -q "^version: ${VER_CLI}\$" "$lev/VERSION" 2>/dev/null \
    || fail "leviculum VERSION does not name version ${VER_CLI}"
grep -q "^build-id: fixture-build\$" "$lev/VERSION" 2>/dev/null \
    || fail "leviculum VERSION does not name the build id"
grep -qxF "rustc: ${FIXTURE_RUSTC}" "$lev/VERSION" 2>/dev/null \
    || fail "leviculum VERSION does not name the compiler that built it: $(sed -n '/^rustc/p' "$lev/VERSION" 2>/dev/null)"

# lnomad is versioned independently and must not carry the stack's changelog.
rm -rf "$stage"; mkdir -p "$stage"
tar -C "$stage" -xzf "$TREE/dist/lnomad-nightly-arm64.tar.gz" 2>/dev/null \
    || fail "lnomad-nightly-arm64.tar.gz is not a readable gzip tarball"
lno="$stage/lnomad-nightly-arm64"
[ -f "$lno/doc/CHANGELOG.md" ] && fail "lnomad tarball carries the leviculum CHANGELOG"
grep -q "^version: ${VER_LNOMAD}\$" "$lno/VERSION" 2>/dev/null \
    || fail "lnomad VERSION does not name its own version ${VER_LNOMAD}"
got="$(cat "$lno/doc/README.md" 2>/dev/null)"
[ "$got" = "fixture lnomad README" ] || fail "lnomad tarball carries the wrong README: '${got}'"

# lnpnd is the pre-release case: its stamp is Debian's `0.2.0~dev~nightly.…`
# and the VERSION file must name the semver `0.2.0-dev` it was built from.
rm -rf "$stage"; mkdir -p "$stage"
tar -C "$stage" -xzf "$TREE/dist/lnpnd-nightly-amd64.tar.gz" 2>/dev/null \
    || fail "lnpnd-nightly-amd64.tar.gz is not a readable gzip tarball"
lpn="$stage/lnpnd-nightly-amd64"
grep -q "^version: ${VER_LNPND}\$" "$lpn/VERSION" 2>/dev/null \
    || fail "lnpnd VERSION says '$(sed -n 's/^version: //p' "$lpn/VERSION" 2>/dev/null)', not the pre-release ${VER_LNPND} it was built from"

# The lnflash bundle is renamed, not rebuilt.
got="$(cat "$TREE/dist/lnflash-nightly-amd64.tar.gz" 2>/dev/null)"
[ "$got" = "real lnflash bundle" ] || fail "lnflash asset holds '${got}'"

# The source tarball is tracked files at HEAD, under one prefix, with no
# build output in it.
names="$(tar tzf "$TREE/dist/leviculum-nightly-source.tar.gz" 2>/dev/null)"
echo "$names" | grep -q '^leviculum-nightly-source/README.md$' \
    || fail "source tarball does not hold leviculum-nightly-source/README.md"
echo "$names" | grep -q 'target/' \
    && fail "source tarball carries build output from target/"

[ "$failures" -eq "$before" ] || dumpout

# --- Case: no compiler was stamped ----------------------------------------
#
# An older tree, or a build whose stamp step predates Codeberg #305, has no
# .rustc-version. That is not worth failing a publish over — but the VERSION
# file must say the compiler is unknown rather than carry an empty field that
# reads like an answer.
echo "[case] no-rustc-stamp"
before=$failures
setup no-rustc-stamp ok no
run_collect
[ "$(rc)" = "0" ] || fail "exit $(rc) with no .rustc-version, expected 0"
rm -rf "$stage"; mkdir -p "$stage"
tar -C "$stage" -xzf "$TREE/dist/leviculum-nightly-amd64.tar.gz" 2>/dev/null \
    || fail "leviculum-nightly-amd64.tar.gz is not a readable gzip tarball"
grep -qxF "rustc: unknown" "$stage/leviculum-nightly-amd64/VERSION" 2>/dev/null \
    || fail "an unstamped compiler is not reported as unknown: $(sed -n '/^rustc/p' "$stage/leviculum-nightly-amd64/VERSION" 2>/dev/null)"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: nobody stamped the target directory ----------------------------
#
# The step cannot derive it — that is the whole point — so it must say which
# script was supposed to leave it rather than die in a glob.
echo "[case] stamp-missing"
before=$failures
setup stamp-missing missing
run_collect
[ "$(rc)" != "0" ] || fail "exit 0 with no .cargo-target-dir"
out | grep -q 'deb-stamp' || fail "the failure does not name scripts/deb-stamp.sh"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: the stamp names a directory that is not there ------------------
echo "[case] stamp-bogus"
before=$failures
setup stamp-bogus bogus
run_collect
[ "$(rc)" != "0" ] || fail "exit 0 with .cargo-target-dir naming nothing"
out | grep -q 'cargo-target-dir' || fail "the failure does not name the stamp file"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: the build produced no packages ---------------------------------
#
# An empty target directory is the shape a failed build leaves behind, and
# publishing nothing is worse than failing here.
echo "[case] no-packages"
before=$failures
setup no-packages empty-target
run_collect
[ "$(rc)" != "0" ] || fail "exit 0 with no .deb under the target directory"
out | grep -q 'no leviculum .deb found' \
    || fail "the failure does not name the package it could not find"
[ "$failures" -eq "$before" ] || dumpout

echo
if [ "$failures" -ne 0 ]; then
    echo "test-collect-nightly-debs: FAILED ($failures assertion(s))"
    exit 1
fi
echo "test-collect-nightly-debs: all cases passed"
