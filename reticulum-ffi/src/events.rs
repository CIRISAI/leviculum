//! The event bridge: a pollable eventfd over the engine's event stream.
//!
//! A bridge task drains the engine `EventReceiver`, projects each `NodeEvent`
//! to a self-owned [`lev_event_t`], and enqueues it. An eventfd in semaphore
//! mode mirrors the queue length so a C app can `poll`/`epoll` the fd and drain
//! with [`lev_next_event`]. The eventfd syscalls are done under the FIFO mutex
//! so the counter-equals-length invariant holds per instant. See
//! `docs/leviculum-api-design.md` §4.

use std::collections::VecDeque;
use std::os::raw::{c_int, c_void};
use std::os::unix::io::RawFd;
use std::sync::Mutex;

use reticulum_std::{EventClass, NodeEvent};

use crate::error::set_last_error;
use crate::{guard, write_out, LEV_ERR_INVALID_ARG, LEV_ERR_NULL_PTR, LEV_ERR_PANIC, LEV_OK};

/// Catch-all for events not yet projected with their own type and accessors.
pub const LEV_EVENT_OTHER: c_int = 0;
/// A validated announce was received from a peer.
pub const LEV_EVENT_ANNOUNCE_RECEIVED: c_int = 1;
/// A path to a requested destination was found.
pub const LEV_EVENT_PATH_FOUND: c_int = 2;
/// An incoming link request (accept with `lev_accept_link`).
pub const LEV_EVENT_LINK_REQUEST: c_int = 3;
/// A link handshake completed.
pub const LEV_EVENT_LINK_ESTABLISHED: c_int = 4;
/// A link closed.
pub const LEV_EVENT_LINK_CLOSED: c_int = 5;
/// Data arrived on a link.
pub const LEV_EVENT_LINK_DATA: c_int = 6;
/// A single packet (datagram) arrived on a destination.
pub const LEV_EVENT_PACKET_RECEIVED: c_int = 7;
/// Control events were dropped; the count is available via
/// `lev_event_dropped_count`.
pub const LEV_EVENT_CONTROL_OVERFLOW: c_int = 8;
/// A request arrived on a link (respond with `lev_send_response`).
pub const LEV_EVENT_REQUEST_RECEIVED: c_int = 9;
/// A response to a sent request arrived.
pub const LEV_EVENT_RESPONSE_RECEIVED: c_int = 10;
/// A sent request timed out without a response.
pub const LEV_EVENT_REQUEST_TIMEOUT: c_int = 11;
/// An incoming resource was advertised (accept or reject it).
pub const LEV_EVENT_RESOURCE_ADVERTISED: c_int = 12;
/// A resource transfer started.
pub const LEV_EVENT_RESOURCE_STARTED: c_int = 13;
/// Resource transfer progress (`lev_event_progress`).
pub const LEV_EVENT_RESOURCE_PROGRESS: c_int = 14;
/// A resource transfer completed (receiver gets data and metadata).
pub const LEV_EVENT_RESOURCE_COMPLETED: c_int = 15;
/// A resource transfer failed.
pub const LEV_EVENT_RESOURCE_FAILED: c_int = 16;
/// The peer proved an identity on a link; the 16-byte identity hash is the
/// event payload (`lev_event_data`), and `lev_link_remote_identity` returns it.
pub const LEV_EVENT_LINK_IDENTIFIED: c_int = 17;
/// A reliable, sequenced message arrived on a link's channel (the peer used
/// the channel, as `lev_link_send` does). Distinct from `LEV_EVENT_LINK_DATA`,
/// which is a raw unsequenced link packet. Carries a message type and a
/// sequence number via `lev_event_msgtype` and `lev_event_sequence`.
pub const LEV_EVENT_LINK_MESSAGE: c_int = 18;
/// A single packet arrived at a destination with the App proof strategy; the
/// app may call `lev_send_proof`. `dest_hash` is the destination and the data
/// payload is the 32-byte packet hash.
pub const LEV_EVENT_PACKET_PROOF_REQUESTED: c_int = 19;
/// Data arrived on a link whose destination has the App proof strategy. The
/// `link_id` is set and the data payload is the 32-byte packet hash.
pub const LEV_EVENT_LINK_PROOF_REQUESTED: c_int = 20;
/// A delivery proof confirmed a packet we sent on a link (PROVE_ALL). The
/// `link_id` is set and the data payload is the 32-byte packet hash.
pub const LEV_EVENT_LINK_DELIVERY_CONFIRMED: c_int = 21;
/// A link went inactive past its keepalive deadline; `link_id` is set. The link
/// is not closed yet (see `LEV_EVENT_LINK_RECOVERED` and `LEV_EVENT_LINK_CLOSED`).
pub const LEV_EVENT_LINK_STALE: c_int = 22;
/// A stale link resumed carrying traffic; `link_id` is set.
pub const LEV_EVENT_LINK_RECOVERED: c_int = 23;
/// A known path to a destination expired; `dest_hash` is set.
pub const LEV_EVENT_PATH_LOST: c_int = 24;
/// A delivery proof confirmed a single packet we sent; the data payload is the
/// 16-byte packet hash (as returned by `lev_send_datagram`).
pub const LEV_EVENT_PACKET_DELIVERY_CONFIRMED: c_int = 25;
/// Delivery of a single packet we sent failed; the data payload is the 16-byte
/// packet hash.
pub const LEV_EVENT_DELIVERY_FAILED: c_int = 26;

