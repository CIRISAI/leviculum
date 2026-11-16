//! Per-entry interface construction, shared by [`initialize_interfaces`] at
//! startup and [`spawn_interface`] at runtime so a type is wired once for both.
//! Each interface family lives in its own submodule; this module holds the
//! shared context and the type dispatch.
//!
//! [`initialize_interfaces`]: super::ReticulumNode
//! [`spawn_interface`]: super::ReticulumNode::spawn_interface

use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::config::InterfaceConfig;
use crate::error::Error;
use crate::interfaces::InterfaceHandle;
use leviculum_core::transport::InterfaceId;

use super::AutoPeerCount;

mod auto;
#[cfg(all(feature = "ble", target_os = "linux"))]
mod ble;
#[cfg(feature = "i2p")]
mod i2p;
#[cfg(feature = "serial")]
mod kiss;
#[cfg(feature = "serial")]
mod pipe;
#[cfg(feature = "serial")]
mod rnode;
#[cfg(feature = "serial")]
mod rnode_multi;
#[cfg(feature = "serial")]
mod serial;
mod tcp;
mod udp;

/// Shared wiring the per-type builders need, so they do not depend on the
/// driver struct itself.
pub(super) struct InterfaceBuildCtx<'a> {
    pub next_id: &'a Arc<AtomicUsize>,
    pub new_iface_tx: &'a mpsc::Sender<InterfaceHandle>,
    pub reconnect_tx: &'a mpsc::Sender<InterfaceId>,
    pub tunnel_notify_tx: &'a mpsc::Sender<InterfaceId>,
    /// Per-peer transition reports (loss + arrival) from multi-peer
    /// interfaces (Codeberg #365); today only the BLE builder hands it
    /// to its interface task — and that builder is Linux-only (it needs
    /// BlueZ over D-Bus) and behind the `ble` feature, so off Linux or
    /// without it this field has no reader and
    /// `-D warnings` would fail the lane.
    #[cfg_attr(not(all(feature = "ble", target_os = "linux")), allow(dead_code))]
    pub peer_event_tx: &'a mpsc::Sender<(InterfaceId, crate::interfaces::PeerEvent)>,
    /// Test-only frame-corruption cadence.
    pub corrupt_every: Option<u64>,
    /// Storage root; I2P persists its per-interface keyfile under it, and
    /// nothing else reads it, so a build without `i2p` has no reader.
    #[cfg_attr(not(feature = "i2p"), allow(dead_code))]
    pub storage_path: Option<PathBuf>,
    /// Applied to each TCP client's connect socket before it dials.
    pub outbound_socket_hook: Option<crate::socket_hook::OutboundSocketHook>,
    /// Resolves each TCP client's target name per attempt; `None` is the
    /// system resolver.
    pub host_resolver: Option<crate::resolver::HostResolver>,
    /// Reporting-side interface inventory (Codeberg #177): listeners register
    /// themselves here because they never become routable interfaces.
    pub inventory: crate::interfaces::inventory::SharedInventory,
    /// Whether transport is enabled, which decides the announce-rate defaults
    /// a listener reports (Reticulum.py:830-833).
    pub transport_enabled: bool,
    /// The node-wide values that fill (`default_ar_*`, Reticulum.py:622-633),
    /// the same the core fills a routable interface from.
    pub default_announce_rate: leviculum_core::transport::AnnounceRateConfig,
    /// The node's 16-byte identity hash. The BLE interface publishes it in
    /// the Columba Identity characteristic, handshakes with it, and derives
    /// its advertised `LN-<hex8>` name from it — the same derivation the
    /// firmware uses (`leviculum_ble_tx::device_name`). The I2P builder
    /// names a connectable interface's key file with it, as the reference
    /// names it with the transport identity's hash.
    #[cfg_attr(
        not(any(feature = "i2p", all(feature = "ble", target_os = "linux"))),
        allow(dead_code)
    )]
    pub identity_hash: [u8; 16],
    /// The node's goodbye count: a radio interface holds a token of it until
    /// its task ends, so the node's stop waits for the goodbye (1567).
    #[cfg_attr(not(feature = "serial"), allow(dead_code))]
    pub goodbyes: crate::interfaces::RadioGoodbyes,
}

/// Outcome of building one configured interface section.
pub(super) enum Built {
    /// Handles the caller registers itself (config init) or dispatches through
    /// `new_iface_tx` (runtime attach).
    Handles(Vec<InterfaceHandle>),
    /// A listener/orchestrator that registered its own children through
    /// `new_iface_tx`, or an unknown type; the caller registers nothing.
    SelfManaged,
}

/// Construct the interface(s) for one section.
///
/// `idx` is the base [`InterfaceId`]: leaf interfaces take it directly (stable
/// across restarts at startup), fan-out children draw further ids from
/// `ctx.next_id`.
pub(super) fn build_interface(
    idx: usize,
    config: &InterfaceConfig,
    ctx: &InterfaceBuildCtx<'_>,
    auto_peer_count: &AutoPeerCount,
) -> Result<Built, Error> {
    // TEST-ONLY `test_drop_direct_ingress` (range emulation for co-located
    // rigs) reads the wire hops byte at `raw[1]`, which only exists on an
    // unwrapped Reticulum frame: IFAC prepends authentication material
    // before the flags byte, so the two cannot coexist. And only the
    // LoRa-capable single-port types implement the filter. Both refused
    // here, centrally, so a misconfiguration fails daemon startup instead
    // of silently running without range emulation.
    if config.test_drop_direct_ingress {
        if !matches!(
            config.interface_type.as_str(),
            "RNodeInterface" | "SerialInterface"
        ) {
            return Err(Error::Config(format!(
                "interface '{}': test_drop_direct_ingress is only supported on \
                 RNodeInterface and SerialInterface, not {}",
                config.name, config.interface_type
            )));
        }
        if config.networkname.is_some() || config.passphrase.is_some() {
            return Err(Error::Config(format!(
                "interface '{}': test_drop_direct_ingress is incompatible with IFAC \
                 (networkname/passphrase): IFAC prepends material before the flags \
                 byte, so the wire hops byte is no longer at raw[1]; remove the IFAC \
                 keys or the test knob",
                config.name
            )));
        }
    }
    match config.interface_type.as_str() {
        "TCPClientInterface" => tcp::build_client(idx, config, ctx),
        "TCPServerInterface" => tcp::build_server(idx, config, ctx),
        "UDPInterface" => udp::build(idx, config, ctx),
        "AutoInterface" => auto::build(config, ctx, auto_peer_count),
        #[cfg(feature = "serial")]
        "RNodeInterface" => rnode::build(idx, config, ctx),
        #[cfg(feature = "serial")]
        "RNodeMultiInterface" => rnode_multi::build(idx, config, ctx),
        #[cfg(feature = "serial")]
        "SerialInterface" => serial::build(idx, config, ctx),
        #[cfg(feature = "serial")]
        "PipeInterface" => pipe::build(idx, config, ctx),
        #[cfg(feature = "serial")]
        "KISSInterface" | "AX25KISSInterface" => kiss::build(idx, config, ctx),
        // A kind this build left out is a configuration error that names the
        // feature, never a silent skip: the operator wrote the section because
        // they expect the interface (#463).
        #[cfg(not(feature = "serial"))]
        kind @ ("RNodeInterface"
        | "RNodeMultiInterface"
        | "SerialInterface"
        | "PipeInterface"
        | "KISSInterface"
        | "AX25KISSInterface") => Err(feature_off(config, kind, "serial")),
        #[cfg(feature = "i2p")]
        "I2PInterface" => i2p::build(idx, config, ctx),
        #[cfg(not(feature = "i2p"))]
        kind @ "I2PInterface" => Err(feature_off(config, kind, "i2p")),
        #[cfg(all(feature = "ble", target_os = "linux"))]
        "BLEInterface" => ble::build(idx, config, ctx),
        #[cfg(all(not(feature = "ble"), target_os = "linux"))]
        "BLEInterface" => Err(feature_off(config, "BLEInterface", "ble")),
        // A BLE section in a config file on a non-Linux host is a
        // configuration error worth naming, not a silent no-op.
        #[cfg(not(target_os = "linux"))]
        "BLEInterface" => Err(crate::Error::Config(
            "BLEInterface is supported on Linux only (it requires BlueZ over D-Bus)".to_string(),
        )),
        #[cfg(feature = "websocket")]
        "WebSocketServerInterface" => websocket::build_server(idx, config, ctx),
        #[cfg(feature = "websocket")]
        "WebSocketClientInterface" => websocket::build_client(idx, config, ctx),
        #[cfg(not(feature = "websocket"))]
        kind @ ("WebSocketServerInterface" | "WebSocketClientInterface") => {
            Err(feature_off(config, kind, "websocket"))
        }
        other => {
            tracing::warn!("Unknown interface type: {}", other);
            Ok(Built::SelfManaged)
        }
    }
}

