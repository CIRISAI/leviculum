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
# PUBLISH_SH overrides the script under test, which is how the pre-fix
# version is checked to be red: `PUBLISH_SH=<old copy> bash
# scripts/test-publish-nightly.sh`.
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
# A `curl` that understands the five request shapes publish-nightly.sh makes
# and nothing else: the release lookup, the release create, the body PATCH,
# an asset upload and an asset delete. It honours `-o` and `-w` the way curl
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
        -H) shift 2;;
        -F) upload="$2"; shift 2;;
        -d) [ "$2" = "@-" ] && stdin_data=1; shift 2;;
        -sS|-s|-S|-f|--fail|--fail-with-body) shift;;
        *) url="$1"; shift;;
    esac
done
# The request bodies are journalled rather than discarded: the release body
# is the only place a stranger reads what a download was built from, so it is
# an assertion here and not a hope.
[ -n "$stdin_data" ] && cat >> "$D/bodies"

log() { printf '%s\n' "$*" >> "$D/log"; }
bump() { local n; n=$(cat "$D/$1" 2>/dev/null || echo 0); n=$((n + 1)); echo "$n" > "$D/$1"; echo "$n"; }
emit() {  # <status> <body>
    if [ -n "$out" ]; then printf '%s' "$2" > "$out"; else printf '%s' "$2"; fi
    [ -n "$want_code" ] && printf '%s' "$1"
    exit 0
}

case "$url" in
    */releases/tags/*)
        log "LOOKUP"
        if [ -f "$D/no-release" ]; then
            emit 404 '{"errors":["release not found"]}'
        fi
        assets=$(awk -F'\t' '{printf "%s{\"id\":%s,\"name\":\"%s\"}", (NR>1?",":""), $1, $2}' "$D/assets")
        emit 200 "{\"id\":42,\"assets\":[$assets]}"
        ;;
    */assets/*)
        [ "$method" = "DELETE" ] || { echo "fixture curl: unexpected $method on $url" >&2; exit 99; }
        n=$(bump deletes)
        id="${url##*/}"
        if [ "${FAKE_DELETE_FAIL_AT:-}" = "$n" ]; then
            log "DELETE-FAIL $id"
            emit 500 '{"message":"internal server error"}'
        fi
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
cat > "$BIN/git" <<'EOF'
#!/usr/bin/env bash
printf 'GIT %s\n' "$*" >> "$FAKE_DIR/log"
exit 0
EOF
chmod +x "$BIN/curl" "$BIN/git"

# Six assets under the fixture's previous build, named as the README's
# download URLs are: those names are what a failed run must not take away.
OLD_NAMES=(
    leviculum-nightly-amd64.deb leviculum-nightly-amd64.deb.sha256
    leviculum-nightly-arm64.deb leviculum-nightly-arm64.deb.sha256
    leviculum-nightly-amd64.tar.gz leviculum-nightly-amd64.tar.gz.sha256
)

# A tree holding the script under test plus the dist/ it publishes. The
# script derives dist/ from its own location, so the copy is what puts the
# fixture files in front of it.
setup() {  # <case> [empty-dist]
    FAKE_DIR="$WORK/$1"; export FAKE_DIR
    TREE="$WORK/$1/tree"
    mkdir -p "$FAKE_DIR" "$TREE/scripts" "$TREE/dist"
    cp "$PUBLISH_SH" "$TREE/scripts/publish-nightly.sh"
    : > "$FAKE_DIR/log"
    : > "$FAKE_DIR/assets"
    : > "$FAKE_DIR/bodies"
    # The compiler stamp scripts/deb-stamp.sh leaves in the repo root. A
    # version that exists nowhere, so a body naming the host's own compiler
    # instead of the stamped one is visible (Codeberg #305).
    echo "rustc 9.9.9 (fixturec0de 2026-01-01)" > "$TREE/.rustc-version"
    local i=0 n
    for n in "${OLD_NAMES[@]}"; do i=$((i + 1)); printf '%s\t%s\n' "$i" "$n" >> "$FAKE_DIR/assets"; done
    if [ "${2:-}" != "empty-dist" ]; then
        for n in leviculum-nightly-amd64.deb leviculum-nightly-arm64.deb \
                 leviculum-nightly-amd64.tar.gz; do
            echo "fixture payload $n" > "$TREE/dist/$n"
            echo "0000  $n" > "$TREE/dist/$n.sha256"
        done
    fi
}