/// One projected event, fully self-owned (all payloads deep-copied out of the
/// `NodeEvent`), so it outlives the queue slot and is valid until
/// `lev_event_free`.
pub struct lev_event_t {
    ty: c_int,
    is_control: bool,
    link_id: Option<[u8; 16]>,
    dest_hash: Option<[u8; 16]>,
    request_id: Option<[u8; 16]>,
    resource_hash: Option<[u8; 32]>,
    path: Option<String>,
    data: Vec<u8>,
    metadata: Option<Vec<u8>>,
    progress: f64,
    dropped_count: u64,
    msgtype: u16,
    sequence: u16,
    is_sender: bool,
}

impl lev_event_t {
    fn bare(ty: c_int, is_control: bool) -> Self {
        Self {
            ty,
            is_control,
            link_id: None,
            dest_hash: None,
            request_id: None,
            resource_hash: None,
            path: None,
            data: Vec::new(),
            metadata: None,
            progress: 0.0,
            dropped_count: 0,
            msgtype: 0,
            sequence: 0,
            is_sender: false,
        }
    }
}

/// Project a `NodeEvent` to a self-owned [`lev_event_t`].
///
/// The class (control or data) is taken from `event_class` for every variant so
/// the queue's per-plane policy is correct even for variants still mapped to
/// [`LEV_EVENT_OTHER`]. Richer per-type projection lands with later phases.
fn project(ev: NodeEvent) -> lev_event_t {
    let is_control = matches!(ev.event_class(), EventClass::Control);
    match ev {
        NodeEvent::AnnounceReceived { announce, .. } => {
            let mut e = lev_event_t::bare(LEV_EVENT_ANNOUNCE_RECEIVED, is_control);
            e.dest_hash = Some(*announce.destination_hash().as_bytes());
            e.data = announce.app_data().to_vec();
            e
        }
        NodeEvent::PathFound {
            destination_hash, ..
        } => {
            let mut e = lev_event_t::bare(LEV_EVENT_PATH_FOUND, is_control);
            e.dest_hash = Some(*destination_hash.as_bytes());
            e
        }
        NodeEvent::ControlPlaneOverflow { dropped_count } => {
            let mut e = lev_event_t::bare(LEV_EVENT_CONTROL_OVERFLOW, is_control);
            e.dropped_count = dropped_count;
            e
        }
        NodeEvent::LinkRequest {
            link_id,
            destination_hash,
            ..
        } => {
            let mut e = lev_event_t::bare(LEV_EVENT_LINK_REQUEST, is_control);
            e.link_id = Some(*link_id.as_bytes());
            e.dest_hash = Some(*destination_hash.as_bytes());
            e
        }
        NodeEvent::LinkEstablished { link_id, .. } => {
            let mut e = lev_event_t::bare(LEV_EVENT_LINK_ESTABLISHED, is_control);
            e.link_id = Some(*link_id.as_bytes());
            e
        }
        NodeEvent::LinkClosed { link_id, .. } => {
            let mut e = lev_event_t::bare(LEV_EVENT_LINK_CLOSED, is_control);
            e.link_id = Some(*link_id.as_bytes());
            e
        }
        NodeEvent::LinkIdentified {
            link_id,
            identity_hash,
        } => {
            let mut e = lev_event_t::bare(LEV_EVENT_LINK_IDENTIFIED, is_control);
            e.link_id = Some(*link_id.as_bytes());
            e.data = identity_hash.to_vec();
            e
        }
        NodeEvent::LinkDataReceived { link_id, data } => {
            let mut e = lev_event_t::bare(LEV_EVENT_LINK_DATA, is_control);
            e.link_id = Some(*link_id.as_bytes());
            e.data = data;
            e
        }
        NodeEvent::MessageReceived {
            link_id,
            msgtype,
            sequence,
            data,
        } => {
            let mut e = lev_event_t::bare(LEV_EVENT_LINK_MESSAGE, is_control);
            e.link_id = Some(*link_id.as_bytes());
            e.data = data;
            e.msgtype = msgtype;
            e.sequence = sequence;
            e
        }
        NodeEvent::PacketProofRequested {
            packet_hash,
            destination_hash,
        } => {
            let mut e = lev_event_t::bare(LEV_EVENT_PACKET_PROOF_REQUESTED, is_control);
            e.dest_hash = Some(*destination_hash.as_bytes());
            e.data = packet_hash.to_vec();
            e
        }
        NodeEvent::LinkProofRequested {
            link_id,
            packet_hash,
        } => {
            let mut e = lev_event_t::bare(LEV_EVENT_LINK_PROOF_REQUESTED, is_control);
            e.link_id = Some(*link_id.as_bytes());
            e.data = packet_hash.to_vec();
            e
        }
        NodeEvent::LinkDeliveryConfirmed {
            link_id,
            packet_hash,
        } => {
            let mut e = lev_event_t::bare(LEV_EVENT_LINK_DELIVERY_CONFIRMED, is_control);
            e.link_id = Some(*link_id.as_bytes());
            e.data = packet_hash.to_vec();
            e
        }
        NodeEvent::LinkStale { link_id } => {
            let mut e = lev_event_t::bare(LEV_EVENT_LINK_STALE, is_control);
            e.link_id = Some(*link_id.as_bytes());
            e
        }
        NodeEvent::LinkRecovered { link_id } => {
            let mut e = lev_event_t::bare(LEV_EVENT_LINK_RECOVERED, is_control);
            e.link_id = Some(*link_id.as_bytes());
            e
        }
        NodeEvent::PathLost { destination_hash } => {
            let mut e = lev_event_t::bare(LEV_EVENT_PATH_LOST, is_control);
            e.dest_hash = Some(*destination_hash.as_bytes());
            e
        }
        NodeEvent::PacketDeliveryConfirmed { packet_hash } => {
            let mut e = lev_event_t::bare(LEV_EVENT_PACKET_DELIVERY_CONFIRMED, is_control);
            e.data = packet_hash.to_vec();
            e
        }
        NodeEvent::DeliveryFailed { packet_hash, .. } => {
            let mut e = lev_event_t::bare(LEV_EVENT_DELIVERY_FAILED, is_control);
            e.data = packet_hash.to_vec();
            e
        }
        NodeEvent::PacketReceived {
            destination, data, ..
        } => {
            let mut e = lev_event_t::bare(LEV_EVENT_PACKET_RECEIVED, is_control);
            e.dest_hash = Some(*destination.as_bytes());
            e.data = data;
            e
        }
        NodeEvent::RequestReceived {
            link_id,
            request_id,
            path,
            data,
            ..
        } => {
            let mut e = lev_event_t::bare(LEV_EVENT_REQUEST_RECEIVED, is_control);
            e.link_id = Some(*link_id.as_bytes());
            e.request_id = Some(request_id);
            e.path = Some(path);
            e.data = data;
            e
        }
        NodeEvent::ResponseReceived {
            link_id,
            request_id,
            response_data,
        } => {
            let mut e = lev_event_t::bare(LEV_EVENT_RESPONSE_RECEIVED, is_control);
            e.link_id = Some(*link_id.as_bytes());
            e.request_id = Some(request_id);
            e.data = response_data;
            e
        }
        NodeEvent::RequestTimedOut {
            link_id,
            request_id,
        } => {
            let mut e = lev_event_t::bare(LEV_EVENT_REQUEST_TIMEOUT, is_control);
            e.link_id = Some(*link_id.as_bytes());
            e.request_id = Some(request_id);
            e
        }
        NodeEvent::ResourceAdvertised {
            link_id,
            resource_hash,
            ..
        } => {
            let mut e = lev_event_t::bare(LEV_EVENT_RESOURCE_ADVERTISED, is_control);
            e.link_id = Some(*link_id.as_bytes());
            e.resource_hash = Some(resource_hash);
            e
        }
        NodeEvent::ResourceTransferStarted {
            link_id,
            resource_hash,
            ..
        } => {
            let mut e = lev_event_t::bare(LEV_EVENT_RESOURCE_STARTED, is_control);
            e.link_id = Some(*link_id.as_bytes());
            e.resource_hash = Some(resource_hash);
            e
        }
        NodeEvent::ResourceProgress {
            link_id,
            resource_hash,
            progress,
            is_sender,
            ..
        } => {
            let mut e = lev_event_t::bare(LEV_EVENT_RESOURCE_PROGRESS, is_control);
            e.link_id = Some(*link_id.as_bytes());
            e.resource_hash = Some(resource_hash);
            e.progress = progress as f64;
            e.is_sender = is_sender;
            e
        }
        NodeEvent::ResourceCompleted {
            link_id,
            resource_hash,
            data,
            metadata,
            is_sender,
            ..
        } => {
            let mut e = lev_event_t::bare(LEV_EVENT_RESOURCE_COMPLETED, is_control);
            e.link_id = Some(*link_id.as_bytes());
            e.resource_hash = Some(resource_hash);
            e.data = data;
            e.metadata = metadata;
            e.is_sender = is_sender;
            e
        }
        NodeEvent::ResourceFailed {
            link_id,
            resource_hash,
            is_sender,
            ..
        } => {
            let mut e = lev_event_t::bare(LEV_EVENT_RESOURCE_FAILED, is_control);
            e.link_id = Some(*link_id.as_bytes());
            e.resource_hash = Some(resource_hash);
            e.is_sender = is_sender;
            e
        }
        // Other variants keep their class so the cap policy is right, but carry
        // no typed fields yet.
        _ => lev_event_t::bare(LEV_EVENT_OTHER, is_control),
    }
}

