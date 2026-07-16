//! Minimal SX1262 LoRa radio driver for T114.
//!
//! Talks to the SX1262 via SPI using Embassy's async SPI infrastructure.
//! Every function follows the Semtech reference driver (sx126x.c).

use embassy_nrf::gpio::{Input, Output};
use embassy_time::{with_timeout, Duration, Timer};
use embedded_hal_async::spi::{Operation, SpiDevice as SpiDeviceTrait};

/// SX1262 opcodes (from sx126x.h / datasheet §11)
mod opcode {
    pub const GET_STATUS: u8 = 0xC0;
    pub const GET_IRQ_STATUS: u8 = 0x12;
    pub const CLEAR_IRQ_STATUS: u8 = 0x02;
    pub const SET_DIO_IRQ_PARAMS: u8 = 0x08;
    pub const SET_STANDBY: u8 = 0x80;
    pub const SET_TX: u8 = 0x83;
    pub const SET_REGULATOR_MODE: u8 = 0x96;
    pub const SET_DIO3_AS_TCXO_CTRL: u8 = 0x97;
    pub const CLEAR_DEVICE_ERRORS: u8 = 0x07;
    pub const CALIBRATE: u8 = 0x89;
    pub const CALIBRATE_IMAGE: u8 = 0x98;
    pub const SET_DIO2_AS_RF_SWITCH: u8 = 0x9D;
    pub const SET_PACKET_TYPE: u8 = 0x8A;
    pub const SET_RF_FREQUENCY: u8 = 0x86;
    pub const SET_PA_CONFIG: u8 = 0x95;
    pub const SET_TX_PARAMS: u8 = 0x8E;
    pub const SET_BUFFER_BASE_ADDRESS: u8 = 0x8F;
    pub const SET_MODULATION_PARAMS: u8 = 0x8B;
    pub const SET_PACKET_PARAMS: u8 = 0x8C;
    pub const WRITE_REGISTER: u8 = 0x0D;
    pub const READ_REGISTER: u8 = 0x1D;
    pub const WRITE_BUFFER: u8 = 0x0E;
    pub const READ_BUFFER: u8 = 0x1E;
    pub const SET_RX: u8 = 0x82;
    pub const GET_RX_BUFFER_STATUS: u8 = 0x13;
    pub const GET_PACKET_STATUS: u8 = 0x14;
    pub const SET_CAD_PARAMS: u8 = 0x88;
    pub const SET_CAD: u8 = 0xC5;
    pub const SET_STOP_RX_TIMER_ON_PREAMBLE: u8 = 0x9F;
}

/// SX1262 register addresses (datasheet §15, key register table)
mod reg {
    pub const LORA_SYNC_WORD: u16 = 0x0740;
    pub const TX_CLAMP_CONFIG: u16 = 0x08D8;
    pub const RTC_CONTROL: u16 = 0x0902;
    pub const EVENT_MASK: u16 = 0x0944;
}

/// IRQ bitmasks (datasheet §8.5, Table 8-4)
mod irq {
    pub const TX_DONE: u16 = 0x0001;
    pub const RX_DONE: u16 = 0x0002;
    pub const PREAMBLE_DETECTED: u16 = 0x0004; // bit 2
    pub const HEADER_VALID: u16 = 0x0010; // bit 4
    pub const CRC_ERR: u16 = 0x0040;
    pub const TIMEOUT: u16 = 0x0200;

    pub const CAD_DONE: u16 = 0x0080; // bit 7
    pub const CAD_DETECTED: u16 = 0x0100; // bit 8

    pub const TX_ALL: u16 = TX_DONE | TIMEOUT;
    pub const RX_ALL: u16 = RX_DONE | CRC_ERR | TIMEOUT;
    pub const CAD_ALL: u16 = CAD_DONE | CAD_DETECTED;
}

/// Received packet status (RSSI and SNR).
pub struct RxStatus {
    pub rssi: i16,
    pub snr: i16,
}

/// SX1262 error type
#[derive(Debug)]
pub enum Error {
    Spi,
    Busy,
    Timeout,
    Crc,
}

