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
fn announce_then_link_message_both_directions() {
    let p = setup_pair();
    let (lb, la) = establish_link(&p.a, &p.b, &p.dest);

    // lev_link_send goes through the reliable channel, so the peer sees a
    // sequenced LINK_MESSAGE (msgtype 0, the raw-bytes message) not raw data.
    let ping = b"ping";
    assert_eq!(
        unsafe { lev_link_send(lb.0, ping.as_ptr(), 4, 5000) },
        LEV_OK
    );
    let ev = wait_event(p.a.0, LEV_EVENT_LINK_MESSAGE, EV).expect("A receives ping");
    assert_eq!(event_data(&ev), ping);
    let mut msgtype = 0u16;
    let mut sequence = 0u16;
    unsafe {
        assert_eq!(lev_event_msgtype(ev.0, &mut msgtype), LEV_OK);
        assert_eq!(lev_event_sequence(ev.0, &mut sequence), LEV_OK);
    }
    assert_eq!(msgtype, 0, "raw-bytes channel message is msgtype 0");
    assert_eq!(sequence, 0, "first channel message is sequence 0");

    let pong = b"pong";
    assert_eq!(
        unsafe { lev_link_send(la.0, pong.as_ptr(), 4, 5000) },
        LEV_OK
    );
    let ev2 = wait_event(p.b.0, LEV_EVENT_LINK_MESSAGE, EV).expect("B receives pong");
    assert_eq!(event_data(&ev2), pong);

    // A second message from B advances the sequence on that channel.
    let pong2 = b"pong2";
    assert_eq!(
        unsafe { lev_link_send(la.0, pong2.as_ptr(), 5, 5000) },
        LEV_OK
    );
    let ev3 = wait_event(p.b.0, LEV_EVENT_LINK_MESSAGE, EV).expect("B receives pong2");
    let mut seq3 = 0u16;
    unsafe { assert_eq!(lev_event_sequence(ev3.0, &mut seq3), LEV_OK) };
    assert_eq!(seq3, 1, "second channel message is sequence 1");
}

/// Learning a path from an announce emits `LEV_EVENT_PATH_FOUND` carrying the
/// destination hash (the documented event contract behind `lev_request_path`).
#[test]
fn announce_emits_path_found_event() {
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
    let dest = register_single_dest(a.0, id_ptr, "levtest", &["pathfound"]);

    let mut found = false;
    for _ in 0..50 {
        unsafe { lev_announce(a.0, dest.as_ptr(), ptr::null(), 0, 2000) };
        if let Some(ev) = wait_event(bnode.0, LEV_EVENT_PATH_FOUND, Duration::from_millis(400)) {
            assert_eq!(support::event_dest_hash(&ev), dest);
            found = true;
            break;
        }
    }
    assert!(
        found,
        "B should emit LEV_EVENT_PATH_FOUND when it learns A's path"
    );
}

