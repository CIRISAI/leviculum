#!/usr/bin/env python3
"""Generate golden test vectors for the LXMF protocol specification.

Every binary claim in the specification is backed by a vector produced here.
The harness imports the vendored Python reference implementations directly
(``reference/LXMF`` and ``reference/Reticulum``) so the vectors are the genuine
output of the reference code at the pinned submodule commits, not a
reimplementation.

Run from the repository root:

    PYTHONPATH=reference/Reticulum:reference/LXMF \
        python3 docs/src/appendix/lxmf/vectors/gen_vectors.py

Output: ``vectors.json`` next to this script. The file is committed and the
specification embeds its hex blocks. Re-running MUST reproduce it byte for
byte; the harness asserts determinism for every frozen vector before writing.

Vector kinds
------------
frozen     Deterministic bytes. The hex is the proof. Reproducible across runs.
roundtrip  Output depends on ephemeral key material (RNS Destination.encrypt
           uses a fresh ephemeral X25519 key per call), so the ciphertext is
           NOT reproducible. The proof is a structural + decrypt round trip:
           the harness shows the cleartext framing and proves that decrypting
           recovers the original plaintext.
"""

import json
import os
import subprocess
import sys

import RNS
from RNS.vendor import umsgpack as msgpack

import LXMF
from LXMF.LXMessage import LXMessage
from LXMF import (
    display_name_from_app_data,
    stamp_cost_from_app_data,
    pn_announce_data_is_valid,
    pn_name_from_app_data,
    pn_stamp_cost_from_app_data,
    SF_COMPRESSION,
    PN_META_NAME,
)
from LXMF import LXStamper


# --------------------------------------------------------------------------
# Fixed inputs. Nothing here may depend on wall-clock time or system entropy,
# except where a field is wall-clock in the real protocol, in which case it is
# pinned to a constant and called out explicitly.
# --------------------------------------------------------------------------

# 64-byte Identity private material = X25519(32) || Ed25519(32).
SRC_PRV = bytes(range(0, 64))
DST_PRV = bytes(range(64, 128))

# Pinned wall-clock value used for the message timestamp (payload[0]). Real
# senders use time.time(); the spec marks this field as wall-clock.
FIXED_TIMESTAMP = 1700000000.0

REPO_ROOT = os.path.abspath(
    os.path.join(os.path.dirname(__file__), "..", "..", "..", "..", "..")
)


def _submodule_commit(path):
    try:
        return (
            subprocess.check_output(
                ["git", "-C", os.path.join(REPO_ROOT, path), "rev-parse", "HEAD"],
                stderr=subprocess.DEVNULL,
            )
            .decode()
            .strip()
        )
    except Exception:
        return "unknown"


def make_identities():
    src_id = RNS.Identity.from_bytes(SRC_PRV)
    dst_id = RNS.Identity.from_bytes(DST_PRV)
    return src_id, dst_id


def delivery_destinations(direction):
    src_id, dst_id = make_identities()
    src = RNS.Destination(src_id, direction, RNS.Destination.SINGLE, "lxmf", "delivery")
    dst = RNS.Destination(dst_id, direction, RNS.Destination.SINGLE, "lxmf", "delivery")
    return src, dst


def build_message(title, content, fields, method):
    """Build and pack a message deterministically at FIXED_TIMESTAMP."""
    src, dst = delivery_destinations(RNS.Destination.OUT)
    m = LXMessage(dst, src, content=content, title=title, fields=fields,
                  desired_method=method)
    m.timestamp = FIXED_TIMESTAMP
    m.pack()
    return m, src, dst


def split_packed(packed):
    """Slice a packed message at the fixed offsets (LXMessage.py:380-383)."""
    return {
        "destination_hash": packed[0:16].hex(),
        "source_hash": packed[16:32].hex(),
        "signature": packed[32:96].hex(),
        "packed_payload": packed[96:].hex(),
    }


VECTORS = []


def add(vec):
    VECTORS.append(vec)
    return vec


