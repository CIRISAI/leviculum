#!/usr/bin/env bash
# Fixture test for scripts/lnflash-bundle.sh: which build a bundle says it is.
#
# PUB-0022: every nightly bundle was named after the bare Cargo version, so
# two nightlies of one development window were the same "lnflash-0.10.0-dev"
# on disk, the same UF2 names and the same manifest version, and the person
# holding one could not say which firmware they had flashed. The bundle now
# carries the leviculum .deb's own stamp, <version>~nightly.<UTCdate>.<sha7>,
# in three places: the tarball name (and the directory inside it), every UF2
# name together with the manifest that points at them, and the README. This
# runs the real script from a known commit and asserts all three.
#
# The script builds two firmware images and a musl binary; none of that is
# what is under test, so a PATH of stubs stands in for the toolchain: cargo
# answers `metadata` and writes placeholder artefacts for `build`, rustc
# names a compiler and writes a copying bin2uf2, llvm-objcopy copies, and the
# lnflash it "builds" accepts --check-bundle. ~1 s, no network, no build.
#
# #438 added the NRF_FEATURES knob (`just lnflash-bundle <features>`) for
# a rig A/B arm. Without it a bundle must stay what it was, so every default
# case also asserts the manifest carries no `features` key and every firmware
# build got its board's bsp-* set and nothing more; the features case asserts
# the `+<feature>` suffix, the manifest list and the extra build features.
#
# LNFLASH_BUNDLE_SH overrides the script under test, which is how the
# pre-fix version is checked to be red:
#   LNFLASH_BUNDLE_SH=<old copy> bash scripts/test-lnflash-bundle.sh
#
# Usage: bash scripts/test-lnflash-bundle.sh

set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUNDLE_SH="${LNFLASH_BUNDLE_SH:-$REPO/scripts/lnflash-bundle.sh}"
[ -f "$BUNDLE_SH" ] || { echo "no script under test at $BUNDLE_SH"; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

failures=0
badn=0
fail() { echo "  FAIL: $*"; failures=$((failures + 1)); }

# The board records of the script under test, so a board added there is a
# board this test stages a payload for and asserts an image of.
mapfile -t RECORDS < <(sed -n 's/^    "\([a-z0-9]*|[^"]*\)"$/\1/p' "$BUNDLE_SH")
[ "${#RECORDS[@]}" -gt 0 ] || { echo "no BOARDS records found in $BUNDLE_SH"; exit 1; }

# --- The toolchain stubs ----------------------------------------------------
STUBS="$WORK/stubs"
mkdir -p "$STUBS"
cat >"$STUBS/cargo" <<'EOF'
#!/usr/bin/env bash
# metadata: where the fixture's target directory and workspace root are,
# and no packages, so nrf-rustflags.sh's per-dependency remaps (order 1620)
# come out empty.
# build: a placeholder where cargo would have written the artefact, and a
# line saying which firmware rustflags it was handed.
case "$1" in
metadata) printf '{"target_directory":"%s","workspace_root":"%s","packages":[]}\n' "$FIXTURE_TARGET" "$PWD" ;;
build)
    echo "$*" >>"$FIXTURE_TARGET/cargo-builds.log"
    [ "${3:-}" = lnflash ] ||
        echo "$4 ${CARGO_TARGET_THUMBV7EM_NONE_EABIHF_RUSTFLAGS:-}" >>"$FIXTURE_TARGET/cargo-rustflags.log"
    if [ "${3:-}" = lnflash ]; then
        out="$FIXTURE_TARGET/x86_64-unknown-linux-musl/release/lnflash"
        mkdir -p "$(dirname "$out")"
        printf '#!/usr/bin/env bash\nexit 0\n' >"$out"
        chmod +x "$out"
    fi
    ;;
