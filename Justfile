# The scenario suites live in the sibling `periculum` checkout, not in this
# workspace: `just extensive` and `just nightly` drive the `periculum` binary
# over its three corpora. Override the checkout with PERICULUM_ROOT or the
# binary with PERICULUM_BIN.

# Guarantee B step 1 (docs/src/concepts/checks-and-citations.md): a gate that
# runs tests records WHICH tests it executed, parsed out of the run's own
# output rather than out of `cargo test --list` — a list records intent, and a
# by-name selector that matches nothing runs zero tests and exits 0.
# `{{manifest}} <name> -- <command>` passes the command's output and exit
# status straight through and writes the manifest beside the other CI run
# state, under ~/.local/state/leviculum-ci/test-manifests/. Step 2 (not built)
# reads the union of those manifests and reports every test in none of them.
#
# It is also what keeps a gate from hanging: it waits for its child to EXIT
# rather than for the output pipe to close, kills the child's process group
# afterwards so a leaked daemon cannot hold the gate open, gives up at 1800 s
# with a named failure, and prints what it had to kill. `just standard` sat for
# two hours on 2026-08-07 for want of the first of those. Per-gate budget:
# `{{ manifest }} <name> --timeout <seconds> -- <command>`; 0 disables.
manifest := "python3 scripts/run-with-manifest.py --gate"

# Minimum-viable-reproduction tier — discipline tier, not size tier.
# Each test < 5 s, deterministic, single named failure mode.  See
# Codeberg #39 for design intent.  --test-threads=1 avoids
# port/resource contention between concurrent integration-style tests
# in the same binary.  Depends on build-integ-bins because the mvr
# tests spawn the release lnsd/lncp binaries directly.
[doc('Minimum-viable-reproduction tier: deterministic, under 5 s each')]
mvr: build-integ-bins
    {{manifest}} mvr -- cargo test -p leviculum-std --test mvr -- --test-threads=1

# Run after a tier RED has emitted to $BRIDGE/auto-bug/instructions.md
# and you want the coder to pick it up.  See scripts/_emit-auto-bug-bundle.sh.
# BRIDGE defaults to ~/.local/state/leviculum (override LEVICULUM_BRIDGE).
# The source bundle is left in place so a re-promotion (e.g. after a
# stomped bridge) works without re-triggering the failing tier.
# One shell block so BRIDGE persists across the recipe lines.
[doc('Promote the most-recent auto-bug bundle to the coder bridge')]
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

# Build the lnflash tarball a stranger can unpack and run:
#   tar xzf lnflash-<stamp>.tar.gz && cd lnflash-<stamp> && sudo ./lnflash
# Contains the musl-static binary, our T114 firmware, Nordic's SoftDevice
# with Nordic's own licence file beside it, a manifest with checksums, and a
# user-facing README. Everything comes from this checkout — a bundle built
# out of a Meshtastic checkout is the hidden dependency our clone-and-deploy
# policy forbids. Cross-compiles the firmware, so the first run takes
# minutes; SKIP_FIRMWARE=1 reuses an existing ELF while iterating on the
# bundle itself. Output under target/lnflash/.
# `just lnflash-bundle a,b` (positional, as every just knob is) builds each
# board image with those leviculum-nrf features on top of its bsp-* set, for
# a rig A/B arm (#438: `just lnflash-bundle subslot-offset`); the bundle is
# then named lnflash-<stamp>+a+b and its manifest lists the features. Empty, the
# bundle is byte for byte the default one.
[doc('Build the lnflash tarball a stranger can unpack and run')]
lnflash-bundle nrf_features="":
    NRF_FEATURES='{{nrf_features}}' bash scripts/lnflash-bundle.sh

# Lint the embedded firmware workspace. leviculum-nrf is its OWN cargo
# workspace — `--workspace` invocations in the repo root never reach it,
# which let 11 clippy findings accumulate unseen (audit 2026-06-11).
# All three BSP feature sets; clippy subsumes `cargo check` diagnostics.
# Note that each of them compiles EVERY bin, not just its own: a board's
# binary that only another board's feature set can compile is still
# linted, which is what keeps the peripherals `bsp-solarnode` does not
# switch on from rotting (Codeberg #233).
# First run compiles the embedded deps into leviculum-nrf/target
# (minutes); warm runs are seconds.
[doc('Lint, doc and host-test the embedded firmware workspace')]
lint-nrf:
    cd leviculum-nrf && cargo clippy --features bsp-rak4631,rak-baseboard -- -D warnings
    cd leviculum-nrf && cargo clippy --features bsp-t114 -- -D warnings
    cd leviculum-nrf && cargo clippy --features bsp-solarnode -- -D warnings
    # The T1000-E is the one board on the LR11xx driver, so its feature set
    # compiles its own binary and no other (`radio-sx1262` in Cargo.toml).
    cd leviculum-nrf && cargo clippy --features bsp-t1000e -- -D warnings
    # The QSPI self-test instrument sits behind its own feature (Cargo.toml,
    # `qspi-selftest`, for why), so the line above no longer builds it.
    cd leviculum-nrf && cargo clippy --features bsp-solarnode,qspi-selftest --bin qspi-selftest -- -D warnings
    # The #438 sub-slot offset is an A/B arm a bundle can carry
    # (`just lnflash-bundle subslot-offset`), so it has to build
    # on every BSP, not only on the one somebody last flashed it to.
    cd leviculum-nrf && cargo clippy --features bsp-rak4631,rak-baseboard,subslot-offset -- -D warnings
    cd leviculum-nrf && cargo clippy --features bsp-t114,subslot-offset -- -D warnings
    cd leviculum-nrf && cargo clippy --features bsp-solarnode,subslot-offset -- -D warnings
    cd leviculum-nrf && cargo clippy --features bsp-t1000e,subslot-offset -- -D warnings
    # Every other member of the leviculum-nrf workspace is a pure,
    # host-testable crate: clippy + tests run on the host triple, because the
    # workspace's own `.cargo/config.toml` defaults `build.target` to
    # thumbv7em. The set is `--workspace --exclude leviculum-nrf` rather than a
    # written-out `-p` list, so a seam crate is gated by being a member and
    # cannot be added without its tests running. The list it replaced had to be
    # kept in three places (the `members` array, these two lines) and the prose
    # copy above them had already lost `leviculum-upload-proof`; a crate missing
    # from the `-p` lines is silently never tested, which a positive control
    # confirmed — a deliberately red test in a fresh member failed this form and
    # passed the list one. The firmware crate is the one member that has to be
    # excluded: it does not compile for a host triple at any feature set.
    # Policy and inventory: docs/src/concepts/firmware-host-test-seam.md.
    #
    # `--all-targets` here, unlike the three embedded feature-set lines above:
    # these crates' whole value is their host test suites, and without the flag
    # clippy lints their libs only while the `cargo test` line below merely
    # COMPILES the test code. A lint that fires solely in a test was therefore
    # invisible to every run of this recipe — the same gap the workspace line in
    # `fast` closed in e27a15e. The embedded lines stay narrow: `--all-targets`
    # there would pull in test/bench harnesses that do not link for thumbv7em.
    cd leviculum-nrf && cargo clippy --workspace --exclude leviculum-nrf --target $(rustc -vV | sed -n 's/host: //p') --all-targets -- -D warnings
    cd leviculum-nrf && cargo test --workspace --exclude leviculum-nrf --target $(rustc -vV | sed -n 's/host: //p')
    #
    # Rustdoc, for the same reason the clippy lines above exist: the root
    # `doc-gate` runs `cargo doc --workspace`, which by construction stops at
    # this workspace's boundary. Until 2026-09-28 `scripts/doc-touched.py`
    # excused leviculum-nrf on the stated grounds that `lint-nrf` carried its
    # own rustdoc gate -- a claim with no line behind it, so `just doc-touched`
    # was green on a batch it could not see. The bill when 362 finally
    # measured it: 18 broken intra-doc links across the host members (cleared
    # in d6a4b620) and 50 across 11 modules of the firmware crate (Codeberg
    # #367). `build-esp32` has carried the line since it was written.
    #
    # Host members first -- all 33, one `--target` host run, 1.7 s warm.
    cd leviculum-nrf && RUSTDOCFLAGS="-D warnings" cargo doc --workspace --exclude leviculum-nrf --no-deps --target $(rustc -vV | sed -n 's/host: //p')
    # Then the firmware crate once per BUNDLE feature set, 2.3 s each warm.
    # Not one run for all three: a feature set decides which modules exist at
    # all (`st7789` is bsp-t114 only), so a link inside a module the t114 set
    # does not compile is gated by nothing unless the other sets doc too --
    # the same argument as the three clippy lines. `--no-deps` because the
    # thumbv7em dependency tree is not ours to document.
    cd leviculum-nrf && RUSTDOCFLAGS="-D warnings" cargo doc -p leviculum-nrf --no-deps --features bsp-rak4631,rak-baseboard
    cd leviculum-nrf && RUSTDOCFLAGS="-D warnings" cargo doc -p leviculum-nrf --no-deps --features bsp-t114
    cd leviculum-nrf && RUSTDOCFLAGS="-D warnings" cargo doc -p leviculum-nrf --no-deps --features bsp-solarnode
    cd leviculum-nrf && RUSTDOCFLAGS="-D warnings" cargo doc -p leviculum-nrf --no-deps --features bsp-t1000e

[doc('Lint, test and document the LR11xx driver')]
lint-lr11xx:
    cd leviculum-lr11xx && cargo clippy --target $(rustc -vV | sed -n 's/host: //p') --all-targets -- -D warnings
    cd leviculum-lr11xx && cargo fmt --all -- --check
    cd leviculum-lr11xx && cargo test --target $(rustc -vV | sed -n 's/host: //p')
    cd leviculum-lr11xx && RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --target $(rustc -vV | sed -n 's/host: //p')
    cd leviculum-lr11xx && cargo check --target thumbv7em-none-eabihf

# Not in `fast`, for build-esp32's reason: the Xtensa compiler fork is optional per host.
[doc('Check the LR11xx driver for the ESP32-S3 (needs the esp toolchain)')]
check-lr11xx-esp32s3:
    cd leviculum-lr11xx && bash -c '. ~/export-esp.sh && cargo +esp check --target xtensa-esp32s3-none-elf -Z build-std=core,alloc'

# Build the ESP32-class firmware (Heltec WiFi LoRa 32 V4) and package the
# flash image.
#
# NOT wired into `just fast` / `just standard`, and it must stay that way:
# the Xtensa target does not exist in stock rustc, only in the esp-rs
# compiler fork that `scripts/install-ci.sh` installs through espup. Making
# a host gate depend on it would mean every machine and the pipeline need a
# second, 1.8 GiB toolchain before `cargo test` can run. The reviewer runs
# this recipe deliberately, the way the hardware gates are run.
#
# `~/export-esp.sh` is what espup writes; it puts xtensa-esp-elf-gcc (the
# linker) and the Xtensa clang on PATH. leviculum-esp is its own cargo
# workspace with its own rust-toolchain.toml, so the `esp` channel is
# selected by entering the directory, not by a `+esp` on every line.
[doc('Build and package the ESP32 firmware image (Heltec V4)')]
build-esp32:
    #!/usr/bin/env bash
    set -euo pipefail
    source ~/export-esp.sh
    # espflash and espup are `cargo install`ed (scripts/install-ci.sh), so
    # they live in ~/.cargo/bin, which a non-login shell may not have.
    export PATH="$HOME/.cargo/bin:$PATH"
    cd leviculum-esp
    cargo fmt --check
    cargo clippy --release -- -D warnings
    # The root `doc-gate` runs `cargo doc --workspace`, which by
    # construction never reaches an excluded workspace. Broken intra-doc
    # links here would therefore be caught by nothing, so the gate is
    # repeated where it applies.
    RUSTDOCFLAGS="-D warnings" cargo doc --release --no-deps
    cargo build --release
    espflash save-image --chip esp32s3 --flash-size 16mb \
        target/xtensa-esp32s3-none-elf/release/heltec_v4 \
        target/xtensa-esp32s3-none-elf/release/heltec_v4.bin
    ls -l target/xtensa-esp32s3-none-elf/release/heltec_v4.bin

# Stack-frame gate for the firmware. The T114 stack grows down into the
# SoftDevice RAM floor, so one oversized frame eats the whole margin and
# surfaces as an SD internal assertion rather than a clean fault. A 94 KB
# `main` frame (a by-value `NodeCore` materialised twice) did exactly that
# and left ~13 KB of margin. Reads the `sub sp` immediates out of the linked
# ELF, so it measures the shipped binary.
[doc('Check the firmware stack frames against the SoftDevice RAM floor')]
nrf-stack-frames:
    bash scripts/check-nrf-stack-frames.sh

# Record-store gap gate (Codeberg #384). The store's 64 KiB region sits directly
# above the firmware image, inside the window a UF2 may write, so an image that
# grew into it would take the board's message store with it on the next flash.
# The linker already refuses the three ways memory.x can say that wrong (see the
# ASSERTs there, and the script's header for what it adds on top): what this
# gate contributes is the remaining gap as a NUMBER for both bins on every run,
# measured on the image as flashed and against the bounds the firmware itself
# mounts. A link error is a cliff with no warning track.
# Reads the linked ELFs nrf-stack-frames already built, so it costs seconds.
[doc('Report the gap between the firmware image and the record store')]
nrf-store-gap:
    bash scripts/check-nrf-store-gap.sh

# Flash headroom gate (order 1300, #359). The store gap above is the same edge
# measured for a different reason and with no floor: on 2026-11-02 the Pocket
# image did not link at all, 56 B short, and on 2026-11-01 it had 32 B left,
# so a 768 B diagnostic could not reach the one board it was written for.
# This links all four images, prints `FLASH_HEADROOM board= used= free=` for
# each, and is red below 8 KiB free (the reason for the number is in the
# script's header). The selftest feeds it fake images, oversized ones among
# them, through the same measurement. Seconds in `fast`, because
# nrf-stack-frames linked the images first; standing alone after a firmware
# change it relinks all four.
[doc('Link every firmware image and refuse one with under 8 KiB of flash left')]
nrf-flash-budget:
    bash scripts/check-nrf-flash-budget.sh --selftest
    bash scripts/check-nrf-flash-budget.sh

# BLE event-buffer gate. nrf-softdevice sizes the `sd_ble_evt_get` buffer from
# a cargo feature, defaults to 128 bytes when none is picked, and panics rather
# than truncating when an event does not fit. Our characteristics are 251 bytes
# wide, so on the default every LNode reset within seconds of a real Android
# peer connecting (Codeberg #354). Losing the feature again is invisible: the
# firmware still builds, and the LNode-to-LNode bench negotiates an MTU small
# enough to stay under 128. Asserted against cargo's resolved feature graph.
[doc('Check the BLE event buffer is sized for our characteristics')]
nrf-evt-max-size:
    bash scripts/check-nrf-evt-max-size.sh

# GAP device-name pointer gate. `ble_gap_cfg_device_name_t` under
# BLE_GATTS_VLOC_STACK takes a flash pointer or NULL and nothing else; a RAM
# pointer earns NRF_ERROR_INVALID_ADDR from `sd_ble_cfg_set`, which
# nrf-softdevice turns into a panic inside `Softdevice::enable` — reached from
# `main` before its first await, so the USB task never runs and the board
# boot-loops without ever enumerating. `e52dba1` shipped exactly that and no
# other gate saw it: the name builder is pure and host-tested, both BSPs build,
# clippy is clean. Text check by necessity (the bad value is a runtime
# address); carries its own positive control.
[doc('Check the GAP device name is a flash pointer, not a RAM one')]
nrf-gap-device-name:
    bash scripts/check-nrf-gap-device-name.sh

# Board pin-map gate. The two pin greps that existed before it were both
# internal-consistency checks, and `e5d62b95` passed them with the T114's QSPI
# IO2/IO3 named P1.00/P1.01 where the part has WP#/HOLD# on P0.07/P0.05: a map
# that is consistently wrong is consistent, so nothing that reads only our own
# tree can see it. Only the reference can. So this compares the QSPI and LoRa
# pins against the Meshtastic variant headers, and separately against the
# `p.P0_07` arguments the bins actually pass, which are not the aliases. (The
# T114's `id=00:00:00` was once blamed on that wrong IO3; it cannot be — the
# JEDEC read is single-line and never touches IO2/IO3. See qspi.rs §Deep power
# down.) Numbers and scope in leviculum-nrf/reference-pins.toml; the upstream
# half needs a Meshtastic checkout ($MESHTASTIC_TREE), names the revision it
# read, and says so when there is none.
[doc('Check the firmware pin maps against the reference variant headers')]
nrf-board-pins:
    bash scripts/check-nrf-board-pins.sh

# SoftDevice guard for the flash runner. Our image is linked at 0x27000 and a
# factory board still carrying S140 6.1.1 forwards to 0x26000, so writing to
# one soft-bricks it (docs/src/concepts/lnode-flashing.md). The runner refuses
# that write; this drives the refusal against fixture INFO_UF2.TXT files, so
# the logic is covered without a board. That a real 6.1.1 board is refused
# stays a rig check.
[doc('Check that the flash runner refuses a wrong-SoftDevice board')]
nrf-sd-guard:
    bash leviculum-nrf/tools/test-softdevice-guard.sh

# Volume selection for the flash runner. The guard above decides WHETHER to
# write; this decides WHERE. It used to take the first UF2 volume in the search
# path, so one foreign board parked in its bootloader shadowed every other board
# and left its mount behind to keep doing so (Codeberg #341). Driven against
# fixture volume directories with stubbed mount/umount, so no board and no sudo.
[doc('Check that the flash runner picks the right UF2 volume')]
nrf-uf2-volumes:
    bash leviculum-nrf/tools/test-uf2-volumes.sh

# Attribution for the flash runner. The guard decides WHETHER to write, the
# volume selection decides WHERE, and this decides WHO GOT IT. A UF2 volume
# carries no board serial, so the runner used to pair it with a board out of
# its own enumeration and report that one — with two T114s attached, one in
# DFU and one running, it wrote the image to the first and named the second
# (Codeberg #343, measured twice, once in each direction). Driven against
# stubbed boards, each with a firmware stamp it reports when read.
[doc('Check that the flash runner names the board it wrote')]
nrf-fw-readback:
    bash leviculum-nrf/tools/test-fw-readback.sh

# The ESP32-side flashing recipes: chip and offsets follow the board, not a
# constant (2026-09-17). `--chip esp32` and a bootloader at 0x1000 were
# hardcoded into every flash-rnode-* recipe, which is right for the T-Beams
# and wrong for the Heltec V4 — an ESP32-S3, bootloader at 0x0 — so the one
# board that needed restoring was the one board the recipes could not touch.
# Runs the recipes in dry-run and asserts the composed esptool command line;
# no port is opened and nothing is written. ~0.5 s. Its --self-test puts the
# S3 bootloader back at 0x1000 in a throwaway copy and requires the
# assertions to go red.
[doc('Check that the RNode flash offsets follow the board, not a constant')]
rnode-chip-offsets:
    @bash scripts/check-rnode-chip-offsets.sh
    @bash scripts/check-rnode-chip-offsets.sh --self-test

