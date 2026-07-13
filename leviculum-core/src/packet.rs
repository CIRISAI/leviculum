//! Packet structure and serialization
//!
//! Packet format:
//! [Flags (1)] [Hops (1)] [Header (19-35)] [Context (1)] [Payload (variable)]
//!
//! The header size depends on header type:
//! - Header Type 1: destination_hash only (16 bytes)
//! - Header Type 2: transport_id + destination_hash (32 bytes)

#[cfg(test)]
use crate::constants::MTU;
use crate::constants::{HEADER_MAXSIZE, HEADER_MINSIZE, MDU, TRUNCATED_HASHBYTES};

// Flag Byte Bit Masks
// Bit layout: [ifac:1][header_type:1][context:1][transport:1][dest_type:2][packet_type:2]

/// Bit mask for IFAC (Interface Access Code) flag (bit 7)
const FLAG_IFAC_MASK: u8 = 0x80;
/// Bit mask for header type flag (bit 6)
const FLAG_HEADER_TYPE_MASK: u8 = 0x40;
/// Bit mask for context flag (bit 5)
const FLAG_CONTEXT_MASK: u8 = 0x20;
/// Bit mask for transport type flag (bit 4)
const FLAG_TRANSPORT_MASK: u8 = 0x10;
/// Bit shift for destination type (bits 3-2)
const FLAG_DEST_TYPE_SHIFT: u8 = 2;
/// Bit mask for destination/packet type (2 bits)
const FLAG_TYPE_MASK: u8 = 0x03;
use crate::destination::DestinationType;

/// Packet type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketType {
    /// Regular data packet
    Data = 0x00,
    /// Destination presence announcement
    Announce = 0x01,
    /// Link establishment request
    LinkRequest = 0x02,
    /// Proof/receipt
    Proof = 0x03,
}

impl TryFrom<u8> for PacketType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value & 0x03 {
            0x00 => Ok(PacketType::Data),
            0x01 => Ok(PacketType::Announce),
            0x02 => Ok(PacketType::LinkRequest),
            0x03 => Ok(PacketType::Proof),
            _ => Err(()),
        }
    }
}

/// Transport type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TransportType {
    /// Broadcast to all reachable interfaces
    Broadcast = 0x00,
    /// Routed via transport layer
    Transport = 0x01,
}

/// Header type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HeaderType {
    /// Header Type 1: destination_hash only
    Type1 = 0x00,
    /// Header Type 2: transport_id + destination_hash
    Type2 = 0x01,
}

/// Packet context (stored in context byte)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketContext {
    None = 0x00,
    Resource = 0x01,
    ResourceAdv = 0x02,
    ResourceReq = 0x03,
    ResourceHmu = 0x04,
    ResourcePrf = 0x05,
    ResourceIcl = 0x06,
    ResourceRcl = 0x07,
    CacheRequest = 0x08,
    Request = 0x09,
    Response = 0x0A,
    PathResponse = 0x0B,
    Command = 0x0C,
    CommandStatus = 0x0D,
    Channel = 0x0E,
    Keepalive = 0xFA,
    LinkIdentify = 0xFB,
    LinkClose = 0xFC,
    LinkProof = 0xFD,
    Lrrtt = 0xFE,
    Lrproof = 0xFF,
}

impl TryFrom<u8> for PacketContext {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(PacketContext::None),
            0x01 => Ok(PacketContext::Resource),
            0x02 => Ok(PacketContext::ResourceAdv),
            0x03 => Ok(PacketContext::ResourceReq),
            0x04 => Ok(PacketContext::ResourceHmu),
            0x05 => Ok(PacketContext::ResourcePrf),
            0x06 => Ok(PacketContext::ResourceIcl),
            0x07 => Ok(PacketContext::ResourceRcl),
            0x08 => Ok(PacketContext::CacheRequest),
            0x09 => Ok(PacketContext::Request),
            0x0A => Ok(PacketContext::Response),
            0x0B => Ok(PacketContext::PathResponse),
            0x0C => Ok(PacketContext::Command),
            0x0D => Ok(PacketContext::CommandStatus),
            0x0E => Ok(PacketContext::Channel),
            0xFA => Ok(PacketContext::Keepalive),
            0xFB => Ok(PacketContext::LinkIdentify),
            0xFC => Ok(PacketContext::LinkClose),
            0xFD => Ok(PacketContext::LinkProof),
            0xFE => Ok(PacketContext::Lrrtt),
            0xFF => Ok(PacketContext::Lrproof),
            _ => Err(()),
        }
    }
}

