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
    /// TCP-loopback port for the shared-instance data channel (Python
    /// `shared_instance_port`, `local_interface_port`, default 37428;
    /// `Reticulum.py:501-503`).
    ///
    /// Only the AF_INET path uses this port: `shared_instance_type = tcp`, or a
    /// platform without AF_UNIX (Windows). On the default AF_UNIX path the
    /// shared instance is keyed by `instance_name` (`\0rns/{instance_name}`) and
    /// the port is unused, exactly as in Python. `None` keeps the 37428 default.
    #[serde(default)]
    pub shared_instance_port: Option<u16>,
    /// TCP-loopback port for the shared-instance RPC control channel (Python
    /// `instance_control_port`, `local_control_port`, default 37429;
    /// `Reticulum.py:505-507`).
    ///
    /// Same AF_INET-only semantics as [`Self::shared_instance_port`]: on the
    /// default AF_UNIX path the RPC socket is `\0rns/{instance_name}/rpc` and the
    /// port is unused. `None` keeps the 37429 default.
    #[serde(default)]
    pub instance_control_port: Option<u16>,
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
    /// Identity hashes permitted to query remote management (`/status`).
    ///
    /// Each entry is a 32-hex-character (16-byte) identity hash, matching
    /// Python's `remote_management_allowed` list (`Reticulum.py:552-561`). An
    /// empty list with `remote_management_enabled = true` registers the
    /// handler but rejects every requester.
    #[serde(default)]
    pub remote_management_allowed: Vec<String>,
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
    /// Auto-connect discovered interfaces (Codeberg #32, sub-task b).
    ///
    /// A single integer that both gates and bounds runtime auto-connect,
    /// matching Python's `autoconnect_discovered_interfaces` option: `0`
    /// (default) disables it; `N > 0` enables it and caps concurrently
    /// auto-connected interfaces at `N`. Opt-in, like Python.
    #[serde(default)]
    pub autoconnect_discovered_interfaces: usize,
    /// Network identity for a private (encrypted) discovery network (Codeberg
    /// #32, sub-task d). Path to a raw 64-byte identity file (Python
    /// `network_identity`, Reticulum.py:521-542). When set, received discovery
    /// announces encrypted for this identity are decrypted before validation;
    /// unset keeps the plaintext discovery path. A relative path is resolved
    /// against the config directory / storage root by the builder.
    #[serde(default)]
    pub network_identity: Option<PathBuf>,
    /// Discovery announcer job interval in seconds (Python
    /// `InterfaceAnnouncer.JOB_INTERVAL`, default 60). Each tick advertises the
    /// most-overdue discoverable interface. Lower it for fast tests (Codeberg
    /// #107).
    #[serde(default = "default_discovery_job_interval_secs")]
    pub discovery_job_interval_secs: u64,
}

/// Default discovery announcer job interval (seconds), Python
/// `InterfaceAnnouncer.JOB_INTERVAL`.
pub const DEFAULT_DISCOVERY_JOB_INTERVAL_SECS: u64 = 60;

