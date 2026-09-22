#!/usr/bin/env bash
# A red `rnsd_interop` must not be able to reach the publish step (Codeberg #312).
#
# THE HOLE THIS CLOSES. The forge gate is `just ci-gate` — fmt, clippy and the
# workspace LIB tests. `rnsd_interop` is a `tests/` target, it needs the
# `reference/Reticulum` submodule and a python3, and both forge pipelines clone
# with `submodules: false` on purpose (#300). So whether we still interoperate
# with a Python-RNS peer is measured NOWHERE on the path from a commit to the
# public releases page. The interop truth is not re-derived there — that would
# put a github fetch back into the release path and undo #300 — it is imported:
# the tier-2 nightly runs the whole workspace with submodules and pushes
# `refs/nightly/green/*` at the commit it tested, and `publish` refuses a
# commit no such ref covers.
#
# That mechanism is a CHAIN, and a chain is exactly what rots quietly: every
# link is a line in a file that a later commit can move for a good local
# reason. This asserts all five links, in the tree, as text:
#
#   publish   `.woodpecker/nightly.yml`'s publish step runs
#             `scripts/publish-nightly.sh` — the script the gate lives in.
#   gate      `scripts/publish-nightly.sh` runs
#             `scripts/check-nightly-green.sh`.
#   order     it runs it BEFORE the first forge request, so a refusal cannot
#             land on a half-swapped release.
#   suite     the `complete` recipe runs `cargo test --workspace
#             --all-targets`. This is what puts `rnsd_interop` inside the run
#             the ref stands for; narrowed to `--lib`, the ref would certify a
#             suite with no interop test in it and every other link would
#             still read green.
#   evidence  `scripts/nightly-green-ref.sh` reads `rnsd_interop` out of the
#             run's manifest before it signs anything, so "the night was green"
#             cannot mean "the suite never ran".
#
# WHAT IT DOES NOT DO. It does not assert that the two scripts BEHAVE — that
# is `just nightly-green-selftest` (scripts/test-nightly-green.sh), which
# drives both of them against a fake remote and makes each of the three
# refusals fire. This is the wiring; that is the mechanism. Neither covers the
# other, and the #312 failure mode is available to both: a gate wired into
# nothing, and a gate wired in that says yes to everything.
#
# --selftest is the positive control and it is not optional: each of the five
# links is broken on purpose in a fixture tree and the classifier has to reject
# it. A guard nobody has seen fire is a guard nobody should trust.
#
# A gate rather than a `#[test]` for the reason the whole check-* family is: it
# reads YAML, a Justfile and shell as text, none of which any test binary
# compiles. ~50 ms, no network, no build.
#
# Exit 0 = the chain is intact. Exit 1 = a link is missing, or the checker's
# own self-test failed.
#
# Usage:
#   bash scripts/check-publish-nightly-gate.sh
#   bash scripts/check-publish-nightly-gate.sh --selftest

set -uo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"

# Strip full-line comments before reading anything. Every file in this chain
# DISCUSSES the chain at length — publish-nightly.sh names
# check-nightly-green.sh in a paragraph above the line that runs it, and names
# `curl` in a paragraph about error handling — so a substring match on the raw
# text would find the wiring in the prose and pass a tree where the wiring had
# been deleted. Only what the shell, `just` and Woodpecker execute counts.
# The `- ` of a YAML sequence entry goes first, so a commented-out command in a
# `commands:` list is not mistaken for one.
uncomment() {  # <file>
    sed -e 's/^[ \t]*-[ \t]\{1,\}//' -e 's/^[ \t]*//' "$1" | grep -v '^#'
}

