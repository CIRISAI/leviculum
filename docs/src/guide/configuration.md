# Configuration

`lnsd` reads the same INI-style configuration file as Python Reticulum
(`rnsd`). The format is a drop-in: a config that `rnsd` accepts, `lnsd`
accepts, and the two share the shared-instance IPC socket so client
tools (`rnstatus`, `rncp`, `lnstest diag`, Sideband, Nomadnet) attach to
either daemon without changes. Keys `lnsd` does not implement are
tolerated, not rejected — an unknown key never makes `lnsd` refuse a
config a current `rnsd` would load (`ini_config.rs:455-460`).

## File location and lookup order

Pass an explicit config directory with `--config DIR` (`lnsd.rs`,
`-c/--config`). With no flag, `lnsd` resolves the directory using the
same order as Python Reticulum (`config.rs:701-718`):

1. `/etc/reticulum` — if `/etc/reticulum/config` exists
2. `$HOME/.config/reticulum` — if that directory's `config` exists
3. `$HOME/.reticulum` — fallback, used even if absent

The config *file* is always named `config` inside that directory
(`config.rs:720-722`). The storage directory defaults to
`<config_dir>/storage` and can be overridden with `--storage`
(`lnsd.rs`, `-s/--storage`).

This order is why the Debian package can install a system-wide config
under `/etc/reticulum` and have Python clients connect to the live
daemon with no extra flags (`config.rs:707-711`).

## INI vs TOML detection

`lnsd` accepts both the Python INI format and native TOML. Detection is
by content, not just extension (`config.rs:662-689`):

- An explicit `.toml` extension forces TOML.
- A file containing `[[` (the ConfigObj subsection marker Python uses
  for interfaces) is parsed as INI.
- Otherwise TOML is tried first, then INI as a fallback.

In practice your `config` file uses the Python INI form shown
throughout this page. Boolean values accept `Yes`, `yes`, `True`,
`true`, `1`, `on` (and their false counterparts); anything else is read
as `false` (`ini_config.rs:919-930`).

## A file with a syntax error is refused

A line that is neither a section header nor a `key = value` pair is a parse
error, and `lnsd` exits non-zero without starting rather than reading the
rest of the file as nothing (`ini_config.rs:206-208`). The error names the
line number and the line, and where the format is ambiguous it reports both
the INI and the TOML verdict (`config.rs:949-956`):

```
$ lnsd --config /etc/reticulum
lnsd: configuration error: Failed to parse config /etc/reticulum/config: not
valid Reticulum INI (Invalid line 1 ('[reticulum'): matched as neither section
nor keyword) and not valid TOML (...)
```

This is the reference's behaviour, not a house rule: ConfigObj raises
`Invalid line ('[reticulum') (matched as neither section nor keyword)`, and
`rnsd` logs `Could not parse the configuration at <path>` and exits 255
(`RNS/Reticulum.py:330-333`). Every bracket shape ConfigObj accepts still
loads here, including a header with a trailing comment (`[reticulum] # note`)
and `[reticulum = x`, which ConfigObj reads as a key rather than a header
(`ini_config.rs:44-62`).

What is NOT refused, because the reference does not refuse it either: an
interface whose `type` we do not implement is skipped with a warning and the
daemon runs with the rest (`ini_config.rs:318-324`), the same way `rnsd` logs
`Could not locate external interface module` and carries on. A key in a
section `lnsd` does not read is likewise kept out of the config and logged
(`ini_config.rs:177-190`).


## The `[reticulum]` section

