use super::*;

const RECIPIENT: [u8; 16] = [0xAA; 16];
const OTHER: [u8; 16] = [0xBB; 16];

fn stored(destination: [u8; 16], sequence: u64, tag: u8) -> StoredMessage {
    StoredMessage {
        transient_id: [tag; 32],
        destination_hash: destination,
        size: 200,
        received_at: 0,
        stamp_value: 13,
        sequence,
    }
}

fn pick(entries: &[StoredMessage], excluded: &[TransientId]) -> Option<StoredMessage> {
    let mut candidate = ActiveCandidate::new(RECIPIENT, excluded);
    for meta in entries {
        candidate.consider(meta);
    }
    candidate.best()
}

/// The board store `lora_pn_board_sync` meets: it survives every reset, so
/// mail an earlier run left for other identities sits beside this run's
/// message, and the scan is in record-log order, not sequence order.
#[test]
fn the_oldest_message_for_the_destination_is_pushed_and_no_other_recipients() {
    let entries = [
        stored(OTHER, 1, 1),
        stored(RECIPIENT, 7, 7),
        stored(OTHER, 2, 2),
        stored(RECIPIENT, 5, 5),
        stored(RECIPIENT, 9, 9),
    ];
    assert_eq!(pick(&entries, &[]).map(|meta| meta.sequence), Some(5));
}

/// Nothing addressed to the destination: no push.
#[test]
fn nothing_stored_for_the_destination_pushes_nothing() {
    assert_eq!(pick(&[stored(OTHER, 1, 1), stored(OTHER, 2, 2)], &[]), None);
    assert_eq!(pick(&[], &[]), None);
}

/// A message the core refused as too large for one packet is left for
/// `/get`: the pick steps to the next-oldest, and to none when that was all.
#[test]
fn a_message_known_too_large_is_skipped_for_the_next_oldest() {
    let entries = [stored(RECIPIENT, 5, 5), stored(RECIPIENT, 7, 7)];
    assert_eq!(
        pick(&entries, &[[5; 32]]).map(|meta| meta.sequence),
        Some(7)
    );
    assert_eq!(pick(&entries, &[[5; 32], [7; 32]]), None);
}

/// The retry budget, every branch: the board pushes with three attempts
/// (`ACTIVE_ATTEMPTS` in the engine), resends with two and then one, and
/// gives up on the third failure. A budget already at zero gives up too.
#[test]
fn a_failed_push_resends_until_the_last_attempt_and_then_gives_up() {
    assert_eq!(
        after_failed_push(3),
        AfterFailedPush::Resend { attempts_left: 2 }
    );
    assert_eq!(
        after_failed_push(2),
        AfterFailedPush::Resend { attempts_left: 1 }
    );
    assert_eq!(after_failed_push(1), AfterFailedPush::GiveUp);
    assert_eq!(after_failed_push(0), AfterFailedPush::GiveUp);
}

/// The core refusing a push as too large for one packet leaves the message
/// for `/get`; every other refusal (no path, pacing, a busy send path, a
/// missing identity) waits the gap and retries.
#[test]
fn a_push_too_large_is_left_for_get_and_any_other_refusal_waits_the_gap() {
    assert_eq!(
        after_refused_push(&SendError::TooLarge),
        AfterRefusedPush::LeaveForGet
    );
    for error in [
        SendError::NoPath,
        SendError::NoLink,
        SendError::LinkFailed,
        SendError::Busy,
        SendError::PacingDelay { ready_at_ms: 5 },
        SendError::EncryptionFailed,
    ] {
        assert_eq!(after_refused_push(&error), AfterRefusedPush::WaitGap);
    }
}

/// The too-large list holds an id once, and past its cap the oldest goes:
/// the pick may name it again, the core refuses it again, and it rejoins.
#[test]
fn the_too_large_list_holds_an_id_once_and_drops_the_oldest_past_its_cap() {
    let mut list = Vec::new();
    remember_too_large(&mut list, [1; 32]);
    remember_too_large(&mut list, [1; 32]);
    assert_eq!(list, alloc::vec![[1; 32]]);

    for n in 2..=TOO_LARGE_CAP as u8 {
        remember_too_large(&mut list, [n; 32]);
    }
    assert_eq!(list.len(), TOO_LARGE_CAP);
    assert_eq!(list[0], [1; 32]);

    remember_too_large(&mut list, [0xFF; 32]);
    assert_eq!(list.len(), TOO_LARGE_CAP);
    assert_eq!(list[0], [2; 32], "the oldest id dropped out");
    assert_eq!(list[TOO_LARGE_CAP - 1], [0xFF; 32]);
}
