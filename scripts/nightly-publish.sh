#!/usr/bin/env bash
# Build and publish the nightly from the tree the tier-2 nightly just tested
# (517, `just nightly-publish`).
#
# Why here and not in .woodpecker/nightly.yml: every step of that pipeline was
# already a script in this repo; Woodpecker contributed a container, a cron and
# the secrets. The decision to publish came from this host anyway (the green
# ref scripts/nightly-green-ref.sh pushes), and a forge cron that fires on its
# own clock and again on the green-ref trigger ran twice on ed0754e4 on
# 2026-10-08, the second site publish refused. Run from the nightly host after
# a green night, the recipe builds the commit that night tested, in the tree it
# tested it in, needs no secret at a third party, and does not care which
# forge the source lives on. Lew decided the move on 2026-10-08.
#
# In order, each step the script the forge pipeline ran:
#
#   scripts/deb-stamp.sh                 build id and versions, from HEAD
#   scripts/build-deb.sh amd64 | arm64   the .debs and the heapacct variants
#   scripts/lnflash-bundle.sh            the firmware bundle, firmware built
#   scripts/build-docs-tarball.sh        the book
#   scripts/collect-nightly-debs.sh      everything staged into dist/
#   scripts/publish-site.sh              leviculum.network/releases/nightly/
#   scripts/publish-nightly.sh           the forge's rolling `nightly` release
#
# REFUSES, before it builds anything:
#   * a missing tool: one line per tool, naming scripts/install-ci.sh;
#   * a missing credential file;
#   * a dirty tree: what goes out must be a commit, the one a night tested;
#   * a HEAD no green night covers (scripts/check-nightly-green.sh, the gate
#     publish-nightly.sh runs anyway; asked here first so a refusal costs
#     seconds, not a build). The nightly that has just gone green on HEAD says
#     so with LEVICULUM_PUBLISH_FROM_GREEN_RUN=<HEAD sha>, because the forge
#     may not serve the ref it pushed seconds earlier.
#
# IDEMPOTENT on one commit and one UTC day, which is one build id: a second
# run finds dist/ already holding that build and publishes it again without
# rebuilding. Rebuilding would not be harmless, because the packages are not
# byte-reproducible (build-deb.sh relinks, tar records mtimes), and the site
# receiver refuses a known build id with different contents. The same bytes
# again it reports as "already published, identical"; the forge release swap
# replaces assets with the same names. LEVICULUM_PUBLISH_REBUILD=1 rebuilds
# anyway.
#
# Environment, all optional:
#   LEVICULUM_PUBLISH_FROM_GREEN_RUN  sha the calling nightly tested green
#   LEVICULUM_PUBLISH_DRY_RUN=1       build everything, upload nothing; both
#                                     publish scripts say what they would send
#   LEVICULUM_PUBLISH_REBUILD=1       rebuild although dist/ holds this build
#   LEVICULUM_SITE_DIR                default ~/.config/leviculum-site, holding
#                                     `id` (the private key) and `known_hosts`
#                                     (one pinned `[host]:port type key` line)
#   SITE_SSH_TARGET, SITE_SSH_PORT    default levsite@<host> and <port> from
#                                     that known_hosts line
#   LEVICULUM_CODEBERG_TOKEN_FILE     default ~/.config/codeberg-token, read by
#                                     publish-nightly.sh when CODEBERG_TOKEN is
#                                     unset
#   CI_REPO                           default Lew_Palm/leviculum
#
# Credentials are passed as files and never printed.
#
# Usage: bash scripts/nightly-publish.sh   (or `just nightly-publish`)

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

say() { printf '[nightly-publish] %s\n' "$*"; }
die() { printf '[nightly-publish] %s\n' "$*" >&2; exit 1; }

DRY_RUN="${LEVICULUM_PUBLISH_DRY_RUN:-0}"
SITE_DIR="${LEVICULUM_SITE_DIR:-${HOME:-}/.config/leviculum-site}"
SITE_KEY="$SITE_DIR/id"
SITE_KNOWN_HOSTS="$SITE_DIR/known_hosts"
TOKEN_FILE="${LEVICULUM_CODEBERG_TOKEN_FILE:-${HOME:-}/.config/codeberg-token}"
# What a previous run left in dist/, as "<build id> <commit>".
BUILT_STAMP="$ROOT/target/nightly-publish-built"

# --- Tools ------------------------------------------------------------------
#
# All of them before anything runs: a tool found missing after twenty minutes
# of arm64 build is twenty minutes spent on nothing.
missing=0
need() {  # <what> <present: 0|1>
    if [ "$2" != 1 ]; then
        say "missing: $1 (scripts/install-ci.sh installs it)"
        missing=1
    fi
}
has() { command -v "$1" >/dev/null 2>&1 && echo 1 || echo 0; }

for tool in cargo rustup git python3 curl jq ssh tar gzip sha256sum musl-gcc \
    cargo-zigbuild flip-link mdbook; do
    need "$tool" "$(has "$tool")"
done
need "cargo-deb" "$(cargo deb --version >/dev/null 2>&1 && echo 1 || echo 0)"
# cargo-zigbuild finds a pip-installed ziglang without a zig on PATH.
need "zig (or the ziglang Python package)" \
    "$({ command -v zig >/dev/null 2>&1 || python3 -c 'import ziglang' >/dev/null 2>&1; } && echo 1 || echo 0)"
if [ "$(has mdbook)" = 1 ]; then
    pin="$(bash scripts/build-docs-tarball.sh --print-mdbook-pin)"
    need "mdbook v${pin} (found '$(mdbook --version)')" \
        "$([ "$(mdbook --version)" = "mdbook v${pin}" ] && echo 1 || echo 0)"
