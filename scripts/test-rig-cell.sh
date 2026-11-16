#!/usr/bin/env bash
# Fixture test for scripts/rig-cell.sh's reader restart.
#
# After a cell every bound board has been reset, and its by-id link is often
# missing for a few seconds. The runner it replaces skipped such a board
# silently and left it unrecorded until somebody noticed. Two claims, driven
# against a fake by-id directory and a fake rig.toml:
#
#   1. a link that appears while readers_back waits gets its reader, and the
#      log says after how long;
#   2. a link that never appears is named as NOT back, and no reader starts;
#   3. a started reader inherits no open fd of its caller above 2: a caller
#      holding a flock on fd 9 (erase.sh did, 2026-11-08, and the reader kept
#      the rig lock for 42 minutes) has the lock free the moment it lets go,
#      while the reader is still running.
#
# The reader is a stub that records its argv: what is under test is the
# runner's decision, and the real reader has its own test
# (scripts/test-debug-witness.sh, --attach-now included).
#
# Positive control, built in: claim 1 is run a second time against a copy of
# rig-cell.sh whose wait is cut to zero, and has to FAIL there. A test that
# passed both ways would not be testing the wait. Claim 3 likewise runs
# against a copy with the fd closer taken out, and has to fail there.
#
# Usage: bash scripts/test-rig-cell.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RIG_CELL="$SCRIPT_DIR/rig-cell.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

PASS=0
FAIL=0
ok() {
    PASS=$((PASS + 1))
    printf 'ok    %s\n' "$1"
}
bad() {
    FAIL=$((FAIL + 1))
    printf 'FAIL  %s\n' "$1"
}

# Two LNodes and an RNode; the RNode must never get a reader.
cat >"$WORK/rig.toml" <<'EOF'
[devices.board-late]
firmware = "lnode"
serial = "AAAA000000000001"

[devices.board-gone]
firmware = "lnode"
serial = "BBBB000000000002"

[devices.some-rnode]
firmware = "rnode"
serial = "5896000022"
EOF

cat >"$WORK/stub-reader.py" <<'PY'
import os, sys
with open(os.environ["STUB_CALLS"], "a") as fh:
    fh.write(" ".join(sys.argv[1:]) + "\n")
PY

# One readers_back pass against a fresh bench. Prints the runner log.
# Args: $1 = rig-cell.sh to source, $2 = case dir, $3 = wait (s)
run_readers_back() {
    local script="$1" dir="$2" wait="$3"
    mkdir -p "$dir/by-id" "$dir/rig-run"
    (
        # shellcheck disable=SC2030,SC2031  # scoped to this subshell on purpose
        export LEV_RIG_RUN="$dir/rig-run" LEV_RIG_TOML="$WORK/rig.toml" \
            LEV_RIG_BY_ID="$dir/by-id" LEV_RIG_RUNLOG="$dir/runner.log" \
            LEV_RIG_READER="$WORK/stub-reader.py" LEV_RIG_READER_WAIT="$wait" \
            STUB_CALLS="$dir/calls"
        # shellcheck source=scripts/rig-cell.sh
        . "$script"
        readers_back >/dev/null
    )
}

# Claim 1 against <script>: board-late's link appears two seconds into the
# wait. Returns 0 iff the reader started on it and the log says so.
# Args: $1 = script, $2 = case dir
late_link_case() {
    local script="$1" dir="$2" link
    link="$dir/by-id/usb-leviculum_leviculum_T114_AAAA000000000001-if00"
    mkdir -p "$dir/by-id"
    (sleep 2 && ln -s /dev/null "$link") &
    run_readers_back "$script" "$dir" 4
    wait
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        [ -s "$dir/calls" ] && break
        sleep 0.1
    done
    grep -qE 'reader board-late back after [2-4] s' "$dir/runner.log" || return 1
    grep -qF -- "--port $link" "$dir/calls" 2>/dev/null || return 1
    grep -qF -- "--attach-now --capture 31536000" "$dir/calls" || return 1
    return 0
}

if late_link_case "$RIG_CELL" "$WORK/late"; then
    ok "a link that comes back during the wait gets its reader"
else
    bad "a link that comes back during the wait gets its reader"
    cat "$WORK/late/runner.log" "$WORK/late/calls" 2>/dev/null | sed 's/^/        /'
fi

