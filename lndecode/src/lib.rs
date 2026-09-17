//! `lndecode` — wire bytes to structured fields, decoded independently.
//!
//! This crate turns a raw Reticulum frame into JSON without going through
//! any of the code that wrote it. That independence is the point: the audit
//! method in `docs/src/concepts/wire-field-semantics.md` asks whether a field
//! we generate *means* what a peer takes it to mean, and a reader that shares
//! its layout constants with the writer cannot answer that question — it
//! agrees with the writer by construction, including where both are wrong.
//! Every offset below is re-derived from `reference/Reticulum`, cited at the
//! line it comes from, and the crate's dependency list contains no
//! `leviculum-*` crate outside `dev-dependencies`.
//!
//! What it decodes:
//!
//! - the packet header for both header types (`Packet.unpack`,
//!   `reference/Reticulum/RNS/Packet.py:242`);
//! - the packet hash over the hop-stable part (`get_hashable_part`,
//!   `reference/Reticulum/RNS/Packet.py:355`), which is what
//!   `periculum trace` and every dedup table key on;
//! - announce payloads, ratcheted and not, including an *independent*
//!   Ed25519 verification of the signature and an independent recomputation
//!   of the destination hash (`validate_announce`,
//!   `reference/Reticulum/RNS/Identity.py:532`);
//! - path requests (`path_request_handler`,
//!   `reference/Reticulum/RNS/Transport.py:2866`);
//! - link requests, including the 3-byte MTU/mode signalling.
//!
//! Anything else is reported as a typed header plus its payload verbatim,
//! which is still more than a hex dump gives.
//!
//! # Warnings are findings, not errors
//!
//! A decoder for adversarial traffic must not refuse to decode adversarial
//! traffic. A frame at 200 hops, an announce whose signature does not verify,
//! an emission timestamp holding uptime seconds (Codeberg #155) — each of
//! those is a thing we specifically want to *see*, so each becomes an entry
//! in the `warnings` array of an otherwise complete decode. The only hard
//! errors are inputs too short to hold a header at all.

use serde_json::{json, Map, Value};

/// Truncated hash length, in bytes (`TRUNCATED_HASHLENGTH`,
/// `reference/Reticulum/RNS/Identity.py:84`).
const TRUNCATED_HASHBYTES: usize = 16;
/// Announce public-key field: X25519 (32) followed by Ed25519 (32)
/// (`KEYSIZE`, `reference/Reticulum/RNS/Identity.py:59`).
const KEYSIZE: usize = 64;
/// Announce name-hash field (`NAME_HASH_LENGTH`,
/// `reference/Reticulum/RNS/Identity.py:83`).
const NAME_HASH_LEN: usize = 10;
/// Announce random-hash field: 5 random bytes then 5 big-endian unix-second
/// bytes (`random_hash`, `reference/Reticulum/RNS/Destination.py:282`).
const RANDOM_HASH_LEN: usize = 10;
/// Ed25519 signature field (`SIGLENGTH`,
/// `reference/Reticulum/RNS/Identity.py:81`).
const SIG_LEN: usize = 64;
/// Ratchet field, present exactly when the context flag is set
/// (`RATCHETSIZE`, `reference/Reticulum/RNS/Identity.py:64`).
const RATCHET_LEN: usize = 32;
/// Hop ceiling a conforming transport enforces (`PATHFINDER_M`,
/// `reference/Reticulum/RNS/Transport.py:63`).
const PATHFINDER_M: u8 = 128;
/// Base MTU (`MTU`, `reference/Reticulum/RNS/Reticulum.py:93`).
const MTU: usize = 500;
/// Link request payload without MTU signalling: X25519 pub + Ed25519 pub
/// (`request_data`, `reference/Reticulum/RNS/Link.py:316`).
const LINK_REQUEST_BASE_SIZE: usize = 64;
/// Link request payload with the 3-byte MTU/mode signalling appended
/// (`signalling_bytes`, `reference/Reticulum/RNS/Link.py:148`).
const LINK_REQUEST_SIGNALLING_SIZE: usize = 67;

/// Unix seconds below which an announce emission timestamp is not a
/// timestamp at all.
///
/// 2001-09-09. Codeberg #155 stamped process uptime here, which a peer then
/// ordered its path table by; every such value is small. A real clock has
/// not produced a number this low since 2001, so the boundary separates the
/// two populations without a false positive on live traffic.
const IMPLAUSIBLE_EMISSION_BEFORE: u64 = 1_000_000_000;

