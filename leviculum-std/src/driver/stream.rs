//! Send-only async handle for Links
//!
//! Locks the core directly and dispatches actions through the event loop,
//! matching the pattern used by `PacketSender`. This ensures that
//! `send()` returns the real result from `send_on_link()`, including
//! `Busy`, instead of silently buffering through an mpsc channel.

use std::sync::{Arc, Mutex};

use crate::sync_ext::MutexRecover;

use tokio::sync::mpsc;

use leviculum_core::link::LinkId;
use leviculum_core::transport::TickOutput;
use leviculum_core::SendError;

use super::StdNodeCore;
use crate::error::Error;

/// Async handle for a Link (send-only)
///
/// `LinkHandle` provides async write operations for a link.
/// It locks the core directly for each send, ensuring the caller sees
/// the real result (including `Busy` mapped to `WouldBlock`).
///
/// Incoming data is delivered via `NodeEvent::LinkDataReceived` /
/// `NodeEvent::MessageReceived` on the node's event channel.
///
/// # Example
///
/// ```no_run
/// # use leviculum_std::driver::{ReticulumNodeBuilder, LinkHandle};
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// # let node = ReticulumNodeBuilder::new().build().await?;
/// # let dest_hash = leviculum_core::DestinationHash::new([0; 16]);
/// # let signing_key = [0u8; 32];
/// let handle = node.connect(&dest_hash, &signing_key).await?;
///
/// // Write data
/// handle.send(b"Hello!").await?;
///
/// // Read responses via node.take_event_receiver()
/// # Ok(())
/// # }
/// ```
pub struct LinkHandle {
    /// Link ID for this link
    link_id: LinkId,
    /// Shared handle to the core node
    inner: Arc<Mutex<StdNodeCore>>,
    /// Channel for dispatching actions to the event loop
    action_dispatch_tx: mpsc::Sender<TickOutput>,
    /// Whether the handle is closed
    closed: bool,
}

impl LinkHandle {
    /// Create a new LinkHandle
    pub(crate) fn new(
        link_id: LinkId,
        inner: Arc<Mutex<StdNodeCore>>,
        action_dispatch_tx: mpsc::Sender<TickOutput>,
    ) -> Self {
        Self {
            link_id,
            inner,
            action_dispatch_tx,
            closed: false,
        }
    }

    /// Get the link ID
    pub fn link_id(&self) -> &LinkId {
        &self.link_id
    }

    /// Check if the handle is closed
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Try to send data on this link (non-blocking)
    ///
    /// Locks the core, calls `send_on_link()`, and dispatches the
    /// resulting actions. Returns `SendError::Busy` if the send path is
    /// occupied, or `SendError::PacingDelay` if the channel is pacing.
    pub async fn try_send(&self, data: &[u8]) -> Result<(), Error> {
        if self.closed {
            return Err(Error::NotRunning);
        }

        super::dispatch_committed(&self.action_dispatch_tx, || {
            let mut core = self.inner.lock_recover();
            Ok(((), core.send_on_link(&self.link_id, data)?))
        })
        .await
    }

    /// Send data on this link, absorbing pacing delays and busy conditions
    ///
    /// Unlike `try_send()`, this method retries automatically when the channel
    /// reports a pacing delay or busy condition, sleeping until ready.
    /// Only returns an error on fatal conditions (link lost, handle closed).
    pub async fn send(&self, data: &[u8]) -> Result<(), Error> {
        loop {
            if self.closed {
                return Err(Error::NotRunning);
            }

            let result = super::dispatch_committed(&self.action_dispatch_tx, || {
                let mut core = self.inner.lock_recover();
                Ok(((), core.send_on_link(&self.link_id, data)?))
            })
            .await;

            match result {
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
                Err(other) => {
                    return Err(other);
                }
            }
        }
    }

