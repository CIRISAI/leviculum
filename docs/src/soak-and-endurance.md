# Soak and endurance

Two independent lines of evidence back the claim that `lnsd` runs as a stable,
long-lived transport node: an in-repo soak test that runs as a CI gate, and a
permanent node in the public Reticulum mesh. A third measure, poison-tolerant
locking, keeps a task panic under a lock from crashing the whole daemon.

## In-repo soak: the TCP-hub endurance test

`leviculum-std/tests/rnsd_interop/loadtest_tcp_hub_tests.rs` (Codeberg #101)
boots the real `lnsd` binary as an internet-facing transport hub, drives
sustained load plus connection churn against it, and samples the hub process's
`/proc/<pid>` RSS and open-fd count throughout. Because the hub is a separate
process, those samples are meaningful.

### Topology

```text
  N raw TCP clients ─┐            ┌─ sink (Single dest, TCP client)
  churn connections ─┼─▶  lnsd  ──▶┘
                     ┘  (transport hub)
```

A pool of steady TCP clients plus a set of churn workers (connections opened,
used, and closed in a tight loop) push sequence-numbered, encrypted single
packets through the hub to a sink daemon. The sink decrypts and folds each
`(client, seq)` into a per-source set, so delivery is verified exactly, not
sampled.

### What it asserts

On every run `report_and_assert` enforces:

- **100% delivery.** TCP is lossless, so every packet a client sends must arrive
  at the sink, contiguous and without duplicates. Any shortfall is a real hub
  bug, never noise. Connection-refused-under-load counts as zero-delivery, so
  backpressure failures cannot hide.
- **RSS plateau (no per-connection leak).** A steady population of connections
  legitimately costs memory, so growth from idle baseline to steady is expected.
  The leak signal is a *continuous climb* across the steady+churn phase, where
  thousands of connections are churned: the test compares the first vs second
  half of the steady-phase RSS samples and fails only when both a proportional
  and an absolute floor are exceeded, so a plateau with jitter never trips. A
  separate absolute ceiling over baseline is a runaway backstop.
- **fd bounded under churn, released after teardown.** Peak fd count must stay
  under `baseline + steady_conns + churn_workers + margin`, and after the
  clients close and drain, the count must fall back near baseline. A
  per-connection fd leak would blow past the ceiling and leave the end count
  elevated.
- **Clean hub log.** The hub's log is scanned for fatal/bad lines; expected
  churn-teardown lines are allow-listed, anything else fails the run.

### Two variants

| Test | Default load | Runtime | When |
|------|--------------|---------|------|
| `loadtest_tcp_hub_smoke` | 24 conns / 5 s | ~15 s | Tier 1 CI gate, every commit |
| `loadtest_tcp_hub_soak`  | 200 conns / 60 s | minutes | on demand / heavier validation |

Both are `#[ignore]`d because they spawn the `lnsd` binary, which the
`leviculum-std` test build does not itself produce — the binary is built first.

### Running it

Use the entrypoint, which builds `lnsd` (release) so the test's `locate_lnsd()`
finds it, then runs the right variant:

```sh
bash scripts/run-soak.sh          # smoke (~15 s + build)
bash scripts/run-soak.sh --full   # heavy soak (minutes)
```

The script honours the ambient `CARGO_TARGET_DIR` so the binary lands where the
test looks, prints the effective parameters, and exits non-zero on failure. A
passing run ends with a `PASS:` block plus the `rss plateau:` and `fds:` lines.

### Tuning

The soak reads these environment variables (defaults shown are the heavy-soak
values; the smoke variant uses smaller ones):

| env | default | meaning |
|-----|---------|---------|
| `LOADTEST_CONNS` | 200 | steady concurrent TCP client connections |
| `LOADTEST_SECS` | 60 | steady + churn duration (seconds) |
| `LOADTEST_PKT_MS` | 50 | per-connection inter-packet interval (ms) |
| `LOADTEST_CHURN_WORKERS` | 16 | connections repeatedly opened/closed |
| `LOADTEST_CHURN_PKTS` | 4 | packets per churn connection before close |
| `LOADTEST_MAX_RSS_GROWTH_PCT` | 40 | max steady-phase RSS growth |
| `LOADTEST_MAX_RSS_ABS_MIB` | 300 | absolute RSS ceiling over baseline |
| `LOADTEST_DRAIN_SECS` | 20 | post-load drain window for the fd check |
| `LOADTEST_SAMPLE_MS` | 250 | RSS/fd/CPU sampler cadence |
| `LOADTEST_LNSD_BIN` | auto | explicit path to the `lnsd` binary |
| `LEVICULUM_DELIVERY_LOG` | unset | append one `DELIVERY` line per run to this file |

### Sweeping delivery against load

The assertion is binary — 100 % or the run is red — which is right for a gate and
useless for the question "at what load does the hub start to drop?". Codeberg
#208 recorded one 99.5454 % run at 128 connections / 15 ms during the #198 A/B
measurement, on a four-core host that was simultaneously running the measurement
harness, and could attribute it to neither the hub nor the machine: the gate runs
at 24 connections / 50 ms, and nobody had ever swept delivery against connection
count and rate.

Every run therefore prints, and with `LEVICULUM_DELIVERY_LOG=<file>` also
appends, one line — before the assertions, so red runs contribute too:

```text
DELIVERY test=lnsd_soak sent=377285 recv=375570 pct=99.5454 ci95=99.5234-99.5665 \
  conns=128 pkt_ms=15 secs=20 churn_workers=16 churn_conns=42 cores=4 \
  hub_cpu_pct=82.4 gen_cpu_pct=210.5 host_busy_pct=96.1
```

`ci95` is the Wilson 95 % interval for the counts on the same line (the project
rule that a delivery ratio is never printed alone), at four decimals because a
hub run's denominator is in the hundreds of thousands, where two significant
digits would erase the very shortfall the line records. The cell coordinates are
on the line because two runs at different `conns`/`pkt_ms` offered different
volumes and cannot be pooled. The three CPU figures are what make a cell
interpretable, and the middle one — the load generator and the sampler, i.e. the
harness itself — is there because that is the cost #208 could not account for.

`scripts/sweep-tcp-hub.sh` drives the grid and reads the matrix back out of the
log:

```sh
bash scripts/sweep-tcp-hub.sh                      # 4x3 cells, 3 runs each
SWEEP_CONNS="128 192" SWEEP_PKT_MS="15 10" bash scripts/sweep-tcp-hub.sh
bash scripts/sweep-tcp-hub.sh --summarize <log>    # re-read an earlier sweep
```

It refuses to start above `SWEEP_MAX_LOAD1` (default 1.5), because the hub, the
sink and the generator all run on the sweep host and any other workload there is
indistinguishable from the hub being slow — the precise ambiguity #208 is about.
A red cell does not stop the sweep; the distribution is the point. What the
matrix is read for:

- a cell below 100 % while nothing is saturated is a defect in the hub;
- a cell below 100 % only at or past saturation is a load ceiling, and the
  finding is that the gate should name where the cliff is.

The sweep itself has not been run yet; #208 stays open until it has.

### Where it runs regularly

The smoke soak is wired into the Tier 1 `standard` CI target (`Justfile`), which
is run once per batch of work, and again by `just extensive` and `just nightly`,
which depend on it — so a green soak is produced regularly and left on record.
Until 2026-08-07 a post-commit hook also ran it after every commit; that hook is
gone and Tier 1 is now started explicitly. See
[CI Pipeline](development-ci.md). The heavier `--full` soak is run on demand.

## Real-world production soak: the `miauhaus` node

A permanent `lnsd` transport node (`miauhaus`) runs continuously in the public
Reticulum mesh, not in a lab harness. It has operated multi-day continuous as a
routing transport node, carrying real announce and path traffic, and has
survived a host reboot with no crash or self-reset observed. This is operational
endurance evidence alongside the synthetic soak: the daemon holds up under real,
unscripted mesh traffic over long uptimes.

Figures are kept deliberately conservative — multi-day continuous operation with
no crash is what is directly observed and defensible; no precise uptime hours are
claimed here.

## Crash containment: poison-tolerant locking

Endurance is not only about not leaking; it is about a fault in one path not
taking down the whole process. The shared-state `std::sync::Mutex` sites in
`leviculum-std` were previously locked with `.lock().unwrap()`, so one task
panicking while holding a lock poisoned it and crashed every later locker —
turning an isolated task panic into a whole-daemon crash.

`MutexRecover::lock_recover()` (`leviculum-std/src/sync_ext.rs`) locks with
`lock().unwrap_or_else(PoisonError::into_inner)` and is applied uniformly to the
non-test std-mutex locks across the driver, interfaces, RPC, and event-log
paths. This is "continue degraded, do not crash," not "isolate one interface":
the dominant lock is the node-wide `core` mutex, held on every RPC, connect, and
dispatch, so recovery continues node-wide state that may be mid-mutation from the
task that panicked. The guarantee is that one task panic no longer cascades into
a whole-daemon crash that drops every peer the node routes for; the first such
recovery logs a `tracing::warn` so the degraded state is visible in the logs.
