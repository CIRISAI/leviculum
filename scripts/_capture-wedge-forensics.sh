#!/usr/bin/env bash
# Codeberg #50 Bug-A forensic capture.  Invoked by Bug-B's timeout
# helper (reticulum-integ/src/timeout.rs) when a lora_* test wedges,
# and runnable standalone if an operator notices a live wedge.
#
# Usage:
#   bash scripts/_capture-wedge-forensics.sh <test-name-tag>
#
# Captures into /tmp/leviculum/wedge-forensics/<timestamp>/:
#   - t114-firmware.txt   T114 USB-serial-id + udev attributes
#   - dmesg-tty-usb.txt   last 200 lines filtered for tty/usb/cdc
#   - lsusb.txt           lsusb -t and lsusb -v for LoRa devices
#   - wchan.txt           per-thread wchan + status for reticulum-integ
#                         processes
#   - event-log-*.log     copy of any LEVICULUM_EVENT_LOG file referenced
#                         by env (best-effort)
#
# Best-effort: any subcommand failure is captured but does not abort
# the script.  Forensic capture must never shadow the original
# timeout-panic.

set +e

TAG="${1:-untagged}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
BASEDIR="/tmp/leviculum/wedge-forensics/${STAMP}-${TAG}"
mkdir -p "$BASEDIR"

LOG="${BASEDIR}/capture.log"
exec 3>"$LOG"
echo "=== _capture-wedge-forensics.sh ===" >&3
echo "tag:    $TAG" >&3
echo "stamp:  $STAMP" >&3
echo "basedir:$BASEDIR" >&3
echo "started:$(date -Iseconds)" >&3

# 1. T114 firmware identifier (USB serial + udev attrs)
{
    echo "=== T114 USB serial identifiers ==="
    for path in /dev/serial/by-id/usb-leviculum_leviculum_T114_*; do
        [ -e "$path" ] || continue
        echo "--- $path ---"
        readlink -f "$path"
        udevadm info "$(readlink -f "$path")" 2>&1 | head -40
    done
} > "${BASEDIR}/t114-firmware.txt" 2>&1
echo "captured: t114-firmware.txt" >&3

# 2. dmesg tail filtered for tty/usb/cdc
{
    dmesg --time-format iso 2>&1 | grep -iE 'tty|usb|cdc|ttyACM' | tail -200
} > "${BASEDIR}/dmesg-tty-usb.txt" 2>&1
echo "captured: dmesg-tty-usb.txt" >&3

# 3. USB state
{
    echo "=== lsusb -t ==="
    lsusb -t 2>&1
    echo
    echo "=== lsusb -v (leviculum + 1a86 only) ==="
    lsusb -d 1209: -v 2>&1
    lsusb -d 1a86: -v 2>&1
} > "${BASEDIR}/lsusb.txt" 2>&1
echo "captured: lsusb.txt" >&3

# 4. wchan / status snapshot for reticulum-integ processes
{
    echo "=== reticulum_integ processes ==="
    pgrep -af 'reticulum_integ|reticulum-integ' 2>&1
    echo
    for pid in $(pgrep -f 'reticulum_integ|reticulum-integ' 2>/dev/null); do
        echo "=== pid $pid ==="
        echo "--- /proc/$pid/status (head 20) ---"
        head -20 "/proc/$pid/status" 2>&1
        echo "--- /proc/$pid/task/*/wchan ---"
        for tid in /proc/"$pid"/task/*; do
            [ -e "$tid/wchan" ] || continue
            tname="$(cat "$tid/comm" 2>/dev/null || echo '?')"
            wch="$(cat "$tid/wchan" 2>/dev/null || echo '?')"
            echo "tid=$(basename "$tid") comm=$tname wchan=$wch"
        done
        echo
    done
} > "${BASEDIR}/wchan.txt" 2>&1
echo "captured: wchan.txt" >&3

# 5. Event log file from LEVICULUM_EVENT_LOG (best-effort copy)
if [ -n "${LEVICULUM_EVENT_LOG:-}" ] && [ -f "$LEVICULUM_EVENT_LOG" ]; then
    cp "$LEVICULUM_EVENT_LOG" "${BASEDIR}/event-log-from-env.log" 2>&3
    echo "captured: event-log-from-env.log (from env)" >&3
fi
# Also pick up any /tmp/leviculum-event-log* file modified in last hour
find /tmp -maxdepth 2 -name 'leviculum-event*.log' -mmin -60 2>/dev/null \
    | while read -r p; do
        cp "$p" "${BASEDIR}/$(basename "$p")" 2>&3 && \
            echo "captured: $(basename "$p") (from /tmp)" >&3
done

echo "finished:$(date -Iseconds)" >&3

# Exit zero regardless — forensic failure must never shadow the original
# timeout-panic that invoked this.
exit 0
