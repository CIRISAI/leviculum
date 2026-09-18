#!/usr/bin/env bash
# Pin the build id and the per-package Debian versions for one nightly
# build, and persist them so every later step sees identical values.
#
# Why a script rather than inline commands: this used to be duplicated
# between .woodpecker/nightly.yml and the Justfile's _deb-stamp recipe,
# and the two drifted (the Justfile still built only the leviculum
# package long after CI had added lnomad). One implementation, two
# callers.
#
# Produces, in the repo root:
#   .build-id                    nightly.<UTCdate>-<sha7>[+<distance>]
#   .cargo-target-dir            absolute path cargo writes artefacts to
#   .deb-version-<crate>         <crate version>~nightly.<UTCdate>.<sha7>
#
# and, under target/deb-changelog/:
#   <binary package name>        a Debian-format changelog for that build
#
# The changelog is generated rather than committed because Debian policy
# wants its top entry to carry the package's own version, and these
# versions are stamped per build. A committed file would go stale on the
# first nightly and stay wrong.
#
# Both are gitignored. The build id is deliberately shared across all
# packages: it stamps *when and from what commit* a build came, which is
# one fact per run. The Debian versions are per package, because the
# packages are versioned independently — leviculum tracks the protocol
# stack, lblogd and lnomad track their own products.
#
# The `~` before "nightly" is not decoration: in Debian version ordering
# `~` sorts BEFORE the empty string, so 0.1.0~nightly.20260728.abc1234
# compares as older than a future release 0.1.0. Nightlies therefore
# upgrade cleanly into a release without an epoch bump.
#
# The same `~` is why a semver pre-release cannot be handed to Debian as
# it is spelled. Cargo spells the development window `0.10.0-dev`; Debian
# reads the `-` as the start of a Debian revision and sorts it ABOVE the
# release, which would make an upgrade path lie. Measured, not argued:
#
#   dpkg --compare-versions 0.10.0-dev gt 0.10.0  -> true
#   dpkg --compare-versions 0.10.0~dev lt 0.10.0  -> true
#
# deb_upstream() below translates the one `-` semver allows into `~`, so
# 0.10.0-dev packages as 0.10.0~dev~nightly.<date>.<sha7> and upgrades
# into 0.10.0 when it is cut. A release version holds no `-` and passes
# through untouched. scripts/test-deb-stamp.sh runs both comparisons.
#
# The build id additionally carries the commit distance from the last
# `v*` tag, as semver build metadata: "+37" reads as 37 commits past
# v0.9.0. It is derived from `git describe` rather than stated in a file
# because git already knows it — no file in the tree changes, no build
# cache is invalidated, and two branches cannot disagree about it. A
# clone with no reachable `v*` tag simply gets no metadata.
#
# Commit and date come from CI when present and from git/date otherwise,
# so a local `just build-deb` produces the same shape as CI.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Where cargo writes. Asked here, once, and written forward exactly like
# .build-id below, because the steps that CONSUME the artefacts cannot ask:
# .woodpecker/nightly.yml runs `package` and `publish` in debian:bookworm-slim,
# which has neither cargo nor the python3 that cargo-target-dir.sh parses the
# metadata with. Asking there is what broke every nightly from 2026-09-11 on
# ("cargo: command not found" in collect-nightly-debs.sh). This step runs in
# rust:bookworm, so the question is answerable here and nowhere later.
# shellcheck source-path=SCRIPTDIR/..
# shellcheck source=scripts/cargo-target-dir.sh
source "$ROOT/scripts/cargo-target-dir.sh"
CARGO_TARGET="$(cargo_target_dir "$ROOT")"

# Every crate that ships a Debian package. Keyed by crate name, which is
# what `cargo deb -p` and `cargo pkgid -p` both take; the resulting .deb
# may be named differently (leviculum-cli ships as "leviculum").
CRATES=(leviculum-cli lnomad lblogd lnpnd)

