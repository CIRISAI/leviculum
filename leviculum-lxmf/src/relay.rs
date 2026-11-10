//! Relayed posts (1482): what a distribution bot puts on a member's
//! message when it hands everyone a copy.
//!
//! SebastianObi's LXMF-Tools `lxmf_distribution_group_extended` is the one
//! seen on the network ("ReticulumNet NL"). On its default configuration a
//! member writes it a plain 1:1 message; it puts
//! `send_prefix = !source_name!!n!<!source_address!>!n!` in front of the
//! text (LXMF-Tools at 6ec9528,
//! `LXMF-Tools/lxmf_distribution_group_extended/lxmf_distribution_group_extended.py:4766`,
//! replaced in `replace()` at `:3373-3377`), keeps the writer's timestamp and fields
//! and signs one copy to every member itself. With `fields_message` on
//! (off by default, `:4561`) the copy carries the writer as field `0xB5`,
//! `[bin16 source, str name]`, and the original's hash as `0xA9`
//! (`:1565-1571`, constants `:120-153`).
//!
//! So the copy's source is the bot, and the writer is only in the text (or
//! in `0xB5`). These readers say who wrote a copy and what it says beyond
//! the prefix. The prefix is the operator's to change: a text without it
//! is simply no relayed post here.

use alloc::string::String;

use crate::lev::Lev;
use crate::message::Field;
use crate::msgpack;

/// The bot's field for the writer of a copy, `[bin16 source, str name]`
/// (`MSG_FIELD_SRC`).
pub const FIELD_RELAY_SOURCE: i64 = 0xB5;

/// The bot's field for the hash of the writer's original message
/// (`MSG_FIELD_HASH`).
pub const FIELD_RELAY_HASH: i64 = 0xA9;

/// The bot's field for the kind of message, 4 for a group's
/// (`MSG_FIELD_TYPE`).
pub const FIELD_RELAY_TYPE: i64 = 0xBD;

/// A copy's writer as its relay names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Writer<'a> {
    /// The writer's LXMF address.
    pub address: [u8; 16],
    /// The writer's name as the relay knows it; empty when it gave none.
    pub name: String,
    /// The text after the relay's prefix: the writer's own words.
    pub text: &'a str,
}

/// The default prefix at the start of `body`: `Name\n<32 hex>\n`, then the
/// writer's words, which may be none. `None` for any other text.
pub fn prefix(body: &str) -> Option<Writer<'_>> {
    let (name, rest) = body.split_once('\n')?;
    let (hash, text) = rest.split_once('\n')?;
    let hex = hash.strip_prefix('<')?.strip_suffix('>')?;
    if name.trim().is_empty() || hex.len() != 32 {
        return None;
    }
    let mut address = [0u8; 16];
    for (i, byte) in address.iter_mut().enumerate() {
        let pair = hex.get(2 * i..2 * i + 2)?;
        if !pair
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return None;
        }
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(Writer {
        address,
        name: name.trim().into(),
        text,
    })
}

/// The writer `0xB5` names, `[bin16 source, str name]`: the address and
/// the name. `None` without the field or of another shape.
pub fn source_field(fields: &[Field]) -> Option<([u8; 16], String)> {
    let (_, value) = fields
        .iter()
        .rev()
        .find(|(key, _)| *key == FIELD_RELAY_SOURCE)?;
    let mut p = 0;
    if msgpack::array_len(value, &mut p).ok()? < 2 {
        return None;
    }
    let address = <[u8; 16]>::try_from(msgpack::read_bin(value, &mut p).ok()?).ok()?;
    // The name as a str, or as Python's bytes.
    let mut at = p;
    let name = match msgpack::read_str(value, &mut at) {
        Ok(name) => String::from(name),
        Err(_) => String::from_utf8_lossy(msgpack::read_bin(value, &mut p).ok()?).into_owned(),
    };
    Some((address, name))
}

/// The writer of a copy from `source`, when it names one that is not
/// `source`: `0xB5` first, else the default prefix. The text is what the
/// bubble shows: after the prefix when the body has one.
pub fn writer<'a>(body: &'a str, fields: &[Field], source: &[u8; 16]) -> Option<Writer<'a>> {
    let prefixed = prefix(body);
    let found = match source_field(fields) {
        Some((address, name)) => Writer {
            address,
            name: match (&prefixed, name.trim().is_empty()) {
                (Some(p), true) => p.name.clone(),
                _ => name.trim().into(),
            },
            text: prefixed.as_ref().map_or(body, |p| p.text),
        },
        None => prefixed?,
    };
    (found.address != *source).then_some(found)
}

