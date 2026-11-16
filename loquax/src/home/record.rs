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
//!      6    1  the reader's settings (1532): bit 0 the large type on the
//!                 M9; zero in a record written before, which reads as
//!                 the small type, the default
//!      7    1  zero
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
/// Byte 6's bits 0-1: the reader's type size (1532, 1561). 0 the
/// default, 1 the large one (1532's flag, so an older record reads as it
/// was written), 2 the very small one; the board names them
/// (`loquax_m9::font::Size::of_record`).
const TYPE_SIZE: u8 = 0b11;

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
    /// The reader's type size on the board (1532, 1561): byte 6 bits
    /// 0-1, 0 the default, 1 large, 2 very small.
    pub type_size: u8,
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
            type_size: 0,
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
        out[6] = self.type_size & TYPE_SIZE;
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
            type_size: bytes[6] & TYPE_SIZE,
        };
        record.device_identity()?;
        Ok(Some(record))
    }
}

/// The record across two flash sectors, copy-then-switch (1493): the old
/// record stands until the new one's write checked. Each slot holds the
/// record, a write counter and a checksum over both; a save goes into the
/// slot that does NOT hold the newest record, so a power loss between
/// erase and write leaves the other slot whole, and the boot takes the
/// slot with the higher counter. A sector written before 1493 holds the
/// bare record and reads as counter 0: the first save after the update
/// goes to the other slot with counter 1.
pub mod slots {
    use super::*;

    /// A slot: the record, the counter and the slot checksum.
    pub const SLOT_LEN: usize = RECORD_LEN + 8;

    /// Encode `record` under write `counter`.
    pub fn encode(record: &ClientRecord, counter: u32) -> [u8; SLOT_LEN] {
        let mut out = [0u8; SLOT_LEN];
        out[..RECORD_LEN].copy_from_slice(&record.encode());
        out[RECORD_LEN..RECORD_LEN + 4].copy_from_slice(&counter.to_le_bytes());
        let sum = sha256(&out[..RECORD_LEN + 4]);
        out[RECORD_LEN + 4..].copy_from_slice(&sum[..4]);
        out
    }

    /// The record in a slot's bytes and its counter: a wrapped one as
    /// [`encode`] wrote it, or a bare pre-1493 record as counter 0.
    /// `None` for an erased slot or torn bytes of either shape.
    pub fn decode(bytes: &[u8]) -> Option<(ClientRecord, u32)> {
        if bytes.len() >= SLOT_LEN {
            let summed = &bytes[..RECORD_LEN + 4];
            if sha256(summed)[..4] == bytes[RECORD_LEN + 4..SLOT_LEN] {
                if let Ok(Some(record)) = ClientRecord::decode(&bytes[..RECORD_LEN]) {
                    let mut counter = [0u8; 4];
                    counter.copy_from_slice(&bytes[RECORD_LEN..RECORD_LEN + 4]);
                    return Some((record, u32::from_le_bytes(counter)));
                }
            }
        }
        match ClientRecord::decode(bytes) {
            Ok(Some(record)) => Some((record, 0)),
            _ => None,
        }
    }

    /// The newest record of the two slots, with which slot holds it.
    pub fn newest(a: &[u8], b: &[u8]) -> Option<(ClientRecord, u32, usize)> {
        match (decode(a), decode(b)) {
            (Some((ra, ca)), Some((rb, cb))) => Some(match cb > ca {
                true => (rb, cb, 1),
                false => (ra, ca, 0),
            }),
            (Some((ra, ca)), None) => Some((ra, ca, 0)),
            (None, Some((rb, cb))) => Some((rb, cb, 1)),
            (None, None) => None,
        }
    }

