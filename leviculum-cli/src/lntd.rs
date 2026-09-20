//! `lntd` — the address a board reports telemetry to, and the file it ends up
//! in.
//!
//! An LNode already knows how to report: `lnflash` asks for an LXMF address at
//! flash time and `--set-telemetry` changes it later without reflashing
//! (Codeberg #236). This is the thing at that address. It runs one LXMF
//! delivery destination, announces it so a board can resolve the key over the
//! air, prints its hash on startup so it can be pasted into `lnflash`, and
//! writes every report that arrives into a SQLite file.
//!
//! # Its one job
//!
//! Lose nothing. That is why every row carries the raw Telemeter blob beside
//! the columns we could fill, why a blob we cannot decode is a row saying so
//! rather than a dropped message, and why the LXMF message id is a UNIQUE
//! column so a restart across a write neither loses a report nor doubles it.
//! See [`lntd_store`] for the storage rules and the tests that hold them.
//!
//! It is not a visualisation and not a query surface: the operator reads the
//! file with `sqlite3`, or whatever else he likes, while the daemon keeps
//! writing. WAL mode is what makes that concurrent read safe.
//!
//! # How it attaches
//!
//! As a shared-instance client of a daemon that is already running — `lnsd`,
//! or Python's `rnsd` — exactly as `lnmsg`, `lnpnd` and `lblogd` do. It
//! starts no Reticulum stack of its own, so the interfaces a board is reached
//! over are configured once, in the daemon's config file.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{Receiver, Sender};

use clap::Parser;
use leviculum_core::identity::Identity;
use leviculum_core::node::NodeEvent;
use leviculum_core::transport::TickOutput;
use leviculum_core::DestinationHash;
use leviculum_lxmf::router::{LxmfRouter, RouterConfig, RouterEvent, RouterOutput};
use leviculum_lxmf::{announce, LxmfNode, LxmfNodeConfig};
use leviculum_std::config::Config;
use leviculum_std::driver::{CoreProcessor, ReticulumNodeBuilder, StdNodeCore};

mod lntd_store;

use lntd_store::{ingest, Ingest, Report, Store, StoreError};

/// Environment override for the state directory, the same lever `lnmsg`
/// offers under `LNMSG_HOME`. A test gets an isolated address per run from
/// it; an operator has no reason to touch it.
const HOME_ENV: &str = "LNTD_HOME";

/// The identity file under the home directory. It *is* the address boards are
/// flashed with, so it is minted once and never silently replaced.
const IDENTITY_FILE: &str = "identity";

/// The database file under the home directory, unless `--database` names one.
const DATABASE_FILE: &str = "telemetry.db";

/// How often the collector announces, after the one it sends immediately.
///
/// A board given the hash alone (`lnflash`'s hash-only path) has to hear this
/// announce before it can encrypt anything: that is what turns its
/// `state=awaiting-key` into a delivery. Once heard, the board has the
/// identity and does not need another, so the interval only bounds how long a
/// board that came up *after* the last announce waits.
///
/// Half an hour rather than `lnpnd`'s six hours, because a node flashed in the
/// field and then left there should not be silent for a working day; and not
/// minutes, because an announce crosses every interface the daemon has,
/// including LoRa ones whose airtime is somebody's duty cycle. A bench where
/// the wait is the annoyance sets `--announce-interval`.
const DEFAULT_ANNOUNCE_INTERVAL_SECS: u64 = 30 * 60;

/// How soon the collector asks the driver to come back. It has a timer of its
/// own (the announce) and the router has its retries, so it always has
/// something to wake for.
const POLL_INTERVAL_MS: u64 = 1_000;

