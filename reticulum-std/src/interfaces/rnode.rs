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
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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

/// Open a serial port, detect the RNode, validate firmware, and configure radio.
///
/// Sequence matches Python RNodeInterface.configure_device():
/// 1. Open serial 115200/8N1
/// 2. Sleep 2s (device settle)
/// 3. Detect + validate firmware >= 1.52
/// 4. Send config commands: frequency, bandwidth, txpower, sf, cr, [st_alock], [lt_alock], radio ON
/// 5. Sleep 250ms, read confirmation frames 250ms
/// 6. Validate: frequency within 100 Hz, others exact match
/// 7. Sleep 300ms
async fn configure_rnode(
    port_path: &str,
    radio: &RadioParams,
) -> Result<(tokio_serial::SerialStream, RNodeDetectResult), RNodeError> {
    let builder = tokio_serial::new(port_path, SERIAL_BAUD_RATE)
        .data_bits(tokio_serial::DataBits::Eight)
        .stop_bits(tokio_serial::StopBits::One)
        .parity(tokio_serial::Parity::None)
        .flow_control(tokio_serial::FlowControl::None);

    let mut port = tokio_serial::SerialStream::open(&builder)?;

    // Wait for device to settle
    tokio::time::sleep(DEVICE_SETTLE).await;

    // --- Detection phase ---
    let detect_result = detect_on_port(&mut port).await?;

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
    send_radio_config(&mut port, radio).await?;

    // Wait for device to process configuration
    tokio::time::sleep(CONFIG_PROCESS_WAIT).await;

    // Read and validate confirmation frames
    validate_radio_config(&mut port, radio, port_path).await?;

    // Final settle
    tokio::time::sleep(FINAL_SETTLE).await;

    Ok((port, detect_result))
}

/// Read KISS frames from the serial port until the deadline, calling `handler`
/// for each successfully deframed frame.
async fn read_frames_until_deadline(
    port: &mut tokio_serial::SerialStream,
    timeout: Duration,
    mut handler: impl FnMut(u8, &[u8]),
) -> Result<(), RNodeError> {
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
async fn detect_on_port(
    port: &mut tokio_serial::SerialStream,
) -> Result<RNodeDetectResult, RNodeError> {
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
async fn send_radio_config(
    port: &mut tokio_serial::SerialStream,
    radio: &RadioParams,
) -> Result<(), RNodeError> {
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
async fn validate_radio_config(
    port: &mut tokio_serial::SerialStream,
    radio: &RadioParams,
    name: &str,
) -> Result<(), RNodeError> {
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
async fn rnode_io_task(
    name: String,
    mut port: tokio_serial::SerialStream,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    counters: Arc<InterfaceCounters>,
    flow_control: bool,
    jitter_max_ms: u64,
    _bandwidth_hz: u32,
    _sf: u8,
    _cr: u8,
) -> mpsc::Receiver<OutgoingPacket> {
    let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
    let mut buf = [0u8; IO_READ_BUF];
    let mut interface_ready = !flow_control; // If no flow control, always ready
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
async fn send_goodbye(port: &mut tokio_serial::SerialStream, name: &str) {
    let mut goodbye = rnode::build_set_radio_state(rnode::RADIO_STATE_OFF);
    goodbye.extend_from_slice(&rnode::build_leave());
    if let Err(e) = port.write_all(&goodbye).await {
        tracing::debug!("{name}: goodbye write failed (expected on disconnect): {e}");
    }
}

// ---------------------------------------------------------------------------
// Reconnect wrapper
// ---------------------------------------------------------------------------

/// Reconnect loop: configure → I/O → on disconnect → wait → retry.
async fn rnode_reconnect_task(
    config: RNodeInterfaceConfig,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    counters: Arc<InterfaceCounters>,
) {
    let radio = config.radio_params();
    let bitrate_bps = rnode::compute_bitrate(radio.sf, radio.cr, radio.bandwidth);
    let jitter_max_ms = compute_jitter_max_ms(radio.sf, radio.bandwidth);
    tracing::debug!(
        "{}: bitrate={} bps, min_spacing={}ms, jitter_max={}ms (airtime-based)",
        config.name,
        bitrate_bps,
        rnode::MIN_SPACING_MS,
        jitter_max_ms,
    );
    let mut has_connected_before = false;

    loop {
        match configure_rnode(&config.port_path, &radio).await {
            Ok((port, detect)) => {
                let is_reconnect = has_connected_before;
                has_connected_before = true;

                if let Some((major, minor)) = detect.firmware_version {
                    tracing::info!(
                        "{}: configured (FW {}.{}, freq={} Hz, bw={} Hz, sf={}, cr={}, txp={} dBm)",
                        config.name,
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
                    if let Some(ref notify) = config.reconnect_notify {
                        if let Err(e) = notify.try_send(config.id) {
                            tracing::warn!("{}: reconnect notify failed: {}", config.name, e);
                        }
                    }
                }

                outgoing_rx = rnode_io_task(
                    config.name.clone(),
                    port,
                    incoming_tx.clone(),
                    outgoing_rx,
                    Arc::clone(&counters),
                    config.flow_control,
                    jitter_max_ms,
                    radio.bandwidth,
                    radio.sf,
                    radio.cr,
                )
                .await;

                tracing::warn!("{}: disconnected", config.name);
            }
            Err(e) => {
                tracing::warn!("{}: configuration failed: {}", config.name, e);
            }
        }

        // Check if event loop shut down
        if incoming_tx.is_closed() {
            tracing::debug!("{}: event loop shut down, stopping reconnect", config.name);
            return;
        }

        tokio::time::sleep(RECONNECT_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------------
// Spawn
// ---------------------------------------------------------------------------

/// Spawn a complete RNode interface with reconnection support.
///
/// Creates channels + counters, spawns the reconnect task, and returns an
/// `InterfaceHandle` for the event loop.
pub(crate) fn spawn_rnode_interface(config: RNodeInterfaceConfig) -> InterfaceHandle {
    let (incoming_tx, incoming_rx) = mpsc::channel(config.buffer_size);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(config.buffer_size);
    let counters = Arc::new(InterfaceCounters::new());

    let id = config.id;
    let name = config.name.clone();
    let task_counters = Arc::clone(&counters);
    let bitrate = rnode::compute_bitrate(config.sf, config.cr, config.bandwidth);

    tokio::spawn(async move {
        rnode_reconnect_task(config, incoming_tx, outgoing_rx, task_counters).await;
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
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
}
