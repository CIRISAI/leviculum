//! Function-level unit and unhappy-path tests for the C API.
//!
//! These call the `lev_*` extern functions directly (the same ABI surface a C
//! caller hits) and need no network, so they run on any target via the rlib.

mod support;

use std::collections::HashSet;
use std::ffi::CStr;
use std::ptr;

use leviculum::*;
use support::{cstr, last_error, read2, Identity};

#[test]
fn version() {
    unsafe {
        let s = lev_version_string();
        assert!(!s.is_null());
        assert!(!CStr::from_ptr(s).to_str().unwrap().is_empty());
        assert_ne!(lev_version_number(), 0);
    }
}

#[test]
fn strerror_distinct_and_unknown() {
    unsafe {
        let msg = |c| CStr::from_ptr(lev_strerror(c)).to_str().unwrap();
        assert_eq!(msg(LEV_OK), "success");
        assert_eq!(msg(LEV_ERR_NULL_PTR), "null pointer");
        assert_eq!(msg(-9999), "unknown error");
        let codes = [
            LEV_OK,
            LEV_ERR_NULL_PTR,
            LEV_ERR_INVALID_ARG,
            LEV_ERR_BUFFER_TOO_SMALL,
            LEV_ERR_NOT_RUNNING,
            LEV_ERR_NO_PATH,
            LEV_ERR_LINK,
            LEV_ERR_TIMEOUT,
            LEV_ERR_AGAIN,
            LEV_ERR_UNKNOWN_DEST,
            LEV_ERR_PANIC,
        ];
        let msgs: HashSet<&str> = codes.iter().map(|&c| msg(c)).collect();
        assert_eq!(msgs.len(), codes.len(), "each code has a distinct message");
    }
}

#[test]
fn hex_roundtrip_and_errors() {
    unsafe {
        let bytes = [0x00u8, 0x1f, 0xab, 0xff];
        let mut need = 0usize;
        // Size query.
        assert_eq!(
            lev_hex_encode(bytes.as_ptr(), 4, ptr::null_mut(), 0, &mut need),
            LEV_ERR_BUFFER_TOO_SMALL
        );
        assert_eq!(need, 8);

        let mut hex = [0u8; 8];
        let mut l = 8usize;
        assert_eq!(
            lev_hex_encode(bytes.as_ptr(), 4, hex.as_mut_ptr(), 8, &mut l),
            LEV_OK
        );
        assert_eq!(&hex, b"001fabff");

        let mut out = [0u8; 4];
        let mut ol = 4usize;
        assert_eq!(
            lev_hex_decode(hex.as_ptr(), 8, out.as_mut_ptr(), 4, &mut ol),
            LEV_OK
        );
        assert_eq!(out, bytes);

        // Odd length and bad digit.
        assert_eq!(
            lev_hex_decode(b"abc".as_ptr(), 3, out.as_mut_ptr(), 4, &mut ol),
            LEV_ERR_INVALID_ARG
        );
        assert_eq!(
            lev_hex_decode(b"zz".as_ptr(), 2, out.as_mut_ptr(), 4, &mut ol),
            LEV_ERR_INVALID_ARG
        );
        // NULL out_len.
        assert_eq!(
            lev_hex_encode(bytes.as_ptr(), 4, hex.as_mut_ptr(), 8, ptr::null_mut()),
            LEV_ERR_NULL_PTR
        );
    }
}

#[test]
fn identity_keys_roundtrip() {
    unsafe {
        let id = Identity::generate();
        assert_eq!(lev_identity_has_private_keys(id.0), 1);

        let prv = read2(|b, c, l| lev_identity_private_key(id.0, b, c, l)).unwrap();
        assert_eq!(prv.len(), 64);
        let id2 = lev_identity_from_private_key(prv.as_ptr(), prv.len());
        assert!(!id2.is_null());
        let id2 = Identity(id2);
        assert_eq!(id.hash(), id2.hash(), "private key round-trips the hash");

        let pubk = read2(|b, c, l| lev_identity_public_key(id.0, b, c, l)).unwrap();
        assert_eq!(pubk.len(), 64);
        let po = lev_identity_from_public_key(pubk.as_ptr(), pubk.len());
        assert!(!po.is_null());
        let po = Identity(po);
        assert_eq!(lev_identity_has_private_keys(po.0), 0);
        let mut buf = [0u8; 64];
        let mut l = 64usize;
        assert_eq!(
            lev_identity_private_key(po.0, buf.as_mut_ptr(), 64, &mut l),
            LEV_ERR_CRYPTO,
            "public-only identity has no private key"
        );

        // Wrong key length is rejected.
        assert!(lev_identity_from_private_key(prv.as_ptr(), 10).is_null());
        assert!(lev_identity_from_public_key(pubk.as_ptr(), 0).is_null());
    }
}

