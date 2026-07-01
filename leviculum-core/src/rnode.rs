//! RNode command protocol, encoding, decoding, and constants
//!
//! This module implements the RNode serial command protocol as a pure data
//! transformation layer (`no_std + alloc`). It defines command constants,
//! encoding functions that produce complete KISS frames, and decoding
//! functions that parse raw payloads from deframed KISS data.
//!
//! The RNode protocol is a KISS superset: standard KISS framing
//! (FEND/FESC/TFEND/TFESC) with RNode-specific command bytes for radio
//! configuration, statistics reporting, and device management.
//!
//! # Encoding
//!
//! Each `build_*` function returns a `Vec<u8>` containing one or more
//! complete KISS frames ready for serial transmission.
//!
//! # Decoding
//!
//! Each `decode_*` function takes the raw payload bytes (already extracted
//! from a KISS frame by [`KissDeframer`](crate::framing::kiss::KissDeframer))
//! and returns `Option<T>`, returning `None` if the payload is too short.
//! All values are returned as raw device integers, no float conversion
//! is performed in core.

use alloc::vec::Vec;

use crate::framing::kiss;

// ---------------------------------------------------------------------------
// Configuration commands
// ---------------------------------------------------------------------------

/// Data packet (standard KISS CMD_DATA)
pub const CMD_DATA: u8 = 0x00;
/// Set/report operating frequency (4 bytes BE, Hz)
pub const CMD_FREQUENCY: u8 = 0x01;
/// Set/report channel bandwidth (4 bytes BE, Hz)
pub const CMD_BANDWIDTH: u8 = 0x02;
/// Set/report TX power (1 byte, dBm)
pub const CMD_TXPOWER: u8 = 0x03;
/// Set/report spreading factor (1 byte, 5-12)
pub const CMD_SF: u8 = 0x04;
/// Set/report coding rate (1 byte, 5-8)
pub const CMD_CR: u8 = 0x05;
/// Set/report radio on/off state
pub const CMD_RADIO_STATE: u8 = 0x06;
/// Device presence detection handshake
pub const CMD_DETECT: u8 = 0x08;
/// Host disconnecting (shutdown notification)
pub const CMD_LEAVE: u8 = 0x0A;
/// Short-term airtime limit (2 bytes BE, value/100 = percent)
pub const CMD_ST_ALOCK: u8 = 0x0B;
/// Long-term airtime limit (2 bytes BE, value/100 = percent)
pub const CMD_LT_ALOCK: u8 = 0x0C;
/// Device ready for next TX packet
pub const CMD_READY: u8 = 0x0F;
/// Select subinterface for next command
/// (multi-interface support, see doc/RNODE_PROTOCOL_RESEARCH.md)
pub const CMD_SEL_INT: u8 = 0x1F;

// ---------------------------------------------------------------------------
// Statistics commands (see Codeberg issue #25)
// ---------------------------------------------------------------------------

/// Total RX packet count (4 bytes BE)
pub const CMD_STAT_RX: u8 = 0x21;
/// Total TX packet count (4 bytes BE)
pub const CMD_STAT_TX: u8 = 0x22;
/// Last packet RSSI (1 byte, unsigned + 157 offset)
pub const CMD_STAT_RSSI: u8 = 0x23;
/// Last packet SNR (1 byte, signed * 0.25 dB)
pub const CMD_STAT_SNR: u8 = 0x24;
/// Channel time/utilization stats (11 bytes single, 8 bytes multi)
pub const CMD_STAT_CHTM: u8 = 0x25;
/// Physical layer parameters (12 bytes single, 10 bytes multi)
pub const CMD_STAT_PHYPRM: u8 = 0x26;
/// Battery status (2 bytes: state, percent)
pub const CMD_STAT_BAT: u8 = 0x27;
/// CSMA contention window params (3 bytes), see Codeberg issue #25
pub const CMD_STAT_CSMA: u8 = 0x28;
/// CPU temperature (1 byte, value - 120 = Celsius)
pub const CMD_STAT_TEMP: u8 = 0x29;

// ---------------------------------------------------------------------------
// System commands
// ---------------------------------------------------------------------------

/// Query/report platform
pub const CMD_PLATFORM: u8 = 0x48;
/// Query/report MCU type
pub const CMD_MCU: u8 = 0x49;
/// Query/report firmware version (2 bytes: major, minor)
pub const CMD_FW_VERSION: u8 = 0x50;
/// Hard reset / reset notification
pub const CMD_RESET: u8 = 0x55;

// ---------------------------------------------------------------------------
// Multi-interface data commands
// (multi-interface support, see doc/RNODE_PROTOCOL_RESEARCH.md)
// ---------------------------------------------------------------------------

/// List available radio interfaces
pub const CMD_INTERFACES: u8 = 0x71;

/// Data commands for multi-interface RNodes (indexed by subinterface)
pub const CMD_INT_DATA: [u8; 12] = [
    0x00, // INT0
    0x10, // INT1
    0x20, // INT2
    0x70, // INT3
    0x75, // INT4
    0x90, // INT5 — NOTE: collides with CMD_ERROR (0x90); context disambiguates
    0xA0, // INT6
    0xB0, // INT7
    0xC0, // INT8 (collides with FEND — see research doc)
    0xD0, // INT9
    0xE0, // INT10
    0xF0, // INT11
];

// ---------------------------------------------------------------------------
// Error command and codes
// ---------------------------------------------------------------------------

/// Error report from device (0x90, same value as `CMD_INT_DATA[5]` and PLATFORM_AVR;
/// disambiguated by protocol context: command byte vs payload byte)
pub const CMD_ERROR: u8 = 0x90;

/// Radio initialization failed
pub const ERROR_INITRADIO: u8 = 0x01;
/// Transmission failed
pub const ERROR_TXFAILED: u8 = 0x02;
/// EEPROM is locked
pub const ERROR_EEPROM_LOCKED: u8 = 0x03;
/// TX queue full (single-interface only)
pub const ERROR_QUEUE_FULL: u8 = 0x04;
/// Memory exhausted (single-interface only)
pub const ERROR_MEMORY_LOW: u8 = 0x05;
/// Modem communication timeout (single-interface only)
pub const ERROR_MODEM_TIMEOUT: u8 = 0x06;

// ---------------------------------------------------------------------------
// Protocol values
// ---------------------------------------------------------------------------

/// Detect request payload (host -> device)
pub const DETECT_REQ: u8 = 0x73;
/// Detect response payload (device -> host)
pub const DETECT_RESP: u8 = 0x46;
/// Radio off
pub const RADIO_STATE_OFF: u8 = 0x00;
/// Radio on
pub const RADIO_STATE_ON: u8 = 0x01;

// ---------------------------------------------------------------------------
// Platform constants
// ---------------------------------------------------------------------------

/// AVR-based RNode
pub const PLATFORM_AVR: u8 = 0x90;
/// ESP32-based RNode
pub const PLATFORM_ESP32: u8 = 0x80;
/// nRF52-based RNode
pub const PLATFORM_NRF52: u8 = 0x70;

// ---------------------------------------------------------------------------
// Firmware requirements
// ---------------------------------------------------------------------------

/// Minimum required firmware major version
pub const REQUIRED_FW_MAJ: u8 = 1;
/// Minimum required firmware minor version
pub const REQUIRED_FW_MIN: u8 = 52;

// ---------------------------------------------------------------------------
// Frequency limits
// ---------------------------------------------------------------------------

/// Minimum allowed operating frequency (Hz)
pub const FREQ_MIN: u32 = 137_000_000;
/// Maximum allowed operating frequency (Hz), covers 2.4 GHz SX128X
pub const FREQ_MAX: u32 = 3_000_000_000;

// ---------------------------------------------------------------------------
// Hardware MTU
// ---------------------------------------------------------------------------

/// Hardware MTU for all RNode variants (bytes)
pub const HW_MTU: usize = 508;

// ---------------------------------------------------------------------------
// Decode/encode constants
// ---------------------------------------------------------------------------

/// RSSI raw-to-dBm offset (subtract from raw byte)
const RSSI_OFFSET: i16 = 157;
/// Temperature raw-to-Celsius offset (subtract from raw byte)
const TEMP_OFFSET: i16 = 120;
/// Interference sentinel (0xFF = no interference data)
const INTERFERENCE_NONE: u8 = 0xFF;
/// Maximum TX power (dBm)
pub const MAX_TX_POWER: u8 = 37;
/// Leave command payload
const LEAVE_PAYLOAD: u8 = 0xFF;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Battery state reported by the device
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryState {
    Unknown,
    Discharging,
    Charging,
    Charged,
}

