//! The HTTP side of `lnmapd`: the store directory served under two
//! routes, `GET /index` and `GET /piece/<id>` — the same artefacts and
//! the same piece ids the Reticulum side answers
//! (`docs/src/concepts/map-server.md`, "The request protocol").
//!
//! Two modes, one directory layout:
//!
//! - **Origin** (no upstream): serves what the store holds, nothing
//!   else. The store at Lew's home runs this.
//! - **Front door** (`--upstream`): serves what it holds; what it lacks
//!   it fetches from the upstream once, verifies, keeps, and serves —
//!   pieces for ever (they are immutable; their name is their hash),
//!   the index for a bounded time. The piece cache is bounded: least
//!   recently served pieces leave when the cap is passed. workorse
//!   runs this in front of the home store.
//!
//! An upstream fetch is serialised behind one lock: the front door asks
//! its upstream for one thing at a time, so a thundering herd on a cold
//! cache costs the home link one transfer per piece, not one per
//! requester.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

use leviculum_core::crypto::sha256;

use crate::store::{self, Index};

/// The front-door cache's knobs.
#[derive(Clone, Debug)]
pub struct CacheConfig {
    /// The upstream `lnmapd`'s HTTP base URL.
    pub upstream: String,
    /// The piece cache's size bound, bytes.
    pub cap_bytes: u64,
    /// How long a fetched index serves before the upstream is asked
    /// again. A failed refresh serves the held index.
    pub index_ttl: Duration,
}

/// Least-recently-served bookkeeping over the pieces directory.
struct Lru {
    /// Piece id to (last-use tick, byte length).
    entries: HashMap<String, (u64, u64)>,
    tick: u64,
    total: u64,
}

impl Lru {
    /// Scan the pieces directory: what is held, ordered by mtime as the
    /// best available guess at past use.
    fn scan(store: &Path) -> Self {
        let mut found: Vec<(String, u64, std::time::SystemTime)> = Vec::new();
        let dir = store.join(store::PIECES_DIR);
        if let Ok(fans) = std::fs::read_dir(&dir) {
            for fan in fans.flatten() {
                let Ok(entries) = std::fs::read_dir(fan.path()) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let Ok(meta) = entry.metadata() else { continue };
                    if store::valid_piece_id(&name) && meta.is_file() {
                        let when = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                        found.push((name, meta.len(), when));
                    }
                }
            }
        }
        found.sort_by_key(|(_, _, when)| *when);
        let mut lru = Lru {
            entries: HashMap::new(),
            tick: 0,
            total: 0,
        };
        for (id, len, _) in found {
            lru.touch(&id, len);
        }
        lru
    }

    fn touch(&mut self, id: &str, len: u64) {
        self.tick += 1;
        if self
            .entries
            .insert(id.to_owned(), (self.tick, len))
            .is_none()
        {
            self.total += len;
        }
    }

    /// The ids to delete so `total` comes back under `cap`, never
    /// `keep` (the piece just written). Entries leave the bookkeeping
    /// here; the caller deletes the files.
    fn evict(&mut self, cap: u64, keep: &str) -> Vec<String> {
        let mut out = Vec::new();
        while self.total > cap {
            let Some(oldest) = self
                .entries
                .iter()
                .filter(|(id, _)| id.as_str() != keep)
                .min_by_key(|(_, (tick, _))| *tick)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            if let Some((_, len)) = self.entries.remove(&oldest) {
                self.total -= len;
            }
            out.push(oldest);
        }
        out
    }
}

/// The server's shared state.
pub struct Served {
    store: PathBuf,
    cache: Option<CacheState>,
}

struct CacheState {
    config: CacheConfig,
    agent: ureq::Agent,
    lru: Mutex<Lru>,
    /// One upstream fetch at a time (see the module docs).
    fetching: tokio::sync::Mutex<()>,
}

impl Served {
    /// An origin server over `store`.
    pub fn origin(store: PathBuf) -> Arc<Self> {
        Arc::new(Self { store, cache: None })
    }

    /// A front-door cache over `store`, filled from `config.upstream`.
    pub fn front_door(store: PathBuf, config: CacheConfig) -> Arc<Self> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .user_agent(concat!("lnmapd/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        let lru = Mutex::new(Lru::scan(&store));
        Arc::new(Self {
            store,
            cache: Some(CacheState {
                config,
                agent,
                lru,
                fetching: tokio::sync::Mutex::new(()),
            }),
        })
    }

    /// The store directory served.
    pub fn store(&self) -> &Path {
        &self.store
    }
}

/// The two routes over `state`.
pub fn router(state: Arc<Served>) -> Router {
    Router::new()
        .route("/index", get(index))
        .route("/piece/{id}", get(piece))
        .with_state(state)
}

fn bytes_response(bytes: Vec<u8>) -> Response {
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
        bytes,
    )
        .into_response()
}