/// Parsed SX1262 status byte
#[derive(Clone, Copy)]
pub struct ChipStatus {
    pub raw: u8,
    /// Chip mode: 0x2=STBY_RC, 0x3=STBY_XOSC, 0x4=FS, 0x5=RX, 0x6=TX
    pub mode: u8,
    /// Command status: 0x2=data_avail, 0x3=cmd_timeout, 0x4=cmd_error, 0x5=exec_fail, 0x6=tx_done
    pub cmd: u8,
}

impl ChipStatus {
    fn from_raw(raw: u8) -> Self {
        Self {
            raw,
            mode: (raw >> 4) & 0x07,
            cmd: (raw >> 1) & 0x07,
        }
    }
}

/// Encode a u32 as 3-byte big-endian (24-bit). Used for SX1262 timeout fields.
fn u24_be(val: u32) -> [u8; 3] {
    [
        ((val >> 16) & 0xFF) as u8,
        ((val >> 8) & 0xFF) as u8,
        (val & 0xFF) as u8,
    ]
}

/// Convert an SX1262 bandwidth register code back to bandwidth in Hz
/// (inverse of the table in `RadioConfig::from_wire`, datasheet Table 14-47).
/// Returns 0 for an unknown code so callers can treat it as "not configured".
fn bw_code_to_hz(code: u8) -> u32 {
    match code {
        0x00 => 7_810,
        0x08 => 10_420,
        0x01 => 15_630,
        0x09 => 20_830,
        0x02 => 31_250,
        0x0A => 41_670,
        0x03 => 62_500,
        0x04 => 125_000,
        0x05 => 250_000,
        0x06 => 500_000,
        _ => 0,
    }
}

/// SX1262 driver, generic over the SPI device type.
pub struct Sx1262<SPI> {
    spi: SPI,
    reset: Output<'static>,
    busy: Input<'static>,
    dio1: Input<'static>,
    preamble_len: u16,
    /// SetDIO3AsTcxoCtrl voltage select byte (0x02 = 1.8 V).
    /// Board-specific; stored at construction so `init_radio` is parameter-free.
    tcxo_voltage_reg: u8,
    /// Cached human-readable modulation params from the last `configure_lora`,
    /// used only to size the RX software-wait extension in `receive()`. `bw_hz`
    /// is 0 until the radio is configured (guards the airtime computation).
    rx_ext_sf: u8,
    rx_ext_bw_hz: u32,
    rx_ext_cr_denom: u8,
}

impl<SPI: SpiDeviceTrait> Sx1262<SPI> {
    /// Create a new SX1262 driver. Does NOT initialize the radio, call `reset()` + init_radio() first.
    ///
    /// `tcxo_voltage_reg` is the SetDIO3AsTcxoCtrl voltage select byte (0x02 = 1.8 V
    /// on T114 and RAK4631; see datasheet §13.3.6 Table 13-35).
    pub fn new(
        spi: SPI,
        reset: Output<'static>,
        busy: Input<'static>,
        dio1: Input<'static>,
        tcxo_voltage_reg: u8,
    ) -> Self {
        Self {
            spi,
            reset,
            busy,
            dio1,
            preamble_len: 24,
            tcxo_voltage_reg,
            rx_ext_sf: 0,
            rx_ext_bw_hz: 0,
            rx_ext_cr_denom: 0,
        }
    }

    /// Wait for BUSY pin LOW via GPIOTE interrupt, with timeout.
    async fn wait_busy_ms(&mut self, timeout_ms: u32) -> Result<(), Error> {
        if self.busy.is_low() {
            return Ok(());
        }
        with_timeout(
            Duration::from_millis(timeout_ms as u64),
            self.busy.wait_for_low(),
        )
        .await
        .map_err(|_| Error::Busy)
    }

    /// Wait for BUSY pin to go LOW (radio ready for commands). 100ms timeout.
    pub async fn wait_busy(&mut self) -> Result<(), Error> {
        self.wait_busy_ms(100).await
    }

    /// Reset the SX1262 via the reset pin.
    pub async fn reset(&mut self) {
        self.reset.set_low();
        Timer::after_millis(10).await;
        self.reset.set_high();
        Timer::after_millis(10).await;
    }

