//! Channel frames with a deadline (+ciris, leviculum#94); the wire side is
//! [`crate::link::channel::timed`].

use rand_core::CryptoRngCore;

use crate::link::channel::timed;
use crate::link::channel::Envelope;
use crate::link::LinkId;
use crate::traits::{Clock, Storage};

use super::{send, FrameExpiry, NodeCore, NodeEvent};

impl<R: CryptoRngCore, C: Clock, S: Storage> NodeCore<R, C, S> {
    /// [`send_on_link`](Self::send_on_link) for a frame worth delivering only
    /// within `ttl_ms` of now (live media).
    ///
    /// Until it is confirmed the frame is retransmitted as usual; once
    /// `ttl_ms` has passed it never is: an expired marker goes in its place
    /// under the same sequence number, so the ordered stream stays whole, and
    /// [`NodeEvent::ChannelMessageExpired`] (`FrameExpiry::Sender`) reports
    /// it. The receiver drops a frame that reaches it older than `ttl_ms`
    /// (it waited behind a gap) instead of delivering it late.
    ///
    /// The frame costs [`timed::TIMED_HEADER_LEN`] bytes of the channel MDU.
    /// Only for a peer running leviculum 0.33.0+ciris.1 or later: other
    /// stacks do not know the frame's message type.
    pub fn send_on_link_timed(
        &mut self,
        link_id: &LinkId,
        data: &[u8],
        ttl_ms: u32,
    ) -> Result<crate::transport::TickOutput, send::SendError> {
        self.send_on_link_ttl(link_id, data, Some(ttl_ms))
    }

    /// A timed frame or an expired marker came off the channel in order:
    /// deliver the frame if it is still fresh, otherwise report the drop.
    pub(super) fn deliver_timed_frame(&mut self, link_id: LinkId, envelope: Envelope) {
        let sequence = envelope.sequence;
        if envelope.msgtype == timed::MSG_EXPIRED {
            self.events.push(NodeEvent::ChannelMessageExpired {
                link_id,
                sequence,
                expiry: FrameExpiry::MarkedByPeer,
            });
            return;
        }
        let Some(frame) = timed::unwrap(&envelope.data) else {
            crate::tracing::debug!(seq = sequence, "timed frame too short, dropped");
            return;
        };
        let now_ms = self.transport.clock().now_ms();
        let age_ms = match self.links.get_mut(&link_id) {
            Some(link) => link.timed_age_mut().age_ms(frame.sent_ms, now_ms),
            None => return,
        };
        if age_ms > u64::from(frame.ttl_ms) {
            self.events.push(NodeEvent::ChannelMessageExpired {
                link_id,
                sequence,
                expiry: FrameExpiry::ArrivedLate { age_ms },
            });
            return;
        }
        self.events.push(NodeEvent::MessageReceived {
            link_id,
            msgtype: frame.msgtype,
            sequence,
            data: frame.data.to_vec(),
        });
    }
}
