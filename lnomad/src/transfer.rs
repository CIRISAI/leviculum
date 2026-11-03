//! The page transfers a frontend runs (the terminal's loop, the window's
//! driver): a fetch and a download as background tasks tagged with a
//! generation, a page's inline pictures one after another, saving and
//! opening a fetched picture, the continuous node discovery over the shared
//! session, and closing the session at teardown. No terminal and no window in here; each
//! frontend folds the outcomes into the model as its events.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use leviculum_micron::MicronDocument;
use tokio::sync::{mpsc, Mutex};

use crate::app::strings::{with, with_args, Arg, StrId, Text as StrText};
use crate::app::{ImageRequest, Model, ToastKind};
use crate::browser;
use crate::fetch::{NodeLend, Session};
use crate::url::Target;

/// The result of a background fetch, tagged with the generation that issued it
/// so a superseded or cancelled fetch's late result can be ignored.
pub struct FetchOutcome {
    pub generation: u64,
    pub result: LoadResult,
}

/// A fetch's outcome: a parsed page and its title, or an error message. A
/// success also carries the session's authoritative identify answer for the
/// fetched destination (read under the same lock as the fetch), so the loop can
/// refresh the model's display flag after every fresh load.
pub enum LoadResult {
    Ok {
        doc: MicronDocument,
        title: String,
        dest: [u8; 16],
        identifying: bool,
    },
    Err(StrText),
    /// A file download completed and was saved to disk.
    Downloaded {
        name: String,
        bytes: usize,
        dir: String,
    },
    /// A file download (or the save to disk) failed.
    DownloadErr(StrText),
}

/// Start (or restart) a background fetch for `target`. Aborts any in-flight
/// task, bumps the generation, and spawns a task that locks the session, fetches
/// and parses the page, resolves the title, and reports the tagged outcome.
pub fn spawn_fetch<N: NodeLend + 'static>(
    inflight: &mut Option<tokio::task::JoinHandle<()>>,
    generation: &mut u64,
    session: &Arc<Mutex<Session<N>>>,
    tx: &mpsc::UnboundedSender<FetchOutcome>,
    timeout: Duration,
    target: Target,
) {
    if let Some(handle) = inflight.take() {
        handle.abort();
    }
    *generation = generation.wrapping_add(1);
    let generation_now = *generation;
    let session = session.clone();
    let tx = tx.clone();
    let handle = tokio::spawn(async move {
        let mut guard = session.lock().await;
        let result = match fetch_page(&mut guard, &target, timeout).await {
            Ok(page) => LoadResult::Ok {
                doc: leviculum_micron::parse(&page.micron),
                title: page.title,
                dest: page.dest,
                identifying: page.identifying,
            },
            Err(text) => LoadResult::Err(text),
        };
        let _ = tx.send(FetchOutcome {
            generation: generation_now,
            result,
        });
    });
    *inflight = Some(handle);
}

/// A page a fetch brought: its micron, decoded as UTF-8 lossily so a page
/// with stray bytes still renders, the title the frame shows, and the
/// session's identify answer for its node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub micron: String,
    pub title: String,
    pub dest: [u8; 16],
    pub identifying: bool,
}

/// Fetch `target`'s page over `session`: what every frontend's fetch is,
/// the terminal's and the window's task ([`spawn_fetch`]) and a hosting
/// messenger's (Loquax's Browse tab), whose core parses the micron itself.
/// The identify answer is read under the same borrow as the fetch, so it
/// is the one the page was fetched under.
pub async fn fetch_page<N: NodeLend>(
    session: &mut Session<N>,
    target: &Target,
    timeout: Duration,
) -> Result<Page, StrText> {
    let bytes = session.fetch(target, timeout).await.map_err(|e| e.text())?;
    let name = session.node_name(&target.dest_hash);
    Ok(Page {
        micron: String::from_utf8_lossy(&bytes).into_owned(),
        title: browser::page_title(name.as_deref(), &target.dest_hash, &target.path),
        dest: target.dest_hash,
        identifying: session.is_identifying(&target.dest_hash),
    })
}

/// Fetch `target`'s file over `session`: its bytes and the name to save it
/// under ([`crate::download::choose_name`]: the server's, else the path's).
/// The terminal saves it into the download directory ([`spawn_download`]);
/// a phone hands it to the platform.
pub async fn fetch_file<N: NodeLend>(
    session: &mut Session<N>,
    target: &Target,
    timeout: Duration,
) -> Result<(String, Vec<u8>), StrText> {
    let (bytes, server_name) = session
        .download_file(target, timeout)
        .await
        .map_err(|err| StrText::Raw(err.to_string()))?;
    let name = crate::download::choose_name(server_name.as_deref(), &target.path);
    Ok((name, bytes))
}

