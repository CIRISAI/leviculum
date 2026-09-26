#!/usr/bin/env python3
# Regenerate the `/status` bundle golden embedded in
# src/status_bundle.rs from the vendored Python reference's own msgpack
# packer. Run from the repo root:
#
#   PYTHONPATH=reference/Reticulum python3 \
#       leviculum-core/tests/status_bundle_golden_gen.py
#
# The dict below is the subset of `Reticulum.get_interface_stats()`
# (reference/Reticulum/RNS/Reticulum.py:1326-1515) that a board can answer
# honestly, in the reference's own key order, filled with the fixture
# `status_bundle.rs::tests::fixture` uses. Byte equality against
# `umsgpack.packb` of this dict is what the unit test asserts.

from RNS.vendor import umsgpack as msgpack

MODE_GATEWAY = 0x06


def iface(name, status, mode, clients, rxb, txb, rxs, txs, bitrate):
    # Key order as Reticulum.get_interface_stats builds it: the optional
    # clients/bitrate/rxs/txs block first, then the unconditional tail.
    return {
        "clients": clients,
        "bitrate": bitrate,
        "rxs": rxs,
        "txs": txs,
        "name": name,
        "rxb": rxb,
        "txb": txb,
        "status": status,
        "mode": mode,
    }


INTERFACES = [
    iface("serial_usb", True, MODE_GATEWAY, None, 4096, 1234, 0.0, 0.0, 1000000),
    iface("lora_sx1262", True, MODE_GATEWAY, None, 98765, 43210, 12.5, 3.25, 3125),
    iface("ble", False, MODE_GATEWAY, 2, 0, 0, 0.0, 0.0, None),
]

TRANSPORT_ID = bytes.fromhex("00112233445566778899aabbccddeeff")
UPTIME = 3600.5

stats = {
    "interfaces": INTERFACES,
    "rxb": sum(i["rxb"] for i in INTERFACES),
    "txb": sum(i["txb"] for i in INTERFACES),
    "rxs": sum(i["rxs"] for i in INTERFACES),
    "txs": sum(i["txs"] for i in INTERFACES),
    "transport_id": TRANSPORT_ID,
    "transport_uptime": UPTIME,
}

packed = msgpack.packb(stats)
print(packed.hex())

# And the round trip the reference tool performs on the wire: the response
# list rnstatus unpacks as `[stats]` / `[stats, link_count]`.
print(msgpack.packb([stats]).hex())
print(msgpack.packb([stats, 3]).hex())
