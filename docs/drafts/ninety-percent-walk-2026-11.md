# The ninety percent walk: Loquax's main path for the first user

Order 1259, Lew's rule of 2026-10-31: the apps are for the 90 % who know
nothing of Reticulum; defaults decide; no Reticulum vocabulary on the main
path; setting up a mailbox means scanning a QR code. This is the first check
against that rule. Docs only: the reviewer turns section 4 into orders.

## 1. How the walk was done, and what it could not see

- **Android: read, not driven.** The pass ran on schneckenschreck, which has
  neither Waydroid nor `adb`; the device walk
  (`loquax-android/scripts/device-walk.py`,
  `docs/src/concepts/loquax-device-walk.md`) runs on hamster, which this
  pass cannot reach. So every Android screen below is walked in the source:
  the strings (`loquax-android/android/res/values/strings.xml` and the
  generated `strings_scene.xml`), the layouts and the activities that put
  them on screen. Tap counts come from the code paths, so they are the
  number of taps the code asks for, not a measurement of a thumb. Android's
  own dialogs (permissions, the photo picker) count as one tap each. There
  are **no Android screenshots**; a device walk on hamster should add them.
- **Terminal: driven.** The TUI was run for real: the workspace's
  `loquax` (feature `tui`) and `lnsd`, a fresh `LNMSG_HOME`, the
  `--script`/`--frames` harness of `loquax/tests/tui_snapshot.rs`, 80 x 24.
  The frames are in `target/1259/tui/` (`first.frames`, `second.frames`,
  `nodaemon.out`, `admit.out`). The later steps (send, receive, read, map)
  are taken from the pinned frames in `loquax/tests/fixtures/`
  (`tui_session.frames`, `tui_chat.frames`, `tui_names.frames`), which the
  same harness writes.
- **The comparison** with WhatsApp and Signal is from public knowledge,
  not from a device; where I am unsure I say so.

## 2. The walk, step by step (Android)

For each step: the words on screen, the taps from the previous step, the
choices, the Reticulum words, and where a default could have decided.

### 2.1 Install

- There is no APK on the download page: `site/pages/download.md` lists the
  firmware, `lnsd` and `loquax` `.deb` files, nothing for Android. A first
  user cannot install the app at all today, except from a build.
- Taps: not countable. Choices: none offered.

### 2.2 First start (mailbox on this phone, the preselected answer)

