# Configuration

`lnsd` reads the same INI-style configuration file as Python Reticulum
(`rnsd`). What the drop-in holds: a config that `rnsd` loads, `lnsd`
loads too; keys `lnsd` does not implement are tolerated, never rejected;
the two daemons serve the same shared-instance sockets, keyed by
`instance_name`, with the same RPC handshake and the same key (the
identity-derived default, or `rpc_key` when the config sets it), so
client tools (`rnstatus`, `rnpath`, `rnprobe`, `rncp`, `lnstest diag`,
Sideband, Nomadnet) attach to either daemon without changes. The
divergences are listed one by one, with both sides' code, in the
shared-instance audit (`docs/src/audits/2026-10-11-shared-instance-rpc.md`).

What it does not hold, so the same file can still give you a different
daemon. Three defaults differ when the config omits the key (audit D12):
`share_instance` is `false` under `lnsd` and `true` under `rnsd`, so a
config that never names it gives Python tools "No shared RNS instance
available" against `lnsd`; `enable_transport` is `true` under `lnsd`
and `false` under `rnsd`, so `lnsd` relays where `rnsd` stays a leaf
(`lnsd` says so at startup in a `CONFIG_DEFAULT` line);
and an interface section without `enabled` is live under `lnsd` and
skipped by `rnsd`. A boolean `rnsd` refuses to parse, such as
`enable_transport = maybe`, stops both daemons; `lnsd` names the line,
the key and the value (audit D14). Some keys are read but not honoured, and `lnsd`
names each one in a warning at startup rather than dropping it in
silence: `panic_on_interface_error` (audit D13), because a failed
interface under `lnsd` keeps reconnecting instead of stopping the
daemon, by design, which leaves restarts to the service manager; and
the ingress and egress limiter tunings `ic_burst_hold`,
`ic_burst_freq_new`, `ic_burst_freq`, `ic_pr_burst_freq_new`,
`ic_pr_burst_freq`, `ic_new_time`, `ic_burst_penalty`,
`ic_held_release_interval` and `ec_pr_freq` (audit D16), for which
`lnsd` runs the reference defaults. Writing all three defaulted keys explicitly, as
`lnsd --exampleconfig` does, keeps one file meaning one daemon.

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
`true`, `1`, `on` (and their false counterparts); anything else is an
error naming the key, as in rnsd (`ini_config.rs:1125-1145`).

## A file with a syntax error is refused

A line that is neither a section header nor a `key = value` pair is a parse
error, and `lnsd` exits non-zero without starting rather than reading the
rest of the file as nothing (`ini_config.rs:214-216`). The error names the
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
(`ini_config.rs:45-63`).

What is NOT refused, because the reference does not refuse it either: an
interface whose `type` we do not implement is skipped with a warning and the
daemon runs with the rest (`ini_config.rs:344-350`), the same way `rnsd` logs
`Could not locate external interface module` and carries on. A key in a
section `lnsd` does not read is likewise kept out of the config and logged
(`ini_config.rs:185-198`).


## The `[reticulum]` section

