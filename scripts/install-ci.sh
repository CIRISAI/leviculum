#!/bin/bash
# Idempotent CI installer for the Leviculum 4-tier self-hosted pipeline.
set -e

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_DIR"

# --vm-mode skips the developer-machine bits (git hooks + their
# accompanying chmods).  The VM never commits or pushes, so nothing
# needs the hooks to be live.  A worktree-scoped marker file is
# written so the tier-runners know to perform a `git fetch + checkout
# --force origin/master` at the head of every scheduled run.
#
# --check installs nothing. It verifies the provisioning this script is
# responsible for that can rot without anyone noticing -- today exactly one
# thing, the patched btvirt and its provenance sidecar (step 1c), because that
# one decides the verdict of the periculum ble_room cells rather than just
# their ability to start. Everything else this script installs announces its
# own absence the moment it is used.
VM_MODE=0
CHECK_MODE=0
for arg in "$@"; do
    case "$arg" in
        --vm-mode) VM_MODE=1 ;;
        --check) CHECK_MODE=1 ;;
        *)
            echo "ERROR: unknown flag '$arg'" >&2
            echo "Usage: $0 [--vm-mode] [--check]" >&2
            exit 1
            ;;
    esac
done

if [[ "$CHECK_MODE" -eq 1 ]]; then
    echo "[install-ci] --check: verifying installed provisioning, installing nothing"
    bash scripts/install-btvirt.sh --check
    exit $?
fi

echo "[install-ci] Installing CI pipeline in $REPO_DIR (vm-mode=$VM_MODE)"

# 1. Dependency check
MISSING=()
# A hard dependency, not an optional one: `just fast` runs `just
# nrf-shellcheck` over the flash-runner scripts (Codeberg #345), and a gate
# that silently does not run is worse than no gate. (Written this way round
# because a comment opening with the tool's own name parses as a directive.)
for cmd in just docker notify-send cargo python3 flock socat shellcheck; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        MISSING+=("$cmd")
    fi
