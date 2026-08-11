//! Serial interface. HDLC-framed bidirectional serial port
//!
//! Implements a plain serial interface matching Python Reticulum's
//! `SerialInterface`. Uses HDLC simplified framing (same as TCP and
//! LocalInterface) over a serial port.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use crate::sync_ext::MutexRecover;
use std::time::Duration;

use leviculum_core::constants::MTU;
use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use leviculum_core::rnode::derive_preamble_symbols;
use leviculum_core::transport::InterfaceId;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::{IncomingPacket, InterfaceCounters, InterfaceHandle, InterfaceInfo, OutgoingPacket};

/// Python SerialInterface HW_MTU
const SERIAL_HW_MTU: u32 = 564;

/// Default channel buffer size for serial interfaces.
pub(crate) const SERIAL_DEFAULT_BUFFER_SIZE: usize = 64;

/// Frame buffer multiplier (accounts for HDLC escaping overhead)
const FRAME_BUFFER_MULTIPLIER: usize = 2;

/// Read buffer size
const READ_BUF_SIZE: usize = 1024;

/// Incomplete frame timeout (ms). Matches Python SerialInterface.timeout = 100.
/// If no data arrives for this duration while in_frame, the partial frame is
/// discarded to prevent desynchronization from noise/corruption.
const FRAME_TIMEOUT: Duration = Duration::from_millis(100);

/// Reconnect interval after serial port loss
const RECONNECT_INTERVAL: Duration = Duration::from_secs(5);

/// Radio configuration to send to LNode firmware over serial (test infrastructure).
pub(crate) struct SerialRadioConfig {
    pub frequency: u64,
    pub bandwidth: u32,
    pub spreading_factor: u8,
    pub coding_rate: u8,
    pub tx_power: i8,
    pub preamble_len: u16,
    pub csma_enabled: bool,
    /// Long-term airtime lock in the firmware's `fraction * 10000` encoding;
    /// `0` is what the firmware reads as unlimited.
    pub lt_alock: u16,
}

/// Build the LNode radio config a `SerialInterface` block asks for, or
/// `None` when the block names no `frequency` and is therefore a plain
/// (non-LoRa) serial pipe.
///
/// Every PHY default here is the firmware's own compiled default
/// (`leviculum_nrf::lora::RadioConfig::eu_medium`), with one exception the
/// preamble makes necessary. The firmware's compiled 24 is a constant for
/// every spreading factor, while the RNode firmware — our reference for LoRa
/// PHY behaviour — scales the preamble to a target duration and floors it at
/// 18 symbols. The two agree at SF7/BW125 and disagree from SF8 down, so a
/// constant here is a wire-level deviation that costs interop with every
/// RNode peer in the long-range regime. The default is therefore
/// [`derive_preamble_symbols`], and `preamble_symbols` in the config file
/// still overrides it — which is how the corner gets re-measured, and how a
/// node with a non-conforming peer copes.
///
/// The airtime lock resolves the same way, and for the same reason. An
/// LNode is configured by this function and by nothing else, so a hardcoded
/// `lt_alock = 0` here was the host telling the firmware "unlimited" and
/// overriding the lawful default the firmware would otherwise have derived
/// from its own frequency ([`firmware_default_lt_alock`]). Sending the
/// resolution instead of a constant means an LNode ends up under the same
/// limit as an RNode on the same frequency (`driver::resolve_lt_alock`),
/// and `airtime_limit_long` in the config file still overrides it — with
/// `0` still available for a bench that means unlimited and says so.
pub(crate) fn serial_radio_config(
    cfg: &crate::config::InterfaceConfig,
) -> Option<SerialRadioConfig> {
    let frequency = cfg.frequency?;
    let bandwidth = cfg.bandwidth.unwrap_or(125_000);
    let spreading_factor = cfg.spreading_factor.unwrap_or(7);
    let coding_rate = cfg.coding_rate.unwrap_or(5);
    Some(SerialRadioConfig {
        frequency,
        bandwidth,
        spreading_factor,
        coding_rate,
        tx_power: leviculum_core::rnode::resolve_tx_power(cfg.tx_power, frequency),
        preamble_len: cfg
            .preamble_symbols
            .unwrap_or_else(|| derive_preamble_symbols(spreading_factor, coding_rate, bandwidth)),
        csma_enabled: cfg.csma_enabled.unwrap_or(true),
        lt_alock: leviculum_core::rnode::firmware_default_lt_alock(
            frequency,
            cfg.airtime_limit_long.map(|p| (p * 100.0) as u16),
        ),
    })
}

