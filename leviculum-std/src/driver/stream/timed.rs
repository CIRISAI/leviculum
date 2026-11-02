//! Frames with a deadline on a link (+ciris, leviculum#94); see
//! [`leviculum_core::node::NodeCore::send_on_link_timed`].

use std::time::Duration;

use super::LinkHandle;
use crate::error::Error;
use crate::sync_ext::MutexRecover;

impl LinkHandle {
    /// Send a frame worth delivering only within `ttl` (live media).
    ///
    /// Never waits: a full window or a pacing delay comes back as the error
    /// [`try_send`](Self::try_send) gives, for the caller to drop or keep
    /// the frame. Once sent, the frame is retransmitted only until `ttl` has
    /// passed; after that an expired marker goes in its place and
    /// `NodeEvent::ChannelMessageExpired` reports it. The receiver drops a
    /// frame that reaches it older than `ttl` instead of delivering it late.
    /// A fresh frame arrives as an ordinary `MessageReceived`.
    ///
    /// Only for a peer running leviculum 0.33.0+ciris.1 or later. Takes the
    /// core lock.
    pub async fn try_send_timed(&self, data: &[u8], ttl: Duration) -> Result<(), Error> {
        if self.closed {
            return Err(Error::NotRunning);
        }
        let ttl_ms = u32::try_from(ttl.as_millis()).unwrap_or(u32::MAX).max(1);
        crate::driver::dispatch_committed(&self.action_dispatch_tx, || {
            let mut core = self.inner.lock_recover();
            Ok(((), core.send_on_link_timed(&self.link_id, data, ttl_ms)?))
        })
        .await
    }
}
