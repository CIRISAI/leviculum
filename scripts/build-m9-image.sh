#!/usr/bin/env bash
#
# The ThinkNode M9's Loquax image (order 1361, slice four of "The M9
# frontend: the plan, measured" in docs/src/concepts/handheld-messenger.md,
# Codeberg #232 and #473): build it for xtensa-esp32s3-none-elf, package it
# as espflash flashes it, and print its headroom the way the nRF flash
# budget prints its own (scripts/check-nrf-flash-budget.sh):
#
#   FLASH_HEADROOM board=thinknode_m9 used=<bytes> free=<bytes>
#   STACK_HEADROOM board=thinknode_m9 stack=<bytes> need=<bytes>
#   STACK_HEADROOM board=thinknode_m9 ui_stack=<bytes> need=<bytes>
#
# `used` is the application image espflash writes, `free` what is left of
# the application partition of the M9's table (`leviculum-esp/
# partitions-m9.csv`, 8 MiB since 1414 made room for the stores), both read
# off espflash's own "App/part. size: <used>/<partition> bytes" line.
#
# Two stacks since 1493 (the two-core split): the UI core's static stack
# (the APP_CORE_STACK symbol in the ELF) carries every scene and draw;
# core 0's `.stack` section carries the node, the client and the flash.
#
# Core 0's floor is measured on this board since 1582 (it was the LNode's
# node peak, 111,000 B, until then). The rule: the deepest core 0 use the
# board's painted stack showed, plus a 16 KiB reserve. The reserve covers
# what the bench did not drive and the paint cannot see:
#   - the deepest static call chain of the same image is 10,032 B deeper
#     than the measure (88,624 B: main -> Handheld::take_code ->
#     Handheld::client -> home::rebuild -> home::model -> ... the open
#     chat's bubbles, `scripts/m9-stack-frames.py`), a forget or a join
#     while a chat is open;
#   - two nested interrupts, 2,624 B: each the 256 B exception frame,
#     128 B of dispatch and 928 B of the deepest handler (kb_tick);
#   - 3,728 B for calls through a vtable or a fn pointer, which no static
#     chain follows.
# Provenance: ThinkNode M9 on the rig, bench image 2f5caa4a (1581;
# thinknode_m9-bench.elf sha256 282655c5...), window 18, 2026-11-08
# 23:00:12-23:00:16 CET: the join from an erased store (`[HOME]
# code=taken`, `restore=rebuilt`, then the first 330-entry page of a
# 4345-entry fixture pull, TLSF fold) took `stack_free=` from 72,564 to
# 31,904 of a 111,008 B stack: 111,008 - 512 guard - 31,904 = 78,592 B.
# Log: rig-run/m9/2026-11-07h/board.log (window 18 from the offset in
# rig-run/m9/2026-11-08j/board-offset-after-flash.txt). Resumed boots,
# the bench, hang and marks walks with their frame dumps and the map
# stayed at or under 66,716 B.
#
# The UI core's floor is measured the same way since 1594 (it was the
# open chat's census ceilings of loquax-m9/src/census.rs added up on the
# 64-bit host, 184,000 B, until a board's `[UART] ui_stack_free=`, 1582,
# read it): the 188,416 B stack less its 512 B guard less the least
# untouched, plus the 16,384 B reserve. 188,416 - 512 - 138,784 + 16,384
# = 65,504 B. Provenance: ThinkNode M9 on the rig, bench image of
# fbe1e3f6 (thinknode_m9-bench.bin sha256 7ced64f8..., ELF e9639d85...),
# window 19, 2026-11-09 02:11:54 CET: the `marks-try1` walk, a Bench chat
# opened with a fresh arrival after a `bench arrive` fold, took
# `ui_stack_free=` to 138,784 (49,120 B used under the guard), the least
# of every walk of the window (pull, bench, hang, forget, marks).
# Log: rig-run/m9/2026-11-09a/marks-try1/board.log. The static chain of
# ui_main in 2f5caa4a is 62,288 B (the boot census's walk), inside it.
# The stack array stays at 188,416 B for now: the 122,912 B above the
# floor are internal DRAM, and where they go (core 0's stack, which sits
# at 30,848 B free at a join, or the heap) is a decision of its own.
#
# Red when the image does not build, when fewer than 8 KiB of the partition
# are free (the nRF floor, for the nRF reason), when the stack is below the
# need, or when the image names this tree's path or the home directory.
#
# The image is the same bytes from any clone of the same commit (order
# 1458, scripts/check-m9-reproducible.sh guards it): rustc gets the path
# remap of scripts/nrf-rustflags.sh for the xtensa triple, so no absolute
# path of the building tree, the cargo home or the toolchain is compiled in,
# and SOURCE_DATE_EPOCH is the commit's date unless the caller set one,
# because esp-bootloader-esp-idf's build script stamps the application
# descriptor with that time, else with the clock.
#
# The remap is not enough on its own. Cargo hashes the ABSOLUTE path of a
# path dependency outside the building workspace into its `-C metadata`,
# and leviculum-esp is a workspace of its own whose dependencies
# (leviculum-core, loquax, ...) live beside it. That hash is the crate's
# StableCrateId, so it lands in every mangled symbol, and LTO folds and
# inlines differently on different names: on 2026-11-06 two clones of one
# commit, both remapped, linked 1,217,088 and 1,213,952 B, with
# `leviculum_core` as Csdsi... in one and Cs7459... in the other. So the
# build runs in a private user and mount namespace (`unshare`, util-linux)
# that binds the tree at one fixed path, $CANON, whatever its real path is.
# The namespace's /mnt is a fresh tmpfs, so nothing on the host is touched
# and concurrent builds do not meet. RUSTC_WRAPPER is emptied in there: an
# sccache server started outside cannot see $CANON. A host that refuses
# unprivileged user namespaces builds in place, says so, and its image is
# not the reproducible one.
#
# The esp toolchain is optional per host (see `build-esp32` in the
# Justfile): without ~/export-esp.sh this says SKIP, unless
# M9_IMAGE_REQUIRE_ESP=1 makes that a failure.
#
# The bench image (order 1533) is the same build with LEVICULUM_M9_BENCH=1,
# which leviculum-esp/build.rs turns into `cfg(m9_bench)` (its comment says
# why that is no cargo feature):
# a TYPE_KEY_INJECT control frame presses a key, and the boot says
# `[BENCH] inject=on`. It is built into its own target directory,
# `<target>/bench/`, so the release image `just flash-m9` writes is never
# a bench one, and it is packaged as `thinknode_m9-bench.bin` beside its
# ELF. Its sha256 is printed as `M9_IMAGE_SHA kind=bench sha256=<hex>`,
# the release image's as `kind=release`.
#
# Usage:
#   bash scripts/build-m9-image.sh             # build, package, measure
#   bash scripts/build-m9-image.sh --bench     # the same, the bench image
#   bash scripts/build-m9-image.sh --selftest  # the verdicts on fake numbers

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ESP="$ROOT/leviculum-esp"
TAG="[m9-image]"
CANON=/mnt/leviculum
FLOOR=8192
# 49,120 B measured + 16,384 B reserve (the rule and provenance above).
UI_STACK_NEED=65504
# 78,592 B measured + 16,384 B reserve (the rule and provenance above).
IO_STACK_NEED=94976

