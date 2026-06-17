//! Error codes and the thread-local last-error detail string.
//!
//! Classic Unix shape: functions return `int`, `0` is success and a negative
//! `LEV_ERR_*` is failure. `lev_strerror` maps a code to a static string;
//! `lev_last_error` returns a thread-local detail string for the most recent
//! failing call on the calling thread. See `docs/leviculum-api-design.md` §2.

use std::cell::RefCell;
use std::ffi::CString;
use std::os::raw::{c_char, c_int};

/// Success.
pub const LEV_OK: c_int = 0;
/// A required pointer argument was NULL.
pub const LEV_ERR_NULL_PTR: c_int = -1;
/// An argument was malformed (bad length, unparseable string, ...).
pub const LEV_ERR_INVALID_ARG: c_int = -2;
/// The caller buffer was too small; `*out_len` holds the required size.
pub const LEV_ERR_BUFFER_TOO_SMALL: c_int = -3;
/// The node event loop is not running.
pub const LEV_ERR_NOT_RUNNING: c_int = -4;
/// An I/O or storage error occurred.
pub const LEV_ERR_IO: c_int = -5;
/// A configuration error occurred.
pub const LEV_ERR_CONFIG: c_int = -6;
/// A cryptographic operation failed.
pub const LEV_ERR_CRYPTO: c_int = -7;
/// No path to the destination is known; request one first.
pub const LEV_ERR_NO_PATH: c_int = -8;
/// A link operation failed (closed, inactive, or handshake).
pub const LEV_ERR_LINK: c_int = -9;
/// A send failed (no route, payload too large, ...).
pub const LEV_ERR_SEND: c_int = -10;
/// A resource transfer operation failed.
pub const LEV_ERR_RESOURCE: c_int = -11;
/// A request or response operation failed.
pub const LEV_ERR_REQUEST: c_int = -12;
/// An operation timed out.
pub const LEV_ERR_TIMEOUT: c_int = -13;
/// Non-fatal backpressure; retry later (mirrors EAGAIN).
pub const LEV_ERR_AGAIN: c_int = -14;
/// A panic was caught at the FFI boundary and converted to an error.
pub const LEV_ERR_PANIC: c_int = -127;

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

/// Build a `CString` from bytes that may contain interior NULs.
///
/// Truncates at the first NUL so construction never fails, keeping this off the
/// `unwrap`/`expect`-free non-test path.
fn to_cstring_lossy(mut msg: Vec<u8>) -> CString {
    if let Some(pos) = msg.iter().position(|&b| b == 0) {
        msg.truncate(pos);
    }
    // Safe: `msg` has no interior NUL after truncation.
    unsafe { CString::from_vec_unchecked(msg) }
}

/// Record the detail string for the most recent failure on this thread.
pub(crate) fn set_last_error(msg: impl Into<Vec<u8>>) {
    let c = to_cstring_lossy(msg.into());
    LAST_ERROR.with(|e| *e.borrow_mut() = Some(c));
}

/// Map a facade [`reticulum_std::Error`] to a `LEV_ERR_*` code, recording its
/// `Display` text as the thread-local detail string.
pub(crate) fn map_error(e: &reticulum_std::Error) -> c_int {
    use reticulum_std::Error;
    set_last_error(e.to_string());
    match e {
        Error::Io(_) | Error::Storage(_) => LEV_ERR_IO,
        Error::Config(_) => LEV_ERR_CONFIG,
        Error::Serialization(_) => LEV_ERR_INVALID_ARG,
        Error::NotRunning => LEV_ERR_NOT_RUNNING,
        Error::Announce(_) | Error::Send(_) => LEV_ERR_SEND,
        Error::Link(_) => LEV_ERR_LINK,
        Error::Resource(_) => LEV_ERR_RESOURCE,
        Error::Request(_) => LEV_ERR_REQUEST,
    }
}

/// Return a static, never-freed message for an error code.
///
/// Safe to call at any time. The returned pointer must not be freed.
#[no_mangle]
pub extern "C" fn lev_strerror(code: c_int) -> *const c_char {
    let msg: &'static [u8] = match code {
        LEV_OK => b"success\0",
        LEV_ERR_NULL_PTR => b"null pointer\0",
        LEV_ERR_INVALID_ARG => b"invalid argument\0",
        LEV_ERR_BUFFER_TOO_SMALL => b"buffer too small\0",
        LEV_ERR_NOT_RUNNING => b"node event loop is not running\0",
        LEV_ERR_IO => b"I/O error\0",
        LEV_ERR_CONFIG => b"configuration error\0",
        LEV_ERR_CRYPTO => b"cryptographic error\0",
        LEV_ERR_NO_PATH => b"no path to destination\0",
        LEV_ERR_LINK => b"link error\0",
        LEV_ERR_SEND => b"send error\0",
        LEV_ERR_RESOURCE => b"resource error\0",
        LEV_ERR_REQUEST => b"request error\0",
        LEV_ERR_TIMEOUT => b"timed out\0",
        LEV_ERR_AGAIN => b"resource temporarily unavailable\0",
        LEV_ERR_PANIC => b"panic at FFI boundary\0",
        _ => b"unknown error\0",
    };
    msg.as_ptr() as *const c_char
}

/// Return the thread-local detail string for the most recent failing call on
/// this thread, or NULL if there is none.
///
/// The string is owned by the library and valid until the next failing call on
/// the same thread. The caller must not free it.
#[no_mangle]
pub extern "C" fn lev_last_error() -> *const c_char {
    LAST_ERROR.with(|e| match &*e.borrow() {
        Some(c) => c.as_ptr(),
        None => std::ptr::null(),
    })
}