/// Configuration for a serial interface.
pub(crate) struct SerialInterfaceConfig {
    pub id: InterfaceId,
    pub name: String,
    pub port: String,
    pub speed: u32,
    pub data_bits: tokio_serial::DataBits,
    pub parity: tokio_serial::Parity,
    pub stop_bits: tokio_serial::StopBits,
    pub buffer_size: usize,
    pub reconnect_notify: Option<mpsc::Sender<InterfaceId>>,
    pub radio_config: Option<SerialRadioConfig>,
}

/// Spawn a serial interface with automatic reconnection.
///
/// Creates channel pair once, spawns a reconnect task that reopens the port
/// on failure. The `InterfaceHandle` stays alive across reconnections.
pub(crate) fn spawn_serial_interface(config: SerialInterfaceConfig) -> InterfaceHandle {
    let (incoming_tx, incoming_rx) = mpsc::channel(config.buffer_size);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(config.buffer_size);
    let counters = Arc::new(InterfaceCounters::new());

    let id = config.id;
    let handle_name = config.name.clone();
    let task_name = config.name.clone();
    let task_counters = Arc::clone(&counters);

    // Build the airtime credit bucket if radio params are known. Non-LoRa
    // Serial consumers (no radio_config) leave credit = None, preserving
    // "always ready" semantics for the next_slot_ms override.
    let credit = config.radio_config.as_ref().map(|rc| {
        Arc::new(Mutex::new(super::airtime::AirtimeCredit::new(
            rc.bandwidth,
            rc.spreading_factor,
            rc.coding_rate,
            rc.preamble_len,
            SERIAL_HW_MTU,
        )))
    });
    let task_credit = credit.clone();

    tokio::spawn(async move {
        serial_reconnect_task(
            id,
            config,
            task_name,
            incoming_tx,
            outgoing_rx,
            task_counters,
            task_credit,
        )
        .await;
    });

    InterfaceHandle {
        info: InterfaceInfo {
            id,
            name: handle_name,
            hw_mtu: Some(SERIAL_HW_MTU),
            is_local_client: false,
            bitrate: None,
            tx_jitter_max_ms: None,
            ifac: None,
            mode: leviculum_core::traits::InterfaceMode::default(),
            kind: leviculum_core::traits::InterfaceKind::Serial,
            ingress_control: None,
        },
        incoming: incoming_rx,
        outgoing: outgoing_tx,
        counters,
        credit,
        // Serial-port readiness mirrors RNode (see note there).
        ready: super::ReadySignal::ready_immediate(),
    }
}