# verdict <used> <partition> <io_stack> <ui_stack>: the headroom lines;
# 1 when any floor is undercut.
verdict() {
    local used="$1" partition="$2" stack="$3" ui_stack="$4" rc=0
    echo "FLASH_HEADROOM board=thinknode_m9 used=$used free=$((partition - used))"
    echo "STACK_HEADROOM board=thinknode_m9 stack=$stack need=$IO_STACK_NEED"
    echo "STACK_HEADROOM board=thinknode_m9 ui_stack=$ui_stack need=$UI_STACK_NEED"
    if [ $((partition - used)) -lt "$FLOOR" ]; then
        echo "$TAG FAIL $((partition - used)) B of the application partition free, below $FLOOR B."
        rc=1
    fi
    if [ "$stack" -lt "$IO_STACK_NEED" ]; then
        echo "$TAG FAIL $stack B of core 0 stack, below the $IO_STACK_NEED B this board's measured peak and its reserve need."
        rc=1
    fi
    if [ "$ui_stack" -lt "$UI_STACK_NEED" ]; then
        echo "$TAG FAIL $ui_stack B of UI stack, below the $UI_STACK_NEED B this board's measured UI peak and its reserve need."
        rc=1
    fi
    return "$rc"
}

# app_size <espflash log>: "<used> <partition>" from espflash's size line.
app_size() {
    sed -n 's/.*App\/part\. size: *\([0-9,]*\)\/\([0-9,]*\) bytes.*/\1 \2/p' "$1" | tr -d , | head -1
}

