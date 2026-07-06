use rand::RngCore;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::io;
use std::path::Path;

// ---------------------------------------------------------------------------
// TOML data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct TestScenario {
    pub test: TestMeta,
    pub nodes: BTreeMap<String, NodeDef>,
    #[serde(default)]
    pub links: BTreeMap<String, LinkDef>,
    #[serde(default)]
    pub radio: Option<RadioConfig>,
    #[serde(default)]
    pub steps: Vec<Step>,
}

/// Link definition: either a simple type string or a detailed table with IFAC config.
///
/// Simple form (backward compatible): `alice-bob = "tcp"` or `alice-bob = "auto"`
/// Detailed form: `[links.alice-bob]` with `type`, `networkname`, `passphrase`,
/// `ifac_size`, `group_id`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum LinkDef {
    Simple(String),
    Detailed {
        #[serde(rename = "type")]
        link_type: String,
        #[serde(default)]
        networkname: Option<String>,
        #[serde(default)]
        passphrase: Option<String>,
        /// IFAC size in bits (divided by 8 for bytes, matching Python convention).
        #[serde(default)]
        ifac_size: Option<usize>,
        /// AutoInterface group_id. Only meaningful for `type = "auto"`.
        /// When omitted, the assigner derives one from the test name.
        #[serde(default)]
        group_id: Option<String>,
    },
}

impl LinkDef {
    pub fn link_type(&self) -> &str {
        match self {
            LinkDef::Simple(t) => t,
            LinkDef::Detailed { link_type, .. } => link_type,
        }
    }

    pub fn networkname(&self) -> Option<&str> {
        match self {
            LinkDef::Simple(_) => None,
            LinkDef::Detailed { networkname, .. } => networkname.as_deref(),
        }
    }

    pub fn passphrase(&self) -> Option<&str> {
        match self {
            LinkDef::Simple(_) => None,
            LinkDef::Detailed { passphrase, .. } => passphrase.as_deref(),
        }
    }

    pub fn ifac_size(&self) -> Option<usize> {
        match self {
            LinkDef::Simple(_) => None,
            LinkDef::Detailed { ifac_size, .. } => *ifac_size,
        }
    }

    pub fn group_id(&self) -> Option<&str> {
        match self {
            LinkDef::Simple(_) => None,
            LinkDef::Detailed { group_id, .. } => group_id.as_deref(),
        }
    }
}

// Fields populated by serde deserialization appear unused to the compiler.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct TestMeta {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// RUST_LOG filter for daemon containers. Defaults to "debug" if unset.
    #[serde(default)]
    pub rust_log: Option<String>,
}

fn default_timeout() -> u64 {
    60
}

#[derive(Debug, Clone, Deserialize)]
pub struct NodeDef {
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(default)]
    pub respond_to_probes: bool,
    #[serde(default = "default_true")]
    pub enable_transport: bool,
    /// This node needs an RNode device. Mutually exclusive with `rnode_interfaces`.
    #[serde(default)]
    pub rnode: bool,
    /// When true, a lora-proxy process sits between the host device and the
    /// container, enabling fault-injection steps (proxy_rule, proxy_stats).
    #[serde(default)]
    pub rnode_proxy: bool,
    /// TCP server port for local tool access (e.g., selftest).
    /// Generates a standalone TCPServerInterface, independent of `[links]`.
    #[serde(default)]
    pub listen_port: Option<u16>,
    /// Multiple RNode interfaces with per-interface radio parameters.
    /// Mutually exclusive with `rnode`, use this for multi-frequency nodes.
    #[serde(default)]
    pub rnode_interfaces: Option<Vec<RNodeInterfaceDef>>,
    /// This node needs an LNode serial connection (e.g., T114).
    /// Debug serial port is auto-assigned from the same LNode.
    #[serde(default)]
    pub serial: bool,

    // Resolved device paths (filled by runner, not in TOML)
    #[serde(skip)]
    pub rnode_path: Option<String>,
    #[serde(skip)]
    pub serial_path: Option<String>,
    #[serde(skip)]
    pub debug_serial_path: Option<String>,
}

/// Per-interface RNode definition with independent radio parameters.
/// Device paths are auto-assigned by the runner from discovered RNodes.
#[derive(Debug, Clone, Deserialize)]
pub struct RNodeInterfaceDef {
    pub frequency: u64,
    pub bandwidth: u32,
    #[serde(alias = "sf")]
    pub spreading_factor: u8,
    #[serde(alias = "cr")]
    pub coding_rate: u8,
    #[serde(alias = "txpower")]
    pub tx_power: u8,

