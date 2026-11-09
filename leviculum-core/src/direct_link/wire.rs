//! Byte formats of the direct-link upgrade (leviculum#70).
//!
//! Three families, all fixed by the rns-rs DirectLink protocol so a
//! leviculum node and an rns-rs node can upgrade a link between them and
//! share a facilitator:
//!
//! - **Signals**: channel messages on the existing link, MSGTYPE
//!   `0xFE00..=0xFE04`, each body a msgpack map keyed by one-letter strings.
//! - **Probes**: raw UDP to the facilitator, outside Reticulum framing. Either
//!   the 21-byte `RNSP` request, or a plain RFC 5389 STUN Binding Request so
//!   any public STUN server can stand in for the facilitator.
//! - **Punch frames**: 56-byte `RNSH` / `RNSA` datagrams the two peers trade
//!   to open the NAT pinholes, and that later keep them open.
//!
//! Everything here is pure: encoders return bytes, decoders return `None`
//! (or [`WireError`]) on anything malformed, and nothing touches a socket.

use alloc::vec::Vec;
use core::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::crypto::derive_key;
use crate::msgpack;

/// Initiator to responder: "upgrade this link", with the facilitator to probe
/// and the initiator's reflexive address.
pub const MSG_UPGRADE_REQUEST: u16 = 0xFE00;
/// Responder to initiator: the request is taken, the responder is probing.
pub const MSG_UPGRADE_ACCEPT: u16 = 0xFE01;
/// Responder to initiator: no upgrade, with a reason byte.
pub const MSG_UPGRADE_REJECT: u16 = 0xFE02;
/// Responder to initiator: the responder's reflexive address; both punch now.
pub const MSG_UPGRADE_READY: u16 = 0xFE03;
/// Defined by the protocol and accepted, never sent: neither implementation
/// needs a confirmation once both sides have seen the other's punch ack.
pub const MSG_UPGRADE_COMPLETE: u16 = 0xFE04;

/// Whether a channel MSGTYPE belongs to the direct-link upgrade.
pub fn is_signal_msgtype(msgtype: u16) -> bool {
    (MSG_UPGRADE_REQUEST..=MSG_UPGRADE_COMPLETE).contains(&msgtype)
}

/// The responder's policy refused the upgrade.
pub const REJECT_POLICY: u8 = 0x01;
/// The responder already has an upgrade running on this link.
pub const REJECT_BUSY: u8 = 0x02;
/// The responder cannot take part (here: its probe of the facilitator failed).
pub const REJECT_UNSUPPORTED: u8 = 0x03;

/// +ciris: the most host candidates a signal carries or a decoder keeps.
pub const MAX_LOCAL_CANDIDATES: usize = 4;

/// +ciris (leviculum#88): the largest host payload a REQUEST carries. Big
/// enough for a hybrid-signed CIRIS contact grant (ML-DSA-65, about 6-8 KiB);
/// the REQUEST must still fit the link's channel MDU.
pub const MAX_HOST_PAYLOAD: usize = 16 * 1024;

/// +ciris (leviculum#89): REJECT reason after a punch, meaning "my side of
/// the punch failed, do not move the link". Sent only to a peer that offered
/// the two-sided commit.
pub const REJECT_PUNCH_FAILED: u8 = 0x10;

/// +ciris (leviculum#96): REJECT reason from a link's initiator to a REQUEST
/// that crossed its own proposal: "mine stands, take the one I sent". A peer
/// that knows it gives way and waits for that REQUEST; one that does not
/// reads it as any other refusal.
pub const REJECT_GLARE: u8 = 0x11;

/// +ciris (leviculum#98): REJECT reason sent after a COMPLETE, by a side that
/// is holding its move for a transfer: "not a refusal, but until I move, my
/// traffic comes over the relay and does not mean I fell back". Sent only to
/// a peer on the two-sided commit, which by then holds no session, so one
/// that does not know the reason ignores it.
pub const REJECT_HOLDING: u8 = 0x12;

/// HKDF `info` for the punch token. Shared with rns-rs; a different label
/// would make every punch frame fail the other side's check.
const PUNCH_TOKEN_INFO: &[u8] = b"rns-holepunch-v1";

/// A 16-byte upgrade session id, drawn by the initiator.
pub type SessionId = [u8; 16];

/// How a node learns its reflexive address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProbeProtocol {
    /// The `RNSP` request, answered by a facilitator node.
    #[default]
    Rnsp,
    /// An RFC 5389 Binding Request, answered by any STUN server.
    Stun,
}

impl ProbeProtocol {
    fn wire_value(self) -> u64 {
        match self {
            ProbeProtocol::Rnsp => 0,
            ProbeProtocol::Stun => 1,
        }
    }

    fn from_wire(value: u64) -> Self {
        // An unknown value falls back to RNSP, the protocol's original probe.
        if value == 1 {
            ProbeProtocol::Stun
        } else {
            ProbeProtocol::Rnsp
        }
    }
}

/// Why a signal body did not decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// The MSGTYPE is not one of the five upgrade signals.
    UnknownMsgtype,
    /// The body is not a map carrying the fields this signal requires.
    Malformed,
}

