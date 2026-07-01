# Testing — Developer Quick Reference

One-page orientation. See [CI Pipeline](development-ci.md) for the
automation details.

## TL;DR

- **Writing code**: run `cargo test -p <crate-you-touched>` as you go.
- **Committing**: nothing manual. Tier 1 runs in the background.
- **Pushing**: Tier 0 runs automatically and blocks on fail.
- **Daily**: Tier 2 (12:30, 18:30) and Tier 3 (02:00) run via systemd.
- **Suite overview**: `just status`.

## The four tiers

| Tier | When                              | Command         | Time      | Scope |
|------|-----------------------------------|-----------------|-----------|-------|
| 0    | on `git push` (hook)              | `just fast`     | ~3 min    | fmt + clippy + workspace lib tests |
| 1    | after `git commit` (hook, background) | `just standard` | ~15 min (40 min cold[^cold]) | Tier 0 + core/tests + ffi + proxy + rnsd_interop |
| 2    | 12:30 & 18:30 daily (systemd timer) | `just extensive` | 30–90 min | Tier 1 + Docker integ suite |
| 3    | 02:00 daily (systemd timer)       | `just nightly`  | 2–6 h     | Tier 2 + LoRa hardware tests |

Each tier includes every lower tier, so a green nightly proves the
whole stack.

[^cold]: The CI uses its own `CARGO_TARGET_DIR` at
`~/.cache/leviculum-ci-target` so IDE builds and CI builds don't
fight over the same incremental cache. The first Tier 1 run after
`install-ci.sh` compiles the workspace from scratch (~40 min);
subsequent runs are incremental (~15 min).

Results go to `~/.local/state/leviculum-ci/last-results.txt`:
`GREEN` = passed, `RED` = failed (sticky `notify-send` alarm),
`SKIPPED` = deferred because another test held the lock (see
"Concurrent runs" below).

## While writing code

Fast feedback. Run only what you changed:

```sh
cargo test -p leviculum-core --lib   # touched core lib code
cargo test -p leviculum-std          # touched std
cargo clippy -p leviculum-core       # clippy for one crate
cargo fmt                             # apply formatter (not --check)
```

End of a work session with unsynced work? Don't wait for the
post-commit hook:

```sh
just standard                        # manually trigger Tier 1 (~15 min)
```

This is the "15-minute-budget" check that CLAUDE.md expects after
every task.

**Never** run the full integ suite casually:

```sh
# DON'T do this without a reason — Docker tests collide with anything
# else using integ infra on the box.
cargo test -p reticulum-integ
```

If you must, use `just extensive` which is lock-protected and
serialised properly.

## Before pushing

Nothing to type. `git push` triggers `.githooks/pre-push` which
runs `just fast` (Tier 0). A red Tier 0 aborts the push — fix, stage,
and push again.

Also checked: Tier 2 staleness.

- **WARN** at 5 commits or 8 h since last GREEN Tier 2 — push goes
  through with a visible warning, no action required.
- **BLOCK** at 10 commits or 24 h — push aborted. Run `just
  extensive` once to clear, or `git push --no-verify` to override.

## After committing

`.githooks/post-commit` fires `scripts/run-tier1.sh` in the
background. You'll get a desktop notification (~15 min later) with
GREEN or a critical RED + log path. Multiple commits in a row
coalesce — no parallel re-runs.

## Checking state

```sh
just status                                  # last result per tier
just logs                                    # tail most recent Tier 1 log
cat ~/.local/state/leviculum-ci/last-results.txt   # full history
```

## LoRa hardware tests

Tier 3 only. Requires two Heltec T114 boards + two RNode radios
connected via USB. Manual runs:

```sh
just flash                        # flashes ALL attached T114s; touch-free
                                  #   since the Bug #13 firmware change.
                                  #   Double-tap RESET only if the runner
                                  #   prompts you (crashed-firmware fallback).
just flash-one /dev/ttyACM3       # flash one specific T114 (A/B testing)
just nightly                      # full Tier 3 run
```

A single LoRa test in isolation:

```sh
cargo test --release -p reticulum-integ --test-threads=1 -- \
    --exact executor::tests::<name> --ignored --nocapture
```

All hardware tests are gated behind `#[ignore]`; `--ignored`
unlocks them. `#[ignore]` is reserved for hardware tests — don't
use it for slow-but-CPU-only tests (wrong tier).

## Concurrent runs

Only one integ test process can run at a time — Docker names and
USB handles would otherwise collide. A second invocation exits in
under a second:

```
[leviculum] Another integration test is already running.
[leviculum] Current holder:
[leviculum]   pid=12345
[leviculum]   started=2026-04-14T02:01:33
[leviculum]   pkg=reticulum-integ
[leviculum]   binary=reticulum_integ-abc123def
[leviculum]   cwd=/path/to/leviculum
[leviculum] Wait for it to finish or stop that process, then retry.
```

A scheduled Tier 2 / Tier 3 that hits this case logs `SKIPPED`,
not `RED`, and sends a normal (not critical) notification. **No
action needed** — the next scheduled slot runs normally. In
practice this means: if you're doing late-night hardware work and
the 02:00 nightly fires, it silently defers. You don't need to
stop it.

Unit tests in other crates (`leviculum-core`, `leviculum-std`,
`leviculum-ffi`, `leviculum-proxy`, `leviculum-cli`) run in
parallel with a held integ lock — they never touch the integ
infrastructure.

## Installing / updating the CI

```sh
just install-ci
```

Idempotent. Installs git hooks, systemd user units, state dirs,
separate cargo target dir. Safe to re-run after pulling.

## Golden rules

- Tests are never flaky. A failure is a real bug — diagnose and
  fix at the root, don't retry until green.
- Don't commit while tests are red.
- `#[ignore]` is only for hardware-dependent tests. For
  CPU-expensive non-hardware tests, use a Cargo feature flag.
