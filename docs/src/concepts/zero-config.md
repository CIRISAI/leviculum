# Zero-config lnsd

A fresh install of lnsd that nobody wrote a configuration for should find
the network on its own: the LAN, the internet, a phone over Bluetooth, a
board on a USB port. Lew decided the shape on 2026-10-23 (Codeberg #471).
This page records the design as decided, the facts it rests on, the
measurements the cells took before any of it was built, and the order it
is built in.

Steps 1 to 4 are built (below), and lnsd writes its first-start file
(open question 1, decided); nothing after them is. Citations into this tree are against
master at the commit that adds the page; citations into the Python
reference are against the pinned `reference/Reticulum` (1.3.5). The
cells live in periculum (`regression/`, periculum `cb51b8c`), so they
are named here, not cited by line.

## What a fresh lnsd did before step 1

Measured on 2026-10-23 by the cells below, against leviculum 77c4ff13,
with lnsd started as `lnsd -c` on an empty directory:

- **No interface.** Without a file lnsd took `Config::default()`, whose
  interface map is empty. It logged `Node started with 0 interface(s)`.
- **No shared instance.** The default is `shared_instance: false`
  (`shared_instance`, `leviculum-std/src/config.rs:657`), where rnsd
  defaults to yes. No client could attach, so lnomad, lnstatus and the
  Python tools saw nothing at all. Every cell line therefore states
  `shared_instance=` first.
- **Transport on.** The default is `enable_transport: true`
  (`enable_transport`, `leviculum-std/src/config.rs:652`), and lnsd said
  so in its `CONFIG_DEFAULT` line (`value=true rnsd=false`). The stock
  file rnsd writes on its first start says `enable_transport = False`
  (`Reticulum.py:1879`).
- **It writes nothing.** rnsd creates its default configuration on the
  first start (`__create_default_config`, `Reticulum.py:1196`), with one
  AutoInterface (`Reticulum.py:1975-1977`). lnsd did not, before or
  after step 1; since 1510 it does (below).

`Config::default()` itself is unchanged by step 1: it is what a program
that builds a node in code starts from, and only the daemon's no-file
start moved.

## Step 1, as built

Without a `config` file lnsd now runs the file rnsd would have written,
from memory (`load_or_default`, `leviculum-std/src/config.rs:1619`). The
text is `RNSD_DEFAULT_CONFIG` (`leviculum-std/src/config.rs:761`): not
the template literal of `Reticulum.py:1860` but what ConfigObj makes of
it on `write()`, which indents every key by two spaces and drops the
last empty line. Captured from the vendored ConfigObj of the pinned
reference, 4013 bytes. Two tests hold it: its lines are the template's
lines, and, where `python3` runs, the template through the reference's
own ConfigObj is byte for byte the same file. lnsd parses it with its
own INI reader (`rnsd_default`, `leviculum-std/src/config.rs:1605`), so
what it runs and what rnsd would write cannot drift apart. It means:
transport off, a shared instance named `default`, log level 4,
`discover_interfaces` at lnsd's default, and one AutoInterface,
`[[Default Interface]]`, in the default group `reticulum`.

- `lnsd --writeconfig` writes the same text to `<dir>/config` and exits;
  it never replaces an existing file.
- Every interface section carries an origin, `configured` or `default`
  (`InterfaceOrigin`, `leviculum-std/src/config.rs:704`). lnsd logs one
  `IFACE_CONFIG name= type= enabled= origin=` line per section before it
  builds any (`leviculum-std/src/driver/mod.rs:2662`), and once
  `CONFIG source=file|default path=` (`leviculum-cli/src/lnsd.rs:229`).
- `interface_stats` serves the origin as the `origin` key
  (`leviculum-std/src/rpc/handlers.rs:694`), and `lnstatus` prints it,
  behind `--origin`, as an `Origin` line under `Status`. Without the flag
  the render is rnstatus's, which the drop-in parity tests pin
  (`leviculum-std/tests/rnsd_interop/status_parity_tests.rs`); rnsd serves
  no such key, so against rnsd the flag adds nothing. A row named after a section takes that
  section's origin; a row no section names (the shared instance, an
  AutoInterface's peers) takes the origin all sections share
  (`origin_of`, `leviculum-std/src/interfaces/inventory.rs:509`), and
  where no section is left, as on a start without a file under
  `LEVICULUM_NO_AUTO=1`, the origin of the configuration itself:
  `default` without a file, `configured` with one.

## Step 2, as built

The switches are in place; the only medium they move today is the LAN,
because the others have nothing to start yet.

- **`[auto]`**, four keys, `lan`, `servers`, `bluetooth`, `serial`, each
  yes or no, read by our INI reader (`apply_auto_key`,
  `leviculum-std/src/ini_config.rs`) and by the TOML path through serde.
  A value that is no boolean refuses the file. A file without the section
  is every key `no`. Without a file lnsd runs `LNSD_DEFAULT_CONFIG`
  (`leviculum-std/src/config.rs`): rnsd's first-start file, unchanged,
  followed by `[auto]` with the shipped defaults (`lan = yes`, the rest
  `no`) and a comment line per key. `lnsd --writeconfig` writes that
  text, and a test runs it through the reference's own ConfigObj, which
  reads rnsd's three sections unchanged beside `[auto]`.
- **The rule** is one function with its table in the doc comment
  (`decide`, `leviculum-std/src/auto_layer.rs`):

  | configuration | marker | `LEVICULUM_NO_AUTO` | media | reason |
  |---|---|---|---|---|
  | asks for a medium | any | `1` | none | `env` |
  | asks for a medium | present | unset | none | `marker` |
  | no file: the shipped defaults | absent, or lifted by `0` | unset or `0` | `lan` | `default` |
  | a file: its `[auto]` keys | absent, or lifted by `0` | unset or `0` | the keys that are `yes` | `config` |
  | a file asking for nothing | any | any | none | `config` |

  The marker and the variable are named only when they turned off
  something the configuration asked for. A file that asks for nothing,
  which is every periculum cell's file, therefore reads `reason=config`
  on a marked host too, and the corpus guard can hold every daemon to
  that one reading.
- **The LAN** (`apply`, same file): with `lan` on and no AutoInterface
  section in the configuration, enabled or not, lnsd adds
  `[[Default Interface]]` from rnsd's first-start file, origin `default`.
  With `lan` off, every interface of origin `default` goes, which on a
  start without a file is that same AutoInterface; a file's own sections
  are never touched, so a file that names an AutoInterface keeps it on a
  marked host.
- **The line**, once per start, before the `IFACE_CONFIG` lines, and
  catalogued (`AUTO_LAYER`, `leviculum-std/src/event_log.rs`):
  `AUTO_LAYER state=on|off reason=config|default|marker|env lan= servers=
  bluetooth= serial=`, each medium `yes` or `no`. A marker or variable
  that turned the layer off also gets a plain line naming it.
  `interface_stats` serves the same fields after `state=` as the
  top-level `auto_layer` key, and `lnstatus` prints them, behind
  `--origin` like the `Origin` lines, as its last line,
  `Auto layer   : on reason=default lan=yes servers=no bluetooth=no
  serial=no`.
- **The marker** is installed by `scripts/install-ci.sh`, step 10b,
  beside the hardware probe: every host that script sets up runs the
  suites with adapters and boards in reach, in `--vm-mode` too. It uses
  `sudo -n` and warns with the command when it cannot.

Open question 1 was sharp after step 2: a Python tool pointed at a fresh
lnsd's directory wrote rnsd's stock file there, and from the next start
lnsd read it as a file without `[auto]`, every medium off. It is decided
and built since 1510 (below).

## The first-start file, as built

lnsd writes its own configuration on a start without a file, the way rnsd
does (`Reticulum.py:336`): the bytes `lnsd --writeconfig` writes, rnsd's
first-start file with the `[auto]` section after it, every medium at its
shipped default (`run`, `leviculum-cli/src/lnsd.rs`). That start keeps
running what it loaded, so its interfaces are `default` and its
`AUTO_LAYER` line reads `reason=default`; the next start reads the file,
`reason=config`, the same media. A Python tool pointed at the directory
afterwards finds a file and writes nothing. The write never replaces a
file (`create_new`, so a tool's file that appeared in between stays), and
a directory lnsd may not write into is a warning, not a failed start. The
`CONFIG` line says which: `written=yes|no|failed`.

The cell `zero_config_first_start_file` holds it: the file exists and
equals `--writeconfig`'s output, `rnstatus` against the directory adds no
path and leaves the file's bytes, and its control, `rnstatus` against an
empty directory, writes RNS's stock file there.

## Step 3, as built

`[auto] servers = yes` is a shipped default now (`AutoMedia::SHIPPED`,
`leviculum-std/src/auto_layer.rs`), and the medium is one module
(`leviculum-std/src/auto_servers.rs`):

- **The list.** `SHIPPED_SERVERS`, one entry today,
  leviculum.network on port 4242. `[auto] server_list = host:port, ...` replaces
  it, and `LEVICULUM_AUTO_SERVERS=host:port,...` in the environment
  replaces both (set but empty is a list of none, which is how every
  periculum subject starts). A pinned list is dialled in its order; the
  shipped one is drawn at random.
- **The slots.** Up to `BOOTSTRAP_ENTRIES` (two) entries, each a
  `TCPClientInterface` section `[[Auto Server <n>]]`, `bootstrap_only`,
  origin `default`, so the existing bootstrap lifecycle (#416) detaches
  them once enough discovered interfaces stand. A configuration's
  `autoconnect_discovered_interfaces = 0` is raised to `DISCOVERED_CEILING`
  (two); a file's own nonzero value stays. Both constants are open
  question 3's starting values, and a measurement moves them.
- **Fail-over.** A slot's TCP client asks the shared pool for the next
  entry no other slot holds after `FAILOVER_AFTER_FAILURES` (three) failed
  connects in a row, its backoff running on (`tcp_client_reconnect_loop`,
  `leviculum-std/src/interfaces/tcp.rs`). The fail-over is the
  interface's business; the core sees a TCP client that reconnects. The
  row keeps the section's first entry in its name after a move.
- **Persisted and logged.** The slots' entries are kept in
  `<storage>/auto_servers`, one `host:port` a line, and a start whose list
  still names all of them takes them again. Every choice is one line,
  `AUTO_SERVER slot= chosen=<host:port> reason=persisted|drawn|pinned|failover
  from=<host:port|none>`, and a list of none `chosen=none reason=empty`.

The cell `zero_config_server_list` holds it: a pinned list whose first
entry never resolves, the slot's move to the third entry, both slots up, and
a restart on the first start's own file that takes the kept choice.

Two facts about the files decide the opt-out format:

- **rnsd ignores a section it does not know.** `__apply_config`
  (`Reticulum.py:469-545`) reads `[logging]` and `[reticulum]`, and the
  interface builder reads `[interfaces]` (`Reticulum.py:701-702`).
  Nothing iterates the rest of the file. `rnstatus` and `rncp` read
  `[reticulum]` only. A file with an `[auto]` section stays readable for
  rnsd and the stock tools.
- **lnsd warned about it before step 2 and went on.** A file with
  `[auto] bluetooth = no` started, and lnsd logged `'bluetooth' is in
  section '[auto]', which lnsd does not read, and is ignored`, the
  warning it still gives for any other section it does not read
  (`leviculum-std/src/ini_config.rs:199`). Measured in the cell
  `ble_room_auto_start`. Since step 2 lnsd reads the section.

And one fact about the Python tools, which the cells ran into: **a Python
client pointed at a config directory without a file writes rnsd's stock
file into it**, AutoInterface and all (`Reticulum.py:336`). A user who
runs `rnstatus` once against a zero-config lnsd's directory leaves a
configuration behind, and from the next start lnsd reads that file and
nothing else. See the open questions.

## The rule: a file is the opt-out, per medium

On a start without a file, lnsd brings up everything the machine offers.
When a file exists, nothing starts that the file does not name, unless
the file's `[auto]` section asks for it. The rule is per medium, so a
user who adds one TCP server to a file does not lose BLE and serial
detection without noticing:

```ini
[auto]
  lan = yes          # AutoInterface when the file names none
  servers = yes      # the shipped server list
  bluetooth = yes    # BLE when an adapter is present
  serial = yes       # LNodes and RNodes on USB
```

A file without `[auto]` means all four are `no`. That is how a periculum
cell stays confined (every cell writes a file), and it is what the cell
`config_present_means_only_config` asserts today and after every step.

The layer that decides sits in front of the core. The core and
`transport.rs` never learn that an interface was started automatically:
the medium knowledge stays in the interface, as
[Interface isolation](interface-isolation.md) requires. `lnstatus` shows
where each interface came from (`configured` or `default`, step 1), and
lnsd states its decision once at start (as built in step 2, above):

```
AUTO_LAYER state=off reason=config lan=no servers=no bluetooth=no serial=no
AUTO_LAYER state=on reason=default lan=yes servers=yes bluetooth=yes serial=yes
```

`lnsd --writeconfig` writes the file lnsd would otherwise hold in memory,
`[auto]` section included, in a form rnsd can read, and lnsd writes the
same file on its first start (open question 1). The server list is not in
the file, only the switch, so a later change to the shipped list reaches
every install whose file does not pin one.

## The lab guard

BLE stays on by default in the shipped builds. Lew decided against a
separate test build: tests run the binary users run. Two things keep the
automatic layer out of the measurement room, where hamster and
schneckenschreck have Bluetooth adapters and boards on USB:

1. **Configuration.** Every periculum cell renders a file, so the rule
   above already says `off`. The periculum guard `periculum auto-layer`
   reads every lnsd log of a corpus run and is red when any daemon logged
   `state=on`. Against the conformance run of 2026-10-23 it read
   `lnsd_logs=115 off=0 on=0 absent=115`: the line did not exist yet.
   Since step 2 the guard requires the line, `state=off reason=config`
   in every lnsd log, and is red on an absent line or any other reason
   (`--allow-absent` reads a run from before the step).
2. **A host marker.** `/etc/leviculum/no-auto` makes lnsd treat a start
   without a file as `state=off reason=marker`, and `LEVICULUM_NO_AUTO=1`
   in its environment as `state=off reason=env`. `scripts/install-ci.sh`
   installs the marker on the rig hosts. The environment wins over the
   marker in both directions: `LEVICULUM_NO_AUTO=0` is how a cell
   measures the automatic layer on a marked host, and the zero-config
   cells set it.

Step 6 measures the guard itself: a rig host without the marker must not
join the room. The cells add a third fence of their own: every subject
the zero-config fixture starts names an adapter BlueZ never lists
(`LEVICULUM_AUTO_BLUETOOTH_ADAPTER`), because a netns node's bus relay
reaches the host's bluetoothd, whose default adapter on a rig host is the
real one. Only the cell that hands its subject a btvirt controller names
another (Step 6, as built). The twin on the rig hosts themselves, an
unmarked lnsd with `LEVICULUM_NO_AUTO=0` joining the Columba room and a
marked one not, is the reviewer's.

## The server list, and what the first entry owes

A start without a file opens two TCP connections to entries drawn at
random from a list shipped in the binary, each marked `bootstrap_only`
(`bootstrap_only`, `leviculum-std/src/driver/builder.rs:749`): it exists
to join the network and is detached once enough discovered connections
stand. Discovery is already on by default (`default_discover_interfaces`,
`leviculum-std/src/config.rs:609`); autoconnect is not
(`autoconnect_discovered_interfaces`, `leviculum-std/src/config.rs:678`),
and the automatic layer raises it to a ceiling of two or three. The
ceiling and the entry count are constants in the code (step 3 as built,
above), changed after a measurement, not before. Transport stays off
on such a start, as rnsd's stock file has it, which is a change from
today's default (above).

The choice is persisted in the storage directory, logged
(`AUTO_SERVER chosen= reason=`), pinnable by a flag and by
`LEVICULUM_AUTO_SERVERS=<host:port>,...` (the knob the cell
`zero_config_server_list` points at a periculum node), and replaced from
the list when an entry fails. Without fail-over a user stays attached to
a dead entry.

The first entry is `leviculum.network`, Lew's own server. A default that
every fresh install dials is an operating duty before it is a
configuration line:

- **monitoring** and capacity on the entry node;
- **a published privacy statement**: the entry node sees the user's
  announces and paths;
- **consent** of every further operator before their server joins the
  list;
- **fail-over** in the client, and the choice persisted and logged, as
  above.

Fail-over, persistence and the log line are built (step 3, 1510), and the
code dials the entry by default since then. Monitoring and the privacy
statement are operating duties outside this tree and stay open (the last
row of the step table); the entry is not announced to users as a default
before they stand. Further operators join only with their consent.

## Serial: LNodes by their name, RNodes by silence, then a probe

**LNodes** need no probe. Our firmware sets its USB manufacturer and
product strings itself (`leviculum-nrf/src/usb.rs:161-162`), so an LNode
appears as `/dev/serial/by-id/usb-leviculum_leviculum_<Board>_<Serial>-if02`.
The string is ours; the test PID beside it (`1209:0001` to `1209:0004`)
is shared, which is why an own PID (#460) stays wanted but is not a
precondition. lnflash recognises boards this way today without writing a
byte.

**RNodes** sit on generic USB bridges (CP210x on a Heltec V3, CH9102 on
the T-Beams, Espressif JTAG/serial on a Heltec V4, measured on the rig),
which a thousand other devices carry too. Detection writes to a device
only when both hold:

1. its by-id name is an RNode-typical candidate string (Heltec, LilyGO,
   RAK, Espressif JTAG/serial), and
2. it stayed silent for one second of listening, or said nothing but
   RNode status frames. A GPS speaking NMEA or a printer reporting its
   status is out, whatever its name.

Then, and only then, one `CMD_DETECT` with `DETECT_REQ`; a device that
answers `DETECT_RESP` is an RNode. lnsd already speaks this handshake
(`detect_on_port`, `leviculum-std/src/interfaces/rnode.rs:1933`; the same
frame is its serial heartbeat, `leviculum-std/src/interfaces/rnode.rs:3087-3089`).
A refused device is remembered and not asked again; hotplug is a rescan
of the by-id directory every 5 s (built so, see step 5 below). The directory is
`LEVICULUM_AUTO_SERIAL_DIR`, default `/dev/serial/by-id`, so a cell can
hand lnsd ptys.

Two rules hold for every port: lnsd opens it with `TIOCEXCL`, so a port
lnflash or a terminal holds is not taken, and a device in its bootloader
is neither probed nor reported as a node. The bootloader ids come from
the catalogue lnflash already keeps (`bootloader_usb`,
`lnflash/catalogue.toml:230`, the T1000-E's `2886:0057`; the UF2
bootloaders at `lnflash/catalogue.toml:71` and `lnflash/catalogue.toml:130`),
read from that file, not copied. A missing `dialout` membership is a
clear log line, not a silent skip.

An attached LNode, and the M9, gets `interface_mode = gateway` on its
interface: the board behind the line asks paths for itself and only a
discovering mode looks for them onwards
(`leviculum-core/src/transport.rs:13242`); in full mode a board's join
to its home ends in `NoPath` (1439, measured on the bench).

All found boards are attached. Two RNodes on one frequency in one room
hear the same frames and contend on transmit; that is documented, not
refused.

## Step 5, as built

The medium is `leviculum-std/src/auto_serial.rs`; lnsd calls it after
the server list, before the node is built.

- **The ids.** Each by-id entry is sorted by its USB ids, read from sysfs
  (the tty's device, up to the USB device's `idVendor` and `idProduct`).
  A link that leads to no USB device, a cell's pty, is sorted by the
  strings udev built its name from instead:

  | ids | or, without ids, the name | is | lnsd |
  |---|---|---|---|
  | a `bootloader_usb` id of `lnflash/catalogue.toml` (`239a:0071`, `239a:0029`, `2886:0057`) | contains `bootloader` | a board in its bootloader | leaves it, unopened |
  | a `candidate_usb` id of the catalogue under `1209` (`1209:0001` to `1209:0004`), with the name `usb-leviculum_[leviculum_]<Board>_<Serial>-ifNN` | the name alone | an LNode | `if02`: attaches it, no byte written first; `if00`, the debug log: leaves it |
  | `10c4:ea60` (CP210x, Heltec V3) | `Silicon_Labs_CP210` | an RNode candidate | listens, then probes |
  | `1a86:55d4` (CH9102, the T-Beams) | `1a86_USB_Single_Serial` | an RNode candidate | listens, then probes |
  | `303a:1001` (ESP32-S3 USB, Heltec V4) | `Espressif_USB_JTAG_serial_debug_unit` | an RNode candidate | listens, then probes |
  | anything else, `1a86:7522` (the M9's CH340K) and stock firmware's `239a:8029` among them | | a stranger | leaves it, unopened |

  The catalogue is read from lnflash's file, compiled in, so a new board
  is one entry for both tools. Our strings on an id that is not ours are
  no LNode. The name fragments are the rig's by-id names as measured
  2026-11-08; "Heltec", "LilyGO" and "RAK" in the plan above appear in no
  by-id name the rig's RNodes carry, so they are not in the table.
- **Held ports.** Before anything is opened, `/proc/*/fd` is searched for
  another process with the same device open; one is a skip with a line
  naming it, and the entry is asked again on the next scan. A candidate is
  opened under `TIOCEXCL`, so a holder this process cannot see in `/proc`
  shows as `EBUSY`, the same skip; `TIOCNXCL` before the close leaves the
  port to the interface. `EACCES` is a warning naming the dialout group.
- **The probe.** A candidate is put raw at 115200 Bd, its input flushed,
  and listened to for one second (`SILENCE`). An RNode is not always
  silent: its radio on, the firmware sends `CMD_STAT_CHTM`
  (`c0 25 .. c0`) about every 1.15 s, and battery and temperature
  (`c0 27 .. c0`, `c0 29 .. c0`) every 5 s even with the radio off, all
  measured on the rig (1567). Before 1567 one such byte made the board
  "talking", and a board left with its radio on by the previous instance
  was refused for good. So the listen is judged by what it heard
  (`listen_verdict`): well-formed KISS frames whose command bytes are all
  of the firmware's status family, `CMD_STAT_RX` 0x21 through
  `CMD_STAT_TEMP` 0x29 (`reference/RNode_Firmware/Framing.h:41-49`,
  `leviculum_core::rnode::CMD_STAT_*`), name the candidate a KISS speaker,
  and it is asked at once, on the first whole frame. A frame cut at either
  edge of the window is tolerated up to 32 bytes; a window that ends
  inside one is stretched by 50 ms (`KISS_TAIL`) to read it whole.
  `CMD_DATA` (0x00), a received packet, is not of the family: a generic
  KISS modem sends it too. Anything else is "talking": a boot ROM's text,
  NMEA, a frame of another command. The talker is refused, and listened
  to again on the next two scans (`TALKING_LISTENS`, a board through its
  boot prints once, a GPS never stops) before it stays refused. A silent
  candidate, or one that spoke only status, is sent `c0 08 73 c0`, the
  four bytes of `CMD_DETECT` with `DETECT_REQ` and nothing else, and has
  500 ms to answer `DETECT_RESP`; one that does not is not asked again.
  Only a candidate, a port whose ids or name are in the table above, is
  ever listened to, so a stranger still receives nothing. Several
  candidates are listened to in parallel, so a start waits one silence,
  not one per board.
- **The sections.** A board becomes `[[Auto Serial <n>]]`, origin
  `default`, its port the by-id link: an LNode a `SerialInterface` with
  `interface_mode = gateway`, an RNode an `RNodeInterface`, both on
  869463000 Hz, 125 kHz, SF8, CR5, the transmit power the builder's
  default (the board's maximum under the lawful limit). A port a section
  of the file names is left to that section (`reason=configured`), and a
  section name the file uses is skipped.
- **Hotplug.** A board present at start is a section before the node is
  built, so its `IFACE_CONFIG` line says `origin=default`. After the start
  the directory is read again every 5 s (`RESCAN`), and a new board is
  handed to the running node (`Reticulum::spawn_interface`). One board is
  one section: a by-id name that got a section keeps it, present or not,
  while lnsd lives, and its interface's own reconnect loop owns the
  board's return, however many rescans it was gone. The section table
  answers before the port is looked at, so the scan neither probes the
  name again nor renders it a second section (and our own open port is
  never mistaken for a stranger's, which the holder search could not tell:
  it skips lnsd's own pid). Before 1567 an absence across one rescan
  forgot the entry and the return became a second section that fought
  the first for the port for ever (rig, a T114 deauthorised 8 s:
  `serial_7` down with `Device or resource busy` every 5 s, `serial_18`
  up). Only an entry that never got a section, a stranger, a refused or
  held candidate, is forgotten when its link disappears, and asked again
  when it is plugged in again. A rescan, not inotify: it needs nothing
  beyond the directory, which a cell can be handed.
- **The line**, catalogued, once per entry and again when its verdict
  changes: `AUTO_SERIAL board=<Board|none> port=<by-id name>
  kind=lnode|rnode|bootloader|other reason=name|detect|debug_port|bootloader|not_candidate|held|talking|no_answer|configured|no_access|open_failed|not_built
  attached=yes|no section=<name|none> holder=<pid:comm|unknown|none>`. A
  candidate that did not answer is `kind=other`. `holder` names the
  process that holds a `held` port, `unknown` when only `TIOCEXCL` said
  so (1567); `none` on every other line.
- **The goodbye.** A stopping lnsd writes every RNode its goodbye, radio
  off and leave (`c0 06 00 c0 c0 0a ff c0`), and waits for each write
  before the process ends, at most 2 s (`GOODBYE_BOUND`,
  `leviculum-std/src/interfaces/goodbye.rs`). Before 1567 the runtime went
  right after the event loop: on the rig a SIGTERM reached two RNodes of
  four, and the other two kept their radios on and talked into the next
  start's probe. A stop during an RNode's configuration cuts it short and
  says the goodbye too, since the configuration had just switched the
  radio on. The `configured` line names the power the board confirmed,
  `txp=17 dBm, 22 dBm requested` on a T-Beam whose maximum is below the
  request.
- **The switch.** `AutoMedia::SHIPPED.serial` is `yes`, and the
  first-start file says so; `[auto] serial = no` turns it off with any
  other content of the file, and a file without `[auto]` asks for
  nothing, as for every medium. A build without the `serial` feature
  names the boards and attaches none (`reason=not_built`), and off Linux
  the medium says once that it does not scan.

The cells, in netns on 2026-11-08 (docker owed: the land gate held the
docker lock):

- `serial_probe_writes_nothing_to_strangers`: `strangers=0 quiet=0
  talker=0 rnode_probe=seen lnode_attached=0 rnode_attached=1 rnode=30`
  (asserted up to `rnode=`, which counts the interface's own opening after
  the attach); the control arm `... rnode_attached=0 rnode=13`.
- `zero_config_serial_lnode`: `attached=1 verdict=lnode:name
  interface=yes up=yes phy=869463000/125000/8/5 debug_bytes=0` (asserted).
- `zero_config_serial_held`: `written=0 attached=0 held=2` (asserted).

The cells' fixtures close their pty slaves once the link is made, so the
only holder of a device is the one a cell means; a subject whose cell
names no directory reads an empty one, so a netns cell on the rig host
never reaches its boards.

## BLE

On when an adapter is present (step 6), Columba-compatible. A missing or
powered-off adapter is a warning, not a failed start. `[auto] bluetooth =
no` keeps it off with any other content of the file, which the cell
`ble_room_auto_start` asserts in its second arm.

## Step 6, as built

The medium is `leviculum-std/src/auto_bluetooth.rs`; lnsd calls it after
the serial medium, before the node is built.

- **The adapter rule.** lnsd asks BlueZ over D-Bus, bounded by 3 s
  (`QUERY_TIMEOUT`), for the adapter the BLE interface itself would take:
  BlueZ's default (`hci0` when there is one, else the lowest name,
  `bluer::Session::default_adapter`), or the one
  `LEVICULUM_AUTO_BLUETOOTH_ADAPTER` names. A powered adapter is
  `present`; one that is off is powered on the spot, as the interface's
  own session would, and one that refuses (rfkill) is `powered_off`. No
  adapter, a name BlueZ does not list, a BlueZ that is not reachable or
  does not answer in time is `no_adapter`. Both are one WARN line, and the
  start goes on.
- **The section.** `present` adds `[[Auto Bluetooth]]`, origin `default`:
  a `BLEInterface` with every key at the interface's shipped default (both
  GATT roles, four links, no allow-lists), `device` unset, so the
  interface takes the same default adapter, or `device` the adapter the
  variable names. Nothing new on the air: it is the interface a
  configured section with no keys starts. A configuration with an enabled
  `BLEInterface` section of its own keeps it and gets none added
  (`reason=configured`); a file that already uses the section name keeps
  its own, with a warning.
- **Later.** A missing or powered-off adapter is asked for again every
  5 s (`RESCAN`), and the section is handed to the running node once one
  is there (`Reticulum::spawn_interface`, as the serial medium does).
  After that the interface's own retry loop owns the adapter: its session
  retries every 10 s after a failed setup and re-resolves a lost adapter
  every scan interval (`interfaces/ble/mod.rs`, `ADAPTER_RETRY` and the
  lost-adapter path). The unit test
  `a_missing_adapter_is_asked_again_and_logged_once_until_it_appears`
  holds the sequence.
- **The line**, catalogued, once and again only when the verdict changes:
  `AUTO_BLUETOOTH adapter=<hciN|none> reason=present|no_adapter|powered_off|configured|not_built
  attached=yes|no section=<name|none>`. `AUTO_LAYER` reads `bluetooth=yes`
  whenever the medium is on, adapter or not.
- **The switch.** `AutoMedia::SHIPPED.bluetooth` is `yes`, and the
  first-start file says so. A build without the `ble` feature (the
  minimal lnsd, and every build off Linux) states `reason=not_built` and
  attaches nothing.

The cell's node shape: `alpha` and `beta` hold a `ble` link, and `alpha`
carries `ble_spare_controller = true`, one more btvirt controller in the
run's air that `alpha`'s own rendered file does not name. The runner
writes its `hciN` into `alpha`'s directory beside the bus relay, and the
subject, a second lnsd inside `alpha` with no file, is handed it through
the variable. Chosen over a third node whose file carries only `[auto]`:
every periculum node is rendered a file with its link's `[[BLE Link]]`,
so that shape needs a second exception in the renderer, while the subject
already lives in `alpha` and reaches bluetoothd through `alpha`'s relay.
The subject cannot link with `alpha` or `beta`, whose allow-lists name
each other only, so the control shows its interface up on the spare and
scanning: a scan decision on `beta`'s address.

The cell, in netns on 2026-11-08 (docker owed: the land gate held the
docker lock), all three arms asserted:

- `arm=no_config ... ble=up origin=default bt_reason=present
  bt_attached=yes bt_lines=1 iface_origin=default layer_bluetooth=yes
  warn=no sighted_link_peer=yes ble_up_on=hci3 sighted=2 bt_adapter=hci3`
- `arm=auto_bluetooth_no shared_instance=yes interfaces=0 ble=absent
  origin=configured bt_reason=absent ... layer_bluetooth=no`
- `arm=no_adapter ... ble=absent bt_reason=no_adapter bt_attached=no
  bt_lines=1 iface_origin=none layer_bluetooth=yes warn=yes`

## The embedded daemon in the tools

lnomad and the other tools connect to a running instance first. Only
when none answers do they start one: lnsd as a child process with
`PR_SET_PDEATHSIG` (`spawn_supervised`, `leviculum-std/src/process.rs:194`),
on the same storage path and the same shared-instance port as a
standalone lnsd, so identity and known destinations are shared and a
second instance never opens a second port in silence. The function
"find the instance or start one" lives in `leviculum-std`, with one
caller per tool. Decided by Lew on 2026-10-23 (#471); built as step 4.

## Step 4, as built

The function is `find_or_start` (`leviculum-std/src/instance.rs`), and
`for_tool` beside it is what a tool calls with its flags:

- **The find**, the way the stock tools look: the data channel
  `rns/<instance>` on the abstract AF_UNIX socket, then TCP loopback at
  the configured `shared_instance_port`, else 37428. The TCP fallback is
  asked only for the instance `default` or a configured port, where it can
  be the same instance; a config that says `shared_instance_type = tcp` is
  asked on TCP only. Each channel is asked until `FIND_BOUND` (150 ms) has
  passed: a loopback connect is refused at once, so the bound covers only
  a daemon caught binding.
- **The start**, when nothing answers: `lnsd -c <config_dir>`, with
  `--instance-name <instance>` when the tool asks for a name the config
  does not give (without it the daemon would answer under the config's name
  and the tool would wait out the bound, 1570), the lnsd
  from `LEVICULUM_LNSD`, else beside the tool, else on `PATH`, through
  `spawn_supervised`, its stdout and stderr appended to `lnsd.log` in the
  config directory. The tool waits until the channel accepts, at most
  `START_BOUND` (20 s). On an empty directory lnsd writes its first-start
  file, so the tool reads its configuration again afterwards. A config with
  `share_instance = No` starts nothing (a daemon without a shared instance
  serves no tool).
- **The line**, catalogued as `INSTANCE`: `INSTANCE found=yes via=unix|tcp
  pid=none` or `INSTANCE started=yes via=unix|tcp pid=<pid>`. The command
  line tools print it on stderr after a start, and after a find too under
  `-v`; Loquax writes it to its log only, its screen being its stderr.
- **Loquax's lookup** (1598): its window and terminal ask for a running
  daemon by `--instance` (or `--config`) alone, else by the name in
  `$LNMSG_HOME/reticulum/config`, then the one in `~/.reticulum/config`,
  then as `default`, and only when none answers start lnsd in
  `$LNMSG_HOME/reticulum` (1570), its `INSTANCE` line carrying
  `source=flag|home|default-dir|none`.
- **Two tools at once.** The find and the start run under an exclusive
  `flock` on `.lnsd-start.lock` in the config directory: the first starts
  the daemon, the second finds it once it holds the lock. Two directories
  naming one instance both start an lnsd; the second cannot bind the name
  and exits, and its tool reports `found`. Held by
  `two_callers_at_once_start_one_daemon`
  (`leviculum-cli/tests/find_or_start.rs`), against the real lnsd.
- **The daemon goes with the tool**: dropping the returned instance stops
  it (`SIGTERM`, then `SIGKILL` after 3 s), and a tool that dies otherwise
  leaves it to the kernel.
- **The callers.** `lnstatus`, `lncp`, `lnprobe` and `lnpath` before they
  attach or ask over the RPC; `lnomad` before its session, terminal and
  window alike; Loquax's terminal and window in `shell::start`, beside the
  shared instance they attached to before. Each has `--no-start`, which
  keeps the old failure for scripts. `lnstest` has no caller: `selftest`
  and `connect` dial TCP addresses, and `diag` reads a daemon that runs,
  which a started one would not answer for. Loquax's command line
  subcommands (`send`, `fetch`, the mailbox) keep their own decision, the
  mailbox's being 1354's.

The cells `zero_config_tool_starts_the_daemon` and
`zero_config_tool_finds_the_daemon` hold it, in netns and docker. Against
master before step 4 both were red (`instance=none ... tool_exit=1`: "No
shared RNS instance available to get status from").

## The steps, and the cells that hold each

The cells were written first, each under five minutes, each printing its
line today and gaining its `expect` when its step lands. Their lines on
2026-10-23, all green as measurements:

| step | what | cell | line today |
|---|---|---|---|
| 0 | the cells | all five, and the corpus guard | |
| 1 (built) | default configuration in memory with AutoInterface (what rnsd writes to disk), a shared instance, transport off, `lnsd --writeconfig`, origin in `lnstatus` behind `--origin` | `zero_config_lan_neighbour` | before: `shared_instance=no interfaces=0 neighbour=none neighbour_by=log`; since step 1: `shared_instance=yes interfaces=2 neighbour=found neighbour_by=path_request origin=default` (from `neighbour=` on asserted; `interfaces=` counts the two AutoInterface peers) |
| 2 (built) | the `[auto]` section, the per-medium rule, the host marker and `LEVICULUM_NO_AUTO`, the `AUTO_LAYER` line | `config_present_means_only_config`; `periculum auto-layer` | `configured=1 running=1 extra=none serial_bytes=0` (asserted); `absent=115` |
| 3 (built) | the server list, choice persisted and logged, pinnable, fail-over, autoconnect ceiling | `zero_config_server_list` | before: `shared_instance=no connected=none discovered=0 failover=n/a`; since 1510: `config=default chosen=absent.invalid:4242,entry:4242 why=pinned failover=absent.invalid:4242tobackbone:4242 up=2`, after a restart `config=file ... why=persisted` (asserted) |
| (built) | lnsd writes its first-start file (open question 1) | `zero_config_first_start_file` | `file=yes equals_writeconfig=yes rnstatus_wrote=none config_unchanged=yes control_wrote=yes` (asserted) |
| 4 (built) | "find the instance or start one" in `leviculum-std`, then lnomad and the other tools | `zero_config_tool_starts_the_daemon`; `zero_config_tool_finds_the_daemon` | before: `instance=none daemon_parent=none ... tool_exit=1`; `instance=none`; since step 4: `before=none instance=started via=unix daemon_parent=tool daemon_after_tool=gone socket_after_tool=closed config_written=yes daemon_log=yes`; `instance=found via=unix lnsd_on_dir=1 tool_children=0 subject_after_tool=alive` (asserted) |
| 5 (built) | serial: LNodes by string, RNodes by silence plus `CMD_DETECT`, `TIOCEXCL`, bootloader ids, hotplug | `serial_probe_writes_nothing_to_strangers`; `zero_config_serial_lnode`; `zero_config_serial_held` | before: `strangers=0 quiet=0 talker=0 rnode=0 rnode_probe=absent`; since 1555: `strangers=0 quiet=0 talker=0 rnode_probe=seen lnode_attached=0 rnode_attached=1`; `attached=1 verdict=lnode:name interface=yes up=yes phy=869463000/125000/8/5 debug_bytes=0`; `written=0 attached=0 held=2` (asserted) |
| 6 (built) | BLE on when an adapter is present, the lab guard measured | `ble_room_auto_start` | before: `arm=no_config shared_instance=no interfaces=0 ble=absent`; since 1566: `arm=no_config ... ble=up ... bt_reason=present bt_attached=yes iface_origin=default layer_bluetooth=yes sighted_link_peer=yes`; `arm=auto_bluetooth_no ... ble=absent`; `arm=no_adapter ... bt_reason=no_adapter warn=yes layer_bluetooth=yes` (asserted); the rig twin is the reviewer's |
| (built) | the Debian package ships no stock configuration | `just verify-deb` | `ships no /etc/reticulum/config` |
| | the first server entry's monitoring, privacy statement and consent | none, an operating duty | |

The Debian package shipped `/etc/reticulum/config` until 1510, Python's
stock file line for line. Under the rule above that file was a
configuration present, so a package install was not zero-config. The
package ships none now ("goes", not an `[auto]`-only file: the first-start
file makes a shipped one unnecessary), `lnsd.service` keeps starting
`lnsd -c /etc/reticulum` (`packaging/debian/lnsd.service:22`), and
postinst only provisions the directory, which the unit's `ReadWritePaths`
needs. The first start writes the file. An upgrade from a package that
shipped one leaves it to dpkg's obsolete-conffile rule: the file stays,
byte for byte, edited or not, and keeps meaning what it meant (no
`[auto]`, so no automatic medium); measured in a container on 2026-11-07.

The cells run a second lnsd inside a periculum node, `-c` on an empty
directory, because a node's own daemon is always rendered a file. Each of
the first four carries a control arm that reads the positive answer today
(rnsd's first-start file finds the LAN neighbour by path request; one
configured TCP client connects and stores the second server's discovery;
an RNodeInterface on the candidate pty writes the detect request), so a
`none` is the daemon's and not the script's. `ble_room_auto_start` got
its control with step 6: a spare btvirt controller its node's own daemon
does not name (Step 6, as built).

## Open questions

1. **A Python tool writes the opt-out file.** Closed 2026-11-07, the
   reviewer: lnsd writes its own first-start file (above). Pointed at a
   zero-config lnsd's directory, `rnstatus` had created rnsd's stock file
   there, and the next lnsd start was "config present": the server list,
   BLE and serial stopped without the user having changed anything.
2. **Which AutoInterface group.** A fresh install joins the default
   `group_id`, `reticulum`. The cells had to render their AutoInterface
   links with that group, because periculum derives one per cell by
   default, which a configless daemon never joins.
3. **The ceiling numbers.** Two `bootstrap_only` entries and two to three
   discovered are the starting values; a measurement moves them.
4. **Zero-config against the propagation node picker.** Closed 2026-10-25, Lew:
   the picker gives way. The messenger adopts a propagation node by itself and
   shows it, never the node on its own device; see the amendment in
   [Loquax: the mailbox](loquax-mailbox.md).

## Decision record

- 2026-10-23, Lew: a fresh start brings up AutoInterface, two TCP entries
  from a shipped list (first `leviculum.network`), discovery and
  autoconnect with a ceiling, BLE when an adapter is present, serial
  LNodes by their USB strings and RNodes after silence and a probe; all
  found boards are attached.
- 2026-10-23, Lew: a configuration file is the opt-out, per medium, with
  an `[auto]` section in the upstream file; no format of our own.
- 2026-10-23, Lew: BLE on by default in the shipped builds; the lab is
  protected by configuration and a host marker, not by a test build.
  Built as step 6 (1566): `[auto] bluetooth = yes` is a shipped default;
  the adapter is the BLE interface's own default, a missing one is
  asked for again every 5 s.
- 2026-10-23, Lew: further server operators only with their consent.
- 2026-10-23, Lew: serial boards are part of zero-config; the lab guard
  (a configuration present means only the configuration) stays. Built as
  step 5 (1555): `[auto] serial = yes` is a shipped default; RNode
  candidates by the rig's measured bridge ids, hotplug by a 5 s rescan.
- 2026-11-07, the reviewer (open question 1): lnsd writes its own
  configuration on first start, the way rnsd does (`Reticulum.py:336`),
  the exact bytes `lnsd --writeconfig` prints, `[auto]` fully populated at
  its defaults. A Python tool pointed at that directory afterwards finds a
  file and writes nothing, and lnsd reads its own file as what it is:
  every medium at its default. The per-medium opt-out rule stays.
  Rejected: recognising Python's stock file by its bytes (fragile across
  rnsd versions), and the shared instance telling clients its directory
  (the stock tools have no such request). The lab guard is unaffected:
  periculum renders every node a file.
- 2026-10-25, Lew: zero-config is the rule for every device, not only
  lnsd: switch on and it works for nine users in ten, radio and
  networking preset on every medium; the tenth configures. A board ships
  Mesh mode on the default PHY ([Operating modes](operating-modes.md),
  [How far one firmware build reaches](board-support-scope.md)); the
  messenger and the browser start without a settings dialog.

## See also

- [lnsd quickstart](../lnsd-quickstart.md), today's manual path.
- [Self-hosted infrastructure](self-hosted-infrastructure.md), the server
  behind the first list entry.
- [Python-RNS compatibility](python-rns-compatibility.md), why the
  upstream file stays the format.
- [Bluetooth interfaces](bluetooth-interfaces.md) and
  [Flashing an LNode](lnode-flashing.md), the media the automatic layer
  takes.
