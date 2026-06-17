//! Destination handle, registration, and announce.
//!
//! `lev_destination_t` wraps a `reticulum_std::api::Destination`. Because the
//! core `Destination` is not `Clone`, `lev_register_destination` consumes it
//! (the handle is emptied, like the builder); read its hash first. See
//! `docs/leviculum-api-design.md` §10.

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::time::Duration;

use reticulum_std::api::{Destination, DestinationHash, DestinationType, Direction};

use crate::error::*;
use crate::identity::lev_identity_t;
use crate::node::{block_on_timeout, leviculum_t};
use crate::{guard, read_array, write_out, LEV_ADDR_LEN};

/// Incoming destination: receives announces, links, and packets.
pub const LEV_DIRECTION_IN: c_int = 0;
/// Outgoing destination: a source address for sending.
pub const LEV_DIRECTION_OUT: c_int = 1;

/// Single destination: point-to-point, ephemeral encryption.
pub const LEV_DEST_SINGLE: c_int = 0;
/// Group destination: shared-key broadcast.
pub const LEV_DEST_GROUP: c_int = 1;
/// Plain destination: unencrypted.
pub const LEV_DEST_PLAIN: c_int = 2;

/// Opaque destination handle. `inner` is taken by
/// `lev_register_destination`; the caller still frees the empty shell.
pub struct lev_destination_t {
    inner: Option<Destination>,
}

fn direction_from(d: c_int) -> Option<Direction> {
    match d {
        LEV_DIRECTION_IN => Some(Direction::In),
        LEV_DIRECTION_OUT => Some(Direction::Out),
        _ => None,
    }
}

fn dest_type_from(t: c_int) -> Option<DestinationType> {
    match t {
        LEV_DEST_SINGLE => Some(DestinationType::Single),
        LEV_DEST_GROUP => Some(DestinationType::Group),
        LEV_DEST_PLAIN => Some(DestinationType::Plain),
        _ => None,
    }
}

/// Create a destination.
///
/// `identity` may be NULL (required for some types, forbidden for PLAIN).
/// `aspects` is an array of `n_aspects` NUL-terminated UTF-8 strings.
/// Returns NULL on failure.
#[no_mangle]
pub unsafe extern "C" fn lev_destination_new(
    identity: *const lev_identity_t,
    direction: c_int,
    dest_type: c_int,
    app_name: *const c_char,
    aspects: *const *const c_char,
    n_aspects: usize,
) -> *mut lev_destination_t {
    guard(std::ptr::null_mut(), || {
        let dir = match direction_from(direction) {
            Some(d) => d,
            None => {
                set_last_error("invalid direction");
                return std::ptr::null_mut();
            }
        };
        let dtype = match dest_type_from(dest_type) {
            Some(t) => t,
            None => {
                set_last_error("invalid destination type");
                return std::ptr::null_mut();
            }
        };
        let app_name = match app_name.as_ref().map(|_| CStr::from_ptr(app_name).to_str()) {
            Some(Ok(s)) => s,
            _ => {
                set_last_error("app_name must be a valid UTF-8 string");
                return std::ptr::null_mut();
            }
        };
        // Borrow each aspect string for the duration of the call; Destination::new
        // copies what it needs into the name hash.
        let aspect_slice: &[*const c_char] = if n_aspects == 0 {
            &[]
        } else if aspects.is_null() {
            set_last_error("aspects pointer is null");
            return std::ptr::null_mut();
        } else {
            std::slice::from_raw_parts(aspects, n_aspects)
        };
        let mut aspect_strs: Vec<&str> = Vec::with_capacity(n_aspects);
        for &p in aspect_slice {
            match p.as_ref().map(|_| CStr::from_ptr(p).to_str()) {
                Some(Ok(s)) => aspect_strs.push(s),
                _ => {
                    set_last_error("aspect must be a valid UTF-8 string");
                    return std::ptr::null_mut();
                }
            }
        }
        let id = identity.as_ref().map(|i| i.inner.clone());
        match Destination::new(id, dir, dtype, app_name, &aspect_strs) {
            Ok(d) => Box::into_raw(Box::new(lev_destination_t { inner: Some(d) })),
            Err(e) => {
                set_last_error(format!("{e:?}"));
                std::ptr::null_mut()
            }
        }
    })
}

/// Free a destination handle. `lev_destination_free(NULL)` is a no-op.
#[no_mangle]
pub unsafe extern "C" fn lev_destination_free(dest: *mut lev_destination_t) {
    guard((), || {
        if !dest.is_null() {
            drop(Box::from_raw(dest));
        }
    })
}

