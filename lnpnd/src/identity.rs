//! The daemon's persistent service identity.
//!
//! A propagation node's destination hash is what every client configures as
//! its outbound node, so the identity is minted once and kept — and, like
//! `lnmsg`'s address (`lnmsg/src/identity.rs`), an unreadable file is a
//! fatal error rather than a silent re-mint: a re-minted identity would
//! strand every client pointing at the old hash.
//!
//! # Why a first start mints, and why it shouts
//!
//! The other choice was to refuse: no identity, no daemon, "put one at
//! `<configdir>/identity` and try again". It was rejected because nothing
//! in this package can produce that file — minting is deliberately not a
//! separate tool — so a refusal would leave the operator of a genuinely
//! new node with no supported way forward, and the reference mints on
//! first start too (`program_setup`,
//! `reference/LXMF/LXMF/Utilities/lxmd.py:337/389`).
//!
//! What was wrong on 2026-09-17 was not the minting, it was the silence:
//! installing the package started the daemon, which minted an identity
//! and announced that address on the public network for 33 seconds, and
//! the only record that a new address had come into existence was the
//! file's own mtime. So [`load_or_create`] logs the mint at the moment it
//! happens, naming the address it just created, and the daemon calls it
//! before it joins the shared instance — there is no path from a fresh
//! identity to the air that does not pass this line. The other half of
//! that day's fix is in the packaging (`lnpnd/Cargo.toml`,
//! `systemd-units.start = false`): installing no longer starts anything,
//! so the operator gets to place an existing identity first.

use std::path::{Path, PathBuf};

use leviculum_core::Destination;
use leviculum_lxmf::node::APP_NAME;
use leviculum_lxmf::propagation_client::PROPAGATION_ASPECT;
use leviculum_std::event_log::Scalar;
use leviculum_std::Identity;

/// The structured event logged when a new node address comes into being.
pub const CREATED_EVENT: &str = "PN_IDENTITY_CREATED";

/// Where the identity in hand came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// Read from the file that was already there; nothing was written.
    Loaded,
    /// Minted and written just now, because no file was there.
    Created,
}

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

/// The node address an identity carries: the `lxmf.propagation`
/// destination hash, derived without a running stack so it can be named
/// at mint time, before any destination is registered.
pub fn propagation_hash(identity: &Identity) -> [u8; 16] {
    let name_hash = Destination::compute_name_hash(APP_NAME, &[PROPAGATION_ASPECT]);
    *Destination::compute_destination_hash(&name_hash, identity.hash()).as_bytes()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Load the identity at `path`, minting and saving one on first run.
///
/// A mint is reported here rather than left to the caller: the guarantee
/// is that no address can reach the network without its creation standing
/// in a log, and a caller that forgets to check [`Provenance`] must not be
/// able to break it.
pub fn load_or_create(path: &Path) -> Result<(Identity, Provenance), IdentityError> {
    let io_err = |source| IdentityError::Io(path.to_path_buf(), source);
    if path.exists() {
        let bytes = std::fs::read(path).map_err(io_err)?;
        let identity = Identity::from_private_key_bytes(&bytes)
            .map_err(|_| IdentityError::Corrupt(path.to_path_buf()))?;
        Ok((identity, Provenance::Loaded))
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        let identity = leviculum_std::generate_identity();
        let private = identity
            .private_key_bytes()
            .map_err(|_| IdentityError::NotStorable)?;
        std::fs::write(path, private).map_err(io_err)?;
        // Private key material: owner-only, tighter than the reference's
        // umask-default (the config directory's 0750 is the other layer).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(io_err)?;
        }
        report_created(path, &identity);
        Ok((identity, Provenance::Created))
    }
}

/// Say that this daemon has just given itself a new address, and which.
///
/// Two audiences, so two lines: the structured [`CREATED_EVENT`] for the
/// log a fleet is grepped from, and plain text for the operator watching
/// the first start.
fn report_created(path: &Path, identity: &Identity) {
    let propagation = hex(&propagation_hash(identity));
    tracing::warn!(
        event = CREATED_EVENT,
        path = %Scalar(&path.display().to_string()),
        propagation = %propagation,
    );
    eprintln!(
        "lnpnd: no identity at {}, so a NEW node address was created:",
        path.display()
    );
    eprintln!("lnpnd:   {propagation}");
    eprintln!(
        "lnpnd: no client and no peer knows this address yet. If this host is \
         meant to continue an existing node, stop lnpnd now, copy that node's \
         identity file over {}, and start again — the address above will then \
         be gone for good.",
        path.display()
    );
}
