#!/usr/bin/env bash
#
# The esptool the RNode recipes talk to an ESP32 through.
#
# Split out of scripts/install-ci.sh so that `just flash-rnode-setup` and the
# CI installer place the SAME binary at the SAME path. Before 2026-09-17 they
# did not: the installer said nothing about esptool at all and the recipe
# built a repo-local venv pinned `esptool<5`, so which esptool a host had
# depended on which of the two had last been run there, and on a host where
# neither had, `just flash-rnode` fell through to /usr/bin/esptool.
#
# WHY NOT THE DISTRIBUTION PACKAGE
#
#   Debian's esptool (4.7.0 in trixie) is dfsg-stripped of the flasher stubs,
#   which are prebuilt Xtensa/RISC-V binaries. What survives in
#   /usr/lib/python3/dist-packages/esptool/targets/stub_flasher/ is the
#   RISC-V set (32c2, 32c3, 32c6, 32h2, 32p4, 8266); every Xtensa stub —
#   esp32, esp32s2, esp32s3 — is absent. Reading an ESP32-S3 with it ends in
#
#     FileNotFoundError: .../stub_flasher/stub_flasher_32s3.json
#
#   and the `--no-stub` fallback talks to the ROM loader instead, which is
#   both slow and unreliable on a large read: a 16 MB read of the Heltec V4
#   died at 12 % with `Failed to read flash block (result was 01090000: CRC
#   or checksum was invalid)` (2026-09-17).
#
#   The PyPI wheel carries the stubs. With it the same 16 MB read off the
#   same board completed in 102.8 s at 1306 kbit/s, no retries
#   (.rnode-fw/extract.log, 2026-09-17).
#
# WHY 5.x AND WHY PINNED
#
#   esptool 5 renamed every command and option to a hyphenated form
#   (`read-flash`, `--flash-mode`, `--before default-reset`). It still
#   accepts the 4.x underscore spellings with a deprecation warning, but 4.x
#   does NOT accept the 5.x spellings, so the two cannot both be targets of
#   one composed command line. scripts/rnode-flash.sh composes the 5.x form
#   and refuses to run against an older binary rather than emit a stub
#   traceback at the worst possible moment.
#
#   Pinned for the reason the other tool pins in install-ci.sh give: this
#   writes to boards, and an unattributable change in what gets written is
#   worse than a scheduled bump.
#
# Idempotent: an already-installed matching version is left untouched
# (measured: 0.1 s, no network).
#
# Usage:
#   scripts/install-esptool.sh            # install/verify
#   scripts/install-esptool.sh --path     # print the binary path and exit
#
# The venv location can be overridden with LEVICULUM_RNODE_TOOLS (the
# Justfile's `esptool :=` reads the same variable), for a host that keeps
# tooling elsewhere.

set -euo pipefail

ESPTOOL_VERSION=5.4.0
VENV="${LEVICULUM_RNODE_TOOLS:-$HOME/.rnode-tools/venv}"
ESPTOOL_BIN="$VENV/bin/esptool"

if [[ "${1:-}" == "--path" ]]; then
    echo "$ESPTOOL_BIN"
    exit 0
fi
if [[ $# -gt 0 ]]; then
    echo "ERROR: unknown argument '$1'" >&2
    echo "Usage: $0 [--path]" >&2
    exit 1
fi

installed_version() {
    # `esptool version` prints its banner and then the bare version.
    [[ -x "$ESPTOOL_BIN" ]] || return 0
    "$ESPTOOL_BIN" version 2>/dev/null | tail -1 || true
}

# The stubs are the whole point of not using the distribution package, so
# their presence is a post-condition of this installer, not an assumption.
assert_xtensa_stubs() {
    local found
    found=$(find "$VENV"/lib/python*/site-packages/esptool/targets/stub_flasher \
                 \( -name 'esp32s3.json' -o -name 'stub_flasher_32s3.json' \) \
                 2>/dev/null | head -1)
    if [[ -z "$found" ]]; then
        echo "[install-esptool] ERROR: no ESP32-S3 flasher stub under $VENV" >&2
        echo "[install-esptool] This install cannot read or write an S3." >&2
        exit 1
    fi
    echo "[install-esptool] ESP32-S3 stub present: $found"
}

if [[ "$(installed_version)" == "$ESPTOOL_VERSION" ]]; then
    echo "[install-esptool] esptool $ESPTOOL_VERSION already installed at $ESPTOOL_BIN"
    assert_xtensa_stubs
    exit 0
fi

echo "[install-esptool] installing esptool $ESPTOOL_VERSION into $VENV"
python3 -m venv "$VENV"
"$VENV/bin/pip" install --quiet "esptool==$ESPTOOL_VERSION"

got=$(installed_version)
if [[ "$got" != "$ESPTOOL_VERSION" ]]; then
    echo "[install-esptool] ERROR: wanted $ESPTOOL_VERSION, got '${got:-nothing}'" >&2
    exit 1
fi
assert_xtensa_stubs
echo "[install-esptool] esptool $ESPTOOL_VERSION at $ESPTOOL_BIN"
