//! mvr for the `lora_lnode_rncp_bidir_seq` red of the 2026-10-31 full
//! hardware run (leviculum 47b8100f, cell logs
//! `snap-periculum-f7afc06/periculum/logs/lora_lnode_rncp_bidir_seq_*`):
//! "send failed on alpha (2KB run 1)", rncp gave up before the link proof
//! arrived (1169, Refs #24).
//!
//! WHAT THE RUN'S OWN RECORDS SAY. Alpha's board took rncp's link request
//! from its host at 03:13:01.922 and keyed it at 03:13:19.201. Ahead of it
//! on alpha's receiver, three copies of one 183-byte path response from
//! beta, `ph=8c778b81` every time, at 10.140, 12.896 and 15.652, each
//! restarting alpha's pre-TX jitter. Beta's capture shows where they came
//! from:
//!
//! ```text
//! 03:12:55.251  LORA_RX_FRAME len=68 ph=0cb3b6ca dst=6b9f6601   path request
//! 03:12:55.652  ANN_TX occasion=path-response dst=cef3c042
//! 03:12:58.010  LORA_RX_FRAME len=68 ph=96d8317d dst=6b9f6601   path request
//! 03:12:58.410  ANN_TX occasion=path-response dst=cef3c042
//! 03:12:59.258  LORA_RX_FRAME len=68 ph=a351c5b6 dst=6b9f6601   path request
//! 03:12:59.658  ANN_TX occasion=path-response dst=cef3c042
//! 03:13:07.408  TX 183 bytes   (after the radio's own receive window)
//! 03:13:10.164  TX 183 bytes   LORA_BURST continue
//! 03:13:12.915  TX 183 bytes   LORA_BURST continue
//! ```
//!
//! Three requests with three tags (alpha's board retries a pending
//! discovery every five seconds with a fresh tag, and its own queue let
//! them out back to back), three answers, each 400 ms after its request,
//! all three in beta's LoRa queue at once, keyed as one burst.
//!
//! THE REFERENCE ANSWERS THE SAME WAY. `Transport.path_request`
//! (Transport.py:2945-3000) puts one announce-table entry per non-duplicate
//! request, and a targeted path response is never held by the announce cap,
//! which covers only `attached_interface == None` (Transport.py:1193-1194
//! against :1246-1283). Measured with the same driver on both daemons
//! (`status_parity_tests::a_path_request_burst_is_answered_alike_on_both_stacks`):
//! three requests 2.5 s apart draw three path responses from rnsd and three
//! from lnsd. The transport is not where we differ.
//!
//! WHERE WE DO. lnsd's RNode send queue refuses a frame whose bytes it
//! already holds (`refuse_queued_duplicate`, 979e82f1); the board's LoRa
//! queue (`leviculum-nrf/src/lora.rs`, `LoRaInterface::try_send`) keyed
//! every copy. The three answers are byte-identical, because each is the
//! same cached announce rebuilt with the same hops and transport id, so the
//! second and third carry nothing the first does not. This file drives the
//! board's core with the measured request spacing and feeds what it hands
//! the LoRa interface through the board's own admission
//! (`leviculum_queue_budget::outbound_verdict`), with the radio busy so
//! nothing leaves the queue: one path response may be queued, not three.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use leviculum_core::constants::{MTU, PATH_REQUEST_GRACE_MS, TRUNCATED_HASHBYTES};
use leviculum_core::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use leviculum_core::transport::{Action, Transport, TransportConfig};
use leviculum_core::{Clock, Destination, DestinationType, Direction, Identity, MemoryStorage};
use leviculum_queue_budget::{
    outbound_verdict, Outbound, QueueBudget, QueuedCopies, LORA_QUEUE_BYTES, LORA_QUEUE_SLOTS,
};
use leviculum_std::test_support::event_log::init_event_log;
use rand_core::OsRng;

/// A clock the test advances by hand, shared with the transport.
#[derive(Clone)]
struct SteppedClock(Arc<AtomicU64>);

impl SteppedClock {
    fn new(start_ms: u64) -> Self {
        Self(Arc::new(AtomicU64::new(start_ms)))
    }
    fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::Relaxed);
    }
}

impl Clock for SteppedClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// The interfaces of beta's board: the USB serial link to its host, and
/// LoRa at the board's own index (`leviculum-nrf/src/lora.rs::IFACE_INDEX`).
const SERIAL: usize = 0;
const LORA: usize = 1;

/// The SF10/BW125 announce-cap bitrate the board registers for LoRa; any
/// value works, the path response is not capped.
const LORA_CAP_BPS: u32 = 440;

/// A transport node's path request, the 48-byte form alpha's board sends:
/// requested destination, the requester's transport id, a tag.
fn path_request(
    transport: &Transport<SteppedClock, MemoryStorage>,
    dest: &[u8; TRUNCATED_HASHBYTES],
    requester: &[u8; TRUNCATED_HASHBYTES],
    tag: u8,
) -> Vec<u8> {
    let mut data = Vec::with_capacity(48);
    data.extend_from_slice(dest);
    data.extend_from_slice(requester);
    data.extend_from_slice(&[tag; TRUNCATED_HASHBYTES]);
    let packet = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Plain,
            packet_type: PacketType::Data,
        },
        hops: 0,
        transport_id: None,
        destination_hash: *transport.path_request_hash(),
        context: PacketContext::None,
        data: PacketData::Owned(data),
    };
    let mut buf = [0u8; MTU];
    let len = packet.pack(&mut buf).expect("pack path request");
    buf[..len].to_vec()
}

