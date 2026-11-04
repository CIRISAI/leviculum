//! The board's stores in flash, found by label in the partition table
//! (1414): the node's identity and, on a Loquax handheld, the client
//! record that makes it a device of its home.
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
use loquax::home::record::{ClientRecord, RecordError, RECORD_LEN};

/// The partition the node's identity lives in.
pub const IDENTITY_LABEL: &str = "identity";
/// The partition the Loquax client record lives in.
pub const CLIENT_LABEL: &str = "loquax_client";

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
}

impl<'d> Stores<'d> {
    /// Open the flash and find both stores in its partition table.
    pub fn open(flash: FLASH<'d>) -> Self {
        let mut flash = FlashStorage::new(flash);
        let mut table = Aligned([0u8; PARTITION_TABLE_MAX_LEN]);
        let (identity, client) = match partitions::read_partition_table(&mut flash, &mut table.0) {
            Err(_) => (Err(NoRegion::NoTable), Err(NoRegion::NoTable)),
            Ok(table) => {
                let mut identity = Err(NoRegion::NoEntry);
                let mut client = Err(NoRegion::NoEntry);
                for entry in table.iter() {
                    let found = match entry.label_as_str() {
                        IDENTITY_LABEL => &mut identity,
                        CLIENT_LABEL => &mut client,
                        _ => continue,
                    };
                    *found = if entry.raw_type() != TYPE_DATA {
                        Err(NoRegion::NotData)
                    } else if entry.is_read_only() {
                        Err(NoRegion::ReadOnly)
                    } else if entry.offset() % SECTOR != 0 || entry.len() < SECTOR {
                        Err(NoRegion::Unfit)
                    } else {
                        Ok(entry.offset())
                    };
                }
                (identity, client)
            }
        };
        Self {
            flash,
            identity: Slot(identity),
            client: Slot(client),
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
