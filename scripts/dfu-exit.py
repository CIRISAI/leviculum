#!/usr/bin/env python3
"""Bring an nRF52 LNode out of its bootloader's BLE DFU mode, without hands.

OTA stage 1 (docs/src/concepts/ota-stage-1-ble-dfu.md) puts a board into
the Adafruit bootloader's BLE DFU mode on a mesh command (`lnmgmt dfu`).
The bootloader never leaves that mode by itself: the T114 advertised for
61.5 minutes on the 2026-11-10 bench and would have gone on. Nobody has to
walk up with a needle, though. The bootloader speaks Nordic Legacy DFU, and
that protocol's SYS_RESET opcode (0x06) written to the DFU Control Point
resets the board; with no start packet sent, the application is still
valid and comes back (1.5 s on the bench).

What this does, host side, with BlueZ's own tools:

1. find the board: the bootloader advertises a name ending in `-OTA`
   (`HT-n5262-OTA` on the T114; no service UUID in the advertisement, the
   DFU service 1530 is visible only in GATT) from the application's BLE
   address with its lowest byte plus one. Give `--app-address` (the address
   the running board advertised under), `--address` (the bootloader's), or
   neither and let the name pattern pick the only match;
2. connect (gatttool), find the Control Point by its UUID 1531, enable its
   notifications (CCCD), write `06`;
3. scan until the bootloader's advert is gone and say whether the
   application's address came back.

Scanning uses `btmgmt find -l`, which needs root (the management socket);
run the script under sudo. `--from-capture FILE` reads a captured scan (a
decoded btmon text or a `btmgmt find -l` transcript) instead and only says
what it would do; no radio is touched. `--selftest DIR` holds the parsers
against the captures in scripts/dfu-exit-fixtures/.

Exit status: 0 the advert vanished (the board left DFU), 1 no board or
several matched, 2 usage, 3 the GATT steps failed, 4 the write went out but
the bootloader still advertises.
"""

from __future__ import annotations

import argparse
import fnmatch
import os
import pty
import re
import select
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path

# Nordic Legacy DFU, as the Adafruit bootloader serves it.
DFU_SERVICE_UUID = "00001530-1212-efde-1523-785feabcd123"
DFU_CONTROL_POINT_UUID = "00001531-1212-efde-1523-785feabcd123"
CCCD_UUID = "00002902-0000-1000-8000-00805f9b34fb"
CHAR_DECL_UUID = "00002803-0000-1000-8000-00805f9b34fb"
OP_SYS_RESET = "06"
DEFAULT_NAME_PATTERN = "*-OTA"

EXIT_GONE = 0
EXIT_NOT_FOUND = 1
EXIT_USAGE = 2
EXIT_GATT = 3
EXIT_STILL_THERE = 4

ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
ADDRESS = re.compile(r"([0-9A-Fa-f]{2}(?::[0-9A-Fa-f]{2}){5})")


@dataclass
class Sighting:
    """One advertisement or scan response, as far as a capture shows it."""

    address: str
    random: bool | None = None
    name: str | None = None
    flags: int | None = None
    uuids: list[str] = field(default_factory=list)
    pdu: str | None = None
    data_len: int | None = None


@dataclass
class Device:
    """Every sighting of one address folded together."""

    address: str
    random: bool | None
    name: str | None
    flags: int | None
    uuids: list[str]
    pdus: list[str]
    sightings: int


def strip(text: str) -> str:
    return ANSI.sub("", text).replace("\r", "")


def normalise(address: str) -> str:
    return address.upper()


def bootloader_address(app_address: str) -> str:
    """The bootloader's address for an application address.

    The bootloader takes the SoftDevice's default address and adds one to its
    lowest byte (Adafruit_nRF52_Bootloader, dfu_transport_ble.c, the
    `addr[0]++` the concept page cites), a byte operation: 0xFF wraps to 0x00
    with no carry. The lowest byte is the last one in the printed form.
    Measured on the T114: app E1:AC:9E:B3:F0:5C, bootloader ...:5D.
    """
    if not ADDRESS.fullmatch(app_address):
        raise ValueError(f"not a BLE address: {app_address!r}")
    octets = app_address.upper().split(":")
    octets[-1] = f"{(int(octets[-1], 16) + 1) & 0xFF:02X}"
    return ":".join(octets)


