//! AutoInterface orchestrator builder.

use crate::config::InterfaceConfig;
use crate::error::Error;
use crate::interfaces::auto_interface::orchestrator::spawn_auto_interface;
use crate::interfaces::auto_interface::{AutoInterfaceConfig, MulticastAddressType};

use super::super::AutoPeerCount;
use super::{Built, InterfaceBuildCtx};

/// Resolve one `[[AutoInterface]]` section into the orchestrator's config.
///
/// Separate from [`build`] so the whole config path - file text through
/// `apply_interface_key` to the multicast group address the orchestrator
/// joins - can be exercised without binding a socket or joining a group on
/// the test host.
pub(super) fn auto_config_from(config: &InterfaceConfig) -> Result<AutoInterfaceConfig, Error> {
    // The address type is part of the multicast group address, so a value we
    // cannot resolve is not a knob to shrug at: silently falling back (as
    // Python does) puts the node in a different group from the one the
    // operator configured, where it discovers nobody and nothing in the logs
    // says why (Codeberg #282). Fail startup and name the value instead.
    let multicast_address_type = match config.multicast_address_type.as_deref() {
        None => MulticastAddressType::default(),
        Some(value) => MulticastAddressType::from_config_str(value).ok_or_else(|| {
            Error::Config(format!(
                "interface '{}': unknown multicast_address_type '{}' \
                 (expected 'temporary' or 'permanent')",
                config.name, value
            ))
        })?,
    };

    Ok(AutoInterfaceConfig {
        group_id: config
            .group_id
            .as_deref()
            .map(|s| s.as_bytes().to_vec())
            .unwrap_or_else(|| crate::interfaces::auto_interface::DEFAULT_GROUP_ID.to_vec()),
        discovery_port: config
            .discovery_port
            .unwrap_or(crate::interfaces::auto_interface::DEFAULT_DISCOVERY_PORT),
        data_port: config
            .data_port
            .unwrap_or(crate::interfaces::auto_interface::DEFAULT_DATA_PORT),
        discovery_scope: config
            .discovery_scope
            .clone()
            .unwrap_or_else(|| "link".to_string()),
        allowed_devices: config.devices.clone(),
        ignored_devices: config.ignored_devices.clone(),
        multicast_loopback: config.multicast_loopback.unwrap_or(true),
        multicast_address_type,
    })
}

pub(super) fn build(
    config: &InterfaceConfig,
    ctx: &InterfaceBuildCtx<'_>,
    auto_peer_count: &AutoPeerCount,
) -> Result<Built, Error> {
    let auto_config = auto_config_from(config)?;
    let (discovery_port, data_port, multicast_address_type) = (
        auto_config.discovery_port,
        auto_config.data_port,
        auto_config.multicast_address_type,
    );
    let peer_count_rx =
        spawn_auto_interface(ctx.next_id.clone(), ctx.new_iface_tx.clone(), auto_config);
    auto_peer_count.push(peer_count_rx);
    tracing::info!(
        "AutoInterface: starting orchestrator (discovery_port={}, data_port={}, \
         multicast_address_type={:?})",
        discovery_port,
        data_port,
        multicast_address_type
    );
    Ok(Built::SelfManaged)
}
