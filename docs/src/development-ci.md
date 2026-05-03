# CI Pipeline

A self-hosted CI pipeline runs entirely on the developer's machine.
Four tiers with different time budgets and triggers automate the
test discipline mandated by `CLAUDE.md` — no GitHub Actions, no
external runners.

## Tiers

| Tier | Name | Trigger | Budget | Test scope |
|------|------|---------|--------|------------|
| 0 | `fast` | pre-push hook | ~3 min | fmt + clippy + workspace lib tests |
| 1 | `standard` | post-commit (background) | ~15 min (first run: 20-40 min cold compile) | Tier 0 + core/tests + ffi + proxy + rnsd_interop |
| 2 | `extensive` | systemd timer 12:30 + 18:30 daily | ~30-90 min | Tier 1 + integ Docker tests |
| 3 | `nightly` | systemd timer 02:00 daily | ~2-6h | Tier 2 + LoRa hardware tests |

Each tier runs everything from the lower tiers as well, so a green
nightly proves the entire stack.

## Installation

One command, idempotent:

```
just install-ci
```

It installs `git` hooks (via `core.hooksPath = .githooks`), runner
scripts, systemd user units, the separate cargo target dir, and the
state dir. Re-running is safe.

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

## Notifications

`notify-send` is called on every Tier 1/2/3 result. Failures use
`-u critical` (sticky until dismissed); successes use `-u low`.

**Prerequisite:** `notify-send` needs `DBUS_SESSION_BUS_ADDRESS` and
`XDG_RUNTIME_DIR` in the user systemd manager environment, which
exists only when you have a logged-in graphical session. On a
headless server, notifications are silently dropped — inspect
`~/.local/state/leviculum-ci/last-results.txt` instead.

## Stale-block on push

`pre-push` blocks the push if Tier 2 hasn't run successfully in
≥ 10 commits or ≥ 24 hours (warning at 5 commits / 8h). To override:

```
git push --no-verify
```

To clear the block normally, run `just extensive` once (or wait for
the next scheduled run).

## Logs

Location: `~/.local/state/leviculum-ci/`

| File | Contents |
|------|----------|
| `last-results.txt` | one-line tally per run (`<iso-timestamp> <tier> GREEN/RED <log-path>`) |
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

## Convention: `#[ignore]` is for hardware-dependent tests only

In `reticulum-integ`, the CI tier separation depends on `#[ignore]`:

- Tier 2 runs `cargo test` with default behavior (skips ignored).
- Tier 3 adds `--include-ignored` to pick up exactly the LoRa
  hardware tests.

If you mark a slow but non-hardware test as `#[ignore]`, it ends up
in nightly Tier 3 alongside the LoRa tests — wrong tier. Use a
Cargo feature (e.g. `slow-tests`) for that case instead. Currently
the invariant `#[ignore] tests == #[serial(lora)] tests` holds; keep
it that way.

## Concurrent test protection

Two `cargo test -p reticulum-integ` invocations on the same machine
fight over Docker container names and USB serial handles. To prevent
that, every integ test silently acquires a process-wide file lock on
`~/.local/state/leviculum-ci/test.lock` as the first step inside
`TestRunner::new()`.

Single invocation: transparent. No extra output.

Two simultaneous invocations: the second exits within a second with
a multi-line `[leviculum]` message naming the current holder —
pid, started time, cwd, optionally the test-name filter. Example:

```
[leviculum] Another integration test is already running.
[leviculum] Current holder:
[leviculum]   pid=12345
[leviculum]   started=2026-04-14T02:01:33
[leviculum]   pkg=reticulum-integ
[leviculum]   binary=reticulum_integ-abc123def
[leviculum]   cwd=/home/lew/coding/libreticulum
[leviculum] Wait for it to finish or stop that process, then retry.
```

Scheduled Tier 2 / Tier 3 runs that collide with a manual test
drop a marker file at `~/.local/state/leviculum-ci/lock-contention`;
the runner scripts observe the marker, classify the run as SKIPPED
(not RED), send a `normal` (not `critical`) notification, and delete
the marker. No false-alarm pages.

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

The lock protects only `reticulum-integ` tests. Unit tests in
`reticulum-core`, `reticulum-std`, `reticulum-ffi`,
`reticulum-proxy`, and `reticulum-cli` do not acquire it — they
parallelise freely with an in-progress integ run. Pure-parse unit
tests inside `reticulum-integ` (e.g. compose YAML validation,
radio-config wire round-trips) also don't acquire the lock because
they never call `TestRunner::new()`.

### Filesystem requirement

Local filesystem only. `flock` semantics over NFS / sshfs are
implementation-defined. If your `$HOME` is on a network filesystem,
the lock behaviour is not guaranteed. This is a single-developer
dev-box tool; not an issue in practice.

## Hardware test profiles (Tier 3)

Tier 3 (`just nightly`) runs LoRa hardware tests over USB-attached
embedded devices.  Different tests need different subsets of the
attached hardware powered on; the rest must stay off so that
their RF activity does not contaminate the run.

The mapping of devices to USB-hub ports lives at
`reticulum-integ/profiles/devices.toml`.  Each LoRa test descriptor
in `reticulum-integ/tests/lora_*.toml` may carry a
`profile = "..."` field that names one of the profiles defined in
`devices.toml`.  Tests without that field default to the `default`
profile (= every device powered on).

Active profile assignments on master HEAD:

| Test descriptor | Profile | Active devices |
|---|---|---|
| `lora_lncp_bidir.toml` | `lora_lncp_bidir` | t-beam-1, t-beam-2 |
| `lora_lnode_lncp_bidir.toml` | `lora_lnode_lncp_bidir` | pocket-v2, t114 |
| (all other `lora_*.toml`) | `default` (implicit) | all five devices |

Adding a new profile means: (a) declare the device subset in
`devices.toml`, (b) add `profile = "<name>"` to the test descriptors
that need it.  No code changes required.

The `default` fallback is intentional: a test without an explicit
profile keeps the historic behaviour of "all devices powered on,
nothing power-managed", so existing tests are not at risk of
silent regression from the introduction of the profile system.

USB-hub power switching is performed by the hamster-side helper
script (`scripts/usbhub-helper`) over a restricted SSH key from
schneckenschreck.  See `scripts/run-tier3-hw.sh` for the
orchestration logic.

### libvirt USB-passthrough caveat

When a device is disabled via the helper, hub power is genuinely
cut on hamster — no power, no LoRa TX/RX, no MCU activity.  But
the schneckenschreck VM keeps the cached USB-passthrough handle:
`/dev/serial/by-id/...` symlinks and `lsusb` entries persist
after the disable.  **`ssh hamster usbhub-helper status` is the
source of truth, not VM-side device enumeration.**  The wrapper
queries hamster after each profile transition and emits the
authoritative state into the per-test log as `[CI_HW] hamster_status=...`.

## Troubleshooting

| Symptom | Action |
|---------|--------|
| post-commit looks dead | `ps -ef | grep run-tier1` and check the latest log file |
| Notification never arrived | Check `last-results.txt`. On headless boxes notifications are dropped. |
| Tier 1 spuriously red | Check log; if Docker is involved, ensure no leftover containers (`docker ps -a`) |
| Timer didn't fire | `systemctl --user list-timers`, then `journalctl --user -u leviculum-ci-tier2.timer` |
| Stale-block annoying | `git push --no-verify` (one-shot) or run `just extensive` |
| Disk filling up | Logs auto-rotate (14d/60d), but `~/.cache/leviculum-ci-target/` can grow large — clear with `cargo clean --target-dir ~/.cache/leviculum-ci-target` |
