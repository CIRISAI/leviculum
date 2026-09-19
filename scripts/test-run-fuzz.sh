#!/usr/bin/env bash
# Fixture test for scripts/run-fuzz.sh (Codeberg #290).
#
# The runner's whole job is to turn a crash into something nobody can miss, and
# the way that job fails is silently: a fuzz run whose finding is not preserved,
# or whose exit code says "clean" when nothing ran, is worse than no run at all,
# because it buys the confidence without doing the work. So every case here
# INJECTS the failure into a throwaway fuzz crate and asserts what the runner
# concluded from it -- a guardrail nobody has watched fire is not a guardrail.
#
#   1. A CRASHING TARGET. Positive control: the fixture panics on the first
#      non-empty input. Asserts exit 1, a CRASH line naming the target, and the
#      offending input preserved with its hash OUTSIDE the crate.
#   2. A CLEAN TARGET. Counterweight to 1: exit 0 and GREEN, so the crash case
#      is not simply "this script always fails".
#   3. THE CORPUS PERSISTS. libFuzzer's new inputs land in the state dir, not in
#      the crate, and a second run starts from what the first one found. Without
#      this the scheduled run re-explores the same shallow paths every night.
#   4. AN UNREGISTERED TARGET. A .rs in fuzz_targets/ that the manifest does not
#      list is fuzzed by nobody -- #290 one level down. Asserts exit 2.
#   5. A MISSING TOOLCHAIN. The one that must never look green: no cargo-fuzz
#      means exit 2, not a clean run over zero targets.
#
# Needs the nightly toolchain and cargo-fuzz (cases 1-4 build a real fuzz
# target); skips with a named reason if they are absent. ~1 min.
#
# Usage: bash scripts/test-run-fuzz.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RUNNER="$SCRIPT_DIR/run-fuzz.sh"

if ! cargo +nightly fuzz --version >/dev/null 2>&1; then
    echo "SKIP: cargo-fuzz or the nightly toolchain is missing"
    echo "      rustup toolchain install nightly && cargo install cargo-fuzz"
    exit 0
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

FIX="$WORK/fixture-fuzz"
STATE="$WORK/state"
mkdir -p "$FIX/fuzz_targets"

cat > "$FIX/Cargo.toml" <<'EOF'
[package]
name = "leviculum-fixture-fuzz"
version = "0.0.0"
publish = false
edition = "2021"

[package.metadata]
cargo-fuzz = true

[dependencies]
libfuzzer-sys = "0.4"

[workspace]

[[bin]]
name = "fixture_crash"
path = "fuzz_targets/fixture_crash.rs"
test = false
doc = false
bench = false

[[bin]]
name = "fixture_clean"
path = "fuzz_targets/fixture_clean.rs"
test = false
doc = false
bench = false
EOF

cat > "$FIX/fuzz_targets/fixture_crash.rs" <<'EOF'
#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    if !data.is_empty() {
        panic!("fixture crash");
    }
});
EOF

# Branchy on purpose, and every branch observable. libFuzzer writes an input to
# the corpus only when it reaches NEW coverage, so a target whose body the
# optimiser can delete proves nothing about persistence: the first version of
# this fixture computed a value nobody read, was optimised away whole, and sat
# at `cov: 14` for 10.7 million executions without saving a single input.
# black_box keeps each rung of the ladder alive; measured `cov: 27, corp: 9` in
# 5 s.
cat > "$FIX/fuzz_targets/fixture_clean.rs" <<'EOF'
#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    if data.len() > 3 && data[0] == b'l' {
        std::hint::black_box(data[1]);
        if data[1] == b'e' {
            std::hint::black_box(data[2]);
            if data[2] == b'v' {
                std::hint::black_box(data[3]);
            }
        }
    }
});
EOF

FAILED=0
pass() { echo "  PASS: $1"; }
fail() { echo "  FAIL: $1" >&2; FAILED=1; }
# `<test>; assert $? "<what it proves>"` -- the condition runs as its own
# command so a failed assertion cannot be mistaken for the next one.
assert() { if [ "$1" = 0 ]; then pass "$2"; else fail "$2"; fi; }
refute() { if [ "$1" = 0 ]; then fail "$2"; else pass "$2"; fi; }
expect_eq() { if [ "$1" = "$2" ]; then pass "$3"; else fail "$3 -- got '$1', wanted '$2'"; fi; }
expect_ge() { if [ "$1" -ge "$2" ]; then pass "$3"; else fail "$3 -- got '$1', wanted >= '$2'"; fi; }

