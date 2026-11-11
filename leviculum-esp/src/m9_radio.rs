//! The ThinkNode M9's LR1110 on SPI2, beside the panel (1478b).
//!
//! The bus rules are `loquax_m9::spi2`'s, stepped on the host there; this
//! module holds the board's half:
//!
//! - [`SPI2`], the arbiter, and the slot the bus lives in while nobody
//!   holds it. The panel keeps the bus (`SpiDma`) in its own field while
//!   it holds it and puts it here before it lets go; the radio takes it
//!   out for one transaction and puts it back on the panel's clock.
//! - [`RadioSpi`], the radio's `SpiDevice`: one granted transaction per
//!   call, its chip select held inside it only, the clock switched to
//!   [`m9::LORA_SPI_HZ`] and back ([`loquax_m9::spi2::run`]). Refused,
//!   it yields to core 0's loop and asks again on the next poll.
//! - [`Polled`], BUSY and IRQ as pins a future polls: no GPIO interrupt,
//!   no waker; the loop polls every turn.
//! - [`RadioTask`], the boot probe: reset, init with the board's facts,
//!   the default PHY, the version read, standby. The bench image goes on
//!   reading the chip's status every 10 ms, the load the `[SPI]` line's
//!   numbers are measured under.
//!
//! Nothing here transmits.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicU32, Ordering};

use critical_section::Mutex;
use embedded_hal::digital::{ErrorType as PinErrorType, InputPin};
use embedded_hal::spi::{ErrorKind, ErrorType, Operation};
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiDevice;
use esp_hal::gpio::{Input, Output};
use esp_hal::spi::master::{Config, SpiDma};
use esp_hal::spi::Mode;
use esp_hal::time::Rate;
use esp_hal::Blocking;
use leviculum_lr11xx::{Lr11xx, Version};
use loquax_m9::spi2::{self, Arbiter, Clocks, Phy, Wire};

use crate::boards::thinknode_m9 as m9;
use crate::time_driver::{poll_once, YieldOnce};
use crate::uptime_us;

/// The PHY the probe configures is the project's default, or the image
/// does not build (`project_lora_default_phy`, `RadioConfig::eu_medium`).
const _: () = assert!(
    m9::LORA_FREQUENCY_HZ == spi2::DEFAULT_PHY.frequency_hz
        && m9::LORA_BANDWIDTH_HZ == spi2::DEFAULT_PHY.bandwidth_hz
        && m9::LORA_SF == spi2::DEFAULT_PHY.sf
        && m9::LORA_CR_DENOM == spi2::DEFAULT_PHY.cr_denom
);

/// The board's PHY, out of its constants.
pub const PHY: Phy = Phy {
    frequency_hz: m9::LORA_FREQUENCY_HZ,
    bandwidth_hz: m9::LORA_BANDWIDTH_HZ,
    sf: m9::LORA_SF,
    cr_denom: m9::LORA_CR_DENOM,
};

/// The bandwidth's register code, the SX126x's, which the LR11xx shares;
/// a bandwidth without one does not build.
const BW_CODE: u8 = match leviculum_core::sx126x::bandwidth_code(PHY.bandwidth_hz) {
    Some(code) => code,
    None => panic!("the M9's bandwidth has no register code"),
};

/// The bus's two clocks: the panel's resting one, the radio's.
pub const CLOCKS: Clocks = Clocks {
    panel_hz: m9::LCD_SPI_MHZ * 1_000_000,
    radio_hz: m9::LORA_SPI_HZ,
};

/// SPI2's configuration at `hz`: mode 0 for both parts, the ST7789's and
/// the LR1110's (datasheet, "SPI interface": CPOL 0, CPHA 0).
pub fn config(hz: u32) -> Config {
    Config::default()
        .with_frequency(Rate::from_hz(hz))
        .with_mode(Mode::_0)
}

/// The board's SPI2 arbiter.
pub static SPI2: Arbiter = Arbiter::new();

/// The bus while neither the panel nor the radio holds it.
static SLOT: Mutex<RefCell<Option<SpiDma<'static, Blocking>>>> = Mutex::new(RefCell::new(None));

/// The longest the panel stood aside for the radio, from its yield to its
/// next chunk, since the last [`take_held_max_us`]: what its watchdog
/// feeding sees of the radio.
static HELD_MAX_US: AtomicU32 = AtomicU32::new(0);

fn now() -> u32 {
    uptime_us() as u32
}

