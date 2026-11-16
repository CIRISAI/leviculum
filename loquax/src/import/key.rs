//! The identity every LXMF messenger shares: 64 raw bytes, the X25519
//! private key then the Ed25519 one (`docs/drafts/loquax-import-2026-10.md`,
//! "The common ground"), as a file or pasted as RFC 4648 Base32 text. And
//! adopting one: the old identity is set aside, never deleted, and the new
//! one is written in loquax's own envelope with mode 0600.
//!
//! Nothing here logs or prints key material; callers name paths and
//! addresses only.

use std::path::{Path, PathBuf};

use leviculum_core::identity_store::{decode_identity, encode_identity};
use leviculum_std::Identity;

use crate::address::to_hex;
use crate::engine::delivery_address;

/// The raw identity's length.
pub const KEY_LEN: usize = 64;

/// A raw identity file's bytes, when they are exactly one identity.
pub fn from_raw(bytes: &[u8]) -> Option<[u8; KEY_LEN]> {
    let key: [u8; KEY_LEN] = bytes.try_into().ok()?;
    Identity::from_private_key_bytes(&key).ok().map(|_| key)
}

/// The 64 bytes of a pasted Base32 key: RFC 4648, upper or lower case,
/// padding and white space ignored.
pub fn from_base32(text: &str) -> Option<[u8; KEY_LEN]> {
    let mut bits: u64 = 0;
    let mut have = 0u32;
    let mut out = Vec::with_capacity(KEY_LEN);
    for c in text.chars().filter(|c| !c.is_whitespace() && *c != '=') {
        let value = match c.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u64 - 'A' as u64,
            c @ '2'..='7' => c as u64 - '2' as u64 + 26,
            _ => return None,
        };
        bits = (bits << 5) | value;
        have += 5;
        if have >= 8 {
            have -= 8;
            out.push((bits >> have) as u8);
            bits &= (1 << have) - 1;
        }
    }
    from_raw(&out)
}

/// The key as RFC 4648 Base32 with padding, the form the other apps export.
pub fn to_base32(key: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    for chunk in key.chunks(5) {
        let mut block = [0u8; 5];
        block[..chunk.len()].copy_from_slice(chunk);
        let n = block.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
        let chars = (chunk.len() * 8).div_ceil(5);
        for i in 0..8 {
            match i < chars {
                true => out.push(ALPHABET[((n >> (35 - 5 * i)) & 31) as usize] as char),
                false => out.push('='),
            }
        }
    }
    out
}

/// The LXMF address the key answers on.
pub fn address_of(key: &[u8; KEY_LEN]) -> Result<[u8; 16], String> {
    let identity =
        Identity::from_private_key_bytes(key).map_err(|e| format!("not an identity: {e:?}"))?;
    delivery_address(&identity)
}

/// What adopting an identity did.
#[derive(Debug, PartialEq, Eq)]
pub enum Adopted {
    /// loquax already had this identity; nothing changed.
    Already,
    /// The new identity is in place. `set_aside` is where the old one went,
    /// with its address, when there was one.
    Switched {
        address: [u8; 16],
        set_aside: Option<(PathBuf, [u8; 16])>,
    },
}

/// Make `key` loquax's identity at `home/identity`. An identity already
/// there with another address is renamed to `identity.<address>.bak` first,
/// never deleted, so switching back is moving it again; its history stays
/// in the store under its address (rows are scoped by identity). The new
/// file is written under a temporary name with mode 0600 and moved into
/// place.
pub fn adopt(home: &Path, key: &[u8; KEY_LEN]) -> Result<Adopted, String> {
    let identity =
        Identity::from_private_key_bytes(key).map_err(|e| format!("not an identity: {e:?}"))?;
    let address = delivery_address(&identity)?;
    let path = home.join("identity");
    let mut set_aside = None;
    match std::fs::read(&path) {
        Ok(bytes) => {
            let old = decode_identity(&bytes).ok_or_else(|| {
                format!(
                    "{} is not a readable identity record; move it aside first",
                    path.display()
                )
            })?;
            let old_address = delivery_address(&old)?;
            if old_address == address {
                return Ok(Adopted::Already);
            }
            let aside = free_name(home, &format!("identity.{}.bak", to_hex(&old_address)));
            std::fs::rename(&path, &aside).map_err(|e| format!("{}: {e}", path.display()))?;
            set_aside = Some((aside, old_address));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("{}: {error}", path.display())),
    }
    let encoded = encode_identity(&identity).ok_or("the identity has no private key")?;
    write_private(&path, &encoded)?;
    crate::identity::mark_moved(home)?;
    Ok(Adopted::Switched { address, set_aside })
}

