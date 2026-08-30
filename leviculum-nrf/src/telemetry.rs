//! The LNode telemetry reporter (Codeberg #236).
//!
//! Three things meet here, and none of them is a decision:
//!
//! * **The policy** — `leviculum_telemetry_policy` owns every "report
//!   now?" question and every target-lifecycle transition. It is a
//!   separate, host-tested crate for the same reason the GNSS presence
//!   machine is: a state machine that needs a radio to be exercised is a
//!   state machine that is never exercised.
//! * **The codec** — `leviculum_lxmf::telemetry` owns the Telemeter bytes
//!   and `build_report` owns the message shape (empty content and title,
//!   opportunistic delivery). Both are proven against the #237 fixtures on
//!   the host.
//! * **The storage** — [`leviculum_core::telemetry_target_store`] owns the
//!   flash record.
//!
//! What is left here is wiring: read the sensors this board has, ask the
//! policy, build the message, hand the packet to the node, persist what
//! the host set, and say out loud what state the target is in. Nothing
//! in this module is RAK-specific; the per-board part is which peripherals
//! exist to read, and that lives in the binary.
//!
//! # Airtime
//!
//! There is no airtime figure in this module and there must never be one.
//! Cadence is policy; when the radio may transmit is the interface's
//! business (`docs/src/concepts/interface-isolation.md`,
//! `docs/src/concepts/regulatory-airtime.md`).

extern crate alloc;

use alloc::vec::Vec;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;

use leviculum_core::envelope::{TelemetryTargetWire, TELEMETRY_PROFILE_OFF};
use leviculum_core::identity::Identity;
use leviculum_core::node::NodeCore;
use leviculum_core::telemetry_target_store::{
    decode_telemetry_target, encode_telemetry_target, ENCODED_SIZE_ALIGNED,
};
use leviculum_core::traits::{Clock, Storage};
use leviculum_core::transport::{Action, DispatchResult};
use leviculum_core::DestinationHash;
use leviculum_lxmf::msgpack::Number;
use leviculum_lxmf::telemetry::{build_report, Battery, Location, Telemetry};
use leviculum_telemetry_policy::{
    command_from_wire, EmissionRoute, Fix, Profile, ReportReason, SendPolicy, TargetCommand,
    TargetState,
};

/// Re-exported so the binaries name the outcome of
/// [`Reporter::apply_target`] without also depending on the policy crate
/// directly: the wiring layer is the seam, and the seam owns its
/// vocabulary.
pub use leviculum_telemetry_policy::TargetOutcome;
use rand_core::CryptoRngCore;

/// The profile ids are allocated twice — once on the wire
/// (`leviculum_core::envelope`) and once in the policy crate, which
/// cannot depend on core — so this is the one place that sees both.
/// A drift between them would turn a "clear" frame into a "set" frame
/// with a zero destination, which is exactly the silent failure the
/// explicit clear encoding exists to avoid.
const _: () = {
    assert!(TELEMETRY_PROFILE_OFF == leviculum_telemetry_policy::PROFILE_ID_OFF);
    assert!(
        leviculum_core::envelope::TELEMETRY_PROFILE_TRACKER
            == leviculum_telemetry_policy::PROFILE_ID_TRACKER
    );
    assert!(
        leviculum_core::envelope::TELEMETRY_PROFILE_STATION
            == leviculum_telemetry_policy::PROFILE_ID_STATION
    );
};

/// Whether this binary wires a [`Reporter`] into its main loop.
///
/// The serial control task gates the telemetry-target ack on this
/// declaration ([`leviculum_core::envelope::telemetry_target_answer`]):
/// a binary that never constructs a reporter must answer a named refusal,
/// not an ack for a target nothing will honor. Set once via
/// [`declare_reporter`] before `usb::init`, so no frame can be answered
/// before the declaration exists.
static REPORTER_WIRED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Declare that this binary constructs a [`Reporter`] and drains
/// [`inbound_target_receiver`]. Call before `usb::init`.
pub fn declare_reporter() {
    REPORTER_WIRED.store(true, core::sync::atomic::Ordering::Relaxed);
}

