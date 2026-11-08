//! Firmware entry point for RAKwireless WisMesh Pocket V2 (RAK4631 module).
//!
//! Three Reticulum interfaces:
//! - Interface 0: USB CDC-ACM serial (HDLC framing) to host
//! - Interface 1: SX1262 LoRa radio (module-internal SPI on P1.10–P1.15+P1.06)
//! - Interface 2: BLE peripheral (Columba v2.2 protocol)
//!
//! The node loop is the shared one (`leviculum_nrf::node_loop`, #469 step
//! 7, order 1572); this file is the RAK4631's wiring between its calls.
//! The shipped image is `bsp-rak4631,rak-baseboard`, the Pocket V2 with
//! its RAK19026 VC baseboard (display and button, GNSS, battery). The bare
//! module build has no product behind it any more (D5, Lew 2026-10-28):
//! its cfgs below exist because every board's feature set compiles every
//! binary (`just lint-nrf`), not because it is a target.

#![no_std]
#![no_main]

extern crate alloc;

use embassy_executor::Spawner;

use leviculum_nrf::boards::rak4631;
use leviculum_nrf::node_loop;
use leviculum_nrf::{info, log_critical};

/// The RAK4631's facts the shared skeleton reads.
const BOARD: node_loop::Board = node_loop::Board {
    config: &rak4631::CONFIG,
    iface_names: &rak4631::IFACE_NAMES,
    gnss_baud: rak4631::GNSS_BAUD,
};

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // First call on purpose: `enter` reads the previous boot's record
    // before `embassy_nrf::init`, and the post-mortems before the first
    // log line of this boot.
    let (early, mut p) = node_loop::enter();

    leviculum_nrf::set_panic_led(
        rak4631::PANIC_LED_PORT,
        rak4631::PANIC_LED_PIN,
        rak4631::PANIC_LED_ACTIVE_LOW,
    );
    // Distinct LED for HardFault, the blue one, so it reads apart from
    // the green panic LED (`boards/rak4631.rs`).
    leviculum_nrf::set_hardfault_led(
        rak4631::HARDFAULT_LED_PORT,
        rak4631::HARDFAULT_LED_PIN,
        rak4631::HARDFAULT_LED_ACTIVE_LOW,
    );
    // A button held through the boot is the factory reset (order 1511):
    // watched here, before any persisted page is read and before the
    // SoftDevice, so the erase is a direct one and nothing below reads a
    // page it is about to lose. Returns at once when nothing is held.
    leviculum_nrf::power::factory_reset_at_boot(
        p.P0_09.reborrow(),
        &rak4631::POWER,
        p.NVMC.reborrow(),
        &rak4631::CONFIG,
    )
    .await;
    // `early` taken into a block for the reason the T114's binary gives:
    // a local `main` has borrowed keeps its slot in the main task's static
    // future across every later await, and it carries the previous boot's
    // 3 KiB log tail.
    let up = {
        // Not redundant, whatever the lint says: the move is the point.
        #[allow(clippy::redundant_locals)]
        let early = early;
        let up = node_loop::pre_usb(&spawner, &BOARD, &early, p.NVMC.reborrow(), p.USBD);

        log_critical!("leviculum RAK4631 booting");
        node_loop::boot_banner(early, &up, &BOARD);
        up
    };

    // The baseboard's 3V3-S rail (OLED, GNSS, sensors) and the SX1262
    // front-end power, both held high for the life of the board; the
    // radio's must be up before `lora::init` talks to it.
    let _periph_3v3 = rak4631::periph_3v3(p.P1_02);
    let _lora_power = rak4631::lora_power(p.P1_05);
    // The two on-module LEDs: green LED1 the main sink with the TX pulses,
    // blue LED2 the notification sink with the RX pulses. Under `display`,
    // the Pocket image's feature, as `led.rs` and the activity signals are.
    #[cfg(feature = "display")]
    let led_tx = rak4631::led(p.P1_03);
    #[cfg(feature = "display")]
    let led_rx = rak4631::led_notification(p.P1_04);

    let built = node_loop::build_node(&spawner, &BOARD, p.NVMC, &up);

    // No QSPI on this module: `CONFIG.qspi_part` is `None`, the six pins
    // are never configured, and there is no `[STG] qspi-init` stage to
    // hang in. Said out loud because a capture with no store line at all
    // would leave the reader guessing which of the two it is looking at —
    // a part that did not answer, or a module that has none. RAK's own
    // board support package says "No onboard flash"; the evidence is in
    // `boards/rak4631.rs` (Codeberg #384).
    leviculum_nrf::log::log_fmt_critical(
        "[QSPI] ",
        format_args!("NONE board=rak4631 reason=no-onboard-flash-see-boards-rak4631-rs"),
    );

    // LoRa (SPIM2; same instance the T114 uses, dictated by the shared
    // lora::init signature). Pin map is RAK4631-module-internal.
    log_critical!("[STG] lora-init");
    let lora = leviculum_nrf::lora::init(
        p.SPI2,
        p.P1_11.into(), // SCK
        p.P1_12.into(), // MOSI
        p.P1_13.into(), // MISO
        p.P1_10.into(), // NSS / CS
        p.P1_06.into(), // RESET
        p.P1_14.into(), // BUSY
        p.P1_15.into(), // DIO1
        leviculum_nrf::radio::FrontEnd {
            tcxo_voltage_reg: rak4631::CONFIG.lora_tcxo_voltage_reg,
            // No host-driven RX enable: DIO2 owns the whole antenna switch, and
            // the module's P1.07 pad must not be driven (`boards/rak4631.rs`).
            rx_enable: None,
        },
    )
    .await;
    info!("SX1262 ready");

    let lora_channels = node_loop::lora_up(&spawner, &BOARD, &up, lora);
    let ble = node_loop::ble_up(
        &spawner,
        &BOARD,
        built.node(),
        &up,
        leviculum_nrf::ble_peripherals!(p),
    )
    .await;

    // The 1.3" OLED on TWISPI0 (SDA P0.13, SCL P0.14), probed at runtime
    // by its task, on the 3V3-S rail raised above.
    #[cfg(feature = "display")]
    {
        log_critical!("[STG] display-spawn");
        leviculum_nrf::display::init(&spawner, p.TWISPI0, p.P0_13, p.P0_14, ble.identity_hash);
        node_loop::log_task_spawned("display", Some(rak4631::DISPLAY_MODULE));
    }
    // The user button on P0.09, active low, on the Pocket's carrier: the
    // gestures of every board (#459). The task prints its own `[BTN] init`
    // line.
    leviculum_nrf::button::init(
        &spawner,
        leviculum_nrf::button::ButtonWiring {
            pin: p.P0_09.into(),
            power: &rak4631::POWER,
            board: "rak4631",
        },
    );

    #[cfg(feature = "gnss")]
    {
        leviculum_nrf::gnss::init(
            &spawner,
            leviculum_nrf::gnss::GnssWiring {
                uarte: p.UARTE0,
                timer: p.TIMER1,
                ppi_a: p.PPI_CH0,
                ppi_b: p.PPI_CH1,
                rx: p.P0_15.into(), // RX from ZOE-M8Q TX
                tx: p.P0_16.into(), // TX to ZOE-M8Q RX
                // No pulse line. The RAK19026 VC routes the ZOE-M8Q's
                // TIMEPULSE onto net `1PPS` and then stops: the only part
                // on it is `R39`, marked `0/NC` and therefore not fitted
                // (schematic sheet 3, U6 pin C3). Behind that gap sits IO3,
                // which is P0.21. The task used to be handed P0.17, which
                // is IO1, which carries the LIS3DH's INT1 — see
                // `boards/rak4631.rs` for the whole trail. Codeberg #394.
                pps: None,
                // Same sheet, same story: `STANDBY_GPS` reaches the
                // module's PIO13/EXTINT, but its link to IO4 is `R44`,
                // `0/NC`, unfitted. The receiver is woken through UBX
                // (CFG-PMS) because there is no wire to do it with, not
                // because the part lacks the pin.
                standby: None,
                // The ZOE-M8Q sits on the baseboard's 3V3-S rail, raised
                // above for four peripherals at once.
                power_enable: None,
                wake: None,
                module: leviculum_nrf::gnss::ModuleKind::UbloxM8,
            },
        );
        node_loop::log_task_spawned("gnss", Some(rak4631::GNSS_MODULE));
    }
    #[cfg(feature = "battery")]
    {
        // No divider-enable pin on the RAK19026 baseboard: the 1.5/2.5
        // divider sits permanently across the pack, so nothing to switch
        // (`boards/rak4631.rs`, which has no `AdcCtrl`).
        leviculum_nrf::battery::init(
            &spawner,
            p.SAADC,
            p.P0_05,
            None,
            leviculum_nrf::battery::BatteryScale::for_board(rak4631::ADC_MULTIPLIER),
        );
        node_loop::log_task_spawned("battery", None);
    }

    node_loop::log_post_init();

    // The LEDs are the feedback grammar's (order 892) and pulse on LoRa
    // activity between patterns (D4): green LED1 is `sink=led` with the TX
    // pulses and opens with the boot blink every board plays (D3, a timer,
    // stamping `[BOOT_TRACE] stage=blink_done`), blue LED2 is
    // `sink=notify` with the RX pulses. Both active high.
    #[cfg(feature = "display")]
    leviculum_nrf::led::init(&spawner, led_tx, led_rx);

    node_loop::run(&spawner, &BOARD, built, up, lora_channels, ble).await;
}
