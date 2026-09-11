//! Candidate (b): `leviculum-record-log`, ported to
//! `embedded-storage-async`.
//!
//! The same six criteria as `tests/sequential_storage.rs`, numbered the same
//! way, over the same `SimNor`. The crate's own suite
//! (`cargo test -p leviculum-record-log`) proves more than this — the
//! power-cut sweep there covers seven body lengths and the reclaim — but
//! this file is what makes the two candidates comparable line for line.

use std::future::Future;

use leviculum_store_spike::{
    assert_contiguous_suffix, block_on, body, ids, noise, wear_spread, SimNor, Yielding, PAGE,
    PAGES, REGION,
};

use leviculum_record_log::{
    is_formatted, Error, RecordLog, FLAG_PURGED, KEY_LEN, MAX_BODY, SECTOR_HEADER_LEN,
};

fn key(n: u32) -> [u8; KEY_LEN] {
    let mut k = [0u8; KEY_LEN];
    k[0..4].copy_from_slice(&n.to_le_bytes());
    k
}

async fn open(flash: SimNor, pages: u32) -> RecordLog<SimNor> {
    RecordLog::open(flash, 0, pages * PAGE).await.unwrap()
}

/// Every live record's body, oldest first.
async fn view(log: &mut RecordLog<SimNor>) -> Vec<Vec<u8>> {
    let mut headers = Vec::new();
    log.for_each(|r| {
        if r.is_live() {
            headers.push(*r)
        }
    })
    .await
    .unwrap();
    let mut out = Vec::new();
    for header in &headers {
        let mut buf = vec![0u8; header.len as usize];
        log.read_body(header, &mut buf).await.unwrap();
        out.push(buf);
    }
    out
}

// ------------------------------------------------------------ criterion 5

#[test]
fn c5_append_iterate_remove_and_drop_the_oldest() {
    block_on(async {
        let mut log = open(SimNor::new(PAGES), PAGES).await;
        for i in 0..5u32 {
            log.append(&key(i), i, 0, &body(i, 64)).await.unwrap();
        }
        assert_eq!(
            view(&mut log).await,
            (0..5).map(|i| body(i, 64)).collect::<Vec<_>>(),
            "iteration is oldest to newest"
        );

        // Remove one arbitrary entry: the third, in the middle of the run.
        // Note the difference from a queue's `pop`: the record keeps its
        // bytes and its place until its page is reclaimed, and what changes
        // is one bit of its flags byte. It leaves the live view; it does not
        // leave the part.
        let mut all = Vec::new();
        log.for_each(|r| all.push(*r)).await.unwrap();
        log.purge(&all[2]).await.unwrap();
        assert_eq!(
            view(&mut log).await,
            vec![body(0, 64), body(1, 64), body(3, 64), body(4, 64)],
            "the removed entry is gone from the live view and no other moved"
        );
        let mut after = Vec::new();
        log.for_each(|r| after.push(*r)).await.unwrap();
        assert_eq!(after.len(), 5, "and it is still on the part");
        assert_eq!(after[2].flags, FLAG_PURGED);
        assert_eq!(
            after[2].offset, all[2].offset,
            "a purge is a bit, not a move"
        );

        // Drop the oldest on overflow: append past the region and check the
        // survivors are the newest run, in order.
        let flash = log.into_flash();
        let mut log = open(flash, PAGES).await;
        let entries = 400u32;
        for i in 100..100 + entries {
            log.append(&key(i), i, 0, &body(i, 300)).await.unwrap();
        }
        let survivors = view(&mut log).await;
        assert!(
            survivors.len() < entries as usize,
            "the region is smaller than the workload, so entries must have been dropped"
        );
        let first = 100 + entries - survivors.len() as u32;
        for (n, entry) in survivors.iter().enumerate() {
            assert_eq!(
                *entry,
                body(first + n as u32, 300),
                "survivors are the newest run"
            );
        }
    });
}

// ------------------------------------------------------------ criterion 6

