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
use leviculum_core::wifi_credentials::{self, WifiCredentials};
use loquax::home::board::store::{Sectors, SECTOR as STORE_SECTOR};
use loquax::home::record::{slots, ClientRecord, RecordError};

use crate::stall::{self, Core, Lock};

/// Run a flash write or erase, `f`, with the UI core parked and this
/// core's interrupts off, named in the stall record (1534).
///
/// The op runs with core 0's interrupts off (esp-storage's own lock,
/// which [`parked`] extends over the whole hold) and the cache off,
/// which the UI core executes through, so the UI core must not run from
/// flash while the op does. Until 1613 that was esp-storage's AutoPark
/// — a hardware clock stall — under the cross-core critical section,
/// held across the whole op so the UI core could not be stalled holding
/// it (the M9's hang of 2026-11-07: the keyboard's 10 ms tick spinning
/// for a section the stalled core held). Since 1613 the UI core parks
/// itself instead ([`crate::corepark`]): a software interrupt puts it
/// into an internal-RAM spin that polls the receive FIFOs into rings
/// for the op's whole masked window, so the bytes the window used to
/// drop past the 128-byte FIFO (1 to 2 of 100 injected key frames,
/// windows 22 and 23) are kept. The cooperative park also removes the
/// hang's shape without holding the section across the op — the park
/// interrupt cannot land inside a held section, so the UI core is never
/// parked holding it; only the fallback stall (an unarmed or wedged UI
/// core) still takes the section around the op, for 1534's reason.
///
/// Since 1507 the op also waits for the panel's DMA out of PSRAM to end
/// and holds the next one off ([`crate::mspi`]): the bus that DMA reads is
/// the one the op takes.
fn parked<R>(op: park::Op, f: impl FnOnce() -> R) -> R {
    // The hold below keeps this core's interrupts off (1595): the
    // UART's receive interrupt stops popping, the parked spin takes
    // over, and the sweep here makes the spin's bytes sort strictly
    // after the keep's.
    crate::drain_rx_fifo();
    // The GNSS receiver's too (1477).
    crate::gnss_uart::drain();
    crate::mspi::flash(|| {
        stall::locked(Core::Io, Lock::Flash, || {
            let t0 = crate::uptime_us();
            let (r, _ack_wait) = crate::corepark::hold(f);
            park::note(op, crate::uptime_us().saturating_sub(t0));
            r
        })
    })
}

/// What the flash ops cost the UI core (1569): each op parks the other
/// core (cooperatively since 1613, [`crate::corepark`]), so the time
/// inside `parked`'s hold is the longest the UI core can have been held
/// per op. Cumulative counters; a reader takes deltas (`[IDLE]` prints
/// them per window), and the longest single park is taken, not read, so
/// each window says its own. The bench image also keeps the last ops
/// one by one (`park::take_records`, 1613), the `[FLASH] op= ms=`
/// lines a window's masked-length distribution is read from.
pub mod park {
    use core::sync::atomic::{AtomicU32, Ordering};

    /// What an op is, for the per-kind distribution (1613): the ring's
    /// own kind, `loquax_m9::oplog::Op`, so the host pins the ring the
    /// board keeps.
    pub use loquax_m9::oplog::Op;

    static TOTAL_US: AtomicU32 = AtomicU32::new(0);
    static OPS: AtomicU32 = AtomicU32::new(0);
    static MAX_US: AtomicU32 = AtomicU32::new(0);

    pub(super) fn note(op: Op, us: u64) {
        let us = u32::try_from(us).unwrap_or(u32::MAX);
        TOTAL_US.fetch_add(us, Ordering::Relaxed);
        OPS.fetch_add(1, Ordering::Relaxed);
        MAX_US.fetch_max(us, Ordering::Relaxed);
        #[cfg(m9_bench)]
        RECORDS.keep(op, us, crate::uptime_ms() as u32);
        #[cfg(not(m9_bench))]
        let _ = op;
    }

