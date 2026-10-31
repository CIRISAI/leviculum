//! mvr for 1190 — one source's connection flood takes the TCP server down
//! for everyone.
//!
//! Measured on the public backbone (leviculum.network, 2026-10-31): 972 of
//! 1 135 connections on lnsd's TCP server port came from one address and
//! almost none of them ever sent a byte. lnsd held 1 001 of its 1 024 file
//! descriptors, the accept queue was full, legitimate clients could not get
//! in, and the accept loop answered every `EMFILE` with one log line and an
//! immediate retry, about 815 lines a second.
//!
//! The scenario here is that incident in one process tree: a release lnsd
//! with a TCP server and a lowered file-descriptor limit, one source
//! (`127.0.0.2`) opening more silent connections than the limit holds, then
//! five legitimate clients (`127.0.0.1`) that each announce a destination
//! and must hear the other four announces relayed back.
//!
//! **Acceptance**: red before 1190 (the flood exhausts the descriptors, the
//! legitimate clients are never served, the accept loop logs one line per
//! failed accept), green after it (the flood source holds
//! `max_connections_per_source`, its connections are closed after the silent
//! timeout, the five clients talk, the accept loop logs no error storm, the
//! daemon's resident memory stays flat).

use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use leviculum_core::constants::{MTU, RANDOM_HASHBYTES};
use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use leviculum_core::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use leviculum_core::{Destination, DestinationType, Direction, Identity};
use leviculum_std::process::spawn_supervised;
use rand_core::{OsRng, RngCore};

/// The daemon's descriptor limit, soft and hard. Above the 256 a server
/// interface needs to start, below the flood below.
const LNSD_NOFILE: u64 = 384;

/// Silent connections the flood source opens: more than the daemon's limit.
const FLOOD_CONNECTIONS: usize = 420;

/// The cap the config sets (the default) and the silent timeout it shortens.
const PER_SOURCE_CAP: usize = 16;
const SILENT_TIMEOUT_SECS: u64 = 2;

/// The legitimate clients.
const CLIENTS: usize = 5;

fn release_bin(name: &str) -> PathBuf {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target"));
    let triple = std::env::var("CARGO_BUILD_TARGET")
        .unwrap_or_else(|_| "x86_64-unknown-linux-musl".to_string());
    let triple_release = target_dir.join(&triple).join("release").join(name);
    let bin = if triple_release.exists() {
        triple_release
    } else {
        target_dir.join("release").join(name)
    };
    crate::assert_release_bin_fresh(&bin);
    bin
}

/// Raise this process's own soft descriptor limit to its hard one: the test
/// holds the flood's client ends, several hundred sockets.
fn raise_own_nofile() {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: plain syscalls on a stack value.
    unsafe {
        assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim), 0);
        lim.rlim_cur = lim.rlim_max;
        assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &lim), 0);
    }
    assert!(
        lim.rlim_cur as usize > FLOOD_CONNECTIONS + 64,
        "the test needs {} descriptors, the hard limit is {}",
        FLOOD_CONNECTIONS + 64,
        lim.rlim_cur
    );
}

