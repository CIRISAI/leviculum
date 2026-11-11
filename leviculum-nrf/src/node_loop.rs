//! The node loop, once (#469 step 7, order 1085): the skeleton every board
//! binary runs, from `embassy_nrf::init` to the main loop's last arm.
//!
//! `docs/drafts/node-loop-diff-2026-10.md` diffed the four binaries line by
//! line: about 705 of about 840 code lines a file were one skeleton, and
//! every other line is a board fact or a behaviour decision Lew has taken
//! (D1 to D7, 2026-10-28). This module is that skeleton; a binary is the
//! board's wiring between its calls. The calls, in the order a binary makes
//! them, with the board's own steps between them:
//!
//! 1. [`enter`]: the boot record, the HAL, the heap, the previous boot's
//!    post-mortems, the stack paint. The binary then sets its panic LED.
//! 2. [`pre_usb`]: IRQ priorities, the set-once registrations, the pre-USB
//!    flash reads, USB. The binary then prints its boot line.
//! 3. [`boot_banner`]: `[FW_BUILD]`, `[TIME_SOURCE]`, the GNSS presence
//!    anchor (D2) and the boot diagnostics. The binary then raises its
//!    rails and takes its LED.
//! 4. [`build_node`]: identity, builder, heap budget, time floor, the
//!    management banner, the interfaces, `LNode started`. The binary then
//!    states its QSPI part, brings its radio up and prints the part.
//! 5. [`lora_up`]: the radio profile and the LoRa spawn decision.
//! 6. [`ble_up`]: BLE, the media and name banners, the flash-backed store
//!    tasks. The binary then spawns its peripheral tasks.
//! 7. [`log_post_init`]: the heap and stack lines. The binary then hands
//!    its LEDs to the feedback grammar.
//! 8. [`run`]: telemetry, the propagation role, the announce gate, and the
//!    main loop, which never returns.
//!
//! The T114 (order 1085) and the RAK4631 (order 1572) run it; the solar
//! node and the T1000-E follow, one order each, and until then their
//! binaries carry their own copy of everything here.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use embassy_executor::Spawner;
use embassy_futures::select::{select, select3, select4, Either, Either3, Either4};
use embassy_nrf::{peripherals, Peri};
use embassy_time::{Duration, Instant, Timer};

use leviculum_core::embedded_storage::EmbeddedStorage;
use leviculum_core::envelope::MediaProfileWire;
use leviculum_core::ifac::IfacConfig;
use leviculum_core::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use leviculum_core::traits::Interface;
use leviculum_core::transport::dispatch_actions;
use leviculum_core::InterfaceId;

use leviculum_firmware::ble_link::{BleChannels, BleInterface};
use leviculum_firmware::clock::{set_time_source, time_source_str, EmbassyClock};
use leviculum_firmware::seams::BleLinkEvent;
use leviculum_firmware::{entropy::Entropy, interface::EmbeddedInterface};

use crate::boards::BoardConfig;
use crate::lora::{LoRaChannels, LoRaInterface};
use crate::{info, log_critical};

/// The node every board runs: the core over the firmware's entropy, clock
/// and storage.
pub type Node = NodeCore<Entropy, EmbassyClock, EmbeddedStorage>;

/// The board facts the skeleton reads, one value per binary.
pub struct Board {
    /// USB ids and strings, the flash pages, the TCXO select.
    pub config: &'static BoardConfig,
    /// The interface names, indexed by interface id.
    pub iface_names: &'static [&'static str; leviculum_firmware::iface_bytes::IFACE_COUNT],
    /// The baud the board's GNSS module ships at, for the `[GNSS_PRESENCE]
    /// state=detecting` anchor (D2). Read only where the `gnss` feature is
    /// on, the same gate the GNSS task and the position source are under.
    pub gnss_baud: u32,
}

/// What [`enter`] read before this boot wrote anything: the previous boot's
/// record, post-mortems and log tail, for [`pre_usb`] and [`boot_banner`].
pub struct Early {
    boot: leviculum_firmware::boot_trace::Captured,
    hardfault_pm: Option<crate::HardfaultPostMortem>,
    panic_pm: Option<crate::PanicPostMortem>,
    persistent_log: Option<crate::log::PersistentLogSnapshot>,
}

/// What [`pre_usb`] read and brought up, for the steps after it.
pub struct Up {
    media: MediaProfileWire,
    media_src: crate::media::Source,
    mgmt_allow: crate::mgmt::RemoteMgmtDecision,
    boot_count: crate::boot_count::Recorded,
    vbus: &'static crate::SoftwareVbusDetect,
    serial: crate::usb::SerialChannels,
}

/// The node [`build_node`] built, and the calendar checkpoint it restored.
pub struct Built {
    node: Box<Node>,
    time_checkpoint: crate::time_floor::Checkpoint,
}

impl Built {
    /// The node, for the steps between [`build_node`] and [`run`] that read
    /// it.
    pub fn node(&self) -> &Node {
        &self.node
    }
}

/// What [`ble_up`] brought up.
pub struct Ble {
    /// The enabled SoftDevice: the flash handle's owner and the die
    /// temperature's only legal path.
    pub sd: &'static nrf_softdevice::Softdevice,
    /// The identity hash the BLE surfaces carry, for a board's display.
    pub identity_hash: [u8; 16],
    channels: BleChannels,
}

/// The peripherals `ble::init` takes, which are the same on every board.
/// Built from the binary's `Peripherals` by [`ble_peripherals!`](crate::ble_peripherals!).
pub struct BlePeripherals {
    pub rtc0: Peri<'static, peripherals::RTC0>,
    pub timer0: Peri<'static, peripherals::TIMER0>,
    pub temp: Peri<'static, peripherals::TEMP>,
    pub ppi_ch17: Peri<'static, peripherals::PPI_CH17>,
    pub ppi_ch18: Peri<'static, peripherals::PPI_CH18>,
    pub ppi_ch19: Peri<'static, peripherals::PPI_CH19>,
    pub ppi_ch20: Peri<'static, peripherals::PPI_CH20>,
    pub ppi_ch21: Peri<'static, peripherals::PPI_CH21>,
    pub ppi_ch22: Peri<'static, peripherals::PPI_CH22>,
    pub ppi_ch23: Peri<'static, peripherals::PPI_CH23>,
    pub ppi_ch24: Peri<'static, peripherals::PPI_CH24>,
    pub ppi_ch25: Peri<'static, peripherals::PPI_CH25>,
    pub ppi_ch26: Peri<'static, peripherals::PPI_CH26>,
    pub ppi_ch27: Peri<'static, peripherals::PPI_CH27>,
    pub ppi_ch28: Peri<'static, peripherals::PPI_CH28>,
    pub ppi_ch29: Peri<'static, peripherals::PPI_CH29>,
    pub ppi_ch30: Peri<'static, peripherals::PPI_CH30>,
    pub ppi_ch31: Peri<'static, peripherals::PPI_CH31>,
    pub rng: Peri<'static, peripherals::RNG>,
}

/// Move the BLE peripherals out of a binary's `Peripherals`, leaving the
/// rest of it to the board. A macro and not a function because a function
/// would take the whole struct, and the board still owns its pins.
#[macro_export]
macro_rules! ble_peripherals {
    ($p:ident) => {
        $crate::node_loop::BlePeripherals {
            rtc0: $p.RTC0,
            timer0: $p.TIMER0,
            temp: $p.TEMP,
            ppi_ch17: $p.PPI_CH17,
            ppi_ch18: $p.PPI_CH18,
            ppi_ch19: $p.PPI_CH19,
            ppi_ch20: $p.PPI_CH20,
            ppi_ch21: $p.PPI_CH21,
            ppi_ch22: $p.PPI_CH22,
            ppi_ch23: $p.PPI_CH23,
            ppi_ch24: $p.PPI_CH24,
            ppi_ch25: $p.PPI_CH25,
            ppi_ch26: $p.PPI_CH26,
            ppi_ch27: $p.PPI_CH27,
            ppi_ch28: $p.PPI_CH28,
            ppi_ch29: $p.PPI_CH29,
            ppi_ch30: $p.PPI_CH30,
            ppi_ch31: $p.PPI_CH31,
            rng: $p.RNG,
        }
    };
}

