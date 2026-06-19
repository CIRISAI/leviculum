//! Interop tests: a C API node against a real Python Reticulum daemon over TCP
//! loopback. They prove the FFI's projection and marshalling keep the wire and
//! semantic compatibility with the reference implementation. If Python RNS is
//! not available the daemon does not ready and each test skips.

mod support;

use std::ptr;
use std::time::Duration;

use leviculum::*;
use support::python_daemon::PyDaemon;
use support::{
    cstr, event_data, event_dest_hash, last_error, register_single_dest, start_node, wait_event,
    Identity, Link,
};

/// Build a started C node that is a TCP client of the daemon.
fn c_node(
    py: &PyDaemon,
    dir: &std::path::Path,
    identity: Option<*mut lev_identity_t>,
) -> support::Node {
    let addr = cstr(&py.rns_addr());
    let server_ptr = addr.as_ptr();
    let id = identity.unwrap_or(ptr::null_mut());
    start_node(dir, |b| unsafe {
        if !id.is_null() {
            assert_eq!(lev_builder_identity(b, id), LEV_OK);
        }
        assert_eq!(lev_builder_add_tcp_client(b, server_ptr), LEV_OK);
    })
}

#[test]
fn c_announce_seen_by_python() {
    let Some(py) = PyDaemon::start() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let ida = Identity::generate();
    let node = c_node(&py, dir.path(), Some(ida.0));
    let dest = register_single_dest(node.0, ida.0, "interop", &["c2py"]);
    let dest_hex = hex::encode(dest);

    let mut ok = false;
    for _ in 0..30 {
        unsafe { lev_announce(node.0, dest.as_ptr(), ptr::null(), 0, 2000) };
        std::thread::sleep(Duration::from_millis(400));
        if py.has_path(&dest_hex) {
            ok = true;
            break;
        }
    }
    assert!(ok, "Python never saw a path to the C destination");
}

#[test]
fn python_announce_seen_by_c() {
    let Some(py) = PyDaemon::start() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let node = c_node(&py, dir.path(), None);
    let (phash, _sk) = py.register_destination("interop", &["py2c"]);
    let want: [u8; 16] = hex::decode(&phash).unwrap().try_into().unwrap();

    let mut seen = false;
    for _ in 0..30 {
        py.announce_destination(&phash, "");
        if let Some(ev) = wait_event(
            node.0,
            LEV_EVENT_ANNOUNCE_RECEIVED,
            Duration::from_millis(800),
        ) {
            if event_dest_hash(&ev) == want {
                seen = true;
                break;
            }
        }
    }
    assert!(seen, "C never saw the Python announce");
}

#[test]
fn c_links_to_python_and_exchanges_data() {
    let Some(py) = PyDaemon::start() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let node = c_node(&py, dir.path(), None);
    let (phash, _sk) = py.register_destination("interop", &["link"]);
    let dest: [u8; 16] = hex::decode(&phash).unwrap().try_into().unwrap();

    // Learn the Python destination from its announce.
    let mut learned = false;
    for _ in 0..30 {
        py.announce_destination(&phash, "");
        let _ = wait_event(
            node.0,
            LEV_EVENT_ANNOUNCE_RECEIVED,
            Duration::from_millis(800),
        );
        if unsafe { lev_has_path(node.0, dest.as_ptr()) } == 1 {
            learned = true;
            break;
        }
    }
    assert!(learned, "C never learned the Python destination");

    // Connect and wait for establishment.
    let mut lb: *mut lev_link_t = ptr::null_mut();
    assert_eq!(
        unsafe { lev_connect(node.0, dest.as_ptr(), 6000, &mut lb) },
        LEV_OK,
        "connect: {}",
        last_error()
    );
    let lb = Link(lb);
    wait_event(node.0, LEV_EVENT_LINK_ESTABLISHED, Duration::from_secs(8))
        .expect("C link to Python established");

    // C -> Python link data.
    let msg = b"from-c";
    assert_eq!(
        unsafe { lev_link_send(lb.0, msg.as_ptr(), 6, 5000) },
        LEV_OK
    );
    let want_hex = hex::encode(msg);
    let mut got = false;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(300));
        if py.received_link_packets().iter().any(|h| h == &want_hex) {
            got = true;
            break;
        }
    }
    assert!(got, "Python did not receive the C link data");

    // Python -> C link data. send_on_link sends a raw RNS.Packet (no channel),
    // so C sees an unsequenced LINK_DATA, distinct from a channel message.
    let links = py.link_hashes();
    assert!(!links.is_empty(), "Python has no link recorded");
    py.send_on_link(&links[0], &hex::encode(b"from-py"));
    let ev = wait_event(node.0, LEV_EVENT_LINK_DATA, Duration::from_secs(8))
        .expect("C receives Python link data");
    assert_eq!(event_data(&ev), b"from-py");
}

/// The reliable channel interoperates with Python's `RawBytesMessage`. With
/// `--echo-channel` the daemon echoes every channel message back over the
/// channel, so a C `lev_link_send` returns as a sequenced LINK_MESSAGE with
/// msgtype 0 (the raw-bytes message both sides agree on).
#[test]
fn c_channel_message_interops_with_python() {
    let Some(py) = PyDaemon::start_with_args(&["--echo-channel"]) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let node = c_node(&py, dir.path(), None);
    let (phash, _sk) = py.register_destination("interop", &["channel"]);
    let dest: [u8; 16] = hex::decode(&phash).unwrap().try_into().unwrap();

    let mut learned = false;
    for _ in 0..30 {
        py.announce_destination(&phash, "");
        let _ = wait_event(
            node.0,
            LEV_EVENT_ANNOUNCE_RECEIVED,
            Duration::from_millis(800),
        );
        if unsafe { lev_has_path(node.0, dest.as_ptr()) } == 1 {
            learned = true;
            break;
        }
    }
    assert!(learned, "C never learned the Python destination");

    let mut lb: *mut lev_link_t = ptr::null_mut();
    assert_eq!(
        unsafe { lev_connect(node.0, dest.as_ptr(), 6000, &mut lb) },
        LEV_OK,
        "connect: {}",
        last_error()
    );
    let lb = Link(lb);
    wait_event(node.0, LEV_EVENT_LINK_ESTABLISHED, Duration::from_secs(8))
        .expect("C link to Python established");

    // C sends a channel message; Python echoes it back over the channel.
    let msg = b"channel-hi";
    assert_eq!(
        unsafe { lev_link_send(lb.0, msg.as_ptr(), msg.len(), 5000) },
        LEV_OK
    );
    let ev = wait_event(node.0, LEV_EVENT_LINK_MESSAGE, Duration::from_secs(8))
        .expect("C receives Python channel echo");
    assert_eq!(event_data(&ev), msg);
    let mut msgtype = 0u16;
    unsafe { assert_eq!(lev_event_msgtype(ev.0, &mut msgtype), LEV_OK) };
    assert_eq!(msgtype, 0, "Python RawBytesMessage is msgtype 0");
}
