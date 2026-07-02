//! Configuration loading and management

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Main Reticulum configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Core reticulum settings
    #[serde(default)]
    pub reticulum: ReticulumConfig,
    /// Interface configurations by name
    #[serde(default)]
    pub interfaces: HashMap<String, InterfaceConfig>,
}

/// Core reticulum settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReticulumConfig {
    /// Enable transport mode (routing for others)
    ///
    /// Defaults to `true`, daemon use enables transport.
    /// Python Reticulum defaults to `false` (library use), but lnsd is a daemon.
    #[serde(default = "default_true")]
    pub enable_transport: bool,
    /// Use implicit proof for link identification
    #[serde(default = "default_true")]
    pub use_implicit_proof: bool,
    /// Allow sharing instance across processes via local Unix socket.
    /// When enabled, the daemon listens on `\0rns/{instance_name}` for local clients.
    #[serde(default)]
    pub shared_instance: bool,
    /// Instance name for the shared instance socket (default: "default").
    /// The abstract socket path will be `\0rns/{instance_name}`.
    #[serde(default = "default_instance_name")]
    pub instance_name: String,
    /// Shared-instance transport type from the RNS config (`unix` or `tcp`).
    ///
    /// `None` means unset (Python defaults to AF_UNIX where available).
    /// Parsed for rnsd drop-in compatibility; lnsd currently only serves
    /// the abstract AF_UNIX socket keyed by `instance_name`. When set to
    /// `tcp` it overrides (clears) `shared_instance_socket` per RNS 1.3.x.
    #[serde(default)]
    pub shared_instance_type: Option<String>,
    /// Explicit AF_UNIX socket path for the shared instance (RNS 1.3.x).
    ///
    /// Parsed for rnsd config compatibility. Ignored (cleared) when
    /// `shared_instance_type = tcp`, since tcp disables AF_UNIX upstream.
    #[serde(default)]
    pub shared_instance_socket: Option<String>,
    /// Respond to rnprobe requests
    ///
    /// When enabled, creates a probe destination (`rnstransport.probe`) with
    /// `ProofStrategy::All`, so the node automatically sends a signed proof
    /// for every incoming probe packet.
    #[serde(default)]
    pub respond_to_probes: bool,
    /// Enable remote management
    #[serde(default)]
    pub remote_management_enabled: bool,
    /// Storage path (relative to config dir or absolute)
    #[serde(default)]
    pub storage_path: Option<PathBuf>,
    /// Interval between periodic storage flushes (seconds).
    ///
    /// Crash protection only, normal shutdown flushes via the signal
    /// handler. Battery-powered or SD-card deployments may want a
    /// different interval. Default: 3600 (one hour).
    #[serde(default = "default_flush_interval_secs")]
    pub flush_interval_secs: u64,
    /// Capacity of the lossless control-plane event channel (Codeberg #71).
    ///
    /// Control events (announces, paths, link/resource lifecycle) are
    /// delivered losslessly until this bounded channel fills, after which
    /// drops are counted and surfaced via `NodeEvent::ControlPlaneOverflow`.
    /// The default is conservative for small std platforms; servers under
    /// heavy announce load should raise it. Library default, platform tunes.
    #[serde(default = "default_control_channel_capacity")]
    pub control_channel_capacity: usize,
    /// Capacity of the droppable data-plane event channel (Codeberg #71).
    ///
    /// Data events (single-packet delivery and its confirmations) drop
    /// silently when this bounded channel is full — normal backpressure.
    /// Kept conservative so a slow consumer cannot grow driver memory on
    /// small std platforms; servers may raise it.
    #[serde(default = "default_data_channel_capacity")]
    pub data_channel_capacity: usize,
    /// Optional link keepalive interval override in seconds.
    ///
    /// When set, every link uses this interval instead of the RTT-derived
    /// default, and the stale-link timeout scales with it. Useful for slow
    /// links. Leviculum tuning extension: local timing only, no wire or
    /// semantic change. `None` (default) keeps RTT-driven behaviour.
    #[serde(default)]
    pub keepalive_interval: Option<u64>,
}

