#!/usr/bin/env bash
#
# The M9's renderer (`loquax-m9`, slice one of "The M9 frontend: the plan,
# measured" in docs/src/concepts/handheld-messenger.md, Codeberg #232 and
# #473) is `no_std` plus `alloc` always, so the firmware links the same
# crate the host draws its goldens with. With it the loquax core and the
# styled-line bridge of leviculum-ui-ratatui without its `terminal`
# feature, which it builds on. This checks the crate for the firmware
# triples:
#
#   control   a temp copy of the workspace manifests and Rust sources with a
#             `std::fs` call injected into the renderer (loquax-m9/src/
#             screen.rs) is refused by the same `cargo check` for
#             thumbv7em-none-eabihf, naming the injected line; and so is
#             one injected into the client the board runs (loquax/src/home/
#             ask.rs, slice five, 1363), which the crate builds on with the
#             pairing screen (loquax-m9/src/pair.rs).
#   thumbv7em `cargo check -p loquax-m9 --lib --target thumbv7em-none-eabihf`.
#   esp32s3   the same for xtensa-esp32s3-none-elf, the M9's ESP32-S3, through
#             the esp-rs compiler fork with `-Z build-std=core,alloc`. The fork
#             is optional per host (see `build-esp32` in the Justfile), so a
#             host without `~/export-esp.sh` says SKIP on its own line; set
#             M9_NO_STD_REQUIRE_ESP=1 to make that a failure.
#
# Usage: check-m9-render-no-std.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAG="[m9-render-no-std]"
THUMB=thumbv7em-none-eabihf
ESP=xtensa-esp32s3-none-elf
INJECTED='pub fn injected_std() -> bool { std::fs::metadata("/").is_ok() }'

# check_thumb <workspace-root> <cargo lock flag>
check_thumb() {
    (cd "$1" && CARGO_TARGET_DIR="$ROOT/target" cargo check "$2" --quiet \
        -p loquax-m9 --lib --target "$THUMB" 2>&1)
}

# control: one temp copy, the std call injected into the renderer first
# and checked, then into the client as well and checked again; the second
# check only rebuilds the loquax core, which fails before the renderer.
control() {
    local tmp out dir target rc
    # Under the tree's target, not /tmp: the copy is ~20 MB and /tmp is the
    # small tmpfs every lane shares.
    mkdir -p "$ROOT/target/tmp"
    tmp="$(mktemp -d "$ROOT/target/tmp/m9-no-std.XXXXXX")"
    trap 'rm -rf "$tmp"' RETURN
    (cd "$ROOT" && git ls-files -z Cargo.toml Cargo.lock '*/Cargo.toml' '*.rs' \
        | xargs -0 cp --parents -t "$tmp")
    # Every tracked file of the path crates the renderer builds on, not only
    # their sources: the map core and the loquax core include data files.
    for dir in $(cd "$ROOT" && cargo tree --frozen -q -p loquax-m9 -e normal,build \
            --prefix none --target "$THUMB" | sed -n 's/.*(\(\/[^)]*\)).*/\1/p' | sort -u); do
        (cd "$ROOT" && git ls-files -z "${dir#"$ROOT"/}" | xargs -0 -r cp --parents -t "$tmp")
    done
    # The working tree's crates and manifests, committed or not: the real
    # check below reads those too.
    mkdir -p "$tmp/loquax-m9/src"
    cp "$ROOT"/loquax-m9/src/*.rs "$tmp/loquax-m9/src/"
    cp "$ROOT/loquax-m9/Cargo.toml" "$tmp/loquax-m9/Cargo.toml"
    cp "$ROOT/Cargo.toml" "$ROOT/Cargo.lock" "$tmp/"
    cp "$ROOT/leviculum-ui-ratatui/Cargo.toml" "$tmp/leviculum-ui-ratatui/Cargo.toml"
    cp "$ROOT"/leviculum-ui-ratatui/src/*.rs "$tmp/leviculum-ui-ratatui/src/"
    cp "$ROOT"/loquax/src/home/*.rs "$tmp/loquax/src/home/"
    for target in loquax-m9/src/screen.rs loquax/src/home/ask.rs; do
        rc=0
        printf '%s\n' "$INJECTED" >> "$tmp/$target"
        out="$(check_thumb "$tmp" --offline)" || rc=$?
        if [ "$rc" -eq 0 ] || ! grep -qF "$target" <<<"$out" \
                || ! grep -qF 'std::fs::metadata' <<<"$out"; then
            echo "$TAG CONTROL FAIL: a std::fs call injected into $target was not refused (exit $rc)"
            printf '%s\n' "$out" | sed 's/^/  /'
            return 1
        fi
        echo "$TAG control ok: a std::fs call injected into $target is refused for $THUMB"
    done
}

rustup target add "$THUMB" >/dev/null 2>&1 || true
control
if ! out="$(check_thumb "$ROOT" --frozen)"; then
    printf '%s\n' "$out"
    echo "$TAG FAIL the M9's renderer needs std for $THUMB (above)."
    exit 1
fi
echo "$TAG ok   loquax-m9 (renderer, pairing) and the client it builds on build for $THUMB"

if [ ! -f "$HOME/export-esp.sh" ]; then
    if [ "${M9_NO_STD_REQUIRE_ESP:-0}" = 1 ]; then
        echo "$TAG FAIL no esp toolchain (~/export-esp.sh) and M9_NO_STD_REQUIRE_ESP=1"
        exit 1
    fi
    echo "$TAG SKIP $ESP: no esp toolchain on this host (~/export-esp.sh; scripts/install-ci.sh installs it)"
    exit 0
fi
if ! out="$(cd "$ROOT" && bash -c '. ~/export-esp.sh >/dev/null && cargo +esp check --frozen --quiet \
        -p loquax-m9 --lib --target '"$ESP"' -Z build-std=core,alloc' 2>&1)"; then
    printf '%s\n' "$out"
    echo "$TAG FAIL the M9's renderer does not build for $ESP (above)."
    exit 1
fi
echo "$TAG ok   loquax-m9 (renderer, pairing) and the client it builds on build for $ESP"
