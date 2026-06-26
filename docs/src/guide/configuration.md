# Configuration

`lnsd` reads the same INI-style configuration file as Python Reticulum
(`rnsd`). The format is a drop-in: a config that `rnsd` accepts, `lnsd`
accepts, and the two share the shared-instance IPC socket so client
tools (`rnstatus`, `rncp`, `lnstest diag`, Sideband, Nomadnet) attach to
either daemon without changes. Keys `lnsd` does not implement are
tolerated, not rejected — an unknown key never makes `lnsd` refuse a
config a current `rnsd` would load (`ini_config.rs:193-198`).

## File location and lookup order

Pass an explicit config directory with `--config DIR` (`lnsd.rs`,
`-c/--config`). With no flag, `lnsd` resolves the directory using the
same order as Python Reticulum (`config.rs:360-391`):

1. `/etc/reticulum` — if `/etc/reticulum/config` exists
2. `$HOME/.config/reticulum` — if that directory's `config` exists
3. `$HOME/.reticulum` — fallback, used even if absent

The config *file* is always named `config` inside that directory
(`config.rs:369-371`). The storage directory defaults to
`<config_dir>/storage` and can be overridden with `--storage`
(`lnsd.rs`, `-s/--storage`).

This order is why the Debian package can install a system-wide config
under `/etc/reticulum` and have Python clients connect to the live
daemon with no extra flags (`config.rs:354-359`).

## INI vs TOML detection

`lnsd` accepts both the Python INI format and native TOML. Detection is
by content, not just extension (`config.rs:315-338`):

- An explicit `.toml` extension forces TOML.
- A file containing `[[` (the ConfigObj subsection marker Python uses
  for interfaces) is parsed as INI.
- Otherwise TOML is tried first, then INI as a fallback.

In practice your `config` file uses the Python INI form shown
throughout this page. Boolean values accept `Yes`, `yes`, `True`,
`true`, `1`, `on` (and their false counterparts); anything else is read
as `false` (`ini_config.rs:255-257`).

## The `[reticulum]` section

Core daemon settings. Every key below is parsed in
`ini_config.rs:153-199`; defaults come from `config.rs:137-155`.

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `enable_transport` | bool | `true` | Route announces and serve paths for other peers. `lnsd` defaults this to `true` (it is a daemon); the Python *library* default is `false`. (`config.rs:27-28`, `140`) |
| `use_implicit_proof` | bool | `true` | Use implicit proof for link identification. (`config.rs:30-31`, `141`) |
| `share_instance` | bool | `false` | Listen on the abstract Unix socket `\0rns/<instance_name>` for local clients. Required for `lnstest diag`, `rnstatus`, Sideband etc. to attach. (`config.rs:32-35`, `142`; key `share_instance` → `shared_instance`, `ini_config.rs:158-159`) |
| `instance_name` | string | `default` | Names the shared-instance socket: `\0rns/<instance_name>`. Use a unique name to run two daemons side by side. (`config.rs:36-39`, `143`; `ini_config.rs:161-163`) |
| `shared_instance_type` | `unix`/`tcp` | unset | Parsed for `rnsd` compatibility. Only `tcp`/`unix` are stored; `tcp` clears `shared_instance_socket` (tcp disables AF_UNIX upstream). `lnsd` currently serves only the abstract AF_UNIX socket. (`config.rs:40-47`; `ini_config.rs:164-173`, `126-128`) |
| `shared_instance_socket` | path | unset | Explicit AF_UNIX socket path (RNS 1.3.x). Parsed for compatibility; cleared when `shared_instance_type = tcp`. (`config.rs:48-53`; `ini_config.rs:174-176`) |
| `respond_to_probes` | bool | `false` | Answer `rnprobe` requests by signing a proof for each probe packet. (`config.rs:54-60`, `146`; `ini_config.rs:177-179`) |
| `remote_management_enabled` | bool | `false` | Enable remote management. (`config.rs:61-63`, `147`; `ini_config.rs:180-182`) |
| `storage_path` | path | unset | Storage path, relative to the config dir or absolute. (`config.rs:64-66`, `148`) |
| `flush_interval` | u64 (sec) | `3600` | Seconds between periodic storage flushes. Crash protection only — normal shutdown always flushes. (`config.rs:67-73`, `149`; `ini_config.rs:183-187`) |
| `control_channel_capacity` | usize | `256` | Capacity of the lossless control-plane event channel (announces, paths, link/resource lifecycle). Raise on servers under heavy announce load. (`config.rs:74-82`, `150`) |
| `data_channel_capacity` | usize | `128` | Capacity of the droppable data-plane event channel; full means normal backpressure (silent drop). (`config.rs:83-90`, `151`) |
| `keepalive_interval` | u64 (sec) | unset | Override link keepalive interval. When set, every link uses this interval and the stale-link timeout scales with it (stale after twice the keepalive). Local timing only, no wire change. Useful for slow links. (`config.rs:91-98`, `152`; `ini_config.rs:188-192`) |

