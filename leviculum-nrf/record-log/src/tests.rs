//! Host tests over the simulated part.
//!
//! The three that matter most are named for what they prove rather than for
//! what they call: [`power_cut_at_every_word_of_a_record_write`],
//! [`word_writes_between_erases_never_exceed_two`] and
//! [`a_pinned_metadata_page_fails_the_wear_check`]. The first is the reason
//! the store can be trusted across a battery pull; the second is the reason
//! it can be trusted on *this* part, whose word-write budget is two; the
//! third is the positive control for the wear pin, without which "wear is
//! level" is a sentence rather than a measurement.
//!
//! Every test drives the async API through [`block_on`]: the store is async
//! because `nrf_softdevice::Flash` is, and the future is the thing under
//! test, not a blocking shim around it.

use alloc::vec;
use alloc::vec::Vec;

use core::future::Future;

use embedded_storage_async::nor_flash::{NorFlash, ReadNorFlash};

use crate::sim::{block_on, SimNor, Yielding, ERASED, WORDS_PER_PAGE, WRITES_PER_WORD};
use crate::*;

/// A 4-byte-aligned literal for the tests that drive [`SimNor`] directly.
/// `[u8; N]` on the stack is only byte-aligned; the part no longer refuses
/// one, but a program buffer that reaches a DMA engine some day should be
/// written the way the rest of the crate writes them.
fn aligned<const N: usize>(bytes: [u8; N]) -> Aligned<N> {
    Aligned(bytes)
}

const SECTORS: u32 = 16;
const REGION: u32 = SECTORS * SECTOR_SIZE;
/// The median stored object the 2026-09-09 field walk measured: 272 B of
/// `lxmf_data` plus a 32-byte propagation stamp.
const FIELD_BODY: usize = 304;

fn key(n: u32) -> [u8; KEY_LEN] {
    let mut k = [0u8; KEY_LEN];
    k[0..4].copy_from_slice(&n.to_le_bytes());
    k[28..32].copy_from_slice(&n.to_be_bytes());
    k
}

fn body(n: u32, len: usize) -> Vec<u8> {
    (0..len).map(|i| (n as u8).wrapping_add(i as u8)).collect()
}

async fn fresh(sectors: u32) -> RecordLog<SimNor> {
    RecordLog::open(SimNor::new(sectors), 0, sectors * SECTOR_SIZE)
        .await
        .unwrap()
}

async fn collect(log: &mut RecordLog<SimNor>) -> Vec<Record> {
    let mut out = Vec::new();
    log.for_each(|r| out.push(*r)).await.unwrap();
    out
}

/// The wear pin itself: the spread between the most- and least-erased page.
/// Round-robin reclaim keeps it at 0 or 1 forever; anything that writes to a
/// fixed place does not.
fn wear_spread(counts: &[u32]) -> u32 {
    let max = counts.iter().copied().max().unwrap_or(0);
    let min = counts.iter().copied().min().unwrap_or(0);
    max - min
}

/// A deterministic stream of bytes that is not flash-shaped: neither `0xFF`
/// nor `0x00` runs, so "somebody else's data" is actually foreign.
fn noise(seed: u32, len: usize) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        })
        .collect()
}

// ---------------------------------------------------------------- the CRC

#[test]
fn crc16_matches_the_standard_check_value() {
    // CRC-16/CCITT-FALSE's published check value, and the same vector
    // `leviculum_core::framing::hdlc`'s own test uses. If this ever
    // disagrees, the two implementations have drifted.
    assert_eq!(crc16_update(CRC_INIT, b"123456789"), 0x29B1);
}

#[test]
fn crc16_is_incremental() {
    let split = crc16_update(crc16_update(CRC_INIT, b"12345"), b"6789");
    assert_eq!(split, crc16_update(CRC_INIT, b"123456789"));
}

// ------------------------------------------- the simulated part's semantics

#[test]
fn a_fresh_part_reads_as_erased() {
    block_on(async {
        let mut sim = SimNor::new(2);
        assert!(sim.bytes().iter().all(|b| *b == ERASED));
        let mut buf = aligned([0u8; 8]);
        sim.read(0, &mut buf.0).await.unwrap();
        assert_eq!(buf.0, [ERASED; 8]);
    });
}

#[test]
fn an_erase_covers_exactly_its_page() {
    block_on(async {
        let mut sim = SimNor::new(2);
        sim.write(0, &aligned([0u8; 8]).0).await.unwrap();
        sim.write(SECTOR_SIZE, &aligned([0u8; 8]).0).await.unwrap();
        sim.erase(0, SECTOR_SIZE).await.unwrap();
        assert!(sim.bytes()[..SECTOR_SIZE as usize]
            .iter()
            .all(|b| *b == ERASED));
        assert_eq!(sim.bytes()[SECTOR_SIZE as usize], 0);
        assert_eq!(sim.erase_counts(), &[1, 0]);
        // And the erase is what resets the word-write budget.
        assert_eq!(sim.word_writes()[0], 0);
        assert_eq!(sim.word_writes()[WORDS_PER_PAGE], 1);
    });
}

#[test]
fn programming_clears_bits_and_never_restores_them() {
    block_on(async {
        let mut sim = SimNor::new(1);
        sim.write(0, &aligned([0b1111_0000, 0xFF, 0xFF, 0xFF]).0)
            .await
            .unwrap();
        assert_eq!(sim.bytes()[0], 0b1111_0000);
        // A second write may clear further bits in the same word without an
        // erase — which is exactly what a purge does to its commit word.
        sim.write(0, &aligned([0b1010_0000, 0xFF, 0xFF, 0xFF]).0)
            .await
            .unwrap();
        assert_eq!(sim.bytes()[0], 0b1010_0000);
        // And programming 0xFF over anything leaves it alone, which is what
        // makes the record padding free.
        assert_eq!(sim.bytes()[1..4], [0xFF; 3]);
        assert_eq!(sim.max_word_writes(), 2);
    });
}