    /// Resolved device path (filled by runner, not in TOML).
    #[serde(skip)]
    pub rnode_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RadioConfig {
    pub frequency: u64,
    pub bandwidth: u32,
    #[serde(alias = "sf")]
    pub spreading_factor: u8,
    #[serde(alias = "cr")]
    pub coding_rate: u8,
    #[serde(alias = "txpower")]
    pub tx_power: u8,
    #[serde(default)]
    pub csma_enabled: bool,
}

fn default_true() -> bool {
    true
}

// Fields on unimplemented step variants are populated by serde but not yet
// read by the executor (see unimplemented!() arms in executor.rs).
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action")]
pub enum Step {
    #[serde(rename = "wait_for_path")]
    WaitForPath {
        on: String,
        destination: String,
        #[serde(default = "default_step_timeout")]
        timeout_secs: u64,
        #[serde(default = "default_expect_success")]
        expect_result: String,
    },
    #[serde(rename = "rnprobe")]
    RnProbe {
        from: String,
        to: String,
        #[serde(default)]
        expect_hops: Option<u32>,
        #[serde(default)]
        expect_hops_max: Option<u32>,
        #[serde(default = "default_expect_success")]
        expect_result: String,
        #[serde(default = "default_step_timeout")]
        timeout_secs: u64,
    },
    #[serde(rename = "rnpath")]
    RnPath {
        on: String,
        destination: String,
        #[serde(default)]
        expect_min_hops: Option<u32>,
        #[serde(default)]
        expect_max_hops: Option<u32>,
    },
    #[serde(rename = "rnstatus")]
    RnStatus {
        on: String,
        #[serde(default)]
        expect_transport: Option<bool>,
    },
    #[serde(rename = "exec")]
    Exec {
        on: String,
        command: String,
        #[serde(default)]
        expect_exit_code: Option<i32>,
        /// Accept any one of several exit codes. Use this instead of
        /// `expect_exit_code` for a command whose process exit code is racy
        /// but whose functional output (asserted via `expect_stdout_contains`)
        /// is deterministic. It still fails on any code outside the listed
        /// set, so a genuine error (RPC failure, crash) is not masked. See
        /// rnpath_conformance.toml for the documented rnpath shutdown race.
        #[serde(default)]
        expect_exit_code_any: Vec<i32>,
        #[serde(default)]
        expect_stdout_contains: Option<String>,
        /// Extra environment variables passed to `docker exec -e`.
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    #[serde(rename = "sleep")]
    Sleep { duration_secs: u64 },
    #[serde(rename = "restart")]
    Restart { node: String },
    /// Block traffic between two containers using iptables DROP rules.
    /// Must be applied on both sides for a clean bidirectional block.
    #[serde(rename = "block_link")]
    BlockLink { from: String, to: String },
    /// Restore traffic between two containers by removing iptables DROP rules.
    #[serde(rename = "restore_link")]
    RestoreLink { from: String, to: String },
    /// Add a fault-injection rule to a node's lora-proxy.
    #[serde(rename = "proxy_rule")]
    ProxyRule { node: String, rule: String },
    /// Clear proxy rules: `id` can be a numeric rule ID or "all".
    #[serde(rename = "proxy_rule_clear")]
    ProxyRuleClear { node: String, id: String },
    /// Query proxy stats and optionally assert on counters.
    #[serde(rename = "proxy_stats")]
    ProxyStats {
        node: String,
        #[serde(default)]
        expect_dropped: Option<u64>,
        #[serde(default)]
        expect_forwarded: Option<u64>,
    },
    /// End-to-end file transfer with md5sum verification and timing.
    #[serde(rename = "file_transfer")]
    FileTransfer {
        sender: String,
        receiver: String,
        sender_tool: String,
        receiver_tool: String,
        file_sizes: Vec<u64>,
        #[serde(default = "default_direction")]
        direction: String,
        #[serde(default = "default_repeats")]
        repeats: u32,
        #[serde(default = "default_transfer_timeout")]
        timeout_secs: u64,
        /// "push" (default) or "fetch", fetch reverses file creation/verification.
        #[serde(default = "default_mode")]
        mode: String,
        /// Extra flags appended to the listener command (e.g., "-F -n", "-F -j /tmp/jail").
        #[serde(default)]
        receiver_flags: String,
        /// Extra flags appended to the sender command.
        #[serde(default)]
        sender_flags: String,
        /// Node whose identity hash is used for `-a` on the listener.
        /// Empty = no auth (default: `-n` for backward compat).
        #[serde(default)]
        auth_from: String,
        /// "success" (default) or "failure", expect the transfer to fail.
        #[serde(default = "default_expect_success")]
        expect_result: String,
        /// Override remote file path for fetch (default: /tmp/fetchable/test_transfer.bin).
        /// Used for jail violation tests to request a path outside the jail.
        #[serde(default)]
        fetch_path: String,
    },
    /// Run multiple file transfers simultaneously in separate threads.
    /// All nodes must be disjoint (no node appears in more than one transfer).
    #[serde(rename = "parallel_file_transfers")]
    ParallelFileTransfers {
        transfers: Vec<ParallelTransferDef>,
        #[serde(default = "default_transfer_timeout")]
        timeout_secs: u64,
        #[serde(default = "default_expect_success")]
        expect_result: String,
    },
    /// Probe throughput benchmark: send probes between pairs for a duration.
    #[serde(rename = "benchmark")]
    Benchmark {
        pairs: Vec<BenchmarkPair>,
        #[serde(default = "default_benchmark_duration")]
        duration_secs: u64,
        #[serde(default)]
        label: String,
    },
    /// Spawn the long-running LXMF helper (`scripts/lxmf_node.py`) inside
    /// `on`'s container. Waits for the helper's `EVENT lxmf_ready hash=…`
    /// line and stores the destination hash under cache key `lxmf_hash:<on>`.
    #[serde(rename = "lxmf_start")]
    LxmfStart {
        on: String,
        #[serde(default)]
        display_name: Option<String>,
        #[serde(default = "default_lxmf_ready_timeout")]
        timeout_secs: u64,
    },
    /// Trigger an LXMF announce on `on`'s helper.
    #[serde(rename = "lxmf_announce")]
    LxmfAnnounce { on: String },
    /// Block until `on`'s helper has learned `peer`'s identity AND has a
    /// resolved path to it. `peer` is a node name; the corresponding hash
    /// must already be in the cache (i.e. `peer` ran `lxmf_start` first).
    #[serde(rename = "lxmf_wait_for_peer")]
    LxmfWaitForPeer {
        on: String,
        peer: String,
        #[serde(default = "default_step_timeout")]
        timeout_secs: u64,
    },
    /// Send an LXMF message from `from` to `to` with `body` as UTF-8 content.
    /// Does not block on delivery; pair with `lxmf_assert_received`.
    #[serde(rename = "lxmf_send")]
    LxmfSend {
        from: String,
        to: String,
        body: String,
    },
    /// Block until `on`'s helper has reported a received LXMF message from
    /// `from` whose body equals `body` (UTF-8 byte-exact comparison).
    #[serde(rename = "lxmf_assert_received")]
    LxmfAssertReceived {
        on: String,
        from: String,
        body: String,
        #[serde(default = "default_lxmf_assert_timeout")]
        timeout_secs: u64,
    },
    /// Send `quit` to `on`'s helper and wait for it to exit cleanly.
    /// Optional: the runner shuts every helper down at test teardown.
    #[serde(rename = "lxmf_stop")]
    LxmfStop { on: String },
    /// Launch a Python rnsd inside `on`'s container as a second, persistent
    /// shared-instance CLIENT of the daemon already running there (a Rust
    /// lnsd shared-instance server). The step launches rnsd detached, then
    /// blocks until rnsd logs it attached to the existing instance AND the
    /// server logged it accepted the client. Fails loudly (with both logs)
    /// if rnsd starts its own transport, aborts, or never attaches.
    #[serde(rename = "start_shared_client")]
    StartSharedClient {
        on: String,
        #[serde(default = "default_shared_client_log")]
        log_path: String,
        #[serde(default = "default_step_timeout")]
        timeout_secs: u64,
    },
}

/// Benchmark pair: probe from one node to another.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct BenchmarkPair {
    pub from: String,
    pub to: String,
}

fn default_benchmark_duration() -> u64 {
    60
}

/// Simplified transfer definition for parallel execution.
// Fields read via serde deserialization, compiler cannot
// see the indirect access through TOML parsing.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct ParallelTransferDef {
    pub sender: String,
    pub receiver: String,
    #[serde(default = "default_sender_tool")]
    pub sender_tool: String,
    #[serde(default = "default_receiver_tool")]
    pub receiver_tool: String,
    pub file_size: u64,
    #[serde(default = "default_push_mode")]
    pub mode: String,
    #[serde(default = "default_transfer_timeout")]
    pub timeout_secs: u64,
}

fn default_mode() -> String {
    "push".to_string()
}

fn default_direction() -> String {
    "both".to_string()
}

fn default_repeats() -> u32 {
    3
}

fn default_transfer_timeout() -> u64 {
    300
}

fn default_step_timeout() -> u64 {
    30
}

fn default_shared_client_log() -> String {
    "/tmp/rnsd_client.log".to_string()
}

fn default_lxmf_ready_timeout() -> u64 {
    30
}

fn default_lxmf_assert_timeout() -> u64 {
    60
}

fn default_expect_success() -> String {
    "success".into()
}

fn default_push_mode() -> String {
    "push".to_string()
}

fn default_sender_tool() -> String {
    "lncp".to_string()
}

