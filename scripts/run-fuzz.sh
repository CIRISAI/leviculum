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
#   2  could not run: missing toolchain, missing crate, unregistered target,
#      or a fuzz crate whose committed Cargo.lock no longer matches its graph
#
# Two modes, because fuzzing and regression-checking are different jobs:
#
#   FUZZ (default) explores. It costs a wall budget per target and its verdict
#   is about inputs nobody has written down yet.
#   REGRESS (--regress) explores nothing. It replays the corpus and the
#   checked-in seeds through each target exactly once (-runs=0) and asserts
#   they are all still handled. That is the check that the three defects the
#   issue names stay fixed -- #263's nesting chain, #267's wrapping bin32
#   length and #271's oversized frame all have a named seed under
#   <crate>/fuzz/seeds/<target>/ -- and it is fast enough for the push path.
#
# Usage:
#   bash scripts/run-fuzz.sh                    # every target, 60 s each
#   bash scripts/run-fuzz.sh --seconds 900      # the nightly budget
#   bash scripts/run-fuzz.sh --nightly          # FUZZ_SECS (120) per target
#   bash scripts/run-fuzz.sh --regress          # replay the corpus, no fuzzing
#   bash scripts/run-fuzz.sh hdlc_deframe       # one target by name
#   bash scripts/run-fuzz.sh --list             # what would run, no build
#
# Toolchain: cargo-fuzz drives libFuzzer through `-Z` sanitizer flags, so the
# targets need NIGHTLY -- the repo's pinned 1.97.1 stable (rust-toolchain.toml)
# cannot build them, which is why every invocation below is `cargo +nightly`.
# The channel is a knob (LEVICULUM_FUZZ_TOOLCHAIN) so a date-pinned nightly can
# replace the rolling one without editing this file; scripts/install-ci.sh
# names the version these targets were last verified against. The resolved
# version is printed as FUZZ_TOOLCHAIN on every run, so a drift is visible in
# the log rather than inferred from a build failure.
#
# Environment:
#   LEVICULUM_FUZZ_SECONDS   per-target wall budget            (default 60)
#   FUZZ_SECS                per-target wall budget in --nightly (default 120)
#   LEVICULUM_FUZZ_STATE     persistent corpus/findings root
#                            (default ~/.local/state/leviculum-fuzz)
#   LEVICULUM_FUZZ_CORPUS    corpus root, <root>/<crate>/<target>/
#                            (default $LEVICULUM_FUZZ_STATE/corpus)
#   LEVICULUM_FUZZ_ARTIFACTS crash-input root, <root>/<crate>/<target>/
#                            (default $LEVICULUM_FUZZ_STATE/findings)
#   LEVICULUM_FUZZ_CRATES    space-separated fuzz crate dirs, repo-relative or
#                            absolute (default the two in this repo)
#   LEVICULUM_FUZZ_MAX_LEN   libFuzzer -max_len                (default 8192)
#   LEVICULUM_FUZZ_RSS_MB    libFuzzer -rss_limit_mb           (default 2048)
#   LEVICULUM_FUZZ_TIMEOUT   libFuzzer -timeout, seconds per input (default 25)
#   LEVICULUM_FUZZ_TOOLCHAIN rustup channel to build with    (default nightly)
#   LEVICULUM_FUZZ_CARGO     cargo binary to drive             (default cargo)

set -uo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"

STATE="${LEVICULUM_FUZZ_STATE:-$HOME/.local/state/leviculum-fuzz}"
CRATES="${LEVICULUM_FUZZ_CRATES:-leviculum-core/fuzz leviculum-std/fuzz}"
MAX_LEN="${LEVICULUM_FUZZ_MAX_LEN:-8192}"
RSS_MB="${LEVICULUM_FUZZ_RSS_MB:-2048}"
# Per-INPUT limit, not per-run: without it libFuzzer waits 1200 s for one
# hanging input, which eats a 120 s budget twenty times over and reports the
# hang as nothing at all. A parser that takes 25 s on 8 KiB is a finding.
TIMEOUT="${LEVICULUM_FUZZ_TIMEOUT:-25}"
TOOLCHAIN="${LEVICULUM_FUZZ_TOOLCHAIN:-nightly}"
CARGO="${LEVICULUM_FUZZ_CARGO:-cargo}"
# ASan wants glibc; the workspace default target is musl (.cargo/config.toml),
# so this is passed explicitly rather than left to cargo-fuzz's default.
HOST_TARGET="x86_64-unknown-linux-gnu"

