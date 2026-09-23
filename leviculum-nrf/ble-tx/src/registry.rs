//! The per-interface peer-link registry: which 16-byte identity each
//! live link slot belongs to, and the first/last-link rules that decide
//! when a link transition is a *peer* transition (Codeberg #365).
//!
//! A Reticulum BLE interface is one broadcast domain carrying several
//! links, and the core is only told about peers, not links: the
//! identity's FIRST link up is a peer arrival (the main loop pulls the
//! peer's delivery path), the identity's LAST link down is a peer loss
//! (the main loop culls the paths via that peer). A same-identity link
//! on another slot — a peer whose rotated address reconnected while its
//! old link was still up — is registry churn on both edges and must
//! report neither.
//!
//! Which of the two links that peer keeps is [`PeerRegistry::link_up`]'s
//! second job, and the rule is [`judge_duplicate`] (#360 round 2): an
//! old link its peer has abandoned — silent on EVERY frame, keepalives
//! included, for [`LINK_ABANDONED_MS`] — loses to the newcomer; a pair
//! whose two connections carry the SAME role (a rotated address dialled
//! twice in one direction) is decided by us alone, because the peer's
//! role preference cannot name one of two connections it holds in one
//! role; and otherwise the arbitration is [`preferred_ble_role`],
//! Columba's own function, computed from the PEER's perspective so both
//! ends of the pair keep the same connection. The losing link is torn
//! down by the caller immediately, never left to the expiry. The
//! registry carries two clocks per slot: the liveness clock
//! ([`PeerRegistry::note_heard`]), fed by the caller's inbound path for
//! EVERY frame, keepalives included, exactly as lnsd's `LinkTable`
//! feeds `last_heard_ms` — the duplicate rule's abandonment test and
//! the expiry sweeps both read it, the latter through
//! [`PeerRegistry::silence_ms`] — and the payload clock
//! ([`PeerRegistry::note_data`]), fed for non-keepalive frames only,
//! which since round 2 is reported on every duplicate decision and
//! consulted by none (the 2026-09-12 field T114 showed payload recency
//! deciding AGAINST the phone's own arbitration, see
//! [`judge_duplicate`]).
//!
//! The rules are pure and their failure modes are sequences (a flap, a
//! displacement, the runtime carrier-off teardown that drops every live
//! link at once), so they live here with the crate's other host-tested
//! state machines; the firmware wraps one instance in a
//! critical-section mutex and reports what the return values tell it to
//! (`leviculum_nrf::ble::columba`).

use leviculum_core::framing::ble::KEEPALIVE_INTERVAL_MS;

use crate::adv::{identity_hint, IDENTITY_HINT_LEN};

/// One interface's live links, indexed by the link's drain-table slot —
/// the same index that selects its outbound queue, so the drain table,
/// the fan-out and this registry can never disagree about which links
/// exist.
///
/// Two facts per slot, learned at different moments: the CONNECTION
/// address ([`conn_up`](Self::conn_up)), known the instant the link
/// exists in either role, and the peer IDENTITY
/// ([`link_up`](Self::link_up)), known only after the handshake or the
/// characteristic read. The address side exists for the scanner
/// (#375 §0): Core Spec Vol 6 Part B §4.5 permits only one connection
/// between two device addresses — an initiator "shall not send a
/// connection request to an advertiser it is already connected to",
/// and an advertiser "shall ignore" one from a device it is connected
/// to — so a dial to a live link's address can never succeed and must
/// be excluded BEFORE it spends five seconds timing out on the air.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerRegistry<const N: usize> {
    /// This node's own 16-byte identity hash — the tie-break input of
    /// [`preferred_ble_role`], set once at interface bring-up
    /// ([`set_local_identity`](Self::set_local_identity)). Zeroed until
    /// then, which only ever loses an identity tie-break, never a link.
    local_identity: [u8; 16],
    slots: [Option<[u8; 16]>; N],
    addrs: [Option<u64>; N],
    last_heard_ms: [u64; N],
    /// When the slot's link last delivered real payload — a fragment
    /// frame, never a keepalive and never the handshake — `None` until
    /// it first does. Reported with every duplicate decision, consulted
    /// by none since #360 round 2.
    last_data_ms: [Option<u64>; N],
    /// The slot's connection's ATT MTU as registered at
    /// [`link_up`](Self::link_up) — the OLD-link MTU input of the
    /// duplicate rule when the identity presents a second connection.
    att_mtus: [u16; N],
    /// Who opened the slot's connection, as registered at
    /// [`link_up`](Self::link_up) — the OLD-link half of
    /// [`judge_duplicate`]'s role map. A slot holding no identity holds
    /// a meaningless value here, which nothing reads: the rule asks for
    /// it only about a slot it just found this identity on, and that
    /// slot went through `link_up`.
    origins: [Origin; N],
}

/// A link that has delivered NOTHING — no packet fragment, no
/// keepalive — for this long is dead, and is torn down: it is the
/// EXPIRY bound, on both stacks (lnsd's `LinkTable::expire`, the
/// firmware session's `link_silent` arm), and since #382 it decides
/// nothing else. [`judge_duplicate`] does not read it — its abandonment
/// bound is the shorter [`LINK_ABANDONED_MS`], and it fires only when
/// the identity presents a new connection — while a link that stops
/// answering is removed by the sweep, whether or not anybody dials it.
///
/// Three missed keepalives at the protocol's 15 s cadence
/// (`leviculum_core::framing::ble::KEEPALIVE_INTERVAL_MS`). One
/// constant, imported by lnsd rather than duplicated, so the two stacks
/// cannot disagree about when a link is over.
///
/// It replaced a 30 s clock that measured PAYLOAD silence only
/// (Codeberg #382). The measurement that killed that clock: over 14.1 h
/// beside a Columba phone (`ble-accept-rns/lnsd.log`, 2026-08-30) the
/// gaps between received non-keepalive packets from a peer that was
/// demonstrably present throughout ran to a median of 51 s, a 90th
/// percentile of 182 s and a maximum of 5590 s, and 502 links outlived
/// 45 s without one byte of payload. Payload silence is what an idle
/// phone looks like; it is not evidence of anything. Keepalives are,
/// and the same log shows them arriving: only 2 of those 502 links were
/// ever closed by the silence timer.
pub const LINK_TIMEOUT_MS: u64 = 3 * KEEPALIVE_INTERVAL_MS;

/// Who opened the connection whose duplicate identity is being judged.
///
/// Round 1 of #360 removed it from the rule's inputs; round 2 restores
/// it — not as a preference (the 2026-09-09 field failures stand:
/// "incoming wins" and "outgoing wins" were each wrong in one
/// direction) but as the ROLE MAP [`judge_duplicate`] needs to compute
/// the peer's own arbitration: the peer's CENTRAL connection of a
/// duplicate pair is the one we did NOT dial, so this value, inverted,
/// is which side of `preferredBleRole` each connection sits on. It is
/// needed for BOTH connections, which is why the registry keeps it per
/// slot — the two are not always opposite. Core Spec Vol 6 Part B §4.5
/// forbids a second connection to the same ADDRESS, and identities
/// outlive addresses: a peer that rotates its RPA can be dialled again
/// by us while our first dial is live (two `Outgoing`), and can dial us
/// again from a fresh RPA while its first dial is live (two
/// `Incoming`). Then the peer holds both connections in one role, its
/// role preference cannot name one of them, and [`judge_duplicate`]
/// answers on its own ([`DupRule::SameRole`]). Every duplicate log line
/// carries the NEWCOMER's value as `origin=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The PEER connected to us (we are the peripheral; on lnsd,
    /// `Role::Peripheral`).
    Incoming,
    /// WE dialled the peer (we are the central; on lnsd,
    /// `Role::Central`).
    Outgoing,
}

/// The role a node keeps when it holds both connections of a duplicate
/// pair — [`preferred_ble_role`]'s answer, in the deciding node's own
/// perspective.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BleRole {
    /// The connection that node dialled.
    Central,
    /// The connection its peer dialled.
    Peripheral,
}

/// Columba's `BleConstants.MIN_USABLE_MTU`: the usable bytes of the
/// un-negotiated default ATT MTU (23 minus the 3-byte ATT header), and
/// the value Columba substitutes for a connection whose MTU its
/// bookkeeping has not recorded yet
/// (`centralPeerMtus[address] ?: BleConstants.MIN_USABLE_MTU`,
/// Columba `KotlinBLEBridge.kt`). The 2026-09-12 field decision turned
/// on exactly this substitution: the phone arbitrated with the fresh
/// connection still at `MTU=20` although our ATT exchange had long
/// settled at 517, so a fresh connection's MTU in the PEER's ledger is
/// this floor, not the wire's truth.
pub const MIN_USABLE_MTU: u16 = 20;

/// Columba's `BleConstants.MAX_ATTRIBUTE_VALUE_LENGTH`: the longest
/// attribute value the Bluetooth specification defines, and the CEILING
/// of the usable length Columba compares
/// (`BleConstants.kt:68`, applied in `usableValueLength` at `:87-88`).
/// It bites at the top of Columba's own `MAX_MTU = 517`
/// (`BleConstants.kt:65`): the phone reads that exchange as 512, not
/// 514.
pub const MAX_ATTRIBUTE_VALUE_LENGTH: u16 = 512;

/// A raw ATT MTU as the usable per-write payload Columba compares —
/// `usableValueLength(rawAttMtu) = (rawAttMtu - ATT_HEADER_SIZE)
/// .coerceIn(MIN_USABLE_MTU, MAX_ATTRIBUTE_VALUE_LENGTH)` (Columba
/// `BleConstants.kt:87-88`), ported with both bounds.
///
/// The floor makes an un-negotiated connection (ATT MTU still 23) and
/// Columba's not-yet-bookkept substitute the same number, which is the
/// point of [`MIN_USABLE_MTU`]. The ceiling matters at exactly one
/// place, and there it decides: two connections of one identity at ATT
/// 517 and ATT 515 are 512 == 512 to the phone — a tie, broken by
/// identity — while an unclamped 514 > 512 would have us break it by
/// MTU, and the pair then keeps different links. Copying the peer's
/// rule is only true if the clamp is copied too.
pub const fn usable_mtu(att_mtu: u16) -> u16 {
    let usable = att_mtu.saturating_sub(3);
    if usable < MIN_USABLE_MTU {
        MIN_USABLE_MTU
    } else if usable > MAX_ATTRIBUTE_VALUE_LENGTH {
        MAX_ATTRIBUTE_VALUE_LENGTH
    } else {
        usable
    }
}

/// An old link that has delivered NOTHING — no payload, no keepalive —
/// for this long when its identity presents a new connection has been
/// abandoned by its peer, and the newcomer wins outright
/// ([`judge_duplicate`], #360 round 2).
///
/// Two keepalive intervals: Columba sends a 1-byte keepalive every
/// 15 s on every connection it holds, so one whole missed interval
/// plus the interval in progress is the earliest instant "it stopped
/// keepaliving this link" is a fact rather than phase noise. This is
/// the honest liveness test round 1's payload window was not — the
/// 2026-09-12 field T114 displaced a link the phone was actively
/// keepaliving because its last PAYLOAD was 15 185 ms old (a quiet
/// link is what an idle phone looks like), while the phone's own
/// arbitration kept it, and each side then tore down the link the
/// other had kept: 45 s of dead air every cycle. Any-frame silence
/// past this bound cannot be an idle phone — an idle phone still
/// keepalives — so the abandoned case is the one case the peer's own
/// arbitration never sees and never contradicts.
pub const LINK_ABANDONED_MS: u64 = 2 * KEEPALIVE_INTERVAL_MS;

/// Columba's arbitration of a duplicate pair, ported verbatim:
/// `preferredBleRole(centralMtu, peripheralMtu, localIdentity,
/// peerIdentity)` (Columba `KotlinBLEBridge.kt:44`, applied at
/// `:1725-1760`, the 2026-09-12 decision at `:1749`) — the node keeps
/// the role with the larger usable MTU and breaks the tie by identity
/// order: `localIdentity < peerIdentity` keeps central.
///
/// `central_mtu`/`peripheral_mtu` are the deciding node's USABLE MTUs
/// of its central- and peripheral-role connection of the pair, with
/// [`MIN_USABLE_MTU`] substituted for one it has not bookkept
/// ([`usable_mtu`] performs both conversions). Columba compares the
/// identities as lowercase-hex strings; fixed-width hex of a byte
/// array orders exactly as the byte array, so the `[u8; 16]`
/// comparison below is the same comparison.
///
/// Ported because the only duplicate rule that never leaves the pair
/// linkless is one both sides compute identically from the same
/// inputs: whatever this function answers, it must be OUR answer too,
/// or each side tears down the link the other kept (the 2026-09-12
/// field failure, #360 round 2).
pub fn preferred_ble_role(
    central_mtu: u16,
    peripheral_mtu: u16,
    local_identity: &[u8; 16],
    peer_identity: &[u8; 16],
) -> BleRole {
    if central_mtu > peripheral_mtu {
        BleRole::Central
    } else if peripheral_mtu > central_mtu {
        BleRole::Peripheral
    } else if local_identity < peer_identity {
        BleRole::Central
    } else {
        BleRole::Peripheral
    }
}

