//! SX1262 LoRa radio initialization, configuration, and async task for T114.
//!
//! Uses the custom sx1262 driver on SPIM2 (SPIM3 has a MISO read bug on T114).
//! Provides an `Interface` impl for NodeCore dispatch and an async task that
//! handles half-duplex TX/RX on the radio.

extern crate alloc;

use alloc::vec::Vec;
use embassy_embedded_hal::shared_bus::asynch::spi::SpiDevice;
use embassy_futures::select::{select, Either};
use embassy_nrf::gpio::{AnyPin, Input, Level, Output, OutputDrive, Pull};
use embassy_nrf::spim::{self, Spim};
use embassy_nrf::{bind_interrupts, peripherals, Peri};
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_sync::mutex::Mutex;
use leviculum_core::traits::{Interface, InterfaceError};
use leviculum_core::InterfaceId;
use leviculum_queue_budget::{QueueBudget, LORA_QUEUE_BYTES, LORA_QUEUE_SLOTS};
use static_cell::StaticCell;

use crate::sx1262::Sx1262;

/// Cumulative count of LoRa frames successfully transmitted at the radio
/// boundary (one increment per `[LORA] TX done`). Read by the status
/// display tasks (SSD1306 on the Pocket V2, ST7789 on the T114); two
/// always-present atomics cost nothing on display-less builds.
pub static LORA_TX_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Cumulative count of fully reassembled LoRa packets handed off to NodeCore
/// (one increment per `[LORA] RX … bytes` followed by a successful
/// reassembler feed).
pub static LORA_RX_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// The last frame this radio received, for the telemetry reporter's
/// physical-link sensor (Codeberg #236). `None` until the first one.
///
/// A `Cell` behind a critical section rather than packed atomics, for the
/// same reason [`RUNNING_CONFIG`] is one: the four fields are a single
/// observation and must be read as one. Two frames cannot be half-mixed
/// here, and the cost is a critical section on a path that was about to
/// format a log line anyway.
///
/// **Successful receptions only.** A frame that failed its payload CRC is
/// also a measurement, and `sx1262::CrcErrFrame` keeps it for exactly that
/// reason — but it is a measurement of the population that did *not* get
/// through, and reporting it as the link's rssi would mean a board whose
/// link had just collapsed would report the signal of the frames that
/// prove it. The `[LORA] RX err: Crc` line remains where that population
/// is counted.
static LAST_RECEPTION: embassy_sync::blocking_mutex::Mutex<
    CriticalSectionRawMutex,
    core::cell::Cell<Option<leviculum_telemetry_policy::link::Reception>>,
> = embassy_sync::blocking_mutex::Mutex::new(core::cell::Cell::new(None));

/// Record one successful reception's signal figures. Called from the
/// frame sink, once per frame handed up.
///
/// The spreading factor is read here rather than at report time so it is
/// the one this frame was demodulated at: a radio reconfigured in between
/// would otherwise put an old frame's snr on a new PHY's quality scale
/// (`leviculum_telemetry_policy::link::quality_percent`). `0` when the
/// chip has not been configured yet — not reachable on this path, since a
/// frame cannot arrive before the receiver was armed, and it yields no
/// `q` rather than a wrong one if it ever is.
fn note_reception(status: &crate::sx1262::RxStatus, at_ms: u64) {
    let spreading_factor = running_config().map_or(0, |config| config.sf);
    LAST_RECEPTION.lock(|slot| {
        slot.set(Some(leviculum_telemetry_policy::link::Reception {
            rssi_dbm: status.rssi,
            snr_db: status.snr,
            spreading_factor,
            at_ms,
        }))
    });
}

/// The last successful reception, or `None` if this board has not
/// received a frame since boot. What the telemetry reporter applies its
/// freshness bound to.
pub fn last_reception() -> Option<leviculum_telemetry_policy::link::Reception> {
    LAST_RECEPTION.lock(|slot| slot.get())
}

// SPIM2, works on T114 (SPIM3 has a MISO read bug)
bind_interrupts!(pub struct SpiIrqs {
    SPI2 => spim::InterruptHandler<peripherals::SPI2>;
});

type SpiBus = Mutex<NoopRawMutex, Spim<'static>>;
type Spi = SpiDevice<'static, NoopRawMutex, Spim<'static>, Output<'static>>;

/// LoRa radio instance type
pub type Radio = Sx1262<Spi>;

// Channels between LoRa task and main loop
//
// `LORA_INCOMING` keeps its four slots and gets no byte budget (#344). Its
// producer is `incoming_tx.send(…).await`, a *blocking* send: a full incoming
// queue costs the LoRa task a wait, never a dropped packet, so there is no
// silent loss for a bound to make visible. Deepening it would only let the
// radio run further ahead of a main loop that is already the thing to measure
// — which is what the receive-path audit accompanying this batch is about, and
// the wrong end to change before that map is read.
static LORA_INCOMING: Channel<CriticalSectionRawMutex, Vec<u8>, 4> = Channel::new();
static LORA_OUTGOING: Channel<CriticalSectionRawMutex, Vec<u8>, LORA_QUEUE_SLOTS> = Channel::new();
static LORA_CONFIG: Channel<CriticalSectionRawMutex, RadioConfig, 1> = Channel::new();

/// On-air transmit spacing in ms, set from the host (#345,
/// `TYPE_TX_SPACING`). One slot: the value is a level, not an event, and a
/// second one arriving while the first is unread means the task has not
/// reached its next key-up yet — the host is told to retry rather than
/// having a sweep point silently overwritten.
static LORA_TX_SPACING: Channel<CriticalSectionRawMutex, u16, 1> = Channel::new();

/// Hand the LoRa task a new on-air transmit spacing (#345). `false` means
/// one is already queued and unread — the caller answers `REFUSE_BUSY`.
///
/// The knob lives here rather than in [`RadioConfig`] because it is a
/// measurement instrument and not part of the board's stored profile: it
/// is deliberately not persisted, so a reset returns the board to the
/// compiled default and no sweep can outlive the bench session that set
/// it.
pub fn deliver_tx_spacing(spacing_ms: u16) -> bool {
    LORA_TX_SPACING.try_send(spacing_ms).is_ok()
}

/// Occupancy of `LORA_OUTGOING`, in slots and in bytes.
///
/// The channel's own slot bound cannot see bytes, so a queue of twelve MTU
/// packets and a queue of twelve acks look identical to it while differing by
/// 6 KB of a 96 KiB heap. This is the second bound, in the reference's shape
/// (`CONFIG_QUEUE_SIZE`); the channel's `try_send` stays as the backstop.
///
/// Reserved by the single producer (`LoRaInterface::try_send`) before the
/// packet enters the channel, released by the single consumer (`lora_task`)
/// the instant it leaves — so the count is of what is *in the channel*, and a
/// packet already handed to the transmitter is not counted against the queue.
static OUTGOING_BUDGET: QueueBudget = QueueBudget::new(LORA_QUEUE_SLOTS, LORA_QUEUE_BYTES);

/// Take one packet out of the outgoing queue, releasing its budget.
///
/// Every dequeue goes through here or through the `Either3::Second` arm of
/// the idle select; a dequeue that forgets to release would leak the budget
/// until the queue refused everything forever, so there is exactly one
/// non-obvious place to get this right and it is spelled once.
fn take_outgoing(
    outgoing_rx: &Receiver<'static, CriticalSectionRawMutex, Vec<u8>, LORA_QUEUE_SLOTS>,
) -> Option<Vec<u8>> {
    match outgoing_rx.try_receive() {
        Ok(data) => {
            OUTGOING_BUDGET.release(data.len());
            Some(data)
        }
        Err(_) => None,
    }
}

pub struct LoRaChannels {
    pub incoming_rx: Receiver<'static, CriticalSectionRawMutex, Vec<u8>, 4>,
    pub outgoing_tx: Sender<'static, CriticalSectionRawMutex, Vec<u8>, LORA_QUEUE_SLOTS>,
}

pub fn channels() -> LoRaChannels {
    LoRaChannels {
        incoming_rx: LORA_INCOMING.receiver(),
        outgoing_tx: LORA_OUTGOING.sender(),
    }
}

/// Get the sender for runtime radio config overrides (used by serial task).
pub fn config_sender() -> Sender<'static, CriticalSectionRawMutex, RadioConfig, 1> {
    LORA_CONFIG.sender()
}

/// Fired each time a runtime config override has actually been programmed
/// into the radio — the success arm of `configure_lora`, beside the
/// `[LORA] active config:` line. Never fired for a reconfig that failed.
///
/// The LoRa task's half of the applied handshake: the serial task resets
/// this before it delivers a config, then waits on it and compares
/// [`running_config`] against what it delivered, so the answer to the host
/// states a PHY the radio is on rather than one it has merely been
/// promised. A bare event, not the config itself: [`RUNNING_CONFIG`] is
/// already the one authoritative record of what is on the chip, and a
/// second copy travelling through a signal could disagree with it.
static CONFIG_APPLIED: embassy_sync::signal::Signal<CriticalSectionRawMutex, ()> =
    embassy_sync::signal::Signal::new();

/// The applied-config signal, for the serial task's answer wait.
pub fn config_applied() -> &'static embassy_sync::signal::Signal<CriticalSectionRawMutex, ()> {
    &CONFIG_APPLIED
}

/// Index of the LoRa carrier in the node's interface table — the one the
/// binaries name `lora_sx1262`, and the one [`LoRaInterface::id`] answers
/// with. Spelled once so a caller that has to address this interface without
/// holding it (the announce cap, #402) cannot address a different one.
pub const IFACE_INDEX: usize = 1;

// LoRaInterface for NodeCore dispatch
pub struct LoRaInterface {
    sender: Sender<'static, CriticalSectionRawMutex, Vec<u8>, LORA_QUEUE_SLOTS>,
    /// What this interface has thrown away since its carrier went down,
    /// so the drops are logged as a run and not as one line per packet
    /// (see `try_send`).
    drops: leviculum_media_state::DropRun,
}

impl LoRaInterface {
    pub fn new(
        sender: Sender<'static, CriticalSectionRawMutex, Vec<u8>, LORA_QUEUE_SLOTS>,
    ) -> Self {
        Self {
            sender,
            drops: leviculum_media_state::DropRun::new(),
        }
    }
}

