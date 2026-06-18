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
    cargo test -p reticulum-std --test mvr -- --test-threads=1

# Promote the most-recent auto-bug bundle to the coder bridge.
# Run after a tier RED has emitted to /tmp/leviculum/auto-bug/instructions.md
# and you want the coder to pick it up.  See scripts/_emit-auto-bug-bundle.sh.
# The source bundle is left in place so a re-promotion (e.g. after a
# stomped bridge) works without re-triggering the failing tier.
spawn-coder:
    @if [ ! -s /tmp/leviculum/auto-bug/instructions.md ]; then \
        echo "ERROR: no auto-bug bundle at /tmp/leviculum/auto-bug/instructions.md"; \
        echo "       Either no recent tier RED, or the file was removed by hand."; \
        exit 1; \
    fi
    cp /tmp/leviculum/auto-bug/instructions.md /tmp/leviculum/instructions.md
    @echo "[spawn-coder] bundle promoted to bridge: /tmp/leviculum/instructions.md"
    @echo "[spawn-coder] source: /tmp/leviculum/auto-bug/instructions.md (left in place for re-promotion)"

# Lint the embedded firmware workspace. reticulum-nrf is its OWN cargo
# workspace — `--workspace` invocations in the repo root never reach it,
# which let 11 clippy findings accumulate unseen (audit 2026-06-11).
# Both BSP feature sets; clippy subsumes `cargo check` diagnostics.
# First run compiles the embedded deps into reticulum-nrf/target
# (minutes); warm runs are seconds.
lint-nrf:
    cd reticulum-nrf && cargo clippy --features bsp-rak4631,rak-baseboard -- -D warnings
    cd reticulum-nrf && cargo clippy --features bsp-t114 -- -D warnings