/// Whether [`declare_reporter`] was called — the capability the serial
/// task's telemetry-target answer is gated on.
pub fn reporter_wired() -> bool {
    REPORTER_WIRED.load(core::sync::atomic::Ordering::Relaxed)
}

/// Targets arriving from the host over the #238 control envelope. Depth 1
/// and `try_send`: the serial task must never block, and a superseded
/// target is worthless — the newest one is the one that must take effect.
static INBOUND_TARGET: Channel<CriticalSectionRawMutex, TelemetryTargetWire, 1> = Channel::new();

/// Hand a target from the serial task to the main loop. Returns whether
/// it was taken; `false` is a main loop that has not drained the previous
/// one yet, which the host's retry covers.
pub fn deliver_target(target: TelemetryTargetWire) -> bool {
    if INBOUND_TARGET.try_send(target).is_err() {
        let _ = INBOUND_TARGET.try_receive();
        INBOUND_TARGET.try_send(target).is_ok()
    } else {
        true
    }
}

/// The main loop's end of [`deliver_target`].
pub fn inbound_target_receiver(
) -> embassy_sync::channel::Receiver<'static, CriticalSectionRawMutex, TelemetryTargetWire, 1> {
    INBOUND_TARGET.receiver()
}

// ---------------------------------------------------------------------------
// Persistence — same two-halves shape as `radio_store`
// ---------------------------------------------------------------------------

/// Pending save requests. Depth 1 for the same reason as the radio store:
/// the newest target is the one that must end up on the page.
static PENDING_SAVE: Channel<CriticalSectionRawMutex, TelemetryTargetWire, 1> = Channel::new();

/// Read the persisted telemetry target, or `None` if the page is blank,
/// corrupt, or written by a different format version — all of which mean
/// "no target", which is telemetry off, which is the default.
///
/// Internal flash is memory-mapped on the nRF52840, so this is an ordinary
/// read and is safe at any point in boot, including before
/// `Softdevice::enable`.
pub fn load(page: u32) -> Option<TelemetryTargetWire> {
    decode_telemetry_target(&read_page(page))
}

fn read_page(page: u32) -> [u8; ENCODED_SIZE_ALIGNED] {
    let mut buf = [0u8; ENCODED_SIZE_ALIGNED];
    // SAFETY: `page` is a flash page address supplied by the board config,
    // outside the linker's FLASH region but inside the 1 MiB flash map
    // (memory.x). Flash is readable as normal memory on this part.
    let stored = unsafe { core::slice::from_raw_parts(page as *const u8, ENCODED_SIZE_ALIGNED) };
    buf.copy_from_slice(stored);
    buf
}

/// Ask the store task to persist `target`. Never blocks and never writes
/// flash on the caller's stack.
pub fn request_save(target: &TelemetryTargetWire) {
    if PENDING_SAVE.try_send(*target).is_err() {
        let _ = PENDING_SAVE.try_receive();
        let _ = PENDING_SAVE.try_send(*target);
    }
}

/// 4-byte-aligned page buffer. `sd_flash_write` writes whole 32-bit words
/// and rejects an unaligned source pointer.
#[repr(align(4))]
struct Aligned([u8; ENCODED_SIZE_ALIGNED]);

/// How often a failed flash operation is retried; the SoftDevice refuses
/// flash access while the radio is busy.
const SAVE_RETRIES: u8 = 3;
const SAVE_RETRY_MS: u64 = 250;