/// Whether a message from `source` says it is another's: its fields are
/// then the writer's, passed through, and none of them is `source`'s word
/// (the `lev` mark, a ticket).
pub fn passed_through(body: &[u8], fields: &[Field], source: &[u8; 16]) -> bool {
    let body = core::str::from_utf8(body).unwrap_or("");
    writer(body, fields, source).is_some()
}

/// The `lev` namespace of a message `source` sent, as `source`'s own word
/// (1482): none when the map names another writer
/// ([`Lev::from_fields_of`]), and none when it names no writer (a writer
/// before 1482) but the message says it is another's copy
/// ([`passed_through`]). What is left may ask for receipts; what is not
/// was passed through by a relay, which must get none.
pub fn own_lev(body: &[u8], fields: &[Field], source: &[u8; 16]) -> Option<Lev> {
    let lev = Lev::from_fields(fields)?;
    match lev.writer {
        Some(writer) => (writer == *source).then_some(lev),
        None => (!passed_through(body, fields, source)).then_some(lev),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    const ANNA: [u8; 16] = [0xa1; 16];
    const BOT: [u8; 16] = [0xb0; 16];

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| alloc::format!("{b:02x}")).collect()
    }

    #[test]
    fn the_default_prefix_names_the_writer_and_leaves_the_words() {
        let body = alloc::format!("Anna\n<{}>\nHallo zusammen\nzweite Zeile", hex(&ANNA));
        let writer = prefix(&body).expect("prefixed");
        assert_eq!(writer.address, ANNA);
        assert_eq!(writer.name, "Anna");
        assert_eq!(writer.text, "Hallo zusammen\nzweite Zeile");
        let empty = alloc::format!("Anna\n<{}>\n", hex(&ANNA));
        assert_eq!(prefix(&empty).expect("prefixed").text, "");
    }

    #[test]
    fn other_texts_are_no_prefix() {
        let joined = alloc::format!("Anna <{}> joins the group.", hex(&ANNA));
        let upper = alloc::format!("Anna\n<{}>\nHallo", hex(&ANNA).to_uppercase());
        let short = "Anna\n<a1a1>\nHallo";
        let no_name = alloc::format!("\n<{}>\nHallo", hex(&ANNA));
        let changed = "[Anna] Hallo";
        for body in [
            joined.as_str(),
            &upper,
            short,
            &no_name,
            changed,
            "",
            "Hallo\nWelt",
        ] {
            assert_eq!(prefix(body), None, "{body:?}");
        }
    }

    #[test]
    fn the_source_field_wins_and_its_own_address_is_no_writer() {
        let mut value = Vec::new();
        msgpack::array(&mut value, 2);
        msgpack::bin(&mut value, &ANNA);
        msgpack::string(&mut value, "Anna B.");
        let fields = alloc::vec![(FIELD_RELAY_SOURCE, value)];
        let w = writer("[Anna] Hallo", &fields, &BOT).expect("the field names her");
        assert_eq!(
            (w.address, w.name.as_str(), w.text),
            (ANNA, "Anna B.", "[Anna] Hallo")
        );
        assert_eq!(
            writer("[Anna] Hallo", &fields, &ANNA),
            None,
            "her own message"
        );
        let body = alloc::format!("Anna\n<{}>\nHallo", hex(&BOT));
        assert_eq!(writer(&body, &[], &BOT), None, "the bot's own line");
        assert!(passed_through(
            alloc::format!("Anna\n<{}>\nHallo", hex(&ANNA)).as_bytes(),
            &[],
            &BOT
        ));
        assert!(!passed_through(b"Hallo", &[], &BOT));
    }

    #[test]
    fn a_mark_is_the_sources_only_when_it_says_so_or_nothing_says_otherwise() {
        let copy = alloc::format!("Anna\n<{}>\nHallo", hex(&ANNA));
        let named = |writer| {
            Lev {
                writer,
                ..Lev::mark()
            }
            .fields()
            .to_vec()
        };
        assert_eq!(own_lev(copy.as_bytes(), &named(Some(ANNA)), &BOT), None);
        assert_eq!(own_lev(b"Hallo", &named(Some(ANNA)), &BOT), None);
        assert_eq!(
            own_lev(copy.as_bytes(), &named(None), &BOT),
            None,
            "before 1482"
        );
        assert!(
            own_lev(b"Hallo", &named(None), &BOT).is_some(),
            "before 1482"
        );
        assert!(own_lev(copy.as_bytes(), &named(Some(BOT)), &BOT).is_some());
        assert_eq!(own_lev(b"Hallo", &[], &BOT), None, "no lev");
    }
}
