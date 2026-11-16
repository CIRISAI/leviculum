//! The delivery destination's own ratchets, brought over from the app the
//! identity came from (leviculum 1185; `docs/src/concepts/loquax-import.md`,
//! "Ratchets").
//!
//! Every Python LXMF router turns ratchets on for its delivery destination
//! (`reference/LXMF/LXMF/LXMRouter.py:357`), and so does Ratspeak. A peer
//! that heard one announced encrypts every single packet to that ratchet for
//! 30 days (`RNS/Identity.py:69`, `RATCHET_EXPIRY`), until an announce with
//! a newer ratchet replaces it. An identity that moves without its ratchets
//! loses every short message from those peers until then, and the peer is
//! never told. So the private ratchet keys come along, into the file the
//! node loads them from at its start
//! ([`leviculum_std::dest_ratchet_keys_path`]), behind the ones it already
//! holds.
//!
//! Two formats, told apart by their content:
//!
//! - **Python** (NomadNet, Sideband, any `LXMRouter`):
//!   `<lxmf storage>/ratchets/<destination hash>.ratchets`, written by
//!   `Destination._persist_ratchets` (`RNS/Destination.py:210-220`):
//!   `umsgpack.packb({"signature": sign(packed), "ratchets": packed})`,
//!   `packed` being `umsgpack.packb(list_of_32_byte_private_keys)`, newest
//!   first. Both values are msgpack `bin`. The signature is the identity's
//!   Ed25519 over `packed`.
//! - **Ratspeak** (rsReticulum's `RatchetRing::save`, `rns-identity`
//!   `ratchet.rs:129`): `identities/<identity hash>/ratchets/ring`, the
//!   struct `{signature: Vec<u8>, ratchets: Vec<u8>, last_rotation: f64}`
//!   through `rmp_serde::to_vec`, which writes a struct as an ARRAY of its
//!   fields in order and a `Vec<u8>` as an array of integers, not `bin`;
//!   `ratchets` is `rmp_serde::to_vec(&Vec<[u8; 32]>)`, newest first, each
//!   key an array of 32 integers. The signature is NOT over the key list:
//!   Ratspeak signs the current ratchet's PUBLIC key
//!   (`runtime/lxmf.rs:938-947` and `:2517-2528`, `identity.sign(
//!   current_public_key)`), so that is what is checked. The reader also
//!   takes the field-named map form and `bin` values, which the same
//!   crate writes with other settings.
//!
//! Either way the signature must hold for the identity loquax answers on:
//! a ring of another identity opens nothing of ours.

use std::path::{Path, PathBuf};

use leviculum_core::constants::RATCHET_SIZE;
use leviculum_lxmf::msgpack::{self, Kind};
use leviculum_lxmf::node::LxmfNode;
use leviculum_std::Identity;

use super::mpack;

/// A ratchet file's private keys, newest first.
pub type Keys = Vec<[u8; RATCHET_SIZE]>;

/// Which program wrote a ratchet file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Python,
    Ratspeak,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Python => "Python LXMF",
            Format::Ratspeak => "Ratspeak",
        }
    }
}

/// A ratchet file read and checked against an identity.
#[derive(Debug)]
pub struct Ring {
    pub format: Format,
    pub keys: Keys,
}

/// Read a ratchet file of either format and check its signature against
/// `identity`.
pub fn read(data: &[u8], identity: &Identity) -> Result<Ring, String> {
    if let Some((signature, packed)) = python_envelope(data) {
        if !identity.verify(packed, signature).unwrap_or(false) {
            return Err(not_ours());
        }
        let keys = key_list(packed).ok_or("the signed key list is not a list of keys")?;
        return Ok(Ring {
            format: Format::Python,
            keys,
        });
    }
    if let Some((signature, packed)) = ratspeak_envelope(data) {
        let keys = key_list(&packed).ok_or("the ring's key list is not a list of keys")?;
        let current = keys.first().ok_or("the ring holds no keys")?;
        let public = leviculum_core::Destination::ratchet_public_key(current);
        if !identity.verify(&public, &signature).unwrap_or(false) {
            return Err(not_ours());
        }
        return Ok(Ring {
            format: Format::Ratspeak,
            keys,
        });
    }
    Err("not a ratchet file: neither Python's signed map nor Ratspeak's ring".to_string())
}

