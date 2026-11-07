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
use loquax::home::record::{ClientRecord, RecordError, RECORD_LEN};

/// The partition the node's identity lives in.
pub const IDENTITY_LABEL: &str = "identity";
/// The partition the Loquax client record lives in.
pub const CLIENT_LABEL: &str = "loquax_client";
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

/// The board's stores.
pub struct Stores<'d> {
    flash: FlashStorage<'d>,
    identity: Slot,
    client: Slot,
    /// The message store's region: its offset and how many sectors.
    messages: Result<(u32, u32), NoRegion>,
}

impl<'d> Stores<'d> {
    /// Open the flash and find both stores in its partition table.
    pub fn open(flash: FLASH<'d>) -> Self {
        let mut flash = FlashStorage::new(flash);
        let mut table = Aligned([0u8; PARTITION_TABLE_MAX_LEN]);
        let (identity, client, messages) =
            match partitions::read_partition_table(&mut flash, &mut table.0) {
                Err(_) => (
                    Err(NoRegion::NoTable),
                    Err(NoRegion::NoTable),
                    Err(NoRegion::NoTable),
                ),
                Ok(table) => {
                    let mut identity = Err(NoRegion::NoEntry);
                    let mut client = Err(NoRegion::NoEntry);
                    let mut messages = Err(NoRegion::NoEntry);
                    for entry in table.iter() {
                        let found = match entry.label_as_str() {
                            IDENTITY_LABEL => &mut identity,
                            CLIENT_LABEL => &mut client,
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
                        messages,
                    )
                }
            };
        Self {
            flash,
            identity: Slot(identity),
            client: Slot(client),
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
        self.flash
            .erase(offset, offset + SECTOR)
            .map_err(StoreError::Flash)?;
        self.flash.write_nor(offset, buf).map_err(StoreError::Flash)
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

    /// The client record: `None` on a board that is no device of a home.
    pub fn load_client(&mut self) -> Result<Option<ClientRecord>, StoreError> {
        let mut bytes = Aligned([0u8; RECORD_LEN]);
        self.read(self.client, &mut bytes.0)?;
        ClientRecord::decode(&bytes.0).map_err(StoreError::Record)
    }

    /// Save the client record, unless the same bytes are there.
    pub fn save_client(&mut self, record: &ClientRecord) -> Result<(), StoreError> {
        let encoded = Aligned(record.encode());
        let mut current = Aligned([0u8; RECORD_LEN]);
        if self.read(self.client, &mut current.0).is_ok() && current.0 == encoded.0 {
            return Ok(());
        }
        self.write(self.client, &encoded.0)
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
            self.flash
                .write_nor(at, &bounce.0[..chunk.len()])
                .map_err(StoreError::Flash)?;
            at += chunk.len() as u32;
        }
        Ok(())
    }

    fn erase(&mut self, sector: u32) -> Result<(), StoreError> {
        let from = self.at(sector, 0, STORE_SECTOR)?;
        self.flash
            .erase(from, from + SECTOR)
            .map_err(StoreError::Flash)
    }
}