impl Interface for LoRaInterface {
    fn id(&self) -> InterfaceId {
        InterfaceId(IFACE_INDEX)
    }
    fn name(&self) -> &str {
        "lora_sx1262"
    }
    fn mtu(&self) -> usize {
        500
    }
    fn is_online(&self) -> bool {
        crate::media::lora_active()
    }
    fn try_send(&mut self, data: &[u8]) -> Result<(), InterfaceError> {
        // The media profile, applied where the medium's quirks belong:
        // the interface knows its carrier is down, the core does not have
        // to. `Ok` and not `BufferFull` — a full buffer is a promise the
        // packet gets another chance, and this one never will, so the
        // driver must not re-queue it forever. Same shape as the
        // `radio_silent` drop the TX path already does, one layer up so
        // the packet is not copied first.
        if !crate::media::lora_active() {
            // Logged as a run rather than per packet: every log line also
            // writes the 2 KiB post-crash tail, and a carrier that is off
            // drops one packet per announce, so per-packet lines empty the
            // tail of the boot and fault diagnostics it exists for — in
            // exactly the single-carrier measurement runs where a crash
            // most needs explaining (#255). Same rule as `dispatch::settle`
            // and `Reporter::note_state`: the transition is the signal.
            if let Some(run) = self.drops.dropped(data.len()) {
                crate::media::log_tx_drop(self.name(), run);
            }
            return Ok(());
        }
        if let Some(run) = self.drops.resumed() {
            crate::media::log_tx_resumed(self.name(), run);
        }
        // Two bounds, the reference's shape (`CONFIG_QUEUE_SIZE` /
        // `CONFIG_QUEUE_MAX_LENGTH`), checked before the packet is copied:
        // whichever binds first refuses, and the copy a refused packet would
        // have needed is not made.
        //
        // Codeberg #344: a full `LORA_OUTGOING` used to be invisible — the
        // caller saw `BufferFull` and (before #344) dropped it, so the most
        // likely place for the board to lose a packet was also the quietest.
        // The four slots this queue had were under a three-second backlog at
        // SF10 (723 ms a frame), which any ordinary burst overran.
        if let Err(bound) = OUTGOING_BUDGET.reserve(data.len()) {
            crate::log::log_fmt(
                "[IFACE_FULL] ",
                format_args!(
                    "iface={} bound={} slots={}/{} bytes={}/{} len={}",
                    self.name(),
                    bound.as_str(),
                    OUTGOING_BUDGET.queued_slots(),
                    OUTGOING_BUDGET.max_slots(),
                    OUTGOING_BUDGET.queued_bytes(),
                    OUTGOING_BUDGET.max_bytes(),
                    data.len()
                ),
            );
            return Err(InterfaceError::BufferFull);
        }
        self.sender.try_send(data.to_vec()).map_err(|_| {
            // Unreachable while the reservation and the channel agree — the
            // slot bound the budget enforces is the channel's own capacity.
            // Handed back rather than asserted: a leaked reservation would
            // close the queue permanently, and the line below says the two
            // disagreed, which is the bug to see.
            OUTGOING_BUDGET.release(data.len());
            crate::log::log_fmt(
                "[IFACE_FULL] ",
                format_args!(
                    "iface={} bound=channel depth={} len={}",
                    self.name(),
                    self.sender.capacity(),
                    data.len()
                ),
            );
            InterfaceError::BufferFull
        })
    }
}

// Radio configuration
// Re-export wire protocol constants from core for use by usb.rs
pub use leviculum_core::rnode::{
    RADIO_CONFIG_ACK as CONFIG_ACK, RADIO_CONFIG_FRAME_LEN as CONFIG_FRAME_LEN,
    RADIO_CONFIG_MAGIC as CONFIG_MAGIC, RADIO_RESET_ACK as RESET_ACK,
    RADIO_RESET_FRAME as RESET_FRAME,
};

pub struct RadioConfig {
    pub frequency_hz: u32,
    pub sf: u8,
    pub bw: u8, // SX1262 bandwidth register code
    pub cr: u8, // SX1262 coding rate register code
    pub tx_power_dbm: i8,
    pub preamble_len: u16,
    pub bw_hz: u32,   // human-readable bandwidth in Hz (for logging)
    pub cr_denom: u8, // human-readable coding rate denominator 5-8 (for logging)
    /// Parsed from the wire and reported back (#349), but no longer
    /// consulted by the transmit path: channel access (acquisition jitter
    /// and CAD listen-before-talk) is unconditional, because the reference
    /// firmware offers no host-visible way to disable its CSMA either
    /// (`tx_queue_handler`, RNode_Firmware.ino:1623) and the flag's
    /// backward-compat default of `false` (absent byte, and the test
    /// runner's unset default) was silently switching all collision
    /// avoidance off — the mechanism behind the
    /// ble_lora_transport-probe-announce-collision ledger.
    pub csma_enabled: bool,
    /// When true, drop every outgoing LoRa packet at the driver boundary.    /// the radio keeps listening but never transmits. Used by the
    /// integration-test runner to neutralize T114s it does not bind, so the
    /// test channel is not polluted by their Reticulum announces.
    pub radio_silent: bool,
    /// Short-term airtime limit, RNode `CMD_ST_ALOCK` u16 encoding
    /// (`percent * 100`). `0` = unlimited. Enforced by the airtime lock.
    pub st_alock: u16,
    /// Long-term airtime limit, RNode `CMD_LT_ALOCK` u16 encoding
    /// (`percent * 100`). `0` = unlimited. Enforced by the airtime lock.
    pub lt_alock: u16,
    /// Whether `lt_alock` came from an explicit host value (new-format radio
    /// config frame) rather than the compiled default. When `false`, a
    /// standalone LNode derives the lawful long-term cap from its own TX
    /// frequency via [`effective_lt_alock`](Self::effective_lt_alock); when
    /// `true`, the host value wins verbatim (including an explicit `0` = off).
    pub lt_alock_present: bool,
}

impl RadioConfig {
    /// EU medium profile — the ReticulumNet NL consensus channel: 869.463
    /// MHz, SF8, BW125, CR4/5, 22 dBm, preamble derived (18).
    pub fn eu_medium() -> Self {
        Self {
            // The ReticulumNet NL consensus channel, verbatim (869463000, not
            // a "corrected" 869462500 — the value of an agreed number is its
            // identity). Inside ERC 70-03 h1.7 (869.4-869.65 MHz, 500 mW
            // e.r.p.); the lower occupied edge at BW125 is 869.4005 MHz, just
            // inside the band, so this channel only fits at BW125.
            frequency_hz: 869_463_000,
            sf: 8,
            bw: 0x04,
            cr: 0x01,
            // The board maximum, not a middle value: a board that boots on its
            // compiled default has nobody to tell it how far it has to reach,
            // and an under-powered LNode has no symptom at the node — only
            // missing range. Same resolution the host applies to an absent
            // `txpower` (`rnode::resolve_tx_power`), and the two are held
            // together by `lnflash`'s
            // `the_default_matches_the_firmwares_compiled_profile`. See the
            // ERP note on `DEFAULT_TX_POWER_DBM`.
            tx_power_dbm: leviculum_core::rnode::DEFAULT_TX_POWER_DBM,
            // Derived, never a fixed number: the RNode firmware picks the
            // preamble from the PHY, and a hardcoded value is how a later SF
            // change ships a mismatched preamble. This profile's SF8/BW125
            // lands on the 18-symbol floor. The arguments are this profile's
            // SF, CR denominator, and bandwidth.
            preamble_len: leviculum_core::rnode::derive_preamble_symbols(8, 5, 125_000),
            bw_hz: 125_000,
            cr_denom: 5,
            csma_enabled: true,
            radio_silent: false,
            st_alock: 0,
            lt_alock: 0,
            // Compiled default: no host ever set an explicit long-term lock, so
            // a standalone LNode derives the ETSI lawful cap from its frequency.
            lt_alock_present: false,
        }
    }

    /// Effective long-term airtime lock (`lt_alock` u16) this config enforces.
    ///
    /// When the host provided an explicit `lt_alock` (new-format frame,
    /// [`lt_alock_present`](Self::lt_alock_present) is `true`) that value wins,
    /// including an explicit `0` = unlimited. Otherwise the firmware derives the
    /// ETSI EU868 lawful default from [`frequency_hz`](Self::frequency_hz) so a
    /// standalone LNode on an EU 863-870 MHz channel is lawful out of the box;
    /// out-of-band frequencies stay off (`0`).
    pub fn effective_lt_alock(&self) -> u16 {
        let explicit = if self.lt_alock_present {
            Some(self.lt_alock)
        } else {
            None
        };
        leviculum_core::rnode::firmware_default_lt_alock(self.frequency_hz as u64, explicit)
    }

    /// Parse a radio config from wire format (13 bytes, after 2-byte magic stripped).
    ///
    /// Returns `None` for invalid data (wrong length, unknown bandwidth).
    pub fn from_wire(data: &[u8]) -> Option<Self> {
        Self::from_wire_config(leviculum_core::rnode::parse_radio_config(data)?)
    }

    /// Build a radio config from an already-parsed wire config.
    ///
    /// Same conversion `from_wire` performs, split out so a config that did
    /// not arrive as wire bytes — the one restored from flash at boot by
    /// [`crate::radio_store::load`] — takes the identical path.
    ///
    /// Returns `None` if the bandwidth has no SX1262 register code.
    pub fn from_wire_config(wire: leviculum_core::rnode::RadioConfigWire) -> Option<Self> {
        // SX1262 bandwidth register codes (datasheet Table 14-47)
        let bw = match wire.bandwidth_hz {
            7_810 => 0x00,
            10_420 => 0x08,
            15_630 => 0x01,
            20_830 => 0x09,
            31_250 => 0x02,
            41_670 => 0x0A,
            62_500 => 0x03,
            125_000 => 0x04,
            250_000 => 0x05,
            500_000 => 0x06,
            _ => return None,
        };

        // CR denominator (5-8) to SX1262 code (1-4)
        let cr = wire.cr - 4;

        Some(Self {
            frequency_hz: wire.frequency_hz,
            sf: wire.sf,
            bw,
            cr,
            tx_power_dbm: wire.tx_power_dbm,
            preamble_len: wire.preamble_len,
            bw_hz: wire.bandwidth_hz,
            cr_denom: wire.cr,
            csma_enabled: wire.csma_enabled,
            radio_silent: wire.radio_silent,
            st_alock: wire.st_alock,
            lt_alock: wire.lt_alock,
            lt_alock_present: wire.lt_alock_present,
        })
    }

