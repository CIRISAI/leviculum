//! Announce-stall probe for Codeberg #418.
//!
//! Drives the REAL `leviculum-core` transport through announce handling while
//! the process-global event-log sink writes to whatever `LEVICULUM_EVENT_LOG`
//! names — in the mvr, a FIFO whose reader the test throttles, which is the
//! smallest thing that makes a `write(2)` block the way a loaded USB disk
//! does.
//!
//! A separate process, not a thread, because the sink is configured once per
//! process from the environment: the two arms of the measurement (blocking
//! writes via `LEVICULUM_EVENT_LOG_SYNC=1`, queued writes by default) cannot
//! coexist in one address space, and a test-only switch inside the sink would
//! be a production foot-gun bought to save a `Command::spawn`.
//!
//! ```sh
//! LEVICULUM_EVENT_LOG=/tmp/fifo announce-stall-probe <rounds> <out-file> [fill]
//! ```
//!
//! # Why it opens the FIFO a second time itself
//!
//! The stall has to be a fact, not a race. Before each announce the probe
//! writes fixed-size filler lines into the pipe through its OWN non-blocking
//! descriptor until the kernel says `EAGAIN`: at that instant the pipe has
//! less room left than one filler line, so the next write the *sink* attempts
//! — the first structured event `process_incoming` emits — must block until
//! the test's reader takes bytes out. No arithmetic on line lengths, no
//! sleeping and hoping.
//!
//! Filler lines are whole lines of a fixed 32 bytes. A pipe write of at most
//! `PIPE_BUF` is atomic, so a full pipe can never leave a half line behind to
//! corrupt the event that follows it.
//!
//! # What it reports
//!
//! One `RX_US <round> <micros> filled=<lines>` line per announce, written to
//! `<out-file>` rather than through `tracing`: a measurement of a sink that
//! stalls must not travel through that sink. Microseconds because the same
//! binary is used for the other half of the question — what announce handling
//! costs when nothing stalls — where milliseconds are all zero.
//!
//! `[fill]` defaults to `1`. Pass `0` to skip the pipe filling entirely and
//! measure the ordinary case against a plain file.

// The probe needs a Unix FIFO and raw descriptors; elsewhere it builds as a
// stub so the workspace still compiles (CIRIS fork: the Windows lane).
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::io::Write;
#[cfg(unix)]
use std::os::fd::{FromRawFd, IntoRawFd};
use std::time::Instant;

use leviculum_core::constants::{RANDOM_HASHBYTES, TRUNCATED_HASHBYTES};
use leviculum_core::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use leviculum_core::transport::{Transport, TransportConfig};
use leviculum_core::{Clock, Destination, DestinationType, Direction, Identity, MemoryStorage};
use leviculum_std::event_log::install_global_subscriber;

use rand_core::OsRng;

/// Exactly 32 bytes including the newline. Under `PIPE_BUF`, so each one
/// either lands whole or does not land at all.
const FILLER_LINE: &[u8] = b"PROBE_FILL node=probe t=0______\n";

/// The interface the announces arrive on.
const IFACE: usize = 0;

/// Monotonic clock over `Instant`. The crate's `SystemClock` is
/// `pub(crate)` and a binary is a separate crate; this is the same three
/// lines.
#[derive(Clone)]
struct ProbeClock {
    start: Instant,
}

impl Clock for ProbeClock {
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

/// A valid signed announce on the wire, one fresh destination per call.
fn make_announce_raw(hops: u8, aspect: &str) -> (Vec<u8>, [u8; TRUNCATED_HASHBYTES]) {
    let identity = Identity::generate(&mut OsRng);
    let dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "stallprobe",
        &[aspect],
    )
    .expect("destination");
    let id = dest.identity().expect("identity");
    let random_hash = [0x42u8; RANDOM_HASHBYTES];

    let mut payload = Vec::new();
    payload.extend_from_slice(&id.public_key_bytes());
    payload.extend_from_slice(dest.name_hash());
    payload.extend_from_slice(&random_hash);