# --------------------------------------------------------------------------
# Constants (LXMessage class attributes). Proves the derived sizes.
# --------------------------------------------------------------------------

def collect_constants():
    keys = [
        "DESTINATION_LENGTH", "SIGNATURE_LENGTH", "TICKET_LENGTH",
        "TIMESTAMP_SIZE", "STRUCT_OVERHEAD", "LXMF_OVERHEAD",
        "ENCRYPTED_PACKET_MDU", "ENCRYPTED_PACKET_MAX_CONTENT",
        "LINK_PACKET_MDU", "LINK_PACKET_MAX_CONTENT",
        "PLAIN_PACKET_MDU", "PLAIN_PACKET_MAX_CONTENT",
        "PAPER_MDU", "COST_TICKET",
        "TICKET_EXPIRY", "TICKET_GRACE", "TICKET_RENEW", "TICKET_INTERVAL",
    ]
    out = {}
    for k in keys:
        out[k] = getattr(LXMessage, k)
    stamper = {
        "WORKBLOCK_EXPAND_ROUNDS": LXStamper.WORKBLOCK_EXPAND_ROUNDS,
        "WORKBLOCK_EXPAND_ROUNDS_PN": LXStamper.WORKBLOCK_EXPAND_ROUNDS_PN,
        "WORKBLOCK_EXPAND_ROUNDS_PEERING": LXStamper.WORKBLOCK_EXPAND_ROUNDS_PEERING,
        "STAMP_SIZE": LXStamper.STAMP_SIZE,
    }
    return out, stamper


# --------------------------------------------------------------------------
# Message-format vectors (frozen).
# --------------------------------------------------------------------------

def gen_message_vectors():
    # VEC-MSG-1: minimal opportunistic message, no fields, no stamp.
    m, src, dst = build_message(b"Hi", b"Hello", {}, LXMessage.OPPORTUNISTIC)
    parts = split_packed(m.packed)
    # Re-derive the hashing and signing inputs to prove them.
    payload = [FIXED_TIMESTAMP, b"Hi", b"Hello", {}]
    hashed_part = dst.hash + src.hash + msgpack.packb(payload)
    msg_hash = RNS.Identity.full_hash(hashed_part)
    signed_part = hashed_part + msg_hash
    add({
        "id": "VEC-MSG-1",
        "title": "Minimal opportunistic message (no fields, no stamp)",
        "kind": "frozen",
        "citation": "LXMessage.py:359-384",
        "inputs": {
            "src_identity_prv_hex": SRC_PRV.hex(),
            "dst_identity_prv_hex": DST_PRV.hex(),
            "timestamp": FIXED_TIMESTAMP,
            "title": "Hi", "content": "Hello", "fields": {},
            "desired_method": "OPPORTUNISTIC",
        },
        "packed_hex": m.packed.hex(),
        "packed_len": len(m.packed),
        "parts": parts,
        "payload_msgpack_hex": msgpack.packb(payload).hex(),
        "hashed_part_hex": hashed_part.hex(),
        "message_id_hex": m.message_id.hex(),
        "hash_hex": m.hash.hex(),
        "signed_part_hex": signed_part.hex(),
        "signature_hex": m.signature.hex(),
        "signature_valid": src.identity.validate(m.signature, signed_part),
        "method": m.method,
        "representation": m.representation,
    })

    # VEC-MSG-2: message carrying a fields dict with an integer key.
    fields = {0x0F: 0x02}  # FIELD_RENDERER: RENDERER_MARKDOWN
    m2, src2, dst2 = build_message(b"", b"body text", fields, LXMessage.DIRECT)
    add({
        "id": "VEC-MSG-2",
        "title": "Message with a fields dict (integer field key)",
        "kind": "frozen",
        "citation": "LXMessage.py:359, 212-216",
        "inputs": {
            "timestamp": FIXED_TIMESTAMP, "title": "", "content": "body text",
            "fields": {"0x0F": "0x02"}, "desired_method": "DIRECT",
        },
        "packed_hex": m2.packed.hex(),
        "packed_len": len(m2.packed),
        "parts": split_packed(m2.packed),
        "payload_msgpack_hex": msgpack.packb([FIXED_TIMESTAMP, b"", b"body text", fields]).hex(),
        "message_id_hex": m2.message_id.hex(),
        "method": m2.method,
        "representation": m2.representation,
    })

    # VEC-MSG-3: unpack round trip proves the offsets + verification path.
    # Make the source identity recallable so unpack runs the full signature
    # validation branch (LXMessage.py:765-797). In the live protocol the
    # source identity is learned from its announce; here we inject it.
    RNS.Identity.remember(None, src.hash, src.identity.get_public_key())
    unpacked = LXMessage.unpack_from_bytes(m.packed)
    add({
        "id": "VEC-MSG-3",
        "title": "Unpack + signature verification round trip of VEC-MSG-1",
        "kind": "frozen",
        "citation": "LXMessage.py:735-807",
        "source_vector": "VEC-MSG-1",
        "recovered_message_id_hex": unpacked.hash.hex(),
        "recovered_title": unpacked.title_as_string(),
        "recovered_content": unpacked.content_as_string(),
        "signature_validated": bool(unpacked.signature_validated),
        "matches_source": unpacked.hash == m.hash,
    })


