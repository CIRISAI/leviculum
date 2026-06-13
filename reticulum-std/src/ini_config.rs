//! Minimal INI parser for Python Reticulum's ConfigObj format
//!
//! Handles:
//! - `[section]` headers
//! - `[[subsection]]` headers (ConfigObj-style nested sections under `[interfaces]`)
//! - `key = value` pairs (whitespace-stripped)
//! - `#` comments
//! - Boolean parsing: Yes/yes/True/true/1 → true, No/no/False/false/0 → false
//!
//! Only TCP interfaces are supported; unknown types are logged and skipped.

use std::collections::HashMap;

use crate::config::{Config, InterfaceConfig, ReticulumConfig, DEFAULT_BITRATE_BPS};

/// Parse a Python Reticulum INI config string into our `Config` struct.
pub(crate) fn parse_ini(content: &str) -> Result<Config, String> {
    let mut reticulum = ReticulumConfig::default();
    let mut interfaces: HashMap<String, InterfaceConfig> = HashMap::new();

    let mut current_section = String::new();
    let mut current_subsection: Option<String> = None;
    let mut current_iface: Option<(String, InterfaceConfig)> = None;

    for line in content.lines() {
        let trimmed = line.trim();

        // Skip empty lines and comments
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Subsection header: [[name]] (must check before section)
        if trimmed.starts_with("[[") && trimmed.ends_with("]]") {
            // Flush previous interface
            if let Some((name, iface)) = current_iface.take() {
                interfaces.insert(name, iface);
            }

            let name = trimmed[2..trimmed.len() - 2].trim().to_string();
            current_subsection = Some(name.clone());
            current_iface = Some((
                name,
                InterfaceConfig {
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
                },
            ));
            continue;
        }

        // Section header: [name]
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            // Flush previous interface
            if let Some((name, iface)) = current_iface.take() {
                interfaces.insert(name, iface);
            }
            current_section = trimmed[1..trimmed.len() - 1].trim().to_string();
            current_subsection = None;
            continue;
        }

        // Key = value
        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim();
            let value = value.trim();

            if current_subsection.is_some() {
                // Inside an interface subsection
                if let Some((_, ref mut iface)) = current_iface {
                    apply_interface_key(iface, key, value);
                }
            } else {
                // Inside a top-level section
                if current_section.as_str() == "reticulum" {
                    apply_reticulum_key(&mut reticulum, key, value);
                }
            }
        }
    }

    // Flush last interface
    if let Some((name, iface)) = current_iface.take() {
        interfaces.insert(name, iface);
    }

    // RNS 1.3.x semantic: shared_instance_type = tcp disables AF_UNIX and
    // therefore overrides any configured shared_instance_socket path (tcp
    // wins on conflict). Applied here, post-parse, so it holds for any key
    // ordering in the file.
    if reticulum.shared_instance_type.as_deref() == Some("tcp") {
        reticulum.shared_instance_socket = None;
    }

    // Filter out unsupported interface types
    let supported: HashMap<String, InterfaceConfig> = interfaces
        .into_iter()
        .filter(|(name, iface)| match iface.interface_type.as_str() {
            "TCPServerInterface" | "TCPClientInterface" | "UDPInterface" | "AutoInterface"
            | "RNodeInterface" | "SerialInterface" => true,
            other => {
                tracing::info!(
                    "Skipping unsupported interface type '{}' for '{}'",
                    other,
                    name
                );
                false
            }
        })
        .collect();

    Ok(Config {
        reticulum,
        interfaces: supported,
    })
}

