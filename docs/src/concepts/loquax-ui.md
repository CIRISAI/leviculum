# Loquax: the user interface

Part of the [Loquax design record](loquax.md). Two decisions live here: how
keys are dispatched, and what an honest delivery display looks like. They
share one discipline — a single table read by everything that renders from
it — and that discipline is the reason both are on one page.

## 1. Decision: the input model

This is the hard one, and the browser's answer does not transfer. In a
browser, plain letters are free because there is nothing to type into most
of the time. In a chat client, the single most common action is typing
prose.

### Options

**A. Modal, vi-style.** Normal mode for commands, insert mode for
composing, `i` to enter, `Esc` to leave.
*For*: every key stays available for commands; scales to any number of
bindings; vi users are instantly at home; it is the only option where
`j`/`k` mean what they mean in `lnomad`.
*Against*: it is the single biggest complaint non-vi users have about
terminal software. A user who types a message, presses `Esc` out of habit,
and then types "quit" has issued four commands. Mode errors in a messenger
are worse than in an editor because the consequence can be sending
something.

**B. Always-insert, everything Ctrl-modified.** NomadNet's answer
(NomadNet's `Conversations.py`, lines 68-80). Focus starts in the compose
box, plain typing always composes, every command is `Ctrl-`something.
*For*: zero mode errors; a user who has never read the manual can still
type and send. *Against*: the Ctrl namespace is about twenty-six slots wide
and readline already claims a dozen of them. NomadNet ran out and started
overloading by focus, so `Ctrl-X` means two different things depending on
invisible state.

**C. Focus-follows-pane.** The conversation list and the message list are
command panes with `lnomad`-style bindings; the compose box is a text pane
where plain keys type. `Tab` moves focus.
*For*: no modes to learn, because the mode is visible as which pane has the
cursor; single-letter commands survive in the panes where they make sense;
it matches how tmux, mutt and every mail client behave.
*Against*: the same key does different things in different panes, which is
a mode by another name, just one you can see. And `Tab` becomes precious.

**D. Prefix key.** Everything types; a prefix (`Ctrl-Space`, `Ctrl-A`, or
`,`) introduces a command.
*For*: unlimited namespace, no modes, tmux and screen users know it.
*Against*: two keystrokes for everything, including scrolling, which is the
operation you do most.

### Decision (2026-08-08)

**C, with a command palette (D) layered on it, and no modes.**

Concretely:

- Three panes: conversations (left), messages (right, upper), compose
  (right, lower). `Tab` and `Shift-Tab` cycle; a click focuses.
- **In the conversation and message panes**, `lnomad`'s keymap applies
  unchanged: the scroll rows verbatim
  (`SCROLL_ROWS`, `leviculum-ui/src/keymap.rs:666`), so `j`/`k`, `Ctrl-n`/`Ctrl-p`, arrows,
  `Ctrl-f`/`Ctrl-b`, `Ctrl-v`/`Alt-v`, `Ctrl-d`/`Ctrl-u`, `g`/`G`,
  `Home`/`End` and the wheel all work, in all four idioms, for free.
- **In the compose pane**, plain keys type. Only `Ctrl-` and `Alt-` chords
  are commands, plus the scroll table's `Ctrl` and `Alt` rows, which do not
  collide with readline because they are page motions and readline's are
  line motions. **`Enter` sends**: LXMF is email on the wire but chat in
  every deployed client — Sideband, NomadNet, MeshChat and columba all
  render conversations, nobody uses the title field — so users arrive with
  chat expectations, and Enter-to-send is the universal chat convention.
  Newline is `Alt-Enter` everywhere and additionally `Shift-Enter` where
  the terminal speaks the keyboard enhancement protocol (legacy terminals
  cannot distinguish `Shift-Enter` or `Ctrl-Enter` from `Enter` — same byte
  — which is also why `Ctrl-Enter`-to-send was dropped; crossterm's
  `PushKeyboardEnhancementFlags` enables the modern protocol where
  available). `Enter` on an empty buffer does nothing. The keymap table
  carries a `send_on_enter` switch; off flips to email style (`Enter`
  newline, `Ctrl-D` send) for long-form writers. `Ctrl-D` on an empty
  buffer must not send and must not quit.
- **A command palette** on `:` in a command pane and `Ctrl-P` in the
  compose pane, with fuzzy matching over named commands. This is what makes
  the scheme learnable: every command has a name, the palette lists them
  all, and each entry shows its key if it has one. NomadNet's static
  shortcut bar is the failure mode to avoid.
- **`f` hint mode from `lnomad`** (`hints`,
  `lnomad/src/app/model.rs:1039-1076`), extended to conversations. Its best
  property is that it matches either the hint label *or* a substring of the
  target's text (`hint_matches`, `lnomad/src/app/model.rs:1066-1076`), so `f`
  then typing part of a contact's name jumps to that conversation. That is
  a better contact switcher than anything in either prior-art program.

**What must not be copied**: `lnomad` quits on `Ctrl-C` from any mode,
before the mode dispatch (the quit row's `Ctrl-c`, in every pane,
`lnomad/src/app/keys.rs:330`). In a messenger
that is a half-written message thrown away by a reflex. `Ctrl-C` in the
compose pane clears the buffer to a recoverable draft; quitting needs
`Ctrl-Q` or the palette.

**The keymap must be one table.** `lnomad` applied single-source-of-truth
discipline to scrolling and nowhere else, and its help overlay's other
groups were hand-typed strings that could drift, until its slice 1b made
them rows of one table (`lnomad/src/app/keys.rs:150`). Every binding here is one table read by
the key handler, the help overlay, the palette and the footer hints. That
table is also the natural place to hang user configuration later, which
`lnomad` has none of.

**What would change this**: if the compose box turns out to be where users
spend nearly all their time, B (always-insert, Ctrl for everything) is
simpler and has no invisible state at all, at the cost of losing
single-letter commands entirely. The way to find out is to build C and
count how often focus is in the compose pane.

**The quiet keyboard, with its one exception** (decided 2026-08-08). The
`lnomad` rule that single letters are reserved for cheap local actions
(`lnomad/src/app/keys.rs:189-191`) generalises here to: **no single keystroke
may put bytes on the air — except `Enter` in the compose pane.** The
exception is principled, not a leak: the deliberate act was typing the
message into a deliberately focused pane, and `Enter` completes that act.
Everywhere else the rule is absolute: no key in a command pane transmits,
syncing needs a chord or the palette, announcing needs a chord or the
palette. Everything free is free.

### Built 2026-10-25, slice one

`loquax tui` (the `tui` cargo feature, which the packages ship; 994, #232;
since 1023 plain `loquax` with no subcommand, `tui` staying as its alias) is the
first frontend on the shared widgets. The screen is a model in the
messenger core, `loquax/src/ui.rs`, `no_std` like the rest of it (`just
loquax-core-no-std` builds it): `UiModel` with the conversations, the
selection, the compose field, the focus, the toasts and the palette;
`update(model, UiEvent) -> Vec<UiEffect>`; and `view`, which hands the
renderer each pane's lines. The terminal half is the binary's
`loquax/src/tui.rs`. A send is the CLI's `--via auto` runner
(`send::run_send_auto`, which `loquax send` runs too) and a fetch is
`loquax fetch`'s, each on a tap of the one engine
(`loquax/src/fanout.rs`) while the screen reads the same events. Sends run
one at a time; a fetch runs alongside them, so a send never waits for a
propagation node that does not answer (order 1072).

In:

- Three panes, focus follows the pane, `Tab`/`Shift-Tab` cycle.
- Conversations and messages: `j`/`k`/arrows move, `g`/`G` and
  `Home`/`End` jump, `Enter` opens the conversation or goes to compose,
  `?` help, `q` quits, `:` opens the palette.
- Compose: plain keys type through the `tui-input` table lnomad's fields
  use; `Enter` sends, and on an empty line does nothing; `Alt-Enter`
  breaks the line (since 1002; crossterm reads the ESC CR a terminal sends
  for it as `Enter` with `ALT`, and `Shift-Enter` arrives only under the
  keyboard enhancement protocol, which `loquax` does not switch on yet,
  so it is bound but legacy terminals cannot send it); the pane grows to
  five rows and then scrolls; `Esc` goes back
  to the messages; `Ctrl-C` clears the line into the kill buffer
  (`Ctrl-Y` brings it back) and does not quit.
- Everywhere: `Ctrl-P` opens the palette, `Ctrl-Q` quits.
- The palette fuzzy-matches the names in `leviculum_ui::commands`
  (`send`, `fetch`, `quit`, `help`, `focus conversations|messages|compose`),
  the one table the desktop GUI will share.
- Delivery glyphs from the table in section 2, mapped from the router's
  states as listed there; received messages, deduplicated by id, with an
  unread count on a conversation that is not open. A body with line
  breaks is drawn as its lines, wrapped to the pane at a space, the mark
  on the first row only and the rest set in by four.
- `loquax --script <file>` runs a key script against the real engine and
  writes the frames it asks for to a file; `loquax/tests/tui_snapshot.rs`
  pins a session (open, type two lines, send, `◷` to `✓✓`, a two-line
  reply) that way; a script's `newline` step is `Alt-Enter`. A script
  ticks only when it says so: `tick [n]`, or `sleep <ms>` (up to 60000),
  which reads the engine every 20 ms as the idle terminal does and applies
  one tick per 250 ms of its own length, however long the host took.
  `until leg <direct|propagated> [secs]` waits for the runner to report
  that leg for the newest sent message, so `until leg direct 5` then
  `sleep 2500` draws `me (direct 2 s):` on every run (1014, the second
  pinned session in `tui_snapshot.rs`).

Not in, and open: history (Loquax keeps no message store yet, so the
screen holds this session's messages and says so), trust states, the
mouse, QR codes, the per-message detail view and its ledger, the
`send_on_enter` switch and the keyboard enhancement protocol for
`Shift-Enter`, `f` hint mode, lnomad's full
`SCROLL_KEYS` table (only the motions above), and display names (a
conversation is its address; `--to` lists one from the start). The keymap
is still a `match` in `ui.rs` beside the help text, not yet the one table
the decision asks for.

### Built 2026-10-26, slice two: the keymap

1030 (#232) closed four items of that list: the keymap is one table,
`loquax/src/keys.rs` beside the shared scroll rows of
`leviculum-ui/src/keymap.rs`, which `lnomad` reads too, and the key
handler, the help overlay, the footer, the palette's keys and the compose
pane's hint all derive from it; lnomad's full scroll table applies in the
two command panes; the `send_on_enter` switch (config key and palette
toggle); and the keyboard enhancement protocol, pushed where the terminal
speaks it, for `Shift-Enter` and `Ctrl-Enter`. With it the four decided
changes of [keys, mouse and layout](loquax-keys-and-layout.md), section 3:
`Enter` on a message shows its details, compose is `i`, `a`, `r`, `Ctrl-p`
and `Ctrl-n` are motions and the palette is `:` or `Alt-x`, and `Ctrl-c`
quits nowhere.

### Built 2026-10-26, the chat layout

1033 (#232) is slice 2 of [keys, mouse and layout](loquax-keys-and-layout.md),
section 7: the list as two rows a conversation, most recent first, with
the last line's preview, the time and the unread badge; the conversation
as blocks, ours right and theirs left, with a date line between days, the
`new` rule where the unread begins and the narrow fallback below 40 cells;
the header with the trust state and the propagation node's state; and the detail view
behind `Enter` on a message, with the ledger of what this session saw
happen to it, in place of the toast of 1030. The model hears the screen's
size and the clock, so a page is the pane's height and a reply arriving
while the reader is scrolled up leaves the view where it is. Its section
5, "Built", lists where the screen differs from the mockups.

### Built 2026-10-26, the colours

1042 (#232) is the colour half of the same slice, section 5, "Colour",
as Lew decided it on 2026-10-26: one token table in `leviculum-ui` with a
value for each of four tiers (monochrome, 16, 256, truecolor) and two
themes, our blocks tinted blue and theirs grey, the date as a pill, the
`new` rule in an accent, the delivery tones per theme and tier. The tier
comes from a ladder over `--no-color`, `NO_COLOR`, `--color`, `COLORTERM`
and `TERM`, the theme from `--theme`, the config file's `theme` key and
the terminal's background, and the palette's `theme` swaps light and dark
for the session. `lnomad` reads the same table for its toast, selection,
muted text and chrome bars. Monochrome is the look the frames pin; the
colour is pinned as per-cell style maps beside them. "Colour", "Built",
lists where the build differs from the text.

### Built 2026-10-26, the mouse

1036 (#232) is slice 3 of [keys, mouse and layout](loquax-keys-and-layout.md),
section 7, and the proposal "The mouse as a first-class citizen" below:
a click focuses a pane and lands on what is drawn there (a conversation,
its badge, a message, its delivery mark, a link, the compose box at a
cell, a footer item), the wheel scrolls the pane under the pointer, and a
drag over the conversation selects text across lines and messages and
copies it by OSC 52 on release. The two clicks that transmit are the
words `send` on the compose box's border and the propagation node's state, the same
commands as `Enter` and `F5`. `--no-mouse` and the palette's `mouse` give
the terminal its own selection back. The OSC 52 sequence and the rule a
link is followed by are `lnomad`'s, moved to `leviculum-ui` so both
programs read one copy. Its section 5, "What the mouse does", lists where
the screen differs from the table.

### Built 2026-10-26, the search

1041 (#232) is slice 4 of [keys, mouse and layout](loquax-keys-and-layout.md),
section 7: `/` or `Ctrl-s` turns the list pane into the search, its field
on top and the results under it as the query is typed, conversations by
name and then this session's messages by text, each most recent first,
the first match marked between `«` and `»`. A result opens with `Enter` or
a click, a message's conversation at that message with the match marked in
its block; `n`/`N` and `Ctrl-s`/`Ctrl-r` step the matches in place, and
`Esc` brings the list back. The footer while it is open is the search's
own, from the same table. Its section 5, "Built 2026-10-26, the search",
lists where the screen differs from the mockup.

### Built 2026-10-26, names

1047 (#232) is slice 5 of [keys, mouse and layout](loquax-keys-and-layout.md),
section 7: a peer is the name it announces, or the reader's petname from
`${LNMSG_HOME}/contacts`, with the address's first four hex digits after
it (`Alice ·4f2a`), in the list, the header, the compose box, the detail
view, the toasts and the search. Names are sanitised for display, two that
look alike are both marked `≈`, a changed name is said once in the
conversation, and the header says the trust state in words: `known`,
`unknown` or `blocked`. Its section 5, "Built 2026-10-26, names", lists
where it differs from the decision in [the mailbox page](loquax-mailbox.md).

**Changed 2026-11-08 (1452): a name is a name.** Lew, on UI walk 1,
finding 3: four hex digits after every name are noise nobody can remember.
On every target (the window, the terminal, the phone, the M9), in lists,
chats, bubbles, the map and the headers, a peer and a group are their name
alone (`loquax/src/names.rs`, `Names::label`): the petname, else the
address book's name, else the announced one. A peer without a name is
"Unknown" ("Unbekannt"), its avatar a `?` in the colour its address gives,
so two of them differ by colour; never hex. The own avatar takes the own
name (the names' word for us, else the profile's from the mailbox).

The short hash is a **collision marker, not an identifier**. It comes back
only on the peers whose shown names collide (`Names::collisions`: the name
the screen shows, petname, address book or announce, compared folded in
case and white space, with the Cyrillic, Greek and digit look-alikes
folded too), and two or more nameless peers with a conversation collide
as well. Where they meet, the chat list's row and the chat header of each
say it in words, "Zwei Kontakte heißen Lew" / "Two contacts are called
Lew" (`Names::same_name`, catalogue strings `same_name_two`,
`same_name_many`, `no_name_two`, `no_name_many`). The `≈` mark is gone. The
window draws the sentence as a third line under the row and small under
the header's name; the terminal and the M9, whose rows are one line of
name, draw it after the name only when it fits whole, and the header
always says it.

**A peer is verified by its address, not by its tail.** The full address
and its QR code live in the profile and the contact info, and a message's
Info sheet names a relayed writer's short hash; that is where one looks a
peer up and compares it, in full or by scanning the QR, never by four
digits beside a name. The search still finds a conversation by its
address in hex.

### Built 2026-10-27, history

1049 (#232) is slice 6 of [keys, mouse and layout](loquax-keys-and-layout.md),
section 7: conversations, their messages with their marks and ledgers, the
pins, the unread counts and the names heard outlive the process in the
SQLite store of [storage](loquax-storage.md). A send cut off by quitting is
`◷` again after the restart, with the reason in its ledger. `d` then `y`
deletes a message from the device, the palette deletes a conversation or
pins one above the list's rule, and the search reaches older messages
through the store's FTS5 index. Its section 5, "Built 2026-10-27, history",
lists where it differs from the pages.

### Built 2026-10-30, the desktop chat

1145 (#232) gives `loquax gui` the chat, after the phone's design: the
conversation list on the left (avatars with initials in the peer's LXMF
icon colours, the name, the last line, the list's time, the unread count
or our delivery mark, a search, a new conversation by address), the open
conversation on the right (a top bar with the avatar and the name,
rounded bubbles, ours right in the accent and theirs left, grouped, date
lines, reaction pills, position lines that open the map, a pill compose
bar with an emoji button and a round send button; `Enter` sends,
`Shift-Enter` breaks the line). A right click on a bubble opens the quick
reactions and reply, copy and delete. The theme follows the desktop's
light or dark setting, which iced reads, and a toggle turns it round; the
map keeps its own (1133). The window draws the shell's model through
`loquax::ui::chat`, which the phone's facade draws through too: the rows,
the bubbles, the grouping, the marks, the avatars, the tokens below.
Pinned by model tests and one golden picture of the chat pane per theme,
drawn off screen by iced's software renderer
(`loquax/tests/golden/gui-chat-*.png`).

Not yet in the window (the next slice): forward, select, info and pin on
a bubble, the full emoji picker over the emoji table with the recents
(the button offers the six quick ones), the address book's names and
photos, and the toolbar menu beyond the map.

### Built 2026-11-01, the window draws the scene

1218 (#232, slice two of the one UI contract) makes the window a renderer
of `loquax::ui::scene`, the value the phone's facade is derived from: one
function per node in `loquax/src/gui/chat.rs` (the top bar, the tabs, the
rows, the chat bar, the groups, the bubbles, the bubble's menu, the
compose bar). The window keeps no rule of its own for the grouping, the
day lines, the liveness words and rings or the list's search
(`List::found`). A press on a node is its `Intent`; a click on a row is
a hit on its `NodeId`, which `Scene::action` turns into its opening. The
window's keys go through `keys::resolve`, so the map takes the
terminal's chords (arrows and `hjkl` pan, `Tab` steps through the pins,
`Enter` opens one), and `Escape` in a stacked chat is Back.

The window shows the shared structure: the reader's top bar with the
avatar, the bar's search and new conversation and the window's theme
toggle as drawn icons, and the tabs with the unread count. From 712
logical pixels (rail, list and a chat of 300) it is Wide, the tabs a rail
at the left; below it is Compact and stacks as the phone does: the list
with the tabs at the bottom, or the open chat alone with a back in its
bar. A resize switches (`Scene::in_class`, no model needed). The words
are the catalogue's in the desktop's locale (`LC_ALL`, `LC_MESSAGES`,
`LANG`); a few of the window's own words stay English (the empty chat,
the address field, the reply line). The mailbox link's dot on the
reader's avatar is drawn once the scene knows the link (slice three).
Pinned by `gui-chat-{light,dark}.png` and
`gui-chat-compact-{chat,list}.png`.

### Built 2026-11-01, drafts

1219 (#232): a message typed and not sent stays with its conversation,
on every target, as the messengers do. The model keeps it
(`Conversation::draft`, `loquax/src/ui/history.rs`: the text, the
message a reply being written answers, and an attachment list that stays
empty until messages carry attachments). The scene's compose node holds
it (`Compose::text`, `Compose::reply`), so a chat opens with it in the
field, and a field reports every change back (`Intent::Draft`,
`UiEvent::Draft`): the phone's `EditText` mirrors the scene and keeps
nothing in Java, the window's editor is put back from the scene when
another chat opens, and the terminal's compose line is the draft of the
open conversation, put back on a switch and at start. A send or a reply
takes it away.

The store keeps it beside the conversation (`drafts`, schema 8 of
[conversation storage](loquax-storage.md)), never in the config. A
keystroke moves the write on by half a second
(`history::DRAFT_DEBOUNCE_MS`); leaving the conversation, quitting and the
phone's session ending write it at once. A conversation not open shows
it as its last line, `Draft: …` in italics, in the catalogue's words
(`draft_row`). It counts as nothing unread, raises no badge and moves
nothing in the list. Pinned by `a_draft_stays_with_its_conversation`, the
terminal's `tui_draft.frames`, the facade's
`a_draft_survives_another_chat_and_a_new_session` and the device walk's
`draft` step.

### Built 2026-11-01, the compose bar after Signal

1270 (#232, Lew 2026-11-01): the compose bar is one rounded field with
the emoji button at its left edge, and inside it at its right edge the
position button (the crosshair ⌖), the camera and the microphone;
outside it, right, one round accent button that is a plus while there is
nothing to send and Send once there is:

```text
( ☺  Message            ⌖  📷  🎤 )  (+)
( ☺  on my way          ⌖  📷     )  (➤)
```

The rule is the core's (`loquax/src/ui/compose.rs`, `Round::of`): text in
the field (blanks are none) or a staged photo make it Send, and the
microphone hides with it; a shell whose plus has no row keeps Send. The
scene's compose node carries the button set as data
(`Compose::round`, `Compose::buttons`, each a `ComposeButton` with its
node id, its action and its words; `Compose::staged`), so every target
draws the same bar from it, and what a shell cannot do is not in it
(`compose::Abilities`: camera, microphone, picker, location).

| Button | Node id | Intent | Shows |
|---|---|---|---|
| emoji | `emoji` | `Emoji` | always, at the field's left edge |
| position ⌖ | `locate` | `Locate { peer }` | where the shell reads its position |
| camera | `camera` | `Camera { peer }`, answered with `StagePhoto` | where the shell has a camera |
| microphone | `record` | `SendVoice` (held, 1222) | where it records, only while the button is the plus |
| plus | `plus` | `Attach { peer }` | outside, nothing to send and a row to offer |
| Send | `send` | `Send { peer }` | outside, text or a staged photo |

The plus opens the attachment sheet, a `Sheet` node with its rows
(`sheet_gallery` Gallery, `sheet_file` File, `sheet_position` My
position; a Contact row later). The position button opens the position
sheet straight away, unlike Signal, which buries the position in its
plus: sharing a position is one tap away. That sheet shows the own fix on
the map and its age ("Found 5 min ago"; the scene's `Whereabouts`) and
the row "Now, once" (`ShareFix`, "Send my position now" until 1273),
which sends the fix to the chat as the position report of 1209 (a state,
not a message) and closes the sheet, then since 1273 the four shares
(`StartShare`, [the map](loquax-map.md), "Sharing for a while"); without a fix it says so and offers "Turn location on", the
permission flow of 1201. Both paths lead to the same sheet.

The camera takes a photo at once and stages it in the bar with its
size; the field's hint becomes "Add a caption", and Send sends the
photo and the text as one message (1268's one bubble). The staged photo
went through the same scaling and stripping as a gallery pick
(`loquax::photo::prepare`), and lives in memory only: a photo is not a
draft. Gallery picks still go at once, as in 1220.

Per target:

- **Android** draws the bar from the facade's `Messages`/`Bar` fields
  (`round`, `buttons`, `hint`, `staged`, `sheet`, `sheet_rows`, the fix);
  `Query::Bar` is asked on every change of the field, so the plus turns
  into Send with the first letter. The camera is the platform's capture
  intent (`ACTION_IMAGE_CAPTURE` into `CaptureProvider`'s one file): the
  app builds without Gradle and AndroidX, so CameraX is not on the table,
  and the intent needs no preview code of ours. The sheets are bottom
  sheets as the map's Friends sheet is.
- **The window** draws the same buttons from the scene. It has no
  camera, no recorder, no picker and no position today, so its bar is
  the emoji and Send; a button appears there once the window can do it.
- **The terminal** keeps its palette and gains no `photo` command: the
  hosts it is built and tested on have no camera (`/dev/video*`) and no
  capture tool (`ffmpeg`, `fswebcam`).

Pinned by `compose::tests` (the plus/send rule, one message of photo and
caption, the share), `scene::tests::the_compose_bar_has_its_buttons_and_its_sheets`,
the facade's `the_compose_bar_stages_a_camera_photo_through_the_gallery_path`
and the device walk's `compose_bar`, `camera_photo` and
`share_position_from_bar` steps.

### Pixel screens: the tokens

The phone and the window draw with one table of colours and sizes,
`loquax/src/ui/chat.rs` (`LIGHT`, `DARK`, `size`). The phone's
`loquax-android/android/res/values*/colors.xml` holds the same colours
name for name, and `the_phone_draws_the_same_colours` refuses a drift.
The chrome's rows of the table (`background`, `text`, `text_secondary`,
`accent`, `bar`, `panel`, `failed_mark_on_panel`, `selected_row`,
`badge`, `badge_text`) are the suite's, `leviculum_ui::pixel`, which
lnomad's window draws in too (#475).
Every text keeps 4.5:1 on its ground, a mark or a control 3:1; the
numbers per pair are the comments in `colors.xml`.

| Token | Light | Dark | Where |
|---|---|---|---|
| `background` | `#ffffff` | `#17181b` | the screen |
| `text` | `#1b1b1d` | `#ececec` | names, the compose text |
| `text_secondary` | `#5c5f66` | `#a9abb0` | last lines, times, date lines, hints |
| `accent` | `#3a7d44` | `#81c784` | links, an unread time |
| `bar` | `#f1f2f4` | `#26272b` | the chat's top bar, the compose pill, the search |
| `bubble_ours` | `#2e6b38` | `#3b6b45` | our bubble |
| `bubble_ours_text` | `#ffffff` | `#ffffff` | its body |
| `bubble_ours_meta` | `#d6eadb` | `#dbeadd` | its time and mark |
| `bubble_theirs` | `#e9eaed` | `#303134` | theirs |
| `bubble_theirs_text` | `#1b1b1d` | `#ececec` | its body |
| `bubble_theirs_meta` | `#55585e` | `#a9abb0` | its time |
| `failed_ground`, `failed_mark` | `#ffdad6`, `#93000a` | the same | the failed mark in a bubble |
| `send`, `on_send` | `#2e6b38`, `#ffffff` | `#4f8f5b`, `#ffffff` | the send button |
| `badge`, `badge_text` | `#2e6b38`, `#ffffff` | `#3b6b45`, `#ffffff` | the unread count |
| `list_failed_ground`, `list_failed_mark` | `#93000a`, `#ffffff` | `#ffdad6`, `#93000a` | the list's failed mark |
| `dim` | `#99000000` | `#b3000000` | under the phone's long press |
| `panel` | `#ffffff` | `#2c2d31` | the reactions and actions |
| `failed_mark_on_panel` | `#ba1a1a` | `#ffb4ab` | delete on the panel |
| `reaction_ours` | `#5f9a69` | `#4f8f5b` | our reaction's circle on the panel |
| `selected_row` | `#403a7d44` | `#4081c784` | a selected message, the open conversation |

The avatar without an icon appearance takes one of eight grounds by the
address's first byte (`AVATAR_PALETTE`), white letters at 5:1 or more.

Sizes, in the phone's dp and sp, which the window takes as logical
pixels (`size`): the list's row 80 high with a 56 avatar, name 18, last
line 15, time 13, the unread pill 22 high with a corner of 11 and text
12; the top bar 64 with a 40 avatar and the name at 20; a bubble's body
17, its time 12, its corner 18 and a grouped bubble's inner corner 6, at
most 0.8 of the chat wide, 10 above a group's first and 2 above the rest;
the date line 14; a reaction pill's text 14 and corner 14; the compose
pill 48 high with a corner of 24 and text 17, the send button 48 round.
Bubbles of one sender at most 180 seconds apart on one day form a group
(`GROUP_SECS`), whose last shows its time, and one of ours whose mark
differs from the next one's keeps its mark.

The second round on the screen, the full key table (vi, Emacs, the mail
and the chat programs side by side, per pane), the mouse, the chat layout
in text mode and the slices after 1002, is
[keys, mouse and layout](loquax-keys-and-layout.md) (2026-10-26).

### Built 2026-11-06, the profile is one scene

Lew, 2026-11-06: a tap on the avatar at the top left opened "Your
profile" on the phone and did nothing in the window, though the
programs share their code and the UI is a thin layer over it. The
profile is now a scene of the core (`loquax/src/ui/profile.rs`,
`ProfileScreen`): the avatar, the name the announce carries, the address
in groups of four, the contact code's QR as the core's bitmap
(`QrNode`, encoded verbatim by `loquax::home::qr`), where the mailbox is
and how it is reached (the rung's header word of
[reachability](loquax-reachability.md), or "Fetched N of M" while a pull
runs, or unreachable), the devices of this identity, and the one action
"Share my address". The reader's avatar in the bar (`Lead::Me`) carries
`Intent::Profile`, so a renderer that draws the avatar knows what a tap
on it does:

- the phone: the avatar opens `ProfileActivity`, which tells the core
  (`Core.profileShown`, decoded to `Intent::Profile` and Back) and draws
  the scene's words (`Profile::words`, from the catalogue); the name's
  field, the share sheet, the scanner, the code sheets and Advanced stay
  the phone's own;
- the window: a click on the avatar, and the settings' "Your profile",
  open the scene's layer; "Share my address" puts the code on the
  clipboard;
- the terminal: `P` in the list or the conversation, a click on the
  list's title (the reader's own name), and the palette's `my code`
  open it as a detail view; `s` there copies the code, Esc closes it;
- the M9: the house key (provisionally `0x82`, `SPECIALS` in
  `loquax-m9/src/keyboard.rs`, 1440) leads to the settings (1561,
  `loquax-m9/src/house.rs`), which hold no profile: the M9 is a device
  of its home, so the cell below says missing.

Goldens: `loquax/tests/golden/gui-profile-light.png`,
`loquax/tests/fixtures/tui_profile.frames`,
`loquax/tests/golden/scene-profile.json` and the phone's answer
`loquax-android/tests/golden/profile-words.json`.

### Built 2026-11-08, one settings structure

Walk 1, finding 21: the phone had its settings in the ⋮ menu with nine
sections, the window a gear with a flat page and a light/dark button in
its bar, the terminal and the M9 none. Since 1450 the sections are one
list in the core, `loquax/src/ui/settings.rs` (`SECTIONS`): Profile,
Mailbox, Appearance, Chats, Notifications, Privacy, Maps, Backup,
Advanced, in that order, their titles and summaries the catalogue's.
Every target lists them from there and enters them its own way:

- the phone: the ⋮ menu opens `SettingsActivity`, whose list is the
  facade's `Query::Settings` (`session::settings_list`); each entry opens
  its page by the section's key, the pages stay Android's;
- the window: the gear opens the list, a section its page; light or dark
  is Appearance's switch, the bar has no theme button any more;
- the terminal: `S` (and the palette's `settings`) opens the list as a
  detail view, `1` to `9` a section, a row's key runs it (`r` read
  receipts, `t` light or dark, `f` the photo size and so on), `q` goes
  back a step;
- the M9: none yet, which `PLACES` says per section with what it takes
  (a menu screen in `loquax-m9` and the key that opens it).

`PLACES` in the same file says per section and target where it is drawn
(a file and a token, as the inventory's cells) or why not; its test is
the guard that every target draws every section it does not mark
missing or n/a, and that a phone, a window and a terminal draw all nine.
The inventory's `Settings` row points to it.

### Built 2026-11-10, the mailbox page

Lew, 2026-11-10, on the Fairphone (1642): Settings, Mailbox said only
"Your mailbox is on another device", and nothing told which one. The
page is now one scene in the core, `loquax/src/ui/mailbox_page.rs`,
which the phone's `SettingsActivity`, the window's Mailbox section and
the terminal's draw. For a mailbox on another device it has three
groups:

- **Which mailbox**: the name it announces, the address in full under
  "Address" with a copy (the one place the no-hash rule yields, Lew asked
  for it), the machine it runs on and its version;
- **Connection**: connected or not since when, the last contact, the
  server and port the device is on and whether that is the shielded
  access-point port 4343 or the 4242 fallback, with a sentence on what
  that means;
- **Devices of this mailbox**: each device the mailbox admitted with when
  it was last there, this one said as such, and "Show my code".

The name, machine, version and devices come from the mailbox itself:
`/about` (`loquax/src/home/about.rs`), answered to an admitted device;
the mailbox keeps a device's last request since schema 20
(`home_devices.last_seen`). A mailbox from before it stays silent on
the path, and the page says it runs an older version. The phone asks on
every connection and when the page is up and its word is older than a
minute; the desktop after a pull, every ten minutes at most.

Goldens: `gui-settings-mailbox-light.png`, `tui-settings-mailbox.txt`
and its German twin, and the phone's answers
`mailbox-page-reachable.json` and `mailbox-page-unreachable.json`, all
from `loquax/tests/support/mailbox_page.rs`.

**Walk 6 (1647, 2026-11-10, the Pixel against Lew's mailbox on
miauhaus).** What the page says now:

- It holds still. The phone asks every five seconds; an answer of the
  same shape (groups, lines, their kinds) is written into the lines on
  screen (`MailboxLines.java`), so no view goes and the scroll position
  stays; one of another shape is drawn whole when it is there and the
  position put back. Before, each ask emptied the page and it jumped to
  the top, and "Show my code" below the fold was out of reach.
- A device the mailbox has not heard since it began keeping the time
  (`last_seen` NULL after the schema 20 migration) is named without a
  time: unknown is not never.
- The version is words only, never a commit: "0.1.8", "0.1.9, newer",
  "0.1.7, older", "0.1.8, another build" where the words agree and the
  commits do not; the commits are in the phone's `HOME about` log line.
  The Loquax build stamp marks a tree with uncommitted changes
  `<hash>-dirty` (the 1642 APK was built before its own commits and
  named the one before).
- Under the address: "This is your address too: the mailbox and you are
  one identity."
- The way names the mailbox's machine from `/about` ("Connection to
  miauhaus, shielded (the phone hears only its mailbox)"), else the
  server; a port only where it is neither 4343 nor 4242.
- "Connected for" counts the phone's watching link to the mailbox, and
  no earlier than the mailbox's own start, which `/about` now says
  (`started`, a sixth field a 1642 reader leaves): a mailbox restarted
  under a TCP connection that stayed up says "less than a minute".
- This device is said once ("This device" alone where it has no name).
- The avatar, the profile and the list's bar read one chain for the own
  name (`profile::own_name`): the names' word, the profile's name, then
  the mailbox's `/about` name. The page said "lew" while the avatar said
  "?" until a pull; the window's profile golden had that "?" in it.
- The settings list says "Which mailbox, connection, devices" under
  Mailbox, and the phone shows no toast of its own on a copy from
  Android 13 on, where the system previews what was copied.

### Built 2026-11-09, the public rooms' hubs

Lew, 2026-11-08 (1577): Loquax offers the popular public room hubs, and
a user never needs the protocol's name. The only community list (the
Reticulum wiki's RRC page, last changed 2026-09-04) and four operators'
pages published seven `rrc.hub` destinations; two warned a hash changes
on a redeploy. So the list ships only what answered.

**The measurement**, 2026-11-09 from the coder host, with
`loquax/examples/rrc_hubs_probe.rs`: a node of its own dialled the
shipped entry servers (leviculum.network, port 4242, and the
Sauerland testnet, 38.242.217.238, port 4242), listened for `rrc.hub` announces in
three windows of 560 s (06:41 to 07:09 CET, plus a 60 s smoke run
before and the 120 s before the opening round), then opened each hub
with Loquax's own hub link (`loquax::rooms_link`: path, link, Identify,
HELLO until WELCOME, six seconds for the hub's notices, then it left;
it joined no room and said nothing). A hub that did not answer was
tried a second time (07:13), and `just rrc-hubs-check 60` ran the
whole round a third time (07:50).

| hub | destination | announce heard | hops | WELCOME | rooms the hub named | shipped |
|---|---|---|---|---|---|---|
| RNS Community | `28c7c1a6…` | yes (window 3) | 4 to 5 | yes, as `rnscommunity`, three of three | `general` ("/join general for the main room!") | yes |
| ShadowLink (TSK-0) | `c3fd7454…` | yes (windows 1 and 3) | 3 to 6 | yes, as `ShadowLink TSK-0`, three of three | none named (published: `teskeslab`) | yes |
| HADARC | `e7fd7f9e…` | yes (windows 1 and 3) | 4 | yes, as `HADARC_rrc`, three of three | none named (published: `general`) | yes |
| RNS Moscow | `42a97b1b…` | no | 3 to 4 (path request) | yes, as `rns.moscow`, three of three | none named | yes |
| Radio Oxford (OXMESH) | `5da9b708…` | no | 5 to 6 (path request) | no at 07:09, yes at 07:13 and 07:50, as `RADIOXFORD` | none named (published: `#radioxford`) | yes |
| MichMesh | `1303a636…` | no | 4 to 6 (path request) | no, three of three | – | no: published, not reached on 2026-11-09 |
| Amber Hubs on NomadNet | `085b5d6a…` | no | 5 to 8 (path request) | no, three of three | – | no: published, not reached on 2026-11-09 |

MichMesh and Amber answer path requests (a path came back every time)
but no link came up to a WELCOME within the link's budgets (30 s for
the link, five HELLOs five seconds apart). They are recorded as
`[[unreached]]` in the list, not offered, and the check tries them
again.

**Hubs the web does not know.** The windows heard 79 distinct `rrc.hub`
announces; 76 are none of the seven, among them (by the name in their
announce) Gothenburg SE, villa, Hollywood Chat, Cornwall UK, CTRS,
Reticulum Community VIE, Chicago Offline, UTN Oregon, SoCal Public RRC,
RMAP.WORLD RRC Hub, Chatsurfer-RRC, CascadiaRRC, Minuteman RRC,
Save Europe, Colorado Mesh, meshage.me Chat Hub, viennahub, Le Salon FR,
EuskalReticulum, Montreal, Whatcom County Mesh Chat and four without a
name. The smoke run opened the three it had heard: Gothenburg and villa
answered a WELCOME, Hollywood Chat did not. None shipped under 1577's
rule (a hub joins the list with a source that publishes it, as the seven
did); 1599 changed the rule, below. The full list with hashes and hops is
the probe's output (`HEARD` lines), which `just rrc-hubs-check` writes
to `target/tmp/rrc-hubs/probe.txt`.

**The list is data**, `loquax/assets/hubs.toml` (name, hash, the rooms
the hub's welcome named, the published rooms, the source, the day
measured), compiled in by `loquax::ui::hubs`. `just rrc-hubs-check`
(default 1800 s of listening) measures again and exits 1 on a shipped
hub that did not answer or an unreached one that now does: a stale hash
is a finding, not a surprise. No hub is connected to on its own; the
list is what the settings offer.

**The settings row**, in Advanced on the phone, the window and the
terminal, from one function (`settings::hub_rows`): "Public rooms: hub"
with the hub in force by its name, each hub by its name with a mark on
the one in force (since 1599 the hubs heard first, ten a page, keys `1`
to `9` and `0` on the terminal, below), "None" (`n` since 1599), "Another
hub: paste its address" (`a`, the terminal's palette
asks; the window has a field; the phone an `EditText`, through
`Query::RoomsHub`), and "Your name is visible to everyone in the room."
A pasted hub is named by the name its welcome gave, else "Your own
hub", never by its hash. The config key stays `rooms_hub`; the Rooms tab
appears as soon as a hub is chosen, and the hub link runs idle on every
target until one is, so no restart is needed.

**The word.** A joined room is listed with the globe and "public"
("öffentlich") beside its name. The catalogue carries no "RRC" and no
"Relay Chat" anywhere a user reads (`no_string_names_the_protocol`), nor
does a shipped hub's name. The first time a user opens a hub, before
the link, the toast says "Your name is visible to everyone in the room."
(the hub learns the identity on Identify); the config's `rooms_told`
keeps that it was said.

Goldens: `loquax/tests/golden/tui-settings-advanced.txt`,
`gui-settings-advanced-light.png`, and the room's row in
`tui-rooms-room.txt`, `gui-rooms-room.png` and the phone's
`loquax-android/tests/golden/rooms-room.json`.

### Built 2026-11-09, hubs discovered from their announces

Lew decided on 2026-11-09 (morning, 1599): well-reachable hubs ship even
without a published source. **A hub ships when the measurement reached
it and it answered a WELCOME; the source says where it was found, a
publication or "heard on the mesh 2026-11-09". Nothing ships
unmeasured.** And users discover hubs themselves: **hubs are discovered
from their announces; the shipped list seeds a fresh install** that has
heard nothing yet.

The protocol has one discovery mechanism, the one 1577 measured with:
every hub announces its `rrc.hub` destination with its name in the
announce's app data, and every node of the mesh hears it. There is no
registry and no room list inside a hub; a hub's rooms are what its
welcome and notices name and what people join.

**The measurement**, 2026-11-09 from the coder host, CET. The probe
gained `--open heard` (every hub heard, closest first), `--seed` (the
hubs an earlier window heard, 1577's `heard-union.txt`) and `--parallel`
(that many hubs opened at once, each over a hub link of its own on the
one node, so 81 hubs fit one command). Try 1 at 12:08: a 90 s listen
plus the 79 hubs of 1577's windows, 24 at once, 81 hubs opened, 48
WELCOMEs. Try 2 at 12:11: the 33 silent ones, 11 at once, 13 more. Try
3 at 12:14: the 20 still silent, four at once, 5 more (ShadowLink among
them). The two of 1577's five not heard (RNS Moscow, Radio Oxford) were
opened from the list at 12:13 and answered; MichMesh and
Amber again did not. The rooms are what the hub's notices named
(`/join <room>`, `#room`); most hubs name none.

| hub | destination | hops at first hearing | WELCOME (try 1/2/3) | rooms its notices named | shipped |
|---|---|---|---|---|---|
| Cascadia | `fcf451c9…` | 3 | yes/–/– | – | yes |
| Chatsurfer | `0de5818c…` | 3 | yes/–/– | – | yes |
| CTRS | `741da322…` | 3 | yes/–/– | – | yes |
| Minuteman | `68dda18a…` | 3 | yes/–/– | #lobby | yes |
| RMAP.WORLD Hub | `6ffeb457…` | 3 | yes/–/– | #test | yes |
| Save Europe | `ceadff8d…` | 3 | yes/–/– | – | yes |
| SoCal Public | `4f07b6e3…` | 3 | yes/–/– | – | yes |
| UTN Oregon | `472c760a…` | 3 | yes/–/– | – | yes |
| (unnamed) | `dd9313ae…` | 4 | yes/–/– | – | yes |
| Chicago Offline | `215f9cc1…` | 4 | yes/–/– | – | yes |
| Cornwall UK | `fd043546…` | 4 | no/yes/– | – | yes |
| Defunct Hub | `a3369c93…` | 4 | no/yes/– | – | yes |
| Gothenburg SE (Swe) | `54f16391…` | 4 | yes/–/– | #lobby | yes |
| HADARC | `e7fd7f9e…` | 4 | yes/–/– | – | yes |
| meshage.me Chat Hub | `0d4c8a03…` | 4 | yes/–/– | – | yes |
| Pi4 | `247a5627…` | 4 | yes/–/– | – | yes |
| Reticulum Community VIE | `bb750bf4…` | 4 | yes/–/– | – | yes |
| villa | `c930a4f1…` | 4 | no/yes/– | #villa | yes |
| (unnamed) | `32d9b180…` | 5 | yes/–/– | – | yes |
| Cyberdelia Chat Hub | `a1831cc6…` | 5 | yes/–/– | – | yes |
| Estonia | `1b9a8757…` | 5 | yes/–/– | – | yes |
| Hub 42 | `53e3ae1c…` | 5 | yes/–/– | – | yes |
| Meshchat neXt | `ef933cd2…` | 5 | yes/–/– | – | yes |
| MeshChatX | `59ef1330…` | 5 | yes/–/– | – | yes |
| RCC-Hub Rottbitze | `38ca57f1…` | 5 | yes/–/– | – | yes |
| RNS Community | `28c7c1a6…` | 5 | yes/–/– | general | yes |
| RootAccessChatHub | `e78ba2ad…` | 5 | yes/–/– | – | yes |
| Whatcom County Mesh Chat | `4e66bb79…` | 5 | yes/–/– | – | yes |
| NastyCanuck rrc | `e48e8764…` | 5 | no/no/no | – | no |
| Woddy+x MAIN | `56898b13…` | 5 | no/no/no | – | no |
| Юрьев польский | `f4ee2aaf…` | 5 | no/no/no | – | no |
| (unnamed) | `6e018521…` | 6 | yes/–/– | – | yes |
| FOXDPI | `0921dbd3…` | 6 | yes/–/– | – | yes |
| library-socialism | `56690ed8…` | 6 | yes/–/– | – | yes |
| Lipetsk Region | `d027d787…` | 6 | no/no/yes | – | yes |
| Secret Society of Questionable Decisions | `d16092e7…` | 6 | yes/–/– | – | yes |
| ShadowLink | `c3fd7454…` | 6 | no/no/yes | – | yes |
| ValleiRug.nl Hub | `6f4fdb14…` | 6 | no/yes/– | – | yes |
| (unnamed) | `be3af5a9…` | 7 | no/yes/– | – | yes |
| Fenian Central | `d60db458…` | 7 | no/yes/– | – | yes |
| Hispagatos HQ | `8d83c900…` | 7 | yes/–/– | – | yes |
| SnakePitTest | `268f377e…` | 7 | no/no/yes | – | yes |
| SPB RNS | `ead77c9b…` | 7 | yes/–/– | – | yes |
| 3moelf | `7cfc227e…` | 8 | no/yes/– | – | yes |
| Batumi hub | `dc5118c5…` | 8 | yes/–/– | – | yes |
| Hollywood Chat | `bf7469d2…` | 8 | yes/–/– | – | yes |
| jonto-hub | `194d9357…` | 8 | yes/–/– | – | yes |
| mtenk-rns relay | `dc6b1e5b…` | 8 | no/yes/– | – | yes |
| The Chili Line | `b7c5db39…` | 8 | yes/–/– | – | yes |
| ValleyMesh Hub | `df572983…` | 8 | yes/–/– | #lobby | yes |
| Montreal | `ea6d2b2e…` | 8 | no/no/no | – | no |
| Le Salon FR | `ba7065c8…` | 9 | yes/–/– | #general, #radio | yes |
| Rettui Hub | `7e1e788b…` | 9 | no/no/yes | – | yes |
| LINUXIM GROUP RELAY CHAT | `808b5663…` | 9 | no/no/no | – | no |
| Colorado Mesh | `d765e919…` | 10 | yes/–/– | – | yes |
| NovasRedes Hub 1 | `060a6187…` | 10 | yes/–/– | – | yes |
| nl1ctm | `e1fc264c…` | 10 | no/no/no | – | no |
| NM-RNS Chat Relay | `e55699b4…` | 11 | yes/–/– | – | yes |
| Три топора | `dd74d7d0…` | 11 | no/no/yes | #tri-topora | yes |
| RandomTestHub | `91375df7…` | 11 | no/no/no | – | no |
| MBUG HUB | `a394106e…` | 12 | yes/–/– | – | yes |
| Permanet Chat | `082f8e42…` | 12 | no/yes/– | lobby | yes |
| Casual Collapse Club | `5cb5c054…` | 14 | yes/–/– | – | yes |
| VLADIMIR | `ba5744bf…` | 14 | yes/–/– | – | yes |
| EuskalReticulum | `14d0b712…` | 16 | yes/–/– | – | yes |
| Banana Boat | `7a37b0aa…` | 18 | yes/–/– | – | yes |
| Berenklauw | `4663fa0e…` | 20 | no/yes/– | – | yes |
| meshpi | `5866cdf2…` | 20 | no/yes/– | – | yes |
| Lanaudiere | `fca7fe6d…` | 23 | yes/–/– | – | yes |
| PalouseMesh | `d3f50137…` | 26 | no/yes/– | – | yes |
| viennahub | `f5f4d4a5…` | 30 | yes/–/– | – | yes |
| RedRiotRRC | `eac2f4a5…` | 31 | no/no/no | – | no |
| rrc | `8c550ac0…` | 33 | no/no/no | – | no |
| AlcoholicsAnonymousChat | `b56058ce…` | 35 | no/yes/– | – | yes |
| QAF - Home Hub | `516bfb29…` | 35 | yes/–/– | – | yes |
| Ratspeak hub | `d8e5e175…` | 35 | no/no/no | – | no |
| Testing in SLC | `0f3c3b50…` | 37 | no/no/no | – | no |
| MasCasa | `54ca8c9a…` | 49 | no/no/no | – | no |
| Test Chat Hub | `1db4b767…` | 53 | no/no/no | – | no |
| Sutemos hub | `0fa194ae…` | 62 | no/no/no | – | no |
| Hub b11c289d | `2b95df64…` | 72 | no/no/no | – | no |

**68 hubs ship**: the 66 heard ones that answered on one of the three
tries and RNS Moscow and Radio Oxford, ordered by hops at first hearing
(the two never heard by their path's hops), each with `hops`. The five
of 1577 keep their names and sources; the others say "heard on the mesh
2026-11-09". Names are cleaned of the protocol's word as 1577 did
(`ui::hubs::shown_name`: "HADARC_rrc" is HADARC, "CascadiaRRC" Cascadia,
"[SPB_RNS]_Relay_Chat_Service" "[SPB RNS] Service", which the list
shortens by hand to "SPB RNS", and "RRC42" became "Hub 42" by hand); four hubs announce no name and welcome as "hub", and ship
with an empty name, which the settings call "Unnamed hub". The fifteen
that never answered neither ship nor are recorded: the check opens only
the list. "Defunct Hub" and "SnakePitTest" answered, and ship by the rule.

**A first try is not a verdict.** 33 of 81 hubs were silent on the first
try and 18 of them answered a second or third, ShadowLink (shipped since
1577) among them. Whether the 24 links at once or the internet's paths
cost the first tries is not measured; `just rrc-hubs-check` therefore
opens the list twelve at once and every silent hub once more, four at
once, before a shipped hub that did not answer is a finding. Over the 68
on 2026-11-09 at 12:18: 66 answered at once, two on the retry, exit 0.
Its positive control (a list with a made-up hash as a shipped hub and
RNS Community as an unreached one, `just rrc-hubs-check 5
<list>`) reported both findings and exited 1.

**Discovery.** The engine turns every `rrc.hub` announce its node hears
into a heard hub (`OutboxEvent::HubAnnounced`: the hub, its name from
the app data, cleaned, its hops, the time), on every device and on the
home. The screen keeps them in `ui::hubs::Heard`: one entry a hub, its
first hearing kept, the newest name and hops taken, last seen first and
the closer first on a tie, at most 256 (`HEARD_BOUND`; the mesh had 79
in half an hour), the one heard longest ago going first. The home keeps
them for its devices as archive entries of kind `KIND_HUB` (14): a
hub's newest only, appended when the hub is new, its name or hops
changed, or an hour passed since the kept hearing
(`HUB_REFRESH_SECS`), bounded the same; like a name it wakes nobody and
rides with the next pull, and a device that pulls it lists the hub as
one it heard itself. Since 1643 the home also asks each hub it hears for
its rooms, at most hourly, and the entry carries them, so a device's
sheet lists them before it links (`loquax-mailbox.md`, section 6). A hub opened that did not answer stays listed,
marked "no answer"; nothing is removed for that.

**The settings row** shows one list (`settings::offered_hubs`): the
heard hubs by last seen, a shipped one among them marked "suggested",
then the shipped ones not heard, closest first; each with how far it is
("4 hops away", "1 hop away"), a nameless one as "Unnamed hub". Ten a
page, then "More…": the terminal turns to the next ten (`m`, keys `1` to
`9` and `0` for the ten, `n` for none), the window and the phone show
every hub in their page's scroll. No hash on screen.

**Not built in this pass, owed:** the Browse tab's "Hubs in your mesh"
under its rooms section (the tab as 1578 sketched it is not in this
tree), and a device without a home keeps its heard hubs in memory only:
a restart forgets them until they are heard again. The home's list
survives a restart in its archive, and a new device gets it on its
first pull.

### Built 2026-11-09, the pages list comes from the home (1579)

Lew, 2026-11-08: the list of NomadNet pages a reader can open must not
depend on the announces that happen to reach the phone. The Browse tab's
list (and the Entdecken list 1578 builds on it) has two sources, folded
into one lnomad registry (`lnomad::discovery::NomadNodeRegistry`):

1. **What this device's own fetch hears**, `BrowseEvent::Node`, as
   before. A device without a home (standalone, or lnomad itself) has
   only this, as lnomad always had.
2. **What the home heard**, `BrowseEvent::Home` (`loquax/src/ui/browse.rs`,
   `home_pages`): after every pull from the home, the desktop shell
   (`Shell::ask_pages`, `loquax/src/shell.rs`) and the phone's session
   (`ask_pages`, `loquax-android/src/session.rs`) ask `/pages` with the
   stamp the device holds (`Browse::shared`). The first ask of a run
   brings the whole list, later ones the rows that changed. Each page
   row becomes the same `DiscoveredNode` an announce would, so the list
   does not know or show where a row came from. The stamp is held in
   memory, as the registry is: a start asks for the whole list again.

**Bookmarks are the identity's.** A bookmark the reader sets or clears
on any device goes to the home (`Browse::outgoing`, handed by
`runner::hand_bookmarks`) stamped with the device's clock, and comes back
to every device in the next `/pages`. The newer stamp wins, a cleared
bookmark travels as a tombstone (`loquax-mailbox.md`, section 4). A
change the home has not echoed yet stays pending and is handed again
before the next ask, and an older row from the home does not undo it.
The desktop's `browse-bookmarks.toml`, lnomad's format, is the import:
its bookmarks are handed with the time the file was written, so a newer
word from another device wins.

**What 1578 reads:** `model.browse.model.node_registry` (both sources,
name, hops, first and last seen) and `model.browse.model.bookmarks` (the
identity's, merged). lnomad's registry keeps 500 nodes
(`lnomad::discovery::NODE_CAPACITY`), the oldest inserted going first, so
a home list of more than that shows the 500 that changed last; raising it
on the hosts with room is 1578's call.

**Not built in this pass, owed:** the periculum cell
`loquax_pages_from_home` (it lives in periculum, outside this tree); the
M9's Browse tab asks no `/pages` yet; the phone keeps no bookmarks past
the session itself, so its home is where they live; a pending change is
held in memory and a restart before the home took it loses it.

### The intent inventory

Every user action as the intent it raises, and per target whether it is
built, missing, or not applicable with the reason. The table is
`loquax/src/ui/inventory.rs`; its tests refuse an intent without a row,
a built cell whose key, palette command or token in its file is not
there, and this section when it differs from the table
(`LOQUAX_WRITE_INVENTORY=1 cargo test -p loquax --lib inventory` writes
it). A missing cell is the next order.

<!-- intent-inventory:begin (loquax/src/ui/inventory.rs) -->
| intent | what the user does | phone | window | terminal | M9 |
|---|---|---|---|---|---|
| `Tab` | a tab of the bar (Chats, Map, Explore) | built (`ConversationsActivity.java`) | built (`gui.rs`) | built (palette) | **missing**: no key switches the tab |
| `Browse` | a verb of the hosted browser in the Browse tab | built (`session.rs`) | built (`gui.rs`) | built (`ui.rs`) | **missing**: the M9 has no Browse tab |
| `Explore` | the Explore tab: search, order, open a page, bookmark it, open a room, pick a hub | built (`session.rs`) | built (`gui.rs`) | built (`ui.rs`) | **missing**: the M9 has no third button; a key and a list screen for it (handheld page) |
| `Back` | back: the bar's arrow, Esc, the M9's back key, leaving the profile | built (`session.rs`) | built (`gui.rs`) | built (key) | built (`keys.rs`) |
| `OpenChat` | a tap or click on a conversation's row | built (`session.rs`) | built (`gui.rs`) | built (`ui.rs`) | built (`keys.rs`) |
| `CloseChat` | leave the open chat for the list | built (`session.rs`) | built (`gui.rs`) | n/a: the list stands beside the chat; Back moves the keys to it | n/a: Back moves the keys to the list |
| `ShowMap` | the map: its tab, the palette's map | built (`ConversationsActivity.java`) | built (`gui.rs`) | built (palette) | **missing**: no key opens the map |
| `OpenMapAt` | a row's map pin, the chat bar's map: the peer on the map | built (`ConversationsActivity.java`) | built (`gui.rs`) | built (key) | **missing**: no key shows the peer on the map |
| `Activate` | a tap or click on a scene node | n/a: each View's click is its own listener | built (`gui.rs`) | built (`tui.rs`) | n/a: no pointer |
| `Menu` | a long press or right click on a row or a bubble: its verbs | built (`ChatActivity.java`) | built (`chat.rs`) | n/a: a row's verbs are the palette's | **missing**: no key opens a row's verbs |
| `Dismiss` | a sheet or menu closed by a tap outside it | n/a: the system's back closes a dialog | built (`gui.rs`) | n/a: Esc is Back | n/a: the back key is Back |
| `Focus` | focus the list, the messages or compose | n/a: a touch screen has no key for it | n/a: the pointer does it | built (palette) | built (`keys.rs`) |
| `NextPane` | Tab: the next pane | n/a: a touch screen has no key for it | n/a: the pointer does it | built (key) | n/a: one pane at a time |
| `PreviousPane` | Shift-Tab: the previous pane | n/a: a touch screen has no key for it | n/a: the pointer does it | built (key) | n/a: one pane at a time |
| `Palette` | `:` opens the command palette | n/a: a touch screen has no key for it | **missing**: the window has no command palette | built (key) | **missing**: no key opens the palette |
| `Help` | `?`: the keys' help | n/a: a touch screen has no key for it | built (`gui.rs`) | built (palette) | **missing**: no key opens the help |
| `Quit` | quit | n/a: Android ends apps | n/a: the window's own close | built (palette) | n/a: the power key |
| `QuitHint` | Ctrl-c outside compose: say what quits | n/a: a touch screen has no key for it | n/a: the window's own close | built (key) | n/a: the power key |
| `Redraw` | redraw the screen | n/a: the platform redraws | n/a: the platform redraws | built (palette) | n/a: the board redraws |
| `Send` | Send: the compose bar's button, Enter | built (`session.rs`) | built (`gui.rs`) | built (palette) | built (`keys.rs`) |
| `Edit` | a key handed to the text being edited | n/a: the platform's text field does it | n/a: the platform's text field does it | built (key) | built (`keys.rs`) |
| `Newline` | Alt-Enter: a new line in compose | n/a: the platform's text field does it | n/a: the platform's text field does it | built (key) | **missing**: no key breaks the line |
| `ClearDraft` | Ctrl-u: clear compose | n/a: the platform's text field does it | n/a: the platform's text field does it | built (key) | **missing**: no key clears compose |
| `Draft` | what the text field holds, kept as the draft | built (`session.rs`) | built (`gui.rs`) | n/a: compose is the model's own buffer | n/a: compose is the model's own buffer |
| `Insert` | an emoji picked into compose | built (`session.rs`) | built (`gui.rs`) | **missing**: no emoji picker | **missing**: no emoji picker |
| `Emoji` | the compose bar's emoji button: the picker | built (`ChatActivity.java`) | built (`gui.rs`) | **missing**: no emoji picker | **missing**: no emoji picker |
| `React` | a reaction on a bubble | built (`session.rs`) | built (`gui.rs`) | built (palette) | **missing**: no key reacts |
| `Reply` | Reply on a bubble | built (`session.rs`) | built (`gui.rs`) | built (palette) | **missing**: no key replies |
| `CancelReply` | the reply bar's cross | built (`ChatActivity.java`) | built (`gui.rs`) | **missing**: no way out of a reply but sending or clearing | **missing**: no key replies |
| `NoticeReply` | a reply typed into a message notification: read up to what it showed | built (`session.rs`) | **missing**: the desktop notification carries no actions | **missing**: the desktop notification carries no actions | n/a: the board posts no notifications |
| `Forward` | Forward on a bubble, then a chat | built (`session.rs`) | built (`gui.rs`) | built (palette) | **missing**: no key forwards |
| `SendAgain` | the red mark of a message not sent | built (`session.rs`) | built (`gui.rs`) | built (palette) | **missing**: no key sends again |
| `ViewPhoto` | a tap on a bubble's photo: the photo whole | built (`ChatActivity.java`) | built (`gui.rs`) | **missing**: no photo viewer | n/a: too large for the radio |
| `SendPhoto` | the plus sheet's Photo: a picture picked and sent | built (`session.rs`) | built (`gui.rs`) | built (palette) | n/a: too large for the radio |
| `PhotoSize` | the settings' photo size | built (`session.rs`) | built (`gui.rs`) | built (palette) | n/a: too large for the radio |
| `Attach` | the compose bar's plus: the attachment sheet | built (`session.rs`) | built (`gui.rs`) | n/a: the palette names each attachment | n/a: too large for the radio |
| `Locate` | the compose bar's position button: the position sheet | built (`session.rs`) | built (`gui.rs`) | n/a: the palette names each position verb | **missing**: no key opens the position sheet |
| `Camera` | the compose bar's camera | built (`ChatActivity.java`) | n/a: no camera | n/a: no camera | n/a: no camera |
| `StagePhoto` | the camera's photo staged in the bar | built (`session.rs`) | n/a: no camera | n/a: no camera | n/a: no camera |
| `Unstage` | the staged photo's cross | built (`session.rs`) | n/a: no camera | n/a: no camera | n/a: no camera |
| `CloseSheet` | a sheet of the compose bar closed | built (`session.rs`) | built (`gui.rs`) | n/a: the terminal opens no sheet | n/a: the M9 opens no compose sheet |
| `ShareFix` | the position sheet's "Send my position now" | built (`session.rs`) | built (`gui.rs`) | built (palette) | **missing**: no key shares the position |
| `StartShare` | a share row of the position sheet: 15 min, 1 h, 8 h, until stopped | built (`session.rs`) | built (`gui.rs`) | **missing**: the palette shares once, never for a while | **missing**: no key shares the position |
| `StopShare` | a running share's Stop: the chat's chip, Settings, the notification | built (`session.rs`) | built (`chat.rs`) | built (palette) | **missing**: no key shares the position |
| `LocationOn` | the position sheet without a fix: turn location on | built (`Locator.java`) | n/a: no position receiver | n/a: no position receiver | **missing**: the M9's GNSS is not wired to the sheet |
| `EnterPosition` | the position sheet's entry on a shell without a receiver | n/a: the phone has a receiver | built (`gui.rs`) | built (palette) | **missing**: no key enters a position |
| `FixAtMapCentre` | the position sheet's "map centre" row | n/a: the phone has a receiver | built (`gui.rs`) | **missing**: no "map centre" in the palette | **missing**: no key enters a position |
| `OwnFix` | the platform's own fix, or none | built (`session.rs`) | n/a: no position receiver | n/a: no position receiver | **missing**: the M9's GNSS is not wired to the model |
| `SendFile` | the plus sheet's File: a file picked and sent | built (`session.rs`) | built (`gui.rs`) | built (palette) | n/a: too large for the radio |
| `OpenFile` | a tap on a bubble's file: saved and opened | built (`session.rs`) | built (`gui.rs`) | **missing**: no way to save a file from a bubble | n/a: too large for the radio |
| `SendVoice` | the microphone held and released: a voice message | built (`session.rs`) | built (`gui.rs`) | built (`keys.rs`) | n/a: too large for the radio |
| `Recording` | a button of the recording bar: lock, pause, listen, send, discard | built (`session.rs`) | built (`gui.rs`) | built (`keys.rs`) | n/a: too large for the radio |
| `PlayVoice` | a voice message's play button | built (`session.rs`) | built (`gui.rs`) | **missing**: no key plays a voice message | n/a: too large for the radio |
| `PauseVoice` | a playing voice message's pause button | built (`session.rs`) | built (`gui.rs`) | **missing**: no key plays a voice message | n/a: too large for the radio |
| `Delete` | Delete on a bubble, asked, then confirmed | built (`session.rs`) | built (`gui.rs`) | built (palette) | **missing**: no key deletes a message |
| `Copy` | Copy on a bubble or a selection | built (`ChatActivity.java`) | built (`gui.rs`) | built (key) | n/a: no clipboard |
| `Share` | a position shared from the map | built (`session.rs`) | built (`gui.rs`) | n/a: the palette's position verbs share | **missing**: no key shares the position |
| `Pin` | a row's Pin | built (`session.rs`) | built (`gui.rs`) | built (palette) | **missing**: no key opens a row's verbs |
| `Mute` | a row's Mute | built (`session.rs`) | built (`gui.rs`) | built (palette) | **missing**: no key opens a row's verbs |
| `Archive` | a row's Archive | built (`session.rs`) | built (`gui.rs`) | built (palette) | **missing**: no key opens a row's verbs |
| `Block` | a contact's Block | built (`session.rs`) | built (`gui.rs`) | built (palette) | **missing**: no key opens a row's verbs |
| `Verify` | a contact's Verify | built (`session.rs`) | built (`gui.rs`) | built (palette) | **missing**: no key opens a row's verbs |
| `MarkUnread` | a row's Mark unread | built (`session.rs`) | built (`gui.rs`) | built (palette) | **missing**: no key opens a row's verbs |
| `Rename` | a contact's Rename | built (`session.rs`) | built (`gui.rs`) | built (palette) | **missing**: no key opens a row's verbs |
| `Forget` | a row's Delete chat, asked, then confirmed | built (`session.rs`) | built (`gui.rs`) | built (palette) | **missing**: no key opens a row's verbs |
| `AddContact` | a friend's code pasted or scanned: the contact | built (`session.rs`) | built (`gui.rs`) | built (palette) | **missing**: no way to add a contact |
| `Group` | New group, Join, the member sheet, Leave, Remove | built (`session.rs`) | built (`gui.rs`) | built (palette) | built (`group.rs`) |
| `NewConversation` | the bar's pencil: a new chat | built (`ConversationsActivity.java`) | built (`gui.rs`) | built (palette) | **missing**: no way to start a chat |
| `Profile` | a tap or click on the reader's avatar, `P`, `my code` | built (`session.rs`) | built (`gui.rs`) | built (key) | **missing**: the house key leads to the settings (1561), which hold no profile: the M9 is a device of its home (super::settings::PLACES) |
| `ShareCode` | the profile's "Share my address" | built (`ProfileActivity.java`) | built (`gui.rs`) | built (`ui.rs`) | n/a: no clipboard or share sheet |
| `Name` | the name recipients see | built (`session.rs`) | **missing**: the profile shows the name, nothing sets it | **missing**: the profile shows the name, nothing sets it | **missing**: the name is the pairing's |
| `Scroll` | the list's cursor up and down | n/a: the list scrolls itself | built (`gui.rs`) | built (key) | built (`keys.rs`) |
| `ScrollMessages` | the messages' cursor from compose | n/a: the chat scrolls itself | n/a: the chat scrolls itself | built (key) | n/a: Back leaves compose for the bubbles |
| `NextConversation` | the next conversation | n/a: a touch screen has no key for it | n/a: the pointer does it | built (palette) | n/a: up and down move the list's cursor |
| `PreviousConversation` | the previous conversation | n/a: a touch screen has no key for it | n/a: the pointer does it | built (palette) | n/a: up and down move the list's cursor |
| `NextUnread` | the next conversation with unread messages | n/a: a touch screen has no key for it | n/a: the pointer does it | built (palette) | **missing**: no key jumps to the unread |
| `PreviousUnread` | the previous conversation with unread messages | n/a: a touch screen has no key for it | n/a: the pointer does it | built (palette) | **missing**: no key jumps to the unread |
| `Search` | the bar's magnifier, `/` | built (`ConversationsActivity.java`) | built (`gui.rs`) | built (palette) | **missing**: no key searches |
| `CloseSearch` | Esc in the search | n/a: the system's back | **missing**: the window's search field has no close | built (key) | **missing**: no key searches |
| `OpenResult` | Enter on a search result | n/a: a result is a row; a tap opens it | n/a: a result is a row; a click opens it | built (key) | **missing**: no key searches |
| `Match` | the next or previous match | n/a: the phone lists the results | n/a: the window lists the results | built (key) | **missing**: no key searches |
| `OpenMessage` | Enter on a bubble: a position on the map, else its detail | n/a: a tap on the bubble is its own | n/a: a click on the bubble is its own | built (key) | **missing**: no key opens a message |
| `Details` | a bubble's Info | built (`ChatActivity.java`) | built (`chat.rs`) | built (palette) | **missing**: no key opens a message |
| `ShowBlocked` | the blocked contacts | built (`ContactActivity.java`) | **missing**: the window lists no blocked contacts | built (palette) | **missing**: no list of the blocked |
| `ShowArchived` | the archived chats | built (`ConversationsActivity.java`) | **missing**: the window lists no archived chats | built (palette) | **missing**: no list of the archived |
| `Map` | zoom, pan, fit, step between pins, the map's drawing | n/a: pinch and pan are the map View's | built (`gui.rs`) | built (key) | **missing**: no key moves the map |
| `SelectPin` | a tap or click on a pin: its card | built (`MapView.java`) | built (`gui.rs`) | built (`ui.rs`) | **missing**: no key steps between pins |
| `LoadMapArea` | load the map's area here | n/a: the map loads what it shows | built (`gui.rs`) | built (palette) | n/a: the board carries its regions |
| `MapDrawing` | how the map is drawn | n/a: the phone's map draws one way | **missing**: the window offers no drawing | built (palette) | n/a: the board draws one way |
| `Pull` | fetch messages now | built (`session.rs`) | built (`chat.rs`) | built (palette) | n/a: the board pulls on its own |
| `Front` | the app came to the front or left it | built (`session.rs`) | built (`gui.rs`) | n/a: a terminal is in front while it runs | n/a: the board's screen is always in front |
| `Network` | the platform's network changed | built (`session.rs`) | n/a: the daemon follows the network | n/a: the daemon follows the network | n/a: the board's radio is its network |
| `Viewport` | these bubbles are on screen: what was read | built (`session.rs`) | built (`gui.rs`) | n/a: the open conversation is read | n/a: the open conversation is read |
| `Theme` | light or dark | n/a: follows the phone | built (`gui.rs`) | built (palette) | n/a: the panel draws dark |
| `SendOnEnter` | whether Enter sends | n/a: the platform's text field does it | **missing**: the window has no switch for Enter | built (palette) | n/a: Enter sends |
| `Mouse` | the mouse captured or the terminal's own selection | n/a: no pointer | n/a: the window's pointer is its own | built (palette) | n/a: no pointer |
| `Import` | import from another messenger | **missing**: the phone imports nothing | **missing**: the window imports nothing | built (palette) | n/a: the board takes its history from the home |
| `ReadReceipts` | the switch "Send read receipts" | built (`session.rs`) | built (`gui.rs`) | built (palette) | **missing**: the M9's settings have no read receipts row: the home signs them (super::settings::PLACES, Privacy) |
| `DeleteAllRegions` | the Maps page's Delete all | built (`session.rs`) | built (`gui.rs`) | built (palette) | n/a: the board carries its regions |
| `Settings` | the settings: the nine sections of super::settings::SECTIONS, whose own table says per section and target where it is drawn | built (`ConversationsActivity.java`) | built (`gui.rs`) | built (key) | built (`house.rs`) |
| `RoomsHub` | the public rooms' hub in Advanced: a shipped hub, none, or another pasted (1577) | built (`session.rs`) | built (`gui.rs`) | built (`ui.rs`) | **missing**: the M9 has no rooms: its settings hold no hub (super::settings::PLACES, Advanced) |
<!-- intent-inventory:end -->

### The goldens' index

Every scene a golden pins, with its golden per target, so a walk that
cannot run a target reads this list instead of guessing file names
(1451, after UI walk 1 found scenes no golden showed). A file stands
under the scene it shows; "not drawn" says the target has no such
screen, with the reason, which is what a walk reports, not a missing
golden. The window's pictures and the terminal's screens are in
`loquax/tests/golden/` (`gui-*`, `tui-*`, the scenes `scene-*` and the
tracks' cells `tracks-*`), the terminal's sessions in
`loquax/tests/fixtures/` (`*.frames`, one file a session with every
frame it asks for), the M9's frames in `loquax-m9/tests/golden/`, the
phone's in `loquax-android/tests/golden/`; each directory's README or
the test named in it says what a file shows. Every terminal golden has
its German twin (`-de`, `_de`), drawn as a desktop under
`LANG=de_DE.UTF-8` draws it; the style map `tui_chat.styles` has none,
it pins colours, not words. `loquax/tests/golden_index.rs` refuses a
name here with no file, a golden file with no name here, and a terminal
golden without its German twin.

<!-- goldens-index:begin -->
| scene | window | terminal | terminal, German | M9 | phone | scene, every target |
|---|---|---|---|---|---|---|
| First start, before a mailbox | `gui-first-start.png`, `gui-first-start-profile.png` | `tui-first-start.txt`, `tui-first-start-profile.txt` | `tui-first-start-de.txt`, `tui-first-start-profile-de.txt` | `m9-fw-splash.png`, `m9-fw-pairing.png` | not pinned: the first start is `WelcomeActivity.java`'s own | |
| Joining a mailbox with its code | not drawn: a desktop joins with `loquax mailbox join` | not drawn: a desktop joins with `loquax mailbox join` | | `m9-joining.png` | not pinned: the join form is `JoinForm.java`'s own | |
| The mailbox out of reach, and back | not drawn: the desktop's shell never learns the mailbox's reach | not drawn: the desktop's shell never learns the mailbox's reach | | `m9-offline.png`, `m9-online.png`, `m9-way-radio.png` | not pinned: the offline bar is `ConversationsActivity.java`'s own | |
| The chats list and an open chat | `gui-chat-light.png`, `gui-chat-dark.png` | `tui_session.frames`, `tui_chat.frames`, `tui_chat.styles`, `tui_layout.frames` | `tui_session_de.frames`, `tui_chat_de.frames`, `tui_layout_de.frames` | `m9-chats.png`, `m9-chats-cursor.png`, `m9-chat.png`, `m9-chat-cursor.png`, `m9-fw-chats.png`, `m9-fw-chat.png` | | `scene-compact-list.json`, `scene-compact-chat.json`, `scene-wide.json` |
| A narrow window | `gui-chat-compact-chat.png`, `gui-chat-compact-list.png` | | | | | |
| The large type | | | | `m9-chats-large.png`, `m9-chat-large.png` | | |
| The very small type (1561) | | | | `m9-chats-tiny.png`, `m9-chat-tiny.png` | | |
| The settings | `gui-settings-light.png`, `gui-settings-privacy-light.png`, `gui-settings-advanced-light.png` | `tui-settings.txt`, `tui-settings-advanced.txt` | `tui-settings-de.txt`, `tui-settings-advanced-de.txt` | `m9-settings.png`, `m9-settings-de.png`, `m9-settings-tiny.png` (the house key's page: the core's settings for the M9, Appearance and Advanced, 1561) | | |
| The mailbox page, which mailbox and how it is reached | `gui-settings-mailbox-light.png` | `tui-settings-mailbox.txt` | `tui-settings-mailbox-de.txt` | not drawn: the M9's settings have no mailbox page (`settings::PLACES`) | `mailbox-page-reachable.json`, `mailbox-page-unreachable.json` | |
| The keys' help | not pinned: the shared overlay lnomad's window draws | not pinned: `?` | | not drawn: no key opens the help; the keys stand on their screens (`m9-fw-pairing.png`, `m9-map-hint.png`, the group's frames) | n/a: no keys | |
| The keymap and the mouse | | `tui_keys.frames`, `tui_mouse.frames` | `tui_keys_de.frames`, `tui_mouse_de.frames` | | | |
| Typing and sending | | | | `m9-compose.png`, `m9-sent.png` | | |
| A reply, a reaction, a photo | `gui-reply-sent.png` | `tui-reply-sent.txt` | `tui-reply-sent-de.txt` | `m9-reply.png`, `m9-reaction.png` (the M9 draws no reaction), `m9-photo.png` | | `scene-reply.json` |
| A reply being written | `gui-reply-compose.png` | `tui-reply-compose.txt` (the terminal draws no quote) | `tui-reply-compose-de.txt` | `m9-reply-compose.png` (the M9 says not what it answers) | | `scene-reply-compose.json` |
| Reactions and their picker | | `tui_reactions.frames` | `tui_reactions_de.frames` | | | |
| A long message read by the line | | `tui_long_message.frames` | `tui_long_message_de.frames` | `m9-chat-long-mid.png`, `m9-chat-long-back.png`, `m9-chat-top-held.png` | | |
| Photos, files and voice in a chat | `gui-chat-media.png`, `gui-attach-sheet-light.png` | `tui_media.frames`, `tui-file-progress.txt` | `tui_media_de.frames`, `tui-file-progress-de.txt` | | | |
| Recording a voice message | `gui-recording-held.png`, `gui-recording-locked.png`, `gui-recording-paused.png` | `tui_recording.frames` | `tui_recording_de.frames` | | | |
| A message's Info | `gui-info-light.png` | | | | | |
| Selecting and copying text | `gui-chat-selected-light.png`, `gui-chat-selected-dark.png` | `tui-copy-mode.txt` | `tui-copy-mode-de.txt` | | | |
| A late message at its sent time | `gui-late-message.png` | `tui-late-message.txt`, `tui_late.frames` | `tui-late-message-de.txt`, `tui_late_de.frames` | `m9-late-message.png` | `late-message.json` | |
| Two contacts of one name | `gui-same-name.png` | `tui-same-name.txt`, `tui_names.frames` | `tui-same-name-de.txt`, `tui_names_de.frames` | `m9-same-name-list.png`, `m9-same-name-chat.png` | | |
| The ticks and the receipts' ladder | | | | | | `scene-ladder.json` |
| Search | | `tui_search.frames`, `tui_search_session.frames` | `tui_search_de.frames`, `tui_search_session_de.frames` | | | |
| A new conversation from the palette | | `tui_palette_new.frames` | `tui_palette_new_de.frames` | | | |
| A send's two legs in the toast | | `tui_toast_two_legs.frame` | `tui_toast_two_legs_de.frame` | | | |
| History, drafts and an import | | `tui_history.frames`, `tui_draft.frames`, `tui_ugly.frames` | `tui_history_de.frames`, `tui_draft_de.frames`, `tui_ugly_de.frames` | | | |
| The profile | `gui-profile-light.png` | `tui_profile.frames` | `tui_profile_de.frames` | | `profile-words.json` | `scene-profile.json` |
| Groups: new, invited, the chat, its members | `gui-group-new.png`, `gui-group-invitation.png`, `gui-group-chat.png`, `gui-group-members.png` | | | `m9-group.png`, `m9-fw-groups.png`, `m9-fw-invite.png`, `m9-fw-members.png`, `m9-fw-admin.png`, `m9-fw-ask.png`, `m9-fw-name.png`, `m9-fw-pick.png` | | `scene-group-list.json`, `scene-group-new-name.json`, `scene-group-new-pick.json`, `scene-group-invitation.json`, `scene-group-invited-chat.json`, `scene-group-chat.json`, `scene-group-members.json`, `scene-group-members-leave.json` |
| A group's one and two ticks | `gui-group-ticks.png` | `tui-group-ticks.txt` | `tui-group-ticks-de.txt` | `m9-group-ticks.png` | `group-ticks.json` | `scene-group-ticks.json` |
| A relayed group | `gui-relay-group.png` | `tui-relay-group.txt` | `tui-relay-group-de.txt` | | `relay-group.json` | `scene-relay-group.json` |
| A room | `gui-rooms-room.png` | `tui-rooms-room.txt` | `tui-rooms-room-de.txt` | not drawn: the M9 has no rooms | `rooms-room.json` | |
| Explore | `gui-explore.png`, `gui-explore-as-of.png` | `tui-explore.txt`, `tui-explore-sheet.txt` | `tui-explore-de.txt`, `tui-explore-sheet-de.txt`, `tui-explore-as-of-de.txt` | not drawn: the M9 has no Explore | `explore-as-of-old.json`, `explore-as-of-down.json` | `scene-explore.json`, `scene-explore-search.json`, `scene-explore-sheet.json`, `scene-explore-wide.json` |
| Browse, a NomadNet page | `gui-browse-list.png`, `gui-browse-page.png` | `tui-browse-list.txt`, `tui-browse-page.txt` | `tui-browse-list-de.txt`, `tui-browse-page-de.txt` | not drawn: the M9 has no Browse tab | | `scene-browse-list.json`, `scene-browse-page.json`, `scene-browse-form.json` |
| The map with two peers, one selected | `gui-map-pins.png`, `gui-map-locations.png`, `gui-map-long-name.png` | `tui-map-pins.txt` | `tui-map-pins-de.txt` | `m9-map-card.png` | | |
| The map screen and its menu | `gui-map-window.png`, `gui-map-window-menu.png`, `gui-map-window-week.png`, `gui-map-uncovered-dark.png` | `tui_map.frames`, `tui_map_cycle.frames`, `tui_mapload.frames` | `tui_map_de.frames`, `tui_map_cycle_de.frames`, `tui_mapload_de.frames` | `m9-map-open.png`, `m9-map-hint.png`, `m9-map-no-region.png`, `m9-map-overview.png`, `m9-map-world-z2.png`, `m9-map-z13.png`, `m9-map-z15.png`, `m9-map-z16.png`, `m9-map-sky-fix.png`, `m9-map-sky-searching.png`, `m9-fw-map.png`, `m9-fw-pan.png` | | |
| The map's picture: names and buildings | `gui-names-florence-z16.png`, `gui-buildings-light-z16.png`, `gui-buildings-dark-z16.png`, `gui-buildings-light-z17.png`, `gui-buildings-dark-z17.png` | | | | `map-florence-z16.png` | |
| The world view over its outline, z2 and z6, and its disputed lines dashed over Kashmir at z5 | `gui-world-window-z2.png`, `gui-world-window-z6.png`, `gui-world-window-kashmir-z5.png` | | | `m9-map-world-z6.png` | `gui-world-phone-z2.png`, `gui-world-phone-z6.png` | |
| A position in a chat, shared and typed | `gui-position-sheet-light.png`, `gui-position-entry-light.png`, `gui-share-chip-light.png` | `tui_telemetry.frames`, `tui_liveness.frames`, `tui_share_chip.frames` | `tui_telemetry_de.frames`, `tui_liveness_de.frames`, `tui_share_chip_de.frames` | | | |
| Tracks | `gui-tracks-hiking.png`, `gui-tracks-park.png`, `gui-tracks-park-dark.png`, `gui-tracks-convoy.png`, `gui-tracks-week-old.png`, `gui-tracks-outlier.png`, `gui-tracks-handbag.png`, `gui-tracks-radar.png`, `gui-tracks-way-back.png`, `gui-tracks-late.png`, `gui-tracks-battery.png`, `gui-tracks-fit-all.png` | `tracks-hiking.txt`, `tracks-park.txt`, `tracks-convoy.txt`, `tracks-week-old.txt`, `tracks-outlier.txt`, `tracks-handbag.txt`, `tracks-radar.txt`, `tracks-way-back.txt`, `tracks-late.txt`, `tracks-battery.txt`, `tracks-fit-all.txt` | `tracks-hiking-de.txt`, `tracks-park-de.txt`, `tracks-convoy-de.txt`, `tracks-week-old-de.txt`, `tracks-outlier-de.txt`, `tracks-handbag-de.txt`, `tracks-radar-de.txt`, `tracks-way-back-de.txt`, `tracks-late-de.txt`, `tracks-battery-de.txt`, `tracks-fit-all-de.txt` | | | |
<!-- goldens-index:end -->

## 2. Decision: what honesty about delivery looks like

Most chat UIs lie by simplification: one check for sent, two for delivered,
and everything ambiguous rounded to the friendlier reading. LXMF has real
states and the library reports them, so there is no excuse.

The states as they actually are, from
[the library survey](loquax-architecture.md):

| Situation | Library state | What is actually true |
|---|---|---|
| queued, waiting for a route or a retry | `Outbound` | nothing has been transmitted |
| computing proof-of-work | `Outbound` + `StampPending` | nothing has been transmitted, and it may take a while |
| link being built, or bytes going out | `Sending` | in flight, no confirmation |
| opportunistic packet handed to Reticulum | `Sent` | transmitted once, unproven, still retryable |
| propagation node accepted the upload | `Sent`, entry deleted | it waits at a propagation node; the recipient may never collect it |
| transport proof received | `Delivered` | the bytes reached the destination identity |
| receiver cancelled the resource | `Rejected` | they refused it, or their client did |
| node refused the upload (bad stamp) | `Rejected` | the propagation node refused it, not the recipient |
| five attempts exhausted | `Failed` | give up, keep the text |

### What a truthful UI shows

**Different marks for different truths, and words in the detail view.** The
three that must never be collapsed:

- **handed to the network, unproven** (opportunistic `Sent`),
- **left at a propagation node for later** (propagated `Sent`),
- **arrived at the destination** (`Delivered`).

columba collapses the first two into one check mark; NomadNet gives them
different colours but never words. Words for each, and,
crucially, **a per-message detail view** (columba's best idea) showing
delivery method with a one-sentence explanation, attempt count, hop count,
and the error string when there is one. In a terminal that is a key press
on a focused message.

**Claim a read only on a receipt.** LXMF has no read field
(`leviculum-lxmf/src/constants.rs:60-86`), so a read is never inferred from
a proof or anything else. Since 2026-10-31 Loquax carries its own, signed
by the reader, in the `lev` namespace of the custom field pair: a message
is shown read only when its peer's receipt named it, and a contact without
receipts stays at delivered. The wire, the gating, the read event and the
glyph ladder that replaces the table below are
[read receipts](loquax-read-receipts.md).

**Say what `Delivered` means, once.** It means the bytes reached the
destination identity, not that a human saw them. The detail view is where
that sentence lives.

**Distinguish the two `Rejected`s** by correlating with the message's
method before the entry disappears. "The propagation node refused this message" and
"the recipient's client refused this message" are different problems with
different fixes.

**Handle the restart discontinuity honestly.** `restore` resets every
queued message to `Outbound` (`leviculum-lxmf/src/router.rs:2339-2342`). So
after a restart, a message that was "sending" is "queued" again, and the UI
must show that rather than a frozen progress bar. NomadNet's equivalent,
forcing a stale mid-flight message to `FAILED` on load (NomadNet's
`Conversation.py`, lines 455-467), is at least honest, though our library
gives us the better option of an honest retry.

**Never let a late failure demote a success.** columba added exactly this
guard after a real bug. `Delivered` must be terminal in the UI even if
something arrives afterwards claiming otherwise.

### Decision (2026-08-10): the ledger and the glyph

**The ledger is one keypress away, inside the per-message detail view** —
part of the normal program, not a debug flag, but costing no space in the
conversation. It is also where the airtime number lives. The ledger rows
are stored beside the message in SQLite (a handful of roughly 50-byte rows
per message) and share the message's retention
([storage](loquax-storage.md)).

**Decided with it: one coloured glyph per message in the conversation
view**, carrying the delivery state at a glance. Since 2026-11-01 (1232) the
glyph is the tick ladder of [read receipts](loquax-read-receipts.md),
section 3, two cells wide, and it replaces the 2026-08-10 table, which drew
unproven, mailbox and proof as three distinct one-cell marks. The words
still keep them apart: the detail view and the screen reader say "sent, not
confirmed", "in the mailbox" and "delivered" for what the glyph draws as a
clock, one tick and two.
The glyph is the compact face of the same state machine the detail view
explains; both render from **one shared state-mapping table** (the keymap
discipline above, applied again — the glyph, the detail view and the ledger
can never disagree).

The ladder, one shape per rung, each carried by its shape before its
colour:

| state | glyph | ASCII | tone | weight |
|---|---|---|---|---|
| queued, or sent with no proof (opportunistic `Sent`) | `◷` | `.` | grey | normal |
| computing proof-of-work, in flight | braille spinner `⠋⠙⠹⠸⠼⠴` | `*` | grey | normal, animated on the tick |
| left at a mailbox (propagated `Sent`; on a client of a home node, the home's `Sent`) | `✓` | `v` | grey | normal |
| arrived (`Delivered`) | `✓✓` | `vv` | grey | normal |
| read (the peer's receipt, `Read`) | `✔✔` | `VV` | green, the accent | bold |
| refused (`Rejected`) or given up (`Failed`) | `!` | `!` | red | bold |

The rules it encodes: **a tick only for what someone else confirmed**, one
from the mailbox, two from the recipient's device; **green only for read**,
and read is also heavier, so a reader who cannot tell green from grey, or a
monochrome terminal, still sees it; the clock means nobody confirmed it yet,
whether it waits here or left without a proof; animation means "the machine
is working right now" and nothing else, driven by the
[event-loop tick](loquax-architecture.md) so it freezes honestly if the
program hangs. Every glyph is padded to two cells, so the text before it
never moves when the state changes. The phone draws the same ladder as bare
ticks on a 24-unit grid (`loquax::ui::chat::Shape`, read by `Glyph.java`);
a failed mark is a red circle with `!`, and its tap sends the message again.

The table's code home is `DELIVERY_GLYPHS` in `leviculum-ui`
(`leviculum-ui/src/delivery.rs`, since 2026-10-25, 984), one row per state
with its glyph, tone, weight and ASCII fallback; its tests hold the rules
above (a tick only on a confirmation, green only on read, every glyph two
cells, only the working states animate). The
spinner rows draw the shared loading spinner of `leviculum-ui/src/spinner.rs`,
the one `lnomad` shows, so its frames are the ones the table lists. The
tones are resolved against the palette by the frontend. Since 1042 that
palette is the token table of `leviculum-ui/src/tokens.rs`, a colour per
tone for each theme and tier (since 1232 three: pending grey, read green,
failed red), measured against our own block's tint and
its selected tint because a mark only ever sits in our own block: the one
dark-pane palette before it put the yellow at 1.7:1 on a white pane. In
16 colours a mark takes its block's text colour, and the glyph alone
carries the state, as it does in monochrome.

`loquax` draws it since 2026-10-25 (994), and maps the router's states
onto the rows in `loquax::ui::delivery_of`: `Generating` and `Outbound` are
queued `◷`, `Sending` is in flight, `Sent` is `◷` (on a client of a home
node `✓`, `loquax::ui::shown_delivery`: the home is the reader's mailbox),
`AwaitingCollection` is `✓`, `Delivered` is `✓✓`, `Rejected`, `Failed` and
`Cancelled` are the red `!`. The stamping row has no router state yet: the
engine mines without saying so. While a send's runner is still working, a
giving-up state of one of its legs is not drawn, because `--via auto`
cancels a stranded direct leg and carries the same text to a propagation node; the
`!` appears when the runner reports it gave up.

A direct leg whose message went onto a link to the peer (`Sending`) and
got no proof inside the leg's time is *unproven*, not failed (1002): the
peer answered the link, so it is reachable, and a propagation node's copy would reach
it a second time. The reference never re-sends such a message through a
propagation node on its own either; it waits for the proof and retries on
a new link (`reference/LXMF/LXMF/LXMRouter.py:2796`,
`reference/LXMF/LXMF/LXMessage.py:616-621`). So the row shows the clock
and stays there: only a proof (`✓✓`) or a refusal (`!`) moves it, while the
router's own retries go on underneath. Lew's test against a third-party
client that receives and never proves turned a delivered message red and
uploaded it to a foreign node before this.

While a send waits, its row names the leg and how long it has been on it,
`me (direct 12 s):` or `me (via propagation node 40 s):`, so a spinner that turns
for a while has its reason on screen. `$LNMSG_HOME/tui.log` holds the same
story in words, at `info` for Loquax's targets (`RUST_LOG` replaces the
filter): one `SEND leg= to= state= elapsed_ms=` line per step of each leg
(`path_wait`, `queued`, `sending`, `sent`, `proof`, `no_proof`, `failed`),
`PATH found to= hops= requests= wait_ms=` or `PATH not_found`, and one
`RPC path_table` line per call to the daemon with its result. Leaving
gives a running send up to a second to read what the engine already said,
so a script that quits on the proof still logs the leg's `sending` and
`proof` (1014); a send still waiting on the network is dropped after it.

A toast too long for one row wraps at the words to up to three rows, the
box growing upward from the footer, and cuts what is left with `…`: a send
that failed both ways names both legs' reasons on an 80-column screen.

The timeline itself:

```
14:02:11  queued
14:02:11  no route known, asked the network
14:02:19  route found, 3 hops
14:02:20  link established
14:02:21  sent, 412 bytes
14:02:26  delivery proof received
```

This is close to free, since the library already emits every one of those
transitions, and it turns "why is this taking so long" from a support
question into something the user can read.

## 3. Proposals, not requirements

Everything in this section is a proposal rather than a decision, and
several of these will not survive contact with a real user.

### Cost before commitment

Show what a message will cost before it is sent, next to the send action:

```
412 bytes   ~7 s airtime at the slowest hop   no stamp required
```

The pieces exist. `leviculum_core::rnode::airtime_ms`
(`leviculum-core/src/rnode.rs:1096`) and `packet_airtime_ms`
(`leviculum-core/src/rnode.rs:1667`) are public, interfaces report a
`bitrate` (`leviculum-std/src/interfaces/mod.rs:761-763`) computed from
spreading factor, coding rate and bandwidth
(`compute_bitrate`, `leviculum-std/src/interfaces/rnode.rs:3452`), and
`fetch_remote_status` (`leviculum-std/src/remote_status.rs:311`) retrieves
the interface list from the daemon, which is how `lnstatus` works. Note two
honesty constraints: `fetch_remote_status` needs the management authkey,
and the status surface reports `bitrate` but not the raw radio parameters,
so an estimate from `bytes * 8 / bitrate` is the best available and must be
labelled as an estimate.

When the peer advertises a stamp cost, the estimate must include the
proof-of-work, and that number has to be *measured* first: the crate
contains no benchmarks, and the cost model (2^cost hashes plus a 3000-round
workblock, `leviculum-lxmf/src/constants.rs:53`,
`leviculum-lxmf/src/stamp.rs:467-475`) predicts scaling but not
milliseconds on a Pi.

### Offline as a state, not a failure

An offline-first client should look deliberate rather than broken. Two
concrete moves:

- A single **posture line** that says what the program can do right now:
  "3 peers reachable directly, propagation node 4m ago, 2 messages waiting to send".
  Not a red error banner; a statement of fact.
- **Queued messages shown in the conversation**, in place, greyed, with
  their reason ("waiting for a route", "computing proof-of-work, about a
  minute"). A message the user wrote should never vanish into a queue they
  cannot see. The library gives progress and next-attempt time per entry
  (`OutboundEntry`, `leviculum-lxmf/src/router/outbound.rs:58-72`).

### The mouse as a first-class citizen

*Built 2026-10-26 for `loquax` (1036); section 1, "Built 2026-10-26, the
mouse", says how.*

`lnomad` enables mouse capture unconditionally with no toggle
(`leviculum-ui-ratatui/src/terminal.rs:74-86`, the capture it takes by
`CrosstermTerminal::new`) and handles neither drag nor selection
(`lnomad/src/app/event.rs:201`). In a browser that costs you the terminal's own
copy-paste; in a messenger, where copying message text is a constant, it is
worse.

Proposal: handle drag selection ourselves over the message IR, so selecting
text across wrapped lines and across message boundaries works and copies
via OSC 52 (`leviculum-ui/src/clipboard.rs:14-42`, lnomad's own until 1036
shared it, which works over SSH). Plus a
`--no-mouse` flag and a runtime toggle for people who want the terminal's
own selection back. The `StyledChar` IR already carries per-cell ownership
(`leviculum-ui/src/style.rs:95-108`), so the hit-testing is a small extension
of `visible_links` rather than new machinery.

### Terminal QR for paper messages and for your own address

`PaperMessage::to_uri()` produces an `lxm://` URI
(`leviculum-lxmf/src/paper.rs:172`) and the crate stops there. A QR code
rendered in Unicode half blocks is a well-trodden trick, and `lnomad`
already has the half-block ladder for images. That gives an air-gapped send
path: compose, render, photograph, and the recipient scans it. Also useful
for showing your own address to someone sitting next to you, which NomadNet
does (`Ctrl-P` in the conversation list).

Constraint: `PAPER_MDU` is 2210 bytes
(`leviculum-lxmf/src/constants.rs:46`), which is near the practical limit of
what a QR code can hold and certainly beyond what a phone camera reads off
a terminal at normal font sizes. The UI must say when a message is too big
to be a QR and offer the URI as text instead.

### A "what changed while I was away" view

On start, after the first sync, one screen summarising what arrived,
grouped by conversation, with the option to mark all read or step through
them. Every mail client has this; no Reticulum client does. It fits the
usage pattern exactly, because the whole point of the propagation node is
that the user was away.

### Micron in messages

If `FIELD_RENDERER` says micron (`reference/LXMF/LXMF/LXMF.py:100`), render
it with `leviculum-micron` (`leviculum-micron/src/lib.rs:33-35`) and
`lnomad`'s renderer. If it says markdown, we already depend on
`pulldown-cmark` at workspace level (`Cargo.toml:82`). This is a capability
the workspace has and nobody has spent, and it costs a match statement.

Sending micron is the more interesting half: a compose box with a preview
toggle, in a client whose sister program is a micron browser.
