//! Network endpoint addressing
//!
//! A Destination represents an addressable endpoint in the Reticulum network.
//! Destinations have:
//! - A unique hash derived from application name, aspects, and identity
//! - A type (SINGLE, GROUP, PLAIN, LINK) determining encryption behavior
//! - A direction (IN, OUT) determining receive/send capability
//!
//! # Ratchet Support
//!
//! Destinations can enable ratchets for forward secrecy. When enabled:
//! - The destination rotates X25519 key pairs periodically
//! - Announces include the current ratchet public key
//! - Incoming packets are decrypted by trying retained ratchets
//!
//! See [`Destination::enable_ratchets`] for more details.

use crate::announce::{build_announce_payload, AnnounceError};
use crate::constants::{
    IDENTITY_HASHBYTES, NAME_HASHBYTES, RATCHET_SIZE, TOKEN_KEY_SIZE, TRUNCATED_HASHBYTES,
};
use crate::crypto::{decrypt_token, encrypt_token, sha256, truncated_hash};
use crate::identity::{Identity, IdentityError};
use crate::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use crate::ratchet::{Ratchet, DEFAULT_INTERVAL_MS, DEFAULT_RETAINED_RATCHETS};

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use rand_core::CryptoRngCore;

/// Error type for destination operations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestinationError {
    /// PLAIN destination type cannot have an identity
    PlainCannotHaveIdentity,
    /// Outbound SINGLE/GROUP destinations require an identity
    OutboundRequiresIdentity,
    /// Operation requires an identity
    NoIdentity,
    /// Ratchets are required but no ratchet was used
    RatchetRequired,
    /// Decryption failed
    DecryptionFailed,
    /// Encryption failed
    EncryptionFailed,
    /// Cannot enable ratchets on OUT destination
    CannotEnableRatchetsOnOut,
    /// Invalid serialized ratchet data
    InvalidRatchetData,
    /// Ed25519 signature verification failed
    InvalidSignature,
    /// Operation is only valid on a GROUP destination
    NotGroupType,
    /// GROUP destination holds no symmetric key (call create/load first)
    NoGroupKey,
    /// Supplied GROUP key has an unsupported length (must be 64 bytes)
    InvalidGroupKeyLength,
}

impl core::fmt::Display for DestinationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DestinationError::PlainCannotHaveIdentity => {
                write!(f, "PLAIN destination type cannot hold an identity")
            }
            DestinationError::OutboundRequiresIdentity => {
                write!(f, "Outbound SINGLE/GROUP destinations require an identity")
            }
            DestinationError::NoIdentity => {
                write!(f, "Operation requires an identity")
            }
            DestinationError::RatchetRequired => {
                write!(f, "Ratchets are required but no ratchet was used")
            }
            DestinationError::DecryptionFailed => {
                write!(f, "Decryption failed")
            }
            DestinationError::EncryptionFailed => {
                write!(f, "Encryption failed")
            }
            DestinationError::CannotEnableRatchetsOnOut => {
                write!(f, "Cannot enable ratchets on OUT destination")
            }
            DestinationError::InvalidRatchetData => {
                write!(f, "Invalid serialized ratchet data")
            }
            DestinationError::InvalidSignature => {
                write!(f, "Ed25519 signature verification failed")
            }
            DestinationError::NotGroupType => {
                write!(f, "Operation is only valid on a GROUP destination")
            }
            DestinationError::NoGroupKey => {
                write!(f, "GROUP destination holds no symmetric key")
            }
            DestinationError::InvalidGroupKeyLength => {
                write!(f, "GROUP key must be 64 bytes")
            }
        }
    }
}

impl From<IdentityError> for DestinationError {
    fn from(e: IdentityError) -> Self {
        match e {
            IdentityError::NoPrivateKey => DestinationError::NoIdentity,
            IdentityError::EncryptionFailed => DestinationError::EncryptionFailed,
            IdentityError::DecryptionFailed => DestinationError::DecryptionFailed,
            _ => DestinationError::DecryptionFailed,
        }
    }
}

/// Destination type determining encryption behavior
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DestinationType {
    /// Point-to-point with ephemeral encryption per packet
    Single = 0x00,
    /// Broadcast with pre-shared key
    Group = 0x01,
    /// Unencrypted
    Plain = 0x02,
    /// Link-based encryption (internal use)
    Link = 0x03,
}

impl TryFrom<u8> for DestinationType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(DestinationType::Single),
            0x01 => Ok(DestinationType::Group),
            0x02 => Ok(DestinationType::Plain),
            0x03 => Ok(DestinationType::Link),
            _ => Err(()),
        }
    }
}

/// Destination direction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Direction {
    /// Incoming - can receive announces and accept links
    In = 0x11,
    /// Outgoing - source address for packets
    Out = 0x12,
}

/// Proof generation strategy for incoming packets
///
/// Controls how the destination responds to requests for delivery proofs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum ProofStrategy {
    /// Never generate proofs (default)
    #[default]
    None = 0x21,
    /// Ask application via ProofRequested event to decide
    App = 0x22,
    /// Automatically prove every packet
    All = 0x23,
}

/// A 16-byte destination hash (truncated hash of name_hash + identity_hash)
///
/// In the Reticulum protocol, destination hashes identify network endpoints.
/// They are computed as `truncated_hash(name_hash + identity_hash)` and
/// appear in packet headers and routing tables.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DestinationHash([u8; TRUNCATED_HASHBYTES]);

impl DestinationHash {
    /// Create a DestinationHash from raw bytes
    pub const fn new(bytes: [u8; TRUNCATED_HASHBYTES]) -> Self {
        Self(bytes)
    }

    /// Get the underlying bytes
    pub const fn as_bytes(&self) -> &[u8; TRUNCATED_HASHBYTES] {
        &self.0
    }

    /// Convert to the raw byte array
    pub const fn into_bytes(self) -> [u8; TRUNCATED_HASHBYTES] {
        self.0
    }
}

impl From<[u8; TRUNCATED_HASHBYTES]> for DestinationHash {
    fn from(bytes: [u8; TRUNCATED_HASHBYTES]) -> Self {
        Self(bytes)
    }
}

impl From<DestinationHash> for [u8; TRUNCATED_HASHBYTES] {
    fn from(hash: DestinationHash) -> Self {
        hash.0
    }
}

impl AsRef<[u8]> for DestinationHash {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl AsRef<[u8; TRUNCATED_HASHBYTES]> for DestinationHash {
    fn as_ref(&self) -> &[u8; TRUNCATED_HASHBYTES] {
        &self.0
    }
}

impl core::borrow::Borrow<[u8; TRUNCATED_HASHBYTES]> for DestinationHash {
    fn borrow(&self) -> &[u8; TRUNCATED_HASHBYTES] {
        &self.0
    }
}

impl PartialEq<[u8; TRUNCATED_HASHBYTES]> for DestinationHash {
    fn eq(&self, other: &[u8; TRUNCATED_HASHBYTES]) -> bool {
        self.0 == *other
    }
}

impl PartialEq<DestinationHash> for [u8; TRUNCATED_HASHBYTES] {
    fn eq(&self, other: &DestinationHash) -> bool {
        *self == other.0
    }
}

impl core::fmt::Debug for DestinationHash {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "DestinationHash(")?;
        for byte in &self.0 {
            write!(f, "{:02x}", byte)?;
        }
        write!(f, ")")
    }
}

impl core::fmt::Display for DestinationHash {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for byte in &self.0 {
            write!(f, "{:02x}", byte)?;
        }
        Ok(())
    }
}

/// A network destination (endpoint)
pub struct Destination {
    /// The destination hash (address)
    hash: DestinationHash,
    /// Name hash (truncated hash of app_name.aspects)
    name_hash: [u8; NAME_HASHBYTES],
    /// Associated identity (optional for PLAIN destinations)
    identity: Option<Identity>,
    /// Destination type
    dest_type: DestinationType,
    /// Direction
    direction: Direction,
    /// Whether to accept incoming links
    accepts_links: bool,
    /// Proof generation strategy for incoming packets
    proof_strategy: ProofStrategy,

    // Ratchet State (for IN destinations)
    /// Retained ratchets for decryption (newest first)
    ratchets: Vec<Ratchet>,
    /// Ratchet rotation interval in milliseconds
    ratchet_interval_ms: u64,
    /// Maximum number of ratchets to retain
    retained_ratchet_count: usize,
    /// Timestamp of last ratchet rotation
    last_ratchet_time_ms: u64,
    /// If true, reject packets not encrypted with a ratchet
    enforce_ratchets: bool,
    /// If true, ratchets are enabled for this destination
    ratchets_enabled: bool,
    /// If true, ratchet keys have changed since last persist
    ratchets_dirty: bool,

    /// Symmetric key for GROUP destinations (64 bytes: 32 signing + 32 AES).
    ///
    /// Mirrors Python `Destination.prv_bytes` for GROUP type. `None` until
    /// `create_group_key` or `load_group_key` is called. Only meaningful for
    /// [`DestinationType::Group`].
    group_key: Option<[u8; TOKEN_KEY_SIZE]>,
}

