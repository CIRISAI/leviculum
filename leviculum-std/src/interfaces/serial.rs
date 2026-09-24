//! Serial interface. HDLC-framed bidirectional serial port
//!
//! Implements a plain serial interface matching Python Reticulum's
//! `SerialInterface`. Uses HDLC simplified framing (same as TCP and
//! LocalInterface) over a serial port.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use crate::sync_ext::MutexRecover;
use std::time::Duration;

use leviculum_core::constants::MTU;
use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use leviculum_core::rnode::{derive_preamble_symbols, RadioConfigWire};
use leviculum_core::transport::InterfaceId;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::{IncomingPacket, InterfaceCounters, InterfaceHandle, InterfaceInfo, OutgoingPacket};

/// Python SerialInterface HW_MTU
const SERIAL_HW_MTU: u32 = 564;

/// Default channel buffer size for serial interfaces.
pub(crate) const SERIAL_DEFAULT_BUFFER_SIZE: usize = 64;

/// Frame buffer multiplier (accounts for HDLC escaping overhead)
const FRAME_BUFFER_MULTIPLIER: usize = 2;

/// Read buffer size
const READ_BUF_SIZE: usize = 1024;

/// Incomplete frame timeout (ms). Matches Python SerialInterface.timeout = 100.
/// If no data arrives for this duration while in_frame, the partial frame is
/// discarded to prevent desynchronization from noise/corruption.
const FRAME_TIMEOUT: Duration = Duration::from_millis(100);

/// Reconnect interval after serial port loss
const RECONNECT_INTERVAL: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Commanded firmware requests, out of band
// ---------------------------------------------------------------------------

/// What a caller can ask this daemon to put on its boards' data ports.
///
/// One frame per variant, and nothing else: this is the escape hatch for
/// the ports the daemon holds exclusively, not a general control channel.
/// A variant earns its place by being impossible to do any other way
/// while the daemon is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareRequest {
    /// Reboot the board ([`leviculum_core::rnode::RADIO_RESET_FRAME`]).
    Reset,
    /// Announce now ([`leviculum_core::envelope::TYPE_ANNOUNCE`]): the
    /// board makes every announce it would make on its own cadence,
    /// immediately. Reaches the daemon as
    /// [`FIRMWARE_ANNOUNCE_SIGNAL`].
    ///
    /// Same standing as the reset, and for the same reason. A harness
    /// that has to observe a client learning a board's destination
    /// cannot wait out the board's announce interval and call the result
    /// a measurement — a 300 s interval against a 300 s step budget is a
    /// race the run loses roughly half the time (periculum's
    /// `ble_pn_board_upload`, 2026-09-14). The daemon is the only
    /// process that can ask, because it holds the port.
    Announce,
    /// Take every carrier off the air: read the board's media profile
    /// ([`leviculum_core::envelope::TYPE_MEDIA_QUERY`]), remember it, then
    /// write a profile with both carriers clear
    /// ([`leviculum_core::envelope::TYPE_MEDIA_PROFILE`]). Reaches the
    /// daemon as [`FIRMWARE_MEDIA_SILENCE_SIGNAL`].
    ///
    /// Earned against the sentence above, and the alternatives are what
    /// earn it. A scenario that measures how a mesh behaves when one node
    /// stops being heard has to stop it being heard *mid-run*: cutting its
    /// power takes its clock, its queues and its links with it, a
    /// [`FirmwareRequest::Reset`] reboots it into a state the run did not
    /// produce, and stopping this daemon takes the port with it — so
    /// nothing would be left that could put the carriers back afterwards.
    /// Silencing leaves the node running and the port answering, which is
    /// the only shape in which "the mesh routed around a node that went
    /// quiet" is a statement about the mesh rather than about the harness.
    /// It is impossible any other way while the daemon is up, for the same
    /// reason the reset is: the daemon holds the port.
    MediaSilence,
    /// Put back exactly the profile [`FirmwareRequest::MediaSilence`] read
    /// off this board before it silenced it. Reaches the daemon as
    /// [`FIRMWARE_MEDIA_RESTORE_SIGNAL`].
    ///
    /// A second variant rather than a parameter on the first, because the
    /// two arrive as two signals and a signal carries no payload. It earns
    /// its place for the reason the silence does — the port is held — plus
    /// one of its own: a silence is only honest if it is reversible, and
    /// the only process that can reverse it is the one holding the port.
    /// Half a verb would leave every silenced board silent until someone
    /// stopped the daemon and opened the port by hand.
    ///
    /// **The daemon owns what "restore" means.** The profile written back
    /// is the one this daemon read off that board, never a value a caller
    /// supplied and never a default: a scenario file that could name a
    /// profile would be making a configuration change wearing a restore's
    /// name, and the run after it would be measuring a board the run
    /// itself reconfigured. [`remembered_media_profile`] is the whole
    /// vocabulary, and a restore with nothing remembered is the documented
    /// no-op there.
    MediaRestore,
}

/// The signal a caller sends lnsd to ask for [`FirmwareRequest::Announce`].
///
/// A NUMBER and not a name, which is the whole point. The named spares are
/// gone — SIGUSR1 is the diagnostics dump, SIGUSR2 the firmware reset, and
/// SIGHUP has to keep terminating a daemon run from a terminal — so this
/// lives in the real-time range Linux reserves for signals with no prior
/// meaning. But `SIGRTMIN` is a libc opinion, not a constant: measured on
/// this host, glibc answers 34 and musl 35 (musl keeps 32-34 for its own
/// `__synccall`), and lnsd ships in both flavours — the workspace builds
/// x86_64-unknown-linux-musl by default (`.cargo/config.toml`) while
/// `docker kill --signal=SIGRTMIN` and `/usr/bin/kill -RTMIN` both resolve
/// against the SENDER's glibc. Naming the signal would have sent 34 to a
/// daemon listening on 35, and 34 in a musl process is a signal musl
/// reserves.
///
/// 40 instead: inside the real-time range of both (glibc 34-64, musl
/// 35-64) and clear of the low end, so a libc that reserves another one or
/// two still leaves it free. Senders write the number too — periculum's
/// `announce_board` step is the other end of this contract.
pub const FIRMWARE_ANNOUNCE_SIGNAL: i32 = 40;

/// The signal a caller sends lnsd to ask for
/// [`FirmwareRequest::MediaSilence`].
///
/// Numbers and not names, for the reason [`FIRMWARE_ANNOUNCE_SIGNAL`] gives
/// in full — and re-measured rather than inherited, because the whole point
/// of that reasoning is that `SIGRTMIN` is a libc's opinion and an opinion
/// can change under us. Measured again on this host on 2026-09-22, by
/// compiling one C program that prints `SIGRTMIN` and `SIGRTMAX` with each
/// libc's own compiler: `gcc` (Debian GLIBC 2.41-12+deb13u4) answers
/// `SIGRTMIN=34 SIGRTMAX=64`, `musl-gcc` (musl 1.2.5-3.1~deb13u1) answers
/// `SIGRTMIN=35 SIGRTMAX=64`. So the range that is real-time under both
/// libcs is 35-64, and the announce's 40 still sits inside it.
///
/// 41 and 42 continue upward from 40 rather than starting a new band: they
/// are clear of the low end by the same margin, so a libc that reserves
/// another one or two leaves all three free, and consecutive numbers make
/// the three verbs one surface to read. Senders write the numbers too —
/// periculum's steps are the other end of this contract, exactly as
/// `announce_board` is for 40.
pub const FIRMWARE_MEDIA_SILENCE_SIGNAL: i32 = 41;

/// The signal a caller sends lnsd to ask for
/// [`FirmwareRequest::MediaRestore`]; see
/// [`FIRMWARE_MEDIA_SILENCE_SIGNAL`] for the measurement behind the number.
pub const FIRMWARE_MEDIA_RESTORE_SIGNAL: i32 = 42;

/// Broadcast of a [`FirmwareRequest`] to every attached board.
///
/// The daemon holds each firmware board's data port for its whole run, and
/// the serial crate opens it `TIOCEXCL`, so nothing else on the host can
/// open it while the daemon is up. That is the right default — two writers
/// interleaving mid-frame is a corrupted link — but it leaves no way to
/// reboot a board that a running daemon is attached to. A harness that
/// measures whether a mesh re-forms after a node goes away has to take the
/// BOARD out, and the only process that can speak to it is this one
/// (periculum #255).
///
/// Broadcast rather than addressed because the request arrives as a
/// process signal, which is addressed to the process and not to one
/// interface. A daemon holds the boards of one node; "every board this
/// daemon has" is the same set either way, and an addressed variant would
/// need a name mapping that nothing else in this file has.
///
/// The frame put on the wire is byte-identical to the one an unattached
/// reset writes ([`leviculum_core::rnode::RADIO_RESET_FRAME`], HDLC-framed
/// exactly as an outgoing packet is), so a board rebooted through here
/// comes back from the same defined state — which is what makes the two
/// resets comparable at all.
static FIRMWARE_REQUESTS: std::sync::OnceLock<tokio::sync::broadcast::Sender<FirmwareRequest>> =
    std::sync::OnceLock::new();

fn firmware_request_channel() -> &'static tokio::sync::broadcast::Sender<FirmwareRequest> {
    FIRMWARE_REQUESTS.get_or_init(|| tokio::sync::broadcast::channel(4).0)
}

/// Ask every serial interface in this process to put `request` on the wire
/// to the board behind it. Returns the number of interfaces that were
/// listening — 0 means this daemon holds no firmware board, which is a
/// finding for the caller rather than an error here.
pub fn request_firmware(request: FirmwareRequest) -> usize {
    firmware_request_channel().send(request).unwrap_or(0)
}

/// [`FirmwareRequest::Reset`], by its own name.
pub fn request_firmware_reset() -> usize {
    request_firmware(FirmwareRequest::Reset)
}

/// [`FirmwareRequest::Announce`], by its own name.
pub fn request_firmware_announce() -> usize {
    request_firmware(FirmwareRequest::Announce)
}

/// [`FirmwareRequest::MediaSilence`], by its own name.
pub fn request_firmware_media_silence() -> usize {
    request_firmware(FirmwareRequest::MediaSilence)
}

/// [`FirmwareRequest::MediaRestore`], by its own name.
pub fn request_firmware_media_restore() -> usize {
    request_firmware(FirmwareRequest::MediaRestore)
}

/// What each board was configured for before this daemon silenced it,
/// keyed by interface name.
///
/// Process-global and not a local of the io task, because the io task
/// ends every time the port does. The profile lives in the board's flash
/// — that is what makes a silence survive a reboot, and it is also what
/// would strand a board silent if the memory of it died with a USB
/// re-enumeration. Keyed by interface name because that is what the
/// daemon addresses a board by everywhere else in this file, and names
/// are unique within one daemon's config.
///
/// An entry means "this interface owes a restore". It is written before
/// the silence frame goes out and removed only once a restore has been
/// confirmed, so both crash windows fall the safe way round: a remembered
/// profile for a board that was never silenced restores to the profile it
/// already has (a no-op), while the reverse ordering would leave a
/// silenced board with nothing to restore to.
static SILENCED_MEDIA_PROFILES: std::sync::OnceLock<
    Mutex<std::collections::HashMap<String, leviculum_core::envelope::MediaProfileWire>>,
> = std::sync::OnceLock::new();

fn silenced_media_profiles(
) -> &'static Mutex<std::collections::HashMap<String, leviculum_core::envelope::MediaProfileWire>> {
    SILENCED_MEDIA_PROFILES.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// The profile `interface`'s board was configured for before this daemon
/// silenced it, or `None` when this daemon never silenced it.
///
/// The entire vocabulary [`FirmwareRequest::MediaRestore`] has: there is
/// no way to ask for a profile that did not come off the board itself.
pub fn remembered_media_profile(
    interface: &str,
) -> Option<leviculum_core::envelope::MediaProfileWire> {
    silenced_media_profiles()
        .lock_recover()
        .get(interface)
        .copied()
}