/// The frames handed to the LoRa interface that are path responses.
fn path_responses_on_lora(actions: &[Action]) -> Vec<Vec<u8>> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::SendPacket { iface, data, .. } if iface.0 == LORA => Some(data.clone()),
            _ => None,
        })
        .filter(|raw| {
            Packet::unpack(raw)
                .map(|p| {
                    p.flags.packet_type == PacketType::Announce
                        && p.context == PacketContext::PathResponse
                })
                .unwrap_or(false)
        })
        .collect()
}

#[test]
fn a_repeated_path_request_queues_one_response_on_the_board() {
    let evlog = init_event_log();

    let clock = SteppedClock::new(1_000_000);
    let config = TransportConfig {
        enable_transport: true,
        ..TransportConfig::default()
    };
    let mut beta = Transport::new(
        config,
        clock.clone(),
        MemoryStorage::with_defaults(),
        Identity::generate(&mut OsRng),
    );
    beta.set_interface_name(SERIAL, "serial_usb".to_string());
    beta.set_interface_online(SERIAL, true);
    beta.set_interface_name(LORA, "lora_sx1262".to_string());
    beta.set_interface_online(LORA, true);
    beta.register_interface_bitrate(LORA, LORA_CAP_BPS);

    // rncp's destination on beta's host, announced over the serial link.
    let mut dest = Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "mvr1169",
        &["rncp", "receive"],
    )
    .expect("create destination");
    let now = clock.now_ms();
    let announce = dest
        .announce(Some(b"mvr1169"), &mut OsRng, now, now / 1000)
        .expect("create announce");
    let mut buf = [0u8; MTU];
    let len = announce.pack(&mut buf).expect("pack announce");
    beta.process_incoming(SERIAL, &buf[..len])
        .expect("process announce");
    // Let the announce's own rebroadcasts run out: they are not under test.
    for _ in 0..40 {
        clock.advance(1_000);
        beta.poll();
    }
    let _ = beta.drain_actions();

    // The board's LoRa queue while its radio sits in a receive window, as
    // beta's did from 03:12:55 to 03:13:07: nothing is dequeued.
    let budget = QueueBudget::new(LORA_QUEUE_SLOTS, LORA_QUEUE_BYTES);
    let copies = QueuedCopies::new();

    let alpha_transport = *Identity::generate(&mut OsRng).hash();
    let dest_hash = *dest.hash().as_bytes();
    // The measured spacing: 0, +2759 ms, +1248 ms.
    let gaps_ms = [0u64, 2_759, 1_248];
    // Each request is answered after the grace, inside its gap.
    let answer_ms = PATH_REQUEST_GRACE_MS + 100;
    let mut handed = Vec::new();
    let mut verdicts = Vec::new();
    for (i, gap) in gaps_ms.iter().enumerate() {
        clock.advance(gap.saturating_sub(answer_ms));
        beta.process_incoming(
            LORA,
            &path_request(&beta, &dest_hash, &alpha_transport, 0xA0 + i as u8),
        )
        .expect("process path request");
        clock.advance(answer_ms);
        beta.poll();
        let responses = path_responses_on_lora(&beta.drain_actions());
        for raw in responses {
            verdicts.push(outbound_verdict(true, &budget, &copies, &raw));
            handed.push(raw);
        }
    }

    let dump = evlog.dump();
    assert_eq!(
        handed.len(),
        3,
        "control: the core answers each fresh-tag request, as the reference \
         does (Transport.py:2945-3000); dump:\n{dump:#?}"
    );
    assert!(
        handed.windows(2).all(|w| w[0] == w[1]),
        "control: the three answers are byte-identical, the same cached \
         announce rebuilt the same way"
    );
    assert_eq!(
        verdicts,
        vec![Outbound::Queue, Outbound::Duplicate, Outbound::Duplicate],
        "the board's LoRa queue must hold ONE path response, not three: the \
         copies are the same frame on the air and each one keyed costs a \
         full SF10 airtime and restarts the requester's jitter"
    );
    assert_eq!(budget.queued_slots(), 1);

    // Handed to the transmitter, the frame has left the queue: a fourth
    // request's answer is a retry of a frame that can still be lost on the
    // air, and it must be queued again.
    budget.release(handed[0].len());
    copies.dequeued(&handed[0]);
    clock.advance(5_000);
    beta.process_incoming(
        LORA,
        &path_request(&beta, &dest_hash, &alpha_transport, 0xA3),
    )
    .expect("process fourth path request");
    clock.advance(PATH_REQUEST_GRACE_MS + 100);
    beta.poll();
    let fourth = path_responses_on_lora(&beta.drain_actions());
    assert_eq!(fourth.len(), 1, "control: the fourth request is answered");
    assert_eq!(
        outbound_verdict(true, &budget, &copies, &fourth[0]),
        Outbound::Queue,
        "a copy of a frame that already left the queue is queued again"
    );
}
