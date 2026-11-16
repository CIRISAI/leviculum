#!/usr/bin/env bash
# Show or set the schedule of the forge's "Nightly Build" cron (1513, #474).
#
# The schedule lives on the forge, not in `.woodpecker/nightly.yml`: YAML
# cannot declare a Woodpecker cron, the file only reacts to the `event: cron`
# it produces, and its comment documents the schedule. This script is how the
# schedule is changed, so the change is reproducible and not a click in the
# web UI nobody can review.
#
# THE RULE (see the comment above `when:` in nightly.yml): start five minutes
# after a three-hour mark, and the whole run must end before the next one.
# Codeberg's Woodpecker server restarts at 00, 03, 06, ... UTC and cancels
# what is running; a full nightly run is about 75 minutes.
#
# Usage:
#   scripts/forge-cron-schedule.sh            print the repo's crons
#   scripts/forge-cron-schedule.sh --set      set "Nightly Build" to $SCHEDULE
#
# The Woodpecker API takes a Woodpecker personal token (ci.codeberg.org, User
# settings -> Personal access token), not the Codeberg one. It is read from
# $WOODPECKER_TOKEN, which wins, or from the file both hosts keep it in,
# ${XDG_CONFIG_HOME:-$HOME/.config}/woodpecker/token, and never printed.
set -euo pipefail

SERVER="${WOODPECKER_SERVER:-https://ci.codeberg.org}"
REPO_ID="${WOODPECKER_REPO_ID:-16851}"   # Lew_Palm/leviculum
CRON_NAME="${CRON_NAME:-Nightly Build}"
SCHEDULE="${SCHEDULE:-5 9 * * *}"

die() { echo "forge-cron-schedule: $*" >&2; exit 1; }

mode="show"
case "${1:-}" in
    "") ;;
    --set) mode="set" ;;
    -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
    *) die "unknown argument '$1' (try --help)" ;;
esac

command -v curl >/dev/null || die "curl is missing"
command -v jq >/dev/null || die "jq is missing"

token="${WOODPECKER_TOKEN:-}"
if [ -z "$token" ]; then
    f="${XDG_CONFIG_HOME:-$HOME/.config}/woodpecker/token"
    [ -r "$f" ] || die "no token: set WOODPECKER_TOKEN or create $f"
    token="$(tr -d '[:space:]' <"$f")"
fi
[ -n "$token" ] || die "the token is empty"

# The token goes in through a header file descriptor, so it never appears in
# the process list.
api() {
    local method="$1" path="$2"
    shift 2
    curl -sS --fail-with-body -X "$method" \
        -H @<(printf 'Authorization: Bearer %s\n' "$token") \
        -H 'Content-Type: application/json' \
        "$SERVER/api/repos/$REPO_ID$path" "$@"
}

crons="$(api GET /cron)" || die "listing the crons failed: $crons"
if [ "$mode" = show ]; then
    jq -r '.[] | "\(.id)\t\(.name)\t\(.schedule)\t\(.branch)"' <<<"$crons"
    exit 0
fi

id="$(jq -r --arg n "$CRON_NAME" '[.[] | select(.name == $n)] | if length == 1 then .[0].id else empty end' <<<"$crons")"
[ -n "$id" ] || die "no single cron named '$CRON_NAME'; the repo has: $(jq -r '[.[].name] | join(", ")' <<<"$crons")"
old="$(jq -r --arg n "$CRON_NAME" '.[] | select(.name == $n) | .schedule' <<<"$crons")"

body="$(jq -nc --arg s "$SCHEDULE" '{schedule: $s}')"
resp="$(api PATCH "/cron/$id" -d "$body")" || die "updating cron $id failed: $resp"
new="$(jq -r '.schedule' <<<"$resp")"
[ "$new" = "$SCHEDULE" ] || die "the forge answered schedule '$new', expected '$SCHEDULE'"
echo "cron $id '$CRON_NAME': '$old' -> '$new' (next: $(jq -r '.next_exec // "?"' <<<"$resp"))"