# Static analysis for the flash-runner scripts (Codeberg #345). They have
# carried `# shellcheck` directives since they were written, so somebody once
# ran it — but nothing ever ran it again, and an SC2034 and an SC2015 sat in
# the runner unnoticed until #341 and #343 happened to remove them.
#
# scripts/flash-lnodes-from-head.sh is in the same list because it sources
# leviculum-nrf/tools/fw-readback.sh: it is part of the same source graph, and
# leaving it out would gate the module while its only non-test caller went
# unchecked.
#
# A gate script that nothing lints is the same hole one layer up. Every
# `scripts/check-*.sh` a Justfile gate invokes is in the list, checked
# 2026-09-25 against the recipes; check-nrf-stack-frames.sh was the one the #46
# pass tripped over, and it brought four more in with it.
#
# scripts/install-ci.sh joined on 2026-09-27, the last shell script no gate
# linted. It was held out because six single-item `for cmd in <tool>; do` loops
# around its optional-dependency notes raised SC2043. Those are plain `if`
# blocks now -- the shape that same file already used for uhubctl -- so the
# entry carries no `disable` directive to stay green.
#
# Must run from the repo root: the `source=` directives in these scripts name
# repo-relative paths, which is what lets shellcheck resolve a `.` through
# $SCRIPT_DIR. -x is what the ticket asks for and covers a future `source`
# line whose directive somebody forgets.
[doc('Shellcheck the flash-runner and CI shell scripts')]
nrf-shellcheck:
    shellcheck -x leviculum-nrf/tools/*.sh scripts/flash-lnodes-from-head.sh \
        scripts/lnode-bootloader.sh \
        scripts/debug-witness.sh scripts/test-debug-witness.sh \
        scripts/rig-cell.sh scripts/test-rig-cell.sh \
        scripts/device-watchdog.sh scripts/test-device-watchdog.sh \
        scripts/run-tier3-hw.sh scripts/tier3-hw-selftest.sh \
        scripts/run-tier2.sh scripts/run-tier3.sh \
        scripts/lock-contention.sh scripts/test-lock-contention.sh \
        scripts/tmp-leftovers.sh scripts/test-tmp-leftovers.sh \
        scripts/check-nrf-evt-max-size.sh \
        scripts/check-nrf-store-gap.sh scripts/check-nrf-flash-budget.sh \
        scripts/nrf-elf.sh scripts/nrf-rustflags.sh scripts/build-m9-image.sh scripts/check-m9-reproducible.sh scripts/check-m9-bench-off.sh scripts/m9-bench-home.sh \
        scripts/nrf-flash.sh scripts/check-nrf-reproducible.sh \
        scripts/check-nrf-board-pins.sh \
        scripts/check-nrf-gap-device-name.sh \
        scripts/check-nrf-stack-frames.sh \
        scripts/check-submodule-pins.sh \
        scripts/check-processor-compile-fail.sh \
        scripts/check-commit-trailers.sh \
        scripts/check-integ-bin-list.sh \
        scripts/lnode-panic-query.sh scripts/lnode-stack-reset.sh \
        scripts/check-prepush-guard.sh scripts/cargo-target-dir.sh \
        scripts/push-clean.sh scripts/check-ci-pipeline.sh scripts/ci-gate.sh \
        scripts/ci-gate-integ.sh \
        scripts/check-ci-secrets.sh \
        scripts/check-nightly-green.sh scripts/nightly-green-ref.sh \
        scripts/test-nightly-green.sh scripts/check-publish-nightly-gate.sh \
        scripts/check-plain-clone.sh \
        scripts/check-changelog-links.sh \
        scripts/publish-nightly.sh scripts/test-publish-nightly.sh \
        scripts/publish-site.sh scripts/test-site-publish.sh scripts/nightly-publish.sh \
        packaging/site/lev-receive-nightly packaging/site/lev-unpack-docs scripts/test-site-unpack-docs.sh scripts/build-docs-tarball.sh \
        scripts/collect-nightly-debs.sh scripts/test-collect-nightly-debs.sh scripts/build-nightly-apk.sh \
        scripts/deb-stamp.sh scripts/test-deb-stamp.sh scripts/build-deb.sh scripts/lnflash-bundle.sh \
        scripts/build-lnflash-deb.sh \
        scripts/t1000e-dfu-package.sh \
        scripts/test-lnflash-bundle.sh \
        scripts/rnode-flash.sh scripts/check-rnode-chip-offsets.sh \
        scripts/install-ci.sh \
        scripts/install-esptool.sh scripts/install-btvirt.sh \
        scripts/run-fuzz.sh scripts/test-run-fuzz.sh \
        scripts/check-tree-clean.sh scripts/test-tree-clean.sh \
        scripts/test-just-sweep.sh \
        scripts/check-firmware-images.sh scripts/check-embassy-pins.sh scripts/test-log-diff-mask.sh scripts/check-firmware-no-hal.sh scripts/check-firmware-no-chip.sh \
        scripts/check-no-printf-grep-pipes.sh

# The tier-3 debug-port witness (Codeberg #353). Two boards on the rig have
# reset themselves mid-run for months and every occurrence was closed as
# "suspected self-reset", because the board printed its reason to nobody: the
# post-mortem is read-and-cleared at boot and nothing listened on a debug port
# during a run. This gate holds the two claims that can be settled without a
# rig — which ports get a witness, and that a reader survives losing its port
# — plus the verdict-side claim that the RED banner names the resulting file.
# No board, no periculum, no flash; the reader half runs against a pty that is
# taken away and given back.
#
# scripts/test-device-watchdog.sh joins it because the witness only explains a
# vanish somebody else decided happened, and that decision was wrong twice: a
# single failing `lsusb` poll latched a board that never moved, and periculum's
# own per-scenario board reset — a real USB disconnect we ordered — was counted
# as a device failure (Codeberg #65). Both are injected there as failures and
# asserted not to fire.
#
# scripts/test-rig-cell.sh joins it for the single-cell bench runner, which
# brings the same boards' readers back after its cell: a board whose link is
# late must be waited for and one that never returns must be named, and the
# runner it replaced skipped both silently.
[doc('Drive the debug-port witness and device watchdog without a rig')]
hw-witness:
    bash scripts/test-debug-witness.sh
    bash scripts/test-rig-cell.sh
    bash scripts/test-device-watchdog.sh
    bash scripts/tier3-hw-selftest.sh

# Run the checked-in cargo-fuzz targets over the parsers that eat untrusted
# bytes (Codeberg #290). Eight targets with seed corpora had existed since #23
# and #108 and were run by nobody: no recipe, no CI step, no schedule. Three
# September 2026 defects sit on top of three of them -- unbounded msgpack
# recursion (#263), a wrapping bin32 length (#267), an uncapped HDLC
# accumulator (#271) -- and a length field, a nesting depth and an unbounded
# accumulator are what a fuzzer finds in minutes.
#
# NOT in any tier: it needs the nightly toolchain and cargo-fuzz, and even a
# short run costs minutes. `just fuzz` is 60 s per target plus its build (345 s
# for all eight at 30 s each, measured 2026-09-19, warm registry; the
# leviculum-std target's ASan build is 95 s of that). The corpus persists under
# ~/.local/state/leviculum-fuzz, OUTSIDE the checkout, so a scheduled run
# accumulates coverage instead of restarting from the seeds -- the nightly
# builds a fresh clone and deletes it when green, so an in-tree corpus would be
# thrown away by construction.
#
#   just fuzz                    every target, 60 s each
#   just fuzz --seconds 900      the budget a scheduled run wants
#   just fuzz hdlc_deframe       one target
# Exit 1 = a crash, input kept under the state dir; exit 2 = it could not run.
[doc('Fuzz the parsers that eat untrusted bytes (60 s per target)')]
fuzz *args:
    bash scripts/run-fuzz.sh {{args}}

# Fixture test for that runner, on the push path because the runner's failure
# mode is silence: a fuzz run whose finding is not preserved, or whose exit
# code says clean when nothing ran, buys the confidence without doing the work.
# Every case injects the failure into a throwaway fuzz crate -- a crashing
# target, an unregistered target, a missing cargo-fuzz, a stale lockfile -- and
# asserts what the runner concluded. ~16 s; skips with a named reason where
# nightly or cargo-fuzz is absent, so it does not make the toolchain a
# push-path dependency.
[doc('Drive the fuzz runner against an injected crash and a missing tool')]
fuzz-selftest:
    bash scripts/test-run-fuzz.sh

# The scheduled run (Codeberg #290). Same runner, a budget that is worth a
# night rather than a coffee: FUZZ_SECS per target, 120 s by default, over the
# same persistent corpus `just fuzz` feeds, so a night starts where the last
# one stopped instead of re-exploring the shallow paths from the seeds.
#
#   just fuzz-nightly              120 s per target
#   FUZZ_SECS=900 just fuzz-nightly
#
# Exit 1 = a crash or a per-input timeout, with the input kept and named;
# exit 2 = a run that could not happen. Where the corpus and the crash inputs
# go is LEVICULUM_FUZZ_CORPUS / LEVICULUM_FUZZ_ARTIFACTS; both default under
# ~/.local/state/leviculum-fuzz, which is deliberately NOT the cargo target
# directory: the nightly host throws that away wholesale when the build cache
# passes its hard size cap, and a corpus on a cache eviction schedule is a
# corpus that silently restarts.
#
# The build takes each fuzz crate's committed Cargo.lock as GIVEN and refuses a
# stale one (#295, `run-fuzz.sh::assert_lock_current`), rather than resolving
# the graph afresh and writing the difference to disk. This run happens in a
# fresh clone nobody reads the working tree of afterwards, so a lock it
# rewrote would be a diff that exists only until the clone is deleted -- and
# the same file on the push path is the one that stopped landing.
[doc('The scheduled fuzz run: FUZZ_SECS (120) per target over the kept corpus')]
fuzz-nightly:
    bash scripts/run-fuzz.sh --nightly

# The regression half (Codeberg #290): replay the kept corpus and the
# checked-in seeds through every target once, generating nothing. It is not
# fuzzing and it does not look for new inputs -- it asserts that the inputs
# that DID find something are still handled.
#
# Three of them are named seeds for the three defects the issue cites:
# resource_advertisement_unpack/recursion_reproducer (#263, the only one of the
# three that aborted this build -- an ASan stack overflow), .../bin32_len_wrap
# and .../ext32_len_wrap (#267, a wire u32 length that wrapped the bounds check
# on a 32-bit usize), hdlc_deframe/oversized_frame (#271, a frame one byte past
# DEFAULT_MAX_FRAME followed by a good one, so the discard and the resync are
# both replayed).
#
# On the push path because a regression check nobody runs is what #290 is
# about. Skips with a named reason where nightly or cargo-fuzz is absent, so
# it does not make the toolchain a push-path dependency.
#
# A LOCKED BUILD, because this recipe runs in trees that are not its own
# (#295). Each fuzz crate carries its own Cargo.lock and resolves the same
# path graph the workspace does, so a dependency added anywhere under it makes
# that lock stale -- and the build then rewrote it, here, in the landing gate's
# shared push tree: a gate refuses to run in a dirty tree, so the next one
# stopped with rc=5 and no log said which run had written the file. The lock
# is now a precondition the recipe fails on by name
# (`run-fuzz.sh::assert_lock_current`), and `check-tree-clean` below is the
# backstop for whatever else in this tier learns the same habit.
# That precondition, alone, for `guards` (Codeberg #442). `fuzz-regress` is the
# only recipe that reads the fuzz locks and it is in no coder gate, so a
# manifest change that moved a fuzz crate's graph was first seen on the land
# gate: 8042e34a gave leviculum-std a path dependency on leviculum-queue-budget
# and the gate up to bc07cee3 went red here with every target green. The same
# `assert_lock_current`, `cargo metadata --locked` per crate: no build, 0.07 s
# red and 0.11 s green over both crates (measured 2026-10-06).
[doc('Refuse a fuzz crate Cargo.lock that no longer matches its graph')]
check-fuzz-lock:
    bash scripts/run-fuzz.sh --check-lock --skip-if-unavailable

[doc('Replay the fuzz corpus and the defect seeds through every target')]
fuzz-regress:
    bash scripts/run-fuzz.sh --regress --skip-if-unavailable

[doc('Rustdoc gate: broken intra-doc links fail instead of warning')]
doc-gate:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# The same gate over the crates this branch touched, cheap enough to run by
# hand (Codeberg #359). `doc-gate` is the only recipe on the push path that
# runs rustdoc and it costs ~21 s over the workspace, so it is excused from
# `guards` -- and a coder pass therefore has no gate that reads a doc comment
# at all. Pass 344 wrote two links to private items into
# leviculum-lxmf-node/src/telemetry.rs, ran fmt, clippy, `cargo test` and
# `just guards` green, and the red arrived a day later on the land gate with
# sixty-three commits queued behind it.
#
# MEASURED, 2026-09-27, standalone after `touch <crate>/src/lib.rs`:
# leviculum-core 1.9 s, leviculum-lxmf-node 2.2 s, leviculum-lxmf 2.8 s,
# leviculum-std 7.6 s -- so a pass that touched one or two crates pays
# seconds. This recipe docs the union over `origin/master...HEAD` plus the
# working tree, which on a lane with sixty-nine unpushed commits is six
# crates: 0.3 s with nothing dirty, 9.9/11.5/11.8 s in three runs after
# `touch leviculum-core/src/lib.rs`. Over the ten, and it checks host
# workspace crates besides, so it is out of `guards` on both clauses and
# CLAUDE.md's per-batch line names it by hand instead.
#
# Inside `fast` its coverage is a subset of `doc-gate`'s; it sits here to
# fail the tier at the cheap end first, and so that the recipe a coder pass
# is told to run is one the push path exercises rather than a script only
# people run. It costs `fast` +12 s in the touch-core case (12.3 s, and
# `doc-gate` behind it still paid 24.9 s against 26.1 s alone: the two
# select different packages, so cargo unifies features differently and
# neither warms the other) and nothing measurable otherwise.
[doc('Rustdoc over the crates this branch touched (the cheap half of doc-gate)')]
doc-touched:
    @python3 scripts/doc-touched.py

# Codeberg #287: the changelog's version headings and its link definitions are
# two lists of the same versions, and Markdown only renders a heading as a link
# while both hold it. Nothing about writing `## [0.9.0]` produces the matching
# definition, so they drift by default and they drift at the top of the file:
# seven headings had none at 752baa4, the four newest releases among them, and
# `## [0.1.0] - 2025-XX-XX` had shipped its placeholder date in every release
# since. Also refuses a definition no heading uses and a date that is not a
# real YYYY-MM-DD. ~20 ms, reads one file, no network, and self-tests each of
# its five verdicts on fixtures first.
[doc('Check every changelog heading against its link definition')]
changelog-links:
    @bash scripts/check-changelog-links.sh

# Tracing-shim gate (PR #57): leviculum-core must pass the SAME suite with
# tracing OFF as with it on. The `tracing` feature is default-on; with it
# off the level macros become no-ops via the `crate::tracing` shim in
# lib.rs. Running the full suite in that config proves the shim changed no
# core logic (a bare `tracing::x!` that slipped past the shim would either
# fail to compile here or, worse, only on M0 — see m0-build-gate).
[doc('Run the leviculum-core suite with the tracing feature off')]
core-no-tracing:
    {{manifest}} core-no-tracing -- cargo test -p leviculum-core --no-default-features

# Codeberg #463: lnsd and leviculum-std without the `ble` and `serial`
# features, the set a router node or a TCP-only transit host builds. The
# features exist so those users stop stripping the interfaces by hand; this
# recipe is what keeps the set compiling and green, the way `core-no-tracing`
# keeps core's. `compression` stays on: it is the bz2 codec a Python peer's
# compressed resources need, so a daemon without it is not a smaller lnsd but
# a broken one. leviculum-cli's default `alloc-mimalloc` goes, because the set
# is the minimal one, and the libc allocator is what `--no-default-features`
# always meant for it.
#
# Clippy over every target compiles the integration suites in this set too,
# which is what catches a test that reaches for a gated item; the test lines
# run leviculum-std's unit tests and all of leviculum-cli's, whose
# `lnsd_without_interface_features` target exists only here and starts the
# minimal lnsd against a SerialInterface and a BLEInterface section.
# leviculum-std's integration targets are not run: they spawn the release
# integ binaries, which are built with the defaults.
[doc('Build and test leviculum-std and lnsd without ble and serial')]
lnsd-minimal:
    cargo clippy -p leviculum-std --no-default-features --features compression --all-targets -- -D warnings
    cargo clippy -p leviculum-cli --no-default-features --features compression --all-targets -- -D warnings
    {{manifest}} lnsd-minimal-std -- cargo test -p leviculum-std --no-default-features --features compression --lib
    {{manifest}} lnsd-minimal-cli -- cargo test -p leviculum-cli --no-default-features --features compression

# Every `event = "NAME"` literal in the workspace has its row in EVENT_CATALOG
# (leviculum-std/src/event_log.rs), and nothing else. The test lives in
# leviculum-std but the emitters live mostly in leviculum-core, so a pass that
# adds an event in core and runs `cargo test -p leviculum-core` never runs it:
# ANN_QUEUE_STALE (transport.rs) went to the land gate on 38ed5816 that way and
# was catalogued one pass later. `cargo test --workspace --lib` holds the same
# test; this recipe is the name a pass can run when it adds an `event = "`
# literal. 0.4 s warm, 33 s after `touch leviculum-core/src/transport.rs`
# (measured 2026-10-11), which is the leviculum-std test build and the reason
# it is not in `guards`.
[doc('Check every emitted event name is declared in EVENT_CATALOG')]
event-catalog:
    {{manifest}} event-catalog -- cargo test -p leviculum-std --lib event_catalog_completeness

# Cortex-M0 gate (PR #57): leviculum-core must cross-compile for thumbv6m
# (atomic-less MCU, e.g. rp2040) with tracing off. tracing-core's CAS-based
# callsite registry does not compile there, so the default build FAILS on
# M0; --no-default-features must succeed. Keeps M0 support from rotting.
[doc('Check that leviculum-core still cross-compiles for Cortex-M0')]
m0-build-gate:
    rustup target add thumbv6m-none-eabi
    cargo build -p leviculum-core --target thumbv6m-none-eabi --no-default-features

# Codeberg #237: leviculum-lxmf must stay buildable for the firmware triple —
# the Telemeter codec is headed for leviculum-nrf, which does not depend on
# the crate yet, so no firmware build would catch a std leak here. Default
# features off keeps `pow`/sha2 out, the configuration an embedded consumer
# would use.
[doc('Check that leviculum-lxmf still builds for the firmware triple')]
lxmf-embedded-gate:
    rustup target add thumbv7em-none-eabihf
    cargo build -p leviculum-lxmf --target thumbv7em-none-eabihf --no-default-features

# Codeberg #219: lnomad's browser core (lnomad/src/app/) does not know it has
# a terminal, so a GUI or the M9 is a second frontend over it, not a rework
# (docs/src/concepts/loquax-architecture.md, "the core must not know it has a
# terminal"). The static half reads the core's sources and lnomad's resolved
# graph without the `tui` feature, after refusing an injected ratatui import
# and tui-input's default features on a temp copy; ~1 s, no build, in `guards`.
# The full recipe adds clippy over that configuration, which compiles
# leviculum-std for the host and so stays out of `guards`.
[doc('Check the lnomad core names no terminal crate (static half)')]
lnomad-core-no-tui-static:
    @bash scripts/check-lnomad-core-no-tui.sh --static

[doc('Check the lnomad core builds and lints without its terminal frontend')]
lnomad-core-no-tui: lnomad-core-no-tui-static
    @bash scripts/check-lnomad-core-no-tui.sh --build

# 984, Codeberg #475: leviculum-ui is the terminal-free half of the UI lnomad
# and loquax share, so a core that takes its types stays terminal-free. The
# same two checks as above, over the crate alone: the static half reads
# leviculum-ui/src/ and its resolved graph with every feature, after refusing
# an injected ratatui import and tui-input's default features on a temp copy;
# ~1 s, no build, in `guards`. The full recipe adds clippy over the crate.
[doc('Check leviculum-ui names no terminal crate (static half)')]
ui-no-terminal-static:
    @bash scripts/check-lnomad-core-no-tui.sh --ui-static

[doc('Check leviculum-ui builds and lints with no terminal crate')]
ui-no-terminal: ui-no-terminal-static
    @bash scripts/check-lnomad-core-no-tui.sh --ui-build

# Codeberg #303: run the leviculum-core lib suite on a 32-bit `usize`.
#
# NOT a firmware test. The target is i686 x86 Linux with std and an
# allocator; the ONE property it shares with thumbv7em-none-eabihf is
# `usize == u32`. Alignment, endianness-independent layout, the absent
# allocator, no_std and the SoftDevice are all different, and a green run
# here says nothing about any of them. What it does cover is the class of
# defect where a wire-supplied length is added to an offset: on 64-bit that
# arithmetic cannot wrap, so every host gate is blind to it, and #267 was
# invisible for the project's lifetime for exactly that reason — a
# `*pos + len > data.len()` guard in resource/msgpack.rs that a peer could
# wrap below `data.len()` with one packet after a link handshake.
#
# Both profiles, because they fail on different inputs:
#   debug   — overflow-checks on, so the ADDITION traps. Catches a wrap even
#             when the wrapped sum would land harmlessly inside the buffer
#             and never reach a bad slice.
#   release — overflow-checks off (see [profile.release] in Cargo.toml: it
#             does not set them), so the wrap happens and the SLICE panics.
#             That is the shipped failure mode, and it is also the only arm
#             that sees a truncation the debug trap cannot — `len as usize`
#             from a u64, or a deliberate `wrapping_add`, do not trap.
# Injecting the pre-#267 guard back into `take` on 2026-08-18 confirmed both
# arms fail on it and the x86_64 run stays green: debug panicked "attempt to
# add with overflow" at the addition, release "slice index starts at 5 but
# ends at 0" at the slice.
#
# Env vars rather than a `[target.i686-unknown-linux-musl]` section in
# .cargo/config.toml: a section there also changes what a bare `cargo build
# --target i686-...` does for everyone, and this gate should not own that.
# rust-toolchain.toml's `targets` stays as it is for the same reason the
# embedded triples are not in it — the download is forced on every checkout
# instead of on whoever runs the gate. The `rustup target add` below is how
# m0-build-gate and lxmf-embedded-gate already handle it, so a pin bump
# self-heals here.
#
# `--all-features`, not the default set. `compression` is not a default
# feature of leviculum-core, so a bare `-p leviculum-core --lib` compiles
# `#[cfg(feature = "compression")] pub mod compression` out and runs 1643 of
# the crate's 1673 lib tests. The 30 it drops are the whole compression
# module — including `resource::compression::tests::decompress_hint_is_
# clamped`, a clamp over a wire-supplied decompressed size, which is the
# defect class this gate exists for. `check-all-targets` and the workspace
# lib run do not show the gap: cargo unifies features across a workspace
# build, so another crate turns `compression` on there and the count comes
# out at 1673 either way. Prefer `--all-features` over naming `compression`
# so the next feature-gated module cannot escape the gate the same way.
# Measured on schneckenschreck 2026-08-18: 15.7 s wall warm (14.6 s debug,
# 1.1 s release — the debug arm is dominated by unoptimised bz2 roundtrips,
# 3.7 s of it the rest of the suite); cold, with the target already fetched,
# ~80 s for the two i686 builds of the crate and its deps — once per host
# per toolchain.
[doc('Run the leviculum-core lib suite on a 32-bit usize')]
i686-usize-gate:
    rustup target add i686-unknown-linux-musl
    CARGO_TARGET_I686_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
    CARGO_TARGET_I686_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C link-self-contained=yes" \
    {{manifest}} i686-usize-debug -- cargo test -p leviculum-core --target i686-unknown-linux-musl --all-features --lib
    CARGO_TARGET_I686_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
    CARGO_TARGET_I686_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C link-self-contained=yes" \
    {{manifest}} i686-usize-release -- cargo test -p leviculum-core --target i686-unknown-linux-musl --all-features --release --lib

# Codeberg #415: check leviculum-std on a target with NO 64-bit atomic.
#
# `std::sync::atomic::AtomicU64` is compiled only where
# `target_has_atomic = "64"`. A user cross-compiling `lnsd` for a MIPS router
# (mips-unknown-linux-musl, soft float) got three unresolved imports out of
# this crate and had to patch it before it would build; the counters now go
# through `leviculum-std/src/counter64.rs`, which keeps the width and varies
# the mechanism.
#
# The i686 gate above cannot see that class of defect: i686 is 32-bit but it
# HAS a 64-bit atomic (cmpxchg8b), so `rustc --print cfg` lists
# target_has_atomic="64" there and every AtomicU64 resolves. Pointer width and
# atomic width are separate properties and need separate targets.
#
# The target is arm-linux-androideabi, and that choice is about the C compiler
# rather than about Android. The reporter's own triple is tier 3 — rustup
# carries no std for mips-unknown-linux-musl, so a gate cannot `rustup target
# add` it and self-heal the way this one does. Of the targets rustup does
# carry without a 64-bit atomic, all the others (armv5te-unknown-linux-gnueabi
# and -musleabi, powerpc-unknown-linux-gnu) have target_os = "linux", which
# turns on the `[target.'cfg(target_os = "linux")'.dependencies]` block in
# leviculum-std/Cargo.toml: bluer -> dbus -> libdbus-sys, whose build script
# compiles vendored C and wants a cross gcc this gate would then demand of
# every host and of the forge. Android's target_os is "android", the block
# does not apply, and the gate needs nothing rustup cannot install.
#
# Same reason it checks the library and not the binary the reporter built:
# `-p leviculum-cli --bin lnsd` pulls libsqlite3-sys, which compiles C for
# every target there is. A cross-build of the whole daemon is the reporter's
# business and needs their toolchain; what this gate owns is that our Rust
# stops being the reason it fails.
#
# What that costs, said plainly: `interfaces/ble/` is linux-only and therefore
# not compiled here, so an AtomicU64 added inside the BLE interface would pass
# this gate. The rest of the crate is covered.
#
# `cargo check`, not `-D warnings`: dropping the linux-only modules leaves
# `OutgoingPacket.peer` read by nobody, a pre-existing dead_code warning about
# cfg coverage and not about portability. Turning it into an error here would
# make the gate fail for a reason it is not about.
#
# The counters' arithmetic is not checked by this recipe — a cross-check
# compiles and never runs. The mutex arm is compiled on every target so its
# unit tests (`counter64::tests`, including the 2^32 crossing) run in the
# workspace lib suite below.
#
# Measured on hamster 2026-09-25: 6.3 s from an empty
# `target/arm-linux-androideabi` with the sccache wrapper switched off — 101
# crates, check-only, no codegen — and ~3 s to re-check the crate alone once
# its dependencies are there. The `rust-std` download is once per host per
# toolchain, like the embedded triples above.
[doc('Check leviculum-std on a target with no 64-bit atomic')]
no-atomic64-gate:
    rustup target add arm-linux-androideabi
    cargo check -p leviculum-std --target arm-linux-androideabi

# Guarantee C step 1 (docs/src/concepts/checks-and-citations.md): the four
# vendored references must sit at the commit their gitlink names. One wrong
# fact — `reference/LXMF` twelve commits behind for five weeks — silently
# repoints every LXMF citation in the tree. Sub-second, no build, and first
# in `fast` so it reports before anything expensive: a batch that gets a
# compile error still learns its references are wrong.
#
# In a gate, not a `#[test]`, deliberately. The `reference_lock` test that
# should have caught the LXMF drift was itself red and unobserved for the
# whole five weeks; a test can be the thing that runs nowhere.
[doc('Check the vendored references sit at the commit their gitlink names')]
check-submodules:
    @bash scripts/check-submodule-pins.sh

# Codeberg #196: the core-processor seam must keep making the two prohibitions
# of the core-lock budget unrepresentable. Builds the leviculum-std `cf_*`
# fixtures, each of which is a `CoreProcessor` attempting one forbidden move,
# and asserts each fails to compile with one SPECIFIC error code — "does not
# compile" is not the assertion.
#
# A gate rather than a #[test] for the same reason as check-submodules, plus a
# mechanical one: shelling out to cargo from inside `cargo test` blocks on the
# build-directory lock the outer invocation holds.
[doc('Check the core-processor seam still refuses the forbidden moves')]
check-processor-seam:
    @bash scripts/check-processor-compile-fail.sh

# Guarantee C step 3: `path:line` citations in docs/src/** and in the Rust
# sources of leviculum-core/-lxmf/-std must still point at what they claim.
# ~3 s, and the leviculum-std test deps are already built by `mvr`, so it is
# nearly free here. No tier ran this suite before — `fast` runs `--lib` only
# and `standard` names leviculum-std suites one by one — which is the
# Guarantee-B-masking-C shape the concept page warns about.
[doc('Check that path:line citations still point at what they claim')]
citation-guard:
    {{manifest}} citation-guard -- cargo test -p leviculum-std --test doc_citations -- --nocapture

# Codeberg #205: no commit since the pinned baseline may carry a machine-
# authorship trailer. ~20 ms — one `git log` over the range and one `awk`.
#
# The enforcement is the forge check (.woodpecker/commit-trailers.yml), which
# a fresh clone cannot skip. This entry is what puts the same check on the
# pre-push path, where `.githooks/pre-push` runs `just fast`: the incident
# this exists for was a trailer that reached a commit and was caught by a
# human reading the message in the window between committing and pushing.
[doc('Check that no commit carries a machine-authorship trailer')]
check-trailers:
    @bash scripts/check-commit-trailers.sh

# Codeberg #310: exactly one list of the binaries periculum mounts, and it is
# `build-integ-bins` below. A caller that writes its own `cargo build --bin`
# line has copied that list, and the copy drifts unseen until a hardware
# nightly aborts in its freshness preflight naming a binary nothing built.
# ~30 ms, reads two files, and self-tests its classifier on seven fixtures
# first. Same family, same reasons, as check-submodules above.
[doc('Check that only build-integ-bins lists the integration binaries')]
check-integ-bin-list:
    @bash scripts/check-integ-bin-list.sh

# Codeberg #299: one workflow in .woodpecker/ must run the CI gate on every
# push, with no `path:` filter. Until #299 an ordinary commit — Rust source,
# no packaging file — reached the public forge with no test having run there:
# the nightly's gate is filtered to the packaging paths, the trailer check
# reads messages, and the pre-push hook is per-clone config that `--no-verify`
# switches off. ~20 ms, reads the pipeline files, and self-tests its
# classifier on six fixtures first. Same family, same reasons, as
# check-submodules above — and it is deliberately not pinned to a filename, so
# renaming the pipeline is not a regression.
[doc('Check that a forge workflow runs the CI gate on every push')]
check-ci-pipeline:
    @bash scripts/check-ci-pipeline.sh

# Every `from_secret:` under .woodpecker/ must name a secret somebody has
# actually created, written down in scripts/ci-secrets.txt.
#
# A missing secret is not a red step in Woodpecker, it is a compile error for
# the whole pipeline: `status: error, workflows: 0`, nothing runs. Cron #434
# ended that way on `secret "site_ssh_target" not found` and took the gate,
# both builds and the FORGE publish down with it — three weeks of stale .debs
# behind every download URL in the tree, caused by a step that runs last and
# could not be configured. Nothing running locally can see that failure; it
# happens before the first container starts. What can be checked locally is
# that no pipeline names a secret nobody wrote down, which is where every
# instance of this bug begins. ~20 ms of awk, self-tested on five fixtures
# first, and red against HEAD~ of the commit that added it.
[doc('Check that every pipeline secret is one somebody wrote down')]
check-ci-secrets:
    @bash scripts/check-ci-secrets.sh

# Codeberg #286: the nightly publish step, driven against a fake forge.
#
# It is the one script in the tree whose only production run is unattended,
# on a machine nobody is watching, against the public releases page — and it
# used to delete the previous build's assets BEFORE uploading the new ones,
# with no status check on the upload at all. A 500 from Codeberg therefore
# left the release empty, the README's hardcoded download URLs at 404, and
# the pipeline green. `scripts/test-publish-nightly.sh` injects that 500, a
# failing delete and an empty dist/ into the real script and asserts what the
# release holds afterwards. ~0.4 s, no network: `curl` and `git` are fixtures
# on PATH.
[doc('Drive the nightly publish step against a fake forge')]
publish-selftest:
    @bash scripts/test-publish-nightly.sh

# Codeberg #312: the two halves of the nightly publish signal, driven against
# a fake remote.
#
# `rnsd_interop` — whether we still interoperate with a Python-RNS peer — runs
# in NO forge pipeline: it needs the reference/Reticulum submodule and a
# python3, and both pipelines clone with `submodules: false` on purpose (#300).
# So the tier-2 nightly's verdict is imported instead of re-derived: on a green
# night it pushes `refs/nightly/green/<stamp>` at the commit it tested, and the
# publish step refuses a commit no such ref covers.
#
# Both scripts run exactly once a day on machines nobody is watching — the same
# blind spot that left the rolling release standing still for five weeks — so
# neither is exercised by being used. This makes each of the three refusals
# fire (no ref, not an ancestor, too old), proves the override works and needs
# a reason rather than a flag, and proves that a red or an ABSENT rnsd_interop
# signs nothing. ~1 s, no network, no build: `git` is a fixture on PATH.
[doc('Drive the nightly green-ref signal against a fake remote')]
nightly-green-selftest:
    @bash scripts/test-nightly-green.sh

# The wiring the selftest above cannot see: that the mechanism is still
# CONNECTED. Five links from a commit to the releases page — the publish step
# runs publish-nightly.sh, that script runs the gate, it runs it before the
# first forge request, `complete` still runs the whole workspace (narrowed to
# `--lib` the ref would certify a run with no interop test in it), and the
# signer reads rnsd_interop out of the run's manifest rather than trusting the
# caller.
#
# Separate from nightly-green-selftest because the #312 failure mode is
# available to both halves: a gate wired into nothing, and a gate wired in that
# says yes to everything. --selftest is the positive control and is not
# optional — it breaks each of the five links in a fixture tree and requires
# the classifier to reject it. ~50 ms, reads YAML, the Justfile and shell as
# text.
[doc('Check that a red rnsd_interop cannot reach the publish step')]
check-publish-nightly-gate:
    @bash scripts/check-publish-nightly-gate.sh
    @bash scripts/check-publish-nightly-gate.sh --selftest

# The nightly's OTHER cron-only step, driven against a fixture tree.
#
# `package` and `publish` both carry `when: event: [cron, manual]` (726), so until this
# recipe existed exactly half of that pair had coverage and the other half
# had none. The rolling release stood still from 2026-08-24 to 2026-09-18
# and every push pipeline over those five weeks was green: 03e2cb95 put a
# `cargo metadata` call on the path of a step that runs in
# debian:bookworm-slim, where there is no cargo, and nothing could say so
# until 02:00 the next morning — where nobody read it.
#
# So the test's environment is its assertion: the real script runs with a
# PATH holding only the tools that image ships plus the git the step
# installs, no cargo and no python3 among them. ~0.5 s, no network, no
# build. Red against the pre-fix script with exactly the pipeline's own
# error ("cargo: command not found").
#
# scripts/test-lnflash-bundle.sh joins it because the collector finds the
# lnflash bundle by its name: the bundle carries the leviculum .deb's stamp
# (PUB-0022) in its tarball name, its UF2 names and manifest, and its README,
# and that test runs the real bundle script on a stubbed toolchain from a
# known commit and asserts all three. ~0.5 s, red against the unstamped script.
[doc('Drive the nightly packaging and lnflash bundle steps on fixtures')]
package-selftest:
    @bash scripts/test-collect-nightly-debs.sh
    @bash scripts/test-lnflash-bundle.sh

# Codeberg #295: "supported" meant two different things on two surfaces. The
# README named the T114 and the RAK4631 as the supported boards for the
# embedded firmware and then offered nothing but a build from source, while
# the nightly had been shipping a self-contained lnflash bundle with a
# prebuilt image for each of them since 24481f12 — and the release body went
# on describing that bundle as carrying "the T114 firmware image" for four
# weeks after the RAK image started shipping inside it. A reader with a RAK
# concluded from what was written that there was no image for their board.
#
# Nothing connected the three surfaces, so this compares them as sets and
# requires them equal: the boards scripts/lnflash-bundle.sh builds an image
# for, the boards the README's flashing table advertises, and the boards the
# release body names. Both directions are the bug — an image nobody is told
# about, and a board advertised with no image behind it — which is why it is
# equality and not containment. It also checks every nightly download URL the
# README hands out against the asset names collect-nightly-debs.sh stages.
#
# --selftest is the positive control, and it is not optional here: the first
# run of it passed one injected bug it was supposed to catch. ~0.1 s, no
# network, no build; it reads Markdown and two shell scripts.
[doc('Check that images, README table and release body name one set')]
check-firmware-images:
    @bash scripts/check-firmware-images.sh
    @bash scripts/check-firmware-images.sh --selftest

# The nightly's SECOND publish target: our own site (Codeberg #286's sibling).
#
# Every download link we publish points at the forge's rolling release, so a
# forge outage breaks every install instruction in the tree, and leaving the
# forge would break them for good. `packaging/site/lev-receive-nightly` is
# what the build is uploaded into, and it is the one script in this tree that
# runs unattended, as a forced command, on input from the network, writing
# into a directory a web server publishes to strangers.
#
# So its refusals ARE the feature: a bad checksum, a `../` member, a symlink
# member, an unpaired file, an empty upload, an unsafe build id. Each is made
# to fire here against the real script, and each case asserts afterwards that
# the previous `latest` still resolves — a half-published release under a
# hardcoded download URL is exactly what this must never produce. The sender
# is driven through its `--tar-only` hook, so the archive the receiver is
# tested against is the real one. ~2 s, no network, no ssh, no build.
[doc('Drive the site publish receiver against the uploads it must refuse')]
site-publish-selftest:
    @bash scripts/test-site-publish.sh

# The version half of the same pipeline. deb-stamp.sh decides what every
# package built between two releases is CALLED, and two of its decisions are
# only correct if they are measured rather than believed: a semver
# pre-release has to change its `-` to a `~` or Debian sorts the development
# window ABOVE the release it precedes, and the commit distance in the build
# id comes from `git describe`, which answers nothing in a tagless clone.
#
# Both are checked against a fixture workspace, the wrong spelling included
# so the guard has a positive control. ~2 s, no network, no build: four empty
# crates and `cargo pkgid`. In `fast` for the same reason package-selftest is
# — the pipeline step it covers runs only from cron, where a break is found
# by nobody at 02:00 the next morning.
[doc('Drive the .deb version stamper against a fixture workspace')]
deb-stamp-selftest:
    @bash scripts/test-deb-stamp.sh

# 1226: every versioned unit whose files changed since the last landing
# (origin/master) carries a version newer than the one landed there, so the
# numbers move with the code instead of waiting for someone to remember
# (the workspace said 0.10.0-dev for 1255 commits). The fixtures run first,
# a scripted git repository with three units, then the check on this tree.
# A tree that forgot runs `python3 -I scripts/bump-touched.py` and commits
# what it changed. ~0.7 s: one `git diff` per unit and one `cargo metadata`.
[doc('Refuse a tree whose changed units did not move their version')]
version-guard:
    @python3 -I scripts/bump-touched.py --selftest
    @python3 -I scripts/bump-touched.py --check

# Codeberg #309: what the tier runners make of periculum's lock-contention
# marker. The marker stopped being an empty flag file in periculum #30/#31 --
# it carries the holder's verdict, pid and age -- and all three runners still
# deleted it before reading a byte, so a suspected wedge (a holder past 24 h,
# or one the kernel disagrees with) reached the ledger as the same
# `SKIPPED lock-held` as a two-minute overlap.
#
# In `fast` for the reason deb-stamp-selftest is: these three scripts run from
# cron and from nowhere else, so a break in them is found by nobody. ~1 s, no
# build, no docker, no rig -- the tier command is stubbed through the runners'
# selftest seam and notify-send is a fixture on PATH.
[doc('Drive the tier runners against a stubbed lock-contention marker')]
lock-contention-selftest:
    @bash scripts/test-lock-contention.sh

# Codeberg #304: the pin in rust-toolchain.toml is raised deliberately at
# release time, and nothing said when that was due. This prints how far it
# has fallen behind current stable -- pinned version, newest release, and the
# distance between them -- and exits 0 whatever the answer is.
#
# A report, not a gate, and the distinction is the whole issue: a check that
# refuses once stable has moved goes red on the Rust release train's schedule
# rather than on a change of ours, and the cheapest way to clear it is to
# bump the pin without considering it, which is the drift the pin exists to
# prevent. This repo has run that experiment once already -- the tier-2
# staleness verdict that blocked every push for 46 days, recorded above the
# tier-2 block in scripts/ci-status.sh.
[doc('How far the pinned toolchain has fallen behind current stable')]
toolchain-status:
    @bash scripts/toolchain-distance.sh

# The report above is consumed by the nightly status, where a non-zero exit
# or a second line of output is a broken morning report rather than an error
# anyone sees. So every case is pinned here against a fake `rustup`: the two
# spellings `rustup check` uses, offline, no rustup at all, no `stable`
# toolchain installed, a floating channel, a missing file -- each asserting
# exit 0 and exactly one line.
#
# The load-bearing case is the parse: `rustup check` prints the INSTALLED
# stable and the NEWEST release on one line, older first, and picking the
# wrong one yields a plausible number that never moves. That case fails on
# sight of the installed version. ~1 s, no network. In `fast` for the reason
# deb-stamp-selftest is: its consumer runs from cron, where a break is found
# by nobody the next morning.
[doc('Selftest: the toolchain report never refuses and prints one line')]
toolchain-status-selftest:
    @bash scripts/test-toolchain-distance.sh

# Codeberg #300: the tree must build from a clone without submodules, which is
# what both forge pipelines and every contributor start from. An
# `include_str!` into `reference/` is a compile-time dependency, so the crate
# holding it does not build there — its tests are lost, not skipped — and
# `cargo test --workspace --lib` fails outright. Three had accumulated
# (lnomad, leviculum-micron, lnpnd) and the CI gate paid for a submodule fetch
# to work around them. ~50 ms of grep, self-tested on five fixtures first.
[doc('Check that the tree builds from a clone without submodules')]
check-plain-clone:
    @bash scripts/check-plain-clone.sh

# Every long-lived process spawn goes through
# `leviculum_std::process::spawn_supervised`, so the kernel takes the child down
# with its parent however the parent dies. ~200 ms, no build: it reads the
# sources. Bare `Command::new(..).spawn()` sites are counted per file against
# scripts/supervised-spawn-counts.txt, so a new one is a diff rather than seven
# orphaned daemons found by hand four hours later (2026-08-07).
#
# In `fast` because it passes all three hook conditions: fast, deterministic
# given the tree, and it fails naming a file:line the author has open. The
# behaviour itself is proved by `--test supervised_spawn` below; this is the
# check that the proof still covers every site.
[doc('Count bare process spawns against the supervised-spawn census')]
check-supervised-spawns:
    @python3 scripts/check-supervised-spawns.py

# Codeberg #199: the public methods that take the core lock, counted rather
# than described. A `CoreProcessor` hook runs with that lock held, so every one
# of them is a way for a consumer holding a node handle to deadlock the node in
# safe synchronous code -- and the seam documented the set as "roughly forty"
# until this gate counted 58. Same shape and same reasons as the spawn census
# above: a text scan, sub-second, no build, fails naming the method.
[doc('Count the public methods that take the core lock')]
check-core-lock-census:
    @python3 scripts/check-core-lock-census.py

# Codeberg #347: the environment knobs the shipped code reads, each with a
# verdict. A variable that changes what a daemon does is a knob nobody
# configured -- not in the config file, not in any status output -- and #347's
# three-arm jitter selector is one that has to be DELETED when the A/B
# answers. The census makes both halves loud: a knob added without a verdict
# fails, and a verdict left behind after its knob is gone fails too. Same
# shape as the two censuses above: a text scan, sub-second, no build, fails
# naming a file:line.
[doc('Give every environment knob a verdict in the knob census')]
check-env-knobs:
    @python3 scripts/check-env-knobs.py

# Codeberg nightly #509: an mvr drew a port, the probe found it free, and its
# own bind of that number a few instructions later was refused (EADDRINUSE).
# A number picked before its bind is a handoff anything on the host can win,
# and a literal is the same handoff for the whole run. Under leviculum-std/tests/
# this refuses a literal port scripts/test-port-literals.txt does not admit, a
# listener the test binds itself on a port it chose first or got by binding 0
# and dropping, and a draw without a `port-handoff:` reason (see
# leviculum-std/tests/support/ports.rs). The selftest injects each fault first.
[doc('Refuse literal and advance-picked test ports')]
check-test-ports:
    @python3 scripts/check-test-ports.py --selftest
    @python3 scripts/check-test-ports.py

# Order 1172: 1159 and 1162 shipped facade tests that pulled "up to three
# times" until the list showed an import, and the retry hid a real gap (an
# import beside the daemon woke no watch, fixed in 1164). A test polls a STATE
# until a deadline; it never re-issues the ACTION under test until it works.
# Over the loquax and loquax-android tests, loquax's #[cfg(test)] items and the
# mvrs, this refuses a loop whose body both issues a request (the list at the
# top of the script, helpers resolved) and sleeps or checks a deadline. An
# allow is `// retry-guard: allow <reason>` above the loop, printed every run.
# The selftest plants the retried pull, its allowed twin and a state poll first.
[doc('Refuse a test loop that retries a request until it works')]
test-retry-guard:
    @python3 scripts/check-test-retries.py --selftest
    @python3 scripts/check-test-retries.py

# Order 1340 (#843): bash's printf writes a pipe one line per write(2), so
# under `pipefail` `printf "$list" | grep -q X` is exit 141 whenever grep
# matches and exits before printf is done (1314: 14 of 20 000 under load).
# Three sites went red that way before the tree was converted to here-strings;
# this refuses the construct, printf or echo into `grep -q`, in every tracked
# shell file that sets pipefail or is only ever sourced. Text scan, 0.3 s. The
# selftest plants five pipes and five clean shapes first.
[doc('Refuse printf/echo piped into grep -q under pipefail')]
no-printf-grep-pipes:
    @bash scripts/check-no-printf-grep-pipes.sh --selftest
    @bash scripts/check-no-printf-grep-pipes.sh

# Codeberg #191c's census, moved forward to the push gate. `just standard`
# already counts the #[ignore]d tests, but from the BUILT test binaries --
# `--ignored --list` on every one of them -- so it cannot run here: Tier 0
# links no test binary and must not start. This counts the same bucket from
# the sources against the same scripts/ignored-counts.txt, ~0.2 s, no build.
#
# What it buys: on 2026-09-23 commit 82425837 added an #[ignore]d reference arm
# to rnsd_interop behind a `just fast` gate, which ran no census at all; the
# land gate found it an hour in, with 30 commits queued behind it, and the
# author had had no way to see it. The two counts agree unit for unit today,
# 48 across 9 units.
#
# One way it can differ from the binary count, stated here and in the script's
# header: a cfg-gated test. `#[cfg(feature = "x")] #[test] #[ignore]` is a line
# of source either way, so this counts it always and the binary census counts
# it only when that cfg is on. Where they disagree the binary one is right and
# the pin follows it. Accepted: the case this gate is for is the ordinary one.
[doc('Count #[ignore] in the sources against the pinned census')]
check-ignored-source:
    @python3 scripts/check-ignored-counts-source.py

# Codeberg #301: `just --list` is the first thing a stranger reads, and until
# this gate most of its lines were the tail of a comment block rather than a
# description -- `ci-gate` introduced itself as "day one it would only teach
# people to skip the gate". just keeps exactly ONE comment line per recipe
# (`take_doc_comment` in its parser walks back over a newline and a single
# `Item::Comment`), so a long block is not truncated to its summary, it is
# truncated to whatever happens to be written last, and a blank line above the
# recipe suppresses the description entirely. `[doc('...')]` takes precedence
# over the comment, which is what makes the line a decision instead of an
# accident; this gate requires one on every listed recipe and keeps it short
# enough that the list stays scannable. ~0.2 s, reads one file, self-tests its
# five verdicts on fixture justfiles first. Same family, same reasons, as
# check-submodules above.
[doc('Check that every listed recipe has an explicit description')]
check-just-docs:
    @python3 scripts/check-just-docs.py

# `guards` is a list of names with no body and `fast` is a longer list with the
# same names in it, so the only thing holding the two together is that somebody
# reads them against each other. This gate is what reads them: subset, order,
# and a written reason for every member of `fast` that `guards` leaves out.
# 0.05 s, no build -- it runs `just --dump --dump-format json` and compares two
# dependency closures, so it cannot drift from the grammar it is checking.
#
# In `guards` itself, which is the point: the pass that would have added the
# unpaired guard is the pass that gets told.
[doc('Check that `guards` is still a subset of `fast`, in order')]
check-guards-subset:
    @python3 scripts/check-guards-subset.py

# #408: `standard` runs every integration-test target in the tree, most of
# them through scripts/standard-integ.sh's computed selection. This holds the
# three things that selection stands on: the enumeration agrees with `cargo
# metadata`, every target the script leaves to another line of `standard`
# (scripts/standard-integ-elsewhere.txt) is still on that line, and `standard`
# still calls the script. ~0.2 s, no build, its own fixtures first.
[doc('Check that `standard` runs every integration-test target')]
check-standard-integ:
    @python3 scripts/check-standard-integ.py

# 676: the config reference's table rows cite the struct member and the INI
# arm of every key, and the citation guard holds a named citation only to its
# name within 8 lines. The members are three lines apart and their doc
# comments repeat the key, so every BLE row had drifted onto the member above
# it with the guard green (674). This holds those two cell shapes to the exact
# item. ~0.1 s, no build, its fixtures first; `--fix` repoints from the
# definitions.
[doc('Check that config-reference table citations land on their key')]
check-config-table-citations:
    @python3 scripts/check-config-table-citations.py

# The guards in .githooks/pre-push and the remedy their refusals print
# (scripts/push-clean.sh), driven against scratch repositories (~0.3 s, no
# build). They are cold code: they fire on the rare wrong push and nothing
# exercises them in between. The one selftest that did cover the ref guard
# lives outside the repository on a single host, so it says nothing about the
# hook in any other clone.
#
# Three of the twenty-two cases are negative controls — an untracked file, a
# non-master ref at another sha, and a push from a clone with core.hooksPath
# unset. The last is the load-bearing one: the old printed remedy produced a
# tree that pushed with no hook at all, and the push still arrived, so only a
# case where the proof FAILS tells the hook-ran evidence apart from the
# push-arrived evidence.
#
# In `fast`, which is what the hook itself runs, so a guard broken by an edit
# is caught by the next push rather than by the push it wrongly refuses.
[doc('Drive the pre-push guards against scratch repositories')]
prepush-guard:
    @bash scripts/check-prepush-guard.sh

# Regenerate THIRD-PARTY-NOTICES from the two lockfiles (Codeberg #288).
# Needs cargo-about; scripts/install-ci.sh installs the pinned version.
[doc('Regenerate THIRD-PARTY-NOTICES from the two lockfiles')]
notices:
    @python3 scripts/gen-notices.py

# Codeberg #288: every published artifact carried our AGPL LICENSE and no
# notice for the MIT- and BSD-licensed crates statically linked into it, which
# both families require to accompany a BINARY distribution. The notices are
# generated from Cargo.lock, checked in, and copied into the .debs, the
# userspace tarballs and the lnflash bundle. This is what stops the checked-in
# copy from describing a dependency graph that no longer exists.
#
# In `fast` rather than in the nightly's `ci-gate`, on all three of the
# conditions the gates above are held to. Fast: ~20 s, no compilation — it
# reads Cargo.lock and the licence files already in the cargo cache. Deterministic
# given the tree: `--frozen` throughout, so no network and no lockfile update
# can move the output, and a CRLF licence file cannot either (the generator
# normalises line endings; without that the guard failed against a file it had
# just written itself). And it fails naming the one command that fixes it, in
# the same session that added the dependency.
#
# The placement follows from where the file can rot: a `cargo add` is the
# moment the checked-in list stops matching, and the push path is the last
# point at which the person who typed it is still there. `ci-gate` would catch
# it too, but a night later and against a container that has neither the
# firmware workspace fetched nor cargo-about installed.
[doc('Check that THIRD-PARTY-NOTICES still matches the lockfiles')]
notices-guard:
    @python3 scripts/gen-notices.py --check

# The kernel-enforced half of "a harness that spawns a long-lived process must
# ensure it dies with the harness" (docs/src/concepts/checks-and-citations.md).
# ~10 s: it SIGKILLs a parent and watches its child disappear, plus the negative
# control where the same child — spawned without the fix — must survive. Both
# arms are bounded and fail loudly rather than waiting, which is the mistake the
# incident behind them was about.
[doc('Prove a supervised child dies with its parent')]
supervised-spawn:
    {{manifest}} supervised-spawn -- cargo test -p leviculum-std --test supervised_spawn

# Codeberg #220: `just fast` was green on a tree where eight integration-test
# targets did not compile. Every Tier-0 gate builds workspace libs only, so an
# E0616 in tests/ (a field privatised under a test that reads it) rode the
# pre-push hook unseen and was first caught by `cargo test --workspace`.
# Compile-only, no execution: every lib, bin, example and integration-test
# target must build. Measured 2026-08-12 on a warm tree: ~11 s after a batch,
# ~0.2 s as a no-op — cheap enough for the push path. `--no-tests` declares
# the empty manifest as intended, so the wrapper's executed-zero failure keeps
# guarding the gates that do run tests.
[doc('Compile every workspace lib, bin, example and test target')]
check-all-targets:
    {{manifest}} check-all-targets --no-tests -- cargo check --workspace --all-targets

# The other half of #220. `check-all-targets` proves every `tests/` target
# COMPILES; until this recipe nothing on the push path proved any of them
# PASSES, and `just standard` is where that was first heard. On 2026-09-22 that
# cost 18 commits a landing gate and two red cycles, and the break was
# attributed to a commit two batches old.
#
# Not `cargo test --workspace` on the push path, and not a hardcoded file
# either. What runs here is one declared class, stated in
# scripts/source-invariant-targets.txt: the target's subject is a FILE IN THIS
# TREE -- a document, a config, a catalogue, the text of a source file -- it
# reads that file and asserts a property of what it read, and it starts no
# process, opens no socket, touches no device and finishes in milliseconds.
# Such a target goes red from an edit that compiles nothing, which is exactly
# what a `--lib` gate cannot see. A target whose subject is the CODE stays out,
# fast and pure or not; that boundary is what keeps this from becoming the
# workspace run under another name.
#
# COST, measured on schneckenschreck 2026-09-22, 11 targets / 55 tests:
#   warm  (every binary up to date)              1.9 s
#   after a batch touched all 11 test sources    3.6 s
#   first run on a tree that has just built the
#     REST of `fast` (links the lnflash and
#     leviculum-ffi test-profile dep trees,
#     which no other Tier-0 step builds)        ~23 s, once
# Of the warm 1.9 s, ~0.9 s is the three `cargo test` invocations and ~0.95 s
# is the manifest wrapper -- hence ONE wrapper around the whole step rather
# than one per package, which measured 3.9 s for the same 55 names.
[doc('Run the tests/ targets that assert on files in this tree')]
source-invariant-tests: check-source-invariant-census
    {{manifest}} source-invariant -- python3 scripts/run-source-invariant-tests.py

# The guard that keeps the list above from drifting: every `test`-kind target
# cargo reports must carry a `run` or `skip` verdict with a stated reason, and
# a target in neither fails naming the file. Same shape and same reason as the
# ignored-tests census (#191c): a hand-picked run list covers the files it named
# on the day it was written and nothing added afterwards, so the mechanism has
# to be that FORGETTING is what goes red. ~0.05 s, one `cargo metadata`.
#
# It cannot check that an admitted target meets the class criterion -- a
# spawned daemon behind a helper module is invisible to it. The criterion is
# held by the one-line reason beside each entry, in a diff, read by a person.
[doc('Check every tests/ target has a run-or-skip verdict')]
check-source-invariant-census:
    @python3 scripts/check-source-invariant-census.py

# The two halves of the tree-hygiene check, #295. A gate must not modify the
# tree it runs in: `just fast` -> `fuzz-regress` rewrote a stale
# `leviculum-std/fuzz/Cargo.lock` in the landing gate's shared push tree on
# 80c11aae and stayed green over it; a gate refuses to run in a dirty tree, so
# the next one stopped with rc=5 on 3b1bf00e and no log named the run that had
# written the file.
#
# The pair is deliberately NOT "assert the tree is clean". The push tree is
# clean before a gate starts, but a developer's checkout is not, and a tier
# that went red over its author's uncommitted work would be switched off
# within a week. So `tree-snapshot` records the dirt that was already there
# and `check-tree-clean` reports only the DELTA, by path and by content hash.
#
# They are wired into `fast` -- snapshot as its first dependency, the verify as
# the last line of its body -- because that is the tier that ran the offending
# recipe, and every later tier reaches it through `fast`. Run by hand,
# `check-tree-clean` with no snapshot compares against a clean tree and says
# so on its BASELINE line.
[doc('Record which tracked files were already dirty before a tier')]
tree-snapshot:
    @bash scripts/check-tree-clean.sh --snapshot

[doc('Report any tracked file a tier modified, by path and content')]
check-tree-clean:
    @bash scripts/check-tree-clean.sh --verify

# Fixture test for that pair, on the push path for the same reason the fuzz
# selftest is: a check for "the gate dirtied its tree" that has never been
# watched fire is a comfort, not a guardrail. Every case injects the damage
# into a throwaway checkout -- the #295 lockfile rewrite through a real
# justfile whose steps run in the same order `fast`'s do, an already-dirty
# tree that must stay green, a second edit to an already-modified file that
# only the content hash can see. ~2 s.
[doc('Drive the tree-hygiene check against an injected dirty file')]
tree-clean-selftest:
    bash scripts/test-tree-clean.sh

# The gate a coder pass runs beside `cargo fmt`, `cargo clippy -D warnings` and
# `cargo test --workspace`: every guard, census and selftest in `fast` that is
# cheap enough to pay on every batch, and nothing else. No body of its own --
# the dependency list IS the recipe, so `guards` and `fast` can be diffed
# against each other by eye.
#
# WHY IT EXISTS. These recipes are the ones a coder pass never ran and the
# landing gate did, so they went red an hour after the work was finished, on a
# host nobody was watching: #310 on `check-supervised-spawns`, #316 on
# `check-source-invariant-census`, both on 2026-09-26, an hour of gate each.
# The third came free: `check-source-invariant-census` was already red on
# e69ca01f when this recipe was written, over `identity_persists` added the
# commit before, and `just guards` is what found it.
#
# WHAT IS IN IT. #316 timed `just fast` recipe by recipe on a warm target:
# 761 s in total, of which four recipes carry 68 % (`mvr` 209 s,
# `fuzz-regress` 110 s, `i686-usize-gate` 66 s, `nrf-stack-frames` 50 s) and
# the whole census/guard/selftest block below is about 20 s. Table in the pass
# report, 20260926-215512-316. The 36 recipes here measured 19.7-20.3 s over
# three warm runs and 20.4 s on the first run after a leviculum-core edit --
# no cold-start penalty at all, which is what dropping `nrf-store-gap` below
# bought: with it in, the same two numbers were 20.0 s and 70.3 s.
#
# THE MEMBERSHIP RULE, for whoever adds the next guard: a recipe belongs here
# iff it (a) costs under ten seconds STANDING ALONE after a source edit, (b)
# compiles no part of the workspace for the host, and (c) drives no test target
# of its own. `m0-build-gate` and `lxmf-embedded-gate` are in because their
# compile is one crate for a foreign triple, 0.4 s and 0.5 s measured with
# leviculum-core freshly touched -- (b) is about the workspace build, not about
# cargo. `check-processor-seam` (8.2 s) is under the ten but fails (c), and
# `source-invariant-tests` (18.8 s) fails both (a) and (c) -- but its census
# half is 0.05 s and passes all three, so the census is here and the run is
# not. Without that split the recipe would catch #310's red and not #316's.
#
# "STANDING ALONE" is where the subset #316's report proposed had to be cut by
# one. `nrf-store-gap` reads the linked firmware ELFs and in `fast` it is
# preceded by `nrf-stack-frames`, which builds them; #316 therefore timed it at
# 0.4 s. `guards` excludes `nrf-stack-frames` (50 s), so here the gap gate pays
# for the firmware link itself: 40.3 s with leviculum-core touched, twice the
# whole rest of this recipe, for a guard whose product is a trend NUMBER and
# not one of the two reds above. Out. It stays in `fast`, where it is free.
# Every other member was re-measured the same way (`touch
# leviculum-core/src/lib.rs`, then the recipe alone) and none of them
# free-rides: the worst are `nrf-shellcheck` 5.6 s, `check-supervised-spawns`
# 3.1 s, `nrf-fw-readback` 2.4 s. `check-fuzz-lock` (#442) came later: 0.11 s,
# `cargo metadata --locked` over the two fuzz crates, the lock half of
# `fuzz-regress` (110 s, below). `loquax-core-no-std` (#232) is in on the
# same ground as `m0-build-gate`: one crate for the firmware triple, 0.76 s
# alone after `touch leviculum-core/src/lib.rs`, 0.15 s warm (2026-10-25);
# `map-core-no-std` (#474) likewise, 1.5 s and 0.08 s (2026-10-27);
# `lnomad-core-no-std` (#475) is in on (a) to (c) too: two foreign triples and
# a temp copy's check for its control, 7.4 s standing alone after `touch
# leviculum-core/src/lib.rs`, 4.0 s warm (2026-11-02, with the esp fork).
# `no-printf-grep-pipes` (1340, #843) is a text scan over the tracked shell
# files: 0.3 s, no build, no test target (2026-11-02).
# `m9-render-no-std` (1351, 1363, #232, #473) on the same three grounds: the
# M9's renderer and client for both firmware triples and one temp copy's checks
# for their two controls, 7.7 s warm with the esp fork (2026-11-03), most of it those.
#
# NOT a tier. Tiers nest (`standard` contains `fast`); `guards` is a strict
# subset of `fast` and adds nothing to it, so a green `guards` is a cheap
# early verdict on part of Tier 0, never a substitute for it. The land gate
# still runs `fast` and `standard`.
#
# CHECKED, NOT PROMISED. All of the above was true the day it was written and
# nothing kept it true: `check-guards-subset` (in this list, below) reads
# `just --dump` and refuses a `guards` member `fast` never reaches, a member
# the two lists run in different orders, and a member of `fast` that is in
# neither `guards` nor the ledger below. The cost clause is the one thing it
# cannot read, so the ledger is a forced decision instead: whoever puts a
# recipe on the push path either puts it in `guards` or writes the line saying
# why not. Reasons carry the measurement they rest on -- #316's recipe-by-
# recipe timing of `fast`, or a standalone run after `touch
# leviculum-core/src/lib.rs`.
#
# not-in-guards: check-processor-seam -- 8.2 s, under the ten, but it drives a
#   compile-fail test target of its own (clause c)
# not-in-guards: mvr -- 209 s (#316), the mvr suite itself
# not-in-guards: build-integ-bins -- the integ binaries mvr runs against; the
#   push path reaches it through mvr, nothing else does
# not-in-guards: supervised-spawn -- a cargo test target (clause c), ~10 s; its
#   census half check-supervised-spawns is in `guards`
# not-in-guards: lint-nrf -- cargo clippy over three firmware feature sets,
#   plus the host members' clippy/test and the rustdoc gate for both halves
#   (#367): minutes cold, and the embedded toolchain is a precondition
# not-in-guards: lint-lr11xx -- clippy, tests and rustdoc of a separate workspace plus a thumbv7em cross-check, not a guard
# not-in-guards: nrf-stack-frames -- 50 s (#316), it links the firmware ELFs
# not-in-guards: nrf-store-gap -- 0.4 s in `fast` only because nrf-stack-frames
#   linked those ELFs first; 40.3 s standing alone (74ec30ed)
# not-in-guards: nrf-flash-budget -- 13 s in `fast` with the images already
#   linked by nrf-stack-frames (measured 2026-11-02), and it relinks all four
#   standing alone. RUN IT BY HAND INSTEAD whenever a pass touches firmware
#   code: `just nrf-flash-budget`. The Pocket image went from 32 B free to 56 B
#   short with no coder gate noticing (1300).
# not-in-guards: hw-witness -- 33.5 s standing alone (measured 2026-09-27)
# not-in-guards: fuzz-selftest -- ~16 s against a throwaway fuzz crate
# not-in-guards: fuzz-regress -- 110 s (#316)
# not-in-guards: notices-guard -- ~20 s over both lockfiles via cargo-about
# not-in-guards: doc-gate -- cargo doc over the whole workspace
# not-in-guards: doc-touched -- 9.9-11.8 s standing alone after `touch
#   leviculum-core/src/lib.rs` with this lane's six touched crates
#   (measured 2026-09-27), and it checks host workspace crates
# not-in-guards: core-no-tracing -- a cargo test target (clause c)
# not-in-guards: lnsd-minimal -- cargo test targets (clause c) and a second
#   feature set of leviculum-std and leviculum-cli to build (clause b)
# not-in-guards: event-catalog -- a cargo test target (clause c) and a host
#   build of leviculum-std (clause b): 0.4 s warm, 33 s standing alone after
#   `touch leviculum-core/src/transport.rs` (measured 2026-10-11). RUN IT BY
#   HAND INSTEAD whenever a pass adds or renames an `event = "` literal
#   anywhere in the workspace: `just event-catalog`. 631 added
#   ANN_QUEUE_STALE in leviculum-core, ran that crate's tests green, and the
#   red arrived on the land gate on 38ed5816.
# not-in-guards: lnomad-core-no-tui -- clippy over lnomad without its `tui`
#   feature, a host build of leviculum-std (clause b): 8.5 s standing alone
#   after touching lnomad/src/app/, 8.6 s after leviculum-core (measured
#   2026-10-25). Its static half, lnomad-core-no-tui-static, is in.
# not-in-guards: ui-no-terminal -- clippy over leviculum-ui (clause b), a
#   build; its static half, ui-no-terminal-static, is in.
# not-in-guards: i686-usize-gate -- 66 s (#316), the core suite on a second triple
# not-in-guards: no-atomic64-gate -- a cargo check on a third triple, which it
#   installs itself
# not-in-guards: check-all-targets -- compiles every target in the workspace
# not-in-guards: deb-features-check -- cargo check of every package's feature
#   set on its release triple (1593): 9.5 s after `touch
#   leviculum-core/src/lib.rs`, 3 min 35 s cold (measured 2026-11-09), and
#   two release feature sets of its own (clause b). RUN IT BY HAND INSTEAD
#   whenever a pass touches a `cfg(feature` boundary, a CLI args struct or a
#   `Cargo.toml` feature list: `just deb-features-check`. 1570 added
#   `GuiArgs::no_start` and loquax under `tui gui` on the gnu triple stayed
#   uncompilable from 902bc129 to cbbd870d with every coder gate green.
# not-in-guards: deb-floor-check -- links the desktop loquax and lnsd with
#   cargo zigbuild at the packages' glibc floor (1674): 23.4 s after `touch
#   leviculum-std/src/proc_memory.rs`, 1.4 s with nothing changed, 3 min 40 s
#   cold (measured 2026-11-11), and two release links (clause b). RUN IT BY
#   HAND INSTEAD whenever a pass adds or changes a `libc::` call or touches
#   `leviculum-std/src/proc_memory.rs`: `just deb-floor-check`. 42cae4a2 put
#   `mallinfo2` (glibc 2.33) into a path the desktop loquax links, every
#   check passed, and the nightly packages failed to link for three days.
# not-in-guards: citation-guard -- a cargo test target (clause c), and over the
#   ten besides: 18.65 s for the bare-anchor test alone and 20.0 s for the
#   recipe (measured 2026-09-27; the other thirteen tests in the binary run
#   beside it and cost nothing extra). So it fails both clauses and there is no
#   subset to buy out with -- selecting only the two tests that catch the drift
#   still pays the 18.65 s one. RUN IT BY HAND INSTEAD, before committing,
#   whenever a pass edits this Justfile, anything under `scripts/`, or any
#   other file the corpus cites: `just citation-guard`, or the fixer
#   (`LEVICULUM_CITATION_FIX=1 LEVICULUM_CITATION_FIX_BASE=HEAD cargo test -p
#   leviculum-std --test doc_citations`) once and last, then the guard for the
#   verdict (docs/src/concepts/checks-and-citations.md:600).
#   WHY, and it is the same hole `doc-touched` above was added to close for
#   rustdoc: inserting a line into this file displaces every cited span below
#   it, and nothing else a coder pass runs reads a citation. 50b609fd added
#   three lines to the flash recipes and left seventeen citations behind
#   (c06112af); 2f78fc5b added `doc-touched` thirty-three lines above this one
#   and left twenty (47992075). Both passes ran fmt, clippy, `cargo test` and
#   `just guards` green, and both reds arrived on the land gate with the lane
#   queued behind them. Scoping the anchor pass to `git diff --name-only` would
#   be cheap enough -- 2.0 s of `git blame` over the 67 citing files that hold
#   a citation into this lane's 153 changed files, against 18.65 s for the
#   whole test -- but it would have to be a second implementation of the
#   anchor rule in a script, and finder and fixer are one implementation here
#   on purpose (docs/src/concepts/checks-and-citations.md:428)
# not-in-guards: tmp-leftovers -- the whole `cargo test --workspace`, about
#   ten minutes (730). Its subject is every test binary, so it has no subset.
# not-in-guards: source-invariant-tests -- 18.8 s and a test runner of its
#   own; its census half check-source-invariant-census is in `guards`.
#   RUN IT BY HAND INSTEAD, before committing, whenever a pass edits a file
#   one of these targets reads as TEXT: the firmware sources under
#   `leviculum-nrf/src/` (lnode_debug_log_format, rak4631_gnss_pulse_pin),
#   the book under `docs/src/`, SECURITY.md, the board catalogue, the
#   build scripts -- `just source-invariant-tests`. A firmware-touching
#   pass runs it always, because the whole deferral and arming instrument
#   lives in one of these targets.
#   WHY, and it is the third turn of the hole `doc-touched` closed for
#   rustdoc and `citation-guard` for citations: nothing else a coder pass
#   runs EXECUTES these targets, and a rename that compiles everywhere
#   turns them red. 10bffcf5 renamed the idle select's outgoing arm in
#   leviculum-nrf/src/lora.rs `data` -> `frame`; the deferral guard finds
#   that arm by its pattern text and failed on a tree whose invariant was
#   intact. That pass ran `lint-nrf`, the leviculum-core tests, `guards`,
#   `doc-touched` and `citation-guard` green -- this recipe is in none of
#   them -- and the red arrived on the land gate with 75 commits queued
#   behind it (#371, fixed in 242b69f6).
#   `just fast` is not the cheap way out of naming it: measured on
#   schneckenschreck 2026-09-27 on a warm target dir, `fast` was still
#   inside `fuzz-regress` -- dependency 43 of 53 -- when it passed eight
#   minutes, with i686-usize-gate, doc-gate, check-all-targets,
#   citation-guard, this recipe and the tier's own fmt, clippy and `--lib`
#   run all still ahead of it. The `~3.5 min` in the recipe's own doc
#   string is older than half the dependency list.
[doc('The coder-pass gate: every guard in `fast` that costs under 10 s')]
guards: tree-snapshot tree-clean-selftest check-submodules check-third-party check-trailers check-integ-bin-list check-ci-pipeline check-ci-secrets publish-selftest nightly-green-selftest check-publish-nightly-gate package-selftest site-publish-selftest site-unpack-docs-selftest deb-stamp-selftest version-guard lock-contention-selftest toolchain-status-selftest sweep-selftest tmp-leftovers-selftest window-bench-selftest dfu-exit-selftest m9-walk-selftest btvirt-selftest check-firmware-images check-plain-clone check-supervised-spawns check-core-lock-census check-env-knobs check-test-ports test-retry-guard no-printf-grep-pipes check-ignored-source check-just-docs check-guards-subset check-standard-integ check-python-test-prerequisites check-config-table-citations prepush-guard nrf-evt-max-size nrf-gap-device-name nrf-board-pins nrf-sd-guard nrf-uf2-volumes nrf-fw-readback rnode-chip-offsets nrf-shellcheck embassy-pins log-diff-mask-selftest firmware-no-hal firmware-no-chip-cfg nrf-module-reach check-fuzz-lock changelog-links loquax-name m0-build-gate lxmf-embedded-gate loquax-core-no-std map-core-no-std lnomad-core-no-std m9-render-no-std lnomad-core-no-tui-static ui-no-terminal-static check-source-invariant-census

# Tier 0 (~3.5 min, runs on every git push): submodule pins + commit-message
# trailers + the single-integ-bin-list guard (#310)
# + fmt + clippy (host + nrf) + rustdoc gate + tracing-shim + M0
# gates + a compile check of every workspace target (#220) + a RUN of the
# `tests/` targets whose subject is a file in this tree (the other half of
# #220) + workspace lib
# tests + the core suite on a 32-bit `usize` (#303) + a check of leviculum-std
# on a target with no 64-bit atomic (#415 — a separate property from pointer
# width: i686 is 32-bit and has one) + the citation guard +
# the third-party notice guard (#288) + the process-supervision pair (census
# over the sources, proof against the kernel) + the #[ignore]d census counted
# from the sources (#191c, the binary count of it is in `standard`)
# + the release gate's two halves
# (#312: the nightly green-ref signal, and the wiring that keeps a red
# rnsd_interop out of the publish step)
# + the tree-hygiene pair (#295: the snapshot that opens the tier, the verify
# that closes it, and the fixture test that has watched it fire).
#
# notices-guard sits after lint-nrf deliberately: it reads the firmware
# workspace `--frozen`, and lint-nrf is what guarantees that workspace's git
# dependencies are fetched by the time it runs.
#
# `clippy --all-targets`, matching ci-gate below. Without it this line lints
# libs and bins only, so a lint that fires solely in test code is invisible on
# the push path: `clippy --workspace --all-targets` was red for three days in
# August 2026 (an `assertions_on_constants` in a #349 test, fixed in c746bf8)
# while every per-batch and pre-push run of this recipe stayed green. The
# `check-all-targets` dependency compiles those targets but does not lint
# them, which is exactly the gap.
[doc('Tier 0 (~3.5 min): the gate every git push runs')]
fast: tree-snapshot tree-clean-selftest check-submodules check-third-party check-trailers check-integ-bin-list check-ci-pipeline check-ci-secrets publish-selftest nightly-green-selftest check-publish-nightly-gate package-selftest site-publish-selftest site-unpack-docs-selftest deb-stamp-selftest version-guard lock-contention-selftest toolchain-status-selftest sweep-selftest tmp-leftovers-selftest window-bench-selftest dfu-exit-selftest m9-walk-selftest btvirt-selftest check-firmware-images check-plain-clone check-supervised-spawns check-core-lock-census check-env-knobs check-test-ports test-retry-guard no-printf-grep-pipes check-ignored-source check-just-docs check-guards-subset check-standard-integ check-python-test-prerequisites check-config-table-citations prepush-guard check-processor-seam mvr supervised-spawn lint-nrf lint-lr11xx nrf-stack-frames nrf-store-gap nrf-flash-budget m9-reproducible nrf-reproducible nrf-evt-max-size nrf-gap-device-name nrf-board-pins nrf-sd-guard nrf-uf2-volumes nrf-fw-readback rnode-chip-offsets nrf-shellcheck embassy-pins log-diff-mask-selftest firmware-no-hal firmware-no-chip-cfg nrf-module-reach hw-witness check-fuzz-lock fuzz-selftest fuzz-regress notices-guard doc-touched doc-gate changelog-links loquax-name core-no-tracing lnsd-minimal event-catalog m0-build-gate lxmf-embedded-gate loquax-core-no-std map-core-no-std lnomad-core-no-std m9-render-no-std lnomad-core-no-tui ui-no-terminal i686-usize-gate no-atomic64-gate check-all-targets deb-features-check deb-floor-check citation-guard source-invariant-tests tmp-leftovers
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    {{manifest}} workspace-lib -- cargo test --workspace --lib
    @# LAST, and after the three lines above rather than among the dependencies:
    @# the subject is everything this tier did, and `cargo test` writing a file
    @# into the tree would refuse the next gate exactly as the fuzz build did.
    @# (`@#`, because just echoes a bare comment line into the gate log as if it
    @# were a command, and this one is for the next author, not for the log.)
    @bash scripts/check-tree-clean.sh --verify

# The gate the forge runs: `.woodpecker/ci.yml` on every push (Codeberg #299)
# and `.woodpecker/nightly.yml` before it builds anything it publishes (#266).
# Until #266 nothing between a commit landing on master and a .deb appearing on
# the public releases page executed a single test, and until #299 nothing ran
# on an ordinary source commit at all: the pre-push hook is per-clone local
# config, and `--no-verify` skips it. Both pipelines reach this recipe through
# `scripts/ci-gate.sh`, which provisions the container they share.
#
# NOT an alias for `fast`, because `fast` cannot run in that pipeline's
# container, and not for want of a package:
#   check-submodules      — the pipeline clones with `submodules: false`
#                           (nightly.yml:168), so every pin is "missing".
#   lint-nrf              — leviculum-nrf is its own workspace, needs the
#                           thumbv7em target plus flip-link as its linker.
#   nrf-stack-frames      — reads a linked firmware ELF that is never built here.
#   nrf-evt-max-size      — resolves the firmware workspace's feature graph
#                           `--frozen`, so it needs that workspace's git
#                           dependencies already fetched.
#   m0-build-gate,
#   lxmf-embedded-gate    — thumbv6m / thumbv7em cross-compiles.
#   no-atomic64-gate      — an arm-linux-androideabi cross-check (#415). It
#                           installs its own target, so it would run here, but
#                           it would pay for that download on every container
#                           and the push path already carries it.
# Those keep running on the push path, which has the targets and the submodules.
# What is left is what a submodule-less host-target container can actually
# prove, and it is nearly all of the suite: fmt, clippy, a compile check of
# every workspace target (#220 — `--lib` gates were green on a tree where eight
# integration-test targets did not build), the workspace lib tests, and — since
# Codeberg #312 — the tests that are not in a lib target.
#
# THE TESTS THAT ARE NOT IN A LIB TARGET were the subject of #312, and they are
# not a rounding error: measured on this host with `cargo test --workspace` on
# 2026-09-26, 596 s and green, 5916 tests pass, of which `--lib` selects 4279.
# The other 1637 sat in 134 integration targets, 23 bin unittest targets and 16
# doctest units, and gated nothing here. `scripts/ci-gate-integ.sh` runs them,
# less three suites that read a `reference/` submodule this container does not
# clone; its header names each one, with what covers it instead (the tier-2
# nightly, whose green ref is what the publish gate reads). That script computes
# the target list from the tree rather than carrying a copy of it, because a
# written copy would go stale silently, which is #312's own shape.
#
# check-plain-clone is the one check-* that does belong here, and it goes
# first: it asserts the precondition the rest of this recipe rests on — that
# no crate needs a submodule to compile (#300) — costs one grep, and its
# failure message is worth more than the compiler's when the answer is "the
# fixture moved back into reference/".
#
# It is a recipe rather than four lines of YAML for the reason nightly.yml
# records at :156-159 for the .deb build: a second copy in the pipeline file
# drifts from the gate developers run, and the drift is found the same way.
#
# Measured cold (fresh rust:bookworm, empty target dir and cargo registry,
# 4 cores) on 2026-08-18: 2m12s for the four lines it had then, 3051 tests
# executed across 10 units. The step's provisioning — musl-tools, the rustfmt
# and clippy components, `cargo install just`, one shallow submodule — costs
# 1m11s on top, so the pipeline pays 3m23s to stop shipping untested .debs.
# The submodule fetch left that provisioning with #300; ~5 s and 27 MB less.
# Re-measured after the widening below, on schneckenschreck with an empty
# target dir but a warm cargo registry: 1m58s, 3057 tests across 10 units.
#
# What the #312 line adds to that is measured on the coder host rather than in
# the container: 596 s for all of `cargo test --workspace`, of which the interop
# suite this gate does not run is 182 s, so the test EXECUTION it adds is ~280 s
# on 10 cores. The container also has to link ~150 test binaries that clippy
# only ever compiled to metadata, which is the larger half of the bill and the
# one an interactive measurement here cannot state honestly — the first cold run
# on the runner is the number to write down, and it goes in this comment.
# Budgeting it against the runner's own limit is the point: a gate under ten
# minutes is cheaper than a red master nobody sees until the next nightly.
# THE RUNNER'S NUMBER, read off the Woodpecker API for the step `gate`:
# 965-1185 s cold over the cron pipelines 476-486 and 1174 s on push 488
# (2026-09-26 to 10-04), provisioning included. All of those were red runs,
# but they ran the whole selection to the end (`--no-fail-fast`), so they
# price the work; what they do not include is the release build below, which
# they never did. So the gate went from 3m23s to ~19 min cold, not to "under
# ten", and push 477 ended in exit 124 at 2388 s.
#
# `build-integ-bins` is a line here, not a dependency, so fmt and clippy still
# answer first. Four mvr tests (`lncp_fetch_rust_responder`,
# `unparsable_config_must_not_start_a_daemon`,
# `resource_consecutive_push_window_policy`, `media_silence_restore_signal`)
# spawn the RELEASE binaries periculum mounts, not the debug ones cargo test
# builds, and resolve them under the target dir (`release_bin` in each file).
# Without the build they failed on the forge naming the recipe (pipelines
# 476-488). It is the one list of those binaries (`check-integ-bin-list`), and
# the release profile is a second compile of the dependency graph, which is
# what it costs.
#
# `clippy --all-targets` and no separate `cargo check`: clippy compiles what
# check compiles, so the check line was a second pass over the same targets.
# The lint findings that kept clippy off test code until 2026-08-18 are fixed.
[doc('Forge gate: fmt, clippy, every workspace test not bound to a submodule')]
ci-gate:
    @bash scripts/check-plain-clone.sh
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    {{manifest}} ci-gate-workspace-lib -- cargo test --workspace --lib
    {{just_executable()}} build-integ-bins
    bash scripts/ci-gate-integ.sh
    {{just_executable()}} loquax-tui

# First run after a fresh CARGO_TARGET_DIR: 20-40 min. Nothing triggers this
# automatically: it is typed once per batch. A post-commit hook detached it
# after every commit until 2026-08-07 — a commit is not a unit anyone wants
# tested, and forty minutes is not a wait a commit can absorb. See
# docs/src/concepts/checks-and-citations.md.
# Tier 1 (~15 min): Tier 0 + core/tests + ffi (incl. C-program + Python interop)
# + proxy + rnsd_interop + the TCP-hub endurance smoke soak.
#
# Note on `--tests` targets: Tier 0 runs `--lib` only, so a crate's
# tests/ directory is covered here or nowhere. Until #408 this recipe named
# its suites one at a time and 94 of 139 targets were in no line of it;
# loquax's python_interop was red on master for eight commits while two land
# gates passed it (401). The lines below are the suites that need a flag, a
# `-p` feature set or a manifest of their own; scripts/standard-integ.sh runs
# every other target in the tree, computed rather than named, and
# scripts/standard-integ-elsewhere.txt is the list of what it leaves to these
# lines. check-standard-integ (in `guards`) keeps that list honest.
[doc('Tier 1 (~15 min): fast plus the integration suites')]
standard: fast test-ffi verify-packaging loquax-tui
    {{manifest}} core-tests -- cargo test -p leviculum-core --tests
    # ~4 s: node_integ builds an in-process daemon + IPC + blog node, which
    # is too slow for the Tier 0 push gate but trivial here.
    {{manifest}} lblogd-tests -- cargo test -p lblogd --tests
    {{manifest}} proxy -- cargo test -p leviculum-proxy
    # 7200, not the wrapper's 1800 (698): 207 s and 396 s are the honest runs
    # the manifests recorded, but under a quarter of a CPU the suite's 393
    # tests summed to 2131 s of shard wall (689's survey on c4d29b71, against
    # 355 s unconstrained), so a slow coder host exited 124 on a green suite.
    # 7200 is 3.4x that, the wrapper default's own headroom over an honest run,
    # and the ceiling workspace-all-targets already has. The forge does not run
    # this suite (38ec9635).
    {{manifest}} rnsd-interop --timeout 7200 -- cargo test -p leviculum-std --test rnsd_interop
    {{manifest}} event-log-subscriber -- cargo test -p leviculum-std --test event_log_subscriber -- --test-threads=1
    {{manifest}} event-log-multiprocess -- cargo test -p leviculum-std --test event_log_multiprocess
    # The LNode debug-log format contract (Codeberg #65): pins the [HEAP] and
    # [PANIC_COUNT] line shapes that catch-reboot.sh and the by-hand heap
    # analysis grep, against the firmware source that emits them.
    {{manifest}} lnode-debug-log-format -- cargo test -p leviculum-std --test lnode_debug_log_format
    # The LXMF helper end to end (#196): two `lxmf-node` processors over TCP
    # loopback exchange messages in both directions, plus the partitioned
    # negative control. ~11 s, no docker — this is the part of the helper's
    # evidence that does not need the rig, and Tier 0 runs `--lib` only, so a
    # crate's tests/ directory is covered here or nowhere.
    {{manifest}} lxmf-node -- cargo test -p leviculum-lxmf-node --test two_node_loopback
    # #408: every integration target no line above or in `fast` runs, the
    # reference/-bound suites included on a host that has the submodules
    # (scripts/integ-prerequisites.txt). Measured cost is in the script's
    # header.
    bash scripts/standard-integ.sh
    # Endurance gate (#101): builds lnsd, boots it as a hub, asserts 100%
    # delivery + RSS plateau + no fd leak. ~15 s smoke; `--full` is on demand.
    bash scripts/run-soak.sh
    # #191a: the only suite that drives lnsd AND the vendor Python rnsd
    # through one traffic script. ~196 s. Kept #[ignore]d and run by name
    # (serially, apart from the parallel suite) — see the script header.
    bash scripts/run-status-parity.sh
    # #191c: the #[ignore]d bucket is a pinned number per test unit, so it
    # cannot grow without a diff. Last, because it needs every workspace
    # test binary built and this tier has built most of them already.
    python3 scripts/check-ignored-counts.py

# Build the production binaries periculum mounts into its node containers. Explicit per-bin list avoids `--workspace --bins` which
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
[doc('Build the release binaries periculum mounts into its containers')]
build-integ-bins:
    find leviculum-cli/src leviculum-proxy/src leviculum-lxmf-node/src lnpnd/src loquax/src -name '*.rs' -exec touch {} +
    cargo build --release --bin lnsd --bin lnstest --bin lncp --bin lnstatus --bin lnprobe --bin lnpath --bin lora-proxy --bin lxmf-node --bin lnpnd --bin loquax

# Tier 2 (~30-90 min, on demand: `systemctl --user start
# leviculum-ci-tier2.service`): Tier 1 + the docker scenario suites.
#
# Runs periculum over its two tier-2 corpora: `conformance/` (portable
# shared-property scenarios that must be all green against any pair of
# implementations) and `regression/` (leviculum-specific product scenarios,
# IP-based, excluded from conformance by its admission rules). Both bind no
# physical device, so this tier needs docker and nothing else. `hardware/` is
# tier 3 and is not run here — periculum would report every one of its
# scenarios SKIPPED_INFRA, which is honest but says nothing.
#
# Exit-code contract: 0 = something ran and nothing was RED, 1 = at least one
# RED, 2 = usage/malformed/internal, 3 = nothing ran at all. The bare
# invocation therefore fails the recipe on 1, 2 and 3, which is what a tier
# gate wants.
# The run that covers everything BY CONSTRUCTION (Guarantee B,
# docs/src/concepts/checks-and-citations.md). Tiers define latency, not
# coverage: `fast` runs `--lib`, `standard` names packages one at a time, and
# 327 ordinary tests were consequently executed by no gate at all (Codeberg
# #194) — whole crates, not stragglers. Naming the missing ones is what
# produced the gap in the first place and loses something again with every new
# test file, so this selects nothing by name. Leaving a test out of the
# complete run is now what has to be declared.
#
# Two commands, because neither spelling covers everything alone. `--all-targets`
# runs every lib, bin and integration target and additionally compiles the six
# leviculum-std examples — but cargo DROPS doctests when it is given, and
# scripts/ignored-counts.txt tracks two doctest units, so `--doc` is its own
# invocation rather than a footnote. The workspace has no bench targets
# (`cargo metadata`, checked 2026-08-06), so `--all-targets` pulls in nothing
# that fails to compile.
#
# `--no-fail-fast` is load-bearing for the manifest, not a convenience: without
# it cargo stops after the first red binary and every later binary goes unrun,
# so a red run would emit a manifest covering a prefix of the workspace while
# looking like the whole of it.
#
# No `--test-threads=1`. It was needed for port contention, which the host-wide
# allocator (leviculum-std/tests/support/port_alloc.rs) removed, and for the
# event-log subscriber suite, which now takes its own lock. Measured green at
# default parallelism, 3669 tests + 28 doctests, 2026-08-06.
#
# Wall time on this host (4 cores, warm target dir): 7m50s + 3s. A fresh
# CARGO_TARGET_DIR adds a full workspace test build on top — which is why this
# one gate raises the wrapper's 1800 s default. 493 s is the longest run the
# manifests have recorded for it, so the default is comfortable warm and only
# marginal cold, and a backstop that fires on an honest cold build is a backstop
# people switch off. The nightly's `timeout 3600` around all of `just complete`
# stays the outer bound there; this is the inner one, so an interactive cold run
# is not cut off mid-build.
#
# 7200, not 3600: the land gate on bb628d67 compiled 1472 crates cold after the
# push-tree sweep and hit 3600 s (started 19:23, rc=124 at 20:22) with two coder
# passes building beside it; the ceiling only has to stop a hung test, which
# 7200 still does.
[doc('Run every test in the workspace, selecting nothing by name')]
complete:
    {{manifest}} workspace-all-targets --timeout 7200 -- cargo test --workspace --all-targets --no-fail-fast
    {{manifest}} workspace-doc -- cargo test --workspace --doc --no-fail-fast

[doc('Tier 2: standard, complete and the periculum docker corpora')]
extensive: standard complete build-integ-bins build-c-lnsd
    #!/usr/bin/env bash
    set -euo pipefail
    PERICULUM_ROOT="${PERICULUM_ROOT:-../periculum}"
    PERICULUM_BIN="${PERICULUM_BIN:-$PERICULUM_ROOT/target/release/periculum}"
    if [ ! -x "$PERICULUM_BIN" ]; then
        echo "[extensive] periculum binary missing - building in $PERICULUM_ROOT"
        # Pin the target dir so the binary lands where the PERICULUM_BIN
        # default expects it, even when the CI runner exports a global
        # CARGO_TARGET_DIR (run-tier2.sh does).
        (cd "$PERICULUM_ROOT" && CARGO_TARGET_DIR=target cargo build --release)
    fi
    "$PERICULUM_BIN" run "$PERICULUM_ROOT/conformance" "$PERICULUM_ROOT/regression"

# Tier 3 (~2-6h, 02:00 nightly): Tier 2 + the LoRa hardware corpus.
#
# Two steps periculum does not do itself. First the LNodes are flashed from
# HEAD and their [FW_BUILD] banner is read back, because periculum tests
# whatever firmware it finds and leaves board preparation out of scope on
# purpose; a run against stale firmware is meaningless, so an unverifiable
# board fails the tier while still letting the rest of the corpus run. Then
# periculum runs `hardware/`, whose scenarios bind a modem or a firmware node
# and report SKIPPED_INFRA — never RED — for any board this bench does not
# hold.
#
# The scheduled nightly goes through scripts/run-tier3-hw.sh instead, which
# adds the CI ledger, the repo sync and the USB device-vanish watchdog.
[doc('Tier 3: extensive plus the LoRa hardware corpus')]
nightly: extensive
    #!/usr/bin/env bash
    set -euo pipefail
    PERICULUM_ROOT="${PERICULUM_ROOT:-../periculum}"
    PERICULUM_BIN="${PERICULUM_BIN:-$PERICULUM_ROOT/target/release/periculum}"
    unverified=$(bash scripts/flash-lnodes-from-head.sh | awk '$1 == "FW_UNVERIFIED" { print $2 }' | paste -sd, -)
    rc=0
    "$PERICULUM_BIN" run "$PERICULUM_ROOT/hardware" || rc=$?
    if [ -n "$unverified" ]; then
        echo "[nightly] FIRMWARE UNVERIFIED: LNode(s) $unverified could not be confirmed to run HEAD" >&2
        echo "[nightly] the run tested UNKNOWN firmware on those boards" >&2
        rc=1
    fi
    exit "$rc"

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
[doc('Run the whole C API suite: Rust, C programs and Python interop')]
test-ffi:
    cargo build -p leviculum-ffi --target x86_64-unknown-linux-gnu
    {{manifest}} ffi -- cargo test-ffi

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
[doc('Run the C API under ASan, TSan and Miri')]
sanitize-ffi:
    RUSTFLAGS="-Zsanitizer=address" cargo +nightly test -p leviculum-ffi -Zbuild-std --target x86_64-unknown-linux-gnu --test ffi_unit --test ffi_integration --test ffi_property -- --test-threads=1
    RUSTFLAGS="-Zsanitizer=thread" TSAN_OPTIONS="halt_on_error=0 suppressions={{justfile_directory()}}/leviculum-ffi/tsan-suppressions.txt" cargo +nightly test -p leviculum-ffi -Zbuild-std --target x86_64-unknown-linux-gnu --test ffi_integration -- --test-threads=1
    MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test -p leviculum-ffi --test ffi_unit identity
    MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test -p leviculum-ffi --test ffi_unit hex
    MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test -p leviculum-ffi --test ffi_unit destination

[doc('Build libleviculum as a cdylib and staticlib for C consumers')]
build-ffi:
    cargo build-ffi

# Verify libleviculum installs and links like a standard Unix C library: a
# staged `make install` produces the SONAME symlink chain, header, static
# archive, and pkg-config file, and a consumer compiles, links, and runs
# against it purely through pkg-config, both dynamically and statically.
# Catches a renamed export breaking the header, a wrong .pc, a missing soname,
# or a load failure. Part of Tier 1.
[doc('Check libleviculum installs and links like a standard C library')]
verify-packaging:
    bash scripts/verify-packaging.sh

# Same end-to-end packaging check for the aarch64 cross build: cross-compiles
# the consumer and runs it under qemu. Skips cleanly if the cross toolchain
# (rustup target + gcc-aarch64-linux-gnu + qemu-user-static) is absent.
[doc('Same packaging check for the aarch64 cross build')]
verify-packaging-arm64:
    bash scripts/verify-packaging.sh aarch64-unknown-linux-gnu

# Same for ARM64. Requires `sudo apt install gcc-aarch64-linux-gnu` and
# `rustup target add aarch64-unknown-linux-gnu` on the build host.
[doc('Same C API build for aarch64')]
build-ffi-arm64:
    cargo build-ffi-arm64

# Build the C daemon (examples/c/lnsd.c) as a self-contained binary, linking
# libleviculum.a statically (glibc stays dynamic, matching the debian-slim
# node container). Output: target/release/c-lnsd, the binary periculum mounts
# for a node whose adapter is `c-lnsd`.
[doc('Build the C daemon example against the static C API library')]
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

# Pin the build ID + per-package DEB versions once and persist them, so
# an amd64 + arm64 pair from a single `just build-deb` run carries
# identical version strings (no midnight-UTC drift between the two
# builds). Shared with nightly.yml, which calls the same script: this
# used to be duplicated shell in both places and the two drifted.
_deb-stamp:
    @bash scripts/deb-stamp.sh

# amd64 musl-static .debs for every package but lnflash: leviculum (the
# daemon and its clients), lnomad (the browser), lblogd (the blog server),
# lnpnd (the propagation node) and loquax (the messenger, with its tui).
# Binaries come from the workspace musl target, so they are fully static
# and run on Debian >= 9 / Ubuntu >= 16.04 regardless of host glibc.
#
# The build itself lives in scripts/build-deb.sh, which nightly.yml calls
# too: the sequence used to be written out in both places and drifted,
# stalling the nightly release for eight days (see the script's header).
# Output: target/debian/*_amd64.deb, hardlinked by cargo-deb under
# target/<triple>/debian/ as well.
[doc('Build the amd64 musl-static .debs')]
build-deb-amd64: (_require-cargo-deb) _deb-stamp
    @bash scripts/build-deb.sh amd64

# arm64 musl-static .debs via cargo-zigbuild (Zig as the cross
# compiler/linker — the only way to reach aarch64-musl from an amd64 host
# without docker-in-docker or an arm64 runner). Requires cargo-zigbuild +
# ziglang on PATH; `pip install ziglang` provides a self-contained Zig
# the zigbuild wrapper finds, or install a full Zig distribution (the
# bare zig binary without its sibling lib/ fails at `zig cc` with "unable
# to find zig installation directory").
[doc('Build the arm64 musl-static .debs via cargo-zigbuild')]
build-deb-arm64: (_require-cargo-deb) _deb-stamp
    @bash scripts/build-deb.sh arm64

# armhf (armv7, hard float) musl-static .debs, the same way as arm64: for a
# Raspberry Pi 2, 3 or Zero 2 under a 32-bit OS (1153). Not yet published by
# the nightly, which ships amd64 and arm64.
[doc('Build the armhf (armv7) musl-static .debs via cargo-zigbuild')]
build-deb-armhf: (_require-cargo-deb) _deb-stamp
    @bash scripts/build-deb.sh armhf

# 1593: every package this host's nightly would build, compiled with exactly
# the target triple and feature set scripts/build-deb.sh builds it with, by
# that script's own `--check` mode, so the two cannot drift: `cargo check
# --release`, nothing linked, nothing packaged, no stamps. loquax under `tui
# gui` on the gnu triple did not compile on master from 902bc129 to cbbd870d
# (1570 added `GuiArgs::no_start` and not the two initialisers in `gui_args`)
# and no coder gate saw it: `loquax-gui` checks `gui` alone and `loquax-tui`
# `tui` alone. In `fast`, not in `guards` (see the ledger there). RUN IT BY
# HAND INSTEAD whenever a pass touches a `cfg(feature` boundary, a CLI args
# struct or a `Cargo.toml` feature list.
[doc('cargo check every .deb feature set on its triple (host arch)')]
deb-features-check:
    bash scripts/build-deb.sh --check

# Link what the packages link against glibc, at the floor they promise
# (GLIBC_FLOOR in scripts/build-deb.sh, 2.31: Debian 11, Ubuntu 20.04), by
# that script's own `--floor` mode: the desktop loquax with the package's
# feature set and lnsd on the same gnu triple. deb-features-check above
# links nothing, so a glibc symbol newer than the floor passes it and fails
# only in the package build: 42cae4a2 called `mallinfo2` (2.33) from the
# census the mailbox pulls into loquax, and every nightly package build from
# 2026-11-08 on died in `ld.lld: error: undefined symbol: mallinfo2` (1674).
# Needs cargo-zigbuild and ziglang (`just _deb-prereqs`). In `fast`, not in
# `guards` (see the ledger there). RUN IT BY HAND INSTEAD whenever a pass
# adds or changes a `libc::` call or touches leviculum-std/src/proc_memory.rs.
[doc('link the desktop loquax and lnsd against the glibc floor of the packages')]
deb-floor-check:
    bash scripts/build-deb.sh --floor

# The heap-accounting variants of the leviculum package (#437) and of the
# lnpnd package (550), for one architecture. Both build-deb-* recipes produce them
# beside the plain packages, because scripts/build-deb.sh always does; this
# recipe exists so asking for the instrumented builds by name does not mean
# knowing that; the script's closing `produced:` lines name the packages.
# The nightly publishes the arm64 leviculum-heapacct only.
[doc('Build the heap-accounting leviculum and lnpnd .debs (plain too)')]
deb-heapacct arch="amd64": (_require-cargo-deb) _deb-stamp
    @bash scripts/build-deb.sh {{arch}}

# The lnflash .deb (1000): the firmware bundle `just lnflash-bundle` builds,
# packaged with the binary in /usr/bin and the images under /usr/share/lnflash,
# where lnflash looks last. amd64 only, like the bundle. The script unpacks the
# finished package and lets the packaged binary check the packaged bundle.
[doc('Build the lnflash firmware bundle and package it as a .deb')]
lnflash-deb: (_require-cargo-deb) _deb-stamp
    @bash scripts/lnflash-bundle.sh
    @bash scripts/build-lnflash-deb.sh

# Build every .deb but lnflash's in one go (that one needs the firmware
# toolchain: `just lnflash-deb`). _deb-stamp runs first (a dependency of each
# child), so all the packages share one build-id and a consistent set of
# per-package versions.
[doc('Build the .debs, all three architectures, on one shared build id')]
build-deb: build-deb-amd64 build-deb-arm64 build-deb-armhf

# The nightly release, built and published from this tree (517). Runs on the
# tier-2 nightly host after a green-or-attributed night, with
# LEVICULUM_PUBLISH_FROM_GREEN_RUN=<the sha it just tested>, and refuses a HEAD
# no green night covers. Both .deb architectures, the lnflash bundle, the
# book, dist/, then the site; the forge release only with
# LEVICULUM_PUBLISH_FORGE=1, as the forge's cron publishes it (726). A rerun
# on one build id republishes dist/ without rebuilding, and
# LEVICULUM_PUBLISH_DRY_RUN=1 uploads nothing. Credentials are files (see the
# script's header): ~/.config/leviculum-site/{id,known_hosts}, for the forge
# ~/.config/codeberg-token. The flow is in docs/src/development-ci.md.
[doc('Build the nightly from this tree and publish it to the site')]
nightly-publish:
    @bash scripts/nightly-publish.sh

# Structural check on the built .debs: metadata, per-package versions,
# file layout, conffiles, unit validity, maintainer-script syntax. Needs
# `just build-deb` (or at least build-deb-amd64) to have run. Root not
# required — nothing is installed.
#
# Deliberately not part of any test tier: it presupposes a build-deb run,
# which does a `cargo clean` on three crates and cross-builds for three
# targets. That is minutes of rebuild plus a zig toolchain, which does
# not belong in the 15-minute Tier 1 budget. Run it by hand whenever
# packaging changes, together with a real install test in a systemd
# container — the structural checks here cannot see a service that
# installs cleanly and then fails to start.
[doc('Check the built .debs structurally: metadata, layout, units')]
verify-deb:
    @bash scripts/verify-deb-packaging.sh

_require-cargo-deb:
    @cargo deb --version >/dev/null 2>&1 || (echo "cargo-deb not found — run: just _deb-prereqs (or cargo install cargo-deb)" && exit 1)

# Best-effort, idempotent install of the cross-build toolchain the
# build-deb* recipes need: the musl and glibc rustup targets, cargo-deb, and
# cargo-zigbuild + ziglang for the cross-links and loquax's glibc build. Safe to re-run.
_deb-prereqs:
    rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl armv7-unknown-linux-musleabihf x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu
    cargo install --locked cargo-deb
    cargo install --locked cargo-zigbuild
    @echo "[_deb-prereqs] also ensure ziglang is available for arm64:"
    @echo "    pip install ziglang   (or install a full Zig distribution on PATH)"

[doc('Status of last runs across all tiers')]
status:
    @bash scripts/ci-status.sh

# For other tiers: ls ~/.local/state/leviculum-ci/ and pick a file.
[doc('Tail the most recent Tier 1 log, live if a run is in progress')]
logs:
    @bash -c 'LOG=$(ls -t ~/.local/state/leviculum-ci/tier1-*.log 2>/dev/null | head -1); \
        if [ -z "$LOG" ]; then echo "No Tier 1 log yet."; exit 1; fi; \
        echo "==> $LOG"; tail -f "$LOG"'

# Idempotent; safe to re-run after pulling.
[doc('Install the git hooks and systemd timers of the 4-tier CI pipeline')]
install-ci:
    bash scripts/install-ci.sh

_require-cargo-sweep:
    @cargo sweep --version >/dev/null 2>&1 || (echo "cargo-sweep not found -- run: cargo install --locked cargo-sweep (or just install-ci)" && exit 1)

# Codeberg #381: a day of gate runs writes well over a hundred gigabytes into
# these target directories and cargo removes none of it. Every changed input
# adds a hash-suffixed artefact NEXT TO the old one, so the directory only
# grows: measured on the CI host 2026-09-24, 2705 files and 27 GB in
# `target/x86_64-unknown-linux-musl/debug/deps` alone, of a 36 GB tree, and
# 137 GB on the day the host's root volume filled and refused work.
#
# `cargo clean` is the blunt answer and costs a full rebuild of everything.
# `cargo sweep --maxsize` drops the OLDEST artefacts until the directory fits
# the budget, which keeps the ones the next build would reuse. The budgets are
# caps measured on that host, not targets: a tree already under one is left
# alone, and both are parameters.
#
# Both workspaces, because this repository has two -- the host one at the root
# and the firmware one in leviculum-nrf -- with a target directory each, and
# sweeping the root leaves the firmware's untouched (6.2 GB on the same day).
# Where those directories LIE is asked, not assumed: cargo-sweep resolves the
# path through cargo, so a tree that moved its artefacts with CARGO_TARGET_DIR
# (the CI tiers and the nightly do) is swept where they actually are. Verified
# 2026-09-24 against a CARGO_TARGET_DIR outside the tree.
#
# What this must never touch is the sccache directory: that cache is what
# makes the rebuild after a sweep cheap, and it bounds itself
# (SCCACHE_CACHE_SIZE).
#
# Three more target directories nest under the root's and are not the root
# workspace's to cargo-sweep, which sweeps by `cargo metadata` and so only the
# directory cargo names: the benches' BASE builds (window-bench, map-bench;
# 1.3 GB measured 2026-11-07, 1470) and the x86-64-v3 loquax build-deb.sh
# packages for amd64 (1469). Each is swept as the root workspace's under
# CARGO_TARGET_DIR, which `cargo metadata` honours (dry run verified
# 2026-11-07); one that does not exist is a warning, not a failure.
[doc('Trim the target directories to a budget, keeping the newest')]
sweep budget="30GB" fw_budget="4GB" side_budget="2GB": _require-cargo-sweep
    cargo sweep --maxsize {{budget}} .
    cargo sweep --maxsize {{fw_budget}} leviculum-nrf
    for dir in target/window-bench/base-target target/map-bench/base-target target/x86-64-v3; do CARGO_TARGET_DIR="{{justfile_directory()}}/$dir" cargo sweep --maxsize {{side_budget}} . || exit 1; done

# Runs the recipe above against a stub cargo, so the assertion costs no
# build and deletes nothing. ~1 s.
[doc('Drive the sweep recipe against a stub cargo, deleting nothing')]
sweep-selftest:
    @bash scripts/test-just-sweep.sh

# scripts/install-btvirt.sh without root, a network or a compiler (periculum
# #49). The ble_room cells run on an emulator whose BUILD decides their
# verdict -- bluez <= 5.82 stalls every room from the third node -- so that
# script patches bluez before building it and writes a sidecar each run
# prints as `origin=`. Both halves are otherwise only exercised by a real
# provisioning run on a bench with deb-src and a compiler, which is to say
# rarely. So --self-test drives the patch step against a source tree
# synthesised from the vendored patch (stock tree takes it, patched tree is
# left alone, unrelated tree refused, and the three post-image lines the fix
# turns on are asserted), and injects each way the sidecar can lie (none, no
# origin line, an origin not naming the patch, one older than the binary,
# one recording another md5) for the checker to refuse. Nothing is built,
# fetched or installed: /tmp scratch only. 0.12 s (measured 2026-09-27).
[doc('Drive the btvirt patch step and provenance check against injected damage')]
btvirt-selftest:
    @bash scripts/install-btvirt.sh --self-test

# Touch-free; double-tap RESET only if the runner prompts for a crashed
# device. Details: leviculum-nrf/README.md §Build and flash.
# The runner refuses a board whose SoftDevice our image is not linked for,
# because that write is a soft brick: docs/src/concepts/lnode-flashing.md.
# scripts/nrf-flash.sh builds at the fixed path of scripts/nrf-rustflags.sh
# (so a clone of the commit builds the same bytes, 1460), prints the image's
# NRF_IMAGE sha256, and runs tools/uf2-runner.sh outside the build's
# namespace on the ELF it built.
[doc('Flash every attached T114 with the current firmware')]
flash:
    bash scripts/nrf-flash.sh t114 bsp-t114

# Wiring, gdb setup and the probe's own quirks: docs/src/firmware/probe-debugging.md
[doc('SWD firmware debugging via the RPi Debug Probe')]
probe *args:
    ./scripts/probe-debug.sh {{args}}

# Useful for A/B testing (one T114 on new firmware, one on old).
#   just flash-one /dev/ttyACM3
#   just flash-one /dev/leviculum-transport
[doc('Flash a single T114 by port path or udev symlink')]
flash-one PORT:
    LEVICULUM_FLASH_ONLY={{PORT}} bash scripts/nrf-flash.sh t114 bsp-t114

# What every RAK4631 flash recipe tells the runner about this board: the USB
# PID our firmware enumerates on, the names it puts in its messages, and the
# one line that makes the manual double-tap prompt something a person can act
# on. The Pocket V2 has no externally accessible RESET, so the generic
# "double-tap RESET" sends its owner looking for a button that is not there
# (Codeberg #261). Declared once because three recipes share it and a hint
# that drifts between them is worse than none.
rak4631_env := 'LEVICULUM_USB_PID=0002 LEVICULUM_BOARD_NAME=RAK4631 LEVICULUM_UF2_BOARD_ID=WisBlock-RAK4631-Board LEVICULUM_DOUBLE_TAP_HINT="No RESET button on this case: double-tap the reset contact in the hidden pinhole beside the USB socket, with a needle (docs/src/firmware/recovery.md)."'

# First flash from Meshtastic / blank firmware needs a manual RESET
# double-tap (the stock app has no 1200-baud-touch handler). Subsequent
# flashes use the touch path automatically. The image is the baseboard one,
# the build lnflash ships: the bare-module build is no target any more (D5,
# Lew 2026-10-28), and a bare module runs this image unharmed
# (docs/src/concepts/board-support-scope.md).
[doc('Flash every attached RAK4631 (WisMesh Pocket V2) with our firmware')]
flash-rak4631:
    {{rak4631_env}} bash scripts/nrf-flash.sh rak4631 bsp-rak4631,rak-baseboard

#   just flash-rak4631-one /dev/ttyACM0
#   just flash-rak4631-one /dev/leviculum-rak-transport
[doc('Flash a single RAK4631 by port path or udev symlink')]
flash-rak4631-one PORT:
    LEVICULUM_FLASH_ONLY={{PORT}} {{rak4631_env}} bash scripts/nrf-flash.sh rak4631 bsp-rak4631,rak-baseboard

# Flash with all RAK19026 baseboard peripherals enabled — the WisMesh
# Pocket V2 build. `--features rak-baseboard` aggregates the three
# baseboard features (display, gnss, battery). This is the build the lnflash
# bundle ships for this board (docs/src/concepts/board-support-scope.md),
# and since D5 the one `flash-rak4631` flashes too; the name stays for the
# hands and scripts that know it.
[doc('Flash a RAK4631 with the WisMesh Pocket V2 baseboard peripherals')]
flash-rak4631-pocket:
    {{rak4631_env}} bash scripts/nrf-flash.sh rak4631 bsp-rak4631,rak-baseboard

# Which bootloader build a board carries: touch PORT (or "-" and double-tap
# by hand), mount the volume read-only, print INFO_UF2.TXT, give it back. The
# board stays in its bootloader afterwards; its own flash recipe brings the
# application back (a Pocket V2 does not leave it on a power cycle, the
# battery holds it). Run on the Pocket and the RAK before any update:
# docs/src/concepts/ota-stage-1-ble-dfu.md, "Bootloader update".
#   just bootloader-info /dev/leviculum-rak-transport
[doc('Print INFO_UF2.TXT of an LNode bootloader (- = double-tap by hand)')]
bootloader-info PORT:
    LEVICULUM_USB_PID=0002 bash scripts/lnode-bootloader.sh info {{PORT}}

# Lift a RAK4631's bootloader to the build in UF2 (RAK's update-*_nosd.uf2;
# 0.4.4 is vendored at lnflash/tests/fixtures/bootloader/rak4631/). The
# planner refuses an image that would stage outside the record log, the run
# reads INFO before and after, flashes our application as flash-rak4631-one
# does and asserts [IDENTITY] kept. PORT "-" means double-tap by hand and
# leaves the identity unverified. The procedure, the recovery over SWD and
# why the record log loses pages: docs/src/concepts/ota-stage-1-ble-dfu.md.
#   just bootloader-update-rak lnflash/tests/fixtures/bootloader/rak4631/update-wiscore_rak4631_board_bootloader-0.4.4_nosd.uf2 /dev/leviculum-rak-transport
[doc('Lift a RAK4631 bootloader from a self-update UF2, reflash our firmware')]
bootloader-update-rak UF2 PORT="-":
    {{rak4631_env}} bash scripts/lnode-bootloader.sh update-rak {{UF2}} {{PORT}}

# What every SenseCAP Solar Node flash recipe tells the runner about this
# board. The Board-ID is the XIAO MODULE's, not this product's: a DIY XIAO
# with entirely different radio wiring reports the same string, so it
# confirms "an Adafruit bootloader on a XIAO nRF52840 is mounted" and
# nothing more. That is exactly why `lnflash` has no manifest entry for
# this board and must not be given one on this evidence (Codeberg #233).
solarnode_env := 'LEVICULUM_USB_PID=0003 LEVICULUM_BOARD_NAME=SolarNode LEVICULUM_UF2_BOARD_ID=nRF52840-SeeedXiao-v1 LEVICULUM_DOUBLE_TAP_HINT="Measured on the unit 2026-09-15: a 1200-baud touch enters DFU, and a full VBUS power cycle does NOT leave it — getting out means writing a UF2 or pressing the button."'

# The FIRST image goes onto the mass-storage volume by hand: stock firmware
# has no 1200-baud-touch handler, so there is nothing for the runner to
# touch. Once our firmware is on, this recipe works like the other two.
[doc('Flash every attached SenseCAP Solar Node P1-Pro with our firmware')]
flash-solarnode:
    {{solarnode_env}} bash scripts/nrf-flash.sh solarnode bsp-solarnode

[doc('Flash a single SenseCAP Solar Node by port path or udev symlink')]
flash-solarnode-one PORT:
    LEVICULUM_FLASH_ONLY={{PORT}} {{solarnode_env}} bash scripts/nrf-flash.sh solarnode bsp-solarnode

# The SenseCAP T1000-E's bootloader (2886:0057, Board-ID nRF52840-T1000-E-v1)
# answers serial DFU only on the rig unit, with no UF2 volume, so the UF2
# runner the other boards use has nothing to copy onto. This builds the image
# and wraps it as the DFU package that bootloader takes; the `dfu serial` line
# that writes it, and where the two constants come from, are in
# docs/src/firmware/flashing.md ("SenseCAP T1000-E: serial DFU").
# adafruit-nrfutil comes from `pip install --user adafruit-nrfutil`.
[doc('Build the SenseCAP T1000-E image as a serial-DFU package')]
t1000e-dfu-package:
    cd leviculum-nrf && bash ../scripts/nrf-rustflags.sh cargo build --release --bin t1000e --features bsp-t1000e
    bash scripts/t1000e-dfu-package.sh

# A measurement instrument for OTA stage 2, not firmware. It ERASES the whole
# 2 MiB external flash, programs two patterns over it, reads them back, and
# leaves the part erased; it repeats all of that on every boot until a
# firmware image replaces it. Results are `[QSPI-TEST]` lines on the debug
# port (`leviculum-nrf/src/bin/qspi-selftest.rs`). In no bundle, no guard and
# no nightly on purpose: flashing it is a deliberate act. Same runner and
# environment as flash-solarnode, so `just flash-solarnode` puts the
# firmware back.
[doc('Measurement tool: ERASES all SolarNode external flash, tests it')]
flash-solarnode-qspi-selftest:
    {{solarnode_env}} bash scripts/nrf-flash.sh qspi-selftest bsp-solarnode,qspi-selftest

# Trigger Adafruit-UF2-bootloader on a stock-Meshtastic WisMesh Pocket V2.
# Stock Meshtastic has no 1200-bps-touch handler and the device has no
# externally accessible RESET pin, so the firmware-side admin command is the
# only software-only DFU entry. After our firmware lands, just-flash-rak4631
# uses the touch handler from src/usb.rs and this recipe is no longer needed.
# Requires the meshtastic CLI on PATH (pip install meshtastic).
# Usage: just dfu-rak4631 /dev/ttyACM0
[doc('Put a stock-Meshtastic RAK4631 into its UF2 bootloader')]
dfu-rak4631 PORT:
    meshtastic --port {{PORT}} --enter-dfu

# RNode (LilyGO T-Beam / Heltec, ESP32 family) flashing with Mark's firmware.
# Run on the host the RNodes are attached to. The ESP32 has a mask-ROM
# download bootloader and cannot be bricked: a failed flash is always
# recoverable by re-running flash-rnode. This is unlike the nRF52 LNodes
# (T114, RAK4631), where a bad external image leaves the device USB-dark.
#
# Run flash-rnode-setup once first: the Debian esptool package is
# dfsg-stripped of its flasher stubs and cannot talk to an ESP32-S3 at all
# (scripts/install-esptool.sh says what that looks like and what replaces
# it). rnodeconf is the repo's vendored copy.
# Mark's autoinstall is interactive (product menu); instead we read Mark's
# signed firmware images off a known-good RNode once (flash-rnode-extract,
# into the gitignored .rnode-fw/), then write them back. The write covers
# only the firmware regions, not the NVS/EEPROM partition, so the device
# signature and provisioning are preserved (verified: a T-Beam stayed
# "Validated, Local signature" across a full reflash).
#
# BOARD argument: `auto` (default) reads the chip off the device; `tbeam`
# and `heltec-v4` name it without one attached; a bare chip name
# (`esp32`, `esp32s3`) goes straight through. Offsets follow the chip —
# the S3 keeps its bootloader at 0x0 and the ESP32 at 0x1000, which is
# why a single hardcoded `--chip esp32` could never restore a V4.
# scripts/rnode-flash.sh holds the table and the evidence for each row.

reference_reticulum := justfile_directory() / "reference" / "Reticulum"
rnodeconf := "PYTHONPATH=" + reference_reticulum + " python3 " + reference_reticulum / "RNS" / "Utilities" / "rnodeconf.py"
rnode_tools := env_var_or_default("LEVICULUM_RNODE_TOOLS", home_directory() / ".rnode-tools" / "venv")
esptool := rnode_tools / "bin" / "esptool"
rnode_fw := justfile_directory() / ".rnode-fw"
rnode_flash := "ESPTOOL=" + esptool + " bash " + justfile_directory() / "scripts" / "rnode-flash.sh"

# One-time setup: the esptool these recipes drive. Pinned, and the same one
# scripts/install-ci.sh puts on a CI host, so a board is not flashed by
# whichever esptool a given machine happens to have.
[doc('Install the pinned esptool the RNode recipes drive')]
flash-rnode-setup:
    bash scripts/install-esptool.sh

#   just flash-rnode-info /dev/ttyACM6
[doc('Read-only device info: connectivity, firmware version, signature')]
flash-rnode-info PORT:
    {{rnodeconf}} --info {{PORT}}

# Which chip is on the far end of this port. Read-only, and the answer the
# other recipes derive their offsets from when BOARD is left at auto.
#   just flash-rnode-chip /dev/ttyACM6
[doc('Report which ESP32 chip is behind this port')]
flash-rnode-chip PORT:
    {{rnode_flash}} chip --port {{PORT}}

# Back up an RNode EEPROM (board model, signature, provisioning) before any
# flash. Writes ~/.config/rnodeconf/eeprom<timestamp>.eeprom.
[doc('Back up an RNode EEPROM before flashing it')]
flash-rnode-backup PORT:
    {{rnodeconf}} --eeprom-backup {{PORT}}

# Extract Mark's signed firmware images off a known-good, signature-validated
# RNode into .rnode-fw/ (gitignored). Run ONCE against a trusted device; the
# images then serve as the flash source for flash-rnode.
#   just flash-rnode-extract /dev/ttyACM6
#   just flash-rnode-extract /dev/ttyACM6 heltec-v4
[doc('Extract the signed RNode firmware off a validated device')]
flash-rnode-extract PORT BOARD="auto":
    {{rnode_flash}} extract --port {{PORT}} --board {{BOARD}} --fw-dir {{rnode_fw}}

# Flash an RNode with the extracted Mark firmware. Deterministic and
# non-interactive. Preserves the EEPROM provisioning. Requires
# flash-rnode-extract to have populated .rnode-fw/ first.
#   just flash-rnode /dev/ttyACM6
#   just flash-rnode /dev/ttyACM6 heltec-v4
[doc('Flash an RNode with the extracted upstream firmware')]
flash-rnode PORT BOARD="auto":
    {{rnode_flash}} write --port {{PORT}} --board {{BOARD}} --fw-dir {{rnode_fw}}

# Our own build of Mark's firmware, for the boards we carry a patch for
# (scripts/patches/rnode-firmware-*.patch). Today that is the Heltec V4 alone:
# upstream leaves its LNA gain in the packet RSSI the ESP32 path reports, 21 dB
# hot on the rig (1260). scripts/build-rnode-firmware.sh says what is pinned
# and why the submodule is copied, never patched in place.
[doc('Install the pinned toolchain for our patched RNode firmware builds')]
rnode-firmware-setup:
    bash scripts/build-rnode-firmware.sh setup

#   just rnode-firmware-build heltec-v4
[doc('Build the patched RNode firmware into target/rnode-firmware/<board>')]
rnode-firmware-build BOARD="heltec-v4":
    bash scripts/build-rnode-firmware.sh build {{BOARD}}

# Write that build's app image and store its hash as the EEPROM target, so the
# board validates it. Bootloader, partitions, console and EEPROM stay as the
# release left them. Back up first (flash-rnode-read-image).
#   just flash-rnode-built /dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_44:1B:F6:69:77:24-if00
[doc('Flash our patched RNode build onto a board')]
flash-rnode-built PORT BOARD="heltec-v4":
    {{rnode_flash}} write-app --port {{PORT}} --board {{BOARD}} --image {{justfile_directory()}}/target/rnode-firmware/{{BOARD}}/RNode_Firmware.ino.bin

# The whole flash in one file. This is the restore path: a per-region set
# presumes the partition table it was cut with, an image presumes nothing.
# Measured on the V4: 16 MB read in 102.8 s (1306 kbit/s), no retries.
#   just flash-rnode-read-image /dev/ttyACM6 .rnode-fw/v4-full-16mb.bin heltec-v4
[doc('Read the whole RNode flash into one image file')]
flash-rnode-read-image PORT IMAGE BOARD="auto":
    {{rnode_flash}} read-image --port {{PORT}} --board {{BOARD}} --image {{IMAGE}}

# Write a full-flash image back. Overwrites EVERYTHING, including the NVS
# partition that carries the device signature and provisioning — which is
# the point when restoring the board the image came off, and a mistake on
# any other board. --flash-size keep: put back exactly what was read.
#   just flash-rnode-write-image /dev/ttyACM6 .rnode-fw/v4-full-16mb.bin heltec-v4
[doc('Write a full-flash image back, EEPROM and all')]
flash-rnode-write-image PORT IMAGE BOARD="auto":
    {{rnode_flash}} write-image --port {{PORT}} --board {{BOARD}} --image {{IMAGE}}

# The leviculum.network pages under site/pages/ (Codeberg #260): the lblogd
# test renders every page on both sides and checks that the site links land,
# the board page names the catalogue's boards and no page takes a reserved
# name; the script then checks the external links, the nightly ones against
# the names collect-nightly-debs.sh produces and, when the forge answers,
# against the release's asset list. Offline, the online half says it skipped.
[doc('Test the site pages and check their links against the nightly release')]
site-check:
    cargo test -p lblogd --test site_content
    python3 scripts/site-check.py --selftest
    python3 scripts/site-check.py

# The book's half of the site publish (Codeberg #260).
# `packaging/site/lev-unpack-docs` turns the nightly's
# leviculum-docs-nightly.tar.gz, as the receiver published it, into what
# https://leviculum.network/docs/ serves, on the same unattended VPS and from
# the same network-borne bytes. Its refusals are made to fire here against
# the real script: a checksum that does not match or names another file, a
# member outside docs/ (`..`, absolute, a sibling), a member that is not a
# regular file (symlink, directory, hard link), a book without index.html, a
# hand-made docs/ directory. Each asserts the previous book still serves.
# ~0.5 s, no network, no build.
[doc('Drive the site docs unpacker against the tarballs it must refuse')]
site-unpack-docs-selftest:
    @bash scripts/test-site-unpack-docs.sh

# 1585: loquax-mailbox's growth soak. `loquax mailbox run` (release) beside
# an in-process shared instance that hears `rate` new named lxmf.delivery
# destinations a second, as a field home hears the network's announces; a
# SIGUSR1 census every `sample` seconds and, at the end, the slope of every
# census table, the node tables and RSS plus swap after the warm-up third
# (`MAILBOX_GROWTH`). Red above `limit` kB/min (`MAILBOX_SOAK_VERDICT`);
# the field mailbox grew 366 kB/min. Two hours by default: a reviewer's
# window job, never a coder pass's (loquax/tests/mailbox_growth_soak.rs).
[doc('Soak loquax mailbox run under network announce load and judge its growth')]
mailbox-growth-soak secs="7200" sample="600" rate="2" limit="20":
    MAILBOX_SOAK_SECS={{secs}} MAILBOX_SOAK_SAMPLE_SECS={{sample}} MAILBOX_SOAK_RATE={{rate}} MAILBOX_SOAK_MAX_KB_PER_MIN={{limit}} \
        cargo test --release -p loquax --features tui,history --test mailbox_growth_soak -- --ignored --nocapture

# 1515 (LV-1000): lnpnd's growth soak, the minimal test for "what keeps
# growing". The release daemon on a store prefilled to `messages` (the field's
# 300 000 by default, cached under the soak directory so a rerun skips the
# fill), 14 in-process peers syncing with it, a PN_CENSUS every minute, and a
# PN_GROWTH line per table at the end; red when a table still grows once the
# store is full. 20 minutes by default, longer than a coder command may run:
# a pass shortens it with `secs`, the reviewer runs the default. Never under
# /tmp: the soak directory is the target dir's tmp unless LNPND_SOAK_DIR says
# otherwise (lnpnd/tests/pn_growth_soak.rs).
#
# `alloc` picks the daemon's allocator (1592): `mimalloc`, lnpnd's default,
# or `system`, the libc allocator the field ran before, built with the
# interface families and without `alloc-mimalloc`. Positional, as just 1.40
# hands `alloc=system` over as a literal: `just pn-growth-soak 1200 300000 14
# system`.
[doc('lnpnd growth soak: does a table grow once the store is full (20 min)')]
pn-growth-soak secs="1200" messages="300000" peers="14" alloc="mimalloc":
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{alloc}}" in
    mimalloc) features=() ;;
    system) features=(--no-default-features --features "ble serial i2p") ;;
    *) echo "pn-growth-soak: alloc={{alloc}}; want mimalloc or system" >&2; exit 2 ;;
    esac
    LNPND_SOAK_SECS={{secs}} LNPND_SOAK_MESSAGES={{messages}} LNPND_SOAK_PEERS={{peers}} \
        cargo test --release -p lnpnd "${features[@]}" --test pn_growth_soak -- --ignored --nocapture

# The forge runner is a shared machine, and a test whose pass depends on the
# host being fast is a red waiting for a slow day (672: lnpnd's stamp worker
# against a ten second wall budget lost the first landing in three days). This
# runs every workspace test binary once unconstrained and once under
# `systemd-run --user --scope -p CPUQuota=<quota>`, with libtest's per-test
# times, and prints every test at or above 10 s under the quota beside its
# normal time, and every red. About 15 min unconstrained and 90 min under 25 %
# on the coder host (689); both resume from where they stopped, per commit,
# under ~/.local/state/leviculum-ci/under-quota/. A coder pass, held to 600 s a
# command, runs the script itself with --budget (see its header). The test
# binaries build unconstrained first: the question is the tests, not rustc.
#
# `only` narrows both runs to the target keys containing it (the script's
# `--only`), a target with required-features included (1067); `report` still
# reads the whole survey directory. A run's exit 1 (red) and 3 (budget) are
# left to `report`, but 2 (an `only` that matches nothing, or a usage error)
# ends the recipe: a narrowed run over nothing read green twice (1059, 1065). just 1.40 has no
# named recipe arguments and hands `quota=25%` to the recipe as a literal
# positional value (895 ran the whole survey that way), so the body reads
# `quota=` and `only=` by name in either slot and takes a bare value by
# position: `just test-under-quota quota=25% only=mvr`, `just test-under-quota
# 25% mvr` and `just test-under-quota only=mvr` all mean what they say.
[doc('Tests under a CPU quota: just test-under-quota quota=25% only=mvr')]
test-under-quota quota="25%" only="":
    #!/usr/bin/env bash
    set -euo pipefail
    q="25%" o="" slot=0
    for a in "{{quota}}" "{{only}}"; do
        slot=$((slot + 1))
        case "$a" in
            quota=*) q="${a#quota=}" ;;
            only=*) o="${a#only=}" ;;
            *=*) echo "test-under-quota: unknown argument '$a' (quota=, only=)" >&2; exit 2 ;;
            "") ;;
            *) if [ "$slot" = 1 ]; then q="$a"; else o="$a"; fi ;;
        esac
    done
    narrow=()
    if [ -n "$o" ]; then narrow=(--only "$o"); fi
    survey() { python3 scripts/test-under-quota.py run "$@" || { rc=$?; [ "$rc" != 2 ] || exit 2; }; }
    {{just_executable()}} build-integ-bins
    survey --quota none "${narrow[@]}"
    survey --quota "$q" "${narrow[@]}"
    python3 scripts/test-under-quota.py report --quota "$q"

# 730: `cargo test --workspace` leaves nothing behind in the temp directory.
# /tmp on the coder host is a tmpfs, so a leaked scratch directory is RAM, and
# on 2026-10-14 it held 40 201 such entries and stood at 100 %. The script runs
# the suite in the ordinary way with TMPDIR pointed at a fresh directory of its
# own and fails, naming them, on anything left in it once the suite exits;
# another lane's run cannot write there, which is what keeps the verdict
# deterministic where a diff of /tmp itself is not. About ten minutes, the
# length of the suite it runs.
[doc('Fail if cargo test --workspace leaves entries in its TMPDIR')]
tmp-leftovers:
    @bash scripts/tmp-leftovers.sh

# 987: the recipe above must start the sccache server before it points TMPDIR
# at the scratch directory it deletes, or a server born during the suite fails
# every later compile on the host (985). Against a stub cargo and sccache. ~1 s.
[doc('Check tmp-leftovers starts sccache outside its scratch TMPDIR')]
tmp-leftovers-selftest:
    @bash scripts/test-tmp-leftovers.sh

# PUB-0034: THIRD-PARTY.md is the one place that names each submodule's and
# each vendored crate's licence and whether it reaches a binary, and it is
# hand-written. This refuses a `.gitmodules` entry or a `[patch.crates-io]`
# path it does not name, so a new one cannot arrive unaccounted. 0.05 s, no
# build: it parses .gitmodules and the tracked manifests.
[doc('Check THIRD-PARTY.md names every submodule and vendored crate')]
check-third-party:
    @python3 scripts/check-third-party.py

# T-0009: the two #[ignore]d live I2P tests (interfaces::i2p::i2pd_live) were
# the only coverage of I2PInterface against a real SAM bridge, and nothing
# started the i2pd they need. This starts a private I2P network of five i2pd
# on loopback (scripts/i2p-privnet.sh, 1576: own netid, own data directories,
# own ports, no reseed server, so a system i2pd is untouched and no internet is
# needed), runs both tests on two of its routers and stops it. About 40 s
# after the build. Not a tier; the reviewer runs it beside `emulated-corpus`.
[doc('Run the live I2P tests on a private I2P network on loopback')]
i2p-live:
    bash scripts/run-i2p-live.sh

# The embassy pin rule (Codeberg #469, docs/src/firmware/embassy-pins.md): one
# version of each embassy crate across every manifest of the firmware
# workspace, none from git, no git `[patch]`. Reads manifest text, no cargo
# call, and refuses an injected second version and git patch on a temp copy
# first. ~0.2 s.
[doc('Check the firmware manifests pin one embassy version set, none from git')]
embassy-pins:
    @bash scripts/check-embassy-pins.sh

# The log-diff masking (Codeberg #469, docs/src/firmware/host-test-seams.md,
# "The log-diff masking"): a capture moved only in stamps and counters masks
# to the same text, one with a reordered line masks to that line. ~0.1 s.
[doc('Check the debug-capture mask hides stamps and counters, not order')]
log-diff-mask-selftest:
    @bash scripts/test-log-diff-mask.sh

# The orchestration stratum's two guards (Codeberg #469,
# docs/src/concepts/firmware-layers.md, "Gates the new stratum brings").
# `leviculum-firmware` is the one copy every chip family binds to, so a HAL in
# its resolved graph (`--target all`, dev edges included) or a BSP `cfg` or
# chip name in its sources outside doc comments is a board decision in the
# shared layer. The graph guard reads the firmware workspace `--frozen`, like
# nrf-evt-max-size. Each refuses an injection on a temp copy before it checks
# the tree. ~0.8 s and ~0.1 s.
[doc('Check the orchestration crate links no HAL or chip crate')]
firmware-no-hal:
    @bash scripts/check-firmware-no-hal.sh

[doc('Check the orchestration crate sources name no chip and no BSP feature')]
firmware-no-chip-cfg:
    @bash scripts/check-firmware-no-chip.sh

# The board firmware's module graph (Codeberg #469, 957,
# docs/src/concepts/firmware-layers.md, "The extraction, bottom-up"): which
# module can move into the orchestration crate is read off what it reaches, so
# the reach of every file in both trees is a checked-in census,
# scripts/nrf-module-reach.txt, re-derived here and diffed. A move that does
# not regenerate it goes red with the diff. The selftest first injects a false
# reach into a copy and must see it refused. The second file (958),
# scripts/nrf-seam-vocabulary.txt, is the same census one level finer: every
# item a bind-crate file names below the bind line, code and doc comments
# apart, so the seam steps are ordered against the calls they have to carry.
# Same regeneration, same check, its own injected false line. ~0.1 s.
[doc('Check the nRF module-reach census against the tree')]
nrf-module-reach:
    @python3 scripts/nrf-module-reach.py --selftest
    @python3 scripts/nrf-module-reach.py

# Codeberg #474: the map core is `no_std` plus `alloc`, so the M9 draws a
# region through the same reader, projection, viewport and tile selection the
# desktop does (docs/src/concepts/loquax-map.md, section 3). Its `std` feature
# adds only a file `Storage`; without it the library is checked for the
# firmware triple, so a `std::` path reaching the core is refused. In
# `guards`: 1.5 s standing alone after `touch leviculum-map/src/lib.rs`,
# 0.08 s warm (measured 2026-10-27).
[doc('Check the map core builds no_std for the firmware triple (#474)')]
map-core-no-std:
    rustup target add thumbv7em-none-eabihf
    cargo check -p leviculum-map --lib --no-default-features --target thumbv7em-none-eabihf

# Codeberg #232: the loquax core is `no_std` plus `alloc`, so the M9 and a GUI
# run the same messenger core the terminal does (docs/src/concepts/
# loquax-architecture.md, "the core must not know it has a terminal"). The
# shell is loquax's `std` feature; without it the library is the core, checked
# for the firmware triple, so a `std::` path, `eprintln!`, tokio or
# leviculum-std reaching a core module is refused. Green since 993 and in
# `guards`; while it was red, `scripts/loquax-no-std-groups.py` grouped its
# error list into the plan's work list on that page.
[doc('Check the loquax core builds no_std for the firmware triple (#232)')]
loquax-core-no-std:
    rustup target add thumbv7em-none-eabihf
    cargo check -p loquax --lib --no-default-features --target thumbv7em-none-eabihf

# 1303, #475: lnomad's browser core is `no_std` plus `alloc` as loquax's is,
# so the M9 runs the model, layout, scene, intents and catalogue the terminal
# and the window run (docs/src/concepts/lnomad-everywhere.md, slice 4a). The
# platform layer (fetch, files, the image decoder, the command line, the
# terminal's text field) is lnomad's `std` feature; without it the library is
# the core, checked with leviculum-micron for thumbv7em-none-eabihf and, where
# the esp fork is installed, for the M9's xtensa-esp32s3-none-elf (a SKIP line
# says when it is not). Positive control first: a `std::fs` call injected into
# a temp copy of the core is refused. In `guards`, see the ledger.
[doc('Check the lnomad core and micron build no_std for firmware (#475)')]
lnomad-core-no-std:
    @bash scripts/check-lnomad-core-no-std.sh

# 1351, #232, #473: the M9's renderer (`loquax-m9`) is `no_std` plus `alloc`,
# so the firmware links the crate the host draws the goldens with
# (docs/src/concepts/handheld-messenger.md, "The M9 frontend: the plan,
# measured", slice one). Checked with what it builds on (the loquax core,
# leviculum-ui-ratatui without `terminal`, ratatui, mousefood) for
# thumbv7em-none-eabihf and, where the esp fork is installed, the M9's
# xtensa-esp32s3-none-elf (a SKIP line says when it is not). Positive control
# first: a `std::fs` call injected into a temp copy of the crate is refused,
# and one injected into the client the board runs (`loquax::home::ask`,
# slice five, 1363) as well. In `guards`, see the ledger.
[doc('Check the M9 renderer and client build no_std for firmware (#232, #473)')]
m9-render-no-std:
    @bash scripts/check-m9-render-no-std.sh

# 994, #232: loquax's `tui` feature is off by default, so `cargo test
# --workspace` and the workspace clippy never compile `loquax tui` or run its
# tests. This does: clippy over every loquax target with the feature, the
# binary's renderer and script tests, and the scripted session against an
# in-process daemon (`loquax/tests/tui_snapshot.rs`, 8 s, and 8 s again in
# each of three runs under a 25 % CPU quota, 2026-10-25), and the home node
# end to end against a stock Python LXMF sender (`loquax/tests/home_node.rs`,
# 2.6 s, 2.9 s under a 25 % quota, 2026-10-29; `--daemon` and `home` are the
# tui build's). In `standard`, not `fast`: both spawn loquax against a daemon
# over sockets. The screen model's own tests are core and run in every lib run.
# The import and the export (`loquax/tests/import.rs`, 1103) need the
# `history` feature the tui build carries; files only, 0.05 s. So does the
# wire's property test (`loquax/tests/wire_props.rs`, 1173): every home
# wire shape and imported entry round trips, 256 cases each, 0.3 s.
# The scene's goldens (`loquax/tests/scene_golden.rs`, 1217): the list and the
# chat built from the ugly store for both layout classes, 0.4 s. Read
# receipts through the mailbox (`loquax/tests/mailbox_receipts.rs`, 1234)
# need `history` too; in-process, no sockets. `loquax mailbox setup`
# (`loquax/tests/mailbox_setup.rs`, 1266) is the terminal build's command;
# it spawns the binary over a temporary home, no daemon. The ticks' cells
# (`loquax/tests/device_receipts.rs`, 1462) need `history`; in-process. So
# do the relay's cells (`loquax/tests/relay_group.rs`, 1482). The bars of
# "only what is visible" and "O(1) per event" over the real fixture
# (`loquax/tests/operation_counts.rs`, 1489) need `history` for the
# store's restart; counted, in-process, 4 s.
# The forge runs this recipe and has no reference submodule, so the home node
# tests that start the Python peer go out there by the names
# scripts/integ-prerequisites.txt declares (scripts/with-prerequisites.sh,
# 1167); where the reference is checked out they run.
# build-integ-bins: tui_snapshot starts the checkout's release lnsd as the
# messenger's daemon (1570), and refuses one older than its sources.
[doc('Lint and test loquax with its tui feature (#232)')]
loquax-tui: build-integ-bins
    cargo clippy -p loquax --features tui --all-targets -- -D warnings
    {{manifest}} loquax-tui -- bash scripts/with-prerequisites.sh loquax/tui_snapshot loquax/home_node loquax/import loquax/wire_props loquax/scene_golden loquax/mailbox_receipts loquax/mailbox_setup loquax/mailbox_mesh loquax/device_receipts loquax/relay_group loquax/operation_counts loquax/mailbox_census loquax/mailbox_growth_soak -- cargo test -p loquax --features tui --bins --test tui_snapshot --test home_node --test import --test wire_props --test scene_golden --test mailbox_receipts --test mailbox_setup --test mailbox_mesh --test device_receipts --test relay_group --test operation_counts --test mailbox_census --test mailbox_growth_soak

# 1084, #474: loquax's `gui` feature (the iced window) is off by default and
# in no other feature, so neither the workspace runs nor `loquax-tui` compile
# it. This does: clippy over every loquax target with the feature, and the
# binary's tests with it, among them the map view's frame over the fixture
# region at three zooms through the window's own pipeline, which needs no
# display. With them the library's tests under the feature, whose
# `screenmap` the tui feature leaves out: the map's frames and names
# (`mapframe`, `mapnames`), among them 1154's frames of the fast paths
# against the old ones. The lint and the tests build for the host's own
# triple, not the workspace's musl-static default: winit opens X11 and
# Wayland with dlopen, which a musl-static binary cannot link, and the
# window widgets (leviculum-ui-iced) are empty there. Not in `standard`:
# nothing ships it yet.
[doc('Lint and test loquax with its gui feature (#474)')]
loquax-gui:
    cargo clippy -p loquax --features gui --all-targets --target "$(rustc -vV | sed -n 's/^host: //p')" -- -D warnings
    {{manifest}} loquax-gui -- cargo test -p loquax --features gui --lib --bins --target "$(rustc -vV | sed -n 's/^host: //p')"

# Slice 2 of lnomad-everywhere (#475): lnomad's window behind its `gui`
# feature, and the window widgets it shares with Loquax (leviculum-ui-iced).
# Clippy over every lnomad target with the feature, then the crate's tests
# and lnomad's with it: the README's window pixel for pixel
# (`lnomad/tests/golden/gui-page-{light,dark}.png`), a headless click on a
# link, the frame time. For the host's own triple, as `loquax-gui`: the
# window's crates link libdl, which a musl-static binary cannot. Not in
# `standard`: nothing ships it yet.
[doc('Lint and test lnomad --features gui and leviculum-ui-iced (#475)')]
lnomad-gui:
    cargo clippy -p leviculum-ui-iced -p lnomad --features gui --all-targets --target "$(rustc -vV | sed -n 's/^host: //p')" -- -D warnings
    {{manifest}} lnomad-gui -- cargo test -p leviculum-ui-iced -p lnomad --features gui --target "$(rustc -vV | sed -n 's/^host: //p')"

# 1217, #232: the phone's strings of the list and the chat come from the core's
# catalogue (`loquax/src/ui/strings.rs`, English and German), which this writes
# to `loquax-android/android/res/values{,-de}/strings_scene.xml`. The library's
# tests refuse a generated file that differs from the catalogue and an Android
# `strings.xml` that still holds one of its keys, so a forgotten run is red in
# every lib run. ~10 s warm, the library's tests filtered to the catalogue.
[doc('Write the phone strings_scene.xml from the core catalogue (#232)')]
loquax-strings:
    LOQUAX_WRITE_STRINGS=1 cargo test -p loquax --lib ui::strings::tests::the_phone_reads_the_catalogue

# 1577, 1599, #232: the public rooms' hubs Loquax offers, measured again. The
# probe (`loquax/examples/rrc_hubs_probe.rs`) dials the shipped entry servers,
# listens `listen` seconds for `rrc.hub` announces, then opens every hub of
# `loquax/assets/hubs.toml` with Loquax's own hub link (HELLO until WELCOME,
# then it leaves; it joins nothing and says nothing), twelve at once. A hub
# that did not answer is opened once more, four at once: on 2026-11-09 33 of
# 81 hubs were silent on a first try and 18 of them answered a second or third
# (1599's measurement). A shipped hub silent on both tries, or an unreached
# one that now answers, is a finding and the recipe exits 1: the list is data,
# and a stale hash is a finding, not a surprise. The probe's lines and the
# announces heard land in `target/tmp/rrc-hubs/`. Needs the internet; about
# `listen` seconds plus up to 100 s a round of hubs that do not answer. `hubs`
# names another list, for the check's own positive control.
[doc('Measure the shipped public room hubs again (#232)')]
rrc-hubs-check listen="1800" hubs="loquax/assets/hubs.toml":
    #!/usr/bin/env bash
    set -euo pipefail
    out=target/tmp/rrc-hubs
    mkdir -p "$out"
    cargo run -q -p loquax --example rrc_hubs_probe -- --listen {{listen}} --open listed --parallel 12 --hubs {{hubs}} > "$out/probe.txt" 2> "$out/probe.err"
    grep '^HUB ' "$out/probe.txt" | grep -v ' welcome=yes ' | sed 's/.* hash=\([0-9a-f]*\) .*/[[hub]]\nhash = "\1"/' > "$out/silent.toml" || true
    : > "$out/retry.txt"
    if [ -s "$out/silent.toml" ]; then
        cargo run -q -p loquax --example rrc_hubs_probe -- --listen 10 --open listed --parallel 4 --hubs "$out/silent.toml" > "$out/retry.txt" 2> "$out/retry.err"
    fi
    awk '/^\[\[hub\]\]/ {k="hub"} /^\[\[unreached\]\]/ {k="unreached"} /^hash *=/ {gsub(/[" ]/, "", $0); split($0, h, "="); print h[2], k}' {{hubs}} > "$out/listed.txt"
    findings=0
    while read -r hash kind; do
        line="$(grep -F "HUB hash=$hash " "$out/retry.txt" | grep -F ' welcome=yes ' || grep -F "HUB hash=$hash " "$out/probe.txt" || true)"
        welcome="$(sed -n 's/.* welcome=\([^ ]*\).*/\1/p' <<< "$line")"
        case "$kind:$welcome" in
            hub:yes|unreached:yes) echo "ANSWERED $kind $line" ;;
            hub:*) echo "FINDING shipped hub did not answer twice: ${line:-HUB hash=$hash (not probed)}"; findings=$((findings + 1)) ;;
            unreached:*) echo "STILL-UNREACHED $line" ;;
        esac
        if [ "$kind:$welcome" = "unreached:yes" ]; then
            echo "FINDING an unreached hub answers now: $line"
            findings=$((findings + 1))
        fi
    done < "$out/listed.txt"
    shipped="$(grep -c ' hub$' "$out/listed.txt" || true)"
    heard="$(grep -c '^HEARD' "$out/probe.txt" || true)"
    echo "SHIPPED $shipped hubs; HEARD $heard rrc.hub announces in {{listen}} s (the list is in $out/probe.txt)"
    if [ "$findings" -gt 0 ]; then
        echo "rrc-hubs-check: $findings finding(s): update {{hubs}} and the page's measurement"
        exit 1
    fi
    echo "rrc-hubs-check: the list is as measured"