/// Send a radio config frame to the LNode firmware and wait for ACK.
///
/// Retries up to 3 times with 2-second ACK timeout each. Returns true on success.
/// This is test infrastructure, normal usage never calls this.
///
/// On ACK, if `credit` is Some, atomically update its radio params so
/// subsequent `try_charge` calls price airtime under the new profile.
async fn send_radio_config(
    port: &mut tokio_serial::SerialStream,
    config: &SerialRadioConfig,
    name: &str,
    credit: Option<&Arc<Mutex<super::airtime::AirtimeCredit>>>,
) -> bool {
    use leviculum_core::rnode::{RadioConfigWire, RADIO_CONFIG_ACK};

    let wire = RadioConfigWire {
        frequency_hz: config.frequency as u32,
        bandwidth_hz: config.bandwidth,
        sf: config.spreading_factor,
        cr: config.coding_rate,
        tx_power_dbm: config.tx_power,
        preamble_len: config.preamble_len,
        csma_enabled: config.csma_enabled,
        radio_silent: false,
        // Airtime limits are enforced by the LNode firmware's airtime lock.
        // Short-term stays unset (no config key spells one); long-term is
        // resolved in `serial_radio_config` — lawful for the frequency
        // unless `airtime_limit_long` says otherwise.
        st_alock: 0,
        lt_alock: config.lt_alock,
        // Send-side only; `build_radio_config_frame` always emits the full
        // 21-byte frame, so the receiver parses the lt_alock field as present.
        lt_alock_present: true,
    };
    let payload = leviculum_core::rnode::build_radio_config_frame(&wire);
    let mut frame_buf = Vec::new();
    frame(&payload, &mut frame_buf);

    for attempt in 1..=3u8 {
        tracing::info!(
            "Serial {}: sending radio config (attempt {}/3): freq={} sf={} bw={} cr={} txp={}",
            name,
            attempt,
            config.frequency,
            config.spreading_factor,
            config.bandwidth,
            config.coding_rate,
            config.tx_power
        );
        if let Err(e) = port.write_all(&frame_buf).await {
            tracing::warn!("Serial {}: config write failed: {}", name, e);
            continue;
        }
        if let Err(e) = port.flush().await {
            tracing::warn!("Serial {}: config flush failed: {}", name, e);
            continue;
        }

        // Wait for ACK
        let mut deframer = Deframer::new();
        let mut buf = [0u8; 64];
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                tracing::warn!("Serial {}: config ACK timeout (attempt {})", name, attempt);
                break;
            }
            match tokio::time::timeout(remaining, port.read(&mut buf)).await {
                Ok(Ok(n)) if n > 0 => {
                    for r in deframer.process(&buf[..n]) {
                        if let DeframeResult::Frame(data) = r {
                            if data.len() == RADIO_CONFIG_ACK.len()
                                && data[..] == RADIO_CONFIG_ACK[..]
                            {
                                tracing::info!("Serial {}: radio config ACK received", name);
                                // Update the host-side airtime bucket to price
                                // subsequent charges under the newly-applied
                                // radio profile.
                                if let Some(credit) = credit {
                                    credit.lock_recover().update_radio_params(
                                        config.bandwidth,
                                        config.spreading_factor,
                                        config.coding_rate,
                                        config.preamble_len,
                                    );
                                }
                                return true;
                            }
                        }
                    }
                }
                Ok(Ok(_)) => break, // EOF
                Ok(Err(e)) => {
                    tracing::warn!("Serial {}: config ACK read error: {}", name, e);
                    break;
                }
                Err(_) => break, // timeout
            }
        }
    }
    tracing::error!("Serial {}: radio config failed after 3 attempts", name);
    false
}

/// Reconnect wrapper for serial port connections.
///
/// Owns channel endpoints across reconnection cycles. On port loss, waits
/// RECONNECT_INTERVAL and retries. Follows the TCP reconnect pattern.
async fn serial_reconnect_task(
    id: InterfaceId,
    config: SerialInterfaceConfig,
    name: String,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    counters: Arc<InterfaceCounters>,
    credit: Option<Arc<Mutex<super::airtime::AirtimeCredit>>>,
) {
    let mut has_connected_before = false;
    loop {
        // Set low_latency mode so the kernel batches USB CDC-ACM writes into
        // 64-byte bulk transfers instead of sending byte-by-byte. Without this,
        // HDLC frames arrive one byte at a time and the receiver's frame timeout
        // discards most frames. pyserial does this via set_low_latency_mode().
        let _ = std::process::Command::new("stty")
            .args(["-F", &config.port, "low_latency"])
            .output();

        let builder = tokio_serial::new(&config.port, config.speed)
            .data_bits(config.data_bits)
            .stop_bits(config.stop_bits)
            .parity(config.parity)
            .flow_control(tokio_serial::FlowControl::None);

        match tokio_serial::SerialStream::open(&builder) {
            Ok(mut port) => {
                let is_reconnect = has_connected_before;
                has_connected_before = true;
                tracing::info!("Serial interface {} online on {}", name, config.port);

                if is_reconnect {
                    if let Some(ref notify) = config.reconnect_notify {
                        let _ = notify.try_send(id);
                    }
                }

                // Send radio config if configured (test infrastructure)
                if let Some(ref radio_cfg) = config.radio_config {
                    if !send_radio_config(&mut port, radio_cfg, &name, credit.as_ref()).await {
                        tracing::warn!(
                            "Serial {}: radio config not acknowledged, T114 uses defaults",
                            name
                        );
                    }
                }

                outgoing_rx = serial_io_task(
                    name.clone(),
                    port,
                    incoming_tx.clone(),
                    outgoing_rx,
                    Arc::clone(&counters),
                )
                .await;
                tracing::warn!("Serial interface {}: port lost, will reconnect", name);
            }
            Err(e) => {
                tracing::warn!(
                    "Serial interface {}: open {} failed: {}",
                    name,
                    config.port,
                    e
                );
            }
        }

        if incoming_tx.is_closed() {
            tracing::debug!("Serial interface {}: event loop shut down", name);
            return;
        }
        tracing::info!(
            "Serial interface {}: reconnecting in {}s",
            name,
            RECONNECT_INTERVAL.as_secs()
        );
        tokio::time::sleep(RECONNECT_INTERVAL).await;
    }
}

