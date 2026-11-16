#!/usr/bin/env bash
# Publishes dist/*.deb + *.sha256 to the rolling `nightly` Codeberg
# release. Called from .woodpecker/nightly.yml. Stable download URL:
#   https://codeberg.org/${CI_REPO}/releases/download/nightly/<filename>
#
# The release is rolling: same tag every night, assets overwritten. The
# overwrite is an upload-then-delete swap, never the other way round:
# the previous build stays downloadable until the new one is fully up
# (Codeberg #286). And it stays downloadable after that, one generation
# deep: each asset this run replaces is renamed to `<name>.previous.<ext>`
# before the uploads, and the `.previous` set it displaces is what the swap
# deletes (Codeberg #323). A good build is then still there the morning a bad
# one that passed the gates has replaced it.
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
#
# Outside Woodpecker (517: `just nightly-publish` on the nightly host, which
# runs this script only with LEVICULUM_PUBLISH_FORGE=1 since 726) the
# token comes from a file instead, read only when CODEBERG_TOKEN is unset:
#   LEVICULUM_CODEBERG_TOKEN_FILE  default ~/.config/codeberg-token
# It is never printed.
#
#   LEVICULUM_PUBLISH_DRY_RUN=1  runs the nightly gate and lists what would be
#                                uploaded, then stops before the first forge
#                                request: no release change, no tag push.
#
# The compiler that produced the assets is read from .rustc-version, the
# stamp scripts/deb-stamp.sh leaves in the repo root; this step's image has
# no rustc to ask.

set -euo pipefail

: "${CI_REPO:?CI_REPO not set}"
: "${CI_COMMIT_SHA:?CI_COMMIT_SHA not set}"
DRY_RUN="${LEVICULUM_PUBLISH_DRY_RUN:-0}"
if [ -z "${CODEBERG_TOKEN:-}" ]; then
    TOKEN_FILE="${LEVICULUM_CODEBERG_TOKEN_FILE:-${HOME:-}/.config/codeberg-token}"
    if [ -r "$TOKEN_FILE" ]; then
        IFS= read -r CODEBERG_TOKEN <"$TOKEN_FILE" || true
    fi
fi
# A dry run makes no authenticated request, so it does not need the token.
if [ "$DRY_RUN" = "1" ]; then
    CODEBERG_TOKEN="${CODEBERG_TOKEN:-dry-run-no-token}"
fi
: "${CODEBERG_TOKEN:?CODEBERG_TOKEN not set, and no token file at ${TOKEN_FILE:-?}}"
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

# What the forge gate cannot prove, imported from the one run that can
# (Codeberg #312). `just ci-gate` runs fmt, clippy and every workspace test bar
# three; `rnsd_interop` — whether we still interoperate with a Python-RNS peer —
# is one of the three and runs in no forge pipeline, because it needs the
# `reference/Reticulum` submodule and both pipelines clone without submodules on
# purpose (#300; the other two and their reasons are in
# scripts/ci-gate-integ.sh). So the tier-2
# nightly's verdict is read here instead: it pushes a `refs/nightly/green/*`
# ref at the commit it tested, and this refuses a commit no such ref covers.
#
# FIRST, before any forge request, and inside this script rather than as a
# separate pipeline step: a step is a line in a YAML file that a future commit
# can reorder or drop, and `just check-publish-nightly-gate` would then be
# asserting the wiring of something that no longer runs. Here the refusal is
# the publisher's own.
#
# The refusal names which of three conditions failed and how to override it by
# hand; see the script and docs/src/development-ci.md §"What may be published".
bash "$ROOT/scripts/check-nightly-green.sh" || exit 1

# Which compiler built what this release carries. In the body because that is
# where a stranger looking at a download can read it without unpacking
# anything, and because codegen differences between compiler versions are the
# first thing a "this build behaves differently" report has to rule out
# (Codeberg #305). Absent stamp says so rather than claiming a version.
RUSTC_VERSION="unknown"
if [ -r "$ROOT/.rustc-version" ]; then
    RUSTC_VERSION="$(cat "$ROOT/.rustc-version")"
fi
[ -n "$RUSTC_VERSION" ] || RUSTC_VERSION="unknown"

