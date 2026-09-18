//! Installing a propagation node must not put a new identity on the air.
//!
//! The 2026-09-17 install on the production host did exactly that: `dpkg`
//! started the unit, the daemon found no identity, minted one, and
//! announced the resulting address on the public network for 33 seconds
//! before anybody noticed. Two decisions came out of it, and this file
//! holds both to their word.
//!
//! 1. **The package does not start the daemon.** `dpkg` has no business
//!    starting a service whose address the operator has not yet decided.
//!    The mechanism is cargo-deb's `systemd-units.start = false`; the
//!    generated `postinst` is checked on the built `.deb` by
//!    `scripts/verify-deb-packaging.sh`, which is deliberately outside
//!    every test tier because it needs a `build-deb` run. So the metadata
//!    that produces it is pinned here, where the fast gate reaches it.
//!
//! 2. **A mint is announced before it can announce itself.** lnpnd still
//!    mints on a genuine first start — refusing would strand the operator
//!    of a new node, since this package deliberately ships no minting tool
//!    — but it says so at the moment of minting, with the address it just
//!    created, and it does that before it joins the shared instance.
//!
//! And underneath both: an identity that is already there is never
//! touched. `lxmd_identity_takeover.rs` proves that for an identity Python
//! wrote; here it is proved byte-for-byte, and without needing the
//! reference checked out.

use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use leviculum_std::process::spawn_supervised;
use lnpnd::identity::{load_or_create, Provenance, CREATED_EVENT};

/// A `tracing` writer that keeps what was written, so a test can assert on
/// the log line itself rather than on a proxy for it.
#[derive(Clone, Default)]
struct LogCapture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl LogCapture {
    /// The captured log with `tracing`'s quoting of string fields removed,
    /// so `path=/tmp/x` reads the way the structured-log format spells it.
    fn unquoted(&self) -> String {
        let bytes = self.0.lock().expect("capture lock").clone();
        String::from_utf8_lossy(&bytes).replace('"', "")
    }
}

impl std::io::Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = LogCapture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Run `body` with a capturing subscriber installed, and return the log.
fn captured(body: impl FnOnce()) -> String {
    let capture = LogCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, body);
    capture.unquoted()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A shared-instance name nothing can be listening on: the socket is
/// `\0rns/<name>`, so a name unique to this process and moment makes the
/// daemon's connect fail immediately instead of depending on whether this
/// host happens to run an lnsd.
fn unreachable_instance() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("a clock after 1970")
        .as_nanos();
    format!("lnpnd-first-start-{}-{nanos}", std::process::id())
}

/// The daemon child, reaped whichever way `run_daemon` leaves.
///
/// `std::process::Child` neither kills nor waits in its own `Drop`, so every
/// `expect` between the spawn and the wait below was a path that unwound with
/// the daemon still alive and holding the descriptors it inherited from this
/// test binary — `stderr.take()`, and the `try_wait()` inside the poll loop.
/// That is the shape that cost a lander five minutes on 2026-09-18: a
/// panicking test left children behind, and a lock descriptor they had
/// inherited outlived the run that took it.
///
/// This covers the unwinding half, where a destructor still runs;
/// [`spawn_supervised`] covers the other half, where one does not — an abort
/// or a `SIGKILL` of the harness, which the kernel answers with `PDEATHSIG`.
struct Reaped(std::process::Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        // Both are no-ops once the poll loop below has reaped the child; std
        // refuses to signal a `Child` it has already waited on, so this
        // cannot reach a recycled pid.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start the daemon against `config_dir` and give it back once it has
/// exited, with everything it said on stderr.
///
/// It is expected to fail: there is no shared instance by that name. The
/// point is what it did *before* it found that out.
fn run_daemon(config_dir: &Path) -> (std::process::ExitStatus, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lnpnd"));
    cmd.arg("--config")
        .arg(config_dir)
        .arg("--instance")
        .arg(unreachable_instance())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = Reaped(spawn_supervised(cmd).expect("the daemon binary runs"));

    let mut stderr = child.0.stderr.take().expect("piped stderr");
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });

    // Generous, because this only has to outlast a connect to an abstract
    // socket nobody is bound to. A hang here is a finding, not a flake, so
    // the timeout kills and fails rather than retrying.
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.0.try_wait().expect("the child is waitable") {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => break None,
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let status = match status {
        Some(status) => status,
        None => {
            // Killed here rather than left to `Drop`, because the reader
            // thread joined below only finishes once the daemon's end of the
            // stderr pipe is closed.
            let _ = child.0.kill();
            let _ = child.0.wait();
            let text = reader.join().expect("the stderr reader finishes");
            panic!("lnpnd did not exit within 30 s without a shared instance:\n{text}");
        }
    };
    let text = reader.join().expect("the stderr reader finishes");
    (status, text)
}

/// Where the daemon gave up for want of a shared instance, whichever of
/// the two ways it can: the connect itself (`daemon`'s builder arm) or the
/// node start behind it. Both are lnpnd's own wording in `main.rs`; the
/// test wants the earlier of them, because that is the first moment the
/// run could have touched the network.
fn mesh_failure_at(text: &str) -> Option<usize> {
    [
        "could not join the Reticulum shared instance",
        "node start:",
    ]
    .iter()
    .filter_map(|marker| text.find(marker))
    .min()
}