/// One upgrade signal, as carried on the link's channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    Request {
        session: SessionId,
        facilitator: SocketAddr,
        initiator_public: SocketAddr,
        protocol: ProbeProtocol,
        /// +ciris: the initiator's host candidates ([`MAX_LOCAL_CANDIDATES`]
        /// at most), punched when both ends turn out to share a NAT.
        initiator_local: Vec<SocketAddr>,
        /// +ciris (leviculum#89): the initiator offers the two-sided commit.
        commit: bool,
        /// +ciris (leviculum#88): opaque bytes for the responder's host to
        /// decide on (CIRIS: the requester's signed contact grant).
        host_payload: Option<Vec<u8>>,
    },
    Accept {
        session: SessionId,
    },
    Reject {
        session: SessionId,
        reason: u8,
    },
    Ready {
        session: SessionId,
        responder_public: SocketAddr,
        /// +ciris: the responder's host candidates.
        responder_local: Vec<SocketAddr>,
        /// +ciris (leviculum#89): the responder agrees to the two-sided
        /// commit (it offers it only when the REQUEST did).
        commit: bool,
    },
    Complete {
        session: SessionId,
    },
}

impl Signal {
    /// The channel MSGTYPE this signal travels under.
    pub fn msgtype(&self) -> u16 {
        match self {
            Signal::Request { .. } => MSG_UPGRADE_REQUEST,
            Signal::Accept { .. } => MSG_UPGRADE_ACCEPT,
            Signal::Reject { .. } => MSG_UPGRADE_REJECT,
            Signal::Ready { .. } => MSG_UPGRADE_READY,
            Signal::Complete { .. } => MSG_UPGRADE_COMPLETE,
        }
    }

    /// The session this signal belongs to.
    pub fn session(&self) -> &SessionId {
        match self {
            Signal::Request { session, .. }
            | Signal::Accept { session }
            | Signal::Reject { session, .. }
            | Signal::Ready { session, .. }
            | Signal::Complete { session } => session,
        }
    }

    /// Encode the body.
    ///
    /// Field order and integer widths follow rns-rs, so the bytes match
    /// theirs exactly, not just their decoder's tolerance (pinned in tests).
    /// `"p"` is written only for STUN: RNSP is the value an absent key means.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        match self {
            Signal::Request {
                session,
                facilitator,
                initiator_public,
                protocol,
                initiator_local,
                commit,
                host_payload,
            } => {
                let stun = *protocol != ProbeProtocol::Rnsp;
                let local = !initiator_local.is_empty();
                msgpack::write_fixmap_header(
                    &mut buf,
                    3 + u8::from(stun)
                        + u8::from(local)
                        + u8::from(*commit)
                        + u8::from(host_payload.is_some()),
                );
                write_session(&mut buf, session);
                msgpack::write_fixstr(&mut buf, "f");
                write_endpoint(&mut buf, facilitator);
                msgpack::write_fixstr(&mut buf, "a");
                write_endpoint(&mut buf, initiator_public);
                if stun {
                    msgpack::write_fixstr(&mut buf, "p");
                    msgpack::write_uint(&mut buf, protocol.wire_value());
                }
                if local {
                    write_local(&mut buf, initiator_local);
                }
                write_extras(&mut buf, *commit, host_payload.as_deref());
            }
            Signal::Accept { session } | Signal::Complete { session } => {
                msgpack::write_fixmap_header(&mut buf, 1);
                write_session(&mut buf, session);
            }
            Signal::Reject { session, reason } => {
                msgpack::write_fixmap_header(&mut buf, 2);
                write_session(&mut buf, session);
                msgpack::write_fixstr(&mut buf, "r");
                msgpack::write_uint(&mut buf, u64::from(*reason));
            }
            Signal::Ready {
                session,
                responder_public,
                responder_local,
                commit,
            } => {
                let local = !responder_local.is_empty();
                msgpack::write_fixmap_header(&mut buf, 2 + u8::from(local) + u8::from(*commit));
                write_session(&mut buf, session);
                msgpack::write_fixstr(&mut buf, "a");
                write_endpoint(&mut buf, responder_public);
                if local {
                    write_local(&mut buf, responder_local);
                }
                write_extras(&mut buf, *commit, None);
            }
        }
        buf
    }

    /// Decode a body received under `msgtype`.
    ///
    /// Keys may come in any order and unknown keys are skipped, so a peer
    /// that adds fields later still decodes here.
    pub fn decode(msgtype: u16, data: &[u8]) -> Result<Self, WireError> {
        if !is_signal_msgtype(msgtype) {
            return Err(WireError::UnknownMsgtype);
        }
        let fields = Fields::parse(data).ok_or(WireError::Malformed)?;
        let session = fields.session.ok_or(WireError::Malformed)?;
        let signal = match msgtype {
            MSG_UPGRADE_REQUEST => Signal::Request {
                session,
                facilitator: fields.facilitator.ok_or(WireError::Malformed)?,
                initiator_public: fields.address.ok_or(WireError::Malformed)?,
                protocol: fields
                    .protocol
                    .map(ProbeProtocol::from_wire)
                    .unwrap_or(ProbeProtocol::Rnsp),
                initiator_local: fields.local,
                commit: fields.commit,
                host_payload: fields.host_payload,
            },
            MSG_UPGRADE_ACCEPT => Signal::Accept { session },
            MSG_UPGRADE_REJECT => Signal::Reject {
                session,
                reason: fields
                    .reason
                    .and_then(|r| u8::try_from(r).ok())
                    .ok_or(WireError::Malformed)?,
            },
            MSG_UPGRADE_READY => Signal::Ready {
                session,
                responder_public: fields.address.ok_or(WireError::Malformed)?,
                responder_local: fields.local,
                commit: fields.commit,
            },
            _ => Signal::Complete { session },
        };
        Ok(signal)
    }
}

