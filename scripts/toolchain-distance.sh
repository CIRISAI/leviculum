#!/usr/bin/env bash
#
# Say how far the pinned Rust toolchain has fallen behind current stable.
# One line, on stdout, and exit 0 whatever the answer is (Codeberg #304).
#
# WHY THIS REPORTS AND NEVER REFUSES
#
# The pin in rust-toolchain.toml is raised deliberately at release time.
# Nothing said when that was due, and a policy whose only home is a comment
# is what this project has decided not to rely on. The obvious alternative
# -- a gate that goes red once stable has moved -- has a shape this repo has
# already paid for once: until 2026-08-07 a pre-push gate consumed a verdict
# word (OK/WARN/STALE) from a tier-2 staleness script, that word read the
# same at 25 hours as at the 46 days it had actually been, and it blocked
# every push for those 46 days with a remedy that could not clear it. The
# note in scripts/ci-status.sh above its tier-2 block records it.
#
# A staleness gate on the pin would be worse than that one, because it goes
# red on the Rust release train's schedule rather than on any change of
# ours, and the cheapest way to clear it is to bump the pin without
# considering it -- which is exactly the unconsidered drift the pin exists
# to prevent. So: a fact, printed, with the judgement left to the reader.
#
# For the same reason the line carries the DISTANCE and not only a verdict:
# "bump due" alone compresses one release and five into the same word, which
# is the failure mode above in miniature.
#
# Every unknown is also a printed fact, never an error: no network, no
# rustup, no `stable` toolchain installed, a non-numeric channel. This is
# called from the nightly report and from `just toolchain-status`, and
# neither may lose a run because the machine was offline.
#
# WHY A "BEHIND" LINE NAMES THE XTENSA PAIRING
#
# The host pin moves only together with the esp-rs Xtensa toolchain of the
# same generation, and only once esp-rs has taken that one out of
# prerelease (the pairing rule and Lew's 2026-10-05 decision sit in
# scripts/install-ci.sh, above the espup pins). On 2026-10-04 stable was
# 1.99.0 while every esp-rs 1.98.x and 1.99.x was still a prerelease, so
# "bump due" printed every morning about a bump that could not be made --
# the unconsidered-bump pressure the paragraph above warns about, from the
# other side. So every "behind" line also says whether the pairing exists:
# "bump due (esp-rs vX paired)", "bump waits for esp-rs (...)", or "esp-rs
# pairing unknown (...)". All three still exit 0.
#
# Usage:
#   bash scripts/toolchain-distance.sh        # or: just toolchain-status

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOML="${LEV_TOOLCHAIN_TOML:-$ROOT/rust-toolchain.toml}"

say() { echo "toolchain: $*"; exit 0; }

# --- The pin --------------------------------------------------------------
#
# Only the `channel` key, and only outside comments: the file's comment block
# quotes version numbers in prose (the stack-frame measurement, the targets
# note), so a grep for a version rather than for the key would find one of
# those first.
if [ ! -r "$TOML" ]; then
    say "no rust-toolchain.toml at $TOML"
fi
PINNED="$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$TOML" | head -1)"
if [ -z "$PINNED" ]; then
    say "rust-toolchain.toml names no channel"
fi
case "$PINNED" in
    [0-9]*.[0-9]*) ;;
    # `stable`, `nightly`, `beta`, a date-stamped nightly: the pin follows
    # the train by construction, so there is no distance to state. Not an
    # error -- a clone may legitimately float -- but worth saying out loud,
    # because a floating channel is what #298 pinned away from.
    *) say "channel is \"$PINNED\", which floats -- no pinned version to compare" ;;
esac

# --- Current stable -------------------------------------------------------
#
# `rustup check` is the only source that does not need a network client of
# our own. It reports one line per INSTALLED channel toolchain:
#
#   stable-x86_64-unknown-linux-gnu - Update available : 1.95.0 (...) -> 1.98.1 (...)
#   stable-x86_64-unknown-linux-gnu - Up to date : 1.98.1 (...)
#
# In both spellings the last x.y.z on the line is the newest stable release,
# which is the number wanted here -- deliberately NOT "the stable toolchain
# this machine has installed", which is a fact about this machine. The
# "(Unknown version) -> 1.98.1" spelling rustup emits for a toolchain it
# cannot read falls out of the same rule.
#
# A specific-version pin (1.97.1) is not a channel, so rustup never lists it
# and this cannot accidentally compare the pin against itself.
#
# The timeout is not decoration: this runs inside a nightly report, and
# rustup talking to static.rust-lang.org over a dead link is a wait with no
# ceiling of its own.
if ! command -v rustup >/dev/null 2>&1; then
    say "pinned $PINNED, current stable unknown (no rustup on PATH)"
fi
CHECK="$(timeout "${LEV_RUSTUP_TIMEOUT:-60}" rustup check 2>/dev/null)" || CHECK=""
STABLE="$(printf '%s\n' "$CHECK" \
    | grep '^stable-' \
    | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' \
    | tail -1)"
if [ -z "$STABLE" ]; then
    # Offline, rustup errored, or no `stable` toolchain is installed for it
    # to check -- a fresh CI container pins straight to a version and has
    # none. All three are the same fact for the reader: no number today.
    say "pinned $PINNED, current stable unknown (rustup check reported no stable channel -- offline, or stable not installed)"