    /// The wire form of this config, for the radio report (#349).
    ///
    /// Every field of [`RadioConfigWire`] survives
    /// [`from_wire_config`](Self::from_wire_config), so this is the exact
    /// inverse rather than a reconstruction with gaps: the register codes
    /// `bw` and `cr` are derived from `bw_hz` and `cr_denom`, which are both
    /// kept, and nothing else is transformed at all.
    pub fn to_wire(&self) -> leviculum_core::rnode::RadioConfigWire {
        leviculum_core::rnode::RadioConfigWire {
            frequency_hz: self.frequency_hz,
            bandwidth_hz: self.bw_hz,
            sf: self.sf,
            cr: self.cr_denom,
            tx_power_dbm: self.tx_power_dbm,
            preamble_len: self.preamble_len,
            csma_enabled: self.csma_enabled,
            radio_silent: self.radio_silent,
            st_alock: self.st_alock,
            lt_alock: self.lt_alock,
            lt_alock_present: self.lt_alock_present,
        }
    }
}

/// The settings the radio is running right now, for the radio query (#349).
///
/// `None` until the LoRa task has configured the chip for the first time. The
/// distinction is the point: a query answered from the compiled default, or
/// from the flash page, would describe what the board *would* come up on
/// rather than what it is on. Only a config that survived `configure_lora`
/// is published here, so an answer is always a description of the live
/// hardware.
static RUNNING_CONFIG: embassy_sync::blocking_mutex::Mutex<
    CriticalSectionRawMutex,
    core::cell::Cell<Option<leviculum_core::rnode::RadioConfigWire>>,
> = embassy_sync::blocking_mutex::Mutex::new(core::cell::Cell::new(None));

/// Publish what the radio was just configured with. Called on the success
/// arm of every `configure_lora`, beside the `[LORA] active config` line and
/// for the same reason.
fn publish_running_config(config: &RadioConfig) {
    RUNNING_CONFIG.lock(|slot| slot.set(Some(config.to_wire())));
}

/// The settings the radio is running, or `None` if it has not been
/// configured yet (#349, `TYPE_RADIO_QUERY`).
pub fn running_config() -> Option<leviculum_core::rnode::RadioConfigWire> {
    RUNNING_CONFIG.lock(|slot| slot.get())
}

/// The config most recently accepted by [`LORA_CONFIG`], or `None` before
/// the first one of the boot.
///
/// Written by the serial task after every successful send and read by it
/// when the channel refuses one, for a single question: a full depth-1
/// channel holds exactly the last value that went in, so this says *which*
/// config the LoRa task has not consumed yet. A second copy of that same
/// config is then not a delivery that failed — it is a delivery that already
/// happened, and answering it "undeliverable" describes the channel instead
/// of the radio, and costs the host an attempt it did not need to spend.
///
/// Sound because there is exactly one producer: `usb.rs::apply_radio_config`
/// records here immediately after each send, with no await in between, so
/// while the channel is full the record cannot name a value the channel does
/// not hold. [`RUNNING_CONFIG`] is the other half — what the radio *is* on,
/// against what is still on its way to it.
static PENDING_CONFIG: embassy_sync::blocking_mutex::Mutex<
    CriticalSectionRawMutex,
    core::cell::Cell<Option<leviculum_core::rnode::RadioConfigWire>>,
> = embassy_sync::blocking_mutex::Mutex::new(core::cell::Cell::new(None));

/// Record a config as handed to the LoRa task's channel. See
/// [`PENDING_CONFIG`] for why the serial task is the only caller.
pub fn note_config_delivered(wire: &leviculum_core::rnode::RadioConfigWire) {
    PENDING_CONFIG.lock(|slot| slot.set(Some(*wire)));
}

/// The last config handed to the channel — which, whenever the channel is
/// full, is the one occupying the slot. See [`PENDING_CONFIG`].
pub fn pending_config() -> Option<leviculum_core::rnode::RadioConfigWire> {
    PENDING_CONFIG.lock(|slot| slot.get())
}

/// Keeps the core's announce bandwidth cap told what a frame costs on this
/// board's LoRa carrier (Codeberg #402).
///
/// The cap mechanism has always been there — the queue, the fewest-hops-first
/// drain, the 2 % share — but it only exists for an interface somebody
/// registered a bitrate for, and the firmware registered none. On a host a
/// `bitrate` config key does it; a board has no config file, it has a radio,
/// so the number is derived from the PHY the radio is actually running
/// ([`leviculum_core::rnode::announce_cap_bitrate_bps`], the same airtime
/// arithmetic the duty ledger charges).
///
/// [`sync`](Self::sync) is called from the main loop after every wake, and
/// registers only on a real PHY change: re-registering replaces the cap entry,
/// clearing the holdoff and the queue with it, so a board that re-registered
/// every wake would carry a cap that never caps.
pub struct AnnounceCap {
    tracker: leviculum_core::rnode::AnnounceCapBitrate,
}

impl Default for AnnounceCap {
    fn default() -> Self {
        Self::new()
    }
}

impl AnnounceCap {
    pub const fn new() -> Self {
        Self {
            tracker: leviculum_core::rnode::AnnounceCapBitrate::new(),
        }
    }

    /// Register the live PHY's announce-cap bitrate if it changed, and say so
    /// on the debug port. Does nothing until the LoRa task has configured the
    /// chip once — [`running_config`] describes live hardware or nothing, and
    /// a cap for a radio that never came up would be a number about a fiction.
    pub fn sync<R, C, S>(&mut self, node: &mut leviculum_core::node::NodeCore<R, C, S>)
    where
        R: rand_core::CryptoRngCore,
        C: leviculum_core::traits::Clock,
        S: leviculum_core::traits::Storage,
    {
        let Some(phy) = running_config() else {
            return;
        };
        let Some(bitrate_bps) = self.tracker.sync_phy(&phy) else {
            return;
        };
        // The board's OWN announces bypass the cap above by design, the
        // reference does the same, and #401 rule 5 closes that hole with
        // the cap's own arithmetic: they may use at most a tenth of the
        // lawful duty budget. Both inputs are already here and neither is
        // re-derived — the frame's airtime is the duty ledger's own
        // (`packet_airtime_ms`, the same call `announce_cap_bitrate_bps`
        // makes), and the allowance is the long-term airtime lock this
        // interface resolved from its own TX frequency.
        crate::announce::note_duty_budget(
            node.now_ms(),
            leviculum_core::rnode::packet_airtime_ms(
                leviculum_core::rnode::ANNOUNCE_CAP_REFERENCE_BYTES,
                phy.bandwidth_hz,
                phy.sf,
                phy.cr,
                phy.preamble_len,
            ),
            leviculum_core::rnode::firmware_default_lt_alock(
                phy.frequency_hz as u64,
                phy.lt_alock_present.then_some(phy.lt_alock),
            ),
        );
        node.register_interface_bitrate(IFACE_INDEX, bitrate_bps);
        // What an operator hunting a quiet board needs: the price this board
        // thinks its medium charges, and the silence one announce buys at
        // that price. `holdoff_ms` is the core's own formula
        // (`len * 8 * 1000 / (bitrate * cap%/100)`) at the reference length,
        // so the line and the throttler quote the same arithmetic.
        let cap_pct = leviculum_core::constants::DEFAULT_ANNOUNCE_CAP_PERCENT;
        let holdoff_ms = (leviculum_core::rnode::ANNOUNCE_CAP_REFERENCE_BYTES as u64 * 8 * 1000)
            .checked_div(bitrate_bps as u64 * cap_pct as u64 / 100)
            .unwrap_or(0);
        crate::log::log_fmt_critical(
            "[ANNOUNCE_CAP] ",
            format_args!(
                "iface=lora_sx1262 bitrate_bps={} cap_pct={} holdoff_ms={} ref_bytes={} sf={} bw={} cr={} preamble={}",
                bitrate_bps,
                cap_pct,
                holdoff_ms,
                leviculum_core::rnode::ANNOUNCE_CAP_REFERENCE_BYTES,
                phy.sf,
                phy.bandwidth_hz,
                phy.cr,
                phy.preamble_len
            ),
        );
    }
}

// CSMA/CA constants. The retry gate itself (attempt budget, contention
// window, forced give-up) lives in `leviculum_channel_access`, where a host
// test can drive it against scripted CAD outcomes.
/// Floor for slot time, matches the 24ms slot used by the RNode firmware.
const CSMA_SLOT_MS_MIN: u64 = 24;

/// Peer-turn yield tunable: after this many consecutive empty post-TX ack
/// windows, the sender stops draining its own queue for one bounded RX so the
/// peer gets a guaranteed clear listening window to CSMA-backoff and send its
/// REQ/ACK. A deep outgoing queue (adaptive's large receive window queues many
/// parts) otherwise keeps the sender TXing back-to-back, never reaching the
/// queue-empty continuous-RX branch, so it never hears the peer's REQ and the
/// transfer livelocks (#23 Bug B). 2 back-to-back empties is the smallest
/// signal that the peer is not getting a turn; "current" drains fast and rarely
/// stacks 2, so it is unaffected.
const PEER_YIELD_AFTER_EMPTY: u32 = 2;

/// Anti-livelock ceiling: force a peer-turn yield after this many un-yielded
/// TX frames, even mid-burst. >= the largest window core enqueues so a legit
/// batch is never split by the frame bound; the airtime bound is the real
/// fairness knob. STARTING value, rig-tuned.
const MAX_BURST_FRAMES: u32 = 16;
/// Fairness bound: force a yield once a burst has consumed this much airtime,
/// so the peer gets a clear window before its receiver-side timeout fires.
/// Must sit BELOW the receiver resource timeout (~8-15s at SF10). STARTING
/// value, rig-tuned.
const MAX_BURST_AIRTIME_MS: u64 = 8000;

/// xorshift32 PRNG step. Mutates state and returns the updated value.
fn xorshift32(state: &mut u32) -> u32 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    *state
}

/// Compute CSMA slot time in ms from the current radio profile.
/// `max(24, airtime(500) / 10)`, scales with spreading factor so SF10/SF12
/// don't keep retrying inside the same airtime window.
fn compute_slot_ms(cfg: &RadioConfig) -> u64 {
    let airtime = leviculum_core::rnode::airtime_ms(500, cfg.bw_hz, cfg.sf, cfg.cr_denom);
    core::cmp::max(CSMA_SLOT_MS_MIN, airtime / 10)
}

