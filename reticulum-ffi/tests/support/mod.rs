//! Shared helpers for the Rust-driven C API test suites.
//!
//! Each test file includes this with `mod support;`. The helpers wrap the
//! `unsafe extern "C"` `lev_*` functions into ergonomic, asserting Rust so the
//! test bodies stay readable. A subdir `mod.rs` is not compiled as its own test
//! binary.
#![allow(dead_code)]

use std::ffi::{CStr, CString};
use std::net::TcpListener;
use std::os::raw::{c_char, c_int};
use std::path::Path;
use std::ptr;
use std::time::{Duration, Instant};

use leviculum::*;

pub mod python_daemon;

/// Build a NUL-terminated C string (test input is never NUL-containing).
pub fn cstr(s: &str) -> CString {
    CString::new(s).expect("test string has no interior NUL")
}

/// The thread-local last-error detail, as a Rust string.
pub fn last_error() -> String {
    unsafe {
        let p = lev_last_error();
        if p.is_null() {
            return String::new();
        }
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

/// A free TCP port on loopback (bind to :0, read the port, release it).
pub fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = l.local_addr().expect("local_addr").port();
    drop(l);
    port
}

/// Owning wrapper for a node handle; frees on drop.
pub struct Node(pub *mut leviculum_t);

impl Drop for Node {
    fn drop(&mut self) {
        unsafe { lev_free(self.0) }
    }
}

impl Node {
    pub fn ptr(&self) -> *mut leviculum_t {
        self.0
    }
}

/// Owning wrapper for an event handle; frees on drop.
pub struct Event(pub *mut lev_event_t);

impl Drop for Event {
    fn drop(&mut self) {
        unsafe { lev_event_free(self.0) }
    }
}

impl Event {
    pub fn ty(&self) -> c_int {
        unsafe { lev_event_type(self.0) }
    }
}

/// Owning wrapper for a link handle; closes and frees on drop.
pub struct Link(pub *mut lev_link_t);

impl Drop for Link {
    fn drop(&mut self) {
        unsafe { lev_link_free(self.0) }
    }
}

impl Link {
    pub fn id(&self) -> [u8; 16] {
        let mut h = [0u8; 16];
        let mut l = 16usize;
        let rc = unsafe { lev_link_id(self.0, h.as_mut_ptr(), 16, &mut l) };
        assert_eq!(rc, LEV_OK);
        h
    }
}

/// Owning wrapper for an identity handle; frees on drop.
pub struct Identity(pub *mut lev_identity_t);

impl Drop for Identity {
    fn drop(&mut self) {
        unsafe { lev_identity_free(self.0) }
    }
}

impl Identity {
    pub fn generate() -> Identity {
        let p = lev_identity_generate();
        assert!(!p.is_null(), "identity_generate failed");
        Identity(p)
    }

    pub fn hash(&self) -> [u8; 16] {
        let mut h = [0u8; 16];
        let mut l = 16usize;
        let rc = unsafe { lev_identity_hash(self.0, h.as_mut_ptr(), 16, &mut l) };
        assert_eq!(rc, LEV_OK);
        h
    }
}

/// Build and start a node, configuring the builder via the closure (which adds
/// interfaces, an identity, etc.). Panics with the detail on failure.
pub fn start_node(storage: &Path, configure: impl FnOnce(*mut lev_builder_t)) -> Node {
    unsafe {
        let b = lev_builder_new();
        assert!(!b.is_null());
        let sp = cstr(storage.to_str().expect("utf-8 path"));
        assert_eq!(lev_builder_storage_path(b, sp.as_ptr()), LEV_OK);
        configure(b);
        let node = lev_builder_build(b);
        lev_builder_free(b);
        assert!(!node.is_null(), "build failed: {}", last_error());
        assert_eq!(lev_start(node), LEV_OK, "start failed: {}", last_error());
        Node(node)
    }
}

/// Register a new incoming SINGLE destination on `node` and return its 16-byte
/// hash. `identity` may be null for a node-default destination test, but is
/// normally the node's own identity.
pub fn register_single_dest(
    node: *mut leviculum_t,
    identity: *mut lev_identity_t,
    app_name: &str,
    aspects: &[&str],
) -> [u8; 16] {
    unsafe {
        let app_c = cstr(app_name);
        let aspect_cs: Vec<CString> = aspects.iter().map(|a| cstr(a)).collect();
        let aspect_ptrs: Vec<*const c_char> = aspect_cs.iter().map(|c| c.as_ptr()).collect();
        let dest = lev_destination_new(
            identity,
            LEV_DIRECTION_IN,
            LEV_DEST_SINGLE,
            app_c.as_ptr(),
            aspect_ptrs.as_ptr(),
            aspect_ptrs.len(),
        );
        assert!(!dest.is_null(), "destination_new: {}", last_error());
        let mut h = [0u8; 16];
        let mut l = 16usize;
        assert_eq!(
            lev_destination_hash(dest, h.as_mut_ptr(), 16, &mut l),
            LEV_OK
        );
        assert_eq!(lev_register_destination(node, dest), LEV_OK);
        lev_destination_free(dest);
        h
    }
}

/// Drain events on `node` up to `timeout`, returning the first event whose type
/// equals `want`. Non-matching events are freed. Returns `None` on timeout.
pub fn wait_event(node: *mut leviculum_t, want: c_int, timeout: Duration) -> Option<Event> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let ms = remaining.as_millis().min(200) as c_int;
        let mut ev: *mut lev_event_t = ptr::null_mut();
        let rc = unsafe { lev_wait_event(node, &mut ev, ms) };
        if rc != LEV_OK {
            return None;
        }
        if ev.is_null() {
            continue;
        }
        if unsafe { lev_event_type(ev) } == want {
            return Some(Event(ev));
        }
        unsafe { lev_event_free(ev) }
    }
}

/// Run a read(2)-style accessor closure, returning the bytes. The closure is
/// `|buf, cap, out_len| -> rc`. Performs a size query then a fill.
pub fn read2<F>(f: F) -> Result<Vec<u8>, c_int>
where
    F: Fn(*mut u8, usize, *mut usize) -> c_int,
{
    let mut len: usize = 0;
    let rc = f(ptr::null_mut(), 0, &mut len);
    if rc != LEV_ERR_BUFFER_TOO_SMALL && rc != LEV_OK {
        return Err(rc);
    }
    let mut buf = vec![0u8; len];
    let rc = f(buf.as_mut_ptr(), buf.len(), &mut len);
    if rc != LEV_OK {
        return Err(rc);
    }
    buf.truncate(len);
    Ok(buf)
}

/// Read an event's link id (16 bytes), or panic.
pub fn event_link_id(ev: &Event) -> [u8; 16] {
    let v = read2(|b, c, l| unsafe { lev_event_link_id(ev.0, b, c, l) }).expect("link id");
    v.try_into().expect("16 bytes")
}

/// Read an event's destination hash (16 bytes), or panic.
pub fn event_dest_hash(ev: &Event) -> [u8; 16] {
    let v = read2(|b, c, l| unsafe { lev_event_dest_hash(ev.0, b, c, l) }).expect("dest hash");
    v.try_into().expect("16 bytes")
}

/// Read an event's primary payload.
pub fn event_data(ev: &Event) -> Vec<u8> {
    read2(|b, c, l| unsafe { lev_event_data(ev.0, b, c, l) }).expect("event data")
}