/// The union of fields any signal may carry, as found in one body.
#[derive(Default)]
struct Fields {
    session: Option<SessionId>,
    facilitator: Option<SocketAddr>,
    address: Option<SocketAddr>,
    protocol: Option<u64>,
    reason: Option<u64>,
    local: Vec<SocketAddr>,
    commit: bool,
    host_payload: Option<Vec<u8>>,
}

impl Fields {
    fn parse(data: &[u8]) -> Option<Self> {
        let mut pos = 0;
        let count = msgpack::read_map_len(data, &mut pos)?;
        let mut fields = Fields::default();
        for _ in 0..count {
            let key = msgpack::read_msgpack_str(data, &mut pos)?;
            match key {
                b"s" => {
                    let bin = msgpack::read_msgpack_bin(data, &mut pos)?;
                    fields.session = Some(bin.try_into().ok()?);
                }
                b"f" => fields.facilitator = Some(read_endpoint(data, &mut pos)?),
                b"a" => fields.address = Some(read_endpoint(data, &mut pos)?),
                b"p" => fields.protocol = Some(msgpack::read_msgpack_uint(data, &mut pos)?),
                b"r" => fields.reason = Some(msgpack::read_msgpack_uint(data, &mut pos)?),
                b"l" => fields.local = read_local(data, &mut pos)?,
                b"c" => fields.commit = read_bool(data, &mut pos)?,
                b"h" => {
                    let bin = msgpack::read_msgpack_bin(data, &mut pos)?;
                    if bin.len() > MAX_HOST_PAYLOAD {
                        return None;
                    }
                    fields.host_payload = Some(bin.to_vec());
                }
                _ => msgpack::skip_msgpack_value(data, &mut pos)?,
            }
        }
        Some(fields)
    }
}

fn write_session(buf: &mut Vec<u8>, session: &SessionId) {
    msgpack::write_fixstr(buf, "s");
    msgpack::write_bin(buf, session);
}

/// An endpoint is `[addr_bytes, port]`: 4 bytes for IPv4, 16 for IPv6.
fn write_endpoint(buf: &mut Vec<u8>, addr: &SocketAddr) {
    msgpack::write_fixarray_header(buf, 2);
    match addr.ip() {
        IpAddr::V4(ip) => msgpack::write_bin(buf, &ip.octets()),
        IpAddr::V6(ip) => msgpack::write_bin(buf, &ip.octets()),
    }
    msgpack::write_uint(buf, u64::from(addr.port()));
}

/// +ciris: `"c"` (the two-sided commit, leviculum#89) and `"h"` (the host
/// payload, leviculum#88), each written only when set, so a signal without
/// them is byte for byte what rns-rs sends; an rns-rs decoder skips both.
fn write_extras(buf: &mut Vec<u8>, commit: bool, host_payload: Option<&[u8]>) {
    if commit {
        msgpack::write_fixstr(buf, "c");
        msgpack::write_bool(buf, true);
    }
    if let Some(payload) = host_payload {
        msgpack::write_fixstr(buf, "h");
        msgpack::write_bin(buf, payload);
    }
}

fn read_bool(data: &[u8], pos: &mut usize) -> Option<bool> {
    let byte = *data.get(*pos)?;
    let value = match byte {
        0xc3 => true,
        0xc2 => false,
        _ => return None,
    };
    *pos += 1;
    Some(value)
}

/// +ciris: `"l"`, the sender's host candidates, as an array of endpoints.
/// Written only when there are any, so a signal without them is byte for
/// byte what rns-rs sends; an rns-rs decoder skips the unknown key.
fn write_local(buf: &mut Vec<u8>, local: &[SocketAddr]) {
    let local = &local[..local.len().min(MAX_LOCAL_CANDIDATES)];
    msgpack::write_fixstr(buf, "l");
    msgpack::write_fixarray_header(buf, local.len());
    for addr in local {
        write_endpoint(buf, addr);
    }
}

/// The candidates past [`MAX_LOCAL_CANDIDATES`] are skipped, not kept.
fn read_local(data: &[u8], pos: &mut usize) -> Option<Vec<SocketAddr>> {
    let len = msgpack::read_array_len(data, pos)?;
    let mut out = Vec::new();
    for _ in 0..len {
        if out.len() < MAX_LOCAL_CANDIDATES {
            out.push(read_endpoint(data, pos)?);
        } else {
            msgpack::skip_msgpack_value(data, pos)?;
        }
    }
    Some(out)
}

