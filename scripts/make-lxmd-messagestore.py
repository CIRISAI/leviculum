#!/usr/bin/env python3
"""Produce a propagation-node message store with the reference's own code.

Writes `<outdir>/storage/messagestore/` exactly as Python `lxmd` leaves it,
so `lnpnd`'s store can be pointed at a directory nobody in this repo wrote
by hand.  A fixture we invent only proves we can read our own idea of the
format; this one is written by `LXMRouter.lxmf_propagation`, which is the
function `lxmd` runs and the only place the reference ever creates a store
file (`reference/LXMF/LXMF/LXMRouter.py:2512-2515`).

    python3 scripts/make-lxmd-messagestore.py <outdir>

The Reticulum instance it starts has no interfaces, so nothing reaches a
network.  Prints one `<transient_id_hex> <stamp_value> <size>` line per
message written, then the store directory.
"""

import os
import sys
import time

# The vendored reference trees, located the way `scripts/test_daemon.py`
# does it, so the fixture is pinned to the submodule the Rust stack ports
# from rather than to whatever RNS/LXMF a host happens to have installed.
_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
for _var, _rel, _pkg in (
    ("RETICULUM_PATH", "reference/Reticulum", "RNS"),
    ("LXMF_PATH", "reference/LXMF", "LXMF"),
):
    _path = os.environ.get(_var) or os.path.join(_ROOT, _rel)
    if os.path.isdir(os.path.join(_path, _pkg)):
        sys.path.insert(0, _path)

try:
    import RNS
    import LXMF
except ImportError:
    print(
        "ERROR: RNS/LXMF not found. Run "
        "`git submodule update --init reference/Reticulum reference/LXMF`, "
        "or set RETICULUM_PATH / LXMF_PATH.",
        file=sys.stderr,
    )
    sys.exit(1)
import RNS.vendor.umsgpack as umsgpack
from LXMF import LXMessage
import LXMF.LXStamper as LXStamper


# How many messages the fixture holds.  Their stamp values are NOT chosen
# here: the reference computes a value from the mined stamp
# (`LXStamper.stamp_value`), and at target cost 0 the values that come out
# are small and varied -- including 0, which is the case that matters,
# because the reference omits the value component of the filename entirely
# at 0 (`value_component`, LXMRouter.py:2513) and then skips such
# two-component names when it re-indexes its own store
# (`enable_propagation`, :568 requires three components).  Keep mining
# until both a zero-valued and a non-zero-valued message are in the store,
# so the fixture carries the quirk rather than hiding it.
MESSAGE_COUNT = 4

def main():
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    outdir = os.path.abspath(sys.argv[1])
    rnsdir = os.path.join(outdir, "reticulum")
    lxmdir = os.path.join(outdir, "storage")
    os.makedirs(rnsdir, exist_ok=True)
    os.makedirs(lxmdir, exist_ok=True)

    # A config with an empty [interfaces] section: a transport instance
    # that is on no medium at all.  The store is written locally, so the
    # message never needs to reach anyone.
    with open(os.path.join(rnsdir, "config"), "w") as fh:
        fh.write("[reticulum]\n  enable_transport = No\n  share_instance = No\n\n[interfaces]\n")

    RNS.Reticulum(configdir=rnsdir, loglevel=2)

    router = LXMF.LXMRouter(storagepath=lxmdir, enforce_stamps=False)
    router.enable_propagation()

    # Sender and recipient are both strangers to this node: the recipient
    # must NOT be one of the router's delivery destinations, or
    # lxmf_propagation delivers locally instead of storing
    # (LXMRouter.py:2502-2509).
    source_identity = RNS.Identity()
    source = RNS.Destination(
        source_identity, RNS.Destination.IN, RNS.Destination.SINGLE, "lxmf", "delivery"
    )
    recipient_identity = RNS.Identity()
    recipient = RNS.Destination(
        recipient_identity, RNS.Destination.OUT, RNS.Destination.SINGLE, "lxmf", "delivery"
    )

    written = []
    seen_zero = False
    seen_nonzero = False
    index = 0
    while len(written) < MESSAGE_COUNT or not (seen_zero and seen_nonzero):
        index += 1
        message = LXMessage(
            recipient,
            source,
            content=f"preflight message {index}",
            title="preflight",
            desired_method=LXMessage.PROPAGATED,
        )
        message.pack()
        # A real propagation stamp, mined by the reference at target cost 0
        # (LXMessage.py:329-352).  Re-packing folds it into
        # `propagation_packed`, which is the msgpack envelope a client
        # uploads: `[timestamp, [lxmf_data + stamp]]` (LXMessage.py:433-436).
        message.get_propagation_stamp(0)
        message.packed = None
        message.pack()
        transient_data = umsgpack.unpackb(message.propagation_packed)[1][0]

        # The reference's own ingest chain, both halves: the stamp validator
        # the network path runs (`propagation_packet`, LXMRouter.py:2243-2249)
        # and then the store write.  Nothing about the file is assembled here.
        validated = LXStamper.validate_pn_stamps([transient_data], 0)
        if not validated:
            print("the reference refused its own stamp", file=sys.stderr)
            return 1
        transient_id, lxmf_data, value, stamp_data = validated[0]
        if value == 0 and seen_zero and len(written) >= MESSAGE_COUNT:
            continue
        if not router.lxmf_propagation(
            lxmf_data, stamp_value=value, stamp_data=stamp_data
        ):
            print(f"lxmf_propagation refused the message at stamp value {value}", file=sys.stderr)
            return 1
        seen_zero |= value == 0
        seen_nonzero |= value > 0
        written.append((transient_id, value, len(lxmf_data) + len(stamp_data)))
        # Distinct receive timestamps: the reference's filename carries
        # `time.time()` and two messages in the same microsecond would be a
        # fixture that hides a collision rather than exercising one.
        time.sleep(0.01)

    for transient_id, value, size in written:
        print(f"{RNS.hexrep(transient_id, delimit=False)} {value} {size}")
    print(router.messagepath)
    RNS.Reticulum.exit_handler()
    return 0


if __name__ == "__main__":
    sys.exit(main())
