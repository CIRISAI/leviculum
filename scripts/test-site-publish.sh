#!/usr/bin/env bash
# Fixture test for the site half of the nightly publish.
#
# The receiver (packaging/site/lev-receive-nightly) is the only thing between
# a public download URL and whatever a broken or hostile upload puts on the
# wire. It runs unattended, as the forced command of an ssh key, on a host
# nobody is watching, and it writes into a directory a web server publishes.
# Its refusals are therefore the whole point of it — and a refusal nobody has
# ever seen fire is not a refusal. Every one of them is made to fire here,
# against the real script, with the real tar, and with the previous state of
# the tree asserted afterwards.
#
# The cases, each on a temporary releases root:
#
#   good-upload        a well-formed build publishes and `latest` points at it
#   bad-checksum       a corrupted file takes the WHOLE upload down with it
#   traversal          a member named `../x` is refused before extraction
#   symlink            a symlink member is refused before extraction
#   missing-pair       a payload file without its .sha256, and the reverse
#   empty-upload       an empty stream, and an archive with no payload files
#   bad-build-id       missing, multi-line, path-shaped, and `latest`
#   already-published  the same build id twice
#   second-build       `latest` swaps, the first build stays downloadable
#   prune              KEEP+1 builds: the oldest goes, `latest`'s never does
#   hostile-env        `env -i`, a broken PATH and a hostile IFS: still works
#   sender-layout      the REAL sender's tar, published by the REAL receiver
#   sender-no-dist     a run with nothing built says so and does not fail
#   sender-unconfigured  no ssh settings at all: it says NOT CONFIGURED, exits
#                        0, and opens no connection
#   sender-part-config   some ssh settings and not others: it fails, names the
#                        missing ones, and opens no connection
#
# Every refusal case additionally asserts that the previous `latest` still
# resolves to the previous build's files, that no new build directory
# appeared, and that no staging directory was left behind: a refused upload
# must be indistinguishable from an upload that never happened.
#
# No ssh and no network. The sender is driven through its `--tar-only` hook,
# which writes exactly the tar it would have piped, so what the receiver is
# tested against is the sender's real archive layout rather than this file's
# idea of it.
#
# RECEIVER / SENDER override the scripts under test, which is how a pre-fix
# copy is checked to be red.
#
# Usage: bash scripts/test-site-publish.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RECEIVER="${RECEIVER:-$REPO_ROOT/packaging/site/lev-receive-nightly}"
SENDER="${SENDER:-$REPO_ROOT/scripts/publish-site.sh}"
[ -f "$RECEIVER" ] || { echo "no receiver under test at $RECEIVER"; exit 1; }
[ -f "$SENDER" ] || { echo "no sender under test at $SENDER"; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

failures=0
fail() { echo "  FAIL: $*"; failures=$((failures + 1)); }

# --- Fixtures -------------------------------------------------------------

# A dist/-shaped directory: two payload files, each with the .sha256 that
# scripts/collect-nightly-debs.sh writes beside it, plus the BUILD_ID member
# scripts/publish-site.sh adds to the tar.
mkbuild() { # <dir> <build-id>
    local d="$1" id="$2" n
    mkdir -p "$d"
    for n in leviculum-nightly-amd64.deb leviculum-nightly-arm64.tar.gz; do
        echo "payload of $n in build $id" >"$d/$n"
        (cd "$d" && sha256sum "$n" >"$n.sha256")
    done
    printf '%s\n' "$id" >"$d/BUILD_ID"
}

# Flat tar of everything in <dir>, which is the layout the receiver demands.
mktar() { # <tarfile-absolute> <dir>
    (cd "$2" && tar -cf "$1" -- *)
}

# One build, one tar, ready to feed in.
build_tar() { # <tarfile-absolute> <build-id>  [-> stages under $WORK/stage.<id>]
    local t="$1" id="$2"
    local d="$WORK/stage.$id"
    rm -rf "$d"
    mkbuild "$d" "$id"
    mktar "$t" "$d"
}

# --- Driving the receiver -------------------------------------------------

new_root() { # <case> -> sets ROOT
    ROOT="$WORK/$1/releases"
    mkdir -p "$ROOT"
}

receive() { # <tarfile> [KEEP]
    local keep="${2:-14}"
    KEEP="$keep" RELEASES_ROOT="$ROOT" bash "$RECEIVER" \
        <"$1" >"$WORK/out" 2>"$WORK/err"
    RC=$?
}

# What `latest` resolves to, or the empty string. Reads through the symlink,
# so a dangling link is not an answer.
latest_id() {
    [ -d "$ROOT/nightly/latest" ] || return 0
    readlink "$ROOT/nightly/latest"
}

latest_payload() { # content served through latest, the thing a user downloads
    cat "$ROOT/nightly/latest/leviculum-nightly-amd64.deb" 2>/dev/null
}

# The build directories, one per line. `latest` is a symlink to one of them
# and a `*/` glob would report it as a seventh build, so it is skipped by
# being a link rather than by its name.
builds_present() {
    local path dir
    for path in "$ROOT"/nightly/*/; do
        dir="${path%/}"
        [ -e "$dir" ] || continue
        [ -L "$dir" ] && continue
        printf '%s\n' "${dir##*/}"
    done | sort
}