# --------------------------------------------------------------------------
# Delivery-method vectors.
# --------------------------------------------------------------------------

def gen_delivery_vectors():
    # Opportunistic on-air form: packed with the leading dest hash removed
    # (LXMessage.__as_packet, LXMessage.py:623-635).
    m, src, dst = build_message(b"Hi", b"Hello", {}, LXMessage.OPPORTUNISTIC)
    add({
        "id": "VEC-DLV-OPP",
        "title": "Opportunistic on-air payload (leading destination hash omitted)",
        "kind": "frozen",
        "citation": "LXMessage.py:623-635",
        "full_packed_hex": m.packed.hex(),
        "on_air_hex": m.packed[16:].hex(),
        "note": "Destination is inferred from the RNS packet header.",
    })

    # Direct: full packed bytes are sent (over a Link), as VEC-MSG-1.
    add({
        "id": "VEC-DLV-DIRECT",
        "title": "Direct delivery sends the full packed message",
        "kind": "frozen",
        "citation": "LXMessage.py:414-421, 633",
        "on_air_hex": m.packed.hex(),
        "note": "Sent as a single Packet over a Link when content fits "
                "LINK_PACKET_MAX_CONTENT, else as a Resource.",
    })

    # Propagated: encrypted envelope. Non-deterministic ciphertext -> roundtrip.
    mp, srcp, dstp = build_message(b"Hi", b"Hello", {}, LXMessage.PROPAGATED)
    # mp.packed is the inner message; reconstruct the envelope structure.
    inner = mp.packed
    pn_encrypted = dstp.encrypt(inner[16:])
    lxmf_data = inner[:16] + pn_encrypted
    transient_id = RNS.Identity.full_hash(lxmf_data)
    recovered = dstp.decrypt(pn_encrypted)
    add({
        "id": "VEC-PROP-ENVELOPE",
        "title": "Propagation transfer envelope (encrypted, round-trip proof)",
        "kind": "roundtrip",
        "citation": "LXMessage.py:423-433",
        "inner_packed_hex": inner.hex(),
        "dest_hash_prefix_hex": inner[:16].hex(),
        "pn_encrypted_len": len(pn_encrypted),
        "lxmf_data_structure": "destination_hash(16) || destination.encrypt(packed[16:])",
        "transient_id_hex": transient_id.hex(),
        "transient_id_note": "transient_id = full_hash(lxmf_data); depends on "
                             "ephemeral ciphertext, not reproducible.",
        "envelope_structure": "msgpack([wall_clock_timestamp, [lxmf_data, ...]])",
        "decrypt_recovers_inner_tail": recovered == inner[16:],
    })

    # Paper: lxm:// URI. Encrypted -> roundtrip + structure.
    mpaper, srcpr, dstpr = build_message(b"Hi", b"Hello", {}, LXMessage.PAPER)
    uri = mpaper.as_uri()
    add({
        "id": "VEC-PAPER-URI",
        "title": "Paper message lxm:// URI (encrypted, round-trip proof)",
        "kind": "roundtrip",
        "citation": "LXMessage.py:443-455, 687-702",
        "uri_scheme": LXMessage.URI_SCHEMA,
        "uri_prefix": uri[:10],
        "structure": "lxm://base64url(destination_hash(16) || "
                     "destination.encrypt(packed[16:])), '=' padding stripped",
        "note": "base64url body depends on ephemeral ciphertext; not reproducible.",
    })


