//! The map store: immutable, content-addressed pieces plus one signed
//! index (`docs/src/concepts/map-server.md`).
//!
//! A vectors pack is one region file (`leviculum-map`'s LMR format, version 4 or
//! newer: the tile index first, the tiles coarse to fine) split at zoom
//! boundaries into byte-range pieces. The pieces concatenate back to the
//! exact region file, and every prefix of pieces is a region file still
//! arriving, which the version 4 reader reads in place — so "coarse
//! before fine" is nothing but fetching the pieces in order and stopping
//! when the budget is spent. Each piece is named by the SHA-256 of its
//! bytes; the index lists the names, so any carrier may hold a piece and
//! the device still knows whether it is the server's.
//!
//! An elevation pack (1740) is a `leviculum_map::elevation` height file:
//! its directory piece, then one piece per block, every block standing
//! alone (see [`KIND_ELEVATION`]).
//!
//! The index is signed by the server's Reticulum identity — the same
//! identity that owns the `lev.maps` destination a device pins as
//! `map_destination`. The trailer carries the identity's public keys, so
//! a verifier recomputes the `lev.maps` destination hash from them and
//! compares it with the pin: one setting authenticates both transports,
//! HTTP included, without an announce in hand.
//!
//! On disk:
//!
//! ```text
//! <store>/index                  the signed index, replaced atomically
//! <store>/pieces/<aa>/<64 hex>   one piece, immutable, named by its hash
//! ```
//!
//! Index format, all integers little-endian:
//!
//! ```text
//! body
//!   header, 48 bytes
//!     0  "LMX1"
//!     4  u16 version (1)        6  u16 header length (48)
//!     8  u64 generation (strictly grows; a verifier refuses a rollback)
//!    16  u64 created, unix seconds
//!    24  u16 http url length    26 u16 pack count    28 u32 0
//!    32  u64 body length (everything the signature covers)
//!    40  u64 0
//!   http url  UTF-8, the server's own HTTP base (may be empty)
//!   packs, ascending by id, each:
//!     fixed, 104 bytes
//!      0  u16 id length          2  u16 kind length
//!      4  u8 min zoom            5  u8 max zoom
//!      6  u16 piece count
//!      8  i32 west, south, east, north, degrees x 10^7
//!     24  u64 total bytes (the assembled file's length)
//!     32  [32] whole-file sha256
//!     64  [32] attribution piece sha256
//!     96  u32 attribution piece length    100 u32 0
//!     id bytes, kind bytes  UTF-8
//!     pieces, in assembly order, 48 bytes each:
//!      0  [32] sha256   32 u64 offset   40 u32 length
//!     44  u8 zoom (the deepest zoom complete once this piece is there)
//!     45  [3] 0
//! trailer, 128 bytes
//!     0  [64] the identity's public keys (x25519 then ed25519)
//!    64  [64] ed25519 signature over the body
//! ```

use std::path::{Path, PathBuf};

use leviculum_core::crypto::sha256;
use leviculum_core::{Destination, DestinationHash, Identity};
use leviculum_map::region::{Region, ENTRY_LEN};

/// The index file's first four bytes.
pub const INDEX_MAGIC: [u8; 4] = *b"LMX1";
/// The index format version this writer writes and this reader reads.
pub const INDEX_VERSION: u16 = 1;
/// The index file's name under the store directory.
pub const INDEX_FILE: &str = "index";
/// The pieces directory under the store directory.
pub const PIECES_DIR: &str = "pieces";
/// The server destination's app name (the `lev` app, as the position
/// beacon uses it).
pub const APP_NAME: &str = "lev";
/// The server destination's aspect: the destination is `lev.maps`.
pub const ASPECT: &str = "maps";
/// The kind string of vector region packs.
pub const KIND_VECTORS: &str = "vectors";
/// The kind string of height packs (1740): a `leviculum_map::elevation`
/// file, its directory piece first, then one piece per block.
pub const KIND_ELEVATION: &str = "elevation";

const HEADER_LEN: usize = 48;
const PACK_FIXED_LEN: usize = 104;
const PIECE_LEN: usize = 48;
const TRAILER_LEN: usize = 128;
/// The oldest region format whose pieces a prefix reader can use: from
/// version 4 the index is first and the tiles are coarse to fine.
const REGION_VERSION_FLOOR: u16 = 4;