fn default_receiver_tool() -> String {
    "lncp".to_string()
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

pub fn parse_scenario(toml_str: &str) -> Result<TestScenario, toml::de::Error> {
    toml::from_str(toml_str)
}

// ---------------------------------------------------------------------------
// Environment-variable radio overrides
// ---------------------------------------------------------------------------

/// Return the timeout scale factor from `LORA_TIMEOUT_SCALE` env var (default 1.0).
///
/// When running tests at slower bitrates, all step timeouts and sleep durations
/// need to be scaled proportionally. Set `LORA_TIMEOUT_SCALE=3` for ~3x slower
/// radio settings (e.g., SF10 vs SF7).
pub fn timeout_scale() -> f64 {
    std::env::var("LORA_TIMEOUT_SCALE")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(1.0)
}

/// Scale a timeout value by the `LORA_TIMEOUT_SCALE` factor.
pub fn scale_timeout(secs: u64) -> u64 {
    let scaled = secs as f64 * timeout_scale();
    scaled.ceil() as u64
}

/// Override radio parameters from environment variables.
///
/// If the scenario has a `[radio]` section, any of these env vars override
/// the corresponding field:
///   - `LORA_FREQUENCY` (u64, Hz)
///   - `LORA_BANDWIDTH` (u32, Hz)
///   - `LORA_SF` (u8, spreading factor)
///   - `LORA_CR` (u8, coding rate)
///   - `LORA_TXPOWER` (u8, dBm)
///
/// This allows running the same TOML test files with different radio settings
/// without modifying them.
pub fn apply_radio_overrides(scenario: &mut TestScenario) {
    let radio = match scenario.radio.as_mut() {
        Some(r) => r,
        None => return,
    };

    if let Ok(val) = std::env::var("LORA_FREQUENCY") {
        if let Ok(v) = val.parse::<u64>() {
            radio.frequency = v;
        }
    }
    if let Ok(val) = std::env::var("LORA_BANDWIDTH") {
        if let Ok(v) = val.parse::<u32>() {
            radio.bandwidth = v;
        }
    }
    if let Ok(val) = std::env::var("LORA_SF") {
        if let Ok(v) = val.parse::<u8>() {
            radio.spreading_factor = v;
        }
    }
    if let Ok(val) = std::env::var("LORA_CR") {
        if let Ok(v) = val.parse::<u8>() {
            radio.coding_rate = v;
        }
    }
    if let Ok(val) = std::env::var("LORA_TXPOWER") {
        if let Ok(v) = val.parse::<u8>() {
            radio.tx_power = v;
        }
    }
}

// ---------------------------------------------------------------------------
// Interface assignment from links
// ---------------------------------------------------------------------------

/// IFAC parameters resolved from a LinkDef for a specific interface.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IfacParams {
    pub networkname: Option<String>,
    pub passphrase: Option<String>,
    /// IFAC size in bits (matching Python convention).
    pub ifac_size: Option<usize>,
}

/// A single interface entry for a node's config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterfaceEntry {
    TcpServer {
        peer: String,
        port: u16,
        ifac: IfacParams,
    },
    TcpClient {
        peer: String,
        target_host: String,
        port: u16,
        ifac: IfacParams,
    },
    /// AutoInterface (IPv6 link-local multicast). All nodes sharing a
    /// `group_id` discover each other automatically.
    Auto { group_id: String },
}

/// Default AutoInterface group_id derived from the test name.
pub fn default_auto_group_id(test_name: &str) -> String {
    format!("test_{test_name}")
}

/// Parse all links and return a map: node_name -> `Vec<InterfaceEntry>`.
///
/// TCP links: for key "X-Y" (X < Y alphabetically), X gets the server role
/// and Y gets the client role. Each server node gets a unique port starting
/// from 4242, incrementing per additional link.
///
/// Auto links: every node appearing in any auto link gets a single
/// `[[Auto]]` entry per distinct `group_id`. Default group_id is
/// `test_<test_name>` so parallel test runs do not see each other.
pub fn assign_interfaces(
    links: &BTreeMap<String, LinkDef>,
    test_name: &str,
) -> Result<BTreeMap<String, Vec<InterfaceEntry>>, String> {
    let mut interfaces: BTreeMap<String, Vec<InterfaceEntry>> = BTreeMap::new();
    // Track next port per server node.
    let mut server_ports: BTreeMap<String, u16> = BTreeMap::new();
    // Set of (node, group_id) pairs seen for auto links — dedupes when a
    // node appears in multiple auto pair-links sharing the same group_id.
    let mut auto_memberships: BTreeMap<String, std::collections::BTreeSet<String>> =
        BTreeMap::new();

    for (link_key, link_def) in links {
        let parts: Vec<&str> = link_key.split('-').collect();
        if parts.len() != 2 {
            return Err(format!("link key must be 'nodeA-nodeB', got: {link_key}"));
        }

        match link_def.link_type() {
            "tcp" => {
                // Alphabetically first = server, second = client.
                let (server, client) = if parts[0] <= parts[1] {
                    (parts[0].to_string(), parts[1].to_string())
                } else {
                    (parts[1].to_string(), parts[0].to_string())
                };

                let ifac = IfacParams {
                    networkname: link_def.networkname().map(String::from),
                    passphrase: link_def.passphrase().map(String::from),
                    ifac_size: link_def.ifac_size(),
                };

                let port = server_ports.entry(server.clone()).or_insert(4242);
                let current_port = *port;
                *port += 1;

                interfaces
                    .entry(server.clone())
                    .or_default()
                    .push(InterfaceEntry::TcpServer {
                        peer: client.clone(),
                        port: current_port,
                        ifac: ifac.clone(),
                    });

                interfaces
                    .entry(client.clone())
                    .or_default()
                    .push(InterfaceEntry::TcpClient {
                        peer: server.clone(),
                        target_host: server.clone(),
                        port: current_port,
                        ifac,
                    });
            }
            "auto" => {
                let group_id = link_def
                    .group_id()
                    .map(String::from)
                    .unwrap_or_else(|| default_auto_group_id(test_name));
                for node in [parts[0], parts[1]] {
                    auto_memberships
                        .entry(node.to_string())
                        .or_default()
                        .insert(group_id.clone());
                }
            }
            other => {
                return Err(format!("unsupported link type: {other}"));
            }
        }
    }

    // Emit one [[Auto]] entry per (node, group_id) pair.
    for (node, group_ids) in auto_memberships {
        let entry = interfaces.entry(node).or_default();
        for group_id in group_ids {
            entry.push(InterfaceEntry::Auto { group_id });
        }
    }

    Ok(interfaces)
}

// ---------------------------------------------------------------------------
// Config generation
// ---------------------------------------------------------------------------

/// Emit IFAC config keys (networkname, passphrase, ifac_size) into an INI interface section.
fn render_ifac_params(out: &mut String, ifac: &IfacParams) {
    if let Some(nn) = &ifac.networkname {
        writeln!(out, "    networkname = {nn}").ok();
    }
    if let Some(pp) = &ifac.passphrase {
        writeln!(out, "    passphrase = {pp}").ok();
    }
    if let Some(sz) = ifac.ifac_size {
        writeln!(out, "    ifac_size = {sz}").ok();
    }
}

