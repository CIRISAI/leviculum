#!/usr/bin/env bash
# Publishes dist/*.deb + *.sha256 to the rolling `nightly` Codeberg
# release. Called from .woodpecker/nightly.yml. Stable download URL:
#   https://codeberg.org/${CI_REPO}/releases/download/nightly/<filename>
#
# The release is rolling: same tag every night, assets overwritten.
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

TAG="nightly"
API="https://codeberg.org/api/v1"
AUTH_HEADER="Authorization: token ${CODEBERG_TOKEN}"
BUILD_ID="${LEVICULUM_BUILD_ID:-unknown}"

DIST="$(cd "$(dirname "$0")/.." && pwd)/dist"
[ -d "$DIST" ] || { echo "dist/ not found — run collect-nightly-debs.sh first"; exit 1; }

RELEASE_BODY=$(cat <<EOF
Rolling nightly build. The assets under this release are **replaced on every CI run** — this tag always points at the latest nightly.

\`\`\`
https://codeberg.org/${CI_REPO}/releases/download/nightly/leviculum-nightly-amd64.deb
https://codeberg.org/${CI_REPO}/releases/download/nightly/leviculum-nightly-arm64.deb
\`\`\`

\`sudo apt install ./leviculum-nightly-amd64.deb\` — no other system packages required, binaries are static musl. Tested against Debian 9+ / Ubuntu 16.04+. Installs lnsd as a system service and sets up /etc/reticulum for Python-RNS client drop-in compatibility.

Current build: \`${BUILD_ID}\` (commit \`${CI_COMMIT_SHA}\`)

Verify with \`lnsd --version\` after install.
EOF
)

# Find existing release
echo "[publish] looking up release tag=${TAG}"
release_json=$(curl -sS -H "$AUTH_HEADER" "$API/repos/$CI_REPO/releases/tags/$TAG" || echo '{}')
release_id=$(echo "$release_json" | jq -r '.id // empty')

if [ -z "$release_id" ]; then
    echo "[publish] no existing release, creating"
    # target_commitish must be a branch name when the tag doesn't
    # yet exist — Forgejo rejected both the bare SHA (build #40) and
    # an omitted field (build #41) with "The target couldn't be
    # found." Use the default branch from Woodpecker, which is
    # 'master' here. The exact build SHA still appears in the body.
    BRANCH="${CI_REPO_DEFAULT_BRANCH:-master}"
    release_json=$(jq -n \
        --arg tag "$TAG" \
        --arg target "$BRANCH" \
        --arg body "$RELEASE_BODY" \
        '{tag_name:$tag, target_commitish:$target, name:"Nightly Builds", body:$body, draft:false, prerelease:true}' \
        | curl -sS -X POST -H "$AUTH_HEADER" -H "Content-Type: application/json" \
            "$API/repos/$CI_REPO/releases" -d @-)
    release_id=$(echo "$release_json" | jq -r '.id')
    [ -n "$release_id" ] && [ "$release_id" != "null" ] || { echo "[publish] create failed: $release_json"; exit 1; }
    echo "[publish] created release id=${release_id}"
else
    echo "[publish] found release id=${release_id}, refreshing body"
    jq -n \
        --arg body "$RELEASE_BODY" \
        '{body:$body}' \
        | curl -sS -X PATCH -H "$AUTH_HEADER" -H "Content-Type: application/json" \
            "$API/repos/$CI_REPO/releases/$release_id" -d @- >/dev/null

    echo "[publish] deleting existing assets"
    echo "$release_json" | jq -r '.assets[].id' | while read -r asset_id; do
        [ -n "$asset_id" ] || continue
        curl -sS -X DELETE -H "$AUTH_HEADER" \
            "$API/repos/$CI_REPO/releases/assets/$asset_id"
    done
fi

echo "[publish] uploading new assets"
shopt -s nullglob
for f in "$DIST"/*.deb "$DIST"/*.sha256; do
    name=$(basename "$f")
    echo "[publish]   → $name"
    curl -sS -X POST -H "$AUTH_HEADER" \
        -F "attachment=@${f}" \
        "$API/repos/$CI_REPO/releases/$release_id/assets?name=${name}" >/dev/null
done

echo "[publish] done"
echo "[publish] latest: https://codeberg.org/${CI_REPO}/releases/tag/${TAG}"
