//! The daemon's own mailbox: an LXMF delivery destination on the node's
//! identity (Codeberg #384 part 4, deliverable 2).
//!
//! `lxmd` is not only a propagation node — it registers a delivery
//! identity, announces it, and hands every received message to an
//! external program (`register_delivery_identity` in `program_setup`,
//! `reference/LXMF/LXMF/Utilities/lxmd.py:422-423`; the `on_inbound`
//! hook in `lxmf_delivery`, `:295-311`). This module is that half of
//! `lnpnd`: it hosts a [`LxmfRouter`] for the delivery destination and
//! surfaces received messages as engine events; writing the message file
//! and running the hook is `main`'s job, off the core lock.
//!
//! The absorb loop is `lnmsg`'s (`lnmsg/src/engine.rs`,
//! `MAX_ABSORB_ROUNDS`): a core call made inside a hook can return
//! events synchronously, and the driver never hands those back, so
//! closing the loop is the consumer's job — bounded, because an
//! unbounded loop under the core lock is a node hang.

use std::collections::VecDeque;

use leviculum_core::node::NodeEvent;
use leviculum_core::transport::TickOutput;
use leviculum_core::{DestinationHash, Identity};
use leviculum_lxmf::announce;
use leviculum_lxmf::router::{LxmfRouter, RouterConfig, RouterEvent, RouterOutput};
use leviculum_lxmf::{LxmfNode, LxmfNodeConfig, Message};

use crate::engine::EngineEvent;

/// See `lnmsg/src/engine.rs` (`MAX_ABSORB_ROUNDS`) for why the re-feed
/// is bounded.
const MAX_ABSORB_ROUNDS: usize = 8;

type Core = leviculum_std::driver::StdNodeCore;

/// What the mailbox half needs to know, from the config file's `[lxmf]`
/// section (`apply_config`, `reference/LXMF/LXMF/Utilities/lxmd.py:75-105`).
#[derive(Debug, Clone)]
pub struct MailboxConfig {
    /// `display_name`, carried in the delivery announce.
    pub display_name: Vec<u8>,
    /// `stamp_cost` — the announced cost for messages to this mailbox
    /// (`peer_stamp_cost`; the reference clamps it to ≥ 1, `lxmd.py:91`).
    pub stamp_cost: u8,
    /// `announce_at_start` (`lxmd.py:80-83`, sent by `deferred_start_jobs`).
    pub announce_at_start: bool,
    /// `announce_interval` in seconds; `None` disables the periodic
    /// announce, like the reference's unset key (`lxmd.py:85-88`).
    pub announce_interval_secs: Option<u64>,
    /// `delivery_transfer_max_accepted_size` in kilobytes — enforced
    /// against advertised delivery resources before transfer, as the
    /// reference enforces its `delivery_limit`
    /// (`delivery_resource_advertised`, `reference/LXMF/LXMF/LXMRouter.py:1977`).
    pub delivery_limit_kb: u64,
    /// The `ignored` file's destination hashes: messages from these
    /// sources are dropped after decode (`ignore_destination`,
    /// `reference/LXMF/LXMF/LXMRouter.py:702-704`).
    pub ignored: Vec<[u8; 16]>,
}

/// The reference's start-up announce runs `DEFFERED_JOBS_DELAY` = 10 s
/// after start (`reference/LXMF/LXMF/Utilities/lxmd.py:33`, applied in
/// `deferred_start_jobs` `:492-507`).
pub const MAILBOX_ANNOUNCE_DELAY_SECS: u64 = 10;

/// When the delivery destination announces next.
///
/// An `Option<u64>` cannot carry this: `None` would have to mean both
/// "no announce booked yet" and "no announce will ever be booked", and
/// reading the second as the first is exactly how a mailbox with
/// `announce_at_start = yes` and no interval re-booked the 10 s start
/// announce on every tick and stormed the mesh for ever. [`Never`] is
/// terminal; nothing moves the schedule out of it.
///
/// [`Never`]: AnnounceSchedule::Never
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnnounceSchedule {
    /// Before the first tick: what is owed depends on a clock we have
    /// not read yet.
    Unscheduled,
    /// Announce once the monotonic clock reaches this millisecond.
    At(u64),
    /// Nothing more is owed: no interval is configured, and the start
    /// announce (if any) has been sent.
    Never,
}

pub(crate) struct MailboxRuntime {
    router: LxmfRouter,
    pub(crate) delivery_hash: [u8; 16],
    config: MailboxConfig,
    announce_schedule: AnnounceSchedule,
    events: std::sync::mpsc::Sender<EngineEvent>,
}

