//! Find the shared instance, or start one: what a client tool does before it
//! attaches (zero-config step 4, `docs/src/concepts/zero-config.md`, #471).
//!
//! A tool (lnomad, lnstatus, lncp, lnstest, lnprobe, lnpath, loquax) used to
//! fail when no daemon answered. [`find_or_start`] looks for a running shared
//! instance the way the stock tools do, and when none answers it starts
//! `lnsd` as the tool's child:
//!
//! 1. **Find.** The data channel `rns/<instance>` on the abstract AF_UNIX
//!    socket, then on TCP loopback at the port the config names, else 37428
//!    (`reference/Reticulum/RNS/Reticulum.py:683-694` puts it there under
//!    `shared_instance_type = tcp`). A config that says `tcp` is dialled on TCP
//!    only. The TCP fallback is tried only where it can be the same instance:
//!    the config names a port, or the instance is `default`, whose TCP port is
//!    37428 whatever the stack. Each channel is asked until [`FIND_BOUND`]
//!    has passed; a loopback connect succeeds or is refused at once, so the
//!    bound only covers a daemon that is binding at that moment.
//! 2. **Start.** `lnsd -c <config_dir>`, with `--instance-name <instance>`
//!    when the tool asks for a name the config does not give, spawned through
//!    [`spawn_supervised`](crate::process::spawn_supervised): on Linux the
//!    kernel kills it when the tool dies (`PR_SET_PDEATHSIG`), however the
//!    tool dies. Its stdout and stderr go to [`DAEMON_LOG`] in the config
//!    directory, never to the tool's terminal. The tool waits until the
//!    channel accepts, at most [`START_BOUND`].
//! 3. **Report.** One line, `INSTANCE found=yes via=unix|tcp pid=none` or
//!    `INSTANCE started=yes via=unix|tcp pid=<pid>`, logged as the catalogued
//!    `INSTANCE` event and, as the caller's [`Report`] asks, on stderr.
//!
//! **Two tools at once.** The find and the start run under an exclusive
//! `flock` on [`START_LOCK`] in the config directory, so of two tools on one
//! directory the first starts the daemon and the second, once it holds the
//! lock, finds it. Two tools on two directories that name one instance both
//! start an lnsd; the second lnsd cannot bind the name and exits
//! (`SharedInstanceNameInUse`), and its tool, which then sees the channel
//! answer, reports `found` rather than a failure.
//!
//! **What a tool does not start.** A config that turns the shared instance
//! off (`share_instance = No`) would give a daemon no client can reach, so
//! that is [`InstanceError::SharingOff`]; and `--no-start` in every tool
//! ([`Request::no_start`]) keeps the old behaviour, a failure instead of a
//! daemon, for scripts that want one.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::interfaces::{set_shared_instance_transport, SharedInstanceTransport};

/// How long the find waits for a channel to answer. A loopback connect is
/// refused at once when nothing listens, so this only covers a daemon caught
/// between its start and its bind.
pub const FIND_BOUND: Duration = Duration::from_millis(150);

/// How long a started lnsd gets until its shared instance accepts. An lnsd on
/// a fresh directory binds in well under a second on the rig hosts; the rest
/// is margin for a loaded machine.
pub const START_BOUND: Duration = Duration::from_secs(20);

/// The file in the config directory that a started lnsd's stdout and stderr
/// are appended to.
pub const DAEMON_LOG: &str = "lnsd.log";

/// The file in the config directory whose `flock` serialises two tools that
/// start at once.
pub const START_LOCK: &str = ".lnsd-start.lock";

/// The environment variable that names the lnsd binary to start, over the one
/// beside the tool and the one on `PATH`.
pub const ENV_LNSD: &str = "LEVICULUM_LNSD";

/// Which channel answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// The abstract AF_UNIX socket `\0rns/<instance>`.
    Unix,
    /// TCP loopback.
    Tcp,
}

impl Via {
    /// The token the `INSTANCE` line writes.
    pub fn as_str(self) -> &'static str {
        match self {
            Via::Unix => "unix",
            Via::Tcp => "tcp",
        }
    }
}

impl fmt::Display for Via {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// When the `INSTANCE` line goes to stderr as well as to the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Report {
    /// Never: a full-screen program, whose stderr is its screen.
    Quiet,
    /// When a daemon was started, which the user should hear about.
    Started,
    /// Always: a tool run with `-v`.
    Always,
}

