//! The board's off switch and its factory reset, both on the user button
//! (order 1511, `docs/src/concepts/one-button.md`).
//!
//! **Off.** A press held to 8 s plays `power-off` at the mark, and its
//! release raises [`leviculum_firmware::button::POWER_OFF`]. The button task
//! (`crate::button`) then waits out the figure and calls [`switch_off`]:
//! the board's [`OffPin`]s go to their off level (the T114's panel rail and
//! backlight and the VEXT rail under it, the Pocket's 3V3-S rail that feeds
//! its OLED and GNSS, the LEDs), every pin's sense is cleared but the
//! button's, the button senses its pressed level, and the chip enters
//! System OFF. GPIO levels and sense survive System OFF; nothing else does.
//! A press wakes it into a normal boot, whose banner says
//! `WAKE reason=button` (`leviculum_boot_trace::WakeReason`).
//!
//! The radio is not put to sleep first: the SX1262 or LR11xx stays in
//! whatever state the LoRa task left it, so a board without a switch on
//! the radio's supply (the T114) keeps the radio's receive current in
//! System OFF. That is the order after this one.
//!
//! **Factory reset.** [`factory_reset_at_boot`] runs before the persisted
//! pages are read and before the SoftDevice exists. A button found pressed
//! is watched by [`leviculum_announce_policy::BootHold`]: held for
//! [`leviculum_announce_policy::FACTORY_RESET_HOLD_MS`], the LED plays
//! `reset-warning`, and if the button is still held when it ends, the
//! identity, radio and settings pages are erased. The boot then goes on
//! with a fresh identity, which announces on the ordinary boot schedule.
//! Letting go at any moment before the wipe keeps everything. The LED is
//! driven through its registers here, as the panic blink does: the
//! players do not exist yet this early.

use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};

use embassy_nrf::gpio::{Input, Pin, Pull};
use embassy_nrf::{peripherals, Peri};
use embassy_time::{Instant, Timer};
use embedded_storage::nor_flash::NorFlash;
use leviculum_announce_policy::{BootHold, BootHoldStep};
use leviculum_boot_trace::postmortem::{
    led_registers, pin_cnf_addr, wake_pin_cnf, PIN_CNF_SENSE_MASK,
};
use leviculum_feedback::{Phase, Playback};

use crate::boards::BoardConfig;

/// One pin the board drives to a fixed level before System OFF, so the
/// part behind it is unpowered or dark while the board is off. The level
/// is held through System OFF and replaced by the next boot's own setup.
#[derive(Debug, Clone, Copy)]
pub struct OffPin {
    /// GPIO port, 0 or 1.
    pub port: u8,
    /// Pin within the port.
    pub pin: u8,
    /// `true` drives it high, `false` low.
    pub high: bool,
}

/// What the off switch and the factory reset need to know of a board.
#[derive(Debug, Clone, Copy)]
pub struct PowerFacts {
    /// `true` when a press pulls the button high (the T1000-E).
    pub button_active_high: bool,
    /// The LED the boot-time warning plays on: port, pin, active low.
    pub led: (u8, u8, bool),
    /// The pins [`switch_off`] drives before System OFF.
    pub off_pins: &'static [OffPin],
}

/// The LED's register-level blink: on or off.
fn led_set(led: (u8, u8, bool), on: bool) {
    let regs = led_registers(led.0, led.1, led.2);
    // SAFETY: GPIO DIRSET/OUTSET/OUTCLR are write-one-to-act registers
    // that touch only the bits written; the pin is this board's LED, which
    // no task owns yet (factory reset) or which nothing will drive again
    // (off).
    unsafe {
        core::ptr::write_volatile(regs.dirset as *mut u32, regs.mask);
        let reg = if on { regs.on } else { regs.off };
        core::ptr::write_volatile(reg as *mut u32, regs.mask);
    }
}

/// The outcome of the boot-time hold, for the banner: 0 nothing held,
/// 1 aborted, 2 wiped, 3 the wipe failed.
static RESET_OUTCOME: AtomicU8 = AtomicU8::new(0);
/// How long the aborted hold lasted.
static RESET_HELD_MS: AtomicU32 = AtomicU32::new(0);

/// The pages a factory reset erases: the identity, the radio profile, and
/// the settings page (telemetry target, fixed position, media profile,
/// name, management allow-list, propagation and position settings). The
/// record log (other people's messages), the boot record and the calendar
/// checkpoint stay: none of them is the user's setting or identity.
fn reset_pages(config: &BoardConfig) -> [u32; 3] {
    [
        config.identity_flash_page,
        config.radio_config_flash_page,
        config.telemetry_flash_page,
    ]
}