#[test]
fn the_word_write_counter_sees_a_third_write() {
    // The positive control for the counter the whole part-fit argument
    // rests on. Without it, "no word is written more than twice" could be
    // true because nothing is counted.
    block_on(async {
        let mut sim = SimNor::new(1);
        for pattern in [0xF0u8, 0xE0, 0xC0] {
            sim.write(0, &aligned([pattern, 0xFF, 0xFF, 0xFF]).0)
                .await
                .unwrap();
        }
        assert_eq!(sim.max_word_writes(), 3);
        assert!(sim.max_word_writes() > WRITES_PER_WORD);
    });
}

#[test]
#[should_panic(expected = "would raise a bit")]
fn programming_a_one_over_a_zero_is_refused() {
    // The negative control for the program-once model: without it, the
    // simulation is RAM and every commit-ordering bug passes.
    block_on(async {
        let mut sim = SimNor::new(1);
        sim.write(0, &aligned([0x00, 0xFF, 0xFF, 0xFF]).0)
            .await
            .unwrap();
        sim.write(0, &aligned([0xFF, 0xFF, 0xFF, 0xFF]).0)
            .await
            .unwrap();
    });
}

#[test]
#[should_panic(expected = "4-byte aligned")]
fn an_unaligned_program_address_is_refused() {
    block_on(async {
        let mut sim = SimNor::new(1);
        sim.write(2, &aligned([0u8; 4]).0).await.unwrap();
    });
}

#[test]
#[should_panic(expected = "multiple of 4")]
fn a_program_length_off_the_unit_is_refused() {
    block_on(async {
        let mut sim = SimNor::new(1);
        sim.write(0, &aligned([0u8; 4]).0[..3]).await.unwrap();
    });
}

#[test]
#[should_panic(expected = "sector-aligned")]
fn an_erase_off_the_page_grid_is_refused() {
    block_on(async {
        let mut sim = SimNor::new(2);
        sim.erase(2048, 2048 + SECTOR_SIZE).await.unwrap();
    });
}

#[test]
fn an_unaligned_read_is_allowed_because_flash_is_mapped() {
    // The QSPI peripheral asserted alignment on reads; the internal flash
    // is memory-mapped and a read is a memcpy, so the simulation must not
    // invent a rule the part does not have — a store that scans with a
    // three-byte read at an odd offset is fine here.
    block_on(async {
        let mut sim = SimNor::new(1);
        sim.write(0, &aligned([0x11, 0x22, 0x33, 0x44]).0)
            .await
            .unwrap();
        let mut buf = [0u8; 3];
        sim.read(1, &mut buf).await.unwrap();
        assert_eq!(buf, [0x22, 0x33, 0x44]);
    });
}

#[test]
fn an_injected_failure_changes_nothing_and_the_retry_succeeds() {
    // The SoftDevice fails a flash operation with a timeout when it finds
    // no gap between radio events. It is an operation that did not happen,
    // not a partial one.
    block_on(async {
        let mut sim = SimNor::new(2);
        sim.write(0, &aligned([0xF0, 0xF0, 0xF0, 0xF0]).0)
            .await
            .unwrap();
        let before: Vec<u8> = sim.bytes().to_vec();
        let spent = sim.spent();

        sim.fail_next_op();
        assert_eq!(
            sim.write(4, &aligned([0x00; 4]).0).await,
            Err(crate::sim::SimError::Timeout)
        );
        assert_eq!(sim.bytes(), before.as_slice(), "a timeout moves no bytes");
        assert_eq!(sim.spent(), spent, "and costs no word");
        assert!(!sim.failure_armed(), "the injection is one shot");

        sim.write(4, &aligned([0x00; 4]).0).await.unwrap();
        assert_eq!(sim.bytes()[4..8], [0x00; 4]);

        // The same for an erase.
        sim.fail_next_op();
        assert_eq!(
            sim.erase(0, SECTOR_SIZE).await,
            Err(crate::sim::SimError::Timeout)
        );
        assert_eq!(sim.erase_counts(), &[0, 0], "a refused erase is not wear");
        sim.erase(0, SECTOR_SIZE).await.unwrap();
        assert_eq!(sim.erase_counts(), &[1, 0]);
    });
}

#[test]
fn a_torn_erase_leaves_the_page_neither_erased_nor_intact() {
    block_on(async {
        let mut sim = SimNor::new(2);
        let planted = noise(7, SECTOR_SIZE as usize);
        sim.plant(0, &planted);

        // Cut a quarter of the way into the page erase.
        let cut = WORDS_PER_PAGE / 4;
        sim.arm_power_cut(cut);
        assert_eq!(
            sim.erase(0, SECTOR_SIZE).await,
            Err(crate::sim::SimError::PowerCut)
        );
        sim.power_on();

        let after = sim.bytes();
        let done = cut * PROGRAM_UNIT as usize;
        assert!(
            after[..done].iter().all(|b| *b == ERASED),
            "the words the erase reached are erased"
        );
        assert!(
            after[done + PROGRAM_UNIT as usize..SECTOR_SIZE as usize]
                == planted[done + PROGRAM_UNIT as usize..],
            "the words it never reached still hold what they held"
        );
        let torn = &after[done..done + PROGRAM_UNIT as usize];
        let was = &planted[done..done + PROGRAM_UNIT as usize];
        assert_ne!(torn, was, "the word it stopped in is not intact");
        assert!(
            torn.iter().any(|b| *b != ERASED),
            "and it is not erased either: {torn:?}"
        );
        assert!(sim.is_torn(0));
        assert!(!sim.is_torn(1));

        // A program into a torn page is refused, because a cell whose erase
        // pulse was cut has no defined program behaviour. Redoing the erase
        // is the one legal recovery, and it works.
        assert_eq!(
            sim.write(0, &aligned([0u8; 4]).0).await,
            Err(crate::sim::SimError::TornPage)
        );
        sim.erase(0, SECTOR_SIZE).await.unwrap();
        assert!(!sim.is_torn(0));
        sim.write(0, &aligned([0u8; 4]).0).await.unwrap();
        assert_eq!(sim.erase_counts(), &[2, 0], "both erases are wear");
    });
}