LIST_ONLY=0
MODE=fuzz
SKIP_IF_UNAVAILABLE=0
SECONDS_SET=""
WANTED=()

while [ $# -gt 0 ]; do
    case "$1" in
        --seconds) SECONDS_SET="${2:?--seconds needs a value}"; shift 2 ;;
        --seconds=*) SECONDS_SET="${1#*=}"; shift ;;
        --regress) MODE=regress; shift ;;
        --skip-if-unavailable) SKIP_IF_UNAVAILABLE=1; shift ;;
        --nightly) MODE=nightly; shift ;;
        --list) LIST_ONLY=1; shift ;;
        -h|--help) sed -n '2,75p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        -*) echo "ERROR: unknown flag '$1'" >&2; exit 2 ;;
        *) WANTED+=("$1"); shift ;;
    esac
done

die() { echo "[run-fuzz] ERROR: $*" >&2; exit 2; }

# For the push path only: a host without the nightly toolchain must not fail
# `just fast`, and must not go quiet either. One named line, exit 0, and the
# caller that asked for it is the only one that gets it -- a scheduled run
# still takes exit 2 for the same condition, because there the toolchain being
# gone IS the finding.
skip_or_die() {
    if [ "$SKIP_IF_UNAVAILABLE" = 1 ]; then
        echo "FUZZ_SKIPPED mode=$MODE reason=\"$1\""
        exit 0
    fi
    die "$2"
}

# The budget default follows the mode: an interactive `just fuzz` is 60 s, the
# scheduled run is FUZZ_SECS (120 s). An explicit --seconds beats both.
case "$MODE" in
    nightly) SECONDS_PER_TARGET="${LEVICULUM_FUZZ_SECONDS:-${FUZZ_SECS:-120}}" ;;
    *)       SECONDS_PER_TARGET="${LEVICULUM_FUZZ_SECONDS:-60}" ;;
esac
[ -n "$SECONDS_SET" ] && SECONDS_PER_TARGET="$SECONDS_SET"

case "$SECONDS_PER_TARGET" in
    ''|*[!0-9]*) die "--seconds wants a whole number of seconds, got '$SECONDS_PER_TARGET'" ;;
esac

# Corpus and crash inputs: one root each, defaulting under $LEVICULUM_FUZZ_STATE
# and overridable per run. They default to the SAME place in every mode on
# purpose -- a scheduled run and a hand-driven one that keep separate corpora
# accumulate two half-explored input sets that never meet.
CORPUS_ROOT="${LEVICULUM_FUZZ_CORPUS:-$STATE/corpus}"
ARTIFACT_ROOT="${LEVICULUM_FUZZ_ARTIFACTS:-$STATE/findings}"

# Preconditions, each with the command that fixes it. A missing toolchain is
# exit 2, never a green run over zero targets.
command -v "$CARGO" >/dev/null 2>&1 || skip_or_die "no '$CARGO' on PATH" "no '$CARGO' on PATH"
if ! TOOLCHAIN_VERSION="$("$CARGO" "+$TOOLCHAIN" --version 2>/dev/null)"; then
    skip_or_die "the '$TOOLCHAIN' toolchain is missing: rustup toolchain install $TOOLCHAIN" \
        "the '$TOOLCHAIN' toolchain is missing (libFuzzer needs -Z flags, which
       the pinned stable in rust-toolchain.toml does not have):
       rustup toolchain install $TOOLCHAIN"
fi
if ! FUZZ_VERSION="$("$CARGO" "+$TOOLCHAIN" fuzz --version 2>/dev/null)"; then
    skip_or_die "cargo-fuzz is missing: cargo install cargo-fuzz" \
        "cargo-fuzz is missing:
       cargo install cargo-fuzz"
fi
# Said, not assumed: which compiler produced these binaries is the first thing
# anyone asks about a sanitizer finding, and a rolling nightly changes under a
# corpus that outlives it.
echo "FUZZ_TOOLCHAIN channel=$TOOLCHAIN version=\"$TOOLCHAIN_VERSION\" cargo_fuzz=\"$FUZZ_VERSION\" mode=$MODE"

