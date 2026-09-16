#!/bin/bash
# One pipeline on the forge must gate every push, with no path filter.
#
# Codeberg #299. The repository publishes .debs to strangers, and until this
# guard existed the only thing between an ordinary commit — Rust source, no
# packaging file — and the public releases page was `.githooks/pre-push`.
# That hook is good and it is not a gate: it is per-clone local config a fresh
# clone does not have, `--no-verify` skips it, and it runs on the developer's
# toolchain rather than the pipeline's (#298 is what that difference costs).
# `.woodpecker/nightly.yml` does run `just ci-gate` (#266), but its push
# trigger is filtered to the packaging paths, so an ordinary commit reaches
# the forge with no test having run there at all.
#
# So the property this asserts is exactly the one that was missing: SOME
# workflow in `.woodpecker/` runs the CI gate, is triggered by `push`, and
# carries no `path:` filter that could exclude a commit from it. Which file
# does it is deliberately not pinned — a rename is not a regression.
#
# What it does NOT check, on purpose:
#   * `branch:` filters — only `master` and tags reach this forge at all
#     (.githooks/pre-push refuses the rest), so a branch filter cannot create
#     the ungated-commit hole this is about.
#   * that the gate passes. That is the pipeline's job; this is the check
#     that the pipeline is asked to run in the first place.
#
# A gate rather than a `#[test]` for the reason the whole check-* family is:
# it reads files no test binary compiles, and it must run on the push path
# where the author who just edited the YAML is still there to fix it.
#
# Exit 0 = a qualifying always-on pipeline exists. Exit 1 = none does, or the
# checker's own self-test failed.
#
# Usage:
#   bash scripts/check-ci-pipeline.sh
set -uo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_DIR" || exit 1

