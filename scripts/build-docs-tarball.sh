#!/usr/bin/env bash
# Build the mdBook under docs/ and pack it as the nightly's documentation
# artefact (Codeberg #260):
#
#   target/docs/leviculum-docs-nightly.tar.gz
#
# collect-nightly-debs.sh stages it into dist/ with its .sha256, and from
# there it rides both publish targets like every other asset. On the site,
# packaging/site/lev-unpack-docs unpacks the copy under
# releases/nightly/latest/ into <webroot>/docs, which is what
# https://leviculum.network/docs/ serves. Until this existed the book was
# built by nobody and published nowhere, and the landing page's
# Documentation link pointed at the forge's Markdown rendering.
#
# The archive layout is the contract lev-unpack-docs checks, so it is built
# to that contract rather than to tar's defaults:
#
#   * every member lies under ONE top directory, `docs/`,
#   * every member is a regular file. No directory entries either: the list
#     handed to tar is files only and --no-recursion keeps it that way, and
#     tar creates the directories on extraction. The receiving side refuses
#     anything that is not `-` in a listing, so a directory member here would
#     make every publish a refusal.
#   * owner and order are fixed, so the same book is the same member list.
#
# The mdbook version is PINNED, in exactly one place: the `cargo install`
# line in scripts/install-ci.sh. The nightly installs what
# `--print-mdbook-pin` prints, and a build with any other mdbook refuses
# here, because a different renderer is a different book under the same
# URL. Bumping the pin is a deliberate act in install-ci.sh.
#
# Usage:
#   scripts/build-docs-tarball.sh                     build and pack
#   scripts/build-docs-tarball.sh --print-mdbook-pin  print the pinned version
#
# Requires mdbook at the pinned version, plus bash, coreutils, sed, sort and
# GNU tar with gzip. Runs in the nightly's build-amd64 step.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

NAME="leviculum-docs-nightly.tar.gz"
OUT_DIR="target/docs"

mdbook_pin() {
    sed -n 's/^cargo install --locked mdbook --version \([0-9][0-9.]*\)$/\1/p' \
        scripts/install-ci.sh | head -n1
}

PIN="$(mdbook_pin)"
if [ -z "$PIN" ]; then
    echo "error: no 'cargo install --locked mdbook --version <x.y.z>' line in scripts/install-ci.sh" >&2
    exit 1
fi

case "${1:-}" in
--print-mdbook-pin)
    echo "$PIN"
    exit 0
    ;;
'') ;;
*)
    echo "usage: $0 [--print-mdbook-pin]" >&2
    exit 2
    ;;
esac

if ! command -v mdbook >/dev/null 2>&1; then
    echo "error: mdbook not found; install the pinned one:" >&2
    echo "       cargo install --locked mdbook --version ${PIN}" >&2
    exit 1
fi
have="$(mdbook --version)"
if [ "$have" != "mdbook v${PIN}" ]; then
    echo "error: '${have}' is not the pinned mdbook v${PIN} (scripts/install-ci.sh)" >&2
    exit 1
fi

STAGE="$(mktemp -d)"
trap 'rm -rf -- "$STAGE"' EXIT

# Built straight into the stage under the name the archive's top directory
# carries, so no rename and no leftover docs/book/ from an earlier run can
# reach the tarball.
mdbook build docs -d "$STAGE/docs"

# Regular files only, and nothing else is allowed to be there: a symlink in
# the rendered tree would be refused on the far end, so it is refused here
# first, where the build log names it.
shopt -s globstar dotglob nullglob
files=()
for path in "$STAGE"/docs/**; do
    rel="${path#"$STAGE"/}"
    if [ -L "$path" ]; then
        echo "error: the rendered book holds a symlink: ${rel}" >&2
        exit 1
    fi
    if [ -f "$path" ]; then
        files+=("$rel")
    elif [ ! -d "$path" ]; then
        echo "error: the rendered book holds a non-regular entry: ${rel}" >&2
        exit 1
    fi
done
shopt -u globstar dotglob nullglob
if [ "${#files[@]}" -eq 0 ] || [ ! -f "$STAGE/docs/index.html" ]; then
    echo "error: mdbook produced no docs/index.html" >&2
    exit 1
fi

mkdir -p "$OUT_DIR"
rm -f -- "$OUT_DIR/$NAME"
printf '%s\n' "${files[@]}" | LC_ALL=C sort |
    tar -C "$STAGE" --no-recursion --owner=0 --group=0 --numeric-owner \
        -czf "$OUT_DIR/$NAME" -T -

echo "[docs] ${OUT_DIR}/${NAME}: ${#files[@]} files, $(wc -c <"$OUT_DIR/$NAME") bytes, mdbook v${PIN}"
