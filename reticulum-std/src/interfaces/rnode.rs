//! RNode serial interface, detection, configuration, and data path
//!
//! Implements the full RNode lifecycle: detect → configure radio → validate →
//! go online → bidirectional data → reconnect on failure → graceful shutdown.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use rand_core::RngCore;
use reticulum_core::framing::kiss::{self, KissDeframeResult, KissDeframer};
use reticulum_core::rnode;
use reticulum_core::transport::InterfaceId;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

use super::{IncomingPacket, InterfaceCounters, InterfaceHandle, InterfaceInfo, OutgoingPacket};

// ---------------------------------------------------------------------------
// Named constants for serial protocol timing and buffers
// ---------------------------------------------------------------------------

/// Serial baud rate for all RNode devices
const SERIAL_BAUD_RATE: u32 = 115_200;
/// Device settle time after opening serial port
const DEVICE_SETTLE: Duration = Duration::from_secs(2);
/// Wait for device to process configuration
const CONFIG_PROCESS_WAIT: Duration = Duration::from_millis(250);
/// Final settle before starting I/O
const FINAL_SETTLE: Duration = Duration::from_millis(300);
/// Detection phase read timeout
const DETECT_TIMEOUT: Duration = Duration::from_millis(200);
/// Configuration validation read timeout
const VALIDATE_TIMEOUT: Duration = Duration::from_millis(2000);
/// Reconnect retry interval
const RECONNECT_INTERVAL: Duration = Duration::from_secs(5);
/// Serial read buffer size (detection + validation phases)
const SERIAL_READ_BUF: usize = 256;
/// Serial read buffer size (I/O phase, larger for sustained throughput)
const IO_READ_BUF: usize = 1024;
/// Frequency confirmation tolerance (Hz)
const FREQ_TOLERANCE_HZ: u32 = 100;
/// Device reset notification marker
const DEVICE_RESET_MARKER: u8 = 0xF8;

/// Result of an RNode detection probe
#[derive(Debug)]
struct RNodeDetectResult {
    detected: bool,
    firmware_version: Option<(u8, u8)>,
    platform: Option<u8>,
    mcu: Option<u8>,
}

/// Errors from RNode serial operations
#[derive(Debug, thiserror::Error)]
pub(crate) enum RNodeError {
    #[error("serial port error: {0}")]
    SerialPort(String),
    #[error("device not detected")]
    NotDetected,
    #[error("firmware {0}.{1} below minimum {2}.{3}")]
    FirmwareTooOld(u8, u8, u8, u8),
    #[error("radio config mismatch: {0}")]
    RadioMismatch(String),
}

impl From<tokio_serial::Error> for RNodeError {
    fn from(e: tokio_serial::Error) -> Self {
        RNodeError::SerialPort(e.to_string())
    }
}

impl From<std::io::Error> for RNodeError {
    fn from(e: std::io::Error) -> Self {
        RNodeError::SerialPort(e.to_string())
    }
}

/// Radio parameters for RNode configuration
struct RadioParams {
    frequency: u32,
    bandwidth: u32,
    tx_power: u8,
    sf: u8,
    cr: u8,
    st_alock: Option<u16>,
    lt_alock: Option<u16>,
}

/// Configuration for spawning an RNode interface
pub(crate) struct RNodeInterfaceConfig {
    pub id: InterfaceId,
    pub name: String,
    pub port_path: String,
    pub frequency: u32,
    pub bandwidth: u32,
    pub tx_power: u8,
    pub sf: u8,
    pub cr: u8,
    pub st_alock: Option<u16>,
    pub lt_alock: Option<u16>,
    pub flow_control: bool,
    pub buffer_size: usize,
    pub reconnect_notify: Option<mpsc::Sender<InterfaceId>>,
}

impl RNodeInterfaceConfig {
    fn radio_params(&self) -> RadioParams {
        RadioParams {
            frequency: self.frequency,
            bandwidth: self.bandwidth,
            tx_power: self.tx_power,
            sf: self.sf,
            cr: self.cr,
            st_alock: self.st_alock,
            lt_alock: self.lt_alock,
        }
    }
}

/// A framed packet queued for serial transmission
struct QueuedFrame {
    data: Vec<u8>,
    payload_len: u64,
    high_priority: bool,
}

/// Compute jitter ceiling from LoRa radio parameters.
///
/// The jitter window must exceed the maximum packet airtime so that two nodes
/// transmitting simultaneously have a chance to desynchronize. Uses 2x the
/// worst-case airtime (500-byte packet, CR=5), minimum 500ms for fast links.
/// No upper cap, slow links (SF10+) need wide jitter to avoid collisions
/// when airtime exceeds several seconds.
fn compute_jitter_max_ms(sf: u8, bandwidth_hz: u32) -> u64 {
    let bitrate = rnode::compute_bitrate(sf, 5, bandwidth_hz);
    if bitrate == 0 {
        return 500;
    }
    // 500 bytes * 8 bits = 4000 bits max Reticulum packet
    let airtime_ms = 4000u64 * 1000 / bitrate as u64;
    (airtime_ms * 2).max(500)
}

/// Default channel buffer size for RNode interfaces.
/// Smaller than TCP because LoRa bitrates are orders of magnitude lower.
pub(crate) const RNODE_DEFAULT_BUFFER_SIZE: usize = 64;

/// Maximum queued TX packets when flow control is active and device is busy.
/// Python uses an unbounded queue. Bounded to 64 here because at LoRa bitrates,
/// a full unbounded queue contains minutes-old stale packets. Drop oldest with warn!.
const FLOW_CONTROL_QUEUE_LIMIT: usize = 64;

// ---------------------------------------------------------------------------
// Configuration (includes detection)
// ---------------------------------------------------------------------------

/// Open + configure a serial RNode in one call. Retained for the hardware
/// smoke test; the production path uses [`open_serial_port`] +
/// [`configure_stream`] separately via the reconnect loop.
#[cfg(test)]
async fn configure_rnode(
    port_path: &str,
    radio: &RadioParams,
) -> Result<(tokio_serial::SerialStream, RNodeDetectResult), RNodeError> {
    let mut port = open_serial_port(port_path).await?;
    let detect_result = configure_stream(&mut port, radio, port_path).await?;
    Ok((port, detect_result))
}

/// Open the serial port (115200/8N1, no flow control) and wait for the device
/// to settle. Serial-specific: opening the port toggles DTR, which reboots many
/// RNode devices, hence the settle delay before any protocol I/O.
async fn open_serial_port(port_path: &str) -> Result<tokio_serial::SerialStream, RNodeError> {
    let builder = tokio_serial::new(port_path, SERIAL_BAUD_RATE)
        .data_bits(tokio_serial::DataBits::Eight)
        .stop_bits(tokio_serial::StopBits::One)
        .parity(tokio_serial::Parity::None)
        .flow_control(tokio_serial::FlowControl::None);

    let port = tokio_serial::SerialStream::open(&builder)?;

    // Wait for device to settle (reboot-on-open)
    tokio::time::sleep(DEVICE_SETTLE).await;

    Ok(port)
}

