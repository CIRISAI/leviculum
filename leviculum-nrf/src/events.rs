//! Core [`NodeEvent`] lines on the debug CDC.
//!
//! The firmware builds leviculum-core without the `tracing` feature, so
//! the core's own structured log lines are compiled out on the boards.
//! An event whose only trace was such a line is invisible at the bench —
//! the `LINK_REFUSED` refusal (#388) was the first: a phone whose link
//! request was refused at the cap left no line at all, and that is
//! precisely the case the cap exists for. Events that must be observable
//! on a board are rendered here instead, from the [`NodeEvent`] the core
//! emits on every build.
//!
//! Called by the bins' engine-pass macro on every drained
//! `TickOutput::events` batch, before the propagation role sees them.
//! One line per event; everything else in the stream is either handled
//! by a role ([`crate::pn`]), the telemetry reporter, or intentionally
//! silent. Line shape is held byte-exact by the host tests in
//! [`leviculum_log_line`].

use leviculum_core::node::NodeEvent;
use leviculum_drop_budget::DropBudget;

/// The board's one budget for `[DROP]` lines.
///
/// A static for the same reason [`crate::announce`]'s cadence is one: the
/// limit is a property of the debug port, not of any node object, and
/// [`log_events`] is a free function every bin calls through its engine-pass
/// macro. Same locking shape, uncontended on the single-core cooperative
/// executor.
///
/// `.bss` cost of the whole #346 feature: this cell. A `u64` window start,
/// two `u32` counters, a flag, inside the critical-section mutex.
static DROP_BUDGET: embassy_sync::blocking_mutex::Mutex<
    embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
    core::cell::RefCell<DropBudget>,
> = embassy_sync::blocking_mutex::Mutex::new(core::cell::RefCell::new(DropBudget::new()));

/// Emit the `[DROP] suppressed=` summary of a rate-limit window that has
/// expired without a further drop to close it.
///
/// Called once per main-loop iteration, not from [`log_events`]: a storm that
/// stops dead produces no further event, and the silence AFTER a storm is
/// exactly the reading the summary is for. On a board that has dropped
/// nothing this is one `Instant` comparison behind a critical section.
pub fn flush_drop_summary(now_ms: u64) {
    let summary = DROP_BUDGET.lock(|cell| cell.borrow_mut().flush(now_ms));
    if let Some(s) = summary {
        log_suppressed(s);
    }
}

fn log_suppressed(s: leviculum_drop_budget::Suppressed) {
    crate::log::log_fmt(
        "[DROP] ",
        format_args!(
            "{}",
            leviculum_log_line::DropSuppressedBody {
                suppressed: s.lines,
                window_ms: s.window_ms,
            }
        ),
    );
}

