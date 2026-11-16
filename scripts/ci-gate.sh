#!/bin/bash
# Provision a stock `rust:bookworm` container and run `just ci-gate` in it.
#
# Two pipelines need that gate — `.woodpecker/ci.yml` on every push
# (Codeberg #299) and `.woodpecker/nightly.yml` before it publishes anything
# (#266) — and the provisioning is not a detail either of them may own
# privately: a package that one file grows and the other does not is a gate
# that passes in one pipeline and dies in cc-rs in the other, found a night
# later. Same reason `scripts/build-deb.sh` exists instead of a second copy
# of the .deb steps in the YAML; that duplication cost eight days once.
#
# Every line below is forced by something, not chosen:
#   musl-tools       — .cargo/config.toml builds everything for
#                      x86_64-unknown-linux-musl, and ring's build script
#                      (rustls, via the lblogd/lnomad dependency graph) needs
#                      a musl C compiler. Without it the gate dies in cc-rs
#                      before it lints anything.
#   socat            — leviculum-std's mvr `media_silence_restore_signal`
#                      links a pty pair with it, the serial-cable stand-in
#                      the interop suite uses (Codeberg #102). Absent from
#                      the image, and the three tests in that file failed on
#                      the spawn (pipelines 476-488).
#   iproute2         — `ss`. leviculum-cli's client_tools,
#                      lnsd_instance_conflict and storage_path_from_config
#                      wait for a daemon's abstract socket by reading
#                      `ss -xl`, and rust:bookworm has no iproute2, so the
#                      wait saw an empty table and reported "lnsd never
#                      bound" for a daemon that had (pipelines 476-488).
#   openjdk-17-jdk-headless
#                    — javac and java. loquax-android's fling test compiles
#                      the phone's Fling.java and its host test and runs
#                      them (1209); without a JDK it is skipped by name
#                      (scripts/integ-prerequisites.txt), and the forge
#                      would never check the Java coast against the Rust
#                      one. Measured in rust:bookworm on 2026-11-01: 118 MB
#                      download, 281 MB installed, 12 s, without recommends
#                      (1256).
#   rustfmt, clippy  — rust:bookworm ships neither component.
#   just             — not packaged in Debian bookworm, and the gate is a
#                      Justfile recipe on purpose (see the recipe's comment).
#                      ~45 s.
#
# No submodule is fetched. One used to be — reference/Reticulum, --depth 1,
# ~5 s and 27 MB — because three compile-time `include_str!`s reached into
# the vendored trees for fixtures and the crates holding them did not build
# without them. Those fixtures now live in the crates' own `tests_data/`
# (Codeberg #300), so the gate builds what a plain `git clone` builds, and
# github.com is out of its dependency set. `just check-plain-clone` is what
# keeps it that way.
#
# Measured cold on 2026-08-18 (fresh image, empty target dir and registry,
# 4 cores): 1m11s provisioning + 2m12s gate = 3m23s, of which the submodule
# fetch removed above was ~5 s.
#
# Usage (inside a CI container):
#   bash scripts/ci-gate.sh
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_DIR"

# This installs system packages and toolchain components, which is a fine
# thing to do to a throwaway container and not to a developer's machine. On a
# workstation the gate itself is one word: `just ci-gate`.
#
# Woodpecker sets `CI=woodpecker` and a `CI_REPO` for every step; either one
# being present is enough, so a runner that drops one of them does not turn
# the forge gate into a refusal. A false refusal is loud and harmless (a red
# pipeline naming this line); a false accept apt-installs into somebody's
# home directory, so the asymmetry is deliberate.
if [ -z "${CI:-}${CI_REPO:-}" ] && [ "${LEV_CI_GATE_FORCE:-0}" != "1" ]; then
    echo "ci-gate.sh: refusing to run outside CI — it apt-installs into the" >&2
    echo "  host it runs on. Run 'just ci-gate' instead, or set" >&2
    echo "  LEV_CI_GATE_FORCE=1 if this really is a throwaway container." >&2
    exit 1
fi

apt-get update && apt-get install -y musl-tools socat iproute2
apt-get install -y --no-install-recommends openjdk-17-jdk-headless
rustup component add rustfmt clippy
cargo install --locked just

# Where the gate runs, printed before it runs. A test that reads the kernel's
# own books (proc_memory's file-page census) went red on the forge and green
# in the same image here (order 1372, pipelines 602/603): the runner's
# kernel, core count and the filesystem under the workspace and the target
# dir are what differ, and tmpfs books file pages as RssShmem, not RssFile.
# The target dir may not exist yet, so its nearest existing parent is named.
fs_of() {
    local p="$1"
    while [ ! -e "$p" ]; do p="$(dirname "$p")"; done
    printf '%s (%s)' "$(stat -f -c %T "$p" 2>/dev/null || echo unknown)" "$p"
}
echo "=== ci-gate environment ==="
uname -a
echo "nproc: $(nproc)"
echo "workspace fs: $(fs_of "$REPO_DIR")"
echo "target fs: $(fs_of "${CARGO_TARGET_DIR:-$REPO_DIR/target}")"
grep -E '^(Rss|VmRSS|VmSwap)' /proc/self/status || echo "no Rss/VmRSS/VmSwap lines in /proc/self/status"
echo "=== end ci-gate environment ==="

just ci-gate
