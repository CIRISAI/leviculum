//! A/B: how long a shared-instance client's announce waits before it is on
//! the air — our `lnsd` against Python `rnsd`, same driver on both.
//!
//! ## Why
//!
//! Python fires a local client's announce immediately: "If the announce is
//! from a local client, it is announced immediately, but only one time",
//! `retransmit_timeout = now`
//! (`reference/Reticulum/RNS/Transport.py:1890-1894`). We added 250 ms to
//! the first announce of each local-client destination
//! (`LOCAL_CLIENT_ANNOUNCE_DELAY_MS`) to batch a start-up burst. That is a
//! deviation, and the deviation rule's third condition — a measurable
//! improvement of priority 1 — never had a measurement behind it. This
//! file is the measurement that was owed, taken on the removal.
//!
//! ## The contract this harness has to keep
//!
//! Drop-in compatibility is the whole point: `lnsd` and `rnsd` share the
//! shared-instance IPC, so the *same* client code drives both. Here that is
//! [`announce_and_time`] — one function, one HDLC-framed announce onto one
//! abstract Unix socket, one deframing read loop on one raw TCP peer. What
//! differs between the two arms is the daemon behind the socket and nothing
//! else. A parallel per-stack driver would smuggle cadence and timeout
//! differences into what claims to be a stack comparison.
//!
//! ```text
//!   client (raw HDLC over abstract unix socket)
//!        │  announce for a fresh destination, t0
//!        v
//!      DUT: lnsd (in-process)  |  rnsd (reference/Reticulum, via TestDaemon)
//!        │  rebroadcast
//!        v
//!   observer (raw RNS/TCP peer), t1
//! ```
//!
//! ## Reading the result
//!
//! **Count the event volumes on both sides before reading any timing.** Both
//! arms must observe the same number of announces for the same number of
//! registrations; if they do not, the comparison is invalid and the timings
//! below mean nothing. The assertions enforce that first.
//!
//! Ignored by default: it spawns a Python daemon per arm and is a
//! measurement, not a gate. Run with
//! `cargo test -p leviculum-std --test rnsd_interop -- --ignored --nocapture
//!  local_client_announce_latency`.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use leviculum_core::packet::{Packet, PacketType};
use leviculum_std::driver::ReticulumNodeBuilder;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::common::{build_announce_raw, init_tracing};
use crate::harness::{find_available_ports, TestDaemon};

/// Registrations per arm. Each one uses a fresh destination, so each is a
/// *first* announce for that destination — the case the 250 ms hold applied
/// to.
const REGISTRATIONS: usize = 12;

/// Generous: what is being measured is a sub-second delay, and a timeout
/// here would show up as a volume mismatch, which is the first thing
/// asserted.
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(10);

/// Connect to an abstract Unix socket, the way a shared-instance client
/// does. Python listens on `\0rns/{instance_name}`; so do we.
fn connect_abstract_unix(instance_name: &str) -> std::io::Result<tokio::net::UnixStream> {
    use std::os::linux::net::SocketAddrExt;
    let addr = std::os::unix::net::SocketAddr::from_abstract_name(
        format!("rns/{instance_name}").as_bytes(),
    )?;
    let std_stream = std::os::unix::net::UnixStream::connect_addr(&addr)?;
    std_stream.set_nonblocking(true)?;
    tokio::net::UnixStream::from_std(std_stream)
}

/// THE driver. Identical code for both stacks, by construction: it is called
/// once per arm with only the two sockets differing.
///
/// Announces one fresh destination over the shared-instance socket and
/// returns the time until that announce is seen on the observing TCP peer,
/// or `None` if it never arrived.
async fn announce_and_time(
    client: &mut tokio::net::UnixStream,
    observer: &mut TcpStream,
    deframer: &mut Deframer,
    index: usize,
) -> Option<Duration> {
    let aspect = format!("reg{index}");
    let (raw, dest_hash, _dest) = build_announce_raw("abann", &[aspect.as_str()], b"ab");

    let mut framed = Vec::new();
    frame(&raw, &mut framed);

    let t0 = Instant::now();
    client.write_all(&framed).await.ok()?;
    client.flush().await.ok()?;

    let mut buf = [0u8; 4096];
    while t0.elapsed() < OBSERVE_TIMEOUT {
        let read = tokio::time::timeout(Duration::from_millis(50), observer.read(&mut buf)).await;
        let n = match read {
            Ok(Ok(0)) => return None,
            Ok(Ok(n)) => n,
            Ok(Err(_)) => return None,
            Err(_) => continue,
        };
        for result in deframer.process(&buf[..n]) {
            if let DeframeResult::Frame(data) = result {
                if let Ok(pkt) = Packet::unpack(&data) {
                    if pkt.flags.packet_type == PacketType::Announce
                        && pkt.destination_hash == *dest_hash.as_bytes()
                    {
                        return Some(t0.elapsed());
                    }
                }
            }
        }
    }
    None
}

