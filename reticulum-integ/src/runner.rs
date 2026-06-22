use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::compose::generate_compose;
use crate::lxmf::{helper_log_path, LxmfHelper};
use crate::topology::{apply_radio_overrides, generate_node_configs, TestScenario};

/// Monotonic counter for generating unique run IDs within a process.
static RUN_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Mint a run ID that is unique across separate test processes.
///
/// The in-process counter alone resets to 0 in every `cargo test` invocation,
/// so two sequential runs of the same scenario would mint identical docker
/// project, container, and network names (`integ-<test>-0-...`). A run killed
/// before `down()` (panic, SIGKILL, timeout) then leaves a same-named project
/// that the next process silently reuses or attaches to, leaking docker state
/// across runs. Seeding the counter with a per-process base keyed on the PID
/// makes every run's docker identity unique, so a previous run's leftovers can
/// never be mistaken for the current run's state. The low 16 bits feed the
/// per-run IPv6 subnet (see `compose.rs`), so the PID is spread across the
/// whole word to keep those bits distinct between processes too.
fn next_run_id() -> u32 {
    static BASE: OnceLock<u32> = OnceLock::new();
    let base = *BASE.get_or_init(|| std::process::id().wrapping_mul(0x9E37_79B9));
    base.wrapping_add(RUN_COUNTER.fetch_add(1, Ordering::Relaxed))
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum RunnerError {
    Compose { action: String, stderr: String },
    ReadinessTimeout { node: String, timeout_secs: u64 },
    Io(io::Error),
    BinaryNotFound(PathBuf),
    StaleBinary(String),
    ConfigGeneration(String),
    ProxyError(String),
    InsufficientRNodes(String),
}

impl fmt::Display for RunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunnerError::Compose { action, stderr } => {
                write!(f, "docker compose {action} failed: {stderr}")
            }
            RunnerError::ReadinessTimeout { node, timeout_secs } => {
                write!(f, "node '{node}' not ready after {timeout_secs}s")
            }
            RunnerError::Io(e) => write!(f, "I/O error: {e}"),
            RunnerError::BinaryNotFound(path) => {
                write!(
                    f,
                    "lnsd binary not found at {}: run `cargo build --release --bin lnsd`",
                    path.display()
                )
            }
            RunnerError::StaleBinary(msg) => {
                write!(f, "stale binary: {msg}")
            }
            RunnerError::ConfigGeneration(msg) => {
                write!(f, "config generation failed: {msg}")
            }
            RunnerError::ProxyError(msg) => {
                write!(f, "proxy error: {msg}")
            }
            RunnerError::InsufficientRNodes(msg) => {
                write!(f, "{msg}")
            }
        }
    }
}

impl std::error::Error for RunnerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RunnerError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for RunnerError {
    fn from(e: io::Error) -> Self {
        RunnerError::Io(e)
    }
}

// ---------------------------------------------------------------------------
// Free helpers (testable without Docker)
// ---------------------------------------------------------------------------

/// Format a container name: `integ-{test_name}-{run_id}-{node_name}`.
///
/// The `run_id` ensures parallel test runs using the same TOML scenario
/// do not collide on container names.
pub fn format_container_name(test_name: &str, run_id: u32, node_name: &str) -> String {
    format!("integ-{test_name}-{run_id}-{node_name}")
}

/// Format a compose project name: `integ-{test_name}-{run_id}`.
pub fn format_project_name(test_name: &str, run_id: u32) -> String {
    format!("integ-{test_name}-{run_id}")
}

// ---------------------------------------------------------------------------
// TestRunner
// ---------------------------------------------------------------------------

pub struct TestRunner {
    scenario: TestScenario,
    run_id: u32,
    _tempdir: TempDir,
    compose_file: PathBuf,
    project_name: String,
    is_up: bool,
    /// lora-proxy child processes (killed on drop).
    proxy_processes: Vec<Child>,
    /// Node name -> control socket path for proxy commands.
    proxy_sockets: BTreeMap<String, PathBuf>,
    /// Background `dmesg --follow` process for capturing USB/kernel events.
    /// Started in `up()`, killed in `down()`.
    dmesg_process: Option<Child>,
    /// Long-running LXMF helper processes spawned via `lxmf_start` steps,
    /// keyed by node name. Shutdown happens in `down()` before compose down.
    lxmf_helpers: Mutex<BTreeMap<String, Arc<LxmfHelper>>>,
    /// Background debug serial capture threads with their shutdown signal.
    /// Started in `up()`, joined in `stop_debug_captures()` so each scenario
    /// releases its serial ports before the next test in the same process.
    debug_captures: Option<DebugCaptures>,
}

/// Lifecycle handle for the background debug serial capture threads.
///
/// Each reader thread owns its `serialport` handle for life and exits only
/// when `stop` is set, dropping the handle and releasing the port. Without
/// this, a leaked thread keeps the port open across scenarios in the same
/// `--test-threads=1` process, making the next LNode test skip
/// `device_inaccessible` (it cannot reclaim a port held by its own pid).
struct DebugCaptures {
    stop: Arc<std::sync::atomic::AtomicBool>,
    handles: Vec<thread::JoinHandle<()>>,
}

impl DebugCaptures {
    /// Signal every reader thread to stop and join it, releasing all ports.
    fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        for handle in self.handles {
            let _ = handle.join();
        }
    }
}

/// Open `port`, assert DTR/RTS (CDC-ACM only sends when DTR is asserted), and
/// spawn a reader thread that appends serial output to `log_path` until `stop`
/// is set. On a read error the loop reconnects, but never after shutdown. The
/// thread owns the serial handle for life and drops it on exit, releasing the
/// port. Returns the join handle, or `None` if the log file or port could not
/// be opened.
fn spawn_debug_capture(
    port: String,
    log_path: PathBuf,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> Option<thread::JoinHandle<()>> {
    let mut file = match fs::File::create(&log_path) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("[debug-capture] cannot create {}: {e}", log_path.display());
            return None;
        }
    };
    // Short read timeout so the thread polls the shutdown flag promptly.
    let mut serial = match serialport::new(&port, 115_200)
        .timeout(Duration::from_millis(250))
        .open()
    {
        Ok(serial) => serial,
        Err(e) => {
            eprintln!("[debug-capture] FAILED to open {port}: {e}");
            return None;
        }
    };
    let _ = serial.write_data_terminal_ready(true);
    let _ = serial.write_request_to_send(true);
    eprintln!("[debug-capture] {port} → {}", log_path.display());

    let handle = thread::spawn(move || {
        use std::io::{Read, Write};
        let mut buf = [0u8; 1024];
        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            match serial.read(&mut buf) {
                Ok(n) if n > 0 => {
                    let _ = file.write_all(&buf[..n]);
                    let _ = file.flush();
                }
                Ok(_) => {}
                Err(ref e) if e.kind() == io::ErrorKind::TimedOut => continue,
                Err(_) => {
                    // Port lost. Never reopen after shutdown; check the flag
                    // before and after the backoff sleep so stop wins the race.
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(500));
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    match serialport::new(&port, 115_200)
                        .timeout(Duration::from_millis(250))
                        .open()
                    {
                        Ok(mut new_serial) => {
                            let _ = new_serial.write_data_terminal_ready(true);
                            serial = new_serial;
                        }
                        Err(_) => break,
                    }
                }
            }
        }
        // `serial` dropped here, releasing the port.
    });
    Some(handle)
}