/// Start a background file download for `target`, sharing the in-flight slot
/// and generation counter with fetches: starting either supersedes the other.
/// The spawned task fetches the raw bytes over the session, saves them into
/// the download directory (`$XDG_DOWNLOAD_DIR`, else `$HOME/Downloads`,
/// created if missing, existing names de-duplicated), and reports the outcome.
pub fn spawn_download<N: NodeLend + 'static>(
    inflight: &mut Option<tokio::task::JoinHandle<()>>,
    generation: &mut u64,
    session: &Arc<Mutex<Session<N>>>,
    tx: &mpsc::UnboundedSender<FetchOutcome>,
    timeout: Duration,
    target: Target,
) {
    if let Some(handle) = inflight.take() {
        handle.abort();
    }
    *generation = generation.wrapping_add(1);
    let generation_now = *generation;
    let session = session.clone();
    let tx = tx.clone();
    let handle = tokio::spawn(async move {
        let mut guard = session.lock().await;
        let fetched = fetch_file(&mut guard, &target, timeout).await;
        drop(guard);
        let result = match fetched {
            Ok((name, bytes)) => match crate::download::download_dir() {
                Some(dir) => match crate::download::save_to_dir(&dir, &name, &bytes) {
                    Ok(path) => LoadResult::Downloaded {
                        name: path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or(name),
                        bytes: bytes.len(),
                        dir: dir.to_string_lossy().into_owned(),
                    },
                    Err(err) => LoadResult::DownloadErr(with_args(
                        StrId::CouldNotSave,
                        vec![Arg::Str(name), Arg::Str(err.to_string())],
                    )),
                },
                None => LoadResult::DownloadErr(StrText::key(StrId::NoDownloadDir)),
            },
            Err(text) => LoadResult::DownloadErr(text),
        };
        let _ = tx.send(FetchOutcome {
            generation: generation_now,
            result,
        });
    });
    *inflight = Some(handle);
}

/// How long the background discovery drain parks on the shared session before
/// yielding the lock. A fetch that wants the session waits at most this slice,
/// while an idle mesh still has its announces drained continuously. Kept small
/// relative to a page fetch so the hand-off is imperceptible.
const DISCOVERY_LOCK_SLICE: Duration = Duration::from_millis(250);

/// Spawn the continuous NomadNet node-discovery drain.
///
/// The single shared-instance event stream carries announces alongside fetch
/// responses and has one consumer (whoever holds the session), so discovery
/// cannot own a second stream. Instead this task drives the same session in
/// short slices: it locks the session, parks for the next node announce, then
/// releases so a queued navigation runs next (the session mutex is fair). Any
/// node announce it drains — or that a concurrent fetch drains — is forwarded to
/// the announce sink wired to the loop's `node_rx`, surfacing as an
/// [`crate::AppEvent::NodeDiscovered`]. So the registry keeps filling whether or not a
/// fetch is in flight, and there is never a second reader racing the stream.
///
/// The returned handle is aborted at teardown (mirroring the in-flight fetch),
/// dropping this task's session clone so the session can be unwrapped and closed.
pub fn spawn_discovery<N: NodeLend + 'static>(
    session: &Arc<Mutex<Session<N>>>,
) -> tokio::task::JoinHandle<()> {
    let session = session.clone();
    tokio::spawn(async move {
        loop {
            let mut guard = session.lock().await;
            // `recv_node_announce` forwards each node announce to the sink itself;
            // it is cancel-safe, so capping the park at one slice drops the wait
            // without losing a buffered event. `Ok(None)` means the stream closed
            // (the session is stopping) — stop draining. Otherwise (an announce
            // arrived, or the slice elapsed) release the lock and re-park.
            let closed = matches!(
                tokio::time::timeout(DISCOVERY_LOCK_SLICE, guard.recv_node_announce()).await,
                Ok(None)
            );
            drop(guard);
            if closed {
                break;
            }
        }
    })
}

