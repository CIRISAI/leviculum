#!/usr/bin/env bash
# Where cargo writes build artefacts — asked, not assumed.
#
# `<workspace>/target` is a default, not a fact: CARGO_TARGET_DIR in the
# environment and `build.target-dir` in a config file both move it, and the
# nightly's fresh-tree wrapper sets the former on purpose so the build cache
# outlives the throwaway clone. A script that hardcodes `$ROOT/target` then
# builds successfully and fails to find what it just built.
#
# That is what happened in /home/lew/ci/push-tree on 2026-09-11: every test in
# `just fast` passed, then `nrf-stack-frames` stopped with "missing ELF"
# because cargo had written the firmware into the CI's external target
# directory while the gate looked inside the tree.
#
# Source this file and call `cargo_target_dir <dir-inside-the-workspace>`;
# `cargo metadata` answers in ~40 ms and is the only party that knows.
#
#   source "$ROOT/scripts/cargo-target-dir.sh"
#   TARGET="$(cargo_target_dir "$ROOT/leviculum-nrf")"
#
# It takes a directory because this repo holds TWO workspaces — the host one at
# the root and the firmware one in leviculum-nrf — whose target directories
# differ whether CARGO_TARGET_DIR is set or not.

# shellcheck shell=bash

cargo_target_dir() {
    local dir="${1:?cargo_target_dir: need a directory inside the workspace}"
    local meta
    # `--no-deps` keeps this to the workspace's own manifests: no registry
    # lookup, no dependency resolution, nothing to fetch when offline.
    meta="$(cd "$dir" && cargo metadata --format-version 1 --no-deps)" || {
        echo "cargo-target-dir: 'cargo metadata' failed in $dir" >&2
        return 1
    }
    printf '%s' "$meta" | python3 -c '
import json, sys
print(json.load(sys.stdin)["target_directory"])
' || {
        echo "cargo-target-dir: no target_directory in cargo metadata for $dir" >&2
        return 1
    }
}