// -------------------------------------------------------- mounting, layout

#[test]
fn the_record_header_is_the_forty_two_bytes_the_paper_tabulates() {
    assert_eq!(HEADER_LEN, 42);
    assert_eq!(SECTOR_SIZE, 4096);
    // 11 field-sized records per page, as the paper's capacity table says.
    assert_eq!(record_stride(FIELD_BODY), 348);
    assert_eq!(SECTOR_PAYLOAD as usize / record_stride(FIELD_BODY), 11);
}

#[test]
fn a_fresh_region_formats_itself_and_holds_nothing() {
    block_on(async {
        let mut log = fresh(SECTORS).await;
        assert_eq!(collect(&mut log).await.len(), 0);
        assert_eq!(log.active_sector(), 0);
        assert_eq!(log.sequence(), 0);
        let sim = log.into_flash();
        // Exactly one page was touched to format the region.
        assert_eq!(sim.erase_counts()[0], 1);
        assert!(sim.erase_counts()[1..].iter().all(|c| *c == 0));
    });
}

#[test]
fn mounting_a_formatted_region_writes_nothing() {
    block_on(async {
        let mut log = fresh(SECTORS).await;
        for i in 0..5u32 {
            log.append(&key(i), i, 1, &body(i, FIELD_BODY))
                .await
                .unwrap();
        }
        let sim = log.into_flash();
        let spent = sim.spent();
        let erases: Vec<u32> = sim.erase_counts().to_vec();

        let mut log = RecordLog::open(sim, 0, REGION).await.unwrap();
        assert_eq!(collect(&mut log).await.len(), 5);
        let sim = log.into_flash();
        // Mounting is a read. If it were not, the mount itself would be a
        // fixed-location write on every boot, which is the failure mode the
        // endurance table is about.
        assert_eq!(sim.spent(), spent);
        assert_eq!(sim.erase_counts(), erases.as_slice());
    });
}

#[test]
fn a_region_shorter_than_two_pages_is_refused() {
    block_on(async {
        assert!(matches!(
            RecordLog::open(SimNor::new(4), 0, SECTOR_SIZE).await,
            Err(Error::BadRegion)
        ));
        assert!(matches!(
            RecordLog::open(SimNor::new(4), 512, 2 * SECTOR_SIZE).await,
            Err(Error::BadRegion)
        ));
        assert!(matches!(
            RecordLog::open(SimNor::new(4), 2 * SECTOR_SIZE, 4 * SECTOR_SIZE).await,
            Err(Error::OutOfBounds)
        ));
    });
}

#[test]
fn a_body_past_a_page_is_refused() {
    block_on(async {
        let mut log = fresh(SECTORS).await;
        assert!(matches!(
            log.append(&key(0), 0, 0, &body(0, MAX_BODY + 1)).await,
            Err(Error::BodyTooLarge)
        ));
        // The largest body that does fit is accepted and reads back. This is
        // the number the spike reports as "largest entry on a 4 KiB page".
        assert_eq!(MAX_BODY, 4042);
        let big = body(1, MAX_BODY);
        log.append(&key(1), 1, 0, &big).await.unwrap();
        let recs = collect(&mut log).await;
        assert_eq!(recs.len(), 1);
        let mut out = vec![0u8; MAX_BODY];
        log.read_body(&recs[0], &mut out).await.unwrap();
        assert_eq!(out, big);
    });
}

// --------------------------------------------------------- append and scan

#[test]
fn appended_records_read_back_in_order() {
    block_on(async {
        let lengths = [0usize, 1, 2, 3, 4, 63, FIELD_BODY, 1000];
        let mut log = fresh(SECTORS).await;
        for (i, len) in lengths.iter().enumerate() {
            let i = i as u32;
            log.append(&key(i), 1_000_000 + i, (i as u8) | 0x80, &body(i, *len))
                .await
                .unwrap();
        }
        let sim = log.into_flash();
        let mut log = RecordLog::open(sim, 0, REGION).await.unwrap();

        let recs = collect(&mut log).await;
        assert_eq!(recs.len(), lengths.len());
        for (i, (rec, len)) in recs.iter().zip(lengths.iter()).enumerate() {
            let i = i as u32;
            assert_eq!(rec.key, key(i));
            assert_eq!(rec.time, 1_000_000 + i);
            assert_eq!(rec.tag, (i as u8) | 0x80);
            assert_eq!(rec.len as usize, *len);
            assert!(rec.is_live());
            assert_eq!(rec.offset % PROGRAM_UNIT, 0, "records stay word-aligned");
            let mut out = vec![0u8; *len];
            assert_eq!(log.read_body(rec, &mut out).await.unwrap(), *len);
            assert_eq!(out, body(i, *len));
        }
    });
}