/// `name` under `dir`, or `name.1`, `name.2` and on when it is taken.
fn free_name(dir: &Path, name: &str) -> PathBuf {
    let mut candidate = dir.join(name);
    let mut n = 1;
    while std::fs::symlink_metadata(&candidate).is_ok() {
        candidate = dir.join(format!("{name}.{n}"));
        n += 1;
    }
    candidate
}

/// Write `bytes` to `path` with mode 0600, through a temporary name in the
/// same directory and a rename, so the file is whole or absent: the stack's
/// one writer for a secret ([`leviculum_std::atomic_write_secret`]), after
/// the directory is made.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().unwrap_or(Path::new("."));
    leviculum_std::create_secret_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    leviculum_std::atomic_write_secret(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> [u8; KEY_LEN] {
        leviculum_std::generate_identity()
            .private_key_bytes()
            .expect("key")
    }

    #[test]
    fn base32_round_trips_and_tolerates_case_padding_and_spaces() {
        let key = key();
        let text = to_base32(&key);
        assert!(
            text.ends_with('='),
            "64 bytes pad to a multiple of 8: {text}"
        );
        assert_eq!(from_base32(&text), Some(key));
        let messy = format!("  {} \n", text.to_ascii_lowercase().trim_end_matches('='));
        assert_eq!(from_base32(&messy), Some(key));
        assert_eq!(from_base32("not base32!"), None);
        assert_eq!(from_base32(&to_base32(&key[..32])), None, "half a key");
    }

    /// The RFC 4648 test vectors, so the alphabet and the padding are the
    /// standard's and not merely self-consistent.
    #[test]
    fn base32_matches_the_rfc_vectors() {
        assert_eq!(to_base32(b"f"), "MY======");
        assert_eq!(to_base32(b"fo"), "MZXQ====");
        assert_eq!(to_base32(b"foob"), "MZXW6YQ=");
        assert_eq!(to_base32(b"fooba"), "MZXW6YTB");
        assert_eq!(to_base32(b"foobar"), "MZXW6YTBOI======");
    }

    #[test]
    fn a_raw_file_is_exactly_64_bytes() {
        let key = key();
        assert_eq!(from_raw(&key), Some(key));
        assert_eq!(from_raw(&key[..63]), None);
        let mut longer = key.to_vec();
        longer.push(0);
        assert_eq!(from_raw(&longer), None);
    }

    #[test]
    fn adopting_sets_the_old_identity_aside_and_writes_0600() {
        let home = tempfile::tempdir().expect("home");
        let old = crate::identity::load_or_create(&home.path().join("identity")).expect("old");
        let old_address = delivery_address(&old).expect("old address");
        let key = key();
        let adopted = adopt(home.path(), &key).expect("adopt");
        let address = address_of(&key).expect("address");
        let aside = home
            .path()
            .join(format!("identity.{}.bak", to_hex(&old_address)));
        assert_eq!(
            adopted,
            Adopted::Switched {
                address,
                set_aside: Some((aside.clone(), old_address)),
            }
        );
        let kept = decode_identity(&std::fs::read(&aside).expect("aside")).expect("decodes");
        assert_eq!(delivery_address(&kept).expect("kept"), old_address);
        let now = crate::identity::load_or_create(&home.path().join("identity")).expect("new");
        assert_eq!(delivery_address(&now).expect("now"), address);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(home.path().join("identity"))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert_eq!(adopt(home.path(), &key).expect("again"), Adopted::Already);
    }

    #[test]
    fn adopting_into_an_empty_home_sets_nothing_aside() {
        let home = tempfile::tempdir().expect("home");
        let key = key();
        let adopted = adopt(home.path(), &key).expect("adopt");
        assert!(matches!(
            adopted,
            Adopted::Switched {
                set_aside: None,
                ..
            }
        ));
    }

    #[test]
    fn a_second_set_aside_of_the_same_address_does_not_overwrite_the_first() {
        let home = tempfile::tempdir().expect("home");
        std::fs::write(home.path().join("x"), b"one").expect("write");
        assert_eq!(free_name(home.path(), "x"), home.path().join("x.1"));
    }
}
