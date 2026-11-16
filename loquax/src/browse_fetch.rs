//! The Browse tab's fetch (1337 pass two, #475; the desktop's since
//! 1369): lnomad's fetch session over the node the messenger already
//! runs, lent by its engine ([`crate::engine::Lent`]), not a second node
//! beside it (`docs/src/concepts/lnomad-everywhere.md`, slice 3). The
//! phone, the terminal and the window run this one task.
//!
//! Which node that is the host decides. On the phone: the mailbox's when
//! it is on this phone, since that node is the phone's way onto the
//! network and the screens' leaf reaches nothing past it; the leaf's when
//! the mailbox is a computer's, whose rendezvous carries it on. On a
//! desktop: the node the messenger attached to its daemon. Either way the
//! node comes on a channel and may come again: the phone's mailbox lends
//! its node anew at every start, a restart after a rename included.
//!
//! The fetch is lnomad's own ([`lnomad::fetch::Session`] over a
//! [`LentNode`], [`lnomad::transfer::fetch_page`],
//! [`lnomad::transfer::fetch_file`], [`lnomad::transfer::spawn_discovery`],
//! [`lnomad::transfer::spawn_image_queue`]); what this adds is the task
//! lnomad's own loop is in the standalone browser: one fetch in flight, a
//! newer one or a cancel superseding it, a page's pictures one after
//! another, and what it brought said back as the hosted browser's events.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::engine::Lent;
use crate::ui::browse::BrowseEvent;
use leviculum_core::Identity;
use lnomad::app::strings::Text;
use lnomad::app::ImageRequest;
use lnomad::fetch::{LentNode, Session as Fetch};
use lnomad::{transfer, IdentifyStore, Target};
use tokio::sync::{mpsc, Mutex};

/// One request's budget: lnomad's own default (`--timeout 30`).
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// The file under the app's directory (Loquax's home on a desktop) that
/// keeps the nodes the reader identifies to, lnomad's `identify.toml`.
pub const IDENTIFY_FILE: &str = "browse-identify.toml";

/// What the session asks of the fetch: the hosted browser's effects that
/// reach the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// Fetch this page ([`lnomad::Effect::Navigate`]).
    Navigate(Target),
    /// Fetch this file ([`lnomad::Effect::Download`]).
    Download(Target),
    /// Identify to `dest` or stop ([`lnomad::Effect::SetIdentify`]).
    Identify { dest: [u8; 16], on: bool },
    /// Drop the fetch in flight ([`lnomad::Effect::Cancel`]).
    Cancel,
    /// Fetch the page's pictures one after another
    /// ([`lnomad::Effect::FetchImages`]); an empty list stops the last
    /// page's, as a navigation and a cancel do.
    Images(Vec<ImageRequest>),
}

/// What the fetch tells the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Told {
    /// For the hosted browser: a page, a failure, a node's announce.
    Browse(BrowseEvent),
    /// A file arrived, to be kept where the platform keeps downloads.
    File { name: String, bytes: Vec<u8> },
    /// One of the page's pictures, for the host that draws it: its
    /// transfer started, its bytes, or why not.
    Picture {
        link: usize,
        target: Target,
        shot: Shot,
    },
}

/// What the picture queue says of one picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shot {
    Started,
    Bytes(Vec<u8>),
    Failed(String),
}

/// The fetch's ends the session holds.
pub struct Fetching {
    pub asks: mpsc::UnboundedSender<Ask>,
    pub told: mpsc::UnboundedReceiver<Told>,
}

/// What the fetch needs: where the lent node comes from, the identity a
/// node is shown on identify, and where the identify decisions live.
pub struct Options {
    pub lends: mpsc::UnboundedReceiver<Lent>,
    pub identity: Identity,
    pub identify: PathBuf,
}

/// Start the fetch on `runtime`; `wake` is told whenever there is
/// something to take.
pub fn start(
    runtime: &tokio::runtime::Handle,
    options: Options,
    wake: Arc<tokio::sync::Notify>,
) -> Fetching {
    let (asks, asks_rx) = mpsc::unbounded_channel();
    let (told_tx, told) = mpsc::unbounded_channel();
    runtime.spawn(run(
        options,
        asks_rx,
        Teller {
            told: told_tx,
            wake,
        },
    ));
    Fetching { asks, told }
}

