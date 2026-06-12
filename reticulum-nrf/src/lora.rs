//! SX1262 LoRa radio initialization, configuration, and async task for T114.
//!
//! Uses the custom sx1262 driver on SPIM2 (SPIM3 has a MISO read bug on T114).
//! Provides an `Interface` impl for NodeCore dispatch and an async task that
//! handles half-duplex TX/RX on the radio.

extern crate alloc;

use alloc::vec::Vec;
use embassy_embedded_hal::shared_bus::asynch::spi::SpiDevice;
use embassy_nrf::gpio::{AnyPin, Input, Level, Output, OutputDrive, Pull};
use embassy_nrf::spim::{self, Spim};
use embassy_nrf::{bind_interrupts, peripherals, Peri};
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_sync::mutex::Mutex;
use reticulum_core::traits::{Interface, InterfaceError};
use reticulum_core::InterfaceId;
use static_cell::StaticCell;

use crate::sx1262::Sx1262;

/// Cumulative count of LoRa frames successfully transmitted at the radio
/// boundary (one increment per `[LORA] TX done`). Read by the OLED status
/// task on RAK4631 baseboard builds.
#[cfg(feature = "display")]
pub static LORA_TX_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Cumulative count of fully reassembled LoRa packets handed off to NodeCore
/// (one increment per `[LORA] RX … bytes` followed by a successful
/// reassembler feed).
#[cfg(feature = "display")]
pub static LORA_RX_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

// SPIM2, works on T114 (SPIM3 has a MISO read bug)
bind_interrupts!(pub struct SpiIrqs {
    SPI2 => spim::InterruptHandler<peripherals::SPI2>;
});

type SpiBus = Mutex<NoopRawMutex, Spim<'static>>;
type Spi = SpiDevice<'static, NoopRawMutex, Spim<'static>, Output<'static>>;

/// LoRa radio instance type
pub type Radio = Sx1262<Spi>;

// Channels between LoRa task and main loop
static LORA_INCOMING: Channel<CriticalSectionRawMutex, Vec<u8>, 4> = Channel::new();
static LORA_OUTGOING: Channel<CriticalSectionRawMutex, Vec<u8>, 4> = Channel::new();
static LORA_CONFIG: Channel<CriticalSectionRawMutex, RadioConfig, 1> = Channel::new();

pub struct LoRaChannels {
    pub incoming_rx: Receiver<'static, CriticalSectionRawMutex, Vec<u8>, 4>,
    pub outgoing_tx: Sender<'static, CriticalSectionRawMutex, Vec<u8>, 4>,
}

pub fn channels() -> LoRaChannels {
    LoRaChannels {
        incoming_rx: LORA_INCOMING.receiver(),
        outgoing_tx: LORA_OUTGOING.sender(),
    }
}

/// Get the sender for runtime radio config overrides (used by serial task).
pub fn config_sender() -> Sender<'static, CriticalSectionRawMutex, RadioConfig, 1> {
    LORA_CONFIG.sender()
}

// LoRaInterface for NodeCore dispatch
pub struct LoRaInterface {
    sender: Sender<'static, CriticalSectionRawMutex, Vec<u8>, 4>,
}

impl LoRaInterface {
    pub fn new(sender: Sender<'static, CriticalSectionRawMutex, Vec<u8>, 4>) -> Self {
        Self { sender }
    }
}

impl Interface for LoRaInterface {
    fn id(&self) -> InterfaceId {
        InterfaceId(1)
    }
    fn name(&self) -> &str {
        "lora_sx1262"
    }
    fn mtu(&self) -> usize {
        500
    }
    fn is_online(&self) -> bool {
        true
    }
    fn try_send(&mut self, data: &[u8]) -> Result<(), InterfaceError> {
        self.sender
            .try_send(data.to_vec())
            .map_err(|_| InterfaceError::BufferFull)
    }
}

// Radio configuration
// Re-export wire protocol constants from core for use by usb.rs
pub use reticulum_core::rnode::{
    RADIO_CONFIG_ACK as CONFIG_ACK, RADIO_CONFIG_FRAME_LEN as CONFIG_FRAME_LEN,
    RADIO_CONFIG_MAGIC as CONFIG_MAGIC,
};

pub struct RadioConfig {
    pub frequency_hz: u32,
    pub sf: u8,
    pub bw: u8, // SX1262 bandwidth register code
    pub cr: u8, // SX1262 coding rate register code
    pub tx_power_dbm: i8,
    pub preamble_len: u16,
    pub bw_hz: u32,   // human-readable bandwidth in Hz (for logging)
    pub cr_denom: u8, // human-readable coding rate denominator 5-8 (for logging)
    pub csma_enabled: bool,
    /// When true, drop every outgoing LoRa packet at the driver boundary.    /// the radio keeps listening but never transmits. Used by the
    /// integration-test runner to neutralize T114s it does not bind, so the
    /// test channel is not polluted by their Reticulum announces.
    pub radio_silent: bool,
}

