//! The wire id of a link, for material both ends must derive alike
//! (CIRISVerify#309).

use leviculum_core::LinkId;

use super::ReticulumNode;
use crate::sync_ext::MutexRecover;

impl ReticulumNode {
    /// The id a link carries on the wire (CIRISVerify#309); see
    /// [`leviculum_core::node::NodeCore::wire_link_id`]. Equal to the id the
    /// application holds unless an establishment retry re-keyed an initiator
    /// link; use it for anything both ends must derive alike.
    pub fn wire_link_id(&self, link_id: &LinkId) -> Option<LinkId> {
        self.inner.lock_recover().wire_link_id(link_id)
    }
}