/// Close the session at teardown, after every task that held a clone of it
/// has been awaited.
///
/// If a clone survives anyway, the unwrap fails; the node is then stopped
/// through the lock with [`Session::shutdown`], which needs no ownership, and
/// the returned line says so. It is printed once the terminal is restored,
/// because a line written into the alternate screen is lost with it.
pub async fn close_session<N: NodeLend>(session: Arc<Mutex<Session<N>>>) -> Option<String> {
    match Arc::try_unwrap(session) {
        Ok(mutex) => {
            let _ = mutex.into_inner().close().await;
            None
        }
        Err(shared) => {
            let holders = Arc::strong_count(&shared) - 1;
            let _ = shared.lock().await.shutdown().await;
            Some(format!(
                "lnomad: error: {holders} task(s) still held the session at exit; \
                 the node was stopped through the shared lock"
            ))
        }
    }
}

/// One inline image's outcome, reported by the image-queue task.
pub struct ImageOutcome {
    /// The queue generation this belongs to; a stale one is dropped.
    pub generation: u64,
    /// The link index it belongs to.
    pub link: usize,
    /// Where it was fetched from, which is the cache key.
    pub target: Target,
    /// What happened.
    pub result: ImageResult,
}

/// What the image-queue task has to say about one picture.
pub enum ImageResult {
    /// Its transfer has started.
    Started,
    /// Its bytes arrived.
    Bytes(Vec<u8>),
    /// Its transfer failed.
    Failed(String),
}

/// Fetch a page's inline images one after another, reporting each outcome
/// tagged with `generation`; the frontend holds the task and aborts it when
/// the page is left.
///
/// Sequential on purpose: a link carries one transfer at a time, so issuing
/// them in parallel would only queue them inside the stack while holding the
/// session lock longer. Between images the lock is released, so a navigation
/// the reader starts is served promptly — and it aborts this task anyway.
pub fn spawn_image_queue<N: NodeLend + 'static>(
    session: &Arc<Mutex<Session<N>>>,
    tx: &mpsc::UnboundedSender<ImageOutcome>,
    timeout: Duration,
    requests: Vec<ImageRequest>,
    generation: u64,
) -> tokio::task::JoinHandle<()> {
    let session = session.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        for request in requests {
            let _ = tx.send(ImageOutcome {
                generation,
                link: request.link,
                target: request.target.clone(),
                result: ImageResult::Started,
            });
            let fetched = {
                let mut guard = session.lock().await;
                guard.download_file(&request.target, timeout).await
            };
            let result = match fetched {
                Ok((bytes, _)) => ImageResult::Bytes(bytes),
                Err(err) => ImageResult::Failed(err.to_string()),
            };
            if tx
                .send(ImageOutcome {
                    generation,
                    link: request.link,
                    target: request.target,
                    result,
                })
                .is_err()
            {
                return;
            }
        }
    })
}

/// Write a held picture `name` of `bytes` into the download directory,
/// once: `saved` remembers where, and a second save returns that path, so
/// `s` then `o` does not produce two files.
pub fn save_picture(
    name: &str,
    bytes: &[u8],
    saved: &mut Option<PathBuf>,
) -> Result<(PathBuf, usize), StrText> {
    if let Some(path) = saved {
        return Ok((path.clone(), bytes.len()));
    }
    let dir = crate::download::download_dir().ok_or(StrText::key(StrId::NoDownloadDir))?;
    let path = crate::download::save_to_dir(&dir, name, bytes).map_err(|e| {
        with_args(
            StrId::CouldNotSave,
            vec![Arg::Str(name.to_string()), Arg::Str(e.to_string())],
        )
    })?;
    *saved = Some(path.clone());
    Ok((path, bytes.len()))
}

/// A picture's save told in a toast.
pub fn say_saved(model: &mut Model, saved: Result<(PathBuf, usize), StrText>) {
    match saved {
        Ok((path, bytes)) => model.say(
            ToastKind::Info,
            with_args(
                StrId::SavedImage,
                vec![Arg::Str(path.display().to_string()), Arg::Int(bytes as i64)],
            ),
        ),
        Err(err) => model.say(ToastKind::Error, err),
    }
}

/// A saved picture opened in the system's viewer, told in a toast. Saved
/// first, because the platform handler needs a file: the picture only ever
/// existed in this process's memory.
pub fn open_saved(model: &mut Model, saved: Result<(PathBuf, usize), StrText>) {
    match saved {
        Ok((path, _)) => match open::that(&path) {
            Ok(_) => model.say(
                ToastKind::Info,
                with(StrId::OpenedImage, path.display().to_string()),
            ),
            Err(_) => model.say(
                ToastKind::Error,
                with(StrId::FailedToOpenImage, path.display().to_string()),
            ),
        },
        Err(err) => model.say(ToastKind::Error, err),
    }
}