fn not_ours() -> String {
    "the ratchets are not signed by the identity Loquax answers on: they belong to another \
     identity, or the file is damaged"
        .to_string()
}

/// Python's `{"signature": bin, "ratchets": bin}`.
fn python_envelope(data: &[u8]) -> Option<(&[u8], &[u8])> {
    if msgpack::peek_kind(data, 0).ok()? != Kind::Map {
        return None;
    }
    let map = mpack::Map::read(data).ok()?;
    let mut p = 0;
    let signature = msgpack::read_bin(map.get("signature")?, &mut p).ok()?;
    let mut p = 0;
    let packed = msgpack::read_bin(map.get("ratchets")?, &mut p).ok()?;
    Some((signature, packed))
}

/// Ratspeak's ring: an array `[signature, ratchets, last_rotation]`, or the
/// same fields by name, each byte string `bin` or an array of integers.
fn ratspeak_envelope(data: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let (signature, packed) = match msgpack::peek_kind(data, 0).ok()? {
        Kind::Array => {
            let fields = mpack::array(data)?;
            (*fields.first()?, *fields.get(1)?)
        }
        Kind::Map => {
            let map = mpack::Map::read(data).ok()?;
            (map.get("signature")?, map.get("ratchets")?)
        }
        _ => return None,
    };
    Some((byte_string(signature)?, byte_string(packed)?))
}

/// A byte string as either crate writes one: `bin`, or an array of
/// integers that each fit a byte.
fn byte_string(raw: &[u8]) -> Option<Vec<u8>> {
    if msgpack::peek_kind(raw, 0).ok()? == Kind::Array {
        return mpack::array(raw)?
            .into_iter()
            .map(|v| u8::try_from(mpack::int(v)?).ok())
            .collect();
    }
    let mut p = 0;
    msgpack::read_bin(raw, &mut p).ok().map(<[u8]>::to_vec)
}

/// The packed key list: an array of 32-byte keys, each a byte string.
fn key_list(packed: &[u8]) -> Option<Keys> {
    mpack::array(packed)?
        .into_iter()
        .map(|raw| byte_string(raw)?.try_into().ok())
        .collect()
}

/// What taking a ring over did, or in a dry run would do.
#[derive(Debug, PartialEq, Eq)]
pub struct Taken {
    /// Keys the destination did not hold yet.
    pub added: usize,
    /// Keys it holds afterwards.
    pub held: usize,
    /// The file the node loads them from.
    pub path: PathBuf,
}

/// Keep `keys` for the delivery destination of `identity` in the loquax
/// home `home`: behind the ratchets it holds already, each once, within the
/// reference's 512 (`RNS/Destination.py:85`). With `dry_run` nothing is
/// written. The caller holds the delivery lock: a running node would write
/// its own set over the file at its next rotation.
pub fn take_over(
    home: &Path,
    identity: &Identity,
    keys: &[[u8; RATCHET_SIZE]],
    dry_run: bool,
) -> Result<Taken, String> {
    let address = crate::engine::delivery_address(identity)?;
    let path = leviculum_std::dest_ratchet_keys_path(&home.join("storage"), &address);
    let bytes = identity
        .private_key_bytes()
        .map_err(|e| format!("the identity has no private key: {e:?}"))?;
    let copy = Identity::from_private_key_bytes(&bytes)
        .map_err(|e| format!("could not copy the identity: {e:?}"))?;
    let mut destination =
        LxmfNode::delivery_destination(copy).map_err(|e| format!("delivery destination: {e:?}"))?;
    match std::fs::read(&path) {
        Ok(held) => {
            destination
                .load_ratchets_signed(&held)
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("{}: {error}", path.display())),
    }
    let added = destination.retain_ratchet_keys(keys);
    if added > 0 && !dry_run {
        let signed = destination
            .serialize_ratchets_signed()
            .ok_or("the ratchets could not be signed")?;
        super::key::write_private(&path, &signed)?;
        crate::identity::mark_moved(home)?;
    }
    Ok(Taken {
        added,
        held: destination.ratchet_count(),
        path,
    })
}

