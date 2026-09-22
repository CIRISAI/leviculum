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
