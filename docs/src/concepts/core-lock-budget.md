# The Core Lock Budget

The async driver owns exactly one `NodeCore` behind one mutex
(`leviculum-std/src/driver/mod.rs:99`). Every packet the node decrypts,
routes, forwards or emits passes through it. It is the narrowest point
in the stack, and the rule that follows from that is:

> **No caller holds the core lock across CPU-heavy work. Work that
> scales with payload size runs off the lock, between a cheap capture
> and a cheap commit.**

This is not a style preference. It was measured.

## The measurement that set the rule

`NodeCore::send_resource` used to run its whole build — bz2 compress,
bulk token encrypt, full/map hashing — inside the mutex. For a 1 MiB
incompressible payload with compression on, that was a **141 ms** hold
in release. On a 20-link inbound flood it cost a **32 % inbound
throughput stall** while round-sized sends ran; ~0 % after the fix
(Codeberg #152).

The fix is the three-phase shape, and it is the house idiom for
anything with the same profile:

| phase | lock | code |
| --- | --- | --- |
| `NodeCore::resource_send_params` | brief | `leviculum-core/src/node/mod.rs:1335` |
| `resource::prepare_resource_send` | **none** | `leviculum-core/src/resource/outgoing.rs:106` |
| `NodeCore::commit_resource_send` | brief | `leviculum-core/src/node/mod.rs:1370` |

Commit re-validates what could have changed while the build ran
unlocked: link gone, a transfer raced in, or the link re-keyed (#66) —
the last returns the retryable `ResourceError::LinkStateChanged` and
the caller rebuilds once. The std driver calls the three phases itself
(`leviculum-std/src/driver/mod.rs:2852`).

`NodeCore::send_resource` still exists as the composed single call
(`leviculum-core/src/node/mod.rs:1308`) because no_std and FFI callers
have no lock to hold and no second thread to starve. It is the
composed form that is dangerous behind the driver, not the code it
composes.

## The numbers, restated for callers

Measured on x86_64 release with `leviculum-core/compression` on (as
`leviculum-std` builds it), comparing the composed call against the
locked portion of the phased path:

| payload | composed (locked) | phased (locked) |
| --- | --- | --- |
| 16 KiB | 1.7 ms | 2 µs |
| 64 KiB | 4.6 ms | 2 µs |
| 256 KiB | 16.4 ms | 2 µs |
| 1 MiB incompressible | 65.2 ms | 5 µs |
| 1 MiB compressible | 89.3 ms | 3 µs |

Compressible data is the worse case: bz2 does more work when it
succeeds. Without the `compression` feature — the embedded default —
the 1 MiB build drops to 6.4 ms, which is why the same code is
tolerable on an nRF52 and intolerable behind the driver.

Costs that do **not** justify phasing, measured the same way: packing a
1 MiB LXMF message is 0.8 ms, and unpacking one with signature
verification is 3.2 ms. Inbound verification cannot be phased away —
the bytes are already in hand — and at that magnitude it does not need
to be.

### The adapter-side number: `LxmfRouter::tick`

Measured during the #196 design pass, on the same machine and profile:
one `LxmfRouter::tick` with 8 due 256 KiB messages holds **126.6 ms** in
a single uninterrupted borrow. It is the composed-`send_resource` cost
of the table above, multiplied by a queue depth an adapter reaches
routinely — the router builds each due message in turn, and nothing
between them yields.

It is recorded here rather than in the issue because this page is where
a caller looks for it, and because a number that lives only in a tracker
cannot be cited from the tree: `PROCESSOR_TICK_BUDGET`
(`leviculum-std/src/driver/processor.rs:137`) is set against this
measurement, and until it was written down the only number behind a
public constant could not be traced at all.

## What this binds

**Any protocol adapter layered on the core.** `leviculum-lxmf` is the
current instance and `leviculum-lxst` will be the next. An adapter that
offers only a monolithic submit call forces its host either to hold the
lock for the build or to fork the driver. Adapters that are expected to
run behind the async driver expose the phase split; the composed form
stays for the embedded caller.

**Anything the driver runs inside its event loop.** The loop's
`dispatch_output` (`leviculum-std/src/driver/mod.rs:4090`) routes
actions to interfaces and forwards events. Work done there blocks not
just the lock but interface I/O dispatch — strictly worse than the
mutex case. The in-loop `/status` responder
(`leviculum-std/src/driver/remote_mgmt.rs:84`) is the reference for how
much is acceptable there: take the lock, build a small bundle, hand
back a `TickOutput`, return.

**Two things the loop's callees may never do.** They may not `.await`,
and they may not call back into the driver's public async API: those
methods end in `action_dispatch_tx.send(output).await` on a bounded
channel that the same loop drains, so a full channel deadlocks the
node.

Those are one rule, not two, and knowing which way round matters when
you have to enforce it. The second is a consequence of the first: an
`async fn` called and not awaited builds a future and drops it, sends
nothing and blocks nothing. The deadlock needs the bounded-channel send
to *complete*, and only `.await` can complete it. So a callee expressed
as a synchronous `fn` has both prohibitions closed at once, which is
what the in-driver core processor (#196) is built on — see
`leviculum-std/src/driver/processor.rs`. It follows that a *runtime*
guard on the async API would be the wrong shape: there is nothing to
guard until an `.await` that cannot be written.

### The residue is re-entrancy, not the async API

Both prohibitions above are special cases of a plainer one, and stating
them first got the emphasis wrong for two commits. **The loop calls its
callees with the core mutex held, and that mutex is a non-reentrant
`std::sync::Mutex`.** Any path from a callee back to it hangs the node
immediately — first call, no load required.

The async route is one such path and not the instructive one. Consider
the `block_on` case the previous wording named as the whole residue: a
callee that smuggles a `PacketSender` and blocks on `send` does not
reach the bounded channel at all, because `PacketSender::send`
(`leviculum-std/src/driver/sender.rs:76-86`) takes the core lock in a
block and releases it *before* its `.await`. It deadlocks one line
earlier, on the mutex.

And no `block_on` is needed. `ReticulumNode` carries roughly forty
plain synchronous `pub fn`s that open by locking the core —
`has_path` (`leviculum-std/src/driver/mod.rs:2429`) is
`self.inner.lock_recover().has_path(dest_hash)` and is entirely typical.
A callee holding an `Arc<ReticulumNode>` deadlocks the node on its first
invocation, in ordinary safe synchronous code, with no `.await`, no
channel, and nothing a compile-fail fixture can catch.

So the rule for anything the loop calls is: **hold no handle to the node
you run inside.** The `&mut StdNodeCore` the seam hands out locks
nothing and is the whole intended surface. Everything else belongs on
the far side of a channel.

What holds this up is not the type system. It is that registration
happens on the builder, before the node exists, so a callee cannot be
*constructed* holding a node handle — injecting one afterwards takes a
deliberate `OnceLock` or `Weak` and a reference cycle. That is a
construction-order barrier, and it is why the hazard stays theoretical
in practice. It is not a guarantee, and this page should not be read as
offering one.

### The one call the seam hands out that this page forbids

`NodeCore::send_resource` is `pub`
(`leviculum-core/src/node/mod.rs:1308`) and therefore reachable on the
`&mut StdNodeCore` a processor hook holds. It is the 141 ms composed
call this page opens with — one line, in consumer code, behind the
driver and under the lock. `PROCESSOR_TICK_BUDGET` reports it 141 ms
after the fact and cannot prevent it, and no fixture can refuse it: it
compiles, because for the no_std and FFI callers it is the correct API.

A hook that has to send a resource uses the three-phase form the driver
itself uses — `resource_send_params`, `prepare_resource_send` off the
lock, `commit_resource_send` — or hands the send to the application side
of a channel. This is named again in
`leviculum-std/src/driver/processor.rs` where a consumer will meet it.

## Work that is already off the core by construction

Proof-of-work stamp generation borrows only its executor, never the
router and never the core
(`leviculum-lxmf/src/router/stamp_runtime.rs:26`). The router emits a
pending-stamp event, the application computes the stamp on whatever
schedule it likes, and hands the result back. This matters because a
peer chooses the stamp cost: an announced cost of 254 is legal and
effectively unfinishable (#185). If that search could ever run under
the core lock, any peer could stop the node by announcing a number.
It cannot, and no seam added later may make it possible.

That pattern — emit a request, compute detached, submit the result —
is the general answer whenever the cost of a step is not ours to bound.

One correction to the paragraph above, because its phrasing is wider
than what holds. It is exactly true of the *peer-priced* search, which
is the one that matters: `generate_with` is an `async fn`, so no
synchronous callee of the event loop can drive it to completion at all.
It is not true that the tree contains no synchronous proof-of-work.
`leviculum-core::discovery::stamp::generate_stamp`
(`leviculum-core/src/discovery/stamp.rs:147`) is a public synchronous
brute-force loop taking a caller-supplied `cost`, and nothing stops a
loop callee from calling it. It is not a DoS vector today because no
peer picks its number: the only caller is the discovery announcer,
which runs it once per discoverable interface during
`ReticulumNode::start()` — off the loop — at the locally fixed
`DEFAULT_STAMP_VALUE`. The invariant to keep is therefore "no
*peer-chosen* cost is ever ground synchronously", and the async
signature is what enforces it.
