# Wire Field Semantics

Every field we write into a wire structure carries a *meaning* that
some peer acts on. This page records the failure mode where the
meaning is wrong while all our own tests stay green, the audit method
that finds it, and the testing rule that keeps it found. It applies to
every field we will ever add to any protocol we implement — Reticulum,
LXMF, or our own.

## The failure mode: self-consistent and wrong

The dangerous defect is not a malformed field. It is a field whose
value is **self-consistent between our writer and our reader, and
means something else to a peer**. Our writer produces it, our reader
consumes it, both apply the same rule — the same *wrong* rule — and
every test that exercises both halves of the misunderstanding passes.
Interop tests pass too, as long as they assert that exchanges
*succeed* rather than that generated values *mean* what the reference
takes them to mean.

Codeberg #155 is the worked example. The 5-byte announce emission
timestamp is defined by the reference as unix seconds, and a peer
orders same-destination paths by it. We stamped process uptime. Our
own reader applied the ordering rule correctly to our own wrong
values, so a pure leviculum mesh was blind to the defect; 318 interop
tests against real Python missed it, because the exchanges all
succeeded. The damage lived only in foreign path tables: entries that
could never win the newer-emission comparison again, worsening with
every restart because every reboot restarted the value from zero. A
live `rnsd` was found carrying 660 of 10 983 path entries with
non-timestamp values. The full story of where wall time comes from is
in [Time and Clocks](time-and-clocks.md); this page is about the
class, not the instance.

## The audit method: three questions per generated field

For every field we **generate** (not fields we merely echo back),
answer three questions, each with a citation:

1. **What does the reference put there?** File and line into
   `reference/Reticulum` (and `reference/LXMF` where applicable).
2. **What does a peer DECIDE from it?** Ordering, acceptance, expiry,
   deduplication, routing — the rule the value feeds, not the byte
   layout it sits in.
3. **Does our value satisfy that rule under adverse conditions?**
   Process restart, absence of a clock, long uptime, the field at its
   representable limit, a peer that has been up much longer or much
   shorter than we have.

Question 2 is the one our old tests never asked. A field whose
encoding round-trips perfectly can still fail the decision rule — the
#155 timestamp round-tripped for months.

## The testing rule: pin the meaning, recomposed independently

A field is verified only when a test pins its **meaning**, not its
encoding — and the test must recompose the expected value
**independently**, never by calling the same helper the writer uses.
A test that shares the writer's helper does not test the writer; it
tests that the helper equals itself, and stays green when writer and
reader drift together.

The worked example of getting this right is the announce-signature
pin from the #159 audit,
`announce_signature_covers_reference_byte_order_on_the_wire`
(`leviculum-core/src/destination.rs:2085`). It takes the raw wire
bytes of a packed announce, rebuilds the signed data in the exact
order the reference composes it (`Destination.py:297-298`:
`hash + public_key + name_hash + random_hash + ratchet [+ app_data]`),
and verifies with raw Ed25519 against the key half at payload bytes
32..64 — then proves the pin bites by showing that dropping the
destination hash from the front makes verification fail. The
signature tests that existed before it could not have caught a drift:
they called `verify_signature`, which shares `build_signed_data`
(`leviculum-core/src/announce.rs:108`) with the writer, so a writer
and reader that both composed the wrong bytes would have verified
each other forever — exactly the #155 class, one layer up.

Where a reference value is computable offline, pin it as a known-
answer test with the reference's own output (the name-hash and
destination-hash KATs in the same audit tranche,
`destination.rs:1884`).

## Deliberate non-behaviours get pins too

When we *intentionally* do not do something — usually because the
reference does not and doing it would desynchronise us — that
non-behaviour is itself a semantic contract, and it gets a pinned
test with the reference citation in the test's doc comment. A later
"improvement" then breaks a test whose comment explains why the
missing behaviour is deliberate, instead of silently shipping a
semantic deviation. The worked examples are the two time
non-behaviours — we emit our own wall clock verbatim and we do not
plausibility-check incoming emission timestamps — pinned with their
`Destination.py`/`Transport.py` citations; see
[Time and Clocks](time-and-clocks.md#we-do-not-validate-our-own-clock-or-incoming-timestamps).

## Where the audit stands

The systematic sweep over every generated field is Codeberg #159 —
the issue, not this page, is the source of truth for its state. As of
2026-08-02: the announce layer is done (tranche 1, pins in
`leviculum-core/src/destination.rs` and `transport.rs`), and the link
and resource layers are done (tranche 2, pins in
`leviculum-core/src/node/mvr_generated_field_pins.rs` and the
resource modules). Tranche 2 found two fields that failed the audit
and were fixed red-first: the request timestamp carried process
uptime (#164) and the resource advertisement sent a content hash
where the reference sends the salted per-transfer hash (#165). The
transport and LXMF layers are open.

## See also

- [Time and Clocks](time-and-clocks.md) — the #155 instance in full:
  where wall time comes from and the hardening around it.
- [Python-RNS Compatibility](python-rns-compatibility.md) — why the
  reference's decision rules, not its internals, are the contract.