# 1147, #232: the messenger is Loquax in prose (Lew, 2026-10-30); `loquax` is
# the command, the package, the crate and every path, so in the docs it is code,
# a path, a hyphenated page name or a man page's bold command. Refuses a bare
# lower-case `loquax` anywhere else in docs/src. ~0.1 s, no build, self-tests
# its controls first (a planted "loquax is" fails, `loquax send` in a code
# span passes).
[doc('Check the docs call the messenger Loquax in prose (#232)')]
loquax-name:
    @python3 scripts/check-loquax-name.py

# 1282: every test that spawns Python has its line in
# scripts/integ-prerequisites.txt, or a runner without the reference runs it
# and it fails there (land gate on d365c0d2, forge stage, home_reach's Python
# peer). Next to check-standard-integ in `guards` and `fast`; here at the
# end so no cited Justfile line moves. Reads the test targets' call graphs
# from the text; ~0.8 s, no build, its fixtures first. Since 1353 the same for
# the four reference codec tools the voice round trip spawns, each by its own
# `command:` line (hamster has no codec2, and the pre-push run panicked).
[doc('Check that every Python- or codec-spawning test declares its needs')]
check-python-test-prerequisites:
    @python3 scripts/check-python-test-prerequisites.py

# The ThinkNode M9's Loquax image (1361, #232, #473): the renderer on the
# panel, the keyboard over I2C, the frame dump on a control frame, no radio.
# Builds and packages it and prints its flash and stack headroom the way
# nrf-flash-budget prints the nRF images' (FLASH_HEADROOM, STACK_HEADROOM),
# red below 8 KiB of the partition free or a stack under the open chat's
# census ceilings. About 1.5 min cold, 35 s warm (2026-11-03). Not in a
# gate: the Xtensa compiler fork is optional per host, as for build-esp32.
[doc('Build the ThinkNode M9 Loquax image and print its headroom')]
build-m9:
    bash scripts/build-m9-image.sh