run_fixture() {
    LEVICULUM_FUZZ_STATE="$STATE" LEVICULUM_FUZZ_CRATES="$FIX" "$@"
}

echo "=== case 1: a crashing target is loud, kept, and exit 1 ==="
OUT="$(run_fixture bash "$RUNNER" --seconds 5 fixture_crash 2>&1)"
RC=$?
expect_eq "$RC" 1 "exit 1 on a crash"
grep -q "name=fixture_crash .*status=CRASH" <<< "$OUT"
assert $? "the summary line names the crashing target"
grep -q "sha256=" <<< "$OUT"
assert $? "the crash report carries the input hash"
KEPT="$(find "$STATE/findings" -type f | wc -l)"
expect_ge "$KEPT" 1 "the crashing input is preserved under the state dir"
LEAKED="$(find "$FIX" -path '*artifacts*' -type f | wc -l)"
expect_eq "$LEAKED" 0 "nothing is left in the crate's artifacts dir, which a fresh clone deletes"
grep -q "FUZZ_SUMMARY targets=1 green=0 crash=1" <<< "$OUT"
assert $? "the summary counts the crash"
[ "$FAILED" = 0 ] || echo "$OUT" >&2

echo "=== case 2: a clean target is green ==="
OUT="$(run_fixture bash "$RUNNER" --seconds 5 fixture_clean 2>&1)"
RC=$?
expect_eq "$RC" 0 "exit 0 on a clean target"
grep -q "name=fixture_clean .*status=GREEN" <<< "$OUT"
assert $? "the summary line reports GREEN"

echo "=== case 3: the corpus persists outside the crate ==="
CORPUS="$(find "$STATE/corpus" -type d -name fixture_clean | head -1)"
FIRST="$(find "$CORPUS" -type f | wc -l)"
expect_ge "$FIRST" 1 "run 1 wrote $FIRST input(s) to the persistent corpus"
run_fixture bash "$RUNNER" --seconds 5 fixture_clean >/dev/null 2>&1
SECOND="$(find "$CORPUS" -type f | wc -l)"
expect_ge "$SECOND" "$FIRST" "run 2 started from run 1's corpus ($FIRST -> $SECOND inputs)"
INSIDE="$(find "$FIX" -type d -name corpus | wc -l)"
expect_eq "$INSIDE" 0 "no corpus inside the crate, so a fresh clone cannot throw it away"

echo "=== case 4: a target the manifest does not register fails the run ==="
cp "$FIX/fuzz_targets/fixture_clean.rs" "$FIX/fuzz_targets/fixture_orphan.rs"
OUT="$(run_fixture bash "$RUNNER" --list 2>&1)"
RC=$?
rm -f "$FIX/fuzz_targets/fixture_orphan.rs"
expect_eq "$RC" 2 "exit 2 on an unregistered target"
grep -q "fixture_orphan.rs is not a \[\[bin\]\]" <<< "$OUT"
assert $? "the error names the file nothing runs"

echo "=== case 5: a missing cargo-fuzz is exit 2, never a green run ==="
STUB="$WORK/stub-bin"
mkdir -p "$STUB"
cat > "$STUB/cargo" <<'EOF'
#!/usr/bin/env bash
# `+nightly --version` succeeds, `fuzz` does not exist: the shape of a host
# that has the toolchain but not cargo-fuzz.
[ "${1:-}" = "+nightly" ] && shift
case "${1:-}" in
    --version) echo "cargo 1.99.0 (stub)"; exit 0 ;;
    fuzz) echo "error: no such command: \`fuzz\`" >&2; exit 101 ;;
esac
exit 101
EOF
chmod +x "$STUB/cargo"
OUT="$(LEVICULUM_FUZZ_CARGO="$STUB/cargo" run_fixture bash "$RUNNER" --seconds 5 2>&1)"
RC=$?
expect_eq "$RC" 2 "exit 2 when cargo-fuzz is missing"
grep -q "cargo install cargo-fuzz" <<< "$OUT"
assert $? "the error says how to fix it"
grep -q "FUZZ_SUMMARY" <<< "$OUT"
refute $? "no summary line for a run that never happened"

echo
if [ "$FAILED" = 0 ]; then
    echo "test-run-fuzz: ALL CASES PASS"
else
    echo "test-run-fuzz: FAILURES ABOVE" >&2
fi
exit "$FAILED"
