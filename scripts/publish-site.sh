#!/usr/bin/env bash
# Publishes dist/ to the project's own site, as the second target of the
# nightly build. Called from the `publish-site` step in
# .woodpecker/nightly.yml. Stable download URL:
#   https://leviculum.network/releases/nightly/latest/<filename>
#
# Why a second target at all: every download link we publish — README,
# lblogd/README.md, the miauhaus update procedure — points at the forge's
# rolling `nightly` release, so a forge outage breaks every install
# instruction we have written down, and leaving the forge would break them
# permanently. The forge publish (scripts/publish-nightly.sh) is unchanged
# and keeps running; this adds a target we own.
#
# Forge-neutral on purpose: no API, no token, no vendor. It tars the build
# and pipes it into `lev-receive-nightly` on the receiving host over ssh.
# Everything that decides what gets published lives on the far end
# (packaging/site/lev-receive-nightly) — this script's job is to hand over
# an exact copy of dist/ plus the build id, and to fail loudly if it cannot.
#
# HOST KEY PINNING. The host key is supplied by CI, `StrictHostKeyChecking`
# is `yes`, and the known-hosts file is this run's alone. There is no
# accept-on-first-use anywhere: an unattended job cannot recognise a host it
# has never seen, so a key it was not given is a failure, not a prompt.
#
# Environment (all three together, or none of them):
#   SITE_SSH_TARGET     user@host of the receiving VPS
#   SITE_SSH_KEY        private key, in full, for that user
#   SITE_SSH_HOST_KEY   the host's public key as a known_hosts line
#
# NONE of them set means this target is not configured: the script says so,
# loudly, and exits 0. It is deliberately not a `from_secret` in the pipeline
# and therefore deliberately not a hard requirement here — a missing
# Woodpecker secret is a compile error for the whole pipeline rather than a
# failure of the step that names it, and that took the forge publish down
# with it for three weeks (cron #434). SOME of them set is a
# misconfiguration and does fail.
#
# Optional:
#   SITE_SSH_PORT       default 22
#   SITE_REMOTE_COMMAND default `lev-receive-nightly`; ignored by the far
#                       end when the key carries a forced command, which is
#                       the intended setup
#   LEVICULUM_BUILD_ID  fallback if .build-id is absent
#
# Test hook:
#   scripts/publish-site.sh --tar-only <file>
# writes exactly the tar it would have piped and makes no ssh call, which is
# how scripts/test-site-publish.sh drives the real sender against the real
# receiver without a network.
#
# Usage:
#   bash scripts/publish-site.sh [--tar-only <file>]

set -euo pipefail
shopt -s nullglob

TAR_ONLY=""
while [ $# -gt 0 ]; do
    case "$1" in
    --tar-only)
        [ $# -ge 2 ] || { echo "[publish-site] --tar-only needs a file" >&2; exit 1; }
        TAR_ONLY="$2"
        shift 2
        ;;
    *)
        echo "[publish-site] unknown argument '$1'" >&2
        exit 1
        ;;
    esac
done

say() { printf '[publish-site] %s\n' "$*"; }
die() { printf '[publish-site] %s\n' "$*" >&2; exit 1; }

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="$ROOT/dist"

# --- Is this target configured at all? ------------------------------------
#
# The site publish needs three values that come from outside; the forge
# publish needs none of them. Until somebody supplies them this step has
# nothing to publish with, and that state has to be LOUD and HARMLESS.
#
# Loud, because an unconfigured publish target that says nothing is exactly
# how a dead publish goes unnoticed: nobody reads a green cron job's log, and
# a step that does nothing quietly looks like a step that worked.
#
# Harmless, because this is the SECOND target. The forge publish has already
# run by the time this step starts, every download link we have written down
# still points there, and nothing points at the site yet. A nightly that is
# red every night for a known, intended, half-finished handover is a nightly
# whose red stops meaning anything — which is how a real break gets ignored.
# The day a published link points at leviculum.network, this exit 0 becomes a
# failure: from then on an unconfigured site publish IS a broken download URL.
#
# Partly configured is a different claim and fails now: a key with no host,
# or a host with no key, is a mistake somebody made rather than a target
# nobody has wired yet, and guessing the rest is not this script's business.
#
# `--tar-only` is the test hook — it opens no connection, so it needs none of
# this and is exempt.
SITE_VARS=(SITE_SSH_TARGET SITE_SSH_KEY SITE_SSH_HOST_KEY)
site_set=()
site_unset=()
for _v in "${SITE_VARS[@]}"; do
    if [ -n "${!_v:-}" ]; then site_set+=("$_v"); else site_unset+=("$_v"); fi
done

if [ -z "$TAR_ONLY" ] && [ "${#site_set[@]}" -eq 0 ]; then
    say "=================================================================="
    say "SITE PUBLISH IS NOT CONFIGURED - nothing was uploaded."
    say ""
    say "None of ${SITE_VARS[*]} is set"
    say "in this step's environment."
    say ""
    say "The forge publish is unaffected and has already run; the download"
    say "links in the README are current. The second target, the one under"
    say "https://leviculum.network/releases/nightly/, is standing still and"
    say "will go on standing still every night until it is wired up."
    say ""
    say "To wire it up: create the Woodpecker repository secrets FIRST,"
    say "then pass them into the step. The publish-site step in"
    say ".woodpecker/nightly.yml spells out the order, and why that order"
    say "is not a matter of taste."
    say "=================================================================="
    exit 0
fi

if [ -z "$TAR_ONLY" ] && [ "${#site_unset[@]}" -gt 0 ]; then
    die "site publish is PARTLY configured: ${site_set[*]} set, ${site_unset[*]} missing - refusing to guess the rest"