/// Why a store operation failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Reading or writing the store failed.
    #[error("store io at {path}: {source}")]
    Io {
        /// What was being read or written.
        path: String,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The region file cannot be split into pieces.
    #[error("region: {0}")]
    Region(String),
    /// The existing index did not verify; it is not silently replaced.
    #[error("existing index: {0}")]
    Existing(#[from] VerifyError),
    /// A field does not fit its encoding (an id over 65535 bytes, more
    /// pieces than a u16 counts, ...).
    #[error("index field out of range: {0}")]
    Field(&'static str),
    /// The signing identity has no private key.
    #[error("identity cannot sign: {0}")]
    Identity(String),
    /// A pack of another kind already holds the id; a device that pinned
    /// it by id would get data of a kind it did not ask for. Only
    /// [`AddOptions::replace`] lets the new kind take its place.
    #[error("pack {id} is already a {existing} pack; refusing to replace it with a {new} pack (lnmapcut --replace)")]
    KindMismatch {
        /// The pack id.
        id: String,
        /// The kind the index holds under the id.
        existing: String,
        /// The kind that was to be added.
        new: String,
    },
}

/// Why index or piece verification refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// The bytes are not an index this reader reads.
    #[error("index format: {0}")]
    Format(&'static str),
    /// The embedded public keys are not a valid identity.
    #[error("index key is not a valid identity")]
    Key,
    /// The key's `lev.maps` destination is not the pinned one.
    #[error("index is signed by {got}, the pin is {pinned}")]
    NotThePinnedServer {
        /// The destination the embedded key yields, hex.
        got: String,
        /// The pinned destination, hex.
        pinned: String,
    },
    /// The signature does not verify.
    #[error("index signature does not verify")]
    Signature,
    /// The index is older than one already seen.
    #[error("index generation {got} is behind the {floor} already seen")]
    Rollback {
        /// The offered index's generation.
        got: u64,
        /// The generation already seen.
        floor: u64,
    },
    /// A piece's bytes do not hash to the name the index gave.
    #[error("piece {id} is tampered: {reason}")]
    Tampered {
        /// The piece id (hex) the index named.
        id: String,
        /// What did not match.
        reason: &'static str,
    },
}

/// One piece of a pack: a byte range of the assembled region file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    /// SHA-256 of the piece's bytes; its name everywhere.
    pub hash: [u8; 32],
    /// Where the piece starts in the assembled file.
    pub offset: u64,
    /// The piece's length in bytes.
    pub len: u32,
    /// The deepest zoom whose tiles are all there once the assembly
    /// reaches the end of this piece.
    pub zoom: u8,
}

impl Piece {
    /// The piece's id: its hash as lowercase hex, the name used in both
    /// request protocols and on disk.
    pub fn id(&self) -> String {
        hex::encode(self.hash)
    }
}

/// One pack: a region file cut into pieces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pack {
    /// The pack's id, e.g. `berlin` (what a catalogue shows).
    pub id: String,
    /// The data kind: [`KIND_VECTORS`] or [`KIND_ELEVATION`]; shading and
    /// pois get their own kinds without a format change.
    pub kind: String,
    /// The region's zoom range.
    pub min_zoom: u8,
    /// See [`Pack::min_zoom`].
    pub max_zoom: u8,
    /// The region's bounds, degrees x 10^7, west/south/east/north.
    pub bounds: [i32; 4],
    /// The assembled file's length.
    pub total: u64,
    /// SHA-256 of the whole assembled file.
    pub whole: [u8; 32],
    /// The attribution piece's hash: a small UTF-8 text piece carrying
    /// the pack's attribution line and licence notice.
    pub attribution: [u8; 32],
    /// The attribution piece's length.
    pub attribution_len: u32,
    /// The pieces, in assembly order (coarse to fine).
    pub pieces: Vec<Piece>,
}

/// The decoded index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Index {
    /// Strictly grows with every index the server writes.
    pub generation: u64,
    /// When the index was written, unix seconds.
    pub created: u64,
    /// The server's own HTTP base URL (`map_source` overrides it on a
    /// device); may be empty when the server has no HTTP side.
    pub http_url: String,
    /// The packs, ascending by id.
    pub packs: Vec<Pack>,
    /// The signer's public identity keys (x25519 then ed25519).
    pub key: [u8; 64],
}

