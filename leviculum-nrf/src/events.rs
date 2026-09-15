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
    }
}
