#!/usr/bin/env bash
#
# lnomad's browser core does not know it has a terminal (Codeberg #219,
# docs/src/concepts/loquax-architecture.md, "the core must not know it has a
# terminal"). The core is `lnomad/src/app/`; the terminal frontend is the
# `tui` cargo feature. Three checks, cheapest first; `--static` runs the
# first two (and the positive control), `--build` the third:
#
#   source  no terminal crate is named in lnomad/src/app/ outside comments,
#           and nothing there is gated on the `tui` feature. The second half
#           is what the build below cannot see: a terminal import behind
#           `cfg(feature = "tui")` compiles away exactly when it is checked.
#   graph   lnomad without default features but with `std` (the platform
#           layer a window builds on; without `std` the core is `no_std`,
#           which `just lnomad-core-no-std` holds) has no terminal crate in
#           its resolved normal-dependency graph, whichever crate would pull
#           it (tui-input's default features pull both ratatui and crossterm).
#   build   that configuration compiles and lints clean: `cargo clippy -p
#           lnomad --lib --no-default-features --features std -- -D warnings`. Lint-clean
#           matters as much as compiling: an item only the frontend reads is
#           dead code here, which is the sign it belongs in the frontend or in
#           the core's public frontend API, not in a `pub(crate)` corner.
#
# Positive control first, on a temp copy of the workspace manifests and Rust
# sources (~27 MB, 0.2 s): the unmodified copy passes source and graph, a
# `use ratatui::layout::Rect;` injected into the copy's core is refused by the
# source check, and tui-input with its default features is refused by the
# graph check, naming the crate.
#
# The same two checks guard leviculum-ui, the terminal-free half of the UI
# lnomad and loquax share (984, Codeberg #475): `--ui-static` runs source (over
# leviculum-ui/src/) and graph (leviculum-ui with every feature) after its own
# positive control, an injected ratatui import and tui-input's default
# features in leviculum-ui's copy; `--ui-build` lints the crate alone. Its
# terminal half, leviculum-ui-ratatui, is itself a forbidden name in both
# graphs.
#
# Usage: check-lnomad-core-no-tui.sh --static | --build | --ui-static | --ui-build

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAG="[lnomad-core-no-tui]"
FORBIDDEN=(ratatui crossterm termbg terminal_size ratatui-image leviculum-ui-ratatui)

# check_source <workspace-root> [<source dir, relative>]
check_source() {
    local rel="${2:-lnomad/src/app}"
    local core="$1/$rel" hits
    # Comments may name the crates (the core documents what it mirrors);
    # code may not. `//` to end of line is dropped before matching.
    hits="$(grep -rn --include='*.rs' '' "$core" \
        | awk -F: '{line=$0; sub(/^[^:]*:[^:]*:/, "", line); sub(/\/\/.*/, "", line);
                    if (line ~ /(^|[^A-Za-z0-9_])(ratatui|crossterm|termbg|terminal_size|ratatui_image|leviculum_ui_ratatui|tui_input::backend)::/ ||
                        line ~ /feature *= *"tui"/) print $1 ":" $2 ": " line}')"
    if [ -n "$hits" ]; then
        echo "$TAG FAIL $rel names a terminal crate or the tui feature:"
        printf '%s\n' "$hits" | sed "s#^$1/##; s/^/  /"
        echo "$TAG The core takes AppEvent in and gives Effect out; terminal types"
        echo "$TAG belong in lnomad/src/tui.rs or leviculum-ui-ratatui."
        return 1
    fi
    echo "$TAG ok   $rel/ names no terminal crate and no tui gate"
}

# check_graph <workspace-root> <cargo lock flag> [<package> <feature flag>]
check_graph() {
    local tree pkg="${3:-lnomad}" feat="${4:---no-default-features --features=std}"
    # shellcheck disable=SC2086 # $feat is one or two flags
    if ! tree="$(cd "$1" && cargo tree "$2" -p "$pkg" $feat \
        -e normal --prefix none 2>&1)"; then
        echo "$TAG ERROR cargo tree failed:"
        printf '%s\n' "$tree" | sed 's/^/  /'
        return 2
    fi
    local hits=() name crate
    while read -r name _; do
        for crate in "${FORBIDDEN[@]}"; do
            if [ "$name" = "$crate" ] || [[ "$name" == "$crate-"* ]]; then
                hits+=("$name")
            fi
        done
    done <<< "$tree"
    if [ "${#hits[@]}" -gt 0 ]; then
        mapfile -t hits < <(printf '%s\n' "${hits[@]}" | sort -u)
        for name in "${hits[@]}"; do
            echo "$TAG FAIL $pkg ($feat) carries $name"
            # shellcheck disable=SC2086 # $feat is one or two flags
            echo "$TAG   path: $(cd "$1" && cargo tree "$2" -p "$pkg" $feat \
                -e normal -i "$name" --prefix none 2>/dev/null | tr '\n' ' ')"
        done
        echo "$TAG A terminal crate is an optional dependency behind lnomad's tui"
        echo "$TAG feature, and never one of leviculum-ui's."
        return 1
    fi
    echo "$TAG ok   none of ${FORBIDDEN[*]} in $pkg's graph ($feat)"
}

# copy_tree <dest>: the workspace manifests and Rust sources the index knows.
copy_tree() {
    (cd "$ROOT" && git ls-files -z Cargo.toml Cargo.lock '*/Cargo.toml' '*.rs' \
        | xargs -0 cp --parents -t "$1")
}

