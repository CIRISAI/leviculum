#!/bin/bash
# lock-contention.sh — read periculum's lock-contention marker.
#
# Sourced by the three tier runners; starts nothing on its own.
#
# WHICH CHANNEL IS THE AUTHORITY (Codeberg #309). periculum tells a refused
# contender apart from a failed run twice over: an exit code
# (`EXIT_CONTENTION` 2 for a legitimate overlap, `EXIT_CONTENTION_SUSPECT` 4
# for a holder that does not look legitimate) and the marker file, which
# carries the same distinction in `verdict=`/`suspect=` plus the holder's
# identity. The marker is the authority here, and the exit code is a
# corroborating signal, for two measured reasons:
#
#   * Two of the three runners cannot see the exit code as periculum meant
#     it. They invoke `just extensive` / `just nightly`, and the `nightly`
#     recipe rewrites its exit status to 1 whenever an LNode's firmware could
#     not be verified, whatever periculum exited with. A contention inside a
#     harness-driven `cargo test` arrives as 101 for the same reason. The
#     marker survives both: it is a file, written before the process that
#     wrote it had an exit status at all.
#   * Only the marker carries WHO. A ledger line and a 03:37 notification are
#     read by a human who has to decide whether to go looking for a wedged
#     process, and `holder_pid`/`holder_age_secs`/`detail` are what makes
#     that decision possible. An exit code can only ever say which of two.
#
# So the runners branch on the marker and never on the code — except
# run-tier3-hw.sh, which calls periculum directly and must therefore route
# BOTH contention codes to this parse rather than only the historic 2.
#
# Staleness is periculum's problem and is already solved there: a marker that
# predates the current acquisition attempt is removed by the contender itself
# (`clear_marker_predating`, periculum #30), so a marker this code finds
# belongs to the run that just failed.

# lock_contention_take <marker-path>
#
# Reads the marker, removes it, and leaves LOCK_VERDICT, LOCK_SUSPECT,
# LOCK_HOLDER_PID, LOCK_HOLDER_AGE_SECS and LOCK_DETAIL set (empty when the
# marker did not carry that field). Returns 0 if there was a marker to take,
# 1 if there was none.
#
# Read THEN remove: the deletion is what this helper exists to move to the
# end. All three runners used to `rm -f` the file inside the branch that
# decided contention had happened, which destroyed the identity at the exact
# moment it became usable.
lock_contention_take() {
    local marker="$1"
    LOCK_VERDICT=""
    LOCK_SUSPECT=""
    LOCK_HOLDER_PID=""
    LOCK_HOLDER_AGE_SECS=""
    LOCK_DETAIL=""
    [ -f "$marker" ] || return 1

    local line key value
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in
            *=*) ;;
            *) continue ;;
        esac
        key=${line%%=*}
        value=${line#*=}
        # LOCK_DETAIL is periculum's own sentence about the holder and is read
        # by the callers, not here -- it is what the 03:37 notification and
        # the tier-3 banner quote.
        # shellcheck disable=SC2034
        case "$key" in
            verdict)         LOCK_VERDICT="$value" ;;
            suspect)         LOCK_SUSPECT="$value" ;;
            holder_pid)      LOCK_HOLDER_PID="$value" ;;
            holder_age_secs) LOCK_HOLDER_AGE_SECS="$value" ;;
            detail)          LOCK_DETAIL="$value" ;;
        esac
    done < "$marker"

    rm -f "$marker"
    return 0
}

# Whether the contention just taken names a holder periculum could not call
# legitimate: a live holder past 24 h, an identity the kernel disagrees with,
# or a held lock nothing claims.
#
# The default is NOT suspect. An empty pre-#30 marker carries no `suspect=`
# field, and inventing an accusation from an absent field would put a wedge
# report in the ledger of every marker written before periculum #31.
lock_contention_is_suspect() {
    [ "${LOCK_SUSPECT:-false}" = "true" ]
}

# The ledger token: the historic `lock-held` for an overlap, a distinct
# `lock-suspect` for the other kind. Distinct rather than RED, because the
# verdict is a heuristic over metadata and the contender never touched the
# lock — a false accusation costs somebody killing a healthy nightly. What
# changes is what the ledger and the notification SAY.
lock_contention_token() {
    if lock_contention_is_suspect; then
        printf 'lock-suspect'
    else
        printf 'lock-held'
    fi
}

# The key=value tail both the ledger line and the notification carry. Fields
# the marker did not have are left out rather than printed empty, so a value
# in the ledger is always one periculum actually wrote.
lock_contention_fields() {
    local out="verdict=${LOCK_VERDICT:-unrecorded}"
    if [ -n "$LOCK_HOLDER_PID" ]; then
        out="$out holder_pid=$LOCK_HOLDER_PID"
    fi
    if [ -n "$LOCK_HOLDER_AGE_SECS" ]; then
        out="$out holder_age_secs=$LOCK_HOLDER_AGE_SECS"
    fi
    printf '%s' "$out"
}
