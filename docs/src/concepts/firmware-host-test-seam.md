# The firmware's host-test seam

> **A decision lives in a host-testable crate. The nRF binary calls it.**

`leviculum-nrf` builds for `thumbv7em-none-eabihf`, sits outside the root
workspace with its own `.cargo` config, and runs no test of its own. Any
statement about firmware behaviour that is written inside it is therefore
provable only by flashing a board. This page says where the line between
the two sides runs, why it is not a matter of taste, and which decisions
are still on the wrong side of it.

## Why the rule is about the bench

The rig is one bench. It is shared between the conformance corpus, the
land gates and every manual measurement, and a hardware run costs hours —
so anything provable *only* there competes with everything else provable
there. A host assertion costs a second and runs on every push.

The pattern was found under time pressure rather than designed. Codeberg
#402 stayed open for days over a board's announce cap registration that
had in fact been correct since `594dd3f8`, because nothing could show it.
What closed it was moving the step that carried the meaning
(`AnnounceCapBitrate::sync_phy`) into `leviculum-core`, where a host test
makes the same call the firmware makes. That is the pattern; this page is
it stated as policy instead of as one lucky fix.

## What counts as a decision

A decision is anything whose wrongness is a behaviour, not a wiring
fault: a cadence, a threshold, an ordering, a predicate, an arithmetic
budget, a byte-exact line other tools grep. If a sentence about the code
can be written as "under X it does Y", it is a decision, and that
sentence belongs in a test.

The far side — what legitimately stays in `leviculum-nrf/src` — is
everything whose argument is a pin, a register, a SoftDevice syscall or
an `embassy_time::Instant`: peripheral access, task spawning, the boot
order, the I/O half of a driver.

The seam between them is a value type. The crate holds the decision and
the vocabulary it is expressed in; the firmware supplies the I/O and the
clock and does what it is told. `leviculum-rx-arming` is the sharpest
example already in the tree — it holds the order in which the receive
path re-arms and hands a frame up, and `src/sx1262.rs` supplies a chip to
drive (`stand_down_for_rx`, `leviculum-nrf/src/sx1262.rs:946`).

## Where the seam runs today

The `leviculum-nrf` workspace has 25 members besides the firmware crate,
every one of them pure and host-testable: `screen`, `sd-policy`,
`gnss-time`, `gnss-presence`, `gnss-init`, `telemetry-policy`, `ble-tx`,
`announce-policy`, `queue-budget`, `log-line`, `tx-spacing`, `rx-arming`,
`persist-ack`, `boot-trace`, `boot-count`, `channel-access`,
`media-state`, `record-log`, `pn-store`, `store-spike`, `qspi-bitbang`,
`battery-scale`, `settle-budget`, `sync-batch`, `upload-proof`
(`members`, `leviculum-nrf/Cargo.toml:14`). Together they carry **786
host assertions across 58 test targets** — measured 2026-09-25 by the
host-triple lines of `lint-nrf` (`Justfile:75`).

`leviculum-core` is the other half of the seam and counts the same way: a
decision that is not board-specific belongs there, where `lnsd` runs the
identical code. `leviculum-announce-policy` is deliberately shared with
the daemon for exactly that reason, so the cadence the desk measures on a
board is the cadence the daemon runs.

That number — how many firmware decisions can be asserted without a board
— is the measure this page is judged by. It goes up when a decision
moves, and it is the only thing that does.

## The far side is not a choice

Whether `leviculum-nrf` should gain a host test target of its own is
settled by the compiler, not by preference. Both BSP features route
through the `softdevice` aggregator, `lib.rs` refuses a build with no BSP
selected, and the SoftDevice bindings do not compile for a host triple:

```
$ cd leviculum-nrf
$ cargo check -p leviculum-nrf --features bsp-t114 \
      --target x86_64-unknown-linux-gnu
error: invalid register `r0`: unknown register
...
error: could not compile `nrf-softdevice-s140` (lib) due to 548 previous errors
```

(measured 2026-09-25). There is no feature combination that both links
and builds for the host, so `#[test]` inside `leviculum-nrf` has nowhere
to run. Consistently, `leviculum-nrf/src` contains zero `#[test]` and no
`tests/` directory today.

**So: "cannot move" is the definition of hardware-only.** A decision that
has not been moved is not hardware-only, it is untested. The question to
ask of any firmware behaviour is never "can the firmware crate test
this?" — it cannot test anything — but "what is the value type that
carries this decision, and what is left over once it is gone?"

## How the rule is gated

