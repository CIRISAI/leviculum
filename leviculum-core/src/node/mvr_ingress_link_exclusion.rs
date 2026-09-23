//! mvr: a broadcast skips the LINK it arrived on, not the whole
//! interface that link belongs to (Codeberg #422).
//!
//! ## The field failure this reproduces
//!
//! Rig cell `ble_reconnect`, red in every retained run (b74027aa and
//! 648650a3, same signature). The middle board (Pocket, RAK4631) is
//! reset, so its path table is empty, and the only recovery channel is
//! the host's path request. The request arrives on the board's BLE
//! link (`BLE: RX 51B conn=1`) and the board re-originates it as `ACT
//! Broadcast excl=2 len=67`, which reaches the serial interface alone
//! (`SER: TX 67`). The t114 link, the only party holding a live path,
//! never hears it. Six retries at 5 s, all serial only.
//!
//! The mechanism is one line of arithmetic about what "the interface it
//! came in on" means. `send_on_all_interfaces_except_many` excludes the
//! requestor's interface, faithful to Python `Transport.py:2792-2806`,
//! and `dispatch_actions` skips that whole interface object. On the
//! board the interfaces are `[serial, lora, ble]`: ONE BLE
//! `InterfaceId(2)` carries BOTH links (`BLE_TX_FLOOD links=2` whenever
//! it is not excluded), so excluding the interface silences the link
//! that heard nothing along with the link that heard everything.
//!
//! ## Reference semantics
//!
//! Python excludes the receiving interface because on a broadcast
//! medium every peer on the segment already heard the original
//! request: the exclusion is a statement about what was heard, and on
//! LoRa or Ethernet "the interface" and "what heard it" are the same
//! set. `ble-reticulum` keeps that identity by spawning one
//! sub-interface per peer (`BLEInterface._spawn_peer_interface`,
//! ble-reticulum/src/ble_reticulum/BLEInterface.py:1892), so Python's
//! Transport excludes exactly one link there too. Our firmware's
//! interface table is fixed
//! at boot, so the same statement is carried per packet instead: the
//! broadcast names the ingress LINK beside the ingress interface, and
//! each interface decides what that means for itself. Wire and
//! semantics are untouched; a Python peer sees the ordinary 48-byte
//! path request it would have seen from a per-peer sub-interface.
//!
//! ## Shape
//!
//! One node, deterministic, sub-second. Two interfaces in the board's
//! own shape: a single-link serial interface and a fake multi-link
//! interface holding two links (the host and the t114). The observable
//! is per LINK, which is why the actions are dispatched into real
//! interface objects rather than read off `TickOutput`: the bug lives
//! exactly in what the dispatch does with the action.

extern crate std;

use std::boxed::Box;
use std::cell::RefCell;
use std::rc::Rc;
use std::string::String;
use std::vec::Vec;

use alloc::collections::BTreeMap;
use rand_core::OsRng;

use crate::constants::{DISCOVERY_RETRY_INTERVAL_MS, TRUNCATED_HASHBYTES};
use crate::destination::{Destination, DestinationType, Direction};
use crate::embedded_storage::EmbeddedStorage;
use crate::identity::Identity;
use crate::ifac::IfacConfig;
use crate::node::{NodeCore, NodeCoreBuilder};
use crate::packet::{Packet, PacketType};
use crate::test_utils::{MockClock, MockInterface, TEST_TIME_MS};
use crate::traits::{Interface, InterfaceError, InterfaceMode};
use crate::transport::{dispatch_actions, InterfaceId};

/// The board's exact shape: `EmbeddedStorage`, transport enabled.
type EmbeddedNode = NodeCore<OsRng, MockClock, EmbeddedStorage>;

/// The host's link and the t114's link on the board's one BLE
/// interface, as the interface reports them on peer-up.
const HOST: [u8; TRUNCATED_HASHBYTES] = [0xaa; TRUNCATED_HASHBYTES];
const T114: [u8; TRUNCATED_HASHBYTES] = [0xbb; TRUNCATED_HASHBYTES];

