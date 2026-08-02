# Client Tools

Leviculum ships command-line tools next to the daemon, and every tool
we will ever ship is governed by the same small set of rules. This page
records them for whoever adds the next tool: the counterpart rule, the
drop-in contract, the explicit permission to exceed the reference, the
case where a tool with no Python equivalent is the right deliverable,
the one situation in which our own tool must *not* be used, and the
marking rule for tools that can forge traffic.

The daemon-level half of this story — shared-instance IPC and config
compatibility, and why drop-in is a design goal rather than an accident
— is in [Python-RNS Compatibility](python-rns-compatibility.md). This
page extends the same property from the daemon to the clients.

## A counterpart for every reference tool

The reference stack ships a family of utilities under
`reference/Reticulum/RNS/Utilities/`. The rule is one `ln*` counterpart
per reference tool. The honest current state, as of 2026-08:

| Reference tool | Counterpart | State |
|---|---|---|
| `rnsd` | `lnsd` | Shipped. Drop-in at IPC and config level; see [Python-RNS Compatibility](python-rns-compatibility.md). |
| `rnstatus` | `lnstatus` | Shipped. Local-mode output is byte-parity-pinned against the reference by the 2×2 matrix `status_parity_matrix_2x2` (`status_parity_tests.rs:1000`); Periculum wiring is Codeberg #174. |
| `rncp` | `lncp` | Shipped (send, fetch, listen). |
| `rnprobe` | — | Missing; filed as Codeberg #173. |
| `rnpath` | — | Missing; filed as Codeberg #173. |
| `rnid` | — | Missing, not yet filed. |
| `rnx` | — | Missing, not yet filed. |
| `rnsh` | — | Missing, not yet filed. |
| `rnir` | — | Missing, not yet filed. |
| `rnpkg` | — | Missing, not yet filed. |
| `rnodeconf` | — | Missing, not yet filed. (`just flash`/`just flash-rnode` cover our own rig's flashing needs, but they are build tooling, not a counterpart.) |

The vendored 1.3.5 tree also carries `rngit` as a utility subpackage;
the counterpart rule covers it the same way, and it is likewise
missing and unfiled. "Not yet filed" rows are gaps in the issue
tracker, not decisions to skip the tool — file the issue when work on
one begins.

Alongside the counterparts we ship tools with no reference equivalent:
`lnstest` (test and diagnostics driver, see below), `lnomad` (NomadNet
browser; its reference counterpart is the NomadNet application rather
than an RNS utility), and `lblogd` (blog daemon). The counterpart rule
does not restrict these; the drop-in rule below does not apply to them
because there is no reference surface to be compatible with.

## Drop-in first

**With the reference tool's arguments, our tool behaves like the
reference tool.** Same flags mean the same thing, same exit codes,
and the output is something a script written for the Python tool can
still parse. This is the daemon-level drop-in property extended to
the clients, and it is what lets a comparison harness point one
driver at either stack.

The worked example is `lnstatus`: the renderer consumes the same
`interface_stats` dict a Python `rnsd` exposes, and feeding an
identical stats dict into `lnstatus` and `rnstatus` yields
byte-identical output (`lnstatus_render.rs:1-15`, pinned by the 2×2
parity matrix, which drives both clients against both daemons after
byte-identical controlled traffic, `status_parity_tests.rs:1-30`).
`lnstatus` mirrors the reference flag surface — `-a`, `-A`, `-P`,
`-l`, `-j/--json`, `-m/--monitor`, `-s/--sort`, the name filter — with
the reference meanings, because those flags and formats are the
compatible surface a user's muscle memory and a user's scripts depend
on.

Drop-in is judged against the vendored reference
(`reference/Reticulum`), which is the same source of truth the
protocol work measures against. When the reference tool's own output
changes between versions, the bridge parsers in Periculum absorb it —
compatibility of meaning, not a frozen byte format, per
[Wire Field Semantics](wire-field-semantics.md).

## Exceeding is allowed and wanted

Additive flags and richer output are welcome. The constraint is only
that the **reference-compatible surface stays reference-compatible**:
an extra flag must not change what an existing flag does, and extra
output must not break what a Python-tool script would parse.

Current examples: `lnstatus --instance_name`
(`leviculum-cli/src/lnstatus.rs:113-115`) selects a shared instance
by name where the reference tool only reads it from the config file,
and the planned structured state dump (the second half of Codeberg
#174) will expose internal tables `rnstatus` cannot show at all.
Both are additive: run `lnstatus` with exactly `rnstatus`'s
arguments and you get `rnstatus`'s behaviour.

One honesty note, because it is easy to get wrong in the other
direction: `-j/--json`, `-m/--monitor` and the announce/path-request/
link statistics flags are **not** exceedances — the reference
`rnstatus` has all of them (`rnstatus.py:685-706`), and ours mirror
them under the drop-in rule. Claiming reference-mirrored features as
our extensions would misstate where the compatible surface ends;
check the reference before calling a flag additive.

## New tools where testing gains from them

`lnstest` exists because Periculum needed a driver the Python tool
set does not offer — deterministic selftest phases (delivery,
ratchet, link) with machine-parseable summary lines
(`leviculum-cli/src/lnstest.rs:1-4`). That is the precedent: **when a
test cannot be written because no tool can express it, the tool is
the deliverable.**

The currently open examples — not a closed list — are:

- Codeberg #175: wire-level tools, a packet injector and a decoder
  with no Python equivalent.
- Codeberg #176: a structured event tap, so Periculum can assert on
  events instead of scraping container logs.

A new tool of this kind has no reference surface, so the drop-in rule
does not bind it; the [evidence rules](evidence-and-honesty.md) do. In
particular a test tool's output is a diagnostic indicator: it must
measure the production path and be observed in both states before its
green is believed.

## The rule a comparison must not break

A stack comparison drives the **same client against both daemons**.
That is the whole point of the drop-in property, and it cuts both
ways: our own client may not replace the reference tool in the very
tests that measure against the reference. Substituting "our better
tool" on one side smuggles config, cadence and timeout differences
into a result that claims to be about the stacks — the parallel-driver
failure described in
[Evidence and Honesty](evidence-and-honesty.md#reference-first-for-compatibility-bound-behaviour).

So: `lnstest selftest` pointed at either daemon is a valid A/B.
`lnstatus` against `lnsd` compared with `rnstatus` against `rnsd` is
not a stack comparison — it varies two things at once. The 2×2 parity
matrix (`status_parity_tests.rs:5-30`) is the shape that untangles
this: each client against each daemon, so client-render parity and
daemon-stats parity are separated instead of conflated. This is the
one place where "use our better tool" is wrong.

## A tool that can forge is marked as such

Anything that emits crafted frames — the planned packet injector of
Codeberg #175 first among them — refuses to run without an explicit
flag acknowledging that it forges traffic, and names itself in its
output so a capture containing forged frames is attributable. A
crafted frame in a mesh is indistinguishable from a real one by
design; the honesty has to live in the tool. No shipped tool forges
today; this rule binds the first one that does.

## Adding the next tool: the checklist

1. **Name and scope.** `ln*` counterpart of one reference tool, or a
   new testing tool per the precedent above. Check the tracker first
   (#173 covers probe and path query).
2. **Drop-in surface.** Implement the reference tool's flags with the
   reference tool's meanings and exit codes. Divergence from the
   reference's *internals* is fine under the
   [deviation rule](python-rns-compatibility.md#the-deviation-rule);
   divergence of the compatible surface is not.
3. **Parity evidence.** Pin the drop-in claim with a test that could
   fail — the 2×2 matrix of `status_parity_tests.rs` is the model.
4. **Periculum wiring.** Add a client manifest
   (`periculum/periculum/adapters/clients/*.toml`) and an output
   parser in the bridge (`periculum/periculum/src/bridge.rs`), so
   scenarios can drive the tool and assert on its output.
5. **Docs.** Guide page, man page, and both in `docs/src/SUMMARY.md`.
6. **Forgery marking**, if the tool can emit crafted frames.

## See also

- [Python-RNS Compatibility](python-rns-compatibility.md) — the
  daemon-level drop-in property this page extends, and the deviation
  rule.
- [Evidence and Honesty in Testing](evidence-and-honesty.md) — the
  A/B discipline the comparison rule enforces.
- [Wire Field Semantics](wire-field-semantics.md) — compatibility as
  meaning, not bytes.
