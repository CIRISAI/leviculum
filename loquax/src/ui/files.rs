//! Files in a chat (1221; Lew 2026-10-31): sent and received as Columba
//! and Sideband send them, the LXMF file attachments field `0x05`
//! (`reference/LXMF/LXMF/LXMF.py:12`) holding `[[name, bytes], …]`.
//! Columba writes the name as a str,
//! `options.fileAttachments.map { (name, data) -> listOf(name, data) }` in
//! `columba/rns-backend-kt/src/main/kotlin/network/columba/app/rns/backend/kt/NativeMessageSender.kt:141-145`,
//! because Sideband renders the name with `str(name)` and a bin shows up as
//! its `b'...'` repr
//! (`columba/rns-backend-py/src/main/kotlin/network/columba/app/rns/backend/py/PythonRnsLxmf.kt:520-531`).
//! Columba's reader takes any entry of two or more elements and a name
//! that is either
//! (`columba/app/src/main/java/network/columba/app/ui/model/MessageMapper.kt:784-800`),
//! so ours reads leniently too.
//!
//! A file is one message with an empty text, as the paperclip sends it.
//! What may go is bounded by what the receiver takes ([`LARGEST`]); the
//! name a sender wrote is the sender's, so a file is saved under a name
//! [`safe_name`] made of it.

use alloc::string::String;
use alloc::vec::Vec;

use leviculum_lxmf::attachments::{FileAttachment, MessageAttachments};
use leviculum_lxmf::constants::FIELD_FILE_ATTACHMENTS;
use leviculum_lxmf::msgpack;
use leviculum_lxmf::Field;

use super::photos::size_words;
use super::strings::{Arg, Lang, StrId, Text};
use super::{UiEffect, UiModel};

/// The most bytes a file may carry. A receiver refuses a delivery resource
/// above `DELIVERY_LIMIT` (1000 KB, `reference/LXMF/LXMF/LXMRouter.py:60`,
/// applied at `:1979`; ours is
/// [`leviculum_lxmf::constants::DELIVERY_LIMIT_BYTES`]), and the message
/// around the file costs its header, signature, title, the name and the
/// field's framing: 990 000 leaves 10 000 for them, as a photo does
/// ([`crate::ui::photos`]).
pub const LARGEST: usize = 990_000;

/// The longest name that goes, in bytes: what every file system takes.
pub const NAME_MAX: usize = 255;

/// What a bubble needs of one file without its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRef {
    /// The name the sender wrote, as written.
    pub name: String,
    /// The file's bytes on the wire.
    pub bytes: usize,
}

/// The files `fields` carry, names and bytes borrowed, in the sender's
/// order; none for no field. Read as Columba reads them: an entry of two
/// or more elements whose name is a str or a bin; an entry that is not
/// is skipped, the rest still read.
pub fn file_parts(fields: &[Field]) -> Vec<(String, &[u8])> {
    let Some((_, value)) = fields
        .iter()
        .find(|(key, _)| *key == FIELD_FILE_ATTACHMENTS)
    else {
        return Vec::new();
    };
    let mut at = 0;
    let Ok(count) = msgpack::array_len(value, &mut at) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for _ in 0..count {
        let Ok(len) = msgpack::array_len(value, &mut at) else {
            break;
        };
        let start = at;
        let name = match msgpack::read_str(value, &mut at) {
            Ok(name) => Some(String::from(name)),
            Err(_) => {
                at = start;
                msgpack::read_bin(value, &mut at)
                    .ok()
                    .map(|name| String::from_utf8_lossy(name).into_owned())
            }
        };
        let data = match name {
            Some(_) if len >= 2 => msgpack::read_bin(value, &mut at).ok(),
            _ => None,
        };
        // The elements left in this entry, skipped: a third Columba might
        // add, or what did not read.
        let read = match (&name, &data) {
            (Some(_), Some(_)) => 2,
            (Some(_), None) if len >= 2 => {
                at = start;
                0
            }
            (Some(_), None) => 1,
            (None, _) => {
                at = start;
                0
            }
        };
        for _ in read..len {
            if msgpack::skip(value, &mut at).is_err() {
                return files;
            }
        }
        if let (Some(name), Some(data)) = (name, data) {
            files.push((name, data));
        }
    }
    files
}

/// The files `fields` carry.
pub fn files_of(fields: &[Field]) -> Vec<FileRef> {
    file_parts(fields)
        .into_iter()
        .map(|(name, data)| FileRef {
            name,
            bytes: data.len(),
        })
        .collect()
}

