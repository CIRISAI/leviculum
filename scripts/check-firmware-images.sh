#!/bin/bash
# Every board this README advertises for the embedded firmware must have a
# prebuilt image somebody can download, and the release page must say so.
#
# Codeberg #295. The README named "Heltec T114, RAK4631" as the supported
# boards and then offered nothing but a source build — while the nightly had
# been shipping a self-contained lnflash bundle with an image for each of them
# since 24481f12. Three surfaces each held a different answer to "can I run
# this on my board", and nothing connected them:
#
#   * scripts/lnflash-bundle.sh    which boards an image is BUILT for
#   * README.md                    which boards a reader is TOLD about
#   * scripts/publish-nightly.sh   what the release page CLAIMS is in the asset
#
# The release body still said "the T114 firmware image" four weeks after the
# RAK image started shipping beside it, so a RAK owner reading the releases
# page concluded — correctly, from what was written — that there was no image
# for their board. "Supported" meant "the code supports it" on one surface and
# "you can run this on it" on another, and only one of those was true.
#
# So this compares the three as SETS and requires them equal. Set equality,
# not containment, because both directions are the bug: a board built but not
# mentioned is an image nobody finds, and a board mentioned but not built is
# the false advertisement the issue was filed about. Adding a third board to
# the bundle therefore fails here until the README row and the release body
# name it too — which is the whole point, since the board list is the one
# thing a new board changes on all three.
#
# It also checks every nightly download URL the README hands out against the
# filenames scripts/collect-nightly-debs.sh actually stages, because a URL in
# a README is a promise with no compiler behind it.
#
# A gate rather than a #[test] for the reason the whole check-* family is: it
# reads Markdown and two shell scripts that no test binary compiles, and it
# must run on the push path where whoever just edited them is still there.
#
# Usage:
#   bash scripts/check-firmware-images.sh              # check this tree
#   bash scripts/check-firmware-images.sh --selftest   # positive controls
#
# Exit 0 = the three surfaces agree. Exit 1 = they do not, or a control failed.
set -uo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"

# --- Reading the three surfaces -------------------------------------------

# The board keys scripts/lnflash-bundle.sh builds an image for: the first
# `|`-separated field of each record in its BOARDS array. That array is the
# bundle's own single source of truth — everything downstream of it in that
# script is a loop over it — so it is the right thing to read rather than the
# finished tarball, which needs the firmware toolchain to exist at all.
boards_built() {  # <lnflash-bundle.sh>
    awk '
        /^BOARDS=\(/ { inside = 1; next }
        inside && /^\)/ { inside = 0 }
        inside && match($0, /"[a-z0-9_-]+\|/) {
            print substr($0, RSTART + 1, RLENGTH - 2)
        }
    ' "$1" | sort -u
}

# The board keys the README advertises: the first cell of each row of the
# board table in the flashing section, as `key`. A table rather than prose
# because prose spells a board five ways ("T114", "Heltec T114", "Heltec Mesh
# Node T114") and a guard that has to recognise all five recognises none of
# them reliably. The backticked key is the same token the bundle, the
# catalogue and `lnflash --board` use, so there is exactly one spelling to
# agree on.
boards_advertised() {  # <README.md>
    awk '
        /^#### Flashing LoRa hardware/ { inside = 1; next }
        inside && /^#/ { inside = 0 }
        inside && match($0, /^\| `[a-z0-9_-]+`/) {
            cell = substr($0, RSTART, RLENGTH)
            gsub(/^\| `|`$/, "", cell)
            print cell
        }
    ' "$1" | sort -u
}

