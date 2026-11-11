//! Photos in a chat (1220; Lew 2026-10-31): sent and received as Columba
//! and Sideband send them, the LXMF image field `0x06` holding
//! `[format, bytes]` (`reference/LXMF/LXMF/LXMF.py:13`). Columba writes
//! `listOf(imageFormat, imageData)` in
//! `columba/rns-backend-kt/src/main/kotlin/network/columba/app/rns/backend/kt/NativeMessageSender.kt:133`
//! and reads the bytes with `BitmapFactory`, whatever the format word
//! says, in
//! `columba/app/src/main/java/network/columba/app/ui/model/MessageMapper.kt:511-547`.
//!
//! The model knows the field and the reader's size choice; the pixels are
//! the shell's (`crate::photo`, behind the `photos` feature): it scales
//! a picked picture before [`super::UiEvent::SendPhoto`] and decodes the
//! thumbnail a bubble shows. A photo is one message with an empty text,
//! as Columba sends one without a caption.

use alloc::string::String;
use alloc::vec::Vec;

use leviculum_lxmf::attachments::{ImageAttachment, MessageAttachments};
use leviculum_lxmf::constants::FIELD_IMAGE;
use leviculum_lxmf::msgpack;
use leviculum_lxmf::Field;

use super::{UiEffect, UiModel};

/// How far a photo is scaled before it goes: the setting under Settings ›
/// Chats, off the main path. [`PhotoSize::Phone`] is the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PhotoSize {
    /// Strongly reduced, for slow links: 800 px, JPEG quality 70, at
    /// most `photo::SHOWN_BY_COLUMBA` bytes.
    Small,
    /// Looks good on a phone: 1600 px, JPEG quality 80, at most
    /// `photo::SHOWN_BY_COLUMBA` bytes (250 000), which Columba shows.
    #[default]
    Phone,
    /// As taken; only its metadata goes.
    Original,
}

impl PhotoSize {
    pub const ALL: [PhotoSize; 3] = [PhotoSize::Small, PhotoSize::Phone, PhotoSize::Original];

    /// The size after this one, the last one's the first: what a press
    /// on the settings' row sets.
    pub fn next(self) -> Self {
        let at = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Self::ALL[(at + 1) % Self::ALL.len()]
    }

    /// The longest edge in pixels it scales to, and the JPEG quality;
    /// `None` for the original.
    pub fn target(self) -> Option<(u32, u8)> {
        match self {
            PhotoSize::Small => Some((800, 70)),
            PhotoSize::Phone => Some((1600, 80)),
            PhotoSize::Original => None,
        }
    }

    /// The config's word (`photo_size`) and the facade's.
    pub fn word(self) -> &'static str {
        match self {
            PhotoSize::Small => "small",
            PhotoSize::Phone => "phone",
            PhotoSize::Original => "original",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|size| size.word() == word)
    }
}

/// What a bubble needs of a photo without decoding it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhotoRef {
    /// The format word the sender wrote: `jpg`, `webp`, `png`.
    pub format: String,
    /// The picture's bytes on the wire; a message carries at most 1000
    /// KB, so 32 bits hold any, and the bubble stays the size it had
    /// before [`PhotoRef::file`] (the M9's heap census).
    pub bytes: u32,
    /// Where a picture sent as a file is among the message's files
    /// (1662): its bubble shows the thumbnail and the file's line under
    /// it. None for the image field's photo; a message's 65 536th file
    /// and later show no picture.
    pub file: Option<u16>,
}

/// The format word and the picture's bytes `fields` carry, borrowed;
/// none for no image field or a malformed one. Read leniently, as Columba
/// reads it, taking the second element of any array of two or more
/// (`columba/app/src/main/java/network/columba/app/ui/model/MessageMapper.kt:537-539`):
/// the format may be a str or a bin, and elements after the bytes are
/// ignored.
pub fn photo_parts(fields: &[Field]) -> Option<(String, &[u8])> {
    let (_, value) = fields.iter().find(|(key, _)| *key == FIELD_IMAGE)?;
    let mut at = 0;
    if msgpack::array_len(value, &mut at).ok()? < 2 {
        return None;
    }
    let start = at;
    let format = match msgpack::read_str(value, &mut at) {
        Ok(word) => String::from(word),
        Err(_) => {
            at = start;
            String::from_utf8_lossy(msgpack::read_bin(value, &mut at).ok()?).into_owned()
        }
    };
    let data = msgpack::read_bin(value, &mut at).ok()?;
    Some((format, data))
}