fn default_discovery_job_interval_secs() -> u64 {
    DEFAULT_DISCOVERY_JOB_INTERVAL_SECS
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
            shared_instance_port: None,
            instance_control_port: None,
            respond_to_probes: false,
            remote_management_enabled: false,
            remote_management_allowed: Vec::new(),
            storage_path: None,
            flush_interval_secs: DEFAULT_FLUSH_INTERVAL_SECS,
            control_channel_capacity: DEFAULT_CONTROL_CHANNEL_CAPACITY,
            data_channel_capacity: DEFAULT_DATA_CHANNEL_CAPACITY,
            keepalive_interval: None,
            autoconnect_discovered_interfaces: 0,
            network_identity: None,
            discovery_job_interval_secs: DEFAULT_DISCOVERY_JOB_INTERVAL_SECS,
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
    /// Reticulum propagation mode (Codeberg #91). Raw config string as written
    /// (`full`, `gateway`/`gw`, `access_point`/`accesspoint`/`ap`,
    /// `pointtopoint`/`ptp`, `roaming`, `boundary`); resolved to
    /// `InterfaceMode` at registration. `interface_mode` is accepted as an
    /// alias (Reticulum.py:717-745). `None` means the `Full` default.
    #[serde(default, alias = "interface_mode")]
    pub mode: Option<String>,
    /// Can send outgoing packets
    #[serde(default = "default_true")]
    pub outgoing: bool,
    /// Configured medium bitrate in bits per second (Python `bitrate`,
    /// Reticulum.py:793-796). `None` when unset or below
    /// [`leviculum_core::constants::MINIMUM_BITRATE`]; a set value overrides the
    /// interface's default and feeds announce bandwidth capping / timing.
    #[serde(default)]
    pub bitrate: Option<u64>,

    // Interface discovery producer (Codeberg #32 / #107). A `discoverable = yes`
    // interface periodically self-advertises on
    // `rnstransport.discovery.interface` (Python Reticulum.py:848-905). The
    // interface supplies the descriptor; the announcer owns cadence + stamping.
    /// Advertise this interface via on-network discovery (Python `discoverable`).
    #[serde(default)]
    pub discoverable: bool,
    /// Human-readable name published in the discovery announce (Python
    /// `discovery_name`). The receiver falls back to `Discovered <type>`.
    #[serde(default)]
    pub discovery_name: Option<String>,
    /// Address peers should connect to, published for TCP/Backbone interfaces
    /// (Python `reachable_on`). Defaults to `listen_ip` when unset.
    #[serde(default)]
    pub reachable_on: Option<String>,
    /// Encrypt the discovery announce with the node's network identity (Python
    /// `discovery_encrypt`). Requires `network_identity` to be configured; an
    /// encrypt request without one skips the interface.
    #[serde(default)]
    pub discovery_encrypt: bool,
    /// Discovery announce interval in minutes (Python `announce_interval`,
    /// clamped to a 5-minute floor, default 6 hours). Superseded by
    /// `discovery_announce_interval_secs` when that is set.
    #[serde(default)]
    pub announce_interval: Option<u64>,
    /// Direct per-interface discovery announce interval in seconds. Leviculum
    /// override that bypasses the Python 5-minute floor so fast tests can drive
    /// the announcer on a short cadence. Takes priority over `announce_interval`.
    #[serde(default)]
    pub discovery_announce_interval_secs: Option<u64>,

    // TCP/UDP specific
    /// Kernel network interface name to bind to (Python `device`,
    /// TCPInterface.py:504 / UDPInterface.py:61 / BackboneInterface.py:114).
    /// When set, the bind address is resolved from this NIC's addresses
    /// instead of the wildcard/`listen_ip` default: for TCP/Backbone the
    /// interface's own IPv4 (or IPv6 when `prefer_ipv6`) address, for UDP its
    /// IPv4 broadcast address. Lets an interface bind one physical NIC on a
    /// multi-homed host (Codeberg #94, #3).
    pub device: Option<String>,
    /// Prefer an IPv6 bind address when resolving `device` (Python
    /// `prefer_ipv6`, TCPInterface.py:509 / BackboneInterface.py:118). No
    /// effect without `device`, or for UDP (broadcast is IPv4-only).
    pub prefer_ipv6: Option<bool>,
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

    // PipeInterface specific
    /// External command to spawn (HDLC-over-stdio bridge). Split shell-style.
    pub command: Option<String>,
    /// Delay in seconds before respawning the child after it exits (default: 5).
    pub respawn_delay: Option<f64>,

    // KISSInterface specific (TNC parameters + beacon).
    /// Preamble / TX delay in ms sent to the TNC (CMD_TXDELAY, default 350).
    pub preamble: Option<u32>,
    /// TX tail in ms sent to the TNC (CMD_TXTAIL, default 20).
    pub txtail: Option<u32>,
    /// Persistence parameter sent to the TNC (CMD_P, default 64).
    pub persistence: Option<u32>,
    /// Slot time in ms sent to the TNC (CMD_SLOTTIME, default 20).
    pub slottime: Option<u32>,
    /// Beacon identification interval in seconds (Python `id_interval`).
    pub id_interval: Option<u32>,
    /// Beacon identification callsign/data (Python `id_callsign`).
    pub id_callsign: Option<String>,

    // AX25KISSInterface specific (AX.25 UI-frame addressing).
    /// Source callsign for the AX.25 address (Python `callsign`, 3-6 chars).
    pub callsign: Option<String>,
    /// Source SSID for the AX.25 address (Python `ssid`, 0-15).
    pub ssid: Option<u8>,

    // I2PInterface specific (Reticulum over I2P via SAM v3).
    /// Remote `.b32.i2p` (or base64) destinations to connect to as a client
    /// (Python `peers`, ConfigObj `as_list`). One outbound sub-interface each.
    pub peers: Option<Vec<String>>,
    /// Whether this interface opens a local I2P endpoint accepting inbound
    /// connections (Python `connectable`).
    pub connectable: Option<bool>,

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

    // Announce-rate limiting (Codeberg #92). Python: Reticulum.py:798-821.
    // The target/grace/penalty keys drive per-destination rebroadcast rate
    // limiting (Transport.py:1838-1864); enforcement lives in transport.
    /// Announce-rate target in seconds (Python `announce_rate_target`).
    pub announce_rate_target: Option<u32>,
    /// Announce-rate penalty in seconds (Python `announce_rate_penalty`).
    pub announce_rate_penalty: Option<u32>,
    /// Announce-rate grace count (Python `announce_rate_grace`).
    pub announce_rate_grace: Option<u32>,
    /// Announce bandwidth cap as a percentage of link capacity (Python
    /// `announce_cap`, Reticulum.py:819-822). Kept only when `0 < v <= 100`.
    /// `None` uses the default cap (2%). Python stores this as a fraction; we
    /// keep the raw percentage.
    pub announce_cap: Option<f32>,

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

    // RNodeMultiInterface only.
    /// Nested subinterface blocks (`[[[name]]]`) of an `RNodeMultiInterface`.
    /// Each becomes one vport logical interface. Empty for every other type.
    #[serde(default)]
    pub subinterfaces: Vec<SubinterfaceConfig>,
}