def decode_ad(data: bytes) -> dict:
    """The AD structures this tool cares about: flags, name, service UUIDs."""
    out: dict = {"uuids": []}
    i = 0
    while i < len(data):
        length = data[i]
        if length == 0 or i + 1 + length > len(data):
            break
        kind, value = data[i + 1], data[i + 2 : i + 1 + length]
        if kind == 0x01 and value:
            out["flags"] = value[0]
        elif kind in (0x08, 0x09):
            out["name"] = value.decode("utf-8", "replace")
        elif kind in (0x02, 0x03):
            out["uuids"] += [
                f"0000{int.from_bytes(value[j:j + 2], 'little'):04x}-0000-1000-8000-00805f9b34fb"
                for j in range(0, len(value) - 1, 2)
            ]
        elif kind in (0x06, 0x07):
            for j in range(0, len(value) - 15, 16):
                h = value[j : j + 16][::-1].hex()
                out["uuids"].append(f"{h[:8]}-{h[8:12]}-{h[12:16]}-{h[16:20]}-{h[20:]}")
        i += 1 + length
    return out


HEX_ROW = re.compile(r"^\s+((?:[0-9a-f]{2} ){0,15}[0-9a-f]{2})(?:\s{2,}.*)?$")


def parse_btmon(text: str) -> list[Sighting]:
    """Advertising-report entries from a decoded btmon text (`btmon -r X`).

    Only the controller's reports are read (`Entry N` blocks under an LE
    Advertising Report); the AD bytes are taken from the hex rows and decoded
    here rather than trusted from btmon's labels, which name a vendor UUID
    "Vendor specific" and do not print it.
    """
    sightings: list[Sighting] = []
    current: Sighting | None = None
    pending_random: bool | None = None
    pending_pdu: str | None = None
    in_report = False
    hex_bytes: bytearray | None = None

    def close() -> None:
        nonlocal current, hex_bytes
        if current is not None:
            if hex_bytes:
                ad = decode_ad(bytes(hex_bytes))
                current.name = ad.get("name", current.name)
                current.flags = ad.get("flags", current.flags)
                current.uuids = ad["uuids"]
            sightings.append(current)
        current, hex_bytes = None, None

    for raw in strip(text).splitlines():
        if raw[:2] in ("> ", "< ", "@ ", "= "):
            close()
            in_report = False
            continue
        line = raw.strip()
        if "Advertising Report" in line:
            in_report = True
            continue
        if not in_report:
            continue
        if re.fullmatch(r"Entry \d+", line):
            close()
            pending_random, pending_pdu = None, None
            continue
        if line.startswith("Legacy PDU Type:") or line.startswith("Event type:") and pending_pdu is None:
            pending_pdu = line.split(":", 1)[1].strip()
            continue
        if line.startswith("Address type:"):
            pending_random = "Random" in line
            continue
        if line.startswith("Address:"):
            m = ADDRESS.search(line)
            if m:
                close()
                current = Sighting(normalise(m.group(1)), pending_random, pdu=pending_pdu)
            continue
        if current is None:
            continue
        if line.startswith("Data length:"):
            current.data_len = int(line.split(":", 1)[1].strip(), 0)
            hex_bytes = bytearray()
            continue
        m = HEX_ROW.match(raw)
        if m and hex_bytes is not None and len(hex_bytes) < (current.data_len or 0):
            hex_bytes += bytes.fromhex(m.group(1).replace(" ", ""))
            continue
        if line.startswith("Name (") and current.name is None:
            current.name = line.split(":", 1)[1].strip()
    close()
    return sightings


BTMGMT_FOUND = re.compile(
    r"dev_found: ([0-9A-Fa-f:]{17}) type (LE Random|LE Public|BR/EDR)"
)


def parse_btmgmt(text: str) -> list[Sighting]:
    """Sightings from a `btmgmt find -l` transcript (timestamps tolerated)."""
    sightings: list[Sighting] = []
    for raw in strip(text).splitlines():
        m = BTMGMT_FOUND.search(raw)
        if m:
            sightings.append(Sighting(normalise(m.group(1)), m.group(2) == "LE Random"))
            continue
        if not sightings:
            continue
        m = re.search(r"\bname (.+?)\s*$", raw)
        if m:
            sightings[-1].name = m.group(1)
            continue
        m = re.search(r"\bAD flags 0x([0-9a-f]{2})", raw)
        if m:
            sightings[-1].flags = int(m.group(1), 16)
    return sightings