/// The photo `fields` carry, if one; a malformed field is none.
pub fn photo_of(fields: &[Field]) -> Option<PhotoRef> {
    photo_parts(fields).map(|(format, data)| PhotoRef {
        format,
        bytes: u32::try_from(data.len()).unwrap_or(u32::MAX),
        file: None,
    })
}

/// The picture's bytes `fields` carry, if a photo.
pub fn photo_bytes(fields: &[Field]) -> Option<&[u8]> {
    photo_parts(fields).map(|(_, data)| data)
}

/// The format word of `data` by its first bytes, for a picture this
/// build decodes (`crate::photo`: JPEG, PNG, WebP, GIF); none for any
/// other bytes, whatever a name says (1662).
pub fn image_format(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("png")
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some("gif")
    } else if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

/// The picture a bubble shows (1662): its photo, else the first of its
/// files whose bytes are a picture, its format word, bytes and place
/// among the files. A picture sent as a file shows its thumbnail as a
/// photo does, with the file's line under it.
pub fn picture_parts(fields: &[Field]) -> Option<(String, &[u8], Option<u16>)> {
    if let Some((format, data)) = photo_parts(fields) {
        return Some((format, data, None));
    }
    super::files::file_parts(fields)
        .into_iter()
        .enumerate()
        .find_map(|(index, (_, data))| {
            let index = u16::try_from(index).ok()?;
            image_format(data).map(|format| (String::from(format), data, Some(index)))
        })
}

/// [`picture_parts`] without its bytes.
pub fn picture_of(fields: &[Field]) -> Option<PhotoRef> {
    picture_parts(fields).map(|(format, data, file)| PhotoRef {
        format,
        bytes: u32::try_from(data.len()).unwrap_or(u32::MAX),
        file,
    })
}

/// The bytes of the picture a bubble shows ([`picture_parts`]).
pub fn picture_bytes(fields: &[Field]) -> Option<&[u8]> {
    picture_parts(fields).map(|(_, data, _)| data)
}

/// The fields of a photo: the image field alone, `[format, bytes]`.
pub fn photo_fields(format: &str, data: Vec<u8>) -> Vec<Field> {
    MessageAttachments {
        image: Some(ImageAttachment {
            format: String::from(format),
            data,
        }),
        ..MessageAttachments::default()
    }
    .into_fields()
}

/// `bytes` as a person reads a size: `312 KB`, `1.4 MB`, and the maps'
/// `1.2 GB` (1290).
pub fn size_words(bytes: u64) -> String {
    if bytes < 1000 {
        alloc::format!("{bytes} B")
    } else if bytes < 1_000_000 {
        alloc::format!("{} KB", bytes.div_ceil(1000))
    } else if bytes < 1_000_000_000 {
        alloc::format!("{}.{} MB", bytes / 1_000_000, (bytes % 1_000_000) / 100_000)
    } else {
        alloc::format!(
            "{}.{} GB",
            bytes / 1_000_000_000,
            (bytes % 1_000_000_000) / 100_000_000
        )
    }
}

impl<B> UiModel<B> {
    /// [`super::UiEvent::SendPhoto`]: the photo goes as a message of its
    /// own in `peer`'s conversation, and the view follows it as it follows
    /// a text.
    pub(super) fn send_photo(
        &mut self,
        peer: [u8; 16],
        format: &str,
        data: Vec<u8>,
    ) -> Vec<UiEffect> {
        if data.is_empty() {
            return Vec::new();
        }
        let index = self.conversation(peer);
        let effects = self.send_fields(index, String::new(), photo_fields(format, data));
        if index == self.selected {
            self.message_cursor = self.conversations[index].messages.len().saturating_sub(1);
            self.scroll_top = None;
        }
        effects
    }