impl Destination {
    /// Create a new destination
    ///
    /// # Arguments
    /// * `identity` - The identity for this destination (required except for PLAIN)
    /// * `direction` - IN for receiving, OUT for sending
    /// * `dest_type` - The encryption type
    /// * `app_name` - Application name
    /// * `aspects` - Additional name components
    ///
    /// # Errors
    /// * `PlainCannotHaveIdentity` - PLAIN destinations cannot have an identity
    /// * `OutboundRequiresIdentity` - SINGLE/GROUP OUT destinations require an identity
    pub fn new(
        identity: Option<Identity>,
        direction: Direction,
        dest_type: DestinationType,
        app_name: &str,
        aspects: &[&str],
    ) -> Result<Self, DestinationError> {
        // PLAIN destinations cannot have an identity (per Python Reticulum spec)
        if dest_type == DestinationType::Plain && identity.is_some() {
            return Err(DestinationError::PlainCannotHaveIdentity);
        }

        // SINGLE/GROUP OUT destinations require an identity for encryption
        if direction == Direction::Out && dest_type != DestinationType::Plain && identity.is_none()
        {
            return Err(DestinationError::OutboundRequiresIdentity);
        }

        let name_hash = Self::compute_name_hash(app_name, aspects);

        let hash = match &identity {
            Some(id) => Self::compute_destination_hash(&name_hash, id.hash()),
            None => {
                // For PLAIN destinations without identity: hash = full_hash(name_hash)[:16]
                // This matches Python: RNS.Identity.full_hash(name_hash)[:TRUNCATED_HASHLENGTH//8]
                DestinationHash::new(truncated_hash(&name_hash))
            }
        };

        Ok(Self {
            hash,
            name_hash,
            identity,
            dest_type,
            direction,
            // Python parity: a fresh Destination accepts incoming links. The
            // OFF switch is `set_accepts_links(false)`.
            accepts_links: true,
            proof_strategy: ProofStrategy::None,
            // Ratchet fields - not enabled by default
            ratchets: Vec::new(),
            ratchet_interval_ms: DEFAULT_INTERVAL_MS,
            retained_ratchet_count: DEFAULT_RETAINED_RATCHETS,
            last_ratchet_time_ms: 0,
            enforce_ratchets: false,
            ratchets_enabled: false,
            ratchets_dirty: false,
            group_key: None,
        })
    }

    /// Get the destination hash
    pub fn hash(&self) -> &DestinationHash {
        &self.hash
    }

    /// Get the name hash
    pub fn name_hash(&self) -> &[u8; NAME_HASHBYTES] {
        &self.name_hash
    }

    /// Get the destination type
    pub fn dest_type(&self) -> DestinationType {
        self.dest_type
    }

    /// Get the direction
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// Get the associated identity
    pub fn identity(&self) -> Option<&Identity> {
        self.identity.as_ref()
    }

    /// Check if this destination accepts incoming links
    pub fn accepts_links(&self) -> bool {
        self.accepts_links
    }

    /// Set whether to accept incoming links
    pub fn set_accepts_links(&mut self, accept: bool) {
        self.accepts_links = accept;
    }

    /// Get the proof generation strategy
    pub fn proof_strategy(&self) -> ProofStrategy {
        self.proof_strategy
    }

    /// Set the proof generation strategy for incoming packets
    ///
    /// # Arguments
    /// * `strategy` - The proof strategy to use:
    ///   - `ProofStrategy::None` - Never generate proofs (default)
    ///   - `ProofStrategy::App` - Emit ProofRequested event for app to decide
    ///   - `ProofStrategy::All` - Automatically prove every packet
    pub fn set_proof_strategy(&mut self, strategy: ProofStrategy) {
        self.proof_strategy = strategy;
    }

    // GROUP symmetric key management
    /// Generate a new symmetric key for this GROUP destination.
    ///
    /// Mirrors Python `Destination.create_keys()` for the GROUP branch, which
    /// calls `Token.generate_key()` with its default mode (`AES_256_CBC`),
    /// producing 64 random bytes: 32-byte HMAC-SHA256 signing key followed by a
    /// 32-byte AES-256 key. A GROUP destination must hold a key before it can
    /// [`encrypt`](Self::encrypt)/[`decrypt`](Self::decrypt).
    ///
    /// # Errors
    /// * `NotGroupType` - the destination is not a GROUP destination.
    pub fn create_group_key(
        &mut self,
        rng: &mut impl CryptoRngCore,
    ) -> Result<(), DestinationError> {
        if self.dest_type != DestinationType::Group {
            return Err(DestinationError::NotGroupType);
        }
        let mut key = [0u8; TOKEN_KEY_SIZE];
        rng.fill_bytes(&mut key);
        self.group_key = Some(key);
        Ok(())
    }

    /// Load a shared symmetric key into this GROUP destination.
    ///
    /// Mirrors Python `Destination.load_private_key(key)` for the GROUP branch.
    /// The key must be exactly 64 bytes (the `AES_256_CBC` token key size that
    /// `create_keys` generates): the first 32 bytes are the HMAC signing key and
    /// the last 32 bytes are the AES-256 key.
    ///
    /// # Errors
    /// * `NotGroupType` - the destination is not a GROUP destination.
    /// * `InvalidGroupKeyLength` - the key is not 64 bytes.
    pub fn load_group_key(&mut self, key: &[u8]) -> Result<(), DestinationError> {
        if self.dest_type != DestinationType::Group {
            return Err(DestinationError::NotGroupType);
        }
        if key.len() != TOKEN_KEY_SIZE {
            return Err(DestinationError::InvalidGroupKeyLength);
        }
        let mut stored = [0u8; TOKEN_KEY_SIZE];
        stored.copy_from_slice(key);
        self.group_key = Some(stored);
        Ok(())
    }

    /// Return the symmetric key held by this GROUP destination.
    ///
    /// Mirrors Python `Destination.get_private_key()` for the GROUP branch.
    /// Returns `None` if no key has been created or loaded.
    pub fn group_key(&self) -> Option<&[u8; TOKEN_KEY_SIZE]> {
        self.group_key.as_ref()
    }

    // Ratchet Management
    /// Enable ratchets for forward secrecy on this destination
    ///
    /// Ratchets provide forward secrecy by rotating X25519 key pairs
    /// periodically. When enabled, announces will include the current
    /// ratchet public key, and packets must be encrypted with that ratchet.
    ///
    /// # Arguments
    /// * `rng` - Random number generator
    /// * `now_ms` - Current timestamp in milliseconds
    ///
    /// # Errors
    /// Returns error if called on an OUT destination.
    ///
    /// # Example
    /// ```
    /// use leviculum_core::{Destination, DestinationType, Direction};
    /// use leviculum_core::identity::Identity;
    /// use rand_core::OsRng;
    ///
    /// let identity = Identity::generate(&mut OsRng);
    /// let mut dest = Destination::new(
    ///     Some(identity),
    ///     Direction::In,
    ///     DestinationType::Single,
    ///     "app",
    ///     &["echo"],
    /// ).unwrap();
    ///
    /// dest.enable_ratchets(&mut OsRng, 1704067200000).unwrap();
    /// assert!(dest.ratchets_enabled());
    /// ```
    pub fn enable_ratchets(
        &mut self,
        rng: &mut impl CryptoRngCore,
        now_ms: u64,
    ) -> Result<(), DestinationError> {
        if self.direction == Direction::Out {
            return Err(DestinationError::CannotEnableRatchetsOnOut);
        }

        // Generate initial ratchet
        let ratchet = Ratchet::generate(rng, now_ms);
        self.last_ratchet_time_ms = now_ms;
        self.ratchets.insert(0, ratchet);
        self.ratchets_enabled = true;
        self.ratchets_dirty = true;

        Ok(())
    }

    /// Check if ratchets are enabled for this destination
    pub fn ratchets_enabled(&self) -> bool {
        self.ratchets_enabled
    }

    /// Get the current ratchet public key (for announces)
    ///
    /// Returns None if ratchets are not enabled.
    pub fn current_ratchet_public(&self) -> Option<[u8; RATCHET_SIZE]> {
        if !self.ratchets_enabled {
            return None;
        }
        self.ratchets.first().map(|r| r.public_key_bytes())
    }

    /// Rotate ratchet if the interval has passed
    ///
    /// Call this before creating an announce to ensure the ratchet is current.
    ///
    /// # Arguments
    /// * `rng` - Random number generator
    /// * `now_ms` - Current timestamp in milliseconds
    ///
    /// # Returns
    /// True if a new ratchet was generated.
    pub fn rotate_ratchet_if_needed(&mut self, rng: &mut impl CryptoRngCore, now_ms: u64) -> bool {
        if !self.ratchets_enabled {
            return false;
        }

        let elapsed = now_ms.saturating_sub(self.last_ratchet_time_ms);

        if elapsed < self.ratchet_interval_ms {
            return false;
        }

        // Generate new ratchet and add to front
        let ratchet = Ratchet::generate(rng, now_ms);
        self.ratchets.insert(0, ratchet);
        self.last_ratchet_time_ms = now_ms;
        self.ratchets_dirty = true;

        // Trim old ratchets if exceeding limit
        if self.ratchets.len() > self.retained_ratchet_count {
            self.ratchets.truncate(self.retained_ratchet_count);
        }

        true
    }

