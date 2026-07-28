#!/usr/bin/env bash
#
# Repair the two things cargo-deb cannot produce, in one unpack/repack
# pass over a finished .deb.
#
# 1. md5sums. cargo-deb 3.6.3 writes control, conffiles, config, preinst,
#    prerm and postrm, and has no md5sums support at all — a gap in the
#    tool, not a misconfiguration. Debian expects the file: `dpkg -V` and
#    `debsums` verify installed files against it, and without it every
#    packaged file reports as unverifiable.
#
#    Conffiles are excluded, matching dh_md5sums: their whole point is
#    that the admin edits them, so a mismatch there is expected rather
#    than a finding.
#
# 2. Empty control fields. A package with no dependencies should omit
#    Depends entirely, but cargo-deb emits `Depends:` with an empty value
#    whether the manifest sets `depends = ""` or leaves it out. An empty
#    field is malformed, so it is dropped here.
#
# `--root-owner-group` on the repack is load-bearing: `dpkg-deb -R`
# restores file modes but leaves the tree owned by whoever ran it, and
# without that flag the rebuilt package would install files owned by the
# build user instead of root.
#
# Usage:
#   scripts/deb-finalize.sh PACKAGE.deb [PACKAGE.deb ...]
#
# Rewrites each package in place. Idempotent.

set -euo pipefail

if [ $# -eq 0 ]; then
    echo "usage: $0 PACKAGE.deb [PACKAGE.deb ...]" >&2
    exit 2
fi

for deb in "$@"; do
    if [ ! -f "$deb" ]; then
        echo "error: no such package: $deb" >&2
        exit 1
    fi

    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT

    dpkg-deb -R "$deb" "$tmp/pkg"

    # --- 1. md5sums -----------------------------------------------------
    conffiles="$tmp/conffiles"
    if [ -f "$tmp/pkg/DEBIAN/conffiles" ]; then
        # Stored with a leading slash; strip it to match the relative
        # paths `find` produces below.
        sed 's|^/||' "$tmp/pkg/DEBIAN/conffiles" >"$conffiles"
    else
        : >"$conffiles"
    fi

    # Sorted, for a file that is stable across rebuilds. LC_ALL=C so the
    # order does not shift with the builder's locale. NUL separators keep
    # paths with spaces intact.
    (
        cd "$tmp/pkg"
        find . -path ./DEBIAN -prune -o -type f -print0 |
            sed -z 's|^\./||' |
            LC_ALL=C sort -z |
            while IFS= read -r -d '' file; do
                if grep -qxF "$file" "$conffiles"; then
                    continue
                fi
                printf '%s  %s\n' "$(md5sum "$file" | cut -d' ' -f1)" "$file"
            done
    ) >"$tmp/pkg/DEBIAN/md5sums"
    chmod 0644 "$tmp/pkg/DEBIAN/md5sums"

    # --- 2. empty control fields ---------------------------------------
    # A field line is `Name:` optionally followed by a value; continuation
    # lines start with whitespace. A field is dropped only when its value
    # is empty *and* no continuation line follows.
    awk '
        /^[A-Za-z0-9-]+:[[:space:]]*$/ {
            name = $0
            if ((getline next_line) > 0) {
                if (next_line ~ /^[[:space:]]/) { print name; print next_line }
                else { print next_line }
            }
            next
        }
        { print }
    ' "$tmp/pkg/DEBIAN/control" >"$tmp/control.new"
    mv "$tmp/control.new" "$tmp/pkg/DEBIAN/control"
    chmod 0644 "$tmp/pkg/DEBIAN/control"

    dpkg-deb --root-owner-group -Zxz -b "$tmp/pkg" "$deb" >/dev/null

    count="$(wc -l <"$tmp/pkg/DEBIAN/md5sums")"
    echo "[deb-finalize] $(basename "$deb"): md5sums for ${count} file(s)"

    rm -rf "$tmp"
    trap - EXIT
done