if grep -qF 'reader board-gone NOT back: link missing after 4 s' "$WORK/late/runner.log"; then
    ok "a link that never comes back is named NOT back"
else
    bad "a link that never comes back is named NOT back"
    sed 's/^/        /' "$WORK/late/runner.log"
fi

if grep -qF 'BBBB000000000002' "$WORK/late/calls" 2>/dev/null; then
    bad "...and gets no reader"
else
    ok "...and gets no reader"
fi

if grep -qE 'some-rnode|5896000022' "$WORK/late/runner.log" "$WORK/late/calls" 2>/dev/null; then
    bad "an RNode in rig.toml is never given a reader"
else
    ok "an RNode in rig.toml is never given a reader"
fi

# The control: the same case with the wait cut to zero must go red.
# shellcheck disable=SC2016  # the pattern is rig-cell.sh's literal text
sed 's/if \[ "\$waited" -ge "\$LEV_RIG_READER_WAIT" \]; then/if true; then/' \
    "$RIG_CELL" >"$WORK/rig-cell-nowait.sh"
if cmp -s "$RIG_CELL" "$WORK/rig-cell-nowait.sh"; then
    bad "the control could not cut the wait: rig-cell.sh's wait line moved"
elif late_link_case "$WORK/rig-cell-nowait.sh" "$WORK/control"; then
    bad "control: with the wait cut to zero the late-link case must fail"
else
    ok "control: with the wait cut to zero the late-link case fails"
fi

# A reader stub that stays up, as the real one does.
cat >"$WORK/sleep-reader.py" <<'PY'
import time
time.sleep(60)
PY

# Claim 3 against <script>: a caller holds a flock on fd 9 (and a plain fd on
# 12, past the single digits an explicit close list would name), starts a
# reader through rig_reader_start and exits. Returns 0 iff the reader is alive
# as the pidfile's pid and holds neither file.
# Args: $1 = script, $2 = case dir
inherited_fd_case() {
    local script="$1" dir="$2" pid holders rc=0
    mkdir -p "$dir/rig-run/readers"
    : >"$dir/rig.lock"
    : >"$dir/other"
    (
        # Both cases scope their bench to a subshell on purpose.
        # shellcheck disable=SC2030,SC2031
        export LEV_RIG_RUN="$dir/rig-run" LEV_RIG_TOML="$WORK/rig.toml" \
            LEV_RIG_RUNLOG="$dir/runner.log" LEV_RIG_READER="$WORK/sleep-reader.py"
        # shellcheck source=scripts/rig-cell.sh
        . "$script"
        exec 9>"$dir/rig.lock" 12>"$dir/other"
        flock -n 9 || exit 1
        rig_reader_start board-x XXXX000000000009 /dev/null
    ) || return 1
    pid="$(cat "$dir/rig-run/readers/board-x.pid" 2>/dev/null)"
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        grep -qF sleep-reader.py "/proc/$pid/cmdline" 2>/dev/null && break
        sleep 0.1
    done
    if ! grep -qF sleep-reader.py "/proc/$pid/cmdline" 2>/dev/null; then
        echo "        reader not running as the pidfile's pid ($pid)"
        rc=1
    fi
    holders="$(fuser "$dir/rig.lock" "$dir/other" 2>/dev/null)"
    if [ -n "${holders// /}" ]; then
        echo "        still held by pid(s):$holders"
        rc=1
    fi
    if ! flock -n "$dir/rig.lock" true; then
        echo "        the lock is still taken"
        rc=1
    fi
    [ -n "$pid" ] && kill "$pid" 2>/dev/null
    return "$rc"
}

if inherited_fd_case "$RIG_CELL" "$WORK/fds"; then
    ok "a reader inherits no fd of its caller, a held lock included"
else
    bad "a reader inherits no fd of its caller, a held lock included"
fi

# The control: without the closer the reader keeps the caller's fd 9 lock.
grep -vxF '        rig_close_inherited_fds' "$RIG_CELL" >"$WORK/rig-cell-noclose.sh"
if cmp -s "$RIG_CELL" "$WORK/rig-cell-noclose.sh"; then
    bad "the control could not take the fd closer out: its call moved"
elif inherited_fd_case "$WORK/rig-cell-noclose.sh" "$WORK/fds-control" >/dev/null; then
    bad "control: without the fd closer the inherited-fd case must fail"
else
    ok "control: without the fd closer the inherited-fd case fails"
fi

printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