run_publish() {
    ( cd "$TREE" && PATH="$BIN:$PATH" \
        CI_REPO="Lew_Palm/leviculum" CI_COMMIT_SHA="deadbeefcafe" \
        CODEBERG_TOKEN="fixture-token" LEVICULUM_BUILD_ID="fixture-build" \
        bash "$TREE/scripts/publish-nightly.sh" ) > "$FAKE_DIR/out" 2>&1
    echo $? > "$FAKE_DIR/rc"
}

rc() { cat "$FAKE_DIR/rc"; }
asset_names() { cut -f2 "$FAKE_DIR/assets" | sort; }
asset_ids() { cut -f1 "$FAKE_DIR/assets" | sort -n | tr '\n' ' '; }
dumplog() { echo "  --- request log ---"; sed 's/^/    /' "$FAKE_DIR/log"; echo "  --- script output ---"; sed 's/^/    /' "$FAKE_DIR/out"; }

# --- Case: happy path -----------------------------------------------------
#
# The release ends up holding exactly the files in dist/ and nothing from the
# previous build, the tag moves, and — the ordering claim — no delete is
# issued before the last upload has succeeded.
echo "[case] happy-path"
setup happy-path
run_publish
[ "$(rc)" = "0" ] || fail "exit $(rc), expected 0"
expected=$(find "$TREE/dist" -maxdepth 1 -type f -printf '%f\n' | sort)
[ "$(asset_names)" = "$expected" ] || fail "release holds $(asset_names | tr '\n' ' '), expected $(echo "$expected" | tr '\n' ' ')"
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
# assets must all still be downloadable afterwards.
echo "[case] upload-fails"
before=$failures
setup upload-fails
FAKE_UPLOAD_FAIL_AT=2 run_publish
[ "$(rc)" != "0" ] || fail "exit 0 on a failed upload"
grep -q '\[publish\] done' "$FAKE_DIR/out" && fail "reported 'done' after a failed upload"
[ "$(asset_ids)" = "1 2 3 4 5 6 " ] || fail "release holds ids '$(asset_ids)', expected the six previous assets untouched"
# Ids 1-6 are the previous build's; this run's start at 101, and deleting
# those again is the rollback doing its job.
grep -qE '^DELETE [1-6]$' "$FAKE_DIR/log" && fail "deleted a previous asset although the upload failed"
grep -q '^GIT ' "$FAKE_DIR/log" && fail "moved the tag although the upload failed"
[ "$failures" -eq "$before" ] || dumplog

# --- Case: a delete fails -------------------------------------------------
#
# The new assets are up, so the run is not lost, but a previous asset with
# the same name survives beside them — the accumulation 3cece2ce fixed. The
# run says so, and the tag does not move onto a release nobody has checked.
echo "[case] delete-fails"
before=$failures
setup delete-fails
FAKE_DELETE_FAIL_AT=1 run_publish
[ "$(rc)" != "0" ] || fail "exit 0 although deleting a replaced asset failed"
grep -q '^UPLOAD ' "$FAKE_DIR/log" || fail "expected the uploads to have happened first"
grep -q '^GIT ' "$FAKE_DIR/log" && fail "moved the tag although a replaced asset survived"
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

echo
if [ "$failures" -ne 0 ]; then
    echo "test-publish-nightly: FAILED ($failures assertion(s))"
    exit 1
fi
echo "test-publish-nightly: all cases passed"