/// A decoded frame: the JSON document plus the findings raised while decoding.
#[derive(Debug)]
pub struct Decoded {
    /// Every parsed field, as one JSON object.
    pub value: Value,
}

/// Errors that stop a decode before any field exists.
#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Fewer bytes than the smallest possible header.
    TooShort {
        /// Bytes supplied.
        got: usize,
        /// Bytes the declared header type needs.
        need: usize,
    },
    /// The input text was not decodable as hex or base64.
    NotBytes(String),
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DecodeError::TooShort { got, need } => {
                write!(f, "frame too short: {got} bytes, header needs {need}")
            }
            DecodeError::NotBytes(why) => write!(f, "input is neither hex nor base64: {why}"),
        }
    }
}

impl std::error::Error for DecodeError {}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let mut out = [0u8; 32];
    out.copy_from_slice(&sha2::Sha256::digest(data));
    out
}

/// The part of a frame whose hash is stable across forwarding.
///
/// The flags byte with its upper nibble cleared, then everything from the
/// destination hash onward — the transport id and the hop count, the two
/// fields a relay rewrites, are excluded (`get_hashable_part`,
/// `reference/Reticulum/RNS/Packet.py:355`).
fn hashable_part(raw: &[u8]) -> Vec<u8> {
    if raw.len() < 2 {
        return Vec::new();
    }
    let header_type_2 = raw[0] & 0x40 != 0;
    let data_start = if header_type_2 {
        2 + TRUNCATED_HASHBYTES
    } else {
        2
    };
    if raw.len() < data_start {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(1 + raw.len() - data_start);
    out.push(raw[0] & 0x0F);
    out.extend_from_slice(&raw[data_start..]);
    out
}

fn packet_type_name(bits: u8) -> &'static str {
    match bits {
        0x00 => "data",
        0x01 => "announce",
        0x02 => "link_request",
        _ => "proof",
    }
}

fn destination_type_name(bits: u8) -> &'static str {
    match bits {
        0x00 => "single",
        0x01 => "group",
        0x02 => "plain",
        _ => "link",
    }
}

/// Name for a context byte, or `null` for one with no assigned meaning.
///
/// Values from `reference/Reticulum/RNS/Packet.py:72-92`. An unknown byte is
/// reported as unknown rather than as a default: a relay never interprets
/// this field, so a value from a newer RNS is legitimate traffic.
fn context_name(byte: u8) -> Option<&'static str> {
    Some(match byte {
        0x00 => "none",
        0x01 => "resource",
        0x02 => "resource_adv",
        0x03 => "resource_req",
        0x04 => "resource_hmu",
        0x05 => "resource_prf",
        0x06 => "resource_icl",
        0x07 => "resource_rcl",
        0x08 => "cache_request",
        0x09 => "request",
        0x0A => "response",
        0x0B => "path_response",
        0x0C => "command",
        0x0D => "command_status",
        0x0E => "channel",
        0xFA => "keepalive",
        0xFB => "link_identify",
        0xFC => "link_close",
        0xFD => "link_proof",
        0xFE => "lrrtt",
        0xFF => "lrproof",
        _ => return None,
    })
}

/// The well-known PLAIN destination a path request is addressed to:
/// `truncated_hash(name_hash("rnstransport", "path", "request"))`
/// (`request_path`, `reference/Reticulum/RNS/Transport.py:2771`).
///
/// Recomputed here rather than copied from a constant so a change to the
/// name-hash rule shows up as a mismatch instead of as silent agreement.
pub fn path_request_destination_hash() -> [u8; TRUNCATED_HASHBYTES] {
    let name_hash = sha256(b"rnstransport.path.request");
    let mut out = [0u8; TRUNCATED_HASHBYTES];
    out.copy_from_slice(&sha256(&name_hash[..NAME_HASH_LEN])[..TRUNCATED_HASHBYTES]);
    out
}

