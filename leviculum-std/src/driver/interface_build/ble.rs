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
    // A bad entry is a startup error, not a dropped line: the two
    // allow-lists exist so an unlisted peer is NOT dialled and NOT
    // served, and a typo that silently shortened either list would
    // restore the free-for-all it was written to prevent — invisibly,
    // and only at the one moment it matters.
    let allowlist = |key: &'static str, entries: Option<&[String]>| match entries {
        Some(entries) => links::PeerAllowlist::parse(key, entries).map_err(Error::Config),
        None => Ok(links::PeerAllowlist::default()),
    };
    let initiate_only = allowlist("initiate_only", config.initiate_only.as_deref())?;
    let accept_only = allowlist("accept_only", config.accept_only.as_deref())?;
    // Below the BLE 4.0 floor a fragment carries no payload, and the
    // link table ignores an MTU under it, so a smaller cap would be
    // silently no cap at all.
    if let Some(cap) = config.fragment_mtu {
        if cap < leviculum_core::framing::ble::MIN_MTU {
            return Err(Error::Config(format!(
                "BLEInterface fragment_mtu must be at least {}, got {cap}",
                leviculum_core::framing::ble::MIN_MTU
            )));
        }
    }
    let max_connections = config.max_connections.unwrap_or(links::DEFAULT_MAX_LINKS);
    let central_links = resolve_central_links(config.central_links, max_connections)?;
    let opts = BleOptions {
        adapter: config.device.clone(),
        max_connections,
        central_links,
        advertise_when_full: config
            .advertise_when_full
            .unwrap_or(BleOptions::default().advertise_when_full),
        min_rssi: config.min_rssi.unwrap_or(defaults.min_rssi),
        discovery_interval,
        enable_central: config.enable_central.unwrap_or(true),
        enable_peripheral: config.enable_peripheral.unwrap_or(true),
        initiate_only,
        accept_only,
        tx_gap_ms: config.tx_gap_ms,
        fragment_mtu: config.fragment_mtu,
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
    // "any" rather than 0: a node that dials whoever the sort picks, and
    // serves whoever dials it, is the default — a count of zero reads
    // like a node that dials or serves nobody.
    let listed = |list: &links::PeerAllowlist| {
        if list.is_empty() {
            "any".to_string()
        } else {
            format!("{} peer(s)", list.len())
        }
    };
    tracing::info!(
        "BLE interface on {} (central={}, peripheral={}, max_links={}, central_links={}, \
         advertise_when_full={}, min_rssi={} dBm, initiate_only={}, accept_only={}, \
         tx_gap_ms={}, fragment_mtu={})",
        opts.adapter.as_deref().unwrap_or("default adapter"),
        opts.enable_central,
        opts.enable_peripheral,
        opts.max_connections,
        opts.central_links,
        opts.advertise_when_full,
        opts.min_rssi,
        listed(&opts.initiate_only),
        listed(&opts.accept_only),
        leviculum_ble_tx::effective_tx_gap_ms(opts.tx_gap_ms),
        opts.fragment_mtu
            .map_or_else(|| "exchanged".to_string(), |cap| cap.to_string()),
    );
    Ok(Built::Handles(vec![handle]))
}

