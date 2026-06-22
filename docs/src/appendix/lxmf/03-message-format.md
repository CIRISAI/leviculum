# Message binary format

This section is normative and is proven by `[VEC-MSG-1]`, `[VEC-MSG-2]`, and
`[VEC-MSG-3]`.

## Packed layout

A packed LXMF message is the concatenation (`LXMessage.py:379-383`):

```
destination_hash(16) || source_hash(16) || signature(64) || packed_payload
offset 0              16                 32                96
```

An implementation MUST produce exactly this layout. `packed_payload` is the
msgpack serialization of the payload array (below). The total fixed prefix is 96
bytes.

## Payload array

The payload is a msgpack array (`LXMessage.py:359`):

```
[ timestamp, title, content, fields ]
```

with an optional fifth element `stamp` appended when a stamp is generated
(`LXMessage.py:368-370`); see [Stamps](07-stamps-pow.md).

The msgpack **type discipline is normative** and is a common interop trap:

| Element | msgpack type | Citation |
|---------|--------------|----------|
| `timestamp` | float64 (`f64`), seconds since the Unix epoch | `LXMessage.py:354,359` |
| `title` | binary (`bin`), not string | `LXMessage.py:190-193` |
| `content` | binary (`bin`), not string | `LXMessage.py:199-202` |
| `fields` | map, integer keys (may be empty `{}`) | `LXMessage.py:212-216` |
| `stamp` (optional) | binary (`bin`), 32 bytes | `LXMessage.py:370` |

`title` and `content` are stored and packed as bytes; the `*_as_string`
accessors only decode UTF-8 on demand (`LXMessage.py:196,205`). An implementation
MUST pack them as msgpack `bin`, never `str`. Mismatching this changes the
serialized bytes and therefore the message-id, so a Python peer rejects the
message.

### Proof: annotated `[VEC-MSG-1]` payload

For `timestamp = 1700000000.0`, `title = b"Hi"`, `content = b"Hello"`,
`fields = {}`, the packed payload is `94cb41d954fc40000000c4024869c40548656c6c6f80`:

```
94                     fixarray, 4 elements
cb 41d954fc40000000    float64  = 1700000000.0      (timestamp)
c4 02 4869             bin8 len 2  = "Hi"            (title)
c4 05 48656c6c6f       bin8 len 5  = "Hello"         (content)
80                     fixmap, 0 entries = {}        (fields)
```

The `cb` (float64), `c4` (bin8), and `80` (fixmap) prefixes prove the type
discipline directly. `[VEC-MSG-2]` shows a non-empty `fields` map carrying an
integer key.

## Hashing input (message-id)

The message hash is (`LXMessage.py:361-366`):

```
hashed_part = destination_hash || source_hash || msgpack(payload_without_stamp)
message_id  = full_hash(hashed_part)
```

The payload hashed here MUST NOT include the optional stamp element. On unpack
the reference strips a present stamp before re-hashing (`LXMessage.py:744-747`).
`[VEC-MSG-1]` records `hashed_part_hex` and the resulting `message_id_hex`.

## Signing input

The signature is (`LXMessage.py:372-375`):

```
signed_part = hashed_part || message_id        (= dest || src || msgpack(payload) || message_id)
signature   = source.sign(signed_part)         (Ed25519, 64 bytes)
```

`[VEC-MSG-1]` records `signed_part_hex`, `signature_hex`, and
`signature_valid = true` (verified with `source.identity.validate`).

## Unpack and verification

`unpack_from_bytes` (`LXMessage.py:735-807`) slices at the fixed offsets:
`destination_hash = bytes[0:16]`, `source_hash = bytes[16:32]`,
`signature = bytes[32:96]`, `packed_payload = bytes[96:]`. It unpacks the
payload, and if the array has more than four elements treats element `[4]` as the
stamp and removes it before recomputing the hash (`LXMessage.py:741-747`).

Verification requires the source identity to be known (learned from its
announce). The outcome is one of (`LXMessage.py:790-801`):

- signature valid: `signature_validated = true`;
- signature present but invalid: `unverified_reason = SIGNATURE_INVALID (0x02)`;
- source identity unknown: `unverified_reason = SOURCE_UNKNOWN (0x01)`.

`[VEC-MSG-3]` makes the source identity recallable, unpacks `[VEC-MSG-1]`, and
records `signature_validated = true`, `matches_source = true`, and the recovered
title and content. An implementation MUST reproduce these offsets and the
stamp-stripping rule, or it will compute a different message-id than the sender.
