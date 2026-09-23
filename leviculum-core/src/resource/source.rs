//! A byte source an outgoing resource can be built from without the
//! payload ever existing as one buffer (Codeberg #384, B2).
//!
//! `OutgoingResource` (`leviculum-core/src/resource/outgoing.rs`) takes an owned
//! `&[u8]` for every transfer the stack sends today, and that is the right
//! shape for `lncp`, for LXMF delivery and for `lnpnd`'s own uploads: the
//! bytes already exist, once, and the resource borrows them. It is the
//! wrong shape for a propagation node's mailbox fetch, where the "payload"
//! is a msgpack framing around records that are already durable in mapped
//! flash. Materialising it costs the board a full response copy before the
//! resource path has copied anything at all — and that copy, at the field's
//! 24-message mailbox, is what the serve cap was shrunk to nine messages to
//! avoid (`serve_peak_bytes`, `leviculum-lxmf/src/propagation_node.rs`).
//!
//! So: a `Read`-like trait, `no_std`, with a **known total length**. The
//! length has to be known before the first byte, because the resource
//! advertisement carries the transfer size and the receiver sizes its
//! reassembly from it; a source that discovers its own length at EOF
//! cannot be advertised.
//!
//! # Why it is read twice
//!
//! The advertisement carries `full_hash(data + random_hash)` over the WHOLE
//! payload (`reference/Reticulum/RNS/Resource.py:441`) and the parts are cut
//! from the *ciphertext*, which is a different byte stream of a different
//! length. One pass cannot produce both without holding one of them, so the
//! builder makes two: pass one hashes the payload, pass two encrypts it and
//! cuts the parts. [`rewind`](ResourceSource::rewind) is what makes the
//! second pass possible, and a source whose bytes changed in between is a
//! corrupt transfer, not a slow one — see the trait's contract.

use alloc::vec::Vec;

/// Why a source could not produce the bytes it promised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceError {
    /// The backing store could not be read (a flash read error, a record
    /// purged between passes).
    Unavailable,
    /// The source yielded fewer bytes than [`ResourceSource::total_len`]
    /// promised, or more.
    LengthChanged,
}

impl core::fmt::Display for SourceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unavailable => write!(f, "resource source unavailable"),
            Self::LengthChanged => write!(f, "resource source length changed"),
        }
    }
}

/// A payload an outgoing resource reads on demand instead of owning.
///
/// # Contract
///
/// * [`total_len`](Self::total_len) is fixed for the source's life and is
///   the exact number of bytes a full read yields.
/// * [`rewind`](Self::rewind) restarts at byte zero, and the bytes of the
///   second pass are the bytes of the first. A source that cannot promise
///   that must not be handed to a resource build: the hash is taken on
///   pass one and the parts on pass two, so a mid-build change ships a
///   resource whose parts do not hash to its advertisement, which the
///   receiver reports as a failed transfer after paying for all of it.
/// * [`read`](Self::read) fills as much of `buf` as it can and returns how
///   much; `Ok(0)` means end of stream and nothing else.
pub trait ResourceSource {
    /// Total bytes this source yields per pass.
    fn total_len(&self) -> usize;

    /// Restart the stream at byte zero.
    fn rewind(&mut self) -> Result<(), SourceError>;

    /// Fill the front of `buf`; return how many bytes were written, `0`
    /// only at end of stream.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, SourceError>;
}

/// The owned-buffer path as a source: the adapter that makes "a source and
/// a buffer of the same bytes produce byte-identical advertisements and
/// parts" a statement two code paths can be compared on
/// (`a_source_and_a_buffer_build_the_same_resource`,
/// `leviculum-core/src/resource/outgoing.rs`).
pub struct SliceSource<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> SliceSource<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
}

impl ResourceSource for SliceSource<'_> {
    fn total_len(&self) -> usize {
        self.bytes.len()
    }

    fn rewind(&mut self) -> Result<(), SourceError> {
        self.position = 0;
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, SourceError> {
        let take = core::cmp::min(buf.len(), self.bytes.len() - self.position);
        buf[..take].copy_from_slice(&self.bytes[self.position..self.position + take]);
        self.position += take;
        Ok(take)
    }
}

/// A source with a short owned prologue in front of it.
///
/// The request/response wrapper is exactly this shape: a response Resource
/// carries `fixarray(2) || bin(request_id) || response`, 19 bytes of frame
/// around a payload that may be thousands
/// (`send_response_resource`, `leviculum-core/src/node/mod.rs`). Framing it
/// by concatenation would rebuild the copy the source exists to avoid, so
/// the frame is a prefix the reader walks through first.
pub struct PrefixSource<'a> {
    prefix: Vec<u8>,
    inner: &'a mut dyn ResourceSource,
    position: usize,
}

impl<'a> PrefixSource<'a> {
    pub fn new(prefix: Vec<u8>, inner: &'a mut dyn ResourceSource) -> Self {
        Self {
            prefix,
            inner,
            position: 0,
        }
    }
}

impl ResourceSource for PrefixSource<'_> {
    fn total_len(&self) -> usize {
        self.prefix.len() + self.inner.total_len()
    }

    fn rewind(&mut self) -> Result<(), SourceError> {
        self.position = 0;
        self.inner.rewind()
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, SourceError> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.position < self.prefix.len() {
            let take = core::cmp::min(buf.len(), self.prefix.len() - self.position);
            buf[..take].copy_from_slice(&self.prefix[self.position..self.position + take]);
            self.position += take;
            return Ok(take);
        }
        self.inner.read(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Read a whole source with a given read size, twice, and assert both
    /// passes yield the promised bytes.
    fn drain(source: &mut dyn ResourceSource, chunk: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; chunk];
        loop {
            let n = source.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        out
    }

    #[test]
    fn a_slice_source_yields_its_slice_at_every_read_size() {
        let bytes: Vec<u8> = (0..300u32).map(|i| i as u8).collect();
        for chunk in [1usize, 7, 300, 4096] {
            let mut source = SliceSource::new(&bytes);
            assert_eq!(source.total_len(), bytes.len());
            assert_eq!(drain(&mut source, chunk), bytes);
            source.rewind().unwrap();
            assert_eq!(drain(&mut source, chunk), bytes, "rewind repeats the pass");
        }
    }

    #[test]
    fn a_prefix_source_is_the_concatenation() {
        let bytes: Vec<u8> = (0..70u32).map(|i| (i * 3) as u8).collect();
        let prefix = vec![0x92, 0xC4, 0x10];
        let mut expected = prefix.clone();
        expected.extend_from_slice(&bytes);
        for chunk in [1usize, 2, 3, 4, 64, 4096] {
            let mut inner = SliceSource::new(&bytes);
            let mut source = PrefixSource::new(prefix.clone(), &mut inner);
            assert_eq!(source.total_len(), expected.len());
            assert_eq!(drain(&mut source, chunk), expected);
            source.rewind().unwrap();
            assert_eq!(drain(&mut source, chunk), expected);
        }
    }
}
