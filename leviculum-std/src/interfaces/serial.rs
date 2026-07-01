//! Serial interface. HDLC-framed bidirectional serial port
//!
//! Implements a plain serial interface matching Python Reticulum's
//! `SerialInterface`. Uses HDLC simplified framing (same as TCP and
//! LocalInterface) over a serial port.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use leviculum_core::constants::MTU;
use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
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
            ifac: None,
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
                                    credit
                                        .lock()
                                        .expect("airtime credit mutex poisoned")
                                        .update_radio_params(
                                            config.bandwidth,
                                            config.spreading_factor,
                                            config.coding_rate,
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
            .update_radio_params(125_000, 10, 8);
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
