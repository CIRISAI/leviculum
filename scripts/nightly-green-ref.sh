#!/usr/bin/env bash
# The tier-2 nightly's verdict, written where the forge can read it (Codeberg #312).
#
# THE PROBLEM. `.woodpecker/nightly.yml` publishes .debs to strangers from a
# cron run whose only test is `just ci-gate` — fmt, clippy and the workspace
# LIB tests. `rnsd_interop`, the suite that measures whether we still
# interoperate with a Python-RNS peer, runs in no forge pipeline at all: it
# needs the `reference/Reticulum` submodule and a python3, and both pipelines
# clone with `submodules: false` deliberately (Codeberg #300, and
# `just check-plain-clone` is what holds that). So an interop break reaches the
# public releases page with every forge gate green.
#
# The truth exists already. The tier-2 nightly runs
# `cargo test --workspace --all-targets` over a fresh, pinned clone WITH
# submodules every night, and `rnsd_interop` is one of its units. What was
# missing is a channel from that verdict to the forge. This script is that
# channel: on a green night it pushes a lightweight ref naming the commit it
# tested, and `scripts/check-nightly-green.sh` on the publish side refuses a
# commit no such ref covers.
#
# WHY A REF. The remote is the one we already push to, so there is no new
# credential, no new host-to-host channel and no forge API to invent. A ref is
# also the one artefact that binds to a commit BY CONSTRUCTION — it names one —
# so "which commit was tested" cannot drift the way a status file or a release
# note can.
#
# WHY ONE REF PER RUN, TIMESTAMPED, RATHER THAN A MOVING `refs/nightly/green`:
#
#   * `refs/nightly/green/<YYYYMMDDTHHMMSSZ>` -> the tested commit.
#   * The name carries the NIGHTLY's own time, which is the only clock the
#     staleness bound on the publish side can honestly use. A moving ref
#     carries no time at all, and the commit's own committer date is not a
#     substitute: on a catch-up run — the timer is `Persistent=true`, so a slot
#     the machine slept through is owed rather than lost — the code can be days
#     older than the run that tested it.
#   * The set is append-only, so a run on an OLDER commit adds a signal instead
#     of moving the only one backwards. A moving ref force-pushed by a catch-up
#     run would silently retract coverage the publish side had already earned.
#
# Refs older than --retention-days are deleted in the same push, so the set
# stays bounded (one ref per night, ~30 live).
#
# WHAT IT REFUSES TO SIGN. The caller hands in the night's overall verdict, and
# a non-green verdict simply means no ref — exit 0, that is the system working.
# But the one property this whole gate is about is not taken on trust: the
# run's own test manifest must show that the `rnsd_interop` unit EXECUTED and
# that every test in it passed. `scripts/run-with-manifest.py` writes that
# manifest for every `{{manifest}}` gate, `just complete` is one of them, and a
# selector that matched nothing appears there as a unit with an empty `ok`
# list rather than as silence. Without that check "the nightly was green" could
# mean "the suite never ran", which is the defect class Guarantee B exists for
# (docs/src/concepts/checks-and-citations.md).
#
# Usage (from the tree that was tested, after the verdict is known):
#
#   bash scripts/nightly-green-ref.sh \
#       --commit   <sha of the tested commit> \
#       --verdict  green|red \
#       --remote   ssh://git@codeberg.org/Lew_Palm/leviculum.git
#
# `--manifest` defaults to the `workspace-all-targets` manifest THIS checkout
# writes, located by the same formula scripts/run-with-manifest.py uses
# (manifest_dir(), the `<dirname>-<sha1 of the real path>` slug), so the
# caller does not have to know it and cannot be handed another tree's manifest
# by accident.
#
# `--remote` may be left out when a remote of this checkout points at the
# forge; the nightly runs in a clone whose `origin` is a LOCAL path, so there
# it has to be named. --dry-run prints the push instead of making it.
#
# Exit 0 = a ref was pushed, or the night was honestly not green.
# Exit 1 = the evidence could not be established, or the push failed. Both are
#          wiring faults and neither may pass silently.

set -uo pipefail

COMMIT=""
VERDICT=""
MANIFEST=""
REMOTE="${LEVICULUM_FORGE_REMOTE:-}"
RETENTION_DAYS="${LEVICULUM_NIGHTLY_REF_RETENTION_DAYS:-30}"
DRY_RUN=0

