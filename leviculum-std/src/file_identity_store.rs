//! File-backed identity store for std targets.
//!
//! Stores the identity as raw 64 bytes at `{storage_path}/transport_identity`,
//! compatible with Python Reticulum (`rnsd`, `rnstatus`, etc.).

use leviculum_core::constants::IDENTITY_KEY_SIZE;
use leviculum_core::identity::Identity;
use leviculum_core::identity_store::IdentityStore;
use std::path::{Path, PathBuf};

const IDENTITY_FILE: &str = "transport_identity";

/// File-backed identity store, Python-compatible.
///
/// The identity file is raw 64 bytes (32 X25519 + 32 Ed25519 private keys),
/// the same format Python Reticulum uses. No magic bytes, no checksum./// the filesystem provides existence checking.
pub struct FileIdentityStore {
    path: PathBuf,
}

impl FileIdentityStore {
    /// Create a store that reads/writes `{storage_dir}/transport_identity`.
    pub fn new(storage_dir: &Path) -> Self {
        Self {
            path: storage_dir.join(IDENTITY_FILE),
        }
    }
}

impl IdentityStore for FileIdentityStore {
    type Error = std::io::Error;

    fn load(&mut self) -> Result<Option<Identity>, Self::Error> {
        match std::fs::read(&self.path) {
            Ok(bytes) if bytes.len() == IDENTITY_KEY_SIZE => {
                Ok(Identity::from_private_key_bytes(&bytes).ok())
            }
            Ok(bytes) => {
                tracing::warn!(
                    "transport_identity has wrong size: {} (expected {})",
                    bytes.len(),
                    IDENTITY_KEY_SIZE
                );
                Ok(None)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn save(&mut self, identity: &Identity) -> Result<(), Self::Error> {
        let bytes = identity
            .private_key_bytes()
            .map_err(|e| std::io::Error::other(format!("{:?}", e)))?;

        // Atomic write: write to .tmp then rename (same pattern as storage.rs)
        let tmp_path = self.path.with_extension("tmp");
        std::fs::write(&tmp_path, bytes)?;
        std::fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }
}