    /// Write a command to the SX1262 (opcode + optional params).
    /// Waits for BUSY before sending.
    pub async fn write_command(&mut self, opcode: u8, params: &[u8]) -> Result<(), Error> {
        self.wait_busy().await?;
        let mut buf = [0u8; 16];
        buf[0] = opcode;
        let len = 1 + params.len();
        buf[1..len].copy_from_slice(params);
        self.spi
            .transaction(&mut [Operation::Write(&buf[..len])])
            .await
            .map_err(|_| Error::Spi)
    }

    /// Read from the SX1262 (opcode + NOP byte, then read response).
    /// Returns the status byte.
    pub async fn read_command(&mut self, opcode: u8, response: &mut [u8]) -> Result<u8, Error> {
        self.wait_busy().await?;
        let cmd = [opcode];
        let mut status_buf = [0u8; 1];
        if response.is_empty() {
            self.spi
                .transaction(&mut [Operation::Write(&cmd), Operation::Read(&mut status_buf)])
                .await
                .map_err(|_| Error::Spi)?;
        } else {
            self.spi
                .transaction(&mut [
                    Operation::Write(&cmd),
                    Operation::Read(&mut status_buf),
                    Operation::Read(response),
                ])
                .await
                .map_err(|_| Error::Spi)?;
        }
        Ok(status_buf[0])
    }

    /// Read the SX1262 status register (GetStatus, opcode 0xC0).
    pub async fn get_status(&mut self) -> Result<ChipStatus, Error> {
        self.wait_busy().await?;
        let tx = [opcode::GET_STATUS, 0x00];
        let mut rx = [0u8; 2];
        self.spi
            .transaction(&mut [Operation::Transfer(&mut rx, &tx)])
            .await
            .map_err(|_| Error::Spi)?;
        Ok(ChipStatus::from_raw(rx[1]))
    }

    /// Read the SX1262 IRQ status (GetIrqStatus, opcode 0x12).
    pub async fn get_irq_status(&mut self) -> Result<u16, Error> {
        let mut buf = [0u8; 2];
        let _status = self.read_command(opcode::GET_IRQ_STATUS, &mut buf).await?;
        Ok(((buf[0] as u16) << 8) | buf[1] as u16)
    }

    /// Set standby mode (STBY_RC).
    pub async fn set_standby_rc(&mut self) -> Result<(), Error> {
        self.write_command(opcode::SET_STANDBY, &[0x00]).await
    }

    /// Configure DIO3 as TCXO control. Timeout is 24-bit in units of 15.625µs.
    pub async fn set_dio3_as_tcxo_ctrl(&mut self, voltage: u8, timeout: u32) -> Result<(), Error> {
        let t = u24_be(timeout);
        self.write_command(
            opcode::SET_DIO3_AS_TCXO_CTRL,
            &[voltage & 0x07, t[0], t[1], t[2]],
        )
        .await
    }

    /// Clear device errors (required before TCXO setup after cold boot).
    pub async fn clear_device_errors(&mut self) -> Result<(), Error> {
        self.write_command(opcode::CLEAR_DEVICE_ERRORS, &[0x00, 0x00])
            .await
    }

    /// Run full calibration (mask 0x7F = all blocks). BUSY is high during calibration.
    pub async fn calibrate(&mut self, mask: u8) -> Result<(), Error> {
        self.write_command(opcode::CALIBRATE, &[mask]).await?;
        self.wait_busy_ms(500).await
    }

    /// Calibrate image for a specific frequency band.
    pub async fn calibrate_image(&mut self, freq_hz: u32) -> Result<(), Error> {
        let (f1, f2) = if freq_hz > 900_000_000 {
            (0xE1, 0xE9)
        } else if freq_hz > 850_000_000 {
            (0xD7, 0xDB)
        } else if freq_hz > 770_000_000 {
            (0xC1, 0xC5)
        } else if freq_hz > 460_000_000 {
            (0x75, 0x81)
        } else {
            (0x6B, 0x6F)
        };
        self.write_command(opcode::CALIBRATE_IMAGE, &[f1, f2])
            .await?;
        self.wait_busy_ms(500).await
    }

