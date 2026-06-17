//! C API for Leviculum.
//!
//! A thin, Unix-idiomatic C surface over the curated `reticulum_std::api`
//! facade. Conventions:
//!
//! - Every symbol is prefixed `lev_` (functions) or `LEV_` (constants).
//! - Complex objects are opaque handles with a `_free` counterpart.
//! - Functions return `int`: `0` success, negative `LEV_ERR_*` on failure.
//! - Output buffers are caller-owned, read(2) style (`buf` + `cap` + `out_len`).
//! - No panic ever crosses the boundary; every function runs under [`guard`].
//!
//! The design of record is `docs/leviculum-api-design.md`.

#![allow(non_camel_case_types)]
#![allow(clippy::missing_safety_doc)]
#![warn(unreachable_pub)]

use std::os::raw::{c_char, c_int};
use std::panic::AssertUnwindSafe;
use std::sync::Once;

mod destination;
mod error;
mod events;
mod identity;
mod log;
mod node;

pub use destination::*;
pub use error::*;
pub use events::*;
pub use identity::*;
pub use log::*;
pub use node::*;

/// Runs process-global setup exactly once.
static INIT: Once = Once::new();

/// Ensure one-time process setup has run: install the logging subscriber and
/// panic hook. Idempotent and thread-safe; the lazy path taken by other entry
/// points goes through the same `Once` as the explicit `lev_init`.
pub(crate) fn ensure_init() {
    INIT.call_once(log::install);
}

/// Perform one-time process setup (logging subscriber, panic hook).
///
/// Idempotent and safe to call from multiple threads. Optional: other entry
/// points run it lazily, but call it explicitly to configure logging before
/// the first node is built.
#[no_mangle]
pub extern "C" fn lev_init() -> c_int {
    guard(LEV_ERR_PANIC, || {
        ensure_init();
        LEV_OK
    })
}

/// Run an FFI body under `catch_unwind`, converting a panic into `default`.
///
/// Unwinding into C is undefined behaviour, so every `extern "C"` function
/// wraps its body in this guard. `default` is `LEV_ERR_PANIC` for the int
/// returning functions and a null pointer for constructors.
pub(crate) fn guard<T>(default: T, f: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => {
            // Allocation-free: a panic handler must not do work that could
            // itself panic (e.g. allocate after an allocation failure).
            error::set_last_error_static(c"panic in libleviculum");
            default
        }
    }
}

/// Copy `src` into a caller-owned buffer, read(2) style.
///
/// Sets `*out_len` to `src.len()`. If `buf` is NULL or `cap` is too small,
/// writes nothing and returns `LEV_ERR_BUFFER_TOO_SMALL` (so a NULL buffer is a
/// valid size query). Returns `LEV_ERR_NULL_PTR` if `out_len` is NULL.
pub(crate) unsafe fn write_out(src: &[u8], buf: *mut u8, cap: usize, out_len: *mut usize) -> c_int {
    if out_len.is_null() {
        return LEV_ERR_NULL_PTR;
    }
    *out_len = src.len();
    if buf.is_null() || cap < src.len() {
        return LEV_ERR_BUFFER_TOO_SMALL;
    }
    std::ptr::copy_nonoverlapping(src.as_ptr(), buf, src.len());
    LEV_OK
}

/// Return the library version string, for example `"0.6.3"`.
///
/// Static storage, never freed.
#[no_mangle]
pub extern "C" fn lev_version_string() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

/// Return the library version as `(major << 16) | (minor << 8) | patch`.
#[no_mangle]
pub extern "C" fn lev_version_number() -> u32 {
    guard(0, || {
        let (major, minor, patch) = reticulum_std::api::version();
        ((major as u32) << 16) | ((minor as u32) << 8) | (patch as u32)
    })
}