fn u16le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32le(b: &[u8], at: usize) -> u32 {
    let mut v = [0u8; 4];
    v.copy_from_slice(&b[at..at + 4]);
    u32::from_le_bytes(v)
}

fn u64le(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(v)
}

fn i32le(b: &[u8], at: usize) -> i32 {
    let mut v = [0u8; 4];
    v.copy_from_slice(&b[at..at + 4]);
    i32::from_le_bytes(v)
}

impl Index {
    /// The `lev.maps` destination hash of the identity whose public keys
    /// these are: what a verifier compares with the pinned
    /// `map_destination`.
    pub fn destination_of(key: &[u8; 64]) -> Result<DestinationHash, VerifyError> {
        let identity = Identity::from_public_key_bytes(key).map_err(|_| VerifyError::Key)?;
        let name_hash = Destination::compute_name_hash(APP_NAME, &[ASPECT]);
        Ok(Destination::compute_destination_hash(
            &name_hash,
            identity.hash(),
        ))
    }

    /// Encode the body (everything the signature covers).
    fn encode_body(&self) -> Result<Vec<u8>, StoreError> {
        let url = self.http_url.as_bytes();
        let url_len = u16::try_from(url.len()).map_err(|_| StoreError::Field("http url"))?;
        let pack_count =
            u16::try_from(self.packs.len()).map_err(|_| StoreError::Field("pack count"))?;
        let mut body_len = HEADER_LEN + url.len();
        for pack in &self.packs {
            body_len +=
                PACK_FIXED_LEN + pack.id.len() + pack.kind.len() + pack.pieces.len() * PIECE_LEN;
        }
        let mut b = Vec::with_capacity(body_len);
        b.extend_from_slice(&INDEX_MAGIC);
        b.extend_from_slice(&INDEX_VERSION.to_le_bytes());
        b.extend_from_slice(&(HEADER_LEN as u16).to_le_bytes());
        b.extend_from_slice(&self.generation.to_le_bytes());
        b.extend_from_slice(&self.created.to_le_bytes());
        b.extend_from_slice(&url_len.to_le_bytes());
        b.extend_from_slice(&pack_count.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&(body_len as u64).to_le_bytes());
        b.extend_from_slice(&0u64.to_le_bytes());
        b.extend_from_slice(url);
        for pack in &self.packs {
            let id_len = u16::try_from(pack.id.len()).map_err(|_| StoreError::Field("pack id"))?;
            let kind_len =
                u16::try_from(pack.kind.len()).map_err(|_| StoreError::Field("pack kind"))?;
            let piece_count =
                u16::try_from(pack.pieces.len()).map_err(|_| StoreError::Field("piece count"))?;
            b.extend_from_slice(&id_len.to_le_bytes());
            b.extend_from_slice(&kind_len.to_le_bytes());
            b.push(pack.min_zoom);
            b.push(pack.max_zoom);
            b.extend_from_slice(&piece_count.to_le_bytes());
            for part in pack.bounds {
                b.extend_from_slice(&part.to_le_bytes());
            }
            b.extend_from_slice(&pack.total.to_le_bytes());
            b.extend_from_slice(&pack.whole);
            b.extend_from_slice(&pack.attribution);
            b.extend_from_slice(&pack.attribution_len.to_le_bytes());
            b.extend_from_slice(&0u32.to_le_bytes());
            b.extend_from_slice(pack.id.as_bytes());
            b.extend_from_slice(pack.kind.as_bytes());
            for piece in &pack.pieces {
                b.extend_from_slice(&piece.hash);
                b.extend_from_slice(&piece.offset.to_le_bytes());
                b.extend_from_slice(&piece.len.to_le_bytes());
                b.push(piece.zoom);
                b.extend_from_slice(&[0u8; 3]);
            }
        }
        debug_assert_eq!(b.len(), body_len);
        Ok(b)
    }

    /// Encode and sign: the body, then the trailer with `identity`'s
    /// public keys and its signature. `identity` must hold private keys.
    pub fn signed(&self, identity: &Identity) -> Result<Vec<u8>, StoreError> {
        let mut index = self.clone();
        index.key = identity.public_key_bytes();
        let body = index.encode_body()?;
        let signature = identity
            .sign(&body)
            .map_err(|e| StoreError::Identity(e.to_string()))?;
        let mut out = body;
        out.extend_from_slice(&index.key);
        out.extend_from_slice(&signature);
        Ok(out)
    }

