//! The pure half of the Columba BLE interface: link admission, per-link
//! framing state, keepalive and expiry policy, and the outbound fan-out
//! plan. No BlueZ, no tokio, no clock — the caller passes `now_ms` and
//! performs what the returned values ask for, which is what makes every
//! state here reachable from a host test (the same split as
//! `leviculum-ble-tx` and for the same reason).
//!
//! Wire rules are not restated here: fragmentation and reassembly come
//! from `leviculum_core::framing::ble`, the advertisement parse and the
//! connection-direction rule from `leviculum_ble_tx::peer`. This module
//! adds only what a dual-role host with more than one live link needs on
//! top: who is admitted, who is expired, and which bytes go to which link.
//!
//! # One interface, one broadcast domain
//!
//! All live BLE links belong to one Reticulum interface. An outbound
//! packet fans out to every link (the firmware's fan-out answers the same
//! question — see the #255 seam report); inbound packets from every link
//! feed the same interface. This also matches what BlueZ can actually do
//! on the peripheral side: a GATT notification goes to every subscribed
//! central at once, so the peripheral TX pipe *is* a broadcast domain and
//! [`TxPlan::notify_fragments`] is computed once at the minimum MTU across
//! peripheral links rather than per link.

use leviculum_ble_tx::{
    addr_value, parse_peer_advertisement, should_initiate, ConnectDecision, ScanMode,
    MANUFACTURER_DATA_LEN,
};
use leviculum_core::framing::ble::{
    fragment_packet, BleDefragmenter, DefragResult, FRAGMENT_HEADER_SIZE, KEEPALIVE_INTERVAL_MS,
    MIN_MTU,
};

/// A BLE address in display order (`AA:BB:CC:DD:EE:FF` → `[0xAA, …]`),
/// the byte order `bluer::Address` carries.
pub(crate) type Addr = [u8; 6];

/// The identity hash a Columba peer publishes and handshakes with.
pub(crate) type IdentityHash = [u8; 16];

/// Our capability flags: dual-role, full capability (v0.3.0 flags 0x00).
/// Advertised in the manufacturer record *and* fed to the connection
/// decision from one constant, so the two cannot disagree (the firmware
/// pins its `LOCAL_CAPS` the same way).
pub(crate) const LOCAL_CAPS: u8 = 0x00;

/// Default cap on simultaneous BLE links, both roles counted together.
/// A policy bound, not a resource one: BlueZ has no SoftDevice-style hard
/// connection slots, but every link costs airtime and the protocol's
/// practical ceiling is 3-4 reliable links (see
/// `docs/src/concepts/bluetooth-interfaces.md`). Matches the firmware's
/// `MAX_LINKS`.
pub(crate) const DEFAULT_MAX_LINKS: usize = 4;

/// A link whose peer has been silent this long is torn down. Three missed
/// keepalives at the protocol's 15 s cadence: one lost keepalive must not
/// cost a link, and BlueZ surfaces no supervision-timeout event for
/// peripheral-role links, so this timer is the only down-detector that
/// covers both roles.
pub(crate) const LINK_TIMEOUT_MS: u64 = 3 * KEEPALIVE_INTERVAL_MS;

/// A central that connects but never writes its 16-byte identity is
/// disconnected after this long — the reference's
/// `_pending_identity_timeout` (`ble-reticulum@07d94130` `BLEInterface.py`, `_pending_identity_timeout`).
pub(crate) const HANDSHAKE_TIMEOUT_MS: u64 = 30_000;

/// An existing link whose peer has sent no *real* data (keepalives do not
/// count) for this long may be displaced by a fresh link carrying the
/// same identity — the reference's `_zombie_timeout`
/// (`ble-reticulum@07d94130` `BLEInterface.py`, `_zombie_timeout`). This is how a peer that rotated its BLE
/// address and reconnected wins against its own stale session.
pub(crate) const ZOMBIE_TIMEOUT_MS: u64 = 30_000;

/// Our side of a link: `Central` when we initiated the connection,
/// `Peripheral` when the peer connected to our GATT server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Central,
    Peripheral,
}

impl Role {
    /// Stable token for the structured log lines.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Role::Central => "central",
            Role::Peripheral => "peripheral",
        }
    }
}

/// One live, handshaked link.
pub(crate) struct Link {
    pub(crate) identity: IdentityHash,
    pub(crate) addr: Addr,
    pub(crate) role: Role,
    /// Negotiated ATT MTU for this link, updated when the carrier reports
    /// a newer value.
    pub(crate) mtu: usize,
    defrag: BleDefragmenter,
    last_heard_ms: u64,
    last_real_data_ms: u64,
    last_keepalive_tx_ms: u64,
}

/// Why a link was (not) admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admission {
    Accept,
    /// The peer presented our own identity — we connected to ourselves
    /// through some reflective path. Firmware: `BLE_LINK_SELF`.
    RejectSelf,
    /// The identity is already live on another link that is still fresh.
    /// Firmware: `BLE_LINK_DUP`.
    RejectDuplicate,
    /// `max_links` reached.
    RejectFull,
}