#[test]
fn a_record_never_straddles_a_page() {
    block_on(async {
        let mut log = fresh(4).await;
        // 11 of these fit in a page with 256 bytes to spare, which is less
        // than one more record: the twelfth has to start a new page.
        for i in 0..12u32 {
            log.append(&key(i), i, 0, &body(i, FIELD_BODY))
                .await
                .unwrap();
        }
        assert_eq!(log.active_sector(), 1);
        let recs = collect(&mut log).await;
        assert_eq!(recs.len(), 12);
        for rec in &recs {
            let start = rec.offset % SECTOR_SIZE;
            assert!(
                start + rec.stride() <= SECTOR_SIZE,
                "record at {:#x} runs past its page",
                rec.offset
            );
            assert!(start >= SECTOR_HEADER_LEN);
        }
        assert_eq!(recs[11].offset / SECTOR_SIZE, 1);
    });
}

#[test]
fn reclaim_is_round_robin_and_takes_the_oldest_page() {
    block_on(async {
        let sectors = 4u32;
        let per_sector = (SECTOR_PAYLOAD as usize / record_stride(FIELD_BODY)) as u32;
        let mut log = fresh(sectors).await;
        // One full lap plus one page, so page 0 is reclaimed and its
        // records are gone.
        let total = per_sector * (sectors + 1);
        for i in 0..total {
            log.append(&key(i), i, 0, &body(i, FIELD_BODY))
                .await
                .unwrap();
        }
        // 55 records of 348 bytes, 11 to a page: four reclaims, ending back
        // on page 0 with the records it started with gone.
        let reclaims = total.div_ceil(per_sector) - 1;
        assert_eq!(reclaims, 4);
        assert_eq!(log.active_sector(), reclaims % sectors);
        assert_eq!(log.active_sector(), 0);

        let sim = log.into_flash();
        // Page 0 was erased twice — once to format, once on the lap — and
        // every other page once. That is the spread the wear pin allows.
        assert_eq!(sim.erase_counts(), &[2, 1, 1, 1]);
        let mut log = RecordLog::open(sim, 0, sectors * SECTOR_SIZE)
            .await
            .unwrap();

        let recs = collect(&mut log).await;
        // Whatever survives is the newest run, contiguous and in order.
        assert_eq!(recs.len() as u32, per_sector * sectors);
        let first = u32::from_le_bytes(recs[0].key[0..4].try_into().unwrap());
        assert_eq!(first, total - recs.len() as u32);
        for (n, rec) in recs.iter().enumerate() {
            assert_eq!(rec.key, key(first + n as u32));
            assert_eq!(rec.time, first + n as u32);
        }
        assert_eq!(log.sequence(), reclaims);
    });
}

#[test]
fn purge_withdraws_a_record_without_moving_anything() {
    block_on(async {
        let mut log = fresh(SECTORS).await;
        for i in 0..3u32 {
            log.append(&key(i), i, 0, &body(i, 64)).await.unwrap();
        }
        let recs = collect(&mut log).await;
        let offsets: Vec<u32> = recs.iter().map(|r| r.offset).collect();
        log.purge(&recs[1]).await.unwrap();

        let sim = log.into_flash();
        let mut log = RecordLog::open(sim, 0, REGION).await.unwrap();
        let after = collect(&mut log).await;
        assert_eq!(after.len(), 3);
        assert_eq!(
            after.iter().map(|r| r.offset).collect::<Vec<_>>(),
            offsets,
            "a purge is a bit, not a move"
        );
        assert!(after[0].is_live());
        assert!(!after[1].is_live());
        assert_eq!(after[1].flags, FLAG_PURGED);
        assert!(after[2].is_live());
        // The body of a purged record is still there and still correct: the
        // reclaim is what removes it, nothing else.
        let mut out = vec![0u8; 64];
        log.read_body(&after[1], &mut out).await.unwrap();
        assert_eq!(out, body(1, 64));
        // And purging twice is not an error — nor a third write to the word,
        // which is what would make it one on this part.
        log.purge(&after[1]).await.unwrap();
        assert_eq!(collect(&mut log).await.len(), 3);
        assert_eq!(log.flash_mut().max_word_writes(), WRITES_PER_WORD);
    });
}

#[test]
fn a_lost_bit_in_a_body_hides_that_record_and_no_other() {
    block_on(async {
        let mut log = fresh(SECTORS).await;
        for i in 0..4u32 {
            log.append(&key(i), i, 0, &body(i, 64)).await.unwrap();
        }
        let recs = collect(&mut log).await;
        let victim = recs[1];
        let mut sim = log.into_flash();
        // body(1, 64)[7] is 0x08; clearing its one set bit is a bit the part
        // could plausibly lose, and `corrupt` refuses a no-op.
        sim.corrupt(victim.body_offset() + 7, 0xF7);

        let mut log = RecordLog::open(sim, 0, REGION).await.unwrap();
        let after = collect(&mut log).await;
        assert_eq!(after.len(), 3, "only the damaged record disappears");
        let keys: Vec<[u8; KEY_LEN]> = after.iter().map(|r| r.key).collect();
        assert_eq!(keys, vec![key(0), key(2), key(3)]);
    });
}

#[test]
fn a_committed_flags_byte_is_the_only_thing_that_makes_a_record() {
    block_on(async {
        // Everything a record needs is on the part except the commit, which
        // is exactly the state a power cut leaves. It must not be readable.
        let mut log = fresh(SECTORS).await;
        log.append(&key(0), 0, 0, &body(0, 32)).await.unwrap();
        let recs = collect(&mut log).await;
        let mut sim = log.into_flash();
        // 0xFE -> 0xFA is a value no commit produces.
        sim.corrupt(recs[0].offset + 39, 0xFA);
        let mut log = RecordLog::open(sim, 0, REGION).await.unwrap();
        assert_eq!(collect(&mut log).await.len(), 0);
    });
}

