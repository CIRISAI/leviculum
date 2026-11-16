//! The `/dfu` verb on the remote-management destination (OTA stage 1,
//! Codeberg #235, `docs/src/concepts/ota-stage-1-ble-dfu.md`).
//!
//! A request over an identified link whose identity is on the board's
//! `LMGA` allow-list puts the board into its bootloader's BLE DFU mode.
//! Authorization is the core's, exactly as for `/status`: the handler is
//! registered under [`RequestPolicy::AllowList`](super::RequestPolicy)
//! beside `/status` (`enable_remote_management`), so an accepted request
//! arrives as [`NodeEvent::RequestReceived`] and a refused one as
//! [`NodeEvent::RequestRefused`]. This module decides what each of those
//! means for `/dfu` ([`dfu_command`]), what the requester is told
//! ([`DfuVerdict::reply`]), and the order the board leaves in
//! ([`exit_to_dfu`]): the reply is handed to the interfaces and allowed to
//! leave, bounded, THEN the magic is written, THEN the board resets.
//!
//! The firmware supplies the three platform acts behind [`DfuExit`]: the
//! dispatch, the "has it left" reading of its carriers' queues, and the
//! `GPREGRET` write plus reset (the same seam the 1200-baud UF2 touch uses,
//! `leviculum-nrf/src/usb.rs`). Everything that decides an order or a bound
//! lives here, where a host test holds it.

use core::future::Future;

use rand_core::CryptoRngCore;

use super::{NodeCore, NodeEvent};
use crate::constants::TRUNCATED_HASHBYTES;
use crate::traits::{Clock, Storage};
use crate::transport::TickOutput;
use crate::{LinkId, RequestError};

/// The request path served. Python-RNS serves `/status` and `/path` on the
/// management destination and never asks for this one, so it is an
/// extension no Python peer can trip over.
pub const DFU_PATH: &str = "/dfu";

/// The `GPREGRET` value that sends the Adafruit nRF52 bootloader into BLE
/// DFU: `DFU_MAGIC_OTA_RESET`, "entered by soft reset, SD is not inited
/// yet" (`Adafruit_nRF52_Bootloader/src/main.c:110`). Not `0xB1`
/// (`DFU_MAGIC_OTA_APPJUM`), which presumes a SoftDevice the application
/// left initialised, and not `0x57`, the UF2 touch's magic.
pub const DFU_MAGIC_OTA_RESET: u8 = 0xA8;

/// How long the board waits for the reply to leave before it resets
/// anyway: the bound that keeps a stuck interface from holding the board
/// in limbo. Above the LoRa queue's stale-drop age (18 s,
/// `leviculum_queue_budget::HOLD_MAX_AGE_MS`, asserted in the firmware), so
/// a reply held behind a duty-cycle lock is either sent or dropped before
/// the bound runs out, and the bound only ever ends a wait on an interface
/// that stopped working.
pub const DFU_REPLY_WAIT_MS: u64 = 20_000;

/// How often [`exit_to_dfu`] asks whether the reply has left.
pub const DFU_REPLY_POLL_MS: u64 = 20;

/// What the board made of a `/dfu` request. The reply carries it as one
/// msgpack positive fixint ([`Self::reply`]), so the sender can tell an
/// accepted command from a refusal, and the refusal's reason, without a
/// timeout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DfuVerdict {
    /// The link's identity is on the allow-list: the board enters DFU.
    Accepted,
    /// The link was identified, with an identity the list does not hold.
    NotListed,
    /// The link never identified.
    NoIdentity,
}

impl DfuVerdict {
    /// Whether the board enters DFU.
    pub const fn accepted(self) -> bool {
        matches!(self, DfuVerdict::Accepted)
    }

    /// The wire code: 0 accepted, 1 not listed, 2 no identity.
    pub const fn code(self) -> u8 {
        match self {
            DfuVerdict::Accepted => 0,
            DfuVerdict::NotListed => 1,
            DfuVerdict::NoIdentity => 2,
        }
    }

    /// The `reason=` token of the log lines.
    pub const fn reason(self) -> &'static str {
        match self {
            DfuVerdict::Accepted => "accepted",
            DfuVerdict::NotListed => "not-listed",
            DfuVerdict::NoIdentity => "no-identity",
        }
    }

    /// The response body: the code as one msgpack positive fixint, which
    /// is the byte itself for every value below 0x80.
    pub const fn reply(self) -> [u8; 1] {
        [self.code()]
    }

    /// Read a response body back (the sender's side).
    pub fn from_reply(data: &[u8]) -> Option<Self> {
        match data {
            [0] => Some(DfuVerdict::Accepted),
            [1] => Some(DfuVerdict::NotListed),
            [2] => Some(DfuVerdict::NoIdentity),
            _ => None,
        }
    }
}

