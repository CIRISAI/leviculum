//! A small HTTP/1.1 server of files held in memory, answering range
//! requests, on a loopback port it binds itself: the map source of the
//! tests (1089), which must not touch the network. One thread accepts,
//! one serves each connection; every response closes its connection.
//!
//! Not a server for anything else: no keep-alive, no `HEAD`, one range per
//! request, which is all [`crate::http::HttpRange`] sends.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// A running server; it stops when dropped.
pub struct Serve {
    base: String,
    shared: Arc<Shared>,
}

struct Shared {
    files: Mutex<HashMap<String, Arc<Vec<u8>>>>,
    requests: AtomicUsize,
    /// Answer every request with `503`, for a source that fails.
    failing: AtomicBool,
    stop: AtomicBool,
    /// A slow link (1291): bytes a second all answers share, 0 for none,
    /// and the wait before each answer's first byte.
    link: Mutex<Link>,
    /// Body bytes written so far, every answer together.
    sent: AtomicU64,
    /// The body byte the answers stop before until [`Serve::release`],
    /// `None` for no hold (1610).
    hold: Mutex<Option<u64>>,
    released: Condvar,
    /// The body byte every answer drops its connection at, `None` for
    /// none (1727): a link that breaks mid-download.
    drop_at: Mutex<Option<u64>>,
}

#[derive(Default)]
struct Link {
    rate: u64,
    latency: Duration,
    /// When the link is free for the next chunk.
    free: Option<Instant>,
}

/// The bytes a throttled answer sends at once.
const CHUNK: usize = 16 * 1024;

impl Serve {
    /// Serve `files` (path without the leading slash, bytes) on a port of
    /// 127.0.0.1 bound here and held.
    pub fn start(files: Vec<(String, Vec<u8>)>) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let base = format!("http://{}/", listener.local_addr()?);
        let shared = Arc::new(Shared {
            files: Mutex::new(files.into_iter().map(|(p, b)| (p, Arc::new(b))).collect()),
            requests: AtomicUsize::new(0),
            failing: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            link: Mutex::new(Link::default()),
            sent: AtomicU64::new(0),
            hold: Mutex::new(None),
            released: Condvar::new(),
            drop_at: Mutex::new(None),
        });
        let theirs = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("map-serve".to_owned())
            .spawn(move || {
                for stream in listener.incoming() {
                    if theirs.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(stream) = stream else { continue };
                    let shared = Arc::clone(&theirs);
                    let _ = std::thread::Builder::new()
                        .name("map-serve-conn".to_owned())
                        .spawn(move || {
                            let _ = answer(&shared, stream);
                        });
                }
            })?;
        Ok(Self { base, shared })
    }

    /// `http://127.0.0.1:<port>/`.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The URL of `path`.
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Requests answered so far.
    pub fn requests(&self) -> usize {
        self.shared.requests.load(Ordering::SeqCst)
    }

    /// Answer `503` from now on, or serve again.
    pub fn fail(&self, failing: bool) {
        self.shared.failing.store(failing, Ordering::SeqCst);
    }

    /// Body bytes written so far, every answer together.
    pub fn sent(&self) -> u64 {
        self.shared.sent.load(Ordering::SeqCst)
    }

    /// Stop every answer once `bytes` body bytes are written in all, until
    /// [`release`](Self::release) (1610): a download held part way, so a
    /// test sees the state it is in for as long as it needs to on a slow
    /// host, instead of racing the link for it.
    pub fn hold_at(&self, bytes: u64) {
        *self
            .shared
            .hold
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(bytes);
        self.shared.released.notify_all();
    }

    /// Close every answer's connection once `bytes` body bytes are
    /// written in all, mid-body, and every later answer's before its
    /// first body byte; `None` serves whole again (1727): a link that
    /// breaks part way through a download, for the test that the region
    /// it was to replace survives.
    pub fn drop_at(&self, bytes: Option<u64>) {
        *self
            .shared
            .drop_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = bytes;
    }

    /// Lift a [`hold_at`](Self::hold_at).
    pub fn release(&self) {
        *self
            .shared
            .hold
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        self.shared.released.notify_all();
    }

    /// Send at most `rate` bytes a second over every connection together,
    /// each answer's first byte `latency` after its request: a slow link,
    /// for the tests and the measurement of a region arriving (1291). A
    /// rate of 0 sends at full speed again.
    pub fn throttle(&self, rate: u64, latency: Duration) {
        let mut link = self
            .shared
            .link
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *link = Link {
            rate,
            latency,
            free: None,
        };
    }
}