impl TestRunner {
    /// Create a new test runner for the given scenario.
    ///
    /// Resolves repo root from `CARGO_MANIFEST_DIR`, checks that lnsd exists,
    /// creates a tempdir, generates node configs and the compose file.
    /// If any node has `rnode_proxy = true`, spawns lora-proxy processes and
    /// waits for their PTYs to appear.
    pub fn new(mut scenario: TestScenario) -> Result<Self, RunnerError> {
        // First thing: acquire the process-wide integ lock so a colliding
        // `cargo test` aborts before any Docker/USB work. Subsequent
        // TestRunners in the same process are no-ops, the `OnceLock`
        // holds the fd for the process lifetime.
        crate::lock::acquire_integ_lock();

        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest_dir
            .parent()
            .expect("CARGO_MANIFEST_DIR has no parent")
            .to_path_buf();
        let target_dir = crate::paths::target_dir(&repo_root);

        let lnsd_path = crate::paths::release_bin(&target_dir, "lnsd");
        if !lnsd_path.exists() {
            return Err(RunnerError::BinaryNotFound(lnsd_path));
        }

        let lns_path = crate::paths::release_bin(&target_dir, "lns");
        if !lns_path.exists() {
            return Err(RunnerError::BinaryNotFound(lns_path));
        }

        let lncp_path = crate::paths::release_bin(&target_dir, "lncp");
        if !lncp_path.exists() {
            return Err(RunnerError::BinaryNotFound(lncp_path));
        }

        let has_proxy = scenario.nodes.values().any(|n| n.rnode_proxy);
        let proxy_path = crate::paths::release_bin(&target_dir, "lora-proxy");
        if has_proxy && !proxy_path.exists() {
            return Err(RunnerError::BinaryNotFound(proxy_path.clone()));
        }

        // The C daemon is required only when a c-api node is in the scenario.
        // It is a static binary built by `just build-c-lnsd`.
        let has_c_api = scenario.nodes.values().any(|n| n.node_type == "c-api");
        let c_lnsd_path = crate::paths::release_bin(&target_dir, "c-lnsd");
        if has_c_api && !c_lnsd_path.exists() {
            return Err(RunnerError::BinaryNotFound(c_lnsd_path.clone()));
        }

        // Freshness check: fail loud if any mounted binary predates HEAD.
        // Opt-out via LEVICULUM_SKIP_FRESHNESS_CHECK=1 for local iteration.
        let mut freshness_targets: Vec<&std::path::Path> = vec![&lnsd_path, &lns_path, &lncp_path];
        if has_proxy {
            freshness_targets.push(&proxy_path);
        }
        if has_c_api {
            freshness_targets.push(&c_lnsd_path);
        }
        crate::paths::check_binary_freshness(&freshness_targets, &repo_root)
            .map_err(|e| RunnerError::StaleBinary(e.to_string()))?;

        // Apply env-var overrides (LORA_BANDWIDTH, LORA_SF, etc.) before
        // generating configs, so the same TOML can be run with different
        // radio settings.
        apply_radio_overrides(&mut scenario);

        // Resolve RNode device paths (env var overrides) and probe each
        // with CMD_DETECT. Skips test if devices are missing.
        resolve_and_probe_rnodes(&mut scenario)?;

        let tempdir = TempDir::new()?;
        let base_dir = tempdir.path().join("nodes");
        let run_id = next_run_id();

        // Spawn proxy processes before generating configs/compose.
        let (proxy_processes, proxy_sockets, proxy_devices) =
            spawn_proxies(&scenario, run_id, &target_dir)?;

        generate_node_configs(&scenario, &base_dir)
            .map_err(|e| RunnerError::ConfigGeneration(e.to_string()))?;

        let yaml = generate_compose(
            &scenario,
            run_id,
            &base_dir,
            &repo_root,
            &target_dir,
            &proxy_devices,
        );
        let compose_file = tempdir.path().join("docker-compose.yml");
        fs::write(&compose_file, &yaml)?;

        let project_name = format_project_name(&scenario.test.name, run_id);

        Ok(TestRunner {
            scenario,
            run_id,
            _tempdir: tempdir,
            compose_file,
            project_name,
            is_up: false,
            proxy_processes,
            proxy_sockets,
            dmesg_process: None,
            lxmf_helpers: Mutex::new(BTreeMap::new()),
            debug_captures: None,
        })
    }

    /// Return the container name for a node.
    pub fn container_name(&self, node: &str) -> String {
        format_container_name(&self.scenario.test.name, self.run_id, node)
    }

    /// Return a reference to the scenario.
    pub fn scenario(&self) -> &TestScenario {
        &self.scenario
    }

    /// Build a base `docker compose` command with project file and name.
    fn compose_cmd(&self) -> Command {
        let mut cmd = Command::new("docker");
        cmd.args([
            "compose",
            "-f",
            self.compose_file.to_str().expect("compose path not UTF-8"),
            "-p",
            &self.project_name,
        ]);
        cmd
    }

    /// Bring up containers in detached mode, building images first.
    pub fn up(&mut self) -> Result<(), RunnerError> {
        // Start debug serial captures BEFORE containers start.
        // The T114 debug port blocks on write_packet if no reader is
        // present, so we must have a reader ready before lnsd connects
        // to the transport port and triggers T114 activity.
        self.start_debug_captures();

        let output = self.compose_cmd().args(["up", "-d", "--build"]).output()?;

        if !output.status.success() {
            return Err(RunnerError::Compose {
                action: "up -d --build".into(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }

        self.is_up = true;
        self.start_dmesg_logger();
        Ok(())
    }

    /// Poll each node until `rnstatus` exits successfully, or timeout.
    ///
    /// Polls every 500ms. On timeout, collects logs and returns
    /// `ReadinessTimeout`.
    pub fn wait_ready(&self, timeout_secs: u64) -> Result<(), RunnerError> {
        for name in self.scenario.nodes.keys() {
            self.wait_ready_single(name, timeout_secs)?;
        }
        Ok(())
    }

    /// Poll a single node until it is ready, or timeout.
    ///
    /// Probes the abstract Unix socket `\0rns/default` that both lnsd
    /// (Rust) and rnsd (Python) listen on. Using a raw socket connect
    /// instead of `rnstatus` avoids a race condition where rnstatus
    /// accidentally becomes the shared-instance server before rnsd starts.
    ///
    /// Polls every 500ms. On timeout, collects logs and returns
    /// `ReadinessTimeout`.
    pub fn wait_ready_single(&self, node: &str, timeout_secs: u64) -> Result<(), RunnerError> {
        let container = self.container_name(node);
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);

        loop {
            let output = Command::new("docker")
                .args([
                    "exec",
                    &container,
                    "python3",
                    "-c",
                    "import socket; s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); s.connect(b'\\x00rns/default'); s.close()",
                ])
                .output()?;

            if output.status.success() {
                return Ok(());
            }

            if Instant::now() >= deadline {
                let _ = self.collect_logs();
                return Err(RunnerError::ReadinessTimeout {
                    node: node.to_string(),
                    timeout_secs,
                });
            }

            thread::sleep(Duration::from_millis(500));
        }
    }

    /// Return the path to the shared logs directory.
    pub(crate) fn logs_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("logs")
    }

    /// UTC timestamp formatted for filenames: `2026-03-20T03-12-00`.
    pub(crate) fn utc_timestamp() -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = now.as_secs();
        let days = secs / 86400;
        let day_secs = secs % 86400;
        let h = day_secs / 3600;
        let m = (day_secs % 3600) / 60;
        let s = day_secs % 60;
        let mut y = 1970i64;
        let mut rem = days as i64;
        loop {
            let ydays = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
                366
            } else {
                365
            };
            if rem < ydays {
                break;
            }
            rem -= ydays;
            y += 1;
        }
        let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
        let mdays = [
            31,
            if leap { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ];
        let mut mo = 0usize;
        for md in &mdays {
            if rem < *md as i64 {
                break;
            }
            rem -= *md as i64;
            mo += 1;
        }
        format!(
            "{:04}-{:02}-{:02}T{:02}-{:02}-{:02}",
            y,
            mo + 1,
            rem + 1,
            h,
            m,
            s
        )
    }

