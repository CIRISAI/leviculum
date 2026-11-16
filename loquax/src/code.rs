//! Every code a person hands Loquax, read in one place (1263, finding 1 of
//! `docs/drafts/ninety-percent-walk-2026-11.md`): the phone's scanner and
//! pencil, the terminal's palette and the window's field all ask
//! [`read`], so a code one of them takes, the others take too.
//!
//! Three kinds read:
//! - a friend's contact code, `lxma://<address>:<key>` ([`crate::lxma`]),
//!   the one every screen shows and shares;
//! - a friend's address alone, 32 hex digits, with or without `lxmf@`,
//!   spaces between groups of four allowed: the first start showed it as
//!   its QR before 1263, and an old screenshot must still work;
//! - a mailbox's join code, `lqxh1…` ([`crate::home::code`]).
//!
//! Anything else is said in one plain sentence ([`Unreadable::words`]),
//! never ignored.

use alloc::string::String;

use crate::address::ADDRESS_BYTES;
use crate::home::code::{HomeCode, PREFIX as MAILBOX_PREFIX};
use crate::lxma::{self, Contact};
use crate::ui::strings::{Lang, StrId};

/// What a code is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Code {
    /// A friend's contact code: the address and the key behind it.
    Contact(Contact),
    /// A friend's address without its key: the first message waits for
    /// its announce.
    Address([u8; ADDRESS_BYTES]),
    /// A mailbox's join code, as it was read: the join takes it whole,
    /// the server behind its `@` included.
    Mailbox(String),
}

impl Code {
    /// The friend's address, for a friend's code.
    pub fn address(&self) -> Option<[u8; ADDRESS_BYTES]> {
        match self {
            Self::Contact(contact) => Some(contact.address),
            Self::Address(address) => Some(*address),
            Self::Mailbox(_) => None,
        }
    }

    /// Its kind as the phone's facade names it: `contact`, `address` or
    /// `mailbox`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Contact(_) => "contact",
            Self::Address(_) => "address",
            Self::Mailbox(_) => "mailbox",
        }
    }
}

/// Why a text is no code Loquax reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unreadable {
    /// Nothing, or a text that is no code of ours: a web link, a ticket,
    /// someone else's QR.
    Foreign,
    /// One of ours, damaged: a contact code whose address is not its
    /// key's, a cut mailbox code, an address one digit short.
    Damaged,
}

impl Unreadable {
    /// The sentence a screen shows.
    pub fn words(self) -> StrId {
        match self {
            Self::Foreign => StrId::CodeForeign,
            Self::Damaged => StrId::CodeDamaged,
        }
    }

    /// [`Unreadable::words`] in `lang`.
    pub fn say(self, lang: Lang) -> &'static str {
        self.words().text(lang)
    }
}

/// Read `text`: a scan's, a paste's, a typed one. Whitespace around it is
/// ignored, and the case is.
pub fn read(text: &str) -> Result<Code, Unreadable> {
    let text = text.trim();
    if lxma::is_lxma(text) {
        return lxma::parse(text)
            .map(Code::Contact)
            .map_err(|_| Unreadable::Damaged);
    }
    if text
        .get(..MAILBOX_PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(MAILBOX_PREFIX))
    {
        return match HomeCode::parse_with_rendezvous(text) {
            Ok(_) => Ok(Code::Mailbox(String::from(text))),
            Err(_) => Err(Unreadable::Damaged),
        };
    }
    let bare = text.strip_prefix("lxmf@").unwrap_or(text);
    let digits: String = bare.chars().filter(|c| !c.is_whitespace()).collect();
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Unreadable::Foreign);
    }
    // Hex alone, the wrong count: an address cut or typed short. A long
    // run of hex is someone else's, a hash or a key.
    match crate::address::parse(&digits) {
        Ok(address) => Ok(Code::Address(address)),
        Err(_) if digits.len() < 2 * ADDRESS_BYTES + 8 => Err(Unreadable::Damaged),
        Err(_) => Err(Unreadable::Foreign),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    fn identity() -> leviculum_core::Identity {
        let private: [u8; 64] = core::array::from_fn(|i| i as u8 + 1);
        leviculum_core::Identity::from_private_key_bytes(&private).expect("a key")
    }

    fn contact_code() -> (String, [u8; 16]) {
        let key = identity().public_key_bytes();
        let code = lxma::encode(&key).expect("a code");
        let address = crate::home::lxmf_address(&key).expect("an address");
        (code, address)
    }

    fn mailbox_code() -> String {
        HomeCode {
            public_key: [0x3c; 64],
            token: [0x99; 16],
        }
        .encode_with_server("192.168.1.20:4242")
    }

    /// The table every frontend relies on: what reads, as what, and what
    /// is said for the rest.
    #[test]
    fn every_code_reads_as_its_kind_and_the_rest_is_said() {
        let (lxma_code, address) = contact_code();
        let hex = crate::address::to_hex(&address);
        let upper = hex.to_ascii_uppercase();
        let grouped: String = hex
            .as_bytes()
            .chunks(4)
            .map(|group| core::str::from_utf8(group).unwrap_or_default())
            .collect::<alloc::vec::Vec<_>>()
            .join(" ");
        let mailbox = mailbox_code();
        let mut broken = lxma_code.clone();
        broken.replace_range(7..8, if &broken[7..8] == "0" { "1" } else { "0" });

        let kind = |text: &str| read(text).map(|code| (code.kind(), code.address()));
        // lxma, either case, pasted with a newline.
        assert_eq!(kind(&lxma_code), Ok(("contact", Some(address))));
        assert_eq!(
            kind(&format!("  {}\n", lxma_code.to_ascii_uppercase())),
            Ok(("contact", Some(address)))
        );
        assert_eq!(
            kind(&format!("LXMA://{}", &lxma_code[7..])),
            Ok(("contact", Some(address)))
        );
        // lqxh1, with its server.
        assert_eq!(kind(&mailbox), Ok(("mailbox", None)));
        assert_eq!(read(&mailbox), Ok(Code::Mailbox(mailbox.clone())));
        // The bare address: lower, upper (the old QR's), grouped (the old
        // screen's), behind lxmf@.
        for text in [&hex, &upper, &grouped, &format!("lxmf@{hex}")] {
            assert_eq!(kind(text), Ok(("address", Some(address))), "{text}");
        }
        // Damaged: a code whose address is not its key's, a cut mailbox
        // code, an address one digit short.
        assert_eq!(kind(&broken), Err(Unreadable::Damaged));
        assert_eq!(kind(&mailbox[..12]), Err(Unreadable::Damaged));
        assert_eq!(kind(&hex[1..]), Err(Unreadable::Damaged));
        // Garbage: none of ours.
        for text in [
            "",
            "   ",
            "https://example.org/",
            "WIFI:S:home;T:WPA;P:secret;;",
            "lqxb1aaaa",
            &"ab".repeat(32),
        ] {
            assert_eq!(kind(text), Err(Unreadable::Foreign), "{text:?}");
        }
    }

    /// Each refusal is one sentence in both languages, and none speaks
    /// Reticulum.
    #[test]
    fn a_refusal_is_one_plain_sentence() {
        for why in [Unreadable::Foreign, Unreadable::Damaged] {
            for lang in Lang::ALL {
                let said = why.say(lang);
                assert!(said.ends_with('.'), "{said}");
                for word in ["LXMF", "hex", "lxma", "lqxh1", "Reticulum", "hash"] {
                    assert!(!said.contains(word), "{said}");
                }
            }
        }
    }
}
