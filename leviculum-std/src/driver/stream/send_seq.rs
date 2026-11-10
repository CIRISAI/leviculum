//! A Channel send that returns its sequence (+ciris); see
//! [`leviculum_core::node::NodeCore::send_on_link_seq`].

use super::LinkHandle;
use crate::error::Error;
use crate::sync_ext::MutexRecover;

impl LinkHandle {
    /// [`try_send`](Self::try_send), returning the Channel sequence the
    /// message went out under. The sequence is read under the same core
    /// lock as the send, so no other sender's message can come between.
    ///
    /// The message has been proved delivered, together with everything sent
    /// on this link before it, once
    /// [`LinkStats::delivered_through`](leviculum_core::node::LinkStats::delivered_through)
    /// holds for that sequence. The link stats come from the driver's
    /// `link_stats`. Takes the core lock.
    pub async fn try_send_seq(&self, data: &[u8]) -> Result<u16, Error> {
        if self.closed {
            return Err(Error::NotRunning);
        }
        crate::driver::dispatch_committed(&self.action_dispatch_tx, || {
            let mut core = self.inner.lock_recover();
            Ok(core.send_on_link_seq(&self.link_id, data)?)
        })
        .await
    }
}