RELEASE_BODY=$(cat <<EOF
Current build: \`${BUILD_ID}\` (commit \`${CI_COMMIT_SHA}\`, built with \`${RUSTC_VERSION}\`)

Rolling nightly build. The assets under this release are **replaced on every CI run** — this tag always points at the latest nightly.

**Debian / Ubuntu packages** (statically linked musl, runs on Debian 9+ / Ubuntu 16.04+, no extra packages needed):

\`\`\`
https://codeberg.org/${CI_REPO}/releases/download/nightly/leviculum-nightly-amd64.deb
https://codeberg.org/${CI_REPO}/releases/download/nightly/leviculum-nightly-arm64.deb
\`\`\`

\`sudo apt install ./leviculum-nightly-amd64.deb\` installs \`lnsd\` as a systemd service and sets up \`/etc/reticulum\` for Python-RNS client drop-in compatibility.

\`leviculum-nightly-arm64-heapacct.deb\` is an instrumented build for memory censuses, not for daily use: its \`lnsd\` counts every allocation, and it replaces the plain package on install and is replaced by it again.

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

**lnpnd — LXMF propagation node** (separate package; a store-and-forward mailbox, drop-in for Python's \`lxmd\`):

\`\`\`
https://codeberg.org/${CI_REPO}/releases/download/nightly/lnpnd-nightly-amd64.deb
https://codeberg.org/${CI_REPO}/releases/download/nightly/lnpnd-nightly-arm64.deb
https://codeberg.org/${CI_REPO}/releases/download/nightly/lnpnd-nightly-amd64.tar.gz
https://codeberg.org/${CI_REPO}/releases/download/nightly/lnpnd-nightly-arm64.tar.gz
\`\`\`

\`sudo apt install ./lnpnd-nightly-amd64.deb\` installs \`lnpnd\` as a systemd service that is enabled but not started: put the node's identity at \`/etc/lnpnd/identity\` first, or accept a fresh one, then \`sudo systemctl start lnpnd\`.

**loquax — LXMF messenger** (separate package, no service; \`loquax send\` for scripts, plain \`loquax\` for people):

\`\`\`
https://codeberg.org/${CI_REPO}/releases/download/nightly/loquax-nightly-amd64.deb
https://codeberg.org/${CI_REPO}/releases/download/nightly/loquax-nightly-arm64.deb
https://codeberg.org/${CI_REPO}/releases/download/nightly/loquax-nightly-amd64.tar.gz
https://codeberg.org/${CI_REPO}/releases/download/nightly/loquax-nightly-arm64.tar.gz
\`\`\`

\`sudo apt install ./loquax-nightly-amd64.deb\`, like \`lnomad\`, needs a running RNS instance.

\`lnomad\`, \`lblogd\`, \`lnpnd\` and \`loquax\` carry their own version numbers, independent of the \`leviculum\` packages above.

**lnflash — firmware flasher for LNode boards** (self-contained bundle: the flasher, one prebuilt firmware image per supported board, and Nordic's S140 SoftDevice with its licence for the board that can need it):

\`\`\`
https://codeberg.org/${CI_REPO}/releases/download/nightly/lnflash-nightly-amd64.tar.gz
https://codeberg.org/${CI_REPO}/releases/download/nightly/lnflash-nightly-amd64.deb
\`\`\`

This is the prebuilt embedded firmware — no checkout, no Rust toolchain, no build. One image per board:

| board | hardware |
| --- | --- |
| \`t114\` | Heltec Mesh Node T114 |
| \`rak4631\` | RAK4631 — the WisMesh Pocket V2 and the other carriers built around that module |
| \`t1000e\` | SenseCAP T1000-E (level \`expected\`: not yet run on our rig) |

\`tar xzf lnflash-nightly-amd64.tar.gz && cd lnflash-* && sudo ./lnflash\` — nothing is downloaded and nothing is installed; everything it writes to the board is in the directory. \`--dry-run\` says what is attached and changes nothing. The flasher binary is amd64; the firmware images and the SoftDevice inside are not architecture-specific.

The \`.deb\` is the same bundle installed: \`sudo apt install ./lnflash-nightly-amd64.deb\` puts \`lnflash\` in \`/usr/bin\` and the images under \`/usr/share/lnflash\`, where it finds them, so \`sudo lnflash\` works from any directory.

Flash with this rather than by dragging a bare \`.uf2\` onto the bootloader drive: our images sit above an S140 7.x SoftDevice and a factory T114 ships 6.1.1, on which the image boots into nothing and the board goes dark until RESET is double-tapped. \`lnflash\` reads the installed version off the board before writing — it repairs a T114 and refuses a board it carries no remedy for.

**Source tarball** (tracked files at the same commit as the .debs above, no submodules):

\`\`\`
https://codeberg.org/${CI_REPO}/releases/download/nightly/leviculum-nightly-source.tar.gz
\`\`\`

Each asset is published with a matching \`.sha256\` next to it.

**Previous build.** Every asset above is also here once more with \`.previous\` before its extension, for instance \`https://codeberg.org/${CI_REPO}/releases/download/nightly/leviculum-nightly-amd64.previous.deb\` and its \`.previous.deb.sha256\`. That is the build this one replaced, exactly one build older, kept as a fallback should this one misbehave; the next run replaces it in turn. \`dpkg-deb -I leviculum-nightly-amd64.previous.deb\` shows its version, which ends in the date and commit it was built from.

Verify with \`lnsd --version\` after install.
EOF
)

# The release's name carries the build's date and commit (PUB-0023). Forgejo
# heads the releases page with the release's creation time, which for a rolling
# release is the day it was first created and never moves; the name is the
# line beside it that this script can change. The date is the UTC build date
# out of the build id (`nightly.<YYYYMMDD>-<sha7>[+<n>]`, scripts/deb-stamp.sh),
# so a republish of an older dist/ names the day it was built, not the day it
# went up; an id without one falls back to today.
#
# `published_at` would be the better fix and is not available: Forgejo's
# EditReleaseOption (codeberg.org/swagger.v1.json, Forgejo 16.0.0-dev) accepts
# tag_name, target_commitish, name, body, draft, prerelease and
# hide_archive_links, nothing else. Sending it anyway would be worse than
# leaving it out: the decoder ignores an unknown field, so the PATCH answers
# 2xx and the date stays where it was, with nothing to fall back on.
release_name() {  # <build id> <commit sha>
    local date
    if [[ "$1" =~ ^nightly\.([0-9]{4})([0-9]{2})([0-9]{2})- ]]; then
        date="${BASH_REMATCH[1]}-${BASH_REMATCH[2]}-${BASH_REMATCH[3]}"
    else
        date="$(date -u +%Y-%m-%d)"
    fi
    printf 'Nightly builds, %s (%s)\n' "$date" "${2:0:7}"
}
RELEASE_NAME="$(release_name "$BUILD_ID" "$CI_COMMIT_SHA")"

# The PATCH that refreshes an existing release: name and body in one request,
# so the page never shows one build's name over another build's body.
release_patch_payload() {  # <name> <body>
    jq -n --arg name "$1" --arg body "$2" '{name:$name, body:$body}'
}

# The assets, listed before the first forge request: an empty dist/ is refused
# without touching the release, and a dry run has everything it reports.
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

# The fallback name of an asset (#323): `.previous` before its extension, so
# a download keeps the extension a browser and dpkg go by, and the checksum of
# `x.previous.deb` is `x.previous.deb.sha256`, the same sibling rule as the
# current build's.
previous_name() {  # <asset name>
    local n="$1" ext
    for ext in .deb.sha256 .tar.gz.sha256 .sha256 .tar.gz .deb; do
        case "$n" in
            *"$ext") printf '%s\n' "${n%"$ext"}.previous${ext}"; return 0 ;;
        esac
    done
    printf '%s\n' "$n.previous"
}

if [ "$DRY_RUN" = "1" ]; then
    echo "[publish] DRY RUN (LEVICULUM_PUBLISH_DRY_RUN=1): no forge request is made."
    echo "[publish] would refresh the name and body of release tag=${TAG} on ${CI_REPO}, build ${BUILD_ID}"
    echo "[publish]   name: ${RELEASE_NAME}"
    for f in "${ASSETS[@]}"; do
        echo "[publish]   would rename $(basename "$f") → $(previous_name "$(basename "$f")"), where the release holds it"
    done
    for f in "${ASSETS[@]}"; do
        echo "[publish]   would upload $(basename "$f") ($(wc -c <"$f") bytes)"
    done
    echo "[publish] would delete the .previous set those renames displace and any asset dist/ no longer holds,"
    echo "[publish] then point tag ${TAG} at ${CI_COMMIT_SHA}"
    exit 0
fi

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

# Deleting one replaced asset, with the forge's gateway errors treated as the
# transient answers they are (Codeberg #270). In pipeline 471 two of 36
# deletions were answered with HTTP 504; measured on the forge a quarter of an
# hour later, both assets were gone and the release held exactly the 36 files
# this run had uploaded. The run had failed anyway, on the status code alone.
# What each answer means here:
#
#   2xx      deleted.
#   404      the asset is not on the release — after a 5xx or a 000 that is the
#            earlier attempt having landed after all, and either way the end
#            state is the one this loop is for.
#   5xx      500, 502, 503, 504: the forge or its gateway, not the request.
#            Retry with a short backoff.
#   000      curl reached no answer at all (the wrapper's own code for it).
#            Same treatment as a 5xx.
#   4xx      anything else — 401, 403, 422 — is a refusal, not a hiccup, and
#            retrying it only spends time. Give up on this id.
#
# No caller reads a verdict off this: whether the swap left the release sound
# is read off the release itself below, never off these statuses.
DELETE_ATTEMPTS=4        # the first try plus three retries
DELETE_BACKOFF_START=2   # seconds before the first retry, doubled after each

# Renaming one asset (#323), PATCH of its name. The answers mean what they
# mean for a delete above, with one difference: a 404 is a failure here, since
# a rename that landed leaves the asset in place and a retry of it answers
# 2xx. Unlike the delete, a rename's verdict is read: a rename that cannot get
# through fails the run before any upload.
rename_asset() {  # <asset_id> <new name>
    local id="$1" name="$2" attempt=1 backoff="$DELETE_BACKOFF_START" status
    while : ; do
        status=$(jq -n --arg name "$name" '{name:$name}' \
            | api -X PATCH -H "$AUTH_HEADER" -H "Content-Type: application/json" \
                "$API/repos/$CI_REPO/releases/$release_id/assets/$id" -d @-)
        case "$status" in
            2*)
                echo "[publish]   rename asset $id → $name, HTTP $status"
                return 0
                ;;
            5*|000)
                if [ "$attempt" -ge "$DELETE_ATTEMPTS" ]; then
                    echo "[publish]   rename asset $id → $name, HTTP $status, giving up after $attempt attempts"
                    return 1
                fi
                echo "[publish]   rename asset $id → $name, HTTP $status, retry $attempt/$((DELETE_ATTEMPTS - 1)) in ${backoff}s"
                sleep "$backoff"
                attempt=$((attempt + 1))
                backoff=$((backoff * 2))
                ;;
            *)
                echo "[publish]   rename asset $id → $name, HTTP $status, not a transient answer"
                return 1
                ;;
        esac
    done
}

# Undo this run's renames: every asset back under the name it had when the run
# found it. Called on each failure path before the run exits, so a failed run
# leaves the release as it was, names included. A failed rename-back cannot be
# repaired from here; it is named so a human can.
renamed_ids=()
renamed_from=()
restore_names() {
    local i
    [ "${#renamed_ids[@]}" -gt 0 ] || return 0
    echo "[publish] restoring the names of ${#renamed_ids[@]} asset(s) this run renamed"
    for i in "${!renamed_ids[@]}"; do
        rename_asset "${renamed_ids[$i]}" "${renamed_from[$i]}" \
            || echo "[publish]   asset ${renamed_ids[$i]} is NOT back under ${renamed_from[$i]}; rename it by hand"
    done
}

delete_asset() {  # <asset_id>
    local id="$1" attempt=1 backoff="$DELETE_BACKOFF_START" status transient=0
    while : ; do
        status=$(api -X DELETE -H "$AUTH_HEADER" \
            "$API/repos/$CI_REPO/releases/$release_id/assets/$id")
        case "$status" in
            2*)
                echo "[publish]   delete asset $id → HTTP $status"
                return 0
                ;;
            404)
                if [ "$transient" -eq 1 ]; then
                    echo "[publish]   delete asset $id → HTTP 404, the retried delete had landed"
                else
                    echo "[publish]   delete asset $id → HTTP 404, already gone"
                fi
                return 0
                ;;
            5*|000)
                transient=1
                if [ "$attempt" -ge "$DELETE_ATTEMPTS" ]; then
                    echo "[publish]   delete asset $id → HTTP $status, giving up after $attempt attempts"
                    return 1
                fi
                echo "[publish]   delete asset $id → HTTP $status, retry $attempt/$((DELETE_ATTEMPTS - 1)) in ${backoff}s"
                sleep "$backoff"
                attempt=$((attempt + 1))
                backoff=$((backoff * 2))
                ;;
            *)
                echo "[publish]   delete asset $id → HTTP $status, not a transient answer"
                return 1
                ;;
        esac
    done
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
#
# Since #323 they split in two. `rename_plan` is the assets this run replaces
# by name, one per name, each with its fallback name: they are renamed before
# the uploads and kept. `stale_asset_ids` is everything else the release held
# — the `.previous` set those renames displace, a file dist/ no longer holds,
# a second copy under one name — and that is what the swap deletes, after the
# uploads, as before. Deleting the displaced `.previous` set only then, and
# not before the renames, is what lets a failed run leave the release exactly
# as it found it; the price is that between the renames and the deletes two
# assets share each `.previous` name.
release_assets_json="[]"
rename_plan=""
stale_asset_ids=""

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
        --arg name "$RELEASE_NAME" \
        --arg body "$RELEASE_BODY" \
        '{tag_name:$tag, target_commitish:$target, name:$name, body:$body, draft:false, prerelease:true}' \
        | api -X POST -H "$AUTH_HEADER" -H "Content-Type: application/json" \
            "$API/repos/$CI_REPO/releases" -d @-)
    ok "$status" || die "create failed: HTTP $status"
    release_id=$(jq -r '.id // empty' < "$RESP")
    [ -n "$release_id" ] || die "create returned HTTP $status with no release id:"
    echo "[publish] created release id=${release_id}"
else
    echo "[publish] found release id=${release_id}, refreshing name and body"
    status=$(release_patch_payload "$RELEASE_NAME" "$RELEASE_BODY" \
        | api -X PATCH -H "$AUTH_HEADER" -H "Content-Type: application/json" \
            "$API/repos/$CI_REPO/releases/$release_id" -d @-)
    ok "$status" || die "name and body refresh failed: HTTP $status"
    release_assets_json=$(echo "$release_json" | jq -c '.assets // []')
fi

# Which commit the current assets were built from: the tag, which this script
# moves only after a complete swap, so it names the build the release holds
# even after a failed run. When it is the commit about to be published, the
# current set is the same code again (the triggered publish and the 09:05
# fallback cron both fire on a day nothing landed), and renaming it would
# replace the one fallback a bad build leaves with a copy of that bad build.
# Then the current set is replaced in place and `.previous` stays. An
# unreadable tag counts as a different commit: the ordinary path.
published_sha=$(git -C "$ROOT" ls-remote "https://codeberg.org/${CI_REPO}.git" "refs/tags/${TAG}" 2>/dev/null \
    | awk 'NR == 1 { print $1 }') || published_sha=""

upload_names=$(for f in "${ASSETS[@]}"; do basename "$f"; done | jq -R . | jq -sc .)
if [ "$published_sha" = "$CI_COMMIT_SHA" ]; then
    echo "[publish] tag ${TAG} already names ${CI_COMMIT_SHA}: replacing the current set in place, .previous stays"
    previous_names=$(for f in "${ASSETS[@]}"; do previous_name "$(basename "$f")"; done | jq -R . | jq -sc .)
    keep_ids=$(jq -c --argjson prev "$previous_names" '
        [.[] | select(.name as $n | $prev | index($n))] | unique_by(.name) | map(.id)' <<< "$release_assets_json")
else
    # id<TAB>name of the first asset under each name this run uploads, in the
    # release's own order.
    rename_plan=$(jq -r --argjson up "$upload_names" '
        [.[] | select(.name as $n | $up | index($n))]
        | unique_by(.name) | sort_by(.id) | .[] | "\(.id)\t\(.name)"' <<< "$release_assets_json")
    keep_ids=$(cut -f1 <<< "$rename_plan" | jq -R 'select(length > 0) | tonumber' | jq -sc .)
fi
stale_asset_ids=$(jq -r --argjson keep "$keep_ids" '
    .[] | select(.id as $i | $keep | index($i) | not) | .id' <<< "$release_assets_json")

if [ -n "$rename_plan" ]; then
    echo "[publish] keeping the build this run replaces as .previous"
    while IFS=$'\t' read -r asset_id name; do
        [ -n "$asset_id" ] || continue
        # Recorded before the request: a rename that answered 5xx may have
        # landed anyway, and renaming it back to its own name is harmless.
        renamed_ids+=("$asset_id")
        renamed_from+=("$name")
        if ! rename_asset "$asset_id" "$(previous_name "$name")"; then
            echo "[publish] a rename did not get through; nothing has been uploaded."
            restore_names
            echo "[publish] release left at the previous build, names as found. Failing the run."
            exit 1
        fi
    done <<< "$rename_plan"
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
        restore_names
        echo "[publish] upload failed; release left at the previous build."
        exit 1
    fi
    asset_id=$(jq -r '.id // empty' < "$RESP")
    uploaded_ids+=("$asset_id")
    echo "[publish]   → $name  HTTP $status id=${asset_id:-?}"
done

if [ "$release_assets_json" != "[]" ]; then
    echo "[publish] deleting the .previous set this run displaced and any asset it no longer publishes"
    # Forgejo's asset-delete endpoint is
    # /repos/{owner}/{repo}/releases/{release_id}/assets/{attachment_id}.
    # The release_id segment is mandatory — omitting it yields a silent
    # 404 with -sS, which is exactly what happened before 3cece2ce and
    # caused assets to accumulate across runs (12 stale entries on the
    # nightly tag pointing at three different builds).
    while read -r asset_id; do
        [ -n "$asset_id" ] || continue
        delete_asset "$asset_id" || true
    done <<< "$stale_asset_ids"

    # The verdict is the release's own listing, not the statuses above. A 504
    # on a deletion the forge carried out anyway says nothing about the state
    # this run leaves behind (Codeberg #270), and neither does a 204 that
    # somehow did not stick. Ask once what the release holds, and fail on the
    # thing that actually breaks a download: the same name twice, which is the
    # state in which Forgejo serves whichever of the two it picks.
    echo "[publish] reading back the release to check the swap"
    status=$(api -H "$AUTH_HEADER" "$API/repos/$CI_REPO/releases/$release_id")
    ok "$status" || die "could not read the release back after the swap: HTTP $status"
    duplicates=$(jq -r '[(.assets // [])[].name] | group_by(.) | map(select(length > 1) | .[0]) | .[]' < "$RESP")
    if [ -n "$duplicates" ]; then
        printf '%s\n' "$duplicates" | sed 's/^/[publish]   twice: /'
        echo "[publish] a replaced asset survived the swap: the release now holds two"
        echo "[publish] files under that name, and the download URL serves whichever"
        echo "[publish] Forgejo picks. Delete the stale one by hand. Failing the run."
        exit 1
    fi
    # A replaced asset can also survive under a name nothing uploaded this run
    # — a file that has left dist/. No download URL is ambiguous then, so this
    # does not fail the run; but this log is the only place anybody would see
    # it, so it is said out loud.
    remaining_ids=$(jq -r '(.assets // [])[].id' < "$RESP")
    leftover=""
    while read -r asset_id; do
        [ -n "$asset_id" ] || continue
        if grep -qx "$asset_id" <<<"$remaining_ids"; then
            leftover="$leftover $asset_id"
        fi
    done <<< "$stale_asset_ids"
    if [ -n "$leftover" ]; then
        echo "[publish] note: replaced asset(s)$leftover are still on the release, under"
        echo "[publish] a name this build did not republish. No download URL is ambiguous,"
        echo "[publish] so the run stands; delete them by hand when convenient."
    fi
    echo "[publish] swap complete, no name appears twice"
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