impl Report {
    /// [`Report::Always`] for a verbose run, else [`Report::Started`].
    pub fn from_verbose(verbose: bool) -> Self {
        if verbose {
            Report::Always
        } else {
            Report::Started
        }
    }
}

/// What a tool asks for.
#[derive(Debug, Clone)]
pub struct Request {
    /// The config directory: read for the transport, passed to a started
    /// lnsd as `-c`, and home of [`DAEMON_LOG`] and [`START_LOCK`].
    pub config_dir: PathBuf,
    /// The shared instance's name (`--instance`, else the config's, else
    /// `default`).
    pub instance_name: String,
    /// `--no-start`: find only, and fail when nothing answers.
    pub no_start: bool,
    /// The lnsd to start; `None` resolves it ([`lnsd_program`]).
    pub lnsd: Option<PathBuf>,
    /// When the line goes to stderr.
    pub report: Report,
    /// How long a started lnsd gets to answer: [`START_BOUND`], shorter only
    /// in a test that wants the [`InstanceError::Timeout`] without waiting
    /// for it.
    pub start_bound: Duration,
    /// Where the tool took [`instance_name`](Self::instance_name) from, a
    /// word the `INSTANCE` event carries as `source=` (loquax: `flag`,
    /// `home`, `default-dir`, `none`). `None` logs no such key.
    pub source: Option<&'static str>,
}

impl Request {
    /// A request for `instance_name` under `config_dir` that may start a
    /// daemon, reporting a start on stderr.
    pub fn new(config_dir: impl Into<PathBuf>, instance_name: impl Into<String>) -> Self {
        Self {
            config_dir: config_dir.into(),
            instance_name: instance_name.into(),
            no_start: false,
            lnsd: None,
            report: Report::Started,
            start_bound: START_BOUND,
            source: None,
        }
    }
}

/// What [`find_or_start`] did.
#[derive(Debug)]
pub enum Instance {
    /// A shared instance answered; nothing was started.
    Found {
        /// The channel that answered.
        via: Via,
    },
    /// No shared instance answered, so an lnsd was started as this process's
    /// child, and it now answers.
    Started {
        /// The channel it answers on.
        via: Via,
        /// Its pid.
        pid: u32,
        /// The child: dropping it stops the daemon.
        daemon: Daemon,
    },
}

impl Instance {
    /// The channel the tool attaches through.
    pub fn via(&self) -> Via {
        match self {
            Instance::Found { via } | Instance::Started { via, .. } => *via,
        }
    }

    /// Whether this call started the daemon.
    pub fn started(&self) -> bool {
        matches!(self, Instance::Started { .. })
    }

    /// The `INSTANCE` line, as logged.
    pub fn line(&self) -> String {
        match self {
            Instance::Found { via } => format!("INSTANCE found=yes via={via} pid=none"),
            Instance::Started { via, pid, .. } => {
                format!("INSTANCE started=yes via={via} pid={pid}")
            }
        }
    }

    fn log(&self, report: Report, source: Option<&str>) {
        match (self, source) {
            (Instance::Found { via }, None) => {
                tracing::info!(event = "INSTANCE", found = "yes", via = %via, pid = "none")
            }
            (Instance::Found { via }, Some(source)) => {
                tracing::info!(
                    event = "INSTANCE",
                    found = "yes",
                    via = %via,
                    pid = "none",
                    source = %source
                )
            }
            (Instance::Started { via, pid, .. }, None) => {
                tracing::info!(event = "INSTANCE", started = "yes", via = %via, pid = *pid)
            }
            (Instance::Started { via, pid, .. }, Some(source)) => {
                tracing::info!(
                    event = "INSTANCE",
                    started = "yes",
                    via = %via,
                    pid = *pid,
                    source = %source
                )
            }
        }
        let loud = match report {
            Report::Quiet => false,
            Report::Started => self.started(),
            Report::Always => true,
        };
        if loud {
            eprintln!("{}", self.line());
        }
    }
}

/// A started lnsd. Dropping it stops the daemon politely (`SIGTERM`, then
/// `SIGKILL` after [`STOP_GRACE`]); a tool that dies without dropping it
/// leaves the kernel's `PR_SET_PDEATHSIG` to do the same.
#[derive(Debug)]
pub struct Daemon {
    child: Option<Child>,
}

/// How long a dropped [`Daemon`] gets to exit after `SIGTERM`.
pub const STOP_GRACE: Duration = Duration::from_secs(3);