#[cfg(feature = "softdevice")]
#[embassy_executor::task]
pub async fn store_task(flash: &'static crate::flash::SharedFlash, page: u32) {
    use embedded_storage_async::nor_flash::NorFlash;

    loop {
        let target = PENDING_SAVE.receive().await;
        let encoded = Aligned(encode_telemetry_target(&target));

        // Read-compare-write: an unchanged page is never erased. A host
        // tool that re-sends the same target on every connect must not
        // burn a flash cycle for it.
        if read_page(page) == encoded.0 {
            crate::log::log_fmt("[TELEMETRY] ", format_args!("persist skipped, unchanged"));
            continue;
        }

        let mut written = false;
        for attempt in 1..=SAVE_RETRIES {
            let result = async {
                let mut flash = flash.lock().await;
                flash.erase(page, page + 4096).await?;
                flash.write(page, &encoded.0).await
            }
            .await;
            match result {
                Ok(()) => {
                    written = true;
                    break;
                }
                Err(_) => {
                    crate::log::log_fmt(
                        "[TELEMETRY] ",
                        format_args!("persist write failed, attempt {}/{}", attempt, SAVE_RETRIES),
                    );
                    embassy_time::Timer::after(embassy_time::Duration::from_millis(SAVE_RETRY_MS))
                        .await;
                }
            }
        }

        if written {
            crate::log::log_fmt(
                "[TELEMETRY] ",
                format_args!("persist saved profile={}", target.profile),
            );
        } else {
            crate::log::log_fmt(
                "[TELEMETRY] ",
                format_args!("persist gave up after retries"),
            );
        }
    }
}

/// Spawn the store task, borrowing the shared SoftDevice flash handle
/// ([`crate::flash::shared_flash`]). Call after `Softdevice::enable`.
#[cfg(feature = "softdevice")]
pub fn spawn_store_task(
    spawner: &embassy_executor::Spawner,
    flash: &'static crate::flash::SharedFlash,
    page: u32,
) {
    spawner.must_spawn(store_task(flash, page));
}

// ---------------------------------------------------------------------------
// Sensor readings
// ---------------------------------------------------------------------------

/// Horizontal position error in metres per unit of HDOP.
///
/// The wire's accuracy slot is metres (`Location.pack`, Sideband
/// `2000d81`; Columba fills it from Android's `Location.accuracy`), and
/// standard NMEA carries no accuracy field at all — only HDOP, which is a
/// geometry factor and not a distance. Turning one into the other needs a
/// user-equivalent range error, and 5 m is the conventional open-sky
/// figure for a single-frequency consumer receiver.
///
/// This is a **model, stated as one**, in the sense
/// `docs/src/concepts/regulatory-airtime.md` fixes for any such figure:
/// it is not a measurement of this antenna in this housing, and a
/// deployment that has measured its own may say so. What it is not is a
/// placeholder — it is derived from a number the receiver actually
/// reported, and when the receiver reports no HDOP the position is not
/// sent at all.
pub const HORIZONTAL_UERE_M: f32 = 5.0;

/// What this board could read at the moment a report was due.
///
/// Every field is optional and an absent one contributes no sensor key —
/// the concept's absence encoding, applied at the point the readings are
/// collected rather than deep inside the codec.
#[derive(Debug, Clone, Copy, Default)]
pub struct Readings {
    /// Unix seconds from the node's calendar. `None` is a node whose
    /// calendar was never seeded; it reports nothing, because a telemetry
    /// row with no time is a row a collector cannot order.
    pub unix_secs: Option<u64>,
    /// Decimal degrees, from a receiver in presence state `Fix`.
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// Metres above mean sea level (GGA).
    pub altitude_m: Option<f32>,
    /// Ground speed in metres per second (RMC).
    pub speed_mps: Option<f32>,
    /// Course over ground in degrees (RMC).
    pub bearing_deg: Option<f32>,
    /// Horizontal dilution of precision (GGA).
    pub hdop: Option<f32>,
    /// Battery charge, percent.
    pub battery_percent: Option<u8>,
}

impl Readings {
    /// The position as the policy wants it: scaled integers plus the
    /// accuracy number the threshold is applied to.
    pub fn fix(&self) -> Option<Fix> {
        let latitude = self.latitude?;
        let longitude = self.longitude?;
        Some(Fix {
            latitude_e6: (latitude * 1e6) as i32,
            longitude_e6: (longitude * 1e6) as i32,
            hdop_e2: self.hdop.map(|h| (h * 100.0).clamp(0.0, 65535.0) as u16),
        })
    }