/// Default interval between periodic storage flushes (seconds)
pub const DEFAULT_FLUSH_INTERVAL_SECS: u64 = 3600;

/// Default capacity of the lossless control-plane event channel.
///
/// Conservative figure safe for small std platforms. Matches the previously
/// shipped single-channel capacity so existing control-plane headroom is
/// preserved; servers override larger via config or builder.
pub const DEFAULT_CONTROL_CHANNEL_CAPACITY: usize = 256;

/// Default capacity of the droppable data-plane event channel.
///
/// Half the control default: data drops are normal backpressure, so the
/// data plane is sized to bound memory rather than to avoid drops.
pub const DEFAULT_DATA_CHANNEL_CAPACITY: usize = 128;

fn default_flush_interval_secs() -> u64 {
    DEFAULT_FLUSH_INTERVAL_SECS
}

fn default_control_channel_capacity() -> usize {
    DEFAULT_CONTROL_CHANNEL_CAPACITY
}

fn default_data_channel_capacity() -> usize {
    DEFAULT_DATA_CHANNEL_CAPACITY
}

fn default_true() -> bool {
    true
}

fn default_instance_name() -> String {
    "default".to_string()
}

impl Default for ReticulumConfig {
    fn default() -> Self {
        Self {
            enable_transport: true,
            use_implicit_proof: true,
            shared_instance: false,
            instance_name: default_instance_name(),
            shared_instance_type: None,
            shared_instance_socket: None,
            respond_to_probes: false,
            remote_management_enabled: false,
            storage_path: None,
            flush_interval_secs: DEFAULT_FLUSH_INTERVAL_SECS,
            control_channel_capacity: DEFAULT_CONTROL_CHANNEL_CAPACITY,
            data_channel_capacity: DEFAULT_DATA_CHANNEL_CAPACITY,
            keepalive_interval: None,
        }
    }
}

/// Interface configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceConfig {
    /// Interface type
    #[serde(rename = "type")]
    pub interface_type: String,
    /// Whether the interface is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Can send outgoing packets
    #[serde(default = "default_true")]
    pub outgoing: bool,
    /// Bitrate in bits per second
    #[serde(default = "default_bitrate")]
    pub bitrate: u64,

    // TCP/UDP specific
    /// Listen IP address
    pub listen_ip: Option<String>,
    /// Listen port
    pub listen_port: Option<u16>,
    /// Target host for client connections
    pub target_host: Option<String>,
    /// Target port for client connections
    pub target_port: Option<u16>,
    /// Forward IP (for UDP broadcast)
    pub forward_ip: Option<String>,
    /// Forward port (for UDP broadcast)
    pub forward_port: Option<u16>,

    // Serial specific
    /// Serial port path
    pub port: Option<String>,
    /// Serial baud rate
    pub speed: Option<u32>,
    /// Data bits
    pub databits: Option<u8>,
    /// Parity (none, even, odd)
    pub parity: Option<String>,
    /// Stop bits
    pub stopbits: Option<u8>,

    // Reconnection / buffer tuning
    /// Channel buffer size for this interface (default: per interface type)
    pub buffer_size: Option<usize>,
    /// Reconnect interval in seconds for client interfaces (default: 5)
    pub reconnect_interval_secs: Option<u64>,
    /// Maximum reconnect attempts before giving up (default: None = unlimited)
    pub max_reconnect_tries: Option<u64>,

    // AutoInterface specific
    /// Group identifier for multicast discovery
    pub group_id: Option<String>,
    /// Multicast discovery scope (link, admin, site, organisation, global)
    pub discovery_scope: Option<String>,
    /// Discovery port (default: 29716)
    pub discovery_port: Option<u16>,
    /// Data port (default: 42671)
    pub data_port: Option<u16>,
    /// Comma-separated whitelist of NIC names
    pub devices: Option<String>,
    /// Comma-separated blacklist of NIC names
    pub ignored_devices: Option<String>,
    /// Enable multicast loopback (for same-machine testing)
    pub multicast_loopback: Option<bool>,

    // Announce-rate limiting (Codeberg #67 Stage 2a). Read + reported only;
    // on-air enforcement is Codeberg #87. Python: Reticulum.py:798-821.
    /// Announce-rate target in seconds (Python `announce_rate_target`).
    pub announce_rate_target: Option<u32>,
    /// Announce-rate penalty in seconds (Python `announce_rate_penalty`).
    pub announce_rate_penalty: Option<u32>,
    /// Announce-rate grace count (Python `announce_rate_grace`).
    pub announce_rate_grace: Option<u32>,

    // IFAC (Interface Access Code)
    /// Network name for IFAC authentication
    pub networkname: Option<String>,
    /// Passphrase for IFAC authentication
    pub passphrase: Option<String>,
    /// IFAC size in bytes (Python config specifies bits, divided by 8 here)
    pub ifac_size: Option<usize>,

    // RNode specific
    /// LoRa frequency in Hz
    pub frequency: Option<u64>,
    /// LoRa bandwidth in Hz
    pub bandwidth: Option<u32>,
    /// LoRa spreading factor
    pub spreading_factor: Option<u8>,
    /// LoRa coding rate
    pub coding_rate: Option<u8>,
    /// TX power in dBm
    pub tx_power: Option<i8>,
    /// Hardware flow control (RNode waits for CMD_READY before next TX)
    pub flow_control: Option<bool>,
    /// Short-term airtime limit as percent (0.0-100.0)
    pub airtime_limit_short: Option<f64>,
    /// Long-term airtime limit as percent (0.0-100.0)
    pub airtime_limit_long: Option<f64>,
    /// Enable CSMA/CA on the T114 LoRa interface (requires CAD-capable firmware).
    pub csma_enabled: Option<bool>,
}

