//! Firmware entry point for RAKwireless WisMesh Pocket V2 (RAK4631 module).
//!
//! Three Reticulum interfaces:
//! - Interface 0: USB CDC-ACM serial (HDLC framing) to host
//! - Interface 1: SX1262 LoRa radio (module-internal SPI on P1.10–P1.15+P1.06)
//! - Interface 2: BLE peripheral (Columba v2.2 protocol)
//!
//! Baseboard peripherals (display, GNSS, battery telemetry) land in Phase 3.
//! Tracked under Codeberg #42.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use embassy_executor::Spawner;
use embassy_futures::select::{select, select4, Either, Either4};
use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::spim;
use embassy_time::{Duration, Instant, Timer};

use leviculum_core::embedded_storage::EmbeddedStorage;
use leviculum_core::ifac::IfacConfig;
use leviculum_core::node::NodeCoreBuilder;
use leviculum_core::traits::Interface;
use leviculum_core::transport::{dispatch_actions, Action};
use leviculum_core::InterfaceId;

use leviculum_nrf::ble::BleInterface;
use leviculum_nrf::boards::rak4631;
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

    // Read post-mortems before the first log call of this boot:
    // `take_persistent_log` must snapshot the PREVIOUS boot's tail
    // before we start writing this boot's into the same ring.
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
        rak4631::PANIC_LED_PORT,
        rak4631::PANIC_LED_PIN,
        rak4631::PANIC_LED_ACTIVE_LOW,
    );
    // Distinct LED for HardFault — blue (P1.04) so it's visually
    // distinct from the green panic LED. Diagnostic for the executor-
    // hang investigation.
    leviculum_nrf::set_hardfault_led(1, 4, false);

    // Set NVIC priorities before SoftDevice enable. S140 reserves
    // P0/P1/P4; everything else goes to P5 (RNG, USBD, TWISPI0, SAADC,
    // SPI2, UARTE0). GPIOTE + RTC1 stay P2 per embassy_nrf::config.
    leviculum_nrf::set_irq_priorities();

    let vbus = leviculum_nrf::init_vbus();
    let serial = leviculum_nrf::usb::init(&spawner, p.USBD, vbus, &rak4631::CONFIG);

    log_critical!("leviculum RAK4631 booting");
    log_critical!("[FW_BUILD] {}", leviculum_nrf::FW_BUILD_STAMP);
    log_critical!("[TIME_SOURCE] source={}", leviculum_nrf::time_source_str());
    // GNSS presence banner (#240): the settled states are emitted as
    // `state=<no-hardware|no-fix|fix>` transitions by the GNSS task;
    // this boot line marks "machinery armed, answer pending" so a log
    // tail always carries a [GNSS_PRESENCE] anchor. A replay keyed on
    // the three settled states treats it like no line at all.
    #[cfg(feature = "gnss")]
    log_critical!("[GNSS_PRESENCE] state=detecting baud=9600");
    leviculum_nrf::log_stack("boot");
    leviculum_nrf::log_panic_count();
    leviculum_nrf::log_irq_priorities();

    // Shared boot/query formatter — the same block is retrievable at any
    // later time via the debug-port query (`p` byte, postmortem_query).
    leviculum_nrf::log_postmortems(hardfault_pm.as_ref(), panic_pm.as_ref());
    // Persistent-log replay from previous boot's last ~2 KiB. Each
    // line is emitted as a `[PERSISTENT_LOG]`-prefixed critical line
    // so it lands ahead of the runtime flood. After this block, the
    // PERSISTENT_TAIL ring is implicitly cleared (the tail mirror's
    // first write of THIS boot will reset it).
    if let Some(snap) = persistent_log {
        let mut start = 0usize;
        while start < snap.len {
            let end = snap.bytes[start..snap.len]
                .iter()
                .position(|&b| b == b'\n')
                .map(|p| start + p + 1)
                .unwrap_or(snap.len);
            // skip the leading partial line (we wrap around inside lines)
            if start == 0 && end == snap.len {
                // single-line case, emit as-is
            } else if start == 0 {
                // first chunk likely a partial line — skip it
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

    // Power up baseboard peripherals (OLED, GNSS, LIS3DH, NCP5623).
    //
    // Per Meshtastic's RAK4631 init (`variants/.../variant.cpp:initVariant`
    // and `src/platform/nrf52/main-nrf52.cpp:453` which drives this LOW
    // exclusively at shutdown), HIGH = peripherals powered. The display
    // task sleeps 500 ms after creating the TWIM so the OLED's internal
    // POR has finished by the first probe.
    let _periph_3v3 = Output::new(p.P1_02, Level::High, OutputDrive::Standard);
    // SX1262 LoRa front-end power. Must be HIGH before any SPI traffic to
    // the radio. The chip itself sits on the module, not the baseboard, so
    // this is independent of the 3V3-S rail above.
    let _lora_pa = Output::new(p.P1_05, Level::High, OutputDrive::Standard);
    // Two on-module LEDs are owned by the LED tasks (under `display`) when
    // that feature is on; for the bare-module build the green LED is taken
    // by the heartbeat task as before.
    #[cfg(not(feature = "display"))]
    let led = rak4631::led(p.P1_03);
    #[cfg(feature = "display")]
    let led_tx = rak4631::led(p.P1_03);
    #[cfg(feature = "display")]
    let led_rx = rak4631::led_notification(p.P1_04);

    let rng = leviculum_nrf::rng::RawHwRng::new();

    // Load or generate persistent identity from internal flash
    let mut id_store = leviculum_nrf::flash::NvmcIdentityStore::new(
        embassy_nrf::nvmc::Nvmc::new(p.NVMC),
        rak4631::CONFIG.identity_flash_page,
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
    spawner.must_spawn(diag_mem_log());

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
        "rak_node={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7],
        hash[8], hash[9], hash[10], hash[11], hash[12], hash[13], hash[14], hash[15]
    ));
    if let Some(probe_hash) = node.probe_dest_hash() {
        let ph = probe_hash.as_bytes();
        leviculum_nrf::log::log_fmt("[IDENTITY] ", format_args!(
            "rak_probe={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            ph[0], ph[1], ph[2], ph[3], ph[4], ph[5], ph[6], ph[7],
            ph[8], ph[9], ph[10], ph[11], ph[12], ph[13], ph[14], ph[15]
        ));
    }

    // LoRa (SPIM2; same instance the T114 uses, dictated by the shared
    // lora::init signature). Pin map is RAK4631-module-internal.
    let lora = leviculum_nrf::lora::init(
        p.SPI2,
        p.P1_11.into(), // SCK
        p.P1_12.into(), // MOSI
        p.P1_13.into(), // MISO
        p.P1_10.into(), // NSS / CS
        p.P1_06.into(), // RESET
        p.P1_14.into(), // BUSY
        p.P1_15.into(), // DIO1
        spim::Frequency::M4,
        rak4631::CONFIG.lora_tcxo_voltage_reg,
    )
    .await;
    info!("SX1262 ready");

    // Radio profile: whatever a host last set and we persisted, else the
    // compiled default. A blank or corrupt page decodes to None.
    let radio_cfg = match leviculum_nrf::radio_store::load(rak4631::CONFIG.radio_config_flash_page)
        .and_then(leviculum_nrf::lora::RadioConfig::from_wire_config)
    {
        Some(cfg) => {
            leviculum_nrf::log::log_fmt(
                "[RADIO] ",
                format_args!(
                    "persisted freq={} bw={} sf={} cr={} pwr={}",
                    cfg.frequency_hz, cfg.bw_hz, cfg.sf, cfg.cr_denom, cfg.tx_power_dbm
                ),
            );
            cfg
        }
        None => {
            leviculum_nrf::log::log_fmt("[RADIO] ", format_args!("default eu_medium"));
            leviculum_nrf::lora::RadioConfig::eu_medium()
        }
    };
    let lora_channels = leviculum_nrf::lora::channels();
    spawner.must_spawn(leviculum_nrf::lora::lora_task(lora, radio_cfg));

    // BLE — same Columba v2.2 service the T114 exposes.
    let identity_hash = *node.identity().hash();
    let sd = leviculum_nrf::ble::init(
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

    // Radio-config persistence. Must come after `ble::init`: writing internal
    // flash with the SoftDevice enabled is only legal through its own
    // `sd_flash_*` syscalls, which need the enabled SoftDevice.
    let shared_flash = leviculum_nrf::flash::shared_flash(sd);
    leviculum_nrf::radio_store::spawn_store_task(
        &spawner,
        shared_flash,
        rak4631::CONFIG.radio_config_flash_page,
    );
    // Telemetry-target persistence (#236): its own page and its own task,
    // borrowing the same one-and-only SoftDevice flash handle.
    leviculum_nrf::telemetry::spawn_store_task(
        &spawner,
        shared_flash,
        rak4631::CONFIG.telemetry_flash_page,
    );

    // Optional baseboard peripherals (RAK19026 VC). Each spawn is gated on
    // its own feature; absent features leave the bare-module build clean.
    #[cfg(feature = "display")]
    {
        leviculum_nrf::display::init(&spawner, p.TWISPI0, p.P0_13, p.P0_14, identity_hash);
        info!("display task spawned");
        // The user button shares the same `display` feature gate — it has
        // no purpose without something to switch on/off.
        leviculum_nrf::button::init(&spawner, p.P0_09.into());
        info!("button task spawned");
    }
    #[cfg(feature = "gnss")]
    {
        leviculum_nrf::gnss::init(
            &spawner,
            p.UARTE0,
            p.TIMER1,       // idle-line detection for read_until_idle
            p.PPI_CH0,      // RXDRDY → timer clear/start
            p.PPI_CH1,      // timer compare → RX stop
            p.P0_15.into(), // RX from ZOE-M8Q TX
            p.P0_16.into(), // TX to ZOE-M8Q RX
            p.P0_17.into(), // PPS (configured but unused)
        );
        info!("gnss task spawned");
    }
    #[cfg(feature = "battery")]
    {
        leviculum_nrf::battery::init(&spawner, p.SAADC, p.P0_05);
        info!("battery task spawned");
    }

    let (hu, hf) = leviculum_nrf::heap_stats();
    info!("heap u={} f={}", hu, hf);
    leviculum_nrf::log_stack("post-init");

    // Interface adapters
    let serial_ctl_tx = serial.outgoing_tx;
    let mut serial_iface = EmbeddedInterface::new(serial.outgoing_tx);
    let mut lora_iface = LoRaInterface::new(lora_channels.outgoing_tx);
    let mut ble_iface = BleInterface::new(ble_channels.outgoing_tx);
    let ifac_configs: BTreeMap<usize, IfacConfig> = BTreeMap::new();

    // LED activity. With `display`: green LED1 = TX activity, blue LED2
    // = RX activity, driven by the lora task's flash signals. Without
    // `display`: a plain 1 Hz heartbeat on LED1 as a basic alive
    // indicator (kept so the bare-module build still has a visible
    // sign of life).
    #[cfg(feature = "display")]
    leviculum_nrf::led::init(&spawner, led_tx, led_rx);
    #[cfg(not(feature = "display"))]
    spawner.must_spawn(led_heartbeat(led));

    // Calendar seeding from GNSS (Codeberg #166 item 1): the main loop
    // owns the node, so it is the one place a fix can reach the
    // wall-time seam. One accepted fix seeds; the monotonic clock
    // carries the calendar from there — the receiver keeps running for
    // position only, never as a clock.
    #[cfg(feature = "gnss")]
    let mut gnss_rx = {
        let rx = leviculum_nrf::baseboard::GNSS_FIX.receiver();
        if rx.is_none() {
            log_critical!("[TIME_SEED_REFUSED] source=gnss reason=watch_capacity");
        }
        rx
    };
    #[cfg(feature = "gnss")]
    let mut time_seed_gate = leviculum_gnss_time::SeedGate::new();

    // Telemetry (Codeberg #236). The delivery destination is registered
    // unconditionally, target or not: it is what a receiver verifies our
    // LXMF signature against, and it is useful on its own — a node that
    // announces it can be addressed by name instead of by hex string.
    let delivery_hash = leviculum_nrf::telemetry::register_delivery_destination(&mut node);
    if let Some(hash) = delivery_hash.as_ref() {
        let dh = hash.as_bytes();
        leviculum_nrf::log::log_fmt("[IDENTITY] ", format_args!(
            "rak_lxmf_delivery={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            dh[0], dh[1], dh[2], dh[3], dh[4], dh[5], dh[6], dh[7],
            dh[8], dh[9], dh[10], dh[11], dh[12], dh[13], dh[14], dh[15]
        ));
    }
    let mut reporter = delivery_hash.map(leviculum_nrf::telemetry::Reporter::new);
    if reporter.is_none() {
        log_critical!("[TELEMETRY] target=00000000 state=off reason=no-delivery-destination");
    }
    if let Some(reporter) = reporter.as_mut() {
        // A persisted target comes back as awaiting-key unless its key
        // was persisted with it; the node then resolves it over the air
        // exactly as it would after a fresh set.
        if let Some(stored) = leviculum_nrf::telemetry::load(rak4631::CONFIG.telemetry_flash_page) {
            reporter.apply_target(&mut node, stored);
        }
        reporter.log_banner();
    }
    let telemetry_target_rx = leviculum_nrf::telemetry::inbound_target_receiver();

    // Periodic `[TRANSPORT]` counters (#344). Rides the main loop rather than
    // a spawned task: the counters live in the node this loop owns.
    let mut transport_stats = leviculum_nrf::transport_stats::Ticker::new();

    // Event-driven main loop, eight event sources:
    // 1. Serial incoming (USB)
    // 2. LoRa incoming (radio)
    // 3. BLE incoming (defragmented Reticulum packets from phone)
    // 4. Timer deadline (protocol maintenance, announces)
    // 5. GNSS time candidate (until the calendar is seeded once)
    // 6. Host wall-time injection (#238 control envelope)
    // 7. Host telemetry target (#238 control envelope, #236)
    // 8. Telemetry evaluation tick (only while a target is configured)
    loop {
        transport_stats.poll(&node);
        // Clamped by the stats deadline so the line is still emitted on a
        // channel quiet enough that the node itself has nothing scheduled.
        let deadline = node
            .next_deadline()
            .map(Instant::from_millis)
            .unwrap_or(Instant::MAX)
            .min(transport_stats.deadline());

        let gnss_time_candidate = async {
            #[cfg(feature = "gnss")]
            {
                if time_seed_gate.is_seeded() {
                    // Seeded for this boot: nothing left to wait for.
                    core::future::pending::<u64>().await
                } else {
                    match gnss_rx.as_mut() {
                        Some(rx) => loop {
                            let fix = rx.changed().await;
                            if let Some(unix) = time_seed_gate.offer(fix.unix_secs) {
                                break unix;
                            }
                        },
                        None => core::future::pending::<u64>().await,
                    }
                }
            }
            #[cfg(not(feature = "gnss"))]
            {
                core::future::pending::<u64>().await
            }
        };

        // The tick rate is also the retry rate for a report the radio
        // could not take: the policy re-arms until a send is confirmed.
        // With no target configured there is nothing to wake up for.
        let telemetry_tick = async {
            match reporter.as_ref() {
                Some(reporter) if !reporter.is_off() => Timer::after(TELEMETRY_TICK_INTERVAL).await,
                _ => core::future::pending::<()>().await,
            }
        };

        match select4(
            select4(
                serial.incoming_rx.receive(),
                lora_channels.incoming_rx.receive(),
                ble_channels.incoming_rx.receive(),
                Timer::at(deadline),
            ),
            gnss_time_candidate,
            select(serial.wall_time_rx.receive(), telemetry_target_rx.receive()),
            telemetry_tick,
        )
        .await
        {
            Either4::Third(Either::First(unix_secs)) => {
                // A host that knows wall time (#238 TYPE_WALL_TIME). The
                // seam applies the same sanity window as every other time
                // source; the bool picks the enveloped ack or the named
                // refusal — mirror of the GNSS path.
                use leviculum_core::envelope;
                use leviculum_core::transport::TimeSource;
                let answer = if node.set_wall_time_unix_secs(unix_secs, TimeSource::Host) {
                    leviculum_nrf::set_time_source(TimeSource::Host);
                    log_critical!("[TIME_SEED] source=host unix={}", unix_secs);
                    log_critical!("[TIME_SOURCE] source={}", leviculum_nrf::time_source_str());
                    envelope::encode_ack(envelope::TYPE_WALL_TIME)
                } else {
                    log_critical!("[TIME_SEED_REFUSED] source=host unix={}", unix_secs);
                    envelope::encode_refusal(envelope::TYPE_WALL_TIME, envelope::REFUSE_VALUE)
                };
                // Best effort: a full outgoing channel means a busy link;
                // the host's retry covers it.
                let _ = serial_ctl_tx.try_send(answer);
            }
            Either4::Third(Either::Second(wire)) => {
                // A host set or cleared the telemetry target (#236). The
                // serial task already acked the frame; what happens here
                // is the part that needs the node — the identity lookup
                // that decides ready vs awaiting-key — plus the persist.
                use leviculum_nrf::telemetry::TargetOutcome;
                if let Some(reporter) = reporter.as_mut() {
                    match reporter.apply_target(&mut node, wire) {
                        TargetOutcome::Set(_) | TargetOutcome::Cleared => {
                            leviculum_nrf::telemetry::request_save(&wire);
                            reporter.log_banner();
                        }
                    }
                }
            }
            Either4::Second(unix) => {
                // A GNSS fix carrying UTC. The seam applies the same
                // sanity window as every other time source; a refusal is
                // surfaced as a structured event, never swallowed.
                #[cfg(feature = "gnss")]
                {
                    use leviculum_core::transport::TimeSource;
                    if node.set_wall_time_unix_secs(unix, TimeSource::Gnss) {
                        time_seed_gate.mark_seeded();
                        leviculum_nrf::set_time_source(TimeSource::Gnss);
                        log_critical!("[TIME_SEED] source=gnss unix={}", unix);
                        log_critical!("[TIME_SOURCE] source={}", leviculum_nrf::time_source_str());
                    } else {
                        log_critical!("[TIME_SEED_REFUSED] source=gnss unix={}", unix);
                    }
                }
                #[cfg(not(feature = "gnss"))]
                let _ = unix;
            }
            Either4::First(Either4::First(data)) => {
                info!("SER RX {} bytes", data.len());
                let output = node.handle_packet(InterfaceId(0), &data);
                info!("SER RX -> {} actions", output.actions.len());
                let mut ifaces: [&mut dyn Interface; 3] =
                    [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                let dispatched = dispatch_actions(&mut ifaces, output.actions, &ifac_configs);
                leviculum_nrf::dispatch::settle("ser-rx", &mut node, &dispatched);
            }
            Either4::First(Either4::Second(data)) => {
                let output = node.handle_packet(InterfaceId(1), &data);
                if !output.actions.is_empty() {
                    info!("LORA RX -> {} actions", output.actions.len());
                }
                let mut ifaces: [&mut dyn Interface; 3] =
                    [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                let dispatched = dispatch_actions(&mut ifaces, output.actions, &ifac_configs);
                leviculum_nrf::dispatch::settle("lora-rx", &mut node, &dispatched);
            }
            Either4::First(Either4::Third(data)) => {
                info!("BLE RX {} bytes", data.len());
                let output = node.handle_packet(InterfaceId(2), &data);
                if !output.actions.is_empty() {
                    info!("BLE RX -> {} actions", output.actions.len());
                }
                let mut ifaces: [&mut dyn Interface; 3] =
                    [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                let dispatched = dispatch_actions(&mut ifaces, output.actions, &ifac_configs);
                leviculum_nrf::dispatch::settle("ble-rx", &mut node, &dispatched);
            }
            Either4::First(Either4::Fourth(())) => {
                let output = node.handle_timeout();
                if !output.actions.is_empty() {
                    info!("timeout: {} actions", output.actions.len());
                    // Diagnostic: log each action's discriminant + target
                    // so we can tell whether a rebroadcast actually emits
                    // a Broadcast (hits all interfaces) or a SendPacket
                    // (single interface only).
                    for act in &output.actions {
                        match act {
                            Action::SendPacket { iface, data } => {
                                info!("ACT SendPacket iface={} len={}", iface.0, data.len());
                            }
                            Action::Broadcast {
                                exclude_iface,
                                data,
                                ..
                            } => {
                                let excl = exclude_iface.map(|i| i.0 as i16).unwrap_or(-1);
                                info!("ACT Broadcast excl={} len={}", excl, data.len());
                            }
                        }
                    }
                }
                let mut ifaces: [&mut dyn Interface; 3] =
                    [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                let dispatched = dispatch_actions(&mut ifaces, output.actions, &ifac_configs);
                leviculum_nrf::dispatch::settle("timeout", &mut node, &dispatched);
            }
            Either4::Fourth(()) => {
                // Telemetry evaluation (#236). Everything decided here is
                // decided in the policy crate; this arm reads the board's
                // sensors, hands them over, and dispatches whatever came
                // back.
                if let Some(reporter) = reporter.as_mut() {
                    let now_ms = node.now_ms();
                    let (readings, has_fix) = collect_readings(&node);
                    let actions = reporter.tick(&mut node, now_ms, has_fix, &readings);
                    if !actions.is_empty() {
                        let mut ifaces: [&mut dyn Interface; 3] =
                            [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                        let dispatched = dispatch_actions(&mut ifaces, actions, &ifac_configs);
                        // The reporter settles first: it is the only caller
                        // that owns a cadence the dispatch's verdict decides
                        // (#344). `settle` counts and logs afterwards, as at
                        // every other site.
                        reporter.note_dispatch(&dispatched);
                        leviculum_nrf::dispatch::settle("telemetry", &mut node, &dispatched);
                    }
                }
            }
        }
    }
}

/// How often the telemetry policy is asked whether a report is due.
///
/// This is a *poll* rate, not a cadence: the cadence lives in the profile
/// and is minutes to hours. Five seconds is fine enough that "report now"
/// means now to an operator watching a serial log, and coarse enough that
/// it is invisible next to the tasks already waking on the same period.
const TELEMETRY_TICK_INTERVAL: Duration = Duration::from_secs(5);

/// Read this board's sensors for one telemetry evaluation.
///
/// Returns the readings and whether GNSS presence is `Fix` — only that
/// state may contribute a position (#240), and the policy is told
/// separately rather than having to infer it from the numbers.
///
/// The per-board part of telemetry is exactly this function: which
/// peripherals exist. On a build without them it returns time and nothing
/// else, which is a legal heartbeat.
fn collect_readings<R, C, S>(
    node: &leviculum_core::node::NodeCore<R, C, S>,
) -> (leviculum_nrf::telemetry::Readings, bool)
where
    R: rand_core::CryptoRngCore,
    C: leviculum_core::traits::Clock,
    S: leviculum_core::traits::Storage,
{
    // A bare-module build has no sensor to fill in, so nothing mutates it
    // there; every feature that adds one needs the binding mutable.
    #[cfg_attr(
        not(any(feature = "gnss", feature = "battery")),
        allow(unused_mut, clippy::let_and_return)
    )]
    let mut readings = leviculum_nrf::telemetry::Readings {
        // A timebase below the plausibility floor is uptime seconds, not a
        // calendar estimate — the anchor model's "never ahead" rule has
        // nothing to work with there. Which arm anchored it is reported
        // alongside every reading as `[TIME_SOURCE]`.
        unix_secs: node
            .has_plausible_wall_clock()
            .then(|| node.emission_secs()),
        ..Default::default()
    };
    #[cfg(not(feature = "gnss"))]
    let has_fix = false;
    #[cfg(feature = "gnss")]
    let has_fix = {
        use leviculum_nrf::baseboard::{GnssPresence, GNSS_FIX, GNSS_PRESENCE};
        if let Some(fix) = GNSS_FIX.try_get() {
            readings.latitude = fix.latitude;
            readings.longitude = fix.longitude;
            readings.altitude_m = fix.altitude_m;
            readings.speed_mps = fix.speed_mps;
            readings.bearing_deg = fix.bearing_deg;
            readings.hdop = fix.hdop;
        }
        matches!(
            GNSS_PRESENCE.try_get().map(|p| p.state),
            Some(GnssPresence::Fix)
        )
    };
    #[cfg(feature = "battery")]
    {
        readings.battery_percent = leviculum_nrf::baseboard::BATTERY_STATE
            .try_get()
            .map(|b| b.percent);
    }
    (readings, has_fix)
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
        log_critical!("[FW_BUILD] {}", leviculum_nrf::FW_BUILD_STAMP);
        log_critical!("[TIME_SOURCE] source={}", leviculum_nrf::time_source_str());
    }
}

/// Diagnostic: log stack and heap headroom every 5 s. Lets us see if
/// the executor-hang under load correlates with stack creeping toward
/// zero (overflow → HardFault) or heap exhaustion. Logs once per period
/// regardless; cheap.
#[embassy_executor::task]
async fn diag_mem_log() {
    loop {
        Timer::after(Duration::from_secs(5)).await;
        let stack = leviculum_nrf::stack_min_free();
        let (hu, hf) = leviculum_nrf::heap_stats();
        info!(
            "[DIAG_MEM] stack_min_free={} heap_used={} heap_free={}",
            stack, hu, hf
        );
    }
}

#[cfg(not(feature = "display"))]
#[embassy_executor::task]
async fn led_heartbeat(mut led: Output<'static>) {
    loop {
        led.set_high();
        Timer::after(Duration::from_millis(500)).await;
        led.set_low();
        Timer::after(Duration::from_millis(500)).await;
    }
}