/// Channel time/utilization statistics from CMD_STAT_CHTM
///
/// All `u16` values are raw device values. Divide by 100 for percent.
/// RSSI fields are already converted to dBm (raw byte - 157).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelStats {
    /// Short-term airtime (raw; /100 for percent)
    pub airtime_short: u16,
    /// Long-term airtime (raw; /100 for percent)
    pub airtime_long: u16,
    /// Short-term channel load (raw; /100 for percent)
    pub channel_load_short: u16,
    /// Long-term channel load (raw; /100 for percent)
    pub channel_load_long: u16,
    /// Current RSSI in dBm (single-interface only)
    pub current_rssi: Option<i16>,
    /// Noise floor in dBm (single-interface only)
    pub noise_floor: Option<i16>,
    /// Interference in dBm, None if 0xFF (single-interface only)
    pub interference: Option<i16>,
}

/// Physical layer parameters from CMD_STAT_PHYPRM
///
/// All `u16` values are raw device values unless otherwise noted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhyParams {
    /// Symbol time (raw; /1000 for ms)
    pub symbol_time_raw: u16,
    /// Symbol rate (baud)
    pub symbol_rate: u16,
    /// Preamble symbols
    pub preamble_symbols: u16,
    /// Preamble time (ms)
    pub preamble_time_ms: u16,
    /// CSMA slot time (ms)
    pub csma_slot_time_ms: u16,
    /// DIFS time (ms), None for multi-interface
    pub difs_time_ms: Option<u16>,
}

// ---------------------------------------------------------------------------
// Encoding functions
// ---------------------------------------------------------------------------

/// Build a single KISS frame: FEND + command + escaped payload + FEND
fn build_single_frame(cmd: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    kiss::frame(cmd, payload, &mut out);
    out
}

/// Build the 4-frame detect + query sequence
///
/// Returns the fixed 13-byte sequence that queries device presence,
/// firmware version, platform, and MCU. Frames share FEND delimiters.
///
/// ```text
/// C0 08 73 C0 50 00 C0 48 00 C0 49 00 C0
/// ```
pub fn build_detect_query() -> Vec<u8> {
    // This is a fixed sequence with shared FENDs between frames.
    // Cannot use frame() directly since it clears the output each time.
    alloc::vec![
        kiss::FEND,
        CMD_DETECT,
        DETECT_REQ,
        kiss::FEND,
        CMD_FW_VERSION,
        0x00,
        kiss::FEND,
        CMD_PLATFORM,
        0x00,
        kiss::FEND,
        CMD_MCU,
        0x00,
        kiss::FEND,
    ]
}

/// Build a CMD_FREQUENCY frame (4 bytes big-endian Hz)
pub fn build_set_frequency(hz: u32) -> Vec<u8> {
    build_single_frame(CMD_FREQUENCY, &hz.to_be_bytes())
}

/// Build a CMD_BANDWIDTH frame (4 bytes big-endian Hz)
pub fn build_set_bandwidth(hz: u32) -> Vec<u8> {
    build_single_frame(CMD_BANDWIDTH, &hz.to_be_bytes())
}

/// Build a CMD_TXPOWER frame (1 byte dBm)
pub fn build_set_txpower(dbm: u8) -> Vec<u8> {
    build_single_frame(CMD_TXPOWER, &[dbm])
}

/// Build a CMD_SF frame (1 byte spreading factor)
pub fn build_set_sf(sf: u8) -> Vec<u8> {
    build_single_frame(CMD_SF, &[sf])
}

/// Build a CMD_CR frame (1 byte coding rate)
pub fn build_set_cr(cr: u8) -> Vec<u8> {
    build_single_frame(CMD_CR, &[cr])
}

/// Build a CMD_RADIO_STATE frame (1 byte state)
pub fn build_set_radio_state(state: u8) -> Vec<u8> {
    build_single_frame(CMD_RADIO_STATE, &[state])
}

/// Build a CMD_ST_ALOCK frame (2 bytes big-endian)
///
/// Caller computes `(percent * 100.0) as u16` before calling.
pub fn build_set_st_alock(value: u16) -> Vec<u8> {
    build_single_frame(CMD_ST_ALOCK, &value.to_be_bytes())
}

/// Build a CMD_LT_ALOCK frame (2 bytes big-endian)
///
/// Caller computes `(percent * 100.0) as u16` before calling.
pub fn build_set_lt_alock(value: u16) -> Vec<u8> {
    build_single_frame(CMD_LT_ALOCK, &value.to_be_bytes())
}

/// Build a CMD_DATA frame with KISS-escaped payload
pub fn build_data_frame(data: &[u8]) -> Vec<u8> {
    build_single_frame(CMD_DATA, data)
}

/// Build a CMD_LEAVE frame (0xFF payload)
pub fn build_leave() -> Vec<u8> {
    build_single_frame(CMD_LEAVE, &[LEAVE_PAYLOAD])
}

// ---------------------------------------------------------------------------
// Decoding functions
// ---------------------------------------------------------------------------

/// Decode RSSI from CMD_STAT_RSSI payload
///
/// Returns RSSI in dBm: `raw_byte as i16 - 157`.
pub fn decode_rssi(payload: &[u8]) -> Option<i16> {
    let &byte = payload.first()?;
    Some(byte as i16 - RSSI_OFFSET)
}

/// Decode SNR from CMD_STAT_SNR payload
///
/// Returns the raw signed byte. Caller multiplies by 0.25 for dB.
pub fn decode_snr(payload: &[u8]) -> Option<i8> {
    let &byte = payload.first()?;
    Some(byte as i8)
}

/// Decode battery status from CMD_STAT_BAT payload
///
/// Returns `(BatteryState, percent)`.
pub fn decode_battery(payload: &[u8]) -> Option<(BatteryState, u8)> {
    if payload.len() < 2 {
        return None;
    }
    let state = match payload[0] {
        0x01 => BatteryState::Discharging,
        0x02 => BatteryState::Charging,
        0x03 => BatteryState::Charged,
        _ => BatteryState::Unknown,
    };
    Some((state, payload[1]))
}

/// Decode temperature from CMD_STAT_TEMP payload
///
/// Returns temperature in Celsius: `raw_byte as i16 - 120`.
pub fn decode_temperature(payload: &[u8]) -> Option<i16> {
    let &byte = payload.first()?;
    Some(byte as i16 - TEMP_OFFSET)
}

/// Decode firmware version from CMD_FW_VERSION payload
///
/// Returns `(major, minor)`.
pub fn decode_firmware_version(payload: &[u8]) -> Option<(u8, u8)> {
    if payload.len() < 2 {
        return None;
    }
    Some((payload[0], payload[1]))
}

/// Read a big-endian u16 from a slice at a given offset
fn read_be_u16(data: &[u8], offset: usize) -> Option<u16> {
    if offset + 2 > data.len() {
        return None;
    }
    Some(u16::from_be_bytes([data[offset], data[offset + 1]]))
}

/// Decode channel statistics from CMD_STAT_CHTM payload
///
/// Accepts 11-byte (single-interface) or 8-byte (multi-interface) payloads.
/// For single-interface, includes RSSI/noise_floor/interference fields.
/// Interference is `None` when the raw byte is 0xFF.
pub fn decode_channel_stats(payload: &[u8]) -> Option<ChannelStats> {
    if payload.len() < 8 {
        return None;
    }

    let airtime_short = read_be_u16(payload, 0)?;
    let airtime_long = read_be_u16(payload, 2)?;
    let channel_load_short = read_be_u16(payload, 4)?;
    let channel_load_long = read_be_u16(payload, 6)?;

    let (current_rssi, noise_floor, interference) = if payload.len() >= 11 {
        let rssi = payload[8] as i16 - RSSI_OFFSET;
        let noise = payload[9] as i16 - RSSI_OFFSET;
        let interf = if payload[10] == INTERFERENCE_NONE {
            None
        } else {
            Some(payload[10] as i16 - RSSI_OFFSET)
        };
        (Some(rssi), Some(noise), interf)
    } else {
        (None, None, None)
    };

    Some(ChannelStats {
        airtime_short,
        airtime_long,
        channel_load_short,
        channel_load_long,
        current_rssi,
        noise_floor,
        interference,
    })
}

/// Decode physical parameters from CMD_STAT_PHYPRM payload
///
/// Accepts 12-byte (single-interface) or 10-byte (multi-interface) payloads.
/// For multi-interface, `difs_time_ms` is `None`.
pub fn decode_phy_params(payload: &[u8]) -> Option<PhyParams> {
    if payload.len() < 10 {
        return None;
    }

    let symbol_time_raw = read_be_u16(payload, 0)?;
    let symbol_rate = read_be_u16(payload, 2)?;
    let preamble_symbols = read_be_u16(payload, 4)?;
    let preamble_time_ms = read_be_u16(payload, 6)?;
    let csma_slot_time_ms = read_be_u16(payload, 8)?;

    let difs_time_ms = if payload.len() >= 12 {
        Some(read_be_u16(payload, 10)?)
    } else {
        None
    };

    Some(PhyParams {
        symbol_time_raw,
        symbol_rate,
        preamble_symbols,
        preamble_time_ms,
        csma_slot_time_ms,
        difs_time_ms,
    })
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Error returned by [`validate_config`] when a radio parameter is out of range
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    FrequencyOutOfRange,
    InvalidBandwidth,
    TxPowerOutOfRange,
    SpreadingFactorOutOfRange,
    CodingRateOutOfRange,
}