/// Format unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
///
/// Civil-from-days after Howard Hinnant's `chrono` algorithms; no date
/// dependency, because the only thing a date crate would add here is a
/// second opinion about leap years.
fn utc_iso8601(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Decode one frame.
///
/// `now_secs` is the wall clock the emission-timestamp plausibility check
/// compares against; pass the real clock from a tool and a fixed value from
/// a test, so the test does not change its verdict tomorrow.
pub fn decode(raw: &[u8], now_secs: u64) -> Result<Decoded, DecodeError> {
    if raw.len() < 2 {
        return Err(DecodeError::TooShort {
            got: raw.len(),
            need: 2,
        });
    }

    let mut warnings: Vec<String> = Vec::new();
    let flags = raw[0];
    let header_type_2 = flags & 0x40 != 0;
    let context_flag = flags & 0x20 != 0;
    let header_size = if header_type_2 {
        2 + TRUNCATED_HASHBYTES * 2 + 1
    } else {
        2 + TRUNCATED_HASHBYTES + 1
    };
    if raw.len() < header_size {
        return Err(DecodeError::TooShort {
            got: raw.len(),
            need: header_size,
        });
    }

    let hops = raw[1];
    let mut pos = 2;
    let transport_id = if header_type_2 {
        let id = &raw[pos..pos + TRUNCATED_HASHBYTES];
        pos += TRUNCATED_HASHBYTES;
        Some(id.to_vec())
    } else {
        None
    };
    let destination_hash = raw[pos..pos + TRUNCATED_HASHBYTES].to_vec();
    pos += TRUNCATED_HASHBYTES;
    let context = raw[pos];
    pos += 1;
    let payload = &raw[pos..];

    let packet_type = flags & 0x03;
    let dest_type = (flags >> 2) & 0x03;

    if hops > PATHFINDER_M {
        warnings.push(format!(
            "hops={hops} is above PATHFINDER_M ({PATHFINDER_M}); a conforming transport drops this frame"
        ));
    }
    if raw.len() > MTU {
        warnings.push(format!(
            "frame is {} bytes, above the base MTU ({MTU})",
            raw.len()
        ));
    }
    if let Some(ref tid) = transport_id {
        if tid.iter().all(|b| *b == 0) {
            warnings.push("header type 2 carries an all-zero transport id".to_string());
        }
        if tid == &destination_hash {
            warnings.push("transport id equals the destination hash".to_string());
        }
    }

    let mut doc = Map::new();
    doc.insert("tool".into(), json!("lndecode"));
    doc.insert("bytes".into(), json!(raw.len()));
    doc.insert("frame_sha256".into(), json!(hex(&sha256(raw))));
    doc.insert(
        "packet_hash".into(),
        json!(hex(&sha256(&hashable_part(raw)))),
    );
    doc.insert(
        "flags".into(),
        json!({
            "byte": format!("0x{flags:02x}"),
            "ifac": flags & 0x80 != 0,
            "header_type": if header_type_2 { 2 } else { 1 },
            "context_flag": context_flag,
            "propagation": if flags & 0x10 != 0 { "transport" } else { "broadcast" },
            "destination_type": destination_type_name(dest_type),
            "packet_type": packet_type_name(packet_type),
        }),
    );
    doc.insert("hops".into(), json!(hops));
    doc.insert(
        "transport_id".into(),
        transport_id.as_ref().map_or(Value::Null, |t| json!(hex(t))),
    );
    doc.insert("destination_hash".into(), json!(hex(&destination_hash)));
    doc.insert(
        "context".into(),
        json!({
            "byte": format!("0x{context:02x}"),
            "name": context_name(context).map_or(Value::Null, |n| json!(n)),
        }),
    );
    doc.insert(
        "payload".into(),
        json!({ "bytes": payload.len(), "hex": hex(payload) }),
    );

    match packet_type {
        0x01 => {
            let (announce, mut w) =
                decode_announce(&destination_hash, payload, context_flag, now_secs);
            warnings.append(&mut w);
            doc.insert("announce".into(), announce);
        }
        0x02 => {
            let (lr, mut w) = decode_link_request(payload);
            warnings.append(&mut w);
            doc.insert("link_request".into(), lr);
        }
        0x00 if destination_hash == path_request_destination_hash() => {
            let (pr, mut w) = decode_path_request(payload);
            warnings.append(&mut w);
            doc.insert("path_request".into(), pr);
        }
        _ => {}
    }

    doc.insert("warnings".into(), json!(warnings));
    Ok(Decoded {
        value: Value::Object(doc),
    })
}

/// Decode an announce payload and check what a peer decides from it.
///
/// Layout and the context-flag rule for the ratchet from `validate_announce`
/// (`reference/Reticulum/RNS/Identity.py:532`); the signature covers
/// `destination_hash + public_key + name_hash + random_hash + ratchet +
/// app_data` (`reference/Reticulum/RNS/Identity.py:566`).
fn decode_announce(
    destination_hash: &[u8],
    payload: &[u8],
    context_flag: bool,
    now_secs: u64,
) -> (Value, Vec<String>) {
    let mut warnings = Vec::new();
    let ratchet_len = if context_flag { RATCHET_LEN } else { 0 };
    let fixed = KEYSIZE + NAME_HASH_LEN + RANDOM_HASH_LEN + ratchet_len + SIG_LEN;
    if payload.len() < fixed {
        warnings.push(format!(
            "announce payload is {} bytes, below the {fixed} its own context flag declares",
            payload.len()
        ));
        return (
            json!({ "truncated": true, "declared_fixed_len": fixed }),
            warnings,
        );
    }

    let public_key = &payload[..KEYSIZE];
    let name_hash = &payload[KEYSIZE..KEYSIZE + NAME_HASH_LEN];
    let rh_at = KEYSIZE + NAME_HASH_LEN;
    let random_hash = &payload[rh_at..rh_at + RANDOM_HASH_LEN];
    let ratchet_at = rh_at + RANDOM_HASH_LEN;
    let ratchet = &payload[ratchet_at..ratchet_at + ratchet_len];
    let sig_at = ratchet_at + ratchet_len;
    let signature = &payload[sig_at..sig_at + SIG_LEN];
    let app_data = &payload[sig_at + SIG_LEN..];

    // Independent recomputation of the identity and destination hashes:
    // identity = truncated_hash(public_key), destination =
    // truncated_hash(name_hash + identity) (`Destination.hash`,
    // `reference/Reticulum/RNS/Destination.py:116`). A peer recomputes both
    // and rejects the announce on a mismatch, so a mismatch here is the
    // single most useful thing a decoder can tell an operator.
    let identity_hash = &sha256(public_key)[..TRUNCATED_HASHBYTES];
    let mut dh_input = Vec::with_capacity(NAME_HASH_LEN + TRUNCATED_HASHBYTES);
    dh_input.extend_from_slice(name_hash);
    dh_input.extend_from_slice(identity_hash);
    let derived_dest = &sha256(&dh_input)[..TRUNCATED_HASHBYTES];
    let dest_matches = derived_dest == destination_hash;
    if !dest_matches {
        warnings.push(format!(
            "destination hash {} does not match truncated_hash(name_hash + identity_hash) = {}",
            hex(destination_hash),
            hex(derived_dest)
        ));
    }

    let mut signed = Vec::new();
    signed.extend_from_slice(destination_hash);
    signed.extend_from_slice(public_key);
    signed.extend_from_slice(name_hash);
    signed.extend_from_slice(random_hash);
    signed.extend_from_slice(ratchet);
    signed.extend_from_slice(app_data);
    // Two verdicts, because one would hide the interesting case. `valid` is
    // what a peer decides: the reference and our own stack both call the
    // permissive Ed25519 verifier (`identity.verify`,
    // `leviculum-core/src/identity.rs:226`), so that is what determines
    // whether this announce is accepted on the mesh. `strict_valid`
    // additionally rejects a small-order public key and a non-canonical R —
    // the shape where an all-zero key and an all-zero signature verify
    // against each other for some messages. A frame that passes one and
    // fails the other is accepted by the mesh and proves nothing about who
    // sent it, which is precisely what an injector test wants to see named.
    let signature_valid = verify_ed25519(&public_key[32..], &signed, signature, false);
    let signature_strict_valid = verify_ed25519(&public_key[32..], &signed, signature, true);
    if !signature_valid {
        warnings.push("announce signature does not verify over its own fields".to_string());
    } else if !signature_strict_valid {
        warnings.push(
            "announce signature verifies only under the permissive verifier: the signing key is \
             small-order or the signature is non-canonical, so a peer will accept this announce \
             while it proves nothing about the sender"
                .to_string(),
        );
    }

    // The emission timebase: the last 5 bytes of the random hash, big-endian
    // unix seconds (`random_hash`,
    // `reference/Reticulum/RNS/Destination.py:282`). A peer orders
    // same-destination paths by it; Codeberg #155 is what a wrong value here
    // costs, and it is invisible to anyone who does not look at the number.
    let mut emission: u64 = 0;
    for b in &random_hash[5..] {
        emission = (emission << 8) | u64::from(*b);
    }
    if emission < IMPLAUSIBLE_EMISSION_BEFORE {
        warnings.push(format!(
            "emission timestamp {emission} is before 2001; this is the Codeberg #155 shape (uptime seconds in a unix-time field)"
        ));
    } else if emission > now_secs.saturating_add(86_400) {
        warnings.push(format!(
            "emission timestamp {emission} ({}) is more than a day in the future",
            utc_iso8601(emission)
        ));
    }

    let value = json!({
        "public_key": {
            "hex": hex(public_key),
            "x25519": hex(&public_key[..32]),
            "ed25519": hex(&public_key[32..]),
        },
        "identity_hash": hex(identity_hash),
        "derived_destination_hash": hex(derived_dest),
        "destination_hash_matches": dest_matches,
        "name_hash": hex(name_hash),
        "random_hash": {
            "hex": hex(random_hash),
            "random": hex(&random_hash[..5]),
            "emission_secs": emission,
            "emission_utc": utc_iso8601(emission),
        },
        "ratchet": if context_flag { json!(hex(ratchet)) } else { Value::Null },
        "signature": hex(signature),
        "signature_valid": signature_valid,
        "signature_strict_valid": signature_strict_valid,
        "app_data": {
            "bytes": app_data.len(),
            "hex": hex(app_data),
            "utf8": std::str::from_utf8(app_data).map_or(Value::Null, |s| json!(s)),
        },
    });
    (value, warnings)
}

/// Verify an Ed25519 signature, permissively or strictly.
///
/// `strict` selects `verify_strict`, which rejects a small-order public key
/// and a non-canonical R. The permissive arm is what the mesh applies; the
/// strict arm is what tells an operator whether the signature means anything.
fn verify_ed25519(public: &[u8], message: &[u8], signature: &[u8], strict: bool) -> bool {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let Ok(pk): Result<[u8; 32], _> = public.try_into() else {
        return false;
    };
    let Ok(sig): Result<[u8; 64], _> = signature.try_into() else {
        return false;
    };
    let Ok(key) = VerifyingKey::from_bytes(&pk) else {
        return false;
    };
    let sig = Signature::from_bytes(&sig);
    if strict {
        key.verify_strict(message, &sig).is_ok()
    } else {
        key.verify(message, &sig).is_ok()
    }
}

/// Decode a path request payload: `destination_hash(16) +
/// [transport_id(16)] + tag(16)` (`path_request_handler`,
/// `reference/Reticulum/RNS/Transport.py:2866`).
fn decode_path_request(payload: &[u8]) -> (Value, Vec<String>) {
    let mut warnings = Vec::new();
    if payload.len() < 2 * TRUNCATED_HASHBYTES {
        warnings.push(format!(
            "path request payload is {} bytes; both stacks emit 32 (untagged requestor) or 48",
            payload.len()
        ));
        return (json!({ "truncated": true }), warnings);
    }
    let requested = &payload[..TRUNCATED_HASHBYTES];
    let requestor = if payload.len() > 2 * TRUNCATED_HASHBYTES {
        Some(&payload[TRUNCATED_HASHBYTES..2 * TRUNCATED_HASHBYTES])
    } else {
        None
    };
    let tag = &payload[payload.len() - TRUNCATED_HASHBYTES..];
    if payload.len() != 2 * TRUNCATED_HASHBYTES && payload.len() != 3 * TRUNCATED_HASHBYTES {
        warnings.push(format!(
            "path request payload is {} bytes, neither of the two conformant layouts (32, 48)",
            payload.len()
        ));
    }
    (
        json!({
            "requested_hash": hex(requested),
            "requestor_transport_id": requestor.map_or(Value::Null, |r| json!(hex(r))),
            "tag": hex(tag),
        }),
        warnings,
    )
}

/// Decode a link request payload: X25519 pub (32) + Ed25519 pub (32),
/// optionally followed by 3 signalling bytes holding a 21-bit MTU and a
/// 3-bit mode (`Link.__init__` / `Transport.inbound`,
/// `reference/Reticulum/RNS/Link.py:148`).
fn decode_link_request(payload: &[u8]) -> (Value, Vec<String>) {
    let mut warnings = Vec::new();
    if payload.len() < LINK_REQUEST_BASE_SIZE {
        warnings.push(format!(
            "link request payload is {} bytes, below the {LINK_REQUEST_BASE_SIZE} of its two public keys",
            payload.len()
        ));
        return (json!({ "truncated": true }), warnings);
    }
    let mut value = json!({
        "x25519": hex(&payload[..32]),
        "ed25519": hex(&payload[32..64]),
        "signalling": Value::Null,
    });
    if payload.len() >= LINK_REQUEST_SIGNALLING_SIZE {
        let raw =
            (u32::from(payload[64]) << 16) | (u32::from(payload[65]) << 8) | u32::from(payload[66]);
        let mtu = raw & 0x1F_FFFF;
        let mode = ((raw >> 21) & 0x07) as u8;
        if mtu < MTU as u32 {
            warnings.push(format!(
                "link request signals MTU {mtu}, below the base MTU ({MTU}); a conforming responder raises it to {MTU}"
            ));
        }
        if mode != 0x01 {
            warnings.push(format!(
                "link request signals mode {mode}; only 1 (AES-256-CBC) is supported"
            ));
        }
        value["signalling"] = json!({ "mtu": mtu, "mode": mode });
    } else if payload.len() != LINK_REQUEST_BASE_SIZE {
        warnings.push(format!(
            "link request payload is {} bytes: longer than the 64-byte key pair but shorter than the 67 a signalled request needs",
            payload.len()
        ));
    }
    (value, warnings)
}

/// Decode a text line as hex (with optional separators) or base64.
///
/// Hex is tried first: a hex string is also valid base64 often enough that
/// guessing the other way round silently produces garbage.
pub fn bytes_from_text(text: &str) -> Result<Vec<u8>, DecodeError> {
    let compact: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':' && *c != '-' && *c != '_')
        .collect();
    if compact.is_empty() {
        return Err(DecodeError::NotBytes("empty input".to_string()));
    }
    if compact.len().is_multiple_of(2) && compact.chars().all(|c| c.is_ascii_hexdigit()) {
        let mut out = Vec::with_capacity(compact.len() / 2);
        let b = compact.as_bytes();
        for pair in b.chunks(2) {
            let s = std::str::from_utf8(pair).unwrap_or("");
            out.push(
                u8::from_str_radix(s, 16)
                    .map_err(|e| DecodeError::NotBytes(format!("hex: {e}")))?,
            );
        }
        return Ok(out);
    }
    base64_decode(&compact)
}

