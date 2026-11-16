//! `loquax`'s own persistent Reticulum identity, and where its state lives.
//!
//! # Why this is not `lnomad`'s `load_or_create`
//!
//! `lnomad` mints a fresh identity whenever the stored one fails to decode
//! (`lnomad/src/identity.rs:39-53`), and for a browser that is right: its
//! identity is disposable. A messenger's identity **is** the user's address.
//! Replacing it silently would change what every correspondent has written
//! down, with no warning and no way back — the architecture record calls that
//! default out by name and says it must be inverted
//! (`docs/src/concepts/loquax-architecture.md:110-115`). So a missing file is a
//! first run and mints; an unreadable or corrupt file is an error that stops
//! the program and says which file to look at.

use std::path::{Path, PathBuf};

use leviculum_core::identity_store::{decode_identity, encode_identity};
use leviculum_std::Identity;

/// Environment override for the state directory, mirroring the `LXMF_STORAGE`
/// precedent in `leviculum-lxmf-node`. Set by tests to get an isolated address
/// per run; a user has no reason to touch it.
pub const HOME_ENV: &str = "LNMSG_HOME";

/// Why the identity could not be established.
#[derive(Debug)]
pub enum IdentityError {
    /// Neither `XDG_CONFIG_HOME` nor `HOME` nor [`HOME_ENV`] is set, so there
    /// is nowhere to keep an address.
    NoHome,
    /// The file exists and is not an identity record. Deliberately fatal.
    Corrupt(PathBuf),
    /// A filesystem error, with the path it happened on.
    Io(PathBuf, std::io::Error),
    /// A freshly generated identity had no private key to store.
    NotStorable,
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHome => write!(
                f,
                "no home directory to keep an address in; set {HOME_ENV} or HOME"
            ),
            Self::Corrupt(path) => write!(
                f,
                "{} is not a readable identity record.\n  \
                 This file is your LXMF address: it is not replaced automatically, \
                 because a new one would silently change the address your \
                 correspondents have.\n  \
                 Restore it from a backup, or move it aside to start over with a \
                 new address.",
                path.display()
            ),
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::NotStorable => write!(f, "the generated identity has no private key to store"),
        }
    }
}

impl std::error::Error for IdentityError {}

/// `${LNMSG_HOME:-${XDG_CONFIG_HOME:-~/.config}/loquax}`: identity and node
/// storage, in one directory, the way `lnomad` keeps its own.
///
/// Made here when it is missing, owner only (0700) like any directory that
/// holds a private key, so no file opened in it has to make it first. A
/// first `loquax send` took the delivery lock in a home nobody had made
/// yet and failed on it before it sent (1141). Runs after
/// [`migrate_legacy_home`], which leaves an existing new directory alone.
pub fn home_dir() -> Result<PathBuf, IdentityError> {
    let home = home_path()?;
    leviculum_std::create_secret_dir_all(&home).map_err(|e| IdentityError::Io(home.clone(), e))?;
    Ok(home)
}

/// Where [`home_dir`] is, without making it: for the commands that only
/// look (`import --dry-run` writes nothing, not even the directory) and
/// make what they write themselves.
pub fn home_path() -> Result<PathBuf, IdentityError> {
    if let Some(explicit) = std::env::var_os(HOME_ENV)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
    {
        return Ok(explicit);
    }
    Ok(config_base().ok_or(IdentityError::NoHome)?.join(DIR_NAME))
}

/// The state directory's name under the config base.
const DIR_NAME: &str = "loquax";

/// Its name before the program was renamed (#232), read once by
/// [`migrate_legacy_home`] and never written.
const LEGACY_DIR_NAME: &str = "lnmsg";

/// `${XDG_CONFIG_HOME:-~/.config}`, or `None` when neither is set.
fn config_base() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
}

/// What [`migrate_in`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Migration {
    /// Nothing to move: no old directory, or the new one already exists.
    NotNeeded,
    /// The old directory now lives at the new path.
    Moved { from: PathBuf, to: PathBuf },
}

/// Move `~/.config/lnmsg` to `~/.config/loquax` on the first start after the
/// rename, so the address the user has published survives it: without this
/// the first run under the new name would mint a fresh identity, which
/// [`load_or_create`] exists to never do silently. An explicit [`HOME_ENV`]
/// names the directory itself and bypasses the move.
pub fn migrate_legacy_home() -> Result<Migration, IdentityError> {
    if std::env::var_os(HOME_ENV).is_some_and(|p| !p.is_empty()) {
        return Ok(Migration::NotNeeded);
    }
    match config_base() {
        Some(base) => migrate_in(&base),
        None => Ok(Migration::NotNeeded),
    }
}