/// Default interface bitrate in bits/second (matches Python Reticulum default)
pub(crate) const DEFAULT_BITRATE_BPS: u64 = 62_500;

fn default_bitrate() -> u64 {
    DEFAULT_BITRATE_BPS
}

impl Default for InterfaceConfig {
    /// An empty interface with the same baseline the serde `default_*` helpers
    /// give: enabled, outgoing, default bitrate, and every optional field unset.
    /// Programmatic builders set `interface_type` plus the fields they care
    /// about and fill the rest with `..Default::default()`.
    fn default() -> Self {
        Self {
            interface_type: String::new(),
            enabled: true,
            outgoing: true,
            bitrate: DEFAULT_BITRATE_BPS,
            listen_ip: None,
            listen_port: None,
            target_host: None,
            target_port: None,
            forward_ip: None,
            forward_port: None,
            port: None,
            speed: None,
            databits: None,
            parity: None,
            stopbits: None,
            buffer_size: None,
            reconnect_interval_secs: None,
            max_reconnect_tries: None,
            group_id: None,
            discovery_scope: None,
            discovery_port: None,
            data_port: None,
            devices: None,
            ignored_devices: None,
            multicast_loopback: None,
            announce_rate_target: None,
            announce_rate_penalty: None,
            announce_rate_grace: None,
            networkname: None,
            passphrase: None,
            ifac_size: None,
            frequency: None,
            bandwidth: None,
            spreading_factor: None,
            coding_rate: None,
            tx_power: None,
            flow_control: None,
            airtime_limit_short: None,
            airtime_limit_long: None,
            csma_enabled: None,
        }
    }
}

