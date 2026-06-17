//! Logging control and the process-global `tracing` bridge.
//!
//! The Reticulum stack logs through `tracing`. A C app cannot install a Rust
//! subscriber, so the library installs one (once, via [`crate::ensure_init`])
//! that forwards records to a level filter and an optional C callback, in the
//! spirit of libcurl's `CURLOPT_DEBUGFUNCTION`. Default level is
//! [`LEV_LOG_OFF`], so the library is silent unless asked. See
//! `docs/leviculum-api-design.md` §12.

use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

use crate::error::set_last_error;
use crate::guard;

/// No logging (default).
pub const LEV_LOG_OFF: c_int = 0;
/// Errors only.
pub const LEV_LOG_ERROR: c_int = 1;
/// Warnings and above.
pub const LEV_LOG_WARN: c_int = 2;
/// Info and above.
pub const LEV_LOG_INFO: c_int = 3;
/// Debug and above.
pub const LEV_LOG_DEBUG: c_int = 4;
/// Everything.
pub const LEV_LOG_TRACE: c_int = 5;

/// Log sink callback: `(level, message, user)`. The message is a
/// NUL-terminated string owned by the library, valid only for the duration of
/// the call. The callback may run on any internal worker thread and must not
/// call back into any `lev_*` function (see design §5, §12). NULL restores the
/// stderr default.
///
/// Nullable on purpose: `Option` is inlined into the typedef so cbindgen
/// renders it as a plain (nullable) C function pointer, not an opaque struct.
pub type lev_log_callback =
    Option<extern "C" fn(level: c_int, message: *const c_char, user: *mut c_void)>;

/// Non-null callback pointer, the inner of [`lev_log_callback`], for storage.
type LogCb = extern "C" fn(level: c_int, message: *const c_char, user: *mut c_void);

/// Current verbosity. Events at a level numerically `<=` this are emitted.
static LEVEL: AtomicU8 = AtomicU8::new(LEV_LOG_OFF as u8);

/// Installed callback and its opaque user pointer. The pointer is stored as a
/// `usize` so the slot is `Send`; the app owns its lifetime.
#[derive(Clone, Copy)]
struct Sink {
    cb: LogCb,
    user: usize,
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);

fn current_level() -> c_int {
    LEVEL.load(Ordering::Relaxed) as c_int
}

fn level_to_lev(level: Level) -> c_int {
    match level {
        Level::ERROR => LEV_LOG_ERROR,
        Level::WARN => LEV_LOG_WARN,
        Level::INFO => LEV_LOG_INFO,
        Level::DEBUG => LEV_LOG_DEBUG,
        Level::TRACE => LEV_LOG_TRACE,
    }
}

/// Forward one formatted record to the C callback, or to stderr when none is
/// set. The sink is copied out under the lock and released before the callback
/// runs, so a callback that is slow or re-enters logging cannot deadlock.
fn dispatch(level: c_int, target: &str, msg: &str) {
    let line = format!("{target}: {msg}");
    let sink = SINK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .copied();
    match sink {
        Some(Sink { cb, user }) => {
            // Truncate at the first NUL so construction cannot fail.
            let bytes = line.into_bytes();
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            // Safe: no interior NUL up to `end`.
            let c = unsafe { std::ffi::CString::from_vec_unchecked(bytes[..end].to_vec()) };
            cb(level, c.as_ptr(), user as *mut c_void);
        }
        None => eprintln!("[leviculum] {line}"),
    }
}

/// Collects an event's `message` field, appending any other fields as
/// `key=value`, into a single line for the C sink.
struct MsgVisitor(String);

impl tracing::field::Visit for MsgVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }
}

/// `tracing` layer that gates by [`LEVEL`] and forwards to [`dispatch`].
struct CLayer;

impl<S: Subscriber> Layer<S> for CLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let lev = level_to_lev(*meta.level());
        if lev > current_level() {
            return;
        }
        let mut visitor = MsgVisitor(String::new());
        event.record(&mut visitor);
        dispatch(lev, meta.target(), &visitor.0);
    }
}

/// Install the subscriber and a panic hook. Called exactly once through the
/// `Once` in [`crate::ensure_init`].
pub(crate) fn install() {
    // try_init fails if the host already set a global subscriber; in that case
    // we defer to it rather than override (good citizenship).
    let _ = tracing_subscriber::registry().with(CLayer).try_init();

    // Chain a hook that surfaces panics through our sink, then calls the
    // previous hook so host behaviour (and backtraces) is preserved.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        dispatch(LEV_LOG_ERROR, "panic", &format!("{info}"));
        prev(info);
    }));
}

/// Set the global log level. One of the `LEV_LOG_*` constants.
#[no_mangle]
pub extern "C" fn lev_log_set_level(level: c_int) -> c_int {
    guard(crate::LEV_ERR_PANIC, || {
        crate::ensure_init();
        if !(LEV_LOG_OFF..=LEV_LOG_TRACE).contains(&level) {
            set_last_error("log level out of range");
            return crate::LEV_ERR_INVALID_ARG;
        }
        LEVEL.store(level as u8, Ordering::Relaxed);
        crate::LEV_OK
    })
}

/// Set the log sink callback, or pass `NULL` to restore the stderr default.
/// `user` is passed back to the callback unchanged.
#[no_mangle]
pub extern "C" fn lev_log_set_callback(cb: lev_log_callback, user: *mut c_void) -> c_int {
    guard(crate::LEV_ERR_PANIC, || {
        crate::ensure_init();
        let mut slot = SINK.lock().unwrap_or_else(|e| e.into_inner());
        *slot = cb.map(|cb| Sink {
            cb,
            user: user as usize,
        });
        crate::LEV_OK
    })
}
