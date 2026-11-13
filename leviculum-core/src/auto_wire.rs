//! AutoInterface's wire facts, `no_std`, for every stack that speaks it
//! (1479).
//!
//! Python-RNS's `AutoInterface` finds its neighbours on a LAN by IPv6
//! link-local multicast: every node sends a 32-byte token to a group
//! address derived from the group id, each receiver checks the token
//! against the sender's address, and a peer it accepts becomes a unicast
//! UDP neighbour on the data port. The daemon (`leviculum-std`'s
//! `interfaces::auto_interface`) runs that over tokio sockets; the
//! ThinkNode M9 runs it over its WiFi without std. The group address, the
//! token, the discovery packet, the ports and the cadences live here once,
//! so the two cannot drift apart, and each is pinned against Python's
//! output by the tests below
//! (`reference/Reticulum/RNS/Interfaces/AutoInterface.py`).
//!
//! [`Peers`] is the part of the orchestrator a board needs without a
//! socket layer: which neighbours are alive, when to send the next
//! discovery token, and which datagrams are its own echo. The daemon keeps
//! its own per-NIC orchestrator, which carries spawned sub-interfaces and
//! carrier-flap handling a single-NIC board has no use for.

use core::fmt::Write as _;
use core::net::Ipv6Addr;

use crate::crypto::full_hash_parts;

/// The group id every node uses unless configured otherwise
/// (`AutoInterface.py`, `DEFAULT_GROUP_ID`'s default `"reticulum"`).
pub const DEFAULT_GROUP_ID: &[u8] = b"reticulum";
/// The multicast discovery port (`AutoInterface.DEFAULT_DISCOVERY_PORT`).
pub const DEFAULT_DISCOVERY_PORT: u16 = 29716;
/// The unicast data port (`AutoInterface.DEFAULT_DATA_PORT`).
pub const DEFAULT_DATA_PORT: u16 = 42671;
/// A peer not heard for this long is dropped (`PEERING_TIMEOUT = 22.0`).
pub const PEERING_TIMEOUT_MS: u64 = 22_000;
/// How often a node multicasts its token (`ANNOUNCE_INTERVAL = 1.6`).
pub const ANNOUNCE_INTERVAL_MS: u64 = 1_600;
/// How often the peer table is swept (Python's `peer_jobs`, 4 s).
pub const PEER_JOB_INTERVAL_MS: u64 = 4_000;
/// A node that has not heard its own multicast echo for this long treats
/// the NIC's multicast as broken (`MULTICAST_ECHO_TIMEOUT = 6.5`).
pub const MCAST_ECHO_TIMEOUT_MS: u64 = 6_500;
/// The interface's MTU (`AutoInterface.HW_MTU = 1196`).
pub const HW_MTU: u32 = 1196;
/// Entries in the receive dedup ring (Python's `deque(maxlen=48)`).
pub const DEDUP_CACHE_SIZE: usize = 48;
/// How long a datagram's hash suppresses its copies (`MULTI_IF_DEQUE_TTL`).
pub const DEDUP_TTL_MS: u64 = 750;
/// The size of the per-instance nonce Rust nodes append to their token.
pub const NONCE_SIZE: usize = 8;
/// A discovery datagram as Rust nodes send it: token, nonce, data port.
/// Python reads `data[:32]` and ignores the rest.
pub const DISCOVERY_PACKET_SIZE: usize = 32 + NONCE_SIZE + 2;

/// The unicast discovery port is the multicast one plus one (Python's
/// `self.unicast_discovery_port = self.discovery_port + 1`).
pub const fn unicast_discovery_port(discovery_port: u16) -> u16 {
    discovery_port.wrapping_add(1)
}

/// Python's reverse peering interval: `announce_interval * 3.25`.
pub const REVERSE_PEERING_INTERVAL_MS: u64 = ANNOUNCE_INTERVAL_MS * 13 / 4;

