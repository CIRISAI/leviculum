#!/usr/bin/env bash
# Fixture test for the nightly publish step (Codeberg #286).
#
# Every case runs the REAL scripts/publish-nightly.sh against a fake `curl`
# and a fake `git` on PATH, with a fixture release whose asset list lives in
# a file. Nothing here mocks the script's own logic: the injected failure is
# an HTTP status from the forge, and the assertion is on what the release
# holds afterwards and whether the run said so.
#
# The two defects this was written for, both reproduced against master
# 0a953f0c before the fix:
#
#   1. AN UPLOAD THAT FAILS IS NOT NOTICED. The upload curl had no `-f`, no
#      status capture and its output went to /dev/null, so an HTTP 500 left
#      the loop running, the run exited 0 and the log ended in `[publish]
#      done` with a link to the release. Case `upload-fails` injects that
#      500 and asserts a non-zero exit.
#
#   2. THE ASSETS WERE DELETED BEFORE THE NEW ONES EXISTED. A failed upload
#      therefore left the release EMPTY, and the README's hardcoded download
#      URLs 404 until the next successful nightly. The same case asserts the
#      previous assets are all still there afterwards, and `happy-path`
#      asserts the order they are now published in: every upload before any
#      delete.
#
# Codeberg #270 added a third: A DELETE THAT ANSWERED 504 WAS CALLED A FAILED
# SWAP. Pipeline 471 was failed by two gateway timeouts out of 36 deletions
# whose assets the forge had in fact removed. The judgement now comes from a
# read-back of the release, and a 5xx is retried; the three `delete-504-*` and
# `delete-403-*` cases below fix both halves of that — green when the retry
# gets through, red when the duplicate is really there.
#
# PUBLISH_SH overrides the script under test, which is how the pre-fix
# version is checked to be red: `PUBLISH_SH=<old copy> bash
# scripts/test-publish-nightly.sh`.
#
# Codeberg #323 added a fourth: A GOOD BUILD WAS GONE THE MOMENT A BAD ONE
# REPLACED IT. The swap deleted every replaced asset, so a run that published a
# broken build left nothing to fall back to. The replaced set is now renamed to
# `<name>.previous.<ext>` before the uploads and kept; the set before that is
# what gets deleted. `happy-path` and `first-run-no-previous` assert the end
# state and the order, `upload-fails` and `rename-504-gives-up` that a failed
# run leaves every name where it was.
#
# Usage: bash scripts/test-publish-nightly.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PUBLISH_SH="${PUBLISH_SH:-$SCRIPT_DIR/publish-nightly.sh}"
[ -f "$PUBLISH_SH" ] || { echo "no script under test at $PUBLISH_SH"; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

failures=0
fail() { echo "  FAIL: $*"; failures=$((failures + 1)); }

# --- The fixture forge ----------------------------------------------------
#
# A `curl` that understands the six request shapes publish-nightly.sh makes
# and nothing else: the release lookup, the release create, the body PATCH,
# an asset upload, an asset rename (a PATCH of its name) and an asset delete. It honours `-o` and `-w` the way curl
# does (with both, only the status reaches stdout) and, like the real thing
# without `-f`, it exits 0 on a 4xx or 5xx — that is the whole bug.
#
# State: $FAKE_DIR/assets is the release's asset list, one `id<TAB>name` per
# line; $FAKE_DIR/log is the request journal the assertions read.
BIN="$WORK/bin"
mkdir -p "$BIN"
cat > "$BIN/curl" <<'EOF'
#!/usr/bin/env bash
set -uo pipefail
D="$FAKE_DIR"
out=""; want_code=""; method="GET"; upload=""; url=""; stdin_data=""
while [ $# -gt 0 ]; do
    case "$1" in
        -o) out="$2"; shift 2;;
        -w) want_code=1; shift 2;;
        -X) method="$2"; shift 2;;
        -H) case "$2" in Authorization:*) printf '%s\n' "$2" >> "$D/auth";; esac; shift 2;;
        -F) upload="$2"; shift 2;;
        -d) [ "$2" = "@-" ] && stdin_data=1; shift 2;;
        -sS|-s|-S|-f|--fail|--fail-with-body) shift;;
        *) url="$1"; shift;;
    esac
done
# The request bodies are journalled rather than discarded: the release body
# is the only place a stranger reads what a download was built from, so it is
# an assertion here and not a hope.
body=""
if [ -n "$stdin_data" ]; then
    body=$(cat)
    printf '%s\n' "$body" >> "$D/bodies"
fi

log() { printf '%s\n' "$*" >> "$D/log"; }
bump() { local n; n=$(cat "$D/$1" 2>/dev/null || echo 0); n=$((n + 1)); echo "$n" > "$D/$1"; echo "$n"; }
emit() {  # <status> <body>
    if [ -n "$out" ]; then printf '%s' "$2" > "$out"; else printf '%s' "$2"; fi
    [ -n "$want_code" ] && printf '%s' "$1"
    exit 0
}

