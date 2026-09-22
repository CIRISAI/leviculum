#!/usr/bin/env bash
# The publish side of the nightly signal: refuse a commit no green night covers.
#
# Codeberg #312. `.woodpecker/nightly.yml` publishes .debs, tarballs and the
# lnflash bundle to strangers. What gates that is `just ci-gate` — fmt, clippy
# and the workspace LIB tests — and `rnsd_interop`, the suite that measures
# whether we still interoperate with a Python-RNS peer, is in none of it: the
# suite needs the `reference/Reticulum` submodule and a python3, and both forge
# pipelines clone with `submodules: false` on purpose (#300). Fetching that
# submodule into the release path would undo exactly the property #300 bought,
# so the interop truth is imported instead of re-derived: the tier-2 nightly
# runs the whole workspace with submodules every night and pushes
# `refs/nightly/green/<YYYYMMDDTHHMMSSZ>` at the commit it tested
# (scripts/nightly-green-ref.sh). This script reads those refs.
#
# THREE CONDITIONS, EACH NAMED IN ITS OWN REFUSAL:
#
#   NO-SIGNAL    there is no `refs/nightly/green/*` on the remote at all, or it
#                could not be read.
#   NOT-COVERED  no green ref names this commit or a descendant of it — the
#                nightly has never seen this code.
#   TOO-OLD      the newest covering ref is older than the staleness bound.
#
# It FAILS CLOSED and says which. That is the #434 lesson, and #434 is in the
# same file: a publish input that could not be configured took five weeks of
# releases down while every push pipeline stayed green. A gate that goes quiet
# when its input is missing is worse than no gate, so the refusal names the
# condition, the numbers behind it, and the override.
#
# THE MANUAL OVERRIDE, because a human must be able to decide otherwise:
#
#   LEVICULUM_PUBLISH_WITHOUT_NIGHTLY="<reason, at least 8 characters>"
#
# It is deliberately not a boolean. A reason has to be typed, it is printed
# into the run's log where it stays with the build it excused, and a value too
# short to be a reason is refused. The runbook is
# docs/src/development-ci.md §"What may be published", and every refusal below
# repeats the variable inline — a person reading a red pipeline at 22:00 should
# not have to find the documentation first.
#
# Runs in `debian:bookworm-slim` with `git` apt-installed, and uses nothing
# else beyond bash, coreutils and sed. No python3, no jq, no cargo: see the
# tool inventory on the publish step in `.woodpecker/nightly.yml`.
#
# Usage:
#   bash scripts/check-nightly-green.sh [--commit <sha>] [--remote <url>]
#
# Exit 0 = a green night covers this commit (or the override was given).
# Exit 1 = refused; the reason is on stdout.

set -uo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"

COMMIT="${CI_COMMIT_SHA:-}"
REMOTE=""
# 72 hours. Justified against the nightly's MEASURED cadence rather than its
# schedule, because the schedule is not what it does: over 2026-08-22..09-22
# the timer produced 27 runs, the median gap between consecutive runs was 24 h,
# and every gap but one was <= 54.4 h (the two largest: 2026-09-19 03:39 ->
# 09-21 10:04 = 54.4 h, and 2026-09-17 08:05 -> 09-19 03:39 = 43.6 h). The one
# exception was 2026-09-04 03:39 -> 09-08 03:39, 96 h, and that gap is exactly
# the case this bound exists to stop.
#
# On top of the gap comes an offset: the forge's publish runs on its own cron,
# so the freshest ref it can possibly read is the previous night's and is
# already ~24 h old when it is read. 72 h therefore accepts the ordinary day
# and one missed night, and refuses two consecutive missed nights. Raise it
# here, with a measurement, not in a caller.
MAX_AGE_H="${LEVICULUM_NIGHTLY_MAX_AGE_H:-72}"