const SERIAL: usize = 0;
const BLE: usize = 1;

/// What every link of the multi-link interface was handed, in order.
type LinkLog = Rc<RefCell<Vec<([u8; TRUNCATED_HASHBYTES], Vec<u8>)>>>;

/// A fake interface of the BLE shape: several point-to-point links
/// multiplexed into ONE interface id, each addressable by the peer
/// identity the interface reports on peer-up (Codeberg #365/#376).
///
/// It is deliberately not a BLE mock: no fragmentation, no MTU, no
/// connection handles. The only quirk it carries is the one the core's
/// broadcast exclusion has to survive — that a packet received here was
/// heard by one link and by no other.
struct MultiLinkInterface {
    id: InterfaceId,
    name: &'static str,
    links: Vec<[u8; TRUNCATED_HASHBYTES]>,
    log: LinkLog,
}

impl MultiLinkInterface {
    fn new(name: &'static str, id: usize, links: &[[u8; TRUNCATED_HASHBYTES]]) -> (Self, LinkLog) {
        let log: LinkLog = Rc::new(RefCell::new(Vec::new()));
        (
            Self {
                id: InterfaceId(id),
                name,
                links: links.to_vec(),
                log: Rc::clone(&log),
            },
            log,
        )
    }

    fn deliver(&self, link: &[u8; TRUNCATED_HASHBYTES], data: &[u8]) {
        self.log.borrow_mut().push((*link, data.to_vec()));
    }
}

impl Interface for MultiLinkInterface {
    fn id(&self) -> InterfaceId {
        self.id
    }
    fn name(&self) -> &str {
        self.name
    }
    fn mtu(&self) -> usize {
        500
    }
    fn is_online(&self) -> bool {
        true
    }
    /// A broadcast reaches every live link: one `try_send` covers the
    /// medium, exactly as one LoRa transmission reaches every listener.
    fn try_send(&mut self, data: &[u8]) -> Result<(), InterfaceError> {
        for link in self.links.clone() {
            self.deliver(&link, data);
        }
        Ok(())
    }
    /// The #376 delivery hint: the addressed peer's link alone.
    fn try_send_to_peer(
        &mut self,
        data: &[u8],
        peer: Option<&[u8; TRUNCATED_HASHBYTES]>,
        high_priority: bool,
    ) -> Result<(), InterfaceError> {
        match peer {
            None => self.try_send_prioritized(data, high_priority),
            Some(peer) => {
                if self.links.contains(peer) {
                    self.deliver(peer, data);
                }
                Ok(())
            }
        }
    }
    /// The #422 ingress-link exclusion: every live link but the one
    /// named, because that one already heard these bytes.
    fn try_send_excluding_peer(
        &mut self,
        data: &[u8],
        peer: &[u8; TRUNCATED_HASHBYTES],
        _high_priority: bool,
    ) -> Result<(), InterfaceError> {
        for link in self.links.clone() {
            if &link != peer {
                self.deliver(&link, data);
            }
        }
        Ok(())
    }
}

fn make_node() -> Box<EmbeddedNode> {
    NodeCoreBuilder::new().enable_transport(true).build_boxed(
        OsRng,
        MockClock::new(TEST_TIME_MS),
        EmbeddedStorage::new(),
    )
}

/// The board: a single-link serial interface and a two-link BLE
/// interface, both Gateway-mode as the firmware sets them
/// (`leviculum-nrf/src/bin/rak4631.rs`), and the BLE peer count
/// mirrored as the main loop mirrors it.
fn make_board() -> Box<EmbeddedNode> {
    let mut node = make_node();
    node.set_interface_name(SERIAL, String::from("serial_usb"));
    node.set_interface_mode(SERIAL, InterfaceMode::Gateway);
    node.set_interface_name(BLE, String::from("ble"));
    node.set_interface_mode(BLE, InterfaceMode::Gateway);
    node.set_interface_peer_count(BLE, 2);
    node
}

