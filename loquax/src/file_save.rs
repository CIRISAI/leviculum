//! A received file on disk (1221): the terminal, the window and the phone
//! save a file a bubble carries before the system opens it, under the
//! home (the phone's app files), one folder per message and file so the
//! sender's name stays as it was written, made safe
//! ([`crate::ui::files::safe_name`]).

use std::path::{Path, PathBuf};

use crate::ui::files::{file_at, safe_name};
use leviculum_lxmf::Field;

/// The folder under the home received files are saved in.
pub const DIR: &str = "files";

/// The `index`th file `fields` carry, saved under `home`'s [`DIR`] as
/// `<peer>-<key>-<index>/<safe name>`, written once; its path. An error
/// says what failed in words.
pub fn save(
    home: &Path,
    peer: [u8; 16],
    key: u64,
    index: usize,
    fields: &[Field],
) -> Result<PathBuf, String> {
    let (name, data) =
        file_at(fields, index).ok_or_else(|| "that message has no such file".to_string())?;
    let dir = home
        .join(DIR)
        .join(format!("{}-{key}-{index}", crate::address::to_hex(&peer)));
    let path = dir.join(safe_name(&name));
    let written = std::fs::metadata(&path).is_ok_and(|m| m.len() == data.len() as u64);
    if !written {
        std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&path, data))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(path)
}

/// The folders under the home a desktop writes a message's saved copies
/// in, besides [`DIR`]: the photo the viewer opens
/// (`photos/<peer>-<key>.<ext>`) and the voice message's WAV for the
/// player (`voice/<peer>-<key>.wav`).
/// `voice` is `crate::voice::DIR`, which a build without the `voice`
/// feature lacks; a test there holds the two together.
const COPIES: [&str; 2] = ["photos", "voice"];

/// Remove what this device saved of the message `key` of `peer`'s
/// conversation, or of every message of it for no key (1662): its files
/// under [`DIR`], its photo and voice copies. A deleted message leaves
/// nothing of itself in the app's storage; a copy the reader exported
/// elsewhere is not under the home and stays. How many entries went; an
/// error says the first that would not go, after trying the rest.
pub fn forget(home: &Path, peer: [u8; 16], key: Option<u64>) -> Result<usize, String> {
    let hex = crate::address::to_hex(&peer);
    // `<peer>-<key>-<n>/` under files/, `<peer>-<key>.<ext>` beside it:
    // the delimiter after the key keeps key 1 from taking key 12.
    let (folder, copy) = match key {
        Some(key) => (format!("{hex}-{key}-"), format!("{hex}-{key}.")),
        None => (format!("{hex}-"), format!("{hex}-")),
    };
    let mut gone = 0;
    let mut failed = None;
    let places = core::iter::once((DIR, folder.as_str()))
        .chain(COPIES.iter().map(|dir| (*dir, copy.as_str())));
    for (dir, prefix) in places {
        let Ok(entries) = std::fs::read_dir(home.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_name().to_string_lossy().starts_with(prefix) {
                continue;
            }
            let path = entry.path();
            // A link is removed, never what it points at.
            let removed = match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&path),
                Ok(_) => std::fs::remove_file(&path),
                Err(error) => Err(error),
            };
            match removed {
                Ok(()) => gone += 1,
                Err(error) => {
                    failed.get_or_insert_with(|| format!("{}: {error}", path.display()));
                }
            }
        }
    }
    match failed {
        Some(why) => Err(why),
        None => Ok(gone),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_lxmf::attachments::{FileAttachment, MessageAttachments};

    /// A hostile name lands inside the message's folder, under the home,
    /// with its bytes; asking again writes nothing new.
    #[test]
    fn a_file_is_saved_inside_the_home_whatever_its_name() {
        let home = std::env::temp_dir().join(format!("loquax-file-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        // As a hostile sender writes it, not as ours goes.
        let fields = MessageAttachments {
            files: vec![FileAttachment {
                name: "../../x/../evil.sh".into(),
                data: b"#!/bin/sh\n".to_vec(),
            }],
            ..MessageAttachments::default()
        }
        .into_fields();
        let path = save(&home, [0xab; 16], 7, 0, &fields).expect("saved");
        assert!(path.starts_with(home.join(DIR)), "{}", path.display());
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("_.._x_.._evil.sh")
        );
        assert_eq!(
            path.parent(),
            Some(
                home.join(DIR)
                    .join(format!("{}-7-0", "ab".repeat(16)))
                    .as_path()
            )
        );
        assert_eq!(std::fs::read(&path).expect("read"), b"#!/bin/sh\n");
        assert_eq!(save(&home, [0xab; 16], 7, 0, &fields), Ok(path));
        assert!(save(&home, [0xab; 16], 7, 1, &fields).is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// 1662: a saved file goes with its message. Send, save, delete: the
    /// path is gone, and so are the message's photo and voice copies; a
    /// neighbour's key, another peer's file and a copy the reader exported
    /// out of the home stay. Deleting the conversation takes the rest of
    /// the peer's.
    #[test]
    fn a_saved_file_goes_with_its_message_and_an_exported_copy_stays() {
        let home = std::env::temp_dir().join(format!("loquax-file-forget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let fields = crate::ui::files::file_fields("Protokoll.pdf", b"%PDF-1.7".to_vec());
        let (anna, bob) = ([0xa1; 16], [0xb2; 16]);
        let saved = save(&home, anna, 1, 0, &fields).expect("saved");
        let neighbour = save(&home, anna, 12, 0, &fields).expect("saved");
        let bobs = save(&home, bob, 1, 0, &fields).expect("saved");
        let photo = home
            .join("photos")
            .join(format!("{}-1.jpg", "a1".repeat(16)));
        let voice = home
            .join("voice")
            .join(format!("{}-1.wav", "a1".repeat(16)));
        for copy in [&photo, &voice] {
            std::fs::create_dir_all(copy.parent().expect("a folder")).expect("folder");
            std::fs::write(copy, b"x").expect("copy");
        }
        // The reader exported the file out of the app's storage.
        let exported = home.with_extension("export").join("Protokoll.pdf");
        std::fs::create_dir_all(exported.parent().expect("a folder")).expect("folder");
        std::fs::copy(&saved, &exported).expect("exported");
        assert_eq!(forget(&home, anna, Some(1)), Ok(3));
        assert!(!saved.exists() && !photo.exists() && !voice.exists());
        assert!(!saved.parent().expect("its folder").exists());
        assert!(neighbour.exists(), "key 12 is not key 1");
        assert!(bobs.exists(), "another peer's");
        assert_eq!(std::fs::read(&exported).expect("still there"), b"%PDF-1.7");
        assert_eq!(forget(&home, anna, Some(1)), Ok(0), "nothing left of it");
        assert_eq!(forget(&home, anna, None), Ok(1));
        assert!(!neighbour.exists() && bobs.exists());
        assert_eq!(forget(&home.join("nowhere"), anna, None), Ok(0));
        #[cfg(feature = "voice")]
        assert_eq!(COPIES[1], crate::voice::DIR);
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(home.with_extension("export"));
    }
}