/// Mutable, lock-guarded bridge state.
struct BridgeState {
    queue: VecDeque<Box<lev_event_t>>,
    control_len: usize,
    data_len: usize,
    /// Control events dropped since the last overflow marker was enqueued.
    control_dropped: u64,
}

/// The event bridge shared between the drain task (producer) and
/// `lev_next_event` (consumer).
pub(crate) struct EventBridge {
    fd: RawFd,
    state: Mutex<BridgeState>,
    control_cap: usize,
    data_cap: usize,
}

// SAFETY: `fd` is a plain integer used only via kernel-atomic eventfd syscalls,
// always under `state`'s lock; the rest is `Send`/`Sync` by composition.
unsafe impl Send for EventBridge {}
unsafe impl Sync for EventBridge {}

/// Increment the eventfd counter by 1. Called under the state lock.
fn fd_write(fd: RawFd) {
    let v: u64 = 1;
    // SAFETY: writing 8 bytes of a u64 to an eventfd is the documented contract.
    unsafe {
        libc::write(fd, &v as *const u64 as *const c_void, 8);
    }
}

/// Decrement the eventfd counter by 1 (semaphore mode). Called under the state
/// lock only after a successful pop, so the counter is `>= 1` and `read` does
/// not block; a spurious `EAGAIN` is tolerated, never treated as an error.
fn fd_read(fd: RawFd) {
    let mut v: u64 = 0;
    // SAFETY: reading 8 bytes from an eventfd into a u64 is the documented
    // contract; the fd is non-blocking so this never blocks.
    unsafe {
        libc::read(fd, &mut v as *mut u64 as *mut c_void, 8);
    }
}