    /// The location sensor, or `None` when there is no position to send.
    ///
    /// `include_position` is the policy's answer, not this function's: a
    /// fix that failed the accuracy gate is not a position, and neither
    /// is a fix seen while presence is not `Fix`.
    fn location(&self, include_position: bool) -> Option<Location> {
        if !include_position {
            return None;
        }
        let fix = self.fix()?;
        let accuracy_m = self.hdop? * HORIZONTAL_UERE_M;
        Some(Location::saturating(
            fix.latitude_e6 as i64,
            fix.longitude_e6 as i64,
            (self.altitude_m.unwrap_or(0.0) * 100.0) as i64,
            (self.speed_mps.unwrap_or(0.0) * 100.0) as i64,
            (self.bearing_deg.unwrap_or(0.0) * 100.0) as i64,
            (accuracy_m * 100.0) as i64,
            self.unix_secs.unwrap_or(0) as i64,
        ))
    }

    /// Assemble the Telemeter for one report.
    pub fn telemetry(&self, include_position: bool) -> Telemetry {
        Telemetry {
            time: self.unix_secs.map(|s| s as i64),
            location: self.location(include_position),
            battery: self.battery_percent.map(|percent| Battery {
                charge_percent: Number::Int(percent as i64),
                // The baseboard reads a voltage divider, which cannot tell
                // charging from discharging. Nil is the reference's own
                // "platform did not say", not a claim that it is idle.
                charging: None,
                temperature: None,
            }),
            ..Telemetry::default()
        }
    }
}

// ---------------------------------------------------------------------------
// The reporter
// ---------------------------------------------------------------------------

/// How often the node re-asks for a path while it is waiting for a
/// target's key. A path request is one small packet and the answer
/// carries the identity, so this is the key-resolution loop; 60 s keeps
/// it well under any announce cadence it might be racing.
const KEY_REQUEST_INTERVAL_MS: u64 = 60_000;

/// The reporting half of a node: policy, configured target, and the
/// delivery destination reports are signed by.
pub struct Reporter {
    policy: SendPolicy,
    target: Option<TelemetryTargetWire>,
    delivery_hash: DestinationHash,
    /// Monotonic time of the last path request issued while awaiting a
    /// key; `None` before the first one.
    last_key_request_ms: Option<u64>,
    /// Why the last tick did not send, so the reason is logged when it
    /// changes and not twelve times a minute for as long as it lasts. A
    /// blocked reporter must be legible in a log tail, which a flood is
    /// not.
    last_withheld: Option<&'static str>,
    /// What the pending report says once its dispatch is settled: the
    /// reason it was sent, the position it carried and the timebase it
    /// stamped. Held only between [`tick`](Self::tick) and
    /// [`note_dispatch`](Self::note_dispatch) — the report line is written
    /// there, not here, because until then it is not known whether there
    /// is a report to write a line about.
    pending_line: Option<PendingLine>,
}

/// The report line of a report that has been handed to transport.
#[derive(Clone, Copy)]
struct PendingLine {
    reason: ReportReason,
    include_position: bool,
    unix_secs: u64,
    /// The interfaces this report's own frames were handed to, which is
    /// what decides whether it went out (#348). The announce that shares
    /// the dispatch is not in here: it is a broadcast toward everyone and
    /// its fate on a third interface says nothing about this report.
    route: EmissionRoute,
}

impl Reporter {
    /// A reporter with no target. `delivery_hash` is the node's own
    /// registered `lxmf.delivery` destination — the one a receiver
    /// verifies our signature against, which is why announcing it is not
    /// optional.
    pub fn new(delivery_hash: DestinationHash) -> Self {
        Self {
            policy: SendPolicy::new(),
            target: None,
            delivery_hash,
            last_key_request_ms: None,
            last_withheld: None,
            pending_line: None,
        }
    }

    /// Log `reason` once, until something else happens.
    fn withhold(&mut self, reason: &'static str) {
        if self.last_withheld != Some(reason) {
            self.last_withheld = Some(reason);
            crate::log::log_fmt_critical(
                "[INFO!] ",
                format_args!(
                    "[TELEMETRY] report withheld target={:08x} reason={}",
                    self.target_short(),
                    reason
                ),
            );
        }
    }