impl core::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::FrequencyOutOfRange => write!(f, "frequency out of range (137 MHz - 3 GHz)"),
            Self::InvalidBandwidth => write!(f, "invalid bandwidth"),
            Self::TxPowerOutOfRange => write!(f, "TX power out of range (0-{MAX_TX_POWER} dBm)"),
            Self::SpreadingFactorOutOfRange => {
                write!(f, "spreading factor out of range (5-12)")
            }
            Self::CodingRateOutOfRange => write!(f, "coding rate out of range (5-8)"),
        }
    }
}

/// Check whether the firmware version meets the minimum requirement
pub fn validate_firmware(major: u8, minor: u8) -> bool {
    (major, minor) >= (REQUIRED_FW_MAJ, REQUIRED_FW_MIN)
}

/// Validate radio configuration parameters
///
/// Returns `Ok(())` if all parameters are within valid ranges,
/// or `Err(ConfigError)` for the first invalid parameter.
pub fn validate_config(freq: u32, bw: u32, txp: u8, sf: u8, cr: u8) -> Result<(), ConfigError> {
    if !(FREQ_MIN..=FREQ_MAX).contains(&freq) {
        return Err(ConfigError::FrequencyOutOfRange);
    }
    // Valid LoRa bandwidths (Hz)
    match bw {
        7800 | 10400 | 15600 | 20800 | 31250 | 41700 | 62500 | 125000 | 250000 | 500000 => {}
        _ => return Err(ConfigError::InvalidBandwidth),
    }
    if txp > MAX_TX_POWER {
        return Err(ConfigError::TxPowerOutOfRange);
    }
    if !(5..=12).contains(&sf) {
        return Err(ConfigError::SpreadingFactorOutOfRange);
    }
    if !(5..=8).contains(&cr) {
        return Err(ConfigError::CodingRateOutOfRange);
    }
    Ok(())
}

/// Compute the on-air bitrate for a LoRa configuration
///
/// Formula: `sf * 4 * bandwidth / (cr * 2^sf)` (integer arithmetic,
/// algebraically equivalent to Python's `sf * (4.0/cr) / (2^sf / (bw/1000)) * 1000`).
/// Returns 0 for invalid inputs (sf=0, cr=0, bandwidth=0, sf>63).
pub fn compute_bitrate(sf: u8, cr: u8, bandwidth: u32) -> u32 {
    if cr == 0 || bandwidth == 0 || sf == 0 || sf > 63 {
        return 0;
    }
    // Use u64 to avoid overflow for large bandwidth * sf * 4
    let numerator = sf as u64 * 4 * bandwidth as u64;
    let denominator = cr as u64 * (1u64 << sf);
    (numerator / denominator) as u32
}

/// Maximum random jitter (ms) for the first packet after idle.
/// Matches Python's PATHFINDER_RW = 0.5s.
pub const JITTER_MAX_MS: u64 = 500;

/// Minimum spacing (ms) between consecutive serial writes.
/// Prevents overrunning the serial buffer (508 bytes at 115200 baud ≈ 44ms).
/// For CSMA-fair pacing, use `compute_spacing_ms()` instead, this constant
/// is only the serial-level floor.
pub const MIN_SPACING_MS: u64 = 50;

/// Firmware CSMA DIFS time (ms). The firmware waits this long with the channel
/// clear before starting the contention window. From RNode_Firmware Config.h.
pub const CSMA_DIFS_MS: u64 = 48;

/// Firmware CSMA maximum contention window (ms). 15 slots × 24ms = 360ms.
/// From RNode_Firmware Config.h: csma_slot_ms=24, cw_max=15.
pub const CSMA_MAX_CW_MS: u64 = 360;

/// Safety margin (ms) added to airtime-based pacing to absorb firmware
/// processing jitter and serial I/O overhead.
pub const PACING_MARGIN_MS: u64 = 100;

/// Compute LoRa airtime in milliseconds for a given payload.
///
/// Uses the SX127x formula from Semtech AN1200.13. Parameters:
/// - `payload_bytes`: total bytes on the wire (header + data)
/// - `bandwidth_hz`: signal bandwidth in Hz (e.g., 62500, 125000, 250000)
/// - `sf`: spreading factor (6-12)
/// - `cr`: coding rate denominator (5-8, meaning 4/5 through 4/8)
///
/// Returns airtime in milliseconds, rounded up.
pub fn airtime_ms(payload_bytes: u32, bandwidth_hz: u32, sf: u8, cr: u8) -> u64 {
    // Symbol time: T_sym = 2^SF / BW (in seconds)
    // We compute in microseconds to avoid floating point.
    // T_sym_us = 2^SF * 1_000_000 / BW
    let sf = sf as u64;
    let bw = bandwidth_hz as u64;
    let t_sym_us = (1u64 << sf) * 1_000_000 / bw;

    // Preamble: 8 symbols + 4.25 symbols = 12.25 symbols
    // In microseconds: 12.25 * T_sym = (49 * T_sym) / 4
    let t_preamble_us = 49 * t_sym_us / 4;

    // Payload symbol count (explicit header mode, CRC on):
    //   n_payload = 8 + max(ceil((8*PL - 4*SF + 28 + 16) / (4*(SF-2*DE))) * (CR-4+4), 0)
    // where DE=0 for SF<11, DE=1 for SF>=11
    // CR parameter is denominator (5-8), so (CR-4) gives 1-4
    let de: u64 = if sf >= 11 { 1 } else { 0 };
    let pl = payload_bytes as i64;
    let sf_i = sf as i64;
    let cr_factor = (cr as i64 - 4).max(1) as u64; // 1-4

    let numerator = 8 * pl - 4 * sf_i + 28 + 16;
    let denominator = 4 * (sf_i - 2 * de as i64);

    let n_extra = if numerator > 0 && denominator > 0 {
        let ceil_div = (numerator + denominator - 1) / denominator;
        (ceil_div as u64) * (cr_factor + 4)
    } else {
        0
    };
    let n_payload = 8 + n_extra;

    let t_payload_us = n_payload * t_sym_us;

    // Total airtime
    let total_us = t_preamble_us + t_payload_us;

    // Convert to ms, round up
    total_us.div_ceil(1000)
}

/// Compute CSMA-fair inter-frame spacing in milliseconds.
///
/// Ensures the firmware's TX queue has at most one frame, so `flush_queue()`
/// (which sends all queued frames back-to-back without CSMA) only sends one
/// frame per CSMA contest.
///
/// spacing = airtime(frame) + DIFS + max_CW + margin
pub fn compute_spacing_ms(payload_bytes: u32, bandwidth_hz: u32, sf: u8, cr: u8) -> u64 {
    let air = airtime_ms(payload_bytes, bandwidth_hz, sf, cr);
    let spacing = air + CSMA_DIFS_MS + CSMA_MAX_CW_MS + PACING_MARGIN_MS;
    // Never go below the serial-level floor
    spacing.max(MIN_SPACING_MS)
}

// ---------------------------------------------------------------------------
// Radio config wire protocol (for runtime config override via serial)
// ---------------------------------------------------------------------------

/// Magic prefix for radio config frames (distinguishes from Reticulum packets).
pub const RADIO_CONFIG_MAGIC: [u8; 2] = [0xA4, 0xA4];

/// Total config frame payload length (2 magic + 14 parameter bytes).
pub const RADIO_CONFIG_FRAME_LEN: usize = 17;

/// ACK payload sent by T114 after applying radio config.
pub const RADIO_CONFIG_ACK: [u8; 3] = [0xA4, 0xA4, 0x01];

/// Parsed radio config from the wire format.
/// All values are in human-readable units (Hz, denominator).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RadioConfigWire {
    pub frequency_hz: u32,
    pub bandwidth_hz: u32,
    pub sf: u8,
    pub cr: u8, // coding rate denominator (5-8)
    pub tx_power_dbm: i8,
    pub preamble_len: u16,
    pub csma_enabled: bool,
    /// When true, the T114 drops every outgoing LoRa packet at the driver
    /// level, the radio stays listening but never transmits. Used by the
    /// integration-test runner to neutralize T114s the scenario does not
    /// bind, so they cannot pollute the benchmark channel with their own
    /// Reticulum announces.
    pub radio_silent: bool,
}