# Counted by globbing rather than by `ls | wc -l`: the names in play here are
# deliberately hostile.
count_matching() { # <glob...>
    local n=0 path
    for path in "$@"; do
        [ -e "$path" ] && n=$((n + 1))
    done
    printf '%s' "$n"
}

dump() {
    echo "  --- rc=$RC"
    echo "  --- stdout ---"; sed 's/^/    /' "$WORK/out"
    echo "  --- stderr ---"; sed 's/^/    /' "$WORK/err"
    echo "  --- tree ---"; (cd "$ROOT" && ls -lR .) | sed 's/^/    /'
}

# Asserts the whole point of a refusal: it failed, it said why in one line,
# and the tree is exactly what it was.
assert_refused() { # <case-label> <expected-latest-id> <expected-builds>
    local label="$1" want_latest="$2" want_builds="$3"
    [ "$RC" -ne 0 ] || fail "$label: exit 0, expected a refusal"
    local lines
    lines="$(wc -l <"$WORK/err")"
    [ "$lines" -eq 1 ] ||
        fail "$label: expected one line on stderr, got ${lines}"
    grep -q 'published' "$WORK/out" &&
        fail "$label: reported a publish although the upload was refused"
    [ "$(latest_id)" = "$want_latest" ] ||
        fail "$label: latest is '$(latest_id)', expected '${want_latest}'"
    [ "$(builds_present | tr '\n' ' ')" = "$want_builds" ] ||
        fail "$label: builds are '$(builds_present | tr '\n' ' ')', expected '${want_builds}'"
    local staging
    staging="$(count_matching "$ROOT"/.incoming.*)"
    [ "$staging" -eq 0 ] || fail "$label: left ${staging} staging directory/ies behind"
}

ID1="nightly.20260901-aaaaaaa+11"
ID2="nightly.20260902-bbbbbbb+12"
ID3="nightly.20260903-ccccccc+13"
ID4="nightly.20260904-ddddddd+14"

# --- Case: a good upload --------------------------------------------------
echo "[case] good-upload"
before=$failures
new_root good-upload
build_tar "$WORK/good.tar" "$ID1"
receive "$WORK/good.tar"
[ "$RC" -eq 0 ] || fail "good-upload: exit $RC, expected 0"
[ "$(latest_id)" = "$ID1" ] || fail "good-upload: latest is '$(latest_id)', expected '$ID1'"
[ "$(builds_present | tr '\n' ' ')" = "$ID1 " ] ||
    fail "good-upload: builds are '$(builds_present | tr '\n' ' ')'"
[ "$(latest_payload)" = "payload of leviculum-nightly-amd64.deb in build $ID1" ] ||
    fail "good-upload: the file served through latest is not the one uploaded"
[ -f "$ROOT/nightly/$ID1/leviculum-nightly-arm64.tar.gz.sha256" ] ||
    fail "good-upload: the checksum files were not published beside their files"
# World-readable, nobody-writable: it is a web root.
perms="$(stat -c '%a' "$ROOT/nightly/$ID1/leviculum-nightly-amd64.deb")"
[ "$perms" = "644" ] || fail "good-upload: published file has mode ${perms}, expected 644"
[ "$(count_matching "$ROOT"/.incoming.*)" -eq 0 ] ||
    fail "good-upload: left a staging directory behind"
[ "$failures" -eq "$before" ] || dump

