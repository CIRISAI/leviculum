//! The WLAN a board joins, as one flash sector (1479).
//!
//! `lnflash --wifi` writes it, the ThinkNode M9 reads it at boot from its
//! `wifi` partition (`leviculum-esp/partitions-m9.csv`); a board without
//! one keeps its WiFi radio off. One codec for both sides, here, so the
//! host and the board cannot disagree on the layout.
//!
//! Layout, little-endian where it matters:
//!
//! ```text
//! 0..4    magic "LWIF"
//! 4       version (1)
//! 5       SSID length, 1..=32
//! 6       passphrase length, 0 (an open network) or 8..=64
//! 7       reserved, 0
//! 8..40   SSID, zero-padded
//! 40..104 passphrase, zero-padded
//! 104..108 check: the first four bytes of full_hash(bytes 0..104)
//! ```
//!
//! An erased sector (all 0xff) and anything else that does not check is
//! "no credentials". The passphrase is a secret: [`WifiCredentials`]'s
//! `Debug` never prints it, and nothing on the board logs it.

use crate::crypto::full_hash_parts;

/// The record's size; the rest of its sector stays erased.
pub const RECORD_LEN: usize = 108;
/// The SSID's longest length (IEEE 802.11).
pub const SSID_MAX: usize = 32;
/// The passphrase's longest length: 63 characters, or 64 hex digits for a
/// raw pre-shared key.
pub const PASS_MAX: usize = 64;
/// The shortest WPA2 passphrase.
pub const PASS_MIN: usize = 8;

const MAGIC: &[u8; 4] = b"LWIF";
const VERSION: u8 = 1;
const SSID_AT: usize = 8;
const PASS_AT: usize = SSID_AT + SSID_MAX;
const CHECK_AT: usize = PASS_AT + PASS_MAX;

/// A WLAN's name and passphrase.
#[derive(Clone, PartialEq, Eq)]
pub struct WifiCredentials {
    ssid: [u8; SSID_MAX],
    ssid_len: u8,
    pass: [u8; PASS_MAX],
    pass_len: u8,
}

/// Why a name or passphrase cannot be stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// The SSID is empty or longer than 32 bytes.
    Ssid,
    /// The passphrase is 1 to 7 or more than 64 bytes long.
    Passphrase,
}

impl core::fmt::Display for Refused {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Refused::Ssid => f.write_str("the network name must be 1 to 32 bytes"),
            Refused::Passphrase => f.write_str(
                "the passphrase must be 8 to 63 characters, 64 hex digits, or empty for an open network",
            ),
        }
    }
}

impl WifiCredentials {
    /// `ssid` with `passphrase`; an empty passphrase is an open network.
    pub fn new(ssid: &[u8], passphrase: &[u8]) -> Result<Self, Refused> {
        if ssid.is_empty() || ssid.len() > SSID_MAX {
            return Err(Refused::Ssid);
        }
        if !passphrase.is_empty() && !(PASS_MIN..=PASS_MAX).contains(&passphrase.len()) {
            return Err(Refused::Passphrase);
        }
        let mut out = Self {
            ssid: [0; SSID_MAX],
            ssid_len: ssid.len() as u8,
            pass: [0; PASS_MAX],
            pass_len: passphrase.len() as u8,
        };
        out.ssid[..ssid.len()].copy_from_slice(ssid);
        out.pass[..passphrase.len()].copy_from_slice(passphrase);
        Ok(out)
    }

    /// The network's name.
    pub fn ssid(&self) -> &[u8] {
        &self.ssid[..usize::from(self.ssid_len)]
    }

    /// The passphrase, empty for an open network. Never log it.
    pub fn passphrase(&self) -> &[u8] {
        &self.pass[..usize::from(self.pass_len)]
    }

    /// The record as it goes into flash.
    pub fn encode(&self) -> [u8; RECORD_LEN] {
        let mut out = [0u8; RECORD_LEN];
        out[..4].copy_from_slice(MAGIC);
        out[4] = VERSION;
        out[5] = self.ssid_len;
        out[6] = self.pass_len;
        out[SSID_AT..PASS_AT].copy_from_slice(&self.ssid);
        out[PASS_AT..CHECK_AT].copy_from_slice(&self.pass);
        let check = full_hash_parts(&[&out[..CHECK_AT]]);
        out[CHECK_AT..].copy_from_slice(&check[..4]);
        out
    }

    /// The record at the start of `sector`; `None` for an erased sector or
    /// one that does not check.
    pub fn decode(sector: &[u8]) -> Option<Self> {
        let rec = sector.get(..RECORD_LEN)?;
        if &rec[..4] != MAGIC || rec[4] != VERSION {
            return None;
        }
        let check = full_hash_parts(&[&rec[..CHECK_AT]]);
        if rec[CHECK_AT..] != check[..4] {
            return None;
        }
        let (ssid_len, pass_len) = (usize::from(rec[5]), usize::from(rec[6]));
        let ssid = rec.get(SSID_AT..SSID_AT + ssid_len.min(SSID_MAX + 1))?;
        let pass = rec.get(PASS_AT..PASS_AT + pass_len.min(PASS_MAX + 1))?;
        Self::new(ssid, pass).ok()
    }
}

impl core::fmt::Debug for WifiCredentials {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WifiCredentials")
            .field(
                "ssid",
                &core::str::from_utf8(self.ssid()).unwrap_or("<bytes>"),
            )
            .field("passphrase", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    use alloc::format;

    #[test]
    fn a_record_comes_back_as_it_went() {
        let c = WifiCredentials::new(b"Heimnetz", b"correct horse battery").unwrap();
        let mut sector = [0xffu8; 4096];
        sector[..RECORD_LEN].copy_from_slice(&c.encode());
        let back = WifiCredentials::decode(&sector).unwrap();
        assert_eq!(back, c);
        assert_eq!(back.ssid(), b"Heimnetz");
        assert_eq!(back.passphrase(), b"correct horse battery");
        let open = WifiCredentials::new(b"cafe", b"").unwrap();
        assert_eq!(WifiCredentials::decode(&open.encode()).unwrap(), open);
    }

    #[test]
    fn an_erased_or_damaged_sector_is_no_credentials() {
        assert!(WifiCredentials::decode(&[0xff; 4096]).is_none());
        assert!(WifiCredentials::decode(&[0; 4096]).is_none());
        let mut rec = WifiCredentials::new(b"x", b"12345678").unwrap().encode();
        rec[SSID_AT] ^= 1;
        assert!(WifiCredentials::decode(&rec).is_none());
        assert!(WifiCredentials::decode(&rec[..RECORD_LEN - 1]).is_none());
    }

    #[test]
    fn names_and_passphrases_wpa2_cannot_take_are_refused() {
        assert_eq!(WifiCredentials::new(b"", b"12345678"), Err(Refused::Ssid));
        assert_eq!(WifiCredentials::new(&[b'a'; 33], b""), Err(Refused::Ssid));
        assert_eq!(
            WifiCredentials::new(b"a", b"1234567"),
            Err(Refused::Passphrase)
        );
        assert_eq!(
            WifiCredentials::new(b"a", &[b'f'; 65]),
            Err(Refused::Passphrase)
        );
        assert!(WifiCredentials::new(&[b'a'; 32], &[b'f'; 64]).is_ok());
    }

    #[test]
    fn the_passphrase_never_prints() {
        let c = WifiCredentials::new(b"Heimnetz", b"geheimgeheim").unwrap();
        let said = format!("{c:?}");
        assert!(said.contains("Heimnetz"), "{said}");
        assert!(!said.contains("geheim"), "{said}");
    }
}