    /// The bench image's per-op records (1613): the last ops, written by
    /// the op's core and read by the same core's loop, which says each as
    /// a `[FLASH] op=<kind> ms=<n>.<frac> at=<ms>` line. Since 1622 sized
    /// for a forget's turn (`oplog::KEEP`, 1024; window 24's ring of 32
    /// said 7 of a forget's erases) and counting what a drain finds
    /// overwritten (`loquax-m9/tests/oplog.rs` pins both).
    #[cfg(m9_bench)]
    static RECORDS: loquax_m9::oplog::OpLog<{ loquax_m9::oplog::KEEP }> =
        loquax_m9::oplog::OpLog::new();

    /// Say every op since the last call: `(op, us, at_ms)`, oldest
    /// first; returns how many were overwritten before this call could
    /// say them. The bench image's loop turns this into `[FLASH]` lines
    /// and a `[FLASH] lost=` line when it is not zero.
    ///
    /// Since 1630 at most `max` of them: window 25's forget handed 550
    /// to one drain and the send ring dropped 388; the loop asks for no
    /// more than the ring has room for (`loquax_m9::oplog::sayable`).
    #[cfg(m9_bench)]
    pub fn take_records(max: u32, say: impl FnMut(Op, u32, u32)) -> u32 {
        RECORDS.take_at_most(max, say)
    }