fi
if [ "$(has rustup)" = 1 ]; then
    installed="$(rustup target list --installed 2>/dev/null || true)"
    for triple in x86_64-unknown-linux-musl aarch64-unknown-linux-musl thumbv7em-none-eabihf; do
        need "rustup target ${triple}" "$(grep -qx "$triple" <<<"$installed" && echo 1 || echo 0)"
    done
    # The same three places scripts/lnflash-bundle.sh's find_objcopy looks.
    sysroot="$(rustc --print sysroot 2>/dev/null || true)"
    objcopy=0
    if [ -n "$sysroot" ] && find "$sysroot/lib/rustlib" -name llvm-objcopy -type f 2>/dev/null | grep -q .; then
        objcopy=1
    elif command -v llvm-objcopy >/dev/null 2>&1 || command -v arm-none-eabi-objcopy >/dev/null 2>&1; then
        objcopy=1
    fi
    need "an objcopy: rustup component llvm-tools, or binutils-arm-none-eabi" "$objcopy"
fi
[ "$missing" = 0 ] || die "refusing: tools missing (above); nothing was built"

# --- Credentials -------------------------------------------------------------
[ -r "$SITE_KEY" ] || die "refusing: no site publish key at ${SITE_KEY} (LEVICULUM_SITE_DIR)"
[ -r "$SITE_KNOWN_HOSTS" ] || die "refusing: no pinned site host key at ${SITE_KNOWN_HOSTS} (LEVICULUM_SITE_DIR)"
if [ -z "${CODEBERG_TOKEN:-}" ] && [ ! -r "$TOKEN_FILE" ]; then
    die "refusing: CODEBERG_TOKEN is unset and ${TOKEN_FILE} is not readable (LEVICULUM_CODEBERG_TOKEN_FILE)"
fi

# The site target follows from the pinned host: `[host]:port` or `host`.
read -r kh_host _ <"$SITE_KNOWN_HOSTS" || true
case "${kh_host:-}" in
\[*\]:*)
    kh_port="${kh_host##*:}"
    kh_host="${kh_host#\[}"
    kh_host="${kh_host%%\]*}"
    ;;
*) kh_port=22 ;;
esac
[ -n "${kh_host:-}" ] || die "refusing: ${SITE_KNOWN_HOSTS} names no host"
export SITE_SSH_TARGET="${SITE_SSH_TARGET:-levsite@${kh_host}}"
export SITE_SSH_PORT="${SITE_SSH_PORT:-${kh_port}}"

# --- The commit -------------------------------------------------------------
if [ -n "$(git status --porcelain)" ]; then
    git status --short | sed 's/^/[nightly-publish]   /'
    die "refusing: the tree is dirty; only a commit can be published"
fi
HEAD_SHA="$(git rev-parse HEAD)"
export CI_COMMIT_SHA="$HEAD_SHA"
export CI_REPO="${CI_REPO:-Lew_Palm/leviculum}"
say "commit ${HEAD_SHA} (${CI_REPO})"

bash scripts/check-nightly-green.sh --commit "$HEAD_SHA" ||
    die "refusing: no green nightly covers ${HEAD_SHA:0:12}; nothing was built"

# --- Build ------------------------------------------------------------------
bash scripts/deb-stamp.sh
BUILD_ID="$(cat .build-id)"
export LEVICULUM_BUILD_ID="$BUILD_ID"

if [ "${LEVICULUM_PUBLISH_REBUILD:-0}" != 1 ] && [ -d dist ] &&
    [ -n "$(ls -A dist 2>/dev/null)" ] && [ -r "$BUILT_STAMP" ] &&
    [ "$(cat "$BUILT_STAMP")" = "$BUILD_ID $HEAD_SHA" ]; then
    say "dist/ already holds build ${BUILD_ID}; publishing it again without a rebuild"
else
    rm -f "$BUILT_STAMP"
    start=$(date +%s)
    bash scripts/build-deb.sh amd64
    bash scripts/build-deb.sh arm64
    # As the forge's build-lnflash step: no SKIP_FIRMWARE, so the bundle's
    # image is built from this commit.
    bash scripts/lnflash-bundle.sh
    bash scripts/build-docs-tarball.sh
    bash scripts/collect-nightly-debs.sh
    mkdir -p "$(dirname "$BUILT_STAMP")"
    printf '%s %s\n' "$BUILD_ID" "$HEAD_SHA" >"$BUILT_STAMP"
    say "built ${BUILD_ID} in $(($(date +%s) - start)) s"
fi

# --- Publish ----------------------------------------------------------------
#
# Both targets, each whatever the other did: a forge outage is the night the
# site copy matters, and the reverse. The run fails if either failed.
export LEVICULUM_PUBLISH_DRY_RUN="$DRY_RUN"
site_rc=0
bash scripts/publish-site.sh --key-file "$SITE_KEY" --known-hosts-file "$SITE_KNOWN_HOSTS" ||
    site_rc=$?
forge_rc=0
LEVICULUM_CODEBERG_TOKEN_FILE="$TOKEN_FILE" bash scripts/publish-nightly.sh || forge_rc=$?

if [ "$site_rc" != 0 ] || [ "$forge_rc" != 0 ]; then
    die "FAILED: site publish exit ${site_rc}, forge publish exit ${forge_rc} (build ${BUILD_ID})"
fi
if [ "$DRY_RUN" = 1 ]; then
    say "dry run done: build ${BUILD_ID} built, nothing uploaded"
else
    say "published ${BUILD_ID} to the site and the forge"
fi
