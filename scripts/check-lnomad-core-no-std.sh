#!/usr/bin/env bash
#
# lnomad's browser core is `no_std` plus `alloc` (slice 4a of
# docs/src/concepts/lnomad-everywhere.md, Codeberg #475), so the ThinkNode M9
# runs the model, `update`, the layout, the scene, the intents and the
# catalogue the terminal and the window run. Without its default `std`
# feature the lnomad library is that core, and leviculum-micron, the page
# parser under it, is `no_std` always. This checks both for the firmware
# triples:
#
#   control   a temp copy of the workspace manifests and Rust sources with a
#             `std::fs` call injected into the core (lnomad/src/app/model.rs)
#             is refused by the same `cargo check` for thumbv7em-none-eabihf,
#             naming the injected line. A check that cannot fail proves
#             nothing; this one has watched itself fail.
#   thumbv7em `cargo check -p lnomad --lib --no-default-features --target
#             thumbv7em-none-eabihf`, the nRF boards' triple.
#   esp32s3   the same for xtensa-esp32s3-none-elf, the M9's ESP32-S3, through
#             the esp-rs compiler fork with `-Z build-std=core,alloc`. The fork
#             is optional per host (see `build-esp32` in the Justfile), so a
#             host without `~/export-esp.sh` says SKIP on its own line; set
#             LNOMAD_NO_STD_REQUIRE_ESP=1 to make that a failure.
#
# The control builds the copy's path crates beside the tree's in the tree's
# own target directory (their paths differ, so their artifacts do too) and
# reuses its registry crates: ~10 s after a warm tree, once.
#
# Usage: check-lnomad-core-no-std.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAG="[lnomad-core-no-std]"
THUMB=thumbv7em-none-eabihf
ESP=xtensa-esp32s3-none-elf
INJECTED='pub fn injected_std() -> bool { std::fs::metadata("/").is_ok() }'

# check_thumb <workspace-root> <cargo lock flag>
check_thumb() {
    (cd "$1" && CARGO_TARGET_DIR="$ROOT/target" cargo check "$2" --quiet \
        -p leviculum-micron -p lnomad --lib --no-default-features --target "$THUMB" 2>&1)
}

control() {
    local tmp out rc=0
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' RETURN
    (cd "$ROOT" && git ls-files -z Cargo.toml Cargo.lock '*/Cargo.toml' '*.rs' \
        | xargs -0 cp --parents -t "$tmp")
    # The working tree's core and manifests, committed or not: the real check
    # below reads those too.
    cp "$ROOT"/lnomad/src/app/*.rs "$tmp/lnomad/src/app/"
    cp "$ROOT/lnomad/Cargo.toml" "$tmp/lnomad/Cargo.toml"
    cp "$ROOT/leviculum-micron/Cargo.toml" "$tmp/leviculum-micron/Cargo.toml"
    printf '%s\n' "$INJECTED" >> "$tmp/lnomad/src/app/model.rs"
    out="$(check_thumb "$tmp" --offline)" || rc=$?
    if [ "$rc" -eq 0 ] || ! grep -qF 'std::fs::metadata' <<<"$out"; then
        echo "$TAG CONTROL FAIL: a std::fs call injected into the core was not refused (exit $rc)"
        printf '%s\n' "$out" | sed 's/^/  /'
        return 1
    fi
    echo "$TAG control ok: a std::fs call injected into lnomad/src/app/model.rs is refused for $THUMB"
}

rustup target add "$THUMB" >/dev/null 2>&1 || true
control
if ! out="$(check_thumb "$ROOT" --frozen)"; then
    printf '%s\n' "$out"
    echo "$TAG FAIL lnomad's core or leviculum-micron needs std for $THUMB (above)."
    echo "$TAG The platform layer is lnomad's \`std\` feature; the core takes alloc only."
    exit 1
fi
echo "$TAG ok   leviculum-micron and lnomad's core build for $THUMB"

if [ ! -f "$HOME/export-esp.sh" ]; then
    if [ "${LNOMAD_NO_STD_REQUIRE_ESP:-0}" = 1 ]; then
        echo "$TAG FAIL no esp toolchain (~/export-esp.sh) and LNOMAD_NO_STD_REQUIRE_ESP=1"
        exit 1
    fi
    echo "$TAG SKIP $ESP: no esp toolchain on this host (~/export-esp.sh; scripts/install-ci.sh installs it)"
    exit 0
fi
if ! out="$(cd "$ROOT" && bash -c '. ~/export-esp.sh >/dev/null && cargo +esp check --frozen --quiet \
        -p leviculum-micron -p lnomad --lib --no-default-features \
        --target '"$ESP"' -Z build-std=core,alloc' 2>&1)"; then
    printf '%s\n' "$out"
    echo "$TAG FAIL lnomad's core or leviculum-micron does not build for $ESP (above)."
    exit 1
fi
echo "$TAG ok   leviculum-micron and lnomad's core build for $ESP"
