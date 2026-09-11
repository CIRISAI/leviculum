//! The daemon's persistent service identity.
//!
//! A propagation node's destination hash is what every client configures as
//! its outbound node, so the identity is minted once and kept — and, like
//! `lnmsg`'s address (`lnmsg/src/identity.rs`), an unreadable file is a
//! fatal error rather than a silent re-mint: a re-minted identity would
//! strand every client pointing at the old hash.

use std::path::{Path, PathBuf};

use leviculum_std::Identity;

#[derive(Debug)]
pub enum IdentityError {
    Corrupt(PathBuf),
    Io(PathBuf, std::io::Error),
    NotStorable,
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Corrupt(path) => write!(
                f,
                "{} is not a readable identity record.\n  \
                 This file is the node's address: it is not replaced automatically, \
                 because a new one would strand every client configured with the \
                 old destination hash.\n  \
                 Restore it from a backup, or move it aside to start over with a \
                 new node address.",
                path.display()
            ),
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::NotStorable => write!(f, "the generated identity has no private key to store"),
        }
    }
}

impl std::error::Error for IdentityError {}

/// Load the identity at `path`, minting and saving one on first run.
pub fn load_or_create(path: &Path) -> Result<Identity, IdentityError> {
    let io_err = |source| IdentityError::Io(path.to_path_buf(), source);
    if path.exists() {
        let bytes = std::fs::read(path).map_err(io_err)?;
        Identity::from_private_key_bytes(&bytes)
            .map_err(|_| IdentityError::Corrupt(path.to_path_buf()))
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        let identity = leviculum_std::generate_identity();
        let private = identity
            .private_key_bytes()
            .map_err(|_| IdentityError::NotStorable)?;
        std::fs::write(path, private).map_err(io_err)?;
        Ok(identity)
    }
}