impl Daemon {
    /// The daemon's pid.
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        #[cfg(unix)]
        {
            if let Ok(pid) = libc::pid_t::try_from(child.id()) {
                // SAFETY: `kill` is a plain syscall on our own child's pid,
                // which cannot have been reused: the child is not reaped yet.
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
                let deadline = Instant::now() + STOP_GRACE;
                while Instant::now() < deadline {
                    if let Ok(Some(_)) = child.try_wait() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Why no instance is there to attach to.
#[derive(Debug)]
pub enum InstanceError {
    /// Nothing answered and `--no-start` forbade a start.
    NotFound {
        /// The channels that were asked, as an operator reads them.
        tried: String,
    },
    /// Nothing answered, and the config turns the shared instance off, so a
    /// started daemon would serve none.
    SharingOff {
        /// The config file.
        config: PathBuf,
    },
    /// lnsd could not be started.
    Spawn {
        /// The program that was run.
        program: PathBuf,
        /// What went wrong.
        error: io::Error,
    },
    /// The config directory, its lock or its log could not be opened.
    Io {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        error: io::Error,
    },
    /// The started lnsd exited before its shared instance answered, and no
    /// other daemon answers instead.
    Exited {
        /// Its exit status.
        status: String,
        /// Its log.
        log: PathBuf,
    },
    /// The started lnsd did not answer within [`Request::start_bound`].
    Timeout {
        /// The bound it had.
        bound: Duration,
        /// Its log.
        log: PathBuf,
    },
}

impl fmt::Display for InstanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InstanceError::NotFound { tried } => write!(
                f,
                "no shared instance answers ({tried}), and --no-start forbids starting lnsd"
            ),
            InstanceError::SharingOff { config } => write!(
                f,
                "no shared instance answers, and {} turns the shared instance off \
                 (share_instance = No), so a started lnsd would serve none",
                config.display()
            ),
            InstanceError::Spawn { program, error } => {
                write!(f, "could not start {}: {error}", program.display())
            }
            InstanceError::Io { path, error } => write!(f, "{}: {error}", path.display()),
            InstanceError::Exited { status, log } => write!(
                f,
                "the started lnsd exited ({status}) before its shared instance answered; \
                 its log is {}",
                log.display()
            ),
            InstanceError::Timeout { bound, log } => write!(
                f,
                "the started lnsd did not answer within {} s; its log is {}",
                bound.as_secs(),
                log.display()
            ),
        }
    }
}

impl std::error::Error for InstanceError {}

/// The lnsd a tool starts: [`ENV_LNSD`] when set, else the `lnsd` beside the
/// running executable (an installed package and a cargo build both put it
/// there), else `lnsd` from `PATH`.
pub fn lnsd_program() -> PathBuf {
    if let Some(path) = std::env::var_os(ENV_LNSD).filter(|v| !v.is_empty()) {
        return PathBuf::from(path);
    }
    let name = format!("lnsd{}", std::env::consts::EXE_SUFFIX);
    if let Some(sibling) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&name)))
        .filter(|p| p.is_file())
    {
        return sibling;
    }
    PathBuf::from(name)
}

/// The channels to ask, in order, for `instance_name` under `config`.
fn candidates(config: Option<&Config>, instance_name: &str) -> Vec<(Via, SharedInstanceTransport)> {
    let configured = config
        .map(|c| SharedInstanceTransport::from_config(&c.reticulum))
        .unwrap_or_default();
    let tcp = SharedInstanceTransport {
        tcp: true,
        ..configured
    };
    if configured.tcp || cfg!(not(unix)) {
        return vec![(Via::Tcp, tcp)];
    }
    let mut out = vec![(Via::Unix, configured)];
    if configured.data_port.is_some() || instance_name == "default" {
        out.push((Via::Tcp, tcp));
    }
    out
}