fi

if [ "$PINNED" = "$STABLE" ]; then
    say "pinned $PINNED, current stable $STABLE -- up to date"
fi

# --- The distance ---------------------------------------------------------
#
# Sorted with `sort -V` rather than by hand: a field-by-field comparison in
# shell is where an off-by-one between 1.9.0 and 1.10.0 lives.
OLDER="$(printf '%s\n%s\n' "$PINNED" "$STABLE" | sort -V | head -1)"
if [ "$OLDER" = "$STABLE" ]; then
    # A pin ahead of stable is a beta or a not-yet-released number, not a
    # bump that is due. Say which way round it is instead of reporting a
    # negative distance.
    say "pinned $PINNED, current stable $STABLE -- pinned ahead of current stable"
fi

p_major="${PINNED%%.*}"; p_rest="${PINNED#*.}"; p_minor="${p_rest%%.*}"
s_major="${STABLE%%.*}"; s_rest="${STABLE#*.}"; s_minor="${s_rest%%.*}"

# --- The Xtensa pairing ---------------------------------------------------
#
# One read of the esp-rs release list, newest first. The 30 newest reach
# back about a year of releases, far past any pairing still worth naming.
# `curl` is looked up on PATH like `rustup` above, which is the seam the
# selftest uses: a fake `curl` that prints a fixture body.
#
# Parsed with python3's json module, not with `grep -o` over the text:
# GitHub promises no key order inside a release object, and a grep that
# pairs each `tag_name` with the next `prerelease` silently pairs the wrong
# two the day the order changes. A JSON parse also tells a rate-limit
# answer (an object with a `message`) apart from a release list, where a
# grep would find no tags in either and call both "none paired".
# No `-f` on curl for the same reason: a 403 body is read and named
# unreadable, not reported as offline.
ESP_RELEASES_URL="https://api.github.com/repos/esp-rs/rust-build/releases?per_page=30"
ESP_PARSE='
import json, re, sys
gen = sys.argv[1]
try:
    rels = json.load(sys.stdin)
except ValueError:
    sys.exit(3)
if not isinstance(rels, list):
    sys.exit(3)
def key(tag):
    return tuple(int(x) for x in tag[1:].split("."))
released = [r["tag_name"] for r in rels
            if isinstance(r, dict)
            and r.get("prerelease") is False and r.get("draft") is False
            and re.fullmatch(r"v[0-9]+(\.[0-9]+)+", str(r.get("tag_name", "")))]
paired = [t for t in released if t.startswith("v" + gen + ".")]
if paired:
    print("paired", max(paired, key=key))
elif released:
    print("waits", max(released, key=key))
else:
    print("waits -")
'

pairing() { # -> the clause that follows "behind, "
    local body verdict tag t_ver t_major t_rest t_minor
    body="$(curl -s -m 15 "$ESP_RELEASES_URL" 2>/dev/null)" || body=""
    if [ -z "$body" ]; then
        echo "esp-rs pairing unknown (offline)"
        return
    fi
    verdict="$(printf '%s' "$body" | python3 -c "$ESP_PARSE" "$s_major.$s_minor" 2>/dev/null)" || verdict=""
    tag="${verdict#* }"
    case "$verdict" in
        "paired "*) echo "bump due (esp-rs $tag paired)" ;;
        "waits -")
            echo "bump waits for esp-rs (no non-prerelease Xtensa among its 30 newest releases)" ;;
        "waits "*)
            t_ver="${tag#v}"
            t_major="${t_ver%%.*}"; t_rest="${t_ver#*.}"; t_minor="${t_rest%%.*}"
            if [ "$t_major.$t_minor" = "$p_major.$p_minor" ]; then
                echo "bump waits for esp-rs (newest non-prerelease Xtensa is $tag, same generation as the pin)"
            else
                # Not the pin's generation: say whose it is, rather than
                # print a "same generation" the numbers contradict.
                echo "bump waits for esp-rs (newest non-prerelease Xtensa is $tag, generation $t_major.$t_minor, pin is $p_major.$p_minor)"
            fi ;;
        *) echo "esp-rs pairing unknown (release list unreadable)" ;;
    esac
}

if [ "$p_major" = "$s_major" ] && [ "$s_minor" -gt "$p_minor" ] 2>/dev/null; then
    # Rust ships one minor per release, so the minor delta IS the number of
    # releases -- the unit the reader thinks in and the one the release
    # calendar is written in.
    n=$((s_minor - p_minor))
    if [ "$n" = 1 ]; then
        say "pinned $PINNED, current stable $STABLE -- 1 release behind, $(pairing)"
    fi
    say "pinned $PINNED, current stable $STABLE -- $n releases behind, $(pairing)"
fi

# Same minor, higher patch: a point release, which Rust cuts out of band.
if [ "$p_major" = "$s_major" ] && [ "$p_minor" = "$s_minor" ]; then
    say "pinned $PINNED, current stable $STABLE -- a point release behind, $(pairing)"
fi

# A major change, or any shape the two branches above do not describe. Both
# numbers are printed, which is the part that matters.
say "pinned $PINNED, current stable $STABLE -- behind, $(pairing)"