#[test]
fn c6_largest_entry_on_one_page() {
    block_on(async {
        assert_eq!(
            MAX_BODY, 4042,
            "4096 byte page, a 12-byte page header, a 42-byte record header"
        );
        let mut log = open(SimNor::new(PAGES), PAGES).await;
        assert!(matches!(
            log.append(&key(0), 0, 0, &body(0, MAX_BODY + 1)).await,
            Err(Error::BodyTooLarge)
        ));
        let largest = body(1, MAX_BODY);
        log.append(&key(1), 1, 0, &largest).await.unwrap();
        assert_eq!(view(&mut log).await, vec![largest]);
    });
}

// ------------------------------------------------------------ criterion 1

#[test]
fn c1_power_cut_at_every_word_of_an_append() {
    block_on(async {
        let body_len = 304usize;
        let pre = 3u32;
        let cost = {
            let mut log = open(SimNor::new(PAGES), PAGES).await;
            for i in 0..pre {
                log.append(&key(i), i, 0, &body(i, body_len)).await.unwrap();
            }
            let before = log.flash_mut().spent();
            log.append(&key(pre), pre, 0, &body(pre, body_len))
                .await
                .unwrap();
            log.flash_mut().spent() - before
        };
        assert_eq!(cost, 87, "one word per word of the 348-byte record");

        let mut cuts = 0usize;
        for cut in 0..=cost {
            let mut log = open(SimNor::new(PAGES), PAGES).await;
            for i in 0..pre {
                log.append(&key(i), i, 0, &body(i, body_len)).await.unwrap();
            }
            log.flash_mut().arm_power_cut(cut);
            let landed = log
                .append(&key(pre), pre, 0, &body(pre, body_len))
                .await
                .is_ok();

            let mut flash = log.into_flash();
            flash.power_on();
            let mut log = open(flash, PAGES).await;
            let got = view(&mut log).await;
            let expected = if landed { pre + 1 } else { pre };
            assert_eq!(
                got.len() as u32,
                expected,
                "cut={cut} of {cost}: every completed entry and no partial one"
            );
            for (n, entry) in got.iter().enumerate() {
                assert_eq!(*entry, body(n as u32, body_len), "cut={cut}");
            }
            log.append(&key(999), 999, 0, &body(999, body_len))
                .await
                .unwrap_or_else(|e| panic!("cut={cut}: the next append must land: {e:?}"));
            assert_eq!(view(&mut log).await.len() as u32, expected + 1, "cut={cut}");
            cuts += 1;
        }
        assert_eq!(cuts, cost + 1);
    });
}

#[test]
fn c1_power_cut_at_every_word_of_an_append_that_changes_page() {
    block_on(async {
        let pages = 3u32;
        let body_len = 1000usize;
        // Which append is the one that reclaims the next page, measured.
        let pre = {
            let mut log = open(SimNor::new(pages), pages).await;
            let mut n = 0u32;
            loop {
                log.append(&key(n), n, 0, &body(n, body_len)).await.unwrap();
                n += 1;
                if log.flash_mut().erase_counts()[1] > 0 {
                    break n - 1;
                }
                assert!(n < 100, "the region never rolled onto page 1");
            }
        };
        assert_eq!(pre, 3, "three 1000-byte records fill a 4 KiB page");

        let cost = {
            let mut log = open(SimNor::new(pages), pages).await;
            for i in 0..pre {
                log.append(&key(i), i, 0, &body(i, body_len)).await.unwrap();
            }
            let before = log.flash_mut().spent();
            log.append(&key(pre), pre, 0, &body(pre, body_len))
                .await
                .unwrap();
            log.flash_mut().spent() - before
        };
        assert_eq!(
            cost, 1288,
            "a 4096-byte erase (1024 words), the 12-byte page header and the \
             261-word record"
        );

        for cut in 0..=cost {
            let mut log = open(SimNor::new(pages), pages).await;
            for i in 0..pre {
                log.append(&key(i), i, 0, &body(i, body_len)).await.unwrap();
            }
            log.flash_mut().arm_power_cut(cut);
            let landed = log
                .append(&key(pre), pre, 0, &body(pre, body_len))
                .await
                .is_ok();

            let mut flash = log.into_flash();
            flash.power_on();
            let mut log = open(flash, pages).await;
            let got = view(&mut log).await;
            let ids: Vec<u32> = got
                .iter()
                .map(|entry| {
                    (0..=pre)
                        .find(|n| body(*n, body_len) == *entry)
                        .unwrap_or_else(|| panic!("cut={cut}: an entry nobody appended"))
                })
                .collect();
            assert!(!ids.is_empty(), "cut={cut}");
            assert!(
                ids.windows(2).all(|w| w[1] == w[0] + 1),
                "cut={cut}: the survivors must be contiguous: {ids:?}"
            );
            assert_eq!(
                *ids.last().unwrap(),
                if landed { pre } else { pre - 1 },
                "cut={cut} of {cost}: every completed entry and no partial one"
            );
            log.append(&key(999), 999, 0, &body(999, body_len))
                .await
                .unwrap_or_else(|e| panic!("cut={cut}: the next append must land: {e:?}"));
        }
    });
}

