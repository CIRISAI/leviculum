//! A client's watching link (`docs/src/concepts/loquax-home-node.md`, "The
//! wake"): one link to `loquax.home`, kept while the client runs, that
//! asked `/watch` and hears a wake whenever the home node's archive grew.
//! The frontend pulls on a wake past its cursor ([`wants_pull`]); the pull
//! itself is the `/sync` it always was, on a link of its own.
//!
//! The link is opened again with [`Backoff`] when it drops or never came
//! up. While it is down the frontend falls back to pulling ([`Fallback`]),
//! besides its open, its Refresh and its schedule. A home node from before
//! `/watch` answers nothing: the request runs out of time, the backoff
//! grows to [`Backoff::MAX`], and the client pulls as it always did.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use leviculum_core::node::NodeEvent;
use leviculum_core::{Identity, LinkId};
use leviculum_std::driver::TapEvent;
use leviculum_std::ReticulumNode;
use tokio::sync::mpsc::UnboundedSender;

use super::client::{self, HomeError};
use super::wire;

/// What the watching link tells its frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchEvent {
    /// The link is up and watching, or it dropped.
    Watching(bool),
    /// The archive's newest sequence is this: pull when it is past the
    /// cursor. Said once on every `/watch` answer too, so entries that came
    /// while the link was down are pulled at once. `u64::MAX` when wakes
    /// may have been missed (the node's event tap fell behind).
    Wake(u64),
}

/// Whether a wake with `newest` calls for a pull from `cursor`.
pub fn wants_pull(newest: u64, cursor: u64) -> bool {
    newest > cursor
}

/// How long one attempt to open the link and ask `/watch` may take.
const ATTEMPT: Duration = Duration::from_secs(30);
/// The longest the link goes unlooked at besides its events. The watch is
/// event-driven (1176): a closed link says so with `LinkClosed`, a tap that
/// lost events says so with `Lagged` and the link is looked at then, and a
/// frontend that went away closes the channel. This look is only the
/// backstop for a close no event reported, and it is the link keepalive's
/// ceiling (`KEEPALIVE_MAX`, Link.py:92; `LINK_KEEPALIVE_SECS`), so on a
/// phone it never wakes more often than the link's own keepalive does. It
/// was 5 s before 1176, a wakeup every 5 s for the life of the app.
pub const LOOK: Duration = Duration::from_secs(leviculum_core::constants::LINK_KEEPALIVE_SECS);

/// The wait before the next attempt to open the watching link: doubled on
/// each attempt that did not come up, back to the first once a link stood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backoff {
    next: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self { next: Self::FIRST }
    }
}

impl Backoff {
    /// The first wait: a dropped link is back within seconds.
    pub const FIRST: Duration = Duration::from_secs(2);
    /// The longest: five minutes, so a home node that does not know
    /// `/watch`, or is away for the night, costs one link attempt per five
    /// minutes and no more.
    pub const MAX: Duration = Duration::from_secs(300);

    /// An attempt failed: the wait before the next, and the one after it
    /// doubled.
    pub fn failed(&mut self) -> Duration {
        let wait = self.next;
        self.next = (self.next * 2).min(Self::MAX);
        wait
    }

    /// A link stood and dropped: the next attempt comes after the first
    /// wait again.
    pub fn held(&mut self) -> Duration {
        self.next = Self::FIRST;
        self.failed()
    }
}

/// The pull every [`Fallback::EVERY`] while the app is in front and the
/// watching link is down: the user looks at the screen and has no wake to
/// rely on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fallback {
    /// Since when the link was down with the app in front, or the last
    /// fallback pull.
    since: Option<u64>,
}

impl Fallback {
    pub const EVERY: Duration = Duration::from_secs(60);