    /// Collect container logs to a timestamped file under `reticulum-integ/logs/`.
    ///
    /// Filename: `{test_name}_{timestamp}.log`, never overwrites.
    pub fn collect_logs(&self) -> Result<PathBuf, RunnerError> {
        let logs_dir = Self::logs_dir();
        fs::create_dir_all(&logs_dir)?;

        let output = self
            .compose_cmd()
            .args(["logs", "--no-color", "--timestamps"])
            .output()?;

        let ts = Self::utc_timestamp();
        let log_file = logs_dir.join(format!("{}_{}.log", self.scenario.test.name, ts));
        fs::write(&log_file, &output.stdout)?;
        Ok(log_file)
    }

    /// Return the last `n` lines of a single container's log.
    pub fn container_logs_tail(&self, node: &str, n: usize) -> Result<String, RunnerError> {
        let container = self.container_name(node);
        let output = Command::new("docker")
            .args(["logs", "--tail", &n.to_string(), &container])
            .output()?;
        // docker logs writes to both stdout and stderr; combine them.
        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.is_empty() {
            combined.push_str(&stderr);
        }
        Ok(combined)
    }

    /// Return the proxy control socket path for a node, if it has one.
    pub fn proxy_socket(&self, node: &str) -> Option<&PathBuf> {
        self.proxy_sockets.get(node)
    }

    /// Return ordered node names from the scenario.
    pub fn node_names(&self) -> Vec<&str> {
        self.scenario.nodes.keys().map(|s| s.as_str()).collect()
    }

    /// Bring down containers with a 10-second timeout. No-op if not up.
    /// Always saves container logs before teardown.
    pub fn down(&mut self) -> Result<(), RunnerError> {
        // Shut down LXMF helpers first: their `docker exec` subprocesses
        // would hang on container teardown, blocking the runner.
        self.lxmf_kill_all();

        // Release serial ports before anything else: the capture threads are
        // started at the top of `up()` (even before containers), so they must
        // be joined on every teardown path, including the early return below.
        self.stop_debug_captures();

        if !self.is_up {
            self.kill_proxies();
            return Ok(());
        }

        // Save logs BEFORE tearing down containers
        match self.collect_logs() {
            Ok(path) => eprintln!("Logs saved to: {}", path.display()),
            Err(e) => eprintln!("Failed to save logs: {e}"),
        }

        // `--volumes --remove-orphans` makes teardown hermetic across runs:
        // orphan containers (e.g. from a renamed service) and any anonymous
        // volumes are removed too, so no docker state from this scenario can
        // leak into a later one. Without `--remove-orphans` compose also
        // leaves the project network behind in some cases (observed lingering
        // `integ-..._default` networks).
        let output = self
            .compose_cmd()
            .args(["down", "--volumes", "--remove-orphans", "--timeout", "10"])
            .output()?;

        self.is_up = false;
        self.kill_proxies();
        self.stop_dmesg_logger();

        // Belt-and-suspenders: if the project network still lingers (compose
        // occasionally leaves it when a container is slow to stop), drop it so
        // it cannot accumulate or be reused by a later run.
        let _ = Command::new("docker")
            .args(["network", "rm", &format!("{}_default", self.project_name)])
            .output();

        if !output.status.success() {
            check_stale_resources(&self.scenario);
            return Err(RunnerError::Compose {
                action: "down --volumes --remove-orphans --timeout 10".into(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }

        check_stale_resources(&self.scenario);
        Ok(())
    }

    /// Kill all proxy child processes and collect stderr output.
    fn kill_proxies(&mut self) {
        for mut child in self.proxy_processes.drain(..) {
            // Send SIGTERM so the proxy can flush its buffers.
            let _ = Command::new("kill")
                .args(["-TERM", &child.id().to_string()])
                .status();
            // Wait up to 2s for graceful shutdown, then force kill.
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    _ if Instant::now() >= deadline => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                    _ => thread::sleep(Duration::from_millis(50)),
                }
            }
            // Collect stderr for debugging.
            if let Some(mut stderr) = child.stderr.take() {
                let mut buf = String::new();
                use std::io::Read;
                let _ = stderr.read_to_string(&mut buf);
                if !buf.is_empty() {
                    let proxy_lines: Vec<&str> = buf
                        .lines()
                        .filter(|l| l.contains("AToB") || l.contains("BToA"))
                        .collect();
                    println!("--- proxy: {} frame lines ---", proxy_lines.len());
                    for line in &proxy_lines {
                        println!("  {line}");
                    }
                    println!("--- end proxy ---");
                }
            }
        }
        for sock_path in self.proxy_sockets.values() {
            let _ = fs::remove_file(sock_path);
        }
        self.proxy_sockets.clear();
    }

    /// Start a background `dmesg --follow` process to capture USB/kernel events.
    ///
    /// Output goes to `reticulum-integ/logs/{test_name}_dmesg_{timestamp}.log`.
    /// Non-fatal if dmesg is unavailable (e.g., no permissions).
    fn start_dmesg_logger(&mut self) {
        let logs_dir = Self::logs_dir();
        let _ = fs::create_dir_all(&logs_dir);
        let ts = Self::utc_timestamp();
        let log_path = logs_dir.join(format!("{}_{}_dmesg.log", self.scenario.test.name, ts));

        match fs::File::create(&log_path) {
            Ok(file) => {
                match Command::new("dmesg")
                    .args(["--follow", "--time-format", "iso"])
                    .stdout(file)
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    Ok(child) => {
                        self.dmesg_process = Some(child);
                    }
                    Err(e) => {
                        eprintln!("dmesg --follow unavailable: {e}");
                    }
                }
            }
            Err(e) => {
                eprintln!("Cannot create dmesg log {}: {e}", log_path.display());
            }
        }
    }

    /// Start background serial debug capture for nodes with a debug serial port.
    /// Uses serialport crate for reliable reading (not `cat` which exits on device reset).
    fn start_debug_captures(&mut self) {
        let logs_dir = Self::logs_dir();
        let _ = fs::create_dir_all(&logs_dir);
        let ts = Self::utc_timestamp();

        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut handles = Vec::new();
        for (name, node) in &self.scenario.nodes {
            if let Some(ref port) = node.debug_serial_path {
                let log_path = logs_dir.join(format!(
                    "{}_{}_debug_{}.log",
                    self.scenario.test.name, name, ts
                ));
                if let Some(handle) = spawn_debug_capture(port.clone(), log_path, Arc::clone(&stop))
                {
                    handles.push(handle);
                }
            }
        }
        self.debug_captures = Some(DebugCaptures { stop, handles });
    }

    /// Stop all background debug serial captures and join their threads,
    /// releasing every serial port before the next scenario runs in the same
    /// process. Idempotent: a no-op if captures were never started or already
    /// stopped.
    fn stop_debug_captures(&mut self) {
        if let Some(captures) = self.debug_captures.take() {
            captures.stop();
        }
    }

    /// Stop the background dmesg logger (if running).
    fn stop_dmesg_logger(&mut self) {
        if let Some(mut child) = self.dmesg_process.take() {
            let _ = Command::new("kill")
                .args(["-TERM", &child.id().to_string()])
                .status();
            let _ = child.wait();
        }
    }