# Write the image `build-m9` made to an M9 through the ROM loader, at the
# 230400 baud its CH340K holds (lnflash/src/esp.rs, the bench notes of
# 2026-10-25), with the M9's partition table (`leviculum-esp/
# partitions-m9.csv`: the identity and the Loquax client record beside the
# application, 1414, and the message store, 1476). The port is the bridge's tty; espflash resets the
# board into the loader and back.
[doc('Flash the ThinkNode M9 Loquax image over its CH340 (port: the tty)')]
flash-m9 port:
    #!/usr/bin/env bash
    set -euo pipefail
    export PATH="$HOME/.cargo/bin:$PATH"
    elf=leviculum-esp/target/xtensa-esp32s3-none-elf/release/thinknode_m9
    test -f "$elf" || { echo "no $elf: run just build-m9 first"; exit 1; }
    # DIO at 80 MHz, as the vendor's image on this flash (ZD25Q128) has it
    # in its header: espflash's default is 40 MHz, and the M9 runs its code
    # from flash through a 32 KB cache, so every miss took twice as long
    # (1463; build-m9-image.sh writes the same into the image).
    espflash flash --chip esp32s3 --port {{port}} --baud 230400 \
        --non-interactive --flash-size 16mb --flash-mode dio --flash-freq 80mhz \
        --partition-table leviculum-esp/partitions-m9.csv "$elf"