impl EventBridge {
    pub(crate) fn new(control_cap: usize, data_cap: usize) -> std::io::Result<Self> {
        // SAFETY: eventfd with these flags returns a new fd or -1.
        let fd = unsafe {
            libc::eventfd(
                0,
                libc::EFD_SEMAPHORE | libc::EFD_NONBLOCK | libc::EFD_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            fd,
            state: Mutex::new(BridgeState {
                queue: VecDeque::new(),
                control_len: 0,
                data_len: 0,
                control_dropped: 0,
            }),
            control_cap,
            data_cap,
        })
    }

    pub(crate) fn fd(&self) -> RawFd {
        self.fd
    }

    /// If control events were dropped and there is now room, enqueue one
    /// coalesced overflow marker reporting the count. Called under the lock.
    fn flush_overflow(&self, state: &mut BridgeState) {
        if state.control_dropped == 0 || state.control_len >= self.control_cap {
            return;
        }
        let mut marker = lev_event_t::bare(LEV_EVENT_CONTROL_OVERFLOW, true);
        marker.dropped_count = state.control_dropped;
        state.queue.push_back(Box::new(marker));
        state.control_len += 1;
        state.control_dropped = 0;
        fd_write(self.fd);
    }

    /// Enqueue one projected event, applying the per-plane cap at enqueue so a
    /// dropped event is never counted and never writes the fd.
    pub(crate) fn enqueue(&self, ev: Box<lev_event_t>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if ev.is_control {
            if state.control_len >= self.control_cap {
                // Lossless-by-default: record the loss, surfaced via a marker.
                state.control_dropped += 1;
            } else {
                state.queue.push_back(ev);
                state.control_len += 1;
                fd_write(self.fd);
            }
        } else if state.data_len < self.data_cap {
            state.queue.push_back(ev);
            state.data_len += 1;
            fd_write(self.fd);
        }
        // else: data region full, drop the incoming event (backpressure).
        self.flush_overflow(&mut state);
    }

