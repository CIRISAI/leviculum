#!/bin/bash
# sweep-tcp-hub.sh — sweep the TCP hub's delivery against connection count and
# packet rate (Codeberg #208).
#
# #208 recorded one 99.5454 % run of the hub load test at 128 connections /
# 15 ms — on a four-core host that was also running the #198 measurement
# harness. It could not say whether the hub drops packets at that load or
# whether the machine was simply full, because nobody had ever asked: the gate
# runs at 24 connections / 50 ms and is green there. This script asks.
#
# It runs the existing soak test once per cell of a (connections x rate) grid,
# several times per cell, with LEVICULUM_DELIVERY_LOG set so each run appends a
# DELIVERY line carrying its cell coordinates AND the CPU occupancy of the hub,
# of this harness, and of the machine. Then it reads the matrix back out of that
# log. A red cell does not stop the sweep — the distribution is the point.
#
# Reading the result (the discriminator #208 asks for):
#   * a cell below 100 % while nothing is saturated  -> a defect in the hub
#   * a cell below 100 % only at/over saturation     -> a load ceiling, and the
#     finding is that the gate should name where the cliff is
#
# Quiet host: the generator, the sink daemon and the hub all run here, so
# anything else on the machine lands in host_busy_pct and muddles exactly the
# question being asked. The sweep refuses to start on a loaded host; override
# with SWEEP_ALLOW_BUSY=1 if you know why.
#
# Usage:
#   bash scripts/sweep-tcp-hub.sh [--out DIR]
#   bash scripts/sweep-tcp-hub.sh --summarize DELIVERY_LOG   # re-read an old sweep
#
# Tuning:
#   SWEEP_CONNS="32 64 96 128"   steady connection counts to sweep
#   SWEEP_PKT_MS="50 25 15"      per-connection inter-packet intervals (ms)
#   SWEEP_REPEATS=3              runs per cell
#   SWEEP_SECS=20                steady-phase duration per run (seconds)
#   SWEEP_DRAIN_SECS=20          post-load drain window per run
#   SWEEP_SAMPLE_MS=250          RSS/fd/CPU sampler cadence
#   SWEEP_MAX_LOAD1=1.5          refuse to start above this 1-minute load average
#   SWEEP_SATURATED_PCT=90       host_busy_pct at/above which a cell counts as
#                                saturated when the summary is read
#   SWEEP_ALLOW_BUSY=1           skip the quiet-host check
#   SWEEP_TEST_NAME=...          test to run per cell; exists so the "the run
#                                produced no delivery record" guard below can be
#                                driven to fire on purpose
#
# Honours the ambient CARGO_TARGET_DIR so the binary lands where the test's
# locate_lnsd() looks; do NOT hardcode it here.
set -uo pipefail

# A decimal point is a decimal point. Under a comma locale awk parses the
# "99.5454" of a DELIVERY line as 99 and prints "99,0000" back, which silently
# turns the measurement this sweep exists for into a wrong number.
export LC_ALL=C

cd "$(dirname "$0")/.." || exit 1

OUT=""
SUMMARIZE=""
while [ $# -gt 0 ]; do
    case "$1" in
        --summarize)
            SUMMARIZE="${2:-}"
            [ -n "$SUMMARIZE" ] || { echo "--summarize needs a delivery log" >&2; exit 2; }
            shift 2
            ;;
        --out)
            OUT="${2:-}"
            [ -n "$OUT" ] || { echo "--out needs a directory" >&2; exit 2; }
            shift 2
            ;;
        -h|--help)
            sed -n '2,44p' "$0"
            exit 0
            ;;
        *)
            echo "usage: $0 [--out DIR] | $0 --summarize DELIVERY_LOG" >&2
            exit 2
            ;;
    esac
done