    /// Decode and verify against the pinned server: the embedded key's
    /// `lev.maps` destination must be `pinned`, the signature must
    /// verify, and `floor` (the highest generation already seen) must
    /// not be undercut. This is what a device runs.
    pub fn verify(
        bytes: &[u8],
        pinned: &DestinationHash,
        floor: Option<u64>,
    ) -> Result<Index, VerifyError> {
        let index = Self::verify_self(bytes, floor)?;
        let destination = Self::destination_of(&index.key)?;
        if destination != *pinned {
            return Err(VerifyError::NotThePinnedServer {
                got: hex::encode(destination.as_bytes()),
                pinned: hex::encode(pinned.as_bytes()),
            });
        }
        Ok(index)
    }

    /// Decode and verify self-consistency alone: well-formed, signed by
    /// the embedded key, `floor` not undercut — but ANY key. This is
    /// what the front-door cache runs; it has no pin of its own, the
    /// devices behind it verify the pin themselves.
    pub fn verify_self(bytes: &[u8], floor: Option<u64>) -> Result<Index, VerifyError> {
        if bytes.len() < HEADER_LEN + TRAILER_LEN {
            return Err(VerifyError::Format("too short"));
        }
        let (body, trailer) = bytes.split_at(bytes.len() - TRAILER_LEN);
        let mut key = [0u8; 64];
        key.copy_from_slice(&trailer[..64]);
        let identity = Identity::from_public_key_bytes(&key).map_err(|_| VerifyError::Key)?;
        if !identity
            .verify(body, &trailer[64..])
            .map_err(|_| VerifyError::Signature)?
        {
            return Err(VerifyError::Signature);
        }
        let index = Self::parse_body(body, key)?;
        if let Some(floor) = floor {
            if index.generation < floor {
                return Err(VerifyError::Rollback {
                    got: index.generation,
                    floor,
                });
            }
        }
        Ok(index)
    }

    /// Parse the body. The signature is already checked, but the parse
    /// stays bounds-checked throughout: the front door feeds it bytes
    /// signed by keys it does not know.
    fn parse_body(b: &[u8], key: [u8; 64]) -> Result<Index, VerifyError> {
        if b.len() < HEADER_LEN {
            return Err(VerifyError::Format("too short"));
        }
        if b[..4] != INDEX_MAGIC {
            return Err(VerifyError::Format("magic"));
        }
        if u16le(b, 4) != INDEX_VERSION {
            return Err(VerifyError::Format("version"));
        }
        if u16le(b, 6) as usize != HEADER_LEN {
            return Err(VerifyError::Format("header length"));
        }
        if u64le(b, 32) != b.len() as u64 {
            return Err(VerifyError::Format("body length"));
        }
        let generation = u64le(b, 8);
        let created = u64le(b, 16);
        let url_len = u16le(b, 24) as usize;
        let pack_count = u16le(b, 26) as usize;
        let mut at = HEADER_LEN;
        let url = b
            .get(at..at + url_len)
            .ok_or(VerifyError::Format("http url"))?;
        let http_url = core::str::from_utf8(url)
            .map_err(|_| VerifyError::Format("http url utf-8"))?
            .to_owned();
        at += url_len;
        let mut packs = Vec::with_capacity(pack_count);
        for _ in 0..pack_count {
            let fixed = b
                .get(at..at + PACK_FIXED_LEN)
                .ok_or(VerifyError::Format("pack"))?;
            let id_len = u16le(fixed, 0) as usize;
            let kind_len = u16le(fixed, 2) as usize;
            let piece_count = u16le(fixed, 6) as usize;
            let bounds = [
                i32le(fixed, 8),
                i32le(fixed, 12),
                i32le(fixed, 16),
                i32le(fixed, 20),
            ];
            let total = u64le(fixed, 24);
            let mut whole = [0u8; 32];
            whole.copy_from_slice(&fixed[32..64]);
            let mut attribution = [0u8; 32];
            attribution.copy_from_slice(&fixed[64..96]);
            let attribution_len = u32le(fixed, 96);
            at += PACK_FIXED_LEN;
            let id = b
                .get(at..at + id_len)
                .ok_or(VerifyError::Format("pack id"))?;
            let id = core::str::from_utf8(id)
                .map_err(|_| VerifyError::Format("pack id utf-8"))?
                .to_owned();
            at += id_len;
            let kind = b
                .get(at..at + kind_len)
                .ok_or(VerifyError::Format("pack kind"))?;
            let kind = core::str::from_utf8(kind)
                .map_err(|_| VerifyError::Format("pack kind utf-8"))?
                .to_owned();
            at += kind_len;
            let mut pieces = Vec::with_capacity(piece_count);
            for _ in 0..piece_count {
                let p = b
                    .get(at..at + PIECE_LEN)
                    .ok_or(VerifyError::Format("piece"))?;
                let mut hash = [0u8; 32];
                hash.copy_from_slice(&p[..32]);
                pieces.push(Piece {
                    hash,
                    offset: u64le(p, 32),
                    len: u32le(p, 40),
                    zoom: p[44],
                });
                at += PIECE_LEN;
            }
            packs.push(Pack {
                id,
                kind,
                min_zoom: fixed[4],
                max_zoom: fixed[5],
                bounds,
                total,
                whole,
                attribution,
                attribution_len,
                pieces,
            });
        }
        if at != b.len() {
            return Err(VerifyError::Format("trailing bytes"));
        }
        Ok(Index {
            generation,
            created,
            http_url,
            packs,
            key,
        })
    }

