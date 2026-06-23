# LNode Firmware: USB Serial Ports

A flashed LNode presents **two** USB CDC-ACM serial ports to the host.
Knowing which is which is the difference between reading a debug log and
talking the Reticulum transport protocol.

## The two ports

The firmware exposes two CDC-ACM serial ports. The **lower-numbered**
port is the debug log output; the **higher-numbered** port is the
Reticulum transport interface that carries HDLC frames. The actual
`/dev/ttyACM*` numbers depend on what else is plugged into USB.

> The firmware exposes two USB CDC-ACM serial ports. The lower-numbered
> port is the debug log output. The higher-numbered port is the
> Reticulum transport interface that carries HDLC frames. The actual
> `/dev/ttyACM*` numbers depend on other connected USB devices.
> (`reticulum-nrf/README.md:44-46`)

Each CDC-ACM class occupies two USB interfaces (a Communication
interface plus a Data interface), so the two ports map onto four USB
interface numbers:

| Port | USB interface nums | Carries |
|------|--------------------|---------|
| Debug | 00 (comm) + 01 (data) | human-readable log lines |
| Transport | 02 (comm) + 03 (data) | Reticulum HDLC frames |

(`reticulum-nrf/udev/99-leviculum.rules`, header comment.)

## Stable device paths via udev

Because the `/dev/ttyACM*` enumeration order is not stable, install the
shipped udev rules to get fixed symlinks:

```sh
sudo cp udev/99-leviculum.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
```

(`reticulum-nrf/README.md:50-53`)

After the next plug-in, the symlinks point at the correct ports
regardless of enumeration order. The names are board-family specific,
keyed off the per-board USB PID:

| Board | USB VID:PID | Debug symlink | Transport symlink |
|-------|-------------|---------------|-------------------|
| T114 | `1209:0001` | `/dev/leviculum-debug` | `/dev/leviculum-transport` |
| RAK4631 / Pocket V2 | `1209:0002` | `/dev/leviculum-rak-debug` | `/dev/leviculum-rak-transport` |

(Symlink names and PIDs: `reticulum-nrf/udev/99-leviculum.rules`. The
firmware-side USB VID/PID constants:
`reticulum-nrf/src/boards/t114.rs:139-140` for `1209:0001`,
`reticulum-nrf/src/boards/rak4631.rs:126-127` for `1209:0002`.)

> **Multiple boards of the same kind.** The short symlinks
> (`/dev/leviculum-transport`) land on whichever device udev sees first.
> The rules also emit per-serial-number symlinks
> (`/dev/leviculum-transport-<SERIAL>`); use those when more than one
> board of the same family is attached.
> (`reticulum-nrf/udev/99-leviculum.rules`, header comment and
> `SYMLINK+="leviculum-transport-%s{serial}"` lines.)

## Reading the debug port

The debug port is plain text at 115200 baud:

```sh
picocom /dev/leviculum-debug -b 115200
```

(`reticulum-nrf/README.md:59-60`)

On the debug port you will see the boot banner, the firmware git SHA, the
node identity, and the periodic diagnostics the firmware emits — for
example the `LNode started -- identity: …` line and the `[FW_BUILD]`
banner re-emitted every few seconds.
(`reticulum-nrf/src/bin/t114.rs:164-168`, `:323-333`.) Do **not** point
`lnsd` at the debug port; it carries log text, not HDLC frames.

## Pointing `lnsd` at the transport port

The transport port speaks the RNode LoRa framing protocol over HDLC, the
same wire protocol `lnsd`/`rnsd` use for an RNode. Configure it as an
`RNodeInterface` (the dedicated single-radio RNode driver) with its
`port` set to the transport symlink.

The radio parameters in the config **must match** the firmware's
compiled-in defaults (the EU medium profile), otherwise the two sides
talk past each other on the air. (`reticulum-nrf/README.md:8`;
`reticulum-nrf/src/lora.rs:124-138`.)

```ini
[interfaces]

  [[LNode T114]]
    type = RNodeInterface
    enabled = yes
    port = /dev/leviculum-transport
    frequency = 869525000
    bandwidth = 125000
    txpower = 17
    spreadingfactor = 7
    codingrate = 5
```

The key names and types come from the `[[RNode Interface]]` config
schema (`port`, `frequency`, `bandwidth`, `txpower`, `spreadingfactor`,
`codingrate`; `docs/src/rnode-protocol.md:674-684`). The values above are
the firmware's compiled defaults: 869.525 MHz, BW 125 kHz, 17 dBm, SF7,
CR4/5 (`reticulum-nrf/src/lora.rs:124-138`).

For a RAK4631 / WisMesh Pocket V2 the only change is the port:

```ini
  [[LNode Pocket V2]]
    type = RNodeInterface
    enabled = yes
    port = /dev/leviculum-rak-transport
    frequency = 869525000
    bandwidth = 125000
    txpower = 17
    spreadingfactor = 7
    codingrate = 5
```

After editing `/etc/reticulum/config`, restart the daemon so it picks up
the new interface:

```sh
sudo systemctl restart lnsd
```

(Same restart flow as any config change; see the
[lnsd Quickstart](../lnsd-quickstart.md).)

## Confirming the link came up

Run the standard health-check and look for the new interface in the
`interface_stats` section with `status=up` and non-zero counters once
LoRa traffic flows:

```sh
lns diag --config /etc/reticulum
```

(`lns diag` usage and the `interface_stats` reading are described in the
[lnsd Quickstart](../lnsd-quickstart.md#check-its-working).)

For the full key-by-key reference of the `[[RNode Interface]]` section
and the meaning of the optional keys (`flow_control`, `airtime_limit_*`,
callsign beaconing), see the [Configuration](../guide/configuration.md)
chapter and `docs/src/rnode-protocol.md`.