/// Write `body` to `stream` at the link's rate, stopping at a hold.
fn send(shared: &Shared, stream: &mut TcpStream, body: &[u8]) -> std::io::Result<()> {
    let (rate, latency) = {
        let link = shared.link.lock().unwrap_or_else(PoisonError::into_inner);
        (link.rate, link.latency)
    };
    std::thread::sleep(latency);
    let mut rest = body;
    while !rest.is_empty() {
        let room = held(shared);
        let cut = *shared
            .drop_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let left = match cut {
            Some(at) => at.saturating_sub(shared.sent.load(Ordering::SeqCst)),
            None => u64::MAX,
        };
        if left == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "the link broke (Serve::drop_at)",
            ));
        }
        let take = rest
            .len()
            .min(CHUNK)
            .min(room)
            .min(usize::try_from(left).unwrap_or(usize::MAX));
        let (chunk, after) = rest.split_at(take);
        pace(shared, rate, chunk.len());
        stream.write_all(chunk)?;
        shared.sent.fetch_add(chunk.len() as u64, Ordering::SeqCst);
        rest = after;
    }
    Ok(())
}

/// The bytes an answer may write before the hold, at least one: waits
/// while the hold is reached.
fn held(shared: &Shared) -> usize {
    let mut hold = shared.hold.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        let sent = shared.sent.load(Ordering::SeqCst);
        match *hold {
            Some(at) if sent >= at => {
                hold = shared
                    .released
                    .wait(hold)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            Some(at) => return usize::try_from(at - sent).unwrap_or(usize::MAX),
            None => return usize::MAX,
        }
    }
}

/// Wait for `len` bytes' time on the shared link of `rate` bytes a second,
/// none at a rate of 0.
fn pace(shared: &Shared, rate: u64, len: usize) {
    if rate == 0 {
        return;
    }
    // Book the chunk's time on the shared link, then wait for it.
    let start = {
        let mut link = shared.link.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        let start = link.free.map_or(now, |f| f.max(now));
        let takes = Duration::from_secs_f64(len as f64 / link.rate.max(1) as f64);
        link.free = Some(start + takes);
        start + takes
    };
    if let Some(wait) = start.checked_duration_since(Instant::now()) {
        std::thread::sleep(wait);
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // An answer stopped at a hold ends instead of waiting for ever.
        self.release();
        // Wake the accept loop so it sees the flag.
        let addr = self
            .base
            .trim_start_matches("http://")
            .trim_end_matches('/');
        let _ = TcpStream::connect(addr);
    }
}

fn answer(shared: &Shared, mut stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let path = line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .trim_start_matches('/')
        .to_owned();
    let mut range: Option<(u64, Option<u64>)> = None;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        let Some((k, v)) = h.split_once(':') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case("range") {
            range = v.trim().strip_prefix("bytes=").and_then(|r| {
                let (a, b) = r.split_once('-')?;
                Some((a.parse().ok()?, b.parse().ok()))
            });
        }
    }
    shared.requests.fetch_add(1, Ordering::SeqCst);
    if shared.failing.load(Ordering::SeqCst) {
        return write!(
            stream,
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
    }
    let file = shared
        .files
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&path)
        .cloned();
    let Some(file) = file else {
        return write!(
            stream,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
    };
    let len = file.len() as u64;
    match range {
        Some((start, end)) if start < len => {
            let end = end.unwrap_or(len - 1).min(len - 1);
            let body = &file[start as usize..=end as usize];
            write!(
                stream,
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{len}\r\nConnection: close\r\n\r\n",
                body.len()
            )?;
            send(shared, &mut stream, body)
        }
        Some(_) => write!(
            stream,
            "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nContent-Range: bytes */{len}\r\nConnection: close\r\n\r\n"
        ),
        None => {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
            )?;
            send(shared, &mut stream, &file)
        }
    }
}