/// RX listening window (ms) opened after every transmission, before the next
/// outgoing item is drained. Half-duplex turn-taking: a busy transmitter must
/// yield the channel back to RX between sends or it never hears the peer's
/// acks, retransmits, and the link dies via retry exhaustion (#23). The RNode
/// firmware achieves the same by returning to continuous RX after each TX and
/// gating the next TX behind a CSMA wait (DIFS + contention window) during
/// which the radio listens; it even reserves a fixed post-TX yield
/// (CSMA_POST_TX_YIELD_SLOTS). We mirror that discipline with an explicit
/// bounded window.
///
/// Sized to one full single-frame reply airtime at the current profile plus a
/// turnaround margin (peer host processing + its CSMA backoff). `rx_window`
/// returns the instant a packet arrives, so this is only an upper bound that
/// costs wall-clock when the channel is genuinely idle, not on every TX.
fn post_tx_rx_window_ms(cfg: &RadioConfig) -> u32 {
    // One full LoRa frame on the wire (header + max single payload).
    let reply_bytes = (leviculum_core::rnode::MAX_SINGLE_PAYLOAD + 1) as u32;
    let reply_airtime = leviculum_core::rnode::airtime_ms_with_preamble(
        reply_bytes,
        cfg.bw_hz,
        cfg.sf,
        cfg.cr_denom,
        cfg.preamble_len,
    );
    // Peer turnaround: host processing jitter + its DIFS-equivalent (2 slots).
    let turnaround = leviculum_core::rnode::PACING_MARGIN_MS + 2 * compute_slot_ms(cfg);
    // Clamp to >=1ms (the SX1262 needs a non-zero timeout) and a sane ceiling.
    (reply_airtime + turnaround).clamp(1, 10_000) as u32
}

/// Routes [`leviculum_log_line::facts`] onto the firmware's two log sinks.
///
/// The mapping is the whole implementation: the decision about which sink a
/// startup fact belongs on is made — and tested — in `facts`, and this only
/// carries it out.
///
/// `pub(crate)` because one of those facts is stated from inside the SX1262
/// driver, which is the only place that knows what the PA was actually
/// programmed with.
pub(crate) struct FirmwareLog;

impl leviculum_log_line::facts::LineSink for FirmwareLog {
    fn line(
        &mut self,
        route: leviculum_log_line::facts::Route,
        prefix: &str,
        args: core::fmt::Arguments,
    ) {
        match route {
            leviculum_log_line::facts::Route::Critical => {
                crate::log::log_fmt_critical(prefix, args)
            }
            leviculum_log_line::facts::Route::Gated => crate::log::log_fmt(prefix, args),
        }
    }
}

/// The human-readable half of a config, as the `[LORA] active config:` line
/// states it.
///
/// `programmed` is what `configure_lora` put on the SPI bus, and it is a
/// parameter rather than a field of `config` on purpose: the transmit power on
/// this line has to be the byte the PA was given, and the only place that byte
/// exists is the return value of the call that wrote it (Codeberg #349). A
/// config carries a *request*, and reading the power off it is exactly the
/// mistake this line used to make.
fn active_facts(
    config: &RadioConfig,
    programmed: &leviculum_core::sx126x::TxPowerProgram,
) -> leviculum_log_line::facts::ActiveRadioConfig {
    leviculum_log_line::facts::ActiveRadioConfig {
        freq_hz: config.frequency_hz,
        sf: config.sf,
        bw_hz: config.bw_hz,
        cr_denom: config.cr_denom,
        txp_dbm: programmed.programmed_dbm,
        txp_requested_dbm: programmed.requested_dbm,
        csma: config.csma_enabled,
        silent: config.radio_silent,
    }
}

/// Apply a config's airtime limits to the tracker, deriving the lawful
/// long-term cap from the TX frequency when the host set no explicit `lt_alock`
/// (see [`RadioConfig::effective_lt_alock`]), and state both limits and their
/// origins on the boot-critical log path.
///
/// The line is unconditional. It used to be emitted only on the derived path,
/// which made the far more dangerous case — a host that explicitly sent `0`,
/// switching the cap off — the case that produced no line at all: the cap in
/// force had to be inferred from a silence, and a board legitimately unlimited
/// on a shielded bench was indistinguishable from one unlimited in the field.
///
/// Called at radio bring-up and again on every runtime reconfiguration, so a
/// board reconfigured in the field states its new cap too.
fn apply_airtime_limits(airtime: &mut leviculum_core::rnode::AirtimeTracker, config: &RadioConfig) {
    let lt_alock = config.effective_lt_alock();
    airtime.set_st_limit_u16(config.st_alock);
    airtime.set_lt_limit_u16(lt_alock);
    use leviculum_log_line::facts::LimitSource;
    leviculum_log_line::facts::airtime_limits(
        &mut FirmwareLog,
        &leviculum_log_line::facts::AirtimeLimits {
            lt_alock,
            lt_source: if config.lt_alock_present {
                LimitSource::Host
            } else {
                LimitSource::Derived
            },
            st_alock: config.st_alock,
            // `lt_alock` sits *after* `st_alock` on the wire, so a frame that
            // carried an explicit long-term lock provably carried the
            // short-term one too — that much is host-authored for certain.
            // Without it the value may be an old short frame's or the compiled
            // default's, and nothing on hand separates the two, so the line
            // reports the value and declines to name an author.
            st_source: if config.lt_alock_present {
                LimitSource::Host
            } else {
                LimitSource::Config
            },
            freq_hz: config.frequency_hz,
            // What the frequency alone would have given, stated even when it
            // lost, so a reader can weigh the host's choice against the lawful
            // value without a sub-band table.
            lawful_lt_alock: leviculum_core::rnode::firmware_default_lt_alock(
                config.frequency_hz as u64,
                None,
            ),
        },
    );
}

/// Transmit one or two LoRa frames back-to-back. For split packets, both
/// frames go out without any CSMA/CAD between them, the receiver's
/// SplitReassembler expects this.
///
/// Every successfully keyed frame's on-air time is recorded into `airtime`
/// (mirrors the RNode firmware's `add_airtime()` on each `transmit()`), which
/// drives the regulatory airtime lock enforced in `lora_task`.
///
/// This is also where the #345 on-air spacing is applied, and it is applied
/// here rather than at any earlier point on purpose: this function is the
/// last thing between a packet and the radio being keyed, so the gap it
/// enforces is a gap between two packets' *airtimes* — the thing a receiver
/// sees — and not a gap between two hand-overs, which the CSMA/CAD path
/// and the airtime lock would then reshape into something else. Whatever
/// the path spent getting here since the previous packet left the air is
/// counted against the requested gap rather than added to it, so the
/// spacing is the number that appears on the air. The spacing applies per
/// *packet*: the split frames below still go out back-to-back, because the
/// receiver's reassembler requires that.
async fn transmit_all_frames(
    radio: &mut Radio,
    data: &[u8],
    rng_state: &mut u32,
    config: &RadioConfig,
    airtime: &mut leviculum_core::rnode::AirtimeTracker,
    spacer: &mut leviculum_tx_spacing::TxSpacer,
) {
    let wait_ms = spacer.wait_ms(embassy_time::Instant::now().as_millis());
    if wait_ms > 0 {
        embassy_time::Timer::after(embassy_time::Duration::from_millis(wait_ms)).await;
    }
    let tx_start = embassy_time::Instant::now();
    // The achieved gap, measured and not inferred: a sweep reads this off
    // the board instead of assuming the value it set is the value the air
    // saw. `gap_ms=-1` is the first packet since boot, which has no
    // previous airtime edge to be measured from.
    crate::log::log_fmt(
        "[LORA_TX_SPACING] ",
        format_args!(
            "intended_ms={} waited_ms={} gap_ms={}",
            spacer.spacing_ms(),
            wait_ms,
            match spacer.achieved_gap_ms(tx_start.as_millis()) {
                Some(gap) => gap as i64,
                None => -1,
            }
        ),
    );
    let seq_nibble = (xorshift32(rng_state) as u8) & 0xF0;
    let frames = leviculum_core::rnode::build_lora_frames(data, seq_nibble);

    if frames.len() > 1 {
        crate::log::log_fmt(
            "[LORA] ",
            format_args!(
                "TX split {} bytes ({}+{})",
                data.len(),
                frames[0].len() - 1,
                frames[1].len() - 1
            ),
        );
    } else {
        crate::log::log_fmt("[LORA] ", format_args!("TX {} bytes", data.len()));
    }

    let mut tx_ok = true;
    for (i, frame) in frames.iter().enumerate() {
        // Per-frame TX identity, mirrors the RX-side [T114_SX_RX] first8 line so
        // a merged two-board timeline can pair each transmitted frame with the
        // peer's RX event (fate classification). Uses the same bytes (frame[1..],
        // skipping the split-sequence header byte) the peer logs as
        // [T114_SX_RX] first8=, and len= the full on-air frame length.
        {
            let n = frame.len().min(9);
            let mut first8 = [0u8; 8];
            let copy_len = n.saturating_sub(1).min(8);
            first8[..copy_len].copy_from_slice(&frame[1..1 + copy_len]);
            crate::log::log_fmt(
                "[T114_TX_FRAME] ",
                format_args!(
                    "first8={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x} len={}",
                    first8[0],
                    first8[1],
                    first8[2],
                    first8[3],
                    first8[4],
                    first8[5],
                    first8[6],
                    first8[7],
                    frame.len()
                ),
            );
        }
        // Timeout sized from the frame's on-air time at the live modulation
        // (10.69 s for a 184 B announce at SF12/BW125/CR4:8/preamble-18); a
        // fixed 5000 ms here aborted every SF12 frame mid-air.
        let tx_timeout_ms = leviculum_core::sx126x::tx_timeout_ms(
            frame.len() as u32,
            config.bw_hz,
            config.sf,
            config.cr_denom,
            config.preamble_len,
        );
        match radio.transmit(frame, tx_timeout_ms).await {
            Ok(()) => {
                // Record this frame's on-air time (header and programmed
                // preamble included, matching the RNode firmware's
                // `add_airtime(written)`).
                let now_ms = embassy_time::Instant::now().as_millis();
                let cost = leviculum_core::rnode::frame_airtime_cost_ms(
                    frame.len() as u32,
                    config.bw_hz,
                    config.sf,
                    config.cr_denom,
                    config.preamble_len,
                );
                airtime.add_airtime(now_ms, cost);
                // This frame has left the air. Recorded per frame, so for
                // a split packet the next packet's gap is measured from
                // the second frame's end — the last edge on the air.
                spacer.note_tx_end(now_ms);
            }
            Err(e) => {
                crate::log::log_fmt("[LORA] ", format_args!("TX err frame {}: {:?}", i, e));
                tx_ok = false;
                break;
            }
        }
    }
    let tx_ms = tx_start.elapsed().as_millis();
    if tx_ok {
        crate::log::log_fmt("[LORA] ", format_args!("TX done"));
        LORA_TX_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        #[cfg(feature = "display")]
        crate::baseboard::LORA_TX_FLASH.signal(());
    }
    crate::log::log_fmt(
        "[T114_LORA_LOOP] ",
        format_args!("op=tx duration_ms={}", tx_ms),
    );
}

