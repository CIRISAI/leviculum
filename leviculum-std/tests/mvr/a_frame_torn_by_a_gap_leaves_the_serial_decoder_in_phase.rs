//! mvr for LV-1006 (order 1671): a serial frame that reaches the host in two
//! pieces 400 ms apart must not throw the HDLC decoder out of phase.
//!
//! The bench window of 2026-11-11 (`t114-ingress`) showed a T114 send one
//! announce frame of more than 128 wire bytes as 128 bytes, a 320 to 401 ms
//! hole, then the rest. Python-RNS `SerialInterface.readLoop`
//! (`RNS/Interfaces/SerialInterface.py:190-195`, 1.5.2) drops the partial
//! frame after its 100 ms timeout and sets `in_frame = False`; the torn
//! frame's closing flag is then taken as an opening one, and because its
//! `in_frame and byte == FLAG` branch closes a frame even with an empty
//! buffer, every later frame is read as the bytes between two frames. rnsd
//! delivered nothing more from that port until it was restarted.
//!
//! Our reader drops the torn frame the same way, on the same 100 ms
//! (`leviculum-std/src/interfaces/serial.rs`, `FRAME_TIMEOUT` and the
//! `deframer.reset()` arms of `serial_io_loop`). What it must not do is
//! inherit the phase bug: `Deframer::process_byte`
//! (`leviculum-core/src/framing/hdlc.rs`) takes a flag on an empty buffer as
//! a frame start, so the stray closing flag and the next opening flag
//! collapse into one start and the next frame decodes whole.
//!
//! The test drives the real `SerialInterface` of a node over a pty: one
//! announce torn at wire offset 128 by a 400 ms gap, then ten whole
//! announces from ten other identities. All ten must reach the transport
//! (their bytes on the interface's rx counter, which counts exactly what
//! the decoder hands on). Red if the
//! decoder mirrored the reference's state machine.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::time::{Duration, Instant};

use leviculum_core::framing::hdlc::frame;
use leviculum_core::{Destination, DestinationType, Direction, Identity};
use leviculum_std::config::{Config, InterfaceConfig};
use leviculum_std::driver::ReticulumNodeBuilder;
use rand_core::OsRng;

/// The gap the bench measured inside the torn frame, at its largest.
const GAP: Duration = Duration::from_millis(400);
/// Where every torn frame of the bench window split: the host's cdc_acm read
/// URB is two 64-byte packets.
const SPLIT_AT: usize = 128;
/// Whole frames after the torn one.
const WHOLE: usize = 10;

/// Open a pseudo-terminal: the master as a `File` the test writes the
/// board's bytes into, and the slave's path the node opens as its port.
fn open_pty() -> (std::fs::File, String) {
    // SAFETY: plain libc calls on a descriptor this function owns; the name
    // buffer is NUL-terminated by `ptsname_r` on success.
    unsafe {
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
        (std::fs::File::from_raw_fd(master), name)
    }
}

/// Read the master until `needle` has gone by, or fail after `within`.
///
/// The sync point: the attach-time media query is the last thing the
/// interface writes before its read loop (`serial_reconnect_task`,
/// `ask_duty_hold_state`), so once it is on the line every byte the test
/// writes meets the loop that carries the frame timeout.
fn read_until(master: &mut std::fs::File, needle: &[u8], within: Duration) {
    let deadline = Instant::now() + within;
    let mut seen = Vec::new();
    let mut buf = [0u8; 256];
    while !seen.windows(needle.len()).any(|w| w == needle) {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "the interface never wrote its media query; heard {seen:02x?}"
        );
        let mut pfd = libc::pollfd {
            fd: master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = i32::try_from(left.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: one pollfd on a descriptor `master` keeps open.
        let ready = unsafe { libc::poll(&mut pfd, 1, ms) };
        if ready > 0 {
            let n = master.read(&mut buf).expect("read the master");
            seen.extend_from_slice(&buf[..n]);
        }
    }
}

/// One HDLC-framed announce of a fresh identity, and its destination hash.
fn framed_announce(tag: usize) -> (Vec<u8>, usize) {
    let mut dest = Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "mvr1671",
        &["torn"],
    )
    .expect("destination");
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // App data long enough that the framed announce is over 128 wire bytes,
    // as every torn frame of the bench window was.
    let app_data = format!("announce {tag:02} torn-frame mvr padding padding");
    let packet = dest
        .announce(Some(app_data.as_bytes()), &mut OsRng, 0, now_secs)
        .expect("announce");
    let mut raw = vec![0u8; 600];
    let len = packet.pack(&mut raw).expect("pack");
    raw.truncate(len);
    let mut wire = Vec::new();
    frame(&raw, &mut wire);
    (wire, len)
}