/// One arm's result: what was observed, and how late.
struct Arm {
    label: &'static str,
    observed: Vec<Duration>,
}

impl Arm {
    fn report(&self) {
        let mut ms: Vec<u128> = self.observed.iter().map(|d| d.as_millis()).collect();
        ms.sort_unstable();
        let n = ms.len();
        println!(
            "ANN_LATENCY stack={} observed={}/{} min={} median={} max={} all={:?}",
            self.label,
            n,
            REGISTRATIONS,
            ms.first().copied().unwrap_or(0),
            ms.get(n / 2).copied().unwrap_or(0),
            ms.last().copied().unwrap_or(0),
            ms,
        );
    }

    fn median_ms(&self) -> u128 {
        let mut ms: Vec<u128> = self.observed.iter().map(|d| d.as_millis()).collect();
        ms.sort_unstable();
        ms.get(ms.len() / 2).copied().unwrap_or(u128::MAX)
    }
}

/// Run the driver `REGISTRATIONS` times against one daemon.
async fn run_arm(label: &'static str, instance_name: &str, tcp_addr: SocketAddr) -> Arm {
    let mut observer = TcpStream::connect(tcp_addr)
        .await
        .expect("observer connects to the DUT's TCP server");
    let mut deframer = Deframer::new();

    let mut client =
        connect_abstract_unix(instance_name).expect("client attaches to the shared instance");
    // Both stacks need the accepted connection to become a LocalClient
    // interface before the first announce; same wait on both arms.
    tokio::time::sleep(Duration::from_millis(800)).await;

    let mut observed = Vec::new();
    for index in 0..REGISTRATIONS {
        if let Some(delay) =
            announce_and_time(&mut client, &mut observer, &mut deframer, index).await
        {
            observed.push(delay);
        }
        // Space the registrations so each is measured on its own, well
        // outside either stack's per-destination announce-rate window.
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    Arm { label, observed }
}

/// A local client's announce must reach the air as promptly on our stack as
/// on the reference's. The pin is the volume first, the timing second.
#[tokio::test]
#[ignore = "measurement: spawns a Python rnsd, ~30 s"]
async fn local_client_announce_latency_matches_the_reference() {
    init_tracing();

    let (ports, _port_alloc) = find_available_ports::<4>()
        .await
        .expect("Failed to allocate ports");
    let [rust_tcp_port, py_rns_port, py_cmd_port, _spare] = ports;

    // --- Arm R: our lnsd, in process, TCP server + shared instance.
    let rust_instance = format!("abann_rust_{}", std::process::id());
    let rust_addr: SocketAddr = format!("127.0.0.1:{rust_tcp_port}").parse().unwrap();
    let storage = crate::common::temp_storage("local_client_announce_latency_ab", "lnsd");
    let mut lnsd = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .share_instance(true)
        .instance_name(rust_instance.clone())
        .add_tcp_server(rust_addr)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("lnsd builds");
    lnsd.start().await.expect("lnsd starts");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let rust_arm = run_arm("lnsd", &rust_instance, rust_addr).await;
    lnsd.stop().await.ok();

    // --- Arm P: the reference rnsd, same driver.
    let py_instance = format!("abann_py_{}", std::process::id());
    let py = TestDaemon::start_with_shared_instance_ports(py_rns_port, py_cmd_port, &py_instance)
        .await
        .expect("rnsd starts");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let py_addr: SocketAddr = format!("127.0.0.1:{py_rns_port}").parse().unwrap();

    let py_arm = run_arm("rnsd", &py_instance, py_addr).await;
    drop(py);

    rust_arm.report();
    py_arm.report();

    // Volume gate. Nothing below this line is meaningful unless it holds:
    // an arm that observed fewer announces was measuring a different
    // scenario, and the timings would be comparing two different things.
    assert_eq!(
        py_arm.observed.len(),
        REGISTRATIONS,
        "the reference arm must observe every registration, else the \
         comparison is invalid: {}/{}",
        py_arm.observed.len(),
        REGISTRATIONS
    );
    assert_eq!(
        rust_arm.observed.len(),
        py_arm.observed.len(),
        "both arms must observe the same number of announces before any \
         timing is read (ours {}, reference {})",
        rust_arm.observed.len(),
        py_arm.observed.len()
    );

    // The timing claim, stated as the removal's purpose: with the core hold
    // gone we are not slower than the reference by a margin that would have
    // to be the hold. 100 ms of headroom absorbs scheduler and Python
    // start-up noise without absorbing a 250 ms hold.
    let ours = rust_arm.median_ms();
    let theirs = py_arm.median_ms();
    assert!(
        ours <= theirs + 100,
        "our median client-announce latency ({ours} ms) must not exceed the \
         reference's ({theirs} ms) by a hold-sized margin"
    );
}