fn read_endpoint(data: &[u8], pos: &mut usize) -> Option<SocketAddr> {
    let len = msgpack::read_array_len(data, pos)?;
    if len < 2 {
        return None;
    }
    let ip = ip_from_bytes(msgpack::read_msgpack_bin(data, pos)?)?;
    let port = u16::try_from(msgpack::read_msgpack_uint(data, pos)?).ok()?;
    for _ in 2..len {
        msgpack::skip_msgpack_value(data, pos)?;
    }
    Some(SocketAddr::new(ip, port))
}

fn ip_from_bytes(bytes: &[u8]) -> Option<IpAddr> {
    if let Ok(v4) = <[u8; 4]>::try_from(bytes) {
        Some(IpAddr::V4(Ipv4Addr::from(v4)))
    } else if let Ok(v6) = <[u8; 16]>::try_from(bytes) {
        Some(IpAddr::V6(Ipv6Addr::from(v6)))
    } else {
        None
    }
}

/// The secret both punch frames carry, bound to this link and this session.
///
/// `HKDF-SHA256(ikm = link key, salt = session id, info = "rns-holepunch-v1")`,
/// 32 bytes. The link key is the 64-byte key both ends derived at link
/// establishment, so only the two link endpoints can compute it, and a fresh
/// session id gives a fresh token.
pub fn punch_token(link_key: &[u8], session: &SessionId) -> [u8; 32] {
    let mut token = [0u8; 32];
    derive_key(link_key, Some(session), Some(PUNCH_TOKEN_INFO), &mut token);
    token
}

// RNSP probe

const RNSP_MAGIC: &[u8; 4] = b"RNSP";
const RNSP_VERSION: u8 = 1;
/// Length of an RNSP request: magic, version, nonce.
pub const RNSP_REQUEST_LEN: usize = 4 + 1 + 16;
const FAMILY_V4: u8 = 4;
const FAMILY_V6: u8 = 6;

/// An RNSP request carrying `nonce`, which the facilitator echoes.
pub fn rnsp_request(nonce: &[u8; 16]) -> [u8; RNSP_REQUEST_LEN] {
    let mut out = [0u8; RNSP_REQUEST_LEN];
    out[..4].copy_from_slice(RNSP_MAGIC);
    out[4] = RNSP_VERSION;
    out[5..].copy_from_slice(nonce);
    out
}

/// Facilitator side: the nonce of a well-formed RNSP request.
pub fn parse_rnsp_request(data: &[u8]) -> Option<[u8; 16]> {
    if data.len() != RNSP_REQUEST_LEN || &data[..4] != RNSP_MAGIC || data[4] != RNSP_VERSION {
        return None;
    }
    data[5..].try_into().ok()
}

/// Facilitator side: the answer telling the prober where it was seen from.
pub fn rnsp_response(nonce: &[u8; 16], observed: &SocketAddr) -> Vec<u8> {
    let mut out = Vec::with_capacity(RNSP_REQUEST_LEN + 1 + 16 + 2);
    out.extend_from_slice(RNSP_MAGIC);
    out.push(RNSP_VERSION);
    out.extend_from_slice(nonce);
    match observed.ip() {
        IpAddr::V4(ip) => {
            out.push(FAMILY_V4);
            out.extend_from_slice(&ip.octets());
        }
        IpAddr::V6(ip) => {
            out.push(FAMILY_V6);
            out.extend_from_slice(&ip.octets());
        }
    }
    out.extend_from_slice(&observed.port().to_be_bytes());
    out
}

/// Prober side: the observed address in an RNSP answer to `nonce`.
pub fn parse_rnsp_response(data: &[u8], nonce: &[u8; 16]) -> Option<SocketAddr> {
    if data.len() < RNSP_REQUEST_LEN + 1
        || &data[..4] != RNSP_MAGIC
        || data[4] != RNSP_VERSION
        || &data[5..RNSP_REQUEST_LEN] != nonce
    {
        return None;
    }
    let rest = &data[RNSP_REQUEST_LEN + 1..];
    let addr_len = match data[RNSP_REQUEST_LEN] {
        FAMILY_V4 => 4,
        FAMILY_V6 => 16,
        _ => return None,
    };
    if rest.len() < addr_len + 2 {
        return None;
    }
    let ip = ip_from_bytes(&rest[..addr_len])?;
    let port = u16::from_be_bytes([rest[addr_len], rest[addr_len + 1]]);
    Some(SocketAddr::new(ip, port))
}

// STUN (RFC 5389), the subset a Binding exchange needs

const STUN_COOKIE: u32 = 0x2112_A442;
const STUN_BINDING_REQUEST: u16 = 0x0001;
const STUN_BINDING_SUCCESS: u16 = 0x0101;
const STUN_ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const STUN_ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const STUN_HEADER_LEN: usize = 20;

/// A Binding Request with no attributes.
pub fn stun_binding_request(txn: &[u8; 12]) -> [u8; STUN_HEADER_LEN] {
    let mut out = [0u8; STUN_HEADER_LEN];
    out[..2].copy_from_slice(&STUN_BINDING_REQUEST.to_be_bytes());
    // Bytes 2..4 stay zero: no attributes.
    out[4..8].copy_from_slice(&STUN_COOKIE.to_be_bytes());
    out[8..].copy_from_slice(txn);
    out
}