# The M9 image is the same bytes from any clone of the same commit (1458,
# #474): builds it in this tree and in a clone of HEAD under a throwaway path
# and compares sha256, red on a difference or on a host path in either image;
# on a tree with tracked changes both builds are clones of HEAD. In `fast`
# beside nrf-flash-budget; it says SKIP on a host without the Xtensa fork or
# without unprivileged user namespaces. 75 s on a clean tree, 89 s with
# tracked changes (two cold clones), measured 2026-11-06. The cheap half, no
# host path in the image, runs in every build-m9.
# not-in-guards: m9-reproducible -- 75 s, two full Xtensa builds, one cold
[doc('Check the ThinkNode M9 image builds to the same bytes from a clone')]
m9-reproducible:
    bash scripts/check-m9-reproducible.sh --selftest
    bash scripts/check-m9-reproducible.sh

# The M9 gates of every coder pass that moves leviculum-esp/src or a crate
# the M9 links (1574, #474): the release image and the bench image, each
# with its FLASH_HEADROOM and two STACK_HEADROOM lines and red below a
# floor, then m9-reproducible. 1569 ran check and clippy on both variants
# but never the image script, and its bench image's stack FAIL was first
# seen on the bench. The report quotes the headroom lines of both images.
# Not in a gate for the same reason as build-m9 (the Xtensa fork is
# optional per host); each image build says SKIP without it.
[doc('Build both M9 images with their headroom, then m9-reproducible')]
m9-gates:
    bash scripts/build-m9-image.sh
    LEVICULUM_M9_BENCH=1 bash scripts/build-m9-image.sh --bench
    just m9-reproducible

