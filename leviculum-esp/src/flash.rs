//! The board's stores in flash, found by label in the partition table
//! (1414): the node's identity and, on a Loquax handheld, the client
//! record that makes it a device of its home and (1476) the message store
//! its chats come back from after a power cycle
//! (`loquax::home::board::store`, over [`Messages`]).
//!
//! The same approach the Heltec node on the `esp` branch took
//! (`9bb5efbf`, "The ESP32 board keeps what it was told"): espflash writes
//! the table the crate ships (`partitions-m9.csv` for the M9), the image
//! looks each region up by its label at boot rather than by address, and
//! a board flashed with another table says `none/no-entry` and runs
//! without persistence instead of writing into somebody else's bytes.
//! When that branch lands the two converge on this file.
//!
//! Every buffer handed to the flash routines lives on the stack. The M9's
//! heap is PSRAM, which sits behind the same cache the flash routines turn
//! off while they run; a heap buffer there would be read with the cache
//! off.

use esp_bootloader_esp_idf::partitions::{self, PARTITION_TABLE_MAX_LEN};
use esp_hal::peripherals::FLASH;
use esp_storage::{FlashStorage, FlashStorageError};
use leviculum_core::identity::Identity;
use leviculum_core::identity_store::{self, ENCODED_SIZE_ALIGNED as IDENTITY_SIZE};
use loquax::home::board::store::{Sectors, SECTOR as STORE_SECTOR};
use loquax::home::record::{slots, ClientRecord, RecordError};

use crate::stall::{self, Core, Lock};

/// Run a flash write or erase, `f`, inside the cross-core critical
/// section, named in the stall record (1534).
///
/// esp-storage's AutoPark stalls the other core wherever it is
/// (`esp-storage-0.10.0/src/common.rs`, `pre_write`, then `post_write`
/// unparks). The UI core takes the one cross-core critical section for
/// every queue push and every key drain (its allocations have their own
/// pool and lock since 1594, `pools.rs`), so it is often parked holding
/// that spinlock. The
/// ROM call itself runs with core 0's interrupts off (esp-storage's own
/// lock), but they are on again between the op and the unpark, and the
/// keyboard's 10 ms tick, pending after any erase (about 45 ms), runs
/// there: its `critical_section::with` spins for a lock the parked core
/// holds, with interrupts off, and both cores are dead (the M9's hang of
/// 2026-11-07, UART silent until a hand reset). Holding the section
/// across the whole op closes both halves: the UI core cannot be parked
/// inside it (core 0 owns it), and no interrupt runs on core 0 until the
/// UI core is unparked. Core 0's interrupts were already off for the op
/// itself, so the latency added is the park and unpark around it.
///
/// Since 1507 the op also waits for the panel's DMA out of PSRAM to end
/// and holds the next one off ([`crate::mspi`]): the bus that DMA reads is
/// the one the op takes.
fn parked<R>(f: impl FnOnce() -> R) -> R {
    crate::mspi::flash(|| {
        stall::locked(Core::Io, Lock::Flash, || {
            let t0 = crate::uptime_us();
            let r = critical_section::with(|_| f());
            park::note(crate::uptime_us().saturating_sub(t0));
            r
        })
    })
}

/// What the flash ops cost the UI core (1569): each op holds the
/// cross-core critical section and parks the other core (AutoPark), so
/// the time inside `parked`'s section is the longest the UI core can
/// have been held per op. Cumulative counters; a reader takes deltas
/// (`[IDLE]` prints them per window), and the longest single park is
/// taken, not read, so each window says its own.
pub mod park {
    use core::sync::atomic::{AtomicU32, Ordering};

    static TOTAL_US: AtomicU32 = AtomicU32::new(0);
    static OPS: AtomicU32 = AtomicU32::new(0);
    static MAX_US: AtomicU32 = AtomicU32::new(0);

    pub(super) fn note(us: u64) {
        let us = u32::try_from(us).unwrap_or(u32::MAX);
        TOTAL_US.fetch_add(us, Ordering::Relaxed);
        OPS.fetch_add(1, Ordering::Relaxed);
        MAX_US.fetch_max(us, Ordering::Relaxed);
    }

