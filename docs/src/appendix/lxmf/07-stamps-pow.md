# Stamps and proof-of-work

Stamps are an anti-spam proof-of-work bound to a message-id (delivery stamps) or
a transient-id (propagation stamps). This section is normative and is proven by
`[VEC-STAMP-1]`. An implementation MUST reproduce the workblock, validity test,
and value computation bit-for-bit, or its stamps will not be accepted by a Python
peer (and vice versa).

## Workblock

```
stamp_workblock(material, expand_rounds):
    workblock = b""
    for n in range(expand_rounds):
        workblock += hkdf(length=256,
                          derive_from=material,
                          salt=full_hash(material || msgpack(n)),
                          context=None)
    return workblock
```

(`LXStamper.py:18-29`). Each round appends 256 bytes, so the workblock is
`expand_rounds * 256` bytes. The salt for round `n` is
`full_hash(material || msgpack(n))`, where `msgpack(n)` is the msgpack encoding of
the integer `n` (`LXStamper.py:24`). The expand-round counts are:

| Context | Rounds | Workblock size | Citation |
|---------|--------|----------------|----------|
| Delivery stamp | `WORKBLOCK_EXPAND_ROUNDS` = 3000 | 768 000 B | `LXStamper.py:10` |
| Propagation stamp | `WORKBLOCK_EXPAND_ROUNDS_PN` = 1000 | 256 000 B | `LXStamper.py:11` |
| Peering key | `WORKBLOCK_EXPAND_ROUNDS_PEERING` = 25 | 6 400 B | `LXStamper.py:12` |

> The 768 kB delivery workblock is held in RAM during both generation and
> validation. On constrained targets this is significant; see the `no_std`
> discussion in the feasibility report.

## Validity

```
stamp_valid(stamp, target_cost, workblock):
    target = 1 << (256 - target_cost)
    return int.from_bytes(full_hash(workblock || stamp), "big") <= target
```

(`LXStamper.py:42-46`). The digest is interpreted as a big-endian 256-bit
integer and compared against `target`. `target_cost` is the number of required
leading zero bits. The stamp itself is 32 random bytes (`STAMP_SIZE`,
`LXStamper.py:13`).

## Value

```
stamp_value(workblock, stamp):
    count leading zero bits of full_hash(workblock || stamp)   # big-endian
```

(`LXStamper.py:31-40`). The value is the achieved number of leading zero bits.

### Proof: `[VEC-STAMP-1]`

For a fixed 32-byte `material`, `expand_rounds = 4`, and `target_cost = 8`, the
harness builds the workblock (1024 bytes = 4 x 256), then deterministically
searches `stamp = full_hash(material || counter_be8)` over increasing `counter`
until `stamp_valid` holds. The vector records the winning counter, the stamp, the
digest, the `target` (`0x0100…00`, i.e. `1 << 248`, one set bit then 248 zero
bits), `valid = true`, and `stamp_value = 8`. The reduced round count keeps the vector cheap to reproduce;
the **algorithm** it pins is identical to the production path, which differs only
in `expand_rounds`.

## Generation

`generate_stamp(material, stamp_cost, expand_rounds)` brute-forces random 32-byte
stamps until `stamp_valid` (`LXStamper.py:92-111`). The reference parallelizes
this across processes on Linux and falls back to single-process elsewhere
(`LXStamper.py:145-354`); the parallelism is informative, the resulting stamp is
not.

## Where stamps are required

- **Delivery stamp**: the recipient advertises a `stamp_cost` in its delivery
  announce (see [Announce application data](09-announce-appdata.md)). The sender
  generates a stamp over the message-id and appends it as payload element `[4]`
  (`LXMessage.py:368-370,317`). The recipient validates it with `validate_stamp`
  (`LXMessage.py:270-291`).
- **Propagation stamp**: generated over the transient-id with
  `WORKBLOCK_EXPAND_ROUNDS_PN` and the node's advertised cost
  (`LXMessage.py:326-350`).
- **Ticket shortcut**: if a valid ticket is held, the stamp is
  `truncated_hash(ticket || message_id)` and the value is `COST_TICKET = 256`,
  bypassing proof-of-work (`LXMessage.py:274-277,296-300`). See
  [Tickets](08-tickets.md).

## Validation order

`validate_stamp(target_cost, tickets)` first tries each held inbound ticket: if
`stamp == truncated_hash(ticket || message_id)` the stamp is accepted with value
`COST_TICKET` (`LXMessage.py:271-277`). Otherwise it builds the workblock over the
message-id and runs `stamp_valid` (`LXMessage.py:284-289`). An implementation
MUST check tickets before proof-of-work to interoperate with ticketed senders.