Screen "choice" (`WelcomeActivity.java` `choice`, strings 160-173):
"Loquax", "Messages that do not depend on any company.", "Where should your
mailbox be?", "Your mailbox receives your messages and keeps them for you.",
radio "On this phone" (preselected) with "Ready now. Messages reach you while
the phone is on and online.", radio "On a computer at home" with "Messages
arrive even when your phone is off. You need a computer or a Raspberry Pi
that stays on.", the box "Your mailbox on this phone" with three bullets
(battery, "messages wait in the network ... The sender sees "delivered"
only then.", "Your address lives on this phone. Save a backup, or a lost
phone means a new address."), "You can move your mailbox later without
losing anything.", buttons "Use this phone" and "I already have a Loquax
address".

- Taps: 1 ("Use this phone"). Choices: 1, defaulted. Reticulum words: none.
- Then "Let Loquax run in the background" / "Otherwise Android stops it when
  the phone lies still, and messages are late. Android will ask you twice
  now: allow both." / "Continue": 1 tap, then Android's notification dialog
  and the battery dialog: 2 taps.
- "Setting up your mailbox…", then "Your address": a QR code, the address
  as 32 hex digits in groups of four, "Show this to people who should write
  to you.", "Share", "Write a message", "Your name for others" with the hint
  "optional", "Save a backup of your address", "Done". 1 tap.
- **Total: 5 taps, one choice (defaulted), no typing.** This is the best
  part of the path and better than both references.
- Default that could decide instead: the name. It is optional and empty by
  default (`WelcomeActivity.java:491-496`), and an empty name means friends
  see `<0c749148…>` (`loquax/src/names.rs:241`).
- The QR on this screen is the bare hex address
  (`loquax/src/home/qr.rs:47` `address_modules`, drawn at
  `WelcomeActivity.java:454`). The scanner that adds contacts accepts only
  `lxma://` and `lqxh1` (`ProfileActivity.java:23` `SCAN_PREFIXES`,
  `ScanActivity.java:257` `wanted`), so a friend scanning the first-start QR
  gets nothing at all: the code is silently ignored. "Share" here sends the
  bare hex too (`WelcomeActivity.java:475`), while the profile shares the
  `lxma://` code (`ProfileActivity.java:230`).

### 2.3 First start (mailbox on a computer)

Phone: "On a computer at home", "Continue" (2 taps), then "Set up your
mailbox on the computer" / "Install Loquax on the computer and run `loquax
home admit --server` there. It shows a code. Scan it here, or paste it."
(strings 175-176), "Scan the code", "Paste a code", "Use this phone until
then".

Computer: no wizard, no page. The user installs the `.deb` (which does not
pull in `lnsd`: `loquax/Cargo.toml:42` depends on `adduser` only), enables
the service with root (`loquax mailbox enable`), and runs `loquax mailbox
admit --server host:port`, typing the computer's own LAN address. Its
output (`target/1259/tui/admit.out`) is the code, a terminal QR, and "On a
phone: scan it with a QR reader and share it to Loquax, or paste it into
Loquax's join screen." (`loquax/src/main.rs:1730-1732`), though the app has
its own scanner. On schneckenschreck, whose lab marker holds the shipped
rendezvous list back, the line was the other one: "paste it, with this
node's address; --server puts that in the code."

Phone again: "Scan the code", Android's camera dialog, the scan (3 taps);
the code carries the address, so the join starts by itself. "Paste a code"
instead opens the join form (`activity_main.xml`): "The code your mailbox
shows, from `loquax home admit` on the computer (with --server it brings
the address along)", the hint `lqxh1…`, "Your mailbox's address: host:port
of its lnsd TCP server", the hint `192.168.1.20:4242`, "Join", and above it
the status line "Loquax <version> / This phone holds no mailbox and reads
none." (`loquax-android/src/lib.rs:232`). A failed join says, for example,
"Your mailbox did not answer through 192.168.1.20:4242. Check the address,
that its lnsd runs a TCP server there, and that this phone can reach it."
(`loquax-android/src/session.rs:2470`).

- Taps on the phone: 5 with the scanner; on the computer: four commands,
  one with root, one with a typed address.
- Reticulum words: lnsd, TCP server, host:port (and on the computer:
  propagation node, rendezvous in the help texts).
- After the join the list opens directly; the "Your address" screen of
  2.2 never shows, so this user never sees their own QR unless they find
  the avatar. The background questions come later, on the list
  (`Asks.java`), with the buttons "Leave it out" and "Not now".
- The plan this departs from: `docs/drafts/loquax-onboarding-2026-10.md`
  section 2, screen 3b ("open leviculum.network/mailbox and follow the
  steps") and section 6, "The wizard" (`loquax mailbox setup`). Neither
  exists yet.

### 2.4 Add a contact

By QR: the avatar at the top left (1), scroll past the QR and the share
buttons to "Scan a contact's code" (1, `ProfileActivity.java:255`), the
camera dialog (1, once), the scan, then the dialog "Add contact" / "What do
you call this contact?" with a name field, "Add contact" (1, typing). Then
the chat opens. **4 taps and a typed name**, and the scanner is behind our
own avatar, not where a new chat starts. It works only for an `lxma://`
code (a profile QR, Columba's or Sideband's), not for the first-start QR
of 2.2.

By code: the pencil (1) opens "New conversation" / "The LXMF address to
write to: 32 hex digits, as the other side's app shows it." (strings 51),
the hint "Address of a new conversation", "Paste", "Open". Pasting an
`lxma://` code adds the contact as the scanner does; pasting hex opens a
conversation with no name. There is no scan button on this screen. An
`lxma://` code received in another messenger is not a link: the manifest
has filters for `text/plain` shares and `imto://lxmf` only
(`AndroidManifest.xml:67`, `:76`), and a shared text is read only for a
mailbox code (`MainActivity.java:133` `homeCode`).

In the chat afterwards: "Not in your contacts. Add?" with "Add" and "Not
now" (strings 89-91). It means the Android address book
(`loquax-android/src/session.rs:1898` `ask_link`), but the user has just
added the contact.

- Reticulum words: LXMF, address as hex.

### 2.5 Send a text

The chat: "Message", the send button (its spoken name "Send", "Hold to
record a voice message" while empty). Type, 1 tap. The mark goes through
"Sending", "Waiting in the mailbox", "Delivered" (spoken names,
`strings_scene.xml` 47-52). From the list: the row (1). No choices.
Reticulum words: none. Good.

### 2.6 Receive a text and see it read

A notification per conversation, "New messages" channel; a tap opens the
chat (1). Read receipts are on by default (`loquax/src/shell.rs:1245`);
ours show as "Read at …" on the mark. The persistent notification reads
"Connected to 192.168.1.20:4242" or "Not connected: <the library's error>"
(strings 21-24, `LoquaxService.java:262`), an address and raw text on a
line the user sees every day. The chat's menu offers "Refresh"
(`ChatActivity.java:530`), and its answers "%1$s new message(s).", "Pull
failed: %1$s" (strings 15-17).

### 2.7 Send a photo

Paperclip "Attach" (1), "Photo" (1), the system picker (1, the photo): it
goes at once (`ChatActivity.java:680-684`), no camera, no preview, no
caption. The size is decided by a good default, "Phone (recommended)",
with the choice in Settings, Chats. 3 taps.

### 2.8 Open the map and find a friend's position

The "Map" tab (1); "No friends on the map yet" with "Share my position"
when nobody shared; "Friends" (1), "Find a friend", a name (1), the card
with "Chat", "Navigate", "Share mine". 3 taps. From a chat row with a fresh
position, its pin opens the map on that friend (1). Location permission is
asked with a reason ("To show where you are on the map. Loquax sends your
position only when you share it."). Sharing our own position sends one
fix (`MapPane.java:299-308`, "Share your position with", a list of chats);
there is no live sharing for a time, so the friend's pin ages. Reticulum
words: none.

### 2.9 Set up a mailbox from a join code

See 2.3: the phone half is a scan, as Lew's rule wants; the computer half
is the command line. "Use another mailbox" on the profile
(`ProfileActivity.java:282`) opens the join form of 2.3 with its host:port
field.

### 2.10 Restore on a second device

Two meanings, two answers:

- **A replacement phone** (mailbox on the phone): "I already have a Loquax
  address", "Your address from a backup", "Open the backup file", "Scan the
  paper backup", a field with the hint `lqxb1…`, "Passphrase", "Restore",
  then 2.2 from the background screen on. About 6 taps and a passphrase.
  The backup must have been made before (Settings, Backup, a passphrase
  twice, a file or a paper).
- **A second device beside the first** (mailbox on the phone): there is
  none. The phone's mailbox admits only its own screens
  (`loquax-android/src/mailbox.rs:103` `admission`); no screen shows a code
  for another device, and "Use another mailbox" wants a code only a
  computer prints. Restoring the same backup onto a second phone while the
  first runs gives two mailboxes with one address; I did not check what
  happens then, and nothing on screen warns.
- With the mailbox on a computer, a second device is one more `loquax
  mailbox admit` on the computer and a scan.

## 3. The terminal walk

Driven, frames in `target/1259/tui/`.

- **No daemon** (`nodaemon.out`): exit 1 and "could not join the Reticulum
  shared instance named 'walk1259none'. Loquax talks to a daemon that is
  already running; it does not start a Reticulum stack of its own. Start
  one with `lnsd` (or Python's `rnsd`), or name another instance with
  --instance / --config. The stack said: I/O error: no local Leviculum
  daemon reachable over IPC socket ..." (`loquax/src/engine/shell.rs:332`).
  The `.deb` does not depend on `lnsd` (2.3), so this is what an
  installation of the messenger alone shows.
- **First start with a daemon** (`first.frames`, frame `first_start`): the
  title "Loquax 0c749148" (our address), "No conversations: start with
  --to <address>." (`loquax/src/ui.rs:4040`), the header "messages" and "no
  propagation node" (`loquax/src/ui.rs:4154`), "nothing earlier on this
  device", the footer "Enter open │ i write │ Tab pane │ : commands │ ?
  help │ q quit". No name is asked, no address or QR shown.
- **Adding a contact** (`second.frames`): the palette knows no "new
  conversation", "add contact" or "my code"; typing `new` or `add` finds
  nothing, `name` finds only `rename`. The only way to write to someone new
  is to quit and start again with `--to` and 32 hex digits. The GUI has
  both intents (`Intent::NewConversation`, `Intent::AddContact`), the TUI
  does not wire them.
- **Send, receive, read** (pinned frames): the peer is `<61ddb79a…>` until
  it announces, then `them ·61dd`; the header carries "unknown" (the trust
  word, `loquax/src/names.rs:260`) and "no propagation node" in every
  conversation; marks `◷`, `✓✓`. A failed send toasts "not sent: direct: no
  route after 30.0s; propagated: no propagation node". The detail view
  (`v`) lists the peer's hex, the message id, "proof received (direct)":
  fine for a detail view, off the main path.
- **Own address**: `loquax address` prints the hex; there is no `lxma://`
  code and no QR for a contact in the terminal.
- **Mailbox and restore**: command line only (`loquax mailbox join CODE`,
  `loquax import`), as designed for the terminal; not counted against the
  90 % rule beyond the findings on the phone side.

## 4. Comparison with WhatsApp and Signal

From public knowledge of current versions; marked where unsure.

| Step | Loquax (Android) | WhatsApp | Signal |
|---|---|---|---|
| Install | no APK published | store, 1 tap | store, 1 tap |
| First start | 5 taps, 1 choice (defaulted), no typing | about 6 taps, phone number and SMS code typed (the code is often read by itself), name typed | about 6 taps, phone number, SMS code, PIN twice (unsure whether the PIN is still asked at registration in every version), name |
| Add a contact | 4 taps and a name by QR, behind the avatar; or 32 hex digits | 0: the phone book is matched; QR or link as an option (about 3 taps) | 0 by phone book; username or QR from the pencil, about 3 taps |
| Send a text | 1 tap from the chat | same | same |
| Receive, see it read | notification, ticks, receipts on by default | same, blue ticks on by default | same, receipts on by default |
| Send a photo | 3 taps, no camera, no preview, no caption | 2-3 taps, camera in the compose bar, preview and caption | same as WhatsApp |
| Find a friend's position | Map tab, 3 taps; one-shot sharing only | no map; live location for 15 min, 1 h or 8 h, opened from the chat | static location only, no live sharing (fairly sure) |
| Set up a mailbox | phone: scan (5 taps); computer: four commands, one with root | nothing to set up (the company's server) | same |
| Second device | none with the mailbox on the phone | Linked devices, scan a QR on the new device, about 3 taps | same, about 3 taps |
| Restore a lost phone | backup file or paper and passphrase, if made before | backup from Google Drive or iCloud, offered after the number check | device transfer by QR from the old phone, or a backup (unsure about the current backup flow) |

What Loquax cannot copy is the phone book match: there is no central
directory. The nearest thing is an invite link that opens the app, which
the comparison makes the most valuable single fix after the QR mismatch.

## 5. Findings

Severity: **B** blocks the 90 % (a newcomer cannot complete the step
alone), **S** slows (completes with effort, a guess or a Reticulum word),
**C** cosmetic.

| # | Sev | Where | Finding | Proposed fix |
|---|---|---|---|---|
| 1 | B | `loquax/src/home/qr.rs:47`, `WelcomeActivity.java:454`, `ProfileActivity.java:23`, `ScanActivity.java:257` | The first-start "Your address" QR is the bare hex address; the contact scanner accepts only `lxma://` and `lqxh1`, so a friend scanning it gets nothing, silently. | Show the profile's `lxma://` QR on the first-start screen; one contact code everywhere. |
| 2 | B | `site/pages/download.md` | No APK is published; the first user cannot install the app. | Publish the signed APK (and later F-Droid) on the download page. |
| 3 | B | strings.xml:51, `activity_new.xml:42`, `NewConversationActivity.java` | The pencil asks for "The LXMF address to write to: 32 hex digits" and has no scanner. | Pencil screen: "Scan a code" first, then "Paste a code"; the hex field under Advanced or gone. |
| 4 | B | strings.xml:6, :8, :176; `loquax/src/main.rs:150-167` | The computer mailbox is the command line: install, `mailbox enable` with root, `admit --server host:port` typed by hand; no wizard, no page, contrary to the onboarding draft section 6. | Build `loquax mailbox setup` (the draft's five steps) and the leviculum.network/mailbox page; the phone screen names the page, not a command. |
| 5 | B | `loquax/Cargo.toml:42`, `loquax/src/engine/shell.rs:332` | The `loquax` package does not depend on `lnsd`; the messenger alone fails with four lines about a "Reticulum shared instance", IPC sockets and rnsd. | Make the package pull in and enable `lnsd`; the error in one sentence with the one command. |
| 6 | B | `loquax/src/ui.rs:4040`, the TUI palette (`target/1259/tui/second.frames`) | The terminal has no way to start a conversation or add a contact from inside: "No conversations: start with --to <address>.", and the palette has no such command. | Wire `Intent::NewConversation` and `Intent::AddContact` into the palette; the empty text names the command. |
| 7 | B | `loquax-android/src/mailbox.rs:103`, `ProfileActivity.java:282` | With the mailbox on the phone there is no second device: nothing shows a code for another device, and "Use another mailbox" wants a code only a computer prints. | "Link a device" on the profile: an `lqxh1` QR from the phone's mailbox, scanned by the new device. |
| 8 | B | `loquax-android/src/session.rs:2470`, `:213`, `:233` | Join errors speak daemon: "Check the address, that its lnsd runs a TCP server there", "Type your mailbox's address, host:port of its lnsd TCP server.", "Paste the code `loquax home admit` printed". | One plain sentence each ("The mailbox did not answer. Is the computer on?"), retry button; details in a log. |
| 9 | S | `WelcomeActivity.java:491-496`, `loquax/src/names.rs:241` | The own name is optional and empty by default, so friends see `<0c749148…>`. | Ask the name as the one field of the address screen, prefilled from the phone owner's profile. |
| 10 | S | `WelcomeActivity.java:475` vs `ProfileActivity.java:230` | First-start "Share" sends the bare hex, the profile shares the `lxma://` code; the hex gives the receiver a conversation with no name. | Share the `lxma://` code (or an invite link, #12) everywhere. |
| 11 | S | `ProfileActivity.java:255` | The contact scanner is behind our own avatar, below the QR and three share rows. | Put it on the pencil screen (#3); keep the profile entry. |
| 12 | S | `AndroidManifest.xml:67`, `:76`, `MainActivity.java:133` | An `lxma://` code sent through another messenger is no link and a share of it to Loquax is ignored (only mailbox codes are read). | An invite link (https or `lxma://` intent filter) that opens "Add contact"; read `lxma://` in shared text. |
| 13 | S | `MainActivity.java` `started` | After joining a computer mailbox the "Your address" screen never shows; the user never sees their QR. | Show the address screen after every first join. |
| 14 | S | strings.xml:280 | With a computer mailbox the name cannot be set on the phone: "Your name is set where your mailbox runs." | Set the name from the phone through the mailbox. |
| 15 | S | strings.xml:89, `loquax-android/src/session.rs:1898` | "Not in your contacts. Add?" shows right after adding a contact by QR; it means the Android address book. | Word it "Save to your phone's contacts?" and do not show it in the first chat after an add. |
| 16 | S | `ChatActivity.java:680-684` | A photo goes on pick: no camera, no preview, no caption. | Camera entry in the attach menu; a preview with a caption field before send. |
| 17 | S | `MapPane.java:299-308`, `Locator.java` | Position sharing is one fix only; no live sharing for a time, so a friend's pin goes stale. | "Share live for 15 min / 1 h / 8 h", as WhatsApp. |
| 18 | S | `ConversationsActivity.java:60` | Help opens the developer documentation (`https://leviculum.network/docs/`). | A user help page (first steps, adding contacts, the mailbox), in the app's languages. |
| 19 | S | strings.xml:21-24, `LoquaxService.java:262` | The always-on notification says "Connected to 192.168.1.20:4242" or "Not connected: <raw error>". | "Connected to your mailbox" / "Not connected, trying again"; the address in Settings. |
| 20 | S | `loquax-android/src/lib.rs:232-243` | The join form's status line: "Loquax <version> / This phone holds no mailbox and reads none.", or the mailbox's hex and "last entry pulled". | Drop the status line from the join form; the version stays in the profile's Advanced. |
| 21 | S | `loquax/src/ui.rs:4154`, `loquax/src/names.rs:260` | Terminal: every chat header says "no propagation node" and "unknown"; peers are `<61ddb79a…>`, `them ·61dd`. | Show the relay state only when a send needs it; no trust word for the default state. |
| 22 | S | the TUI send path (toast text, `loquax/src/ui.rs:5808` pins it) | Terminal: a failed send toasts "not sent: direct: no route after 30.0s; propagated: no propagation node". | "Not sent yet; Loquax tries again", the legs in the detail view. |
| 23 | S | `loquax/src/main.rs` `run_address` | Terminal: our address only as hex from `loquax address`; no contact code, no QR. | A palette command "my code" with the `lxma://` code and its terminal QR (`qr::lines` exists). |
| 24 | S | strings.xml:191-196 | Restore wants a backup made before and a passphrase; no transfer from the old phone; restoring onto a second phone while the first runs is not guarded (unchecked what happens). | Transfer by QR from the old phone (onboarding draft section 4); warn when the address is live elsewhere. |
| 25 | C | strings.xml:6, :176; `loquax-android/src/session.rs:213` | The app names the old spelling `loquax home admit`; the command is `loquax mailbox admit`. | Name the page (#4); until then `loquax mailbox admit`. |
| 26 | C | `loquax/src/main.rs:1730-1732` | `mailbox admit` says "scan it with a QR reader and share it to Loquax"; the app has its own scanner. | "In Loquax: On a computer at home, Scan the code." |
| 27 | C | strings.xml:28-29 | The battery question's buttons "Leave it out" / "Not now". | "Allow" / "Not now". |
| 28 | C | `ChatActivity.java:530`, strings.xml:15-17 | "Refresh" in the chat menu, "Pull failed", "new message(s)". | Drop Refresh from the menu; "Checking for messages", plurals. |
| 29 | C | strings.xml:128, :133, :138, :142-143 | Contact info: "None; the name it announces", "The address is a hash of the identity's public key", "Network", "Path: held by your mailbox, which routes for this phone". | Move Identity and Network under an Advanced fold; "the name they chose". |
| 30 | C | strings.xml:214-215, :241, :291-293 | Advanced says "Propagation Node", "Identity hash", "Home node", "Rendezvous"; the onboarding draft section 1 asked "relay" in the UI. | "Relay" in Advanced too; the rest stays under Advanced. |
| 31 | C | `site/pages/download.md:26` | "`loquax tui` for people", the old name; plain `loquax` is the TUI. | "`loquax` for people". |

Counts: **B 8, S 16, C 7** (31 findings).

Done since: 1263 takes #1, #3, #6 and #8, and with #1 also #10 (the
first start's Share sends the contact code). 1264 takes #7: "Link a device"
on the profile of a phone that holds the mailbox, "Join an existing
mailbox" at the first start; over the rendezvous today, not yet over the
same WiFi (`docs/src/concepts/loquax-reachability.md`).
1278 takes #9 (the name is the address page's one field, above the QR,
prefilled from the owner's card where the app may read it), #12 (an
`lxma://` link, or a shared text carrying one, opens "Add contact"),
#13 (the address page once after a join), #15 ("Save to your phone's
contacts?", not shown in the chat of a contact just added), #18 (Help
opens /help, /help-de), #19 ("Connected to your mailbox" / "Not
connected, trying again") and #20 (no status line on the join form). #14
(the name set from the phone through a computer mailbox) went back to the
pool: it needs a new request path to the mailbox and a rename of the
running mailbox's announce.
1279 takes #22 (the toast "Not sent. Its details say why.", the legs in
the detail view; not "Loquax tries again", since the terminal does not
retry a send its runner gave up), #23 (the palette's `my code` with the
`lxma://` code and its QR; `loquax address` prints the code under the hex
at a terminal) and #21 in part (no trust word for the default state; no
relay words without a relay until a send needs one, then `no relay`). The
rest of #21, a nameless peer's form `<61ddb79a…>`, went back to the pool:
`names::address_form` is shared with the phone, whose device walk matches
that form as its raw-hash check, and "the way the list does" names no other
form the list draws today.
1281 takes #24: "Move to a new phone" on the old phone's profile, "Move
from my old phone" at the new phone's first start; the identity, its
ratchets, the contacts' keys, the name and the whole archive travel over
the rendezvous, the old phone stops and says so until "Use this phone
again"; a restore of an address that answers elsewhere warns first
(`docs/src/concepts/loquax-reachability.md`, "Moving to a new phone").
1283 takes #27 (the battery question's "Allow" / "Not now"), #28 (no
Refresh in the chat menu; "Checking for messages failed, trying again",
the library's words to the log; "1 new message" / "2 new messages" as
plurals in English and German), #29 (the contact screen's Identity and
Network under a closed Advanced; "None; the name they chose"; no hash in
the comparing line) and #30 ("Relay" wherever Advanced named the
Propagation Node, in English and German, "Relais").

## 6. The three worst

1. **#1, the QR that scans to nothing.** The one QR a new user is shown
   at the end of the first start is the one the contact scanner ignores.
   Two new users following the screens cannot add each other by QR, and
   nothing tells them why.
2. **#2 with #5, nothing to install.** No APK on the download page, and
   the terminal package fails without a daemon it does not pull in. The
   90 % never reach the first screen.
3. **#4, the computer mailbox.** Lew's rule says a mailbox is a QR scan;
   the phone half is, the computer half is four commands, root and a typed
   LAN address, and the screen tells the user a command, not a page.
