# CI Pipeline

Two things run tests here, and they are not the same thing. Four
local tiers with different time budgets and triggers automate the
project's test discipline ([Testing](development-testing.md)), on the developer's machine —
no GitHub Actions. On the forge, Woodpecker runs a smaller set on
every push, because the local tiers are hooks a fresh clone does not
have and `--no-verify` switches off. The tiers come first; the forge
pipelines are below them.

## Tiers

| Tier | Name | Trigger | Budget | Test scope |
|------|------|---------|--------|------------|
| 0 | `fast` | pre-push hook | ~3 min | fmt + clippy (host + nrf firmware workspace, both BSPs) + the firmware stack-frame gate + rustdoc gate + the [third-party notice guard](concepts/licensing-and-notices.md) + the pre-push guard selftest + workspace lib tests |
| 1 | `standard` | on demand: `just standard`, once per batch | ~15 min (first run: 20-40 min cold compile) | Tier 0 + every integration-test target in the tree (the ones needing a flag or their own feature set by name, the rest computed by `scripts/standard-integ.sh`) + TCP-hub endurance smoke soak (see [Soak and endurance](soak-and-endurance.md)) + the `status_parity` two-daemon suite + the ignored-test census |
| 2 | `extensive` | on demand: `systemctl --user start leviculum-ci-tier2.service`; scheduled as the tier-2 nightly, 03:37 daily | ~30-90 min | Tier 1 + the periculum `conformance/` and `regression/` corpora (docker) |
| 3 | `nightly` (the recipe, not the nightly run) | the rig's hourly cadence of targeted hardware scenarios, plus a full run before a release | ~2-6h full | Tier 2 + LNode flash-from-HEAD + the periculum `hardware/` corpus |

Each tier runs everything from the lower tiers as well. Three different
things are called "nightly", and each proves something different:

- **The tier-2 nightly** (03:37 daily on the nightly host, a fresh clone of
  `origin/master`) runs `just complete`, `just fuzz-nightly` and the
  periculum `conformance/` and `regression/` corpora, whose nodes run in
  containers on their own network namespaces. A green or attributed night
  proves the whole workspace test suite, the fuzz targets and the two
  docker corpora on the commit it tested, and signs a
  `refs/nightly/green/*` ref for it. It flashes no board and radiates
  nothing.
