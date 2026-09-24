//! mvr for the frame an RNode modem accepts over serial and never keys,
//! measured twice on the rig at firmware 1.85 (t-beam-1 and t-beam-2,
//! 2026-09-24): `LORA_TX` written and the KISS frame complete, then no
//! listener decode, no receiver decode, no `airtime_short` step in the next
//! `LORA_CHTM` while the medium was free, and the frame not in the modem's
//! queue afterwards — the next frame aired alone.
//!
//! ## The mechanism, and why the host could not see it
//!
//! Six of the ten firmware paths from an accepted `CMD_DATA` frame to no
//! transmission signal nothing at all over KISS. The two that can produce
//! "written, never aired, not queued afterwards" are the length guards in the
//! two queue drains, which pop the packet's start and length BEFORE testing
//! them (`RNode_Firmware/RNode_Firmware.ino:586-589` and
//! `RNode_Firmware/RNode_Firmware.ino:626-628`), so a rejected packet is
//! already out of the queue. `stat_tx` is never incremented in 1.85 and
//! `CMD_READY` answers a single queue-full bit that no realistic backlog
//! reaches, so there is no counter to read either.
//!
//! What the firmware does emit is one per-transmission receipt, and the host
//! logged it for weeks without reading it: the `airtime_short` field of
//! `CMD_STAT_CHTM`. `kiss_indicate_channel_stats`
//! (`RNode_Firmware/RNode_Firmware.ino:712`) is the last statement of
//! `update_airtime`, which is the last statement of both queue drains,
//! reached only after `transmit()` ran `endPacket()` and `add_airtime` folded
//! the cost in. A CHTM therefore follows every drain — keyed or silently
//! consumed — and the presence or absence of a STEP in it is the difference.
//!
//! ## What this file measures
//!
//! Not delivery: there is no radio here and no second node. It measures the
//! two things the host now does with that receipt — it accuses, and it
//! re-hands once — against a scripted modem that answers KISS and keeps its
//! own ledger.
//!
//! Both directions are pinned, in one file, because the value of the
//! accusation is entirely in its being falsifiable:
//!
//! * a modem whose ledger stays frozen while the medium is idle must produce
//!   `LORA_TX_UNACCOUNTED` and exactly ONE re-hand of the same bytes, and
//!   then stop;
//! * a modem whose ledger rises by the SMALLEST step it can report — one raw
//!   unit, 1.5 ms of airtime — must produce none of it. Any rise means the
//!   frame reached `add_airtime`, and the accounting says so; a test that
//!   only fed it a full frame cost could not tell the documented rule from a
//!   threshold.
//!
//! ## Topology
//!
//! ```text
//!   node (in-process) ──► RNode channel interface
//!                          │ KISS over an in-memory duplex
//!                          v
//!                     fake modem: reports its PHY, timestamps CMD_DATA,
//!                                 emits CHTM on a cadence from a ledger
//!                                 this test controls
//! ```
//!
//! Sans-hardware, deterministic, a few seconds.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use leviculum_core::framing::kiss::{self, KissDeframeResult, KissDeframer};
use leviculum_core::identity::Identity;
use leviculum_core::rnode;
use leviculum_core::{Destination, DestinationType, Direction};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::interfaces::{RNodeChannelFactory, RNodeChannelHalves, RNodeChannelOpenFuture};
use leviculum_std::test_support::warn_capture::register_warn_capture;
use rand_core::OsRng;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The `[radio]` block of `hardware/lora_ratchet_rotation_listened.toml` — the
/// cell both measurements came out of.
const FREQ_HZ: u64 = 869_525_000;
const BW_HZ: u32 = 62_500;
const SF: u8 = 7;
const CR: u8 = 5;
const TX_POWER_DBM: i8 = 2;