fn apply_reticulum_key(config: &mut ReticulumConfig, key: &str, value: &str) {
    match key {
        "enable_transport" => {
            config.enable_transport = parse_bool(value);
        }
        "share_instance" => {
            config.shared_instance = parse_bool(value);
        }
        "instance_name" => {
            config.instance_name = value.trim().to_string();
        }
        "shared_instance_type" => {
            // Upstream only honours tcp/unix (lowercased); other values
            // are tolerated but not stored. The tcp-overrides-socket rule
            // is applied after the full parse (see parse_ini), so it holds
            // regardless of key order.
            let v = value.trim().to_ascii_lowercase();
            if v == "tcp" || v == "unix" {
                config.shared_instance_type = Some(v);
            }
        }
        "shared_instance_socket" => {
            config.shared_instance_socket = Some(value.trim().to_string());
        }
        "respond_to_probes" => {
            config.respond_to_probes = parse_bool(value);
        }
        "remote_management_enabled" => {
            config.remote_management_enabled = parse_bool(value);
        }
        "flush_interval" => {
            if let Ok(v) = value.trim().parse() {
                config.flush_interval_secs = v;
            }
        }
        // Tolerate (accept without error) RNS 1.2.2..1.3.5 reticulum-level
        // keys we don't implement: blackhole_update_interval, default_ar_*,
        // egress_control, the ic_*/ic_pr_*/ec_pr_freq ingress/egress-control
        // tuning knobs, shared_instance_port, etc. An unknown key must never
        // make lnsd reject a config a current rnsd would accept.
        _ => {}
    }
}

fn apply_interface_key(iface: &mut InterfaceConfig, key: &str, value: &str) {
    match key {
        "type" => iface.interface_type = value.to_string(),
        "enabled" => iface.enabled = parse_bool(value),
        "outgoing" => iface.outgoing = parse_bool(value),
        "listen_ip" => iface.listen_ip = Some(value.to_string()),
        "listen_port" => iface.listen_port = value.parse().ok(),
        "target_host" => iface.target_host = Some(value.to_string()),
        "target_port" => iface.target_port = value.parse().ok(),
        "forward_ip" => iface.forward_ip = Some(value.to_string()),
        "forward_port" => iface.forward_port = value.parse().ok(),
        "port" => iface.port = Some(value.to_string()),
        "speed" | "baudrate" => iface.speed = value.parse().ok(),
        "databits" => iface.databits = value.parse().ok(),
        "parity" => iface.parity = Some(value.to_string()),
        "stopbits" => iface.stopbits = value.parse().ok(),
        "bitrate" => {
            if let Ok(v) = value.parse() {
                iface.bitrate = v;
            }
        }
        "buffer_size" => iface.buffer_size = value.parse().ok(),
        "reconnect_interval" => iface.reconnect_interval_secs = value.parse().ok(),
        "max_reconnect_tries" => iface.max_reconnect_tries = value.parse().ok(),
        "frequency" => iface.frequency = value.parse().ok(),
        "bandwidth" => iface.bandwidth = value.parse().ok(),
        "spreadingfactor" | "spreading_factor" => iface.spreading_factor = value.parse().ok(),
        "codingrate" | "coding_rate" => iface.coding_rate = value.parse().ok(),
        "txpower" | "tx_power" => iface.tx_power = value.parse().ok(),
        // AutoInterface specific
        "group_id" => iface.group_id = Some(value.to_string()),
        "discovery_scope" => iface.discovery_scope = Some(value.to_string()),
        "discovery_port" => iface.discovery_port = value.parse().ok(),
        "data_port" => iface.data_port = value.parse().ok(),
        "devices" => iface.devices = Some(value.to_string()),
        "ignored_devices" => iface.ignored_devices = Some(value.to_string()),
        "multicast_loopback" => iface.multicast_loopback = Some(parse_bool(value)),
        "flow_control" => iface.flow_control = Some(parse_bool(value)),
        "airtime_limit_short" => iface.airtime_limit_short = value.parse().ok(),
        "airtime_limit_long" => iface.airtime_limit_long = value.parse().ok(),
        "csma_enabled" => iface.csma_enabled = Some(parse_bool(value)),
        "networkname" | "network_name" => iface.networkname = Some(value.to_string()),
        "passphrase" => iface.passphrase = Some(value.to_string()),
        "ifac_size" => iface.ifac_size = value.parse::<usize>().ok().map(|bits| bits / 8),
        _ => {} // Ignore unknown keys (id_callsign, id_interval, modulation, etc.)
    }
}