# The release body scripts/publish-nightly.sh PATCHes onto the rolling
# release: the heredoc it builds RELEASE_BODY from. Scoped to the heredoc and
# not the whole file, so a board named only in a source comment — which no
# reader of the releases page ever sees — cannot satisfy the check.
release_body() {  # <publish-nightly.sh>
    awk '
        /^RELEASE_BODY=\$\(cat <<EOF/ { inside = 1; next }
        inside && /^EOF$/ { inside = 0 }
        inside { print }
    ' "$1"
}

# Every nightly asset the collect script stages, by the stable filename the
# download URL uses. The names are built there from `$pkg`/`$arch` shell
# variables, so they cannot be read as literals; what is literal is the
# `<pkg>-nightly-<arch>.<ext>` shape plus the package and arch lists. Expand
# that shape here rather than run the script, which needs a built tree.
assets_staged() {  # <collect-nightly-debs.sh>
    local collect="$1" pkg arch
    # The packages it calls collect_deb/pack_bin_tarball for, read off those
    # call lines so a package added there is added here.
    local pkgs
    pkgs=$(awk '/^collect_deb [a-z]+ (amd64|arm64)$/ { print $2 }' "$collect" | sort -u)
    for pkg in $pkgs; do
        for arch in amd64 arm64; do
            printf '%s\n' "${pkg}-nightly-${arch}.deb" "${pkg}-nightly-${arch}.tar.gz"
        done
    done
    # The two it names literally.
    awk 'match($0, /[a-z0-9-]+-nightly(-[a-z0-9]+)?\.(tar\.gz|deb|uf2)/) {
             print substr($0, RSTART, RLENGTH)
         }' "$collect"
}

# --- The check ------------------------------------------------------------

# Compare this tree, or a fixture triple. Prints its failures and returns 1.
check_tree() {  # <bundle.sh> <README.md> <publish.sh> <collect.sh>
    local bundle="$1" readme="$2" publish="$3" collect="$4"
    local failures=0
    local built advertised
    built="$(boards_built "$bundle")"
    advertised="$(boards_advertised "$readme")"

    if [ -z "$built" ]; then
        echo "  FAIL: no board records found in $bundle — the BOARDS array moved"
        return 1
    fi
    if [ -z "$advertised" ]; then
        echo "  FAIL: no board rows found in the flashing section of $readme"
        echo "        (expected table rows starting with a backticked board key)"
        return 1
    fi

    # Both directions at once.
    local only_built only_advertised
    only_built="$(comm -23 <(echo "$built") <(echo "$advertised"))"
    only_advertised="$(comm -13 <(echo "$built") <(echo "$advertised"))"
    if [ -n "$only_built" ]; then
        echo "  FAIL: the bundle ships an image for these boards and the README"
        echo "        does not name them, so nobody who owns one finds it:"
        echo "$only_built" | awk '{ print "          " $0 }'
        failures=$((failures + 1))
    fi
    if [ -n "$only_advertised" ]; then
        echo "  FAIL: the README advertises these boards and no image is built"
        echo "        for them, which is #295 exactly — 'supported' promising a"
        echo "        binary that does not exist:"
        echo "$only_advertised" | awk '{ print "          " $0 }'
        failures=$((failures + 1))
    fi

    # The release page is the third reader of the same list.
    local body board
    body="$(release_body "$publish")"
    if [ -z "$body" ]; then
        echo "  FAIL: no RELEASE_BODY heredoc found in $publish"
        return 1
    fi
    for board in $built; do
        printf '%s\n' "$body" | grep -qiF -- "$board" || {
            echo "  FAIL: the release body never mentions the '$board' image the"
            echo "        bundle carries — the releases page is where a stranger"
            echo "        looks first, and it would tell them their board has none"
            failures=$((failures + 1))
        }
    done

    # Every download URL the README hands out must be an asset that exists.
    local staged url name
    staged="$(assets_staged "$collect" | sort -u)"
    while read -r url; do
        [ -n "$url" ] || continue
        name="${url##*/}"
        printf '%s\n' "$staged" | grep -qxF -- "$name" || {
            echo "  FAIL: the README links releases/download/nightly/$name and"
            echo "        the collect script stages no such asset — a 404 under a"
            echo "        URL we published"
            failures=$((failures + 1))
        }
    done < <(grep -o 'releases/download/nightly/[A-Za-z0-9._-]*' "$readme" | sort -u)

    [ "$failures" -eq 0 ]
}