# `publish=<0|1> gate=<0|1> order=<0|1> suite=<0|1> evidence=<0|1>` for the
# tree rooted at $1.
classify() {
    local root="$1"
    local yml="$root/.woodpecker/nightly.yml"
    local pub="$root/scripts/publish-nightly.sh"
    local just="$root/Justfile"
    local ref="$root/scripts/nightly-green-ref.sh"
    local publish=0 gate=0 order=0 suite=0 evidence=0

    if [ -f "$yml" ] && uncomment "$yml" | grep -q 'scripts/publish-nightly\.sh'; then
        publish=1
    fi

    if [ -f "$pub" ]; then
        local body gate_line curl_line
        body="$(uncomment "$pub")"
        gate_line="$(printf '%s\n' "$body" | grep -n 'scripts/check-nightly-green\.sh' | head -1 | cut -d: -f1)"
        # The first thing that speaks to the forge. `api` is this script's own
        # wrapper around curl and is defined below the gate; naming both means
        # a future rewrite that drops the wrapper is still measured.
        curl_line="$(printf '%s\n' "$body" | grep -nE '(^|[ \t(=$])curl[ \t]' | head -1 | cut -d: -f1)"
        if [ -n "$gate_line" ]; then
            gate=1
            if [ -z "$curl_line" ] || [ "$gate_line" -lt "$curl_line" ]; then
                order=1
            fi
        fi
    fi

    # The `complete` recipe's body: the lines from `complete:` at column 0 up
    # to the next column-0 line.
    if [ -f "$just" ]; then
        if sed -n '/^complete:/,/^[^ \t#]/p' "$just" \
             | grep -q 'cargo test --workspace --all-targets'; then
            suite=1
        fi
    fi

    if [ -f "$ref" ] && uncomment "$ref" | grep -q 'rnsd_interop'; then
        evidence=1
    fi

    printf 'publish=%d gate=%d order=%d suite=%d evidence=%d\n' \
        "$publish" "$gate" "$order" "$suite" "$evidence"
}

intact() {
    [ "$(classify "$1")" = "publish=1 gate=1 order=1 suite=1 evidence=1" ]
}

