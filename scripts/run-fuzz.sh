#!/usr/bin/env bash
# Run the checked-in cargo-fuzz targets over the untrusted-bytes parsers
# (Codeberg #290).
#
# Eight targets with seed corpora had been sitting in the tree since #23 and
# #108 with nothing to run them: no Justfile recipe, no CI step, no schedule.
# Three defects found by hand in September 2026 sit exactly on top of three of
# them -- unbounded msgpack recursion (#263) and a wrapping bin32 length (#267)
# in `resource_advertisement_unpack`, an uncapped HDLC accumulator (#271) in
# `hdlc_deframe`. A length field, a nesting depth and an unbounded accumulator
# are what a fuzzer finds in minutes. This script is what runs them.
#
# Three properties it exists for, in the order the issue ranks them:
#
#   1. ONE INVOCATION. Nobody should have to reconstruct the nightly-toolchain
#      + glibc-target + seed-path incantation from two READMEs to fuzz at all.
#   2. THE CORPUS PERSISTS. The working corpus and any crash input live under
#      $LEVICULUM_FUZZ_STATE, OUTSIDE the checkout, so coverage accumulates
#      across runs instead of restarting from the seeds every night -- the
#      nightly runs on a fresh clone that is deleted when green, so a corpus
#      inside the tree would be thrown away by construction.
#   3. A CRASH IS LOUD AND KEPT. The input is preserved with its hash and a
#      hexdump, named in the summary, and the exit code separates it from an
#      infrastructure failure.
#
# Exit codes (a run that could not happen must never look like a clean one):
#   0  every target ran its full time budget and found nothing
#   1  at least one target crashed -- the input is under $LEVICULUM_FUZZ_STATE/findings
#   2  could not run: missing toolchain, missing crate, unregistered target
#
# Usage:
#   bash scripts/run-fuzz.sh                    # every target, 60 s each
#   bash scripts/run-fuzz.sh --seconds 900      # the nightly budget
#   bash scripts/run-fuzz.sh hdlc_deframe       # one target by name
#   bash scripts/run-fuzz.sh --list             # what would run, no build
#
# Environment:
#   LEVICULUM_FUZZ_SECONDS   per-target wall budget            (default 60)
#   LEVICULUM_FUZZ_STATE     persistent corpus/findings root
#                            (default ~/.local/state/leviculum-fuzz)
#   LEVICULUM_FUZZ_CRATES    space-separated fuzz crate dirs, repo-relative or
#                            absolute (default the two in this repo)
#   LEVICULUM_FUZZ_MAX_LEN   libFuzzer -max_len                (default 8192)
#   LEVICULUM_FUZZ_RSS_MB    libFuzzer -rss_limit_mb           (default 2048)
#   LEVICULUM_FUZZ_CARGO     cargo binary to drive             (default cargo)

set -uo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"

SECONDS_PER_TARGET="${LEVICULUM_FUZZ_SECONDS:-60}"
STATE="${LEVICULUM_FUZZ_STATE:-$HOME/.local/state/leviculum-fuzz}"
CRATES="${LEVICULUM_FUZZ_CRATES:-leviculum-core/fuzz leviculum-std/fuzz}"
MAX_LEN="${LEVICULUM_FUZZ_MAX_LEN:-8192}"
RSS_MB="${LEVICULUM_FUZZ_RSS_MB:-2048}"
CARGO="${LEVICULUM_FUZZ_CARGO:-cargo}"
# ASan wants glibc; the workspace default target is musl (.cargo/config.toml),
# so this is passed explicitly rather than left to cargo-fuzz's default.
HOST_TARGET="x86_64-unknown-linux-gnu"

LIST_ONLY=0
WANTED=()