/// The refusal for an interface kind whose Cargo feature this build left out.
#[cfg(any(
    not(feature = "serial"),
    not(feature = "i2p"),
    all(not(feature = "ble"), target_os = "linux"),
    not(feature = "websocket")
))]
fn feature_off(config: &InterfaceConfig, kind: &str, feature: &str) -> Error {
    Error::Config(format!(
        "interface '{}': {kind} needs the leviculum-std `{feature}` feature, which this \
         build was compiled without; rebuild with `--features {feature}` or remove the section",
        config.name
    ))
}

/// The bandwidth an RNode is sent: the SX127x spelling of the configured
/// value, which is the one the firmware echoes back and the bring-up compares
/// for equality (L-0007). `validate_config` accepts both spellings; an SX1262
/// spelling sent raw would mismatch the echo, and below 10.4 kHz program the
/// next bandwidth up. A value outside the table is left for `validate_config`
/// to refuse by name.
#[cfg(feature = "serial")]
pub(super) fn rnode_wire_bandwidth(name: &str, bandwidth: u32) -> u32 {
    let wire = leviculum_core::sx126x::sx127x_bandwidth_hz(bandwidth).unwrap_or(bandwidth);
    if wire != bandwidth {
        tracing::info!(
            "{}: bandwidth {} Hz is the SX1262 spelling of the RNode's {} Hz, the same \
             register on both parts; sending {} Hz, which the RNode echoes",
            name,
            bandwidth,
            wire,
            wire
        );
    }
    wire
}

