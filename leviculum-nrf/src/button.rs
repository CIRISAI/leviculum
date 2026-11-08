//! The user button, on every board that has one (Codeberg #459, the
//! gestures of `docs/src/concepts/one-button.md`): the GPIO half.
//!
//! What a press means and what it does is the orchestration's
//! ([`leviculum_firmware::button`], since #469 976): the gate, the
//! `BTN_PRESS` and `BTN_HOLD` lines, the display wake, the feedback and
//! the double press's signal to the node loop. This file configures the
//! pin with its pull, writes the bring-up line, and hands the
//! orchestration a
//! [`ButtonInput`](leviculum_firmware::seams::ButtonInput),
//! [`NrfButton`](crate::button::NrfButton),
//! that folds the wiring into a pressed level.
//!
//! A very long press switches the board off (`crate::power`, order 1511):
//! the task waits on the press handling and on its `POWER_OFF` signal, and
//! the second ends the board's run.
//!
//! | Board | Pin | Wiring |
//! |---|---|---|
//! | WisMesh Pocket V2 | P0.09 (NFC1, freed by the RAK bootloader's UICR patch) | active low, internal pull-up |
//! | T1000-E | P0.06 | active high, internal pull-down (`boards/t1000e.rs`) |
//! | Heltec T114 | P1.10 (`boards/t114.rs`) | active low, internal pull-up |
//! | Solar Node | P1.01, the XIAO's program button (`boards/solarnode.rs`) | active low, internal pull-up |

use embassy_executor::Spawner;
use embassy_nrf::gpio::{AnyPin, Input, Pin, Pull};
use embassy_nrf::Peri;
use leviculum_announce_policy::BUTTON_DEBOUNCE_MS;
use leviculum_firmware::clock::EmbassyClock;
use leviculum_firmware::seams::ButtonInput;

/// How a board's button is wired.
pub struct ButtonWiring {
    /// The pin.
    pub pin: Peri<'static, AnyPin>,
    /// Which level a press drives, the LED and the pins the board drives
    /// before it switches off (`crate::power`). A press pulling the pin
    /// high is the T1000-E's; the internal pull goes the other way.
    pub power: &'static crate::power::PowerFacts,
    /// The `board=` token of the `BTN_PRESS` and `BTN_HOLD` lines.
    pub board: &'static str,
}

#[embassy_executor::task]
pub async fn button_task(wiring: ButtonWiring) {
    let ButtonWiring { pin, power, board } = wiring;
    let active_high = power.button_active_high;
    let (port, pin_no) = (pin.port() as u8, pin.pin());
    let pull = if active_high { Pull::Down } else { Pull::Up };
    let input = Input::new(pin, pull);
    // The bring-up line goes through the gate-free path: the task starts
    // before the host attaches, and a gated line written then is dropped,
    // not delayed (Codeberg #296).
    leviculum_firmware::log::log_fmt_critical(
        "[BTN] ",
        format_args!(
            "init pin=P{}.{:02} pull={} active={} debounce={}ms",
            port,
            pin_no,
            if active_high { "down" } else { "up" },
            if active_high { "high" } else { "low" },
            BUTTON_DEBOUNCE_MS
        ),
    );
    let handling = async {
        leviculum_firmware::button::run(NrfButton { input, active_high }, EmbassyClock, board).await
    };
    // The press handling never ends; a very long press's release raises
    // `POWER_OFF` (order 1511). Selecting drops the handling, and with it
    // the pin's `Input`, before `switch_off` writes the pin's wake
    // configuration.
    let _ = embassy_futures::select::select(handling, leviculum_firmware::button::POWER_OFF.wait())
        .await;
    leviculum_firmware::log::log_fmt_critical(
        "",
        format_args!(
            "POWER_OFF board={} wake=button pin=P{}.{:02}",
            board, port, pin_no
        ),
    );
    // Let the `power-off` figure the 8 s mark started finish, and the line
    // above reach a host still attached.
    embassy_time::Timer::after_millis(u64::from(leviculum_feedback::power_off().total_ms())).await;
    crate::power::switch_off((port, pin_no), power)
}

/// The board's button as the orchestration reads it: the GPIO input and
/// which level a press drives.
pub struct NrfButton {
    input: Input<'static>,
    active_high: bool,
}

impl ButtonInput for NrfButton {
    fn wait_for_edge(&mut self) -> impl core::future::Future<Output = ()> {
        self.input.wait_for_any_edge()
    }

    fn is_pressed(&self) -> bool {
        self.input.is_high() == self.active_high
    }
}

/// Spawn the button task for a board that has one.
pub fn init(spawner: &Spawner, wiring: ButtonWiring) {
    spawner.must_spawn(button_task(wiring));
}