# --- Case: a bad checksum -------------------------------------------------
#
# One corrupted file refuses the WHOLE upload: a build that is half what it
# claims to be is not a build, and half of it under a download URL is worse
# than none of it.
echo "[case] bad-checksum"
before=$failures
new_root bad-checksum
build_tar "$WORK/first.tar" "$ID1"
receive "$WORK/first.tar"
[ "$RC" -eq 0 ] || fail "bad-checksum: the setup upload failed"
d="$WORK/stage.$ID2"
rm -rf "$d"; mkbuild "$d" "$ID2"
echo "tampered" >>"$d/leviculum-nightly-amd64.deb"
mktar "$WORK/bad.tar" "$d"
receive "$WORK/bad.tar"
assert_refused bad-checksum "$ID1" "$ID1 "
grep -q 'checksum mismatch' "$WORK/err" ||
    fail "bad-checksum: stderr does not name the mismatch: $(cat "$WORK/err")"
[ "$(latest_payload)" = "payload of leviculum-nightly-amd64.deb in build $ID1" ] ||
    fail "bad-checksum: the previous build is no longer downloadable through latest"
[ "$failures" -eq "$before" ] || dump

# --- Case: a traversal path -----------------------------------------------
#
# `../evil` as a member name. GNU tar would refuse it on extraction, which is
# not the point: the refusal must be OURS and must happen before anything is
# extracted at all, because the next tar may be a different tar.
echo "[case] traversal"
before=$failures
new_root traversal
build_tar "$WORK/first.tar" "$ID1"
receive "$WORK/first.tar"
[ "$RC" -eq 0 ] || fail "traversal: the setup upload failed"
d="$WORK/stage.$ID2"
rm -rf "$d"; mkbuild "$d" "$ID2"
echo "owned" >"$d/evil"
mktar "$WORK/trav.tar" "$d"
# Built by appending a member whose stored name is `../evil`; -P is what
# keeps tar from normalising it away while creating the archive.
(cd "$d" && tar -rf "$WORK/trav.tar" -P --transform='s|^|../|' -- evil) 2>/dev/null
tar -tf "$WORK/trav.tar" 2>/dev/null | grep -qx '\.\./evil' ||
    fail "traversal: the fixture archive does not hold a '../evil' member"
receive "$WORK/trav.tar"
assert_refused traversal "$ID1" "$ID1 "
grep -q "'\.\./evil'" "$WORK/err" ||
    fail "traversal: stderr does not name the member: $(cat "$WORK/err")"
if [ -e "$ROOT/evil" ] || [ -e "$WORK/traversal/evil" ]; then
    fail "traversal: a file escaped the staging directory"
fi
[ "$failures" -eq "$before" ] || dump

# --- Case: a symlink member -----------------------------------------------
#
# The classic way out of an extraction directory. Refused on the archive's
# own listing, before it can be created.
echo "[case] symlink"
before=$failures
new_root symlink
build_tar "$WORK/first.tar" "$ID1"
receive "$WORK/first.tar"
[ "$RC" -eq 0 ] || fail "symlink: the setup upload failed"
d="$WORK/stage.$ID2"
rm -rf "$d"; mkbuild "$d" "$ID2"
# With a VALID checksum beside it: sha256sum follows the link, so this
# archive passes the pairing rule and the digest check, and the member type
# is the only thing left standing between /etc/passwd and a download URL.
ln -s /etc/passwd "$d/leviculum-nightly-secrets.deb"
(cd "$d" && sha256sum leviculum-nightly-secrets.deb >leviculum-nightly-secrets.deb.sha256)
(cd "$d" && tar -cf "$WORK/link.tar" -- *)
tar -tvf "$WORK/link.tar" 2>/dev/null | grep -q '^l' ||
    fail "symlink: the fixture archive holds no symlink member"
receive "$WORK/link.tar"
assert_refused symlink "$ID1" "$ID1 "
grep -q 'not a regular file' "$WORK/err" ||
    fail "symlink: stderr does not say why: $(cat "$WORK/err")"
[ ! -e "$ROOT/nightly/$ID2" ] || fail "symlink: the upload was published anyway"
[ "$failures" -eq "$before" ] || dump

# --- Case: unpaired files -------------------------------------------------
#
# Both directions. A file with no checksum is a file nobody vouched for; a
# checksum with no file is an upload that lost one on the way.
echo "[case] missing-pair"
before=$failures
new_root missing-pair
build_tar "$WORK/first.tar" "$ID1"
receive "$WORK/first.tar"
[ "$RC" -eq 0 ] || fail "missing-pair: the setup upload failed"