    /// Minimum ratchet interval in milliseconds (1 second)
    const MIN_RATCHET_INTERVAL_MS: u64 = 1000;

    /// Set the ratchet rotation interval
    ///
    /// # Arguments
    /// * `interval_ms` - Rotation interval in milliseconds (default: 30 minutes, minimum: 1 second)
    pub fn set_ratchet_interval(&mut self, interval_ms: u64) {
        self.ratchet_interval_ms = interval_ms.max(Self::MIN_RATCHET_INTERVAL_MS);
    }

    /// Get the current ratchet rotation interval in milliseconds
    pub fn ratchet_interval(&self) -> u64 {
        self.ratchet_interval_ms
    }

    /// Set the maximum number of ratchets to retain
    ///
    /// # Arguments
    /// * `count` - Maximum retained ratchets (default: 512, minimum: 1)
    pub fn set_retained_ratchets(&mut self, count: usize) {
        // Ensure at least 1 ratchet is retained
        self.retained_ratchet_count = count.max(1);

        // Trim if needed
        if self.ratchets.len() > self.retained_ratchet_count {
            self.ratchets.truncate(self.retained_ratchet_count);
        }
    }

    /// Get the number of retained ratchets
    pub fn retained_ratchet_count(&self) -> usize {
        self.retained_ratchet_count
    }

    /// Get the current number of ratchets stored
    pub fn ratchet_count(&self) -> usize {
        self.ratchets.len()
    }

    /// Check if ratchet keys have changed since last persist.
    pub fn ratchets_dirty(&self) -> bool {
        self.ratchets_dirty
    }

    /// Clear the dirty flag (call after persisting ratchet keys).
    pub fn clear_ratchets_dirty(&mut self) {
        self.ratchets_dirty = false;
    }

    /// Serialize ratchet private keys as Python-compatible signed msgpack.
    ///
    /// Format: `msgpack({"signature": Ed25519(inner), "ratchets": inner})`
    /// where inner = `msgpack([key1, key2, ...])` and each key is 32 bytes
    /// (X25519 private key), newest first.
    ///
    /// Python ref: `Destination._persist_ratchets()` (Destination.py:210-225).
    /// Python stores only 32-byte private keys, no timestamps. Public keys
    /// are derived on load via `X25519PrivateKey.from_private_bytes()`.
    ///
    /// Returns None if destination has no identity (cannot sign).
    pub fn serialize_ratchets_signed(&self) -> Option<Vec<u8>> {
        let identity = self.identity.as_ref()?;

        // Build inner: msgpack array of N binary(32) entries
        let inner = msgpack_encode_ratchet_array(&self.ratchets);

        // Sign the inner bytes
        let signature = identity.sign(&inner).ok()?;

        // Build outer: msgpack fixmap(2) with "signature" and "ratchets" keys
        msgpack_encode_signed_ratchets(&signature, &inner)
    }

    /// Load ratchet private keys from Python-compatible signed msgpack.
    ///
    /// Verifies Ed25519 signature, then reconstructs Ratchet structs.
    /// Since Python doesn't store timestamps, loaded ratchets get
    /// `created_at_ms = 0`.
    ///
    /// Python ref: `Destination._reload_ratchets()` (Destination.py:437-476).
    pub fn load_ratchets_signed(&mut self, data: &[u8]) -> Result<usize, DestinationError> {
        let identity = self.identity.as_ref().ok_or(DestinationError::NoIdentity)?;

        // Parse outer map: extract "signature" and "ratchets" blobs
        let (signature, inner) =
            msgpack_parse_signed_ratchets(data).ok_or(DestinationError::InvalidRatchetData)?;

        // Verify signature
        let valid = identity
            .verify(inner, signature)
            .map_err(|_| DestinationError::InvalidSignature)?;
        if !valid {
            return Err(DestinationError::InvalidSignature);
        }

        // Parse inner array: extract 32-byte private key blobs
        let keys =
            msgpack_parse_ratchet_array(inner).ok_or(DestinationError::InvalidRatchetData)?;

        let mut ratchets = Vec::with_capacity(keys.len());
        for key_bytes in keys {
            let mut key = [0u8; RATCHET_SIZE];
            key.copy_from_slice(key_bytes);
            ratchets.push(Ratchet::from_private_key_bytes(key, 0));
        }

        let count = ratchets.len();
        self.ratchets = ratchets;
        self.ratchets_dirty = false;

        if count > 0 {
            self.ratchets_enabled = true;
        }

        Ok(count)
    }

    /// Set the last ratchet rotation time.
    ///
    /// Used after loading persisted ratchets whose timestamps are from a
    /// previous session's monotonic domain and would block rotation.
    pub fn set_last_ratchet_time(&mut self, time_ms: u64) {
        self.last_ratchet_time_ms = time_ms;
    }

    /// Enforce ratchet-only decryption
    ///
    /// When enabled, packets not encrypted with a ratchet will be rejected
    /// even if they could be decrypted with the identity key.
    pub fn set_enforce_ratchets(&mut self, enforce: bool) {
        self.enforce_ratchets = enforce;
    }

    /// Check if ratchet enforcement is enabled
    pub fn enforces_ratchets(&self) -> bool {
        self.enforce_ratchets
    }

    // Encryption/Decryption
    /// Decrypt data received at this destination
    ///
    /// Tries decryption with each retained ratchet, then falls back to
    /// the identity key (unless ratchets are enforced).
    ///
    /// # Arguments
    /// * `ciphertext` - Encrypted data from an incoming packet
    ///
    /// # Returns
    /// Decrypted plaintext on success.
    ///
    /// # Errors
    /// * `NoIdentity` - Destination has no identity
    /// * `DecryptionFailed` - No ratchet or identity key could decrypt
    /// * `RatchetRequired` - Ratchets enforced but decrypted with identity key
    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>, DestinationError> {
        // GROUP destinations decrypt with the shared symmetric RNS Token.
        if self.dest_type == DestinationType::Group {
            return self.group_decrypt(ciphertext);
        }

        let identity = self.identity.as_ref().ok_or(DestinationError::NoIdentity)?;

        // Always try with identity fallback to know if ratchet was used
        let (plaintext, ratchet_id) =
            identity.decrypt_with_ratchets(ciphertext, &self.ratchets, true)?;

        // If ratchets are enforced and we decrypted without a ratchet, reject
        if self.enforce_ratchets && ratchet_id.is_none() {
            crate::tracing::debug!(
                "Packet rejected: ratchet enforcement active, \
                 packet encrypted without ratchet key"
            );
            return Err(DestinationError::RatchetRequired);
        }

        Ok(plaintext)
    }

    /// Encrypt data for sending to another destination
    ///
    /// For OUT destinations, encrypts using the target identity and
    /// optionally a known ratchet public key.
    ///
    /// # Arguments
    /// * `plaintext` - Data to encrypt
    /// * `ratchet_public` - Optional ratchet public key from target's announce
    /// * `rng` - Random number generator
    ///
    /// # Returns
    /// Ciphertext that can be decrypted by the target destination.
    ///
    /// # Errors
    /// * `NoIdentity` - Destination has no identity to encrypt for
    pub fn encrypt(
        &self,
        plaintext: &[u8],
        ratchet_public: Option<&[u8; RATCHET_SIZE]>,
        rng: &mut impl CryptoRngCore,
    ) -> Result<Vec<u8>, DestinationError> {
        // GROUP destinations encrypt with the shared symmetric RNS Token. The
        // ratchet key is not used for GROUP; the random IV comes from `rng`.
        if self.dest_type == DestinationType::Group {
            return self.group_encrypt(plaintext, rng);
        }

        let identity = self.identity.as_ref().ok_or(DestinationError::NoIdentity)?;

        Ok(identity.encrypt_for_destination(plaintext, ratchet_public, rng)?)
    }

    /// Encrypt with the GROUP shared key, producing RNS Token wire bytes.
    ///
    /// Layout (Python `RNS.Cryptography.Token`): `IV(16) || AES-256-CBC(PKCS7)
    /// || HMAC-SHA256(32)`, HMAC computed over `IV || ciphertext`.
    fn group_encrypt(
        &self,
        plaintext: &[u8],
        rng: &mut impl CryptoRngCore,
    ) -> Result<Vec<u8>, DestinationError> {
        let key = self
            .group_key
            .as_ref()
            .ok_or(DestinationError::NoGroupKey)?;

        let mut iv = [0u8; 16];
        rng.fill_bytes(&mut iv);

        // token length = IV(16) + padded ciphertext + HMAC(32)
        let padded_len = ((plaintext.len() / 16) + 1) * 16;
        let mut out = vec![0u8; 16 + padded_len + 32];

        let n = encrypt_token(key, &iv, plaintext, &mut out)
            .map_err(|_| DestinationError::EncryptionFailed)?;
        out.truncate(n);
        Ok(out)
    }