/// Which multicast address type the discovery group address carries, the
/// second nibble of `ff<type><scope>:...`.
///
/// Python-RNS selects it per interface with the `multicast_address_type`
/// config key (`MULTICAST_PERMANENT_ADDRESS_TYPE`
/// (`reference/Reticulum/RNS/Interfaces/AutoInterface.py:58-59`), resolved at
/// `AutoInterface.py:175-182`). The type is part of the group address, so two
/// nodes that disagree on it are not in the same multicast group and never
/// see each other, with no error on either side - discovery failure looks
/// exactly like an empty network (Codeberg #282).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MulticastAddressType {
    /// `"1"` - a transient IPv6 group. The default, as in Python.
    #[default]
    Temporary,
    /// `"0"` - a well-known (permanent) IPv6 group.
    Permanent,
}

impl MulticastAddressType {
    /// The nibble this type contributes to the group address.
    pub const fn nibble(self) -> u16 {
        match self {
            Self::Temporary => 1,
            Self::Permanent => 0,
        }
    }

    /// Parse a `multicast_address_type` config value.
    ///
    /// Accepts exactly the two spellings Python honours, case-insensitively
    /// (`AutoInterface.py:175-182`), so a config file that selects a group
    /// under `rnsd` selects the same group here.
    ///
    /// Returns `None` for anything else instead of falling back to
    /// `Temporary` as Python does. That fallback is the reason this knob is
    /// worth having at all: a node that silently lands in a group the
    /// operator did not ask for discovers nobody, and no log line anywhere
    /// points at the address type. Rejecting names the typo at startup
    /// instead. The deviation is config-side only - every value Python acts
    /// on resolves identically here, so the derived address and the wire
    /// traffic are unchanged.
    pub fn from_config_str(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("temporary") {
            Some(Self::Temporary)
        } else if value.eq_ignore_ascii_case("permanent") {
            Some(Self::Permanent)
        } else {
            None
        }
    }
}

/// The IPv6 multicast scope nibble Python maps a `discovery_scope` name
/// to; anything it does not know is link-local, as in Python.
pub fn scope_nibble(scope: &str) -> u16 {
    const SCOPES: [(&str, u16); 5] = [
        ("link", 0x2),
        ("admin", 0x4),
        ("site", 0x5),
        ("organisation", 0x8),
        ("global", 0xe),
    ];
    SCOPES
        .iter()
        .find(|(name, _)| scope.eq_ignore_ascii_case(name))
        .map_or(0x2, |&(_, nibble)| nibble)
}

/// The discovery group address for `group_id`.
///
/// Python: `g = full_hash(group_id)`, then
/// `"ff" + type + scope + ":0" + ":{:02x}" % (g[2k+1] + (g[2k] << 8))` for
/// the six words from byte 2 to byte 13.
pub fn multicast_address(
    group_id: &[u8],
    scope: u16,
    address_type: MulticastAddressType,
) -> Ipv6Addr {
    let g = full_hash_parts(&[group_id]);
    let word = |i: usize| (u16::from(g[i]) << 8) | u16::from(g[i + 1]);
    Ipv6Addr::new(
        0xff00 | (address_type.nibble() << 4) | (scope & 0xf),
        0,
        word(2),
        word(4),
        word(6),
        word(8),
        word(10),
        word(12),
    )
}

/// The discovery token for a node whose link-local address prints as
/// `link_local_text`: `full_hash(group_id + text)`, the text being what
/// Python's `str()` of the descoped address gives, without `%ifname`.
pub fn discovery_token(group_id: &[u8], link_local_text: &str) -> [u8; 32] {
    full_hash_parts(&[group_id, link_local_text.as_bytes()])
}

/// [`discovery_token`] for an address the caller holds as bytes: the text
/// is the RFC 5952 form `core::net::Ipv6Addr` prints, the same form the
/// C library's `inet_ntop` gives Python.
pub fn discovery_token_for(group_id: &[u8], link_local: &Ipv6Addr) -> [u8; 32] {
    let text = AddrText::of(link_local);
    discovery_token(group_id, text.as_str())
}