    /// The records overwritten unsaid since boot, wrapping (1622): the
    /// `[IDLE]` line's `flash_lost=` is its delta per window.
    #[cfg(m9_bench)]
    pub fn records_lost() -> u32 {
        RECORDS.lost()
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
/// The partition the WLAN credentials live in (1479).
pub const WIFI_LABEL: &str = "wifi";
/// The partition the map regions `lnflash --region` wrote live in (1477),
/// a `leviculum_map::area`.
pub const REGIONS_LABEL: &str = "regions";

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

/// The map regions the area held (1477): those read, by name, and those
/// whose bytes did not match the directory.
#[derive(Default)]
pub struct Regions {
    pub loaded: alloc::vec::Vec<(alloc::string::String, &'static [u8])>,
    pub damaged: alloc::vec::Vec<alloc::string::String>,
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
    /// The map regions' directory is not one (1477).
    Area(leviculum_map::area::AreaError),
}

impl core::fmt::Display for StoreError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            StoreError::NoRegion(reason) => write!(f, "no-region/{reason}"),
            StoreError::Flash(error) => write!(f, "flash/{error:?}"),
            StoreError::Record(error) => write!(f, "record/{error:?}"),
            StoreError::Area(error) => write!(f, "area/{error}"),
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
    /// The map regions' area (1477): its offset and how many sectors.
    regions: Result<(u32, u32), NoRegion>,
    /// The WLAN credentials (1479).
    wifi: Slot,
}

impl<'d> Stores<'d> {
    /// Open the flash and find both stores in its partition table.
    pub fn open(flash: FLASH<'d>) -> Self {
        // The UI runs on the second core since 1493: every write and
        // erase parks it for the op, since the flash routines run with
        // the cache the core executes from off. The parking is this
        // crate's since 1613 (`corepark`: the cooperative spin that
        // polls the FIFOs, or its hardware-stall fallback), so
        // esp-storage must not stall the core a second time — AutoPark
        // would freeze the polling spin where it stands.
        // SAFETY (multicore_ignore's contract): every write and erase
        // goes through `parked`, whose hold guarantees the other core
        // is not started, spinning in internal RAM, or hardware-stalled
        // for the op's whole window.
        let mut flash = unsafe { FlashStorage::new(flash).multicore_ignore() };
        let mut table = Aligned([0u8; PARTITION_TABLE_MAX_LEN]);
        let (identity, client, client_b, messages, regions, wifi) =
            match partitions::read_partition_table(&mut flash, &mut table.0) {
                Err(_) => (
                    Err(NoRegion::NoTable),
                    Err(NoRegion::NoTable),
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
                    let mut regions = Err(NoRegion::NoEntry);
                    let mut wifi = Err(NoRegion::NoEntry);
                    for entry in table.iter() {
                        let found = match entry.label_as_str() {
                            IDENTITY_LABEL => &mut identity,
                            CLIENT_LABEL => &mut client,
                            CLIENT_B_LABEL => &mut client_b,
                            MESSAGES_LABEL => &mut messages,
                            REGIONS_LABEL => &mut regions,
                            WIFI_LABEL => &mut wifi,
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
                        regions,
                        wifi.map(|(offset, _)| offset),
                    )
                }
            };
        Self {
            flash,
            identity: Slot(identity),
            client: Slot(client),
            client_b: Slot(client_b),
            messages,
            regions,
            wifi: Slot(wifi),
        }
    }

    /// Where the map regions' area lives (1477).
    pub fn regions_slot(&self) -> Slot {
        Slot(self.regions.map(|(offset, _)| offset))
    }

    /// The map regions the area holds (1477), each read whole into the
    /// heap (PSRAM) and kept for the image's life: the map reads them in
    /// place from there, as it reads the fixture from the image. A region
    /// whose bytes do not match the directory's sum is left out and
    /// named in `damaged`.
    pub fn load_regions(&mut self) -> Result<Regions, StoreError> {
        let (offset, count) = self.regions.map_err(StoreError::NoRegion)?;
        let capacity = (count * SECTOR) as usize;
        let mut head = alloc::vec![0u8; leviculum_map::area::SECTOR];
        self.read_raw(offset, &mut head)?;
        let entries = match leviculum_map::area::directory(&head, capacity) {
            Ok(entries) => entries,
            Err(leviculum_map::area::AreaError::Empty) => alloc::vec::Vec::new(),
            Err(error) => return Err(StoreError::Area(error)),
        };
        let mut out = Regions::default();
        for entry in entries {
            let mut bytes = alloc::vec![0u8; (entry.len as usize + 3) & !3];
            self.read_raw(offset + entry.offset, &mut bytes)?;
            bytes.truncate(entry.len as usize);
            match leviculum_map::area::check(&entry, &bytes) {
                Ok(()) => {
                    let kept: &'static [u8] = alloc::boxed::Box::leak(bytes.into_boxed_slice());
                    out.loaded.push((entry.name, kept));
                }
                Err(_) => out.damaged.push(entry.name),
            }
        }
        Ok(out)
    }

    /// Read `buf` (a multiple of four) from `at` through the bounce
    /// buffer.
    fn read_raw(&mut self, mut at: u32, buf: &mut [u8]) -> Result<(), StoreError> {
        let mut bounce = Aligned([0u8; BOUNCE]);
        for chunk in buf.chunks_mut(BOUNCE) {
            let words = (chunk.len() + 3) & !3;
            self.flash
                .read_nor(at, &mut bounce.0[..words])
                .map_err(StoreError::Flash)?;
            chunk.copy_from_slice(&bounce.0[..chunk.len()]);
            at += chunk.len() as u32;
        }
        Ok(())
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

    /// Where the WLAN credentials live (1479).
    pub fn wifi_slot(&self) -> Slot {
        self.wifi
    }

    /// The WLAN the board joins (1479): `None` where `lnflash --wifi`
    /// wrote nothing, which keeps the WiFi radio off. Read only, the
    /// board never writes it.
    pub fn load_wifi(&mut self) -> Result<Option<WifiCredentials>, StoreError> {
        let mut record = Aligned([0u8; wifi_credentials::RECORD_LEN]);
        self.read(self.wifi, &mut record.0)?;
        Ok(WifiCredentials::decode(&record.0))
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
        parked(park::Op::Erase, || flash.erase(offset, offset + SECTOR))
            .map_err(StoreError::Flash)?;
        parked(park::Op::Write, || flash.write_nor(offset, buf)).map_err(StoreError::Flash)
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
            parked(park::Op::Write, || {
                flash.write_nor(at, &bounce.0[..chunk.len()])
            })
            .map_err(StoreError::Flash)?;
            at += chunk.len() as u32;
        }
        Ok(())
    }

    fn erase(&mut self, sector: u32) -> Result<(), StoreError> {
        let from = self.at(sector, 0, STORE_SECTOR)?;
        let flash = &mut *self.flash;
        parked(park::Op::Erase, || flash.erase(from, from + SECTOR)).map_err(StoreError::Flash)
    }
}