/// What one inbound carrier frame amounted to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Inbound {
    /// Sub-header-size frame: keepalive (or noise). Filtered before
    /// reassembly, per the firmware's rule (columba.rs: anything shorter
    /// than the 5-byte fragment header is ignored); the reference's
    /// explicit `len == 1 && data[0] == 0x00` check is a subset.
    Keepalive,
    /// Fragment consumed, packet not yet complete.
    NeedMore,
    /// A whole Reticulum packet, reassembled.
    Packet(Vec<u8>),
    /// A central completed the 16-byte identity handshake and the link is
    /// now live. The driver should log its `BLE_LINK_UP` — and, when the
    /// admission displaced a zombie link with the same identity,
    /// disconnect the displaced device.
    HandshakeComplete {
        identity: IdentityHash,
        displaced: Option<(IdentityHash, Addr, Role)>,
    },
    /// A central's handshake was rejected; the driver must disconnect the
    /// device. The old link, if the rejection displaced nothing, stays.
    HandshakeRejected(Admission),
    /// Frame from an address with no link and no valid handshake — a peer
    /// writing data before identifying itself. Ignored.
    NotHandshaked,
    /// The defragmenter rejected the frame.
    Error,
}

/// A peripheral-role connection that has not handshaked yet.
#[derive(Debug)]
struct Pending {
    addr: Addr,
    since_ms: u64,
}

/// Links and pending handshakes torn down by [`LinkTable::expire`]. The
/// driver disconnects the named devices; the table has already forgotten
/// them.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Expired {
    /// `(identity, addr, role)` of each timed-out link.
    pub(crate) links: Vec<(IdentityHash, Addr, Role)>,
    /// Addresses whose handshake never arrived.
    pub(crate) pending: Vec<Addr>,
}

/// The outbound fan-out for one Reticulum packet: what to write to the
/// shared notify pipe, and what to write to each central-role link.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct TxPlan {
    /// Fragments for the peripheral-side notify pipe, fragmented at the
    /// minimum MTU across peripheral links. Empty when no peripheral link
    /// is live.
    pub(crate) notify_fragments: Vec<Vec<u8>>,
    /// Per central-role link: the address and its fragments at that
    /// link's own MTU.
    pub(crate) central: Vec<(Addr, Vec<Vec<u8>>)>,
}

/// Which links are due a keepalive.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct KeepalivePlan {
    /// Write one keepalive byte to the notify pipe (it reaches every
    /// subscribed central at once).
    pub(crate) notify: bool,
    /// Central-role links due an RX write.
    pub(crate) central: Vec<Addr>,
}

/// The link table: every live link and every pending peripheral-side
/// handshake, owned by the interface task.
pub(crate) struct LinkTable {
    own_identity: IdentityHash,
    max_links: usize,
    links: Vec<Link>,
    pending: Vec<Pending>,
    /// Reassemblies a link discarded before completion, queued for the
    /// driver's `BLE_RX_ABANDON` lines (#373): `(identity, lost,
    /// running total)` in frame order. Each entry is one or more whole
    /// Reticulum packets this receiver lost — a torn or interleaved
    /// fragment stream from the peer — which before the line existed
    /// was invisible on every surface.
    abandon_reports: Vec<(IdentityHash, u32, u32)>,
}