/// `GET /index`.
async fn index(State(state): State<Arc<Served>>) -> Response {
    let path = store::index_path(&state.store);
    let Some(cache) = &state.cache else {
        return match std::fs::read(&path) {
            Ok(bytes) => bytes_response(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                StatusCode::NOT_FOUND.into_response()
            }
            Err(e) => failure("read index", &e.to_string()),
        };
    };

    let held = std::fs::read(&path).ok();
    let fresh_enough = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|when| when.elapsed().ok())
        .is_some_and(|age| age < cache.config.index_ttl);
    if let (Some(bytes), true) = (&held, fresh_enough) {
        return bytes_response(bytes.clone());
    }

    let _one_at_a_time = cache.fetching.lock().await;
    let url = format!("{}/index", cache.config.upstream.trim_end_matches('/'));
    let agent = cache.agent.clone();
    let fetched = tokio::task::spawn_blocking(move || http_get(&agent, &url)).await;
    let fetched = match fetched {
        Ok(result) => result,
        Err(join) => Err(join.to_string()),
    };
    match fetched {
        Ok(bytes) => {
            // Self-consistency and no rollback behind what is held; the
            // pin against `map_destination` is the device's own check.
            let floor = held
                .as_deref()
                .and_then(|old| Index::verify_self(old, None).ok())
                .map(|old| old.generation);
            match Index::verify_self(&bytes, floor) {
                Ok(_) => {
                    if let Err(e) = store::atomic_write(&path, &bytes) {
                        return failure("write index", &e.to_string());
                    }
                    bytes_response(bytes)
                }
                Err(e) => match held {
                    // A worse or broken upstream answer never replaces a
                    // held index.
                    Some(bytes) => {
                        eprintln!("lnmapd: upstream index refused ({e}), serving the held one");
                        bytes_response(bytes)
                    }
                    None => failure("upstream index", &e.to_string()),
                },
            }
        }
        Err(reason) => match held {
            Some(bytes) => {
                eprintln!("lnmapd: upstream index unreachable ({reason}), serving the held one");
                bytes_response(bytes)
            }
            None => failure("upstream index", &reason),
        },
    }
}

/// `GET /piece/<id>`.
async fn piece(State(state): State<Arc<Served>>, UrlPath(id): UrlPath<String>) -> Response {
    if !store::valid_piece_id(&id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = store::piece_path(&state.store, &id);
    if let Ok(bytes) = std::fs::read(&path) {
        if let Some(cache) = &state.cache {
            if let Ok(mut lru) = cache.lru.lock() {
                lru.touch(&id, bytes.len() as u64);
            }
        }
        return bytes_response(bytes);
    }
    let Some(cache) = &state.cache else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let _one_at_a_time = cache.fetching.lock().await;
    // A concurrent request may have landed it while this one waited.
    if let Ok(bytes) = std::fs::read(&path) {
        return bytes_response(bytes);
    }
    let url = format!("{}/piece/{id}", cache.config.upstream.trim_end_matches('/'));
    let agent = cache.agent.clone();
    let fetched = tokio::task::spawn_blocking(move || http_get(&agent, &url)).await;
    let bytes = match fetched {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(reason)) => {
            if reason == "404" {
                return StatusCode::NOT_FOUND.into_response();
            }
            return failure("upstream piece", &reason);
        }
        Err(join) => return failure("upstream piece", &join.to_string()),
    };
    // Content addressing is the whole integrity check: bytes that do
    // not hash to the asked name are refused and nothing is cached.
    if hex::encode(sha256(&bytes)) != id {
        return failure("upstream piece", "bytes do not hash to the piece id");
    }
    if let Err(e) = store::atomic_write(&path, &bytes) {
        return failure("write piece", &e.to_string());
    }
    let evicted = match cache.lru.lock() {
        Ok(mut lru) => {
            lru.touch(&id, bytes.len() as u64);
            lru.evict(cache.config.cap_bytes, &id)
        }
        Err(_) => Vec::new(),
    };
    for old in evicted {
        let _ = std::fs::remove_file(store::piece_path(&state.store, &old));
    }
    bytes_response(bytes)
}

fn failure(what: &str, reason: &str) -> Response {
    eprintln!("lnmapd: {what}: {reason}");
    StatusCode::BAD_GATEWAY.into_response()
}

/// One blocking GET, the whole body; an error is its reason, a plain
/// `404` for a missing upstream piece so the handler can pass it on.
fn http_get(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    let mut response = agent.get(url).call().map_err(|e| match e {
        ureq::Error::StatusCode(404) => "404".to_owned(),
        other => other.to_string(),
    })?;
    response.body_mut().read_to_vec().map_err(|e| e.to_string())
}