#[test]
fn the_commit_word_stays_erased_until_it_is_the_commit() {
    // The one format change the internal flash forced, asserted directly:
    // after the two program runs of an append the commit word must still be
    // 0xFF, or the commit would be its second write and a purge its
    // forbidden third.
    block_on(async {
        let mut log = fresh(2).await;
        log.flash_mut().arm_power_cut(WORDS_PER_PAGE); // enough for the runs
        let offset = SECTOR_HEADER_LEN;
        // Cut the append exactly after its two program runs, before the
        // commit: the record's stride in words, minus the commit word.
        let words = record_stride(FIELD_BODY) / PROGRAM_UNIT as usize - 1;
        log.flash_mut().arm_power_cut(words);
        assert!(log
            .append(&key(0), 7, 3, &body(0, FIELD_BODY))
            .await
            .is_err());
        let mut sim = log.into_flash();
        sim.power_on();
        assert_eq!(
            &sim.bytes()[(offset + COMMIT_OFF) as usize..(offset + AFTER_COMMIT) as usize],
            &[ERASED; PROGRAM_UNIT as usize],
            "the commit word must be untouched by the body runs"
        );
        assert_eq!(
            sim.word_writes()[((offset + COMMIT_OFF) / PROGRAM_UNIT) as usize],
            0
        );
        // Everything around it did land, so this is not a cut that happened
        // before the record was written at all.
        assert_ne!(sim.bytes()[offset as usize], ERASED);
    });
}

// ------------------------------------------------------------- power cuts

/// Drive one power cut, `cut` word units into the record write that follows
/// `pre` complete records, and assert what came back.
async fn probe_cut(sectors: u32, body_len: usize, pre: u32, cut: usize, cost: usize) {
    let region = sectors * SECTOR_SIZE;
    let mut log = RecordLog::open(SimNor::new(sectors), 0, region)
        .await
        .unwrap();
    for i in 0..pre {
        log.append(&key(i), i, 1, &body(i, body_len)).await.unwrap();
    }
    log.flash_mut().arm_power_cut(cut);
    let landed = log
        .append(&key(pre), pre, 1, &body(pre, body_len))
        .await
        .is_ok();
    assert_eq!(
        landed,
        cut >= cost,
        "cut={cut} of {cost}: the append's own verdict must match the budget"
    );

    let mut sim = log.into_flash();
    sim.power_on();
    let mut log = RecordLog::open(sim, 0, region).await.unwrap();
    let recs = collect(&mut log).await;

    let expected = if landed { pre + 1 } else { pre };
    assert_eq!(
        recs.len() as u32,
        expected,
        "cut={cut} of {cost}: every completed record and no partial one"
    );
    for (n, rec) in recs.iter().enumerate() {
        let n = n as u32;
        assert_eq!(rec.key, key(n), "cut={cut}");
        assert_eq!(rec.time, n, "cut={cut}");
        assert!(rec.is_live(), "cut={cut}");
        let mut out = vec![0u8; rec.len as usize];
        log.read_body(rec, &mut out).await.unwrap();
        assert_eq!(
            out,
            body(n, body_len),
            "cut={cut}: body {n} survived intact"
        );
    }

    // A store that reopens read-only after a cut is half a store. The next
    // record has to land, which is where "seal the dirty page" earns its
    // keep: appending into the half-written tail would need to raise bits
    // and `SimNor` would panic.
    log.append(&key(9999), 9999, 2, &body(9999, body_len))
        .await
        .unwrap();
    assert_eq!(
        collect(&mut log).await.len() as u32,
        expected + 1,
        "cut={cut}"
    );
}

/// What one append costs the part in word units, measured rather than
/// derived, so the sweep below cannot silently stop short of the end.
async fn append_cost(sectors: u32, body_len: usize, pre: u32) -> usize {
    let region = sectors * SECTOR_SIZE;
    let mut log = RecordLog::open(SimNor::new(sectors), 0, region)
        .await
        .unwrap();
    for i in 0..pre {
        log.append(&key(i), i, 1, &body(i, body_len)).await.unwrap();
    }
    let before = log.flash_mut().spent();
    log.append(&key(pre), pre, 1, &body(pre, body_len))
        .await
        .unwrap();
    log.flash_mut().spent() - before
}

#[test]
fn power_cut_at_every_word_of_a_record_write() {
    block_on(async {
        // Body lengths chosen so that 42 + len lands on each residue mod 4 —
        // the padding path differs in each — plus the empty body and the
        // median the field walk measured.
        let lengths = [0usize, 1, 2, 3, 5, 61, FIELD_BODY];
        let mut offsets = 0usize;
        for len in lengths {
            let cost = append_cost(SECTORS, len, 3).await;
            assert_eq!(
                cost,
                record_stride(len) / PROGRAM_UNIT as usize,
                "an append writes each word of the record's stride exactly once"
            );
            for cut in 0..=cost {
                probe_cut(SECTORS, len, 3, cut, cost).await;
                offsets += 1;
            }
        }
        // 177 cut points over seven body lengths — one per word of each
        // record plus the no-op cut before the first. Kept as an assertion
        // so a change that quietly shrinks the sweep is a failure, not a
        // faster test. It was 715 byte-offsets while the part programmed
        // single bytes; the unit is the word now because `sd_flash_write`
        // has no other.
        assert_eq!(offsets, 177);
    });
}

#[test]
fn power_cut_at_every_word_of_a_reclaim() {
    block_on(async {
        // Three 1000-byte records fill a page, so the fourth append has to
        // erase the next one: this sweep covers the erase — every word of
        // it — the new page header, the record and the commit.
        let sectors = 4u32;
        let body_len = 1000usize;
        let pre = 3u32;
        let cost = append_cost(sectors, body_len, pre).await;
        assert_eq!(
            cost,
            WORDS_PER_PAGE
                + SECTOR_HEADER_LEN as usize / PROGRAM_UNIT as usize
                + record_stride(body_len) / PROGRAM_UNIT as usize
        );
        assert_eq!(cost, 1288);
        let mut offsets = 0usize;
        for cut in 0..=cost {
            probe_cut(sectors, body_len, pre, cut, cost).await;
            offsets += 1;
        }
        assert_eq!(offsets, 1289);
    });
}