    /// The pack with this id, if the index has it.
    pub fn pack(&self, id: &str) -> Option<&Pack> {
        self.packs.iter().find(|p| p.id == id)
    }
}

/// Check one piece's bytes against what the index promised: the length
/// and the hash. The piece is content-addressed, so this is the whole
/// integrity check, whoever carried the piece.
pub fn verify_piece(piece: &Piece, bytes: &[u8]) -> Result<(), VerifyError> {
    if bytes.len() as u64 != u64::from(piece.len) {
        return Err(VerifyError::Tampered {
            id: piece.id(),
            reason: "length",
        });
    }
    if sha256(bytes) != piece.hash {
        return Err(VerifyError::Tampered {
            id: piece.id(),
            reason: "hash",
        });
    }
    Ok(())
}

/// Whether `id` is a well-formed piece id: exactly 64 lowercase hex
/// characters. Everything that touches the filesystem goes through
/// this, so a request path can never climb out of the pieces directory.
pub fn valid_piece_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Where a piece lives under the store: `pieces/<aa>/<id>`, the first
/// two hex characters fanning out the directory.
pub fn piece_path(store: &Path, id: &str) -> PathBuf {
    store.join(PIECES_DIR).join(&id[..2]).join(id)
}

/// Where the index lives under the store.
pub fn index_path(store: &Path) -> PathBuf {
    store.join(INDEX_FILE)
}

/// One split point of a region file: the byte range and the deepest
/// zoom that is complete once the range is there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitPiece {
    /// Start of the range in the file.
    pub start: u64,
    /// End of the range (exclusive).
    pub end: u64,
    /// The deepest zoom complete at `end`.
    pub zoom: u8,
}

