//! Candidate (a): `sequential-storage` 8.0.1, its queue.
//!
//! Six criteria, numbered as in the #384 brief, over
//! `leviculum_record_log::sim::SimNor` — the same device candidate (b) runs
//! on. What each test asserts is what the brief asks for; where the
//! candidate cannot meet one, the test says so in its name and its message
//! rather than being relaxed.
//!
//! Cancellation: the crate's own README warns that a cancelled future can
//! leave data needing repair. Criterion 1's sweep is the power-cut half of
//! that; `cancellation_needs_the_store_to_own_the_future` is the other half,
//! and says what a store task has to do about it.

use std::future::Future;

use leviculum_store_spike::{
    assert_contiguous_suffix, block_on, body, ids, noise, wear_spread, SimNor, Yielding, PAGE,
    PAGES, REGION,
};
use sequential_storage::cache::Cache;
use sequential_storage::queue::{QueueConfig, QueueStorage};
use sequential_storage::Error as SsError;

type Queue = QueueStorage<
    SimNor,
    Cache<
        sequential_storage::cache::Uncached,
        sequential_storage::cache::Uncached,
        sequential_storage::cache::Uncached,
        (),
    >,
>;

fn open(flash: SimNor, pages: u32) -> Queue {
    QueueStorage::new(
        flash,
        QueueConfig::new(0..pages * PAGE),
        Cache::new_uncached(),
    )
}

/// Every entry in the queue, oldest first.
async fn drain_view(
    queue: &mut Queue,
) -> Result<Vec<Vec<u8>>, SsError<<SimNor as embedded_storage_async::nor_flash::ErrorType>::Error>>
{
    let mut buf = [0u8; 4096];
    let mut out = Vec::new();
    let mut iter = queue.iter().await?;
    while let Some(entry) = iter.next(&mut buf).await? {
        out.push(entry.into_buf().to_vec());
    }
    Ok(out)
}

// ------------------------------------------------------------ criterion 5
// Append, iterate oldest to newest, remove one arbitrary entry, drop the
// oldest on overflow. Taken first because the rest lean on it working.

#[test]
fn c5_append_iterate_remove_and_drop_the_oldest() {
    block_on(async {
        let mut queue = open(SimNor::new(PAGES), PAGES);
        for i in 0..5u32 {
            queue.push(&body(i, 64), false).await.unwrap();
        }
        assert_eq!(
            drain_view(&mut queue).await.unwrap(),
            (0..5).map(|i| body(i, 64)).collect::<Vec<_>>(),
            "iteration is oldest to newest"
        );

        // Remove one arbitrary entry: the third, in the middle of the run.
        let mut buf = [0u8; 4096];
        {
            let mut iter = queue.iter().await.unwrap();
            let mut seen = 0;
            while let Some(entry) = iter.next(&mut buf).await.unwrap() {
                if seen == 2 {
                    entry.pop().await.unwrap();
                    break;
                }
                seen += 1;
            }
        }
        assert_eq!(
            drain_view(&mut queue).await.unwrap(),
            vec![body(0, 64), body(1, 64), body(3, 64), body(4, 64)],
            "the removed entry is gone and no other moved"
        );

        // Drop the oldest on overflow: push past the region with
        // `allow_overwrite_old_data`, then check the survivors are the tail
        // of what went in and that nothing errored on the way.
        let (flash, _) = queue.destroy();
        let mut queue = open(flash, PAGES);
        let entries = 400u32;
        for i in 100..100 + entries {
            queue.push(&body(i, 300), true).await.unwrap();
        }
        let view = drain_view(&mut queue).await.unwrap();
        assert!(
            view.len() < entries as usize,
            "the region is smaller than the workload, so entries must have been dropped"
        );
        let first = 100 + entries - view.len() as u32;
        for (n, entry) in view.iter().enumerate() {
            assert_eq!(
                *entry,
                body(first + n as u32, 300),
                "survivors are the newest run, in order"
            );
        }
    });
}

// ------------------------------------------------------------ criterion 6
// Largest entry each accepts on a 4 KiB page, stated as a number.