impl MailboxRuntime {
    /// Register the delivery destination on the daemon's identity.
    ///
    /// The identity is the same one the propagation destination uses,
    /// exactly as `lxmd` derives both from its one primary identity
    /// (`program_setup`, `reference/LXMF/LXMF/Utilities/lxmd.py:397-423`).
    pub(crate) fn register(
        core: &mut Core,
        identity: &Identity,
        config: MailboxConfig,
        events: std::sync::mpsc::Sender<EngineEvent>,
    ) -> Result<Self, String> {
        let identity_hash = *identity.hash();
        let bytes = identity
            .private_key_bytes()
            .map_err(|e| format!("the identity has no private key: {e:?}"))?;
        let copy = Identity::from_private_key_bytes(&bytes)
            .map_err(|e| format!("could not copy the identity: {e:?}"))?;
        let destination = LxmfNode::delivery_destination(copy)
            .map_err(|e| format!("delivery destination: {e:?}"))?;
        let delivery_hash = *destination.hash().as_bytes();
        let node_config = LxmfNodeConfig {
            max_incoming_resource_size: Some(config.delivery_limit_kb.saturating_mul(1000)),
            ..LxmfNodeConfig::default()
        };
        let node = LxmfNode::register(core, destination, node_config)
            .map_err(|e| format!("register delivery destination: {e:?}"))?;
        let router = LxmfRouter::new(node, identity_hash, RouterConfig::default());
        let _ = events.send(EngineEvent::MailboxReady { delivery_hash });
        Ok(Self {
            router,
            delivery_hash,
            config,
            announce_schedule: AnnounceSchedule::Unscheduled,
            events,
        })
    }

    fn emit(&self, event: EngineEvent) {
        let _ = self.events.send(event);
    }

    /// Route one router output; see the module docs for the bound.
    fn absorb(&mut self, core: &mut Core, first: RouterOutput, out: &mut TickOutput) {
        let mut queue = VecDeque::from([first]);
        let mut rounds = 0usize;
        while let Some(router_output) = queue.pop_front() {
            for event in router_output.events {
                self.report(event);
            }
            let mut core_output = router_output.core;
            let events = std::mem::take(&mut core_output.events);
            out.merge(core_output);

            rounds += 1;
            let refeed = rounds <= MAX_ABSORB_ROUNDS;
            for event in events {
                if refeed {
                    match self.router.handle_event(core, &event) {
                        Ok(next) => queue.push_back(next),
                        Err(e) => tracing::warn!("lnpnd: mailbox handle_event: {e:?}"),
                    }
                }
                out.events.push(event);
            }
        }
    }

    fn report(&mut self, event: RouterEvent) {
        match event {
            RouterEvent::MessageReceived(message) => self.deliver(*message),
            // The daemon composes no outbound messages, so outbound stamp
            // and resource-build events cannot occur; inbound bookkeeping
            // events are the router's own. Everything else is diagnostics.
            other => tracing::debug!("lnpnd: mailbox router event: {other:?}"),
        }
    }

    /// One received message: the reference's `lxmf_delivery` with the
    /// ignore list applied (`reference/LXMF/LXMF/Utilities/lxmd.py:295-311`;
    /// ignores registered at `:419-420`).
    fn deliver(&mut self, message: Message) {
        if self.config.ignored.contains(&message.source_hash) {
            tracing::debug!(
                "lnpnd: mailbox dropped message from ignored source {}",
                message
                    .source_hash
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            );
            return;
        }
        tracing::debug!(
            event = "PN_MAILBOX",
            src = message
                .source_hash
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            bytes = message.content.len(),
        );
        self.emit(EngineEvent::Inbound {
            message: Box::new(message),
        });
    }

    /// Deliver one unstamped propagated message addressed to this
    /// mailbox, locally (the engine's own-mailbox short-circuit; see
    /// `Engine::deliver_own_mailbox`).
    pub(crate) fn deliver_propagated(
        &mut self,
        core: &mut Core,
        unstamped: &[u8],
        out: &mut TickOutput,
    ) {
        match self.router.deliver_propagated_local(core, unstamped) {
            Ok(output) => self.absorb(core, output, out),
            Err(e) => tracing::debug!("lnpnd: own-mailbox delivery failed: {e:?}"),
        }
    }

    pub(crate) fn on_event(&mut self, core: &mut Core, event: &NodeEvent, out: &mut TickOutput) {
        match self.router.handle_event(core, event) {
            Ok(output) => self.absorb(core, output, out),
            Err(e) => tracing::warn!("lnpnd: mailbox handle_event: {e:?}"),
        }
    }

    /// Announce the delivery destination: at start when configured, then
    /// on `announce_interval` (`jobs`,
    /// `reference/LXMF/LXMF/Utilities/lxmd.py:475-479`).
    fn announce(&mut self, core: &mut Core, out: &mut TickOutput) {
        let cost = if self.config.stamp_cost > 0 {
            Some(self.config.stamp_cost)
        } else {
            None
        };
        let app_data = announce::delivery(Some(&self.config.display_name), cost);
        match core.announce_destination(&DestinationHash::new(self.delivery_hash), Some(&app_data))
        {
            Ok(output) => {
                out.merge(output);
                self.emit(EngineEvent::MailboxAnnounced);
            }
            Err(e) => tracing::warn!("lnpnd: mailbox announce failed: {e:?}"),
        }
    }

