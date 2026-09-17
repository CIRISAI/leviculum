//! Every key a public backbone node's `rnsd` config carries, pinned to what
//! `lnsd`'s parser does with it.
//!
//! This is the acceptance list for taking `leviculum.network` over from
//! Python `rnsd`: a transport instance with a discoverable `BackboneInterface`
//! listener and two `bootstrap_only` seed clients. The point is not that the
//! file parses — it is that each key lands somewhere observable, so the
//! difference between "we ignore this and say so" and "we ignore this
//! quietly" is a test failure rather than a surprise on a public node.
//!
//! `listen_on`/`port` and the seed targets are the shapes of the live file,
//! with loopback substituted for the real seed hosts: nothing here connects.

use leviculum_std::config::Config;

/// Write `text` into a fresh temp directory as `config` and load it the way
/// `lnsd --config <dir>` does, extension-free so the `[[` detection picks
/// the INI path.
fn load(text: &str) -> Config {
    let dir = tempfile::tempdir().expect("temp config dir");
    let path = dir.path().join("config");
    std::fs::write(&path, text).expect("write config");
    Config::load(&path).expect("the config loads")
}

/// The production shape. Hostnames and seed addresses substituted; every key
/// and every value that drives behaviour is the live one.
const PUBLIC_NODE_CONFIG: &str = r#"
[reticulum]
  enable_transport = True
  share_instance = Yes
  instance_name = default
  discover_interfaces = yes
  autoconnect_discovered_interfaces = 3

[logging]
  loglevel = 4

[interfaces]

  [[Backbone Listener]]
    type = BackboneInterface
    enabled = yes
    listen_on = 0.0.0.0
    port = 4242
    discoverable = yes
    reachable_on = leviculum.network
    discovery_name = leviculum.network
    announce_interval = 360

  [[Seed A]]
    type = TCPClientInterface
    enabled = yes
    target_host = 127.0.0.1
    target_port = 14242
    bootstrap_only = yes

  [[Seed B]]
    type = TCPClientInterface
    enabled = yes
    target_host = 127.0.0.1
    target_port = 14243
    bootstrap_only = yes
"#;

fn parse() -> Config {
    load(PUBLIC_NODE_CONFIG)
}

#[test]
fn the_reticulum_section_lands_key_for_key() {
    let config = parse();
    let r = &config.reticulum;
    assert!(r.enable_transport, "enable_transport = True");
    assert!(r.shared_instance, "share_instance = Yes");
    assert_eq!(r.instance_name, "default");
    assert!(r.discover_interfaces, "discover_interfaces = yes");
    assert_eq!(
        r.autoconnect_discovered_interfaces, 3,
        "autoconnect_discovered_interfaces is both the switch and the cap"
    );
    assert_eq!(r.loglevel, Some(4), "[logging] loglevel = 4");
}

/// `discover_interfaces = no` has to be honoured, not just tolerated: the
/// whole point of parsing it is that an operator who turns collection off
/// gets it off. Paired with the positive case above, this is the control.
#[test]
fn discover_interfaces_no_is_honoured() {
    let config = load("[reticulum]\ndiscover_interfaces = no\n[interfaces]\n  [[x]]\n    type = TCPClientInterface\n    target_host = 127.0.0.1\n    target_port = 14242\n");
    assert!(!config.reticulum.discover_interfaces);
}

#[test]
fn the_backbone_listener_becomes_a_discoverable_tcp_server() {
    let config = parse();
    let iface = &config.interfaces["Backbone Listener"];
    // Backbone is HDLC-over-TCP; a Backbone entry without `remote` listens.
    assert_eq!(iface.interface_type, "TCPServerInterface");
    assert!(iface.enabled);
    assert_eq!(iface.listen_ip.as_deref(), Some("0.0.0.0"), "listen_on");
    assert_eq!(iface.listen_port, Some(4242), "port");
    assert!(iface.discoverable, "discoverable = yes");
    assert_eq!(iface.reachable_on.as_deref(), Some("leviculum.network"));
    assert_eq!(iface.discovery_name.as_deref(), Some("leviculum.network"));
    // Python reads `announce_interval` on a discoverable interface in
    // MINUTES (`as_int(...)*60`, Reticulum.py:852-854). Stored verbatim;
    // the conversion happens where the announce job is built.
    assert_eq!(iface.announce_interval, Some(360));
    // The file says nothing about `mode`, and Python's rnsd still runs this
    // listener as a gateway: `discoverable` without gateway/AP mode is raised
    // to gateway (Reticulum.py:869-876). It is not cosmetic — a `Full`
    // interface does not re-originate a path request for a destination it has
    // never seen (`InterfaceMode::discovers_paths`, Codeberg #104), which is
    // what a public hub is asked for all day. lnstatus showed Full here on
    // 2026-09-17 where the rnsd it replaced showed Gateway.
    assert_eq!(
        iface
            .mode
            .as_deref()
            .and_then(leviculum_core::traits::InterfaceMode::from_config_str),
        Some(leviculum_core::traits::InterfaceMode::Gateway),
        "a discoverable listener with no configured mode runs as a gateway"
    );
}

#[test]
fn both_seeds_are_tcp_clients_carrying_bootstrap_only() {
    let config = parse();
    for (name, port) in [("Seed A", 14242), ("Seed B", 14243)] {
        let iface = &config.interfaces[name];
        assert_eq!(iface.interface_type, "TCPClientInterface");
        assert!(iface.enabled);
        assert_eq!(iface.target_host.as_deref(), Some("127.0.0.1"));
        assert_eq!(iface.target_port, Some(port));
        // Parsed, so the daemon can say out loud that it does not act on it.
        // A silently dropped `bootstrap_only` is a seed connection that is
        // never torn down and that nothing in the log admits to.
        assert!(iface.bootstrap_only, "{name}: bootstrap_only = yes");
    }
}

/// The interface-key catch-all logs at DEBUG. An rnsd-shaped config runs at
/// `loglevel = 4` (info), where DEBUG is not emitted — so a key that only
/// reaches that catch-all is, in production, silent. This test exists to
/// pin the two keys that were found there and moved out of it; it fails if
/// either falls back in.
#[test]
fn the_two_keys_that_must_not_be_silent_have_homes() {
    let config = parse();
    assert!(
        config.interfaces["Seed A"].bootstrap_only,
        "bootstrap_only must be parsed, not swallowed by the unknown-key arm"
    );
    assert!(
        config.reticulum.discover_interfaces,
        "discover_interfaces must be parsed, not swallowed by the tolerated-key arm"
    );
}
