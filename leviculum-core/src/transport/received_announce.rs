//! Where a received announce's entry goes in the announce table (1262).
//!
//! A child of `transport.rs`, so it reaches the table and the held slot of
//! the #170 hold without either being opened up.

use super::{AnnounceEntry, Transport, TRUNCATED_HASHBYTES};
use crate::hex_fmt::HexShort;
use crate::traits::{Clock, Storage};

impl<C: Clock, S: Storage> Transport<C, S> {
    /// Put a received announce's entry in the announce table, unless a
    /// targeted path response for the same destination is still waiting out
    /// its grace there. Writing over it, as the reference does
    /// (Transport.py:1898), loses the answer: the rebroadcast that fires
    /// instead skips the access point interfaces, so a requester on one
    /// never hears the path it asked for (`node::mvr_path_response_survives_reannounce`).
    /// So the response stays, carrying the newer emission the path table
    /// just moved to, and the new entry waits in the held slot the #170
    /// hold uses, reinserted when the response fires. A held entry from
    /// before is an older emission's and gives way.
    /// `forwards_pending_response` is the one announce meant to displace a
    /// cached answer (#171) and is written as it is.
    pub(super) fn insert_received_announce(
        &mut self,
        dest_hash: [u8; TRUNCATED_HASHBYTES],
        entry: AnnounceEntry,
        forwards_pending_response: bool,
    ) {
        let pending_response = match self.storage.get_announce(&dest_hash) {
            Some(existing)
                if !forwards_pending_response
                    && existing.target_interface.is_some()
                    && existing.retransmit_at_ms.is_some() =>
            {
                Some(existing.clone())
            }
            _ => None,
        };
        match pending_response {
            Some(mut response) => {
                response.raw_packet.clone_from(&entry.raw_packet);
                response.hops = entry.hops;
                crate::tracing::debug!(
                    event = "ANN_HELD_FOR_RESPONSE",
                    dst = %HexShort(&dest_hash),
                    iface_out = %self.iface_name(response.target_interface.unwrap_or_default()),
                );
                self.storage.set_announce(dest_hash, response);
                self.held_announce_entries.insert(dest_hash, entry);
            }
            None => self.storage.set_announce(dest_hash, entry),
        }
    }
}