    /// Save exec step stdout/stderr to a timestamped file under `reticulum-integ/logs/`.
    ///
    /// Filename: `{test_name}_{label}_{timestamp}.log`, never overwrites.
    pub fn save_exec_output(
        &self,
        step_label: &str,
        stdout: &[u8],
        stderr: &[u8],
    ) -> Result<PathBuf, RunnerError> {
        let logs_dir = Self::logs_dir();
        fs::create_dir_all(&logs_dir)?;
        let ts = Self::utc_timestamp();
        let log_file = logs_dir.join(format!(
            "{}_{}_{}.log",
            self.scenario.test.name, step_label, ts
        ));
        let mut content = Vec::new();
        content.extend_from_slice(b"=== STDOUT ===\n");
        content.extend_from_slice(stdout);
        content.extend_from_slice(b"\n=== STDERR ===\n");
        content.extend_from_slice(stderr);
        fs::write(&log_file, &content)?;
        Ok(log_file)
    }

    /// Execute a command inside a node's container.
    ///
    /// Returns raw `Output`, the caller interprets success/failure.
    pub fn docker_exec(&self, node: &str, args: &[&str]) -> Result<Output, RunnerError> {
        let container = self.container_name(node);
        let mut cmd = Command::new("docker");
        cmd.arg("exec").arg(&container);
        cmd.args(args);
        let output = cmd.output()?;
        Ok(output)
    }

    /// Read a container's log output via `docker logs`.
    /// Used for extracting identity hashes from lnsd's startup log.
    pub fn docker_logs(&self, node: &str) -> Result<Output, RunnerError> {
        let container = self.container_name(node);
        let mut cmd = Command::new("docker");
        cmd.arg("logs").arg(&container);
        let output = cmd.output()?;
        Ok(output)
    }

    /// Execute a command inside a node's container with extra environment variables.
    ///
    /// Returns raw `Output`, the caller interprets success/failure.
    pub fn docker_exec_with_env(
        &self,
        node: &str,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Result<Output, RunnerError> {
        let container = self.container_name(node);
        let mut cmd = Command::new("docker");
        cmd.arg("exec");
        for (k, v) in env {
            cmd.arg("-e").arg(format!("{k}={v}"));
        }
        cmd.arg(&container);
        cmd.args(args);
        let output = cmd.output()?;
        Ok(output)
    }

    /// Spawn a long-running LXMF helper inside a node's container.
    ///
    /// The helper is `scripts/lxmf_node.py` driven via `docker exec -i`.
    /// Stdin is held open for the helper's lifetime and used to send
    /// commands; stdout is parsed for structured `EVENT …` lines.
    /// Errors if a helper is already running for this node.
    pub fn lxmf_spawn(
        &self,
        node: &str,
        display_name: &str,
    ) -> Result<Arc<LxmfHelper>, RunnerError> {
        {
            let guard = self.lxmf_helpers.lock().unwrap();
            if guard.contains_key(node) {
                return Err(RunnerError::Io(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("lxmf helper for '{node}' already running"),
                )));
            }
        }
        let container = self.container_name(node);
        let logs_dir = Self::logs_dir();
        let _ = fs::create_dir_all(&logs_dir);
        let ts = Self::utc_timestamp();
        let log_path = helper_log_path(&logs_dir, &self.scenario.test.name, node, &ts);
        let helper = Arc::new(LxmfHelper::spawn(container, display_name, log_path)?);
        self.lxmf_helpers
            .lock()
            .unwrap()
            .insert(node.to_string(), Arc::clone(&helper));
        Ok(helper)
    }

    /// Look up a previously spawned LXMF helper.
    pub fn lxmf_helper(&self, node: &str) -> Result<Arc<LxmfHelper>, RunnerError> {
        self.lxmf_helpers
            .lock()
            .unwrap()
            .get(node)
            .cloned()
            .ok_or_else(|| {
                RunnerError::Io(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no lxmf helper for '{node}'; call lxmf_start step first"),
                ))
            })
    }

    /// Remove a single helper entry without affecting siblings. Returns the
    /// `Arc` so the caller can `shutdown()` it. No-op if the key is absent.
    pub fn lxmf_remove(&self, node: &str) -> Option<Arc<LxmfHelper>> {
        self.lxmf_helpers.lock().unwrap().remove(node)
    }

    /// Shut down all LXMF helpers (sends `quit`, then SIGKILL on timeout).
    /// Idempotent: safe to call multiple times.
    pub fn lxmf_kill_all(&self) {
        let drained: Vec<(String, Arc<LxmfHelper>)> = {
            let mut guard = self.lxmf_helpers.lock().unwrap();
            std::mem::take(&mut *guard).into_iter().collect()
        };
        for (name, helper) in drained {
            eprintln!("[lxmf] shutting down helper for '{name}'");
            helper.shutdown();
        }
    }
}

// ---------------------------------------------------------------------------
// USB device auto-discovery
// ---------------------------------------------------------------------------

/// A discovered LNode (T114) with its two CDC-ACM ports.
#[derive(Debug, Clone)]
pub struct LNodeDevice {
    /// Debug log port (USB interface 00)
    pub debug_port: String,
    /// Reticulum serial data port (USB interface 02)
    pub data_port: String,
    /// USB serial number for deterministic ordering
    pub usb_serial: String,
}

/// Result of scanning all connected USB serial devices.
#[derive(Debug, Clone)]
pub struct DiscoveredDevices {
    /// T114 LNode devices, sorted by USB serial number
    pub lnodes: Vec<LNodeDevice>,
    /// Candidate RNode device paths (confirmed by CMD_DETECT), sorted
    pub rnodes: Vec<String>,
}

/// Cached discovery result. CMD_DETECT probing is expensive (up to 6s per
/// candidate with retries), so we run it once per process and reuse.
static DISCOVERED: std::sync::OnceLock<DiscoveredDevices> = std::sync::OnceLock::new();

/// Public for hardware-bound integration tests (e.g. the #65 LNode
/// instrumentation check) that need the LNode debug ports without a
/// scenario runner.
pub fn get_discovered_devices() -> &'static DiscoveredDevices {
    DISCOVERED.get_or_init(discover_devices)
}

/// Query udevadm properties for a device path.
/// Returns a map of key=value pairs.
fn udevadm_properties(path: &str) -> BTreeMap<String, String> {
    let output = Command::new("udevadm")
        .args(["info", "--query=property", path])
        .output();
    let mut props = BTreeMap::new();
    if let Ok(out) = output {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                props.insert(k.to_string(), v.to_string());
            }
        }
    }
    props
}

