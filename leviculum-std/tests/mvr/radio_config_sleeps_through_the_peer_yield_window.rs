//! mvr: `lora_path_discovery_wide_mixed` went `SKIPPED_INFRA
//! reason=lnode_radio_config_failed result=no_ack_after_3` on the T114
//! DEC9947D in the 648650a3 full run, 2026-09-23 08:03 UTC, because the LoRa
//! loop was parked in a 20 s RX window that had no way to notice a config.
//!
//! The board's own debug capture
//! (`lora_path_discovery_wide_mixed_receiver_debug_2026-09-23T08-03-06Z.log`,
//! board time in ms):
//!
//! ```text
//! 227028  [T114_PEER_YIELD] after_empty=2 yield_ms=20000
//! 227029  [SX_RX_ARM] site=yield timeout_ms=20000 dark_ms=1
//! 229452  [SER ] SER: radio config received
//! 229537  [RADIO] persist saved freq=869525000 bw=250000 sf=7 cr=8 pwr=2
//! 230652  [SER ] SER: radio config apply unconfirmed, answered busy
//! 231954  [SER ] SER: radio config undeliverable, refused
//! 233957  [SER ] SER: radio config undeliverable, refused
//! ```
//!
//! The cell before it had left the board at SF12, so the peer-turn yield
//! (`lora.rs`, `PEER_YIELD_AFTER_EMPTY`) was two post-TX windows = 20 s. The
//! config landed 2.4 s into it. f4ecf16ab had made "a radio config wake the
//! LoRa loop instead of waiting out its RX window" — but only at the idle
//! select, which is not the window the loop was in. So:
//!
//! * the loop slept on, and the serial task's 1.2 s wait for the apply ran
//!   out: `answered busy`, which is truthful and retryable;
//! * the host retried the *same* config, and the one-slot `LORA_CONFIG`
//!   channel was still holding its first copy, so the retry was answered
//!   `undeliverable` — a statement about the channel, where the host had
//!   asked about the radio;
//! * periculum gave up after three and skipped the cell.
//!
//! Two failures, one per half of that. This file pins both. What it cannot
//! host-run is the LoRa loop itself: `lora_task` is an embassy task on
//! thumbv7em, so the window that has to wake is not linkable here. The parts
//! that are host-testable are the arithmetic that makes the window fatal and
//! the channel semantics the fix rests on; the loop's own compliance is a
//! source invariant, asserted the way `lnode_debug_log_format.rs` asserts the
//! firmware's log shapes.

use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll};
use std::path::{Path, PathBuf};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use leviculum_core::rnode::RadioConfigWire;

/// `usb.rs::CONFIG_DELIVER_WITHIN` + `CONFIG_APPLY_WITHIN`: everything the
/// serial task will wait before it answers the host without an ack.
const SERIAL_ANSWER_BUDGET_MS: u64 = 500 + 1200;