impl RadioConfig {
    /// EU medium profile: 869.525 MHz, SF7, BW125, CR4/5, 17 dBm, preamble 24.
    pub fn eu_medium() -> Self {
        Self {
            frequency_hz: 869_525_000,
            sf: 7,
            bw: 0x04,
            cr: 0x01,
            tx_power_dbm: 17,
            preamble_len: 24,
            bw_hz: 125_000,
            cr_denom: 5,
            csma_enabled: true,
            radio_silent: false,
        }
    }

    /// Parse a radio config from wire format (13 bytes, after 2-byte magic stripped).
    ///
    /// Returns `None` for invalid data (wrong length, unknown bandwidth).
    pub fn from_wire(data: &[u8]) -> Option<Self> {
        let wire = reticulum_core::rnode::parse_radio_config(data)?;

        // SX1262 bandwidth register codes (datasheet Table 14-47)
        let bw = match wire.bandwidth_hz {
            7_810 => 0x00,
            10_420 => 0x08,
            15_630 => 0x01,
            20_830 => 0x09,
            31_250 => 0x02,
            41_670 => 0x0A,
            62_500 => 0x03,
            125_000 => 0x04,
            250_000 => 0x05,
            500_000 => 0x06,
            _ => return None,
        };

        // CR denominator (5-8) to SX1262 code (1-4)
        let cr = wire.cr - 4;

        Some(Self {
            frequency_hz: wire.frequency_hz,
            sf: wire.sf,
            bw,
            cr,
            tx_power_dbm: wire.tx_power_dbm,
            preamble_len: wire.preamble_len,
            bw_hz: wire.bandwidth_hz,
            cr_denom: wire.cr,
            csma_enabled: wire.csma_enabled,
            radio_silent: wire.radio_silent,
        })
    }
}

// CSMA/CA constants
/// Max CSMA attempts before forcing a TX even though the channel appears busy.
const CSMA_MAX_RETRIES: u8 = 8;
/// Initial contention window (slots). Matches the RNode firmware. Starting at
/// 2 guarantees a non-zero-slot random choice on the first retry so two nodes
/// that simultaneously detect traffic desynchronize meaningfully.
const CSMA_CW_INITIAL: u8 = 2;
/// Maximum contention window (slots) after exponential back-off.
const CSMA_CW_MAX: u8 = 64;
/// Floor for slot time, matches the 24ms slot used by the RNode firmware.
const CSMA_SLOT_MS_MIN: u64 = 24;

/// xorshift32 PRNG step. Mutates state and returns the updated value.
fn xorshift32(state: &mut u32) -> u32 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    *state
}

/// Compute CSMA slot time in ms from the current radio profile.
/// `max(24, airtime(500) / 10)`, scales with spreading factor so SF10/SF12
/// don't keep retrying inside the same airtime window.
fn compute_slot_ms(cfg: &RadioConfig) -> u64 {
    let airtime = reticulum_core::rnode::airtime_ms(500, cfg.bw_hz, cfg.sf, cfg.cr_denom);
    core::cmp::max(CSMA_SLOT_MS_MIN, airtime / 10)
}

/// Transmit one or two LoRa frames back-to-back. For split packets, both
/// frames go out without any CSMA/CAD between them, the receiver's
/// SplitReassembler expects this.
async fn transmit_all_frames(radio: &mut Radio, data: &[u8], rng_state: &mut u32) {
    let tx_start = embassy_time::Instant::now();
    let seq_nibble = (xorshift32(rng_state) as u8) & 0xF0;
    let frames = reticulum_core::rnode::build_lora_frames(data, seq_nibble);

    if frames.len() > 1 {
        crate::log::log_fmt(
            "[LORA] ",
            format_args!(
                "TX split {} bytes ({}+{})",
                data.len(),
                frames[0].len() - 1,
                frames[1].len() - 1
            ),
        );
    } else {
        crate::log::log_fmt("[LORA] ", format_args!("TX {} bytes", data.len()));
    }

    let mut tx_ok = true;
    for (i, frame) in frames.iter().enumerate() {
        match radio.transmit(frame, 5000).await {
            Ok(()) => {}
            Err(e) => {
                crate::log::log_fmt("[LORA] ", format_args!("TX err frame {}: {:?}", i, e));
                tx_ok = false;
                break;
            }
        }
    }
    let tx_ms = tx_start.elapsed().as_millis();
    if tx_ok {
        crate::log::log_fmt("[LORA] ", format_args!("TX done"));
        #[cfg(feature = "display")]
        {
            LORA_TX_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            crate::baseboard::LORA_TX_FLASH.signal(());
        }
    }
    crate::log::log_fmt(
        "[T114_LORA_LOOP] ",
        format_args!("op=tx duration_ms={}", tx_ms),
    );
}

