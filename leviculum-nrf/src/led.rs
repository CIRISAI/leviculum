//! Activity blinks on the two on-module LEDs of the RAK4631.
//!
//! Hardware:
//! - LED1 (green, P1.03)  — TX activity indicator
//! - LED2 (blue,  P1.04)  — RX activity indicator
//!
//! Both are driven from `embassy_sync::signal::Signal`s in
//! `leviculum_firmware::board_state`. The LoRa task raises a signal at every successful
//! TX-done / RX-frame-reassembly, and the LED's player converts each
//! signal into an ~80 ms pulse with a forced 30 ms off period. The off
//! window matters: a burst of completions inside one 80 ms span would
//! otherwise look like a single longer flash and the user couldn't count
//! the events.
//!
//! Since order 892 the two LEDs are also the Pocket V2's feedback sinks
//! (`crate::feedback`): LED1 is `sink=led`, LED2 `sink=notify`. The
//! player pulses on activity while no pattern plays, and a pattern takes
//! the LED for its length.
//!
//! The green LED opens with the boot blink every board plays (D3 of order
//! 1085, a timer and not a spin, `crate::feedback::led_task`); its TX
//! pulses start once it is over.
//!
//! Module is gated on the same `display` feature as the rest of the
//! baseboard tasks, the Pocket image's feature: the activity signals it
//! waits on are (`leviculum_firmware::board_state`).

use embassy_executor::Spawner;
use embassy_nrf::gpio::Output;

use crate::feedback::{spawn_activity_led_with_boot_blink, spawn_led, Sink};

/// Convenience wrapper invoked from the bin file. Both LEDs are active
/// high (`boards/rak4631.rs`).
pub fn init(spawner: &Spawner, tx_led: Output<'static>, rx_led: Output<'static>) {
    crate::log::log_fmt("[LED] ", format_args!("tx_blink_task alive"));
    spawn_activity_led_with_boot_blink(
        spawner,
        tx_led,
        true,
        Sink::Led,
        &leviculum_firmware::board_state::LORA_TX_FLASH,
    );
    crate::log::log_fmt("[LED] ", format_args!("rx_blink_task alive"));
    spawn_led(
        spawner,
        rx_led,
        true,
        Sink::Notify,
        Some(&leviculum_firmware::board_state::LORA_RX_FLASH),
    );
}