selftest() {
    local tmp out rc=0 rc2
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' RETURN
    copy_tree "$tmp"
    # The core files may be new and not yet committed; the copy takes the
    # working tree's core, which is what the real check reads too.
    mkdir -p "$tmp/lnomad/src/app"
    cp "$ROOT"/lnomad/src/app/*.rs "$tmp/lnomad/src/app/"
    cp "$ROOT/lnomad/Cargo.toml" "$tmp/lnomad/Cargo.toml"
    if ! out="$(check_source "$tmp" && check_graph "$tmp" --offline)"; then
        # The copy carries the working tree's core and manifest, so this is
        # the tree itself failing; the selftest has nothing clean to inject
        # into, and the verdict below is the tree's.
        printf '%s\n' "$out"
        echo "$TAG FAIL before any injection: the working tree is refused (above)"
        return 1
    fi
    printf 'use ratatui::layout::Rect;\n' >> "$tmp/lnomad/src/app/model.rs"
    rc2=0
    out="$(check_source "$tmp")" || rc2=$?
    if [ "$rc2" -ne 1 ] || ! grep -qF "use ratatui::layout::Rect" <<<"$out"; then
        echo "$TAG SELFTEST FAIL: an injected ratatui import was not refused (exit $rc2)"
        printf '%s\n' "$out"
        rc=1
    fi
    # The dependency is optional since slice 4a (lnomad's `std` feature),
    # which the graph check turns on.
    sed -i -E 's/^tui-input = \{ version = "0.15", default-features = false, optional = true \}$/tui-input = { version = "0.15", optional = true }/' \
        "$tmp/lnomad/Cargo.toml"
    if ! grep -qx 'tui-input = { version = "0.15", optional = true }' "$tmp/lnomad/Cargo.toml"; then
        echo "$TAG SELFTEST FAIL: could not inject tui-input's default features into lnomad's manifest copy; the pattern above no longer matches its tui-input line"
        return 1
    fi
    rc2=0
    out="$(check_graph "$tmp" --offline)" || rc2=$?
    if [ "$rc2" -ne 1 ] || ! grep -qF "carries ratatui" <<<"$out"; then
        echo "$TAG SELFTEST FAIL: tui-input with default features was not refused (exit $rc2)"
        printf '%s\n' "$out"
        rc=1
    fi
    [ "$rc" -eq 0 ] && echo "$TAG selftest ok: an injected ratatui import and tui-input's default features are refused"
    return "$rc"
}

# The same positive control for leviculum-ui: the working tree's crate passes
# both checks, an injected ratatui import is refused by the source check, and
# tui-input with its default features by the graph check.
selftest_ui() {
    local tmp out rc=0 rc2
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' RETURN
    copy_tree "$tmp"
    mkdir -p "$tmp/leviculum-ui/src"
    cp "$ROOT"/leviculum-ui/src/*.rs "$tmp/leviculum-ui/src/"
    cp "$ROOT/leviculum-ui/Cargo.toml" "$tmp/leviculum-ui/Cargo.toml"
    if ! out="$(check_source "$tmp" leviculum-ui/src \
        && check_graph "$tmp" --offline leviculum-ui --all-features)"; then
        printf '%s\n' "$out"
        echo "$TAG FAIL before any injection: the working tree is refused (above)"
        return 1
    fi
    printf 'use ratatui::layout::Rect;\n' >> "$tmp/leviculum-ui/src/lib.rs"
    rc2=0
    out="$(check_source "$tmp" leviculum-ui/src)" || rc2=$?
    if [ "$rc2" -ne 1 ] || ! grep -qF "use ratatui::layout::Rect" <<<"$out"; then
        echo "$TAG SELFTEST FAIL: an injected ratatui import in leviculum-ui was not refused (exit $rc2)"
        printf '%s\n' "$out"
        rc=1
    fi
    # The dependency is optional since 994 (leviculum-ui's `std` feature),
    # and `--all-features` below turns it on.
    sed -i -E 's/^tui-input = \{ version = "0.15", default-features = false, optional = true \}$/tui-input = { version = "0.15", optional = true }/' \
        "$tmp/leviculum-ui/Cargo.toml"
    if ! grep -qx 'tui-input = { version = "0.15", optional = true }' "$tmp/leviculum-ui/Cargo.toml"; then
        echo "$TAG SELFTEST FAIL: could not inject tui-input's default features into leviculum-ui's manifest copy; the pattern above no longer matches its tui-input line"
        rc=1
    fi
    rc2=0
    out="$(check_graph "$tmp" --offline leviculum-ui --all-features)" || rc2=$?
    if [ "$rc2" -ne 1 ] || ! grep -qF "carries ratatui" <<<"$out"; then
        echo "$TAG SELFTEST FAIL: tui-input with default features in leviculum-ui was not refused (exit $rc2)"
        printf '%s\n' "$out"
        rc=1
    fi
    [ "$rc" -eq 0 ] && echo "$TAG selftest ok: leviculum-ui refuses an injected ratatui import and tui-input's default features"
    return "$rc"
}

case "${1:-}" in
    --static)
        selftest
        check_source "$ROOT"
        check_graph "$ROOT" --frozen
        ;;
    --build)
        (cd "$ROOT" && cargo clippy --frozen -p lnomad --lib --no-default-features \
            --features std --quiet -- -D warnings)
        echo "$TAG ok   lnomad's core builds and lints clean without the tui feature"
        ;;
    --ui-static)
        TAG="[ui-no-terminal]"
        selftest_ui
        check_source "$ROOT" leviculum-ui/src
        check_graph "$ROOT" --frozen leviculum-ui --all-features
        ;;
    --ui-build)
        TAG="[ui-no-terminal]"
        (cd "$ROOT" && cargo clippy --frozen -p leviculum-ui --all-targets --all-features \
            --quiet -- -D warnings)
        echo "$TAG ok   leviculum-ui builds and lints clean on its own"
        ;;
    *)
        echo "usage: $0 --static | --build | --ui-static | --ui-build" >&2
        exit 2
        ;;
esac