    /// Microseconds inside flash ops and the ops, since boot, wrapping.
    pub fn so_far() -> (u32, u32) {
        (
            TOTAL_US.load(Ordering::Relaxed),
            OPS.load(Ordering::Relaxed),
        )
    }

    /// The longest single op since the last call.
    pub fn take_max_us() -> u32 {
        MAX_US.swap(0, Ordering::Relaxed)
    }
}

/// The partition the node's identity lives in.
pub const IDENTITY_LABEL: &str = "identity";
/// The partition the Loquax client record lives in.
pub const CLIENT_LABEL: &str = "loquax_client";
/// The record's second slot (1493): the record is written
/// copy-then-switch across the two, so a power loss during a save leaves
/// the other slot's record standing (`loquax::home::record::slots`).
pub const CLIENT_B_LABEL: &str = "loquax_client_b";
/// The partition the message store lives in (1476).
pub const MESSAGES_LABEL: &str = "messages";

/// `data`, the partition type every store is.
const TYPE_DATA: u8 = 0x01;
/// The erase unit of the part.
const SECTOR: u32 = FlashStorage::SECTOR_SIZE;

/// Why a store has no region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoRegion {
    /// The partition table did not read.
    NoTable,
    /// The table has no entry of the label.
    NoEntry,
    /// The entry is not a data partition.
    NotData,
    /// The entry is read-only.
    ReadOnly,
    /// The entry is off a sector boundary or shorter than one.
    Unfit,
}

impl core::fmt::Display for NoRegion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            NoRegion::NoTable => "no-table",
            NoRegion::NoEntry => "no-entry",
            NoRegion::NotData => "not-data",
            NoRegion::ReadOnly => "read-only",
            NoRegion::Unfit => "unfit",
        })
    }
}

/// Where a store lives, or why it has nowhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot(Result<u32, NoRegion>);

impl core::fmt::Display for Slot {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Ok(offset) => write!(f, "{offset:#08x}"),
            Err(reason) => write!(f, "none/{reason}"),
        }
    }
}

/// Why a store did not read or write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    /// The store has no region.
    NoRegion(NoRegion),
    /// The flash refused.
    Flash(FlashStorageError),
    /// The bytes there are not a record.
    Record(RecordError),
}

impl core::fmt::Display for StoreError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            StoreError::NoRegion(reason) => write!(f, "no-region/{reason}"),
            StoreError::Flash(error) => write!(f, "flash/{error:?}"),
            StoreError::Record(error) => write!(f, "record/{error:?}"),
        }
    }
}

/// A record's bytes on the stack, word aligned as the flash wants them.
#[repr(align(4))]
struct Aligned<const N: usize>([u8; N]);

/// What a record slot read moves: the slot, padded to whole words.
const SLOT_READ: usize = (slots::SLOT_LEN + 3) & !3;

/// The board's stores.
pub struct Stores<'d> {
    flash: FlashStorage<'d>,
    identity: Slot,
    client: Slot,
    /// The record's second slot (1493); `none/no-entry` on a board
    /// flashed with the old table, which saves in place as before.
    client_b: Slot,
    /// The message store's region: its offset and how many sectors.
    messages: Result<(u32, u32), NoRegion>,
}