/// The panel holds the bus from boot: its init and the splash run before
/// the radio exists.
pub fn panel_start() -> bool {
    SPI2.panel_take(now())
}

/// The panel takes or keeps the bus for its next chunk. `held` is the
/// bus it holds, if it does; it comes back `Some` with `true`. With
/// `false` a radio waits or has the bus: a panel that held it closes its
/// chip select and then gives it up with [`panel_give`], in that order,
/// so no radio byte reaches a selected panel.
pub fn panel_hold(held: &mut Option<SpiDma<'static, Blocking>>) -> bool {
    let at = now();
    if !SPI2.panel_take(at) {
        return false;
    }
    if held.is_none() {
        *held = critical_section::with(|cs| SLOT.borrow_ref_mut(cs).take());
        if held.is_none() {
            // Nobody holds the bus and it is not in the slot: it was lost
            // in a failed transfer. The panel reports its error.
            SPI2.panel_release(at);
            return false;
        }
    }
    true
}

/// The panel lets go of the bus: into the slot first, then free.
pub fn panel_give(spi: SpiDma<'static, Blocking>) {
    critical_section::with(|cs| *SLOT.borrow_ref_mut(cs) = Some(spi));
    SPI2.panel_release(now());
}

/// The panel resumed after standing aside `us` microseconds.
pub fn panel_held(us: u32) {
    HELD_MAX_US.fetch_max(us, Ordering::Relaxed);
}

/// [`panel_held`]'s longest since the last call.
pub fn take_held_max_us() -> u32 {
    HELD_MAX_US.swap(0, Ordering::Relaxed)
}

/// A bus error, as embedded-hal names it.
#[derive(Debug)]
pub enum BusError {
    Spi(esp_hal::spi::Error),
    Config(esp_hal::spi::master::ConfigError),
    /// Granted, but the bus was not in the slot.
    Lost,
}

impl embedded_hal::spi::Error for BusError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

/// The bus as the radio drives it for one transaction.
struct RadioWire<'a> {
    spi: &'a mut SpiDma<'static, Blocking>,
    nss: &'a mut Output<'static>,
}

impl Wire for RadioWire<'_> {
    type Error = BusError;

    fn clock(&mut self, hz: u32) -> Result<(), BusError> {
        self.spi.apply_config(&config(hz)).map_err(BusError::Config)
    }

    fn select(&mut self, selected: bool) {
        match selected {
            true => self.nss.set_low(),
            false => self.nss.set_high(),
        }
    }
}

/// The LR1110's `SpiDevice` on the shared bus.
pub struct RadioSpi {
    nss: Output<'static>,
}

impl RadioSpi {
    /// `nss` high, the radio deselected.
    pub fn new(nss: Output<'static>) -> Self {
        Self { nss }
    }
}

impl ErrorType for RadioSpi {
    type Error = BusError;
}

impl SpiDevice for RadioSpi {
    async fn transaction(&mut self, ops: &mut [Operation<'_, u8>]) -> Result<(), BusError> {
        while !SPI2.radio_take(now()) {
            YieldOnce::new().await;
        }
        let Some(mut spi) = critical_section::with(|cs| SLOT.borrow_ref_mut(cs).take()) else {
            SPI2.radio_release();
            return Err(BusError::Lost);
        };
        let r = spi2::run(
            &mut RadioWire {
                spi: &mut spi,
                nss: &mut self.nss,
            },
            CLOCKS,
            |w| {
                for op in ops.iter_mut() {
                    match op {
                        Operation::Read(buf) => w.spi.read(buf),
                        Operation::Write(buf) => w.spi.write(buf),
                        Operation::Transfer(read, write) => w.spi.transfer(read, write),
                        Operation::TransferInPlace(buf) => w.spi.transfer_in_place(buf),
                        Operation::DelayNs(ns) => {
                            esp_hal::delay::Delay::new().delay_nanos(*ns);
                            Ok(())
                        }
                    }
                    .map_err(BusError::Spi)?;
                }
                Ok(())
            },
        );
        critical_section::with(|cs| *SLOT.borrow_ref_mut(cs) = Some(spi));
        SPI2.radio_release();
        r
    }
}

/// BUSY or IRQ as a pin a future polls: high or low now, and a wait that
/// is pending until the level is there.
pub struct Polled(pub Input<'static>);

impl PinErrorType for Polled {
    type Error = core::convert::Infallible;
}

impl InputPin for Polled {
    fn is_high(&mut self) -> Result<bool, Self::Error> {
        Ok(self.0.is_high())
    }
    fn is_low(&mut self) -> Result<bool, Self::Error> {
        Ok(self.0.is_low())
    }
}

impl Wait for Polled {
    async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
        while self.0.is_low() {
            YieldOnce::new().await;
        }
        Ok(())
    }
    async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
        while self.0.is_high() {
            YieldOnce::new().await;
        }
        Ok(())
    }
    async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
        self.wait_for_low().await?;
        self.wait_for_high().await
    }
    async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
        self.wait_for_high().await?;
        self.wait_for_low().await
    }
    async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
        let was = self.0.is_high();
        while self.0.is_high() == was {
            YieldOnce::new().await;
        }
        Ok(())
    }
}