// RX helper
/// Run one RX cycle with the given timeout. Feeds results through the split
/// reassembler and pushes reassembled payloads to `incoming_tx`.
/// Safe to call from both the idle-poll path and CSMA backoff windows.
async fn rx_once(
    radio: &mut Radio,
    rx_buf: &mut [u8; 255],
    timeout_ms: u32,
    reassembler: &mut reticulum_core::rnode::SplitReassembler,
    incoming_tx: &Sender<'static, CriticalSectionRawMutex, Vec<u8>, 4>,
    rx_timeout_count: &mut u32,
) {
    let rx_start = embassy_time::Instant::now();
    let rx_result = radio.receive(rx_buf, timeout_ms).await;
    let rx_ms = rx_start.elapsed().as_millis();
    match rx_result {
        Ok((len, status)) => {
            crate::log::log_fmt(
                "[T114_LORA_LOOP] ",
                format_args!("op=rx_success duration_ms={}", rx_ms),
            );
            let frame = &rx_buf[..len as usize];
            let h = frame;
            let n = h.len().min(9);
            if n >= 1 {
                let mut first8 = [0u8; 8];
                let copy_len = (n - 1).min(8);
                first8[..copy_len].copy_from_slice(&h[1..1 + copy_len]);
                crate::log::log_fmt(
                    "[T114_SX_RX] ",
                    format_args!(
                    "len={} first8={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x} rssi={} snr={}",
                    len, first8[0], first8[1], first8[2], first8[3],
                    first8[4], first8[5], first8[6], first8[7],
                    status.rssi, status.snr
                ),
                );
            }
            if let Some(data) = reassembler.feed(frame, *rx_timeout_count) {
                crate::log::log_fmt(
                    "[LORA] ",
                    format_args!(
                        "RX {} bytes rssi={} snr={}",
                        data.len(),
                        status.rssi,
                        status.snr
                    ),
                );
                #[cfg(feature = "display")]
                {
                    LORA_RX_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                    crate::baseboard::LORA_RX_FLASH.signal(());
                }
                let plen = data.len();
                let d = data.as_slice();
                let m = d.len().min(8);
                let mut p8 = [0u8; 8];
                p8[..m].copy_from_slice(&d[..m]);
                crate::log::log_fmt(
                    "[T114_LORA_DELIVER] ",
                    format_args!(
                        "pkt_hash8={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x} len={}",
                        p8[0], p8[1], p8[2], p8[3], p8[4], p8[5], p8[6], p8[7], plen
                    ),
                );
                incoming_tx.send(data).await;
            } else if len >= 2 && (rx_buf[0] & reticulum_core::rnode::FLAG_SPLIT) != 0 {
                crate::log::log_fmt(
                    "[LORA] ",
                    format_args!(
                        "RX split part {} bytes seq={} rssi={} snr={}",
                        len - 1,
                        rx_buf[0] >> 4,
                        status.rssi,
                        status.snr
                    ),
                );
            } else if len < 2 {
                crate::log::log_fmt("[LORA] ", format_args!("RX too short ({})", len));
            }
        }
        Err(crate::sx1262::Error::Timeout) => {
            *rx_timeout_count = rx_timeout_count.wrapping_add(1);
            crate::log::log_fmt("[T114_SX_TIMEOUT] ", format_args!(""));
            crate::log::log_fmt(
                "[T114_LORA_LOOP] ",
                format_args!("op=rx_timeout duration_ms={}", rx_ms),
            );
            if rx_timeout_count.is_multiple_of(60) {
                crate::log::log_fmt("[LORA] ", format_args!("RX idle ({})", *rx_timeout_count));
            }
        }
        Err(e) => {
            crate::log::log_fmt("[T114_SX_ERR] ", format_args!("error={:?}", e));
            crate::log::log_fmt(
                "[T114_LORA_LOOP] ",
                format_args!("op=rx_err duration_ms={}", rx_ms),
            );
            crate::log::log_fmt("[LORA] ", format_args!("RX err: {:?}", e));
        }
    }
}