# Rustdoc gate: broken intra-doc links fail instead of warning.
doc-gate:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Tracing-shim gate (PR #57): reticulum-core must pass the SAME suite with
# tracing OFF as with it on. The `tracing` feature is default-on; with it
# off the level macros become no-ops via the `crate::tracing` shim in
# lib.rs. Running the full suite in that config proves the shim changed no
# core logic (a bare `tracing::x!` that slipped past the shim would either
# fail to compile here or, worse, only on M0 — see m0-build-gate).
core-no-tracing:
    cargo test -p reticulum-core --no-default-features

# Cortex-M0 gate (PR #57): reticulum-core must cross-compile for thumbv6m
# (atomic-less MCU, e.g. rp2040) with tracing off. tracing-core's CAS-based
# callsite registry does not compile there, so the default build FAILS on
# M0; --no-default-features must succeed. Keeps M0 support from rotting.
m0-build-gate:
    rustup target add thumbv6m-none-eabi
    cargo build -p reticulum-core --target thumbv6m-none-eabi --no-default-features

# Tier 0 (~3 min, runs on every git push): fmt + clippy (host + nrf)
# + rustdoc gate + tracing-shim + M0 gates + workspace lib tests.
fast: mvr lint-nrf doc-gate core-no-tracing m0-build-gate
    cargo fmt --all -- --check
    cargo clippy --workspace -- -D warnings
    cargo test --workspace --lib --exclude reticulum-integ

# First run after a fresh CARGO_TARGET_DIR: 20-40 min. Runs in background
# after every commit via the post-commit hook.
# Tier 1 (~15 min): Tier 0 + core/tests + ffi (incl. C-program + Python interop)
# + proxy + rnsd_interop.
standard: fast test-ffi
    cargo test -p reticulum-core --tests
    cargo test -p reticulum-proxy
    cargo test -p reticulum-std --test rnsd_interop
    cargo test -p reticulum-std --test event_log_subscriber -- --test-threads=1
    cargo test -p reticulum-std --test event_log_multiprocess

# Build the production binaries the integ runner mounts into Docker
# containers. Explicit per-bin list avoids `--workspace --bins` which
# would also try to build reticulum-nrf firmware on the host. Runs on
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
    find reticulum-cli/src reticulum-proxy/src -name '*.rs' -exec touch {} +
    cargo build --release --bin lnsd --bin lns --bin lncp --bin lora-proxy

# Default cargo test runs non-ignored tests and skips ignored ones.
# Docker tests (#[serial(docker)]) run; LoRa tests (#[ignore] #[serial(lora)])
# skip automatically. --test-threads=1 is required even though serial_test
# enforces per-group serialization, because the embedded #[serial(docker)]
# groups can still overlap resource usage with unit tests in the same
# binary on a multi-CPU harness.
# Tier 2 (~30-90 min, on demand: `systemctl --user start
# leviculum-ci-tier2.service`): Tier 1 + Docker integ suite.
extensive: standard build-integ-bins
    cargo test -p reticulum-integ -- --test-threads=1

# --include-ignored adds the LoRa hardware tests on top of Tier 2.
# Tier 3 (~2-6h, 02:00 nightly): Tier 2 + LoRa hardware tests.
nightly: extensive
    cargo test -p reticulum-integ -- --include-ignored --test-threads=1

# Build reticulum-ffi as a real glibc-dynamic cdylib + staticlib for
# C-API consumers ("apt install libreticulum-dev" ergonomics). This
# deliberately overrides the workspace musl default — see the comment
# in .cargo/config.toml. cbindgen regenerates reticulum.h as a side
# effect of the build.rs.
# Comprehensive C API test suite on the glibc target: the Rust unit,
# integration, and Python-interop suites plus the C acceptance programs linked
# against the real cdylib. Builds the debug glibc cdylib first, because once
# the crate has an rlib `cargo test` no longer builds the cdylib, and the
# C-program harness needs libleviculum.so to link and run. The Python interop
# tests skip cleanly if Python RNS is unavailable.
test-ffi:
    cargo build -p reticulum-ffi --target x86_64-unknown-linux-gnu
    cargo test-ffi

# Memory- and race-check the C API under sanitizers and Miri. On demand, not in
# the standard tiers: it needs the nightly toolchain
# (`rustup toolchain install nightly --component rust-src miri`) and is heavy,
# since -Zbuild-std rebuilds std and every dependency with instrumentation
# (several GB of target per sanitizer). AddressSanitizer (+ LeakSanitizer) and
# ThreadSanitizer run the in-process two-node integration suite, covering the
# handle lifecycle, the eventfd bridge, and the two-runtime threading. Miri
# checks the pure unsafe marshalling paths (buffer read(2), handle boxing,
# char** aspects); it cannot run tokio or real I/O, so node/network tests are
# excluded by filtering to identity/hex/destination.
sanitize-ffi:
    RUSTFLAGS="-Zsanitizer=address" cargo +nightly test -p reticulum-ffi -Zbuild-std --target x86_64-unknown-linux-gnu --test ffi_unit --test ffi_integration -- --test-threads=1
    RUSTFLAGS="-Zsanitizer=thread" TSAN_OPTIONS="halt_on_error=0" cargo +nightly test -p reticulum-ffi -Zbuild-std --target x86_64-unknown-linux-gnu --test ffi_integration -- --test-threads=1
    MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test -p reticulum-ffi --test ffi_unit identity
    MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test -p reticulum-ffi --test ffi_unit hex
    MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test -p reticulum-ffi --test ffi_unit destination

build-ffi:
    cargo build-ffi

# Same for ARM64. Requires `sudo apt install gcc-aarch64-linux-gnu` and
# `rustup target add aarch64-unknown-linux-gnu` on the build host.
build-ffi-arm64:
    cargo build-ffi-arm64

# Build the leviculum .deb package for amd64. Binaries come from the
# workspace musl target, so the .deb is fully static and runs on
# Debian ≥ 9 / Ubuntu ≥ 16.04 regardless of host glibc. Requires
# `cargo install cargo-deb`. Output: target/debian/leviculum_*.deb.
build-deb: (_require-cargo-deb)
    cargo build --release --bin lnsd --bin lns --bin lncp
    cargo deb -p reticulum-cli --target x86_64-unknown-linux-musl --no-build

# ARM64 .deb via cargo-zigbuild (Zig as cross-linker). Requires:
#   cargo install cargo-zigbuild
#   rustup target add aarch64-unknown-linux-musl
#   Zig installed as a full distribution (binary + lib/ directory),
#   e.g.  tar xf zig-x86_64-linux-<ver>.tar.xz -C /opt/  and
#   ln -sf /opt/zig-.../zig /usr/local/bin/zig. A bare zig binary
#   without its sibling lib/ fails at `zig cc` with "unable to find
#   zig installation directory".
build-deb-arm64: (_require-cargo-deb)
    cargo zigbuild --release --target aarch64-unknown-linux-musl --bin lnsd --bin lns --bin lncp
    cargo deb -p reticulum-cli --target aarch64-unknown-linux-musl --no-build

_require-cargo-deb:
    @cargo deb --version >/dev/null 2>&1 || (echo "cargo-deb not found — run: cargo install cargo-deb" && exit 1)

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
# device. Details: reticulum-nrf/README.md §Build and flash.
# The firmware crate is outside the workspace (cross-compiled), so we
# invoke cargo from its own directory.
# Flash every attached T114 with the current firmware.
flash:
    cd reticulum-nrf && cargo run --release --bin t114 --features bsp-t114

# Useful for A/B testing (one T114 on new firmware, one on old).
#   just flash-one /dev/ttyACM3
#   just flash-one /dev/leviculum-transport
# Flash a single T114 by port path or udev symlink.
flash-one PORT:
    cd reticulum-nrf && LEVICULUM_FLASH_ONLY={{PORT}} cargo run --release --bin t114 --features bsp-t114

# First flash from Meshtastic / blank firmware needs a manual RESET
# double-tap (the stock app has no 1200-baud-touch handler). Subsequent
# flashes use the touch path automatically.
# Flash every attached RAK4631 (WisMesh Pocket V2) with the current firmware.
flash-rak4631:
    cd reticulum-nrf && LEVICULUM_USB_PID=0002 LEVICULUM_BOARD_NAME=RAK4631 LEVICULUM_UF2_BOARD_ID=WisBlock-RAK4631-Board cargo run --release --bin rak4631 --features bsp-rak4631

# Flash a single RAK4631 by port path or udev symlink.
#   just flash-rak4631-one /dev/ttyACM0
#   just flash-rak4631-one /dev/leviculum-rak-transport
flash-rak4631-one PORT:
    cd reticulum-nrf && LEVICULUM_FLASH_ONLY={{PORT}} LEVICULUM_USB_PID=0002 LEVICULUM_BOARD_NAME=RAK4631 LEVICULUM_UF2_BOARD_ID=WisBlock-RAK4631-Board cargo run --release --bin rak4631 --features bsp-rak4631

# Flash with all RAK19026 baseboard peripherals enabled — the WisMesh
# Pocket V2 build. `--features rak-baseboard` aggregates the three
# baseboard features (display, gnss, battery).
flash-rak4631-pocket:
    cd reticulum-nrf && LEVICULUM_USB_PID=0002 LEVICULUM_BOARD_NAME=RAK4631 LEVICULUM_UF2_BOARD_ID=WisBlock-RAK4631-Board cargo run --release --bin rak4631 --features bsp-rak4631,rak-baseboard

# Trigger Adafruit-UF2-bootloader on a stock-Meshtastic WisMesh Pocket V2.
# Stock Meshtastic has no 1200-bps-touch handler and the device has no
# externally accessible RESET pin, so the firmware-side admin command is the
# only software-only DFU entry. After our firmware lands, just-flash-rak4631
# uses the touch handler from src/usb.rs and this recipe is no longer needed.
# Requires meshtastic CLI: /home/lew/pythonenvironment/bin/pip install meshtastic
# Usage: just dfu-rak4631 /dev/ttyACM0
dfu-rak4631 PORT:
    /home/lew/pythonenvironment/bin/meshtastic --port {{PORT}} --enter-dfu