fi

# No dist/ at all means nothing was built in this run. The step is allowed to
# run after a failed earlier step (see the `status:` note in
# .woodpecker/nightly.yml), and a build that never produced packages is that
# step's red, not this one's — a second failure here would only add noise to
# a pipeline that is already failing. An EMPTY dist/ is a different claim:
# the packaging step ran and produced nothing, and that is refused below.
if [ ! -d "$DIST" ]; then
    say "no dist/ — nothing was built in this run, nothing to publish"
    exit 0
fi

# The same name rule the receiver enforces, checked here too so a bad name
# fails on the runner, where the log is read by whoever caused it, instead of
# at the far end in a refusal nobody sees.
SAFE_NAME='^[A-Za-z0-9][A-Za-z0-9._+-]{0,127}$'

names=()
for path in "$DIST"/*; do
    name="${path##*/}"
    [ ! -L "$path" ] || die "dist/${name} is a symlink; refusing to publish it"
    [ -f "$path" ] || die "dist/${name} is not a regular file; refusing to publish it"
    [[ $name =~ $SAFE_NAME ]] || die "dist/${name} is not a plain safe filename"
    names+=("$name")
done
[ "${#names[@]}" -gt 0 ] ||
    die "dist/ is empty — refusing to publish an empty build"

# The build id is what the receiver names the published directory. It is the
# one written by scripts/deb-stamp.sh in the build step and read by every
# step after it, so all targets of one nightly agree on what it is called.
if [ -r "$ROOT/.build-id" ]; then
    IFS= read -r BUILD_ID <"$ROOT/.build-id" || true
else
    BUILD_ID="${LEVICULUM_BUILD_ID:-}"
fi
[ -n "$BUILD_ID" ] ||
    die "no build id: neither .build-id nor LEVICULUM_BUILD_ID is set"
[[ $BUILD_ID =~ $SAFE_NAME ]] ||
    die "build id '${BUILD_ID}' is not a plain safe name"
[ "$BUILD_ID" != latest ] ||
    die "build id 'latest' collides with the pointer symlink on the site"

WORK="$(mktemp -d)"
trap 'rm -rf -- "$WORK"' EXIT

# One flat tar: the dist/ files under their own names, plus BUILD_ID. Flat
# because the receiver refuses any member name holding a `/` — a directory
# member is a place for a name to hide. Uncompressed because every file in
# dist/ is a .deb or a .tar.gz and is already compressed; gzip here would
# buy a fraction of a percent and cost the CPU of both ends.
printf '%s\n' "$BUILD_ID" >"$WORK/BUILD_ID"
tar -cf "$WORK/upload.tar" -C "$DIST" -- "${names[@]}"
tar -rf "$WORK/upload.tar" -C "$WORK" -- BUILD_ID

size="$(wc -c <"$WORK/upload.tar")"
say "build ${BUILD_ID}: ${#names[@]} file(s), $((size / 1024)) KiB"

if [ -n "$TAR_ONLY" ]; then
    cp -- "$WORK/upload.tar" "$TAR_ONLY"
    say "wrote ${TAR_ONLY}, no upload (--tar-only)"
    exit 0
fi

# The three required values were checked at the top of the script, where an
# unconfigured target is told apart from a misconfigured one; by here all
# three are set. Only the two with defaults are read again.
SITE_SSH_PORT="${SITE_SSH_PORT:-22}"
SITE_REMOTE_COMMAND="${SITE_REMOTE_COMMAND:-lev-receive-nightly}"

# A known_hosts file holding one entry: the key we were given. Anything else
# the host presents is a failure. The shape is checked rather than assumed —
# a secret that got truncated or shell-mangled would otherwise produce
# "no matching host key", which reads like the host changed its key.
(
    umask 077
    printf '%s\n' "$SITE_SSH_KEY" >"$WORK/id"
    printf '%s\n' "$SITE_SSH_HOST_KEY" >"$WORK/known_hosts"
)
read -r _kh_host _kh_type _kh_material <"$WORK/known_hosts" || true
case "${_kh_type:-}" in
ssh-ed25519 | ssh-rsa | ecdsa-sha2-* | sk-*) ;;
*) die "SITE_SSH_HOST_KEY is not a known_hosts line (second field '${_kh_type:-}')" ;;
esac
[ -n "${_kh_material:-}" ] || die "SITE_SSH_HOST_KEY carries no key material"
[ -n "${_kh_host:-}" ] || die "SITE_SSH_HOST_KEY names no host"

say "uploading to ${SITE_SSH_TARGET} port ${SITE_SSH_PORT}"
# IdentitiesOnly + IdentityAgent=none: the key we were handed is the only
# one offered, so a stray agent socket in the runner cannot change which
# identity publishes. BatchMode: never wait for a human who is not there.
ssh \
    -o BatchMode=yes \
    -o StrictHostKeyChecking=yes \
    -o UserKnownHostsFile="$WORK/known_hosts" \
    -o GlobalKnownHostsFile=/dev/null \
    -o IdentitiesOnly=yes \
    -o IdentityAgent=none \
    -o PasswordAuthentication=no \
    -o ConnectTimeout=30 \
    -i "$WORK/id" \
    -p "$SITE_SSH_PORT" \
    "$SITE_SSH_TARGET" "$SITE_REMOTE_COMMAND" <"$WORK/upload.tar" ||
    die "upload failed: the site still serves the previous build"

say "done"
say "latest: https://leviculum.network/releases/nightly/latest/"