/// Which branch of [`judge_duplicate`] fired — the `rule=` token every
/// duplicate log line carries since #360 round 2, so a capture states
/// not just the outcome but the reasoning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DupRule {
    /// The old link was silent past [`LINK_ABANDONED_MS`]: abandoned,
    /// newcomer wins.
    Abandoned,
    /// [`preferred_ble_role`] decided on the MTU comparison.
    ColumbaMtu,
    /// The MTUs tied; [`preferred_ble_role`] decided on identity order.
    ColumbaIdentity,
    /// Both connections carry the SAME role, so the peer holds both in
    /// one role and [`preferred_ble_role`] — which chooses between a
    /// central and a peripheral — has no opinion to copy. We decide
    /// alone, and the decision is the one the peer cannot contradict:
    /// see [`judge_duplicate`].
    SameRole,
    /// The old link's MTU was not known, so the peer's arbitration
    /// could not be computed — the caller keeps both links and sends
    /// on the old one until it is ([`DupVerdict::Wait`]).
    WaitingMtu,
}

impl DupRule {
    /// The stable `rule=` log token.
    pub fn as_str(self) -> &'static str {
        match self {
            DupRule::Abandoned => "abandoned",
            DupRule::ColumbaMtu => "columba_mtu",
            DupRule::ColumbaIdentity => "columba_identity",
            DupRule::SameRole => "same_role",
            DupRule::WaitingMtu => "waiting_mtu",
        }
    }
}

/// What to do with a second connection carrying an identity we already
/// hold a live link to (see [`judge_duplicate`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DupVerdict {
    /// Tear the old link down NOW — by us, whichever role, never left
    /// to the expiry — and let the newcomer take the peer over.
    KeepNew(DupRule),
    /// The old link keeps the peer; the newcomer is refused and
    /// disconnected NOW.
    KeepOld(DupRule),
    /// The old link's MTU is unknown, so the peer's arbitration cannot
    /// be computed yet: keep both links, send on the old one, and
    /// re-judge when the MTU is known (`rule=waiting_mtu`). Never
    /// produced when `old_usable_mtu` is supplied, which both current
    /// callers always do.
    Wait,
}

/// Decide a duplicate identity so that BOTH sides of the pair keep the
/// same connection (#360 round 2).
///
/// The 2026-09-12 field failure was two arbitrations pulling in
/// opposite directions: the board kept the newer connection because
/// the old one's last payload was 15 185 ms old, the phone kept its
/// central because its ledger still had the newer connection's MTU at
/// the floor, and each side then closed the link the other had kept —
/// the pair was linkless for 45 s of every cycle. The only rule with
/// no such hole is the PEER's own rule, so this function computes it:
///
/// 1. **Abandoned old link** (`old_silence_ms` — ANY frame, keepalives
///    included — at or past [`LINK_ABANDONED_MS`]): the newcomer wins.
///    This is the one case the peer's arbitration never sees — it has
///    already walked away from the old connection, stopped keepaliving
///    it, and a node that keeps it anyway strands the peer until the
///    45 s expiry (the morning failure of 5e7168a7).
/// 2. **Same-role pair** (`old_origin == new_origin`): the peer holds
///    BOTH connections in one role, so `preferredBleRole` — which
///    chooses between a central and a peripheral — has nothing to
///    arbitrate and we decide alone. Reachable because the pre-dial
///    exclusion is address-keyed while this rule is identity-keyed: a
///    peer that rotates its RPA can be dialled again by us, or dial us
///    again, while the first connection is live. Both sub-cases are
///    decided the way the peer cannot contradict:
///    - two `Outgoing` — both are OUR dials, and a second one adds no
///      reachability the live first one does not already have. Keep the
///      old; the newcomer is refused pre-handshake, so the peer never
///      learns a duplicate existed.
///    - two `Incoming` — both are the PEER's dials, and it dialled
///      again, which is what a node does with a connection it no longer
///      intends to use. Keep the new, and tear the old down ourselves
///      at once.
/// 3. **Alive opposite-role old link**: [`preferred_ble_role`] decides,
///    evaluated from the PEER's perspective — its central connection of
///    the pair is the one WE did not dial, its local identity is
///    `peer_identity` — and we keep whichever connection it keeps. Its
///    view of the two MTUs is the caller's job, and differs by
///    direction (see [`PeerRegistry::link_up`] for the argument): our
///    own dial (`Origin::Outgoing`) is judged BEFORE we handshake,
///    against the peer's ledger in which a fresh connection still reads
///    [`MIN_USABLE_MTU`]; an incoming handshake is a decision the peer
///    has ALREADY taken, with the MTU it negotiated itself — the
///    connection's ATT MTU as we observe it.
/// 4. **Unknown old MTU** (`old_usable_mtu = None`): [`DupVerdict::Wait`]
///    — the peer's arbitration cannot be computed, so nothing may be
///    torn down yet. Neither shipped caller produces it: both always
///    know both connections' ATT MTUs at the decision (see the report
///    for #360 round 2).
pub fn judge_duplicate(
    old_silence_ms: u64,
    old_origin: Origin,
    new_origin: Origin,
    old_usable_mtu: Option<u16>,
    new_usable_mtu: u16,
    local_identity: &[u8; 16],
    peer_identity: &[u8; 16],
) -> DupVerdict {
    if old_silence_ms >= LINK_ABANDONED_MS {
        return DupVerdict::KeepNew(DupRule::Abandoned);
    }
    if old_origin == new_origin {
        return match new_origin {
            Origin::Incoming => DupVerdict::KeepNew(DupRule::SameRole),
            Origin::Outgoing => DupVerdict::KeepOld(DupRule::SameRole),
        };
    }
    let Some(old_usable_mtu) = old_usable_mtu else {
        return DupVerdict::Wait;
    };
    // The peer's central connection is the one we did NOT dial.
    let (peer_central_mtu, peer_peripheral_mtu) = match new_origin {
        Origin::Outgoing => (old_usable_mtu, new_usable_mtu),
        Origin::Incoming => (new_usable_mtu, old_usable_mtu),
    };
    let keeps = preferred_ble_role(
        peer_central_mtu,
        peer_peripheral_mtu,
        peer_identity,
        local_identity,
    );
    let keeps_new = match (keeps, new_origin) {
        (BleRole::Central, Origin::Incoming) | (BleRole::Peripheral, Origin::Outgoing) => true,
        (BleRole::Central, Origin::Outgoing) | (BleRole::Peripheral, Origin::Incoming) => false,
    };
    let rule = if peer_central_mtu != peer_peripheral_mtu {
        DupRule::ColumbaMtu
    } else {
        DupRule::ColumbaIdentity
    };
    if keeps_new {
        DupVerdict::KeepNew(rule)
    } else {
        DupVerdict::KeepOld(rule)
    }
}

/// `old_data_silence_ms=` as the duplicate lines print it: the number,
/// or `never` when the old link has not carried one payload frame.
/// Its own token rather than a sentinel number, because every number in
/// that position is also a real answer (the same argument as the
/// firmware's `FreeSlots` token in its `BLE_SCAN_DECISION` line).
pub struct DataSilence(pub Option<u64>);

impl core::fmt::Display for DataSilence {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(ms) => write!(f, "{ms}"),
            None => f.write_str("never"),
        }
    }
}

/// What registering a link amounted to (see [`PeerRegistry::link_up`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkUp {
    /// Registered. `first` iff this is the identity's FIRST live link —
    /// the caller reports a peer arrival exactly then.
    Accepted { first: bool },
    /// Registered, and the identity's OLD link on `old_slot` must be
    /// torn down by the caller NOW. Never an arrival — the peer was
    /// never gone. `rule` names the [`judge_duplicate`] branch;
    /// `old_silence_ms` (the abandonment test's input) and the usable
    /// MTUs the arbitration compared are carried for the log line;
    /// `old_data_silence_ms` — how long since the old link carried real
    /// payload, `None` for never — is reported, not consulted.
    Displaced {
        old_slot: usize,
        rule: DupRule,
        old_silence_ms: u64,
        old_data_silence_ms: Option<u64>,
        old_usable_mtu: u16,
        new_usable_mtu: u16,
    },
    /// NOT registered: the identity's existing link on `old_slot` keeps
    /// the peer — it is alive (within [`LINK_ABANDONED_MS`]) and the
    /// peer's own arbitration keeps it. The caller drops THIS
    /// connection. Fields as on [`Displaced`](Self::Displaced).
    Refused {
        old_slot: usize,
        rule: DupRule,
        old_silence_ms: u64,
        old_data_silence_ms: Option<u64>,
        old_usable_mtu: u16,
        new_usable_mtu: u16,
    },
}

impl<const N: usize> Default for PeerRegistry<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> PeerRegistry<N> {
    /// No links.
    pub const fn new() -> Self {
        Self {
            local_identity: [0; 16],
            slots: [None; N],
            addrs: [None; N],
            last_heard_ms: [0; N],
            last_data_ms: [None; N],
            att_mtus: [0; N],
            origins: [Origin::Incoming; N],
        }
    }

    /// Set this node's own identity hash — the tie-break input of
    /// [`preferred_ble_role`]. Called once at interface bring-up,
    /// before any link can exist.
    pub fn set_local_identity(&mut self, identity: [u8; 16]) {
        self.local_identity = identity;
    }

    /// Register a slot's connection address — at the connection event,
    /// in either role, before any identity is known.
    pub fn conn_up(&mut self, slot: usize, addr_value: u64) {
        self.addrs[slot] = Some(addr_value);
    }

    /// Clear a slot's connection address at teardown (the counterpart
    /// of [`conn_up`](Self::conn_up); identity clearing is
    /// [`link_down`](Self::link_down)'s job).
    pub fn conn_down(&mut self, slot: usize) {
        self.addrs[slot] = None;
    }

    /// Whether a live connection was made on this address — the
    /// scanner's pre-dial exclusion (see the struct docs). Addresses
    /// rotate, so a linked PEER can still reappear under a fresh
    /// address this check cannot know — and the identity behind an
    /// advertisement is unknowable before connecting, so that residual
    /// dial is inherent. It is resolved post-connect by
    /// [`link_up`](Self::link_up)'s identity duplicate rule, which
    /// refuses such a dial outright.
    pub fn addr_linked(&self, addr_value: u64) -> bool {
        self.addrs.iter().flatten().any(|a| *a == addr_value)
    }