fn spawn_lnsd(dir: &Path, port: u16) -> Child {
    fs::create_dir_all(dir.join("storage")).expect("storage dir");
    let cfg = format!(
        "[reticulum]\n  enable_transport = yes\n  share_instance = no\n\n\
         [logging]\n  loglevel = 4\n\n\
         [interfaces]\n  [[Public]]\n    type = TCPServerInterface\n    \
         enabled = yes\n    listen_ip = 127.0.0.1\n    listen_port = {port}\n    \
         ingress_control = no\n    \
         max_connections_per_source = {PER_SOURCE_CAP}\n    \
         silent_timeout_secs = {SILENT_TIMEOUT_SECS}\n"
    );
    fs::write(dir.join("config"), cfg).expect("write config");
    let mut cmd = Command::new(release_bin("lnsd"));
    cmd.arg("--config")
        .arg(dir)
        .env("LEVICULUM_NO_AUTO", "1")
        .stdout(Stdio::from(
            fs::File::create(dir.join("lnsd-stdout.log")).expect("stdout log"),
        ))
        .stderr(Stdio::from(
            fs::File::create(dir.join("lnsd-stderr.log")).expect("stderr log"),
        ));
    // SAFETY: setrlimit is async-signal-safe; the closure allocates nothing.
    unsafe {
        cmd.pre_exec(|| {
            let lim = libc::rlimit {
                rlim_cur: LNSD_NOFILE as libc::rlim_t,
                rlim_max: LNSD_NOFILE as libc::rlim_t,
            };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &lim) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    spawn_supervised(cmd).expect("spawn lnsd")
}

fn log_text(dir: &Path) -> String {
    let mut text = fs::read_to_string(dir.join("lnsd-stderr.log")).unwrap_or_default();
    text.push_str(&fs::read_to_string(dir.join("lnsd-stdout.log")).unwrap_or_default());
    text
}

fn vm_rss_kb(pid: u32) -> u64 {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

fn open_fds(pid: u32) -> usize {
    fs::read_dir(format!("/proc/{pid}/fd"))
        .map(|d| d.count())
        .unwrap_or(0)
}

/// Connect from `source` with a short timeout; a full accept queue drops the
/// SYN, which is a timeout here rather than a hang.
fn connect_from(source: Ipv4Addr, server: SocketAddr) -> Option<TcpStream> {
    let sock = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).ok()?;
    sock.bind(&SocketAddr::new(IpAddr::V4(source), 0).into())
        .ok()?;
    sock.connect_timeout(&server.into(), Duration::from_millis(250))
        .ok()?;
    let stream: TcpStream = sock.into();
    stream.set_nodelay(true).ok();
    Some(stream)
}

/// A signed announce for a fresh destination, and the destination hash.
fn announce_frame() -> (Vec<u8>, [u8; 16]) {
    let identity = Identity::generate(&mut OsRng);
    let dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "mvr1190",
        &["flood"],
    )
    .expect("destination");
    let identity = dest.identity().expect("identity");
    let public_key = identity.public_key_bytes();
    let mut random_hash = [0u8; RANDOM_HASHBYTES];
    OsRng.fill_bytes(&mut random_hash[..5]);
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    random_hash[5..].copy_from_slice(&secs.to_be_bytes()[3..]);
    let app_data = b"mvr1190";

    let mut signed = Vec::new();
    signed.extend_from_slice(dest.hash().as_bytes());
    signed.extend_from_slice(&public_key);
    signed.extend_from_slice(dest.name_hash());
    signed.extend_from_slice(&random_hash);
    signed.extend_from_slice(app_data);
    let signature = identity.sign(&signed).expect("sign");

    let mut payload = Vec::new();
    payload.extend_from_slice(&public_key);
    payload.extend_from_slice(dest.name_hash());
    payload.extend_from_slice(&random_hash);
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
        hops: 0,
        transport_id: None,
        destination_hash: *dest.hash().as_bytes(),
        context: PacketContext::None,
        data: PacketData::Owned(payload),
    };
    let mut buf = [0u8; MTU];
    let len = packet.pack(&mut buf).expect("pack");
    let mut framed = Vec::new();
    frame(&buf[..len], &mut framed);
    (framed, *dest.hash().as_bytes())
}

/// The destination hashes of the announces `stream` receives before
/// `deadline`, stopping early once all of `want` are in.
fn heard_announces(
    stream: &mut TcpStream,
    want: &HashSet<[u8; 16]>,
    deadline: Instant,
) -> HashSet<[u8; 16]> {
    let mut deframer = Deframer::new();
    let mut heard = HashSet::new();
    let mut buf = [0u8; 4096];
    while Instant::now() < deadline && !want.is_subset(&heard) {
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .ok();
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                for r in deframer.process(&buf[..n]) {
                    if let DeframeResult::Frame(data) = r {
                        if let Ok(p) = Packet::unpack(&data) {
                            if p.flags.packet_type == PacketType::Announce {
                                heard.insert(p.destination_hash);
                            }
                        }
                    }
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }
    heard
}

/// Whether the peer closed `stream`: EOF or a reset within `wait`.
fn closed_by_peer(stream: &mut TcpStream, wait: Duration) -> bool {
    stream.set_read_timeout(Some(wait)).ok();
    let mut b = [0u8; 64];
    loop {
        match stream.read(&mut b) {
            Ok(0) => return true,
            // The server relays traffic to a connection it holds; reading
            // past it keeps waiting for the close.
            Ok(_) => continue,
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                return false
            }
            Err(_) => return true,
        }
    }
}