#[test]
fn identity_file_roundtrip() {
    unsafe {
        let dir = tempfile::tempdir().unwrap();
        let path = cstr(dir.path().join("id").to_str().unwrap());
        let id = Identity::generate();
        assert_eq!(lev_identity_save_file(id.0, path.as_ptr()), LEV_OK);

        let loaded = lev_identity_load_file(path.as_ptr());
        assert!(!loaded.is_null());
        let loaded = Identity(loaded);
        assert_eq!(id.hash(), loaded.hash());
        assert_eq!(lev_identity_has_private_keys(loaded.0), 1);

        let missing = cstr(dir.path().join("nope").to_str().unwrap());
        assert!(lev_identity_load_file(missing.as_ptr()).is_null());
    }
}

#[test]
fn identity_null_and_buffer_guards() {
    unsafe {
        let mut buf = [0u8; 16];
        let mut l = 16usize;
        assert_eq!(
            lev_identity_hash(ptr::null(), buf.as_mut_ptr(), 16, &mut l),
            LEV_ERR_NULL_PTR
        );
        assert_eq!(lev_identity_has_private_keys(ptr::null()), 0);

        let id = Identity::generate();
        let mut small = [0u8; 4];
        let mut need = 0usize;
        assert_eq!(
            lev_identity_public_key(id.0, small.as_mut_ptr(), 4, &mut need),
            LEV_ERR_BUFFER_TOO_SMALL
        );
        assert_eq!(need, 64);
        assert_eq!(
            lev_identity_hash(id.0, ptr::null_mut(), 0, &mut need),
            LEV_ERR_BUFFER_TOO_SMALL
        );
        assert_eq!(need, 16);
        assert_eq!(
            lev_identity_hash(id.0, buf.as_mut_ptr(), 16, ptr::null_mut()),
            LEV_ERR_NULL_PTR
        );
    }
}

#[test]
fn destination_validation() {
    unsafe {
        let id = Identity::generate();
        let app = cstr("app");
        let asp = cstr("a");
        let asp_ptrs = [asp.as_ptr()];

        // PLAIN cannot have an identity.
        assert!(lev_destination_new(
            id.0,
            LEV_DIRECTION_IN,
            LEV_DEST_PLAIN,
            app.as_ptr(),
            asp_ptrs.as_ptr(),
            1
        )
        .is_null());
        // Invalid direction / type.
        assert!(lev_destination_new(
            id.0,
            99,
            LEV_DEST_SINGLE,
            app.as_ptr(),
            asp_ptrs.as_ptr(),
            1
        )
        .is_null());
        assert!(lev_destination_new(
            id.0,
            LEV_DIRECTION_IN,
            99,
            app.as_ptr(),
            asp_ptrs.as_ptr(),
            1
        )
        .is_null());
        // NULL app_name.
        assert!(lev_destination_new(
            id.0,
            LEV_DIRECTION_IN,
            LEV_DEST_SINGLE,
            ptr::null(),
            asp_ptrs.as_ptr(),
            1
        )
        .is_null());

        // Valid SINGLE; hash readable; freeable.
        let d = lev_destination_new(
            id.0,
            LEV_DIRECTION_IN,
            LEV_DEST_SINGLE,
            app.as_ptr(),
            asp_ptrs.as_ptr(),
            1,
        );
        assert!(!d.is_null());
        let mut h = [0u8; 16];
        let mut l = 16usize;
        assert_eq!(lev_destination_hash(d, h.as_mut_ptr(), 16, &mut l), LEV_OK);
        lev_destination_free(d);
    }
}

#[test]
fn builder_validation() {
    unsafe {
        let b = lev_builder_new();
        assert!(!b.is_null());

        let bad = cstr("not-a-host-port");
        assert_eq!(
            lev_builder_add_tcp_client(b, bad.as_ptr()),
            LEV_ERR_INVALID_ARG
        );
        assert!(!last_error().is_empty());
        assert_eq!(lev_builder_event_capacity(b, 16, 16), LEV_OK);
        assert_eq!(
            lev_builder_storage_path(b, ptr::null()),
            LEV_ERR_INVALID_ARG
        );

        let dir = tempfile::tempdir().unwrap();
        let sp = cstr(dir.path().to_str().unwrap());
        assert_eq!(lev_builder_storage_path(b, sp.as_ptr()), LEV_OK);

        let node = lev_builder_build(b);
        assert!(!node.is_null());
        // Second build fails; the empty shell is still ours to free.
        assert!(lev_builder_build(b).is_null());
        // A setter after consumption is an error, not a crash.
        assert_eq!(lev_builder_enable_transport(b, 1), LEV_ERR_INVALID_ARG);
        lev_builder_free(b);
        lev_free(node);
    }
}