/// One `/dfu` request on this node's management destination, decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DfuCommand {
    /// The link the request came over, which the reply goes back on.
    pub link_id: LinkId,
    /// The request the reply answers.
    pub request_id: [u8; TRUNCATED_HASHBYTES],
    /// The identity the link was identified with, if any.
    pub from: Option<[u8; TRUNCATED_HASHBYTES]>,
    /// What the board does about it.
    pub verdict: DfuVerdict,
}

/// Match `event` against this node's `/dfu` handler. `None` for anything
/// else, including a `/dfu` request on another destination and every
/// event of a node without remote management.
///
/// An accepted request is one the core's allow-list let through; it is
/// still refused here when the link holds no identity, so a handler
/// registered under a looser policy by mistake cannot open the door.
pub fn dfu_command<R, C, S>(node: &NodeCore<R, C, S>, event: &NodeEvent) -> Option<DfuCommand>
where
    R: CryptoRngCore,
    C: Clock,
    S: Storage,
{
    let management = node.remote_mgmt_dest_hash()?;
    let command = match event {
        NodeEvent::RequestReceived {
            link_id,
            destination_hash,
            request_id,
            path,
            ..
        } if destination_hash == management && path == DFU_PATH => {
            let from = node
                .link(link_id)
                .and_then(|link| link.remote_identity())
                .map(|identity| *identity.hash());
            DfuCommand {
                link_id: *link_id,
                request_id: *request_id,
                from,
                verdict: if from.is_some() {
                    DfuVerdict::Accepted
                } else {
                    DfuVerdict::NoIdentity
                },
            }
        }
        NodeEvent::RequestRefused {
            link_id,
            destination_hash,
            request_id,
            path,
            requester,
        } if destination_hash == management && path == DFU_PATH => DfuCommand {
            link_id: *link_id,
            request_id: *request_id,
            from: *requester,
            verdict: if requester.is_some() {
                DfuVerdict::NotListed
            } else {
                DfuVerdict::NoIdentity
            },
        },
        _ => return None,
    };
    log_command(&command);
    Some(command)
}

/// The host-side `MGMT_DFU` line; the boards render the same key set from
/// the [`DfuCommand`] (`leviculum_log_line::MgmtDfuBody`), because they build
/// core without `tracing`.
fn log_command(command: &DfuCommand) {
    #[cfg(feature = "tracing")]
    {
        use crate::hex_fmt::HexShort;
        let accepted = if command.verdict.accepted() {
            "yes"
        } else {
            "no"
        };
        match &command.from {
            Some(from) => crate::tracing::info!(
                event = "MGMT_DFU",
                accepted,
                from = %HexShort(from),
                reason = command.verdict.reason(),
            ),
            None => crate::tracing::info!(
                event = "MGMT_DFU",
                accepted,
                from = "none",
                reason = command.verdict.reason(),
            ),
        }
    }
    #[cfg(not(feature = "tracing"))]
    let _ = command;
}

/// Build the typed reply for `command` on its link. The caller dispatches
/// it: a refusal right away, an accepted command through [`exit_to_dfu`].
pub fn send_dfu_reply<R, C, S>(
    node: &mut NodeCore<R, C, S>,
    command: &DfuCommand,
) -> Result<TickOutput, RequestError>
where
    R: CryptoRngCore,
    C: Clock,
    S: Storage,
{
    node.send_response(
        &command.link_id,
        &command.request_id,
        &command.verdict.reply(),
    )
}

/// How the wait for the reply ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyLeave {
    /// Every carrier reported the reply gone.
    Left,
    /// [`DFU_REPLY_WAIT_MS`] ran out first; the board resets anyway.
    TimedOut,
}

impl ReplyLeave {
    /// The `reply=` token of the board's last line before the reset.
    pub const fn as_str(self) -> &'static str {
        match self {
            ReplyLeave::Left => "left",
            ReplyLeave::TimedOut => "timeout",
        }
    }
}

