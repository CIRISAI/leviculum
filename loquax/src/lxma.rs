//! The contact code Columba and Sideband exchange: `lxma://<address>:<key>`,
//! the LXMF delivery address as 32 hex digits and the identity's 64 byte
//! public key as 128 (Columba's `IdentityQrCodeUtils.kt:8` and `:49`, the
//! prefix `ValidationConstants.kt:125`, the key length `:58`). A QR code of
//! it adds a contact in one scan, and because the key comes with it, a
//! message can go out before any announce of the contact was heard.
//!
//! One implementation for every frontend: the phone's profile and scanner,
//! the new conversation field, and [`crate::address::parse`], which takes
//! the code where an address is asked for.

use alloc::string::String;

use crate::address::{to_hex, ADDRESS_BYTES};

/// The scheme in front, as Columba writes and expects it.
pub const PREFIX: &str = "lxma://";

/// The length of an identity's public key: X25519 then Ed25519.
pub const KEY_BYTES: usize = 64;

/// A contact as the code carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Contact {
    /// Its LXMF delivery address.
    pub address: [u8; ADDRESS_BYTES],
    /// Its identity's public key.
    pub public_key: [u8; KEY_BYTES],
}

/// Why a text is no contact code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LxmaError {
    /// It does not start with [`PREFIX`].
    NotLxma,
    /// No `:` between the address and the key, or more than one.
    NoKey,
    /// The address part is not 32 hex digits.
    BadAddress,
    /// The key part is not 128 hex digits.
    BadKey,
    /// The address is not the one the key makes: a code copied wrong, or
    /// made up.
    Mismatch,
}

impl core::fmt::Display for LxmaError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotLxma => write!(f, "a contact code starts with {PREFIX}"),
            Self::NoKey => write!(f, "a contact code is {PREFIX}address:key"),
            Self::BadAddress => write!(
                f,
                "the address in a contact code is {} hex characters",
                ADDRESS_BYTES * 2
            ),
            Self::BadKey => write!(
                f,
                "the key in a contact code is {} hex characters",
                KEY_BYTES * 2
            ),
            Self::Mismatch => write!(f, "the address in this contact code does not match its key"),
        }
    }
}

impl core::error::Error for LxmaError {}

/// Whether `text` is meant as a contact code: it starts with [`PREFIX`],
/// in any case, after whitespace. A frontend routes on this, then parses.
pub fn is_lxma(text: &str) -> bool {
    let text = text.trim_start();
    text.get(..PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(PREFIX))
}

/// The code of the identity whose public key is `public_key`, its address
/// computed from it.
pub fn encode(public_key: &[u8; KEY_BYTES]) -> Result<String, String> {
    let address = crate::home::lxmf_address(public_key)?;
    Ok(encode_parts(&address, public_key))
}

/// The code of `address` and `public_key` as given, byte for byte the
/// string Columba's `encodeToQrString` makes: lowercase hex.
pub fn encode_parts(address: &[u8; ADDRESS_BYTES], public_key: &[u8; KEY_BYTES]) -> String {
    let mut out = String::with_capacity(PREFIX.len() + 1 + 2 * (ADDRESS_BYTES + KEY_BYTES));
    out.push_str(PREFIX);
    out.push_str(&to_hex(address));
    out.push(':');
    out.push_str(&to_hex(public_key));
    out
}

/// Parse a contact code. Tolerant where Columba's input field is (the
/// whitespace of a paste, any case) and stricter in one place: the address
/// has to be the one the key makes, so a code with a typo is refused here
/// rather than becoming a contact that never answers.
pub fn parse(text: &str) -> Result<Contact, LxmaError> {
    let text = text.trim();
    if !is_lxma(text) {
        return Err(LxmaError::NotLxma);
    }
    let rest = &text[PREFIX.len()..];
    let mut parts = rest.split(':');
    let (Some(address), Some(key), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(LxmaError::NoKey);
    };
    let address = hex_array::<ADDRESS_BYTES>(address).ok_or(LxmaError::BadAddress)?;
    let public_key = hex_array::<KEY_BYTES>(key).ok_or(LxmaError::BadKey)?;
    match crate::home::lxmf_address(&public_key) {
        Ok(made) if made == address => Ok(Contact {
            address,
            public_key,
        }),
        _ => Err(LxmaError::Mismatch),
    }
}

