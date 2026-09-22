#!/usr/bin/env bash
# Fixture test for the two halves of the nightly publish signal (Codeberg #312).
#
# `scripts/nightly-green-ref.sh` runs once a night on a machine nobody is
# watching, and `scripts/check-nightly-green.sh` runs inside a cron-only
# pipeline step that no push gate reaches — the same blind spot that let the
# rolling release stand still for five weeks (#434, and the `package`/`publish`
# steps' own note in `.woodpecker/nightly.yml`). Neither script can be
# exercised by using it, so both are driven here against a fake git.
#
# Every case runs the REAL script. What is faked is the remote: a `git` on
# PATH that answers `ls-remote`, `fetch`, `merge-base --is-ancestor` and
# `push` out of fixture files. Nothing mocks either script's own logic.
#
# The properties under test are the ones a reader of a refusal depends on:
#
#   * each of the three refusals fires on its own condition and NAMES it
#     (NO-SIGNAL, NOT-COVERED, TOO-OLD) — a gate that fails closed silently is
#     the failure mode this whole mechanism exists to end;
#   * the newest ref that COVERS the commit is the one judged, not the newest
#     ref on the remote;
#   * the override works and requires a reason rather than a flag;
#   * a red night, a red `rnsd_interop`, an ABSENT `rnsd_interop` and a
#     manifest belonging to another commit each push no ref at all.
#
# ~1 s, no network, no build.
#
# Usage: bash scripts/test-nightly-green.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CHECK_SH="${CHECK_SH:-$SCRIPT_DIR/check-nightly-green.sh}"
REF_SH="${REF_SH:-$SCRIPT_DIR/nightly-green-ref.sh}"
[ -f "$CHECK_SH" ] || { echo "no script under test at $CHECK_SH"; exit 1; }
[ -f "$REF_SH" ]   || { echo "no script under test at $REF_SH"; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

failures=0
fail() { echo "  FAIL: $*"; failures=$((failures + 1)); }

BIN="$WORK/bin"
mkdir -p "$BIN"

# --- The fixture remote ---------------------------------------------------
#
# A `git` that answers exactly the queries the two scripts make and refuses
# anything else loudly, so a query added later cannot be answered by an
# accidental exit 0.
cat > "$BIN/git" <<'EOF'
#!/usr/bin/env bash
set -uo pipefail
[ "${1:-}" = "-C" ] && shift 2
printf 'GIT %s\n' "$*" >> "$FAKE_DIR/log"
case "${1:-}" in
    ls-remote)
        if [ -f "$FAKE_DIR/ls-remote-fails" ]; then
            echo "fatal: could not read from remote repository" >&2
            exit 128
        fi
        cat "$FAKE_DIR/refs" 2>/dev/null
        exit 0
        ;;
    fetch)
        if [ -f "$FAKE_DIR/fetch-fails" ]; then
            echo "fatal: couldn't find remote ref refs/nightly/green/*" >&2
            exit 1
        fi
        exit 0
        ;;
    merge-base)
        shift
        [ "${1:-}" = "--is-ancestor" ] && shift
        a="${1:-}"; b="${2:-}"
        [ "$a" = "$b" ] && exit 0
        grep -qxF "$a $b" "$FAKE_DIR/ancestry" 2>/dev/null && exit 0
        exit 1
        ;;
    remote)
        shift
        if [ "${1:-}" = "get-url" ]; then printf '%s\n' "${FAKE_REMOTE_URL:-}"; exit 0; fi
        printf '%s\n' "${FAKE_REMOTES:-}"
        exit 0
        ;;
    push)
        shift
        printf 'PUSH %s\n' "$*" >> "$FAKE_DIR/log"
        [ -f "$FAKE_DIR/push-fails" ] && exit 1
        exit 0
        ;;
esac
echo "fixture git: unhandled query '$*'" >&2
exit 99
EOF
chmod +x "$BIN/git"

STAMP_NOW="$(date -u +%Y%m%dT%H%M%SZ)"
STAMP_2D="$(date -u -d '2 days ago' +%Y%m%dT%H%M%SZ)"
STAMP_5D="$(date -u -d '5 days ago' +%Y%m%dT%H%M%SZ)"

