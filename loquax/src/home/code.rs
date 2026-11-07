//! The admission code: what `loquax mailbox admit` prints on the home node and
//! `loquax home join` reads on the new device.
//!
//! It carries the home identity's public key (64 bytes), from which the
//! client derives both the home destination it links to and the LXMF address
//! it shows as its own, and a single-use token (16 bytes). Four bytes of
//! SHA-256 over the two follow, so a code mistyped by hand is refused before
//! anything goes on the wire rather than turning into a link to nowhere.
//! The 84 bytes are written in RFC 4648 base32, lower case and unpadded,
//! behind the prefix `lqxh1`: 140 characters, which a terminal can copy and
//! slice 5's QR code can carry.
//!
//! A phone has no shared instance to reach the home node through, so it
//! also needs the home node's TCP server. `loquax mailbox admit --server`
//! writes it behind the code, `lqxh1...@host:port` (an IPv6 address in
//! brackets), so one scan or one paste carries both. The base32 alphabet
//! has no `@`, so the split is unambiguous, and a code without the address
//! reads as before.
//!
//! Behind the `@` goes the mailbox's rendezvous list (1196,
//! `docs/src/concepts/loquax-reachability.md`, rule 5): `host:port`
//! entries separated by [`LIST_SEPARATOR`], the first to be tried first.
//! A code with one entry is the form above, byte for byte, so every app
//! reads it as it always did. An app from before the list takes a code
//! with several entries as one address, whose host then holds a colon,
//! and refuses it with "an IPv6 address goes in brackets" before anything
//! goes on the wire: it never dials a wrong host. `loquax mailbox admit
//! --server host:port` with a single entry writes the old form for it.

use alloc::string::String;
use alloc::vec::Vec;

/// What separates the code from the home node's address in the combined form.
pub const SERVER_SEPARATOR: char = '@';

/// What separates the entries of the rendezvous list behind the code. Not
/// in the base32 alphabet, not in a host name, an IPv4 or a bracketed IPv6
/// address.
pub const LIST_SEPARATOR: char = ',';

/// What every code starts with, so a code is told from an address or a hash
/// at a glance, and a later format can take `lqxh2`.
pub const PREFIX: &str = "lqxh1";

const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
const CHECK_LEN: usize = 4;
const BODY_LEN: usize = 64 + 16;

/// An admission code, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeCode {
    /// The home identity's public key, X25519 then Ed25519, as RNS writes it.
    pub public_key: [u8; 64],
    /// The single-use admission token.
    pub token: [u8; 16],
}

/// Why a text is not a code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeError {
    /// It does not start with [`PREFIX`].
    Prefix,
    /// A character outside the base32 alphabet, or the wrong length.
    Shape,
    /// The checksum does not match: a typo.
    Checksum,
}

impl core::fmt::Display for CodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Prefix => write!(f, "a home code starts with {PREFIX}"),
            Self::Shape => write!(f, "not a home code: wrong length or a stray character"),
            Self::Checksum => write!(f, "the home code has a typo (its checksum does not match)"),
        }
    }
}

impl HomeCode {
    /// The code as text.
    pub fn encode(&self) -> String {
        let mut bytes = Vec::with_capacity(BODY_LEN + CHECK_LEN);
        bytes.extend_from_slice(&self.public_key);
        bytes.extend_from_slice(&self.token);
        let check = leviculum_core::crypto::sha256(&bytes);
        bytes.extend_from_slice(&check[..CHECK_LEN]);
        let mut text = String::from(PREFIX);
        text.push_str(&base32(&bytes));
        text
    }

    /// The code with the home node's TCP server behind it, `host:port`:
    /// what a phone takes in one go.
    pub fn encode_with_server(&self, server: &str) -> String {
        let mut text = self.encode();
        text.push(SERVER_SEPARATOR);
        text.push_str(server.trim());
        text
    }

