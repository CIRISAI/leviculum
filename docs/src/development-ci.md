# CI Pipeline

Two things run tests here, and they are not the same thing. Four
local tiers with different time budgets and triggers automate the
test discipline mandated by `CLAUDE.md`, on the developer's machine —
no GitHub Actions. On the forge, Woodpecker runs a smaller set on
every push, because the local tiers are hooks a fresh clone does not
have and `--no-verify` switches off. The tiers come first; the forge
pipelines are below them.

## Tiers

| Tier | Name | Trigger | Budget | Test scope |
|------|------|---------|--------|------------|
| 0 | `fast` | pre-push hook | ~3 min | fmt + clippy (host + nrf firmware workspace, both BSPs) + the firmware stack-frame gate + rustdoc gate + the [third-party notice guard](concepts/licensing-and-notices.md) + the pre-push guard selftest + workspace lib tests |
| 1 | `standard` | on demand: `just standard`, once per batch | ~15 min (first run: 20-40 min cold compile) | Tier 0 + core/tests + ffi + proxy + rnsd_interop + TCP-hub endurance smoke soak (see [Soak and endurance](soak-and-endurance.md)) + the `status_parity` two-daemon suite + the ignored-test census |
| 2 | `extensive` | on demand: `systemctl --user start leviculum-ci-tier2.service` | ~30-90 min | Tier 1 + the periculum `conformance/` and `regression/` corpora (docker) |
| 3 | `nightly` | systemd timer 02:00 daily | ~2-6h | Tier 2 + LNode flash-from-HEAD + the periculum `hardware/` corpus |

Each tier runs everything from the lower tiers as well, so a green
nightly proves the entire stack.

## What the forge runs

Three Woodpecker workflows on `ci.codeberg.org`, all in
`.woodpecker/`:

| File | Fires on | Runs |
|------|----------|------|
| `ci.yml` | every push, every pull request, manual | `just ci-gate` — fmt, clippy over all targets, the workspace lib tests |
| `commit-trailers.yml` | every push, every pull request, manual | `scripts/check-commit-trailers.sh` over the pushed range |
| `nightly.yml` | cron, plus pushes touching the packaging paths | the same gate, then the .deb + tarball build; the cron run also publishes |

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
the targets. Measured cost, cold: 3m23s including provisioning.

## What may be published

The forge gate is `fmt`, `clippy` and the workspace **lib** tests.
`rnsd_interop` — the suite that measures whether we still interoperate
with a Python-RNS peer, which is half of Priority 1 — runs in neither
forge pipeline and cannot: it needs the `reference/Reticulum`
submodule and a `python3`, and both pipelines clone with
`submodules: false` on purpose, which is the property
`just check-plain-clone` exists to hold (Codeberg #300). Fetching the
submodule into the release path would undo exactly that.

So the interop verdict is **imported rather than re-derived**
(Codeberg #312). The tier-2 nightly already runs the whole workspace,
with submodules, over a fresh clone pinned to `origin/master`. When
that run is green it pushes a lightweight ref at the commit it tested:

```
refs/nightly/green/<YYYYMMDDTHHMMSSZ>  ->  <tested commit>
```

and `scripts/publish-nightly.sh` refuses, before it touches the forge,
any commit those refs do not cover. Three conditions, and a refusal
always names which one failed:

| Condition | Meaning |
| --- | --- |
| `NO-SIGNAL` | there is no `refs/nightly/green/*` on the remote, or it could not be read |
| `NOT-COVERED` | no green ref names this commit or a descendant of it — no nightly has seen this code |
| `TOO-OLD` | the newest covering ref is older than the staleness bound (72 h) |

The 72 h bound is measured, not assumed: over 2026-08-22..09-22 the
nightly timer produced 27 runs with a median gap of 24 h, every gap
but one at or under 54.4 h, and one 96 h gap (2026-09-04 to 09-08)
which is precisely the case the bound exists to stop. When the forge
publishes on the fallback cron rather than on the trigger described
below, the freshest ref it can read is normally the previous night's and
is already ~24 h old. 72 h therefore accepts the ordinary day plus one
missed night and refuses two.

### What fires the publish

The publish is fired by the green ref, not by the clock. Since
2026-09-24 the reviewer host polls the forge every ten minutes and, as
soon as a new `refs/nightly/green/*` appears, fires the nightly cron
through the Woodpecker API (`lev-nightly-publish-trigger`, reviewer-host
tooling: it is not in this repo, and the API token stays on that host on
purpose). The scheduled cron, `0 4 * * *` UTC, remains as a fallback
rather than as the normal path; both firing on the same day is harmless,
because the release is rolling and `scripts/publish-nightly.sh` writes
the same `nightly` tag with its assets overwritten. Ordering is the
whole point of the change: pipelines 455 (2026-09-23) and 458
(2026-09-24) both refused at the publish gate because the 04:00 cron ran
before the nightly host had pushed that day's green ref. The code was
fine and the night had been green; the signal simply was not there yet.

That refusal is also what an operator sees when the trigger did not
fire. The publish step refuses with `NO-SIGNAL` when no green ref can be
read from the remote at all, and with `NOT-COVERED` — printing "the
newest green ref is ..." and "is neither that commit nor an ancestor of
it" — when refs exist but none of them names this commit or a
descendant of it, which is what a night that has not pushed yet looks
like from the publish step. Neither is a build failure and neither
needs a code change:
`bash scripts/check-nightly-green.sh --commit <sha>` answers which ref
the forge can see, and the fix is to wait for the night's ref to land
and then start the nightly cron by hand from the Woodpecker UI
(Repo → Settings → Crons) if the trigger has not done it first.

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
build-directory sweeper `just sweep` needs, and the state dir.
Re-running is safe.

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
`NodeCoreBuilder::build_boxed` allocates first and configures through the
box, which drops that frame to 12 672 B.

The gate prefers `arm-none-eabi-objdump` and falls back to the rustup
`llvm-tools` `llvm-objdump`; `install-ci.sh` installs the latter.

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