/// Write the destination hash (16 bytes) into `buf`, read(2) style.
/// `LEV_ERR_INVALID_ARG` if the destination was already registered (consumed).
#[no_mangle]
pub unsafe extern "C" fn lev_destination_hash(
    dest: *const lev_destination_t,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> c_int {
    guard(LEV_ERR_PANIC, || {
        let d = match dest.as_ref() {
            Some(d) => d,
            None => return LEV_ERR_NULL_PTR,
        };
        match &d.inner {
            Some(dest) => write_out(dest.hash().as_bytes(), buf, cap, out_len),
            None => {
                set_last_error("destination already registered");
                LEV_ERR_INVALID_ARG
            }
        }
    })
}

/// Register a destination on the node so it can be announced and can accept
/// links or packets. Consumes the destination (the handle is emptied; still
/// free it). `LEV_ERR_INVALID_ARG` if already registered.
#[no_mangle]
pub unsafe extern "C" fn lev_register_destination(
    node: *const leviculum_t,
    dest: *mut lev_destination_t,
) -> c_int {
    guard(LEV_ERR_PANIC, || {
        let h = match node.as_ref() {
            Some(h) => h,
            None => return LEV_ERR_NULL_PTR,
        };
        let d = match dest.as_mut() {
            Some(d) => d,
            None => return LEV_ERR_NULL_PTR,
        };
        match d.inner.take() {
            Some(destination) => {
                h.node().register_destination(destination);
                LEV_OK
            }
            None => {
                set_last_error("destination already registered");
                LEV_ERR_INVALID_ARG
            }
        }
    })
}

/// Announce a registered destination (16-byte hash) on all interfaces.
///
/// `app_data`/`app_data_len` is optional application payload (pass NULL/0 for
/// none). Blocks up to `timeout_ms` (negative means forever).
#[no_mangle]
pub unsafe extern "C" fn lev_announce(
    node: *const leviculum_t,
    dest_hash: *const u8,
    app_data: *const u8,
    app_data_len: usize,
    timeout_ms: c_int,
) -> c_int {
    guard(LEV_ERR_PANIC, || {
        let h = match node.as_ref() {
            Some(h) => h,
            None => return LEV_ERR_NULL_PTR,
        };
        if dest_hash.is_null() {
            return LEV_ERR_NULL_PTR;
        }
        let mut hash = [0u8; LEV_ADDR_LEN];
        std::ptr::copy_nonoverlapping(dest_hash, hash.as_mut_ptr(), LEV_ADDR_LEN);
        let dh = DestinationHash::new(hash);
        let data: Option<&[u8]> = if app_data.is_null() || app_data_len == 0 {
            None
        } else {
            Some(std::slice::from_raw_parts(app_data, app_data_len))
        };

        let fut = h.node().announce(&dh, data);
        let res = if timeout_ms < 0 {
            h.runtime().block_on(fut).map_err(Some)
        } else {
            let dur = Duration::from_millis(timeout_ms as u64);
            h.runtime()
                .block_on(async { tokio::time::timeout(dur, fut).await })
                .map_err(|_| None)
                .and_then(|r| r.map_err(Some))
        };
        match res {
            Ok(()) => LEV_OK,
            Err(Some(e)) => map_error(&e),
            Err(None) => LEV_ERR_TIMEOUT,
        }
    })
}

/// Send one unreliable datagram (single packet) to a destination (16-byte
/// hash), writing the 16-byte packet hash into `out_hash`. A path must already
/// be known (`LEV_ERR_NO_PATH` otherwise). Blocks up to `timeout_ms`.
#[no_mangle]
pub unsafe extern "C" fn lev_send_datagram(
    node: *const leviculum_t,
    dest_hash: *const u8,
    data: *const u8,
    data_len: usize,
    out_hash: *mut u8,
    timeout_ms: c_int,
) -> c_int {
    guard(LEV_ERR_PANIC, || {
        let h = match node.as_ref() {
            Some(h) => h,
            None => return LEV_ERR_NULL_PTR,
        };
        if dest_hash.is_null() || out_hash.is_null() {
            return LEV_ERR_NULL_PTR;
        }
        if data.is_null() && data_len > 0 {
            return LEV_ERR_NULL_PTR;
        }
        let dh = DestinationHash::new(read_array::<LEV_ADDR_LEN>(dest_hash));
        if !h.node().has_path(&dh) {
            set_last_error("no path to destination");
            return LEV_ERR_NO_PATH;
        }
        let slice: &[u8] = if data_len == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(data, data_len)
        };
        match block_on_timeout(h.runtime(), h.node().send_datagram(&dh, slice), timeout_ms) {
            Ok(Ok(hash)) => {
                std::ptr::copy_nonoverlapping(hash.as_ptr(), out_hash, LEV_ADDR_LEN);
                LEV_OK
            }
            Ok(Err(e)) => map_error(&e),
            Err(()) => LEV_ERR_TIMEOUT,
        }
    })
}
