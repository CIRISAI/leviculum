//! The daemon's interface inventory as a monitoring client sees it
//! (Codeberg #177).
//!
//! ## Why this exists next to transport's interface map
//!
//! `Transport` keeps the interfaces it can *route packets on*: every entry in
//! `Transport::interface_names` is a send target, and the broadcast paths
//! iterate exactly that map. A listener is not a send target — a
//! `TCPServerInterface` accepts connections, a shared-instance server accepts
//! IPC clients, and neither carries a packet itself — so listeners must never
//! enter that map.
//!
//! Python-RNS has no such split: `RNS.Transport.interfaces` holds everything
//! `Reticulum` runs, listeners included, and `Reticulum.get_interface_stats`
//! reports that list (Reticulum.py:1334). Reporting only the routing map
//! therefore answers a monitoring query about a *different* collection than
//! the one the daemon runs interfaces out of, which is how three of the four
//! rows an `rnsd` shows went missing from `lnsd` entirely.
//!
//! This inventory is the reporting-side collection: the listeners the daemon
//! runs, plus the presentation identity (Python `str(interface)` /
//! `interface.name` / parent link) of the connections they spawn. It is owned
//! by the driver, never by the core, so the routing layer stays free of
//! listener rows and free of any awareness of a carrier medium.
//!
//! ## Naming
//!
//! The name is the interface's identity to a script, so the display names
//! below reproduce the reference `__str__` implementations exactly:
//!
//! | Row | Python `__str__` | Reference |
//! |---|---|---|
//! | shared-instance server | `Shared Instance[rns/<instance>]` | LocalInterface.py:496-498 |
//! | its accepted IPC clients | `LocalInterface[rns/<instance>]` | LocalInterface.py:372-374 |
//! | TCP listener | `TCPServerInterface[<section>/<ip>:<port>]` | TCPInterface.py:666-672 |
//! | its accepted connections | `TCPInterface[Client on <section>/<ip>:<port>]` | TCPInterface.py:443-449, 577 |

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use leviculum_core::traits::InterfaceMode;

use crate::auto_layer::AutoLayer;
use crate::config::InterfaceOrigin;

/// Python renders an IPv6 literal in brackets and an IPv4 literal bare
/// (TCPInterface.py:444-448 / 667-671).
fn ip_str(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    }
}

/// `TCPServerInterface[<section>/<ip>:<port>]` (TCPInterface.py:672).
pub(crate) fn tcp_listener_name(section: &str, bind: SocketAddr) -> String {
    format!(
        "TCPServerInterface[{}/{}:{}]",
        section,
        ip_str(bind.ip()),
        bind.port()
    )
}

/// `TCPInterface[Client on <section>/<ip>:<port>]`: the spawned interface is a
/// `TCPClientInterface` named `"Client on "+parent.name`
/// (TCPInterface.py:577), rendered by the client `__str__`
/// (TCPInterface.py:449) with the *peer's* address.
pub(crate) fn tcp_spawned_name(section: &str, peer: SocketAddr) -> String {
    format!(
        "TCPInterface[Client on {}/{}:{}]",
        section,
        ip_str(peer.ip()),
        peer.port()
    )
}

/// `TCPInterface[<section>/<host>:<port>]`, a configured TCP client: the
/// client `__str__` (TCPInterface.py:443-449) over the `target_host` string as
/// configured, bracketed when it carries a colon.
pub(crate) fn tcp_client_name(section: &str, host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("TCPInterface[{section}/[{host}]:{port}]")
    } else {
        format!("TCPInterface[{section}/{host}:{port}]")
    }
}

/// `Client on <section>`, the spawned interface's `short_name`
/// (TCPInterface.py:577).
pub(crate) fn tcp_spawned_short_name(section: &str) -> String {
    format!("Client on {section}")
}

/// `Shared Instance[rns/<instance>]` (LocalInterface.py:497). `socket` is the
/// abstract socket name without the leading NUL, exactly what Python prints
/// after its `replace("\0", "")`.
pub(crate) fn shared_instance_name(socket: &str) -> String {
    format!("Shared Instance[{socket}]")
}

/// `LocalInterface[rns/<instance>]` (LocalInterface.py:373). Every accepted
/// IPC client renders the same name in the reference, because the client
/// `__str__` prints the shared socket path, not the per-client label.
pub(crate) fn local_client_name(socket: &str) -> String {
    format!("LocalInterface[{socket}]")
}

/// `<n>@\0rns/<instance>`, the accepted IPC client's `short_name`
/// (LocalInterface.py:441-443). The NUL is part of the reference string: the
/// label is built from the raw abstract socket path, and only `__str__`
/// strips it.
pub(crate) fn local_client_short_name(index: usize, socket: &str) -> String {
    format!("{index}@\0{socket}")
}