#[test]
fn a_cut_costs_at_most_the_rest_of_one_page() {
    block_on(async {
        // The sealed-page rule is a cost as well as a guarantee. Pin it: a
        // cut early in a nearly empty page loses that page's remaining room
        // and nothing else.
        let mut log = fresh(SECTORS).await;
        log.append(&key(0), 0, 1, &body(0, 64)).await.unwrap();
        log.flash_mut().arm_power_cut(2);
        assert!(log.append(&key(1), 1, 1, &body(1, 64)).await.is_err());
        let mut sim = log.into_flash();
        sim.power_on();
        let mut log = RecordLog::open(sim, 0, REGION).await.unwrap();
        log.append(&key(2), 2, 1, &body(2, 64)).await.unwrap();
        let recs = collect(&mut log).await;
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].offset / SECTOR_SIZE, 0);
        assert_eq!(recs[1].offset / SECTOR_SIZE, 1, "the dirty page is sealed");
        let sim = log.into_flash();
        // Sealing costs one extra erase and nothing more.
        assert_eq!(sim.erase_counts()[0], 1);
        assert_eq!(sim.erase_counts()[1], 1);
        assert!(sim.erase_counts()[2..].iter().all(|c| *c == 0));
    });
}

/// How many flash operations one append costs, measured, so the sweep below
/// covers all of them instead of the first few.
async fn append_ops(sectors: u32, body_len: usize, pre: u32) -> usize {
    let region = sectors * SECTOR_SIZE;
    let mut log = RecordLog::open(SimNor::new(sectors), 0, region)
        .await
        .unwrap();
    for i in 0..pre {
        log.append(&key(i), i, 1, &body(i, body_len)).await.unwrap();
    }
    let before = log.flash_mut().ops();
    log.append(&key(pre), pre, 1, &body(pre, body_len))
        .await
        .unwrap();
    log.flash_mut().ops() - before
}

#[test]
fn a_failed_operation_at_every_step_of_an_append() {
    // The SoftDevice fails a flash operation with a timeout when it finds no
    // gap between radio events: an operation that did not happen, not a
    // partial one. Fail each operation of an append in turn — the reclaim's
    // erase, the page header, both program runs, the commit — and require
    // the same three things every time: the call reports the error, the
    // records already on the part are untouched, and a retry lands.
    //
    // The retry carries a *different* record on purpose. A retry of the
    // identical bytes would be programming the same values over themselves
    // and would pass even if the store had no sealing rule at all.
    block_on(async {
        let sectors = 4u32;
        let region = sectors * SECTOR_SIZE;
        let body_len = 1000usize;
        let ops = append_ops(sectors, body_len, 3).await;
        assert_eq!(
            ops, 20,
            "erase, page header, header run, sixteen body windows, commit"
        );

        let mut sealed = 0usize;
        for nth in 0..ops {
            let mut log = RecordLog::open(SimNor::new(sectors), 0, region)
                .await
                .unwrap();
            for i in 0..3u32 {
                log.append(&key(i), i, 1, &body(i, body_len)).await.unwrap();
            }
            log.flash_mut().fail_op_after(nth);
            assert!(
                log.append(&key(3), 3, 1, &body(3, body_len)).await.is_err(),
                "nth={nth}: the injected failure must reach the caller"
            );
            assert!(!log.flash_mut().failure_armed(), "nth={nth}");
            if log.sector_room() == 0 {
                sealed += 1;
            }

            // Consistent where it stands: the records already accepted are
            // all there, all intact, and none of them moved.
            let recs = collect(&mut log).await;
            assert_eq!(recs.len(), 3, "nth={nth}");
            for (n, rec) in recs.iter().enumerate() {
                let mut out = vec![0u8; rec.len as usize];
                log.read_body(rec, &mut out).await.unwrap();
                assert_eq!(out, body(n as u32, body_len), "nth={nth}");
            }

            // And the retry lands — with different bytes than the record
            // that failed.
            log.append(&key(4), 4, 9, &body(4, body_len)).await.unwrap();

            let sim = log.into_flash();
            let mut log = RecordLog::open(sim, 0, region).await.unwrap();
            let recs = collect(&mut log).await;
            let keys: Vec<[u8; KEY_LEN]> = recs.iter().map(|r| r.key).collect();
            assert_eq!(keys, vec![key(0), key(1), key(2), key(4)], "nth={nth}");
            for rec in &recs {
                let n = u32::from_le_bytes(rec.key[0..4].try_into().unwrap());
                let mut out = vec![0u8; rec.len as usize];
                log.read_body(rec, &mut out).await.unwrap();
                assert_eq!(out, body(n, body_len), "nth={nth}");
            }
            assert!(
                log.flash_mut().max_word_writes() <= WRITES_PER_WORD,
                "nth={nth}"
            );
        }

        // What the sealing rule costs, as a number rather than a promise:
        // of the twenty places a timeout can land in this append, three
        // leave the page usable (the reclaim's erase, its page header, and
        // the first program run — none of which has put a byte of the record
        // down) and seventeen give up the rest of the page.
        assert_eq!(sealed, 17);
    });
}

