//! mvr: the board state behind `SKIPPED_INFRA reason=lnode_radio_config_failed
//! result=no_ack_after_3` — 26 LNode cells in the full run of 2026-09-22
//! (`b74027aa`).
//!
//! The LNode hands a host radio config to its LoRa task through a depth-1
//! `embassy-sync` channel (`leviculum-nrf/src/usb.rs::apply_radio_config`).
//! A board that booted on a `lora=off` media profile — which is what every
//! LoRa cell that follows a BLE cell finds, because the corpus pushes the
//! profile one reboot early — never spawned that task, so the channel has
//! no consumer. This test reproduces what the board's own debug log showed
//! (`board_delivery_promise_board_debug_2026-09-22T18-57-38Z.log`), on the
//! identical channel type with the identical payload type:
//!
//! * the boot's first config is taken (`SER: radio config received`) and
//!   fills the one slot for the rest of the boot;
//! * the grace `send` the `Full` path falls back to can never complete —
//!   not "is slow", *cannot*: its only waker is a consumer that does not
//!   exist, so the bounded grace period is guaranteed to expire
//!   (`SER: radio config undeliverable, refused`, three per cell, 27 in
//!   the run);
//! * every later config in the same boot is refused the same way.
//!
//! What the host cannot stand in for is the spawn decision itself: whether
//! the consumer exists is decided in the board mains (`bin/t114.rs` etc.)
//! on the embassy executor, thumbv7em-only. The firmware therefore now
//! routes around this channel whenever the boot did not spawn the LoRa
//! task (`media::lora_booted`, host-pinned in `leviculum-media-state`),
//! and this test is the mechanism half of that pair: proof that the
//! channel route can never produce an ack on such a boot, so the stored
//! route is not an optimisation but the only answerable one.

use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, TrySendError};
use leviculum_core::rnode::RadioConfigWire;

/// The corpus channel: 869.463 MHz / BW 125 k / SF8 / CR5, the values the
/// failing cells pushed.
fn corpus_config(sf: u8) -> RadioConfigWire {
    RadioConfigWire {
        frequency_hz: 869_463_000,
        bandwidth_hz: 125_000,
        sf,
        cr: 5,
        tx_power_dbm: 7,
        preamble_len: 16,
        csma_enabled: true,
        radio_silent: false,
        st_alock: 0,
        lt_alock: 0,
        lt_alock_present: true,
    }
}

#[test]
fn a_consumerless_config_channel_wedges_for_the_rest_of_the_boot() {
    // The firmware's channel, verbatim: depth 1, critical-section mutex,
    // radio-config payload. No receiver is ever constructed from it —
    // that is the `lora=off` boot.
    let channel: Channel<CriticalSectionRawMutex, RadioConfigWire, 1> = Channel::new();
    let tx = channel.sender();

    // The boot's first config: taken. On the old firmware this was the
    // moment the receipt-ack went out; the value itself is now parked in
    // the slot with nothing to drain it.
    assert!(tx.try_send(corpus_config(8)).is_ok());

    // Every subsequent config in this boot finds the slot full...
    for attempt in 0..3 {
        let cfg = corpus_config(9);
        let Err(TrySendError::Full(cfg)) = tx.try_send(cfg) else {
            panic!("attempt {attempt}: a consumerless depth-1 channel took a second config");
        };

        // ...and the grace `send` the firmware falls back to is Pending on
        // every poll, with no consumer to ever wake it: the grace period
        // (`CONFIG_DELIVER_WITHIN`) is not a margin that might save the
        // frame, it is a timer that is certain to expire. Polling is the
        // whole of embassy's contract for progress — a future that is
        // Pending under repeated polls and whose only waker is an absent
        // consumer never completes.
        let mut send = pin!(tx.send(cfg));
        let mut cx = Context::from_waker(core::task::Waker::noop());
        for _ in 0..64 {
            assert!(
                matches!(send.as_mut().poll(&mut cx), Poll::Pending),
                "attempt {attempt}: the grace send completed without a consumer"
            );
        }
    }

    // The wedge is the boot's, not the frame's: the slot still holds the
    // first config, so the state is identical for the next cell, and the
    // next, until a reset — which is exactly why one BLE-profiled boot
    // cost every following LNode cell of the run.
    assert!(matches!(
        tx.try_send(corpus_config(10)),
        Err(TrySendError::Full(_))
    ));
}

// ---------------------------------------------------------------------------
// The host's half: what an unanswered config costs on this side of the port
// ---------------------------------------------------------------------------
//
// The firmware half above is why a config can go unanswered. This half is what
// the host did about it, and it is the same finding from the other end
// (L-0007, Refs #334): the airtime bucket was built from the profile the
// config block ASKED for, and the only production call that could ever move it
// re-applied those same values in the ACK branch. So a board that did not take
// the config was driven at the profile it was not running — and under-pricing
// is the dangerous direction: an SF7 bucket in front of an SF12 board
// under-counts duty by an order of magnitude and hands the serial queue frames
// faster than the modem can key them.
//
// The answer that is not a guess is on the board: the firmware answers
// `TYPE_RADIO_QUERY` out of what its LoRa task actually configured
// (`leviculum-nrf/src/usb.rs`, `ControlAction::RadioQuery`, Codeberg #349).
// The two cases below are the two ways that can end.
//
// **Acceptance**: both red against the old policy (price the requested profile
// always, never refuse) injected into `radio_pricing_phy`, green with the
// policy this pass lands. The board is an in-memory duplex, so there is no
// hardware in either direction.