d="$WORK/stage.$ID2"
rm -rf "$d"; mkbuild "$d" "$ID2"
rm "$d/leviculum-nightly-amd64.deb.sha256"
mktar "$WORK/nosum.tar" "$d"
receive "$WORK/nosum.tar"
assert_refused missing-pair-nosum "$ID1" "$ID1 "
grep -q 'no checksum file' "$WORK/err" ||
    fail "missing-pair: stderr does not say why: $(cat "$WORK/err")"

rm -rf "$d"; mkbuild "$d" "$ID2"
rm "$d/leviculum-nightly-amd64.deb"
mktar "$WORK/nofile.tar" "$d"
receive "$WORK/nofile.tar"
assert_refused missing-pair-nofile "$ID1" "$ID1 "
grep -q 'no file beside it' "$WORK/err" ||
    fail "missing-pair: stderr does not say why: $(cat "$WORK/err")"

# A checksum file that vouches for something else entirely.
rm -rf "$d"; mkbuild "$d" "$ID2"
sed 's/ leviculum-nightly-amd64.deb$/ somewhere-else.deb/' \
    "$d/leviculum-nightly-amd64.deb.sha256" >"$d/x" && mv "$d/x" "$d/leviculum-nightly-amd64.deb.sha256"
mktar "$WORK/wrongname.tar" "$d"
receive "$WORK/wrongname.tar"
assert_refused missing-pair-wrongname "$ID1" "$ID1 "
grep -q "names 'somewhere-else.deb'" "$WORK/err" ||
    fail "missing-pair: a checksum naming another file was not refused as such: $(cat "$WORK/err")"
[ "$failures" -eq "$before" ] || dump

# --- Case: nothing in it --------------------------------------------------
echo "[case] empty-upload"
before=$failures
new_root empty-upload
build_tar "$WORK/first.tar" "$ID1"
receive "$WORK/first.tar"
[ "$RC" -eq 0 ] || fail "empty-upload: the setup upload failed"

: >"$WORK/empty.tar"
receive "$WORK/empty.tar"
assert_refused empty-upload-stream "$ID1" "$ID1 "

# An archive holding only a BUILD_ID: syntactically fine, and it would
# publish an empty directory under a download URL.
d="$WORK/stage.empty"
rm -rf "$d"; mkdir -p "$d"; printf '%s\n' "$ID2" >"$d/BUILD_ID"
mktar "$WORK/onlyid.tar" "$d"
receive "$WORK/onlyid.tar"
assert_refused empty-upload-nopayload "$ID1" "$ID1 "
grep -q 'no payload files' "$WORK/err" ||
    fail "empty-upload: stderr does not say why: $(cat "$WORK/err")"
[ "$failures" -eq "$before" ] || dump

# --- Case: the build id ---------------------------------------------------
#
# It names the directory, so every way of making it name the wrong one is
# refused: absent, more than one line, path-shaped, and `latest` itself.
echo "[case] bad-build-id"
before=$failures
new_root bad-build-id
build_tar "$WORK/first.tar" "$ID1"
receive "$WORK/first.tar"
[ "$RC" -eq 0 ] || fail "bad-build-id: the setup upload failed"

d="$WORK/stage.$ID2"
rm -rf "$d"; mkbuild "$d" "$ID2"; rm "$d/BUILD_ID"
mktar "$WORK/noid.tar" "$d"
receive "$WORK/noid.tar"
assert_refused bad-build-id-absent "$ID1" "$ID1 "

for bad in "../../etc/cron.d/x" "two
lines" "-rf" ".hidden" "latest"; do
    rm -rf "$d"; mkbuild "$d" "$ID2"
    printf '%s\n' "$bad" >"$d/BUILD_ID"
    mktar "$WORK/badid.tar" "$d"
    receive "$WORK/badid.tar"
    assert_refused "bad-build-id[${bad%%$'\n'*}]" "$ID1" "$ID1 "
done
[ "$failures" -eq "$before" ] || dump

# --- Case: the same build twice -------------------------------------------
#
# Re-running a nightly that already published must not half-replace what is
# already under its URL.
echo "[case] already-published"
before=$failures
new_root already-published
build_tar "$WORK/first.tar" "$ID1"
receive "$WORK/first.tar"
[ "$RC" -eq 0 ] || fail "already-published: the setup upload failed"
receive "$WORK/first.tar"
assert_refused already-published "$ID1" "$ID1 "
grep -q 'already published' "$WORK/err" ||
    fail "already-published: stderr does not say why: $(cat "$WORK/err")"