/// Facilitator side: the transaction id of a Binding Request.
///
/// A facilitator answers STUN as well as RNSP, so a peer configured for
/// either protocol can probe a leviculum facilitator.
pub fn parse_stun_binding_request(data: &[u8]) -> Option<[u8; 12]> {
    if data.len() < STUN_HEADER_LEN
        || u16::from_be_bytes([data[0], data[1]]) != STUN_BINDING_REQUEST
        || u32::from_be_bytes([data[4], data[5], data[6], data[7]]) != STUN_COOKIE
    {
        return None;
    }
    data[8..STUN_HEADER_LEN].try_into().ok()
}

/// Facilitator side: a Binding Success carrying XOR-MAPPED-ADDRESS.
pub fn stun_binding_response(txn: &[u8; 12], observed: &SocketAddr) -> Vec<u8> {
    let mut value = Vec::with_capacity(20);
    value.push(0);
    let port = observed.port() ^ (STUN_COOKIE >> 16) as u16;
    match observed.ip() {
        IpAddr::V4(ip) => {
            value.push(0x01);
            value.extend_from_slice(&port.to_be_bytes());
            value.extend_from_slice(&xor_address(&ip.octets(), txn));
        }
        IpAddr::V6(ip) => {
            value.push(0x02);
            value.extend_from_slice(&port.to_be_bytes());
            value.extend_from_slice(&xor_address(&ip.octets(), txn));
        }
    }
    let mut out = Vec::with_capacity(STUN_HEADER_LEN + 4 + value.len());
    out.extend_from_slice(&STUN_BINDING_SUCCESS.to_be_bytes());
    out.extend_from_slice(&((4 + value.len()) as u16).to_be_bytes());
    out.extend_from_slice(&STUN_COOKIE.to_be_bytes());
    out.extend_from_slice(txn);
    out.extend_from_slice(&STUN_ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(&value);
    out
}

/// Prober side: the reflexive address in a Binding Success for `txn`.
///
/// XOR-MAPPED-ADDRESS wins; plain MAPPED-ADDRESS is read only when the
/// server sent no XOR form (RFC 3489-era servers).
pub fn parse_stun_binding_response(data: &[u8], txn: &[u8; 12]) -> Option<SocketAddr> {
    if data.len() < STUN_HEADER_LEN
        || u16::from_be_bytes([data[0], data[1]]) != STUN_BINDING_SUCCESS
        || u32::from_be_bytes([data[4], data[5], data[6], data[7]]) != STUN_COOKIE
        || &data[8..STUN_HEADER_LEN] != txn
    {
        return None;
    }
    let body_len = usize::from(u16::from_be_bytes([data[2], data[3]]));
    let body = data.get(STUN_HEADER_LEN..STUN_HEADER_LEN + body_len)?;

    let mut plain = None;
    let mut rest = body;
    while rest.len() >= 4 {
        let kind = u16::from_be_bytes([rest[0], rest[1]]);
        let len = usize::from(u16::from_be_bytes([rest[2], rest[3]]));
        let value = rest.get(4..4 + len)?;
        match kind {
            STUN_ATTR_XOR_MAPPED_ADDRESS => return parse_stun_address(value, Some(txn)),
            STUN_ATTR_MAPPED_ADDRESS if plain.is_none() => {
                plain = parse_stun_address(value, None);
            }
            _ => {}
        }
        // Attribute values are padded to a 4-byte boundary.
        let advance = 4 + len.div_ceil(4) * 4;
        rest = rest.get(advance..).unwrap_or(&[]);
    }
    plain
}

/// A (XOR-)MAPPED-ADDRESS value; `txn` is `Some` for the XOR form.
fn parse_stun_address(value: &[u8], txn: Option<&[u8; 12]>) -> Option<SocketAddr> {
    if value.len() < 4 {
        return None;
    }
    let mut port = u16::from_be_bytes([value[2], value[3]]);
    let addr_len = match value[1] {
        0x01 => 4,
        0x02 => 16,
        _ => return None,
    };
    let raw = value.get(4..4 + addr_len)?;
    let ip = match txn {
        Some(txn) => {
            port ^= (STUN_COOKIE >> 16) as u16;
            ip_from_bytes(&xor_address(raw, txn))?
        }
        None => ip_from_bytes(raw)?,
    };
    Some(SocketAddr::new(ip, port))
}

/// XOR an address with the cookie, then (IPv6 only) the transaction id.
fn xor_address(addr: &[u8], txn: &[u8; 12]) -> Vec<u8> {
    let cookie = STUN_COOKIE.to_be_bytes();
    addr.iter()
        .enumerate()
        .map(|(i, b)| {
            let mask = if i < 4 { cookie[i] } else { txn[i - 4] };
            b ^ mask
        })
        .collect()
}

// Punch frames

const PUNCH_MAGIC: &[u8; 4] = b"RNSH";
const ACK_MAGIC: &[u8; 4] = b"RNSA";
/// Length of every punch and ack frame.
pub const PUNCH_FRAME_LEN: usize = 4 + 16 + 32 + 4;
/// The sequence number a keepalive carries; ordinary punches count up from 0.
pub const KEEPALIVE_SEQ: u32 = u32::MAX;

/// Which of the two punch frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PunchKind {
    /// "I am sending to you" (also the keepalive).
    Punch,
    /// "Your punch reached me", echoing its sequence number.
    Ack,
}