// RX helper
//
// The classification fields of a received Reticulum packet — flags byte,
// destination prefix and context byte — come from
// `leviculum_core::packet::peek_wire_class`, which owns the offsets because
// it owns the layout. This crate cross-compiles and runs no host tests; the
// arithmetic is held by `packet.rs`'s own tests instead.

/// Everything a reception has to pass through on its way to the core, as one
/// [`FrameSink`](leviculum_rx_arming::FrameSink).
///
/// This is the hand-off, and it is what the radio used to wait behind: the
/// last line of `deliver` is a bounded channel send that wakes the main task
/// and yields it the CPU, so `node.handle_packet` — announce signature
/// verification included — runs before this function returns. Everything in
/// it now happens with the receiver already armed.
struct CoreHandoff<'a> {
    /// When the window was armed, for the `op=rx_success duration_ms` line.
    /// Read at the top of `deliver`, i.e. immediately after the re-arm, so
    /// the figure still brackets the reception and not the hand-off behind it.
    rx_start: embassy_time::Instant,
    reassembler: &'a mut leviculum_core::rnode::SplitReassembler,
    incoming_tx: &'a Sender<'static, CriticalSectionRawMutex, Vec<u8>, 4>,
    /// Snapshot of the loop's RX-timeout counter, which is what the split
    /// reassembler ages its partial frames against.
    rx_timeout_count: u32,
}

impl leviculum_rx_arming::FrameSink for CoreHandoff<'_> {
    type Meta = crate::sx1262::RxStatus;

    async fn deliver(&mut self, frame: &[u8], status: &Self::Meta) {
        // Before anything else, and for every frame rather than only the
        // ones that complete a reassembly: what the physical link did is
        // a property of the reception, not of whether the packet above it
        // turned out to be whole.
        note_reception(status, embassy_time::Instant::now().as_millis());
        crate::log::log_fmt(
            "[T114_LORA_LOOP] ",
            format_args!(
                "op=rx_success duration_ms={}",
                self.rx_start.elapsed().as_millis()
            ),
        );
        let len = frame.len();
        let n = len.min(9);
        if n >= 1 {
            let mut first8 = [0u8; 8];
            let copy_len = (n - 1).min(8);
            first8[..copy_len].copy_from_slice(&frame[1..1 + copy_len]);
            crate::log::log_fmt(
                "[T114_SX_RX] ",
                format_args!(
                    "len={} first8={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x} rssi={} snr={}",
                    len,
                    first8[0],
                    first8[1],
                    first8[2],
                    first8[3],
                    first8[4],
                    first8[5],
                    first8[6],
                    first8[7],
                    status.rssi,
                    status.snr
                ),
            );
        }
        if let Some(data) = self.reassembler.feed(frame, self.rx_timeout_count) {
            match leviculum_core::packet::peek_wire_class(&data) {
                // `ctx=` is appended AFTER `dst=` on purpose: every existing
                // consumer of this line (`lnflash --summarize`, the field
                // captures) matches on the `flags=`/`dst=` keys, and
                // appending leaves the shape they grep untouched. It is the
                // byte that separates a relayed announce (`ctx=0x00`) from a
                // path response carrying the same announce (`ctx=0x0b`) —
                // the two have the same `flags=` and the same `dst=`, so
                // until now a capture could not tell them apart at all.
                Some(cls) => crate::log::log_fmt(
                    "[LORA] ",
                    format_args!(
                        "RX {} bytes rssi={} snr={} flags=0x{:02x} dst={:02x}{:02x}{:02x}{:02x} ctx=0x{:02x}",
                        data.len(),
                        status.rssi,
                        status.snr,
                        cls.flags,
                        cls.dest_prefix[0],
                        cls.dest_prefix[1],
                        cls.dest_prefix[2],
                        cls.dest_prefix[3],
                        cls.context
                    ),
                ),
                None => crate::log::log_fmt(
                    "[LORA] ",
                    format_args!(
                        "RX {} bytes rssi={} snr={}",
                        data.len(),
                        status.rssi,
                        status.snr
                    ),
                ),
            }
            LORA_RX_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            #[cfg(feature = "display")]
            crate::baseboard::LORA_RX_FLASH.signal(());
            let plen = data.len();
            let d = data.as_slice();
            let m = d.len().min(8);
            let mut p8 = [0u8; 8];
            p8[..m].copy_from_slice(&d[..m]);
            crate::log::log_fmt(
                "[T114_LORA_DELIVER] ",
                format_args!(
                    "pkt_hash8={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x} len={}",
                    p8[0], p8[1], p8[2], p8[3], p8[4], p8[5], p8[6], p8[7], plen
                ),
            );
            self.incoming_tx.send(data).await;
        } else if len >= 2 && (frame[0] & leviculum_core::rnode::FLAG_SPLIT) != 0 {
            crate::log::log_fmt(
                "[LORA] ",
                format_args!(
                    "RX split part {} bytes seq={} rssi={} snr={}",
                    len - 1,
                    frame[0] >> 4,
                    status.rssi,
                    status.snr
                ),
            );
        } else if len < 2 {
            crate::log::log_fmt("[LORA] ", format_args!("RX too short ({})", len));
        }
    }
}

/// Run one RX window — the given timeout at the given site — with the host's
/// runtime radio config as its second exit. Feeds receptions through the
/// split reassembler and pushes reassembled payloads to `incoming_tx`.
///
/// The radio is armed, awaited, and — on a reception — armed again *before*
/// the frame is handed up; the sequence itself is
/// [`leviculum_rx_arming::receive_and_hand_up`], where a fake radio asserts
/// it. What this function keeps is everything specific to the board: the
/// logging, the reassembler, and the classification of the three RX errors.
///
/// `window` is the `(timeout_ms, site)` pair the driver puts in the
/// `[SX_RX_ARM]` line. Every caller below passes a distinct site, so a
/// capture says which of the loop's windows was listening without anybody
/// matching timeouts against source lines.
///
/// # Why the config is an arm of *every* window
///
/// A window's other exits are a reception and its own timeout, and the
/// loop's windows are as long as the PHY is slow: the peer-turn yield is two
/// post-TX windows, 20 s at SF12/125 kHz, while the serial task waits 1.2 s
/// (`usb.rs::CONFIG_APPLY_WITHIN`) for the apply before it answers the host
/// busy. A config pushed into a window the loop does not wake from is
/// therefore answered busy by construction, and the host's next attempt
/// finds the one-slot channel still holding the first — which is how
/// `lora_path_discovery_wide_mixed` went `SKIPPED_INFRA
/// reason=lnode_radio_config_failed result=no_ack_after_3` on T114 DEC9947D,
/// 2026-09-23 08:03 UTC, out of a `site=yield` window armed for 20 s.
/// f4ecf16ab gave the config an arm of the idle select and of nothing else;
/// this is that mechanism at every window the loop has, because "the one
/// window nobody covered" does not show up in a capture until a cell is
/// already lost.
///
/// The wake does not *consume* the config: the arm is `ready_to_receive`, so
/// the value stays in the channel and [`apply_runtime_config`] keeps its
/// single caller at the top of the turn. Every window here therefore ends the
/// same way — back in the loop, with the config still queued.
///
/// A window entered with a config already waiting is not armed at all, and
/// that is what holds the deferral below to one per push: the first window to
/// see the config stands down through `disarm_rx_for_tx`, which may wait out
/// a frame that is measurably mid-air, and every later window of the same
/// turn returns here without touching the radio.
async fn rx_window(
    radio: &mut Radio,
    rx_buf: &mut [u8; 255],
    window: (u32, leviculum_core::sx126x::RxSite),
    reassembler: &mut leviculum_core::rnode::SplitReassembler,
    incoming_tx: &Sender<'static, CriticalSectionRawMutex, Vec<u8>, 4>,
    rx_timeout_count: &mut u32,
    config_rx: &Receiver<'static, CriticalSectionRawMutex, RadioConfig, 1>,
) -> bool {
    let (timeout_ms, site) = window;
    if !config_rx.is_empty() {
        // Nothing to listen for that the turn is not about to be interrupted
        // for anyway. Reported, because a capture that shows a window's log
        // line without its `[SX_RX_ARM]` should say why.
        crate::log::log_fmt(
            "[T114_LORA_LOOP] ",
            format_args!("op=rx_config_pending site={}", site.tag()),
        );
        return false;
    }
    let rx_start = embassy_time::Instant::now();
    let mut sink = CoreHandoff {
        rx_start,
        reassembler,
        incoming_tx,
        rx_timeout_count: *rx_timeout_count,
    };
    // Bound to a local so both futures are dropped — and the radio and the
    // sink released — before the config arm below reaches for them again.
    let exit = select(
        leviculum_rx_arming::receive_and_hand_up(radio, rx_buf, (timeout_ms, site), &mut sink),
        config_rx.ready_to_receive(),
    )
    .await;
    let rx_ms = rx_start.elapsed().as_millis();
    let rx_result = match exit {
        Either::First(rx_result) => rx_result,
        // The host pushed a radio config. The RX future was dropped, so the
        // receiver is stood down the same way the idle select's outgoing arm
        // stands it down: `disarm_rx_for_tx` holds the teardown for a frame
        // that is measurably mid-air — bounded at one maximum-size frame's
        // airtime at the live modulation — and hands it up through this same
        // sink, instead of ending a reception in progress for a retune that
        // can afford to wait that bound. An idle window costs the plain
        // standby it always did. `configure_lora`'s own teardown then finds
        // nothing standing.
        Either::Second(()) => {
            let _ = radio
                .disarm_rx_for_tx(
                    leviculum_core::sx126x::RxTeardownBy::Config,
                    leviculum_rx_arming::DeferPolicy::OneFrame,
                    rx_buf,
                    &mut sink,
                )
                .await;
            crate::log::log_fmt(
                "[T114_LORA_LOOP] ",
                format_args!("op=rx_config site={} duration_ms={}", site.tag(), rx_ms),
            );
            // Not a reception: a frame the teardown caught went up through
            // the sink on its own, and what the callers count here is windows
            // that ran their course.
            return false;
        }
    };
    match rx_result {
        Ok(reception) => {
            // The re-arm that covers the hand-off is not allowed to cost the
            // frame, so its failure is reported rather than propagated. The
            // next window arms from scratch: the arming state still owes a
            // standby, so nothing is left half-armed.
            if let Err(e) = reception.rearm {
                crate::log::log_fmt("[SX_RX_REARM] ", format_args!("failed error={:?}", e));
            }
            // A packet was received this window (full delivery, split part, or
            // runt). The caller uses this to reset its consecutive-empty-ack
            // counter: any reception means the peer is being heard.
            true
        }
        Err(crate::sx1262::Error::Timeout) => {
            *rx_timeout_count = rx_timeout_count.wrapping_add(1);
            crate::log::log_fmt("[T114_SX_TIMEOUT] ", format_args!(""));
            crate::log::log_fmt(
                "[T114_LORA_LOOP] ",
                format_args!("op=rx_timeout duration_ms={}", rx_ms),
            );
            if rx_timeout_count.is_multiple_of(60) {
                crate::log::log_fmt("[LORA] ", format_args!("RX idle ({})", *rx_timeout_count));
            }
            // Window expired with no reception.
            false
        }
        Err(e) => {
            crate::log::log_fmt("[T114_SX_ERR] ", format_args!("error={:?}", e));
            crate::log::log_fmt(
                "[T114_LORA_LOOP] ",
                format_args!("op=rx_err duration_ms={}", rx_ms),
            );
            // A payload-CRC failure is the only RX error that carries a
            // measurement, and it is the one we need numbers on: the frames
            // that fail are exactly the frames a lossy link is made of
            // (Codeberg #258). `len` comes out of the explicit header, which
            // passed its own CRC, so it is the length the transmitter meant.
            //
            // There is no frequency-error field: the SX1262 exposes no FEI
            // readout (see `sx1262::CrcErrFrame`), unlike the SX127x, so this
            // line carries what the chip actually reports and nothing shaped
            // to look like more.
            match &e {
                crate::sx1262::Error::Crc(frame) => crate::log::log_fmt(
                    "[LORA] ",
                    format_args!(
                        "RX err: Crc len={} rssi={} snr={}",
                        frame.len, frame.rssi, frame.snr
                    ),
                ),
                _ => crate::log::log_fmt("[LORA] ", format_args!("RX err: {:?}", e)),
            }
            // Radio error, treat as no reception.
            false
        }
    }
}