    /// Register a slot's peer, at `now_ms`. `origin` is who opened this
    /// connection, `att_mtu` its ATT MTU as currently negotiated
    /// (`Connection::att_mtu()` — still 23 if no exchange has landed).
    ///
    /// With no other link from that identity this is a plain
    /// [`LinkUp::Accepted`], `first` iff it is the identity's FIRST live
    /// link — the caller reports a peer arrival exactly then.
    ///
    /// A second connection from an identity we already hold is decided
    /// by [`judge_duplicate`] (#360 round 2): abandoned old link (any
    /// frame silence at or past [`LINK_ABANDONED_MS`]) → the newcomer
    /// wins; both connections in the same role → we decide alone (the
    /// peer has no central-vs-peripheral choice to make); otherwise →
    /// the PEER's own arbitration ([`preferred_ble_role`]) decides, and
    /// we keep the connection the peer keeps. The old link's origin
    /// comes from the registry, which recorded it at that link's own
    /// `link_up`. The peer's view of the new connection's MTU differs
    /// by direction:
    ///
    /// - `Origin::Outgoing` — OUR dial, judged at the identity read,
    ///   BEFORE our handshake. The peer has not seen the duplicate yet
    ///   and will arbitrate only when our handshake lands, against a
    ///   ledger in which a fresh connection still reads
    ///   [`MIN_USABLE_MTU`] (field 2026-09-12: the phone arbitrated
    ///   with `MTU=20` 5.5 s after connect, our ATT long settled at
    ///   517). So the new connection enters the comparison at the
    ///   floor, which an alive old link at any negotiated MTU beats —
    ///   and the refusal happens pre-handshake, so the peer never sees
    ///   a duplicate at all and its ledger's timing never matters.
    /// - `Origin::Incoming` — the peer's dial, judged at its handshake.
    ///   The peer has ALREADY arbitrated (a Columba central dedups when
    ///   it learns our identity, before writing its handshake; a board
    ///   central pre-refuses exactly as above, so its handshake means
    ///   its judge said the newcomer wins) with the MTU it negotiated
    ///   itself — which we observe as this connection's ATT MTU. The
    ///   residual race — its exchange completing between its decision
    ///   and its handshake reaching us — is one connection event wide.
    ///
    /// The old link's payload silence is still MEASURED and carried on
    /// both variants — round 1 consulted it, and the capture line keeps
    /// the number so a round-1-vs-round-2 comparison stays greppable —
    /// but the rule no longer reads it. Neither edge of a displacement
    /// is a peer transition — the peer was never gone — which is why
    /// `Displaced` carries no `first` flag.
    ///
    /// Re-registering the SAME slot is neither: the link the caller
    /// would tear down is the one it just kept.
    ///
    /// An accepted link starts its liveness clock here, and its payload
    /// clock at `never`: the handshake (peripheral) or the identity
    /// read (central) that got us this far proves the peer is present,
    /// not that this link carries traffic.
    pub fn link_up(
        &mut self,
        slot: usize,
        peer: [u8; 16],
        origin: Origin,
        att_mtu: u16,
        now_ms: u64,
    ) -> LinkUp {
        let old = self
            .slots
            .iter()
            .position(|id| *id == Some(peer))
            .filter(|old| *old != slot);
        if let Some(old_slot) = old {
            let old_silence_ms = now_ms.saturating_sub(self.last_heard_ms[old_slot]);
            let old_data_silence_ms =
                self.last_data_ms[old_slot].map(|last| now_ms.saturating_sub(last));
            let old_usable_mtu = usable_mtu(self.att_mtus[old_slot]);
            let new_usable_mtu = match origin {
                // The peer's ledger reads a fresh connection at the
                // floor (see the method docs).
                Origin::Outgoing => MIN_USABLE_MTU,
                Origin::Incoming => usable_mtu(att_mtu),
            };
            let verdict = judge_duplicate(
                old_silence_ms,
                self.origins[old_slot],
                origin,
                Some(old_usable_mtu),
                new_usable_mtu,
                &self.local_identity,
                &peer,
            );
            let rule = match verdict {
                DupVerdict::KeepNew(rule) | DupVerdict::KeepOld(rule) => rule,
                // Unreachable — the old MTU above is always supplied —
                // and mapped to the conservative half of "keep both":
                // the newcomer waits, the working link keeps the peer.
                DupVerdict::Wait => DupRule::WaitingMtu,
            };
            if !matches!(verdict, DupVerdict::KeepNew(_)) {
                return LinkUp::Refused {
                    old_slot,
                    rule,
                    old_silence_ms,
                    old_data_silence_ms,
                    old_usable_mtu,
                    new_usable_mtu,
                };
            }
            self.slots[slot] = Some(peer);
            self.last_heard_ms[slot] = now_ms;
            self.last_data_ms[slot] = None;
            self.att_mtus[slot] = att_mtu;
            self.origins[slot] = origin;
            return LinkUp::Displaced {
                old_slot,
                rule,
                old_silence_ms,
                old_data_silence_ms,
                old_usable_mtu,
                new_usable_mtu,
            };
        }
        let first = self.slots.iter().flatten().all(|id| *id != peer);
        self.slots[slot] = Some(peer);
        self.last_heard_ms[slot] = now_ms;
        self.last_data_ms[slot] = None;
        self.att_mtus[slot] = att_mtu;
        self.origins[slot] = origin;
        LinkUp::Accepted { first }
    }

    /// A slot's peer delivered a frame at `now_ms` — the liveness clock
    /// the expiry reads through [`silence_ms`](Self::silence_ms), and
    /// the abandonment test's input on each of
    /// [`link_up`](Self::link_up)'s duplicate decisions (#360 round 2).
    ///
    /// EVERY inbound frame, keepalives included (Codeberg #382). A
    /// keepalive is the one thing a peer with nothing to say still
    /// sends, so excluding it made a quiet peer indistinguishable from
    /// a departed one; the failure the old payload-only clock guarded
    /// against — a degraded link that still passes 1-byte writes while
    /// packet-sized ones fail — is now caught by the peer itself, which
    /// reconnects and displaces the link as an incoming duplicate.
    ///
    /// Called per FRAME, not per reassembled packet, so a long packet's
    /// fragments each count and a packet that never finishes
    /// reassembling still proves the link delivers.
    pub fn note_heard(&mut self, slot: usize, now_ms: u64) {
        self.last_heard_ms[slot] = now_ms;
    }

    /// A slot's peer delivered a real PAYLOAD frame at `now_ms` — a
    /// fragment, never the 1-byte keepalive — which feeds BOTH clocks:
    /// payload is also liveness. Since #360 round 2 the payload clock
    /// is reported with every duplicate decision and consulted by
    /// none. The caller's inbound path calls this instead of
    /// [`note_heard`](Self::note_heard) for every frame at or above the
    /// fragment-header size, per frame and before reassembly, like the
    /// liveness clock and for the same reason.
    pub fn note_data(&mut self, slot: usize, now_ms: u64) {
        self.last_heard_ms[slot] = now_ms;
        self.last_data_ms[slot] = Some(now_ms);
    }

    /// How long the slot's link has delivered nothing at all — no
    /// payload, no keepalive — at `now_ms`. `None` while the slot holds
    /// no identity: a connection that has not handshaked yet has no
    /// liveness clock to read, and the slot's previous tenant's clock
    /// is not it.
    ///
    /// The expiry sweep's input. lnsd's `LinkTable::expire` makes the
    /// same subtraction over its own rows; the firmware has no sweep
    /// task, so its per-session silence arm asks the registry directly
    /// (`leviculum_nrf::ble::columba::link_silent`). A link at or past
    /// [`LINK_TIMEOUT_MS`] here is torn down — that, not a displacement,
    /// is what clears a dead link since #382.
    pub fn silence_ms(&self, slot: usize, now_ms: u64) -> Option<u64> {
        self.slots[slot]?;
        Some(now_ms.saturating_sub(self.last_heard_ms[slot]))
    }

    /// Clear a slot. `Some(identity)` iff that took the identity's LAST
    /// live link — the caller reports a peer loss exactly then. An
    /// unclaimed slot yields `None`: no link, no loss.
    pub fn link_down(&mut self, slot: usize) -> Option<[u8; 16]> {
        let identity = self.slots[slot].take()?;
        self.last_data_ms[slot] = None;
        self.att_mtus[slot] = 0;
        self.slots
            .iter()
            .flatten()
            .all(|id| *id != identity)
            .then_some(identity)
    }

    /// Whether the identity holds a live link — the central path's
    /// duplicate check (BLE addresses rotate, identities do not).
    pub fn is_linked(&self, peer: &[u8; 16]) -> bool {
        self.slots.iter().flatten().any(|id| id == peer)
    }

    /// Whether a live link's identity begins with this advertised
    /// identity hint (#412) — the scanner's pre-dial exclusion for a
    /// peer that rotated its address.
    ///
    /// The companion of [`addr_linked`](Self::addr_linked), one level
    /// up: that one catches a peer still advertising the address we are
    /// connected on, this one catches the same peer under a fresh
    /// address — a board that rebooted, an lnsd adapter that
    /// re-registered, any advertiser of OURS that draws a new one.
    ///
    /// What it does NOT catch is the capture #412 was opened on, and
    /// the sources say so rather than the capture: Columba at 6674ae87
    /// advertises the service UUID and nothing else
    /// (`BleAdvertiser.kt:205-210`; `addManufacturerData` appears in
    /// none of its 990 .kt files), so the phone that took seven of
    /// seven outgoing links carries no capability record, yields no
    /// hint, and is dialled exactly as before. That half needs Columba
    /// to carry the hint too, or the dial ledger and role preference
    /// from #412's design comment.
    ///
    /// `None` — an advertiser that carried no hint — is never a match:
    /// a peer that said nothing about who it is keeps exactly its
    /// pre-#412 standing and is dialled as before.
    ///
    /// It is a HINT and it is used in ONE direction only: to SKIP a
    /// dial. Four bytes collide once in 2^32, and a false match costs
    /// that peer one dial cycle — it is dialled at its next rotation.
    /// Nothing durable is keyed on it: admission, duplicate
    /// arbitration and peer identity all still come from
    /// [`link_up`](Self::link_up)'s 16-byte handshake.
    #[must_use]
    pub fn hint_linked(&self, hint: Option<[u8; IDENTITY_HINT_LEN]>) -> bool {
        let Some(hint) = hint else {
            return false;
        };
        self.slots
            .iter()
            .flatten()
            .any(|id| identity_hint(id) == hint)
    }

    /// The slot of a live link to this peer, if it holds one (Codeberg
    /// #376) — the fan-out's peer-to-link map.
    ///
    /// A peer holding two links is still one peer, and either link
    /// reaches it, so the lowest slot is returned. Two links exist only
    /// during a displacement's hand-over, whose old link is
    /// already being torn down: [`Self::link_up`] registers the new slot
    /// BEFORE the old one is signalled, and the old session clears its
    /// registry entry before releasing its drain slot, so the window is a
    /// fan-out or two wide and both slots are live throughout it.
    pub fn slot_for(&self, peer: &[u8; 16]) -> Option<usize> {
        self.slots.iter().position(|id| id.as_ref() == Some(peer))
    }

    /// EVERY slot this peer holds, as a mask (Codeberg #422).
    ///
    /// [`slot_for`](Self::slot_for) answers "where do I send to it",
    /// where the first live link is as good as any. This answers "what
    /// did it already hear", and during a displacement's hand-over the
    /// answer can be two slots wide.
    pub fn slots_of(&self, peer: &[u8; 16]) -> SlotMask {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, id)| id.as_ref() == Some(peer))
            .fold(SlotMask::EMPTY, |mask, (index, _)| mask.with(index))
    }

    /// The number of DISTINCT live peer identities (Codeberg #365) —
    /// the value the main loop mirrors into the core as the
    /// interface's peer count. Distinct, not per-slot: during a
    /// displacement's hand-over (#376) one peer briefly holds two links,
    /// and it is still one peer.
    pub fn peer_count(&self) -> usize {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.as_ref().map(|id| (i, id)))
            .filter(|(i, id)| {
                self.slots[..*i]
                    .iter()
                    .flatten()
                    .all(|earlier| earlier != *id)
            })
            .count()
    }
}

/// What the core said about one outbound packet's links, the input of
/// [`plan_fanout`].
///
/// Two different statements, both about peers and neither about BLE:
/// "these bytes are FOR that peer" (the #376 delivery hint) and "that
/// peer already HEARD these bytes" (the #422 ingress link of a
/// broadcast). The core never learns what a connection handle is; it
/// names the peer identity this interface itself reported on peer-up,
/// and the mapping to links happens here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxAim {
    /// Every live link: an announce, a path request from a medium with
    /// no link of its own, anything the core neither addressed nor
    /// received on a named link.
    Flood,
    /// Addressed at one peer (Codeberg #376).
    Peer([u8; 16]),
    /// A broadcast that arrived on this peer's link (Codeberg #422):
    /// every OTHER live link, because they heard nothing. This is what
    /// keeps a path request crossing a board between two links of the
    /// one BLE interface.
    FloodExcept([u8; 16]),
}

/// The link slots a [`TxFanout::FloodExcept`] leaves out.
///
/// A set, not one index: during a displacement's hand-over (#376) one
/// peer briefly holds two slots, and the packet it already heard must
/// not come back on its second link either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SlotMask(u32);

impl SlotMask {
    /// The mask that skips nothing.
    pub const EMPTY: SlotMask = SlotMask(0);

    /// Add `slot` to the set. Slots at or above 32 cannot be
    /// represented and are ignored; `MAX_LINKS` is 4 on every board we
    /// build, and the static assert in the firmware keeps it there.
    pub fn with(self, slot: usize) -> Self {
        match u32::try_from(slot) {
            Ok(bit) if bit < u32::BITS => SlotMask(self.0 | (1 << bit)),
            _ => self,
        }
    }