/// Split a complete region file (version 4 or newer) at zoom
/// boundaries: piece 0 carries the header, the texts, the tile index
/// and the shallowest zoom's tiles; each further piece one zoom deeper;
/// the last piece additionally the names, buildings and pois sections
/// at the file's end. A zoom with no bytes of its own folds into the
/// piece before it.
pub fn split_region(bytes: &[u8]) -> Result<Vec<SplitPiece>, StoreError> {
    if bytes.len() < 8 {
        return Err(StoreError::Region("too short".into()));
    }
    let version = u16le(bytes, 4);
    if version < REGION_VERSION_FLOOR {
        return Err(StoreError::Region(format!(
            "region version {version}: pieces need version {REGION_VERSION_FLOOR} or newer \
             (the tile index first, the tiles coarse to fine)"
        )));
    }
    let region = Region::from_bytes(bytes).map_err(|e| StoreError::Region(format!("{e:?}")))?;
    if !region.is_complete() {
        return Err(StoreError::Region("file is not complete".into()));
    }
    let header = *region.header();
    // Where the tile blobs start: right after the index, which version 4
    // places after the texts.
    let data_start = header.index + u64::from(header.tiles) * ENTRY_LEN as u64;
    // The furthest byte any tile of zoom <= z needs. Shared blobs point
    // backwards (the cut stores identical small tiles once), which a
    // running maximum absorbs.
    let mut end_of = vec![data_start; usize::from(header.max_zoom) + 1];
    for n in 0..header.tiles {
        let (id, slot) = region
            .slot(n)
            .map_err(|e| StoreError::Region(format!("slot {n}: {e:?}")))?;
        if let leviculum_map::region::Slot::Here(e) = slot {
            let end = e.offset + u64::from(e.len);
            let z = usize::from(id.z);
            if end_of[z] < end {
                end_of[z] = end;
            }
        }
    }
    let mut pieces: Vec<SplitPiece> = Vec::new();
    let mut start = 0u64;
    let mut running = data_start;
    for z in header.min_zoom..header.max_zoom {
        running = running.max(end_of[usize::from(z)]);
        if running > start {
            pieces.push(SplitPiece {
                start,
                end: running,
                zoom: z,
            });
            start = running;
        } else if let Some(last) = pieces.last_mut() {
            // Nothing of its own at this zoom: the piece before it
            // already completes it.
            last.zoom = z;
        }
    }
    // The deepest zoom's tiles plus the names, buildings and pois
    // sections at the end of the file.
    pieces.push(SplitPiece {
        start,
        end: header.len,
        zoom: header.max_zoom,
    });
    Ok(pieces)
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> StoreError + '_ {
    move |source| StoreError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// Write `bytes` at `path` atomically: a temporary file beside it,
/// renamed over it. Pieces are immutable, so an existing file is left
/// alone (its name is its content).
fn write_once(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    if path.exists() {
        return Ok(());
    }
    atomic_write(path, bytes)
}

/// Write `bytes` at `path` atomically, replacing what is there.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let parent = path.parent().ok_or_else(|| StoreError::Io {
        path: path.display().to_string(),
        source: std::io::Error::other("no parent directory"),
    })?;
    std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    let tmp = parent.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        hex::encode(&sha256(bytes)[..8])
    ));
    std::fs::write(&tmp, bytes).map_err(io_err(&tmp))?;
    std::fs::rename(&tmp, path).map_err(io_err(path))?;
    Ok(())
}

/// Store `bytes` as a piece, returning its hash. Immutable: a piece
/// already there is not rewritten.
pub fn write_piece(store: &Path, bytes: &[u8]) -> Result<[u8; 32], StoreError> {
    let hash = sha256(bytes);
    let path = piece_path(store, &hex::encode(hash));
    write_once(&path, bytes)?;
    Ok(hash)
}

/// What [`add_pack`] needs besides the region bytes.
#[derive(Clone, Debug)]
pub struct AddOptions {
    /// The pack's id in the catalogue.
    pub id: String,
    /// The data kind: [`KIND_VECTORS`] for a region file,
    /// [`KIND_ELEVATION`] for a height file.
    pub kind: String,
    /// The server's HTTP base URL to write into the index; `None` keeps
    /// what the index already names.
    pub http_url: Option<String>,
    /// Let the pack replace one of another kind under the same id. Off,
    /// such an add is refused ([`StoreError::KindMismatch`]); a pack of
    /// the same kind always replaces its predecessor.
    pub replace: bool,
}

/// A pack's file cut into pieces, with what the index says of it.
struct Layout {
    pieces: Vec<SplitPiece>,
    min_zoom: u8,
    max_zoom: u8,
    bounds: [i32; 4],
    attribution: String,
}

/// A region file: split at zoom boundaries, a prefix ladder.
fn region_layout(region: &[u8]) -> Result<Layout, StoreError> {
    let pieces = split_region(region)?;
    let parsed = Region::from_bytes(region).map_err(|e| StoreError::Region(format!("{e:?}")))?;
    let header = *parsed.header();
    Ok(Layout {
        pieces,
        min_zoom: header.min_zoom,
        max_zoom: header.max_zoom,
        bounds: [
            header.bounds.west,
            header.bounds.south,
            header.bounds.east,
            header.bounds.north,
        ],
        attribution: format!("{}\n{}\n", parsed.attribution(), parsed.notice()),
    })
}