/// How many of the `max_connections` links a host dials (#723), two
/// unless the key says otherwise (770). Refused rather than clamped
/// outside 1..=3 and outside the total's room for one incoming link, so
/// a room log never claims a dial count the table did not run. Unset
/// under a small `max_connections` keeps the shape that key had before
/// the default moved: one incoming slot survives at 2, and 1 is one dial
/// as it always was.
fn resolve_central_links(key: Option<usize>, max_connections: usize) -> Result<usize, Error> {
    match key {
        Some(n)
            if (1..=links::MAX_CENTRAL_LINKS).contains(&n)
                && n <= max_connections.saturating_sub(1) =>
        {
            Ok(n)
        }
        Some(bad) => Err(Error::Config(format!(
            "BLEInterface central_links must be between 1 and {} and below \
             max_connections ({max_connections}), got {bad}",
            links::MAX_CENTRAL_LINKS
        ))),
        None => Ok(links::DEFAULT_CENTRAL_LINKS.min(max_connections.saturating_sub(1).max(1))),
    }
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
                host_resolver: None,
                inventory: self.inventory.clone(),
                transport_enabled: false,
                default_announce_rate: Default::default(),
                identity_hash: [0x5A; 16],
                goodbyes: crate::interfaces::RadioGoodbyes::new(),
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
        // Codeberg #230, order 1380: BLE is a radio and declares the
        // announce re-hear evidence rule for its direct links.
        assert_eq!(
            handles[0].info.path_choice,
            leviculum_core::transport::PathChoice::HopsAndLoss
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

    /// The symmetric key gets the symmetric treatment: a bad
    /// `accept_only` entry stops the daemon, and the error names the
    /// key that holds it rather than the other list. An interface that
    /// built with a silently shortened accept list would serve a peer
    /// the operator had excluded, which is the hole the key closes.
    #[tokio::test]
    async fn a_bad_accept_only_entry_is_a_config_error() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            interface_type: "BLEInterface".to_string(),
            accept_only: Some(vec!["b2a8bea1".to_string(), "nonsense".to_string()]),
            ..Default::default()
        };
        let err = build(0, &config, &owner.ctx())
            .err()
            .expect("must not build");
        assert!(err.to_string().contains("accept_only"), "{err}");
        assert!(!err.to_string().contains("initiate_only"), "{err}");
        assert!(err.to_string().contains("nonsense"), "{err}");
    }

    /// A good accept list builds; an absent one is the value the
    /// interface had before the key existed — the negative control that
    /// every deployment today depends on.
    #[tokio::test]
    async fn a_good_accept_only_list_builds_and_an_absent_one_is_the_default() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            interface_type: "BLEInterface".to_string(),
            accept_only: Some(vec![
                "AA:BB:CC:DD:EE:FF".to_string(),
                "b2a8bea1a1a1a1a1a1a1a1a1a1a1a1a1".to_string(),
            ]),
            ..Default::default()
        };
        build(0, &config, &owner.ctx()).expect("two well-formed entries build");

        let bare = InterfaceConfig {
            interface_type: "BLEInterface".to_string(),
            ..Default::default()
        };
        assert!(bare.accept_only.is_none());
        build(1, &bare, &owner.ctx()).expect("no key, no restriction");
        assert!(BleOptions::default().accept_only.is_empty());
    }

    /// `fragment_mtu` below the BLE 4.0 floor is refused at startup:
    /// the link table ignores an MTU under 23, so such a cap would build
    /// and then cap nothing. The floor itself builds.
    #[tokio::test]
    async fn a_fragment_mtu_below_the_floor_is_a_config_error() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            interface_type: "BLEInterface".to_string(),
            fragment_mtu: Some(22),
            ..Default::default()
        };
        let err = build(0, &config, &owner.ctx())
            .err()
            .expect("must not build");
        assert!(err.to_string().contains("fragment_mtu"), "{err}");

        let floor = InterfaceConfig {
            interface_type: "BLEInterface".to_string(),
            fragment_mtu: Some(23),
            ..Default::default()
        };
        build(1, &floor, &owner.ctx()).expect("the floor itself builds");
    }

    /// The two keys are set independently and neither reads the other's
    /// value: a config that pins whom it dials says nothing about whom
    /// it serves, and the reverse.
    #[tokio::test]
    async fn the_two_allowlists_are_independent_at_build_time() {
        let owner = CtxOwner::new();
        let dial_only = InterfaceConfig {
            interface_type: "BLEInterface".to_string(),
            initiate_only: Some(vec!["b2a8bea1".to_string()]),
            ..Default::default()
        };
        assert!(dial_only.accept_only.is_none());
        build(0, &dial_only, &owner.ctx()).expect("a dial pin alone builds");

        let serve_only = InterfaceConfig {
            interface_type: "BLEInterface".to_string(),
            accept_only: Some(vec!["b2a8bea1".to_string()]),
            ..Default::default()
        };
        assert!(serve_only.initiate_only.is_none());
        build(1, &serve_only, &owner.ctx()).expect("a serve pin alone builds");
    }

    /// `central_links` builds from 1 to 3 below the total, and is
    /// refused with its name at 0, above 3, and where it would leave the
    /// total no incoming link (#723).
    #[tokio::test]
    async fn central_links_is_range_checked_with_the_key_named() {
        let owner = CtxOwner::new();
        let with = |central: usize, max: Option<usize>| InterfaceConfig {
            interface_type: "BLEInterface".to_string(),
            central_links: Some(central),
            max_connections: max,
            ..Default::default()
        };
        for good in [1, 2, 3] {
            build(0, &with(good, None), &owner.ctx())
                .unwrap_or_else(|e| panic!("{good} of 4 must build: {e}"));
        }
        for (bad, max) in [
            (0, None),
            (4, None),
            (4, Some(7)),
            (2, Some(2)),
            (1, Some(1)),
        ] {
            let err = build(0, &with(bad, max), &owner.ctx())
                .err()
                .unwrap_or_else(|| panic!("{bad} of {max:?} must not build"));
            assert!(err.to_string().contains("central_links"), "{err}");
        }
        assert_eq!(
            BleOptions::default().central_links,
            links::DEFAULT_CENTRAL_LINKS,
            "no key is the host default"
        );
    }

    /// Unset, `central_links` is two dials (770) under the default total,
    /// and never takes a small `max_connections`' last incoming slot:
    /// one dial at 2 and at 1, as those caps ran before the default moved.
    #[test]
    fn an_unset_central_links_is_two_dials_and_leaves_an_incoming_slot() {
        assert_eq!(links::DEFAULT_CENTRAL_LINKS, 2);
        let resolved = |max| resolve_central_links(None, max).ok();
        assert_eq!(
            resolved(links::DEFAULT_MAX_LINKS),
            Some(2),
            "two out, two in"
        );
        assert_eq!(resolved(3), Some(2));
        assert_eq!(resolved(2), Some(1), "one incoming slot survives");
        assert_eq!(resolved(1), Some(1), "one dial, as before");
        assert_eq!(
            resolve_central_links(Some(1), 4).ok(),
            Some(1),
            "the board's shape"
        );
    }

    /// `advertise_when_full` (752) is on unless the key says so (770);
    /// `false` is the boards' dark-when-full gate, which
    /// `an_incoming_full_table_goes_dark_and_a_freed_slot_re_advertises`
    /// keeps covered.
    #[test]
    fn advertise_when_full_defaults_on() {
        assert!(
            BleOptions::default().advertise_when_full,
            "no key keeps a full host on the air saying zero"
        );
        let unset = InterfaceConfig::default();
        assert_eq!(unset.advertise_when_full, None);
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