#[test]
fn a_connection_flood_from_one_source_leaves_room_for_everyone_else() {
    raise_own_nofile();
    let tmp = tempfile::Builder::new()
        .prefix("mvr_tcp_flood_")
        // Not /tmp: before the fix this daemon logs millions of lines in
        // seconds, and /tmp is a small tmpfs every lane shares.
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .expect("tempdir");
    let dir = tmp.path().to_path_buf();
    // port-handoff: lnsd binds this from its config file, in another process.
    let port = crate::ports::free_port();
    let server = SocketAddr::from(([127, 0, 0, 1], port));
    let mut lnsd = spawn_lnsd(&dir, port);
    let pid = lnsd.id();

    let ready = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect_timeout(&server, Duration::from_millis(100)).is_err() {
        assert!(
            Instant::now() < ready,
            "lnsd never listened:\n{}",
            log_text(&dir)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // The probe connection above is itself silent and from 127.0.0.1; it is
    // closed by now or will be by the silent timeout.
    std::thread::sleep(Duration::from_millis(300));
    let rss_before = vm_rss_kb(pid);
    let fds_before = open_fds(pid);

    // The flood: one source, silent connections, more than the limit.
    let flood_source = Ipv4Addr::new(127, 0, 0, 2);
    let mut flood: Vec<TcpStream> = Vec::new();
    for _ in 0..FLOOD_CONNECTIONS {
        if let Some(s) = connect_from(flood_source, server) {
            flood.push(s);
        }
    }
    std::thread::sleep(Duration::from_millis(500));
    let fds_flooded = open_fds(pid);

    // How many of the flood's connections the server still holds: the rest
    // it closed right after accepting.
    let mut held = 0usize;
    for s in flood.iter_mut() {
        if !closed_by_peer(s, Duration::from_millis(5)) {
            held += 1;
        }
    }

    // The legitimate clients connect and announce.
    let mut clients = Vec::new();
    let mut refused = 0usize;
    for _ in 0..CLIENTS {
        match connect_from(Ipv4Addr::LOCALHOST, server) {
            Some(s) => clients.push(s),
            None => refused += 1,
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    let mut hashes = Vec::new();
    for c in clients.iter_mut() {
        let (framed, hash) = announce_frame();
        c.write_all(&framed).ok();
        hashes.push(hash);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut talked = 0usize;
    for (i, c) in clients.iter_mut().enumerate() {
        let want: HashSet<[u8; 16]> = hashes
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, h)| *h)
            .collect();
        if want.is_subset(&heard_announces(c, &want, deadline)) {
            talked += 1;
        }
    }

    // The flood's held connections close within the silent timeout; the
    // clients, which spoke, stay.
    std::thread::sleep(Duration::from_secs(SILENT_TIMEOUT_SECS + 2));
    let mut flood_still_open = 0usize;
    for s in flood.iter_mut() {
        if !closed_by_peer(s, Duration::from_millis(5)) {
            flood_still_open += 1;
        }
    }
    let mut clients_closed = 0usize;
    for c in clients.iter_mut() {
        if closed_by_peer(c, Duration::from_millis(5)) {
            clients_closed += 1;
        }
    }
    drop(flood);
    std::thread::sleep(Duration::from_millis(500));
    let rss_after = vm_rss_kb(pid);
    let fds_after = open_fds(pid);

    let _ = lnsd.kill();
    let _ = lnsd.wait();
    let log = log_text(&dir);
    let accept_error_lines = log.matches("TCP accept error").count();
    let backoff_lines = log.matches("TCP_ACCEPT_BACKOFF").count();
    let capped_lines = log.matches("TCP_SOURCE_CAPPED").count();
    let silent_lines = log.matches("TCP_SILENT_CLOSED").count();

    let summary = format!(
        "FLOOD1190 flood_connected={} held={held} fds_before={fds_before} \
         fds_flooded={fds_flooded} fds_after={fds_after} clients_refused={refused} \
         clients_talked={talked}/{CLIENTS} flood_still_open={flood_still_open} \
         clients_closed={clients_closed} rss_before_kb={rss_before} \
         rss_after_kb={rss_after} accept_error_lines={accept_error_lines} \
         backoff_lines={backoff_lines} capped_lines={capped_lines} \
         silent_lines={silent_lines}",
        FLOOD_CONNECTIONS
    );
    println!("{summary}");

    assert!(
        held <= PER_SOURCE_CAP,
        "the flood source must hold at most {PER_SOURCE_CAP} connections: {summary}"
    );
    assert_eq!(
        talked, CLIENTS,
        "every legitimate client must hear the others: {summary}"
    );
    assert_eq!(
        flood_still_open, 0,
        "the silent timeout must close the flood: {summary}"
    );
    assert_eq!(
        clients_closed, 0,
        "a client that spoke is never closed as silent: {summary}"
    );
    assert!(
        accept_error_lines + backoff_lines <= 2,
        "the accept loop must not log an error storm: {summary}"
    );
    assert!(
        (1..=2).contains(&capped_lines),
        "the cap is reported once per source per minute: {summary}"
    );
    assert!(
        rss_after <= rss_before + 4096,
        "the daemon's memory must stay flat: {summary}"
    );
}