/// A destination the board has never heard of (the host's peer, gone
/// from the table with the reset).
fn unknown_destination() -> [u8; TRUNCATED_HASHBYTES] {
    let identity = Identity::generate(&mut OsRng);
    let dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["delivery"],
    )
    .unwrap();
    *dest.hash().as_bytes()
}

/// The 48-byte transport-form path request the host puts on the wire,
/// built from a standalone node so the board sees a foreign requestor
/// id and tag.
fn foreign_path_request(dest: &[u8; TRUNCATED_HASHBYTES]) -> Vec<u8> {
    let mut requester = make_node();
    requester.set_interface_name(0, String::from("ble"));
    let pr_hash = *requester.transport().path_request_hash();
    let out = requester.request_path(&crate::DestinationHash::new(*dest));
    out.actions
        .iter()
        .map(|action| match action {
            crate::transport::Action::SendPacket { data, .. }
            | crate::transport::Action::Broadcast { data, .. } => data.clone(),
        })
        .find(|data| {
            Packet::unpack(data)
                .map(|p| p.destination_hash == pr_hash)
                .unwrap_or(false)
        })
        .expect("the requester emits a path request")
}

/// Is this a path request naming `dest`?
fn is_path_request_for(node: &EmbeddedNode, data: &[u8], dest: &[u8; TRUNCATED_HASHBYTES]) -> bool {
    let pr_hash = *node.transport().path_request_hash();
    Packet::unpack(data)
        .map(|p| {
            p.flags.packet_type == PacketType::Data
                && p.destination_hash == pr_hash
                && p.data.as_slice().len() >= TRUNCATED_HASHBYTES
                && &p.data.as_slice()[..TRUNCATED_HASHBYTES] == dest
        })
        .unwrap_or(false)
}

/// The links of the multi-link interface that were handed a path
/// request for `dest`, in delivery order.
fn links_asked(
    node: &EmbeddedNode,
    log: &LinkLog,
    dest: &[u8; TRUNCATED_HASHBYTES],
) -> Vec<[u8; TRUNCATED_HASHBYTES]> {
    log.borrow()
        .iter()
        .filter(|(_, data)| is_path_request_for(node, data, dest))
        .map(|(link, _)| *link)
        .collect()
}

/// How many path requests for `dest` the single-link interface carried.
fn serial_asks(
    node: &EmbeddedNode,
    serial: &MockInterface,
    dest: &[u8; TRUNCATED_HASHBYTES],
) -> usize {
    serial
        .sent
        .iter()
        .filter(|data| is_path_request_for(node, data, dest))
        .count()
}

/// THE rig failure, one board wide: a path request that arrives on the
/// host's BLE link is re-originated on the t114's BLE link and on
/// serial, and never back down the link it came from.
#[test]
fn a_request_from_one_link_reaches_the_other_link_of_the_same_interface() {
    let mut board = make_board();
    let dest = unknown_destination();
    let request = foreign_path_request(&dest);

    let mut serial = MockInterface::new("serial_usb", SERIAL as u8);
    let (mut ble, ble_log) = MultiLinkInterface::new("ble", BLE, &[HOST, T114]);
    let ifac_configs: BTreeMap<usize, IfacConfig> = BTreeMap::new();

    let out = board.handle_packet_from_peer(InterfaceId(BLE), HOST, &request);
    {
        let mut ifaces: [&mut dyn Interface; 2] = [&mut serial, &mut ble];
        let dispatched = dispatch_actions(&mut ifaces, out.actions, &ifac_configs);
        assert!(
            dispatched.is_clean(),
            "the re-origination loses nothing: {:?}",
            dispatched.loss().map(|l| std::format!("{}", l))
        );
    }

    assert_eq!(
        links_asked(&board, &ble_log, &dest),
        std::vec![T114],
        "the request goes out on the link that did NOT hear it, and on \
         that link alone"
    );
    assert_eq!(
        serial_asks(&board, &serial, &dest),
        1,
        "and still on every other interface, unchanged"
    );
}