// Init
//
// Board-agnostic: GPIO pins arrive as AnyPin (degraded in the bin file)
// and the SX1262-specific knobs (SPI clock, TCXO voltage) come from
// `BoardConfig`. SPI peripheral is still typed because embassy-nrf does
// not expose an erased-instance Spim. SPI2 is used on both T114 (SPI3
// has a MISO read bug there) and RAK4631 — same instance keeps the
// shared Spim<'static> type stable.
//
// 10 parameters: the radio's full pin/bus/board wiring arrives here once
// at boot. A grouping struct would only move the same ten names one file
// up; every caller is a board bring-up that lists them all anyway.
#[allow(clippy::too_many_arguments)]
pub async fn init(
    spi_periph: Peri<'static, peripherals::SPI2>,
    sck: Peri<'static, AnyPin>,
    mosi: Peri<'static, AnyPin>,
    miso: Peri<'static, AnyPin>,
    cs: Peri<'static, AnyPin>,
    reset: Peri<'static, AnyPin>,
    busy: Peri<'static, AnyPin>,
    dio1: Peri<'static, AnyPin>,
    // The board's external RX-enable line, or `None` where DIO2 owns the
    // whole antenna switch (T114, RAK4631). Passed through to the driver,
    // which is where the switching happens — see `crate::boards` for why
    // the front end is not one boolean, and `boards/solarnode.rs` for the
    // board that needs it.
    rx_enable: Option<Peri<'static, AnyPin>>,
    spi_freq: spim::Frequency,
    tcxo_voltage_reg: u8,
) -> Radio {
    let mut spi_config = spim::Config::default();
    spi_config.frequency = spi_freq;

    let spi = Spim::new(spi_periph, SpiIrqs, sck, miso, mosi, spi_config);

    static SPI_BUS: StaticCell<SpiBus> = StaticCell::new();
    let spi_bus = SPI_BUS.init(Mutex::new(spi));

    let cs_pin = Output::new(cs, Level::High, OutputDrive::Standard);
    let spi_device = SpiDevice::new(spi_bus, cs_pin);

    let reset_pin = Output::new(reset, Level::High, OutputDrive::Standard);
    let busy_pin = Input::new(busy, Pull::None);
    let dio1_pin = Input::new(dio1, Pull::Down);
    // Released at construction: the chip is not listening yet, and the very
    // first thing the LoRa task does to it is a reset.
    let rx_enable_pin = rx_enable.map(|pin| Output::new(pin, Level::Low, OutputDrive::Standard));

    Sx1262::new(
        spi_device,
        reset_pin,
        busy_pin,
        dio1_pin,
        tcxo_voltage_reg,
        rx_enable_pin,
    )
}

/// One word of per-board, per-boot entropy for [`lora_task`]'s
/// `channel_seed`.
///
/// Every board binary draws its seed through this one function instead of
/// inlining the RNG call, so the question Codeberg #268 was filed about —
/// *what seeds the channel-access randomness?* — has exactly one answer to
/// read, and a board added later inherits it rather than re-deciding it.
/// Back then the answer was a compile-time constant and every board walked
/// the same jitter draw and the same CAD backoff ladder.
///
/// The hardware RNG is the source because it is the only per-board, per-boot
/// entropy reachable here. It is reachable on both sides of BLE bring-up:
/// [`crate::rng::RawHwRng`] goes through the SoftDevice syscall once the SD
/// is enabled and falls back to direct register access while it is off, and
/// the board binaries call this from `main` before `Softdevice::enable`.
///
/// A board's own sequence stays reproducible from its seed, so a host test
/// drives the same decisions the firmware makes (see
/// `leviculum_channel_access`); only the seed differs between boards.
pub fn channel_seed() -> u32 {
    use rand_core::RngCore as _;
    crate::rng::RawHwRng::new().next_u32()
}

/// Program a runtime config override into the radio and refresh every piece
/// of loop state derived from it: the slot time, the channel-access PHY, the
/// airtime limits, the published running config.
///
/// One caller, the loop's single intake at the top of the turn. Every window
/// the loop can park in wakes on a queued config without consuming it
/// (see [`rx_window`]), so waking and applying are separate concerns and the
/// set of derived state has one place it can be rewritten from.
/// [`CONFIG_APPLIED`] fires only on the success arm: a reconfig that failed
/// left the radio on the old PHY, and the serial task's answer to the host
/// must not read as applied.
async fn apply_runtime_config(
    radio: &mut Radio,
    new_cfg: RadioConfig,
    config: &mut RadioConfig,
    slot_ms: &mut u64,
    access: &mut leviculum_channel_access::ChannelAccess,
    airtime: &mut leviculum_core::rnode::AirtimeTracker,
) {
    match radio
        .configure_lora(
            new_cfg.frequency_hz,
            new_cfg.sf,
            new_cfg.bw,
            new_cfg.cr,
            new_cfg.tx_power_dbm,
            new_cfg.preamble_len,
        )
        .await
    {
        Ok(programmed) => {
            leviculum_log_line::facts::active_radio_config(
                &mut FirmwareLog,
                &active_facts(&new_cfg, &programmed),
            );
            *config = new_cfg;
            publish_running_config(config);
            *slot_ms = compute_slot_ms(config);
            access.set_phy(config.bw_hz, config.sf, config.cr_denom);
            apply_airtime_limits(airtime, config);
            CONFIG_APPLIED.signal(());
        }
        Err(e) => crate::log::log_fmt("[LORA] ", format_args!("reconfig FAILED: {:?}", e)),
    }
}

/// What the task does with a packet the outgoing queue has just handed it:
/// either it becomes the packet being transmitted, or the host's mute
/// (`RadioConfig::radio_silent`) swallows it here. Returns whether it
/// became `pending_tx`.
///
/// One function for all three dequeue sites (the pre-TX pick-up, the burst
/// continuation, and the idle select's outgoing arm), because the mute is
/// the one outcome of this decision that produces no other trace. A frame
/// that is kept never reaches the acquisition jitter, the CAD, the CSMA
/// verdict or the airtime lock, each of which logs; the three sites used to
/// spell `drop(data)` and say nothing, so a muted board produced a capture
/// in which frames entered the queue, were dequeued, and vanished.
///
/// Codeberg #410: that is how the rig came to stand mute for seven hours
/// after the 2026-09-15 corpus, with `[MEDIA] lora=on`, a live receive loop
/// and a climbing transport counter — the diagnosis had to be made from
/// which lines were *absent*. The drop run is counted the way
/// `media::log_tx_drop`'s is and for the same reason: every line also writes
/// the 2 KiB post-crash tail, and a muted board drops a frame per announce.
fn admit_for_transmit(
    data: Vec<u8>,
    config: &RadioConfig,
    muted: &mut leviculum_media_state::DropRun,
    pending_tx: &mut Option<Vec<u8>>,
    access: &mut leviculum_channel_access::ChannelAccess,
) -> bool {
    if config.radio_silent {
        if let Some(run) = muted.dropped(data.len()) {
            leviculum_log_line::facts::lora_tx_muted(
                &mut FirmwareLog,
                &leviculum_log_line::facts::MuteRun {
                    packets: run.packets,
                    bytes: run.bytes,
                },
            );
        }
        return false;
    }
    if let Some(run) = muted.resumed() {
        leviculum_log_line::facts::lora_tx_unmuted(
            &mut FirmwareLog,
            &leviculum_log_line::facts::MuteRun {
                packets: run.packets,
                bytes: run.bytes,
            },
        );
    }
    *pending_tx = Some(data);
    access.begin_packet();
    true
}

