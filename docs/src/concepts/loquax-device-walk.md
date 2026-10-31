# Loquax on Android: the scripted device walk

Status: slice one, order 1175 (Lew, 2026-10-31; Refs #232). The script is
`loquax-android/scripts/device-walk.py`; the reviewer runs it on hamster
every night and reads its log.

## Why

The three device bugs of 2026-10-30/31 (an import the running session did
not see, names that did not come, Info that failed on an imported row)
were found by Lew's thumb, on a real phone, with a real store. The host
tests had small, fresh, well formed stores and passed. The ugly store of
order 1173 (`loquax/tests/support/ugly_store.rs`) brought the store's
shape into the host tests; the walk brings the screens. It installs the
app on an Android device the reviewer controls through `adb`, against a
mailbox on the same host that holds the ugly store, and does what the
thumb did, step by step, reading the screen after every step.

## What it proves

One walk, one APK, one device, one fresh mailbox. Each step prints one
line, `WALK step=<name> ok=<0|1> ms=<n> detail=<...>`, and the walk ends
with `WALK_SUMMARY ok=<n> failed=<n> ms=<total>` and exit 1 on any
failure.

| Step | What it asserts on screen |
|---|---|
| `fixture` | on the host: lnsd with a TCP server on `--server`, `loquax mailbox run` over the ugly store's archive (800 messages, 80 imported), an admission code for one device |
| `install` | the app data cleared, the APK installed with its run time permissions granted |
| `first_start` | the onboarding's "Where should your mailbox be?", "On a computer at home", Continue, "Paste a code", the code typed in full, Join; the notification and battery questions dismissed if they show; the conversation list appears |
| `list` | within 10 s of the list and without a restart, a named row shows; scrolling, every conversation that is neither archived nor blocked shows (18 of the store's 20), each named one by its name and never as the address form `<8 hex…>` |
| `open_imported` | the conversation imported from Ratspeak opens; scrolled to the top, its topmost body is the oldest imported one and a day separator shows |
| `info` | a long press on an imported message, Info: the "Message details" sheet shows and no text on screen says "did not decode" |
| `reply_send` | a long press, Reply, a typed body, Send: the bubble shows with a delivery mark of "sent" or "delivered ✓" within 5 s, and the mailbox's log says `HOME send accepted` |
| `receive_wake` | a second `loquax` on the host, with the Ratspeak peer's identity, sends to the mailbox; the bubble appears in the open chat without a tap, seen by a UI dump begun within 3 s of the send |
| `receive_report` | back to the chats, the same peer sends a position report (`loquax send --position`, no body): within 5 s the row's last line reads "📍 live"; `dumpsys notification` lists no new or raised again notification tagged with the peer; and at least half of 16 points sampled on the middle of the row avatar's ring changed from before the report (1209: a report is a state, not a message) |
| `map` | the Map tab opens the map; its view is not uniform and has no black part of sixteen; the dump shows exactly the two tabs Chats and Map and none of the old header controls ("Load area", "All", the sun or moon, 1199), and the crosshair (1201); after each of two zooms and a pan, at least 5 % of the sampled pixels changed and no part is black; Settings, Appearance set to light and then dark changes the map's mean brightness |
| `friends` | the Map tab's Friends button opens the sheet; every name in it is a peer of the fixture that sent a position (the manifest's `friends`), none twice, under the "Older than a day" divider (the ugly store's fixes are months old); a tap on the first closes the sheet and opens that friend's card with its name; Chat on the card opens the chat, back returns to the map with the card, back closes the card, back goes to the chats (1200). Whether Navigate shows (an app takes `geo:`) is said in the detail, not asserted |
| `settings` | More, Settings: the list shows the eight entries Profile, Mailbox, Appearance, Notifications, Privacy, Maps, Backup, Advanced in that order, each with a one line description, no raw hash (32 hex digits or `<8 hex…>`) on a main row and nothing that "did not decode"; each page opens, shows nothing that did not decode and its own mark where it has one (Profile the QR code `profile_qr`, which it shows since 1187 opened the profile screen from there, Appearance "Like the phone", Privacy "Positions you shared", Maps the automatic download switch `maps_auto`), and back returns to the list; back goes to the chats (1201). Before the pages the walk puts the repository's Florence region (`leviculum-map/fixtures/firenze.lmr`) into the app's `files/maps` through `run-as`; the Maps page must show the automatic download switch on (the default, 1089), a storage line with a size that is not 0 B, a row of that region ("Around 43.77° N, 11.25° E") whose line has its size, and nothing that did not decode; the switch is turned off, the page left and opened again still off (stored), and turned on again, every dump on the way free of "did not decode" (1207, after Lew's phone showed the switch off, 0 B and no maps). A toast is not in every `uiautomator dump`, so the stored state is what proves the request decoded. The Maps page's line is said in the detail |
| `profile` | from the chats, a tap on our avatar at the top left opens the profile (1187): its address row `profile_address` is 32 hex digits and the fixture mailbox's address, the QR code `profile_qr` and Share `profile_share` are there, nothing "did not decode"; back goes to the chats |
| `teardown` | the device revoked on the mailbox, lnsd and the mailbox stopped, the app uninstalled |

Every assertion reads the screen: the texts and content descriptions of a
`uiautomator dump`, or pixels sampled from a raw `screencap`. A timing is
only ever measured and reported, never taken as proof that something
happened. A failing step writes `<step>.png` and `<step>.xml` (the screen
and its UI dump) beside `walk.log`, and the walk goes on where the next
step can still start.

## What it does not prove

- **No real radio, no real phone.** Waydroid runs Android in a container
  on hamster's kernel. There is no battery, no doze as a phone lying
  still does it, no mobile network, no BLE, no camera. The wake step proves
  the wake path over a LAN TCP link that never drops.
- **x86_64 only.** The APK of `make waydroid` carries only the x86_64
  library. The arm64 and armv7 libraries of `make debug` are the same Rust
  source cross-built, but a bug that only one target's code generation
  makes, or one that only a 32-bit `usize` makes, stays invisible.
- **One gesture is not the thumb's.** `adb shell input` has one pointer,
  so the map's zoom is a double tap (`MapView.onDoubleTap`, the same zoom
  path as a pinch), not a pinch. A bug in the pinch detector itself stays
  for the thumb.
- **One mailbox, one fixed store.** The walk joins over the mailbox's TCP
  server on the LAN. Rendezvous, mesh paths
  ([reaching the mailbox](loquax-reachability.md)) and the mailbox on the
  phone are not walked.
- **English only.** The step texts are `android/res/values/strings.xml`'s;
  a device set to German shows `values-de/`'s and the walk fails at the
  first text.

## How the reviewer runs it on hamster

Once: Waydroid's session up, `adb connect` to it, so `adb devices` lists
its serial. The device must reach the host's TCP server: Waydroid's
bridge gives the host `192.168.240.1` by default.

```sh
cargo build -p loquax --features tui --bin loquax
cargo build -p leviculum-cli --bin lnsd
(cd loquax-android && make waydroid)
python3 -I loquax-android/scripts/device-walk.py \
    --serial 192.168.240.112:5555 \
    --server 192.168.240.1:4242 \
    --out ~/scratch/walk-$(date +%F)
```

`--loquax` and `--lnsd` name other host binaries (default: the workspace's
debug ones), `--apk` another APK, `--listen` the address the TCP server
binds (default `0.0.0.0`), `--walk-mailbox` a built `walk_mailbox` example
(default: `cargo run` it), `--steps` a subset of the steps in the walk's
order, `--server-mode` and `--link-keepalive` set the TCP server's interface
mode and the host's link keepalive (the power proxy's levers,
[tier P](loquax-phone-power.md)), `--keep` leaves the fixture, the device's admission and the app in
place for a look by hand. The fixture's homes, logs and the code live in
`OUT/fixture-*/`.

## How the fixture is made

`loquax-android/examples/walk_mailbox.rs` includes the ugly store's module
as the host tests do and writes three things: the store into the archive
of a mailbox home, through the same writer the home node tests use; the
Ratspeak peer's identity into a second home, the sender of
`receive_wake`; and a manifest in JSON with the mailbox's address, every
conversation's peer, the name a screen shows for it, whether it is
archived, blocked or imported, and the bodies the `open_imported` step
looks for. The script asserts against the manifest only, so a change to
the ugly store changes what the walk expects in the same commit.

## How to add a step

1. Add its name to `STEPS` in the walk's order, and a method
   `step_<name>(self) -> str` on `Walk`. It returns its detail; it raises
   `StepFailed` with the reason when an assertion fails. The step's line,
   its time and the evidence on failure come from `run_step`.
2. Read the screen with `self.need_device().dump()` and `find`, wait with
   `wait_text` or `wait_id`, never with a bare sleep as proof. Pixels come
   from `dev.screen()` and `sample_grid`.
3. A step that needs a new fact about the store takes it from the
   manifest: add the fact to `walk_mailbox.rs`, from the ugly store's own
   functions, not as a literal in the script.
4. A new parser gets a test in `loquax-android/scripts/tests/`;
   `cargo test -p loquax-android` runs them through
   `loquax-android/tests/device_walk_script.rs`.

## The parsers and their tests

The UI dump goes through `parse_dump`, which also repairs what Android's
serializer writes for a character outside the BMP: two character
references to its UTF-16 surrogates, which XML forbids. Every emoji in
the ugly store's names is such a character, so without the repair the
first dump of the list would not parse. The raw screencap goes through
`parse_screencap`, with either header Android writes (12 bytes before
Android 9, 16 after). The tests pin both, the sampling (uniform, black
parts, the share of changed samples, the brightness), the admission code
and the `WALK` line format.
