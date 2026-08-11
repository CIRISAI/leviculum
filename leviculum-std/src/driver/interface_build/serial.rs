//! Serial (KISS-less async serial / LoRa-serial) interface builder.

use crate::config::InterfaceConfig;
use crate::error::Error;
use leviculum_core::transport::InterfaceId;

use super::{Built, InterfaceBuildCtx};

pub(super) fn build(
    idx: usize,
    config: &InterfaceConfig,
    ctx: &InterfaceBuildCtx<'_>,
) -> Result<Built, Error> {
    let port_path = config
        .port
        .as_ref()
        .ok_or_else(|| Error::Config("SerialInterface requires port".to_string()))?
        .clone();
    let speed = config.speed.unwrap_or(9600);
    let data_bits = crate::interfaces::serial::parse_data_bits(config.databits.unwrap_or(8));
    let parity = crate::interfaces::serial::parse_parity(config.parity.as_deref().unwrap_or("N"));
    let stop_bits = crate::interfaces::serial::parse_stop_bits(config.stopbits.unwrap_or(1));
    let buffer_size = config
        .buffer_size
        .unwrap_or(crate::interfaces::serial::SERIAL_DEFAULT_BUFFER_SIZE);

    let iface_name = format!("serial_{}", idx);
    let id = InterfaceId(idx);

    // A `frequency` makes this a LoRa modem, and a carrier occupying one of
    // the ERC 70-03 narrowband alarm bands is a config error, not a default.
    // Checked before `serial_radio_config` resolves any radio parameter. The
    // bandwidth default mirrors `serial_radio_config`'s.
    if let Some(frequency) = config.frequency {
        let bandwidth = config.bandwidth.unwrap_or(125_000);
        if let Some(gap) = leviculum_core::rnode::erp_band_gap(frequency, bandwidth) {
            return Err(Error::Config(format!(
                "SerialInterface: frequency {} Hz with bandwidth {} Hz overlaps the {} band, \
                 where ERC 70-03 permits only <= 25 kHz channel spacing; \
                 choose a centre frequency whose signal fits a listed sub-band",
                frequency, bandwidth, gap
            )));
        }
    }

    let radio_config = crate::interfaces::serial::serial_radio_config(config);

    let mut handle = crate::interfaces::serial::spawn_serial_interface(
        crate::interfaces::serial::SerialInterfaceConfig {
            id,
            name: iface_name.clone(),
            port: port_path.clone(),
            speed,
            data_bits,
            parity,
            stop_bits,
            buffer_size,
            reconnect_notify: Some(ctx.reconnect_tx.clone()),
            radio_config,
            test_drop_direct_ingress: config.test_drop_direct_ingress,
        },
    );
    handle.info.bitrate = Some(speed);

    tracing::info!("Serial interface on {} (speed={} baud)", port_path, speed,);
    Ok(Built::Handles(vec![handle]))
}