    /// [`super::UiEvent::ViewPhoto`]: a message of `peer`'s with a photo
    /// is shown whole; any other tap does nothing.
    pub(super) fn view_photo(&mut self, peer: [u8; 16], key: u64) -> Vec<UiEffect> {
        let carries = self
            .conversations
            .iter()
            .find(|c| c.peer == peer)
            .and_then(|c| c.messages.iter().find(|m| m.activity == key))
            .is_some_and(|m| picture_of(&m.fields).is_some());
        match carries {
            true => alloc::vec![UiEffect::ShowPhoto { peer, key }],
            false => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// The field is what the reference packs for
    /// `fields[FIELD_IMAGE] = ["jpg", data]`: a two-element array, a str
    /// and a bin (`umsgpack.packb(["jpg", b"\xff\xd8"])`).
    #[test]
    fn the_field_is_the_references_format_and_bytes() {
        let fields = photo_fields("jpg", vec![0xff, 0xd8]);
        assert_eq!(
            fields,
            vec![(
                FIELD_IMAGE,
                vec![0x92, 0xa3, b'j', b'p', b'g', 0xc4, 0x02, 0xff, 0xd8]
            )]
        );
        assert_eq!(
            photo_of(&fields),
            Some(PhotoRef {
                format: "jpg".into(),
                bytes: 2,
                file: None,
            })
        );
        assert_eq!(photo_bytes(&fields), Some(&[0xff, 0xd8][..]));
    }

    /// A format sent as bin, and a third element, still read.
    #[test]
    fn a_bin_format_and_extra_elements_are_read_as_columba_reads_them() {
        let value = vec![
            0x93, 0xc4, 0x04, b'w', b'e', b'b', b'p', 0xc4, 0x01, 0x09, 0xc0,
        ];
        let fields = vec![(FIELD_IMAGE, value)];
        assert_eq!(photo_parts(&fields), Some(("webp".into(), &[0x09][..])));
    }

    /// 1662: a file whose bytes are a picture is the bubble's picture,
    /// told by its bytes and not its name; a photo comes first; a file
    /// that is no picture is none, whatever it is called.
    #[test]
    fn a_picture_sent_as_a_file_is_told_by_its_bytes() {
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
        assert_eq!(image_format(&png), Some("png"));
        assert_eq!(image_format(&[0xff, 0xd8, 0xff, 0xe0]), Some("jpg"));
        assert_eq!(image_format(b"GIF89a.."), Some("gif"));
        assert_eq!(image_format(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(image_format(b"RIFF\0\0\0\0WAVEfmt "), None);
        assert_eq!(image_format(b"%PDF-1.7"), None);
        assert_eq!(image_format(&[]), None);
        let mut fields = super::super::files::file_fields("notes.txt", b"hello".to_vec());
        assert_eq!(picture_of(&fields), None, "a text file is no picture");
        let disguised = MessageAttachments {
            files: vec![
                leviculum_lxmf::attachments::FileAttachment {
                    name: "report.pdf".into(),
                    data: b"%PDF".to_vec(),
                },
                leviculum_lxmf::attachments::FileAttachment {
                    name: "scan.dat".into(),
                    data: png.to_vec(),
                },
            ],
            ..MessageAttachments::default()
        }
        .into_fields();
        assert_eq!(
            picture_of(&disguised),
            Some(PhotoRef {
                format: "png".into(),
                bytes: 10,
                file: Some(1),
            })
        );
        assert_eq!(picture_bytes(&disguised), Some(&png[..]));
        assert_eq!(photo_of(&disguised), None, "still no photo field");
        // The image field's photo comes first.
        fields.extend(photo_fields("jpg", vec![0xff, 0xd8, 0xff]));
        assert_eq!(picture_of(&fields).and_then(|p| p.file), None);
        assert_eq!(picture_of(&fields).map(|p| p.format), Some("jpg".into()));
    }

    #[test]
    fn a_malformed_field_is_no_photo() {
        assert_eq!(photo_of(&[(FIELD_IMAGE, vec![0x01])]), None);
        assert_eq!(photo_of(&[(FIELD_IMAGE, vec![0x91, 0xa1, b'x'])]), None);
        assert_eq!(photo_of(&[]), None);
    }

    #[test]
    fn the_sizes_are_lews_and_phone_is_the_default() {
        assert_eq!(PhotoSize::default(), PhotoSize::Phone);
        assert_eq!(PhotoSize::Phone.target(), Some((1600, 80)));
        assert_eq!(PhotoSize::Small.target(), Some((800, 70)));
        assert_eq!(PhotoSize::Original.target(), None);
        for size in PhotoSize::ALL {
            assert_eq!(PhotoSize::from_word(size.word()), Some(size));
        }
        assert_eq!(PhotoSize::from_word("huge"), None);
    }

    #[test]
    fn sizes_read_as_people_say_them() {
        assert_eq!(size_words(999), "999 B");
        assert_eq!(size_words(312_400), "313 KB");
        assert_eq!(size_words(1_450_000), "1.4 MB");
        assert_eq!(size_words(1_234_000_000), "1.2 GB");
    }
}
