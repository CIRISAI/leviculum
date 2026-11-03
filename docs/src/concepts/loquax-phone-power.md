# Loquax on a phone: what it costs, tier P proxies

Status: tier P, order 1177 (Lew, 2026-10-31; Refs #232). The script is
`loquax-android/scripts/power-proxy.py`; the reviewer runs it on hamster
against Waydroid. Tier R, a real phone at the bench with the charge
counter, is a later order.

## Why

Lew's rule for battery work: a mode that needs wake infrastructure of its
own (a push distributor, UnifiedPush, FCM) is built only if a measurement
shows a large gain. The research note of 2026-10-31 ranks the mechanisms
that keep a phone awake while Loquax waits for mail. In order: TCP
keepalive probes every 5 s at both ends, the link keepalive derived from
the RTT, announces forwarded onto the phone's connection, and CPU timers.
It proposes three levers that need no wire change: a long TCP keepalive,
a long link keepalive, and the phone's server interface in
`access_point` mode. Tier P gives a number to each mechanism every night,
without a phone, so a change to a lever shows up the next morning. Tier R
then turns the winner into mAh.

## The modes

| Mode | What runs |
|---|---|
| `B` | today's defaults: the device walk's fixture (lnsd with a TCP server, the mailbox over the ugly store), the app installed and joined |
| `Bprime` | the same with the levers this tree can pull from the host: the TCP server in `access_point` mode, `keepalive_interval` of the host's links set by `--bprime-link-keepalive` (600 s). The line named two levers missing until 1346, the TCP idle and the phone's link keepalive; both are the device link's defaults now (below), so `missing=` is empty and `B` already measures them on the phone's side |
| `A` | a standalone node with AutoInterface only, no mailbox. It fails at setup today: loquax-android starts no AutoInterface, and its first start offers only a mailbox at home or on the phone, both over TCP |
| `ref` | the reference app (Columba), given by `--ref-apk` and `--ref-package`, connected to the same lnsd. Its TCP client interface is set to the server once, by hand; the script installs over the old app with `-r`, so the setting stays. `--ref-address` is the app's LXMF address, which the traffic phase sends to |

Each mode runs on a fresh fixture and the same host clock: setup, a
settle time (`--settle`, 120 s, so that the first pull of the ugly store
is not counted as idle), 2 h idle (`--idle`), then 1 h (`--traffic`) with
one message every 10 min (`--message-every`). The host sends with
`loquax send` from the fixture's second identity, as the walk's
`receive_wake` step does.

## The proxies, what each stands for, and what it cannot show

| Field | How | Stands for | Cannot show |
|---|---|---|---|
| `segs_in_h`, `segs_out_h`, `bytes_in_h`, `bytes_out_h` | `ss -tin` on hamster for the server's end of the device's connections, every `--sample` (30 s): `segs_in`, `segs_out`, `bytes_received`, `bytes_acked` summed over every connection seen, a reconnect counted from zero | how often the radio has to carry something, and how much | which Reticulum packet it was (keepalive, announce, data); traffic of the app on any other socket; whether the kernel's keepalive probes count in `segs_out` is a positive control the reviewer runs once (with both ends probing every 5 s an idle connection shows about 1 440 segments an hour each way: 720 probes and 720 answers) |
| `conns` | the number of distinct connections in the phase | 1 is healthy; more are reconnects, the stale-link cycle the note fears | why the connection dropped |
| `radio_on_frac` | `ss` again every `--tick` (1 s): a growing counter is a packet, timed by `lastsnd`/`lastrcv` when those are younger than the tick, else at the tick's middle. On time is the sum of min(gap, `--tail`) over the packets, the tail 10 s by default | the share of the hour a cellular modem would stay in its high power state | a real modem's timers; packets within one tick count as one, so the share is low by up to a tick per packet |
| `cpu_s_h`, `cpu_src` | `/proc/<pid>/stat` (utime + stime) of every process of the package, read through `adb shell`, CLK_TCK from `getconf`; when `/proc` gives nothing, the package's percent in `dumpsys cpuinfo` | the CPU a phone would burn while awake | anything while the phone sleeps: Waydroid never suspends, so a timer fires here that a sleeping phone would skip |
| `vcsw_h`, `nvcsw_h`, `csw_src` | `voluntary_ctxt_switches` and `nonvoluntary_ctxt_switches` of every thread under `/proc/<pid>/task/*/status` | wakeups: each voluntary switch is a sleep that ended | which timer it was |
| `wakelock_held_frac`, `wakelock_src` | the `Wake Locks` section of `dumpsys power`, per sample: whether the app's uid holds one | whether the app keeps a phone awake; the note expects 0 for Loquax and a partial wakelock for Columba | the time between two samples |
| `bs_partial_wl_s_h`, `bs_cpu_s_h`, `bs_src` | `dumpsys battery unplug`, `batterystats --reset` before the idle phase, `--checkin` at each phase's end: the uid's `wl` and `cpu` rows | the framework's own accounting, the same that tier R reads | anything when Waydroid's batterystats do not count; `bs_src=no rows for the uid` says so |
| `sends` | the host's sends that exited 0, of those tried | that the traffic phase had traffic | that the phone showed the message |

The adb reads of the samples go over Waydroid's bridge but not over the
app's connection, so they do not show up in its counters. They do cost
adbd's CPU, not the app's.

## What Waydroid exposes without root

The script reads everything as hamster's user and as Android's `shell`
user through adb: `ss` on the host (no root needed for its own namespace),
`/proc/<pid>/stat` and `/proc/<pid>/task/*/status` (Android mounts `/proc`
with `hidepid`; that `shell` may read other uids' entries there is an assumption),
`dumpsys cpuinfo`, `power` and `batterystats`. Which of these give data in
Waydroid is shown per phase by the `*_src` fields: `cpu_src=proc` or
`cpuinfo` or `none`, `csw_src=proc` or `none`, `wakelock_src=dumpsys power`
or `no section`, `bs_src=checkin` or `no rows for the uid`. The first run
on hamster settles it; until then it is not known.

## How the reviewer runs it on hamster

Once: Waydroid's session up and `adb connect` to it, as for the walk;
Columba installed and its TCP client interface set to
`192.168.240.1:4242`, its LXMF address noted.

```sh
cargo build -p loquax --features tui --bin loquax
cargo build -p leviculum-cli --bin lnsd
cargo build -p loquax-android --example walk_mailbox
(cd loquax-android && make waydroid)
python3 -I loquax-android/scripts/power-proxy.py \
    --serial 192.168.240.112:5555 --server 192.168.240.1:4242 \
    --walk-mailbox target/debug/examples/walk_mailbox \
    --modes B,Bprime,A,ref \
    --ref-apk ~/apk/columba.apk --ref-package <package> --ref-address <address> \
    --out ~/scratch/power-$(date +%F)
```

The four modes take about 9 h plus setup, mode `A` failing at once. A
smoke run takes `--idle 300 --traffic 300 --message-every 120`. The device
walk takes `--server-mode` and `--link-keepalive` too, which is how
`Bprime` builds its fixture.

## The output

One `POWER mode=<m> phase=<idle|traffic> ok=1 ...` line per phase with
the fields above, rates per hour; a mode that could not run gives
`POWER mode=<m> phase=<setup|idle|traffic> ok=0 detail=<why>`. A TCP mode
whose phase saw no connection from the device has not run. Then a table
of every phase and `POWER_SUMMARY modes=<n> ok=<n> failed=<n> ms=<n>`.
`OUT/power.json` holds every phase's numbers with the wakelock tags,
`OUT/<mode>/samples.jsonl` the raw samples, `OUT/<mode>/walk.log` the
fixture's steps. The exit code is 1 when a mode failed to run, never
because of a number: the numbers are for the reviewer to judge against
the note's thresholds, which tier R applies.

## The device link's keepalives (1346)

Option O2 of the Android background study: the battery is won at the
keepalives, not at the architecture. Both are local timing; nothing on the
wire changes. `loquax/src/home/keepalive.rs` holds the values and why.

| | before 1346 | since 1346 | where |
|---|---|---|---|
| TCP keepalive idle, the device's dial to its mailbox or rendezvous | 5 s (`TCPInterface.py:85`) | 240 s, unless the config's `tcp_keepalive_idle_secs` says another | `TcpLiveness::for_device` in `loquax/src/config.rs`, applied by `attach_leaf` and the phone's own mailbox |
| TCP keepalive idle, the mailbox's phones' port | 5 s | 240 s, written into a new `[[Loquax Mailbox TCP]]` (lnsd takes it, rnsd ignores it); a config that runs is never rewritten | `loquax/src/home/phone_server.rs` |
| probe interval, probes, user timeout | 2 s, 12, 24 s | unchanged | `leviculum-std/src/interfaces/tcp.rs` |
| link keepalive on the device's link to its mailbox, both ends | `rtt * 360 / 1.75`, 10.3 s on a 50 ms path (`Link.py:845`) | 230 s | the phone's watching link (`loquax/src/home/watch.rs`), the mailbox on the first request of every device link (`loquax/src/home/daemon.rs`) |
| link keepalive on every other link | RTT-derived | unchanged | |
| the phone's idle pump | 360 s | unchanged | `IDLE_PUMP` in `loquax-android/src/session.rs` |

The link keepalive comes 10 s before the TCP probe would, so a live link
costs one exchange per 230 s and the TCP probe never fires; it is the
backstop for a connection that carries no link. 240 s stays under the 5 min
RFC 4787 REQ-5 recommends for a NAT's idle mapping (2 min is its minimum;
RFC 5382 REQ-5 asks 2 h 4 min for established TCP, which carrier NATs cut
short). A relay keeps an idle link 900 s (`Transport.py:89`, `:688`).

**A reference peer.** A responder goes stale at twice *its own* interval
(`Link.py:797`), so a long interval works only where both ends set it, and
both ends of the device link are Loquax. A Python rendezvous between them
relays the link and keeps it (900 s above), and probes its side of the TCP
connection every 5 s, which a phone dialling it pays for: only a leviculum
mailbox or rendezvous can lengthen its side. A mailbox from before 1346
keeps the RTT-derived interval and drops a quiet device link after about
20 s: the phone turns the long interval off after two watching links in a
row that ended before their first keepalive (`Lengthen`), and keeps the
reference's for the rest of its session.

**`access_point`.** The reference has no `I_WANT_TO_BE_AN_ACCESS_POINT`;
access point is the interface's `mode`. It blocks the broadcast of
announces onto the interface (`Transport.py:1195-1197`) and keeps paths
learned through it a day (`AP_PATH_TIME`, `Transport.py:72`, `:774`,
`:1875`). The phones' port has been written with `mode = access_point`
since 1176 and keeps it: every announce of the mesh would wake a sleeping
phone's modem.

**A silent mailbox.** "Not connected" (the service notice, `Notice.java`,
and the session's `link` event) comes within 300 s
(`NOT_CONNECTED_WITHIN`): the kernel drops an idle connection to a peer
that stopped answering after its idle and one probe, 242 s
(`InterfaceConfig::tcp_silent_peer_bound`; 26 s before), and the carrier
change (`leviculum_std::interfaces::carrier_changes`) wakes the idle pump,
which looks at once. A TCP client that loses its connection tells the core
nothing and redials on its own, so the carrier is the only word of it. The
status dot (`home_link`) follows: the watching link is closed when no
connection is left under it, instead of waiting out its stale time, twice
230 s. A mailbox that closes the connection (a restart, a power cut) is
noticed at once, as before.

**The counters.** Once an hour the phone's log says
`WAKE_SUMMARY keepalives=N links=N pumps=N bytes=N` (the keepalive round
trips of its links to the home, the links that came up, the pump's turns,
the bytes on its connections), and the mailbox says
`WAKE_SUMMARY device=<address> keepalives=N links=N wakes=N bytes=N` for
each device (`wakes` are the wakes it sent; `bytes` the requests, answers
and wakes, without link headers). Tier R reads them beside the mAh (the
study's M1 to M7).

## The parsers and their tests

`loquax-android/scripts/tests/test_power_proxy.py` pins the `ss -tin`
parser (with and without the header, v4-mapped addresses, the filter to
the device), `/proc` stat with a name holding parentheses, the task
status blocks, `ps` and `pm` with namesakes of the package, `dumpsys
cpuinfo`, `power` and `batterystats --checkin`, the accumulator over
reconnects and counter resets, the radio model, the timeline, the line
format, the table and the levers in the fixture's config. `cargo test -p
loquax-android` runs them through `loquax-android/tests/device_walk_script.rs`.
