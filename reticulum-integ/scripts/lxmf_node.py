#!/usr/bin/env python3
"""LXMF helper for reticulum-integ tests.

One long-running process per node. Started by the runner via:

    docker exec -i <container> python3 -u /opt/integ-scripts/lxmf_node.py [display_name]

Reads commands from stdin (one per line):

    announce
    wait_for_peer <hex> <timeout_secs>
    send <hex> <body_b64>
    quit

Emits structured EVENT lines on stdout (one per line, flushed):

    EVENT lxmf_ready hash=<hex> t=<ms>
    EVENT lxmf_announce_sent hash=<hex> t=<ms>
    EVENT lxmf_wait_for_peer_ok peer=<hex> t=<ms>
    EVENT lxmf_wait_for_peer_timeout peer=<hex> t=<ms>
    EVENT lxmf_msg_sent dst=<hex> body_b64=<b64> t=<ms>
    EVENT lxmf_msg_received src=<hex> body_b64=<b64> sig_valid=<bool> transport_encryption=<str> t=<ms>
    EVENT lxmf_error detail=<str> t=<ms>

Diagnostic / human-readable text goes to stderr.

The helper connects as an RNS shared-instance client to the daemon
running in the container (lnsd or rnsd). RNS_REQUIRE_SHARED=1 is set
on the container env so this fails loudly if the daemon is not
reachable. All LXMF traffic therefore flows through the daemon under
test, exercising its real link / announce / delivery code paths.
"""
import base64
import os
import sys
import threading
import time

import RNS
import LXMF


_print_lock = threading.Lock()


def now_ms() -> int:
    return int(time.monotonic() * 1000)


def emit_event(name: str, **fields) -> None:
    parts = [f"EVENT {name}"]
    for key, value in fields.items():
        parts.append(f"{key}={value}")
    parts.append(f"t={now_ms()}")
    line = " ".join(parts)
    with _print_lock:
        print(line, flush=True)


def log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def main() -> None:
    display_name = sys.argv[1] if len(sys.argv) > 1 else "lxmf-test"
    storagepath = os.environ.get("LXMF_STORAGE", "/tmp/lxmf-state")
    os.makedirs(storagepath, exist_ok=True)

    log(f"[lxmf_node] starting display_name={display_name} storage={storagepath}")

    RNS.Reticulum("/root/.reticulum")
    time.sleep(1)
    log("[lxmf_node] RNS shared-instance client connected")

    identity = RNS.Identity()
    log(f"[lxmf_node] identity {identity.hash.hex()}")

    router = LXMF.LXMRouter(storagepath=storagepath)
    delivery_dest = router.register_delivery_identity(
        identity, display_name=display_name, stamp_cost=0
    )

    def delivery_callback(message) -> None:
        try:
            content = message.content_as_string()
            if isinstance(content, str):
                body_bytes = content.encode("utf-8")
            else:
                body_bytes = bytes(content)
            body_b64 = base64.b64encode(body_bytes).decode("ascii")
        except Exception as exc:
            body_b64 = ""
            log(f"[lxmf_node] callback decode error: {exc}")
        transport_enc = str(getattr(message, "transport_encryption", "")).replace(" ", "_")
        emit_event(
            "lxmf_msg_received",
            src=message.source_hash.hex(),
            body_b64=body_b64,
            sig_valid=str(bool(getattr(message, "signature_validated", False))).lower(),
            transport_encryption=transport_enc,
        )

    router.register_delivery_callback(delivery_callback)

    my_hash = delivery_dest.hash
    emit_event("lxmf_ready", hash=my_hash.hex())

    for raw in sys.stdin:
        line = raw.strip()
        if not line:
            continue
        try:
            handle_command(line, router, delivery_dest, my_hash)
        except Exception as exc:
            detail = str(exc).replace(" ", "_")[:200]
            emit_event("lxmf_error", detail=detail)
            log(f"[lxmf_node] error on '{line}': {exc}")
        if line == "quit":
            break

    log("[lxmf_node] shutting down")


def handle_command(line: str, router, delivery_dest, my_hash: bytes) -> None:
    parts = line.split()
    cmd = parts[0]

    if cmd == "announce":
        router.announce(my_hash)
        emit_event("lxmf_announce_sent", hash=my_hash.hex())
        return

    if cmd == "wait_for_peer":
        if len(parts) < 3:
            raise ValueError("usage: wait_for_peer <hex> <timeout_secs>")
        peer_hash = bytes.fromhex(parts[1])
        timeout = float(parts[2])
        deadline = time.monotonic() + timeout
        path_requested = False
        ok = False
        while time.monotonic() < deadline:
            identity_known = RNS.Identity.recall(peer_hash) is not None
            has_path = RNS.Transport.has_path(peer_hash)
            if identity_known and has_path:
                ok = True
                break
            if identity_known and not has_path and not path_requested:
                RNS.Transport.request_path(peer_hash)
                path_requested = True
            time.sleep(0.2)
        emit_event(
            "lxmf_wait_for_peer_ok" if ok else "lxmf_wait_for_peer_timeout",
            peer=peer_hash.hex(),
        )
        return

    if cmd == "send":
        if len(parts) < 3:
            raise ValueError("usage: send <hex> <body_b64>")
        peer_hash = bytes.fromhex(parts[1])
        body = base64.b64decode(parts[2]).decode("utf-8")
        peer_identity = RNS.Identity.recall(peer_hash)
        if peer_identity is None:
            raise RuntimeError(f"identity for {peer_hash.hex()} not known; call wait_for_peer first")
        peer_dest = RNS.Destination(
            peer_identity,
            RNS.Destination.OUT,
            RNS.Destination.SINGLE,
            "lxmf",
            "delivery",
        )
        lxm = LXMF.LXMessage(
            peer_dest,
            delivery_dest,
            body,
            "test",
            desired_method=LXMF.LXMessage.DIRECT,
            include_ticket=False,
        )
        router.handle_outbound(lxm)
        emit_event("lxmf_msg_sent", dst=peer_hash.hex(), body_b64=parts[2])
        return

    if cmd == "quit":
        emit_event("lxmf_shutdown")
        return

    raise ValueError(f"unknown command: {cmd}")


if __name__ == "__main__":
    main()
