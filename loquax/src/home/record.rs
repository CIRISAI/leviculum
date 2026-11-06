//! What makes a board a client of its home, in one record in flash (1414):
//! the device's own identity, the home's public key, where the admission
//! stands, the sync cursor and the name the device joined under.
//! `no_std`, so the M9 writes it and the host pins it.
//!
//! The desktop keeps the same facts as three files beside its store
//! (`client::CLIENT_FILE`, `client::DEVICE_IDENTITY_FILE`,
//! `client::CURSOR_FILE`); a board has a flash sector, which is erased and
//! written whole, so they are one record with a checksum: a write cut by a
//! power loss reads back as no record, never as somebody else's key.
//!
//! ```text
//! offset  len  field
//!      0    4  magic "LQXC"
//!      4    1  version, 1
//!      5    1  admission: 1 pending (a code typed, the join not done), 2 admitted
//!      6    2  zero
//!      8   64  the device identity's private key
//!     72   64  the home identity's public key
//!    136   16  the admission token while pending, zero once admitted
//!    152    8  the sync cursor, little end first
//!    160    1  the name's length, at most 31
//!    161   31  the name, UTF-8, zero padded
//!    192    4  the first four bytes of SHA-256 over bytes 0..192
//! ```
//!
//! 196 bytes, a multiple of four as the ESP32's flash writes want.

use alloc::string::String;

use leviculum_core::crypto::sha256;
use leviculum_core::Identity;

use super::ask::{Ask, HomeError, Verb};
use super::code::HomeCode;
use super::wire::Fold;

/// The record's length in flash.
pub const RECORD_LEN: usize = 196;
/// The longest device name the record holds, in bytes.
pub const NAME_MAX: usize = 31;

const MAGIC: &[u8; 4] = b"LQXC";
const VERSION: u8 = 1;
const PENDING: u8 = 1;
const ADMITTED: u8 = 2;
const SUMMED: usize = RECORD_LEN - 4;

/// Where the device's admission stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// A code was taken in and its join is owed: the token it carries.
    Pending { token: [u8; 16] },
    /// The home admitted this device.
    Admitted,
}

/// Why bytes in the record's place are not a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordError {
    /// Not the magic, or a version this build does not read.
    Foreign,
    /// The checksum does not match: a write that did not finish.
    Torn,
    /// A field out of its range: an admission state, a name.
    Malformed,
    /// The device identity is not a private key.
    Identity,
}

/// A board's client record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRecord {
    /// The device identity's private key: what the home admitted.
    pub device: [u8; 64],
    /// The home identity's public key, from the admission code.
    pub home: [u8; 64],
    /// Where the admission stands.
    pub admission: Admission,
    /// The newest archive sequence this device took in.
    pub cursor: u64,
    /// The name the device joins under, at most [`NAME_MAX`] bytes.
    pub name: String,
}

impl ClientRecord {
    /// A record for a code just taken in: `device` joins the home of
    /// `code` as `name`, cut to [`NAME_MAX`] bytes on a character boundary.
    pub fn pending(device: &Identity, code: &HomeCode, name: &str) -> Result<Self, RecordError> {
        let device = device
            .private_key_bytes()
            .map_err(|_| RecordError::Identity)?;
        let mut end = name.len().min(NAME_MAX);
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        Ok(Self {
            device,
            home: code.public_key,
            admission: Admission::Pending { token: code.token },
            cursor: 0,
            name: String::from(&name[..end]),
        })
    }

    /// The device identity, with its private key.
    pub fn device_identity(&self) -> Result<Identity, RecordError> {
        Identity::from_private_key_bytes(&self.device).map_err(|_| RecordError::Identity)
    }

    /// Whether the home admitted this device.
    pub fn admitted(&self) -> bool {
        self.admission == Admission::Admitted
    }

    /// The join a pending record owes, begun at `now_ms` with `budget_ms`;
    /// `None` once admitted.
    pub fn join(&self, now_ms: u64, budget_ms: u64) -> Option<Result<Ask, HomeError>> {
        let Admission::Pending { token } = self.admission else {
            return None;
        };
        let verb = Verb::Join {
            token,
            name: self.name.clone(),
        };
        Some(Ask::new(&self.home, verb, now_ms, budget_ms))
    }

    /// A pull from the record's cursor in pages of `page_bytes`; `None`
    /// before the device is admitted.
    pub fn pull(
        &self,
        page_bytes: u64,
        now_ms: u64,
        budget_ms: u64,
    ) -> Option<Result<Ask, HomeError>> {
        if !self.admitted() {
            return None;
        }
        let verb = Verb::Pull {
            cursor: self.cursor,
            page_bytes: Some(page_bytes),
        };
        // The record is the board's (1415): it folds as the board does,
        // so the home serves it only what `board::fold` keeps (1439).
        Some(Ask::new(&self.home, verb, now_ms, budget_ms).map(|ask| ask.folding(Fold::Board)))
    }

    /// The bytes that go to flash.
    pub fn encode(&self) -> [u8; RECORD_LEN] {
        let mut out = [0u8; RECORD_LEN];
        out[..4].copy_from_slice(MAGIC);
        out[4] = VERSION;
        out[8..72].copy_from_slice(&self.device);
        out[72..136].copy_from_slice(&self.home);
        match self.admission {
            Admission::Pending { token } => {
                out[5] = PENDING;
                out[136..152].copy_from_slice(&token);
            }
            Admission::Admitted => out[5] = ADMITTED,
        }
        out[152..160].copy_from_slice(&self.cursor.to_le_bytes());
        let name = &self.name.as_bytes()[..self.name.len().min(NAME_MAX)];
        out[160] = name.len() as u8;
        out[161..161 + name.len()].copy_from_slice(name);
        let sum = sha256(&out[..SUMMED]);
        out[SUMMED..].copy_from_slice(&sum[..4]);
        out
    }