    /// Is this slot skipped?
    #[must_use]
    pub fn skips(self, slot: usize) -> bool {
        match u32::try_from(slot) {
            Ok(bit) if bit < u32::BITS => self.0 & (1 << bit) != 0,
            _ => false,
        }
    }
}

/// What the core's delivery statement made of one outbound packet
/// (`leviculum_nrf::ble::tx_fanout_task`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxFanout {
    /// No hint — a broadcast (announce, path request, anything the core
    /// did not address at a named peer). Copied into every live link's
    /// queue, which is what a Reticulum interface owes a broadcast: one
    /// `try_send` reaches every peer on the medium, exactly as one LoRa
    /// transmission reaches every listener.
    Flood,
    /// Hinted, and the peer holds a live link: queue on that slot alone.
    Route(usize),
    /// Hinted at a peer with NO live link here. The packet is DROPPED,
    /// not flooded.
    ///
    /// The hint exists because the core routed these bytes at that one
    /// neighbour. The remaining links are not a route to it: flooding
    /// them spends their airtime on a packet they must forward or drop,
    /// and a neighbour that forwards it re-creates exactly the relayed
    /// duplicate the hint removes (the 2026-09-09 desk failure, where a
    /// telemetry report addressed to the phone reached it twice, once
    /// through the other board). The peer's disappearance is separately
    /// reported to the core as a peer loss (Codeberg #365), which culls
    /// the paths via it, so the next packet for that destination is
    /// routed afresh — over another interface or after a fresh path
    /// request — instead of sprayed at links that cannot deliver it.
    NoLink,
    /// A broadcast that arrived on one of these links (Codeberg #422):
    /// every live link except the masked ones.
    ///
    /// No `NoLink` counterpart: a peer whose link has since died
    /// excludes nothing, and the remaining links still never heard the
    /// packet, so the honest answer is an empty mask rather than a
    /// drop. That is the opposite decision from [`Self::NoLink`] for
    /// the opposite reason: a hint says who the bytes are FOR, this
    /// says who they are NOT for.
    FloodExcept(SlotMask),
}

