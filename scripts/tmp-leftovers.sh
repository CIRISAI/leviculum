#!/usr/bin/env bash
# `cargo test --workspace` leaves nothing behind in the temp directory (730).
#
# /tmp on the coder host is a tmpfs, so every scratch directory a test leaks
# is RAM, and the host runs the suite all day. On 2026-10-14 it held 40 201
# entries older than two days and stood at 100 %; the OOM killer took two
# passes beside a gate that week.
#
# Shape: run the suite in the ordinary way, but with TMPDIR pointed at a fresh
# directory of its own. `std::env::temp_dir()` and the `tempfile` crate both
# honour TMPDIR, and so do the daemons and Python peers a test spawns, so every
# scratch path the suite builds lands there and nothing else does. Whatever is
# still in it once `cargo test` has exited is a leak, attributed by name, and a
# concurrent gate on the same host cannot put anything in it. That is what
# makes the verdict deterministic where a before/after diff of /tmp itself is
# not: another lane's run creates and deletes entries there the whole time.
#
# The /tmp diff is still printed, as information: a test that hardcodes
# "/tmp" bypasses TMPDIR, and the diff is where such a path would show up.
# It is not part of the verdict for the reason above.
#
# The one path the suite keeps on purpose is the test port counter, and it lives
# outside TMPDIR: $XDG_RUNTIME_DIR, /run/user/<uid> or /tmp, keyed by uid
# (leviculum-std/tests/support/port_alloc.rs, state_dir; 778). No exclusion here.
#
# A test whose run-time prerequisite is absent on this host is left out by
# name, as every other runner that selects from the tree leaves it out
# (scripts/integ-prerequisites.txt, through scripts/with-prerequisites.sh),
# and printed with the path it lacks. Before 1353 this ran the plain suite:
# hamster has no codec2, and the pre-push hook's `just fast` failed on the
# voice round trip's `c2enc: No such file or directory` where its line in
# that file says a host without the tool leaves it out. This gate's subject
# is the temp directory, not the codecs; on a host with every prerequisite
# the command is the plain `cargo test --workspace`, unchanged.
#
# Exit 0 = the suite passed and left nothing in its TMPDIR.
# Exit 1 = it left entries (named), or the suite itself failed.
set -euo pipefail

cd "$(dirname "$0")/.."

# shellcheck source=scripts/integ-targets.sh
. scripts/integ-targets.sh

# Short on purpose: a test that puts a Unix socket under temp_dir() has 108
# bytes for the whole path.
scratch=$(mktemp -d /tmp/tl.XXXXXX)
log=$(mktemp /tmp/tl-log.XXXXXX)
trap 'rm -rf "$scratch" "$log"' EXIT

# /tmp is shared: an entry another process deletes between find's readdir
# and its stat for -user makes find exit 1, which under pipefail and set -e
# ended this script silently, its stderr discarded (1190: the guards run
# failed `tmp-leftovers-selftest` with an empty message; 3 of 20 runs of the
# selftest beside a /tmp churner). The listing is information only, so a
# vanished entry is not an error.
list_tmp() {
    { find /tmp -mindepth 1 -maxdepth 1 -user "$(id -u)" -printf '%f\n' 2>/dev/null || true; } | sort
}

# The compiler cache's server must not be born inside $scratch (987). sccache
# runs as RUSTC_WRAPPER on the coder host, and its client starts the server on
# first use when none is running, as a child that inherits the client's
# environment. The server makes a temp dir per compile under temp_dir(), so a
# server started by this suite would live in $scratch, and once the trap below
# removes it every compile on the host fails with "sccache: Failed to create
# temp dir ... /tmp/tl.XXXXXX/sccache..." (exit 254) until the server idles out.
# 985 hit exactly that with a hand-rolled `TMPDIR=$(mktemp -d) cargo test`.
# sccache 0.10 has no variable that places the server's temp dir (SCCACHE_DIR
# is the cache, not the scratch), so the server is started here, before TMPDIR
# changes, under the host's own environment. A server already running answers
# "Address in use" and is kept, which is the case this wants. If it idles out
# mid-suite (SCCACHE_IDLE_TIMEOUT, 600 s by default) a later compile could still
# restart it in $scratch; cargo compiles every target before running any, so
# that would take a test that itself spawns a build after ten idle minutes.
wrapper=${RUSTC_WRAPPER:-${CARGO_BUILD_RUSTC_WRAPPER:-}}
if [ "$(basename -- "${wrapper:-none}")" = sccache ]; then
    "$wrapper" --start-server >/dev/null 2>&1 || true
fi

before=$(list_tmp)

# The targets with a prerequisite absent here; a whole-target one is refused
# by the wrapper, naming it, since `--workspace` cannot leave a target out.
unmet_targets=$(integ_unmet_prerequisites | cut -f1 | sort -u)
suite=(cargo test --workspace)
if [ -n "$unmet_targets" ]; then
    mapfile -t unmet_list <<<"$unmet_targets"
    suite=(bash scripts/with-prerequisites.sh "${unmet_list[@]}" -- "${suite[@]}")
fi

status=0
TMPDIR="$scratch" "${suite[@]}" >"$log" 2>&1 || status=$?
# What the wrapper left out, and why; the rest of its output is the suite's.
skipped=$(grep -e '^\[with-prerequisites\]' -e '^with-prerequisites:' "$log" || true)
[ -z "$skipped" ] || echo "$skipped"

after=$(list_tmp)
outside=$(comm -13 <(echo "$before") <(echo "$after") | grep -v -x -e "${scratch#/tmp/}" -e "${log#/tmp/}" || true)
leftovers=$(find "$scratch" -mindepth 1 -maxdepth 1 -printf '%f\n' | sort)

if [ "$status" -ne 0 ]; then
    echo "tmp-leftovers: cargo test --workspace failed (exit $status); its output:" >&2
    cat "$log" >&2
    echo "tmp-leftovers: FAIL, the suite is red, so the leftover count below is not a verdict" >&2
fi

if [ -n "$outside" ]; then
    echo "tmp-leftovers: new in /tmp during the run (information only, any user process may have made these):"
    while IFS= read -r entry; do
        printf '  %s\n' "$entry"
    done <<<"$outside"
fi

if [ -n "$leftovers" ]; then
    count=$(echo "$leftovers" | wc -l)
    echo "tmp-leftovers: FAIL, the suite left $count entries in its TMPDIR:" >&2
    # Digits collapsed to N, so one leaking test is one line with a count.
    echo "$leftovers" | sed -E 's/[0-9]+/N/g' | sort | uniq -c | sort -rn >&2
    echo "tmp-leftovers: by name, with what each holds:" >&2
    while IFS= read -r entry; do
        if [ -d "$scratch/$entry" ]; then
            echo "  $entry/: $(find "$scratch/$entry" -mindepth 1 -maxdepth 2 -printf '%P ' | head -c 400)"
        else
            echo "  $entry"
        fi
    done <<<"$leftovers" >&2
    exit 1
fi

[ "$status" -eq 0 ] || exit 1
passed=$(awk '/^test result: ok\./ { binaries++; passed += $4 } END { printf "%d tests passed in %d test binaries", passed, binaries }' "$log")
echo "tmp-leftovers: ok, cargo test --workspace ($passed) left nothing in its TMPDIR"
