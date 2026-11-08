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
  (`shared_instance`, `leviculum-std/src/config.rs:635`), where rnsd
  defaults to yes. No client could attach, so lnomad, lnstatus and the
  Python tools saw nothing at all. Every cell line therefore states
  `shared_instance=` first.
- **Transport on.** The default is `enable_transport: true`
  (`enable_transport`, `leviculum-std/src/config.rs:631`), and lnsd said
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
from memory (`load_or_default`, `leviculum-std/src/config.rs:1563`). The
text is `RNSD_DEFAULT_CONFIG` (`leviculum-std/src/config.rs:738`): not
the template literal of `Reticulum.py:1860` but what ConfigObj makes of
it on `write()`, which indents every key by two spaces and drops the
last empty line. Captured from the vendored ConfigObj of the pinned
reference, 4013 bytes. Two tests hold it: its lines are the template's
lines, and, where `python3` runs, the template through the reference's
own ConfigObj is byte for byte the same file. lnsd parses it with its
own INI reader (`rnsd_default`, `leviculum-std/src/config.rs:1549`), so
what it runs and what rnsd would write cannot drift apart. It means:
transport off, a shared instance named `default`, log level 4,
`discover_interfaces` at lnsd's default, and one AutoInterface,
`[[Default Interface]]`, in the default group `reticulum`.

- `lnsd --writeconfig` writes the same text to `<dir>/config` and exits;
  it never replaces an existing file.
- Every interface section carries an origin, `configured` or `default`
  (`InterfaceOrigin`, `leviculum-std/src/config.rs:681`). lnsd logs one
  `IFACE_CONFIG name= type= enabled= origin=` line per section before it
  builds any (`leviculum-std/src/driver/mod.rs:2338`), and once
  `CONFIG source=file|default path=` (`leviculum-cli/src/lnsd.rs:215`).
- `interface_stats` serves the origin as the `origin` key
  (`leviculum-std/src/rpc/handlers.rs:674`), and `lnstatus` prints it,
  behind `--origin`, as an `Origin` line under `Status`. Without the flag
  the render is rnstatus's, which the drop-in parity tests pin
  (`leviculum-std/tests/rnsd_interop/status_parity_tests.rs`); rnsd serves
  no such key, so against rnsd the flag adds nothing. A row named after a section takes that
  section's origin; a row no section names (the shared instance, an
  AutoInterface's peers) takes the origin all sections share
  (`origin_of`, `leviculum-std/src/interfaces/inventory.rs:266`), and
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
AUTO_LAYER state=on reason=default lan=yes servers=no bluetooth=no serial=no
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
join the room.

## The server list, and what the first entry owes

A start without a file opens two TCP connections to entries drawn at
random from a list shipped in the binary, each marked `bootstrap_only`
(`bootstrap_only`, `leviculum-std/src/driver/builder.rs:645`): it exists
to join the network and is detached once enough discovered connections
stand. Discovery is already on by default (`default_discover_interfaces`,
`leviculum-std/src/config.rs:588`); autoconnect is not
(`autoconnect_discovered_interfaces`, `leviculum-std/src/config.rs:656`),
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
2. it stayed silent for one second of listening. A GPS speaking NMEA or
   a printer reporting its status is out, whatever its name.

Then, and only then, one `CMD_DETECT` with `DETECT_REQ`; a device that
answers `DETECT_RESP` is an RNode. lnsd already speaks this handshake
(`detect_on_port`, `leviculum-std/src/interfaces/rnode.rs:1923`; the same
frame is its serial heartbeat, `leviculum-std/src/interfaces/rnode.rs:3076-3078`).
A refused device is remembered and not asked again; hotplug is read from
inotify on the by-id directory. The directory is
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
(`leviculum-core/src/transport.rs:12753`); in full mode a board's join
to its home ends in `NoPath` (1439, measured on the bench).

All found boards are attached. Two RNodes on one frequency in one room
hear the same frames and contend on transmit; that is documented, not
refused.

## BLE

On when an adapter is present (step 6), Columba-compatible. A missing or
powered-off adapter is a warning, not a failed start. `[auto] bluetooth =
no` keeps it off with any other content of the file, which the cell
`ble_room_auto_start` asserts in its second arm today.

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
- **The start**, when nothing answers: `lnsd -c <config_dir>`, the lnsd
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
| 5 | serial: LNodes by string, RNodes by silence plus `CMD_DETECT`, `TIOCEXCL`, bootloader ids, hotplug | `serial_probe_writes_nothing_to_strangers` | `strangers=0 quiet=0 talker=0 rnode=0 rnode_probe=absent` (`strangers=0` asserted) |
| 6 | BLE on when an adapter is present, the lab guard measured | `ble_room_auto_start` | `arm=no_config shared_instance=no interfaces=0 ble=absent`; `arm=auto_bluetooth_no ... ble=absent` (asserted) |
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
`none` is the daemon's and not the script's. `ble_room_auto_start` has no
such control yet: the only controller its subject can reach is the one
its node's own daemon holds, and step 6 needs a node shape that gives a
daemon a controller without a configured BLE interface.

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
- 2026-10-23, Lew: further server operators only with their consent.
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