    /// Decrypt RNS Token wire bytes with the GROUP shared key.
    fn group_decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>, DestinationError> {
        let key = self
            .group_key
            .as_ref()
            .ok_or(DestinationError::NoGroupKey)?;

        // Plaintext is never longer than the ciphertext body.
        let mut out = vec![0u8; ciphertext.len()];
        let n = decrypt_token(key, ciphertext, &mut out)
            .map_err(|_| DestinationError::DecryptionFailed)?;
        out.truncate(n);
        Ok(out)
    }

    /// Compute the name hash from app_name and aspects.
    ///
    /// The name hash is the first 10 bytes of SHA256(app_name.aspect1.aspect2...).
    pub fn compute_name_hash(app_name: &str, aspects: &[&str]) -> [u8; NAME_HASHBYTES] {
        let mut full_name = String::from(app_name);
        for aspect in aspects {
            full_name.push('.');
            full_name.push_str(aspect);
        }

        let hash = sha256(full_name.as_bytes());
        let mut name_hash = [0u8; NAME_HASHBYTES];
        name_hash.copy_from_slice(&hash[..NAME_HASHBYTES]);
        name_hash
    }

    /// Compute the destination hash from name_hash and identity_hash.
    ///
    /// destination_hash = truncated_hash(name_hash + identity_hash)
    pub fn compute_destination_hash(
        name_hash: &[u8; NAME_HASHBYTES],
        identity_hash: &[u8; IDENTITY_HASHBYTES],
    ) -> DestinationHash {
        let mut combined = [0u8; NAME_HASHBYTES + IDENTITY_HASHBYTES];
        combined[..NAME_HASHBYTES].copy_from_slice(name_hash);
        combined[NAME_HASHBYTES..].copy_from_slice(identity_hash);
        DestinationHash::new(truncated_hash(&combined))
    }

    /// Create a signed announce packet for this destination.
    ///
    /// Announces inform the network about this destination's presence.
    /// Only IN destinations can announce (they receive traffic).
    ///
    /// If ratchets are enabled, the announce will include the current ratchet
    /// public key and the context_flag will be set. The ratchet will be rotated
    /// if the rotation interval has passed.
    ///
    /// # Arguments
    /// * `app_data` - Optional application-specific data (max ~350 bytes)
    /// * `rng` - Random number generator
    /// * `now_ms` - Current timestamp in milliseconds
    ///
    /// # Errors
    /// * `OnlySingleCanAnnounce` - Only SINGLE destinations can announce
    /// * `WrongDirection` - OUT destinations cannot announce
    /// * `NoIdentity` - Destination has no identity
    /// * `SigningFailed` - Signature could not be created
    ///
    /// # Example
    /// ```
    /// use leviculum_core::{Destination, DestinationType, Direction};
    /// use leviculum_core::identity::Identity;
    /// use rand_core::OsRng;
    ///
    /// let identity = Identity::generate(&mut OsRng);
    /// let mut dest = Destination::new(
    ///     Some(identity),
    ///     Direction::In,
    ///     DestinationType::Single,
    ///     "app",
    ///     &["echo"],
    /// ).unwrap();
    ///
    /// let packet = dest.announce(Some(b"my-data"), &mut OsRng, 1704067200000).unwrap();
    /// assert_eq!(packet.destination_hash, *dest.hash());
    /// ```
    pub fn announce(
        &mut self,
        app_data: Option<&[u8]>,
        rng: &mut impl CryptoRngCore,
        now_ms: u64,
    ) -> Result<Packet, AnnounceError> {
        // Only SINGLE destinations can announce (per Python Reticulum spec)
        if self.dest_type != DestinationType::Single {
            return Err(AnnounceError::OnlySingleCanAnnounce);
        }

        // Only IN destinations can announce
        if self.direction != Direction::In {
            return Err(AnnounceError::WrongDirection);
        }

        // Check identity exists early
        if self.identity.is_none() {
            return Err(AnnounceError::NoIdentity);
        }

        // Rotate ratchet if needed
        self.rotate_ratchet_if_needed(rng, now_ms);

        // Get current ratchet (if enabled)
        let ratchet = self.current_ratchet_public();
        let has_ratchet = ratchet.is_some();

        // Get identity (safe because we checked above)
        let identity = self.identity.as_ref().unwrap();

        // Build the signed payload with optional ratchet
        let payload = build_announce_payload(
            identity,
            self.hash.as_bytes(),
            &self.name_hash,
            ratchet.as_ref(),
            app_data,
            rng,
            now_ms,
        )?;

        // Create the packet
        let packet = Packet {
            flags: PacketFlags {
                ifac_flag: false,
                header_type: HeaderType::Type1,
                context_flag: has_ratchet, // Set if ratchet is present
                transport_type: TransportType::Broadcast,
                dest_type: self.dest_type,
                packet_type: PacketType::Announce,
            },
            hops: 0,
            transport_id: None,
            destination_hash: self.hash.into_bytes(),
            context: PacketContext::None,
            data: PacketData::Owned(payload),
        };

        Ok(packet)
    }
}

// Hand-rolled msgpack helpers (no_std compatible)
//
// These encode/decode only the exact msgpack types needed for the signed
// ratchet format. Not a general-purpose msgpack library.

/// Encode an array of ratchet private keys as msgpack.
///
/// Format: array header + N × bin8(32).
fn msgpack_encode_ratchet_array(ratchets: &[Ratchet]) -> Vec<u8> {
    let n = ratchets.len();
    // Each entry: 1 (bin8) + 1 (len=32) + 32 (data) = 34 bytes
    let mut buf = Vec::with_capacity(3 + n * 34);

    // Array header
    if n <= 15 {
        buf.push(0x90 | (n as u8));
    } else {
        buf.push(0xdc); // array16
        buf.extend_from_slice(&(n as u16).to_be_bytes());
    }

    // Elements: bin8(32) for each private key
    for ratchet in ratchets {
        buf.push(0xc4); // bin8
        buf.push(RATCHET_SIZE as u8); // 32
        buf.extend_from_slice(&ratchet.private_key_bytes());
    }

    buf
}

/// Encode the outer signed ratchets map as msgpack.
///
/// Format: fixmap(2) { "signature": bin8(64), "ratchets": bin16/bin32(inner) }
fn msgpack_encode_signed_ratchets(signature: &[u8; 64], inner: &[u8]) -> Option<Vec<u8>> {
    // Outer map size: 1 (fixmap) + 10 (fixstr "signature") + 2+64 (bin8 sig)
    //               + 9 (fixstr "ratchets") + 3..5+inner.len() (bin16/bin32)
    let mut buf = Vec::with_capacity(1 + 10 + 66 + 9 + 5 + inner.len());

    buf.push(0x82); // fixmap(2)

    // Key: "signature" (9 bytes)
    buf.push(0xa9); // fixstr(9)
    buf.extend_from_slice(b"signature");

    // Value: bin8(64). Ed25519 signature
    buf.push(0xc4); // bin8
    buf.push(64);
    buf.extend_from_slice(signature);

    // Key: "ratchets" (8 bytes)
    buf.push(0xa8); // fixstr(8)
    buf.extend_from_slice(b"ratchets");

    // Value: bin16 or bin32, inner msgpack array
    let inner_len = inner.len();
    if inner_len <= 0xFFFF {
        buf.push(0xc5); // bin16
        buf.extend_from_slice(&(inner_len as u16).to_be_bytes());
    } else {
        buf.push(0xc6); // bin32
        buf.extend_from_slice(&(inner_len as u32).to_be_bytes());
    }
    buf.extend_from_slice(inner);

    Some(buf)
}

/// Parse the outer signed ratchets map from msgpack.
///
/// Returns (signature_bytes, inner_ratchets_bytes) or None if malformed.
fn msgpack_parse_signed_ratchets(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let mut pos = 0;

    // fixmap(2)
    if read_byte(data, &mut pos)? != 0x82 {
        return None;
    }

    let mut signature: Option<&[u8]> = None;
    let mut ratchets: Option<&[u8]> = None;

    for _ in 0..2 {
        let key = read_msgpack_str(data, &mut pos)?;
        let value = read_msgpack_bin(data, &mut pos)?;

        if key == b"signature" {
            signature = Some(value);
        } else if key == b"ratchets" {
            ratchets = Some(value);
        }
    }

    Some((signature?, ratchets?))
}

/// Parse an array of 32-byte binary blobs from msgpack.
///
/// Returns slices into the original data.
fn msgpack_parse_ratchet_array(data: &[u8]) -> Option<Vec<&[u8]>> {
    let mut pos = 0;
    let count = read_msgpack_array_len(data, &mut pos)?;

    let mut keys = Vec::with_capacity(count);
    for _ in 0..count {
        let blob = read_msgpack_bin(data, &mut pos)?;
        if blob.len() != RATCHET_SIZE {
            return None;
        }
        keys.push(blob);
    }

    Some(keys)
}

// Low-level msgpack readers
fn read_byte(data: &[u8], pos: &mut usize) -> Option<u8> {
    let b = *data.get(*pos)?;
    *pos += 1;
    Some(b)
}

