//! The radios' goodbye at shutdown (1567, #471).
//!
//! An RNode interface whose event loop goes away writes the firmware its
//! goodbye, radio off and leave (`interfaces::rnode::send_goodbye`). That
//! write runs on the interface's own task, and the node tears its runtime
//! down right after the event loop has exited: measured on the rig
//! (2026-11-08, four RNodes, SIGTERM), the goodbye reached two of the four
//! ports before the process ended, and the other two boards kept their radios
//! on and talking into the next start's probe.
//!
//! [`RadioGoodbyes`] is the node's count of interface tasks that still owe
//! a goodbye. Each RNode task holds a [`GoodbyeToken`] for its whole life and
//! drops it when it ends, after the goodbye; `stop()` waits for the count to
//! reach zero, at most [`GOODBYE_BOUND`], before the runtime goes.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How long a stopping node waits for its radios' goodbyes. A goodbye is one
/// 8-byte write and a close: the rig's four took 1 to 4 ms each (run3
/// strace, 2026-11-08), serialised on one worker. Two seconds is that many
/// hundred times over, and still short beside a systemd stop timeout.
pub(crate) const GOODBYE_BOUND: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Inner {
    live: AtomicUsize,
    done: tokio::sync::Notify,
}

/// The node's interface tasks that still owe their radio a goodbye.
#[derive(Clone, Default)]
pub(crate) struct RadioGoodbyes {
    inner: Arc<Inner>,
}

/// One task's debt: counted from [`RadioGoodbyes::token`] until dropped.
/// Only the radio interfaces of the `serial` feature take one; a build
/// without it waits on nothing.
#[cfg_attr(not(feature = "serial"), allow(dead_code))]
pub(crate) struct GoodbyeToken {
    inner: Arc<Inner>,
}

impl Drop for GoodbyeToken {
    fn drop(&mut self) {
        if self.inner.live.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.inner.done.notify_waiters();
        }
    }
}

impl RadioGoodbyes {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A token for one interface task; hold it until the task ends.
    #[cfg_attr(not(feature = "serial"), allow(dead_code))]
    pub(crate) fn token(&self) -> GoodbyeToken {
        self.inner.live.fetch_add(1, Ordering::AcqRel);
        GoodbyeToken {
            inner: Arc::clone(&self.inner),
        }
    }

    /// How many tasks still hold a token.
    pub(crate) fn outstanding(&self) -> usize {
        self.inner.live.load(Ordering::Acquire)
    }

    /// Wait until every token is dropped, at most `bound`. `Err` carries how
    /// many were still out when the bound ran out.
    pub(crate) async fn wait(&self, bound: Duration) -> Result<(), usize> {
        let all_gone = async {
            loop {
                let notified = self.inner.done.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.outstanding() == 0 {
                    return;
                }
                notified.await;
            }
        };
        tokio::time::timeout(bound, all_gone)
            .await
            .map_err(|_| self.outstanding())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn nothing_owed_returns_at_once() {
        let g = RadioGoodbyes::new();
        assert_eq!(g.wait(Duration::from_millis(1)).await, Ok(()));
    }

    #[tokio::test]
    async fn the_wait_ends_when_the_last_token_goes() {
        let g = RadioGoodbyes::new();
        let tokens: Vec<_> = (0..4).map(|_| g.token()).collect();
        assert_eq!(g.outstanding(), 4);
        let dropper = tokio::spawn(async move {
            for t in tokens {
                tokio::task::yield_now().await;
                drop(t);
            }
        });
        assert_eq!(g.wait(Duration::from_secs(5)).await, Ok(()));
        dropper.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_token_never_dropped_costs_the_bound_and_is_counted() {
        let g = RadioGoodbyes::new();
        let _held = g.token();
        drop(g.token());
        assert_eq!(g.wait(Duration::from_millis(50)).await, Err(1));
    }
}