def parse_capture(text: str) -> list[Sighting]:
    if "dev_found:" in text:
        return parse_btmgmt(text)
    return parse_btmon(text)


def fold(sightings: list[Sighting]) -> list[Device]:
    devices: dict[str, Device] = {}
    for s in sightings:
        d = devices.get(s.address)
        if d is None:
            d = devices[s.address] = Device(s.address, s.random, None, None, [], [], 0)
        d.sightings += 1
        d.name = d.name or s.name
        d.flags = d.flags if d.flags is not None else s.flags
        d.random = d.random if d.random is not None else s.random
        d.uuids += [u for u in s.uuids if u not in d.uuids]
        if s.pdu and s.pdu not in d.pdus:
            d.pdus.append(s.pdu)
    return list(devices.values())


def choose(
    devices: list[Device],
    address: str | None = None,
    app_address: str | None = None,
    pattern: str = DEFAULT_NAME_PATTERN,
) -> tuple[Device | None, str]:
    """The one bootloader to talk to, and a sentence saying why (or why not)."""
    if app_address:
        address = bootloader_address(app_address)
        why = f"the application's {normalise(app_address)} plus one"
    elif address:
        address = normalise(address)
        why = "the address given"
    else:
        why = f"the only name matching {pattern!r}"
    if address:
        for d in devices:
            if d.address == address:
                return d, f"{d.address} ({d.name or 'no name'}): {why}"
        return None, f"nothing advertises from {address} ({why})"
    matches = [d for d in devices if d.name and fnmatch.fnmatchcase(d.name, pattern)]
    if len(matches) == 1:
        d = matches[0]
        return d, f"{d.address} ({d.name}): {why}"
    if not matches:
        seen = ", ".join(f"{d.address} {d.name or '-'}" for d in devices) or "nothing"
        return None, f"no advertised name matches {pattern!r}; seen: {seen}"
    listed = ", ".join(f"{d.address} {d.name}" for d in matches)
    return None, f"{len(matches)} names match {pattern!r} ({listed}); give --address or --app-address"


GATT_CHAR = re.compile(
    r"handle\s*[:=]\s*0x([0-9a-f]{4}),\s*char properties\s*[:=]\s*0x([0-9a-f]{2}),\s*"
    r"char value handle\s*[:=]\s*0x([0-9a-f]{4}),\s*uuid\s*[:=]\s*([0-9a-f-]{36})",
    re.I,
)
GATT_DESC = re.compile(r"handle\s*[:=]\s*0x([0-9a-f]{4}),\s*uuid\s*[:=]\s*([0-9a-f-]{36})", re.I)


def control_point_handles(chars_text: str, desc_text: str) -> tuple[int, int] | None:
    """(value handle, CCCD handle) of the DFU Control Point, from gatttool's
    `characteristics` and `char-desc` output. The CCCD is the first 2902
    descriptor after the value handle and before the next characteristic
    declaration. On the T114: 0x0012 and 0x0013."""
    value = None
    for m in GATT_CHAR.finditer(strip(chars_text)):
        if m.group(4).lower() == DFU_CONTROL_POINT_UUID:
            value = int(m.group(3), 16)
    if value is None:
        return None
    for m in GATT_DESC.finditer(strip(desc_text)):
        handle, uuid = int(m.group(1), 16), m.group(2).lower()
        if handle <= value:
            continue
        if uuid == CHAR_DECL_UUID:
            break
        if uuid == CCCD_UUID:
            return value, handle
    return None


def describe(d: Device) -> str:
    kind = {True: "random", False: "public", None: "unknown type"}[d.random]
    uuids = ", ".join(d.uuids) if d.uuids else "none in the advertisement"
    flags = f"0x{d.flags:02x}" if d.flags is not None else "-"
    return (
        f"{d.address} ({kind}) name={d.name or '-'} flags={flags} service UUIDs: {uuids}; "
        f"{d.sightings} sighting(s)"
    )


def plan_lines(d: Device) -> list[str]:
    return [
        f"would connect to {d.address} (LE {'random' if d.random is not False else 'public'})",
        f"would find the DFU Control Point by UUID {DFU_CONTROL_POINT_UUID} "
        "(value handle 0x0012, CCCD 0x0013 on the T114 bench)",
        "would enable its notifications (write 0100 to the CCCD)",
        f"would write {OP_SYS_RESET} (Legacy DFU SYS_RESET) to the Control Point",
        f"would scan until {d.address} stops advertising",
    ]