#[test]
fn c6_largest_entry_on_one_page() {
    block_on(async {
        let mut queue = open(SimNor::new(PAGES), PAGES);
        let fit = queue.find_max_fit().await.unwrap().unwrap();
        assert_eq!(
            fit, 4088,
            "what the crate itself says fits on a fresh 4 KiB page"
        );
        // What it says and what it takes are two questions. The number this
        // spike reports is the one that survives a round trip.
        let largest = (1..=fit)
            .rev()
            .find(|n| {
                block_on(async {
                    let mut probe = open(SimNor::new(PAGES), PAGES);
                    probe.push(&body(1, *n as usize), false).await.is_ok()
                })
            })
            .unwrap();
        assert_eq!(
            largest, 4080,
            "4096 byte page, two 4-byte page-state markers, an 8-byte item header"
        );
        queue.push(&body(1, largest as usize), false).await.unwrap();
        assert_eq!(
            drain_view(&mut queue).await.unwrap(),
            vec![body(1, largest as usize)]
        );

        // And one byte more does not fit on a page — reported as
        // `FullStorage` rather than `ItemTooBig`, because the crate's own
        // fit calculation said it would.
        let (flash, _) = queue.destroy();
        let mut queue = open(flash, PAGES);
        assert!(
            queue
                .push(&body(2, largest as usize + 1), false)
                .await
                .is_err(),
            "one byte past the page must not be accepted"
        );
    });
}

// ------------------------------------------------------------ criterion 1
// Power cut at every word of an append: every completed entry survives, no
// partial one appears.

#[test]
fn c1_power_cut_at_every_word_of_a_push() {
    block_on(async {
        let body_len = 304usize;
        let pre = 3u32;
        // What one push costs the part, measured rather than derived.
        let cost = {
            let mut queue = open(SimNor::new(PAGES), PAGES);
            for i in 0..pre {
                queue.push(&body(i, body_len), false).await.unwrap();
            }
            let before = queue.flash().spent();
            queue.push(&body(pre, body_len), false).await.unwrap();
            queue.flash().spent() - before
        };

        let mut cuts = 0usize;
        for cut in 0..=cost {
            let mut queue = open(SimNor::new(PAGES), PAGES);
            for i in 0..pre {
                queue.push(&body(i, body_len), false).await.unwrap();
            }
            queue.flash().arm_power_cut(cut);
            let landed = queue.push(&body(pre, body_len), false).await.is_ok();

            let (mut flash, _) = queue.destroy();
            flash.power_on();
            // A fresh cache, because a reboot has none.
            let mut queue = open(flash, PAGES);
            let view = drain_view(&mut queue)
                .await
                .unwrap_or_else(|e| panic!("cut={cut}: reopening after the cut failed: {e:?}"));

            let expected = if landed { pre + 1 } else { pre };
            assert_eq!(
                view.len() as u32,
                expected,
                "cut={cut} of {cost}: every completed entry and no partial one"
            );
            for (n, entry) in view.iter().enumerate() {
                assert_eq!(*entry, body(n as u32, body_len), "cut={cut}");
            }
            // A store that reopens read-only after a cut is half a store.
            queue
                .push(&body(999, body_len), false)
                .await
                .unwrap_or_else(|e| panic!("cut={cut}: the next push must land: {e:?}"));
            assert_eq!(
                drain_view(&mut queue).await.unwrap().len() as u32,
                expected + 1,
                "cut={cut}"
            );
            cuts += 1;
        }
        assert_eq!(cuts, cost + 1);
        assert_eq!(
            cost, 78,
            "an 8-byte item header and 304 bytes of data: 78 words, written \
             in that order"
        );
    });
}