/// The session's end of [`Told`], waking its pump on every word.
struct Teller {
    told: mpsc::UnboundedSender<Told>,
    wake: Arc<tokio::sync::Notify>,
}

impl Teller {
    /// Whether the session still listens.
    fn tell(&self, told: Told) -> bool {
        let heard = self.told.send(told).is_ok();
        self.wake.notify_one();
        heard
    }
}

/// What a fetch in flight brings, with the generation that asked it.
enum Brought {
    Page(Result<transfer::Page, Text>),
    File(Result<(String, Vec<u8>), Text>),
}

/// The fetch: one lent node at a time, the newest. Ends once the session
/// does.
async fn run(options: Options, mut asks: mpsc::UnboundedReceiver<Ask>, teller: Teller) {
    let Options {
        mut lends,
        identity,
        identify,
    } = options;
    loop {
        // Before a node is lent, a page asked for has nowhere to come from.
        let lent = tokio::select! {
            lent = lends.recv() => match lent {
                Some(lent) => lent,
                None => return,
            },
            ask = asks.recv() => match ask {
                Some(Ask::Navigate(_) | Ask::Download(_)) => {
                    let not_yet = lnomad::FetchError::Connect("the phone's node is not up yet".into());
                    let event = BrowseEvent::FailedText(not_yet.text());
                    if !teller.tell(Told::Browse(event)) {
                        return;
                    }
                    continue;
                }
                Some(Ask::Identify { .. } | Ask::Cancel | Ask::Images(_)) => continue,
                None => return,
            },
        };
        let mut lent = lent;
        while let Ok(newer) = lends.try_recv() {
            lent = newer;
        }
        if !serve(lent, &identity, &identify, &mut asks, &teller).await {
            return;
        }
    }
}