# ---------------------------------------------------------------- live side


class Pty:
    """A child process on a pseudo-terminal, read with a deadline.

    btmgmt and gatttool are interactive shells that quit or buffer on a pipe;
    a pty is what the bench ran them on (`script -qfc`), without the extra
    process."""

    def __init__(self, argv: list[str]):
        master, slave = pty.openpty()
        self.proc = subprocess.Popen(argv, stdin=slave, stdout=slave, stderr=slave, close_fds=True)
        os.close(slave)
        self.master = master
        self.buffer = ""

    def read_for(self, seconds: float) -> str:
        end = time.monotonic() + seconds
        while (left := end - time.monotonic()) > 0:
            ready, _, _ = select.select([self.master], [], [], left)
            if not ready:
                break
            try:
                chunk = os.read(self.master, 4096)
            except OSError:
                break
            if not chunk:
                break
            self.buffer += strip(chunk.decode("utf-8", "replace"))
        return self.buffer

    def expect(self, pattern: str, seconds: float) -> re.Match | None:
        end = time.monotonic() + seconds
        rx = re.compile(pattern)
        while True:
            m = rx.search(self.buffer)
            if m:
                return m
            left = end - time.monotonic()
            if left <= 0:
                return None
            self.read_for(min(left, 0.2))
            if self.proc.poll() is not None and not rx.search(self.buffer):
                self.read_for(0.1)
                return rx.search(self.buffer)

    def send(self, line: str) -> None:
        self.buffer = ""
        os.write(self.master, (line + "\n").encode())

    def close(self) -> None:
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        os.close(self.master)


def scan(hci: int, seconds: float) -> list[Sighting]:
    """One LE discovery round (`btmgmt find -l`, about 10 s)."""
    p = Pty(["btmgmt", "--index", str(hci), "find", "-l"])
    try:
        p.expect(r"discovering off|Unable to start discovery|Permission Denied", seconds)
        text = p.buffer
    finally:
        p.close()
    if "Permission Denied" in text or "Unable to start discovery" in text:
        raise PermissionError(
            "btmgmt could not start discovery (" + text.strip().splitlines()[-1] + "); run under sudo"
        )
    return parse_btmgmt(text)


def sys_reset(d: Device, say) -> int:
    """Connect, enable the Control Point's CCCD, write SYS_RESET."""
    argv = ["gatttool", "-I", "-t", "random" if d.random is not False else "public", "-b", d.address]
    p = Pty(argv)
    try:
        p.read_for(0.5)
        p.send("connect")
        if not p.expect(r"Connection successful", 20):
            say(f"connect to {d.address} failed: {p.buffer.strip()[-200:]}")
            return EXIT_GATT
        say(f"connected to {d.address}")
        p.send("characteristics")
        p.expect(r"1531-1212-efde-1523-785feabcd123", 10)
        p.read_for(1.0)
        chars = p.buffer
        p.send("char-desc")
        p.expect(r"0x00[0-9a-f]{2}, uuid: 00002a26|Discover descriptors finished", 10)
        p.read_for(1.0)
        descs = p.buffer
        handles = control_point_handles(chars, descs)
        if handles is None:
            say(
                "no Legacy DFU Control Point (UUID 1531 with a CCCD) on this device; "
                "it is not an Adafruit bootloader in BLE DFU, nothing was written"
            )
            return EXIT_GATT
        value, cccd = handles
        say(f"DFU Control Point at 0x{value:04x}, its CCCD at 0x{cccd:04x}")
        p.send(f"char-write-req 0x{cccd:04x} 0100")
        if not p.expect(r"written successfully", 10):
            say(f"enabling notifications failed: {p.buffer.strip()[-200:]}")
            return EXIT_GATT
        say(f"notifications enabled (0100 to 0x{cccd:04x})")
        p.send(f"char-write-req 0x{value:04x} {OP_SYS_RESET}")
        # The board resets under the write; the response may never come.
        p.read_for(2.0)
        say(f"wrote {OP_SYS_RESET} (SYS_RESET) to 0x{value:04x}")
        p.send("exit")
        p.read_for(0.5)
        return EXIT_GONE
    finally:
        p.close()