/// Radio configuration to send to LNode firmware over serial (test infrastructure).
pub(crate) struct SerialRadioConfig {
    pub frequency: u64,
    pub bandwidth: u32,
    pub spreading_factor: u8,
    pub coding_rate: u8,
    pub tx_power: i8,
    pub preamble_len: u16,
    pub csma_enabled: bool,
    /// Long-term airtime lock in the firmware's `fraction * 10000` encoding;
    /// `0` is what the firmware reads as unlimited.
    pub lt_alock: u16,
}

/// Build the LNode radio config a `SerialInterface` block asks for, or
/// `None` when the block names no `frequency` and is therefore a plain
/// (non-LoRa) serial pipe.
///
/// Every PHY default here is the firmware's own compiled default
/// (`leviculum_nrf::lora::RadioConfig::eu_medium`, the ReticulumNet
/// consensus: BW125, SF8, CR4/5), so a block that names only a `frequency`
/// puts the board on the same PHY it would boot on standalone. The preamble
/// is not a number but the RNode firmware's own derivation — our reference
/// for LoRa PHY behaviour scales the preamble to a target duration and
/// floors it at 18 symbols, and a constant here was a wire-level deviation
/// that cost interop with every RNode peer in the long-range regime. The
/// default is therefore [`derive_preamble_symbols`], and `preamble_symbols`
/// in the config file still overrides it — which is how the corner gets
/// re-measured, and how a node with a non-conforming peer copes.
///
/// The airtime lock resolves the same way, and for the same reason. An
/// LNode is configured by this function and by nothing else, so a hardcoded
/// `lt_alock = 0` here was the host telling the firmware "unlimited" and
/// overriding the lawful default the firmware would otherwise have derived
/// from its own frequency ([`firmware_default_lt_alock`]). Sending the
/// resolution instead of a constant means an LNode ends up under the same
/// limit as an RNode on the same frequency (`driver::resolve_lt_alock`),
/// and `airtime_limit_long` in the config file still overrides it — with
/// `0` still available for a bench that means unlimited and says so.
pub(crate) fn serial_radio_config(
    cfg: &crate::config::InterfaceConfig,
) -> Option<SerialRadioConfig> {
    let frequency = cfg.frequency?;
    let requested_bandwidth = cfg.bandwidth.unwrap_or(125_000);
    // A SerialInterface radio block is an LNode, and every LNode is an
    // SX1262, so the bandwidth that goes on the wire is spelled the way that
    // chip's register table spells it. Five of the ten bandwidths have a
    // second, SX127x spelling that Python-RNS and RNode_Firmware use and that
    // `validate_config` accepts; the board's parser matches exactly, and its
    // answer to a config it cannot take is silence, so leaving the RNode
    // spelling on the wire loses the config without a word (L-0007). An
    // unmappable value is left alone for the builder's `validate_config` to
    // refuse by name.
    let bandwidth = leviculum_core::sx126x::canonical_bandwidth_hz(requested_bandwidth)
        .unwrap_or(requested_bandwidth);
    if bandwidth != requested_bandwidth {
        tracing::info!(
            "SerialInterface: bandwidth {} Hz is the SX127x spelling of the SX1262's {} Hz — \
             the same register on both parts; programming {} Hz, which the LNode accepts",
            requested_bandwidth,
            bandwidth,
            bandwidth
        );
    }
    let spreading_factor = cfg.spreading_factor.unwrap_or(8);
    let coding_rate = cfg.coding_rate.unwrap_or(5);
    Some(SerialRadioConfig {
        frequency,
        bandwidth,
        spreading_factor,
        coding_rate,
        tx_power: leviculum_core::rnode::resolve_tx_power(cfg.tx_power, frequency),
        preamble_len: cfg
            .preamble_symbols
            .unwrap_or_else(|| derive_preamble_symbols(spreading_factor, coding_rate, bandwidth)),
        csma_enabled: cfg.csma_enabled.unwrap_or(true),
        lt_alock: leviculum_core::rnode::firmware_default_lt_alock(
            frequency,
            cfg.airtime_limit_long.map(|p| (p * 100.0) as u16),
        ),
    })
}

/// Symbol count above which an SX127x receiver stopped decoding on the rig
/// (Codeberg #315). Measured 2026-08-21 at SF10/BW125, T114 transmitting,
/// t-beam-1 receiving in raw KISS with our stack out of the RX path, ~20
/// forced path requests per rung: 18 symbols (147 ms) decoded 36 frames, 20
/// (164 ms) decoded 2, and 22 / 24 / 28 decoded none while the carrier was
/// present at up to -6 dBm on every rung.
const SX127X_RX_PREAMBLE_CEILING_SYMBOLS: u16 = 20;

/// On-air preamble duration at the same ceiling — 20 symbols at SF10/BW125,
/// the last rung that still decoded anything.
///
/// The symbol count alone does not explain the measurement, and a warning on
/// the count alone would be wrong: `lora_path_discovery_fast_mixed` keys 24
/// symbols at SF7/BW125 (24.6 ms) into the same SX1276 and is green. What
/// separates the two is time on air, which is why both terms are required
/// below — the warning fires only inside the region where a receiver was
/// actually measured going deaf.
const SX127X_RX_PREAMBLE_CEILING_MS: u64 = 164;

/// The config-time warning for a `preamble_symbols` pin that SX127x peers
/// cannot receive (Codeberg #315), or `None` for a block that is safe, has no
/// pin, or is not a LoRa block at all.
///
/// The pin bypasses [`derive_preamble_symbols`] (see `serial_radio_config`
/// above), so it is the one path in either stack that can put a preamble on
/// the air longer than the RNode firmware would ever key. Above the measured
/// ceiling an SX1276 peer decodes nothing from this interface while its own
/// frames still arrive here — silent, one-way loss that looks like a range or
/// routing problem from both ends.
///
/// This warns and does not refuse. An SX126x-only mesh may key long preambles
/// legitimately (the SX1262 copes), the peer population is not knowable from a
/// config file, and the pin is also how the corner gets re-measured.
pub(crate) fn preamble_ceiling_warning(
    name: &str,
    cfg: &crate::config::InterfaceConfig,
) -> Option<String> {
    // No frequency means a plain serial pipe: `serial_radio_config` returns
    // `None` and the pin never reaches a modem.
    let _frequency = cfg.frequency?;
    let pinned = cfg.preamble_symbols?;
    let bandwidth = cfg.bandwidth.unwrap_or(125_000);
    let spreading_factor = cfg.spreading_factor.unwrap_or(8);
    let coding_rate = cfg.coding_rate.unwrap_or(5);

    if pinned <= SX127X_RX_PREAMBLE_CEILING_SYMBOLS {
        return None;
    }
    let preamble_ms = preamble_airtime_ms(pinned, spreading_factor, bandwidth)?;
    if preamble_ms < SX127X_RX_PREAMBLE_CEILING_MS {
        return None;
    }

    let derived = derive_preamble_symbols(spreading_factor, coding_rate, bandwidth);
    Some(format!(
        "{name}: preamble_symbols = {pinned} is pinned, {preamble_ms} ms on air at SF{spreading_factor}/BW{bandwidth}. \
         Measured on the rig, SX127x receivers stop decoding above ~{SX127X_RX_PREAMBLE_CEILING_SYMBOLS} symbols \
         (~{SX127X_RX_PREAMBLE_CEILING_MS} ms) at this bandwidth (Codeberg #315): every RNode and any other SX127x peer \
         will lose all frames from this interface silently and one-way, while their own frames still arrive here. \
         Keep the pin only for a mesh of SX126x receivers; removing it derives {derived} symbols, which every peer receives."
    ))
}

/// On-air duration of `symbols` preamble symbols, in whole milliseconds.
///
/// Symbol time is `2^sf / bandwidth`; `None` for a PHY no modem offers, where
/// the shift would be meaningless rather than merely large.
fn preamble_airtime_ms(symbols: u16, sf: u8, bandwidth_hz: u32) -> Option<u64> {
    if sf == 0 || sf > 12 || bandwidth_hz == 0 {
        return None;
    }
    Some(symbols as u64 * (1u64 << sf) * 1000 / bandwidth_hz as u64)
}

/// Configuration for a serial interface.
pub(crate) struct SerialInterfaceConfig {
    pub id: InterfaceId,
    pub name: String,
    pub port: String,
    pub speed: u32,
    pub data_bits: tokio_serial::DataBits,
    pub parity: tokio_serial::Parity,
    pub stop_bits: tokio_serial::StopBits,
    pub buffer_size: usize,
    pub reconnect_notify: Option<mpsc::Sender<InterfaceId>>,
    pub radio_config: Option<SerialRadioConfig>,
    /// TEST-ONLY range emulation: drop deframed hops=0 ingress frames
    /// (see [`super::test_drop_direct_ingress_frame`]).
    pub test_drop_direct_ingress: bool,
}

/// Spawn a serial interface with automatic reconnection.
///
/// Creates channel pair once, spawns a reconnect task that reopens the port
/// on failure. The `InterfaceHandle` stays alive across reconnections.
pub(crate) fn spawn_serial_interface(config: SerialInterfaceConfig) -> InterfaceHandle {
    let (incoming_tx, incoming_rx) = mpsc::channel(config.buffer_size);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(config.buffer_size);
    let counters = Arc::new(InterfaceCounters::new());
    // Offline until the reconnect loop opens the port (L-0020).
    counters.set_online(false);

    let id = config.id;
    let handle_name = config.name.clone();
    let task_name = config.name.clone();
    let task_counters = Arc::clone(&counters);

    // Build the airtime credit bucket if radio params are known. Non-LoRa
    // Serial consumers (no radio_config) leave credit = None, preserving
    // "always ready" semantics for the next_slot_ms override.
    let credit = config.radio_config.as_ref().map(|rc| {
        Arc::new(Mutex::new(super::airtime::AirtimeCredit::new(
            rc.bandwidth,
            rc.spreading_factor,
            rc.coding_rate,
            rc.preamble_len,
            SERIAL_HW_MTU,
        )))
    });
    let task_credit = credit.clone();

    tokio::spawn(async move {
        serial_reconnect_task(
            id,
            config,
            task_name,
            incoming_tx,
            outgoing_rx,
            task_counters,
            task_credit,
        )
        .await;
    });

    InterfaceHandle {
        info: InterfaceInfo {
            id,
            name: handle_name,
            hw_mtu: Some(SERIAL_HW_MTU),
            is_local_client: false,
            bitrate: None,
            announce_cap_bitrate: None,
            tx_jitter_max_ms: None,
            frame_turnaround_ms: None,
            ifac: None,
            mode: leviculum_core::traits::InterfaceMode::default(),
            kind: leviculum_core::traits::InterfaceKind::Serial,
            ingress_control: None,
        },
        incoming: incoming_rx,
        outgoing: outgoing_tx,
        counters,
        credit,
        // Serial-port readiness mirrors RNode (see note there).
        ready: super::ReadySignal::ready_immediate(),
    }
}

// ---------------------------------------------------------------------------
// Radio bring-up: what the board is running, and whether we may drive it
// ---------------------------------------------------------------------------

/// How long one radio-config attempt waits for the legacy ACK.
const CONFIG_ACK_TIMEOUT: Duration = Duration::from_secs(2);

/// How many times the config is pushed before the host stops asking for an
/// ACK and starts asking what the board is actually running.
const CONFIG_ATTEMPTS: u8 = 3;

/// How long the radio query waits for the board's own report — the config
/// ACK's budget, for the reason [`MEDIA_ANSWER_TIMEOUT`] carries the same
/// one: the answer is composed by the firmware's USB task the moment the
/// frame lands, so anything past it is a board that is not going to answer.
const RADIO_REPORT_TIMEOUT: Duration = Duration::from_secs(2);