    /// Where the next save goes and under which counter: the slot that
    /// does not hold the newest record.
    pub fn plan(a: &[u8], b: &[u8]) -> (usize, u32) {
        match newest(a, b) {
            Some((_, counter, held)) => (1 - held, counter.wrapping_add(1)),
            None => (0, 1),
        }
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

    /// The type size (1532, 1561) round-trips in byte 6's two low bits
    /// under the checksum, all three of them; a record written before
    /// 1532, byte 6 zero, reads as the default, and one written by 1532
    /// with its large flag reads as large; a bit flipped by hand without
    /// the checksum is a torn record.
    #[test]
    fn the_type_size_is_kept_in_two_bits_and_an_older_record_reads_as_written() {
        let device = Identity::generate(&mut rand_core::OsRng);
        let mut record = ClientRecord::pending(&device, &code(), "M9").expect("record");
        assert_eq!(record.type_size, 0);
        let small = record.encode();
        assert_eq!(small[6], 0, "as a record before 1532 wrote it");
        for size in [1u8, 2] {
            record.type_size = size;
            let bytes = record.encode();
            assert_eq!(bytes[6], size);
            let back = ClientRecord::decode(&bytes).expect("decode").expect("some");
            assert_eq!(back.type_size, size);
            assert_eq!(back, record);
        }
        record.type_size = 1;
        let large = record.encode();
        let back = ClientRecord::decode(&small).expect("decode").expect("some");
        assert_eq!(back.type_size, 0);
        let mut forged = small;
        forged[6] = 2;
        assert_eq!(ClientRecord::decode(&forged), Err(RecordError::Torn));
        let back = ClientRecord::decode(&large).expect("decode").expect("some");
        assert_eq!(back.type_size, 1, "1532's large flag");
        record.type_size = 2;
        let slot = slots::encode(&record, 3);
        assert_eq!(slots::decode(&slot).map(|(r, _)| r.type_size), Some(2));
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

#[cfg(test)]
mod slot_tests {
    use super::slots::*;
    use super::*;

    fn record(cursor: u64) -> ClientRecord {
        let device = Identity::generate(&mut rand_core::OsRng);
        let code = HomeCode {
            public_key: Identity::generate(&mut rand_core::OsRng).public_key_bytes(),
            token: [7; 16],
        };
        let mut record = ClientRecord::pending(&device, &code, "M9").expect("record");
        record.cursor = cursor;
        record
    }

    /// Copy-then-switch (1493): the save goes to the other slot under the
    /// next counter, the boot takes the newer, and a save cut mid-write
    /// leaves the previous record standing — before, the one sector was
    /// erased first and a cut left the pairing screen.
    #[test]
    fn the_newer_slot_wins_and_a_cut_write_keeps_the_old_record() {
        let erased = [0xFFu8; 4096];
        assert_eq!(plan(&erased, &erased), (0, 1));
        let first = record(1);
        let a = encode(&first, 1);
        assert_eq!(plan(&a, &erased), (1, 2));
        let second = record(2);
        let b = encode(&second, 2);
        let (newest_record, counter, held) = newest(&a, &b).expect("two slots");
        assert_eq!((newest_record.cursor, counter, held), (2, 2, 1));
        assert_eq!(plan(&a, &b), (0, 3));
        // The third write is cut halfway: slot A holds garbage, slot B
        // still answers with the second record.
        let third = encode(&record(3), 3);
        let mut torn = [0xFFu8; SLOT_LEN];
        torn[..SLOT_LEN / 2].copy_from_slice(&third[..SLOT_LEN / 2]);
        let (kept, counter, held) = newest(&torn, &b).expect("the old record stands");
        assert_eq!((kept.cursor, counter, held), (2, 2, 1));
    }

    /// A sector written before 1493 holds the bare record: it reads as
    /// counter 0, and the first save after the update leaves it alone.
    #[test]
    fn a_bare_record_reads_as_counter_zero() {
        let old = record(7);
        let mut sector = [0xFFu8; 4096];
        sector[..RECORD_LEN].copy_from_slice(&old.encode());
        let erased = [0xFFu8; 4096];
        let (kept, counter, held) = newest(&sector, &erased).expect("legacy record");
        assert_eq!((kept.cursor, counter, held), (7, 0, 0));
        assert_eq!(plan(&sector, &erased), (1, 1));
    }
}