# The .deb is not always named after its crate: leviculum-cli ships as
# "leviculum". The changelog's first token must be the *binary package*
# name, so the mapping is spelled out here rather than assumed.
pkg_name() {
    case "$1" in
    leviculum-cli) echo leviculum ;;
    *) echo "$1" ;;
    esac
}

# Cargo's version as Debian must spell it. Semver allows exactly one `-`,
# the pre-release separator, so replacing the first occurrence is the
# whole translation; everything after it is the pre-release identifier
# and Debian is happy with its dots and alphanumerics. See the note above
# for what the untranslated form sorts as.
deb_upstream() {
    printf '%s' "$1" | sed 's/-/~/'
}

SHA="${CI_COMMIT_SHA:-$(git rev-parse HEAD)}"
SHA7="$(printf '%.7s' "$SHA")"
DATE="$(date -u +%Y%m%d)"
# RFC 5322, which is what a Debian changelog trailer takes. Honour
# SOURCE_DATE_EPOCH so a reproducible build gets a stable timestamp.
if [ -n "${SOURCE_DATE_EPOCH:-}" ]; then
    STAMP="$(date -uR -d "@${SOURCE_DATE_EPOCH}")"
else
    STAMP="$(date -uR)"
fi
MAINTAINER="Lew Palm <lp@lew-palm.de>"
CHANGELOG_DIR="target/deb-changelog"

# Commit distance from the last `v*` tag. `--long` always answers
# <tag>-<n>-g<sha>, so the distance is the field before the abbreviated
# sha. Guarded by a digit test rather than trusted: a tag name that is not
# of the `v<semver>` shape would split elsewhere, and a build id is not
# worth a wrong number. No tag reachable (shallow or tagless clone) leaves
# DISTANCE empty and the build id keeps the shape it has always had.
DISTANCE=""
BASE_TAG=""
if described="$(git describe --tags --match 'v*' --long 2>/dev/null)"; then
    without_sha="${described%-g*}"
    candidate="${without_sha##*-}"
    case "$candidate" in
    '' | *[!0-9]*) ;;
    *)
        DISTANCE="$candidate"
        BASE_TAG="${without_sha%-*}"
        ;;
    esac
fi

BUILD_ID="nightly.${DATE}-${SHA7}"
[ -n "$DISTANCE" ] && BUILD_ID="${BUILD_ID}+${DISTANCE}"

echo "$BUILD_ID" >.build-id
echo "$CARGO_TARGET" >.cargo-target-dir

for crate in "${CRATES[@]}"; do
    # `cargo pkgid` resolves the version through cargo itself rather than
    # by grepping a manifest, which is what makes per-package versions
    # work at all: three crates, three answers, no assumption that any of
    # them equals the workspace version.
    version="$(cargo pkgid -p "$crate" | sed 's/.*[#@]//')"
    if [ -z "$version" ]; then
        echo "error: could not resolve a version for crate ${crate}" >&2
        exit 1
    fi
    deb_version="$(deb_upstream "$version")~nightly.${DATE}.${SHA7}"
    echo "$deb_version" >".deb-version-${crate}"

    mkdir -p "$CHANGELOG_DIR"
    pkg="$(pkg_name "$crate")"
    cat >"${CHANGELOG_DIR}/${pkg}" <<EOF
${pkg} (${deb_version}) unstable; urgency=medium

  * Nightly build from commit ${SHA7}.

 -- ${MAINTAINER}  ${STAMP}
EOF
done

echo "[deb-stamp] build-id=$(cat .build-id)"
if [ -n "$DISTANCE" ]; then
    echo "[deb-stamp] distance=${DISTANCE} commit(s) past ${BASE_TAG}"
else
    echo "[deb-stamp] distance=unknown (no v* tag reachable)"
fi
echo "[deb-stamp] cargo-target-dir=$(cat .cargo-target-dir)"
for crate in "${CRATES[@]}"; do
    echo "[deb-stamp] ${crate}=$(cat ".deb-version-${crate}")"
done
