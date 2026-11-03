# Loquax: one messenger on every target

Lew's product rule (2026-10-25, restated 2026-11-02 21:40): the full
messenger on the phone, the Linux window, the terminal and the embedded
screen (the ThinkNode M9 today), from one core. Every feature moves its
platform layer on all of them where that is technically possible. This
page is the measured state of that rule, feature by feature (1341).

## How the table was measured

Each row is a user-facing feature of the phone build, found by walking the
Android facade's `Request` and `Query` enums
(`loquax-android/src/schema.rs:261`, `loquax-android/src/schema.rs:679`),
the bridge methods Java calls (`Core.java`), the activities that call them,
and the scene's node kinds (`loquax/src/ui/scene.rs`). For every target a
cell names the place that does it, `file:line`, or says MISSING. A MISSING
is a grep that found nothing, with a positive control beside it: for the
terminal's keys and palette, `Reply`, `Forward`, `SendAgain`, `Mute`,
`Verify`, `MarkUnread` and `Share` match zero times in
`leviculum-ui/src/keymap.rs` and `loquax/src/keys.rs`, while `Copy`
matches 14 and 8 times there. For the window, a MISSING intent is one
that falls into the catch-all arm of `App::intent` (`loquax/src/gui.rs`)
and has no other handler in `loquax/src/gui.rs` or
`loquax/src/gui/chat.rs`.

Columns:

- **Scene**: the decision lives in the shared core (`loquax::ui`, the
  scene, the intents); yes for every row, which is the point of the core.
- **Android**: the Java call site of the bridge method, under
  `loquax-android/android/src/network/leviculum/loquax/`.
- **Window**: `loquax gui` (iced), under `loquax/src/`; plain `loquax`
  on a desktop since 1342.
- **Terminal**: `loquax tui` (ratatui), under `loquax/src/`, and plain
  `loquax` where there is no display; `ui.rs` is the model's palette and
  keys, which only the terminal reads.

Window and terminal are one binary in the amd64 and arm64 packages
(1342): built against glibc 2.31 with both features, since winit opens
X11 and Wayland by dlopen, which a musl-static binary cannot. Plain
`loquax` picks by `WAYLAND_DISPLAY` and `DISPLAY` (`loquax/src/front.rs`);
the armhf package and the static tarballs stay musl with the terminal
alone.
- **M9**: see "The M9" below; the firmware has no messenger screen yet.

"CLI" means the target has it as a `loquax` subcommand
(`loquax/src/main.rs`), not on its screen. "n/a" means the target lacks
the hardware, which is no gap.

## The table

Rows marked (1341) were MISSING before this pass and are closed by it.