/// The socket address a listener binds for a configured `listen_ip` and port.
///
/// The ip and the port are joined as values, not as text: `"{ip}:{port}"`
/// turns `::` into `:::4343`, which nothing parses, while rnsd binds `::` and
/// every IPv6 literal (1119). A bracketed `[::]` is taken too, as it always
/// was here. Anything that is not an IP literal is a name, looked up as the
/// reference looks it up (`TCPServerInterface.get_address_for_host`,
/// TCPInterface.py:478-492: `getaddrinfo`, the first IPv4 answer unless
/// `prefer_ipv6`, else the first answer); a link-local `fe80::1%eth0` goes
/// the same way, since only the system resolver knows the scope.
pub(super) fn bind_address(
    listen_ip: &str,
    port: u16,
    prefer_ipv6: bool,
) -> Result<std::net::SocketAddr, String> {
    use std::net::{IpAddr, SocketAddr, ToSocketAddrs as _};

    let host = listen_ip.trim();
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if host.is_empty() {
        return Err("no address".to_string());
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, port));
    }
    let answers: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .collect();
    answers
        .iter()
        .find(|a| a.is_ipv6() == prefer_ipv6)
        .or_else(|| answers.first())
        .copied()
        .ok_or_else(|| format!("no addresses for {host}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1119: rnsd binds `listen_ip = ::` and any IPv6 literal
    /// (`TCPServerInterface.get_address_for_host`, TCPInterface.py:478-492,
    /// which is `getaddrinfo`); gluing `{ip}:{port}` made `:::4343`, which no
    /// socket-address parser takes.
    #[test]
    fn a_listen_ip_binds_as_rnsd_binds_it() {
        use std::net::SocketAddr;
        let bound = |ip: &str| bind_address(ip, 4343, false);
        let addr = |s: &str| s.parse::<SocketAddr>().expect("literal");
        assert_eq!(bound("::"), Ok(addr("[::]:4343")));
        assert_eq!(bound("::1"), Ok(addr("[::1]:4343")));
        assert_eq!(bound("0.0.0.0"), Ok(addr("0.0.0.0:4343")));
        assert_eq!(
            bound("[::]"),
            Ok(addr("[::]:4343")),
            "the bracketed form stays"
        );
        assert_eq!(bound(" 127.0.0.1 "), Ok(addr("127.0.0.1:4343")));
        // A name resolves, IPv4 first unless `prefer_ipv6` (the reference's
        // pick from the getaddrinfo answer).
        assert_eq!(bound("localhost"), Ok(addr("127.0.0.1:4343")));
        assert!(bound("").is_err());
    }

    /// L-0007: every spelling an operator may write reaches an RNode as the
    /// one RNode_Firmware's `getSignalBandwidth` reports for the register it
    /// programs (`reference/RNode_Firmware/sx126x.cpp:768`, the same values
    /// in `sx127x.cpp:397`), so the bring-up's equality check holds.
    #[cfg(feature = "serial")]
    #[test]
    fn an_rnode_is_sent_the_bandwidth_spelling_it_echoes() {
        const RNODE_ECHOES_HZ: [u32; 10] = [
            7_800, 10_400, 15_600, 20_800, 31_250, 41_700, 62_500, 125_000, 250_000, 500_000,
        ];
        for (sx1262, sx127x) in [
            (7_810, 7_800),
            (10_420, 10_400),
            (15_630, 15_600),
            (20_830, 20_800),
            (41_670, 41_700),
        ] {
            assert_eq!(rnode_wire_bandwidth("t", sx1262), sx127x);
            assert_eq!(rnode_wire_bandwidth("t", sx127x), sx127x);
        }
        for bw in RNODE_ECHOES_HZ {
            assert_eq!(rnode_wire_bandwidth("t", bw), bw, "BW{bw} moved");
        }
        assert_eq!(rnode_wire_bandwidth("t", 100_000), 100_000);
    }

    /// Owned wiring a test `InterfaceBuildCtx` borrows from.
    struct CtxOwner {
        next_id: Arc<AtomicUsize>,
        new_iface_tx: mpsc::Sender<InterfaceHandle>,
        reconnect_tx: mpsc::Sender<InterfaceId>,
        tunnel_notify_tx: mpsc::Sender<InterfaceId>,
        peer_event_tx: mpsc::Sender<(InterfaceId, crate::interfaces::PeerEvent)>,
        inventory: crate::interfaces::inventory::SharedInventory,
        // An explicit storage root, so no builder falls back to
        // `Config::default_config_dir()` and the host's ~/.reticulum.
        storage: tempfile::TempDir,
    }

    impl CtxOwner {
        fn new() -> Self {
            Self::with_iface_channel(4).0
        }

        /// Like [`new`](Self::new) but with a caller-sized new-interface
        /// channel, returning its receiver so a test can drain or fill it.
        fn with_iface_channel(capacity: usize) -> (Self, mpsc::Receiver<InterfaceHandle>) {
            let (new_iface_tx, new_iface_rx) = mpsc::channel(capacity);
            let (reconnect_tx, _reconnect_rx) = mpsc::channel(4);
            let (tunnel_notify_tx, _tunnel_notify_rx) = mpsc::channel(4);
            let (peer_event_tx, _peer_event_rx) = mpsc::channel(4);
            (
                Self {
                    next_id: Arc::new(AtomicUsize::new(100)),
                    new_iface_tx,
                    reconnect_tx,
                    tunnel_notify_tx,
                    peer_event_tx,
                    inventory: crate::interfaces::inventory::InterfaceInventory::shared(),
                    storage: tempfile::tempdir().expect("tempdir"),
                },
                new_iface_rx,
            )
        }

        fn ctx(&self) -> InterfaceBuildCtx<'_> {
            InterfaceBuildCtx {
                next_id: &self.next_id,
                new_iface_tx: &self.new_iface_tx,
                reconnect_tx: &self.reconnect_tx,
                tunnel_notify_tx: &self.tunnel_notify_tx,
                peer_event_tx: &self.peer_event_tx,
                corrupt_every: None,
                storage_path: Some(self.storage.path().to_path_buf()),
                outbound_socket_hook: None,
                host_resolver: None,
                inventory: self.inventory.clone(),
                transport_enabled: false,
                default_announce_rate: Default::default(),
                identity_hash: [0u8; 16],
                goodbyes: crate::interfaces::RadioGoodbyes::new(),
            }
        }
    }

    #[cfg(feature = "serial")]
    fn rnode_config(frequency: u64) -> InterfaceConfig {
        InterfaceConfig {
            interface_type: "RNodeInterface".to_string(),
            port: Some("/dev/nonexistent-test-port".to_string()),
            frequency: Some(frequency),
            bandwidth: Some(125_000),
            spreading_factor: Some(7),
            coding_rate: Some(5),
            ..Default::default()
        }
    }

    /// A 125 kHz carrier centred at 868.65 MHz sits in the 868.6-868.7 MHz
    /// alarm band (<= 25 kHz channel spacing only). It builds — the operator
    /// carries the regulatory responsibility, and the same carrier is lawful
    /// under a licence or in a shielded chamber — but it does not build
    /// quietly: the band is named at WARN, where no debug filter can hide it.
    ///
    /// Until 2026-08-27 this was an `Err`, which is the one refusal on the
    /// RNode path that was about radio law rather than device capability.
    #[cfg(feature = "serial")]
    #[tokio::test]
    async fn rnode_build_warns_about_a_carrier_in_an_alarm_band_and_honours_it() {
        let owner = CtxOwner::new();
        let (seen, subscriber) = warn_tap();
        {
            let _guard = tracing::subscriber::set_default(subscriber);
            rnode::build(0, &rnode_config(868_650_000), &owner.ctx())
                .expect("868.65 MHz / 125 kHz builds; it is warned about, not refused");
        }
        let warnings = seen.lock().expect("warn tap");
        assert!(
            warnings.iter().any(|w| w.contains("868.6-868.7 MHz")),
            "the band is named at WARN: {warnings:?}"
        );
    }

    /// The same block one sub-band over builds fine — the refusal is the gap,
    /// not the neighbourhood. Needs a runtime because a successful build
    /// spawns the interface tasks (the port itself may fail later; the
    /// reconnect loop owns that).
    #[cfg(feature = "serial")]
    #[tokio::test]
    async fn rnode_build_accepts_a_carrier_in_a_listed_sub_band() {
        let owner = CtxOwner::new();
        assert!(rnode::build(0, &rnode_config(869_525_000), &owner.ctx()).is_ok());
    }

    /// The `SerialInterface` LNode path treats the same carrier the same way:
    /// it builds, and the band is named at WARN before `serial_radio_config`
    /// resolves anything.
    ///
    /// Until 2026-08-27 this was an `Err`. The 2026-08-27 audit enumerated the
    /// refusals reachable from an *RNode* build and missed this one, because a
    /// `SerialInterface` with a `frequency` is the LNode LoRa modem, not an
    /// RNode — the same predicate, the same band, the same reason, on the path
    /// our own hardware uses. Needs a runtime: a successful build spawns the
    /// interface tasks.
    #[cfg(feature = "serial")]
    #[tokio::test]
    async fn serial_build_warns_about_a_carrier_in_an_alarm_band_and_honours_it() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            interface_type: "SerialInterface".to_string(),
            port: Some("/dev/nonexistent-test-port".to_string()),
            frequency: Some(868_650_000),
            ..Default::default()
        };
        let (seen, subscriber) = warn_tap();
        {
            let _guard = tracing::subscriber::set_default(subscriber);
            serial::build(0, &config, &owner.ctx())
                .expect("868.65 MHz / 125 kHz builds; it is warned about, not refused");
        }
        let warnings = seen.lock().expect("warn tap");
        assert!(
            warnings.iter().any(|w| w.contains("868.6-868.7 MHz")),
            "the band is named at WARN: {warnings:?}"
        );
    }

    /// Codeberg #404: an RNode section with no `bitrate` line still tells the
    /// driver what an announce costs on its carrier, so `lnsd` caps its transit
    /// announces exactly like a Python `RNodeInterface` on the same channel —
    /// which sets `self.bitrate` from the live radio settings unconditionally
    /// (`RNodeInterface.py:695`) and has therefore always been capped.
    ///
    /// The expectation is re-derived here from the airtime the PHY actually
    /// costs (`packet_airtime_ms` over the reference announce, the same call
    /// the interface charges to its duty ledger), never a number copied out of
    /// the implementation: a constant would go green on any refactor that
    /// changed the arithmetic. The band checks underneath are the independent
    /// half — they come from the measured on-air time of an announce-sized
    /// frame (#402, 2026-09-14), so a formula that silently lost its preamble
    /// or its coding rate fails them even if both sides of the equality moved
    /// together.
    #[cfg(feature = "serial")]
    #[tokio::test]
    async fn rnode_without_a_bitrate_key_declares_its_announce_cap_bitrate() {
        use leviculum_core::rnode::{
            derive_preamble_symbols, packet_airtime_ms, ANNOUNCE_CAP_REFERENCE_BYTES,
        };

        // (sf, bandwidth, band the effective rate must land in).
        //
        // The bands are +/-10 % around the on-air time of a 184 B frame
        // (183 B reference announce plus the 1 B LoRa framing header) worked
        // out by hand from the modem datasheet formula, not read back out of
        // this tree: 169 ms at SF7/BW250 with the 47-symbol preamble the
        // firmware programs there, 1764 ms at SF10/BW125 with its 18-symbol
        // floor. Tight enough that dropping either the preamble term
        // (10096 / 905 bps) or the coding-rate term falls outside.
        let phys: [(u8, u32, core::ops::Range<u32>); 2] =
            [(7, 250_000, 7_800..9_530), (10, 125_000, 746..912)];

        for (sf, bandwidth, band) in phys {
            let owner = CtxOwner::new();
            let config = InterfaceConfig {
                interface_type: "RNodeInterface".to_string(),
                port: Some("/dev/nonexistent-test-port".to_string()),
                frequency: Some(869_525_000),
                bandwidth: Some(bandwidth),
                spreading_factor: Some(sf),
                coding_rate: Some(5),
                // The point of the test: no `bitrate` key anywhere.
                bitrate: None,
                ..Default::default()
            };

            let Built::Handles(handles) = rnode::build(0, &config, &owner.ctx())
                .expect("a valid RNode section builds without hardware")
            else {
                panic!("an RNode section produces its own handle");
            };
            let info = &handles[0].info;

            let preamble = derive_preamble_symbols(sf, 5, bandwidth);
            let airtime_ms =
                packet_airtime_ms(ANNOUNCE_CAP_REFERENCE_BYTES, bandwidth, sf, 5, preamble);
            let expected = (ANNOUNCE_CAP_REFERENCE_BYTES as u64 * 8 * 1000 / airtime_ms) as u32;

            assert_eq!(
                info.announce_cap_bitrate,
                Some(expected),
                "sf={sf} bw={bandwidth}: the cap bitrate must be the reference \
                 announce over the airtime it costs at the live settings"
            );
            assert!(
                band.contains(&expected),
                "sf={sf} bw={bandwidth}: {expected} bps is outside the band a \
                 {ANNOUNCE_CAP_REFERENCE_BYTES} B frame measures at on this PHY ({band:?}); \
                 the airtime arithmetic has lost a term"
            );
            // The nominal symbol rate stays what it was, and stays separate:
            // the cap needs the effective rate, and on a slow PHY the two are
            // far enough apart that swapping them would be a third of the
            // intended silence.
            assert!(
                info.bitrate.is_some_and(|nominal| nominal > expected),
                "sf={sf} bw={bandwidth}: the nominal rate ({:?}) must stay above \
                 the effective one ({expected}) and must not have been replaced by it",
                info.bitrate
            );
        }
    }

    /// Every `warn!` message emitted while the returned guard lives.
    #[cfg(feature = "serial")]
    fn warn_tap() -> (
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        impl tracing::Subscriber + Send + Sync,
    ) {
        use tracing::field::{Field, Visit};
        use tracing_subscriber::layer::SubscriberExt;

        struct Sink(std::sync::Arc<std::sync::Mutex<Vec<String>>>);
        impl Visit for Sink {
            fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
                if f.name() == "message" {
                    if let Ok(mut seen) = self.0.lock() {
                        seen.push(format!("{v:?}"));
                    }
                }
            }
        }

        struct Layer(std::sync::Arc<std::sync::Mutex<Vec<String>>>);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Layer {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                if *event.metadata().level() == tracing::Level::WARN {
                    event.record(&mut Sink(std::sync::Arc::clone(&self.0)));
                }
            }
        }

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        (
            std::sync::Arc::clone(&seen),
            tracing_subscriber::registry().with(Layer(seen)),
        )
    }

    /// The #315 ceiling warning reaches the daemon log, and only for the block
    /// that crosses it. The pure decision is tested in `interfaces::serial`;
    /// what this adds is the wiring — a warning nothing calls encodes nothing.
    ///
    /// Both halves are the control for each other: same builder, same PHY,
    /// same port, one differing key.
    #[cfg(feature = "serial")]
    #[tokio::test]
    async fn a_preamble_pin_over_the_ceiling_is_warned_at_build_time() {
        let lora_block = |preamble: u16| InterfaceConfig {
            interface_type: "SerialInterface".to_string(),
            port: Some("/dev/nonexistent-test-port".to_string()),
            frequency: Some(869_525_000),
            bandwidth: Some(125_000),
            spreading_factor: Some(10),
            coding_rate: Some(8),
            preamble_symbols: Some(preamble),
            ..Default::default()
        };

        let owner = CtxOwner::new();
        let (seen, subscriber) = warn_tap();
        {
            let _guard = tracing::subscriber::set_default(subscriber);
            serial::build(0, &lora_block(24), &owner.ctx()).expect("24 builds, it only warns");
            serial::build(1, &lora_block(18), &owner.ctx()).expect("18 builds silently");
        }

        let warnings = seen.lock().expect("warn tap");
        assert_eq!(
            warnings.len(),
            1,
            "exactly the pinned-24 block warns: {warnings:?}"
        );
        assert!(
            warnings[0].contains("#315") && warnings[0].contains("serial_0"),
            "the warning names the issue and the interface: {}",
            warnings[0]
        );
    }

    /// TEST-ONLY `test_drop_direct_ingress` is refused together with IFAC:
    /// IFAC prepends material before the flags byte, so the wire hops byte
    /// the filter reads is no longer at `raw[1]`. The refusal is a config
    /// error at build time, on both LoRa-capable types.
    #[test]
    fn drop_direct_ingress_with_ifac_is_a_config_error() {
        let owner = CtxOwner::new();
        for (iface_type, extra_port_keys) in [("RNodeInterface", true), ("SerialInterface", false)]
        {
            let mut config = InterfaceConfig {
                name: "deaf".to_string(),
                interface_type: iface_type.to_string(),
                port: Some("/dev/nonexistent-test-port".to_string()),
                frequency: Some(869_525_000),
                test_drop_direct_ingress: true,
                networkname: Some("testnet".to_string()),
                ..Default::default()
            };
            if extra_port_keys {
                config.bandwidth = Some(125_000);
                config.spreading_factor = Some(7);
                config.coding_rate = Some(5);
            }
            let err = build_interface(0, &config, &owner.ctx(), &AutoPeerCount::default())
                .err()
                .unwrap_or_else(|| panic!("{iface_type}: IFAC + knob must not build"));
            let msg = err.to_string();
            assert!(
                msg.contains("incompatible with IFAC"),
                "{iface_type}: {msg}"
            );
        }
    }

    /// #463: a build without `serial` refuses every kind of that family as a
    /// config error naming the kind and the feature. A silent skip would
    /// start a node without the radio its operator configured.
    #[cfg(not(feature = "serial"))]
    #[test]
    fn a_serial_kind_without_its_feature_is_refused_naming_the_feature() {
        let owner = CtxOwner::new();
        for kind in [
            "RNodeInterface",
            "RNodeMultiInterface",
            "SerialInterface",
            "PipeInterface",
            "KISSInterface",
            "AX25KISSInterface",
        ] {
            let config = InterfaceConfig {
                name: "radio".to_string(),
                interface_type: kind.to_string(),
                ..Default::default()
            };
            match build_interface(0, &config, &owner.ctx(), &AutoPeerCount::default()) {
                Err(Error::Config(msg)) => assert!(
                    msg.contains("'radio'") && msg.contains(kind) && msg.contains("`serial`"),
                    "{kind}: {msg}"
                ),
                Err(other) => panic!("{kind}: not a config error: {other}"),
                Ok(_) => panic!("{kind}: built without the serial feature"),
            }
        }
    }

    /// #463: the same for `BLEInterface` without `ble`, on the one OS where
    /// the feature could have provided it.
    #[cfg(all(not(feature = "ble"), target_os = "linux"))]
    #[test]
    fn ble_without_its_feature_is_refused_naming_the_feature() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            name: "columba".to_string(),
            interface_type: "BLEInterface".to_string(),
            ..Default::default()
        };
        match build_interface(0, &config, &owner.ctx(), &AutoPeerCount::default()) {
            Err(Error::Config(msg)) => assert!(
                msg.contains("'columba'") && msg.contains("BLEInterface") && msg.contains("`ble`"),
                "{msg}"
            ),
            Err(other) => panic!("not a config error: {other}"),
            Ok(_) => panic!("BLEInterface built without the ble feature"),
        }
    }

    /// 1576: the same for `I2PInterface` without `i2p`.
    #[cfg(not(feature = "i2p"))]
    #[test]
    fn i2p_without_its_feature_is_refused_naming_the_feature() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            name: "anon".to_string(),
            interface_type: "I2PInterface".to_string(),
            connectable: Some(true),
            ..Default::default()
        };
        match build_interface(0, &config, &owner.ctx(), &AutoPeerCount::default()) {
            Err(Error::Config(msg)) => assert!(
                msg.contains("'anon'") && msg.contains("I2PInterface") && msg.contains("`i2p`"),
                "{msg}"
            ),
            Err(other) => panic!("not a config error: {other}"),
            Ok(_) => panic!("I2PInterface built without the i2p feature"),
        }
    }

    /// The knob on a type that does not implement the filter is refused
    /// rather than silently ignored — a rig config that thinks it emulates
    /// range but does not would invalidate a hardware run.
    #[test]
    fn drop_direct_ingress_on_an_unsupported_type_is_a_config_error() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            name: "deaf-tcp".to_string(),
            interface_type: "TCPClientInterface".to_string(),
            target_host: Some("127.0.0.1".to_string()),
            target_port: Some(4242),
            test_drop_direct_ingress: true,
            ..Default::default()
        };
        let err = build_interface(0, &config, &owner.ctx(), &AutoPeerCount::default())
            .err()
            .expect("knob on TCP must not build");
        let msg = err.to_string();
        assert!(msg.contains("only supported on"), "{msg}");
    }

    /// The multi builder checks each subinterface's own frequency, and warns
    /// per subinterface rather than refusing the whole port: one
    /// out-of-sub-band vport must not take the other three off the air.
    #[cfg(feature = "serial")]
    #[tokio::test]
    async fn rnode_multi_build_warns_about_a_subinterface_in_an_alarm_band() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            interface_type: "RNodeMultiInterface".to_string(),
            port: Some("/dev/nonexistent-test-port".to_string()),
            subinterfaces: vec![crate::config::SubinterfaceConfig {
                name: "gap".to_string(),
                vport: Some(0),
                frequency: Some(868_650_000),
                bandwidth: Some(125_000),
                spreading_factor: Some(7),
                coding_rate: Some(5),
                ..Default::default()
            }],
            ..Default::default()
        };
        let (seen, subscriber) = warn_tap();
        {
            let _guard = tracing::subscriber::set_default(subscriber);
            rnode_multi::build(0, &config, &owner.ctx())
                .expect("868.65 MHz / 125 kHz builds; it is warned about, not refused");
        }
        let warnings = seen.lock().expect("warn tap");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("868.6-868.7 MHz") && w.contains("'gap'")),
            "the band and the subinterface are named at WARN: {warnings:?}"
        );
    }

    // ------------------------------------------------------------------
    // The multi builder's decisions (T-0007)
    // ------------------------------------------------------------------
    //
    // `rnode_multi::plan` holds every decision the builder makes; `build`
    // only spawns what it returns. Each refusal it can give has a case, each
    // naming the key it refuses on, and each shape it can build has one.

    /// A subinterface lawful on its own: BW125 / SF7 / CR4:5 at `frequency`.
    #[cfg(feature = "serial")]
    fn multi_sub(name: &str, vport: u8, frequency: u64) -> crate::config::SubinterfaceConfig {
        crate::config::SubinterfaceConfig {
            name: name.to_string(),
            vport: Some(vport),
            frequency: Some(frequency),
            bandwidth: Some(125_000),
            spreading_factor: Some(7),
            coding_rate: Some(5),
            ..Default::default()
        }
    }

    #[cfg(feature = "serial")]
    fn multi_config(subinterfaces: Vec<crate::config::SubinterfaceConfig>) -> InterfaceConfig {
        InterfaceConfig {
            interface_type: "RNodeMultiInterface".to_string(),
            port: Some("/dev/nonexistent-test-port".to_string()),
            subinterfaces,
            ..Default::default()
        }
    }

    #[cfg(feature = "serial")]
    fn multi_plan(
        config: &InterfaceConfig,
    ) -> Result<crate::interfaces::rnode::RNodeMultiInterfaceConfig, Error> {
        rnode_multi::plan(3, config, &CtxOwner::new().ctx())
    }

    #[cfg(feature = "serial")]
    fn multi_refusal(config: &InterfaceConfig) -> String {
        match multi_plan(config) {
            Ok(_) => panic!("the section must be refused"),
            Err(e) => e.to_string(),
        }
    }

    #[cfg(feature = "serial")]
    #[test]
    fn rnode_multi_without_a_port_is_refused() {
        let mut config = multi_config(vec![multi_sub("a", 0, 869_525_000)]);
        config.port = None;
        let msg = multi_refusal(&config);
        assert!(msg.contains("requires port"), "{msg}");
    }

    #[cfg(feature = "serial")]
    #[test]
    fn rnode_multi_subinterface_without_a_vport_is_refused() {
        let mut sub = multi_sub("a", 0, 869_525_000);
        sub.vport = None;
        let msg = multi_refusal(&multi_config(vec![sub]));
        assert!(msg.contains("'a' requires vport"), "{msg}");
    }

    #[cfg(feature = "serial")]
    #[test]
    fn rnode_multi_vport_past_the_last_radio_is_refused() {
        let max = leviculum_core::rnode::MAX_SUBINTERFACES;
        let msg = multi_refusal(&multi_config(vec![multi_sub("a", max, 869_525_000)]));
        assert!(
            msg.contains(&format!("'a' vport {max} out of range (0-{})", max - 1)),
            "{msg}"
        );
        // The last vport itself is a radio.
        assert!(multi_plan(&multi_config(vec![multi_sub("a", max - 1, 869_525_000)])).is_ok());
    }

    /// Two subinterfaces on one vport would share one radio: the later PHY
    /// overwrites the earlier, and the hub routes that vport's receptions to
    /// only one of them. Refused, naming both and the vport.
    #[cfg(feature = "serial")]
    #[test]
    fn rnode_multi_duplicate_vport_is_refused_naming_both() {
        let msg = multi_refusal(&multi_config(vec![
            multi_sub("a", 2, 869_525_000),
            multi_sub("b", 2, 868_100_000),
        ]));
        assert!(
            msg.contains("'b' sets vport = 2") && msg.contains("rnode_multi_3[a]"),
            "{msg}"
        );
    }

    /// Each PHY key is refused by its own name when it is missing.
    #[cfg(feature = "serial")]
    #[test]
    fn rnode_multi_subinterface_missing_a_phy_key_is_refused_by_that_key() {
        type Clear = fn(&mut crate::config::SubinterfaceConfig);
        let cases: [(&str, Clear); 4] = [
            ("frequency", |s| s.frequency = None),
            ("bandwidth", |s| s.bandwidth = None),
            ("spreadingfactor", |s| s.spreading_factor = None),
            ("codingrate", |s| s.coding_rate = None),
        ];
        for (key, clear) in cases {
            let mut sub = multi_sub("a", 0, 869_525_000);
            clear(&mut sub);
            let msg = multi_refusal(&multi_config(vec![sub]));
            assert!(msg.contains(&format!("'a' requires {key}")), "{key}: {msg}");
        }
    }

    #[cfg(feature = "serial")]
    #[test]
    fn rnode_multi_frequency_past_u32_is_refused() {
        let msg = multi_refusal(&multi_config(vec![multi_sub("a", 0, 1 << 32)]));
        assert!(
            msg.contains("frequency 4294967296 exceeds u32 range"),
            "{msg}"
        );
    }

    /// The RNode wire field is a u8, so a negative `txpower` cannot be sent.
    #[cfg(feature = "serial")]
    #[test]
    fn rnode_multi_negative_txpower_is_refused() {
        let mut sub = multi_sub("a", 0, 869_525_000);
        sub.tx_power = Some(-1);
        let msg = multi_refusal(&multi_config(vec![sub]));
        assert!(msg.contains("tx_power -1 out of range (0-37)"), "{msg}");
    }

    /// The capability check shared with the single builder, attributed to
    /// the subinterface that failed it.
    #[cfg(feature = "serial")]
    #[test]
    fn rnode_multi_capability_refusal_names_the_subinterface() {
        let mut sub = multi_sub("wide", 0, 869_525_000);
        sub.spreading_factor = Some(13);
        let msg = multi_refusal(&multi_config(vec![multi_sub("ok", 1, 868_100_000), sub]));
        assert!(msg.contains("subinterface 'wide':"), "{msg}");
    }

    #[cfg(feature = "serial")]
    #[test]
    fn rnode_multi_with_no_enabled_subinterface_is_refused() {
        let mut off = multi_sub("off", 0, 869_525_000);
        off.enabled = false;
        for subs in [vec![], vec![off]] {
            let msg = multi_refusal(&multi_config(subs));
            assert!(
                msg.contains("'rnode_multi_3' has no enabled subinterfaces"),
                "{msg}"
            );
        }
    }

    /// Two vports become two subinterfaces: the first under the section's
    /// own id, the next drawn from the shared counter, each named after its
    /// block and carrying its own PHY. A disabled block is skipped entirely:
    /// it draws no id, and the vport it names stays free.
    #[cfg(feature = "serial")]
    #[test]
    fn rnode_multi_two_vports_plan_two_subinterfaces_on_their_own_phys() {
        let owner = CtxOwner::new();
        let mut off = multi_sub("off", 1, 869_525_000);
        off.enabled = false;
        let mut slow = multi_sub("slow", 1, 868_100_000);
        slow.spreading_factor = Some(12);
        slow.coding_rate = Some(8);
        slow.outgoing = false;
        let mut config = multi_config(vec![off, multi_sub("fast", 0, 869_525_000), slow]);
        config.flow_control = Some(true);
        config.buffer_size = Some(7);
        let plan = match rnode_multi::plan(3, &config, &owner.ctx()) {
            Ok(plan) => plan,
            Err(e) => panic!("two lawful vports plan: {e}"),
        };
        assert_eq!(plan.name, "rnode_multi_3");
        assert_eq!(plan.port_path, "/dev/nonexistent-test-port");
        assert!(plan.flow_control);
        assert_eq!(plan.buffer_size, 7);
        let got: Vec<_> = plan
            .subinterfaces
            .iter()
            .map(|s| {
                (
                    s.id,
                    s.name.as_str(),
                    s.vport,
                    s.frequency,
                    s.sf,
                    s.cr,
                    s.outgoing,
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    InterfaceId(3),
                    "rnode_multi_3[fast]",
                    0,
                    869_525_000,
                    7,
                    5,
                    true
                ),
                (
                    InterfaceId(100),
                    "rnode_multi_3[slow]",
                    1,
                    868_100_000,
                    12,
                    8,
                    false
                ),
            ]
        );
        assert_eq!(
            owner.next_id.load(std::sync::atomic::Ordering::Relaxed),
            101,
            "one id drawn, for the second enabled block only"
        );
    }

    /// Each vport's airtime locks come from its own PHY: with no limit set,
    /// the long-term lock is the lawful duty cycle of that vport's own
    /// sub-band (10 % at 869.525 MHz, 1 % at 868.1 MHz), and explicit
    /// percentages are carried per vport in hundredths.
    #[cfg(feature = "serial")]
    #[test]
    fn rnode_multi_airtime_locks_follow_each_vports_own_phy() {
        let mut capped = multi_sub("capped", 2, 869_525_000);
        capped.airtime_limit_short = Some(2.5);
        capped.airtime_limit_long = Some(0.5);
        let config = multi_config(vec![
            multi_sub("p", 0, 869_525_000),
            multi_sub("m", 1, 868_100_000),
            capped,
        ]);
        let plan = match multi_plan(&config) {
            Ok(plan) => plan,
            Err(e) => panic!("three lawful vports plan: {e}"),
        };
        let locks: Vec<_> = plan
            .subinterfaces
            .iter()
            .map(|s| (s.st_alock, s.lt_alock))
            .collect();
        assert_eq!(
            locks,
            vec![(None, Some(1000)), (None, Some(100)), (Some(250), Some(50))]
        );
    }

    /// An absent `txpower` is derived from each vport's own frequency and
    /// marked as derived; an explicit one, 0 included, is taken as written.
    #[cfg(feature = "serial")]
    #[test]
    fn rnode_multi_txpower_is_derived_per_vport_unless_written() {
        let mut quiet = multi_sub("quiet", 2, 869_525_000);
        quiet.tx_power = Some(0);
        let config = multi_config(vec![
            multi_sub("p", 0, 869_525_000),
            multi_sub("m", 1, 868_100_000),
            quiet,
        ]);
        let plan = match multi_plan(&config) {
            Ok(plan) => plan,
            Err(e) => panic!("three lawful vports plan: {e}"),
        };
        let powers: Vec<_> = plan
            .subinterfaces
            .iter()
            .map(|s| (s.tx_power, s.tx_power_derived))
            .collect();
        let derived = |f| leviculum_core::rnode::resolve_tx_power(None, f) as u8;
        assert_eq!(
            powers,
            vec![
                (derived(869_525_000), true),
                (derived(868_100_000), true),
                (0, false)
            ]
        );
    }

    /// An SX1262 spelling of a bandwidth reaches the RNode as the spelling
    /// it echoes, per vport, as in the single builder.
    #[cfg(feature = "serial")]
    #[test]
    fn rnode_multi_bandwidth_is_sent_in_the_rnode_spelling() {
        let mut sub = multi_sub("narrow", 0, 869_525_000);
        sub.bandwidth = Some(41_670);
        let plan = match multi_plan(&multi_config(vec![sub])) {
            Ok(plan) => plan,
            Err(e) => panic!("BW 41.67 kHz plans: {e}"),
        };
        assert_eq!(plan.subinterfaces[0].bandwidth, 41_700);
    }

    /// What reaches the transport: one interface per vport, under the planned
    /// names and ids, each reporting the bitrate of its own PHY.
    #[cfg(feature = "serial")]
    #[tokio::test]
    async fn rnode_multi_build_yields_one_interface_per_vport_with_its_own_bitrate() {
        let owner = CtxOwner::new();
        let mut slow = multi_sub("slow", 1, 868_100_000);
        slow.spreading_factor = Some(12);
        slow.coding_rate = Some(8);
        let config = multi_config(vec![multi_sub("fast", 0, 869_525_000), slow]);
        let handles = match rnode_multi::build(3, &config, &owner.ctx()) {
            Ok(Built::Handles(handles)) => handles,
            Ok(_) => panic!("a multi builder returns one handle per vport"),
            Err(e) => panic!("two lawful vports build: {e}"),
        };
        let got: Vec<_> = handles
            .iter()
            .map(|h| (h.info.id, h.info.name.as_str(), h.info.bitrate))
            .collect();
        let bitrate = |sf, cr| Some(leviculum_core::rnode::compute_bitrate(sf, cr, 125_000));
        assert_eq!(
            got,
            vec![
                (InterfaceId(3), "rnode_multi_3[fast]", bitrate(7, 5)),
                (InterfaceId(100), "rnode_multi_3[slow]", bitrate(12, 8)),
            ]
        );
        assert_ne!(bitrate(7, 5), bitrate(12, 8), "the two PHYs are told apart");
    }

    // ------------------------------------------------------------------
    // The regulatory guard
    // ------------------------------------------------------------------
    //
    // Project policy, decided 2026-08-16 and restated 2026-08-27:
    //
    //   No radio configuration is ever refused for a radio-regulatory
    //   reason. It is warned about, loudly, and then honoured.
    //
    // The reasoning is not only that we cannot know the operator's
    // jurisdiction. In the EU the operator, not the manufacturer, carries the
    // legal responsibility for compliant operation, so software that refuses
    // a setting takes on a responsibility it does not hold — and blocks
    // operation that is lawful elsewhere, under a licence, or in a shielded
    // chamber.
    //
    // The rule was written down and then drifted for eleven days without
    // anyone noticing. Prose has failed twice, so this is the mechanical
    // form. It is behavioural rather than source-level (an assertion that no
    // `Err` arm in this module is reachable from a regulatory predicate) for
    // three reasons:
    //
    //   * a source-level check cannot tell a regulatory predicate from a
    //     capability one without a hand-maintained list of predicate names,
    //     and that list is exactly the prose that already drifted;
    //   * it proves nothing about what a config file does — a refusal moved
    //     one call deeper, into `validate_config` or a new helper, passes it
    //     while the operator's daemon still will not start;
    //   * it goes stale the first time the module is refactored, whereas
    //     these cases are stated in the operator's own terms.
    //
    // The cost is a spawned interface per case against a port that does not
    // exist, which the reconnect loop already tolerates.
    //
    // The two halves are one guard. `no_radio_configuration_is_refused_for_a_
    // regulatory_reason` alone would be satisfied by deleting every check in
    // the file, so `capability_refusals_are_untouched_by_the_regulatory_guard`
    // pins the five refusals that must stay: the chip's tuning range, the ten
    // LoRa bandwidths, the RNode wire field's 0..=37, and the SF/CR ranges
    // shared with Python-RNS. Those are arithmetic, not paternalism.

    /// One radio block, so each case differs from the lawful control by
    /// exactly the key under test.
    #[cfg(feature = "serial")]
    #[derive(Clone, Copy)]
    struct Radio {
        frequency: u64,
        bandwidth: u32,
        sf: u8,
        cr: u8,
        tx_power: Option<i8>,
        airtime_limit_long: Option<f64>,
    }

    #[cfg(feature = "serial")]
    impl Radio {
        /// 869.525 MHz / BW125 / SF7 / CR4:5 — ERC 70-03 sub-band h1.7,
        /// inside every listed limit, with nothing explicitly configured that
        /// a regulatory predicate could object to.
        fn lawful() -> Self {
            Self {
                frequency: 869_525_000,
                bandwidth: 125_000,
                sf: 7,
                cr: 5,
                tx_power: None,
                airtime_limit_long: None,
            }
        }

        fn single(&self) -> InterfaceConfig {
            InterfaceConfig {
                name: "guard".to_string(),
                interface_type: "RNodeInterface".to_string(),
                port: Some("/dev/nonexistent-test-port".to_string()),
                frequency: Some(self.frequency),
                bandwidth: Some(self.bandwidth),
                spreading_factor: Some(self.sf),
                coding_rate: Some(self.cr),
                tx_power: self.tx_power,
                airtime_limit_long: self.airtime_limit_long,
                ..Default::default()
            }
        }

        fn multi(&self) -> InterfaceConfig {
            InterfaceConfig {
                name: "guard".to_string(),
                interface_type: "RNodeMultiInterface".to_string(),
                port: Some("/dev/nonexistent-test-port".to_string()),
                subinterfaces: vec![crate::config::SubinterfaceConfig {
                    name: "guard".to_string(),
                    vport: Some(0),
                    frequency: Some(self.frequency),
                    bandwidth: Some(self.bandwidth),
                    spreading_factor: Some(self.sf),
                    coding_rate: Some(self.cr),
                    tx_power: self.tx_power,
                    airtime_limit_long: self.airtime_limit_long,
                    ..Default::default()
                }],
                ..Default::default()
            }
        }

        /// The LNode LoRa modem: a `SerialInterface` block with a `frequency`.
        /// Not an RNode — it speaks our own firmware, not the RNode KISS
        /// dialect — but it carries the same radio keys, so the same policy
        /// binds it. This is the shape the 2026-08-27 audit missed.
        fn serial(&self) -> InterfaceConfig {
            InterfaceConfig {
                name: "guard".to_string(),
                interface_type: "SerialInterface".to_string(),
                port: Some("/dev/nonexistent-test-port".to_string()),
                frequency: Some(self.frequency),
                bandwidth: Some(self.bandwidth),
                spreading_factor: Some(self.sf),
                coding_rate: Some(self.cr),
                tx_power: self.tx_power,
                airtime_limit_long: self.airtime_limit_long,
                ..Default::default()
            }
        }
    }

    /// Every shape a radio configuration can arrive in, so a refusal cannot
    /// survive by living only in one the guard does not drive. Driven through
    /// `build_interface`, the dispatch a config file actually reaches, not the
    /// submodule entry points.
    #[cfg(feature = "serial")]
    fn radio_shapes(radio: &Radio) -> [(&'static str, InterfaceConfig); 3] {
        [
            ("RNodeInterface", radio.single()),
            ("RNodeMultiInterface", radio.multi()),
            ("SerialInterface", radio.serial()),
        ]
    }

    /// One regulatory edge case: the block, the substring the warning it must
    /// provoke carries, and the shapes that can carry it.
    ///
    /// Every shape must *build* every case — that half is unconditional, it is
    /// the policy itself. `warns_on` is narrower because the warning comes out
    /// wherever the key is resolved, and `SerialInterface` resolves
    /// `airtime_limit_long` through the firmware's own silent derivation
    /// (`leviculum_core::rnode::firmware_default_lt_alock`) rather than the
    /// driver's `resolve_lt_alock`, which is where that warning lives. The gap
    /// is data here, not prose, so it cannot widen unnoticed: a shape added to
    /// `radio_shapes` is refusal-checked whether or not anyone remembers it.
    #[cfg(feature = "serial")]
    struct Case {
        what: &'static str,
        radio: Radio,
        expected: &'static str,
        warns_on: &'static [&'static str],
    }

    /// The guard. Every known regulatory edge case builds, and says so out
    /// loud. See the block comment above for why this shape.
    ///
    /// "Warns" is asserted at WARN specifically: a decision narrated at debug
    /// is invisible in a default daemon log, which is the silent-substitution
    /// defect of Codeberg #349/#350 rather than a fix for it.
    #[cfg(feature = "serial")]
    #[tokio::test]
    async fn no_radio_configuration_is_refused_for_a_regulatory_reason() {
        const RADIO_SHAPES: &[&str] = &["RNodeInterface", "RNodeMultiInterface", "SerialInterface"];
        let cases: [Case; 3] = [
            Case {
                what: "a carrier occupying the 868.6-868.7 MHz narrowband alarm band",
                radio: Radio {
                    frequency: 868_650_000,
                    ..Radio::lawful()
                },
                expected: "868.6-868.7 MHz",
                warns_on: RADIO_SHAPES,
            },
            Case {
                what: "22 dBm on 867.2 MHz, 8 dB over the 14 dBm h1.4 e.r.p. limit",
                radio: Radio {
                    frequency: 867_200_000,
                    tx_power: Some(22),
                    ..Radio::lawful()
                },
                expected: "exceeds the derived ERP limit",
                warns_on: RADIO_SHAPES,
            },
            Case {
                what: "the ETSI duty cycle switched off outright",
                radio: Radio {
                    airtime_limit_long: Some(0.0),
                    ..Radio::lawful()
                },
                expected: "exceeds the ETSI EU868 lawful default",
                // Not SerialInterface: see `Case`.
                warns_on: &["RNodeInterface", "RNodeMultiInterface"],
            },
        ];

        let owner = CtxOwner::new();
        for case in cases {
            for (idx, (shape, config)) in radio_shapes(&case.radio).into_iter().enumerate() {
                let (seen, subscriber) = warn_tap();
                let built = {
                    let _guard = tracing::subscriber::set_default(subscriber);
                    build_interface(idx, &config, &owner.ctx(), &AutoPeerCount::default())
                };
                if let Err(e) = built {
                    panic!(
                        "{shape}: {} was refused, which project policy forbids \
                         for a regulatory reason: {e}",
                        case.what
                    );
                }
                if !case.warns_on.contains(&shape) {
                    continue;
                }
                let warnings = seen.lock().expect("warn tap");
                assert!(
                    warnings.iter().any(|w| w.contains(case.expected)),
                    "{shape}: {} built silently; a warning containing {:?} \
                     must reach the daemon log at WARN. Seen: {warnings:?}",
                    case.what,
                    case.expected
                );
            }
        }
    }

    /// The other half. A refusal about what the chip or the wire format can
    /// carry is arithmetic, not radio law, and must survive the guard above
    /// intact — a guard that also forbade these would be worse than none.
    ///
    /// Driven over `radio_shapes`, every shape a radio block can arrive in.
    /// `SerialInterface` was outside this half while it validated nothing:
    /// it resolved its radio block and handed it to the LNode firmware, so
    /// the same `[radio]` keys were checked when they drove an RNode and
    /// unchecked when they drove an LNode, and an unbuildable PHY reached the
    /// board instead of failing at daemon startup (Codeberg #352). The
    /// builder has called `leviculum_core::rnode::validate_config` since
    /// 3d40594b, which landed it for the `bandwidth = 0` of #274 and brought
    /// the other four bounds with it — unremarked, and therefore unpinned for
    /// two weeks while this comment still said the gap was open. Driving the
    /// shape here is what makes the claim checkable rather than prose.
    #[cfg(feature = "serial")]
    #[tokio::test]
    async fn capability_refusals_are_untouched_by_the_regulatory_guard() {
        let cases: [(&str, Radio); 5] = [
            (
                "below the transceiver's 137 MHz tuning floor",
                Radio {
                    frequency: 100_000_000,
                    ..Radio::lawful()
                },
            ),
            (
                "not one of the ten LoRa bandwidths",
                Radio {
                    bandwidth: 100_000,
                    ..Radio::lawful()
                },
            ),
            (
                "over the RNode wire field's 0..=37 dBm",
                Radio {
                    tx_power: Some(38),
                    ..Radio::lawful()
                },
            ),
            (
                "outside the LoRa spreading factors the driver carrier-senses \
                 at, 7..=12",
                Radio {
                    sf: 13,
                    ..Radio::lawful()
                },
            ),
            (
                "outside the LoRa coding rates 5..=8",
                Radio {
                    cr: 9,
                    ..Radio::lawful()
                },
            ),
        ];

        let owner = CtxOwner::new();
        // Control: the block every case is one key away from must build, or
        // the assertions below would pass for the wrong reason. All three
        // shapes, because it is also the control for the regulatory guard.
        for (shape, config) in radio_shapes(&Radio::lawful()) {
            build_interface(0, &config, &owner.ctx(), &AutoPeerCount::default())
                .unwrap_or_else(|e| panic!("{shape}: the lawful control must build: {e}"));
        }

        for (case, radio) in cases {
            for (idx, (shape, config)) in radio_shapes(&radio).into_iter().enumerate() {
                assert!(
                    build_interface(idx, &config, &owner.ctx(), &AutoPeerCount::default()).is_err(),
                    "{shape}: {case} is a capability limit, not radio law, and must \
                     still be refused"
                );
            }
        }
    }

    /// L-0063: an I2P peer sub-interface that cannot be registered (the
    /// new-interface channel is full) must not be spawned at all. The old
    /// path spawned the client task, then dropped the handle on `try_send`
    /// failure — the task kept building SAM tunnels the driver could
    /// neither see nor tear down. The id counter is the spawn's footprint:
    /// each spawned peer allocates exactly one id right before its spawn,
    /// so a full channel must leave the counter untouched for that peer.
    #[cfg(feature = "i2p")]
    #[tokio::test]
    async fn i2p_peer_that_cannot_register_is_not_spawned() {
        let (owner, mut new_iface_rx) = CtxOwner::with_iface_channel(1);
        let config = InterfaceConfig {
            interface_type: "I2PInterface".to_string(),
            peers: Some(vec![
                "peer-a.b32.i2p".to_string(),
                "peer-b.b32.i2p".to_string(),
            ]),
            ..Default::default()
        };

        let before = owner.next_id.load(std::sync::atomic::Ordering::Relaxed);
        i2p::build(0, &config, &owner.ctx()).expect("i2p peers build");

        // Exactly one peer fits the channel...
        assert!(
            new_iface_rx.try_recv().is_ok(),
            "the first peer must register"
        );
        assert!(
            new_iface_rx.try_recv().is_err(),
            "the second peer cannot fit a capacity-1 channel"
        );
        // ...and only that one may have been spawned.
        let allocated = owner.next_id.load(std::sync::atomic::Ordering::Relaxed) - before;
        assert_eq!(
            allocated, 1,
            "L-0063: a peer that cannot register was spawned anyway \
             ({allocated} ids allocated for 1 registrable slot)"
        );
    }

    /// Codeberg #282, the whole config path for `multicast_address_type`:
    /// file text -> `apply_interface_key` -> the AutoInterface builder -> the
    /// multicast group address the orchestrator joins. Before the key was
    /// read, a section asking for the permanent group derived the temporary
    /// address regardless, so an `lnsd` node added to a permanent-type `rnsd`
    /// peer group discovered nobody and nobody discovered it, with no error
    /// on either side.
    #[test]
    fn auto_multicast_address_type_reaches_the_group_address() {
        use crate::interfaces::auto_interface::{derive_multicast_address, MulticastAddressType};

        fn group_address(section: &str) -> std::net::Ipv6Addr {
            let config = crate::ini_config::parse_ini(section).expect("config parses");
            let iface = config.interfaces.get("Auto").expect("auto section");
            let auto = auto::auto_config_from(iface).expect("section resolves");
            derive_multicast_address(
                &auto.group_id,
                &auto.discovery_scope,
                auto.multicast_address_type,
            )
            .expect("group address derives")
        }

        let default = group_address("[interfaces]\n  [[Auto]]\n    type = AutoInterface\n");
        let temporary = group_address(
            "[interfaces]\n  [[Auto]]\n    type = AutoInterface\n \
             multicast_address_type = temporary\n",
        );
        let permanent = group_address(
            "[interfaces]\n  [[Auto]]\n    type = AutoInterface\n \
             multicast_address_type = Permanent\n",
        );

        // An absent key, and an explicit `temporary`, are the group every
        // existing deployment is already in.
        assert_eq!(
            default,
            derive_multicast_address(b"reticulum", "link", MulticastAddressType::Temporary)
                .unwrap(),
        );
        assert_eq!(temporary, default, "explicit temporary changes nothing");

        // `permanent` is a different group, and it is Python's.
        assert_eq!(
            permanent,
            derive_multicast_address(b"reticulum", "link", MulticastAddressType::Permanent)
                .unwrap(),
        );
        assert_ne!(
            permanent, default,
            "a permanent-type section must not land in the temporary group"
        );
    }

    /// An unrecognised value fails the build, and the failure names the key
    /// and the value. Python resolves the same string to the temporary group
    /// and says nothing (`AutoInterface.py:175-182`); that silence is the
    /// bug, because the operator's next move is to suspect the network: no
    /// log line anywhere mentions the address type.
    #[test]
    fn auto_build_refuses_an_unknown_multicast_address_type() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            name: "Auto".to_string(),
            interface_type: "AutoInterface".to_string(),
            multicast_address_type: Some("permanant".to_string()),
            ..Default::default()
        };

        let Err(err) = build_interface(0, &config, &owner.ctx(), &AutoPeerCount::default()) else {
            panic!("a typo in the group address must not build");
        };
        let msg = err.to_string();
        assert!(
            msg.contains("multicast_address_type") && msg.contains("permanant"),
            "the refusal names the key and the value: {msg}"
        );
    }

    /// L-0040, the config path of the two peer-table knobs: absent keys
    /// inherit the defaults, set keys reach the orchestrator's config.
    #[test]
    fn auto_peer_table_knobs_reach_the_orchestrator() {
        use crate::interfaces::auto_interface::{
            DEFAULT_MAX_PEERS, DEFAULT_MAX_PEERS_PER_SOURCE, DEFAULT_PEER_ADMISSION_INTERVAL_SECS,
        };
        use std::time::Duration;

        fn resolve(section: &str) -> crate::interfaces::auto_interface::AutoInterfaceConfig {
            let config = crate::ini_config::parse_ini(section).expect("config parses");
            let iface = config.interfaces.get("Auto").expect("auto section");
            auto::auto_config_from(iface).expect("section resolves")
        }

        let default = resolve("[interfaces]\n  [[Auto]]\n    type = AutoInterface\n");
        assert_eq!(default.max_peers, DEFAULT_MAX_PEERS);
        assert_eq!(default.max_peers_per_source, DEFAULT_MAX_PEERS_PER_SOURCE);
        assert_eq!(
            default.peer_admission_interval,
            Duration::from_secs(DEFAULT_PEER_ADMISSION_INTERVAL_SECS)
        );

        let set = resolve(
            "[interfaces]\n  [[Auto]]\n    type = AutoInterface\n \
             max_peers = 16\n    max_peers_per_source = 2\n    \
             peer_admission_interval = 0\n",
        );
        assert_eq!(set.max_peers, 16);
        assert_eq!(set.max_peers_per_source, 2);
        assert_eq!(set.peer_admission_interval, Duration::ZERO);
    }

    /// A cap of zero would refuse every peer; the build names it instead.
    #[test]
    fn auto_build_refuses_zero_max_peers() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            name: "Auto".to_string(),
            interface_type: "AutoInterface".to_string(),
            max_peers: Some(0),
            ..Default::default()
        };

        let Err(err) = build_interface(0, &config, &owner.ctx(), &AutoPeerCount::default()) else {
            panic!("max_peers = 0 must not build");
        };
        assert!(err.to_string().contains("max_peers"), "{err}");
    }

    /// Zero peers per source refuses every peer too; the build names it.
    #[test]
    fn auto_build_refuses_zero_max_peers_per_source() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            name: "Auto".to_string(),
            interface_type: "AutoInterface".to_string(),
            max_peers_per_source: Some(0),
            ..Default::default()
        };

        let Err(err) = build_interface(0, &config, &owner.ctx(), &AutoPeerCount::default()) else {
            panic!("max_peers_per_source = 0 must not build");
        };
        assert!(err.to_string().contains("max_peers_per_source"), "{err}");
    }
}

// +ciris (#109): the WebSocket server and client builders.
#[cfg(feature = "websocket")]
pub(crate) mod websocket;