`use_implicit_proof`, `storage_path`, `control_channel_capacity`, and
`data_channel_capacity` are read from TOML only; they have no INI key in
`apply_reticulum_key` (`ini_config.rs:155-199` parses just the nine keys
above) and are best set in a TOML config or left at their defaults.
`storage_path` is also settable from the command line via `lnsd --storage`.

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

## The `[interfaces]` section

Interfaces are ConfigObj subsections under `[interfaces]`, each named in
double brackets `[[Name]]`. The name is free-form; the `type` key
selects the interface implementation. Six types are supported
(`ini_config.rs:133-145`):

`TCPServerInterface`, `TCPClientInterface`, `UDPInterface`,
`AutoInterface`, `RNodeInterface`, `SerialInterface`.

An interface of any other type is skipped with a log line, not an error
(`ini_config.rs:136-143`).

All interface keys are parsed in `ini_config.rs:202-247`; struct
defaults are in `config.rs:259-305`.

### Keys common to every interface

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `type` | string | (required) | Interface type, one of the six above. (`ini_config.rs:204`) |
| `enabled` | bool | `true` | Bring this interface up. (`ini_config.rs:205`; `config.rs:164-165`) |
| `outgoing` | bool | `true` | Allow sending outgoing packets. (`ini_config.rs:206`; `config.rs:167-168`) |
| `bitrate` | u64 (bps) | `62500` | Advertised link bitrate, used for airtime accounting. (`ini_config.rs:218-222`; `config.rs:170-171`, `253`) |
| `buffer_size` | usize | per type | Channel buffer size. (`ini_config.rs:223`; `config.rs:200-201`) |

### TCP server (`TCPServerInterface`)

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `listen_ip` | string | unset | Address to bind. (`ini_config.rs:207`) |
| `listen_port` | u16 | unset | Port to listen on. (`ini_config.rs:208`) |

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
| `target_host` | string | unset | Remote host to connect to. (`ini_config.rs:209`) |
| `target_port` | u16 | unset | Remote port. (`ini_config.rs:210`) |
| `reconnect_interval` | u64 (sec) | `5` | Delay between reconnect attempts. (`ini_config.rs:224`; `config.rs:202-203`) |
| `max_reconnect_tries` | u64 | unlimited | Give up after this many attempts; unset means never. (`ini_config.rs:225`; `config.rs:204-205`) |

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
| `listen_ip` | string | unset | Local bind address. (`ini_config.rs:207`) |
| `listen_port` | u16 | unset | Local bind port. (`ini_config.rs:208`) |
| `forward_ip` | string | unset | Broadcast/forward address. (`ini_config.rs:211`) |
| `forward_port` | u16 | unset | Broadcast/forward port. (`ini_config.rs:212`) |

### AutoInterface (`AutoInterface`)

