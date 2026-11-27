//! What a frame is, for an egress scheduler (+ciris, leviculum egress
//! scheduler, `docs/egress-scheduler.md`).
//!
//! [`dispatch_actions`](crate::transport::dispatch_actions) reads every
//! outbound frame's header BEFORE the IFAC wrap masks it and hands the
//! answer to the interface beside the frame
//! ([`Interface::hint_next_frame`](crate::traits::Interface::hint_next_frame)),
//! the same way it hands the journey tag. The class comes from the wire type
//! and the context byte only: nothing is decrypted, nothing is added to the
//! wire, and an interface that schedules nothing ignores the hint (the trait
//! default).
//!
//! The header cannot tell a Channel on a media link from one on any other
//! link, nor audio from video; those are the owner's to say, and the std
//! scheduler takes them as per-link and per-send hints keyed by
//! [`FrameHint::dst`] and [`FrameHint::fingerprint`].

use crate::constants::TRUNCATED_HASHBYTES;
use crate::packet::{PacketContext, FLAG_HEADER_TYPE_MASK};

/// The class of an outbound frame, read from its header.
///
/// The order of the variants is not a priority order; the scheduler that
/// reads the class decides that (leviculum-std: Control and Proof, then
/// LinkSetup, then Media, then Announce, then Bulk).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameClass {
    /// Link and destination traffic that is neither media, nor a resource,
    /// nor link setup: Channel messages, requests and responses, plain data.
    Control,
    /// Every PROOF packet: delivery proofs, link proofs (LRPROOF) and
    /// resource proofs.
    Proof,
    /// Link requests, the link RTT packet, identify, close and keepalive.
    LinkSetup,
    /// An unordered link datagram (`PacketContext::Datagram`, leviculum#102):
    /// live media, worth its moment only.
    Media,
    /// Announces (path responses included) and path requests.
    Announce,
    /// Resource transfer: advertisement, parts, requests, hashmap updates
    /// and cancels (every `Resource*` context of a DATA packet).
    Bulk,
}

/// What [`dispatch_actions`](crate::transport::dispatch_actions) tells an
/// interface about the frame its next `try_send*` call hands over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHint {
    /// The class the header gives.
    pub class: FrameClass,
    /// The destination hash in the header (the link id, for a link
    /// packet), all zeros when the frame is too short to carry one. The key
    /// a per-link hint is looked up by.
    pub dst: [u8; TRUNCATED_HASHBYTES],
    /// [`frame_fingerprint`] of the frame: the key a per-send hint is looked
    /// up by. Computed for every frame; only a [`FrameClass::Media`] frame's
    /// is ever looked up.
    pub fingerprint: u64,
}

/// The truncated hash of the `rnstransport.path.request` PLAIN destination,
/// `Transport::compute_path_request_hash`, as a constant so the per-frame
/// classification hashes nothing. A unit test holds it to the computed one.
pub const PATH_REQUEST_DEST: [u8; TRUNCATED_HASHBYTES] = [
    0x6b, 0x9f, 0x66, 0x01, 0x4d, 0x98, 0x53, 0xfa, 0xab, 0x22, 0x0f, 0xba, 0x47, 0xd0, 0x27, 0x61,
];

/// Offset of the destination hash in raw (un-IFAC'd) frame bytes.
fn dst_at(flags: u8) -> usize {
    if flags & FLAG_HEADER_TYPE_MASK != 0 {
        2 + TRUNCATED_HASHBYTES
    } else {
        2
    }
}

/// Classify raw outbound frame bytes, as the core built them (before IFAC).
///
/// Total and allocation-free. A frame too short for a context byte is
/// [`FrameClass::Control`], which a scheduler never drops by age.
pub fn classify_frame(data: &[u8]) -> FrameHint {
    let flags = data.first().copied().unwrap_or(0);
    let at = dst_at(flags);
    let mut dst = [0u8; TRUNCATED_HASHBYTES];
    if let Some(d) = data.get(at..at + TRUNCATED_HASHBYTES) {
        dst.copy_from_slice(d);
    }
    let context = data
        .get(at + TRUNCATED_HASHBYTES)
        .map(|b| PacketContext::from_byte(*b));
    let class = match flags & 0x03 {
        0x01 => FrameClass::Announce,
        0x02 => FrameClass::LinkSetup,
        0x03 => FrameClass::Proof,
        _ => match context {
            Some(PacketContext::Datagram) => FrameClass::Media,
            Some(
                PacketContext::Resource
                | PacketContext::ResourceAdv
                | PacketContext::ResourceReq
                | PacketContext::ResourceHmu
                | PacketContext::ResourcePrf
                | PacketContext::ResourceIcl
                | PacketContext::ResourceRcl,
            ) => FrameClass::Bulk,
            Some(
                PacketContext::Lrrtt
                | PacketContext::LinkIdentify
                | PacketContext::LinkClose
                | PacketContext::Keepalive,
            ) => FrameClass::LinkSetup,
            Some(PacketContext::None) if dst == PATH_REQUEST_DEST => FrameClass::Announce,
            _ => FrameClass::Control,
        },
    };
    FrameHint {
        class,
        dst,
        fingerprint: frame_fingerprint(data),
    }
}