/// The first call of a board's `main`: the boot record, the HAL, the heap,
/// the previous boot's diagnostics and the stack paint. Returns the
/// peripherals for the board to wire.
pub fn enter() -> (Early, embassy_nrf::Peripherals) {
    // First statement on purpose: reads the PREVIOUS boot's breadcrumbs
    // and RESETREAS (and clears it), then arms this boot's record at
    // enter-main — so even a hang inside `embassy_nrf::init`'s HFXO wait
    // is attributable on the next boot's BOOT_TRACE line.
    let boot = crate::boot_trace::capture();

    let mut config = embassy_nrf::config::Config::default();
    config.hfclk_source = embassy_nrf::config::HfclkSource::ExternalXtal;
    config.gpiote_interrupt_priority = embassy_nrf::interrupt::Priority::P2;
    config.time_interrupt_priority = embassy_nrf::interrupt::Priority::P2;
    // The board binds this `mut`: the boot record reborrows the NVMC in
    // [`pre_usb`] and the identity store takes it for good in
    // [`build_node`].
    let p = embassy_nrf::init(config);

    crate::init_heap();

    // Boot diagnostics from the previous run. Read before the first log
    // call of this boot: `take_persistent_log` must snapshot the PREVIOUS
    // boot's tail before we start writing this boot's into the same ring.
    let hardfault_pm = crate::take_hardfault_postmortem();
    let panic_pm = crate::take_panic_postmortem();
    let persistent_log = crate::log::take_persistent_log();

    // SAFETY: called once before any complex work or concurrent tasks.
    // Under flip-link the painted region is `[_stack_end, SP-1KiB)` at
    // the BOTTOM of RAM — disjoint from `.uninit`, so the post-mortem
    // reads above are no longer order-critical against it.
    unsafe {
        crate::paint_stack();
    }

    (
        Early {
            boot,
            hardfault_pm,
            panic_pm,
            persistent_log,
        },
        p,
    )
}

/// IRQ priorities, the set-once registrations, the flash reads that must
/// precede USB and the SoftDevice, then USB. The board's boot line follows,
/// before [`boot_banner`].
pub fn pre_usb(
    spawner: &Spawner,
    board: &Board,
    early: &Early,
    nvmc: Peri<'_, peripherals::NVMC>,
    usbd: Peri<'static, peripherals::USBD>,
) -> Up {
    crate::set_irq_priorities();
    leviculum_firmware::iface_bytes::set_names(board.iface_names);
    leviculum_firmware::entropy::set_source(crate::rng::fill_entropy);
    leviculum_firmware::feedback::set_sinks::<crate::feedback::NrfSinks>();
    // This binary wires a telemetry Reporter into its main loop; declared
    // before USB comes up so the serial task can never answer a telemetry
    // target ahead of the declaration (ack honesty, #236).
    crate::telemetry::declare_reporter();
    // Which position sources this board boots with — the second clause of
    // the telemetry send condition, and a question a host may put before
    // the reporter exists, so it is answered before USB comes up. The
    // GNSS bit says a receiver is *configured*, not that it has a fix:
    // reports run `position=0` until the L76K acquires and `position=1
    // possrc=gnss` after (#69, #255). `cfg!` and not a constant, because
    // the spawn below is under the same gate — the two must not be able
    // to disagree.
    crate::telemetry::declare_position_sources(
        /* gnss_available */ cfg!(feature = "gnss"),
        board.config.telemetry_flash_page,
    );
    // The media profile decides which carriers come up at all, so it is
    // read before USB and before either carrier: before USB so a host
    // frame can never be answered against the default while the flash
    // record is still unread, and before the carriers because the answer
    // is their spawn decision. A memory-mapped flash read, legal this
    // early and before `Softdevice::enable`.
    let (media, media_src) = crate::media::load_at_boot(board.config.telemetry_flash_page);
    // The node's name, read on the same page and for the same reason the
    // profile is: before USB, so a host frame is never answered against
    // the derived default while the record is still unread, and before
    // the first announce and `ble::init`, which both display it.
    crate::name::load_at_boot(board.config.telemetry_flash_page);
    // Remote management (#235): the identity allow-list this board serves
    // `rnstransport.remote.management` `/status` to, read on the same page
    // and this early because the answer decides how the NODE IS BUILT —
    // the destination is created by the builder below and cannot be added
    // or withdrawn while the node runs. Also before USB, so a host frame
    // is never answered against an unread record.
    let mgmt_allow = crate::mgmt::load_at_boot(board.config.telemetry_flash_page);
    // How many times this board has restarted, appended to its own flash
    // page. Here because it must be before `Softdevice::enable` — after
    // it, writing internal flash means going through the SD, and a board
    // that boot-loops on a sagging pack rarely gets that far (the whole
    // reasoning is in `boot_count`). The line it produces waits for USB.
    let boot_count =
        crate::boot_count::record_at_boot(embassy_nrf::nvmc::Nvmc::new(nvmc), &early.boot);
    leviculum_firmware::boot_trace::phase(leviculum_firmware::boot_trace::Phase::PersistRead);
    let vbus = crate::init_vbus();
    let serial = crate::usb::init(spawner, usbd, vbus, board.config);
    leviculum_firmware::boot_trace::phase(leviculum_firmware::boot_trace::Phase::UsbUp);

    Up {
        media,
        media_src,
        mgmt_allow,
        boot_count,
        vbus,
        serial,
    }
}

/// The lines that follow the board's boot line: the build, the time source,
/// the GNSS presence anchor and the previous boot's diagnostics.
pub fn boot_banner(early: Early, up: &Up, board: &Board) {
    log_critical!("[FW_BUILD] {}", crate::FW_BUILD_STAMP);
    log_critical!("[TIME_SOURCE] source={}", time_source_str());
    // D2 (Lew, 2026-10-28): every GNSS board marks "machinery armed,
    // answer pending" here, so a log tail always carries a
    // `[GNSS_PRESENCE]` anchor before the task's first transition.
    #[cfg(feature = "gnss")]
    crate::gnss::log_detecting(board.gnss_baud);
    #[cfg(not(feature = "gnss"))]
    let _ = board;
    crate::log_boot_diagnostics(
        &early.boot,
        &up.boot_count,
        early.hardfault_pm.as_ref(),
        early.panic_pm.as_ref(),
        early.persistent_log,
    );
}