done
if [ ${#MISSING[@]} -gt 0 ]; then
    echo "[install-ci] Missing dependencies: ${MISSING[*]}"
    echo "[install-ci] Hint: sudo apt install ${MISSING[*]}"
    exit 1
fi

# Optional test dependency: i2pd provides the SAM bridge (127.0.0.1:7656) the
# I2PInterface live tests need. The default suite covers I2PInterface with an
# in-process mock SAM bridge, so i2pd is not required to go green; it only gates
# the `#[ignore]`d live tests in leviculum-std (interfaces::i2p::i2pd_live). Warn
# rather than fail when it is absent.
if ! command -v i2pd >/dev/null 2>&1; then
    echo "[install-ci] Note: optional test dependency 'i2pd' not found"
    echo "[install-ci] Hint: sudo apt install i2pd (needed only for the ignored I2P live tests)"
fi

# Optional test dependency: lintian is the Debian-policy authority
# scripts/verify-deb-packaging.sh defers to. `just verify-deb` presupposes a
# build-deb run and is not part of any tier, so warn rather than fail — but a
# verify run without it skips the policy checks entirely.
if ! command -v lintian >/dev/null 2>&1; then
    echo "[install-ci] Note: optional test dependency 'lintian' not found"
    echo "[install-ci] Hint: sudo apt install lintian (needed for the just verify-deb policy checks)"
fi

# Optional test dependency: cargo-fuzz plus the nightly toolchain run the
# wire-parser fuzz targets (`just fuzz`, `just fuzz-nightly`, `just
# fuzz-regress`, scripts/run-fuzz.sh, Codeberg #290 and #23).
#
# WHERE THE TARGETS ARE: eight of them, in two crates. Seven under
# leviculum-core/fuzz (packet_unpack, announce_from_packet, discovery_announce,
# kiss_deframe, hdlc_deframe, ifac_verify, resource_advertisement_unpack) and
# one under leviculum-std/fuzz (sam_parse, the I2P SAM bridge). One binary and
# one toolchain serve both directories, so they get ONE note here: until
# 2026-09-27 #290 and #23 each carried their own `command -v cargo-fuzz`
# block, and a host without the tool printed the same missing dependency twice
# with two different hints.
#
# WHICH TOOLCHAIN, and why it is not the pinned one: cargo-fuzz drives
# libFuzzer through `-Z` sanitizer flags, which the repo's pinned stable
# (rust-toolchain.toml, 1.97.1) does not accept. So these targets -- and only
# these -- build on NIGHTLY. That is a deliberate exception to the pin, not a
# drift: no shipped binary comes out of this toolchain, only fuzz targets that
# never leave the host.
#
# The channel is a knob rather than a hardcode. `LEVICULUM_FUZZ_TOOLCHAIN` is
# what run-fuzz.sh passes to cargo, so a rolling `nightly` can be replaced by a
# date-pinned one on a host that wants reproducibility:
#
#   rustup toolchain install nightly-2026-06-17
#   LEVICULUM_FUZZ_TOOLCHAIN=nightly-2026-06-17 just fuzz-nightly
#
# Last verified: cargo 1.98.0-nightly (598ab48ec 2026-06-17) with cargo-fuzz
# 0.13.2, all eight targets green (2026-09-25). Every run prints the resolved
# version as a FUZZ_TOOLCHAIN line, so a nightly that moved under the corpus is
# visible in the log instead of inferred from a build failure.
#
# No tier runs the fuzzing itself, and `just standard` never builds these --
# the regression test for any crash they find lands in the normal unit suite.
# `just fuzz-selftest` and `just fuzz-regress` are on the push path but both
# skip with a named reason when these are absent, so warn rather than fail.
# Longer form: docs/src/development-testing.md.
#
# The hint is `cargo install`, not apt: Debian packages no cargo-fuzz (checked
# 2026-09-27, `apt-cache show cargo-fuzz` -> no packages found), and this bench
# runs the crates.io build out of ~/.cargo/bin (cargo-fuzz 0.13.2).
if ! command -v cargo-fuzz >/dev/null 2>&1; then
    echo "[install-ci] Note: optional test dependency 'cargo-fuzz' not found"
    echo "[install-ci] Hint: cargo install cargo-fuzz && rustup toolchain install nightly"
    echo "[install-ci]       (no Debian package; needed for 'just fuzz' over"
    echo "[install-ci]        leviculum-core/fuzz and leviculum-std/fuzz, and"
    echo "[install-ci]        'just fuzz-selftest' / 'just fuzz-regress' skip"
    echo "[install-ci]        without it -- see docs/src/development-testing.md)"
fi

# Optional test dependency: nomadnet drives the on-demand lnomad acceptance
# (scripts/lnomad_nomadnet_acceptance.sh). Not part of any tier, so warn rather
# than fail when it is absent.
if ! command -v nomadnet >/dev/null 2>&1; then
    echo "[install-ci] Note: optional test dependency 'nomadnet' not found"
    echo "[install-ci] Hint: pip install nomadnet (needed only for the lnomad acceptance)"
fi

# Optional test dependency: valgrind's massif is the only instrument that says
# WHICH CALL SITE the live heap belongs to — the counting allocator in
# leviculum-std/src/heap_accounting.rs gives the total and nothing else. It
# works on a gnu-target build of `heap-gap-bench` only (massif interposes on
# malloc by symbol, and the musl-static default has nothing to interpose on:
# it records mem_heap_B=0). No tier runs it; it is reached by hand during a
# heap investigation, so warn rather than fail.
if ! command -v valgrind >/dev/null 2>&1; then
    echo "[install-ci] Note: optional test dependency 'valgrind' not found"
    echo "[install-ci] Hint: sudo apt install valgrind"
    echo "[install-ci]       (massif call-site attribution for heap-gap-bench;"
    echo "[install-ci]        see the binary's module docs for the --alloc-fn list)"
fi

# Optional test dependency: the other half of the same investigation. massif
# and the counting allocator both say what the PROGRAM asked for; neither can
# say which musl size class the resident set is sitting in. mallocng keeps
# that in symbols a musl-static binary carries, and scripts/mallocng-census.gdb
# reads them out of a running heap-gap-bench without instrumenting our code.
# Reached by hand during a heap investigation, so warn rather than fail.
if ! command -v gdb >/dev/null 2>&1; then
    echo "[install-ci] Note: optional test dependency 'gdb' not found"
    echo "[install-ci] Hint: sudo apt install gdb"
    echo "[install-ci]       (scripts/mallocng-census.gdb, the per-size-class"
    echo "[install-ci]        census behind heap-gap-bench's ratio)"
fi

# Optional rig dependency: uhubctl cuts and restores power on a single USB
# hub port, which is how a board that stopped enumerating gets recovered
# without touching it (scripts/install-usbhub-helper.sh wires the
# passwordless sudo the runner needs; scripts/run-tier3-hw.sh uses it between
# profiles). Only the hardware host has a hub to drive, so warn rather than
# fail — a VM runner has nothing to power-cycle.
if ! command -v uhubctl >/dev/null 2>&1; then
    echo "[install-ci] Note: optional rig dependency 'uhubctl' not found"
    echo "[install-ci] Hint: sudo apt install uhubctl"
    echo "[install-ci]       (needed only to power-cycle a hung board's hub port)"
fi

# 1c. Optional test dependency: btvirt hosts the periculum `ble_room_*` cells
# (periculum #49): N virtual LE controllers on one emulated air, so N lnsd
# daemons can prove BLE mesh formation with no boards. Debian packages no
# btvirt, and a STOCK build of it is not good enough either -- bluez <= 5.82's
# emulator hands the peripheral the central's connection handle, which stalls
# every room from the second concurrent link (N >= 3). So the build is not a
# hint in a comment any more: scripts/install-btvirt.sh fetches the bluez
# source matching this host, applies the vendored upstream fix
# (scripts/patches/, commit 4ff7deaf8c), builds emulator/btvirt, installs it,
# and writes the provenance sidecar the room prints as `origin=`. It is
# idempotent and it warns rather than fails when a prerequisite (deb-src,
# sudo, a compiler) is missing -- only the BLE bench needs any of this. A
# source tree that already carries the fix is recognised and left alone, so
# the first bluez release that ships it needs no change here.
#
# Re-verify later without installing anything: bash scripts/install-ci.sh --check
# Drive the patch and sidecar logic with no root and no build, against a
# source tree synthesised from the vendored patch: just btvirt-selftest.
#
# THE CEILING: SIXTEEN CONTROLLERS, AND THE CELL THAT DECLARES IT
#
# btvirt holds at most sixteen emulated controllers -- `MAX_BTDEV_ENTRIES`
# is 16 in bluez's `emulator/btdev.c`, in the 5.82 this host builds from and
# in upstream master (read 2026-09-27 at bluez HEAD 8b4a4176). Measured the
# same day: `btvirt -L -l16` runs, `-l17` exits at once with "Failed to open
# Virtual HCI device". So sixteen is the largest room this bench can host,
# and it is green on the patched binary (240 of 240 ordered probes). Above
# it nothing here can help: a second emulator instance sharing one air, a
# patched ceiling, or real radios.
#
# That is why periculum's `regression/ble_room_20.toml` carries an
# `[unsupported]` section rather than a red -- twenty nodes cannot be built
# here at all, and the cell skips as infra after 21 s with the kernel
# showing no new controllers. Do not read that skip as a bug in lnsd or in
# this script. `ble_room_2`, `ble_room_10` and the rest run normally.
#
# Prerequisites the cells need beyond the binary (one-time provisioning):
#   - kernel hci_vhci module, loaded at boot and group-writable:
#       echo hci_vhci | sudo tee /etc/modules-load.d/hci_vhci.conf
#       echo 'KERNEL=="vhci", GROUP="bluetooth", MODE="0660"' | \
#         sudo tee /etc/udev/rules.d/60-vhci-bluetooth.rules
#     (the executing user must be in the bluetooth group)
#   - the SYSTEM bluetoothd running: it must own the vhci adapters the
#     moment btvirt creates them — lnsd reaches controllers only through
#     BlueZ, and the cells do not start their own daemon
#   - NO BLE MIDI GATT service on the bench, or agentless SMP pairing
#     kills every room connection before the Columba handshake
#     (root-caused 2026-09-09). Two registrars to silence:
#       * bluetoothd's midi plugin: systemd drop-in overriding ExecStart
#         with `/usr/libexec/bluetooth/bluetoothd --noplugin=midi`
#       * WirePlumber's bluez monitors, SYSTEM-WIDE (the GDM session runs
#         its own instance): /etc/wireplumber/wireplumber.conf.d/ snippet
#         with `monitor.bluez = disabled` and `monitor.bluez-midi =
#         disabled` in the main profile
#   - btmon usable by the executing user, for the room's optional HCI
#     capture (PERICULUM_BLE_ROOM_BTMON): the monitor channel needs
#     CAP_NET_RAW, granted once with
#       sudo setcap cap_net_raw+ep /usr/bin/btmon
# Warn-only: only the bench that runs the BLE room needs any of it, and none
# of the four is something this script can do for you.
if ! bash scripts/install-btvirt.sh; then
    echo "[install-ci] WARNING: btvirt install failed; the periculum ble_room cells"
    echo "[install-ci]          will not run here. See the output above."
fi

# 2. Activate git hooks (developer-machine mode only)
if [[ "$VM_MODE" -eq 0 ]]; then
    git config core.hooksPath .githooks
    echo "[install-ci] git core.hooksPath -> .githooks"
else
    echo "[install-ci] --vm-mode: skipping git core.hooksPath config"
fi

# 3. chmod hook + runner scripts
if [[ "$VM_MODE" -eq 0 ]]; then
    # commit-msg is the machine-authorship trailer guard (Codeberg #205). It
    # is convenience only — the enforcement is .woodpecker/commit-trailers.yml,
    # because a fresh clone has none of these hooks.
    #
    # These two are the whole set. A post-commit hook detached
    # `scripts/run-tier1.sh` — a 15-40 min background docker run — after every
    # commit until 2026-08-07; the rule that removed it is in
    # docs/src/concepts/checks-and-citations.md ("What may live in a git hook").
    chmod +x .githooks/pre-push .githooks/commit-msg
fi
chmod +x scripts/run-tier1.sh scripts/run-tier2.sh scripts/run-tier3.sh scripts/run-tier3-hw.sh
chmod +x scripts/flash-lnodes-from-head.sh
chmod +x scripts/ci-status.sh
chmod +x scripts/check-submodule-pins.sh scripts/check-commit-trailers.sh
chmod +x scripts/install-ci.sh
echo "[install-ci] runner scripts made executable"

# 4. State directory
mkdir -p ~/.local/state/leviculum-ci
echo "[install-ci] state dir: ~/.local/state/leviculum-ci"

# 5. Separate cargo target dir
mkdir -p ~/.cache/leviculum-ci-target
echo "[install-ci] cargo target dir: ~/.cache/leviculum-ci-target"

# 5b. The sweeper those target directories need (Codeberg #381). A day of
#     gate runs adds well over a hundred gigabytes of artefacts that cargo
#     never removes, and on the host that runs these tiers a full root
#     volume does not fail loudly: it turns hardware runs red for want of
#     space and makes the dispatcher refuse work. `just sweep` is what
#     bounds the two target directories without paying for a full rebuild,
#     and it is the tool, not the recipe, that is usually missing -- a
#     sweeper that is not installed makes every hygiene job a silent no-op.
#     Installed rather than hinted at for that reason; idempotent like the
#     lines below it. Unpinned, because nothing diffs its output: it deletes
#     rebuildable artefacts and a newer version deletes them just as well.
cargo install --locked cargo-sweep
echo "[install-ci] build-directory sweeper: cargo-sweep (just sweep)"

# 6. Firmware build toolchain.  flip-link is the firmware linker
#    (stack-overflow protection, Codeberg #50); run-tier3-hw.sh builds
#    the firmware via `just flash*`.  Both lines are idempotent: the
#    rustup target is a no-op once added, and `cargo install` skips a
#    crate that is already present at the requested version.
rustup target add thumbv7em-none-eabihf
cargo install --locked flip-link
#    llvm-tools ships llvm-objdump, which `just nrf-stack-frames` uses to
#    read the frame-allocating `sub sp` immediates out of the linked ELF
#    when binutils-arm-none-eabi is absent.
rustup component add llvm-tools
echo "[install-ci] firmware toolchain: thumbv7em-none-eabihf + flip-link + llvm-tools"

# 6b. Third-party licence notices (Codeberg #288). `just notices-guard`
#     runs in Tier 0, so this is a hard dependency of the push path, not
#     an optional extra — hence an install rather than the warn-only
#     treatment the test-only tools above get.
#
#     Pinned: the generated file is checked in and diffed byte for byte,
#     so a cargo-about that formats or classifies anything differently
#     turns the gate red on every machine that has the other version.
#     Bumping the pin is a deliberate act with a `just notices` commit
#     next to it.
#
#     `--features cli` is not optional: without it the crate builds as a
#     library and cargo installs no binary at all, reporting only a
#     warning.
cargo install --locked cargo-about --version 0.9.2 --features cli
echo "[install-ci] licence tooling: cargo-about 0.9.2 (just notices / just notices-guard)"

# 6c. ESP32-S3 (Xtensa) firmware toolchain.  ESP32-S3 is an Xtensa part,
#     not RISC-V, so stock rustc cannot target it at all: there is no
#     `rustup target add xtensa-esp32s3-none-elf`, and the esp-rs fork of
#     the compiler is the only thing that emits that triple.  espup is
#     what installs the fork (plus the Xtensa LLVM and the xtensa-esp-elf
#     GCC the linker needs).  espflash is the probe-rs of these parts: it
#     wraps the linked ELF in a bootloader image and writes it over the
#     USB serial/JTAG port.
#
#     Pinned, like cargo-about above and for the reason rust-toolchain.toml
#     gives for the host pin: this is embedded code where the compiler
#     generation decides codegen, and an unattributable stack-frame or
#     image-size move is worse than a scheduled bump.
#
#       espup 0.17.1          newest release; 0.17.0 is yanked upstream.
#       Xtensa Rust 1.97.0.0  newest esp-rs toolchain NOT marked
#                             prerelease (1.98.0.0 / 1.98.1.0 still are).
#                             It is rustc 1.97.0-nightly, i.e. the same
#                             generation as the 1.97.1 host pin, so the
#                             two compilers do not disagree about the
#                             core crate a shared crate is built with.
#       espflash 4.6.0        newest release; produces the image layout
#                             esp-hal 1.2.x expects.
#
#     `--targets esp32s3` instead of espup's `all` default: every extra
#     chip is more download and more disk, and there is exactly one
#     ESP32-class board in the rig (the Heltec V4 — `esptool chip_id`
#     says ESP32-S3, QFN56, rev v0.2).  Widen the list when a second
#     one appears; espup reuses what is already installed.
#     `--stable-version` pins what espup would use for RISC-V parts; it
#     installs nothing today, because an Xtensa-only target list needs no
#     stable toolchain, and it keeps a future esp32c* addition on the
#     same compiler as rust-toolchain.toml.
#
#     Both steps are idempotent: cargo install skips a crate already at
#     the requested version, and a repeat `espup install` of the same
#     version reuses the existing GCC/LLVM/Rust trees (measured on
#     schneckenschreck: 1.3 s, no download, identical byte count).
#
#     Disk cost, measured 2026-09-15: ~1.9 GB under
#     ~/.rustup/toolchains/esp (790 MB xtensa-esp-elf GCC, 335 MB Xtensa
#     LLVM, the rest Xtensa rustc + rust-src) plus ~450 MB of cargo
#     registry and the two binaries.  Not free — check `df` before
#     running this on a host that is near full.
#
#     Building for the chip additionally needs the environment espup
#     writes to ~/export-esp.sh (LIBCLANG_PATH, and xtensa-esp-elf-gcc on
#     PATH); source it in the shell that runs cargo.
#
#     No pre-built core/alloc ships for this triple, so leviculum-esp
#     builds them itself (`build-std` in its .cargo/config.toml).  That
#     needs the `rust-src` component, which the espup install above
#     already places in the esp toolchain; nothing further is required
#     here for `just build-esp32` to run.
cargo install --locked espup --version 0.17.1
"$HOME/.cargo/bin/espup" install \
    --toolchain-version 1.97.0.0 \
    --stable-version 1.97.1 \
    --targets esp32s3
cargo install --locked espflash --version 4.6.0
echo "[install-ci] ESP32-S3 toolchain: espup 0.17.1 + Xtensa Rust 1.97.0.0 + espflash 4.6.0"
echo "[install-ci]   xtensa builds need: . ~/export-esp.sh"

# 6d. esptool, the one that can read and write an ESP32-S3 (2026-09-17).
#     espflash above writes OUR firmware; this is the other direction —
#     reading Mark's signed RNode images off a board and putting them back,
#     which is what `just flash-rnode-extract` / `just flash-rnode` /
#     `just flash-rnode-write-image` do.
#
#     It is installed here, and not left to the flashing recipe, because the
#     recipe is reached for exactly when a board is already in trouble. On
#     2026-09-17 a Heltec V4 needed restoring and the tooling did not fit:
#     Debian's esptool 4.7.0 is dfsg-stripped of its flasher stubs (the
#     Xtensa ones are prebuilt binaries, so esp32, esp32s2 and esp32s3 are
#     all missing while the RISC-V set survives) and died with
#
#       FileNotFoundError: .../stub_flasher/stub_flasher_32s3.json
#
#     The `--no-stub` fallback reached 12 % of a 16 MB read before failing
#     with `Failed to read flash block (result was 01090000: CRC or checksum
#     was invalid)`. The pinned PyPI build read the same 16 MB in 102.8 s at
#     1306 kbit/s with no retries (.rnode-fw/extract.log).
#
#     Pinned at 5.4.0 for the reason the pins above give, plus one specific
#     to this tool: esptool 5 renamed every command and option to a
#     hyphenated form and scripts/rnode-flash.sh composes that form, which
#     a 4.x binary rejects outright.
#
#     Idempotent, and cheap when it has nothing to do: 0.06 s to confirm an
#     existing install (measured), 3.1 s to build the venv from scratch.
#     The script is also what `just flash-rnode-setup` runs, so a developer
#     machine and a CI host end up with the same binary at the same path.
#     Needs python3-venv, which is separate from python3 on Debian.
bash scripts/install-esptool.sh
echo "[install-ci] RNode flashing: esptool 5.4.0 (just flash-rnode-*)"

# 6e. The Python Reticulum a HOST `type = "python"` periculum node runs
#     (periculum 376, eb4bee0). Same shape as the esptool step above -- a
#     pinned tool in a venv the runner owns -- and installed for the same
#     reason it is: a python node in a CONTAINER imports what the image
#     installs last, the pinned `rns==<pin>` wheel, but a python node run as
#     a HOST process (every `emulated/` cell, and a BLE node) has no image
#     to import from. Until this venv exists such a cell does not quietly
#     run the older vendored tree either -- it SKIPS, as SKIPPED_INFRA with
#     `reason=image_runtime_missing`, naming the script below.
#
#     WHERE IT LANDS: `~/.local/state/leviculum-ci/rns-<pin>`, beside the CI
#     state directory step 4 creates, one directory per pin. The pin is read
#     out of periculum's `periculum/assets/Dockerfile` rather than written
#     down a second time, so the venv and the containers carry the same
#     string by construction; and because the pin is in the directory NAME, a
#     pin bump is a venv that does not exist yet (a loud named skip) instead
#     of an existing venv serving the version before the bump.
#     $PERICULUM_PYTHON_RUNTIME_DIR overrides the parent directory; the
#     runner reads the same variable.
#
#     WHY IT IS GUARDED and warn-only rather than a hard dependency: the
#     script lives in periculum because periculum's runner is what resolves a
#     node onto the venv, so a host without the sibling checkout has no
#     corpus to run and needs no runtime -- it gets a note naming the skip
#     token, not a failure. Warn-only on failure for the btvirt step's
#     reason: only the bench that runs the emulated cells needs this, and an
#     installer that stops here helps no other host.
#
#     Idempotent, and cheap when it has nothing to do: a venv that already
#     answers with the pin and imports LXMF is left alone at the cost of one
#     interpreter start, printing the `already carries RNS <pin>` line.
PERICULUM_ROOT="${PERICULUM_ROOT:-$REPO_DIR/../periculum}"
PYTHON_RUNTIME_INSTALLER="$PERICULUM_ROOT/scripts/install-python-runtime.sh"
if [ -f "$PYTHON_RUNTIME_INSTALLER" ]; then
    if bash "$PYTHON_RUNTIME_INSTALLER"; then
        echo "[install-ci] host python runtime: RNS venv under" \
             "${PERICULUM_PYTHON_RUNTIME_DIR:-~/.local/state/leviculum-ci}" \
             "(periculum host 'type = \"python\"' cells)"
    else
        echo "[install-ci] WARNING: host python runtime install failed; periculum's host"
        echo "[install-ci]          python cells will skip with reason=image_runtime_missing."
        echo "[install-ci]          See the output above."
    fi
else
    echo "[install-ci] Note: no periculum checkout at $PERICULUM_ROOT, so the host"
    echo "[install-ci]       python runtime (~/.local/state/leviculum-ci/rns-<pin>) is"
    echo "[install-ci]       not provisioned. periculum's host 'type = \"python\"' cells"
    echo "[install-ci]       skip with reason=image_runtime_missing until it is; set"
    echo "[install-ci]       PERICULUM_ROOT or clone the sibling, then re-run this script."
fi

# 7. Install systemd user units, patching the hardcoded
#    %h/coding/libreticulum literal to point at the worktree this
#    installer was actually run from.  Lets a `git worktree`-based
#    second checkout (e.g. ~/coding/libreticulum-ci) install its
#    own units that fire against itself, instead of silently
#    targeting the developer's primary checkout.
SYSTEMD_USER_DIR=~/.config/systemd/user
mkdir -p "$SYSTEMD_USER_DIR"
for unit in scripts/systemd/leviculum-ci-tier2.service \
            scripts/systemd/leviculum-ci-nightly.service \
            scripts/systemd/leviculum-ci-nightly.timer; do
    sed "s|%h/coding/libreticulum|$REPO_DIR|g" "$unit" \
      > "$SYSTEMD_USER_DIR/$(basename "$unit")"
done
echo "[install-ci] systemd user units installed in $SYSTEMD_USER_DIR (path: $REPO_DIR)"

# 8. Reload systemd
systemctl --user daemon-reload

# 9. Enable timers.  Tier 2 is ON-DEMAND (Lew, 2026-06-12): only the
#    nightly stays scheduled.  Start tier2 manually when needed:
#      systemctl --user start leviculum-ci-tier2.service
#    Upgrade path: drop a previously-installed tier2 timer.
systemctl --user disable --now leviculum-ci-tier2.timer 2>/dev/null || true
rm -f "$SYSTEMD_USER_DIR/leviculum-ci-tier2.timer"
systemctl --user enable --now leviculum-ci-nightly.timer
echo "[install-ci] nightly timer enabled; tier2 is on-demand"

# 10. LoRa hardware probe (warning only)
if ! ls /dev/ttyACM* >/dev/null 2>&1; then
    echo "[install-ci] WARNING: no /dev/ttyACM* devices found — LoRa tests will skip in nightly."
fi

# 11. Worktree-scoped vm-mode marker.  Tier-runners check for this
#     file inside their git-dir before running _repo-sync.sh.  Marker
#     is per-worktree (not per-user) so a manual `bash run-tier2.sh`
#     from the developer's primary checkout never triggers a
#     destructive `git checkout --force` against the wrong tree.
if [[ "$VM_MODE" -eq 1 ]]; then
    GIT_DIR=$(git rev-parse --git-dir)
    touch "$GIT_DIR/leviculum-ci-vm-mode-marker"
    echo "[install-ci] vm-mode marker: $GIT_DIR/leviculum-ci-vm-mode-marker"
fi

# Summary
echo ""
echo "[install-ci] Installation complete."
echo ""
echo "  Run manually:    just fast | just standard | just extensive | just nightly"
echo "  Show status:     just status"
echo "  Logs:            ~/.local/state/leviculum-ci/"
echo "  Timers:          systemctl --user list-timers"
echo ""
echo "  Nothing starts Tier 1 for you. The pre-push hook runs Tier 0 only;"
echo "  Tier 1 is 'just standard', typed once per batch. A post-commit hook"
echo "  used to detach it after every commit — removed 2026-08-07."
echo "  A cold Tier 1 compiles the whole workspace: plan for 20-40 min."