/// Scan all /dev/ttyACM* devices, classify them by USB properties, and
/// confirm RNode candidates with CMD_DETECT.
fn discover_devices() -> DiscoveredDevices {
    // Collect all ttyACM devices
    let mut acm_paths: Vec<String> = Vec::new();
    for e in fs::read_dir("/dev").into_iter().flatten().flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("ttyACM") || name.starts_with("ttyUSB") {
            acm_paths.push(format!("/dev/{name}"));
        }
    }
    acm_paths.sort();

    // Classify each device by USB vendor
    let mut lnode_ports: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new(); // serial -> (debug, data)
    let mut rnode_candidates: Vec<String> = Vec::new();

    for path in &acm_paths {
        let props = udevadm_properties(path);
        let vendor = props.get("ID_VENDOR").map(|s| s.as_str()).unwrap_or("");
        let iface_num = props
            .get("ID_USB_INTERFACE_NUM")
            .map(|s| s.as_str())
            .unwrap_or("");
        let usb_serial = props.get("ID_SERIAL_SHORT").cloned().unwrap_or_default();

        if vendor == "leviculum" {
            // T114 LNode: interface 00 = debug, interface 02 = data
            let entry = lnode_ports.entry(usb_serial).or_insert((None, None));
            match iface_num {
                "00" => entry.0 = Some(path.clone()),
                "02" => entry.1 = Some(path.clone()),
                _ => {}
            }
        } else {
            // Potential RNode, will be confirmed by CMD_DETECT probe
            rnode_candidates.push(path.clone());
        }
    }

    // Build LNode list (only include devices where both ports were found)
    let mut lnodes: Vec<LNodeDevice> = Vec::new();
    for (serial, (debug, data)) in &lnode_ports {
        if let (Some(debug_port), Some(data_port)) = (debug, data) {
            lnodes.push(LNodeDevice {
                debug_port: debug_port.clone(),
                data_port: data_port.clone(),
                usb_serial: serial.clone(),
            });
        } else {
            eprintln!(
                "[discovery] T114 {} incomplete: debug={:?} data={:?}",
                serial, debug, data
            );
        }
    }
    lnodes.sort_by(|a, b| a.usb_serial.cmp(&b.usb_serial));

    // Probe RNode candidates with CMD_DETECT
    let mut confirmed_rnodes: Vec<String> = Vec::new();
    for path in &rnode_candidates {
        // Err means "not an RNode", silently skip.
        if probe_rnode(path).is_ok() {
            confirmed_rnodes.push(path.clone());
        }
    }
    confirmed_rnodes.sort();

    let n_lnodes = lnodes.len();
    let n_rnodes = confirmed_rnodes.len();
    eprintln!("[discovery] found {n_lnodes} LNode(s), {n_rnodes} RNode(s)");
    for (i, ln) in lnodes.iter().enumerate() {
        eprintln!(
            "[discovery]   LNode {i}: debug={} data={} serial={}",
            ln.debug_port, ln.data_port, ln.usb_serial
        );
    }
    for (i, rn) in confirmed_rnodes.iter().enumerate() {
        eprintln!("[discovery]   RNode {i}: {rn}");
    }

    DiscoveredDevices {
        lnodes,
        rnodes: confirmed_rnodes,
    }
}

// ---------------------------------------------------------------------------
// RNode discovery and probing
// ---------------------------------------------------------------------------

/// Probe a serial device for RNode CMD_DETECT response.
/// Returns Ok(()) if detected, Err(reason) if not.
fn probe_rnode(path: &str) -> Result<(), String> {
    if !std::path::Path::new(path).exists() {
        return Err("device does not exist".into());
    }
    // Three attempts with increasing settle times
    for settle_ms in [0, 500, 1500] {
        match probe_rnode_once(path, settle_ms) {
            Ok(true) => return Ok(()),
            Ok(false) => continue,   // no detect response, try again
            Err(e) => return Err(e), // can't open — don't retry
        }
    }
    Err("no CMD_DETECT response after 3 attempts".into())
}

/// Returns Ok(true) if RNode detected, Ok(false) if no response, Err if can't open.
fn probe_rnode_once(path: &str, settle_ms: u64) -> Result<bool, String> {
    use std::io::{Read, Write};

    const FEND: u8 = 0xC0;
    const CMD_DETECT: u8 = 0x08;
    const DETECT_REQ: u8 = 0x73;
    const DETECT_RESP: u8 = 0x46;

    let mut port = match serialport::new(path, 115_200)
        .timeout(Duration::from_secs(1))
        .open()
    {
        Ok(p) => p,
        Err(e) => return Err(format!("cannot open: {e}")),
    };

    if settle_ms > 0 {
        thread::sleep(Duration::from_millis(settle_ms));
    }

    // Drain any pending data from the RNode (status frames, etc.)
    let _ = port.clear(serialport::ClearBuffer::Input);

    let query = [FEND, CMD_DETECT, DETECT_REQ, FEND];
    if port.write_all(&query).is_err() {
        return Ok(false);
    }

    let mut buf = [0u8; 128];
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match port.read(&mut buf) {
            Ok(n) if buf[..n].contains(&DETECT_RESP) => return Ok(true),
            Ok(_) => continue,
            Err(ref e) if e.kind() == io::ErrorKind::TimedOut => continue,
            Err(_) => return Ok(false),
        }
    }
    Ok(false)
}

/// Check that a device file exists, can be opened, and is not held by another process.
/// Returns Ok(()) if accessible, Err(reason) if not.
/// Kill any OTHER process holding `path` open before a scenario uses it.
///
/// serialport-rs opens with `TIOCEXCL`, so a board still held by a leaked
/// process cannot be opened and every scenario needing it is skipped
/// (2026-06-13 nightly: 11 `device_inaccessible` skips; the holder was a
/// wedged `reticulum_integ` process from an earlier profile group whose
/// serial-reader thread blocked after a profile power-cut and kept the
/// process — and the fd — alive). Profile power-switching is the
/// aggravator: a board powered off mid-open wedges the reader.
///
/// Best-effort: logs every kill, never fails the run, and NEVER targets
/// our own process (killing it would abort the run). A self-held fd is
/// logged loudly as a distinct diagnostic instead of being silently
/// swallowed into a device-busy skip.
fn reclaim_serial_port(path: &str) {
    let own = std::process::id();
    let out = match Command::new("lsof").args(["-t", path]).output() {
        Ok(o) => o,
        Err(_) => return, // lsof absent — best-effort, nothing to do
    };
    let pids: Vec<u32> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse::<u32>().ok())
        .collect();
    for pid in pids {
        if pid == own {
            eprintln!(
                "[reclaim] WARNING: this test process ({own}) itself holds {path} \
                 (leaked fd) — cannot reclaim by killing; the runner is leaking a \
                 serial handle, investigate rather than skip"
            );
            continue;
        }
        eprintln!("[reclaim] {path} held by leaked pid {pid}; terminating");
        let _ = Command::new("kill").arg(pid.to_string()).output();
        thread::sleep(Duration::from_millis(300));
        // Force: a holder wedged in a blocking serial read ignores SIGTERM.
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
    }
    // Give the kernel a moment to release the fd before the open retry.
    thread::sleep(Duration::from_millis(100));
}

fn check_device_accessible(path: &str) -> Result<(), String> {
    if !std::path::Path::new(path).exists() {
        return Err("does not exist".into());
    }
    // Try to open it briefly to check it's not busy
    match serialport::new(path, 115_200)
        .timeout(Duration::from_millis(100))
        .open()
    {
        Ok(_port) => Ok(()), // opened successfully, drop closes it
        Err(e) => {
            // Check if another process holds it
            let lsof = Command::new("lsof").arg(path).output().ok().and_then(|o| {
                if o.status.success() {
                    String::from_utf8(o.stdout).ok()
                } else {
                    None
                }
            });
            let holder = lsof
                .as_deref()
                .and_then(|s| s.lines().nth(1)) // skip header
                .unwrap_or("unknown");
            Err(format!("cannot open ({e}), held by: {holder}"))
        }
    }
}

/// Check for stale Docker containers or processes holding test devices.
/// Called after test teardown. Prints warnings but doesn't fail.
fn check_stale_resources(scenario: &TestScenario) {
    // Check for stale Docker containers
    if let Ok(output) = Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            "name=integ-",
            "--format",
            "{{.Names}} {{.Status}}",
        ])
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            if !line.is_empty() {
                eprintln!("[WARN] stale container: {line}");
            }
        }
    }

    // Check all device ports are released
    let mut ports: Vec<&str> = Vec::new();
    for node in scenario.nodes.values() {
        if let Some(ref p) = node.rnode_path {
            ports.push(p);
        }
        if let Some(ref p) = node.serial_path {
            ports.push(p);
        }
        if let Some(ref p) = node.debug_serial_path {
            ports.push(p);
        }
        if let Some(ref ifaces) = node.rnode_interfaces {
            for iface in ifaces {
                if let Some(ref p) = iface.rnode_path {
                    ports.push(p);
                }
            }
        }
    }
    for port in ports {
        if let Ok(output) = Command::new("lsof").arg(port).output() {
            let text = String::from_utf8_lossy(&output.stdout);
            if text.lines().count() > 1 {
                // lsof header + at least one process
                eprintln!("[WARN] device {port} still held after teardown:");
                for line in text.lines().skip(1) {
                    eprintln!("  {line}");
                }
            }
        }
    }
}

