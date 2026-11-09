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
    /// as soon as it decrypts and passes its replay window, and a lost one
    /// stays lost. An interface that cannot take a frame yet is the error
    /// [`try_send`](Self::try_send) gives for it; one that is full when the
    /// frame reaches it sheds the frame (counted as
    /// `DropReason::DatagramShed`) rather than queueing it behind, or ahead
    /// of, the link's Channel traffic. The payload may be up to
    /// [`ReticulumNode::link_datagram_mdu`] bytes.
    ///
    /// Never waits for the interface, but does wait for room in the event
    /// loop's action queue, as every `LinkHandle` send does; a media loop
    /// that must never wait calls [`Self::try_send_unordered`].
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

    /// [`Self::send_unordered`] that never waits: when the event loop's
    /// action queue is full the datagram is not sent and the error is
    /// `SendError::Busy`, for the caller to drop the frame. Nothing is
    /// sealed or numbered for a datagram refused this way.
    pub fn try_send_unordered(&self, data: &[u8]) -> Result<u32, Error> {
        use tokio::sync::mpsc::error::TrySendError;
        if self.closed {
            return Err(Error::NotRunning);
        }
        let permit = self.action_dispatch_tx.try_reserve().map_err(|e| match e {
            TrySendError::Full(()) => Error::from(leviculum_core::SendError::Busy),
            TrySendError::Closed(()) => Error::NotRunning,
        })?;
        let (sequence, output) = {
            let mut core = self.inner.lock_recover();
            core.send_unordered(&self.link_id, data)?
        };
        permit.send(output);
        Ok(sequence)
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
