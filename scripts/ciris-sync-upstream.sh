#!/usr/bin/env bash
# Mechanically sync this CIRIS fork onto the latest upstream (Lew_Palm/leviculum).
#
# Model: our fork = upstream/master + a *rebasable* CIRIS patch series, carried
# on `main`. The series is two layers:
#   1. Fixes we are upstreaming (PRs against Lew_Palm/leviculum). When upstream
#      merges one, `git rebase` drops it automatically — our divergence shrinks.
#   2. Permanent CIRIS-only infra (submodule strip, GitHub Actions CI, interop
#      pip-resolution) that never goes upstream and always rebases forward.
#
# So catching up to future upstream releases is just: run this script, eyeball
# the validation gate, push. No archaeology.
#
# Usage:  scripts/ciris-sync-upstream.sh [series-branch]   (default: main)
set -euo pipefail
SERIES_BRANCH="${1:-main}"
UPSTREAM_REMOTE="${UPSTREAM_REMOTE:-upstream}"
UPSTREAM_URL="${UPSTREAM_URL:-https://codeberg.org/Lew_Palm/leviculum.git}"
UPSTREAM_BRANCH="${UPSTREAM_BRANCH:-master}"

git remote get-url "$UPSTREAM_REMOTE" >/dev/null 2>&1 || git remote add "$UPSTREAM_REMOTE" "$UPSTREAM_URL"
echo "==> fetching $UPSTREAM_REMOTE/$UPSTREAM_BRANCH"
git fetch --tags "$UPSTREAM_REMOTE"

OLD_BASE=$(git merge-base "$SERIES_BRANCH" "$UPSTREAM_REMOTE/$UPSTREAM_BRANCH")
NEW_BASE=$(git rev-parse "$UPSTREAM_REMOTE/$UPSTREAM_BRANCH")
if [ "$OLD_BASE" = "$NEW_BASE" ]; then echo "==> already on latest upstream ($NEW_BASE). nothing to do."; exit 0; fi
echo "==> rebasing CIRIS series: base ${OLD_BASE:0:9} -> upstream ${NEW_BASE:0:9}"
echo "    (commits already merged upstream are dropped automatically)"
git rebase --onto "$UPSTREAM_REMOTE/$UPSTREAM_BRANCH" "$OLD_BASE" "$SERIES_BRANCH"

echo "==> validation gate"
cargo build -p leviculum-core -p leviculum-std
cargo test -p leviculum-core --lib
cargo test -p leviculum-std --lib
rustup target add thumbv6m-none-eabi >/dev/null 2>&1 || true
cargo build -p leviculum-core --target thumbv6m-none-eabi --no-default-features
cargo fmt --all -- --check
cargo clippy -p leviculum-core -p leviculum-std -- -D warnings
echo "==> SYNC OK on '$SERIES_BRANCH'. Review the rebased series, then push."
echo "    (full interop suite additionally needs: git submodule update --init reference/Reticulum)"