# The commit under publish, and a later one the nightly might have tested.
HEAD_SHA="1111111111111111111111111111111111111111"
LATER_SHA="2222222222222222222222222222222222222222"
OTHER_SHA="3333333333333333333333333333333333333333"

setup() {  # <case>
    FAKE_DIR="$WORK/$1"; export FAKE_DIR
    TREE="$WORK/$1/tree"
    mkdir -p "$FAKE_DIR" "$TREE/scripts"
    cp "$CHECK_SH" "$TREE/scripts/check-nightly-green.sh"
    cp "$REF_SH"   "$TREE/scripts/nightly-green-ref.sh"
    : > "$FAKE_DIR/log"
    : > "$FAKE_DIR/refs"
    : > "$FAKE_DIR/ancestry"
    unset LEVICULUM_PUBLISH_WITHOUT_NIGHTLY
}

add_ref() {  # <stamp> <sha>
    printf '%s\trefs/nightly/green/%s\n' "$2" "$1" >> "$FAKE_DIR/refs"
}

run_check() {  # [extra env assignments are the caller's]
    ( PATH="$BIN:$PATH" bash "$TREE/scripts/check-nightly-green.sh" \
        --commit "$HEAD_SHA" --remote "fixture://remote" ) > "$FAKE_DIR/out" 2>&1
    echo $? > "$FAKE_DIR/rc"
}

rc() { cat "$FAKE_DIR/rc"; }
dumpout() { echo "  --- output ---"; sed 's/^/    /' "$FAKE_DIR/out"; }

# =========================================================================
# check-nightly-green.sh
# =========================================================================

# --- Case: a fresh ref names the commit itself ----------------------------
echo "[case] check/covered-exact"
setup check-covered-exact
add_ref "$STAMP_NOW" "$HEAD_SHA"
run_check
[ "$(rc)" = "0" ] || { fail "exit $(rc), expected 0"; dumpout; }
grep -q 'ACCEPTED' "$FAKE_DIR/out" || { fail "accepted without saying so"; dumpout; }

