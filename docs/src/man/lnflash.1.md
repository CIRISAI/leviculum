# lnflash(1)

## NAME

lnflash -- flash, configure and watch LNode boards

## SYNOPSIS

**lnflash** [*options*]\
**lnflash** **--set-time** | **--set-telemetry** | **--set-media** [*spec*] | **--set-name** [*name*] | **--set-position** *lat,lon[,alt]* | **--set-tx-power** *dbm* | **--set-tx-spacing** *ms* | **--set-ble-tx-gap** *ms* | **--store-storm** *count[,bytes]* | **--announce**\
**lnflash** **--management-identity** [*hex*] | **--clear-management** [**--port** *tty*]\
**lnflash** **--watch** [*serial-or-port*] [**--out** *file*]\
**lnflash** **--summarize** *file*

## DESCRIPTION

**lnflash** brings an LNode board up on the firmware bundle beside the binary: it finds attached boards, brings each into its bootloader, confirms from the bootloader what the board is, checks the SoftDevice precondition and writes. It asks nothing: starting it is the decision to flash every board it recognises and has an image for, and **--dry-run** shows what that would do. Serial devices it does not know (an ESP32 RNode, a ThinkNode M9) get one line each and are left alone; lnflash does not flash ESP32 boards. After a flash the radio is settled without a question: stored settings other than the shipped eu868 profile are kept, otherwise the board gets the preset the host's time zone (or locale) names, eu868 where none does. Telemetry and remote management stay off. A port another program holds (lnsd's serial medium opens every LNode's transport port) is named with its holder and left alone. **--ask** puts every one of these decisions as a question. It needs no network and no external programs; writing needs root because the bootloader drive is a root:disk block device.

The configure sessions (**--set-** *something*) talk to boards that are already running and never flash: activation is configuration, not firmware. Each session ends the run, so only one of them can be given at a time.

**--watch** and **--summarize** are the field-testing pair: the first records a board's debug log with wall-clock timestamps, the second reads such a recording back. See FIELD WATCH below.

## FLASHING OPTIONS

**--bundle** *path*
:   Where the bundle is. Otherwise $LNFLASH_BUNDLE, then next to the binary, then /usr/share/lnflash.

**--board** *name*
:   A board model that is present; repeatable. A board whose model is not named is skipped and nothing is written to it; one found running is decided on its USB ID before the 1200-baud touch, and is not touched ("not named, not touched"). A Board-ID several models share is written without a question only when its model is named, or when the board was running our own image for that model (a re-flash: our USB ID and manufacturer string, settled before the touch), which is how an unattended run under **--yes** flashes boards one owns: **--board t114 --board rak4631 --yes**.

**--dry-run**
:   Report what is attached and what would happen; change nothing. A board in application mode is not rebooted into its bootloader. A board already in its bootloader is identified only under root, because reading `INFO_UF2.TXT` means mounting it; a question the identity step would ask is reported as "would ask", and an identity a real run would refuse as "would be refused". A dry run refuses nothing itself.

**--ask**
:   Ask instead of deciding: the overwrite question for every board, the radio menu (eu868, us915, au915, custom), and the telemetry and remote-management questions, each defaulting to no.

**--yes**
:   Answer yes to every confirmation but one. Writing a board at level *expected* (wiring checked, never run by us) needs the word yes typed at its question, by a person or piped in, and **--yes** deliberately never gives it: that question is asked before the 1200-baud touch, so the board is refused untouched and keeps running. Fails rather than waits when a board needs a physical double-tap.

**--check-bundle**
:   Verify the bundle's manifest and payload checksums, then exit.

**--radio-preset** *name*, **--radio-freq** *hz*, **--radio-bw** *hz*, **--radio-sf** *n*, **--radio-cr** *n*, **--radio-txpower** *dbm*, **--no-radio**
:   The radio settings written after a flash, instead of the estimate. A preset (eu868, us915, au915) and explicit values are two ways to state one configuration; pick one. **--no-radio** leaves the board's stored settings alone. Without any of them the estimate rule applies: a board reporting stored settings other than the shipped eu868 profile keeps them; otherwise the preset of the host's country is sent, the country read from the time zone (`$TZ`, `/etc/timezone`, `/etc/localtime`, through `zone.tab`) and, failing that, the locale's territory (`$LC_ALL`, `$LC_MEASUREMENT`, `$LANG`): eu868 in CEPT Europe, us915 in the US, Canada and Mexico, au915 in Australia and New Zealand, the shipped eu868 elsewhere, each said with its reason.

**--telemetry** *address*, **--telemetry-profile** *name*, **--telemetry-key** *hex*, **--no-telemetry**
:   The telemetry target sent after a flash, or the answer for **--set-telemetry**. Without them a flash sends none; **--telemetry-profile** alone, or **--ask**, asks for the address.

**--quiet**
:   Print less. With **--watch**, print nothing (an **--out** file is then required).

## CONFIGURE SESSIONS

