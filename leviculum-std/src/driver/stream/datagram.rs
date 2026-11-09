//! Unordered link datagrams (+ciris, leviculum#102); see
//! [`leviculum_core::node::NodeCore::send_unordered`].

use super::LinkHandle;
use crate::driver::ReticulumNode;
use crate::error::Error;
use crate::sync_ext::MutexRecover;
use leviculum_core::node::DatagramStats;
use leviculum_core::LinkId;

impl LinkHandle {
    /// Send `data` as one unordered, unreliable datagram (live media), and
    /// return the sequence number it carries.
    ///
    /// Sent once, never acknowledged, retransmitted or held for order: the
    /// peer gets `NodeEvent::LinkDatagramReceived` with the sequence number
    /// as soon as it decrypts, and a lost one stays lost. Never waits: an
    /// interface that cannot take a frame yet is the error
    /// [`try_send`](Self::try_send) gives for it, for the caller to drop or
    /// keep the frame. The payload may be up to
    /// [`ReticulumNode::link_datagram_mdu`] bytes.
    ///
    /// Only for a peer running a leviculum with leviculum#102; any other
    /// drops the datagram unread. [`try_send_timed`](Self::try_send_timed)
    /// is the fallback. Takes the core lock.
    pub async fn send_unordered(&self, data: &[u8]) -> Result<u32, Error> {
        if self.closed {
            return Err(Error::NotRunning);
        }
        crate::driver::dispatch_committed(&self.action_dispatch_tx, || {
            let mut core = self.inner.lock_recover();
            Ok(core.send_unordered(&self.link_id, data)?)
        })
        .await
    }
}

impl ReticulumNode {
    /// The largest payload [`LinkHandle::send_unordered`] takes on a link:
    /// its MDU less the datagram's sequence number. `None` if the link does
    /// not exist.
    pub fn link_datagram_mdu(&self, link_id: &LinkId) -> Option<usize> {
        self.inner.lock_recover().link_datagram_mdu(link_id)
    }

    /// What a link has sent and received as datagrams. `None` if the link
    /// does not exist.
    pub fn link_datagram_stats(&self, link_id: &LinkId) -> Option<DatagramStats> {
        self.inner.lock_recover().link_datagram_stats(link_id)
    }
}
