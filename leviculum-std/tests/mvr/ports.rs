//! Ports for the mvr tier: who picks the number, and when.
//!
//! A port number picked in advance and bound later is a handoff, and in the
//! window between the two anything on the host may take it. Codeberg nightly
//! #509 (2026-10-11, 3032da10) went red on exactly that: the silent-resume
//! mvr drew 61455 from the shared allocator, whose probe bind found it free,
//! and the proxy's own bind of the same number a few instructions later was
//! refused with `EADDRINUSE`. Pipelines #473 (61322) and #486 (61450) had
//! failed the same way, always that proxy and never a held port: a port held
//! for the whole run is skipped by the probe, which is why the first
//! reproduction (`nc -l` on the number for the whole run) stays green.
//!
//! So the rule here is that the consumer binds, and the kernel picks:
//!
//! * a listener the test owns (a proxy, a stub) binds [`LOOPBACK_ANY_PORT`]
//!   through [`bind_loopback_tcp`] and hands its `local_addr()` on;
//! * an in-process node's TCP server is built with [`LOOPBACK_ANY_PORT`] and
//!   its port is read back with [`bound_tcp_addr`] after `start()`. The bind
//!   is synchronous inside the TCP server interface's spawn, so the address
//!   is in the node's inventory before `start()` returns.
//!
//! Neither leaves a window: the number exists only once something holds it.
//!
//! [`free_port`] is what remains for a consumer that cannot report back: a
//! daemon in another process, whose port has to be written into its config
//! file before it starts. The race between the pick and the daemon's bind is
//! accepted there and named at each call site with a `port-handoff:` comment,
//! which `scripts/check-test-ports.py` (`just check-test-ports`) requires.
//! So is the one in-process exception, a client that must keep dialling a
//! port after its server is gone: a kernel-picked port lies in
//! `ip_local_port_range`, where the client's own next ephemeral port can be
//! that very number and the dial connects to itself.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener};

/// `127.0.0.1:0`: loopback, port chosen by the kernel at bind time.
pub const LOOPBACK_ANY_PORT: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));

/// A loopback TCP listener on a port the kernel picked, already listening.
///
/// The caller reads the port off `local_addr()`; nothing else can hold it,
/// because the number did not exist before this bind.
pub fn bind_loopback_tcp() -> TcpListener {
    TcpListener::bind(LOOPBACK_ANY_PORT)
        .unwrap_or_else(|e| panic!("bind {LOOPBACK_ANY_PORT} failed: {e}"))
}

/// The one TCP server address a started node reports, for a node built with
/// `add_tcp_server(LOOPBACK_ANY_PORT)`.
pub fn bound_tcp_addr(listen_addrs: &[SocketAddr]) -> SocketAddr {
    match listen_addrs {
        [addr] => *addr,
        other => panic!(
            "expected the node to report exactly one bound TCP listener after \
             start(), got {other:?}"
        ),
    }
}

/// A port number for a consumer that binds it later, outside this process.
///
/// From the host-wide counter in `crate::harness::port_alloc`, above the
/// kernel's ephemeral range and probed free at the time of the call. Between
/// this call and the consumer's bind the number is unprotected: the counter
/// keeps every other drawer off it, nothing keeps out a socket that never
/// asked the counter. Accepted, and only where the consumer cannot bind
/// [`LOOPBACK_ANY_PORT`] and report back; every call site says why in a
/// `port-handoff:` comment.
pub fn free_port() -> u16 {
    crate::harness::port_alloc::free_tcp_port()
}
