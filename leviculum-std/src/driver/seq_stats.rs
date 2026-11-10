//! A link's Channel sequence watermarks (+ciris); see
//! [`leviculum_core::node::LinkSeqStats`].

use super::ReticulumNode;
use crate::sync_ext::MutexRecover;

impl ReticulumNode {
    /// The link's newest sent Channel sequence and its oldest unproved one,
    /// for a per-message delivery proof
    /// ([`LinkSeqStats::delivered_through`](leviculum_core::node::LinkSeqStats::delivered_through));
    /// `None` when the link is gone or has sent nothing on its Channel.
    pub fn link_seq_stats(
        &self,
        link_id: &leviculum_core::link::LinkId,
    ) -> Option<leviculum_core::node::LinkSeqStats> {
        self.inner.lock_recover().link_seq_stats(link_id)
    }
}