    /// Pop one event, decrementing the eventfd counter under the lock. Returns
    /// `None` when the queue is empty.
    pub(crate) fn next(&self) -> Option<Box<lev_event_t>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let ev = state.queue.pop_front();
        if let Some(ref e) = ev {
            if e.is_control {
                state.control_len -= 1;
            } else {
                state.data_len -= 1;
            }
            fd_read(self.fd);
        }
        // Room may have appeared for a pending overflow marker.
        self.flush_overflow(&mut state);
        ev
    }
}

impl Drop for EventBridge {
    fn drop(&mut self) {
        // SAFETY: `fd` is owned by this bridge and closed exactly once.
        unsafe {
            libc::close(self.fd);
        }
    }
}

/// Drain task: project and enqueue every event until the channels close.
pub(crate) async fn run_bridge(
    mut rx: reticulum_std::EventReceiver,
    bridge: std::sync::Arc<EventBridge>,
) {
    while let Some(ev) = rx.recv().await {
        bridge.enqueue(Box::new(project(ev)));
    }
}

// --- C accessors on a drained event handle ---

/// The event's type, one of the `LEV_EVENT_*` constants (0 on NULL).
#[no_mangle]
pub unsafe extern "C" fn lev_event_type(ev: *const lev_event_t) -> c_int {
    guard(LEV_EVENT_OTHER, || match ev.as_ref() {
        Some(e) => e.ty,
        None => LEV_EVENT_OTHER,
    })
}