/// Parse a radio config from wire bytes (13, 14, or 15 bytes, after magic stripped).
///
/// Wire layout: freq_hz(4 BE) + bw_hz(4 BE) + sf(1) + cr(1) + tx_power(1) + preamble(2 BE)
/// + csma_enabled(1, optional, defaults to false if absent for backward compat)
/// + radio_silent(1, optional, defaults to false if absent for backward compat)
pub fn parse_radio_config(data: &[u8]) -> Option<RadioConfigWire> {
    if !(13..=15).contains(&data.len()) {
        return None;
    }
    let frequency_hz = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    let bandwidth_hz = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    let sf = data[8];
    let cr = data[9];
    let tx_power_dbm = data[10] as i8;
    let preamble_len = u16::from_be_bytes([data[11], data[12]]);
    let csma_enabled = data.len() >= 14 && data[13] != 0;
    let radio_silent = data.len() >= 15 && data[14] != 0;

    if !(5..=12).contains(&sf) {
        return None;
    }
    if !(5..=8).contains(&cr) {
        return None;
    }

    Some(RadioConfigWire {
        frequency_hz,
        bandwidth_hz,
        sf,
        cr,
        tx_power_dbm,
        preamble_len,
        csma_enabled,
        radio_silent,
    })
}

/// Build a radio config frame payload (17 bytes including magic prefix).
pub fn build_radio_config_frame(cfg: &RadioConfigWire) -> Vec<u8> {
    let mut out = Vec::with_capacity(RADIO_CONFIG_FRAME_LEN);
    out.extend_from_slice(&RADIO_CONFIG_MAGIC);
    out.extend_from_slice(&cfg.frequency_hz.to_be_bytes());
    out.extend_from_slice(&cfg.bandwidth_hz.to_be_bytes());
    out.push(cfg.sf);
    out.push(cfg.cr);
    out.push(cfg.tx_power_dbm as u8);
    out.extend_from_slice(&cfg.preamble_len.to_be_bytes());
    out.push(cfg.csma_enabled as u8);
    out.push(cfg.radio_silent as u8);
    out
}

// ---------------------------------------------------------------------------
// LoRa frame header: split protocol
// ---------------------------------------------------------------------------

/// Split flag in the 1-byte LoRa frame header. When set, the frame is
/// part of a split packet (payload > 254 bytes sent as two frames).
pub const FLAG_SPLIT: u8 = 0x01;

/// Maximum payload bytes in a single LoRa frame (255-byte FIFO minus 1-byte header).
pub const MAX_SINGLE_PAYLOAD: usize = 254;

/// Build one or two LoRa frames from a Reticulum packet.
///
/// `seq_nibble` must already be masked to upper 4 bits (0xF0).
/// Returns 1 frame for payload <= 254 bytes, 2 frames for larger.
/// Both split frames use the identical header byte (same sequence nibble,
/// same FLAG_SPLIT bit), matching the RNode firmware exactly.
pub fn build_lora_frames(data: &[u8], seq_nibble: u8) -> Vec<Vec<u8>> {
    let seq = seq_nibble & 0xF0;
    if data.len() > MAX_SINGLE_PAYLOAD {
        let header = seq | FLAG_SPLIT;
        let mut frame1 = Vec::with_capacity(1 + MAX_SINGLE_PAYLOAD);
        frame1.push(header);
        frame1.extend_from_slice(&data[..MAX_SINGLE_PAYLOAD]);

        let mut frame2 = Vec::with_capacity(1 + data.len() - MAX_SINGLE_PAYLOAD);
        frame2.push(header);
        frame2.extend_from_slice(&data[MAX_SINGLE_PAYLOAD..]);

        alloc::vec![frame1, frame2]
    } else {
        let mut frame = Vec::with_capacity(1 + data.len());
        frame.push(seq);
        frame.extend_from_slice(data);
        alloc::vec![frame]
    }
}

/// State machine for reassembling split LoRa frames.
///
/// Implements the four-case logic from the RNode firmware:
/// 1. Split + no buffer → store first half, return None
/// 2. Split + same sequence → concatenate, return assembled payload
/// 3. Split + different sequence → discard old, store new first half
/// 4. Not split → return payload directly (discard any pending buffer)
pub struct SplitReassembler {
    buf: Vec<u8>,
    seq: Option<u8>,
    tick: u32,
}

impl Default for SplitReassembler {
    fn default() -> Self {
        Self::new()
    }
}