// ------------------------------------------------------------ criterion 2

#[test]
fn c2_a_failed_operation_at_every_step_of_an_append() {
    // The SoftDevice fails a flash operation with a timeout when it finds no
    // gap between radio events: an operation that did not happen, not a
    // partial one. Fail each operation of an append in turn and require the
    // same three things every time: the call reports the error, what is left
    // is consistent, and a retry lands.
    //
    // The swept append is the one that has to reclaim a page holding
    // records, so the sweep covers an erase that drops entries as well as
    // the writes. The retry carries a *different* record on purpose: a retry
    // of the identical bytes would be programming the same values over
    // themselves and would pass even without a sealing rule.
    block_on(async {
        let pages = 3u32;
        let body_len = 1000usize;
        let pre = {
            let mut log = open(SimNor::new(pages), pages).await;
            let mut n = 0u32;
            loop {
                log.append(&key(n), n, 0, &body(n, body_len)).await.unwrap();
                n += 1;
                if log.flash_mut().erase_counts().iter().any(|c| *c > 1) {
                    break n - 1;
                }
                assert!(
                    n < 100,
                    "the region never reclaimed a page with records in it"
                );
            }
        };
        assert_eq!(pre, 9, "three 1000-byte records to a page, three pages");

        let ops = {
            let mut log = open(SimNor::new(pages), pages).await;
            for i in 0..pre {
                log.append(&key(i), i, 0, &body(i, body_len)).await.unwrap();
            }
            let before = log.flash_mut().ops();
            log.append(&key(pre), pre, 0, &body(pre, body_len))
                .await
                .unwrap();
            log.flash_mut().ops() - before
        };
        assert_eq!(
            ops, 20,
            "erase, page header, header run, sixteen body windows, commit"
        );

        let mut failures = 0usize;
        let mut lost = 0usize;
        let mut sealed = 0usize;
        for nth in 0..ops {
            let mut log = open(SimNor::new(pages), pages).await;
            for i in 0..pre {
                log.append(&key(i), i, 0, &body(i, body_len)).await.unwrap();
            }
            let before = ids(&view(&mut log).await, pre, body_len);

            log.flash_mut().fail_op_after(nth);
            let outcome = log.append(&key(pre), pre, 0, &body(pre, body_len)).await;
            if outcome.is_err() {
                failures += 1;
                assert!(!log.flash_mut().failure_armed(), "nth={nth}");
                if log.sector_room() == 0 {
                    sealed += 1;
                }
                // The reclaim drops records from the oldest end by design;
                // what a failure may not do is invent one, reorder them,
                // leave a gap, or lose one from the newest end.
                let after = ids(&view(&mut log).await, pre, body_len);
                lost += assert_contiguous_suffix(&before, &after, pre - 1, &format!("nth={nth}"));
                // A retry with different bytes than the append that failed.
                log.append(&key(pre + 100), pre + 100, 0, &body(pre + 100, body_len))
                    .await
                    .unwrap_or_else(|e| panic!("nth={nth}: the retry must land: {e:?}"));
            }

            let flash = log.into_flash();
            let mut log = open(flash, pages).await;
            let got = view(&mut log).await;
            assert!(!got.is_empty(), "nth={nth}");
            assert!(
                got.last() == Some(&body(pre, body_len))
                    || got.last() == Some(&body(pre + 100, body_len)),
                "nth={nth}: the newest entry is the one that landed"
            );
            assert!(log.flash_mut().max_word_writes() <= 2, "nth={nth}");
        }
        assert_eq!(failures, ops, "every operation of this append can fail");
        // What the sealing rule costs, as numbers rather than promises. Of
        // the twenty places a timeout can land, three leave the page usable
        // (the reclaim's erase, its page header, and the first program run,
        // none of which has put a byte of the record down) and seventeen
        // give up the rest of the page.
        assert_eq!(sealed, 17);
        assert_eq!(
            lost, 57,
            "the reclaim's erase drops the three records of the page it took, \
             on each of the nineteen sweeps that get past the erase itself"
        );
    });
}