/// Bidirectional serial I/O task.
///
/// Read path: serial read → HDLC deframe → incoming channel
/// Write path: outgoing channel → HDLC frame → serial write → flush
///
/// Enforces:
/// - Frame timeout: partial frames discarded after 100ms of silence (Python parity)
/// - HW_MTU: deframer buffer exceeding 564 bytes is reset (prevents OOM on embedded)
///
/// Returns `outgoing_rx` on port loss for reconnect reuse.
async fn serial_io_task(
    name: String,
    mut port: tokio_serial::SerialStream,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    counters: Arc<InterfaceCounters>,
) -> mpsc::Receiver<OutgoingPacket> {
    let mut deframer = Deframer::new();
    let mut read_buf = vec![0u8; READ_BUF_SIZE];
    let mut frame_buf = Vec::with_capacity(MTU * FRAME_BUFFER_MULTIPLIER);
    let mut last_read_at = Instant::now();

    loop {
        // Compute timeout: if mid-frame, use FRAME_TIMEOUT; otherwise wait indefinitely
        let timeout = if deframer.is_in_frame() {
            let elapsed = last_read_at.elapsed();
            if elapsed >= FRAME_TIMEOUT {
                // Already expired, reset immediately
                tracing::trace!("Serial {}: frame timeout, discarding partial frame", name);
                deframer.reset();
                tokio::time::sleep(Duration::from_millis(1)).await;
                continue;
            }
            FRAME_TIMEOUT - elapsed
        } else {
            Duration::from_secs(3600) // effectively infinite
        };

        tokio::select! {
            // Read path
            result = port.read(&mut read_buf) => {
                match result {
                    Ok(0) => {
                        tracing::debug!("Serial interface {} EOF", name);
                        return outgoing_rx;
                    }
                    Ok(n) => {
                        last_read_at = Instant::now();
                        let results = deframer.process(&read_buf[..n]);
                        for r in results {
                            if let DeframeResult::Frame(data) = r {
                                counters.rx_bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
                                if incoming_tx.send(IncomingPacket { data }).await.is_err() {
                                    return outgoing_rx;
                                }
                            }
                        }
                        // HW_MTU enforcement: reset if buffer grew beyond limit
                        if deframer.buffer_len() > SERIAL_HW_MTU as usize {
                            tracing::trace!(
                                "Serial {}: frame exceeds HW_MTU ({}), discarding",
                                name, deframer.buffer_len()
                            );
                            deframer.reset();
                        }
                    }
                    Err(e) => {
                        tracing::debug!("Serial interface {} read error: {}", name, e);
                        return outgoing_rx;
                    }
                }
            }

            // Write path
            msg = outgoing_rx.recv() => {
                match msg {
                    Some(pkt) => {
                        tracing::debug!("Serial interface {} TX {} bytes", name, pkt.data.len());
                        frame(&pkt.data, &mut frame_buf);
                        if let Err(e) = port.write_all(&frame_buf).await {
                            tracing::debug!("Serial interface {} write error: {}", name, e);
                            return outgoing_rx;
                        }
                        if let Err(e) = port.flush().await {
                            tracing::debug!("Serial interface {} flush error: {}", name, e);
                            return outgoing_rx;
                        }
                        counters.tx_bytes.fetch_add(frame_buf.len() as u64, Ordering::Relaxed);
                    }
                    None => {
                        tracing::debug!("Serial interface {} outgoing channel closed", name);
                        return outgoing_rx;
                    }
                }
            }

            // Frame timeout
            _ = tokio::time::sleep(timeout) => {
                if deframer.is_in_frame() {
                    tracing::trace!("Serial {}: frame timeout, discarding partial frame", name);
                    deframer.reset();
                }
            }
        }
    }
}