while [ $# -gt 0 ]; do
    case "$1" in
        --commit) COMMIT="${2:-}"; shift 2 ;;
        --remote) REMOTE="${2:-}"; shift 2 ;;
        -h|--help) sed -n '2,50p' "$0"; exit 0 ;;
        *) echo "check-nightly-green: unknown argument '$1'" >&2; exit 1 ;;
    esac
done

say() { echo "[nightly-gate] $*"; }

override_hint() {
    echo ""
    echo "[nightly-gate] A human may publish anyway. Re-run the publish step with"
    echo "[nightly-gate]"
    echo "[nightly-gate]   LEVICULUM_PUBLISH_WITHOUT_NIGHTLY=\"why you are doing this\""
    echo "[nightly-gate]"
    echo "[nightly-gate] set to a reason of at least 8 characters. It is printed into"
    echo "[nightly-gate] this log and stays with the build it excused."
    echo "[nightly-gate] Runbook: docs/src/development-ci.md, \"What may be published\"."
}

refuse() {  # <condition> <message...>
    local cond="$1"; shift
    echo ""
    say "REFUSED ($cond): $*"
    override_hint
    exit 1
}

# --- The override ---------------------------------------------------------
#
# First, so a human who has decided pays for no round trip, and so the banner
# is at the top of the log rather than under a page of refusal text.
OVERRIDE="${LEVICULUM_PUBLISH_WITHOUT_NIGHTLY:-}"
if [ -n "$OVERRIDE" ]; then
    if [ "${#OVERRIDE}" -lt 8 ]; then
        say "LEVICULUM_PUBLISH_WITHOUT_NIGHTLY is set to '$OVERRIDE', which is not a reason."
        say "It takes a sentence, not a flag: at least 8 characters saying why this"
        say "build may go out without a green night behind it."
        exit 1
    fi
    say "=============================================================="
    say "OVERRIDDEN. Publishing WITHOUT a green tier-2 nightly behind it."
    say "Reason given: $OVERRIDE"
    say "=============================================================="
    exit 0
fi

[ -n "$COMMIT" ] || refuse NO-SIGNAL "no commit to check — CI_COMMIT_SHA is unset and --commit was not given"

if [ -z "$REMOTE" ]; then
    if [ -n "${CI_REPO:-}" ]; then
        REMOTE="https://codeberg.org/${CI_REPO}.git"
    else
        REMOTE="$(git -C "$REPO_DIR" remote get-url origin 2>/dev/null)"
    fi
fi
[ -n "$REMOTE" ] || refuse NO-SIGNAL "no remote to read the nightly signal from (CI_REPO unset, no origin)"

say "commit    ${COMMIT:0:12}"
say "remote    $REMOTE"
say "bound     ${MAX_AGE_H} h"

# --- Read the signal ------------------------------------------------------
LS_ERR="$(mktemp)"
trap 'rm -f "$LS_ERR"' EXIT
LS_OUT="$(git -C "$REPO_DIR" ls-remote "$REMOTE" 'refs/nightly/green/*' 2>"$LS_ERR")"
LS_RC=$?
if [ "$LS_RC" -ne 0 ]; then
    say "git ls-remote exited $LS_RC:"
    sed 's/^/[nightly-gate]   /' "$LS_ERR"
    refuse NO-SIGNAL "the nightly signal could not be read from the remote"
fi
[ -n "$LS_OUT" ] || refuse NO-SIGNAL \
    "the remote carries no refs/nightly/green/* at all. Either no nightly has been green since the signal was introduced, or scripts/nightly-green-ref.sh is not wired into the nightly run."

# The commits the refs name have to be local before ancestry can be asked, and
# fetching them BY NAME always works — fetching a bare sha needs a server-side
# option no forge promises.
if ! git -C "$REPO_DIR" fetch --quiet --no-tags "$REMOTE" \
        '+refs/nightly/green/*:refs/nightly/green/*' 2>"$LS_ERR"; then
    say "git fetch of the green refs failed:"
    sed 's/^/[nightly-gate]   /' "$LS_ERR"
    refuse NO-SIGNAL "the commits the nightly signed could not be fetched"
