//! The helper's own LXMF identity, and the file it survives a restart in.
//!
//! # Why this file exists
//!
//! The helper's LXMF address is `H(name_hash || identity_hash)` of the
//! delivery destination it registers, so it is the identity that decides the
//! address a correspondent writes to. Until this module the identity was drawn
//! fresh in [`crate::processor::LxmfHelperProcessor::new`] on every start,
//! mirroring Python's helper (`identity = RNS.Identity()`,
//! `periculum/assets/scripts/lxmf_node.py:310`) — which is right for a
//! throwaway test peer in a container that is destroyed with the scenario, and
//! wrong the moment the same binary is a standing endpoint. The field base ran
//! it that way and came back from a restart on 2026-09-26 as a different
//! address (`4c64d723…` at 12:0x, `8f35c8d5…` at 23:55), having already given
//! the old one to its operator. `LXMF_STORAGE` held the client stack's
//! `transport_identity` across that restart and nothing else.
//!
//! Python's helper persists no identity at all, so there is no file name of
//! its to match; the name here follows the two leviculum daemons that do keep
//! one (`lnpnd/src/identity.rs`, `lnmsg/src/identity.rs`) and the reference
//! `lxmd` (`reference/LXMF/LXMF/Utilities/lxmd.py:337`, `<configdir>/identity`).
//! It is spelled [`IDENTITY_FILE`] rather than plain `identity` because this
//! directory is a *storage* directory, not a config directory: it already
//! holds `transport_identity`, the network identity of the shared-instance
//! client underneath the router, and two files called `identity` and
//! `transport_identity` side by side would not say which address is the one
//! people write to.
//!
//! # Why an unreadable file is fatal
//!
//! Same inversion `lnmsg` and `lnpnd` argue for, for the same reason: a
//! re-mint on a corrupt file would silently publish a new address and strand
//! everyone holding the old one. A missing file is a first run and mints; an
//! unreadable one stops the helper and names the file to rescue.

use std::path::{Path, PathBuf};

use leviculum_core::identity::Identity;

/// The identity file inside `LXMF_STORAGE`. Raw 64 bytes of private key
/// material, the RNS identity-file format — the same bytes `RNS.Identity
/// .from_file`, `lxmd --identity` and `lnpnd --identity` read, and the format
/// this helper's own `control_identity` verb already writes.
pub const IDENTITY_FILE: &str = "lxmf_identity";

/// Where the identity in hand came from, so the caller can say so once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// Read from the file that was already there; nothing was written.
    Loaded,
    /// Minted and written just now, because no file was there.
    Created,
}

/// Why the identity could not be established.
#[derive(Debug)]
pub enum IdentityError {
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
            Self::Corrupt(path) => write!(
                f,
                "{} is not a readable identity record.\n  \
                 This file is this node's LXMF address: it is not replaced \
                 automatically, because a new one would silently change the \
                 address every correspondent has written down.\n  \
                 Restore it from a backup, or move it aside to start over with \
                 a new address.",
                path.display()
            ),
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::NotStorable => write!(f, "the generated identity has no private key to store"),
        }
    }
}

impl std::error::Error for IdentityError {}

/// Load the identity at `path`, minting and saving one only if no file is
/// there at all.
///
/// The mint is reported through [`Provenance`] rather than logged here: this
/// crate's diagnostics all go through the [`crate::processor::Emitter`], which
/// is wired in `main.rs` and does not exist yet when the identity is needed.
pub fn load_or_create(path: &Path) -> Result<(Identity, Provenance), IdentityError> {
    let io_err = |source| IdentityError::Io(path.to_path_buf(), source);
    match std::fs::read(path) {
        Ok(bytes) => {
            let identity = Identity::from_private_key_bytes(&bytes)
                .map_err(|_| IdentityError::Corrupt(path.to_path_buf()))?;
            Ok((identity, Provenance::Loaded))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => mint(path),
        Err(error) => Err(io_err(error)),
    }
}

fn mint(path: &Path) -> Result<(Identity, Provenance), IdentityError> {
    let io_err = |source| IdentityError::Io(path.to_path_buf(), source);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| IdentityError::Io(parent.to_path_buf(), e))?;
    }
    let identity = Identity::generate(&mut rand_core::OsRng);
    let private = identity
        .private_key_bytes()
        .map_err(|_| IdentityError::NotStorable)?;
    std::fs::write(path, private).map_err(io_err)?;
    // Private key material: owner-only, as `lnpnd` writes its own
    // (`lnpnd/src/identity.rs`).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(io_err)?;
    }
    Ok((identity, Provenance::Created))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_start_mints_and_every_later_one_loads_the_same_address() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(IDENTITY_FILE);

        let (minted, provenance) = load_or_create(&path).expect("a first start mints");
        assert_eq!(provenance, Provenance::Created);
        assert!(path.exists(), "the minted identity must be persisted");

        let (reloaded, provenance) = load_or_create(&path).expect("a second start loads");
        assert_eq!(provenance, Provenance::Loaded);
        assert_eq!(
            reloaded.hash(),
            minted.hash(),
            "a restart must keep the address the first start published"
        );
    }

    /// The other half of the contract: nothing is shared between storage
    /// directories, so a scenario that wants a fresh peer still gets one.
    #[test]
    fn a_fresh_storage_directory_is_a_fresh_address() {
        let one = tempfile::tempdir().expect("tempdir");
        let two = tempfile::tempdir().expect("tempdir");
        let (first, _) = load_or_create(&one.path().join(IDENTITY_FILE)).expect("mint");
        let (second, _) = load_or_create(&two.path().join(IDENTITY_FILE)).expect("mint");
        assert_ne!(
            first.hash(),
            second.hash(),
            "two storage directories must not share an address"
        );
    }

    /// The file is written where a missing parent directory would otherwise
    /// make a first start fail — `main.rs` creates the storage directory, but
    /// `pn-messagestore` shows the same directory grows subdirectories.
    #[test]
    fn a_missing_parent_directory_is_created() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested/deeper").join(IDENTITY_FILE);
        assert!(
            load_or_create(&path).is_ok(),
            "a first start must mint here"
        );
        assert!(path.exists());
    }

    /// The inversion of a silent re-mint: a corrupt file stops the helper and
    /// is left exactly as it was, so the operator can still rescue it.
    #[test]
    fn a_corrupt_record_is_fatal_and_is_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(IDENTITY_FILE);
        std::fs::write(&path, b"not an identity record").expect("write");

        // `Identity` has no `Debug`, so the success arm cannot be unwrapped
        // into a panic message; match instead.
        let error = match load_or_create(&path) {
            Ok(_) => panic!("a corrupt record must not be replaced"),
            Err(error) => error,
        };
        assert!(matches!(error, IdentityError::Corrupt(_)), "{error}");
        assert!(
            error.to_string().contains(&path.display().to_string()),
            "the message must name the file to rescue: {error}"
        );
        assert_eq!(
            std::fs::read(&path).expect("read back"),
            b"not an identity record",
            "the file the operator has to rescue must still be there"
        );
    }

    #[test]
    fn a_truncated_record_counts_as_corrupt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(IDENTITY_FILE);
        let full = Identity::generate(&mut rand_core::OsRng)
            .private_key_bytes()
            .expect("a generated identity has a private key");
        std::fs::write(&path, &full[..full.len() / 2]).expect("write");

        assert!(
            matches!(load_or_create(&path), Err(IdentityError::Corrupt(_))),
            "a half-written record is not an address to publish"
        );
    }
}
