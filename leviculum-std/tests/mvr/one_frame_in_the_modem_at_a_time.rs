//! mvr for the `lora_ratchet_rotation` red of 2026-09-23 (`sent=10 recv=4`
//! in the two latest full runs, green in the one before).
//!
//! ## The mechanism
//!
//! The selftest hands both daemons five frames each inside one frame
//! airtime. The interface charged channel access on ACQUISITION only: the
//! first frame of a burst served the randomised wait `ChannelAccess` draws,
//! every further frame followed at the fixed `rnode::MIN_SPACING_MS` (50 ms)
//! — documented at its definition as the serial-buffer floor and explicitly
//! not a CSMA-fair pacing — against a frame airtime of several hundred
//! milliseconds at every corpus PHY. Three to five frames therefore sat in
//! the modem's queue at once.
//!
//! The RNode firmware runs its CSMA once per queue drain and then flushes
//! everything it holds with no carrier sense between frames
//! (`tx_queue_handler` → `flush_queue()`, `RNode_Firmware.ino:1623-1645`;
//! Codeberg #36). When both modems hold queues at the same medium-free
//! instant and both win their draw inside one another's preamble time, each
//! transmits its whole queue deaf: the overlap destroys three frames per
//! side, the tails land, and the count is 4 of 10 by arithmetic — twice.
//! When one queue fills while the other burst is already on the air, the
//! firmware's DIFS restart serialises the two and the same load is 10 of 10,
//! which is the run before. Codeberg #374 is the same firmware behaviour
//! from the other side: a queued second frame leaves deaf behind the first,
//! and the answer to the first returns into it.
//!
//! ## What this file measures
//!
//! Not delivery — there is no radio here and no second node. It measures the
//! one thing the host controls and the one layer allowed to know about the
//! medium: the instants at which the interface hands frames to the modem.
//! The fake modem below is the firmware's serial side. It answers the detect
//! probe and the radio configuration, reports its PHY and CSMA parameters
//! the way `updateBitrate()` → `setPreamble()` → `kiss_indicate_phy_stats()`
//! does at every radio configuration (`Utilities.h:1226-1228,1243-1258`).
//!
//! The handover instants themselves are stamped one layer earlier, in the
//! write the interface hands the bytes down with ([`StampingWrite`]) — not in
//! the modem's read loop, which is a task wake-up and can be late by more
//! than this test's whole margin.
//!
//! The assertion is that the second frame of a burst does not reach the
//! modem until the first has left the air: the first frame's airtime at the
//! PHY the modem reported, plus the DIFS it reported, plus its longest
//! contention draw. Every term is read back out of the modem's own stat
//! frame; none is typed into the test. Before the fix the gap is the 50 ms
//! serial floor at a frame airtime of ~600 ms, and this test fails on the
//! first assertion.
//!
//! ## Topology
//!
//! ```text
//!   node (in-process) ──► RNode channel interface
//!                          │ KISS over an in-memory duplex
//!                          v
//!                     fake modem: reports its PHY, timestamps CMD_DATA
//! ```
//!
//! Sans-hardware, deterministic, a few seconds.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use leviculum_channel_access::JITTER_CW_SLOTS;
use leviculum_core::framing::kiss::{self, KissDeframeResult, KissDeframer};
use leviculum_core::identity::Identity;
use leviculum_core::rnode;
use leviculum_core::{Destination, DestinationType, Direction};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::interfaces::{RNodeChannelFactory, RNodeChannelHalves, RNodeChannelOpenFuture};
use rand_core::OsRng;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The `[radio]` block of `hardware/lora_ratchet_rotation.toml` — the cell
/// that is red.
const FREQ_HZ: u64 = 869_525_000;
const BW_HZ: u32 = 62_500;
const SF: u8 = 7;
const CR: u8 = 5;
const TX_POWER_DBM: i8 = 2;

/// What the modem reported about itself, read back out of its `CMD_STAT_PHYPRM`
/// frame. The test builds its expectation from these and from the length of
/// the frame it actually saw — never from a typed millisecond figure.
#[derive(Debug, Clone, Copy, Default)]
struct ReportedPhy {
    preamble_symbols: u16,
    csma_slot_ms: u64,
    csma_difs_ms: u64,
}

/// One frame the modem was handed, and when — the instant the interface let
/// go of it, stamped by [`StampingWrite`].
#[derive(Debug, Clone)]
struct Handover {
    at: Instant,
    len: u32,
}

#[derive(Default)]
struct ModemRecord {
    handovers: Vec<Handover>,
    phy: ReportedPhy,
}

/// Hands leviculum one half of an in-memory duplex; the modem owns the other.
struct DuplexFactory(Mutex<Option<RNodeChannelHalves>>);

impl RNodeChannelFactory for DuplexFactory {
    fn open(&self) -> RNodeChannelOpenFuture {
        let taken = self.0.lock().expect("factory lock").take();
        Box::pin(async move { taken.ok_or_else(|| "channel already opened".into()) })
    }
}

