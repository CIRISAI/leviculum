//! Size harness for Codeberg #384. Throwaway, and behind a non-default
//! feature that no shipped build enables.
//!
//! The spike weighs two stores for the region behind the firmware image on
//! the nRF52840's own flash. Everything about their behaviour is decided on
//! the host, over `leviculum_record_log::sim::SimNor`
//! (`leviculum-nrf/store-spike`). What the host cannot answer is what each
//! costs in flash, and space is the binding constraint here: every KiB the
//! store's code takes is a KiB less for the store's data or the firmware's
//! growth.
//!
//! So one of two features drags one candidate's code into the image:
//!
//! ```text
//! cargo build --release --bin t114 --features bsp-t114
//! cargo build --release --bin t114 --features bsp-t114,store-spike-record-log
//! cargo build --release --bin t114 --features bsp-t114,store-spike-sequential
//! ```
//!
//! and `arm-none-eabi-size` on the three ELFs is the measurement
//! (`tools/store-spike-size.sh`). [`exercise`] appends, iterates and
//! removes once, which is what keeps the linker from discarding any of it;
//! it is called once from each bin's `main`, under the same feature gate.
//!
//! **Nothing here is a proposal for the firmware.** The region below is a
//! placeholder so the call compiles, and no build that enables these
//! features is meant to be flashed.

use embedded_storage_async::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};

use crate::flash::SharedFlash;

/// First page boundary behind the Pocket image of de6e74ed. Placeholder: the
/// real base has to come from the linker, not from a constant somebody has
/// to remember to move.
const SPIKE_BASE: u32 = 0xA_6000;
/// `USER_FLASH_END` minus the three pages already spoken for (identity,
/// radio config, telemetry) is where the region would stop; here it is only
/// large enough to be a legal region.
const SPIKE_LEN: u32 = 8 * 4096;

/// A borrowed flash handle that still carries the marker traits.
///
/// `embedded-storage-async` blanket-implements `ReadNorFlash` and `NorFlash`
/// for `&mut T` but not `MultiwriteNorFlash`, and both candidates need that
/// marker to withdraw an entry. Both get it through the same wrapper, so
/// whatever it costs it costs them equally.
struct Borrowed<'a>(&'a mut nrf_softdevice::Flash);

type Sd = nrf_softdevice::Flash;

impl ErrorType for Borrowed<'_> {
    type Error = <Sd as ErrorType>::Error;
}

impl ReadNorFlash for Borrowed<'_> {
    const READ_SIZE: usize = <Sd as ReadNorFlash>::READ_SIZE;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        <Sd as ReadNorFlash>::read(self.0, offset, bytes).await
    }

    fn capacity(&self) -> usize {
        <Sd as ReadNorFlash>::capacity(self.0)
    }
}

impl NorFlash for Borrowed<'_> {
    const WRITE_SIZE: usize = <Sd as NorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <Sd as NorFlash>::ERASE_SIZE;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        <Sd as NorFlash>::erase(self.0, from, to).await
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        <Sd as NorFlash>::write(self.0, offset, bytes).await
    }
}

impl MultiwriteNorFlash for Borrowed<'_> {}

/// Append one entry, iterate the store, remove one entry.
///
/// Runs exactly once, reports what it did on the debug log, and returns. The
/// point is not the behaviour — that is measured on the host — but that
/// every part of the candidate's code is reachable from `main` and therefore
/// survives LTO.
pub async fn exercise(shared: &'static SharedFlash) {
    let mut guard = shared.lock().await;
    let flash = Borrowed(&mut guard);
    let outcome = run(flash).await;
    crate::log::log_fmt("[SPIKE] ", format_args!("store exercise: {}", outcome));
}

#[cfg(feature = "store-spike-record-log")]
async fn run(flash: Borrowed<'_>) -> &'static str {
    use leviculum_record_log::RecordLog;

    let body = [0xA5u8; 304];
    let key = [0x11u8; leviculum_record_log::KEY_LEN];
    let Ok(mut log) = RecordLog::open(flash, SPIKE_BASE, SPIKE_LEN).await else {
        return "open failed";
    };
    if log.append(&key, 1, 0, &body).await.is_err() {
        return "append failed";
    }
    let mut first = None;
    if log
        .for_each(|record| first = first.or(Some(*record)))
        .await
        .is_err()
    {
        return "iterate failed";
    }
    let Some(record) = first else {
        return "iterated nothing";
    };
    let mut out = [0u8; 304];
    if log.read_body(&record, &mut out).await.is_err() {
        return "read failed";
    }
    if log.purge(&record).await.is_err() {
        return "purge failed";
    }
    "record-log ok"
}

#[cfg(feature = "store-spike-sequential")]
async fn run(flash: Borrowed<'_>) -> &'static str {
    use sequential_storage::cache::Cache;
    use sequential_storage::queue::{QueueConfig, QueueStorage};

    let body = [0xA5u8; 304];
    let mut buf = [0u8; 320];
    let Ok(config) = QueueConfig::try_new(SPIKE_BASE..SPIKE_BASE + SPIKE_LEN) else {
        return "bad region";
    };
    let mut queue = QueueStorage::new(flash, config, Cache::new_uncached());
    if queue.push(&body, true).await.is_err() {
        return "push failed";
    }
    let Ok(mut iter) = queue.iter().await else {
        return "iterate failed";
    };
    match iter.next(&mut buf).await {
        Ok(Some(entry)) => {
            if entry.pop().await.is_err() {
                return "pop failed";
            }
        }
        Ok(None) => return "iterated nothing",
        Err(_) => return "iterate failed",
    }
    "sequential-storage ok"
}