/// Kill all `integ-*` Docker containers left over from previous test runs.
///
/// Called at the start of every test to ensure no zombie containers hold
/// USB devices or ports. Logs what it kills but never fails, stale
/// containers are best-effort cleanup.
fn cleanup_stale_containers() {
    let output = Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            "name=integ-",
            "--format",
            "{{.Names}}",
        ])
        .output();
    let names: Vec<String> = match output {
        Ok(ref o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect(),
        _ => return,
    };
    if names.is_empty() {
        return;
    }
    eprintln!("[cleanup] killing {} stale container(s)", names.len());
    for name in &names {
        eprintln!("[cleanup]   {name}");
    }
    let mut args = vec!["rm".to_string(), "-f".to_string()];
    args.extend(names);
    let _ = Command::new("docker").args(&args).output();
}

/// Discover USB devices and assign them to nodes that need hardware.
///
/// Counts required RNodes and LNodes from the node definitions, compares
/// against discovered devices, and skips the test if not enough hardware
/// is available. Otherwise assigns device paths to nodes with
/// `rnode = true`, `serial = true`, or `rnode_interfaces`.
fn resolve_and_probe_rnodes(scenario: &mut TestScenario) -> Result<(), RunnerError> {
    let needed_rnodes = scenario.nodes.values().filter(|n| n.rnode).count()
        + scenario
            .nodes
            .values()
            .filter_map(|n| n.rnode_interfaces.as_ref())
            .map(|ifaces| ifaces.len())
            .sum::<usize>();
    let needed_lnodes = scenario.nodes.values().filter(|n| n.serial).count();

    // A proxy node without `rnode = true` would evade the device count
    // below and only fail later in spawn_proxies with a confusing
    // "no device assigned" (the topology-level validation runs in
    // generate_node_configs, which is too late on this path). Fail
    // clearly before counting.
    for (name, node) in &scenario.nodes {
        if node.rnode_proxy && !node.rnode {
            return Err(RunnerError::ProxyError(format!(
                "node '{name}': rnode_proxy requires `rnode = true` in the scenario TOML"
            )));
        }
    }

    // Kill stale containers from previous runs that may hold USB devices.
    cleanup_stale_containers();

    if needed_rnodes == 0 && needed_lnodes == 0 {
        return Ok(());
    }

    let discovered = get_discovered_devices();

    // Pre-check: enough hardware available? Message is structured —
    // require_runner! relays it verbatim on a SCENARIO_SKIPPED line that
    // run-tier3-hw.sh tallies into the run summary.
    if discovered.rnodes.len() < needed_rnodes || discovered.lnodes.len() < needed_lnodes {
        return Err(RunnerError::InsufficientRNodes(format!(
            "reason=insufficient_devices required_rnodes={} required_lnodes={} found_rnodes={} found_lnodes={}",
            needed_rnodes,
            needed_lnodes,
            discovered.rnodes.len(),
            discovered.lnodes.len()
        )));
    }

    // Resolve the ordered list of LNodes to bind to serial nodes.
    //
    // LEVICULUM_REQUIRED_LNODE_SERIALS (set by run-tier3-hw.sh from the
    // active profile's required LNode boards) selects LNodes by USB-serial
    // identity instead of discovery sort order. Without it (manual dev
    // runs) we fall back to the sorted discovery order. Binding by sort
    // order mis-assigned the receiver to whichever LNode sorted first
    // (Pocket-V2 ABFAB3F1807E459B before T114 DEC9947DAD9D2869), silencing
    // the profile's required board as "unused".
    let required_lnode_serials: Vec<String> = std::env::var("LEVICULUM_REQUIRED_LNODE_SERIALS")
        .ok()
        .map(|s| s.split_whitespace().map(|t| t.to_string()).collect())
        .unwrap_or_default();

    let assign_lnodes: Vec<LNodeDevice> = if required_lnode_serials.is_empty() {
        discovered.lnodes.clone()
    } else {
        let mut v = Vec::new();
        for serial in &required_lnode_serials {
            match discovered.lnodes.iter().find(|l| &l.usb_serial == serial) {
                Some(l) => v.push(l.clone()),
                None => {
                    return Err(RunnerError::InsufficientRNodes(format!(
                        "reason=required_lnode_not_found serial={} found_lnodes={}",
                        serial,
                        discovered.lnodes.len()
                    )));
                }
            }
        }
        v
    };

    if assign_lnodes.len() < needed_lnodes {
        return Err(RunnerError::InsufficientRNodes(format!(
            "reason=required_lnodes_too_few bindable_lnodes={} needed_lnodes={}",
            assign_lnodes.len(),
            needed_lnodes
        )));
    }

    // Assign discovered devices to nodes
    let mut rnode_idx: usize = 0;
    let mut lnode_idx: usize = 0;

    for (name, node) in scenario.nodes.iter_mut() {
        // Assign LNode (serial + debug)
        if node.serial {
            let lnode = &assign_lnodes[lnode_idx];
            node.serial_path = Some(lnode.data_port.clone());
            node.debug_serial_path = Some(lnode.debug_port.clone());
            eprintln!(
                "[discovery] node '{}' -> LNode {} serial={} (data={}, debug={})",
                name, lnode_idx, lnode.usb_serial, lnode.data_port, lnode.debug_port
            );
            lnode_idx += 1;
        }

        // Assign single RNode
        if node.rnode {
            node.rnode_path = Some(discovered.rnodes[rnode_idx].clone());
            eprintln!(
                "[discovery] node '{}' -> RNode {} ({})",
                name, rnode_idx, discovered.rnodes[rnode_idx]
            );
            rnode_idx += 1;
        }

        // Assign rnode_interfaces
        if let Some(ref mut interfaces) = node.rnode_interfaces {
            for iface in interfaces.iter_mut() {
                iface.rnode_path = Some(discovered.rnodes[rnode_idx].clone());
                eprintln!(
                    "[discovery] node '{}' rnode_interface -> RNode {} ({})",
                    name, rnode_idx, discovered.rnodes[rnode_idx]
                );
                rnode_idx += 1;
            }
        }
    }

    // Reclaim then verify each assigned device. Reclaim kills any leaked
    // process holding the port (a prior profile group's wedged test
    // process, a leaked native daemon/proxy) so a single stuck holder
    // cannot skip every later scenario that needs the board.
    for node in scenario.nodes.values() {
        for port in [&node.serial_path, &node.debug_serial_path, &node.rnode_path]
            .into_iter()
            .flatten()
        {
            reclaim_serial_port(port);
            check_device_accessible(port).map_err(|reason| {
                RunnerError::InsufficientRNodes(format!(
                    "reason=device_inaccessible device={port} detail=\"{reason}\""
                ))
            })?;
        }
    }

    // Silence every discovered T114 the scenario did not bind. A fresh-flashed
    // T114 defaults to csma=false and emits Reticulum announces on the
    // benchmark channel, which the sender's RNode reads as CSMA-busy and
    // backs off from. Pushing the test channel's radio config with
    // csma_enabled=true tunes the idle T114 to the same frequency (so its
    // CAD actually sees the benchmark traffic) and makes it a polite
    // neighbour.
    if let Some(ref radio) = scenario.radio {
        let assigned: std::collections::HashSet<&str> = assign_lnodes
            .iter()
            .take(lnode_idx)
            .map(|l| l.usb_serial.as_str())
            .collect();
        for lnode in discovered.lnodes.iter() {
            if !assigned.contains(lnode.usb_serial.as_str()) {
                silence_unused_lnode(&lnode.data_port, &lnode.usb_serial, radio);
            }
        }
    }

    Ok(())
}

