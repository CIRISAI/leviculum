# Contributing

For a contributor about to open a PR: the one rule, what a change must
pass, and how commits are attributed.

## A check is not implemented until it has been observed failing

A guard or assertion counts as done when someone has watched it fail —
until then it is indistinguishable from nothing, because nothing also
passes. Write the failing case first, or extract the decision into a
pure function so the failing side is reachable without hardware, and
make the test measure what production reads. The mechanics behind that
rule — every pin carrying a negative control, every test being executed
by some gate, every citation still pointing at what it claims — are in
[Checks That Are Actually Checks](docs/src/concepts/checks-and-citations.md);
the rule itself is in
[Evidence and Honesty](docs/src/concepts/evidence-and-honesty.md).

## What a change must pass

Four tiers, each including the ones before it:

* **Per batch of work:** `cargo fmt`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `just guards` (the sub-ten-second
  guards and censuses, about 20 s) and `cargo test --workspace`; then
  `just standard` (Tier 1, about 15 minutes warm) once the batch is
  done.
* **Before pushing:** `just fast` (Tier 0: fmt, clippy, the rustdoc,
  citation, submodule and commit-trailer gates and the workspace lib
  tests). The pre-push hook runs it for you once `scripts/install-ci.sh`
  has set `core.hooksPath`. A change that touches a core type runs
  `just standard` first; a protocol- or daemon-touching one adds
  `periculum run conformance`, every scenario green. The corpus grows,
  so `periculum list conformance` prints the live count rather than
  this file.
* **Nightly:** Tier 2, which is `just complete` (every test in the
  workspace), `just fuzz-nightly` and the periculum `conformance/` and
  `regression/` corpora in docker, on the maintainers' host every night.
  `just extensive` is the same tier on demand. You do not have to run
  it; a red night is triaged there.
* **Release:** the full periculum hardware corpus on the LoRa rig
  (Tier 3, `just nightly`), green or each red attributed, before a tag.
  That one needs the radios and is the maintainers' job.

[Testing](docs/src/development-testing.md) states what each tier runs,
how long it takes and what it proves.

Do not commit while tests are red, and do not carry a red test forward
as a known issue.

On the forge, every push and every pull request runs `just ci-gate`
(fmt, clippy over all targets, and every test in the workspace except
the suites and tests that need a `reference/` submodule the pipeline
does not clone — `scripts/ci-gate-integ.sh` names them) through
`.woodpecker/ci.yml`, and the commit-message check through
`.woodpecker/commit-trailers.yml`. You do not have to install
anything for those two to run; they are the checks that do not depend
on your clone having our hooks.

## Versions

The version numbers move with the code, by script, at every landing (a
push of gated work to `origin/master`, a few a day). The rule: every
versioned unit whose files changed since `origin/master` carries a
version newer than the one there.