/// `lev_event_msgtype`/`_sequence` only apply to LINK_MESSAGE events; other
/// events reject them with `LEV_ERR_INVALID_ARG`, and NULL pointers are
/// guarded.
#[test]
fn message_metadata_rejected_on_non_message_events() {
    // Build a fresh pair so B's first announce (before it has a path) is
    // available as a non-message event to probe.
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
    let dest = register_single_dest(a.0, id_ptr, "levtest", &["meta"]);

    let mut ann = None;
    for _ in 0..50 {
        unsafe { lev_announce(a.0, dest.as_ptr(), ptr::null(), 0, 2000) };
        if let Some(ev) = wait_event(
            bnode.0,
            LEV_EVENT_ANNOUNCE_RECEIVED,
            Duration::from_millis(400),
        ) {
            ann = Some(ev);
            break;
        }
    }
    let ann = ann.expect("announce on B");
    let mut v = 0u16;
    unsafe {
        assert_eq!(lev_event_msgtype(ann.0, &mut v), LEV_ERR_INVALID_ARG);
        assert_eq!(lev_event_sequence(ann.0, &mut v), LEV_ERR_INVALID_ARG);
        assert_eq!(lev_event_msgtype(ann.0, ptr::null_mut()), LEV_ERR_NULL_PTR);
        assert_eq!(lev_event_msgtype(ptr::null(), &mut v), LEV_ERR_NULL_PTR);
    }
    let _ = (a, bnode);
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

#[test]
fn interface_stats_snapshot_lists_the_tcp_interface() {
    let p = setup_pair();

    // A runs a TCP server interface; it shows in the snapshot with a name, and
    // its byte counters move once B has exchanged announces with it.
    let table = unsafe { lev_interface_stats_snapshot(p.a.0) };
    assert!(!table.is_null());
    let count = unsafe { lev_interface_stats_count(table) };
    assert!(count >= 1, "A should have at least one interface");

    let mut any_named = false;
    let mut any_traffic = false;
    for i in 0..count as usize {
        let name = read2(|b, c, l| unsafe { lev_interface_stats_name(table, i, b, c, l) })
            .expect("interface name");
        if !name.is_empty() {
            any_named = true;
        }
        let mut online = 0i32;
        let mut is_local = 0i32;
        let mut rx = 0u64;
        let mut tx = 0u64;
        assert_eq!(
            unsafe {
                lev_interface_stats_entry(table, i, &mut online, &mut is_local, &mut rx, &mut tx)
            },
            LEV_OK
        );
        assert!(online == 0 || online == 1);
        if rx > 0 || tx > 0 {
            any_traffic = true;
        }
    }
    assert!(any_named, "an interface should have a name");
    assert!(any_traffic, "the TCP link carried bytes");

    unsafe { lev_interface_stats_free(table) };
}

#[test]
fn path_table_snapshot_lists_the_learned_path() {
    let p = setup_pair();

    // B learned a path to A's destination during setup; it shows in the
    // snapshot with its destination hash and a hop count.
    let table = unsafe { lev_path_table_snapshot(p.b.0) };
    assert!(!table.is_null());
    let count = unsafe { lev_path_table_count(table) };
    assert!(count >= 1, "snapshot should list at least one path");

    let mut found = false;
    for i in 0..count as usize {
        let mut dest = [0u8; 16];
        let mut hops = 0u8;
        let mut has_next = 0i32;
        let mut iface = 0u64;
        let mut expires = 0u64;
        assert_eq!(
            unsafe {
                lev_path_table_entry(
                    table,
                    i,
                    dest.as_mut_ptr(),
                    &mut hops,
                    ptr::null_mut(),
                    &mut has_next,
                    &mut iface,
                    &mut expires,
                )
            },
            LEV_OK
        );
        if dest == p.dest {
            assert!(hops >= 1, "a learned path has at least one hop");
            assert!(has_next == 0 || has_next == 1);
            found = true;
        }
    }
    assert!(found, "the snapshot should contain A's destination");

    // Reading past the end is rejected.
    assert_eq!(
        unsafe {
            lev_path_table_entry(
                table,
                count as usize,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        },
        LEV_ERR_INVALID_ARG
    );
    unsafe { lev_path_table_free(table) };
}

#[test]
fn transport_stats_reflect_traffic_and_paths() {
    let p = setup_pair();

    // B learned a path to A during setup.
    let mut b_paths = 0u64;
    unsafe {
        assert_eq!(
            lev_transport_stats(
                p.b.0,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut b_paths,
            ),
            LEV_OK
        );
    }
    assert!(b_paths >= 1, "B should know at least one path to A");

    // B sends a datagram to A; the counters move on both sides.
    let data = b"stats";
    let mut ph = [0u8; 16];
    assert_eq!(
        unsafe {
            lev_send_datagram(
                p.b.0,
                p.dest.as_ptr(),
                data.as_ptr(),
                data.len(),
                ph.as_mut_ptr(),
                3000,
            )
        },
        LEV_OK
    );
    wait_event(p.a.0, LEV_EVENT_PACKET_RECEIVED, EV).expect("A receives the datagram");

    let mut b_sent = 0u64;
    let mut a_received = 0u64;
    unsafe {
        assert_eq!(
            lev_transport_stats(
                p.b.0,
                &mut b_sent,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            ),
            LEV_OK
        );
        assert_eq!(
            lev_transport_stats(
                p.a.0,
                ptr::null_mut(),
                &mut a_received,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            ),
            LEV_OK
        );
    }
    assert!(b_sent > 0, "B sent packets");
    assert!(a_received > 0, "A received packets");
}

#[test]
fn app_proof_strategy_requests_and_sends_proof() {
    let port = support::free_port();
    let da = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let ida = Identity::generate();
    let idb = Identity::generate();
    let addr = format!("127.0.0.1:{port}");
    let addr_c = cstr(&addr);
    let sp = addr_c.as_ptr();
    let a = start_node(da.path(), |b| unsafe {
        assert_eq!(lev_builder_identity(b, ida.0), LEV_OK);
        assert_eq!(lev_builder_add_tcp_server(b, sp), LEV_OK);
    });
    let bnode = start_node(db.path(), |b| unsafe {
        assert_eq!(lev_builder_identity(b, idb.0), LEV_OK);
        assert_eq!(lev_builder_add_tcp_client(b, sp), LEV_OK);
    });

    // A's destination uses the App proof strategy.
    let app = cstr("levtest");
    let asp = cstr("proof");
    let asp_ptrs = [asp.as_ptr()];
    let dest_a = unsafe {
        let d = lev_destination_new(
            ida.0,
            LEV_DIRECTION_IN,
            LEV_DEST_SINGLE,
            app.as_ptr(),
            asp_ptrs.as_ptr(),
            1,
        );
        assert!(!d.is_null());
        assert_eq!(lev_destination_set_proof_strategy(d, LEV_PROOF_APP), LEV_OK);
        let mut h = [0u8; 16];
        let mut l = 16usize;
        assert_eq!(lev_destination_hash(d, h.as_mut_ptr(), 16, &mut l), LEV_OK);
        assert_eq!(lev_register_destination(a.0, d), LEV_OK);
        lev_destination_free(d);
        h
    };
    // B's own destination so A has a return path for the proof.
    let dest_b = register_single_dest(bnode.0, idb.0, "levtest", &["proofback"]);
    learn(&a, &bnode, &dest_a);
    learn(&bnode, &a, &dest_b);

    // B sends a datagram to A's App-strategy destination.
    let payload = b"prove-me";
    let mut ph = [0u8; 16];
    assert_eq!(
        unsafe {
            lev_send_datagram(
                bnode.0,
                dest_a.as_ptr(),
                payload.as_ptr(),
                payload.len(),
                ph.as_mut_ptr(),
                3000,
            )
        },
        LEV_OK
    );

    // A is asked to prove the packet; the event carries its 32-byte hash.
    let pr = wait_event(a.0, LEV_EVENT_PACKET_PROOF_REQUESTED, EV).expect("A asked to prove");
    assert_eq!(support::event_dest_hash(&pr), dest_a);
    let phash = event_data(&pr);
    assert_eq!(phash.len(), 32);

    // A dispatches the delivery proof for that packet. Whether it routes
    // depends on a path to the receiving destination being present (in a real
    // mesh it is, from the announce); over this 2-node loopback the local path
    // table may lack it, so accept the dispatch or a clean no-path result, not
    // a panic or other error.
    let rc = unsafe { lev_send_proof(a.0, dest_a.as_ptr(), phash.as_ptr(), 3000) };
    assert!(
        rc == LEV_OK || rc == LEV_ERR_SEND,
        "send_proof returned {rc}: {}",
        last_error()
    );
}

#[test]
fn all_proof_strategy_does_not_request_proof() {
    let port = support::free_port();
    let da = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let ida = Identity::generate();
    let addr = format!("127.0.0.1:{port}");
    let addr_c = cstr(&addr);
    let sp = addr_c.as_ptr();
    let a = start_node(da.path(), |b| unsafe {
        assert_eq!(lev_builder_identity(b, ida.0), LEV_OK);
        assert_eq!(lev_builder_add_tcp_server(b, sp), LEV_OK);
    });
    let bnode = start_node(db.path(), |b| unsafe {
        assert_eq!(lev_builder_add_tcp_client(b, sp), LEV_OK);
    });

    let app = cstr("levtest");
    let asp = cstr("proofall");
    let asp_ptrs = [asp.as_ptr()];
    let dest_a = unsafe {
        let d = lev_destination_new(
            ida.0,
            LEV_DIRECTION_IN,
            LEV_DEST_SINGLE,
            app.as_ptr(),
            asp_ptrs.as_ptr(),
            1,
        );
        assert!(!d.is_null());
        assert_eq!(lev_destination_set_proof_strategy(d, LEV_PROOF_ALL), LEV_OK);
        let mut h = [0u8; 16];
        let mut l = 16usize;
        assert_eq!(lev_destination_hash(d, h.as_mut_ptr(), 16, &mut l), LEV_OK);
        assert_eq!(lev_register_destination(a.0, d), LEV_OK);
        lev_destination_free(d);
        h
    };
    learn(&a, &bnode, &dest_a);

    let payload = b"auto-proved";
    let mut ph = [0u8; 16];
    assert_eq!(
        unsafe {
            lev_send_datagram(
                bnode.0,
                dest_a.as_ptr(),
                payload.as_ptr(),
                payload.len(),
                ph.as_mut_ptr(),
                3000,
            )
        },
        LEV_OK
    );

    // The packet arrives, but PROVE_ALL handles the proof itself: no request.
    wait_event(a.0, LEV_EVENT_PACKET_RECEIVED, EV).expect("A receives datagram");
    assert!(
        wait_event(
            a.0,
            LEV_EVENT_PACKET_PROOF_REQUESTED,
            Duration::from_millis(500)
        )
        .is_none(),
        "PROVE_ALL must not ask the app to prove"
    );
}

#[test]
fn ratchet_enabled_destination_links_and_exposes_key() {
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

    // Create an inbound destination with ratchets enabled, then register it.
    let app = cstr("levtest");
    let asp = cstr("ratchet");
    let asp_ptrs = [asp.as_ptr()];
    let dest_h = unsafe {
        let dest = lev_destination_new(
            id_ptr,
            LEV_DIRECTION_IN,
            LEV_DEST_SINGLE,
            app.as_ptr(),
            asp_ptrs.as_ptr(),
            1,
        );
        assert!(!dest.is_null());
        assert_eq!(
            lev_destination_enable_ratchets(dest, 1_700_000_000_000),
            LEV_OK
        );
        let mut h = [0u8; 16];
        let mut l = 16usize;
        assert_eq!(
            lev_destination_hash(dest, h.as_mut_ptr(), 16, &mut l),
            LEV_OK
        );
        assert_eq!(lev_register_destination(a.0, dest), LEV_OK);
        lev_destination_free(dest);
        h
    };

    // The ratchet public key is exposed (32 bytes, not all zero).
    let key =
        read2(|b, c, l| unsafe { lev_destination_ratchet_public(a.0, dest_h.as_ptr(), b, c, l) })
            .expect("ratchet public key");
    assert_eq!(key.len(), 32);
    assert!(key.iter().any(|&x| x != 0));

    // A destination without ratchets reports none.
    let plain = register_single_dest(a.0, id_ptr, "levtest", &["noratchet"]);
    let mut nb = [0u8; 32];
    let mut nl = 32usize;
    assert_eq!(
        unsafe {
            lev_destination_ratchet_public(a.0, plain.as_ptr(), nb.as_mut_ptr(), 32, &mut nl)
        },
        LEV_ERR_INVALID_ARG
    );

    // Ratchets must not break the link: learn, connect, exchange a message.
    learn(&a, &bnode, &dest_h);
    let (lb, _la) = establish_link(&a, &bnode, &dest_h);
    let ping = b"ratchet-ping";
    assert_eq!(
        unsafe { lev_link_send(lb.0, ping.as_ptr(), ping.len(), 5000) },
        LEV_OK
    );
    let ev = wait_event(a.0, LEV_EVENT_LINK_MESSAGE, EV).expect("message over ratcheted link");
    assert_eq!(event_data(&ev), ping);
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
