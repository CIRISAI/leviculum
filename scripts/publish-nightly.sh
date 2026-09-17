#!/usr/bin/env bash
# Publishes dist/*.deb + *.sha256 to the rolling `nightly` Codeberg
# release. Called from .woodpecker/nightly.yml. Stable download URL:
#   https://codeberg.org/${CI_REPO}/releases/download/nightly/<filename>
#
# The release is rolling: same tag every night, assets overwritten. The
# overwrite is an upload-then-delete swap, never the other way round:
# the previous build stays downloadable until the new one is fully up
# (Codeberg #286).
# Version info for each build is embedded in the binaries themselves
# (lnsd --version) and in the release body.
#
# Authentication uses a Codeberg API token with `write:repository`
# scope, exposed to the publish step via the Woodpecker secret
# `codeberg_token`. CI_NETRC_PASSWORD (Woodpecker's OAuth-derived
# token) is NOT visible outside the clone step, so a manual token is
# required.
#
# Required env (set by Woodpecker):
#   CI_REPO         — e.g. "Lew_Palm/leviculum"
#   CI_COMMIT_SHA   — current commit
#   CODEBERG_TOKEN  — Codeberg API token (Woodpecker secret)
#   LEVICULUM_BUILD_ID (optional, for release body)

set -euo pipefail

: "${CI_REPO:?CI_REPO not set}"
: "${CI_COMMIT_SHA:?CI_COMMIT_SHA not set}"
: "${CODEBERG_TOKEN:?CODEBERG_TOKEN not set}"
# The tag push at the end reads the token from the environment via a git
# credential helper; Woodpecker exports it, a manual caller might not.
export CODEBERG_TOKEN

TAG="nightly"
API="https://codeberg.org/api/v1"
AUTH_HEADER="Authorization: token ${CODEBERG_TOKEN}"
BUILD_ID="${LEVICULUM_BUILD_ID:-unknown}"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="$ROOT/dist"
[ -d "$DIST" ] || { echo "dist/ not found — run collect-nightly-debs.sh first"; exit 1; }