[ "$failures" -eq "$before" ] || dump

# --- Case: a second build -------------------------------------------------
#
# The swap: `latest` moves, and the build it moved off stays exactly where it
# was, so a link to a specific build id keeps working.
echo "[case] second-build"
before=$failures
new_root second-build
build_tar "$WORK/b1.tar" "$ID1"
build_tar "$WORK/b2.tar" "$ID2"
receive "$WORK/b1.tar"
[ "$RC" -eq 0 ] || fail "second-build: the first upload failed"
receive "$WORK/b2.tar"
[ "$RC" -eq 0 ] || fail "second-build: exit $RC on the second upload"
[ "$(latest_id)" = "$ID2" ] || fail "second-build: latest is '$(latest_id)', expected '$ID2'"
[ "$(latest_payload)" = "payload of leviculum-nightly-amd64.deb in build $ID2" ] ||
    fail "second-build: latest still serves the first build"
[ "$(builds_present | tr '\n' ' ')" = "$ID1 $ID2 " ] ||
    fail "second-build: builds are '$(builds_present | tr '\n' ' ')', expected both"
[ -f "$ROOT/nightly/$ID1/leviculum-nightly-amd64.deb" ] ||
    fail "second-build: the first build's files were removed"
[ "$failures" -eq "$before" ] || dump

# --- Case: pruning --------------------------------------------------------
#
# KEEP+1 builds: the oldest goes. And the one `latest` points at never does,
# which is not the same statement — a build id that sorts below the newest
# KEEP (a re-run of an older commit) is still the one being served.
echo "[case] prune"
before=$failures
new_root prune
for id in "$ID1" "$ID2" "$ID3" "$ID4"; do
    build_tar "$WORK/p.tar" "$id"
    receive "$WORK/p.tar" 3
    [ "$RC" -eq 0 ] || fail "prune: upload of ${id} failed"
done
[ "$(builds_present | tr '\n' ' ')" = "$ID2 $ID3 $ID4 " ] ||
    fail "prune: builds are '$(builds_present | tr '\n' ' ')', expected the newest three"
[ "$(latest_id)" = "$ID4" ] || fail "prune: latest is '$(latest_id)', expected '$ID4'"
grep -q "pruned ${ID1}" "$WORK/out" ||
    fail "prune: the run did not say it pruned ${ID1}: $(cat "$WORK/out")"

OLD="nightly.20260101-eeeeeee+1"
build_tar "$WORK/old.tar" "$OLD"
receive "$WORK/old.tar" 2
[ "$RC" -eq 0 ] || fail "prune: upload of the out-of-order build failed"
[ "$(latest_id)" = "$OLD" ] || fail "prune: latest is '$(latest_id)', expected '$OLD'"
[ -f "$ROOT/nightly/$OLD/leviculum-nightly-amd64.deb" ] ||
    fail "prune: pruned the build latest points at"
[ "$(latest_payload)" = "payload of leviculum-nightly-amd64.deb in build $OLD" ] ||
    fail "prune: latest no longer resolves to a downloadable file"
grep -q "keeping ${OLD}: latest points at it" "$WORK/out" ||
    fail "prune: the run did not say it kept the build latest points at: $(cat "$WORK/out")"
[ "$failures" -eq "$before" ] || dump

# --- Case: no environment at all ------------------------------------------
#
# The receiver is the forced command of an ssh key: what it inherits is
# whatever sshd hands a non-interactive session, which is next to nothing and
# is not ours to choose. So it is run here under `env -i` — no PATH, no HOME,
# no locale — plus a deliberately hostile IFS and a PATH that does not exist,
# and it must still publish. Anything it needs, it must set for itself.
echo "[case] hostile-env"
before=$failures
new_root hostile-env
build_tar "$WORK/env.tar" "$ID1"
# "$BASH" rather than `bash`: with PATH=/nonexistent, `env` itself could not
# find the interpreter — sshd reaches it through the script's shebang, which
# is an absolute path too.
env -i IFS=: PATH=/nonexistent LC_ALL=tr_TR.UTF-8 \
    RELEASES_ROOT="$ROOT" KEEP=14 "$BASH" "$RECEIVER" \
    <"$WORK/env.tar" >"$WORK/out" 2>"$WORK/err"