use leviculum_core::envelope;
use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use leviculum_std::interfaces::{radio_bring_up, radio_pricing_phy, RadioBringUp};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

/// The profile a corpus cell pushes at a board: 869.525 MHz / BW 250 k / SF7,
/// the `lora_path_discovery_wide_mixed` config of the 2026-09-23 capture.
fn requested_phy() -> RadioConfigWire {
    RadioConfigWire {
        frequency_hz: 869_525_000,
        bandwidth_hz: 250_000,
        sf: 7,
        cr: 8,
        tx_power_dbm: 2,
        preamble_len: 16,
        csma_enabled: true,
        radio_silent: false,
        st_alock: 0,
        lt_alock: 0,
        lt_alock_present: true,
    }
}

/// What the board is still on when it does not take that config: the profile
/// the cell before left it at, SF12/BW125k. Slower in every term, which is why
/// pricing the requested one under-counts.
fn running_phy() -> RadioConfigWire {
    RadioConfigWire {
        sf: 12,
        bandwidth_hz: 125_000,
        cr: 5,
        ..requested_phy()
    }
}

/// Airtime of a full single-frame payload at one profile — the number the
/// interface's credit bucket charges per frame, and the thing "priced at the
/// wrong PHY" means in milliseconds.
fn frame_cost_ms(phy: &RadioConfigWire) -> u64 {
    let bytes = (leviculum_core::rnode::MAX_SINGLE_PAYLOAD + 1) as u32;
    leviculum_core::rnode::airtime_ms_with_preamble(
        bytes,
        phy.bandwidth_hz,
        phy.sf,
        phy.cr,
        phy.preamble_len,
    )
}

/// A board on the far end of the port that never acks a radio config, and
/// answers the radio query only if `report` says so.
///
/// Deliberately nothing else: a board that answers the config is the green
/// path this pass does not change, and a board that answers more than it was
/// asked would let a test pass on a frame the host never requested.
fn scripted_board(mut port: DuplexStream, report: Option<RadioConfigWire>) {
    tokio::spawn(async move {
        let mut deframer = Deframer::with_max_frame(564);
        let mut buf = vec![0u8; 1024];
        let mut out = Vec::new();
        loop {
            let n = match port.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            for r in deframer.process(&buf[..n]) {
                let DeframeResult::Frame(data) = r else {
                    continue;
                };
                let Ok(f) = envelope::decode_frame(&data) else {
                    // The legacy radio config frame, which carries its own
                    // magic and not an envelope header. Unanswered, on
                    // purpose: that is the whole premise.
                    continue;
                };
                if f.frame_type != envelope::TYPE_RADIO_QUERY {
                    continue;
                }
                let Some(wire) = report else { continue };
                frame(&envelope::encode_radio_report(&wire), &mut out);
                if port.write_all(&out).await.is_err() {
                    return;
                }
                let _ = port.flush().await;
                out.clear();
            }
        }
    });
}

/// Config unanswered, report answered with the old PHY: the interface runs on
/// the reported profile and says the mismatch out loud.
///
/// Paused time, because the honest wait is three 2 s config attempts and one
/// 2 s query — eight seconds the runtime spends parked, not slept.
#[tokio::test(start_paused = true)]
async fn an_unanswered_config_prices_the_profile_the_board_reports() {
    let (mut host, board) = tokio::io::duplex(8192);
    scripted_board(board, Some(running_phy()));

    let requested = requested_phy();
    let outcome = radio_bring_up(&mut host, &requested, "mvr").await;
    assert_eq!(
        outcome,
        RadioBringUp::Running(running_phy()),
        "an unacknowledged config must leave the host asking the board what it \
         is running, not assuming"
    );

    let (priced, mismatch) = radio_pricing_phy(&outcome, &requested, "mvr")
        .expect("a board that answered is not refused");
    assert_eq!(
        priced,
        running_phy(),
        "the interface is priced at the requested profile, which the board is \
         not running"
    );
    // In milliseconds, on the frame the bucket charges for: the two profiles
    // are not a rounding apart.
    assert_eq!(frame_cost_ms(&priced), frame_cost_ms(&running_phy()));
    assert!(
        frame_cost_ms(&running_phy()) > 8 * frame_cost_ms(&requested),
        "SF12/125k against SF7/250k is {} ms against {} ms — if that ratio ever \
         shrinks, this case stops being a demonstration of under-pricing",
        frame_cost_ms(&running_phy()),
        frame_cost_ms(&requested)
    );

    let mismatch = mismatch.expect("a PHY the board is not running must be said out loud");
    for key in [
        "requested_sf=7",
        "requested_bw=250000",
        "requested_freq=869525000",
        "running_sf=12",
        "running_bw=125000",
    ] {
        assert!(
            mismatch.contains(key),
            "the mismatch event does not carry {key}: {mismatch}"
        );
    }
}

