//! Steering a link through a chosen relay (+ciris, leviculum#91, #92); see
//! [`leviculum_core::node::NodeCore::connect_via`].

use std::sync::Arc;
use std::time::Duration;

use leviculum_core::relay_routes::RelayRoute;
use leviculum_core::DestinationHash;

use super::{dispatch_committed, LinkHandle, ReticulumNode};
use crate::error::Error;
use crate::sync_ext::MutexRecover;

impl ReticulumNode {
    /// The relays `dest` has been heard through, freshest first: the
    /// candidates [`connect_via`](Self::connect_via) can route by. Rows a
    /// live `connect_via` link uses are kept; otherwise distinct interfaces
    /// are kept before fresh rows. Takes the core lock.
    pub fn relay_routes(&self, dest: &DestinationHash) -> Vec<RelayRoute> {
        self.inner.lock_recover().relay_routes(dest)
    }

    /// Size the relay table: relays kept per destination (default 4) and
    /// destinations kept (default 1024). Either 0 turns it off. Takes the
    /// core lock.
    pub fn set_relay_route_bounds(&self, per_destination: usize, destinations: usize) {
        self.inner
            .lock_recover()
            .set_relay_route_bounds(per_destination, destinations);
    }

    /// Ask for a path to `dest` on one interface only (leviculum#92), so a
    /// single relay behind it can answer and no other interface hears the
    /// request. An answer through a relay arrives as
    /// [`NodeEvent::RelayRouteLearned`](crate::NodeEvent::RelayRouteLearned);
    /// there is no explicit "no". Not throttled: pace repeats yourself.
    /// Takes the core lock.
    pub async fn request_path_on(
        &self,
        dest: &DestinationHash,
        interface_index: usize,
    ) -> Result<(), Error> {
        dispatch_committed(&self.action_dispatch_tx(), || {
            let output = self
                .inner
                .lock_recover()
                .request_path_on(dest, interface_index);
            Ok(((), output))
        })
        .await
    }

    /// Open a link to `dest` through `relay` (leviculum#91), on the
    /// interface the relay was heard on, for every request attempt. Two
    /// links to one destination through different relays can be up at once.
    ///
    /// `establish_timeout` is the budget for this relay: one attempt that
    /// long and no retry, so a relay that silently drops the request fails
    /// in time for the caller to try the next. `None`: the hop-scaled
    /// timeout and retries, all through `relay`.
    ///
    /// # Errors
    /// [`Error::NoRouteViaRelay`] when `dest` has no live row for `relay`
    /// (see [`relay_routes`](Self::relay_routes),
    /// [`request_path_on`](Self::request_path_on)); otherwise as
    /// [`connect`](Self::connect). Takes the core lock.
    pub async fn connect_via(
        &self,
        dest: &DestinationHash,
        dest_signing_key: &[u8; 32],
        relay: [u8; 16],
        establish_timeout: Option<Duration>,
    ) -> Result<LinkHandle, Error> {
        let budget_ms =
            establish_timeout.map(|d| u32::try_from(d.as_millis()).unwrap_or(u32::MAX).max(1));
        let link_id = dispatch_committed(&self.action_dispatch_tx(), || {
            let (link_id, output) =
                self.inner
                    .lock_recover()
                    .connect_via(*dest, dest_signing_key, relay, budget_ms)?;
            Ok((link_id, output))
        })
        .await?;
        Ok(LinkHandle::new(
            link_id,
            Arc::clone(&self.inner),
            self.action_dispatch_tx(),
        ))
    }
}