Discovers other Reticulum nodes on the same broadcast domain via
multicast. No router or DHCP needed; the link must carry multicast.

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `group_id` | string | unset | Multicast group identifier; isolate co-located meshes by setting different IDs. (`ini_config.rs:232`; `config.rs:208-209`) |
| `discovery_scope` | string | unset | Multicast scope: `link`, `admin`, `site`, `organisation`, `global`. (`ini_config.rs:233`; `config.rs:210-211`) |
| `discovery_port` | u16 | `29716` | Discovery (announce) port. (`ini_config.rs:234`; `config.rs:212-213`) |
| `data_port` | u16 | `42671` | Data port. (`ini_config.rs:235`; `config.rs:214-215`) |
| `devices` | string (CSV) | unset | Whitelist of NIC names to use. (`ini_config.rs:236`; `config.rs:216-217`) |
| `ignored_devices` | string (CSV) | unset | Blacklist of NIC names to skip. (`ini_config.rs:237`; `config.rs:218-219`) |
| `multicast_loopback` | bool | unset | Enable multicast loopback (same-machine testing). (`ini_config.rs:238`; `config.rs:220-221`) |

### RNode and Serial (`RNodeInterface`, `SerialInterface`)

`RNodeInterface` drives an RNode LoRa modem; `SerialInterface` is a raw
serial KISS link. They share the serial-port and (for RNode) LoRa keys.

Serial keys:

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `port` | string | unset | Serial device path, e.g. `/dev/ttyACM0`. (`ini_config.rs:213`; `config.rs:188-189`) |
| `speed` / `baudrate` | u32 | unset | Serial baud rate (either spelling). (`ini_config.rs:214`; `config.rs:190-191`) |
| `databits` | u8 | unset | Data bits. (`ini_config.rs:215`; `config.rs:192-193`) |
| `parity` | string | unset | `none`, `even`, or `odd`. (`ini_config.rs:216`; `config.rs:194-195`) |
| `stopbits` | u8 | unset | Stop bits. (`ini_config.rs:217`; `config.rs:196-197`) |

LoRa keys (RNode), derived from source — the meanings below describe the
RNode radio parameters the interface configures (`config.rs:231-249`):

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `frequency` | u64 (Hz) | unset | LoRa centre frequency. (`ini_config.rs:226`; `config.rs:232-233`) |
| `bandwidth` | u32 (Hz) | unset | LoRa bandwidth. (`ini_config.rs:227`; `config.rs:234-235`) |
| `spreadingfactor` / `spreading_factor` | u8 | unset | LoRa spreading factor (either spelling). (`ini_config.rs:228`; `config.rs:236-237`) |
| `codingrate` / `coding_rate` | u8 | unset | LoRa coding rate (either spelling). (`ini_config.rs:229`; `config.rs:238-239`) |
| `txpower` / `tx_power` | i8 (dBm) | unset | Transmit power (either spelling). (`ini_config.rs:230`; `config.rs:240-241`) |
| `flow_control` | bool | unset | Wait for the RNode's `CMD_READY` before the next TX. (`ini_config.rs:239`; `config.rs:242-243`) |
| `airtime_limit_short` | f64 (%) | unset | Short-term airtime cap, percent (0.0–100.0). (`ini_config.rs:240`; `config.rs:244-245`) |
| `airtime_limit_long` | f64 (%) | unset | Long-term airtime cap, percent (0.0–100.0). (`ini_config.rs:241`; `config.rs:246-247`) |
| `csma_enabled` | bool | unset | Enable CSMA/CA on the T114 LoRa interface (needs CAD-capable firmware). (`ini_config.rs:242`; `config.rs:248-249`) |

### IFAC (Interface Access Codes)

IFAC keys apply to any interface and authenticate / isolate a virtual
network on the link. They are common to all interface types
(`ini_config.rs:243-245`):

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `networkname` / `network_name` | string | unset | Network name for IFAC (either spelling). (`ini_config.rs:243`; `config.rs:224-225`) |
| `passphrase` | string | unset | IFAC passphrase. (`ini_config.rs:244`; `config.rs:226-227`) |
| `ifac_size` | usize (bits) | unset | IFAC size, specified in bits in the file and stored as bytes (`bits / 8`). (`ini_config.rs:245`; `config.rs:228-229`) |

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
