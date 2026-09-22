#!/usr/bin/env bash
#
# Selftest for scripts/toolchain-distance.sh.
#
# Two properties, and neither is visible by reading the script:
#
#   1. It NEVER refuses. The whole point of #304 is a report rather than a
#      gate, and a report that exits non-zero when the machine is offline,
#      has no rustup, or has no `stable` toolchain installed becomes a gate
#      the moment something consumes it -- which is the failure this repo
#      already paid 46 days of blocked pushes for. Every case below asserts
#      the exit code, the unknowns included.
#
#   2. The number it reports is the NEWEST stable release, not the stable
#      toolchain this machine happens to have installed. `rustup check`
#      prints both on the same line, older first, so the two are one parse
#      away from each other -- and the wrong one of them is a number that
#      looks entirely plausible and never moves. The negative control is
#      explicit: the "Update available" case fails if 1.95.0 ever appears
#      in the output.
#
# The fixture drives a fake `rustup` on PATH, so there is no network and
# no dependency on what this host has installed. ~1 s.
#
# Usage: bash scripts/test-toolchain-distance.sh

set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
# Exported, not passed per invocation: the fake rustup below reads it, and a
# `WORK=... bash` prefix would leave shellcheck unable to tell which of the
# two WORKs the PATH beside it expands.
export WORK
trap 'rm -rf "$WORK"' EXIT

failures=0
fail() { echo "  FAIL: $*"; failures=$((failures + 1)); }

mkdir -p "$WORK/tree/scripts" "$WORK/bin"
cp "$REPO/scripts/toolchain-distance.sh" "$WORK/tree/scripts/"

# The fixture toolchain file carries a comment block quoting other version
# numbers, exactly as the real one does (its stack-frame measurement and its
# `rustup target add` notes). A parse that greps the file for a version
# instead of for the `channel` key finds 1.95.0 here and is wrong in a way
# that still prints a believable line.
write_toml() { # <channel>
    cat > "$WORK/tree/rust-toolchain.toml" <<EOF
[toolchain]
# Both hosts sat on 1.95.0 from April while a container resolved 1.97.1.
# Raising the pin needs: rustup target add thumbv7em-none-eabihf
channel = "$1"
targets = ["x86_64-unknown-linux-musl"]
EOF
}

# The fake rustup writes whatever the case put in $WORK/rustup.out and exits
# with $WORK/rustup.rc, which is how the offline case is expressed.
cat > "$WORK/bin/rustup" <<'EOF'
#!/usr/bin/env bash
cat "$WORK/rustup.out" 2>/dev/null
exit "$(cat "$WORK/rustup.rc" 2>/dev/null || echo 0)"
EOF
chmod +x "$WORK/bin/rustup"

set_rustup() { # <rc> <output...>
    echo "$1" > "$WORK/rustup.rc"
    shift
    printf '%s\n' "$@" > "$WORK/rustup.out"
}

run() { # runs the script with the fake rustup first on PATH
    ( cd "$WORK/tree" && PATH="$WORK/bin:$PATH" \
        bash scripts/toolchain-distance.sh ) > "$WORK/out" 2>&1
    echo "$?" > "$WORK/rc"
}

expect() { # <case> <expected line>
    local case="$1" want="$2" got rc
    got="$(cat "$WORK/out")"
    rc="$(cat "$WORK/rc")"
    [ "$rc" = 0 ] || fail "$case: exit $rc, and this must never refuse"
    [ "$got" = "$want" ] || fail "$case: got '$got', expected '$want'"
    # One line, always: the nightly pastes it into a status block.
    [ "$(wc -l < "$WORK/out")" = 1 ] || fail "$case: printed more than one line"
}

UPDATE_LINE="stable-x86_64-unknown-linux-gnu - Update available : 1.95.0 (59807616e 2026-04-14) -> 1.98.1 (48a229cea 2026-09-01)"
NOISE_1="nightly-x86_64-unknown-linux-gnu - Update available : 1.98.0-nightly (c1b22f44c 2026-06-17) -> 1.100.0-nightly (1303417c4 2026-09-21)"
NOISE_2="rustup - Update available : 1.27.1 -> 1.29.1"

echo "[case] behind, with the installed stable on the same line"
write_toml 1.96.0
set_rustup 0 "$UPDATE_LINE" "$NOISE_1" "$NOISE_2"
run
expect "behind" "toolchain: pinned 1.96.0, current stable 1.98.1 -- 2 releases behind, bump due"
# The negative control for property 2 above: reporting the number rustup
# lists FIRST would print a line that reads fine and is about this machine.
grep -q '1\.95\.0' "$WORK/out" \
    && fail "behind: reported the installed stable (1.95.0) instead of the newest release"

