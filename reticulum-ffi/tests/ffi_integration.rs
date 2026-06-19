//! In-process integration tests: two real nodes over TCP loopback, driven
//! entirely through the C API, covering the core flows and their unhappy paths.

mod support;

use std::ptr;
use std::time::Duration;

use leviculum::*;
use support::{
    cstr, event_data, event_link_id, last_error, read2, register_single_dest, start_node,
    wait_event, Identity, Link, Node,
};

const EV: Duration = Duration::from_secs(5);

/// Two nodes where B has learned A's destination via an announce: A is a TCP
/// server with a registered SINGLE destination, B a TCP client to it.
struct Pair {
    a: Node,
    b: Node,
    _ida: Identity,
    dest: [u8; 16],
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

fn setup_pair() -> Pair {
    let port = support::free_port();
    let da = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let ida = Identity::generate();
    let id_ptr = ida.0;
    let addr = format!("127.0.0.1:{port}");
    let addr_c = cstr(&addr);
    let server_ptr = addr_c.as_ptr();

    let a = start_node(da.path(), |b| unsafe {
        assert_eq!(lev_builder_identity(b, id_ptr), LEV_OK);
        assert_eq!(lev_builder_add_tcp_server(b, server_ptr), LEV_OK);
    });
    let bnode = start_node(db.path(), |b| unsafe {
        assert_eq!(lev_builder_add_tcp_client(b, server_ptr), LEV_OK);
    });

    let dest = register_single_dest(a.0, id_ptr, "levtest", &["integ"]);
    learn(&a, &bnode, &dest);
    Pair {
        a,
        b: bnode,
        _ida: ida,
        dest,
        _dirs: (da, db),
    }
}

/// Re-announce until B has a path to A (and thus A's cached identity).
fn learn(a: &Node, b: &Node, dest: &[u8; 16]) {
    for _ in 0..50 {
        unsafe { lev_announce(a.0, dest.as_ptr(), ptr::null(), 0, 2000) };
        // Drain B's events for ~300ms so the announce is processed.
        let mut ev: *mut lev_event_t = ptr::null_mut();
        unsafe { lev_wait_event(b.0, &mut ev, 300) };
        while !ev.is_null() {
            unsafe {
                lev_event_free(ev);
                ev = ptr::null_mut();
            }
            if unsafe { lev_next_event(b.0, &mut ev) } != LEV_OK {
                break;
            }
        }
        if unsafe { lev_has_path(b.0, dest.as_ptr()) } == 1 {
            return;
        }
    }
    panic!("B never learned a path to A");
}

/// Connect B to A's destination, accept on A, wait for established on B.
fn establish_link(a: &Node, b: &Node, dest: &[u8; 16]) -> (Link, Link) {
    let mut lb: *mut lev_link_t = ptr::null_mut();
    assert_eq!(
        unsafe { lev_connect(b.0, dest.as_ptr(), 5000, &mut lb) },
        LEV_OK,
        "connect: {}",
        last_error()
    );
    assert!(!lb.is_null());

    let req = wait_event(a.0, LEV_EVENT_LINK_REQUEST, EV).expect("link request on A");
    let lid = event_link_id(&req);
    drop(req);

    let mut la: *mut lev_link_t = ptr::null_mut();
    assert_eq!(
        unsafe { lev_accept_link(a.0, lid.as_ptr(), 5000, &mut la) },
        LEV_OK
    );
    assert!(!la.is_null());

    wait_event(b.0, LEV_EVENT_LINK_ESTABLISHED, EV).expect("established on B");
    (Link(lb), Link(la))
}

#[test]
fn announce_then_link_data_both_directions() {
    let p = setup_pair();
    let (lb, la) = establish_link(&p.a, &p.b, &p.dest);

    let ping = b"ping";
    assert_eq!(
        unsafe { lev_link_send(lb.0, ping.as_ptr(), 4, 5000) },
        LEV_OK
    );
    let ev = wait_event(p.a.0, LEV_EVENT_LINK_DATA, EV).expect("A receives ping");
    assert_eq!(event_data(&ev), ping);

    let pong = b"pong";
    assert_eq!(
        unsafe { lev_link_send(la.0, pong.as_ptr(), 4, 5000) },
        LEV_OK
    );
    let ev2 = wait_event(p.b.0, LEV_EVENT_LINK_DATA, EV).expect("B receives pong");
    assert_eq!(event_data(&ev2), pong);
}

#[test]
fn link_identify_and_remote_identity() {
    let p = setup_pair();
    let (lb, la) = establish_link(&p.a, &p.b, &p.dest);

    let bident = Identity::generate();
    let lbid = lb.id();
    assert_eq!(
        unsafe { lev_link_identify(p.b.0, lbid.as_ptr(), bident.0, 3000) },
        LEV_OK
    );
    wait_event(p.a.0, LEV_EVENT_LINK_IDENTIFIED, EV).expect("A sees identify");

    let laid = la.id();
    let remote = unsafe { lev_link_remote_identity(p.a.0, laid.as_ptr()) };
    assert!(!remote.is_null());
    let remote = Identity(remote);
    assert_eq!(remote.hash(), bident.hash());
}

#[test]
fn request_response_echo() {
    let p = setup_pair();
    let path = cstr("/echo");
    assert_eq!(
        unsafe {
            lev_register_request_handler(
                p.a.0,
                p.dest.as_ptr(),
                path.as_ptr(),
                LEV_REQUEST_POLICY_ALLOW_ALL,
                ptr::null(),
                0,
            )
        },
        LEV_OK
    );

    let (lb, _la) = establish_link(&p.a, &p.b, &p.dest);
    let req = [0xA4u8, b'p', b'i', b'n', b'g']; // msgpack "ping"
    let resp = [0xA4u8, b'p', b'o', b'n', b'g']; // msgpack "pong"
    let lbid = lb.id();
    let mut req_id = [0u8; 16];
    assert_eq!(
        unsafe {
            lev_send_request(
                p.b.0,
                lbid.as_ptr(),
                path.as_ptr(),
                req.as_ptr(),
                req.len(),
                5000,
                req_id.as_mut_ptr(),
            )
        },
        LEV_OK
    );

    let rr = wait_event(p.a.0, LEV_EVENT_REQUEST_RECEIVED, EV).expect("A receives request");
    let got_path = read2(|b, c, l| unsafe { lev_event_path(rr.0, b, c, l) }).unwrap();
    assert_eq!(got_path, b"/echo");
    assert_eq!(event_data(&rr), req);
    let a_link = event_link_id(&rr);
    let got_id = read2(|b, c, l| unsafe { lev_event_request_id(rr.0, b, c, l) }).unwrap();
    assert_eq!(
        unsafe {
            lev_send_response(
                p.a.0,
                a_link.as_ptr(),
                got_id.as_ptr(),
                resp.as_ptr(),
                resp.len(),
                3000,
            )
        },
        LEV_OK
    );

    let re = wait_event(p.b.0, LEV_EVENT_RESPONSE_RECEIVED, EV).expect("B receives response");
    let resp_id = read2(|b, c, l| unsafe { lev_event_request_id(re.0, b, c, l) }).unwrap();
    assert_eq!(resp_id, req_id);
    assert_eq!(event_data(&re), resp);
}

#[test]
fn request_to_unhandled_path_times_out() {
    let p = setup_pair();
    let (lb, _la) = establish_link(&p.a, &p.b, &p.dest);
    let path = cstr("/nohandler");
    let lbid = lb.id();
    let mut req_id = [0u8; 16];
    assert_eq!(
        unsafe {
            lev_send_request(
                p.b.0,
                lbid.as_ptr(),
                path.as_ptr(),
                ptr::null(),
                0,
                500, // tiny response deadline
                req_id.as_mut_ptr(),
            )
        },
        LEV_OK
    );
    wait_event(p.b.0, LEV_EVENT_REQUEST_TIMEOUT, EV).expect("request times out");
}

#[test]
fn datagram_delivery_and_no_path() {
    let p = setup_pair();
    let data = b"hi";
    let mut phash = [0u8; 16];
    assert_eq!(
        unsafe {
            lev_send_datagram(
                p.b.0,
                p.dest.as_ptr(),
                data.as_ptr(),
                2,
                phash.as_mut_ptr(),
                3000,
            )
        },
        LEV_OK,
        "{}",
        last_error()
    );
    let ev = wait_event(p.a.0, LEV_EVENT_PACKET_RECEIVED, EV).expect("A receives datagram");
    assert_eq!(event_data(&ev), data);

    // No path to an unknown destination.
    let unknown = [0x11u8; 16];
    assert_eq!(
        unsafe {
            lev_send_datagram(
                p.b.0,
                unknown.as_ptr(),
                data.as_ptr(),
                2,
                phash.as_mut_ptr(),
                1000,
            )
        },
        LEV_ERR_NO_PATH
    );
}

#[test]
fn resource_transfer_accept_app() {
    let p = setup_pair();
    let (lb, la) = establish_link(&p.a, &p.b, &p.dest);
    let laid = la.id();
    assert_eq!(
        unsafe { lev_set_resource_strategy(p.a.0, laid.as_ptr(), LEV_RESOURCE_ACCEPT_APP) },
        LEV_OK
    );

    let payload: Vec<u8> = (0..300u32).map(|i| (i * 7 + 1) as u8).collect();
    let lbid = lb.id();
    let mut rhash = [0u8; 32];
    assert_eq!(
        unsafe {
            lev_send_resource(
                p.b.0,
                lbid.as_ptr(),
                payload.as_ptr(),
                payload.len(),
                ptr::null(),
                0,
                1,
                rhash.as_mut_ptr(),
                5000,
            )
        },
        LEV_OK,
        "{}",
        last_error()
    );

    let adv = wait_event(
        p.a.0,
        LEV_EVENT_RESOURCE_ADVERTISED,
        Duration::from_secs(10),
    )
    .expect("advertised");
    drop(adv);
    assert_eq!(
        unsafe { lev_accept_resource(p.a.0, laid.as_ptr(), 3000) },
        LEV_OK
    );
    let done = wait_event(p.a.0, LEV_EVENT_RESOURCE_COMPLETED, Duration::from_secs(15))
        .expect("resource completed");
    assert_eq!(event_data(&done), payload);
}

/// Open a pseudo-terminal and return (master fd, slave device path). The
/// master must stay open to keep the pty alive; the node opens the slave path
/// as a serial port.
unsafe fn open_pty() -> (i32, String) {
    let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
    assert!(master >= 0, "posix_openpt failed");
    assert_eq!(libc::grantpt(master), 0, "grantpt failed");
    assert_eq!(libc::unlockpt(master), 0, "unlockpt failed");
    let mut buf = [0 as libc::c_char; 256];
    assert_eq!(
        libc::ptsname_r(master, buf.as_mut_ptr(), buf.len()),
        0,
        "ptsname_r failed"
    );
    let name = std::ffi::CStr::from_ptr(buf.as_ptr())
        .to_str()
        .expect("pty name utf-8")
        .to_string();
    (master, name)
}

/// A serial interface is a raw KISS port with no link-up handshake, so it
/// comes up over a bare pty with nothing on the far end. This proves the
/// programmatic serial path opens the device and the node runs. (RNode needs
/// the CMD_DETECT handshake, so it is exercised over the lora-proxy mock in the
/// LoRa tier, not here.)
#[test]
fn serial_interface_comes_up_over_pty() {
    let (master, slave) = unsafe { open_pty() };
    let dir = tempfile::tempdir().unwrap();
    let slave_c = cstr(&slave);
    let port = slave_c.as_ptr();

    let node = start_node(dir.path(), |b| unsafe {
        assert_eq!(
            lev_builder_add_serial(b, port, 115_200, 8, cstr("N").as_ptr(), 1),
            LEV_OK
        );
    });
    unsafe {
        assert_eq!(
            lev_is_running(node.0),
            1,
            "node not running with serial iface"
        );
    }
    drop(node);
    unsafe { libc::close(master) };
}

#[test]
fn shared_instance_forwards_announce() {
    // A unique abstract-socket name per run (the namespace is machine-wide).
    let name = format!("levtest-{}", support::free_port());
    let da = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let ida = Identity::generate();
    let id_ptr = ida.0;
    let name_c = cstr(&name);
    let name_ptr = name_c.as_ptr();

    // A offers a shared instance (local IPC socket + RPC).
    let a = start_node(da.path(), |b| unsafe {
        assert_eq!(lev_builder_identity(b, id_ptr), LEV_OK);
        assert_eq!(lev_builder_share_instance(b, name_ptr), LEV_OK);
    });
    // Let A's local server bind before B connects.
    std::thread::sleep(Duration::from_millis(400));
    // B is a client of A's shared instance, no interfaces of its own.
    let bnode = start_node(db.path(), |b| unsafe {
        assert_eq!(lev_builder_connect_shared_instance(b, name_ptr), LEV_OK);
    });

    let dest = register_single_dest(a.0, id_ptr, "shared", &["test"]);
    let mut seen = false;
    for _ in 0..40 {
        unsafe { lev_announce(a.0, dest.as_ptr(), ptr::null(), 0, 2000) };
        if let Some(ev) = wait_event(
            bnode.0,
            LEV_EVENT_ANNOUNCE_RECEIVED,
            Duration::from_millis(700),
        ) {
            if support::event_dest_hash(&ev) == dest {
                seen = true;
                break;
            }
        }
    }
    assert!(seen, "shared-instance client B never saw A's announce");
}

#[test]
fn connect_unknown_destination() {
    let d = tempfile::tempdir().unwrap();
    let node = start_node(d.path(), |_b| {});
    let unknown = [0x22u8; 16];
    let mut link: *mut lev_link_t = ptr::null_mut();
    assert_eq!(
        unsafe { lev_connect(node.0, unknown.as_ptr(), 1000, &mut link) },
        LEV_ERR_UNKNOWN_DEST
    );
    assert!(link.is_null());
}

#[test]
fn double_start_and_restart() {
    let d = tempfile::tempdir().unwrap();
    let node = start_node(d.path(), |_b| {});
    assert_eq!(
        unsafe { lev_start(node.0) },
        LEV_ERR_CONFIG,
        "double start rejected"
    );
    assert_eq!(unsafe { lev_stop(node.0) }, LEV_OK);
    assert_eq!(unsafe { lev_is_running(node.0) }, 0);
    assert_eq!(unsafe { lev_start(node.0) }, LEV_OK, "restart");
    assert_eq!(unsafe { lev_is_running(node.0) }, 1);
    assert_eq!(unsafe { lev_stop(node.0) }, LEV_OK);
}

#[test]
fn send_on_closed_link_fails() {
    let p = setup_pair();
    let (lb, _la) = establish_link(&p.a, &p.b, &p.dest);
    assert_eq!(unsafe { lev_close_link(lb.0, 2000) }, LEV_OK);
    std::thread::sleep(Duration::from_millis(200));
    let rc = unsafe { lev_link_send(lb.0, b"x".as_ptr(), 1, 1000) };
    assert_ne!(rc, LEV_OK, "send on a closed link must fail");
}