/// What a board ended up running after a radio config was pushed at it, as
/// far as the board itself has said.
///
/// Public, with [`radio_bring_up`] and [`radio_pricing_phy`], because the
/// mvr drives this exchange over an in-memory duplex instead of a soldered
/// board — the same reason the RNode channel seam
/// ([`super::RNodeChannelFactory`]) is public.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadioBringUp {
    /// The board acked the config: it runs the profile that was requested.
    ///
    /// The legacy ACK is a bare receipt — three bytes, no parameters — so
    /// "what it adopted" is readable only as "what it was sent". That is
    /// also why there is no fourth variant for an ACK the host cannot
    /// reconcile: an ACK carries nothing to disagree with, and three bytes
    /// that are not [`leviculum_core::rnode::RADIO_CONFIG_ACK`] are not an
    /// ACK at all, so they leave the attempt on the no-ACK path.
    Adopted,
    /// No ACK, but the board answered
    /// [`leviculum_core::envelope::TYPE_RADIO_QUERY`]: this is the profile
    /// it is running right now, read off the board rather than guessed.
    Running(RadioConfigWire),
    /// The board answered neither frame within the wait. A board that
    /// refused the query by name lands here too: a refusal says what the
    /// board will not tell us, not what it is running.
    Silent,
}

/// The config block as it goes on the wire, and as the board's own report
/// is compared against.
fn requested_wire(config: &SerialRadioConfig) -> RadioConfigWire {
    RadioConfigWire {
        frequency_hz: config.frequency as u32,
        bandwidth_hz: config.bandwidth,
        sf: config.spreading_factor,
        cr: config.coding_rate,
        tx_power_dbm: config.tx_power,
        preamble_len: config.preamble_len,
        csma_enabled: config.csma_enabled,
        radio_silent: false,
        // Airtime limits are enforced by the LNode firmware's airtime lock.
        // Short-term stays unset (no config key spells one); long-term is
        // resolved in `serial_radio_config` — lawful for the frequency
        // unless `airtime_limit_long` says otherwise.
        st_alock: 0,
        lt_alock: config.lt_alock,
        // Send-side only; `build_radio_config_frame` always emits the full
        // 21-byte frame, so the receiver parses the lt_alock field as present.
        lt_alock_present: true,
    }
}

/// The pricing-relevant parameters of one profile, as the scalar log keys
/// periculum greps for. `media_keys`' sibling.
fn phy_keys(prefix: &str, w: &RadioConfigWire) -> String {
    format!(
        "{prefix}_freq={} {prefix}_bw={} {prefix}_sf={} {prefix}_cr={} {prefix}_preamble={}",
        w.frequency_hz, w.bandwidth_hz, w.sf, w.cr, w.preamble_len
    )
}

/// Do these two profiles price and place a frame differently?
///
/// Everything the airtime bucket charges from (bandwidth, spreading factor,
/// coding rate, preamble) plus the frequency, which decides whether the two
/// radios are on the same channel at all. Transmit power is deliberately not
/// here: it changes who hears the frame, not what it costs, and a board that
/// clamped a power it cannot key is not running a profile the host mispriced.
fn phy_differs(a: &RadioConfigWire, b: &RadioConfigWire) -> bool {
    a.frequency_hz != b.frequency_hz
        || a.bandwidth_hz != b.bandwidth_hz
        || a.sf != b.sf
        || a.cr != b.cr
        || a.preamble_len != b.preamble_len
}

/// Push `requested` at the LNode firmware and find out what it ends up
/// running.
///
/// `CONFIG_ATTEMPTS` pushes of the legacy config frame, each waiting
/// `CONFIG_ACK_TIMEOUT` for the legacy ACK. An ACK ends it at
/// [`RadioBringUp::Adopted`]. Silence does not: the host then asks the
/// board what it is running (`ask_radio_report`), because the alternative
/// to asking is guessing, and a guess about the PHY is a guess about every
/// frame's airtime for as long as the interface is up.
pub async fn radio_bring_up<S>(
    port: &mut S,
    requested: &RadioConfigWire,
    name: &str,
) -> RadioBringUp
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use leviculum_core::rnode::RADIO_CONFIG_ACK;

    let payload = leviculum_core::rnode::build_radio_config_frame(requested);
    let mut frame_buf = Vec::new();
    frame(&payload, &mut frame_buf);

    for attempt in 1..=CONFIG_ATTEMPTS {
        tracing::info!(
            "Serial {}: sending radio config (attempt {}/{}): freq={} sf={} bw={} cr={} txp={}",
            name,
            attempt,
            CONFIG_ATTEMPTS,
            requested.frequency_hz,
            requested.sf,
            requested.bandwidth_hz,
            requested.cr,
            requested.tx_power_dbm
        );
        if let Err(e) = port.write_all(&frame_buf).await {
            tracing::warn!("Serial {}: config write failed: {}", name, e);
            continue;
        }
        if let Err(e) = port.flush().await {
            tracing::warn!("Serial {}: config flush failed: {}", name, e);
            continue;
        }

        // Wait for ACK
        let mut deframer = Deframer::with_max_frame(SERIAL_HW_MTU as usize);
        let mut buf = [0u8; 64];
        let deadline = tokio::time::Instant::now() + CONFIG_ACK_TIMEOUT;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                tracing::warn!("Serial {}: config ACK timeout (attempt {})", name, attempt);
                break;
            }
            match tokio::time::timeout(remaining, port.read(&mut buf)).await {
                Ok(Ok(n)) if n > 0 => {
                    for r in deframer.process(&buf[..n]) {
                        if let DeframeResult::Frame(data) = r {
                            if data.len() == RADIO_CONFIG_ACK.len()
                                && data[..] == RADIO_CONFIG_ACK[..]
                            {
                                tracing::info!("Serial {}: radio config ACK received", name);
                                return RadioBringUp::Adopted;
                            }
                        }
                    }
                }
                Ok(Ok(_)) => break, // EOF
                Ok(Err(e)) => {
                    tracing::warn!("Serial {}: config ACK read error: {}", name, e);
                    break;
                }
                Err(_) => break, // timeout
            }
        }
    }
    tracing::warn!(
        "Serial {}: radio config not acknowledged after {} attempts — asking the board \
         what it is running",
        name,
        CONFIG_ATTEMPTS
    );
    match ask_radio_report(port, name).await {
        Some(running) => RadioBringUp::Running(running),
        None => RadioBringUp::Silent,
    }
}

