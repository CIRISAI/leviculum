# Announce application data

LXMF carries application data in Reticulum announces. There are two formats: the
delivery announce (sent by a normal LXMF destination) and the propagation-node
announce. Both are normative and proven by `[VEC-ANN-DELIVERY]` and
`[VEC-ANN-PROPAGATION]`.

## Delivery announce

The delivery announce app_data is (`LXMRouter.py:990-1002`):

```
msgpack([ display_name, stamp_cost ])
```

- `display_name`: the UTF-8 encoded display name as `bin`, or `None`
  (`LXMRouter.py:991-992`).
- `stamp_cost`: an integer in `(0, 255)`, or `None`
  (`LXMRouter.py:994-997`).

### Format detection

The decoders distinguish this version-0.5.0+ format from the legacy format
(a bare UTF-8 display name) by sniffing the first byte: it is the new format iff
`app_data[0]` is in `0x90..0x9f` (msgpack fixarray) or equals `0xdc` (array16)
(`LXMF.py:122,145`). An implementation MUST emit a msgpack array so this sniff
succeeds; a two-element array begins with `0x92`.

### Proof: `[VEC-ANN-DELIVERY]`

`msgpack([b"Alice", 8])` produces app_data with `first_byte = 0x92`. The genuine
decoders recover `display_name = "Alice"` (`display_name_from_app_data`,
`LXMF.py:117-139`) and `stamp_cost = 8` (`stamp_cost_from_app_data`,
`LXMF.py:141-152`).

## Propagation-node announce

The propagation announce app_data is a 7-element list
(`LXMRouter.py:307-319`):

```
msgpack([
  legacy_flag,              # 0: bool, legacy LXMF PN support
  timebase,                 # 1: int, int(time.time())
  propagation_enabled,      # 2: bool
  per_transfer_limit_kb,    # 3: int
  per_sync_limit_kb,        # 4: int
  [prop_cost, prop_flex, peering_cost],   # 5: list of three ints
  metadata,                 # 6: dict (PN_META_* keys)
])
```

### Validity

`pn_announce_data_is_valid` (`LXMF.py:191-217`) requires: `data` decodes to a
list of length `>= 7`; `data[1]` (timebase), `data[3]`, `data[4]` are integer-
coercible; `data[2]` is strictly `True` or `False`; `data[5]` is a list whose
first three elements are integer-coercible; and `data[6]` is a dict. An
implementation MUST satisfy all of these for its propagation announce to be
accepted.

### Metadata map (`LXMF.py:98-104`)

Keys: `PN_META_VERSION` (0x00), `PN_META_NAME` (0x01), `PN_META_SYNC_STRATUM`
(0x02), `PN_META_SYNC_THROTTLE` (0x03), `PN_META_AUTH_BAND` (0x04),
`PN_META_UTIL_PRESSURE` (0x05), `PN_META_CUSTOM` (0xFF). The node name is
`metadata[PN_META_NAME]` as UTF-8 bytes (`LXMRouter.py:304`).

### Proof: `[VEC-ANN-PROPAGATION]`

The vector builds the 7-element list with `metadata = {PN_META_NAME: b"NodeA"}`,
`stamp_costs = [16, 3, 18]`, and a fixed timebase, then proves with the genuine
helpers: `pn_announce_data_is_valid = true`, `pn_name_from_app_data = "NodeA"`
(`LXMF.py:169-180`), and `pn_stamp_cost_from_app_data = 16` (`LXMF.py:182-189`).
Field 1 (timebase) is `int(time.time())` in the real protocol and is pinned to a
constant in the vector.