/// Detect the RNode, validate firmware, configure the radio, and confirm —
/// over an already-open, settled byte channel.
///
/// Carrier-agnostic: works on any `AsyncRead + AsyncWrite` stream, whether a
/// `tokio_serial::SerialStream` or a host-supplied channel (USB-CDC, BLE GATT,
/// mock pipe). The far end speaks RNode KISS regardless of substrate.
///
/// Sequence matches Python `RNodeInterface.configure_device()` from the detect
/// step onward (the port-open + device-settle step is the caller's, since it is
/// substrate-specific):
/// 1. Detect + validate firmware >= 1.52
/// 2. Send config commands: frequency, bandwidth, txpower, sf, cr, [st_alock], [lt_alock], radio ON
/// 3. Sleep 250ms, read confirmation frames
/// 4. Validate: frequency within 100 Hz, others exact match
/// 5. Sleep 300ms
async fn configure_stream<S>(
    port: &mut S,
    radio: &RadioParams,
    name: &str,
) -> Result<RNodeDetectResult, RNodeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // --- Detection phase ---
    let detect_result = detect_on_port(port).await?;

    // Validate firmware version
    if let Some((major, minor)) = detect_result.firmware_version {
        if !rnode::validate_firmware(major, minor) {
            return Err(RNodeError::FirmwareTooOld(
                major,
                minor,
                rnode::REQUIRED_FW_MAJ,
                rnode::REQUIRED_FW_MIN,
            ));
        }
    } else {
        return Err(RNodeError::NotDetected);
    }

    // --- Configuration phase ---
    rnode::validate_config(
        radio.frequency,
        radio.bandwidth,
        radio.tx_power,
        radio.sf,
        radio.cr,
    )
    .map_err(|e| RNodeError::RadioMismatch(e.to_string()))?;
    send_radio_config(port, radio).await?;

    // Wait for device to process configuration
    tokio::time::sleep(CONFIG_PROCESS_WAIT).await;

    // Read and validate confirmation frames
    validate_radio_config(port, radio, name).await?;

    // Final settle
    tokio::time::sleep(FINAL_SETTLE).await;

    Ok(detect_result)
}

/// Read KISS frames from the serial port until the deadline, calling `handler`
/// for each successfully deframed frame.
async fn read_frames_until_deadline<S>(
    port: &mut S,
    timeout: Duration,
    mut handler: impl FnMut(u8, &[u8]),
) -> Result<(), RNodeError>
where
    S: AsyncRead + Unpin,
{
    let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
    let mut buf = [0u8; SERIAL_READ_BUF];
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, port.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                tracing::info!("rnode read {} bytes: {:02x?}", n, &buf[..n.min(32)]);
                for frame in deframer.process(&buf[..n]) {
                    if let KissDeframeResult::Frame { command, payload } = frame {
                        tracing::debug!(
                            "rnode KISS frame: cmd=0x{:02x} len={}",
                            command,
                            payload.len()
                        );
                        handler(command, &payload);
                    }
                }
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                tracing::debug!("rnode read timeout ({:?} remaining)", remaining);
                break;
            }
        }
    }
    Ok(())
}

/// Send detect query and parse response frames from an open port.
async fn detect_on_port<S>(port: &mut S) -> Result<RNodeDetectResult, RNodeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let query = rnode::build_detect_query();
    port.write_all(&query).await?;

    let mut result = RNodeDetectResult {
        detected: false,
        firmware_version: None,
        platform: None,
        mcu: None,
    };

    read_frames_until_deadline(port, DETECT_TIMEOUT, |command, payload| match command {
        rnode::CMD_DETECT if payload.first() == Some(&rnode::DETECT_RESP) => {
            result.detected = true;
        }
        rnode::CMD_FW_VERSION => {
            result.firmware_version = rnode::decode_firmware_version(payload);
        }
        rnode::CMD_PLATFORM => {
            result.platform = payload.first().copied();
        }
        rnode::CMD_MCU => {
            result.mcu = payload.first().copied();
        }
        _ => {}
    })
    .await?;

    if !result.detected {
        return Err(RNodeError::NotDetected);
    }

    Ok(result)
}

/// Send radio configuration commands to the RNode.
async fn send_radio_config<S>(port: &mut S, radio: &RadioParams) -> Result<(), RNodeError>
where
    S: AsyncWrite + Unpin,
{
    let mut config_bytes = Vec::with_capacity(64);
    config_bytes.extend_from_slice(&rnode::build_set_frequency(radio.frequency));
    config_bytes.extend_from_slice(&rnode::build_set_bandwidth(radio.bandwidth));
    config_bytes.extend_from_slice(&rnode::build_set_txpower(radio.tx_power));
    config_bytes.extend_from_slice(&rnode::build_set_sf(radio.sf));
    config_bytes.extend_from_slice(&rnode::build_set_cr(radio.cr));
    if let Some(st) = radio.st_alock {
        config_bytes.extend_from_slice(&rnode::build_set_st_alock(st));
    }
    if let Some(lt) = radio.lt_alock {
        config_bytes.extend_from_slice(&rnode::build_set_lt_alock(lt));
    }
    config_bytes.extend_from_slice(&rnode::build_set_radio_state(rnode::RADIO_STATE_ON));

    tracing::info!("rnode: sending {} config bytes", config_bytes.len());
    port.write_all(&config_bytes).await?;
    port.flush().await?;
    tracing::info!("rnode: config sent and flushed");
    Ok(())
}