# The M9 image's stack frames and deepest static call chains (1582): the
# `entry` frames of the Xtensa windowed ABI read out of the linked ELF,
# the call graph walked from core 0's main, the UI core's ui_main and the
# interrupt handlers. A report, not a gate: the floors in
# scripts/build-m9-image.sh are the board's measured peaks plus a reserve
# this sizes. Reads the ELF build-m9 left (or the one named); without
# the Xtensa fork it says SKIP.
[doc('Print the M9 image largest stack frames and deepest static call chains')]
m9-stack-frames elf="":
    #!/usr/bin/env bash
    set -uo pipefail
    python3 -I scripts/m9-stack-frames.py --selftest || exit 1
    if [ ! -f "$HOME/export-esp.sh" ]; then
        echo "[m9-stack-frames] SKIP no esp toolchain on this host (~/export-esp.sh)"
        exit 0
    fi
    # shellcheck disable=SC1091
    source "$HOME/export-esp.sh" >/dev/null
    source scripts/cargo-target-dir.sh
    elf="{{elf}}"
    [ -n "$elf" ] || elf="$(cargo_target_dir leviculum-esp)/xtensa-esp32s3-none-elf/release/thinknode_m9"
    python3 -I scripts/m9-stack-frames.py "$elf"

# The nRF images are the same bytes from any clone of the same commit (1460,
# #474): builds the T114 image in this tree and in a clone of HEAD under a
# throwaway path, both through scripts/nrf-rustflags.sh's fixed-path wrapper,
# and compares the flat images by sha256, red on a difference or on a host
# path in either; on a tree with tracked changes both builds are clones of
# HEAD. Says SKIP on a host without unprivileged user namespaces. In `fast`
# beside m9-reproducible: 49 s on a clean tree, 51 s with tracked changes
# (two cold clones), measured 2026-11-06.
# not-in-guards: nrf-reproducible -- 49 s, two full T114 release builds
[doc('Check the T114 firmware image builds to the same bytes from a clone')]
nrf-reproducible:
    bash scripts/check-nrf-reproducible.sh --selftest
    bash scripts/check-nrf-reproducible.sh