// Init
//
// Board-agnostic: GPIO pins arrive as AnyPin (degraded in the bin file)
// and the SX1262-specific knobs (SPI clock, TCXO voltage) come from
// `BoardConfig`. SPI peripheral is still typed because embassy-nrf does
// not expose an erased-instance Spim. SPI2 is used on both T114 (SPI3
// has a MISO read bug there) and RAK4631 — same instance keeps the
// shared Spim<'static> type stable.
//
// 10 parameters: the radio's full pin/bus/board wiring arrives here once
// at boot. A grouping struct would only move the same ten names one file
// up; every caller is a board bring-up that lists them all anyway.
#[allow(clippy::too_many_arguments)]
pub async fn init(
    spi_periph: Peri<'static, peripherals::SPI2>,
    sck: Peri<'static, AnyPin>,
    mosi: Peri<'static, AnyPin>,
    miso: Peri<'static, AnyPin>,
    cs: Peri<'static, AnyPin>,
    reset: Peri<'static, AnyPin>,
    busy: Peri<'static, AnyPin>,
    dio1: Peri<'static, AnyPin>,
    spi_freq: spim::Frequency,
    tcxo_voltage_reg: u8,
) -> Radio {
    let mut spi_config = spim::Config::default();
    spi_config.frequency = spi_freq;

    let spi = Spim::new(spi_periph, SpiIrqs, sck, miso, mosi, spi_config);

    static SPI_BUS: StaticCell<SpiBus> = StaticCell::new();
    let spi_bus = SPI_BUS.init(Mutex::new(spi));

    let cs_pin = Output::new(cs, Level::High, OutputDrive::Standard);
    let spi_device = SpiDevice::new(spi_bus, cs_pin);

    let reset_pin = Output::new(reset, Level::High, OutputDrive::Standard);
    let busy_pin = Input::new(busy, Pull::None);
    let dio1_pin = Input::new(dio1, Pull::Down);

    Sx1262::new(spi_device, reset_pin, busy_pin, dio1_pin, tcxo_voltage_reg)
}