    pub fn state(&self) -> TargetState {
        self.policy.state()
    }

    pub fn profile(&self) -> Profile {
        self.policy.profile()
    }

    /// The configured target's destination hash, if any.
    pub fn target_hash(&self) -> Option<DestinationHash> {
        self.target.map(|t| DestinationHash::new(t.dest_hash))
    }

    /// Whether telemetry is switched off, which is what the caller uses
    /// to decide it has nothing to wake up for.
    pub fn is_off(&self) -> bool {
        self.policy.state() == TargetState::Off
    }

    /// The first four bytes of the target hash, for the `[TELEMETRY]`
    /// events. Zero when there is no target.
    pub fn target_short(&self) -> u32 {
        self.target
            .map(|t| {
                u32::from_be_bytes([
                    t.dest_hash[0],
                    t.dest_hash[1],
                    t.dest_hash[2],
                    t.dest_hash[3],
                ])
            })
            .unwrap_or(0)
    }

    /// Apply a target frame from the host or from flash.
    ///
    /// `profile == TELEMETRY_PROFILE_OFF` clears; anything else sets. An
    /// unknown profile id falls back to the default profile rather than
    /// refusing the destination — a newer host's cadence preference is
    /// not worth losing the target over, and the state the node reports
    /// says which profile it actually runs.
    ///
    /// If the frame carried a public key, it is remembered here, which is
    /// what lets a key-bearing target go straight to `ready`.
    pub fn apply_target<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        wire: TelemetryTargetWire,
    ) -> TargetOutcome
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        self.last_key_request_ms = None;
        self.last_withheld = None;
        let command = command_from_wire(wire.profile);
        let key_known = match command {
            TargetCommand::Clear => {
                self.target = None;
                false
            }
            TargetCommand::Set(_) => {
                let hash = DestinationHash::new(wire.dest_hash);
                // A frame that carried a key skips the over-the-air
                // resolution entirely, which is the whole benefit of
                // carrying one.
                if let Some(key) = wire.public_key {
                    if let Ok(identity) = Identity::from_public_key_bytes(&key) {
                        node.remember_identity(hash, identity);
                    }
                }
                self.target = Some(wire);
                node.storage().get_identity(hash.as_bytes()).is_some()
            }
        };
        self.policy.apply(command, key_known)
    }

    /// One evaluation step. Returns the packets the caller must dispatch;
    /// an empty vector is the common case.
    ///
    /// The caller decides the tick rate, and that rate is the retry rate
    /// for a report the radio could not take — the policy re-arms until
    /// [`SendPolicy::note_sent`] confirms one went out.
    pub fn tick<R, C, S>(
        &mut self,
        node: &mut NodeCore<R, C, S>,
        now_ms: u64,
        presence_has_fix: bool,
        readings: &Readings,
    ) -> Vec<Action>
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let mut actions = Vec::new();
        let Some(target) = self.target else {
            return actions;
        };
        let hash = DestinationHash::new(target.dest_hash);

        if self.policy.state() == TargetState::AwaitingKey {
            if node.storage().get_identity(hash.as_bytes()).is_some() {
                if self.policy.note_key_available() {
                    crate::log::log_fmt_critical(
                        "[INFO!] ",
                        format_args!(
                            "[TELEMETRY] target={:08x} state={}",
                            self.target_short(),
                            self.policy.state().as_str()
                        ),
                    );
                }
            } else {
                // Resolve the key over the air. A path request is
                // answered with the destination's announce, which carries
                // the identity — that is the whole mechanism behind
                // hash-only configuration.
                let due = match self.last_key_request_ms {
                    None => true,
                    Some(last) => now_ms.saturating_sub(last) >= KEY_REQUEST_INTERVAL_MS,
                };
                if due {
                    self.last_key_request_ms = Some(now_ms);
                    actions.extend(node.request_path(&hash).actions);
                }
                return actions;
            }
        }

        // Only a receiver in presence state Fix may contribute a position.
        let fix = if presence_has_fix {
            readings.fix()
        } else {
            None
        };
        let Some(reason) = self.policy.poll(now_ms, fix) else {
            return actions;
        };

        let include_position = fix.map(|f| self.policy.position_is_reportable(f)) == Some(true);
        let telemetry = readings.telemetry(include_position);
        let Some(unix_secs) = readings.unix_secs else {
            // The emission timebase is still below the plausibility floor,
            // which means it is uptime seconds and not a calendar estimate
            // at all — the anchor model's "never ahead" rule has nothing
            // to be applied to. Two concrete harms, not one cosmetic one:
            // the row lands in 1970 at the receiver, and uptime seconds
            // repeat across reboots, so two reports from two boots can
            // collide on Sideband's `(source, ts)` dedup key.
            self.withhold("no-clock");
            return actions;
        };

        // Announce our delivery destination first: a receiver verifies the
        // LXMF signature against our public key, which it can only have
        // from an announce. Sending the report to a peer that has never
        // heard us is sending an unverifiable reading.
        if let Ok(out) = node.announce_destination(
            &self.delivery_hash,
            Some(&announce_app_data(node.identity())),
        ) {
            actions.extend(out.actions);
        }

        let message = match build_report(
            target.dest_hash,
            self.delivery_hash.into_bytes(),
            node.identity(),
            unix_secs as f64,
            &telemetry,
        ) {
            Ok(message) => message,
            Err(_) => {
                self.withhold("no-readings");
                return actions;
            }
        };
        let Ok(on_air) = message.on_air() else {
            return actions;
        };

        if !node.has_path(&hash) {
            actions.extend(node.request_path(&hash).actions);
            self.withhold("no-path");
            return actions;
        }

        match node.send_single_packet(&hash, &on_air) {
            Ok((_, out)) => {
                // Note the route before the actions are merged with the
                // announce's: after the merge there is no telling which
                // frame was whose, and that distinction is the whole of
                // the #348 fix.
                let mut route = EmissionRoute::new();
                for action in &out.actions {
                    if let Action::SendPacket { iface, .. } = action {
                        route.add(iface.0);
                    }
                }
                actions.extend(out.actions);
                // Handed to transport, not yet on the air. The cadence is
                // consumed in `note_dispatch`, once the dispatch has said
                // whether the frame reached an interface at all (#344) —
                // a full `LORA_OUTGOING` makes this arm succeed and the
                // dispatch that follows drop the packet, and the report
                // used to count as sent anyway.
                self.policy
                    .note_emitted(now_ms, if include_position { fix } else { None });
                self.pending_line = Some(PendingLine {
                    reason,
                    include_position,
                    unix_secs,
                    route,
                });
            }
            Err(_) => {
                // The core could not build or route it at all. This path
                // already leaves the cadence unconsumed — `note_emitted`
                // is never reached — so it needs no settlement, and it
                // keeps the reason string `note_dispatch` reuses.
                self.withhold("send-failed");
            }
        }
        actions
    }

    /// Settle the report `tick` handed over against what the dispatch did
    /// with it (#344).
    ///
    /// The caller passes the `DispatchResult` of the dispatch that carried
    /// this tick's actions. A report that did not go out leaves the
    /// cadence unconsumed, so the next ordinary tick emits it again;
    /// nothing is re-sent here and nothing is queued.
    ///
    /// **The report went out when every interface its own frames were
    /// addressed to accepted them** ([`EmissionRoute`]) — not when the
    /// dispatch was *clean*. `DispatchResult::is_clean` asks whether
    /// anything was lost anywhere, which is the right question for the
    /// `[DISPATCH_LOSS]` line `settle` writes and the wrong one here: on a
    /// board advertising BLE with no phone attached, the announce that
    /// shares this dispatch is refused by the BLE queue while LoRa puts the
    /// report on the air. Reading that refusal as "not emitted" left
    /// `last_report_ms` for ever unset, so a report was always due and only
    /// the attempt floor stood between two of them — one per minute against
    /// a fifteen-minute profile (#348).
    ///
    /// The announce's own fate is therefore no longer this report's: it is
    /// a broadcast toward everyone, and an interface with nobody behind it
    /// refusing a copy is not this report's failure. A loss on the
    /// interface the report itself used still is one, announce or report,
    /// and that keeps the older argument intact where it applies — a
    /// receiver on that interface that missed the announce cannot verify
    /// the report anyway.
    ///
    /// Silent when there is nothing pending — the common case, since the
    /// tick that sends is one in hundreds.
    pub fn note_dispatch(&mut self, result: &DispatchResult) {
        let Some(line) = self.pending_line.take() else {
            // No report this tick. `note_dispatch` still runs so a
            // caller never has to know whether one was emitted.
            let _ = self.policy.note_dispatch(false);
            return;
        };
        // `retries` repeats what `errors` already recorded for a
        // `BufferFull`; chaining all three costs one extra comparison and
        // keeps this total if that ever stops being true.
        let losses = result
            .errors
            .iter()
            .map(|(iface, _)| iface.0)
            .chain(result.drops.iter().map(|(iface, _)| iface.0))
            .chain(result.retries.iter().map(|retry| retry.iface_idx));
        if !self.policy.note_dispatch(line.route.went_out(losses)) {
            self.withhold("send-failed");
            return;
        }
        self.last_withheld = None;
        crate::log::log_fmt_critical(
            "[INFO!] ",
            format_args!(
                "[TELEMETRY] report target={:08x} reason={} position={} unix={} src={}",
                self.target_short(),
                line.reason.as_str(),
                line.include_position as u8,
                line.unix_secs,
                crate::time_source_str()
            ),
        );
    }

    /// The banner line, emitted beside `[TIME_SOURCE]` so a log tail
    /// always carries the current answer.
    pub fn log_banner(&self) {
        crate::log::log_fmt_critical(
            "[INFO!] ",
            format_args!(
                "[TELEMETRY] target={:08x} state={} profile={}",
                self.target_short(),
                self.state().as_str(),
                self.profile().as_str()
            ),
        );
    }
}