/// A single vport subinterface of an `RNodeMultiInterface`, parsed from a
/// `[[[name]]]` block nested under the multi interface. Carries only the
/// per-vport radio + routing keys; the shared serial `port` and `id_*` beacon
/// keys live on the parent `InterfaceConfig`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubinterfaceConfig {
    /// Subinterface name (the `[[[name]]]` header).
    pub name: String,
    /// Whether this subinterface is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Whether this subinterface may transmit (Python `outgoing`).
    #[serde(default = "default_true")]
    pub outgoing: bool,
    /// Virtual port index on the device.
    pub vport: Option<u8>,
    /// LoRa frequency in Hz.
    pub frequency: Option<u64>,
    /// LoRa bandwidth in Hz.
    pub bandwidth: Option<u32>,
    /// LoRa spreading factor.
    pub spreading_factor: Option<u8>,
    /// LoRa coding rate.
    pub coding_rate: Option<u8>,
    /// TX power in dBm.
    pub tx_power: Option<i8>,
    /// Short-term airtime limit as percent (0.0-100.0).
    pub airtime_limit_short: Option<f64>,
    /// Long-term airtime limit as percent (0.0-100.0).
    pub airtime_limit_long: Option<f64>,
}

impl Default for SubinterfaceConfig {
    /// Enabled and outgoing by default (matching the serde `default_*` helpers),
    /// every radio field unset. The INI parser fills fields from the
    /// `[[[name]]]` block, leaving these defaults for keys the block omits.
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            outgoing: true,
            vport: None,
            frequency: None,
            bandwidth: None,
            spreading_factor: None,
            coding_rate: None,
            tx_power: None,
            airtime_limit_short: None,
            airtime_limit_long: None,
        }
    }
}

impl Default for InterfaceConfig {
    /// An empty interface with the same baseline the serde `default_*` helpers
    /// give: enabled, outgoing, no configured bitrate, and every optional field
    /// unset. Programmatic builders set `interface_type` plus the fields they
    /// care about and fill the rest with `..Default::default()`.
    fn default() -> Self {
        Self {
            interface_type: String::new(),
            enabled: true,
            mode: None,
            outgoing: true,
            bitrate: None,
            discoverable: false,
            discovery_name: None,
            reachable_on: None,
            discovery_encrypt: false,
            announce_interval: None,
            discovery_announce_interval_secs: None,
            device: None,
            prefer_ipv6: None,
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
            command: None,
            respawn_delay: None,
            preamble: None,
            txtail: None,
            persistence: None,
            slottime: None,
            id_interval: None,
            id_callsign: None,
            callsign: None,
            ssid: None,
            peers: None,
            connectable: None,
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
            announce_cap: None,
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
            subinterfaces: Vec::new(),
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