/// Ask the board what its radio is running
/// ([`leviculum_core::envelope::TYPE_RADIO_QUERY`], Codeberg #349) and wait
/// [`RADIO_REPORT_TIMEOUT`] for the report.
///
/// The firmware answers this one out of what its LoRa task actually
/// configured, never out of the flash page or the compiled default
/// (`leviculum-nrf/src/usb.rs`, `ControlAction::RadioQuery`), which is the
/// whole reason it can settle a question an unanswered config leaves open.
/// Before the radio is up it refuses as busy instead of inventing an answer;
/// that refusal is logged here and returns `None`, because "I will not say"
/// is not a profile anything can be priced at.
///
/// Frames that are neither answer are dropped, exactly as the ACK wait above
/// drops them: this runs before the io task exists, so there is nothing yet
/// to hand a data packet to.
async fn ask_radio_report<S>(port: &mut S, name: &str) -> Option<RadioConfigWire>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use leviculum_core::envelope;

    let mut frame_buf = Vec::new();
    frame(&envelope::encode_radio_query(), &mut frame_buf);
    if let Err(e) = port.write_all(&frame_buf).await {
        tracing::warn!("Serial {}: radio query write failed: {}", name, e);
        return None;
    }
    if let Err(e) = port.flush().await {
        tracing::warn!("Serial {}: radio query flush failed: {}", name, e);
        return None;
    }

    let mut deframer = Deframer::with_max_frame(SERIAL_HW_MTU as usize);
    let mut buf = vec![0u8; READ_BUF_SIZE];
    let deadline = tokio::time::Instant::now() + RADIO_REPORT_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            tracing::warn!(
                "Serial {}: no radio report within {:?} of the radio query",
                name,
                RADIO_REPORT_TIMEOUT
            );
            return None;
        }
        let n = match tokio::time::timeout(remaining, port.read(&mut buf)).await {
            Ok(Ok(0)) => {
                tracing::debug!("Serial {}: EOF while waiting for the radio report", name);
                return None;
            }
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                tracing::warn!("Serial {}: radio report read error: {}", name, e);
                return None;
            }
            Err(_) => continue,
        };
        for r in deframer.process(&buf[..n]) {
            let DeframeResult::Frame(data) = r else {
                continue;
            };
            match envelope::decode_frame(&data) {
                Ok(f) if f.frame_type == envelope::TYPE_RADIO_REPORT => {
                    match envelope::decode_radio_report_payload(f.payload) {
                        Some(wire) => return Some(wire),
                        None => {
                            tracing::warn!(
                                "Serial {}: radio report payload is not a radio config block",
                                name
                            );
                            return None;
                        }
                    }
                }
                Ok(f) if f.frame_type == envelope::TYPE_REFUSAL => {
                    if let Some((refused, reason)) = envelope::decode_refusal_payload(f.payload) {
                        if refused == envelope::TYPE_RADIO_QUERY {
                            tracing::warn!(
                                "Serial {}: board refused the radio query, reason 0x{:02x}",
                                name,
                                reason
                            );
                            return None;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// The profile this interface prices its airtime at, given what the board
/// said — or the refusal that keeps the interface off the air.
///
/// **A modem the host cannot price is a modem the host does not drive.**
/// Under-pricing is the dangerous direction: a bucket charging SF7 for
/// frames the board keys at SF12 under-counts duty by an order of magnitude
/// and hands the serial queue frames faster than the modem can key them.
/// Over-pricing by guessing is no better — it is a silent lie about airtime,
/// and nothing downstream can tell it from a measurement. So the two honest
/// outcomes are "price what the board reported" and "do not come up".
///
/// * `Ok((phy, None))` — the board runs `phy` and nobody has to be told.
/// * `Ok((phy, Some(warn)))` — the board runs `phy`, which is not what was
///   asked for; `warn` is the event naming both, for the caller to log.
/// * `Err(refusal)` — nobody answered; the interface does not come up and
///   the refusal says which frames went unanswered and for how long.
pub fn radio_pricing_phy(
    outcome: &RadioBringUp,
    requested: &RadioConfigWire,
    name: &str,
) -> Result<(RadioConfigWire, Option<String>), String> {
    match outcome {
        RadioBringUp::Adopted => Ok((*requested, None)),
        RadioBringUp::Running(running) if phy_differs(requested, running) => Ok((
            *running,
            Some(format!(
                "RADIO_BRINGUP iface={name} outcome=running-differs {} {} \
                 (the board did not adopt the config and reports another profile; \
                 airtime here is priced at the one it reports)",
                phy_keys("requested", requested),
                phy_keys("running", running)
            )),
        )),
        // Reported and identical: the config did arrive, only its ACK did
        // not. Nothing to warn about and nothing to move.
        RadioBringUp::Running(running) => Ok((*running, None)),
        RadioBringUp::Silent => Err(format!(
            "RADIO_BRINGUP iface={name} outcome=refused config_frame=legacy-radio-config \
             config_attempts={CONFIG_ATTEMPTS} config_wait_ms={} query_frame=0x{:02x} \
             query_wait_ms={} {} (the board answered neither frame, so this host cannot \
             name the profile it is running; an LNode restores its stored config at boot, \
             so the requested one is a guess. A modem the host cannot price is a modem the \
             host does not drive: this interface does not come up, and the daemon keeps \
             running without it)",
            CONFIG_ACK_TIMEOUT.as_millis(),
            leviculum_core::envelope::TYPE_RADIO_QUERY,
            RADIO_REPORT_TIMEOUT.as_millis(),
            phy_keys("requested", requested),
        )),
    }
}

/// Run `stty -F <port> low_latency`, reporting failure instead of
/// swallowing it (L-0023).
///
/// Failure modes: stty not spawnable (missing binary), or stty exiting
/// non-zero (non-Linux stty syntax, nonexistent device, EPERM). The `Err`
/// carries stty's own stderr so the operator sees its diagnosis.
fn set_low_latency(port: &str) -> Result<(), String> {
    match std::process::Command::new("stty")
        .args(["-F", port, "low_latency"])
        .output()
    {
        Err(e) => Err(format!("stty not runnable: {e}")),
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(format!(
            "stty exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

/// Reconnect wrapper for serial port connections.
///
/// Owns channel endpoints across reconnection cycles. On port loss, waits
/// RECONNECT_INTERVAL and retries. Follows the TCP reconnect pattern.
async fn serial_reconnect_task(
    id: InterfaceId,
    config: SerialInterfaceConfig,
    name: String,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    counters: Arc<InterfaceCounters>,
    credit: Option<Arc<Mutex<super::airtime::AirtimeCredit>>>,
) {
    let mut has_connected_before = false;
    // The low-latency warn fires once per task, not once per reconnect
    // cycle: the condition (missing stty, non-Linux stty syntax, EPERM on
    // the device) is static for the process lifetime, and the reconnect
    // loop ticks every RECONNECT_INTERVAL against a dead port.
    let mut low_latency_warned = false;
    loop {
        // Set low_latency mode so USB CDC-ACM traffic moves in full bulk
        // transfers instead of byte-by-byte. Observed on the T114 rig
        // (dfc3fb5): without it, HDLC frames trickled in one byte at a time
        // and the receiver's frame timeout discarded most frames. The mode
        // is load-bearing for LoRa-over-serial throughput, so a failure to
        // set it is an operator-visible finding, not a shrug (L-0023).
        // pyserial OFFERS this as set_low_latency_mode(); Python-RNS never
        // calls it, so this is rig-driven, not reference parity.
        if let Err(e) = set_low_latency(&config.port) {
            if !low_latency_warned {
                low_latency_warned = true;
                tracing::warn!(
                    "Serial interface {}: could not set low_latency on {}: {} — \
                     expect degraded HDLC framing on USB CDC-ACM",
                    name,
                    config.port,
                    e
                );
            }
        } else {
            low_latency_warned = false;
        }

        let builder = tokio_serial::new(&config.port, config.speed)
            .data_bits(config.data_bits)
            .stop_bits(config.stop_bits)
            .parity(config.parity)
            .flow_control(tokio_serial::FlowControl::None);

        match tokio_serial::SerialStream::open(&builder) {
            Ok(mut port) => {
                let is_reconnect = has_connected_before;
                has_connected_before = true;
                counters.set_online(true);
                tracing::info!("Serial interface {} online on {}", name, config.port);

                if is_reconnect {
                    if let Some(ref notify) = config.reconnect_notify {
                        let _ = notify.try_send(id);
                    }
                }

                // Send radio config if configured (test infrastructure)
                if let Some(ref radio_cfg) = config.radio_config {
                    let requested = requested_wire(radio_cfg);
                    let outcome = radio_bring_up(&mut port, &requested, &name).await;
                    match radio_pricing_phy(&outcome, &requested, &name) {
                        Ok((running, mismatch)) => {
                            if let Some(mismatch) = mismatch {
                                tracing::warn!("{}", mismatch);
                            }
                            // The one production caller. The bucket was built
                            // from the requested profile in
                            // `spawn_serial_interface`, so on the ACK path this
                            // moves nothing; on the report path it is the whole
                            // point — the interface runs on what the board said
                            // it is running, not on what it was asked for
                            // (L-0007, Refs #334).
                            if let Some(credit) = credit.as_ref() {
                                credit.lock_recover().update_radio_params(
                                    running.bandwidth_hz,
                                    running.sf,
                                    running.cr,
                                    running.preamble_len,
                                );
                            }
                        }
                        Err(refusal) => {
                            // Neither frame answered. The interface does not
                            // come up — not this cycle and not later: the
                            // reconnect loop is left behind with the port, so
                            // nothing here carries a frame it cannot price.
                            // `set_online(false)` is what `rnstatus` reads
                            // (L-0020), so the refusal is visible as a Down
                            // interface and not only as a log line. The daemon
                            // keeps running without it, the way an unparsable
                            // config refuses without taking the rest of the
                            // process with it (d828140e).
                            tracing::error!("{}", refusal);
                            counters.set_online(false);
                            return;
                        }
                    }
                }

                outgoing_rx = serial_io_task(
                    name.clone(),
                    port,
                    incoming_tx.clone(),
                    outgoing_rx,
                    Arc::clone(&counters),
                    config.test_drop_direct_ingress,
                )
                .await;
                counters.set_online(false);
                tracing::warn!("Serial interface {}: port lost, will reconnect", name);
            }
            Err(e) => {
                tracing::warn!(
                    "Serial interface {}: open {} failed: {}",
                    name,
                    config.port,
                    e
                );
            }
        }

        if incoming_tx.is_closed() {
            tracing::debug!("Serial interface {}: event loop shut down", name);
            return;
        }
        tracing::info!(
            "Serial interface {}: reconnecting in {}s",
            name,
            RECONNECT_INTERVAL.as_secs()
        );
        tokio::time::sleep(RECONNECT_INTERVAL).await;
    }
}

/// How long a media verb waits for the board's own report before giving
/// up on it. The radio config's ACK budget, for the same reason: the
/// answer is composed by the firmware's USB task the moment the frame
/// lands, so anything beyond this is a board that is not going to answer.
/// The io task's read and write paths are parked for the wait, which is
/// why it is a budget and not a retry loop.
const MEDIA_ANSWER_TIMEOUT: Duration = Duration::from_secs(2);

/// Everything the read path needs to hand a frame to the transport,
/// borrowed from the io task.
///
/// A struct because the media verbs' own read loop takes it whole: while a
/// verb waits for the board's report, ordinary traffic keeps arriving on
/// the same port, and dropping it — or counting it differently — would
/// make a silenced-node measurement a measurement of the silencing.
struct Ingress<'a> {
    name: &'a str,
    incoming_tx: &'a mpsc::Sender<IncomingPacket>,
    counters: &'a InterfaceCounters,
    drop_direct_ingress: bool,
}

impl Ingress<'_> {
    /// Hand one deframed frame to the transport, with the TEST-ONLY
    /// ingress filter and the rx counter applied. `false` means the
    /// transport channel is gone and the io task must return.
    async fn deliver(&self, data: Vec<u8>) -> bool {
        if super::test_drop_direct_ingress_frame(
            self.drop_direct_ingress,
            self.name,
            &data,
            self.counters,
        ) {
            return true;
        }
        self.counters
            .rx_bytes
            .fetch_add(data.len() as u64, Ordering::Relaxed);
        self.incoming_tx.send(IncomingPacket { data }).await.is_ok()
    }
}

/// What a board said when asked about its media profile.
enum MediaAnswer {
    /// The board's own report: `(running, configured)`. See
    /// [`leviculum_core::envelope::TYPE_MEDIA_REPORT`] for why those are
    /// two values.
    Report(
        leviculum_core::envelope::MediaProfileWire,
        leviculum_core::envelope::MediaProfileWire,
    ),
    /// The board refused by name, answered something malformed, or said
    /// nothing within [`MEDIA_ANSWER_TIMEOUT`]. All three are "this board
    /// did not tell us what it is doing", and the verbs treat them alike.
    NoAnswer,
    /// The port died during the exchange; the io task must return and let
    /// the reconnect loop have it.
    PortLost,
}

/// Write one media frame and wait for the board's [`
/// leviculum_core::envelope::TYPE_MEDIA_REPORT`], forwarding everything
/// else that arrives meanwhile to the transport.
///
/// `asked_type` is the frame type being answered, so a refusal aimed at
/// *this* frame ends the wait while a refusal of something else does not.
///
/// The partial-frame rule of the main loop is kept inside the wait: a
/// read that goes quiet for [`FRAME_TIMEOUT`] mid-frame discards the
/// partial, exactly as the io task's own timeout arm does. Without it a
/// stale half-frame at entry would glue itself to the report and the
/// board would read as not having answered.
async fn ask_board_media<S>(
    port: &mut S,
    payload: &[u8],
    asked_type: u8,
    frame_buf: &mut Vec<u8>,
    deframer: &mut Deframer,
    ingress: &Ingress<'_>,
) -> MediaAnswer
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use leviculum_core::envelope;

    let name = ingress.name;

    frame(payload, frame_buf);
    if let Err(e) = port.write_all(frame_buf).await {
        tracing::warn!("Serial {}: media frame write failed: {}", name, e);
        return MediaAnswer::PortLost;
    }
    if let Err(e) = port.flush().await {
        tracing::warn!("Serial {}: media frame flush failed: {}", name, e);
        return MediaAnswer::PortLost;
    }

    /// What one frame off the port was, as far as this wait cares.
    enum Seen {
        Report(Option<(envelope::MediaProfileWire, envelope::MediaProfileWire)>),
        Refused(u8),
        Other,
    }

    let deadline = Instant::now() + MEDIA_ANSWER_TIMEOUT;
    let mut read_buf = vec![0u8; READ_BUF_SIZE];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            tracing::warn!(
                "Serial {}: no media report within {:?} of frame type 0x{:02x}",
                name,
                MEDIA_ANSWER_TIMEOUT,
                asked_type
            );
            return MediaAnswer::NoAnswer;
        }
        let slice = if deframer.is_in_frame() {
            remaining.min(FRAME_TIMEOUT)
        } else {
            remaining
        };
        let n = match tokio::time::timeout(slice, port.read(&mut read_buf)).await {
            Err(_) => {
                if deframer.is_in_frame() {
                    tracing::trace!("Serial {}: frame timeout, discarding partial frame", name);
                    deframer.reset();
                }
                continue;
            }
            Ok(Ok(0)) => {
                tracing::debug!("Serial interface {} EOF", name);
                return MediaAnswer::PortLost;
            }
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                tracing::debug!("Serial interface {} read error: {}", name, e);
                return MediaAnswer::PortLost;
            }
        };

        // Every frame this read produced is dealt with before returning:
        // an answer arriving in the same read as a data packet must not
        // take that packet down with it.
        let mut answer = None;
        for r in deframer.process(&read_buf[..n]) {
            let DeframeResult::Frame(data) = r else {
                if matches!(r, DeframeResult::Oversized) {
                    tracing::trace!("Serial {}: frame exceeds HW_MTU, discarded", name);
                }
                continue;
            };
            let seen = if answer.is_some() {
                Seen::Other
            } else {
                match envelope::decode_frame(&data) {
                    Ok(f) if f.frame_type == envelope::TYPE_MEDIA_REPORT => {
                        Seen::Report(envelope::decode_media_report_payload(f.payload))
                    }
                    Ok(f) if f.frame_type == envelope::TYPE_REFUSAL => {
                        match envelope::decode_refusal_payload(f.payload) {
                            Some((refused, reason)) if refused == asked_type => {
                                Seen::Refused(reason)
                            }
                            _ => Seen::Other,
                        }
                    }
                    _ => Seen::Other,
                }
            };
            match seen {
                Seen::Report(Some((running, configured))) => {
                    answer = Some(MediaAnswer::Report(running, configured));
                }
                Seen::Report(None) => {
                    tracing::warn!(
                        "Serial {}: media report payload is not two known flag bytes",
                        name
                    );
                    answer = Some(MediaAnswer::NoAnswer);
                }
                Seen::Refused(reason) => {
                    tracing::warn!(
                        "Serial {}: board refused frame type 0x{:02x}, reason 0x{:02x}",
                        name,
                        asked_type,
                        reason
                    );
                    answer = Some(MediaAnswer::NoAnswer);
                }
                Seen::Other => {
                    if !ingress.deliver(data).await {
                        return MediaAnswer::PortLost;
                    }
                }
            }
        }
        if let Some(answer) = answer {
            return answer;
        }
    }
}

/// One media profile as the scalar log keys periculum greps for.
fn media_keys(prefix: &str, profile: leviculum_core::envelope::MediaProfileWire) -> String {
    format!(
        "{prefix}_lora={} {prefix}_ble={}",
        u8::from(profile.lora_enabled),
        u8::from(profile.ble_enabled)
    )
}

/// Bidirectional serial I/O task.
///
/// Read path: serial read → HDLC deframe → incoming channel
/// Write path: outgoing channel → HDLC frame → serial write → flush
///
/// Enforces:
/// - Frame timeout: partial frames discarded after 100ms of silence (Python parity)
/// - HW_MTU: deframer buffer exceeding 564 bytes is reset (prevents OOM on embedded)
///
/// Returns `outgoing_rx` on port loss for reconnect reuse.
async fn serial_io_task<S>(
    name: String,
    mut port: S,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    counters: Arc<InterfaceCounters>,
    drop_direct_ingress: bool,
) -> mpsc::Receiver<OutgoingPacket>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    super::log_direct_ingress_filter_armed(drop_direct_ingress, &name);
    let mut deframer = Deframer::with_max_frame(SERIAL_HW_MTU as usize);
    let mut read_buf = vec![0u8; READ_BUF_SIZE];
    let mut frame_buf = Vec::with_capacity(MTU * FRAME_BUFFER_MULTIPLIER);
    let mut last_read_at = Instant::now();
    // Subscribed here rather than at spawn: a request made while the
    // port was down was not made about the board that just came back,
    // and acting on it would reboot (or announce from) a board nobody
    // asked about.
    let mut firmware_requests = firmware_request_channel().subscribe();
    // Borrowed once for the whole task: the read path and the media
    // verbs' wait hand frames on through the same one.
    let ingress = Ingress {
        name: &name,
        incoming_tx: &incoming_tx,
        counters: &counters,
        drop_direct_ingress,
    };

    loop {
        // Compute timeout: if mid-frame, use FRAME_TIMEOUT; otherwise wait indefinitely
        let timeout = if deframer.is_in_frame() {
            let elapsed = last_read_at.elapsed();
            if elapsed >= FRAME_TIMEOUT {
                // Already expired, reset immediately
                tracing::trace!("Serial {}: frame timeout, discarding partial frame", name);
                deframer.reset();
                tokio::time::sleep(Duration::from_millis(1)).await;
                continue;
            }
            FRAME_TIMEOUT - elapsed
        } else {
            Duration::from_secs(3600) // effectively infinite
        };

        tokio::select! {
            // Read path
            result = port.read(&mut read_buf) => {
                match result {
                    Ok(0) => {
                        tracing::debug!("Serial interface {} EOF", name);
                        return outgoing_rx;
                    }
                    Ok(n) => {
                        last_read_at = Instant::now();
                        let results = deframer.process(&read_buf[..n]);
                        for r in results {
                            match r {
                                DeframeResult::Frame(data) => {
                                    // The TEST-ONLY range emulation (an
                                    // out-of-range frame was never heard, so it
                                    // is dropped before any counter or the
                                    // transport sees it) and the rx counter both
                                    // live in the shared helper, which the media
                                    // verbs' wait uses too.
                                    if !ingress.deliver(data).await {
                                        return outgoing_rx;
                                    }
                                }
                                // HW_MTU enforcement lives in the deframer now.
                                DeframeResult::Oversized => tracing::trace!(
                                    "Serial {}: frame exceeds HW_MTU, discarded", name
                                ),
                                _ => {}
                            }
                        }
                    }
                    Err(e) => {
                        tracing::debug!("Serial interface {} read error: {}", name, e);
                        return outgoing_rx;
                    }
                }
            }

            // Write path
            msg = outgoing_rx.recv() => {
                match msg {
                    Some(pkt) => {
                        tracing::debug!("Serial interface {} TX {} bytes", name, pkt.data.len());
                        frame(&pkt.data, &mut frame_buf);
                        // Counted before the write: the moment the peer can
                        // observe any byte of this frame, the counter must
                        // already cover it (Codeberg #389, same ordering as
                        // TCP). On a write error the dying port charges one
                        // frame whose tail never left — bounded by that frame.
                        counters.tx_bytes.fetch_add(frame_buf.len() as u64, Ordering::Relaxed);
                        if let Err(e) = port.write_all(&frame_buf).await {
                            tracing::debug!("Serial interface {} write error: {}", name, e);
                            return outgoing_rx;
                        }
                        if let Err(e) = port.flush().await {
                            tracing::debug!("Serial interface {} flush error: {}", name, e);
                            return outgoing_rx;
                        }
                    }
                    None => {
                        tracing::debug!("Serial interface {} outgoing channel closed", name);
                        return outgoing_rx;
                    }
                }
            }

            // Commanded firmware request, made out of band.
            //
            // Written on the same port, from the same task, as every
            // outgoing packet: nothing else may hold this port, so nothing
            // else can send it, and doing it here rather than from the
            // signal handler's task is what keeps it from interleaving
            // with a frame already half-written.
            //
            // Neither frame is waited on here. A reset is answered with
            // RADIO_RESET_ACK and then the board reboots, taking its USB
            // device with it; an announce is answered with the envelope
            // ack or a named refusal. Both answers arrive on the read
            // path above as a short frame the transport will not
            // recognise and discards. For the reset the reboot IS the
            // outcome and the caller watches for it on the bus; for the
            // announce it is the announce reaching a peer, which this
            // port cannot see either way. A wait here would only add a
            // way for the port's own task to stall.
            recv = firmware_requests.recv() => {
                // A lagged receiver has missed requests it cannot name,
                // and with more than one kind in the channel it must not
                // guess: serving a `Reset` for what was an `Announce`
                // reboots a board mid-measurement and every assertion
                // after it silently measures a mesh that was taken down
                // by the harness. Doing nothing is the detectable
                // failure instead — both callers observe the OUTCOME
                // (a reboot on the USB bus, a peer learning a
                // destination) and fail by name when it does not come.
                // While the channel carried one kind this arm did serve
                // a lagged reset; that reading died with the second
                // variant.
                let request = match recv {
                    Ok(request) => Some(request),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            "Serial {}: {} out-of-band firmware request(s) missed while \
                             this port was busy; not guessing which",
                            name, n
                        );
                        None
                    }
                    // The sender is a `OnceLock` static that is never
                    // dropped, so this is unreachable; stopping the select
                    // arm rather than spinning on it is the safe reading.
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::debug!("Serial {}: firmware request channel closed", name);
                        None
                    }
                };
                match request {
                    None => {}
                    // The write-and-forget pair: the outcome is on the
                    // bus or on the air, never on this port.
                    Some(request @ (FirmwareRequest::Reset | FirmwareRequest::Announce)) => {
                        let (what, payload) = match request {
                            FirmwareRequest::Reset => (
                                "reset",
                                leviculum_core::rnode::RADIO_RESET_FRAME.to_vec(),
                            ),
                            _ => (
                                "announce",
                                leviculum_core::envelope::encode_announce(),
                            ),
                        };
                        tracing::info!(
                            "Serial {}: commanded firmware {} requested, sending frame",
                            name, what
                        );
                        frame(&payload, &mut frame_buf);
                        if let Err(e) = port.write_all(&frame_buf).await {
                            tracing::warn!("Serial {}: {} frame write failed: {}", name, what, e);
                            return outgoing_rx;
                        }
                        if let Err(e) = port.flush().await {
                            tracing::warn!("Serial {}: {} frame flush failed: {}", name, what, e);
                            return outgoing_rx;
                        }
                        tracing::info!("Serial {}: {} frame sent", name, what);
                    }
                    // The media pair, which DOES wait: the profile that
                    // has to be put back afterwards exists nowhere but in
                    // the board's answer, so a silence that did not read
                    // it is a silence nobody can undo.
                    Some(FirmwareRequest::MediaSilence) => {
                        tracing::info!(
                            "Serial {}: commanded media silence requested, reading the board's profile",
                            name
                        );
                        let asked = ask_board_media(
                            &mut port, &leviculum_core::envelope::encode_media_query(),
                            leviculum_core::envelope::TYPE_MEDIA_QUERY, &mut frame_buf,
                            &mut deframer, &ingress,
                        ).await;
                        let configured = match asked {
                            MediaAnswer::PortLost => return outgoing_rx,
                            MediaAnswer::NoAnswer => {
                                // Nothing was written, so nothing has to
                                // be put back. Silencing a board whose
                                // previous profile we failed to learn
                                // would be the one unrecoverable outcome
                                // this verb can produce.
                                tracing::warn!(
                                    "MEDIA_SILENCE iface={} outcome=no-report \
                                     (board did not report its profile; nothing written)",
                                    name
                                );
                                continue;
                            }
                            MediaAnswer::Report(_, configured) => configured,
                        };
                        // Remembered BEFORE the silence goes out: see
                        // `SILENCED_MEDIA_PROFILES` for why that ordering
                        // is the safe one. `configured` and not `running`
                        // — writing a profile sets what a reboot comes up
                        // with, so restoring `running` would silently drop
                        // a carrier that was configured on but had not
                        // come up.
                        silenced_media_profiles()
                            .lock_recover()
                            .insert(name.clone(), configured);
                        let silent = leviculum_core::envelope::MediaProfileWire {
                            lora_enabled: false,
                            ble_enabled: false,
                        };
                        let applied = ask_board_media(
                            &mut port,
                            &leviculum_core::envelope::encode_media_profile(&silent),
                            leviculum_core::envelope::TYPE_MEDIA_PROFILE, &mut frame_buf,
                            &mut deframer, &ingress,
                        ).await;
                        match applied {
                            MediaAnswer::PortLost => return outgoing_rx,
                            MediaAnswer::NoAnswer => tracing::warn!(
                                "MEDIA_SILENCE iface={} outcome=unconfirmed {} \
                                 (frame sent, no report back; the profile stays remembered)",
                                name, media_keys("remembered", configured)
                            ),
                            MediaAnswer::Report(running, now_configured) => tracing::info!(
                                "MEDIA_SILENCE iface={} outcome=applied {} {} {}",
                                name,
                                media_keys("remembered", configured),
                                media_keys("running", running),
                                media_keys("configured", now_configured)
                            ),
                        }
                    }
                    Some(FirmwareRequest::MediaRestore) => {
                        let Some(profile) = remembered_media_profile(&name) else {
                            // The documented answer to "restore with no
                            // prior silence": a no-op, said out loud. The
                            // clever alternative — write the both-on
                            // default — would be this daemon changing a
                            // board's configuration on the strength of a
                            // guess, and the guess is wrong for exactly
                            // the boards a media profile exists for.
                            tracing::warn!(
                                "MEDIA_RESTORE iface={} outcome=nothing-remembered \
                                 (no silence from this daemon; nothing written)",
                                name
                            );
                            continue;
                        };
                        tracing::info!(
                            "Serial {}: commanded media restore requested, {}",
                            name, media_keys("remembered", profile)
                        );
                        let restored = ask_board_media(
                            &mut port,
                            &leviculum_core::envelope::encode_media_profile(&profile),
                            leviculum_core::envelope::TYPE_MEDIA_PROFILE, &mut frame_buf,
                            &mut deframer, &ingress,
                        ).await;
                        match restored {
                            MediaAnswer::PortLost => return outgoing_rx,
                            // The memory is kept: an unconfirmed restore
                            // is a board that may still owe one, and a
                            // second signal must be able to try again.
                            MediaAnswer::NoAnswer => tracing::warn!(
                                "MEDIA_RESTORE iface={} outcome=unconfirmed {} \
                                 (frame sent, no report back; still remembered)",
                                name, media_keys("restored", profile)
                            ),
                            MediaAnswer::Report(running, configured) => {
                                silenced_media_profiles().lock_recover().remove(&name);
                                tracing::info!(
                                    "MEDIA_RESTORE iface={} outcome=restored {} {} {}",
                                    name,
                                    media_keys("restored", profile),
                                    media_keys("running", running),
                                    media_keys("configured", configured)
                                );
                            }
                        }
                    }
                }
            }

            // Frame timeout
            _ = tokio::time::sleep(timeout) => {
                if deframer.is_in_frame() {
                    tracing::trace!("Serial {}: frame timeout, discarding partial frame", name);
                    deframer.reset();
                }
            }
        }
    }
}

