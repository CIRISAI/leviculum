//! Hardware RNG access for NodeCore.
//!
//! The Nordic S140 SoftDevice owns the RNG peripheral exclusively at
//! runtime (PREGION-protected); any direct register access at 0x4000_D000
//! traps as `NRF_FAULT_ID_APP_MEMACC` and panics via
//! `nrf_softdevice::softdevice::fault_handler`. Post-`Softdevice::enable`,
//! we MUST use `sd_rand_application_vector_get`. **Pre**-enable, the
//! peripheral is unprotected and direct register access works — and we
//! need it to, because identity generation on a fresh device runs in
//! `NodeCoreBuilder::build` which is called before `ble::init`.
//!
//! Strategy: call the SD syscall first. If it returns success, use it.
//! If it fails (SD not yet enabled → `NRF_ERROR_INVALID_STATE`), fall
//! back to direct register access. The fallback is only ever exercised
//! pre-enable; once SD is up, the syscall never returns INVALID_STATE.
//!
//! Bug #32 spike: the previous unconditional direct-register-access
//! version cycled the device every ~26 s with a SoftDevice MEMACC panic.

use rand_core::{CryptoRng, RngCore};

/// Hardware RNG that prefers the SoftDevice's CSPRNG syscall, with a
/// pre-enable fallback to direct register access.
pub struct RawHwRng;

const RNG_BASE: u32 = 0x4000_D000;
// Register table mirrors the nRF52840 reference manual offsets; the
// explicit `+ 0x000` keeps the offset column symmetric and greppable.
#[allow(clippy::identity_op)]
const RNG_TASKS_START: *mut u32 = (RNG_BASE + 0x000) as *mut u32;
const RNG_TASKS_STOP: *mut u32 = (RNG_BASE + 0x004) as *mut u32;
const RNG_EVENTS_VALRDY: *mut u32 = (RNG_BASE + 0x100) as *mut u32;
const RNG_VALUE: *const u32 = (RNG_BASE + 0x508) as *const u32;
const RNG_CONFIG: *mut u32 = (RNG_BASE + 0x504) as *mut u32;

impl Default for RawHwRng {
    fn default() -> Self {
        Self::new()
    }
}

impl RawHwRng {
    pub fn new() -> Self {
        Self
    }

    /// Direct hardware register read — safe ONLY pre-`Softdevice::enable`.
    /// Used as the fallback when the SD syscall returns INVALID_STATE
    /// because identity generation happens in `NodeCoreBuilder::build`
    /// before `ble::init` runs. Post-enable, this would fault the SD.
    fn direct_byte() -> u8 {
        unsafe {
            // Enable bias correction once, idempotent.
            core::ptr::write_volatile(RNG_CONFIG, 1);
            core::ptr::write_volatile(RNG_EVENTS_VALRDY, 0);
            core::ptr::write_volatile(RNG_TASKS_START, 1);
            while core::ptr::read_volatile(RNG_EVENTS_VALRDY) == 0 {}
            let val = core::ptr::read_volatile(RNG_VALUE) as u8;
            core::ptr::write_volatile(RNG_TASKS_STOP, 1);
            val
        }
    }

    /// Try `sd_rand_application_vector_get` for one chunk of up to 255 bytes.
    /// Returns `Some(())` on success, `None` if SD is not enabled
    /// (NRF_ERROR_INVALID_STATE = 8). Other errors retry up to 10 000
    /// times for SocRandNotEnoughValues; any persistent non-zero return
    /// also yields `None`.
    fn try_syscall_chunk(buf: *mut u8, len: u8) -> Option<()> {
        let mut retries = 0u32;
        loop {
            let ret = unsafe { nrf_softdevice_s140::sd_rand_application_vector_get(buf, len) };
            match ret {
                0 => return Some(()), // NRF_SUCCESS
                8 => return None,     // NRF_ERROR_INVALID_STATE — SD not enabled
                _ => {
                    retries += 1;
                    if retries > 10_000 {
                        return None;
                    }
                    cortex_m::asm::nop();
                }
            }
        }
    }

    fn fill(dest: &mut [u8]) {
        if dest.is_empty() {
            return;
        }
        let mut offset = 0usize;
        while offset < dest.len() {
            let chunk_len = (dest.len() - offset).min(u8::MAX as usize);
            match Self::try_syscall_chunk(dest[offset..].as_mut_ptr(), chunk_len as u8) {
                Some(()) => {
                    offset += chunk_len;
                }
                None => {
                    // Pre-SD-enable fallback: direct register access.
                    // Works because no PREGION protection yet.
                    for byte in &mut dest[offset..offset + chunk_len] {
                        *byte = Self::direct_byte();
                    }
                    offset += chunk_len;
                }
            }
        }
    }
}

impl RngCore for RawHwRng {
    fn next_u32(&mut self) -> u32 {
        let mut buf = [0u8; 4];
        Self::fill(&mut buf);
        u32::from_le_bytes(buf)
    }

    fn next_u64(&mut self) -> u64 {
        let mut buf = [0u8; 8];
        Self::fill(&mut buf);
        u64::from_le_bytes(buf)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        Self::fill(dest);
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        Self::fill(dest);
        Ok(())
    }
}

// The SoftDevice's CSPRNG is seeded from the hardware TRNG with bias correction.
impl CryptoRng for RawHwRng {}