/// Parse a ConfigObj boolean value.
///
/// Accepts: Yes, yes, True, true, 1 → true
///          No, no, False, false, 0 → false
///          Anything else → false (conservative default)
fn parse_bool(value: &str) -> bool {
    matches!(value, "Yes" | "yes" | "True" | "true" | "1" | "on" | "On")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bool_true() {
        assert!(parse_bool("Yes"));
        assert!(parse_bool("yes"));
        assert!(parse_bool("True"));
        assert!(parse_bool("true"));
        assert!(parse_bool("1"));
        assert!(parse_bool("on"));
    }

    #[test]
    fn test_parse_bool_false() {
        assert!(!parse_bool("No"));
        assert!(!parse_bool("no"));
        assert!(!parse_bool("False"));
        assert!(!parse_bool("false"));
        assert!(!parse_bool("0"));
        assert!(!parse_bool("off"));
        assert!(!parse_bool(""));
    }

    #[test]
    fn test_parse_minimal_ini() {
        let config = parse_ini(
            r#"
[reticulum]
  enable_transport = True

[interfaces]
  [[My TCP Server]]
    type = TCPServerInterface
    enabled = yes
    listen_ip = 0.0.0.0
    listen_port = 4242

  [[My TCP Client]]
    type = TCPClientInterface
    enabled = True
    target_host = 127.0.0.1
    target_port = 4243
"#,
        )
        .unwrap();

        assert!(config.reticulum.enable_transport);

        let server = config.interfaces.get("My TCP Server").expect("server");
        assert_eq!(server.interface_type, "TCPServerInterface");
        assert!(server.enabled);
        assert_eq!(server.listen_ip, Some("0.0.0.0".to_string()));
        assert_eq!(server.listen_port, Some(4242));

        let client = config.interfaces.get("My TCP Client").expect("client");
        assert_eq!(client.interface_type, "TCPClientInterface");
        assert!(client.enabled);
        assert_eq!(client.target_host, Some("127.0.0.1".to_string()));
        assert_eq!(client.target_port, Some(4243));
    }

    #[test]
    fn test_skip_unsupported_interface_types() {
        let config = parse_ini(
            r#"
[interfaces]
  [[Auto]]
    type = AutoInterface
    enabled = Yes

  [[RNode]]
    type = RNodeInterface
    port = /dev/ttyACM0

  [[TCP Server]]
    type = TCPServerInterface
    enabled = yes
    listen_port = 4242

  [[Serial KISS]]
    type = KISSInterface
    port = /dev/ttyUSB0
"#,
        )
        .unwrap();

        // KISSInterface should be skipped; Auto, RNode, TCP are supported
        assert_eq!(config.interfaces.len(), 3);
        assert!(config.interfaces.contains_key("TCP Server"));
        assert!(config.interfaces.contains_key("Auto"));
        assert!(config.interfaces.contains_key("RNode"));
        assert!(!config.interfaces.contains_key("Serial KISS"));
    }

    #[test]
    fn test_parse_auto_interface_all_params() {
        let config = parse_ini(
            r#"
[interfaces]
  [[Auto Interface]]
    type = AutoInterface
    enabled = yes
    group_id = my_network
    discovery_scope = site
    discovery_port = 30000
    data_port = 40000
    devices = eth0, wlan0
    ignored_devices = docker0
"#,
        )
        .unwrap();

        let auto = config.interfaces.get("Auto Interface").expect("auto iface");
        assert_eq!(auto.interface_type, "AutoInterface");
        assert!(auto.enabled);
        assert_eq!(auto.group_id, Some("my_network".to_string()));
        assert_eq!(auto.discovery_scope, Some("site".to_string()));
        assert_eq!(auto.discovery_port, Some(30000));
        assert_eq!(auto.data_port, Some(40000));
        assert_eq!(auto.devices, Some("eth0, wlan0".to_string()));
        assert_eq!(auto.ignored_devices, Some("docker0".to_string()));
    }

    #[test]
    fn test_parse_auto_interface_defaults() {
        let config = parse_ini(
            r#"
[interfaces]
  [[Auto]]
    type = AutoInterface
"#,
        )
        .unwrap();

        let auto = config.interfaces.get("Auto").expect("auto iface");
        assert_eq!(auto.interface_type, "AutoInterface");
        assert!(auto.enabled); // default
        assert_eq!(auto.group_id, None);
        assert_eq!(auto.discovery_scope, None);
        assert_eq!(auto.discovery_port, None);
        assert_eq!(auto.data_port, None);
        assert_eq!(auto.devices, None);
        assert_eq!(auto.ignored_devices, None);
    }

    #[test]
    fn test_parse_udp_interface() {
        let config = parse_ini(
            r#"
[interfaces]
  [[UDP Interface]]
    type = UDPInterface
    enabled = yes
    listen_ip = 0.0.0.0
    listen_port = 4242
    forward_ip = 192.168.1.255
    forward_port = 4242
"#,
        )
        .unwrap();

        let udp = config.interfaces.get("UDP Interface").expect("udp");
        assert_eq!(udp.interface_type, "UDPInterface");
        assert!(udp.enabled);
        assert_eq!(udp.listen_ip, Some("0.0.0.0".to_string()));
        assert_eq!(udp.listen_port, Some(4242));
        assert_eq!(udp.forward_ip, Some("192.168.1.255".to_string()));
        assert_eq!(udp.forward_port, Some(4242));
    }

    #[test]
    fn test_comments_and_whitespace() {
        let config = parse_ini(
            r#"
# This is a comment
[reticulum]
  # Another comment
  enable_transport = True
  share_instance = No

[interfaces]
  # Commented out interface
  # [[Disabled]]
  #   type = TCPClientInterface

  [[Active]]
    type = TCPServerInterface
    enabled = yes
    listen_port = 1234
"#,
        )
        .unwrap();

        assert!(config.reticulum.enable_transport);
        assert!(!config.reticulum.shared_instance);
        assert_eq!(config.interfaces.len(), 1);
    }

    #[test]
    fn test_disabled_interface() {
        let config = parse_ini(
            r#"
[interfaces]
  [[Disabled Server]]
    type = TCPServerInterface
    enabled = No
    listen_port = 4242
"#,
        )
        .unwrap();

        let server = config.interfaces.get("Disabled Server").expect("server");
        assert!(!server.enabled);
    }

    #[test]
    fn test_empty_config() {
        let config = parse_ini("").unwrap();
        assert!(
            config.reticulum.enable_transport,
            "empty config should default enable_transport to true"
        );
        assert!(config.interfaces.is_empty());
    }

    #[test]
    fn test_reticulum_section_only() {
        let config = parse_ini(
            r#"
[reticulum]
  enable_transport = False
  share_instance = Yes
"#,
        )
        .unwrap();

        assert!(!config.reticulum.enable_transport);
        assert!(config.reticulum.shared_instance);
        assert!(config.interfaces.is_empty());
    }

    #[test]
    fn test_flush_interval_parsed() {
        let config = parse_ini(
            r#"
[reticulum]
  flush_interval = 600
"#,
        )
        .unwrap();
        assert_eq!(config.reticulum.flush_interval_secs, 600);
    }

    #[test]
    fn test_flush_interval_default_when_absent() {
        let config = parse_ini("[reticulum]\n  enable_transport = True\n").unwrap();
        assert_eq!(
            config.reticulum.flush_interval_secs,
            crate::config::DEFAULT_FLUSH_INTERVAL_SECS,
            "absence of flush_interval must keep the 3600 s default"
        );
    }

    #[test]
    fn test_flush_interval_unparseable_keeps_default() {
        let config = parse_ini("[reticulum]\n  flush_interval = often\n").unwrap();
        assert_eq!(
            config.reticulum.flush_interval_secs,
            crate::config::DEFAULT_FLUSH_INTERVAL_SECS
        );
    }

    #[test]
    fn test_respond_to_probes_default_false() {
        let config = parse_ini("[reticulum]\n").unwrap();
        assert!(!config.reticulum.respond_to_probes);
    }

    #[test]
    fn test_respond_to_probes_enabled() {
        let config = parse_ini(
            r#"
[reticulum]
  respond_to_probes = Yes
"#,
        )
        .unwrap();
        assert!(config.reticulum.respond_to_probes);
    }

    #[test]
    fn test_instance_name_parsed() {
        let config = parse_ini(
            r#"
[reticulum]
  share_instance = Yes
  instance_name = miauhaus
"#,
        )
        .unwrap();
        assert!(config.reticulum.shared_instance);
        assert_eq!(config.reticulum.instance_name, "miauhaus");
    }

    #[test]
    fn test_instance_name_defaults_to_default() {
        let config = parse_ini("[reticulum]\n").unwrap();
        assert_eq!(config.reticulum.instance_name, "default");
    }

    #[test]
    fn test_shared_instance_type_tcp_overrides_socket_path() {
        // RNS 1.3.x semantic: shared_instance_type = tcp wins over a
        // configured shared_instance_socket path (tcp disables AF_UNIX),
        // regardless of key order in the file.
        let config = parse_ini(
            r#"
[reticulum]
  share_instance = Yes
  shared_instance_socket = /run/reticulum/custom.sock
  shared_instance_type = tcp
"#,
        )
        .unwrap();
        assert!(config.reticulum.shared_instance);
        assert_eq!(config.reticulum.shared_instance_type, Some("tcp".into()));
        assert_eq!(
            config.reticulum.shared_instance_socket, None,
            "tcp must override (clear) a configured socket path"
        );
    }

    #[test]
    fn test_shared_instance_socket_kept_when_type_unix() {
        let config = parse_ini(
            r#"
[reticulum]
  shared_instance_socket = /run/reticulum/custom.sock
  shared_instance_type = unix
"#,
        )
        .unwrap();
        assert_eq!(config.reticulum.shared_instance_type, Some("unix".into()));
        assert_eq!(
            config.reticulum.shared_instance_socket,
            Some("/run/reticulum/custom.sock".into())
        );
    }

    #[test]
    fn test_shared_instance_socket_kept_when_type_absent() {
        let config = parse_ini(
            r#"
[reticulum]
  shared_instance_socket = /run/reticulum/custom.sock
"#,
        )
        .unwrap();
        assert_eq!(config.reticulum.shared_instance_type, None);
        assert_eq!(
            config.reticulum.shared_instance_socket,
            Some("/run/reticulum/custom.sock".into())
        );
    }

    #[test]
    fn test_shared_instance_type_invalid_value_ignored() {
        // Mirror upstream: only tcp/unix are accepted; anything else is
        // tolerated but not stored.
        let config = parse_ini("[reticulum]\n  shared_instance_type = bogus\n").unwrap();
        assert_eq!(config.reticulum.shared_instance_type, None);
    }

    #[test]
    fn test_tolerate_all_rns_13x_new_keys() {
        // A config carrying every new 1.2.2..1.3.5 key (reticulum-level,
        // logging section, and per-interface) must parse without error.
        // None of these features are implemented; they are tolerate-only.
        let config = parse_ini(
            r#"
[reticulum]
  enable_transport = True
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

        // Known keys still take effect; the new keys are ignored cleanly.
        assert!(config.reticulum.enable_transport);
        let iface = config.interfaces.get("Tolerant TCP").expect("iface");
        assert_eq!(iface.interface_type, "TCPServerInterface");
        assert_eq!(iface.listen_port, Some(4242));
    }

    #[test]
    fn test_parse_rnode_interface() {
        let config = parse_ini(
            r#"
[interfaces]
  [[My RNode]]
    type = RNodeInterface
    port = /dev/ttyACM0
    frequency = 868000000
    bandwidth = 125000
    spreadingfactor = 7
    codingrate = 5
    txpower = 17
    flow_control = true
    airtime_limit_short = 15.0
    airtime_limit_long = 5.0
"#,
        )
        .unwrap();

        let rnode = config.interfaces.get("My RNode").expect("rnode iface");
        assert_eq!(rnode.interface_type, "RNodeInterface");
        assert_eq!(rnode.port, Some("/dev/ttyACM0".to_string()));
        assert_eq!(rnode.frequency, Some(868_000_000));
        assert_eq!(rnode.bandwidth, Some(125_000));
        assert_eq!(rnode.spreading_factor, Some(7));
        assert_eq!(rnode.coding_rate, Some(5));
        assert_eq!(rnode.tx_power, Some(17));
        assert_eq!(rnode.flow_control, Some(true));
        assert_eq!(rnode.airtime_limit_short, Some(15.0));
        assert_eq!(rnode.airtime_limit_long, Some(5.0));
    }
}