// ------------------------------------------------------------ criterion 3

#[test]
fn c3_wear_is_level_after_wrapping_the_region_twice() {
    block_on(async {
        let mut log = open(SimNor::new(PAGES), PAGES).await;
        let mut entries = 0u32;
        while log
            .flash_mut()
            .erase_counts()
            .iter()
            .copied()
            .min()
            .unwrap()
            < 2
        {
            log.append(&key(entries), entries, 0, &body(entries, 304))
                .await
                .unwrap();
            entries += 1;
            assert!(entries < 10_000, "the region never wrapped twice");
        }
        assert_eq!(entries, 166, "304-byte entries it takes to wrap twice");
        let flash = log.into_flash();
        let counts = flash.erase_counts();
        assert!(
            counts.iter().copied().min().unwrap() >= 2,
            "the region must have been wrapped at least twice: {counts:?}"
        );
        assert!(
            wear_spread(counts) <= 1,
            "per-page erase counts must differ by at most one: {counts:?}"
        );
    });
}

#[test]
fn c3_negative_control_a_pinned_page_fails_the_wear_check() {
    block_on(async {
        use embedded_storage_async::nor_flash::NorFlash;
        let pages = PAGES + 1;
        let mut log = RecordLog::open(SimNor::new(pages), PAGE, PAGES * PAGE)
            .await
            .unwrap();
        let entries = 166u32;
        for i in 0..entries {
            log.append(&key(i), i, 0, &body(i, 304)).await.unwrap();
            log.flash_mut().erase(0, PAGE).await.unwrap();
        }
        let flash = log.into_flash();
        let counts = flash.erase_counts();
        assert_eq!(counts[0], entries, "the pinned page takes one erase each");
        assert!(
            wear_spread(counts) > 1,
            "the wear check has to fail here: {counts:?}"
        );
        assert!(wear_spread(&counts[1..]) <= 1, "{counts:?}");
        assert!(
            counts[0] / counts[1] > 50,
            "the ratio is the argument: {counts:?}"
        );
    });
}

// ------------------------------------------------------------ criterion 4

/// What a store made of a region it did not write. Same three outcomes the
/// sequential-storage file distinguishes.
#[derive(Debug, PartialEq, Eq)]
enum Foreign {
    ReadAsEmpty,
    Formatted,
    SurfacedAsEntries,
}

async fn foreign_region(content: &[u8]) -> Foreign {
    let mut flash = SimNor::new(PAGES);
    flash.plant(0, content);
    // A runaway scan would spin rather than fail; cap the flash operations
    // so an endless loop shows up as an error instead of a hang.
    flash.fail_op_after(10_000);

    let formatted_before = is_formatted(&mut flash, 0, REGION).await.unwrap();
    let mut log = open(flash, PAGES).await;
    let verdict = match view(&mut log).await {
        entries if entries.is_empty() && !formatted_before => Foreign::ReadAsEmpty,
        entries if entries.is_empty() => Foreign::Formatted,
        _ => Foreign::SurfacedAsEntries,
    };

    for i in 0..20u32 {
        log.append(&key(i), i, 0, &body(i, 304))
            .await
            .unwrap_or_else(|e| panic!("after recovery the store must take entries: {e:?}"));
    }
    let flash = log.into_flash();
    let mut log = open(flash, PAGES).await;
    let got = view(&mut log).await;
    assert!(got.len() >= 20, "{} entries survived", got.len());
    assert_eq!(
        got[got.len() - 20..],
        (0..20).map(|i| body(i, 304)).collect::<Vec<_>>()[..]
    );
    verdict
}