die() { echo "nightly-green-ref: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --commit)          COMMIT="${2:-}"; shift 2 ;;
        --verdict)         VERDICT="${2:-}"; shift 2 ;;
        --manifest)        MANIFEST="${2:-}"; shift 2 ;;
        --remote)          REMOTE="${2:-}"; shift 2 ;;
        --retention-days)  RETENTION_DAYS="${2:-}"; shift 2 ;;
        --dry-run)         DRY_RUN=1; shift ;;
        -h|--help)         sed -n '2,70p' "$0"; exit 0 ;;
        *)                 die "unknown argument '$1'" ;;
    esac
done

[ -n "$COMMIT" ]   || die "--commit is required"
[ -n "$VERDICT" ]  || die "--verdict is required (green|red)"

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"

# --- The night's own verdict ----------------------------------------------
#
# Anything but green means no ref, and that is not an error: it is the entire
# point of the mechanism. Said out loud, because a channel that goes quiet is
# indistinguishable from a channel that is broken.
case "$VERDICT" in
    green|GREEN|GRUEN|gruen) ;;
    *)
        echo "nightly-green-ref: verdict '$VERDICT' is not green — no ref pushed."
        echo "nightly-green-ref: the forge will refuse to publish ${COMMIT:0:12} until a green night covers it."
        exit 0
        ;;
esac

# --- The evidence, which is not taken on trust ----------------------------
#
# Located rather than passed in, by the formula scripts/run-with-manifest.py
# resolves manifest_dir() with. Two checkouts on one host — the nightly's
# pinned clone and the developer's working tree — write into different
# directories on purpose, and a caller reaching for the wrong one would read a
# green interop result that belongs to another tree.
if [ -z "$MANIFEST" ]; then
    MANIFEST="$(
        REPO_DIR="$REPO_DIR" python3 - <<'PY'
import hashlib, os
from pathlib import Path

override = os.environ.get("LEVICULUM_MANIFEST_DIR")
if override:
    d = Path(override)
else:
    root = Path(os.environ["REPO_DIR"]).resolve()
    state = os.environ.get("XDG_STATE_HOME") or str(Path.home() / ".local" / "state")
    slug = f"{root.name}-{hashlib.sha1(str(root).encode()).hexdigest()[:8]}"
    d = Path(state) / "leviculum-ci" / "test-manifests" / slug
print(d / "workspace-all-targets.json")
PY
    )"
    echo "nightly-green-ref: manifest $MANIFEST"
fi
[ -n "$MANIFEST" ] || die "--manifest is required: a green verdict alone does not say rnsd_interop ran"
[ -r "$MANIFEST" ] || die "manifest '$MANIFEST' is not readable — cannot show that rnsd_interop ran"

# python3 is the nightly host's, not the publish container's: this script runs
# beside `just complete`, which is itself driven by a python3 wrapper.
manifest_verdict="$(
    MANIFEST="$MANIFEST" COMMIT="$COMMIT" python3 - <<'PY'
import json, os, sys

path = os.environ["MANIFEST"]
want_commit = os.environ["COMMIT"]

try:
    with open(path) as fh:
        m = json.load(fh)
except Exception as exc:  # noqa: BLE001 - the message is the product
    print(f"REFUSE manifest is not readable JSON: {exc}")
    sys.exit(0)

if m.get("schema") != 2:
    print(f"REFUSE manifest schema is {m.get('schema')!r}, this script reads schema 2")
    sys.exit(0)

got_commit = m.get("commit") or ""
if not (got_commit.startswith(want_commit) or want_commit.startswith(got_commit)):
    print(f"REFUSE manifest records commit {got_commit[:12]!r}, the ref would name {want_commit[:12]!r}")
    sys.exit(0)

if m.get("dirty"):
    print("REFUSE the manifest was recorded on a DIRTY tree; it describes no commit")
    sys.exit(0)

if m.get("exit_code") != 0:
    print(f"REFUSE the run this manifest belongs to exited {m.get('exit_code')!r}")
    sys.exit(0)

units = [u for u in m.get("units", []) if "rnsd_interop" in (u.get("selector") or "")]
if not units:
    print("REFUSE no unit in the manifest names rnsd_interop — the suite did not run")
    sys.exit(0)

ok = failed = 0
for u in units:
    ok += len(u.get("ok") or [])
    failed += len(u.get("failed") or [])