RELEASE_BODY=$(cat <<EOF
Rolling nightly build. The assets under this release are **replaced on every CI run** — this tag always points at the latest nightly.

**Debian / Ubuntu packages** (statically linked musl, runs on Debian 9+ / Ubuntu 16.04+, no extra packages needed):

\`\`\`
https://codeberg.org/${CI_REPO}/releases/download/nightly/leviculum-nightly-amd64.deb
https://codeberg.org/${CI_REPO}/releases/download/nightly/leviculum-nightly-arm64.deb
\`\`\`

\`sudo apt install ./leviculum-nightly-amd64.deb\` installs \`lnsd\` as a systemd service and sets up \`/etc/reticulum\` for Python-RNS client drop-in compatibility.

**Userspace tarball** (just the binaries plus README/LICENSE/CHANGELOG, no service, no root needed):

\`\`\`
https://codeberg.org/${CI_REPO}/releases/download/nightly/leviculum-nightly-amd64.tar.gz
https://codeberg.org/${CI_REPO}/releases/download/nightly/leviculum-nightly-arm64.tar.gz
\`\`\`

\`tar xzf leviculum-nightly-amd64.tar.gz && ./leviculum-nightly-amd64/bin/lnsd --version\` runs without installing anything system-wide.

**lnomad — Nomadnet terminal browser** (separate package, does not install or start the lnsd service):

\`\`\`
https://codeberg.org/${CI_REPO}/releases/download/nightly/lnomad-nightly-amd64.deb
https://codeberg.org/${CI_REPO}/releases/download/nightly/lnomad-nightly-arm64.deb
https://codeberg.org/${CI_REPO}/releases/download/nightly/lnomad-nightly-amd64.tar.gz
https://codeberg.org/${CI_REPO}/releases/download/nightly/lnomad-nightly-arm64.tar.gz
\`\`\`

\`sudo apt install ./lnomad-nightly-amd64.deb\` — the browser needs a running RNS instance (leviculum's \`lnsd\` or Python \`rnsd\`).

**lblogd — dev-blog server** (separate package; serves Markdown posts as a NomadNet page node and on the clearnet):

\`\`\`
https://codeberg.org/${CI_REPO}/releases/download/nightly/lblogd-nightly-amd64.deb
https://codeberg.org/${CI_REPO}/releases/download/nightly/lblogd-nightly-arm64.deb
https://codeberg.org/${CI_REPO}/releases/download/nightly/lblogd-nightly-amd64.tar.gz
https://codeberg.org/${CI_REPO}/releases/download/nightly/lblogd-nightly-arm64.tar.gz
\`\`\`

\`sudo apt install ./lblogd-nightly-amd64.deb\` installs and starts \`lblogd\` as a systemd service. As shipped it serves on \`http://127.0.0.1:8180/\`; \`/etc/lblogd/config.toml\` explains how to put it on a public domain with automatic HTTPS.

\`lnomad\` and \`lblogd\` carry their own version numbers, independent of the \`leviculum\` packages above.

**lnflash — firmware flasher for LNode boards** (self-contained bundle: the flasher, the T114 firmware image, and Nordic's S140 SoftDevice with its licence):

\`\`\`
https://codeberg.org/${CI_REPO}/releases/download/nightly/lnflash-nightly-amd64.tar.gz
\`\`\`

\`tar xzf lnflash-nightly-amd64.tar.gz && cd lnflash-* && sudo ./lnflash\` — nothing is downloaded and nothing is installed; everything it writes to the board is in the directory. The flasher binary is amd64; the firmware image and SoftDevice inside are not architecture-specific.

**Source tarball** (tracked files at the same commit as the .debs above, no submodules):

\`\`\`
https://codeberg.org/${CI_REPO}/releases/download/nightly/leviculum-nightly-source.tar.gz
\`\`\`

Each asset is published with a matching \`.sha256\` next to it.

Current build: \`${BUILD_ID}\` (commit \`${CI_COMMIT_SHA}\`)

Verify with \`lnsd --version\` after install.
EOF
)

# --- Forge requests -------------------------------------------------------
#
# `curl` without `-f` exits 0 on an HTTP 4xx or 5xx: from its point of view
# the transfer succeeded, and the caller sees a successful command. Every
# request below that CHANGES the release therefore goes through this wrapper,
# which puts the HTTP status on stdout and leaves the response body in $RESP
# for the failure path to print. A connection failure yields `000`, which is
# a failure like any other status. (Codeberg #286: the upload loop had no
# status check at all, wrote its output to /dev/null and had no failure
# branch, so a 500 from the forge ended in `[publish] done`.)
RESP="$(mktemp)"
trap 'rm -f "$RESP"' EXIT

api() {  # <curl args...> — HTTP status on stdout, body in $RESP
    curl -sS -o "$RESP" -w '%{http_code}' "$@" || true
}

ok() { case "$1" in 2*) return 0 ;; *) return 1 ;; esac; }

die() {  # <message...> — print the forge's own answer with it
    echo "[publish] $*"
    sed 's/^/[publish]   /' "$RESP"
    echo
    exit 1
}

# Find existing release
echo "[publish] looking up release tag=${TAG}"
release_json=$(curl -sS -H "$AUTH_HEADER" "$API/repos/$CI_REPO/releases/tags/$TAG" || echo '{}')
release_id=$(echo "$release_json" | jq -r '.id // empty')

# The previous build's assets. They are NOT deleted here: they stay in place
# until this run has uploaded every file that replaces them, so a run that
# dies halfway leaves a release with stale assets rather than an empty one.
# The README's download URLs are hardcoded to those names and 404 the moment
# the release is empty, which is what the old delete-then-upload order cost.
replaced_asset_ids=""

if [ -z "$release_id" ]; then
    echo "[publish] no existing release, creating"
    # target_commitish must be a branch name when the tag doesn't
    # yet exist — Forgejo rejected both the bare SHA (build #40) and
    # an omitted field (build #41) with "The target couldn't be
    # found." Use the default branch from Woodpecker, which is
    # 'master' here. The exact build SHA still appears in the body.
    BRANCH="${CI_REPO_DEFAULT_BRANCH:-master}"
    status=$(jq -n \
        --arg tag "$TAG" \
        --arg target "$BRANCH" \
        --arg body "$RELEASE_BODY" \
        '{tag_name:$tag, target_commitish:$target, name:"Nightly Builds", body:$body, draft:false, prerelease:true}' \
        | api -X POST -H "$AUTH_HEADER" -H "Content-Type: application/json" \
            "$API/repos/$CI_REPO/releases" -d @-)
    ok "$status" || die "create failed: HTTP $status"
    release_id=$(jq -r '.id // empty' < "$RESP")
    [ -n "$release_id" ] || die "create returned HTTP $status with no release id:"
    echo "[publish] created release id=${release_id}"
else
    echo "[publish] found release id=${release_id}, refreshing body"
    status=$(jq -n \
        --arg body "$RELEASE_BODY" \
        '{body:$body}' \
        | api -X PATCH -H "$AUTH_HEADER" -H "Content-Type: application/json" \
            "$API/repos/$CI_REPO/releases/$release_id" -d @-)
    ok "$status" || die "body refresh failed: HTTP $status"
    replaced_asset_ids=$(echo "$release_json" | jq -r '.assets[].id')
fi

shopt -s nullglob
ASSETS=("$DIST"/*.deb "$DIST"/*.tar.gz "$DIST"/*.sha256)
shopt -u nullglob

# An empty dist/ empties the release just as thoroughly as a failed upload
# does, and it is the likelier of the two: a build step that produced nothing
# still leaves the directory behind.
if [ "${#ASSETS[@]}" -eq 0 ]; then
    echo "[publish] dist/ contains no .deb, .tar.gz or .sha256 — nothing to publish."
    echo "[publish] The release keeps the previous build's assets. Failing the run."
    exit 1
fi

echo "[publish] uploading ${#ASSETS[@]} new assets"
uploaded_ids=()
for f in "${ASSETS[@]}"; do
    name=$(basename "$f")
    status=$(api -X POST -H "$AUTH_HEADER" \
        -F "attachment=@${f}" \
        "$API/repos/$CI_REPO/releases/$release_id/assets?name=${name}")
    if ! ok "$status"; then
        echo "[publish]   → $name FAILED, HTTP $status"
        sed 's/^/[publish]   /' "$RESP"; echo
        # Undo this run's uploads so the release is exactly what it was
        # before: the previous build, complete and downloadable. Forgejo
        # accepts two assets under one name (that is how 12 stale entries
        # once accumulated on this tag), so leaving them would publish a
        # half-swapped release under the README's download URLs.
        if [ "${#uploaded_ids[@]}" -gt 0 ]; then
            echo "[publish] rolling back ${#uploaded_ids[@]} asset(s) uploaded by this run"
            for id in "${uploaded_ids[@]}"; do
                rb=$(api -X DELETE -H "$AUTH_HEADER" \
                    "$API/repos/$CI_REPO/releases/$release_id/assets/$id")
                echo "[publish]   rollback asset $id → HTTP $rb"
            done
        fi
        echo "[publish] upload failed; release left at the previous build."
        exit 1
    fi
    asset_id=$(jq -r '.id // empty' < "$RESP")
    uploaded_ids+=("$asset_id")
    echo "[publish]   → $name  HTTP $status id=${asset_id:-?}"
done

if [ -n "$replaced_asset_ids" ]; then
    echo "[publish] deleting the assets this run replaced"
    # Forgejo's asset-delete endpoint is
    # /repos/{owner}/{repo}/releases/{release_id}/assets/{attachment_id}.
    # The release_id segment is mandatory — omitting it yields a silent
    # 404 with -sS, which is exactly what happened before 3cece2ce and
    # caused assets to accumulate across runs (12 stale entries on the
    # nightly tag pointing at three different builds).
    delete_failed=0
    while read -r asset_id; do
        [ -n "$asset_id" ] || continue
        status=$(api -X DELETE -H "$AUTH_HEADER" \
            "$API/repos/$CI_REPO/releases/$release_id/assets/$asset_id")
        echo "[publish]   delete asset $asset_id → HTTP $status"
        ok "$status" || delete_failed=1
    done <<< "$replaced_asset_ids"
    if [ "$delete_failed" -ne 0 ]; then
        echo "[publish] a replaced asset survived the swap: the release now holds two"
        echo "[publish] files under that name, and the download URL serves whichever"
        echo "[publish] Forgejo picks. Delete the stale one by hand. Failing the run."
        exit 1
    fi
fi

# The release rolls, so the git tag must roll with it. Forgejo points the
# tag at a commit only when the release is CREATED (the branch above);
# refreshing an existing release never moves the ref. That left `nightly`
# frozen at its creation commit (05a17675, 2026-05-06) while the assets
# beside it moved on nightly — the release page's "Source code" links
# served months-old source next to current binaries. Force-push the tag to
# the commit this run actually built, after the swap is complete so a failed
# upload or a surviving stale asset never moves it. The commit is already on
# the remote (CI builds pushed commits), so this transfers no objects, only
# the ref.
#
# The push authenticates through a one-shot credential helper, never a
# token-in-URL remote: when a push fails, git prints the full remote URL
# into the error message, and this log is public. The helper string is
# single-quoted on purpose — git expands the variable when it invokes
# the helper, reading the environment (exported above), so the token
# appears in no URL and in no process argument. The empty helper first
# clears any inherited helpers so ours is the only one consulted.
# Everything else in this script authenticates via header for the same
# no-token-in-URL reason.
echo "[publish] pointing tag ${TAG} at ${CI_COMMIT_SHA}"
# shellcheck disable=SC2016
git -C "$ROOT" \
    -c credential.helper= \
    -c credential.helper='!f() { echo "username=oauth2"; echo "password=${CODEBERG_TOKEN}"; }; f' \
    push "https://codeberg.org/${CI_REPO}.git" \
    "+${CI_COMMIT_SHA}:refs/tags/${TAG}"

echo "[publish] done"
echo "[publish] latest: https://codeberg.org/${CI_REPO}/releases/tag/${TAG}"