selftest() {
    local log rc=0
    mkdir -p "$ROOT/target/tmp"
    log="$(mktemp "$ROOT/target/tmp/m9-image-selftest.XXXXXX")"
    printf 'Chip type:         esp32s3\nApp/part. size:    927,520/16,384,000 bytes, 5.66%%\n' >"$log"
    [ "$(app_size "$log")" = "927520 16384000" ] || { echo "$TAG selftest FAIL: size line not read"; rc=1; }
    rm -f "$log"
    verdict 927520 16384000 112000 188416 >/dev/null || { echo "$TAG selftest FAIL: a roomy image was refused"; rc=1; }
    verdict 16380000 16384000 112000 188416 >/dev/null && { echo "$TAG selftest FAIL: 4000 B free passed"; rc=1; }
    verdict 927520 16384000 90000 188416 >/dev/null && { echo "$TAG selftest FAIL: a 90 kB core 0 stack passed"; rc=1; }
    verdict 927520 16384000 112000 60000 >/dev/null && { echo "$TAG selftest FAIL: a 60 kB UI stack passed"; rc=1; }
    [ "$rc" -eq 0 ] && echo "$TAG selftest ok: the size line reads, every floor refuses"
    return "$rc"
}

if [ "${1:-}" = "--selftest" ]; then
    selftest
    exit $?
fi

if [ ! -f "$HOME/export-esp.sh" ]; then
    if [ "${M9_IMAGE_REQUIRE_ESP:-0}" = 1 ]; then
        echo "$TAG FAIL no esp toolchain (~/export-esp.sh) and M9_IMAGE_REQUIRE_ESP=1"
        exit 1
    fi
    echo "$TAG SKIP no esp toolchain on this host (~/export-esp.sh; scripts/install-ci.sh installs it)"
    exit 0
fi

# shellcheck source=scripts/cargo-target-dir.sh
source "$ROOT/scripts/cargo-target-dir.sh"
# shellcheck source=scripts/nrf-rustflags.sh
source "$ROOT/scripts/nrf-rustflags.sh"

# The bench image's name and cargo arguments (1533), from M9_BENCH=1, which
# --bench sets for this script and the namespace hop below.
BENCH="${M9_BENCH:-0}"
if [ "${1:-}" = "--bench" ]; then
    BENCH=1
    shift
fi
export M9_BENCH="$BENCH"
NAME=thinknode_m9
KIND=release
if [ "$BENCH" = 1 ]; then
    NAME=thinknode_m9-bench
    KIND=bench
    export LEVICULUM_M9_BENCH=1
else
    unset LEVICULUM_M9_BENCH
fi

# target_of: where cargo writes this kind's build; the bench image's own
# directory under the release one.
target_of() {
    local target
    target="$(cargo_target_dir "$ESP")" || return 1
    if [ "$BENCH" = 1 ]; then
        target="$target/bench"
    fi
    echo "$target"
}

# build_here: build and package the image from the tree this script lies in,
# with the remap for that path. In the namespace that tree is $CANON.
build_here() {
    local target out
    # shellcheck disable=SC1091
    source "$HOME/export-esp.sh" >/dev/null || return 1
    export PATH="$HOME/.cargo/bin:$PATH"
    esp_rustflags_export || return 1
    target="$(target_of)" || return 1
    out="$target/xtensa-esp32s3-none-elf/release"
    # The release build is cargo's own default target, as before 1533.
    local where=()
    [ "$BENCH" = 1 ] && where=(--target-dir "$target")
    (cd "$ESP" && cargo build --release --locked "${where[@]}" --bin thinknode_m9) &&
        espflash save-image --chip esp32s3 --flash-size 16mb \
            --flash-mode dio --flash-freq 80mhz \
            --partition-table "$ESP/partitions-m9.csv" \
            "$out/thinknode_m9" "$out/$NAME.bin"
}

# Internal: the second half of the namespace hop below.
if [ "${1:-}" = "--build-here" ]; then
    build_here
    exit $?
fi

selftest || exit 1
target="$(target_of)" || { echo "$TAG FAIL no target directory for $ESP"; exit 1; }
OUT="$target/xtensa-esp32s3-none-elf/release"
firmware_epoch_export "$ROOT" ||
    echo "$TAG note: no git commit date here, the image carries the build clock"
mkdir -p "$ROOT/target/tmp"
log="$ROOT/target/tmp/m9-image-$KIND.log"
if unshare --user --map-root-user --mount true 2>/dev/null; then
    # shellcheck disable=SC2016 # expanded by the inner shell
    RUSTC_WRAPPER="" unshare --user --map-root-user --mount bash -c '
        mount -t tmpfs m9-build "${2%/*}" && mkdir "$2" && mount --bind "$1" "$2" &&
            cd "$2" && exec bash scripts/build-m9-image.sh --build-here' \
        bash "$ROOT" "$CANON" >"$log" 2>&1
    built=$?