/// Write the event's link id (16 bytes) into `buf`, read(2) style.
/// `LEV_ERR_INVALID_ARG` if the event has no link id.
#[no_mangle]
pub unsafe extern "C" fn lev_event_link_id(
    ev: *const lev_event_t,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> c_int {
    guard(LEV_ERR_PANIC, || {
        let e = match ev.as_ref() {
            Some(e) => e,
            None => return LEV_ERR_NULL_PTR,
        };
        match &e.link_id {
            Some(id) => write_out(id, buf, cap, out_len),
            None => LEV_ERR_INVALID_ARG,
        }
    })
}

/// Write the event's destination hash (16 bytes) into `buf`, read(2) style.
/// `LEV_ERR_INVALID_ARG` if the event has no destination hash.
#[no_mangle]
pub unsafe extern "C" fn lev_event_dest_hash(
    ev: *const lev_event_t,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> c_int {
    guard(LEV_ERR_PANIC, || {
        let e = match ev.as_ref() {
            Some(e) => e,
            None => return LEV_ERR_NULL_PTR,
        };
        match &e.dest_hash {
            Some(h) => write_out(h, buf, cap, out_len),
            None => LEV_ERR_INVALID_ARG,
        }
    })
}

/// Write the event's request id (16 bytes) into `buf`, read(2) style.
/// `LEV_ERR_INVALID_ARG` if the event has no request id.
#[no_mangle]
pub unsafe extern "C" fn lev_event_request_id(
    ev: *const lev_event_t,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> c_int {
    guard(LEV_ERR_PANIC, || {
        let e = match ev.as_ref() {
            Some(e) => e,
            None => return LEV_ERR_NULL_PTR,
        };
        match &e.request_id {
            Some(id) => write_out(id, buf, cap, out_len),
            None => LEV_ERR_INVALID_ARG,
        }
    })
}

/// Write the event's request path into `buf` as UTF-8 bytes (not
/// NUL-terminated), read(2) style. `LEV_ERR_INVALID_ARG` if the event has no
/// path.
#[no_mangle]
pub unsafe extern "C" fn lev_event_path(
    ev: *const lev_event_t,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> c_int {
    guard(LEV_ERR_PANIC, || {
        let e = match ev.as_ref() {
            Some(e) => e,
            None => return LEV_ERR_NULL_PTR,
        };
        match &e.path {
            Some(p) => write_out(p.as_bytes(), buf, cap, out_len),
            None => LEV_ERR_INVALID_ARG,
        }
    })
}

/// Write the event's primary payload into `buf`, read(2) style. The payload may
/// be empty (sets `*out_len` to 0).
#[no_mangle]
pub unsafe extern "C" fn lev_event_data(
    ev: *const lev_event_t,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> c_int {
    guard(LEV_ERR_PANIC, || {
        let e = match ev.as_ref() {
            Some(e) => e,
            None => return LEV_ERR_NULL_PTR,
        };
        write_out(&e.data, buf, cap, out_len)
    })
}

/// Write the event's resource hash (32 bytes) into `buf`, read(2) style.
/// `LEV_ERR_INVALID_ARG` if the event has no resource hash.
#[no_mangle]
pub unsafe extern "C" fn lev_event_resource_hash(
    ev: *const lev_event_t,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> c_int {
    guard(LEV_ERR_PANIC, || {
        let e = match ev.as_ref() {
            Some(e) => e,
            None => return LEV_ERR_NULL_PTR,
        };
        match &e.resource_hash {
            Some(h) => write_out(h, buf, cap, out_len),
            None => LEV_ERR_INVALID_ARG,
        }
    })
}

/// Write the event's metadata (msgpack bytes) into `buf`, read(2) style.
/// `LEV_ERR_INVALID_ARG` if the event has no metadata.
#[no_mangle]
pub unsafe extern "C" fn lev_event_metadata(
    ev: *const lev_event_t,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> c_int {
    guard(LEV_ERR_PANIC, || {
        let e = match ev.as_ref() {
            Some(e) => e,
            None => return LEV_ERR_NULL_PTR,
        };
        match &e.metadata {
            Some(m) => write_out(m, buf, cap, out_len),
            None => LEV_ERR_INVALID_ARG,
        }
    })
}

/// Write the transfer progress (0.0..1.0) of a `LEV_EVENT_RESOURCE_PROGRESS`
/// event into `*out`. `LEV_ERR_INVALID_ARG` for any other event type.
#[no_mangle]
pub unsafe extern "C" fn lev_event_progress(ev: *const lev_event_t, out: *mut f64) -> c_int {
    guard(LEV_ERR_PANIC, || {
        let e = match ev.as_ref() {
            Some(e) => e,
            None => return LEV_ERR_NULL_PTR,
        };
        if out.is_null() {
            return LEV_ERR_NULL_PTR;
        }
        if e.ty != LEV_EVENT_RESOURCE_PROGRESS {
            set_last_error("event has no progress");
            return LEV_ERR_INVALID_ARG;
        }
        *out = e.progress;
        LEV_OK
    })
}

/// Read the dropped-event count of a `LEV_EVENT_CONTROL_OVERFLOW` event.
/// `LEV_ERR_INVALID_ARG` for any other event type.
#[no_mangle]
pub unsafe extern "C" fn lev_event_dropped_count(ev: *const lev_event_t, out: *mut u64) -> c_int {
    guard(LEV_ERR_PANIC, || {
        let e = match ev.as_ref() {
            Some(e) => e,
            None => return LEV_ERR_NULL_PTR,
        };
        if out.is_null() {
            return LEV_ERR_NULL_PTR;
        }
        if e.ty != LEV_EVENT_CONTROL_OVERFLOW {
            set_last_error("event has no dropped count");
            return LEV_ERR_INVALID_ARG;
        }
        *out = e.dropped_count;
        LEV_OK
    })
}

/// Whether a resource event is for a transfer this node is *sending*. Returns 1
/// on the sender side of a `LEV_EVENT_RESOURCE_PROGRESS`/`_COMPLETED`/`_FAILED`
/// event, 0 on the receiver side (and 0 for other events or a NULL pointer).
///
/// A sender's `LEV_EVENT_RESOURCE_COMPLETED` is the signal that an outgoing
/// transfer finished (its data payload is empty); a receiver's carries the
/// assembled data. Use this to tell the two apart on a node that both sends and
/// receives resources.
#[no_mangle]
pub unsafe extern "C" fn lev_event_is_sender(ev: *const lev_event_t) -> c_int {
    guard(0, || match ev.as_ref() {
        Some(e) if e.is_sender => 1,
        _ => 0,
    })
}

/// Read the message type of a `LEV_EVENT_LINK_MESSAGE` event into `*out`. The
/// type identifies the channel message kind on the wire (0 is the raw bytes
/// message that `lev_link_send` uses and that Python's `RawBytesMessage`
/// carries). `LEV_ERR_INVALID_ARG` for any other event type.
#[no_mangle]
pub unsafe extern "C" fn lev_event_msgtype(ev: *const lev_event_t, out: *mut u16) -> c_int {
    guard(LEV_ERR_PANIC, || {
        let e = match ev.as_ref() {
            Some(e) => e,
            None => return LEV_ERR_NULL_PTR,
        };
        if out.is_null() {
            return LEV_ERR_NULL_PTR;
        }
        if e.ty != LEV_EVENT_LINK_MESSAGE {
            set_last_error("event has no message type");
            return LEV_ERR_INVALID_ARG;
        }
        *out = e.msgtype;
        LEV_OK
    })
}

/// Read the sequence number of a `LEV_EVENT_LINK_MESSAGE` event into `*out`.
/// The channel assigns sequence numbers in send order for reliable, ordered
/// delivery. `LEV_ERR_INVALID_ARG` for any other event type.
#[no_mangle]
pub unsafe extern "C" fn lev_event_sequence(ev: *const lev_event_t, out: *mut u16) -> c_int {
    guard(LEV_ERR_PANIC, || {
        let e = match ev.as_ref() {
            Some(e) => e,
            None => return LEV_ERR_NULL_PTR,
        };
        if out.is_null() {
            return LEV_ERR_NULL_PTR;
        }
        if e.ty != LEV_EVENT_LINK_MESSAGE {
            set_last_error("event has no sequence number");
            return LEV_ERR_INVALID_ARG;
        }
        *out = e.sequence;
        LEV_OK
    })
}

/// Free an event handle returned by `lev_next_event`/`lev_wait_event`.
/// `lev_event_free(NULL)` is a no-op.
#[no_mangle]
pub unsafe extern "C" fn lev_event_free(ev: *mut lev_event_t) {
    guard((), || {
        if !ev.is_null() {
            drop(Box::from_raw(ev));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::raw::c_int as poll_c_int;

    /// Is the eventfd readable right now (poll with zero timeout)?
    fn readable(fd: RawFd) -> bool {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: single valid pollfd, zero timeout.
        let n = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, 0 as poll_c_int) };
        n > 0 && (pfd.revents & libc::POLLIN) != 0
    }

    fn ev(ty: c_int, control: bool) -> Box<lev_event_t> {
        Box::new(lev_event_t::bare(ty, control))
    }

    #[test]
    fn fd_tracks_queue_length() {
        let b = EventBridge::new(8, 8).unwrap();
        assert!(!readable(b.fd()));
        b.enqueue(ev(LEV_EVENT_OTHER, true));
        assert!(readable(b.fd()));
        b.enqueue(ev(LEV_EVENT_OTHER, false));
        assert!(readable(b.fd()));
        assert!(b.next().is_some());
        assert!(readable(b.fd())); // one still queued
        assert!(b.next().is_some());
        assert!(!readable(b.fd())); // drained
        assert!(b.next().is_none());
        assert!(!readable(b.fd()));
    }

    #[test]
    fn data_region_drops_incoming_when_full() {
        let b = EventBridge::new(8, 2).unwrap();
        b.enqueue(ev(LEV_EVENT_OTHER, false));
        b.enqueue(ev(LEV_EVENT_OTHER, false));
        b.enqueue(ev(LEV_EVENT_OTHER, false)); // dropped, never counted
        assert!(b.next().is_some());
        assert!(b.next().is_some());
        assert!(b.next().is_none()); // only two were ever queued
        assert!(!readable(b.fd()));
    }

    #[test]
    fn control_overflow_surfaces_a_marker() {
        let b = EventBridge::new(2, 8).unwrap();
        b.enqueue(ev(LEV_EVENT_OTHER, true));
        b.enqueue(ev(LEV_EVENT_OTHER, true));
        b.enqueue(ev(LEV_EVENT_OTHER, true)); // over cap, recorded as dropped
        b.enqueue(ev(LEV_EVENT_OTHER, true)); // still over cap, dropped count 2

        // Drain the two queued control events; room appears and the marker is
        // flushed with the dropped count.
        let a = b.next().unwrap();
        assert_eq!(a.ty, LEV_EVENT_OTHER);
        let c = b.next().unwrap();
        assert_eq!(c.ty, LEV_EVENT_OTHER);
        let marker = b.next().unwrap();
        assert_eq!(marker.ty, LEV_EVENT_CONTROL_OVERFLOW);
        assert_eq!(marker.dropped_count, 2);
        assert!(b.next().is_none());
        assert!(!readable(b.fd()));
    }

    #[test]
    fn flood_preserves_invariant() {
        let b = EventBridge::new(64, 64).unwrap();
        for i in 0..1000 {
            b.enqueue(ev(LEV_EVENT_OTHER, i % 2 == 0));
        }
        let mut drained = 0;
        while b.next().is_some() {
            drained += 1;
        }
        // Caps bound the queue; nothing readable after a full drain.
        assert!(drained <= 64 + 64 + 1); // + possible overflow marker
        assert!(!readable(b.fd()));
    }
}