/// Map the core's delivery statement onto this interface's links (see
/// [`TxFanout`]).
///
/// Pure, so the decision is host-tested; the firmware's fan-out task
/// supplies the registry and executes the answer.
pub fn plan_fanout<const N: usize>(registry: &PeerRegistry<N>, aim: TxAim) -> TxFanout {
    match aim {
        TxAim::Flood => TxFanout::Flood,
        TxAim::Peer(peer) => match registry.slot_for(&peer) {
            Some(slot) => TxFanout::Route(slot),
            None => TxFanout::NoLink,
        },
        TxAim::FloodExcept(peer) => TxFanout::FloodExcept(registry.slots_of(&peer)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [u8; 16] = [0xaa; 16];
    const B: [u8; 16] = [0xbb; 16];

    /// The 2026-09-12 field pair, first four bytes as logged: the board
    /// `b2a8bea1…` sorts below the phone `b99af2ec…` in Columba's
    /// hex-string order and therefore in ours.
    const BOARD: [u8; 16] = [0xb2, 0xa8, 0xbe, 0xa1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    const PHONE: [u8; 16] = [0xb9, 0x9a, 0xf2, 0xec, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

    /// The number itself, pinned. It is the link timeout both
    /// interfaces expire on — three missed keepalives — and since #382
    /// it is an expiry bound only.
    #[test]
    fn the_dead_link_bound_is_the_link_timeout_itself() {
        assert_eq!(LINK_TIMEOUT_MS, 45_000);
        assert_eq!(LINK_TIMEOUT_MS, 3 * KEEPALIVE_INTERVAL_MS);
    }

    /// The abandonment bound, pinned: two keepalive intervals on the
    /// ANY-FRAME clock (#360 round 2) — one whole missed interval plus
    /// the interval in progress, the honest "it stopped keepaliving
    /// this link".
    #[test]
    fn the_abandonment_bound_is_two_keepalive_intervals() {
        assert_eq!(LINK_ABANDONED_MS, 30_000);
        assert_eq!(LINK_ABANDONED_MS, 2 * KEEPALIVE_INTERVAL_MS);
    }

    /// Columba's floor and the ATT conversion: `MIN_USABLE_MTU` is the
    /// usable payload of the un-negotiated default ATT MTU, so "never
    /// exchanged" and "not yet bookkept" are the same number.
    #[test]
    fn the_mtu_floor_is_the_unnegotiated_usable_payload() {
        assert_eq!(MIN_USABLE_MTU, 20);
        assert_eq!(usable_mtu(23), MIN_USABLE_MTU);
        assert_eq!(usable_mtu(0), MIN_USABLE_MTU);
        assert_eq!(usable_mtu(185), 182);
    }

    /// The ceiling, against Columba's own unit test of
    /// `usableValueLength` (`BleConstantsTest.kt:15-18`) — the same
    /// four inputs, the same four answers, 517 included.
    #[test]
    fn the_mtu_ceiling_is_the_spec_attribute_length() {
        assert_eq!(MAX_ATTRIBUTE_VALUE_LENGTH, 512);
        assert_eq!(usable_mtu(23), 20);
        assert_eq!(usable_mtu(185), 182);
        assert_eq!(usable_mtu(247), 244);
        assert_eq!(usable_mtu(517), 512);
        // The boundary the duplicate rule turns on: the last ATT MTU
        // below the clamp, and the two above it that collapse onto it.
        assert_eq!(usable_mtu(514), 511);
        assert_eq!(usable_mtu(515), 512);
        assert_eq!(usable_mtu(516), 512);
        assert_eq!(usable_mtu(u16::MAX), MAX_ATTRIBUTE_VALUE_LENGTH);
    }

    /// The verbatim port of `preferredBleRole` (Columba
    /// KotlinBLEBridge.kt:44): the larger usable MTU's role wins, the
    /// tie goes to central iff `localIdentity < peerIdentity`.
    #[test]
    fn preferred_ble_role_is_kotlinblebridge_44() {
        assert_eq!(preferred_ble_role(514, 20, &A, &B), BleRole::Central);
        assert_eq!(preferred_ble_role(20, 514, &A, &B), BleRole::Peripheral);
        assert_eq!(preferred_ble_role(182, 182, &A, &B), BleRole::Central);
        assert_eq!(preferred_ble_role(182, 182, &B, &A), BleRole::Peripheral);
        // Equal identities cannot occur between two nodes (that is the
        // BLE_LINK_SELF check's job); the port still answers, and
        // answers peripheral, exactly as the Kotlin `<` does.
        assert_eq!(preferred_ble_role(182, 182, &A, &A), BleRole::Peripheral);
    }

    /// Columba compares its identities as lowercase-hex STRINGS; the
    /// byte-array comparison is the same order (fixed-width hex is
    /// order-preserving), checked here on the 2026-09-12 field pair.
    #[test]
    fn preferred_ble_role_identity_order_matches_columbas_hex_strings() {
        // "b2a8bea1…" < "b99af2ec…" because '2' < '9' at index 1.
        assert!(BOARD < PHONE);
        assert_eq!(
            preferred_ble_role(514, 514, &BOARD, &PHONE),
            BleRole::Central
        );
        assert_eq!(
            preferred_ble_role(514, 514, &PHONE, &BOARD),
            BleRole::Peripheral
        );
    }

    /// The judge's first branch: an old link silent past the bound has
    /// been abandoned and the newcomer wins, in both directions, with
    /// `>=` at the bound.
    #[test]
    fn an_abandoned_old_link_loses_to_the_newcomer() {
        for (old_origin, new_origin) in [
            (Origin::Incoming, Origin::Outgoing),
            (Origin::Outgoing, Origin::Incoming),
            // Same-role pairs too: abandonment is tested BEFORE the
            // role map, because an abandoned link is the one case no
            // arbitration of either side ever sees.
            (Origin::Incoming, Origin::Incoming),
            (Origin::Outgoing, Origin::Outgoing),
        ] {
            assert_eq!(
                judge_duplicate(
                    LINK_ABANDONED_MS,
                    old_origin,
                    new_origin,
                    Some(514),
                    514,
                    &BOARD,
                    &PHONE
                ),
                DupVerdict::KeepNew(DupRule::Abandoned)
            );
            assert_ne!(
                judge_duplicate(
                    LINK_ABANDONED_MS - 1,
                    old_origin,
                    new_origin,
                    Some(514),
                    514,
                    &BOARD,
                    &PHONE
                ),
                DupVerdict::KeepNew(DupRule::Abandoned)
            );
        }
    }

    /// The same-role branch, both sub-cases: a rotated RPA lets the
    /// same identity appear twice in ONE role (the pre-dial exclusion
    /// is address-keyed, this rule identity-keyed), and then the peer
    /// holds both connections in one role and `preferredBleRole` has
    /// nothing to arbitrate. Our own second dial adds no reachability
    /// and is refused; the peer's second dial is what a node does with
    /// a connection it has stopped using, so it wins.
    #[test]
    fn a_same_role_pair_is_decided_without_the_peers_arbitration() {
        assert_eq!(
            judge_duplicate(
                1_000,
                Origin::Outgoing,
                Origin::Outgoing,
                Some(514),
                MIN_USABLE_MTU,
                &BOARD,
                &PHONE
            ),
            DupVerdict::KeepOld(DupRule::SameRole),
            "our own redundant dial reaches nothing the live link does not"
        );
        assert_eq!(
            judge_duplicate(
                1_000,
                Origin::Incoming,
                Origin::Incoming,
                Some(514),
                514,
                &BOARD,
                &PHONE
            ),
            DupVerdict::KeepNew(DupRule::SameRole),
            "the peer dialled again: it is done with the old connection"
        );
    }

    /// The same-role branch answers before the MTUs are consulted at
    /// all — it must not depend on a comparison the peer never makes.
    /// The identity tie-break is likewise not reached: both directions
    /// of the pair give the same answer.
    #[test]
    fn the_same_role_branch_reads_neither_mtu_nor_identity_order() {
        for old_mtu in [None, Some(MIN_USABLE_MTU), Some(514)] {
            for new_mtu in [MIN_USABLE_MTU, 182, 514] {
                for (local, peer) in [(&BOARD, &PHONE), (&PHONE, &BOARD)] {
                    assert_eq!(
                        judge_duplicate(
                            1_000,
                            Origin::Outgoing,
                            Origin::Outgoing,
                            old_mtu,
                            new_mtu,
                            local,
                            peer
                        ),
                        DupVerdict::KeepOld(DupRule::SameRole)
                    );
                    assert_eq!(
                        judge_duplicate(
                            1_000,
                            Origin::Incoming,
                            Origin::Incoming,
                            old_mtu,
                            new_mtu,
                            local,
                            peer
                        ),
                        DupVerdict::KeepNew(DupRule::SameRole)
                    );
                }
            }
        }
    }

    /// An unknown old MTU cannot be arbitrated: Wait, both links kept,
    /// nothing torn down.
    #[test]
    fn an_unknown_old_mtu_waits() {
        assert_eq!(
            judge_duplicate(
                1_000,
                Origin::Outgoing,
                Origin::Incoming,
                None,
                514,
                &BOARD,
                &PHONE
            ),
            DupVerdict::Wait
        );
    }

    /// Tonight's decision as the judge sees it (#360, 2026-09-12): our
    /// own dial of an identity whose old link is alive enters the
    /// peer's arbitration at the floor and loses to any negotiated old
    /// MTU — `rule=columba_mtu`, keep old.
    #[test]
    fn our_dial_against_a_live_negotiated_link_loses_at_the_floor() {
        assert_eq!(
            judge_duplicate(
                7_341,
                Origin::Incoming,
                Origin::Outgoing,
                Some(514),
                MIN_USABLE_MTU,
                &BOARD,
                &PHONE
            ),
            DupVerdict::KeepOld(DupRule::ColumbaMtu)
        );
    }

    /// The identity tie-break in both directions, on an incoming
    /// handshake with both MTUs negotiated equal: the peer keeps its
    /// central (the new connection) iff its identity sorts below ours.
    #[test]
    fn an_incoming_tie_is_broken_by_columbas_identity_order() {
        // Peer PHONE (local BOARD): PHONE > BOARD, peer keeps
        // peripheral = old.
        assert_eq!(
            judge_duplicate(
                1_000,
                Origin::Outgoing,
                Origin::Incoming,
                Some(514),
                514,
                &BOARD,
                &PHONE
            ),
            DupVerdict::KeepOld(DupRule::ColumbaIdentity)
        );
        // Peer BOARD (local PHONE): BOARD < PHONE, peer keeps central
        // = new.
        assert_eq!(
            judge_duplicate(
                1_000,
                Origin::Outgoing,
                Origin::Incoming,
                Some(514),
                514,
                &PHONE,
                &BOARD
            ),
            DupVerdict::KeepNew(DupRule::ColumbaIdentity)
        );
    }

    /// An incoming connection whose negotiated MTU beats the old
    /// link's: the peer keeps its central on the MTU comparison alone.
    #[test]
    fn an_incoming_bigger_mtu_wins_on_the_mtu_comparison() {
        assert_eq!(
            judge_duplicate(
                1_000,
                Origin::Outgoing,
                Origin::Incoming,
                Some(155),
                514,
                &BOARD,
                &PHONE
            ),
            DupVerdict::KeepNew(DupRule::ColumbaMtu)
        );
    }

    #[test]
    fn the_first_link_of_an_identity_is_an_arrival_and_displaces_nothing() {
        let mut reg = PeerRegistry::<4>::new();
        assert_eq!(
            reg.link_up(0, A, Origin::Incoming, 517, 0),
            LinkUp::Accepted { first: true }
        );
        assert_eq!(
            reg.link_up(1, B, Origin::Outgoing, 517, 0),
            LinkUp::Accepted { first: true },
            "a different identity is its own arrival"
        );
    }

    /// The #360 round 2 minimal reproducer, the 2026-09-12 21:41 field
    /// T114 as a unit: the phone held a live central link (keepalive
    /// 7.3 s ago, last payload 15 185 ms ago), rotated its RPA, and our
    /// scanner dialled the new address and learned the same identity.
    /// Round 1 displaced the old link on its payload silence while the
    /// phone's own arbitration (fresh connection at the floor,
    /// KotlinBLEBridge.kt:1749) kept it — each side closed the link the
    /// other kept, 45 s of dead air. Required now: OUR dial is refused,
    /// pre-handshake, `rule=columba_mtu`, and the phone never even
    /// sees a duplicate.
    #[test]
    fn a_live_old_links_own_dial_is_refused_before_the_handshake() {
        let mut reg = PeerRegistry::<4>::new();
        reg.set_local_identity(BOARD);
        reg.link_up(0, PHONE, Origin::Incoming, 517, 0);
        reg.note_data(0, 80_115);
        reg.note_heard(0, 87_959);
        assert_eq!(
            reg.link_up(1, PHONE, Origin::Outgoing, 517, 95_300),
            LinkUp::Refused {
                old_slot: 0,
                rule: DupRule::ColumbaMtu,
                old_silence_ms: 7_341,
                old_data_silence_ms: Some(15_185),
                // ATT 517 clamped by Columba's own ceiling.
                old_usable_mtu: 512,
                new_usable_mtu: MIN_USABLE_MTU,
            },
            "the phone's ledger reads our fresh dial at the floor: old wins"
        );
        assert_eq!(reg.slot_for(&PHONE), Some(0), "the old link still holds it");
        assert_eq!(
            reg.link_down(1),
            None,
            "the refused slot was never registered"
        );
    }

    /// The 26 ms race, on the rule directly: round 1 flipped between
    /// refuse and displace on whether the old link's last payload
    /// landed just inside or just outside a 15 s window (the field
    /// decision sat at 15 185 ms, the refreshing write 26 ms short of
    /// flipping it). Round 2 does not read the payload clock, so both
    /// sides of the race produce the same verdict — the phone's.
    #[test]
    fn payload_recency_no_longer_flips_the_verdict() {
        let mut verdicts = [None, None];
        for (i, last_payload) in [95_274_u64, 80_115_u64].into_iter().enumerate() {
            let mut reg = PeerRegistry::<4>::new();
            reg.set_local_identity(BOARD);
            reg.link_up(0, PHONE, Origin::Incoming, 517, 0);
            reg.note_data(0, last_payload);
            reg.note_heard(0, 87_959);
            match reg.link_up(1, PHONE, Origin::Outgoing, 517, 95_300) {
                LinkUp::Refused { rule, .. } => verdicts[i] = Some(rule),
                other => panic!("payload at {last_payload} changed the verdict: {other:?}"),
            }
        }
        assert_eq!(verdicts[0], verdicts[1]);
        assert_eq!(verdicts[0], Some(DupRule::ColumbaMtu));
    }

    /// The morning case of 5e7168a7's doc comment, round 2 shape: the
    /// phone rotated and ABANDONED its old connection — no keepalives
    /// since — and our dial of the new address lands past the
    /// abandonment bound. The newcomer wins, `rule=abandoned`, and the
    /// old link is torn down by us now rather than at the 45 s expiry.
    #[test]
    fn an_abandoned_rotated_link_is_displaced_by_our_dial() {
        let mut reg = PeerRegistry::<4>::new();
        reg.set_local_identity(BOARD);
        reg.link_up(0, PHONE, Origin::Incoming, 517, 0);
        reg.note_heard(0, 60_000);
        assert_eq!(
            reg.link_up(1, PHONE, Origin::Outgoing, 517, 91_000),
            LinkUp::Displaced {
                old_slot: 0,
                rule: DupRule::Abandoned,
                old_silence_ms: 31_000,
                old_data_silence_ms: None,
                old_usable_mtu: 512,
                new_usable_mtu: MIN_USABLE_MTU,
            }
        );
        assert_eq!(reg.peer_count(), 1, "one peer throughout the hand-over");
        assert_eq!(reg.link_down(0), None, "the old link's death is churn");
        assert_eq!(reg.slot_for(&PHONE), Some(1), "the new link holds the peer");
    }

    /// A keepalive-fed link is ALIVE, and alive means the peer's
    /// arbitration decides — the round 1 rule displaced such a link
    /// (its payload clock was stale) and stranded the phone. The
    /// keepalive that used to be dismissed as "not active use" is
    /// exactly the evidence the peer still holds the connection.
    #[test]
    fn keepalives_keep_a_link_out_of_the_abandoned_branch() {
        let mut reg = PeerRegistry::<4>::new();
        reg.set_local_identity(BOARD);
        reg.link_up(0, PHONE, Origin::Incoming, 517, 0);
        let mut t = 0;
        for _ in 0..100 {
            t += KEEPALIVE_INTERVAL_MS;
            reg.note_heard(0, t);
            assert!(
                reg.silence_ms(0, t).is_some_and(|s| s < LINK_TIMEOUT_MS),
                "the expiry never comes for a link that keeps answering"
            );
        }
        assert!(
            matches!(
                reg.link_up(1, PHONE, Origin::Outgoing, 517, t + 1),
                LinkUp::Refused {
                    rule: DupRule::ColumbaMtu,
                    old_silence_ms: 1,
                    ..
                }
            ),
            "a keepalive one millisecond ago proves the peer holds the old link"
        );
    }

    /// The expiry's own input: no identity, no clock; a registered
    /// link starts at zero and ages from what it last delivered.
    #[test]
    fn silence_is_reported_only_for_a_slot_that_holds_a_link() {
        let mut reg = PeerRegistry::<4>::new();
        assert_eq!(
            reg.silence_ms(0, 10_000),
            None,
            "an un-handshaked connection has no liveness clock to read"
        );
        reg.link_up(0, A, Origin::Incoming, 517, 10_000);
        assert_eq!(reg.silence_ms(0, 10_000), Some(0));
        assert_eq!(
            reg.silence_ms(0, 10_000 + LINK_TIMEOUT_MS),
            Some(LINK_TIMEOUT_MS)
        );
        reg.note_heard(0, 10_000 + LINK_TIMEOUT_MS);
        assert_eq!(reg.silence_ms(0, 10_000 + LINK_TIMEOUT_MS), Some(0));
        reg.link_down(0);
        assert_eq!(
            reg.silence_ms(0, 99_000),
            None,
            "and a freed slot's stale clock is nobody's evidence"
        );
    }

    /// A same-slot re-registration must not name its own slot: the
    /// caller would refuse — or tear down — the very connection it just
    /// kept. True in both directions and on both sides of the boundary.
    #[test]
    fn re_registering_the_same_slot_neither_refuses_nor_displaces() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, Origin::Incoming, 517, 0);
        assert_eq!(
            reg.link_up(0, A, Origin::Incoming, 517, 1),
            LinkUp::Accepted { first: false }
        );
        assert_eq!(
            reg.link_up(0, A, Origin::Outgoing, 517, 10 * LINK_TIMEOUT_MS),
            LinkUp::Accepted { first: false }
        );
    }

    /// A slot's clocks belong to the link that holds it now: a fresh
    /// registration resets them, so a link inheriting a slot whose
    /// previous tenant was recently alive is not judged by that
    /// tenant's clocks — nor protected by them.
    #[test]
    fn a_new_link_starts_its_own_clocks() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, B, Origin::Incoming, 517, 0);
        reg.note_data(0, 1_000);
        reg.link_down(0);
        // B was heard 1 ms ago; A claims the slot now. A's clock starts
        // here, so a dial of A's other address 31 s later finds an
        // ABANDONED link, not B's liveness.
        reg.link_up(0, A, Origin::Incoming, 517, 1_001);
        assert!(
            matches!(
                reg.link_up(1, A, Origin::Outgoing, 517, 1_001 + LINK_ABANDONED_MS),
                LinkUp::Displaced {
                    old_slot: 0,
                    rule: DupRule::Abandoned,
                    old_silence_ms: LINK_ABANDONED_MS,
                    ..
                }
            ),
            "the previous tenant's clock is nobody's evidence"
        );
    }

    #[test]
    fn the_last_link_of_an_identity_is_a_loss() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, Origin::Incoming, 517, 0);
        assert_eq!(reg.link_down(0), Some(A));
    }

    #[test]
    fn a_non_last_link_down_is_churn_not_a_loss() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, Origin::Incoming, 517, 0);
        reg.link_up(1, A, Origin::Incoming, 517, LINK_TIMEOUT_MS);
        assert_eq!(reg.link_down(0), None);
        assert!(reg.is_linked(&A));
        assert_eq!(reg.link_down(1), Some(A));
    }

    #[test]
    fn an_unclaimed_slot_going_down_reports_nothing() {
        let mut reg = PeerRegistry::<4>::new();
        assert_eq!(reg.link_down(2), None);
    }

    #[test]
    fn the_duplicate_check_tracks_liveness() {
        let mut reg = PeerRegistry::<4>::new();
        assert!(!reg.is_linked(&A));
        reg.link_up(0, A, Origin::Incoming, 517, 0);
        assert!(reg.is_linked(&A));
        reg.link_down(0);
        assert!(!reg.is_linked(&A));
    }

    /// The runtime carrier-off teardown: `--set-media ble=off` makes
    /// every connection task drop its own link, in whatever order the
    /// executor reaches them. Every linked identity must yield exactly
    /// one loss.
    #[test]
    fn dropping_every_claimed_slot_yields_one_loss_per_identity() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, Origin::Incoming, 517, 0);
        reg.link_up(2, B, Origin::Incoming, 517, 0);
        let losses: Vec<[u8; 16]> = (0..4).filter_map(|slot| reg.link_down(slot)).collect();
        assert_eq!(losses, vec![A, B]);
        assert!(!reg.is_linked(&A));
        assert!(!reg.is_linked(&B));
    }

    /// Same teardown mid-displacement-hand-over: two links, one
    /// identity. One loss, not two — the peer left once.
    #[test]
    fn a_displaced_identity_is_lost_once_when_all_slots_drop() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, Origin::Incoming, 517, 0);
        reg.link_up(1, A, Origin::Incoming, 517, LINK_TIMEOUT_MS);
        let losses: Vec<[u8; 16]> = (0..4).filter_map(|slot| reg.link_down(slot)).collect();
        assert_eq!(losses, vec![A]);
    }

    /// The #372 shape: every slot filled by a different identity — a
    /// phone and three boards. Each is an arrival, the count says four,
    /// and losing one leaves the other three linked and untouched.
    #[test]
    fn n_identities_fill_n_slots_and_one_loss_leaves_n_minus_one() {
        const C: [u8; 16] = [0xcc; 16];
        const D: [u8; 16] = [0xdd; 16];
        let mut reg = PeerRegistry::<4>::new();
        for (slot, id) in [A, B, C, D].into_iter().enumerate() {
            assert_eq!(
                reg.link_up(slot, id, Origin::Incoming, 517, 0),
                LinkUp::Accepted { first: true },
                "each identity's first link"
            );
        }
        assert_eq!(reg.peer_count(), 4);

        assert_eq!(reg.link_down(1), Some(B), "B's only link is a loss");
        assert_eq!(reg.peer_count(), 3);
        for id in [A, C, D] {
            assert!(reg.is_linked(&id), "the others are untouched");
        }
        assert!(!reg.is_linked(&B));
    }

    /// The #375 §0 exclusion input: a connection's address is known
    /// from the connection event, before any identity arrives, and
    /// clears at teardown.
    #[test]
    fn conn_addresses_are_tracked_from_connect_to_teardown() {
        let mut reg = PeerRegistry::<4>::new();
        assert!(!reg.addr_linked(0xC0DE));

        // The rig's exact gap: connected, identity not yet presented.
        reg.conn_up(1, 0xC0DE);
        assert!(reg.addr_linked(0xC0DE), "excluded before any dial");
        assert!(!reg.addr_linked(0xBEEF));

        // Identity arrives; the address side is unaffected.
        assert_eq!(
            reg.link_up(1, A, Origin::Incoming, 517, 0),
            LinkUp::Accepted { first: true }
        );
        assert!(reg.addr_linked(0xC0DE));

        // Teardown clears both facts independently.
        assert_eq!(reg.link_down(1), Some(A));
        assert!(
            reg.addr_linked(0xC0DE),
            "identity gone, connection fact still set"
        );
        reg.conn_down(1);
        assert!(!reg.addr_linked(0xC0DE));
    }

    /// The #412 dial decision, the whole of it: a candidate whose hint
    /// matches an identity we already hold a live link to is NOT
    /// dialled, one with an unknown hint IS, and one with no hint
    /// behaves exactly as it did before the hint existed.
    ///
    /// The three cases are what the room capture is made of: `BOARD`
    /// is the live link, `PHONE` the neighbour we still want, `None`
    /// the pre-#412 board and the phone that speaks no capability
    /// record at all.
    #[test]
    fn a_hint_matching_a_live_identity_is_not_dialled_and_nothing_else_changes() {
        let mut reg = PeerRegistry::<4>::new();
        // Nothing live: no hint matches, not even a real one.
        assert!(!reg.hint_linked(Some(identity_hint(&BOARD))));
        assert!(!reg.hint_linked(None));

        reg.conn_up(1, 0xC0DE);
        assert_eq!(
            reg.link_up(1, BOARD, Origin::Incoming, 517, 0),
            LinkUp::Accepted { first: true }
        );

        // 1. The live peer under ANY address — the rotation case, which
        //    `addr_linked` cannot see.
        assert!(reg.hint_linked(Some(identity_hint(&BOARD))));
        assert!(
            !reg.addr_linked(0xFACE),
            "a fresh address is unknown to the address filter"
        );
        // 2. An unknown hint is still dialled.
        assert!(!reg.hint_linked(Some(identity_hint(&PHONE))));
        // 3. Silence is not a match: pre-#412 behaviour, unchanged.
        assert!(!reg.hint_linked(None));

        // Only the first four bytes decide — that is what is on the air.
        let mut same_prefix = BOARD;
        same_prefix[15] = 0xFF;
        assert_ne!(same_prefix, BOARD);
        assert!(
            reg.hint_linked(Some(identity_hint(&same_prefix))),
            "a four-byte collision skips the dial, which is its whole cost"
        );
        // …and one differing byte INSIDE the hint is a different peer.
        let mut other_prefix = BOARD;
        other_prefix[3] ^= 0x01;
        assert!(!reg.hint_linked(Some(identity_hint(&other_prefix))));

        // The exclusion lasts exactly as long as the link does.
        assert_eq!(reg.link_down(1), Some(BOARD));
        assert!(
            !reg.hint_linked(Some(identity_hint(&BOARD))),
            "the peer is dialled again as soon as the link is gone"
        );
    }

    /// Every live identity is checked, not just the first slot, and a
    /// connection with no identity yet excludes nothing — the hint
    /// answers about PEERS, the address filter about connections.
    #[test]
    fn the_hint_covers_every_slot_and_only_identified_ones() {
        let mut reg = PeerRegistry::<4>::new();
        reg.conn_up(0, 0x1111);
        reg.conn_up(3, 0x3333);
        // Connected, identity not yet presented: the address filter
        // holds this gap, the hint filter has nothing to say.
        assert!(reg.addr_linked(0x1111));
        assert!(!reg.hint_linked(Some(identity_hint(&A))));

        assert_eq!(
            reg.link_up(3, PHONE, Origin::Outgoing, 517, 0),
            LinkUp::Accepted { first: true }
        );
        assert!(
            reg.hint_linked(Some(identity_hint(&PHONE))),
            "the last slot"
        );
        assert!(!reg.hint_linked(Some(identity_hint(&A))));

        assert_eq!(
            reg.link_up(0, A, Origin::Incoming, 517, 0),
            LinkUp::Accepted { first: true }
        );
        assert!(reg.hint_linked(Some(identity_hint(&A))), "the first slot");
        assert!(reg.hint_linked(Some(identity_hint(&PHONE))), "and still it");
    }

    /// `identity_hint` is the first four bytes and nothing else — the
    /// value the capture prints as `peer=`, stated once for both ends.
    #[test]
    fn the_hint_is_the_leading_four_bytes_of_the_identity() {
        assert_eq!(identity_hint(&BOARD), [0xb2, 0xa8, 0xbe, 0xa1]);
        assert_eq!(identity_hint(&PHONE), [0xb9, 0x9a, 0xf2, 0xec]);
        assert_eq!(identity_hint(&A), [0xaa; IDENTITY_HINT_LEN]);
        assert_eq!(identity_hint(&B), [0xbb; IDENTITY_HINT_LEN]);
    }

    /// Two live connections on different slots: clearing one leaves the
    /// other's address linked.
    #[test]
    fn one_teardown_leaves_the_other_connection_linked() {
        let mut reg = PeerRegistry::<4>::new();
        reg.conn_up(0, 0x1111);
        reg.conn_up(2, 0x2222);
        reg.conn_down(0);
        assert!(!reg.addr_linked(0x1111));
        assert!(reg.addr_linked(0x2222));
    }

    /// The mirrored peer count is DISTINCT identities: a displaced
    /// identity on two slots is one peer, and a slot gap does not
    /// confuse the count.
    #[test]
    fn peer_count_is_distinct_identities_across_slot_gaps() {
        let mut reg = PeerRegistry::<4>::new();
        assert_eq!(reg.peer_count(), 0);
        reg.link_up(1, A, Origin::Incoming, 517, 0);
        assert_eq!(reg.peer_count(), 1);
        // The displacement hand-over: same identity on a second slot.
        reg.link_up(3, A, Origin::Incoming, 517, LINK_TIMEOUT_MS);
        assert_eq!(reg.peer_count(), 1, "two links, one peer");
        reg.link_up(0, B, Origin::Incoming, 517, LINK_TIMEOUT_MS);
        assert_eq!(reg.peer_count(), 2);
        reg.link_down(1);
        assert_eq!(reg.peer_count(), 2, "A still holds slot 3");
        reg.link_down(3);
        assert_eq!(reg.peer_count(), 1);
    }

    /// Without a hint the fan-out is a flood, whatever the registry
    /// holds: a broadcast owes every peer on the medium a copy.
    #[test]
    fn a_packet_without_a_hint_floods_every_live_link() {
        let mut reg = PeerRegistry::<4>::new();
        assert_eq!(plan_fanout(&reg, TxAim::Flood), TxFanout::Flood, "no links");
        reg.link_up(0, A, Origin::Incoming, 517, 0);
        reg.link_up(1, B, Origin::Incoming, 517, 0);
        assert_eq!(plan_fanout(&reg, TxAim::Flood), TxFanout::Flood);
    }

    /// With a hint the packet goes on the hinted peer's link and on no
    /// other — the whole point of #376 part 2.
    #[test]
    fn a_hinted_packet_takes_only_that_peers_link() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, Origin::Incoming, 517, 0);
        reg.link_up(2, B, Origin::Incoming, 517, 0);
        assert_eq!(plan_fanout(&reg, TxAim::Peer(A)), TxFanout::Route(0));
        assert_eq!(plan_fanout(&reg, TxAim::Peer(B)), TxFanout::Route(2));
    }

    /// A peer holding two links (the displacement hand-over window) is
    /// one peer: either link reaches it, and the decision picks one
    /// rather than duplicating the packet across both.
    #[test]
    fn a_peer_with_two_links_gets_the_packet_once() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(1, A, Origin::Incoming, 517, 0);
        reg.link_up(3, A, Origin::Incoming, 517, LINK_TIMEOUT_MS);
        assert_eq!(plan_fanout(&reg, TxAim::Peer(A)), TxFanout::Route(1));
    }

    /// The peer walked out between the core's routing decision and this
    /// fan-out: DROP, never a fallback flood. See [`TxFanout::NoLink`].
    #[test]
    fn a_hint_for_a_peer_with_no_link_drops_instead_of_flooding() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, B, Origin::Incoming, 517, 0);
        assert_eq!(
            plan_fanout(&reg, TxAim::Peer(A)),
            TxFanout::NoLink,
            "A is gone; B's link is not a route to A"
        );
        // And with nothing live at all it is still a drop, not a flood.
        reg.link_down(0);
        assert_eq!(plan_fanout(&reg, TxAim::Peer(A)), TxFanout::NoLink);
    }

    /// The #422 shape: a broadcast that arrived on one link goes to
    /// every OTHER live link. Without it a path request from the phone
    /// never reaches the board next to it, because both links are one
    /// `InterfaceId`.
    #[test]
    fn a_broadcast_skips_the_link_it_arrived_on() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, A, Origin::Incoming, 517, 0);
        reg.link_up(2, B, Origin::Incoming, 517, 0);
        let TxFanout::FloodExcept(mask) = plan_fanout(&reg, TxAim::FloodExcept(A)) else {
            panic!("an ingress link excludes, it does not route");
        };
        assert!(mask.skips(0), "A's link heard it already");
        assert!(!mask.skips(2), "B's link heard nothing");
    }

    /// A peer mid-displacement holds two slots, and the packet it
    /// already heard must not return on the second one either.
    #[test]
    fn a_broadcast_skips_every_link_of_the_peer_it_arrived_on() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(1, A, Origin::Incoming, 517, 0);
        reg.link_up(3, A, Origin::Incoming, 517, LINK_TIMEOUT_MS);
        reg.link_up(2, B, Origin::Incoming, 517, 0);
        let TxFanout::FloodExcept(mask) = plan_fanout(&reg, TxAim::FloodExcept(A)) else {
            panic!("an ingress link excludes, it does not route");
        };
        assert!(mask.skips(1) && mask.skips(3), "both of A's links");
        assert!(!mask.skips(2));
    }

    /// The opposite decision from `NoLink`, for the opposite reason: a
    /// peer whose link died excludes nothing, and the links that are
    /// still up never heard the packet, so they are still served.
    #[test]
    fn an_ingress_link_that_is_gone_excludes_nothing() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(0, B, Origin::Incoming, 517, 0);
        assert_eq!(
            plan_fanout(&reg, TxAim::FloodExcept(A)),
            TxFanout::FloodExcept(SlotMask::EMPTY),
            "A is gone; B still never heard this packet"
        );
    }

    /// The peer's LAST link died: `slot_for` must not keep naming the
    /// slot the teardown released.
    #[test]
    fn slot_for_forgets_a_slot_at_teardown() {
        let mut reg = PeerRegistry::<4>::new();
        reg.link_up(2, A, Origin::Incoming, 517, 0);
        assert_eq!(reg.slot_for(&A), Some(2));
        reg.link_down(2);
        assert_eq!(reg.slot_for(&A), None);
    }
}

