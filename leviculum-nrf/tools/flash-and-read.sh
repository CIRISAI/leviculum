#!/usr/bin/env bash
# Flash firmware and capture debug output until a pattern matches or times out.
#
# Usage: tools/flash-and-read.sh <elf-path> --expect "pattern" --timeout 15
# Exit code 0 if pattern found, 1 if timeout.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
NRF_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
TARGET_DIR="$NRF_DIR/target"

# --- Parse arguments -------------------------------------------------------

ELF=""
EXPECT=""
TIMEOUT=15

while [ $# -gt 0 ]; do
    case "$1" in
        --expect)
            EXPECT="${2:?--expect requires a pattern}"
            shift 2
            ;;
        --timeout)
            TIMEOUT="${2:?--timeout requires seconds}"
            shift 2
            ;;
        *)
            ELF="$1"
            shift
            ;;
    esac
done

if [ -z "$ELF" ]; then
    echo "Usage: flash-and-read.sh <elf-path> --expect \"pattern\" [--timeout N]" >&2
    exit 1
fi

if [ -z "$EXPECT" ]; then
    echo "Error: --expect pattern is required" >&2
    exit 1
fi

# --- Flash firmware ---------------------------------------------------------

echo "==> Flashing firmware: $ELF"
"$SCRIPT_DIR/uf2-runner.sh" "$ELF"

# --- Read debug port --------------------------------------------------------

if [ ! -f "$TARGET_DIR/debug-port" ]; then
    echo "Error: $TARGET_DIR/debug-port not found. Did flashing fail?" >&2
    exit 1
fi

DEBUG_PORT="$(cat "$TARGET_DIR/debug-port")"
if [ ! -c "$DEBUG_PORT" ]; then
    echo "Error: Debug port $DEBUG_PORT does not exist" >&2
    exit 1
fi

echo "==> Reading debug output from $DEBUG_PORT (timeout: ${TIMEOUT}s, expect: \"$EXPECT\")"

# Check serial port access
if ! test -r "$DEBUG_PORT" 2>/dev/null; then
    echo "Error: No read permission for $DEBUG_PORT" >&2
    echo "  Run: sudo usermod -aG dialout \$USER" >&2
    echo "  Then log out and back in (or use: newgrp dialout)" >&2
    exit 1
fi

# Read from the port in a background process, grep for the expected pattern.
# No stty needed — CDC-ACM virtual serial works with default kernel settings.
OUTPUT_FILE="$(mktemp)"
trap 'rm -f "$OUTPUT_FILE"; kill "$CAT_PID" 2>/dev/null || true' EXIT

# Background: cat from serial port into output file
cat "$DEBUG_PORT" >> "$OUTPUT_FILE" &
CAT_PID=$!

FOUND=0
DEADLINE=$((SECONDS + TIMEOUT))

while [ "$SECONDS" -lt "$DEADLINE" ]; do
    sleep 1
    if grep -qF "$EXPECT" "$OUTPUT_FILE" 2>/dev/null; then
        FOUND=1
        break
    fi
done

kill "$CAT_PID" 2>/dev/null || true
wait "$CAT_PID" 2>/dev/null || true

# Show captured output
if [ -s "$OUTPUT_FILE" ]; then
    echo "--- Device output ---"
    cat "$OUTPUT_FILE"
    echo "--- End output ---"
fi

echo ""

if [ "$FOUND" -eq 1 ]; then
    echo "==> Pattern matched: \"$EXPECT\""
    exit 0
else
    echo "==> Timeout after ${TIMEOUT}s — pattern \"$EXPECT\" not found" >&2
    exit 1
fi