/// A cheap key for one frame: its length and the first eight payload bytes
/// behind the context byte. For a link datagram those are the head of the
/// token's random IV, so two datagrams share a key with negligible odds; a
/// collision only gives a datagram the other's audio/video hint.
pub fn frame_fingerprint(data: &[u8]) -> u64 {
    let flags = data.first().copied().unwrap_or(0);
    let at = dst_at(flags) + TRUNCATED_HASHBYTES + 1;
    let mut head = [0u8; 8];
    if let Some(src) = data.get(at..) {
        let n = src.len().min(8);
        head[..n].copy_from_slice(&src[..n]);
    }
    u64::from_le_bytes(head) ^ ((data.len() as u64) << 48)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::{HeaderType, Packet, PacketData, PacketFlags, PacketType, TransportType};
    use crate::DestinationType;
    use alloc::vec;
    use alloc::vec::Vec;

    fn frame(ptype: PacketType, context: PacketContext, header_2: bool, dst: [u8; 16]) -> Vec<u8> {
        let packet = Packet {
            flags: PacketFlags {
                ifac_flag: false,
                header_type: if header_2 {
                    HeaderType::Type2
                } else {
                    HeaderType::Type1
                },
                context_flag: false,
                transport_type: if header_2 {
                    TransportType::Transport
                } else {
                    TransportType::Broadcast
                },
                dest_type: DestinationType::Link,
                packet_type: ptype,
            },
            hops: 0,
            transport_id: if header_2 { Some([0xEE; 16]) } else { None },
            destination_hash: dst,
            context,
            data: PacketData::Owned(vec![0xA5; 40]),
        };
        let mut buf = [0u8; crate::constants::MTU];
        let n = packet.pack(&mut buf).expect("pack");
        buf[..n].to_vec()
    }

    /// The constant is the hash the transport computes.
    #[test]
    fn path_request_dest_is_the_computed_hash() {
        let name_hash = crate::destination::Destination::compute_name_hash(
            "rnstransport",
            &["path", "request"],
        );
        assert_eq!(crate::crypto::truncated_hash(&name_hash), PATH_REQUEST_DEST);
    }

    /// Every packet kind lands in its class, for both header types.
    #[test]
    fn class_for_every_packet_kind() {
        use FrameClass::*;
        use PacketContext as C;
        let cases: &[(PacketType, PacketContext, FrameClass)] = &[
            (PacketType::Data, C::Channel, Control),
            (PacketType::Data, C::Request, Control),
            (PacketType::Data, C::Response, Control),
            (PacketType::Data, C::None, Control),
            (PacketType::Proof, C::None, Proof),
            (PacketType::Proof, C::Lrproof, Proof),
            (PacketType::Proof, C::ResourcePrf, Proof),
            (PacketType::LinkRequest, C::None, LinkSetup),
            (PacketType::Data, C::Lrrtt, LinkSetup),
            (PacketType::Data, C::LinkIdentify, LinkSetup),
            (PacketType::Data, C::LinkClose, LinkSetup),
            (PacketType::Data, C::Keepalive, LinkSetup),
            (PacketType::Data, C::Datagram, Media),
            (PacketType::Announce, C::None, Announce),
            (PacketType::Announce, C::PathResponse, Announce),
            (PacketType::Data, C::Resource, Bulk),
            (PacketType::Data, C::ResourceAdv, Bulk),
            (PacketType::Data, C::ResourceReq, Bulk),
            (PacketType::Data, C::ResourceHmu, Bulk),
            (PacketType::Data, C::ResourcePrf, Bulk),
            (PacketType::Data, C::ResourceIcl, Bulk),
            (PacketType::Data, C::ResourceRcl, Bulk),
        ];
        for header_2 in [false, true] {
            for (ptype, ctx, want) in cases {
                let f = frame(*ptype, *ctx, header_2, [0x11; 16]);
                let hint = classify_frame(&f);
                assert_eq!(hint.class, *want, "{ptype:?}/{ctx:?} header_2={header_2}");
                assert_eq!(hint.dst, [0x11; 16]);
            }
        }
    }

    /// A path request is DATA with no context to the well-known
    /// destination; the same packet to any other destination is Control.
    #[test]
    fn path_request_is_announce_class() {
        let f = frame(
            PacketType::Data,
            PacketContext::None,
            false,
            PATH_REQUEST_DEST,
        );
        assert_eq!(classify_frame(&f).class, FrameClass::Announce);
        let f = frame(PacketType::Data, PacketContext::None, false, [0x22; 16]);
        assert_eq!(classify_frame(&f).class, FrameClass::Control);
    }

    /// Truncated bytes classify without panicking, and never as Media.
    #[test]
    fn short_frames_are_control() {
        for len in 0..20 {
            let f = vec![0u8; len];
            let hint = classify_frame(&f);
            assert_ne!(hint.class, FrameClass::Media);
        }
        assert_eq!(classify_frame(&[]).class, FrameClass::Control);
    }

    /// The fingerprint reads the payload head: frames that differ only
    /// there differ in fingerprint.
    #[test]
    fn fingerprint_reads_the_payload_head() {
        let mut a = frame(PacketType::Data, PacketContext::Datagram, false, [1; 16]);
        let b = a.clone();
        assert_eq!(frame_fingerprint(&a), frame_fingerprint(&b));
        a[19] ^= 0xFF;
        assert_ne!(frame_fingerprint(&a), frame_fingerprint(&b));
    }

    /// A recording interface: the bytes it was handed and the hint beside
    /// each.
    struct Recorder {
        sent: Vec<Vec<u8>>,
        hints: Vec<FrameHint>,
        pending: Option<FrameHint>,
    }

    impl crate::traits::Interface for Recorder {
        fn id(&self) -> crate::transport::InterfaceId {
            crate::transport::InterfaceId(0)
        }
        fn name(&self) -> &str {
            "rec"
        }
        fn mtu(&self) -> usize {
            crate::constants::MTU
        }
        fn is_online(&self) -> bool {
            true
        }
        fn try_send(&mut self, data: &[u8]) -> Result<(), crate::traits::InterfaceError> {
            self.sent.push(data.to_vec());
            self.hints
                .push(self.pending.take().expect("hinted before the send"));
            Ok(())
        }
        fn hint_next_frame(&mut self, hint: FrameHint) {
            self.pending = Some(hint);
        }
    }

    /// `dispatch_actions` hands the class read BEFORE the IFAC wrap, for a
    /// directed frame and a broadcast alike: the interface gets masked bytes
    /// and the frame's real class beside them.
    #[test]
    fn dispatch_hints_the_class_before_ifac() {
        use crate::transport::{dispatch_actions, Action, InterfaceId};
        let mut ifac = alloc::collections::BTreeMap::new();
        ifac.insert(
            0,
            crate::ifac::IfacConfig::new(Some("net"), Some("pass"), 16).unwrap(),
        );
        let datagram = frame(PacketType::Data, PacketContext::Datagram, false, [7; 16]);
        let resource = frame(PacketType::Data, PacketContext::Resource, false, [7; 16]);
        let announce = frame(PacketType::Announce, PacketContext::None, false, [8; 16]);
        let mut rec = Recorder {
            sent: Vec::new(),
            hints: Vec::new(),
            pending: None,
        };
        {
            let mut ifaces: Vec<&mut dyn crate::traits::Interface> = vec![&mut rec];
            let actions = vec![
                Action::SendPacket {
                    peer: None,
                    iface: InterfaceId(0),
                    data: datagram.clone(),
                },
                Action::SendPacket {
                    peer: None,
                    iface: InterfaceId(0),
                    data: resource.clone(),
                },
                Action::Broadcast {
                    data: announce.clone(),
                    exclude_iface: None,
                    exclude_ifaces: Vec::new(),
                    exclude_peer: None,
                },
            ];
            let result = dispatch_actions(&mut ifaces, actions, &ifac);
            assert!(result.is_clean());
        }
        assert_eq!(rec.sent.len(), 3);
        for (sent, raw) in rec.sent.iter().zip([&datagram, &resource, &announce]) {
            assert_ne!(sent, raw, "the frame went out IFAC-wrapped");
        }
        let classes: Vec<FrameClass> = rec.hints.iter().map(|h| h.class).collect();
        assert_eq!(
            classes,
            vec![FrameClass::Media, FrameClass::Bulk, FrameClass::Announce]
        );
        assert_eq!(rec.hints[0].dst, [7; 16]);
        assert_eq!(rec.hints[0].fingerprint, frame_fingerprint(&datagram));
    }
}