/// A punch or ack frame for this session.
pub fn punch_frame(
    kind: PunchKind,
    session: &SessionId,
    token: &[u8; 32],
    seq: u32,
) -> [u8; PUNCH_FRAME_LEN] {
    let mut out = [0u8; PUNCH_FRAME_LEN];
    out[..4].copy_from_slice(match kind {
        PunchKind::Punch => PUNCH_MAGIC,
        PunchKind::Ack => ACK_MAGIC,
    });
    out[4..20].copy_from_slice(session);
    out[20..52].copy_from_slice(token);
    out[52..].copy_from_slice(&seq.to_be_bytes());
    out
}

/// The kind and sequence of a frame that belongs to this session.
pub fn parse_punch_frame(
    data: &[u8],
    session: &SessionId,
    token: &[u8; 32],
) -> Option<(PunchKind, u32)> {
    let kind = punch_frame_kind(data)?;
    if &data[4..20] != session || &data[20..52] != token {
        return None;
    }
    Some((
        kind,
        u32::from_be_bytes([data[52], data[53], data[54], data[55]]),
    ))
}

/// The kind of a punch-shaped frame, whatever session it claims.
///
/// A direct interface drops every such frame instead of handing it to
/// transport. No Reticulum packet can look like one: its second byte would
/// be a hop count of 78.
pub fn punch_frame_kind(data: &[u8]) -> Option<PunchKind> {
    if data.len() != PUNCH_FRAME_LEN {
        return None;
    }
    match &data[..4] {
        m if m == PUNCH_MAGIC => Some(PunchKind::Punch),
        m if m == ACK_MAGIC => Some(PunchKind::Ack),
        _ => None,
    }
}

/// +ciris (leviculum#104): the magic of a path-MTU probe.
const MTU_PROBE_MAGIC: &[u8; 4] = b"RNSM";

/// +ciris (leviculum#104): the probe sizes, largest first. Each is the size
/// of the UDP payload, which is what a Reticulum packet at that MTU fills:
/// `DIRECT_LINK_MTU`, then the IPv6 minimum link MTU. A path that carries
/// neither (a tunnel, an APN or 464XLAT clamping below it) signals no MTU,
/// so links over it hold the base 500.
pub const MTU_PROBE_STEPS: [u32; 2] = [super::DIRECT_LINK_MTU, 1280];

/// The high half of the sequence number a probe carries, and its ack echoes.
const MTU_PROBE_SEQ_TAG: u32 = 0xFFFE_0000;

/// +ciris (leviculum#104): a path-MTU probe of `size` bytes for this
/// session: a punch frame with its own magic, the probe's size in its
/// sequence number, zero-padded to `size`. The peer answers with an ordinary
/// [`PunchKind::Ack`] echoing that sequence number. A peer that predates
/// probes does not recognise the frame (its length is not
/// [`PUNCH_FRAME_LEN`]), hands it to transport, which drops it as a
/// malformed packet, and never acks, which reads as "too large".
pub fn mtu_probe_frame(session: &SessionId, token: &[u8; 32], size: u32) -> alloc::vec::Vec<u8> {
    let size = (size as usize).max(PUNCH_FRAME_LEN);
    let mut out = alloc::vec![0u8; size];
    out[..4].copy_from_slice(MTU_PROBE_MAGIC);
    out[4..20].copy_from_slice(session);
    out[20..52].copy_from_slice(token);
    out[52..56].copy_from_slice(&mtu_probe_seq(size as u32).to_be_bytes());
    out
}

/// The sequence number a probe of `size` bytes carries.
pub const fn mtu_probe_seq(size: u32) -> u32 {
    MTU_PROBE_SEQ_TAG | (size & 0xFFFF)
}

/// Whether `data` is probe-shaped, whatever session it claims. A direct
/// interface never hands one to transport.
pub fn is_mtu_probe(data: &[u8]) -> bool {
    data.len() >= PUNCH_FRAME_LEN && &data[..4] == MTU_PROBE_MAGIC
}

/// The sequence number to ack for a probe that belongs to this session and
/// arrived whole: its length matches the size it names.
pub fn parse_mtu_probe(data: &[u8], session: &SessionId, token: &[u8; 32]) -> Option<u32> {
    if !is_mtu_probe(data) || &data[4..20] != session || &data[20..52] != token {
        return None;
    }
    let seq = u32::from_be_bytes([data[52], data[53], data[54], data[55]]);
    (seq == mtu_probe_seq(data.len() as u32)).then_some(seq)
}