# 1422, #474: the map's frame budget measured (the eye candy study of
# 2026-11-04, section 4). `REGION` is the Frankfurt region the study cut
# (LMR3 with names and buildings, sha256 5022565b...a2b7c7a). One MAP_BENCH
# line a scene, size and zoom on the host, as an A/B in the same run: the
# bench of `BASE` (default: the commit that last wrote the baseline,
# loquax/examples/map_eye_bench.baseline) built in a worktree under target/
# into its own target directory (1470), and this tree's, in turn, five rounds, one worker thread, the medians compared; a held
# column more than 5 % and 0.1 ms slower fails. A run against the file alone
# measured a phone frame's compose 3.2 and 4.8 ms with one binary, which no
# 5 % rule survives. Then this tree's bench under the weak CPU emulation
# (12.5 % of one core in 1 ms periods, about ten times slower than the host
# core; the 100 ms default period lets short frames through and measures
# bimodal), reported only. The tables land in target/map-bench/. A
# measuring tool, never in `guards`: a build of BASE, about 1 min host,
# 1 min emulated.
[doc('Measure the map frame budget, A/B on the host and on a weak CPU (#474)')]
map-bench REGION BASE="":
    #!/usr/bin/env bash
    set -euo pipefail
    out=target/map-bench
    mkdir -p "$out"
    exe() { python3 -c 'import json,sys
    for l in sys.stdin:
        m = json.loads(l)
        if m.get("reason") == "compiler-artifact" and m.get("executable") and m["target"]["name"] == "map_eye_bench":
            print(m["executable"])'; }
    base="{{BASE}}"
    [ -n "$base" ] || base="$(git log -1 --format=%H -- loquax/examples/map_eye_bench.baseline)"
    src="$out/base-src"
    git worktree remove --force "$src" 2>/dev/null || rm -rf "$src"
    git worktree add -q --detach "$src" "$base"
    # BASE into its own target directory (1470): sharing this tree's, cargo
    # can take BASE's artefacts as fresh for this tree, and the A/B is one
    # binary against itself. The same bytes when BASE is not this tree is red.
    target="$(realpath -m "$out/base-target")"
    built="$(cd "$src" && CARGO_TARGET_DIR="$target" cargo build --release -p loquax --features screenmap --example map_eye_bench --message-format=json-render-diagnostics | exe)"
    cp "$built" "$out/base-bench"
    git worktree remove --force "$src"
    bench="$(cargo build --release -p loquax --features screenmap --example map_eye_bench --message-format=json-render-diagnostics | exe)"
    mode=ab
    if [ "$(git rev-parse "$base^{commit}")" = "$(git rev-parse HEAD)" ] && git diff --quiet HEAD; then mode=aa; fi
    python3 -I scripts/window-bench-compare.py --check-exes "$mode" "$out/base-bench" "$bench"
    rm -rf "$out/maps" && mkdir -p "$out/maps"
    ln -s "$(realpath "{{REGION}}")" "$out/maps/$(basename "{{REGION}}")"
    status=0
    "$bench" "$out/maps" --label host --against "$out/base-bench" --write "$out/host.txt" > "$out/host.md" || status=$?
    cat "$out/host.md"
    systemd-run --user --scope -q -p CPUQuota=12.5% -p CPUQuotaPeriodSec=1ms taskset -c 0 "$bench" "$out/maps" --label weak --write "$out/weak.txt" > "$out/weak.md"
    cat "$out/weak.md"
    exit "$status"

# 1422: rewrite the baseline `map-bench` measures against, after a change
# whose cost was measured and accepted in its commit message; the commit
# that writes it is the next runs' BASE.
[doc('Rewrite the map-bench baseline from a host run (#474)')]
map-bench-baseline REGION:
    #!/usr/bin/env bash
    set -euo pipefail
    bench="$(cargo build --release -p loquax --features screenmap --example map_eye_bench --message-format=json-render-diagnostics | python3 -c 'import json,sys
    for l in sys.stdin:
        m = json.loads(l)
        if m.get("reason") == "compiler-artifact" and m.get("executable") and m["target"]["name"] == "map_eye_bench":
            print(m["executable"])')"
    out=target/map-bench
    rm -rf "$out/maps" && mkdir -p "$out/maps"
    ln -s "$(realpath "{{REGION}}")" "$out/maps/$(basename "{{REGION}}")"
    "$bench" "$out/maps" --label host --write loquax/examples/map_eye_bench.baseline

# 1441, #474, #232: the window's frames measured as Lew's window draws them
# (loquax/src/gui/bench.rs): a map drag, five wheel notches, the chat's
# wheel, its scrollbar dragged, a mouse drag on its body, a touchpad
# scroll and the compose field's blink, over 3000 one-line messages
# (`chat-*`) and over a few hundred long ones shaped like Lew's chats
# (`real-*`, 1456), each in real time through iced's user interface as
# its loop runs on X11, the frames presented as the software renderer's
# compositor presents them (damage, no read back), at 900x560, 1280x1190,
# 1276x2099, 1527x2099 and 1920x1080, headless, and at both tall sizes
# again with the frame meter's overlay on (`+fps`). One WINDOW_BENCH line a size and
# gesture. As map-bench: the bench of BASE (default: the commit that last
# wrote loquax/tests/window_bench.baseline) built in a worktree under
# target/ into its own target directory (1470: with one, cargo took BASE's
# artefacts as fresh for this tree and the A/B compared one binary with
# itself; the exes' sha256 must now differ), and this tree's, in turn for
# ROUNDS rounds (at least 3), the medians compared; a frame_ms_mean more
# than 5 % and 0.1 ms slower fails, a row whose rounds spread more than
# 5 % on a side is "noise" with no verdict, and a 1-minute load above
# WINDOW_BENCH_MAX_LOAD (default a fifth of the CPUs, 2.0 here) at any round's start is a
# SKIP, exit 3 (a gesture of fewer than 30 frames is reported, not held;
# the chat's drag rows, 0 frames, are left out). `just window-bench HEAD`
# in a clean tree is the A/A selftest: one binary as both sides, every row
# held or noise. The steps are scripts/window-bench.sh's, which runs one
# at a time too. The table lands in target/window-bench/table.md. A
# measuring tool, never in `guards`: two release builds, about 160 s a
# round and side.
[doc('Measure window frames at four sizes, A/B against the baseline (#474)')]
window-bench BASE="" ROUNDS="3":
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'bash scripts/window-bench.sh clean' EXIT
    bash scripts/window-bench.sh prepare "{{BASE}}"
    for i in $(seq 1 {{ROUNDS}}); do
        bash scripts/window-bench.sh round "$i"
    done
    bash scripts/window-bench.sh compare

# 1469, #474: the amd64 package's two loquax builds measured against each
# other, as window-bench measures two commits: this tree's bench as BASE,
# and the same built with -C target-cpu=x86-64-v3 (build-deb.sh's v3 target
# directory) as this tree; the verdict rules are window-bench's, so a v3 row
# slower than the baseline is a REGRESSION. The table lands in
# target/window-bench/table.md. A measuring tool, never in `guards`.
[doc('Measure the window frames of the baseline and the x86-64-v3 build (#474)')]
window-bench-v3 ROUNDS="3":
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'bash scripts/window-bench.sh clean' EXIT
    bash scripts/window-bench.sh prepare-v3
    for i in $(seq 1 {{ROUNDS}}); do
        bash scripts/window-bench.sh round "$i"
    done
    bash scripts/window-bench.sh compare

# 1470: the window bench's verdicts (regression, noise, load SKIP, too few
# rounds, the exes' bytes) on synthetic rounds, each with its positive
# control. ~0.1 s, no build.
[doc('Check the window-bench verdict rules on synthetic rounds')]
window-bench-selftest:
    @python3 -I scripts/window-bench-compare.py --selftest

# 1660: the DFU exit tool's parsers on the 2026-11-10 T114 bench captures
# (docs/src/concepts/ota-stage-1-ble-dfu.md, "Leaving DFU without hands"):
# the bootloader's advert from a decoded btmon and a btmgmt transcript, the
# address-plus-one rule, the Control Point and CCCD handles from gatttool,
# each with a control that must not match. ~0.1 s, no radio.
[doc('Check the BLE DFU exit tool reads the captured T114 bootloader')]
dfu-exit-selftest:
    @python3 -I scripts/dfu-exit.py --selftest scripts/dfu-exit-fixtures

# 1533: the M9 bench walk's verdicts on a simulated board behind the rig
# proxy's files (a good walk passes and writes its dump as a PNG, a 150 ms
# frame and a growing rx_lost fail), and the shipped walk file reads; and
# first the proxy itself (1542, scripts/m9-proxy.py) on a pty: the board's
# bytes reach raw.bin, board.log and the link, tx.hex and the link reach
# the board, STOP ends it. ~5 s, no build, no board.
[doc('Check the M9 bench walk verdicts on a simulated board')]
m9-walk-selftest:
    @bash scripts/m9-walk.sh --selftest

# 1533: walk the M9 on the bench without hands: flash `image` (the bench
# ELF, `scripts/build-m9-image.sh --bench`) through the rig folder's proxy,
# boot twice, run `walk` (scripts/m9-walks/bench.walk) and print the
# VERDICT; walk.md and the dumps land in `<rig>/walk-<time>/`. `proxy` is
# the M9's tty for a bench whose rig folder has no proxy running (1542):
# scripts/m9-proxy.py is started on it.
[doc('Walk the M9 on the bench: flash, two boots, a walk file, a verdict')]
m9-walk rig walk="scripts/m9-walks/bench.walk" image="" proxy="":
    bash scripts/m9-walk.sh --rig {{rig}} --walk {{walk}} {{ if image != "" { "--image " + image } else { "" } }} {{ if proxy != "" { "--start-proxy " + proxy } else { "" } }}

# 1562: the bench M9's own mailbox, never Lew's: `setup` seeds a `loquax
# mailbox` state folder with its own identity and the 1489 fixture, `run`
# starts it beside the bench's lnsd, `join rig=<folder>` writes a fresh
# code to the board, `send <words>` delivers a live arrival from the
# reference LXMF client, `stop`. scripts/m9-bench-home.sh has the rest.
[doc('The bench M9 mailbox: setup, run, code, join, send, stop')]
m9-bench-home verb *args:
    bash scripts/m9-bench-home.sh {{verb}} {{args}}

# 1562: the bench home's setup and code offline in a scratch folder: the
# code names the seeded home, a second setup appends nothing, another
# home's code names another (the control). Builds loquax and the sim in
# release; not in `fast`.
[doc('Check the bench M9 mailbox setup offline')]
m9-bench-home-selftest:
    @bash scripts/m9-bench-home.sh --selftest

# 1530, #232: the M9 on the host, headless (loquax-m9's `loquax-m9-sim`,
# feature `sim`): the bin's tests first, among them the golden parity (every
# `tests/golden/m9-fw-*.png` reached through the sim's board, by its
# FRAME_DUMP path and by its keys from power-on, compared pixel for pixel),
# then the two walks under scripts/m9-sim-walks/: the fixture from the
# pairing on, and the 1489 store pulled into a home's board and booted again;
# and since 1575 the pull walk (pull.walk) with the board's fold cost, once
# with the drain's budget, whose keys must flush under 100 ms, and once
# without, the control, whose keys must not.
# Dumps, logs and text rows land in `out`, under target/; the bin's lines
# are the board's (`KEY`, `[PAGE]`, `[STORE]`). Not in `fast`: nothing ships
# it, and it builds the bin.
[doc('Walk the M9 sim headless over the fixture and the 1489 store (#232)')]
m9-sim-walk out="target/m9-sim-walk":
    #!/usr/bin/env bash
    set -euo pipefail
    root="$(pwd)"
    {{manifest}} m9-sim-tests -- cargo test -p loquax-m9 --features sim --bin loquax-m9-sim
    cargo build -q -p loquax-m9 --features sim --bin loquax-m9-sim
    sim() { cargo run -q --manifest-path "$root/Cargo.toml" -p loquax-m9 --features sim --bin loquax-m9-sim -- "$@"; }
    for walk in fixture real; do
        dir="$root/{{out}}/$walk"
        rm -rf "$dir" && mkdir -p "$dir"
        store=fixture; [ "$walk" = real ] && store=real
        (cd "$dir" && sim --store "$store" --save flash.img --script "$root/scripts/m9-sim-walks/$walk.walk" --dump last.png --text > text.txt 2> board.log)
        echo "m9-sim-walk: $walk ok, $(ls "$dir"/*.png | wc -l) dumps in $dir"
    done
    # What the walks must have shown, read off the board's own lines.
    grep -q '^\[PAGE\] name=map ' "{{out}}/fixture/board.log" || { echo "m9-sim-walk: FAIL the fixture walk never reached the map"; exit 1; }
    grep -q '^\[PAGE\] name=compose ' "{{out}}/fixture/board.log" || { echo "m9-sim-walk: FAIL the fixture walk never reached compose"; exit 1; }
    grep -q 'restore=resumed' "{{out}}/real/board.log" || { echo "m9-sim-walk: FAIL the real store's second boot did not resume"; exit 1; }
    grep -q '^\[STORE\] page_read ' "{{out}}/real/board.log" || { echo "m9-sim-walk: FAIL no chat page was read from the ring"; exit 1; }
    grep -q '^\[PAGE\] name=house ' "{{out}}/fixture/board.log" || { echo "m9-sim-walk: FAIL the house key never opened the settings"; exit 1; }
    # 1478b: the LR1110's boot line, in the board's shape (the sim's
    # stand-in, versions 0), and the SPI2 arbiter's line beside it.
    grep -qE '^\[LORA\] init ok hw=0x[0-9a-f]{2} fw=0x[0-9a-f]{4} phy=869463000/125000/8/5 ' "{{out}}/fixture/board.log" || { echo "m9-sim-walk: FAIL the boot never said [LORA] init ok on the default PHY"; exit 1; }
    grep -m 2 -E '^\[(LORA|SPI)\] ' "{{out}}/fixture/board.log"
    grep -qE '^\[SPI\] SPI2 owner=[a-z]+ wait_max_us=[0-9]+ chunks=[0-9]+ ' "{{out}}/fixture/board.log" || { echo "m9-sim-walk: FAIL the boot never said the SPI2 line"; exit 1; }
    # 1561: the settings' row confirms each of the three sizes, in the
    # walk's order, and nothing else switches the type.
    [ "$(sed -n 's/^\[TYPE\] size=\([a-z]*\) via=\([a-z]*\) .*/\1:\2/p' "{{out}}/fixture/board.log" | grep -v ':boot$' | tr '\n' ' ')" = "tiny:row large:row tiny:row small:row " ] || { echo "m9-sim-walk: FAIL the settings' row did not confirm tiny, large, tiny, small"; exit 1; }
    grep -q '^\[TYPE\] size=large via=boot ' "{{out}}/real/board.log" || { echo "m9-sim-walk: FAIL the record did not keep the large type across the reboot"; exit 1; }
    # 1564: the top bar says the page and one word, in the small type, the
    # large one and on the settings; no sentence, no avatar, no cut word.
    grep '^\[SIM\] bar=' "{{out}}/real/board.log" || true
    [ "$(grep -c '^\[SIM\] bar=' "{{out}}/real/board.log")" -eq 3 ] || { echo "m9-sim-walk: FAIL the real walk did not say its three bars"; exit 1; }
    [ "$(grep -cE '^\[SIM\] bar="Chats +Verbunden" ' "{{out}}/real/board.log")" -eq 2 ] || { echo "m9-sim-walk: FAIL a list bar said more than the page and its word"; exit 1; }
    [ "$(grep -cE '^\[SIM\] bar="Einstellungen +Verbunden" ' "{{out}}/real/board.log")" -eq 1 ] || { echo "m9-sim-walk: FAIL the settings' bar said more than the page and its word"; exit 1; }
    # 1575: the pull walk, the fold costing the board's 3.6 ms an entry
    # (window 16b), with the drain's budget and without it (as before):
    # every key's frame under 100 ms with it, the control over it without.
    for mode in budget none; do
        dir="$root/{{out}}/pull-$mode"
        rm -rf "$dir" && mkdir -p "$dir"
        budget=""; [ "$mode" = none ] && budget="--fold-budget-us 0"
        (cd "$dir" && sim --store pulling --fold-cost-us 3600 $budget --script "$root/scripts/m9-sim-walks/pull.walk" --text > text.txt 2> board.log)
        max="$(sed -n 's/^\[SIM\] key_ms=\([0-9]*\) .*/\1/p' "$dir/board.log" | sort -n | tail -1)"
        keys="$(grep -c '^\[SIM\] key_ms=' "$dir/board.log" || true)"
        echo "m9-sim-walk: pull budget=$mode keys=$keys key_ms_max=${max:-none}"
        [ "${keys:-0}" -eq 24 ] || { echo "m9-sim-walk: FAIL the pull walk ($mode) answered $keys of 24 keys"; exit 1; }
    done
    max_budget="$(sed -n 's/^\[SIM\] key_ms=\([0-9]*\) .*/\1/p' "{{out}}/pull-budget/board.log" | sort -n | tail -1)"
    max_none="$(sed -n 's/^\[SIM\] key_ms=\([0-9]*\) .*/\1/p' "{{out}}/pull-none/board.log" | sort -n | tail -1)"
    [ "$max_budget" -lt 100 ] || { echo "m9-sim-walk: FAIL a key under the pull waited $max_budget ms with the budget"; exit 1; }
    [ "$max_none" -ge 100 ] || { echo "m9-sim-walk: FAIL the control: without the budget no key waited 100 ms ($max_none)"; exit 1; }
    echo "m9-sim-walk: PASS"

# 1530, #232: the M9 in a window on the host: the panel at `scale` (2 or 3),
# the host's keys as the M9's (arrows the ring, Enter its centre, Esc Back,
# F1-F5 the bubbles, house, square bubble, pin, map, the letters the QWERTY
# block). `store` is `fixture`, `real` or a flash image a run saved with
# `--save`. For the host's own triple, as `loquax-gui`: winit opens X11 and
# Wayland with dlopen, which a musl-static binary cannot.
[doc('Run the M9 sim in a window (#232)')]
m9-sim store="fixture" scale="2":
    cargo run -p loquax-m9 --features sim-window --bin loquax-m9-sim --target "$(rustc -vV | sed -n 's/^host: //p')" -- --window --scale {{scale}} --store {{store}}

# 1441: rewrite the baseline `window-bench` measures against, after a change
# whose cost was measured and accepted in its commit message; the commit
# that writes it is the next runs' BASE.
[doc('Rewrite the window-bench baseline from a host run (#474)')]
window-bench-baseline:
    #!/usr/bin/env bash
    set -euo pipefail
    host="$(rustc -vV | sed -n 's/^host: //p')"
    bench="$(cargo test --release -p loquax --features gui --bins --target "$host" --no-run --message-format=json-render-diagnostics | python3 -c 'import json,sys
    for l in sys.stdin:
        m = json.loads(l)
        if m.get("reason") == "compiler-artifact" and m.get("executable") and m["target"]["name"] == "loquax" and m["profile"]["test"]:
            print(m["executable"])')"
    LOQUAX_WINDOW_BENCH="$(realpath loquax/tests/window_bench.baseline)" "$bench" --exact gui::bench::window_bench --nocapture
