#!/bin/bash
# Idempotent CI installer for the Leviculum 4-tier self-hosted pipeline.
set -e

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_DIR"

echo "[install-ci] Installing CI pipeline in $REPO_DIR"

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

# 2. Activate git hooks
git config core.hooksPath .githooks
echo "[install-ci] git core.hooksPath -> .githooks"

# 3. chmod hook + runner scripts
chmod +x .githooks/pre-push .githooks/post-commit
chmod +x scripts/run-tier1.sh scripts/run-tier2.sh scripts/run-tier3.sh
chmod +x scripts/check-tier2-staleness.sh scripts/ci-status.sh
chmod +x scripts/install-ci.sh
echo "[install-ci] hook + script files made executable"

# 4. State directory
mkdir -p ~/.local/state/leviculum-ci
echo "[install-ci] state dir: ~/.local/state/leviculum-ci"

# 5. Separate cargo target dir
mkdir -p ~/.cache/leviculum-ci-target
echo "[install-ci] cargo target dir: ~/.cache/leviculum-ci-target"

# 6. Install systemd user units, patching the hardcoded
#    %h/coding/libreticulum literal to point at the worktree this
#    installer was actually run from.  Lets a `git worktree`-based
#    second checkout (e.g. ~/coding/libreticulum-ci) install its
#    own units that fire against itself, instead of silently
#    targeting the developer's primary checkout.
SYSTEMD_USER_DIR=~/.config/systemd/user
mkdir -p "$SYSTEMD_USER_DIR"
for unit in scripts/systemd/leviculum-ci-tier2.service \
            scripts/systemd/leviculum-ci-tier2.timer \
            scripts/systemd/leviculum-ci-nightly.service \
            scripts/systemd/leviculum-ci-nightly.timer; do
    sed "s|%h/coding/libreticulum|$REPO_DIR|g" "$unit" \
      > "$SYSTEMD_USER_DIR/$(basename "$unit")"
done
echo "[install-ci] systemd user units installed in $SYSTEMD_USER_DIR (path: $REPO_DIR)"

# 7. Reload systemd
systemctl --user daemon-reload

# 8. Enable timers
systemctl --user enable --now leviculum-ci-tier2.timer leviculum-ci-nightly.timer
echo "[install-ci] timers enabled"

# 9. LoRa hardware probe (warning only)
if ! ls /dev/ttyACM* >/dev/null 2>&1; then
    echo "[install-ci] WARNING: no /dev/ttyACM* devices found — LoRa tests will skip in nightly."
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