fn read_be_u16(data: &[u8], pos: &mut usize) -> Option<u16> {
    if *pos + 2 > data.len() {
        return None;
    }
    let val = u16::from_be_bytes([data[*pos], data[*pos + 1]]);
    *pos += 2;
    Some(val)
}

fn read_be_u32(data: &[u8], pos: &mut usize) -> Option<u32> {
    if *pos + 4 > data.len() {
        return None;
    }
    let val = u32::from_be_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    Some(val)
}

fn read_msgpack_str<'a>(data: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    let tag = read_byte(data, pos)?;
    let len = if tag & 0xe0 == 0xa0 {
        // fixstr: 0xa0..0xbf, length in lower 5 bits
        (tag & 0x1f) as usize
    } else if tag == 0xd9 {
        // str8
        read_byte(data, pos)? as usize
    } else if tag == 0xda {
        // str16
        read_be_u16(data, pos)? as usize
    } else {
        return None;
    };

    if *pos + len > data.len() {
        return None;
    }
    let s = &data[*pos..*pos + len];
    *pos += len;
    Some(s)
}

fn read_msgpack_bin<'a>(data: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    let tag = read_byte(data, pos)?;
    let len = if tag == 0xc4 {
        // bin8
        read_byte(data, pos)? as usize
    } else if tag == 0xc5 {
        // bin16
        read_be_u16(data, pos)? as usize
    } else if tag == 0xc6 {
        // bin32
        read_be_u32(data, pos)? as usize
    } else {
        return None;
    };

    if *pos + len > data.len() {
        return None;
    }
    let b = &data[*pos..*pos + len];
    *pos += len;
    Some(b)
}