# --- Positive controls ----------------------------------------------------
#
# A guard nobody has seen fail is a guard nobody has tested. Each control
# injects one shape of the bug this exists to catch into a copy of the real
# files and requires the real check to refuse it — and the clean copy has to
# pass, or the controls prove only that the fixture is broken.
selftest() {
    local work rc out failures=0
    work="$(mktemp -d)"
    # shellcheck disable=SC2064
    trap "rm -rf '$work'" RETURN

    local B="$work/bundle.sh" R="$work/README.md" P="$work/publish.sh" C="$work/collect.sh"
    fresh() {
        cp "$REPO_DIR/scripts/lnflash-bundle.sh" "$B"
        cp "$REPO_DIR/README.md" "$R"
        cp "$REPO_DIR/scripts/publish-nightly.sh" "$P"
        cp "$REPO_DIR/scripts/collect-nightly-debs.sh" "$C"
    }
    run() { out="$(check_tree "$B" "$R" "$P" "$C" 2>&1)"; rc=$?; }

    echo "[control] the tree as it stands must pass"
    fresh
    run
    [ "$rc" -eq 0 ] || {
        echo "  FAIL: the real files do not satisfy the check"
        printf '%s\n' "$out" | awk '{ print "    " $0 }'
        failures=$((failures + 1))
    }

    echo "[control] a board built but not in the README must fail"
    fresh
    sed -i 's#^BOARDS=($#BOARDS=(\n    "xiao|xiao|bsp-xiao|"#' "$B"
    run
    [ "$rc" -ne 0 ] || { echo "  FAIL: passed with an unadvertised board"; failures=$((failures + 1)); }
    printf '%s\n' "$out" | grep -q xiao || {
        echo "  FAIL: the failure does not name the board"
        failures=$((failures + 1))
    }

    echo "[control] a board in the README with no image must fail"
    fresh
    # shellcheck disable=SC2016  # the backticks are Markdown, not a substitution
    sed -i 's#^| `t114`#| `nosuchboard`#' "$R"
    run
    [ "$rc" -ne 0 ] || { echo "  FAIL: passed with an unbuilt board advertised"; failures=$((failures + 1)); }
    printf '%s\n' "$out" | grep -q nosuchboard || {
        echo "  FAIL: the failure does not name the board"
        failures=$((failures + 1))
    }

    echo "[control] a board missing from the release body must fail"
    fresh
    # Every spelling, hence the case-insensitive substitution: the body names
    # the board both as the `rak4631` key and as the product "RAK4631", and
    # the check above is deliberately case-insensitive because "RAK4631" is
    # how a human reading the releases page recognises their own board. A
    # control that removed only the lowercase key would leave the uppercase
    # one matching and prove nothing — it did, on the first run of this file.
    sed -i 's/rak4631/BOARDREMOVEDBYCONTROL/Ig' "$P"
    run
    [ "$rc" -ne 0 ] || { echo "  FAIL: passed with the release body silent on a board"; failures=$((failures + 1)); }

    echo "[control] a README link to an asset nobody publishes must fail"
    fresh
    sed -i 's#releases/download/nightly/leviculum-nightly-amd64.deb#releases/download/nightly/leviculum-nightly-ppc64.deb#' "$R"
    run
    [ "$rc" -ne 0 ] || { echo "  FAIL: passed with a link to an unpublished asset"; failures=$((failures + 1)); }
    printf '%s\n' "$out" | grep -q ppc64 || {
        echo "  FAIL: the failure does not name the missing asset"
        failures=$((failures + 1))
    }

    echo
    if [ "$failures" -ne 0 ]; then
        echo "check-firmware-images --selftest: FAILED ($failures control(s))"
        return 1
    fi
    echo "check-firmware-images --selftest: all controls passed"
}

case "${1:-}" in
--selftest)
    selftest
    ;;
"")
    if check_tree \
        "$REPO_DIR/scripts/lnflash-bundle.sh" \
        "$REPO_DIR/README.md" \
        "$REPO_DIR/scripts/publish-nightly.sh" \
        "$REPO_DIR/scripts/collect-nightly-debs.sh"
    then
        echo "check-firmware-images: every advertised board has a published image"
    else
        echo
        echo "check-firmware-images: FAILED"
        exit 1
    fi
    ;;
*)
    echo "usage: $0 [--selftest]" >&2
    exit 2
    ;;
esac