if failed:
    print(f"REFUSE rnsd_interop had {failed} failing test(s)")
    sys.exit(0)
if ok == 0:
    print("REFUSE rnsd_interop is in the manifest but executed zero tests")
    sys.exit(0)

print(f"ACCEPT rnsd_interop: {ok} test(s) passed, 0 failed")
PY
)"

case "$manifest_verdict" in
    ACCEPT*)
        echo "nightly-green-ref: ${manifest_verdict#ACCEPT }"
        ;;
    *)
        echo "nightly-green-ref: NO REF — ${manifest_verdict#REFUSE }" >&2
        echo "nightly-green-ref: the night reported green, but its own manifest does not" >&2
        echo "nightly-green-ref: show a green rnsd_interop. That disagreement is a wiring" >&2
        echo "nightly-green-ref: fault, not a quiet night: manifest $MANIFEST" >&2
        exit 1
        ;;
esac

# --- Which remote ---------------------------------------------------------
#
# Not simply `origin`: the nightly runs in a clone of the developer's working
# tree, so its `origin` is a local path and a ref pushed there reaches nobody.
# A remote is accepted only if it names the forge.
if [ -z "$REMOTE" ]; then
    while read -r name _; do
        [ -n "$name" ] || continue
        url="$(git -C "$REPO_DIR" remote get-url --push "$name" 2>/dev/null)"
        case "$url" in
            *codeberg.org*) REMOTE="$url"; break ;;
        esac
    done < <(git -C "$REPO_DIR" remote 2>/dev/null | sed 's/$/ /')
fi
if [ -z "$REMOTE" ]; then
    echo "nightly-green-ref: no forge remote found in $REPO_DIR and none named." >&2
    echo "nightly-green-ref: pass --remote <url> or set LEVICULUM_FORGE_REMOTE." >&2
    echo "nightly-green-ref: the nightly runs in a clone whose origin is a local" >&2
    echo "nightly-green-ref: path, so there the URL must be named explicitly." >&2
    exit 1
fi

# --- The refspecs ---------------------------------------------------------
NOW_EPOCH="$(date -u +%s)"
STAMP="$(date -u -d "@$NOW_EPOCH" +%Y%m%dT%H%M%SZ)"
NEW_REF="refs/nightly/green/$STAMP"

REFSPECS=("+${COMMIT}:${NEW_REF}")

# Prune in the same push. Reading the remote first costs one round trip and
# keeps the ref set bounded without a second scheduled job — an unscheduled
# cleanup is a cleanup that never runs.
CUTOFF=$(( NOW_EPOCH - RETENTION_DAYS * 86400 ))
pruned=0
while read -r _sha ref; do
    case "$ref" in refs/nightly/green/*) ;; *) continue ;; esac
    name="${ref##*/}"
    case "$name" in
        [0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]T[0-9][0-9][0-9][0-9][0-9][0-9]Z) ;;
        *) continue ;;
    esac
    iso="${name:0:4}-${name:4:2}-${name:6:2}T${name:9:2}:${name:11:2}:${name:13:2}Z"
    epoch="$(date -u -d "$iso" +%s 2>/dev/null)" || continue
    if [ "$epoch" -lt "$CUTOFF" ]; then
        REFSPECS+=(":${ref}")
        pruned=$((pruned + 1))
    fi
done < <(git -C "$REPO_DIR" ls-remote "$REMOTE" 'refs/nightly/green/*' 2>/dev/null)

echo "nightly-green-ref: $NEW_REF -> ${COMMIT:0:12} on $REMOTE"
[ "$pruned" -gt 0 ] && echo "nightly-green-ref: pruning $pruned ref(s) older than ${RETENTION_DAYS} days"

if [ "$DRY_RUN" = "1" ]; then
    echo "nightly-green-ref: DRY RUN — would push: ${REFSPECS[*]}"
    exit 0
fi

if ! git -C "$REPO_DIR" push "$REMOTE" "${REFSPECS[@]}"; then
    echo "nightly-green-ref: the push FAILED — the forge has no signal for ${COMMIT:0:12}." >&2
    echo "nightly-green-ref: the next publish run will refuse, which is the safe" >&2
    echo "nightly-green-ref: direction, but the cause is here and not there." >&2
    exit 1
fi

echo "nightly-green-ref: done"
