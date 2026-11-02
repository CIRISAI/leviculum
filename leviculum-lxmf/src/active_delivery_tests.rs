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

fn solicited(entries: &[StoredMessage], excluded: &[TransientId]) -> Vec<[u8; 16]> {
    let mut pick = PeerUpSolicit::new(excluded);
    for meta in entries {
        pick.consider(meta);
    }
    pick.destinations()
}

/// Every destination with pushable mail is asked about once, the one whose
/// mail has waited longest first.
#[test]
fn a_peer_up_asks_each_stored_destination_once_oldest_first() {
    let entries = [
        stored(OTHER, 4, 4),
        stored(RECIPIENT, 7, 7),
        stored(OTHER, 9, 9),
        stored(RECIPIENT, 2, 2),
    ];
    assert_eq!(solicited(&entries, &[]), alloc::vec![RECIPIENT, OTHER]);
}

/// A destination whose only mail is too large for a push has nothing a
/// path would trigger, so the peer is not asked about it.
#[test]
fn a_destination_with_only_too_large_mail_is_not_asked_about() {
    let entries = [stored(RECIPIENT, 1, 1), stored(OTHER, 2, 2)];
    assert_eq!(solicited(&entries, &[[1; 32]]), alloc::vec![OTHER]);
}

/// A full store asks about at most the cap, and the destinations whose mail
/// has waited longest are the ones kept, whatever the scan order.
#[test]
fn a_full_store_asks_about_the_longest_waiting_destinations_up_to_the_cap() {
    let entries: Vec<StoredMessage> = (0..(SOLICIT_DESTINATIONS_CAP as u8 + 4))
        .rev()
        .map(|n| stored([n; 16], u64::from(n), n))
        .collect();
    let asked = solicited(&entries, &[]);
    let expected: Vec<[u8; 16]> = (0..SOLICIT_DESTINATIONS_CAP as u8)
        .map(|n| [n; 16])
        .collect();
    assert_eq!(asked, expected);
}

/// The hand-written sort in `destinations` gives the order the standard
/// stable sort gives, for every length up to the cap and with ties in the
/// sequences (two destinations whose oldest mail arrived together keep the
/// order the scan offered them in).
#[test]
fn destinations_come_out_in_the_order_a_stable_sort_by_sequence_gives() {
    let mut seed: u32 = 0x1300;
    for _ in 0..2000 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let len = (seed >> 8) as usize % (SOLICIT_DESTINATIONS_CAP + 1);
        let picks: Vec<([u8; 16], u64)> = (0..len)
            .map(|i| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ([i as u8; 16], u64::from((seed >> 16) % 4))
            })
            .collect();
        let mut expected = picks.clone();
        expected.sort_by_key(|(_, sequence)| *sequence);
        let expected: Vec<[u8; 16]> = expected.into_iter().map(|(d, _)| d).collect();
        let solicit = PeerUpSolicit {
            excluded: &[],
            picks: picks.clone(),
        };
        assert_eq!(solicit.destinations(), expected, "picks {picks:?}");
    }
}

/// One pair is asked once per interval; another peer or another
/// destination is its own pair.
#[test]
fn the_pacer_asks_each_peer_and_destination_pair_once_per_interval() {
    let phone = [0xC4; 16];
    let other_phone = [0x5A; 16];
    let mut pacer = SolicitPacer::new();
    assert!(pacer.due(0, phone, RECIPIENT));
    assert!(!pacer.due(SOLICIT_INTERVAL_MS - 1, phone, RECIPIENT));
    assert!(pacer.due(1, other_phone, RECIPIENT));
    assert!(pacer.due(1, phone, OTHER));
    assert!(pacer.due(SOLICIT_INTERVAL_MS, phone, RECIPIENT));
}

/// The marks stay bounded: expired ones go on every call, and a burst of
/// distinct peers inside one interval evicts the oldest at the cap.
#[test]
fn the_pacer_holds_at_most_its_cap() {
    let mut pacer = SolicitPacer::new();
    for n in 0..(SOLICIT_PACER_CAP as u8 + 5) {
        assert!(pacer.due(u64::from(n), [n; 16], RECIPIENT));
    }
    assert_eq!(pacer.marks().len(), SOLICIT_PACER_CAP);
    assert!(pacer.due(SOLICIT_INTERVAL_MS + 100, [0xEE; 16], RECIPIENT));
    assert_eq!(pacer.marks().len(), 1);
}