/// Read confirmation frames and validate they match the requested config.
async fn validate_radio_config<S>(
    port: &mut S,
    radio: &RadioParams,
    name: &str,
) -> Result<(), RNodeError>
where
    S: AsyncRead + Unpin,
{
    let mut confirmed_freq: Option<u32> = None;
    let mut confirmed_bw: Option<u32> = None;
    let mut confirmed_txp: Option<u8> = None;
    let mut confirmed_sf: Option<u8> = None;
    let mut confirmed_cr: Option<u8> = None;
    let mut confirmed_radio_state: Option<u8> = None;

    read_frames_until_deadline(port, VALIDATE_TIMEOUT, |command, payload| match command {
        rnode::CMD_FREQUENCY if payload.len() >= 4 => {
            confirmed_freq = Some(u32::from_be_bytes([
                payload[0], payload[1], payload[2], payload[3],
            ]));
        }
        rnode::CMD_BANDWIDTH if payload.len() >= 4 => {
            confirmed_bw = Some(u32::from_be_bytes([
                payload[0], payload[1], payload[2], payload[3],
            ]));
        }
        rnode::CMD_TXPOWER if !payload.is_empty() => {
            confirmed_txp = Some(payload[0]);
        }
        rnode::CMD_SF if !payload.is_empty() => {
            confirmed_sf = Some(payload[0]);
        }
        rnode::CMD_CR if !payload.is_empty() => {
            confirmed_cr = Some(payload[0]);
        }
        rnode::CMD_RADIO_STATE if !payload.is_empty() => {
            confirmed_radio_state = Some(payload[0]);
        }
        _ => {}
    })
    .await?;

    // Log warnings for missing confirmations to aid debugging
    if confirmed_freq.is_none() {
        tracing::debug!("{}: no frequency confirmation received", name);
    }
    if confirmed_bw.is_none() {
        tracing::debug!("{}: no bandwidth confirmation received", name);
    }
    if confirmed_txp.is_none() {
        tracing::debug!("{}: no tx_power confirmation received", name);
    }
    if confirmed_sf.is_none() {
        tracing::debug!("{}: no spreading factor confirmation received", name);
    }
    if confirmed_cr.is_none() {
        tracing::debug!("{}: no coding rate confirmation received", name);
    }
    if confirmed_radio_state.is_none() {
        tracing::debug!("{}: no radio_state confirmation received", name);
    }

    if let Some(cf) = confirmed_freq {
        if cf.abs_diff(radio.frequency) > FREQ_TOLERANCE_HZ {
            return Err(RNodeError::RadioMismatch(format!(
                "frequency: requested {} Hz, got {} Hz",
                radio.frequency, cf
            )));
        }
    }
    if let Some(cb) = confirmed_bw {
        if cb != radio.bandwidth {
            return Err(RNodeError::RadioMismatch(format!(
                "bandwidth: requested {} Hz, got {} Hz",
                radio.bandwidth, cb
            )));
        }
    }
    if let Some(ct) = confirmed_txp {
        if ct != radio.tx_power {
            return Err(RNodeError::RadioMismatch(format!(
                "tx_power: requested {} dBm, got {} dBm",
                radio.tx_power, ct
            )));
        }
    }
    if let Some(cs) = confirmed_sf {
        if cs != radio.sf {
            return Err(RNodeError::RadioMismatch(format!(
                "sf: requested {}, got {}",
                radio.sf, cs
            )));
        }
    }
    if let Some(cc) = confirmed_cr {
        if cc != radio.cr {
            return Err(RNodeError::RadioMismatch(format!(
                "cr: requested {}, got {}",
                radio.cr, cc
            )));
        }
    }
    if confirmed_radio_state == Some(rnode::RADIO_STATE_OFF) {
        return Err(RNodeError::RadioMismatch(
            "radio did not turn on".to_string(),
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// I/O task
// ---------------------------------------------------------------------------

/// Bidirectional I/O loop for a configured RNode.
///
/// Returns the `outgoing_rx` on disconnect so the reconnect wrapper can
/// reuse the same channel (matching TCP interface pattern).
///
/// Send-side jitter: packets are not sent immediately. The first packet after
/// idle gets a random 0–500ms delay (desynchronizes rebroadcasts from multiple
/// nodes). Subsequent queued packets use a fixed 50ms spacing to avoid serial
/// buffer overrun. RNode firmware CSMA handles radio-level collision avoidance.
#[allow(clippy::too_many_arguments)]
async fn rnode_io_task<S>(
    name: String,
    mut port: S,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    counters: Arc<InterfaceCounters>,
    flow_control: bool,
    jitter_max_ms: u64,
    _bandwidth_hz: u32,
    _sf: u8,
    _cr: u8,
) -> mpsc::Receiver<OutgoingPacket>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
    let mut buf = [0u8; IO_READ_BUF];
    // The RNode firmware emits CMD_READY only *after* a TX as a "next frame
    // welcome" signal — never spontaneously after init. Starting at false
    // when flow_control is on would deadlock: no TX ⇒ no CMD_READY ⇒ no
    // TX, ever. Mirrors Python RNodeInterface.py:459 which sets
    // interface_ready = True directly after validateRadioState() succeeds.
    let mut interface_ready = true;
    let mut send_queue: VecDeque<QueuedFrame> = VecDeque::new();
    let mut send_timer: Option<Pin<Box<tokio::time::Sleep>>> = None;
    let mut timer_ready = false;

    // Periodic heartbeat: send CMD_DETECT every 5 minutes to keep the
    // serial link alive and verify the RNode firmware is responsive.
    // This does NOT transmit over LoRa, it's a serial-only ping.
    const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(300);
    let mut heartbeat_timer = Box::pin(tokio::time::sleep(HEARTBEAT_INTERVAL));
    let mut heartbeat_pending = false;

    loop {
        tokio::select! {
            // Branch 1: Read from serial port
            result = port.read(&mut buf) => {
                match result {
                    Ok(0) => {
                        tracing::warn!("{}: serial port EOF", name);
                        return outgoing_rx;
                    }
                    Ok(n) => {
                        let frames = deframer.process(&buf[..n]);
                        for frame in frames {
                            if let KissDeframeResult::Frame { command, payload } = frame {
                                match command {
                                    rnode::CMD_DATA => {
                                        tracing::debug!("{}: RX {} bytes from radio", name, payload.len());
                                        // Bug #25 capture-compare: structured
                                        // event at the RNode → host serial
                                        // boundary. Mirrors `LORA_TX` on the
                                        // send side; together they let the
                                        // analysis align TX on one node with
                                        // RX on the other.
                                        tracing::debug!(
                                            target: "reticulum_std::interfaces::rnode::rx_trace",
                                            "LORA_RX iface={name} len={}",
                                            payload.len()
                                        );
                                        counters.rx_bytes.fetch_add(
                                            payload.len() as u64,
                                            std::sync::atomic::Ordering::Relaxed,
                                        );
                                        let pkt = IncomingPacket { data: payload.to_vec() };
                                        if incoming_tx.send(pkt).await.is_err() {
                                            // Event loop shut down
                                            send_goodbye(&mut port, &name).await;
                                            return outgoing_rx;
                                        }
                                    }
                                    rnode::CMD_READY => {
                                        tracing::debug!("{}: CMD_READY received", name);
                                        if flow_control {
                                            interface_ready = true;
                                        }
                                    }
                                    rnode::CMD_DETECT => {
                                        if payload.first() == Some(&rnode::DETECT_RESP)
                                            && heartbeat_pending
                                        {
                                            tracing::debug!("{}: heartbeat OK", name);
                                            heartbeat_pending = false;
                                        }
                                    }
                                    rnode::CMD_RESET => {
                                        if payload.first() == Some(&DEVICE_RESET_MARKER) {
                                            tracing::warn!("{}: device reset (0xF8)", name);
                                            return outgoing_rx;
                                        }
                                    }
                                    rnode::CMD_ERROR => {
                                        let Some(code) = payload.first().copied() else {
                                            tracing::warn!("{}: CMD_ERROR with empty payload", name);
                                            continue;
                                        };
                                        match code {
                                            rnode::ERROR_INITRADIO => {
                                                tracing::error!("{}: radio init failed", name);
                                                return outgoing_rx;
                                            }
                                            rnode::ERROR_TXFAILED => {
                                                tracing::error!("{}: TX failed", name);
                                                return outgoing_rx;
                                            }
                                            rnode::ERROR_EEPROM_LOCKED => {
                                                tracing::error!("{}: EEPROM locked", name);
                                            }
                                            rnode::ERROR_QUEUE_FULL => {
                                                tracing::warn!("{}: device TX queue full", name);
                                            }
                                            rnode::ERROR_MEMORY_LOW => {
                                                tracing::warn!("{}: device memory low", name);
                                            }
                                            rnode::ERROR_MODEM_TIMEOUT => {
                                                tracing::error!("{}: modem timeout", name);
                                                return outgoing_rx;
                                            }
                                            _ => {
                                                tracing::warn!(
                                                    "{}: unknown device error 0x{:02X}",
                                                    name, code
                                                );
                                            }
                                        }
                                    }
                                    // Bug #25 investigation telemetry: explicit parsers
                                    // for the two CSMA-related stat frames the firmware
                                    // emits unsolicited. Structured events under the
                                    // `reticulum_std::interfaces::rnode::csma_probe`
                                    // tracing target let the debugger correlate
                                    // firmware CSMA state with on-air TX behaviour.
                                    // Measurement-only; no TX-path change.
                                    rnode::CMD_STAT_CSMA if payload.len() >= 3 => {
                                        let cw_band = payload[0];
                                        let cw_min = payload[1];
                                        let cw_max = payload[2];
                                        tracing::debug!(
                                            target: "reticulum_std::interfaces::rnode::csma_probe",
                                            "CSMA_STAT iface={name} cw_band={cw_band} \
                                             cw_min={cw_min} cw_max={cw_max}"
                                        );
                                    }
                                    rnode::CMD_STAT_PHYPRM => {
                                        tracing::debug!(
                                            target: "reticulum_std::interfaces::rnode::csma_probe",
                                            "CSMA_PHY_RAW iface={name} payload_len={} bytes={:?}",
                                            payload.len(), payload
                                        );
                                        if payload.len() >= 12 {
                                            let symbol_time_ms =
                                                u16::from_be_bytes([payload[0], payload[1]]) as f32
                                                    / 1000.0;
                                            let symbol_rate =
                                                u16::from_be_bytes([payload[2], payload[3]]);
                                            let preamble_symbols =
                                                u16::from_be_bytes([payload[4], payload[5]]);
                                            let preamble_time_ms =
                                                u16::from_be_bytes([payload[6], payload[7]]);
                                            let csma_slot_time_ms =
                                                u16::from_be_bytes([payload[8], payload[9]]);
                                            let csma_difs_ms =
                                                u16::from_be_bytes([payload[10], payload[11]]);
                                            tracing::debug!(
                                                target: "reticulum_std::interfaces::rnode::csma_probe",
                                                "CSMA_PHY iface={name} symbol_time_ms={symbol_time_ms:.3} \
                                                 symbol_rate={symbol_rate} preamble_symbols={preamble_symbols} \
                                                 preamble_time_ms={preamble_time_ms} \
                                                 csma_slot_time_ms={csma_slot_time_ms} \
                                                 csma_difs_ms={csma_difs_ms}"
                                            );
                                        }
                                    }
                                    // Statistics, log at trace level
                                    // TODO: Parse stat values (RSSI, SNR, channel time,
                                    // battery, temperature) and store for rnstatus reporting
                                    //, see Codeberg issue #25
                                    rnode::CMD_STAT_RSSI
                                    | rnode::CMD_STAT_SNR
                                    | rnode::CMD_STAT_CHTM
                                    | rnode::CMD_STAT_BAT
                                    | rnode::CMD_STAT_TEMP => {
                                        tracing::trace!(
                                            "{}: stat cmd 0x{:02X} ({} bytes)",
                                            name, command, payload.len()
                                        );
                                    }
                                    _ => {
                                        tracing::trace!(
                                            "{}: unhandled cmd 0x{:02X}",
                                            name, command
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("{}: serial read error: {}", name, e);
                        return outgoing_rx;
                    }
                }
            }

            // Branch 2: Outgoing packet from driver → enqueue with jitter
            recv = outgoing_rx.recv() => {
                match recv {
                    Some(pkt) => {
                        let frame = rnode::build_data_frame(&pkt.data);
                        let high_priority = pkt.high_priority;
                        if send_queue.len() >= FLOW_CONTROL_QUEUE_LIMIT {
                            tracing::warn!("{}: send queue full, dropping oldest", name);
                            send_queue.pop_front();
                        }
                        let queued = QueuedFrame {
                            data: frame,
                            payload_len: pkt.data.len() as u64,
                            high_priority,
                        };
                        if high_priority {
                            // Insert before the first non-high-priority packet
                            let pos = send_queue
                                .iter()
                                .position(|f| !f.high_priority)
                                .unwrap_or(send_queue.len());
                            send_queue.insert(pos, queued);
                            tracing::debug!(
                                "{}: send queue: {} packets (priority insert at {})",
                                name, send_queue.len(), pos
                            );
                        } else {
                            send_queue.push_back(queued);
                        }
                        // High-priority packet at front of queue: bypass initial jitter
                        // ONLY if no CSMA spacing timer is active. The jitter timer
                        // desynchronizes announce rebroadcasts, directed traffic
                        // (proofs, link requests, data) should not wait for that.
                        // But the CSMA spacing timer (set after a TX) must NOT be
                        // bypassed, it ensures the firmware queue stays at depth 1
                        // so flush_queue() sends one frame per CSMA contest.
                        if high_priority
                            && send_queue.front().map(|f| f.high_priority).unwrap_or(false)
                            && send_timer.is_none()
                        {
                            timer_ready = true;
                            tracing::debug!(
                                "{}: send queue: {} packets (priority bypass jitter)",
                                name, send_queue.len()
                            );
                        } else if send_timer.is_none() && !timer_ready {
                            let delay = rand_core::OsRng.next_u64() % jitter_max_ms;
                            tracing::debug!(
                                "{}: send queue: {} packets, jitter {}ms",
                                name, send_queue.len(), delay
                            );
                            send_timer = Some(Box::pin(
                                tokio::time::sleep(Duration::from_millis(delay))
                            ));
                        }
                    }
                    None => {
                        // Event loop shut down
                        send_goodbye(&mut port, &name).await;
                        return outgoing_rx;
                    }
                }
            }

            // Branch 3: Send timer fires
            _ = async {
                if let Some(ref mut timer) = send_timer {
                    timer.await;
                }
            }, if send_timer.is_some() => {
                send_timer = None;
                timer_ready = true;
            }

            // Branch 4: Periodic heartbeat. CMD_DETECT ping to verify firmware
            _ = &mut heartbeat_timer => {
                let detect_frame = [kiss::FEND, rnode::CMD_DETECT, rnode::DETECT_REQ, kiss::FEND];
                if let Err(e) = port.write_all(&detect_frame).await {
                    tracing::warn!("{}: heartbeat write error: {}", name, e);
                    return outgoing_rx;
                }
                heartbeat_pending = true;
                tracing::debug!("{}: heartbeat sent", name);
                heartbeat_timer = Box::pin(tokio::time::sleep(HEARTBEAT_INTERVAL));
            }
        }

        // After any branch: try to send if both gates are open
        //   Gate 1: timer_ready (jitter/spacing delay elapsed)
        //   Gate 2: interface_ready || !flow_control
        if timer_ready && (interface_ready || !flow_control) {
            if let Some(queued) = send_queue.pop_front() {
                if let Err(e) = port.write_all(&queued.data).await {
                    tracing::warn!("{}: write error: {}", name, e);
                    return outgoing_rx;
                }
                // tcdrain: block until firmware has received all bytes.
                // Without this, write_all() returns as soon as bytes enter
                // the OS serial buffer, multiple frames accumulate in the
                // firmware queue and flush_queue() sends them all in one
                // burst without CSMA between them.
                if let Err(e) = port.flush().await {
                    tracing::warn!("{}: flush error: {}", name, e);
                    return outgoing_rx;
                }
                counters
                    .tx_bytes
                    .fetch_add(queued.payload_len, std::sync::atomic::Ordering::Relaxed);
                tracing::debug!("{}: TX {} bytes to serial", name, queued.payload_len);
                // Bug #25 capture-compare: structured event at the host →
                // RNode serial boundary. Measurement-only; DEBUG-level under
                // the dedicated target so it can be filtered independently
                // of the rest of the rnode logs.
                tracing::debug!(
                    target: "reticulum_std::interfaces::rnode::tx_trace",
                    "LORA_TX iface={name} len={}",
                    queued.payload_len
                );

                timer_ready = false;
                if flow_control {
                    interface_ready = false;
                }

                // Schedule spacing timer after every TX. The flush() above
                // ensures the firmware has received this frame before we proceed.
                // MIN_SPACING_MS gives the firmware time to move the frame from
                // its serial buffer into the TX queue. The firmware's own CSMA
                // handles radio-level collision avoidance, we don't simulate
                // airtime in software.
                {
                    send_timer = Some(Box::pin(tokio::time::sleep(Duration::from_millis(
                        rnode::MIN_SPACING_MS,
                    ))));
                }
            } else {
                timer_ready = false;
            }
        }
    }
}

/// Best-effort send radio-off + leave commands on shutdown
async fn send_goodbye<S>(port: &mut S, name: &str)
where
    S: tokio::io::AsyncWrite + Unpin,
{
    let mut goodbye = rnode::build_set_radio_state(rnode::RADIO_STATE_OFF);
    goodbye.extend_from_slice(&rnode::build_leave());
    if let Err(e) = port.write_all(&goodbye).await {
        tracing::debug!("{name}: goodbye write failed (expected on disconnect): {e}");
    }
}

// ---------------------------------------------------------------------------
// Reconnect wrapper
// ---------------------------------------------------------------------------

/// Runtime parameters for the reconnect loop, independent of how the byte
/// channel is obtained (serial path vs. host-supplied channel factory).
struct RNodeReconnectCtx {
    id: InterfaceId,
    name: String,
    radio: RadioParams,
    flow_control: bool,
    reconnect_notify: Option<mpsc::Sender<InterfaceId>>,
    jitter_max_ms: u64,
}

/// Reconnect loop: open channel → configure → I/O → on disconnect → wait → retry.
///
/// Carrier-agnostic: `connect` yields a fresh, opened (but unconfigured) byte
/// channel each attempt — a serial port for the path-based interface, or a
/// host-supplied duplex channel for [`spawn_rnode_channel_interface`]. The loop
/// runs detect/configure/validate on it via [`configure_stream`], so the
/// lifecycle is identical regardless of substrate.
async fn rnode_reconnect_task<S, C, Fut>(
    ctx: RNodeReconnectCtx,
    connect: C,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    counters: Arc<InterfaceCounters>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    C: Fn() -> Fut + Send + Sync,
    Fut: std::future::Future<Output = Result<S, RNodeError>> + Send,
{
    let radio = &ctx.radio;
    let bitrate_bps = rnode::compute_bitrate(radio.sf, radio.cr, radio.bandwidth);
    tracing::debug!(
        "{}: bitrate={} bps, min_spacing={}ms, jitter_max={}ms (airtime-based)",
        ctx.name,
        bitrate_bps,
        rnode::MIN_SPACING_MS,
        ctx.jitter_max_ms,
    );
    let mut has_connected_before = false;

    loop {
        // Open the channel, then configure it. Combined so either step's error
        // routes through the same reconnect-retry path.
        let opened = async {
            let mut port = connect().await?;
            let detect = configure_stream(&mut port, radio, &ctx.name).await?;
            Ok::<_, RNodeError>((port, detect))
        }
        .await;

        match opened {
            Ok((port, detect)) => {
                let is_reconnect = has_connected_before;
                has_connected_before = true;

                if let Some((major, minor)) = detect.firmware_version {
                    tracing::info!(
                        "{}: configured (FW {}.{}, freq={} Hz, bw={} Hz, sf={}, cr={}, txp={} dBm)",
                        ctx.name,
                        major,
                        minor,
                        radio.frequency,
                        radio.bandwidth,
                        radio.sf,
                        radio.cr,
                        radio.tx_power
                    );
                }

                // Notify driver about reconnection so it can re-announce
                if is_reconnect {
                    if let Some(ref notify) = ctx.reconnect_notify {
                        if let Err(e) = notify.try_send(ctx.id) {
                            tracing::warn!("{}: reconnect notify failed: {}", ctx.name, e);
                        }
                    }
                }

                outgoing_rx = rnode_io_task(
                    ctx.name.clone(),
                    port,
                    incoming_tx.clone(),
                    outgoing_rx,
                    Arc::clone(&counters),
                    ctx.flow_control,
                    ctx.jitter_max_ms,
                    radio.bandwidth,
                    radio.sf,
                    radio.cr,
                )
                .await;

                tracing::warn!("{}: disconnected", ctx.name);
            }
            Err(e) => {
                tracing::warn!("{}: configuration failed: {}", ctx.name, e);
            }
        }

        // Check if event loop shut down
        if incoming_tx.is_closed() {
            tracing::debug!("{}: event loop shut down, stopping reconnect", ctx.name);
            return;
        }

        tokio::time::sleep(RECONNECT_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------------
// Custom byte-channel factory (phone-attached radios)
// ---------------------------------------------------------------------------

/// The two boxed halves of a duplex byte channel, as yielded by an
/// [`RNodeChannelFactory`]. Separate read/write halves rather than a single
/// `AsyncRead + AsyncWrite` object because a trait object can name only one
/// non-marker trait; the interface re-joins them with [`tokio::io::join`].
pub type RNodeChannelHalves = (
    Box<dyn AsyncRead + Send + Unpin>,
    Box<dyn AsyncWrite + Send + Unpin>,
);

/// The future returned by [`RNodeChannelFactory::open`]: resolves to a fresh
/// pair of channel halves, or a boxed error.
pub type RNodeChannelOpenFuture = Pin<
    Box<
        dyn std::future::Future<
                Output = Result<RNodeChannelHalves, Box<dyn std::error::Error + Send + Sync>>,
            > + Send,
    >,
>;

/// A factory the reconnect loop calls to obtain a fresh duplex byte channel to
/// the RNode firmware.
///
/// Lets a host application supply the radio I/O over any substrate — USB-CDC,
/// BLE GATT (notify characteristic for read + write characteristic for write),
/// BT-Classic SPP, or an in-process mock pipe — on platforms where leviculum
/// never sees `/dev/ttyACM*` and cannot `open()` a serial path (Android, iOS).
/// The far end still speaks RNode KISS; leviculum still drives
/// detection/configuration/lifecycle. `open` is called once per (re)connection
/// attempt and should return a freshly-established channel each time.
pub trait RNodeChannelFactory: Send + Sync + 'static {
    /// Open a fresh duplex byte channel to the radio.
    fn open(&self) -> RNodeChannelOpenFuture;
}

/// Configuration for spawning an RNode interface over a host-supplied byte
/// channel (see [`RNodeChannelFactory`]). Mirrors [`RNodeInterfaceConfig`] but
/// replaces `port_path` with a `channel_factory`.
pub(crate) struct RNodeChannelInterfaceConfig {
    pub id: InterfaceId,
    pub name: String,
    pub channel_factory: Arc<dyn RNodeChannelFactory>,
    pub frequency: u32,
    pub bandwidth: u32,
    pub tx_power: u8,
    pub sf: u8,
    pub cr: u8,
    pub st_alock: Option<u16>,
    pub lt_alock: Option<u16>,
    pub flow_control: bool,
    pub buffer_size: usize,
    pub reconnect_notify: Option<mpsc::Sender<InterfaceId>>,
}

impl RNodeChannelInterfaceConfig {
    fn radio_params(&self) -> RadioParams {
        RadioParams {
            frequency: self.frequency,
            bandwidth: self.bandwidth,
            tx_power: self.tx_power,
            sf: self.sf,
            cr: self.cr,
            st_alock: self.st_alock,
            lt_alock: self.lt_alock,
        }
    }
}

/// Builder-side description of a channel-backed RNode interface, captured by
/// [`ReticulumNodeBuilder::add_rnode_channel_interface`](crate::driver::ReticulumNodeBuilder::add_rnode_channel_interface)
/// and turned into an [`RNodeChannelInterfaceConfig`] (with an assigned id and
/// name) when the node initializes its interfaces.
pub(crate) struct RNodeChannelSpec {
    pub factory: Arc<dyn RNodeChannelFactory>,
    pub frequency: u32,
    pub bandwidth: u32,
    pub tx_power: u8,
    pub sf: u8,
    pub cr: u8,
    pub st_alock: Option<u16>,
    pub lt_alock: Option<u16>,
    pub flow_control: bool,
    pub buffer_size: usize,
}

// ---------------------------------------------------------------------------
// Spawn
// ---------------------------------------------------------------------------

/// Wire up channels + counters, spawn the reconnect task with the given
/// connector, and return an `InterfaceHandle`. Shared by both the serial
/// (`spawn_rnode_interface`) and channel (`spawn_rnode_channel_interface`)
/// entry points.
fn spawn_rnode_with_connector<S, C, Fut>(
    ctx: RNodeReconnectCtx,
    buffer_size: usize,
    connect: C,
) -> InterfaceHandle
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    C: Fn() -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<S, RNodeError>> + Send,
{
    let (incoming_tx, incoming_rx) = mpsc::channel(buffer_size);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(buffer_size);
    let counters = Arc::new(InterfaceCounters::new());

    let id = ctx.id;
    let name = ctx.name.clone();
    let task_counters = Arc::clone(&counters);
    let bitrate = rnode::compute_bitrate(ctx.radio.sf, ctx.radio.cr, ctx.radio.bandwidth);

    tokio::spawn(async move {
        rnode_reconnect_task(ctx, connect, incoming_tx, outgoing_rx, task_counters).await;
    });

    InterfaceHandle {
        info: InterfaceInfo {
            id,
            name,
            hw_mtu: Some(rnode::HW_MTU as u32),
            is_local_client: false,
            bitrate: Some(bitrate),
            ifac: None,
        },
        incoming: incoming_rx,
        outgoing: outgoing_tx,
        counters,
        credit: None,
        // RNode readiness is async (channel open + firmware probe + radio
        // config) but is currently outside the scope of
        // wait_for_interface_ready (TCP-client race is the bug we
        // fixed in this batch).  Pre-signal so the API doesn't block
        // on RNode interfaces; future work can convert this to a
        // post-open signal if needed.
        ready: super::ReadySignal::ready_immediate(),
    }
}

fn reconnect_ctx_from_radio(
    id: InterfaceId,
    name: String,
    radio: RadioParams,
    flow_control: bool,
    reconnect_notify: Option<mpsc::Sender<InterfaceId>>,
) -> RNodeReconnectCtx {
    let jitter_max_ms = compute_jitter_max_ms(radio.sf, radio.bandwidth);
    RNodeReconnectCtx {
        id,
        name,
        radio,
        flow_control,
        reconnect_notify,
        jitter_max_ms,
    }
}

/// Spawn a complete RNode interface over a serial port, with reconnection.
///
/// Creates channels + counters, spawns the reconnect task, and returns an
/// `InterfaceHandle` for the event loop. Each (re)connection opens the serial
/// port fresh via [`open_serial_port`].
pub(crate) fn spawn_rnode_interface(config: RNodeInterfaceConfig) -> InterfaceHandle {
    let ctx = reconnect_ctx_from_radio(
        config.id,
        config.name.clone(),
        config.radio_params(),
        config.flow_control,
        config.reconnect_notify,
    );
    let buffer_size = config.buffer_size;
    let port_path = config.port_path;
    spawn_rnode_with_connector(ctx, buffer_size, move || {
        let path = port_path.clone();
        async move { open_serial_port(&path).await }
    })
}

/// Spawn a complete RNode interface over a host-supplied byte channel, with
/// reconnection. The lifecycle (detect → configure → online → I/O →
/// reconnect-on-drop) is identical to [`spawn_rnode_interface`]; only the
/// transport differs. Each (re)connection calls
/// [`RNodeChannelFactory::open`] for a fresh duplex channel.
pub(crate) fn spawn_rnode_channel_interface(
    config: RNodeChannelInterfaceConfig,
) -> InterfaceHandle {
    let ctx = reconnect_ctx_from_radio(
        config.id,
        config.name.clone(),
        config.radio_params(),
        config.flow_control,
        config.reconnect_notify,
    );
    let buffer_size = config.buffer_size;
    let factory = config.channel_factory;
    spawn_rnode_with_connector(ctx, buffer_size, move || {
        let factory = Arc::clone(&factory);
        async move {
            let (read_half, write_half) = factory
                .open()
                .await
                .map_err(|e| RNodeError::SerialPort(e.to_string()))?;
            // Re-join the two boxed halves into one AsyncRead + AsyncWrite stream
            // for the carrier-agnostic configure/IO path.
            Ok(tokio::io::join(read_half, write_half))
        }
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// In-process RNode firmware stub over one half of a `tokio::io::duplex`
    /// pair. Answers the detect probe (so firmware validation passes) and echoes
    /// each radio-config command back as its confirmation. When it receives the
    /// first outbound `CMD_DATA` from the interface (proving the I/O phase is
    /// live) it records the payload and injects one inbound data frame in reply
    /// — injecting earlier would have it swallowed by the config-validation
    /// read window, which ignores data frames.
    async fn rnode_firmware_stub(
        mut peer: tokio::io::DuplexStream,
        inbound: Vec<u8>,
        got_outbound: tokio::sync::mpsc::Sender<Vec<u8>>,
    ) {
        let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
        let mut buf = [0u8; 512];
        let mut injected = false;
        // kiss::frame clears its output each call, so accumulate via a scratch.
        let push = |reply: &mut Vec<u8>, cmd: u8, payload: &[u8]| {
            let mut one = Vec::new();
            kiss::frame(cmd, payload, &mut one);
            reply.extend_from_slice(&one);
        };
        loop {
            let n = match peer.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            let mut reply: Vec<u8> = Vec::new();
            for f in deframer.process(&buf[..n]) {
                if let KissDeframeResult::Frame { command, payload } = f {
                    match command {
                        rnode::CMD_DETECT => {
                            // Answer the probe: detected + firmware >= required.
                            push(&mut reply, rnode::CMD_DETECT, &[rnode::DETECT_RESP]);
                            push(
                                &mut reply,
                                rnode::CMD_FW_VERSION,
                                &[rnode::REQUIRED_FW_MAJ, rnode::REQUIRED_FW_MIN],
                            );
                            push(&mut reply, rnode::CMD_PLATFORM, &[rnode::PLATFORM_ESP32]);
                            push(&mut reply, rnode::CMD_MCU, &[0x00]);
                        }
                        rnode::CMD_FREQUENCY
                        | rnode::CMD_BANDWIDTH
                        | rnode::CMD_TXPOWER
                        | rnode::CMD_SF
                        | rnode::CMD_CR
                        | rnode::CMD_RADIO_STATE => {
                            // Echo the requested value back as confirmation.
                            push(&mut reply, command, &payload);
                        }
                        rnode::CMD_DATA => {
                            let _ = got_outbound.try_send(payload.to_vec());
                            if !injected {
                                injected = true;
                                push(&mut reply, rnode::CMD_DATA, &inbound);
                            }
                        }
                        _ => {}
                    }
                }
            }
            if !reply.is_empty() && peer.write_all(&reply).await.is_err() {
                return;
            }
        }
    }

    // #19: a host-supplied byte channel drives the full RNode lifecycle —
    // detect → configure → online → outbound frame → inbound frame — with no
    // serial port, via spawn_rnode_channel_interface + RNodeChannelFactory.
    #[tokio::test]
    async fn test_rnode_channel_interface_lifecycle() {
        // The factory hands leviculum one (split) half of an in-memory duplex;
        // the firmware stub owns the other half.
        let (port, peer) = tokio::io::duplex(64 * 1024);
        let (read_half, write_half) = tokio::io::split(port);
        let halves: std::sync::Mutex<Option<RNodeChannelHalves>> = std::sync::Mutex::new(Some((
            Box::new(read_half) as Box<dyn AsyncRead + Send + Unpin>,
            Box::new(write_half) as Box<dyn AsyncWrite + Send + Unpin>,
        )));

        struct MockFactory(std::sync::Mutex<Option<RNodeChannelHalves>>);
        impl RNodeChannelFactory for MockFactory {
            fn open(&self) -> RNodeChannelOpenFuture {
                let taken = self.0.lock().unwrap().take();
                Box::pin(async move { taken.ok_or_else(|| "channel already opened".into()) })
            }
        }

        let (got_tx, mut got_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
        let stub = tokio::spawn(async move {
            rnode_firmware_stub(peer, b"inbound-over-channel".to_vec(), got_tx).await;
        });

        let mut handle = spawn_rnode_channel_interface(RNodeChannelInterfaceConfig {
            id: InterfaceId(0),
            name: "rnode_channel_test".to_string(),
            channel_factory: Arc::new(MockFactory(halves)),
            frequency: 868_000_000,
            bandwidth: 125_000,
            tx_power: 17,
            sf: 7,
            cr: 5,
            st_alock: None,
            lt_alock: None,
            flow_control: false,
            buffer_size: RNODE_DEFAULT_BUFFER_SIZE,
            reconnect_notify: None,
        });

        // Bitrate is computed at spawn from the radio params.
        assert!(handle.info.bitrate.is_some());

        // Queue an outbound packet. Once detect+configure complete and the I/O
        // phase begins, it must traverse the channel to the firmware stub.
        handle
            .outgoing
            .send(OutgoingPacket {
                data: b"outbound-over-channel".to_vec(),
                high_priority: false,
            })
            .await
            .expect("send to interface");

        // The stub records the outbound payload (proves detect → configure →
        // online completed over the channel) and injects an inbound frame in
        // reply, which must surface on the interface's incoming channel.
        let outbound = tokio::time::timeout(Duration::from_secs(8), got_rx.recv())
            .await
            .expect("outbound CMD_DATA must reach the stub within 8s (lifecycle reached online)")
            .expect("got_outbound channel open");
        assert_eq!(outbound, b"outbound-over-channel");

        let incoming = tokio::time::timeout(Duration::from_secs(8), handle.incoming.recv())
            .await
            .expect("inbound packet must arrive within 8s")
            .expect("incoming channel open");
        assert_eq!(incoming.data, b"inbound-over-channel");

        stub.abort();
    }

    #[tokio::test]
    #[ignore] // Requires RNode hardware at /dev/ttyACM0
    async fn test_configure_real_rnode() {
        let radio = RadioParams {
            frequency: 868_000_000,
            bandwidth: 125_000,
            tx_power: 17,
            sf: 7,
            cr: 5,
            st_alock: None,
            lt_alock: None,
        };
        let result = configure_rnode("/dev/ttyACM0", &radio).await;

        match result {
            Ok((mut port, detect)) => {
                println!("RNode configured successfully!");
                if let Some((major, minor)) = detect.firmware_version {
                    println!("  Firmware: {major}.{minor}");
                }
                if let Some(platform) = detect.platform {
                    let name = match platform {
                        rnode::PLATFORM_ESP32 => "ESP32",
                        rnode::PLATFORM_NRF52 => "nRF52",
                        rnode::PLATFORM_AVR => "AVR",
                        _ => "Unknown",
                    };
                    println!("  Platform: {name} (0x{platform:02X})");
                }
                if let Some(mcu) = detect.mcu {
                    println!("  MCU: 0x{mcu:02X}");
                }
                // Turn radio off and send leave
                send_goodbye(&mut port, "test_rnode").await;
                println!("  Radio off, leave sent");
            }
            Err(e) => {
                panic!("Configuration failed: {e}");
            }
        }
    }

    #[tokio::test]
    #[ignore] // Requires RNode hardware at /dev/ttyACM0
    async fn test_rnode_interface_lifecycle() {
        let config = RNodeInterfaceConfig {
            id: InterfaceId(0),
            name: "test_rnode".to_string(),
            port_path: "/dev/ttyACM0".to_string(),
            frequency: 868_000_000,
            bandwidth: 125_000,
            tx_power: 17,
            sf: 7,
            cr: 5,
            st_alock: None,
            lt_alock: None,
            flow_control: true,
            buffer_size: RNODE_DEFAULT_BUFFER_SIZE,
            reconnect_notify: None,
        };

        let mut handle = spawn_rnode_interface(config);

        // Wait for the interface to come online (~5s for detect + configure)
        println!("Waiting for RNode to come online...");
        tokio::time::sleep(Duration::from_secs(6)).await;

        assert!(
            handle.info.bitrate.is_some(),
            "bitrate should be computed at spawn"
        );
        println!("Bitrate: {} bps", handle.info.bitrate.unwrap());

        // Send a test packet via outgoing channel
        let test_data = b"Hello from Rust RNode test";
        handle
            .outgoing
            .send(OutgoingPacket {
                data: test_data.to_vec(),
                high_priority: false,
            })
            .await
            .expect("send should succeed");
        println!("Sent test packet ({} bytes)", test_data.len());

        // Brief wait, then drop the handle to trigger shutdown
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Drop outgoing sender to signal shutdown
        drop(handle.outgoing);

        // Read remaining incoming (drain)
        while let Ok(pkt) = handle.incoming.try_recv() {
            println!("Received: {} bytes", pkt.data.len());
        }

        println!("Interface lifecycle test complete");
    }

    /// Reproduce the flow-control startup deadlock without hardware.
    ///
    /// With `flow_control = true`, the io task historically initialised
    /// `interface_ready = false` and only flipped it on receipt of a
    /// `CMD_READY` (0x0F) frame. The RNode firmware sends `CMD_READY`
    /// **after** each TX as a "I can accept the next frame" signal — never
    /// spontaneously after init. That produced a chicken-and-egg stall: no
    /// TX ⇒ no `CMD_READY` ⇒ no TX, ever. Observed on miauhaus 2026-04-29
    /// as 0 bytes TX in 24 minutes uptime while the send queue spammed
    /// "send queue full, dropping oldest".
    ///
    /// Test fails (timeout) before the fix, passes after.
    #[tokio::test]
    async fn test_flow_control_initial_ready_no_stall() {
        // tokio::io::duplex gives us a pair of in-memory streams: `port` is
        // what the io task drives; `peer` simulates the RNode-side serial
        // endpoint that we read TX bytes from. The peer never sends
        // CMD_READY — the test's whole point is that the first TX must go
        // through *without* one.
        let (port, mut peer) = tokio::io::duplex(8192);

        let (incoming_tx, _incoming_rx) = mpsc::channel::<IncomingPacket>(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<OutgoingPacket>(16);
        let counters = Arc::new(InterfaceCounters::new());

        // jitter_max_ms = 1 so the random pre-TX jitter is effectively zero
        // and the test does not depend on a long tail.
        let task_counters = Arc::clone(&counters);
        let task = tokio::spawn(async move {
            rnode_io_task(
                "test_rnode".to_string(),
                port,
                incoming_tx,
                outgoing_rx,
                task_counters,
                /* flow_control = */ true,
                /* jitter_max_ms = */ 1,
                125_000,
                7,
                5,
            )
            .await;
        });

        // Submit one outgoing packet. With flow_control = true and the
        // fixed initial-ready bug, this must reach `peer` within a short
        // timeout. With the bug, the io task stalls waiting for
        // CMD_READY and `peer.read` times out.
        let payload = b"hello";
        outgoing_tx
            .send(OutgoingPacket {
                data: payload.to_vec(),
                high_priority: false,
            })
            .await
            .expect("send to io task");

        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(1), peer.read(&mut buf))
            .await
            .expect("io task must send first frame within 1s (would deadlock if interface_ready stayed false)")
            .expect("read from duplex");

        assert!(n >= 3, "expected at least a 3-byte KISS frame, got {n}");
        // KISS data frame: FEND CMD_DATA payload FEND
        assert_eq!(buf[0], kiss::FEND, "first byte should be KISS FEND");
        assert_eq!(buf[1], rnode::CMD_DATA, "second byte should be CMD_DATA");

        let tx_bytes = counters.tx_bytes.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            tx_bytes,
            payload.len() as u64,
            "tx_bytes counter must reflect the dispatched payload"
        );

        drop(outgoing_tx);
        let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
    }

    /// Read KISS frames from `peer` until `timeout` elapses or no more bytes
    /// arrive. Returns every successfully deframed `(command, payload)` pair
    /// in arrival order. Used by the multi-frame send tests below.
    async fn drain_kiss_frames<S: tokio::io::AsyncReadExt + Unpin>(
        peer: &mut S,
        timeout: Duration,
    ) -> Vec<(u8, Vec<u8>)> {
        let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
        let mut frames: Vec<(u8, Vec<u8>)> = Vec::new();
        let mut buf = [0u8; 256];
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, peer.read(&mut buf)).await {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => {
                    for f in deframer.process(&buf[..n]) {
                        if let KissDeframeResult::Frame { command, payload } = f {
                            frames.push((command, payload.to_vec()));
                        }
                    }
                }
                Ok(Err(_)) | Err(_) => break,
            }
        }
        frames
    }

    /// Steady-state throughput sanity for the default `flow_control = false`
    /// path: pushed packets must all reach `peer` without anyone feeding
    /// CMD_READY back. This is the configuration that lnsd (and Python-RNS)
    /// actually defaults to and that miauhaus runs after the 2026-04-30
    /// flow_control flip. Guards against a regression where someone
    /// reintroduces a hidden CMD_READY-gate on the non-flow-control path.
    #[tokio::test]
    async fn test_no_flow_control_multi_frame_throughput() {
        let (port, mut peer) = tokio::io::duplex(8192);
        let (incoming_tx, _incoming_rx) = mpsc::channel::<IncomingPacket>(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<OutgoingPacket>(16);
        let counters = Arc::new(InterfaceCounters::new());

        let task_counters = Arc::clone(&counters);
        let task = tokio::spawn(async move {
            rnode_io_task(
                "test_rnode".to_string(),
                port,
                incoming_tx,
                outgoing_rx,
                task_counters,
                /* flow_control = */ false,
                /* jitter_max_ms = */ 1,
                125_000,
                7,
                5,
            )
            .await;
        });

        let payloads: [&[u8]; 3] = [b"alpha", b"bravo", b"charlie"];
        for p in payloads.iter() {
            outgoing_tx
                .send(OutgoingPacket {
                    data: p.to_vec(),
                    high_priority: false,
                })
                .await
                .expect("send to io task");
        }

        // 3 × MIN_SPACING_MS (50ms) + jitter (~1ms) + serial latency.
        // 1s is generous; the io task should drain all three within ~150ms.
        let frames = drain_kiss_frames(&mut peer, Duration::from_secs(1)).await;
        let data_frames: Vec<&Vec<u8>> = frames
            .iter()
            .filter(|(c, _)| *c == rnode::CMD_DATA)
            .map(|(_, p)| p)
            .collect();

        assert_eq!(
            data_frames.len(),
            3,
            "all three frames must reach the peer; got {} CMD_DATA frames out of {} total",
            data_frames.len(),
            frames.len()
        );
        for (i, p) in payloads.iter().enumerate() {
            assert_eq!(
                data_frames[i].as_slice(),
                *p,
                "frame {} payload mismatch",
                i
            );
        }

        let tx_bytes = counters.tx_bytes.load(std::sync::atomic::Ordering::Relaxed);
        let total_payload: usize = payloads.iter().map(|p| p.len()).sum();
        assert_eq!(
            tx_bytes, total_payload as u64,
            "tx_bytes counter must sum payloads"
        );

        drop(outgoing_tx);
        let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
    }

    /// Steady-state throughput when the firmware DOES emit CMD_READY after
    /// every TX (the contract `flow_control = true` was designed for):
    /// each TX is followed by a `CMD_READY` (0x0F) on the peer side, which
    /// re-arms `interface_ready` and lets the next queued packet ship.
    /// Documents the intended `flow_control = true` round-trip. If a future
    /// firmware delivers CMD_READY reliably and we want to flip the default
    /// back, this test guards the wire-side handshake.
    #[tokio::test]
    async fn test_flow_control_with_cmd_ready_multi_frame_throughput() {
        let (port, mut peer) = tokio::io::duplex(8192);
        let (incoming_tx, _incoming_rx) = mpsc::channel::<IncomingPacket>(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<OutgoingPacket>(16);
        let counters = Arc::new(InterfaceCounters::new());

        let task_counters = Arc::clone(&counters);
        let task = tokio::spawn(async move {
            rnode_io_task(
                "test_rnode".to_string(),
                port,
                incoming_tx,
                outgoing_rx,
                task_counters,
                /* flow_control = */ true,
                /* jitter_max_ms = */ 1,
                125_000,
                7,
                5,
            )
            .await;
        });

        let payloads: [&[u8]; 3] = [b"alpha", b"bravo", b"charlie"];

        // Push frame, read it, write CMD_READY, repeat. Sequencing one at
        // a time keeps the queue depth at 1 and guarantees each frame is
        // gated on its own CMD_READY (no race between enqueue and the
        // ready-flag flip).
        let cmd_ready_frame = [kiss::FEND, rnode::CMD_READY, kiss::FEND];
        for (i, p) in payloads.iter().enumerate() {
            outgoing_tx
                .send(OutgoingPacket {
                    data: p.to_vec(),
                    high_priority: false,
                })
                .await
                .expect("send to io task");

            let frames = drain_kiss_frames(&mut peer, Duration::from_millis(500)).await;
            let data_frames: Vec<&Vec<u8>> = frames
                .iter()
                .filter(|(c, _)| *c == rnode::CMD_DATA)
                .map(|(_, p)| p)
                .collect();
            assert_eq!(
                data_frames.len(),
                1,
                "iteration {}: expected exactly one TX frame on the wire, got {}",
                i,
                data_frames.len()
            );
            assert_eq!(
                data_frames[0].as_slice(),
                *p,
                "iteration {} payload mismatch",
                i
            );

            // Re-arm the io task by feeding CMD_READY back over the duplex.
            // Without this, iteration i+1 would stall (proven by the next
            // test below).
            peer.write_all(&cmd_ready_frame)
                .await
                .expect("write CMD_READY");
        }

        let tx_bytes = counters.tx_bytes.load(std::sync::atomic::Ordering::Relaxed);
        let total_payload: usize = payloads.iter().map(|p| p.len()).sum();
        assert_eq!(
            tx_bytes, total_payload as u64,
            "tx_bytes counter must reflect all three payloads"
        );

        drop(outgoing_tx);
        let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
    }

    /// Document the per-frame stall that hits `flow_control = true` against
    /// firmware that does not emit CMD_READY after TX (observed on RNode
    /// FW 1.85, miauhaus 2026-04-30): the io task ships exactly one frame,
    /// then waits forever for a re-arming CMD_READY that never arrives.
    ///
    /// This test pins the current behaviour as a documented contract.
    /// If someone later adds a recovery mechanism (timeout-based re-arm,
    /// auto-disable of flow_control on CMD_READY-silence, or removing the
    /// flow_control gate altogether), this test must be updated to reflect
    /// the new contract — its existence forces a deliberate decision.
    #[tokio::test]
    async fn test_flow_control_without_cmd_ready_stalls_after_first_frame() {
        let (port, mut peer) = tokio::io::duplex(8192);
        let (incoming_tx, _incoming_rx) = mpsc::channel::<IncomingPacket>(16);
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<OutgoingPacket>(16);
        let counters = Arc::new(InterfaceCounters::new());

        let task_counters = Arc::clone(&counters);
        let task = tokio::spawn(async move {
            rnode_io_task(
                "test_rnode".to_string(),
                port,
                incoming_tx,
                outgoing_rx,
                task_counters,
                /* flow_control = */ true,
                /* jitter_max_ms = */ 1,
                125_000,
                7,
                5,
            )
            .await;
        });

        let payloads: [&[u8]; 3] = [b"alpha", b"bravo", b"charlie"];
        for p in payloads.iter() {
            outgoing_tx
                .send(OutgoingPacket {
                    data: p.to_vec(),
                    high_priority: false,
                })
                .await
                .expect("send to io task");
        }

        // Generous read window. The first frame must arrive; any later frames
        // would only show up if the stall bug were silently fixed.
        let frames = drain_kiss_frames(&mut peer, Duration::from_millis(500)).await;
        let data_frames: Vec<&Vec<u8>> = frames
            .iter()
            .filter(|(c, _)| *c == rnode::CMD_DATA)
            .map(|(_, p)| p)
            .collect();

        assert_eq!(
            data_frames.len(),
            1,
            "exactly one frame must reach the wire; the others must remain queued \
             waiting for CMD_READY (got {} CMD_DATA frames)",
            data_frames.len()
        );
        assert_eq!(
            data_frames[0].as_slice(),
            payloads[0],
            "the single TX must be the first queued payload"
        );

        let tx_bytes = counters.tx_bytes.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            tx_bytes,
            payloads[0].len() as u64,
            "tx_bytes must reflect exactly the first payload"
        );

        drop(outgoing_tx);
        let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
    }
}