#[test]
fn cancel_safety_is_the_power_cut_case() {
    // The store is async, so a `select!` can drop an append at any `.await`.
    // Over `Yielding` every flash operation is such a point. Dropping the
    // future at each of them in turn must leave exactly what a power cut at
    // the same place leaves: every committed record, no partial one, and a
    // store the next append still lands in.
    block_on(async {
        let sectors = 4u32;
        let region = sectors * SECTOR_SIZE;
        let mut points = 0usize;
        for stop in 1..=4usize {
            let mut log = RecordLog::open(Yielding::new(SimNor::new(sectors)), 0, region)
                .await
                .unwrap();
            for i in 0..2u32 {
                log.append(&key(i), i, 1, &body(i, 200)).await.unwrap();
            }

            let k = key(2);
            let b = body(2, 200);
            {
                // Poll the append `stop` times and drop it where it stands.
                let mut fut = core::pin::pin!(log.append(&k, 2, 1, &b));
                let mut cx = core::task::Context::from_waker(core::task::Waker::noop());
                for _ in 0..stop {
                    assert!(
                        fut.as_mut().poll(&mut cx).is_pending(),
                        "stop={stop}: the append finished before the drop"
                    );
                }
            }
            points += 1;

            // No repair pass, no recovery call: just reopen, exactly as a
            // reboot would.
            let flash = log.into_flash().into_inner();
            let mut log = RecordLog::open(flash, 0, region).await.unwrap();
            let recs = collect(&mut log).await;
            assert_eq!(recs.len(), 2, "stop={stop}: the cancelled record is absent");
            for (n, rec) in recs.iter().enumerate() {
                let mut out = vec![0u8; rec.len as usize];
                log.read_body(rec, &mut out).await.unwrap();
                assert_eq!(out, body(n as u32, 200), "stop={stop}");
            }
            log.append(&key(3), 3, 1, &body(3, 200)).await.unwrap();
            assert_eq!(collect(&mut log).await.len(), 3, "stop={stop}");
        }
        assert_eq!(points, 4);
    });
}

// ------------------------------------------------------------ the wear pin

#[test]
fn wear_is_level_after_wrapping_the_part_twice() {
    block_on(async {
        let per_sector = (SECTOR_PAYLOAD as usize / record_stride(FIELD_BODY)) as u32;
        let laps = 3u32;
        let mut log = fresh(SECTORS).await;
        for i in 0..laps * SECTORS * per_sector {
            log.append(&key(i), i, 0, &body(i, FIELD_BODY))
                .await
                .unwrap();
        }
        let sim = log.into_flash();
        let counts = sim.erase_counts();
        assert!(
            counts.iter().copied().min().unwrap() >= 2,
            "the part must have been wrapped at least twice: {counts:?}"
        );
        assert_eq!(
            wear_spread(counts),
            0,
            "round-robin reclaim erases every page the same number of times: {counts:?}"
        );
    });
}

#[test]
fn a_pinned_metadata_page_fails_the_wear_check() {
    block_on(async {
        // The negative control, and the endurance argument made executable.
        // The part is one page larger than the log's region; page 0 stands
        // in for the fixed index page a reference-shaped store would
        // rewrite on every accepted record. The same workload, the same
        // wear check, and it fails — which is what makes the passing case
        // above a measurement rather than a tautology.
        let per_sector = (SECTOR_PAYLOAD as usize / record_stride(FIELD_BODY)) as u32;
        let laps = 3u32;
        let records = laps * SECTORS * per_sector;

        let sim = SimNor::new(SECTORS + 1);
        let mut log = RecordLog::open(sim, SECTOR_SIZE, REGION).await.unwrap();
        for i in 0..records {
            log.append(&key(i), i, 0, &body(i, FIELD_BODY))
                .await
                .unwrap();
            log.flash_mut().erase(0, SECTOR_SIZE).await.unwrap();
        }
        let sim = log.into_flash();
        let counts = sim.erase_counts();

        assert_eq!(counts[0], records, "the pinned page takes one erase each");
        assert_eq!(counts[0], 528);
        assert_eq!(counts[1], laps, "and the log's own pages take one per lap");
        assert!(
            wear_spread(counts) > 1,
            "the wear check has to fail here: {counts:?}"
        );
        // And the log's own pages are untouched by the neighbour's fate.
        assert_eq!(wear_spread(&counts[1..]), 0, "{counts:?}");
        // The ratio is the whole argument: at the measured field duty the
        // pinned page burns its 10 000 cycles in eighteen days while the
        // log burns three.
        assert!(counts[0] / counts[1] > 100, "{counts:?}");
    });
}

#[test]
fn word_writes_between_erases_never_exceed_two() {
    // The constraint the internal flash adds and the external part did not
    // have: a word accepts two writes between erases, no more. The workload
    // that maximises it is the one where every record is committed and then
    // withdrawn — commit word written twice — on the smallest stride the
    // format allows, so as many of them as possible sit in one page.
    block_on(async {
        let mut log = fresh(SECTORS).await;
        let mut written = 0u32;
        while log.sector_room() >= MIN_STRIDE {
            log.append(&key(written), written, 0, &[]).await.unwrap();
            written += 1;
        }
        assert_eq!(log.active_sector(), 0, "the page must not have rolled");
        assert_eq!(written, 92, "44-byte stride, 4084 bytes of payload");
        for record in collect(&mut log).await {
            log.purge(&record).await.unwrap();
        }

        let sim = log.into_flash();
        assert_eq!(
            sim.max_word_writes(),
            WRITES_PER_WORD,
            "a word is written at most twice between erases; the nRF52840 \
             allows no third. Re-derive rather than relax: this is the \
             number the part holds the format to."
        );
        // And the two are the commit and the purge of one word, not two
        // writes of some header word: exactly one word per record has two.
        let twos = sim.word_writes().iter().filter(|w| **w == 2).count();
        assert_eq!(twos, written as usize, "{twos} words written twice");
    });
}