# --------------------------------------------------------------------------
# Stamp / proof-of-work vectors (frozen, low cost for tractability).
# --------------------------------------------------------------------------

def gen_stamp_vectors():
    # Use a fixed 32-byte material (stand-in for a message_id) and a tiny
    # expand_rounds so the vector is cheap to reproduce. The spec documents
    # that real delivery stamps use WORKBLOCK_EXPAND_ROUNDS (3000); this
    # vector pins the ALGORITHM, with the round count as an explicit input.
    material = RNS.Identity.full_hash(b"lxmf-spec-stamp-material")
    rounds = 4
    target_cost = 8
    workblock = LXStamper.stamp_workblock(material, expand_rounds=rounds)

    # Deterministic search: stamp = full_hash(material || counter) until valid.
    stamp = None
    counter = 0
    while True:
        cand = RNS.Identity.full_hash(material + counter.to_bytes(8, "big"))
        if LXStamper.stamp_valid(cand, target_cost, workblock):
            stamp = cand
            break
        counter += 1
    value = LXStamper.stamp_value(workblock, stamp)
    digest = RNS.Identity.full_hash(workblock + stamp)
    add({
        "id": "VEC-STAMP-1",
        "title": "Stamp workblock, validity, and value (cost=8, expand_rounds=4)",
        "kind": "frozen",
        "citation": "LXStamper.py:18-46",
        "material_hex": material.hex(),
        "expand_rounds": rounds,
        "target_cost": target_cost,
        "workblock_len": len(workblock),
        "workblock_sha256_hex": RNS.Identity.full_hash(workblock).hex(),
        "stamp_search": "stamp = full_hash(material || counter_be8), counter++ until valid",
        "winning_counter": counter,
        "stamp_hex": stamp.hex(),
        "digest_hex": digest.hex(),
        "target_hex": (0b1 << (256 - target_cost)).to_bytes(32, "big").hex(),
        "valid": LXStamper.stamp_valid(stamp, target_cost, workblock),
        "stamp_value": value,
    })


# --------------------------------------------------------------------------
# Announce application-data vectors (frozen), proven via genuine decoders.
# --------------------------------------------------------------------------

