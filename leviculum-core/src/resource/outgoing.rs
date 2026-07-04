//! Outgoing resource state machine (sender side).
//!
//! Sans-I/O: receives packets via methods, returns raw packet bytes to send.
//! Owned by Link (same pattern as Channel).

use alloc::vec;
use alloc::vec::Vec;

use rand_core::CryptoRngCore;

#[cfg(feature = "compression")]
use crate::constants::RESOURCE_AUTO_COMPRESS_MAX;
use crate::constants::{RESOURCE_HASHMAP_LEN, RESOURCE_WINDOW_MAX_FAST};
use crate::crypto::full_hash;
use crate::link::Link;
use crate::packet::PacketContext;
use crate::resource::hashmap::map_hash;
use crate::resource::msgpack;
use crate::resource::{
    resource_sdu, ResourceAdvertisement, ResourceError, ResourceFlags, ResourceStatus,
    COLLISION_GUARD_SIZE, HASHMAP_IS_EXHAUSTED, HASHMAP_MAX_LEN, PART_TIMEOUT_FACTOR_AFTER_RTT,
    PART_TIMEOUT_FACTOR_INITIAL, PER_RETRY_DELAY_MS, PROCESSING_GRACE_MS, PROOF_TIMEOUT_FACTOR,
    RESOURCE_MAX_ADV_RETRIES, RESOURCE_MAX_RETRIES, RESOURCE_RANDOM_HASH_SIZE,
    SENDER_GRACE_TIME_MS,
};

/// Result of polling an outgoing resource for timeout.
#[derive(Debug)]
pub(crate) enum ResourcePollResult {
    /// No action needed.
    Nothing,
    /// Re-send advertisement packet.
    RetransmitAdv(Vec<u8>),
    /// Send CacheRequest for the expected proof.
    /// Contains proof_data: [resource_hash:32][expected_proof:32].
    RequestProof { proof_data: Vec<u8> },
    /// Transfer has timed out, should be failed.
    TimedOut,
}

/// Outgoing resource transfer state machine.
///
/// Fields like `flags`, `original_hash`, `random_hash`, `uncompressed_size`,
/// `total_hashmap_segments`, `request_id`, and `sdu` are stored during resource
/// creation and needed for protocol correctness (advertisement reconstruction,
/// hash verification, hashmap segment calculation). Accessors are provided and
/// exercised in tests; production callers tracked in Codeberg issues #27/#28.
#[allow(dead_code)] // Protocol state fields — see Codeberg issues #27/#28
pub(crate) struct OutgoingResource {
    status: ResourceStatus,
    flags: ResourceFlags,
    resource_hash: [u8; 32],
    original_hash: [u8; 32],
    random_hash: [u8; RESOURCE_RANDOM_HASH_SIZE],
    encrypted_data: Vec<u8>,
    expected_proof: [u8; 32],
    uncompressed_size: u64,
    parts: Vec<Vec<u8>>,
    hashmap: Vec<[u8; RESOURCE_HASHMAP_LEN]>,
    num_parts: u32,
    sent_parts: usize,
    receiver_min_consecutive_height: usize,
    total_hashmap_segments: u32,
    window: usize,
    req_received: bool,
    retries: usize,
    adv_retries: usize,
    last_activity_ms: u64,
    request_id: Option<Vec<u8>>,
    adv_packet: Vec<u8>,
    link_mdu: usize,
    sdu: usize,
}

impl OutgoingResource {
    /// Create a new outgoing resource for transfer over a link.
    ///
    /// # Arguments
    /// * `data` - Application data to send
    /// * `metadata` - Optional metadata (msgpack-encoded by caller)
    /// * `request_id` - Optional request ID for request/response pairing
    /// * `link` - The link to send over (must be Active)
    /// * `rng` - Random number generator
    /// * `now_ms` - Current time in milliseconds
    pub(crate) fn new(
        data: &[u8],
        metadata: Option<&[u8]>,
        request_id: Option<&[u8]>,
        link: &Link,
        auto_compress: bool,
        rng: &mut impl CryptoRngCore,
        now_ms: u64,
    ) -> Result<Self, ResourceError> {
        Self::new_with_flags(
            data,
            metadata,
            request_id,
            link,
            auto_compress,
            rng,
            now_ms,
            false,
        )
    }