while [ $# -gt 0 ]; do
    case "$1" in
        --seconds) SECONDS_PER_TARGET="${2:?--seconds needs a value}"; shift 2 ;;
        --seconds=*) SECONDS_PER_TARGET="${1#*=}"; shift ;;
        --list) LIST_ONLY=1; shift ;;
        -h|--help) sed -n '2,50p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        -*) echo "ERROR: unknown flag '$1'" >&2; exit 2 ;;
        *) WANTED+=("$1"); shift ;;
    esac
done

die() { echo "[run-fuzz] ERROR: $*" >&2; exit 2; }

case "$SECONDS_PER_TARGET" in
    ''|*[!0-9]*) die "--seconds wants a whole number of seconds, got '$SECONDS_PER_TARGET'" ;;
esac

# Preconditions, each with the command that fixes it. A missing toolchain is
# exit 2, never a green run over zero targets.
command -v "$CARGO" >/dev/null 2>&1 || die "no '$CARGO' on PATH"
if ! "$CARGO" +nightly --version >/dev/null 2>&1; then
    die "the nightly toolchain is missing (libFuzzer needs -Z flags):
       rustup toolchain install nightly"
fi
if ! "$CARGO" +nightly fuzz --version >/dev/null 2>&1; then
    die "cargo-fuzz is missing:
       cargo install cargo-fuzz"
fi

RUN_TS="$(date +%Y%m%d-%H%M%S)"
LOG_DIR="$STATE/logs/$RUN_TS"
mkdir -p "$LOG_DIR" || die "cannot create $LOG_DIR"

# slug for the state layout: leviculum-core/fuzz -> leviculum-core
crate_slug() {
    local dir="$1"
    basename "$(dirname "$(cd "$dir" && pwd)")"
}

total=0 green=0 crashed=0 errored=0
declare -a CRASH_LINES=()