    /// The record in `bytes`: `Ok(None)` for an erased sector (every byte
    /// 0xFF) or one never written (every byte zero), an error for bytes that
    /// are something else.
    pub fn decode(bytes: &[u8]) -> Result<Option<Self>, RecordError> {
        let Some(bytes) = bytes.get(..RECORD_LEN) else {
            return Err(RecordError::Malformed);
        };
        if bytes.iter().all(|&b| b == 0xFF) || bytes.iter().all(|&b| b == 0) {
            return Ok(None);
        }
        if &bytes[..4] != MAGIC || bytes[4] != VERSION {
            return Err(RecordError::Foreign);
        }
        if sha256(&bytes[..SUMMED])[..4] != bytes[SUMMED..] {
            return Err(RecordError::Torn);
        }
        let mut token = [0u8; 16];
        token.copy_from_slice(&bytes[136..152]);
        let admission = match bytes[5] {
            PENDING => Admission::Pending { token },
            ADMITTED => Admission::Admitted,
            _ => return Err(RecordError::Malformed),
        };
        let len = usize::from(bytes[160]);
        if len > NAME_MAX {
            return Err(RecordError::Malformed);
        }
        let name =
            core::str::from_utf8(&bytes[161..161 + len]).map_err(|_| RecordError::Malformed)?;
        let mut device = [0u8; 64];
        device.copy_from_slice(&bytes[8..72]);
        let mut home = [0u8; 64];
        home.copy_from_slice(&bytes[72..136]);
        let mut cursor = [0u8; 8];
        cursor.copy_from_slice(&bytes[152..160]);
        let record = Self {
            device,
            home,
            admission,
            cursor: u64::from_le_bytes(cursor),
            name: String::from(name),
        };
        record.device_identity()?;
        Ok(Some(record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code() -> HomeCode {
        let home = Identity::generate(&mut rand_core::OsRng);
        HomeCode {
            public_key: home.public_key_bytes(),
            token: [7; 16],
        }
    }

    /// A pending record, admitted and moved on, reads back as written;
    /// the device identity it holds is the one it was made with.
    #[test]
    fn a_record_reads_back_as_written_through_its_life() {
        let device = Identity::generate(&mut rand_core::OsRng);
        let code = code();
        let mut record = ClientRecord::pending(&device, &code, "M9").expect("record");
        let bytes = record.encode();
        assert_eq!(ClientRecord::decode(&bytes), Ok(Some(record.clone())));
        assert!(record.join(0, 1_000).is_some());
        assert!(record.pull(412, 0, 1_000).is_none());

        record.admission = Admission::Admitted;
        record.cursor = 0x0102_0304_0506_0708;
        let bytes = record.encode();
        assert_eq!(&bytes[136..152], &[0; 16], "no token once admitted");
        let back = ClientRecord::decode(&bytes).expect("decode").expect("some");
        assert_eq!(back, record);
        assert_eq!(
            back.device_identity().expect("identity").hash(),
            device.hash()
        );
        assert!(back.join(0, 1_000).is_none());
        let pull = back.pull(412, 0, 1_000).expect("admitted").expect("ask");
        assert_eq!(pull.cursor(), Some(0x0102_0304_0506_0708));
        assert_eq!(pull.page_bytes(), Some(412));
    }

    /// An erased or empty sector is no record; a flipped byte anywhere is a
    /// torn write, not a different key; foreign bytes are named as such.
    #[test]
    fn erased_torn_and_foreign_bytes_are_told_apart() {
        let device = Identity::generate(&mut rand_core::OsRng);
        let record = ClientRecord::pending(&device, &code(), "M9").expect("record");
        assert_eq!(ClientRecord::decode(&[0xFF; RECORD_LEN]), Ok(None));
        assert_eq!(ClientRecord::decode(&[0; RECORD_LEN]), Ok(None));
        let good = record.encode();
        for at in [5, 8, 71, 100, 140, 155, 161, SUMMED - 1, SUMMED] {
            let mut bad = good;
            bad[at] ^= 0x01;
            assert_eq!(ClientRecord::decode(&bad), Err(RecordError::Torn), "{at}");
        }
        let mut foreign = good;
        foreign[0] = b'X';
        assert_eq!(ClientRecord::decode(&foreign), Err(RecordError::Foreign));
        assert_eq!(
            ClientRecord::decode(&good[..RECORD_LEN - 1]),
            Err(RecordError::Malformed)
        );
    }

    /// A name longer than the record holds is cut on a character, never
    /// inside one.
    #[test]
    fn a_long_name_is_cut_on_a_character() {
        let device = Identity::generate(&mut rand_core::OsRng);
        let name = "Lews ThinkNode M9 im Rucksack äöü";
        let record = ClientRecord::pending(&device, &code(), name).expect("record");
        assert!(record.name.len() <= NAME_MAX);
        assert!(name.starts_with(&record.name));
        let back = ClientRecord::decode(&record.encode())
            .expect("decode")
            .expect("some");
        assert_eq!(back.name, record.name);
    }
}
