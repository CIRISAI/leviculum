//! The SX1262's SPI port on an ESP32-class board.
//!
//! This is a *bus*, not a driver. Everything that decides what goes on the
//! air — the opcode sequences, the register brackets, the transmit-power
//! program, the IRQ masks, the RX-extend arithmetic — lives in
//! [`leviculum_core::sx126x`], where a host test can run it against a fake
//! chip. That module reaches the hardware through exactly two traits,
//! [`RegisterBus`] and
//! [`CommandBus`], and what this file
//! adds is their implementation over an esp-hal SPI bus and two GPIOs.
//!
//! The same division is already in place on the nRF side
//! (`leviculum-nrf/src/sx1262.rs`, two forwarding methods at the bottom of
//! a 1300-line driver). Writing a second SX1262 driver here would mean a
//! second copy of the sequences, which is a second thing to keep in step
//! with the datasheet and with the reference firmware; the whole reason
//! the sequences were lifted into `leviculum-core` was that a constant or
//! an ordering that lives in one firmware crate is one nothing can check.
//!
//! # What this file owns
//!
//! Three things, and they are the three the chip's SPI protocol demands
//! (SX1262 datasheet rev 2.1, §13.1 and §13.2):
//!
//!  * the frame around a command — chip-select low, opcode, arguments,
//!    chip-select high;
//!  * the frame around a register access, which is a command
//!    (`WriteRegister` 0x0D / `ReadRegister` 0x1D) with a 16-bit address
//!    and, on the read side, one status byte to discard before the data;
//!  * the BUSY handshake, which precedes every one of them: the chip
//!    raises BUSY while it digests the previous command and ignores the
//!    bus until it drops.
//!
//! # Blocking inside an `async fn`
//!
//! The two traits are async because the nRF driver behind them is. Here
//! the BUSY poll spins rather than yields. That is honest for what this
//! step is — nothing else runs yet, there is no executor — but it is not
//! where it should end up, and the bound is written as a deadline rather
//! than a retry count so that turning the spin into a yield is a change to
//! one statement and not to the shape of the function.

use embedded_hal_async::spi::SpiBus;
use esp_hal::{
    gpio::{Input, Output},
    time::{Duration, Instant},
};
use leviculum_core::sx126x::{CommandBus, RegisterBus};

/// SX126x opcodes this file issues on its own account.
///
/// Only the two register-access opcodes are here. Every other opcode
/// belongs to a sequence, and a sequence lives in
/// [`leviculum_core::sx126x`] beside the host test that can run it — the
/// same rule `leviculum-nrf/src/sx1262.rs` states for `SetPaConfig` and
/// `SetTxParams`. A third copy of an opcode table would be a third thing
/// to keep in step.
mod opcode {
    /// `WriteRegister`, datasheet §13.2.1.
    pub const WRITE_REGISTER: u8 = 0x0D;
    /// `ReadRegister`, datasheet §13.2.2.
    pub const READ_REGISTER: u8 = 0x1D;
}

/// How long a BUSY may stay high before the access is abandoned.
///
/// Public so the error it produces can name it.
///
/// The longest documented BUSY on this part is the image calibration at
/// about 3.5 ms; 100 ms is the same bound the RNode firmware uses and what
/// `leviculum-nrf` carries, so a board that is wedged reports it in the
/// same time on either family.
pub const BUSY_TIMEOUT: Duration = Duration::from_millis(100);

/// What a bus access can fail with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The SPI peripheral refused the transfer.
    Spi,
    /// BUSY stayed high past [`BUSY_TIMEOUT`]. Either the chip is not
    /// powered, or it is not there, or it is still executing something
    /// that should have finished long ago — all three are "do not send
    /// the next command", which is the only distinction the caller can
    /// act on.
    Busy,
    /// The caller handed in a command whose argument list does not fit
    /// the fixed frame buffer. A bug, not a bus condition, and named
    /// separately so it can never be read as one.
    Frame,
}

/// The SX1262's SPI port: the bus, its chip-select, and its BUSY line.
///
/// Chip-select is a plain [`Output`] rather than the SPI peripheral's own
/// CS, because the SX126x protocol needs the line held across a
/// write-then-read pair and released at a point the peripheral's
/// per-transfer CS does not offer.
pub struct Sx1262Bus<'d, SPI> {
    spi: SPI,
    nss: Output<'d>,
    busy: Input<'d>,
}

impl<'d, SPI: SpiBus> Sx1262Bus<'d, SPI> {
    /// Take ownership of the port.
    ///
    /// `nss` must already be high: the chip samples it at its own reset
    /// and a low line during the SoC's boot is a partial command.
    pub fn new(spi: SPI, nss: Output<'d>, busy: Input<'d>) -> Self {
        Self { spi, nss, busy }
    }

    /// Block until BUSY drops, or give up after [`BUSY_TIMEOUT`].
    fn wait_busy(&mut self) -> Result<(), Error> {
        let deadline = Instant::now() + BUSY_TIMEOUT;
        while self.busy.is_high() {
            if Instant::now() >= deadline {
                return Err(Error::Busy);
            }
        }
        Ok(())
    }

    /// One chip-select-framed transfer: write `write`, then read
    /// `read.len()` bytes while clocking out zeros.
    ///
    /// Both halves are optional — a command is write-only, a register
    /// read is both — and chip-select spans the pair, which is why this
    /// is one function and not two.
    async fn transaction(&mut self, write: &[u8], read: &mut [u8]) -> Result<(), Error> {
        self.wait_busy()?;
        self.nss.set_low();
        let result = async {
            if !write.is_empty() {
                self.spi.write(write).await.map_err(|_| Error::Spi)?;
            }
            if !read.is_empty() {
                self.spi.read(read).await.map_err(|_| Error::Spi)?;
            }
            self.spi.flush().await.map_err(|_| Error::Spi)
        }
        .await;
        self.nss.set_high();
        result
    }
}

impl<SPI: SpiBus> RegisterBus for Sx1262Bus<'_, SPI> {
    type Error = Error;

    async fn read_reg(&mut self, addr: u16) -> Result<u8, Error> {
        // `ReadRegister` answers with one status byte before the first
        // data byte (datasheet §13.2.2, Table 13-3). Reading it into the
        // same buffer and discarding it is what makes the byte returned
        // here the register and not the status.
        let header = [
            opcode::READ_REGISTER,
            (addr >> 8) as u8,
            (addr & 0xFF) as u8,
        ];
        let mut answer = [0u8; 2];
        self.transaction(&header, &mut answer).await?;
        Ok(answer[1])
    }

    async fn write_reg(&mut self, addr: u16, value: u8) -> Result<(), Error> {
        let frame = [
            opcode::WRITE_REGISTER,
            (addr >> 8) as u8,
            (addr & 0xFF) as u8,
            value,
        ];
        self.transaction(&frame, &mut []).await
    }
}

impl<SPI: SpiBus> CommandBus for Sx1262Bus<'_, SPI> {
    async fn write_cmd(&mut self, opcode: u8, args: &[u8]) -> Result<(), Error> {
        // The opcode and its arguments are one chip-select frame, so they
        // cannot be two `write` calls with the line dropped between them.
        // The longest argument list this driver will ever issue is
        // `SetPacketParams` at nine bytes; sixteen leaves room without
        // putting a heap allocation in the radio path.
        let mut frame = [0u8; 16];
        if args.len() + 1 > frame.len() {
            return Err(Error::Frame);
        }
        frame[0] = opcode;
        frame[1..1 + args.len()].copy_from_slice(args);
        self.transaction(&frame[..1 + args.len()], &mut []).await
    }
}