/// Serve the asks over one lent node until its lend ends (`true`, wait
/// for the next) or the session does (`false`).
async fn serve(
    lent: Lent,
    identity: &Identity,
    identify: &std::path::Path,
    asks: &mut mpsc::UnboundedReceiver<Ask>,
    teller: &Teller,
) -> bool {
    let Lent {
        node,
        events,
        mut ended,
    } = lent;
    let mut fetch = Fetch::over(
        LentNode::new(node, events),
        identity.clone(),
        IdentifyStore::load(identify),
    );
    let (nodes_tx, mut nodes) = mpsc::unbounded_channel();
    fetch.set_announce_sink(nodes_tx);
    let session = Arc::new(Mutex::new(fetch));
    let discovery = transfer::spawn_discovery(&session);
    let (brought_tx, mut brought) = mpsc::unbounded_channel::<(u64, Brought)>();
    let mut inflight: Option<tokio::task::JoinHandle<()>> = None;
    let mut generation = 0u64;
    // The page asked last, for its outcome's event line.
    let mut asked_page: Option<Target> = None;
    let (shots_tx, mut shots) = mpsc::unbounded_channel::<transfer::ImageOutcome>();
    let mut pictures: Option<tokio::task::JoinHandle<()>> = None;
    let mut picture_generation = 0u64;
    let listening = loop {
        tokio::select! {
            _ = &mut ended => break true,
            Some(node) = nodes.recv() => {
                if !teller.tell(Told::Browse(BrowseEvent::Node(node))) {
                    break false;
                }
            }
            Some((asked, outcome)) = brought.recv() => {
                if asked != generation {
                    continue;
                }
                inflight = None;
                if let (Brought::Page(result), Some(target)) = (&outcome, asked_page.take()) {
                    let (state, why) = match result {
                        Ok(_) => ("ok", String::from("-")),
                        Err(text) => ("failed", text.resolve(lnomad::app::strings::Lang::En)),
                    };
                    crate::events::browse_fetch(&target.dest_hash, &target.path, state, &why);
                }
                let told = match outcome {
                    Brought::Page(Ok(page)) => vec![
                        Told::Browse(BrowseEvent::Loaded { micron: page.micron, title: page.title }),
                        Told::Browse(BrowseEvent::Identifying { dest: page.dest, on: page.identifying }),
                    ],
                    Brought::Page(Err(text)) => vec![Told::Browse(BrowseEvent::FailedText(text))],
                    Brought::File(Ok((name, bytes))) => vec![Told::File { name, bytes }],
                    Brought::File(Err(text)) => vec![Told::Browse(BrowseEvent::DownloadFailed(text))],
                };
                if !told.into_iter().all(|told| teller.tell(told)) {
                    break false;
                }
            }
            Some(shot) = shots.recv() => {
                // A stale generation belongs to a page the reader has left.
                if shot.generation != picture_generation {
                    continue;
                }
                let told = Told::Picture {
                    link: shot.link,
                    target: shot.target,
                    shot: match shot.result {
                        transfer::ImageResult::Started => Shot::Started,
                        transfer::ImageResult::Bytes(bytes) => Shot::Bytes(bytes),
                        transfer::ImageResult::Failed(why) => Shot::Failed(why),
                    },
                };
                if !teller.tell(told) {
                    break false;
                }
            }
            ask = asks.recv() => {
                let Some(ask) = ask else {
                    break false;
                };
                // A page's pictures go after the page, beside no other
                // fetch: they neither end the fetch in flight nor wait.
                if let Ask::Images(requests) = ask {
                    picture_generation = stop(&mut pictures, picture_generation);
                    if !requests.is_empty() {
                        pictures = Some(transfer::spawn_image_queue(
                            &session, &shots_tx, TIMEOUT, requests, picture_generation,
                        ));
                    }
                    continue;
                }
                if !matches!(ask, Ask::Download(_) | Ask::Identify { .. }) {
                    picture_generation = stop(&mut pictures, picture_generation);
                }
                if let Some(handle) = inflight.take() {
                    handle.abort();
                    // A page whose fetch ends here says so (1667, walk 11,
                    // finding 65: an abandoned fetch left no end line): a
                    // cancel is the reader stopping it, a Back or the
                    // browser left (1669, walk 12, finding 73), anything
                    // else another ask taking its place.
                    if let Some(target) = asked_page.take() {
                        let why = match ask {
                            Ask::Cancel => "stopped",
                            _ => "superseded",
                        };
                        crate::events::browse_fetch(&target.dest_hash, &target.path, "aborted", why);
                    }
                }
                generation = generation.wrapping_add(1);
                let asked = generation;
                match ask {
                    Ask::Navigate(target) => {
                        crate::events::browse_fetch(&target.dest_hash, &target.path, "asked", "-");
                        asked_page = Some(target.clone());
                        let (session, tx) = (Arc::clone(&session), brought_tx.clone());
                        inflight = Some(tokio::spawn(async move {
                            let page = transfer::fetch_page(&mut *session.lock().await, &target, TIMEOUT).await;
                            let _ = tx.send((asked, Brought::Page(page)));
                        }));
                    }
                    Ask::Download(target) => {
                        let (session, tx) = (Arc::clone(&session), brought_tx.clone());
                        inflight = Some(tokio::spawn(async move {
                            let file = transfer::fetch_file(&mut *session.lock().await, &target, TIMEOUT).await;
                            let _ = tx.send((asked, Brought::File(file)));
                        }));
                    }
                    Ask::Identify { dest, on } => {
                        // Stored, and the reused link dropped, before the
                        // reload that follows builds its fresh one.
                        if let Err(error) = session.lock().await.set_identify(&dest, on).await {
                            tracing::warn!("BROWSE identify not saved: {error}");
                        }
                    }
                    Ask::Cancel | Ask::Images(_) => {}
                }
            }
        }
    };
    // The lender may be stopping its node, which it can only once nothing
    // here holds it: the fetch in flight and the discovery go first.
    if let (Some(_), Some(target)) = (&inflight, asked_page.take()) {
        crate::events::browse_fetch(&target.dest_hash, &target.path, "aborted", "stopped");
    }
    for handle in [inflight.take(), pictures.take()].into_iter().flatten() {
        handle.abort();
        let _ = handle.await;
    }
    discovery.abort();
    let _ = discovery.await;
    if let Ok(session) = Arc::try_unwrap(session) {
        // Closes the session's link; the node stays the lender's.
        let _ = session.into_inner().close().await;
    }
    listening
}

/// Stop the last page's picture queue, whose pictures are nobody's now:
/// the task aborted and the generation moved on, which is returned.
fn stop(task: &mut Option<tokio::task::JoinHandle<()>>, generation: u64) -> u64 {
    if let Some(handle) = task.take() {
        handle.abort();
    }
    generation.wrapping_add(1)
}