    /// Like [`new`](Self::new) but marks the advertisement as a response
    /// resource (`is_response` flag set, `request_id` carried in the ADV).
    ///
    /// Mirrors Python `RNS.Resource(..., is_response=True, request_id=...)`
    /// (`Link.py` response path): a request whose packed response exceeds the
    /// link MDU is delivered as a Resource the receiver correlates to its
    /// pending request. Used by the request/response responder for large
    /// `/status`-style bundles.
    pub(crate) fn new_response(
        data: &[u8],
        metadata: Option<&[u8]>,
        request_id: Option<&[u8]>,
        link: &Link,
        auto_compress: bool,
        rng: &mut impl CryptoRngCore,
        now_ms: u64,
    ) -> Result<Self, ResourceError> {
        Self::new_with_flags(
            data,
            metadata,
            request_id,
            link,
            auto_compress,
            rng,
            now_ms,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_with_flags(
        data: &[u8],
        metadata: Option<&[u8]>,
        request_id: Option<&[u8]>,
        link: &Link,
        auto_compress: bool,
        rng: &mut impl CryptoRngCore,
        now_ms: u64,
        is_response: bool,
    ) -> Result<Self, ResourceError> {
        if !link.is_active() {
            return Err(ResourceError::LinkNotActive);
        }

        let sdu = resource_sdu(link.negotiated_mtu());
        let link_mdu = link.mdu();

        // Build combined = metadata_prefix + data
        // Python line 264: struct.pack(">I", metadata_size)[1:] + packed_metadata
        let mut combined = Vec::new();
        let has_metadata = metadata.is_some();
        if let Some(meta) = metadata {
            let meta_len = meta.len();
            // 3-byte big-endian length (high 3 bytes of u32)
            combined.push((meta_len >> 16) as u8);
            combined.push((meta_len >> 8) as u8);
            combined.push(meta_len as u8);
            combined.extend_from_slice(meta);
        }
        combined.extend_from_slice(data);

        let uncompressed_size = combined.len() as u64;

        // Compute original_hash over unencrypted combined data
        let original_hash_full = full_hash(&combined);
        let mut original_hash = [0u8; 32];
        original_hash.copy_from_slice(&original_hash_full);

        // Try compression
        #[allow(unused_mut)]
        let mut compressed = false;
        let data_to_encrypt = {
            #[cfg(feature = "compression")]
            {
                if auto_compress && combined.len() <= RESOURCE_AUTO_COMPRESS_MAX {
                    match super::compression::bz2_compress(&combined) {
                        Ok(compressed_data) if compressed_data.len() < combined.len() => {
                            compressed = true;
                            compressed_data
                        }
                        _ => combined.clone(),
                    }
                } else {
                    combined.clone()
                }
            }
            #[cfg(not(feature = "compression"))]
            {
                let _ = auto_compress;
                combined.clone()
            }
        };

        // Generate wire random (prepended, not stored)
        let mut wire_random = [0u8; RESOURCE_RANDOM_HASH_SIZE];
        rng.fill_bytes(&mut wire_random);

        // Build plaintext: wire_random + data_to_encrypt
        let mut plaintext = Vec::with_capacity(RESOURCE_RANDOM_HASH_SIZE + data_to_encrypt.len());
        plaintext.extend_from_slice(&wire_random);
        plaintext.extend_from_slice(&data_to_encrypt);

        // Encrypt via link
        let enc_size = Link::encrypted_size(plaintext.len());
        let mut encrypted = vec![0u8; enc_size];
        let written = link
            .encrypt(&plaintext, &mut encrypted, rng)
            .map_err(|_| ResourceError::CryptoError)?;
        encrypted.truncate(written);

        // Generate verification random_hash (stored, sent in ADV "r" field)
        let mut random_hash = [0u8; RESOURCE_RANDOM_HASH_SIZE];
        rng.fill_bytes(&mut random_hash);

        // resource_hash = full_hash(combined + random_hash), uses UNENCRYPTED combined
        let mut hash_input = Vec::with_capacity(combined.len() + RESOURCE_RANDOM_HASH_SIZE);
        hash_input.extend_from_slice(&combined);
        hash_input.extend_from_slice(&random_hash);
        let resource_hash_full = full_hash(&hash_input);
        let mut resource_hash = [0u8; 32];
        resource_hash.copy_from_slice(&resource_hash_full);

        // expected_proof = full_hash(combined + resource_hash), precomputed
        let mut proof_input = Vec::with_capacity(combined.len() + 32);
        proof_input.extend_from_slice(&combined);
        proof_input.extend_from_slice(&resource_hash);
        let expected_proof_full = full_hash(&proof_input);
        let mut expected_proof = [0u8; 32];
        expected_proof.copy_from_slice(&expected_proof_full);

        // Segment encrypted data into parts
        let num_parts = if encrypted.is_empty() {
            1
        } else {
            encrypted.len().div_ceil(sdu) as u32
        };

        let guard_size = COLLISION_GUARD_SIZE;
        let hashmap_max = HASHMAP_MAX_LEN;

        // Build parts and hashmap, retrying if hash collisions occur
        let (parts, hashmap) = loop {
            let mut parts = Vec::with_capacity(num_parts as usize);
            let mut hashmap_entries = Vec::with_capacity(num_parts as usize);
            let mut collision_guard: Vec<[u8; RESOURCE_HASHMAP_LEN]> = Vec::new();
            let mut collision_found = false;

            for i in 0..num_parts as usize {
                let start = i * sdu;
                let end = core::cmp::min(start + sdu, encrypted.len());
                let part_data = &encrypted[start..end];

                let mh = map_hash(part_data, &random_hash);

                if collision_guard.contains(&mh) {
                    // Collision, regenerate random_hash and retry
                    rng.fill_bytes(&mut random_hash);

                    // Recompute resource_hash and expected_proof with new random_hash
                    let mut hi = Vec::with_capacity(combined.len() + RESOURCE_RANDOM_HASH_SIZE);
                    hi.extend_from_slice(&combined);
                    hi.extend_from_slice(&random_hash);
                    let rh = full_hash(&hi);
                    resource_hash.copy_from_slice(&rh);

                    let mut pi = Vec::with_capacity(combined.len() + 32);
                    pi.extend_from_slice(&combined);
                    pi.extend_from_slice(&resource_hash);
                    let ep = full_hash(&pi);
                    expected_proof.copy_from_slice(&ep);

                    collision_found = true;
                    break;
                }

                collision_guard.push(mh);
                if collision_guard.len() > guard_size {
                    collision_guard.remove(0);
                }

                hashmap_entries.push(mh);
                parts.push(part_data.to_vec());
            }

            if !collision_found {
                break (parts, hashmap_entries);
            }
        };

        // Calculate hashmap segments
        let total_hashmap_segments = if hashmap_max == 0 {
            1
        } else {
            hashmap.len().div_ceil(hashmap_max) as u32
        };

        // Build first hashmap segment for advertisement
        let first_segment_end = core::cmp::min(hashmap_max, hashmap.len());
        let mut hashmap_data = Vec::with_capacity(first_segment_end * RESOURCE_HASHMAP_LEN);
        for entry in &hashmap[..first_segment_end] {
            hashmap_data.extend_from_slice(entry);
        }

        // Build flags
        let flags = ResourceFlags {
            encrypted: true,
            compressed,
            split: false,
            is_request: false,
            is_response,
            has_metadata,
        };

        // Build and cache advertisement
        let adv = ResourceAdvertisement {
            transfer_size: encrypted.len() as u64,
            data_size: uncompressed_size,
            num_parts,
            resource_hash,
            random_hash,
            original_hash,
            segment_index: 1,
            total_segments: 1,
            request_id: request_id.map(|r| r.to_vec()),
            flags,
            hashmap_data,
        };
        let adv_packet = adv.pack();

        Ok(Self {
            status: ResourceStatus::Advertised,
            flags,
            resource_hash,
            original_hash,
            random_hash,
            encrypted_data: encrypted,
            expected_proof,
            uncompressed_size,
            parts,
            hashmap,
            num_parts,
            sent_parts: 0,
            receiver_min_consecutive_height: 0,
            total_hashmap_segments,
            window: crate::constants::RESOURCE_WINDOW_INITIAL,
            req_received: false,
            retries: 0,
            adv_retries: 0,
            last_activity_ms: now_ms,
            request_id: request_id.map(|r| r.to_vec()),
            adv_packet,
            link_mdu,
            sdu,
        })
    }

    /// Handle a REQ packet from the receiver.
    ///
    /// Returns raw data packets to send (RESOURCE context, pre-encrypted).
    /// May also return an HMU packet if the receiver's hashmap is exhausted.
    pub(crate) fn handle_request(
        &mut self,
        req_data: &[u8],
        link: &Link,
        rng: &mut impl CryptoRngCore,
        now_ms: u64,
    ) -> Result<Vec<Vec<u8>>, ResourceError> {
        if self.status == ResourceStatus::Failed {
            return Err(ResourceError::Cancelled);
        }

        // Parse REQ wire format: [1:exhausted_flag][4?:last_map_hash][32:resource_hash][N*4:requested_hashes]
        if req_data.is_empty() {
            return Err(ResourceError::InvalidRequest);
        }

        let wants_more_hashmap = req_data[0] == HASHMAP_IS_EXHAUSTED;
        let pad = if wants_more_hashmap {
            1 + RESOURCE_HASHMAP_LEN
        } else {
            1
        };

        // resource_hash starts at offset `pad`
        if req_data.len() < pad + 32 {
            return Err(ResourceError::InvalidRequest);
        }

        let req_resource_hash = &req_data[pad..pad + 32];
        if req_resource_hash != self.resource_hash {
            return Err(ResourceError::InvalidRequest);
        }

        // Transition to transferring on first REQ
        if self.status == ResourceStatus::Advertised {
            self.status = ResourceStatus::Transferring;
        }

        self.req_received = true;
        self.retries = 0;
        self.last_activity_ms = now_ms;

        let mut packets = Vec::new();

        // Parse requested map hashes
        let requested_hashes_data = &req_data[pad + 32..];
        let num_requested = requested_hashes_data.len() / RESOURCE_HASHMAP_LEN;

        let mut requested_hashes = Vec::with_capacity(num_requested);
        for i in 0..num_requested {
            let start = i * RESOURCE_HASHMAP_LEN;
            let mut mh = [0u8; RESOURCE_HASHMAP_LEN];
            mh.copy_from_slice(&requested_hashes_data[start..start + RESOURCE_HASHMAP_LEN]);
            requested_hashes.push(mh);
        }

        // Search within collision guard scope for matching parts
        let search_start = self.receiver_min_consecutive_height;
        let search_end = core::cmp::min(search_start + COLLISION_GUARD_SIZE, self.parts.len());

        for i in search_start..search_end {
            if requested_hashes.contains(&self.hashmap[i]) {
                // Build raw data packet (no per-packet encryption)
                let raw_pkt = link
                    .build_raw_data_packet(&self.parts[i], PacketContext::Resource)
                    .map_err(|_| ResourceError::LinkNotActive)?;
                packets.push(raw_pkt);
                self.sent_parts += 1;
            }
        }

        // Handle hashmap exhaustion, send next HMU segment
        if wants_more_hashmap {
            let last_map_hash = &req_data[1..1 + RESOURCE_HASHMAP_LEN];
            let hashmap_max = HASHMAP_MAX_LEN;

            // Find the part index matching last_map_hash
            let mut part_index = self.receiver_min_consecutive_height;
            let scan_end = core::cmp::min(
                self.receiver_min_consecutive_height + COLLISION_GUARD_SIZE,
                self.hashmap.len(),
            );
            for i in self.receiver_min_consecutive_height..scan_end {
                part_index = i + 1;
                if self.hashmap[i] == *last_map_hash {
                    break;
                }
            }

            // Update receiver_min_consecutive_height
            self.receiver_min_consecutive_height =
                part_index.saturating_sub(1 + RESOURCE_WINDOW_MAX_FAST);

            if let Some(segment) = part_index.checked_div(hashmap_max) {
                let hashmap_start = segment * hashmap_max;
                let hashmap_end = core::cmp::min((segment + 1) * hashmap_max, self.hashmap.len());

                // Build hashmap bytes for this segment
                let mut hashmap_bytes =
                    Vec::with_capacity((hashmap_end - hashmap_start) * RESOURCE_HASHMAP_LEN);
                for entry in &self.hashmap[hashmap_start..hashmap_end] {
                    hashmap_bytes.extend_from_slice(entry);
                }

                // Build HMU: resource_hash + msgpack([segment, hashmap_bytes])
                let mut hmu = Vec::with_capacity(32 + 10 + hashmap_bytes.len());
                hmu.extend_from_slice(&self.resource_hash);
                msgpack::write_fixarray_header(&mut hmu, 2);
                msgpack::write_uint(&mut hmu, segment as u64);
                msgpack::write_bin(&mut hmu, &hashmap_bytes);

                // Wrap in encrypted link packet
                let hmu_pkt = link
                    .build_data_packet_with_context(&hmu, PacketContext::ResourceHmu, rng)
                    .map_err(|_| ResourceError::LinkNotActive)?;
                packets.push(hmu_pkt);
            }
        }

        // Check if all parts sent → transition to AwaitingProof
        if self.sent_parts >= self.parts.len() {
            self.status = ResourceStatus::AwaitingProof;
            self.retries = 0;
        }

        Ok(packets)
    }

    /// Validate a proof from the receiver.
    ///
    /// Returns `Ok(ResourceStatus::Complete)` if valid.
    pub(crate) fn handle_proof(
        &mut self,
        proof_data: &[u8],
    ) -> Result<ResourceStatus, ResourceError> {
        if self.status == ResourceStatus::Failed {
            return Err(ResourceError::Cancelled);
        }

        // Proof format: [32: resource_hash] [32: proof_hash]
        if proof_data.len() != 64 {
            return Err(ResourceError::InvalidProof);
        }

        let proof_resource_hash = &proof_data[..32];
        let proof_hash = &proof_data[32..];

        if proof_resource_hash != self.resource_hash {
            return Err(ResourceError::InvalidProof);
        }

        if proof_hash != self.expected_proof {
            return Err(ResourceError::InvalidProof);
        }

        self.status = ResourceStatus::Complete;
        Ok(ResourceStatus::Complete)
    }

    /// Poll for timeout. Called periodically by the timeout handler.
    pub(crate) fn poll(&mut self, now_ms: u64, rtt_ms: u64) -> ResourcePollResult {
        let rtt_ms = core::cmp::max(rtt_ms, 1);

        match self.status {
            ResourceStatus::Advertised => {
                // Python Resource.py:571: timeout + PROCESSING_GRACE
                let timeout = rtt_ms.saturating_mul(6) + PROCESSING_GRACE_MS;
                if now_ms.saturating_sub(self.last_activity_ms) >= timeout {
                    self.adv_retries += 1;
                    if self.adv_retries < RESOURCE_MAX_ADV_RETRIES {
                        self.last_activity_ms = now_ms;
                        ResourcePollResult::RetransmitAdv(self.adv_packet.clone())
                    } else {
                        self.status = ResourceStatus::Failed;
                        ResourcePollResult::TimedOut
                    }
                } else {
                    ResourcePollResult::Nothing
                }
            }
            ResourceStatus::Transferring => {
                // Sender watchdog: wait for receiver's REQ. The receiver drives
                // retransmission, so the sender should be patient.
                // Python sender uses global budget (Resource.py:627-633).
                let timeout_factor = if self.req_received {
                    PART_TIMEOUT_FACTOR_AFTER_RTT // 2: link characteristics known
                } else {
                    PART_TIMEOUT_FACTOR_INITIAL // 4: initial, generous
                };
                let per_retry_extra = self.retries as u64 * PER_RETRY_DELAY_MS;
                let timeout =
                    rtt_ms.saturating_mul(timeout_factor) + SENDER_GRACE_TIME_MS + per_retry_extra;

                if now_ms.saturating_sub(self.last_activity_ms) >= timeout {
                    self.retries += 1;
                    self.last_activity_ms = now_ms;
                    if self.retries >= RESOURCE_MAX_RETRIES {
                        self.status = ResourceStatus::Failed;
                        ResourcePollResult::TimedOut
                    } else {
                        // Just wait for another REQ
                        ResourcePollResult::Nothing
                    }
                } else {
                    ResourcePollResult::Nothing
                }
            }
            ResourceStatus::AwaitingProof => {
                // Python Resource.py:638-640: PROOF_TIMEOUT_FACTOR * RTT + SENDER_GRACE_TIME
                let per_retry_extra = self.retries as u64 * PER_RETRY_DELAY_MS;
                let timeout = rtt_ms.saturating_mul(PROOF_TIMEOUT_FACTOR)
                    + SENDER_GRACE_TIME_MS
                    + per_retry_extra;
                if now_ms.saturating_sub(self.last_activity_ms) >= timeout {
                    self.retries += 1;
                    self.last_activity_ms = now_ms;
                    if self.retries >= RESOURCE_MAX_RETRIES {
                        self.status = ResourceStatus::Failed;
                        ResourcePollResult::TimedOut
                    } else {
                        // Send CacheRequest so receiver re-sends the proof
                        let mut proof_data = Vec::with_capacity(64);
                        proof_data.extend_from_slice(&self.resource_hash);
                        proof_data.extend_from_slice(&self.expected_proof);
                        ResourcePollResult::RequestProof { proof_data }
                    }
                } else {
                    ResourcePollResult::Nothing
                }
            }
            _ => ResourcePollResult::Nothing,
        }
    }

    /// Compute the next deadline (absolute ms) for this resource.
    pub(crate) fn next_deadline(&self, rtt_ms: u64) -> Option<u64> {
        let rtt_ms = core::cmp::max(rtt_ms, 1);
        match self.status {
            ResourceStatus::Advertised => Some(
                self.last_activity_ms
                    .saturating_add(rtt_ms.saturating_mul(6) + PROCESSING_GRACE_MS),
            ),
            ResourceStatus::Transferring => {
                let timeout_factor = if self.req_received {
                    PART_TIMEOUT_FACTOR_AFTER_RTT
                } else {
                    PART_TIMEOUT_FACTOR_INITIAL
                };
                let per_retry_extra = self.retries as u64 * PER_RETRY_DELAY_MS;
                let timeout =
                    rtt_ms.saturating_mul(timeout_factor) + SENDER_GRACE_TIME_MS + per_retry_extra;
                Some(self.last_activity_ms.saturating_add(timeout))
            }
            ResourceStatus::AwaitingProof => {
                let per_retry_extra = self.retries as u64 * PER_RETRY_DELAY_MS;
                let timeout = rtt_ms.saturating_mul(PROOF_TIMEOUT_FACTOR)
                    + SENDER_GRACE_TIME_MS
                    + per_retry_extra;
                Some(self.last_activity_ms.saturating_add(timeout))
            }
            _ => None,
        }
    }

    /// Mark this resource as failed/cancelled.
    #[allow(dead_code)] // Resource cancel API — see Codeberg issues #27/#28
    pub(crate) fn cancel(&mut self) {
        self.status = ResourceStatus::Failed;
    }

    // Accessors
    pub(crate) fn status(&self) -> ResourceStatus {
        self.status
    }

    pub(crate) fn resource_hash(&self) -> &[u8; 32] {
        &self.resource_hash
    }

    pub(crate) fn adv_packet(&self) -> &[u8] {
        &self.adv_packet
    }

    pub(crate) fn progress(&self) -> f32 {
        if self.num_parts == 0 {
            return 1.0;
        }
        self.sent_parts as f32 / self.num_parts as f32
    }

    pub(crate) fn transfer_size(&self) -> u64 {
        self.encrypted_data.len() as u64
    }

    #[allow(dead_code)] // Resource accessor API — see Codeberg issues #27/#28
    pub(crate) fn flags(&self) -> &ResourceFlags {
        &self.flags
    }

    #[allow(dead_code)] // Resource accessor API — see Codeberg issues #27/#28
    pub(crate) fn original_hash(&self) -> &[u8; 32] {
        &self.original_hash
    }

    #[allow(dead_code)] // Resource accessor API — see Codeberg issues #27/#28
    pub(crate) fn random_hash(&self) -> &[u8; RESOURCE_RANDOM_HASH_SIZE] {
        &self.random_hash
    }

    pub(crate) fn uncompressed_size(&self) -> u64 {
        self.uncompressed_size
    }

    #[allow(dead_code)] // Resource accessor API — see Codeberg issues #27/#28
    pub(crate) fn total_hashmap_segments(&self) -> u32 {
        self.total_hashmap_segments
    }

    #[allow(dead_code)] // Resource accessor API — see Codeberg issues #27/#28
    pub(crate) fn request_id(&self) -> Option<&[u8]> {
        self.request_id.as_deref()
    }

    #[allow(dead_code)] // Resource accessor API — see Codeberg issues #27/#28
    pub(crate) fn sdu(&self) -> usize {
        self.sdu
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::ResourceAdvertisement;

    /// Helper to create a test link with known keys.
    /// We use Link's test infrastructure.
    fn make_test_link() -> (Link, Link) {
        use rand_core::OsRng;
        let dest_hash = crate::destination::DestinationHash::new([0xAA; 16]);
        let mut initiator = Link::new_outgoing(dest_hash, &mut OsRng);

        // Create responder and complete handshake for testing
        let dest_hash2 = crate::destination::DestinationHash::new([0xBB; 16]);
        let _responder = Link::new_outgoing(dest_hash2, &mut OsRng);

        // We need an active link for encryption, use the test helper
        // that sets link_key directly.
        let link_key = [0x42u8; 64];
        initiator.set_link_key_for_test(link_key);

        let mut resp = Link::new_outgoing(dest_hash, &mut OsRng);
        resp.set_link_key_for_test(link_key);

        // Both need to be Active
        initiator.set_state(crate::link::LinkState::Active);
        resp.set_state(crate::link::LinkState::Active);

        (initiator, resp)
    }

    #[test]
    fn test_outgoing_resource_creation() {
        let (link, _) = make_test_link();
        let mut rng = rand_core::OsRng;
        let data = b"Hello, Resource!";

        let res = OutgoingResource::new(data, None, None, &link, true, &mut rng, 1000).unwrap();

        assert_eq!(res.status(), ResourceStatus::Advertised);
        assert_eq!(res.num_parts, 1); // small data = 1 part
        assert!(!res.adv_packet().is_empty());
    }

    #[test]
    fn test_outgoing_resource_adv_roundtrip() {
        let (link, _) = make_test_link();
        let mut rng = rand_core::OsRng;
        let data = b"Test data for advertisement roundtrip";

        let res = OutgoingResource::new(data, None, None, &link, true, &mut rng, 1000).unwrap();

        // Verify the cached ADV unpacks correctly
        let adv = ResourceAdvertisement::unpack(res.adv_packet()).unwrap();
        assert_eq!(adv.resource_hash, *res.resource_hash());
        assert_eq!(adv.num_parts, res.num_parts);
        assert!(adv.flags.encrypted);
        assert_eq!(adv.segment_index, 1);
        assert_eq!(adv.total_segments, 1);
        assert!(adv.request_id.is_none());
    }

    #[test]
    fn test_outgoing_resource_with_metadata() {
        let (link, _) = make_test_link();
        let mut rng = rand_core::OsRng;
        let data = b"data with metadata";
        let metadata = b"some metadata";

        let res =
            OutgoingResource::new(data, Some(metadata), None, &link, true, &mut rng, 1000).unwrap();

        let adv = ResourceAdvertisement::unpack(res.adv_packet()).unwrap();
        assert!(adv.flags.has_metadata);
    }

    #[test]
    fn test_outgoing_resource_proof_validation() {
        let (link, _) = make_test_link();
        let mut rng = rand_core::OsRng;
        let data = b"proof test data";

        let mut res = OutgoingResource::new(data, None, None, &link, true, &mut rng, 1000).unwrap();

        // Forge a valid proof: resource_hash + expected_proof
        let mut valid_proof = Vec::new();
        valid_proof.extend_from_slice(&res.resource_hash);
        valid_proof.extend_from_slice(&res.expected_proof);

        let result = res.handle_proof(&valid_proof).unwrap();
        assert_eq!(result, ResourceStatus::Complete);
        assert_eq!(res.status(), ResourceStatus::Complete);
    }

    #[test]
    fn test_outgoing_resource_invalid_proof() {
        let (link, _) = make_test_link();
        let mut rng = rand_core::OsRng;
        let data = b"proof test data";

        let mut res = OutgoingResource::new(data, None, None, &link, true, &mut rng, 1000).unwrap();

        // Invalid proof
        let bad_proof = [0u8; 64];
        assert_eq!(
            res.handle_proof(&bad_proof),
            Err(ResourceError::InvalidProof)
        );
    }

    #[test]
    fn test_outgoing_resource_poll_adv_timeout() {
        let (link, _) = make_test_link();
        let mut rng = rand_core::OsRng;
        let data = b"timeout test";

        let mut res = OutgoingResource::new(data, None, None, &link, true, &mut rng, 1000).unwrap();

        // Not timed out yet
        let result = res.poll(1000, 100);
        assert!(matches!(result, ResourcePollResult::Nothing));

        // Timed out (rtt_ms * 6 + PROCESSING_GRACE_MS = 600 + 1000 = 1600ms)
        let result = res.poll(2601, 100);
        assert!(matches!(result, ResourcePollResult::RetransmitAdv(_)));

        // After max retries, should be TimedOut
        for _ in 0..RESOURCE_MAX_ADV_RETRIES {
            res.poll(res.last_activity_ms + 1601, 100);
        }
        assert_eq!(res.status(), ResourceStatus::Failed);
    }

    #[test]
    fn test_transferring_retries_spaced_by_timeout() {
        let (link, _) = make_test_link();
        let mut rng = rand_core::OsRng;
        // Large enough for multiple parts. With the `compression` feature
        // active (workspace builds enable it via leviculum-std), repeated
        // bytes would compress below MTU and yield a single part. Use
        // pseudo-random bytes so part count is independent of the feature.
        use rand_core::{OsRng, RngCore};
        let mut data = vec![0u8; 2000];
        OsRng.fill_bytes(&mut data);

        let mut res =
            OutgoingResource::new(&data, None, None, &link, true, &mut rng, 1000).unwrap();
        assert!(res.parts.len() >= 2, "need multi-part resource");

        // Build a partial REQ requesting only the first part to keep status=Transferring.
        // Format: [0x00][resource_hash:32][hashmap_entry_0:4]
        let mut req = Vec::new();
        req.push(0x00); // not exhausted
        req.extend_from_slice(&res.resource_hash);
        req.extend_from_slice(&res.hashmap[0]);
        let _ = res.handle_request(&req, &link, &mut rng, 2000);
        assert_eq!(res.status(), ResourceStatus::Transferring);

        // Use rtt_ms = 1000. After handle_request, req_received=true, so
        // timeout_factor = PART_TIMEOUT_FACTOR_AFTER_RTT (2).
        // Sender timeout = rtt * 2 + SENDER_GRACE_TIME_MS(10000) + retries*500
        // = 2000 + 10000 = 12000ms (0 retries)
        let rtt_ms = 1000;

        // First poll just after the REQ, should NOT time out.
        let result = res.poll(2500, rtt_ms);
        assert!(matches!(result, ResourcePollResult::Nothing));

        // Fire first timeout (12000ms after last activity at t=2000 → t=14000).
        let result = res.poll(14001, rtt_ms);
        assert!(matches!(result, ResourcePollResult::Nothing)); // retry incremented, returns Nothing
        assert_eq!(res.retries, 1);

        // Immediately polling again should NOT fire another retry because
        // last_activity_ms was reset.
        let result = res.poll(14002, rtt_ms);
        assert!(matches!(result, ResourcePollResult::Nothing));
        assert_eq!(
            res.retries, 1,
            "retry must not increment without waiting full timeout"
        );

        // After another full timeout period (now 12500ms = 12000 + 500 backoff), retry fires.
        let result = res.poll(14001 + 12501, rtt_ms);
        assert!(matches!(result, ResourcePollResult::Nothing));
        assert_eq!(res.retries, 2);

        // Verify we don't immediately hit max retries (16) from rapid polling.
        for _ in 0..20 {
            res.poll(14001 + 12501 + 1, rtt_ms);
        }
        assert!(
            res.retries < RESOURCE_MAX_RETRIES,
            "retries should not exhaust from rapid polling: got {}",
            res.retries
        );
        assert_eq!(res.status(), ResourceStatus::Transferring);
    }

    #[test]
    fn test_awaiting_proof_retries() {
        let (link, _) = make_test_link();
        let mut rng = rand_core::OsRng;
        // Small data, fits in few parts so all get sent in one REQ
        let data = vec![0x42u8; 200];

        let mut res =
            OutgoingResource::new(&data, None, None, &link, true, &mut rng, 1000).unwrap();

        // Build full REQ requesting all parts
        let mut req = Vec::new();
        req.push(0x00); // not exhausted
        req.extend_from_slice(&res.resource_hash);
        for h in &res.hashmap {
            req.extend_from_slice(h);
        }
        let _ = res.handle_request(&req, &link, &mut rng, 2000);
        // All parts sent → should transition to AwaitingProof
        assert_eq!(res.status(), ResourceStatus::AwaitingProof);

        let rtt_ms = 1000;
        // AwaitingProof timeout = PROOF_TIMEOUT_FACTOR * rtt + SENDER_GRACE_TIME_MS + retries*500
        // = 3 * 1000 + 10000 + 0 = 13000ms (0 retries)

        // Not timed out yet
        let result = res.poll(14999, rtt_ms);
        assert!(matches!(result, ResourcePollResult::Nothing));

        // First timeout fires at 2000 + 13000 = 15000, sends CacheRequest
        let result = res.poll(15001, rtt_ms);
        assert!(matches!(result, ResourcePollResult::RequestProof { .. }));
        assert_eq!(res.retries, 1);
        assert_eq!(res.status(), ResourceStatus::AwaitingProof);

        // Second timeout at 15001 + 13500 (13000 + 500 backoff) = 28501
        let result = res.poll(28502, rtt_ms);
        assert!(matches!(result, ResourcePollResult::RequestProof { .. }));
        assert_eq!(res.retries, 2);
        assert_eq!(res.status(), ResourceStatus::AwaitingProof);

        // Rapid polling should not exhaust retries
        for _ in 0..20 {
            res.poll(28503, rtt_ms);
        }
        assert!(
            res.retries < RESOURCE_MAX_RETRIES,
            "retries should not exhaust from rapid polling: got {}",
            res.retries
        );
        assert_eq!(res.status(), ResourceStatus::AwaitingProof);

        // Exhaust all retries
        for _ in res.retries..RESOURCE_MAX_RETRIES {
            // Each retry adds 500ms more: timeout grows with retries
            let timeout = 13000 + res.retries as u64 * PER_RETRY_DELAY_MS;
            let t = res.last_activity_ms + timeout + 1;
            res.poll(t, rtt_ms);
        }
        assert_eq!(res.status(), ResourceStatus::Failed);
    }

    #[test]
    fn test_outgoing_resource_hashmap_integrity() {
        let (link, _) = make_test_link();
        let mut rng = rand_core::OsRng;
        // Create data large enough for multiple parts
        let data = vec![0x42u8; 2000];

        let res = OutgoingResource::new(&data, None, None, &link, true, &mut rng, 1000).unwrap();

        // Verify each part's map_hash matches
        for (i, part) in res.parts.iter().enumerate() {
            let expected = map_hash(part, &res.random_hash);
            assert_eq!(res.hashmap[i], expected, "hashmap mismatch at part {i}");
        }
    }

    #[test]
    fn test_outgoing_resource_accessors() {
        let (link, _) = make_test_link();
        let mut rng = rand_core::OsRng;
        let data = b"accessor test data";
        let request_id = b"req-42";

        let mut res =
            OutgoingResource::new(data, None, Some(request_id), &link, true, &mut rng, 1000)
                .unwrap();

        // Protocol state fields accessible via accessors
        assert!(res.flags().encrypted);
        assert!(!res.flags().compressed);
        assert!(res.original_hash() != &[0u8; 32]);
        assert!(res.random_hash() != &[0u8; RESOURCE_RANDOM_HASH_SIZE]);
        assert!(res.uncompressed_size() > 0);
        assert!(res.total_hashmap_segments() >= 1);
        assert_eq!(res.request_id(), Some(request_id.as_slice()));
        assert!(res.sdu() > 0);

        // cancel() transitions to Failed
        res.cancel();
        assert_eq!(res.status(), ResourceStatus::Failed);
    }

    #[test]
    fn test_awaiting_proof_returns_request_proof_with_correct_data() {
        let (link, _) = make_test_link();
        let mut rng = rand_core::OsRng;
        let data = vec![0x42u8; 200];

        let mut res =
            OutgoingResource::new(&data, None, None, &link, true, &mut rng, 1000).unwrap();

        // Build full REQ requesting all parts → transition to AwaitingProof
        let mut req = Vec::new();
        req.push(0x00);
        req.extend_from_slice(&res.resource_hash);
        for h in &res.hashmap {
            req.extend_from_slice(h);
        }
        let _ = res.handle_request(&req, &link, &mut rng, 2000);
        assert_eq!(res.status(), ResourceStatus::AwaitingProof);

        let expected_resource_hash = res.resource_hash;
        let expected_proof = res.expected_proof;

        // Trigger first timeout → should return RequestProof
        let rtt_ms = 1000;
        let timeout = rtt_ms * PROOF_TIMEOUT_FACTOR + SENDER_GRACE_TIME_MS;
        let result = res.poll(2000 + timeout + 1, rtt_ms);

        match result {
            ResourcePollResult::RequestProof { proof_data } => {
                assert_eq!(proof_data.len(), 64, "proof_data must be 64 bytes");
                assert_eq!(
                    &proof_data[..32],
                    &expected_resource_hash,
                    "first 32 bytes must be resource_hash"
                );
                assert_eq!(
                    &proof_data[32..],
                    &expected_proof,
                    "last 32 bytes must be expected_proof"
                );
            }
            other => panic!("expected RequestProof, got {other:?}"),
        }
    }
}