/// The identity, the node, its budget and calendar floor, the management
/// banner, the banner tasks, the three interfaces, and `LNode started`.
pub fn build_node(
    spawner: &Spawner,
    board: &Board,
    nvmc: Peri<'static, peripherals::NVMC>,
    up: &Up,
) -> Built {
    let rng = Entropy;

    // Load or generate persistent identity from internal flash
    let mut id_store = crate::flash::NvmcIdentityStore::new(
        embassy_nrf::nvmc::Nvmc::new(nvmc),
        board.config.identity_flash_page,
    );

    // #388 pass 3: the heap budget's `links=` term, derived from the
    // arithmetic in `heap_census::max_endpoint_links` and ENFORCED as the
    // core's link cap — the budget and the cap are the same constant.
    const MAX_ENDPOINT_LINKS: usize = crate::heap_census::max_endpoint_links(core::mem::size_of::<
        leviculum_core::node::NodeCore<Entropy, EmbassyClock, EmbeddedStorage>,
    >());
    const _: () = assert!(
        MAX_ENDPOINT_LINKS >= crate::ble::MAX_LINKS,
        "HEAP_BUDGET affords fewer endpoint links than claimable BLE sessions (#388)"
    );

    let mut builder = NodeCoreBuilder::new()
        .enable_transport(true)
        .max_links(Some(MAX_ENDPOINT_LINKS))
        .max_incoming_resource_size(leviculum_heap_budget::MAX_INCOMING_RESOURCE_BYTES)
        .max_incoming_resources_per_link(leviculum_heap_budget::BOARD_INCOMING_RESOURCES_PER_LINK)
        // #402: announces held back by the LoRa announce cap wait here, and
        // on a board the queue is a heap term, not a formality. One entry is
        // its raw announce (~183 B) plus its Vec header, destination, hops
        // and timestamp, about 220 B, so eight entries are ~1.8 KiB of the
        // 45-50 KiB idle heap — against 16384 entries (the host default, and
        // Python's `MAX_QUEUED_ANNOUNCES`), which is no bound at all here.
        // Eight is also a time bound: at SF10 the 2 % share holds an announce
        // ~92 s, so a full queue is ~12 minutes deep, and past that an
        // announce is better re-learned from its source's next one than
        // replayed stale. A full queue drops the ARRIVING announce, the
        // newest one; the queue keeps what it has and drains fewest-hops-
        // first (`Transport::drain_announce_queues`).
        .max_queued_announces(8)
        // #598: announces the ingress burst limiter holds are the same kind
        // of heap term, and the core's 256 per interface would be more than
        // the heap. The cap and its arithmetic are the heap budget's
        // (`leviculum_heap_budget::HELD_ANNOUNCE_SLICE_BYTES`), pinned there.
        .max_held_announces(crate::heap_census::BOARD_MAX_HELD_ANNOUNCES)
        .max_random_blobs(8)
        .respond_to_probes(true);

    // Only a stored, NON-EMPTY list registers anything. That rule is
    // `leviculum_core::mgmt_allow_store::remote_mgmt_decision`, and it is
    // deliberately not the daemon's: `lnsd` and Python's `rnsd` register
    // the handler with an empty list and consult it per request
    // (`leviculum-std/src/config.rs:88-96`), which is safe on a machine
    // with an operator and a login, and is not safe on a board left on a
    // mast — an unattended node announcing a management destination with
    // nobody on the list is advertising a door (#235).
    if let crate::mgmt::RemoteMgmtDecision::Enabled(allowed) = &up.mgmt_allow {
        // `/dfu` beside `/status`, under the same list (#235): a
        // board can enter its bootloader, so it serves the verb.
        builder = builder
            .remote_management(true, allowed.clone())
            .remote_dfu(true);
    }

    let identity_loaded = {
        use leviculum_core::identity_store::IdentityStore;
        // Critical for the same reason as the `[IDENTITY]` banner below
        // (#234): this says whether the hash that banner prints was the
        // board's own or made up this boot, and the recovery doc sends an
        // operator to the debug port to read exactly that after a flash.
        if let Ok(Some(identity)) = id_store.load() {
            log_critical!("Identity loaded from flash");
            builder = builder.identity(identity);
            true
        } else {
            log_critical!("No identity in flash, generating new");
            false
        }
    };

    let mut node = builder.build_boxed(rng, EmbassyClock, EmbeddedStorage::new());
    // #388: state the heap budget and refuse a configuration that
    // cannot fit — at boot, where the panic names the arithmetic. The
    // compile-time twin below fails the build before it can fail a
    // board.
    crate::heap_census::log_budget_and_assert(core::mem::size_of_val(&*node));
    // Codeberg #166 item 4: the calendar this board learned before it lost
    // power comes back as a floor at the birth rank, here, before any
    // interface exists to deliver an announce, so the first one is judged
    // against it. Printed beside `[TIME_SOURCE]`, which says
    // `restored-floor` when the seam took it.
    let time_checkpoint = crate::time_floor::restore_at_boot(|v| node.restore_emission_floor(v));
    if set_time_source(node.time_source()) {
        log_critical!("[TIME_SOURCE] source={}", time_source_str());
    }
    const _: () = assert!(
        crate::heap_census::budget_total(core::mem::size_of::<
            leviculum_core::node::NodeCore<Entropy, EmbassyClock, EmbeddedStorage>,
        >()) <= leviculum_heap_budget::HEAP_SIZE,
        "HEAP_BUDGET total exceeds the heap (#388): shrink a term"
    );

    // What the builder actually did with the allow-list, on the critical
    // log path: `[MGMT] enabled allowed=N`, or `disabled (no allow-list)`
    // for the state a board ships in. Read `remote_mgmt_dest_hash` rather
    // than assuming, so a boot that could not derive the destination says
    // so instead of promising a service nothing serves.
    let mgmt_registered = node.remote_mgmt_dest_hash().is_some();
    crate::mgmt::note_registered(mgmt_registered);
    crate::mgmt::log_banner(&up.mgmt_allow, mgmt_registered);

    let initial_path_len = node.path_count();
    info!("[BOOT] path_table_initial_len={}", initial_path_len);
    spawner.must_spawn(boot_log_repeater(initial_path_len));
    spawner.must_spawn(fw_build_banner(up.media_src));
    crate::spawn_diagnostics_tasks(spawner);

    if !identity_loaded {
        use leviculum_core::identity_store::IdentityStore;
        let _ = id_store.save(node.identity());
        info!("Identity saved to flash");
    }

    // Register all three interfaces
    node.set_interface_name(0, alloc::string::String::from(board.iface_names[0]));
    node.set_interface_hw_mtu(0, 564);
    // Codeberg #117: the client-facing serial interface must be Gateway so the
    // node discovers unknown paths on behalf of the host.
    node.set_interface_mode(0, leviculum_core::InterfaceMode::Gateway);
    node.set_interface_name(1, alloc::string::String::from(board.iface_names[1]));
    // Codeberg #390: 508, not 255. Three places already agree on 508 for a
    // LoRa carrier -- the reference (`RNS/Interfaces/RNodeInterface.py`,
    // `HW_MTU = 508`), our own RNode constant (`leviculum-core/src/rnode.rs`,
    // `HW_MTU: usize = 508`), and the splitter that actually puts the bytes on
    // the air (`build_lora_frames`, two frames of at most
    // `MAX_SINGLE_PAYLOAD = 254`). The radio was never the limit; 255 was.
    //
    // What the old 255 cost: an incoming link over this interface clamped to
    // 255 on the responder (`Link::new_incoming`, `path_mtu.min(hw_mtu)`)
    // while the initiator floored the confirmed value back at 500
    // (`Link::process_proof`). The two ends then derived different resource
    // SDUs (464 vs 219), the receiver computed more parts than the
    // advertisement had hashmap entries, and every request it built came back
    // flagged exhausted -- an unbounded 115-byte REQ/HMU loop that burned a PN
    // sync round for 177 s until the outbound watchdog reaped it
    // (`lora_pn_board_sync`, hardware, 2026-09-14). Pinned in
    // `leviculum-core/src/node/mvr_link_mtu_asymmetry.rs`. Read that price
    // before tidying this number back down.
    node.set_interface_hw_mtu(1, 508);
    // The board is the mesh's relay on this link, so it must answer "where
    // is X" for the nodes behind it, not only for the daemon on its serial
    // line (#117 set that one alone). A `Full` interface returns from
    // `handle_path_request` case 3 without re-originating discovery
    // (`Full.discovers_paths() == false`), which left the LoRa and BLE sides
    // of a board deaf to path requests and the announce-capped queue -- 2 %
    // of link capacity, ~13.4 s per relayed announce at SF7 -- as the only
    // way anything could be resolved across the carrier seam. That cost
    // `ble_lora_transport` its `file_transfer` step: the host's path request
    // reached the pocket board over BLE and produced no action, and the
    // listener's announce arrived 91 s later, 1.2 s after the step's 60 s
    // budget. Re-origination excludes the receiving interface, so this adds
    // no airtime to the band the question came in on. Pinned in
    // `leviculum-core/src/node/mvr_board_radio_pathresolve.rs`.
    node.set_interface_mode(1, leviculum_core::InterfaceMode::Gateway);
    node.set_interface_name(2, alloc::string::String::from(board.iface_names[2]));
    node.set_interface_hw_mtu(2, 564);
    node.set_interface_mode(2, leviculum_core::InterfaceMode::Gateway);
    // #854: the BLE interface serves every link but one
    // (`try_send_excluding_peer`), so an announce can be kept off the link
    // of the neighbour our own route runs through.
    node.set_interface_link_sparing(2, true);
    // #606: the USB serial interface runs no ingress burst limiter, LoRa
    // and BLE do. The table and its reasons are the heap budget's, which
    // sizes the held-announce cap for exactly the interfaces left on.
    leviculum_firmware::iface_bytes::apply_board_ingress_control(&mut *node);

    // #234: boot-critical, because `log_fmt` DROPS a line while the
    // runtime-drain gate is shut — it returns before the ring buffer and
    // before the reset-surviving tail — and the gate opens on the first
    // DTR-assert or after 30 s, both long after this point. A one-shot
    // line written there is not late, it is gone, and nothing re-emits it.
    // The full hashes are no longer spelled out here at all: the
    // `[IDENTITY]` banner below carries every one of them, on the critical
    // path and repeated every 5 s, so a reader attached at any moment gets
    // them instead of only one attached across the boot.
    let hash = node.identity().hash();
    log_critical!(
        "LNode started -- identity: {:02X}{:02X}{:02X}{:02X}{:02X}",
        hash[0],
        hash[1],
        hash[2],
        hash[3],
        hash[4]
    );

    Built {
        node,
        time_checkpoint,
    }
}

