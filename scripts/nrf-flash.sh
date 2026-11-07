#!/usr/bin/env bash
#
# Build one nRF firmware image and flash it with the UF2 runner (order 1460,
# #474).
#
# The flash recipes used to say `cargo run`, which builds and then hands the
# ELF to leviculum-nrf/tools/uf2-runner.sh (the cargo runner). The build now
# happens at one fixed path in a private mount namespace
# (scripts/nrf-rustflags.sh, the header says why), and the runner must not
# run in there: a UF2 volume mounted after the 1200-baud touch never appears
# inside it. So this builds through the wrapper and runs the runner outside,
# on the ELF in the target directory cargo reports (scripts/cargo-target-dir.sh),
# the same directory the build in the namespace wrote to.
#
# Before flashing it prints the image as flashed, the flat binary the runner
# makes the UF2 from:
#
#   NRF_IMAGE bin=<bin> sha256=<sha256> bytes=<n>
#
# so a reviewer who builds the same commit from a clone can compare the hash
# before the board takes it (scripts/check-nrf-reproducible.sh shows the two
# builds agree). The runner's environment (LEVICULUM_FLASH_ONLY,
# LEVICULUM_USB_PID, ...) passes through unchanged.
#
# Usage:
#   bash scripts/nrf-flash.sh <bin> <features>
#   NRF_FLASH_BUILD_ONLY=1 bash scripts/nrf-flash.sh <bin> <features>  # no flash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NRF="$ROOT/leviculum-nrf"
bin="${1:?usage: nrf-flash.sh <bin> <features>}"
features="${2:?usage: nrf-flash.sh <bin> <features>}"

# shellcheck source=scripts/cargo-target-dir.sh
source "$ROOT/scripts/cargo-target-dir.sh"
# shellcheck source=scripts/nrf-rustflags.sh
source "$ROOT/scripts/nrf-rustflags.sh"
# shellcheck source=scripts/nrf-elf.sh
source "$ROOT/scripts/nrf-elf.sh"

(cd "$NRF" && nrf_cargo build --release --bin "$bin" --features "$features")
elf="$(cargo_target_dir "$NRF")/thumbv7em-none-eabihf/release/$bin"
[ -f "$elf" ] || { echo "nrf-flash: the build left no ELF at $elf" >&2; exit 1; }

flat="$(mktemp "${elf}.image.XXXXXX")"
if ! nrf_flat_image "$elf" "$flat"; then
    rm -f "$flat"
    exit 1
fi
echo "NRF_IMAGE bin=$bin sha256=$(sha256sum "$flat" | cut -d' ' -f1) bytes=$(stat -c %s "$flat")"
rm -f "$flat"

if [ "${NRF_FLASH_BUILD_ONLY:-0}" = 1 ]; then
    echo "nrf-flash: NRF_FLASH_BUILD_ONLY=1, not flashing $elf"
    exit 0
fi
cd "$NRF"
exec bash tools/uf2-runner.sh "$elf"
