#!/usr/bin/env python3
"""A live arrival for the bench home (order 1562): the reference LXMF client,
unmodified, on the bench's shared instance, sends one message to the bench
mailbox's address and waits for its proof. The bench M9, a device of that
mailbox, pulls it as it pulls any message its home took in: a real arrival,
through the real path, into a chat of the bench home's, never Lew's.

    PYTHONPATH=reference/Reticulum:reference/LXMF \\
        python3 scripts/m9-bench-send.py --to <32 hex> --text <words> \\
        [--rnsconfig DIR] [--identity FILE] [--timeout S]

scripts/m9-bench-home.sh send is the usual way in. The sender keeps its
identity in --identity (made on first use), so its messages stay in one
conversation at the bench home, and it announces under the name
"Bench sender". Prints one line,

    SENT to=<hex> state=<delivered|failed|timeout> ms=<ms>

and exits 0 iff the message was delivered.
"""

import argparse
import os
import sys
import tempfile
import time

import RNS
import LXMF


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--to", required=True, help="the bench home's LXMF address, 32 hex")
    ap.add_argument("--text", required=True)
    ap.add_argument("--rnsconfig", default=None, help="the shared instance's config folder")
    ap.add_argument("--identity", default=None, help="the sender's identity file, made when missing")
    ap.add_argument("--timeout", type=float, default=120.0)
    ap.add_argument("--address-out", default=None,
                    help="write the sender's LXMF address here, 32 hex (1594: marks.walk's `open peer sender`)")
    a = ap.parse_args()
    to = bytes.fromhex(a.to)
    if len(to) != 16:
        ap.error("--to is 32 hex characters")

    RNS.Reticulum(configdir=a.rnsconfig, loglevel=RNS.LOG_WARNING)
    if a.identity and os.path.exists(a.identity):
        identity = RNS.Identity.from_file(a.identity)
    else:
        identity = RNS.Identity()
        if a.identity:
            os.makedirs(os.path.dirname(os.path.abspath(a.identity)), exist_ok=True)
            identity.to_file(a.identity)
    storage = tempfile.mkdtemp(prefix="m9-bench-send-")
    router = LXMF.LXMRouter(identity=identity, storagepath=storage)
    source = router.register_delivery_identity(identity, display_name="Bench sender")
    if a.address_out:
        with open(a.address_out, "w") as f:
            f.write(source.hash.hex() + "\n")
    router.announce(source.hash)

    t0 = time.monotonic()
    if not RNS.Transport.has_path(to):
        RNS.Transport.request_path(to)
        while not RNS.Transport.has_path(to) and time.monotonic() - t0 < a.timeout:
            time.sleep(0.2)
    recipient = RNS.Identity.recall(to)
    if recipient is None:
        print(f"SENT to={a.to} state=timeout ms={int((time.monotonic() - t0) * 1000)} why=no-path")
        return 1
    destination = RNS.Destination(recipient, RNS.Destination.OUT, RNS.Destination.SINGLE, "lxmf", "delivery")
    message = LXMF.LXMessage(destination, source, a.text, desired_method=LXMF.LXMessage.DIRECT)
    router.handle_outbound(message)
    while time.monotonic() - t0 < a.timeout:
        if message.state == LXMF.LXMessage.DELIVERED:
            print(f"SENT to={a.to} state=delivered ms={int((time.monotonic() - t0) * 1000)}")
            return 0
        if message.state == LXMF.LXMessage.FAILED:
            break
        time.sleep(0.2)
    state = "failed" if message.state == LXMF.LXMessage.FAILED else "timeout"
    print(f"SENT to={a.to} state={state} ms={int((time.monotonic() - t0) * 1000)}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