/// The radio profile and the LoRa spawn decision, after the board brought
/// its transceiver up with its own pins.
pub fn lora_up(
    spawner: &Spawner,
    board: &Board,
    up: &Up,
    lora: crate::radio::Radio,
) -> LoRaChannels {
    // Radio profile: whatever a host last set and we persisted, else the
    // compiled default. A blank or corrupt page decodes to None.
    let radio_cfg = match crate::radio_store::load(board.config.radio_config_flash_page)
        .and_then(crate::lora::RadioConfig::from_wire_config)
    {
        Some(cfg) => {
            crate::log::log_fmt(
                "[RADIO] ",
                format_args!(
                    "persisted freq={} bw={} sf={} cr={} pwr={}",
                    cfg.frequency_hz, cfg.bw_hz, cfg.sf, cfg.cr_denom, cfg.tx_power_dbm
                ),
            );
            cfg
        }
        None => {
            crate::log::log_fmt("[RADIO] ", format_args!("default eu_medium"));
            crate::lora::RadioConfig::eu_medium()
        }
    };
    let lora_channels = crate::lora::channels();
    // `lora=off` means the radio stays down: the task that resets,
    // configures and keys the SX1262 is never spawned, so the chip is
    // never brought out of the state `lora::init` left it in. Nothing
    // transmits and nothing is received — which is the point, since a
    // reception over the medium under test's neighbour is exactly what
    // makes a single-medium measurement falsifiable.
    if up.media.lora_enabled {
        // Per-board entropy for the TX channel-access randomness: a fixed
        // seed here would make co-booted boards draw identical pre-TX
        // jitter and collide anyway (see `lora_task`).
        let channel_seed = crate::lora::channel_seed();
        crate::lora::spawn(spawner, lora, radio_cfg, channel_seed);
        leviculum_firmware::boot_trace::phase(leviculum_firmware::boot_trace::Phase::LoraTask);
    } else {
        leviculum_firmware::media::log_carrier_held_down("lora");
        leviculum_firmware::boot_trace::phase(leviculum_firmware::boot_trace::Phase::LoraSkipped);
    }

    lora_channels
}

/// BLE, the media and name banners, and the store tasks on the
/// SoftDevice's flash handle.
pub async fn ble_up(
    spawner: &Spawner,
    board: &Board,
    node: &Node,
    up: &Up,
    ble: BlePeripherals,
) -> Ble {
    // BLE — full init restored. RAM ORIGIN bumped to 40K (memory.x) to give
    // Softdevice::enable headroom for our config (att_mtu=256, …).
    let identity_hash = *node.identity().hash();
    // Freeze the BLE name for this boot, before the SoftDevice is
    // enabled and before the advertisement is built: both read it, and a
    // name that changed between them would put two different strings on
    // the two BLE surfaces. This also publishes the identity hash the
    // control-envelope report derives its defaults from.
    crate::name::note_boot_name(&identity_hash);
    log_critical!("[STG] ble-init");
    let sd = crate::ble::init(
        spawner,
        up.media.ble_enabled,
        identity_hash,
        crate::name::boot_gap_name(),
        up.vbus,
        ble.rtc0,
        ble.timer0,
        ble.temp,
        ble.ppi_ch19,
        ble.ppi_ch30,
        ble.ppi_ch31,
        ble.ppi_ch17,
        ble.ppi_ch18,
        ble.ppi_ch20,
        ble.ppi_ch21,
        ble.ppi_ch22,
        ble.ppi_ch23,
        ble.ppi_ch24,
        ble.ppi_ch25,
        ble.ppi_ch26,
        ble.ppi_ch27,
        ble.ppi_ch28,
        ble.ppi_ch29,
        ble.rng,
    );
    let ble_channels = leviculum_firmware::ble_link::channels();
    info!("BLE ready");

    // Both spawn decisions are made: say what actually came up, then
    // prove it on the debug port. `note_boot_state` is passed what was
    // really spawned, which is what makes "switching a medium on takes
    // effect at reboot" a fact the board can state rather than a hope.
    crate::media::note_boot_state(up.media.lora_enabled, up.media.ble_enabled);
    crate::media::log_banner(up.media_src);
    crate::name::log_banner();

    // Radio-config persistence. Must come after `ble::init`: writing internal
    // flash with the SoftDevice enabled is only legal through its own
    // `sd_flash_*` syscalls, which need the enabled SoftDevice.
    let shared_flash = crate::flash::shared_flash(sd);
    crate::radio_store::spawn_store_task(
        spawner,
        shared_flash,
        board.config.radio_config_flash_page,
    );
    // The calendar checkpoint ring (#166 item 4), on the same handle.
    crate::time_floor::spawn_store_task(spawner, shared_flash);
    // Telemetry-target persistence (#236): its own page (0xEA000, reserved
    // in memory.x beside identity and radio config, above the linker's
    // FLASH region and at the bootloader's USER_FLASH_END so it survives a
    // UF2 update), borrowing the same one-and-only SoftDevice flash handle.
    crate::telemetry::spawn_store_task(spawner, shared_flash, board.config.telemetry_flash_page);

    // The message store (#384): mounts the record log on the region
    // `memory.x` reserves behind the image, formats it once if it is not ours,
    // and then does nothing until something asks it to append — nothing does
    // yet except the `--store-storm` bench instrument. Borrows the same
    // one-and-only SoftDevice flash handle as the two stores above, per
    // operation rather than per append (see `record_store` module docs).
    crate::record_store::spawn_store_task(spawner, shared_flash);

    // Codeberg #384 size harness, behind a throwaway non-default feature:
    // appends, iterates and removes once so the linker keeps the candidate
    // store's code and `arm-none-eabi-size` can weigh it. No shipped build
    // enables this.
    #[cfg(any(feature = "store-spike-record-log", feature = "store-spike-sequential"))]
    crate::store_spike::exercise(shared_flash).await;

    Ble {
        sd,
        identity_hash,
        channels: ble_channels,
    }
}

/// A board's line for a peripheral task it spawned, in the one wording
/// every board uses (D7 of order 1085): `<task> task spawned (<module>)`,
/// the module named by the board file, or `<task> task spawned` where the
/// task drives no module of its own.
pub fn log_task_spawned(task: &str, module: Option<&str>) {
    match module {
        Some(module) => info!("{} task spawned ({})", task, module),
        None => info!("{} task spawned", task),
    }
}

/// The heap and stack lines that close the bring-up, before the board hands
/// its LEDs to the feedback grammar.
pub fn log_post_init() {
    let (hu, hf) = leviculum_firmware::heap::heap_stats();
    info!("heap u={} f={}", hu, hf);
    crate::log_stack("post-init");
}