fn describe(candidates: &[(Via, SharedInstanceTransport)], instance_name: &str) -> String {
    candidates
        .iter()
        .map(|(via, t)| match via {
            Via::Unix => format!("unix @rns/{instance_name}"),
            Via::Tcp => format!(
                "tcp 127.0.0.1:{}",
                t.data_port
                    .unwrap_or(crate::interfaces::ipc::DEFAULT_DATA_PORT)
            ),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Ask each candidate once.
fn probe(
    candidates: &[(Via, SharedInstanceTransport)],
    instance_name: &str,
) -> Option<(Via, SharedInstanceTransport)> {
    candidates
        .iter()
        .find(|(_, t)| t.answers(instance_name))
        .copied()
}

/// Whether a shared instance named `instance_name` answers where `config`
/// puts it (the find of [`find_or_start`] without the start), and on which
/// channel. Waits at most [`FIND_BOUND`].
pub fn find(config: Option<&Config>, instance_name: &str) -> Option<Via> {
    find_transport(&candidates(config, instance_name), instance_name).map(|(via, _)| via)
}

fn find_transport(
    candidates: &[(Via, SharedInstanceTransport)],
    instance_name: &str,
) -> Option<(Via, SharedInstanceTransport)> {
    let deadline = Instant::now() + FIND_BOUND;
    loop {
        if let Some(hit) = probe(candidates, instance_name) {
            return Some(hit);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Load `<config_dir>/config` when there is one.
fn load(config_dir: &Path) -> Option<Config> {
    let file = config_dir.join("config");
    file.is_file().then(|| Config::load(&file).ok()).flatten()
}

/// Find the shared instance `request` names, or start lnsd so there is one;
/// see the module documentation. On success this process's shared-instance
/// dial points at the channel that answered
/// ([`set_shared_instance_transport`]), so a node built afterwards with
/// `connect_to_shared_instance` and the RPC queries reach it. Blocking: call
/// it from a tool's `main` or through `spawn_blocking`.
pub fn find_or_start(request: &Request) -> Result<Instance, InstanceError> {
    let config = load(&request.config_dir);
    let wanted = candidates(config.as_ref(), &request.instance_name);
    if let Some(found) = found(&wanted, request) {
        return Ok(found);
    }
    if request.no_start {
        return Err(InstanceError::NotFound {
            tried: describe(&wanted, &request.instance_name),
        });
    }
    if let Some(c) = &config {
        if !c.reticulum.shared_instance {
            return Err(InstanceError::SharingOff {
                config: request.config_dir.join("config"),
            });
        }
    }

    std::fs::create_dir_all(&request.config_dir).map_err(|error| InstanceError::Io {
        path: request.config_dir.clone(),
        error,
    })?;
    let _lock = StartLock::take(&request.config_dir.join(START_LOCK))?;
    // Under the lock: a tool that held it before us may have started one.
    if let Some(found) = found(&wanted, request) {
        return Ok(found);
    }

    let program = request.lnsd.clone().unwrap_or_else(lnsd_program);
    let log_path = request.config_dir.join(DAEMON_LOG);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| InstanceError::Io {
            path: log_path.clone(),
            error,
        })?;
    let log_err = log.try_clone().map_err(|error| InstanceError::Io {
        path: log_path.clone(),
        error,
    })?;
    let mut cmd = Command::new(&program);
    cmd.arg("-c").arg(&request.config_dir);
    // The daemon serves the name its config gives, `default` without one.
    // A tool that asks for another name would wait the whole bound for a
    // daemon answering elsewhere, so the started one is told the name.
    let configured_name = config
        .as_ref()
        .map_or("default", |c| c.reticulum.instance_name.as_str());
    if request.instance_name != configured_name {
        cmd.arg("--instance-name").arg(&request.instance_name);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    let child = crate::process::spawn_supervised(cmd).map_err(|error| InstanceError::Spawn {
        program: program.clone(),
        error,
    })?;
    let mut daemon = Daemon { child: Some(child) };
    let pid = daemon.pid().unwrap_or(0);

    let deadline = Instant::now() + request.start_bound;
    loop {
        // What the started lnsd serves is what the config it wrote or read
        // says, so the candidates are read again.
        let now_wanted = candidates(load(&request.config_dir).as_ref(), &request.instance_name);
        if let Some((via, transport)) = probe(&now_wanted, &request.instance_name) {
            let exited = daemon
                .child
                .as_mut()
                .map(|c| matches!(c.try_wait(), Ok(Some(_))))
                .unwrap_or(true);
            set_shared_instance_transport(transport);
            // Ours exited and someone answers: another tool's lnsd won the
            // name (the module documentation, "Two tools at once").
            let instance = if exited {
                Instance::Found { via }
            } else {
                Instance::Started { via, pid, daemon }
            };
            instance.log(request.report, request.source);
            return Ok(instance);
        }
        if let Some(child) = daemon.child.as_mut() {
            if let Ok(Some(status)) = child.try_wait() {
                // One more look: the winner may bind a moment after ours left.
                if let Some(found) = found(&now_wanted, request) {
                    return Ok(found);
                }
                return Err(InstanceError::Exited {
                    status: status.to_string(),
                    log: log_path,
                });
            }
        }
        if Instant::now() >= deadline {
            return Err(InstanceError::Timeout {
                bound: request.start_bound,
                log: log_path,
            });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// [`find_or_start`] as a client tool calls it, with the tool's flags.
///
/// `Ok(Some)` is an instance to attach to, to be held for the tool's life (a
/// started daemon goes when it is dropped). `Ok(None)` is nothing found where
/// nothing may be started, `--no-start` or a config without a shared
/// instance: the tool then connects as it did before step 4 and fails with
/// its own words, which scripts and the drop-in tests read. `Err` is a start
/// that failed.
pub fn for_tool(
    config_dir: &Path,
    instance_name: &str,
    no_start: bool,
    report: Report,
) -> Result<Option<Instance>, InstanceError> {
    let mut request = Request::new(config_dir, instance_name);
    request.no_start = no_start;
    request.report = report;
    for_request(&request)
}

/// [`for_tool`] for a tool that fills the [`Request`] itself (loquax names
/// the `source`): the same `Ok(None)` for nothing found where nothing may be
/// started.
pub fn for_request(request: &Request) -> Result<Option<Instance>, InstanceError> {
    match find_or_start(request) {
        Ok(instance) => Ok(Some(instance)),
        Err(InstanceError::NotFound { .. } | InstanceError::SharingOff { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The find step: on a hit, point the dial there and report it.
fn found(wanted: &[(Via, SharedInstanceTransport)], request: &Request) -> Option<Instance> {
    let (via, transport) = find_transport(wanted, &request.instance_name)?;
    set_shared_instance_transport(transport);
    let instance = Instance::Found { via };
    instance.log(request.report, request.source);
    Some(instance)
}

/// The exclusive `flock` on [`START_LOCK`], released on drop (the kernel
/// releases it on close, and on death).
struct StartLock {
    _file: File,
}

impl StartLock {
    fn take(path: &Path) -> Result<Self, InstanceError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(|error| InstanceError::Io {
                path: path.to_path_buf(),
                error,
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            loop {
                // SAFETY: `flock` on a descriptor this function owns.
                let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
                if rc == 0 {
                    break;
                }
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(InstanceError::Io {
                        path: path.to_path_buf(),
                        error,
                    });
                }
            }
        }
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Config {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("config");
        std::fs::write(&file, text).expect("write");
        Config::load(&file).expect("load")
    }

    // Unix paths and sockets (CIRIS fork: the Windows lane).

    #[cfg(unix)]
    #[test]
    fn a_default_instance_is_asked_on_unix_then_on_37428() {
        let got = candidates(None, "default");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, Via::Unix);
        assert_eq!(got[1].0, Via::Tcp);
        assert_eq!(
            describe(&got, "default"),
            "unix @rns/default, tcp 127.0.0.1:37428"
        );
    }

    // Unix paths and sockets (CIRIS fork: the Windows lane).

    #[cfg(unix)]
    #[test]
    fn a_named_instance_without_a_port_is_asked_on_unix_only() {
        let got = candidates(None, "alpha");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, Via::Unix);
    }

    // Unix paths and sockets (CIRIS fork: the Windows lane).

    #[cfg(unix)]
    #[test]
    fn a_configured_port_is_the_tcp_fallback() {
        let c = config("[reticulum]\n  share_instance = Yes\n  shared_instance_port = 37500\n");
        let got = candidates(Some(&c), "alpha");
        assert_eq!(
            describe(&got, "alpha"),
            "unix @rns/alpha, tcp 127.0.0.1:37500"
        );
    }

    #[test]
    fn a_tcp_config_is_asked_on_tcp_only() {
        let c = config("[reticulum]\n  share_instance = Yes\n  shared_instance_type = tcp\n");
        let got = candidates(Some(&c), "default");
        assert_eq!(describe(&got, "default"), "tcp 127.0.0.1:37428");
    }

    #[test]
    fn the_lines_read_as_the_cells_grep_them() {
        assert_eq!(
            Instance::Found { via: Via::Unix }.line(),
            "INSTANCE found=yes via=unix pid=none"
        );
        let started = Instance::Started {
            via: Via::Tcp,
            pid: 42,
            daemon: Daemon { child: None },
        };
        assert_eq!(started.line(), "INSTANCE started=yes via=tcp pid=42");
    }

    // Unix paths and sockets (CIRIS fork: the Windows lane).

    #[cfg(unix)]
    #[test]
    fn no_start_fails_instead_of_starting() {
        let dir = tempfile::tempdir().expect("temp dir");
        let name = format!("nostart{}", std::process::id());
        let mut request = Request::new(dir.path(), &name);
        request.no_start = true;
        request.lnsd = Some(PathBuf::from("/nonexistent/lnsd-must-not-run"));
        match find_or_start(&request) {
            Err(InstanceError::NotFound { tried }) => {
                assert_eq!(tried, format!("unix @rns/{name}"))
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
        assert!(!dir.path().join(DAEMON_LOG).exists(), "nothing was started");
    }

    #[test]
    fn a_config_without_a_shared_instance_starts_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("config"),
            "[reticulum]\n  share_instance = No\n[interfaces]\n",
        )
        .expect("write");
        let mut request = Request::new(dir.path(), format!("sharingoff{}", std::process::id()));
        request.lnsd = Some(PathBuf::from("/nonexistent/lnsd-must-not-run"));
        assert!(matches!(
            find_or_start(&request),
            Err(InstanceError::SharingOff { .. })
        ));
    }

    #[test]
    fn a_missing_lnsd_is_a_spawn_error_not_a_hang() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut request = Request::new(dir.path(), format!("nolnsd{}", std::process::id()));
        request.lnsd = Some(PathBuf::from("/nonexistent/lnsd-probe"));
        request.report = Report::Quiet;
        assert!(matches!(
            find_or_start(&request),
            Err(InstanceError::Spawn { .. })
        ));
    }

    /// A stub lnsd at `dir/lnsd` that writes its arguments to `dir/lnsd.args`,
    /// one a line, and then sleeps without ever binding a socket. Written by
    /// a child shell, so no descriptor open for writing on it lives in this
    /// multi-threaded test process, where a sibling test's fork could carry
    /// it into its child and make the exec fail with ETXTBSY.
    #[cfg(unix)]
    fn silent_stub(dir: &Path) -> PathBuf {
        const SCRIPT: &str = "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.args\"\nexec sleep 30\n";
        let stub = dir.join("lnsd");
        let status = Command::new("/bin/sh")
            .arg("-c")
            .arg(r#"printf '%s' "$2" > "$1" && chmod +x "$1""#)
            .arg("stub")
            .arg(&stub)
            .arg(SCRIPT)
            .status()
            .expect("run sh");
        assert!(status.success());
        stub
    }

    /// The real failure the 20 s sentence is for: an lnsd that starts and
    /// never answers. The bound is the test's, one second; the sentence
    /// names it and the log, which lies in the config directory. The stub
    /// was told the instance asked for, which its config does not name.
    #[cfg(unix)]
    #[test]
    fn a_started_lnsd_that_never_answers_is_a_timeout_naming_its_log() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bin = tempfile::tempdir().expect("stub dir");
        let stub = silent_stub(bin.path());
        let name = format!("silent{}", std::process::id());
        let mut request = Request::new(dir.path(), name.clone());
        request.lnsd = Some(stub.clone());
        request.report = Report::Quiet;
        request.start_bound = Duration::from_secs(1);
        let started = Instant::now();
        let error = find_or_start(&request).expect_err("nothing answers");
        assert!(
            started.elapsed() < Duration::from_secs(1) + STOP_GRACE + Duration::from_secs(2),
            "the bound is the request's, not START_BOUND: {:?}",
            started.elapsed()
        );
        let log = dir.path().join(DAEMON_LOG);
        assert!(
            matches!(&error, InstanceError::Timeout { bound, log: l }
                if *bound == Duration::from_secs(1) && *l == log),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            format!(
                "the started lnsd did not answer within 1 s; its log is {}",
                log.display()
            )
        );
        let args = std::fs::read_to_string(bin.path().join("lnsd.args")).expect("stub args");
        assert_eq!(
            args,
            format!("-c\n{}\n--instance-name\n{name}\n", dir.path().display())
        );
    }

    // CIRIS fork: with no Unix socket, a default instance is asked on 37428 only.
    #[cfg(not(unix))]
    #[test]
    fn a_default_instance_is_asked_on_37428_only_off_unix() {
        let got = candidates(None, "default");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, Via::Tcp);
        assert_eq!(describe(&got, "default"), "tcp 127.0.0.1:37428");
    }
}