// LoRa async task
//
// `channel_seed` feeds the channel-access randomness (acquisition jitter,
// CAD backoff, split-frame sequence nibbles) and must come from per-board
// entropy: two boards seeded identically draw identical jitter at identical
// draw counts, which re-creates exactly the phase lock the jitter exists to
// break. The previous fixed `0xDEAD_BEEF` seed did precisely that.
#[embassy_executor::task]
pub async fn lora_task(mut radio: Radio, mut config: RadioConfig, channel_seed: u32) {
    let outgoing_rx = LORA_OUTGOING.receiver();
    let incoming_tx = LORA_INCOMING.sender();
    let config_rx = LORA_CONFIG.receiver();
    let spacing_rx = LORA_TX_SPACING.receiver();

    // Init radio
    radio.reset().await;
    let _ = radio.wait_busy().await;

    match radio.init_radio(config.frequency_hz).await {
        Ok(s) => crate::log::log_fmt("[LORA] ", format_args!("init ok, status=0x{:02X}", s.raw)),
        Err(e) => {
            crate::log::log_fmt("[LORA] ", format_args!("init FAILED: {:?}", e));
            return; // Can't continue without radio
        }
    }

    match radio
        .configure_lora(
            config.frequency_hz,
            config.sf,
            config.bw,
            config.cr,
            config.tx_power_dbm,
            config.preamble_len,
        )
        .await
    {
        Ok(programmed) => {
            leviculum_log_line::facts::active_radio_config(
                &mut FirmwareLog,
                &active_facts(&config, &programmed),
            );
            publish_running_config(&config);
        }
        Err(e) => {
            crate::log::log_fmt("[LORA] ", format_args!("configure FAILED: {:?}", e));
            return;
        }
    }

    crate::log::log_fmt("[LORA] ", format_args!("task started"));

    let mut rx_buf = [0u8; 255];
    let mut rx_timeout_count: u32 = 0;
    // Split-frame sequence nibbles. Derived from the per-board seed (the
    // xor keeps it from replaying the channel-access stream); the zero
    // guard is xorshift32's fixed point.
    let mut rng_state: u32 = match channel_seed ^ 0xDEAD_BEEF {
        0 => 0xDEAD_BEEF,
        s => s,
    };
    let mut reassembler = leviculum_core::rnode::SplitReassembler::new();

    // Channel access: the seeded acquisition jitter and the CAD retry
    // gate. Unconditional for every key-up — this interface knows its
    // carrier is a shared half-duplex channel, and whether it talks over
    // a peer is not a host policy (the host `csma_enabled` flag is
    // parsed and reported, no longer obeyed; see `RadioConfig`).
    let mut access = leviculum_channel_access::ChannelAccess::new(channel_seed);
    access.set_phy(config.bw_hz, config.sf, config.cr_denom);

    let mut pending_tx: Option<Vec<u8>> = None;
    // Frames the host's mute has swallowed since it was set (#410), so the
    // suppression is logged as a run and not as one line per frame. See
    // `admit_for_transmit`.
    let mut muted = leviculum_media_state::DropRun::new();
    let mut slot_ms: u64 = compute_slot_ms(&config);
    // Count of consecutive post-TX ack windows that expired with no reception.
    // Drives the peer-turn yield (see PEER_YIELD_AFTER_EMPTY). Reset to 0 on any
    // reception, anywhere rx_window returns true.
    let mut consecutive_empty_acks: u32 = 0;
    // Bounded-burst accounting: TX frames and airtime since the last channel
    // yield (post-TX ack window). Reset whenever a yield actually runs.
    let mut frames_since_yield: u32 = 0;
    let mut airtime_since_yield_ms: u64 = 0;

    // Regulatory airtime lock (mirrors the RNode firmware). The bin histogram
    // lives in the task's static future, off the heap (960 bytes). Limits of 0
    // mean unlimited, so unconfigured devices and tests are never throttled.
    let mut airtime = leviculum_core::rnode::AirtimeTracker::new();
    apply_airtime_limits(&mut airtime, &config);

    // On-air spacing knob (#345). Starts at the compiled default, which
    // imposes nothing, so an unconfigured board transmits exactly as it did
    // before the knob existed.
    let mut spacer =
        leviculum_tx_spacing::TxSpacer::new(leviculum_tx_spacing::DEFAULT_TX_SPACING_MS);

    loop {
        // Take a new on-air spacing before anything is keyed this
        // iteration, so a value the host set is in force for the very next
        // transmission rather than the one after it.
        if let Ok(spacing_ms) = spacing_rx.try_receive() {
            spacer.set_spacing_ms(spacing_ms);
            crate::log::log_fmt(
                "[LORA_TX_SPACING] ",
                format_args!("set intended_ms={}", spacing_ms),
            );
        }

        // Check for a runtime radio config override. The loop's only
        // intake, and it serves every turn: a config arriving mid-window
        // wakes that window (`rx_window`) without being taken out of the
        // channel, so whichever window the loop was parked in — the 20 s
        // peer-turn yield as much as the idle listen that can park for a
        // minute — the value is still here when the turn comes back around.
        if let Ok(new_cfg) = config_rx.try_receive() {
            apply_runtime_config(
                &mut radio,
                new_cfg,
                &mut config,
                &mut slot_ms,
                &mut access,
                &mut airtime,
            )
            .await;
        }

        // Pick up a new packet to send if no TX is in flight. When
        // `radio_silent` is set, drop everything the stack hands us instead
        // of starting a TX, the radio stays listening but never transmits.
        // Used to keep unused test T114s from polluting the LoRa channel
        // with their own Reticulum announces.
        if pending_tx.is_none() {
            if let Some(data) = take_outgoing(&outgoing_rx) {
                admit_for_transmit(data, &config, &mut muted, &mut pending_tx, &mut access);
            }
        }

        if let Some(data) = pending_tx.as_ref() {
            // Regulatory airtime lock: recompute short/long-term airtime and, if
            // over the configured limit, hold this queued frame instead of
            // keying the radio (mirrors the RNode firmware's
            // `if (!airtime_lock && queue_height > 0)` TX gate). We keep
            // listening during the hold so RX is not starved, then retry.
            let now_ms = embassy_time::Instant::now().as_millis();
            airtime.update(now_ms);
            if airtime.is_locked() {
                // Integer milliseconds, not the fractions: the tracker's own
                // ledger unit, and the only float formatting on the LoRa
                // path — printing the f32 fractions here was what linked
                // core's flt2dec into the image.
                crate::log::log_fmt(
                    "[LORA_AIRTIME_LOCK] ",
                    format_args!(
                        "st={} lt={} holding",
                        airtime.short_term_airtime_ms(),
                        airtime.long_term_airtime_ms()
                    ),
                );
                let hold_ms = post_tx_rx_window_ms(&config);
                reassembler.check_timeout(rx_timeout_count, 10);
                if rx_window(
                    &mut radio,
                    &mut rx_buf,
                    (hold_ms, leviculum_core::sx126x::RxSite::Hold),
                    &mut reassembler,
                    &incoming_tx,
                    &mut rx_timeout_count,
                    &config_rx,
                )
                .await
                {
                    consecutive_empty_acks = 0;
                }
                continue;
            }

            // Whole-packet on-air cost (all split frames) for the burst
            // airtime bound, computed while the packet is still borrowed.
            let tx_cost_ms = leviculum_core::rnode::packet_airtime_ms(
                data.len(),
                config.bw_hz,
                config.sf,
                config.cr_denom,
                config.preamble_len,
            );

            // Acquisition jitter: the first key-up after a channel release
            // waits a randomised, listened-through window (reference band-1
            // draw, see leviculum_channel_access) before it probes. CAD
            // alone cannot de-tile two senders whose transmissions share a
            // trigger — co-started probe announces, rebroadcasts of the
            // same received frame — because both probe a channel neither
            // has keyed yet. Spent in `rx_window`, so a peer that keys inside
            // the window is received, not talked over; burst continuations
            // (same acquisition) owe nothing and skip this entirely.
            //
            // `rx_window` returns the instant it receives, which is the
            // common case here — the window is most often opened right
            // after hearing something. The part of the draw that did not
            // get listened through is still owed: the frame that cut the
            // window short released every other waiting node at the same
            // instant, so keying up on its heels is precisely the phase
            // lock the draw exists to break. Hence the elapsed report and
            // the `continue` back into the remainder, rather than falling
            // through to CAD. Wall-clock elapsed (floor 1 ms so a radio
            // erroring out instantly still drains the debt) bounds the
            // resumes at the drawn window.
            let jitter_ms = access.acquisition_jitter_ms();
            if jitter_ms > 0 {
                crate::log::log_fmt(
                    if access.jitter_was_drawn() {
                        "[LORA_JITTER] "
                    } else {
                        "[LORA_JITTER_RESUME] "
                    },
                    format_args!("wait_ms={} slot_ms={}", jitter_ms, access.jitter_slot()),
                );
                let rx_ms = jitter_ms.clamp(1, 10_000) as u32;
                reassembler.check_timeout(rx_timeout_count, 10);
                let jitter_start = embassy_time::Instant::now();
                if rx_window(
                    &mut radio,
                    &mut rx_buf,
                    (rx_ms, leviculum_core::sx126x::RxSite::Jitter),
                    &mut reassembler,
                    &incoming_tx,
                    &mut rx_timeout_count,
                    &config_rx,
                )
                .await
                {
                    consecutive_empty_acks = 0;
                }
                access.jitter_spent(jitter_start.elapsed().as_millis().max(1));
                continue;
            }

            // The carrier-detect's own teardown of a standing window may have a
            // frame in it, and that frame goes up through the loop's sink like
            // any other (Codeberg #426). `rx_start` is taken before the
            // detection so the `op=rx_success duration_ms` a caught frame
            // reports brackets the wait rather than nothing.
            let mut cad_sink = CoreHandoff {
                rx_start: embassy_time::Instant::now(),
                reassembler: &mut reassembler,
                incoming_tx: &incoming_tx,
                rx_timeout_count,
            };
            let cad = radio.cad(config.sf, &mut rx_buf, &mut cad_sink).await;
            match cad {
                Ok(false) => {
                    // Channel clear, send the whole packet (both split
                    // frames back-to-back, no CAD between them).
                    crate::log::log_fmt(
                        "[LORA_CAD] ",
                        format_args!("busy=false attempt={}", access.retries()),
                    );
                    crate::log::log_fmt(
                        "[LORA_CSMA_TX] ",
                        format_args!(
                            "retries={} forced=false slot_ms={}",
                            access.retries(),
                            slot_ms
                        ),
                    );
                    transmit_all_frames(
                        &mut radio,
                        data,
                        &mut rng_state,
                        &config,
                        &mut airtime,
                        &mut spacer,
                    )
                    .await;
                    pending_tx = None;
                    frames_since_yield += 1;
                    airtime_since_yield_ms += tx_cost_ms;
                }
                Ok(true) => {
                    crate::log::log_fmt(
                        "[LORA_CAD] ",
                        format_args!("busy=true attempt={}", access.retries()),
                    );
                    match access.cad_busy() {
                        leviculum_channel_access::Verdict::Transmit { retries, .. } => {
                            crate::log::log_fmt(
                                "[LORA_CSMA_TX] ",
                                format_args!("retries={} forced=true slot_ms={}", retries, slot_ms),
                            );
                            transmit_all_frames(
                                &mut radio,
                                data,
                                &mut rng_state,
                                &config,
                                &mut airtime,
                                &mut spacer,
                            )
                            .await;
                            pending_tx = None;
                            frames_since_yield += 1;
                            airtime_since_yield_ms += tx_cost_ms;
                        }
                        leviculum_channel_access::Verdict::Backoff { slots } => {
                            let backoff_ms = slots * slot_ms;
                            // RX during the backoff so incoming packets aren't lost.
                            // Clamp to >=1ms, the SX1262 needs a non-zero timeout.
                            let rx_ms = backoff_ms.clamp(1, 10_000) as u32;
                            reassembler.check_timeout(rx_timeout_count, 10);
                            if rx_window(
                                &mut radio,
                                &mut rx_buf,
                                (rx_ms, leviculum_core::sx126x::RxSite::Csma),
                                &mut reassembler,
                                &incoming_tx,
                                &mut rx_timeout_count,
                                &config_rx,
                            )
                            .await
                            {
                                consecutive_empty_acks = 0;
                            }
                            continue;
                        }
                        leviculum_channel_access::Verdict::Retry => continue,
                    }
                }
                Err(e) => {
                    crate::log::log_fmt(
                        "[LORA_CAD] ",
                        format_args!("err={:?} attempt={}", e, access.retries()),
                    );
                    if let leviculum_channel_access::Verdict::Transmit { retries, .. } =
                        access.cad_error()
                    {
                        crate::log::log_fmt(
                            "[LORA_CSMA_TX] ",
                            format_args!("retries={} forced=true slot_ms={}", retries, slot_ms),
                        );
                        transmit_all_frames(
                            &mut radio,
                            data,
                            &mut rng_state,
                            &config,
                            &mut airtime,
                            &mut spacer,
                        )
                        .await;
                        pending_tx = None;
                        // This forced-TX path skips the ack window below
                        // (pre-existing continue); still account its
                        // airtime so the burst bounds stay accurate.
                        frames_since_yield += 1;
                        airtime_since_yield_ms += tx_cost_ms;
                    }
                    continue;
                }
            }
            // TX just completed. Before draining the next outgoing item, give
            // RX a real, bounded listening window so the peer's ack/reply gets
            // through. Without this the loop pulled the next queued packet
            // immediately (only a one-symbol CAD in between) and a sender with
            // a non-empty queue transmitted back-to-back, never listening for
            // acks. At slow SF (2.7s/frame at SF10) the busy side went deaf,
            // retransmitted, and the link died via retry exhaustion (#23). The
            // window is airtime-aware and self-shortens: rx_window returns as
            // soon as a packet arrives. Idle continuous RX (queue empty) is
            // unchanged below.
            //
            // Bounded burst (Bug B): receivers REQ/ACK per transfer window,
            // not per part, so a full ack window after every part of the same
            // requested window is dead air that spaces parts further apart
            // than the receiver's timeout, provoking premature re-REQs. Keep
            // draining queued frames back-to-back (CSMA/CAD before each TX
            // still spaces them and listens during backoff) until the queue
            // empties or a burst bound trips; only then yield exactly as
            // before. Embassy's try_receive consumes, so a peeked frame is
            // stashed into pending_tx and transmitted next iteration through
            // the normal CSMA/CAD TX path.
            let queue_empty = match take_outgoing(&outgoing_rx) {
                // A frame the mute swallows leaves the burst with nothing in
                // flight, which is what `true` says here: the accounting
                // follows the radio, not the queue.
                Some(next) => {
                    !admit_for_transmit(next, &config, &mut muted, &mut pending_tx, &mut access)
                }
                None => true,
            };
            if !leviculum_core::rnode::burst_should_yield(
                queue_empty,
                frames_since_yield,
                airtime_since_yield_ms,
                MAX_BURST_FRAMES,
                MAX_BURST_AIRTIME_MS,
            ) {
                crate::log::log_fmt(
                    "[LORA_BURST] ",
                    format_args!(
                        "continue frames={} airtime_ms={}",
                        frames_since_yield, airtime_since_yield_ms
                    ),
                );
                continue;
            }
            let ack_window_ms = post_tx_rx_window_ms(&config);
            reassembler.check_timeout(rx_timeout_count, 10);
            let ack_received = rx_window(
                &mut radio,
                &mut rx_buf,
                (ack_window_ms, leviculum_core::sx126x::RxSite::Ack),
                &mut reassembler,
                &incoming_tx,
                &mut rx_timeout_count,
                &config_rx,
            )
            .await;
            if ack_received {
                consecutive_empty_acks = 0;
            } else {
                consecutive_empty_acks += 1;
            }

            // Peer-turn yield: with a deep outgoing queue the loop would now
            // `continue` and immediately drain the next frame, TXing again
            // without ever parking in the queue-empty continuous-RX branch. The
            // peer, waiting to CSMA-backoff and send its REQ, never gets a clear
            // window, so the transfer livelocks (#23 Bug B). After
            // PEER_YIELD_AFTER_EMPTY consecutive empty ack windows, give the
            // peer one guaranteed listening window that does NOT consult the
            // outgoing queue. Length is two ack windows: one peer reply window
            // plus headroom for the peer's CSMA backoff before it transmits.
            if consecutive_empty_acks >= PEER_YIELD_AFTER_EMPTY {
                let yield_ms = post_tx_rx_window_ms(&config).saturating_mul(2);
                crate::log::log_fmt(
                    "[T114_PEER_YIELD] ",
                    format_args!(
                        "after_empty={} yield_ms={}",
                        consecutive_empty_acks, yield_ms
                    ),
                );
                reassembler.check_timeout(rx_timeout_count, 10);
                rx_window(
                    &mut radio,
                    &mut rx_buf,
                    (yield_ms, leviculum_core::sx126x::RxSite::Yield),
                    &mut reassembler,
                    &incoming_tx,
                    &mut rx_timeout_count,
                    &config_rx,
                )
                .await;
                consecutive_empty_acks = 0;
            }
            // A yield ran (ack window, plus backstop if it triggered): the
            // channel was handed back, restart the burst accounting — and
            // the next acquisition owes fresh jitter, because whatever
            // transmits after a shared listening window is again a
            // candidate for phase lock with its peers.
            frames_since_yield = 0;
            airtime_since_yield_ms = 0;
            access.channel_released();
            continue;
        }

        // Queue empty: timeout stale split reassembly buffers, then stay in
        // continuous RX until either a packet arrives or the daemon hands us
        // something to send. rx_window with timeout_ms==0 arms SetRx in single
        // mode (no HW timeout), so the radio listens with no re-arm gap. The
        // fixed-window loop re-armed every 500ms; at slow SF a long preamble
        // (~197ms at SF10) almost always fell into a re-arm gap and was never
        // detected. select yields to TX the instant the daemon has data, so
        // continuous RX does not starve path responses or announces.
        //
        // Two arms, not three: the config is an arm of `rx_window` itself now
        // — of this window and of every other one the loop has — so this site
        // carries only what is peculiar to it, the outgoing queue. Bound to a
        // local so the futures are dropped before the outgoing arm reaches
        // for the radio and the loop state again.
        reassembler.check_timeout(rx_timeout_count, 10);
        let idle = select(
            rx_window(
                &mut radio,
                &mut rx_buf,
                (0, leviculum_core::sx126x::RxSite::Idle),
                &mut reassembler,
                &incoming_tx,
                &mut rx_timeout_count,
                &config_rx,
            ),
            outgoing_rx.receive(),
        )
        .await;
        match idle {
            // RX finished (packet delivered, single-mode wait elapsed, or a
            // config waiting for the top of the turn). Loop re-arms RX
            // immediately; the only gap is this brief re-arm, taken right
            // after a reception. A reception here also means the peer is
            // being heard, so clear the empty-ack counter.
            Either::First(received) => {
                if received {
                    consecutive_empty_acks = 0;
                }
            }
            // The daemon has outgoing data. The RX future was dropped, which
            // can now happen at three points instead of one — inside the
            // arming, inside the wait, or inside the hand-off that follows the
            // provisional re-arm — so the receiver is stood down here rather
            // than assumed to be down. The arming state is the one that knows:
            // it is set before `SetRx` goes out and cleared only after a
            // standby completes, so a drop anywhere in that span still owes
            // exactly one standby and this spends it.
            //
            // How often this arm runs is measured: 21-34 % of all armings on
            // the bench take it, so the "rare" this comment used to claim was
            // wrong. Whether anything was on the air when the window came down
            // is measured too, and by the same call that takes it down: an
            // idle listen stood down here loses nothing — that is half duplex,
            // and the reference firmware does the same — while a window with
            // `PreambleDetected` latched loses a frame that would otherwise
            // have completed.
            //
            // The sweep that measured it named this site and nothing else: at
            // a 20 ms on-air gap, 8 of the teardowns here carried a live frame
            // (4 of them past the header) and 0 reports were delivered, while
            // at 30-60 ms the windows were adopted instead and the reports
            // landed. So this is the one teardown in the firmware that waits:
            // `disarm_rx_for_tx` holds the key-up for the frame that is
            // arriving, for one maximum-size frame's airtime at the live
            // modulation and no longer, hands that frame up by the same route
            // `rx_window` would have, and then spends the same single standby.
            // A window with a clear latch is stood down exactly as before —
            // the wait is conditional on a measured reception and on nothing
            // else, which is what keeps this from being a spacing delay.
            // radio_silent still drops outgoing instead of transmitting.
            Either::Second(data) => {
                // The one dequeue that does not go through `take_outgoing`:
                // `receive()` is the awaited form, and the budget it held is
                // released here for the same reason and at the same moment.
                OUTGOING_BUDGET.release(data.len());
                // The same sink `rx_window` builds, so a frame the deferral
                // catches reaches the core indistinguishably from any other.
                // `rx_start` is taken here, before the wait, so the
                // `op=rx_success duration_ms` it reports brackets the
                // deferral rather than nothing.
                let mut sink = CoreHandoff {
                    rx_start: embassy_time::Instant::now(),
                    reassembler: &mut reassembler,
                    incoming_tx: &incoming_tx,
                    rx_timeout_count,
                };
                let _ = radio
                    .disarm_rx_for_tx(
                        leviculum_core::sx126x::RxTeardownBy::Select,
                        leviculum_rx_arming::DeferPolicy::OneFrame,
                        &mut rx_buf,
                        &mut sink,
                    )
                    .await;
                admit_for_transmit(data, &config, &mut muted, &mut pending_tx, &mut access);
            }
        }
    }
}
