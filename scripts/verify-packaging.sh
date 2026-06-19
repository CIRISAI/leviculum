#!/usr/bin/env bash
#
# Verify the libleviculum install feels like a normal Unix C library: a staged
# `make install` produces the SONAME symlink chain, header, and pkg-config file,
# and a third-party consumer compiles, links, and runs against it purely through
# pkg-config. Fails loudly on any drift (a renamed export breaking the header, a
# wrong .pc, a missing soname, a load failure).
#
# Usage: scripts/verify-packaging.sh
# Requires: cargo, cc, pkg-config, readelf (binutils).

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ffi_dir="$repo_root/reticulum-ffi"

stage="$(mktemp -d)"
cleanup() { rm -rf "$stage"; }
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }

echo "==> staged install into $stage"
make -C "$ffi_dir" install PREFIX="$stage" >/dev/null

export PKG_CONFIG_PATH="$stage/lib/pkgconfig"

echo "==> install layout"
[ -f "$stage/include/leviculum.h" ] || fail "header not installed"
[ -e "$stage/lib/libleviculum.so" ] || fail "libleviculum.so (dev symlink) missing"
[ -e "$stage/lib/libleviculum.so.0" ] || fail "libleviculum.so.0 (soname symlink) missing"
real="$(readlink -f "$stage/lib/libleviculum.so")"
[ -f "$real" ] || fail "symlink chain does not resolve to a real .so"
soname="$(readelf -d "$real" | sed -n 's/.*SONAME).*\[\(.*\)\]/\1/p')"
[ "$soname" = "libleviculum.so.0" ] || fail "embedded SONAME is '$soname', expected libleviculum.so.0"

echo "==> pkg-config"
pkg-config --exists leviculum || fail "pkg-config does not see leviculum"
want_ver="$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo_root/Cargo.toml" | head -1)"
got_ver="$(pkg-config --modversion leviculum)"
[ "$got_ver" = "$want_ver" ] || fail "pkg-config modversion '$got_ver' != Cargo.toml '$want_ver'"

echo "==> compile + link a consumer purely via pkg-config"
cc "$ffi_dir/examples/c/consumer.c" $(pkg-config --cflags --libs leviculum) -o "$stage/consumer" \
    || fail "consumer failed to compile/link against the installed library"

echo "==> consumer links against the SONAME, not the bare .so"
readelf -d "$stage/consumer" | grep -q "NEEDED.*libleviculum.so.0" \
    || fail "consumer is not dynamically linked to libleviculum.so.0"

echo "==> run the consumer (resolve the lib at runtime)"
out="$(LD_LIBRARY_PATH="$stage/lib" "$stage/consumer")"
echo "$out" | sed 's/^/    /'
echo "$out" | grep -q "^leviculum $want_ver " || fail "consumer did not report version $want_ver"
echo "$out" | grep -q "builder create/free: ok" || fail "consumer could not use the library"

echo "PASS: libleviculum installs and links as a standard Unix C library"