#[test]
fn mount_refuses_to_format_and_open_does_it() {
    block_on(async {
        // The firmware's boot probe reads a region that may already hold
        // somebody else's data, so asking the question must not be an act.
        // `is_formatted` borrows the device, which is what lets this assert
        // both halves: the answer, and that asking cost nothing.
        let mut sim = SimNor::new(SECTORS);
        assert!(!is_formatted(&mut sim, 0, REGION).await.unwrap());
        assert_eq!(
            sim.spent(),
            0,
            "probing an unformatted region writes nothing"
        );
        assert!(sim.bytes().iter().all(|b| *b == ERASED));
        assert!(sim.erase_counts().iter().all(|c| *c == 0));

        // `mount` on the same region agrees and hands back nothing to mount.
        assert!(RecordLog::mount(sim, 0, REGION).await.unwrap().is_none());

        // `open` is the one that formats: one erase, one page header.
        let mut log = RecordLog::open(SimNor::new(SECTORS), 0, REGION)
            .await
            .unwrap();
        log.append(&key(0), 0, 3, &body(0, 40)).await.unwrap();
        let mut sim = log.into_flash();
        assert_eq!(sim.erase_counts()[0], 1);
        assert!(sim.erase_counts()[1..].iter().all(|c| *c == 0));

        // And now `mount` finds it, without writing anything either.
        assert!(is_formatted(&mut sim, 0, REGION).await.unwrap());
        let spent = sim.spent();
        let mut mounted = RecordLog::mount(sim, 0, REGION).await.unwrap().unwrap();
        assert_eq!(mounted.count().await.unwrap(), (1, 0));
        let recs = collect(&mut mounted).await;
        assert_eq!(recs[0].tag, 3);
        assert_eq!(mounted.flash_mut().spent(), spent, "a mount is a read");
    });
}

#[test]
fn count_separates_live_from_purged() {
    block_on(async {
        let mut log = fresh(SECTORS).await;
        for i in 0..5u32 {
            log.append(&key(i), i, 0, &body(i, 24)).await.unwrap();
        }
        let recs = collect(&mut log).await;
        log.purge(&recs[1]).await.unwrap();
        log.purge(&recs[3]).await.unwrap();
        assert_eq!(log.count().await.unwrap(), (3, 2));
    });
}

// ------------------------------------------------------- foreign content

/// Plant `content` over the whole region, then assert that opening it
/// neither panics nor hangs and leaves a store that works.
async fn foreign_region_is_survivable(content: &[u8], expect_formatted_before: bool) {
    let mut sim = SimNor::new(SECTORS);
    sim.plant(0, content);
    assert_eq!(
        is_formatted(&mut sim, 0, REGION).await.unwrap(),
        expect_formatted_before
    );

    let mut log = RecordLog::open(sim, 0, REGION).await.unwrap();
    // Whatever it made of the region, it must hold no record it did not
    // write, and it must take new ones.
    for i in 0..20u32 {
        log.append(&key(i), i, 1, &body(i, FIELD_BODY))
            .await
            .unwrap();
    }
    let sim = log.into_flash();
    let mut log = RecordLog::open(sim, 0, REGION).await.unwrap();
    let recs = collect(&mut log).await;
    assert_eq!(recs.len(), 20);
    for (n, rec) in recs.iter().enumerate() {
        assert_eq!(rec.key, key(n as u32));
        let mut out = vec![0u8; rec.len as usize];
        log.read_body(rec, &mut out).await.unwrap();
        assert_eq!(out, body(n as u32, FIELD_BODY));
    }
}

#[test]
fn random_bytes_in_the_region_are_not_records() {
    block_on(async {
        foreign_region_is_survivable(&noise(0x5EED, REGION as usize), false).await;
    });
}

#[test]
fn a_region_of_zeros_is_not_records() {
    // The hard one for a format that reads 0xFF as "empty": every byte is
    // clear, so every flags byte and every length reads as *something*.
    block_on(async {
        foreign_region_is_survivable(&vec![0u8; REGION as usize], false).await;
    });
}

#[test]
fn a_page_of_plausible_headers_is_sealed_rather_than_believed() {
    // The nastiest foreign content is our own format with the wrong bytes
    // after it: a valid page header, a high sequence, and garbage where the
    // records should be. Mounting must terminate, must yield no record, and
    // must leave the region writable.
    block_on(async {
        let mut sim = SimNor::new(SECTORS);
        sim.plant(0, &noise(0xC0FFEE, REGION as usize));

        // A page header this log would accept, on page 3, claiming to be
        // the newest page there is.
        let page = 3 * SECTOR_SIZE;
        let mut header = [0u8; SECTOR_HEADER_LEN as usize];
        header[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        header[4..8].copy_from_slice(&0xFFFF_FF00u32.to_le_bytes());
        header[8] = VERSION;
        header[9] = 0;
        let crc = crc16_update(CRC_INIT, &header[0..10]);
        header[10..12].copy_from_slice(&crc.to_le_bytes());
        sim.plant(page, &header);

        assert!(
            is_formatted(&mut sim, 0, REGION).await.unwrap(),
            "the header is one we would have written, so we do believe it"
        );
        let mut log = RecordLog::open(sim, 0, REGION).await.unwrap();
        assert_eq!(log.active_sector(), 3);
        assert_eq!(collect(&mut log).await.len(), 0, "garbage is not records");
        assert_eq!(
            log.sector_room(),
            0,
            "a page whose tail is not erased is sealed, not written into"
        );

        // And the store still works: the next append rolls onto page 4.
        log.append(&key(0), 0, 1, &body(0, FIELD_BODY))
            .await
            .unwrap();
        assert_eq!(log.active_sector(), 4);
        let recs = collect(&mut log).await;
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].key, key(0));
    });
}