/// Presentation identity of one reported interface row.
#[derive(Debug, Clone)]
pub(crate) struct InterfaceIdentity {
    /// Python `str(interface)`, the `name` field of `interface_stats`.
    pub name: String,
    /// Python `interface.name`, the `short_name` field.
    pub short_name: String,
    /// Python `type(interface).__name__`, the `type` field.
    pub type_name: &'static str,
    /// Inventory id of the listener that spawned this interface, if any.
    /// Drives the `parent_interface_name` / `parent_interface_hash` keys
    /// Python emits for spawned interfaces (Reticulum.py:1342-1344).
    pub parent: Option<usize>,
}

/// A listener the daemon runs: it appears in the reported inventory but never
/// in transport's routing map, because it carries no packets of its own.
#[derive(Debug, Clone)]
pub(crate) struct ListenerRow {
    pub identity: InterfaceIdentity,
    /// Python `interface.bitrate` (`TCPServerInterface.BITRATE_GUESS` /
    /// `LocalServerInterface` 1 Gbps, LocalInterface.py:431).
    pub bitrate: i64,
    /// Python `interface.HW_MTU`, the `mtu` stats key: 262144 for the
    /// shared-instance server (`HW_MTU`, LocalInterface.py:71), and for a TCP
    /// listener whatever `optimise_mtu()` derives from its bitrate — 16384 on
    /// the Reticulum 1.5.2 the nightly containers run, 8192 on 1.3.5, never
    /// the `TCPInterface.HW_MTU` class value (Codeberg #355).
    pub hw_mtu: i64,
    pub mode: InterfaceMode,
    /// `announce_rate_target/penalty/grace`. The shared-instance server pins
    /// all three to `None` (LocalInterface.py:427-429); a configured listener
    /// carries the resolved config values like any other config interface
    /// (Reticulum.py:830-833).
    pub announce_rate: (Option<u32>, Option<u32>, Option<u32>),
    /// `ifac_size` in bits, or `None` when IFAC is off for this listener.
    pub ifac_size_bits: Option<i64>,
    /// Bytes carried by children that have since disconnected. Python keeps
    /// them because the parent counter is incremented alongside the child's
    /// (TCPInterface.py:306-308/327-329) and outlives the child; ours has to
    /// bank them explicitly when a child goes away.
    pub departed_rxb: u64,
    pub departed_txb: u64,
    /// The socket address the listener actually bound, when it listens on an
    /// IP socket. For a listener configured with port 0 this carries the
    /// kernel-assigned port, which is how a caller learns it (Codeberg #221);
    /// `None` for non-IP listeners (the shared-instance Unix socket).
    pub bound_addr: Option<SocketAddr>,
    /// The TCP server's admission tracker (1190), which `lnstatus` reads its
    /// connection counters from; `None` for every other listener.
    pub admission: Option<Arc<Mutex<crate::interfaces::tcp_admission::ConnTracker>>>,
}

/// The reporting-side interface inventory (see the module docs).
#[derive(Debug, Default)]
pub(crate) struct InterfaceInventory {
    /// Listener rows by inventory id. Ids come from the same allocator as
    /// interface ids, so a listener id can never collide with an interface id
    /// and the natural ordering stays "in the order the daemon started them".
    listeners: BTreeMap<usize, ListenerRow>,
    /// Presentation identity of spawned interfaces, and of configured
    /// interfaces whose reference name the driver's internal one does not
    /// carry, by interface id.
    spawned: BTreeMap<usize, InterfaceIdentity>,
    /// The shared-instance server's inventory id, reported first to match the
    /// reference order (Python starts the local interface before the config
    /// interfaces, Reticulum.py:696).
    shared_instance: Option<usize>,
    /// The origin of every configured section, by section name (#471): what
    /// a row reports as `origin`.
    section_origins: BTreeMap<String, InterfaceOrigin>,
    /// The automatic layer's decision at start (#471 step 2), served as the
    /// top-level `auto_layer` key; `None` for a node built in code.
    auto_layer: Option<AutoLayer>,
    /// Where the configuration came from, a file or the default lnsd holds
    /// without one (#471): the origin of a row no section names when no
    /// section is left. Configured for a node built in code.
    config_origin: InterfaceOrigin,
}

/// Shared handle: the accept loops write, the stats builder reads.
pub(crate) type SharedInventory = Arc<Mutex<InterfaceInventory>>;

impl InterfaceInventory {
    pub(crate) fn shared() -> SharedInventory {
        Arc::new(Mutex::new(InterfaceInventory::default()))
    }

