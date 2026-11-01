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
  `lnomad/src/app/model.rs:941-978`), extended to conversations. Its best
  property is that it matches either the hint label *or* a substring of the
  target's text (`hint_matches`, `lnomad/src/app/model.rs:968-978`), so `f`
  then typing part of a contact's name jumps to that conversation. That is
  a better contact switcher than anything in either prior-art program.

**What must not be copied**: `lnomad` quits on `Ctrl-C` from any mode,
before the mode dispatch (`lnomad/src/app/event.rs:399-402`). In a messenger
that is a half-written message thrown away by a reflex. `Ctrl-C` in the
compose pane clears the buffer to a recoverable draft; quitting needs
`Ctrl-Q` or the palette.

**The keymap must be one table.** `lnomad` applied single-source-of-truth
discipline to scrolling and nowhere else, and its help overlay's other
groups are hand-typed strings that can drift
(`lnomad/src/app/chrome.rs:426-527`). Every binding here is one table read by
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
(`lnomad/src/app/event.rs:760-763`) generalises here to: **no single keystroke
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
  pins a session (open, type two lines, send, `○` to `✓`, a two-line
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

### Built 2026-10-27, history

1049 (#232) is slice 6 of [keys, mouse and layout](loquax-keys-and-layout.md),
section 7: conversations, their messages with their marks and ledgers, the
pins, the unread counts and the names heard outlive the process in the
SQLite store of [storage](loquax-storage.md). A send cut off by quitting is
`○` again after the restart, with the reason in its ledger. `d` then `y`
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

### Pixel screens: the tokens

The phone and the window draw with one table of colours and sizes,
`loquax/src/ui/chat.rs` (`LIGHT`, `DARK`, `size`). The phone's
`loquax-android/android/res/values*/colors.xml` holds the same colours
name for name, and `the_phone_draws_the_same_colours` refuses a drift.
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
different colours but never words. Three visually distinct marks, and,
crucially, **a per-message detail view** (columba's best idea) showing
delivery method with a one-sentence explanation, attempt count, hop count,
and the error string when there is one. In a terminal that is a key press
on a focused message.

**Never claim a read receipt.** LXMF has no such field
(`leviculum-lxmf/src/constants.rs:60-86`). Any UI element that suggests one
is a lie in the protocol's own terms.

**Say what `Delivered` means, once.** It means the bytes reached the
destination identity, not that a human saw them. The detail view is where
that sentence lives.

**Distinguish the two `Rejected`s** by correlating with the message's
method before the entry disappears. "The propagation node refused this message" and
"the recipient's client refused this message" are different problems with
different fixes.

**Handle the restart discontinuity honestly.** `restore` resets every
queued message to `Outbound` (`leviculum-lxmf/src/router.rs:2111-2114`). So
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
view**, occupying a single cell, carrying the delivery state at a glance.
The glyph is the compact face of the same state machine the detail view
explains; both render from **one shared state-mapping table** (the keymap
discipline above, applied again — the glyph, the detail view and the ledger
can never disagree).

The glyph language, chosen so the colours carry the meaning even before the
shapes are learned:

| state | glyph | colour | motion |
|---|---|---|---|
| queued, nothing transmitted | `○` | dim gray | static |
| computing proof-of-work | braille spinner `⠋⠙⠹⠸⠼⠴` | dim yellow | animated on the tick |
| in flight (link building, bytes out) | braille spinner | yellow | animated on the tick |
| handed to the network, unproven (opportunistic `Sent`) | `◇` | amber | static |
| left at a propagation node (propagated `Sent`) | `⌂` | blue | static |
| arrived (`Delivered`) | `✓` | green | static, terminal |
| refused (`Rejected`, either kind) | `✗` | red | static |
| given up (`Failed`) | `✗` | dim red | static |

The rules the table encodes: **green and a check mark appear only on
proof** — the propagation node's state is a blue house, deliberately not a second
check, because "at a node the recipient may never collect from" must not read as
progress toward delivered; amber `◇` (hollow) against green `✓` (solid)
mirrors unproven-versus-proven; animation means "the machine is working
right now" and nothing else, driven by the
[event-loop tick](loquax-architecture.md) so it freezes honestly if the
program hangs. An ASCII fallback set (`. * o ^ v x`) ships behind the same
table for terminals that mangle the glyphs.

The table's code home is `DELIVERY_GLYPHS` in `leviculum-ui`
(`leviculum-ui/src/delivery.rs`, since 2026-10-25, 984), one row per state
with its glyph, tone, dimming and ASCII fallback; its tests hold the rules
above (green and `✓` only on proof, only the working states animate). The
spinner rows draw the shared loading spinner of `leviculum-ui/src/spinner.rs`,
the one `lnomad` shows, so its frames are the ones the table lists. The
tones are resolved against the palette by the frontend. Since 1042 that
palette is the token table of `leviculum-ui/src/tokens.rs`, a colour per
tone for each theme and tier, measured against our own block's tint and
its selected tint because a mark only ever sits in our own block: the one
dark-pane palette before it put the yellow at 1.7:1 on a white pane. In
16 colours a mark takes its block's text colour, and the glyph alone
carries the state, as it does in monochrome.

`loquax` draws it since 2026-10-25 (994), and maps the router's states
onto the rows in `loquax::ui::delivery_of`: `Generating` and `Outbound` are
queued `○`, `Sending` is in flight, `Sent` is `◇`, `AwaitingCollection` is
`⌂`, `Delivered` is `✓`, `Rejected` is the red `✗`, `Failed` and
`Cancelled` the dim one. The stamping row has no router state yet: the
engine mines without saying so. While a send's runner is still working, a
giving-up state of one of its legs is not drawn, because `--via auto`
cancels a stranded direct leg and carries the same text to a propagation node; the
`✗` appears when the runner reports it gave up.

A direct leg whose message went onto a link to the peer (`Sending`) and
got no proof inside the leg's time is *unproven*, not failed (1002): the
peer answered the link, so it is reachable, and a propagation node's copy would reach
it a second time. The reference never re-sends such a message through a
propagation node on its own either; it waits for the proof and retries on
a new link (`reference/LXMF/LXMF/LXMRouter.py:2796`,
`reference/LXMF/LXMF/LXMessage.py:616-621`). So the row shows `◇` and
stays there: only a proof (`✓`) or a refusal (`✗`) moves it, while the
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
(`leviculum-core/src/rnode.rs:1107`) and `packet_airtime_ms`
(`leviculum-core/src/rnode.rs:1701`) are public, interfaces report a
`bitrate` (`leviculum-std/src/interfaces/mod.rs:716-718`) computed from
spreading factor, coding rate and bandwidth
(`compute_bitrate`, `leviculum-std/src/interfaces/rnode.rs:3441`), and
`fetch_remote_status` (`leviculum-std/src/remote_status.rs:187`) retrieves
the interface list from the daemon, which is how `lnstatus` works. Note two
honesty constraints: `fetch_remote_status` needs the management authkey,
and the status surface reports `bitrate` but not the raw radio parameters,
so an estimate from `bytes * 8 / bitrate` is the best available and must be
labelled as an estimate.

When the peer advertises a stamp cost, the estimate must include the
proof-of-work, and that number has to be *measured* first: the crate
contains no benchmarks, and the cost model (2^cost hashes plus a 3000-round
workblock, `leviculum-lxmf/src/constants.rs:53`,
`leviculum-lxmf/src/stamp.rs:458-466`) predicts scaling but not
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
(`leviculum-ui-ratatui/src/terminal.rs:73-85`, the capture it takes by
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
it with `leviculum-micron` (`leviculum-micron/src/lib.rs:25-27`) and
`lnomad`'s renderer. If it says markdown, we already depend on
`pulldown-cmark` at workspace level (`Cargo.toml:82`). This is a capability
the workspace has and nobody has spent, and it costs a match statement.

Sending micron is the more interesting half: a compose box with a preview
toggle, in a client whose sister program is a micron browser.