* **The units.** The workspace (`[workspace.package] version` in the
  root `Cargo.toml`, and with it every crate whose version is the
  workspace's: core, std, the CLI with `lnsd`, `lnflash`, ...), and
  each of `loquax`, `loquax-android`, `lnomad`, `lnpnd`, `lblogd`,
  `leviculum-nrf`, `leviculum-esp` and `leviculum-lr11xx` with a
  version of its own. A unit's files are its crate directory; `docs/`,
  `scripts/` and the `Justfile` move nothing on their own.
* **What moves.** A plain version bumps its patch, `0.1.0` to `0.1.1`.
  The workspace sits in a development window and its moving number is
  the dev counter: `0.10.0-dev`, `0.10.0-dev.1`, `0.10.0-dev.2`. The
  packaging turns that into `0.10.0~dev.2`, which upgrades an installed
  `0.10.0~dev` and sorts below the `0.10.0` it leads to
  (`scripts/test-deb-stamp.sh` compares both). `loquax-android`'s
  `[package.metadata.android] version-code` grows by one with its
  version, because Android refuses an update whose versionCode did not
  grow; the app's `Makefile` stamps that code and the version into the
  manifest.
* **How.** `python3 -I scripts/bump-touched.py` bumps every owed unit
  and the lock files that name it; commit the result. `just
  version-guard`, part of `just guards`, refuses a tree that forgot.
  Several passes between two landings share one bump.
* **The base.** Both print what they compared against first,
  `base=<sha12> (origin/master, <date>)`, and every `OWED` line names
  the unit's `base_version=` and `head_version=`. `--base <sha>`
  compares against any other commit instead. A base that is not an
  ancestor of HEAD stops the script with `base is not an ancestor of
  HEAD; fetch origin` and exit 2, which a gate reads as its own
  infrastructure, not as owed versions (exit 1). A base that is an
  ancestor but stale looks fresh to git; run `git fetch origin` before
  the guard and read the date on the base line.
* **What only the maintainer moves.** Minor and major. At a release
  `0.10.0-dev.N` becomes `0.10.0` and the next window opens as
  `0.11.0-dev.1`; no script does that step.

Every binary's `--version` prints the version and the commit it was
built from, and the firmware's `[FW_BUILD]` banner carries
`version=` after `git_sha=` and `dirty=`.

## Commit messages and attribution

Documentation and commit messages are English; no AI trailers. Commit
under your real name and a reachable e-mail address; sign-off is not
required.

The wording above is [periculum's](https://codeberg.org/Lew_Palm/periculum),
verbatim and on purpose: the two repositories are developed together and
a rule that is stated once and relied on twice is a rule with a seam in
it. It had that seam until 2026-08-07, when a `Co-Authored-By:` naming a
model reached a periculum commit — in a pass whose author had cited this
exact rule, correctly, hours earlier — and was caught only because a
human read the message before pushing.

So the rule now has something behind it in both repositories:

* `scripts/check-commit-trailers.sh` rejects a commit message carrying a
  machine-authorship line. It matches only at column 0, because that is
  where a trailer is a trailer; a message that needs to *quote* one
  indents it.
* `.woodpecker/commit-trailers.yml` runs that over every commit pushed
  since a pinned baseline. This is the enforcement, and it is a forge
  check rather than a hook because a fresh clone has no hooks.
* `.githooks/commit-msg` — the only commit-msg hook in the tree, and the
  one `scripts/install-ci.sh` installs by pointing `core.hooksPath` at
  `.githooks` — runs the same script at commit time so the failure
  arrives while the message is still in the editor. It refuses; it never
  edits what you wrote. Convenience, not enforcement.

**The rule binds our commits, not yours.** A machine-authorship trailer
is a violation on a commit authored by one of the project's own
identities — the `ours` lines in `scripts/commit-trailer-baseline.txt` —
and on nothing else: we do not rewrite, and do not refuse, a message
somebody outside the project wrote. Your attribution is yours. The hook
and the pushed-range check both read `git log --format=%ae` and agree;
if you install our hooks in your own clone they will not ask you to
change your commit messages.

That exemption is counted rather than trusted. `foreign` in the same
file pins how many commits above the baseline carry such a trailer under
an author that is not ours, and the check recomputes it every run — so a
new external contribution with a trailer lands, keeps its message, and
still shows up as a number moving in a diff. Left uncounted, the
exemption would be an off switch anyone could reach by setting an author
e-mail.

Sixteen commits below the baseline carry such a trailer. They are listed
in the same file rather than rewritten out of published history, and
their count is checked the same way, so the baseline cannot be quietly
moved forward.

## Security reports

A suspected vulnerability goes to the private contact in
[SECURITY.md](SECURITY.md), not into an issue and not into a pull request.
Both are public before a fix exists, and the fix is the point. That file
also states the acknowledgement window and the disclosure terms, so you can
decide whether to report before you do.

## Licensing

Contributions are AGPL-3.0-or-later; see [LICENSE](LICENSE).
