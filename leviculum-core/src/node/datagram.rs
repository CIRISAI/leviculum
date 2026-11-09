//! Unordered link datagrams (+ciris, leviculum#102); the wire side is
//! [`crate::link::datagram`].

use rand_core::CryptoRngCore;

use crate::hex_fmt::HexShort;
use crate::link::datagram::{self, DatagramStats, DATAGRAM_HEADER_LEN};
use crate::link::{LinkId, LinkState};
use crate::packet::{Packet, PacketContext};
use crate::traits::{Clock, Storage};
use crate::transport::DropReason;

use super::link_management::LinkRx;
use super::{send, NodeCore, NodeEvent};

/// The data-plane event slots a host keeps for Channel messages: below this
/// much room ([`NodeCore::set_channel_delivery_budget`]) an arriving
/// datagram is shed, not delivered. A quarter of leviculum-std's default
/// data-plane capacity (128).
pub const DATAGRAM_SINK_RESERVE: usize = 32;

impl<R: CryptoRngCore, C: Clock, S: Storage> NodeCore<R, C, S> {
    /// Send `data` on a link as one unordered, unreliable datagram (live
    /// media), returning the sequence number it carries.
    ///
    /// The packet is encrypted for the link like any link packet and sent
    /// once. Nothing waits for it: no window, no proof, no retransmit, and a
    /// datagram lost or overtaken stays that way. The peer gets it as
    /// [`NodeEvent::LinkDatagramReceived`] the moment it decrypts, with the
    /// sequence number, and does its own loss accounting and reordering.
    ///
    /// The interface gate of [`send_on_link`](Self::send_on_link) still
    /// applies: an interface that cannot take a frame yet is
    /// [`send::SendError::PacingDelay`], and the frame is the caller's to
    /// drop or keep. The payload may be up to
    /// [`link_datagram_mdu`](Self::link_datagram_mdu) bytes.
    ///
    /// Only for a peer running a leviculum that knows
    /// [`PacketContext::Datagram`]; any other drops it unread. Fall back to
    /// [`send_on_link_timed`](Self::send_on_link_timed) otherwise.
    pub fn send_unordered(
        &mut self,
        link_id: &LinkId,
        data: &[u8],
    ) -> Result<(u32, crate::transport::TickOutput), send::SendError> {
        let link_id = &self.resolve_link_id(link_id);
        let link = self.links.get(link_id).ok_or(send::SendError::NoLink)?;
        if link.state() != LinkState::Active {
            return Err(send::SendError::LinkFailed);
        }
        if data.len() > link.mdu().saturating_sub(DATAGRAM_HEADER_LEN) {
            return Err(send::SendError::TooLarge);
        }
        // The same interface gate an app-originated Channel send meets.
        let now_ms = self.transport.clock().now_ms();
        if let Some(iface_idx) = link.attached_interface() {
            let next_slot = self.transport.next_slot_ms_for_interface(iface_idx, now_ms);
            if next_slot > now_ms {
                if let Some(link) = self.links.get_mut(link_id) {
                    link.note_iface_pacing_rejection();
                }
                return Err(send::SendError::PacingDelay {
                    ready_at_ms: next_slot,
                });
            }
        }

        let link = self.links.get_mut(link_id).ok_or(send::SendError::NoLink)?;
        let sequence = link.datagrams_mut().take_sequence();
        let packet_bytes = link
            .build_data_packet_with_context(
                &datagram::wrap(sequence, data),
                PacketContext::Datagram,
                &mut self.rng,
            )
            .map_err(|_| send::SendError::LinkFailed)?;
        link.record_outbound(now_ms);
        let stats = &mut link.datagrams_mut().stats;
        stats.sent += 1;
        stats.sent_bytes += data.len() as u64;
        self.route_link_packet(link_id, &packet_bytes);

        Ok((sequence, self.process_events_and_actions()))
    }

    /// The largest payload [`send_unordered`](Self::send_unordered) takes on
    /// `link_id`: the link MDU less the sequence number. `None` when there is
    /// no such link.
    pub fn link_datagram_mdu(&self, link_id: &LinkId) -> Option<usize> {
        self.link(link_id)
            .map(|l| l.mdu().saturating_sub(DATAGRAM_HEADER_LEN))
    }

    /// What `link_id` has sent and received as datagrams. `None` when there
    /// is no such link.
    pub fn link_datagram_stats(&self, link_id: &LinkId) -> Option<DatagramStats> {
        self.link(link_id).map(|l| l.datagram_stats())
    }

    /// A datagram arrived on a link: deliver it if it decrypts.
    pub(super) fn handle_datagram_packet(
        &mut self,
        link_id: LinkId,
        packet: &Packet,
        rx: LinkRx<'_>,
        now_ms: u64,
    ) {
        let Some(link) = self.links.get_mut(&link_id) else {
            self.drop_link_packet(packet, rx, DropReason::LinkDataNoLink);
            return;
        };
        if link.state() != LinkState::Active {
            self.drop_link_packet(packet, rx, DropReason::LinkNotActive);
            return;
        }
        let encrypted = packet.data.as_slice();
        let mut plaintext = alloc::vec![0u8; encrypted.len()];
        let delivered = match link.decrypt(encrypted, &mut plaintext) {
            Ok(len) => {
                plaintext.truncate(len);
                datagram::unwrap(&plaintext).map(|(sequence, payload)| (sequence, payload.to_vec()))
            }
            Err(_) => None,
        };
        let Some((sequence, data)) = delivered else {
            link.datagrams_mut().stats.refused += 1;
            crate::tracing::debug!(
                link = %HexShort(link_id.as_bytes()),
                "Datagram did not decrypt or carries no sequence number, dropped"
            );
            self.drop_link_packet(packet, rx, DropReason::LinkDecryptFail);
            return;
        };
        // A replay is authentic ciphertext; the window stops it before it
        // counts as liveness or as the peer speaking on this interface.
        if !link.datagrams_mut().window.accept(sequence) {
            link.datagrams_mut().stats.replayed += 1;
            crate::tracing::debug!(
                link = %HexShort(link_id.as_bytes()),
                sequence,
                "Datagram replayed or older than the window, dropped"
            );
            return;
        }
        // Liveness only once it is known to be from the peer, and fresh.
        link.record_inbound(now_ms);
        self.direct_link_authenticated(&link_id, now_ms);
        // Keep the host's last `DATAGRAM_SINK_RESERVE` event slots for the
        // Channel: a datagram flood must never hold its delivery budget at 0
        // (leviculum#102 review). The Channel's own messages decrement the
        // same budget as they are delivered.
        let Some(link) = self.links.get_mut(&link_id) else {
            return;
        };
        if let Some(budget) = &mut self.channel_delivery_budget {
            if *budget <= DATAGRAM_SINK_RESERVE {
                link.datagrams_mut().stats.shed += 1;
                return;
            }
            *budget -= 1;
        }
        let stats = &mut link.datagrams_mut().stats;
        stats.received += 1;
        stats.received_bytes += data.len() as u64;
        self.events.push(NodeEvent::LinkDatagramReceived {
            link_id,
            sequence,
            data,
        });
    }
}