# --- The matrix, read back out of a delivery log. --------------------------
# Kept as a function so an old sweep's log can be re-read with --summarize, and
# so the reading itself can be exercised against a known log.
summarize() {
    echo "===== TCP hub delivery sweep ====="
    awk -v sat="$SATURATED_PCT" '
    /^DELIVERY / {
        delete f
        for (i = 2; i <= NF; i++) { split($i, kv, "="); f[kv[1]] = kv[2] }
        key = f["conns"] "\t" f["pkt_ms"]
        if (!(key in runs)) { keys[++nk] = key }
        runs[key]++
        # A cell that offered nothing is unmeasured, not lossy: counting it as a
        # shortfall would invent a defect out of a run that never generated load.
        if (f["pct"] == "n/a") {
            unmeasured[key]++
        } else {
            if (f["pct"] != "100.0000") { lossy[key]++ }
            if (!(key in worst) || f["pct"] + 0 < worst[key]) { worst[key] = f["pct"] + 0 }
        }
        if (f["hub_cpu_pct"] != "n/a" && (!(key in hub) || f["hub_cpu_pct"] + 0 > hub[key])) { hub[key] = f["hub_cpu_pct"] + 0 }
        if (f["host_busy_pct"] != "n/a" && (!(key in host) || f["host_busy_pct"] + 0 > host[key])) { host[key] = f["host_busy_pct"] + 0 }
    }
    END {
        printf "%-7s %-7s %-5s %-11s %-6s %-11s %-11s\n", \
            "conns", "pkt_ms", "runs", "worst_pct", "lossy", "hub_cpu_max", "host_busy_max"
        for (i = 1; i <= nk; i++) {
            k = keys[i]
            split(k, c, "\t")
            printf "%-7s %-7s %-5d %-11s %-6d %-11s %-11s\n", \
                c[1], c[2], runs[k], (k in worst ? sprintf("%.4f", worst[k]) : "n/a"), \
                (k in lossy ? lossy[k] : 0), \
                (k in hub ? sprintf("%.1f", hub[k]) : "n/a"), \
                (k in host ? sprintf("%.1f", host[k]) : "n/a")
            if (k in unmeasured) {
                printf "  -> %d run(s) offered no packets at all: not a delivery result, look at the run log\n", unmeasured[k]
            }
            if (k in lossy) {
                if (!(k in host)) {
                    printf "  -> lossy with no CPU measurement: the run was too short to sample; re-run this cell longer\n"
                } else if (host[k] + 0 >= sat) {
                    printf "  -> lossy at/over saturation (host_busy %.1f%% >= %s%%): a load ceiling, not proof of a hub defect\n", host[k], sat
                } else {
                    printf "  -> lossy WITHOUT saturation (host_busy %.1f%% < %s%%): a defect in the hub\n", host[k], sat
                }
            }
        }
    }
    ' "$1"
}

CONNS="${SWEEP_CONNS:-32 64 96 128}"
PKT_MS="${SWEEP_PKT_MS:-50 25 15}"
REPEATS="${SWEEP_REPEATS:-3}"
SECS="${SWEEP_SECS:-20}"
DRAIN_SECS="${SWEEP_DRAIN_SECS:-20}"
SAMPLE_MS="${SWEEP_SAMPLE_MS:-250}"
MAX_LOAD1="${SWEEP_MAX_LOAD1:-1.5}"
SATURATED_PCT="${SWEEP_SATURATED_PCT:-90}"

if [ -n "$SUMMARIZE" ]; then
    [ -r "$SUMMARIZE" ] || { echo "cannot read $SUMMARIZE" >&2; exit 2; }
    summarize "$SUMMARIZE"
    exit 0
fi

if [ -z "$OUT" ]; then
    OUT="$(mktemp -d "${TMPDIR:-/tmp}/tcp-hub-sweep-XXXXXX")"
fi
mkdir -p "$OUT" || exit 1
DELIVERY_LOG="$OUT/delivery.log"
: > "$DELIVERY_LOG"

# --- Quiet-host precondition, measured rather than assumed. ---------------
LOAD1="$(awk '{print $1}' /proc/loadavg)"
CORES="$(nproc)"
echo "[sweep] host: $CORES cores, load average $(cat /proc/loadavg)"
if [ "${SWEEP_ALLOW_BUSY:-0}" != "1" ]; then
    if awk -v l="$LOAD1" -v m="$MAX_LOAD1" 'BEGIN { exit !(l > m) }'; then
        cat >&2 <<EOF
[sweep] REFUSING: 1-minute load average $LOAD1 exceeds SWEEP_MAX_LOAD1=$MAX_LOAD1.
        The hub, the sink and the generator all run on this host, so another
        workload here is indistinguishable from the hub being slow — which is
        precisely the ambiguity #208 is about. Wait for the machine to go quiet,
        or set SWEEP_ALLOW_BUSY=1 and say so when you report the numbers.