/// The `index`th file `fields` carry, its name and bytes.
pub fn file_at(fields: &[Field], index: usize) -> Option<(String, &[u8])> {
    file_parts(fields).into_iter().nth(index)
}

/// The fields of a file: the attachments field alone, one entry,
/// `[[name, bytes]]`, the name a str as Columba writes it.
pub fn file_fields(name: &str, data: Vec<u8>) -> Vec<Field> {
    MessageAttachments {
        files: alloc::vec![FileAttachment {
            name: send_name(name),
            data,
        }],
        ..MessageAttachments::default()
    }
    .into_fields()
}

/// The name a picked file goes under: its last path element, cut to
/// [`NAME_MAX`] bytes on a character, `file` for none.
pub fn send_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default().trim();
    let mut out = String::new();
    for c in last.chars() {
        if out.len() + c.len_utf8() > NAME_MAX {
            break;
        }
        out.push(c);
    }
    match out.is_empty() {
        true => String::from("file"),
        false => out,
    }
}

/// A name a received file may be saved under: what the sender wrote
/// without a path, a control character, a separator, a reserved
/// character of Windows or Android's storage, a leading dot or a
/// trailing dot or space, cut to 120 bytes keeping its extension; `file`
/// for nothing left. Never `.` or `..`, never a path.
pub fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned
        .trim_start_matches(['.', ' '])
        .trim_end_matches(['.', ' ']);
    const KEEP: usize = 120;
    let cut = match cleaned.len() <= KEEP {
        true => String::from(cleaned),
        false => {
            let ext = cleaned
                .rfind('.')
                .map(|dot| &cleaned[dot..])
                .filter(|ext| ext.len() <= 16)
                .unwrap_or_default();
            let mut stem = String::new();
            for c in cleaned.chars() {
                if stem.len() + c.len_utf8() + ext.len() > KEEP {
                    break;
                }
                stem.push(c);
            }
            alloc::format!("{stem}{ext}")
        }
    };
    match cut.is_empty() {
        true => String::from("file"),
        false => cut,
    }
}

/// Why a picked file does not go, in the reader's words: larger than
/// [`LARGEST`]; none for one that goes.
pub fn refusal(bytes: usize) -> Option<Text> {
    (bytes > LARGEST).then(|| {
        Text::Key(
            StrId::FileTooLarge,
            alloc::vec![
                Arg::Str(size_words(bytes as u64)),
                Arg::Str(size_words(LARGEST as u64))
            ],
        )
    })
}

/// What a screen reader says for a file: `File report.pdf, 312 KB`, the
/// name as [`safe_name`] shows it.
pub fn file_words(file: &FileRef) -> Text {
    Text::Key(
        StrId::File,
        alloc::vec![
            Arg::Str(safe_name(&file.name)),
            Arg::Str(size_words(file.bytes as u64))
        ],
    )
}

impl<B> UiModel<B> {
    /// [`super::UiEvent::SendFile`]: the file goes as a message of its own
    /// in `peer`'s conversation and the view follows it; one larger than a
    /// receiver takes is refused with a toast saying both sizes.
    pub(super) fn send_file(&mut self, peer: [u8; 16], name: &str, data: Vec<u8>) -> Vec<UiEffect> {
        if let Some(why) = refusal(data.len()) {
            self.toast(leviculum_ui::toast::ToastKind::Error, why.resolve(Lang::En));
            return Vec::new();
        }
        let index = self.conversation(peer);
        let effects = self.send_fields(index, String::new(), file_fields(name, data));
        if index == self.selected {
            self.message_cursor = self.conversations[index].messages.len().saturating_sub(1);
            self.scroll_top = None;
        }
        effects
    }