    /// Close the link gracefully, sending LINKCLOSE to the peer
    pub async fn close(&mut self) -> Result<(), Error> {
        if self.closed {
            return Ok(());
        }
        // Marked closed only once the call returns: an abort while it waits
        // for room in the dispatch channel leaves the link open in the core,
        // and the handle has to agree.
        let result = super::dispatch_committed(&self.action_dispatch_tx, || {
            let mut core = self.inner.lock_recover();
            Ok(((), core.close_link(&self.link_id)))
        })
        .await;
        self.closed = true;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::ReticulumNodeBuilder;

    // TempDir is the fourth tuple element; callers must bind it to a
    // live variable so RAII drop doesn't delete the storage directory
    // out from under the node.
    fn make_node_and_inner() -> (
        Arc<Mutex<StdNodeCore>>,
        mpsc::Sender<TickOutput>,
        mpsc::Receiver<TickOutput>,
        tempfile::TempDir,
    ) {
        let td = tempfile::tempdir().expect("tempdir");
        let node = ReticulumNodeBuilder::new()
            .storage_path(td.path().to_path_buf())
            .build_sync()
            .expect("build_sync");
        let inner = node.inner();
        let (tx, rx) = mpsc::channel(16);
        (inner, tx, rx, td)
    }

    #[tokio::test]
    async fn test_link_handle_send_no_link() {
        let (inner, tx, _rx, _td) = make_node_and_inner();
        let link_id = LinkId::from([0u8; 16]);

        let handle = LinkHandle::new(link_id, inner, tx);

        // Sending on a non-existent link should return Send(NoLink)
        let result = handle.try_send(b"hello").await;
        assert!(result.is_err());
        assert!(
            matches!(result, Err(Error::Send(SendError::NoLink))),
            "expected Send(NoLink), got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_link_handle_close() {
        let (inner, tx, _rx, _td) = make_node_and_inner();
        let link_id = LinkId::from([0u8; 16]);

        let mut handle = LinkHandle::new(link_id, inner, tx);

        assert!(!handle.is_closed());
        // close_link on a non-existent link is harmless (returns empty output)
        handle.close().await.unwrap();
        assert!(handle.is_closed());

        // Send should fail after close
        let result = handle.try_send(b"test").await;
        assert!(result.is_err());
        assert!(matches!(result, Err(Error::NotRunning)));
    }

    #[tokio::test]
    async fn test_link_handle_close_idempotent() {
        let (inner, tx, _rx, _td) = make_node_and_inner();
        let link_id = LinkId::from([0u8; 16]);

        let mut handle = LinkHandle::new(link_id, inner, tx);

        handle.close().await.unwrap();
        // Second close is a no-op
        handle.close().await.unwrap();
        assert!(handle.is_closed());
    }

    /// L-0059: a caller aborted while it waits on a full dispatch channel
    /// must leave core and channel consistent. Either the abort lands before
    /// the core moved (link still there, nothing enqueued) or after the
    /// enqueue (link gone, its close output in the channel). Committing the
    /// close and then losing the output is the defect: the link is removed
    /// and LinkClosed plus the LINKCLOSE bytes never reach the event loop.
    #[tokio::test(flavor = "current_thread")]
    async fn test_link_handle_close_aborted_on_full_channel_loses_nothing() {
        let (inner, _, _, _td) = make_node_and_inner();
        let identity = leviculum_core::Identity::generate(&mut rand_core::OsRng);
        let signing_key = identity.ed25519_verifying().to_bytes();
        let dest_hash = leviculum_core::DestinationHash::new([0x42; 16]);
        let (link_id, _, connect_output) = inner
            .lock_recover()
            .connect(dest_hash, &signing_key)
            .expect("connect");
        drop(connect_output);
        assert_eq!(inner.lock_recover().link_count(), 1);

        // A dispatch channel that is already full: the event loop is stalled.
        let (tx, mut rx) = mpsc::channel::<TickOutput>(1);
        tx.try_send(TickOutput::empty()).expect("fill the one slot");

        let mut handle = LinkHandle::new(link_id, Arc::clone(&inner), tx);
        let task = tokio::spawn(async move { handle.close().await });
        // Let the task run until it parks on the full channel.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(
            !task.is_finished(),
            "close must be parked on the full channel"
        );
        task.abort();
        let _ = task.await;

        let link_count = inner.lock_recover().link_count();
        let mut queued = 0;
        while rx.try_recv().is_ok() {
            queued += 1;
        }
        // One output is the filler. A second one is the close, if it was
        // enqueued at all.
        let close_enqueued = queued == 2;
        assert!(
            (link_count == 1 && !close_enqueued) || (link_count == 0 && close_enqueued),
            "core and dispatch channel disagree: link_count={link_count}, outputs queued={queued}"
        );
    }

    #[tokio::test]
    async fn test_link_handle_send_closed_dispatch_channel() {
        let (inner, _, _, _td) = make_node_and_inner();
        let link_id = LinkId::from([0u8; 16]);

        // Create with a channel whose receiver is dropped
        let (tx, rx) = mpsc::channel::<TickOutput>(1);
        drop(rx);

        let handle = LinkHandle::new(link_id, inner, tx);

        // Send fails because there's no link, so dispatch channel
        // closure is not reached, but Send(NoLink) is returned first
        let result = handle.try_send(b"hello").await;
        assert!(result.is_err());
    }
}

// +ciris (leviculum#94): frames with a deadline, in a child module so no
// cited line here moves.
mod timed;

// +ciris (leviculum#102): unordered datagrams, in a child module for the
// same reason.
mod datagram;