// LoRa async task
#[embassy_executor::task]
pub async fn lora_task(mut radio: Radio, mut config: RadioConfig) {
    let outgoing_rx = LORA_OUTGOING.receiver();
    let incoming_tx = LORA_INCOMING.sender();
    let config_rx = LORA_CONFIG.receiver();

    // Init radio
    radio.reset().await;
    let _ = radio.wait_busy().await;

    match radio.init_radio(config.frequency_hz).await {
        Ok(s) => crate::log::log_fmt("[LORA] ", format_args!("init ok, status=0x{:02X}", s.raw)),
        Err(e) => {
            crate::log::log_fmt("[LORA] ", format_args!("init FAILED: {:?}", e));
            return; // Can't continue without radio
        }
    }

    match radio
        .configure_lora(
            config.frequency_hz,
            config.sf,
            config.bw,
            config.cr,
            config.tx_power_dbm,
            config.preamble_len,
        )
        .await
    {
        Ok(()) => crate::log::log_fmt(
            "[LORA] ",
            format_args!(
                "active config: freq={} sf={} bw={} cr={} txp={} csma={}",
                config.frequency_hz,
                config.sf,
                config.bw_hz,
                config.cr_denom,
                config.tx_power_dbm,
                config.csma_enabled
            ),
        ),
        Err(e) => {
            crate::log::log_fmt("[LORA] ", format_args!("configure FAILED: {:?}", e));
            return;
        }
    }

    crate::log::log_fmt("[LORA] ", format_args!("task started"));

    let mut rx_buf = [0u8; 255];
    let mut rx_timeout_count: u32 = 0;
    let mut rng_state: u32 = 0xDEAD_BEEF; // xorshift32 seed
    let mut reassembler = reticulum_core::rnode::SplitReassembler::new();

    // CSMA state (only used when config.csma_enabled)
    let mut pending_tx: Option<Vec<u8>> = None;
    let mut csma_attempt: u8 = 0;
    let mut csma_cw: u8 = CSMA_CW_INITIAL;
    let mut slot_ms: u64 = compute_slot_ms(&config);

    loop {
        // Check for runtime radio config override (test infrastructure)
        if let Ok(new_cfg) = config_rx.try_receive() {
            match radio
                .configure_lora(
                    new_cfg.frequency_hz,
                    new_cfg.sf,
                    new_cfg.bw,
                    new_cfg.cr,
                    new_cfg.tx_power_dbm,
                    new_cfg.preamble_len,
                )
                .await
            {
                Ok(()) => {
                    crate::log::log_fmt(
                        "[LORA] ",
                        format_args!(
                            "active config: freq={} sf={} bw={} cr={} txp={} csma={}",
                            new_cfg.frequency_hz,
                            new_cfg.sf,
                            new_cfg.bw_hz,
                            new_cfg.cr_denom,
                            new_cfg.tx_power_dbm,
                            new_cfg.csma_enabled
                        ),
                    );
                    config = new_cfg;
                    slot_ms = compute_slot_ms(&config);
                }
                Err(e) => crate::log::log_fmt("[LORA] ", format_args!("reconfig FAILED: {:?}", e)),
            }
        }

        // Pick up a new packet to send if no TX is in flight. When
        // `radio_silent` is set, drop everything the stack hands us instead
        // of starting a TX, the radio stays listening but never transmits.
        // Used to keep unused test T114s from polluting the LoRa channel
        // with their own Reticulum announces.
        if pending_tx.is_none() {
            if let Ok(data) = outgoing_rx.try_receive() {
                if config.radio_silent {
                    drop(data);
                } else {
                    pending_tx = Some(data);
                    csma_attempt = 0;
                    csma_cw = CSMA_CW_INITIAL;
                }
            }
        }

        if let Some(data) = pending_tx.as_ref() {
            if !config.csma_enabled {
                transmit_all_frames(&mut radio, data, &mut rng_state).await;
                pending_tx = None;
            } else {
                match radio.cad(config.sf).await {
                    Ok(false) => {
                        // Channel clear, send the whole packet (both split
                        // frames back-to-back, no CAD between them).
                        crate::log::log_fmt(
                            "[LORA_CAD] ",
                            format_args!("busy=false attempt={}", csma_attempt),
                        );
                        crate::log::log_fmt(
                            "[LORA_CSMA_TX] ",
                            format_args!(
                                "retries={} forced=false slot_ms={}",
                                csma_attempt, slot_ms
                            ),
                        );
                        transmit_all_frames(&mut radio, data, &mut rng_state).await;
                        pending_tx = None;
                    }
                    Ok(true) => {
                        crate::log::log_fmt(
                            "[LORA_CAD] ",
                            format_args!("busy=true attempt={}", csma_attempt),
                        );
                        csma_attempt += 1;
                        if csma_attempt >= CSMA_MAX_RETRIES {
                            crate::log::log_fmt(
                                "[LORA_CSMA_TX] ",
                                format_args!(
                                    "retries={} forced=true slot_ms={}",
                                    csma_attempt, slot_ms
                                ),
                            );
                            transmit_all_frames(&mut radio, data, &mut rng_state).await;
                            pending_tx = None;
                        } else {
                            let slots = (xorshift32(&mut rng_state) as u64) % (csma_cw as u64);
                            let backoff_ms = slots * slot_ms;
                            csma_cw = core::cmp::min(csma_cw.saturating_mul(2), CSMA_CW_MAX);
                            // RX during the backoff so incoming packets aren't lost.
                            // Clamp to >=1ms, the SX1262 needs a non-zero timeout.
                            let rx_ms = backoff_ms.clamp(1, 10_000) as u32;
                            reassembler.check_timeout(rx_timeout_count, 10);
                            rx_once(
                                &mut radio,
                                &mut rx_buf,
                                rx_ms,
                                &mut reassembler,
                                &incoming_tx,
                                &mut rx_timeout_count,
                            )
                            .await;
                            continue;
                        }
                    }
                    Err(e) => {
                        crate::log::log_fmt(
                            "[LORA_CAD] ",
                            format_args!("err={:?} attempt={}", e, csma_attempt),
                        );
                        csma_attempt += 1;
                        if csma_attempt >= CSMA_MAX_RETRIES {
                            crate::log::log_fmt(
                                "[LORA_CSMA_TX] ",
                                format_args!(
                                    "retries={} forced=true slot_ms={}",
                                    csma_attempt, slot_ms
                                ),
                            );
                            transmit_all_frames(&mut radio, data, &mut rng_state).await;
                            pending_tx = None;
                        }
                        continue;
                    }
                }
            }
            // TX just completed, go back to the top of the loop to drain
            // the next pending packet immediately. This matches the original
            // tight-drain behavior and avoids an extra 500ms RX gap between
            // back-to-back outbound packets (e.g. announce rebroadcasts).
            continue;
        }

        // Queue empty, timeout stale split reassembly buffers and RX.
        reassembler.check_timeout(rx_timeout_count, 10);
        rx_once(
            &mut radio,
            &mut rx_buf,
            500,
            &mut reassembler,
            &incoming_tx,
            &mut rx_timeout_count,
        )
        .await;
    }
}
