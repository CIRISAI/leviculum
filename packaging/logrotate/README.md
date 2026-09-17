# Capping the event log on a public node

`LEVICULUM_EVENT_LOG` is the most valuable and the largest thing a public
node writes. The miauhaus soak node reached 60 GB, at times around 30 GB a
day. The host `leviculum.network` is moving onto has about 29 GB free, and
the disk that fills is the same disk the propagation node's message store
lives on — an unrotated log does not lose you diagnostics, it loses you
mail.

Three files here:

| file | what it is |
| --- | --- |
| `leviculum` | the logrotate config, installed as `/etc/logrotate.d/leviculum` |
| `leviculum-logrotate.service` | a oneshot that runs logrotate against it with its own state file |
| `leviculum-logrotate.timer` | fires that oneshot every 15 minutes |

## Install

```sh
sudo install -d -o leviculum -g leviculum -m 0750 /var/log/leviculum
sudo install -d -o lnpnd     -g lnpnd     -m 0750 /var/log/lnpnd
sudo install -m 0644 leviculum /etc/logrotate.d/leviculum
sudo install -m 0644 leviculum-logrotate.service leviculum-logrotate.timer \
    /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now leviculum-logrotate.timer
```

Neither daemon sets `LEVICULUM_EVENT_LOG` on its own; the shipped units log
to the journal only. Point them at a file with a drop-in, once per daemon:

```sh
sudo systemctl edit lnsd.service    # Environment=LEVICULUM_EVENT_LOG=/var/log/leviculum/events.log
sudo systemctl edit lnpnd.service   # Environment=LEVICULUM_EVENT_LOG=/var/log/lnpnd/events.log
```

`ProtectSystem=strict` in both units means the log directory also has to be
named in a `ReadWritePaths=` line of the same drop-in, or the daemon starts
and silently logs nothing:

```ini
[Service]
Environment=LEVICULUM_EVENT_LOG=/var/log/lnpnd/events.log
ReadWritePaths=/var/log/lnpnd
```

Check the rotation without waiting for the timer:

```sh
sudo logrotate -v --state /var/lib/logrotate/leviculum.status \
    /etc/logrotate.d/leviculum
```

A working rotation prints `copying …` followed by `truncating …`. If it
ever prints `renaming`, the `copytruncate` directive has been lost and the
next section says what that costs.

## copytruncate, not the default

The writer opens the log **once per process** and caches the handle for the
life of the daemon — `event_log_file`,
`leviculum-std/src/event_log.rs:668-684`, a `OnceLock<Option<Mutex<File>>>`
filled on first use. There is no reopen: no SIGHUP handler, no retry. That
is deliberate (a failing `open(2)` on the tracing hot path is worse than a
log that stopped), and it decides the rotation mechanism.

Under logrotate's default — rename the file aside, create a new one — the
daemon keeps writing into the renamed inode for ever. The new file stays
empty, the old one keeps growing, and the disk is never freed: the exact
failure the rotation was installed to prevent, arriving silently.

`copytruncate` keeps the inode. The cached handle is `O_APPEND`, so after
the truncate the next write lands at offset 0 of the same file and an
operator keeps seeing a live log.

Both halves are pinned by `leviculum-std/tests/event_log_rotation.rs`:
one test rotates by copytruncate mid-run and asserts every event lands
somewhere and the live file keeps growing; the other injects the rename
rotation and asserts the new file stays empty. The second is the positive
control — if the writer ever gains a reopen path it goes red, and that is
the signal to revisit this directive.

The known cost is the window between the copy and the truncate: events
emitted inside it are lost. Against a log measured in gigabytes that is a
handful of lines, and it is the price of a writer with no reopen.

## The ceiling

`size 512M`, `rotate 8`, `compress`:

* **Hard bound, no assumptions: 9 × 512 MB = 4.6 GB.** True even if
  compression achieved nothing at all.
* Measured, for the realistic case: gzip -9 on 200 000 synthetic `PN_*`
  lines of the shape this daemon emits (random hex identifiers, so a
  conservative floor — a real log repeats peer and destination hashes and
  compresses better) gave **3.5:1**, 25.1 MB → 7.2 MB. That puts the
  expected steady state near 512 MB live plus ~1.2 GB archived.

Against 29 GB free, either figure leaves the disk to the message store,
which is what has to survive.

`size` is only evaluated when logrotate **runs**, so the timer interval is
part of the ceiling: the live file can overshoot by one interval's worth of
writing. At 15 minutes and the miauhaus peak rate that is about 310 MB. On
the distribution's daily schedule the same node would reach 30 GB between
checks and the ceiling would be a decoration — which is why this ships its
own timer instead of a `daily` line.

`delaycompress` leaves the newest archive uncompressed for one cycle. That
is on purpose twice over: the file an operator reaches for first stays
greppable, and nothing compresses a file the writer may still have been
appending to during the copy window.

The oneshot uses `/var/lib/logrotate/leviculum.status` rather than the
distribution's status file. Two schedules sharing one state file make each
one's "when did I last rotate this" answer wrong.

## Reading the log

`scripts/analyze-lnpnd.py` summarises an `lnpnd` event log in one streaming
pass with constant memory, which is the only shape that works at this size.
Measured on this machine over synthetic `PN_*` traffic: 25 MB in 0.27 s at
10.7 MB RSS, 251 MB in 2.54 s at 10.8 MB RSS — ten times the input, the
same memory. Point it at the live file and the archives together:

```sh
zcat -f /var/log/lnpnd/events.log* | python3 scripts/analyze-lnpnd.py -
```
