#!/usr/bin/env bash
# Codeberg #384: what each candidate store costs in flash.
#
# Builds both firmware bins three ways - without the harness, with
# leviculum-record-log, with sequential-storage - and prints
# arm-none-eabi-size for each plus the deltas. Nothing is flashed; the
# harness features exist only so the linker keeps the candidate's code (see
# src/store_spike.rs).
#
# Run from leviculum-nrf/: bash tools/store-spike-size.sh
set -euo pipefail

cd "$(dirname "$0")/.."
NRF="$PWD"
# cargo decides where the ELFs land; CARGO_TARGET_DIR moves them out of the
# workspace entirely (scripts/cargo-target-dir.sh in the repo root).
# shellcheck source-path=SCRIPTDIR/../..
# shellcheck source=scripts/cargo-target-dir.sh
source "$NRF/../scripts/cargo-target-dir.sh"
elfdir="$(cargo_target_dir "$NRF")/thumbv7em-none-eabihf/release"
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT

sect() { # sect <elf> <section>
    arm-none-eabi-size -A "$1" | awk -v s="$2" '$1 == s { print $2 }'
}

bin_features() {
    case "$1" in
    t114) echo "bsp-t114" ;;
    rak4631) echo "bsp-rak4631,rak-baseboard" ;;
    *)
        echo "unknown bin $1" >&2
        exit 1
        ;;
    esac
}

for bin in t114 rak4631; do
    base_features=$(bin_features "$bin")
    for variant in none record-log sequential; do
        case "$variant" in
        none) features="$base_features" ;;
        record-log) features="$base_features,store-spike-record-log" ;;
        sequential) features="$base_features,store-spike-sequential" ;;
        esac
        cargo build --release --bin "$bin" --features "$features" >/dev/null
        cp "$elfdir/$bin" "$out/$bin-$variant"
    done

    echo "=== $bin ($base_features)"
    printf '%-12s %10s %10s %10s %10s\n' variant .text .rodata .bss .data
    for variant in none record-log sequential; do
        elf="$out/$bin-$variant"
        printf '%-12s %10s %10s %10s %10s\n' "$variant" \
            "$(sect "$elf" .text)" "$(sect "$elf" .rodata)" \
            "$(sect "$elf" .bss)" "$(sect "$elf" .data)"
    done
    for variant in record-log sequential; do
        for s in .text .rodata .bss .data; do
            a=$(sect "$out/$bin-none" "$s")
            b=$(sect "$out/$bin-$variant" "$s")
            printf 'delta %-12s %-8s %+d\n' "$variant" "$s" "$((b - a))"
        done
    done
    echo
done