fi

# --- Pick the newest ref that COVERS this commit --------------------------
#
# Covers = the commit is the ref's commit, or an ancestor of it. The nightly
# tests master's head; a commit pushed after that night is simply not covered
# yet, and publishing it would publish code no nightly has seen.
#
# Newest COVERING, not newest overall: a later ref that does not cover this
# commit says nothing about it, and the older ref that does is still honest
# evidence. The staleness bound is then applied to the ref that was chosen.
best_ref=""; best_sha=""; best_stamp=""
newest_ref=""; newest_sha=""; newest_stamp=""
seen=0

while read -r sha ref; do
    [ -n "$ref" ] || continue
    case "$ref" in refs/nightly/green/*) ;; *) continue ;; esac
    stamp="${ref##*/}"
    case "$stamp" in
        [0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]T[0-9][0-9][0-9][0-9][0-9][0-9]Z) ;;
        *) continue ;;
    esac
    seen=$((seen + 1))
    # The compact UTC stamp sorts lexicographically, so no `sort` is needed.
    if [ -z "$newest_stamp" ] || [ "$stamp" \> "$newest_stamp" ]; then
        newest_stamp="$stamp"; newest_sha="$sha"; newest_ref="$ref"
    fi
    if [ -n "$best_stamp" ] && [ ! "$stamp" \> "$best_stamp" ]; then
        continue
    fi
    if [ "$sha" = "$COMMIT" ] || git -C "$REPO_DIR" merge-base --is-ancestor "$COMMIT" "$sha" 2>/dev/null; then
        best_stamp="$stamp"; best_sha="$sha"; best_ref="$ref"
    fi
done <<< "$LS_OUT"

[ "$seen" -gt 0 ] || refuse NO-SIGNAL \
    "the remote carries refs under refs/nightly/green/ but none with a parsable <YYYYMMDDTHHMMSSZ> name"

if [ -z "$best_ref" ]; then
    say "the newest green ref is $newest_ref -> ${newest_sha:0:12}"
    say "and ${COMMIT:0:12} is neither that commit nor an ancestor of it."
    say "$seen green ref(s) were considered."
    refuse NOT-COVERED \
        "no green nightly covers ${COMMIT:0:12}. This commit landed after the last green night; the next one will cover it."
fi

say "covered by $best_ref -> ${best_sha:0:12}"

# --- Staleness ------------------------------------------------------------
iso="${best_stamp:0:4}-${best_stamp:4:2}-${best_stamp:6:2}T${best_stamp:9:2}:${best_stamp:11:2}:${best_stamp:13:2}Z"
ref_epoch="$(date -u -d "$iso" +%s 2>/dev/null)"
[ -n "$ref_epoch" ] || refuse TOO-OLD "the ref name '$best_stamp' does not parse as a UTC timestamp"
now_epoch="$(date -u +%s)"
age_h=$(( (now_epoch - ref_epoch) / 3600 ))

# A ref from the future is a clock disagreement between the nightly host and
# this runner, not freshness. It is refused as staleness rather than accepted
# as the freshest thing on the remote.
if [ "$age_h" -lt 0 ]; then
    refuse TOO-OLD "$best_ref is stamped $iso, which is in this runner's future — the two clocks disagree"
fi

say "age       ${age_h} h (stamped $iso)"

if [ "$age_h" -gt "$MAX_AGE_H" ]; then
    refuse TOO-OLD \
        "the newest green night covering ${COMMIT:0:12} is ${age_h} h old, past the ${MAX_AGE_H} h bound. The nightly has missed at least two runs, or has not been green since; check ~/.local/state/leviculum/nightly/latest-status.txt on the nightly host."
fi

say "ACCEPTED: ${COMMIT:0:12} is covered by a green tier-2 nightly ${age_h} h old."
exit 0
