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
VM_MODE=0
for arg in "$@"; do
    case "$arg" in
        --vm-mode) VM_MODE=1 ;;
        *)
            echo "ERROR: unknown flag '$arg'" >&2
            echo "Usage: $0 [--vm-mode]" >&2
            exit 1
            ;;
    esac
done

echo "[install-ci] Installing CI pipeline in $REPO_DIR (vm-mode=$VM_MODE)"

# 1. Dependency check
MISSING=()
for cmd in just docker notify-send cargo python3 flock; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        MISSING+=("$cmd")
    fi
done
if [ ${#MISSING[@]} -gt 0 ]; then
    echo "[install-ci] Missing dependencies: ${MISSING[*]}"
    echo "[install-ci] Hint: sudo apt install ${MISSING[*]}"
    exit 1
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
    chmod +x .githooks/pre-push .githooks/post-commit
fi
chmod +x scripts/run-tier1.sh scripts/run-tier2.sh scripts/run-tier3.sh scripts/run-tier3-hw.sh
chmod +x scripts/check-tier2-staleness.sh scripts/ci-status.sh
chmod +x scripts/install-ci.sh
echo "[install-ci] runner scripts made executable"

# 4. State directory
mkdir -p ~/.local/state/leviculum-ci
echo "[install-ci] state dir: ~/.local/state/leviculum-ci"

# 5. Separate cargo target dir
mkdir -p ~/.cache/leviculum-ci-target
echo "[install-ci] cargo target dir: ~/.cache/leviculum-ci-target"

# 6. Firmware build toolchain.  flip-link is the firmware linker
#    (stack-overflow protection, Codeberg #50); run-tier3-hw.sh builds
#    the firmware via `just flash*`.  Both lines are idempotent: the
#    rustup target is a no-op once added, and `cargo install` skips a
#    crate that is already present at the requested version.
rustup target add thumbv7em-none-eabihf
cargo install --locked flip-link
echo "[install-ci] firmware toolchain: thumbv7em-none-eabihf + flip-link"

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
echo "  Override stale:  git push --no-verify"
echo "  Logs:            ~/.local/state/leviculum-ci/"
echo "  Timers:          systemctl --user list-timers"
echo ""
echo "  First Tier 1 run uses a fresh CARGO_TARGET_DIR and takes 20-40 min."
echo "  Subsequent runs are incremental, ~5-15 min."