Core daemon settings. Every key below is parsed in
`ini_config.rs:428-700`; defaults come from `config.rs:604-653`.

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `enable_transport` | bool | `true` | Route announces and serve paths for other peers. `lnsd` defaults this to `true` (it is a daemon); the Python *library* default is `false`. A config that omits the key gets one INFO line from `lnsd` at startup saying which default it took and that `rnsd` would take the other: `CONFIG_DEFAULT key=enable_transport rnsd=false value=true`. (`config.rs:49-50`, `366`, `494`) |
| `use_implicit_proof` | bool | `true` | Send packet proofs in the implicit form, the 64-byte signature alone, as `rnsd` does by default; `false` sends the explicit form, packet hash plus signature, 96 bytes. Both stacks accept both, so this is only a size choice: at 869.463 MHz, BW 125 kHz, SF8, CR 4/5 a proof frame takes 288 ms implicit and 370 ms explicit. Proofs over a link stay explicit either way, as in `rnsd`. (`config.rs:53-59`, `468`; `ini_config.rs:485-487`) |
| `share_instance` | bool | `false` | Listen on the abstract Unix socket `\0rns/<instance_name>` for local clients. Required for `lnstest diag`, `rnstatus`, Sideband etc. to attach. (`config.rs:37-40`, `205`; key `share_instance` → `shared_instance`, `ini_config.rs:434-436`) |
| `instance_name` | string | `default` | Names the shared-instance socket: `\0rns/<instance_name>`. Use a unique name to run two daemons side by side. (`config.rs:41-44`, `206`; `ini_config.rs:359-363`) |
| `shared_instance_type` | `unix`/`tcp` | unset | Parsed for `rnsd` compatibility. Only `tcp`/`unix` are stored; `tcp` clears `shared_instance_socket` (tcp disables AF_UNIX upstream) and moves both shared-instance channels, data and RPC, to TCP loopback at `shared_instance_port` and `instance_control_port` (37428/37429 by default), on Linux as on every platform, as `rnsd` does: `lnsd` listens there and the client tools reading the same file dial there (audit D2). (`config.rs:45-52`; `ini_config.rs:440-448`, `284-289`) |
| `rpc_key` | hex | unset | Key for the shared-instance RPC handshake, read with Python's `bytes.fromhex`. When set, `lnsd`'s RPC listener and every client tool reading the same config (`rnstatus`, `lnstatus`, ...) use it instead of the hash of the transport identity; an unparseable value logs an error and falls back to the default, as `rnsd` does. (audit D1) |
| `shared_instance_socket` | path | unset | Explicit AF_UNIX socket path (RNS 1.3.x). Parsed for compatibility; cleared when `shared_instance_type = tcp`. (`config.rs:53-58`; `ini_config.rs:450-452`) |
| `respond_to_probes` | bool | `false` | Answer `rnprobe` requests by signing a proof for each probe packet. (`config.rs:54-60`, `146`; `ini_config.rs:398-400`) |
| `remote_management_enabled` | bool | `false` | Enable remote management. (`config.rs:61-63`, `147`; `ini_config.rs:491-493`) |
| `storage_path` | path | unset | Where identity, known destinations and packet hashlist live. Relative values resolve against the config dir. (`config.rs:97-98`; `storage_path` (`ini_config.rs:636-641`)) |
| `flush_interval` | u64 (sec) | `3600` | Seconds between periodic storage flushes. Crash protection only — normal shutdown always flushes. (`config.rs:67-73`, `149`; `ini_config.rs:447-451`) |
| `control_channel_capacity` | usize | `256` | Capacity of the lossless control-plane event channel (announces, paths, link/resource lifecycle). Raise on servers under heavy announce load. (`config.rs:74-82`, `150`) |
| `data_channel_capacity` | usize | `128` | Capacity of the droppable data-plane event channel; full means normal backpressure (silent drop). Reliable channel messages are exempt: the node stops proofing them to the sender instead of dropping them, so this value also bounds how far a slow reader lets a channel run ahead. (`config.rs:83-90`, `151`) |
| `keepalive_interval` | u64 (sec) | unset | Override link keepalive interval. When set, every link uses this interval and the stale-link timeout scales with it (stale after twice the keepalive). Local timing only, no wire change. Useful for slow links. (`config.rs:91-98`, `152`; `ini_config.rs:433-459`) |
| `link_establishment_timeout_ms` | u32 (ms) | unset | TEST ONLY, no deployment sets it. Replaces the establishment timeout of every pending link (before its jitter) with this value, so a test about a link's death does not wait out three attempts of about 13 s each. Absent or `0` keeps the reference formula. Leviculum-only; `lxmf-node` reads it from the same file. (`config.rs:168-175`; `ini_config.rs:513-518`) |
| `mgmt_announce_interval_secs` | u64 (sec) | `7200` | Seconds between this node's management announces, its `rnstransport.probe` and remote-management destinations. Leviculum-only; for test rooms that need a probe destination to re-announce within minutes, a field node keeps the default. Below `5` the config is refused with the key named. (`config.rs:187-195`, `319-325`; `ini_config.rs:591-601`) |
| `storage_profile` | `desktop`/`compact` | `desktop` | Transport-table sizing profile (Codeberg #421). `compact` is sized to leave a Raspberry Pi Zero 2W (512 MB shared with the GPU, no swap) usable. An unrecognised value keeps `desktop`. (`config.rs:197-205`; `ini_config.rs:525-535`) |
| `path_table_cap` | usize | profile | Maximum `path_table` entries, and with them `path_states`, `path_requests` and `discovery_path_requests`. Desktop `32768`, compact `8192`. The path table expires after seven days, so on a node up less than a week this is its only bound. (`config.rs:206-213`; `ini_config.rs:537-539`) |
| `reverse_table_cap` | usize | profile | Maximum `reverse_table` entries. Desktop `200000`, compact `16384`. Entries expire after 8 minutes, so the working size is forwarding rate times that window; a field node measured 73 901. (`config.rs:214-221`; `ini_config.rs:540-542`) |
| `link_table_cap` | usize | profile | Maximum `link_table` entries — links this node routes for, not its own (that is `max_links`). Desktop `8192`, compact `1024`. (`config.rs:222-229`; `ini_config.rs:543-545`) |
| `announce_table_cap` | usize | profile | Maximum `announce_table` entries, the pending-rebroadcast queue. Each holds a full copy of an announce packet. Desktop `16384`, compact `2048`. (`config.rs:230-236`; `ini_config.rs:546-548`) |
| `destination_cap` | usize | profile | Maximum entries in the destination-keyed tables: `announce_cache`, `announce_rate_table`, `known_ratchets`, `known_dest_use`. Desktop `50000`, compact `4096`. One key for four tables because they share one population. (`config.rs:237-247`; `ini_config.rs:549-551`) |
| `max_identities` | usize | profile | Maximum `known_identities` entries, the public keys of remote destinations this node has heard announce (Codeberg #464). Desktop `50000`, compact `1000`. About 1.7 kB an entry, the most expensive table per entry. Below `1` the config is refused with the key named. |
| `max_packet_hashes` | usize | `100000` | Packet hashes the duplicate filter remembers across its two generations; each rotates at half this (Codeberg #464). A lower value forgets a packet sooner, so a late copy over a slow path is forwarded once more instead of dropped. Below `2` the config is refused with the key named. |
| `max_known_destinations` | usize | profile | The #464 name for the `destination_cap` ceiling: desktop `50000`, compact `4096`. Setting it beside `destination_cap` is refused, so no reader has to guess which won. Below `1` the config is refused with the key named. |
| `path_expiry_secs` | u64 (sec) | `604800` | How long a path learned from an announce lives without a refresh: Python's `PATHFINDER_E`, seven days (`reference/Reticulum/RNS/Transport.py:71`), as a key (Codeberg #464). Access-point and roaming paths keep their own one day and six hours. Outside `1..=4294967295` the config is refused with the key named; `ReticulumNodeBuilder::path_expiry_secs` overrides it. |
| `default_ar_target` | u32 (sec) | `3600` | Announce-rate target a transport-enabled node gives every interface that sets no `announce_rate_target` of its own, as `rnsd` does; `0` keeps the default, as in `rnsd`. (audit D16) |
| `default_ar_penalty` | u32 (sec) | `0` | The matching announce-rate penalty. (audit D16) |
| `default_ar_grace` | u32 | `5` | The matching announce-rate grace; `0` keeps the default, as in `rnsd`. (audit D16) |
| `egress_control` | bool | `false` | Path-request egress control on every interface that does not set `egress_control` itself. (audit D16) |
| `ic_max_held_announces` | usize | `256` | Announces an interface holds while its ingress limiter is active; `0` keeps the default, as in `rnsd`. (audit D16) |
| `panic_on_interface_error` | bool | `false` | Read but not honoured: a failed interface keeps reconnecting, and a config that sets this gets a warning saying so. (audit D13) |

The four #464 keys, `max_identities`, `max_packet_hashes`,
`max_known_destinations` and `path_expiry_secs`, are for small routers: a
host with 128 MB of RAM lowers them to fit, where it used to edit the
constants and carry a fork. Every default is the value the daemon used
before the keys existed, so a config that leaves them out behaves as
before. The per-interface announce queue stays a constant, 16384 entries,
Python's `MAX_QUEUED_ANNOUNCES` (`reference/Reticulum/RNS/Reticulum.py:111`,
read at `reference/Reticulum/RNS/Transport.py:1264`). It is a wire-adjacent
sizing, not a memory knob: it decides which announces an interface over its
announce bandwidth still puts on the air, which the neighbours observe, so it
stays the reference's number on every node.

`control_channel_capacity` and `data_channel_capacity` are read from TOML
only; they have no INI key in `apply_reticulum_key` (`ini_config.rs:428-700`)
and are best set in a TOML config or left at their defaults.

`storage_path` is read from both formats and resolves in one order
everywhere: `lnsd --storage`, then the config key, then
`<config_dir>/storage` (Python's only choice, `Reticulum.py:246`). The
client tools resolve it the same way (`resolve_storage_path`,
`config.rs:1585`), so `lnstatus`, `lncp`, `lnpath` and `lnprobe` open the
same directory as the daemon and derive the same RPC authkey from its
`transport_identity`. Point the key at an external disk and nothing else
has to be told about it — but note that `--storage` moves the daemon
alone, and the clients then still follow the config.

The storage directory holds private keys (`transport_identity` and
`ratchetkeys/`), and the RPC authkey is the hash of `transport_identity`,
so the daemon writes them owner-only (0600) and creates the storage and
ratchet directories 0700 (`atomic_write_secret`, `storage.rs`). A
directory or file that already exists keeps its mode: a node set up
before this release should run `chmod 600 <storage>/transport_identity`
and `chmod -R go= <storage>/ratchetkeys` once. A client tool run as
another user can then no longer read the identity, which is the point.

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

## The `[auto]` section

`lnsd` can start interfaces nobody configured, one medium at a time (the
automatic layer, [Zero-config lnsd](../concepts/zero-config.md)). Which
media it may start is the `[auto]` section, four yes/no keys, read by
`lnsd` and ignored by `rnsd`, which reads `[logging]`, `[reticulum]` and
`[interfaces]` only:

```ini
[auto]
  lan = yes          # an AutoInterface when the file names none
  servers = no       # the shipped server list (not built yet)
  bluetooth = no     # BLE when an adapter is present (not built yet)
  serial = no        # LNodes and RNodes on USB (not built yet)
```

- **A file without `[auto]` starts nothing automatic.** Every key is
  `no`, so a file means exactly what it says.
- **No file at all** runs the shipped defaults: `lan = yes`, the others
  `no` until their steps land. `lnsd --writeconfig` writes those
  defaults as an `[auto]` section after rnsd's first-start file.
- **`lan = yes`** adds the AutoInterface of rnsd's first-start file,
  `[[Default Interface]]` in the default group, when the configuration
  names no AutoInterface section (enabled or not). A file that names one
  keeps exactly its own.
- A value that is not a boolean refuses the file, as in `[reticulum]`; an
  unknown key is warned about and ignored.

Two switches outside the file turn every medium off, whatever the file
says: the host marker `/etc/leviculum/no-auto`, which
`scripts/install-ci.sh` installs on rig and coder hosts, and
`LEVICULUM_NO_AUTO=1` in `lnsd`'s environment. `LEVICULUM_NO_AUTO=0`
lifts the marker. Neither touches an interface the file names.

`lnsd` states its decision once at start, and `lnstatus` repeats it as
its last line (`Auto layer`):

```
AUTO_LAYER state=off reason=config lan=no servers=no bluetooth=no serial=no
```

`reason=` is `config` (the file), `default` (no file), `marker` or
`env`; the last two only when they turned off a medium the configuration
asked for, so a file that asks for nothing reads `reason=config` on a
marked host too. The rule and its table are `decide` in
`leviculum-std/src/auto_layer.rs`.

## The `[interfaces]` section

Interfaces are ConfigObj subsections under `[interfaces]`, each named in
double brackets `[[Name]]`. The name is free-form; the `type` key
selects the interface implementation. Twelve types pass the
supported-type filter (`interface_type` (`ini_config.rs:328-353`)):

`TCPServerInterface`, `TCPClientInterface`, `UDPInterface`,
`AutoInterface`, `RNodeInterface`, `RNodeMultiInterface`,
`SerialInterface`, `PipeInterface`, `KISSInterface`,
`AX25KISSInterface`, `I2PInterface`, `BLEInterface`.

`BackboneInterface` and `BackboneClientInterface` are accepted too:
they are wire-identical to TCP and are mapped onto the TCP interface at
parse time, as Python does (`normalize_backbone_interface`
(`ini_config.rs:1044-1079`)). An interface of any other type is skipped
with a log line (`tracing::warn` (`ini_config.rs:344-349`)), not an
error.

The per-type tables below cover the six types most deployments use. All
interface keys are parsed in `apply_interface_key`
(`ini_config.rs:770-1005`); the struct they land in, with its defaults,
is `InterfaceConfig` (`config.rs:258-741`).

### Keys common to every interface

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `type` | string | (required) | Interface type, one of the eleven above. (`type` (`ini_config.rs:772`)) |
| `enabled` | bool | `true` | Bring this interface up; the legacy spelling `interface_enabled` is honoured too. (`enabled` (`ini_config.rs:776`); `InterfaceConfig::enabled` (`config.rs:760-762`)) |
| `outgoing` | bool | `true` | Allow sending outgoing packets. (`outgoing` (`ini_config.rs:777`); `InterfaceConfig::outgoing` (`config.rs:782-784`)) |
| `bitrate` | u64 (bps) | per type | Override the interface's own bitrate figure, which feeds announce bandwidth capping and timing. Values below `MINIMUM_BITRATE` (`constants.rs:405-408`), 5 bps, are ignored. (`bitrate` (`ini_config.rs:832-838`); `InterfaceConfig::bitrate` (`config.rs:773-778`)) |
| `buffer_size` | usize | per type | Channel buffer size. (`buffer_size` (`ini_config.rs:878`); `InterfaceConfig::buffer_size` (`config.rs:951-952`)) |

### TCP server (`TCPServerInterface`)

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `listen_ip` | string | unset | Address to bind. (`listen_ip` (`ini_config.rs:784`)) |
| `listen_port` | u16 | unset | Port to listen on. (`listen_port` (`ini_config.rs:785`)) |

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
| `target_host` | string | unset | Remote host to connect to. (`target_host` (`ini_config.rs:786`)) |
| `target_port` | u16 | unset | Remote port. (`target_port` (`ini_config.rs:787`)) |
| `reconnect_interval` | u64 (sec) | `5` | Delay between reconnect attempts. (`reconnect_interval` (`ini_config.rs:879`); `InterfaceConfig::reconnect_interval_secs` (`config.rs:953-954`)) |
| `max_reconnect_tries` | u64 | unlimited | Give up after this many attempts; unset means never. (`max_reconnect_tries` (`ini_config.rs:880`); `InterfaceConfig::max_reconnect_tries` (`config.rs:955-956`)) |

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
| `listen_ip` | string | `0.0.0.0` | Local bind address. (`listen_ip` (`ini_config.rs:784`)) |
| `listen_port` | u16 | unset | Local bind port. (`listen_port` (`ini_config.rs:785`)) |
| `forward_ip` | string | unset | Broadcast/forward address or hostname. Names are resolved at runtime and re-resolved periodically; a resolution failure is a logged interface error, not a config error. (`forward_ip` (`ini_config.rs:795`); `InterfaceConfig::forward_ip` (`config.rs:877-887`)) |
| `forward_port` | u16 | unset | Broadcast/forward port. (`forward_port` (`ini_config.rs:796`)) |
| `port` | u16 | unset | Fills both `listen_port` and `forward_port`; either explicit key wins over it. (`port` (`ini_config.rs:797`)) |
| `device` | string | unset | Kernel interface name; its IPv4 broadcast address fills both `listen_ip` and `forward_ip`. Either explicit key wins over it. (`device` (`ini_config.rs:782`)) |

Bind and forward are independent, as in `rnsd`: an interface with only bind
parameters receives without transmitting, one with only forward parameters
transmits without listening, and only an interface that would do neither is a
configuration error.

### AutoInterface (`AutoInterface`)

Discovers other Reticulum nodes on the same broadcast domain via
multicast. No router or DHCP needed; the link must carry multicast.

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `group_id` | string | unset | Multicast group identifier; isolate co-located meshes by setting different IDs. (`group_id` (`ini_config.rs:892`); `InterfaceConfig::group_id` (`config.rs:959-960`)) |
| `discovery_scope` | string | unset | Multicast scope: `link`, `admin`, `site`, `organisation`, `global`. (`discovery_scope` (`ini_config.rs:893`); `InterfaceConfig::discovery_scope` (`config.rs:961-962`)) |
| `discovery_port` | u16 | `29716` | Discovery (announce) port. (`discovery_port` (`ini_config.rs:894`); `InterfaceConfig::discovery_port` (`config.rs:963-964`)) |
| `data_port` | u16 | `42671` | Data port. (`data_port` (`ini_config.rs:895`); `InterfaceConfig::data_port` (`config.rs:965-966`)) |
| `devices` | string (CSV) | unset | Whitelist of NIC names to use. (`devices` (`ini_config.rs:896`); `InterfaceConfig::devices` (`config.rs:967-968`)) |
| `ignored_devices` | string (CSV) | unset | Blacklist of NIC names to skip. (`ignored_devices` (`ini_config.rs:897`); `InterfaceConfig::ignored_devices` (`config.rs:969-970`)) |
| `multicast_loopback` | bool | unset (inherits `true`) | Multicast loopback (`IPV6_MULTICAST_LOOP`), the carrier self-echo mechanism. Unset inherits the default `true`, matching Python-RNS; set `no` to opt out. (`multicast_loopback` (`ini_config.rs:898`); `InterfaceConfig::multicast_loopback` (`config.rs:971-974`)) |
| `multicast_address_type` | string | unset (inherits `temporary`) | Multicast address type of the discovery group, `temporary` or `permanent`. It is part of the group address, so peers must agree on it: an `lnsd` node left on the default next to a `permanent`-type `rnsd` peer group discovers nobody, and nothing on either side says why. Unset inherits `temporary`, the group Python joins when the key is absent. A value that is neither spelling is refused at startup rather than resolved to `temporary` the way Python resolves it. (`multicast_address_type` (`ini_config.rs:905`); `InterfaceConfig::multicast_address_type` (`config.rs:963-968`); `MulticastAddressType` (`interfaces/auto_interface/mod.rs:95-144`)) |
| `max_peers` | integer | `128` | Most peers the interface keeps at once. Discovery is unauthenticated (the token hashes a public default group id and the sender's address), so without a cap one host on the segment could make the daemon spawn a task, two channels and a routable interface per discovery packet. A home LAN has tens of Reticulum instances; 128 leaves headroom for a busy shared segment. A new peer past the cap is refused and logged as `AUTO_PEER_REFUSED reason=cap`; peers already known keep working and still expire after 22 s of silence. `0` is refused at startup. No Python equivalent: Python keeps every peer, and the cap is invisible on the wire. (`max_peers` (`ini_config.rs:908`); `InterfaceConfig::max_peers` (`config.rs:981-983`); `DEFAULT_MAX_PEERS` (`interfaces/auto_interface/mod.rs:52-61`)) |
| `max_peers_per_source` | integer | `4` | Most peers one source address holds at once. The admission interval only slows one host down: refreshing the peers it has, it would gain one more every interval and own the whole table in about ten minutes, crowding out every real neighbour. A host runs a handful of Reticulum instances at most, each on its own data port, so 4 admits every same-host setup. A new peer past it is refused and logged as `AUTO_PEER_REFUSED reason=source`; the host's known peers keep working. `0` is refused at startup. (`max_peers_per_source` (`ini_config.rs:909`); `InterfaceConfig::max_peers_per_source` (`config.rs:984-986`); `DEFAULT_MAX_PEERS_PER_SOURCE` (`interfaces/auto_interface/mod.rs:63-72`)) |
| `peer_admission_interval` | integer (seconds) | `5` | One source address registers at most one new peer per interval. The discovery token does not cover the data port, so without it one host mints a peer per packet by naming a fresh port each time. A second instance on the same host is admitted at its first announce after the interval (announces go out every 1.6 s). Refusals are logged as `AUTO_PEER_REFUSED reason=rate`. `0` turns the limit off; the cap still holds. (`peer_admission_interval` (`ini_config.rs:910`); `InterfaceConfig::peer_admission_interval` (`config.rs:987-990`); `DEFAULT_PEER_ADMISSION_INTERVAL_SECS` (`interfaces/auto_interface/mod.rs:74-85`)) |

`AUTO_PEER_REFUSED` goes out at most once per 10 s, naming the first refusal of the burst (`reason`, `src`, `port`, `nic`, `peers`); `suppressed` counts the refusals the previous line held back.

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
| `device` | string | default adapter | BlueZ adapter to use, e.g. `hci0`. (`device` (`ini_config.rs:782`)) |
| `max_connections` | usize | `4` | Simultaneous BLE link cap, both GATT roles counted together — and split by role inside it since #432: `central_links` slots are this node's own outgoing dials, the other `max_connections - central_links` are incoming links (two and two at the defaults; one and three is the boards' shape, `CENTRAL_LINKS` / `PERIPH_LINKS`). Dialling is gated on the outgoing slot and advertising on the incoming ones, so a node that has dialled still advertises and a node with a full GATT server still dials. The default is the firmware's `MAX_LINKS` (4), not the reference's 7: 3-4 links is the protocol's reliable ceiling. (`max_connections` (`ini_config.rs:977`); `InterfaceConfig::max_connections` (`config.rs:1104-1107`)) |
| `min_rssi` | i16 (dBm) | `-85` | Sightings weaker than this are not dialled. (`min_rssi` (`ini_config.rs:978`); `InterfaceConfig::min_rssi` (`config.rs:1108-1110`)) |
| `discovery_interval` | f64 (sec) | `5` | Pause between the 2-second BLE scan windows. While a dial waits out its pre-connect jitter the scanner stays on, so the dial reads its peer's current record; a dial due before that scan has heard its peer waits for it, at most one 1.28 s advertising interval from the scan's start. (`discovery_interval` (`ini_config.rs:979`); `InterfaceConfig::discovery_interval` (`config.rs:1111-1113`)) |
| `enable_central` | bool | `true` | Run the scanning + dialling central role. (`enable_central` (`ini_config.rs:980`); `InterfaceConfig::enable_central` (`config.rs:1114-1116`)) |
| `enable_peripheral` | bool | `true` | Run the advertising + GATT-server peripheral role. Disabling both roles is a config error. (`enable_peripheral` (`ini_config.rs:981`); `InterfaceConfig::enable_peripheral` (`config.rs:1117-1119`)) |
| `initiate_only` | string (CSV) | unset (every peer) | Peers this interface may DIAL: BLE addresses (`AA:BB:CC:DD:EE:FF`, `-` or no separator) or peer identities in hex, 8 digits (the four bytes an advertiser publishes as its hint) or all 32 (a board's `[IDENTITY]` line, truncated to those four). Unset or empty dials whoever the connection-direction rule picks, the behaviour that predates the key. It narrows dialling and nothing else: a peer left off the list that connects to US is admitted and served exactly as before, and nothing on the wire changes — it sees a node that has not dialled it yet. The digit count decides which is which — 12 is an address, 8 or 32 an identity — so both spellings can be copied out of a log line (`BLE_SCAN_DECISION addr=`, a board's `BLE_CENTRAL_ADDR`, its `[IDENTITY]`). A malformed entry is a startup error, not a dropped line. (`initiate_only` (`ini_config.rs:983-985`); `InterfaceConfig::initiate_only` (`config.rs:1120-1129`); `PeerAllowlist` (`interfaces/ble/links.rs:1586`)) |
| `accept_only` | string (CSV) | unset (every peer) | Peers whose INCOMING link this interface SERVES — the symmetric counterpart of `initiate_only`, same vocabulary, same validation, same "unset or empty means everyone". A non-empty list narrows who we serve and nothing else: a peer left off it is still dialled if `initiate_only` allows it, and a peer left off `initiate_only` is still served if this list names it. An unlisted peer that connects is refused at the identity handshake — the first moment an inbound BLE connection has said who it is, since under RPA its address names nobody — so it never becomes a link, never enters the fan-out and is never reported to the core as a peer. Each refusal emits one `BLE_LINK_NOT_ADMITTED peer=<hex8> identity=<hex32> addr=<a> role=peripheral listed=<n> action=disconnect` line, so a run that turned strangers away is distinguishable from a run nobody tried. A malformed entry is a startup error, not a dropped line. (`accept_only` (`ini_config.rs:989-991`); `InterfaceConfig::accept_only` (`config.rs:1130-1145`); the refusal point (`interfaces/ble/links.rs:851`)) |
| `tx_gap_ms` | u16 (ms) | unset (100) | The gap between the last fragment of one packet and the first fragment of the next on a link, overriding the firmware's compiled `DEFAULT_TX_GAP_MS`; `0` disables it. A measurement knob (#379), the sibling of a board's `--set-ble-tx-gap`: leave it unset in a deployment, where lnsd paces exactly as a board does. The bring-up line prints the gap in force. (`tx_gap_ms` (`ini_config.rs:994`); `InterfaceConfig::tx_gap_ms` (`config.rs:1146-1151`); `LinkPacer` (`interfaces/ble/links.rs:1981`)) |
| `fragment_mtu` | usize | unset (exchanged) | Cap on the ATT MTU outbound fragments are sized by; at least 23, a smaller value is a startup error. Two BlueZ hosts exchange 512, at which no Reticulum packet needs a second fragment, a board 253. A measurement knob (#379) for making a desk link fragment the way a board link does; leave it unset in a deployment. The bring-up line and `BLE_LINK_UP mtu=` print the capped value. (`fragment_mtu` (`ini_config.rs:996`); `InterfaceConfig::fragment_mtu` (`config.rs:1152-1156`); `sizing_mtu` (`interfaces/ble/mod.rs:1838`)) |
| `central_links` | usize | `2` | Outgoing links inside `max_connections`, the rest are incoming: `max_connections - central_links`. Two since 770 (#723): twenty-node rooms of host nodes settled to 40 edges and diameter 4 with two dials each, against 20 edges and diameter 8 to 11 with one. Unset under `max_connections` of 2 or 1 it is one dial, so an incoming slot survives. 1 to 3 and below `max_connections`, anything else is a startup error naming the key. A board has one central slot and no such key, so set `central_links = 1` and `advertise_when_full = false` wherever lnsd should behave like a board. Both bring-up lines print it. (`central_links` (`ini_config.rs:999`); `InterfaceConfig::central_links` (`config.rs:1157-1161`); `LinkTable::new` (`interfaces/ble/links.rs:455`)) |
| `advertise_when_full` | bool | `true` | Keep advertising with a published `free_slots = 0` once every incoming slot is taken, instead of taking the advertisement off the air (the boards' full-is-dark gate, which `false` keeps). A dialler that reads the fresh zero skips the node rather than connecting into a refusal or a 5 s connect timeout on a stale record. On by default since 770 (752); a board has no such switch and stays dark when full. A value that is not a bool is a startup error. The `BLE_ADV_GATE state=on` line prints `free_slots=0` for a full node that stays on. (`advertise_when_full` (`ini_config.rs:1002`); `InterfaceConfig::advertise_when_full` (`config.rs:1162-1165`); `LinkTable::with_advertise_when_full` (`interfaces/ble/links.rs:474`)) |

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
| `port` | string | unset | Serial device path, e.g. `/dev/ttyACM0`. (`port` (`ini_config.rs:797`); `InterfaceConfig::port` (`config.rs:893-894`)) |
| `speed` / `baudrate` | u32 | unset | Serial baud rate (either spelling). (`speed` (`ini_config.rs:798`); `InterfaceConfig::speed` (`config.rs:907-908`)) |
| `databits` | u8 | unset | Data bits. (`databits` (`ini_config.rs:799`); `InterfaceConfig::databits` (`config.rs:909-910`)) |
| `parity` | string | unset | `none`, `even`, or `odd`. (`parity` (`ini_config.rs:800`); `InterfaceConfig::parity` (`config.rs:911-912`)) |
| `stopbits` | u8 | unset | Stop bits. (`stopbits` (`ini_config.rs:801`); `InterfaceConfig::stopbits` (`config.rs:913-914`)) |

LoRa keys, derived from source — the meanings below describe the radio
parameters the interface configures; the fields sit together in the
RNode block of `InterfaceConfig` (`InterfaceConfig::frequency`
(`config.rs:825-863`)):

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `frequency` | u64 (Hz) | unset | LoRa centre frequency. (`frequency` (`ini_config.rs:881`); `InterfaceConfig::frequency` (`config.rs:1029-1030`)) |
| `bandwidth` | u32 (Hz) | unset | LoRa bandwidth: 7800, 10400, 15600, 20800, 31250, 41700, 62500, 125000, 250000 or 500000. Five of these also have the SX1262 spelling 7810, 10420, 15630, 20830 and 41670, the same register on either chip; both spellings are accepted, and each interface sends the one its board answers (an RNode the SX127x spelling, an LNode the SX1262 one). (`bandwidth` (`ini_config.rs:882`); `InterfaceConfig::bandwidth` (`config.rs:1043-1044`); `BANDWIDTH_TABLE` (`sx126x.rs:674`)) |
| `spreadingfactor` / `spreading_factor` | u8 | unset | LoRa spreading factor (either spelling). (`spreadingfactor` (`ini_config.rs:883`); `InterfaceConfig::spreading_factor` (`config.rs:1045-1046`)) |
| `codingrate` / `coding_rate` | u8 | unset | LoRa coding rate (either spelling). (`codingrate` (`ini_config.rs:884`); `InterfaceConfig::coding_rate` (`config.rs:1047-1048`)) |
| `txpower` / `tx_power` | i8 (dBm) | unset (resolves to the board maximum, 22 dBm) | Transmit power (either spelling). Unset asks the board for its maximum — a board that can do less clamps and says so — rather than the 0 dBm (1 mW) Python-Reticulum resolves it to, which has no symptom at the node. An explicit `txpower = 0` still means 0. Above roughly 7 dBi of antenna gain, 22 dBm conducted exceeds the EU 27 dBm ERP allowance and has to be set down. (`txpower` (`ini_config.rs:885`); `InterfaceConfig::tx_power` (`config.rs:1049-1050`); `resolve_tx_power` (`rnode.rs:807`); [deviation](../concepts/python-rns-compatibility.md)) |
| `flow_control` | bool | unset | Wait for the RNode's `CMD_READY` before the next TX. (`flow_control` (`ini_config.rs:911`); `InterfaceConfig::flow_control` (`config.rs:1067-1068`)) |
| `airtime_limit_short` | f64 (%) | unset | Short-term airtime cap, percent (0.0–100.0). (`airtime_limit_short` (`ini_config.rs:912`); `InterfaceConfig::airtime_limit_short` (`config.rs:1069-1070`)) |
| `airtime_limit_long` | f64 (%) | unset | Long-term airtime cap, percent (0.0–100.0). (`airtime_limit_long` (`ini_config.rs:913`); `InterfaceConfig::airtime_limit_long` (`config.rs:1071-1072`)) |
| `csma_enabled` | bool | unset | Carried in the LNode radio-config frame and reported back, but current firmware no longer obeys it: LoRa channel access (pre-TX jitter plus CAD listen-before-talk) is always on, matching the RNode firmware, which offers no CSMA disable either. Only firmware older than the change still honours the flag. (`csma_enabled` (`ini_config.rs:914`); `InterfaceConfig::csma_enabled` (`config.rs:1073-1074`)) |
| `preamble_symbols` | u16 (symbols) | unset (derived from the PHY) | LoRa preamble length pushed to LNode firmware in the radio-config frame (`SerialInterface` only). Unset derives what an RNode peer programs for the same PHY — 24 symbols at SF7/BW125, the 18-symbol floor from SF8 down — so a mixed pair agrees on the wire; set it only to pin a value against a non-conforming peer. A pin above roughly 20 symbols / 164 ms on air is warned about at startup and not refused: SX127x receivers (every RNode) were measured going deaf above that, losing every frame from the interface silently and one-way, while an SX126x peer copes ([Codeberg #315](https://codeberg.org/Lew_Palm/leviculum/issues/315)). Not the same key as the KISS `preamble` (TX delay in ms), which never reaches a LoRa modem. (`preamble_symbols` (`ini_config.rs:890`); `InterfaceConfig::preamble_symbols` (`config.rs:1039-1054`); `derive_preamble_symbols` (`rnode.rs:963`); `preamble_ceiling_warning` (`interfaces/serial.rs`)) |

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
| `networkname` / `network_name` | string | unset | Network name for IFAC (either spelling). (`networkname` (`ini_config.rs:948`); `InterfaceConfig::networkname` (`config.rs:1033-1034`)) |
| `passphrase` / `pass_phrase` | string | unset | IFAC passphrase (either spelling). (`passphrase` (`ini_config.rs:949`); `InterfaceConfig::passphrase` (`config.rs:1035-1036`)) |
| `ifac_size` | usize (bits) | unset | IFAC size, specified in bits in the file and stored as bytes (`bits / 8`). Range 8 to 512 bits (1 to 64 bytes, the Ed25519 signature the code is cut from). Values below 8 bits are dropped, as Python drops them, so the interface falls back to its per-type default; a value above 512 bits is a startup error naming the key. (`ifac_size` (`ini_config.rs:960-970`); `InterfaceConfig::ifac_size` (`config.rs:1037-1038`)) |

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

## Direct links (+ciris, leviculum#70)

These keys go in the `[reticulum]` section.

Two nodes that both dialled out to a transport node can move a link between
them onto a direct UDP path, punched through both NATs. Each side learns its
public address from a *facilitator*, the two swap addresses over the link
they already share, and both send at once. When the punch works the link moves
onto a new point-to-point interface; when it does not (symmetric NAT, common
on cellular) the link stays where it was. The wire protocol is rns-rs's
DirectLink, so a leviculum node can upgrade a link to an rns-rs node and use
an rns-rs facilitator. The feature is off unless configured:

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `probe_port` | u16 | unset | Run a facilitator on this UDP port (all IPv4 addresses). It answers RNSP and STUN Binding requests with the address each request came from, and holds no state. Put it on a node with a public address, usually the transport node. |
| `probe_addr` | `host:port` | unset | The facilitator this node probes when it proposes an upgrade. Without it the node cannot propose, but it can still accept. |
| `probe_protocol` | `rnsp`/`stun` | `rnsp` | `stun` lets any public STUN server stand in for the facilitator. |
| `direct_connect_policy` | `reject`/`accept_all`/`identified_only`/`ask_host` | `reject` | Which peers' proposals to accept. Accepting tells the peer this node's public address; `identified_only` accepts only from a peer that identified itself on the link; `ask_host` holds each proposal and raises `DirectLinkRequested` for the host to answer with `accept_direct_link` or `reject_direct_link` (no answer within 5 s refuses it, and nothing is probed before an accept). |

An upgrade is never automatic: the application calls
`ReticulumNode::propose_direct_link` and gets `DirectLinkEstablished` or
`DirectLinkFailed`. Only propose to a peer known to run leviculum or rns-rs. A
Python RNS peer cannot skip the request, and every later channel message on
that link stalls behind it.

```ini
[reticulum]
  # On the public transport node:
  probe_port = 4343

  # On each NAT'd node:
  probe_addr = transport.example.org:4343
  direct_connect_policy = identified_only
```
