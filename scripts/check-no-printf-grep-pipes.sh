#!/bin/bash
# No `printf ... | grep -q` (or `echo ... | grep -q`) in a script that runs
# under `pipefail`.
#
# bash's `printf` builtin writes a pipe one line per write(2). `grep -q`
# exits on its first match. When the match comes before the last line, the
# next write finds the pipe closed and printf dies of SIGPIPE (141), and
# `pipefail` reports 141 for the whole pipeline: a MATCH reads as a failure.
# Order 1314 measured it on lnflash-bundle.sh's selftest: 14 of 20 000 runs
# red under `stress-ng --cpu 40`, 0 of 20 000 with `grep -q X <<<"$list"`.
# It is #843's third site; forge pipeline 536 (ci-gate-integ.sh) and the
# skip list in standard-integ.sh were the first two. Order 1340 converted the
# rest of the tree and put this guard on the push path, so the fourth is
# refused where it is written instead of found under load a week later.
#
# In scope: every tracked shell file (`*.sh`, `*.bash`, or a `sh`/`bash`
# shebang) that sets pipefail on an uncommented `set` line, and every one
# without a shebang at all, because a file that is only ever sourced runs
# under its caller's options and its callers here set pipefail. A script
# without pipefail is out of scope: there the pipeline's status is grep's.
#
# The construct: `printf` or `echo` piped straight into `grep` with `-q`
# among its short flags, or `--quiet`/`--silent`. A line ending in `|` or
# `\` is joined with the next before matching, so a pipe broken over two
# lines is caught too. Full-line comments are skipped: the scripts that were
# fixed explain the trap in prose. The remedy is the here-string,
# `grep -q X <<<"$var"`, which bash feeds from a temporary file or a pipe it
# fills before grep starts, so nothing is left to write when grep exits.
#
# Exit 0 = no hit. Exit 1 = a hit, or the self-test failed.
#
# Usage:
#   bash scripts/check-no-printf-grep-pipes.sh             # the tree
#   bash scripts/check-no-printf-grep-pipes.sh --selftest  # fixtures only
set -uo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
TAG="[no-printf-grep-pipes]"

# Print `<label>:<line>: <text>` for every hit in shell file $1 ($2 names it
# in the output, $1 by default); nothing if the file is out of scope.
hits() {
    awk -v file="${2:-$1}" '
        NR == 1 { shebang = ($0 ~ /^#!/) }
        { raw[NR] = $0 }
        /^[[:space:]]*set[[:space:]].*pipefail/ { pipefail = 1 }
        END {
            if (shebang && !pipefail) exit 0
            joined = ""; start = 0
            for (i = 1; i <= NR; i++) {
                line = raw[i]
                if (line ~ /^[[:space:]]*#/ && joined == "") continue
                if (joined == "") start = i
                joined = joined " " line
                if (line ~ /(\\|[^|]\|)[[:space:]]*$/) continue
                if (joined ~ /(^|[^A-Za-z0-9_-])(printf|echo)[[:space:]][^|]*\|[[:space:]]*\\?[[:space:]]*grep([[:space:]]+-[A-Za-z-]+)*[[:space:]]+(-[A-Za-z]*q[A-Za-z]*|--quiet|--silent)([[:space:]]|$)/) {
                    sub(/^[[:space:]]+/, "", joined)
                    printf "%s:%d: %s\n", file, start, joined
                }
                joined = ""
            }
        }
    ' "$1"
}

# Every tracked shell file under $1 (a git work tree), one path per line.
shell_files() {
    git -C "$1" ls-files | while IFS= read -r f; do
        [ -f "$1/$f" ] || continue
        case "$f" in
            *.sh|*.bash) printf '%s\n' "$f" ;;
            *)
                IFS= read -r first < "$1/$f" || true
                [[ "$first" =~ ^#!.*[/[:space:]](ba)?sh([[:space:]]|$) ]] && printf '%s\n' "$f"
                ;;
        esac
    done
}

# SC2016: the fixtures are shell source, and their `$x` is meant literally.
# shellcheck disable=SC2016
selftest() {
    local dir rc=0 got want name
    dir="$(mktemp -d)"
    # The fixtures spell the pipe through a variable, so this file's own
    # source never carries the construct it refuses.
    local P='|' PF=$'#!/bin/bash\nset -euo pipefail\n'
    printf '%sif printf %%s "$x" %s grep -qF y; then :; fi\n' "$PF" "$P" > "$dir/pipe.sh"
    printf '%sif grep -qF y <<<"$x"; then :; fi\n' "$PF" > "$dir/herestring.sh"
    printf '#!/bin/bash\nset -e\nif printf %%s "$x" %s grep -q y; then :; fi\n' "$P" > "$dir/no-pipefail.sh"
    printf 'echo "$x" %s grep -qx y && :\n' "$P" > "$dir/sourced-lib.sh"
    printf '%s# never printf "$x" %s grep -q y\n' "$PF" "$P" > "$dir/comment.sh"
    printf '%sprintf "%%s\\n" "$x" %s\n    grep -q y || :\n' "$PF" "$P" > "$dir/continued.sh"
    printf '%secho "$x" %s grep -E -q y || :\n' "$PF" "$P" > "$dir/later-flag.sh"
    printf '#!/bin/bash\nset -o pipefail\necho "$x" %s grep --quiet y || :\n' "$P" > "$dir/set-o.sh"
    printf '%secho "$x" %s%s grep -q y\n' "$PF" "$P" "$P" > "$dir/or-list.sh"
    printf '%secho "$x" %s grep -c y\n' "$PF" "$P" > "$dir/count.sh"

    for name in pipe herestring no-pipefail sourced-lib comment continued later-flag set-o or-list count; do
        case "$name" in
            pipe|sourced-lib|continued|later-flag|set-o) want=hit ;;
            *) want=clean ;;
        esac
        got=clean
        [ -n "$(hits "$dir/$name.sh")" ] && got=hit
        if [ "$got" != "$want" ]; then
            echo "$TAG SELFTEST FAIL: fixture '$name' was $got, expected $want"
            sed 's/^/    /' "$dir/$name.sh"
            rc=1
        fi
    done
    rm -rf "$dir"
    if [ "$rc" -ne 0 ]; then
        echo "$TAG the checker itself is broken; its verdict on the tree means nothing."
        return 1
    fi
    echo "$TAG selftest ok: 5 planted pipes refused, 5 clean shapes passed"
}

if [ "${1:-}" = "--selftest" ]; then
    selftest
    exit $?
fi

found=""
while IFS= read -r f; do
    out="$(hits "$REPO_DIR/$f" "$f")"
    [ -n "$out" ] && found="${found}${out}"$'\n'
done < <(shell_files "$REPO_DIR")

if [ -z "$found" ]; then
    exit 0
fi

echo "$TAG FAILED: printf/echo piped into grep -q under pipefail:"
printf '%s' "$found" | sed 's/^/  /'
echo
echo "grep -q exits on its first match; when printf still has lines to write"
echo "it dies of SIGPIPE and pipefail turns the match into exit 141 (#843,"
echo "order 1314: 14 of 20 000 under load). Write it as a here-string:"
echo "  grep -q PATTERN <<<\"\$var\""
exit 1