/// The two-sided model (#360 round 2, item 3 of the batch): a board
/// running the shipped rule against a Columba stub running
/// `preferredBleRole` verbatim — including the `?: MIN_USABLE_MTU`
/// ledger lookup whose timing decided the 2026-09-12 field failure —
/// with the invariant under test spelled out per scenario: from the
/// moment the pair can be linked, no interval longer than 2 s in which
/// it holds zero usable links, and both sides keep the SAME connection.
#[cfg(test)]
mod duel {
    use super::*;

    const BOARD: [u8; 16] = [0xb2, 0xa8, 0xbe, 0xa1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    const PHONE: [u8; 16] = [0xb9, 0x9a, 0xf2, 0xec, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    /// An identity that sorts BELOW the board's, for the tie-break's
    /// other direction.
    const PHONE_LO: [u8; 16] = [0xa0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

    /// The bound every scenario asserts: the rule may not leave the
    /// pair linkless longer than this.
    const MAX_GAP_MS: u64 = 2_000;

    /// The pair's two possible connections. Conn `i` uses registry
    /// slot `i` on the board.
    #[derive(Clone, Copy)]
    struct Conn {
        /// Who dialled it, in the board's terms.
        board_origin: Origin,
        /// Raw ATT MTU after the exchange (both ends of a connection
        /// always observe the same exchanged value). Raw rather than
        /// usable, because the clamp that turns 517 into 512 is part
        /// of what is under test.
        negotiated_att: u16,
        /// When the PHONE's ledger records that value — `None` = not
        /// yet / never, which reads as the floor. This lag, not the
        /// wire, is what arbitrated on 2026-09-12 (`MTU=20` at the
        /// dedup, ATT long since at 517).
        phone_bookkept_at: Option<u64>,
    }

    struct Duel {
        board: PeerRegistry<4>,
        conns: [Conn; 2],
        board_holds: [bool; 2],
        phone_holds: [bool; 2],
        handshaked: [bool; 2],
        phone_identity: [u8; 16],
        /// (time, pair-usable) transitions, for the gap assertion.
        transitions: Vec<(u64, bool)>,
    }

    impl Duel {
        fn new(phone_identity: [u8; 16], conns: [Conn; 2]) -> Self {
            let mut board = PeerRegistry::new();
            board.set_local_identity(BOARD);
            Self {
                board,
                conns,
                board_holds: [false; 2],
                phone_holds: [false; 2],
                handshaked: [false; 2],
                phone_identity,
                transitions: Vec::new(),
            }
        }

        /// A connection delivers only when both sides still hold it and
        /// the identity handshake completed.
        fn usable(&self) -> bool {
            (0..2).any(|i| self.board_holds[i] && self.phone_holds[i] && self.handshaked[i])
        }

        fn note(&mut self, t: u64) {
            let usable = self.usable();
            if self.transitions.last().map(|&(_, u)| u) != Some(usable) {
                self.transitions.push((t, usable));
            }
        }

        /// The phone's ledger view of one connection's usable MTU at
        /// `t` — the verbatim `?: MIN_USABLE_MTU` lookup over what
        /// `usableValueLength` put in the ledger (`BleGattServer.kt:922`
        /// writes `centralMtus[address] = usableMtu`, so the clamp has
        /// already been applied to the stored number).
        fn phone_view(&self, conn: usize, t: u64) -> u16 {
            match self.conns[conn].phone_bookkept_at {
                Some(at) if at <= t => usable_mtu(self.conns[conn].negotiated_att),
                _ => MIN_USABLE_MTU,
            }
        }

        /// Columba's dedup, verbatim (KotlinBLEBridge.kt:1725-1760):
        /// on learning a duplicate identity the phone keeps
        /// `preferredBleRole`'s role and cancels the other connection
        /// WITHOUT dropping the ACL (field 2026-09-12 21:41:48.146 —
        /// "Connection cancelled" left our link up receiving
        /// refusals). Returns the kept connection.
        fn phone_dedup(&mut self, t: u64) -> usize {
            let phone_central = match self.conns[0].board_origin {
                // The board dialled conn 0, so the phone's central is
                // conn 1 — and vice versa.
                Origin::Outgoing => 1,
                Origin::Incoming => 0,
            };
            let phone_peripheral = 1 - phone_central;
            let kept = match preferred_ble_role(
                self.phone_view(phone_central, t),
                self.phone_view(phone_peripheral, t),
                &self.phone_identity,
                &BOARD,
            ) {
                BleRole::Central => phone_central,
                BleRole::Peripheral => phone_peripheral,
            };
            self.phone_holds[1 - kept] = false;
            self.note(t);
            kept
        }

        /// The board learns the peer's identity on `conn` and runs the
        /// shipped rule; the loser (if any) is closed on the board's
        /// side immediately, as the firmware does.
        fn board_link_up(&mut self, conn: usize, t: u64) -> LinkUp {
            let up = self.board.link_up(
                conn,
                self.phone_identity,
                self.conns[conn].board_origin,
                self.conns[conn].negotiated_att,
                t,
            );
            match up {
                LinkUp::Refused { .. } => {
                    self.board_holds[conn] = false;
                }
                LinkUp::Displaced { old_slot, .. } => {
                    self.board_holds[old_slot] = false;
                    self.board.link_down(old_slot);
                }
                LinkUp::Accepted { .. } => {}
            }
            self.note(t);
            up
        }

        /// The longest linkless interval from `watch_from` to `end`.
        fn max_gap(&self, watch_from: u64, end: u64) -> u64 {
            let mut gap_start = Some(watch_from);
            let mut max = 0;
            for &(t, usable) in &self.transitions {
                if t < watch_from {
                    gap_start = if usable { None } else { Some(watch_from) };
                    continue;
                }
                match (usable, gap_start) {
                    (true, Some(start)) => {
                        max = max.max(t.saturating_sub(start));
                        gap_start = None;
                    }
                    (false, None) => gap_start = Some(t),
                    _ => {}
                }
            }
            if let Some(start) = gap_start {
                max = max.max(end.saturating_sub(start));
            }
            max
        }
    }

    /// Scenario (a), tonight's 21:41 failure: the phone holds a live
    /// central link, rotates its RPA, the board dials the new address,
    /// both would learn the identity within a second — and the new
    /// connection's MTU is still at the floor in the phone's ledger.
    /// Round 1: board displaced the old link, phone kept it, 45 s dead
    /// air. Round 2: the board's dial is refused BEFORE it handshakes,
    /// the phone never sees a duplicate, and the old link carries the
    /// pair throughout — zero gap.
    #[test]
    fn scenario_a_rotation_with_the_floor_race_keeps_the_old_link() {
        let mut d = Duel::new(
            PHONE,
            [
                Conn {
                    board_origin: Origin::Incoming,
                    negotiated_att: 517,
                    phone_bookkept_at: Some(0),
                },
                Conn {
                    board_origin: Origin::Outgoing,
                    negotiated_att: 517,
                    // The race: ATT settled on the wire, the phone's
                    // ledger has not recorded it (and will not before
                    // any dedup could run).
                    phone_bookkept_at: None,
                },
            ],
        );
        // t=0: the phone's central link, up and handshaked.
        d.board_holds[0] = true;
        d.phone_holds[0] = true;
        d.handshaked[0] = true;
        assert!(matches!(
            d.board_link_up(0, 0),
            LinkUp::Accepted { first: true }
        ));
        // Keepalives every 15 s; last one 7.3 s before the decision.
        for t in [15_000, 30_000, 45_000, 60_000, 75_000, 87_959] {
            d.board.note_heard(0, t);
        }
        d.board.note_data(0, 80_115);
        // t=89 s: the phone rotates its RPA (the old connection is
        // untouched); the board's scanner dials the new address.
        d.board_holds[1] = true;
        d.phone_holds[1] = true;
        // t=95.3 s: the board reads the identity — the shipped rule.
        assert!(matches!(
            d.board_link_up(1, 95_300),
            LinkUp::Refused {
                old_slot: 0,
                rule: DupRule::ColumbaMtu,
                ..
            }
        ));
        // The board never handshakes the refused dial, so the phone
        // never learns a duplicate — but had it deduped, the verbatim
        // stub with the floor race picks the SAME connection:
        assert_eq!(
            d.phone_dedup(95_400),
            0,
            "phone (floor for the fresh conn) keeps its central = old"
        );
        assert_eq!(d.board.slot_for(&PHONE), Some(0), "board keeps old too");
        assert_eq!(
            d.max_gap(0, 120_000),
            0,
            "the old link carries the pair throughout"
        );
    }

    /// Scenario (a)'s 26 ms race, two-sided: the old link's last
    /// payload lands just before the decision in one run and 15 185 ms
    /// before it in the other (the field value — round 1's flip point,
    /// missed by 26 ms). Both runs must produce the same board verdict,
    /// and it must be the stub's.
    #[test]
    fn scenario_a_regression_the_26ms_payload_race_cannot_flip_the_pair() {
        for last_payload in [95_274_u64, 80_115_u64] {
            let mut d = Duel::new(
                PHONE,
                [
                    Conn {
                        board_origin: Origin::Incoming,
                        negotiated_att: 517,
                        phone_bookkept_at: Some(0),
                    },
                    Conn {
                        board_origin: Origin::Outgoing,
                        negotiated_att: 517,
                        phone_bookkept_at: None,
                    },
                ],
            );
            d.board_holds[0] = true;
            d.phone_holds[0] = true;
            d.handshaked[0] = true;
            d.board_link_up(0, 0);
            d.board.note_heard(0, 87_959);
            d.board.note_data(0, last_payload);
            d.board_holds[1] = true;
            d.phone_holds[1] = true;
            assert!(
                matches!(d.board_link_up(1, 95_300), LinkUp::Refused { .. }),
                "payload at {last_payload} must not flip the verdict"
            );
            assert_eq!(d.phone_dedup(95_400), 0, "and the stub agrees: old");
            assert_eq!(d.max_gap(0, 120_000), 0);
        }
    }

    /// Scenario (b), the morning case from 5e7168a7's doc comment: the
    /// phone rotates and ABANDONS the old connection (no keepalives
    /// since), the board dials the new address. The abandonment branch
    /// promotes the dial immediately and the old link is torn down by
    /// us at the decision — not at the 45 s expiry. The watch window
    /// opens at the dial's identity read: the [60 s, 91 s] hole before
    /// it is discovery physics (the phone abandoned its only link; no
    /// rule can act before its new address is found and read), and the
    /// assertion pins that the RULE adds nothing on top.
    #[test]
    fn scenario_b_abandoned_rotation_promotes_the_dial_at_once() {
        let mut d = Duel::new(
            PHONE,
            [
                Conn {
                    board_origin: Origin::Incoming,
                    negotiated_att: 517,
                    phone_bookkept_at: Some(0),
                },
                Conn {
                    board_origin: Origin::Outgoing,
                    negotiated_att: 517,
                    phone_bookkept_at: None,
                },
            ],
        );
        d.board_holds[0] = true;
        d.phone_holds[0] = true;
        d.handshaked[0] = true;
        d.board_link_up(0, 0);
        // Keepalives until t=60 s, then the phone rotates and walks
        // away from the old connection.
        d.board.note_heard(0, 60_000);
        d.phone_holds[0] = false;
        d.note(60_000);
        // t=91 s: the board's dial of the new address reads the
        // identity; the old link has been silent 31 s.
        d.board_holds[1] = true;
        d.phone_holds[1] = true;
        let up = d.board_link_up(1, 91_000);
        assert!(
            matches!(
                up,
                LinkUp::Displaced {
                    old_slot: 0,
                    rule: DupRule::Abandoned,
                    old_silence_ms: 31_000,
                    ..
                }
            ),
            "31 s of any-frame silence is an abandoned link: {up:?}"
        );
        assert!(
            !d.board_holds[0],
            "the old link is closed by us at the decision, not at the \
             45 s expiry (t=105 s)"
        );
        // The board handshakes the accepted dial; the phone holds no
        // link of this identity, so there is nothing to dedup and the
        // link is simply up.
        d.handshaked[1] = true;
        d.note(91_100);
        assert_eq!(d.board.slot_for(&PHONE), Some(1));
        assert!(
            d.max_gap(91_000, 150_000) <= MAX_GAP_MS,
            "from the identity read the rule adds no linkless time"
        );
    }

    /// Scenario (c), the mirror: the phone dials the board while the
    /// board holds a live central link to it. A Columba central
    /// negotiates its MTU before it reads our identity, so its dedup
    /// runs on negotiated-vs-negotiated — a tie, broken by identity
    /// order. Sub-case 1: the phone's identity sorts below ours — it
    /// keeps its central (the new connection) and handshakes it; our
    /// incoming judge computes the same tie the same way and displaces
    /// the old link at once.
    #[test]
    fn scenario_c_phone_dial_wins_the_tie_and_both_sides_switch() {
        let mut d = Duel::new(
            PHONE_LO,
            [
                Conn {
                    board_origin: Origin::Outgoing,
                    negotiated_att: 517,
                    phone_bookkept_at: Some(0),
                },
                Conn {
                    board_origin: Origin::Incoming,
                    negotiated_att: 517,
                    // Its own MTU request, completed before its
                    // identity read at t=50.5 s.
                    phone_bookkept_at: Some(50_200),
                },
            ],
        );
        d.board_holds[0] = true;
        d.phone_holds[0] = true;
        d.handshaked[0] = true;
        d.board_link_up(0, 0);
        d.board.note_heard(0, 49_000);
        // t=50 s: the phone dials us.
        d.board_holds[1] = true;
        d.phone_holds[1] = true;
        // t=50.5 s: the phone reads our identity and dedups: tie,
        // PHONE_LO < BOARD keeps central = the new connection.
        assert_eq!(d.phone_dedup(50_500), 1);
        // t=50.6 s: its handshake lands; our judge computes the same
        // arbitration and displaces the old link NOW.
        let up = d.board_link_up(1, 50_600);
        assert!(
            matches!(
                up,
                LinkUp::Displaced {
                    old_slot: 0,
                    rule: DupRule::ColumbaIdentity,
                    ..
                }
            ),
            "same tie, same identity order, same survivor: {up:?}"
        );
        d.handshaked[1] = true;
        d.note(50_600);
        assert_eq!(d.board.slot_for(&PHONE_LO), Some(1));
        assert!(
            d.max_gap(0, 90_000) <= MAX_GAP_MS,
            "the hand-over leaves no linkless interval beyond the bound"
        );
    }

    /// Scenario (c), sub-case 2: the phone's identity sorts above ours
    /// — its dedup keeps its peripheral (the OLD connection) and it
    /// never handshakes the dial, so our judge never runs, the old
    /// link is never touched, and the pair stays linked throughout.
    /// (The phone-side cancel leaves its dial's ACL up — the field
    /// behaviour — which ages out as an unhandshaked connection;
    /// nothing of ours is torn down for it.)
    #[test]
    fn scenario_c_phone_dial_loses_the_tie_and_the_old_link_stands() {
        let mut d = Duel::new(
            PHONE,
            [
                Conn {
                    board_origin: Origin::Outgoing,
                    negotiated_att: 517,
                    phone_bookkept_at: Some(0),
                },
                Conn {
                    board_origin: Origin::Incoming,
                    negotiated_att: 517,
                    phone_bookkept_at: Some(50_200),
                },
            ],
        );
        d.board_holds[0] = true;
        d.phone_holds[0] = true;
        d.handshaked[0] = true;
        d.board_link_up(0, 0);
        d.board.note_heard(0, 49_000);
        d.board_holds[1] = true;
        d.phone_holds[1] = true;
        // Tie, PHONE > BOARD: the phone keeps its peripheral = old,
        // cancels its own dial, never handshakes it.
        assert_eq!(d.phone_dedup(50_500), 0);
        assert_eq!(d.board.slot_for(&PHONE), Some(0), "registry untouched");
        assert_eq!(d.board.peer_count(), 1);
        assert_eq!(d.max_gap(0, 90_000), 0, "the old link never blinked");
    }

    /// Scenario (f): the clamp boundary, the one place where copying
    /// `preferredBleRole` without copying `usableValueLength`'s ceiling
    /// still splits the pair. Our dial exchanged ATT 515, the phone's
    /// dial ATT 517 — one byte apart on the wire, and both 512 to
    /// `coerceIn(_, MAX_ATTRIBUTE_VALUE_LENGTH)`. The phone therefore
    /// sees a TIE and breaks it by identity (PHONE > BOARD: it keeps
    /// its peripheral, our dial); with an unclamped 514 we would see
    /// the phone's dial as the larger MTU and displace our own. Each
    /// side would then hold what the other closed — the 45 s hole of
    /// 2026-09-12, reached from a third direction.
    #[test]
    fn scenario_f_the_512_clamp_boundary_keeps_both_sides_on_one_link() {
        let mut d = Duel::new(
            PHONE,
            [
                Conn {
                    board_origin: Origin::Outgoing,
                    negotiated_att: 515,
                    phone_bookkept_at: Some(0),
                },
                Conn {
                    board_origin: Origin::Incoming,
                    // The phone negotiated this one itself, so its
                    // ledger holds it from the connect.
                    negotiated_att: 517,
                    phone_bookkept_at: Some(50_000),
                },
            ],
        );
        d.board_holds[0] = true;
        d.phone_holds[0] = true;
        d.handshaked[0] = true;
        d.board_link_up(0, 0);
        d.board.note_heard(0, 49_000);
        // t=50 s: the phone dials us from a rotated RPA and writes its
        // identity; our judge runs first, as it does in the field.
        d.board_holds[1] = true;
        d.phone_holds[1] = true;
        let up = d.board_link_up(1, 50_300);
        assert!(
            matches!(
                up,
                LinkUp::Refused {
                    old_slot: 0,
                    rule: DupRule::ColumbaIdentity,
                    ..
                }
            ),
            "clamped, 512 == 512 is a tie the identity order decides: {up:?}"
        );
        // The same two connections with the ceiling left off — ATT 515
        // as 512, ATT 517 as 514, the state before this change —
        // answer the other way, which is what makes the clamp
        // load-bearing rather than cosmetic.
        assert_eq!(
            judge_duplicate(
                1_300,
                Origin::Outgoing,
                Origin::Incoming,
                Some(515 - 3),
                517 - 3,
                &BOARD,
                &PHONE,
            ),
            DupVerdict::KeepNew(DupRule::ColumbaMtu),
            "unclamped, ATT 517 would outrank ATT 515 and split the pair"
        );
        // And the phone, arbitrating on its own numbers, keeps the
        // connection we kept.
        assert_eq!(
            d.phone_dedup(50_400),
            0,
            "phone: tie at 512, PHONE > BOARD, so it keeps its peripheral"
        );
        assert_eq!(d.board.slot_for(&PHONE), Some(0), "board keeps it too");
        assert_eq!(d.max_gap(0, 90_000), 0, "the old link never blinked");
    }

    /// Scenario (e), beyond the batch's four: the SAME-role rotation,
    /// the case Columba's function cannot arbitrate. The pre-dial
    /// exclusion is address-keyed and this rule identity-keyed, so a
    /// rotated RPA puts one identity on two connections of one role;
    /// the phone then holds both in the other single role and its
    /// `preferredBleRole` — a choice between a central and a
    /// peripheral — never fires. The stub is therefore PASSIVE here,
    /// which is the faithful model: it cancels nothing, so the board's
    /// answer alone has to leave the pair linked.
    ///
    /// Sub-case 1, two of OUR dials: the second adds no reachability
    /// the live first has, and is refused pre-handshake — the old link
    /// carries the pair, zero gap.
    #[test]
    fn scenario_e_our_second_dial_of_a_rotated_address_is_refused() {
        let mut d = Duel::new(
            PHONE,
            [
                Conn {
                    board_origin: Origin::Outgoing,
                    negotiated_att: 517,
                    phone_bookkept_at: Some(0),
                },
                Conn {
                    board_origin: Origin::Outgoing,
                    negotiated_att: 517,
                    phone_bookkept_at: None,
                },
            ],
        );
        d.board_holds[0] = true;
        d.phone_holds[0] = true;
        d.handshaked[0] = true;
        d.board_link_up(0, 0);
        d.board.note_heard(0, 60_000);
        // t=61 s: the phone's rotated address is dialled — a second
        // OUTGOING connection to an identity we already hold.
        d.board_holds[1] = true;
        d.phone_holds[1] = true;
        let up = d.board_link_up(1, 61_000);
        assert!(
            matches!(
                up,
                LinkUp::Refused {
                    old_slot: 0,
                    rule: DupRule::SameRole,
                    old_silence_ms: 1_000,
                    ..
                }
            ),
            "no arbitration to copy, and nothing to gain: {up:?}"
        );
        assert_eq!(d.board.slot_for(&PHONE), Some(0));
        assert_eq!(d.max_gap(0, 120_000), 0, "the old link never blinked");
    }

    /// Scenario (e), sub-case 2: the PHONE dials us twice from two
    /// RPAs. It dialled again, which is what a node does with a
    /// connection it has stopped using, so the newcomer wins and the
    /// old link is torn down by us at the decision. The hand-over is
    /// instant — the new connection is already handshaked when the
    /// verdict lands, because the handshake IS the decision point on
    /// this path — so the pair is never linkless.
    #[test]
    fn scenario_e_the_peers_second_dial_of_a_rotated_address_wins() {
        let mut d = Duel::new(
            PHONE,
            [
                Conn {
                    board_origin: Origin::Incoming,
                    negotiated_att: 517,
                    phone_bookkept_at: Some(0),
                },
                Conn {
                    board_origin: Origin::Incoming,
                    negotiated_att: 517,
                    phone_bookkept_at: Some(60_500),
                },
            ],
        );
        d.board_holds[0] = true;
        d.phone_holds[0] = true;
        d.handshaked[0] = true;
        d.board_link_up(0, 0);
        d.board.note_heard(0, 60_000);
        // t=61 s: the phone's second dial handshakes.
        d.board_holds[1] = true;
        d.phone_holds[1] = true;
        let up = d.board_link_up(1, 61_000);
        assert!(
            matches!(
                up,
                LinkUp::Displaced {
                    old_slot: 0,
                    rule: DupRule::SameRole,
                    old_silence_ms: 1_000,
                    ..
                }
            ),
            "the peer dialled again; the old connection is history: {up:?}"
        );
        assert!(!d.board_holds[0], "and we close it ourselves, at once");
        d.handshaked[1] = true;
        d.note(61_000);
        assert_eq!(d.board.slot_for(&PHONE), Some(1));
        assert_eq!(
            d.max_gap(0, 120_000),
            0,
            "the new link was usable the instant the old one closed"
        );
    }

    /// Scenario (d): two boards, both running the shipped rule, no
    /// Columba — the cross-dial race. A (lower address AND lower
    /// identity — the initiator convention and the tie-break point the
    /// same way for boards, whose addresses are static) and B dial
    /// each other simultaneously; each accepts its own dial (no
    /// duplicate visible yet), then receives the other's handshake.
    /// Both incoming judges compute the same tie from opposite ends
    /// and converge on the SAME connection — the lower identity's dial
    /// — leaving exactly one link and no linkless interval.
    #[test]
    fn scenario_d_two_boards_cross_dial_converges_on_one_link() {
        const A_ID: [u8; 16] = [0x11; 16];
        const B_ID: [u8; 16] = [0x99; 16];
        let mut rega = PeerRegistry::<4>::new();
        rega.set_local_identity(A_ID);
        let mut regb = PeerRegistry::<4>::new();
        regb.set_local_identity(B_ID);

        // Conn 0 = A dials B (slot 0 on both); conn 1 = B dials A
        // (slot 1 on both). Both ATT exchanges settle at connect.
        // t=1.00 s: A reads B's identity on its dial — no duplicate
        // visible (B has not handshaked conn 1 to A yet) — accepted,
        // A handshakes.
        assert!(matches!(
            rega.link_up(0, B_ID, Origin::Outgoing, 517, 1_000),
            LinkUp::Accepted { first: true }
        ));
        // t=1.05 s: B reads A's identity on ITS dial — same picture,
        // accepted, B handshakes.
        assert!(matches!(
            regb.link_up(1, A_ID, Origin::Outgoing, 517, 1_050),
            LinkUp::Accepted { first: true }
        ));
        // t=1.10 s: A's handshake lands at B on conn 0: a duplicate
        // against B's own live dial. The tie says A (lower identity)
        // keeps central — A is central on conn 0 — so conn 0 wins and
        // B tears down its own dial now.
        assert!(matches!(
            regb.link_up(0, A_ID, Origin::Incoming, 517, 1_100),
            LinkUp::Displaced {
                old_slot: 1,
                rule: DupRule::ColumbaIdentity,
                ..
            }
        ));
        regb.link_down(1);
        // t=1.15 s: B's handshake (sent before its teardown) lands at
        // A on conn 1: A's judge computes the same tie — B keeps
        // peripheral = conn 0 — and refuses conn 1.
        assert!(matches!(
            rega.link_up(1, B_ID, Origin::Incoming, 517, 1_150),
            LinkUp::Refused {
                old_slot: 0,
                rule: DupRule::ColumbaIdentity,
                ..
            }
        ));
        // Exactly one link survives, the same one on both ends, and
        // the pair was never linkless: conn 0 was usable from t=1.10 s
        // and never went down.
        assert_eq!(rega.slot_for(&B_ID), Some(0));
        assert_eq!(regb.slot_for(&A_ID), Some(0));
        assert_eq!(rega.peer_count(), 1);
        assert_eq!(regb.peer_count(), 1);
    }
}