    /// Full initialization sequence (SX1262 + TCXO + LDO regulator).
    /// Order follows the datasheet init summary (§9.2.1, §13).
    /// Call after reset().
    pub async fn init_radio(&mut self, freq_hz: u32) -> Result<ChipStatus, Error> {
        self.set_standby_rc().await?;
        self.write_command(opcode::SET_REGULATOR_MODE, &[0x00])
            .await?; // LDO: RNode firmware sx126x.cpp begin() never calls
                     // OP_REGULATOR_MODE, leaving the SX1262 at its LDO default.
        self.write_command(opcode::SET_DIO2_AS_RF_SWITCH, &[0x01])
            .await?;
        self.clear_device_errors().await?;
        // Datasheet §9.2.1: calibration must be done AFTER SetDIO3AsTcxoCtrl.
        // Timeout 0x0000FF (~4 ms) is the value RNode firmware uses.
        self.set_dio3_as_tcxo_ctrl(self.tcxo_voltage_reg, 0x0000FF)
            .await?;
        self.calibrate(0x7F).await?;
        self.calibrate_image(freq_hz).await?;
        self.write_command(opcode::SET_PACKET_TYPE, &[0x01]).await?; // LoRa
        self.get_status().await
    }

    // Frequency, modulation, packet params
    /// Write a register (datasheet §13.2.1).
    pub async fn write_register(&mut self, addr: u16, data: &[u8]) -> Result<(), Error> {
        self.wait_busy().await?;
        let mut buf = [0u8; 19];
        buf[0] = opcode::WRITE_REGISTER;
        buf[1] = (addr >> 8) as u8;
        buf[2] = (addr & 0xFF) as u8;
        let len = 3 + data.len();
        buf[3..len].copy_from_slice(data);
        self.spi
            .transaction(&mut [Operation::Write(&buf[..len])])
            .await
            .map_err(|_| Error::Spi)
    }

    /// Read a register (datasheet §13.2.2).
    pub async fn read_register(&mut self, addr: u16, data: &mut [u8]) -> Result<(), Error> {
        self.wait_busy().await?;
        let cmd = [
            opcode::READ_REGISTER,
            (addr >> 8) as u8,
            (addr & 0xFF) as u8,
        ];
        let mut status = [0u8; 1];
        self.spi
            .transaction(&mut [
                Operation::Write(&cmd),
                Operation::Read(&mut status),
                Operation::Read(data),
            ])
            .await
            .map_err(|_| Error::Spi)
    }

    /// Set RF frequency (datasheet §13.4.1). RF_Freq = freq_hz * 2^25 / 32_000_000
    pub async fn set_rf_frequency(&mut self, freq_hz: u32) -> Result<(), Error> {
        let rf_freq = ((freq_hz as u64 * (1u64 << 25)) / 32_000_000) as u32;
        self.write_command(
            opcode::SET_RF_FREQUENCY,
            &[
                (rf_freq >> 24) as u8,
                (rf_freq >> 16) as u8,
                (rf_freq >> 8) as u8,
                rf_freq as u8,
            ],
        )
        .await
    }

    /// Set LoRa packet params (datasheet §13.4.6).
    pub async fn set_packet_params(&mut self, payload_len: u8) -> Result<(), Error> {
        self.write_command(
            opcode::SET_PACKET_PARAMS,
            &[
                (self.preamble_len >> 8) as u8,
                self.preamble_len as u8,
                0x00, // explicit header
                payload_len,
                0x01, // CRC on
                0x00, // standard IQ
            ],
        )
        .await
    }

    /// Apply TX PA clamp workaround (datasheet §15.2).
    async fn apply_tx_clamp_workaround(&mut self) -> Result<(), Error> {
        let mut val = [0u8; 1];
        self.read_register(reg::TX_CLAMP_CONFIG, &mut val).await?;
        val[0] |= 0x1E;
        self.write_register(reg::TX_CLAMP_CONFIG, &val).await
    }