| # | Feature | Android | Window | Terminal |
|---|---------|---------|--------|----------|
| 1 | Chats: list, open, send text | `ChatActivity.java:1825` | `loquax/src/gui.rs:1006` | `loquax/src/ui.rs:2148` |
| 2 | Drafts | `ChatActivity.java:2321` | `loquax/src/gui.rs:848` | `loquax/src/ui.rs:1029` |
| 3 | React | `ChatActivity.java:1952` | `loquax/src/gui.rs:1057` | `loquax/src/ui.rs:3542` (palette `react`) |
| 4 | Reply | `ChatActivity.java:1825` | `loquax/src/gui.rs:1071` | MISSING |
| 5 | Forward | `ChatActivity.java:2053` | MISSING | MISSING |
| 6 | Send again | `ChatActivity.java:2445` | `loquax/src/gui.rs:1117` | MISSING |
| 7 | Delete a message | `ChatActivity.java:2129` | `loquax/src/gui.rs:1128` | `loquax/src/ui.rs:3504` |
| 8 | Copy a message | `ChatActivity.java:2073` | `loquax/src/gui.rs:1107` | `loquax/src/ui.rs:2339` (OSC 52) |
| 9 | Search | `ConversationsActivity.java:65` | `loquax/src/gui.rs:996` | `loquax/src/ui.rs:3500` |
| 10 | Read receipts on/off | `SettingsActivity.java:589` | `loquax/src/gui.rs:980` | `loquax/src/ui.rs:3478` |
| 11 | Send a picture (Gallery) | `ChatActivity.java:1355` | `loquax/src/gui.rs:945`, `loquax/src/desktop.rs:158` (1341) | MISSING |
| 12 | Photo size, three tiers | `SettingsActivity.java:316` | MISSING | MISSING (config key `photo_size` only) |
| 13 | View a photo | `ChatActivity.java:1675` | `loquax/src/gui.rs:1118` (own viewer) | `loquax/src/ui.rs:2367`, `loquax/src/shell.rs:341` (system viewer) |
| 14 | Camera | `ChatActivity.java:988` | n/a | n/a |
| 15 | Send a file | `ChatActivity.java:1408` | `loquax/src/gui.rs:948`, `loquax/src/shell.rs:773` (1341) | MISSING |
| 16 | Open and save a received file | `ChatActivity.java:1575` | `loquax/src/gui.rs:1119`, `loquax/src/shell.rs:348` | `loquax/src/ui.rs:2378`, `loquax/src/shell.rs:348` |
| 17 | Record a voice message | `RecordingBar.java:369` | `loquax/src/gui/chat.rs:1255`, `loquax/src/shell.rs:435`, `mic.rs` | `loquax/src/ui.rs:2850`, `loquax/src/shell.rs:435`, `mic.rs` |
| 18 | Play a voice message | `ChatActivity.java:1526` | `loquax/src/gui.rs:1122`, `loquax/src/shell.rs:367` | `loquax/src/ui.rs:2387`, `loquax/src/shell.rs:367` |
| 19 | Groups: new, join, members, add, remove, leave, delete, start again | `GroupDialogs.java:34` | `loquax/src/gui.rs:973` (all verbs) | `loquax/src/ui.rs:3569` to `loquax/src/ui.rs:3651` |
| 20 | Map with positions | `MapPane.java:350` | `loquax/src/gui.rs:1035` | `loquax/src/ui.rs:3522`, `tui/map.rs` |
| 21 | Buildings on the map | `loquax/src/picture.rs:44` (shared) | `loquax/src/picture.rs:44` | `loquax/src/picture.rs:44` (sixel, kitty) |
| 22 | Load the map area | `MapPane.java:35` (autoload) | `loquax/src/gui.rs:1143` | `loquax/src/ui.rs:3518` |
| 23 | Delete all downloaded maps | `SettingsActivity.java:513` | `loquax/src/gui.rs:987` | MISSING |
| 24 | Mesh positions (nodes nearby) | `MapPane.java:370` | `loquax/src/gui.rs:1310` | `loquax/src/tui/map.rs:1380` |
| 25 | Share the position, once or timed | `ChatActivity.java:910`, `ShareService.java:39` | MISSING | MISSING |
| 26 | Pin | `ConversationsActivity.java:481` | MISSING | `loquax/src/ui.rs:3512` |
| 27 | Mute | `ContactActivity.java:444` | MISSING | MISSING |
| 28 | Archive | `ConversationsActivity.java:485` | MISSING | `loquax/src/ui.rs:3513` |
| 29 | Block | `ContactActivity.java:353` | MISSING | `loquax/src/ui.rs:3546` |
| 30 | Verify | `ContactActivity.java:299` | MISSING | MISSING |
| 31 | Mark unread | `ConversationsActivity.java:479` | MISSING | MISSING |
| 32 | Rename | `ContactActivity.java:493` | MISSING | `loquax/src/ui.rs:3538` |
| 33 | Forget a conversation | `ContactActivity.java:411` | MISSING | `loquax/src/ui.rs:3508` |
| 34 | Add a contact by code, typed or pasted | `AddContact.java:76` | `loquax/src/gui.rs:821` | `loquax/src/ui.rs:3563` |
| 35 | A contact code from a QR | `loquax-android/src/lib.rs:326` (the scanner screen's decoder) | `loquax/src/gui.rs:1327`, `loquax/src/gui/chat.rs:441`, `loquax/src/scan.rs:39` (1341) | MISSING |
| 36 | My code | `ProfileActivity.java:232` | MISSING | `loquax/src/ui.rs:3668` |
| 37 | Home setup | `WelcomeActivity.java:528` | CLI (`loquax/src/main.rs:193`) | CLI (`loquax/src/main.rs:193`) |
| 38 | Link a device | `ProfileActivity.java:319` | CLI (`loquax/src/main.rs:207`, `loquax/src/main.rs:223`) | CLI (`loquax/src/main.rs:207`, `loquax/src/main.rs:223`) |
| 39 | Move the phone | `ProfileActivity.java:346` | MISSING | MISSING |
| 40 | Backup | `BackupActivity.java:117` | MISSING | MISSING |
| 41 | Help | `ConversationsActivity.java:421` | MISSING | `loquax/src/ui.rs:3446` |
| 42 | Settings | `SettingsActivity.java` | `loquax/src/gui.rs:1346` | palette (`loquax/src/ui.rs:3678`, `loquax/src/ui.rs:3478`) |
| 43 | Browse tab | `BrowsePane.java:187` (behind `browse_tab`) | off (`loquax/src/gui.rs:1052`) | off (`loquax/src/shell.rs:315`) |
| 44 | Notifications of new messages | `LoquaxService.java:305` | `loquax/src/gui.rs:236`, `loquax/src/desktop.rs:118` (1341) | `loquax/src/tui.rs:408`, bell and `loquax/src/desktop.rs:118` (1341) |
| 45 | Icon and application id | `AndroidManifest.xml:67` | `loquax/src/gui.rs:324` (1341) | n/a |

## Counts

| | MISSING before 1341 | MISSING after 1341 |
|---|---|---|
| Window | 20 | 15 |
| Terminal | 15 | 14 |

Counted are the MISSING cells of the table; the Browse tab (row 43) is
off on both desktop targets by decision, not counted, and "CLI" and "n/a"
are no gaps.

## What 1341 brought, and how

- **Pictures and files in the window.** The window sets the core's
  `Abilities::picker` (`loquax/src/gui.rs:163`), so the scene gives it the plus and
  the attachment sheet with Gallery and File, which the window draws over
  the bar (`loquax/src/gui/chat.rs:1171`). A row asks the XDG desktop portal's
  `FileChooser.OpenFile` over the session bus (`loquax/src/desktop.rs:158`), the
  dialog the desktop draws for every program. The crate is zbus, already
  in the lock through the window's theme reader (leviculum-ui-iced, mundy);
  neither `rfd` nor another dialog crate is in the tree, and zbus needs no
  GTK. The shell sends the pick as the phone sends its picker's
  (`loquax/src/shell.rs:773`): a picture scaled at the reader's photo size, a file
  whole up to what a receiver takes. Saving a received file is the
  existing save under the home's `files/` and the system's handler
  (`loquax/src/shell.rs:348`), the same in both desktop targets.
- **Voice** was already on both desktop targets: recording through the
  system's recorder (`pw-record`, `parecord`, `arecord`, `mic.rs`),
  playback as a WAV file for the system's player (`loquax/src/shell.rs:367`). No
  audio crate was added.
- **QR without a camera.** The phone's decoder (rqrr) moved from
  `loquax-android` into `loquax::scan`, and `scan::image_codes` reads a
  PNG, JPEG, WebP or GIF of a code (`loquax/src/scan.rs:39`). The window's new
  conversation offers "Read a code from a picture"; pasting a code's text
  already worked in both desktop targets.
- **Notifications** follow the phone's rule (`LoquaxService.java:305`):
  a grown unread count, not muted, not at the first look, not while the
  screen is in front (`desktop.rs`, `Notices`). The window posts through
  `org.freedesktop.Notifications` while it lacks the focus; the terminal,
  which cannot tell whether it is in front, rings its bell for every chat
  but the open one and posts the same notification where a session bus
  exists.
- **The application id** is `loquax`, the desktop file's basename
  (`packaging/loquax/loquax.desktop`), so Wayland matches the window to
  the launcher entry and its icon. No asset moved.

## GNSS on the desktop

A desktop has no position of its own. The core's position sheet needs an
own fix (`compose::position_rows`), so row 25 stays MISSING on both
desktop targets: the fix will be entered by hand on the desktop (a map
click or typed coordinates), never read from a receiver, and that entry
is not built yet.

## The Browse tab

1337b's fetch through the phone's own node landed, but in the Android
crate (`loquax-android/src/browse_fetch.rs`); the desktop shell has no
fetch of its own and drops the browser's effects (`loquax/src/shell.rs:315`). The
tab stays off in the window and the terminal until the fetch moves into
the shared shell; on a PC lnomad remains its own program meanwhile.

## The remaining gaps, with their reason

- **Window, rows 26 to 33 (pin, mute, archive, block, verify, mark unread,
  rename, forget):** the scene offers them on a row's menu, the window
  draws no row menu and no contact screen yet. Next window pass.
- **Window, row 5 (forward):** needs a conversation picker in the window.
- **Window, row 12 (photo size) and row 41 (help):** the settings page has
  no row for them yet; help needs the shared help page of
  leviculum-ui-iced wired in.
- **Window, row 36 (my code):** the window draws no QR yet; `qr` is in its
  features since 1341, so the code can come.
- **Terminal, rows 11, 15 and 35 (send a picture, a file, a code from a
  picture):** the terminal has no picker and its palette's asks are
  answered inside the core, which reads no files. The shell's attach
  (`loquax/src/shell.rs:773`) and the decoder (`loquax/src/scan.rs:39`) are in place; what is
  missing is a palette command and an effect that hands a typed path to
  the shell, a change to the core's intents.
- **Terminal, rows 4, 6, 27, 30, 31 (reply, send again, mute, verify, mark
  unread) and 23 (delete all maps):** no key and no palette entry; each is
  a `Command` and its arm in `ui.rs`.
- **Both desktop targets, rows 39 and 40 (move the phone, backup):** the
  phone's flows; on a computer the mailbox is the CLI's `loquax mailbox`,
  and a backup of a desktop identity has no screen yet.

## The M9

The firmware has no messenger screen yet: the order of work
([The handheld messenger](handheld-messenger.md), "Order of work") puts
the M9 frontend at step 5, after the status screen and the core taken out
of `loquax`, and the tree holds no M9 binary today (`leviculum-esp/src/bin`
has the Heltec V4 alone). What the board can carry once it has one, from
[The ThinkNode M9](thinknode-m9.md): a 240x320 screen without touch and a
37-key keyboard, GNSS, a buzzer.

- Rows 1 to 10, 19, 26 to 34, 36, 41 and 42 (text, groups, the contact
  verbs, settings): the core's scene on the 240x320 screen, driven by the
  keyboard as the terminal is.
- Rows 11 to 15 and 35 (pictures, files, camera, QR): the board has no
  camera; a received photo can be drawn small, a file only listed.
- Rows 17 and 18 (voice): the board has no microphone and only a buzzer,
  so it neither records nor plays a voice message; it shows that one came.
- Rows 20 to 25 (map, positions, sharing): the GNSS gives an own fix, so
  sharing a position is possible there, unlike on the desktop; the map is
  steps 6 and 7.
- Row 44 (notifications): the buzzer and the screen.
- Rows 37 to 40 (home setup, link, move, backup): through the phone or
  the CLI, not on the device.

## Related

- [Loquax: architecture](loquax-architecture.md)
- [Loquax: the user interface](loquax-ui.md)
- [Loquax on Android: navigation and views](loquax-android-navigation.md)
- [The handheld messenger](handheld-messenger.md)