fn read_msgpack_array_len(data: &[u8], pos: &mut usize) -> Option<usize> {
    let tag = read_byte(data, pos)?;
    if tag & 0xf0 == 0x90 {
        // fixarray: 0x90..0x9f
        Some((tag & 0x0f) as usize)
    } else if tag == 0xdc {
        // array16
        Some(read_be_u16(data, pos)? as usize)
    } else if tag == 0xdd {
        // array32
        Some(read_be_u32(data, pos)? as usize)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::announce::ReceivedAnnounce;
    use rand_core::OsRng;

    const TEST_TIME_MS: u64 = 1704067200000; // 2024-01-01 00:00:00 UTC

    #[test]
    fn test_destination_creation() {
        let identity = Identity::generate(&mut OsRng);
        let dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "test",
            &["echo"],
        )
        .unwrap();

        assert_eq!(dest.dest_type(), DestinationType::Single);
        assert_eq!(dest.direction(), Direction::In);
        assert_eq!(dest.hash().as_bytes().len(), TRUNCATED_HASHBYTES);
    }

    #[test]
    fn test_name_hash() {
        let hash1 = Destination::compute_name_hash("app", &["aspect1"]);
        let hash2 = Destination::compute_name_hash("app", &["aspect2"]);
        let hash3 = Destination::compute_name_hash("app", &["aspect1"]);

        assert_ne!(hash1, hash2);
        assert_eq!(hash1, hash3);
    }

    #[test]
    fn test_destination_type_conversion() {
        assert_eq!(DestinationType::try_from(0x00), Ok(DestinationType::Single));
        assert_eq!(DestinationType::try_from(0x01), Ok(DestinationType::Group));
        assert_eq!(DestinationType::try_from(0x02), Ok(DestinationType::Plain));
        assert_eq!(DestinationType::try_from(0x03), Ok(DestinationType::Link));
        assert_eq!(DestinationType::try_from(0x04), Err(()));
    }

    #[test]
    fn test_destination_announce_creates_valid_packet() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["echo"],
        )
        .unwrap();

        let packet = dest
            .announce(Some(b"hello"), &mut OsRng, TEST_TIME_MS)
            .unwrap();

        // Verify packet structure
        assert_eq!(packet.flags.packet_type, PacketType::Announce);
        assert_eq!(packet.flags.transport_type, TransportType::Broadcast);
        assert_eq!(packet.flags.dest_type, DestinationType::Single);
        assert_eq!(packet.flags.header_type, HeaderType::Type1);
        assert!(!packet.flags.context_flag); // No ratchet
        assert_eq!(packet.hops, 0);
        assert_eq!(packet.destination_hash, *dest.hash());

        // Parse and verify the announce
        let announce = ReceivedAnnounce::from_packet(&packet).unwrap();
        assert!(announce.verify_destination_hash());
        assert!(announce.verify_signature().unwrap());
        assert_eq!(announce.app_data(), b"hello");
    }

    #[test]
    fn test_destination_announce_out_direction_fails() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::Out, // OUT cannot announce
            DestinationType::Single,
            "testapp",
            &["echo"],
        )
        .unwrap();

        let result = dest.announce(None, &mut OsRng, TEST_TIME_MS);

        assert!(matches!(result, Err(AnnounceError::WrongDirection)));
    }

    #[test]
    fn test_destination_announce_plain_type_fails() {
        // PLAIN destinations cannot announce (only SINGLE can)
        let mut dest = Destination::new(
            None, // No identity (valid for PLAIN)
            Direction::In,
            DestinationType::Plain,
            "testapp",
            &["echo"],
        )
        .unwrap();

        let result = dest.announce(None, &mut OsRng, TEST_TIME_MS);

        // PLAIN destinations can't announce - OnlySingleCanAnnounce takes priority
        assert!(matches!(result, Err(AnnounceError::OnlySingleCanAnnounce)));
    }

    #[test]
    fn test_destination_announce_without_app_data() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["echo"],
        )
        .unwrap();

        let packet = dest.announce(None, &mut OsRng, TEST_TIME_MS).unwrap();

        let announce = ReceivedAnnounce::from_packet(&packet).unwrap();
        assert!(announce.app_data().is_empty());
        assert!(announce.validate().is_ok());
    }

    #[test]
    fn test_destination_announce_validates_correctly() {
        // Create destination and announce
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "myapp",
            &["service", "v1"],
        )
        .unwrap();

        let packet = dest
            .announce(Some(b"app-data"), &mut OsRng, TEST_TIME_MS)
            .unwrap();

        // Full validation should pass
        let announce = ReceivedAnnounce::from_packet(&packet).unwrap();
        assert!(announce.validate().is_ok());

        // Computed hashes should match
        assert_eq!(announce.computed_destination_hash(), *dest.hash());
    }

    // Spec Compliance Tests
    #[test]
    fn test_plain_destination_cannot_have_identity() {
        let identity = Identity::generate(&mut OsRng);
        let result = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Plain,
            "test",
            &["echo"],
        );
        assert!(matches!(
            result,
            Err(DestinationError::PlainCannotHaveIdentity)
        ));
    }

    #[test]
    fn test_plain_destination_without_identity_succeeds() {
        let result = Destination::new(
            None,
            Direction::In,
            DestinationType::Plain,
            "test",
            &["echo"],
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_single_out_requires_identity() {
        let result = Destination::new(
            None,
            Direction::Out,
            DestinationType::Single,
            "test",
            &["echo"],
        );
        assert!(matches!(
            result,
            Err(DestinationError::OutboundRequiresIdentity)
        ));
    }

    #[test]
    fn test_group_out_requires_identity() {
        let result = Destination::new(
            None,
            Direction::Out,
            DestinationType::Group,
            "test",
            &["echo"],
        );
        assert!(matches!(
            result,
            Err(DestinationError::OutboundRequiresIdentity)
        ));
    }

    #[test]
    fn test_only_single_can_announce() {
        let identity = Identity::generate(&mut OsRng);

        // GROUP destination cannot announce
        let mut group_dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Group,
            "test",
            &["echo"],
        )
        .unwrap();

        let result = group_dest.announce(None, &mut OsRng, TEST_TIME_MS);
        assert!(matches!(result, Err(AnnounceError::OnlySingleCanAnnounce)));
    }

    #[test]
    fn test_single_in_can_announce() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "test",
            &["echo"],
        )
        .unwrap();

        let result = dest.announce(None, &mut OsRng, TEST_TIME_MS);
        assert!(result.is_ok());
    }

    #[test]
    fn test_plain_out_allowed_without_identity() {
        // PLAIN OUT is valid without identity (for local broadcast)
        let result = Destination::new(
            None,
            Direction::Out,
            DestinationType::Plain,
            "test",
            &["broadcast"],
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_single_in_without_identity_allowed() {
        // SINGLE IN can exist without identity initially
        // (auto-creation would happen in a full implementation)
        let result = Destination::new(
            None,
            Direction::In,
            DestinationType::Single,
            "test",
            &["echo"],
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_group_in_allowed_with_identity() {
        let identity = Identity::generate(&mut OsRng);
        let result = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Group,
            "test",
            &["broadcast"],
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_link_type_allowed_without_identity() {
        // LINK type is internal use, allowing various configs
        let result = Destination::new(
            None,
            Direction::In,
            DestinationType::Link,
            "test",
            &["link"],
        );
        assert!(result.is_ok());
    }

    // Ratchet Tests
    #[test]
    fn test_enable_ratchets() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "test",
            &["ratchet"],
        )
        .unwrap();

        assert!(!dest.ratchets_enabled());
        assert!(dest.current_ratchet_public().is_none());
        assert_eq!(dest.ratchet_count(), 0);

        dest.enable_ratchets(&mut OsRng, TEST_TIME_MS).unwrap();

        assert!(dest.ratchets_enabled());
        assert!(dest.current_ratchet_public().is_some());
        assert_eq!(dest.ratchet_count(), 1);
    }

    #[test]
    fn test_enable_ratchets_on_out_fails() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::Out,
            DestinationType::Single,
            "test",
            &["ratchet"],
        )
        .unwrap();

        let result = dest.enable_ratchets(&mut OsRng, TEST_TIME_MS);

        assert!(matches!(
            result,
            Err(DestinationError::CannotEnableRatchetsOnOut)
        ));
    }

    #[test]
    fn test_ratchet_rotation() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "test",
            &["ratchet"],
        )
        .unwrap();

        // Use a shorter interval for testing
        dest.set_ratchet_interval(1000); // 1 second

        let mut time_ms = TEST_TIME_MS;
        dest.enable_ratchets(&mut OsRng, time_ms).unwrap();

        let first_ratchet = dest.current_ratchet_public().unwrap();

        // No rotation needed yet
        assert!(!dest.rotate_ratchet_if_needed(&mut OsRng, time_ms));
        assert_eq!(dest.current_ratchet_public().unwrap(), first_ratchet);

        // Advance time past interval
        time_ms += 2000; // 2 seconds later

        // Rotation should happen
        assert!(dest.rotate_ratchet_if_needed(&mut OsRng, time_ms));
        let second_ratchet = dest.current_ratchet_public().unwrap();
        assert_ne!(first_ratchet, second_ratchet);

        // Old ratchet should still be retained
        assert_eq!(dest.ratchet_count(), 2);
    }

    #[test]
    fn test_retained_ratchet_limit() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "test",
            &["ratchet"],
        )
        .unwrap();

        // Set small limit for testing
        dest.set_retained_ratchets(3);
        dest.set_ratchet_interval(1000);

        let mut time_ms = TEST_TIME_MS;
        dest.enable_ratchets(&mut OsRng, time_ms).unwrap();
        assert_eq!(dest.ratchet_count(), 1);

        // Rotate 5 times
        for _ in 1..=5 {
            time_ms += 2000;
            dest.rotate_ratchet_if_needed(&mut OsRng, time_ms);
        }

        // Should only have 3 ratchets (limit enforced)
        assert_eq!(dest.ratchet_count(), 3);
    }

    #[test]
    fn test_encrypt_decrypt_with_ratchet() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "test",
            &["ratchet"],
        )
        .unwrap();

        dest.enable_ratchets(&mut OsRng, TEST_TIME_MS).unwrap();

        // Get ratchet public key (simulating what a sender would get from announce)
        let ratchet_pub = dest.current_ratchet_public().unwrap();

        // Encrypt using the destination's identity and ratchet
        let plaintext = b"Hello with forward secrecy!";
        let ciphertext = dest
            .encrypt(plaintext, Some(&ratchet_pub), &mut OsRng)
            .unwrap();

        // Decrypt
        let decrypted = dest.decrypt(&ciphertext).unwrap();
        assert_eq!(&decrypted[..], plaintext);
    }

    #[test]
    fn test_decrypt_with_old_ratchet() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "test",
            &["ratchet"],
        )
        .unwrap();

        dest.set_ratchet_interval(1000);

        let mut time_ms = TEST_TIME_MS;
        dest.enable_ratchets(&mut OsRng, time_ms).unwrap();

        // Encrypt with current ratchet
        let first_ratchet = dest.current_ratchet_public().unwrap();
        let plaintext = b"Message encrypted with first ratchet";
        let ciphertext = dest
            .encrypt(plaintext, Some(&first_ratchet), &mut OsRng)
            .unwrap();

        // Rotate ratchet
        time_ms += 2000;
        dest.rotate_ratchet_if_needed(&mut OsRng, time_ms);

        // Should still be able to decrypt with old ratchet
        let decrypted = dest.decrypt(&ciphertext).unwrap();
        assert_eq!(&decrypted[..], plaintext);
    }

    #[test]
    fn test_enforce_ratchets() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "test",
            &["ratchet"],
        )
        .unwrap();

        dest.enable_ratchets(&mut OsRng, TEST_TIME_MS).unwrap();
        dest.set_enforce_ratchets(true);

        // Encrypt WITHOUT ratchet (using identity key)
        let plaintext = b"No ratchet used";
        let ciphertext = dest.encrypt(plaintext, None, &mut OsRng).unwrap();

        // Should fail because ratchets are enforced
        let result = dest.decrypt(&ciphertext);
        assert!(matches!(result, Err(DestinationError::RatchetRequired)));
    }

    #[test]
    fn test_decrypt_without_ratchet_when_not_enforced() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "test",
            &["ratchet"],
        )
        .unwrap();

        dest.enable_ratchets(&mut OsRng, TEST_TIME_MS).unwrap();

        // Encrypt WITHOUT ratchet (using identity key)
        let plaintext = b"No ratchet used";
        let ciphertext = dest.encrypt(plaintext, None, &mut OsRng).unwrap();

        // Should succeed because ratchets are not enforced
        let decrypted = dest.decrypt(&ciphertext).unwrap();
        assert_eq!(&decrypted[..], plaintext);
    }

    #[test]
    fn test_ratchet_settings() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "test",
            &["ratchet"],
        )
        .unwrap();

        // Test default settings
        assert_eq!(dest.ratchet_interval(), DEFAULT_INTERVAL_MS);
        assert_eq!(dest.retained_ratchet_count(), DEFAULT_RETAINED_RATCHETS);
        assert!(!dest.enforces_ratchets());

        // Change settings
        dest.set_ratchet_interval(60000);
        dest.set_retained_ratchets(100);
        dest.set_enforce_ratchets(true);

        assert_eq!(dest.ratchet_interval(), 60000);
        assert_eq!(dest.retained_ratchet_count(), 100);
        assert!(dest.enforces_ratchets());
    }

    #[test]
    fn test_no_rotation_when_ratchets_disabled() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "test",
            &["ratchet"],
        )
        .unwrap();

        // Don't enable ratchets
        let far_future = TEST_TIME_MS + 100000000;
        assert!(!dest.rotate_ratchet_if_needed(&mut OsRng, far_future));
    }

    // Ratcheted Announce Tests
    #[test]
    fn test_announce_with_ratchet() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "test",
            &["ratchet"],
        )
        .unwrap();

        dest.enable_ratchets(&mut OsRng, TEST_TIME_MS).unwrap();

        let packet = dest
            .announce(Some(b"test-data"), &mut OsRng, TEST_TIME_MS)
            .unwrap();

        // context_flag should be set
        assert!(packet.flags.context_flag);

        // Parse and verify
        let announce = ReceivedAnnounce::from_packet(&packet).unwrap();
        assert!(announce.has_ratchet());
        assert!(announce.validate().is_ok());
        assert_eq!(announce.app_data(), b"test-data");

        // Ratchet in announce should match destination's current ratchet
        assert_eq!(
            announce.ratchet().unwrap(),
            &dest.current_ratchet_public().unwrap()
        );
    }

    #[test]
    fn test_announce_without_ratchet_when_disabled() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "test",
            &["noratchet"],
        )
        .unwrap();

        // Don't enable ratchets

        let packet = dest
            .announce(Some(b"test-data"), &mut OsRng, TEST_TIME_MS)
            .unwrap();

        // context_flag should NOT be set
        assert!(!packet.flags.context_flag);

        // Parse and verify
        let announce = ReceivedAnnounce::from_packet(&packet).unwrap();
        assert!(!announce.has_ratchet());
        assert!(announce.ratchet().is_none());
        assert!(announce.validate().is_ok());
    }

    #[test]
    fn test_announce_rotates_ratchet() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "test",
            &["ratchet"],
        )
        .unwrap();

        dest.set_ratchet_interval(1000);

        let mut time_ms = TEST_TIME_MS;
        dest.enable_ratchets(&mut OsRng, time_ms).unwrap();

        let first_ratchet = dest.current_ratchet_public().unwrap();

        // First announce - uses first ratchet
        let packet1 = dest.announce(None, &mut OsRng, time_ms).unwrap();
        let announce1 = ReceivedAnnounce::from_packet(&packet1).unwrap();
        assert_eq!(announce1.ratchet().unwrap(), &first_ratchet);

        // Advance time to trigger rotation
        time_ms += 2000;

        // Second announce - should rotate and use new ratchet
        let packet2 = dest.announce(None, &mut OsRng, time_ms).unwrap();
        let announce2 = ReceivedAnnounce::from_packet(&packet2).unwrap();

        let second_ratchet = dest.current_ratchet_public().unwrap();
        assert_ne!(first_ratchet, second_ratchet);
        assert_eq!(announce2.ratchet().unwrap(), &second_ratchet);
    }

    // Proof Strategy Tests
    #[test]
    fn test_proof_strategy_default() {
        let identity = Identity::generate(&mut OsRng);
        let dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "test",
            &["proof"],
        )
        .unwrap();

        assert_eq!(dest.proof_strategy(), ProofStrategy::None);
    }

    #[test]
    fn test_proof_strategy_setter() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "test",
            &["proof"],
        )
        .unwrap();

        dest.set_proof_strategy(ProofStrategy::All);
        assert_eq!(dest.proof_strategy(), ProofStrategy::All);

        dest.set_proof_strategy(ProofStrategy::App);
        assert_eq!(dest.proof_strategy(), ProofStrategy::App);

        dest.set_proof_strategy(ProofStrategy::None);
        assert_eq!(dest.proof_strategy(), ProofStrategy::None);
    }

    #[test]
    fn test_proof_strategy_enum_values() {
        // Verify the enum values match the protocol constants
        assert_eq!(ProofStrategy::None as u8, 0x21);
        assert_eq!(ProofStrategy::App as u8, 0x22);
        assert_eq!(ProofStrategy::All as u8, 0x23);
    }

    // DestinationHash Tests
    #[test]
    fn test_destination_hash_construction() {
        let bytes = [0x42u8; 16];
        let hash = DestinationHash::new(bytes);
        assert_eq!(*hash.as_bytes(), bytes);
    }

    #[test]
    fn test_destination_hash_into_bytes_roundtrip() {
        let bytes = [0xAB; 16];
        let hash = DestinationHash::new(bytes);
        assert_eq!(hash.into_bytes(), bytes);
    }

    #[test]
    fn test_destination_hash_from_array() {
        let bytes = [0x01; 16];
        let hash: DestinationHash = bytes.into();
        assert_eq!(*hash.as_bytes(), bytes);

        let back: [u8; 16] = hash.into();
        assert_eq!(back, bytes);
    }

    #[test]
    fn test_destination_hash_display() {
        let hash = DestinationHash::new([
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef,
        ]);
        let display = alloc::format!("{}", hash);
        assert_eq!(display, "0123456789abcdef0123456789abcdef");
    }

    #[test]
    fn test_destination_hash_debug() {
        let hash = DestinationHash::new([0xAA; 16]);
        let debug = alloc::format!("{:?}", hash);
        assert!(debug.starts_with("DestinationHash("));
        assert!(debug.contains("aa"));
    }

    #[test]
    fn test_destination_hash_equality_with_raw() {
        let bytes = [0x42; 16];
        let hash = DestinationHash::new(bytes);
        assert_eq!(hash, bytes);
        assert_eq!(bytes, hash);
    }

    #[test]
    fn test_destination_hash_inequality() {
        let hash1 = DestinationHash::new([0x01; 16]);
        let hash2 = DestinationHash::new([0x02; 16]);
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_destination_hash_as_ref() {
        let hash = DestinationHash::new([0x42; 16]);
        let slice: &[u8] = hash.as_ref();
        assert_eq!(slice.len(), 16);
        assert_eq!(slice[0], 0x42);

        let arr_ref: &[u8; 16] = hash.as_ref();
        assert_eq!(*arr_ref, [0x42; 16]);
    }

    #[test]
    fn test_destination_hash_borrow() {
        use core::borrow::Borrow;
        let hash = DestinationHash::new([0x42; 16]);
        let borrowed: &[u8; 16] = hash.borrow();
        assert_eq!(*borrowed, [0x42; 16]);
    }

    #[test]
    fn test_destination_hash_ord() {
        let hash1 = DestinationHash::new([0x01; 16]);
        let hash2 = DestinationHash::new([0x02; 16]);
        assert!(hash1 < hash2);
    }

    #[test]
    fn test_destination_hash_copy() {
        let hash1 = DestinationHash::new([0x42; 16]);
        let hash2 = hash1; // Copy
        assert_eq!(hash1, hash2); // Original still usable
    }

    #[test]
    fn test_destination_hash_is_different_type_from_link_id() {
        use crate::link::LinkId;
        let bytes = [0x42; 16];
        let _dest_hash = DestinationHash::new(bytes);
        let _link_id = LinkId::new(bytes);
        // These are different types - compiler prevents mixing them
        // (No PartialEq<LinkId> for DestinationHash or vice versa)
    }

    #[test]
    fn test_destination_hash_btreemap_key() {
        use alloc::collections::BTreeMap;
        let mut map = BTreeMap::new();
        let hash = DestinationHash::new([0x42; 16]);
        map.insert(hash, "test");
        assert_eq!(map.get(&hash), Some(&"test"));
        // Lookup via raw bytes via Borrow
        assert_eq!(map.get(&[0x42u8; 16]), Some(&"test"));
    }

    // Signed Ratchet Serialization Tests
    #[test]
    fn test_serialize_ratchets_signed_roundtrip() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["ratchetrt"],
        )
        .unwrap();

        dest.enable_ratchets(&mut OsRng, TEST_TIME_MS).unwrap();
        // Generate a few more ratchets by forcing rotation
        dest.set_ratchet_interval(1000); // 1 second
        assert!(dest.rotate_ratchet_if_needed(&mut OsRng, TEST_TIME_MS + 2000));
        assert!(dest.rotate_ratchet_if_needed(&mut OsRng, TEST_TIME_MS + 4000));
        let original_count = dest.ratchet_count();
        assert_eq!(original_count, 3);

        // Collect original public keys
        let original_keys: Vec<[u8; RATCHET_SIZE]> =
            dest.ratchets.iter().map(|r| r.public_key_bytes()).collect();

        let signed = dest.serialize_ratchets_signed().unwrap();

        // Load into a fresh destination with the same identity
        let id_bytes = dest.identity().unwrap().private_key_bytes().unwrap();
        let identity2 = Identity::from_private_key_bytes(&id_bytes).unwrap();
        let mut dest2 = Destination::new(
            Some(identity2),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["ratchetrt"],
        )
        .unwrap();

        let loaded = dest2.load_ratchets_signed(&signed).unwrap();
        assert_eq!(loaded, original_count);
        assert_eq!(dest2.ratchet_count(), original_count);
        assert!(dest2.ratchets_enabled());

        // Verify same public keys (derived from same private keys)
        let loaded_keys: Vec<[u8; RATCHET_SIZE]> = dest2
            .ratchets
            .iter()
            .map(|r| r.public_key_bytes())
            .collect();
        assert_eq!(original_keys, loaded_keys);

        // Loaded ratchets preserve keys (no timestamps in Python format)
        assert_eq!(dest2.ratchets.len(), original_keys.len());
    }

    #[test]
    fn test_load_ratchets_signed_bad_signature() {
        let identity = Identity::generate(&mut OsRng);
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["badsig"],
        )
        .unwrap();

        dest.enable_ratchets(&mut OsRng, TEST_TIME_MS).unwrap();
        let mut signed = dest.serialize_ratchets_signed().unwrap();

        // Flip a byte in the signature (which is at a known offset after the key)
        // The signature comes after: 0x82 + 0xa9 + "signature" + 0xc4 + 0x40 = 13 bytes
        signed[13] ^= 0xff;

        let id_bytes = dest.identity().unwrap().private_key_bytes().unwrap();
        let identity2 = Identity::from_private_key_bytes(&id_bytes).unwrap();
        let mut dest2 = Destination::new(
            Some(identity2),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["badsig"],
        )
        .unwrap();

        let result = dest2.load_ratchets_signed(&signed);
        assert_eq!(result, Err(DestinationError::InvalidSignature));
    }

    #[test]
    fn test_load_ratchets_signed_python_compat() {
        // Test vector generated by scripts/gen_ratchet_test_vector.py using
        // Python's umsgpack.packb() and Ed25519PrivateKey.sign(), the exact
        // same code path as Destination._persist_ratchets().

        // Identity private key bytes: [x25519_prv(32) | ed25519_prv(32)]
        #[rustfmt::skip]
        let identity_prv: [u8; 64] = [
            // X25519 private key
            96, 156, 116, 115, 83, 108, 6, 122, 130, 102, 19, 240, 158, 159, 111, 48,
            143, 152, 128, 101, 110, 18, 55, 203, 191, 42, 184, 208, 141, 75, 215, 76,
            // Ed25519 signing private key
            190, 174, 16, 135, 56, 204, 86, 145, 169, 175, 200, 150, 128, 74, 169, 110,
            166, 94, 141, 91, 116, 6, 94, 141, 196, 208, 179, 117, 237, 118, 121, 46,
        ];

        // Signed ratchet file bytes (123 bytes), output of Python's
        // umsgpack.packb({"signature": sign(packed), "ratchets": packed})
        // where packed = umsgpack.packb([bytes(0x42 * 32)])
        #[rustfmt::skip]
        let python_file_bytes: [u8; 123] = [
            0x82, 0xa9, 0x73, 0x69, 0x67, 0x6e, 0x61, 0x74, 0x75, 0x72, 0x65,
            0xc4, 0x40,
            0x48, 0xe4, 0x54, 0x9d, 0x60, 0xcf, 0xea, 0xfd, 0xc3, 0x87, 0xa7,
            0x27, 0xa9, 0x7c, 0xdd, 0x0e, 0x88, 0x18, 0xde, 0x28, 0xd1, 0x21,
            0xd8, 0x80, 0x6d, 0x29, 0x15, 0xfc, 0x48, 0xff, 0x7f, 0x93, 0xfe,
            0xd2, 0x1b, 0x29, 0x7e, 0x75, 0xbf, 0x86, 0x10, 0x72, 0x15, 0xc4,
            0xc6, 0x2c, 0xb2, 0xc5, 0xb7, 0x69, 0x44, 0x00, 0xaa, 0x29, 0xed,
            0x90, 0x9c, 0xb1, 0x84, 0x70, 0xc4, 0x9b, 0x62, 0x08,
            0xa8, 0x72, 0x61, 0x74, 0x63, 0x68, 0x65, 0x74, 0x73,
            0xc4, 0x23,
            0x91, 0xc4, 0x20,
            0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42,
            0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42,
            0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42,
        ];

        // Expected ratchet public key (derived by Python from the 0x42-filled private key)
        #[rustfmt::skip]
        let expected_ratchet_pub: [u8; 32] = [
            19, 44, 68, 43, 224, 16, 251, 213, 126, 114, 96, 51, 40, 170, 118, 231,
            31, 204, 193, 80, 58, 174, 33, 147, 39, 209, 77, 156, 153, 147, 244, 114,
        ];

        // Reconstruct the identity from private key bytes
        let identity = Identity::from_private_key_bytes(&identity_prv).unwrap();

        // Load the Python-generated signed ratchet file
        let mut dest = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "testapp",
            &["pycompat"],
        )
        .unwrap();

        let loaded = dest.load_ratchets_signed(&python_file_bytes).unwrap();
        assert_eq!(loaded, 1);

        // Verify the loaded ratchet derives the same public key Python computed
        assert_eq!(dest.ratchets[0].public_key_bytes(), expected_ratchet_pub);
    }

    #[test]
    fn test_serialize_ratchets_signed_no_identity() {
        // PLAIN destination has no identity, serialize should return None
        let dest =
            Destination::new(None, Direction::In, DestinationType::Plain, "plain", &[]).unwrap();

        assert!(dest.serialize_ratchets_signed().is_none());
    }

    // GROUP shared-key crypto tests
    fn group_dest() -> Destination {
        // GROUP IN destinations auto-scope like Python, but for crypto the
        // identity is irrelevant. We attach one so the address is well-formed.
        let identity = Identity::generate(&mut OsRng);
        Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Group,
            "levgroup",
            &["interop"],
        )
        .unwrap()
    }

    #[test]
    fn test_group_create_key_size() {
        let mut dest = group_dest();
        assert!(dest.group_key().is_none());
        dest.create_group_key(&mut OsRng).unwrap();
        // Matches Python create_keys -> Token.generate_key() default (AES-256): 64 bytes.
        assert_eq!(dest.group_key().unwrap().len(), TOKEN_KEY_SIZE);
        assert_eq!(TOKEN_KEY_SIZE, 64);
    }

    #[test]
    fn test_group_get_and_load_key() {
        let mut src = group_dest();
        src.create_group_key(&mut OsRng).unwrap();
        let key = *src.group_key().unwrap();

        let mut dst = group_dest();
        dst.load_group_key(&key).unwrap();
        assert_eq!(dst.group_key().unwrap(), &key);
    }

    #[test]
    fn test_group_roundtrip() {
        let mut sender = group_dest();
        sender.create_group_key(&mut OsRng).unwrap();
        let key = *sender.group_key().unwrap();

        let mut receiver = group_dest();
        receiver.load_group_key(&key).unwrap();

        let plaintext = b"group broadcast message";
        let ciphertext = sender.encrypt(plaintext, None, &mut OsRng).unwrap();
        // Ciphertext must not contain the plaintext.
        assert!(!ciphertext.windows(plaintext.len()).any(|w| w == plaintext));

        let decrypted = receiver.decrypt(&ciphertext).unwrap();
        assert_eq!(&decrypted[..], plaintext);
    }

    #[test]
    fn test_group_empty_plaintext_roundtrip() {
        let mut dest = group_dest();
        dest.create_group_key(&mut OsRng).unwrap();
        let ct = dest.encrypt(b"", None, &mut OsRng).unwrap();
        let pt = dest.decrypt(&ct).unwrap();
        assert!(pt.is_empty());
    }

    #[test]
    fn test_group_wrong_key_fails() {
        let mut sender = group_dest();
        sender.create_group_key(&mut OsRng).unwrap();
        let ciphertext = sender.encrypt(b"secret", None, &mut OsRng).unwrap();

        let mut wrong = group_dest();
        wrong.create_group_key(&mut OsRng).unwrap();
        let result = wrong.decrypt(&ciphertext);
        assert_eq!(result, Err(DestinationError::DecryptionFailed));
    }

    #[test]
    fn test_group_encrypt_without_key_fails() {
        let dest = group_dest();
        let result = dest.encrypt(b"x", None, &mut OsRng);
        assert_eq!(result, Err(DestinationError::NoGroupKey));
    }

    #[test]
    fn test_group_key_ops_reject_non_group() {
        let identity = Identity::generate(&mut OsRng);
        let mut single = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "app",
            &["s"],
        )
        .unwrap();
        assert_eq!(
            single.create_group_key(&mut OsRng),
            Err(DestinationError::NotGroupType)
        );
        assert_eq!(
            single.load_group_key(&[0u8; TOKEN_KEY_SIZE]),
            Err(DestinationError::NotGroupType)
        );
    }

    #[test]
    fn test_group_load_key_wrong_length() {
        let mut dest = group_dest();
        assert_eq!(
            dest.load_group_key(&[0u8; 32]),
            Err(DestinationError::InvalidGroupKeyLength)
        );
    }

    /// Known-answer test pinning the GROUP wire format to Python-RNS.
    ///
    /// Source: vendored Python-RNS `RNS.Destination` GROUP branch
    /// (`dest.load_private_key(key)`, `dest.encrypt`/`dest.decrypt`), which
    /// routes through `RNS.Cryptography.Token` in `AES_256_CBC` mode. Captured
    /// with `os.urandom` patched to emit the fixed IV below so the encrypt is
    /// deterministic.
    ///
    /// Fixed 64-byte key (0x00..0x3f), fixed 16-byte IV (0x00..0x0f),
    /// plaintext b"Reticulum GROUP KAT" (19 bytes). Reference token is 80 bytes:
    /// IV(16) || AES-256-CBC ciphertext(32) || HMAC-SHA256(32).
    ///
    /// Asserts both directions:
    ///   - our decrypt(reference token) == plaintext, and
    ///   - our encrypt(key, iv, plaintext) == reference token, byte-for-byte.
    #[test]
    fn kat_group_python_rns_vector() {
        let key: [u8; 64] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
            0x1c, 0x1d, 0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29,
            0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37,
            0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e, 0x3f,
        ];
        let iv: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        // b"Reticulum GROUP KAT"
        let plaintext: [u8; 19] = [
            0x52, 0x65, 0x74, 0x69, 0x63, 0x75, 0x6c, 0x75, 0x6d, 0x20, 0x47, 0x52, 0x4f, 0x55,
            0x50, 0x20, 0x4b, 0x41, 0x54,
        ];
        // Reference token captured from Python-RNS GROUP Destination.encrypt.
        let reference_token: [u8; 80] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0x02, 0xd6, 0x54, 0x59, 0x85, 0x2c, 0xf1, 0xef, 0xe0, 0xed, 0xcf, 0xd8,
            0x31, 0xf6, 0x85, 0xa6, 0x10, 0x0d, 0x7a, 0x4c, 0x4c, 0x5c, 0x8c, 0x87, 0x77, 0x6a,
            0xee, 0xd6, 0x7c, 0x3b, 0x2e, 0x9a, 0x7d, 0xbe, 0xa6, 0x7c, 0x10, 0x09, 0xb9, 0xb6,
            0xbf, 0x76, 0x36, 0x1d, 0x9b, 0x4f, 0x6f, 0x3e, 0xc6, 0xf2, 0xcf, 0x21, 0x41, 0xa9,
            0xe9, 0x68, 0xa6, 0x21, 0xaa, 0x8f, 0xa1, 0xdc, 0xe6, 0xfa,
        ];

        // Our decrypt of the fixed Python-RNS GROUP token recovers the plaintext.
        let mut dest = group_dest();
        dest.load_group_key(&key).unwrap();
        let decrypted = dest.decrypt(&reference_token).unwrap();
        assert_eq!(
            &decrypted[..],
            &plaintext,
            "our GROUP decrypt of the Python-RNS token must recover the plaintext"
        );

        // Our encrypt with the same key + IV reproduces the exact token bytes.
        // (group_encrypt draws the IV from rng, so exercise the token layer with
        // the pinned IV directly, matching the same code path.)
        let padded_len = ((plaintext.len() / 16) + 1) * 16;
        let mut tok = vec![0u8; 16 + padded_len + 32];
        let n = crate::crypto::encrypt_token(&key, &iv, &plaintext, &mut tok).unwrap();
        tok.truncate(n);
        assert_eq!(
            &tok[..],
            &reference_token,
            "our GROUP token bytes must match the Python-RNS reference token"
        );
    }
}
