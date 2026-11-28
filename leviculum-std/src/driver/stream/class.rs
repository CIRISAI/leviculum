//! Reliable link sends with an egress class (+ciris, egress scheduler; see
//! `docs/egress-scheduler.md`).

use super::LinkHandle;
use crate::egress::EgressClass;
use crate::error::Error;
use crate::sync_ext::MutexRecover;
use leviculum_core::SendError;

/// Note `class` for the Channel frames `output` carries on `link` (its wire
/// id), before the event loop dispatches them.
fn hint_channel_frames(
    egress: Option<&crate::egress::EgressScheduler>,
    output: &leviculum_core::TickOutput,
    link: &[u8; 16],
    class: EgressClass,
) {
    let Some(egress) = egress else {
        return;
    };
    for action in &output.actions {
        if let leviculum_core::Action::SendPacket { data, .. } = action {
            let hint = leviculum_core::egress::classify_frame(data);
            if hint.class == leviculum_core::egress::FrameClass::Control && &hint.dst == link {
                egress.hint_class(hint.fingerprint, class);
            }
        }
    }
}

impl LinkHandle {
    /// [`Self::try_send`], scheduled as `class` (+ciris, egress scheduler):
    /// the Channel frame this send puts on the wire waits and goes as
    /// `class` instead of Control, so one link can carry both urgent control
    /// and bulk replication. Its retransmits, which are encrypted afresh,
    /// go as Control. A Channel frame is never dropped by age, whatever its
    /// class. Takes the core lock, as `try_send` does.
    pub async fn try_send_as(&self, data: &[u8], class: EgressClass) -> Result<(), Error> {
        if self.closed {
            return Err(Error::NotRunning);
        }
        crate::driver::dispatch_committed(&self.action_dispatch_tx, || {
            let mut core = self.inner.lock_recover();
            let wire = core.wire_link_id(&self.link_id).unwrap_or(self.link_id);
            let output = core.send_on_link(&self.link_id, data)?;
            hint_channel_frames(self.egress.as_deref(), &output, wire.as_bytes(), class);
            Ok(((), output))
        })
        .await
    }

    /// [`Self::send`] (pacing delays and busy absorbed), scheduled as
    /// `class`; see [`Self::try_send_as`].
    pub async fn send_as(&self, data: &[u8], class: EgressClass) -> Result<(), Error> {
        loop {
            match self.try_send_as(data, class).await {
                Ok(()) => return Ok(()),
                Err(Error::Send(SendError::PacingDelay { ready_at_ms })) => {
                    let now_ms = self.inner.lock_recover().now_ms();
                    let delay = ready_at_ms.saturating_sub(now_ms);
                    if delay > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    }
                }
                Err(Error::Send(SendError::Busy)) => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(other) => return Err(other),
            }
        }
    }
}