    let app_data = b"probe";
    let mut signed = Vec::new();
    signed.extend_from_slice(dest.hash().as_bytes());
    signed.extend_from_slice(&id.public_key_bytes());
    signed.extend_from_slice(dest.name_hash());
    signed.extend_from_slice(&random_hash);
    signed.extend_from_slice(app_data);
    let signature = id.sign(&signed).expect("sign");
    payload.extend_from_slice(&signature);
    payload.extend_from_slice(app_data);

    let packet = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Single,
            packet_type: PacketType::Announce,
        },
        hops,
        transport_id: None,
        destination_hash: dest.hash().into_bytes(),
        context: PacketContext::None,
        data: PacketData::Owned(payload),
    };
    let mut buf = [0u8; 500];
    let len = packet.pack(&mut buf).expect("pack");
    (buf[..len].to_vec(), dest.hash().into_bytes())
}

/// Open the event-log FIFO a second time, write-only and non-blocking.
///
/// `ENXIO` means no reader has it open yet; the test's reader is on its way,
/// so retry briefly rather than failing the run on a scheduling order.
#[cfg(unix)]
fn open_filler(path: &str) -> std::fs::File {
    let c = std::ffi::CString::new(path).expect("path");
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
        if fd >= 0 {
            return unsafe { std::fs::File::from_raw_fd(fd) };
        }
        let err = std::io::Error::last_os_error();
        assert!(
            Instant::now() < deadline,
            "cannot open {path} for filling: {err}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Fill the pipe until the kernel refuses another filler line. Returns the
/// number of lines it took, which is only interesting when it is zero (the
/// pipe was already full, so the next sink write blocks either way).
fn fill_to_eagain(filler: &mut std::fs::File) -> usize {
    let mut lines = 0usize;
    loop {
        match filler.write(FILLER_LINE) {
            Ok(_) => lines += 1,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return lines,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => panic!("filling the event-log pipe: {e}"),
        }
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("announce-stall-probe needs a Unix FIFO; it does nothing on this platform");
    std::process::exit(2);
}

#[cfg(unix)]
fn main() {
    let mut args = std::env::args().skip(1);
    let rounds: usize = args
        .next()
        .and_then(|s| s.parse().ok())
        .expect("usage: announce-stall-probe <rounds> <out-file>");
    let out_path = args
        .next()
        .expect("usage: announce-stall-probe <rounds> <out-file>");
    let fill = args.next().map(|a| a != "0").unwrap_or(true);
    let log_path = std::env::var("LEVICULUM_EVENT_LOG").expect("LEVICULUM_EVENT_LOG");

    let mut filler = if fill {
        Some(open_filler(&log_path))
    } else {
        None
    };
    install_global_subscriber("debug");

    let clock = ProbeClock {
        start: Instant::now(),
    };
    let identity = Identity::generate(&mut OsRng);
    let config = TransportConfig {
        enable_transport: true,
        ..TransportConfig::default()
    };
    let mut transport = Transport::new(config, clock, MemoryStorage::with_defaults(), identity);
    // Announce ingress burst limiting would hold most of these for later
    // release, and a held announce never reaches the span under measurement.
    // Point-to-point media turn it off for exactly this reason.
    transport.set_interface_ingress_control(IFACE, false);

    let announces: Vec<Vec<u8>> = (0..rounds)
        .map(|i| make_announce_raw(1, &format!("a{i}")).0)
        .collect();

    let mut report = String::new();
    for (i, raw) in announces.iter().enumerate() {
        let filled = filler.as_mut().map(fill_to_eagain).unwrap_or(0);
        let started = Instant::now();
        transport
            .process_incoming(IFACE, raw)
            .expect("process announce");
        let took_us = started.elapsed().as_micros();
        transport.drain_events();
        report.push_str(&format!("RX_US {i} {took_us} filled={filled}\n"));
    }

    // Out of band on purpose: a measurement of a sink that stalls must not
    // travel through that sink.
    std::fs::write(&out_path, report).expect("write report");

    // Let go of the filler descriptor so the reader sees EOF once the sink's
    // own handle closes at exit.
    if let Some(f) = filler {
        let fd = f.into_raw_fd();
        unsafe { libc::close(fd) };
    }
}