/// Send a radio-config frame with `csma_enabled=true` to a T114 that the
/// current scenario does not bind. Best-effort: failures warn and continue./// a silent failure here only reintroduces the CSMA-busy backoff on the
/// sender.
fn silence_unused_lnode(port_path: &str, usb_serial: &str, radio: &crate::topology::RadioConfig) {
    use reticulum_core::framing::hdlc::{frame, DeframeResult, Deframer};
    use reticulum_core::rnode::{build_radio_config_frame, RadioConfigWire, RADIO_CONFIG_ACK};
    use std::io::{Read, Write};

    // Codeberg #50 Bug-A forensic instrumentation.  Structured Stage-6
    // event at function entry; matching EXIT event at every return
    // branch carries `result` ∈ {open_failed, write_error,
    // no_ack_after_3, acked}.  Lets jl/jldiff diff RNode-only and
    // T114-involved scenarios for the silence path's outcomes.
    tracing::debug!(
        event = "SILENCE_LNODE_ENTER",
        usb_serial = usb_serial,
        port_path = port_path,
    );

    let wire = RadioConfigWire {
        frequency_hz: radio.frequency as u32,
        bandwidth_hz: radio.bandwidth,
        sf: radio.spreading_factor,
        cr: radio.coding_rate,
        tx_power_dbm: radio.tx_power as i8,
        preamble_len: 24,
        csma_enabled: true,
        // Drop every outgoing LoRa frame at the driver level. CSMA alone
        // still allows the idle T114 to announce between probe bursts,
        // producing an alternating-timeout pattern even with csma=on.
        // radio_silent makes the idle T114 a listen-only neighbour.
        radio_silent: true,
    };
    let payload = build_radio_config_frame(&wire);
    let mut framed = Vec::new();
    frame(&payload, &mut framed);

    let mut port = match serialport::new(port_path, 115_200)
        .timeout(Duration::from_millis(200))
        .open()
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[silence] T114 {usb_serial} at {port_path}: open failed: {e}");
            tracing::debug!(
                event = "SILENCE_LNODE_EXIT",
                usb_serial = usb_serial,
                port_path = port_path,
                result = "open_failed",
            );
            return;
        }
    };
    // CDC-ACM only transmits after DTR is asserted (matches debug-capture path).
    let _ = port.write_data_terminal_ready(true);

    let mut any_write_ok = false;
    for attempt in 1..=3u8 {
        if let Err(e) = port.write_all(&framed) {
            eprintln!("[silence] T114 {usb_serial}: write (attempt {attempt}) failed: {e}");
            continue;
        }
        any_write_ok = true;
        let _ = port.flush();

        let mut deframer = Deframer::new();
        let mut buf = [0u8; 64];
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match port.read(&mut buf) {
                Ok(n) if n > 0 => {
                    for r in deframer.process(&buf[..n]) {
                        if let DeframeResult::Frame(data) = r {
                            if data.as_slice() == RADIO_CONFIG_ACK {
                                eprintln!(
                                    "[silence] T114 {usb_serial} at {port_path}: csma=on acked"
                                );
                                tracing::debug!(
                                    event = "SILENCE_LNODE_EXIT",
                                    usb_serial = usb_serial,
                                    port_path = port_path,
                                    result = "acked",
                                );
                                return;
                            }
                        }
                    }
                }
                Ok(_) => {}
                Err(ref e) if e.kind() == io::ErrorKind::TimedOut => continue,
                Err(e) => {
                    eprintln!("[silence] T114 {usb_serial}: read error: {e}");
                    break;
                }
            }
        }
        eprintln!("[silence] T114 {usb_serial}: attempt {attempt}/3 no ack, retrying");
    }
    eprintln!(
        "[silence] T114 {usb_serial} at {port_path}: NO ACK after 3 attempts — idle T114 may still disturb channel"
    );
    let result = if any_write_ok {
        "no_ack_after_3"
    } else {
        "write_error"
    };
    tracing::debug!(
        event = "SILENCE_LNODE_EXIT",
        usb_serial = usb_serial,
        port_path = port_path,
        result = result,
    );
}

/// Spawn lora-proxy processes for all nodes with `rnode_proxy = true`.
///
/// Returns:
/// - Vec of child process handles
/// - BTreeMap of node name -> control socket path
/// - BTreeMap of node name -> PTY slave device path (for compose device mapping)
type ProxySpawnResult = (
    Vec<Child>,
    BTreeMap<String, PathBuf>,
    BTreeMap<String, PathBuf>,
);

/// RAII guard for spawned lora-proxy children. `std::process::Child::drop`
/// does NOT kill the process, so any early return from `spawn_proxies`
/// before success would leak native proxies that hold `/dev/ttyACM*`
/// (2026-06-13 nightly root: the `fs::read_link` error path returned via
/// `?` and dropped the children Vec without killing). The guard kills
/// every child on drop unless `disarm`ed once the proxies are handed to
/// the caller, so ALL error paths are covered, not just the one we knew.
struct ProxyChildGuard {
    children: Vec<Child>,
}

impl ProxyChildGuard {
    fn new() -> Self {
        ProxyChildGuard {
            children: Vec::new(),
        }
    }
    fn push(&mut self, child: Child) {
        self.children.push(child);
    }
    /// Hand the children to the caller; the guard no longer kills them.
    fn disarm(mut self) -> Vec<Child> {
        std::mem::take(&mut self.children)
    }
}

impl Drop for ProxyChildGuard {
    fn drop(&mut self) {
        for mut c in self.children.drain(..) {
            eprintln!(
                "[cleanup] killing leaked lora-proxy child pid {} (spawn_proxies error path)",
                c.id()
            );
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn spawn_proxies(
    scenario: &TestScenario,
    run_id: u32,
    target_dir: &std::path::Path,
) -> Result<ProxySpawnResult, RunnerError> {
    let proxy_bin = crate::paths::release_bin(target_dir, "lora-proxy");
    let mut children = ProxyChildGuard::new();
    let mut sockets = BTreeMap::new();
    let mut devices = BTreeMap::new();

    for (name, node) in &scenario.nodes {
        if !node.rnode_proxy {
            continue;
        }
        let device = node.rnode_path.as_ref().ok_or_else(|| {
            RunnerError::ProxyError(format!(
                "node '{name}': rnode_proxy requires rnode (no device assigned)"
            ))
        })?;

        let pty_path = PathBuf::from(format!(
            "/tmp/proxy-{}-{run_id}-{name}.pty",
            scenario.test.name
        ));
        let sock_path = PathBuf::from(format!(
            "/tmp/proxy-{}-{run_id}-{name}.sock",
            scenario.test.name
        ));

        // Clean up stale files from previous runs.
        let _ = fs::remove_file(&pty_path);
        let _ = fs::remove_file(&sock_path);

        let child = Command::new(&proxy_bin)
            .args([
                "hardware",
                "--device",
                device,
                "--pty-out",
                pty_path.to_str().expect("pty path not UTF-8"),
                "--control",
                sock_path.to_str().expect("sock path not UTF-8"),
            ])
            .env("RUST_LOG", "debug")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                RunnerError::ProxyError(format!("failed to spawn lora-proxy for {name}: {e}"))
            })?;

        children.push(child);
        sockets.insert(name.clone(), sock_path);

        // Wait for the PTY symlink to appear (proxy creates it on startup).
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if pty_path.exists() {
                break;
            }
            if Instant::now() >= deadline {
                // ProxyChildGuard kills every spawned child on drop.
                return Err(RunnerError::ProxyError(format!(
                    "proxy for node '{name}' did not create PTY at {} within 5s",
                    pty_path.display()
                )));
            }
            thread::sleep(Duration::from_millis(50));
        }

        // Resolve the PTY symlink to get the actual /dev/pts/N path.
        let real_pty = fs::read_link(&pty_path).map_err(|e| {
            RunnerError::ProxyError(format!(
                "cannot read PTY symlink {}: {e}",
                pty_path.display()
            ))
        })?;
        devices.insert(name.clone(), real_pty);
    }

