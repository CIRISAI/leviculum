//! Persistent storage for whom a board shares its position with (order
//! 1301, Refs #232): the `lev.position` announce's privacy setting.
//!
//! A host sets it over the control envelope
//! ([`crate::envelope::TYPE_POSITION_SHARE`], `lnflash --share-position`).
//! The record shares the telemetry flash page (the firmware names the
//! layout in `leviculum_nrf::telemetry`); this module only fixes the
//! record's bytes, in the same magic + version + checksum envelope as its
//! neighbours.
//!
//! Layout (8 bytes, word aligned):
//!
//! ```text
//!  0..4   magic "LPSH"
//!  4      format version (0x01)
//!  5      setting: 0 default, 1 off, 2 owner, 3 everyone
//!  6..8   checksum over bytes 0..6
//! ```
//!
//! The setting's meaning, and the default's resolution by role, is
//! `leviculum_announce_policy::ShareSetting` and `PositionShare::resolve`;
//! this crate does not depend on that one, so it stores the byte and
//! refuses only what names no setting ([`POSITION_SHARE_MAX`]).
//!
//! **`None` is "no record"**, which the caller runs as the default: on for
//! a tracker, sealed to its owner, off for every other board.

const MAGIC: [u8; 4] = [0x4C, 0x50, 0x53, 0x48]; // "LPSH"
const FORMAT_VERSION: u8 = 0x01;
const SETTING_OFFSET: usize = 5;
const CHECKSUM_OFFSET: usize = 6;
const CHECKSUM_SIZE: usize = 2;

/// Total encoded size: 6 header+payload + 2 checksum = 8 bytes.
pub const ENCODED_SIZE: usize = CHECKSUM_OFFSET + CHECKSUM_SIZE;

/// Encoded size rounded up to 4-byte alignment (flash writes are
/// word-wide): 8.
pub const ENCODED_SIZE_ALIGNED: usize = ENCODED_SIZE.div_ceil(4) * 4;

/// The largest byte that names a setting ("everyone").
pub const POSITION_SHARE_MAX: u8 = 3;

/// The settings' names, indexed by their byte: what `lnflash
/// --share-position` takes and the board's `[POSITION] share=` line
/// writes (`leviculum_announce_policy::ShareSetting::as_str`, which the
/// firmware holds to these).
pub const SETTING_NAMES: [&str; POSITION_SHARE_MAX as usize + 1] =
    ["default", "off", "owner", "everyone"];

/// The byte a setting's name stands for.
pub fn parse_setting(name: &str) -> Option<u8> {
    SETTING_NAMES
        .iter()
        .position(|known| *known == name)
        .map(|i| i as u8)
}

/// Same two-byte XOR checksum every record on the page uses.
fn checksum(data: &[u8]) -> [u8; 2] {
    let mut a: u8 = 0;
    let mut b: u8 = 0;
    for (i, &byte) in data.iter().enumerate() {
        if i % 2 == 0 {
            a ^= byte;
        } else {
            b ^= byte;
        }
    }
    [a, b]
}

/// Encode the setting byte into a fixed-size buffer for persistent storage.
pub fn encode_position_share(setting: u8) -> [u8; ENCODED_SIZE_ALIGNED] {
    let mut buf = [0u8; ENCODED_SIZE_ALIGNED];
    buf[0..4].copy_from_slice(&MAGIC);
    buf[4] = FORMAT_VERSION;
    buf[SETTING_OFFSET] = setting;
    let cs = checksum(&buf[..CHECKSUM_OFFSET]);
    buf[CHECKSUM_OFFSET] = cs[0];
    buf[CHECKSUM_OFFSET + 1] = cs[1];
    buf
}

/// Decode the setting byte. `None` for a blank region, wrong magic or
/// version, checksum mismatch, or a byte above [`POSITION_SHARE_MAX`].
pub fn decode_position_share(buf: &[u8]) -> Option<u8> {
    if buf.len() < ENCODED_SIZE {
        return None;
    }
    if buf[0] == 0xFF || buf[0..4] != MAGIC || buf[4] != FORMAT_VERSION {
        return None;
    }
    let stored = [buf[CHECKSUM_OFFSET], buf[CHECKSUM_OFFSET + 1]];
    if stored != checksum(&buf[..CHECKSUM_OFFSET]) {
        return None;
    }
    let setting = buf[SETTING_OFFSET];
    (setting <= POSITION_SHARE_MAX).then_some(setting)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_round_trips() {
        for setting in 0..=POSITION_SHARE_MAX {
            assert_eq!(
                decode_position_share(&encode_position_share(setting)),
                Some(setting)
            );
        }
    }

    #[test]
    fn a_blank_zeroed_or_unknown_record_is_no_record() {
        assert!(decode_position_share(&[0xFF; ENCODED_SIZE_ALIGNED]).is_none());
        assert!(decode_position_share(&[0x00; ENCODED_SIZE_ALIGNED]).is_none());
        assert!(decode_position_share(&encode_position_share(POSITION_SHARE_MAX + 1)).is_none());
    }

    #[test]
    fn a_bit_flip_anywhere_is_no_record() {
        for i in 0..ENCODED_SIZE {
            let mut buf = encode_position_share(2);
            buf[i] ^= 0x01;
            assert!(
                decode_position_share(&buf).is_none(),
                "bit flip at byte {i} was accepted"
            );
        }
    }

    #[test]
    fn every_name_parses_to_its_byte_and_nothing_else_does() {
        for (byte, name) in SETTING_NAMES.iter().enumerate() {
            assert_eq!(parse_setting(name), Some(byte as u8));
        }
        assert_eq!(parse_setting("Owner"), None);
        assert_eq!(parse_setting(""), None);
    }

    #[test]
    fn the_encoded_size_is_word_aligned() {
        assert_eq!(ENCODED_SIZE, 8);
        assert_eq!(ENCODED_SIZE_ALIGNED, 8);
    }
}
