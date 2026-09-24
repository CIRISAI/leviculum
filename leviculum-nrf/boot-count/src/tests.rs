//! Host tests for the boot-record page.
//!
//! Everything here runs against [`Page`], a simulated NOR page with the
//! two properties that make this format's argument work: programming only
//! clears bits, and an erase is the only way to raise one. Programming a
//! slot that is not erased is a panic rather than a silent AND, because
//! doing it is the defect the torn-write rule exists to prevent — a test
//! that merely produced wrong bytes would leave it to an assertion
//! somewhere else to notice.

use super::*;

/// A page of simulated internal flash, plus the erase counter the wear
/// arithmetic is asserted against.
struct Page {
    bytes: [u8; PAGE_SIZE],
    erases: u32,
}

impl Page {
    /// A page as the NVMC leaves it after an erase.
    fn erased() -> Self {
        Page {
            bytes: [ERASED; PAGE_SIZE],
            erases: 0,
        }
    }

    fn erase(&mut self) {
        self.bytes = [ERASED; PAGE_SIZE];
        self.erases += 1;
    }

    /// Program bytes at `offset`. Panics if the target is not erased.
    fn program(&mut self, offset: usize, data: &[u8]) {
        let target = &mut self.bytes[offset..offset + data.len()];
        assert!(
            target.iter().all(|b| *b == ERASED),
            "programmed over a slot that was not erased, at offset {offset}"
        );
        target.copy_from_slice(data);
    }

    /// One boot: plan against the page's current contents, then carry the
    /// plan out exactly as `leviculum-nrf/src/boot_count.rs` does.
    fn boot(&mut self, reset_reason: u32, retained: bool) -> Plan {
        let plan = plan(&self.bytes, reset_reason, retained);
        if plan.erase_first {
            self.erase();
        }
        self.program(plan.offset, &encode(&plan.record));
        plan
    }

    /// A boot whose record was cut off after `words` of its four NVMC
    /// words landed: the prefix is programmed, the rest stays erased.
    fn torn_boot(&mut self, words: usize) {
        let plan = plan(&self.bytes, 0, false);
        if plan.erase_first {
            self.erase();
        }
        let encoded = encode(&plan.record);
        self.program(plan.offset, &encoded[..words * 4]);
    }

    fn slot(&self, index: usize) -> &[u8] {
        &self.bytes[index * RECORD_SIZE..(index + 1) * RECORD_SIZE]
    }
}

// ------------------------------------------------------------ the record

#[test]
fn the_page_is_tiled_exactly_by_its_records() {
    assert_eq!(RECORD_SIZE % 4, 0, "a record is whole NVMC words");
    assert_eq!(RECORDS_PER_PAGE * RECORD_SIZE, PAGE_SIZE);
    assert_eq!(RECORDS_PER_PAGE, 256);
}

#[test]
fn a_record_round_trips() {
    let record = BootRecord {
        boot: 42,
        reset_reason: 0x0000_0004,
        retained: true,
    };
    assert_eq!(decode(&encode(&record)), Some(record));
}

#[test]
fn the_reset_reason_bits_round_trip() {
    // Every bit POWER.RESETREAS assigns (0-3 and 16-20), one at a time,
    // plus the all-zero value a power-on or brownout leaves, every
    // assigned bit at once, and the all-ones value an erased slot holds.
    let mut reasons = vec![0x0000_0000u32];
    for bit in [0u32, 1, 2, 3, 16, 17, 18, 19, 20] {
        reasons.push(1 << bit);
    }
    reasons.push(0x001F_000F);
    reasons.push(0xFFFF_FFFF);
    for raw in reasons {
        for retained in [false, true] {
            let record = BootRecord {
                boot: 7,
                reset_reason: raw,
                retained,
            };
            let read = decode(&encode(&record)).expect("own record reads back");
            assert_eq!(read.reset_reason, raw);
            assert_eq!(read.retained, retained);
        }
    }
}

#[test]
fn an_erased_slot_is_not_a_record() {
    assert_eq!(decode(&[ERASED; RECORD_SIZE]), None);
}

#[test]
fn a_slot_with_the_wrong_magic_is_not_a_record() {
    let mut bytes = encode(&BootRecord {
        boot: 3,
        reset_reason: 0,
        retained: false,
    });
    bytes[0] ^= 0x01;
    assert_eq!(decode(&bytes), None);
}

#[test]
fn a_slot_from_another_layout_version_is_not_a_record() {
    let mut bytes = encode(&BootRecord {
        boot: 3,
        reset_reason: 0,
        retained: false,
    });
    bytes[13] = FORMAT_VERSION + 1;
    assert_eq!(decode(&bytes), None);
}

#[test]
fn a_single_flipped_bit_anywhere_fails_the_crc() {
    let bytes = encode(&BootRecord {
        boot: 0x1234_5678,
        reset_reason: 0x0000_0002,
        retained: true,
    });
    for byte in 0..RECORD_SIZE {
        for bit in 0..8 {
            let mut damaged = bytes;
            damaged[byte] ^= 1 << bit;
            assert_eq!(
                decode(&damaged),
                None,
                "byte {byte} bit {bit} passed validation"
            );
        }
    }
}