/// Parse a parity string ("N", "E"/"even", "O"/"odd") to tokio_serial::Parity.
pub(crate) fn parse_parity(s: &str) -> tokio_serial::Parity {
    match s.to_lowercase().as_str() {
        "e" | "even" => tokio_serial::Parity::Even,
        "o" | "odd" => tokio_serial::Parity::Odd,
        _ => tokio_serial::Parity::None,
    }
}

/// Parse a data bits value to tokio_serial::DataBits.
pub(crate) fn parse_data_bits(n: u8) -> tokio_serial::DataBits {
    match n {
        5 => tokio_serial::DataBits::Five,
        6 => tokio_serial::DataBits::Six,
        7 => tokio_serial::DataBits::Seven,
        _ => tokio_serial::DataBits::Eight,
    }
}

/// Parse a stop bits value to tokio_serial::StopBits.
pub(crate) fn parse_stop_bits(n: u8) -> tokio_serial::StopBits {
    match n {
        2 => tokio_serial::StopBits::Two,
        _ => tokio_serial::StopBits::One,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An LNode is lawful out of the box: with no `airtime_limit_long` in
    /// the config, the frequency's own ETSI sub-band limit is what gets
    /// pushed, not the 0 (unlimited) this used to hardcode.
    #[test]
    fn absent_airtime_limit_pushes_the_lawful_limit_for_the_frequency() {
        let at = |frequency, explicit| {
            serial_radio_config(&crate::config::InterfaceConfig {
                interface_type: "SerialInterface".to_string(),
                port: Some("/dev/ttyACM0".to_string()),
                frequency: Some(frequency),
                airtime_limit_long: explicit,
                ..Default::default()
            })
            .expect("frequency present → radio config")
            .lt_alock
        };
        // 869.525 MHz is ETSI sub-band P: 10% -> 0.10 * 10000.
        assert_eq!(at(869_525_000, None), 1000);
        // 868.1 MHz is sub-band M: 1%. The limit follows the frequency, so a
        // node that moves band moves limit without touching its config.
        assert_eq!(at(868_100_000, None), 100);
        // Outside the band this build can cite, nothing is invented.
        assert_eq!(at(915_000_000, None), 0);
        // An explicit value still wins, including an explicit 0: a bench
        // that means unlimited says so, and the host does not second-guess.
        assert_eq!(at(869_525_000, Some(5.0)), 500);
        assert_eq!(at(869_525_000, Some(0.0)), 0);
    }

    /// The whole point of the key: a `preamble_symbols` written in a config
    /// file has to survive as far as the bytes on the wire. This drives the
    /// same `serial_radio_config` the driver calls and then the same
    /// `build_radio_config_frame` `send_radio_config` calls, and reads the
    /// preamble back out of the frame — so it fails if any link of the
    /// chain drops the value, not merely if the struct field is unset.
    #[test]
    fn preamble_symbols_travels_from_config_to_wire_frame() {
        let cfg = crate::config::InterfaceConfig {
            interface_type: "SerialInterface".to_string(),
            port: Some("/dev/ttyACM0".to_string()),
            frequency: Some(869_525_000),
            bandwidth: Some(125_000),
            spreading_factor: Some(10),
            coding_rate: Some(8),
            tx_power: Some(17),
            preamble_symbols: Some(18),
            ..Default::default()
        };
        let radio = serial_radio_config(&cfg).expect("frequency present → radio config");
        assert_eq!(radio.preamble_len, 18);

        let payload = leviculum_core::rnode::build_radio_config_frame(
            &leviculum_core::rnode::RadioConfigWire {
                frequency_hz: radio.frequency as u32,
                bandwidth_hz: radio.bandwidth,
                sf: radio.spreading_factor,
                cr: radio.coding_rate,
                tx_power_dbm: radio.tx_power,
                preamble_len: radio.preamble_len,
                csma_enabled: radio.csma_enabled,
                radio_silent: false,
                st_alock: 0,
                lt_alock: 0,
                lt_alock_present: true,
            },
        );
        // Strip the 2-byte magic the parser expects to be gone.
        let parsed =
            leviculum_core::rnode::parse_radio_config(&payload[2..]).expect("frame parses back");
        assert_eq!(parsed.preamble_len, 18);
    }

    /// A block that omits the key gets the preamble the RNode firmware would
    /// program for the same PHY, not a constant. At the block's own defaults
    /// (SF7/BW125) that derives 24 — the value this code pushed before the
    /// derivation existed — so every config file that named no spreading
    /// factor still means exactly what it meant.
    #[test]
    fn absent_preamble_symbols_derives_the_reference_value() {
        let cfg = crate::config::InterfaceConfig {
            interface_type: "SerialInterface".to_string(),
            port: Some("/dev/ttyACM0".to_string()),
            frequency: Some(869_525_000),
            ..Default::default()
        };
        let radio = serial_radio_config(&cfg).expect("frequency present → radio config");
        assert_eq!(radio.spreading_factor, 7);
        assert_eq!(radio.preamble_len, 24);
    }

    /// The defect this closes, at the interface boundary. The same block at
    /// SF10 used to push 24 while the RNode on the far end programmed 18,
    /// and a mixed pair resolved 4 of 20 path requests. Deriving gives 18 on
    /// both sides. Written out per spreading factor rather than looped, so a
    /// regression names the SF it broke.
    #[test]
    fn absent_preamble_symbols_scales_with_the_spreading_factor() {
        let derived_at = |sf: u8| {
            let cfg = crate::config::InterfaceConfig {
                interface_type: "SerialInterface".to_string(),
                port: Some("/dev/ttyACM0".to_string()),
                frequency: Some(869_525_000),
                bandwidth: Some(125_000),
                spreading_factor: Some(sf),
                coding_rate: Some(8),
                ..Default::default()
            };
            serial_radio_config(&cfg)
                .expect("frequency present → radio config")
                .preamble_len
        };
        assert_eq!(derived_at(7), 24);
        assert_eq!(derived_at(8), 18);
        assert_eq!(derived_at(9), 18);
        assert_eq!(derived_at(10), 18);
        assert_eq!(derived_at(11), 18);
        assert_eq!(derived_at(12), 18);
    }

    /// The override still wins over the derivation, including when it names
    /// the value the derivation would have rejected. That is what makes the
    /// corner re-measurable — an A/B over the preamble needs a way to pin the
    /// old 24 on the fixed build — and what lets a node with a
    /// non-conforming peer cope.
    #[test]
    fn explicit_preamble_symbols_overrides_the_derivation() {
        let pinned = |sf: u8, preamble: u16| {
            let cfg = crate::config::InterfaceConfig {
                interface_type: "SerialInterface".to_string(),
                port: Some("/dev/ttyACM0".to_string()),
                frequency: Some(869_525_000),
                bandwidth: Some(125_000),
                spreading_factor: Some(sf),
                coding_rate: Some(8),
                preamble_symbols: Some(preamble),
                ..Default::default()
            };
            serial_radio_config(&cfg)
                .expect("frequency present → radio config")
                .preamble_len
        };
        // The pre-fix constant, pinned back on at the SF where it is wrong.
        assert_eq!(pinned(10, 24), 24);
        // And a value below the reference's own floor, which the derivation
        // would never produce.
        assert_eq!(pinned(10, 8), 8);
    }

    /// An LNode whose block names no `txpower` is programmed to the board
    /// maximum, which is also the firmware's own compiled default — the
    /// invariant this function's doc comment states, that every PHY default
    /// here is `RadioConfig::eu_medium`'s.
    #[test]
    fn absent_txpower_programs_the_board_maximum() {
        let cfg = crate::config::InterfaceConfig {
            interface_type: "SerialInterface".to_string(),
            port: Some("/dev/ttyACM0".to_string()),
            frequency: Some(869_525_000),
            ..Default::default()
        };
        let radio = serial_radio_config(&cfg).expect("frequency present → radio config");
        assert_eq!(radio.tx_power, 22);
        assert_eq!(
            radio.tx_power,
            leviculum_core::rnode::DEFAULT_TX_POWER_DBM,
            "the serial path and the resolver must not drift apart"
        );
    }

    /// The frequency cap reaches the serial path: the same block on a 25 mW
    /// sub-band (867.2 MHz, ERC 70-03 h1.4) resolves an absent `txpower` to
    /// the lawful 14 dBm, not the board maximum it gets at 869.525 MHz.
    #[test]
    fn absent_txpower_is_capped_on_a_25_mw_band() {
        let cfg = crate::config::InterfaceConfig {
            interface_type: "SerialInterface".to_string(),
            port: Some("/dev/ttyACM0".to_string()),
            frequency: Some(867_200_000),
            ..Default::default()
        };
        let radio = serial_radio_config(&cfg).expect("frequency present → radio config");
        assert_eq!(radio.tx_power, 14);
    }

    /// And an explicit `txpower = 0` still reaches the modem as 0. This
    /// drives the same chain the driver does — config to
    /// `serial_radio_config` to the wire frame and back out of the parser —
    /// so it fails if any link collapses the absent case into the zero case.
    #[test]
    fn an_explicit_zero_txpower_reaches_the_radio_as_zero() {
        let cfg = crate::config::InterfaceConfig {
            interface_type: "SerialInterface".to_string(),
            port: Some("/dev/ttyACM0".to_string()),
            frequency: Some(869_525_000),
            bandwidth: Some(125_000),
            spreading_factor: Some(7),
            coding_rate: Some(5),
            tx_power: Some(0),
            ..Default::default()
        };
        let radio = serial_radio_config(&cfg).expect("frequency present → radio config");
        assert_eq!(radio.tx_power, 0);

        let payload = leviculum_core::rnode::build_radio_config_frame(
            &leviculum_core::rnode::RadioConfigWire {
                frequency_hz: radio.frequency as u32,
                bandwidth_hz: radio.bandwidth,
                sf: radio.spreading_factor,
                cr: radio.coding_rate,
                tx_power_dbm: radio.tx_power,
                preamble_len: radio.preamble_len,
                csma_enabled: radio.csma_enabled,
                radio_silent: false,
                st_alock: 0,
                lt_alock: radio.lt_alock,
                lt_alock_present: true,
            },
        );
        let parsed =
            leviculum_core::rnode::parse_radio_config(&payload[2..]).expect("frame parses back");
        assert_eq!(parsed.tx_power_dbm, 0);
    }

    /// No `frequency` means a plain serial pipe, not a LoRa modem: no radio
    /// config is pushed at all, whatever the other keys say.
    #[test]
    fn no_frequency_means_no_radio_config() {
        let cfg = crate::config::InterfaceConfig {
            interface_type: "SerialInterface".to_string(),
            port: Some("/dev/ttyACM0".to_string()),
            preamble_symbols: Some(18),
            ..Default::default()
        };
        assert!(serial_radio_config(&cfg).is_none());
    }

    fn base_config(port: &str, radio: Option<SerialRadioConfig>) -> SerialInterfaceConfig {
        SerialInterfaceConfig {
            id: InterfaceId(0),
            name: "serial-test".to_string(),
            port: port.to_string(),
            speed: 115_200,
            data_bits: tokio_serial::DataBits::Eight,
            parity: tokio_serial::Parity::None,
            stop_bits: tokio_serial::StopBits::One,
            buffer_size: SERIAL_DEFAULT_BUFFER_SIZE,
            reconnect_notify: None,
            radio_config: radio,
        }
    }

    /// With a radio config present, the spawned handle carries an
    /// AirtimeCredit bucket.
    #[tokio::test(flavor = "current_thread")]
    async fn spawn_with_radio_config_populates_credit() {
        let radio = SerialRadioConfig {
            frequency: 869_525_000,
            bandwidth: 125_000,
            spreading_factor: 10,
            coding_rate: 8,
            tx_power: 17,
            preamble_len: 24,
            csma_enabled: true,
            lt_alock: 1000,
        };
        let handle = spawn_serial_interface(base_config("/dev/null-test-no-radio-a", Some(radio)));
        assert!(handle.credit.is_some());
    }

    /// Without a radio config, the spawned handle leaves credit = None.
    /// This is the "plain serial" (non-LoRa) path used by leviculum-std's
    /// rnsd_interop tests.
    #[tokio::test(flavor = "current_thread")]
    async fn spawn_without_radio_config_leaves_credit_none() {
        let handle = spawn_serial_interface(base_config("/dev/null-test-no-radio-b", None));
        assert!(handle.credit.is_none());
    }

    /// B5 wiring sanity: the Arc<Mutex<AirtimeCredit>> attached to the
    /// spawned handle is the SAME instance that `send_radio_config`'s
    /// `update_radio_params` call would mutate. Verified by spawning at
    /// SF=7, manually applying the SF=10/CR=8 reconfig through the
    /// shared Arc (mirroring what the ACK path does), and observing a
    /// concrete behavior difference on the handle-side bucket.
    ///
    /// Test construction: at SF=7 after a MTU charge, a small follow-up
    /// packet is rejected (fresh cost X_50_sf7 pushes credit below the
    /// tight SF=7 threshold). After reconfig to SF=10/CR=8 the threshold
    /// grows in magnitude (MTU airtime at SF10 is ~10× SF7), so the same
    /// carried-over deficit now leaves room for the small follow-up.
    /// The change in accept/reject is observable only if update_radio_params
    /// actually ran, so this asserts the wiring.
    ///
    /// End-to-end send_radio_config coverage requires a T114 and lives
    /// in Phase G hardware verification; this test locks down the
    /// Arc-shared-state invariant only.
    #[tokio::test(flavor = "current_thread")]
    async fn reconfig_propagates_to_handle_side_bucket() {
        let radio = SerialRadioConfig {
            frequency: 869_525_000,
            bandwidth: 125_000,
            spreading_factor: 7,
            coding_rate: 5,
            tx_power: 17,
            preamble_len: 24,
            csma_enabled: true,
            lt_alock: 1000,
        };
        let handle = spawn_serial_interface(base_config("/dev/null-test-reconfig", Some(radio)));
        let credit_arc = handle
            .credit
            .as_ref()
            .expect("radio_config present → bucket attached")
            .clone();
        // Exhaust at SF=7: a full-MTU charge puts credit at the SF7 threshold.
        {
            let mut c = credit_arc.lock().unwrap();
            c.try_charge(500, 0).expect("initial charge at SF7 fits");
            // Small follow-up at SF7 MUST fail (any positive-cost packet from
            // exactly-threshold pushes below threshold).
            assert!(
                c.try_charge(50, 0).is_err(),
                "small follow-up at SF7 should be rejected"
            );
        }
        // Simulate the ACK path's update to SF=10/CR=8 (as a scenario
        // might push via send_radio_config's post-ACK hook).
        credit_arc
            .lock()
            .unwrap()
            .update_radio_params(125_000, 10, 8, 18);
        // Under the new, more-permissive SF10 threshold, the carried-over
        // SF7 deficit leaves room for the same small packet.
        {
            let mut c = credit_arc.lock().unwrap();
            assert!(
                c.try_charge(50, 0).is_ok(),
                "small follow-up after SF7→SF10 reconfig should succeed"
            );
        }
    }
}
