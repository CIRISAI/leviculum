//! The decisions the board's store task makes around the log (leviculum#413,
//! the stores section of `docs/src/firmware/host-test-seams.md`).
//!
//! `leviculum-nrf/src/record_store.rs` owns the region, the SoftDevice flash
//! handle and the one task allowed to append. What that task decides is here,
//! so a torn page, a region at its limit or a part that refuses an erase is a
//! host test over [`crate::sim::SimNor`] instead of a field event read off a
//! board that has since rebooted:
//!
//! - [`mount_or_format`]: what a boot does with the region it finds, and
//!   which failure leaves the board without a store until the next one.
//! - [`sealed_a_page`]: which failed append counts a page given up.
//! - [`retry_after`]: how often a refused flash operation is tried, and how
//!   long the store waits between tries.
//! - [`storm_admits`] and [`storm_record`]: what the `--store-storm` bench
//!   instrument refuses and what each synthetic record carries.

use embedded_storage_async::nor_flash::NorFlash;

use crate::{is_formatted, Error, RecordLog, KEY_LEN, MAX_BODY};

/// What the boot found on the region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountState {
    /// A log of ours: mounted as it was, nothing written.
    Ours,
    /// No page carried our header (a fresh part, or somebody else's bytes):
    /// one erase and one page header made it ours.
    Formatted,
}

impl MountState {
    /// The `state=` value of the `STORE mount` line.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ours => "ours",
            Self::Formatted => "formatted",
        }
    }
}

/// A mounted store and what the mount saw.
pub struct Mounted<F> {
    /// The log, ready to append.
    pub log: RecordLog<F>,
    /// Whether the region was ours or had to be formatted.
    pub state: MountState,
    /// Live records on the part after the mount, the number that says
    /// whether the reboot found anything.
    pub live: u32,
}

/// Mount the region, formatting it once if no log of ours wrote it, and
/// count what is on it.
///
/// A fresh part and a region holding a stranger's bytes take the same path
/// on purpose: the region is inside the board's own image window, so there
/// is no stranger whose data would be protected by refusing. The question
/// whether it is ours is asked first and costs reads only, so the decision
/// to write is this function's rather than a side effect of opening.
///
/// Any error leaves the board without a store for this boot: the region does
/// not fit the part, the format's erase or header write was refused, or the
/// scan that counts the records failed. The caller then refuses every
/// request rather than queueing it into a channel nothing reads.
pub async fn mount_or_format<F: NorFlash>(
    mut flash: F,
    base: u32,
    len: u32,
) -> Result<Mounted<F>, Error<F::Error>> {
    let state = if is_formatted(&mut flash, base, len).await? {
        MountState::Ours
    } else {
        MountState::Formatted
    };
    let mut log = RecordLog::open(flash, base, len).await?;
    let (live, _purged) = log.count().await?;
    Ok(Mounted { log, state, live })
}

/// Whether a failed append gave up a page: the active page had room before
/// the append and none after it.
///
/// The log seals a page it cannot trust any more by moving its cursor to the
/// end, so the next append starts on a fresh page. A failure that changed
/// nothing on the page (the part refused the first operation, or the body
/// was too large) leaves the room as it was and seals nothing.
pub const fn sealed_a_page(room_before: u32, room_after: u32) -> bool {
    room_before > 0 && room_after == 0
}

/// How many times one flash operation is attempted before the store gives up
/// on it.
///
/// The SoftDevice schedules flash work between radio events and fails the
/// operation outright when it finds no gap (S140 SDS, Flash API timing), so a
/// refusal is a statement about the next few milliseconds of radio traffic
/// and not about the part. Four attempts with a doubling delay walk off a
/// busy window; hammering at a fixed interval can sit inside one.
pub const FLASH_ATTEMPTS: u8 = 4;

/// Delay before the second attempt, doubled for each further one: 50, 100,
/// 200 ms.
pub const FLASH_BACKOFF_MS: u64 = 50;

/// After attempt `attempt` (1-based) was refused: `Some(ms)` to wait before
/// the next one, `None` to give up and hand the refusal to the log.
///
/// No wait follows the last attempt; the log's answer to the refusal (a
/// sealed page, an op the engine forgets) is due at once.
pub const fn retry_after(attempt: u8) -> Option<u64> {
    if attempt == 0 || attempt >= FLASH_ATTEMPTS {
        return None;
    }
    Some(FLASH_BACKOFF_MS << (attempt - 1))
}

/// Whether the store takes a storm of `records` synthetic records of `size`
/// bytes.
///
/// Refused while unmounted (nothing to append to), while a storm is running
/// (the instrument measures one storm at a time), for zero records, and for
/// a body no page can hold. The envelope classifier bounds both numbers on
/// every binary; the body check is repeated here so a caller that is not the
/// envelope cannot get past it.
pub const fn storm_admits(mounted: bool, running: bool, records: u16, size: u16) -> bool {
    mounted && !running && records > 0 && size as usize <= MAX_BODY
}