echo "[case] one release behind reads as one, not as a plural"
write_toml 1.97.1
set_rustup 0 "$UPDATE_LINE"
run
expect "one-behind" "toolchain: pinned 1.97.1, current stable 1.98.1 -- 1 release behind, bump due"

echo "[case] up to date"
write_toml 1.98.1
set_rustup 0 "stable-x86_64-unknown-linux-gnu - Up to date : 1.98.1 (48a229cea 2026-09-01)" "$NOISE_2"
run
expect "up-to-date" "toolchain: pinned 1.98.1, current stable 1.98.1 -- up to date"

echo "[case] a point release behind"
write_toml 1.98.0
set_rustup 0 "stable-x86_64-unknown-linux-gnu - Up to date : 1.98.1 (48a229cea 2026-09-01)"
run
expect "point-release" "toolchain: pinned 1.98.0, current stable 1.98.1 -- a point release behind, bump due"

echo "[case] pinned ahead of stable"
write_toml 1.99.0
set_rustup 0 "stable-x86_64-unknown-linux-gnu - Up to date : 1.98.1 (48a229cea 2026-09-01)"
run
expect "ahead" "toolchain: pinned 1.99.0, current stable 1.98.1 -- pinned ahead of current stable"

echo "[case] rustup cannot read the installed toolchain"
# rustup's own spelling when it has no version for the installed side. The
# last number on the line is still the release, which is what makes the
# parse survive this.
write_toml 1.97.1
set_rustup 0 "stable-x86_64-unknown-linux-gnu - Update available : (Unknown version) -> 1.98.1 (48a229cea 2026-09-01)"
run
expect "unknown-installed" "toolchain: pinned 1.97.1, current stable 1.98.1 -- 1 release behind, bump due"

UNKNOWN="toolchain: pinned 1.97.1, current stable unknown (rustup check reported no stable channel -- offline, or stable not installed)"

echo "[case] offline: rustup exits non-zero"
write_toml 1.97.1
set_rustup 1 "error: could not download file from 'https://static.rust-lang.org/dist/channel-rust-stable.toml'"
run
expect "offline" "$UNKNOWN"

echo "[case] no stable toolchain installed for rustup to check"
write_toml 1.97.1
set_rustup 0 "$NOISE_1" "$NOISE_2"
run
expect "no-stable" "$UNKNOWN"

echo "[case] no rustup on PATH at all"
# A PATH holding the ordinary utilities the script uses and nothing else.
# Emptying PATH instead would test that `bash` is missing, not that rustup
# is -- and would pass for entirely the wrong reason.
write_toml 1.97.1
mkdir -p "$WORK/norustup"
for tool in dirname sed grep head tail sort timeout; do
    ln -sf "$(command -v "$tool")" "$WORK/norustup/$tool"
done
[ -x "$WORK/norustup/sed" ] || fail "no-rustup: could not build the utilities-only PATH"
( cd "$WORK/tree" && PATH="$WORK/norustup" "$(command -v bash)" scripts/toolchain-distance.sh ) > "$WORK/out" 2>&1
echo "$?" > "$WORK/rc"
expect "no-rustup" "toolchain: pinned 1.97.1, current stable unknown (no rustup on PATH)"

echo "[case] a floating channel has no distance to state"
write_toml stable
set_rustup 0 "$UPDATE_LINE"
run
expect "floating" 'toolchain: channel is "stable", which floats -- no pinned version to compare'

echo "[case] no rust-toolchain.toml"
rm -f "$WORK/tree/rust-toolchain.toml"
set_rustup 0 "$UPDATE_LINE"
run
expect "no-toml" "toolchain: no rust-toolchain.toml at $WORK/tree/rust-toolchain.toml"

echo "[case] a toolchain file with no channel key"
printf '[toolchain]\ntargets = ["x86_64-unknown-linux-musl"]\n' > "$WORK/tree/rust-toolchain.toml"
run
expect "no-channel" "toolchain: rust-toolchain.toml names no channel"

echo
if [ "$failures" -eq 0 ]; then
    echo "test-toolchain-distance: all cases passed"
    exit 0
fi
echo "test-toolchain-distance: FAILED (${failures} assertion(s))"
exit 1