    /// Configure radio for LoRa TX/RX with specific parameters.
    /// Call after init_radio(). Sets frequency, PA, modulation, packet params, sync word.
    pub async fn configure_lora(
        &mut self,
        freq_hz: u32,
        sf: u8,
        bw: u8, // SX1262 bandwidth code: 0x04 = 125kHz
        cr: u8, // SX1262 coding rate code: 0x01 = 4/5
        power_dbm: i8,
        preamble_len: u16,
    ) -> Result<(), Error> {
        self.preamble_len = preamble_len;
        // Cache the modulation profile so `receive()` can size its software-wait
        // extension. `cr` is the SX1262 code (denominator - 4); `bw` is the
        // register code (see `bw_code_to_hz`).
        self.rx_ext_sf = sf;
        self.rx_ext_bw_hz = bw_code_to_hz(bw);
        self.rx_ext_cr_denom = cr.saturating_add(4);
        self.set_rf_frequency(freq_hz).await?;
        // PA config for target power (datasheet Table 13-21)
        let (pa_duty, hp_max) = match power_dbm {
            22 => (0x04, 0x07),
            20 => (0x03, 0x05),
            17 => (0x02, 0x03),
            _ => (0x02, 0x02), // 14 dBm
        };
        self.write_command(opcode::SET_PA_CONFIG, &[pa_duty, hp_max, 0x00, 0x01])
            .await?;
        // SetTxParams: +22 raw power (PA config limits actual output), ramp 200µs
        self.write_command(opcode::SET_TX_PARAMS, &[22u8, 0x04])
            .await?;
        self.write_command(opcode::SET_BUFFER_BASE_ADDRESS, &[0x00, 0x00])
            .await?;
        // LDRO: needed when symbol time > 16ms
        let ldro = if sf >= 11 && bw <= 0x04 { 1 } else { 0 };
        self.write_command(
            opcode::SET_MODULATION_PARAMS,
            &[sf, bw, cr, ldro, 0, 0, 0, 0],
        )
        .await?;
        self.set_packet_params(0xFF).await?;
        // Private network sync word (matches RNode)
        self.write_register(reg::LORA_SYNC_WORD, &[0x14, 0x24])
            .await?;
        self.apply_tx_clamp_workaround().await?;
        // StopRxTimerOnPreambleDetect: the SetRx hardware timeout only bounds the
        // preamble wait. Once a preamble is detected the timer stops and RX runs
        // to packet completion regardless of length. Without this a slow-SF
        // packet whose airtime exceeds the 500ms idle-loop timeout (e.g. SF10
        // path requests ~887ms, announces ~2590ms) is aborted mid-packet, so
        // the LNode receives almost nothing at slow spreading factors.
        // Persistent setting, re-asserted on every reconfig via this path.
        self.write_command(opcode::SET_STOP_RX_TIMER_ON_PREAMBLE, &[0x01])
            .await
    }

    // TX
    /// Transmit a packet. Blocks until TxDone or timeout.
    /// Call configure_lora() first to set frequency/modulation/power.
    pub async fn transmit(&mut self, data: &[u8], timeout_ms: u32) -> Result<(), Error> {
        self.set_packet_params(data.len() as u8).await?;
        self.write_command(opcode::SET_DIO_IRQ_PARAMS, &{
            let m = irq::TX_ALL;
            [(m >> 8) as u8, m as u8, (m >> 8) as u8, m as u8, 0, 0, 0, 0]
        })
        .await?;
        self.write_command(opcode::CLEAR_IRQ_STATUS, &[0xFF, 0xFF])
            .await?;

        // Write payload, use two SPI operations to avoid 258-byte stack buffer
        self.wait_busy().await?;
        let header = [opcode::WRITE_BUFFER, 0x00];
        self.spi
            .transaction(&mut [Operation::Write(&header), Operation::Write(data)])
            .await
            .map_err(|_| Error::Spi)?;

        // Start TX (no hardware timeout, we use our own)
        let t = u24_be(0);
        self.write_command(opcode::SET_TX, &t).await?;

        // Wait for DIO1 high (TxDone) via GPIOTE interrupt
        match with_timeout(
            Duration::from_millis(timeout_ms.max(100) as u64),
            self.dio1.wait_for_high(),
        )
        .await
        {
            Ok(()) => {
                let flags = self.get_irq_status().await?;
                self.write_command(opcode::CLEAR_IRQ_STATUS, &[0xFF, 0xFF])
                    .await?;
                if flags & irq::TX_DONE != 0 {
                    return Ok(());
                }
                let _ = self.set_standby_rc().await;
                Err(Error::Timeout)
            }
            Err(_) => {
                let _ = self.set_standby_rc().await;
                Err(Error::Timeout)
            }
        }
    }

    // RX
    /// Get RX buffer status: payload length and start pointer (datasheet §13.5.2).
    async fn get_rx_buffer_status(&mut self) -> Result<(u8, u8), Error> {
        let mut buf = [0u8; 2];
        let _status = self
            .read_command(opcode::GET_RX_BUFFER_STATUS, &mut buf)
            .await?;
        Ok((buf[0], buf[1]))
    }

