//! The page transfers a frontend runs (the terminal's loop, the window's
//! driver): a fetch and a download as background tasks tagged with a
//! generation, the continuous node discovery over the shared session, and
//! closing the session at teardown. No terminal and no window in here; each
//! frontend folds the outcomes into the model as its events.

use std::sync::Arc;
use std::time::Duration;

use leviculum_micron::MicronDocument;
use tokio::sync::{mpsc, Mutex};

use crate::app::strings::{with_args, Arg, StrId, Text as StrText};
use crate::browser;
use crate::fetch::Session;
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
pub fn spawn_fetch(
    inflight: &mut Option<tokio::task::JoinHandle<()>>,
    generation: &mut u64,
    session: &Arc<Mutex<Session>>,
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
        let result = match browser::fetch_document(&mut guard, &target, timeout).await {
            Ok(doc) => {
                let name = guard.node_name(&target.dest_hash);
                let title = browser::page_title(name.as_deref(), &target.dest_hash, &target.path);
                LoadResult::Ok {
                    doc,
                    title,
                    dest: target.dest_hash,
                    identifying: guard.is_identifying(&target.dest_hash),
                }
            }
            Err(err) => LoadResult::Err(err.text()),
        };
        let _ = tx.send(FetchOutcome {
            generation: generation_now,
            result,
        });
    });
    *inflight = Some(handle);
}

/// Start a background file download for `target`, sharing the in-flight slot
/// and generation counter with fetches: starting either supersedes the other.
/// The spawned task fetches the raw bytes over the session, saves them into
/// the download directory (`$XDG_DOWNLOAD_DIR`, else `$HOME/Downloads`,
/// created if missing, existing names de-duplicated), and reports the outcome.
pub fn spawn_download(
    inflight: &mut Option<tokio::task::JoinHandle<()>>,
    generation: &mut u64,
    session: &Arc<Mutex<Session>>,
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
        let fetched = guard.download_file(&target, timeout).await;
        drop(guard);
        let result = match fetched {
            Ok((bytes, server_name)) => {
                let name = crate::download::choose_name(server_name.as_deref(), &target.path);
                match crate::download::download_dir() {
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
                }
            }
            Err(err) => LoadResult::DownloadErr(StrText::Raw(err.to_string())),
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
pub fn spawn_discovery(session: &Arc<Mutex<Session>>) -> tokio::task::JoinHandle<()> {
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
pub async fn close_session(session: Arc<Mutex<Session>>) -> Option<String> {
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