impl Config {
    /// Load configuration from a file
    ///
    /// Supports both TOML (native) and INI (Python Reticulum's ConfigObj format).
    /// Detection heuristic:
    /// - Explicit `.toml` extension → TOML
    /// - Contains `[[` (ConfigObj subsections) → INI
    /// - Default: try TOML, fall back to INI
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("Failed to read config: {e}")))?;

        // Explicit .toml extension → TOML only
        if path.extension().is_some_and(|e| e == "toml") {
            return toml::from_str(&content)
                .map_err(|e| Error::Config(format!("Failed to parse TOML config: {e}")));
        }

        // Python INI configs use [[ for interface subsections.
        // TOML uses [[ for array-of-tables, which our configs never use.
        if content.contains("[[") {
            return crate::ini_config::parse_ini(&content)
                .map_err(|e| Error::Config(format!("Failed to parse INI config: {e}")));
        }

        // Default: try TOML first, fall back to INI
        toml::from_str(&content).or_else(|_| {
            crate::ini_config::parse_ini(&content)
                .map_err(|e| Error::Config(format!("Failed to parse config: {e}")))
        })
    }

    /// Save configuration to a file
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let content = toml::to_string_pretty(self)
            .map_err(|e| Error::Config(format!("Failed to serialize config: {e}")))?;

        std::fs::write(path.as_ref(), content)
            .map_err(|e| Error::Config(format!("Failed to write config: {e}")))
    }

    /// Resolve the default config directory using the same lookup
    /// order as Python-Reticulum (RNS/Reticulum.py:230-237):
    ///   1. `/etc/reticulum`          — if `/etc/reticulum/config` exists
    ///   2. `$HOME/.config/reticulum` — if that dir's `config` exists
    ///   3. `$HOME/.reticulum`        — fallback, returned even if absent
    ///
    /// Matching this order keeps lnsd drop-in compatible with rnsd:
    /// when the Debian package installs a system-wide config under
    /// `/etc/reticulum`, Python clients (rnstatus, rncp, Sideband,
    /// Nomadnet) connect to the live daemon's shared-instance socket
    /// without any extra flags or env vars.
    pub fn default_config_dir() -> PathBuf {
        resolve_config_dir(
            Path::new("/etc/reticulum"),
            dirs::home_dir().as_deref(),
            |p| p.is_file(),
        )
    }

    /// Get the default config file path
    pub fn default_config_path() -> PathBuf {
        Self::default_config_dir().join("config")
    }
}

/// Pure resolver for the Python-Reticulum config-dir lookup, factored
/// out so tests can drive it without touching the real filesystem or
/// mutating `$HOME`.
fn resolve_config_dir<F: Fn(&Path) -> bool>(
    system_dir: &Path,
    home_dir: Option<&Path>,
    config_file_exists: F,
) -> PathBuf {
    if config_file_exists(&system_dir.join("config")) {
        return system_dir.to_path_buf();
    }
    let home = home_dir.unwrap_or(Path::new("."));
    let xdg = home.join(".config/reticulum");
    if config_file_exists(&xdg.join("config")) {
        return xdg;
    }
    home.join(".reticulum")
}

// Minimal home_dir implementation to avoid dirs crate dependency
mod dirs {
    use std::path::PathBuf;