/// Exactly `N` bytes of hex, either case; `None` for anything else.
fn hex_array<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != 2 * N || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; N];
    for (byte, pair) in out.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
        let digit = |c: u8| match c {
            b'0'..=b'9' => c - b'0',
            _ => c.to_ascii_lowercase() - b'a' + 10,
        };
        *byte = (digit(pair[0]) << 4) | digit(pair[1]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    /// A fixed identity: its private key bytes are 0x01..=0x40, so the
    /// public key, address and code never change.
    fn fixture() -> leviculum_core::Identity {
        let private: [u8; 64] = core::array::from_fn(|i| i as u8 + 1);
        leviculum_core::Identity::from_private_key_bytes(&private).expect("a key")
    }

    /// Columba's `encodeToQrString`, written out: the prefix, the
    /// address's lowercase hex (`"%02x".format` of each byte), a colon,
    /// the key's. Our encoder must give the same bytes.
    #[test]
    fn the_code_is_columbas_byte_for_byte() {
        let identity = fixture();
        let key = identity.public_key_bytes();
        let address = crate::engine::delivery_address(&identity).expect("address");
        let mut columba = String::from("lxma://");
        for byte in address {
            columba.push_str(&format!("{byte:02x}"));
        }
        columba.push(':');
        for byte in key {
            columba.push_str(&format!("{byte:02x}"));
        }
        let ours = encode(&key).expect("a code");
        assert_eq!(ours.as_bytes(), columba.as_bytes());
        assert_eq!(ours.len(), 7 + 32 + 1 + 128);
    }

    /// A code of the shape Columba shares, pinned as text: the fixture's
    /// own, so it is one whose address really is its key's. It parses,
    /// and the fixture's encoding is still exactly this string.
    #[test]
    fn a_columba_shaped_code_parses() {
        const CODE: &str = "lxma://4ca1677223757e1036d8f87cf18d9ad9:\
            07a37cbc142093c8b755dc1b10e86cb426374ad16aa853ed0bdfc0b2b86d1c7c\
            e7f162a10bec559afea195e4dce84b69568d5d2cb0963eb446c0685e2b17f2f0";
        assert_eq!(encode(&fixture().public_key_bytes()).as_deref(), Ok(CODE));
        let contact = parse(CODE).expect("it parses");
        assert_eq!(contact.public_key, fixture().public_key_bytes());
    }

    #[test]
    fn our_own_code_parses_back_to_our_address_and_key() {
        let identity = fixture();
        let key = identity.public_key_bytes();
        let contact = parse(&encode(&key).expect("a code")).expect("it parses");
        assert_eq!(contact.public_key, key);
        assert_eq!(
            contact.address,
            crate::engine::delivery_address(&identity).expect("address")
        );
    }

    #[test]
    fn a_pasted_code_in_upper_case_with_whitespace_parses() {
        let code = encode(&fixture().public_key_bytes()).expect("a code");
        let pasted = format!("  {}\n", code.to_ascii_uppercase());
        assert!(is_lxma(&pasted));
        assert_eq!(parse(&pasted), parse(&code));
    }

    #[test]
    fn a_malformed_code_is_refused_with_its_reason() {
        let key = fixture().public_key_bytes();
        let code = encode(&key).expect("a code");
        let (address, hex_key) = code[PREFIX.len()..].split_once(':').expect("two parts");
        assert_eq!(parse(address), Err(LxmaError::NotLxma));
        assert_eq!(parse(&format!("lxma://{address}")), Err(LxmaError::NoKey));
        assert_eq!(
            parse(&format!("lxma://{address}:{hex_key}:00")),
            Err(LxmaError::NoKey)
        );
        assert_eq!(
            parse(&format!("lxma://{}:{hex_key}", &address[2..])),
            Err(LxmaError::BadAddress)
        );
        assert_eq!(
            parse(&format!("lxma://{address}:{}zz", &hex_key[2..])),
            Err(LxmaError::BadKey)
        );
        // One digit of the address changed: well formed, and refused.
        let flipped = if address.starts_with('0') { "1" } else { "0" };
        assert_eq!(
            parse(&format!("lxma://{flipped}{}:{hex_key}", &address[1..])),
            Err(LxmaError::Mismatch)
        );
        assert!(!format!("{}", LxmaError::Mismatch).is_empty());
    }
}