/// Generate the Reticulum INI config string for a node.
pub fn render_config(
    node: &NodeDef,
    ifaces: &[InterfaceEntry],
    radio: Option<&RadioConfig>,
) -> String {
    let mut out = String::new();

    let enabled = if node.enable_transport { "yes" } else { "no" };
    let probes = if node.respond_to_probes { "yes" } else { "no" };

    writeln!(out, "[reticulum]").ok();
    writeln!(out, "  enable_transport = {enabled}").ok();
    writeln!(out, "  share_instance = yes").ok();
    writeln!(out, "  respond_to_probes = {probes}").ok();
    writeln!(out).ok();
    writeln!(out, "[logging]").ok();
    writeln!(out, "  loglevel = 5").ok();
    writeln!(out).ok();
    writeln!(out, "[interfaces]").ok();

    for iface in ifaces {
        match iface {
            InterfaceEntry::TcpServer { peer, port, ifac } => {
                writeln!(out, "  [[TCPServerInterface-{peer}]]").ok();
                writeln!(out, "    type = TCPServerInterface").ok();
                writeln!(out, "    enabled = yes").ok();
                writeln!(out, "    listen_ip = 0.0.0.0").ok();
                writeln!(out, "    listen_port = {port}").ok();
                // Disable ingress control in integration tests to avoid
                // non-deterministic announce suppression during rapid startup.
                writeln!(out, "    ingress_control = false").ok();
                render_ifac_params(&mut out, ifac);
            }
            InterfaceEntry::TcpClient {
                peer,
                target_host,
                port,
                ifac,
            } => {
                writeln!(out, "  [[TCPClientInterface-{peer}]]").ok();
                writeln!(out, "    type = TCPClientInterface").ok();
                writeln!(out, "    enabled = yes").ok();
                writeln!(out, "    target_host = {target_host}").ok();
                writeln!(out, "    target_port = {port}").ok();
                writeln!(out, "    ingress_control = false").ok();
                render_ifac_params(&mut out, ifac);
            }
            InterfaceEntry::Auto { group_id } => {
                writeln!(out, "  [[Auto-{group_id}]]").ok();
                writeln!(out, "    type = AutoInterface").ok();
                writeln!(out, "    enabled = yes").ok();
                writeln!(out, "    group_id = {group_id}").ok();
                // discovery_scope = link confines multicast to the local
                // segment (Docker bridge), which is what we want for
                // container tests.
                writeln!(out, "    discovery_scope = link").ok();
            }
        }
    }

    if let (Some(ref port), Some(radio)) = (&node.rnode_path, radio) {
        let device_path = if node.rnode_proxy {
            "/dev/rnode_proxy".to_string()
        } else {
            port.clone()
        };
        writeln!(out, "  [[RNode Interface]]").ok();
        writeln!(out, "    type = RNodeInterface").ok();
        writeln!(out, "    enabled = yes").ok();
        writeln!(out, "    port = {device_path}").ok();
        writeln!(out, "    frequency = {}", radio.frequency).ok();
        writeln!(out, "    bandwidth = {}", radio.bandwidth).ok();
        writeln!(out, "    spreadingfactor = {}", radio.spreading_factor).ok();
        writeln!(out, "    codingrate = {}", radio.coding_rate).ok();
        writeln!(out, "    txpower = {}", radio.tx_power).ok();
        writeln!(out, "    ingress_control = false").ok();
        // Codeberg #55: the regulatory duty-cycle default is frequency-aware
        // (lawful-by-default), so our test frequency 869.525 MHz would enforce
        // the ETSI P sub-band at 10% and throttle the run. Automated tests
        // measure the stack, not the regulator, so pin the gate off. Rust-only:
        // Python peers are never throttled by our gate.
        if node.node_type == "rust" {
            writeln!(out, "    duty_cycle = off").ok();
        }
    }

    // Multi-interface: render one [[RNode Interface N]] per entry.
    if let Some(ref interfaces) = node.rnode_interfaces {
        for (i, iface) in interfaces.iter().enumerate() {
            let port = iface.rnode_path.as_deref().unwrap_or("UNRESOLVED");
            writeln!(out, "  [[RNode Interface {i}]]").ok();
            writeln!(out, "    type = RNodeInterface").ok();
            writeln!(out, "    enabled = yes").ok();
            writeln!(out, "    port = {port}").ok();
            writeln!(out, "    frequency = {}", iface.frequency).ok();
            writeln!(out, "    bandwidth = {}", iface.bandwidth).ok();
            writeln!(out, "    spreadingfactor = {}", iface.spreading_factor).ok();
            writeln!(out, "    codingrate = {}", iface.coding_rate).ok();
            writeln!(out, "    txpower = {}", iface.tx_power).ok();
            writeln!(out, "    ingress_control = false").ok();
            // Codeberg #55: pin the regulatory duty-cycle gate off for tests
            // (see the single-interface note above).
            if node.node_type == "rust" {
                writeln!(out, "    duty_cycle = off").ok();
            }
        }
    }

    if let Some(ref port) = node.serial_path {
        writeln!(out, "  [[Serial LNode]]").ok();
        writeln!(out, "    type = SerialInterface").ok();
        writeln!(out, "    enabled = yes").ok();
        writeln!(out, "    port = {port}").ok();
        writeln!(out, "    speed = 115200").ok();
        writeln!(out, "    databits = 8").ok();
        writeln!(out, "    parity = N").ok();
        writeln!(out, "    stopbits = 1").ok();
        if let Some(r) = radio {
            writeln!(out, "    frequency = {}", r.frequency).ok();
            writeln!(out, "    bandwidth = {}", r.bandwidth).ok();
            writeln!(out, "    spreadingfactor = {}", r.spreading_factor).ok();
            writeln!(out, "    codingrate = {}", r.coding_rate).ok();
            writeln!(out, "    txpower = {}", r.tx_power).ok();
            writeln!(
                out,
                "    csma_enabled = {}",
                if r.csma_enabled { "yes" } else { "no" }
            )
            .ok();
            // Codeberg #55: pin the regulatory duty-cycle gate off for tests
            // (see the single-interface note above).
            if node.node_type == "rust" {
                writeln!(out, "    duty_cycle = off").ok();
            }
        }
    }

    if let Some(port) = node.listen_port {
        writeln!(out, "  [[Selftest TCP Server]]").ok();
        writeln!(out, "    type = TCPServerInterface").ok();
        writeln!(out, "    enabled = yes").ok();
        writeln!(out, "    listen_ip = 0.0.0.0").ok();
        writeln!(out, "    listen_port = {port}").ok();
        writeln!(out, "    ingress_control = false").ok();
    }

    out
}

