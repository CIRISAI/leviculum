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
#   .build-id                    nightly.<UTCdate>-<sha7>
#   .deb-version-<crate>         <crate version>~nightly.<UTCdate>.<sha7>
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
# Commit and date come from CI when present and from git/date otherwise,
# so a local `just build-deb` produces the same shape as CI.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Every crate that ships a Debian package. Keyed by crate name, which is
# what `cargo deb -p` and `cargo pkgid -p` both take; the resulting .deb
# may be named differently (leviculum-cli ships as "leviculum").
CRATES=(leviculum-cli lnomad lblogd)

SHA="${CI_COMMIT_SHA:-$(git rev-parse HEAD)}"
SHA7="$(printf '%.7s' "$SHA")"
DATE="$(date -u +%Y%m%d)"

echo "nightly.${DATE}-${SHA7}" >.build-id

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
    echo "${version}~nightly.${DATE}.${SHA7}" >".deb-version-${crate}"
done

echo "[deb-stamp] build-id=$(cat .build-id)"
for crate in "${CRATES[@]}"; do
    echo "[deb-stamp] ${crate}=$(cat ".deb-version-${crate}")"
done