/// Packet flags byte layout (matching Python Reticulum):
/// Bit 7: IFAC Flag (0=open/public interface, 1=authenticated interface)
/// Bit 6: Header Type (0=H1, 1=H2)
/// Bit 5: Context Flag (0=unset, 1=set)
/// Bit 4: Transport Type (0=broadcast, 1=transport)
/// Bits 3-2: Destination Type
/// Bits 1-0: Packet Type
#[derive(Debug, Clone, Copy)]
pub struct PacketFlags {
    /// IFAC (Interface Access Code) flag - indicates authenticated interface
    pub ifac_flag: bool,
    pub header_type: HeaderType,
    pub context_flag: bool,
    pub transport_type: TransportType,
    pub dest_type: DestinationType,
    pub packet_type: PacketType,
}

impl PacketFlags {
    /// Encode flags to a byte
    pub fn to_byte(&self) -> u8 {
        let mut flags = 0u8;
        flags |= (self.ifac_flag as u8) << 7;
        flags |= (self.header_type as u8) << 6;
        flags |= (self.context_flag as u8) << 5;
        flags |= (self.transport_type as u8) << 4;
        flags |= (self.dest_type as u8) << 2;
        flags |= self.packet_type as u8;
        flags
    }

    /// Decode flags from a byte
    pub fn from_byte(byte: u8) -> Result<Self, PacketError> {
        let ifac_flag = byte & FLAG_IFAC_MASK != 0;
        let header_type = if byte & FLAG_HEADER_TYPE_MASK != 0 {
            HeaderType::Type2
        } else {
            HeaderType::Type1
        };
        let context_flag = byte & FLAG_CONTEXT_MASK != 0;
        let transport_type = if byte & FLAG_TRANSPORT_MASK != 0 {
            TransportType::Transport
        } else {
            TransportType::Broadcast
        };
        let dest_type = DestinationType::try_from((byte >> FLAG_DEST_TYPE_SHIFT) & FLAG_TYPE_MASK)
            .map_err(|_| PacketError::InvalidFlags)?;
        let packet_type =
            PacketType::try_from(byte & FLAG_TYPE_MASK).map_err(|_| PacketError::InvalidFlags)?;

        Ok(Self {
            ifac_flag,
            header_type,
            context_flag,
            transport_type,
            dest_type,
            packet_type,
        })
    }
}

/// Packet parsing/construction errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketError {
    /// Packet too short
    TooShort,
    /// Packet too long
    TooLong,
    /// Invalid flags byte
    InvalidFlags,
    /// Invalid context byte
    InvalidContext,
    /// Payload too large
    PayloadTooLarge,
}

impl core::fmt::Display for PacketError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PacketError::TooShort => write!(f, "packet too short"),
            PacketError::TooLong => write!(f, "packet too long"),
            PacketError::InvalidFlags => write!(f, "invalid flags byte"),
            PacketError::InvalidContext => write!(f, "invalid context byte"),
            PacketError::PayloadTooLarge => write!(f, "payload too large"),
        }
    }
}

/// A Reticulum packet (unpacked form)
#[derive(Debug)]
pub struct Packet {
    /// Packet flags
    pub flags: PacketFlags,
    /// Hop count
    pub hops: u8,
    /// Transport ID (only for Header Type 2)
    pub transport_id: Option<[u8; TRUNCATED_HASHBYTES]>,
    /// Destination hash
    pub destination_hash: [u8; TRUNCATED_HASHBYTES],
    /// Packet context
    pub context: PacketContext,
    /// Payload data (may be encrypted)
    pub data: PacketData,
}

/// Packet data storage
#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // Inline variant intentional for no_std embedded use
pub enum PacketData {
    /// Borrowed data (for parsing)
    Owned(alloc::vec::Vec<u8>),
    /// Fixed-size inline buffer for no_std
    Inline { buffer: [u8; MDU], len: usize },
}

impl PacketData {
    /// Get the data as a slice
    pub fn as_slice(&self) -> &[u8] {
        match self {
            PacketData::Owned(v) => v,
            PacketData::Inline { buffer, len } => &buffer[..*len],
        }
    }

