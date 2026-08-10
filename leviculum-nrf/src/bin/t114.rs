//! Firmware entry point for Heltec Mesh Node T114
//!
//! Runs a Reticulum transport node with three interfaces:
//! - Interface 0: USB CDC-ACM serial (HDLC framing) to host
//! - Interface 1: SX1262 LoRa radio
//! - Interface 2: BLE peripheral (Columba v2.2 protocol)
//!
//! The transport engine routes packets between all interfaces.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use embassy_executor::Spawner;
use embassy_futures::select::{select4, Either4};
use embassy_nrf::gpio::Level;
use embassy_nrf::spim;
use embassy_time::{Duration, Instant, Timer};

use leviculum_core::embedded_storage::EmbeddedStorage;
use leviculum_core::ifac::IfacConfig;
use leviculum_core::node::NodeCoreBuilder;
use leviculum_core::traits::Interface;
use leviculum_core::transport::dispatch_actions;
use leviculum_core::InterfaceId;

use leviculum_nrf::ble::BleInterface;
use leviculum_nrf::boards::t114;
use leviculum_nrf::clock::EmbassyClock;
use leviculum_nrf::interface::EmbeddedInterface;
use leviculum_nrf::lora::LoRaInterface;
use leviculum_nrf::{info, init_heap, log_critical};

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut config = embassy_nrf::config::Config::default();
    config.hfclk_source = embassy_nrf::config::HfclkSource::ExternalXtal;
    config.gpiote_interrupt_priority = embassy_nrf::interrupt::Priority::P2;
    config.time_interrupt_priority = embassy_nrf::interrupt::Priority::P2;
    let p = embassy_nrf::init(config);

    init_heap();
    leviculum_nrf::init_tracing();

    // Boot diagnostics from the previous run. Read before the first log
    // call of this boot: `take_persistent_log` must snapshot the PREVIOUS
    // boot's tail before we start writing this boot's into the same ring.
    let hardfault_pm = leviculum_nrf::take_hardfault_postmortem();
    let panic_pm = leviculum_nrf::take_panic_postmortem();
    let persistent_log = leviculum_nrf::log::take_persistent_log();

    // SAFETY: called once before any complex work or concurrent tasks.
    // Under flip-link the painted region is `[_stack_end, SP-1KiB)` at
    // the BOTTOM of RAM — disjoint from `.uninit`, so the post-mortem
    // reads above are no longer order-critical against it.
    unsafe {
        leviculum_nrf::paint_stack();
    }

    leviculum_nrf::set_panic_led(
        t114::PANIC_LED_PORT,
        t114::PANIC_LED_PIN,
        t114::PANIC_LED_ACTIVE_LOW,
    );
    leviculum_nrf::set_irq_priorities();
    let vbus = leviculum_nrf::init_vbus();
    let serial = leviculum_nrf::usb::init(&spawner, p.USBD, vbus, &t114::CONFIG);

    log_critical!("leviculum T114 booting");
    log_critical!(
        "[FW_BUILD] git_sha={} dirty={}",
        env!("LEVICULUM_GIT_SHA"),
        env!("LEVICULUM_GIT_DIRTY")
    );
    leviculum_nrf::log_stack("boot");
    log_critical!("[PANIC_COUNT] total={}", leviculum_nrf::panic_count());
    // Boot-loop instrumentation: what kind of reset got us here? Must
    // stay ahead of ble::init — after Softdevice::enable the POWER
    // registers belong to the SD.
    leviculum_nrf::log_reset_reason();
    leviculum_nrf::log_irq_priorities();

    if let Some(pm) = hardfault_pm {
        log_critical!(
            "[HARDFAULT_PMRT] pc=0x{:08x} lr=0x{:08x} r0=0x{:08x} r1=0x{:08x} r2=0x{:08x} r3=0x{:08x} r12=0x{:08x} xpsr=0x{:08x}",
            pm.pc, pm.lr, pm.r0, pm.r1, pm.r2, pm.r3, pm.r12, pm.xpsr
        );
    }
    if let Some(pm) = panic_pm {
        let msg = core::str::from_utf8(&pm.bytes[..pm.len]).unwrap_or("<non-utf8>");
        // EscapeCtrl keeps this ONE line: the SD fault panic embeds a
        // raw '\n' after the source location, which split the line and
        // cost us the PC/PREGION tail in capture.
        log_critical!(
            "[PANIC_PMRT] len={} total={} msg={}",
            pm.len,
            pm.total,
            leviculum_nrf::log::EscapeCtrl(msg)
        );
        // Tail again on its own short line — PC/PREGION ride at the END
        // of SD fault messages, and a short line survives any reader-
        // side line-length cap.
        let mut tail_start = msg.len().saturating_sub(96);
        while !msg.is_char_boundary(tail_start) {
            tail_start += 1;
        }
        log_critical!(
            "[PANIC_PMRT_TAIL] {}",
            leviculum_nrf::log::EscapeCtrl(&msg[tail_start..])
        );
    }
    if let Some(snap) = persistent_log {
        let mut start = 0usize;
        while start < snap.len {
            let end = snap.bytes[start..snap.len]
                .iter()
                .position(|&b| b == b'\n')
                .map(|p| start + p + 1)
                .unwrap_or(snap.len);
            if start == 0 && end == snap.len {
                // single-line case
            } else if start == 0 {
                start = end;
                continue;
            }
            let raw = &snap.bytes[start..end];
            let trimmed = core::str::from_utf8(raw)
                .unwrap_or("<non-utf8>")
                .trim_end_matches(['\r', '\n']);
            if !trimmed.is_empty() {
                log_critical!("[PERSISTENT_LOG] {}", trimmed);
            }
            start = end;
        }
    }

    // VEXT (P0.21) is owned by the display task, which drives it HIGH:
    // both references power the TFT chain from that rail at boot
    // (Meshtastic main.cpp "turn on the display power"; RNode setup()).
    let mut led = t114::led(p.P1_03);

    let rng = leviculum_nrf::rng::RawHwRng::new();

    // Load or generate persistent identity from internal flash
    let mut id_store = leviculum_nrf::flash::NvmcIdentityStore::new(
        embassy_nrf::nvmc::Nvmc::new(p.NVMC),
        t114::CONFIG.identity_flash_page,
    );

    let mut builder = NodeCoreBuilder::new()
        .enable_transport(true)
        .max_incoming_resource_size(8 * 1024)
        .max_queued_announces(32)
        .max_random_blobs(8)
        .respond_to_probes(true);

    let identity_loaded = {
        use leviculum_core::identity_store::IdentityStore;
        if let Ok(Some(identity)) = id_store.load() {
            info!("Identity loaded from flash");
            builder = builder.identity(identity);
            true
        } else {
            info!("No identity in flash, generating new");
            false
        }
    };

    let mut node = builder.build_boxed(rng, EmbassyClock, EmbeddedStorage::new());

    let initial_path_len = node.path_count();
    info!("[BOOT] path_table_initial_len={}", initial_path_len);
    spawner.must_spawn(boot_log_repeater(initial_path_len));
    spawner.must_spawn(fw_build_banner());
    spawner.must_spawn(leviculum_nrf::heap_watermark_task());
    spawner.must_spawn(leviculum_nrf::stack_watermark_task());

    if !identity_loaded {
        use leviculum_core::identity_store::IdentityStore;
        let _ = id_store.save(node.identity());
        info!("Identity saved to flash");
    }

    // Register all three interfaces
    node.set_interface_name(0, alloc::string::String::from("serial_usb"));
    node.set_interface_hw_mtu(0, 564);
    // Codeberg #117: the client-facing serial interface must be Gateway so the
    // node discovers unknown paths on behalf of the host.
    node.set_interface_mode(0, leviculum_core::InterfaceMode::Gateway);
    node.set_interface_name(1, alloc::string::String::from("lora_sx1262"));
    node.set_interface_hw_mtu(1, 255);
    node.set_interface_name(2, alloc::string::String::from("ble"));
    node.set_interface_hw_mtu(2, 564);

    let hash = node.identity().hash();
    info!(
        "LNode started -- identity: {:02X}{:02X}{:02X}{:02X}{:02X}",
        hash[0], hash[1], hash[2], hash[3], hash[4]
    );
    // Full identity hash for benchmark trace correlation
    leviculum_nrf::log::log_fmt("[IDENTITY] ", format_args!(
        "t114_node={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7],
        hash[8], hash[9], hash[10], hash[11], hash[12], hash[13], hash[14], hash[15]
    ));
    if let Some(probe_hash) = node.probe_dest_hash() {
        let ph = probe_hash.as_bytes();
        leviculum_nrf::log::log_fmt("[IDENTITY] ", format_args!(
            "t114_probe={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            ph[0], ph[1], ph[2], ph[3], ph[4], ph[5], ph[6], ph[7],
            ph[8], ph[9], ph[10], ph[11], ph[12], ph[13], ph[14], ph[15]
        ));
    }

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
        spim::Frequency::M4,
        t114::CONFIG.lora_tcxo_voltage_reg,
    )
    .await;
    info!("SX1262 ready");

    let radio_cfg = leviculum_nrf::lora::RadioConfig::eu_medium();
    let lora_channels = leviculum_nrf::lora::channels();
    spawner.must_spawn(leviculum_nrf::lora::lora_task(lora, radio_cfg));

    // BLE — full init restored. RAM ORIGIN bumped to 40K (memory.x) to give
    // Softdevice::enable headroom for our config (att_mtu=256, …).
    let identity_hash = *node.identity().hash();
    log_critical!("[STG] ble-init");
    leviculum_nrf::ble::init(
        &spawner,
        identity_hash,
        vbus,
        p.RTC0,
        p.TIMER0,
        p.TEMP,
        p.PPI_CH19,
        p.PPI_CH30,
        p.PPI_CH31,
        p.PPI_CH17,
        p.PPI_CH18,
        p.PPI_CH20,
        p.PPI_CH21,
        p.PPI_CH22,
        p.PPI_CH23,
        p.PPI_CH24,
        p.PPI_CH25,
        p.PPI_CH26,
        p.PPI_CH27,
        p.PPI_CH28,
        p.PPI_CH29,
        p.RNG,
    );
    let ble_channels = leviculum_nrf::ble::channels();
    info!("BLE ready");

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
            leviculum_nrf::st7789::TftWiring {
                spi: p.SPI3,
                sck: p.P1_08,
                mosi: p.P1_09,
                cs: p.P0_11,
                dc: p.P0_12,
                rst: p.P0_02,
                vext: p.P0_21,
                vtft: p.P0_03,
                leda: p.P0_15,
            },
            identity_hash,
        );
        info!("display task spawned (ST7789 blind-drive)");
    }

    let (hu, hf) = leviculum_nrf::heap_stats();
    info!("heap u={} f={}", hu, hf);
    leviculum_nrf::log_stack("post-init");

    // Interface adapters
    let mut serial_iface = EmbeddedInterface::new(serial.outgoing_tx);
    let mut lora_iface = LoRaInterface::new(lora_channels.outgoing_tx);
    let mut ble_iface = BleInterface::new(ble_channels.outgoing_tx);
    let ifac_configs: BTreeMap<usize, IfacConfig> = BTreeMap::new();

    // Boot blink
    led.set_level(Level::Low);
    for _ in 0..12_000_000u32 {
        cortex_m::asm::nop();
    }
    led.set_level(Level::High);

    log_critical!("[STG] main-loop");
    // Event-driven main loop, four event sources:
    // 1. Serial incoming (USB)
    // 2. LoRa incoming (radio)
    // 3. BLE incoming (defragmented Reticulum packets from phone)
    // 4. Timer deadline (protocol maintenance, announces)
    loop {
        let deadline = node
            .next_deadline()
            .map(Instant::from_millis)
            .unwrap_or(Instant::MAX);

        match select4(
            serial.incoming_rx.receive(),
            lora_channels.incoming_rx.receive(),
            ble_channels.incoming_rx.receive(),
            Timer::at(deadline),
        )
        .await
        {
            Either4::First(data) => {
                info!("SER RX {} bytes", data.len());
                let output = node.handle_packet(InterfaceId(0), &data);
                info!("SER RX -> {} actions", output.actions.len());
                let mut ifaces: [&mut dyn Interface; 3] =
                    [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                dispatch_actions(&mut ifaces, output.actions, &ifac_configs);
            }
            Either4::Second(data) => {
                let output = node.handle_packet(InterfaceId(1), &data);
                if !output.actions.is_empty() {
                    info!("LORA RX -> {} actions", output.actions.len());
                }
                let mut ifaces: [&mut dyn Interface; 3] =
                    [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                dispatch_actions(&mut ifaces, output.actions, &ifac_configs);
            }
            Either4::Third(data) => {
                info!("BLE RX {} bytes", data.len());
                let output = node.handle_packet(InterfaceId(2), &data);
                if !output.actions.is_empty() {
                    info!("BLE RX -> {} actions", output.actions.len());
                }
                let mut ifaces: [&mut dyn Interface; 3] =
                    [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                dispatch_actions(&mut ifaces, output.actions, &ifac_configs);
            }
            Either4::Fourth(()) => {
                let output = node.handle_timeout();
                if !output.actions.is_empty() {
                    info!("timeout: {} actions", output.actions.len());
                }
                let mut ifaces: [&mut dyn Interface; 3] =
                    [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                dispatch_actions(&mut ifaces, output.actions, &ifac_configs);
            }
        }
    }
}

#[embassy_executor::task]
async fn boot_log_repeater(initial_len: usize) {
    for _ in 0..6 {
        Timer::after(Duration::from_secs(10)).await;
        info!("[BOOT] path_table_initial_len={}", initial_len);
    }
}

/// Re-emit the firmware build banner periodically so a debug-serial
/// reader can verify the running git_sha at any time, not only inside the
/// short boot window. The CI auto-flash verify reads this back. Uses the
/// embassy time driver (same timing infra the other periodic tasks use);
/// no SD-reserved peripheral is touched directly.
#[embassy_executor::task]
async fn fw_build_banner() {
    loop {
        Timer::after(Duration::from_secs(5)).await;
        log_critical!(
            "[FW_BUILD] git_sha={} dirty={}",
            env!("LEVICULUM_GIT_SHA"),
            env!("LEVICULUM_GIT_DIRTY")
        );
    }
}