#[derive(Parser, Debug)]
#[command(
    name = "lntd",
    version = env!("LEVICULUM_VERSION"),
    about = "LXMF telemetry collector for Reticulum",
    long_about = "Runs an LXMF destination that receives telemetry reports and \
                  writes them to a SQLite file.\n\n\
                  lntd attaches to a Reticulum daemon that is already running \
                  (lnsd, or Python's rnsd) the way lnmsg and lnpnd do; it does \
                  not start a stack of its own. It prints its LXMF address at \
                  startup: hand that to `lnflash --set-telemetry` (or to the \
                  telemetry prompt during a flash) and the board reports here.\n\n\
                  Every report is one row. Each sensor lntd understands gets \
                  its own column, and the raw Telemeter blob is stored beside \
                  them always -- including for sensors it does not understand \
                  yet, and for a blob it could not decode at all. Nothing is \
                  dropped.\n\n\
                  There is no query surface: read the file with sqlite3 while \
                  lntd keeps writing (the database is in WAL mode)."
)]
struct Args {
    /// Reticulum config directory (decides which shared instance to join).
    #[arg(long, value_name = "DIR")]
    config: Option<PathBuf>,

    /// Shared-instance name to connect to (overrides the Reticulum config).
    #[arg(long, value_name = "NAME")]
    instance: Option<String>,

    /// Directory for this collector's identity and database.
    #[arg(long, value_name = "DIR")]
    home: Option<PathBuf>,

    /// Database file (default: telemetry.db in the --home directory).
    #[arg(long, value_name = "FILE")]
    database: Option<PathBuf>,

    /// Display name carried in the announce.
    #[arg(long, value_name = "NAME", default_value = "lntd")]
    display_name: String,

    /// Seconds between announces.
    #[arg(long, value_name = "SECS", default_value_t = DEFAULT_ANNOUNCE_INTERVAL_SECS)]
    announce_interval: u64,

    /// Print the address and the database status, then exit without
    /// attaching to anything. What an operator runs to get the hash for
    /// `lnflash` before the field node is anywhere near a bench.
    #[arg(long)]
    address: bool,
}

/// `${LNTD_HOME:-${XDG_CONFIG_HOME:-~/.config}/lntd}` — identity, database
/// and node storage in one directory, the way `lnmsg` and `lnomad` keep
/// theirs.
fn home_dir() -> Result<PathBuf, String> {
    if let Some(explicit) = std::env::var_os(HOME_ENV)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
    {
        return Ok(explicit);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| {
            format!("no home directory to keep an address in; set {HOME_ENV} or HOME")
        })?;
    Ok(base.join("lntd"))
}

/// Same derivation `lnmsg`, `lncp`, `lnstatus` and `lnpnd` use: the instance
/// name comes from the daemon's own config file unless overridden.
fn instance_name(config_dir: &Path) -> String {
    let config_file = config_dir.join("config");
    if config_file.exists() {
        if let Ok(config) = Config::load(&config_file) {
            return config.reticulum.instance_name;
        }
    }
    "default".to_string()
}