/// Whether `token` is the one `src` must have sent for `group_id`, in
/// constant time.
pub fn verify_discovery_token(token: &[u8], group_id: &[u8], src_text: &str) -> bool {
    if token.len() != 32 {
        return false;
    }
    let expected = discovery_token(group_id, src_text);
    let mut diff = 0u8;
    for (x, y) in token.iter().zip(expected.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// A discovery datagram as Rust nodes send it: `[token(32)][nonce(8)]
/// [data_port(2, big-endian)]`. The token first, because Python reads
/// `data[:32]`; the nonce lets a Rust receiver tell its own echo from a
/// peer that shares its address, the port lets it reach a peer on a
/// non-default data port.
pub fn discovery_packet(
    token: &[u8; 32],
    nonce: &[u8; NONCE_SIZE],
    data_port: u16,
) -> [u8; DISCOVERY_PACKET_SIZE] {
    let mut pkt = [0u8; DISCOVERY_PACKET_SIZE];
    pkt[..32].copy_from_slice(token);
    pkt[32..32 + NONCE_SIZE].copy_from_slice(nonce);
    pkt[32 + NONCE_SIZE..].copy_from_slice(&data_port.to_be_bytes());
    pkt
}

/// A received discovery datagram's fields. A Python sender's datagram is
/// the token alone, so the nonce and the port are absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Discovery {
    pub token: [u8; 32],
    pub nonce: Option<[u8; NONCE_SIZE]>,
    pub data_port: Option<u16>,
}

/// Parse a discovery datagram; `None` when it is shorter than a token.
pub fn parse_discovery(data: &[u8]) -> Option<Discovery> {
    let token: [u8; 32] = data.get(..32)?.try_into().ok()?;
    let nonce = data
        .get(32..32 + NONCE_SIZE)
        .and_then(|n| <[u8; NONCE_SIZE]>::try_from(n).ok());
    let data_port = data
        .get(32 + NONCE_SIZE..32 + NONCE_SIZE + 2)
        .map(|p| u16::from_be_bytes([p[0], p[1]]));
    Some(Discovery {
        token,
        nonce,
        data_port,
    })
}

/// An IPv6 address's text on the stack: at most 39 characters
/// (`ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff`), or 45 for an IPv4-mapped
/// form, which a link-local address never is.
pub struct AddrText {
    buf: [u8; 46],
    len: usize,
}

impl AddrText {
    /// The RFC 5952 text of `addr`.
    pub fn of(addr: &Ipv6Addr) -> Self {
        let mut text = Self {
            buf: [0; 46],
            len: 0,
        };
        // Cannot fail: the buffer holds the longest form.
        let _ = write!(text, "{addr}");
        text
    }

    /// The text.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl core::fmt::Write for AddrText {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let end = self.len + s.len();
        let dst = self.buf.get_mut(self.len..end).ok_or(core::fmt::Error)?;
        dst.copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// The receive dedup ring: a datagram whose hash was seen within
/// [`DEDUP_TTL_MS`] is a copy (the same packet arriving over two NICs, or
/// twice over one). Python keeps the last 48 hashes; so does this.
pub struct Dedup {
    entries: [(u64, [u8; 32]); DEDUP_CACHE_SIZE],
    next: usize,
    filled: usize,
}

impl Default for Dedup {
    fn default() -> Self {
        Self::new()
    }
}

impl Dedup {
    /// An empty ring.
    pub const fn new() -> Self {
        Self {
            entries: [(0, [0; 32]); DEDUP_CACHE_SIZE],
            next: 0,
            filled: 0,
        }
    }

    /// Whether `data` is a copy of a datagram seen at most
    /// [`DEDUP_TTL_MS`] before `now_ms`; a datagram that is not is
    /// remembered.
    pub fn is_duplicate(&mut self, data: &[u8], now_ms: u64) -> bool {
        let hash = full_hash_parts(&[data]);
        let seen = self.entries[..self.filled]
            .iter()
            .any(|(at, h)| *h == hash && now_ms.saturating_sub(*at) < DEDUP_TTL_MS);
        if seen {
            return true;
        }
        self.entries[self.next] = (now_ms, hash);
        self.next = (self.next + 1) % DEDUP_CACHE_SIZE;
        self.filled = (self.filled + 1).min(DEDUP_CACHE_SIZE);
        false
    }
}

/// A neighbour [`Peers`] accepted: its link-local address, the port it
/// takes data on, when it was last heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peer {
    pub addr: Ipv6Addr,
    pub data_port: u16,
    pub last_heard_ms: u64,
}

/// What a received discovery datagram did to the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    /// The token does not match the sender's address or the group.
    Refused,
    /// Our own multicast came back; the NIC's multicast works.
    OwnEcho,
    /// A new neighbour.
    Added,
    /// A known neighbour, its timer refreshed.
    Refreshed,
    /// A valid neighbour the full table had no room for.
    Full,
}