RC=$?
[ "$RC" -eq 0 ] || fail "hostile-env: exit $RC, expected 0"
[ "$(latest_id)" = "$ID1" ] ||
    fail "hostile-env: latest is '$(latest_id)', expected '$ID1'"
[ "$(latest_payload)" = "payload of leviculum-nightly-amd64.deb in build $ID1" ] ||
    fail "hostile-env: the published file is not the one uploaded"
[ "$failures" -eq "$before" ] || dump

# --- Case: the sender's own tar -------------------------------------------
#
# The layout contract between the two scripts, asserted by running both: the
# REAL sender builds the archive from a dist/ tree and the REAL receiver
# publishes it. No ssh — `--tar-only` writes exactly the archive that would
# otherwise have gone down the pipe.
echo "[case] sender-layout"
before=$failures
new_root sender-layout
TREE="$WORK/sender-layout/tree"
mkdir -p "$TREE/scripts" "$TREE/dist"
cp "$SENDER" "$TREE/scripts/publish-site.sh"
for n in leviculum-nightly-amd64.deb lnomad-nightly-arm64.tar.gz \
    leviculum-nightly-source.tar.gz; do
    echo "built $n" >"$TREE/dist/$n"
    (cd "$TREE/dist" && sha256sum "$n" >"$n.sha256")
done
printf '%s\n' "$ID3" >"$TREE/.build-id"
if ! ( cd "$TREE" && bash scripts/publish-site.sh --tar-only "$WORK/sent.tar" ) \
    >"$WORK/send-out" 2>"$WORK/send-err"; then
    fail "sender-layout: the sender failed"
    sed 's/^/    /' "$WORK/send-err"
fi
grep -q '^BUILD_ID$' <(tar -tf "$WORK/sent.tar") ||
    fail "sender-layout: the tar carries no flat BUILD_ID member"
tar -tf "$WORK/sent.tar" | grep -q '/' &&
    fail "sender-layout: the tar holds a member with a path separator"
receive "$WORK/sent.tar"
[ "$RC" -eq 0 ] || { fail "sender-layout: the receiver refused the sender's tar"; dump; }
[ "$(latest_id)" = "$ID3" ] ||
    fail "sender-layout: latest is '$(latest_id)', expected '$ID3'"
[ "$(cat "$ROOT/nightly/latest/lnomad-nightly-arm64.tar.gz" 2>/dev/null)" = "built lnomad-nightly-arm64.tar.gz" ] ||
    fail "sender-layout: a dist/ file did not arrive intact"
[ "$(count_matching "$ROOT/nightly/$ID3"/*)" -eq 7 ] ||
    fail "sender-layout: published $(count_matching "$ROOT/nightly/$ID3"/*) entries, expected 6 dist files + BUILD_ID"
[ "$failures" -eq "$before" ] || dump