Core daemon settings. Every key below is parsed in
`ini_config.rs:315-462`; defaults come from `config.rs:213-238`.

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `enable_transport` | bool | `true` | Route announces and serve paths for other peers. `lnsd` defaults this to `true` (it is a daemon); the Python *library* default is `false`. (`config.rs:27-28`, `202`) |
| `use_implicit_proof` | bool | `true` | Use implicit proof for link identification. (`config.rs:30-31`, `203`; `ini_config.rs:417-419`) |
| `share_instance` | bool | `false` | Listen on the abstract Unix socket `\0rns/<instance_name>` for local clients. Required for `lnstest diag`, `rnstatus`, Sideband etc. to attach. (`config.rs:37-40`, `205`; key `share_instance` → `shared_instance`, `ini_config.rs:410-412`) |
| `instance_name` | string | `default` | Names the shared-instance socket: `\0rns/<instance_name>`. Use a unique name to run two daemons side by side. (`config.rs:41-44`, `206`; `ini_config.rs:333-335`) |
| `shared_instance_type` | `unix`/`tcp` | unset | Parsed for `rnsd` compatibility. Only `tcp`/`unix` are stored; `tcp` clears `shared_instance_socket` (tcp disables AF_UNIX upstream). `lnsd` currently serves only the abstract AF_UNIX socket. (`config.rs:45-52`; `ini_config.rs:381-390`, `179-181`) |
| `shared_instance_socket` | path | unset | Explicit AF_UNIX socket path (RNS 1.3.x). Parsed for compatibility; cleared when `shared_instance_type = tcp`. (`config.rs:53-58`; `ini_config.rs:391-393`) |
| `respond_to_probes` | bool | `false` | Answer `rnprobe` requests by signing a proof for each probe packet. (`config.rs:54-60`, `146`; `ini_config.rs:398-400`) |
| `remote_management_enabled` | bool | `false` | Enable remote management. (`config.rs:61-63`, `147`; `ini_config.rs:415-419`) |
| `storage_path` | path | unset | Where identity, known destinations and packet hashlist live. Relative values resolve against the config dir. (`config.rs:97-98`; `storage_path` (`ini_config.rs:553`)) |
| `flush_interval` | u64 (sec) | `3600` | Seconds between periodic storage flushes. Crash protection only — normal shutdown always flushes. (`config.rs:67-73`, `149`; `ini_config.rs:427-431`) |
| `control_channel_capacity` | usize | `256` | Capacity of the lossless control-plane event channel (announces, paths, link/resource lifecycle). Raise on servers under heavy announce load. (`config.rs:74-82`, `150`) |
| `data_channel_capacity` | usize | `128` | Capacity of the droppable data-plane event channel; full means normal backpressure (silent drop). Reliable channel messages are exempt: the node stops proofing them to the sender instead of dropping them, so this value also bounds how far a slow reader lets a channel run ahead. (`config.rs:83-90`, `151`) |
| `keepalive_interval` | u64 (sec) | unset | Override link keepalive interval. When set, every link uses this interval and the stale-link timeout scales with it (stale after twice the keepalive). Local timing only, no wire change. Useful for slow links. (`config.rs:91-98`, `152`; `ini_config.rs:432-439`) |
| `storage_profile` | `desktop`/`compact` | `desktop` | Transport-table sizing profile (Codeberg #421). `compact` is sized to leave a Raspberry Pi Zero 2W (512 MB shared with the GPU, no swap) usable. An unrecognised value keeps `desktop`. (`config.rs:197-205`; `ini_config.rs:485-496`) |
| `path_table_cap` | usize | profile | Maximum `path_table` entries, and with them `path_states`, `path_requests` and `discovery_path_requests`. Desktop `32768`, compact `8192`. The path table expires after seven days, so on a node up less than a week this is its only bound. (`config.rs:206-213`; `ini_config.rs:497-499`) |
| `reverse_table_cap` | usize | profile | Maximum `reverse_table` entries. Desktop `200000`, compact `16384`. Entries expire after 8 minutes, so the working size is forwarding rate times that window; a field node measured 73 901. (`config.rs:214-221`; `ini_config.rs:500-502`) |
| `link_table_cap` | usize | profile | Maximum `link_table` entries — links this node routes for, not its own (that is `max_links`). Desktop `8192`, compact `1024`. (`config.rs:222-229`; `ini_config.rs:503-505`) |
| `announce_table_cap` | usize | profile | Maximum `announce_table` entries, the pending-rebroadcast queue. Each holds a full copy of an announce packet. Desktop `16384`, compact `2048`. (`config.rs:230-236`; `ini_config.rs:506-508`) |
| `destination_cap` | usize | profile | Maximum entries in the destination-keyed tables: `announce_cache`, `announce_rate_table`, `known_ratchets`, `known_dest_use`. Desktop `50000`, compact `4096`. One key for four tables because they share one population. (`config.rs:237-247`; `ini_config.rs:509-511`) |

`control_channel_capacity` and `data_channel_capacity` are read from TOML
only; they have no INI key in `apply_reticulum_key` (`ini_config.rs:280-401`)
and are best set in a TOML config or left at their defaults.

`storage_path` is read from both formats and resolves in one order
everywhere: `lnsd --storage`, then the config key, then
`<config_dir>/storage` (Python's only choice, `Reticulum.py:246`). The
client tools resolve it the same way (`resolve_storage_path`,
`config.rs:1057`), so `lnstatus`, `lncp`, `lnpath` and `lnprobe` open the
same directory as the daemon and derive the same RPC authkey from its
`transport_identity`. Point the key at an external disk and nothing else
has to be told about it — but note that `--storage` moves the daemon
alone, and the clients then still follow the config.

`flush_interval` and `keepalive_interval` are Leviculum tuning
extensions — Python Reticulum ignores them. Battery-powered or SD-card
deployments may want a longer `flush_interval`; slow links benefit from
a fixed `keepalive_interval`:

```ini
[reticulum]
  # Seconds between periodic storage flushes (crash protection only,
  # normal shutdown always flushes). Default: 3600.
  flush_interval = 3600

  # Link keepalive interval in seconds. When set, every link uses this
  # interval instead of the RTT-derived default. Default: unset.
  keepalive_interval = 360
```

### Table ceilings (Codeberg #421)

Every transport table has a maximum size, and a full table evicts rather
than refuses: the new entry always lands, an old one goes. Without a
ceiling each table's real bound was arrival rate times expiry window — a
number the neighbours choose, not the operator.

The eviction order is oldest-first for most tables and argued per table in
`TableCaps` and the field docs of `memory_storage.rs`; three tables
deviate, because plain FIFO would do damage there: `link_table` drops an
unvalidated link request before a live link, `announce_cache` drops an
unretained destination before a pinned one, and `receipts` drops a
terminal receipt before a pending one — and a pending one it does have to
drop is still reported as a timeout, which is what Python does on the same
overflow (`Transport.py:558-561`).

The defaults are `entries × modelled bytes` from the same model the
`lnstatus` diagnostic dump prints, not round numbers: desktop totals about
339 MB across the tables these keys bound, compact about 22 MB. Pick the
profile first and override individual tables only where the deployment
differs:

```ini
[reticulum]
  # A Pi Zero 2W with a busy uplink: compact everywhere, but a reverse
  # table large enough for the traffic it actually forwards.
  storage_profile = compact
  reverse_table_cap = 40000
```

## The `[interfaces]` section

Interfaces are ConfigObj subsections under `[interfaces]`, each named in
double brackets `[[Name]]`. The name is free-form; the `type` key
selects the interface implementation. Twelve types pass the
supported-type filter (`interface_type` (`ini_config.rs:292-316`)):

`TCPServerInterface`, `TCPClientInterface`, `UDPInterface`,
`AutoInterface`, `RNodeInterface`, `RNodeMultiInterface`,
`SerialInterface`, `PipeInterface`, `KISSInterface`,
`AX25KISSInterface`, `I2PInterface`, `BLEInterface`.

`BackboneInterface` and `BackboneClientInterface` are accepted too:
they are wire-identical to TCP and are mapped onto the TCP interface at
parse time, as Python does (`normalize_backbone_interface`
(`ini_config.rs:852-882`)). An interface of any other type is skipped
with a log line (`tracing::warn` (`ini_config.rs:306-314`)), not an
error.

The per-type tables below cover the six types most deployments use. All
interface keys are parsed in `apply_interface_key`
(`ini_config.rs:431-616`); the struct they land in, with its defaults,
is `InterfaceConfig` (`config.rs:242-520`).

### Keys common to every interface

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `type` | string | (required) | Interface type, one of the eleven above. (`type` (`ini_config.rs:615`)) |
| `enabled` | bool | `true` | Bring this interface up; the legacy spelling `interface_enabled` is honoured too. (`enabled` (`ini_config.rs:616`); `InterfaceConfig::enabled` (`config.rs:334-336`)) |
| `outgoing` | bool | `true` | Allow sending outgoing packets. (`outgoing` (`ini_config.rs:620`); `InterfaceConfig::outgoing` (`config.rs:385-387`)) |
| `bitrate` | u64 (bps) | per type | Override the interface's own bitrate figure, which feeds announce bandwidth capping and timing. Values below `MINIMUM_BITRATE` (`constants.rs:335-338`), 5 bps, are ignored. (`bitrate` (`ini_config.rs:673-679`); `InterfaceConfig::bitrate` (`config.rs:388-393`)) |
| `buffer_size` | usize | per type | Channel buffer size. (`buffer_size` (`ini_config.rs:724`); `InterfaceConfig::buffer_size` (`config.rs:526-528`)) |

### TCP server (`TCPServerInterface`)

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `listen_ip` | string | unset | Address to bind. (`listen_ip` (`ini_config.rs:627`)) |
| `listen_port` | u16 | unset | Port to listen on. (`listen_port` (`ini_config.rs:628`)) |

```ini
[interfaces]
  [[Loopback TCP]]
    type = TCPServerInterface
    enabled = Yes
    listen_ip = 127.0.0.1
    listen_port = 45999
```

### TCP client (`TCPClientInterface`)

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `target_host` | string | unset | Remote host to connect to. (`target_host` (`ini_config.rs:629`)) |
| `target_port` | u16 | unset | Remote port. (`target_port` (`ini_config.rs:630`)) |
| `reconnect_interval` | u64 (sec) | `5` | Delay between reconnect attempts. (`reconnect_interval` (`ini_config.rs:725`); `InterfaceConfig::reconnect_interval_secs` (`config.rs:528-529`)) |
| `max_reconnect_tries` | u64 | unlimited | Give up after this many attempts; unset means never. (`max_reconnect_tries` (`ini_config.rs:726`); `InterfaceConfig::max_reconnect_tries` (`config.rs:530-531`)) |

```ini
[interfaces]
  [[RNS TCP Node Germany 002]]
    type = TCPClientInterface
    enabled = Yes
    target_host = 193.26.158.230
    target_port = 4965
```

### UDP (`UDPInterface`)

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `listen_ip` | string | `0.0.0.0` | Local bind address. (`listen_ip` (`ini_config.rs:627`)) |
| `listen_port` | u16 | unset | Local bind port. (`listen_port` (`ini_config.rs:628`)) |
| `forward_ip` | string | unset | Broadcast/forward address or hostname. Names are resolved at runtime and re-resolved periodically; a resolution failure is a logged interface error, not a config error. (`forward_ip` (`ini_config.rs:638`); `InterfaceConfig::forward_ip` (`config.rs:465-475`)) |
| `forward_port` | u16 | unset | Broadcast/forward port. (`forward_port` (`ini_config.rs:639`)) |
| `port` | u16 | unset | Fills both `listen_port` and `forward_port`; either explicit key wins over it. (`port` (`ini_config.rs:528`)) |
| `device` | string | unset | Kernel interface name; its IPv4 broadcast address fills both `listen_ip` and `forward_ip`. Either explicit key wins over it. (`device` (`ini_config.rs:625`)) |

Bind and forward are independent, as in `rnsd`: an interface with only bind
parameters receives without transmitting, one with only forward parameters
transmits without listening, and only an interface that would do neither is a
configuration error.

### AutoInterface (`AutoInterface`)

Discovers other Reticulum nodes on the same broadcast domain via
multicast. No router or DHCP needed; the link must carry multicast.

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `group_id` | string | unset | Multicast group identifier; isolate co-located meshes by setting different IDs. (`group_id` (`ini_config.rs:738`); `InterfaceConfig::group_id` (`config.rs:534-536`)) |
| `discovery_scope` | string | unset | Multicast scope: `link`, `admin`, `site`, `organisation`, `global`. (`discovery_scope` (`ini_config.rs:739`); `InterfaceConfig::discovery_scope` (`config.rs:536-537`)) |
| `discovery_port` | u16 | `29716` | Discovery (announce) port. (`discovery_port` (`ini_config.rs:740`); `InterfaceConfig::discovery_port` (`config.rs:538-539`)) |
| `data_port` | u16 | `42671` | Data port. (`data_port` (`ini_config.rs:741`); `InterfaceConfig::data_port` (`config.rs:540-541`)) |
| `devices` | string (CSV) | unset | Whitelist of NIC names to use. (`devices` (`ini_config.rs:742`); `InterfaceConfig::devices` (`config.rs:542-543`)) |
| `ignored_devices` | string (CSV) | unset | Blacklist of NIC names to skip. (`ignored_devices` (`ini_config.rs:743`); `InterfaceConfig::ignored_devices` (`config.rs:544-545`)) |
| `multicast_loopback` | bool | unset (inherits `true`) | Multicast loopback (`IPV6_MULTICAST_LOOP`), the carrier self-echo mechanism. Unset inherits the default `true`, matching Python-RNS; set `no` to opt out. (`multicast_loopback` (`ini_config.rs:744`); `InterfaceConfig::multicast_loopback` (`config.rs:548-551`)) |
| `multicast_address_type` | string | unset (inherits `temporary`) | Multicast address type of the discovery group, `temporary` or `permanent`. It is part of the group address, so peers must agree on it: an `lnsd` node left on the default next to a `permanent`-type `rnsd` peer group discovers nobody, and nothing on either side says why. Unset inherits `temporary`, the group Python joins when the key is absent. A value that is neither spelling is refused at startup rather than resolved to `temporary` the way Python resolves it. (`multicast_address_type` (`ini_config.rs:746-752`); `InterfaceConfig::multicast_address_type` (`config.rs:550-555`); `MulticastAddressType` (`interfaces/auto_interface/mod.rs:42-91`)) |

### BLE (`BLEInterface`)

Joins the Columba BLE mesh (the `ble-reticulum` protocol, v2.2 wire
format with the v0.3.0 capability record) as a dual-role BlueZ node: it
advertises and serves the Columba GATT layout like an LNode board does,
and it scans for and connects to nearby peers under the same
connection-direction rule the boards and phones apply. One section is
one Reticulum interface — a single broadcast domain across all live BLE
links. Requires a BlueZ (`bluetoothd`) host with a BLE-capable adapter;
if the adapter is missing or powered off at startup the interface keeps
retrying rather than failing the daemon.

The interface is off unless a `[[BLE Interface]]` section exists in the
config; the daemon never brings BLE up on its own. The advertised name
is derived from the daemon identity as `LN-<hex8>` exactly like the
firmware's, so scanner listings show lnsd and boards the same way.

Key names follow the reference `ble-reticulum` package where its options
map onto this implementation:

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `device` | string | default adapter | BlueZ adapter to use, e.g. `hci0`. (`device` (`ini_config.rs:625`)) |
| `max_connections` | usize | `4` | Simultaneous BLE link cap, both GATT roles counted together. The default is the firmware's `MAX_LINKS` (4), not the reference's 7: 3-4 links is the protocol's reliable ceiling. (`max_connections` (`ini_config.rs:810`); `InterfaceConfig::max_connections` (`config.rs:669-672`)) |
| `min_rssi` | i16 (dBm) | `-85` | Sightings weaker than this are not dialled. (`min_rssi` (`ini_config.rs:811`); `InterfaceConfig::min_rssi` (`config.rs:673-675`)) |
| `discovery_interval` | f64 (sec) | `5` | Pause between the 2-second BLE scan windows. (`discovery_interval` (`ini_config.rs:812`); `InterfaceConfig::discovery_interval` (`config.rs:676-678`)) |
| `enable_central` | bool | `true` | Run the scanning + dialling central role. (`enable_central` (`ini_config.rs:813`); `InterfaceConfig::enable_central` (`config.rs:679-681`)) |
| `enable_peripheral` | bool | `true` | Run the advertising + GATT-server peripheral role. Disabling both roles is a config error. (`enable_peripheral` (`ini_config.rs:814`); `InterfaceConfig::enable_peripheral` (`config.rs:681-683`)) |
| `initiate_only` | string (CSV) | unset (every peer) | Peers this interface may DIAL: BLE addresses (`AA:BB:CC:DD:EE:FF`, `-` or no separator) or peer identities in hex, 8 digits (the four bytes an advertiser publishes as its hint) or all 32 (a board's `[IDENTITY]` line, truncated to those four). Unset or empty dials whoever the connection-direction rule picks, the behaviour that predates the key. It narrows dialling and nothing else: a peer left off the list that connects to US is admitted and served exactly as before, and nothing on the wire changes — it sees a node that has not dialled it yet. The digit count decides which is which — 12 is an address, 8 or 32 an identity — so both spellings can be copied out of a log line (`BLE_SCAN_DECISION addr=`, a board's `BLE_CENTRAL_ADDR`, its `[IDENTITY]`). A malformed entry is a startup error, not a dropped line. (`initiate_only` (`ini_config.rs:817-819`); `InterfaceConfig::initiate_only` (`config.rs:685-694`); `PeerAllowlist` (`interfaces/ble/links.rs:1014`)) |
| `accept_only` | string (CSV) | unset (every peer) | Peers whose INCOMING link this interface SERVES — the symmetric counterpart of `initiate_only`, same vocabulary, same validation, same "unset or empty means everyone". A non-empty list narrows who we serve and nothing else: a peer left off it is still dialled if `initiate_only` allows it, and a peer left off `initiate_only` is still served if this list names it. An unlisted peer that connects is refused at the identity handshake — the first moment an inbound BLE connection has said who it is, since under RPA its address names nobody — so it never becomes a link, never enters the fan-out and is never reported to the core as a peer. Each refusal emits one `BLE_LINK_NOT_ADMITTED peer=<hex8> identity=<hex32> addr=<a> role=peripheral listed=<n> action=disconnect` line, so a run that turned strangers away is distinguishable from a run nobody tried. A malformed entry is a startup error, not a dropped line. (`accept_only` (`ini_config.rs:823-825`); `InterfaceConfig::accept_only` (`config.rs:695-710`); the refusal point (`interfaces/ble/links.rs:603`)) |

```ini
[interfaces]
  [[BLE Interface]]
    type = BLEInterface
    enabled = yes
    # device = hci0
    # max_connections = 4
    # min_rssi = -85
    # Dial nothing but these two; still answer anyone who dials us.
    # initiate_only = b2a8bea1, AA:BB:CC:DD:EE:FF
    # Serve nothing but these two; a stranger's connection is refused.
    # accept_only = b2a8bea1, AA:BB:CC:DD:EE:FF
```

A node that should be a leaf rather than a hub — one uplink out, still
reachable from anybody near it — is `initiate_only` naming that uplink.
A node that should be a leaf and invisible as well adds
`enable_peripheral = no`, which is the stronger statement: it stops
advertising, so no peer can dial it either.

The two keys answer independent questions and `accept_only` is the one
for a room the operator does not control. `enable_peripheral = no`
refuses every incoming link, including the ones the deployment wants;
`accept_only` names the set it wants and refuses the rest, which is what
a measurement in a flat needs — a Faraday cage stops LoRa, it does not
stop the phone in someone's pocket from dialling a Columba advertiser.
Setting both keys to the same list pins a closed mesh: we dial nobody
else and we serve nobody else. Setting neither is the default and is
what every existing deployment does.

### RNode and Serial (`RNodeInterface`, `SerialInterface`)

`RNodeInterface` drives an RNode LoRa modem; `SerialInterface` is a raw
serial HDLC link. They share the serial-port and LoRa keys.

Divergence from Python: there, only `RNodeInterface` honours the LoRa
keys (`frequency`, `bandwidth`, `spreadingfactor`, `codingrate`,
`txpower`) and `SerialInterface` reads port settings only. Leviculum's
`SerialInterface` honours them too and configures the attached LNode's
radio over the serial port — the LNode frames HDLC, so it cannot be
driven by the KISS-framed `RNodeInterface`.

Serial keys:

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `port` | string | unset | Serial device path, e.g. `/dev/ttyACM0`. (`port` (`ini_config.rs:528`); `InterfaceConfig::port` (`config.rs:479-480`)) |
| `speed` / `baudrate` | u32 | unset | Serial baud rate (either spelling). (`speed` (`ini_config.rs:641`); `InterfaceConfig::speed` (`config.rs:482-483`)) |
| `databits` | u8 | unset | Data bits. (`databits` (`ini_config.rs:642`); `InterfaceConfig::databits` (`config.rs:484-485`)) |
| `parity` | string | unset | `none`, `even`, or `odd`. (`parity` (`ini_config.rs:643`); `InterfaceConfig::parity` (`config.rs:486-487`)) |
| `stopbits` | u8 | unset | Stop bits. (`stopbits` (`ini_config.rs:644`); `InterfaceConfig::stopbits` (`config.rs:488-489`)) |

LoRa keys, derived from source — the meanings below describe the radio
parameters the interface configures; the fields sit together in the
RNode block of `InterfaceConfig` (`InterfaceConfig::frequency`
(`config.rs:554-585`)):

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `frequency` | u64 (Hz) | unset | LoRa centre frequency. (`frequency` (`ini_config.rs:727`); `InterfaceConfig::frequency` (`config.rs:589-591`)) |
| `bandwidth` | u32 (Hz) | unset | LoRa bandwidth. (`bandwidth` (`ini_config.rs:679`); `InterfaceConfig::bandwidth` (`config.rs:568-569`)) |
| `spreadingfactor` / `spreading_factor` | u8 | unset | LoRa spreading factor (either spelling). (`spreadingfactor` (`ini_config.rs:729`); `InterfaceConfig::spreading_factor` (`config.rs:610-611`)) |
| `codingrate` / `coding_rate` | u8 | unset | LoRa coding rate (either spelling). (`codingrate` (`ini_config.rs:730`); `InterfaceConfig::coding_rate` (`config.rs:612-613`)) |
| `txpower` / `tx_power` | i8 (dBm) | unset (resolves to the board maximum, 22 dBm) | Transmit power (either spelling). Unset asks the board for its maximum — a board that can do less clamps and says so — rather than the 0 dBm (1 mW) Python-Reticulum resolves it to, which has no symptom at the node. An explicit `txpower = 0` still means 0. Above roughly 7 dBi of antenna gain, 22 dBm conducted exceeds the EU 27 dBm ERP allowance and has to be set down. (`txpower` (`ini_config.rs:731`); `InterfaceConfig::tx_power` (`config.rs:614-615`); `resolve_tx_power` (`rnode.rs:702`); [deviation](../concepts/python-rns-compatibility.md)) |
| `flow_control` | bool | unset | Wait for the RNode's `CMD_READY` before the next TX. (`flow_control` (`ini_config.rs:752`); `InterfaceConfig::flow_control` (`config.rs:632-633`)) |
| `airtime_limit_short` | f64 (%) | unset | Short-term airtime cap, percent (0.0–100.0). (`airtime_limit_short` (`ini_config.rs:753`); `InterfaceConfig::airtime_limit_short` (`config.rs:634-635`)) |
| `airtime_limit_long` | f64 (%) | unset | Long-term airtime cap, percent (0.0–100.0). (`airtime_limit_long` (`ini_config.rs:754`); `InterfaceConfig::airtime_limit_long` (`config.rs:636-637`)) |
| `csma_enabled` | bool | unset | Carried in the LNode radio-config frame and reported back, but current firmware no longer obeys it: LoRa channel access (pre-TX jitter plus CAD listen-before-talk) is always on, matching the RNode firmware, which offers no CSMA disable either. Only firmware older than the change still honours the flag. (`csma_enabled` (`ini_config.rs:755`); `InterfaceConfig::csma_enabled` (`config.rs:638-639`)) |
| `preamble_symbols` | u16 (symbols) | unset (derived from the PHY) | LoRa preamble length pushed to LNode firmware in the radio-config frame (`SerialInterface` only). Unset derives what an RNode peer programs for the same PHY — 24 symbols at SF7/BW125, the 18-symbol floor from SF8 down — so a mixed pair agrees on the wire; set it only to pin a value against a non-conforming peer. A pin above roughly 20 symbols / 164 ms on air is warned about at startup and not refused: SX127x receivers (every RNode) were measured going deaf above that, losing every frame from the interface silently and one-way, while an SX126x peer copes ([Codeberg #315](https://codeberg.org/Lew_Palm/leviculum/issues/315)). Not the same key as the KISS `preamble` (TX delay in ms), which never reaches a LoRa modem. (`preamble_symbols` (`ini_config.rs:736`); `InterfaceConfig::preamble_symbols` (`config.rs:618-630`); `derive_preamble_symbols` (`rnode.rs:858`); `preamble_ceiling_warning` (`interfaces/serial.rs`)) |

#### A board that does not take the config

A `SerialInterface` with a LoRa block pushes the radio config at the board up
to three times, waiting 2 s for the firmware's ACK each time. Then — ACK or no
ACK — it sends the radio query (`TYPE_RADIO_QUERY`,
[Codeberg #349](https://codeberg.org/Lew_Palm/leviculum/issues/349)) and waits
a further 2 s for the board's report, which the firmware answers out of what
its LoRa task actually configured. The ACK alone does not settle it: the
legacy config frame's entire vocabulary is three bytes or silence, so a board
that only wrote the config to its flash page acks it exactly like a board that
keyed it ([Codeberg #363](https://codeberg.org/Lew_Palm/leviculum/issues/363)).

* **Report received, same profile** — the board is running what was asked
  for. The interface comes up on it. (Without an ACK this is the same
  outcome: the config did arrive, only its receipt did not.)
* **Report received, different profile** — the interface comes up priced at
  the profile the board reported, and logs
  `RADIO_BRINGUP iface=<name> outcome=running-differs` with the requested and
  the running parameters side by side.
* **Query refused as busy or not-running** — the board is talking and says no
  radio is running this boot, which on an LNode means it booted `lora=off`.
  The interface refuses to come up and logs
  `RADIO_BRINGUP iface=<name> outcome=dead-radio lora=off`. An ACK does not
  change this; it is the case that ACK is unable to distinguish.
* **Neither answered** — the interface refuses to come up. It logs
  `RADIO_BRINGUP iface=<name> outcome=refused` naming both frames and both
  waits, reports Down to `rnstatus`, and the daemon keeps running with its
  other interfaces. It does not retry on that port; fix the board and restart.

Both refusals report Down to `rnstatus` and leave the rest of the daemon
running.

A modem the host cannot price is a modem the host does not drive. Airtime
accounting and transmit spacing are computed from the PHY, so an interface
pricing SF7 in front of a board keying SF12 under-counts duty by an order of
magnitude and hands the serial queue frames faster than the modem can key
them; guessing the other way is a silent lie about airtime that nothing
downstream can tell from a measurement.

Firmware older than #349 does not answer the radio query — it refuses it as a
frame it does not know, or says nothing. That is not a board stating its radio
is off, so such a board still comes up on its ACK, with a debug line saying the
running profile could not be read; an LNode running it that also misses the
config ACK is refused rather than driven blind.
(`radio_bring_up`, `radio_pricing_phy` (`interfaces/serial.rs`))

**Test-only:** `test_drop_direct_ingress` (bool, default off) emulates
out-of-range placement on a co-located rig: the interface drops every
received frame whose wire hops byte (`raw[1]`) is 0 — frames heard
directly from their originator — before they reach the transport, while
relayed copies (hops ≥ 1) pass. Two endpoints with this knob on one
bench are mutually deaf but both hear a relay, giving the A–B–C
repeater topology without attenuators; the consumer is the 3-node relay
hardware scenario (periculum `hardware/lora_3node_relay.toml`).
Python-RNS has no such option — frames are dropped locally on ingress
and nothing on the air changes, so this is a test-harness affordance,
not a wire or semantic deviation. Supported on `RNodeInterface` and
`SerialInterface`; incompatible with IFAC (which prepends material
before the flags byte), and that combination is refused at startup.

### IFAC (Interface Access Codes)

IFAC keys apply to any interface and authenticate / isolate a virtual
network on the link. They are common to all interface types:

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `networkname` / `network_name` | string | unset | Network name for IFAC (either spelling). (`networkname` (`ini_config.rs:789`); `InterfaceConfig::networkname` (`config.rs:598-600`)) |
| `passphrase` / `pass_phrase` | string | unset | IFAC passphrase (either spelling). (`passphrase` (`ini_config.rs:701`); `InterfaceConfig::passphrase` (`config.rs:600-601`)) |
| `ifac_size` | usize (bits) | unset | IFAC size, specified in bits in the file and stored as bytes (`bits / 8`). Values below 8 bits are dropped, as Python drops them, so the interface falls back to its per-type default. (`ifac_size` (`ini_config.rs:791-797`); `InterfaceConfig::ifac_size` (`config.rs:602-603`)) |

`networkname` and `passphrase` are secrets: `lnstest diag` redacts them
before serialising a bundle (see the [`lnstest diag`](lnstest.md#diag) section).

## Example configurations

### Simple AutoInterface node

A node that talks to other Reticulum peers on the same LAN, no transport
routing:

```ini
[reticulum]
  enable_transport = No
  share_instance = Yes

[interfaces]
  [[Default Interface]]
    type = AutoInterface
    enabled = Yes
```

### TCP-server transport node

A routing entrypoint that accepts inbound TCP peers and bridges them
with the local LAN:

```ini
[reticulum]
  enable_transport = Yes
  share_instance = Yes
  instance_name = entrypoint

[interfaces]
  [[Public TCP]]
    type = TCPServerInterface
    enabled = Yes
    listen_ip = 0.0.0.0
    listen_port = 4965

  [[Local LAN]]
    type = AutoInterface
    enabled = Yes
```

### LoRa RNode node

A node on a LoRa RNode modem (radio values below are an EU 868 MHz
example; set them for your region and hardware):

```ini
[reticulum]
  enable_transport = Yes
  share_instance = Yes

[interfaces]
  [[LoRa RNode]]
    type = RNodeInterface
    enabled = Yes
    port = /dev/ttyACM0
    frequency = 867200000
    bandwidth = 125000
    spreadingfactor = 8
    codingrate = 5
    txpower = 14
```

See the upstream
[Reticulum Manual](https://reticulum.network/manual/) for the
protocol-level meaning of the radio and IFAC parameters.