impl<'d> Stores<'d> {
    /// Open the flash and find both stores in its partition table.
    pub fn open(flash: FLASH<'d>) -> Self {
        // The UI runs on the second core since 1493: every write and
        // erase parks it for the op (esp-storage's AutoPark), since the
        // flash routines run with the cache the core executes from off.
        // Never while it holds the critical section (1534): every write
        // and erase goes through `parked`.
        let mut flash = FlashStorage::new(flash).multicore_auto_park();
        let mut table = Aligned([0u8; PARTITION_TABLE_MAX_LEN]);
        let (identity, client, client_b, messages) =
            match partitions::read_partition_table(&mut flash, &mut table.0) {
                Err(_) => (
                    Err(NoRegion::NoTable),
                    Err(NoRegion::NoTable),
                    Err(NoRegion::NoTable),
                    Err(NoRegion::NoTable),
                ),
                Ok(table) => {
                    let mut identity = Err(NoRegion::NoEntry);
                    let mut client = Err(NoRegion::NoEntry);
                    let mut client_b = Err(NoRegion::NoEntry);
                    let mut messages = Err(NoRegion::NoEntry);
                    for entry in table.iter() {
                        let found = match entry.label_as_str() {
                            IDENTITY_LABEL => &mut identity,
                            CLIENT_LABEL => &mut client,
                            CLIENT_B_LABEL => &mut client_b,
                            MESSAGES_LABEL => &mut messages,
                            _ => continue,
                        };
                        *found = if entry.raw_type() != TYPE_DATA {
                            Err(NoRegion::NotData)
                        } else if entry.is_read_only() {
                            Err(NoRegion::ReadOnly)
                        } else if entry.offset() % SECTOR != 0 || entry.len() < SECTOR {
                            Err(NoRegion::Unfit)
                        } else {
                            Ok((entry.offset(), entry.len() / SECTOR))
                        };
                    }
                    (
                        identity.map(|(offset, _)| offset),
                        client.map(|(offset, _)| offset),
                        client_b.map(|(offset, _)| offset),
                        messages,
                    )
                }
            };
        Self {
            flash,
            identity: Slot(identity),
            client: Slot(client),
            client_b: Slot(client_b),
            messages,
        }
    }

    /// Where the identity lives.
    pub fn identity_slot(&self) -> Slot {
        self.identity
    }

    /// Where the client record lives.
    pub fn client_slot(&self) -> Slot {
        self.client
    }

    /// Where its second slot lives (1493).
    pub fn client_b_slot(&self) -> Slot {
        self.client_b
    }

    /// Where the message store lives.
    pub fn messages_slot(&self) -> Slot {
        Slot(self.messages.map(|(offset, _)| offset))
    }

    /// The message store's sectors, as the store reads and writes them;
    /// `None` on a board whose table has no `messages` partition.
    pub fn messages(&mut self) -> Option<Messages<'_, 'd>> {
        let (offset, count) = self.messages.ok()?;
        Some(Messages {
            flash: &mut self.flash,
            offset,
            count,
        })
    }

    fn read(&mut self, slot: Slot, buf: &mut [u8]) -> Result<(), StoreError> {
        let offset = slot.0.map_err(StoreError::NoRegion)?;
        self.flash.read_nor(offset, buf).map_err(StoreError::Flash)
    }

    fn write(&mut self, slot: Slot, buf: &[u8]) -> Result<(), StoreError> {
        let offset = slot.0.map_err(StoreError::NoRegion)?;
        let flash = &mut self.flash;
        parked(|| flash.erase(offset, offset + SECTOR)).map_err(StoreError::Flash)?;
        parked(|| flash.write_nor(offset, buf)).map_err(StoreError::Flash)
    }

    /// The node's identity, when one was saved.
    pub fn load_identity(&mut self) -> Result<Option<Identity>, StoreError> {
        let mut record = Aligned([0u8; IDENTITY_SIZE]);
        self.read(self.identity, &mut record.0)?;
        Ok(identity_store::decode_identity(&record.0))
    }

    /// Save the node's identity, unless the same bytes are there.
    pub fn save_identity(&mut self, identity: &Identity) -> Result<(), StoreError> {
        let Some(encoded) = identity_store::encode_identity(identity) else {
            return Ok(());
        };
        let encoded = Aligned(encoded);
        let mut current = Aligned([0u8; IDENTITY_SIZE]);
        if self.read(self.identity, &mut current.0).is_ok() && current.0 == encoded.0 {
            return Ok(());
        }
        self.write(self.identity, &encoded.0)
    }