/// A height file: the directory piece (zoom byte 0: it completes no
/// zoom), then one piece per block, each complete from the block zoom to
/// the tile zoom over its own area, so its zoom byte is the tile zoom.
/// The blocks are independent: a device fetches the directory, then the
/// blocks of its view in any order.
fn elevation_layout(file: &[u8]) -> Result<Layout, StoreError> {
    let (directory, ranges) = leviculum_map::elevation::split(file)
        .map_err(|e| StoreError::Region(format!("height file: {e:?}")))?;
    let pieces = ranges
        .iter()
        .enumerate()
        .map(|(n, r)| SplitPiece {
            start: r.start as u64,
            end: r.end as u64,
            zoom: if n == 0 { 0 } else { directory.tile_zoom() },
        })
        .collect();
    Ok(Layout {
        pieces,
        min_zoom: directory.block_zoom(),
        max_zoom: directory.tile_zoom(),
        bounds: directory.bounds(),
        attribution: format!("{}\n{}\n", directory.attribution(), directory.notice()),
    })
}

/// Cut `file` into pieces under `store` and write a fresh signed index:
/// the pack replaces one with the same id, every other pack is kept, the
/// generation grows by one, and `identity` signs. Returns the new index.
/// A pack of another kind under the same id is refused unless
/// `options.replace` says otherwise.
/// `options.kind` says how the file splits: [`KIND_ELEVATION`] a height
/// file, anything else a region file.
pub fn add_pack(
    store: &Path,
    file: &[u8],
    identity: &Identity,
    options: &AddOptions,
) -> Result<Index, StoreError> {
    let layout = if options.kind == KIND_ELEVATION {
        elevation_layout(file)?
    } else {
        region_layout(file)?
    };

    // The index that is there, verified against our own key before it
    // feeds the new one: a corrupt or foreign index is an error, never
    // silently replaced.
    let path = index_path(store);
    let old = match std::fs::read(&path) {
        Ok(bytes) => {
            let own = Index::destination_of(&identity.public_key_bytes())
                .map_err(StoreError::Existing)?;
            Some(Index::verify(&bytes, &own, None)?)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(io_err(&path)(e)),
    };

    // A pack of another kind under the same id is refused before a piece
    // is written, unless the caller asked to replace it.
    if let Some(existing) = old.as_ref().and_then(|index| index.pack(&options.id)) {
        if existing.kind != options.kind && !options.replace {
            return Err(StoreError::KindMismatch {
                id: options.id.clone(),
                existing: existing.kind.clone(),
                new: options.kind.clone(),
            });
        }
    }

    let mut pieces = Vec::with_capacity(layout.pieces.len());
    for part in &layout.pieces {
        let bytes = &file[part.start as usize..part.end as usize];
        let hash = write_piece(store, bytes)?;
        pieces.push(Piece {
            hash,
            offset: part.start,
            len: u32::try_from(part.end - part.start).map_err(|_| StoreError::Field("piece"))?,
            zoom: part.zoom,
        });
    }
    let attribution = write_piece(store, layout.attribution.as_bytes())?;

    let pack = Pack {
        id: options.id.clone(),
        kind: options.kind.clone(),
        min_zoom: layout.min_zoom,
        max_zoom: layout.max_zoom,
        bounds: layout.bounds,
        total: file.len() as u64,
        whole: sha256(file),
        attribution,
        attribution_len: u32::try_from(layout.attribution.len())
            .map_err(|_| StoreError::Field("attribution"))?,
        pieces,
    };

    let mut packs: Vec<Pack> = old
        .as_ref()
        .map(|index| {
            index
                .packs
                .iter()
                .filter(|p| p.id != options.id)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    packs.push(pack);
    packs.sort_by(|a, b| a.id.cmp(&b.id));

    let index = Index {
        generation: old.as_ref().map(|i| i.generation + 1).unwrap_or(1),
        created: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        http_url: options
            .http_url
            .clone()
            .or_else(|| old.as_ref().map(|i| i.http_url.clone()))
            .unwrap_or_default(),
        packs,
        key: identity.public_key_bytes(),
    };
    let signed = index.signed(identity)?;
    atomic_write(&path, &signed)?;
    Ok(index)
}