/// How often the scripted modem reports its channel stats. The firmware's own
/// cadence is `UTIL_UPDATE_INTERVAL_MS` = 1000 ms
/// (`RNode_Firmware/Config.h:179`) plus one frame per queue drain; faster here
/// so the test spends seconds rather than tens of them, and the host's
/// deadline of one firmware cadence past the hold still has to hold.
const CHTM_CADENCE: Duration = Duration::from_millis(400);

/// What the ledger does when the modem is handed a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ledger {
    /// The measured shape: the frame is consumed and the ledger never moves.
    Frozen,
    /// The frame is charged the smallest step the CHTM fields can carry — one
    /// raw unit, 0.01 %, 1.5 ms inside the firmware's 15 s window.
    OneRawUnit,
}

/// What the modem reported about itself, read back out of its
/// `CMD_STAT_PHYPRM` frame.
#[derive(Debug, Clone, Copy, Default)]
struct ReportedPhy {
    csma_slot_ms: u64,
    csma_difs_ms: u64,
}

/// One frame the modem was handed, and when.
#[derive(Debug, Clone)]
struct Handover {
    at: Instant,
    payload: Vec<u8>,
}

#[derive(Default)]
struct ModemRecord {
    handovers: Vec<Handover>,
    phy: ReportedPhy,
    /// The ledger as the modem would report it: raw CHTM units.
    airtime_short: u16,
    airtime_long: u16,
    radio_on: bool,
}

/// Hands leviculum one half of an in-memory duplex; the modem owns the other.
struct DuplexFactory(Mutex<Option<RNodeChannelHalves>>);

impl RNodeChannelFactory for DuplexFactory {
    fn open(&self) -> RNodeChannelOpenFuture {
        let taken = self.0.lock().expect("factory lock").take();
        Box::pin(async move { taken.ok_or_else(|| "channel already opened".into()) })
    }
}

/// The firmware's own slot derivation, transcribed rather than imported: 12
/// symbol times, clamped to at most 100 ms and at least 24 ms — 6 ms when the
/// modulation runs faster than 30 kbps (`Config.h:102-107`,
/// `Utilities.h:1244-1252`). The host has its own copy of this rule; the point
/// of a scripted modem is that it speaks for the firmware, not for the host.
fn modem_slot_ms(bw_hz: u32, sf: u8, cr: u8) -> u64 {
    let symbol_us = (1u64 << sf) * 1_000_000 / bw_hz as u64;
    let slot_ms = 12 * symbol_us / 1_000;
    let bitrate_bps = sf as u64 * 4 * bw_hz as u64 / (cr as u64 * (1u64 << sf));
    let floor = if bitrate_bps > 30_000 { 6 } else { 24 };
    slot_ms.clamp(floor, 100)
}

/// The 11-byte single-interface `CMD_STAT_CHTM` payload, in the firmware's own
/// field order and scaling (`kiss_indicate_channel_stats`,
/// `RNode_Firmware/Utilities.h:959-981`).
///
/// `channel_load_short` is `total_channel_util` = `local_channel_util` +
/// `airtime` (`RNode_Firmware/RNode_Firmware.ino:1459`), so a modem that is
/// keying but hears nothing else reports its own airtime there and no more:
/// that is what tells the host the medium was free.
fn chtm_payload(airtime_short: u16, airtime_long: u16) -> Vec<u8> {
    let mut p = Vec::with_capacity(11);
    p.extend_from_slice(&airtime_short.to_be_bytes());
    p.extend_from_slice(&airtime_long.to_be_bytes());
    p.extend_from_slice(&airtime_short.to_be_bytes());
    p.extend_from_slice(&airtime_long.to_be_bytes());
    // current_rssi, noise_floor (both `raw - 157` dBm), interference = none.
    p.push(0x00);
    p.push(0x00);
    p.push(0xFF);
    p
}