    pub(super) fn home_dir() -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert!(config.reticulum.enable_transport);
        assert!(config.reticulum.use_implicit_proof);
    }

    #[test]
    fn test_enable_transport_defaults_true_when_missing_from_toml() {
        let toml_str = "[reticulum]\nuse_implicit_proof = true\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(
            config.reticulum.enable_transport,
            "missing enable_transport should default to true"
        );
    }

    #[test]
    fn test_flush_interval_defaults_when_missing_from_toml() {
        let toml_str = "[reticulum]\nuse_implicit_proof = true\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.reticulum.flush_interval_secs, DEFAULT_FLUSH_INTERVAL_SECS,
            "missing flush_interval_secs should default to 3600"
        );
    }

    #[test]
    fn test_channel_capacities_default_when_missing_from_toml() {
        let toml_str = "[reticulum]\nuse_implicit_proof = true\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.reticulum.control_channel_capacity, DEFAULT_CONTROL_CHANNEL_CAPACITY,
            "missing control_channel_capacity should default"
        );
        assert_eq!(
            config.reticulum.data_channel_capacity, DEFAULT_DATA_CHANNEL_CAPACITY,
            "missing data_channel_capacity should default"
        );
    }

    #[test]
    fn test_channel_capacities_explicit_in_toml() {
        let toml_str =
            "[reticulum]\ncontrol_channel_capacity = 1000\ndata_channel_capacity = 500\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.reticulum.control_channel_capacity, 1000);
        assert_eq!(config.reticulum.data_channel_capacity, 500);
    }

    #[test]
    fn test_flush_interval_explicit_in_toml() {
        let toml_str = "[reticulum]\nflush_interval_secs = 120\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.reticulum.flush_interval_secs, 120);
    }

    #[test]
    fn test_enable_transport_false_when_explicit_in_toml() {
        let toml_str = "[reticulum]\nenable_transport = false\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(
            !config.reticulum.enable_transport,
            "explicit false should be respected"
        );
    }

    #[test]
    fn test_config_serialization() {
        let config = Config::default();
        let toml_str = toml::to_string(&config).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(
            parsed.reticulum.enable_transport,
            config.reticulum.enable_transport
        );
    }

    // Mirror Python-Reticulum's RNS/Reticulum.py:230-237 lookup order.

    // Integ-level load: drive Config::load against a real on-disk INI file
    // carrying every RNS 1.2.2..1.3.5 new key plus the tcp-override case.
    // Exercises the full extension/`[[`-detection path, not just parse_ini.
    #[test]
    fn load_rns_13x_config_from_disk_tolerates_and_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        std::fs::write(
            &path,
            r#"
[reticulum]
  enable_transport = True
  share_instance = Yes
  blackhole_update_interval = 3600
  default_ar_target = 0.5
  default_ar_penalty = 2.0
  default_ar_grace = 60
  ic_max_held_announces = 10
  ic_burst_hold = 5.0
  ic_burst_freq_new = 3.5
  ic_burst_freq = 12.0
  ic_pr_burst_freq_new = 3.5
  ic_pr_burst_freq = 12.0
  ec_pr_freq = 1.0
  egress_control = True
  ic_new_time = 2.0
  ic_burst_penalty = 5.0
  ic_held_release_interval = 60
  shared_instance_socket = /run/reticulum/custom.sock
  shared_instance_type = tcp

[logging]
  logtimestamps = True

[interfaces]
  [[Tolerant TCP]]
    type = TCPServerInterface
    enabled = yes
    listen_port = 4242
    egress_control = True
    ic_pr_burst_freq_new = 3.5
    ic_pr_burst_freq = 12.0
    ec_pr_freq = 1.0
"#,
        )
        .unwrap();

        let config = Config::load(&path).expect("config with all new keys must load");
        assert!(config.reticulum.enable_transport);
        assert!(config.reticulum.shared_instance);
        // tcp overrides the configured socket path.
        assert_eq!(config.reticulum.shared_instance_type, Some("tcp".into()));
        assert_eq!(config.reticulum.shared_instance_socket, None);
        // Known interface semantics still hold; new keys ignored cleanly.
        let iface = config.interfaces.get("Tolerant TCP").expect("iface");
        assert_eq!(iface.interface_type, "TCPServerInterface");
        assert_eq!(iface.listen_port, Some(4242));
    }

    #[test]
    fn resolve_prefers_system_dir_when_config_present() {
        let r = resolve_config_dir(
            Path::new("/etc/reticulum"),
            Some(Path::new("/home/alice")),
            |p| p == Path::new("/etc/reticulum/config"),
        );
        assert_eq!(r, PathBuf::from("/etc/reticulum"));
    }

    #[test]
    fn resolve_falls_through_to_xdg_when_system_missing() {
        let r = resolve_config_dir(
            Path::new("/etc/reticulum"),
            Some(Path::new("/home/alice")),
            |p| p == Path::new("/home/alice/.config/reticulum/config"),
        );
        assert_eq!(r, PathBuf::from("/home/alice/.config/reticulum"));
    }

    #[test]
    fn resolve_final_fallback_is_dot_reticulum() {
        let r = resolve_config_dir(
            Path::new("/etc/reticulum"),
            Some(Path::new("/home/alice")),
            |_| false,
        );
        assert_eq!(r, PathBuf::from("/home/alice/.reticulum"));
    }

    #[test]
    fn resolve_prefers_system_over_xdg_when_both_present() {
        let r = resolve_config_dir(
            Path::new("/etc/reticulum"),
            Some(Path::new("/home/alice")),
            |_| true,
        );
        assert_eq!(r, PathBuf::from("/etc/reticulum"));
    }

    #[test]
    fn resolve_without_home_uses_current_dir_fallback() {
        let r = resolve_config_dir(Path::new("/etc/reticulum"), None, |_| false);
        assert_eq!(r, PathBuf::from("./.reticulum"));
    }
}
