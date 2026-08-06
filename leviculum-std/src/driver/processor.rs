//! In-driver core processor seam (Codeberg #196, design B).
//!
//! A consumer registers a [`CoreProcessor`]; inside the driver's tick it
//! receives `&mut StdNodeCore` plus each [`NodeEvent`] and returns a
//! [`TickOutput`] the driver applies on its own send path. `leviculum-std`
//! stays protocol-agnostic: `leviculum-lxmf`'s `LxmfNode` is the first
//! consumer, `leviculum-lxst` the expected second.
//!
//! The production precedent is the in-loop `/status` responder
//! (`super::remote_mgmt`): take the lock, build a small bundle, hand back a
//! `TickOutput`, return. This is that shape, made generic and public.
//!
//! # What a processor is structurally unable to do
//!
//! `docs/src/concepts/core-lock-budget.md` forbids anything the loop calls
//! from `.await`ing, and from calling back into the driver's public async API:
//! those methods end in `action_dispatch_tx.send(output).await` on a bounded
//! channel that the same loop drains, so a full channel deadlocks the node.
//!
//! The page lists those as two rules; they are one, and which way round
//! decided this design. Neither is documented-and-hoped-for here:
//!
//! * [`CoreProcessor::on_event`] and [`CoreProcessor::on_tick`] are
//!   synchronous `fn`s. A `.await` in either body does not compile. This is
//!   the load-bearing half: the deadlock needs the bounded-channel send to
//!   *complete*, and `.await` is the only construct that can complete it.
//!   Building the future and dropping it sends nothing and blocks nothing.
//! * The handle is `&mut StdNodeCore` — the sans-io core, which owns no
//!   channel to the loop and has no async surface at all. Every type that can
//!   reach `action_dispatch_tx` ([`super::PacketSender`],
//!   [`super::LinkHandle`], [`super::ReticulumNode`]) is constructed from
//!   `ReticulumNode`, never from the core.
//!
//! Both are pinned by `scripts/check-processor-compile-fail.sh`, which builds
//! the `leviculum-std/tests/cf_*.rs` fixtures and asserts each fails with one
//! specific error code. `just fast` runs it.
//!
//! The residual hole is named rather than papered over: a processor's own
//! struct may hold anything, so `futures::executor::block_on` over a smuggled
//! sender is expressible in any synchronous `fn` in Rust and no signature can
//! prevent it. What the seam guarantees is that it never *hands out* the means,
//! and that the natural way to write the mistake does not compile.
//!
//! # Registration happens before the channel exists
//!
//! A processor is installed on [`super::ReticulumNodeBuilder`], i.e. before
//! [`super::ReticulumNode::start`] constructs the real `action_dispatch_tx`.
//! Any handle cloned off the node at build time carries the placeholder channel
//! created in `ReticulumNode::new`, whose receiver is dropped immediately — a
//! send on it fails rather than blocks.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use leviculum_core::node::NodeEvent;
use leviculum_core::transport::TickOutput;

use crate::sync_ext::MutexRecover;

use super::StdNodeCore;

/// The documented ceiling on how long one tick's processor work may hold the
/// core lock.
///
/// It is a reported ceiling, not an enforced one, and that is a property of
/// the language rather than a shortcut: a synchronous `fn` cannot be
/// preempted, so the only way to bound it by force would be to run it on
/// another thread — at which point it no longer holds `&mut StdNodeCore`
/// inside the tick, which is the entire seam. Exceeding it emits
/// `CORE_PROCESSOR_OVER_BUDGET`.
///
/// The number comes off `docs/src/concepts/core-lock-budget.md` rather than
/// off a feeling. The page's own tolerance line is drawn at 3.2 ms: packing a
/// 1 MiB LXMF message (0.8 ms) and unpacking one with signature verification
/// (3.2 ms) are listed as costs that explicitly do *not* justify phasing. The
/// failure mode it names is 126.6 ms (`LxmfRouter::tick` with 8 due 256 KiB
/// messages) against a 141 ms measured stall. 5 ms therefore sits just above
/// everything the page calls acceptable and ~25x below what it calls the
/// defect, which is the widest gap available that still separates the two.
pub const PROCESSOR_TICK_BUDGET: Duration = Duration::from_millis(5);