    /// The interval announce after `now_ms`, or [`AnnounceSchedule::Never`]
    /// when no interval is configured — the reference's unset
    /// `peer_announce_interval`, which its `jobs` loop skips outright
    /// (`reference/LXMF/LXMF/Utilities/lxmd.py:475-479`).
    fn after_interval(&self, now_ms: u64) -> AnnounceSchedule {
        match self.config.announce_interval_secs {
            Some(secs) => AnnounceSchedule::At(now_ms + secs * 1000),
            None => AnnounceSchedule::Never,
        }
    }

    pub(crate) fn on_tick(&mut self, core: &mut Core, now_ms: u64, out: &mut TickOutput) {
        if self.announce_schedule == AnnounceSchedule::Unscheduled {
            // First pass: book the deferred start announce, or only the
            // interval when announce-at-start is off.
            self.announce_schedule = if self.config.announce_at_start {
                AnnounceSchedule::At(now_ms + MAILBOX_ANNOUNCE_DELAY_SECS * 1000)
            } else {
                self.after_interval(now_ms)
            };
        }
        if let AnnounceSchedule::At(at) = self.announce_schedule {
            if now_ms >= at {
                self.announce(core, out);
                // The start announce is spent here; only an interval can
                // book another one.
                self.announce_schedule = self.after_interval(now_ms);
            }
        }
        match self.router.tick(core) {
            Ok(output) => self.absorb(core, output, out),
            Err(e) => tracing::warn!("lnpnd: mailbox tick: {e:?}"),
        }
        if let AnnounceSchedule::At(at) = self.announce_schedule {
            out.next_deadline_ms = Some(match out.next_deadline_ms {
                Some(existing) => existing.min(at),
                None => at,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use leviculum_core::node::NodeCoreBuilder;
    use leviculum_std::driver::{StdClock, StdStorage};

    fn core(dir: &std::path::Path) -> Core {
        NodeCoreBuilder::new().enable_transport(false).build(
            rand_core::OsRng,
            StdClock::new(),
            StdStorage::new(dir).expect("storage under a fresh temp dir"),
        )
    }

    fn config(announce_at_start: bool, announce_interval_secs: Option<u64>) -> MailboxConfig {
        MailboxConfig {
            display_name: b"an-operator".to_vec(),
            stamp_cost: 7,
            announce_at_start,
            announce_interval_secs,
            delivery_limit_kb: 1000,
            ignored: Vec::new(),
        }
    }

    /// Tick once a simulated second for `span_secs` and count the delivery
    /// announces the mailbox emitted. One tick per second is denser than
    /// the daemon's deadline-driven loop, which is the point: a schedule
    /// that re-arms itself shows up as a count, not as a timing artefact.
    fn announces_over(
        announce_at_start: bool,
        announce_interval_secs: Option<u64>,
        span_secs: u64,
    ) -> usize {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut core = core(dir.path());
        let (events, received) = std::sync::mpsc::channel();
        let identity = leviculum_std::generate_identity();
        let mut mailbox = MailboxRuntime::register(
            &mut core,
            &identity,
            config(announce_at_start, announce_interval_secs),
            events,
        )
        .expect("the delivery destination registers");

        let start = core.now_ms();
        let mut out = TickOutput::empty();
        for second in 0..=span_secs {
            mailbox.on_tick(&mut core, start + second * 1000, &mut out);
        }
        std::iter::from_fn(|| received.try_recv().ok())
            .filter(|event| matches!(event, EngineEvent::MailboxAnnounced))
            .count()
    }

    /// The field regression (miauhaus, 2026-09-20): with the example
    /// config's `[lxmf]` — `announce_at_start = yes` and no announce
    /// interval, which is what anyone who does not set the undocumented
    /// key runs — the daemon sent a delivery announce every 10 s to a
    /// public mesh for as long as it ran. The start announce is owed
    /// once; after it, nothing is.
    #[test]
    fn a_start_announce_without_an_interval_is_sent_exactly_once() {
        assert_eq!(
            announces_over(true, None, 120),
            1,
            "announce_at_start with no interval must announce once, then go quiet"
        );
    }

    /// No start announce and no interval: the mailbox never announces at
    /// all, like the reference whose `jobs` loop skips an unset
    /// `peer_announce_interval` (`lxmd.py:475-479`).
    #[test]
    fn without_a_start_announce_or_an_interval_the_mailbox_stays_silent() {
        assert_eq!(announces_over(false, None, 120), 0);
    }

    /// A configured interval paces the announces at that interval — not
    /// at the 10 s start delay. Start announce at t=10 s, then every 60 s
    /// from the tick that sent it: 10, 70, 130, 190.
    #[test]
    fn a_configured_interval_paces_the_announces_after_the_start_one() {
        assert_eq!(announces_over(true, Some(60), 200), 4);
    }

    /// The interval alone, with no start announce: first announce one
    /// interval in, then one per interval — 60, 120, 180.
    #[test]
    fn an_interval_without_a_start_announce_begins_one_interval_in() {
        assert_eq!(announces_over(false, Some(60), 200), 3);
    }
}