fn nrf_source(rel: &str) -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("leviculum-nrf/src")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The config the run pushed, as the capture's `persist saved` line reports
/// it: 869.525 MHz / BW 250 k / SF7 / CR8 / 2 dBm.
fn pushed_config() -> RadioConfigWire {
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

/// Why a window that does not wake is fatal rather than slow: at the PHY the
/// board was sitting on, the yield window alone is an order of magnitude
/// longer than everything the serial task will wait.
///
/// `post_tx_rx_window_ms` is one full single-frame reply's airtime plus a
/// turnaround margin (the pacing margin and two CSMA slots), clamped to
/// 10 s, and the peer-turn yield is two of them. At SF12/125 kHz the sum
/// runs past the clamp, so the yield is exactly the 20 000 ms the capture
/// reports and no slower profile can make it shorter.
#[test]
fn the_peer_yield_window_dwarfs_the_serial_answer_budget() {
    // `post_tx_rx_window_ms`: header + max single payload, at the PHY the
    // previous cell left the board on.
    let reply_bytes = (leviculum_core::rnode::MAX_SINGLE_PAYLOAD + 1) as u32;
    let reply_airtime =
        leviculum_core::rnode::airtime_ms_with_preamble(reply_bytes, 125_000, 12, 5, 16);
    // `compute_slot_ms`: a tenth of a 500-byte frame's airtime. Its
    // `CSMA_SLOT_MS_MIN` floor of 24 ms does not bind at this PHY — it is
    // there for the fast profiles — so the floor is not reproduced here.
    let slot_ms = leviculum_core::rnode::airtime_ms(500, 125_000, 12, 5) / 10;
    let turnaround = leviculum_core::rnode::PACING_MARGIN_MS + 2 * slot_ms;
    assert!(
        reply_airtime + turnaround >= 10_000,
        "an SF12/125k post-TX window is {} ms, so the 10 s clamp in \
         post_tx_rx_window_ms is not what sizes this window any more",
        reply_airtime + turnaround
    );
    let post_tx_window_ms = (reply_airtime + turnaround).clamp(1, 10_000);
    let yield_ms = post_tx_window_ms * 2;
    assert_eq!(
        yield_ms, 20_000,
        "the arithmetic no longer reproduces the capture's `yield_ms=20000`"
    );
    assert!(
        yield_ms > SERIAL_ANSWER_BUDGET_MS,
        "a {yield_ms} ms window against a {SERIAL_ANSWER_BUDGET_MS} ms answer budget"
    );
    // The margin is not thin: it is 11x. A window the loop does not wake from
    // does not "sometimes" miss the answer, it misses it always.
    assert!(yield_ms / SERIAL_ANSWER_BUDGET_MS >= 10);
}

/// The mechanism the fix rests on: the arm a window races is
/// `Receiver::ready_to_receive`, which completes when a config is queued and
/// leaves it in the channel.
///
/// Both halves matter. Pending-while-empty is what keeps an idle window from
/// being cut short; not-consuming is what lets the loop keep a single intake
/// at the top of its turn, so waking a window and reprogramming the radio
/// stay separate concerns and the derived loop state has one place it is
/// rewritten from.
#[test]
fn the_config_wake_does_not_consume_the_config() {
    // The firmware's channel, verbatim: depth 1, critical-section mutex.
    let channel: Channel<CriticalSectionRawMutex, RadioConfigWire, 1> = Channel::new();
    let rx = channel.receiver();
    let mut cx = Context::from_waker(core::task::Waker::noop());

    let mut ready = pin!(rx.ready_to_receive());
    for _ in 0..64 {
        assert!(
            matches!(ready.as_mut().poll(&mut cx), Poll::Pending),
            "an empty config channel completed the arm, which would cut every \
             idle RX window short"
        );
    }

    let cfg = pushed_config();
    channel.sender().try_send(cfg).expect("empty channel");
    assert!(
        matches!(ready.as_mut().poll(&mut cx), Poll::Ready(())),
        "a queued config did not complete the arm the RX window races"
    );

    // Still there: the wake reports, the turn's intake applies.
    assert_eq!(
        rx.try_receive().expect("the wake must not consume"),
        cfg,
        "the config was taken out of the channel by the wake itself"
    );
}

/// The loop's own half, as a source invariant: every window it arms wakes on
/// a config, not just the one f4ecf16ab covered.
///
/// This is the assertion that was red before the fix. It is a source check
/// because the subject is `lora_task`, an embassy task on thumbv7em that no
/// host test can drive — and because the failure mode is precisely a window
/// that *nobody enumerated*. Counting them is the point: a new window added
/// without the config arm fails here by arithmetic, without anybody having to
/// notice it.
#[test]
fn every_rx_window_the_lora_loop_arms_wakes_on_a_config() {
    let src = nrf_source("lora.rs");

    // One window function. `rx_once` was the bare one — a window with a
    // timeout and no second exit — and its absence is what makes the count
    // below exhaustive rather than a sample.
    assert!(
        !src.contains("async fn rx_once("),
        "leviculum-nrf/src/lora.rs still has a window function without a \
         config arm; every window must go through the one that has it"
    );
    let def = src
        .find("async fn rx_window(")
        .expect("lora.rs has no rx_window: the loop's single window function");
    let body_end = src[def..].find("\n}\n").expect("unterminated rx_window") + def;
    let body = &src[def..body_end];
    assert!(
        body.contains("config_rx.ready_to_receive()"),
        "rx_window does not race the config channel, so no window wakes on a \
         config"
    );
    assert!(
        body.contains("RxTeardownBy::Config"),
        "rx_window leaves the receiver armed when it wakes on a config"
    );
    assert!(
        body.contains("if !config_rx.is_empty()"),
        "rx_window arms a window even when a config is already waiting, which \
         is what would let one push cost more than one mid-air deferral"
    );

    // Every site the loop names, and every one of them going through that
    // function. The sites are `leviculum_core::sx126x::RxSite` variants, the
    // tags a capture shows in `[SX_RX_ARM] site=`.
    let sites: Vec<&str> = ["Idle", "Ack", "Csma", "Jitter", "Hold", "Yield"].into();
    let armed: Vec<&str> = sites
        .iter()
        .copied()
        .filter(|s| src.contains(&format!("leviculum_core::sx126x::RxSite::{s}")))
        .collect();
    assert_eq!(
        armed, sites,
        "the loop's set of RX sites changed; update this test and the site \
         table in the fix's commit message"
    );
    // Occurrences of the call, i.e. every mention of the name followed by an
    // open paren, less the definition itself. Counted on the token rather
    // than on a formatted argument list so `cargo fmt` cannot change the
    // answer.
    let calls = src.matches("rx_window(").count() - 1;
    assert_eq!(
        calls,
        sites.len(),
        "{} RX sites but {calls} calls to rx_window: a window is being armed \
         somewhere else, and that is the window a config will sleep through",
        sites.len()
    );
}

/// The serial task's half: a repeat of the config that is already in the slot
/// is a delivery that already happened, not one that failed.
///
/// The channel property this rests on is worth pinning on the real type: a
/// full depth-1 channel holds exactly the last value that went in. With one
/// producer that is what makes `lora::pending_config` — a record written
/// beside each send — able to name what is in the slot without peeking.
#[test]
fn a_repeat_of_the_queued_config_is_not_undeliverable() {
    let channel: Channel<CriticalSectionRawMutex, RadioConfigWire, 1> = Channel::new();
    let tx = channel.sender();
    let cfg = pushed_config();

    // Attempt 1, 229452 in the capture: taken.
    tx.try_send(cfg).expect("empty channel");
    // Attempt 2, 231454: the LoRa task has not consumed it (it is asleep in
    // the yield window), so the slot is full...
    assert!(tx.try_send(cfg).is_err(), "a depth-1 channel took a second");
    // ...and what it is full OF is attempt 1's config — the very config the
    // host is asking for. Nothing is undeliverable here.
    assert_eq!(
        channel.try_peek().expect("full channel"),
        cfg,
        "a full depth-1 channel does not hold the last value sent, which is \
         what lora::pending_config assumes"
    );

    // And the serial task asks that question before it refuses.
    let usb = nrf_source("usb.rs");
    let full_arm = usb
        .find("Err(embassy_sync::channel::TrySendError::Full(cfg))")
        .expect("apply_radio_config no longer has a full-channel branch");
    let refusal = usb[full_arm..]
        .find("ConfigDelivery::Undeliverable")
        .expect("the full-channel branch no longer refuses at all");
    let branch = &usb[full_arm..full_arm + refusal];
    assert!(
        branch.contains("crate::lora::pending_config() == Some(wire)"),
        "apply_radio_config refuses a full slot without asking whether the \
         slot holds this very config; the host's retry of its own config is \
         then answered undeliverable, which is how one slow apply became \
         no_ack_after_3"
    );
}
