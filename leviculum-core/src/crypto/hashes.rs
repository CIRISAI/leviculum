//! Hash functions

use sha2::{Digest, Sha256, Sha512};

use crate::constants::TRUNCATED_HASHBYTES;

/// Compute SHA-256 hash
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Compute SHA-512 hash
pub fn sha512(data: &[u8]) -> [u8; 64] {
    let mut hasher = Sha512::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Compute full SHA-256 hash
/// Returns the complete 32-byte hash
pub fn full_hash(data: &[u8]) -> [u8; 32] {
    sha256(data)
}

/// [`full_hash`] over the concatenation of `parts`, without ever
/// materialising that concatenation.
///
/// SHA-256 is a streaming construction, so feeding the parts in order is
/// bit-for-bit the same digest as hashing one buffer holding them —
/// callers that only build a buffer to hash it once can drop the buffer.
/// That matters on a board: the resource constructor hashed a
/// response-sized copy twice for two digests it then threw away (#384).
pub fn full_hash_parts(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// [`full_hash_parts`] when the leading part is not a slice but a stream.
///
/// The resource advertisement needs two digests over the same payload —
/// `full_hash(data + random_hash)` and `full_hash(data + resource_hash)`
/// (`reference/Reticulum/RNS/Resource.py:441,443`) — and a payload read from a
/// store is expensive to read twice and impossible to hold. So the payload
/// is fed once and the two short tails are appended to a CLONE of the
/// state: SHA-256's state is the whole of what it remembers, so a clone
/// finalised with a different tail is exactly the digest of that
/// concatenation.
#[derive(Clone)]
pub struct StreamHasher(Sha256);

impl StreamHasher {
    pub fn new() -> Self {
        Self(Sha256::new())
    }

    /// Feed the next bytes of the payload.
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// The digest of everything fed so far followed by `tail`, leaving
    /// this hasher usable for another tail.
    pub fn finish_with(&self, tail: &[u8]) -> [u8; 32] {
        let mut hasher = self.0.clone();
        hasher.update(tail);
        hasher.finalize().into()
    }
}

impl Default for StreamHasher {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute truncated hash (first 16 bytes of SHA-256)
/// Used for destination addresses
pub fn truncated_hash(data: &[u8]) -> [u8; TRUNCATED_HASHBYTES] {
    let hash = sha256(data);
    let mut result = [0u8; TRUNCATED_HASHBYTES];
    result.copy_from_slice(&hash[..TRUNCATED_HASHBYTES]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A streamed payload with a tail is the digest of the concatenation,
    /// at every split of the payload — the property both resource digests
    /// rest on.
    #[test]
    fn a_stream_hasher_with_a_tail_is_full_hash_of_the_concatenation() {
        let payload: alloc::vec::Vec<u8> = (0..500u32).map(|i| (i * 13) as u8).collect();
        let tail = [9u8; 4];
        let mut joined = payload.clone();
        joined.extend_from_slice(&tail);
        for chunk in [1usize, 3, 64, 500, 4096] {
            let mut hasher = StreamHasher::new();
            for piece in payload.chunks(chunk) {
                hasher.update(piece);
            }
            assert_eq!(hasher.finish_with(&tail), full_hash(&joined));
            // Reusable: a second tail sees the same payload state.
            assert_eq!(hasher.finish_with(&tail), full_hash(&joined));
        }
    }

    #[test]
    fn test_sha256_empty() {
        let expected = [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55,
        ];
        assert_eq!(sha256(b""), expected);
    }

    #[test]
    fn test_sha256_hello() {
        let hash = sha256(b"hello");
        assert_eq!(hash.len(), 32);
    }

    #[test]
    fn test_truncated_hash() {
        let hash = truncated_hash(b"test data");
        assert_eq!(hash.len(), TRUNCATED_HASHBYTES);
    }
}
