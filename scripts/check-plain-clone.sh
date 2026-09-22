#!/bin/bash
# No workspace source may read a `reference/` submodule at COMPILE time.
#
# Codeberg #300. `include_str!`/`include_bytes!` are not a runtime lookup a
# test can skip when the file is missing: they are a build dependency of the
# crate that writes them. Three of them had accumulated —
# `lnomad/src/render.rs` and `leviculum-micron/tests/parser_tests.rs` on
# `reference/Reticulum/README.mu`, `lnpnd/src/config.rs` on
# `reference/LXMF/.../lxmd.py` — and the consequence is not "a test is
# skipped" but "the crate does not build": `cargo test --workspace --lib` and
# `cargo clippy --workspace --all-targets` both fail outright on a clone
# without submodules. That is what every forge pipeline here is (`submodules:
# false` in .woodpecker/ci.yml and nightly.yml) and what a contributor gets
# from a plain `git clone`.
#
# Runtime use of `reference/` is untouched and stays legitimate: the
# rnsd_interop suites spawn the vendored Python at RUN time, so a missing
# submodule costs those tests and nothing else. It does cost them: measured
# 2026-09-22, exactly one test in that suite skips itself when the vendored
# RNS is absent (`reverse_rpc_interop_tests.rs:128`, guarded by its own
# `python_rns_available()`); every other test there FAILS, because
# `harness.rs:136` hands a missing `reference/Reticulum` on to
# scripts/test_daemon.py and lets it error out. That is a deliberate
# difference in kind, not a second bug: a failing test is a test that ran and
# said something, while a compile-time `include_str!` into `reference/` takes
# the whole crate — and every test in it — out of the run without a word.
# Only the compile-time form is forbidden here.
#
# What the difference costs the release path is the subject of Codeberg #312:
# because both forge pipelines clone without submodules, rnsd_interop runs in
# neither, and the interop verdict reaches `publish` as a ref pushed by the
# tier-2 nightly instead (scripts/check-nightly-green.sh).
#
# The remedy is always the same: copy the fixture into the crate's
# `tests_data/` and pin it. The test wanted representative input, not that
# particular inode.
#
# A gate rather than a `#[test]` for the reason the whole check-* family is: a
# test inside a crate that does not compile never runs, so the property has to
# be asserted by something that reads source as text.
#
# Exit 0 = no compile-time include reaches into reference/. Exit 1 = one does,
# or the checker's own self-test failed.
#
# Usage:
#   bash scripts/check-plain-clone.sh
set -uo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_DIR" || exit 1

# Print `<file>:<line>: <code>` for every offending include under directory $1.
#
# Full-line comments are stripped first, in all three spellings (`//`, `///`,
# `//!`): a comment that QUOTES the forbidden form — the replacement tests do,
# to say why they no longer use it, and so does this script's own self-test —
# is documentation, not a build dependency. Only what the compiler sees counts.
#
# `reference/` is required to sit at the start of the literal or directly
# after a `/`, so a crate's own `tests_data/reference_vectors.json` is not
# caught by a substring match on the word.
scan() {
    grep -rn --include='*.rs' -E 'include_(str|bytes)!' "$1" 2>/dev/null |
    awk '
    {
        split($0, parts, ":")
        code = $0
        sub(/^[^:]*:[^:]*:[ \t]*/, "", code)
        if (substr(code, 1, 2) == "//") next
        if (code ~ /include_(str|bytes)![ \t]*\([ \t]*"([^"]*\/)?reference\//) {
            printf "%s:%s: %s\n", parts[1], parts[2], code
        }
    }'
}

# --- Self-test ------------------------------------------------------------
#
# "Nothing found" is satisfied forever by a scanner that stopped reading, so
# it runs over fixtures before it is allowed to say anything about the tree:
# the two shapes it must catch, and three near-misses it must not.
SELFTEST_DIR="$(mktemp -d)"
trap 'rm -rf "$SELFTEST_DIR"' EXIT

mkdir -p "$SELFTEST_DIR/bad" "$SELFTEST_DIR/good"

cat > "$SELFTEST_DIR/bad/in_src.rs" <<'EOF'
fn a() {
    let src = include_str!("../../reference/Reticulum/README.mu");
}
EOF

cat > "$SELFTEST_DIR/bad/in_tests.rs" <<'EOF'
const PY: &[u8] = include_bytes!("../../../reference/LXMF/LXMF/Utilities/lxmd.py");
EOF

# A doc comment naming the old spelling, which is exactly what the fix leaves
# behind in the tests it touched.
cat > "$SELFTEST_DIR/good/quoted_in_doc.rs" <<'EOF'
/// Vendored rather than `include_str!("../../reference/Reticulum/README.mu")`,
/// because that is a compile-time dependency (Codeberg #300).
const README: &str = include_str!("../tests_data/reticulum_readme.mu");
EOF

# A crate's own fixture whose NAME carries the word.
cat > "$SELFTEST_DIR/good/word_in_filename.rs" <<'EOF'
const V: &str = include_str!("../tests_data/reference_vectors.json");
EOF

# Runtime use, which stays allowed.
cat > "$SELFTEST_DIR/good/runtime_path.rs" <<'EOF'
fn runtime_is_fine() {
    let p = std::path::Path::new("reference/Reticulum/RNS/Utilities/rnsd.py");
    let _ = std::fs::read_to_string(p);
}
EOF

selftest_failed=0
bad_hits="$(scan "$SELFTEST_DIR/bad")"
for fixture in in_src in_tests; do
    if ! printf '%s' "$bad_hits" | grep -q "$fixture.rs"; then
        echo "check-plain-clone: SELF-TEST FAILED — '$fixture' was not caught." >&2
        selftest_failed=1
    fi
done
good_hits="$(scan "$SELFTEST_DIR/good")"
if [ -n "$good_hits" ]; then
    echo "check-plain-clone: SELF-TEST FAILED — legitimate code was flagged:" >&2
    printf '%s\n' "$good_hits" >&2
    selftest_failed=1
fi
if [ "$selftest_failed" -ne 0 ]; then
    echo "The checker itself is broken; its verdict on the tree means nothing." >&2
    exit 1
fi

# --- The gate -------------------------------------------------------------
#
# Every directory in the tree except the vendored trees themselves and the
# build output: `reference/` and `vendor/` hold nobody's Rust that we build,
# and `target/` holds generated copies of ours.
hits=""
for dir in */; do
    case "$dir" in
        reference/|vendor/|target/) continue ;;
    esac
    found="$(scan "$dir")"
    if [ -n "$found" ]; then
        hits="${hits}${found}
"
    fi
done

if [ -z "$hits" ]; then
    exit 0
fi

echo "check-plain-clone: FAILED — a compile-time include reads a submodule (Codeberg #300)." >&2
echo "" >&2
printf '%s' "$hits" >&2
echo "" >&2
echo "An include_str!/include_bytes! into reference/ is a BUILD dependency, not" >&2
echo "a runtime one: with the submodule absent the crate does not compile, so" >&2
echo "every test it holds is lost rather than skipped. Both forge pipelines" >&2
echo "clone with submodules: false, and so does a contributor's plain clone." >&2
echo "" >&2
echo "Fix: copy the fixture into the crate's tests_data/ and include that," >&2
echo "naming in the test where it came from and at which submodule pin." >&2
exit 1