RUN_TS="$(date +%Y%m%d-%H%M%S)"
LOG_DIR="$STATE/logs/$RUN_TS"
mkdir -p "$LOG_DIR" || die "cannot create $LOG_DIR"

# slug for the state layout: leviculum-core/fuzz -> leviculum-core
crate_slug() {
    local dir="$1"
    basename "$(dirname "$(cd "$dir" && pwd)")"
}

# A BUILD MUST NOT REWRITE A TRACKED FILE (Codeberg #295).
#
# Each fuzz crate carries its own committed Cargo.lock, and it resolves the
# same path graph the workspace does -- so a dependency added anywhere under
# it makes the fuzz lock stale, and the next build silently rewrites it. That
# is how `just fast` -> `fuzz-regress` left three lines of dirt in the gate's
# push tree on 80c11aae: the following gate refused the tree at
# gate-run.sh:51 with rc=5 and landing stopped, with nothing in either log
# saying which run had written the file.
#
# So the lock is a PRECONDITION here, not an output. cargo-fuzz 0.13 has no
# `--locked` of its own and passes trailing arguments to libFuzzer, so the
# assertion is made with the resolver directly: `cargo metadata --locked`
# re-resolves the graph and refuses to write, which is the same question the
# build would have answered by editing the file. It costs ~0.2 s per crate
# and it fails as exit 2 -- a run that could not happen, never a green one.
#
# Deliberately in every mode that builds, not only on the push path: the
# scheduled `fuzz-nightly` runs in a fresh clone whose dirt nobody reads, and
# an interactive `just fuzz` writes into somebody's working tree, where the
# file is tracked just the same. `--list` builds nothing and is exempt.
# No --offline: a fresh clone has no registry cache, and the build that
# follows needs those manifests anyway.
assert_lock_current() {
    local dir="$1" rel="$2" out
    [ -f "$dir/Cargo.lock" ] || return 0
    if ! out="$("$CARGO" "+$TOOLCHAIN" metadata --locked --format-version 1 \
                --manifest-path "$dir/Cargo.toml" 2>&1 >/dev/null)"; then
        echo "$out" >&2
        die "$rel/Cargo.lock does not match $rel's dependency graph, and a build
       would rewrite it. Regenerate it minimally and commit the diff:
         $CARGO +$TOOLCHAIN metadata --manifest-path $rel/Cargo.toml \\
             --format-version 1 --offline >/dev/null"
    fi
}

# The per-target status lines carry the mode in their KEY, not in a field: a
# regress line and a fuzz line report different things (inputs replayed vs
# inputs explored) and a report that greps one must not catch the other.
LINE=FUZZ_TARGET
[ "$MODE" = regress ] && LINE=FUZZ_REGRESS

total=0 green=0 crashed=0 errored=0
declare -a CRASH_LINES=()