/// [`migrate_legacy_home`] under a given config base. Never touches an
/// existing new directory; a rename across filesystems falls back to a copy,
/// which leaves the old directory where it was.
pub fn migrate_in(base: &Path) -> Result<Migration, IdentityError> {
    let from = base.join(LEGACY_DIR_NAME);
    let to = base.join(DIR_NAME);
    if std::fs::symlink_metadata(&to).is_ok() || !from.is_dir() {
        return Ok(Migration::NotNeeded);
    }
    if std::fs::rename(&from, &to).is_err() {
        copy_dir(&from, &to)?;
    }
    Ok(Migration::Moved { from, to })
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), IdentityError> {
    std::fs::create_dir(to).map_err(|e| IdentityError::Io(to.to_owned(), e))?;
    let entries = std::fs::read_dir(from).map_err(|e| IdentityError::Io(from.to_owned(), e))?;
    for entry in entries {
        let entry = entry.map_err(|e| IdentityError::Io(from.to_owned(), e))?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        if source.is_dir() {
            copy_dir(&source, &target)?;
        } else {
            std::fs::copy(&source, &target).map_err(|e| IdentityError::Io(source.clone(), e))?;
        }
    }
    Ok(())
}

/// Load the identity at `path`, minting and persisting one only if the file is
/// not there at all. See the module header for why corruption is fatal.
///
/// The file is the private key, so it is written 0600 whatever the umask,
/// and one an older loquax left wider is narrowed here (1119).
pub fn load_or_create(path: &Path) -> Result<Identity, IdentityError> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let identity =
                decode_identity(&bytes).ok_or_else(|| IdentityError::Corrupt(path.to_owned()))?;
            leviculum_std::narrow_secret_mode(path)
                .map_err(|e| IdentityError::Io(path.to_owned(), e))?;
            Ok(identity)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => mint(path),
        Err(error) => Err(IdentityError::Io(path.to_owned(), error)),
    }
}

fn mint(path: &Path) -> Result<Identity, IdentityError> {
    let identity = leviculum_std::generate_identity();
    store(path, &identity)?;
    Ok(identity)
}

/// Keep `identity` at `path` as [`load_or_create`] reads it, readable by the
/// user alone: a minted one, or one restored from a backup (1152).
pub fn store(path: &Path, identity: &Identity) -> Result<(), IdentityError> {
    let encoded = encode_identity(identity).ok_or(IdentityError::NotStorable)?;
    if let Some(parent) = path.parent() {
        leviculum_std::create_secret_dir_all(parent)
            .map_err(|e| IdentityError::Io(parent.to_owned(), e))?;
    }
    leviculum_std::atomic_write_secret(path, &encoded)
        .map_err(|e| IdentityError::Io(path.to_owned(), e))
}

/// The file under the loquax home that says when the identity last moved
/// in: an import adopted one, or brought its ratchets over. Its content is
/// the Unix time in seconds.
pub const MOVED_FILE: &str = "identity.moved";

/// Note that the identity moved in now. Loquax Mailbox announces more often
/// for a day after it (`crate::home::daemon`), so peers that remember the
/// old program's ratchet hear the new one sooner.
pub fn mark_moved(home: &Path) -> Result<(), String> {
    let path = home.join(MOVED_FILE);
    let now = leviculum_std::local_time::unix_now_secs();
    std::fs::write(&path, format!("{now}\n")).map_err(|e| format!("{}: {e}", path.display()))
}