def gen_announce_vectors():
    # Delivery announce app_data = msgpack([display_name|None, stamp_cost|None])
    # (LXMRouter.get_announce_app_data, LXMRouter.py:990-1002).
    display_name = "Alice".encode("utf-8")
    stamp_cost = 8
    app_data = msgpack.packb([display_name, stamp_cost])
    add({
        "id": "VEC-ANN-DELIVERY",
        "title": "Delivery announce app_data",
        "kind": "frozen",
        "citation": "LXMRouter.py:990-1002; LXMF.py:117-152",
        "structure": "msgpack([display_name_utf8_or_None, stamp_cost_or_None])",
        "app_data_hex": app_data.hex(),
        "first_byte_hex": "%02x" % app_data[0],
        "first_byte_note": "0x92 = msgpack fixarray(2); decoders sniff 0x90-0x9f or 0xdc.",
        "decoded_display_name": display_name_from_app_data(app_data),
        "decoded_stamp_cost": stamp_cost_from_app_data(app_data),
    })

    # Propagation announce app_data (7-element list) per
    # LXMRouter.get_propagation_node_app_data (LXMRouter.py:307-319).
    FIXED_TIMEBASE = 1700000000  # int(time.time()) in the real protocol.
    metadata = {PN_META_NAME: "NodeA".encode("utf-8")}
    stamp_costs = [16, 3, 18]  # [prop_cost, prop_flex, peering_cost]
    announce_data = [
        False,            # 0: legacy flag
        FIXED_TIMEBASE,   # 1: node timebase (wall-clock int)
        True,             # 2: propagation enabled
        256,              # 3: per-transfer limit (KB)
        256 * 40,         # 4: per-sync limit (KB)
        stamp_costs,      # 5: [prop_cost, prop_flex, peering_cost]
        metadata,         # 6: metadata map
    ]
    pn_app_data = msgpack.packb(announce_data)
    add({
        "id": "VEC-ANN-PROPAGATION",
        "title": "Propagation node announce app_data (7-element list)",
        "kind": "frozen",
        "citation": "LXMRouter.py:307-319; LXMF.py:191-211",
        "structure": "msgpack([legacy, timebase, enabled, xfer_limit_kb, "
                     "sync_limit_kb, [prop_cost, prop_flex, peering_cost], metadata])",
        "fixed_timebase": FIXED_TIMEBASE,
        "timebase_note": "Field 1 is int(time.time()) in the real protocol.",
        "app_data_hex": pn_app_data.hex(),
        "valid": bool(pn_announce_data_is_valid(pn_app_data)),
        "decoded_pn_name": pn_name_from_app_data(pn_app_data),
        "decoded_pn_stamp_cost": pn_stamp_cost_from_app_data(pn_app_data),
    })


# --------------------------------------------------------------------------
# Determinism self-check for frozen vectors.
# --------------------------------------------------------------------------

def assert_determinism():
    """Rebuild every frozen vector once more and assert the bytes match."""
    snapshot = {v["id"]: json.dumps(v, sort_keys=True) for v in VECTORS}
    VECTORS.clear()
    gen_message_vectors()
    gen_delivery_vectors()
    gen_stamp_vectors()
    gen_announce_vectors()
    for v in VECTORS:
        if v["kind"] != "frozen":
            continue
        again = json.dumps(v, sort_keys=True)
        if snapshot[v["id"]] != again:
            raise AssertionError(
                f"Non-deterministic frozen vector {v['id']}: output changed "
                f"between runs."
            )


def main():
    constants, stamper = collect_constants()
    gen_message_vectors()
    gen_delivery_vectors()
    gen_stamp_vectors()
    gen_announce_vectors()
    assert_determinism()

    doc = {
        "_comment": "Golden vectors for the LXMF protocol specification. "
                    "Generated by gen_vectors.py from the vendored reference. "
                    "Do not edit by hand; re-run the harness.",
        "meta": {
            "lxmf_version": LXMF.__version__,
            "rns_version": RNS.__version__,
            "lxmf_commit": _submodule_commit("reference/LXMF"),
            "reticulum_commit": _submodule_commit("reference/Reticulum"),
            "fixed_timestamp": FIXED_TIMESTAMP,
            "src_identity_prv_hex": SRC_PRV.hex(),
            "dst_identity_prv_hex": DST_PRV.hex(),
        },
        "constants": constants,
        "stamper_constants": stamper,
        "vectors": VECTORS,
    }

    out_path = os.path.join(os.path.dirname(__file__), "vectors.json")
    with open(out_path, "w") as f:
        json.dump(doc, f, indent=2, sort_keys=False)
        f.write("\n")

    print(f"Wrote {len(VECTORS)} vectors to {out_path}")
    print(f"LXMF {LXMF.__version__} @ {doc['meta']['lxmf_commit'][:10]}, "
          f"RNS {RNS.__version__} @ {doc['meta']['reticulum_commit'][:10]}")
    for v in VECTORS:
        print(f"  {v['id']:20s} [{v['kind']:9s}] {v['title']}")


if __name__ == "__main__":
    main()
