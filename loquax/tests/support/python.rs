//! The Python LXMF peer and the in-process Rust daemon loquax attaches to,
//! shared by `python_interop.rs` and `home_node.rs`.

// Each test crate includes this module and uses its own part of it.
#![allow(dead_code)]

use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::ReticulumNode;

#[path = "python_peer.rs"]
mod python_peer;

pub use python_peer::PythonPeer;

pub const LNMSG: &str = env!("CARGO_BIN_EXE_loquax");

/// The Rust daemon `loquax` attaches to, plus everything it needs alive.
pub struct Mesh {
    pub daemon: ReticulumNode,
    pub instance: String,
    pub home: tempfile::TempDir,
    /// The port of the daemon's TCP server on 127.0.0.1, where a leaf
    /// (the Android app's node) dials in: [`Mesh::start_serving`] only.
    pub tcp_port: Option<u16>,
    /// The daemon's Reticulum config directory, its storage under
    /// `storage/`: every run gets it as `--config`, as a user's loquax
    /// finds its lnsd, and takes the daemon's RPC key from it (1119).
    pub config_dir: tempfile::TempDir,
}

impl Mesh {
    pub async fn start(python_rns_port: u16) -> Self {
        Self::start_many(&[python_rns_port]).await
    }

    /// [`Mesh::start`] with a TCP link to each of several Python daemons,
    /// so they reach each other through the Rust transport.
    pub async fn start_many(python_rns_ports: &[u16]) -> Self {
        Self::start_with(python_rns_ports, false).await
    }

    /// [`Mesh::start_many`] with a TCP server on a free loopback port too,
    /// as a home node's lnsd has one for its phones.
    pub async fn start_serving(python_rns_ports: &[u16]) -> Self {
        Self::start_with(python_rns_ports, true).await
    }

    async fn start_with(python_rns_ports: &[u16], serve: bool) -> Self {
        // Unique per process AND per call: two tests in one binary run in
        // parallel threads and would otherwise fight over one abstract socket.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let instance = format!(
            "loquax-interop-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let config_dir = tempfile::tempdir().expect("daemon config dir");
        let mut builder = ReticulumNodeBuilder::new()
            .enable_transport(true)
            .share_instance(true)
            .instance_name(instance.clone());
        for port in python_rns_ports {
            let python: SocketAddr = format!("127.0.0.1:{port}")
                .parse()
                .expect("loopback address");
            builder = builder.add_tcp_client(python);
        }
        let mut tcp_port = None;
        if serve {
            // Bound here, so the port is known before the daemon runs and
            // no other test can take it in between.
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback TCP port");
            tcp_port = Some(listener.local_addr().expect("its address").port());
            builder = builder.add_tcp_server_listener(listener);
        }
        let mut daemon = builder
            .storage_path(config_dir.path().join("storage"))
            .build_sync()
            .expect("build the Rust daemon");
        daemon.start().await.expect("start the Rust daemon");
        // Let the abstract Unix socket listener and the TCP link come up
        // before a client connects; the same wait `lnomad`'s integ test uses.
        tokio::time::sleep(Duration::from_millis(500)).await;

        Self {
            daemon,
            instance,
            home: tempfile::tempdir().expect("loquax state dir"),
            tcp_port,
            config_dir,
        }
    }

    /// Run `loquax send` against this mesh with the mesh's own state dir.
    pub async fn send(&self, args: Vec<String>, stdin: Vec<u8>) -> Sent {
        let home = self.home.path().to_path_buf();
        self.run("send", home, args, stdin).await
    }

    /// Run one `loquax` subcommand against this mesh, on a blocking thread so
    /// the daemon's runtime keeps serving the IPC socket while the child
    /// talks to it. `home` is the client's state directory — the mailbox
    /// tests run two clients (a sender and a recipient) against one mesh,
    /// and each needs its own identity and seen store.
    ///
    /// The child writes its structured event log into the state directory and
    /// [`Sent::log`] carries it back, so a failing assertion prints the state
    /// sequence that produced it rather than a bare exit code. That is what the
    /// log is for (`docs/src/concepts/loquax-architecture.md` §7).
    pub async fn run(
        &self,
        subcommand: &'static str,
        home: PathBuf,
        args: Vec<String>,
        stdin: Vec<u8>,
    ) -> Sent {
        let instance = self.instance.clone();
        let config_dir = self.config_dir.path().to_path_buf();
        // A path among the arguments (`--photo FILE`) is no directory of
        // the log's name, and a long one no longer than a name may be.
        let named: String = args.join("-").replace('/', "_").chars().take(160).collect();
        let log = home.join(format!("events-{subcommand}-{named}.log"));
        let read_log = log.clone();
        let sent = tokio::task::spawn_blocking(move || {
            let mut command = Command::new(LNMSG);
            command
                .arg(subcommand)
                .args(&args)
                .args(["--instance", &instance])
                .arg("--config")
                .arg(&config_dir)
                .env("LNMSG_HOME", &home)
                .env("LEVICULUM_EVENT_LOG", &log)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            // Supervised for the same reason the Python daemon is: a client
            // that outlives a killed run keeps talking to the shared instance.
            let mut child =
                leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
            child
                .stdin
                .take()
                .expect("stdin is piped")
                .write_all(&stdin)
                .expect("write the body");
            let output = child.wait_with_output().expect("wait for loquax");
            Sent {
                code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                log: String::new(),
            }
        })
        .await
        .expect("the loquax child thread");
        Sent {
            log: std::fs::read_to_string(&read_log).unwrap_or_default(),
            ..sent
        }
    }
}

/// One `loquax send` run, as a script and a debugger see it.
pub struct Sent {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// The child's structured event log, for failure messages.
    pub log: String,
}

impl Sent {
    /// The message id, from the `id=` field of this run's `LNMSG_ENQUEUED`
    /// line. Since 2026-08-21 the id is not on stdout, and this is the
    /// documented way to get it back out of a run.
    pub fn enqueued_id(&self) -> String {
        // The event log of a home is appended to by every run in it: this
        // run's line is the last one.
        let line = self
            .log
            .lines()
            .rev()
            .find(|line| line.starts_with("LNMSG_ENQUEUED "))
            .unwrap_or_else(|| panic!("no LNMSG_ENQUEUED line in this run: {self}"));
        let id = line
            .split_whitespace()
            .find_map(|token| token.strip_prefix("id="))
            .unwrap_or_else(|| panic!("no id= field in {line}"));
        assert_eq!(
            id.len(),
            64,
            "an LXMF message id is 32 bytes of hex: {id:?}"
        );
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit()),
            "the id must be hex: {id:?}"
        );
        id.to_string()
    }
}

impl std::fmt::Display for Sent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "exit {:?}\nstdout: {:?}\nstderr: {}\nevents:\n{}",
            self.code, self.stdout, self.stderr, self.log
        )
    }
}
