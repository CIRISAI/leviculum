#!/bin/bash
# Every `from_secret:` in `.woodpecker/**` must name a secret that exists.
#
# A missing Woodpecker secret is not a failed step. It is a compile error for
# the whole pipeline: the server refuses to build any workflow at all and the
# run ends `status: error, workflows: 0` with one line naming the secret. The
# steps that had nothing to do with it never start.
#
# That is not a hypothetical. Cron run #434 on commit d7979113 ended
# `secret "site_ssh_target" not found`; the gate, both .deb builds, the
# packaging step and the FORGE publish — the one every download URL we have
# written down points at — never ran, for three weeks, because a step added
# last and running last named three secrets nobody had created yet.
#
# Nothing we can run locally sees that: it happens before the first container
# starts, so there is no log, no step, and no test in this tree whose subject
# it is. What CAN be checked locally is the half that is a statement about
# the repository rather than about the server — that every secret a pipeline
# names was written down deliberately, in `scripts/ci-secrets.txt`, by
# somebody who had just created it.
#
# So this gate holds one property: the set of secret names referenced under
# `.woodpecker/` is a subset of the names in that file. It cannot know what
# the server really holds (that needs an API token and a network call, and a
# gate that needs a token is a gate contributors cannot run). It turns a
# silent pipeline-level error into a push-time refusal naming the file, the
# line and the name — which is the difference between finding this in the
# commit that caused it and finding it a night later, or not at all.
#
# It also refuses the deprecated step-level `secrets:` list outright: that
# spelling carries the same compile-time requirement, is not what this tree
# uses, and would slip past a gate that only reads `from_secret:`.
#
# A gate rather than a `#[test]`, for the reason the whole check-* family is:
# it reads files no test binary compiles, and it belongs on the push path,
# where the author who just edited the YAML is still there to fix it.
#
# Exit 0 = every referenced secret is accounted for. Exit 1 = one is not, or
# the checker's own self-test failed.
#
# Usage:
#   bash scripts/check-ci-secrets.sh
set -uo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_DIR" || exit 1

ALLOWLIST="${LEV_CI_SECRETS_FILE:-$REPO_DIR/scripts/ci-secrets.txt}"

# Print one `<line>:<name>` per secret reference in the pipeline file $1, and
# `<line>:!legacy-secrets-key` for each deprecated `secrets:` list key.
#
# Full-line comments are stripped first, including the `- ` of a YAML
# sequence entry: this file's own neighbours discuss `from_secret` in prose,
# and the publish-site step carries a commented-out block showing the exact
# lines to add once the secrets exist. A commented reference resolves
# nothing, so it must not count as one — and must not be a way to smuggle one
# past this gate either, because a commented line is not a reference at all.
#
# Quotes are stripped from the name: `from_secret: "x"` and `from_secret: x`
# are the same reference to Woodpecker, so they must be the same reference
# here. Case is folded: secret names are lowercase on the server.
refs() {
    awk '
    {
        head = $0
        sub(/^[ \t]*(-[ \t]+)?/, "", head)
        if (substr(head, 1, 1) == "#") next
        if (match($0, /from_secret:[ \t]*["'"'"']?[A-Za-z0-9_.-]+/)) {
            name = substr($0, RSTART, RLENGTH)
            sub(/^from_secret:[ \t]*["'"'"']?/, "", name)
            printf "%d:%s\n", NR, tolower(name)
        }
        if ($0 ~ /^[ \t]*(-[ \t]+)?secrets:/) printf "%d:!legacy-secrets-key\n", NR
    }
    ' "$1"
}

# Every name in the allowlist file, lowercased, comments and blanks dropped.
allowed_names() { # <allowlist-file>
    sed -e 's/#.*//' -e 's/[[:space:]]//g' "$1" | grep -v '^$' | tr '[:upper:]' '[:lower:]'
}

# Print the offending `<file>:<line>:<name>` lines of pipeline file $1 against
# allowlist $2. No output = nothing wrong.
offenders() { # <pipeline-file> <allowlist-file>
    local allowed ref line name
    allowed="$(allowed_names "$2")"
    while IFS= read -r ref; do
        [ -n "$ref" ] || continue
        line="${ref%%:*}"
        name="${ref#*:}"
        if [ "$name" = '!legacy-secrets-key' ]; then
            printf '%s:%s:%s\n' "$1" "$line" "$name"
        elif ! printf '%s\n' "$allowed" | grep -qxF -- "$name"; then
            printf '%s:%s:%s\n' "$1" "$line" "$name"
        fi
    done < <(refs "$1")
}

# --- Self-test ------------------------------------------------------------
#
# "Nothing is wrong anywhere" is satisfied forever by a checker that stopped
# reading, so it is run over fixtures before it is allowed to say anything
# about the tree: one shape it must accept and four it must reject, each of
# which is a real way a secret reference has been or could be lost.
SELFTEST_DIR="$(mktemp -d)"
trap 'rm -rf "$SELFTEST_DIR"' EXIT