`lint-nrf` runs clippy and the tests of every workspace member except the
firmware crate, on the host triple, as `--workspace --exclude
leviculum-nrf`. It is spelled that way rather than as a list of `-p`
flags so that adding a seam crate to `members` is the whole act of
gating it. The list it replaced lived in three places, and its prose copy
had already lost `leviculum-upload-proof` within a day of that crate
landing. A positive control confirmed the failure mode is silent: a
member carrying a deliberately red test failed the workspace form with
exit 101 and passed the `-p` list with exit 0, because the list did not
name it.

New code follows the rule. Old code moves when it is touched anyway — a
bug fix in a stranded decision is the moment to move it, not a reason to
defer.

## Still stranded

The four areas that prompted this page are already seamed, and it is
worth saying which, because the list below is what is actually left:

| Decision | Crate | Since |
|---|---|---|
| Announce cadence and per-peer limit | `leviculum-announce-policy` | `787ce002`, 2026-09-09 |
| LoRa channel access (jitter, CAD retry) | `leviculum-channel-access` | `11c532f0`, 2026-09-01 |
| Receive re-arm / hand-off order | `leviculum-rx-arming` | `6d289255`, 2026-08-26 |
| Media flags, running vs configured | `leviculum-media-state` | `8bbce725`, 2026-09-01 |

What has no host assertion, in the order it is cheap to move:

**1. The heap budget arithmetic.** Pure `usize` maths that decides how
many endpoint links the board admits (`max_endpoint_links`,
`leviculum-nrf/src/heap_census.rs:173`), the boot serve cap
(`budget_serve_cap`, `leviculum-nrf/src/heap_census.rs:243`) and what the
propagation role may still serve given the live free and largest-block
figures (`live_serve_cap`, `leviculum-nrf/src/heap_census.rs:317`). Its
whole check today is one compile-time assert on a single derived constant
(`SERVE_MARGIN_BYTES`, `leviculum-nrf/src/heap_census.rs:281`). Nothing
in it touches a peripheral; it is the cheapest move on this list.

**2. The node name and its BLE-pending flag.** The derivation of the mesh
name (`mesh_name`, `leviculum-nrf/src/name.rs:156`) and the GAP name the
next boot will advertise (`pending_gap_name`,
`leviculum-nrf/src/name.rs:183`), and the rule that publishes
`NODE_NAME_FLAG_BLE_PENDING` when the two surfaces disagree (`report`,
`leviculum-nrf/src/name.rs:206`). "The board is one reset behind" is a
statement about two strings; only its storage is flash.

**3. The front-end position as a driver invariant.** "Asserted for
receive, released for transmit" is claimed as a property of the driver
rather than of its callers (`rx_frontend`,
`leviculum-nrf/src/sx1262.rs:373`), and the ordering it depends on — off
the receive path before the PA keys, because a switch asked for both
positions at once passes neither — is a comment, not an assertion
(`leviculum-nrf/src/sx1262.rs:789`). This is the shape of the Solar Node
receiving nothing (Codeberg #411): a board where DIO2 owns only the
transmit side (`LORA_DIO2_AS_RF_SWITCH`,
`leviculum-nrf/src/boards/solarnode.rs:82`) has three candidate causes
and no way to tell them apart short of flashing. The seam is a
front-end position tracked beside the arm state in
`leviculum-rx-arming`, so every direction change can be asserted to leave
the pair consistent.

**4. The 1200-baud touch predicate.** Whether a USB control-out request
means "reset into the UF2 bootloader" is a four-term match on request
type, recipient, request code and payload length before the rate is even
read (`BaudTouchHandler`, `leviculum-nrf/src/usb.rs:216`). A false
positive resets a board mid-session; a false negative costs a physical
double-tap on every flash. The predicate is pure; only the GPREGRET write
and `sys_reset` after it are not.

**5. The control-envelope answer windows.** Three durations, each
justified against a host-side timeout it must stay inside:
`CONFIG_DELIVER_WITHIN` (`leviculum-nrf/src/usb.rs:444`),
`CONFIG_APPLY_WITHIN` (`leviculum-nrf/src/usb.rs:456`) and
`FRAME_TIMEOUT_MS` (`leviculum-nrf/src/usb.rs:601`). The reasoning is
airtime arithmetic at the live modulation, which is exactly what a host
test can recompute and a comment cannot.

**6. The `[TRANSPORT]` ticker.** The re-arm deliberately drops missed
periods so a busy loop does not then emit a burst of catch-up lines
(`poll`, `leviculum-nrf/src/transport_stats.rs:77`), and the line is
byte-exact because capture consumers grep it (`log`,
`leviculum-nrf/src/transport_stats.rs:105`). `leviculum-log-line` already
exists for the second half.

See also: [Checks that are actually
checks](checks-and-citations.md) for why a stated rule without a gate
does not hold, and [Evidence and honesty in
testing](evidence-and-honesty.md).
