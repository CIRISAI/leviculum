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
        // What this board did with ONE packet a neighbour addressed to it
        // for relay (#346). Off-board the same decision is already legible
        // as the journey events PKT_FORWARD / PKT_DROP / DEDUP_DROP; here
        // they are compiled out, and the periodic `[TRANSPORT]` counter line
        // can only say how many, never which. Only the ADDRESSED relay path
        // reaches this — an overheard copy bound elsewhere stays on the
        // counter, so the line cannot drown a shared-medium capture.
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