#[test]
fn c4_random_bytes_in_the_region() {
    block_on(async {
        assert_eq!(
            foreign_region(&noise(0x5EED, REGION as usize)).await,
            Foreign::ReadAsEmpty,
            "random bytes must not surface as entries"
        );
    });
}

#[test]
fn c4_a_region_of_zeros() {
    block_on(async {
        assert_eq!(
            foreign_region(&vec![0u8; REGION as usize]).await,
            Foreign::ReadAsEmpty
        );
    });
}

#[test]
fn c4_a_page_of_plausible_headers() {
    // The store's own shape with the wrong bytes after it: a page header it
    // would have written, claiming the highest sequence there is, and
    // garbage where the records should be.
    block_on(async {
        let mut content = noise(0xC0FFEE, REGION as usize);
        let page = 3 * PAGE as usize;
        let mut header = [0u8; SECTOR_HEADER_LEN as usize];
        header[0..4].copy_from_slice(b"LVR1");
        header[4..8].copy_from_slice(&0xFFFF_FF00u32.to_le_bytes());
        header[8] = 1;
        header[9] = 0;
        let crc = leviculum_record_log::crc16_update(0xFFFF, &header[0..10]);
        header[10..12].copy_from_slice(&crc.to_le_bytes());
        content[page..page + header.len()].copy_from_slice(&header);
        assert_eq!(foreign_region(&content).await, Foreign::Formatted);
    });
}

// -------------------------------------------- the part's own word budget

#[test]
fn word_writes_between_erases() {
    block_on(async {
        // The workload that maximises it: bodyless records, the smallest
        // stride the format allows, each committed and then withdrawn.
        let mut log = open(SimNor::new(PAGES), PAGES).await;
        let mut appended = 0u32;
        while log.sector_room() >= 44 {
            log.append(&key(appended), appended, 0, &[]).await.unwrap();
            appended += 1;
        }
        let mut all = Vec::new();
        log.for_each(|r| all.push(*r)).await.unwrap();
        for record in &all {
            log.purge(record).await.unwrap();
        }
        let flash = log.into_flash();
        assert_eq!(
            flash.max_word_writes(),
            2,
            "a word takes at most the two writes the nRF52840 allows"
        );
    });
}

// ------------------------------------------------------------ cancellation

#[test]
fn cancellation_needs_the_store_to_own_the_future() {
    block_on(async {
        let pages = 4u32;
        for stop in 1..=6usize {
            let mut log = RecordLog::open(Yielding::new(SimNor::new(pages)), 0, pages * PAGE)
                .await
                .unwrap();
            for i in 0..2u32 {
                log.append(&key(i), i, 0, &body(i, 200)).await.unwrap();
            }
            {
                let k = key(2);
                let b = body(2, 200);
                let mut fut = core::pin::pin!(log.append(&k, 2, 0, &b));
                let mut cx = core::task::Context::from_waker(core::task::Waker::noop());
                let mut finished = false;
                for _ in 0..stop {
                    if fut.as_mut().poll(&mut cx).is_ready() {
                        finished = true;
                        break;
                    }
                }
                assert!(
                    !finished,
                    "stop={stop}: the append finished before the drop"
                );
            }

            let flash = log.into_flash().into_inner();
            let mut log = open(flash, pages).await;
            let got = view(&mut log).await;
            assert_eq!(got.len(), 2, "stop={stop}: the cancelled record is absent");
            assert_eq!(got[0], body(0, 200), "stop={stop}");
            assert_eq!(got[1], body(1, 200), "stop={stop}");
            log.append(&key(3), 3, 0, &body(3, 200))
                .await
                .unwrap_or_else(|e| panic!("stop={stop}: the next append must land: {e:?}"));
        }
    });
}
