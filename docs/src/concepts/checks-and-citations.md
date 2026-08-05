# Checks That Are Actually Checks

A check makes two promises: that it *ran*, and that it *could have
failed*. A citation makes a third: that it still points at what it
claims. None is self-evident, and this codebase has broken all three —
silently, and for months at a time.

This page records the three mechanical guarantees that make those
promises verifiable, what they cost, and what they do not reach.

The underlying rule is in [Evidence and Honesty](evidence-and-honesty.md):
*a check you have never seen fail is not a check*. That page tells a
person what to do. This one is about the cases where the person forgot.

## The incidents, and which guarantee would have caught each

| Defect | Caught by |
|---|---|
| `execute_benchmark` ended in an unconditional `Ok(())` (`periculum/src/assertions.rs`); 6 of 10 recorded runs carried zero packets and reported GREEN | **neither** — a scenario step, not a Rust test |
| the sx1262 RX-extend guard tested IRQ flags its own latch mask had disabled (#144, fixed 26ce3a0) | **neither** — no test at all, and `exclude` (`Cargo.toml:33`) puts `leviculum-nrf` outside the workspace |
| `status_parity` `#[ignore]`d with a reason naming a procedure no script implements; never executed by any gate (#189) | **B** |
| 14 further ignored tests in `rnsd_interop` executed by nothing (#189) | **B** |
| 20 scenario steps produced a delivery figure no step asserted; GREEN at 70-90 % (#188, Periculum #25) | **neither** — scenario steps |
| the status-parity volume guard compared one interface, so a whole-inventory divergence stayed green (#177) | **A**, only if the author's negative control covers the whole inventory rather than the one interface they compared |
| drifted `file:line` citations — six across five concept documents in the 2026-07 manual audit (`leviculum-std/tests/doc_citations.rs:6`), sixteen across the whole book on the guard's first automated run | **C** |
| `reference/LXMF` sat twelve commits behind its gitlink for five weeks; every LXMF citation meant something other than it said | **C**, and the red `reference_lock` test that should have said so was itself unobserved — a **B** failure masking a **C** failure |

The last row is worth reading twice: the guarantees are not
independent. A rotted citation had a test attached, and that test ran
nowhere. **Guarantees that only report are worth what their observation
is worth.**

## Guarantee A — every pin carries its own negative control

A **pin** is a test that fixes a claim we rely on: a wire-field
semantic, a deliberate deviation, a measured protection level, a chosen
non-behaviour.

The rule: **a pin must contain an assertion that fails when the claim it
pins is broken**, in the same test, on every run. The pattern is in the
tree — `announce_signature_covers_reference_byte_order_on_the_wire`
(`leviculum-core/src/destination.rs:2085`) verifies its signature, then
drops the destination hash from the front and asserts that verification
now fails.

### The gate checks presence. Only the audit checks efficacy.

A registry gives the gate a citation. The strongest thing it can check
is that the cited assertion still exists where it says — drift
detection. **It cannot see a vacuous control**: one that verifies
against a random key, one behind an early return, one in a branch the
test never takes. Efficacy is checkable only by mutating the pin's
subject and confirming the pin dies.

**A is therefore not in force until the audit runs.** The registry gate
will land first because it is cheap; a tree that has only the registry
has drift detection on negative controls and nothing more, and should
not be described as having Guarantee A.

### Why mutation cannot be the per-batch gate

The blocking facts, in order: **`-j` is unsafe** here, so the pass is
serial — though not for the reason it first looks. Storage is not the
hazard: the suites take theirs from `tempfile::tempdir()`, and
`cargo-mutants` gives each job its own copy of the tree anyway. Ports
are. Both the mvr and `rnsd_interop` suites draw listeners from a
counter over a fixed band, 61000-65000, and that counter is explicitly
per-process — `PORT_COUNTER`
(`leviculum-std/tests/rnsd_interop/harness.rs:58`). It test-binds each
candidate, releases it, and hands it to its intended consumer to bind.
Between threads of one process that is sound. Two concurrent suite
processes start the same counter at the same base and race in exactly
the handoff window the comment above it describes. On top of a serial
pass come cold baseline builds and hang-mutant timeouts.

Measured on a 32-core host (musl, warm; the CI host has four cores,
where every figure below is worse): incremental rebuild after a content
change in `leviculum-core/src/transport.rs` ~1.65 s; the downstream
`leviculum-std --test mvr` binary ~1.8 s; `leviculum-core --lib`
build-and-run 4.3 s. Floor ~2 s per mutant. `cargo-mutants` generates
roughly 3-15 mutants per subject (its documented behaviour, not measured
here). At an *assumed* 200 pins that is 600-3000 mutants:
**realistically over an hour**, against a ~15 min batch budget. The
dominant term is the rebuild, which scales with the codebase, not with
the number of pins.

Two reach limits. `cargo-mutants` replaces a function body with a
guessed value, swaps binary operators, deletes unary ones, deletes match
arms a wildcard still covers, replaces match guards with `true` and
`false`, and deletes fields from struct literals that have a base
expression. It does not substitute literals and does not mutate consts —
where this project's semantics live: `PATHFINDER_RETRIES`
(`leviculum-core/src/constants.rs:115`). The #192 defect was
`retries: 0` where `PATHFINDER_RETRIES` belonged, and the fixed site
writes `PATHFINDER_RETRIES` (`leviculum-core/src/transport.rs:7479`)
into a literal that spells every field out — so not even the
field-deletion operator reaches it, and no operator substitutes one
const for another.

### What is a pin

Marked in the test by a doc-comment tag, **and** listed in
`scripts/pins.txt` with the location of its negative control. The gate
checks both directions: every tag has an entry and every entry has a
tag. Registration alone would make pinhood circular — the gate would
check that everything in the registry is in the registry, and a
pin-worthy test nobody registers would be silently absent.

The location is checked by reusing the window rule from
`leviculum-std/tests/doc_citations.rs`, so a moved control is caught the
same way a moved doc citation is. That is Guarantee C doing A's work,
which is the point of having all three on one page.

## Guarantee B — every test is executed by some gate

Half of it exists: `scripts/check-ignored-counts.py` enumerates tests
per unit and pins the ignored count. That stops the bucket growing
silently; it says nothing about whether anything in the bucket runs.

The other half:

1. **Every gate emits a manifest of what it executed**, by test name,
   per binary, **derived from run output, not from `cargo test --list`**
   — a list records intent. The hazard this must catch is a by-name
   selector that matches nothing: `cargo test <filter>` runs zero tests
   and exits 0, so a gate that selects by name reads green whether or
   not it measured anything. `scripts/run-status-parity.sh` already
   closes that hole by hand, pinning `EXPECTED=3` and parsing the
   summary line back — which is one gate's worth of what a manifest
   gives every gate.
2. **A check reports every test that exists and appears in no manifest**,
   by name.

### Enumerate at runtime, pin only counts

Item 2 must cover **all** tests, not only pins and declared exceptions.
The 14 unrun `rnsd_interop` tests were ordinary tests; a report scoped
to pins would not have seen them, and the per-unit count would have
stayed right the whole time — which is the argument against counts,
reintroduced.

The existing set is enumerated at run time, the way
`check-ignored-counts.py` already does it. Nothing needs a checked-in
list of ~3600 names, which would invite a `--bless` flag that silently
blesses the deletion it exists to catch.

What *is* pinned is **counts per unit**, and only for deletion
detection: a test that vanishes leaves both the manifest and the
runtime enumeration, so only a baseline catches it. Counts suffice for
that, and the file stays the size of `ignored-counts.txt`.

Note the per-configuration caveat: `leviculum-ffi` is gnu-only and
outside `default-members`, `leviculum-nrf` outside the workspace, so the
canonical configuration for the counts must be named.

Rollout has an ordering constraint: counts and reporting cannot be
enforced until every gate emits manifests, or everything reads as unrun.
Manifests first, enforcement second.

Step 1 is built: `scripts/run-with-manifest.py` wraps a gate's test
command, parses the names out of the run and writes one JSON manifest per
gate under `~/.local/state/leviculum-ci/test-manifests/`, next to the
other CI run state rather than in the tree or in `target/` — the tiers
run with their own `CARGO_TARGET_DIR`, so a manifest under `target/`
would split the union into one per tier. Fourteen gates emit today. The
union covers 3361 of the 3719 tests the workspace holds; the 358 outside
it are 31 `#[ignore]`d and 327 in units no gate names — `leviculum-cli`
and `leviculum-micron` entirely, most of `leviculum-lxmf`, the `jl`/
`jldiff` suites and every `lnomad` test. That is the size of step 2's
first report.

### Declared exceptions expire by execution, not by date

A test may legitimately run in no automatic gate: a rig test needing
hardware, a soak run before a release. The mechanism does not forbid it;
it requires the fact to be declared, with who runs it and where.

An expiry *date* is gateable but is the wrong quantity — it goes red on
a day unrelated to any change, and the cheapest fix is bumping it a
year, a one-line diff indistinguishable from maintenance. Instead: **an
exception is stale when no manifest has recorded that test executing for
longer than N.** It uses the manifests already being built, cannot be
satisfied by editing a number, and folds the exception list into the
staleness bound rather than leaving it a separate off switch.

The same bound applies to manifests themselves: a union with no age
limit counts a gate retired weeks ago. The repo already does this for
tier 2 (`scripts/check-tier2-staleness.sh`).

Two reach limits: tier-3 hardware manifests and the release-only
92-scenario corpus are produced on another host on a per-release
cadence, so either their transport is specified or the guarantee is
scoped to host-runnable gates. And `nextest` cannot run doctests, of
which `scripts/ignored-counts.txt` tracks two units (`leviculum-core
--doc` and `leviculum-std --doc`) — whichever runner is used, the
doctest gap is explicit.

## Guarantee C — every citation still points at what it claims

Our method rests on citations: a pinned deviation means nothing without
the reference line it deviates from. When a citation rots, the test
still passes and the sentence still reads — it simply stops being true.

`leviculum-std/tests/doc_citations.rs` is the working exemplar and the
proof that this rots fast. The *practice* of checking the reference
before auditing against it is already stated in
[Wire Field Semantics](wire-field-semantics.md); what follows is the
mechanism, not a restatement of the method.

### 1. The submodule check is O(1), and that is the whole incident

The five-week LXMF drift was not hundreds of citations going wrong
individually. It was **one fact**: the checked-out submodule disagreed
with its gitlink. The cheap catch is a per-batch assertion that
`git submodule status` reports no `+`/`-` for the four vendored
references — a few lines of shell, in a gate rather than in a `#[test]`
that Guarantee B can fail to observe.

That, and nothing more, would have caught the incident on day one.

### 2. Re-verification on a bump is a separate, larger problem

Binding every citation to a submodule commit and failing until they are
re-verified is a different mechanism, and it did not cause the incident.
If it is built, it must be incremental or it will not be used: on a
bump, `git diff --name-only old..new` inside the submodule bounds the
work to citations into changed files — usually a handful, not hundreds.

**This is what makes gap 3 a prerequisite rather than an afterthought.**
A citation written as ``ident` (`path:line`)` can be re-located
mechanically at the new commit and its line updated. A bare
`Transport.py:2970` can only be re-verified by a person reading it. So
converting *reference* citations to the identifier-adjacent form is what
turns a submodule bump from hundreds of manual re-reads into a
mechanical re-resolve.

### 3. Coverage: source is uncovered, and most citations are weak

The guard reads `docs/src/**`. A grep for reference citations
(`reference/…`, `Transport.py:`, `LXMRouter.py:`, `Destination.py:`)
across `leviculum-core`, `leviculum-lxmf` and `leviculum-std` finds
**421 in their `src/` trees, 497 counting their `tests/` directories —
against 804 covered in the book** — and the uncovered ones are
load-bearing, because they are what a pinned deviation cites.

Extending the glob to Rust source is cheap and should be done, but be
clear what it buys: for a bare citation it is existence-and-length
checking, so it catches renames and deletions and not drift inside a
file that stays long enough.

Counts from the guard's own output at the commit that landed this page:
**804 total, 72 identifier-checked, 731 bare, 1 external.** The bare
majority is an editorial problem — the fix is to write citations in the
identifier-adjacent form, and it cannot be mechanised without rewriting
prose. The honest response is to publish the ratio on every run so
nobody reads a green guard as full coverage, and to convert
opportunistically (#167 is the standing example).

### What C cannot reach

Issue comments, commit messages and batch reports carry hundreds of
`file:line` claims that nothing checks — and given how much of this
project's reasoning is recorded there rather than in the tree, that is
the largest uncovered surface of the three guarantees. A `just cite`
helper that emits a verified citation would reduce fabrication at the
point of writing; nothing can verify it after the fact.

## Standing canaries

Every gate on this page carries a permanent pair, checked before it
reports anything else:

- **A, registry gate**: a tagged test deliberately absent from
  `scripts/pins.txt` that must be reported, and a correctly registered
  one that must not be.
- **A, mutation audit**: a deliberately weak pin that must survive and a
  strong one that must die. Additionally: an unresolvable declared
  subject is a hard error, `mutants_generated > 0` is asserted per pin,
  and the equivalent-mutant allowlist carries per-entry justifications
  and is pinned like `scripts/ignored-counts.txt`.
- **B, manifest writer**: a fixture of libtest output, parsed before every
  wrapped run, holding tests that must land in the manifest — including a
  `#[should_panic]` one, which libtest names `<test> - should panic` and
  `--list` names plainly — and lines that must not: an ignored test, an
  indented look-alike a test could print. The counts are then reconciled
  against libtest's own summary line, so a manifest that disagrees with
  the run fails the gate.
- **B, manifest check**: a test deliberately in no gate that must always
  be reported, and one in a gate that must never be.
- **C, citation guard**: a deliberately drifted citation that must be
  reported. The existing guard has floor asserts against parser rot; the
  canary is the stronger form.

A one-time demonstration at implementation time is not enough. A gate
that stops matching — a glob that no longer resolves, a parser that
returns nothing — is green forever, which is the defect this page exists
to remove.

## Composition

**A pin is exempt from no gate, and may not appear in the exception
list.** Nothing else here makes that true: a pin that is `#[ignore]`d
satisfies A — its negative control is present and correct — while no
gate observes its green.

## What none of the three reaches

- **Whether the check is the *right* check.** A pin can be executed,
  carry a negative control, cite a live line, and still assert the wrong
  thing. That is what the reference-first and independent-recomposition
  rules in [Wire Field Semantics](wire-field-semantics.md) are for.
- **A negative control is author-chosen at both ends.** It proves the
  pin bites on one break the author thought of — not on the break that
  will happen. Mutation supplies an adversary who is not the author,
  which is the other reason the audit is not optional.
- **A control written to pass regardless** — verifying against a random
  key, asserting `is_err()` on something unrelated. It satisfies the
  registry gate; only the audit can see it.
- **Whether a test asserts anything at all.** A body of `let _ = f();`
  can be executed and coupled to its subject.
- **Prose.** Issue comments, commit messages, reports.
- **Scenario corpora.** A Periculum step that asserts nothing is the
  same defect in another language; its analogue is the delivery bar
  (Periculum #25).
- **Firmware.** `leviculum-nrf` is excluded from the workspace
  (`Cargo.toml:33`) and cross-compiles. All three stop there, and the
  sx1262 incident lives on the far side.

## Where this stands

Codeberg is the source of truth for what is built. At the time of
writing: C covers `docs/src/**` and the Rust sources of `leviculum-core`,
`leviculum-lxmf` and `leviculum-std`, and its submodule check runs first
in `just fast`; its bump path is unbuilt. B emits manifests from every
host gate that runs tests; the check that reads their union, and the
staleness bound that ages them out, are unbuilt. A is unbuilt.

All three are subject to the rule they enforce. The standing canaries
above are the demonstration made permanent, because a one-time one
decays.

## See also

- [Evidence and Honesty](evidence-and-honesty.md) — the rule this page
  mechanises, and the incidents behind it.
- [Wire Field Semantics](wire-field-semantics.md) — what a pin must
  assert, which is a different question from whether it can fail; and
  the practice of checking the reference before auditing against it,
  which Guarantee C mechanises.