else
    echo "$TAG note: no unprivileged user namespace here; built at $ROOT, not $CANON, so this image is not the reproducible one"
    (build_here) >"$log" 2>&1
    built=$?
fi
if [ "$built" -ne 0 ]; then
    tail -30 "$log"
    echo "$TAG FAIL the image did not build or package (the full log: $log)"
    exit 1
fi
read -r used partition < <(app_size "$log")
if [ -z "${used:-}" ] || [ -z "${partition:-}" ]; then
    tail -10 "$log"
    echo "$TAG FAIL espflash printed no size line (the full log: $log)"
    exit 1
fi
# shellcheck disable=SC1091
stack="$(source "$HOME/export-esp.sh" >/dev/null && xtensa-esp32s3-elf-size -A "$OUT/thinknode_m9" \
    | awk '$1 == ".stack" { print $2 }')"
# The UI core's static stack: the APP_CORE_STACK symbol's size (1493).
# shellcheck disable=SC1091
ui_stack="$(source "$HOME/export-esp.sh" >/dev/null && xtensa-esp32s3-elf-nm -S "$OUT/thinknode_m9" \
    | awk '/APP_CORE_STACK/ { print strtonum("0x" $2) }' | head -1)"
echo "$TAG image $OUT/$NAME.bin ($(stat -c %s "$OUT/$NAME.bin") B), elf $OUT/thinknode_m9"
echo "M9_IMAGE_SHA kind=$KIND sha256=$(sha256sum "$OUT/$NAME.bin" | cut -d' ' -f1)"
rc=0
verdict "$used" "$partition" "${stack:-0}" "${ui_stack:-0}" || rc=1
# The park spin (1613, leviculum-esp/src/corepark.rs) runs while a flash
# op has the cache off: the handler must lie whole in .rwtext (internal
# RAM, literals included -- the linker script keeps them beside the
# code), and its body must hold no call and no indirect jump, either of
# which could reach flash, which the op makes unreadable. This is the
# completeness proof the IRAM options of 1613 were asked to name: the
# set is one function, and the linked ELF shows it.
# shellcheck disable=SC1091
park_line="$(source "$HOME/export-esp.sh" >/dev/null && xtensa-esp32s3-elf-nm -S "$OUT/thinknode_m9" \
    | grep -m1 __esp_hal_internal_park_ipi)"
if [ -z "$park_line" ]; then
    echo "$TAG FAIL no park_ipi symbol in the ELF; the park spin (corepark.rs) is gone or renamed"
    rc=1
else
    park_addr=$((16#$(echo "$park_line" | awk '{print $1}')))
    park_size=$((16#$(echo "$park_line" | awk '{print $2}')))
    # shellcheck disable=SC1091
    read -r rw_addr rw_size < <(source "$HOME/export-esp.sh" >/dev/null &&
        xtensa-esp32s3-elf-readelf -SW "$OUT/thinknode_m9" |
        awk '{ for (i = 1; i <= NF; i++) if ($i == ".rwtext") {
            print strtonum("0x" $(i + 2)), strtonum("0x" $(i + 4)); exit } }')
    if [ -z "${rw_addr:-}" ] || [ "$park_addr" -lt "$rw_addr" ] ||
        [ $((park_addr + park_size)) -gt $((rw_addr + rw_size)) ]; then
        echo "$TAG FAIL park_ipi at $(printf 0x%x "$park_addr")+$park_size is not inside .rwtext ($(printf 0x%x "${rw_addr:-0}")+${rw_size:-0}); the spin would run from flash with the cache off"
        rc=1
    fi
    # shellcheck disable=SC1091
    park_calls="$(source "$HOME/export-esp.sh" >/dev/null &&
        xtensa-esp32s3-elf-objdump -d --start-address="$park_addr" \
            --stop-address=$((park_addr + park_size)) "$OUT/thinknode_m9" |
        grep -cE '\b(call(x)?(0|4|8|12)|jx)\b')"
    if [ "$park_calls" -ne 0 ]; then
        echo "$TAG FAIL the park spin holds $park_calls call/jx instruction(s); everything it runs must inline into the one .rwtext function"
        rc=1
    fi
fi
# The cheap half of check-m9-reproducible.sh, on every build: the shipped
# image names no path of this host (the remap above, order 1458).
for p in "$ROOT/" "$HOME/" "$CANON/"; do
    if grep -qaF -- "$p" "$OUT/$NAME.bin"; then
        echo "$TAG FAIL the image names the host path $p ($(grep -caF -- "$p" "$OUT/$NAME.bin") lines); the path remap did not reach it"
        rc=1
    fi
done
exit "$rc"
