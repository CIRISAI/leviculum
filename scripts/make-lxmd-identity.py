#!/usr/bin/env python3
"""Write an `lxmd` identity file and print the hashes it implies.

`lxmd` keeps its primary identity at `<configdir>/identity` in RNS's own
`Identity.to_file` form (`program_setup`,
`reference/LXMF/LXMF/Utilities/lxmd.py:337/389`), and `lnpnd` keeps its at
the same path in the same form. Whether a copied file gives the same node
address is therefore a question about the derivation, and this script is one
half of the measurement: Python mints the identity and computes the hashes,
Rust loads the same file and has to agree.

    python3 scripts/make-lxmd-identity.py <configdir>

Prints:

    <identity file path>
    <lxmf.propagation destination hash, hex>
    <lxmf.delivery destination hash, hex>
"""

import os
import sys

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
except ImportError:
    print(
        "ERROR: RNS not found. Run "
        "`git submodule update --init reference/Reticulum`, "
        "or set RETICULUM_PATH.",
        file=sys.stderr,
    )
    sys.exit(1)


def main():
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    configdir = os.path.abspath(sys.argv[1])
    os.makedirs(configdir, exist_ok=True)
    identitypath = os.path.join(configdir, "identity")

    identity = RNS.Identity()
    identity.to_file(identitypath)

    # The two destinations lxmd builds from that identity (`program_setup`,
    # lxmd.py:398 hands it to LXMRouter, which registers the delivery
    # destination and, with the node enabled, the propagation one). Hashed
    # through the reference's own static derivation (`Destination.hash`,
    # `reference/Reticulum/RNS/Destination.py:116`) rather than by
    # constructing Destinations, which would need a running Transport.
    propagation = RNS.Destination.hash(identity, "lxmf", "propagation")
    delivery = RNS.Destination.hash(identity, "lxmf", "delivery")

    print(identitypath)
    print(RNS.hexrep(propagation, delimit=False))
    print(RNS.hexrep(delivery, delimit=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
