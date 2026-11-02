#!/usr/bin/env bash
#
# The orchestration stratum links no HAL (Codeberg #469, gate
# `firmware-no-hal` of docs/src/concepts/firmware-layers.md, "Gates the new
# stratum brings").
#
# `leviculum-firmware` is the one copy of the node loop every chip family
# binds to, so a HAL or a radio stack in its graph is a chip in stratum 2,
# whichever module pulled it. The subject is cargo's resolved graph rather
# than the manifest text, so a HAL that arrives through some intermediate
# crate is caught too; the precedent is scripts/check-nrf-evt-max-size.sh.
# `--target all` and dev edges included: a HAL behind a target table or in a
# test-only dependency is still a chip the crate was built against.
#
# Positive control first, on a temp copy of the workspace sources (git
# ls-files of the firmware workspace and its path dependencies, ~14 MB): the
# unmodified copy must pass, and the copy with `embassy-nrf` injected as a
# dev-dependency must be refused naming it.
#
# Usage: check-firmware-no-hal.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FORBIDDEN=(embassy-nrf nrf-softdevice esp-hal embassy-rp cyw43 trouble-host)

# check <workspace-root> <cargo lock flag>
check() {
    local tree
    if ! tree="$(cd "$1/leviculum-nrf" && cargo tree "$2" -p leviculum-firmware \
        --target all -e normal,build,dev --prefix none 2>&1)"; then
        echo "[firmware-no-hal] ERROR cargo tree failed:"
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
            echo "[firmware-no-hal] FAIL leviculum-firmware's graph carries $name"
            echo "[firmware-no-hal]   path: $(cd "$1/leviculum-nrf" && cargo tree "$2" \
                -p leviculum-firmware --target all -e normal,build,dev -i "$name" \
                --prefix none 2>/dev/null | tr '\n' ' ')"
        done
        echo "[firmware-no-hal] Stratum 2 names no chip; a backend belongs in a bind"
        echo "[firmware-no-hal] crate (docs/src/concepts/firmware-layers.md)."
        return 1
    fi
    echo "[firmware-no-hal] ok   none of ${FORBIDDEN[*]} in leviculum-firmware's graph"
}

selftest() {
    local tmp rc=0 out
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' RETURN
    (cd "$ROOT" && git ls-files -z Cargo.toml leviculum-nrf leviculum-core leviculum-lxmf \
        leviculum-lr11xx vendor | xargs -0 cp --parents -t "$tmp")
    # The copy may resolve a changed graph, so it may not use --frozen; it
    # resolves offline from the registry cache the real tree already filled.
    if ! out="$(check "$tmp" --offline)"; then
        echo "[firmware-no-hal] SELFTEST FAIL: the unmodified copy was refused"
        printf '%s\n' "$out"
        return 1
    fi
    printf '\n[target.%s.dev-dependencies]\nembassy-nrf = { version = "0.9", features = ["nrf52840"] }\n' \
        "'cfg(all())'" >> "$tmp/leviculum-nrf/firmware/Cargo.toml"
    local rc2=0
    out="$(check "$tmp" --offline)" || rc2=$?
    if [ "$rc2" -ne 1 ] || ! grep -qF "carries embassy-nrf" <<<"$out"; then
        echo "[firmware-no-hal] SELFTEST FAIL: an injected embassy-nrf dev-dependency was not refused (exit $rc2)"
        printf '%s\n' "$out"
        rc=1
    fi
    [ "$rc" -eq 0 ] && echo "[firmware-no-hal] selftest ok: an injected embassy-nrf is refused"
    return "$rc"
}

selftest
check "$ROOT" --frozen
