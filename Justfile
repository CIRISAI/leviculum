# Exclude reticulum-integ: its lib tests are Docker-based and require
# --test-threads=1 (run in Tier 2). Including them here would race-fail
# and blow the 3 min budget.

# Minimum-viable-reproduction tier — discipline tier, not size tier.
# Each test < 5 s, deterministic, single named failure mode.  See
# Codeberg #39 for design intent.  --test-threads=1 avoids
# port/resource contention between concurrent integration-style tests
# in the same binary.  Depends on build-integ-bins because the mvr
# tests spawn the release lnsd/lncp binaries directly.
mvr: build-integ-bins
    cargo test -p leviculum-std --test mvr -- --test-threads=1

# Promote the most-recent auto-bug bundle to the coder bridge.
# Run after a tier RED has emitted to $BRIDGE/auto-bug/instructions.md
# and you want the coder to pick it up.  See scripts/_emit-auto-bug-bundle.sh.
# BRIDGE defaults to ~/.local/state/leviculum (override LEVICULUM_BRIDGE).
# The source bundle is left in place so a re-promotion (e.g. after a
# stomped bridge) works without re-triggering the failing tier.
# One shell block so BRIDGE persists across the recipe lines.
spawn-coder:
    @BRIDGE="${LEVICULUM_BRIDGE:-$HOME/.local/state/leviculum}"; \
    if [ ! -s "$BRIDGE/auto-bug/instructions.md" ]; then \
        echo "ERROR: no auto-bug bundle at $BRIDGE/auto-bug/instructions.md"; \
        echo "       Either no recent tier RED, or the file was removed by hand."; \
        exit 1; \
    fi; \
    cp "$BRIDGE/auto-bug/instructions.md" "$BRIDGE/instructions.md"; \
    echo "[spawn-coder] bundle promoted to bridge: $BRIDGE/instructions.md"; \
    echo "[spawn-coder] source: $BRIDGE/auto-bug/instructions.md (left in place for re-promotion)"

# Lint the embedded firmware workspace. leviculum-nrf is its OWN cargo
# workspace — `--workspace` invocations in the repo root never reach it,
# which let 11 clippy findings accumulate unseen (audit 2026-06-11).
# Both BSP feature sets; clippy subsumes `cargo check` diagnostics.
# First run compiles the embedded deps into leviculum-nrf/target
# (minutes); warm runs are seconds.
lint-nrf:
    cd leviculum-nrf && cargo clippy --features bsp-rak4631,rak-baseboard -- -D warnings
    cd leviculum-nrf && cargo clippy --features bsp-t114 -- -D warnings