# --- Self-test ------------------------------------------------------------
#
# One fixture tree that must be accepted, and one per link that must be
# rejected. Each break is a real thing somebody would do: move the gate into
# its own pipeline step, drop the call while refactoring, put it after the
# release lookup "so the cheap checks run first", narrow `complete` to `--lib`
# to save four minutes, and sign the night on the caller's word.
selftest() {
    local dir failed=0
    dir="$(mktemp -d)"
    trap 'rm -rf "$dir"' RETURN

    make_tree() {  # <name>
        local t="$dir/$1"
        mkdir -p "$t/.woodpecker" "$t/scripts"
        cat > "$t/.woodpecker/nightly.yml" <<'EOF'
steps:
  publish:
    image: debian:bookworm-slim
    commands:
      - apt-get update && apt-get install -y curl jq git
      - bash scripts/publish-nightly.sh
EOF
        cat > "$t/scripts/publish-nightly.sh" <<'EOF'
#!/usr/bin/env bash
# The release lookup below uses curl; the gate must precede it.
bash "$ROOT/scripts/check-nightly-green.sh" || exit 1
release_json=$(curl -sS "$API/releases/tags/$TAG")
EOF
        cat > "$t/Justfile" <<'EOF'
complete:
    {{manifest}} workspace-all-targets -- cargo test --workspace --all-targets --no-fail-fast

extensive: standard complete
EOF
        cat > "$t/scripts/nightly-green-ref.sh" <<'EOF'
#!/usr/bin/env bash
units = [u for u in m.get("units", []) if "rnsd_interop" in (u.get("selector") or "")]
EOF
        printf '%s' "$t"
    }

    local good; good="$(make_tree good)"
    if ! intact "$good"; then
        echo "check-publish-nightly-gate: SELF-TEST FAILED — the intact fixture was rejected:"
        echo "  $(classify "$good")"
        failed=1
    fi

    # publish: the step runs something else entirely.
    local t; t="$(make_tree bad-publish)"
    sed -i 's|bash scripts/publish-nightly.sh|bash scripts/publish-to-somewhere-else.sh|' \
        "$t/.woodpecker/nightly.yml"

    # gate: the call is gone, and only the paragraph about it remains.
    t="$(make_tree bad-gate)"
    # shellcheck disable=SC2016  # $ROOT is literal here: it is the text of the
    # line being replaced, not a variable this script expands.
    sed -i 's|^bash "\$ROOT/scripts/check-nightly-green.sh" .*|# bash "$ROOT/scripts/check-nightly-green.sh" (temporarily disabled)|' \
        "$t/scripts/publish-nightly.sh"

    # order: the gate runs after the first forge request.
    t="$(make_tree bad-order)"
    cat > "$t/scripts/publish-nightly.sh" <<'EOF'
#!/usr/bin/env bash
release_json=$(curl -sS "$API/releases/tags/$TAG")
bash "$ROOT/scripts/check-nightly-green.sh" || exit 1
EOF

    # suite: `complete` narrowed to the lib tests, which is #312's own shape —
    # every other link intact and the ref certifying a run with no interop
    # test in it.
    t="$(make_tree bad-suite)"
    sed -i 's|cargo test --workspace --all-targets --no-fail-fast|cargo test --workspace --lib|' \
        "$t/Justfile"

    # evidence: the signer takes the caller's verdict on trust.
    t="$(make_tree bad-evidence)"
    cat > "$t/scripts/nightly-green-ref.sh" <<'EOF'
#!/usr/bin/env bash
[ "$VERDICT" = green ] && git push "$REMOTE" "+$COMMIT:$NEW_REF"
EOF

    local fixture
    for fixture in bad-publish bad-gate bad-order bad-suite bad-evidence; do
        if intact "$dir/$fixture"; then
            echo "check-publish-nightly-gate: SELF-TEST FAILED — fixture '$fixture' was accepted:"
            echo "  $(classify "$dir/$fixture")"
            failed=1
        fi
    done

    if [ "$failed" -ne 0 ]; then
        echo "The checker itself is broken; its verdict on the tree means nothing."
        return 1
    fi
    echo "check-publish-nightly-gate: self-test passed (1 intact fixture, 5 broken links caught)"
    return 0
}

if [ "${1:-}" = "--selftest" ]; then
    selftest
    exit $?
fi

# --- The gate -------------------------------------------------------------
if intact "$REPO_DIR"; then
    exit 0
fi

echo "check-publish-nightly-gate: FAILED — a red rnsd_interop can reach publish (Codeberg #312)." >&2
echo "" >&2
echo "  $(classify "$REPO_DIR")" >&2
echo "" >&2
echo "What each link is:" >&2
echo "  publish   .woodpecker/nightly.yml's publish step runs scripts/publish-nightly.sh" >&2
echo "  gate      scripts/publish-nightly.sh runs scripts/check-nightly-green.sh" >&2
echo "  order     ...before the first forge request, so a refusal cannot land on a" >&2
echo "            half-swapped release" >&2
echo "  suite     the Justfile's 'complete' recipe runs" >&2
echo "            'cargo test --workspace --all-targets', which is what puts" >&2
echo "            rnsd_interop inside the run the nightly ref stands for" >&2
echo "  evidence  scripts/nightly-green-ref.sh reads rnsd_interop out of the run's" >&2
echo "            manifest before it signs, so a green verdict cannot mean a suite" >&2
echo "            that never ran" >&2
echo "" >&2
echo "The forge gate is fmt + clippy + the workspace LIB tests; rnsd_interop needs" >&2
echo "the reference/Reticulum submodule and both pipelines clone without submodules" >&2
echo "(Codeberg #300). With a link missing, an interop break reaches the public" >&2
echo "releases page with every pipeline green." >&2
echo "" >&2
echo "See docs/src/development-ci.md, \"What may be published\"." >&2
exit 1