for crate_rel in $CRATES; do
    case "$crate_rel" in
        /*) FUZZ_DIR="$crate_rel" ;;
        *)  FUZZ_DIR="$REPO_DIR/$crate_rel" ;;
    esac
    [ -f "$FUZZ_DIR/Cargo.toml" ] || die "no fuzz crate at $FUZZ_DIR"
    slug="$(crate_slug "$FUZZ_DIR")"

    if ! listed="$("$CARGO" +nightly fuzz list --fuzz-dir "$FUZZ_DIR" 2>&1)"; then
        echo "$listed" >&2
        die "cargo fuzz list failed in $FUZZ_DIR"
    fi

    # A target file that the manifest does not register is fuzzed by nobody --
    # the same failure as #290 one level down, and invisible without this.
    for f in "$FUZZ_DIR"/fuzz_targets/*.rs; do
        [ -e "$f" ] || continue
        name="$(basename "$f" .rs)"
        if ! printf '%s\n' "$listed" | grep -qx -- "$name"; then
            die "$crate_rel/fuzz_targets/$name.rs is not a [[bin]] in $crate_rel/Cargo.toml,
       so no run reaches it"
        fi
    done

    while read -r target; do
        [ -n "$target" ] || continue
        if [ ${#WANTED[@]} -gt 0 ]; then
            found=0
            for w in "${WANTED[@]}"; do [ "$w" = "$target" ] && found=1; done
            [ "$found" = 1 ] || continue
        fi

        total=$((total + 1))
        corpus="$STATE/corpus/$slug/$target"
        findings="$STATE/findings/$slug/$target"
        seeds="$FUZZ_DIR/seeds/$target"
        log="$LOG_DIR/$slug-$target.log"

        if [ "$LIST_ONLY" = 1 ]; then
            echo "FUZZ_TARGET name=$target crate=$slug seeds=$([ -d "$seeds" ] && echo yes || echo no) corpus=$corpus"
            continue
        fi

        mkdir -p "$corpus" "$findings" || die "cannot create $corpus"
        before="$(find "$corpus" -type f | wc -l)"

        # The persistent corpus is the FIRST dir (libFuzzer writes new inputs
        # there); the checked-in seeds follow as read-only input.
        cmd=("$CARGO" +nightly fuzz run --fuzz-dir "$FUZZ_DIR" --target "$HOST_TARGET"
             "$target" "$corpus")
        [ -d "$seeds" ] && cmd+=("$seeds")
        cmd+=(--
             "-max_total_time=$SECONDS_PER_TARGET"
             "-max_len=$MAX_LEN"
             "-rss_limit_mb=$RSS_MB"
             "-artifact_prefix=$findings/")

        echo "[run-fuzz] $slug/$target: ${SECONDS_PER_TARGET}s, corpus $corpus"
        t0="$(date +%s)"
        "${cmd[@]}" > "$log" 2>&1
        rc=$?
        elapsed=$(( $(date +%s) - t0 ))
        after="$(find "$corpus" -type f | wc -l)"

        # cargo-fuzz sets its own -artifact_prefix inside the checkout; ours is
        # passed after it and wins, but a crash file must never depend on that
        # ordering, so anything left in the in-tree artifact dir is moved out.
        intree="$FUZZ_DIR/artifacts/$target"
        if [ -d "$intree" ]; then
            find "$intree" -type f -exec mv -n {} "$findings/" \; 2>/dev/null
        fi

        if [ "$rc" = 0 ]; then
            green=$((green + 1))
            echo "FUZZ_TARGET name=$target crate=$slug status=GREEN secs=$elapsed corpus=$after new=$((after - before)) log=$log"
            continue
        fi

        # Non-zero: a crash libFuzzer kept, or a run that never started.
        artifacts="$(find "$findings" -type f -newermt "@$t0" 2>/dev/null | sort)"
        if [ -n "$artifacts" ]; then
            crashed=$((crashed + 1))
            echo "FUZZ_TARGET name=$target crate=$slug status=CRASH rc=$rc secs=$elapsed corpus=$after log=$log"
            while read -r a; do
                [ -n "$a" ] || continue
                sum="$(sha256sum "$a" | cut -d' ' -f1)"
                CRASH_LINES+=("$slug/$target  $a  sha256=$sum")
                {
                    echo
                    echo "================ CRASH: $slug/$target ================"
                    echo "input:  $a"
                    echo "sha256: $sum"
                    echo "bytes:  $(wc -c < "$a")"
                    echo "reproduce:"
                    echo "  $CARGO +nightly fuzz run --fuzz-dir $FUZZ_DIR --target $HOST_TARGET $target $a"
                    echo "input (first 256 bytes):"
                    head -c 256 "$a" | od -A x -t x1z
                    echo "libFuzzer output (tail):"
                    tail -40 "$log"
                    echo "======================================================"
                } >&2
            done <<< "$artifacts"
        else
            errored=$((errored + 1))
            echo "FUZZ_TARGET name=$target crate=$slug status=ERROR rc=$rc secs=$elapsed log=$log"
            {
                echo
                echo "================ ERROR: $slug/$target did not run (rc=$rc) ================"
                tail -40 "$log"
                echo "=========================================================================="
            } >&2
        fi
    done <<< "$listed"
done

if [ "$LIST_ONLY" = 1 ]; then
    exit 0
fi

if [ "$total" = 0 ]; then
    if [ ${#WANTED[@]} -gt 0 ]; then
        die "no fuzz target is named ${WANTED[*]} -- nothing ran"
    fi
    die "no fuzz target found in: $CRATES -- nothing ran"
fi

echo
echo "FUZZ_SUMMARY targets=$total green=$green crash=$crashed error=$errored secs_per_target=$SECONDS_PER_TARGET state=$STATE logs=$LOG_DIR"

if [ ${#CRASH_LINES[@]} -gt 0 ]; then
    echo "FUZZ CRASHES FOUND -- inputs kept:" >&2
    printf '  %s\n' "${CRASH_LINES[@]}" >&2
fi

[ "$errored" -gt 0 ] && exit 2
[ "$crashed" -gt 0 ] && exit 1
exit 0