# Rustdoc gate: broken intra-doc links fail instead of warning.
doc-gate:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Tracing-shim gate (PR #57): leviculum-core must pass the SAME suite with
# tracing OFF as with it on. The `tracing` feature is default-on; with it
# off the level macros become no-ops via the `crate::tracing` shim in
# lib.rs. Running the full suite in that config proves the shim changed no
# core logic (a bare `tracing::x!` that slipped past the shim would either
# fail to compile here or, worse, only on M0 — see m0-build-gate).
core-no-tracing:
    cargo test -p leviculum-core --no-default-features

# Cortex-M0 gate (PR #57): leviculum-core must cross-compile for thumbv6m
# (atomic-less MCU, e.g. rp2040) with tracing off. tracing-core's CAS-based
# callsite registry does not compile there, so the default build FAILS on
# M0; --no-default-features must succeed. Keeps M0 support from rotting.
m0-build-gate:
    rustup target add thumbv6m-none-eabi
    cargo build -p leviculum-core --target thumbv6m-none-eabi --no-default-features

# Tier 0 (~3 min, runs on every git push): fmt + clippy (host + nrf)
# + rustdoc gate + tracing-shim + M0 gates + workspace lib tests.
fast: mvr lint-nrf doc-gate core-no-tracing m0-build-gate
    cargo fmt --all -- --check
    cargo clippy --workspace -- -D warnings
    cargo test --workspace --lib --exclude reticulum-integ

# First run after a fresh CARGO_TARGET_DIR: 20-40 min. Runs in background
# after every commit via the post-commit hook.
# Tier 1 (~15 min): Tier 0 + core/tests + ffi (incl. C-program + Python interop)
# + proxy + rnsd_interop + the TCP-hub endurance smoke soak.
standard: fast test-ffi verify-packaging
    cargo test -p leviculum-core --tests
    cargo test -p leviculum-proxy
    cargo test -p leviculum-std --test rnsd_interop
    cargo test -p leviculum-std --test event_log_subscriber -- --test-threads=1
    cargo test -p leviculum-std --test event_log_multiprocess
    # Endurance gate (#101): builds lnsd, boots it as a hub, asserts 100%
    # delivery + RSS plateau + no fd leak. ~15 s smoke; `--full` is on demand.
    bash scripts/run-soak.sh

# Build the production binaries the integ runner mounts into Docker
# containers. Explicit per-bin list avoids `--workspace --bins` which
# would also try to build leviculum-nrf firmware on the host. Runs on
# the same CARGO_TARGET_DIR as the enclosing `cargo test`, so the
# runner's CARGO_TARGET_DIR-aware path resolver finds them.
#
# Touch the bin-crate sources first so cargo always relinks and stamps a
# fresh mtime: after a repo-sync pulls newer commits without changing
# source mtimes, cargo would otherwise skip the relink and leave
# binaries that check_binary_freshness rejects (2026-06-13 nightly).
# Every tier that mounts binaries depends on this target, so the
# guarantee holds for tier1/tier2 the same way run-tier3-hw.sh enforces
# it for the hardware nightly. Deleting the binary does NOT work: cargo
# re-hardlinks it from deps/ without relinking, keeping the old mtime.
build-integ-bins:
    find leviculum-cli/src leviculum-proxy/src -name '*.rs' -exec touch {} +
    cargo build --release --bin lnsd --bin lnstest --bin lncp --bin lora-proxy

# Default cargo test runs non-ignored tests and skips ignored ones.
# Docker tests (#[serial(docker)]) run; LoRa tests (#[ignore] #[serial(lora)])
# skip automatically. --test-threads=1 is required even though serial_test
# enforces per-group serialization, because the embedded #[serial(docker)]
# groups can still overlap resource usage with unit tests in the same
# binary on a multi-CPU harness.
# Tier 2 (~30-90 min, on demand: `systemctl --user start
# leviculum-ci-tier2.service`): Tier 1 + Docker integ suite.
extensive: standard build-integ-bins build-c-lnsd
    cargo test -p reticulum-integ -- --test-threads=1

# --include-ignored adds the LoRa hardware tests on top of Tier 2.
# Tier 3 (~2-6h, 02:00 nightly): Tier 2 + LoRa hardware tests.
nightly: extensive
    cargo test -p reticulum-integ -- --include-ignored --test-threads=1

# Build leviculum-ffi as a real glibc-dynamic cdylib + staticlib for
# C-API consumers ("apt install libreticulum-dev" ergonomics). This
# deliberately overrides the workspace musl default — see the comment
# in .cargo/config.toml. cbindgen regenerates leviculum.h as a side
# effect of the build.rs.
# Comprehensive C API test suite on the glibc target: the Rust unit,
# integration, and Python-interop suites plus the C acceptance programs linked
# against the real cdylib. Builds the debug glibc cdylib first, because once
# the crate has an rlib `cargo test` no longer builds the cdylib, and the
# C-program harness needs libleviculum.so to link and run. The Python interop
# tests skip cleanly if Python RNS is unavailable.
test-ffi:
    cargo build -p leviculum-ffi --target x86_64-unknown-linux-gnu
    cargo test-ffi

# Memory- and race-check the C API under sanitizers and Miri. On demand, not in
# the standard tiers: it needs the nightly toolchain
# (`rustup toolchain install nightly --component rust-src miri`) and is heavy,
# since -Zbuild-std rebuilds std and every dependency with instrumentation
# (several GB of target per sanitizer). AddressSanitizer (+ LeakSanitizer) and
# ThreadSanitizer run the in-process two-node integration suite, covering the
# handle lifecycle, the eventfd bridge, and the two-runtime threading; ASan also
# runs the property suite, where randomised buffer sizes stress the read(2)
# protocol for overflows. Miri
# checks the pure unsafe marshalling paths (buffer read(2), handle boxing,
# char** aspects); it cannot run tokio or real I/O, so node/network tests are
# excluded by filtering to identity/hex/destination.
sanitize-ffi:
    RUSTFLAGS="-Zsanitizer=address" cargo +nightly test -p leviculum-ffi -Zbuild-std --target x86_64-unknown-linux-gnu --test ffi_unit --test ffi_integration --test ffi_property -- --test-threads=1
    RUSTFLAGS="-Zsanitizer=thread" TSAN_OPTIONS="halt_on_error=0 suppressions={{justfile_directory()}}/leviculum-ffi/tsan-suppressions.txt" cargo +nightly test -p leviculum-ffi -Zbuild-std --target x86_64-unknown-linux-gnu --test ffi_integration -- --test-threads=1
    MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test -p leviculum-ffi --test ffi_unit identity
    MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test -p leviculum-ffi --test ffi_unit hex
    MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test -p leviculum-ffi --test ffi_unit destination

build-ffi:
    cargo build-ffi

# Verify libleviculum installs and links like a standard Unix C library: a
# staged `make install` produces the SONAME symlink chain, header, static
# archive, and pkg-config file, and a consumer compiles, links, and runs
# against it purely through pkg-config, both dynamically and statically.
# Catches a renamed export breaking the header, a wrong .pc, a missing soname,
# or a load failure. Part of Tier 1.
verify-packaging:
    bash scripts/verify-packaging.sh

# Same end-to-end packaging check for the aarch64 cross build: cross-compiles
# the consumer and runs it under qemu. Skips cleanly if the cross toolchain
# (rustup target + gcc-aarch64-linux-gnu + qemu-user-static) is absent.
verify-packaging-arm64:
    bash scripts/verify-packaging.sh aarch64-unknown-linux-gnu

# Same for ARM64. Requires `sudo apt install gcc-aarch64-linux-gnu` and
# `rustup target add aarch64-unknown-linux-gnu` on the build host.
build-ffi-arm64:
    cargo build-ffi-arm64

# Build the C daemon (examples/c/lnsd.c) as a self-contained binary, linking
# libleviculum.a statically (glibc stays dynamic, matching the debian-slim
# integ container). Output: target/release/c-lnsd, the binary the
# reticulum-integ runner mounts for a `c-api` node.
build-c-lnsd: build-ffi
    T="${CARGO_TARGET_DIR:-target}"; \
    mkdir -p "$T/release"; \
    cc leviculum-ffi/examples/c/lnsd.c \
       "$T/x86_64-unknown-linux-gnu/release/libleviculum.a" \
       -I leviculum-ffi -O2 -Wall -Wextra -Werror \
       -lpthread -ldl -lm \
       -o "$T/release/c-lnsd"

# Local .deb production, mirroring .woodpecker/nightly.yml (build-amd64 +
# build-arm64). Use to build a master .deb by hand for the aarch64 soak
# node (miauhaus) without waiting for CI. The nightly pipeline is the
# source of truth; these recipes replicate its exact steps minus the
# publish/upload (that stays CI-only). Tooling: rustup targets
# x86_64/aarch64-unknown-linux-musl, cargo-deb, and for arm64
# cargo-zigbuild + ziglang. Run `just _deb-prereqs` to install them.

# Pin the build ID + DEB version once and persist them, so an amd64 +
# arm64 pair from a single `just build-deb` run carries identical
# version strings (no midnight-UTC drift between the two builds), exactly
# as nightly.yml pins them in its build-amd64 step. Formats match
# nightly: build-id  nightly.<UTCdate>-<sha7> ; deb-version
# <cargo version>~nightly.<UTCdate>.<sha7>. The short sha is the first 7
# chars of git HEAD; a dirty tree still builds, the version just reflects
# HEAD. No CI env vars are assumed; everything is computed from git+date.
_deb-stamp:
    @SHA="$(git rev-parse HEAD | cut -c1-7)"; \
    DATE="$(date -u +%Y%m%d)"; \
    VERSION="$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)"; \
    echo "nightly.${DATE}-${SHA}" >.build-id; \
    echo "${VERSION}~nightly.${DATE}.${SHA}" >.deb-version; \
    echo "[deb-stamp] build-id=$(cat .build-id) deb-version=$(cat .deb-version)"

# amd64 musl-static .deb. Binaries come from the workspace musl target,
# so the .deb is fully static and runs on Debian >= 9 / Ubuntu >= 16.04
# regardless of host glibc. `cargo clean -p leviculum-cli` is the same
# incremental-relink insurance nightly uses (a repeated build with an
# unchanged LEVICULUM_BUILD_ID can skip relinking and ship a stale
# version string). --no-strip: rust already strips debuginfo at link
# time; cargo-deb's default strip --strip-all corrupts musl-static
# binaries (SIGSEGV at startup). Output: target/debian/leviculum_*_amd64.deb
# (cargo-deb also hardlinks it under target/<triple>/debian/).
build-deb-amd64: (_require-cargo-deb) _deb-stamp
    cargo clean -p leviculum-cli
    LEVICULUM_BUILD_ID="$(cat .build-id)" cargo build --release --target x86_64-unknown-linux-musl --bin lnsd --bin lnstest --bin lncp --bin lnstatus
    cargo deb -p leviculum-cli --target x86_64-unknown-linux-musl --no-build --no-strip --deb-version "$(cat .deb-version)"
    @echo "[build-deb-amd64] produced: $(ls -1t target/debian/leviculum_*_amd64.deb | head -1)"

# arm64 musl-static .deb via cargo-zigbuild (Zig as the cross
# compiler/linker — the only way to reach aarch64-musl from an amd64 host
# without docker-in-docker or an arm64 runner). Requires cargo-zigbuild +
# ziglang on PATH; `pip install ziglang` provides a self-contained Zig
# the zigbuild wrapper finds, or install a full Zig distribution (the
# bare zig binary without its sibling lib/ fails at `zig cc` with "unable
# to find zig installation directory"). Same clean/--no-strip/version
# handling as build-deb-amd64. Output: target/debian/leviculum_*_arm64.deb
# (cargo-deb also hardlinks it under target/<triple>/debian/).
build-deb-arm64: (_require-cargo-deb) _deb-stamp
    cargo clean -p leviculum-cli
    LEVICULUM_BUILD_ID="$(cat .build-id)" cargo zigbuild --release --target aarch64-unknown-linux-musl --bin lnsd --bin lnstest --bin lncp --bin lnstatus
    cargo deb -p leviculum-cli --target aarch64-unknown-linux-musl --no-build --no-strip --deb-version "$(cat .deb-version)"
    @echo "[build-deb-arm64] produced: $(ls -1t target/debian/leviculum_*_arm64.deb | head -1)"

# Build both .debs in one go. _deb-stamp runs first (a dependency of each
# child), so both packages share one build-id/version pair.
build-deb: build-deb-amd64 build-deb-arm64

_require-cargo-deb:
    @cargo deb --version >/dev/null 2>&1 || (echo "cargo-deb not found — run: just _deb-prereqs (or cargo install cargo-deb)" && exit 1)

# Best-effort, idempotent install of the cross-build toolchain the
# build-deb* recipes need: the two musl rustup targets, cargo-deb, and
# cargo-zigbuild + ziglang for the arm64 cross-link. Safe to re-run.
_deb-prereqs:
    rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
    cargo install --locked cargo-deb
    cargo install --locked cargo-zigbuild
    @echo "[_deb-prereqs] also ensure ziglang is available for arm64:"
    @echo "    pip install ziglang   (or install a full Zig distribution on PATH)"

# Status of last runs across all tiers
status:
    @bash scripts/ci-status.sh

# For other tiers: ls ~/.local/state/leviculum-ci/ and pick a file.
# Tail the most recent Tier 1 log (live if a run is in progress).
logs:
    @bash -c 'LOG=$(ls -t ~/.local/state/leviculum-ci/tier1-*.log 2>/dev/null | head -1); \
        if [ -z "$LOG" ]; then echo "No Tier 1 log yet."; exit 1; fi; \
        echo "==> $LOG"; tail -f "$LOG"'

# Idempotent; safe to re-run after pulling.
# Install git hooks and systemd user timers for the 4-tier CI pipeline.
install-ci:
    bash scripts/install-ci.sh

# Touch-free; double-tap RESET only if the runner prompts for a crashed
# device. Details: leviculum-nrf/README.md §Build and flash.
# The firmware crate is outside the workspace (cross-compiled), so we
# invoke cargo from its own directory.
# Flash every attached T114 with the current firmware.
flash:
    cd leviculum-nrf && cargo run --release --bin t114 --features bsp-t114

# SWD firmware debugging via the RPi Debug Probe (see docs/src/firmware/probe-debugging.md)
probe *args:
    ./scripts/probe-debug.sh {{args}}

# Useful for A/B testing (one T114 on new firmware, one on old).
#   just flash-one /dev/ttyACM3
#   just flash-one /dev/leviculum-transport
# Flash a single T114 by port path or udev symlink.
flash-one PORT:
    cd leviculum-nrf && LEVICULUM_FLASH_ONLY={{PORT}} cargo run --release --bin t114 --features bsp-t114

# First flash from Meshtastic / blank firmware needs a manual RESET
# double-tap (the stock app has no 1200-baud-touch handler). Subsequent
# flashes use the touch path automatically.
# Flash every attached RAK4631 (WisMesh Pocket V2) with the current firmware.
flash-rak4631:
    cd leviculum-nrf && LEVICULUM_USB_PID=0002 LEVICULUM_BOARD_NAME=RAK4631 LEVICULUM_UF2_BOARD_ID=WisBlock-RAK4631-Board cargo run --release --bin rak4631 --features bsp-rak4631

# Flash a single RAK4631 by port path or udev symlink.
#   just flash-rak4631-one /dev/ttyACM0
#   just flash-rak4631-one /dev/leviculum-rak-transport
flash-rak4631-one PORT:
    cd leviculum-nrf && LEVICULUM_FLASH_ONLY={{PORT}} LEVICULUM_USB_PID=0002 LEVICULUM_BOARD_NAME=RAK4631 LEVICULUM_UF2_BOARD_ID=WisBlock-RAK4631-Board cargo run --release --bin rak4631 --features bsp-rak4631

# Flash with all RAK19026 baseboard peripherals enabled — the WisMesh
# Pocket V2 build. `--features rak-baseboard` aggregates the three
# baseboard features (display, gnss, battery).
flash-rak4631-pocket:
    cd leviculum-nrf && LEVICULUM_USB_PID=0002 LEVICULUM_BOARD_NAME=RAK4631 LEVICULUM_UF2_BOARD_ID=WisBlock-RAK4631-Board cargo run --release --bin rak4631 --features bsp-rak4631,rak-baseboard

# Trigger Adafruit-UF2-bootloader on a stock-Meshtastic WisMesh Pocket V2.
# Stock Meshtastic has no 1200-bps-touch handler and the device has no
# externally accessible RESET pin, so the firmware-side admin command is the
# only software-only DFU entry. After our firmware lands, just-flash-rak4631
# uses the touch handler from src/usb.rs and this recipe is no longer needed.
# Requires the meshtastic CLI on PATH (pip install meshtastic).
# Usage: just dfu-rak4631 /dev/ttyACM0
dfu-rak4631 PORT:
    meshtastic --port {{PORT}} --enter-dfu

# RNode (LilyGO T-Beam, ESP32 + SX1276) flashing with Mark's firmware.
# Run on the host the RNodes are attached to. The ESP32 has a mask-ROM
# download bootloader and cannot be bricked: a failed flash is always
# recoverable by re-running flash-rnode. This is unlike the nRF52 LNodes
# (T114, RAK4631), where a bad external image leaves the device USB-dark.
#
# Run flash-rnode-setup once first. It pip-installs esptool into a
# gitignored repo-local venv (.rnode-tools/): the Debian esptool package is
# dfsg-stripped of its flasher stubs and fails on large flash reads/writes,
# so a real esptool is needed. rnodeconf is the repo's vendored copy.
# Mark's autoinstall is interactive (product menu); instead we read Mark's
# signed firmware images off a known-good RNode once (flash-rnode-extract,
# into the gitignored .rnode-fw/), then write them back. The write covers
# only the firmware regions, not the NVS/EEPROM partition, so the device
# signature and provisioning are preserved (verified: a T-Beam stayed
# "Validated, Local signature" across a full reflash).

reference_reticulum := justfile_directory() / "reference" / "Reticulum"
rnodeconf := "PYTHONPATH=" + reference_reticulum + " python3 " + reference_reticulum / "RNS" / "Utilities" / "rnodeconf.py"
esptool := justfile_directory() / ".rnode-tools" / "bin" / "esptool.py"
rnode_fw := justfile_directory() / ".rnode-fw"

# One-time setup: a repo-local venv with a working esptool (the Debian
# package cannot read/write large flash regions, its stubs are dfsg-stripped).
flash-rnode-setup:
    python3 -m venv {{justfile_directory()}}/.rnode-tools
    {{justfile_directory()}}/.rnode-tools/bin/pip install --quiet 'esptool<5'

# Read-only device info: connectivity, firmware version, signature.
#   just flash-rnode-info /dev/ttyACM6
flash-rnode-info PORT:
    {{rnodeconf}} --info {{PORT}}

# Back up an RNode EEPROM (board model, signature, provisioning) before any
# flash. Writes ~/.config/rnodeconf/eeprom<timestamp>.eeprom.
flash-rnode-backup PORT:
    {{rnodeconf}} --eeprom-backup {{PORT}}

# Extract Mark's signed firmware images off a known-good, signature-validated
# RNode into .rnode-fw/ (gitignored). Run ONCE against a trusted device; the
# images then serve as the flash source for flash-rnode.
#   just flash-rnode-extract /dev/ttyACM6
flash-rnode-extract PORT:
    mkdir -p {{rnode_fw}}
    {{esptool}} --chip esp32 --port {{PORT}} --baud 921600 read_flash 0x1000 0x4650 {{rnode_fw}}/bootloader.bin
    {{esptool}} --chip esp32 --port {{PORT}} --baud 921600 read_flash 0x8000 0xc00 {{rnode_fw}}/partitions.bin
    {{esptool}} --chip esp32 --port {{PORT}} --baud 921600 read_flash 0xe000 0x2000 {{rnode_fw}}/boot_app0.bin
    {{esptool}} --chip esp32 --port {{PORT}} --baud 921600 read_flash 0x10000 0x200000 {{rnode_fw}}/app.bin
    {{esptool}} --chip esp32 --port {{PORT}} --baud 921600 read_flash 0x210000 0x1f0000 {{rnode_fw}}/console.bin

# Flash a T-Beam RNode with the extracted Mark firmware. Deterministic and
# non-interactive. Preserves the EEPROM provisioning. Requires
# flash-rnode-extract to have populated .rnode-fw/ first.
#   just flash-rnode /dev/ttyACM6
flash-rnode PORT:
    {{esptool}} --chip esp32 --port {{PORT}} --baud 921600 --before default_reset --after hard_reset write_flash --flash_mode dio --flash_freq 80m --flash_size detect 0x1000 {{rnode_fw}}/bootloader.bin 0x8000 {{rnode_fw}}/partitions.bin 0xe000 {{rnode_fw}}/boot_app0.bin 0x10000 {{rnode_fw}}/app.bin 0x210000 {{rnode_fw}}/console.bin
