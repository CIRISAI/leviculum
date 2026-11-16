//! Firmware entry point for Heltec Mesh Node T114
//!
//! Runs a Reticulum transport node with three interfaces:
//! - Interface 0: USB CDC-ACM serial (HDLC framing) to host
//! - Interface 1: SX1262 LoRa radio
//! - Interface 2: BLE peripheral (Columba v2.2 protocol)
//!
//! The transport engine routes packets between all interfaces. The node
//! loop is the shared one (`leviculum_nrf::node_loop`, #469 step 7); this
//! file is the T114's wiring between its calls.

#![no_std]
#![no_main]

extern crate alloc;

use embassy_executor::Spawner;

use leviculum_nrf::boards::t114;
use leviculum_nrf::node_loop;
use leviculum_nrf::{info, log_critical};

/// The T114's facts the shared skeleton reads.
const BOARD: node_loop::Board = node_loop::Board {
    config: &t114::CONFIG,
    iface_names: &t114::IFACE_NAMES,
    gnss_baud: t114::GNSS_BAUD,
};

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // First call on purpose: `enter` reads the previous boot's record
    // before `embassy_nrf::init`, and the post-mortems before the first
    // log line of this boot.
    let (early, mut p) = node_loop::enter();

    leviculum_nrf::set_panic_led(
        t114::PANIC_LED_PORT,
        t114::PANIC_LED_PIN,
        t114::PANIC_LED_ACTIVE_LOW,
    );
    // A button held through the boot is the factory reset (order 1511):
    // watched here, before any persisted page is read and before the
    // SoftDevice, so the erase is a direct one and nothing below reads a
    // page it is about to lose. Returns at once when nothing is held.
    leviculum_nrf::power::factory_reset_at_boot(
        p.P1_10.reborrow(),
        &t114::POWER,
        p.NVMC.reborrow(),
        &t114::CONFIG,
    )
    .await;
    // A block that takes `early` in, so the borrow below is of the block's
    // own binding and its storage ends with the block. `early` carries the
    // previous boot's log tail (3 KiB); a local `main` has borrowed keeps
    // its slot in the main task's static future across every later await,
    // which is stack the board no longer has (+2200 B of .bss, measured
    // with `-Zprint-type-sizes` in order 1085 before this block).
    let up = {
        // Not redundant, whatever the lint says: the move is the point.
        #[allow(clippy::redundant_locals)]
        let early = early;
        let up = node_loop::pre_usb(&spawner, &BOARD, &early, p.NVMC.reborrow(), p.USBD);

        log_critical!("leviculum T114 booting");
        node_loop::boot_banner(early, &up, &BOARD);
        up
    };

    // VEXT (P0.21) carries BOTH the TFT chain and the L76K receiver, so
    // it belongs to neither task; the binary raises it here, as the
    // references do at board level (Meshtastic main.cpp:420-422 "turn on
    // the display power"; RNode setup()). Raised as early as the
    // peripherals exist so its 1 s warmup overlaps the rest of init
    // instead of delaying a consumer.
    leviculum_nrf::vext::raise::<t114::VextEnable>(p.P0_21);
    let led = t114::led(p.P1_03);

    let built = node_loop::build_node(&spawner, &BOARD, p.NVMC, &up);

    // No QSPI on this board: `CONFIG.qspi_part` is `None`, the six pins
    // are never configured, and there is no `[STG] qspi-init` stage to
    // hang in. Said out loud because a capture with no store line at all
    // would leave the reader guessing which of the two it is looking at —
    // a part that did not answer, or a board that has none. The evidence
    // is in `boards/t114.rs` (Codeberg #384).
    leviculum_nrf::log::log_fmt_critical(
        "[QSPI] ",
        format_args!("NONE board=t114 reason=not-fitted-see-boards-t114-rs"),
    );

    // LoRa (SPIM2. SPIM3 has a MISO read bug on T114)
    log_critical!("[STG] lora-init");
    let lora = leviculum_nrf::lora::init(
        p.SPI2,
        p.P0_19.into(),
        p.P0_22.into(),
        p.P0_23.into(),
        p.P0_24.into(),
        p.P0_25.into(),
        p.P0_17.into(),
        p.P0_20.into(),
        leviculum_nrf::radio::FrontEnd {
            tcxo_voltage_reg: t114::CONFIG.lora_tcxo_voltage_reg,
            // No host-driven RX enable: DIO2 owns the whole antenna switch on
            // this board (`boards/t114.rs`).
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

    // ST7789 status display — blind-driven (the panel is write-only, no
    // probe possible; see leviculum_nrf::st7789 module docs). Safe and
    // default-on for panel-less boards, so this single UF2 serves both
    // populations. The task owns the VEXT/VTFT power rails. The cfg only
    // exists because the workspace-wide clippy run also checks this bin
    // under the rak4631 feature set; the real t114 build always has it.
    #[cfg(feature = "bsp-t114")]
    {
        log_critical!("[STG] display-spawn");
        leviculum_nrf::st7789::init(
            &spawner,
            t114::tft_wiring(
                p.SPI3, p.P1_08, // sck
                p.P1_09, // mosi
                p.P0_11, // cs
                p.P0_12, // dc
                p.P0_02, // rst
                p.P0_03, // vtft
                p.P0_15, // leda
            ),
            ble.identity_hash,
        );
        node_loop::log_task_spawned("display", Some(t114::DISPLAY_MODULE));
    }
    // The user button on P1.10, active low (`boards/t114.rs`, Meshtastic's
    // `PIN_BUTTON1 (32 + 10)`): the gestures of every board (#459). The
    // task prints its own `[BTN] init` line.
    leviculum_nrf::button::init(
        &spawner,
        leviculum_nrf::button::ButtonWiring {
            pin: p.P1_10.into(),
            power: &t114::POWER,
            board: "t114",
        },
    );

    // Quectel L76K on UARTE0 (#69). Pin naming is from the MCU's side:
    // the MCU receives on P1.05 and transmits on P1.07. Beware the
    // Meshtastic reference (variants/nrf52840/heltec_mesh_node_t114/
    // variant.h): the comments on GPS_TX_PIN/GPS_RX_PIN (lines 177/178)
    // describe the opposite of what the code does — effective are lines
    // 182/183, `PIN_SERIAL1_RX = GPS_RX_PIN` (P1.05) as the CPU's RX and
    // `PIN_SERIAL1_TX = GPS_TX_PIN` (P1.07) as the CPU's TX. Follow the
    // code, not the comments. P1.02 is the standby control the driver
    // holds high, P1.04 the unused PPS. Power comes from the VEXT rail
    // raised above.
    #[cfg(feature = "gnss")]
    {
        leviculum_nrf::gnss::init(
            &spawner,
            leviculum_nrf::gnss::GnssWiring {
                uarte: p.UARTE0,
                timer: p.TIMER1,
                ppi_a: p.PPI_CH0,
                ppi_b: p.PPI_CH1,
                rx: p.P1_05.into(),
                tx: p.P1_07.into(),
                pps: Some(p.P1_04.into()),
                standby: Some(p.P1_02.into()),
                // The L76K's supply is the VEXT rail this binary raised,
                // not a switch of the receiver's own.
                power_enable: None,
                wake: None,
                module: leviculum_nrf::gnss::ModuleKind::QuectelL76k,
            },
        );
        node_loop::log_task_spawned("gnss", Some(t114::GNSS_MODULE));
    }

    // Battery sense on AIN2 (P0.04) through the 100/490 divider the
    // T114's `ADC_MULTIPLIER` of 4.916 encodes, with P0.06 as the
    // divider's enable — the pin the RAK does not have, held high only
    // for the duration of a sample so 490 kΩ does not sit across the
    // pack between them (`boards/t114.rs`).
    #[cfg(feature = "battery")]
    {
        leviculum_nrf::battery::init(
            &spawner,
            p.SAADC,
            p.P0_04,
            Some(leviculum_nrf::battery::DividerEnable {
                pin: p.P0_06.into(),
                active: t114::ADC_CTRL_ACTIVE,
            }),
            leviculum_nrf::battery::BatteryScale::for_board(t114::ADC_MULTIPLIER),
        );
        node_loop::log_task_spawned("battery", None);
    }

    node_loop::log_post_init();

    // Boot blink, then the LED is the feedback grammar's (order 892): a
    // short press blinks the battery, a held button blips at 2 s, a
    // message for this board blinks once. Active low (`boards/t114.rs`).
    // The board has no notification LED, so `notify` plays here too. The
    // blink is the player's, on a timer (order 1055): a 12 M `nop` spin
    // here held the executor, and with it the USB task, for 2093 ms. The
    // player stamps `[BOOT_TRACE] stage=blink_done` when it ends.
    leviculum_nrf::feedback::spawn_led_with_boot_blink(
        &spawner,
        led,
        false,
        leviculum_nrf::feedback::Sink::Led,
    );

    node_loop::run(&spawner, &BOARD, built, up, lora_channels, ble).await;
}