/// One NIC's AutoInterface state without sockets: the board's half of the
/// daemon's orchestrator. `N` bounds the neighbours, the board's table
/// being fixed-size.
pub struct Peers<const N: usize> {
    group_id: &'static [u8],
    own: Ipv6Addr,
    nonce: [u8; NONCE_SIZE],
    data_port: u16,
    peers: [Option<Peer>; N],
    next_discovery_ms: u64,
    last_echo_ms: Option<u64>,
}

impl<const N: usize> Peers<N> {
    /// The table for a node at link-local `own`, with the per-boot
    /// `nonce`, listening for data on `data_port`.
    pub fn new(
        group_id: &'static [u8],
        own: Ipv6Addr,
        nonce: [u8; NONCE_SIZE],
        data_port: u16,
    ) -> Self {
        Self {
            group_id,
            own,
            nonce,
            data_port,
            peers: [None; N],
            next_discovery_ms: 0,
            last_echo_ms: None,
        }
    }

    /// The datagram to multicast at `now_ms`, when one is due; then the
    /// next one is due [`ANNOUNCE_INTERVAL_MS`] later.
    pub fn discovery_due(&mut self, now_ms: u64) -> Option<[u8; DISCOVERY_PACKET_SIZE]> {
        if now_ms < self.next_discovery_ms {
            return None;
        }
        self.next_discovery_ms = now_ms + ANNOUNCE_INTERVAL_MS;
        let token = discovery_token_for(self.group_id, &self.own);
        Some(discovery_packet(&token, &self.nonce, self.data_port))
    }

    /// A discovery datagram `data` from link-local `src`, at `now_ms`.
    pub fn heard(&mut self, src: Ipv6Addr, data: &[u8], now_ms: u64) -> Heard {
        let Some(found) = parse_discovery(data) else {
            return Heard::Refused;
        };
        let text = AddrText::of(&src);
        if !verify_discovery_token(&found.token, self.group_id, text.as_str()) {
            return Heard::Refused;
        }
        if src == self.own && found.nonce.is_none_or(|n| n == self.nonce) {
            self.last_echo_ms = Some(now_ms);
            return Heard::OwnEcho;
        }
        let data_port = found.data_port.unwrap_or(DEFAULT_DATA_PORT);
        if let Some(peer) = self.peers.iter_mut().flatten().find(|p| p.addr == src) {
            peer.last_heard_ms = now_ms;
            peer.data_port = data_port;
            return Heard::Refreshed;
        }
        match self.peers.iter_mut().find(|p| p.is_none()) {
            Some(slot) => {
                *slot = Some(Peer {
                    addr: src,
                    data_port,
                    last_heard_ms: now_ms,
                });
                Heard::Added
            }
            None => Heard::Full,
        }
    }

    /// Drop every neighbour not heard for [`PEERING_TIMEOUT_MS`]; returns
    /// how many went.
    pub fn expire(&mut self, now_ms: u64) -> usize {
        let mut gone = 0;
        for slot in &mut self.peers {
            if slot.is_some_and(|p| now_ms.saturating_sub(p.last_heard_ms) > PEERING_TIMEOUT_MS) {
                *slot = None;
                gone += 1;
            }
        }
        gone
    }

    /// The live neighbours, where a data packet goes.
    pub fn peers(&self) -> impl Iterator<Item = &Peer> {
        self.peers.iter().flatten()
    }