release_json() {  # the release as the forge reports it, assets and all
    local assets
    assets=$(awk -F'\t' '{printf "%s{\"id\":%s,\"name\":\"%s\"}", (NR>1?",":""), $1, $2}' "$D/assets")
    printf '{"id":42,"assets":[%s]}' "$assets"
}

case "$url" in
    */releases/tags/*)
        log "LOOKUP"
        if [ -f "$D/no-release" ]; then
            emit 404 '{"errors":["release not found"]}'
        fi
        emit 200 "$(release_json)"
        ;;
    */assets/*)
        if [ "$method" = "PATCH" ]; then
            n=$(bump renames)
            id="${url##*/}"
            new=$(printf '%s' "$body" | jq -r '.name')
            # FAKE_RENAME_FAIL_AT counts rename REQUESTS, as the delete knob
            # below counts deletes: a retry is a further number.
            for f in ${FAKE_RENAME_FAIL_AT:-}; do
                if [ "$f" = "$n" ]; then
                    log "RENAME-FAIL $id"
                    emit "${FAKE_RENAME_STATUS:-500}" '{"message":"gateway timeout"}'
                fi
            done
            if ! grep -q "^${id}	" "$D/assets"; then
                log "RENAME-MISSING $id"
                emit 404 '{"errors":["attachment does not exist"]}'
            fi
            old=$(awk -F'\t' -v id="$id" '$1 == id { print $2 }' "$D/assets")
            awk -F'\t' -v OFS='\t' -v id="$id" -v new="$new" '$1 == id { $2 = new } { print }' \
                "$D/assets" > "$D/assets.new"; mv "$D/assets.new" "$D/assets"
            log "RENAME $id $old -> $new"
            emit 200 "{\"id\":$id,\"name\":\"$new\"}"
        fi
        [ "$method" = "DELETE" ] || { echo "fixture curl: unexpected $method on $url" >&2; exit 99; }
        n=$(bump deletes)
        id="${url##*/}"
        # FAKE_DELETE_FAIL_AT is a space-separated list of delete REQUEST
        # numbers, not asset ids: a retry of the same asset is a further
        # number, which is how "504 then 204" and "504 every time" are told
        # apart. FAKE_DELETE_STATUS picks the answer (504 is the one the forge
        # gave in pipeline 471).
        for f in ${FAKE_DELETE_FAIL_AT:-}; do
            if [ "$f" = "$n" ]; then
                log "DELETE-FAIL $id"
                emit "${FAKE_DELETE_STATUS:-500}" '{"message":"gateway timeout"}'
            fi
        done
        if ! grep -q "^${id}	" "$D/assets"; then
            log "DELETE-MISSING $id"
            emit 404 '{"errors":["attachment does not exist"]}'
        fi
        grep -v "^${id}	" "$D/assets" > "$D/assets.new"; mv "$D/assets.new" "$D/assets"
        log "DELETE $id"
        emit 204 ''
        ;;
    *assets?name=*)
        n=$(bump uploads)
        name="${url##*name=}"
        src="${upload#attachment=@}"
        if [ ! -f "$src" ]; then
            echo "curl: (26) Failed to open/read local data from file: $src" >&2
            emit 000 ''
        fi
        if [ "${FAKE_UPLOAD_FAIL_AT:-}" = "$n" ]; then
            log "UPLOAD-FAIL $name"
            emit 500 '{"message":"internal server error"}'
        fi
        id=$(bump next_id)
        id=$((100 + id))
        printf '%s\t%s\n' "$id" "$name" >> "$D/assets"
        log "UPLOAD $name id=$id"
        emit 201 "{\"id\":$id,\"name\":\"$name\"}"
        ;;
    */releases)
        [ "$method" = "POST" ] || { echo "fixture curl: unexpected $method on $url" >&2; exit 99; }
        log "CREATE"
        rm -f "$D/no-release"
        emit 201 '{"id":42}'
        ;;
    */releases/*)
        # The read-back after the swap: since Codeberg #270 the publisher's
        # verdict on the delete loop is this listing, not the delete statuses.
        if [ "$method" = "GET" ]; then
            log "LIST"
            emit 200 "$(release_json)"
        fi
        [ "$method" = "PATCH" ] || { echo "fixture curl: unexpected $method on $url" >&2; exit 99; }
        log "PATCH"
        if [ -n "${FAKE_PATCH_FAIL:-}" ]; then
            emit 500 '{"message":"internal server error"}'
        fi
        emit 200 '{"id":42}'
        ;;
esac
echo "fixture curl: unhandled request $method $url" >&2
exit 99
EOF

# The tag push must not reach a real remote, and whether it happened at all
# is an assertion: a run that could not publish its assets must not move the
# tag that names them.
#
# It also answers the queries scripts/check-nightly-green.sh makes, because
# since Codeberg #312 the publish step begins by reading the tier-2 nightly's
# verdict off the remote. Only a PUSH is logged as `GIT `; every other query
# is logged as `GITQ `, so the assertions below still mean "the tag moved"
# rather than "git was invoked at all".
cat > "$BIN/git" <<'EOF'
#!/usr/bin/env bash
set -uo pipefail
printf 'GITQ %s\n' "$*" >> "$FAKE_DIR/log"
# The subcommand is the first argument that is one: `-C <dir>` and any number
# of `-c key=value` pairs come before it (publish-nightly.sh's tag push passes
# two of the latter to keep the token out of the remote URL).
sub=""
for a in "$@"; do
    case "$a" in
        ls-remote|fetch|merge-base|push) sub="$a"; break ;;
    esac
done
case "$sub" in
    ls-remote)
        # The publisher asks where the `nightly` tag points (#323); the gate
        # asks for the green refs.
        case "$*" in
            *refs/tags/*) cat "$FAKE_DIR/tag-ref" 2>/dev/null ;;
            *) cat "$FAKE_DIR/green-refs" 2>/dev/null ;;
        esac
        exit 0
        ;;
    fetch)
        exit 0
        ;;
    merge-base)
        # merge-base --is-ancestor <commit> <ref-commit>
        seen=0; a=""; b=""
        for x in "$@"; do
            if [ "$seen" = "1" ] && [ -z "$a" ]; then a="$x"; continue; fi
            if [ -n "$a" ] && [ -z "$b" ]; then b="$x"; continue; fi
            [ "$x" = "--is-ancestor" ] && seen=1
        done
        [ "$a" = "$b" ] && exit 0
        exit 1
        ;;
    push)
        printf 'GIT %s\n' "$*" >> "$FAKE_DIR/log"
        exit 0
        ;;
esac
exit 0
EOF
# The retry backoff, made free. A stub rather than a knob in the script: the
# production path keeps one behaviour, and the wait is still an assertion here
# because every skipped one is logged.
cat > "$BIN/sleep" <<'EOF'
#!/usr/bin/env bash
printf 'SLEEP %s\n' "$*" >> "$FAKE_DIR/log"
exit 0
EOF
chmod +x "$BIN/curl" "$BIN/git" "$BIN/sleep"

# Six assets under the fixture's previous build, named as the README's
# download URLs are: those names are what a failed run must not take away.
OLD_NAMES=(
    leviculum-nightly-amd64.deb leviculum-nightly-amd64.deb.sha256
    leviculum-nightly-arm64.deb leviculum-nightly-arm64.deb.sha256
    leviculum-nightly-amd64.tar.gz leviculum-nightly-amd64.tar.gz.sha256
)

# The same six one build older, as #323 leaves them on the release: the
# fallback generation, which the next successful run deletes.
PREV_NAMES=(
    leviculum-nightly-amd64.previous.deb leviculum-nightly-amd64.previous.deb.sha256
    leviculum-nightly-arm64.previous.deb leviculum-nightly-arm64.previous.deb.sha256
    leviculum-nightly-amd64.previous.tar.gz leviculum-nightly-amd64.previous.tar.gz.sha256
)

# A tree holding the script under test plus the dist/ it publishes. The
# script derives dist/ from its own location, so the copy is what puts the
# fixture files in front of it. `with-previous` adds PREV_NAMES as ids 7-12.
setup() {  # <case> [empty-dist|with-previous]
    FAKE_DIR="$WORK/$1"; export FAKE_DIR
    TREE="$WORK/$1/tree"
    mkdir -p "$FAKE_DIR" "$TREE/scripts" "$TREE/dist"
    cp "$PUBLISH_SH" "$TREE/scripts/publish-nightly.sh"
    # Codeberg #312: the publish step now begins by reading the nightly's
    # verdict, so the script it calls has to be in the fixture tree too.
    cp "$SCRIPT_DIR/check-nightly-green.sh" "$TREE/scripts/check-nightly-green.sh"
    : > "$FAKE_DIR/log"
    : > "$FAKE_DIR/assets"
    : > "$FAKE_DIR/bodies"
    # A green tier-2 nightly from a minute ago, naming the commit under
    # publish. Every case below except the two that are about the gate itself
    # is about what happens AFTER it, so the default fixture is a covered one.
    printf '%s\trefs/nightly/green/%s\n' "deadbeefcafe" "$(date -u +%Y%m%dT%H%M%SZ)" \
        > "$FAKE_DIR/green-refs"
    # The compiler stamp scripts/deb-stamp.sh leaves in the repo root. A
    # version that exists nowhere, so a body naming the host's own compiler
    # instead of the stamped one is visible (Codeberg #305).
    echo "rustc 9.9.9 (fixturec0de 2026-01-01)" > "$TREE/.rustc-version"
    local i=0 n
    for n in "${OLD_NAMES[@]}"; do i=$((i + 1)); printf '%s\t%s\n' "$i" "$n" >> "$FAKE_DIR/assets"; done
    if [ "${2:-}" = "with-previous" ]; then
        for n in "${PREV_NAMES[@]}"; do i=$((i + 1)); printf '%s\t%s\n' "$i" "$n" >> "$FAKE_DIR/assets"; done
    fi
    if [ "${2:-}" != "empty-dist" ]; then
        for n in leviculum-nightly-amd64.deb leviculum-nightly-arm64.deb \
                 leviculum-nightly-amd64.tar.gz; do
            echo "fixture payload $n" > "$TREE/dist/$n"
            echo "0000  $n" > "$TREE/dist/$n.sha256"
        done
    fi
}

# The build id has deb-stamp.sh's shape, with a date that is not today, so a
# release name taken from the clock instead of the id is visible (PUB-0023).
run_publish() {
    ( cd "$TREE" && PATH="$BIN:$PATH" \
        CI_REPO="Lew_Palm/leviculum" CI_COMMIT_SHA="deadbeefcafe" \
        CODEBERG_TOKEN="fixture-token" LEVICULUM_BUILD_ID="${FIXTURE_BUILD_ID:-nightly.20260102-deadbee+3}" \
        bash "$TREE/scripts/publish-nightly.sh" ) > "$FAKE_DIR/out" 2>&1
    echo $? > "$FAKE_DIR/rc"
}

rc() { cat "$FAKE_DIR/rc"; }
asset_names() { cut -f2 "$FAKE_DIR/assets" | sort; }
asset_ids() { cut -f1 "$FAKE_DIR/assets" | sort -n | tr '\n' ' '; }
# What the release must hold after a successful run: every file in dist/, and
# one `.previous` copy of each of the six the run replaced.
expected_after_swap() {
    { find "$TREE/dist" -maxdepth 1 -type f -printf '%f\n'; printf '%s\n' "${PREV_NAMES[@]}"; } | sort
}
dumplog() { echo "  --- request log ---"; sed 's/^/    /' "$FAKE_DIR/log"; echo "  --- script output ---"; sed 's/^/    /' "$FAKE_DIR/out"; }

# --- Case: happy path -----------------------------------------------------
#
# The release ends up holding exactly the files in dist/ plus the build they
# replaced under `.previous` names (#323); the generation before that is gone,
# and so is an asset this build no longer publishes (id 13). The tag moves,
# and the ordering claims: every rename before the first upload, and no delete
# issued before the last upload has succeeded.
echo "[case] happy-path"
setup happy-path with-previous
printf '13\tlnomad-nightly-amd64.deb\n' >> "$FAKE_DIR/assets"
run_publish
[ "$(rc)" = "0" ] || fail "exit $(rc), expected 0"
[ "$(asset_names)" = "$(expected_after_swap)" ] || fail "release holds $(asset_names | tr '\n' ' '), expected $(expected_after_swap | tr '\n' ' ')"
for id in 1 2 3 4 5 6; do
    grep -qE "^RENAME $id .* -> .*\.previous\." "$FAKE_DIR/log" || fail "asset $id was not renamed to its .previous name"
done
for id in 7 8 9 10 11 12 13; do
    grep -qx "DELETE $id" "$FAKE_DIR/log" || fail "stale asset $id was not deleted"
done
grep -qE '^DELETE [1-6]$' "$FAKE_DIR/log" && fail "deleted a replaced asset instead of keeping it as .previous"
last_rename=$(grep -n '^RENAME ' "$FAKE_DIR/log" | tail -1 | cut -d: -f1)
first_upload=$(grep -n '^UPLOAD ' "$FAKE_DIR/log" | head -1 | cut -d: -f1)
if [ -z "$last_rename" ] || [ -z "$first_upload" ]; then
    fail "expected both renames and uploads in the log"
elif [ "$last_rename" -gt "$first_upload" ]; then
    fail "an asset was renamed at line $last_rename, after the first upload at line $first_upload"
fi
grep -q 'leviculum-nightly-amd64.previous.deb' "$FAKE_DIR/bodies" \
    || fail "the release body does not name the .previous fallback"
grep -q '^GIT ' "$FAKE_DIR/log" || fail "tag was not pushed"
grep -q 'built with .rustc 9.9.9 (fixturec0de 2026-01-01).' "$FAKE_DIR/bodies" \
    || fail "the release body does not name the compiler the assets were built with"
first_delete=$(grep -n '^DELETE ' "$FAKE_DIR/log" | head -1 | cut -d: -f1)
last_upload=$(grep -n '^UPLOAD ' "$FAKE_DIR/log" | tail -1 | cut -d: -f1)
if [ -z "$first_delete" ] || [ -z "$last_upload" ]; then
    fail "expected both uploads and deletes in the log"
elif [ "$first_delete" -lt "$last_upload" ]; then
    fail "an asset was deleted at line $first_delete, before the last upload at line $last_upload"
fi
[ "$failures" -eq 0 ] || dumplog

# --- Case: an upload fails ------------------------------------------------
#
# Defect 1 and defect 2 together: the run must fail, and the previous build's
# assets must all still be downloadable afterwards — under their own names
# (#323: the renames are undone) and with the fallback generation beside them.
# --- Case: the release PATCH names the build -----------------------------
#
# PUB-0023: the releases page headed the rolling release with its creation
# day, months old, because the refresh PATCHed the body alone. The PATCH now
# carries the name, with the build's date and commit, and the body opens with
# the `Current build:` line instead of ending with it. The payload is read
# from the request journal, so this is what the forge received.
echo "[case] release-patch-names-the-build"
before=$failures
setup release-patch
run_publish
[ "$(rc)" = "0" ] || fail "exit $(rc), expected 0"
patch=$(jq -c 'select(has("body"))' "$FAKE_DIR/bodies")
[ "$(jq -r '.name' <<< "$patch")" = "Nightly builds, 2026-01-02 (deadbee)" ] \
    || fail "the release PATCH names '$(jq -r '.name' <<< "$patch")', expected the build's date and commit"
first_line=$(jq -r '.body' <<< "$patch" | head -1)
[[ "$first_line" == "Current build: \`nightly.20260102-deadbee+3\`"* ]] \
    || fail "the release body opens with '$first_line', expected the Current build line"
[ "$(jq -r 'keys | join(",")' <<< "$patch")" = "body,name" ] \
    || fail "the release PATCH sends $(jq -r 'keys | join(",")' <<< "$patch"), expected body and name only"
# An id without a date (a hand run) names the day it was published.
setup release-patch-no-date
today_before=$(date -u +%Y-%m-%d)
FIXTURE_BUILD_ID="hand-build" run_publish
today_after=$(date -u +%Y-%m-%d)
name=$(jq -r 'select(has("body")) | .name' "$FAKE_DIR/bodies")
case "$name" in
    "Nightly builds, $today_before (deadbee)" | "Nightly builds, $today_after (deadbee)") ;;
    *) fail "an id without a date named the release '$name', expected today" ;;
esac
[ "$failures" -eq "$before" ] || dumplog

echo "[case] upload-fails"
before=$failures
setup upload-fails with-previous
cp "$FAKE_DIR/assets" "$FAKE_DIR/assets.before"
FAKE_UPLOAD_FAIL_AT=2 run_publish
[ "$(rc)" != "0" ] || fail "exit 0 on a failed upload"
grep -q '\[publish\] done' "$FAKE_DIR/out" && fail "reported 'done' after a failed upload"
[ "$(asset_ids)" = "1 2 3 4 5 6 7 8 9 10 11 12 " ] || fail "release holds ids '$(asset_ids)', expected the twelve previous assets untouched"
[ "$(sort -n "$FAKE_DIR/assets")" = "$(sort -n "$FAKE_DIR/assets.before")" ] \
    || fail "names changed: $(sort -n "$FAKE_DIR/assets" | tr '\t\n' ': ')"
grep -q '^RENAME ' "$FAKE_DIR/log" || fail "nothing was renamed before the uploads"
# Ids 1-12 are the previous builds'; this run's start at 101, and deleting
# those again is the rollback doing its job.
grep -qE '^DELETE ([1-9]|1[0-2])$' "$FAKE_DIR/log" && fail "deleted a previous asset although the upload failed"
grep -q '^GIT ' "$FAKE_DIR/log" && fail "moved the tag although the upload failed"
[ "$failures" -eq "$before" ] || dumplog

# --- Case: a delete answers 504 and the retry gets through ----------------
#
# Codeberg #270: pipeline 471 was failed by two 504s out of 36 deletions whose
# assets were in fact gone. The first delete here answers 504, the retry 204,
# and the release ends up exactly as on the happy path — so the run is green.
# The retry is asserted as a request, not inferred from the exit code: seven
# delete requests for six assets, and a backoff waited before the seventh.
echo "[case] delete-504-then-retry"
before=$failures
setup delete-504-retry with-previous
FAKE_DELETE_FAIL_AT=1 FAKE_DELETE_STATUS=504 run_publish
[ "$(rc)" = "0" ] || fail "exit $(rc) although the retried delete succeeded"
[ "$(asset_names)" = "$(expected_after_swap)" ] || fail "release holds $(asset_names | tr '\n' ' '), expected $(expected_after_swap | tr '\n' ' ')"
[ "$(cat "$FAKE_DIR/deletes")" = "7" ] || fail "$(cat "$FAKE_DIR/deletes") delete requests for six assets, expected 7 (one retried)"
grep -q '^SLEEP ' "$FAKE_DIR/log" || fail "retried without any backoff"
grep -q '^LIST' "$FAKE_DIR/log" || fail "the release was never read back"
grep -q '^GIT ' "$FAKE_DIR/log" || fail "tag was not pushed after a swap that worked"
[ "$failures" -eq "$before" ] || dumplog

# --- Case: a delete answers 504 every time --------------------------------
#
# Same answer, other end state: the asset is still there when the retries run
# out, and the release holds two files under one name — the accumulation
# 3cece2ce fixed. The listing is what says so, and the run fails with the
# wording that names the manual repair.
echo "[case] delete-504-survives"
before=$failures
setup delete-504-survives with-previous
FAKE_DELETE_FAIL_AT="1 2 3 4" FAKE_DELETE_STATUS=504 run_publish
[ "$(rc)" != "0" ] || fail "exit 0 although a replaced asset survived the swap"
grep -q 'a replaced asset survived the swap' "$FAKE_DIR/out" \
    || fail "the failure does not name what is wrong with the release"
grep -q 'twice: leviculum-nightly-amd64.previous.deb' "$FAKE_DIR/out" \
    || fail "the failure does not name the asset that appears twice"
grep -q '^LIST' "$FAKE_DIR/log" || fail "the verdict was taken without reading the release back"
grep -q '^UPLOAD ' "$FAKE_DIR/log" || fail "expected the uploads to have happened first"
grep -q '^GIT ' "$FAKE_DIR/log" && fail "moved the tag although a replaced asset survived"
[ "$failures" -eq "$before" ] || dumplog

# --- Case: a delete is refused, not timed out -----------------------------
#
# 403 is a refusal, not a hiccup; retrying it only spends time. One request,
# and the listing still decides — here it shows the duplicate, so red.
echo "[case] delete-403-not-retried"
before=$failures
setup delete-403 with-previous
FAKE_DELETE_FAIL_AT=1 FAKE_DELETE_STATUS=403 run_publish
[ "$(rc)" != "0" ] || fail "exit 0 although a replaced asset survived the swap"
[ "$(cat "$FAKE_DIR/deletes")" = "6" ] || fail "$(cat "$FAKE_DIR/deletes") delete requests, expected 6: a 403 is not retried"
grep -q 'a replaced asset survived the swap' "$FAKE_DIR/out" \
    || fail "the failure does not name what is wrong with the release"
grep -q '^GIT ' "$FAKE_DIR/log" && fail "moved the tag although a replaced asset survived"
[ "$failures" -eq "$before" ] || dumplog

# --- Case: the first run after #323 ---------------------------------------
#
# The release holds no `.previous` set yet. Nothing is deleted, the six
# replaced assets become the first fallback generation, and the read-back
# still runs although the delete set is empty.
echo "[case] first-run-no-previous"
before=$failures
setup first-run
run_publish
[ "$(rc)" = "0" ] || fail "exit $(rc) on the first run after #323"
[ "$(asset_names)" = "$(expected_after_swap)" ] || fail "release holds $(asset_names | tr '\n' ' '), expected $(expected_after_swap | tr '\n' ' ')"
grep -q '^DELETE' "$FAKE_DIR/log" && fail "deleted an asset although nothing was stale"
grep -q '^LIST' "$FAKE_DIR/log" || fail "the release was never read back"
grep -q '^GIT ' "$FAKE_DIR/log" || fail "tag was not pushed"
[ "$failures" -eq "$before" ] || dumplog

# --- Case: the same commit published twice --------------------------------
#
# The triggered publish and the 09:05 fallback cron both fire on a day nothing
# landed, so the second run publishes the commit the tag already names. The
# current set is then the same code again; renaming it would throw away the
# fallback generation for a copy of what replaces it. So: no rename, the
# current set replaced in place, the `.previous` set (ids 7-12) untouched.
echo "[case] republish-same-commit"
before=$failures
setup same-commit with-previous
printf 'deadbeefcafe\trefs/tags/nightly\n' > "$FAKE_DIR/tag-ref"
run_publish
[ "$(rc)" = "0" ] || fail "exit $(rc) republishing the same commit"
[ "$(asset_names)" = "$(expected_after_swap)" ] || fail "release holds $(asset_names | tr '\n' ' '), expected $(expected_after_swap | tr '\n' ' ')"
grep -q '^RENAME' "$FAKE_DIR/log" && fail "renamed the current set although it is the commit being published"
for id in 7 8 9 10 11 12; do
    grep -q "^${id}	" "$FAKE_DIR/assets" || fail "the .previous asset $id was not kept"
done
for id in 1 2 3 4 5 6; do
    grep -qx "DELETE $id" "$FAKE_DIR/log" || fail "the replaced asset $id was not deleted"
done
grep -q '^GIT ' "$FAKE_DIR/log" || fail "tag was not pushed"
[ "$failures" -eq "$before" ] || dumplog

# --- Case: a rename answers 504 every time --------------------------------
#
# A rename is retried on a gateway error like a delete is. When the retries
# run out, the renames this run made are undone and the run fails BEFORE the
# first upload: the release keeps every asset under the name it had, nothing
# is half-renamed, nothing is uploaded, the tag stays.
echo "[case] rename-504-gives-up"
before=$failures
setup rename-504 with-previous
cp "$FAKE_DIR/assets" "$FAKE_DIR/assets.before"
FAKE_RENAME_FAIL_AT="3 4 5 6" FAKE_RENAME_STATUS=504 run_publish
[ "$(rc)" != "0" ] || fail "exit 0 although a rename never got through"
[ "$(sort -n "$FAKE_DIR/assets")" = "$(sort -n "$FAKE_DIR/assets.before")" ] \
    || fail "names changed: $(sort -n "$FAKE_DIR/assets" | tr '\t\n' ': ')"
grep -q '^SLEEP ' "$FAKE_DIR/log" || fail "retried without any backoff"
[ "$(grep -c '^RENAME-FAIL ' "$FAKE_DIR/log")" = "4" ] || fail "expected four attempts at the failing rename"
grep -q '^UPLOAD' "$FAKE_DIR/log" && fail "uploaded although the renames had failed"
grep -q '^DELETE' "$FAKE_DIR/log" && fail "deleted an asset although the renames had failed"
grep -q '^GIT ' "$FAKE_DIR/log" && fail "moved the tag although nothing was published"
[ "$failures" -eq "$before" ] || dumplog

# --- Case: dist/ is empty -------------------------------------------------
#
# Nothing to publish is not "publish nothing": deleting the previous assets
# here empties the release just as surely as a failed upload does.
echo "[case] empty-dist"
before=$failures
setup empty-dist empty-dist
run_publish
[ "$(rc)" != "0" ] || fail "exit 0 with an empty dist/"
[ "$(asset_ids)" = "1 2 3 4 5 6 " ] || fail "release holds ids '$(asset_ids)', expected the six previous assets untouched"
grep -q '^GIT ' "$FAKE_DIR/log" && fail "moved the tag with nothing published"
[ "$failures" -eq "$before" ] || dumplog

# --- Case: no release yet -------------------------------------------------
#
# The create path has no previous assets to keep, and must still publish and
# move the tag.
echo "[case] creates-release"
before=$failures
setup creates-release
: > "$FAKE_DIR/assets"
touch "$FAKE_DIR/no-release"
run_publish
[ "$(rc)" = "0" ] || fail "exit $(rc) on the create path, expected 0"
grep -q '^CREATE' "$FAKE_DIR/log" || fail "no release was created"
expected=$(find "$TREE/dist" -maxdepth 1 -type f -printf '%f\n' | sort)
[ "$(asset_names)" = "$expected" ] || fail "release holds $(asset_names | tr '\n' ' '), expected $(echo "$expected" | tr '\n' ' ')"
grep -q '^DELETE' "$FAKE_DIR/log" && fail "deleted an asset on a release that had none"
grep -q '^GIT ' "$FAKE_DIR/log" || fail "tag was not pushed"
[ "$failures" -eq "$before" ] || dumplog

# --- Case: no green nightly covers the commit -----------------------------
#
# Codeberg #312, end to end. `rnsd_interop` runs in no forge pipeline — it
# needs the reference/Reticulum submodule and both pipelines clone without
# submodules (#300) — so the interop verdict is imported from the tier-2
# nightly as a ref, and a commit no green ref covers must not reach the
# releases page at all. The assertion is that the refusal happens BEFORE the
# forge is touched: not one request, not one asset moved, and the tag where it
# was.
echo "[case] refuses-without-a-green-nightly"
before=$failures
setup no-nightly
: > "$FAKE_DIR/green-refs"
run_publish
[ "$(rc)" != "0" ] || fail "exit 0 with no green nightly behind the commit"
grep -q 'REFUSED (NO-SIGNAL)' "$FAKE_DIR/out" || fail "the refusal does not name which condition failed"
grep -qE '^(LOOKUP|CREATE|PATCH|UPLOAD|DELETE|RENAME)' "$FAKE_DIR/log" && fail "the forge was contacted before the gate refused"
[ "$(asset_ids)" = "1 2 3 4 5 6 " ] || fail "release holds ids '$(asset_ids)', expected the six previous assets untouched"
grep -q '^GIT ' "$FAKE_DIR/log" && fail "moved the tag although nothing was published"
[ "$failures" -eq "$before" ] || dumplog

# --- Case: a human overrides the refusal ----------------------------------
#
# The override has to actually work, or the first forge outage makes somebody
# comment the gate out and it never comes back.
echo "[case] override-publishes-anyway"
before=$failures
setup override
: > "$FAKE_DIR/green-refs"
( cd "$TREE" && PATH="$BIN:$PATH" \
    CI_REPO="Lew_Palm/leviculum" CI_COMMIT_SHA="deadbeefcafe" \
    CODEBERG_TOKEN="fixture-token" LEVICULUM_BUILD_ID="fixture-build" \
    LEVICULUM_PUBLISH_WITHOUT_NIGHTLY="nightly host down, release cut by hand" \
    bash "$TREE/scripts/publish-nightly.sh" ) > "$FAKE_DIR/out" 2>&1
echo $? > "$FAKE_DIR/rc"
[ "$(rc)" = "0" ] || fail "exit $(rc) with a reasoned override"
grep -q 'OVERRIDDEN' "$FAKE_DIR/out" || fail "the override did not announce itself in the log"
grep -q 'nightly host down, release cut by hand' "$FAKE_DIR/out" \
    || fail "the reason was not recorded with the build it excused"
[ "$(asset_names)" = "$(expected_after_swap)" ] || fail "release holds $(asset_names | tr '\n' ' '), expected $(expected_after_swap | tr '\n' ' ')"
[ "$failures" -eq "$before" ] || dumplog

# --- Case: dry run --------------------------------------------------------
#
# 517: `just nightly-publish` is exercised end to end with
# LEVICULUM_PUBLISH_DRY_RUN=1, so the knob has to keep its promise: the gate
# still runs, every asset is named, and the forge sees nothing at all.
echo "[case] dry-run"
before=$failures
setup dry-run
( cd "$TREE" && PATH="$BIN:$PATH" \
    CI_REPO="Lew_Palm/leviculum" CI_COMMIT_SHA="deadbeefcafe" \
    CODEBERG_TOKEN="fixture-token" LEVICULUM_BUILD_ID="fixture-build" \
    LEVICULUM_PUBLISH_DRY_RUN=1 \
    bash "$TREE/scripts/publish-nightly.sh" ) > "$FAKE_DIR/out" 2>&1
echo $? > "$FAKE_DIR/rc"
[ "$(rc)" = "0" ] || fail "exit $(rc) on a dry run"
grep -q 'ACCEPTED' "$FAKE_DIR/out" || fail "the dry run skipped the nightly gate"
grep -q 'would upload leviculum-nightly-amd64.deb ' "$FAKE_DIR/out" || fail "the dry run does not name the assets it would upload"
grep -q 'would rename leviculum-nightly-amd64.deb → leviculum-nightly-amd64.previous.deb' "$FAKE_DIR/out" \
    || fail "the dry run does not name the renames it would do"
grep -qE '^(LOOKUP|CREATE|PATCH|UPLOAD|DELETE|RENAME)' "$FAKE_DIR/log" && fail "the dry run made a forge request"
grep -q '^GIT ' "$FAKE_DIR/log" && fail "the dry run moved the tag"
[ "$(asset_ids)" = "1 2 3 4 5 6 " ] || fail "release holds ids '$(asset_ids)', expected the six previous assets untouched"
[ "$failures" -eq "$before" ] || dumplog

# --- Case: the token from a file ------------------------------------------
#
# 517: on the nightly host there is no Woodpecker secret; the token is a file,
# read only when CODEBERG_TOKEN is unset, and never printed.
echo "[case] token-from-file"
before=$failures
setup token-file
printf 'file-token-517\n' > "$FAKE_DIR/token"
( cd "$TREE" && env -u CODEBERG_TOKEN PATH="$BIN:$PATH" \
    CI_REPO="Lew_Palm/leviculum" CI_COMMIT_SHA="deadbeefcafe" \
    LEVICULUM_BUILD_ID="fixture-build" \
    LEVICULUM_CODEBERG_TOKEN_FILE="$FAKE_DIR/token" \
    bash "$TREE/scripts/publish-nightly.sh" ) > "$FAKE_DIR/out" 2>&1
echo $? > "$FAKE_DIR/rc"
[ "$(rc)" = "0" ] || fail "exit $(rc) with the token in a file"
grep -qx 'Authorization: token file-token-517' "$FAKE_DIR/auth" || fail "the requests did not carry the token from the file"
grep -q 'file-token-517' "$FAKE_DIR/out" && fail "the token was printed"
[ "$failures" -eq "$before" ] || dumplog

echo
if [ "$failures" -ne 0 ]; then
    echo "test-publish-nightly: FAILED ($failures assertion(s))"
    exit 1
fi
echo "test-publish-nightly: all cases passed"