EOF
        exit 3
    fi
fi

CELLS=0
for _c in $CONNS; do for _p in $PKT_MS; do CELLS=$((CELLS + 1)); done; done
RUNS=$((CELLS * REPEATS))
# Per run: warm-up + steady + drain + daemon startup. The constant is startup
# plus drain, both roughly fixed; it is an estimate for the operator, printed so
# nobody starts a 40-minute sweep believing it is a two-minute one.
PER_RUN=$((SECS + DRAIN_SECS + 10))
echo "[sweep] grid: conns=[$CONNS] x pkt_ms=[$PKT_MS] = $CELLS cells, $REPEATS run(s) each = $RUNS runs"
echo "[sweep] per run: ${SECS}s steady + ${DRAIN_SECS}s drain (+startup) -> about $((RUNS * PER_RUN / 60)) min total"
echo "[sweep] output:  $OUT"
echo "[sweep] delivery log: $DELIVERY_LOG"

# --- Build the binary the test spawns. ------------------------------------
echo "[sweep] building lnsd (release)..."
if ! cargo build -p leviculum-cli --release --bin lnsd > "$OUT/build.log" 2>&1; then
    echo "[sweep] build FAILED, see $OUT/build.log" >&2
    tail -20 "$OUT/build.log" >&2
    exit 1
fi

TEST="${SWEEP_TEST_NAME:-loadtest_tcp_hub_tests::loadtest_tcp_hub_soak}"
FAILED_CELLS=0
run_no=0
for conns in $CONNS; do
    for pkt_ms in $PKT_MS; do
        for rep in $(seq 1 "$REPEATS"); do
            run_no=$((run_no + 1))
            log="$OUT/c${conns}_p${pkt_ms}_r${rep}.log"
            before="$(wc -l < "$DELIVERY_LOG")"
            printf '[sweep] run %d/%d: conns=%s pkt_ms=%s rep=%s ... ' \
                "$run_no" "$RUNS" "$conns" "$pkt_ms" "$rep"
            LEVICULUM_DELIVERY_LOG="$DELIVERY_LOG" \
            LOADTEST_CONNS="$conns" \
            LOADTEST_PKT_MS="$pkt_ms" \
            LOADTEST_SECS="$SECS" \
            LOADTEST_DRAIN_SECS="$DRAIN_SECS" \
            LOADTEST_SAMPLE_MS="$SAMPLE_MS" \
                cargo test -p leviculum-std --test rnsd_interop -- \
                --ignored --exact "$TEST" --nocapture > "$log" 2>&1
            rc=$?
            after="$(wc -l < "$DELIVERY_LOG")"
            if [ "$after" -ne $((before + 1)) ]; then
                # The run produced no delivery record: the test did not execute
                # (a renamed test still exits 0 under --exact), or it died before
                # reporting. Either way the sweep would silently grow a hole, so
                # it stops here instead.
                echo "NO RECORD (rc=$rc)"
                echo "[sweep] ABORTING: the run appended $((after - before)) DELIVERY line(s), expected 1." >&2
                echo "[sweep] The test may not have run at all (check the name '$TEST') — see $log" >&2
                tail -30 "$log" >&2
                exit 1
            fi
            pct="$(tail -1 "$DELIVERY_LOG" | sed -n 's/.* pct=\([^ ]*\).*/\1/p')"
            if [ "$rc" -eq 0 ]; then
                echo "green, pct=$pct"
            else
                echo "RED (rc=$rc), pct=$pct  [$log]"
                FAILED_CELLS=$((FAILED_CELLS + 1))
            fi
        done
    done
done

# --- The matrix. ----------------------------------------------------------
echo
summarize "$DELIVERY_LOG"

echo
echo "[sweep] full logs: $OUT"
echo "[sweep] distribution: grep DELIVERY $DELIVERY_LOG"
if [ "$FAILED_CELLS" -gt 0 ]; then
    echo "[sweep] $FAILED_CELLS of $RUNS runs were RED. A lab run below 100% delivery is a bug (CLAUDE.md);"
    echo "        read the matrix above for whether the machine was saturated where it happened."
    exit 1
fi
echo "[sweep] all $RUNS runs green across $CELLS cells."