*) echo "cargo stub: unexpected $*" >&2; exit 1 ;;
esac
EOF
cat >"$STUBS/rustc" <<'EOF'
#!/usr/bin/env bash
case "$1" in
--version) echo "rustc 9.9.9 (fixturec0de 2026-01-01)" ;;
--print) echo "$FIXTURE_TARGET/no-sysroot" ;;
-O)
    # rustc -O -o <out> <src>: a bin2uf2 that copies its input.
    printf '#!/usr/bin/env bash\ncp "${@: -2:1}" "${@: -1}"\n' >"$3"
    chmod +x "$3"
    ;;
*) echo "rustc stub: unexpected $*" >&2; exit 1 ;;
esac
EOF
cat >"$STUBS/llvm-objcopy" <<'EOF'
#!/usr/bin/env bash
cp "${@: -2:1}" "${@: -1}"
EOF
chmod +x "$STUBS"/*

# Where the firmware build sees the fixture tree: the fixed path of
# scripts/nrf-rustflags.sh when this host has unprivileged user namespaces,
# else the tree itself (BUILT_AT is set per case in setup).
if unshare --user --map-root-user --mount true 2>/dev/null; then
    CANON_BUILD=/mnt/leviculum
else
    CANON_BUILD=""
fi

# --- The fixture tree -------------------------------------------------------
VERSION="0.10.0-dev"

setup() { # <case>
    TREE="$WORK/$1/tree"
    BUILT_AT="${CANON_BUILD:-$TREE}"
    FIXTURE_TARGET="$WORK/$1/cargo-target"
    mkdir -p "$TREE/scripts" "$TREE/leviculum-nrf/tools" "$TREE/lnflash/payload" \
        "$FIXTURE_TARGET/thumbv7em-none-eabihf/release"
    cp "$BUNDLE_SH" "$TREE/scripts/lnflash-bundle.sh"
    cp "$REPO/scripts/cargo-target-dir.sh" "$TREE/scripts/"
    cp "$REPO/scripts/nrf-rustflags.sh" "$TREE/scripts/"
    printf '[workspace.package]\nversion = "%s"\n' "$VERSION" >"$TREE/Cargo.toml"
    printf '/target/\n/.deb-version-*\n' >"$TREE/.gitignore"
    echo "fixture bin2uf2 source" >"$TREE/leviculum-nrf/tools/bin2uf2.rs"
    echo "fixture README" >"$TREE/lnflash/payload/README-bundle.md"
    echo "fixture LICENSE" >"$TREE/LICENSE"
    printf 'PART 1 — host binaries\nPART 2 — LNode firmware image\n' \
        >"$TREE/THIRD-PARTY-NOTICES"
    local record name bin stem
    for record in "${RECORDS[@]}"; do
        IFS='|' read -r name bin _ stem <<<"$record"
        echo "fixture $name firmware" >"$FIXTURE_TARGET/thumbv7em-none-eabihf/release/$bin"
        [ -n "$stem" ] || continue
        mkdir -p "$TREE/lnflash/payload/$name"
        echo "fixture softdevice" >"$TREE/lnflash/payload/$name/${stem}_softdevice.hex"
        echo "fixture licence" >"$TREE/lnflash/payload/$name/${stem}_license-agreement.txt"
    done
    (cd "$TREE" &&
        git init -q . &&
        git add -A &&
        git -c user.name=fixture -c user.email=fixture@example.invalid \
            commit -q -m fixture) >/dev/null 2>&1 ||
        { echo "could not build the fixture git repo"; exit 1; }
    SHA7="$(git -C "$TREE" rev-parse HEAD | cut -c1-7)"
}

# The firmware builds run, against a cargo stub that only logs them, so the
# features each board is built with are observable.
run_bundle() { # [NRF_FEATURES]
    (cd "$TREE" && env -u CI_COMMIT_SHA -u CARGO_TARGET_DIR \
        PATH="$STUBS:$PATH" FIXTURE_TARGET="$FIXTURE_TARGET" \
        NRF_FEATURES="${1:-}" \
        bash scripts/lnflash-bundle.sh) >"$TREE/../out" 2>&1
    echo $? >"$TREE/../rc"
}
rc() { cat "$TREE/../rc"; }
dumpout() { echo "  --- script output ---"; sed 's/^/    /' "$TREE/../out"; }

# The three places, for a bundle that must be called <stamp>[<suffix>]: the
# suffix names the tarball, its directory and the UF2s, the manifest version
# and the README keep the bare stamp.
assert_stamped() { # <stamp> [<suffix>]
    local stamp="$1" suffix="${2:-}" tarball listing unpack name image
    local bundle="$stamp$suffix"
    # Cleared first: a case whose bundle never got this far must not have the
    # previous case's manifest asserted in its name (and with none set at all,
    # `set -u` ended the whole test before the script output was dumped).
    MANIFEST="$TREE/../no-manifest"
    tarball="$TREE/target/lnflash/lnflash-$bundle.tar.gz"
    [ "$(rc)" = 0 ] || { fail "the bundle script exited $(rc)"; return; }
    # 1. The tarball, and the one directory inside it that `cd lnflash-*`
    #    (publish-nightly.sh, README, flashing.md) enters.
    [ -f "$tarball" ] || {
        fail "no tarball named lnflash-$bundle.tar.gz; target/lnflash holds: $(find "$TREE/target/lnflash" -mindepth 1 -maxdepth 1 -printf '%f ')"
        return
    }
    listing="$(tar -tzf "$tarball")"
    [ "$(printf '%s\n' "$listing" | cut -d/ -f1 | sort -u)" = "lnflash-$bundle" ] ||
        fail "the tarball's top directory is not lnflash-$bundle: $(printf '%s\n' "$listing" | cut -d/ -f1 | sort -u | tr '\n' ' ')"
    unpack="$TREE/../unpack"
    rm -rf "$unpack" && mkdir -p "$unpack" && tar -xzf "$tarball" -C "$unpack"
    local dir="$unpack/lnflash-$bundle"
    MANIFEST="$dir/firmware/manifest.toml"
    # 2. Every UF2 is named after the stamp, the manifest's version is the
    #    stamp, and every image the manifest names is in the archive.
    grep -qxF "version = \"$stamp\"" "$dir/firmware/manifest.toml" 2>/dev/null ||
        fail "manifest [bundle] version is not \"$stamp\": $(grep '^version' "$dir/firmware/manifest.toml" 2>/dev/null)"
    for record in "${RECORDS[@]}"; do
        IFS='|' read -r name _ <<<"$record"
        image="$name/leviculum-$name-$bundle.uf2"
        [ -f "$dir/firmware/$image" ] || fail "no image firmware/$image"
        grep -qF "file    = \"$image\"" "$dir/firmware/manifest.toml" 2>/dev/null ||
            fail "the manifest does not point at $image"
    done
    # 3. The README a person opens names the build.
    grep -qxF "    $stamp" "$dir/README.md" 2>/dev/null ||
        fail "README.md does not state the stamp $stamp"
}

# A default bundle: no `features` key, and each board built with exactly its
# own feature set.
assert_default_features() {
    ! grep -q '^features' "$MANIFEST" 2>/dev/null ||
        fail "a default manifest carries a features key: $(grep '^features' "$MANIFEST")"
    assert_build_features ""
}

assert_build_features() { # <extra, comma-prefixed or empty>
    local record name bin features
    for record in "${RECORDS[@]}"; do
        IFS='|' read -r name bin features _ <<<"$record"
        grep -qxF -- "build --release --bin $bin --features $features$1" \
            "$FIXTURE_TARGET/cargo-builds.log" 2>/dev/null ||
            fail "the $name firmware was not built with --features $features$1: $(cat "$FIXTURE_TARGET/cargo-builds.log" 2>/dev/null)"
        # The shipped images carry no host path: the tree is remapped to /src
        # (scripts/nrf-rustflags.sh, order 1368), and where this host allows a
        # user namespace the tree the build saw is the fixed path, not the
        # fixture's own (order 1460), so a clone builds the same bytes.
        grep -F -- "$bin --remap-path-prefix=" "$FIXTURE_TARGET/cargo-rustflags.log" 2>/dev/null |
            grep -qF -- "=$BUILT_AT=/src" ||
            fail "the $name firmware was not built with the path remap of $BUILT_AT: $(cat "$FIXTURE_TARGET/cargo-rustflags.log" 2>/dev/null)"
    done
}

# --- Case: the stamp deb-stamp.sh wrote for this commit is used verbatim ----
#
# A date no clock produces today, so the assertion can tell "read the file"
# from "derived it again" apart.
echo "[case] stamp-file"
before=$failures
setup stamp-file
STAMP="0.10.0~dev~nightly.19991231.$SHA7"
echo "$STAMP" >"$TREE/.deb-version-leviculum-cli"
run_bundle
assert_stamped "$STAMP"
assert_default_features
[ "$failures" -eq "$before" ] || dumpout

# --- Case: no stamp file (a developer's `just lnflash-bundle`) --------------
echo "[case] no-stamp-file"
before=$failures
setup no-stamp-file
day_before="$(date -u +%Y%m%d)"
run_bundle
day_after="$(date -u +%Y%m%d)"
stamp="0.10.0~dev~nightly.$day_before.$SHA7"
[ -f "$TREE/target/lnflash/lnflash-$stamp.tar.gz" ] ||
    stamp="0.10.0~dev~nightly.$day_after.$SHA7"
assert_stamped "$stamp"
assert_default_features
[ "$failures" -eq "$before" ] || dumpout

# --- Case: a stamp file left by another commit is not believed -------------
echo "[case] stale-stamp-file"
before=$failures
setup stale-stamp-file
echo "0.10.0~dev~nightly.19991231.0000000" >"$TREE/.deb-version-leviculum-cli"
day_before="$(date -u +%Y%m%d)"
run_bundle
day_after="$(date -u +%Y%m%d)"
stamp="0.10.0~dev~nightly.$day_before.$SHA7"
[ -f "$TREE/target/lnflash/lnflash-$stamp.tar.gz" ] ||
    stamp="0.10.0~dev~nightly.$day_after.$SHA7"
assert_stamped "$stamp"
assert_default_features
grep -q "ignoring .deb-version-leviculum-cli" "$TREE/../out" ||
    fail "a stale stamp file was dropped without saying so"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: an A/B arm's bundle names its features (#438) -------------------
echo "[case] nrf-features"
before=$failures
setup nrf-features
STAMP="0.10.0~dev~nightly.19991231.$SHA7"
echo "$STAMP" >"$TREE/.deb-version-leviculum-cli"
run_bundle subslot-offset
assert_stamped "$STAMP" "+subslot-offset"
grep -qxF 'features = ["subslot-offset"]' "$MANIFEST" 2>/dev/null ||
    fail "the manifest does not list the features: $(grep '^features' "$MANIFEST" 2>/dev/null)"
assert_build_features ",subslot-offset"
grep -qF 'subslot-offset' "$(dirname "$MANIFEST")/../README.md" ||
    fail "README.md does not name the extra features"
[ "$failures" -eq "$before" ] || dumpout

# --- Case: a feature list that is not one is refused ------------------------
for bad in "bsp-t114" "a,,b" "x;rm" ",subslot-offset"; do
    echo "[case] refuses NRF_FEATURES='$bad'"
    before=$failures
    setup "bad-$((++badn))"
    run_bundle "$bad"
    [ "$(rc)" != 0 ] || fail "NRF_FEATURES='$bad' was accepted"
    [ -z "$(find "$TREE/target/lnflash" -name '*.tar.gz' 2>/dev/null)" ] ||
        fail "NRF_FEATURES='$bad' still wrote a tarball"
    [ "$failures" -eq "$before" ] || dumpout
done

echo
if [ "$failures" -eq 0 ]; then
    echo "test-lnflash-bundle: all cases passed"
else
    echo "test-lnflash-bundle: FAILED ($failures assertion(s))"
    exit 1
fi