Each finds every running LNode on the bus, talks to it over the control envelope on the transport CDC (if02), reports what each board answered and exits. **--set-time** teaches the boards the host clock; **--set-tx-spacing** sets the on-air transmit spacing (not persisted); **--set-tx-power** sets the transmit power (persisted); **--set-position**/**--clear-position** pin or release a fixed position; **--set-media** reads or sets which carriers a board meshes over; **--set-name**/**--clear-name** read or set what the board is called; **--set-telemetry** configures the telemetry target; **--set-ble-tx-gap** sets the gap the board leaves between packets on one Bluetooth connection, 0 to 5000 ms (not persisted; 0 imposes nothing); **--announce** makes each board make every announce it makes on its own cadence, immediately and on all interfaces: its LXMF delivery destination exactly as its telemetry path does, and on a board running the propagation node role its lxmf.propagation destination too — a board without a calendar clock withholds the delivery announce and says so (run **--set-time** first), while the role's is not clock-gated; **--store-storm** *count*[,*bytes*] asks each board to append that many synthetic records to its message store (bytes defaults to 304, the measured field median; at most 1000 records of at most 1024 bytes). A flag given with no value, where allowed, only reads the boards back.

**--store-storm** is a bench instrument for the board's message store and the only thing that writes to it today: nothing on a board stores messages yet. It provokes the flash page erases a filling store causes, so their cost to Bluetooth throughput and LoRa airtime can be measured without waiting for a mesh to fill the region. The board acks the request and appends on its own task, so watch its debug port (**--watch**) for the `STORE storm` line that reports what landed, and `STORE op_fail` for every flash operation the SoftDevice refused. Nothing is persisted as configuration and the records are tagged as synthetic; a board that is still running a previous storm refuses a new one.

**--management-identity** [*hex*] and **--clear-management** set, read back (no value) or empty the list of identities that may query a board remotely (`rnstatus -R`, `lnstatus -R`, `lnmgmt dfu`); the list takes effect at the board's next reset, and the board's answer says whether the running list is already the stored one. Like the sessions above they talk to every running LNode, unless **--port** *tty* names one: then only the board that owns that serial port (any of its CDC ports, a `/dev/serial/by-id` link works) is opened and the others are not touched. `lnflash --management-identity --port` *tty* lists one board's list, `lnflash --clear-management --port` *tty* empties it.

## FIELD WATCH

**--watch** [*serial-or-port*]
:   Open a running board's debug CDC (if00) with DTR and RTS raised — the firmware transmits only with both set — and keep reading. Every line is prefixed with an ISO-8601 wall-clock timestamp with milliseconds (the board's own `t=` stays in the line). If the port vanishes (reset, reflash, unplug) the gap is logged as its own `[WATCH]` line and the port is reopened with a bounded backoff; the watch never exits on EOF. With no value and exactly one running board, that board is watched; with several, name a board's USB serial or bus port (e.g. 3-2.4). A value containing a slash is opened directly as a serial port path. The watch runs until interrupted. It is not a daemon and has no background mode; run it in a terminal, or under **nohup**(1) yourself.

**--out** *file*
:   Append every watched line to *file* as well as stdout, flushed per line so a crash loses nothing. The file is the evidence a field walk leaves.

**--summarize** *file*
:   Read a watch file and print, per hour, how many LoRa receptions of each class it holds — announce, data, path request — plus the last line seen per class and the number of reconnect gaps. Classification uses the `flags=` byte in a `[LORA] RX` line when present (the low two bits are the Reticulum packet type; a data packet to the well-known path request destination is a path request) and the class word otherwise; a bare `RX n bytes` line counts as unclassified. The watch itself never filters: the file is the evidence, the summary is a view of it.

## EXAMPLES

Watch the only attached board, keeping the log:

    lnflash --watch --out walk-$(date +%F).log

Watch one of several boards by serial, silently:

    lnflash --watch 183004F712B4A7FE --out walk.log --quiet

Summarize the walk afterwards:

    lnflash --summarize walk.log

## EXIT STATUS

0 when every addressed board did what was asked (for **--watch**: never reached; the watch runs until killed).

On a flash run the three outcomes are kept apart, because they need different things done about them. Each board's line states which build it is running, which mechanism read it — the `[FW_BUILD]` banner the board emitted after the reset **lnflash** triggered — which port that line was read on, which device node the open was proved against, and how long after the port was flushed the line arrived. A line that arrived in the first milliseconds was already in flight and is worth doubting; a board that says nothing is reported as unknown and no sha is named for it.

0
:   Every board was written and named the build in this bundle on its debug port.

1
:   The flash failed: a board never came back, or came back naming a different build, or nothing was written to it. A board refused on its identity (a shared Board-ID under **--yes** alone, a board at a level that needs a confirmation nobody gave, a Board-ID the catalogue does not know, the wrong model for **--board**) counts as nothing written, and fails the run even when every other board was confirmed. A board found in its bootloader, or touched there before it could be identified, stays there; one refused at the level question while still running was never touched and keeps running. A board skipped because the run was not for it (not among the boards named with **--board**, not flashed by this tool, or stopped by **--dry-run**) does not count. The closing summary names refused and skipped boards on separate lines, each with a count and the ports.

2
:   Every board took the write and none contradicted it, and at least one could not be read back. The firmware is on the board as far as anything here knows; which build it is running is unknown. Read it back again (**--watch**, or re-run the flash) rather than assuming the write failed.

A **--dry-run** exits 0 once it has walked the bus: it writes nothing, so it refuses nothing, and a board a real run would refuse is a skip with that reason said.

Every other session exits 0 when every addressed board did what was asked and 1 otherwise.

## SEE ALSO

**lnsd**(1), **lnstatus**(1), **lnprobe**(1)

The flashing design and its evidence: `docs/src/concepts/lnode-flashing.md`. Field testing: `docs/src/guide/field-testing.md`.