/// A minimal RNode firmware: answers the detect probe and the radio
/// configuration, reports its PHY, timestamps every `CMD_DATA` it is handed,
/// and reports its channel stats on a cadence.
///
/// It does NOT send `CMD_STAT_CSMA`: the firmware emits that only when its
/// contention band changes (`RNode_Firmware/RNode_Firmware.ino:1614-1618`), so
/// a modem that stays in band 1 never sends one, which is the case this
/// models.
async fn fake_modem(
    mut peer: tokio::io::DuplexStream,
    record: Arc<Mutex<ModemRecord>>,
    mode: Ledger,
) {
    let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
    let mut buf = [0u8; 1024];
    let mut stat_tick = tokio::time::interval(CHTM_CADENCE);
    let push = |reply: &mut Vec<u8>, cmd: u8, payload: &[u8]| {
        let mut one = Vec::new();
        kiss::frame(cmd, payload, &mut one);
        reply.extend_from_slice(&one);
    };
    loop {
        let mut reply: Vec<u8> = Vec::new();
        tokio::select! {
            read = peer.read(&mut buf) => {
                let n = match read {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                for f in deframer.process(&buf[..n]) {
                    if let KissDeframeResult::Frame { command, payload } = f {
                        match command {
                            rnode::CMD_DETECT => {
                                push(&mut reply, rnode::CMD_DETECT, &[rnode::DETECT_RESP]);
                                push(
                                    &mut reply,
                                    rnode::CMD_FW_VERSION,
                                    &[rnode::REQUIRED_FW_MAJ, rnode::REQUIRED_FW_MIN],
                                );
                                push(&mut reply, rnode::CMD_PLATFORM, &[rnode::PLATFORM_ESP32]);
                                push(&mut reply, rnode::CMD_MCU, &[0x00]);
                            }
                            rnode::CMD_RADIO_STATE => {
                                push(&mut reply, command, &payload);
                                if payload.first() == Some(&rnode::RADIO_STATE_ON) {
                                    // `updateBitrate()` recomputes the slot and
                                    // DIFS and then calls `setPreamble()`, whose
                                    // last statement reports both.
                                    let symbol_us = (1u64 << SF) * 1_000_000 / BW_HZ as u64;
                                    let preamble =
                                        rnode::derive_preamble_symbols(SF, CR, BW_HZ);
                                    let preamble_time_ms =
                                        (preamble as u64 * symbol_us).div_ceil(1_000) as u16;
                                    let slot = modem_slot_ms(BW_HZ, SF, CR);
                                    let difs = 2 * slot;
                                    let mut stat = Vec::new();
                                    stat.extend_from_slice(&(symbol_us as u16).to_be_bytes());
                                    stat.extend_from_slice(
                                        &((BW_HZ as u64 / (1u64 << SF)) as u16).to_be_bytes(),
                                    );
                                    stat.extend_from_slice(&preamble.to_be_bytes());
                                    stat.extend_from_slice(&preamble_time_ms.to_be_bytes());
                                    stat.extend_from_slice(&(slot as u16).to_be_bytes());
                                    stat.extend_from_slice(&(difs as u16).to_be_bytes());
                                    push(&mut reply, rnode::CMD_STAT_PHYPRM, &stat);
                                    let mut rec = record.lock().expect("record lock");
                                    rec.phy = ReportedPhy {
                                        csma_slot_ms: slot,
                                        csma_difs_ms: difs,
                                    };
                                    rec.radio_on = true;
                                }
                            }
                            rnode::CMD_FREQUENCY
                            | rnode::CMD_BANDWIDTH
                            | rnode::CMD_TXPOWER
                            | rnode::CMD_SF
                            | rnode::CMD_CR => push(&mut reply, command, &payload),
                            rnode::CMD_DATA => {
                                let mut rec = record.lock().expect("record lock");
                                rec.handovers.push(Handover {
                                    at: Instant::now(),
                                    payload: payload.to_vec(),
                                });
                                if mode == Ledger::OneRawUnit {
                                    rec.airtime_short = rec.airtime_short.saturating_add(1);
                                    rec.airtime_long = rec.airtime_long.saturating_add(1);
                                }
                                // Every queue drain ends in `update_airtime()`,
                                // whose last statement is the CHTM frame — keyed
                                // or silently consumed, the receipt follows.
                                let (ats, atl) = (rec.airtime_short, rec.airtime_long);
                                drop(rec);
                                push(&mut reply, rnode::CMD_STAT_CHTM, &chtm_payload(ats, atl));
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ = stat_tick.tick() => {
                let rec = record.lock().expect("record lock");
                if !rec.radio_on {
                    continue;
                }
                let (ats, atl) = (rec.airtime_short, rec.airtime_long);
                drop(rec);
                push(&mut reply, rnode::CMD_STAT_CHTM, &chtm_payload(ats, atl));
            }
        }
        if !reply.is_empty() && peer.write_all(&reply).await.is_err() {
            return;
        }
    }
}

/// A destination this node can announce, to get a frame onto the medium
/// without a peer, a path or a link.
fn announceable(app_name: &str) -> Destination {
    Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        app_name,
        &["unaccounted", "mvr"],
    )
    .expect("destination")
}

/// Run one announce past a scripted modem and report what it was handed.
///
/// The wait is generous on purpose: the host may accuse only once a CHTM
/// arrives past the frame's own hold (airtime at this PHY plus the DIFS and
/// the widest contention draw the modem reported), and the re-hand then owes
/// a fresh acquisition wait of its own. Two full rounds of that, plus the
/// modem's stat cadence, fit inside six seconds at this PHY with room to
/// spare — and the point of the second assertion in each test is that nothing
/// further happens in the remaining seconds.
async fn one_announce_past(mode: Ledger, app_name: &str) -> (Vec<Handover>, ReportedPhy) {
    let (port, peer) = tokio::io::duplex(64 * 1024);
    let (read_half, write_half) = tokio::io::split(port);
    let halves: Mutex<Option<RNodeChannelHalves>> = Mutex::new(Some((
        Box::new(read_half) as Box<dyn AsyncRead + Send + Unpin>,
        Box::new(write_half) as Box<dyn AsyncWrite + Send + Unpin>,
    )));

    let record = Arc::new(Mutex::new(ModemRecord::default()));
    let modem = tokio::spawn(fake_modem(peer, Arc::clone(&record), mode));

    let storage = tempfile::tempdir().expect("storage");
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .add_rnode_channel_interface(
            Arc::new(DuplexFactory(halves)),
            FREQ_HZ,
            BW_HZ,
            SF,
            CR,
            TX_POWER_DBM,
        )
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("node builds");
    node.start().await.expect("node starts");

    // The radio has to be configured and online, and at least one CHTM has to
    // have arrived, before the frame under test: with no previous reading
    // there is no step to take and the accounting declines to judge. This is
    // the modem's own precondition, not a test artefact — the firmware reports
    // channel stats on its cadence from the moment the radio is on.
    let dest = announceable(app_name);
    let hash = *dest.hash();
    node.register_destination(dest);
    tokio::time::sleep(Duration::from_secs(2)).await;
    record.lock().expect("record lock").handovers.clear();

    node.announce_destination(&hash, Some(b"u"))
        .await
        .expect("announce");
    tokio::time::sleep(Duration::from_secs(6)).await;

    let (handovers, phy) = {
        let rec = record.lock().expect("record lock");
        (rec.handovers.clone(), rec.phy)
    };
    node.stop().await.ok();
    modem.abort();
    (handovers, phy)
}

/// Parse the scalar fields of one `key=value` trace line.
fn fields(line: &str) -> std::collections::BTreeMap<&str, &str> {
    line.split_whitespace()
        .filter_map(|tok| tok.split_once('='))
        .collect()
}

/// THE pin, accusing half: a ledger that does not move while the medium is
/// idle names the frame and re-hands it once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_frozen_airtime_ledger_names_the_frame_and_re_hands_it_once() {
    let warns = register_warn_capture();
    let (handovers, phy) = one_announce_past(Ledger::Frozen, "unaccfroz").await;

    assert!(
        phy.csma_slot_ms > 0 && phy.csma_difs_ms > 0,
        "the modem must have reported its CSMA parameters before the frame; \
         the hold the accusation waits out is priced from them: got {phy:?}"
    );
    assert!(
        !handovers.is_empty(),
        "the announce must reach the modem — a frame that never arrives is a \
         different bug"
    );

    let captured = warns.snapshot();
    let unaccounted: Vec<&str> = captured
        .lines()
        .filter(|l| l.contains("LORA_TX_UNACCOUNTED"))
        .collect();
    assert!(
        !unaccounted.is_empty(),
        "a frame the modem consumed without transmitting must be named: the \
         ledger stayed at 0.00 % across {} handover(s) and the medium was \
         idle throughout, and nothing said so.\ncaptured warns:\n{captured}",
        handovers.len()
    );

    let f = fields(unaccounted[0]);
    let len: u64 = f
        .get("len")
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("LORA_TX_UNACCOUNTED must carry len: {}", unaccounted[0]));
    assert_eq!(
        len,
        handovers[0].payload.len() as u64,
        "the accusation must name the frame's own length: {}",
        unaccounted[0]
    );
    let handover_t: u64 = f
        .get("handover_t")
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("must carry handover_t: {}", unaccounted[0]));
    let chtm_t: u64 = f
        .get("chtm_t")
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("must carry chtm_t: {}", unaccounted[0]));
    assert!(
        chtm_t >= handover_t,
        "the reading that failed to account for the frame cannot predate the \
         handover it is about: {}",
        unaccounted[0]
    );
    let expected: f64 = f
        .get("expected_delta")
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("must carry expected_delta: {}", unaccounted[0]));
    assert!(
        expected > 0.0,
        "an expectation of zero would make the accusation vacuous: {}",
        unaccounted[0]
    );

    assert!(
        captured.contains("LORA_TX_REHAND"),
        "the workaround must name its retry.\ncaptured warns:\n{captured}"
    );

    // The retry itself, from the modem's side: the same bytes again, once.
    assert_eq!(
        handovers.len(),
        2,
        "the frame must be re-handed exactly once — a modem that swallowed the \
         retry too is not going to send it, and a second retry would only buy \
         latency for everything behind it. Handover lengths: {:?}",
        handovers
            .iter()
            .map(|h| h.payload.len())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        handovers[0].payload, handovers[1].payload,
        "the re-hand must be the same frame, byte for byte"
    );
    let gap = handovers[1].at.duration_since(handovers[0].at);
    let hold_floor = Duration::from_millis(phy.csma_difs_ms);
    assert!(
        gap > hold_floor,
        "the retry must wait out the frame's hold before it is even \
         suspected, and then its own acquisition wait: {gap:?} against a DIFS \
         of {hold_floor:?}"
    );
}

/// THE pin, absolving half: the smallest step the modem can report is proof
/// the frame keyed, and produces no accusation and no retry.
///
/// Asserted on the modem's record rather than on the captured warns: every
/// registered capture buffer sees every warn in the process, so "no
/// `LORA_TX_UNACCOUNTED` in the buffer" could fail on the other test's line
/// under the parallel suite. A re-hand is the accusation's only consequence,
/// and it is visible here per-modem.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_raw_unit_of_airtime_absolves_the_frame() {
    let (handovers, phy) = one_announce_past(Ledger::OneRawUnit, "unaccrise").await;

    assert!(
        phy.csma_slot_ms > 0,
        "the modem must have reported its CSMA parameters: got {phy:?}"
    );
    assert_eq!(
        handovers.len(),
        1,
        "a ledger that rose by one raw unit — 1.5 ms, the smallest step the \
         CHTM fields can carry — says the frame reached `add_airtime`, which \
         is reachable only after `endPacket()` returned. Re-handing it would \
         duplicate a frame that keyed. Handover lengths: {:?}",
        handovers
            .iter()
            .map(|h| h.payload.len())
            .collect::<Vec<_>>()
    );
}