# --- Case: the ref names a DESCENDANT of the commit -----------------------
#
# The publish commit is an ancestor of the tested one, which is coverage: the
# nightly saw this code and more of it.
echo "[case] check/covered-as-ancestor"
before=$failures
setup check-covered-ancestor
add_ref "$STAMP_NOW" "$LATER_SHA"
echo "$HEAD_SHA $LATER_SHA" > "$FAKE_DIR/ancestry"
run_check
[ "$(rc)" = "0" ] || fail "exit $(rc) although the commit is an ancestor of the tested one"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: no signal at all -----------------------------------------------
echo "[case] check/no-signal-empty"
before=$failures
setup check-no-signal
run_check
[ "$(rc)" != "0" ] || fail "exit 0 with no green ref on the remote"
grep -q 'REFUSED (NO-SIGNAL)' "$FAKE_DIR/out" || fail "the refusal does not name NO-SIGNAL"
grep -q 'LEVICULUM_PUBLISH_WITHOUT_NIGHTLY' "$FAKE_DIR/out" || fail "the refusal does not name the override"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: the remote cannot be read --------------------------------------
#
# Unreadable is not "no nightly has been green"; both refuse, and the log has
# to carry git's own words or the next reader debugs the wrong thing.
echo "[case] check/no-signal-unreadable"
before=$failures
setup check-unreadable
touch "$FAKE_DIR/ls-remote-fails"
run_check
[ "$(rc)" != "0" ] || fail "exit 0 although ls-remote failed"
grep -q 'REFUSED (NO-SIGNAL)' "$FAKE_DIR/out" || fail "the refusal does not name NO-SIGNAL"
grep -q 'could not read from remote' "$FAKE_DIR/out" || fail "git's own error was swallowed"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: refs exist, none covers ----------------------------------------
echo "[case] check/not-covered"
before=$failures
setup check-not-covered
add_ref "$STAMP_NOW" "$OTHER_SHA"
run_check
[ "$(rc)" != "0" ] || fail "exit 0 although no ref covers the commit"
grep -q 'REFUSED (NOT-COVERED)' "$FAKE_DIR/out" || fail "the refusal does not name NOT-COVERED"
grep -q "${OTHER_SHA:0:12}" "$FAKE_DIR/out" || fail "the refusal does not say what the newest ref points at"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: the covering ref is too old ------------------------------------
echo "[case] check/too-old"
before=$failures
setup check-too-old
add_ref "$STAMP_5D" "$HEAD_SHA"
run_check
[ "$(rc)" != "0" ] || fail "exit 0 on a 5-day-old signal with a 72 h bound"
grep -q 'REFUSED (TOO-OLD)' "$FAKE_DIR/out" || fail "the refusal does not name TOO-OLD"
grep -q '72 h bound' "$FAKE_DIR/out" || fail "the refusal does not name the bound it applied"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: 2 days old is inside the bound ---------------------------------
#
# The bound has to accept the ordinary case, or it is a gate nobody keeps: the
# forge publishes on its own cron and the freshest ref it can read is the
# previous night's.
echo "[case] check/two-days-is-fresh-enough"
before=$failures
setup check-two-days
add_ref "$STAMP_2D" "$HEAD_SHA"
run_check
[ "$(rc)" = "0" ] || fail "exit $(rc) on a 2-day-old signal, inside the 72 h bound"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: the newest ref does not cover, an older one does ---------------
#
# The load-bearing selection rule. A later night that tested a commit on a
# different line says nothing about this one; the older ref that does cover it
# is still honest evidence, and picking the newest ref outright would refuse a
# build the nightly has actually tested.
echo "[case] check/newest-covering-not-newest"
before=$failures
setup check-newest-covering
add_ref "$STAMP_2D" "$HEAD_SHA"
add_ref "$STAMP_NOW" "$OTHER_SHA"
run_check
[ "$(rc)" = "0" ] || fail "exit $(rc) although an older ref covers the commit"
grep -q "covered by refs/nightly/green/$STAMP_2D" "$FAKE_DIR/out" \
    || fail "the older covering ref was not the one chosen"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: the override ---------------------------------------------------
echo "[case] check/override"
before=$failures
setup check-override
run_check_override() {
    ( PATH="$BIN:$PATH" LEVICULUM_PUBLISH_WITHOUT_NIGHTLY="$1" \
        bash "$TREE/scripts/check-nightly-green.sh" \
        --commit "$HEAD_SHA" --remote "fixture://remote" ) > "$FAKE_DIR/out" 2>&1
    echo $? > "$FAKE_DIR/rc"
}
run_check_override "forge outage, release cut by hand"
[ "$(rc)" = "0" ] || fail "exit $(rc) with a reasoned override on an empty remote"
grep -q 'OVERRIDDEN' "$FAKE_DIR/out" || fail "the override did not announce itself"
grep -q 'forge outage, release cut by hand' "$FAKE_DIR/out" \
    || fail "the reason was not printed into the log"
grep -q '^GIT ls-remote' "$FAKE_DIR/log" && fail "the override still went to the remote"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: the override is not a flag -------------------------------------
echo "[case] check/override-needs-a-reason"
before=$failures
setup check-override-flag
run_check_override "1"
[ "$(rc)" != "0" ] || fail "exit 0 on LEVICULUM_PUBLISH_WITHOUT_NIGHTLY=1"
grep -q 'not a reason' "$FAKE_DIR/out" || fail "the refusal does not say why '1' is not enough"
[ "$failures" -eq "$before" ] || dumpout

# =========================================================================
# nightly-green-ref.sh
# =========================================================================