    Ok((children.disarm(), sockets, devices))
}

impl Drop for TestRunner {
    fn drop(&mut self) {
        if self.is_up {
            // Best-effort teardown on panic or early return.
            let _ = self.down();
        }
        // Ensure helpers, proxies, and capture threads are released even if
        // down() wasn't called (e.g. up() failed before is_up was set).
        self.lxmf_kill_all();
        self.kill_proxies();
        self.stop_debug_captures();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::parse_scenario;

    /// Root-2 regression (2026-06-13 nightly): a leaked process holding a
    /// board's serial port blocked every later scenario needing it.
    /// `reclaim_serial_port` must kill the holder so the port is free.
    /// Hardware-free: a regular temp file stands in for the device — lsof
    /// reports holders of regular files exactly as for /dev/ttyACM*.
    #[test]
    fn reclaim_serial_port_kills_leaked_holder() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dev = tmp.path().join("fake-ttyACM0");
        std::fs::write(&dev, b"x").expect("write fake device");
        let dev_str = dev.to_str().unwrap();

        // Holder: open the file read-only on fd 9 and sleep, so it shows
        // up in lsof exactly like a process holding a serial port.
        let mut holder = Command::new("sh")
            .args(["-c", &format!("exec 9< '{dev_str}'; sleep 30")])
            .spawn()
            .expect("spawn holder");

        let lsof_holds = |path: &str| -> bool {
            Command::new("lsof")
                .args(["-t", path])
                .output()
                .map(|o| !o.stdout.iter().all(u8::is_ascii_whitespace))
                .unwrap_or(false)
        };

        // Wait until lsof actually sees the holder (open + exec race).
        let mut held = false;
        for _ in 0..50 {
            if lsof_holds(dev_str) {
                held = true;
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(
            held,
            "precondition: holder must hold the file (is lsof installed?)"
        );

        // Unit under test: the holder is freed.
        reclaim_serial_port(dev_str);
        assert!(
            !lsof_holds(dev_str),
            "reclaim_serial_port must free the port (holder still present)"
        );

        let _ = holder.kill();
        let _ = holder.wait();
    }

    /// Lifecycle regression (2026-06-13): the debug capture thread used to be
    /// detached with no shutdown path, so it held its serial port for the life
    /// of the process and reopened it on read error. Across a `--test-threads=1`
    /// profile group that leaked port blocked every later LNode scenario with
    /// `device_inaccessible`. The fix gives the threads a stop flag + join.
    ///
    /// Hardware-free: a Linux pty pair stands in for the board. serialport-rs
    /// opens the slave exclusively (TIOCEXCL), so while the capture thread holds
    /// it a fresh open returns EBUSY; once the thread stops and drops the handle
    /// the port becomes openable again. Pre-fix (thread ignores the flag) the
    /// port stays held and this assertion fails; post-fix it is released.
    #[test]
    fn debug_capture_releases_port_on_stop() {
        use std::ffi::CStr;
        use std::sync::atomic::AtomicBool;

        // Create a pty pair; the slave is the stand-in "debug serial device".
        let mut master: libc::c_int = 0;
        let mut slave: libc::c_int = 0;
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        assert_eq!(rc, 0, "openpty failed (no pty available?)");
        let mut namebuf = [0 as libc::c_char; 256];
        let rc = unsafe { libc::ttyname_r(slave, namebuf.as_mut_ptr(), namebuf.len()) };
        assert_eq!(rc, 0, "ttyname_r failed");
        let pty = unsafe { CStr::from_ptr(namebuf.as_ptr()) }
            .to_str()
            .expect("pts path not UTF-8")
            .to_string();
        // Close the slave fd but keep the master open so the pty stays alive;
        // the capture thread reopens the slave by path.
        unsafe {
            libc::close(slave);
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        let log_path = tmp.path().join("debug.log");
        let stop = Arc::new(AtomicBool::new(false));

        let try_open = |path: &str| {
            serialport::new(path, 115_200)
                .timeout(Duration::from_millis(250))
                .open()
        };

        let handle = spawn_debug_capture(pty.clone(), log_path, Arc::clone(&stop))
            .expect("spawn_debug_capture should open the pty");

        // Precondition: the capture thread holds the port exclusively, so a
        // fresh open from this thread must fail. Allow for the open + TIOCEXCL
        // race by polling briefly.
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut held = false;
        while Instant::now() < deadline {
            if try_open(&pty).is_err() {
                held = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            held,
            "precondition: capture thread should hold the port exclusively"
        );

        // Teardown: signal stop, exactly as `stop_debug_captures` does. Poll
        // for release with a bounded deadline rather than joining first, so the
        // pre-fix case (thread ignores the flag, never exits) fails cleanly
        // here instead of hanging on a join that never returns.
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut released = false;
        while Instant::now() < deadline {
            if try_open(&pty).is_ok() {
                released = true;
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(
            released,
            "port not released after stop: capture thread did not exit"
        );

        // The thread has exited; the join must return promptly.
        let _ = handle.join();

        unsafe {
            libc::close(master);
        }
    }

    #[test]
    fn container_name_format() {
        assert_eq!(
            format_container_name("basic_probe", 42, "alice"),
            "integ-basic_probe-42-alice"
        );
    }

    #[test]
    fn project_name_format() {
        assert_eq!(
            format_project_name("basic_probe", 42),
            "integ-basic_probe-42"
        );
    }

    #[test]
    fn lnsd_not_found_error_message() {
        let err = RunnerError::BinaryNotFound(PathBuf::from("/some/path/lnsd"));
        let msg = err.to_string();
        assert!(msg.contains("/some/path/lnsd"), "should contain path");
        assert!(
            msg.contains("cargo build --release --bin lnsd"),
            "should contain build hint"
        );
    }

    #[test]
    fn compose_error_display() {
        let err = RunnerError::Compose {
            action: "up -d".into(),
            stderr: "no such image".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("up -d"), "should contain action");
        assert!(msg.contains("no such image"), "should contain stderr");
    }

    #[test]
    fn readiness_timeout_display() {
        let err = RunnerError::ReadinessTimeout {
            node: "alice".into(),
            timeout_secs: 30,
        };
        let msg = err.to_string();
        assert!(msg.contains("alice"), "should contain node name");
        assert!(msg.contains("30"), "should contain timeout seconds");
    }

    #[test]
    fn basic_probe_lifecycle() {
        let toml_str = fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/basic_probe.toml"
        ))
        .expect("basic_probe.toml not found");
        let scenario = parse_scenario(&toml_str).expect("parse failed");

        let mut runner = TestRunner::new(scenario).expect("TestRunner::new failed");

        // Verify compose file was generated.
        assert!(runner.compose_file.exists(), "compose file should exist");

        runner.up().expect("up failed");
        runner.wait_ready(30).expect("wait_ready failed");

        // Verify rnstatus works on both nodes.
        for node in ["alice", "bob"] {
            let output = runner
                .docker_exec(node, &["rnstatus", "--config", "/root/.reticulum"])
                .expect("docker_exec failed");
            assert!(
                output.status.success(),
                "rnstatus on {node} should succeed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        runner.down().expect("down failed");
    }
}