/// The node loop proper: telemetry, the propagation role, the announce gate
/// and the main loop. Never returns.
pub async fn run(
    spawner: &Spawner,
    board: &Board,
    built: Built,
    up: Up,
    lora_channels: LoRaChannels,
    ble: Ble,
) {
    let Built {
        mut node,
        mut time_checkpoint,
    } = built;
    let serial = up.serial;
    let sd = ble.sd;
    let ble_channels = ble.channels;

    // Interface adapters
    let serial_ctl_tx = serial.outgoing_tx;
    let mut serial_iface = EmbeddedInterface::new(serial.outgoing_tx);
    let mut lora_iface = LoRaInterface::new(lora_channels.outgoing_tx);
    let mut ble_iface = BleInterface::new(ble_channels.outgoing_tx);
    let ifac_configs: BTreeMap<usize, IfacConfig> = BTreeMap::new();

    // Telemetry (Codeberg #236). The delivery destination is registered
    // unconditionally, target or not: it is what a receiver verifies our
    // LXMF signature against, and it is useful on its own — a node that
    // announces it can be addressed by name instead of by hex string.
    let delivery_hash = crate::telemetry::register_delivery_destination(&mut node);
    // Every hash a prober needs, on one boot-critical line (and from
    // here on in the periodic banner and the identity query). This is
    // the only place they are written: a per-BSP copy on the gated path
    // was a line a late reader never saw (#234).
    leviculum_firmware::identity::note_boot_identity(
        *node.identity().hash(),
        node.probe_dest_hash().map(|h| *h.as_bytes()),
        delivery_hash.as_ref().map(|h| *h.as_bytes()),
    );
    // The propagation-node role (#384 part 3): register the
    // `lxmf.propagation` destination and its two request handlers, over
    // the record-log store the store task mounts. Costs from the config
    // page (PN_CONFIG line above states which source answered). The
    // miner task is the peering-key grinder — cooperative, off the main
    // loop, results harvested by the engine.
    let pn_config = crate::pn::load_config_at_boot(board.config.telemetry_flash_page);
    crate::position::load_at_boot(board.config.telemetry_flash_page);
    let mut pn_engine = crate::pn::Engine::new(&mut node, pn_config);
    if let Some(pn) = pn_engine.as_ref() {
        leviculum_firmware::identity::note_propagation(*pn.destination_hash().as_bytes());
        spawner.must_spawn(crate::pn::miner_task());
    } else {
        log_critical!("PN role=off reason=identity-underivable");
    }

    // The announce occasions telemetry does not cover (#376): a new BLE
    // peer, and the plain timer. Armed from the node's clock so the first
    // periodic announce is a fixed delay after boot, not after whatever
    // the loop happened to do first.
    let mut announce_gate = crate::announce::AnnounceGate::new(node.now_ms());
    let mut reporter = delivery_hash.map(crate::telemetry::Reporter::new);
    if reporter.is_none() {
        log_critical!("[TELEMETRY] target=00000000 state=off reason=no-delivery-destination");
    }
    if let Some(reporter) = reporter.as_mut() {
        // A persisted target comes back as awaiting-key unless its key
        // was persisted with it; the node then resolves it over the air
        // exactly as it would after a fresh set. The reporter rewrites a
        // hash-only record with the key once it is resolved (#370), so
        // after one successful resolution a reboot restores ready and
        // owes the immediate report.
        if let Some(stored) = crate::telemetry::load(board.config.telemetry_flash_page) {
            reporter.apply_target(&mut node, stored);
        }
        // A persisted fixed position replaces the sensor from the first
        // report of this boot on — the pin must not depend on which
        // record the host set last.
        if let Some(stored) =
            crate::telemetry::load_fixed_position(board.config.telemetry_flash_page)
        {
            reporter.apply_fixed_position(Some(stored));
        }
        reporter.log_banner();
    }
    let telemetry_target_rx = crate::telemetry::inbound_target_receiver();
    let fixed_position_rx = crate::telemetry::inbound_fixed_position_receiver();

    // Calendar seeding from GNSS (Codeberg #166 item 1, #69 on this
    // board): the main loop owns the node, so it is the one place a fix
    // can reach the wall-time seam. One accepted fix seeds; the monotonic
    // clock carries the calendar from there — the receiver keeps running
    // for position only, never as a clock.
    #[cfg(feature = "gnss")]
    let mut gnss_rx = {
        let rx = leviculum_firmware::board_state::GNSS_FIX.receiver();
        if rx.is_none() {
            log_critical!("[TIME_SEED_REFUSED] source=gnss reason=watch_capacity");
        }
        rx
    };
    #[cfg(feature = "gnss")]
    let mut time_seed_gate = leviculum_gnss_time::SeedGate::new();

    // Periodic `[TRANSPORT]` counters (#344). Rides the main loop rather than
    // a spawned task: the counters live in the node this loop owns.
    let mut transport_stats = crate::transport_stats::Ticker::new();

    // The `[HEAP_CENSUS]` line (#388): who holds the heap. Rides the
    // main loop like the `[TRANSPORT]` ticker — the node and the engine
    // it walks are this loop's own state.
    let mut heap_census = crate::heap_census::Ticker::new();

    // The announce bandwidth cap's price list (#402). The core has always had
    // the cap — queue, fewest-hops-first drain, 2 % share — but it only binds
    // an interface somebody registered a bitrate for, and the firmware
    // registered none, so the medium where an announce is DEAREST was the one
    // medium that never held one back. Synced after every wake because the
    // PHY changes under it (the test cells reconfigure the radio per profile);
    // the tracker registers only on a real change.
    let mut announce_cap = crate::lora::AnnounceCap::new();

    log_critical!("[STG] main-loop");
    leviculum_firmware::boot_trace::phase(leviculum_firmware::boot_trace::Phase::MainLoop);

    // One engine pass: digest a dispatch's events, run the async half
    // (validation, flash flushes, periodic jobs), and put whatever the
    // role wants on the wire. A macro because the borrows are the loop's
    // own locals; expanded only after a dispatch, never inside the
    // select, so the engine's awaits cannot be cancelled (the DropBomb
    // rule, `crate::record_store` module docs).
    // An accepted `/dfu` command's reply, parked by the engine pass for the
    // exit at the top of the loop (#235).
    let mut dfu_exit: Option<leviculum_core::transport::TickOutput> = None;
    macro_rules! pn_step {
        ($events:expr) => {
            // The events' own phase until the engine names its steps
            // (order 1665).
            leviculum_firmware::thread_watch::set_phase(
                leviculum_firmware::thread_watch::Phase::CoreEvents,
            );
            // Board-visible core-event lines (LINK_REFUSED, #388) render
            // regardless of whether a propagation role runs.
            crate::events::log_events($events, node.now_ms());
            // #235: answer a `/status` request from an allow-listed identity.
            // In this macro because every arm that produces core events runs
            // it, and a remote query may arrive over any of the three
            // carriers — a responder wired into one arm would answer only the
            // carrier its author had in mind.
            {
                let mut status_out = leviculum_core::transport::TickOutput::empty();
                crate::remote_status::handle_events(&mut node, $events, &mut status_out);
                // #235, OTA stage 1: `/dfu`. A refusal's typed reply rides
                // this dispatch; an accepted command is parked for the exit
                // at the top of the loop, the one place it is expanded.
                crate::remote_dfu::handle_events(
                    &mut node,
                    $events,
                    &mut status_out,
                    &mut dfu_exit,
                );
                if !status_out.actions.is_empty() {
                    let mut ifaces: [&mut dyn Interface; 3] =
                        [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                    let dispatched =
                        dispatch_actions(&mut ifaces, status_out.actions, &ifac_configs);
                    leviculum_firmware::dispatch::settle("remote-status", &mut node, &dispatched);
                }
            }
            if let Some(pn) = pn_engine.as_mut() {
                let mut pn_out = pn.on_events(&mut node, $events);
                pn_out.merge(pn.settle(&mut node).await);
                if !pn_out.actions.is_empty() {
                    let mut ifaces: [&mut dyn Interface; 3] =
                        [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                    let dispatched = dispatch_actions(&mut ifaces, pn_out.actions, &ifac_configs);
                    leviculum_firmware::dispatch::settle("pn", &mut node, &dispatched);
                }
            }
        };
    }
    // Event-driven main loop, ten event sources:
    // 1. Serial incoming (USB)
    // 2. LoRa incoming (radio)
    // 3. BLE incoming (defragmented Reticulum packets from phone)
    // 4. Timer deadline (protocol maintenance, announces)
    // 5. GNSS time candidate (until the calendar is seeded once)
    // 6. Host wall-time injection (#238 control envelope)
    // 7. Host telemetry target (#238 control envelope, #236)
    // 8. Host fixed position (#238 control envelope)
    // 9. Telemetry evaluation tick (only while a target is configured)
    // 10. User button press that announces (#459; never on a board without one)
    loop {
        // #235: an accepted `/dfu` leaves from here: reply handed over and
        // waited for, then the bootloader magic and the reset. Does not
        // return when a command is parked.
        crate::remote_dfu::exit_if_commanded(&mut node, &mut dfu_exit, |node, out| {
            if !out.actions.is_empty() {
                let mut ifaces: [&mut dyn Interface; 3] =
                    [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                let dispatched = dispatch_actions(&mut ifaces, out.actions, &ifac_configs);
                leviculum_firmware::dispatch::settle("remote-dfu", node, &dispatched);
            }
        })
        .await;
        transport_stats.poll(&node);
        // Close a `[DROP]` rate-limit window that expired without a further
        // drop to carry its summary (#346): the silence after a storm is the
        // reading that summary is for, and no event arrives to trigger it.
        crate::events::flush_drop_summary(node.now_ms());
        heap_census.poll(&node, pn_engine.as_ref());
        // Clamped by the stats deadline so the line is still emitted on a
        // channel quiet enough that the node itself has nothing scheduled.
        let deadline = node
            .next_deadline()
            .map(Instant::from_millis)
            .unwrap_or(Instant::MAX)
            .min(transport_stats.deadline())
            .min(heap_census.deadline());
        // The propagation engine's own schedule (queued work, announce,
        // sync scheduler) rides the same timer arm.
        let deadline = match pn_engine.as_ref() {
            Some(pn) => deadline.min(Instant::from_millis(pn.next_deadline_ms(node.now_ms()))),
            None => deadline,
        };

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

        // The periodic announce (#376 item 2). Sleeps exactly to its own
        // deadline rather than polling: an idle board with telemetry off
        // wakes twice an hour for this, and a clockless one once a
        // minute until its clock arrives.
        let announce_tick = async {
            Timer::after(Duration::from_millis(
                announce_gate.periodic_wait_ms(node.now_ms()).max(1),
            ))
            .await
        };

        // Back to the select: what runs from here until an arm is entered is
        // another task's, and a stall or panic in it must not blame the last
        // arm (order 1641).
        leviculum_firmware::thread_watch::set_phase(leviculum_firmware::thread_watch::Phase::Idle);
        let wake = select4(
            select4(
                serial.incoming_rx.receive(),
                lora_channels.incoming_rx.receive(),
                select(
                    ble_channels.incoming_rx.receive(),
                    ble_channels.peer_event_rx.receive(),
                ),
                Timer::at(deadline),
            ),
            gnss_time_candidate,
            select4(
                serial.wall_time_rx.receive(),
                telemetry_target_rx.receive(),
                fixed_position_rx.receive(),
                serial.announce_rx.receive(),
            ),
            select3(
                telemetry_tick,
                announce_tick,
                leviculum_firmware::button::announce_press(),
            ),
        )
        .await;
        // Mirror each interface's is_online() into the core (#365): a
        // path over an offline interface is no path, so a report due
        // while a carrier is off (or peerless, for BLE) falls through
        // to a path request instead of a silent carrier-off drop. After
        // the await, not at the loop top: a `--set-media ble=off` lands
        // in another task while this one sleeps, and the arm below must
        // route with the state as it is NOW.
        node.set_interface_online(0, serial_iface.is_online());
        node.set_interface_online(1, lora_iface.is_online());
        node.set_interface_online(2, ble_iface.is_online());
        // The peer-count sibling of the online mirror (Codeberg #365):
        // marks BLE as a peer-link carrier the transport may
        // re-originate an unanswerable path request on. Serial and LoRa
        // mirror nothing — no "peers" at this layer — and stay at zero.
        node.set_interface_peer_count(2, ble_iface.peer_count());
        // The LoRa PHY's price, mirrored into the announce cap in the same
        // place and for the same reason as the online mirror above: a
        // reconfigure lands in another task while this one sleeps, and the
        // arms below must throttle against the PHY as it is NOW.
        announce_cap.sync(&mut node);
        // The calendar's own healing, mirrored here for the same reason as
        // the state above: it happens in code this task does not run. The
        // seeding arms below record the source they inject, but
        // `learn_emission_timebase` seats an anchor from an overheard
        // announce with nothing at this layer called, so a board healed
        // from traffic kept saying the birth state forever (Codeberg #398).
        // Reading the node is what keeps the banner honest without a
        // notification path per source.
        if set_time_source(node.time_source()) {
            log_critical!("[TIME_SOURCE] source={}", time_source_str());
        }
        // Write the calendar back at most hourly once a live source vouched
        // for it (#166 item 4); the store task does the flash work.
        time_checkpoint.poll(node.anchor_rank(), node.emission_secs(), node.now_ms());
        match wake {
            Either4::Second(unix) => {
                leviculum_firmware::thread_watch::set_phase(
                    leviculum_firmware::thread_watch::Phase::HostCtl,
                );
                // A GNSS fix carrying UTC. The seam applies the same
                // sanity window as every other time source; a refusal is
                // surfaced as a structured event, never swallowed.
                #[cfg(feature = "gnss")]
                {
                    use leviculum_core::transport::TimeSource;
                    // A refusal leaves the gate open for the next fix:
                    // `leviculum_gnss_time::SeedGate::settle`.
                    if time_seed_gate.settle(node.set_wall_time_unix_secs(unix, TimeSource::Gnss)) {
                        set_time_source(TimeSource::Gnss);
                        log_critical!("[TIME_SEED] source=gnss unix={}", unix);
                        log_critical!("[TIME_SOURCE] source={}", time_source_str());
                    } else {
                        log_critical!("[TIME_SEED_REFUSED] source=gnss unix={}", unix);
                    }
                }
                #[cfg(not(feature = "gnss"))]
                let _ = unix;
            }
            Either4::Third(Either4::First(unix_secs)) => {
                leviculum_firmware::thread_watch::set_phase(
                    leviculum_firmware::thread_watch::Phase::HostCtl,
                );
                // A host that knows wall time (#238 TYPE_WALL_TIME). The
                // seam applies the same sanity window as every other time
                // source; the bool picks the enveloped ack or the named
                // refusal — mirror of the GNSS path.
                use leviculum_core::envelope;
                use leviculum_core::transport::TimeSource;
                let answer = if node.set_wall_time_unix_secs(unix_secs, TimeSource::Host) {
                    set_time_source(TimeSource::Host);
                    log_critical!("[TIME_SEED] source=host unix={}", unix_secs);
                    log_critical!("[TIME_SOURCE] source={}", time_source_str());
                    envelope::encode_ack(envelope::TYPE_WALL_TIME)
                } else {
                    log_critical!("[TIME_SEED_REFUSED] source=host unix={}", unix_secs);
                    envelope::encode_refusal(envelope::TYPE_WALL_TIME, envelope::REFUSE_VALUE)
                };
                // Best effort: a full outgoing channel means a busy link;
                // the host's retry covers it.
                let _ = serial_ctl_tx.try_send(answer);
            }
            Either4::Third(Either4::Second(wire)) => {
                leviculum_firmware::thread_watch::set_phase(
                    leviculum_firmware::thread_watch::Phase::HostCtl,
                );
                // A host set or cleared the telemetry target (#236). What
                // happens here is the part that needs the node — the
                // identity lookup that decides ready vs awaiting-key. The
                // persist and the answer both belong to the serial task
                // now: the answer may not go out before the record is on
                // the page (#358), and only the task that requested the
                // save can wait for it.
                use crate::telemetry::TargetOutcome;
                if let Some(reporter) = reporter.as_mut() {
                    match reporter.apply_target(&mut node, wire) {
                        TargetOutcome::Set(_) | TargetOutcome::Cleared => {
                            reporter.log_banner();
                        }
                    }
                }
            }
            Either4::Third(Either4::Third(position)) => {
                leviculum_firmware::thread_watch::set_phase(
                    leviculum_firmware::thread_watch::Phase::HostCtl,
                );
                // A host set or cleared the fixed position. This is the
                // part that needs the reporter — the source switch and the
                // confirmation re-arm; the persist is the serial task's,
                // for the reason above.
                if let Some(reporter) = reporter.as_mut() {
                    reporter.apply_fixed_position(position);
                    match position {
                        Some(p) => log_critical!(
                            "[TELEMETRY] fixed-position set lat_e6={} lon_e6={} alt_e2={} alt_present={}",
                            p.latitude_e6,
                            p.longitude_e6,
                            p.altitude_e2.unwrap_or(0),
                            p.altitude_e2.is_some() as u8
                        ),
                        None => log_critical!("[TELEMETRY] fixed-position cleared"),
                    }
                }
            }
            Either4::Third(Either4::Fourth(())) => {
                leviculum_firmware::thread_watch::set_phase(
                    leviculum_firmware::thread_watch::Phase::Announce,
                );
                // A host asked for an announce now (#376 `TYPE_ANNOUNCE`,
                // `lnflash --announce`, periculum's `announce_board`):
                // every announce this board makes on its own cadence,
                // made now.
                //
                // TWO announces, not one, because a board running the
                // propagation role has two destinations a peer can look
                // up: the `lxmf.delivery` one the telemetry path
                // announces, and the `lxmf.propagation` one the role
                // announces (`crate::pn::Engine::announce_now`).
                // A client that wants to upload needs the SECOND —
                // `Identity.recall` is keyed by destination hash, so
                // knowing the delivery destination tells it nothing
                // about the mailbox — and until this arm sent it, the
                // only way to learn it was to wait out the role's own
                // 300 s interval.
                //
                // Each half keeps its own rule: the delivery announce
                // stays clock-gated (an uptime-stamped announce poisons
                // the path ranking a desk measures), the role's is not
                // (#384 item 6 — a clockless board still announces, and
                // the contact it invites is what delivers the seed). So
                // the answer grades the COMMAND and not either half:
                // ack when at least one announce left the board, the
                // named refusal only when none did. Which one went out
                // is on the board's own `[ANNOUNCE] sent ... reason=`
                // lines — `pn-host` for the role, `host` for delivery.
                use leviculum_core::envelope;
                let pn_announced = match pn_engine.as_mut() {
                    Some(pn) => {
                        let out = pn.announce_now(&mut node, "pn-host");
                        if out.actions.is_empty() {
                            false
                        } else {
                            let mut ifaces: [&mut dyn Interface; 3] =
                                [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                            let dispatched =
                                dispatch_actions(&mut ifaces, out.actions, &ifac_configs);
                            leviculum_firmware::dispatch::settle(
                                "pn-announce-host",
                                &mut node,
                                &dispatched,
                            );
                            true
                        }
                    }
                    None => false,
                };
                let answer = match delivery_hash.as_ref() {
                    None => {
                        // No delivery destination was registered this
                        // boot: nothing exists to announce, and neither a
                        // retry nor the clock can change that.
                        envelope::encode_refusal(
                            envelope::TYPE_ANNOUNCE,
                            envelope::REFUSE_UNSUPPORTED,
                        )
                    }
                    Some(_) if !node.has_plausible_wall_clock() => {
                        // The telemetry path's clock gate: the emission
                        // timestamp inside the announce is what peers
                        // rank paths by, and an uptime-stamped announce
                        // would poison the very path under measurement.
                        log_critical!("[ANNOUNCE] withheld reason=no-clock");
                        envelope::encode_refusal(envelope::TYPE_ANNOUNCE, envelope::REFUSE_NO_CLOCK)
                    }
                    Some(hash) => {
                        let app_data = crate::telemetry::announce_app_data(node.identity());
                        match node.announce_destination(hash, Some(&app_data)) {
                            Ok(out) => {
                                let mut ifaces: [&mut dyn Interface; 3] =
                                    [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                                let dispatched =
                                    dispatch_actions(&mut ifaces, out.actions, &ifac_configs);
                                leviculum_firmware::dispatch::settle(
                                    "announce-host",
                                    &mut node,
                                    &dispatched,
                                );
                                let dh = hash.as_bytes();
                                log_critical!(
                                    "[ANNOUNCE] sent dst={:02x}{:02x}{:02x}{:02x} reason=host",
                                    dh[0],
                                    dh[1],
                                    dh[2],
                                    dh[3]
                                );
                                envelope::encode_ack(envelope::TYPE_ANNOUNCE)
                            }
                            Err(_) => envelope::encode_refusal(
                                envelope::TYPE_ANNOUNCE,
                                envelope::REFUSE_BUSY,
                            ),
                        }
                    }
                };
                // The role's announce is on the air even where the
                // delivery half was withheld or does not exist, and a
                // refusal would report the command as having done
                // nothing.
                let answer = if pn_announced {
                    envelope::encode_ack(envelope::TYPE_ANNOUNCE)
                } else {
                    answer
                };
                // Best effort, like the wall-time answer: a full outgoing
                // channel means a busy link; the host's retry covers it.
                let _ = serial_ctl_tx.try_send(answer);
            }
            Either4::Fourth(Either3::First(())) => {
                leviculum_firmware::thread_watch::set_phase(
                    leviculum_firmware::thread_watch::Phase::Telemetry,
                );
                // Telemetry evaluation (#236). Everything decided here is
                // decided in the policy crate; this arm reads the board's
                // sensors, hands them over, and dispatches whatever came
                // back.
                if let Some(reporter) = reporter.as_mut() {
                    let now_ms = node.now_ms();
                    let (readings, has_fix) = collect_readings(&node, sd);
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
                        leviculum_firmware::dispatch::settle("telemetry", &mut node, &dispatched);
                    }
                }
            }
            Either4::Fourth(Either3::Second(())) => {
                leviculum_firmware::thread_watch::set_phase(
                    leviculum_firmware::thread_watch::Phase::Announce,
                );
                // The periodic announce (#376 item 2). Independent of
                // telemetry: a node that reports rarely, or has no target
                // at all, still has to be findable, and a phone that
                // missed the peer-up announce gets one from here. The gate
                // owns the deadline and the clock rule; this arm dispatches
                // whatever it hands back, which is nothing before the
                // deadline and nothing without a plausible clock.
                let actions = announce_gate.periodic(&mut node, delivery_hash.as_ref());
                if !actions.is_empty() {
                    let mut ifaces: [&mut dyn Interface; 3] =
                        [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                    let dispatched = dispatch_actions(&mut ifaces, actions, &ifac_configs);
                    leviculum_firmware::dispatch::settle(
                        "announce-periodic",
                        &mut node,
                        &dispatched,
                    );
                }
            }
            Either4::Fourth(Either3::Third(())) => {
                leviculum_firmware::thread_watch::set_phase(
                    leviculum_firmware::thread_watch::Phase::Announce,
                );
                // A double press of the user button that got past its
                // debouncer and its 30 s limit (#459, `crate::button`):
                // both announces, booked in the cadence's duty budget. A
                // board without a button never raises this. The air watch
                // around the dispatch plays `announce-sent` once the last
                // LoRa packet it queued is keyed (order 930).
                leviculum_firmware::feedback::announce_press_begin();
                let lora_before = crate::lora::accepted_packets();
                let actions = crate::announce::on_button_press(
                    &mut node,
                    &mut announce_gate,
                    pn_engine.as_mut(),
                    delivery_hash.as_ref(),
                );
                let announced = !actions.is_empty();
                if announced {
                    let mut ifaces: [&mut dyn Interface; 3] =
                        [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                    let dispatched = dispatch_actions(&mut ifaces, actions, &ifac_configs);
                    leviculum_firmware::dispatch::settle("announce-button", &mut node, &dispatched);
                }
                leviculum_firmware::feedback::announce_press_end(
                    lora_before,
                    crate::lora::accepted_packets(),
                    announced,
                    crate::media::lora_active(),
                );
                // Then one immediate position report when a recipient is
                // configured (order 914): armed here, sent by the next
                // telemetry tick through the reporter's own path.
                let now_ms = node.now_ms();
                crate::telemetry::on_button_press(reporter.as_mut(), now_ms);
            }
            Either4::First(Either4::First(data)) => {
                leviculum_firmware::thread_watch::set_phase(
                    leviculum_firmware::thread_watch::Phase::SerRx,
                );
                info!("SER RX {} bytes", data.len());
                let output = node.handle_packet(InterfaceId(0), &data);
                info!("SER RX -> {} actions", output.actions.len());
                // An inbound TELEMETRY_REQUEST may ride any carrier (#371).
                if let Some(reporter) = reporter.as_mut() {
                    let now_ms = node.now_ms();
                    reporter.handle_inbound_events(&node, &output.events, now_ms);
                }
                let mut ifaces: [&mut dyn Interface; 3] =
                    [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                let dispatched = dispatch_actions(&mut ifaces, output.actions, &ifac_configs);
                leviculum_firmware::dispatch::settle("ser-rx", &mut node, &dispatched);
                pn_step!(&output.events);
            }
            Either4::First(Either4::Second(data)) => {
                leviculum_firmware::thread_watch::set_phase(
                    leviculum_firmware::thread_watch::Phase::LoraRx,
                );
                // A medium switched off at runtime stops carrying traffic
                // in BOTH directions from the moment the frame was
                // answered: the interface drops what the core hands it,
                // this drops what the medium hands up. Without the second
                // half a "LoRa off" node would still deliver over LoRa,
                // which is precisely the masking the profile exists to
                // remove.
                if !crate::media::lora_active() {
                    continue;
                }
                let output = node.handle_packet(InterfaceId(1), &data);
                if !output.actions.is_empty() {
                    info!("LORA RX -> {} actions", output.actions.len());
                }
                if let Some(reporter) = reporter.as_mut() {
                    let now_ms = node.now_ms();
                    reporter.handle_inbound_events(&node, &output.events, now_ms);
                }
                let mut ifaces: [&mut dyn Interface; 3] =
                    [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                let dispatched = dispatch_actions(&mut ifaces, output.actions, &ifac_configs);
                leviculum_firmware::dispatch::settle("lora-rx", &mut node, &dispatched);
                pn_step!(&output.events);
            }
            Either4::First(Either4::Third(Either::First(event))) => {
                leviculum_firmware::thread_watch::set_phase(
                    leviculum_firmware::thread_watch::Phase::BleRx,
                );
                // `ble_link::deliver` routes peer transitions onto their
                // own channel; only inbound packets ride this one.
                let BleLinkEvent::Inbound {
                    link: peer,
                    packet: data,
                } = event
                else {
                    continue;
                };
                // #388 census: this packet just left BLE_INCOMING
                // custody (counted by the session that queued it).
                leviculum_firmware::ble_link::incoming_held_sub(data.capacity());
                // The reception itself is already on the log: columba's
                // `BLE: RX <n>B conn=<h> frags=<k>` line names the link
                // and the peer's fragmentation (#376), so a second
                // per-packet line here said less and doubled the noise.
                // See the LoRa arm: a medium switched off at runtime
                // delivers nothing upward either.
                if !crate::media::ble_active() {
                    continue;
                }
                // Name the ingress link when the handshake did (#365):
                // paths learned through it become attributable to the
                // peer when the link dies.
                let output = match peer {
                    Some(peer) => node.handle_packet_from_peer(InterfaceId(2), peer, &data),
                    None => node.handle_packet(InterfaceId(2), &data),
                };
                if !output.actions.is_empty() {
                    info!("BLE RX -> {} actions", output.actions.len());
                }
                if let Some(reporter) = reporter.as_mut() {
                    let now_ms = node.now_ms();
                    reporter.handle_inbound_events(&node, &output.events, now_ms);
                }
                let mut ifaces: [&mut dyn Interface; 3] =
                    [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                let dispatched = dispatch_actions(&mut ifaces, output.actions, &ifac_configs);
                leviculum_firmware::dispatch::settle("ble-rx", &mut node, &dispatched);
                pn_step!(&output.events);
            }
            Either4::First(Either4::Third(Either::Second(event))) => {
                leviculum_firmware::thread_watch::set_phase(
                    leviculum_firmware::thread_watch::Phase::BlePeer,
                );
                // A BLE peer transition (Codeberg #365). Lost: cull the
                // paths whose next hop is that peer so the next report
                // re-resolves over a carrier that can still deliver.
                // Up: pull the peer's delivery path over the fresh link
                // (Columba answers a path request but announces on
                // neither connect nor reconnect). Not gated on
                // media::ble_active — the cull is state cleanup, and a
                // pull for a medium switched off mid-run dies in the
                // interface's carrier-off drop like any other packet.
                let (label, output) = match event {
                    // `ble_link::deliver` routes inbound packets onto
                    // their own channel; only peer transitions ride
                    // this one.
                    BleLinkEvent::Inbound { .. } => continue,
                    BleLinkEvent::PeerLost { identity: peer, .. } => {
                        // Sub-phases (order 1665): all four of the T114's
                        // 3456 B panics died in this arm after a stall of
                        // seconds; these say which step held it.
                        leviculum_firmware::thread_watch::set_phase(
                            leviculum_firmware::thread_watch::Phase::BlePeerCore,
                        );
                        let output = node.handle_interface_peer_lost(InterfaceId(2), peer);
                        info!(
                            "BLE peer lost, {} paths culled",
                            output
                                .events
                                .iter()
                                .filter(|e| matches!(e, NodeEvent::PathLost { .. }))
                                .count()
                        );
                        ("ble-peer-lost", output)
                    }
                    BleLinkEvent::PeerUp { identity: peer, .. } => {
                        leviculum_firmware::thread_watch::set_phase(
                            leviculum_firmware::thread_watch::Phase::BlePeerCore,
                        );
                        let mut output = node.handle_interface_peer_up(InterfaceId(2), peer);
                        info!("BLE peer up, {} pull actions", output.actions.len());
                        // #376: and announce OURSELVES to that peer, on
                        // its link alone. The peer just finished its
                        // identity handshake, so it can receive and it is
                        // exactly the node that does not know us; the pull
                        // above only asks what IT is. A broadcast here
                        // would reach the neighbour board, which forwards
                        // it, and the relayed copy racing the direct one
                        // is the two-hop reading this issue opened with.
                        leviculum_firmware::thread_watch::set_phase(
                            leviculum_firmware::thread_watch::Phase::BlePeerAnnounce,
                        );
                        output.actions.extend(announce_gate.peer_up(
                            &mut node,
                            delivery_hash.as_ref(),
                            peer,
                        ));
                        // And the propagation role (#384): the phone
                        // that just linked is exactly the client that
                        // should learn its board is a mailbox.
                        if let Some(pn) = pn_engine.as_mut() {
                            leviculum_firmware::thread_watch::set_phase(
                                leviculum_firmware::thread_watch::Phase::BlePeerSolicit,
                            );
                            output
                                .actions
                                .extend(pn.on_ble_peer_up(&mut node, 2, peer).actions);
                        }
                        ("ble-peer-up", output)
                    }
                };
                leviculum_firmware::thread_watch::set_phase(
                    leviculum_firmware::thread_watch::Phase::BlePeerDispatch,
                );
                let mut ifaces: [&mut dyn Interface; 3] =
                    [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                let dispatched = dispatch_actions(&mut ifaces, output.actions, &ifac_configs);
                leviculum_firmware::dispatch::settle(label, &mut node, &dispatched);
                pn_step!(&output.events);
            }
            Either4::First(Either4::Fourth(())) => {
                leviculum_firmware::thread_watch::set_phase(
                    leviculum_firmware::thread_watch::Phase::Timer,
                );
                let output = node.handle_timeout();
                if !output.actions.is_empty() {
                    info!("timeout: {} actions", output.actions.len());
                }
                // A receipt timeout surfaces here, not on a reception —
                // the proof wait (#373) needs this arm's events too.
                if let Some(reporter) = reporter.as_mut() {
                    let now_ms = node.now_ms();
                    reporter.handle_inbound_events(&node, &output.events, now_ms);
                }
                let mut ifaces: [&mut dyn Interface; 3] =
                    [&mut serial_iface, &mut lora_iface, &mut ble_iface];
                let dispatched = dispatch_actions(&mut ifaces, output.actions, &ifac_configs);
                leviculum_firmware::dispatch::settle("timeout", &mut node, &dispatched);
                pn_step!(&output.events);
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
/// peripherals exist, by the `gnss` and `battery` features. The T114 has
/// the L76K (#69) and the Pocket V2 the ZOE-M8Q, so a fix contributes
/// position, speed, bearing and the HDOP the accuracy gate reads.
///
/// The battery field is filled from the same ADC task the panel and the
/// `BATTERY` log line read (#380). A pack voltage on the air is the one
/// reading that reaches an operator who is still in the field, which is
/// the situation #380 exists for. One source, one shape on every board.
///
/// The die temperature it does have: every nRF52840 carries one and the
/// SoftDevice is enabled on both boards, so it is read here through the
/// only legal path ([`crate::telemetry::die_temperature_quarter_c`]).
fn collect_readings<R, C, S>(
    node: &leviculum_core::node::NodeCore<R, C, S>,
    sd: &nrf_softdevice::Softdevice,
) -> (crate::telemetry::Readings, bool)
where
    R: rand_core::CryptoRngCore,
    C: leviculum_core::traits::Clock,
    S: leviculum_core::traits::Storage,
{
    // Without the gnss and battery features nothing mutates this; the die
    // temperature is set in the initialiser, so it does not lift the gate.
    #[cfg_attr(
        not(any(feature = "gnss", feature = "battery")),
        allow(unused_mut, clippy::let_and_return)
    )]
    let mut readings = crate::telemetry::Readings {
        // A timebase below the plausibility floor is uptime seconds, not a
        // calendar estimate — the anchor model's "never ahead" rule has
        // nothing to work with there. Which arm anchored it is reported
        // alongside every reading as `[TIME_SOURCE]`.
        unix_secs: node
            .has_plausible_wall_clock()
            .then(|| node.emission_secs()),
        die_temperature_quarter_c: crate::telemetry::die_temperature_quarter_c(sd),
        // Not feature-gated and not board-specific: every board we build
        // has the same SX1262, and what it last heard is the one sensor
        // that describes the mesh rather than the box.
        link: crate::telemetry::link_reading(),
        ..Default::default()
    };
    #[cfg(not(feature = "gnss"))]
    let has_fix = false;
    #[cfg(feature = "gnss")]
    let has_fix = {
        use leviculum_firmware::board_state::{GnssPresence, GNSS_FIX, GNSS_PRESENCE};
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
        // `and_then`, not `map`: the published percentage is itself an
        // Option since #380, and a pack whose voltage its classification
        // cannot explain sends no battery sensor at all rather than a
        // number a collector cannot check.
        readings.battery_percent = leviculum_firmware::board_state::BATTERY_STATE
            .try_get()
            .and_then(|b| b.percent);
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

/// The build banner group (`[FW_BUILD]`, `[TIME_SOURCE]`, `[MEDIA]`,
/// `[NAME ]`, `[IDENTITY]`): every 5 s for the first two minutes after
/// boot, every 5 minutes after that, and at once on a `b` byte on the debug
/// port. Cadence, readers and the on-demand request:
/// `crate::banner` (911).
#[embassy_executor::task]
async fn fw_build_banner(media_src: crate::media::Source) {
    crate::banner::run(media_src).await
}
