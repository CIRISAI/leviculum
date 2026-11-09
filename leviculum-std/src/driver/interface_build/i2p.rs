//! I2P interface builder: an optional inbound endpoint plus one outbound
//! sub-interface per configured peer, all registered through `new_iface_tx`.

use std::sync::Arc;
use std::time::Duration;

use crate::config::InterfaceConfig;
use crate::error::Error;
use crate::interfaces::i2p::{
    resolve_keyfile, spawn_i2p_client, spawn_i2p_server, I2pClientConfig, I2pServerConfig,
    I2pTimeouts, I2P_DEFAULT_BUFFER_SIZE, I2P_DEFAULT_RECONNECT_WAIT, I2P_HW_MTU,
};
use crate::interfaces::inventory::{
    self as inventory_names, I2pEndpoint, I2pTunnelState, InterfaceIdentity, ListenerRow,
};
use crate::sync_ext::MutexRecover as _;
use leviculum_core::transport::InterfaceId;

use super::super::{build_announce_rate_config, build_ifac_config};
use super::{Built, InterfaceBuildCtx};

pub(super) fn build(
    idx: usize,
    config: &InterfaceConfig,
    ctx: &InterfaceBuildCtx<'_>,
) -> Result<Built, Error> {
    // SAM bridge address: honour the I2P_SAM_ADDRESS env var (i2plib
    // `get_sam_address`), else the default 7656.
    let sam_address = std::env::var("I2P_SAM_ADDRESS")
        .unwrap_or_else(|_| crate::interfaces::i2p::sam::DEFAULT_SAM_ADDRESS.to_string());
    let buffer_size = config.buffer_size.unwrap_or(I2P_DEFAULT_BUFFER_SIZE);
    let reconnect_wait = config
        .reconnect_interval_secs
        .map(Duration::from_secs)
        .unwrap_or(I2P_DEFAULT_RECONNECT_WAIT);
    let ifac = build_ifac_config(config);
    // Codeberg #189: resolve the entry's ingress control once; every
    // sub-interface it spawns (accepted or dialled) inherits it, as in the
    // reference where a spawned I2P interface takes `self.ingress_control`
    // from the one parent (I2PInterface.py:951). A `connectable` entry takes
    // the listener default, a peers-only entry the dial-out one.
    let ingress_control = config.resolve_ingress_control();
    let storage_root = ctx
        .storage_path
        .clone()
        .unwrap_or_else(|| crate::config::Config::default_config_dir().join("storage"));

    // The parent row, `I2PInterface[<section>]` (Reticulum.py:1346-1356): the
    // reference runs the section itself as an interface that carries no
    // packets, with its peers and accepted connections as children. It never
    // becomes routable here either, so it takes the config index as its
    // inventory id, which the id allocator skips for exactly this purpose (as
    // the TCP listener does).
    let parent_id = idx;
    let connectable = config.connectable.unwrap_or(false);
    let mode = config
        .mode
        .as_deref()
        .and_then(leviculum_core::traits::InterfaceMode::from_config_str)
        .unwrap_or_default();
    let endpoint = Arc::new(I2pEndpoint::new(connectable));
    ctx.inventory.lock_recover().add_i2p_parent(
        parent_id,
        ListenerRow {
            identity: InterfaceIdentity {
                name: inventory_names::i2p_parent_name(&config.name),
                short_name: config.name.clone(),
                type_name: "I2PInterface",
                parent: None,
            },
            bitrate: config
                .bitrate
                .map(|b| b as i64)
                .unwrap_or(inventory_names::I2P_BITRATE_GUESS),
            hw_mtu: I2P_HW_MTU as i64,
            mode,
            announce_rate: leviculum_core::transport::resolve_announce_rate(
                build_announce_rate_config(config).as_ref(),
                ctx.transport_enabled,
                &ctx.default_announce_rate,
            ),
            ifac_size_bits: ifac.as_ref().map(|c| (c.ifac_size() * 8) as i64),
            departed_rxb: 0,
            departed_txb: 0,
            bound_addr: None,
            admission: None,
        },
        Arc::clone(&endpoint),
    );

    // Server endpoint (accepts inbound I2P connections), spawning one
    // sub-interface per peer via new_iface_tx.
    if connectable {
        let keyfile = resolve_keyfile(
            &storage_root.join("i2p"),
            &config.name,
            &ctx.identity_hash,
            idx,
        );
        spawn_i2p_server(I2pServerConfig {
            sam_address: sam_address.clone(),
            keyfile,
            buffer_size,
            name_prefix: format!("i2p_{}", idx),
            reconnect_wait,
            timeouts: I2pTimeouts::default(),
            next_id: ctx.next_id.clone(),
            new_interface_tx: ctx.new_iface_tx.clone(),
            ifac: ifac.clone(),
            ingress_control,
            outbound_socket_hook: ctx.outbound_socket_hook.clone(),
            endpoint: Arc::clone(&endpoint),
            section: config.name.clone(),
            parent_id,
            inventory: Arc::clone(&ctx.inventory),
            mode,
        });
        tracing::info!("I2P connectable endpoint (interface {})", idx);
    }

    // Outbound client sub-interface per configured peer. Routed through
    // new_iface_tx (like server-accepted connections) so each gets a unique id
    // with IFAC and hw_mtu applied uniformly by the registration branch. The
    // event loop is not consuming yet, so handles buffer in the channel until
    // it starts.
    if let Some(peers) = &config.peers {
        for peer in peers {
            let name = format!("i2p_{}_to_{}", idx, peer);
            // Reserve the registration slot BEFORE spawning (L-0063): a
            // spawned client whose handle cannot be delivered would keep
            // building SAM tunnels the driver can neither see nor tear
            // down. No slot, no task.
            let permit = match ctx.new_iface_tx.try_reserve() {
                Ok(permit) => permit,
                Err(e) => {
                    tracing::error!(
                        "NOT spawning I2P peer interface {}: no registration slot ({})",
                        name,
                        e
                    );
                    continue;
                }
            };
            let id = InterfaceId(
                ctx.next_id
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            );
            // `I2PInterfacePeer[<section> to <peer>]`, a child of the parent
            // row that adds nothing to its client count or bytes
            // (`parent_count = False`, I2PInterface.py:936-941).
            let tunnel = Arc::new(I2pTunnelState::default());
            let short_name = inventory_names::i2p_peer_short_name(&config.name, peer);
            ctx.inventory.lock_recover().add_i2p_peer(
                id.0,
                InterfaceIdentity {
                    name: inventory_names::i2p_peer_name(&short_name),
                    short_name,
                    type_name: "I2PInterfacePeer",
                    parent: Some(parent_id),
                },
                Arc::clone(&tunnel),
                false,
            );
            let handle = spawn_i2p_client(I2pClientConfig {
                id,
                name,
                sam_address: sam_address.clone(),
                peer: peer.clone(),
                buffer_size,
                reconnect_wait,
                timeouts: I2pTimeouts::default(),
                ifac: ifac.clone(),
                reconnect_notify: Some(ctx.reconnect_tx.clone()),
                ingress_control,
                outbound_socket_hook: ctx.outbound_socket_hook.clone(),
                tunnel,
            });
            permit.send(handle);
            tracing::info!("I2P client peer {} -> {}", idx, peer);
        }
    }

    if !config.connectable.unwrap_or(false) && config.peers.is_none() {
        tracing::warn!(
            "I2PInterface {} has neither `connectable = yes` nor `peers`; nothing to do",
            idx
        );
    }
    Ok(Built::SelfManaged)
}