cat > "$SELFTEST_DIR/allow.txt" <<'EOF'
# a fixture allowlist
codeberg_token
EOF

# The shape the tree has: one declared secret, and prose plus a commented-out
# block naming three that do not exist yet.
cat > "$SELFTEST_DIR/good.yml" <<'EOF'
steps:
  publish:
    environment:
      CODEBERG_TOKEN:
        from_secret: codeberg_token
    commands:
      - bash scripts/publish-nightly.sh
  # publish-site, once the secrets exist:
  #    environment:
  #      SITE_SSH_TARGET:
  #        from_secret: site_ssh_target
  publish-site:
    commands:
      - bash scripts/publish-site.sh
EOF

# #434 itself.
cat > "$SELFTEST_DIR/bad-unlisted.yml" <<'EOF'
steps:
  publish-site:
    environment:
      SITE_SSH_TARGET:
        from_secret: site_ssh_target
EOF

# The same reference wearing quotes.
cat > "$SELFTEST_DIR/bad-quoted.yml" <<'EOF'
steps:
  publish-site:
    environment:
      SITE_SSH_KEY: { from_secret: "site_ssh_key" }
EOF

# Case is the server's, not the author's: an unlisted name in capitals is
# still unlisted.
cat > "$SELFTEST_DIR/bad-uppercase.yml" <<'EOF'
steps:
  publish-site:
    environment:
      SITE_SSH_HOST_KEY:
        from_secret: SITE_SSH_HOST_KEY
EOF

# The deprecated spelling, which carries the same requirement and would
# otherwise be an unaudited way to name a secret.
cat > "$SELFTEST_DIR/bad-legacy.yml" <<'EOF'
steps:
  publish:
    secrets: [codeberg_token]
EOF

selftest_failed=0
if [ -n "$(offenders "$SELFTEST_DIR/good.yml" "$SELFTEST_DIR/allow.txt")" ]; then
    echo "check-ci-secrets: SELF-TEST FAILED — the good fixture was rejected:"
    offenders "$SELFTEST_DIR/good.yml" "$SELFTEST_DIR/allow.txt" | sed 's/^/  /'
    selftest_failed=1
fi
for fixture in bad-unlisted bad-quoted bad-uppercase bad-legacy; do
    if [ -z "$(offenders "$SELFTEST_DIR/$fixture.yml" "$SELFTEST_DIR/allow.txt")" ]; then
        echo "check-ci-secrets: SELF-TEST FAILED — fixture '$fixture' was accepted."
        selftest_failed=1
    fi
done
if [ "$selftest_failed" -ne 0 ]; then
    echo "The checker itself is broken; its verdict on the tree means nothing."
    exit 1
fi

# --- The gate -------------------------------------------------------------

if [ ! -r "$ALLOWLIST" ]; then
    echo "check-ci-secrets: FAILED — no secret allowlist at $ALLOWLIST."
    echo "Every secret the pipelines name is supposed to be written down there."
    exit 1
fi

PIPELINES=()
shopt -s nullglob
for candidate in .woodpecker/*.yml .woodpecker/*.yaml .woodpecker.yml .woodpecker.yaml; do
    [ -f "$candidate" ] && PIPELINES+=("$candidate")
done
shopt -u nullglob

if [ "${#PIPELINES[@]}" -eq 0 ]; then
    # Nothing to check, and check-ci-pipeline.sh is the gate that says
    # whether that is allowed. Not this one's verdict to give.
    exit 0
fi

FOUND=""
for f in "${PIPELINES[@]}"; do
    out="$(offenders "$f" "$ALLOWLIST")"
    [ -n "$out" ] && FOUND="${FOUND}${out}"$'\n'
done

if [ -z "$FOUND" ]; then
    exit 0
fi

echo "check-ci-secrets: FAILED — a pipeline names a secret that is not"
echo "accounted for in scripts/ci-secrets.txt:"
echo ""
printf '%s' "$FOUND" | sed 's/^/  /'
echo ""
echo "In Woodpecker a from_secret naming a secret the repository does not"
echo "have is a PIPELINE-level error, not a step failure: the run ends"
echo "'status: error, workflows: 0' and nothing executes — not the gate, not"
echo "the builds, and not the forge publish every download URL we hand out"
echo "points at. That is how cron #434 stopped the nightly release."
echo ""
echo "If the secret exists on the server: add its name to"
echo "scripts/ci-secrets.txt in this commit."
echo "If it does not: create it FIRST, or drop the reference and let the"
echo "step read a plain environment variable that is allowed to be unset,"
echo "the way the publish-site step does."
echo ""
echo "'!legacy-secrets-key' names the deprecated step-level 'secrets:' list."
echo "It carries the same requirement and is not audited here; write the"
echo "reference as 'from_secret:' instead."
exit 1