# A manifest in the shape scripts/run-with-manifest.py writes (schema 2).
write_manifest() {  # <path> <commit> <dirty:true|false> <exit_code> <rnsd:green|red|absent|empty>
    local path="$1" commit="$2" dirty="$3" code="$4" rnsd="$5" unit=""
    case "$rnsd" in
        green)  unit='{"selector":"-p leviculum-std --test rnsd_interop","descriptor":"tests/rnsd_interop/main.rs","ok":["announce_interop_tests::a","link_tests::b"],"failed":[],"ignored":[]}' ;;
        red)    unit='{"selector":"-p leviculum-std --test rnsd_interop","descriptor":"tests/rnsd_interop/main.rs","ok":["announce_interop_tests::a"],"failed":["link_tests::b"],"ignored":[]}' ;;
        empty)  unit='{"selector":"-p leviculum-std --test rnsd_interop","descriptor":"tests/rnsd_interop/main.rs","ok":[],"failed":[],"ignored":[]}' ;;
        absent) unit='{"selector":"-p leviculum-core --lib","descriptor":"unittests src/lib.rs","ok":["x::y"],"failed":[],"ignored":[]}' ;;
    esac
    cat > "$path" <<JSON
{"schema":2,"gate":"workspace-all-targets","command":["cargo","test"],
 "repo":"/home/lew/ci/nightly-tree","commit":"$commit","dirty":$dirty,
 "host":"fixture","exit_code":$code,"units":[$unit]}
JSON
}

run_ref() {  # <verdict> <manifest>
    ( PATH="$BIN:$PATH" bash "$TREE/scripts/nightly-green-ref.sh" \
        --commit "$HEAD_SHA" --verdict "$1" --manifest "$2" \
        --remote "fixture://remote" --dry-run ) > "$FAKE_DIR/out" 2>&1
    echo $? > "$FAKE_DIR/rc"
}

# --- Case: a green night with a green rnsd_interop ------------------------
echo "[case] ref/green-pushes"
before=$failures
setup ref-green
write_manifest "$FAKE_DIR/m.json" "$HEAD_SHA" false 0 green
run_ref green "$FAKE_DIR/m.json"
[ "$(rc)" = "0" ] || fail "exit $(rc) on a green night with green interop"
grep -qE 'would push: \+'"$HEAD_SHA"':refs/nightly/green/[0-9]{8}T[0-9]{6}Z' "$FAKE_DIR/out" \
    || fail "the refspec is not '+<commit>:refs/nightly/green/<stamp>'"