    /// Read data from the RX buffer (datasheet §13.2.4).
    async fn read_buffer(&mut self, offset: u8, data: &mut [u8]) -> Result<(), Error> {
        self.wait_busy().await?;
        let cmd = [opcode::READ_BUFFER, offset];
        let mut status = [0u8; 1];
        self.spi
            .transaction(&mut [
                Operation::Write(&cmd),
                Operation::Read(&mut status),
                Operation::Read(data),
            ])
            .await
            .map_err(|_| Error::Spi)
    }

    /// Get packet status: RSSI and SNR (datasheet §13.5.3).
    async fn get_packet_status(&mut self) -> Result<RxStatus, Error> {
        let mut buf = [0u8; 3];
        let _status = self
            .read_command(opcode::GET_PACKET_STATUS, &mut buf)
            .await?;
        Ok(RxStatus {
            rssi: -(buf[0] as i16) / 2,
            snr: ((buf[1] as i8) as i16 + 2) / 4,
        })
    }

    /// Apply workaround 15.3: stop RTC after Rx with timeout (datasheet §15.3).
    async fn apply_rx_timeout_workaround(&mut self) -> Result<(), Error> {
        self.write_register(reg::RTC_CONTROL, &[0x00]).await?;
        let mut val = [0u8; 1];
        self.read_register(reg::EVENT_MASK, &mut val).await?;
        val[0] |= 0x02;
        self.write_register(reg::EVENT_MASK, &val).await
    }

    /// Receive a packet with timeout. Returns (bytes_written, RxStatus) on success.
    /// timeout_ms=0 means single mode (receive one packet then return to standby).
    pub async fn receive(
        &mut self,
        buf: &mut [u8],
        timeout_ms: u32,
    ) -> Result<(u8, RxStatus), Error> {
        self.write_command(opcode::SET_DIO_IRQ_PARAMS, &{
            let m = irq::RX_ALL;
            [(m >> 8) as u8, m as u8, (m >> 8) as u8, m as u8, 0, 0, 0, 0]
        })
        .await?;
        self.write_command(opcode::CLEAR_IRQ_STATUS, &[0xFF, 0xFF])
            .await?;

        // Convert timeout_ms to RTC steps (15.625µs per step = 64 steps/ms)
        let hw_timeout = if timeout_ms == 0 {
            0x000000 // single mode
        } else {
            (timeout_ms as u64 * 64).min(0xFFFFFF) as u32
        };
        let t = u24_be(hw_timeout);
        self.write_command(opcode::SET_RX, &t).await?;

        // Wait for DIO1 high via GPIOTE interrupt (hw timeout + margin)
        let sw_timeout_ms = if timeout_ms == 0 {
            60_000u64
        } else {
            timeout_ms as u64 + 500
        };
        let _ = with_timeout(
            Duration::from_millis(sw_timeout_ms),
            self.dio1.wait_for_high(),
        )
        .await;

        let mut flags = self.get_irq_status().await?;

        // The software wait (timeout_ms + 500) can expire while a slow-SF frame
        // is still on the air: StopRxTimerOnPreambleDetect halts the HW RX timer
        // once a preamble is detected, so a long frame (up to ~7s airtime) keeps
        // being received past the sw deadline. If the IRQ register shows a
        // preamble/header was detected (these bits latch even though they are
        // not routed to DIO1) but neither RxDone nor Timeout has fired, the
        // reception is still in progress; extend the wait by one max
        // single-frame airtime rather than aborting it with set_standby_rc.
        // Bounded to a single extension so a dead channel still times out.
        // The IRQ status is deliberately NOT cleared before the extension so a
        // pending RxDone survives into the extended wait.
        if self.rx_ext_bw_hz != 0
            && flags & (irq::RX_DONE | irq::TIMEOUT) == 0
            && flags & (irq::PREAMBLE_DETECTED | irq::HEADER_VALID) != 0
        {
            let extend_ms = leviculum_core::rnode::airtime_ms(
                (leviculum_core::rnode::MAX_SINGLE_PAYLOAD + 1) as u32,
                self.rx_ext_bw_hz,
                self.rx_ext_sf,
                self.rx_ext_cr_denom,
            );
            crate::log::log_fmt(
                "[SX_RX_EXTEND] ",
                format_args!(
                    "flags={:#06x} extend_ms={} sf={} bw_hz={}",
                    flags, extend_ms, self.rx_ext_sf, self.rx_ext_bw_hz
                ),
            );
            let _ = with_timeout(
                Duration::from_millis(extend_ms.max(1)),
                self.dio1.wait_for_high(),
            )
            .await;
            flags = self.get_irq_status().await?;
        }

        self.write_command(opcode::CLEAR_IRQ_STATUS, &[0xFF, 0xFF])
            .await?;

        // Workaround 15.3 for timed RX
        if hw_timeout != 0 && hw_timeout != 0xFFFFFF {
            let _ = self.apply_rx_timeout_workaround().await;
        }

        if flags & irq::RX_DONE != 0 {
            if flags & irq::CRC_ERR != 0 {
                return Err(Error::Crc);
            }
            let (len, ptr) = self.get_rx_buffer_status().await?;
            let read_len = (len as usize).min(buf.len());
            self.read_buffer(ptr, &mut buf[..read_len]).await?;
            let status = self.get_packet_status().await?;
            Ok((read_len as u8, status))
        } else if flags & irq::TIMEOUT != 0 {
            Err(Error::Timeout)
        } else {
            let _ = self.set_standby_rc().await;
            Err(Error::Timeout)
        }
    }