/// The metadata that decides whether `dpkg` starts the daemon. Read off
/// the manifest rather than assumed, because the failure mode being
/// guarded against is somebody flipping this back to the cargo-deb
/// default while every test stays green.
#[test]
fn the_package_enables_the_unit_but_does_not_start_it() {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("the crate's own manifest is readable");

    let section = manifest
        .split("[package.metadata.deb.systemd-units]")
        .nth(1)
        .expect("lnpnd declares systemd-units metadata");
    // Stop at the next table header, so a key from a later section cannot
    // satisfy an assertion about this one.
    let section = section.split("\n[").next().expect("a section body");
    let keys: Vec<&str> = section
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();

    assert!(
        keys.contains(&"start = false"),
        "installing must not start lnpnd: with `start = true` cargo-deb \
         writes a postinst that runs `deb-systemd-invoke start`, and the \
         daemon comes up before the operator can place an identity.\n\
         systemd-units section was:\n{section}"
    );
    assert!(
        keys.contains(&"enable = true"),
        "the unit still has to be enabled, so the node comes back after a \
         reboot; not starting it is about *this* moment, not about \
         forever.\nsystemd-units section was:\n{section}"
    );
    assert!(
        keys.contains(&"restart-after-upgrade = true"),
        "with start = false this is what makes an upgrade a `try-restart`: \
         a node that was running comes back, a stopped one stays \
         stopped.\nsystemd-units section was:\n{section}"
    );
}

/// The first start mints, and the mint is on the record before anything
/// else happens.
#[test]
fn a_first_start_mints_and_says_which_address_it_made() {
    let dir = tempfile::tempdir().expect("temp config dir");
    let path = dir.path().join("identity");

    let mut minted = None;
    let log = captured(|| {
        let (identity, provenance) = load_or_create(&path).expect("a first start mints");
        assert_eq!(
            provenance,
            Provenance::Created,
            "no file was there, so this is a creation"
        );
        minted = Some(hex(&lnpnd::identity::propagation_hash(&identity)));
    });
    let minted = minted.expect("the mint produced an address");

    assert!(path.is_file(), "the minted identity is written to {path:?}");
    assert!(
        log.contains(CREATED_EVENT),
        "a new node address must appear in the log at the moment it is \
         created; the log said:\n{log}"
    );
    assert!(
        log.contains(&format!("propagation={minted}")),
        "the log line has to carry the address that was created — that is \
         the whole point, since the address is otherwise only in a file \
         nobody reads. The log said:\n{log}"
    );
    assert!(
        log.contains(&format!("path={}", path.display())),
        "and the file it was written to, so the operator knows what to \
         replace. The log said:\n{log}"
    );
}

/// The one thing that must never happen twice.
#[test]
fn an_existing_identity_is_never_replaced() {
    let dir = tempfile::tempdir().expect("temp config dir");
    let path = dir.path().join("identity");

    let first = captured(|| {
        load_or_create(&path).expect("a first start mints");
    });
    assert!(first.contains(CREATED_EVENT), "the first start minted");
    let before = std::fs::read(&path).expect("the identity is readable");

    let second = captured(|| {
        let (_, provenance) = load_or_create(&path).expect("a second start loads");
        assert_eq!(
            provenance,
            Provenance::Loaded,
            "a file that is there is loaded, never re-minted"
        );
    });

    assert_eq!(
        std::fs::read(&path).expect("the identity is still readable"),
        before,
        "the identity file is the node's address: loading it must leave \
         every byte where it was"
    );
    assert!(
        !second.contains(CREATED_EVENT),
        "and a load must not claim a creation, or the line stops meaning \
         anything. The second start said:\n{second}"
    );
}

/// The same two properties through the real binary, which is where the
/// ordering claim lives: the mint has to be logged *before* the daemon
/// reaches the network, not merely somewhere in the run.
#[test]
fn the_daemon_reports_a_mint_before_it_joins_the_mesh() {
    let dir = tempfile::tempdir().expect("temp config dir");

    let (status, first) = run_daemon(dir.path());
    assert!(
        !status.success(),
        "without a shared instance the daemon exits non-zero; it did not, \
         so this run proves nothing about the ordering:\n{first}"
    );
    assert!(
        first.contains(CREATED_EVENT),
        "the mint is logged even though the run never got as far as the \
         mesh — the log line comes first, so no address can be announced \
         without it. The daemon said:\n{first}"
    );
    let join_at = mesh_failure_at(&first).unwrap_or_else(|| {
        panic!(
            "the run is only evidence if it did reach the shared instance \
             and fail there. The daemon said:\n{first}"
        )
    });
    // Ordering, stated as an index rather than as an intention.
    let mint_at = first.find(CREATED_EVENT).expect("the mint line");
    assert!(
        mint_at < join_at,
        "the mint must be reported before the daemon touches the shared \
         instance. The daemon said:\n{first}"
    );

    let identity = dir.path().join("identity");
    let written = std::fs::read(&identity).expect("the first start wrote an identity");

    let (status, second) = run_daemon(dir.path());
    assert!(
        !status.success(),
        "the second run also has no daemon to join"
    );
    assert!(
        !second.contains(CREATED_EVENT),
        "restarting a node must not mint anything — this is the upgrade \
         case, and a mint here would hand every peer a stranger. The \
         daemon said:\n{second}"
    );
    assert_eq!(
        std::fs::read(&identity).expect("the identity survives"),
        written,
        "and the file itself is untouched"
    );
}