    /// The code with the rendezvous list behind it, `host:port` each, the
    /// first tried first: what a phone takes in one go. An empty list is
    /// the code alone, a list of one [`HomeCode::encode_with_server`].
    pub fn encode_with_rendezvous(&self, list: &[String]) -> String {
        let entries: Vec<&str> = list
            .iter()
            .map(|entry| entry.trim())
            .filter(|entry| !entry.is_empty())
            .collect();
        match entries.is_empty() {
            true => self.encode(),
            false => {
                let mut separator = String::new();
                separator.push(LIST_SEPARATOR);
                self.encode_with_server(&entries.join(separator.as_str()))
            }
        }
    }

    /// Read a code and the rendezvous list behind it, empty when there is
    /// none. Whitespace and empty entries are ignored; the entries are not
    /// checked, as in [`HomeCode::parse_with_server`].
    pub fn parse_with_rendezvous(text: &str) -> Result<(Self, Vec<String>), CodeError> {
        let (code, tail) = Self::parse_tail(text)?;
        let list = tail
            .map(|tail| {
                tail.split(LIST_SEPARATOR)
                    .filter(|entry| !entry.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        Ok((code, list))
    }

    /// Read a code, in either form; an address behind it is dropped. A
    /// device that joins through its shared instance has no use for it.
    pub fn parse(text: &str) -> Result<Self, CodeError> {
        Self::parse_with_server(text).map(|(code, _)| code)
    }

    /// Read a code and the address behind it, if any. Whitespace anywhere
    /// is ignored, and the code's case is, so a code that a terminal
    /// wrapped or a person typed in capitals still reads. The address is
    /// not checked here: whoever dials it says what is wrong with it.
    ///
    /// A code with a rendezvous list yields its first entry: the address
    /// to try first.
    pub fn parse_with_server(text: &str) -> Result<(Self, Option<String>), CodeError> {
        let (code, list) = Self::parse_with_rendezvous(text)?;
        Ok((code, list.into_iter().next()))
    }

    /// The code and the text behind its `@`, whitespace gone, `None` for
    /// none or an empty one.
    fn parse_tail(text: &str) -> Result<(Self, Option<String>), CodeError> {
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        let (code, server) = match compact.split_once(SERVER_SEPARATOR) {
            Some((code, server)) => (code, Some(server).filter(|s| !s.is_empty())),
            None => (compact.as_str(), None),
        };
        let code = Self::parse_code(&code.to_ascii_lowercase())?;
        Ok((code, server.map(String::from)))
    }

    fn parse_code(compact: &str) -> Result<Self, CodeError> {
        let body = compact.strip_prefix(PREFIX).ok_or(CodeError::Prefix)?;
        let bytes = unbase32(body).ok_or(CodeError::Shape)?;
        if bytes.len() != BODY_LEN + CHECK_LEN {
            return Err(CodeError::Shape);
        }
        let (payload, check) = bytes.split_at(BODY_LEN);
        if leviculum_core::crypto::sha256(payload)[..CHECK_LEN] != *check {
            return Err(CodeError::Checksum);
        }
        let mut public_key = [0u8; 64];
        public_key.copy_from_slice(&payload[..64]);
        let mut token = [0u8; 16];
        token.copy_from_slice(&payload[64..]);
        Ok(Self { public_key, token })
    }
}

/// How many characters a code without an address has: the prefix and the
/// 84 bytes in base32.
pub const TYPED_LEN: usize = PREFIX.len() + ((BODY_LEN + CHECK_LEN) * 8).div_ceil(5);

/// How far a code typed by hand has come, on a device without a camera or
/// a clipboard (the M9 types it on its keys, 1359). Whitespace and case
/// are ignored as [`HomeCode::parse`] ignores them; positions count the
/// characters that are left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typing {
    /// So far so good: `typed` of [`TYPED_LEN`] characters.
    Short { typed: usize },
    /// The character at `at` cannot stand there: not the prefix's, not in
    /// the alphabet, or one past the end. Said at once, not after the
    /// last of 140.
    Stray { at: usize },
    /// All there: the code, or the checksum's verdict on a typo.
    Complete(Result<HomeCode, CodeError>),
}

/// Where `text`, typed so far, stands.
pub fn typing(text: &str) -> Typing {
    let compact: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let stray = compact.chars().enumerate().find(|&(at, c)| match at {
        _ if at >= TYPED_LEN => true,
        _ if at < PREFIX.len() => PREFIX.as_bytes()[at] != c as u8 || !c.is_ascii(),
        _ => !c.is_ascii() || !ALPHABET.contains(&(c as u8)),
    });
    match (stray, compact.len()) {
        (Some((at, _)), _) => Typing::Stray { at },
        (None, TYPED_LEN) => Typing::Complete(HomeCode::parse_code(&compact)),
        (None, typed) => Typing::Short { typed },
    }
}

/// `text` in groups of five with a space between, whitespace and case
/// gone: how a device shows a code typed into it, so a person keeps their
/// place against the code they copy from.
pub fn grouped(text: &str) -> String {
    let mut out = String::new();
    let compact = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase());
    for (i, c) in compact.enumerate() {
        if i > 0 && i % 5 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// RFC 4648 base32 in lower case, unpadded: the code's, and a backup's
/// ([`super::backup`]).
pub(super) fn base32(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 8 / 5 + 1);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[((buffer >> bits) & 31) as usize]));
        }
    }
    if bits > 0 {
        out.push(char::from(ALPHABET[((buffer << (5 - bits)) & 31) as usize]));
    }
    out
}