/// The write half the interface is handed, with a KISS deframer reading over
/// the shoulder of every write: each `CMD_DATA` frame is stamped at the
/// `poll_write` that completed it.
///
/// Until 2026-09-25 the stamp was taken in the fake modem's read loop. Both
/// ends of the duplex run on one runtime, so that stamp is a task WAKE-UP —
/// the instant the reader was next scheduled, not the instant the frame
/// crossed the boundary — and this test allows no slack at all: the hold is a
/// sleep of exactly the milliseconds the assertion recomputes, so whatever the
/// loaded host adds to the FIRST wake-up alone comes straight off the measured
/// gap. The land gate on 23641c94 reported 752 ms against 924 owed, and
/// `stress-ng --cpu 20 --vm 2` beside the test at `nice -n 19` reproduced it
/// at 760 ms and 820 ms in two of five runs — on a tree whose `tx_hold` is
/// byte-identical to the one that was green nine commits earlier, so there was
/// nothing in the interface to find. The same load pushed other runs to
/// 1046 ms and 1052 ms: the wake-up moved the gap in both directions, which is
/// what a delayed observer looks like and not what a changed policy looks
/// like.
///
/// The write is the right place for it on the merits, not only for the
/// stability. What this test measures is the one thing the host decides — when
/// it lets go of a frame — and the host has no influence on when the firmware
/// on the far side of a serial line gets around to reading it. The stamp is
/// taken inside the interface's own task, in the call that hands the bytes
/// down, before the hold timer for that frame exists; the two stamps the gap
/// is made of are therefore separated by exactly the timer the interface set
/// between them, and nothing the scheduler does to any other task can move
/// them.
struct StampingWrite<W> {
    inner: W,
    deframer: KissDeframer,
    record: Arc<Mutex<ModemRecord>>,
}

impl<W> StampingWrite<W> {
    fn new(inner: W, record: Arc<Mutex<ModemRecord>>) -> Self {
        Self {
            inner,
            deframer: KissDeframer::with_max_payload(rnode::HW_MTU),
            record,
        }
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for StampingWrite<W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let me = self.get_mut();
        let written = match Pin::new(&mut me.inner).poll_write(cx, buf) {
            Poll::Ready(Ok(n)) => n,
            other => return other,
        };
        // Only the bytes that were actually accepted: a short write leaves the
        // rest for the next call, and a frame that completes there is stamped
        // there.
        let at = Instant::now();
        for f in me.deframer.process(&buf[..written]) {
            if let KissDeframeResult::Frame { command, payload } = f {
                if command == rnode::CMD_DATA {
                    me.record
                        .lock()
                        .expect("record lock")
                        .handovers
                        .push(Handover {
                            at,
                            len: payload.len() as u32,
                        });
                }
            }
        }
        Poll::Ready(Ok(written))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// The firmware's own slot derivation, transcribed rather than imported: 12
/// symbol times, clamped to at most 100 ms and at least 24 ms — 6 ms when the
/// modulation runs faster than 30 kbps (`Config.h:102-107`,
/// `Utilities.h:1244-1252`). The host has its own copy of this rule; the
/// point of the test is that the two agree, so this side must not call the
/// host's.
fn modem_slot_ms(bw_hz: u32, sf: u8, cr: u8) -> u64 {
    let symbol_us = (1u64 << sf) * 1_000_000 / bw_hz as u64;
    let slot_ms = 12 * symbol_us / 1_000;
    let bitrate_bps = sf as u64 * 4 * bw_hz as u64 / (cr as u64 * (1u64 << sf));
    let floor = if bitrate_bps > 30_000 { 6 } else { 24 };
    slot_ms.clamp(floor, 100)
}

/// A minimal RNode firmware: answers the detect probe and the radio
/// configuration, and reports its PHY and CSMA parameters once the radio is
/// on. It does not stamp the data frames — see [`StampingWrite`].
///
/// It does NOT send `CMD_STAT_CSMA`: the firmware emits that only when its
/// contention band changes (`RNode_Firmware.ino:1614-1618`), so a modem that
/// stays in band 1 never sends one, which is the case this models.
async fn fake_modem(mut peer: tokio::io::DuplexStream, record: Arc<Mutex<ModemRecord>>) {
    let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
    let mut buf = [0u8; 1024];
    let push = |reply: &mut Vec<u8>, cmd: u8, payload: &[u8]| {
        let mut one = Vec::new();
        kiss::frame(cmd, payload, &mut one);
        reply.extend_from_slice(&one);
    };
    loop {
        let n = match peer.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        let mut reply: Vec<u8> = Vec::new();
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
                            // `updateBitrate()` recomputes the slot and DIFS
                            // and then calls `setPreamble()`, whose last
                            // statement reports both.
                            let symbol_us = (1u64 << SF) * 1_000_000 / BW_HZ as u64;
                            let preamble = rnode::derive_preamble_symbols(SF, CR, BW_HZ);
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
                                preamble_symbols: preamble,
                                csma_slot_ms: slot,
                                csma_difs_ms: difs,
                            };
                        }
                    }
                    rnode::CMD_FREQUENCY
                    | rnode::CMD_BANDWIDTH
                    | rnode::CMD_TXPOWER
                    | rnode::CMD_SF
                    | rnode::CMD_CR => push(&mut reply, command, &payload),
                    // `CMD_DATA` is deliberately not timestamped here: the
                    // stamp that matters is taken in `StampingWrite`, on the
                    // other side of the duplex. Falls through to the modem's
                    // silence, which is what the firmware answers a data
                    // frame with.
                    _ => {}
                }
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
        &["hold", "mvr"],
    )
    .expect("destination")
}