    pub(crate) fn add_listener(&mut self, id: usize, row: ListenerRow) {
        if row.identity.type_name == "LocalServerInterface" {
            self.shared_instance = Some(id);
        }
        self.listeners.insert(id, row);
    }

    pub(crate) fn add_spawned(&mut self, id: usize, identity: InterfaceIdentity) {
        self.spawned.insert(id, identity);
    }

    /// Drop a spawned interface and bank its byte counters on its parent, so a
    /// listener's totals do not fall when a client disconnects.
    pub(crate) fn remove_spawned(&mut self, id: usize, rxb: u64, txb: u64) {
        if let Some(identity) = self.spawned.remove(&id) {
            if let Some(parent) = identity.parent.and_then(|p| self.listeners.get_mut(&p)) {
                parent.departed_rxb += rxb;
                parent.departed_txb += txb;
            }
        }
    }

    pub(crate) fn identity(&self, id: usize) -> Option<&InterfaceIdentity> {
        self.spawned.get(&id)
    }

    pub(crate) fn listeners(&self) -> impl Iterator<Item = (usize, &ListenerRow)> {
        self.listeners.iter().map(|(&id, row)| (id, row))
    }

    /// Record where a configured section came from, before its interfaces
    /// are built.
    pub(crate) fn set_section_origin(&mut self, section: &str, origin: InterfaceOrigin) {
        self.section_origins.insert(section.to_string(), origin);
    }

    /// Record the automatic layer's decision, once, before the node runs.
    pub(crate) fn set_auto_layer(&mut self, layer: Option<AutoLayer>) {
        self.auto_layer = layer;
    }

    /// Record where the configuration came from, once, before the node runs.
    pub(crate) fn set_config_origin(&mut self, origin: InterfaceOrigin) {
        self.config_origin = origin;
    }

    /// The automatic layer's decision, if this node took one.
    pub(crate) fn auto_layer(&self) -> Option<AutoLayer> {
        self.auto_layer
    }

    /// The origin a row reports. A row whose short name is a section's name,
    /// or that a listener built from a section spawned, has that section's
    /// origin. Every other row (the shared instance, its local clients, an
    /// AutoInterface's peers, whose names carry no section) comes from the
    /// `[reticulum]` and `[interfaces]` of the same configuration, so it
    /// takes the origin all sections share; with sections of both origins it
    /// was configured, and with no section left (lnsd without a file, its
    /// AutoInterface removed by the automatic layer) it takes the
    /// configuration's own origin.
    pub(crate) fn origin_of(&self, short_name: &str, parent: Option<usize>) -> InterfaceOrigin {
        let parent_section = parent
            .and_then(|p| self.listeners.get(&p))
            .map(|l| l.identity.short_name.as_str());
        if let Some(origin) = self
            .section_origins
            .get(short_name)
            .or_else(|| parent_section.and_then(|s| self.section_origins.get(s)))
        {
            return *origin;
        }
        let mut origins = self.section_origins.values();
        match origins.next() {
            None => self.config_origin,
            Some(&first) if origins.all(|&o| o == first) => first,
            Some(_) => InterfaceOrigin::Configured,
        }
    }