    /// Whether `src` is a live neighbour: a data datagram from anyone else
    /// is not ours to take (Python only takes data from spawned peers).
    pub fn is_peer(&self, src: &Ipv6Addr) -> bool {
        self.peers().any(|p| p.addr == *src)
    }

    /// Whether our own multicast came back within
    /// [`MCAST_ECHO_TIMEOUT_MS`] before `now_ms`.
    pub fn multicast_echoes(&self, now_ms: u64) -> bool {
        self.last_echo_ms
            .is_some_and(|at| now_ms.saturating_sub(at) <= MCAST_ECHO_TIMEOUT_MS)
    }

    /// The address the table was made for.
    pub fn own(&self) -> Ipv6Addr {
        self.own
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LL_A: Ipv6Addr = Ipv6Addr::new(0xfe80, 0, 0, 0, 0x1, 0, 0, 0xa);
    const LL_B: Ipv6Addr = Ipv6Addr::new(0xfe80, 0, 0, 0, 0x1, 0, 0, 0xb);

    #[test]
    fn the_group_address_is_pythons() {
        // Python: group_id=b"reticulum", scope "link", temporary.
        let want: Ipv6Addr = "ff12:0:d70b:fb1c:16e4:5e39:485e:31e1".parse().unwrap();
        assert_eq!(
            multicast_address(DEFAULT_GROUP_ID, scope_nibble("link"), Default::default()),
            want
        );
        let site = multicast_address(
            DEFAULT_GROUP_ID,
            scope_nibble("Site"),
            MulticastAddressType::Permanent,
        );
        assert_eq!(site.segments()[0], 0xff05);
        assert_eq!(site.segments()[2..], want.segments()[2..]);
    }

    #[test]
    fn the_token_is_pythons() {
        // Python: full_hash(b"reticulum" + b"fe80::1").
        let want = "97b25576749ea936b0d8a8536ffaf442d157cf47d460dcf13c48b7bd18b6c163";
        let token = discovery_token(DEFAULT_GROUP_ID, "fe80::1");
        assert_eq!(hex::encode(token), want);
        let one: Ipv6Addr = "fe80::1".parse().unwrap();
        assert_eq!(discovery_token_for(DEFAULT_GROUP_ID, &one), token);
        assert!(verify_discovery_token(&token, DEFAULT_GROUP_ID, "fe80::1"));
        assert!(!verify_discovery_token(&token, DEFAULT_GROUP_ID, "fe80::2"));
        assert!(!verify_discovery_token(
            &token[..16],
            DEFAULT_GROUP_ID,
            "fe80::1"
        ));
    }

    #[test]
    fn the_address_text_is_rfc5952() {
        // The single zero word is not compressed, the longest run is.
        let a: Ipv6Addr = "fe80:0:0:0:1:0:0:a".parse().unwrap();
        assert_eq!(AddrText::of(&a).as_str(), "fe80::1:0:0:a");
        let max = Ipv6Addr::new(
            0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff,
        );
        assert_eq!(AddrText::of(&max).as_str().len(), 39);
    }

    #[test]
    fn a_python_token_alone_parses_and_a_short_one_does_not() {
        let token = discovery_token(DEFAULT_GROUP_ID, "fe80::1");
        let python = parse_discovery(&token).unwrap();
        assert_eq!((python.nonce, python.data_port), (None, None));
        let ours = parse_discovery(&discovery_packet(&token, &[7; 8], 4242)).unwrap();
        assert_eq!((ours.nonce, ours.data_port), (Some([7; 8]), Some(4242)));
        assert!(parse_discovery(&token[..31]).is_none());
    }

    #[test]
    fn ports_and_cadences_are_pythons() {
        assert_eq!(unicast_discovery_port(DEFAULT_DISCOVERY_PORT), 29717);
        assert_eq!(REVERSE_PEERING_INTERVAL_MS, 5_200);
        assert_eq!((ANNOUNCE_INTERVAL_MS, PEERING_TIMEOUT_MS), (1_600, 22_000));
    }

    #[test]
    fn a_neighbour_is_added_refreshed_and_expires() {
        let mut a: Peers<2> = Peers::new(DEFAULT_GROUP_ID, LL_A, [1; 8], DEFAULT_DATA_PORT);
        let mut b: Peers<2> = Peers::new(DEFAULT_GROUP_ID, LL_B, [2; 8], 4000);
        let from_b = b.discovery_due(0).unwrap();
        assert_eq!(a.heard(LL_B, &from_b, 10), Heard::Added);
        assert_eq!(a.peers().next().unwrap().data_port, 4000);
        assert_eq!(a.heard(LL_B, &from_b, 20_000), Heard::Refreshed);
        assert_eq!(a.expire(42_000), 0);
        assert_eq!(a.expire(42_001), 1);
        assert!(!a.is_peer(&LL_B));
    }

    #[test]
    fn a_python_neighbour_gets_the_default_data_port() {
        let mut a: Peers<2> = Peers::new(DEFAULT_GROUP_ID, LL_A, [1; 8], DEFAULT_DATA_PORT);
        let text = AddrText::of(&LL_B);
        let python = discovery_token(DEFAULT_GROUP_ID, text.as_str());
        assert_eq!(a.heard(LL_B, &python, 0), Heard::Added);
        assert_eq!(a.peers().next().unwrap().data_port, DEFAULT_DATA_PORT);
    }

    #[test]
    fn a_forged_token_and_the_own_echo_add_nobody() {
        let mut a: Peers<2> = Peers::new(DEFAULT_GROUP_ID, LL_A, [1; 8], DEFAULT_DATA_PORT);
        let own = a.discovery_due(0).unwrap();
        assert_eq!(a.heard(LL_A, &own, 5), Heard::OwnEcho);
        assert!(a.multicast_echoes(5 + MCAST_ECHO_TIMEOUT_MS));
        assert!(!a.multicast_echoes(6 + MCAST_ECHO_TIMEOUT_MS));
        // B's token replayed from C's address.
        let from_b = Peers::<1>::new(DEFAULT_GROUP_ID, LL_B, [2; 8], 1).discovery_due(0);
        let ll_c = Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0xc);
        assert_eq!(a.heard(ll_c, &from_b.unwrap(), 9), Heard::Refused);
        // Another group.
        let other = discovery_token(b"other", AddrText::of(&LL_B).as_str());
        assert_eq!(a.heard(LL_B, &other, 9), Heard::Refused);
        assert_eq!(a.peers().count(), 0);
    }