/// When the identity last moved in, in Unix seconds, if it ever did.
pub fn moved_at(home: &Path) -> Option<i64> {
    std::fs::read_to_string(home.join(MOVED_FILE))
        .ok()?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_run_mints_and_then_keeps_the_same_address() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested/identity");

        let minted = load_or_create(&path).expect("first run mints");
        assert!(path.exists(), "the minted identity must be persisted");
        let reloaded = load_or_create(&path).expect("second run loads");
        assert_eq!(
            reloaded.hash(),
            minted.hash(),
            "a second run must keep the address the first one published"
        );
    }

    /// The inversion of `lnomad`'s behaviour, and the whole point of this
    /// module: a corrupt file must NOT quietly become a new address.
    #[test]
    fn a_corrupt_record_is_fatal_and_is_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("identity");
        std::fs::write(&path, b"not an identity record").expect("write");

        // `Identity` has no `Debug`, so the success arm cannot be unwrapped
        // into a panic message; match instead.
        let error = match load_or_create(&path) {
            Ok(_) => panic!("a corrupt record must not be replaced"),
            Err(error) => error,
        };
        assert!(matches!(error, IdentityError::Corrupt(_)), "{error}");
        assert_eq!(
            std::fs::read(&path).expect("read back"),
            b"not an identity record",
            "the file the user has to rescue must still be there"
        );
    }

    #[test]
    fn a_truncated_record_counts_as_corrupt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("identity");
        let full = encode_identity(&leviculum_std::generate_identity()).expect("encode");
        std::fs::write(&path, &full[..full.len() / 2]).expect("write");

        assert!(
            matches!(load_or_create(&path), Err(IdentityError::Corrupt(_))),
            "a half-written record is not an address to publish"
        );
    }

    #[test]
    fn the_home_env_var_wins_over_xdg() {
        // Serialised with the other env-reading test by running in one test
        // function: cargo runs test fns in parallel threads of one process,
        // and the environment is process-wide.
        let previous_home = std::env::var_os(HOME_ENV);
        std::env::set_var(HOME_ENV, "/tmp/loquax-home-test");
        assert_eq!(
            home_path().expect("explicit home"),
            PathBuf::from("/tmp/loquax-home-test")
        );
        std::env::set_var(HOME_ENV, "");
        let xdg = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/loquax-xdg-test");
        assert_eq!(
            home_path().expect("xdg home"),
            PathBuf::from("/tmp/loquax-xdg-test/loquax")
        );
        match xdg {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match previous_home {
            Some(value) => std::env::set_var(HOME_ENV, value),
            None => std::env::remove_var(HOME_ENV),
        }
    }

    /// The four starting states of the rename's migration: only the old
    /// directory, only the new one, both, and neither.
    #[test]
    fn the_old_directory_moves_once_and_a_new_one_is_never_touched() {
        let write = |path: &Path, body: &[u8]| {
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, body).expect("write");
        };

        let old_only = tempfile::tempdir().expect("tempdir");
        write(&old_only.path().join("lnmsg/identity"), b"old address");
        assert_eq!(
            migrate_in(old_only.path()).expect("old only"),
            Migration::Moved {
                from: old_only.path().join("lnmsg"),
                to: old_only.path().join("loquax"),
            }
        );
        assert_eq!(
            std::fs::read(old_only.path().join("loquax/identity")).expect("moved"),
            b"old address"
        );
        assert!(!old_only.path().join("lnmsg").exists());
        assert_eq!(
            migrate_in(old_only.path()).expect("second start"),
            Migration::NotNeeded
        );

        let new_only = tempfile::tempdir().expect("tempdir");
        write(&new_only.path().join("loquax/identity"), b"new address");
        assert_eq!(
            migrate_in(new_only.path()).expect("new only"),
            Migration::NotNeeded
        );
        assert_eq!(
            std::fs::read(new_only.path().join("loquax/identity")).expect("kept"),
            b"new address"
        );

        let both = tempfile::tempdir().expect("tempdir");
        write(&both.path().join("lnmsg/identity"), b"old address");
        write(&both.path().join("loquax/identity"), b"new address");
        assert_eq!(migrate_in(both.path()).expect("both"), Migration::NotNeeded);
        assert_eq!(
            std::fs::read(both.path().join("loquax/identity")).expect("kept"),
            b"new address"
        );
        assert_eq!(
            std::fs::read(both.path().join("lnmsg/identity")).expect("left"),
            b"old address"
        );

        let none = tempfile::tempdir().expect("tempdir");
        assert_eq!(migrate_in(none.path()).expect("none"), Migration::NotNeeded);
        assert!(!none.path().join("loquax").exists());
    }

    #[test]
    fn a_copy_carries_nested_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("from/storage")).expect("mkdir");
        std::fs::write(dir.path().join("from/identity"), b"id").expect("write");
        std::fs::write(dir.path().join("from/storage/x"), b"x").expect("write");
        copy_dir(&dir.path().join("from"), &dir.path().join("to")).expect("copy");
        assert_eq!(
            std::fs::read(dir.path().join("to/identity")).expect("id"),
            b"id"
        );
        assert_eq!(
            std::fs::read(dir.path().join("to/storage/x")).expect("x"),
            b"x"
        );
    }
}