grep -q '2 test(s) passed' "$FAKE_DIR/out" || fail "the evidence it checked is not in the log"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: a red night pushes nothing, and is not an error ----------------
echo "[case] ref/red-night"
before=$failures
setup ref-red
write_manifest "$FAKE_DIR/m.json" "$HEAD_SHA" false 0 green
run_ref red "$FAKE_DIR/m.json"
[ "$(rc)" = "0" ] || fail "exit $(rc) on a red night — declining to sign is not a failure"
grep -q 'no ref pushed' "$FAKE_DIR/out" || fail "a red night went by silently"
grep -q 'would push' "$FAKE_DIR/out" && fail "a red night pushed a ref"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: green night, RED rnsd_interop ----------------------------------
#
# The case the whole mechanism is for. The caller says the night was green;
# its own manifest says otherwise, and the manifest wins.
echo "[case] ref/red-interop-is-refused"
before=$failures
setup ref-red-interop
write_manifest "$FAKE_DIR/m.json" "$HEAD_SHA" false 0 red
run_ref green "$FAKE_DIR/m.json"
[ "$(rc)" != "0" ] || fail "exit 0 with a failing rnsd_interop in the manifest"
grep -q 'would push' "$FAKE_DIR/out" && fail "pushed a ref although rnsd_interop was red"
grep -q '1 failing test' "$FAKE_DIR/out" || fail "the refusal does not say what it found"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: green night, rnsd_interop NEVER RAN ----------------------------
#
# Indistinguishable from green to anything that only reads an exit status,
# which is why the manifest is read instead (Guarantee B).
echo "[case] ref/absent-interop-is-refused"
before=$failures
setup ref-absent-interop
write_manifest "$FAKE_DIR/m.json" "$HEAD_SHA" false 0 absent
run_ref green "$FAKE_DIR/m.json"
[ "$(rc)" != "0" ] || fail "exit 0 with no rnsd_interop unit in the manifest"
grep -q 'did not run' "$FAKE_DIR/out" || fail "the refusal does not say the suite never ran"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: the unit is there but executed nothing -------------------------
echo "[case] ref/zero-tests-is-refused"
before=$failures
setup ref-zero-tests
write_manifest "$FAKE_DIR/m.json" "$HEAD_SHA" false 0 empty
run_ref green "$FAKE_DIR/m.json"
[ "$(rc)" != "0" ] || fail "exit 0 with an rnsd_interop unit that executed zero tests"
grep -q 'zero tests' "$FAKE_DIR/out" || fail "the refusal does not name the empty run"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: the manifest belongs to another commit -------------------------
echo "[case] ref/wrong-commit"
before=$failures
setup ref-wrong-commit
write_manifest "$FAKE_DIR/m.json" "$OTHER_SHA" false 0 green
run_ref green "$FAKE_DIR/m.json"
[ "$(rc)" != "0" ] || fail "exit 0 with a manifest recorded at a different commit"
grep -q 'records commit' "$FAKE_DIR/out" || fail "the refusal does not name the mismatch"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: the manifest was recorded on a dirty tree ----------------------
echo "[case] ref/dirty-tree"
before=$failures
setup ref-dirty
write_manifest "$FAKE_DIR/m.json" "$HEAD_SHA" true 0 green
run_ref green "$FAKE_DIR/m.json"
[ "$(rc)" != "0" ] || fail "exit 0 with a manifest from a dirty tree"
grep -q 'DIRTY' "$FAKE_DIR/out" || fail "the refusal does not name the dirty tree"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: the manifest is missing ----------------------------------------
echo "[case] ref/no-manifest"
before=$failures
setup ref-no-manifest
run_ref green "$FAKE_DIR/does-not-exist.json"
[ "$(rc)" != "0" ] || fail "exit 0 with no manifest to read"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: the manifest is found without being named ----------------------
#
# The caller must not have to know where run-with-manifest.py put it: two
# checkouts on one host write into different directories, and a caller
# reaching for the wrong one would read another tree's green interop result.
echo "[case] ref/finds-its-own-manifest"
before=$failures
setup ref-default-manifest
mkdir -p "$FAKE_DIR/manifests"
write_manifest "$FAKE_DIR/manifests/workspace-all-targets.json" "$HEAD_SHA" false 0 green
( PATH="$BIN:$PATH" LEVICULUM_MANIFEST_DIR="$FAKE_DIR/manifests" \
    bash "$TREE/scripts/nightly-green-ref.sh" \
    --commit "$HEAD_SHA" --verdict green \
    --remote "fixture://remote" --dry-run ) > "$FAKE_DIR/out" 2>&1
echo $? > "$FAKE_DIR/rc"
[ "$(rc)" = "0" ] || fail "exit $(rc) when the manifest had to be located"
grep -q 'would push' "$FAKE_DIR/out" || fail "no ref although the located manifest is green"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: old refs are pruned in the same push ---------------------------
echo "[case] ref/prunes-old-refs"
before=$failures
setup ref-prune
write_manifest "$FAKE_DIR/m.json" "$HEAD_SHA" false 0 green
OLD_STAMP="$(date -u -d '40 days ago' +%Y%m%dT%H%M%SZ)"
add_ref "$OLD_STAMP" "$OTHER_SHA"
add_ref "$STAMP_2D"  "$OTHER_SHA"
run_ref green "$FAKE_DIR/m.json"
[ "$(rc)" = "0" ] || fail "exit $(rc) on the prune case"
grep -q ":refs/nightly/green/$OLD_STAMP" "$FAKE_DIR/out" || fail "the 40-day-old ref was not pruned"
grep -q ":refs/nightly/green/$STAMP_2D" "$FAKE_DIR/out" && fail "a 2-day-old ref was pruned"
[ "$failures" -eq "$before" ] || dumpout

echo
if [ "$failures" -ne 0 ]; then
    echo "test-nightly-green: FAILED ($failures assertion(s))"
    exit 1
fi
echo "test-nightly-green: all cases passed"