    #[test]
    fn discovery_goes_out_every_interval_and_a_full_table_says_so() {
        let mut a: Peers<1> = Peers::new(DEFAULT_GROUP_ID, LL_A, [1; 8], DEFAULT_DATA_PORT);
        assert!(a.discovery_due(0).is_some());
        assert!(a.discovery_due(ANNOUNCE_INTERVAL_MS - 1).is_none());
        assert!(a.discovery_due(ANNOUNCE_INTERVAL_MS).is_some());
        let b = discovery_token(DEFAULT_GROUP_ID, AddrText::of(&LL_B).as_str());
        let ll_c = Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0xc);
        let c = discovery_token(DEFAULT_GROUP_ID, AddrText::of(&ll_c).as_str());
        assert_eq!(a.heard(LL_B, &b, 0), Heard::Added);
        assert_eq!(a.heard(ll_c, &c, 0), Heard::Full);
    }

    #[test]
    fn a_copy_within_the_ttl_is_a_duplicate_and_after_it_is_not() {
        let mut d = Dedup::new();
        assert!(!d.is_duplicate(b"x", 0));
        assert!(d.is_duplicate(b"x", DEDUP_TTL_MS - 1));
        assert!(!d.is_duplicate(b"x", DEDUP_TTL_MS + 10));
        for i in 0..DEDUP_CACHE_SIZE as u8 {
            assert!(!d.is_duplicate(&[i, 0xee], 2_000));
        }
        // The oldest entry left the ring.
        assert!(!d.is_duplicate(b"x", DEDUP_TTL_MS + 11));
    }
}