fn base64_decode(input: &str) -> Result<Vec<u8>, DecodeError> {
    fn val(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    }
    let body: Vec<u8> = input.bytes().filter(|c| *c != b'=').collect();
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for c in body {
        let v =
            val(c).ok_or_else(|| DecodeError::NotBytes(format!("byte {c:#04x} is not base64")))?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xFF) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal hand-built header. Nothing in this module's code produced
    /// these bytes, which is what makes the assertions below meaningful.
    fn header1(flags: u8, hops: u8, dest: [u8; 16], context: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![flags, hops];
        v.extend_from_slice(&dest);
        v.push(context);
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn header_type_1_fields_come_out_where_the_reference_puts_them() {
        let raw = header1(0x01, 3, [0xAB; 16], 0x00, b"hi");
        let d = decode(&raw, 1_800_000_000).unwrap();
        let v = &d.value;
        assert_eq!(v["flags"]["packet_type"], "announce");
        assert_eq!(v["flags"]["destination_type"], "single");
        assert_eq!(v["flags"]["header_type"], 1);
        assert_eq!(v["hops"], 3);
        assert_eq!(v["transport_id"], Value::Null);
        assert_eq!(v["destination_hash"], "ab".repeat(16));
        assert_eq!(v["context"]["name"], "none");
        assert_eq!(v["payload"]["bytes"], 2);
    }

    #[test]
    fn header_type_2_carries_a_transport_id_ahead_of_the_destination() {
        let mut raw = vec![0x40, 7];
        raw.extend_from_slice(&[0x11; 16]);
        raw.extend_from_slice(&[0x22; 16]);
        raw.push(0x00);
        let d = decode(&raw, 1_800_000_000).unwrap();
        assert_eq!(d.value["flags"]["header_type"], 2);
        assert_eq!(d.value["transport_id"], "11".repeat(16));
        assert_eq!(d.value["destination_hash"], "22".repeat(16));
    }

    #[test]
    fn the_packet_hash_ignores_hops_and_transport_id() {
        // Same frame relayed: hop count bumped, transport id rewritten. A
        // dedup table must see one packet, not two.
        let mut a = vec![0x40, 0];
        a.extend_from_slice(&[0x00; 16]);
        a.extend_from_slice(&[0x33; 16]);
        a.push(0x00);
        a.extend_from_slice(b"payload");
        let mut b = a.clone();
        b[1] = 5;
        b[2..18].copy_from_slice(&[0x99; 16]);
        let ha = decode(&a, 1_800_000_000).unwrap().value["packet_hash"].clone();
        let hb = decode(&b, 1_800_000_000).unwrap().value["packet_hash"].clone();
        assert_eq!(ha, hb);
    }

    #[test]
    fn a_hop_count_above_the_ceiling_is_reported_not_rejected() {
        let raw = header1(0x01, 200, [0x00; 16], 0x00, &[0u8; 8]);
        let d = decode(&raw, 1_800_000_000).unwrap();
        assert_eq!(d.value["hops"], 200);
        let warnings = d.value["warnings"].as_array().unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap().contains("PATHFINDER_M")),
            "expected a hop-ceiling warning, got {warnings:?}"
        );
    }

    #[test]
    fn an_unknown_context_byte_decodes_with_a_null_name() {
        let raw = header1(0x00, 0, [0x00; 16], 0x7A, b"x");
        let d = decode(&raw, 1_800_000_000).unwrap();
        assert_eq!(d.value["context"]["byte"], "0x7a");
        assert_eq!(d.value["context"]["name"], Value::Null);
    }

    #[test]
    fn a_frame_too_short_for_its_declared_header_is_an_error() {
        // Header type 2 declared, but only a type-1 header's worth of bytes.
        let mut raw = vec![0x40, 0];
        raw.extend_from_slice(&[0x11; 16]);
        assert_eq!(
            decode(&raw, 0).unwrap_err(),
            DecodeError::TooShort { got: 18, need: 35 }
        );
    }

    #[test]
    fn an_announce_shorter_than_its_own_context_flag_declares_is_flagged() {
        // Context flag set (ratchet declared) but only a non-ratcheted
        // payload supplied.
        let raw = header1(0x21, 0, [0x00; 16], 0x00, &[0u8; 148]);
        let d = decode(&raw, 1_800_000_000).unwrap();
        assert_eq!(d.value["announce"]["truncated"], true);
        assert_eq!(d.value["announce"]["declared_fixed_len"], 180);
    }

    #[test]
    fn an_unsigned_announce_reports_an_invalid_signature_and_still_decodes() {
        let mut payload = vec![0u8; 148];
        payload[74..79].copy_from_slice(&[1, 2, 3, 4, 5]);
        payload[79..84].copy_from_slice(&[0x00, 0x6B, 0x49, 0xD2, 0x00]); // 1800000000
        let raw = header1(0x01, 0, [0x00; 16], 0x00, &payload);
        let d = decode(&raw, 1_800_000_000).unwrap();
        let a = &d.value["announce"];
        assert_eq!(a["signature_valid"], false);
        assert_eq!(a["random_hash"]["emission_secs"], 1_800_000_000u64);
        assert_eq!(a["random_hash"]["emission_utc"], "2027-01-15T08:00:00Z");
        assert_eq!(a["app_data"]["bytes"], 0);
    }

    #[test]
    fn an_all_zero_key_that_passes_the_permissive_verifier_is_named() {
        // The degenerate signer: an all-zero Ed25519 key and an all-zero
        // signature verify against each other for some messages. The mesh
        // accepts such an announce, so `signature_valid` must say so — and
        // the strict verdict is what tells the operator it proves nothing.
        let mut payload = vec![0u8; 148];
        payload[79..84].copy_from_slice(&[0, 0, 0, 0x01, 0x2C]);
        let raw = header1(0x01, 0, [0xAB; 16], 0x00, &payload);
        let d = decode(&raw, 1_800_000_000).unwrap();
        let a = &d.value["announce"];
        assert_eq!(a["signature_valid"], true, "a peer accepts this announce");
        assert_eq!(a["signature_strict_valid"], false);
        let warnings = d.value["warnings"].as_array().unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap().contains("small-order")),
            "expected the degenerate-signer finding, got {warnings:?}"
        );
    }

    #[test]
    fn an_uptime_seconds_emission_is_named_as_the_155_shape() {
        let mut payload = vec![0u8; 148];
        payload[79..84].copy_from_slice(&[0, 0, 0, 0x01, 0x2C]); // 300 seconds of uptime
        let raw = header1(0x01, 0, [0x00; 16], 0x00, &payload);
        let d = decode(&raw, 1_800_000_000).unwrap();
        assert_eq!(d.value["announce"]["random_hash"]["emission_secs"], 300);
        let warnings = d.value["warnings"].as_array().unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap().contains("#155")),
            "expected the uptime-seconds finding, got {warnings:?}"
        );
    }

    #[test]
    fn a_future_emission_is_named_as_future() {
        let mut payload = vec![0u8; 148];
        payload[79..84].copy_from_slice(&[0x00, 0x6B, 0x49, 0xD2, 0x00]); // 1800000000
        let raw = header1(0x01, 0, [0x00; 16], 0x00, &payload);
        let d = decode(&raw, 1_700_000_000).unwrap();
        let warnings = d.value["warnings"].as_array().unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap().contains("in the future")),
            "expected the future-emission finding, got {warnings:?}"
        );
    }

    #[test]
    fn a_path_request_is_recognised_by_its_well_known_destination() {
        let dest = path_request_destination_hash();
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0x44; 16]); // requested
        payload.extend_from_slice(&[0x55; 16]); // requestor transport id
        payload.extend_from_slice(&[0x66; 16]); // tag
        let raw = header1(0x08, 0, dest, 0x00, &payload);
        let d = decode(&raw, 1_800_000_000).unwrap();
        let pr = &d.value["path_request"];
        assert_eq!(pr["requested_hash"], "44".repeat(16));
        assert_eq!(pr["requestor_transport_id"], "55".repeat(16));
        assert_eq!(pr["tag"], "66".repeat(16));
        assert!(d.value["warnings"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_short_tagged_path_request_decodes_and_is_flagged() {
        let dest = path_request_destination_hash();
        let raw = header1(0x08, 0, dest, 0x00, &[0x44u8; 40]);
        let d = decode(&raw, 1_800_000_000).unwrap();
        assert_eq!(d.value["path_request"]["requested_hash"], "44".repeat(16));
        let warnings = d.value["warnings"].as_array().unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap().contains("conformant layouts")),
            "expected a layout warning, got {warnings:?}"
        );
    }

    #[test]
    fn a_hostile_link_request_mtu_is_decoded_and_named() {
        let mut payload = vec![0u8; 64];
        // 21-bit MTU = 3, mode = 1: below the base MTU a responder may assume.
        payload.extend_from_slice(&[0x20, 0x00, 0x03]);
        let raw = header1(0x02, 0, [0x00; 16], 0x00, &payload);
        let d = decode(&raw, 1_800_000_000).unwrap();
        assert_eq!(d.value["link_request"]["signalling"]["mtu"], 3);
        assert_eq!(d.value["link_request"]["signalling"]["mode"], 1);
        let warnings = d.value["warnings"].as_array().unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap().contains("below the base MTU")),
            "expected an MTU warning, got {warnings:?}"
        );
    }

    #[test]
    fn hex_and_base64_inputs_reach_the_same_bytes() {
        assert_eq!(bytes_from_text("01 02:03-04").unwrap(), vec![1, 2, 3, 4]);
        assert_eq!(bytes_from_text("q83v").unwrap(), vec![0xAB, 0xCD, 0xEF]);
        assert!(bytes_from_text("").is_err());
        assert!(bytes_from_text("!!!").is_err());
    }

    #[test]
    fn utc_formatting_matches_known_instants() {
        assert_eq!(utc_iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_iso8601(1_704_067_200), "2024-01-01T00:00:00Z");
        assert_eq!(utc_iso8601(951_782_400), "2000-02-29T00:00:00Z");
    }
}