# The sender refuses an empty dist/ rather than uploading a build with
# nothing in it — the receiver would refuse it too, but the log that gets
# read is the runner's.
rm -f "$TREE"/dist/*
if ( cd "$TREE" && bash scripts/publish-site.sh --tar-only "$WORK/sent2.tar" ) \
    >"$WORK/send-out" 2>"$WORK/send-err"; then
    fail "sender-layout: exit 0 with an empty dist/"
fi
grep -q 'dist/ is empty' "$WORK/send-err" ||
    fail "sender-layout: empty dist/ was not named as the reason: $(cat "$WORK/send-err")"

# --- Case: nothing was built ----------------------------------------------
#
# The step is allowed to run after an earlier step failed (see the `status:`
# note in .woodpecker/nightly.yml), and then there is no dist/ at all. That
# is the failed step's red, not this one's: it says so and exits 0. An empty
# dist/ above is a different claim and does fail.
echo "[case] sender-no-dist"
before=$failures
TREE="$WORK/sender-no-dist/tree"
mkdir -p "$TREE/scripts"
cp "$SENDER" "$TREE/scripts/publish-site.sh"
if ! ( cd "$TREE" && bash scripts/publish-site.sh --tar-only "$WORK/never.tar" ) \
    >"$WORK/send-out" 2>"$WORK/send-err"; then
    fail "sender-no-dist: non-zero exit, expected 0"
fi
grep -q 'nothing was built' "$WORK/send-out" ||
    fail "sender-no-dist: did not say why it published nothing: $(cat "$WORK/send-out")"
[ ! -e "$WORK/never.tar" ] || fail "sender-no-dist: wrote a tar anyway"
[ "$failures" -eq "$before" ] || { sed 's/^/    /' "$WORK/send-out" "$WORK/send-err"; }

# --- Case: the target is not configured -----------------------------------
#
# The three ssh settings are deliberately NOT `from_secret:` entries in
# .woodpecker/nightly.yml: a secret the repository does not have is a compile
# error for the whole pipeline, and on cron #434 that error took the forge
# publish — the target every download link we hand out points at — down with
# a site publish that had never been configured. What replaces the hard
# requirement is this: unset is a legal state, the step says so in words
# somebody skimming a green log would see, it exits 0, and it makes no
# connection at all.
#
# The "makes no connection" half is asserted rather than assumed, with an
# `ssh` on PATH that records having been called. A script that prints the
# banner and then tries to upload anyway would pass a grep-only test.
echo "[case] sender-unconfigured"
before=$failures
TREE="$WORK/sender-unconfigured/tree"
SHIM="$WORK/sender-unconfigured/bin"
mkdir -p "$TREE/scripts" "$TREE/dist" "$SHIM"
cp "$SENDER" "$TREE/scripts/publish-site.sh"
cat >"$SHIM/ssh" <<EOF
#!/bin/sh
echo called >"$WORK/ssh-was-called"
exit 0
EOF
chmod +x "$SHIM/ssh"
rm -f "$WORK/ssh-was-called"
echo "built leviculum-nightly-amd64.deb" >"$TREE/dist/leviculum-nightly-amd64.deb"
(cd "$TREE/dist" && sha256sum leviculum-nightly-amd64.deb >leviculum-nightly-amd64.deb.sha256)
printf '%s\n' "$ID1" >"$TREE/.build-id"
(
    cd "$TREE" &&
        env -u SITE_SSH_TARGET -u SITE_SSH_KEY -u SITE_SSH_HOST_KEY \
            PATH="$SHIM:$PATH" bash scripts/publish-site.sh
) >"$WORK/unconf-out" 2>"$WORK/unconf-err"
RC=$?
[ "$RC" -eq 0 ] || fail "sender-unconfigured: exit $RC, expected 0"
grep -q 'NOT CONFIGURED' "$WORK/unconf-out" ||
    fail "sender-unconfigured: the log does not say it is unconfigured: $(cat "$WORK/unconf-out")"
grep -q 'SITE_SSH_TARGET' "$WORK/unconf-out" ||
    fail "sender-unconfigured: the log does not name what is missing"
[ ! -e "$WORK/ssh-was-called" ] ||
    fail "sender-unconfigured: it tried to upload anyway"
[ "$failures" -eq "$before" ] || sed 's/^/    /' "$WORK/unconf-out" "$WORK/unconf-err"

# --- Case: the target is half configured ----------------------------------
#
# A host with no key is not "nobody has wired this up yet", it is a mistake
# somebody made, and the two must not produce the same line. This one fails,
# names both halves, and still opens no connection — publishing with two of
# three settings is not something to attempt and report on afterwards.
echo "[case] sender-part-config"
before=$failures
rm -f "$WORK/ssh-was-called"
(
    cd "$TREE" &&
        env -u SITE_SSH_KEY -u SITE_SSH_HOST_KEY \
            SITE_SSH_TARGET="deploy@example.invalid" \
            PATH="$SHIM:$PATH" bash scripts/publish-site.sh
) >"$WORK/part-out" 2>"$WORK/part-err"
RC=$?
[ "$RC" -ne 0 ] || fail "sender-part-config: exit 0 with two settings missing"
grep -q 'PARTLY configured' "$WORK/part-err" ||
    fail "sender-part-config: the failure does not name the reason: $(cat "$WORK/part-err")"
grep -q 'SITE_SSH_KEY' "$WORK/part-err" ||
    fail "sender-part-config: the failure does not name the missing settings"
grep -q 'NOT CONFIGURED' "$WORK/part-out" &&
    fail "sender-part-config: it reported an unconfigured target instead of a broken one"
[ ! -e "$WORK/ssh-was-called" ] ||
    fail "sender-part-config: it tried to upload with an incomplete configuration"
[ "$failures" -eq "$before" ] || sed 's/^/    /' "$WORK/part-out" "$WORK/part-err"

echo
if [ "$failures" -ne 0 ]; then
    echo "test-site-publish: FAILED ($failures assertion(s))"
    exit 1
fi
echo "test-site-publish: all cases passed"