    /// Get the data length
    pub fn len(&self) -> usize {
        match self {
            PacketData::Owned(v) => v.len(),
            PacketData::Inline { len, .. } => *len,
        }
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Packet {
    /// Calculate the header size based on header type
    pub fn header_size(&self) -> usize {
        match self.flags.header_type {
            HeaderType::Type1 => HEADER_MINSIZE,
            HeaderType::Type2 => HEADER_MAXSIZE,
        }
    }

    /// Calculate the total packed size
    ///
    /// `header_size()` already covers flags + hops + destination hash(es) +
    /// context, so the payload is all that is added.
    pub fn packed_size(&self) -> usize {
        self.header_size() + self.data.len()
    }

    /// Pack the packet into a byte buffer
    ///
    /// The caller controls the maximum packet size by choosing the buffer
    /// size. Packets created locally should use `[0u8; MTU]`; forwarded
    /// packets (which may use a negotiated link MTU larger than the base
    /// MTU) should use a buffer sized to `packed_size()`.
    pub fn pack(&self, output: &mut [u8]) -> Result<usize, PacketError> {
        let size = self.packed_size();
        if output.len() < size {
            return Err(PacketError::TooShort);
        }

        let mut pos = 0;

        output[pos] = self.flags.to_byte();
        pos += 1;

        output[pos] = self.hops;
        pos += 1;

        // Header
        if self.flags.header_type == HeaderType::Type2 {
            if let Some(ref tid) = self.transport_id {
                output[pos..pos + TRUNCATED_HASHBYTES].copy_from_slice(tid);
                pos += TRUNCATED_HASHBYTES;
            } else {
                return Err(PacketError::InvalidFlags);
            }
        }
        output[pos..pos + TRUNCATED_HASHBYTES].copy_from_slice(&self.destination_hash);
        pos += TRUNCATED_HASHBYTES;

        // Context
        output[pos] = self.context as u8;
        pos += 1;

        // Data
        let data = self.data.as_slice();
        output[pos..pos + data.len()].copy_from_slice(data);
        pos += data.len();

        Ok(pos)
    }

    /// Unpack a packet from bytes
    ///
    /// No upper size limit is enforced: a transport relay must accept
    /// any deframed packet, including those using a negotiated link MTU
    /// larger than the base protocol MTU (link MTU discovery).
    pub fn unpack(raw: &[u8]) -> Result<Self, PacketError> {
        // HEADER_MINSIZE includes flags(1) + hops(1) + dest_hash(16) + context(1) = 19 bytes
        if raw.len() < HEADER_MINSIZE {
            return Err(PacketError::TooShort);
        }

        let mut pos = 0;

        // Flags
        let flags = PacketFlags::from_byte(raw[pos])?;
        pos += 1;

        // Hops
        let hops = raw[pos];
        pos += 1;

        // Header
        let transport_id = if flags.header_type == HeaderType::Type2 {
            // HEADER_MAXSIZE includes transport_id(16) + dest_hash(16) = 35 bytes total
            if raw.len() < HEADER_MAXSIZE {
                return Err(PacketError::TooShort);
            }
            let mut tid = [0u8; TRUNCATED_HASHBYTES];
            tid.copy_from_slice(&raw[pos..pos + TRUNCATED_HASHBYTES]);
            pos += TRUNCATED_HASHBYTES;
            Some(tid)
        } else {
            None
        };

        let mut destination_hash = [0u8; TRUNCATED_HASHBYTES];
        destination_hash.copy_from_slice(&raw[pos..pos + TRUNCATED_HASHBYTES]);
        pos += TRUNCATED_HASHBYTES;

        // Context
        let context = PacketContext::try_from(raw[pos]).map_err(|_| PacketError::InvalidContext)?;
        pos += 1;

        // Data
        let data = PacketData::Owned(raw[pos..].to_vec());

        Ok(Self {
            flags,
            hops,
            transport_id,
            destination_hash,
            context,
            data,
        })
    }
}

/// Compute the hashable part of a raw packet (matching Python Reticulum)
///
/// The hashable part strips routing-specific information so that the hash
/// remains consistent regardless of whether the packet was forwarded:
/// - Flags byte with upper nibble masked out (removes ifac, header_type, context, transport bits)
/// - Everything after transport_id (destination_hash + context + data)
///
/// This matches Python's `Packet.get_hashable_part()`.
///
/// # Arguments
/// * `raw` - The raw packet bytes
///
/// # Returns
/// The hashable portion of the packet
pub fn get_hashable_part(raw: &[u8]) -> alloc::vec::Vec<u8> {
    if raw.len() < 2 {
        return alloc::vec::Vec::new();
    }

    let flags = raw[0];
    let is_header_type_2 = (flags & FLAG_HEADER_TYPE_MASK) != 0;

    // Data starts after: flags(1) + hops(1) + [transport_id(16) if HEADER_2]
    let data_start = if is_header_type_2 {
        2 + TRUNCATED_HASHBYTES // Skip transport_id
    } else {
        2
    };

    if raw.len() < data_start {
        return alloc::vec::Vec::new();
    }

    let mut hashable = alloc::vec::Vec::with_capacity(1 + raw.len() - data_start);
    // First byte: flags with upper nibble masked out
    hashable.push(flags & 0x0F);
    // Everything after transport_id (or after hops for HEADER_1)
    hashable.extend_from_slice(&raw[data_start..]);
    hashable
}

/// Compute the packet hash (matching Python Reticulum)
///
/// This computes SHA256 of the hashable part of the packet. The hash is
/// consistent regardless of forwarding because it excludes routing information.
///
/// Use this instead of `sha256(raw_packet)` for:
/// - Packet receipts
/// - Proof generation and validation
/// - Packet deduplication
///
/// # Arguments
/// * `raw` - The raw packet bytes
///
/// # Returns
/// The 32-byte SHA256 hash
///
/// # Example
/// ```
/// use leviculum_core::packet::packet_hash;
///
/// let raw_packet = [0x00, 0x01, /* ... destination, context, data ... */];
/// let hash = packet_hash(&raw_packet);
/// ```
pub fn packet_hash(raw: &[u8]) -> [u8; 32] {
    use crate::crypto::sha256;
    let hashable = get_hashable_part(raw);
    sha256(&hashable)
}

/// Compute the truncated packet hash (16 bytes)
///
/// Returns the first 16 bytes of the packet hash, used for reverse_table
/// routing and proof routing. Not used for deduplication, the full 32-byte
/// hash is used for that (matching Python Transport.py:1227).
pub fn truncated_packet_hash(raw: &[u8]) -> [u8; TRUNCATED_HASHBYTES] {
    let hash = packet_hash(raw);
    let mut truncated = [0u8; TRUNCATED_HASHBYTES];
    truncated.copy_from_slice(&hash[..TRUNCATED_HASHBYTES]);
    truncated
}

/// Build a PROOF packet for delivery confirmation
///
/// A proof packet is sent from the receiver back to the sender to confirm
/// that a packet was received. The proof contains the packet hash and a
/// signature by the receiver.
///
/// # Arguments
/// * `destination_hash` - The destination to send the proof to (the original sender)
/// * `proof_data` - The proof data (96 bytes: packet_hash + signature)
///
/// # Returns
/// A `Packet` ready to be packed and sent
///
/// # Example
/// ```
/// use leviculum_core::packet::build_proof_packet;
///
/// let dest_hash = [0x01u8; 16];
/// let proof_data = [0x42u8; 96]; // hash + signature
/// let packet = build_proof_packet(&dest_hash, &proof_data);
/// ```
pub fn build_proof_packet(
    destination_hash: &[u8; TRUNCATED_HASHBYTES],
    proof_data: &[u8],
) -> Packet {
    Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Single,
            packet_type: PacketType::Proof,
        },
        hops: 0,
        transport_id: None,
        destination_hash: *destination_hash,
        context: PacketContext::None,
        data: PacketData::Owned(proof_data.to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flags_roundtrip() {
        let flags = PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Single,
            packet_type: PacketType::Data,
        };

        let byte = flags.to_byte();
        let decoded = PacketFlags::from_byte(byte).unwrap();

        assert_eq!(decoded.ifac_flag, flags.ifac_flag);
        assert_eq!(decoded.header_type as u8, flags.header_type as u8);
        assert_eq!(decoded.context_flag, flags.context_flag);
        assert_eq!(decoded.transport_type as u8, flags.transport_type as u8);
        assert_eq!(decoded.dest_type as u8, flags.dest_type as u8);
        assert_eq!(decoded.packet_type as u8, flags.packet_type as u8);
    }

    /// `packed_size()` must report exactly the number of bytes `pack()` writes.
    ///
    /// `header_size()` already accounts for flags + hops + context (that is what
    /// HEADER_MINSIZE/HEADER_MAXSIZE mean), so adding `2 + ... + 1` on top of it
    /// double-counts them. Callers treat `packed_size()` as the wire length and
    /// as the MTU budget, so an over-count silently steals usable payload.
    #[test]
    fn test_packed_size_matches_packed_len() {
        for (header_type, transport_id) in [
            (HeaderType::Type1, None),
            (HeaderType::Type2, Some([0x11; TRUNCATED_HASHBYTES])),
        ] {
            let packet = Packet {
                flags: PacketFlags {
                    ifac_flag: false,
                    header_type,
                    context_flag: false,
                    transport_type: TransportType::Broadcast,
                    dest_type: DestinationType::Single,
                    packet_type: PacketType::Data,
                },
                hops: 0,
                transport_id,
                destination_hash: [0x42; TRUNCATED_HASHBYTES],
                context: PacketContext::None,
                data: PacketData::Owned(b"Hello".to_vec()),
            };

            let mut buffer = [0u8; MTU];
            let written = packet.pack(&mut buffer).unwrap();

            assert_eq!(
                packet.packed_size(),
                written,
                "packed_size() disagrees with pack() for {header_type:?}"
            );
        }
    }

    /// A packet whose true wire length is exactly the MTU must pack into an
    /// MTU-sized buffer. An over-counting `packed_size()` rejects it as
    /// `TooShort` — the buffer is "too short" only for the phantom bytes.
    #[test]
    fn test_packet_of_exactly_mtu_packs() {
        let payload_len = MTU - HEADER_MINSIZE;
        let packet = Packet {
            flags: PacketFlags {
                ifac_flag: false,
                header_type: HeaderType::Type1,
                context_flag: false,
                transport_type: TransportType::Broadcast,
                dest_type: DestinationType::Single,
                packet_type: PacketType::Data,
            },
            hops: 0,
            transport_id: None,
            destination_hash: [0x42; TRUNCATED_HASHBYTES],
            context: PacketContext::None,
            data: PacketData::Owned(alloc::vec![0xAA; payload_len]),
        };

        let mut buffer = [0u8; MTU];
        let written = packet.pack(&mut buffer).expect("MTU-sized packet must pack");
        assert_eq!(written, MTU);
    }

    #[test]
    fn test_packet_pack_unpack() {
        let packet = Packet {
            flags: PacketFlags {
                ifac_flag: false,
                header_type: HeaderType::Type1,
                context_flag: false,
                transport_type: TransportType::Broadcast,
                dest_type: DestinationType::Single,
                packet_type: PacketType::Data,
            },
            hops: 0,
            transport_id: None,
            destination_hash: [0x42; TRUNCATED_HASHBYTES],
            context: PacketContext::None,
            data: PacketData::Owned(b"Hello".to_vec()),
        };

        let mut buffer = [0u8; MTU];
        let size = packet.pack(&mut buffer).unwrap();

        let unpacked = Packet::unpack(&buffer[..size]).unwrap();
        assert_eq!(unpacked.hops, packet.hops);
        assert_eq!(unpacked.destination_hash, packet.destination_hash);
        assert_eq!(unpacked.data.as_slice(), b"Hello");
    }

    #[test]
    fn test_ifac_flag_roundtrip() {
        // Test with IFAC flag set
        let flags_with_ifac = PacketFlags {
            ifac_flag: true,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Single,
            packet_type: PacketType::Data,
        };

        let byte = flags_with_ifac.to_byte();
        assert_eq!(byte & 0x80, 0x80, "IFAC flag (bit 7) should be set");

        let decoded = PacketFlags::from_byte(byte).unwrap();
        assert!(decoded.ifac_flag, "IFAC flag should be true after decode");

        // Test without IFAC flag
        let flags_without_ifac = PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Single,
            packet_type: PacketType::Data,
        };

        let byte = flags_without_ifac.to_byte();
        assert_eq!(byte & 0x80, 0x00, "IFAC flag (bit 7) should be unset");

        let decoded = PacketFlags::from_byte(byte).unwrap();
        assert!(!decoded.ifac_flag, "IFAC flag should be false after decode");
    }

    #[test]
    fn test_flags_all_bits_set() {
        // Test with all flags set to verify no bit interference
        let flags = PacketFlags {
            ifac_flag: true,
            header_type: HeaderType::Type2,
            context_flag: true,
            transport_type: TransportType::Transport,
            dest_type: DestinationType::Link,
            packet_type: PacketType::Proof,
        };

        let byte = flags.to_byte();
        // Expected: 1_1_1_1_11_11 = 0xFF
        assert_eq!(byte, 0xFF, "All bits should be set");

        let decoded = PacketFlags::from_byte(byte).unwrap();
        assert!(decoded.ifac_flag);
        assert_eq!(decoded.header_type as u8, HeaderType::Type2 as u8);
        assert!(decoded.context_flag);
        assert_eq!(decoded.transport_type as u8, TransportType::Transport as u8);
        assert_eq!(decoded.dest_type as u8, DestinationType::Link as u8);
        assert_eq!(decoded.packet_type as u8, PacketType::Proof as u8);
    }
}