/// The M9's LR1110 driver.
pub type Radio = Lr11xx<RadioSpi, Output<'static>, Polled, Polled>;

/// What the probe found.
pub enum Said {
    /// Up on the default PHY, parked in standby.
    Ok(Version),
    /// The step that failed and the driver's error.
    Failed { step: &'static str, err: String },
}

/// What the task hands the loop.
#[derive(Default)]
pub struct Outbox {
    said: Option<Said>,
    /// Status reads the bench's poll made, and the ones that failed.
    pub polls: u32,
    pub poll_errors: u32,
}

/// The radio's task on core 0, polled once a turn.
pub struct RadioTask {
    fut: Option<Pin<Box<dyn Future<Output = ()>>>>,
    out: Rc<RefCell<Outbox>>,
}

impl RadioTask {
    /// The probe over `radio`; then, with `poll_every_ms`, a status read
    /// that often (the bench image's load for the `[SPI]` numbers).
    pub fn new(radio: Radio, poll_every_ms: Option<u64>) -> Self {
        let out = Rc::new(RefCell::new(Outbox::default()));
        let fut: Pin<Box<dyn Future<Output = ()>>> =
            Box::pin(task(radio, poll_every_ms, out.clone()));
        Self {
            fut: Some(fut),
            out,
        }
    }

    /// Poll the task once; `true` while it runs.
    pub fn poll(&mut self) -> bool {
        let Some(fut) = self.fut.as_mut() else {
            return false;
        };
        if poll_once(fut.as_mut()).is_ready() {
            self.fut = None;
            return false;
        }
        true
    }

    /// What the probe found, once.
    pub fn take_said(&self) -> Option<Said> {
        self.out.borrow_mut().said.take()
    }

    /// The bench poll's counts.
    pub fn polls(&self) -> (u32, u32) {
        let out = self.out.borrow();
        (out.polls, out.poll_errors)
    }
}

/// The probe's steps, each named for `[LORA] init failed step=`.
async fn probe(radio: &mut Radio) -> Result<Version, (&'static str, leviculum_lr11xx::Error)> {
    radio.reset().await.map_err(|e| ("reset", e))?;
    // GetVersion first, then the board's facts: regulator, RF switch,
    // TCXO, LF clock, calibration (`Lr11xx::init_radio`).
    let version = radio.init_radio().await.map_err(|e| ("init", e))?;
    radio
        .configure_lora(
            PHY.frequency_hz,
            PHY.sf,
            BW_CODE,
            PHY.cr_denom - 4,
            leviculum_core::rnode::DEFAULT_TX_POWER_DBM,
            leviculum_core::rnode::derive_preamble_symbols(PHY.sf, PHY.cr_denom, PHY.bandwidth_hz),
        )
        .await
        .map_err(|e| ("configure", e))?;
    radio.set_standby_rc().await.map_err(|e| ("standby", e))?;
    Ok(version)
}

async fn task(mut radio: Radio, poll_every_ms: Option<u64>, out: Rc<RefCell<Outbox>>) {
    let said = match probe(&mut radio).await {
        Ok(version) => Said::Ok(version),
        Err((step, err)) => {
            out.borrow_mut().said = Some(Said::Failed {
                step,
                err: alloc::format!("{err:?}"),
            });
            return;
        }
    };
    out.borrow_mut().said = Some(said);
    let Some(every) = poll_every_ms else {
        return;
    };
    loop {
        embassy_time::Timer::after_millis(every).await;
        let r = radio.get_status().await;
        let mut out = out.borrow_mut();
        out.polls = out.polls.wrapping_add(1);
        if r.is_err() {
            out.poll_errors = out.poll_errors.wrapping_add(1);
        }
    }
}