def wait_gone(d: Device, hci: int, rounds: int, say) -> int:
    app = None
    octets = d.address.split(":")
    octets[-1] = f"{(int(octets[-1], 16) - 1) & 0xFF:02X}"
    app = ":".join(octets)
    for n in range(1, rounds + 1):
        seen = {s.address for s in scan(hci, 30)}
        if d.address not in seen:
            back = "and the application's address " + app + (
                " advertises again" if app in seen else " was not seen in that round"
            )
            say(f"scan round {n}: {d.address} no longer advertises, {back}")
            return EXIT_GONE
        say(f"scan round {n}: {d.address} still advertises")
    return EXIT_STILL_THERE


# ---------------------------------------------------------------- selftest


def selftest(fixtures: Path) -> int:
    failures: list[str] = []

    def check(ok: bool, what: str) -> None:
        print(("ok    " if ok else "FAIL  ") + what)
        if not ok:
            failures.append(what)

    t114_boot = "E1:AC:9E:B3:F0:5D"
    btmon = fold(parse_btmon((fixtures / "btmon-t114-ota.txt").read_text()))
    by = {d.address: d for d in btmon}
    check(t114_boot in by, "btmon: the T114 bootloader's address is read")
    d = by.get(t114_boot)
    check(d is not None and d.name == "HT-n5262-OTA", "btmon: its name is HT-n5262-OTA, from the AD bytes")
    check(d is not None and d.random is True, "btmon: its address is random (static)")
    check(d is not None and d.flags == 0x06, "btmon: flags 0x06")
    check(d is not None and d.uuids == [], "btmon: no service UUID in its advertisement")
    check(
        d is not None and any("ADV_IND" in p for p in d.pdus) and any("SCAN_RSP" in p for p in d.pdus),
        "btmon: connectable ADV_IND and a scan response",
    )
    ln = by.get("D9:16:E2:92:3E:D2")
    check(
        ln is not None and ln.name == "LN-e19b2b38" and len(ln.uuids) == 1,
        "btmon (control): an LNode application's name from its scan response, its 128-bit UUID decoded",
    )
    check(DFU_SERVICE_UUID not in (ln.uuids if ln else []), "btmon (control): that UUID is not the DFU service")

    chosen, why = choose(btmon)
    check(chosen is not None and chosen.address == t114_boot, f"choose by name pattern: {why}")
    chosen, why = choose(btmon, app_address="e1:ac:9e:b3:f0:5c")
    check(chosen is not None and chosen.address == t114_boot, f"choose by app address + 1: {why}")
    chosen, why = choose(btmon, pattern="AdaDFU")
    check(chosen is None and "no advertised name" in why, f"AdaDFU finds no T114 (control): {why}")
    chosen, why = choose(btmon, app_address="D9:16:E2:92:3E:D1")
    check(chosen is not None and chosen.address == "D9:16:E2:92:3E:D2", "app address + 1 is literal, not a name test")
    check(bootloader_address("00:11:22:33:44:FF") == "00:11:22:33:44:00", "plus one wraps the byte, no carry")

    btmgmt = fold(parse_btmgmt((fixtures / "btmgmt-find-t114-ota.txt").read_text()))
    m = {d.address: d for d in btmgmt}
    check(
        t114_boot in m and m[t114_boot].name == "HT-n5262-OTA" and m[t114_boot].random is True,
        "btmgmt: the same bootloader from a find -l transcript",
    )
    check(
        m.get(t114_boot) is not None and m[t114_boot].sightings == 3,
        "btmgmt: every sighting counted (once in round one, twice in round two)",
    )
    chosen, _ = choose(btmgmt)
    check(chosen is not None and chosen.address == t114_boot, "btmgmt: chosen by name pattern")
    chosen, why = choose(btmgmt, pattern="LN-*")
    check(chosen is None and "names match" in why, f"an ambiguous pattern is refused (control): {why}")

    handles = control_point_handles(
        (fixtures / "gatt-chars-t114-ota.txt").read_text(),
        (fixtures / "gatt-desc-t114-ota.txt").read_text(),
    )
    check(handles == (0x0012, 0x0013), f"gatt: Control Point 0x0012, CCCD 0x0013 (got {handles})")
    interactive = (
        "handle: 0x0011, char properties: 0x18, char value handle: 0x0012, "
        "uuid: 00001531-1212-efde-1523-785feabcd123\n"
    )
    desc_interactive = (
        "handle: 0x0012, uuid: 00001531-1212-efde-1523-785feabcd123\n"
        "handle: 0x0013, uuid: 00002902-0000-1000-8000-00805f9b34fb\n"
    )
    check(
        control_point_handles(interactive, desc_interactive) == (0x0012, 0x0013),
        "gatt: gatttool's interactive (colon) form reads the same",
    )
    no_cccd = "handle: 0x0013, uuid: 00002803-0000-1000-8000-00805f9b34fb\n"
    check(control_point_handles(interactive, no_cccd) is None, "gatt (control): no CCCD, no handles")
    check(
        control_point_handles((fixtures / "gatt-desc-t114-ota.txt").read_text(), "") is None,
        "gatt (control): no characteristics listing, no handles",
    )

    print(f"{'FAILED' if failures else 'passed'}: {len(failures)} of the checks failed")
    return 1 if failures else 0


