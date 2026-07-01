//! Identity persistence via internal flash (NVMC) for nRF52840.
//!
//! Implements [`IdentityStore`] using the nRF52840's internal flash.
//! The wire format (magic, version, checksum) is defined in
//! `leviculum_core::identity_store` and shared across all targets.

use embassy_nrf::nvmc::Nvmc;
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use leviculum_core::identity::Identity;
use leviculum_core::identity_store::{self, IdentityStore, ENCODED_SIZE_ALIGNED};

/// NVMC-backed identity store for the T114.
///
/// The flash page address is supplied by the bin file (typically from
/// `BoardConfig::identity_flash_page`). On T114 it is 0xEC000, just below
/// Heltec's reserved area (0xED000); on RAK4631 the same address falls
/// in unused application flash.
pub struct NvmcIdentityStore<'d> {
    nvmc: Nvmc<'d>,
    page: u32,
}

impl<'d> NvmcIdentityStore<'d> {
    pub fn new(nvmc: Nvmc<'d>, page: u32) -> Self {
        Self { nvmc, page }
    }
}

impl IdentityStore for NvmcIdentityStore<'_> {
    type Error = embassy_nrf::nvmc::Error;

    fn load(&mut self) -> Result<Option<Identity>, Self::Error> {
        let mut buf = [0u8; ENCODED_SIZE_ALIGNED];
        self.nvmc.read(self.page, &mut buf)?;
        Ok(identity_store::decode_identity(&buf))
    }

    fn save(&mut self, identity: &Identity) -> Result<(), Self::Error> {
        let buf = match identity_store::encode_identity(identity) {
            Some(b) => b,
            None => return Ok(()),
        };
        self.nvmc.erase(self.page, self.page + 4096)?;
        self.nvmc.write(self.page, &buf)?;
        Ok(())
    }
}
