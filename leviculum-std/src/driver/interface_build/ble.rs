//! BLE interface builder (Columba `ble-reticulum` protocol over BlueZ).

use std::time::Duration;

use crate::config::InterfaceConfig;
use crate::error::Error;
use crate::interfaces::ble::{links, spawn_ble_interface, BleOptions};
use leviculum_core::transport::InterfaceId;

use super::{Built, InterfaceBuildCtx};

pub(super) fn build(
    idx: usize,
    config: &InterfaceConfig,
    ctx: &InterfaceBuildCtx<'_>,
) -> Result<Built, Error> {
    let defaults = BleOptions::default();
    // `Duration::from_secs_f64` panics on NaN, negative and overflowing
    // input, so the range is checked here where it is a config error. A
    // day is far beyond any useful scan pause and well inside `Duration`.
    let discovery_interval = match config.discovery_interval {
        Some(secs) if (0.0..=86_400.0).contains(&secs) => Duration::from_secs_f64(secs),
        Some(bad) => {
            return Err(Error::Config(format!(
                "BLEInterface discovery_interval must be between 0 and 86400 seconds, got {bad}"
            )));
        }
        None => defaults.discovery_interval,
    };
    // A bad entry is a startup error, not a dropped line: `initiate_only`
    // exists so an unlisted peer is NOT dialled, and a typo that silently
    // shortened the list would restore the free-for-all it was written to
    // prevent — invisibly, and only at the one moment it matters.
    let initiate_only = match config.initiate_only.as_deref() {
        Some(entries) => links::InitiateAllowlist::parse(entries).map_err(Error::Config)?,
        None => links::InitiateAllowlist::default(),
    };
    let opts = BleOptions {
        adapter: config.device.clone(),
        max_connections: config.max_connections.unwrap_or(links::DEFAULT_MAX_LINKS),
        min_rssi: config.min_rssi.unwrap_or(defaults.min_rssi),
        discovery_interval,
        enable_central: config.enable_central.unwrap_or(true),
        enable_peripheral: config.enable_peripheral.unwrap_or(true),
        initiate_only,
    };
    if !opts.enable_central && !opts.enable_peripheral {
        return Err(Error::Config(
            "BLEInterface with enable_central = no and enable_peripheral = no would \
             neither scan nor advertise; enable at least one role"
                .to_string(),
        ));
    }

    let iface_name = format!("ble_{}", idx);
    let id = InterfaceId(idx);
    let handle = spawn_ble_interface(
        id,
        iface_name,
        opts.clone(),
        ctx.identity_hash,
        ctx.peer_event_tx.clone(),
    );
    tracing::info!(
        "BLE interface on {} (central={}, peripheral={}, max_links={}, min_rssi={} dBm, \
         initiate_only={})",
        opts.adapter.as_deref().unwrap_or("default adapter"),
        opts.enable_central,
        opts.enable_peripheral,
        opts.max_connections,
        opts.min_rssi,
        // "any" rather than 0: a node that dials whoever the sort picks
        // is the default, and a count of zero reads like a node that
        // dials nobody.
        if opts.initiate_only.is_empty() {
            "any".to_string()
        } else {
            format!("{} peer(s)", opts.initiate_only.len())
        },
    );
    Ok(Built::Handles(vec![handle]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    struct CtxOwner {
        next_id: Arc<AtomicUsize>,
        new_iface_tx: mpsc::Sender<crate::interfaces::InterfaceHandle>,
        reconnect_tx: mpsc::Sender<InterfaceId>,
        tunnel_notify_tx: mpsc::Sender<InterfaceId>,
        peer_event_tx: mpsc::Sender<(InterfaceId, crate::interfaces::PeerEvent)>,
        inventory: crate::interfaces::inventory::SharedInventory,
    }

    impl CtxOwner {
        fn new() -> Self {
            let (new_iface_tx, _) = mpsc::channel(4);
            let (reconnect_tx, _) = mpsc::channel(4);
            let (tunnel_notify_tx, _) = mpsc::channel(4);
            let (peer_event_tx, _) = mpsc::channel(4);
            Self {
                next_id: Arc::new(AtomicUsize::new(100)),
                new_iface_tx,
                reconnect_tx,
                tunnel_notify_tx,
                peer_event_tx,
                inventory: crate::interfaces::inventory::InterfaceInventory::shared(),
            }
        }

        fn ctx(&self) -> InterfaceBuildCtx<'_> {
            InterfaceBuildCtx {
                next_id: &self.next_id,
                new_iface_tx: &self.new_iface_tx,
                reconnect_tx: &self.reconnect_tx,
                tunnel_notify_tx: &self.tunnel_notify_tx,
                peer_event_tx: &self.peer_event_tx,
                corrupt_every: None,
                storage_path: None,
                outbound_socket_hook: None,
                inventory: self.inventory.clone(),
                transport_enabled: false,
                identity_hash: [0x5A; 16],
            }
        }
    }

    /// A bare section builds with the documented defaults; BlueZ
    /// availability is a runtime concern (the task retries), never a
    /// build error. Needs a runtime because a successful build spawns
    /// the interface task.
    #[tokio::test]
    async fn a_bare_ble_section_builds_with_defaults() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            interface_type: "BLEInterface".to_string(),
            ..Default::default()
        };
        let built = build(3, &config, &owner.ctx()).expect("defaults build");
        let Built::Handles(handles) = built else {
            panic!("BLE builds one handle");
        };
        assert_eq!(handles.len(), 1);
        assert_eq!(handles[0].info.name, "ble_3");
        assert_eq!(
            handles[0].info.kind,
            leviculum_core::traits::InterfaceKind::Ble
        );
    }

    /// Disabling both roles is a config error, not a silent no-op
    /// interface.
    #[tokio::test]
    async fn both_roles_disabled_is_a_config_error() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            interface_type: "BLEInterface".to_string(),
            enable_central: Some(false),
            enable_peripheral: Some(false),
            ..Default::default()
        };
        let err = build(0, &config, &owner.ctx())
            .err()
            .expect("must not build");
        assert!(err.to_string().contains("at least one role"), "{err}");
    }

    /// A bad `initiate_only` entry stops the daemon instead of building
    /// an interface that dials everybody. The key's whole value is that
    /// an unlisted peer is not dialled; a silently dropped entry is that
    /// value withdrawn at the one moment nobody is watching.
    #[tokio::test]
    async fn a_bad_initiate_only_entry_is_a_config_error() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            interface_type: "BLEInterface".to_string(),
            initiate_only: Some(vec![
                "aa:bb:cc:dd:ee:ff".to_string(),
                "nonsense".to_string(),
            ]),
            ..Default::default()
        };
        let err = build(0, &config, &owner.ctx())
            .err()
            .expect("must not build");
        assert!(err.to_string().contains("initiate_only"), "{err}");
        assert!(err.to_string().contains("nonsense"), "{err}");
    }

    /// A good list builds, and an absent one is not an empty one that
    /// happens to behave the same — it is the same value the interface
    /// had before the key existed.
    #[tokio::test]
    async fn a_good_initiate_only_list_builds_and_an_absent_one_is_the_default() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            interface_type: "BLEInterface".to_string(),
            initiate_only: Some(vec![
                "AA:BB:CC:DD:EE:FF".to_string(),
                "b2a8bea1".to_string(),
            ]),
            ..Default::default()
        };
        build(0, &config, &owner.ctx()).expect("two well-formed entries build");

        let bare = InterfaceConfig {
            interface_type: "BLEInterface".to_string(),
            ..Default::default()
        };
        assert!(bare.initiate_only.is_none());
        build(1, &bare, &owner.ctx()).expect("no key, no restriction");
        assert!(BleOptions::default().initiate_only.is_empty());
    }

    /// A negative or non-finite discovery_interval is refused at build
    /// time — `Duration::from_secs_f64` would panic on it at runtime.
    #[tokio::test]
    async fn a_bad_discovery_interval_is_a_config_error() {
        let owner = CtxOwner::new();
        for bad in [-1.0, f64::NAN, f64::INFINITY] {
            let config = InterfaceConfig {
                interface_type: "BLEInterface".to_string(),
                discovery_interval: Some(bad),
                ..Default::default()
            };
            assert!(
                build(0, &config, &owner.ctx()).is_err(),
                "{bad} must be refused"
            );
        }
    }
}