# ---------------------------------------------------------------- main


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(
        prog="dfu-exit.py",
        description=(
            "Bring an nRF52 LNode out of its bootloader's BLE DFU mode: find its "
            "advert (name *-OTA, or the application's address plus one), connect, "
            "enable notifications on the Legacy DFU Control Point and write 06 "
            "(SYS_RESET). The application comes back intact as long as no "
            "transfer was started. Live use needs root for btmgmt (sudo)."
        ),
        epilog=(
            "Measured on a T114 (2026-11-10): advert HT-n5262-OTA from E1:AC:9E:B3:F0:5D "
            "for the application's ...:5C, Control Point 0x0012, CCCD 0x0013, application "
            "back 1.5 s after the write. Exit status: 0 the advert vanished, 1 no single "
            "board matched, 2 usage, 3 GATT failure, 4 still advertising."
        ),
    )
    who = ap.add_mutually_exclusive_group()
    who.add_argument("--address", help="the bootloader's BLE address")
    who.add_argument(
        "--app-address", help="the address the running application advertised under; the bootloader's is this plus one"
    )
    ap.add_argument("--name-pattern", default=DEFAULT_NAME_PATTERN, help="shell pattern for the advertised name (default: %(default)s)")
    ap.add_argument("--hci", type=int, default=0, help="controller index for btmgmt (default: %(default)s)")
    ap.add_argument("--rounds", type=int, default=3, help="scan rounds to wait for the advert to vanish (default: %(default)s)")
    ap.add_argument("--dry-run", action="store_true", help="scan and say what would be done; connect to nothing")
    ap.add_argument(
        "--from-capture", type=Path, metavar="FILE",
        help="read a captured scan (decoded btmon text or btmgmt find -l) instead of scanning; implies --dry-run",
    )
    ap.add_argument("--selftest", type=Path, metavar="DIR", help="check the parsers against the captures in DIR and exit")
    args = ap.parse_args(argv)

    if args.selftest:
        return selftest(args.selftest)
    if args.app_address and not ADDRESS.fullmatch(args.app_address):
        ap.error(f"--app-address: not a BLE address: {args.app_address}")
    if args.address and not ADDRESS.fullmatch(args.address):
        ap.error(f"--address: not a BLE address: {args.address}")

    def say(line: str) -> None:
        print(f"DFU_EXIT {line}", flush=True)

    if args.from_capture:
        sightings = parse_capture(args.from_capture.read_text(errors="replace"))
        say(f"read {len(sightings)} sighting(s) from {args.from_capture}")
    else:
        missing = [t for t in ("btmgmt", "gatttool") if shutil.which(t) is None]
        if missing:
            say(f"missing {', '.join(missing)} (Debian package bluez)")
            return EXIT_USAGE
        try:
            sightings = scan(args.hci, 30)
        except PermissionError as err:
            say(str(err))
            return EXIT_USAGE
        say(f"scanned hci{args.hci}: {len(sightings)} sighting(s)")

    devices = fold(sightings)
    target, why = choose(devices, args.address, args.app_address, args.name_pattern)
    if target is None:
        say(why)
        return EXIT_NOT_FOUND
    say(f"target {why}")
    say(f"advert {describe(target)}")
    if args.from_capture or args.dry_run:
        for line in plan_lines(target):
            say(line)
        return EXIT_GONE

    status = sys_reset(target, say)
    if status != EXIT_GONE:
        return status
    return wait_gone(target, args.hci, args.rounds, say)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