/// Parse a parity string ("N", "E"/"even", "O"/"odd") to tokio_serial::Parity.
pub(crate) fn parse_parity(s: &str) -> tokio_serial::Parity {
    match s.to_lowercase().as_str() {
        "e" | "even" => tokio_serial::Parity::Even,
        "o" | "odd" => tokio_serial::Parity::Odd,
        _ => tokio_serial::Parity::None,
    }
}

/// Parse a data bits value to tokio_serial::DataBits.
pub(crate) fn parse_data_bits(n: u8) -> tokio_serial::DataBits {
    match n {
        5 => tokio_serial::DataBits::Five,
        6 => tokio_serial::DataBits::Six,
        7 => tokio_serial::DataBits::Seven,
        _ => tokio_serial::DataBits::Eight,
    }
}

/// Parse a stop bits value to tokio_serial::StopBits.
pub(crate) fn parse_stop_bits(n: u8) -> tokio_serial::StopBits {
    match n {
        2 => tokio_serial::StopBits::Two,
        _ => tokio_serial::StopBits::One,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L-0023: the stty result carries the diagnosis; discarding it (the
    /// old `let _ =`) hid every failure of a mode the comment above calls
    /// load-bearing. A device that cannot exist must yield the error the
    /// reconnect loop warns with.
    #[test]
    fn low_latency_failure_is_reported_not_swallowed() {
        let err = set_low_latency("/dev/nonexistent-lnode-l0023")
            .expect_err("a nonexistent device cannot accept low_latency");
        assert!(
            err.contains("stty"),
            "the error must name the failing tool: {err}"
        );
    }

    /// An LNode is lawful out of the box: with no `airtime_limit_long` in
    /// the config, the frequency's own ETSI sub-band limit is what gets
    /// pushed, not the 0 (unlimited) this used to hardcode.
    #[test]
    fn absent_airtime_limit_pushes_the_lawful_limit_for_the_frequency() {
        let at = |frequency, explicit| {
            serial_radio_config(&crate::config::InterfaceConfig {
                interface_type: "SerialInterface".to_string(),
                port: Some("/dev/ttyACM0".to_string()),
                frequency: Some(frequency),
                airtime_limit_long: explicit,
                ..Default::default()
            })
            .expect("frequency present → radio config")
            .lt_alock
        };
        // 869.525 MHz is ETSI sub-band P: 10% -> 0.10 * 10000.
        assert_eq!(at(869_525_000, None), 1000);
        // 868.1 MHz is sub-band M: 1%. The limit follows the frequency, so a
        // node that moves band moves limit without touching its config.
        assert_eq!(at(868_100_000, None), 100);
        // Outside the band this build can cite, nothing is invented.
        assert_eq!(at(915_000_000, None), 0);
        // An explicit value still wins, including an explicit 0: a bench
        // that means unlimited says so, and the host does not second-guess.
        assert_eq!(at(869_525_000, Some(5.0)), 500);
        assert_eq!(at(869_525_000, Some(0.0)), 0);
    }

    /// The ten bandwidths `RadioConfig::from_wire_config`
    /// (`leviculum-nrf/src/lora.rs`) has an SX1262 register code for, and the
    /// only ones `bw_code_to_hz` (`leviculum-nrf/src/sx1262.rs:206`) ever
    /// returns. Written out here rather than imported so this test states the
    /// firmware's acceptance set independently of whatever the host computes.
    const FIRMWARE_ACCEPTS_HZ: [u32; 10] = [
        7_810, 10_420, 15_630, 20_830, 31_250, 41_670, 62_500, 125_000, 250_000, 500_000,
    ];

    /// L-0007, the door: five of the ten LoRa bandwidths are spelled
    /// differently by the SX127x/Arduino-LoRa table Python-RNS and
    /// RNode_Firmware use than by the SX1262 register table our own firmware
    /// decodes. `validate_config` accepts the RNode spelling, so a
    /// `SerialInterface` block naming one builds, spawns, and puts a value on
    /// the wire that `RadioConfig::from_wire_config` returns `None` for. The
    /// legacy contract for a config the driver cannot take is silence
    /// (`leviculum-nrf/src/usb.rs:686-707`), so the board stays on the PHY it
    /// already had while the host's airtime bucket goes on pricing every
    /// frame at the bandwidth it asked for.
    #[test]
    fn rnode_spelled_bandwidths_reach_the_wire_as_codes_the_board_accepts() {
        for rnode_spelling in [7_800u32, 10_400, 15_600, 20_800, 41_700] {
            let cfg = crate::config::InterfaceConfig {
                interface_type: "SerialInterface".to_string(),
                port: Some("/dev/ttyACM0".to_string()),
                frequency: Some(869_525_000),
                bandwidth: Some(rnode_spelling),
                ..Default::default()
            };
            // The host lets it through: this is the block an operator writes
            // after copying a working rnsd config across.
            assert!(
                leviculum_core::rnode::validate_config(869_525_000, rnode_spelling, 17, 8, 5)
                    .is_ok(),
                "{rnode_spelling} Hz is a bandwidth the host accepts"
            );
            let radio = serial_radio_config(&cfg).expect("frequency present → radio config");
            assert!(
                FIRMWARE_ACCEPTS_HZ.contains(&radio.bandwidth),
                "bandwidth {rnode_spelling} Hz reaches the wire as {} Hz, \
                 which the LNode firmware refuses without a word",
                radio.bandwidth
            );
        }
    }

    /// The five that already agree are not moved: a config naming one of them
    /// has to arrive at the modem as itself, or the substitution introduced
    /// for the other five has become a rewrite of every bandwidth.
    #[test]
    fn agreeing_bandwidths_pass_through_unchanged() {
        for agreed in [31_250u32, 62_500, 125_000, 250_000, 500_000] {
            let cfg = crate::config::InterfaceConfig {
                interface_type: "SerialInterface".to_string(),
                port: Some("/dev/ttyACM0".to_string()),
                frequency: Some(869_525_000),
                bandwidth: Some(agreed),
                ..Default::default()
            };
            let radio = serial_radio_config(&cfg).expect("frequency present → radio config");
            assert_eq!(radio.bandwidth, agreed);
        }
    }

    /// The whole point of the key: a `preamble_symbols` written in a config
    /// file has to survive as far as the bytes on the wire. This drives the
    /// same `serial_radio_config` the driver calls and then the same
    /// `build_radio_config_frame` `send_radio_config` calls, and reads the
    /// preamble back out of the frame — so it fails if any link of the
    /// chain drops the value, not merely if the struct field is unset.
    #[test]
    fn preamble_symbols_travels_from_config_to_wire_frame() {
        let cfg = crate::config::InterfaceConfig {
            interface_type: "SerialInterface".to_string(),
            port: Some("/dev/ttyACM0".to_string()),
            frequency: Some(869_525_000),
            bandwidth: Some(125_000),
            spreading_factor: Some(10),
            coding_rate: Some(8),
            tx_power: Some(17),
            preamble_symbols: Some(18),
            ..Default::default()
        };
        let radio = serial_radio_config(&cfg).expect("frequency present → radio config");
        assert_eq!(radio.preamble_len, 18);

        let payload = leviculum_core::rnode::build_radio_config_frame(
            &leviculum_core::rnode::RadioConfigWire {
                frequency_hz: radio.frequency as u32,
                bandwidth_hz: radio.bandwidth,
                sf: radio.spreading_factor,
                cr: radio.coding_rate,
                tx_power_dbm: radio.tx_power,
                preamble_len: radio.preamble_len,
                csma_enabled: radio.csma_enabled,
                radio_silent: false,
                st_alock: 0,
                lt_alock: 0,
                lt_alock_present: true,
            },
        );
        // Strip the 2-byte magic the parser expects to be gone.
        let parsed =
            leviculum_core::rnode::parse_radio_config(&payload[2..]).expect("frame parses back");
        assert_eq!(parsed.preamble_len, 18);
    }

    /// A block that omits the key gets the preamble the RNode firmware would
    /// program for the same PHY, not a constant. The block's own defaults
    /// are the firmware's compiled profile — the ReticulumNet consensus,
    /// SF8/BW125 — where the derivation lands on the 18-symbol floor.
    #[test]
    fn absent_preamble_symbols_derives_the_reference_value() {
        let cfg = crate::config::InterfaceConfig {
            interface_type: "SerialInterface".to_string(),
            port: Some("/dev/ttyACM0".to_string()),
            frequency: Some(869_463_000),
            ..Default::default()
        };
        let radio = serial_radio_config(&cfg).expect("frequency present → radio config");
        assert_eq!(radio.spreading_factor, 8);
        assert_eq!(radio.preamble_len, 18);
    }

    /// The defect this closes, at the interface boundary. The same block at
    /// SF10 used to push 24 while the RNode on the far end programmed 18,
    /// and a mixed pair resolved 4 of 20 path requests. Deriving gives 18 on
    /// both sides. Written out per spreading factor rather than looped, so a
    /// regression names the SF it broke.
    #[test]
    fn absent_preamble_symbols_scales_with_the_spreading_factor() {
        let derived_at = |sf: u8| {
            let cfg = crate::config::InterfaceConfig {
                interface_type: "SerialInterface".to_string(),
                port: Some("/dev/ttyACM0".to_string()),
                frequency: Some(869_525_000),
                bandwidth: Some(125_000),
                spreading_factor: Some(sf),
                coding_rate: Some(8),
                ..Default::default()
            };
            serial_radio_config(&cfg)
                .expect("frequency present → radio config")
                .preamble_len
        };
        assert_eq!(derived_at(7), 24);
        assert_eq!(derived_at(8), 18);
        assert_eq!(derived_at(9), 18);
        assert_eq!(derived_at(10), 18);
        assert_eq!(derived_at(11), 18);
        assert_eq!(derived_at(12), 18);
    }

    /// The override still wins over the derivation, including when it names
    /// the value the derivation would have rejected. That is what makes the
    /// corner re-measurable — an A/B over the preamble needs a way to pin the
    /// old 24 on the fixed build — and what lets a node with a
    /// non-conforming peer cope.
    #[test]
    fn explicit_preamble_symbols_overrides_the_derivation() {
        let pinned = |sf: u8, preamble: u16| {
            let cfg = crate::config::InterfaceConfig {
                interface_type: "SerialInterface".to_string(),
                port: Some("/dev/ttyACM0".to_string()),
                frequency: Some(869_525_000),
                bandwidth: Some(125_000),
                spreading_factor: Some(sf),
                coding_rate: Some(8),
                preamble_symbols: Some(preamble),
                ..Default::default()
            };
            serial_radio_config(&cfg)
                .expect("frequency present → radio config")
                .preamble_len
        };
        // The pre-fix constant, pinned back on at the SF where it is wrong.
        assert_eq!(pinned(10, 24), 24);
        // And a value below the reference's own floor, which the derivation
        // would never produce.
        assert_eq!(pinned(10, 8), 8);
    }

    /// A LoRa `SerialInterface` block, SF and preamble to taste.
    fn preamble_cfg(sf: u8, preamble: Option<u16>) -> crate::config::InterfaceConfig {
        crate::config::InterfaceConfig {
            interface_type: "SerialInterface".to_string(),
            port: Some("/dev/ttyACM0".to_string()),
            frequency: Some(869_525_000),
            bandwidth: Some(125_000),
            spreading_factor: Some(sf),
            coding_rate: Some(8),
            preamble_symbols: preamble,
            ..Default::default()
        }
    }

    /// The measured ceiling of Codeberg #315, encoded where a config file can
    /// still cross it: above ~20 symbols an SX127x receiver decodes nothing,
    /// and the only way to key that is a pin.
    ///
    /// The rung either side of the ceiling is the whole test — 21 warns, 20 is
    /// silent — and the positive control is that this same function stays
    /// quiet on every configuration the corpus runs green.
    #[test]
    fn a_preamble_pin_above_the_sx127x_ceiling_warns() {
        let warning = preamble_ceiling_warning("serial_0", &preamble_cfg(10, Some(21)))
            .expect("21 symbols at SF10 is above the measured ceiling");
        assert!(
            warning.contains("#315"),
            "the warning names the issue: {warning}"
        );
        assert!(
            warning.contains("20 symbols"),
            "the warning names the measured ceiling: {warning}"
        );
        assert!(
            warning.contains("SX127x"),
            "the warning names who goes deaf: {warning}"
        );
        assert!(
            warning.contains("serial_0"),
            "the warning names the interface: {warning}"
        );

        // The pin the preamble24 cell keys, one rung further out.
        let at_24 = preamble_ceiling_warning("serial_0", &preamble_cfg(10, Some(24)))
            .expect("24 symbols at SF10 warns");
        assert!(at_24.contains("196 ms"), "names the airtime: {at_24}");
    }

    /// Positive control for the silence: the same function on the same PHY one
    /// symbol lower, on the derived value, and on a block with no pin at all.
    /// Without this, a function that returned `None` unconditionally would
    /// pass the test above's negative half.
    #[test]
    fn a_preamble_at_or_below_the_ceiling_is_silent() {
        assert_eq!(
            preamble_ceiling_warning("serial_0", &preamble_cfg(10, Some(20))),
            None,
            "20 symbols is the last rung that decoded — marginal, not warned"
        );
        assert_eq!(
            preamble_ceiling_warning("serial_0", &preamble_cfg(10, Some(18))),
            None,
            "the derived value must never warn"
        );
        assert_eq!(
            preamble_ceiling_warning("serial_0", &preamble_cfg(10, None)),
            None,
            "no pin, no warning"
        );
    }

    /// The measurement is a duration, not a symbol count, so the warning is
    /// too: `lora_path_discovery_fast_mixed` keys 24 symbols at SF7/BW125 —
    /// 24.6 ms — into the same SX1276 and is green in every corpus run. A
    /// warning that fired on the count alone would call that config broken.
    #[test]
    fn a_long_preamble_at_a_fast_spreading_factor_is_silent() {
        assert_eq!(
            preamble_ceiling_warning("serial_0", &preamble_cfg(7, Some(24))),
            None,
            "24 symbols at SF7 is 24.6 ms on air, far under the ceiling"
        );
        // Same count, slow SF: the duration is what moved, and so is the verdict.
        assert!(preamble_ceiling_warning("serial_0", &preamble_cfg(10, Some(24))).is_some());
    }

    /// A block with no `frequency` is a plain serial pipe: `serial_radio_config`
    /// returns `None` for it, no radio config is sent, and an inert key must
    /// not produce a warning about the air.
    #[test]
    fn a_pin_on_a_non_lora_serial_block_is_silent() {
        let cfg = crate::config::InterfaceConfig {
            interface_type: "SerialInterface".to_string(),
            port: Some("/dev/ttyACM0".to_string()),
            preamble_symbols: Some(64),
            ..Default::default()
        };
        assert!(serial_radio_config(&cfg).is_none());
        assert_eq!(preamble_ceiling_warning("serial_0", &cfg), None);
    }

    /// An LNode whose block names no `txpower` is programmed to the board
    /// maximum, which is also the firmware's own compiled default — the
    /// invariant this function's doc comment states, that every PHY default
    /// here is `RadioConfig::eu_medium`'s.
    #[test]
    fn absent_txpower_programs_the_board_maximum() {
        let cfg = crate::config::InterfaceConfig {
            interface_type: "SerialInterface".to_string(),
            port: Some("/dev/ttyACM0".to_string()),
            frequency: Some(869_525_000),
            ..Default::default()
        };
        let radio = serial_radio_config(&cfg).expect("frequency present → radio config");
        assert_eq!(radio.tx_power, 22);
        assert_eq!(
            radio.tx_power,
            leviculum_core::rnode::DEFAULT_TX_POWER_DBM,
            "the serial path and the resolver must not drift apart"
        );
    }

    /// The frequency cap reaches the serial path: the same block on a 25 mW
    /// sub-band (867.2 MHz, ERC 70-03 h1.4) resolves an absent `txpower` to
    /// the lawful 14 dBm, not the board maximum it gets at 869.525 MHz.
    #[test]
    fn absent_txpower_is_capped_on_a_25_mw_band() {
        let cfg = crate::config::InterfaceConfig {
            interface_type: "SerialInterface".to_string(),
            port: Some("/dev/ttyACM0".to_string()),
            frequency: Some(867_200_000),
            ..Default::default()
        };
        let radio = serial_radio_config(&cfg).expect("frequency present → radio config");
        assert_eq!(radio.tx_power, 14);
    }

    /// And an explicit `txpower = 0` still reaches the modem as 0. This
    /// drives the same chain the driver does — config to
    /// `serial_radio_config` to the wire frame and back out of the parser —
    /// so it fails if any link collapses the absent case into the zero case.
    #[test]
    fn an_explicit_zero_txpower_reaches_the_radio_as_zero() {
        let cfg = crate::config::InterfaceConfig {
            interface_type: "SerialInterface".to_string(),
            port: Some("/dev/ttyACM0".to_string()),
            frequency: Some(869_525_000),
            bandwidth: Some(125_000),
            spreading_factor: Some(7),
            coding_rate: Some(5),
            tx_power: Some(0),
            ..Default::default()
        };
        let radio = serial_radio_config(&cfg).expect("frequency present → radio config");
        assert_eq!(radio.tx_power, 0);

        let payload = leviculum_core::rnode::build_radio_config_frame(
            &leviculum_core::rnode::RadioConfigWire {
                frequency_hz: radio.frequency as u32,
                bandwidth_hz: radio.bandwidth,
                sf: radio.spreading_factor,
                cr: radio.coding_rate,
                tx_power_dbm: radio.tx_power,
                preamble_len: radio.preamble_len,
                csma_enabled: radio.csma_enabled,
                radio_silent: false,
                st_alock: 0,
                lt_alock: radio.lt_alock,
                lt_alock_present: true,
            },
        );
        let parsed =
            leviculum_core::rnode::parse_radio_config(&payload[2..]).expect("frame parses back");
        assert_eq!(parsed.tx_power_dbm, 0);
    }

    /// No `frequency` means a plain serial pipe, not a LoRa modem: no radio
    /// config is pushed at all, whatever the other keys say.
    #[test]
    fn no_frequency_means_no_radio_config() {
        let cfg = crate::config::InterfaceConfig {
            interface_type: "SerialInterface".to_string(),
            port: Some("/dev/ttyACM0".to_string()),
            preamble_symbols: Some(18),
            ..Default::default()
        };
        assert!(serial_radio_config(&cfg).is_none());
    }

    fn base_config(port: &str, radio: Option<SerialRadioConfig>) -> SerialInterfaceConfig {
        SerialInterfaceConfig {
            id: InterfaceId(0),
            name: "serial-test".to_string(),
            port: port.to_string(),
            speed: 115_200,
            data_bits: tokio_serial::DataBits::Eight,
            parity: tokio_serial::Parity::None,
            stop_bits: tokio_serial::StopBits::One,
            buffer_size: SERIAL_DEFAULT_BUFFER_SIZE,
            reconnect_notify: None,
            radio_config: radio,
            test_drop_direct_ingress: false,
        }
    }

    /// Serialises every test in this module that keeps a live
    /// [`serial_io_task`].
    ///
    /// Not fussiness: the out-of-band firmware-request channel is a
    /// process-global broadcast to every serial interface IN THIS PROCESS
    /// — the property the daemon wants, because a signal is addressed to
    /// a process and not to one port — and `cargo test` runs a crate's
    /// tests as threads of ONE process. So two live io tasks means one
    /// test's request is served by the other test's port: it writes a
    /// frame nobody asked it for, and logs a line the other test's
    /// tracing capture then reads as its own. Measured before this lock
    /// existed: `test_drop_direct_ingress_announces_arming_at_the_serial_boundary`
    /// failed 8 times in 30 runs of `cargo test -p leviculum-std --lib
    /// interfaces::serial`, some of those with the announce request's own
    /// log line sitting in its capture buffer.
    ///
    /// Every test that spawns the task therefore takes this, and every
    /// one of them ends its task before releasing it — a leaked task is a
    /// subscriber for the rest of the binary and the lock cannot help
    /// against that.
    ///
    /// Held across awaits deliberately: a `tokio::sync::Mutex` would make
    /// the plain `#[test]` sibling enter a runtime just to take it.
    static SERIAL_IO_TASK_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The lock, poisoning ignored: a panicking test has already failed,
    /// and refusing to run the others adds nothing to the report.
    fn serial_io_task_test_guard() -> std::sync::MutexGuard<'static, ()> {
        SERIAL_IO_TASK_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// The TEST-ONLY `test_drop_direct_ingress` knob at the interface level
    /// (HDLC deframe → filter, no daemon): a frame whose wire hops byte
    /// (`raw[1]`) is 0 is dropped before the transport channel, a relayed
    /// copy (hops >= 1) passes, and with the knob off (the default) the
    /// hops-0 frame passes too.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn test_drop_direct_ingress_filters_hops0_at_the_serial_boundary() {
        let _guard = serial_io_task_test_guard();
        async fn rx_through_io_task(
            drop_direct: bool,
            frames: &[Vec<u8>],
        ) -> (Vec<Vec<u8>>, Arc<InterfaceCounters>) {
            let (port, mut peer) = tokio::io::duplex(8192);
            let (incoming_tx, mut incoming_rx) = mpsc::channel::<IncomingPacket>(16);
            let (outgoing_tx, outgoing_rx) = mpsc::channel::<OutgoingPacket>(16);
            let counters = Arc::new(InterfaceCounters::new());
            let task_counters = Arc::clone(&counters);
            let task = tokio::spawn(async move {
                serial_io_task(
                    "test_serial_deaf".to_string(),
                    port,
                    incoming_tx,
                    outgoing_rx,
                    task_counters,
                    drop_direct,
                )
                .await;
            });
            for f in frames {
                let mut out = Vec::new();
                frame(f, &mut out);
                peer.write_all(&out).await.expect("write frame");
            }
            let mut got = Vec::new();
            while let Ok(Some(pkt)) =
                tokio::time::timeout(Duration::from_millis(300), incoming_rx.recv()).await
            {
                got.push(pkt.data);
            }
            drop(outgoing_tx);
            drop(peer);
            let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
            (got, counters)
        }

        // flags(1) hops(1) tail — the filter reads only raw[1].
        let direct = vec![0x00u8, 0x00, 0xAA, 0xBB];
        let relayed = vec![0x00u8, 0x01, 0xAA, 0xBB];

        let (got, counters) = rx_through_io_task(true, &[direct.clone(), relayed.clone()]).await;
        assert_eq!(got, vec![relayed.clone()], "only the relayed copy passes");
        assert_eq!(
            counters.test_direct_ingress_drops.load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            counters.rx_bytes.load(Ordering::Relaxed),
            relayed.len() as u64,
            "a dropped frame was never heard, so it must not count as RX"
        );

        let (got, counters) = rx_through_io_task(false, &[direct.clone(), relayed.clone()]).await;
        assert_eq!(got, vec![direct, relayed]);
        assert_eq!(
            counters.test_direct_ingress_drops.load(Ordering::Relaxed),
            0
        );
    }

    /// The filter announces itself (Codeberg #223): when
    /// `test_drop_direct_ingress` is on, the io task emits exactly the
    /// armed line the periculum cells assert on — from the same task that
    /// holds the flag the drop filter reads, so the line proves the
    /// production drop path is live and not merely that a config key
    /// parsed. Off (the default), the line must be absent.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn test_drop_direct_ingress_announces_arming_at_the_serial_boundary() {
        let _guard = serial_io_task_test_guard();
        /// Capture tracing output for the duration of the returned guard.
        /// Thread-local default subscriber + current-thread tokio runtime,
        /// same pattern as the rnode airtime-lock tests.
        #[derive(Clone)]
        struct LogSink(Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for LogSink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogSink {
            type Writer = LogSink;
            fn make_writer(&'a self) -> LogSink {
                self.clone()
            }
        }

        async fn logs_from_io_task(drop_direct: bool) -> String {
            let buf = Arc::new(std::sync::Mutex::new(Vec::new()));
            let subscriber = tracing_subscriber::fmt()
                .with_writer(LogSink(Arc::clone(&buf)))
                .with_max_level(tracing::Level::DEBUG)
                .with_ansi(false)
                .finish();
            let _guard = tracing::subscriber::set_default(subscriber);

            let (port, peer) = tokio::io::duplex(8192);
            let (incoming_tx, _incoming_rx) = mpsc::channel::<IncomingPacket>(16);
            let (outgoing_tx, outgoing_rx) = mpsc::channel::<OutgoingPacket>(16);
            let counters = Arc::new(InterfaceCounters::new());
            let task = tokio::spawn(async move {
                serial_io_task(
                    "test_serial_armed".to_string(),
                    port,
                    incoming_tx,
                    outgoing_rx,
                    counters,
                    drop_direct,
                )
                .await;
            });
            // Let the io task start and emit (or not emit) the armed line,
            // then shut it down by closing its peer and channels.
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(outgoing_tx);
            drop(peer);
            let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
            let captured = buf.lock().unwrap();
            String::from_utf8_lossy(&captured).into_owned()
        }

        let logs = logs_from_io_task(true).await;
        assert!(
            logs.contains("DIRECT_INGRESS_FILTER armed iface=test_serial_armed"),
            "armed line missing with the knob on; logs:\n{logs}"
        );

        let logs = logs_from_io_task(false).await;
        assert!(
            !logs.contains("DIRECT_INGRESS_FILTER"),
            "armed line must be absent with the knob off; logs:\n{logs}"
        );
    }

    /// With a radio config present, the spawned handle carries an
    /// AirtimeCredit bucket.
    #[tokio::test(flavor = "current_thread")]
    async fn spawn_with_radio_config_populates_credit() {
        let radio = SerialRadioConfig {
            frequency: 869_525_000,
            bandwidth: 125_000,
            spreading_factor: 10,
            coding_rate: 8,
            tx_power: 17,
            preamble_len: 24,
            csma_enabled: true,
            lt_alock: 1000,
        };
        let handle = spawn_serial_interface(base_config("/dev/null-test-no-radio-a", Some(radio)));
        assert!(handle.credit.is_some());
    }

    /// Without a radio config, the spawned handle leaves credit = None.
    /// This is the "plain serial" (non-LoRa) path used by leviculum-std's
    /// rnsd_interop tests.
    #[tokio::test(flavor = "current_thread")]
    async fn spawn_without_radio_config_leaves_credit_none() {
        let handle = spawn_serial_interface(base_config("/dev/null-test-no-radio-b", None));
        assert!(handle.credit.is_none());
    }

    /// B5 wiring sanity: the Arc<Mutex<AirtimeCredit>> attached to the
    /// spawned handle is the SAME instance that `send_radio_config`'s
    /// `update_radio_params` call would mutate. Verified by spawning at
    /// SF=7, manually applying the SF=10/CR=8 reconfig through the
    /// shared Arc (mirroring what the ACK path does), and observing a
    /// concrete behavior difference on the handle-side bucket.
    ///
    /// Test construction: at SF=7 after a MTU charge, a small follow-up
    /// packet is rejected (fresh cost X_50_sf7 pushes credit below the
    /// tight SF=7 threshold). After reconfig to SF=10/CR=8 the threshold
    /// grows in magnitude (MTU airtime at SF10 is ~10× SF7), so the same
    /// carried-over deficit now leaves room for the small follow-up.
    /// The change in accept/reject is observable only if update_radio_params
    /// actually ran, so this asserts the wiring.
    ///
    /// End-to-end send_radio_config coverage requires a T114 and lives
    /// in Phase G hardware verification; this test locks down the
    /// Arc-shared-state invariant only.
    #[tokio::test(flavor = "current_thread")]
    async fn reconfig_propagates_to_handle_side_bucket() {
        let radio = SerialRadioConfig {
            frequency: 869_525_000,
            bandwidth: 125_000,
            spreading_factor: 7,
            coding_rate: 5,
            tx_power: 17,
            preamble_len: 24,
            csma_enabled: true,
            lt_alock: 1000,
        };
        let handle = spawn_serial_interface(base_config("/dev/null-test-reconfig", Some(radio)));
        let credit_arc = handle
            .credit
            .as_ref()
            .expect("radio_config present → bucket attached")
            .clone();
        // Exhaust at SF=7: a full-MTU charge puts credit at the SF7 threshold.
        {
            let mut c = credit_arc.lock().unwrap();
            c.try_charge(500, 0).expect("initial charge at SF7 fits");
            // Small follow-up at SF7 MUST fail (any positive-cost packet from
            // exactly-threshold pushes below threshold).
            assert!(
                c.try_charge(50, 0).is_err(),
                "small follow-up at SF7 should be rejected"
            );
        }
        // Simulate the ACK path's update to SF=10/CR=8 (as a scenario
        // might push via send_radio_config's post-ACK hook).
        credit_arc
            .lock()
            .unwrap()
            .update_radio_params(125_000, 10, 8, 18);
        // Under the new, more-permissive SF10 threshold, the carried-over
        // SF7 deficit leaves room for the same small packet.
        {
            let mut c = credit_arc.lock().unwrap();
            assert!(
                c.try_charge(50, 0).is_ok(),
                "small follow-up after SF7→SF10 reconfig should succeed"
            );
        }
    }

    /// The commanded reset, end to end through the task that owns the
    /// port: a request goes in, and what comes out of the port is the
    /// SAME bytes an unattached reset writes.
    ///
    /// This is the property the harness depends on. `periculum`'s direct
    /// reset opens the port itself and writes `frame(RADIO_RESET_FRAME)`;
    /// mid-scenario it cannot, because this daemon holds the port
    /// exclusively, so it asks the daemon instead. If the two ever put
    /// different bytes on the wire, "the board came back from a defined
    /// state" stops being true of one of them and the two measurements
    /// stop being comparable — which is exactly the kind of drift a
    /// re-formation cell would report as a mesh finding.
    // The guard is a std mutex held across awaits on purpose (see its
    // doc): a tokio mutex would need the plain `#[test]` sibling below to
    // enter a runtime just to take it.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn a_reset_request_puts_the_unattached_reset_bytes_on_the_port() {
        let _guard = serial_io_task_test_guard();
        let (near, mut far) = tokio::io::duplex(1024);
        let (incoming_tx, _incoming_rx) = mpsc::channel(8);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(8);
        let counters = Arc::new(InterfaceCounters::new());
        let task = tokio::spawn(serial_io_task(
            "test0".to_string(),
            near,
            incoming_tx,
            outgoing_rx,
            counters,
            false,
        ));

        // The subscribe happens inside the task; give it a turn to run
        // before the request, or the broadcast has no receiver yet.
        let reached = loop {
            tokio::task::yield_now().await;
            let reached = request_firmware_reset();
            if reached > 0 {
                break reached;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        // At least this port. NOT exactly one: the channel is
        // process-global (see `FIRMWARE_REQUEST_TEST_LOCK`) and other
        // tests in this file keep their own `serial_io_task` alive in
        // parallel, so the count is a property of the whole test process
        // and not of this test. What this test is actually about is the
        // BYTES on this port, asserted below; the count only has to show
        // the request was delivered somewhere.
        assert!(reached >= 1, "the request reached no interface at all");

        let mut expected = Vec::new();
        frame(&leviculum_core::rnode::RADIO_RESET_FRAME, &mut expected);
        let mut got = vec![0u8; expected.len()];
        tokio::time::timeout(Duration::from_secs(5), far.read_exact(&mut got))
            .await
            .expect("the reset frame is written promptly")
            .expect("the port receives it");
        assert_eq!(
            got, expected,
            "the daemon-routed reset must be byte-identical to the direct one"
        );

        drop(outgoing_tx);
        drop(far);
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    }

    /// A daemon holding no firmware board reports that it reached none,
    /// rather than reporting a reset it did not perform. The caller needs
    /// the difference: signalling the wrong node is a scenario error, and
    /// a silent 0 would read as a board that rebooted.
    #[test]
    fn a_request_with_no_attached_interface_reaches_nothing() {
        let _guard = serial_io_task_test_guard();
        assert_eq!(request_firmware_reset(), 0);
    }

    /// The commanded ANNOUNCE puts `TYPE_ANNOUNCE` on the port, HDLC-framed
    /// exactly as an outgoing packet is — the same claim the reset test
    /// above makes about its frame, and for the same reason: a board only
    /// acts on the bytes, so what is asserted has to be the bytes.
    ///
    /// The two requests share one broadcast channel, so this also pins
    /// that they do not share a FRAME: an announce that wrote
    /// `RADIO_RESET_FRAME` would reboot every board a harness asked to
    /// announce, and the cell after it would measure a mesh the harness
    /// took down.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn the_announce_request_writes_the_announce_frame() {
        let _guard = serial_io_task_test_guard();
        let (near, mut far) = tokio::io::duplex(256);
        let (incoming_tx, _incoming_rx) = mpsc::channel(8);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(8);
        let counters = Arc::new(InterfaceCounters::new());
        let task = tokio::spawn(serial_io_task(
            "test-announce".to_string(),
            near,
            incoming_tx,
            outgoing_rx,
            counters,
            false,
        ));

        // The subscribe happens inside the task; give it a turn to run
        // before the request, or the broadcast has no receiver yet.
        let reached = loop {
            tokio::task::yield_now().await;
            let reached = request_firmware_announce();
            if reached > 0 {
                break reached;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        // At least this port. NOT exactly one: the channel is
        // process-global (see `FIRMWARE_REQUEST_TEST_LOCK`) and other
        // tests in this file keep their own `serial_io_task` alive in
        // parallel, so the count is a property of the whole test process
        // and not of this test. What this test is actually about is the
        // BYTES on this port, asserted below; the count only has to show
        // the request was delivered somewhere.
        assert!(reached >= 1, "the request reached no interface at all");

        let mut expected = Vec::new();
        frame(&leviculum_core::envelope::encode_announce(), &mut expected);
        let mut got = vec![0u8; expected.len()];
        tokio::time::timeout(Duration::from_secs(5), far.read_exact(&mut got))
            .await
            .expect("the announce frame is written promptly")
            .expect("the port receives it");
        assert_eq!(
            got, expected,
            "the daemon-routed announce must be the TYPE_ANNOUNCE envelope"
        );

        let mut reset_frame = Vec::new();
        frame(&leviculum_core::rnode::RADIO_RESET_FRAME, &mut reset_frame);
        assert_ne!(
            got, reset_frame,
            "an announce request must never write the reset frame"
        );

        drop(outgoing_tx);
        drop(far);
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    }

    /// Codeberg #389 mvr (tx counter siblings): once the peer can observe
    /// bytes of a frame, the interface's tx counter already covers that
    /// frame — same ordering the TCP interface pins. A 4 KiB duplex parks
    /// `write_all` mid-frame, the peer reads the frame's head, and the
    /// counter is inspected inside that window.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn tx_counter_covers_bytes_the_peer_can_already_observe() {
        let _guard = serial_io_task_test_guard();
        let (near, mut far) = tokio::io::duplex(4096);
        let (incoming_tx, _incoming_rx) = mpsc::channel(8);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(8);
        let counters = Arc::new(InterfaceCounters::new());
        let task_counters = Arc::clone(&counters);
        // Held rather than discarded: this task parks in `write_all` for
        // good (the frame is larger than the duplex), so without the
        // abort below it outlives the test and stays subscribed to the
        // firmware-request channel for the rest of the binary — the leak
        // that made a sibling test read this one's frames.
        let task = tokio::spawn(serial_io_task(
            "mvr_389".to_string(),
            near,
            incoming_tx,
            outgoing_rx,
            task_counters,
            false,
        ));

        // One frame far larger than the duplex buffer: the write cannot
        // complete, so the task stays parked in write_all while the frame's
        // head is already at the peer.
        outgoing_tx
            .send(OutgoingPacket {
                data: vec![0x42u8; 64 * 1024],
                high_priority: false,
                peer: None,
            })
            .await
            .expect("send into task");

        let mut head = [0u8; 1024];
        let n = tokio::time::timeout(Duration::from_secs(5), far.read(&mut head))
            .await
            .expect("the frame head is written promptly")
            .expect("peer read");
        assert!(n > 0, "peer must observe bytes of the frame");

        let tx = counters.tx_bytes.load(Ordering::Relaxed);
        assert!(
            tx > 0,
            "peer observed {n} bytes but the tx counter reads {tx}"
        );

        // The task is parked mid-`write_all` by construction and will
        // never return on its own; abort it so it stops being a
        // subscriber the moment this test is done with it.
        task.abort();
        let _ = task.await;
    }
}
