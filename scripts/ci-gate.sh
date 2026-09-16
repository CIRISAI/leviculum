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

apt-get update && apt-get install -y musl-tools
rustup component add rustfmt clippy
cargo install --locked just

just ci-gate