/// Generate node config files and identity files into `base_dir`.
///
/// Creates for each node:
///   - `{base_dir}/{node_name}/storage/transport_identity` (64 random bytes)
///   - `{base_dir}/{node_name}/config` (Reticulum INI format)
pub fn generate_node_configs(scenario: &TestScenario, base_dir: &Path) -> io::Result<()> {
    let interfaces = assign_interfaces(&scenario.links, &scenario.test.name)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    // Validate: if any node has rnode set, [radio] must be present.
    let has_rnode = scenario.nodes.values().any(|n| n.rnode);
    if has_rnode && scenario.radio.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "nodes with rnode require a [radio] section",
        ));
    }

    // Validate: rnode and rnode_interfaces are mutually exclusive.
    for (name, node) in &scenario.nodes {
        if node.rnode && node.rnode_interfaces.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("node '{name}': cannot have both rnode and rnode_interfaces"),
            ));
        }
    }

    // Validate: rnode_proxy requires single rnode (not rnode_interfaces).
    for (name, node) in &scenario.nodes {
        if node.rnode_proxy && !node.rnode {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("node '{name}': rnode_proxy requires rnode"),
            ));
        }
        if node.rnode_proxy && node.rnode_interfaces.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("node '{name}': rnode_proxy is not supported with rnode_interfaces"),
            ));
        }
    }

    // Validate: listen_port must not collide with link-based TCP server ports.
    for (name, node) in &scenario.nodes {
        if let Some(listen_port) = node.listen_port {
            if let Some(ifaces) = interfaces.get(name.as_str()) {
                for iface in ifaces {
                    if let InterfaceEntry::TcpServer { port, .. } = iface {
                        if *port == listen_port {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!(
                                    "node '{name}': listen_port {listen_port} conflicts with \
                                     link-based TCP server on the same port"
                                ),
                            ));
                        }
                    }
                }
            }
        }
    }

    let mut rng = rand::thread_rng();

    for (name, node) in &scenario.nodes {
        let node_dir = base_dir.join(name);
        let storage_dir = node_dir.join("storage");
        fs::create_dir_all(&storage_dir)?;

        // Generate 64-byte identity file.
        let mut identity = [0u8; 64];
        rng.fill_bytes(&mut identity);
        fs::write(storage_dir.join("transport_identity"), identity)?;

        // Render and write config.
        let ifaces = interfaces.get(name.as_str()).cloned().unwrap_or_default();
        let config = render_config(node, &ifaces, scenario.radio.as_ref());
        fs::write(node_dir.join("config"), config)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn load_basic_probe() -> TestScenario {
        let toml_str = fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/basic_probe.toml"
        ))
        .expect("basic_probe.toml not found");
        parse_scenario(&toml_str).expect("parse failed")
    }

    #[test]
    fn parse_basic_probe() {
        let scenario = load_basic_probe();
        assert_eq!(scenario.test.name, "basic_probe");
        assert_eq!(scenario.test.timeout_secs, 60);
        assert_eq!(scenario.nodes.len(), 2);
        assert!(scenario.nodes.contains_key("alice"));
        assert!(scenario.nodes.contains_key("bob"));
        assert_eq!(scenario.nodes["alice"].node_type, "rust");
        assert_eq!(scenario.nodes["bob"].node_type, "python");
        assert!(scenario.nodes["alice"].respond_to_probes);
        assert!(scenario.nodes["bob"].respond_to_probes);
        assert!(scenario.nodes["alice"].enable_transport);
        assert_eq!(scenario.links.len(), 1);
        assert_eq!(scenario.links["alice-bob"].link_type(), "tcp");
        assert_eq!(scenario.steps.len(), 2);
    }

    #[test]
    fn parse_steps_variants() {
        let scenario = load_basic_probe();

        match &scenario.steps[0] {
            Step::WaitForPath {
                on,
                destination,
                timeout_secs,
                expect_result,
            } => {
                assert_eq!(on, "alice");
                assert_eq!(destination, "bob.probe");
                assert_eq!(*timeout_secs, 30);
                assert_eq!(expect_result, "success");
            }
            other => panic!("expected WaitForPath, got: {other:?}"),
        }

        match &scenario.steps[1] {
            Step::RnProbe {
                from,
                to,
                expect_hops,
                expect_result,
                ..
            } => {
                assert_eq!(from, "alice");
                assert_eq!(to, "bob.probe");
                assert_eq!(*expect_hops, Some(1));
                assert_eq!(expect_result, "success");
            }
            other => panic!("expected RnProbe, got: {other:?}"),
        }
    }

    #[test]
    fn generate_configs_creates_correct_structure() {
        let scenario = load_basic_probe();
        let tmp = TempDir::new().unwrap();

        generate_node_configs(&scenario, tmp.path()).unwrap();

        // Check directory structure.
        assert!(tmp.path().join("alice/storage/transport_identity").exists());
        assert!(tmp.path().join("alice/config").exists());
        assert!(tmp.path().join("bob/storage/transport_identity").exists());
        assert!(tmp.path().join("bob/config").exists());
    }

    #[test]
    fn identity_files_are_64_bytes() {
        let scenario = load_basic_probe();
        let tmp = TempDir::new().unwrap();

        generate_node_configs(&scenario, tmp.path()).unwrap();

        let alice_id = fs::read(tmp.path().join("alice/storage/transport_identity")).unwrap();
        let bob_id = fs::read(tmp.path().join("bob/storage/transport_identity")).unwrap();
        assert_eq!(alice_id.len(), 64);
        assert_eq!(bob_id.len(), 64);
        // Identities should differ (random).
        assert_ne!(alice_id, bob_id);
    }

    #[test]
    fn alice_config_has_tcp_server() {
        let scenario = load_basic_probe();
        let tmp = TempDir::new().unwrap();

        generate_node_configs(&scenario, tmp.path()).unwrap();

        let config = fs::read_to_string(tmp.path().join("alice/config")).unwrap();
        assert!(
            config.contains("TCPServerInterface"),
            "alice should have TCPServerInterface"
        );
        assert!(config.contains("listen_port = 4242"));
        assert!(config.contains("listen_ip = 0.0.0.0"));
        assert!(
            !config.contains("TCPClientInterface"),
            "alice should NOT have TCPClientInterface"
        );
    }

    #[test]
    fn bob_config_has_tcp_client() {
        let scenario = load_basic_probe();
        let tmp = TempDir::new().unwrap();

        generate_node_configs(&scenario, tmp.path()).unwrap();

        let config = fs::read_to_string(tmp.path().join("bob/config")).unwrap();
        assert!(
            config.contains("TCPClientInterface"),
            "bob should have TCPClientInterface"
        );
        assert!(config.contains("target_host = alice"));
        assert!(config.contains("target_port = 4242"));
        assert!(
            !config.contains("TCPServerInterface"),
            "bob should NOT have TCPServerInterface"
        );
    }

    #[test]
    fn config_contains_transport_and_probe_settings() {
        let scenario = load_basic_probe();
        let tmp = TempDir::new().unwrap();

        generate_node_configs(&scenario, tmp.path()).unwrap();

        for node_name in ["alice", "bob"] {
            let config =
                fs::read_to_string(tmp.path().join(format!("{node_name}/config"))).unwrap();
            assert!(
                config.contains("enable_transport = yes"),
                "{node_name}: missing enable_transport"
            );
            assert!(
                config.contains("share_instance = yes"),
                "{node_name}: missing share_instance"
            );
            assert!(
                config.contains("respond_to_probes = yes"),
                "{node_name}: missing respond_to_probes"
            );
            assert!(
                config.contains("loglevel = 5"),
                "{node_name}: missing loglevel"
            );
        }
    }

    #[test]
    fn relay_topology_three_nodes() {
        let toml_str = r#"
[test]
name = "relay_test"
description = "A through R to B"
timeout_secs = 90

[nodes.alice]
type = "rust"
respond_to_probes = true

[nodes.relay]
type = "python"
respond_to_probes = false

[nodes.bob]
type = "rust"
respond_to_probes = true

[links]
alice-relay = "tcp"
bob-relay = "tcp"

[[steps]]
action = "sleep"
duration_secs = 5
"#;
        let scenario = parse_scenario(toml_str).unwrap();
        let tmp = TempDir::new().unwrap();

        generate_node_configs(&scenario, tmp.path()).unwrap();

        // alice-relay: alice < relay alphabetically → alice=server, relay=client
        let alice_cfg = fs::read_to_string(tmp.path().join("alice/config")).unwrap();
        assert!(alice_cfg.contains("TCPServerInterface-relay"));
        assert!(alice_cfg.contains("listen_port = 4242"));

        // bob-relay: bob < relay alphabetically → bob=server, relay=client
        let bob_cfg = fs::read_to_string(tmp.path().join("bob/config")).unwrap();
        assert!(bob_cfg.contains("TCPServerInterface-relay"));
        assert!(bob_cfg.contains("listen_port = 4242"));

        // relay should have two TCPClientInterfaces
        let relay_cfg = fs::read_to_string(tmp.path().join("relay/config")).unwrap();
        assert!(relay_cfg.contains("TCPClientInterface-alice"));
        assert!(relay_cfg.contains("target_host = alice"));
        assert!(relay_cfg.contains("TCPClientInterface-bob"));
        assert!(relay_cfg.contains("target_host = bob"));
        assert!(relay_cfg.contains("respond_to_probes = no"));
        assert!(!relay_cfg.contains("TCPServerInterface"));
    }

    #[test]
    fn multiple_links_same_server_increments_port() {
        let toml_str = r#"
[test]
name = "hub_spoke"

[nodes.hub]
type = "rust"
respond_to_probes = true

[nodes.spoke1]
type = "python"
respond_to_probes = true

[nodes.spoke2]
type = "python"
respond_to_probes = true

[links]
hub-spoke1 = "tcp"
hub-spoke2 = "tcp"
"#;
        let scenario = parse_scenario(toml_str).unwrap();
        let interfaces = assign_interfaces(&scenario.links, &scenario.test.name).unwrap();

        // hub is alphabetically first in both links → hub is server for both
        let hub_ifaces = &interfaces["hub"];
        assert_eq!(hub_ifaces.len(), 2);

        let ports: Vec<u16> = hub_ifaces
            .iter()
            .map(|i| match i {
                InterfaceEntry::TcpServer { port, .. } => *port,
                other => panic!("expected TcpServer, got: {other:?}"),
            })
            .collect();

        assert!(ports.contains(&4242));
        assert!(ports.contains(&4243));
    }

    #[test]
    fn unsupported_link_type_errors() {
        let mut links = BTreeMap::new();
        links.insert("alice-bob".to_string(), LinkDef::Simple("udp".into()));
        let result = assign_interfaces(&links, "test");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unsupported link type"));
    }

    #[test]
    fn malformed_link_key_errors() {
        let mut links = BTreeMap::new();
        links.insert("alice".to_string(), LinkDef::Simple("tcp".into()));
        let result = assign_interfaces(&links, "test");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("nodeA-nodeB"));
    }

    #[test]
    fn parse_radio_config() {
        let toml_str = r#"
[test]
name = "radio_test"

[radio]
frequency = 868000000
bandwidth = 125000
sf = 7
cr = 5
txpower = 17

[nodes.alpha]
type = "rust"
respond_to_probes = true
rnode = true
"#;
        let scenario = parse_scenario(toml_str).unwrap();
        let radio = scenario.radio.as_ref().expect("radio should be present");
        assert_eq!(radio.frequency, 868000000);
        assert_eq!(radio.bandwidth, 125000);
        assert_eq!(radio.spreading_factor, 7);
        assert_eq!(radio.coding_rate, 5);
        assert_eq!(radio.tx_power, 17);
        assert!(scenario.nodes["alpha"].rnode);
    }

    #[test]
    fn rnode_config_generates_ini() {
        let node = NodeDef {
            node_type: "rust".into(),
            respond_to_probes: true,
            enable_transport: true,
            rnode: true,
            rnode_proxy: false,
            listen_port: None,
            rnode_interfaces: None,
            serial: false,
            rnode_path: Some("/dev/ttyACM0".into()),
            serial_path: None,
            debug_serial_path: None,
        };
        let radio = RadioConfig {
            frequency: 868000000,
            bandwidth: 125000,
            spreading_factor: 7,
            coding_rate: 5,
            tx_power: 17,
            csma_enabled: false,
        };
        let config = render_config(&node, &[], Some(&radio));
        assert!(
            config.contains("[[RNode Interface]]"),
            "missing RNode section"
        );
        assert!(config.contains("type = RNodeInterface"));
        assert!(config.contains("enabled = yes"));
        assert!(config.contains("port = /dev/ttyACM0"));
        assert!(config.contains("frequency = 868000000"));
        assert!(config.contains("bandwidth = 125000"));
        assert!(config.contains("spreadingfactor = 7"));
        assert!(config.contains("codingrate = 5"));
        assert!(config.contains("txpower = 17"));
        assert!(config.contains("ingress_control = false"));
        // Codeberg #55: the RNode block pins the regulatory duty-cycle gate off
        // so the lawful-by-default frequency-aware policy does not throttle the
        // test at 869.525 MHz (ETSI P sub-band, 10%).
        assert!(
            config.contains("duty_cycle = off"),
            "rust RNode config must pin duty_cycle off for tests"
        );
    }

    #[test]
    fn python_rnode_config_omits_duty_cycle() {
        // Python peers run RNS, which has no duty-cycle gate; do not inject the
        // key into their config. Only our own rust nodes get `duty_cycle = off`.
        let node = NodeDef {
            node_type: "python".into(),
            respond_to_probes: true,
            enable_transport: true,
            rnode: true,
            rnode_proxy: false,
            listen_port: None,
            rnode_interfaces: None,
            serial: false,
            rnode_path: Some("/dev/ttyACM0".into()),
            serial_path: None,
            debug_serial_path: None,
        };
        let radio = RadioConfig {
            frequency: 869525000,
            bandwidth: 500000,
            spreading_factor: 7,
            coding_rate: 5,
            tx_power: 17,
            csma_enabled: false,
        };
        let config = render_config(&node, &[], Some(&radio));
        assert!(config.contains("[[RNode Interface]]"));
        assert!(
            !config.contains("duty_cycle"),
            "python config must not carry a duty_cycle key"
        );
    }

    #[test]
    fn rnode_without_radio_section_errors() {
        let toml_str = r#"
[test]
name = "missing_radio"

[nodes.alpha]
type = "rust"
respond_to_probes = true
rnode = true
"#;
        let scenario = parse_scenario(toml_str).unwrap();
        let tmp = TempDir::new().unwrap();
        let result = generate_node_configs(&scenario, tmp.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("rnode require a [radio] section"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn existing_tcp_only_unchanged() {
        // Verify that radio: None doesn't affect existing TCP-only scenarios.
        let scenario = load_basic_probe();
        assert!(scenario.radio.is_none());
        assert!(!scenario.nodes["alice"].rnode);

        let tmp = TempDir::new().unwrap();
        generate_node_configs(&scenario, tmp.path()).unwrap();

        let alice_cfg = fs::read_to_string(tmp.path().join("alice/config")).unwrap();
        assert!(alice_cfg.contains("TCPServerInterface"));
        assert!(!alice_cfg.contains("RNodeInterface"));

        let bob_cfg = fs::read_to_string(tmp.path().join("bob/config")).unwrap();
        assert!(bob_cfg.contains("TCPClientInterface"));
        assert!(!bob_cfg.contains("RNodeInterface"));
    }

    #[test]
    fn listen_port_generates_tcp_server() {
        let node = NodeDef {
            node_type: "rust".into(),
            respond_to_probes: true,
            enable_transport: true,
            rnode: false,
            rnode_proxy: false,
            listen_port: Some(4242),
            rnode_interfaces: None,
            serial: false,
            rnode_path: None,
            serial_path: None,
            debug_serial_path: None,
        };
        let config = render_config(&node, &[], None);
        assert!(
            config.contains("[[Selftest TCP Server]]"),
            "missing Selftest TCP Server section"
        );
        assert!(config.contains("type = TCPServerInterface"));
        assert!(config.contains("listen_ip = 0.0.0.0"));
        assert!(config.contains("listen_port = 4242"));
        assert!(config.contains("enabled = yes"));
        assert!(config.contains("ingress_control = false"));
    }

    #[test]
    fn listen_port_coexists_with_rnode() {
        let node = NodeDef {
            node_type: "rust".into(),
            respond_to_probes: true,
            enable_transport: true,
            rnode: true,
            rnode_proxy: false,
            listen_port: Some(4242),
            rnode_interfaces: None,
            serial: false,
            rnode_path: Some("/dev/ttyACM0".into()),
            serial_path: None,
            debug_serial_path: None,
        };
        let radio = RadioConfig {
            frequency: 868000000,
            bandwidth: 125000,
            spreading_factor: 7,
            coding_rate: 5,
            tx_power: 17,
            csma_enabled: false,
        };
        let config = render_config(&node, &[], Some(&radio));
        assert!(
            config.contains("[[RNode Interface]]"),
            "missing RNode section"
        );
        assert!(
            config.contains("[[Selftest TCP Server]]"),
            "missing Selftest TCP Server section"
        );
    }

    #[test]
    fn listen_port_collides_with_link_port() {
        let toml_str = r#"
[test]
name = "port_collision"

[nodes.alpha]
type = "rust"
respond_to_probes = true
listen_port = 4242

[nodes.beta]
type = "rust"
respond_to_probes = true

[links]
alpha-beta = "tcp"
"#;
        let scenario = parse_scenario(toml_str).unwrap();
        let tmp = TempDir::new().unwrap();
        let result = generate_node_configs(&scenario, tmp.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("listen_port 4242 conflicts"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn listen_port_no_collision_different_port() {
        let toml_str = r#"
[test]
name = "no_collision"

[nodes.alpha]
type = "rust"
respond_to_probes = true
listen_port = 5555

[nodes.beta]
type = "rust"
respond_to_probes = true

[links]
alpha-beta = "tcp"
"#;
        let scenario = parse_scenario(toml_str).unwrap();
        let tmp = TempDir::new().unwrap();
        // Should succeed, listen_port 5555 != link port 4242.
        generate_node_configs(&scenario, tmp.path()).unwrap();
    }

    #[test]
    fn listen_port_none_no_section() {
        let node = NodeDef {
            node_type: "rust".into(),
            respond_to_probes: true,
            enable_transport: true,
            rnode: false,
            rnode_proxy: false,
            listen_port: None,
            rnode_interfaces: None,
            serial: false,
            rnode_path: None,
            serial_path: None,
            debug_serial_path: None,
        };
        let config = render_config(&node, &[], None);
        assert!(
            !config.contains("Selftest TCP Server"),
            "should not have Selftest TCP Server section"
        );
    }

    #[test]
    fn parse_link_simple_string() {
        let toml_str = r#"
[test]
name = "simple_link"

[nodes.alice]
type = "rust"
respond_to_probes = true

[nodes.bob]
type = "python"
respond_to_probes = true

[links]
alice-bob = "tcp"
"#;
        let scenario = parse_scenario(toml_str).unwrap();
        assert_eq!(scenario.links["alice-bob"], LinkDef::Simple("tcp".into()));
        assert_eq!(scenario.links["alice-bob"].link_type(), "tcp");
        assert!(scenario.links["alice-bob"].networkname().is_none());
        assert!(scenario.links["alice-bob"].passphrase().is_none());
    }

    #[test]
    fn parse_link_detailed_with_ifac() {
        let toml_str = r#"
[test]
name = "ifac_link"

[nodes.alice]
type = "rust"
respond_to_probes = true

[nodes.bob]
type = "python"
respond_to_probes = true

[links.alice-bob]
type = "tcp"
networkname = "testnet"
passphrase = "secret123"
ifac_size = 128
"#;
        let scenario = parse_scenario(toml_str).unwrap();
        let link = &scenario.links["alice-bob"];
        assert_eq!(link.link_type(), "tcp");
        assert_eq!(link.networkname(), Some("testnet"));
        assert_eq!(link.passphrase(), Some("secret123"));
        assert_eq!(link.ifac_size(), Some(128));
    }

    #[test]
    fn parse_link_detailed_without_ifac() {
        let toml_str = r#"
[test]
name = "no_ifac_link"

[nodes.alice]
type = "rust"
respond_to_probes = true

[nodes.bob]
type = "python"
respond_to_probes = true

[links.alice-bob]
type = "tcp"
"#;
        let scenario = parse_scenario(toml_str).unwrap();
        let link = &scenario.links["alice-bob"];
        assert_eq!(link.link_type(), "tcp");
        assert!(link.networkname().is_none());
        assert!(link.passphrase().is_none());
    }

    #[test]
    fn mixed_simple_and_detailed_links() {
        let toml_str = r#"
[test]
name = "mixed_links"

[nodes.alice]
type = "rust"
respond_to_probes = true

[nodes.bob]
type = "python"
respond_to_probes = true

[nodes.relay]
type = "python"
respond_to_probes = false

[links]
alice-relay = "tcp"

[links.bob-relay]
type = "tcp"
networkname = "securenet"
passphrase = "secret456"
"#;
        let scenario = parse_scenario(toml_str).unwrap();
        assert_eq!(scenario.links.len(), 2);
        assert_eq!(scenario.links["alice-relay"].link_type(), "tcp");
        assert!(scenario.links["alice-relay"].networkname().is_none());
        assert_eq!(scenario.links["bob-relay"].networkname(), Some("securenet"));
        assert_eq!(scenario.links["bob-relay"].passphrase(), Some("secret456"));
    }

    #[test]
    fn ifac_config_in_rendered_ini() {
        let node = NodeDef {
            node_type: "rust".into(),
            respond_to_probes: true,
            enable_transport: true,
            rnode: false,
            rnode_proxy: false,
            listen_port: None,
            rnode_interfaces: None,
            serial: false,
            rnode_path: None,
            serial_path: None,
            debug_serial_path: None,
        };
        let ifaces = vec![InterfaceEntry::TcpServer {
            peer: "bob".into(),
            port: 4242,
            ifac: IfacParams {
                networkname: Some("testnet".into()),
                passphrase: Some("secret".into()),
                ifac_size: Some(128),
            },
        }];
        let config = render_config(&node, &ifaces, None);
        assert!(
            config.contains("networkname = testnet"),
            "missing networkname"
        );
        assert!(config.contains("passphrase = secret"), "missing passphrase");
        assert!(config.contains("ifac_size = 128"), "missing ifac_size");
    }

    #[test]
    fn no_ifac_config_in_rendered_ini() {
        let node = NodeDef {
            node_type: "rust".into(),
            respond_to_probes: true,
            enable_transport: true,
            rnode: false,
            rnode_proxy: false,
            listen_port: None,
            rnode_interfaces: None,
            serial: false,
            rnode_path: None,
            serial_path: None,
            debug_serial_path: None,
        };
        let ifaces = vec![InterfaceEntry::TcpServer {
            peer: "bob".into(),
            port: 4242,
            ifac: IfacParams::default(),
        }];
        let config = render_config(&node, &ifaces, None);
        assert!(
            !config.contains("networkname"),
            "should not have networkname"
        );
        assert!(!config.contains("passphrase"), "should not have passphrase");
        assert!(!config.contains("ifac_size"), "should not have ifac_size");
    }

    #[test]
    fn assign_interfaces_propagates_ifac() {
        let mut links = BTreeMap::new();
        links.insert(
            "alice-bob".to_string(),
            LinkDef::Detailed {
                link_type: "tcp".into(),
                networkname: Some("net1".into()),
                passphrase: Some("pass1".into()),
                ifac_size: Some(64),
                group_id: None,
            },
        );
        let interfaces = assign_interfaces(&links, "test").unwrap();
        let alice_ifaces = &interfaces["alice"];
        let bob_ifaces = &interfaces["bob"];

        match &alice_ifaces[0] {
            InterfaceEntry::TcpServer { ifac, .. } => {
                assert_eq!(ifac.networkname.as_deref(), Some("net1"));
                assert_eq!(ifac.passphrase.as_deref(), Some("pass1"));
                assert_eq!(ifac.ifac_size, Some(64));
            }
            other => panic!("expected TcpServer, got: {other:?}"),
        }

        match &bob_ifaces[0] {
            InterfaceEntry::TcpClient { ifac, .. } => {
                assert_eq!(ifac.networkname.as_deref(), Some("net1"));
                assert_eq!(ifac.passphrase.as_deref(), Some("pass1"));
                assert_eq!(ifac.ifac_size, Some(64));
            }
            other => panic!("expected TcpClient, got: {other:?}"),
        }
    }

    #[test]
    fn ifac_end_to_end_config_generation() {
        let toml_str = r#"
[test]
name = "ifac_e2e"

[nodes.alice]
type = "rust"
respond_to_probes = true

[nodes.bob]
type = "python"
respond_to_probes = true

[links.alice-bob]
type = "tcp"
networkname = "mynet"
passphrase = "mypass"
"#;
        let scenario = parse_scenario(toml_str).unwrap();
        let tmp = TempDir::new().unwrap();
        generate_node_configs(&scenario, tmp.path()).unwrap();

        let alice_cfg = fs::read_to_string(tmp.path().join("alice/config")).unwrap();
        assert!(
            alice_cfg.contains("networkname = mynet"),
            "alice missing networkname"
        );
        assert!(
            alice_cfg.contains("passphrase = mypass"),
            "alice missing passphrase"
        );

        let bob_cfg = fs::read_to_string(tmp.path().join("bob/config")).unwrap();
        assert!(
            bob_cfg.contains("networkname = mynet"),
            "bob missing networkname"
        );
        assert!(
            bob_cfg.contains("passphrase = mypass"),
            "bob missing passphrase"
        );
    }

    #[test]
    fn parse_file_transfer_step() {
        let toml_str = fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/lncp_baseline.toml"
        ))
        .expect("lncp_baseline.toml not found");
        let scenario = parse_scenario(&toml_str).expect("parse failed");
        assert_eq!(scenario.test.name, "lncp_baseline");
        assert_eq!(scenario.nodes.len(), 3);
        assert_eq!(scenario.steps.len(), 3);
        // Verify wait_for_path step has the expected timeout
        match &scenario.steps[1] {
            Step::WaitForPath { timeout_secs, .. } => assert_eq!(*timeout_secs, 60),
            other => panic!("expected WaitForPath, got: {other:?}"),
        }

        match &scenario.steps[2] {
            Step::FileTransfer {
                sender,
                receiver,
                sender_tool,
                receiver_tool,
                file_sizes,
                direction,
                repeats,
                timeout_secs,
                mode,
                receiver_flags,
                sender_flags,
                auth_from,
                expect_result,
                fetch_path,
            } => {
                assert_eq!(sender, "alpha");
                assert_eq!(receiver, "charlie");
                assert_eq!(sender_tool, "rncp");
                assert_eq!(receiver_tool, "rncp");
                assert_eq!(file_sizes, &[102400, 1048576]);
                assert_eq!(direction, "a_to_b");
                assert_eq!(*repeats, 3);
                assert_eq!(*timeout_secs, 300);
                // Defaults for new fields
                assert_eq!(mode, "push");
                assert_eq!(receiver_flags, "");
                assert_eq!(sender_flags, "");
                assert_eq!(auth_from, "");
                assert_eq!(expect_result, "success");
                assert_eq!(fetch_path, "");
            }
            other => panic!("expected FileTransfer, got: {other:?}"),
        }
    }

    #[test]
    fn parse_file_transfer_defaults() {
        let toml_str = r#"
[test]
name = "ft_defaults"

[nodes.alice]
type = "rust"
respond_to_probes = true

[nodes.bob]
type = "python"
respond_to_probes = true

[links]
alice-bob = "tcp"

[[steps]]
action = "file_transfer"
sender = "alice"
receiver = "bob"
sender_tool = "lncp"
receiver_tool = "rncp"
file_sizes = [1024]
"#;
        let scenario = parse_scenario(toml_str).unwrap();
        match &scenario.steps[0] {
            Step::FileTransfer {
                direction,
                repeats,
                timeout_secs,
                ..
            } => {
                assert_eq!(direction, "both");
                assert_eq!(*repeats, 3);
                assert_eq!(*timeout_secs, 300);
            }
            other => panic!("expected FileTransfer, got: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // AutoInterface link tests
    // -----------------------------------------------------------------------

    #[test]
    fn auto_link_simple_form_assigns_default_group_id() {
        let mut links = BTreeMap::new();
        links.insert("alice-bob".to_string(), LinkDef::Simple("auto".into()));
        let interfaces = assign_interfaces(&links, "mytest").unwrap();

        for node in ["alice", "bob"] {
            let entries = &interfaces[node];
            assert_eq!(entries.len(), 1, "{node}: expected one Auto entry");
            match &entries[0] {
                InterfaceEntry::Auto { group_id } => {
                    assert_eq!(group_id, "test_mytest");
                }
                other => panic!("{node}: expected Auto, got {other:?}"),
            }
        }
    }

    #[test]
    fn auto_link_detailed_form_with_explicit_group_id() {
        let mut links = BTreeMap::new();
        links.insert(
            "alice-bob".to_string(),
            LinkDef::Detailed {
                link_type: "auto".into(),
                networkname: None,
                passphrase: None,
                ifac_size: None,
                group_id: Some("custom_group".into()),
            },
        );
        let interfaces = assign_interfaces(&links, "mytest").unwrap();
        match &interfaces["alice"][0] {
            InterfaceEntry::Auto { group_id } => assert_eq!(group_id, "custom_group"),
            other => panic!("expected Auto, got {other:?}"),
        }
    }

    #[test]
    fn auto_link_three_nodes_share_one_group() {
        let mut links = BTreeMap::new();
        links.insert("alice-bob".to_string(), LinkDef::Simple("auto".into()));
        links.insert("bob-charlie".to_string(), LinkDef::Simple("auto".into()));
        let interfaces = assign_interfaces(&links, "tri").unwrap();

        // Each of the three nodes should have exactly one [[Auto]] entry,
        // all with the same group_id (no duplication for shared nodes).
        for node in ["alice", "bob", "charlie"] {
            let entries = &interfaces[node];
            assert_eq!(entries.len(), 1, "{node}: expected one Auto entry");
            match &entries[0] {
                InterfaceEntry::Auto { group_id } => assert_eq!(group_id, "test_tri"),
                other => panic!("{node}: expected Auto, got {other:?}"),
            }
        }
    }

    #[test]
    fn auto_and_tcp_links_can_coexist() {
        let mut links = BTreeMap::new();
        links.insert("alice-bob".to_string(), LinkDef::Simple("auto".into()));
        links.insert("alice-charlie".to_string(), LinkDef::Simple("tcp".into()));
        let interfaces = assign_interfaces(&links, "mixed").unwrap();

        // alice gets one Auto + one TcpServer (alphabetically first in alice-charlie).
        let alice = &interfaces["alice"];
        assert_eq!(alice.len(), 2);
        let has_auto = alice
            .iter()
            .any(|e| matches!(e, InterfaceEntry::Auto { .. }));
        let has_tcp_server = alice
            .iter()
            .any(|e| matches!(e, InterfaceEntry::TcpServer { .. }));
        assert!(has_auto && has_tcp_server);

        // bob gets only Auto.
        assert!(matches!(interfaces["bob"][0], InterfaceEntry::Auto { .. }));
        // charlie gets only TcpClient.
        assert!(matches!(
            interfaces["charlie"][0],
            InterfaceEntry::TcpClient { .. }
        ));
    }

    #[test]
    fn render_config_emits_auto_block() {
        let node = NodeDef {
            node_type: "rust".into(),
            respond_to_probes: false,
            enable_transport: true,
            rnode: false,
            rnode_proxy: false,
            listen_port: None,
            rnode_interfaces: None,
            serial: false,
            rnode_path: None,
            serial_path: None,
            debug_serial_path: None,
        };
        let entries = vec![InterfaceEntry::Auto {
            group_id: "test_foo".into(),
        }];
        let ini = render_config(&node, &entries, None);
        assert!(ini.contains("[[Auto-test_foo]]"));
        assert!(ini.contains("type = AutoInterface"));
        assert!(ini.contains("group_id = test_foo"));
        assert!(ini.contains("discovery_scope = link"));
    }
}
