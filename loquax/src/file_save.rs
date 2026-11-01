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
}