#[test]
fn crc16_matches_the_standard_check_value() {
    // CRC-16/CCITT-FALSE's published check value, and the same vector
    // `leviculum_core::framing`'s and the record log's own tests use. If
    // this disagrees, the three implementations have drifted.
    assert_eq!(crc16(b"123456789"), 0x29B1);
}

// -------------------------------------------------------------- the page

#[test]
fn the_first_boot_on_an_erased_page_is_boot_one() {
    let mut page = Page::erased();
    let plan = page.boot(0x0000_0000, false);
    assert!(!plan.erase_first);
    assert_eq!(plan.offset, 0);
    assert_eq!(plan.record.boot, 1);
    assert_eq!(plan.since_erase, 1);
    assert_eq!(page.erases, 0, "an erased page is not erased again");
}

#[test]
fn each_boot_appends_one_slot_and_bumps_the_number() {
    let mut page = Page::erased();
    for expected in 1..=10u32 {
        let plan = page.boot(0x0000_0004, true);
        assert_eq!(plan.record.boot, expected);
        assert_eq!(plan.offset, (expected as usize - 1) * RECORD_SIZE);
        assert_eq!(plan.since_erase, expected);
        assert!(!plan.erase_first);
    }
    assert_eq!(page.erases, 0);
    let scan = scan(&page.bytes);
    assert_eq!(scan.records, 10);
    assert_eq!(scan.last.map(|r| r.boot), Some(10));
    assert_eq!(scan.free, Some(10 * RECORD_SIZE));
}

#[test]
fn a_full_page_is_erased_and_the_boot_number_carries_forward() {
    let mut page = Page::erased();
    for _ in 0..RECORDS_PER_PAGE {
        let plan = page.boot(0, false);
        assert!(!plan.erase_first);
    }
    assert_eq!(page.erases, 0, "the page fills without a single erase");
    assert_eq!(scan(&page.bytes).free, None);

    // The boot that finds it full: one erase, back to slot 0, and the
    // count continues where the spent page left off.
    let plan = page.boot(0, false);
    assert!(plan.erase_first);
    assert_eq!(plan.offset, 0);
    assert_eq!(plan.since_erase, 1);
    assert_eq!(plan.record.boot, RECORDS_PER_PAGE as u32 + 1);
    assert_eq!(page.erases, 1);
    assert_eq!(
        scan(&page.bytes).records,
        1,
        "the erase dropped the history and kept the count"
    );

    // And the next lap carries it again.
    for _ in 1..RECORDS_PER_PAGE {
        page.boot(0, false);
    }
    let plan = page.boot(0, false);
    assert!(plan.erase_first);
    assert_eq!(page.erases, 2);
    assert_eq!(plan.record.boot, 2 * RECORDS_PER_PAGE as u32 + 1);
}

#[test]
fn a_partial_record_from_a_cut_is_not_counted() {
    // Three clean boots, then one that lost power after two of its four
    // words landed.
    let mut page = Page::erased();
    for _ in 0..3 {
        page.boot(0, false);
    }
    page.torn_boot(2);

    let scan = scan(&page.bytes);
    assert_eq!(scan.records, 3, "the torn record is not a boot");
    assert_eq!(scan.last.map(|r| r.boot), Some(3));
    assert_eq!(
        scan.free,
        Some(4 * RECORD_SIZE),
        "the torn slot is retired, not overwritten"
    );

    // The next boot is 4 — the torn one never counted — and it lands past
    // the retired slot. `Page::program` panics if it did not.
    let plan = page.boot(0x0000_0001, true);
    assert_eq!(plan.record.boot, 4);
    assert_eq!(plan.offset, 4 * RECORD_SIZE);
    assert_eq!(
        plan.since_erase, 5,
        "the retired slot is spent all the same"
    );
    assert_eq!(decode(page.slot(4)).map(|r| r.boot), Some(4));
}

#[test]
fn a_cut_at_every_word_boundary_leaves_the_count_readable() {
    for words in 0..4 {
        let mut page = Page::erased();
        for _ in 0..5 {
            page.boot(0, false);
        }
        page.torn_boot(words);
        let scan = scan(&page.bytes);
        assert_eq!(scan.records, 5, "words={words}");
        assert_eq!(scan.last.map(|r| r.boot), Some(5), "words={words}");
        // A cut before the first word landed leaves the slot erased, so
        // it is the free one; anything else retires it.
        let expected_free = if words == 0 { 5 } else { 6 };
        assert_eq!(
            scan.free,
            Some(expected_free * RECORD_SIZE),
            "words={words}"
        );
    }
}

#[test]
fn a_torn_record_in_the_last_slot_makes_the_page_full() {
    let mut page = Page::erased();
    for _ in 0..RECORDS_PER_PAGE - 1 {
        page.boot(0, false);
    }
    page.torn_boot(1);
    assert_eq!(scan(&page.bytes).free, None);

    let plan = page.boot(0, false);
    assert!(plan.erase_first);
    assert_eq!(
        plan.record.boot, RECORDS_PER_PAGE as u32,
        "the count follows the last READABLE record, not the slot index"
    );
}