#[test]
fn log_level_validation() {
    // These entry points take no pointers and are safe to call.
    assert_eq!(lev_init(), LEV_OK);
    assert_eq!(lev_log_set_level(LEV_LOG_INFO), LEV_OK);
    assert_eq!(lev_log_set_level(9999), LEV_ERR_INVALID_ARG);
    assert_eq!(lev_log_set_level(-1), LEV_ERR_INVALID_ARG);
    assert_eq!(lev_log_set_level(LEV_LOG_OFF), LEV_OK);
}

#[test]
fn phase1_builder_setters_validate_args() {
    unsafe {
        let b = lev_builder_new();
        assert_eq!(lev_builder_config_file(b, ptr::null()), LEV_ERR_INVALID_ARG);
        assert_eq!(
            lev_builder_share_instance(b, ptr::null()),
            LEV_ERR_INVALID_ARG
        );
        assert_eq!(
            lev_builder_connect_shared_instance(b, ptr::null()),
            LEV_ERR_INVALID_ARG
        );
        let name = cstr("levtest");
        assert_eq!(lev_builder_share_instance(b, name.as_ptr()), LEV_OK);
        lev_builder_free(b);
    }
}

#[test]
fn phase2_radio_setters_validate_args() {
    unsafe {
        let b = lev_builder_new();
        // NULL device path / parity is rejected; the builder stays usable.
        assert_eq!(
            lev_builder_add_rnode(b, ptr::null(), 867_200_000, 125_000, 8, 5, 0),
            LEV_ERR_INVALID_ARG
        );
        assert_eq!(
            lev_builder_add_serial(b, ptr::null(), 115_200, 8, cstr("N").as_ptr(), 1),
            LEV_ERR_INVALID_ARG
        );
        let dev = cstr("/dev/null");
        assert_eq!(
            lev_builder_add_serial(b, dev.as_ptr(), 115_200, 8, ptr::null(), 1),
            LEV_ERR_INVALID_ARG
        );
        // Valid argument shapes are accepted by the setters (no device opened
        // until build/start).
        assert_eq!(
            lev_builder_add_rnode(b, dev.as_ptr(), 867_200_000, 125_000, 8, 5, 0),
            LEV_OK
        );
        assert_eq!(
            lev_builder_add_serial(b, dev.as_ptr(), 115_200, 8, cstr("N").as_ptr(), 1),
            LEV_OK
        );
        lev_builder_free(b);
    }
}

#[test]
fn config_file_brings_up_a_node() {
    unsafe {
        let dir = tempfile::tempdir().unwrap();
        let port = support::free_port();
        let cfg = format!(
            "[reticulum]\n  enable_transport = no\n\n[interfaces]\n  \
             [[Test TCP Server]]\n    type = TCPServerInterface\n    enabled = yes\n    \
             listen_ip = 127.0.0.1\n    listen_port = {port}\n    mode = gateway\n"
        );
        let cfg_path = dir.path().join("config");
        std::fs::write(&cfg_path, cfg).unwrap();

        let b = lev_builder_new();
        let sp = cstr(dir.path().to_str().unwrap());
        assert_eq!(lev_builder_storage_path(b, sp.as_ptr()), LEV_OK);
        let cf = cstr(cfg_path.to_str().unwrap());
        assert_eq!(lev_builder_config_file(b, cf.as_ptr()), LEV_OK);

        let node = lev_builder_build(b);
        lev_builder_free(b);
        assert!(!node.is_null(), "build with config_file: {}", last_error());
        assert_eq!(lev_start(node), LEV_OK, "start: {}", last_error());
        assert_eq!(lev_is_running(node), 1);
        assert_eq!(lev_stop(node), LEV_OK);
        lev_free(node);
    }
}

#[test]
fn node_null_guards_and_free_null() {
    unsafe {
        assert_eq!(lev_start(ptr::null_mut()), LEV_ERR_NULL_PTR);
        assert_eq!(lev_stop(ptr::null_mut()), LEV_ERR_NULL_PTR);
        assert_eq!(lev_is_running(ptr::null()), 0);
        assert!(lev_event_fd(ptr::null()) < 0);

        let dest = [0u8; 16];
        assert_eq!(lev_has_path(ptr::null(), dest.as_ptr()), LEV_ERR_NULL_PTR);
        let mut link: *mut lev_link_t = ptr::null_mut();
        assert_eq!(
            lev_connect(ptr::null(), dest.as_ptr(), 100, &mut link),
            LEV_ERR_NULL_PTR
        );
        let mut ev: *mut lev_event_t = ptr::null_mut();
        assert_eq!(lev_next_event(ptr::null_mut(), &mut ev), LEV_ERR_NULL_PTR);

        // Every free is a no-op on NULL.
        lev_free(ptr::null_mut());
        lev_builder_free(ptr::null_mut());
        lev_identity_free(ptr::null_mut());
        lev_destination_free(ptr::null_mut());
        lev_link_free(ptr::null_mut());
        lev_event_free(ptr::null_mut());
    }
}