/// Watch a button held at boot and wipe the board if it is held through
/// the warning. Call before any persisted page is read and before the
/// SoftDevice is enabled (the erase is a direct NVMC one). Returns at once
/// when the button is not pressed.
pub async fn factory_reset_at_boot<P: Pin>(
    button: Peri<'_, P>,
    facts: &PowerFacts,
    nvmc: Peri<'_, peripherals::NVMC>,
    config: &BoardConfig,
) {
    let pull = if facts.button_active_high {
        Pull::Down
    } else {
        Pull::Up
    };
    let input = Input::new(button, pull);
    // Let the pull settle before the first look.
    Timer::after_millis(1).await;
    let pressed = || input.is_high() == facts.button_active_high;
    if !pressed() {
        return;
    }
    let warning = leviculum_feedback::reset_warning();
    let start = Instant::now().as_millis();
    let mut hold = BootHold::new(start, u64::from(warning.total_ms()));
    let mut playing: Option<Playback> = None;
    loop {
        let now = Instant::now().as_millis();
        match hold.step(now, pressed()) {
            BootHoldStep::Hold { .. } => {}
            BootHoldStep::Warn { .. } => {
                crate::log::log_fmt_critical(
                    "",
                    format_args!("FEEDBACK pattern={} sink=led", warning.name),
                );
                playing = Some(Playback::new(warning, now));
            }
            BootHoldStep::Abort { held_ms } => {
                led_set(facts.led, false);
                RESET_HELD_MS.store(held_ms.min(u64::from(u32::MAX)) as u32, Ordering::Relaxed);
                RESET_OUTCOME.store(1, Ordering::Relaxed);
                return;
            }
            BootHoldStep::Wipe => break,
        }
        if let Some(p) = playing {
            led_set(facts.led, matches!(p.phase(now), Phase::On { .. }));
        }
        Timer::after_millis(10).await;
    }
    led_set(facts.led, false);
    let mut flash = embassy_nrf::nvmc::Nvmc::new(nvmc);
    let mut ok = true;
    for page in reset_pages(config) {
        ok &= flash
            .erase(page, page + embassy_nrf::nvmc::PAGE_SIZE as u32)
            .is_ok();
    }
    RESET_OUTCOME.store(if ok { 2 } else { 3 }, Ordering::Relaxed);
    // Wait for the release, so the button task does not start on a held
    // button and count it toward the 8 s off. A minute at most: a stuck
    // button must not keep the board from booting.
    let deadline = Instant::now().as_millis() + 60_000;
    while pressed() && Instant::now().as_millis() < deadline {
        Timer::after_millis(10).await;
    }
}

/// The banner lines of this boot's wake and of a factory reset, if the
/// button was held through the boot. `reset_reason` is the RESETREAS
/// value `boot_trace::capture` read.
pub fn log_banner(reset_reason: u32) {
    crate::log::log_fmt_critical(
        "",
        format_args!(
            "WAKE reason={}",
            leviculum_boot_trace::ResetReasons::decode(reset_reason)
                .wake()
                .as_str()
        ),
    );
    match RESET_OUTCOME.load(Ordering::Relaxed) {
        1 => crate::log::log_fmt_critical(
            "",
            format_args!(
                "FACTORY_RESET done=0 reason=released held_ms={}",
                RESET_HELD_MS.load(Ordering::Relaxed)
            ),
        ),
        2 => crate::log::log_fmt_critical(
            "",
            format_args!("FACTORY_RESET done=1 pages=identity,radio,settings"),
        ),
        3 => crate::log::log_fmt_critical(
            "",
            format_args!("FACTORY_RESET done=0 reason=erase-failed"),
        ),
        _ => {}
    }
}

/// Switch the board off: the off pins, the button as the only wake source,
/// System OFF. `button` is the button's port and pin. Never returns; the
/// next thing the chip does is boot.
pub fn switch_off(button: (u8, u8), facts: &PowerFacts) -> ! {
    for off in facts.off_pins {
        let regs = led_registers(off.port, off.pin, !off.high);
        // SAFETY: as in `led_set`: write-one-to-act registers on a pin the
        // board names as this one's to drive, at the end of the board's run.
        unsafe {
            core::ptr::write_volatile(regs.dirset as *mut u32, regs.mask);
            core::ptr::write_volatile(regs.on as *mut u32, regs.mask);
        }
    }
    // SAFETY: GPIO is not a SoftDevice-protected peripheral, and nothing
    // else runs after this function. Every pin's sense is cleared (any
    // pin left sensing would wake the board too, the LoRa DIO1 among
    // them), the button's is set, and both ports' latches are cleared so
    // DETECT is low when System OFF is entered: a DETECT already high
    // would wake the chip at once.
    unsafe {
        for port in 0..2u8 {
            let pins = if port == 0 { 32 } else { 16 };
            for pin in 0..pins {
                let addr = pin_cnf_addr(port, pin) as *mut u32;
                let cnf = core::ptr::read_volatile(addr);
                core::ptr::write_volatile(addr, cnf & !PIN_CNF_SENSE_MASK);
            }
        }
        core::ptr::write_volatile(
            pin_cnf_addr(button.0, button.1) as *mut u32,
            wake_pin_cnf(facts.button_active_high),
        );
        for base in [
            leviculum_boot_trace::postmortem::P0_BASE,
            leviculum_boot_trace::postmortem::P1_BASE,
        ] {
            // DETECTMODE (+0x524) back to the default, LATCH (+0x520)
            // cleared by writing its own bits back.
            core::ptr::write_volatile((base + 0x524) as *mut u32, 0);
            let latch = core::ptr::read_volatile((base + 0x520) as *const u32);
            core::ptr::write_volatile((base + 0x520) as *mut u32, latch);
        }
    }
    if crate::usb::softdevice_enabled() {
        // SAFETY: the SoftDevice's own System OFF call, legal in every
        // state it is enabled in; it does not return on success.
        unsafe {
            nrf_softdevice_s140::sd_power_system_off();
        }
    }
    // SAFETY: before `Softdevice::enable` POWER is this core's: write
    // SYSTEMOFF (0x40000500). After it, the call above has not returned.
    unsafe {
        core::ptr::write_volatile(0x4000_0500 as *mut u32, 1);
    }
    loop {
        cortex_m::asm::wfe();
    }
}