- **The published build** is the forge's `nightly.yml`, on its cron at
  09:05 UTC (10:05 CET) or fired early by the green ref (see [What fires
  the publish](#what-fires-the-publish)). It proves that `just ci-gate`
  passes and that the .debs and the lnflash bundle build, and it publishes
  only a commit a green ref covers. It runs no corpus.
- **Tier 3** is the only one that touches radios: the hourly rig cadence
  of targeted hardware scenarios, and the full `hardware/` corpus
  (`just nightly`) before a release. Only a full run proves the stack on
  real boards; an hourly cell proves its own scenario.

`just guards` is not in this table because it is not a tier: it is the
coder-side subset of Tier 0's sub-ten-second guards and censuses (~20 s warm),
run beside fmt, clippy and the workspace tests after a batch so that a guard
red is found by its author rather than by the landing gate. `fast` and
`standard` remain what the push path and the landing gate run. See
[Testing](development-testing.md#the-four-tiers).

## What the forge runs

Three Woodpecker workflows on `ci.codeberg.org`, all in
`.woodpecker/`:

| File | Fires on | Runs |
|------|----------|------|
| `ci.yml` | every push, every pull request, manual | `just ci-gate` — fmt, clippy over all targets, and every workspace test except the submodule-bound suites and tests named in `scripts/ci-gate-integ.sh` (Codeberg #312) |
| `commit-trailers.yml` | every push, every pull request, manual | `scripts/check-commit-trailers.sh` over the pushed range |
| `nightly.yml` | cron, manual, plus pushes touching the packaging paths | cron: the same gate, both .deb builds and the lnflash bundle, then the forge release; push: only the amd64 .debs, the book and the lnflash bundle (726) |

`ci.yml` exists because until Codeberg #299 none of the others
covered an ordinary source commit: the nightly's push trigger is
filtered to the packaging paths, the trailer check reads messages
rather than code, and what stood between a Rust-only commit and the
public releases page was `.githooks/pre-push` — per-clone local
config, skipped by `--no-verify`, running the developer's toolchain
and not the pipeline's. So it carries no `path:` filter, and
`scripts/check-ci-pipeline.sh` (part of `just fast`) fails the push
path if any future edit gives it one.

`just ci-gate` is a Justfile recipe rather than commands spelled out
in YAML, and the container provisioning both pipelines need is
`scripts/ci-gate.sh` rather than two copies of the same apt lines:
one gate, one environment, no drift between the two files that run
it. The gate is deliberately not an alias for `just fast` — the
recipe's comment lists what a submodule-less host-target container
cannot prove (the firmware workspace, the cross-compiles, the
submodule pins), and those stay on the local push path, which has
the targets. Measured cost, cold, of the version that ran `--lib`
only: 3m23s including provisioning. With Codeberg #312's widening the
runner's own step timing is 965-1185 s (cron pipelines 476-486, push
488, provisioning included), before the release build of the
integration binaries that four `mvr` tests need was added to it.

What the gate runs, and what it leaves out. The forge container is a
stock `rust:bookworm` with `musl-tools`, `socat` and `iproute2` added
by `scripts/ci-gate.sh`, and a clone without submodules (#300). In it
`just ci-gate` runs fmt, clippy over every target, the workspace lib
tests, `just build-integ-bins` (the release binaries the `mvr` tests
spawn), and then `scripts/ci-gate-integ.sh`: every integration-test
target the tree has, every bin unittest target and the doctests. Out
of that it leaves exactly what needs a `reference/` submodule at run
time, because those fail rather than skip without it: three whole
targets (`rnsd_interop`, `leviculum-lxmf`'s `reference_lock`,
`loquax`'s `python_interop`) and seven tests by exact name inside `mvr`,
which includes the rnsd_interop harness by `#[path]` and so runs the
harness's own tests and the four tests that spawn a Python peer
through it. The script's header holds the
list, one written reason per entry, and checks every entry against the
tree so a stale one is a red gate. A target that declares
`required-features` (`loquax`'s `tui_snapshot`) is left out of that
featureless run too, because cargo refuses the whole invocation over
it, and `ci-gate` runs the recipe that passes the feature
(`just loquax-tui`) right after; `check-standard-integ` holds both
halves. The citation guard runs in full but
skips the citations into the absent references and prints how many
(about 2600). Everything left out runs on the tier-2 nightly with the
submodules present, which is what the publish gate reads.

## What may be published

The forge gate runs `fmt`, `clippy` and every test in the workspace
except three suites and seven submodule-bound tests inside `mvr`, and
`rnsd_interop` — the suite that measures
whether we still interoperate with a Python-RNS peer, which is half of
Priority 1 — is the largest of the three. It runs in neither forge
pipeline and cannot: it needs the `reference/Reticulum` submodule and a
`python3`, and both pipelines clone with `submodules: false` on purpose,
which is the property `just check-plain-clone` exists to hold (Codeberg
#300). Fetching the submodule into the release path would undo exactly
that. The other two are `leviculum-lxmf`'s `reference_lock` and
`loquax`'s `python_interop`, for the same reason;
`scripts/ci-gate-integ.sh` holds the list, one written reason per
entry, and computes everything else it runs from the tree.

So the interop verdict is **imported rather than re-derived**
(Codeberg #312). The tier-2 nightly already runs the whole workspace,
with submodules, over a fresh clone pinned to `origin/master`. When
that run is green, or green-or-attributed, it pushes a lightweight ref
at the commit it tested:

```
refs/nightly/green/<YYYYMMDDTHHMMSSZ>  ->  <tested commit>
```

and `scripts/publish-nightly.sh` refuses, before it touches the forge,
any commit those refs do not cover. Four conditions, and a refusal
always names which one failed:

| Condition | Meaning |
| --- | --- |
| `NO-SIGNAL` | there is no `refs/nightly/green/*` on the remote, or it could not be read |
| `NOT-COVERED` | no green ref names this commit or a descendant of it — no nightly has seen this code |
| `TOO-OLD` | the newest covering ref is older than the staleness bound (72 h) |
| `GREEN-RUN-MISMATCH` | `LEVICULUM_PUBLISH_FROM_GREEN_RUN` names another commit than the one under publish (see below) |

The 72 h bound is measured, not assumed: over 2026-08-22..09-22 the
nightly timer produced 27 runs with a median gap of 24 h, every gap
but one at or under 54.4 h, and one 96 h gap (2026-09-04 to 09-08)
which is precisely the case the bound exists to stop. When the forge
publishes on the fallback cron rather than on the trigger described
below, the freshest ref it can read is normally the previous night's and
is already ~24 h old. 72 h therefore accepts the ordinary day plus one
missed night and refuses two.

### What fires the publish

Two publishers, one target each (Lew's decision 2026-10-14, leviculum
726). The forge's `nightly.yml` cron run publishes the forge's rolling
`nightly` release. The tier-2 nightly host publishes leviculum.network
from the tree its night tested. A manual run of `nightly.yml` from the
Woodpecker UI publishes both, the fallback for a morning the host
cannot.

**What signs a night.** `scripts/nightly-green-ref.sh --verdict`
takes `green`, `attributed` or `red`. `attributed` means
green-or-attributed: no new unexplained red, no undecided cell, the
unit suite green, no stale exclusion; registered reds and infra skips
are allowed. It signs exactly as `green` does, after the same check
that the run's manifest shows a green `rnsd_interop`, under the same
`refs/nightly/green/<stamp>` name, so `scripts/check-nightly-green.sh`
on the forge does not tell the two apart. The word that signed a ref is
in the signer's log (`signed green` or `signed attributed`) on the
nightly host; the ref is lightweight and carries none. `red` signs
nothing. The reviewer's `nightly-tier2.sh`, which is not in this repo,
passes the word. Until 726 only a strictly green night signed, and none
did from 2026-10-08 to 10-14: every night carried a registered red or
the heap-accounting canary's infra skip, so the forge release stood at
ed0754e4 for six days.

**The forge.** `package` and `publish` run on `event: [cron, manual]`,
after `gate`, `build-amd64`, `build-arm64` and `build-lnflash`; a red
step before them stops them, because a step's default `when:` is
success-only. `publish` runs `scripts/publish-nightly.sh`, which asks
`scripts/check-nightly-green.sh` first and refuses master HEAD when no
green ref covers it. The cron is `5 9 * * *` UTC (10:05 CET; 1513).
The time is set by the forge, not by the nightly: Codeberg's Woodpecker
server restarts every three hours on the full hour and cancels what is
running, and a full run is about 75 minutes, so it starts five minutes
after a three-hour mark (the note above the cron in `nightly.yml`, and
`scripts/forge-cron-schedule.sh` to set it). The tier-2 nightly has no
fixed end, so a clock-fired run sees the same night's ref only when that
night finished before 09:05: the refs on the remote are stamped 05:07Z to
07:15Z on such nights
(`20261019T050710Z`, `20261029T071557Z`, `20261105T071415Z`). It covers
HEAD only when nothing landed between that ref and 09:05, and a night that
ends later reads the night before (`20261021T132124Z`,
`20261103T193322Z`, `20261106T132853Z`), which is the refusal pipelines
455 and 458 met under an earlier 04:00 cron. Since 2026-09-24 the reviewer
host polls the forge every ten minutes and fires this cron through the
Woodpecker API as soon as a new `refs/nightly/green/*` appears
(`lev-nightly-publish-trigger`, reviewer-host tooling: it is not in this
repo, and the API token stays on that host on purpose). That run is the one
that normally publishes; the 09:05 run is the fallback, and both on one day
is harmless, because the release is rolling and `scripts/publish-nightly.sh` writes the same
`nightly` tag with its assets overwritten.

The overwrite keeps one generation (#323): before its uploads a run
renames every asset it replaces to `<name>.previous.<ext>`
(`leviculum-nightly-amd64.previous.deb`), and after them it deletes the
`.previous` set that displaced, so a bad build that passed the gates still
leaves the build before it downloadable. A failed run undoes its renames.
A run whose commit the `nightly` tag already names (the trigger and the
09:05 fallback on a day nothing landed) renames nothing and replaces the
current set in place, so the second publish of one commit cannot push the
fallback out.

**The site.** After a green-or-attributed night the tier-2 nightly runs
`just nightly-publish` in the very tree it tested, seconds after it
pushed the green ref:

```
LEVICULUM_PUBLISH_FROM_GREEN_RUN=<tested sha> \
  just --justfile <tree>/Justfile --working-directory <tree> nightly-publish
```

`scripts/nightly-publish.sh` checks every tool and credential file
first, refuses a dirty tree, and asks `scripts/check-nightly-green.sh`
whether HEAD may go out. `LEVICULUM_PUBLISH_FROM_GREEN_RUN` is accepted
only when it equals HEAD; it exists because the forge may not yet serve
the ref the night pushed a moment earlier. Then it runs the scripts the
forge pipeline runs, in the same order: `deb-stamp.sh`, `build-deb.sh`
for amd64 and arm64, `lnflash-bundle.sh` with the firmware built,
`build-docs-tarball.sh`, `collect-nightly-debs.sh` into `dist/`, then
`build-nightly-apk.sh`, which adds the Android APKs to `dist/` (1267; the
forge has no step for it, its builders carry no Android SDK, so the APKs
are on the site only), then `publish-site.sh`. It does **not** run `publish-nightly.sh` by default:
the forge cron owns the forge release. `LEVICULUM_PUBLISH_FORGE=1` is
the manual fallback for a morning the forge could not publish; it adds
`publish-nightly.sh`, each of the two publishes then running whatever
the other did. The credentials are files on that host:
`~/.config/leviculum-site/{id,known_hosts}` (the site target and port
follow from the pinned `known_hosts` line) and, with
`LEVICULUM_PUBLISH_FORGE=1` only, `~/.config/codeberg-token`, read when
`CODEBERG_TOKEN` is unset. A second run on the same build id (one
commit, one UTC day) publishes the same `dist/` again rather than
rebuilding, because the packages are not byte-reproducible and the site
refuses a known build id with different contents.
`LEVICULUM_PUBLISH_DRY_RUN=1` builds everything and has the publish
scripts say what they would upload, uploading nothing.

`publish-site` in `nightly.yml` stays on `event: manual`. One publisher
per target is the fix for 2026-10-08, when the cron and its trigger ran
twice on ed0754e4 and the second site publish was refused: the site
receiver takes one build per build id, while the forge release takes a
second build by overwriting the first. Do not start a manual run on a
day the host has published the site.

**When the two publish different commits.** The forge publishes master
HEAD of the cron's morning; the host publishes the commit its night
tested. Usually they are the same commit, because the publish gate
accepts HEAD only when a green ref names it or a descendant of it. They
differ when a manual run, the override below, or a catch-up night's ref
lets the forge publish a HEAD the host's night did not test. Then one
UTC day has two builds: the forge release carries the later HEAD, the
site the tested commit, each under its own build id
(`scripts/deb-stamp.sh` puts the commit in it), so neither receiver
sees a second build under a known id. That is acceptable because every
master commit passed the land gate, the full conformance corpus, before
it was pushed; what a green ref adds on top is the interop suite the
forge cannot run, and the gate still refuses a HEAD no signed night
covers. The next night brings both back to one commit.

**What an operator sees when no ref covers HEAD.** The publish step
refuses with `NO-SIGNAL` when no green ref can be read from the remote
at all, and with `NOT-COVERED` — printing "the newest green ref is
..." and "is neither that commit nor an ancestor of it" — when refs
exist but none of them names this commit or a descendant of it, which
is what a night that has not pushed yet looks like from the publish
step. Neither is a build failure and neither needs a code change:
`bash scripts/check-nightly-green.sh --commit <sha>` answers which ref
the forge can see, and the fix is to wait for the night's ref to land
and then start the nightly cron by hand from the Woodpecker UI
(Repo → Settings → Crons) if the trigger has not done it first.

**The push run.** A push touching the packaging paths runs only
`build-amd64` and `build-lnflash`, about 12 minutes; `gate` and
`build-arm64` are cron- and manual-only (726). The gate is the same
`bash scripts/ci-gate.sh` that `ci.yml` runs on the same push, and it
had grown to 18 to 21 minutes, which made the push run 45. Pipelines
515 and 516 (2026-10-13) were killed with `Canceled` 21.5 minutes in
while `ci.yml` beside them succeeded. It was not the timeout (the
repository's is 60 minutes) and not cancel-previous (no newer pipeline
was created at either moment): every run of this repository the forge
has killed so far (469, 474, 494, 505, 515, 516) ended between
HH:00:36 and HH:00:52 UTC on an hour divisible by three, while runs
crossing 05:00 (514) and 23:00 (501) were left alone. What cancels them
is outside this repository; a short push run is simply less often
caught by it. The amd64 build proves the manifests and `build-deb.sh`
the arm64 build uses, and the lnflash bundle has its own inputs in the
path filter.

### Publishing anyway

A human can decide otherwise. Set, on the publish step:

```
LEVICULUM_PUBLISH_WITHOUT_NIGHTLY="why you are doing this"
```

It is deliberately not a boolean: the value is the reason, it must be
at least 8 characters, and it is printed into the run's log where it
stays with the build it excused. A value too short to be a reason is
refused.

Two ways to set it, and neither needs a code change:

* **Woodpecker** — start the `nightly.yml` workflow manually and add
  the variable in the run dialog.
* **By hand, from a checkout** — `CI_REPO=Lew_Palm/leviculum
  CI_COMMIT_SHA=$(git rev-parse HEAD) CODEBERG_TOKEN=...
  LEVICULUM_PUBLISH_WITHOUT_NIGHTLY="..." bash
  scripts/publish-nightly.sh`, with `dist/` already staged by
  `scripts/collect-nightly-debs.sh`.

`bash scripts/check-nightly-green.sh --commit <sha>` answers the
question on its own, without publishing anything, which is the first
thing to run when a nightly publish has gone red.

### What holds the chain together

| Gate | Asserts |
| --- | --- |
| `just nightly-green-selftest` | both scripts BEHAVE: each of the three refusals fires, the override works and needs a reason, and a red or absent `rnsd_interop` signs nothing |
| `just check-publish-nightly-gate` | the mechanism is still CONNECTED: five links from the publish step to the manifest the signer reads, each broken on purpose in its own self-test |

Both are in `just fast`, so they run on the push path. They are
separate because the failure mode is available to both halves: a gate
wired into nothing, and a gate wired in that says yes to everything.

The signing half is `scripts/nightly-green-ref.sh`. It does not take
the night's verdict on trust for the one property this is about — it
reads the run's own manifest (`scripts/run-with-manifest.py`,
Guarantee B) and refuses to sign unless the `rnsd_interop` unit
executed and every test in it passed. "The nightly was green" must not
be able to mean "the suite never ran".

## Installation

One command, idempotent:

```
just install-ci
```

It installs `git` hooks (via `core.hooksPath = .githooks`), runner
scripts, systemd user units, the separate cargo target dir, the
build-directory sweeper `just sweep` needs, the state dir, and the two
pinned tools that live in venvs the runner owns — `esptool` for RNode
flashing, and the Python Reticulum periculum's host `type = "python"`
nodes run (`~/.local/state/leviculum-ci/rns-<pin>`, see "Host python
cells need the image's Reticulum" below). Re-running is safe.

The installer detects the worktree it was run from and patches the
systemd-unit `ExecStart` paths to match — so a `git worktree`-based
second checkout (see "VM-mode install" below) installs its own units
that fire against itself.

### VM-mode install (CI worktree on a long-running host)

For schneckenschreck or any other dedicated CI machine where the
nightly Tier-3 runs land, install with `--vm-mode`:

```
git worktree add ~/coding/libreticulum-ci master
cd ~/coding/libreticulum-ci
bash scripts/install-ci.sh --vm-mode
```

`--vm-mode` differs from the default install in two ways:

1. The git-hook wiring (`core.hooksPath = .githooks`) is **skipped**.
   The VM never commits or pushes; hooks would never fire.
2. A worktree-scoped marker file
   (`.git/worktrees/<name>/leviculum-ci-vm-mode-marker`) is created.
   `run-tier2.sh` and `run-tier3-hw.sh` check this marker at the
   head of every run and, if present, invoke `_repo-sync.sh` to do
   `git fetch + git checkout --force origin/master + git submodule
   update --recursive`.

The marker is per-worktree, not per-user: a manual invocation of
`run-tier2.sh` from the developer's primary checkout will **not**
trigger the destructive `--force` checkout against the wrong tree.

The synced commit hash is appended to `last-results.txt` as
`<timestamp> tier2 sync HEAD=<short-hash>` (or `tier3-hw` for the
nightly), so you can correlate scheduled runs with the master commit
they tested.

## The firmware stack-frame gate

`just nrf-stack-frames` builds both firmware binaries and reads the
frame-allocating `sub sp` immediates out of the linked ELF. Any frame
above 16 KB fails the gate.

The T114 stack is 128 008 B and grows down into the SoftDevice's RAM
floor. An overflow past `_stack_end` does not fault: it overwrites SD
state, and the board dies later in an SD internal assertion with a
useless PC. So a single oversized frame is both fatal and invisible,
which is why this is checked statically on every push rather than
observed at runtime.

The frame it was written for: `Box::new(builder.build(..))` materialised
a by-value `NodeCore` — over 40 KB once `EmbeddedStorage`'s inline
collections are counted — twice in `main`'s poll frame. 94 720 B, 74 % of
the stack, ~13 KB of margin left for the whole call tree.
`NodeCoreBuilder::build_boxed` configures through the box, which drops
that frame to 12 672 B. It does so only because the compiler builds the
`Box::new` argument straight into the allocation, which Rust does not
promise; the gate's limit is therefore held below the node box
(`NODE_BOX_BYTES`, 31 744 B), so a build that stops eliding the copy puts
one whole `NodeCore` in a frame and fails here.

The gate prefers `arm-none-eabi-objdump` and falls back to the rustup
`llvm-tools` `llvm-objdump`; `install-ci.sh` installs the latter.

## The package feature-set gate

`just deb-features-check` (1593) compiles every package this host's
architecture ships with exactly the target triple and feature set the
packages are built with: `cargo check --release`, nothing linked or
packaged. On amd64 that is the musl binaries with `loquax/tui`, Loquax
on the gnu triple with `loquax/tui loquax/gui`, and lnsd and lnpnd with
`heap-accounting`. The recipe runs `scripts/build-deb.sh --check`, the
packaging script's own mode over the same argument arrays its build
reads, so the check and the build cannot name different sets.

It exists because Loquax with `tui` and `gui` together on the gnu triple
did not compile on master from 902bc129 to cbbd870d: 1570 added a field
to `GuiArgs` and not to the two initialisers that only that pair
compiles, so no package could have been built, and `loquax-gui` (`gui`
alone) and `loquax-tui` (`tui` alone) were both green.

It is in `fast` and not in `guards`: 9.5 s after touching
`leviculum-core`, 3 min 35 s cold (measured 2026-11-09), and two release
feature sets of its own to build. Run it by hand before committing
whenever a change touches a `cfg(feature` boundary, a CLI args struct or
a feature list in a `Cargo.toml`.

## Manual operation

```
just fast        # Tier 0
just standard    # Tier 1
just extensive   # Tier 2
just nightly     # Tier 3
just status      # show recent runs across all tiers
```

## First-run expectation

Tier 1 runs in a separate `CARGO_TARGET_DIR` (`~/.cache/leviculum-ci-
target/`) so it doesn't fight your IDE's `target/` for inkremental
caches. The first run after `install-ci.sh` compiles the whole
workspace and all test binaries from scratch — **plan for 20-40
minutes**. Subsequent runs are incremental, ~5-15 minutes.

## Keeping the build directories bounded

Cargo adds; it never removes. Every changed input writes a new
hash-suffixed artefact next to the old one, so a target directory only
grows, and the growth rate is the point rather than any one build:
measured on the CI host on 2026-09-24, the `deps` directory under
`target/x86_64-unknown-linux-musl/debug` alone held 2705 files and
27 GB of that tree's 36 GB, and on 2026-09-09 one working day of gate
runs took the same tree to 137 GB and filled the root volume
(Codeberg #381). A full volume does not announce itself as a full
volume: hardware runs go red for want of space and look like the
stack.

```
just sweep                  # both workspaces, 30 GB and 4 GB caps
just sweep 20GB 2GB         # tighter caps
```

Two directories, because this repository has two workspaces — the host
one at the root and the firmware one in `leviculum-nrf` — and sweeping
the root leaves the firmware's 6 GB untouched. Where they lie is asked
rather than assumed, so a tree that moved its artefacts with
`CARGO_TARGET_DIR` (Tier 1 and the nightly do) is swept where they
actually are.

`cargo sweep --maxsize` drops the oldest artefacts until the directory
fits the cap, which keeps exactly the ones the next build would reuse.
`cargo clean` is the blunt version of the same thing and costs a full
rebuild of everything.

What no cleanup may take is the compilation cache: with
`RUSTC_WRAPPER=sccache` set, that cache is what makes the rebuild after
a sweep cheap, and it bounds itself through `SCCACHE_CACHE_SIZE`.
Deleting it to free space buys one-off gigabytes and charges the next
build for them.

## Notifications

**Read this as history, not as behaviour.** `scripts/run-tier3.sh` calls
`notify-send` on its verdict — `-u critical` for RED (sticky until
dismissed), `-u normal` for GREEN and for a lock-held skip, `-u critical`
again when the lock's holder is a suspected wedge. It is the only
tier runner that ever did. It is also not the script the nightly starts:
`leviculum-ci-nightly.service` runs `scripts/run-tier3-hw.sh`, which
writes the ledger and notifies nobody. So no tier notifies today. Results
are pull-only — `just status`, or
`~/.local/state/leviculum-ci/last-results.txt`.

**Prerequisite:** `notify-send` needs `DBUS_SESSION_BUS_ADDRESS` and
`XDG_RUNTIME_DIR` in the user systemd manager environment, which
exists only when you have a logged-in graphical session. On a
headless server, notifications are silently dropped — inspect
`~/.local/state/leviculum-ci/last-results.txt` instead.

## Stale-block on push (removed 2026-08-07)

`pre-push` used to block the push when the last `tier2 GREEN` line in
`last-results.txt` was ≥ 10 commits or ≥ 24 hours old. It was removed,
not repaired. Only `scripts/run-tier2.sh` writes that line, nothing has
started it since the Tier 2 timer was retired on 2026-06-12, and the
remedy the block printed (`just extensive`) does not write it either —
so the block could not be cleared by doing what it said. It was
unsatisfiable for 46 days, and the 502 commits that landed in that
window all used `git push --no-verify`, which switches off the lint,
Tier 0, mvr and the trailer guard along with it.

`scripts/ci-status.sh` still reports how long it has been since a Tier 2
run was recorded. It states the age and blocks nothing.

## Tier 1 after every commit (removed 2026-08-07)

`.githooks/post-commit` detached `scripts/run-tier1.sh` — `just standard`
under docker, 15 minutes warm and 20-40 cold — after every commit that was
not part of a rebase. It was removed, and the rule it failed is in
[Checks That Are Actually Checks](concepts/checks-and-citations.md).

The short form: a commit is not a unit anybody wants tested. WIP commits,
amends and commits mid-refactor all started a forty-minute run, which is
why the runner needed a dirty-flag loop to coalesce them — it was
repairing a granularity that was wrong to begin with. It was also
invisible: batches were separately instructed to start `just standard`
under `nohup`, so the same tier ran twice per batch for a week before
anyone noticed the hook existed. And it ran docker in the background,
which tears down containers whatever else is on the box was using — the
standing rule against starting the full integ suite behind someone's back
exists for that collision, and this hook was doing it after every commit.

Tier 1 is now started explicitly, once per batch, by typing
`just standard`.

## Logs

Location: `~/.local/state/leviculum-ci/`

| File | Contents |
|------|----------|
| `last-results.txt` | one-line tally per run (`<iso-timestamp> <tier> GREEN/RED <log-path>`, or `<tier> SKIPPED lock-held\|lock-suspect <verdict fields> <log-path>`) |
| `tier1-YYYYMMDD-HHMMSS-PID.log` | full Tier 1 output (one file per run) |
| `tier2-YYYYMMDD-HHMMSS-PID.log` | full Tier 2 output |
| `nightly-YYYYMMDD-HHMMSS-PID.log` | full Tier 3 output |
| `tier1.lock` | flock for Tier 1 concurrency control |
| `tier1.dirty` | marker that Tier 1 needs to (re-)run |

Rotation: tier 1/2 logs are deleted after 14 days; nightly logs after
60 days. Done at the start of each runner script.

Each script run gets its own log file (timestamp + PID suffix). No
run ever overwrites another run's log — this is intentional so a
failure trace cannot vanish under a successful re-run. The path of
the specific log goes into `last-results.txt` so `just status` can
point at exactly the right file.

## The scenario suites live in periculum

The multi-node scenarios that used to be `reticulum-integ` are now the
sibling [periculum](https://codeberg.org/Lew_Palm/periculum) checkout,
which leviculum expects at `../periculum` (override with
`PERICULUM_ROOT`, or the binary with `PERICULUM_BIN`). They are TOML
files, not `#[test]` functions, so the tier separation is a matter of
which directory a tier runs rather than of `#[ignore]`:

| Corpus | Binds hardware | Run by |
|---|---|---|
| `conformance/` | no | Tier 2 |
| `regression/` | no | Tier 2 |
| `hardware/` | yes | Tier 3 |

The split is machine-checked in periculum
(`periculum/tests/corpus_admission.rs`), so a scenario cannot drift into
the wrong tier by convention alone. A `hardware/` scenario whose boards
this bench does not hold reports `SKIPPED_INFRA` naming what was
missing — never RED.

Run one scenario by hand:

```
periculum run ../periculum/hardware/lora_link_rust.toml
```

### Host python cells need the image's Reticulum

A periculum node with `type = "python"` runs one of two Python
Reticulums. In a CONTAINER it runs the image's: `assets/Dockerfile`
installs the pinned `rns==<pin>` wheel last, where no resolver step can
move it. Run as a HOST process — every `emulated/` cell, and a BLE node
— there is no image, so the runner resolves it onto a venv carrying the
same two packages the image carries, in the same order:

```
~/.local/state/leviculum-ci/rns-<pin>   # $PERICULUM_PYTHON_RUNTIME_DIR overrides the parent
```

`just install-ci` provisions it, the way it provisions the other pinned
tool in a venv: one guarded call out to
`../periculum/scripts/install-python-runtime.sh`, which reads the pin
out of periculum's `assets/Dockerfile` rather than writing it down a
second time, installs the vendored LXMF first and the pinned wheel
last, and asserts the version before leaving the venv on disk. It is
idempotent — an already-provisioned venv costs one interpreter start —
and both the guard and a failure are warn-only: a host with no sibling
periculum checkout has no corpus to run and gets a note, not a failed
install.

Until it exists, a host python cell **skips** — `SKIPPED_INFRA` with
`reason=image_runtime_missing`, naming the script that builds it.
Nothing falls back to the vendored citation trees
(`reference/Reticulum`, 1.3.5), because a figure produced against a
stack nobody deploys is worse than a skip; a cell that wants the
sources our comments quote asks for them per node with
`python_runtime = "reference"`. The pin is in the directory NAME for
the same reason: a pin bump is then a venv that does not exist yet, so
it reads as that named skip rather than as a run quietly served by the
version before the bump.

### The BLE room needs a patched btvirt

The `ble_room_*` cells put N virtual LE controllers on one emulated air
so that N `lnsd` daemons can prove BLE mesh formation with no boards.
The emulator is bluez's `btvirt`, which Debian does not package, and
which **cannot be a stock build**: bluez 5.82 and older report the
central's connection handle to the peripheral and forward ACL data
under the sender's handle, so every room stalls from the second
concurrent link (N >= 3) and the emulator's bug is measured as a mesh
finding. Upstream commit `4ff7deaf8c` fixes it; with it the ladder is
green to N = 16, which is the stock emulator's own `MAX_BTDEV_ENTRIES`
ceiling (a third patch raises it, below).

The fix is not the only thing a stock build gets wrong. btvirt reports a
legacy advertiser only at two instants, when a scanner enables scanning
and when the advertiser enables advertising, where a real radio repeats
its advertisement every interval. On stock btvirt a scan window therefore
sees a peer only if the peer was advertising at the instant the window
opened, and any test that bounds a re-sighting measures the emulator's
phase: a redial guard test was red 4 of 5 runs without `btmon` attached
and green 5 of 5 with it (2026-10-10). A local patch, tag
`adv-every-interval`, re-sends the report every advertising interval
(1280 ms for a `bluetoothd` advertiser); its header in `scripts/patches/`
carries the before/after trace and the runs that justify it.

`just install-ci` therefore builds btvirt itself
(`scripts/install-btvirt.sh`): it fetches the bluez source matching this
host, applies the vendored upstream patch and then the three local ones from
`scripts/patches/`, and installs the binary together with a provenance
sidecar, `/usr/local/bin/btvirt.provenance`, whose first non-comment line
every room prints as `origin=`:

```
BLE_ROOM_BTVIRT bin=/usr/local/bin/btvirt bytes=896816 origin="bluez 5.82-1.1 + upstream 4ff7deaf8c + local adv-every-interval, max-btdev-24, deferred-fd-free; built 2026-10-24, md5 2484faa5cfe1e5defb5f04763a8ac87a"
```

A sidecar that names `4ff7deaf8c` without `adv-every-interval`,
`max-btdev-24` or `deferred-fd-free` is a bench provisioned before that local
patch; `--check`
fails it and the installer rebuilds it.

A room with no sidecar still runs and says `origin=unrecorded`, which
is a result nobody can attribute afterwards. Verify a bench without
installing anything with `bash scripts/install-ci.sh --check`. The build
is skipped on a host that already holds the patched binary, and it warns
rather than fails when a prerequisite is missing — only the bench that
runs the room needs it. The four prerequisites beyond the binary (the
`hci_vhci` module, a running system `bluetoothd`, no BLE MIDI GATT
service, `cap_net_raw` on `btmon`) are listed in
`scripts/install-ci.sh` at the btvirt step; nothing can provision them
for you.

Each patch is applied by content, not by version number, each local one
against the tree the patches before it left. Before touching the source tree
the script asks `patch --dry-run` which of three states it is in: the fix reverse-applies cleanly (already there — leave it
alone), it applies forward (stock — apply it), or neither (refuse,
rather than build an emulator nobody can name). No released bluez known
here carries the fix yet, 5.82 being the newest and the commit dated
after it, so the first release that ships it will simply be recognised
and need no change to this script.

Both halves run with no root, no network and no compiler under `just
btvirt-selftest` (in `guards` and `fast`, 0.12 s): the patch step
against a bluez source tree synthesised from the vendored patches' own
pre-images, and the provenance checker against each way the sidecar can
lie. What the synthesised tree cannot answer is whether the patch still
fits *Debian's* bluez — a patch and a fixture derived from it drift
together — and that is what `patch --dry-run` answers at provisioning
time.

**Twenty-four controllers is the ceiling.** Stock `MAX_BTDEV_ENTRIES`
is 16 in bluez's `emulator/btdev.c`, in the 5.82 this host builds from
and in upstream master (read 2026-09-27 at bluez HEAD `8b4a4176`):
`btvirt -L -l16` runs, `-l17` exits at once with "Failed to open Virtual
HCI device". Until 2026-10-13 that made periculum's
`regression/ble_room_20` an `[unsupported]` infra skip. The local patch
`max-btdev-24` raises the constant to 24, the number periculum's
`MAX_CONTROLLERS` validates rooms against, so the room can now run
`ble_room_20`: on the patched binary `-l20` brings up twenty
controllers, `-l25` fails as `-l17` did, and `ble_room_20` was green
three of three with 380 of 380 ordered probes each (periculum order
701). A bench rebuilt from a tree without the patch drops back to
sixteen, and the cell skips as infra again.

**The event loop frees a removed fd only after its batch.** Stock
`mainloop_remove_fd` (bluez `src/shared/mainloop.c`) freed an fd's data at
once, also from inside a callback `mainloop_run` was still dispatching,
while the loop's local copy of that epoll batch still held the pointer.
Under timer-fd churn a later entry of the batch read released memory and
btvirt died: `scripts/btvirt-churn.sh` measured 50 SIGSEGV exits in 3625
iterations on the three-patch binary. The local patch `deferred-fd-free`
marks such data removed, skips it for the rest of the batch and frees it
afterwards; on the rebuilt binary the same bench drove 12000 iterations
with no exit (2026-10-24, order 955).

## Concurrent test protection

Two scenario runs on the same machine fight over Docker container names
and USB serial handles. To prevent that, periculum acquires a
process-wide file lock on `~/.local/state/leviculum-ci/test.lock` before
bringing any node up.

Single invocation: transparent. No extra output.

Two simultaneous invocations: the second exits within a second with
a multi-line `[leviculum]` message naming the current holder —
pid, started time, cwd, optionally the test-name filter. Example:

```
[leviculum] Another integration test is already running.
[leviculum] Current holder:
[leviculum]   pid=12345
[leviculum]   started=2026-04-14T02:01:33
[leviculum]   pkg=periculum
[leviculum]   binary=periculum
[leviculum]   cwd=/path/to/leviculum
[leviculum] Wait for it to finish or stop that process, then retry.
```

On-demand Tier 2 / scheduled Tier 3 runs that collide with a manual test
drop a marker file at `~/.local/state/leviculum-ci/lock-contention`;
the runner scripts read the marker, classify the run as SKIPPED
(not RED), and delete it. No false-alarm pages.

### Which kind of contention (Codeberg #309)

The marker is not a flag: it carries periculum's verdict on the process
holding the lock, plus that process's identity.

| Field | Meaning |
|-------|---------|
| `verdict=` | `running`, `suspected_wedge`, `misrecorded`, `unattributable` |
| `suspect=` | `true` for every verdict but `running` |
| `holder_pid=`, `holder_age_secs=` | who is holding it, and for how long |
| `detail=` | periculum's sentence about the holder, with what to inspect |

A holder alive past 24 hours has by definition starved at least one
nightly, and one whose recorded identity the kernel disagrees with is a
bug shape nothing else can see. Both reach the ledger under their own
token, so the case worth acting on is greppable:

```
<iso> tier3 SKIPPED lock-held    verdict=running         holder_pid=… holder_age_secs=… <log>
<iso> tier3 SKIPPED lock-suspect verdict=suspected_wedge holder_pid=… holder_age_secs=… <log>
```

Neither is RED. The verdict is a heuristic over metadata — a genuinely
enormous run looks like a wedge — and the contender never touches the
lock, so a false accusation would cost somebody killing a healthy
nightly. What changes is what the ledger says and, in
`scripts/run-tier3.sh`, whether the notification is `normal` or
`critical`.

**The marker, not the exit code, is what the runners branch on.**
periculum also carries the distinction in its exit status (2 for an
overlap, 4 for a suspect holder), but `run-tier2.sh` and `run-tier3.sh`
reach periculum through `just extensive` / `just nightly`, and the
`nightly` recipe rewrites its status to 1 whenever an LNode's firmware
could not be verified. The exit code is therefore a corroborating signal
there, and it is also the only channel that cannot say WHO.
`scripts/run-tier3-hw.sh` calls periculum directly and uses the codes for
one decision only: 2 and 4 both mean "look at the marker".

`scripts/test-lock-contention.sh` (`just lock-contention-selftest`) and
the contention cases in `scripts/tier3-hw-selftest.sh` hold this against
stubbed markers; neither needs a build, docker or the rig.

### Inspecting the lock

```
cat ~/.local/state/leviculum-ci/test.lock     # current (or last) holder
ls  ~/.local/state/leviculum-ci/lock-contention  # marker if present
```

### Force-release

Not applicable. The kernel releases the flock the moment the holding
process closes its fd — on clean exit, panic, SIGINT, SIGKILL, and
even host reboot. There is no TTL, no heartbeat, no manual cleanup
path. A stale `test.lock` file on disk after a reboot is self-
healing: the next invocation opens it, flock succeeds immediately
(kernel state is empty post-reboot), and the stale content is
overwritten.

### Scope

The lock protects only scenario runs. Unit tests in `leviculum-core`,
`leviculum-std`, `leviculum-ffi`, `leviculum-proxy`, and
`leviculum-cli` do not acquire it — they parallelise freely with an
in-progress scenario run. `periculum validate` and `periculum list`
do not acquire it either: they read scenario files and touch no node,
container or radio.

### Filesystem requirement

Local filesystem only. `flock` semantics over NFS / sshfs are
implementation-defined. If your `$HOME` is on a network filesystem,
the lock behaviour is not guaranteed. This is a single-developer
dev-box tool; not an issue in practice.

## Hardware test profiles (Tier 3)

Tier 3 runs the periculum `hardware/` corpus over USB-attached
embedded devices. Different scenarios need different subsets of the
attached boards; the rest must not transmit, so their RF activity does
not contaminate the run.

**No USB-hub power switching.** Every board stays permanently powered
and passed through to the VM. RF isolation of non-participating
firmware nodes is done in software: the runner pushes `radio_silent`
over serial to every discovered board it did not bind. Per-port power
cycling correlated with hamster hardware-watchdog freezes (proven
2026-06-15) and was removed, together with the `usbhub-helper` and its
libvirt-passthrough caveats.

Which individual boards exist on this bench is site data and lives in
periculum's `rig.toml` (override with `$PERICULUM_RIG`). What *kind* of
board each is — how it is recognised over USB, which port carries which
role, what it can be asked to do — lives in `periculum/devices/*.toml`
and is the same everywhere. A scenario names the set of boards it needs:

```toml
profile = "rnode_lnode_pair"
```

which is resolved against the rig file. A scenario needing more boards
than the bench holds is `SKIPPED_INFRA` with a reason naming what was
missing — never RED. An absent board is not a protocol result.

### Firmware identity

Before any hardware scenario runs, `scripts/flash-lnodes-from-head.sh`
flashes every attached LNode from the current commit and reads its
`[FW_BUILD]` banner back over the debug serial to confirm the board
really runs that commit. A board whose firmware cannot be confirmed
makes the tier RED and is named in the verdict
(`firmware_unverified=<vid:pid>`): a run against unknown firmware must
never be silently trusted. This step is leviculum's, not periculum's —
periculum tests whatever firmware it finds and leaves board preparation
out of scope on purpose.

### Device-vanish watchdog

`scripts/run-tier3-hw.sh` polls `lsusb` once a second for the whole run,
cross-checks every sub-baseline reading against sysfs, and records one
journal line per event — every vanish and every return, not one latched
line per board. Under VFIO controller passthrough the host cannot inject
a phantom VM-side disconnect, so a board that leaves the bus really left
it; what that means, though, is decided afterwards. A disconnect
periculum's own `BOARD_RESET` lines say it commanded (it reboots every
bound board per scenario) is *accounted* and never RED. An unaccounted
one forces RED with the board named (`board_vanish=<vid:pid>
cause=<what the witness supports>`), and every scenario verdict from the
vanish onwards is untrusted. The cause token is read off the board's
debug witness or reads `cause=unknown`; it is never asserted.

The journal also records what the board's own witness cannot see,
because a board that loses power writes nothing:

* **where each board sat** — its USB bus path and the hub it hangs off,
  snapshotted at baseline while the whole rig is still present, and
  quoted back on the vanish line (`last_paths=`, `last_hubs=`). The RED
  banner turns that into a per-hub count, and says so when more than one
  board was lost on a single hub: that is the shape a hub or power event
  has, and independent firmware failures do not have it.
* **what the kernel said** — the `usb`/`hub` lines about those paths,
  taken at the moment of the vanish (`kernel ... msg=`). `dmesg` is a
  ring buffer that rolls over long before anyone reads a nightly, and
  `USB disconnect` versus `disabled by hub` or an over-current report is
  the whole difference between a board fault and a hub fault. An
  unreadable buffer is recorded as `unavailable reason=...`, never as
  silence.

Both were added after the 2026-08-12 run (Codeberg #251) lost two LNodes
four minutes apart while a third board on another hub ran on, and left
no artefact able to say whether one hub had dropped out or two firmwares
had failed.

## Troubleshooting

| Symptom | Action |
|---------|--------|
| Tier 1 never seems to run | It doesn't run itself. Type `just standard`. Nothing has started it automatically since the post-commit hook went (2026-08-07). |
| Notification never arrived | Expected: nothing that currently runs calls `notify-send` (see Notifications above). Check `last-results.txt`. |
| Tier 1 spuriously red | Check log; if Docker is involved, ensure no leftover containers (`docker ps -a`) |
| Timer didn't fire | `systemctl --user list-timers`, then `journalctl --user -u leviculum-ci-nightly.timer`. The nightly is the only timer this installer enables; Tier 2 has no timer. |
| Tier 2 looks like it never runs | It doesn't, unless started: `systemctl --user start leviculum-ci-tier2.service`. `scripts/ci-status.sh` prints how long it has been. |
| Disk filling up | Logs auto-rotate (14d/60d); the build directories do not — see [Keeping the build directories bounded](#keeping-the-build-directories-bounded). `just sweep` caps both workspaces, `just sweep 20GB 2GB` harder. Tier 1's own directory is separate: `cargo clean --target-dir ~/.cache/leviculum-ci-target`. Never the sccache. |