/// The platform acts [`exit_to_dfu`] orders. The board implements them on
/// its carriers' queues and the `GPREGRET` syscall; a host test implements
/// them on a recorder.
pub trait DfuExit {
    /// Hand the reply's actions to the interfaces.
    fn hand_over(&mut self, reply: TickOutput);
    /// Whether everything handed over has left its carrier: transmitted,
    /// or dropped by the carrier's own rules. Not "accepted into a queue".
    fn reply_left(&mut self) -> bool;
    /// The clock the bound is measured on.
    fn now_ms(&self) -> u64;
    /// Let the carriers run for [`DFU_REPLY_POLL_MS`].
    fn pause(&mut self) -> impl Future<Output = ()>;
    /// Last word before the magic: the board logs how the wait ended.
    /// Nothing may be logged after [`Self::write_magic`].
    fn leaving(&mut self, outcome: ReplyLeave);
    /// Write the bootloader magic.
    fn write_magic(&mut self, magic: u8);
    /// Reset. Does not return on a board.
    fn reset(&mut self);
}

/// Leave for the bootloader in the one order that makes the reply count:
/// hand the reply over, wait (bounded by [`DFU_REPLY_WAIT_MS`]) until it has
/// left, then write [`DFU_MAGIC_OTA_RESET`], then reset. Over the mesh the
/// reply is the only evidence the command arrived; a board that resets
/// first is, from the far end, a board that heard nothing.
///
/// Returns only where [`DfuExit::reset`] returns, which is a host test.
pub async fn exit_to_dfu<E: DfuExit>(exit: &mut E, reply: TickOutput) -> ReplyLeave {
    exit.hand_over(reply);
    let deadline = exit.now_ms().saturating_add(DFU_REPLY_WAIT_MS);
    let outcome = loop {
        if exit.reply_left() {
            break ReplyLeave::Left;
        }
        if exit.now_ms() >= deadline {
            break ReplyLeave::TimedOut;
        }
        exit.pause().await;
    };
    exit.leaving(outcome);
    exit.write_magic(DFU_MAGIC_OTA_RESET);
    exit.reset();
    outcome
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};
    use rand_core::OsRng;

    use super::*;
    use crate::identity::Identity;
    use crate::node::NodeCoreBuilder;
    use crate::test_utils::{MockClock, TEST_TIME_MS};
    use crate::traits::NoStorage;
    use crate::transport::{Action, InterfaceId};
    use crate::DestinationHash;

    type Node = NodeCore<OsRng, MockClock, NoStorage>;

    fn packets(output: &TickOutput) -> Vec<Vec<u8>> {
        output
            .actions
            .iter()
            .map(|action| match action {
                Action::Broadcast { data, .. } | Action::SendPacket { data, .. } => data.clone(),
            })
            .collect()
    }

    fn one_packet(output: &TickOutput) -> Vec<u8> {
        let mut packets = packets(output);
        assert_eq!(packets.len(), 1);
        packets.remove(0)
    }

    /// A board as the firmware builds one: remote management only from a
    /// stored list, `/dfu` beside `/status`.
    fn board(allowed: Vec<[u8; 16]>) -> Node {
        NodeCoreBuilder::new()
            .remote_management(true, allowed)
            .remote_dfu(true)
            .build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage)
    }

    struct Pair {
        client: Node,
        server: Node,
        client_link: LinkId,
    }

    impl Pair {
        fn connect(mut server: Node) -> Self {
            let destination: DestinationHash = *server.remote_mgmt_dest_hash().unwrap();
            let signing_key = server
                .destination(&destination)
                .unwrap()
                .identity()
                .unwrap()
                .ed25519_verifying()
                .to_bytes();
            let mut client =
                NodeCoreBuilder::new().build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
            let (client_link, _, output) = client.connect(destination, &signing_key).unwrap();
            let proof = server.handle_packet(InterfaceId(0), &one_packet(&output));
            let rtt = client.handle_packet(InterfaceId(0), &one_packet(&proof));
            let _ = server.handle_packet(InterfaceId(0), &one_packet(&rtt));
            Self {
                client,
                server,
                client_link,
            }
        }

        fn identify(&mut self, identity: &Identity) {
            let output = self
                .client
                .identify_link(&self.client_link, identity)
                .unwrap();
            let _ = self
                .server
                .handle_packet(InterfaceId(0), &one_packet(&output));
        }

        /// Send `/dfu` and return what the board decided, if anything.
        fn dfu(&mut self) -> Option<DfuCommand> {
            let (_, output) = self
                .client
                .send_request(&self.client_link, DFU_PATH, None, None)
                .unwrap();
            let output = self
                .server
                .handle_packet(InterfaceId(0), &one_packet(&output));
            let mut commands = output
                .events
                .iter()
                .filter_map(|event| dfu_command(&self.server, event));
            let command = commands.next();
            assert!(commands.next().is_none(), "one request, one decision");
            command
        }

        /// Deliver the board's reply and read the verdict the client got.
        fn reply_verdict(&mut self, command: &DfuCommand) -> DfuVerdict {
            let reply = send_dfu_reply(&mut self.server, command).unwrap();
            let output = self
                .client
                .handle_packet(InterfaceId(0), &one_packet(&reply));
            output
                .events
                .iter()
                .find_map(|event| match event {
                    NodeEvent::ResponseReceived { response_data, .. } => {
                        DfuVerdict::from_reply(response_data)
                    }
                    _ => None,
                })
                .expect("the client must receive a typed verdict")
        }
    }

    #[test]
    fn a_listed_identity_is_accepted_and_told_so() {
        let operator = Identity::generate(&mut OsRng);
        let mut pair = Pair::connect(board(vec![*operator.hash()]));
        pair.identify(&operator);
        let command = pair.dfu().expect("a listed /dfu is a command");
        assert_eq!(command.verdict, DfuVerdict::Accepted);
        assert_eq!(command.from, Some(*operator.hash()));
        assert_eq!(pair.reply_verdict(&command), DfuVerdict::Accepted);
    }

    #[test]
    fn an_unlisted_identity_is_refused_with_a_typed_reply() {
        let operator = Identity::generate(&mut OsRng);
        let stranger = Identity::generate(&mut OsRng);
        let mut pair = Pair::connect(board(vec![*operator.hash()]));
        pair.identify(&stranger);
        let command = pair.dfu().expect("a refused /dfu is still decided");
        assert_eq!(command.verdict, DfuVerdict::NotListed);
        assert_eq!(command.from, Some(*stranger.hash()));
        assert_eq!(pair.reply_verdict(&command), DfuVerdict::NotListed);
    }

    #[test]
    fn a_link_without_identity_is_refused() {
        let operator = Identity::generate(&mut OsRng);
        let mut pair = Pair::connect(board(vec![*operator.hash()]));
        let command = pair.dfu().expect("a refused /dfu is still decided");
        assert_eq!(command.verdict, DfuVerdict::NoIdentity);
        assert_eq!(command.from, None);
        assert_eq!(pair.reply_verdict(&command), DfuVerdict::NoIdentity);
    }

    /// An empty list accepts nobody, as for `/status`: the identity that
    /// would be listed on another board is refused here.
    #[test]
    fn an_empty_list_refuses_the_identity_it_would_list() {
        let operator = Identity::generate(&mut OsRng);
        let mut pair = Pair::connect(board(Vec::new()));
        pair.identify(&operator);
        let command = pair.dfu().expect("a refused /dfu is still decided");
        assert_eq!(command.verdict, DfuVerdict::NotListed);
        assert_eq!(command.from, Some(*operator.hash()));
    }

    /// `/dfu` is the board's verb: a node built without `remote_dfu` (a
    /// daemon) has no handler, so the request is not even decided.
    #[test]
    fn without_remote_dfu_the_verb_is_not_served() {
        let operator = Identity::generate(&mut OsRng);
        let server = NodeCoreBuilder::new()
            .remote_management(true, vec![*operator.hash()])
            .build(OsRng, MockClock::new(TEST_TIME_MS), NoStorage);
        let mut pair = Pair::connect(server);
        pair.identify(&operator);
        assert_eq!(pair.dfu(), None);
    }

    /// `/status` refusals raise `RequestRefused` too, and must not be read
    /// as a `/dfu` command.
    #[test]
    fn a_refused_status_request_is_not_a_dfu_command() {
        let operator = Identity::generate(&mut OsRng);
        let mut pair = Pair::connect(board(vec![*operator.hash()]));
        let (_, output) = pair
            .client
            .send_request(&pair.client_link, "/status", None, None)
            .unwrap();
        let output = pair
            .server
            .handle_packet(InterfaceId(0), &one_packet(&output));
        assert!(output.events.iter().any(
            |event| matches!(event, NodeEvent::RequestRefused { path, .. } if path == "/status")
        ));
        assert!(
            packets(&output).is_empty(),
            "a refusal sends nothing by itself"
        );
        assert!(output
            .events
            .iter()
            .all(|event| dfu_command(&pair.server, event).is_none()));
    }

    #[test]
    fn the_magic_is_the_ota_reset_value() {
        assert_eq!(DFU_MAGIC_OTA_RESET, 0xA8);
        assert_ne!(DFU_MAGIC_OTA_RESET, 0xB1, "DFU_MAGIC_OTA_APPJUM");
        assert_ne!(DFU_MAGIC_OTA_RESET, 0x57, "the UF2 touch's magic");
    }

    #[test]
    fn every_verdict_round_trips_its_reply() {
        for verdict in [
            DfuVerdict::Accepted,
            DfuVerdict::NotListed,
            DfuVerdict::NoIdentity,
        ] {
            assert_eq!(DfuVerdict::from_reply(&verdict.reply()), Some(verdict));
        }
        assert_eq!(DfuVerdict::from_reply(&[]), None);
        assert_eq!(DfuVerdict::from_reply(&[3]), None);
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Step {
        HandOver(usize),
        Leaving(ReplyLeave),
        Magic(u8),
        Reset,
    }

    /// A board whose carriers report the reply gone after `left_after`
    /// polls, or never.
    struct Recorder {
        steps: Vec<Step>,
        now_ms: u64,
        polls: u32,
        left_after: Option<u32>,
    }

    impl Recorder {
        fn new(left_after: Option<u32>) -> Self {
            Self {
                steps: Vec::new(),
                now_ms: TEST_TIME_MS,
                polls: 0,
                left_after,
            }
        }
    }

    impl DfuExit for Recorder {
        fn hand_over(&mut self, reply: TickOutput) {
            self.steps.push(Step::HandOver(reply.actions.len()));
        }
        fn reply_left(&mut self) -> bool {
            assert!(
                matches!(self.steps.first(), Some(Step::HandOver(_))),
                "asked before the reply was handed over"
            );
            let left = self.left_after.is_some_and(|n| self.polls >= n);
            self.polls += 1;
            left
        }
        fn now_ms(&self) -> u64 {
            self.now_ms
        }
        fn pause(&mut self) -> impl Future<Output = ()> {
            self.now_ms += DFU_REPLY_POLL_MS;
            core::future::ready(())
        }
        fn leaving(&mut self, outcome: ReplyLeave) {
            self.steps.push(Step::Leaving(outcome));
        }
        fn write_magic(&mut self, magic: u8) {
            self.steps.push(Step::Magic(magic));
        }
        fn reset(&mut self) {
            self.steps.push(Step::Reset);
        }
    }

    fn run(exit: &mut Recorder, reply: TickOutput) -> ReplyLeave {
        let mut future = pin!(exit_to_dfu(exit, reply));
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(outcome) = future.as_mut().poll(&mut cx) {
                return outcome;
            }
        }
    }

    fn a_reply() -> TickOutput {
        let operator = Identity::generate(&mut OsRng);
        let mut pair = Pair::connect(board(vec![*operator.hash()]));
        pair.identify(&operator);
        let command = pair.dfu().unwrap();
        send_dfu_reply(&mut pair.server, &command).unwrap()
    }

    #[test]
    fn the_reply_is_handed_over_and_waited_for_before_the_magic() {
        let mut exit = Recorder::new(Some(3));
        assert_eq!(run(&mut exit, a_reply()), ReplyLeave::Left);
        assert_eq!(
            exit.steps,
            vec![
                Step::HandOver(1),
                Step::Leaving(ReplyLeave::Left),
                Step::Magic(0xA8),
                Step::Reset,
            ]
        );
        assert_eq!(exit.polls, 4, "three polls said no, the fourth said gone");
        assert_eq!(exit.now_ms, TEST_TIME_MS + 3 * DFU_REPLY_POLL_MS);
    }

    #[test]
    fn a_carrier_that_never_transmits_cannot_hold_the_board() {
        let mut exit = Recorder::new(None);
        assert_eq!(run(&mut exit, a_reply()), ReplyLeave::TimedOut);
        assert_eq!(
            exit.steps,
            vec![
                Step::HandOver(1),
                Step::Leaving(ReplyLeave::TimedOut),
                Step::Magic(0xA8),
                Step::Reset,
            ]
        );
        assert_eq!(exit.now_ms, TEST_TIME_MS + DFU_REPLY_WAIT_MS);
    }

    #[test]
    fn a_reply_already_gone_costs_no_wait() {
        let mut exit = Recorder::new(Some(0));
        assert_eq!(run(&mut exit, a_reply()), ReplyLeave::Left);
        assert_eq!(exit.now_ms, TEST_TIME_MS);
        assert_eq!(exit.steps.last(), Some(&Step::Reset));
    }
}