/// Neither frame answered: the interface refuses to come up, and the refusal
/// names both frames and the wait.
#[tokio::test(start_paused = true)]
async fn a_board_that_answers_neither_frame_does_not_come_up() {
    let (mut host, board) = tokio::io::duplex(8192);
    scripted_board(board, None);

    let requested = requested_phy();
    let outcome = radio_bring_up(&mut host, &requested, "mvr").await;
    assert_eq!(
        outcome,
        RadioBringUp::Silent,
        "a board that answers neither the config nor the query is not a board \
         whose profile the host may name"
    );

    let refusal = radio_pricing_phy(&outcome, &requested, "mvr")
        .expect_err("a profile nobody reported must not be priced at all");
    // Both frames, and the wait each got. An operator reading this line has to
    // be able to tell "the board never answered" from "the board said no".
    for key in [
        "config_frame=legacy-radio-config",
        "config_attempts=3",
        "config_wait_ms=2000",
        &format!("query_frame=0x{:02x}", envelope::TYPE_RADIO_QUERY),
        "query_wait_ms=2000",
    ] {
        assert!(
            refusal.contains(key),
            "the refusal does not carry {key}: {refusal}"
        );
    }

    // And the refusal is acted on rather than merely logged. The subject is
    // `serial_reconnect_task`'s control flow — it opens a `tokio_serial` port,
    // so no host test can reach it — and the failure mode being guarded is
    // precisely a refusal that falls through to the io task anyway. Same
    // reasoning as the source invariants in
    // `radio_config_sleeps_through_the_peer_yield_window`.
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/interfaces/serial.rs"),
    )
    .expect("read serial.rs");
    let call = src
        .find("match radio_pricing_phy(&outcome, &requested, &name)")
        .expect("serial_reconnect_task no longer settles the bring-up");
    let arm = src[call..]
        .find("Err(refusal) => {")
        .expect("no refusal arm");
    let end = src[call + arm..]
        .find("\n                    }\n")
        .expect("unterminated refusal arm");
    let body = &src[call + arm..call + arm + end];
    assert!(
        body.contains("counters.set_online(false)"),
        "a refused interface still reports online, so `rnstatus` shows it Up \
         (L-0020): {body}"
    );
    assert!(
        body.contains("return;"),
        "the refusal arm falls through to serial_io_task, so the interface \
         carries frames it cannot price: {body}"
    );
    assert!(
        !src[..call].contains("serial_io_task("),
        "serial_io_task is now started before the bring-up is settled, which \
         makes the refusal above unreachable"
    );
}

// ---------------------------------------------------------------------------
// The answer the stored route sends (Codeberg #363)
// ---------------------------------------------------------------------------
//
// The route above is the mechanism; this is what the board says about it. The
// decision itself lives in `leviculum-core`
// (`envelope::radio_config_answer`, tested there against every outcome),
// because a thumbv7em `match` is a wire contract no host test can reach. What
// is still only readable in the firmware is that the board *uses* it — and
// that the boot state it branches on is the media state's, not a guess — so
// that is what these two invariants hold. Same reasoning as the source
// invariants above and in
// `radio_config_sleeps_through_the_peer_yield_window`.

fn nrf_usb_source() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("leviculum-nrf/src/usb.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn a_taskless_boot_answers_out_of_the_media_state_and_the_shared_decision() {
    let usb = nrf_usb_source();

    // The branch: the stored route is taken because the boot did not bring
    // the carrier up, read off the media state. A firmware that guessed this
    // from the channel's fullness would take the wedge route on the very
    // boot the wedge is fatal on.
    let gate = usb
        .find("if !crate::media::lora_booted() {")
        .expect("apply_radio_config no longer asks the media state what booted");
    let stored = usb[gate..]
        .find("ConfigDelivery::Stored")
        .expect("the taskless branch no longer reaches the stored outcome");
    let branch = &usb[gate..gate + stored];
    assert!(
        branch.contains("request_save_confirmed"),
        "the taskless branch answers before the page write is confirmed, so \
         `stored` claims a reboot that the reboot itself would disprove \
         (#358): {branch}"
    );

    // The answer: built by the core decision, for both dialects. A firmware
    // that spelled the ack or the refusal here again would be a second copy
    // of a wire contract, free to drift from the one the tests cover.
    assert!(
        usb.contains("envelope::radio_config_answer("),
        "the enveloped radio-config answer is no longer built by \
         `envelope::radio_config_answer`, so what the board sends is not what \
         leviculum-core's tests grade"
    );
    assert!(
        usb.contains("envelope::legacy_radio_config_acked("),
        "the legacy radio-config ack is no longer decided by \
         `envelope::legacy_radio_config_acked`, so the two dialects can drift \
         on which outcomes they ack"
    );
    assert!(
        !usb.contains("encode_ack(envelope::TYPE_RADIO_CONFIG)"),
        "usb.rs acks a radio config directly again; the one ack this frame has \
         belongs to `ConfigDelivery::Applied` alone (#363)"
    );
}