#[test]
fn c1_power_cut_at_every_word_of_a_push_that_changes_page() {
    // The sweep above never erases: it pushes into a page that is already
    // open. This one covers the other half — closing a page, erasing the
    // next and opening it — so every word of an erase is a cut point too.
    block_on(async {
        let pages = 3u32;
        let body_len = 1000usize;
        // Which push is the one that opens the next page, measured.
        let pre = {
            let mut queue = open(SimNor::new(pages), pages);
            let mut n = 0u32;
            loop {
                queue.push(&body(n, body_len), true).await.unwrap();
                n += 1;
                if queue.flash().erase_counts()[1] > 0 {
                    break n - 1;
                }
                assert!(n < 100, "the region never rolled onto page 1");
            }
        };
        assert_eq!(
            pre, 16,
            "the seventeenth 1000-byte push is the first that has to reuse a page"
        );
        let cost = {
            let mut queue = open(SimNor::new(pages), pages);
            for i in 0..pre {
                queue.push(&body(i, body_len), true).await.unwrap();
            }
            let before = queue.flash().spent();
            queue.push(&body(pre, body_len), true).await.unwrap();
            assert!(
                queue.flash().erase_counts()[1] > 0,
                "the swept push must be the one that opens the next page"
            );
            queue.flash().spent() - before
        };
        assert_eq!(
            cost, 1278,
            "a page-state marker, a 4096-byte erase (1024 words) and the \
             252-word item"
        );

        for cut in 0..=cost {
            let mut queue = open(SimNor::new(pages), pages);
            for i in 0..pre {
                queue.push(&body(i, body_len), true).await.unwrap();
            }
            queue.flash().arm_power_cut(cut);
            let landed = queue.push(&body(pre, body_len), true).await.is_ok();

            let (mut flash, _) = queue.destroy();
            flash.power_on();
            let mut queue = open(flash, pages);
            let view = drain_view(&mut queue)
                .await
                .unwrap_or_else(|e| panic!("cut={cut}: reopening after the cut failed: {e:?}"));

            // This push overwrites, so entries may be gone from the oldest
            // end — that is what overwriting means. What may not happen is a
            // partial entry, an entry out of order, or a gap. Identify each
            // survivor by which push produced it and require a contiguous
            // run ending where the cut left it.
            let ids: Vec<u32> = view
                .iter()
                .map(|entry| {
                    (0..=pre)
                        .find(|n| body(*n, body_len) == *entry)
                        .unwrap_or_else(|| panic!("cut={cut}: an entry nobody pushed"))
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
            queue
                .push(&body(999, body_len), true)
                .await
                .unwrap_or_else(|e| panic!("cut={cut}: the next push must land: {e:?}"));
        }
    });
}

// -------------------------------------------- the part's own word budget
// Not one of the six, and the one that decides the spike: the nRF52840
// accepts two writes to a word between erases and no more.

#[test]
fn word_writes_between_erases() {
    block_on(async {
        // The workload that maximises it: the smallest entries the format
        // allows, each pushed and then popped, so every word an item
        // occupies gets whatever the erase marking costs on top of the push.
        let mut queue = open(SimNor::new(PAGES), PAGES);
        let mut pushed = 0u32;
        while queue.push(&[pushed as u8], false).await.is_ok() {
            pushed += 1;
            if pushed > 5_000 {
                break;
            }
        }
        let mut buf = [0u8; 64];
        let mut popped = 0u32;
        while queue.pop(&mut buf).await.unwrap().is_some() {
            popped += 1;
        }
        assert_eq!(popped, pushed);
        let flash = queue.destroy().0;
        assert_eq!(
            flash.max_word_writes(),
            2,
            "a word takes at most the two writes the nRF52840 allows"
        );
    });
}

// ------------------------------------------------------------ criterion 2
// A failed erase or write in the middle of an append: the call returns an
// error, the store stays consistent, and a retry succeeds.

#[test]
fn c2_a_failed_operation_at_every_step_of_a_push() {
    block_on(async {
        let pages = 3u32;
        let body_len = 1000usize;
        // Fill enough that the swept push has to close a page and erase the
        // next, so the sweep covers an erase as well as the writes. Which
        // push that is, measured.
        let pre = {
            let mut queue = open(SimNor::new(pages), pages);
            let mut n = 0u32;
            loop {
                queue.push(&body(n, body_len), true).await.unwrap();
                n += 1;
                if queue.flash().erase_counts()[1] > 0 {
                    break n - 1;
                }
                assert!(n < 100, "the region never rolled onto page 1");
            }
        };
        let ops = {
            let mut queue = open(SimNor::new(pages), pages);
            for i in 0..pre {
                queue.push(&body(i, body_len), true).await.unwrap();
            }
            let before = queue.flash().ops();
            queue.push(&body(pre, body_len), true).await.unwrap();
            queue.flash().ops() - before
        };
        assert_eq!(
            ops, 5,
            "close the page, open the next, erase it, item header, item data"
        );

        let mut failures = 0usize;
        let mut lost = 0usize;
        for nth in 0..ops {
            let mut queue = open(SimNor::new(pages), pages);
            for i in 0..pre {
                queue.push(&body(i, body_len), true).await.unwrap();
            }
            let before = ids(&drain_view(&mut queue).await.unwrap(), pre, body_len);

            queue.flash().fail_op_after(nth);
            let outcome = queue.push(&body(pre, body_len), true).await;
            if outcome.is_err() {
                failures += 1;
                assert!(!queue.flash().failure_armed(), "nth={nth}");
                // Consistent where it failed. This push overwrites, so it may
                // already have dropped entries from the oldest end before the
                // failure reached it; what it may not do is invent one,
                // reorder them, leave a gap, or lose one from the newest end.
                let after = drain_view(&mut queue)
                    .await
                    .unwrap_or_else(|e| panic!("nth={nth}: inconsistent after the failure: {e:?}"));
                lost += assert_contiguous_suffix(
                    &before,
                    &ids(&after, pre, body_len),
                    pre - 1,
                    &format!("nth={nth}"),
                );
                // And a retry succeeds, with different bytes than the push
                // that failed.
                queue
                    .push(&body(pre + 100, body_len), true)
                    .await
                    .unwrap_or_else(|e| panic!("nth={nth}: the retry must land: {e:?}"));
            }

            let (flash, _) = queue.destroy();
            let mut queue = open(flash, pages);
            let view = drain_view(&mut queue).await.unwrap();
            assert!(!view.is_empty(), "nth={nth}");
            assert!(
                view.last() == Some(&body(pre, body_len))
                    || view.last() == Some(&body(pre + 100, body_len)),
                "nth={nth}: the newest entry is the one that landed"
            );
        }
        assert_eq!(
            failures, 5,
            "every operation of this page-closing push can fail"
        );
        // The erase drops the four entries of the page it took, on each of
        // the four sweeps that get past the erase itself. Both candidates
        // pay this: it is what "erase before write" costs a store that
        // overwrites, not a defect in either.
        assert_eq!(lost, 16);
    });
}

// ------------------------------------------------------------ criterion 3
// Wear: wrap the region twice; per-page erase counts differ by at most one;
// negative control pins a page.

#[test]
fn c3_wear_is_level_after_wrapping_the_region_twice() {
    block_on(async {
        let mut queue = open(SimNor::new(PAGES), PAGES);
        // Push until every page has been erased twice: two wraps, measured
        // rather than estimated from an assumed per-page capacity.
        let mut entries = 0u32;
        while queue.flash().erase_counts().iter().copied().min().unwrap() < 2 {
            queue.push(&body(entries, 304), true).await.unwrap();
            entries += 1;
            assert!(entries < 10_000, "the region never wrapped twice");
        }
        assert_eq!(entries, 300, "304-byte entries it takes to wrap twice");
        let flash = queue.destroy().0;
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
    // Without this, "wear is level" could be true because nothing is
    // counted. Page 0 stands in for a fixed metadata page a store might
    // rewrite on every accepted entry; the same workload, the same check,
    // and it fails.
    block_on(async {
        let pages = PAGES + 1;
        let mut queue = QueueStorage::new(
            SimNor::new(pages),
            QueueConfig::new(PAGE..pages * PAGE),
            Cache::new_uncached(),
        );
        let entries = 300u32;
        for i in 0..entries {
            queue.push(&body(i, 304), true).await.unwrap();
            use embedded_storage_async::nor_flash::NorFlash;
            queue.flash().erase(0, PAGE).await.unwrap();
        }
        let flash = queue.destroy().0;
        let counts = flash.erase_counts();
        assert_eq!(counts[0], entries, "the pinned page takes one erase each");
        assert!(
            wear_spread(counts) > 1,
            "the wear check has to fail here: {counts:?}"
        );
        assert!(wear_spread(&counts[1..]) <= 1, "{counts:?}");
        assert!(
            counts[0] / counts[1] > 100,
            "the ratio is the argument: {counts:?}"
        );
    });
}

// ------------------------------------------------------------ criterion 4
// Foreign content in the region: detected and formatted, never a panic,
// never an endless loop.

/// What a store made of a region it did not write.
#[derive(Debug, PartialEq, Eq)]
enum Foreign {
    /// Read as empty and usable straight away: the best outcome.
    ReadAsEmpty,
    /// Reported an error; one `erase_all` made it usable.
    Formatted,
    /// Handed back entries nobody put there: the outcome that would be a
    /// defect rather than a result.
    SurfacedAsEntries,
}

/// Plant `content` over the region, then find out which of the three it is —
/// asserting only that it is one of them, that nothing panics, and that the
/// store works afterwards.
async fn foreign_region(content: &[u8]) -> Foreign {
    let mut flash = SimNor::new(PAGES);
    flash.plant(0, content);
    // A runaway scan would spin forever rather than fail; cap the flash
    // operations so an endless loop shows up as an error instead of a hang.
    flash.fail_op_after(10_000);
    let mut queue = open(flash, PAGES);

    let verdict = match drain_view(&mut queue).await {
        Ok(view) if view.is_empty() => {
            if queue.push(&body(1, 64), false).await.is_ok() {
                Foreign::ReadAsEmpty
            } else {
                Foreign::Formatted
            }
        }
        Ok(_) => Foreign::SurfacedAsEntries,
        Err(_) => Foreign::Formatted,
    };
    if verdict != Foreign::ReadAsEmpty {
        queue.erase_all().await.unwrap();
    }
    let (flash, _) = queue.destroy();
    let mut queue = open(flash, PAGES);
    for i in 0..20u32 {
        queue
            .push(&body(i, 304), true)
            .await
            .unwrap_or_else(|e| panic!("after recovery the store must take entries: {e:?}"));
    }
    let view = drain_view(&mut queue).await.unwrap();
    assert!(view.len() >= 20, "{} entries survived", view.len());
    assert_eq!(
        view[view.len() - 20..],
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
        // Every page looks closed to the page-state markers, so the store
        // reports rather than guesses, and `erase_all` is the recovery.
        assert_eq!(
            foreign_region(&vec![0u8; REGION as usize]).await,
            Foreign::Formatted
        );
    });
}

#[test]
fn c4_a_page_of_plausible_markers() {
    // The nastiest foreign content is the store's own shape with the wrong
    // bytes after it: page-state markers that say "closed", and garbage
    // where the items should be.
    block_on(async {
        let mut content = noise(0xC0FFEE, REGION as usize);
        for page in 0..PAGES as usize {
            let base = page * PAGE as usize;
            content[base..base + 4].copy_from_slice(&[0u8; 4]);
            content[base + PAGE as usize - 4..base + PAGE as usize].copy_from_slice(&[0u8; 4]);
        }
        assert_eq!(foreign_region(&content).await, Foreign::Formatted);
    });
}

// ------------------------------------------------------------ cancellation

#[test]
fn cancellation_needs_the_store_to_own_the_future() {
    // The crate's README warns that a cancelled future can leave data that
    // needs repair. Over `Yielding` every flash operation is a point a
    // `select!` could drop the push at. Drop it at each in turn and require
    // what a power cut at the same place gives: the entries already accepted
    // are still readable and the next push lands. `try_repair` runs inside
    // `push` and `iter` automatically, so the discipline a store task needs
    // is not a repair call — it is never handing the future to a caller that
    // can drop it: own the flash in one task and reach it by channel.
    block_on(async {
        let pages = 4u32;
        for stop in 1..=6usize {
            let mut queue = QueueStorage::new(
                Yielding::new(SimNor::new(pages)),
                QueueConfig::new(0..pages * PAGE),
                Cache::new_uncached(),
            );
            for i in 0..2u32 {
                queue.push(&body(i, 200), true).await.unwrap();
            }
            {
                let entry = body(2, 200);
                let mut fut = core::pin::pin!(queue.push(&entry, true));
                let mut cx = core::task::Context::from_waker(core::task::Waker::noop());
                let mut finished = false;
                for _ in 0..stop {
                    if fut.as_mut().poll(&mut cx).is_ready() {
                        finished = true;
                        break;
                    }
                }
                assert!(!finished, "stop={stop}: the push finished before the drop");
            }

            let flash = queue.destroy().0.into_inner();
            let mut queue = open(flash, pages);
            let view = drain_view(&mut queue).await.unwrap_or_else(|e| {
                panic!("stop={stop}: reopening after the cancel failed: {e:?}")
            });
            assert!(
                view.len() >= 2,
                "stop={stop}: the entries already accepted must survive: {} left",
                view.len()
            );
            assert_eq!(view[0], body(0, 200), "stop={stop}");
            assert_eq!(view[1], body(1, 200), "stop={stop}");
            queue
                .push(&body(3, 200), true)
                .await
                .unwrap_or_else(|e| panic!("stop={stop}: the next push must land: {e:?}"));
        }
    });
}