/// Shape synthetic record `n` of a storm into `body` and return its key and
/// the body length to append.
///
/// The length is `size` clamped to the buffer. The key is the record's place
/// in the storm and its size, so a later reader can order the records
/// without parsing a body. A non-empty body carries the index in its first
/// and last byte, so two records differ and a CRC failure cannot hide behind
/// every record being identical.
pub fn storm_record(n: u16, size: u16, body: &mut [u8]) -> ([u8; KEY_LEN], usize) {
    let len = core::cmp::min(size as usize, body.len());
    let mut key = [0u8; KEY_LEN];
    key[0..2].copy_from_slice(&n.to_be_bytes());
    key[2..4].copy_from_slice(&(len as u16).to_be_bytes());
    if len > 0 {
        body[0] = n as u8;
        body[len - 1] = (n >> 8) as u8;
    }
    (key, len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{block_on, SimError, SimNor};
    use crate::SECTOR_SIZE;
    use alloc::vec;

    const SECTORS: u32 = 4;
    const REGION: u32 = SECTORS * SECTOR_SIZE;

    fn key(n: u8) -> [u8; KEY_LEN] {
        [n; KEY_LEN]
    }

    #[test]
    fn a_fresh_part_is_formatted_and_counts_nothing() {
        block_on(async {
            let mounted = mount_or_format(SimNor::new(SECTORS), 0, REGION)
                .await
                .unwrap();
            assert_eq!(mounted.state, MountState::Formatted);
            assert_eq!(mounted.live, 0);
        });
    }

    #[test]
    fn a_strangers_bytes_are_formatted_over() {
        block_on(async {
            let mut sim = SimNor::new(SECTORS);
            // Somebody else's data on every page: no header of ours.
            for page in 0..SECTORS {
                sim.plant(page * SECTOR_SIZE, &[0x5A; 64]);
            }
            let mounted = mount_or_format(sim, 0, REGION).await.unwrap();
            assert_eq!(mounted.state, MountState::Formatted);
            assert_eq!(mounted.live, 0);
        });
    }

    #[test]
    fn a_reboot_finds_its_own_records() {
        block_on(async {
            let mut log = RecordLog::open(SimNor::new(SECTORS), 0, REGION)
                .await
                .unwrap();
            for n in 0..3 {
                log.append(&key(n), 0, 0, &[n; 40]).await.unwrap();
            }
            let sim = log.into_flash();
            let erases_before: u32 = sim.erase_counts().iter().sum();
            let mounted = mount_or_format(sim, 0, REGION).await.unwrap();
            assert_eq!(mounted.state, MountState::Ours);
            assert_eq!(mounted.live, 3);
            let erases_after: u32 = mounted.log.into_flash().erase_counts().iter().sum();
            assert_eq!(erases_after, erases_before, "an own region is not erased");
        });
    }

    #[test]
    fn a_record_torn_by_a_power_cut_is_not_counted() {
        block_on(async {
            let mut log = RecordLog::open(SimNor::new(SECTORS), 0, REGION)
                .await
                .unwrap();
            log.append(&key(1), 0, 0, &[1; 40]).await.unwrap();
            // The battery goes in the middle of the second record.
            log.flash_mut().arm_power_cut(4);
            assert!(log.append(&key(2), 0, 0, &[2; 40]).await.is_err());
            let mut sim = log.into_flash();
            sim.power_on();
            let mounted = mount_or_format(sim, 0, REGION).await.unwrap();
            assert_eq!(mounted.state, MountState::Ours);
            assert_eq!(mounted.live, 1, "the torn record is no record");
        });
    }

    #[test]
    fn a_refused_format_leaves_no_store() {
        block_on(async {
            let mut sim = SimNor::new(SECTORS);
            // The format's erase finds no gap between radio events.
            sim.fail_next_op();
            let outcome = mount_or_format(sim, 0, REGION).await;
            assert!(matches!(outcome, Err(Error::Flash(SimError::Timeout))));
        });
    }

    #[test]
    fn a_region_that_does_not_fit_the_part_leaves_no_store() {
        block_on(async {
            let outcome = mount_or_format(SimNor::new(SECTORS), 0, REGION + SECTOR_SIZE).await;
            assert!(matches!(outcome, Err(Error::OutOfBounds)));
            let outcome = mount_or_format(SimNor::new(SECTORS), 0, SECTOR_SIZE).await;
            assert!(matches!(outcome, Err(Error::BadRegion)));
        });
    }

    #[test]
    fn an_append_refused_mid_record_seals_its_page() {
        block_on(async {
            let mut log = RecordLog::open(SimNor::new(SECTORS), 0, REGION)
                .await
                .unwrap();
            log.append(&key(1), 0, 0, &[1; 40]).await.unwrap();
            let before = log.sector_room();
            // The header lands, the body write is refused past every retry.
            log.flash_mut().fail_op_after(1);
            assert!(log.append(&key(2), 0, 0, &[2; 40]).await.is_err());
            assert!(sealed_a_page(before, log.sector_room()));
            // The next append starts on a fresh page and lands.
            log.append(&key(3), 0, 0, &[3; 40]).await.unwrap();
        });
    }

    #[test]
    fn an_append_refused_before_its_first_write_seals_nothing() {
        block_on(async {
            let mut log = RecordLog::open(SimNor::new(SECTORS), 0, REGION)
                .await
                .unwrap();
            log.append(&key(1), 0, 0, &[1; 40]).await.unwrap();
            let before = log.sector_room();
            log.flash_mut().fail_next_op();
            assert!(log.append(&key(2), 0, 0, &[2; 40]).await.is_err());
            assert!(!sealed_a_page(before, log.sector_room()));
        });
    }

    #[test]
    fn an_append_at_the_regions_limit_whose_reclaim_is_refused_seals_nothing() {
        block_on(async {
            let mut log = RecordLog::open(SimNor::new(SECTORS), 0, REGION)
                .await
                .unwrap();
            let body = vec![7u8; MAX_BODY];
            // Fill every page, so the next append must reclaim the oldest.
            for n in 0..SECTORS as u8 {
                log.append(&key(n), 0, 0, &body).await.unwrap();
            }
            let before = log.sector_room();
            // The reclaim's erase is refused: the active page is untouched.
            log.flash_mut().fail_next_op();
            assert!(log.append(&key(9), 0, 0, &body).await.is_err());
            assert!(!sealed_a_page(before, log.sector_room()));
        });
    }

    #[test]
    fn a_full_page_that_fails_is_not_sealed_a_second_time() {
        assert!(!sealed_a_page(0, 0));
        assert!(sealed_a_page(1, 0));
        assert!(!sealed_a_page(100, 40));
    }

    #[test]
    fn a_refused_operation_is_tried_four_times_with_a_doubling_wait() {
        assert_eq!(retry_after(1), Some(50));
        assert_eq!(retry_after(2), Some(100));
        assert_eq!(retry_after(3), Some(200));
        assert_eq!(retry_after(FLASH_ATTEMPTS), None);
        assert_eq!(retry_after(0), None);
    }

    /// The store task's loop over the schedule, against a part that refuses
    /// the first `refusals` attempts of one erase.
    fn erase_with_refusals(refusals: usize) -> (bool, alloc::vec::Vec<u64>) {
        block_on(async {
            let mut sim = SimNor::new(SECTORS);
            let mut waits = alloc::vec::Vec::new();
            let mut attempt = 1u8;
            loop {
                if usize::from(attempt) <= refusals {
                    sim.fail_next_op();
                }
                if sim.erase(0, SECTOR_SIZE).await.is_ok() {
                    return (true, waits);
                }
                match retry_after(attempt) {
                    Some(ms) => waits.push(ms),
                    None => return (false, waits),
                }
                attempt += 1;
            }
        })
    }

    #[test]
    fn an_erase_refused_three_times_lands_on_the_fourth() {
        assert_eq!(erase_with_refusals(3), (true, vec![50, 100, 200]));
    }

    #[test]
    fn an_erase_refused_four_times_is_given_up_without_a_last_wait() {
        assert_eq!(erase_with_refusals(4), (false, vec![50, 100, 200]));
    }

    #[test]
    fn a_storm_is_refused_unmounted_running_empty_or_oversize() {
        assert!(storm_admits(true, false, 1, 0));
        assert!(storm_admits(true, false, 100, MAX_BODY as u16));
        assert!(!storm_admits(false, false, 1, 10));
        assert!(!storm_admits(true, true, 1, 10));
        assert!(!storm_admits(true, false, 0, 10));
        assert!(!storm_admits(true, false, 1, MAX_BODY as u16 + 1));
    }

    #[test]
    fn a_storm_record_is_clamped_to_its_buffer_and_carries_its_index() {
        let mut body = [0xA5u8; 64];
        let (key, len) = storm_record(0x0102, 200, &mut body);
        assert_eq!(len, 64);
        assert_eq!(&key[0..4], &[0x01, 0x02, 0x00, 64]);
        assert_eq!(body[0], 0x02);
        assert_eq!(body[63], 0x01);

        let mut body = [0xA5u8; 64];
        let (key, len) = storm_record(7, 0, &mut body);
        assert_eq!(len, 0);
        assert_eq!(&key[0..4], &[0, 7, 0, 0]);
        assert!(
            body.iter().all(|&b| b == 0xA5),
            "an empty body is not stamped"
        );
    }

    #[test]
    fn a_storm_record_appends_and_reads_back_by_its_key() {
        block_on(async {
            let mut log = RecordLog::open(SimNor::new(SECTORS), 0, REGION)
                .await
                .unwrap();
            let mut body = [0xA5u8; 32];
            for n in 0..3u16 {
                let (key, len) = storm_record(n, 32, &mut body);
                log.append(&key, 0, 0xB0, &body[..len]).await.unwrap();
            }
            let mut keys = alloc::vec::Vec::new();
            log.for_each(|r| keys.push(r.key[1])).await.unwrap();
            assert_eq!(keys, vec![0, 1, 2]);
        });
    }
}