impl SplitReassembler {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            seq: None,
            tick: 0,
        }
    }

    /// Feed a raw LoRa frame (including the 1-byte header).
    /// Returns `Some(payload)` when a complete packet is ready.
    /// Returns `None` when buffering a split first-half or on errors.
    pub fn feed(&mut self, frame: &[u8], current_tick: u32) -> Option<Vec<u8>> {
        if frame.len() < 2 {
            return None;
        }
        let header = frame[0];
        let is_split = (header & FLAG_SPLIT) != 0;
        let sequence = header >> 4;
        let payload = &frame[1..];

        if is_split && self.seq.is_none() {
            // First part of a split packet
            self.buf.clear();
            self.buf.extend_from_slice(payload);
            self.seq = Some(sequence);
            self.tick = current_tick;
            None
        } else if is_split && self.seq == Some(sequence) {
            // Second part: concatenate and deliver
            self.buf.extend_from_slice(payload);
            self.seq = None;
            Some(core::mem::take(&mut self.buf))
        } else if is_split {
            // Different sequence: discard old, start new
            self.buf.clear();
            self.buf.extend_from_slice(payload);
            self.seq = Some(sequence);
            self.tick = current_tick;
            None
        } else {
            // Not a split packet: deliver directly
            if self.seq.is_some() {
                self.buf.clear();
                self.seq = None;
            }
            Some(payload.to_vec())
        }
    }

    /// Discard stale split buffers older than `max_age` ticks.
    pub fn check_timeout(&mut self, current_tick: u32, max_age: u32) {
        if self.seq.is_some() && current_tick.wrapping_sub(self.tick) >= max_age {
            self.buf.clear();
            self.seq = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framing::kiss::{KissDeframeResult, KissDeframer};
    use alloc::vec;

    /// Deframe a single KISS frame and assert it matches the expected command and payload.
    fn assert_single_frame(frame: &[u8], expected_cmd: u8, expected_payload: &[u8]) {
        let mut deframer = KissDeframer::with_max_payload(HW_MTU);
        let results = deframer.process(frame);
        assert_eq!(results.len(), 1, "expected exactly 1 frame");
        match &results[0] {
            KissDeframeResult::Frame { command, payload } => {
                assert_eq!(*command, expected_cmd);
                assert_eq!(payload.as_slice(), expected_payload);
            }
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    // --- Encoding tests ---

    #[test]
    fn test_build_detect_query() {
        let query = build_detect_query();
        assert_eq!(
            query,
            vec![0xC0, 0x08, 0x73, 0xC0, 0x50, 0x00, 0xC0, 0x48, 0x00, 0xC0, 0x49, 0x00, 0xC0]
        );
        assert_eq!(query.len(), 13);
    }

    #[test]
    fn test_build_set_frequency() {
        // 868.0 MHz = 868_000_000 Hz = 0x33BCA100
        assert_single_frame(
            &build_set_frequency(868_000_000),
            CMD_FREQUENCY,
            &[0x33, 0xBC, 0xA1, 0x00],
        );
    }

    #[test]
    fn test_build_set_bandwidth() {
        // 125 kHz = 125_000 Hz = 0x0001E848
        assert_single_frame(
            &build_set_bandwidth(125_000),
            CMD_BANDWIDTH,
            &[0x00, 0x01, 0xE8, 0x48],
        );
    }

    #[test]
    fn test_build_set_txpower() {
        assert_single_frame(&build_set_txpower(17), CMD_TXPOWER, &[17]);
    }

    #[test]
    fn test_build_set_sf() {
        assert_single_frame(&build_set_sf(7), CMD_SF, &[7]);
    }

    #[test]
    fn test_build_set_cr() {
        assert_single_frame(&build_set_cr(5), CMD_CR, &[5]);
    }

    #[test]
    fn test_build_set_radio_state_on() {
        assert_single_frame(
            &build_set_radio_state(RADIO_STATE_ON),
            CMD_RADIO_STATE,
            &[0x01],
        );
    }

    #[test]
    fn test_build_set_st_alock() {
        // 50.0% -> 5000
        assert_single_frame(&build_set_st_alock(5000), CMD_ST_ALOCK, &[0x13, 0x88]);
    }

    #[test]
    fn test_build_set_lt_alock() {
        assert_single_frame(&build_set_lt_alock(1000), CMD_LT_ALOCK, &[0x03, 0xE8]);
    }

    #[test]
    fn test_build_data_frame() {
        assert_single_frame(&build_data_frame(b"Hello RNode"), CMD_DATA, b"Hello RNode");
    }

    #[test]
    fn test_build_leave() {
        assert_single_frame(&build_leave(), CMD_LEAVE, &[LEAVE_PAYLOAD]);
    }

    // --- Decoding tests ---

    #[test]
    fn test_decode_rssi() {
        // raw 157 -> 157 - 157 = 0 dBm
        assert_eq!(decode_rssi(&[157]), Some(0));
        // raw 0 -> 0 - 157 = -157 dBm
        assert_eq!(decode_rssi(&[0]), Some(-157));
        // raw 200 -> 200 - 157 = 43 dBm
        assert_eq!(decode_rssi(&[200]), Some(43));
        // empty
        assert_eq!(decode_rssi(&[]), None);
    }

    #[test]
    fn test_decode_snr() {
        // raw 0x80 = 128 unsigned, but as i8 = -128
        assert_eq!(decode_snr(&[0x80]), Some(-128_i8));
        // raw 0x00 = 0
        assert_eq!(decode_snr(&[0x00]), Some(0_i8));
        // raw 0x7F = 127
        assert_eq!(decode_snr(&[0x7F]), Some(127_i8));
        // empty
        assert_eq!(decode_snr(&[]), None);
    }

    #[test]
    fn test_decode_battery() {
        // Charging, 85%
        assert_eq!(
            decode_battery(&[0x02, 85]),
            Some((BatteryState::Charging, 85))
        );
        // Discharging, 50%
        assert_eq!(
            decode_battery(&[0x01, 50]),
            Some((BatteryState::Discharging, 50))
        );
        // Charged, 100%
        assert_eq!(
            decode_battery(&[0x03, 100]),
            Some((BatteryState::Charged, 100))
        );
        // Unknown state byte
        assert_eq!(decode_battery(&[0x00, 0]), Some((BatteryState::Unknown, 0)));
        // Too short
        assert_eq!(decode_battery(&[0x02]), None);
        assert_eq!(decode_battery(&[]), None);
    }

    #[test]
    fn test_decode_temperature() {
        // raw 150 -> 150 - 120 = 30 Celsius
        assert_eq!(decode_temperature(&[150]), Some(30));
        // raw 120 -> 120 - 120 = 0 Celsius
        assert_eq!(decode_temperature(&[120]), Some(0));
        // raw 90 -> 90 - 120 = -30 Celsius
        assert_eq!(decode_temperature(&[90]), Some(-30));
        // empty
        assert_eq!(decode_temperature(&[]), None);
    }

    #[test]
    fn test_decode_firmware_version() {
        assert_eq!(decode_firmware_version(&[1, 85]), Some((1, 85)));
        assert_eq!(decode_firmware_version(&[2, 0]), Some((2, 0)));
        // Too short
        assert_eq!(decode_firmware_version(&[1]), None);
        assert_eq!(decode_firmware_version(&[]), None);
    }

    #[test]
    fn test_decode_channel_stats_single() {
        // 11-byte single-interface payload
        let payload = [
            0x00, 0x64, // airtime_short = 100
            0x01, 0xF4, // airtime_long = 500
            0x00, 0xC8, // channel_load_short = 200
            0x02, 0x58, // channel_load_long = 600
            200,  // current_rssi raw -> 200 - 157 = 43
            100,  // noise_floor raw -> 100 - 157 = -57
            50,   // interference raw -> 50 - 157 = -107
        ];
        let stats = decode_channel_stats(&payload).unwrap();
        assert_eq!(stats.airtime_short, 100);
        assert_eq!(stats.airtime_long, 500);
        assert_eq!(stats.channel_load_short, 200);
        assert_eq!(stats.channel_load_long, 600);
        assert_eq!(stats.current_rssi, Some(43));
        assert_eq!(stats.noise_floor, Some(-57));
        assert_eq!(stats.interference, Some(-107));
    }

    #[test]
    fn test_decode_channel_stats_multi() {
        // 8-byte multi-interface payload (no RSSI fields)
        let payload = [
            0x00, 0x64, // airtime_short = 100
            0x01, 0xF4, // airtime_long = 500
            0x00, 0xC8, // channel_load_short = 200
            0x02, 0x58, // channel_load_long = 600
        ];
        let stats = decode_channel_stats(&payload).unwrap();
        assert_eq!(stats.airtime_short, 100);
        assert_eq!(stats.airtime_long, 500);
        assert_eq!(stats.channel_load_short, 200);
        assert_eq!(stats.channel_load_long, 600);
        assert_eq!(stats.current_rssi, None);
        assert_eq!(stats.noise_floor, None);
        assert_eq!(stats.interference, None);
    }

    #[test]
    fn test_decode_channel_stats_interference_ff() {
        // 11-byte payload with 0xFF interference -> None
        let payload = [
            0x00, 0x64, // airtime_short = 100
            0x01, 0xF4, // airtime_long = 500
            0x00, 0xC8, // channel_load_short = 200
            0x02, 0x58, // channel_load_long = 600
            200,  // current_rssi
            100,  // noise_floor
            0xFF, // interference = 0xFF -> None
        ];
        let stats = decode_channel_stats(&payload).unwrap();
        assert_eq!(stats.current_rssi, Some(43));
        assert_eq!(stats.noise_floor, Some(-57));
        assert_eq!(stats.interference, None);
    }

    #[test]
    fn test_decode_phy_params_single() {
        // 12-byte single-interface payload
        let payload = [
            0x00, 0x0A, // symbol_time_raw = 10
            0x03, 0xE8, // symbol_rate = 1000
            0x00, 0x08, // preamble_symbols = 8
            0x00, 0x50, // preamble_time_ms = 80
            0x00, 0x0F, // csma_slot_time_ms = 15
            0x00, 0x1E, // difs_time_ms = 30
        ];
        let params = decode_phy_params(&payload).unwrap();
        assert_eq!(params.symbol_time_raw, 10);
        assert_eq!(params.symbol_rate, 1000);
        assert_eq!(params.preamble_symbols, 8);
        assert_eq!(params.preamble_time_ms, 80);
        assert_eq!(params.csma_slot_time_ms, 15);
        assert_eq!(params.difs_time_ms, Some(30));
    }

    #[test]
    fn test_decode_phy_params_multi() {
        // 10-byte multi-interface payload (no difs_time)
        let payload = [
            0x00, 0x0A, // symbol_time_raw = 10
            0x03, 0xE8, // symbol_rate = 1000
            0x00, 0x08, // preamble_symbols = 8
            0x00, 0x50, // preamble_time_ms = 80
            0x00, 0x0F, // csma_slot_time_ms = 15
        ];
        let params = decode_phy_params(&payload).unwrap();
        assert_eq!(params.symbol_time_raw, 10);
        assert_eq!(params.symbol_rate, 1000);
        assert_eq!(params.preamble_symbols, 8);
        assert_eq!(params.preamble_time_ms, 80);
        assert_eq!(params.csma_slot_time_ms, 15);
        assert_eq!(params.difs_time_ms, None);
    }

    #[test]
    fn test_decode_too_short() {
        // All decode functions return None for empty/short payloads
        assert_eq!(decode_rssi(&[]), None);
        assert_eq!(decode_snr(&[]), None);
        assert_eq!(decode_battery(&[]), None);
        assert_eq!(decode_battery(&[0x01]), None);
        assert_eq!(decode_temperature(&[]), None);
        assert_eq!(decode_firmware_version(&[]), None);
        assert_eq!(decode_firmware_version(&[1]), None);
        assert_eq!(decode_channel_stats(&[]), None);
        assert_eq!(decode_channel_stats(&[0; 7]), None);
        assert_eq!(decode_phy_params(&[]), None);
        assert_eq!(decode_phy_params(&[0; 9]), None);
    }

    // --- Round-trip tests ---

    #[test]
    fn test_roundtrip_frequency() {
        let frame = build_set_frequency(868_000_000);
        let mut deframer = KissDeframer::with_max_payload(HW_MTU);
        let results = deframer.process(&frame);
        assert_eq!(results.len(), 1);
        match &results[0] {
            KissDeframeResult::Frame { command, payload } => {
                assert_eq!(*command, CMD_FREQUENCY);
                let hz = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
                assert_eq!(hz, 868_000_000);
            }
            _ => panic!("Expected frame"),
        }
    }

    #[test]
    fn test_roundtrip_detect_query() {
        let query = build_detect_query();
        let mut deframer = KissDeframer::with_max_payload(HW_MTU);
        let results = deframer.process(&query);

        assert_eq!(results.len(), 4);

        // Frame 1: CMD_DETECT with DETECT_REQ
        match &results[0] {
            KissDeframeResult::Frame { command, payload } => {
                assert_eq!(*command, CMD_DETECT);
                assert_eq!(payload.as_slice(), &[DETECT_REQ]);
            }
            _ => panic!("Expected detect frame"),
        }

        // Frame 2: CMD_FW_VERSION query
        match &results[1] {
            KissDeframeResult::Frame { command, payload } => {
                assert_eq!(*command, CMD_FW_VERSION);
                assert_eq!(payload.as_slice(), &[0x00]);
            }
            _ => panic!("Expected fw_version frame"),
        }

        // Frame 3: CMD_PLATFORM query
        match &results[2] {
            KissDeframeResult::Frame { command, payload } => {
                assert_eq!(*command, CMD_PLATFORM);
                assert_eq!(payload.as_slice(), &[0x00]);
            }
            _ => panic!("Expected platform frame"),
        }

        // Frame 4: CMD_MCU query
        match &results[3] {
            KissDeframeResult::Frame { command, payload } => {
                assert_eq!(*command, CMD_MCU);
                assert_eq!(payload.as_slice(), &[0x00]);
            }
            _ => panic!("Expected mcu frame"),
        }
    }

    #[test]
    fn test_roundtrip_data_with_special_bytes() {
        // Data containing FEND and FESC bytes, must survive escaping
        let data = [0xC0, 0xDB, 0x42, 0xC0, 0xDB];
        let frame = build_data_frame(&data);
        let mut deframer = KissDeframer::with_max_payload(HW_MTU);
        let results = deframer.process(&frame);
        assert_eq!(results.len(), 1);
        match &results[0] {
            KissDeframeResult::Frame { command, payload } => {
                assert_eq!(*command, CMD_DATA);
                assert_eq!(payload.as_slice(), &data);
            }
            _ => panic!("Expected frame"),
        }
    }

    #[test]
    fn test_cmd_int_data_values() {
        assert_eq!(CMD_INT_DATA[0], 0x00);
        assert_eq!(CMD_INT_DATA[1], 0x10);
        assert_eq!(CMD_INT_DATA[2], 0x20);
        assert_eq!(CMD_INT_DATA[3], 0x70);
        assert_eq!(CMD_INT_DATA[4], 0x75);
        assert_eq!(CMD_INT_DATA[5], 0x90);
        assert_eq!(CMD_INT_DATA[6], 0xA0);
        assert_eq!(CMD_INT_DATA[7], 0xB0);
        assert_eq!(CMD_INT_DATA[8], 0xC0);
        assert_eq!(CMD_INT_DATA[9], 0xD0);
        assert_eq!(CMD_INT_DATA[10], 0xE0);
        assert_eq!(CMD_INT_DATA[11], 0xF0);
    }

    // --- Validation tests ---

    #[test]
    fn test_validate_firmware_ok() {
        assert!(validate_firmware(1, 52));
        assert!(validate_firmware(1, 85));
        assert!(validate_firmware(2, 0));
    }

    #[test]
    fn test_validate_firmware_too_old() {
        assert!(!validate_firmware(1, 51));
        assert!(!validate_firmware(0, 99));
        assert!(!validate_firmware(1, 0));
    }

    #[test]
    fn test_validate_config_ok() {
        assert!(validate_config(868_000_000, 125_000, 17, 7, 5).is_ok());
        assert!(validate_config(915_000_000, 500_000, 22, 12, 8).is_ok());
        assert!(validate_config(2_400_000_000, 250_000, 0, 5, 5).is_ok());
    }

    #[test]
    fn test_validate_config_bad_frequency() {
        assert_eq!(
            validate_config(100_000_000, 125_000, 17, 7, 5),
            Err(ConfigError::FrequencyOutOfRange)
        );
        // u32 max is ~4.29 GHz, over the 3 GHz limit
        assert_eq!(
            validate_config(3_500_000_000, 125_000, 17, 7, 5),
            Err(ConfigError::FrequencyOutOfRange)
        );
    }

    #[test]
    fn test_validate_config_bad_bandwidth() {
        assert_eq!(
            validate_config(868_000_000, 100_000, 17, 7, 5),
            Err(ConfigError::InvalidBandwidth)
        );
    }

    #[test]
    fn test_validate_config_bad_txpower() {
        assert_eq!(
            validate_config(868_000_000, 125_000, 38, 7, 5),
            Err(ConfigError::TxPowerOutOfRange)
        );
    }

    #[test]
    fn test_validate_config_bad_sf() {
        assert_eq!(
            validate_config(868_000_000, 125_000, 17, 4, 5),
            Err(ConfigError::SpreadingFactorOutOfRange)
        );
        assert_eq!(
            validate_config(868_000_000, 125_000, 17, 13, 5),
            Err(ConfigError::SpreadingFactorOutOfRange)
        );
    }

    #[test]
    fn test_validate_config_bad_cr() {
        assert_eq!(
            validate_config(868_000_000, 125_000, 17, 7, 4),
            Err(ConfigError::CodingRateOutOfRange)
        );
        assert_eq!(
            validate_config(868_000_000, 125_000, 17, 7, 9),
            Err(ConfigError::CodingRateOutOfRange)
        );
    }

    #[test]
    fn test_compute_bitrate() {
        // SF7, CR5, BW125kHz → Python produces 5468
        // Formula: 7 * (4.0/5) / (128 / 125) * 1000 = 7 * 0.8 / 1.024 * 1000 = 5468.75 → 5468
        let br = compute_bitrate(7, 5, 125_000);
        assert_eq!(br, 5468);
    }

    #[test]
    fn test_compute_bitrate_sf12() {
        // SF12, CR8, BW125kHz
        // 12 * (4.0/8) / (4096 / 125) * 1000 = 12 * 0.5 / 32.768 * 1000 = 183.105... → 183
        let br = compute_bitrate(12, 8, 125_000);
        assert_eq!(br, 183);
    }

    #[test]
    fn test_compute_bitrate_62_5khz() {
        // SF7, CR5, BW62.5kHz, the slow bandwidth used by LoRa integration tests
        // 7 * (4.0/5) / (128 / 62.5) * 1000 = 7 * 0.8 / 2.048 * 1000 = 2734.375 → 2734
        let br = compute_bitrate(7, 5, 62_500);
        assert_eq!(br, 2734);
    }

    #[test]
    fn test_compute_bitrate_500khz() {
        // SF7, CR5, BW500kHz, fast LoRa configuration
        // 7 * (4.0/5) / (128 / 500) * 1000 = 7 * 0.8 / 0.256 * 1000 = 21875
        let br = compute_bitrate(7, 5, 500_000);
        assert_eq!(br, 21875);
    }

    // Airtime tests
    #[test]
    fn test_airtime_491b_bw62500_sf7_cr5() {
        // 491-byte resource data segment at 62.5kHz SF7 CR5
        // Semtech calculator: ~1440ms
        let ms = airtime_ms(491, 62_500, 7, 5);
        assert!(
            (1400..=1500).contains(&ms),
            "airtime={ms}ms, expected ~1440ms"
        );
    }

    #[test]
    fn test_airtime_491b_bw125000_sf7_cr5() {
        // Same payload at 125kHz, should be ~half
        let ms = airtime_ms(491, 125_000, 7, 5);
        assert!((700..=800).contains(&ms), "airtime={ms}ms, expected ~750ms");
    }

    #[test]
    fn test_airtime_491b_bw250000_sf7_cr5() {
        // Same payload at 250kHz, should be ~quarter
        let ms = airtime_ms(491, 250_000, 7, 5);
        assert!((350..=420).contains(&ms), "airtime={ms}ms, expected ~380ms");
    }

    #[test]
    fn test_airtime_small_packet() {
        // 20-byte keepalive at 62.5kHz SF7 CR5
        let ms = airtime_ms(20, 62_500, 7, 5);
        assert!(
            ms > 0 && ms < 500,
            "airtime={ms}ms, expected <500ms for small packet"
        );
    }

    #[test]
    fn test_airtime_sf12_long_range() {
        // 100 bytes at SF12 125kHz CR8, very slow long range
        let ms = airtime_ms(100, 125_000, 12, 8);
        assert!(ms >= 2000, "airtime={ms}ms, expected >2000ms for SF12");
    }

    #[test]
    fn test_compute_spacing_includes_csma_overhead() {
        let air = airtime_ms(491, 62_500, 7, 5);
        let spacing = compute_spacing_ms(491, 62_500, 7, 5);
        assert_eq!(
            spacing,
            air + CSMA_DIFS_MS + CSMA_MAX_CW_MS + PACING_MARGIN_MS,
            "spacing must be airtime + DIFS + max CW + margin"
        );
    }

    #[test]
    fn test_compute_spacing_floor() {
        // Tiny packet with huge bandwidth, airtime < MIN_SPACING_MS
        let spacing = compute_spacing_ms(1, 500_000, 7, 5);
        assert!(
            spacing >= MIN_SPACING_MS,
            "spacing must never go below MIN_SPACING_MS"
        );
    }

    // -----------------------------------------------------------------------
    // Split protocol tests
    // -----------------------------------------------------------------------

    // TX tests

    #[test]
    fn single_frame_small() {
        let data = vec![0xAA; 100];
        let frames = build_lora_frames(&data, 0x50);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0][0] & FLAG_SPLIT, 0);
        assert_eq!(&frames[0][1..], &data[..]);
    }

    #[test]
    fn single_frame_exact_254() {
        let data = vec![0xBB; 254];
        let frames = build_lora_frames(&data, 0x30);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0][0] & FLAG_SPLIT, 0);
        assert_eq!(frames[0].len(), 255);
    }

    #[test]
    fn split_at_255() {
        let data = vec![0xCC; 255];
        let frames = build_lora_frames(&data, 0x70);
        assert_eq!(frames.len(), 2);
        assert_ne!(frames[0][0] & FLAG_SPLIT, 0);
        assert_ne!(frames[1][0] & FLAG_SPLIT, 0);
        assert_eq!(frames[0].len(), 255); // 1 header + 254 payload
        assert_eq!(frames[1].len(), 2); // 1 header + 1 payload
    }

    #[test]
    fn split_300_bytes() {
        let data: Vec<u8> = (0u16..300).map(|i| (i & 0xFF) as u8).collect();
        let frames = build_lora_frames(&data, 0xA0);
        assert_eq!(frames.len(), 2);
        assert_eq!(&frames[0][1..], &data[..254]);
        assert_eq!(&frames[1][1..], &data[254..]);
    }

    #[test]
    fn split_max_508() {
        let data = vec![0xDD; 508];
        let frames = build_lora_frames(&data, 0xE0);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].len(), 255);
        assert_eq!(frames[1].len(), 255);
    }

    #[test]
    fn header_sequence_nibble() {
        let frames = build_lora_frames(&[1, 2, 3], 0xB0);
        assert_eq!(frames[0][0] >> 4, 0x0B);
    }

    #[test]
    fn both_frames_same_header() {
        let data = vec![0xFF; 300];
        let frames = build_lora_frames(&data, 0x40);
        assert_eq!(frames[0][0], frames[1][0]);
    }

    #[test]
    fn empty_payload() {
        let frames = build_lora_frames(&[], 0x20);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len(), 1); // header only
        assert_eq!(frames[0][0] & FLAG_SPLIT, 0);
    }

    // RX tests

    #[test]
    fn single_frame_delivery() {
        let mut r = SplitReassembler::new();
        let frame = vec![0x50, 0xAA, 0xBB, 0xCC];
        let result = r.feed(&frame, 0);
        assert_eq!(result, Some(vec![0xAA, 0xBB, 0xCC]));
    }

    #[test]
    fn split_reassembly() {
        let mut r = SplitReassembler::new();
        let header: u8 = 0x30 | FLAG_SPLIT;
        let frame1 = {
            let mut f = vec![header];
            f.extend_from_slice(&[1; 254]);
            f
        };
        let frame2 = {
            let mut f = vec![header];
            f.extend_from_slice(&[2; 46]);
            f
        };
        assert_eq!(r.feed(&frame1, 0), None);
        let result = r.feed(&frame2, 1).unwrap();
        assert_eq!(result.len(), 300);
        assert!(result[..254].iter().all(|&b| b == 1));
        assert!(result[254..].iter().all(|&b| b == 2));
    }

    #[test]
    fn split_different_sequence_restarts() {
        let mut r = SplitReassembler::new();
        let frame_a = vec![0x30 | FLAG_SPLIT, 0xAA];
        let frame_b = vec![0x50 | FLAG_SPLIT, 0xBB];
        assert_eq!(r.feed(&frame_a, 0), None);
        assert_eq!(r.feed(&frame_b, 1), None);
        // Buffer should now hold frame_b's payload
        let frame_b2 = vec![0x50 | FLAG_SPLIT, 0xCC];
        let result = r.feed(&frame_b2, 2).unwrap();
        assert_eq!(result, vec![0xBB, 0xCC]);
    }

    #[test]
    fn non_split_clears_pending() {
        let mut r = SplitReassembler::new();
        let split_frame = vec![0x30 | FLAG_SPLIT, 0xAA];
        assert_eq!(r.feed(&split_frame, 0), None);
        let single_frame = vec![0x70, 0xDD, 0xEE];
        let result = r.feed(&single_frame, 1);
        assert_eq!(result, Some(vec![0xDD, 0xEE]));
        // Buffer should be cleared, a second split half should not match
        let split_half2 = vec![0x30 | FLAG_SPLIT, 0xBB];
        assert_eq!(r.feed(&split_half2, 2), None); // new first half
    }

    #[test]
    fn timeout_clears_buffer() {
        let mut r = SplitReassembler::new();
        let frame = vec![0x30 | FLAG_SPLIT, 0xAA];
        assert_eq!(r.feed(&frame, 100), None);
        r.check_timeout(109, 10); // not expired yet
                                  // Buffer should still be there
        let frame2 = vec![0x30 | FLAG_SPLIT, 0xBB];
        let result = r.feed(&frame2, 109).unwrap();
        assert_eq!(result, vec![0xAA, 0xBB]);
    }

    #[test]
    fn timeout_expires_buffer() {
        let mut r = SplitReassembler::new();
        let frame = vec![0x30 | FLAG_SPLIT, 0xAA];
        assert_eq!(r.feed(&frame, 100), None);
        r.check_timeout(110, 10); // expired
                                  // Buffer cleared, second half should start a new buffer
        let frame2 = vec![0x30 | FLAG_SPLIT, 0xBB];
        assert_eq!(r.feed(&frame2, 111), None); // new first half, not reassembly
    }

    #[test]
    fn frame_too_short() {
        let mut r = SplitReassembler::new();
        assert_eq!(r.feed(&[], 0), None);
        assert_eq!(r.feed(&[0x50], 0), None);
    }

    #[test]
    fn round_trip_split() {
        let data: Vec<u8> = (0u16..300).map(|i| (i & 0xFF) as u8).collect();
        let frames = build_lora_frames(&data, 0x60);
        let mut r = SplitReassembler::new();
        assert_eq!(r.feed(&frames[0], 0), None);
        let result = r.feed(&frames[1], 1).unwrap();
        assert_eq!(result, data);
    }

    #[test]
    fn round_trip_single() {
        let data = vec![0x42; 100];
        let frames = build_lora_frames(&data, 0x80);
        let mut r = SplitReassembler::new();
        let result = r.feed(&frames[0], 0).unwrap();
        assert_eq!(result, data);
    }

    #[test]
    fn round_trip_exact_boundary() {
        // 254 bytes = single frame
        let data254 = vec![0x11; 254];
        let frames = build_lora_frames(&data254, 0x90);
        assert_eq!(frames.len(), 1);
        let mut r = SplitReassembler::new();
        assert_eq!(r.feed(&frames[0], 0).unwrap(), data254);

        // 255 bytes = split
        let data255 = vec![0x22; 255];
        let frames = build_lora_frames(&data255, 0x90);
        assert_eq!(frames.len(), 2);
        let mut r = SplitReassembler::new();
        assert_eq!(r.feed(&frames[0], 0), None);
        assert_eq!(r.feed(&frames[1], 1).unwrap(), data255);
    }

    #[test]
    fn interleaved_sequences() {
        let mut r = SplitReassembler::new();
        let frame_a1 = vec![0xA0 | FLAG_SPLIT, 0x11];
        let frame_b1 = vec![0xB0 | FLAG_SPLIT, 0x22];
        assert_eq!(r.feed(&frame_a1, 0), None);
        // frame_b1 arrives, frame_a1 is discarded, frame_b1 is now buffered
        assert_eq!(r.feed(&frame_b1, 1), None);
        // frame_a2 arrives, sequence mismatch with frame_b1, frame_b1 is discarded,
        // frame_a2 becomes the new buffer
        let frame_a2 = vec![0xA0 | FLAG_SPLIT, 0x33];
        assert_eq!(r.feed(&frame_a2, 2), None);
        // Another A-sequence frame arrives, same sequence as buffered frame, reassemble
        let frame_a3 = vec![0xA0 | FLAG_SPLIT, 0x44];
        let result = r.feed(&frame_a3, 3).unwrap();
        assert_eq!(result, vec![0x33, 0x44]);
    }

    #[test]
    fn split_second_half_never_arrives() {
        let mut r = SplitReassembler::new();
        let frame = vec![0x50 | FLAG_SPLIT, 0xAA, 0xBB];
        assert_eq!(r.feed(&frame, 0), None);
        // No second half, timeout will clear it
        r.check_timeout(10, 10);
        assert_eq!(r.seq, None);
    }

    #[test]
    fn two_complete_split_packets_in_sequence() {
        let mut r = SplitReassembler::new();
        // First split pair (seq 0x30)
        let f1a = vec![0x30 | FLAG_SPLIT, 0x11, 0x22];
        let f1b = vec![0x30 | FLAG_SPLIT, 0x33, 0x44];
        assert_eq!(r.feed(&f1a, 0), None);
        assert_eq!(r.feed(&f1b, 1).unwrap(), vec![0x11, 0x22, 0x33, 0x44]);
        // Second split pair (seq 0x70)
        let f2a = vec![0x70 | FLAG_SPLIT, 0xAA];
        let f2b = vec![0x70 | FLAG_SPLIT, 0xBB];
        assert_eq!(r.feed(&f2a, 2), None);
        assert_eq!(r.feed(&f2b, 3).unwrap(), vec![0xAA, 0xBB]);
    }

    #[test]
    fn non_split_between_split_halves() {
        let mut r = SplitReassembler::new();
        // First half of split
        let split1 = vec![0x30 | FLAG_SPLIT, 0xAA];
        assert_eq!(r.feed(&split1, 0), None);
        // Non-split interrupts, buffer cleared, non-split delivered
        let single = vec![0x50, 0xDD];
        assert_eq!(r.feed(&single, 1).unwrap(), vec![0xDD]);
        // Second half with same seq, treated as NEW first half (buffer was cleared)
        let split2 = vec![0x30 | FLAG_SPLIT, 0xBB];
        assert_eq!(r.feed(&split2, 2), None);
    }

    // Radio config wire protocol tests
    fn medium_profile() -> RadioConfigWire {
        RadioConfigWire {
            frequency_hz: 869_525_000,
            bandwidth_hz: 125_000,
            sf: 7,
            cr: 5,
            tx_power_dbm: 17,
            preamble_len: 24,
            csma_enabled: false,
            radio_silent: false,
        }
    }

    fn fast_profile() -> RadioConfigWire {
        RadioConfigWire {
            frequency_hz: 869_525_000,
            bandwidth_hz: 500_000,
            sf: 7,
            cr: 5,
            tx_power_dbm: 17,
            preamble_len: 24,
            csma_enabled: false,
            radio_silent: false,
        }
    }

    fn slow_profile() -> RadioConfigWire {
        RadioConfigWire {
            frequency_hz: 869_525_000,
            bandwidth_hz: 125_000,
            sf: 10,
            cr: 8,
            tx_power_dbm: 17,
            preamble_len: 24,
            csma_enabled: false,
            radio_silent: false,
        }
    }

    #[test]
    fn radio_config_round_trip_medium() {
        let cfg = medium_profile();
        let frame = build_radio_config_frame(&cfg);
        assert_eq!(frame.len(), RADIO_CONFIG_FRAME_LEN);
        assert_eq!(&frame[0..2], &RADIO_CONFIG_MAGIC);
        let parsed = parse_radio_config(&frame[2..]).unwrap();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn radio_config_round_trip_fast() {
        let cfg = fast_profile();
        let frame = build_radio_config_frame(&cfg);
        let parsed = parse_radio_config(&frame[2..]).unwrap();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn radio_config_round_trip_slow() {
        let cfg = slow_profile();
        let frame = build_radio_config_frame(&cfg);
        let parsed = parse_radio_config(&frame[2..]).unwrap();
        assert_eq!(parsed, cfg);
    }

    #[test]
    fn radio_config_byte_layout() {
        let cfg = medium_profile();
        let frame = build_radio_config_frame(&cfg);
        // magic
        assert_eq!(frame[0], 0xA4);
        assert_eq!(frame[1], 0xA4);
        // freq 869525000 = 0x33D3_E608
        assert_eq!(&frame[2..6], &[0x33, 0xD3, 0xE6, 0x08]);
        // bw 125000 = 0x0001_E848
        assert_eq!(&frame[6..10], &[0x00, 0x01, 0xE8, 0x48]);
        // sf=7, cr=5, txp=17
        assert_eq!(frame[10], 7);
        assert_eq!(frame[11], 5);
        assert_eq!(frame[12], 17);
        // preamble 24 = 0x0018
        assert_eq!(&frame[13..15], &[0x00, 0x18]);
        // csma_enabled = false
        assert_eq!(frame[15], 0);
        // radio_silent = false
        assert_eq!(frame[16], 0);
    }

    #[test]
    fn radio_config_parse_too_short() {
        assert!(parse_radio_config(&[0; 12]).is_none());
    }

    #[test]
    fn radio_config_parse_too_long() {
        assert!(parse_radio_config(&[0; 16]).is_none());
    }

    #[test]
    fn radio_config_parse_13_byte_backward_compat() {
        // Old 13-byte payload (no csma, no radio_silent) must still parse,
        // defaulting both flags to false.
        let cfg = medium_profile();
        let frame = build_radio_config_frame(&cfg);
        // Drop magic (2) and the trailing 2 flag bytes -> exactly 13 bytes
        let legacy = &frame[2..frame.len() - 2];
        assert_eq!(legacy.len(), 13);
        let parsed = parse_radio_config(legacy).unwrap();
        assert!(!parsed.csma_enabled);
        assert!(!parsed.radio_silent);
        assert_eq!(parsed.sf, 7);
    }

    #[test]
    fn radio_config_parse_14_byte_backward_compat() {
        // 14-byte payload from before the radio_silent flag existed, the
        // csma byte is present but radio_silent must default to false.
        let cfg = RadioConfigWire {
            csma_enabled: true,
            ..medium_profile()
        };
        let frame = build_radio_config_frame(&cfg);
        let legacy = &frame[2..frame.len() - 1];
        assert_eq!(legacy.len(), 14);
        let parsed = parse_radio_config(legacy).unwrap();
        assert!(parsed.csma_enabled);
        assert!(!parsed.radio_silent);
    }

    #[test]
    fn radio_config_round_trip_radio_silent() {
        let cfg = RadioConfigWire {
            csma_enabled: true,
            radio_silent: true,
            ..medium_profile()
        };
        let frame = build_radio_config_frame(&cfg);
        assert_eq!(frame.len(), RADIO_CONFIG_FRAME_LEN);
        assert_eq!(frame[15], 1);
        assert_eq!(frame[16], 1);
        let parsed = parse_radio_config(&frame[2..]).unwrap();
        assert_eq!(parsed, cfg);
        assert!(parsed.radio_silent);
    }

    #[test]
    fn radio_config_round_trip_csma_enabled() {
        let cfg = RadioConfigWire {
            csma_enabled: true,
            ..medium_profile()
        };
        let frame = build_radio_config_frame(&cfg);
        assert_eq!(frame.len(), RADIO_CONFIG_FRAME_LEN);
        assert_eq!(frame[15], 1);
        let parsed = parse_radio_config(&frame[2..]).unwrap();
        assert_eq!(parsed, cfg);
        assert!(parsed.csma_enabled);
    }

    #[test]
    fn radio_config_parse_invalid_sf() {
        let cfg = medium_profile();
        let frame = build_radio_config_frame(&cfg);
        let mut data = frame[2..].to_vec();
        data[8] = 13; // SF out of range
        assert!(parse_radio_config(&data).is_none());
    }

    #[test]
    fn radio_config_parse_invalid_cr() {
        let cfg = medium_profile();
        let frame = build_radio_config_frame(&cfg);
        let mut data = frame[2..].to_vec();
        data[9] = 4; // CR too low
        assert!(parse_radio_config(&data).is_none());
        data[9] = 9; // CR too high
        assert!(parse_radio_config(&data).is_none());
    }

    #[test]
    fn radio_config_negative_tx_power() {
        let cfg = RadioConfigWire {
            tx_power_dbm: -3,
            ..medium_profile()
        };
        let frame = build_radio_config_frame(&cfg);
        let parsed = parse_radio_config(&frame[2..]).unwrap();
        assert_eq!(parsed.tx_power_dbm, -3);
    }

    #[test]
    fn radio_config_all_bandwidths() {
        for &bw in &[
            7_810u32, 10_420, 15_630, 20_830, 31_250, 41_670, 62_500, 125_000, 250_000, 500_000,
        ] {
            let cfg = RadioConfigWire {
                bandwidth_hz: bw,
                ..medium_profile()
            };
            let frame = build_radio_config_frame(&cfg);
            let parsed = parse_radio_config(&frame[2..]).unwrap();
            assert_eq!(parsed.bandwidth_hz, bw);
        }
    }

    #[test]
    fn radio_config_all_coding_rates() {
        for cr in 5..=8u8 {
            let cfg = RadioConfigWire {
                cr,
                ..medium_profile()
            };
            let frame = build_radio_config_frame(&cfg);
            let parsed = parse_radio_config(&frame[2..]).unwrap();
            assert_eq!(parsed.cr, cr);
        }
    }

    #[test]
    fn radio_config_sf_boundaries() {
        for sf in 5..=12u8 {
            let cfg = RadioConfigWire {
                sf,
                ..medium_profile()
            };
            let frame = build_radio_config_frame(&cfg);
            let parsed = parse_radio_config(&frame[2..]).unwrap();
            assert_eq!(parsed.sf, sf);
        }
    }

    #[test]
    fn radio_config_ack_format() {
        assert_eq!(RADIO_CONFIG_ACK, [0xA4, 0xA4, 0x01]);
    }
}