/// A synchronous state machine the driver runs inside its own tick.
///
/// The consumer plugs in `LxmfNode` (or any other core-driving state machine);
/// the driver hands it `&mut StdNodeCore` and each [`NodeEvent`] the core
/// produced, and dispatches the returned [`TickOutput`] on the same path as
/// its own. See the `driver::processor` module documentation for what the
/// shape makes impossible and why.
///
/// # Where the events come from
///
/// The tap sits on `output.events` inside the driver's `dispatch_output`,
/// *before* the event sink classifies. Seven of the event types LXMF needs,
/// including `PacketReceived` and `LinkDataReceived` — how a message
/// *arrives* — are [`EventClass::Data`] and therefore droppable under load. A
/// processor fed from the public
/// [`take_event_receiver`](super::ReticulumNode::take_event_receiver) would
/// silently lose inbound messages with nothing underneath to retransmit them.
///
/// [`EventClass::Data`]: leviculum_core::node::EventClass::Data
///
/// # Answering in the same tick
///
/// Three events cannot be deferred: `PacketProofRequested` →
/// `send_proof_on_interface`, `LinkProofRequested` → `send_data_proof`, and
/// `ResourceAdvertised` → `accept_resource`/`reject_resource`. Call them on
/// the `core` handle and return the resulting `TickOutput`; the driver
/// dispatches it before it returns from the same `dispatch_output`.
///
/// # Recursion
///
/// A processor does **not** see the events of its own `TickOutput`. The bound
/// is one, and `run_event_tap` in this module carries the reasoning.
///
/// # Talking to the rest of the application
///
/// A processor owns whatever channels it likes, but may only use non-blocking
/// sends on them (`try_send`, `std::sync::mpsc::Sender::send`). Anything that
/// can block belongs on the other side of the queue.
pub trait CoreProcessor: Send + 'static {
    /// Consume one event the core produced this tick.
    ///
    /// Runs with the core lock held. Return `TickOutput::empty()` for events
    /// this processor does not care about — the driver skips dispatch when the
    /// merged output is empty.
    fn on_event(&mut self, core: &mut StdNodeCore, event: &NodeEvent) -> TickOutput;

    /// Periodic slot, called from the driver's timer branch (at most ~1 s
    /// apart, sooner when the core has an earlier deadline).
    ///
    /// This is where a processor drains its own outbound command queue and
    /// runs its own timers — an event tap alone can never *initiate* anything,
    /// because it only fires when the core has something to say. Runs with the
    /// core lock held, under the same [`PROCESSOR_TICK_BUDGET`].
    ///
    /// The default implementation does nothing.
    fn on_tick(&mut self, core: &mut StdNodeCore, now_ms: u64) -> TickOutput {
        let _ = (core, now_ms);
        TickOutput::empty()
    }
}

/// Feed one tick's raw events to the processor and return its merged
/// `TickOutput`, or `None` when there is nothing to dispatch.
///
/// Called from `dispatch_output` with the raw `output.events`, before the
/// event sink classifies them.
///
/// # The recursion bound is one
///
/// The returned `TickOutput` is dispatched with the processor detached, so a
/// processor never observes the events it itself emitted. This is a bound, not
/// an oversight, and it costs nothing:
///
/// * Applying a `TickOutput` does not run the core. `dispatch_output` routes
///   actions to interfaces and forwards events; it cannot *produce* a new
///   `NodeEvent` from the protocol. Everything a processor's actions cause
///   comes back through `handle_packet`/`handle_timeout` on a later tick,
///   where the tap is live again.
/// * The only events in a processor's `TickOutput` are ones it synthesised
///   itself. Feeding those back is a self-cycle with no fixpoint anyone can
///   guarantee — `LxmfNode::handle_event` legitimately emits events in
///   response to events, so the loop would be unbounded. An unbounded loop
///   here is a node hang, not a bug report.
///
/// This mirrors the `/status` responder, which passes `remote_mgmt: None` on
/// its own recursive dispatch for the same reason.
pub(crate) fn run_event_tap(
    processor: &mut dyn CoreProcessor,
    inner: &Arc<Mutex<StdNodeCore>>,
    events: &[NodeEvent],
) -> Option<TickOutput> {
    if events.is_empty() {
        return None;
    }

    let started = Instant::now();
    let mut merged = TickOutput::empty();
    {
        // One acquisition for the whole tick, like the `/status` responder:
        // re-taking it per event would multiply the contention this seam is
        // supposed to be careful with.
        let mut core = inner.lock_recover();
        for event in events {
            merged.merge(processor.on_event(&mut core, event));
        }
    }
    report_budget(started, "on_event", events.len());

    (!merged.is_empty()).then_some(merged)
}

/// Run the processor's periodic slot. The caller already holds the core lock.
pub(crate) fn run_tick(
    processor: &mut dyn CoreProcessor,
    core: &mut StdNodeCore,
    now_ms: u64,
) -> TickOutput {
    let started = Instant::now();
    let output = processor.on_tick(core, now_ms);
    report_budget(started, "on_tick", 0);
    output
}

/// Emit `CORE_PROCESSOR_OVER_BUDGET` when a hook ran longer than
/// [`PROCESSOR_TICK_BUDGET`].
///
/// Structured fields only, no trailing prose: the canonical event-log line is
/// whitespace-tokenised (see `EventSink::emit_control`).
fn report_budget(started: Instant, hook: &'static str, events: usize) {
    let elapsed = started.elapsed();
    if elapsed > PROCESSOR_TICK_BUDGET {
        tracing::warn!(
            event = "CORE_PROCESSOR_OVER_BUDGET",
            hook,
            elapsed_us = elapsed.as_micros() as u64,
            budget_us = PROCESSOR_TICK_BUDGET.as_micros() as u64,
            events,
        );
    }
}