    /// Reporting order: the shared-instance server first (the reference starts
    /// it before any config interface), everything else by id.
    pub(crate) fn sort_key(&self, id: usize) -> (u8, usize) {
        if self.shared_instance == Some(id) {
            (0, id)
        } else {
            (1, id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names measured against a live `rnsd` (reference 1.3.5) carrying one
    /// `[[Parity TCP Server 0]]` section on 127.0.0.1:45911 with
    /// `instance_name = inv_probe_1`.
    #[test]
    fn display_names_match_the_reference() {
        assert_eq!(
            tcp_client_name("Upstream Link", "127.0.0.1", 4242),
            "TCPInterface[Upstream Link/127.0.0.1:4242]"
        );
        assert_eq!(
            tcp_client_name("v6", "fe80::1", 4242),
            "TCPInterface[v6/[fe80::1]:4242]"
        );
        assert_eq!(
            tcp_client_name("named", "example.org", 4242),
            "TCPInterface[named/example.org:4242]"
        );
        assert_eq!(
            tcp_listener_name("Parity TCP Server 0", "127.0.0.1:45911".parse().unwrap()),
            "TCPServerInterface[Parity TCP Server 0/127.0.0.1:45911]"
        );
        assert_eq!(
            tcp_spawned_name("Parity TCP Server 0", "127.0.0.1:51262".parse().unwrap()),
            "TCPInterface[Client on Parity TCP Server 0/127.0.0.1:51262]"
        );
        assert_eq!(
            tcp_spawned_short_name("Parity TCP Server 0"),
            "Client on Parity TCP Server 0"
        );
        assert_eq!(
            shared_instance_name("rns/inv_probe_1"),
            "Shared Instance[rns/inv_probe_1]"
        );
        assert_eq!(
            local_client_name("rns/inv_probe_1"),
            "LocalInterface[rns/inv_probe_1]"
        );
        assert_eq!(
            local_client_short_name(0, "rns/inv_probe_1"),
            "0@\0rns/inv_probe_1"
        );
    }

    /// IPv6 literals are bracketed, IPv4 literals are not.
    #[test]
    fn ipv6_literals_are_bracketed() {
        assert_eq!(
            tcp_listener_name("v6", "[::1]:4242".parse().unwrap()),
            "TCPServerInterface[v6/[::1]:4242]"
        );
    }

    /// A row takes its section's origin, directly or through its listener;
    /// a row no section names takes the origin the sections share.
    #[test]
    fn rows_take_the_origin_of_their_section() {
        let mut inv = InterfaceInventory::default();
        // No section at all, and a node built in code: configured.
        assert_eq!(
            inv.origin_of("Shared Instance", None),
            InterfaceOrigin::Configured
        );

        inv.set_section_origin("Default Interface", InterfaceOrigin::Default);
        assert_eq!(
            inv.origin_of("Default Interface", None),
            InterfaceOrigin::Default
        );
        // The shared instance and an AutoInterface peer of a default config.
        assert_eq!(inv.origin_of("rns/default", None), InterfaceOrigin::Default);
        assert_eq!(inv.origin_of("fe80::1", None), InterfaceOrigin::Default);

        inv.add_listener(
            3,
            ListenerRow {
                identity: InterfaceIdentity {
                    name: "TCPServerInterface[s/127.0.0.1:1]".into(),
                    short_name: "s".into(),
                    type_name: "TCPServerInterface",
                    parent: None,
                },
                bitrate: 10_000_000,
                hw_mtu: 262_144,
                mode: InterfaceMode::default(),
                announce_rate: (None, None, None),
                ifac_size_bits: None,
                departed_rxb: 0,
                departed_txb: 0,
                bound_addr: None,
                admission: None,
            },
        );
        inv.set_section_origin("s", InterfaceOrigin::Configured);
        assert_eq!(
            inv.origin_of("Client on s", Some(3)),
            InterfaceOrigin::Configured
        );
        assert_eq!(
            inv.origin_of("Default Interface", None),
            InterfaceOrigin::Default
        );
        // Mixed origins: a row no section names was configured.
        assert_eq!(
            inv.origin_of("rns/default", None),
            InterfaceOrigin::Configured
        );
    }

    /// With no section left, a row takes the configuration's origin: lnsd
    /// without a file and with `LEVICULUM_NO_AUTO=1` holds the default
    /// configuration minus its AutoInterface, and its shared instance came
    /// from that default, not from a file (977).
    #[test]
    fn rows_without_sections_take_the_configuration_origin() {
        let mut inv = InterfaceInventory::default();
        inv.set_config_origin(InterfaceOrigin::Default);
        assert_eq!(inv.origin_of("rns/default", None), InterfaceOrigin::Default);

        inv.set_config_origin(InterfaceOrigin::Configured);
        assert_eq!(
            inv.origin_of("rns/default", None),
            InterfaceOrigin::Configured
        );

        // A section still decides over the configuration's origin.
        inv.set_section_origin("Default Interface", InterfaceOrigin::Default);
        assert_eq!(inv.origin_of("rns/default", None), InterfaceOrigin::Default);
    }

    /// A departing child's bytes stay on its parent listener.
    #[test]
    fn departed_children_keep_their_bytes_on_the_listener() {
        let mut inv = InterfaceInventory::default();
        inv.add_listener(
            0,
            ListenerRow {
                identity: InterfaceIdentity {
                    name: "TCPServerInterface[s/127.0.0.1:1]".into(),
                    short_name: "s".into(),
                    type_name: "TCPServerInterface",
                    parent: None,
                },
                bitrate: 10_000_000,
                hw_mtu: 262_144,
                mode: InterfaceMode::default(),
                announce_rate: (None, None, None),
                ifac_size_bits: None,
                departed_rxb: 0,
                departed_txb: 0,
                bound_addr: Some("127.0.0.1:1".parse().unwrap()),
                admission: None,
            },
        );
        inv.add_spawned(
            7,
            InterfaceIdentity {
                name: "TCPInterface[Client on s/127.0.0.1:2]".into(),
                short_name: "Client on s".into(),
                type_name: "TCPClientInterface",
                parent: Some(0),
            },
        );
        inv.remove_spawned(7, 400, 90);
        let row = inv.listeners().next().expect("listener").1;
        assert_eq!((row.departed_rxb, row.departed_txb), (400, 90));
        assert!(inv.identity(7).is_none());
    }
}