    /// The two record slots as they stand: A, then B (erased where B has
    /// no region, so the picker sees one slot).
    fn client_slots(&mut self) -> Result<([u8; SLOT_READ], [u8; SLOT_READ]), StoreError> {
        let mut a = Aligned([0u8; SLOT_READ]);
        self.read(self.client, &mut a.0)?;
        let mut b = Aligned([0xFFu8; SLOT_READ]);
        if self.client_b.0.is_ok() {
            self.read(self.client_b, &mut b.0)?;
        }
        Ok((a.0, b.0))
    }

    /// The client record: `None` on a board that is no device of a home.
    /// The newer of the two slots answers (1493); a record a pre-1493
    /// image wrote reads as write 0.
    pub fn load_client(&mut self) -> Result<Option<ClientRecord>, StoreError> {
        let (a, b) = self.client_slots()?;
        Ok(slots::newest(&a, &b).map(|(record, _, _)| record))
    }

    /// Save the client record, unless it already stands: copy-then-switch
    /// (1493), into the slot that does not hold the newest record, so a
    /// power loss between erase and write leaves the old record standing.
    /// A board without the B partition saves in place as before.
    pub fn save_client(&mut self, record: &ClientRecord) -> Result<(), StoreError> {
        let (a, b) = self.client_slots()?;
        if slots::newest(&a, &b).is_some_and(|(current, _, _)| current == *record) {
            return Ok(());
        }
        let (target, counter) = slots::plan(&a, &b);
        let slot = match (target, self.client_b.0.is_ok()) {
            (1, true) => self.client_b,
            // No B region: the one sector, erased and rewritten as the
            // old images did it.
            _ => self.client,
        };
        let encoded = Aligned(slots::encode(record, counter));
        self.write(slot, &encoded.0)
    }
}

/// The bytes one flash call moves for the message store: the store's
/// buffers are on the heap, which is PSRAM behind the cache the flash
/// routines turn off, so every byte goes through this much stack.
const BOUNCE: usize = 512;

/// The message store's partition as [`Sectors`] (1476).
pub struct Messages<'a, 'd> {
    flash: &'a mut FlashStorage<'d>,
    offset: u32,
    count: u32,
}

impl Messages<'_, '_> {
    fn at(&self, sector: u32, offset: usize, len: usize) -> Result<u32, StoreError> {
        let inside = sector < self.count && offset + len <= STORE_SECTOR;
        match inside {
            true => Ok(self.offset + sector * SECTOR + offset as u32),
            false => Err(StoreError::Flash(FlashStorageError::OutOfBounds)),
        }
    }
}

impl Sectors for Messages<'_, '_> {
    type Error = StoreError;

    fn count(&self) -> u32 {
        self.count
    }

    fn read(&mut self, sector: u32, offset: usize, buf: &mut [u8]) -> Result<(), StoreError> {
        let mut at = self.at(sector, offset, buf.len())?;
        let mut bounce = Aligned([0u8; BOUNCE]);
        for chunk in buf.chunks_mut(BOUNCE) {
            // The store reads whole words: offsets and lengths are its
            // frames', a multiple of four.
            let words = (chunk.len() + 3) & !3;
            self.flash
                .read_nor(at, &mut bounce.0[..words])
                .map_err(StoreError::Flash)?;
            chunk.copy_from_slice(&bounce.0[..chunk.len()]);
            at += chunk.len() as u32;
        }
        Ok(())
    }

    fn write(&mut self, sector: u32, offset: usize, data: &[u8]) -> Result<(), StoreError> {
        let mut at = self.at(sector, offset, data.len())?;
        let mut bounce = Aligned([0u8; BOUNCE]);
        for chunk in data.chunks(BOUNCE) {
            bounce.0[..chunk.len()].copy_from_slice(chunk);
            let flash = &mut *self.flash;
            parked(|| flash.write_nor(at, &bounce.0[..chunk.len()])).map_err(StoreError::Flash)?;
            at += chunk.len() as u32;
        }
        Ok(())
    }

    fn erase(&mut self, sector: u32) -> Result<(), StoreError> {
        let from = self.at(sector, 0, STORE_SECTOR)?;
        let flash = &mut *self.flash;
        parked(|| flash.erase(from, from + SECTOR)).map_err(StoreError::Flash)
    }
}