#[test]
fn a_page_of_foreign_bytes_is_erased_rather_than_parsed() {
    // The page's address was inside the linker's FLASH region until this
    // batch, so a board may carry application bytes there. Two slots of
    // them are enough to make the page unappendable, and the first boot
    // that meets them erases it and starts at 1.
    let mut page = Page::erased();
    page.program(0, &[0xA5; 2 * RECORD_SIZE]);

    let plan = page.boot(0x0000_0004, false);
    assert!(plan.erase_first);
    assert_eq!(plan.offset, 0);
    assert_eq!(plan.record.boot, 1);
    assert_eq!(page.erases, 1);
    assert_eq!(scan(&page.bytes).records, 1);
}

#[test]
fn foreign_bytes_in_one_slot_only_are_retired_like_a_torn_write() {
    let mut page = Page::erased();
    page.boot(0, false);
    page.program(RECORD_SIZE, &[0x5A; RECORD_SIZE]);

    let plan = page.boot(0, false);
    assert!(!plan.erase_first);
    assert_eq!(plan.offset, 2 * RECORD_SIZE);
    assert_eq!(plan.record.boot, 2);
}

// ----------------------------------------------------------- the wear bill

#[test]
fn one_erase_cycle_covers_two_hundred_and_fifty_six_boots() {
    // The claim the module comment makes, as a measurement: fill the page
    // twice over and count the erases the plans asked for.
    let mut page = Page::erased();
    for _ in 0..2 * RECORDS_PER_PAGE {
        page.boot(0, false);
    }
    assert_eq!(page.erases, 1, "256 boots per erase cycle");
    assert_eq!(scan(&page.bytes).records, RECORDS_PER_PAGE);

    // Two more laps, to show the rate does not depend on where it started.
    for _ in 0..2 * RECORDS_PER_PAGE {
        page.boot(0, false);
    }
    assert_eq!(page.erases, 3);

    // Negative control: rewriting a single fixed record instead — one
    // erase per boot — is what those 1024 boots would have cost.
    let mut fixed = Page::erased();
    for _ in 0..4 * RECORDS_PER_PAGE {
        fixed.erase();
        fixed.program(
            0,
            &encode(&BootRecord {
                boot: 1,
                reset_reason: 0,
                retained: false,
            }),
        );
    }
    assert_eq!(
        fixed.erases,
        4 * RECORDS_PER_PAGE as u32,
        "1024 boots, 1024 erases"
    );
    // 1024 boots cost three erases here and 1024 there. Three and not
    // four because the first lap runs on a page that was already erased —
    // which is why the ratio is a lower bound of 256 rather than exactly
    // 256, and the bound is what the endurance arithmetic below uses.
    assert_eq!(page.erases, 3);
    assert!(fixed.erases / page.erases >= RECORDS_PER_PAGE as u32);
}

#[test]
fn the_rated_endurance_buys_two_and_a_half_million_boots() {
    assert_eq!(BOOTS_PER_ENDURANCE, 2_560_000);
    assert_eq!(
        BOOTS_PER_ENDURANCE,
        RECORDS_PER_PAGE as u32 * ENDURANCE_CYCLES
    );
    // What the same budget buys a counter rewritten at a fixed address:
    // one boot per erase cycle, 10 000 boots. A board that is reset
    // twenty times a day on a bench reaches that inside two years.
    assert_eq!(BOOTS_PER_ENDURANCE / ENDURANCE_CYCLES, 256);
    // At ten boots a day — a field board that restarts far more often
    // than ours do — the page outlives everything around it.
    assert_eq!(BOOTS_PER_ENDURANCE / (10 * 365), 701);
}

#[test]
fn the_counter_outlives_the_page_it_lives_on() {
    // Why `wrapping_add` on the boot number is not a question a board
    // reaches: the flash is spent three orders of magnitude earlier.
    assert_eq!(u32::MAX / BOOTS_PER_ENDURANCE, 1_677);
}

// ------------------------------------------------------------- the line

#[test]
fn the_boot_count_line_is_byte_exact() {
    let line = BootCountLine {
        record: BootRecord {
            boot: 42,
            reset_reason: 0x0000_0004,
            retained: true,
        },
        since_erase: 17,
    };
    assert_eq!(
        format!("{line}"),
        "BOOT_COUNT n=42 reset_reason=0x00000004 retained=1 since_erase=17"
    );
}

#[test]
fn the_line_says_zero_for_lost_retained_ram() {
    // The field case: a power loss latches no RESETREAS bit and takes
    // retained RAM with it, so both fields read the same way every time
    // and the line is the only place the restart is recorded at all.
    let line = BootCountLine {
        record: BootRecord {
            boot: 3,
            reset_reason: 0,
            retained: false,
        },
        since_erase: 3,
    };
    assert_eq!(
        format!("{line}"),
        "BOOT_COUNT n=3 reset_reason=0x00000000 retained=0 since_erase=3"
    );
}