/// The ring the delivery destination of `identity` holds in the loquax
/// home `home`, as `loquax mailbox ratchets` prints it (1305): how many
/// ratchets, and the SHA-256 over their public keys newest first. Two homes
/// with the same hash open the same packets; the private keys never leave
/// the file. `None` when the destination has no ring yet.
pub fn ring_hash(home: &Path, identity: &Identity) -> Result<Option<(usize, [u8; 32])>, String> {
    let address = crate::engine::delivery_address(identity)?;
    let path = leviculum_std::dest_ratchet_keys_path(&home.join("storage"), &address);
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let ring = read(&data, identity).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some((ring.keys.len(), public_hash(&ring.keys))))
}

fn public_hash(keys: &[[u8; RATCHET_SIZE]]) -> [u8; 32] {
    let publics: Vec<u8> = keys
        .iter()
        .flat_map(leviculum_core::Destination::ratchet_public_key)
        .collect();
    leviculum_core::crypto::sha256(&publics)
}

/// Read the ratchet file at `file`, check it against `identity` and take it
/// over into `home`: [`read`] then [`take_over`].
pub fn import_file(
    home: &Path,
    identity: &Identity,
    file: &Path,
    dry_run: bool,
) -> Result<(Format, Taken), String> {
    let data = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let ring = read(&data, identity).map_err(|e| format!("{}: {e}", file.display()))?;
    let taken = take_over(home, identity, &ring.keys, dry_run)?;
    Ok((ring.format, taken))
}