/// The probe size an ack's sequence number confirms, if it is a probe ack.
pub const fn mtu_probe_acked_size(seq: u32) -> Option<u32> {
    if seq & 0xFFFF_0000 == MTU_PROBE_SEQ_TAG {
        Some(seq & 0xFFFF)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    const SESSION: SessionId = [0x11; 16];

    fn facilitator() -> SocketAddr {
        "203.0.113.7:4343".parse().unwrap()
    }

    fn public_v4() -> SocketAddr {
        "198.51.100.23:40000".parse().unwrap()
    }

    fn public_v6() -> SocketAddr {
        "[2001:db8::1]:5".parse().unwrap()
    }

    // The golden bodies below were produced by rns-rs 0.7.3's own encoder
    // (`rns-core/src/holepunch/engine.rs`, commit 98205ea) for the same
    // inputs, so equality here is equality with their wire.

    #[test]
    fn request_rnsp_matches_rns_rs_bytes() {
        let signal = Signal::Request {
            session: SESSION,
            facilitator: facilitator(),
            initiator_public: public_v4(),
            protocol: ProbeProtocol::Rnsp,
            initiator_local: Vec::new(),
            commit: false,
            host_payload: None,
        };
        let golden = hex(
            "83a173c41011111111111111111111111111111111a16692c404cb007107cd10f7a16192c404c6336417cd9c40",
        );
        assert_eq!(signal.encode(), golden);
        assert_eq!(Signal::decode(MSG_UPGRADE_REQUEST, &golden), Ok(signal));
    }

    #[test]
    fn request_stun_v6_matches_rns_rs_bytes() {
        let signal = Signal::Request {
            session: SESSION,
            facilitator: facilitator(),
            initiator_public: public_v6(),
            protocol: ProbeProtocol::Stun,
            initiator_local: Vec::new(),
            commit: false,
            host_payload: None,
        };
        let golden = hex(
            "84a173c41011111111111111111111111111111111a16692c404cb007107cd10f7a16192c41020010db800000000000000000000000105a17001",
        );
        assert_eq!(signal.encode(), golden);
        assert_eq!(Signal::decode(MSG_UPGRADE_REQUEST, &golden), Ok(signal));
    }

    #[test]
    fn short_signals_match_rns_rs_bytes() {
        let cases = [
            (
                Signal::Accept { session: SESSION },
                "81a173c41011111111111111111111111111111111",
            ),
            (
                Signal::Reject {
                    session: SESSION,
                    reason: REJECT_BUSY,
                },
                "82a173c41011111111111111111111111111111111a17202",
            ),
            (
                Signal::Ready {
                    session: SESSION,
                    responder_public: public_v4(),
                    responder_local: Vec::new(),
                    commit: false,
                },
                "82a173c41011111111111111111111111111111111a16192c404c6336417cd9c40",
            ),
            (
                Signal::Complete { session: SESSION },
                "81a173c41011111111111111111111111111111111",
            ),
        ];
        for (signal, golden) in cases {
            let golden = hex(golden);
            assert_eq!(signal.encode(), golden, "{signal:?}");
            assert_eq!(Signal::decode(signal.msgtype(), &golden), Ok(signal));
        }
    }

    #[test]
    fn punch_token_matches_rns_rs() {
        let key: Vec<u8> = (0u8..64).collect();
        assert_eq!(
            punch_token(&key, &SESSION).to_vec(),
            hex("7f73b9da94a1bf8f00e01f7322a12b221245a3b8e853a122e9464e17cb872019")
        );
    }

    #[test]
    fn decode_tolerates_key_order_and_unknown_keys() {
        // {"x": nil, "a": [..], "s": .., } for READY: reordered, one extra key.
        let mut body = Vec::new();
        msgpack::write_fixmap_header(&mut body, 3);
        msgpack::write_fixstr(&mut body, "x");
        msgpack::write_nil(&mut body);
        msgpack::write_fixstr(&mut body, "a");
        write_endpoint(&mut body, &public_v6());
        write_session(&mut body, &SESSION);
        assert_eq!(
            Signal::decode(MSG_UPGRADE_READY, &body),
            Ok(Signal::Ready {
                session: SESSION,
                responder_public: public_v6(),
                responder_local: Vec::new(),
                commit: false,
            })
        );
    }

    /// +ciris: host candidates ride `"l"` on REQUEST and READY, round-trip,
    /// and are capped at [`MAX_LOCAL_CANDIDATES`] on both ends.
    #[test]
    fn host_candidates_round_trip_and_are_capped() {
        let lan = |n: u8| SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, n)), 40_000);
        let request = Signal::Request {
            session: SESSION,
            facilitator: public_v4(),
            initiator_public: public_v4(),
            protocol: ProbeProtocol::Rnsp,
            initiator_local: alloc::vec![lan(10)],
            commit: false,
            host_payload: None,
        };
        assert_eq!(
            Signal::decode(MSG_UPGRADE_REQUEST, &request.encode()),
            Ok(request)
        );
        let many: Vec<SocketAddr> = (1..=9).map(lan).collect();
        let ready = Signal::Ready {
            session: SESSION,
            responder_public: public_v6(),
            responder_local: many.clone(),
            commit: false,
        };
        assert_eq!(
            Signal::decode(MSG_UPGRADE_READY, &ready.encode()),
            Ok(Signal::Ready {
                session: SESSION,
                responder_public: public_v6(),
                responder_local: many[..MAX_LOCAL_CANDIDATES].to_vec(),
                commit: false,
            })
        );
    }

    #[test]
    fn decode_refuses_what_it_cannot_use() {
        let accept = Signal::Accept { session: SESSION }.encode();
        // A READY needs an address; an ACCEPT body lacks one.
        assert_eq!(
            Signal::decode(MSG_UPGRADE_READY, &accept),
            Err(WireError::Malformed)
        );
        assert_eq!(
            Signal::decode(0x1234, &accept),
            Err(WireError::UnknownMsgtype)
        );
        assert_eq!(
            Signal::decode(MSG_UPGRADE_ACCEPT, &accept[..10]),
            Err(WireError::Malformed)
        );
        // A 15-byte session is not a session.
        let mut short = Vec::new();
        msgpack::write_fixmap_header(&mut short, 1);
        msgpack::write_fixstr(&mut short, "s");
        msgpack::write_bin(&mut short, &[0u8; 15]);
        assert_eq!(
            Signal::decode(MSG_UPGRADE_ACCEPT, &short),
            Err(WireError::Malformed)
        );
    }

    #[test]
    fn msgtype_range_is_exactly_the_five_signals() {
        assert!(!is_signal_msgtype(0xFDFF));
        for t in 0xFE00..=0xFE04 {
            assert!(is_signal_msgtype(t));
        }
        assert!(!is_signal_msgtype(0xFE05));
        // The Buffer system's stream message stays outside the range.
        assert!(!is_signal_msgtype(0xFF00));
    }

    #[test]
    fn rnsp_round_trip_both_families() {
        let nonce = [7u8; 16];
        let request = rnsp_request(&nonce);
        assert_eq!(parse_rnsp_request(&request), Some(nonce));
        for observed in [public_v4(), public_v6()] {
            let response = rnsp_response(&nonce, &observed);
            assert_eq!(response.len(), if observed.is_ipv4() { 28 } else { 40 });
            assert_eq!(parse_rnsp_response(&response, &nonce), Some(observed));
            assert_eq!(parse_rnsp_response(&response, &[8u8; 16]), None);
        }
        assert_eq!(parse_rnsp_request(&request[..20]), None);
    }

    #[test]
    fn stun_round_trip_both_families() {
        let txn = [0x5a; 12];
        let request = stun_binding_request(&txn);
        assert_eq!(parse_stun_binding_request(&request), Some(txn));
        for observed in [public_v4(), public_v6()] {
            let response = stun_binding_response(&txn, &observed);
            assert_eq!(parse_stun_binding_response(&response, &txn), Some(observed));
            assert_eq!(parse_stun_binding_response(&response, &[0; 12]), None);
        }
    }

    #[test]
    fn stun_reads_an_rfc5389_vector() {
        // RFC 5769 §2.2: IPv4 Binding Success, XOR-MAPPED-ADDRESS
        // 192.0.2.1:32853, preceded by a SOFTWARE attribute and followed by
        // MESSAGE-INTEGRITY and FINGERPRINT.
        let response = hex(concat!(
            "0101003c2112a442b7e7a701bc34d686fa87dfae",
            "8022000b7465737420766563746f7220",
            "002000080001a147e112a643",
            "000800142b91f599fd9e90c38c7489f92af9ba53f06be7d7",
            "80280004c07d4c96",
        ));
        let txn: [u8; 12] = hex("b7e7a701bc34d686fa87dfae").try_into().unwrap();
        assert_eq!(
            parse_stun_binding_response(&response, &txn),
            Some("192.0.2.1:32853".parse().unwrap())
        );
    }

    #[test]
    fn punch_frames_carry_their_session() {
        let token = [0x22; 32];
        let frame = punch_frame(PunchKind::Ack, &SESSION, &token, 9);
        assert_eq!(
            parse_punch_frame(&frame, &SESSION, &token),
            Some((PunchKind::Ack, 9))
        );
        assert_eq!(parse_punch_frame(&frame, &SESSION, &[0x23; 32]), None);
        assert_eq!(parse_punch_frame(&frame, &[0x12; 16], &token), None);
        assert_eq!(punch_frame_kind(&frame), Some(PunchKind::Ack));
        assert_eq!(punch_frame_kind(&frame[..55]), None);
        let keepalive = punch_frame(PunchKind::Punch, &SESSION, &token, KEEPALIVE_SEQ);
        assert_eq!(&keepalive[..4], b"RNSH");
    }

    #[test]
    fn an_mtu_probe_is_its_size_and_is_acked_by_size() {
        let token = [7u8; 32];
        let frame = mtu_probe_frame(&SESSION, &token, 1280);
        assert_eq!(frame.len(), 1280);
        assert!(is_mtu_probe(&frame));
        assert_eq!(
            punch_frame_kind(&frame),
            None,
            "never a punch to an old peer"
        );
        let seq = parse_mtu_probe(&frame, &SESSION, &token).expect("ours");
        assert_eq!(mtu_probe_acked_size(seq), Some(1280));
        assert_eq!(
            parse_mtu_probe(&frame, &SESSION, &[8u8; 32]),
            None,
            "wrong token"
        );
        assert_eq!(
            parse_mtu_probe(&frame[..1000], &SESSION, &token),
            None,
            "truncated"
        );
        assert_eq!(
            mtu_probe_acked_size(5),
            None,
            "a keepalive ack is not a probe ack"
        );
    }
}
