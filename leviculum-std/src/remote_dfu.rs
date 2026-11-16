//! The `/dfu` sender: ask a board, over the mesh, to enter its bootloader's
//! BLE DFU mode (OTA stage 1, Codeberg #235,
//! `docs/src/concepts/ota-stage-1-ble-dfu.md`).
//!
//! The board's side is `leviculum_core::node::remote_dfu`: `/dfu` on the
//! `rnstransport.remote.management` destination, under the same allow-list
//! as `/status`, answered with a one-byte typed verdict
//! ([`DfuVerdict`]). This side is the same request flow `rnstatus -R` runs
//! ([`request_on_management`]) with that verdict read back, so `lnmgmt` and,
//! later, an "update this board" button in the `lnflash` GUI call one
//! function.

use std::time::Duration;

pub use leviculum_core::node::remote_dfu::{DfuVerdict, DFU_PATH};

use crate::driver::{EventReceiver, ReticulumNode};
use crate::remote_status::{request_on_management, ManagementError, ManagementReply};
use crate::Identity;

/// How a `/dfu` request ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DfuOutcome {
    /// The board answered with its verdict.
    Verdict(DfuVerdict),
    /// Nothing came back within the budget: no path, no reply. The board
    /// always answers a `/dfu` it received, so this is never a refusal.
    TimedOut(ManagementError),
    /// The request failed some other way, worded.
    Failed(String),
}

/// Read the board's reply. The verdict is one msgpack fixint in a RESPONSE
/// packet; a Resource, or any other body, is not a reply this verb has.
pub fn verdict_from_reply(reply: &ManagementReply) -> Result<DfuVerdict, String> {
    match reply {
        ManagementReply::Packet(data) => DfuVerdict::from_reply(data)
            .ok_or_else(|| format!("unreadable /dfu reply: {} byte(s) {data:02x?}", data.len())),
        ManagementReply::Resource(data) => Err(format!(
            "unexpected /dfu reply: a resource of {} byte(s)",
            data.len()
        )),
    }
}

/// Send `/dfu` to the transport instance whose identity hash is
/// `identity_hash`, identified as `identity`, and return what came of it.
///
/// `node` must already be started and `events` must be its event receiver,
/// as for [`crate::remote_status::fetch_remote_status`].
pub async fn send_dfu(
    node: &ReticulumNode,
    events: &mut EventReceiver,
    identity_hash: &[u8; 16],
    identity: &Identity,
    timeout: Duration,
    quiet: bool,
) -> DfuOutcome {
    match request_on_management(
        node,
        events,
        identity_hash,
        identity,
        DFU_PATH,
        None,
        timeout,
        quiet,
    )
    .await
    {
        Ok(reply) => match verdict_from_reply(&reply) {
            Ok(verdict) => DfuOutcome::Verdict(verdict),
            Err(message) => DfuOutcome::Failed(message),
        },
        Err(error) if error.is_timeout() => DfuOutcome::TimedOut(error),
        Err(ManagementError::Failed(message)) => DfuOutcome::Failed(message),
        Err(ManagementError::TransferFailed) => {
            DfuOutcome::Failed("the reply transfer failed".to_string())
        }
        Err(other) => DfuOutcome::Failed(format!("{other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_wire_verdict_is_read_from_a_packet() {
        for verdict in [
            DfuVerdict::Accepted,
            DfuVerdict::NotListed,
            DfuVerdict::NoIdentity,
        ] {
            let reply = ManagementReply::Packet(verdict.reply().to_vec());
            assert_eq!(verdict_from_reply(&reply), Ok(verdict));
        }
    }

    #[test]
    fn an_unknown_code_or_a_resource_is_not_a_verdict() {
        assert!(verdict_from_reply(&ManagementReply::Packet(vec![3])).is_err());
        assert!(verdict_from_reply(&ManagementReply::Packet(Vec::new())).is_err());
        assert!(verdict_from_reply(&ManagementReply::Resource(vec![0])).is_err());
    }
}