#[tokio::test]
async fn a_frame_torn_by_a_400ms_gap_leaves_the_next_ten_whole() {
    let (mut master, slave) = open_pty();
    let storage = tempfile::tempdir().expect("tempdir");
    let mut config = Config::default();
    config.interfaces.insert(
        "board".to_string(),
        InterfaceConfig {
            interface_type: "SerialInterface".to_string(),
            port: Some(slave),
            speed: Some(115_200),
            databits: Some(8),
            parity: Some("N".to_string()),
            stopbits: Some(1),
            // Configured, so no capability detection runs: that wait reads
            // with a decoder of its own and no frame timeout, and a board
            // answers it at once anyway. The torn frame of the bench came
            // long after it.
            serial_mux: Some(false),
            ..Default::default()
        },
    );
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .config(config)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build node");
    node.start().await.expect("start node");
    // The port is open (and set raw) before the board says anything: bytes
    // written into a pty nobody has opened yet meet a cooked line discipline.
    node.wait_for_interfaces_ready(Duration::from_secs(5))
        .await
        .expect("serial interface ready");
    let mut media_query = Vec::new();
    frame(
        &leviculum_core::envelope::encode_media_query(),
        &mut media_query,
    );
    read_until(&mut master, &media_query, Duration::from_secs(5));

    let (torn, torn_len) = framed_announce(99);
    assert!(
        torn.len() > SPLIT_AT,
        "the torn frame must be longer than the split ({} B)",
        torn.len()
    );
    master
        .write_all(&torn[..SPLIT_AT])
        .expect("write first piece");
    tokio::time::sleep(GAP).await;
    master
        .write_all(&torn[SPLIT_AT..])
        .expect("write second piece");

    let mut whole = Vec::new();
    for tag in 0..WHOLE {
        let (wire, len) = framed_announce(tag);
        master.write_all(&wire).expect("write whole frame");
        whole.push(len as u64);
    }

    // The observable is the interface's rx counter, which counts exactly the
    // frames the decoder handed to the transport (`Ingress::deliver`). Paths
    // are not: ten announces from fresh identities in one burst on a new
    // interface meet the announce ingress limiter, which holds some of them
    // as Python's does (Codeberg #87), and that is the transport's business.
    let whole_bytes: u64 = whole.iter().sum();
    let rx_bytes = || -> u64 { node.interface_stats().iter().map(|s| s.rx_bytes).sum() };
    let deadline = Instant::now() + Duration::from_secs(3);
    while rx_bytes() < whole_bytes && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Settle: a decoder that delivered more than it should have shows it here.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let rx = rx_bytes();
    eprintln!(
        "TORN_FRAME_MVR torn_len={torn_len} whole={WHOLE} rx_bytes={rx} whole_bytes={whole_bytes}"
    );

    // The torn frame itself may be dropped (our 100 ms timeout, the
    // reference's too) or not; the ten after it must all arrive whole.
    assert!(
        rx == whole_bytes || rx == whole_bytes + torn_len as u64,
        "the interface handed {rx} B to the transport; the ten whole frames \
         after the torn one are {whole_bytes} B (the torn one {torn_len} B): \
         the decoder lost phase on the torn frame's closing flag, the \
         reference's SerialInterface.py:190-195 state bug"
    );

    node.stop().await.expect("stop node");
}