    // CAD (Channel Activity Detection)
    /// Perform a Channel Activity Detection. Returns true if a LoRa preamble
    /// was detected (channel busy), false if clear. Blocks until CadDone IRQ.
    /// Exit mode 0x00 leaves the chip in STBY_RC regardless of result.
    pub async fn cad(&mut self, sf: u8) -> Result<bool, Error> {
        // Datasheet Table 13-81 recommended cadDetPeak values per SF.
        // cadSymbolNum: 0x02 = 4 symbols, 0x03 = 8 symbols. For sf>=10 the
        // packet airtime is long (SF10 ~2.7s) and a 4-symbol CAD window usually
        // misses an in-progress peer transmission, so both nodes transmit and
        // collide. Listen over 8 symbols at slow SF to span more of the peer's
        // airtime and detect "busy" reliably. cadDetPeak unchanged.
        let (cad_sym_num, cad_det_peak) = match sf {
            7 | 8 => (0x02, 0x16),
            9 => (0x02, 0x17),
            10 => (0x03, 0x18),
            11 => (0x03, 0x19),
            12 => (0x03, 0x1A),
            _ => (0x02, 0x16),
        };
        let cad_det_min = 0x0A;
        let cad_exit_mode = 0x00; // CAD-only, return to STBY_RC
        let cad_timeout = [0u8; 3];

        self.write_command(
            opcode::SET_CAD_PARAMS,
            &[
                cad_sym_num,
                cad_det_peak,
                cad_det_min,
                cad_exit_mode,
                cad_timeout[0],
                cad_timeout[1],
                cad_timeout[2],
            ],
        )
        .await?;

        self.write_command(opcode::SET_DIO_IRQ_PARAMS, &{
            let m = irq::CAD_ALL;
            [(m >> 8) as u8, m as u8, (m >> 8) as u8, m as u8, 0, 0, 0, 0]
        })
        .await?;
        self.write_command(opcode::CLEAR_IRQ_STATUS, &[0xFF, 0xFF])
            .await?;

        self.write_command(opcode::SET_CAD, &[]).await?;

        // Allow for 2 symbols of CAD + margin. SF12/BW125 ~ 260ms.
        let timeout_ms = match sf {
            7 | 8 => 50,
            9 => 100,
            10 => 200,
            11 => 400,
            12 => 800,
            _ => 100,
        };
        match with_timeout(Duration::from_millis(timeout_ms), self.dio1.wait_for_high()).await {
            Ok(()) => {
                let flags = self.get_irq_status().await?;
                self.write_command(opcode::CLEAR_IRQ_STATUS, &[0xFF, 0xFF])
                    .await?;
                Ok((flags & irq::CAD_DETECTED) != 0)
            }
            Err(_) => {
                let _ = self.set_standby_rc().await;
                Err(Error::Timeout)
            }
        }
    }
}