/// [`base32`] read back; `None` for a character outside the alphabet or a
/// tail with bits set.
pub(super) fn unbase32(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for c in text.bytes() {
        let value = ALPHABET.iter().position(|&a| a == c)? as u32;
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    // The tail holds fewer than eight bits, which must all be zero.
    (buffer & ((1 << bits) - 1) == 0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code() -> HomeCode {
        let mut public_key = [0u8; 64];
        for (i, b) in public_key.iter_mut().enumerate() {
            *b = i as u8 * 3;
        }
        HomeCode {
            public_key,
            token: [0xa5; 16],
        }
    }

    #[test]
    fn a_code_reads_back_as_it_was_written() {
        let text = code().encode();
        assert!(text.starts_with(PREFIX));
        assert_eq!(text.len(), PREFIX.len() + 135);
        assert_eq!(HomeCode::parse(&text), Ok(code()));
    }

    #[test]
    fn wrapped_or_capitalised_text_still_reads() {
        let text = code().encode();
        let (a, b) = text.split_at(60);
        let mangled = alloc::format!("  {}\n{} ", a.to_ascii_uppercase(), b);
        assert_eq!(HomeCode::parse(&mangled), Ok(code()));
    }

    #[test]
    fn a_typo_is_refused_by_the_checksum() {
        let mut text = code().encode().into_bytes();
        let at = PREFIX.len() + 20;
        text[at] = if text[at] == b'a' { b'b' } else { b'a' };
        let text = String::from_utf8(text).expect("ascii");
        assert_eq!(HomeCode::parse(&text), Err(CodeError::Checksum));
    }

    #[test]
    fn the_address_rides_behind_the_code() {
        let text = code().encode_with_server("homenode.example:4242");
        assert_eq!(
            HomeCode::parse_with_server(&text),
            Ok((code(), Some("homenode.example:4242".into())))
        );
        // A device that joins through its shared instance drops it.
        assert_eq!(HomeCode::parse(&text), Ok(code()));
        // An IPv6 address keeps its brackets, a host name its case.
        let text = code().encode_with_server("[fd00::1]:4242");
        assert_eq!(
            HomeCode::parse_with_server(&text).map(|(_, server)| server),
            Ok(Some("[fd00::1]:4242".into()))
        );
        let (a, b) = text.split_at(70);
        let wrapped = alloc::format!("{}\n{}", a.to_ascii_uppercase(), b);
        assert_eq!(
            HomeCode::parse_with_server(&wrapped),
            Ok((code(), Some("[fd00::1]:4242".into())))
        );
        let text = alloc::format!("{}@Home.Example:1", code().encode());
        assert_eq!(
            HomeCode::parse_with_server(&text).map(|(_, server)| server),
            Ok(Some("Home.Example:1".into()))
        );
    }

    #[test]
    fn the_rendezvous_list_rides_behind_the_code_and_one_entry_is_the_old_form() {
        let list = alloc::vec![
            String::from("home.example:4243"),
            String::from("leviculum.network:4343"),
            String::from("[fd00::1]:4343"),
        ];
        let text = code().encode_with_rendezvous(&list);
        assert_eq!(
            text,
            alloc::format!(
                "{}@home.example:4243,leviculum.network:4343,[fd00::1]:4343",
                code().encode()
            )
        );
        assert_eq!(HomeCode::parse_with_rendezvous(&text), Ok((code(), list)));
        // `loquax mailbox join CODE` takes the code whole, its list too (1471).
        assert_eq!(HomeCode::parse(&text), Ok(code()));
        // The first entry is the address to try first.
        assert_eq!(
            HomeCode::parse_with_server(&text).map(|(_, first)| first),
            Ok(Some("home.example:4243".into()))
        );
        // One entry is the old form, byte for byte, read by either call.
        let one = code().encode_with_rendezvous(&[String::from("h.example:1")]);
        assert_eq!(one, code().encode_with_server("h.example:1"));
        assert_eq!(
            HomeCode::parse_with_rendezvous(&one),
            Ok((code(), alloc::vec![String::from("h.example:1")]))
        );
        // None is the code alone; a wrapped list still reads.
        assert_eq!(code().encode_with_rendezvous(&[]), code().encode());
        assert_eq!(
            HomeCode::parse_with_rendezvous(&code().encode()),
            Ok((code(), Vec::new()))
        );
        let (a, b) = text.split_at(150);
        let wrapped = alloc::format!("{a}\n {b}");
        assert_eq!(
            HomeCode::parse_with_rendezvous(&wrapped).map(|(_, l)| l.len()),
            Ok(3)
        );
    }

    #[test]
    fn a_code_alone_or_with_an_empty_address_has_none() {
        let text = code().encode();
        assert_eq!(HomeCode::parse_with_server(&text), Ok((code(), None)));
        let text = alloc::format!("{text}@");
        assert_eq!(HomeCode::parse_with_server(&text), Ok((code(), None)));
        // The checksum still guards the code in front of an address.
        let mut broken = code().encode_with_server("h:1").into_bytes();
        let at = PREFIX.len() + 20;
        broken[at] = if broken[at] == b'a' { b'b' } else { b'a' };
        let broken = String::from_utf8(broken).expect("ascii");
        assert_eq!(
            HomeCode::parse_with_server(&broken),
            Err(CodeError::Checksum)
        );
    }

    #[test]
    fn other_text_is_not_a_code() {
        assert_eq!(
            HomeCode::parse("76cff79ab2f43bebcb7398e08e56ee0f"),
            Err(CodeError::Prefix)
        );
        assert_eq!(HomeCode::parse("lqxh1abc"), Err(CodeError::Shape));
        assert_eq!(HomeCode::parse("lqxh1ab!d"), Err(CodeError::Shape));
    }

    /// A code typed one character at a time is short until its last, then
    /// complete; a stray character is named where it stands; a typo the
    /// alphabet allows is the checksum's.
    #[test]
    fn a_typed_code_says_how_far_it_came_and_where_it_went_wrong() {
        let text = code().encode();
        assert_eq!(text.len(), TYPED_LEN);
        assert_eq!(TYPED_LEN, 140);
        for typed in 0..TYPED_LEN {
            assert_eq!(typing(&text[..typed]), Typing::Short { typed });
        }
        assert_eq!(typing(&text), Typing::Complete(Ok(code())));
        assert_eq!(
            typing(&grouped(&text.to_ascii_uppercase())),
            Typing::Complete(Ok(code()))
        );
        assert_eq!(typing("lqxj1"), Typing::Stray { at: 3 });
        assert_eq!(typing("lqxh1ab1"), Typing::Stray { at: 7 });
        assert_eq!(
            typing(&alloc::format!("{text}a")),
            Typing::Stray { at: 140 }
        );
        let mut typo = text.clone();
        typo.replace_range(60..61, if &text[60..61] == "a" { "b" } else { "a" });
        assert_eq!(typing(&typo), Typing::Complete(Err(CodeError::Checksum)));
    }

    #[test]
    fn a_code_is_shown_in_groups_of_five() {
        assert_eq!(grouped("LQXH1 abcdefg"), "lqxh1 abcde fg");
        assert_eq!(grouped(""), "");
    }
}