impl LinkTable {
    pub(crate) fn new(own_identity: IdentityHash, max_links: usize) -> Self {
        Self {
            own_identity,
            max_links: max_links.max(1),
            links: Vec::new(),
            pending: Vec::new(),
            abandon_reports: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn link_count(&self) -> usize {
        self.links.len()
    }

    pub(crate) fn is_full(&self) -> bool {
        self.links.len() >= self.max_links
    }

    pub(crate) fn link_by_addr(&self, addr: &Addr) -> Option<&Link> {
        self.links.iter().find(|l| &l.addr == addr)
    }

    fn link_mut_by_addr(&mut self, addr: &Addr) -> Option<&mut Link> {
        self.links.iter_mut().find(|l| &l.addr == addr)
    }

    /// Whether a scanner hit at this address already belongs to a live
    /// link or a pending handshake (then it must not be dialled again).
    pub(crate) fn knows_addr(&self, addr: &Addr) -> bool {
        self.links.iter().any(|l| &l.addr == addr) || self.pending.iter().any(|p| &p.addr == addr)
    }

    /// Whether `identity` still owns a live link (Codeberg #365).
    ///
    /// Decides whether a link removal is a real peer loss the transport
    /// must hear about (drop the paths via that peer) or a same-identity
    /// churn — a zombie displaced by its own reconnect — where the peer
    /// is still reachable and the paths must stay.
    pub(crate) fn knows_identity(&self, identity: &IdentityHash) -> bool {
        self.links.iter().any(|l| &l.identity == identity)
    }

    /// Admission check + insert, shared by both roles.
    ///
    /// Duplicate handling is identity-keyed, never address-keyed, because
    /// addresses rotate (v2.2 §"Why Not Use MAC Addresses as Keys?"). A
    /// live link with the same identity blocks the newcomer *unless* it
    /// has gone zombie — no real data for [`ZOMBIE_TIMEOUT_MS`] — in
    /// which case the newcomer displaces it (the reference's
    /// `_check_duplicate_identity`, `ble-reticulum@07d94130`
    /// `BLEInterface.py`). The
    /// displaced link's `(identity, addr, role)` is returned so the
    /// driver can disconnect the stale device.
    #[allow(clippy::type_complexity)]
    pub(crate) fn admit(
        &mut self,
        identity: IdentityHash,
        addr: Addr,
        role: Role,
        mtu: usize,
        now_ms: u64,
    ) -> (Admission, Option<(IdentityHash, Addr, Role)>) {
        if identity == self.own_identity {
            return (Admission::RejectSelf, None);
        }
        let mut displaced = None;
        if let Some(pos) = self.links.iter().position(|l| l.identity == identity) {
            let existing = &self.links[pos];
            if now_ms.saturating_sub(existing.last_real_data_ms) < ZOMBIE_TIMEOUT_MS {
                return (Admission::RejectDuplicate, None);
            }
            let old = self.links.remove(pos);
            displaced = Some((old.identity, old.addr, old.role));
        }
        if self.links.len() >= self.max_links {
            return (Admission::RejectFull, displaced);
        }
        self.links.push(Link {
            identity,
            addr,
            role,
            mtu: mtu.max(MIN_MTU),
            defrag: BleDefragmenter::new(),
            last_heard_ms: now_ms,
            last_real_data_ms: now_ms,
            last_keepalive_tx_ms: now_ms,
        });
        (Admission::Accept, displaced)
    }

    /// Remove the link at `addr` (carrier reported it down). Returns what
    /// was removed, for the `BLE_LINK_DOWN` log line.
    pub(crate) fn remove_by_addr(&mut self, addr: &Addr) -> Option<(IdentityHash, Addr, Role)> {
        self.pending.retain(|p| &p.addr != addr);
        let pos = self.links.iter().position(|l| &l.addr == addr)?;
        let old = self.links.remove(pos);
        Some((old.identity, old.addr, old.role))
    }

    /// A frame written to our RX characteristic (we are the peripheral).
    ///
    /// `mtu` is the exchanged ATT MTU BlueZ reports with the write; it
    /// keeps the link's fragment sizing current. The first 16-byte write
    /// from an unknown address is the identity handshake
    /// (BLE_PROTOCOL_v2.2 §Identity Handshake); after the handshake a
    /// 16-byte frame is ordinary fragment traffic, the firmware's
    /// rig-proven reading (columba.rs gates on `!handshake_done`) — the
    /// reference consumes *every* 16-byte frame from a known address,
    /// which would eat a real 16-byte tail fragment at small MTUs.
    pub(crate) fn peripheral_frame(
        &mut self,
        addr: Addr,
        mtu: usize,
        data: &[u8],
        now_ms: u64,
    ) -> Inbound {
        if self.link_by_addr(&addr).is_some() {
            if let Some(link) = self.link_mut_by_addr(&addr) {
                if mtu >= MIN_MTU {
                    link.mtu = mtu;
                }
            }
            return self.link_frame(addr, data, now_ms);
        }
        // No live link at this address: only a handshake opens one.
        if data.len() == 16 {
            let mut identity = [0u8; 16];
            identity.copy_from_slice(data);
            self.pending.retain(|p| p.addr != addr);
            let (admission, displaced) = self.admit(identity, addr, Role::Peripheral, mtu, now_ms);
            return match admission {
                Admission::Accept => Inbound::HandshakeComplete {
                    identity,
                    displaced,
                },
                other => Inbound::HandshakeRejected(other),
            };
        }
        // Track the connection so a peer that never identifies itself is
        // eventually disconnected instead of camping for free.
        if !self.pending.iter().any(|p| p.addr == addr) {
            self.pending.push(Pending {
                addr,
                since_ms: now_ms,
            });
        }
        if data.len() < FRAGMENT_HEADER_SIZE {
            Inbound::Keepalive
        } else {
            Inbound::NotHandshaked
        }
    }

    /// A notification from a peer we are connected to as central.
    pub(crate) fn central_frame(&mut self, addr: Addr, data: &[u8], now_ms: u64) -> Inbound {
        if self.link_by_addr(&addr).is_none() {
            return Inbound::NotHandshaked;
        }
        self.link_frame(addr, data, now_ms)
    }

    fn link_frame(&mut self, addr: Addr, data: &[u8], now_ms: u64) -> Inbound {
        let Some(link) = self.link_mut_by_addr(&addr) else {
            return Inbound::NotHandshaked;
        };
        link.last_heard_ms = now_ms;
        // Keepalives are filtered before reassembly and do not count as
        // real data for the zombie rule (`ble-reticulum@07d94130` `BLEInterface.py`, `_zombie_timeout`).
        if data.len() < FRAGMENT_HEADER_SIZE {
            return Inbound::Keepalive;
        }
        link.last_real_data_ms = now_ms;
        let before = link.defrag.abandoned_count();
        let result = link.defrag.process(data, now_ms);
        if matches!(result, DefragResult::Error) {
            // Hard reset, as the firmware does: a garbage frame amid a
            // reassembly must not leave a stale head for the next
            // packet's tail to complete (#255) — and the head it
            // discards is a loss this link must report.
            link.defrag.abandon();
        }
        let after = link.defrag.abandoned_count();
        let report =
            (after != before).then(|| (link.identity, after.saturating_sub(before), after));
        if let Some(report) = report {
            self.abandon_reports.push(report);
        }
        match result {
            DefragResult::Complete(packet) => Inbound::Packet(packet),
            DefragResult::NeedMore => Inbound::NeedMore,
            DefragResult::Error => Inbound::Error,
        }
    }

    /// Drain the queued reassembly-loss reports (#373); the driver turns
    /// each into one `BLE_RX_ABANDON` line.
    pub(crate) fn take_abandon_reports(&mut self) -> Vec<(IdentityHash, u32, u32)> {
        std::mem::take(&mut self.abandon_reports)
    }

    /// Fan one outbound Reticulum packet out to every live link.
    pub(crate) fn plan_tx(&self, packet: &[u8]) -> TxPlan {
        let mut plan = TxPlan::default();
        let periph_mtu = self
            .links
            .iter()
            .filter(|l| l.role == Role::Peripheral)
            .map(|l| l.mtu)
            .min();
        if let Some(mtu) = periph_mtu {
            plan.notify_fragments = fragment_packet(packet, mtu);
        }
        for link in self.links.iter().filter(|l| l.role == Role::Central) {
            plan.central
                .push((link.addr, fragment_packet(packet, link.mtu)));
        }
        plan
    }

    /// Which links are due a keepalive at `now_ms`; marks them sent.
    ///
    /// The notify pipe reaches every subscribed central at once, so one
    /// due peripheral link triggers one shared keepalive and rearms all
    /// of them.
    pub(crate) fn keepalives_due(&mut self, now_ms: u64) -> KeepalivePlan {
        let mut plan = KeepalivePlan::default();
        let periph_due = self.links.iter().any(|l| {
            l.role == Role::Peripheral
                && now_ms.saturating_sub(l.last_keepalive_tx_ms) >= KEEPALIVE_INTERVAL_MS
        });
        for link in &mut self.links {
            match link.role {
                Role::Peripheral if periph_due => {
                    link.last_keepalive_tx_ms = now_ms;
                }
                Role::Central
                    if now_ms.saturating_sub(link.last_keepalive_tx_ms)
                        >= KEEPALIVE_INTERVAL_MS =>
                {
                    link.last_keepalive_tx_ms = now_ms;
                    plan.central.push(link.addr);
                }
                _ => {}
            }
        }
        plan.notify = periph_due;
        plan
    }

    /// Tear down silent links and overdue handshakes.
    pub(crate) fn expire(&mut self, now_ms: u64) -> Expired {
        let mut expired = Expired::default();
        self.links.retain(|l| {
            if now_ms.saturating_sub(l.last_heard_ms) >= LINK_TIMEOUT_MS {
                expired.links.push((l.identity, l.addr, l.role));
                false
            } else {
                true
            }
        });
        self.pending.retain(|p| {
            if now_ms.saturating_sub(p.since_ms) >= HANDSHAKE_TIMEOUT_MS {
                expired.pending.push(p.addr);
                false
            } else {
                true
            }
        });
        expired
    }
}

// ---------------------------------------------------------------------
// Scan decision — the shared parser and rule on BlueZ-shaped inputs
// ---------------------------------------------------------------------

/// The Columba service UUID, `37145b00-442d-4a94-917f-8f42c5da28e3`.
pub(crate) const SERVICE_UUID_U128: u128 = 0x37145b00_442d_4a94_917f_8f42c5da28e3;

/// The same UUID in AD-structure byte order (little-endian), as
/// `parse_peer_advertisement` compares it.
pub(crate) const SERVICE_UUID_LE: [u8; 16] = SERVICE_UUID_U128.to_le_bytes();

/// The advertised device name, `LN-<hex8>` — the firmware's own
/// derivation (`leviculum_ble_tx::device_name`) over the daemon identity,
/// so a scanner listing shows lnsd exactly like a board.
pub(crate) fn local_name(identity: &IdentityHash) -> String {
    String::from_utf8_lossy(&leviculum_ble_tx::device_name(identity)).into_owned()
}

/// A BLE address in display order as the number the v2.2 sort compares.
/// `leviculum_ble_tx::addr_value` takes the wire (LSB-first) order; a
/// `bluer::Address` is the displayed order, so it is reversed here once.
pub(crate) fn addr_value_display(addr: &Addr) -> u64 {
    let mut le = *addr;
    le.reverse();
    addr_value(&le)
}

/// What one scanner sighting resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScanDecision {
    pub(crate) decision: ConnectDecision,
    /// Whether the peer carried a readable v0.3.0 capability record
    /// (`caps_record=` in the `BLE_SCAN_DECISION` line; when 0, `caps`
    /// is meaningless).
    pub(crate) caps_record: bool,
    pub(crate) caps: u8,
}

/// Decide who connects, from the properties BlueZ hands a scanner.
///
/// BlueZ parses the advertisement for us — the service list into a UUID
/// set, the manufacturer record into a `CID → payload` map — so the raw
/// PDU the shared parser wants is rebuilt from those parts and run
/// through `parse_peer_advertisement` + `should_initiate` unchanged.
/// Rebuilding costs a few bytes on the stack and buys the exact
/// rig-proven version gate and decision table instead of a second
/// reading of them.
///
/// Returns `None` when the sighting does not offer the Columba service
/// (not a peer; no decision to log).
pub(crate) fn decide_from_scan(
    local_addr: &Addr,
    peer_addr: &Addr,
    offers_service: bool,
    manufacturer_ffff: Option<&[u8]>,
) -> Option<ScanDecision> {
    let mut pdu: Vec<u8> = Vec::with_capacity(2 + 16 + 2 + MANUFACTURER_DATA_LEN + 2);
    if offers_service {
        pdu.push(17); // 1 type byte + 16 UUID bytes
        pdu.push(0x07); // Complete List of 128-bit Service Class UUIDs
        pdu.extend_from_slice(&SERVICE_UUID_LE);
    }
    if let Some(payload) = manufacturer_ffff {
        // BlueZ strips the company ID into the map key; the AD structure
        // carries it in front of the payload.
        let data_len = 2 + payload.len();
        if let Ok(len_byte) = u8::try_from(1 + data_len) {
            pdu.push(len_byte);
            pdu.push(0xFF); // Manufacturer Specific Data
            pdu.extend_from_slice(&0xFFFFu16.to_le_bytes());
            pdu.extend_from_slice(payload);
        }
    }
    let parsed = parse_peer_advertisement(&pdu, &SERVICE_UUID_LE);
    if !parsed.offers_service {
        return None;
    }
    // Always the strict rule: the #375 fallback needs a clock over the
    // whole search (how long since the last initiate verdict or link),
    // and lnsd's scanner does not keep one yet — this batch wires the
    // fallback into the firmware's central task only. Until lnsd grows
    // the same clock it can, like any node, sit out the sort when every
    // permitted peer is dark; a later batch decides that.
    let decision = should_initiate(
        LOCAL_CAPS,
        addr_value_display(local_addr),
        parsed.caps,
        addr_value_display(peer_addr),
        ScanMode::Strict,
    );
    Some(ScanDecision {
        decision,
        caps_record: parsed.caps.is_some(),
        caps: parsed.caps.unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_core::framing::ble::payload_per_fragment;

    const ID_A: IdentityHash = [0xA1; 16];
    const ID_B: IdentityHash = [0xB2; 16];
    const OWN: IdentityHash = [0x0E; 16];
    const ADDR_1: Addr = [0xC0, 0x00, 0x00, 0x00, 0x00, 0x01];
    const ADDR_2: Addr = [0xC0, 0x00, 0x00, 0x00, 0x00, 0x02];
    const ADDR_3: Addr = [0xC0, 0x00, 0x00, 0x00, 0x00, 0x03];

    fn table() -> LinkTable {
        LinkTable::new(OWN, DEFAULT_MAX_LINKS)
    }

    /// lnsd advertises under the same name a board with the same identity
    /// would: the firmware's `device_name` derivation, byte for byte.
    #[test]
    fn local_name_matches_the_firmware_derivation() {
        let hash: IdentityHash = [
            0xa1, 0xb2, 0xc3, 0xd4, 0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, 0x88, 0x77, 0x66,
            0x55, 0x44,
        ];
        assert_eq!(local_name(&hash), "LN-a1b2c3d4");
        assert_eq!(
            local_name(&hash).as_bytes(),
            leviculum_ble_tx::device_name(&hash)
        );
    }

    /// The peer-loss report decision (Codeberg #365): a removal that
    /// leaves the identity without any live link is a real loss; a
    /// zombie displaced by its own reconnect is not.
    #[test]
    fn knows_identity_separates_peer_loss_from_same_identity_churn() {
        let mut t = table();
        let (adm, _) = t.admit(ID_A, ADDR_1, Role::Peripheral, 100, 0);
        assert_eq!(adm, Admission::Accept);
        assert!(t.knows_identity(&ID_A));
        assert!(!t.knows_identity(&ID_B));

        // Same identity reappears on a fresh address after the old link
        // went zombie: displaced, but the peer is still linked — no
        // peer-loss report.
        let (adm, displaced) = t.admit(ID_A, ADDR_2, Role::Peripheral, 100, ZOMBIE_TIMEOUT_MS + 1);
        assert_eq!(adm, Admission::Accept);
        assert_eq!(
            displaced.map(|(id, addr, _)| (id, addr)),
            Some((ID_A, ADDR_1))
        );
        assert!(
            t.knows_identity(&ID_A),
            "displacement is churn, not loss: the identity still owns a link"
        );

        // The real loss: the only link goes away.
        let removed = t.remove_by_addr(&ADDR_2);
        assert_eq!(removed.map(|(id, ..)| id), Some(ID_A));
        assert!(
            !t.knows_identity(&ID_A),
            "after the last link is gone the loss must be reportable"
        );
    }

    /// The #373 receiver-side visibility: a torn/interleaved fragment
    /// stream that costs a partial packet queues one report the driver
    /// turns into a `BLE_RX_ABANDON` line, and the second packet still
    /// reassembles. Before the report existed the discard was a silent
    /// `NeedMore`.
    #[test]
    fn an_abandoned_reassembly_is_reported_and_the_next_packet_survives() {
        let mut t = table();
        let (adm, _) = t.admit(ID_A, ADDR_1, Role::Central, 185, 0);
        assert_eq!(adm, Admission::Accept);

        let a = vec![0xAA; 300];
        let b = vec![0xBB; 300];
        let frags_a = fragment_packet(&a, 185);
        let frags_b = fragment_packet(&b, 185);
        assert_eq!(frags_a.len(), 2);

        // A's head, then B's whole stream: A is discarded, B completes.
        assert_eq!(t.central_frame(ADDR_1, &frags_a[0], 100), Inbound::NeedMore);
        assert!(t.take_abandon_reports().is_empty(), "A is still pending");
        assert_eq!(t.central_frame(ADDR_1, &frags_b[0], 101), Inbound::NeedMore);
        assert_eq!(
            t.take_abandon_reports(),
            vec![(ID_A, 1, 1)],
            "the torn head is one reported loss"
        );
        assert_eq!(
            t.central_frame(ADDR_1, &frags_b[1], 102),
            Inbound::Packet(b)
        );
        assert!(
            t.take_abandon_reports().is_empty(),
            "completion never counts"
        );
    }

    /// The peer-up report decision (Codeberg #365), the mirror of the
    /// loss side above: `admit` displaces same-identity links only, so
    /// an `Accept` with `displaced == None` is exactly the moment an
    /// identity gains its FIRST link — `knows_identity` flips true and
    /// the orchestrator starts counting the link as known. That is the
    /// condition the interface reports peer-up on; an `Accept` that
    /// carries a displacement is the phone's random-address relink
    /// (churn), and reporting it would spray one pull per ~60 s.
    #[test]
    fn admit_without_displacement_is_the_peer_up_report_point() {
        let mut t = table();

        assert!(!t.knows_identity(&ID_A));
        let (adm, displaced) = t.admit(ID_A, ADDR_1, Role::Peripheral, 100, 0);
        assert_eq!(adm, Admission::Accept);
        assert!(
            displaced.is_none(),
            "first link: no displacement — this admission is reported as peer-up"
        );
        assert!(
            t.knows_identity(&ID_A),
            "the report point IS the moment the identity becomes known"
        );

        // The relink: same identity, fresh random address, old link gone
        // zombie. Accepted, but with a displacement — churn, no report.
        let (adm, displaced) = t.admit(ID_A, ADDR_2, Role::Peripheral, 100, ZOMBIE_TIMEOUT_MS + 1);
        assert_eq!(adm, Admission::Accept);
        assert!(
            displaced.is_some(),
            "same-identity relink carries the displacement — not a peer-up"
        );

        // After a real loss the next admission is a first link again and
        // must be reported, or the flap-recovery pull never fires.
        let _ = t.remove_by_addr(&ADDR_2);
        assert!(!t.knows_identity(&ID_A));
        let (adm, displaced) = t.admit(ID_A, ADDR_1, Role::Central, 100, 2 * ZOMBIE_TIMEOUT_MS);
        assert_eq!(adm, Admission::Accept);
        assert!(
            displaced.is_none(),
            "reconnect after a loss is a first link again: peer-up reported"
        );
    }

    /// The v2.2 §Connection Direction worked example, through the
    /// display-order conversion: `B8:27:EB:A8:A7:22` reads as
    /// 0xB827EBA8A722 and the lower Pi initiates.
    #[test]
    fn addr_value_display_reads_like_the_spec_reads_the_hex_string() {
        let pi1: Addr = [0xB8, 0x27, 0xEB, 0xA8, 0xA7, 0x22];
        let pi2: Addr = [0xB8, 0x27, 0xEB, 0x10, 0x28, 0xCD];
        assert_eq!(addr_value_display(&pi1), 0xB827_EBA8_A722);
        assert_eq!(addr_value_display(&pi2), 0xB827_EB10_28CD);
        assert!(addr_value_display(&pi2) < addr_value_display(&pi1));
    }

    /// The rebuilt-PDU path produces the same decisions as the shared
    /// table: dual-role record, no record, peripheral-only override.
    #[test]
    fn scan_decision_reuses_the_shared_parser_and_rule() {
        let local: Addr = [0x18, 0x69, 0x45, 0x42, 0xAA, 0x0E];
        let higher: Addr = [0xC0, 0x00, 0x00, 0x00, 0x00, 0x01];

        // Dual-role record, our public address sorts below static random.
        let d =
            decide_from_scan(&local, &higher, true, Some(&[0x03, 0x00])).expect("service offered");
        assert_eq!(d.decision, ConnectDecision::InitiateLowerAddress);
        assert!(d.caps_record);
        assert_eq!(d.caps, 0x00);

        // No manufacturer record: same sort, caps_record=0 (v2.2 peer).
        let d = decide_from_scan(&local, &higher, true, None).expect("service offered");
        assert_eq!(d.decision, ConnectDecision::InitiateLowerAddress);
        assert!(!d.caps_record);

        // Peripheral-only override beats a losing sort.
        let lower: Addr = [0x00, 0x00, 0x00, 0x00, 0x00, 0x01];
        let d =
            decide_from_scan(&local, &lower, true, Some(&[0x03, 0x01])).expect("service offered");
        assert_eq!(d.decision, ConnectDecision::InitiatePeripheralOnlyPeer);
        assert!(d.decision.initiate());

        // An older record version is ignored → treated as no record.
        let d =
            decide_from_scan(&local, &lower, true, Some(&[0x02, 0x01])).expect("service offered");
        assert!(!d.caps_record);
        assert_eq!(d.decision, ConnectDecision::WaitPeerHasLowerAddress);

        // No service, no decision.
        assert_eq!(decide_from_scan(&local, &higher, false, None), None);
    }

    #[test]
    fn central_handshake_is_admit_then_traffic() {
        let mut t = table();
        let (adm, displaced) = t.admit(ID_A, ADDR_1, Role::Central, 185, 1_000);
        assert_eq!(adm, Admission::Accept);
        assert!(displaced.is_none());
        assert_eq!(t.link_count(), 1);

        // A single-fragment packet round-trips through the link.
        let packet = b"announce bytes".to_vec();
        let frags = fragment_packet(&packet, 185);
        assert_eq!(
            t.central_frame(ADDR_1, &frags[0], 2_000),
            Inbound::Packet(packet)
        );
    }

    #[test]
    fn own_identity_is_rejected_as_self() {
        let mut t = table();
        let (adm, _) = t.admit(OWN, ADDR_1, Role::Central, 185, 0);
        assert_eq!(adm, Admission::RejectSelf);
        assert_eq!(t.link_count(), 0);
    }

    #[test]
    fn a_fresh_duplicate_identity_is_rejected_a_zombie_is_displaced() {
        let mut t = table();
        t.admit(ID_A, ADDR_1, Role::Central, 185, 0);
        // Real data at t=1000 keeps the link fresh.
        let frags = fragment_packet(b"data", 185);
        t.central_frame(ADDR_1, &frags[0], 1_000);

        // Same identity from a rotated address, link still fresh: reject.
        let (adm, displaced) = t.admit(ID_A, ADDR_2, Role::Peripheral, 185, 10_000);
        assert_eq!(adm, Admission::RejectDuplicate);
        assert!(displaced.is_none());

        // Keepalives do not refresh the zombie clock.
        t.central_frame(ADDR_1, &[0x00], 40_000);
        let (adm, displaced) = t.admit(ID_A, ADDR_2, Role::Peripheral, 185, 40_000);
        assert_eq!(adm, Admission::Accept, "zombie displaced");
        assert_eq!(displaced, Some((ID_A, ADDR_1, Role::Central)));
        assert_eq!(t.link_count(), 1);
        assert_eq!(
            t.link_by_addr(&ADDR_2).map(|l| l.role),
            Some(Role::Peripheral)
        );
    }

    #[test]
    fn max_links_bounds_both_roles_together() {
        let mut t = LinkTable::new(OWN, 2);
        assert_eq!(
            t.admit([1; 16], ADDR_1, Role::Central, 185, 0).0,
            Admission::Accept
        );
        assert_eq!(
            t.admit([2; 16], ADDR_2, Role::Peripheral, 185, 0).0,
            Admission::Accept
        );
        assert!(t.is_full());
        assert_eq!(
            t.admit([3; 16], ADDR_3, Role::Central, 185, 0).0,
            Admission::RejectFull
        );
    }

    #[test]
    fn peripheral_handshake_opens_the_link_and_data_flows() {
        let mut t = table();
        // Pre-handshake data is ignored, keepalives tolerated.
        assert_eq!(
            t.peripheral_frame(ADDR_1, 185, &[0x01, 0, 0, 0, 1, 0xAA], 100),
            Inbound::NotHandshaked
        );
        assert_eq!(
            t.peripheral_frame(ADDR_1, 185, &[0x00], 200),
            Inbound::Keepalive
        );
        assert_eq!(t.link_count(), 0);

        // The 16-byte identity write opens the link.
        assert_eq!(
            t.peripheral_frame(ADDR_1, 185, &ID_B, 300),
            Inbound::HandshakeComplete {
                identity: ID_B,
                displaced: None
            }
        );
        assert_eq!(
            t.link_by_addr(&ADDR_1).map(|l| l.role),
            Some(Role::Peripheral)
        );

        // After the handshake a 16-byte frame is ordinary traffic, not a
        // repeated handshake (firmware reading; see peripheral_frame).
        let sixteen = [0x04; 16];
        assert_eq!(
            t.peripheral_frame(ADDR_1, 185, &sixteen, 400),
            Inbound::Error
        );

        // Multi-fragment reassembly across writes.
        let packet: Vec<u8> = (0..300).map(|i| (i % 251) as u8).collect();
        let frags = fragment_packet(&packet, 185);
        assert!(frags.len() > 1);
        let mut last = Inbound::NeedMore;
        for frag in &frags {
            last = t.peripheral_frame(ADDR_1, 185, frag, 500);
        }
        assert_eq!(last, Inbound::Packet(packet));
    }

    /// The #372 question, answered for lnsd: the peripheral side holds
    /// two centrals CONCURRENTLY. Both handshake, both links are live at
    /// once, their interleaved fragment streams reassemble on separate
    /// per-link state, the outbound fan-out serves both through the
    /// shared notify pipe at the smaller MTU, and losing one central
    /// leaves the other untouched. (The BlueZ layer above imposes no
    /// stricter limit: the advertisement stays registered while centrals
    /// are connected, writes arrive keyed by device address, and one
    /// notify reaches every subscriber — so this table IS the admission
    /// bound, `max_links` = 4 by default.)
    #[test]
    fn two_centrals_hold_concurrent_peripheral_links() {
        let mut t = table();
        assert_eq!(
            t.peripheral_frame(ADDR_1, 185, &ID_A, 100),
            Inbound::HandshakeComplete {
                identity: ID_A,
                displaced: None
            }
        );
        assert_eq!(
            t.peripheral_frame(ADDR_2, 23, &ID_B, 200),
            Inbound::HandshakeComplete {
                identity: ID_B,
                displaced: None
            },
            "the second central is admitted while the first is live"
        );
        assert_eq!(t.link_count(), 2);
        assert!(t.knows_identity(&ID_A) && t.knows_identity(&ID_B));

        // Interleaved multi-fragment traffic from both centrals: each
        // link reassembles on its own defragmenter, nothing crosses.
        let pkt_a: Vec<u8> = vec![0xA5; 300];
        let pkt_b: Vec<u8> = vec![0x5B; 300];
        let frags_a = fragment_packet(&pkt_a, 185);
        let frags_b = fragment_packet(&pkt_b, 23);
        assert_eq!(
            t.peripheral_frame(ADDR_1, 185, &frags_a[0], 300),
            Inbound::NeedMore
        );
        let mut last_b = Inbound::NeedMore;
        for frag in &frags_b {
            last_b = t.peripheral_frame(ADDR_2, 23, frag, 301);
        }
        assert_eq!(last_b, Inbound::Packet(pkt_b));
        assert_eq!(
            t.peripheral_frame(ADDR_1, 185, &frags_a[1], 302),
            Inbound::Packet(pkt_a),
            "B's whole stream in between did not touch A's reassembly"
        );
        assert!(t.take_abandon_reports().is_empty());

        // Outbound: one shared notify pipe, fragmented at the MINIMUM
        // peripheral MTU so the smaller subscriber gets whole fragments.
        let out: Vec<u8> = vec![0x77; 100];
        let plan = t.plan_tx(&out);
        assert_eq!(
            plan.notify_fragments.len(),
            100usize.div_ceil(payload_per_fragment(23))
        );
        assert!(plan.central.is_empty());

        // One central disconnecting is one loss; the other link stands.
        assert_eq!(t.remove_by_addr(&ADDR_2).map(|(id, ..)| id), Some(ID_B));
        assert!(!t.knows_identity(&ID_B));
        assert!(t.knows_identity(&ID_A), "the first central is untouched");
        assert_eq!(t.link_count(), 1);
    }

    #[test]
    fn a_rejected_peripheral_handshake_does_not_open_a_link() {
        let mut t = table();
        assert_eq!(
            t.peripheral_frame(ADDR_1, 185, &OWN, 100),
            Inbound::HandshakeRejected(Admission::RejectSelf)
        );
        assert_eq!(t.link_count(), 0);
    }

    #[test]
    fn tx_plan_fans_out_per_role_and_mtu() {
        let mut t = table();
        t.admit(ID_A, ADDR_1, Role::Central, 517, 0);
        t.admit(ID_B, ADDR_2, Role::Peripheral, 185, 0);
        t.admit([3; 16], ADDR_3, Role::Peripheral, 23, 0);

        let packet: Vec<u8> = vec![0x55; 400];
        let plan = t.plan_tx(&packet);

        // Central link fragments at its own MTU: 400 <= 509, one fragment.
        assert_eq!(plan.central.len(), 1);
        assert_eq!(plan.central[0].0, ADDR_1);
        assert_eq!(plan.central[0].1.len(), 1);

        // Notify pipe fragments at the MINIMUM peripheral MTU (23), so
        // the smallest subscriber still receives whole fragments.
        let expected = 400usize.div_ceil(payload_per_fragment(23));
        assert_eq!(plan.notify_fragments.len(), expected);

        // No peripheral links → no notify fragments.
        let mut t2 = table();
        t2.admit(ID_A, ADDR_1, Role::Central, 185, 0);
        assert!(t2.plan_tx(&packet).notify_fragments.is_empty());
    }

    #[test]
    fn keepalives_fire_per_cadence_and_shared_notify_rearms_all_peripherals() {
        let mut t = table();
        t.admit(ID_A, ADDR_1, Role::Central, 185, 0);
        t.admit(ID_B, ADDR_2, Role::Peripheral, 185, 0);
        t.admit([3; 16], ADDR_3, Role::Peripheral, 185, 5_000);

        // Before the interval: nothing due.
        let plan = t.keepalives_due(KEEPALIVE_INTERVAL_MS - 1);
        assert_eq!(plan, KeepalivePlan::default());

        // At the interval: central link due, and the first peripheral
        // link pulls the shared notify keepalive (rearming both).
        let plan = t.keepalives_due(KEEPALIVE_INTERVAL_MS);
        assert!(plan.notify);
        assert_eq!(plan.central, vec![ADDR_1]);

        // Immediately after: nothing due again, including the second
        // peripheral link that was rearmed by the shared send.
        let plan = t.keepalives_due(KEEPALIVE_INTERVAL_MS + 1);
        assert_eq!(plan, KeepalivePlan::default());
    }

    #[test]
    fn silent_links_and_stale_handshakes_expire() {
        let mut t = table();
        t.admit(ID_A, ADDR_1, Role::Central, 185, 0);
        t.peripheral_frame(ADDR_2, 185, &[0xFF; 20], 1_000); // pending, never identifies

        // A keepalive at 30s keeps the link alive past one interval.
        t.central_frame(ADDR_1, &[0x00], 30_000);

        let e = t.expire(31_000);
        assert_eq!(e.pending, vec![ADDR_2], "handshake timed out");
        assert!(e.links.is_empty(), "link was heard 1s ago");

        let e = t.expire(30_000 + LINK_TIMEOUT_MS);
        assert_eq!(e.links, vec![(ID_A, ADDR_1, Role::Central)]);
        assert_eq!(t.link_count(), 0);
    }

    #[test]
    fn peripheral_write_updates_the_link_mtu() {
        let mut t = table();
        t.peripheral_frame(ADDR_1, 23, &ID_B, 0);
        assert_eq!(t.link_by_addr(&ADDR_1).map(|l| l.mtu), Some(23));
        // MTU renegotiation surfaces on the next write.
        t.peripheral_frame(ADDR_1, 185, &[0x00], 100);
        assert_eq!(t.link_by_addr(&ADDR_1).map(|l| l.mtu), Some(185));
    }
}