/// Load the identity, minting one only when the file is not there at all.
///
/// A corrupt record is fatal, for `lnmsg`'s reason applied to a collector:
/// this identity is the address every board in the field was flashed with,
/// and a silent re-mint would strand all of them at once with no trace.
fn load_or_create_identity(path: &Path) -> Result<Identity, String> {
    match std::fs::read(path) {
        Ok(bytes) => leviculum_core::identity_store::decode_identity(&bytes).ok_or_else(|| {
            format!(
                "{} is not a readable identity record.\n  \
                 This file is the address your boards report to: it is not replaced \
                 automatically, because a new one would strand every board flashed \
                 with the old hash.\n  \
                 Restore it from a backup, or move it aside to start over with a \
                 new address and reflash the boards.",
                path.display()
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let identity = leviculum_std::generate_identity();
            let encoded = leviculum_core::identity_store::encode_identity(&identity)
                .ok_or("the generated identity has no private key to store")?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            std::fs::write(path, encoded).map_err(|e| format!("{}: {e}", path.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                    .map_err(|e| format!("{}: {e}", path.display()))?;
            }
            Ok(identity)
        }
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// Copy an identity through its private key bytes.
///
/// `LxmfNode::delivery_destination` consumes the identity and the address has
/// to be printed before the node exists, so the one on disk is duplicated
/// rather than moved.
fn copy_identity(identity: &Identity) -> Result<Identity, String> {
    let bytes = identity
        .private_key_bytes()
        .map_err(|e| format!("the identity has no private key: {e:?}"))?;
    Identity::from_private_key_bytes(&bytes)
        .map_err(|e| format!("could not copy the identity: {e:?}"))
}

/// The LXMF address boards are pointed at: the delivery destination hash.
fn delivery_hash(identity: &Identity) -> Result<[u8; 16], String> {
    let destination = LxmfNode::delivery_destination(copy_identity(identity)?)
        .map_err(|e| format!("delivery destination: {e:?}"))?;
    Ok(*destination.hash().as_bytes())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What the collector hands to the thread that owns the database.
///
/// One channel for rows *and* notices so the order an operator reads on
/// stderr is the order things happened, and so the hook never touches the
/// store: a SQLite commit at `synchronous = FULL` is an fsync, and the hooks
/// run under the core lock with a budget measured in milliseconds.
enum Note {
    Ready([u8; 16]),
    Announced,
    Row(Box<Report>),
    /// A message that was not a report, and why.
    Ignored {
        source: [u8; 16],
        reason: &'static str,
    },
    /// A router event that is not a delivery: the stack's own housekeeping.
    /// Kept visible because a link that never establishes is diagnosed from
    /// these lines and from nothing else.
    Router(String),
    /// Something went wrong.
    Problem(String),
}

/// The LXMF stack, once it is registered.
struct Ready {
    router: LxmfRouter,
    delivery_hash: [u8; 16],
}

enum State {
    /// The identity is loaded but nothing is registered: registering needs
    /// `&mut StdNodeCore`, and a processor is installed on the builder,
    /// before the node it will run inside exists.
    Unregistered(Box<Identity>),
    Ready(Box<Ready>),
    /// Registration failed and was reported; the daemon stays up saying so
    /// rather than pretending to collect.
    Failed,
}

/// The collector, as the driver sees it.
struct Collector {
    display_name: Vec<u8>,
    announce_interval_ms: u64,
    notes: Sender<Note>,
    state: State,
    /// `None` until the first tick after registration, which announces
    /// immediately: a board that is already reporting should not wait an
    /// interval for the key it is missing.
    next_announce_ms: Option<u64>,
}

impl Collector {
    fn new(
        display_name: Vec<u8>,
        announce_interval_secs: u64,
        notes: Sender<Note>,
        identity: Identity,
    ) -> Self {
        Self {
            display_name,
            announce_interval_ms: announce_interval_secs.saturating_mul(1_000).max(1_000),
            notes,
            state: State::Unregistered(Box::new(identity)),
            next_announce_ms: None,
        }
    }

    /// A dropped receiver can only follow a shutdown that is already under
    /// way, so a failed send is nothing to report to a channel that is gone.
    fn note(&self, note: Note) {
        let _ = self.notes.send(note);
    }

    fn register_if_needed(&mut self, core: &mut StdNodeCore) {
        let identity = match std::mem::replace(&mut self.state, State::Failed) {
            State::Unregistered(identity) => *identity,
            other => {
                self.state = other;
                return;
            }
        };
        self.state = match register(core, identity) {
            Ok(ready) => {
                self.note(Note::Ready(ready.delivery_hash));
                State::Ready(Box::new(ready))
            }
            Err(detail) => {
                self.note(Note::Problem(detail));
                State::Failed
            }
        };
    }

    fn take_ready(&mut self, core: &mut StdNodeCore) -> Option<Box<Ready>> {
        self.register_if_needed(core);
        match std::mem::replace(&mut self.state, State::Failed) {
            State::Ready(ready) => Some(ready),
            other => {
                self.state = other;
                None
            }
        }
    }

    /// Route one router output: report its events, collect its wire actions,
    /// and re-feed the core events it produced.
    fn absorb(
        &self,
        ready: &mut Ready,
        core: &mut StdNodeCore,
        first: RouterOutput,
        out: &mut TickOutput,
    ) {
        let mut queue = std::collections::VecDeque::from([first]);
        let mut rounds = 0usize;
        while let Some(router_output) = queue.pop_front() {
            for event in router_output.events {
                self.report(event);
            }
            let mut core_output = router_output.core;
            let events = std::mem::take(&mut core_output.events);
            out.merge(core_output);

            rounds += 1;
            // The same bound `leviculum-lxmf-node` carries: re-feeding the
            // router its own output terminates in practice, and a cycle that
            // did not would spin under the core lock.
            let refeed = rounds <= 8;
            for event in events {
                if refeed {
                    match ready.router.handle_event(core, &event) {
                        Ok(next) => queue.push_back(next),
                        Err(e) => self.note(Note::Problem(format!("router handle_event: {e:?}"))),
                    }
                }
                out.events.push(event);
            }
        }
    }

    /// The only router event this daemon exists for, plus diagnostics.
    fn report(&self, event: RouterEvent) {
        match event {
            RouterEvent::MessageReceived(message) => {
                let received_at = unix_seconds();
                match ingest(&message, received_at) {
                    Ingest::Row(report) => self.note(Note::Row(report)),
                    Ingest::EmptyMap => self.note(Note::Ignored {
                        source: message.source_hash,
                        reason: "empty telemetry map",
                    }),
                    Ingest::NotTelemetry => self.note(Note::Ignored {
                        source: message.source_hash,
                        reason: "no telemetry field",
                    }),
                }
            }
            // A collector sends nothing, so none of the rest is load-bearing
            // here — but it is the only window onto a link that never
            // establishes.
            other => self.note(Note::Router(format!("{other:?}"))),
        }
    }

    fn announce_if_due(
        &mut self,
        ready: &mut Ready,
        core: &mut StdNodeCore,
        now_ms: u64,
        out: &mut TickOutput,
    ) {
        if self.next_announce_ms.is_some_and(|due| now_ms < due) {
            return;
        }
        self.next_announce_ms = Some(now_ms.saturating_add(self.announce_interval_ms));
        // Three-element LXMF delivery app data: display name, stamp cost,
        // compression support. `None` is "no stamp cost" — a field node has
        // no cycles to spend mining for the privilege of reporting.
        let app_data = announce::delivery(Some(&self.display_name), None);
        let hash = DestinationHash::new(ready.delivery_hash);
        match core.announce_destination(&hash, Some(&app_data)) {
            Ok(core_output) => {
                self.absorb(
                    ready,
                    core,
                    RouterOutput {
                        core: core_output,
                        events: Vec::new(),
                    },
                    out,
                );
                self.note(Note::Announced);
            }
            Err(e) => self.note(Note::Problem(format!("announce failed: {e:?}"))),
        }
    }
}

impl CoreProcessor for Collector {
    fn on_event(&mut self, core: &mut StdNodeCore, event: &NodeEvent) -> TickOutput {
        let mut out = TickOutput::empty();
        let Some(mut ready) = self.take_ready(core) else {
            return out;
        };
        match ready.router.handle_event(core, event) {
            Ok(output) => self.absorb(&mut ready, core, output, &mut out),
            Err(e) => self.note(Note::Problem(format!("router handle_event: {e:?}"))),
        }
        self.state = State::Ready(ready);
        out
    }

    fn on_tick(&mut self, core: &mut StdNodeCore, now_ms: u64) -> TickOutput {
        let mut out = TickOutput::empty();
        let Some(mut ready) = self.take_ready(core) else {
            return out;
        };
        self.announce_if_due(&mut ready, core, now_ms, &mut out);
        match ready.router.tick(core) {
            Ok(output) => self.absorb(&mut ready, core, output, &mut out),
            Err(e) => self.note(Note::Problem(format!("router tick: {e:?}"))),
        }
        self.state = State::Ready(ready);
        let poll = now_ms.saturating_add(POLL_INTERVAL_MS);
        out.next_deadline_ms = Some(match out.next_deadline_ms {
            Some(existing) => existing.min(poll),
            None => poll,
        });
        out
    }
}

/// Unix seconds, as a float, for the row's receive time.
///
/// A clock before the epoch is not a time this daemon can record; 0.0 says
/// "the host had no calendar" honestly rather than wrapping into the future.
fn unix_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Mint the delivery destination and the router that drives it.
fn register(core: &mut StdNodeCore, identity: Identity) -> Result<Ready, String> {
    let identity_hash = *identity.hash();
    let copy = copy_identity(&identity)?;
    let destination =
        LxmfNode::delivery_destination(copy).map_err(|e| format!("delivery destination: {e:?}"))?;
    let delivery_hash = *destination.hash().as_bytes();
    let node = LxmfNode::register(core, destination, LxmfNodeConfig::default())
        .map_err(|e| format!("register delivery destination: {e:?}"))?;
    // No propagation client: a collector is the endpoint boards address
    // directly, and a second local destination would be surface this daemon
    // does not use.
    let router = LxmfRouter::new(node, identity_hash, RouterConfig::default());
    Ok(Ready {
        router,
        delivery_hash,
    })
}

/// The thread that owns the database.
///
/// Everything upstream of it only pushes onto a queue, so no fsync can ever
/// happen under the core lock. It returns when the channel closes, which is
/// what makes shutdown a drain rather than a truncation: `main` drops the
/// sender, joins this, and only then exits.
fn write_notes(store: Store, notes: Receiver<Note>) {
    for note in notes {
        match note {
            Note::Ready(hash) => {
                eprintln!("lntd: collecting on LXMF address {}", hex(&hash));
            }
            Note::Announced => eprintln!("lntd: announce sent"),
            Note::Row(report) => {
                let source = hex(&report.source_hash);
                match store.insert(&report) {
                    Ok(true) => eprintln!(
                        "lntd: stored report from {source} ({} raw bytes)",
                        report.raw.len()
                    ),
                    Ok(false) => {
                        eprintln!("lntd: report from {source} was already stored")
                    }
                    // Not fatal, and deliberately loud: the row is lost and
                    // the operator has to know which one and why.
                    Err(error) => eprintln!("lntd: COULD NOT STORE report from {source}: {error}"),
                }
            }
            Note::Ignored { source, reason } => {
                eprintln!("lntd: ignored message from {}: {reason}", hex(&source))
            }
            Note::Router(detail) => eprintln!("lntd: router: {detail}"),
            Note::Problem(detail) => eprintln!("lntd: {detail}"),
        }
    }
}

/// What to say when there is no shared instance to attach to. `lntd` starts
/// no Reticulum stack of its own, so this is a missing prerequisite rather
/// than a fault in the collector.
fn no_daemon(instance: &str, error: impl std::fmt::Display) -> String {
    format!(
        "could not join the Reticulum shared instance named '{instance}'.\n  \
         lntd talks to a daemon that is already running; start one with `lnsd` \
         (or Python's `rnsd`), or name another instance with --instance / \
         --config.\n  The stack said: {error}"
    )
}

fn failure(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("lntd: {message}");
    ExitCode::FAILURE
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    match run(args).await {
        Ok(code) => code,
        Err(message) => failure(message),
    }
}

async fn run(args: Args) -> Result<ExitCode, String> {
    let home = match args.home {
        Some(home) => home,
        None => home_dir()?,
    };
    let database = args.database.unwrap_or_else(|| home.join(DATABASE_FILE));

    // The store first: a database that cannot be opened is a collector that
    // would receive reports and drop them, which is the one outcome this
    // daemon must never have.
    let store = Store::open(&database).map_err(|e: StoreError| e.to_string())?;
    let rows = store.row_count().map_err(|e| e.to_string())?;
    eprintln!(
        "lntd: database {} (schema {}, {rows} row(s))",
        database.display(),
        store.schema_version().map_err(|e| e.to_string())?
    );
    if let Some(last) = store.last_received_at().map_err(|e| e.to_string())? {
        eprintln!("lntd: last report received at {last} (unix seconds)");
    }

    let identity = load_or_create_identity(&home.join(IDENTITY_FILE))?;
    let address = delivery_hash(&identity)?;
    // The line an operator copies into `lnflash --set-telemetry`. On stdout,
    // alone, so `lntd --address | xargs lnflash --set-telemetry` works.
    println!("{}", hex(&address));
    eprintln!("lntd: hand that address to `lnflash --set-telemetry`");

    if args.address {
        return Ok(ExitCode::SUCCESS);
    }

    let config_dir = args.config.unwrap_or_else(Config::default_config_dir);
    let instance = args.instance.unwrap_or_else(|| instance_name(&config_dir));

    let (notes_tx, notes_rx) = std::sync::mpsc::channel::<Note>();
    let writer = std::thread::spawn(move || write_notes(store, notes_rx));

    let collector = Collector::new(
        args.display_name.into_bytes(),
        args.announce_interval,
        notes_tx.clone(),
        identity,
    );

    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(&instance)
        .storage_path(home.join("storage"))
        .core_processor(collector)
        // The collector consumes events on the processor tap; nothing here
        // ever takes the driver's application receiver, so enabling it would
        // only fill a queue with no reader (Codeberg #419).
        .without_events()
        .build()
        .await
        .map_err(|error| no_daemon(&instance, error))?;
    // The absence of a daemon surfaces here rather than at `build`, so both
    // get the same explanation: the stack's own message names the socket, and
    // this names the thing to start.
    node.start()
        .await
        .map_err(|error| no_daemon(&instance, error))?;

    tokio::signal::ctrl_c()
        .await
        .map_err(|e| format!("signal handling: {e}"))?;
    eprintln!("lntd: shutting down");
    let _ = node.stop().await;

    // Drop every sender, then wait for the writer to drain. This is the
    // whole of "survives a restart without losing rows" on the shutdown
    // side: a report already handed over is written before the process
    // returns.
    drop(notes_tx);
    let _ = writer.join();
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    use clap::CommandFactory as _;

    #[test]
    fn the_command_line_is_well_formed() {
        Args::command().debug_assert();
    }

    /// The address is a pure function of the identity file, which is what
    /// lets `--address` answer before any daemon is running and what makes a
    /// board flashed today still deliver tomorrow.
    #[test]
    fn the_address_follows_the_identity_across_runs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("identity");
        let first = load_or_create_identity(&path).expect("mint");
        let second = load_or_create_identity(&path).expect("load");
        assert_eq!(
            delivery_hash(&first).expect("hash"),
            delivery_hash(&second).expect("hash"),
            "a restart must not change the address boards were flashed with"
        );
    }

    /// `lnflash` asks for "32 hex characters"; this is where they come from.
    #[test]
    fn the_printed_address_is_what_lnflash_asks_for() {
        let dir = tempfile::tempdir().expect("tempdir");
        let identity = load_or_create_identity(&dir.path().join("identity")).expect("mint");
        let printed = hex(&delivery_hash(&identity).expect("hash"));
        assert_eq!(printed.len(), 32);
        assert!(printed.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// A corrupt record must not quietly become a new address: every board
    /// in the field is flashed with the old one.
    #[test]
    fn a_corrupt_identity_is_fatal_and_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("identity");
        std::fs::write(&path, b"not an identity record").expect("write");
        assert!(load_or_create_identity(&path).is_err());
        assert_eq!(
            std::fs::read(&path).expect("read back"),
            b"not an identity record",
            "the file the operator has to rescue must still be there"
        );
    }
}