    /// [`super::UiEvent::OpenFile`]: the `index`th file of a message of
    /// `peer`'s is saved and opened; any other tap does nothing.
    pub(super) fn open_file(&mut self, peer: [u8; 16], key: u64, index: usize) -> Vec<UiEffect> {
        let carries = self
            .conversations
            .iter()
            .find(|c| c.peer == peer)
            .and_then(|c| c.messages.iter().find(|m| m.activity == key))
            .is_some_and(|m| file_at(&m.fields, index).is_some());
        match carries {
            true => alloc::vec![UiEffect::OpenFile { peer, key, index }],
            false => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// The field is what the reference packs for
    /// `fields[FIELD_FILE_ATTACHMENTS] = [["a.txt", data]]`
    /// (`umsgpack.packb([["a.txt", b"\x01\x02"]])`).
    #[test]
    fn the_field_is_the_references_list_of_name_and_bytes() {
        let fields = file_fields("a.txt", vec![1, 2]);
        assert_eq!(
            fields,
            vec![(
                FIELD_FILE_ATTACHMENTS,
                vec![0x91, 0x92, 0xa5, b'a', b'.', b't', b'x', b't', 0xc4, 0x02, 0x01, 0x02]
            )]
        );
        assert_eq!(
            files_of(&fields),
            vec![FileRef {
                name: "a.txt".into(),
                bytes: 2
            }]
        );
        assert_eq!(file_at(&fields, 0), Some(("a.txt".into(), &[1, 2][..])));
        assert_eq!(file_at(&fields, 1), None);
    }

    /// A bin name (Sideband's own test peer sends one), a third element,
    /// and a broken entry between two good ones: read as Columba reads.
    #[test]
    fn bin_names_extra_elements_and_broken_entries_are_read_as_columba_reads_them() {
        let value = vec![
            0x93, // three entries
            0x93, 0xc4, 0x01, b'x', 0xc4, 0x01, 0x07, 0xc0, // [b"x", b"\x07", nil]
            0x91, 0xa1, b'y', // ["y"]: no bytes
            0x92, 0xa1, b'z', 0xc4, 0x00, // ["z", b""]
        ];
        let fields = vec![(FIELD_FILE_ATTACHMENTS, value)];
        assert_eq!(
            file_parts(&fields),
            vec![("x".into(), &[0x07][..]), ("z".into(), &[][..])]
        );
    }

    #[test]
    fn a_malformed_field_is_no_file() {
        assert!(files_of(&[(FIELD_FILE_ATTACHMENTS, vec![0x01])]).is_empty());
        assert!(files_of(&[(FIELD_FILE_ATTACHMENTS, vec![0x91, 0x01])]).is_empty());
        assert!(files_of(&[(FIELD_FILE_ATTACHMENTS, vec![0x91, 0x92, 0x01, 0x02])]).is_empty());
        assert!(files_of(&[]).is_empty());
    }

    /// A sender's name never leaves the folder it is saved in.
    #[test]
    fn a_received_name_is_made_safe() {
        assert_eq!(safe_name("report.pdf"), "report.pdf");
        assert_eq!(safe_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(safe_name(".."), "file");
        assert_eq!(safe_name("."), "file");
        assert_eq!(safe_name(""), "file");
        assert_eq!(safe_name(".bashrc"), "bashrc");
        assert_eq!(safe_name("C:\\x\\a?.txt"), "C__x_a_.txt");
        assert_eq!(safe_name("a\nb\u{0}.txt "), "a_b_.txt");
        assert_eq!(safe_name("Grüße.odt"), "Grüße.odt");
        let long = alloc::format!("{}.tar.gz", "ä".repeat(200));
        let cut = safe_name(&long);
        assert!(cut.len() <= 120, "{}", cut.len());
        assert!(cut.ends_with(".gz") && cut.starts_with('ä'), "{cut}");
    }

    #[test]
    fn a_sent_name_is_its_last_element_and_fits() {
        assert_eq!(send_name("/storage/x/report.pdf"), "report.pdf");
        assert_eq!(send_name("C:\\Users\\a\\b.txt"), "b.txt");
        assert_eq!(send_name("  "), "file");
        assert_eq!(send_name(&"ü".repeat(200)).len(), 254);
    }

    /// The cap and what it says: 990 000 goes, one more is refused with
    /// both sizes in the reader's words.
    #[test]
    fn the_cap_is_a_receivers_limit_and_says_both_sizes() {
        assert_eq!(refusal(LARGEST), None);
        let why = refusal(4_200_000).expect("refused");
        assert_eq!(
            why.resolve(Lang::En),
            "This file is 4.2 MB. A message carries at most 990 KB."
        );
        assert_eq!(
            why.resolve(Lang::De),
            "Diese Datei hat 4.2 MB. Eine Nachricht trägt höchstens 990 KB."
        );
        assert!(LARGEST as u64 + 10_000 <= leviculum_lxmf::constants::DELIVERY_LIMIT_BYTES);
    }

    #[test]
    fn a_file_says_its_name_and_size() {
        let file = FileRef {
            name: "report.pdf".into(),
            bytes: 312_400,
        };
        assert_eq!(
            file_words(&file).resolve(Lang::En),
            "File report.pdf, 313 KB"
        );
        assert_eq!(
            file_words(&file).resolve(Lang::De),
            "Datei report.pdf, 313 KB"
        );
    }
}