/// THE pin: the modem is handed the next frame only after the previous one
/// has left the air.
#[tokio::test]
async fn the_modem_holds_one_frame_at_a_time() {
    let (port, peer) = tokio::io::duplex(64 * 1024);
    let (read_half, write_half) = tokio::io::split(port);
    let record = Arc::new(Mutex::new(ModemRecord::default()));
    let halves: Mutex<Option<RNodeChannelHalves>> = Mutex::new(Some((
        Box::new(read_half) as Box<dyn AsyncRead + Send + Unpin>,
        Box::new(StampingWrite::new(write_half, Arc::clone(&record)))
            as Box<dyn AsyncWrite + Send + Unpin>,
    )));

    let modem = tokio::spawn(fake_modem(peer, Arc::clone(&record)));

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

    // The radio has to be configured and online before the burst, or what is
    // measured is the lifecycle rather than the spacing.
    let dest_a = announceable("holdmvra");
    let dest_b = announceable("holdmvrb");
    let hash_a = *dest_a.hash();
    let hash_b = *dest_b.hash();
    node.register_destination(dest_a);
    node.register_destination(dest_b);
    tokio::time::sleep(Duration::from_secs(2)).await;
    record.lock().expect("record lock").handovers.clear();

    // The burst: two frames handed down in the same instant, the shape the
    // selftest produces and the shape a relay produces when an announce
    // queues behind a proof.
    node.announce_destination(&hash_a, Some(b"a"))
        .await
        .expect("announce a");
    node.announce_destination(&hash_b, Some(b"b"))
        .await
        .expect("announce b");

    // Long enough for the acquisition draw (48..360 ms at this PHY) plus one
    // hold (~1 s for an announce-sized frame at SF7/62.5 kHz), with room to
    // spare. A test that read too early would see one frame and could not
    // tell a held frame from a lost one, so it waits well past both.
    tokio::time::sleep(Duration::from_secs(5)).await;

    let (handovers, phy) = {
        let rec = record.lock().expect("record lock");
        (rec.handovers.clone(), rec.phy)
    };

    assert!(
        phy.csma_slot_ms > 0 && phy.csma_difs_ms > 0,
        "the modem must have reported its CSMA parameters before the first \
         frame; got {phy:?}"
    );
    assert!(
        handovers.len() >= 2,
        "both announces must be handed down — this is a spacing test, and \
         a frame that is never sent at all is a different bug: got {} \
         handovers",
        handovers.len()
    );

    // What the first frame occupies the air for, at the PHY the modem
    // reported, for the length the modem actually received.
    let airtime_ms =
        rnode::airtime_ms_with_preamble(handovers[0].len, BW_HZ, SF, CR, phy.preamble_symbols);
    // The contest the firmware runs before the next frame: the DIFS it
    // reported, plus its longest contention draw. `random(cw_min, cw_max)`
    // is upper-exclusive, so at band 1 the widest draw is one slot short of
    // the window's width.
    let cw_ms = (JITTER_CW_SLOTS as u64 - 1) * phy.csma_slot_ms;
    let owed_ms = airtime_ms + phy.csma_difs_ms + cw_ms;

    let gap_ms = handovers[1].at.duration_since(handovers[0].at).as_millis() as u64;
    println!(
        "TX_HOLD_MVR len={} airtime_ms={airtime_ms} difs_ms={} cw_ms={cw_ms} \
         owed_ms={owed_ms} gap_ms={gap_ms}",
        handovers[0].len, phy.csma_difs_ms
    );

    assert!(
        gap_ms >= owed_ms,
        "the second frame reached the modem {gap_ms} ms after the first, \
         while the first was still on the air: it needs {airtime_ms} ms of \
         airtime and the firmware then owes {} ms of DIFS and {cw_ms} ms of \
         contention before it can key again ({owed_ms} ms in total). Two \
         frames in the modem's queue are flushed back to back with no \
         carrier sense between them, and the second one leaves deaf.",
        phy.csma_difs_ms
    );
    assert!(
        gap_ms < owed_ms + 1_000,
        "the hold must be the frame's own cost and no more: {gap_ms} ms \
         against {owed_ms} ms owed"
    );

    node.stop().await.ok();
    modem.abort();
}
