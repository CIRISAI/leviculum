# Loquax for Android

The phone side of the Loquax home node (`docs/src/concepts/loquax-home-node.md`):
an Android app with a Java UI on Android's own views and the Loquax core
in a Rust library behind JNI. Why this shape and not another is the study
`docs/drafts/loquax-android-2026-10.md`.

What it does today:

- **First start** (1152, `docs/drafts/loquax-onboarding-2026-10.md`,
  section 2). A phone that holds no mailbox and reads none opens on
  "Where should your mailbox be?", "On this phone" preselected and its
  three warnings below it (background, offline mail waits in the network,
  back up your address). The background screen explains notifications
  and the battery exemption once, then Android asks twice. "Your address"
  ends it: the QR code, share, write a message, the name for others and,
  for a phone mailbox, the backup. "I already have a Loquax address"
  restores from a backup file or a scanned paper backup.
- **The mailbox on the phone.** `loquax --daemon`'s code
  (`loquax::home::daemon`) runs inside the app with a node of its own:
  TCP to the entry servers (`leviculum.network:4242`, or the lines of
  `mailbox/servers` in the app's files directory) and a TCP server on a
  loopback port for the app's own screens, which join it with a token it
  issues itself and stay its client, as of a mailbox on a computer. Its
  files are under `mailbox/`; `mailbox-here` marks the choice.
- **Join a mailbox on a computer.** Scan the code `loquax home admit
  --server` shows, paste it, or share it from another app
  (`lqxh1...@host:port`); a code without the address asks for `host:port`
  of the computer's lnsd TCP server. What went wrong is said in plain
  words, and the field it concerns is focused.
- **Backup.** "Back up your mailbox" in the settings seals the address
  with a passphrase (`loquax::home::backup`) into a file saved through the
  system's file picker and a paper code to print; both hold one line.
- **Conversations.** After a modern messenger's overview: a bar with our
  own avatar, "Loquax", a search (by name and last line) and a menu
  (pull now, the map, stop the service). Each row a large avatar circle
  (initials on a colour from the address, or the peer's own colours when
  it sent an LXMF icon appearance), the name as every Loquax screen gives
  it, up to two lines of the last message ("Position" for a report,
  "reacted 👍" for a reaction), the time ("Now", minutes, hours under a
  day, the weekday within the week, then the date; German on a German
  phone), and under it the unread count or, when ours is the last, its
  delivery mark. Newest activity first, again whenever a message arrives
  or goes. The round pencil button opens a screen to paste the address
  of a new conversation.
- **Chat.** After a modern messenger's layout: a bar with back, the
  peer's avatar and name and a menu (pull now, the map). Rounded bubbles,
  ours right in Loquax's green and theirs left in a grey a step lighter
  than the screen, at most 80 % of its width. Inside a bubble's bottom
  right its time and for ours the mark from the home node's archive: a
  clock queued or sending, a tick sent, two ticks in a filled circle
  delivered, a red mark failed. One sender's messages within three
  minutes group: close together, inner corners less round, only the last
  with its time (the newest group's in minutes ago). A day separator
  ("Today", "Yesterday", the weekday, the date; German on a German phone)
  between days. Reactions are pills over a bubble's bottom edge, the emoji
  and a count above one. A long press (1137, after Signal's) dims the
  chat around the bubble: above it Signal's six quick reactions, ours
  marked, and "…" for the full picker; below it reply, forward, copy,
  select, info, pin and delete. A reply shows its quote in the bubble and
  goes out with FIELD_REPLY_TO and FIELD_REPLY_QUOTE, as Columba,
  MeshChatX and Ratspeak send and read it; a forward is the text and the
  content fields again, unmarked; select copies or deletes several; info
  is the terminal's detail view; a pin is this phone's alone, a bar at the
  top. The compose field is a pill with the same full emoji picker behind
  its emoji button (categories, recents, search by English and German
  name, skin tones on a long press), and a round send button beside it. Send goes through the home node's `/send`. Opening the
  chat reads it (a read mark for the home's other devices) and pulls; the
  menu pulls; the sync schedule pulls while the app runs. Leaving the chat
  closes it, so what arrives then stays unread.

- **In the background.** After the start a foreground service
  (`LoquaxService`, type `remoteMessaging`) holds the session: the link to
  the home node and the sync schedule's quiet pulls go on with no screen in
  front. Its notification says the home node and the unread count and has
  a stop button, which ends the session and closes the app. A conversation
  with new messages gets a notification of its own, sender and last line,
  which opens the chat; none while a screen of the app is visible. When
  the system ends the process, the service comes back and starts the
  session from the saved code and address.
- **Asked once.** On the first start's background screen, or for a phone
  that joined without it on the first conversation list: notifications
  (Android 13 and later) and the battery optimisation exemption, with a
  plain reason. Neither is forced, neither is asked again.
- **Two tabs, one bar** (1199, `docs/src/concepts/loquax-android-navigation.md`).
  Chats and Map sit side by side under a bottom tab bar, each keeping
  its state; the selected tab survives a rotation, and back on the map
  goes to the chats. Both share one top bar: the user's avatar with a
  status dot (green while the watching link to the mailbox is up, grey
  otherwise), the tab's name, search on Chats, and a menu with Settings
  and Help (the book on leviculum.network). Chats carries a badge with the
  number of chats that hold unread messages. A chat, the archive, the
  settings and the map opened from a chat are sub views: a back arrow,
  no tabs. Settings is one list of eight pages: Profile, Mailbox,
  Appearance, Notifications, Privacy, Maps, Backup, Advanced; a page
  without controls yet says what exists today. Maps (1201) is where
  "Load area" went: the switch for the automatic download, the storage
  the maps take, and each map with its place, size, date and zooms and a
  Delete. Privacy lists the positions you shared, to whom and when, under
  the blocked contacts.
- **Map.** The contacts' latest positions as pins over Loquax's own map
  (the Map tab), drawn by the library with the window's
  renderer: the regions in the app's `maps/`, the world outline, the
  names, the pins, stale ones dimmed, in the map's dark colours when the
  phone is dark. One finger pans, a pinch or a double tap zooms, and the
  map is drawn anew while the finger moves: each frame a quarter screen
  past every edge, at the exact zoom of the pinch, which the map keeps
  when the fingers let go, composed from a tile cache, with the names the
  last frame at rest placed. At rest the map draws the tiles around the
  view ahead of time, first in the direction it last moved, while the
  app is in front and the battery saver is off. Never more than one
  world shows. The map has the whole room between the bars: a chip at
  the top edge says when a region loads, when there is no internet, when
  no region holds the view ("No map here yet") or a load failed. The
  people are the app's own (1200): each friend an avatar circle with the
  fix's age under it, overlapping ones one circle with a count, which a
  tap zooms into until they part (or, where no zoom parts them, lists in
  the Friends sheet). A tap on a pin opens its card: name, age, Chat, and
  Navigate, a `geo:` link for the phone's navigation app, hidden when
  none takes it. The round Friends button bottom right, and the search
  icon on the Map tab, open the Friends sheet: everyone with a position,
  the last day first, older ones greyed under a divider; a tap flies the
  map there and opens the card. A tap on a position report in a chat
  opens the map on its fix with the sender's card, and back returns to
  the chat. Location (1201): the crosshair above the Friends button
  centres on the phone's own position at about z16, a second tap follows
  it until a finger moves the map; while the map is in front the own
  position is a dot and the friends' distances are measured from it. The
  permission is asked at the first tap on the crosshair or on "Share my
  position", after a line that says why, never at start; refused, a chip
  says "Location is off". Reading sends nothing: the position leaves the
  phone only when shared, once, to one chat, from the empty map's "Share
  my position", a friend's card ("Share mine") or the chat's attachment
  menu (the paper clip, "My position"). Regions load by
  themselves around the pins, a report's place and where the user looks,
  as in the terminal (`map_autoload`, `map_source`, `map_cache_mb` in the
  app's `config`). Settings, Appearance chooses the map's colours: like
  the phone, light or dark (`map_theme` in the app's `config`).
- **Light or dark** by the phone's setting: the theme and every colour are
  resources with a `-night` set (`android/res/values-night/`), no AndroidX.
  Switching while a screen is open recreates it, as a rotation does; the
  session in the service goes on untouched.

Not yet: the battery measurement (study, section 3), UnifiedPush, moving
the mailbox between devices, AutoInterface on the phone's WLAN.

## How it is built

One Rust library holds the node, the engine, the store and Loquax's screen
model; Java only draws. The library's node is a leaf: transport off, one
TCP client interface to the home node, no lnsd and no shared instance on
the phone (`loquax::engine::attach_leaf`). Its session
(`src/session.rs`) runs the model's effects through the client runners the
terminal's shell runs too (`loquax::home::runner`). Java calls six functions with msgpack
bytes in the versioned schema of `src/schema.rs`:

| Call | What |
|---|---|
| `start` | start, joining with the code (`lqxh1...`, or `lqxh1...@host:port` with the address) on the first run |
| `command` | `open`, `close`, `send`, `react`, `pull` |
| `query` | `conversations`, `messages` of one peer |
| `nextEvent` | blocks on a Java thread: `changed`, `notice`, `sync`, `link`, `stopped` |
| `stop` | end the session |
| `renderMap` | one map frame (`src/map.rs`): the view and the gestures since the last in, the view and its RGBA pixels out; needs no session |

## Build

The build follows android-make-template: make, the SDK's command line
tools and a JDK, no Gradle, no AndroidX, no Google services. On top of the
template, make cross-builds the crate in this directory with cargo and puts
`libloquax_android.so` into the APK.

Once per host:

```bash
sudo apt install default-jdk-headless unzip make
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
loquax-android/install-toolchain.sh      # build-tools 35.0.1, android-35, NDK r27d
```

Then, in this directory:

```bash
make debug                               # android/bin/loquax-debug.apk, both ABIs
make debug ABIS=arm64-v8a                # the 64-bit library only
make waydroid                            # android/bin/loquax-debug-x86_64.apk, x86_64 only
make install-debug start-debug           # onto a connected phone
KSPASS=... make release                  # needs android/loquax.keystore
```

`make waydroid` is the debug build for x86_64 Android, Waydroid or the
emulator, under its own name so it never stands in for a phone's APK. The
scripted device walk installs it (`scripts/device-walk.py`,
[the walk's page](../docs/src/concepts/loquax-device-walk.md)).

The nightly publishes both on the download page, as
`loquax-nightly-android.apk` and `loquax-nightly-android-x86_64.apk`
(`scripts/build-nightly-apk.sh`, 1267), signed with a key kept outside the
tree so that each nightly installs over the last.

`make debug` makes `android/debug.keystore` on first use. No keystore goes
into git (`.gitignore`); the release key is made by hand:

```bash
keytool -genkeypair -keystore android/loquax.keystore \
    -alias release -keyalg RSA -keysize 2048 -validity 10000
```

## Layout

| Path | What |
|---|---|
| `Cargo.toml`, `src/lib.rs` | the JNI library, a workspace member; `cargo test -p loquax-android` runs its tests on the host, `tests/facade.rs` against an in-process home node |
| `src/schema.rs`, `src/session.rs` | the payloads and the session behind them; the rows, bubbles and avatars they carry come from `loquax::ui::chat` (1145), which the desktop window draws too |
| `src/map.rs`, `tests/map.rs` | the map screen's frames, and their test against the shared renderer and its golden (`tests/golden/`) |
| `Makefile` | the template's, with the `native` step and the Android settings below |
| `android/AndroidManifest.xml` | with the template's build-time tokens |
| `android/src/network/leviculum/loquax/` | the four screens (`MainActivity` joins, `ConversationsActivity`, `ChatActivity`, `MapActivity` with its `MapView`), `LoquaxService`, which holds the session, and `ServiceLink`, the screens' binding to it, `Asks`, the two questions asked once, `Native`, the Java face of the library, `Core` and `MsgPack`, which read it, `Events`, and the chat's drawing: `AvatarView`, `BubbleLayout` (body with time and mark inside) and `Glyph` (its icons as paths) |
| `android/res/` | layouts, the compose field's and badge's shapes, strings (the chat's times also in `values-de/`), the theme and its colours, light in `values/` and dark in `values-night/` |
| `icon/`, `scripts/render-icon.py`, `tests/icon.rs` | the jackdaw (1289, 1336): one geometry, coloured from `leviculum_ui::pixel::LIGHT`, written by the script as the SVG sources, the adaptive icon (`res/mipmap-anydpi-v26/`, its layers in `res/drawable/` and `res/values/ic_launcher.xml`), the legacy `ic_launcher.png` per density, the Linux package's `packaging/loquax/loquax.png` and the windows' `leviculum-ui-iced/assets/window-icon.png`; PNGs by resvg, `--check` compares the tree with a fresh render. The site's favicon is still owed: lblogd's page template has no icon link yet, and the site package ships no file for one, so the Loquax pages (`site/pages/help*.md`, `mailbox.md`) get `loquax.svg` once lblogd takes an `icon` in a page's front matter |
| `install-toolchain.sh` | the SDK and NDK pieces, checked by SHA-1 |
| `scripts/device-walk.py`, `scripts/tests/` | the scripted walk through the app on one adb device, and its parsers' tests, which `tests/device_walk_script.rs` runs |
| `examples/walk_mailbox.rs` | the walk's mailbox home: the 1173 ugly store in a mailbox's archive, and a manifest of what the screens must show |

## Where it differs from the template

- `MIN_API` is 26, not 19: Rust's Android std needs 21, and 26 brings the
  notification channels a foreground service needs.
- `ADB` is `adb` from the PATH. On hamster use a current adb from
  Google's platform-tools; Debian's freezes that host when a phone
  connects.
- `pack` sorts the class files before `d8` and adds the native libraries
  after `classes.dex`.
- The library is linked with 16 KiB page alignment and with the checkout
  and cargo home paths remapped, so neither path ends up in the library,
  and without its symbol table (the dynamic one JNI needs stays).
- `pack` adds `assets/FiraSans-OFL.txt`, the licence of the font the
  map's names are set in.
- `pack` adds `assets/emoji.tsv`, the picker's table from
  `loquax/emoji/` (Emoji 15.1, what Android 15 draws; an older phone's
  picker leaves out what its font cannot), and
  `assets/emoji-LICENSE-UNICODE.txt`, the Unicode licence it is under.
- SQLite inside the store is C, compiled by the NDK's clang, which the
  Makefile names for the cc crate.
- The Java has no lambdas: javac compiles them against
  `LambdaMetafactory`, which `android.jar` does not have.
