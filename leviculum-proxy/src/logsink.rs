//! A stderr sink for the proxy's diagnostics that can never block forwarding.
//!
//! The proxy forwards both directions from a single task, and logs two lines
//! per KISS frame. Its stderr is a pipe held by the test runner, which only
//! reads it once the proxy has been killed. A pipe holds about 64 KiB, so a
//! blocking writer stops the forwarder after a couple of hundred frames and
//! the board behind the proxy goes deaf and mute — the data plane taken down
//! by its own telemetry.
//!
//! So the sink hands log lines to a dedicated thread through a bounded queue
//! and drops them when that queue is full. A dropped line is still reported:
//! the count is emitted as soon as the reader drains again, so a thinned log
//! says so rather than reading as a quiet one.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

/// Queued chunks before new ones are dropped. One chunk is one log event, so
/// this is a few hundred KiB of backlog at proxy line lengths — deep enough
/// to ride out a reader that stalls for a moment, shallow enough that the
/// memory cost is bounded.
const QUEUE_CAPACITY: usize = 4096;

struct Shared {
    queue: Mutex<VecDeque<Vec<u8>>>,
    ready: Condvar,
    dropped: AtomicUsize,
}

/// Handle to the writer thread. Holding it keeps the sink installed; the
/// thread runs for the lifetime of the process.
pub struct LogSink {
    shared: Arc<Shared>,
}

/// The `io::Write` half handed to `tracing_subscriber` for each event.
pub struct SinkWriter {
    shared: Arc<Shared>,
}

impl Write for SinkWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut queue = match self.shared.queue.lock() {
            Ok(q) => q,
            // A poisoned log queue must not take the data plane with it.
            Err(poisoned) => poisoned.into_inner(),
        };
        if queue.len() < QUEUE_CAPACITY {
            queue.push_back(buf.to_vec());
            drop(queue);
            self.shared.ready.notify_one();
        } else {
            drop(queue);
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
        }
        // Always claim the full write: the caller is a logger, and telling it
        // the write was short would only make it try again.
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl LogSink {
    /// Start the writer thread and return the handle.
    pub fn spawn() -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
            dropped: AtomicUsize::new(0),
        });
        let worker = Arc::clone(&shared);
        // Detached on purpose: it must outlive every logging call site, and
        // the process has no shutdown path that could join it.
        std::thread::Builder::new()
            .name("lora-proxy-log".into())
            .spawn(move || writer_loop(&worker))
            .expect("spawn log writer thread");
        Self { shared }
    }

    /// The `MakeWriter` closure to hand to `tracing_subscriber`.
    pub fn make_writer(&self) -> impl Fn() -> SinkWriter + Send + Sync + 'static {
        let shared = Arc::clone(&self.shared);
        move || SinkWriter {
            shared: Arc::clone(&shared),
        }
    }
}

fn writer_loop(shared: &Arc<Shared>) {
    let mut stderr = io::stderr();
    loop {
        let chunk = {
            let mut queue = match shared.queue.lock() {
                Ok(q) => q,
                Err(poisoned) => poisoned.into_inner(),
            };
            while queue.is_empty() {
                queue = match shared.ready.wait(queue) {
                    Ok(q) => q,
                    Err(poisoned) => poisoned.into_inner(),
                };
            }
            // `while` above guarantees a front element.
            match queue.pop_front() {
                Some(c) => c,
                None => continue,
            }
        };

        // Announce the gap before the first line that follows it, so the log
        // never hides how much of itself is missing.
        let dropped = shared.dropped.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            let _ = writeln!(
                stderr,
                "--- lora-proxy: {dropped} log line(s) dropped, stderr not draining ---"
            );
        }
        // Blocking here is the point: this thread absorbs the back-pressure
        // instead of the forwarder.
        let _ = stderr.write_all(&chunk);
        let _ = stderr.flush();
    }
}
