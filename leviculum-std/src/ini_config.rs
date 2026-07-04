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
                    command: None,
                    respawn_delay: None,
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

    // Normalize Backbone interfaces onto our TCP interface. BackboneInterface is
    // wire-identical to TCPInterface (HDLC-over-TCP), so we map the config the
    // same way Python does (Reticulum.py:960-972) and let the rest of the stack
    // treat it as a TCPServer/TCPClient. Done post-parse so key order in the file
    // does not matter -- mirrors ConfigObj handing Python the whole section dict.
    for iface in interfaces.values_mut() {
        normalize_backbone_interface(iface);
    }

    // IFAC keys (network_name/networkname/passphrase/pass_phrase/ifac_size) are
    // now enforced by lnsd (Codeberg #90): the driver derives an IfacConfig and
    // the transport applies the HMAC on TX and verifies + drops on RX. A VALID
    // IFAC config must therefore NOT warn -- it is authenticated on the air.
    //
    // Two cases still deserve a warning:
    //   1. IFAC is requested (network_name and/or passphrase) but the derived
    //      IfacConfig would be INVALID (e.g. an out-of-range ifac_size), which
    //      would silently leave the interface unauthenticated.
    //   2. `ifac_size` is set with NEITHER network_name nor passphrase. Python
    //      needs a netname or netkey to enable IFAC (Reticulum.py:923-926), so
    //      a lone ifac_size is a no-op -- flag the likely misconfiguration.
    for (name, iface) in interfaces.iter() {
        let has_ident = iface.networkname.is_some() || iface.passphrase.is_some();
        if has_ident {
            let size = iface
                .ifac_size
                .unwrap_or(match iface.interface_type.as_str() {
                    "RNodeInterface" | "SerialInterface" | "PipeInterface" => 8,
                    _ => 16,
                });
            if leviculum_core::ifac::IfacConfig::new(
                iface.networkname.as_deref(),
                iface.passphrase.as_deref(),
                size,
            )
            .is_err()
            {
                tracing::warn!(
                    "interface '{}': IFAC (network_name/passphrase) is configured but the \
                     derived access code is INVALID (check ifac_size) -- this interface will \
                     run UNAUTHENTICATED and cannot join the IFAC-protected network.",
                    name
                );
            }
        } else if iface.ifac_size.is_some() {
            tracing::warn!(
                "interface '{}': ifac_size is set without network_name or passphrase -- IFAC \
                 needs one of those to be enabled, so ifac_size alone has no effect.",
                name
            );
        }
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
            | "RNodeInterface" | "SerialInterface" | "PipeInterface" => true,
            other => {
                tracing::warn!(
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
        // `enable_remote_management` is the upstream rnsd key
        // (Reticulum.py:548); `remote_management_enabled` is accepted as a
        // Leviculum-side alias so either spelling works.
        "enable_remote_management" | "remote_management_enabled" => {
            config.remote_management_enabled = parse_bool(value);
        }
        // Comma-separated identity hashes (ConfigObj `as_list`,
        // Reticulum.py:553). Hex is validated when the destination is built.
        "remote_management_allowed" => {
            config.remote_management_allowed = value
                .split(',')
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
                .collect();
        }
        "flush_interval" => {
            if let Ok(v) = value.trim().parse() {
                config.flush_interval_secs = v;
            }
        }
        "keepalive_interval" => {
            if let Ok(v) = value.trim().parse() {
                config.keepalive_interval = Some(v);
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
        // `enabled` is the modern key; `interface_enabled` is the legacy spelling
        // upstream still honours (Reticulum.py:950). Accept both so a config that
        // disables an interface with the old key is not silently enabled.
        "enabled" | "interface_enabled" => iface.enabled = parse_bool(value),
        "outgoing" => iface.outgoing = parse_bool(value),
        "listen_ip" => iface.listen_ip = Some(value.to_string()),
        "listen_port" => iface.listen_port = value.parse().ok(),
        "target_host" => iface.target_host = Some(value.to_string()),
        "target_port" => iface.target_port = value.parse().ok(),
        // Backbone key aliases (Reticulum.py:963-964): `remote` is the peer to
        // connect to (-> target_host), `listen_on` is the local bind address
        // (-> listen_ip). No other interface type uses these spellings, so the
        // mapping is a safe superset; the `port` alias needs the type context and
        // is handled in `normalize_backbone_interface` after the full parse.
        "remote" => iface.target_host = Some(value.to_string()),
        "listen_on" => iface.listen_ip = Some(value.to_string()),
        "forward_ip" => iface.forward_ip = Some(value.to_string()),
        "forward_port" => iface.forward_port = value.parse().ok(),
        "port" => iface.port = Some(value.to_string()),
        "speed" | "baudrate" => iface.speed = value.parse().ok(),
        "databits" => iface.databits = value.parse().ok(),
        "parity" => iface.parity = Some(value.to_string()),
        "stopbits" => iface.stopbits = value.parse().ok(),
        // PipeInterface: external command + optional respawn delay
        // (PipeInterface.py:67-68). `command` is a plain shell-style string.
        "command" => iface.command = Some(value.to_string()),
        "respawn_delay" => iface.respawn_delay = value.parse().ok(),
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
        // Announce-rate keys (Codeberg #67 Stage 2a). Parsed as unsigned, so a
        // negative value fails to parse and stays None; Python's >0 (target) /
        // >=0 (grace, penalty) validation is applied later in the driver.
        "announce_rate_target" => iface.announce_rate_target = value.parse().ok(),
        "announce_rate_penalty" => iface.announce_rate_penalty = value.parse().ok(),
        "announce_rate_grace" => iface.announce_rate_grace = value.parse().ok(),
        "networkname" | "network_name" => iface.networkname = Some(value.to_string()),
        "passphrase" | "pass_phrase" => iface.passphrase = Some(value.to_string()),
        // `ifac_size` is specified in BITS and stored in BYTES. Python only
        // honours it when it is at least `IFAC_MIN_SIZE*8` bits (= 8 bits =
        // 1 byte); a smaller value is dropped so the interface falls back to
        // its per-type DEFAULT_IFAC_SIZE (Reticulum.py:747-750). Mirror that
        // guard exactly, otherwise a sub-8-bit value would round down to 0
        // bytes and silently disable IFAC where Python keeps it enabled.
        "ifac_size" => {
            iface.ifac_size = value
                .parse::<usize>()
                .ok()
                .filter(|&bits| bits >= 8)
                .map(|bits| bits / 8)
        }
        // Unknown per-interface key: log and ignore. An unrecognised key (a
        // Backbone-only knob like `prioritise`, an IFAC field, a kernel device
        // bind, id_callsign, modulation, ...) must never make lnsd reject an
        // interface a current rnsd would accept.
        _ => {
            tracing::debug!("Ignoring unknown interface key '{}' = '{}'", key, value);
        }
    }
}

/// Map a `BackboneInterface` / `BackboneClientInterface` config onto our TCP
/// interface, mirroring Python's normalization + mode dispatch
/// (`Reticulum.py:960-972`). BackboneInterface is wire-identical to TCPInterface
/// (HDLC-over-TCP, same FLAG/ESC framing, no handshake), so the resulting
/// `InterfaceConfig` is indistinguishable from the equivalent
/// `TCPServerInterface` / `TCPClientInterface` and the rest of the stack is
/// untouched. A non-Backbone interface passes through unchanged.
fn normalize_backbone_interface(iface: &mut InterfaceConfig) {
    match iface.interface_type.as_str() {
        "BackboneInterface" | "BackboneClientInterface" => {}
        _ => return,
    }

    // `port` -> both listen_port and target_port (Reticulum.py:961-962). On a
    // Backbone entry `port` is a TCP port number, not a serial device path, so
    // consume `iface.port` here. `remote` (-> target_host) and `listen_on`
    // (-> listen_ip) were already applied during the key scan.
    if let Some(p) = iface
        .port
        .as_deref()
        .and_then(|s| s.trim().parse::<u16>().ok())
    {
        iface.listen_port = Some(p);
        iface.target_port = Some(p);
        iface.port = None;
    }

    // Mode dispatch (Reticulum.py:966-972): a BackboneClientInterface is always a
    // client; a plain BackboneInterface with a target host is a client, otherwise
    // it listens as a server.
    let is_client =
        iface.interface_type == "BackboneClientInterface" || iface.target_host.is_some();
    iface.interface_type = if is_client {
        "TCPClientInterface".to_string()
    } else {
        "TCPServerInterface".to_string()
    };
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
    fn test_parse_ifac_keys() {
        // network_name + passphrase + ifac_size (in BITS) parse into the
        // interface config; ifac_size is stored in BYTES.
        let config = parse_ini(
            r#"
[interfaces]
  [[Secure TCP]]
    type = TCPClientInterface
    target_host = 127.0.0.1
    target_port = 4242
    network_name = mynet
    passphrase = s3cret
    ifac_size = 128
"#,
        )
        .unwrap();

        let iface = config.interfaces.get("Secure TCP").expect("iface");
        assert_eq!(iface.networkname.as_deref(), Some("mynet"));
        assert_eq!(iface.passphrase.as_deref(), Some("s3cret"));
        // 128 bits / 8 = 16 bytes
        assert_eq!(iface.ifac_size, Some(16));
    }

    #[test]
    fn test_parse_pipe_interface() {
        // A PipeInterface config maps `command` + `respawn_delay` onto the
        // interface, is recognised as a supported type, and survives the
        // filter (Codeberg #95).
        let config = parse_ini(
            r#"
[interfaces]
  [[My Pipe]]
    type = PipeInterface
    command = /usr/bin/socat - TCP:host:4242
    respawn_delay = 3
"#,
        )
        .unwrap();

        let iface = config.interfaces.get("My Pipe").expect("pipe iface");
        assert_eq!(iface.interface_type, "PipeInterface");
        assert_eq!(
            iface.command.as_deref(),
            Some("/usr/bin/socat - TCP:host:4242")
        );
        assert_eq!(iface.respawn_delay, Some(3.0));
    }

    #[test]
    fn test_parse_ifac_alt_spellings() {
        // The Python config accepts both `networkname`/`network_name` and
        // `passphrase`/`pass_phrase`. Both spellings must populate the same
        // field so a drop-in rnsd config authenticates identically.
        let config = parse_ini(
            r#"
[interfaces]
  [[Alt Spelling]]
    type = TCPClientInterface
    target_host = 127.0.0.1
    target_port = 4242
    networkname = altnet
    pass_phrase = altpass
"#,
        )
        .unwrap();

        let iface = config.interfaces.get("Alt Spelling").expect("iface");
        assert_eq!(iface.networkname.as_deref(), Some("altnet"));
        assert_eq!(iface.passphrase.as_deref(), Some("altpass"));
    }

    #[test]
    fn test_parse_ifac_size_below_min_dropped() {
        // Python drops an ifac_size below IFAC_MIN_SIZE*8 (= 8 bits) so the
        // interface falls back to its per-type default. We must do the same:
        // a value of 4 bits must NOT round down to 0 bytes.
        let config = parse_ini(
            r#"
[interfaces]
  [[Tiny IFAC]]
    type = TCPClientInterface
    target_host = 127.0.0.1
    target_port = 4242
    network_name = mynet
    ifac_size = 4
"#,
        )
        .unwrap();

        let iface = config.interfaces.get("Tiny IFAC").expect("iface");
        assert_eq!(
            iface.ifac_size, None,
            "sub-8-bit ifac_size must be dropped, not stored as 0 bytes"
        );
        // A valid IFAC config is still derivable from network_name alone, using
        // the interface default size (16 bytes for TCP).
        let ifac = leviculum_core::ifac::IfacConfig::new(iface.networkname.as_deref(), None, 16);
        assert!(ifac.is_ok());
    }

    #[test]
    fn test_parse_ifac_config_builds_expected_identity() {
        // The parsed keys must derive the SAME IFAC identity as constructing
        // the IfacConfig directly from those values -- this is what the driver
        // does when it enforces IFAC, so it pins the config-to-crypto path.
        let config = parse_ini(
            r#"
[interfaces]
  [[Secure]]
    type = TCPClientInterface
    target_host = 127.0.0.1
    target_port = 4242
    network_name = mynet
    passphrase = s3cret
    ifac_size = 128
"#,
        )
        .unwrap();
        let iface = config.interfaces.get("Secure").expect("iface");

        let from_parsed = leviculum_core::ifac::IfacConfig::new(
            iface.networkname.as_deref(),
            iface.passphrase.as_deref(),
            iface.ifac_size.unwrap(),
        )
        .expect("valid IFAC config");
        let direct = leviculum_core::ifac::IfacConfig::new(Some("mynet"), Some("s3cret"), 16)
            .expect("valid IFAC config");

        assert_eq!(from_parsed.ifac_size(), 16);
        assert_eq!(
            from_parsed.identity().hash(),
            direct.identity().hash(),
            "parsed IFAC keys must derive the canonical IFAC identity"
        );
    }

    #[test]
    fn test_parse_announce_rate_keys() {
        // Codeberg #67 Stage 2a: the three announce_rate_* keys parse into the
        // interface config; an interface that omits them leaves them None.
        let config = parse_ini(
            r#"
[reticulum]
  enable_transport = True

[interfaces]
  [[Rated TCP]]
    type = TCPClientInterface
    target_host = 127.0.0.1
    target_port = 4243
    announce_rate_target = 7200
    announce_rate_penalty = 30
    announce_rate_grace = 2

  [[Plain TCP]]
    type = TCPClientInterface
    target_host = 127.0.0.1
    target_port = 4244
"#,
        )
        .unwrap();

        let rated = config.interfaces.get("Rated TCP").expect("rated");
        assert_eq!(rated.announce_rate_target, Some(7200));
        assert_eq!(rated.announce_rate_penalty, Some(30));
        assert_eq!(rated.announce_rate_grace, Some(2));

        let plain = config.interfaces.get("Plain TCP").expect("plain");
        assert_eq!(plain.announce_rate_target, None);
        assert_eq!(plain.announce_rate_penalty, None);
        assert_eq!(plain.announce_rate_grace, None);
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
    fn test_remote_management_default_off() {
        let config = parse_ini("[reticulum]\n").unwrap();
        assert!(!config.reticulum.remote_management_enabled);
        assert!(config.reticulum.remote_management_allowed.is_empty());
    }

    #[test]
    fn test_remote_management_python_keys() {
        // The upstream rnsd spelling `enable_remote_management` plus the
        // comma-separated `remote_management_allowed` list (ConfigObj as_list).
        let config = parse_ini(
            r#"
[reticulum]
  enable_remote_management = Yes
  remote_management_allowed = aabbccddeeff00112233445566778899, 00112233445566778899aabbccddeeff
"#,
        )
        .unwrap();
        assert!(config.reticulum.remote_management_enabled);
        assert_eq!(
            config.reticulum.remote_management_allowed,
            vec![
                "aabbccddeeff00112233445566778899".to_string(),
                "00112233445566778899aabbccddeeff".to_string(),
            ]
        );
    }

    #[test]
    fn test_remote_management_enabled_alias() {
        // `remote_management_enabled` is accepted as a Leviculum-side alias.
        let config = parse_ini("[reticulum]\n  remote_management_enabled = True\n").unwrap();
        assert!(config.reticulum.remote_management_enabled);
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

    // --- Backbone drop-in (Codeberg #89) -----------------------------------
    // BackboneInterface is wire-identical to TCPInterface; a stock rnsd config
    // that uses `type = BackboneInterface` must load unchanged and normalize
    // onto our TCP interface exactly as Python does (Reticulum.py:960-972).

    #[test]
    fn test_backbone_interface_server_listen() {
        // `type = BackboneInterface` WITHOUT a target host -> TCP server.
        // `port` -> listen_port, `listen_on` -> listen_ip.
        let config = parse_ini(
            r#"
[interfaces]
  [[Backbone Listen]]
    type = BackboneInterface
    listen_on = 0.0.0.0
    port = 4242
"#,
        )
        .unwrap();

        let iface = config.interfaces.get("Backbone Listen").expect("iface");
        assert_eq!(
            iface.interface_type, "TCPServerInterface",
            "a Backbone entry with no target host must become a TCP server"
        );
        assert_eq!(iface.listen_ip, Some("0.0.0.0".to_string()));
        assert_eq!(iface.listen_port, Some(4242));
        // `port` is a TCP port here, not a serial device path -> consumed.
        assert_eq!(iface.port, None);
        assert_eq!(iface.target_host, None);
    }

    #[test]
    fn test_backbone_interface_client_via_remote() {
        // `type = BackboneInterface` WITH `remote` -> TCP client.
        // `port` -> target_port, `remote` -> target_host.
        let config = parse_ini(
            r#"
[interfaces]
  [[Backbone Uplink]]
    type = BackboneInterface
    remote = backbone.example.com
    port = 4965
"#,
        )
        .unwrap();

        let iface = config.interfaces.get("Backbone Uplink").expect("iface");
        assert_eq!(
            iface.interface_type, "TCPClientInterface",
            "a Backbone entry with a remote host must become a TCP client"
        );
        assert_eq!(iface.target_host, Some("backbone.example.com".to_string()));
        assert_eq!(iface.target_port, Some(4965));
        assert_eq!(iface.port, None);
    }

    #[test]
    fn test_backbone_client_interface_always_client() {
        // `type = BackboneClientInterface` is always a client, even though the
        // reference config below only carries `remote`/`port`.
        let config = parse_ini(
            r#"
[interfaces]
  [[Backbone Client]]
    type = BackboneClientInterface
    remote = 127.0.0.1
    port = 7822
"#,
        )
        .unwrap();

        let iface = config.interfaces.get("Backbone Client").expect("iface");
        assert_eq!(iface.interface_type, "TCPClientInterface");
        assert_eq!(iface.target_host, Some("127.0.0.1".to_string()));
        assert_eq!(iface.target_port, Some(7822));
    }

    #[test]
    fn test_backbone_uses_explicit_tcp_keys() {
        // A Backbone entry may also carry the literal TCP keys instead of the
        // `port`/`remote`/`listen_on` aliases; presence of target_host still
        // dispatches to client (Reticulum.py:967).
        let config = parse_ini(
            r#"
[interfaces]
  [[Backbone Explicit]]
    type = BackboneInterface
    target_host = 10.0.0.1
    target_port = 4242
"#,
        )
        .unwrap();

        let iface = config.interfaces.get("Backbone Explicit").expect("iface");
        assert_eq!(iface.interface_type, "TCPClientInterface");
        assert_eq!(iface.target_host, Some("10.0.0.1".to_string()));
        assert_eq!(iface.target_port, Some(4242));
    }

    #[test]
    fn test_stock_rnsd_default_config_loads() {
        // Config drop-in audit (Codeberg #89): the verbatim stock rnsd default
        // config (Reticulum.py __default_rns_config__) must load unchanged --
        // known [reticulum] keys take effect, [logging] is ignored, the default
        // AutoInterface is accepted.
        let config = parse_ini(
            r#"[reticulum]
enable_transport = False
share_instance = Yes
instance_name = default

[logging]
loglevel = 4

[interfaces]
  [[Default Interface]]
    type = AutoInterface
    enabled = Yes
"#,
        )
        .unwrap();

        assert!(!config.reticulum.enable_transport);
        assert!(config.reticulum.shared_instance);
        assert_eq!(config.reticulum.instance_name, "default");
        let auto = config.interfaces.get("Default Interface").expect("auto");
        assert_eq!(auto.interface_type, "AutoInterface");
        assert!(auto.enabled);
    }

    #[test]
    fn test_example_config_interface_type_coverage() {
        // Config drop-in audit (Codeberg #89): `rnsd --exampleconfig` documents
        // eight interface types. We ACCEPT Auto/UDP/TCP*/RNode; I2P/KISS/AX25
        // are not implemented yet and are SKIPPED (logged, not rejected). No key
        // on any block causes the config to fail to load.
        let config = parse_ini(
            r#"
[interfaces]
  [[Default Interface]]
    type = AutoInterface
    enabled = Yes
  [[UDP Interface]]
    type = UDPInterface
    enabled = no
    listen_ip = 0.0.0.0
    listen_port = 4242
    forward_ip = 255.255.255.255
    forward_port = 4242
  [[TCP Server Interface]]
    type = TCPServerInterface
    enabled = no
    listen_ip = 0.0.0.0
    listen_port = 4242
    device = eth0
    connectable = yes
  [[TCP Client Interface]]
    type = TCPClientInterface
    enabled = no
    target_host = 127.0.0.1
    target_port = 4242
  [[I2P]]
    type = I2PInterface
    enabled = no
    connectable = yes
    peers = ykzlw5ujbaqc2xkec4cpvgyxj257wcrmmgkuxqmqcur7cq3w3lha.b32.i2p
  [[RNode LoRa Interface]]
    type = RNodeInterface
    enabled = no
    port = /dev/ttyUSB0
    frequency = 867200000
    bandwidth = 125000
    txpower = 7
    spreadingfactor = 8
    codingrate = 5
    id_callsign = MYCALL-0
    id_interval = 600
  [[Packet Radio KISS Interface]]
    type = KISSInterface
    enabled = no
    port = /dev/ttyUSB1
    speed = 115200
    databits = 8
    parity = none
    stopbits = 1
    preamble = 150
    txtail = 10
    persistence = 200
    slottime = 20
  [[Packet Radio AX.25 KISS Interface]]
    type = AX25KISSInterface
    enabled = no
    callsign = MYCALL
    ssid = 0
    port = /dev/ttyUSB2
    speed = 115200
    databits = 8
    parity = none
    stopbits = 1
"#,
        )
        .unwrap();

        // Accepted interface types survive the load.
        for accepted in [
            "Default Interface",
            "UDP Interface",
            "TCP Server Interface",
            "TCP Client Interface",
            "RNode LoRa Interface",
        ] {
            assert!(
                config.interfaces.contains_key(accepted),
                "{accepted} must be accepted"
            );
        }
        // Not-yet-implemented interface types are skipped, not rejected outright.
        for skipped in [
            "I2P",
            "Packet Radio KISS Interface",
            "Packet Radio AX.25 KISS Interface",
        ] {
            assert!(
                !config.interfaces.contains_key(skipped),
                "{skipped} is unimplemented and must be skipped"
            );
        }
    }

    #[test]
    fn test_interface_enabled_legacy_alias() {
        // Config drop-in audit fix: the legacy `interface_enabled` key must
        // disable an interface just like `enabled` (Reticulum.py:950).
        let config = parse_ini(
            r#"
[interfaces]
  [[Legacy Off]]
    type = TCPServerInterface
    interface_enabled = No
    listen_port = 4242
"#,
        )
        .unwrap();
        let iface = config.interfaces.get("Legacy Off").expect("iface");
        assert!(
            !iface.enabled,
            "interface_enabled = No must disable the interface"
        );
    }

    #[test]
    fn test_backbone_with_unknown_extra_keys_accepted() {
        // Backbone-only knobs (prioritise, network_name, ifac_*, device binds)
        // must not reject the interface -- they are logged and ignored.
        let config = parse_ini(
            r#"
[interfaces]
  [[Backbone Rich]]
    type = BackboneInterface
    listen_on = 0.0.0.0
    port = 4242
    prioritise = eth0
    ifac_netname = mynet
    ifac_netkey = supersecret
    some_future_backbone_knob = 1234
"#,
        )
        .unwrap();

        let iface = config.interfaces.get("Backbone Rich").expect("iface");
        assert_eq!(iface.interface_type, "TCPServerInterface");
        assert_eq!(iface.listen_port, Some(4242));
        assert_eq!(iface.listen_ip, Some("0.0.0.0".to_string()));
    }
}