/// The line the import shows for a ring taken over.
pub fn line(file: &Path, format: Format, taken: &Taken, dry_run: bool) -> String {
    let verb = match dry_run {
        true => "to take over",
        false => "taken over",
    };
    format!(
        "ratchets: {} of {}'s {verb} from {}, {} held; a peer that remembered one reaches \
         this identity again",
        taken.added,
        format.name(),
        file.display(),
        taken.held
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Identity {
        leviculum_std::generate_identity()
    }

    /// A file as `Destination._persist_ratchets` writes it.
    fn python_file(identity: &Identity, keys: &[[u8; 32]]) -> Vec<u8> {
        let mut packed = Vec::new();
        msgpack::array(&mut packed, keys.len());
        for key in keys {
            msgpack::bin(&mut packed, key);
        }
        let signature = identity.sign(&packed).expect("sign");
        let mut out = Vec::new();
        msgpack::map(&mut out, 2);
        msgpack::string(&mut out, "signature");
        msgpack::bin(&mut out, &signature);
        msgpack::string(&mut out, "ratchets");
        msgpack::bin(&mut out, &packed);
        out
    }

    /// The struct `RatchetRing::save` serialises, field for field.
    #[derive(serde::Serialize)]
    struct RatchetRingPersisted {
        signature: Vec<u8>,
        ratchets: Vec<u8>,
        last_rotation: f64,
    }

    /// A ring as Ratspeak writes it: `RatchetRing::save` (rsReticulum
    /// `rns-identity/src/ratchet.rs:129`) through the same crate and the
    /// same calls, signed as `runtime/lxmf.rs:2517-2528` signs it.
    fn ratspeak_ring(identity: &Identity, keys: &[[u8; 32]]) -> Vec<u8> {
        let ratchets = rmp_serde::to_vec(&keys.to_vec()).expect("the key list");
        let public = leviculum_core::Destination::ratchet_public_key(&keys[0]);
        let signature = identity.sign(&public).expect("sign").to_vec();
        rmp_serde::to_vec(&RatchetRingPersisted {
            signature,
            ratchets,
            last_rotation: 1_793_000_000.5,
        })
        .expect("the ring")
    }

    #[test]
    fn both_formats_read_back_their_keys_newest_first() {
        let id = identity();
        let keys = [[7u8; 32], [8u8; 32], [0xF0u8; 32]];
        let python = read(&python_file(&id, &keys), &id).expect("python");
        assert_eq!(python.format, Format::Python);
        assert_eq!(python.keys, keys.to_vec());
        let ring = ratspeak_ring(&id, &keys);
        // The crate writes the struct as an array and bytes as integers:
        // the layout the module header cites, not bin.
        assert_eq!(msgpack::peek_kind(&ring, 0), Ok(Kind::Array));
        let ratspeak = read(&ring, &id).expect("ratspeak");
        assert_eq!(ratspeak.format, Format::Ratspeak);
        assert_eq!(ratspeak.keys, keys.to_vec());
    }

    /// A 512-key ring is about the size of Lew's (42 936 bytes, 2026-10-31):
    /// the layout is the one his file has.
    #[test]
    fn a_full_ring_is_the_size_of_the_one_on_the_mailbox() {
        let id = identity();
        let keys: Vec<[u8; 32]> = (0..512u32)
            .map(|i| {
                let mut key = [0u8; 32];
                for (j, b) in key.iter_mut().enumerate() {
                    *b = (i as u8)
                        .wrapping_mul(31)
                        .wrapping_add((j as u8).wrapping_mul(97));
                }
                key
            })
            .collect();
        let ring = ratspeak_ring(&id, &keys);
        assert!(
            (40_000..46_000).contains(&ring.len()),
            "a 512-key ring is {} bytes",
            ring.len()
        );
        assert_eq!(read(&ring, &id).expect("ring").keys.len(), 512);
    }

    #[test]
    fn another_identitys_ratchets_are_refused() {
        let (id, other) = (identity(), identity());
        let keys = [[7u8; 32]];
        assert!(read(&python_file(&other, &keys), &id)
            .unwrap_err()
            .contains("not signed"));
        assert!(read(&ratspeak_ring(&other, &keys), &id)
            .unwrap_err()
            .contains("not signed"));
        assert!(read(b"\x93\x01\x02\x03", &id).is_err());
        assert!(read(b"not msgpack", &id).is_err());
    }

    /// Taken over behind what the node holds, once each, and loaded by the
    /// node's own reader at its next start.
    #[test]
    fn taken_over_keys_go_behind_the_held_ones_and_load_at_the_start() {
        let dir = tempfile::tempdir().expect("tempdir");
        let id = identity();
        let first = take_over(dir.path(), &id, &[[1u8; 32]], false).expect("first");
        assert_eq!((first.added, first.held), (1, 1));
        let dry = take_over(dir.path(), &id, &[[2u8; 32], [1u8; 32]], true).expect("dry");
        assert_eq!((dry.added, dry.held), (1, 2));
        let again = take_over(dir.path(), &id, &[[2u8; 32], [1u8; 32]], false).expect("again");
        assert_eq!((again.added, again.held), (1, 2));
        assert!(crate::identity::moved_at(dir.path()).is_some());

        let copy =
            Identity::from_private_key_bytes(&id.private_key_bytes().expect("key")).expect("copy");
        let mut destination = LxmfNode::delivery_destination(copy).expect("destination");
        let held = std::fs::read(&again.path).expect("the file");
        assert_eq!(destination.load_ratchets_signed(&held), Ok(2));
        assert_eq!(
            destination.current_ratchet_public(),
            Some(leviculum_core::Destination::ratchet_public_key(&[1u8; 32]))
        );
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&again.path)
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