/// The six retries at 5 s the rig logged: they carry the same ingress
/// link, so a retry is not the place the exclusion silently widens back
/// to the whole interface.
#[test]
fn the_discovery_retry_excludes_the_same_link() {
    let mut board = make_board();
    let dest = unknown_destination();
    let request = foreign_path_request(&dest);

    let mut serial = MockInterface::new("serial_usb", SERIAL as u8);
    let (mut ble, ble_log) = MultiLinkInterface::new("ble", BLE, &[HOST, T114]);
    let ifac_configs: BTreeMap<usize, IfacConfig> = BTreeMap::new();

    let out = board.handle_packet_from_peer(InterfaceId(BLE), HOST, &request);
    {
        let mut ifaces: [&mut dyn Interface; 2] = [&mut serial, &mut ble];
        let _ = dispatch_actions(&mut ifaces, out.actions, &ifac_configs);
    }
    ble_log.borrow_mut().clear();
    serial.sent.clear();

    board
        .transport()
        .clock()
        .advance(DISCOVERY_RETRY_INTERVAL_MS + 1);
    let out = board.handle_timeout();
    {
        let mut ifaces: [&mut dyn Interface; 2] = [&mut serial, &mut ble];
        let _ = dispatch_actions(&mut ifaces, out.actions, &ifac_configs);
    }

    assert_eq!(
        links_asked(&board, &ble_log, &dest),
        std::vec![T114],
        "the retry asks the other link too, not just the interfaces the \
         first pass reached"
    );
    assert_eq!(
        serial_asks(&board, &serial, &dest),
        1,
        "and the retry still reaches serial once"
    );
}

/// Control: a request arriving on an interface with no link of its own
/// leaves the multi-link interface flooded as before. The exclusion
/// must not leak into packets that were never heard there.
#[test]
fn control_a_request_from_another_interface_still_floods_every_link() {
    let mut board = make_board();
    let dest = unknown_destination();
    let request = foreign_path_request(&dest);

    let mut serial = MockInterface::new("serial_usb", SERIAL as u8);
    let (mut ble, ble_log) = MultiLinkInterface::new("ble", BLE, &[HOST, T114]);
    let ifac_configs: BTreeMap<usize, IfacConfig> = BTreeMap::new();

    let out = board.handle_packet(InterfaceId(SERIAL), &request);
    {
        let mut ifaces: [&mut dyn Interface; 2] = [&mut serial, &mut ble];
        let _ = dispatch_actions(&mut ifaces, out.actions, &ifac_configs);
    }

    assert_eq!(
        links_asked(&board, &ble_log, &dest),
        std::vec![HOST, T114],
        "both links hear a request that arrived on the serial interface"
    );
    assert_eq!(
        serial_asks(&board, &serial, &dest),
        0,
        "and never back onto the requestor's own interface"
    );
}

/// Control: an interface that does not name the arrival link is
/// excluded whole, exactly as before. The finer exclusion is the
/// interface's own statement, not something the core assumes.
#[test]
fn control_without_a_named_link_the_whole_interface_is_excluded() {
    let mut board = make_board();
    let dest = unknown_destination();
    let request = foreign_path_request(&dest);

    let mut serial = MockInterface::new("serial_usb", SERIAL as u8);
    let (mut ble, ble_log) = MultiLinkInterface::new("ble", BLE, &[HOST, T114]);
    let ifac_configs: BTreeMap<usize, IfacConfig> = BTreeMap::new();

    // `handle_packet`, not `handle_packet_from_peer`: the driver said
    // nothing about which link these bytes came from.
    let out = board.handle_packet(InterfaceId(BLE), &request);
    {
        let mut ifaces: [&mut dyn Interface; 2] = [&mut serial, &mut ble];
        let _ = dispatch_actions(&mut ifaces, out.actions, &ifac_configs);
    }

    assert!(
        links_asked(&board, &ble_log, &dest).is_empty(),
        "no link named, no link served: the whole interface stays out"
    );
    assert_eq!(
        serial_asks(&board, &serial, &dest),
        1,
        "the other interfaces are reached either way"
    );
}