/// The `lxmf.delivery` announce payload: a display name, so a receiver
/// shows a name instead of a hex string.
///
/// The default is derived from the identity — `LNode-<8 hex>` — because a
/// node that has never been named still has to be distinguishable from the
/// next one on the bench. Making it configurable is #235 (remote
/// management) and #238 (the control envelope), which own the config
/// surface; this is the value they will override.
pub fn announce_app_data(identity: &Identity) -> Vec<u8> {
    use leviculum_lxmf::announce::DeliveryAnnounce;
    let hash = identity.hash();
    let mut name = alloc::vec::Vec::with_capacity(14);
    name.extend_from_slice(b"LNode-");
    for byte in &hash[..4] {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        name.push(HEX[(byte >> 4) as usize]);
        name.push(HEX[(byte & 0x0F) as usize]);
    }
    // Stamp cost 0: the node mines nothing (the `pow` feature is off), so
    // advertising a cost it cannot pay itself would be a lie to every
    // peer that reads it.
    DeliveryAnnounce {
        display_name: Some(name),
        stamp_cost: None,
        compression_supported: false,
    }
    .encode()
}

/// Build and register the node's `lxmf.delivery` destination.
///
/// Returns its hash, which is the source a receiver verifies against.
/// Fails only if the identity cannot be cloned out of transport, which is
/// the same failure mode the probe destination has.
pub fn register_delivery_destination<R, C, S>(
    node: &mut NodeCore<R, C, S>,
) -> Option<DestinationHash>
where
    R: CryptoRngCore,
    C: Clock,
    S: Storage,
{
    let identity_bytes = node.identity().private_key_bytes().ok()?;
    let identity = Identity::from_private_key_bytes(&identity_bytes).ok()?;
    let destination = leviculum_lxmf::LxmfNode::delivery_destination(identity).ok()?;
    let hash = *destination.hash();
    node.register_destination(destination);
    Some(hash)
}