for crate_rel in $CRATES; do
    case "$crate_rel" in
        /*) FUZZ_DIR="$crate_rel" ;;
        *)  FUZZ_DIR="$REPO_DIR/$crate_rel" ;;
    esac
    [ -f "$FUZZ_DIR/Cargo.toml" ] || die "no fuzz crate at $FUZZ_DIR"
    slug="$(crate_slug "$FUZZ_DIR")"

    [ "$LIST_ONLY" = 1 ] || assert_lock_current "$FUZZ_DIR" "$crate_rel"

    if ! listed="$("$CARGO" "+$TOOLCHAIN" fuzz list --fuzz-dir "$FUZZ_DIR" 2>&1)"; then
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
        corpus="$CORPUS_ROOT/$slug/$target"
        findings="$ARTIFACT_ROOT/$slug/$target"
        seeds="$FUZZ_DIR/seeds/$target"
        log="$LOG_DIR/$slug-$target.log"

        if [ "$LIST_ONLY" = 1 ]; then
            echo "$LINE name=$target crate=$slug seeds=$([ -d "$seeds" ] && echo yes || echo no) corpus=$corpus"
            continue
        fi

        mkdir -p "$corpus" "$findings" || die "cannot create $corpus"
        before="$(find "$corpus" -type f | wc -l)"

        # The persistent corpus is the FIRST dir (libFuzzer writes new inputs
        # there); the checked-in seeds follow as read-only input.
        cmd=("$CARGO" "+$TOOLCHAIN" fuzz run --fuzz-dir "$FUZZ_DIR" --target "$HOST_TARGET"
             "$target" "$corpus")
        [ -d "$seeds" ] && cmd+=("$seeds")

        if [ "$MODE" = regress ]; then
            # Replay only. -runs=0 executes every loaded input once and exits;
            # nothing is generated, nothing is written to the corpus.
            inputs=0 biggest=0
            for d in "$corpus" "$seeds"; do
                [ -d "$d" ] || continue
                while read -r sz; do
                    [ -n "$sz" ] || continue
                    inputs=$((inputs + 1))
                    [ "$sz" -gt "$biggest" ] && biggest="$sz"
                done < <(find "$d" -type f -printf '%s\n' 2>/dev/null)
            done
            # A target with nothing to replay is a check that checks nothing.
            # It is the #290 failure in miniature, so it is an ERROR, not a
            # green run over an empty corpus.
            if [ "$inputs" = 0 ]; then
                errored=$((errored + 1))
                echo "$LINE name=$target crate=$slug status=ERROR inputs=0 -- neither $corpus nor $seeds holds an input to replay" >&2
                continue
            fi
            # -max_len is a TRUNCATION, not a filter: libFuzzer loads a larger
            # corpus file and silently cuts it to the limit (measured against
            # the 100004-byte #263 reproducer at lim: 8192, 2026-09-25), which
            # replays something other than the input that used to crash. The
            # limit therefore follows the corpus in this mode.
            replay_len="$MAX_LEN"
            [ "$biggest" -gt "$replay_len" ] && replay_len="$biggest"
            cmd+=(--
                 "-runs=0"
                 "-max_len=$replay_len"
                 "-rss_limit_mb=$RSS_MB"
                 "-timeout=$TIMEOUT"
                 "-artifact_prefix=$findings/")
            echo "[run-fuzz] $slug/$target: replaying $inputs input(s), max $biggest B"
        else
            cmd+=(--
                 "-max_total_time=$SECONDS_PER_TARGET"
                 "-max_len=$MAX_LEN"
                 "-rss_limit_mb=$RSS_MB"
                 "-timeout=$TIMEOUT"
                 "-artifact_prefix=$findings/")
            echo "[run-fuzz] $slug/$target: ${SECONDS_PER_TARGET}s, corpus $corpus"
        fi
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
            if [ "$MODE" = regress ]; then
                echo "$LINE name=$target crate=$slug status=GREEN secs=$elapsed inputs=$inputs bytes_max=$biggest log=$log"
            else
                echo "$LINE name=$target crate=$slug status=GREEN secs=$elapsed corpus=$after new=$((after - before)) log=$log"
            fi
            continue
        fi

        # Non-zero: a crash libFuzzer kept, or a run that never started.
        artifacts="$(find "$findings" -type f -newermt "@$t0" 2>/dev/null | sort)"
        if [ -n "$artifacts" ]; then
            crashed=$((crashed + 1))
            echo "$LINE name=$target crate=$slug status=CRASH rc=$rc secs=$elapsed corpus=$after log=$log"
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
                    echo "  $CARGO +$TOOLCHAIN fuzz run --fuzz-dir $FUZZ_DIR --target $HOST_TARGET $target $a"
                    echo "input (first 256 bytes):"
                    head -c 256 "$a" | od -A x -t x1z
                    echo "libFuzzer output (tail):"
                    tail -40 "$log"
                    echo "======================================================"
                } >&2
            done <<< "$artifacts"
        else
            errored=$((errored + 1))
            echo "$LINE name=$target crate=$slug status=ERROR rc=$rc secs=$elapsed log=$log"
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
# The budget is a fuzz-mode fact. Printing it in regress mode would state a
# number no target in that run was given, which is the kind of key a report
# generator later averages.
budget="$SECONDS_PER_TARGET"
[ "$MODE" = regress ] && budget="n/a"
echo "FUZZ_SUMMARY mode=$MODE targets=$total green=$green crash=$crashed error=$errored secs_per_target=$budget corpus=$CORPUS_ROOT artifacts=$ARTIFACT_ROOT logs=$LOG_DIR"

if [ ${#CRASH_LINES[@]} -gt 0 ]; then
    echo "FUZZ CRASHES FOUND -- inputs kept:" >&2
    printf '  %s\n' "${CRASH_LINES[@]}" >&2
fi

[ "$errored" -gt 0 ] && exit 2
[ "$crashed" -gt 0 ] && exit 1
exit 0