/// Render the events that carry board-visible evidence, and feed the
/// announce cadence the one event it takes from this stream.
///
/// The second job rides here because this is the only pass every bin
/// already makes over every event batch, propagation role or not. Rule 4
/// of #401: the FIRST announce heard from a destination buys one
/// immediate announce, so two stationary boards in one room find each
/// other in seconds instead of waiting out the hourly floor. It is a
/// trigger and never a movement signal — see
/// [`crate::announce::note_announce_heard`].
pub fn log_events(events: &[NodeEvent], now_ms: u64) {
    for event in events {
        if let NodeEvent::AnnounceReceived { announce, .. } = event {
            crate::announce::note_announce_heard(now_ms, announce.destination_hash().as_bytes());
        }
        if let NodeEvent::LinkRefused {
            destination_hash,
            links,
            max,
        } = event
        {
            crate::log::log_fmt(
                "LINK_REFUSED ",
                format_args!(
                    "{}",
                    leviculum_log_line::LinkRefusedBody {
                        links: *links,
                        max: *max,
                        dest: *destination_hash.as_bytes(),
                    }
                ),
            );
        }
        // The announce a board learned from and had nowhere to send.
        // A board registers no shared-instance local client, so
        // the announce table is its only general route to the serial
        // host; an announce the table does not take, with no discovery
        // request waiting for it, reaches the host by no route at all.
        // Nothing dropped it, so no bucket moves and — until this line —
        // no capture could show it happened. The wording is deliberate:
        // the packet arrived and the path is installed.
        if let NodeEvent::AnnounceLearnedNotRelayed {
            destination_hash,
            closed,
            discovery,
        } = event
        {
            crate::log::log_fmt(
                "ANNOUNCE_LEARNED_NOT_RELAYED ",
                format_args!(
                    "{}",
                    leviculum_log_line::AnnounceLearnedNotRelayedBody {
                        closed: closed.as_str(),
                        discovery: discovery.as_str(),
                        dest: *destination_hash.as_bytes(),
                    }
                ),
            );
        }
        // Why this board just talked (#405). Since the board registers an
        // airtime cap (#402) a capture shows announce-sized transmissions
        // that the cap's holdoff cannot account for, and nothing said which
        // of them the cap was even meant to pace: `[ANNOUNCE] sent` covers
        // only the announces this board originates, and the core's own
        // `ANN_TX` line is compiled out here. `occasion=` is that answer —
        // `transit` passed the cap, `local` bypassed it because it started
        // here, `uncapped` was never paced at all, `path-response` was asked
        // for. Transmissions only: an announce the cap held back emits no
        // line here.
        if let NodeEvent::AnnounceTransmitted {
            destination_hash,
            occasion,
            hops,
            interface_out,
        } = event
        {
            crate::log::log_fmt(
                "ANN_TX ",
                format_args!(
                    "{}",
                    leviculum_log_line::AnnounceTransmittedBody {
                        occasion: occasion.as_str(),
                        dest: *destination_hash.as_bytes(),
                        hops: *hops,
                        iface_out: *interface_out,
                    }
                ),
            );
        }
        // Why this board threw ONE packet away (#346). The complement of
        // `PKT_RELAY` below, never a second copy of it: the core's two event
        // sites are disjoint, so a packet that raises `RelayDecided` never
        // raises `PacketDropped`, and one dropped packet is one line.
        //
        // There is deliberately no `forwarded` line here. A line per
        // successfully relayed packet is a line nobody reads on a busy mesh,
        // and the count is already on the periodic `[TRANSPORT] fwd=` field
        // (`crate::transport_stats`).
        //
        // Rate-limited, because this is the path that carries the overheard
        // copies: on a shared medium the board hears every packet routed via
        // its neighbours. Unlimited, a storm would not lose drop lines — the
        // 8 KiB `LOG_RING` overwrites oldest-first — it would evict the
        // `[STACK]`, `[TRANSPORT]` and panic lines the capture was taken
        // for. `leviculum-drop-budget` holds the policy and its derivation.
        if let NodeEvent::PacketDropped {
            destination_hash,
            reason,
            interface_in,
            ..
        } = event
        {
            let decision = DROP_BUDGET.lock(|cell| cell.borrow_mut().admit(now_ms));
            // The closed window's summary first, so the capture reads in the
            // order the events happened.
            if let Some(s) = decision.summary {
                log_suppressed(s);
            }
            if decision.emit {
                crate::log::log_fmt(
                    "[DROP] ",
                    format_args!(
                        "{}",
                        leviculum_log_line::PacketDroppedBody {
                            reason: reason.kebab(),
                            dest: *destination_hash.as_bytes(),
                            iface_in: *interface_in,
                        }
                    ),
                );
            }
        }
        // What this board did with ONE packet a neighbour addressed to it
        // for relay (#346). Off-board the same decision is already legible
        // as the journey events PKT_FORWARD / PKT_DROP / DEDUP_DROP; here
        // they are compiled out, and the periodic `[TRANSPORT]` counter line
        // can only say how many, never which. Only the ADDRESSED relay path
        // reaches this; an overheard copy bound elsewhere is the `[DROP]`
        // line above, so the two never double-report one packet.
        if let NodeEvent::RelayDecided {
            destination_hash,
            packet_hash_prefix,
            outcome,
            hops,
            interface_out,
        } = event
        {
            crate::log::log_fmt(
                "PKT_RELAY ",
                format_args!(
                    "{}",
                    leviculum_log_line::RelayDecidedBody {
                        outcome: outcome.as_str(),
                        ph: *packet_hash_prefix,
                        dest: *destination_hash.as_bytes(),
                        hops: *hops,
                        iface_out: *interface_out,
                    }
                ),
            );
        }
    }
}