    /// Whether a fallback pull is due at `now_ms`.
    pub fn due(&mut self, now_ms: u64, watching: bool, front: bool) -> bool {
        if watching || !front {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(now_ms);
        let every = u64::try_from(Self::EVERY.as_millis()).unwrap_or(u64::MAX);
        if now_ms.saturating_sub(since) < every {
            return false;
        }
        self.since = Some(now_ms);
        true
    }
}

/// What the watching link needs: the node, the home identity's key, the
/// device's own identity, and the home directory its cursor is kept in.
#[derive(Clone)]
pub struct Watch {
    pub node: Arc<ReticulumNode>,
    pub public_key: [u8; 64],
    pub device: Identity,
    pub home: PathBuf,
}

/// Keep the watching link until `events` is dropped: open it, ask
/// `/watch`, hand on every wake, and open it again after the backoff when
/// it drops.
pub async fn run(watch: Watch, events: UnboundedSender<WatchEvent>) {
    let mut backoff = Backoff::default();
    let mut lengthen = super::keepalive::Lengthen::default();
    loop {
        let wait = match keep(&watch, &events, lengthen.on()).await {
            Ok(held) => {
                if lengthen.ended(held.as_secs()) {
                    tracing::warn!(
                        "HOME watch: the mailbox dropped {} links in a row before their \
                         first keepalive; the next ones keep the reference's interval",
                        super::keepalive::SHORT_HOLDS
                    );
                }
                backoff.held()
            }
            Err(error) => {
                tracing::info!("HOME watch: {error}");
                backoff.failed()
            }
        };
        if events.is_closed() {
            return;
        }
        tracing::info!("HOME watch again in {}s", wait.as_secs());
        tokio::time::sleep(wait).await;
    }
}

/// One watching link, from its opening to its end: `Ok` with how long it
/// stood once it dropped, the error when it never came up. `long` gives
/// the link the device link's keepalive ([`super::keepalive`], 1346): the
/// phone is the initiator, the side that sends them, and the mailbox
/// takes the same interval for the link once it is asked on it.
async fn keep(
    watch: &Watch,
    events: &UnboundedSender<WatchEvent>,
    long: bool,
) -> Result<Duration, HomeError> {
    // Subscribed before the request, so a wake right behind the answer is
    // heard.
    let mut tap = watch.node.subscribe_events();
    let deadline = tokio::time::Instant::now() + ATTEMPT;
    let link = client::open(&watch.node, &watch.public_key, &watch.device, deadline).await?;
    if long {
        watch
            .node
            .set_link_keepalive(&link, super::keepalive::LINK_KEEPALIVE_SECS);
    }
    let cursor = client::load_cursor(&watch.home);
    let answer = client::request(
        &watch.node,
        &link,
        super::WATCH_PATH,
        &wire::watch_request(cursor),
        deadline,
    )
    .await
    .and_then(|bytes| {
        wire::read_watch_answer(&bytes)
            .map_err(|_| HomeError::Malformed)?
            .map_err(HomeError::Refused)
    });
    let newest = match answer {
        Ok(newest) => newest,
        Err(error) => {
            let _ = watch.node.close_link(&link).await;
            return Err(error);
        }
    };
    tracing::info!("HOME watching cursor={cursor} newest={newest}");
    let since = tokio::time::Instant::now();
    let _ = events.send(WatchEvent::Watching(true));
    let _ = events.send(WatchEvent::Wake(newest));
    listen(watch, &link, &mut tap, events).await;
    let _ = events.send(WatchEvent::Watching(false));
    let held = since.elapsed();
    tracing::info!("HOME watch link dropped after {}s", held.as_secs());
    Ok(held)
}

/// Hand on the wakes on `link` until it closes or nobody listens.
async fn listen(
    watch: &Watch,
    link: &LinkId,
    tap: &mut leviculum_std::driver::EventTap,
    events: &UnboundedSender<WatchEvent>,
) {
    // The first tick of an interval is at once; the link was just checked.
    let mut look = tokio::time::interval_at(tokio::time::Instant::now() + LOOK, LOOK);
    let mut carrier = leviculum_std::interfaces::carrier_changes();
    loop {
        tokio::select! {
            // A TCP client that lost its connection tells the core nothing
            // (it redials on its own): its carrier says it.
            Ok(()) = carrier.changed() => {
                if !carried(&watch.node) {
                    tracing::info!("HOME watch: no connection left under the link");
                    let _ = watch.node.close_link(link).await;
                    return;
                }
            }
            event = tap.recv() => match event {
                Some(TapEvent::Event(NodeEvent::MessageReceived { link_id, data, .. }))
                    if link_id == *link =>
                {
                    match wire::read_wake(&data) {
                        Ok(newest) => {
                            tracing::info!("HOME wake newest={newest}");
                            let _ = events.send(WatchEvent::Wake(newest));
                        }
                        Err(_) => tracing::info!("HOME watch: {} bytes that are no wake", data.len()),
                    }
                }
                Some(TapEvent::Event(NodeEvent::LinkClosed { link_id, .. })) if link_id == *link => {
                    return;
                }
                // The connection under the link is gone and none is left
                // (1346). A dropped connection keeps its links, in case it
                // comes back under the same index, and this one would wait
                // for its stale timeout, twice the long keepalive, before
                // the device said it is not watching. Closed here, it says
                // so when the connection does, and the next one comes up on
                // whatever connection the rotation finds. The carrier arm
                // above hears a TCP client's loss, this one a removal.
                Some(TapEvent::Event(NodeEvent::InterfaceDown(_))) if !carried(&watch.node) => {
                    tracing::info!("HOME watch: no connection left under the link");
                    let _ = watch.node.close_link(link).await;
                    return;
                }
                Some(TapEvent::Lagged(lost)) => {
                    tracing::info!("HOME watch lost {lost} node events; pulling");
                    let _ = events.send(WatchEvent::Wake(u64::MAX));
                    // The lost events may have held this link's close.
                    if !watch.node.link_is_established(link) {
                        return;
                    }
                }
                Some(_) => {}
                None => return,
            },
            () = events.closed() => {
                let _ = watch.node.close_link(link).await;
                return;
            }
            _ = look.tick() => {
                if !watch.node.link_is_established(link) {
                    return;
                }
            }
        }
    }
}

/// Whether the node has a connection that is up besides a local client's.
fn carried(node: &ReticulumNode) -> bool {
    node.interface_stats()
        .iter()
        .any(|interface| !interface.is_local_client && interface.online)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The watch's own timer is no faster than the link keepalive's
    /// ceiling (1176): the link's events carry everything else.
    #[test]
    fn the_watch_looks_no_more_often_than_the_link_keepalive() {
        assert!(LOOK >= Duration::from_secs(leviculum_core::constants::LINK_KEEPALIVE_SECS));
    }

    #[test]
    fn a_wake_pulls_only_past_the_cursor() {
        assert!(!wants_pull(0, 0));
        assert!(!wants_pull(7, 7));
        assert!(!wants_pull(6, 7), "an older home answer pulls nothing");
        assert!(wants_pull(8, 7));
        assert!(wants_pull(u64::MAX, 7), "a lost wake pulls");
    }

    /// The link is opened again after 2, 4, 8 ... seconds up to five
    /// minutes while it does not come up, and after 2 s again once one
    /// stood and dropped.
    #[test]
    fn the_link_is_opened_again_with_a_doubling_wait_that_a_standing_link_resets() {
        let mut backoff = Backoff::default();
        let waits: Vec<u64> = (0..10).map(|_| backoff.failed().as_secs()).collect();
        assert_eq!(waits, [2, 4, 8, 16, 32, 64, 128, 256, 300, 300]);
        assert_eq!(backoff.held(), Backoff::FIRST);
        assert_eq!(backoff.failed().as_secs(), 4);
    }

    #[test]
    fn the_fallback_pulls_every_minute_only_in_front_with_the_link_down() {
        let mut fallback = Fallback::default();
        // Watching, or in the back: never.
        assert!(!fallback.due(0, true, true));
        assert!(!fallback.due(120_000, false, false));
        // In front, the link down: a minute after it went down, then every
        // minute.
        assert!(!fallback.due(200_000, false, true));
        assert!(!fallback.due(259_999, false, true));
        assert!(fallback.due(260_000, false, true));
        assert!(!fallback.due(300_000, false, true));
        assert!(fallback.due(320_000, false, true));
        // The link back: the count starts over the next time it drops.
        assert!(!fallback.due(330_000, true, true));
        assert!(!fallback.due(340_000, false, true));
        assert!(!fallback.due(399_999, false, true));
        assert!(fallback.due(400_000, false, true));
    }
}