# Print `push=<0|1> gate=<0|1> path=<0|1>` for the pipeline file $1.
#
#   push  — the workflow-level `when:` block (a `when:` key at column 0, up to
#           the next column-0 key) names the `push` event. Workflow-level and
#           not per-step: a push trigger that only reaches some steps leaves
#           the question "is the gate among them" to a YAML parser this guard
#           deliberately is not, so it is reported as no trigger at all.
#   gate   — a command line runs the gate, in either of the two spellings the
#           tree uses: `just ci-gate` directly, or the provisioning wrapper
#           `scripts/ci-gate.sh` that ends in it.
#   path   — a `path:` key appears anywhere in the file. In Woodpecker that
#            key exists only inside a `when:` filter, so its presence in the
#            file that carries the always-on gate is the disqualifier.
#
# Full-line comments are stripped first, including the `- ` of a YAML sequence
# entry: a commented-out gate line runs nothing, and a comment that discusses
# path filters (this file's own neighbours do) is not one.
classify() {
    awk '
    {
        head = $0
        sub(/^[ \t]*(-[ \t]+)?/, "", head)
        if (substr(head, 1, 1) == "#") next
        if ($0 ~ /^[A-Za-z_][A-Za-z0-9_.-]*:/) toplevel_when = ($0 ~ /^when:[ \t]*(#.*)?$/)
        if (toplevel_when && $0 ~ /event:/ && $0 ~ /(^|[^A-Za-z_-])push([^A-Za-z_-]|$)/) push = 1
        if ($0 ~ /^[ \t]*(-[ \t]+)?path:/) path = 1
        if ($0 ~ /(^|[ \t])just[ \t]+ci-gate([ \t]|$)/) gate = 1
        if ($0 ~ /(^|[ \t])scripts\/ci-gate\.sh([ \t]|$)/) gate = 1
    }
    END { printf "push=%d gate=%d path=%d\n", push, gate, path }
    ' "$1"
}

# `always-on` is the one verdict that matters; the rest are the reasons.
qualifies() {
    [ "$(classify "$1")" = "push=1 gate=1 path=0" ]
}

# --- Self-test ------------------------------------------------------------
#
# "No qualifying file found" and "every file qualifies" are both satisfied
# forever by a classifier that stopped reading, so it is run over fixtures
# before it is allowed to say anything about the tree: one shape it must
# accept and five near-misses it must reject, each of which is a real way
# this property has been or could be lost.
SELFTEST_DIR="$(mktemp -d)"
trap 'rm -rf "$SELFTEST_DIR"' EXIT

cat > "$SELFTEST_DIR/good.yml" <<'EOF'
when:
  - event: [push, pull_request, manual]
steps:
  gate:
    image: rust:bookworm
    commands:
      - bash scripts/ci-gate.sh
EOF

# The #299 shape itself: a gate that only fires when packaging files change.
cat > "$SELFTEST_DIR/bad-path-filter.yml" <<'EOF'
when:
  - event: cron
  - event: push
    path:
      include:
        - 'packaging/**'
steps:
  gate:
    commands:
      - just ci-gate
EOF

cat > "$SELFTEST_DIR/bad-cron-only.yml" <<'EOF'
when:
  - event: cron
steps:
  gate:
    commands:
      - just ci-gate
EOF

cat > "$SELFTEST_DIR/bad-no-gate.yml" <<'EOF'
when:
  - event: [push, manual]
steps:
  lint:
    commands:
      - bash scripts/check-commit-trailers.sh
EOF

cat > "$SELFTEST_DIR/bad-commented-out.yml" <<'EOF'
when:
  - event: [push, manual]
steps:
  gate:
    commands:
      # - just ci-gate   (disabled while the runner is slow)
      - echo skipped
EOF

# A push trigger that belongs to one step rather than to the workflow: the
# gate step next to it may well be cron-only, and this guard does not parse
# far enough to tell.
cat > "$SELFTEST_DIR/bad-step-level-when.yml" <<'EOF'
when:
  - event: cron
steps:
  notify:
    when:
      - event: push
    commands:
      - echo hello
  gate:
    commands:
      - just ci-gate
EOF

selftest_failed=0
if ! qualifies "$SELFTEST_DIR/good.yml"; then
    echo "check-ci-pipeline: SELF-TEST FAILED — the always-on fixture was rejected:"
    echo "  $(classify "$SELFTEST_DIR/good.yml")"
    selftest_failed=1
fi
for fixture in bad-path-filter bad-cron-only bad-no-gate bad-commented-out bad-step-level-when; do
    if qualifies "$SELFTEST_DIR/$fixture.yml"; then
        echo "check-ci-pipeline: SELF-TEST FAILED — fixture '$fixture' was accepted."
        selftest_failed=1
    fi
done
if [ "$selftest_failed" -ne 0 ]; then
    echo "The checker itself is broken; its verdict on the tree means nothing."
    exit 1
fi

# --- The gate -------------------------------------------------------------

# Both layouts Woodpecker accepts: the `.woodpecker/` directory this tree
# uses, and the single `.woodpecker.yml` at the root (periculum's shape), so
# a repository that collapses to one file is still covered. A literal name is
# not a glob, so `nullglob` does not drop it — each candidate is tested.
PIPELINES=()
shopt -s nullglob
for candidate in .woodpecker/*.yml .woodpecker/*.yaml .woodpecker.yml .woodpecker.yaml; do
    [ -f "$candidate" ] && PIPELINES+=("$candidate")
done
shopt -u nullglob

if [ "${#PIPELINES[@]}" -eq 0 ]; then
    echo "check-ci-pipeline: FAILED — no Woodpecker pipeline files found."
    echo "This repository publishes to a public forge; it runs a forge gate."
    exit 1
fi

for f in "${PIPELINES[@]}"; do
    if qualifies "$f"; then
        exit 0
    fi
done

echo "check-ci-pipeline: FAILED — no always-on CI pipeline (Codeberg #299)."
echo ""
echo "No file in .woodpecker/ runs the CI gate on every push. Ordinary commits"
echo "therefore reach the forge — and the public releases page — without a"
echo "single test having run there. The pre-push hook does not close this: a"
echo "fresh clone has no hooks and --no-verify skips the ones it has."
echo ""
echo "What each pipeline file is missing (push=1 gate=1 path=0 qualifies):"
for f in "${PIPELINES[@]}"; do
    printf '  %-40s %s\n' "$f" "$(classify "$f")"
done
echo ""
echo "Fix: a workflow with a workflow-level 'when: - event: [push, ...]', no"
echo "'path:' filter, and a step that runs 'bash scripts/ci-gate.sh'."
exit 1
