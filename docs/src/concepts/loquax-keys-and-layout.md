# Loquax: keys, mouse and layout

Part of the [Loquax design record](loquax.md). The second decision round on
the screen, after the input model of 2026-08-08 and the delivery display
of 2026-08-10 ([the user interface](loquax-ui.md)). Those stand: focus
follows the pane, a command palette, no modes, `Enter` in compose the one
keystroke that transmits. What this page adds is the full key table, the
mouse, and the layout, and the order they get built in.

**Status: decided 2026-10-26.** The principles in section 1 are Lew's.
The table in section 3, the four changes to slice one it asks for
(section 3, "Four changes to what slice one does") and the Enter rule
(section 3, Compose) were decided by Lew on 2026-10-26 at 18:15, and slice
1 of section 7, the keymap, is built (leviculum 1030). The layout in
section 5 is Lew's (2026-10-26: Signal's shape in text mode, ours right,
theirs left) and slice 2, the chat layout, is built in monochrome
(leviculum 1033; section 5, "Built", names where it differs). Slice 4,
the search, is built (leviculum 1041; section 5, "Built 2026-10-26, the
search", names where it differs). Slice 5, names from announces, is
built (leviculum 1047; section 5, "Built 2026-10-26, names", names where
it differs). The slice
order in section 7 is the plan the later slices are ordered from. The
colour part of section 5 ("Colour") is proposed, not decided.

## 1. The principles (Lew, 2026-10-26)

`lnomad` is operable with the mouse like a classic browser, with the
browser's keyboard, and with the typical Emacs and vi commands, all at
once: one keymap whose rows carry the vi, Emacs and arrow spellings of the
same motion side by side (`SCROLL_ROWS`, `leviculum-ui/src/keymap.rs:666`,
which were `lnomad`'s own `SCROLL_KEYS` until the keymap slice;
the mechanism is described under [architecture](loquax-architecture.md),
section 1). `loquax` takes the principle over, with three substitutions:

- **Not the browser's keys, but the mail programs' and the chat
  programs'**, where a convention exists. A mutt user and a Signal user
  each arrive with habits, and the habits are the reference.
- **Emacs and vi keys are mandatory.** Where a chat convention and an
  Emacs or vi key want the same chord, Emacs or vi wins, and the chat
  action gets another key, a palette name, and a mouse target.
- **The mouse is a first-class citizen**, as the proposal on
  [the user interface](loquax-ui.md) page already asked: everything on
  the screen that looks clickable is.

And the layout: what Signal Desktop and WhatsApp Desktop look like and how
they work, rebuilt in text mode.

The test for the table is that a Signal user, a mutt user and a vi user
each find their habit on the first try, and that the one who does not
find it is told where it went.

## 2. The key families, surveyed

Each program's documented defaults, read from its source or its manual on
2026-10-26.

**mutt**, the default bindings in its `functions.h`
(<https://gitlab.com/muttmua/mutt/-/raw/master/functions.h>, master):
index `j`/`k` and `↓`/`↑` next and previous message, `Enter` (also
`Space`) display, `r` reply, `g` group reply, `L` list reply, `m` mail,
`f` forward, `d` delete, `u` undelete, `s` save, `c` change folder, `/`
search and `n` search next, `Esc /` reverse search, `Home`/`=` and
`End`/`*` first and last, `z`/`Z` page, `Tab` next new-then-unread, `t`
tag, `:` command line, `?` help, `q` quit, `Ctrl-L` redraw. Its line
editor is Emacs-shaped: `Ctrl-A`/`Ctrl-E`, `Ctrl-B`/`Ctrl-F`,
`Alt-B`/`Alt-F`, `Ctrl-D` delete forward, `Ctrl-K` kill to end.

**aerc**, the shipped `config/binds.conf`
(<https://github.com/rjarry/aerc/blob/master/config/binds.conf>, master):
messages `j`/`k`, `Ctrl-D`/`Ctrl-U` half and `Ctrl-F`/`Ctrl-B` full page,
`g`/`G` top and bottom, `Enter` view, `rr` reply all and `Rr` reply,
`C` and `m` compose, `/` search with `n`/`N`, `d` delete after a
confirmation and `D` without, `a` archive, `q` quit, `Ctrl-N`/`Ctrl-P`
next and previous tab.

**Signal Desktop**, the shortcut guide's own table
(`ts/components/ShortcutGuide.dom.tsx` in
<https://github.com/signalapp/Signal-Desktop>, main, last changed
2026-10-13): `Alt-↑`/`Alt-↓` and `Ctrl-Shift-Tab`/`Ctrl-Tab` previous and
next conversation, `Alt-Shift-↑`/`Alt-Shift-↓` previous and next unread,
`Ctrl-1`..`9` conversation by index, `Ctrl-N` new conversation, `Ctrl-F`
search, `Ctrl-Shift-F` search in the conversation, `Ctrl-Shift-T` focus
the composer, `Ctrl-↑`/`Ctrl-↓` scroll to top and bottom, `Enter` the
selected message's default action, `Ctrl-D` its details, `Ctrl-Shift-R`
reply to it, `Ctrl-Shift-D` delete, `Shift-Enter` newline, `Ctrl-Enter`
send in the expanded composer, `Ctrl-U` attach a file, `Ctrl-,`
preferences.

**WhatsApp Desktop** publishes no source, and its help page on
shortcuts did not resolve on 2026-10-26, so it is read from secondary
lists (XDA Developers, "WhatsApp keyboard shortcuts", updated
2023-02-15, <https://www.xda-developers.com/whatsapp-keyboard-shortcuts/>),
and only what those agree on and Signal shares is taken: `Ctrl-N` new
chat, `Ctrl-Tab`/`Ctrl-Shift-Tab` next and previous chat, `Ctrl-F`
search, `Ctrl-Shift-F` search in the chat, `Esc` closes the chat, `Enter`
sends, `Shift-Enter` breaks the line.

**Emacs**, the GNU Emacs reference card
(`etc/refcards/refcard.tex` in
<https://git.savannah.gnu.org/cgit/emacs.git>): `Ctrl-N`/`Ctrl-P` line,
`Ctrl-F`/`Ctrl-B` character, `Alt-F`/`Alt-B` word, `Ctrl-A`/`Ctrl-E` line
start and end, `Alt-<`/`Alt->` buffer start and end, `Ctrl-V`/`Alt-V`
page, `Ctrl-S`/`Ctrl-R` incremental search forward and back, `Ctrl-G`
abort, `Ctrl-L` redraw, `Ctrl-D` delete forward, `Alt-DEL`/`Alt-D` kill
word back and forward, `Ctrl-K` kill to end of line, `Ctrl-Y` yank,
`Alt-W` copy, `Alt-X` run a command by name, `Ctrl-X Ctrl-C` exit.

**vi**, the POSIX specification
(<https://pubs.opengroup.org/onlinepubs/9799919799/utilities/vi.html>):
`j`/`k` line, `Ctrl-F`/`Ctrl-B` page, `Ctrl-D`/`Ctrl-U` half page, `G`
last line, `/` and `?` search forward and back, `n`/`N` repeat in the
same and the opposite direction, `i`/`a` insert and append, `y` yank,
`ZZ` exit, `:` the ex command line. `gg` for the first line is Vim's, not
POSIX's; `lnomad` already binds single `g` for it.

The families agree more than they differ: `j`/`k`, `/`, `n`, `Enter`,
`q`, `?` and `:` mean the same thing in mutt, aerc, vi and `lnomad`, and
the Emacs motions are what mutt's line editor and every readline already
do. The real collisions are between the chat programs and Emacs, because
both want `Ctrl-` letters: `Ctrl-N` (new conversation against next line),
`Ctrl-F` (search against forward), `Ctrl-D` (details against delete or
half page), and between vi's `?` (search back) and everybody else's `?`
(help).

## 3. The Loquax keymap

One row per action. The panes are **L** the conversation list, **M** the
messages, **C** compose, **S** the search field, and **all** means every
pane outside an open overlay (help, palette, detail view). `Alt` is
Emacs's `M-`. A chord marked *(enh.)* exists only where the terminal
speaks the keyboard enhancement protocol, because a legacy terminal sends
the same byte for it as for the plain key (`Shift-Enter` and `Enter`,
`Ctrl-Tab` and `Tab`, `Ctrl-Shift-F` and `Ctrl-F`); every such row has a
spelling that works everywhere. **TX** marks an action that puts bytes
on the air.

### Moving

| Action | vi | Emacs | mail, chat | mouse | where |
|---|---|---|---|---|---|
| next / previous item | `j` / `k` | `Ctrl-n` / `Ctrl-p` | `↓` / `↑` (mutt, aerc `j`/`k`) | wheel | L M; in C the same chords move a line of the body, and `j`/`k` type |
| page down / up | `Ctrl-f` / `Ctrl-b`, `Space` | `Ctrl-v` / `Alt-v` | `PageDown` / `PageUp` (aerc `Ctrl-f`/`Ctrl-b`) | wheel | L M; `PageDown`/`PageUp` also from C, scrolling M |
| half a page | `Ctrl-d` / `Ctrl-u` | | (aerc the same) | | L M |
| first / last | `g` / `G` | `Alt-<` / `Alt->` | `Home` / `End` (mutt); `Ctrl-↑` / `Ctrl-↓` (Signal) | | L M; in C `Alt-<`/`Alt->` are body start and end |
| jump to a hint or a name | `f`, then the label or part of the name | | | | L M (`lnomad`'s hint mode) |

The first four rows are `lnomad`'s `SCROLL_KEYS` rows unchanged, read from
the shared table (section 4); the `Ctrl-↑`/`Ctrl-↓` spelling is the one
chord `loquax` adds to them.

**In the conversation (M) the keys read by the line (1529, Lew on the
M9, 2026-11-07).** Before, a press jumped a message, which skipped the
middle of a long one and landed at its start on the way back. The rule
is a pager with a focus (`step`, `loquax/src/ui/pager.rs`), one place
for the terminal and the M9:

| Key | in M |
|---|---|
| `↓` / `↑`, `j` / `k`, `Ctrl-n` / `Ctrl-p`; the M9's ring up and down | the view a line. The selection stays on a message while it has lines still to come and moves to the next only once its last line is on screen and the next one's first; on the way up a long message is entered at its end, line by line, never at its start |
| `→` / `←` (1529); the page keys, `Space`, `Ctrl-f` / `Ctrl-b`, `Ctrl-v` / `Alt-v`; the M9's ring right and left | a page: a screen less one line of overlap. The selection goes to the first message the page brought lines of (the last, up); a page that reaches the bottom selects the newest and the pane follows it again, one that reaches the top the first |
| `Ctrl-d` / `Ctrl-u` | half a screen, the same selection rule |
| `g` / `G`, `Home` / `End` | the first message at the top, the newest at the bottom |

`Enter` and the message's keys (`I`, `y`, `d`, `e`) act on the selected
message as before. A held key repeats where its source repeats: the
terminal's own key repeat, and on the M9 the keyboard controller's code
answered again on the 10 ms poll, which the driver takes as a press each
time (no timer of the board's). A message counts as read (the receipt's
viewport, `hit::viewport`) once its last line has been on screen, not
when its bubble was selected or half of it showed. The window and the
phone scroll freely and keep their own selection rules; the list (L)
stays a row an entry.

### Conversations

| Action | vi | Emacs | mail, chat | mouse | where |
|---|---|---|---|---|---|
| open the selected conversation | `Enter` | | `Enter` (mutt, aerc, Signal) | click the row | L |
| previous / next conversation | | | `Alt-↑` / `Alt-↓` (Signal); `Ctrl-Shift-Tab` / `Ctrl-Tab` *(enh.)* (Signal, WhatsApp) | click a row | all |
| previous / next unread | | | `Alt-Shift-↑` / `Alt-Shift-↓` (Signal) | click an unread badge | all |
| new conversation | | | `m`, `C` (mutt, aerc) | click `+` in the list's title | L M |
| write in this conversation | `i`, `a` | | `r` (mutt, aerc reply); `Ctrl-Shift-T` *(enh.)* (Signal) | click the compose box | L M |
| search | `/` | `Ctrl-s` | `Ctrl-Shift-f` *(enh.)* (Signal, WhatsApp) | click `/ search` | `/` L M; `Ctrl-s` all |
| next / previous match | `n` / `N` | `Ctrl-s` / `Ctrl-r` | (mutt, aerc `n`) | click a result | S, and L M while a search is open |

### Messages

| Action | vi | Emacs | mail, chat | mouse | where |
|---|---|---|---|---|---|
| open the message: one with a position (a report, or a text with telemetry) the map at its fix, any other its detail view | `Enter` | | `Enter` (mutt display, Signal default action) | click a report or a text with a position; click a delivery mark for the detail | M |
| the message's detail view and ledger, any message | `I` (`v` until 1475) | | palette `details` (Signal `Ctrl-D`, vi's half page here; WhatsApp's Info) | click its delivery mark | M |
| copy the message's text | `y` | `Alt-w` | | drag selects, release copies (OSC 52) | M |
| the copy mode: a cursor on the text that selects as it moves (1475) | `v` | `Ctrl-Space` (the mark) | `Shift` with an arrow | drag | M |
| follow a link in the body | `f`, then its label | | | click the link, as in `lnomad` | M |
| delete from this device | `d`, then confirm | | `d` (mutt; aerc asks first) | | M, from the history slice on |
| react: a quick emoji by its digit, or typed text and `Enter` (1104) | `e` | | palette `react` (the long press in Signal and Columba) | | M |

### Compose

| Action | vi | Emacs | mail, chat | mouse | where |
|---|---|---|---|---|---|
| send **TX** | | | `Enter` (Signal, WhatsApp); `Ctrl-Enter` *(enh.)* (Signal), which never breaks the line | click `send` on the box's border | C |
| line break | | `Ctrl-j` | `Alt-Enter`; `Shift-Enter` *(enh.)* (Signal, WhatsApp) | | C |
| line start / end | | `Ctrl-a` / `Ctrl-e` | `Home` / `End` | click places the cursor | C |
| character, word | | `Ctrl-f` / `Ctrl-b`, `Alt-f` / `Alt-b` | `→` / `←`, `Ctrl-→` / `Ctrl-←` | | C |
| delete forward, word back, word forward | | `Ctrl-d`, `Alt-Backspace`, `Alt-d` | `Delete`, `Ctrl-w` (readline) | | C |
| kill to end, yank | | `Ctrl-k`, `Ctrl-y` | | | C |
| clear to a recoverable draft | | | `Ctrl-c` (decided 2026-08-08; `Ctrl-y` brings it back) | | C |
| leave compose | `Esc` | `Ctrl-g` | `Esc` | click another pane | C |

**The Enter rule** (decided 2026-10-26). With the keyboard enhancement
protocol active, `Shift-Enter` breaks the line and `Ctrl-Enter` sends;
`Ctrl-Enter` never breaks the line, in any setting. Without it, `Alt-Enter`
and `Ctrl-j` break the line. `Ctrl-j` and `Alt-Enter` work in both worlds,
since a terminal that sends them sends them either way. The compose frame's
hint names the chords live in this terminal, `Enter send · Shift-Enter
newline` or `Enter send · Alt-Enter newline`, never both. With
`send_on_enter` off, `Enter` breaks the line and `Alt-Enter` sends (and
`Ctrl-Enter`, which the hint offers where the terminal can send it):
`Ctrl-Enter send · Enter newline` or `Alt-Enter send · Enter newline`.

### Map

Built with the map's slice (c) (leviculum 1065, `loquax-map.md`). **P**
is the map, which takes the conversation pane's place while it is open.
`m` stays the new conversation's; the map is the capital, which no row
held, and `Alt-m`, which no compose edit holds, so it is reachable from
compose too.

| Action | vi | Emacs | mail, chat | mouse | where |
|---|---|---|---|---|---|
| the map | `M` | `Alt-m` | palette `map` | | `M` L M; `Alt-m` all |
| this conversation's peer on the map, at its latest fix | `L` | `Alt-l` | | click the position in the header | `L` L M; `Alt-l` all |
| zoom in / out | | | `+` (`=`) / `-` | wheel | P |
| pan | `h` `j` `k` `l` | | arrows | drag; the wheel tilted left / right an eighth (1102) | P |
| load the map of the visible area (1089) | `d` | | palette `load map area` | | P |
| next / previous way of drawing the map (1093) | `t` / `T` | | `F2` | | P |
| every way of drawing the map, the one now marked | | | palette `map drawing` | | P |
| the glyph test: which glyph sets the font draws | `g` | | | | P |
| every pin in view again | | | `0` | | P |
| next / previous pin | | | `Tab` / `Shift-Tab` | | P |
| open the pin's conversation | | | `Enter` | click the pin | P |
| close the map | `Esc` | `Ctrl-g` | `Esc` | | P |

The palette (`:` and `Alt-x`), the help (`?` and `F1`), `F5`, `Ctrl-l`,
`Ctrl-q` and the conversation steps of **all** reach the map too.

Built with 1076: `Enter` on a position report, or a click on its line,
opens the map centred on that report's fix (a folded line is the latest
report of its run) at the deepest zoom the region under it has, the
sender's pin selected; `Esc` returns to the conversation with the cursor
where it was. The detail view moved to `v`, which opens it for every
message, a report too; `Enter` on any other message still opens it. `L`
is free in L and M (vi's screen-bottom has no meaning in a list of
blocks) and `Alt-l` is held by no compose edit, so the peer's position
is reachable from vi and Emacs hands alike. Since 1132 a text that came
with a telemetry position opens the map the same way, its position line
drawn under the text.

### Everywhere

| Action | vi | Emacs | mail, chat | mouse | where |
|---|---|---|---|---|---|
| next / previous pane | | | `Tab` / `Shift-Tab` | click the pane | all |
| back out, cancel, close | `Esc` | `Ctrl-g` | `Esc` (WhatsApp closes the chat) | click outside the overlay | all: C to M to L, and any overlay closes |
| command palette | `:` | `Alt-x` | | | `:` L M; `Alt-x` all |
| help | `?` | | `F1` | click `? help` in the footer | `?` L M; `F1` all |
| check the propagation node **TX** | | | `F5`; `Ctrl-r` (`lnomad`'s reload) | click the propagation node's state in the header | `F5` all; `Ctrl-r` L M |
| redraw | | `Ctrl-l` | (mutt `Ctrl-l`) | | all |
| quit | `:q`, `q` | `Ctrl-x Ctrl-c` | `Ctrl-q` | click `quit` in the footer | `q` L M; the rest all |
| mouse capture off and on | | | palette `mouse`, `--no-mouse` | | all |

### How the collisions are settled

Per pane, in this order, and every loser keeps a palette name and a
mouse target:

- **In compose, plain keys type**, and so do `Shift`-letters. Every
  single-letter row above is L and M only.
- **In compose, Emacs editing wins every `Ctrl` letter it has**: `Ctrl-n`
  and `Ctrl-p` move a line, `Ctrl-f`/`Ctrl-b` a character, `Ctrl-d`
  deletes, `Ctrl-k`/`Ctrl-y` kill and yank, as 994 built the last two. The
  palette from compose is `Alt-x`, or `Esc` and then `:`.
- **In the command panes, the shared scroll rows win**, because they are
  `lnomad`'s and a reader of both programs must not learn two meanings:
  `Ctrl-f` pages and `Ctrl-d` half-pages there, as in vi and aerc, and
  `Ctrl-p` moves up a line. This page first also offered `Ctrl-p` as the
  palette in L M (the editors' palette); that row shadowed the scroll row,
  the keymap slice's shadowing test refused it, and the palette is `:` and
  `Alt-x`.
- **The chat programs' `Ctrl` letters lose to both**: Signal's and
  WhatsApp's `Ctrl-N` (new conversation) is Emacs's next line, their
  `Ctrl-F` (search) is a page in L M and a character in C, Signal's
  `Ctrl-D` (details) is vi's half page. They go to `m`/`C`, `/`/`Ctrl-s`
  and `Enter`. The chords that do not collide are taken as they are:
  `Alt-↑`/`Alt-↓`, `Alt-Shift-↑`/`Alt-Shift-↓`, `Ctrl-↑`/`Ctrl-↓`,
  `Shift-Enter`, `Ctrl-Enter`, `Ctrl-Tab`, `Ctrl-Shift-F`, `Esc`.
- **vi's `?` is help**, as in `lnomad`, mutt and aerc; backward search is
  `/` and then `N`.
- **mutt's `Tab`** (next unread) stays the pane cycle decided on
  2026-08-08; next unread is Signal's `Alt-Shift-↓`. mutt's `g` (group
  reply) and `c` (change folder) have no counterpart, since LXMF has no
  groups and `loquax` no folders, so `g` stays vi's top.
- **The quiet keyboard holds**: `Enter` in compose is the only single
  keystroke marked TX. Fetching needs `F5`, a `Ctrl` chord or a click, and
  `lnomad`'s bare `R` reload is deliberately not taken over, because in
  `loquax` it would be a single keystroke that transmits.

A lost habit is not silent: the help overlay lists, under each row, the
chat program's key that went elsewhere ("`Ctrl-N` in Signal: here `m`"),
and the palette finds the action by the chat program's word too (`new
chat`, `find`).

**`:q` quits for free.** The palette's fuzzy match is a subsequence test
(`fuzzy_match`, `leviculum-ui/src/commands.rs:256`), and `quit` is the only
palette name containing a `q`, so `:`, `q`, `Enter` quits as a vi user
expects. That stays true only while it is tested, so the keymap slice
pins it.

### Four changes to what slice one does

The table asked four things of slice one (loquax-ui.md, "Built 2026-10-25,
slice one"). **Decided 2026-10-26** (Lew, 18:15), all four, and built with
the keymap slice (leviculum 1030), each pinned by a test in
`loquax/src/ui.rs`:

1. **`Enter` on a message opens its detail view**, as in mutt and Signal;
   before, it moved to compose. Compose is `i`, `a`, `r`, `Tab` or a click.
   Until the detail view of slice 2 exists, `Enter` shows what the session
   knows of the message in a one-line toast (who wrote it, its delivery
   state in words, the first bytes of its id).
   (`enter_on_a_message_shows_its_details_and_i_a_r_compose`)
2. **`Ctrl-p` and `Ctrl-n` in compose are Emacs line motions**, now that a
   body has lines (1002); before, `Ctrl-P` opened the palette from every
   pane. The palette from compose is `Alt-x`.
   (`ctrl_p_is_a_motion_everywhere_and_never_the_palette`)
3. **`Ctrl-c` quits nowhere.** Before, it quit outside compose; the reflex
   that throws a draft away in compose copies or quits elsewhere by habit,
   and a messenger that vanishes on a copy reflex loses the screen.
   Pressed outside compose it shows a toast, `Ctrl-q quits`.
   (`ctrl_c_quits_nowhere_and_says_ctrl_q_does`)
4. **The `send_on_enter` switch's email style swaps `Enter` and
   `Alt-Enter`** (`Enter` breaks the line, `Alt-Enter` sends) instead of
   sending on `Ctrl-D`, which Emacs needs as delete forward. The switch is
   the `send_on_enter` key of Loquax's config and the palette's `send on
   enter`, on by default. (`send_on_enter_off_swaps_enter_and_alt_enter`)

### What the terminal can and cannot send

- `Ctrl-j` is the line break every terminal can send: in raw mode a
  terminal's `0x0A` reaches crossterm 0.29 as `Ctrl-J`, not as `Enter`
  (`src/event/sys/unix/parse.rs` in crossterm, branch 0.29: the `b'\n'`
  arm applies only outside raw mode), so it needs no enhancement. Before
  the keymap slice no row bound it, so it did nothing.
- `Alt-f`, `Alt-b` and `Alt-d` did not edit before the keymap slice. The
  editing table (`input_request`, `leviculum-ui/src/input.rs`) accepted
  them with `META` only, which a terminal reports under the enhancement
  protocol and not otherwise, while the frontend passes crossterm's `ALT`
  through as `ALT`; and it mapped `Meta-d` to deleting the word *back*,
  where Emacs kills the word forward. `Ctrl-d` (delete forward) was not in
  it at all. The keymap slice fixed the three in the shared table, so
  `lnomad`'s address bar has them too.
- `Ctrl-a` and `Home` went to the start of the whole body, not of its
  line, once the body became multi-line (`GoToStart` over one buffer). A
  buffer of lines now edits through `line_request`, where `Ctrl-a`,
  `Ctrl-e`, `Home` and `End` are line motions, `↑`, `↓`, `Ctrl-p` and
  `Ctrl-n` move a line keeping the column, and `Alt-<`/`Alt->` are the
  body's start and end. On one line that is the plain table, so
  `lnomad`'s single-line fields are unchanged.
- `Ctrl-s` reaches the program: raw mode clears `IXON`, so the terminal
  does not take it as flow control.
- The enhancement rows need `PushKeyboardEnhancementFlags` where crossterm
  reports support. `loquax` pushes `DISAMBIGUATE_ESCAPE_CODES` where
  `supports_keyboard_enhancement` says so, pops it on every way out
  (the terminal guard and the panic hook), and writes the live chords once
  to `tui.log`: `KEYS enhancement=yes newline=Shift-Enter send=Enter`.
  Without support nothing changes. A key script can spell `shift-enter`
  and `ctrl-enter` whatever the host terminal is, and `keyboard enhanced`
  tells the screen which world it is in, so `tui_snapshot` pins both.

## 4. One table, four readers

The decision of 2026-08-08 asked for the keymap as one table read by the
key handler, the help overlay, the palette and the footer hints. Before
the keymap slice it was not: `loquax`'s keys were a `match` in `on_key` and
`command_pane_key` (`loquax/src/ui.rs`) beside a hand-written help text, and
`lnomad`'s `SCROLL_KEYS` was private to `lnomad` (`pub(crate)`, in
`lnomad/src/app/event.rs`) and typed over crossterm's `KeyCode`, so `loquax`
could not read it. Its rows also carried the keys twice, once as the
displayed `keys` string and once as the `chords`, so the help could still
drift from the handler inside one row.

The keymap slice therefore does three things in `leviculum-ui`, which is
terminal-free and already owns the key types (`leviculum-ui/src/input.rs`)
and the palette names (`leviculum-ui/src/commands.rs`):

1. **The scroll rows move there**, over `leviculum_ui::input::KeyCode`, and
   `lnomad` reads them from there; its help overlay and its key handler do
   not change behaviour, which `lnomad`'s own tests and its golden files
   hold.
2. **A row is one action with its chords, and the displayed keys are
   derived from the chords**, never typed beside them. A chord carries the
   panes it applies in, so the per-pane rules of section 3 are data and
   not `if` order.
3. **`loquax` adds its rows** beside the shared ones: the conversation,
   message, compose and global rows of section 3, each with its palette
   name (the `Command` enum and `COMMANDS` grow to the full list), its
   footer label if it has one (`FooterItem`, `leviculum-ui/src/footer.rs`),
   its help group, the lost-habit note, and the TX flag.

Readers: the handler resolves a key by scanning the rows for the focused
pane; the help overlay is built from the rows (`HelpGroup`,
`leviculum-ui/src/help.rs`); the footer takes the rows that carry a label
for the focused pane; the palette lists every row's name and shows its
first chord. The desktop GUI reads the same table.
The keymap and the palette are shared with lnflash's GUI once it exists
(Codeberg #476), so a flashing action is a palette entry there, not a
new grammar.

Built so in 1030: the rows are `leviculum-ui/src/keymap.rs` (`Row`,
`Chord`, `SCROLL_ROWS`) and `loquax/src/keys.rs` (`ROWS`). A row reads

```rust
Row::new("send", COMPOSE_GROUP, &[
    &[key(Enter, NONE, run(C::Send), COMPOSE).unless(ENTER_BREAKS)],
    &[key(Enter, CTRL, run(C::Send), COMPOSE).enhanced()],
    &[key(Enter, ALT, run(C::Send), COMPOSE).when(ENTER_BREAKS)],
]).palette("send").footer("send").tx()
```

and its help keys, `Enter   Ctrl-Enter   Alt-Enter`, are built from those
chords at compile time. A key resolves to the most specific chord (exact
modifiers over a class over any), the first among equals, which for the
scroll rows is `lnomad`'s old first-match scan, so its behaviour and its
golden files did not move. Three scroll spellings `lnomad`'s help never
listed (`Space`, `Alt-v`, `Alt-<`/`Alt->`) are quiet chords: they work and
stay out of the displayed keys, so `lnomad`'s overlay keeps its width.

What makes it testable, and what the slice's unit tests hold
(`loquax/src/keys.rs`, the tests module; the shared rows' own in
`leviculum-ui/src/keymap.rs`):

- no chord resolves to two actions in one pane (no shadowing, which
  `lnomad`'s first-match scan cannot see);
- every row appears in help, and every palette name belongs to a row;
- the quiet keyboard: the only row with the TX flag and a single
  keystroke is send-in-compose (the `Enter` chord);
- every *(enh.)* chord's row has a chord that is not *(enh.)*;
- `:` `q` `Enter` quits.

## 5. The layout: a modern chat app in text mode

Signal Desktop and WhatsApp Desktop share one shape: a conversation list
on the left with name, last-line preview, time and an unread badge, the
open conversation on the right with a header naming the peer, messages as
blocks with ours on the right and theirs on the left, a time and a
delivery mark on each, a date between days, a "new messages" line where
the unread begins, and a compose box at the bottom that grows with the
text. Here it is in cells.

### 80 x 24

```text
┌─ Chats ────────────────┐┌─ Alice ·4f2a  known ────────────────── pn ⌂ 4 min ─┐
│/ search                ││                                                    │
│                        ││                                                    │
│▌Alice ·4f2a      14:05 ││             ── Saturday 24 October ──              │
│▌ok, bring the T114.    ││ are you at the field site tomorrow?  13:58         │
│ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ││                                                    │
│ Bob ·b7d0        12:40 ││                yes, from ten. Shall I bring the    │
│ you: the antenna is…   ││               T114 and the long antenna?  14:02 ✓✓ │
│                        ││                                                    │
│ <9c31e0a2…>        Fri ││                    ── Today ──                     │
│ hello? is this th… [1] ││                                on my way  09:12 ✓  │
│                        ││                                                    │
│ Carol ·17ee     22 Oct ││ ──────────────────── 2 new ─────────────────────── │
│ you: thanks, merged    ││ ok, bring the T114.  14:04                         │
│                        ││                                                    │
│                        ││ and the small tripod                               │
│                        ││ if it fits in the bag  14:05                       │
│                        ││                                                    │
│                        ││                           the tripod fits  14:06 ⠹ │
│                        │├─ to Alice ──────── Enter send · Alt-Enter newline ─┤
│                        ││ sure, and I will charge                            │
│                        ││ the batteries tonight▏                             │
└────────────────────────┘└────────────────────────────────────────────────────┘
 Tab focus  Alt-↑↓ chat  / search  : commands  ? help  Ctrl-Q quit              
```

### 120 x 40, with a search open

```text
┌─ Search ──────────── esc closes ─┐┌─ Alice ·4f2a  known ────────────────────────── 4f2a91c03be8d7e15a60f2b44c9d0e71 ─┐
│ / tripod▏                        ││ direct, 2 hops · heard 14:05                   relay Bern adopted · synced 14:03 │
│                                  ││ ──────────────────────────────────────────────────────────────────────────────── │
│ CONVERSATIONS                  2 ││                                                                                  │
│  Alice ·4f2a               14:06 ││                                                                                  │
│  you: the «tripod» fits          ││                                                                                  │
│                                  ││                                                                                  │
│  Bob ·b7d0                 3 Oct ││                                                                                  │
│  «tripod» stays in the van       ││                                                                                  │
│                                  ││                                                                                  │
│ MESSAGES                       3 ││                          nothing earlier on this device                          │
│ ▌Alice                     14:05 ││                             ── Friday 23 October ──                              │
│ ▌and the small «tripod» if it…   ││ did the board come back from the soak?  19:40                                    │
│                                  ││                         yes, 100 % over the night, log is on the share  19:52 ✓✓ │
│  you to Alice              14:06 ││                                                                                  │
│  the «tripod» fits               ││                            ── Saturday 24 October ──                             │
│                                  ││ are you at the field site tomorrow? Dana says the north                          │
│  Bob                       3 Oct ││ gate is locked after six, so the car park by the road it is.  13:58              │
│  the «tripod» stays in the van,  ││                                                                                  │
│                                  ││                           yes, from ten. Shall I bring the T114 and the long     │
│                                  ││                          antenna, or is the stick enough for the hill?  14:02 ✓✓ │
│                                  ││                                                                                  │
│                                  ││ long one, the hill eats signal  14:03                                            │
│                                  ││                                                                                  │
│                                  ││                                   ── Today ──                                    │
│                                  ││                                                              on my way  09:12 ✓  │
│                                  ││                                            stuck at the level crossing  09:31 ◷  │
│                                  ││                                                                                  │
│                                  ││ ────────────────────────────── 2 new ─────────────────────────────────────────── │
│                                  ││ ok, bring the T114.  14:04                                                       │
│                                  ││                                                                                  │
│                                  ││ and the small «tripod»                                                           │
│                                  ││ if it fits in the bag  14:05                                                     │
│                                  ││                                                                                  │
│                                  ││                                                      the «tripod» fits  14:06 ✓✓ │
│                                  ││                                                         leaving in ten  14:07 ◷  │
│                                  │├─ to Alice ─────────────────────── Enter send · Alt-Enter newline · M-x commands ─┤
│                                  ││ ▏                                                                                │
└──────────────────────────────────┘└──────────────────────────────────────────────────────────────────────────────────┘
 Enter open  n / N next, previous  Ctrl-S / Ctrl-R next, previous  Tab to the results  Esc close search                 
```

### The elements

**The conversation list.** Two rows a conversation: the name with its
short hash and the time of the last message, then the last line's preview,
prefixed `you:` when it was ours, and an unread count in brackets, drawn
reversed. The name follows the display rule of
[the mailbox page](loquax-mailbox.md) (petname, else announced name,
always with a short hash); until names arrive it is the address in angle
brackets. Most recent first. Pinned conversations, if we want them, sit
on top above a dotted rule; pinning is a palette action (`pin`), stored
with the conversation, and needs the history slice. The selected row has
the bar in the left margin; under focus it is drawn reversed.

**The header** is the open conversation's title line: the peer's name and
short hash, its trust state in words (`known`, `unknown`, `blocked`, the
three of the propagation node page, never a glyph alone), and on the right the
propagation node's state, the same words the `propagation node:` line prints
(`loquax/src/mailbox_state.rs`). At 120 columns it gains the full address
in the title and a second row: how the peer is reached (`direct, 2 hops`,
from the path table) and when it was last heard, and the propagation node,
selection and last sync.

**The conversation pane.** Ours are blocks set flush right, theirs flush
left, a block at most three quarters of the pane wide so the two sides
read apart; the text inside a block is left-aligned and wrapped at a
space. A multi-line body is drawn as its lines, each wrapped, as one
block. The time and, for ours, the delivery glyph of
[the delivery table](loquax-ui.md) close the block's last line, so a mark
always sits at the same place relative to its text. Between days a
centred date (`Today`, then weekday and date). Where the unread begins, a
full-width rule with the count (`2 new`), which stays until the
conversation is left. Below a pane width of 40 cells the right alignment
stops paying for itself, and ours are drawn flush left with `you` before
the time instead. The top of what the device holds says so (`nothing
earlier on this device`), because until the history slice that is the
start of this session.

**The compose box** sits under the messages with the conversation's name
on its border and the hint for send and line break on the right. It shows
as many rows as the body has lines, up to five, then scrolls (1002). The
attachment and the QR code of the paper message are palette actions
(`attach`, `qr`) until they earn a key.

**Search** is the list pane's top row. `/` or `Ctrl-s` puts the cursor
there; as the query is typed the list becomes the results, conversations
whose name matches and then messages whose text does, each with the match
marked. Moving through the results opens the conversation on the right,
scrolled to the hit, with the hit marked in the block; `Enter` keeps it
open and closes the search, `Esc` closes the search and returns to where
the reader was. Until the history slice it searches what this session
holds; with history it is the FTS5 index of [storage](loquax-storage.md).

**The footer** is the shared footer strip (`leviculum-ui/src/footer.rs`),
filled from the keymap rows that carry a label for the focused pane, so
it changes with the focus and drops low-priority hints first on a narrow
screen.

### Built 2026-10-26 (leviculum 1033)

The list, the header, the blocks, the date lines, the `new` rule, the
narrow fallback, the top line and the compose box are drawn as above
(d13c1a6d, 7b72a2ef, 27e356f0), in `loquax/src/ui/layout.rs` from the
model, so the model knows the screen's size and the time of day. The
frames are pinned at both mockup sizes: from the model in
`loquax/tests/fixtures/tui_layout.frames`, from a real exchange against a
daemon in `tui_chat.frames`. Where they differ from the mockups:

- **No search row and no pinned rule.** The list starts at its first
  conversation. Pinning needs history (slice 6); the search came with
  slice 4 and its way in is on the list's bottom border ("Built
  2026-10-26, the search", below).
- **Names are the address**, `<9c31e0a2…>`, everywhere a name stands, and
  every peer is `unknown`, until slice 5 (built since, "Built 2026-10-26,
  names", below).
- **The list's title is our own address** (`Loquax afbda7b3`), as before,
  not `Chats`.
- **The header's second row at 120 columns has the propagation node half only**
  (`relay Bern adopted · synced 14:03`, the relay by the name it announces, `a relay` without one, never its hash (1587), right-aligned, with the rule
  under it). How the peer is reached and when it was last heard need the
  path table read for the screen, which nothing does yet. At 80 columns
  the propagation node's state is words, the longest form that fits, never the `⌂`
  and an age.
- **The time may reach past the three-quarter cap** when the block's last
  line has no room for it inside: the text wraps at the cap, the time and
  the mark take the pane's last quarter if they fit there, else a row of
  their own. Both mockups already do this.
- **A send still waiting names its leg** before the time
  (`are you there  direct 2 s · 12:00 ◷`), and a received message whose
  signature could not be checked yet, its sender's key unknown, says
  `unverified` after its time, until the key arrives and the block changes
  in place; one whose signature does not hold says `INVALID signature`.
- **The focus mark** `●` before a pane's title stays, the mockups leave it
  out. The footer is the keymap's (slice 1), not the mockup's text.
- **Monochrome.** The selected block is reversed behind a `▌` bar while
  the conversation has the focus, the list's selected row likewise, the
  badge always; no colour token is drawn, because "Colour" below is
  proposed and not decided. The delivery marks keep the tones of
  2026-08-10.
- **Times are the moment this session saw the message**, sent from the
  compose line or arrived, not the LXMF timestamp the sender put in it:
  until the history slice the device holds no other time, and a peer's
  clock is not ours to show. A key script pins the clock with a `clock`
  step.

### Built 2026-10-26, the search (leviculum 1041)

The search is built (c065767c, 7a4f57f8) in `loquax/src/ui/search.rs`:
the matcher, a case-insensitive substring test over the conversation names
(the short-hash form until slice 5) and this session's message bodies,
and the list pane it turns into. `/` in the list and the messages,
`Ctrl-s` everywhere and `Ctrl-Shift-F` under the enhancement protocol
open it with the cursor in its field; every keystroke there runs the
matcher again, and an empty field shows the plain list under it. The
results are two groups, `CONVERSATIONS` and `MESSAGES` with their counts,
each most recent first, two rows a result: who and when, then the preview
with the first match between `«` and `»`, cut from the left where the
match lies past the row. `Tab` goes from the field to the results, `Enter`
or a click opens one, and while the search is open `n`/`N` and
`Ctrl-s`/`Ctrl-r` step the matches: rows in the field and the results,
messages of the open conversation in the conversation, round its ends with
a toast. `Esc` from the field or the results closes it. The frames are
pinned from the model at both mockup sizes in
`loquax/tests/fixtures/tui_search.frames` and from a session against a
daemon in `tui_search_session.frames`. Where it differs from the mockup
and the element text above:

- **`/ search` is on the list's bottom border**, right-aligned, not the
  list's top row: at 80 columns the title has no room for it beside our
  own address, and a row would cost the list a row. A click on it opens
  the search. While the search is open the pane's title is `Search` with
  `esc closes` on its border, as the mockup has it, and the hint is gone.
- **A conversation matches by its name only.** The mockup's
  `CONVERSATIONS` rows show the term in their last line, which a name
  match does not give; a conversation result marks the match in its name
  (`<«4f4f»4f4f…>`) over its last line unmarked. A message whose
  conversation also matches by name is in both groups.
- **Moving through the results does not open the conversation**; `Enter`
  or a click does, and the search stays open, so `n` goes on in the
  opened conversation. `Esc` there goes back to the results, as it backs
  out of every pane, and `Esc` in the results or the field closes the
  search; the conversation keeps what it shows.
- **Every matching block is marked in place**, its first match, while the
  search is open: the marks are part of the drawn text, so a drag over a
  marked block copies them with it.
- **The footer is the table's, per focus.** In the field it is `Enter
  open`, `Ctrl-s / Ctrl-r next, previous`, `Tab to the results`, `Esc
  close search`, then the palette, the help and `Ctrl-q quit` as room
  allows; in the results `n / N next, previous` joins it. A row shows a
  pair of directions as one button (`footer_spelled`,
  `leviculum-ui/src/keymap.rs`); a second live spelling and `Tab to the
  results` are the first to drop, so at 80 columns the field's footer is
  `Enter open │ Ctrl-s / Ctrl-r next, previous │ Esc close search │
  Ctrl-q quit`.
- **`Ctrl-r` fetches only outside a search**, because inside one it is the
  previous match; `F5` fetches always. `Ctrl-s` in compose goes back to
  the field, and steps nothing.
- **Times are this session's**, as everywhere on the screen, and the
  header's path half is still missing (section 5, "Built", above).

### Built 2026-10-26, names (leviculum 1047)

Names are built in `loquax/src/names.rs`, the model, and drawn everywhere a
peer stood as its address: the list, the header, the compose box's
border, the detail view, the toasts and the search. The engine forwards
each `RouterEvent::PeerAnnounced` as `OutboxEvent::Announced` with the
display name as the reference reads it from the app data
(`LXMF.display_name_from_app_data`, `reference/LXMF/LXMF/LXMF.py:151-172`:
the msgpack list of LXMF 0.5 and later, with or without a stamp cost, and
the bare bytes before it); the screen sanitises it. The frames are pinned
from a session against a daemon in `loquax/tests/fixtures/tui_names.frames`
(three correspondents: one announced name, a look-alike of it, and a peer
the contacts file names), and the sessions that already had an announcing
correspondent show its name since. Where it differs from the text above and
the propagation node page's decision, or fills a gap they leave:

- **The display rule** is the propagation node page's: the petname, else the
  announced name, then a space and the address's first four hex digits
  after a `·` (`Alice ·4f2a`); with neither, the address form
  `<9c31e0a2…>` as before. A name is cut before its hash, so the hash
  always stays. *Superseded 2026-11-08 (1452): the name alone, the tail
  only on a collision, "Unknown" instead of the address form, and no `≈`;
  see [the user interface page](loquax-ui.md), "Built 2026-10-26, names".*
- **Sanitising** removes control characters, the bidi embeddings,
  overrides, isolates and marks, and the invisible code points (Unicode's
  default-ignorables: zero-width characters, the soft hyphen, the Hangul
  fillers, the variation selectors, the tags); folds every run of white
  space to one space; replaces with `?` a glyph the screen draws as a mark
  of its own (the delivery marks from the shared table, check marks, the
  spinner's braille, `·` and its look-alikes, `…`, `«` `»`, `≈`, box
  drawing and block elements); and caps the width at 32 cells, a wide
  character two, cut with `…`. The pages name a cap and no number; 32 keeps
  a first and last name. The pages also say "normalise": no Unicode
  normalisation crate is in the tree, so a name is drawn as it came and the
  look-alike folding happens only where names are compared.
- **Look-alikes** are compared, not rewritten: two names collide when they
  have one skeleton (lower case, full-width forms as ASCII, the Cyrillic,
  Greek and digit letters that draw like a Latin one folded onto it, and
  `i`, `l`, `I`, `1`, `|` one letter). Unicode's full confusables table is
  not carried. "Ian" and "lan" collide; a mark can afford that.
- **The collision mark** is `≈` after the hash, on both peers, wherever the
  label stands, and the header says `same name as ·b7d0`. The pages say
  "mark it" and name no glyph. It compares the shown names, so a stranger
  announcing a contact's petname is marked with the contact.
- **The name-change mark** is a centred line in the conversation,
  `── was Alice, now announces Ally ──`, where it happened, said once a
  change and only where there is a conversation with the peer: the pages'
  "an address you have talked to", read as a conversation this session
  holds. An announce with no name keeps the name heard before.
- **Petnames and blocks come from a file**, `${LNMSG_HOME}/contacts`, one
  `name <address> <petname>` or `block <address>` a line; a wrong line
  stops the start with the file and line, as `config` does. The pages say
  trust follows from naming someone and do not say where the naming
  happens. (The palette writes it since 1082: see the propagation node page,
  section 2, "Blocking, and names from the palette".)
- **Trust** is the three words of the propagation node page in the header:
  `known` with a petname, `blocked` with a block (over a petname),
  `unknown` else; since 1279 (the ninety percent walk, finding 21) the
  default `unknown` is not drawn, and the header says nothing of a missing
  propagation node until a send needs one (`no relay`). Beside a petname that differs from what the peer
  announces the header adds `announces Ally`, the provenance of option C.
  `blocked` was only said until Lew decided what it does on 2026-10-28
  (1082): stored silently, see the propagation node page, section 2.
- **Names are this session's.** The store across restarts is the history
  slice; a peer heard in an earlier run is its address until it announces
  again. (Kept across restarts since 1049, "Built 2026-10-27, history".)
- **Our own announce**, if it comes back, names nobody.
- **A script waits for names** with `until named <n> [secs]`.

### Built 2026-10-27, history (leviculum 1049)

The history is built: the store in `loquax/src/store.rs` (SQLite with its
FTS5 index, `${LNMSG_HOME}/history.sqlite3`, as
[storage](loquax-storage.md) decided), the model's side in
`loquax/src/ui/history.rs`. The model notes what each fold changed and the
terminal's shell writes it in one transaction before the next event; the
engine, which runs under the core lock and does no I/O, writes nothing. At
start every conversation in the store is listed, `--to` or not, with its
newest messages, its marks, its ledger, its pin and its unread count, and
the names peers announced. The frames are pinned from three runs over one
home in `loquax/tests/fixtures/tui_history.frames`: an exchange with Bob
and a send to Carol cut off by quitting; a restart that shows both from the
store, Bob by name, the proof `✓✓`, the cut-off send `◷` with the restart in
its ledger, then pins Bob and deletes his reply; and a third start that
shows the pin and the deletion kept. Where it differs from the text above,
or fills a gap it leaves:

- **The top line is the store's start**: `nothing earlier on this device`
  where a conversation's first stored message is loaded. A conversation
  loads its newest 200 messages at start (`LOAD_WINDOW`); above them the
  top line says `180 earlier on this device, scroll up for them`, and
  reaching the top, by key or wheel, loads 200 more above, the cursor and
  the view staying on what the reader was reading. The pages say "the last
  screens" and name no number.
- **The restart discontinuity** is the page's: a send still queued or in
  flight at exit is `◷` after the restart, with `Loquax restarted: no
  longer being sent` in its ledger at the time of the start. Nothing sends
  it again, because nothing holds the router's queue across a restart
  (`loquax/src/engine.rs`, "What it deliberately does not do"); no screen
  line marks the restart beyond that. A proof or a refusal that came before
  the exit is kept as it was.
- **The ledger spans runs**: the detail view's heading says `on this
  device`, no longer `this session`.
- **The search keeps its matcher** for what the screen has loaded, so a
  loaded message matches exactly as before; for messages older than a
  conversation's loaded window the store's FTS5 index answers (trigram
  tokens, a substring of three characters or more; a shorter query scans
  with `LIKE`, whose case folding is ASCII only), and those results join
  the messages group by age. Opening one loads its conversation down to it
  and opens it there.
- **Delete** is `d` in the messages, then `y`; any other key keeps the
  message and says so, and the question stays up until answered, unlike a
  toast. A message the send runner still works on is not deleted: it says
  to wait until it settles, because the runner holds its place. A
  conversation is deleted from the palette (`delete conversation`), after
  the same question; it leaves the list, and comes back if the peer writes
  again.
- **Pin** is the palette's `pin`, a toggle. Pinned conversations stand
  first, most recent first among themselves, and the dotted rule takes the
  blank row under the last of them when an unpinned one follows. No key and
  no mouse target, as the element text has it.
- **The palette ranks** a name typed whole and then one it begins before
  the scattered matches (`leviculum-ui/src/commands.rs:268`); without it
  `:pin` and `Enter` ran `previous conversation`, whose letters hold
  `p`, `i`, `n` in order.
- **The unread count** and the `new` rule survive a restart: a conversation
  that had unread messages shows its badge, and its rule above them when
  opened.
- **The command line writes no history**: `loquax send` and `loquax fetch`
  print and exit as before. A message a `fetch` from the command line
  drained is therefore never in the history.

### Colour

**Status: Decided 2026-10-26 (Lew), built 53b6c159 and ddff9dd2
(leviculum 1042).** Lew asked on 2026-10-26 whether background and
foreground colours can carry the two sides in a terminal the way Signal
and WhatsApp do on the phone. They can. This part says how; Lew took all
five proposals of part 6 as they stood, and "Built" after it names where
the build differs from the text. The table is `leviculum-ui/src/tokens.rs`
(`tokens`, `leviculum-ui/src/tokens.rs:304`).

The model is the desktop apps': Signal Desktop and WhatsApp Desktop tint
our own blocks in the app's accent and draw the other side's in a neutral
grey, keep the text inside both at full contrast, set the date as a small
pill on a tint of its own, and draw the unread line in an accent. Each of
those is a token below.

#### 1. Three tiers, one table

One table of tokens, each with a value for a dark and for a light
terminal in each of three colour tiers. Foreground over background where
a token sets both; a token that sets only a foreground is drawn on the
pane. The pane itself is never painted: it stays the terminal's own
background and foreground, as `lnomad` leaves its content
(`leviculum-ui/src/theme.rs`, module doc), so a reader's terminal theme
still shows around the blocks.

| Token | truecolor dark | truecolor light | 256 dark | 256 light | 16 dark | 16 light |
|---|---|---|---|---|---|---|
| pane, text on pane | unset | unset | unset | unset | unset | unset |
| muted on pane (`you`, the `nothing earlier` line, separator rules) | `#9aa1ad` | `#5f6672` | 246 | 242 | 7 | unset |
| own block, text | `#f2f5fa` on `#1d3f6e` | `#0b1a2e` on `#d3e3fd` | 255 on 24 | 16 on 189 | 15 on 4 | 0 on 6 |
| own block, time | `#a9c1e6` | `#3b5578` | 153 | 238 | 15 | 0 |
| their block, text | `#e6e8ec` on `#2f333b` | `#1a1c20` on `#ebedf0` | 255 on 236 | 235 on 254 | unset | 0 on 7 |
| their block, time | `#aab0ba` | `#565c66` | 248 | 241 | 7 | 0 |
| own block, selected | `#ffffff` on `#285a9e` | `#0b1a2e` on `#b3cdf6` | 231 on 25 | 16 on 153 | reverse | reverse |
| their block, selected | `#ffffff` on `#454b56` | `#1a1c20` on `#d3d8e0` | 231 on 239 | 235 on 252 | reverse | reverse |
| date pill | `#d0d4dc` on `#3a3f4a` | `#2b2f36` on `#e4e7ec` | 252 on 238 | 237 on 253 | unset | 0 on 7 |
| unread rule and count | `#f0a030` | `#a34f00` | 214 | 94 | 11 | 4 |
| toast | `#e6e8ec` on `#3a3f4a` | `#ffffff` on `#2b2f36` | 255 on 238 | 231 on 237 | reverse | reverse |
| error (text, the error toast's border) | `#ff6b6b` | `#b3261e` | 203 | 124 | 9 | 1 |

The delivery marks have colours already, decided on 2026-08-10
([the user interface](loquax-ui.md), "the ledger and the glyph"), drawn
until 1042 from one theme-blind palette (`tone_rgb` in `loquax`'s `ui`).
That palette was chosen for a dark pane. On a white pane its yellow is
1.7:1 and its amber 1.9:1, and over the own tint above its red is 2.8:1
and every dim tone below 3:1. So the tones join the table, per theme and
tier, measured against the own tint and the selected own tint, because a
mark only ever sits in an own block:

Since 2026-11-01 (1232) the column draws the tick ladder of
[read receipts](loquax-read-receipts.md), section 3, and needs three tones,
not eight: grey for every rung before read, the green for read, the red
for not sent. Each keeps the value its state had in the table before, so
the contrast measured for it holds (`Tones` in
`leviculum-ui/src/tokens.rs`, checked at 3:1 over both own tints):

| Tone (rungs) | truecolor dark | truecolor light | 256 dark | 256 light |
|---|---|---|---|---|
| grey (sending, mailbox, delivered; the old queued) | `#b4bcc8` | `#4f5866` | 250 | 240 |
| green (read; the old delivered) | `#6fe39a` | `#176a35` | 121 | 22 |
| red (not sent; the old refused) | `#ff8a80` | `#a3211a` | 217 | 124 |

Read is bold besides, and drawn `✔✔` against delivered's `✓✓`, so it does
not rest on the colour alone.

In 16 colours the marks take the block's text colour. Over the blue own
tint the bright red (9) is 2.4:1 and the bright blue (12) 2.0:1 on the
xterm palette; over the cyan one the red (1) is 3.0:1 and the green (2)
1.1:1. The glyph carries the state on its own (rule 3 below).

The 256 tier names its own indexes rather than sending the truecolor
values through `rgb_to_ansi256` (`leviculum-ui/src/color.rs:240`). The
nearest index does not keep contrast: the dark own time `#a9c1e6` on
`#1d3f6e` is 5.8:1, its nearest pair 146 on 23 is 3.6:1, and the light one
falls from 5.9:1 to 4.3:1.

**How the tier is chosen.** In this order, the first that holds:

1. `--no-color`, or `NO_COLOR` set and not empty: monochrome.
2. `--color truecolor|256|16`: that tier. `lnomad` had the flag with two
   values (`lnomad/src/main.rs:65`); `loquax` has it now
   (`loquax/src/main.rs:454`), and both have `16`.
3. `COLORTERM` is `truecolor` or `24bit`: truecolor (the rule
   `resolve_depth` had before 1042).
4. `TERM` contains `256color`: 256. `xterm-256color`, `tmux-256color` and
   `screen-256color` land here.
5. `TERM` is `dumb` or unset: monochrome.
6. Anything else (`linux`, `xterm`, `screen`, `vt220`): 16.

Before 1042 steps 4 to 6 did not exist and every terminal without
`COLORTERM` got 256, which the Linux console and a bare `xterm` render as
their own nearest guess. The ladder is `resolve_tier`
(`leviculum-ui/src/color.rs:138`), pure over a snapshot of the
environment. A terminfo lookup of the `colors` capability would be
exact where `TERM` is a name the heuristic does not know; the only
terminfo reader in `Cargo.lock` arrives with `termwiz`, outside the
shipped programs, so it is not proposed now.

**Monochrome stays first class.** It is the layout of today: alignment,
the glyphs, reverse video for the badge, the focused selection and the
toast, no colour at all. The snapshot frames pin it, and it is the one
look every test frame shows. One gap was fixed with it: until 1042
`NO_COLOR` reached the screen only because crossterm drops colour
sequences on output when it is set
(`vendor/crossterm/src/style/types/colored.rs:76`), while `loquax`'s own
`no_color` came from the flag alone and `lnomad`'s from the flag and a
non-tty. So under `NO_COLOR` alone the colour was gone but the reverse
video that `no_color` chooses instead never came, and a toast drew as
plain text. Both programs now read `NO_COLOR` through the ladder, whose
first rung it is.

#### 2. Light or dark

`loquax` asks the terminal for its background before raw mode
(`loquax/src/tui.rs:251`), the same query `lnomad` makes: OSC 11, with
`COLORFGBG` as the fallback, 100 ms to answer
(`leviculum-ui-ratatui/src/background.rs:12`), decided by
`resolve_theme` (`leviculum-ui/src/theme.rs:73`). It was hardcoded to
`auto` there until 1042. Built as proposed:

- a key `theme = auto|dark|light` in `loquax`'s config file
  (`loquax/src/config.rs:367`, which holds `key = value` lines), `auto` the
  default;
- `--theme auto|dark|light` on the command line over the key, as `lnomad`
  has it (`lnomad/src/main.rs:57`);
- a terminal that does not answer in time and sets no `COLORFGBG` gives
  dark, as in `lnomad`;
- a palette action `theme` toggles for the session, the way `t` calls
  `toggle_theme` in lnomad (`lnomad/src/app/intent.rs:351`); no key, the
  keymap is decided. Its toast names the theme it turned to
  (`loquax/src/ui.rs:6069`).

#### 3. What colour carries and what it must not carry alone

- **The side is carried by alignment and tint, never by tint alone.** A
  colour-blind reader, a monochrome terminal and a 16-colour dark terminal
  (where their blocks are not tinted, there being no grey background among
  the eight) all read the side from where the block sits. Below 40 cells,
  where ours go flush left, the `you` before the time carries it, as
  section 5 already has it. On a pane close to the their tint (a `#282c34`
  terminal against `#2f333b`) the their tint all but vanishes, which is
  fine for this reason.
- **The delivery state is the glyph.** Colour reinforces it and the
  `--ascii` marks carry it the same way; nothing about colour changes
  which mark is drawn, and colour applies to whichever mark set is on.
- **Unread is a rule with a count.** The `2 new` text is the signal; its
  colour is a help. The badge in the list stays reverse video in every
  tier.
- **Selection is the bar in the margin** in the list, and in the
  conversation a block's selected tint plus the same bar; in 16 colours
  and in monochrome it is reverse video.
- **The time inside a block** takes the block's time colour; on a selected
  block it takes the block's text colour, so no third pair has to be
  checked.

#### 4. Mockups

The frames of section 5 once more, each followed by its tint map: the same
cells, `.` where the own tint lands, `,` where the their tint lands, `=`
where the date pill lands, blank for the pane. A tinted block is the
rectangle of its text plus one cell either side, which is the margin the
frame already leaves; consecutive messages of one side stay separate
rectangles, so their ragged edge shows where one ends. Text colours are
not mapped.

**80 x 24.**

```text
┌─ Chats ────────────────┐┌─ Alice ·4f2a  known ────────────────── pn ⌂ 4 min ─┐
│/ search                ││                                                    │
│                        ││                                                    │
│▌Alice ·4f2a      14:05 ││             ── Saturday 24 October ──              │
│▌ok, bring the T114.    ││ are you at the field site tomorrow?  13:58         │
│ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ ││                                                    │
│ Bob ·b7d0        12:40 ││                yes, from ten. Shall I bring the    │
│ you: the antenna is…   ││               T114 and the long antenna?  14:02 ✓✓ │
│                        ││                                                    │
│ <9c31e0a2…>        Fri ││                    ── Today ──                     │
│ hello? is this th… [1] ││                                on my way  09:12 ✓  │
│                        ││                                                    │
│ Carol ·17ee     22 Oct ││ ──────────────────── 2 new ─────────────────────── │
│ you: thanks, merged    ││ ok, bring the T114.  14:04                         │
│                        ││                                                    │
│                        ││ and the small tripod                               │
│                        ││ if it fits in the bag  14:05                       │
│                        ││                                                    │
│                        ││                           the tripod fits  14:06 ⠹ │
│                        │├─ to Alice ──────── Enter send · Alt-Enter newline ─┤
│                        ││ sure, and I will charge                            │
│                        ││ the batteries tonight▏                             │
└────────────────────────┘└────────────────────────────────────────────────────┘
 Tab focus  Alt-↑↓ chat  / search  : commands  ? help  Ctrl-Q quit              
```

```text
┌────────────────────────┐┌────────────────────────────────────────────────────┐
│                        ││                                                    │
│                        ││                                                    │
│                        ││               =====================                │
│                        ││,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,        │
│                        ││                                                    │
│                        ││               .....................................│
│                        ││               .....................................│
│                        ││                                                    │
│                        ││                      =======                       │
│                        ││                                ....................│
│                        ││                                                    │
│                        ││                                                    │
│                        ││,,,,,,,,,,,,,,,,,,,,,,,,,,,,                        │
│                        ││                                                    │
│                        ││,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,                      │
│                        ││,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,                      │
│                        ││                                                    │
│                        ││                          ..........................│
│                        │├────────────────────────────────────────────────────┤
│                        ││                                                    │
│                        ││                                                    │
└────────────────────────┘└────────────────────────────────────────────────────┘
```

Text over the own tint is 9.7:1 on dark and 13.5:1 on light in
truecolor, 6.1:1 and 15.1:1 in 256; over the their tint 10.3:1 and
14.5:1, 11.4:1 and 11.9:1; the times over the tints 5.7:1 to 5.9:1 in
truecolor and 4.7:1 to 7.0:1 in 256. In 16 colours the pairs are bright
white on blue (15 on 4) and black on cyan (0 on 6) for ours, black on
white (0 on 7) for theirs on light: 9.4:1, 10.6:1 and 16.7:1 on the xterm
palette, 13.3:1, 7.3:1 and 9.0:1 on the VGA one.

**120 x 40, with a search open.**

```text
┌─ Search ──────────── esc closes ─┐┌─ Alice ·4f2a  known ────────────────────────── 4f2a91c03be8d7e15a60f2b44c9d0e71 ─┐
│ / tripod▏                        ││ direct, 2 hops · heard 14:05                   relay Bern adopted · synced 14:03 │
│                                  ││ ──────────────────────────────────────────────────────────────────────────────── │
│ CONVERSATIONS                  2 ││                                                                                  │
│  Alice ·4f2a               14:06 ││                                                                                  │
│  you: the «tripod» fits          ││                                                                                  │
│                                  ││                                                                                  │
│  Bob ·b7d0                 3 Oct ││                                                                                  │
│  «tripod» stays in the van       ││                                                                                  │
│                                  ││                                                                                  │
│ MESSAGES                       3 ││                          nothing earlier on this device                          │
│ ▌Alice                     14:05 ││                             ── Friday 23 October ──                              │
│ ▌and the small «tripod» if it…   ││ did the board come back from the soak?  19:40                                    │
│                                  ││                         yes, 100 % over the night, log is on the share  19:52 ✓✓ │
│  you to Alice              14:06 ││                                                                                  │
│  the «tripod» fits               ││                            ── Saturday 24 October ──                             │
│                                  ││ are you at the field site tomorrow? Dana says the north                          │
│  Bob                       3 Oct ││ gate is locked after six, so the car park by the road it is.  13:58              │
│  the «tripod» stays in the van,  ││                                                                                  │
│                                  ││                           yes, from ten. Shall I bring the T114 and the long     │
│                                  ││                          antenna, or is the stick enough for the hill?  14:02 ✓✓ │
│                                  ││                                                                                  │
│                                  ││ long one, the hill eats signal  14:03                                            │
│                                  ││                                                                                  │
│                                  ││                                   ── Today ──                                    │
│                                  ││                                                              on my way  09:12 ✓  │
│                                  ││                                            stuck at the level crossing  09:31 ◷  │
│                                  ││                                                                                  │
│                                  ││ ────────────────────────────── 2 new ─────────────────────────────────────────── │
│                                  ││ ok, bring the T114.  14:04                                                       │
│                                  ││                                                                                  │
│                                  ││ and the small «tripod»                                                           │
│                                  ││ if it fits in the bag  14:05                                                     │
│                                  ││                                                                                  │
│                                  ││                                                      the «tripod» fits  14:06 ✓✓ │
│                                  ││                                                         leaving in ten  14:07 ◷  │
│                                  │├─ to Alice ─────────────────────── Enter send · Alt-Enter newline · M-x commands ─┤
│                                  ││ ▏                                                                                │
└──────────────────────────────────┘└──────────────────────────────────────────────────────────────────────────────────┘
 Enter open  n / N next, previous  Ctrl-S / Ctrl-R next, previous  Tab to the results  Esc close search                 
```

```text
┌──────────────────────────────────┐┌──────────────────────────────────────────────────────────────────────────────────┐
│                                  ││                                                                                  │
│                                  ││                                                                                  │
│                                  ││                                                                                  │
│                                  ││                                                                                  │
│                                  ││                                                                                  │
│                                  ││                                                                                  │
│                                  ││                                                                                  │
│                                  ││                                                                                  │
│                                  ││                                                                                  │
│                                  ││                                                                                  │
│                                  ││                               ===================                                │
│                                  ││,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,                                   │
│                                  ││                         .........................................................│
│                                  ││                                                                                  │
│                                  ││                              =====================                               │
│                                  ││,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,             │
│                                  ││,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,             │
│                                  ││                                                                                  │
│                                  ││                          ........................................................│
│                                  ││                          ........................................................│
│                                  ││                                                                                  │
│                                  ││,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,                                           │
│                                  ││                                                                                  │
│                                  ││                                     =======                                      │
│                                  ││                                                              ....................│
│                                  ││                                            ......................................│
│                                  ││                                                                                  │
│                                  ││                                                                                  │
│                                  ││,,,,,,,,,,,,,,,,,,,,,,,,,,,,                                                      │
│                                  ││                                                                                  │
│                                  ││,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,                                                    │
│                                  ││,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,                                                    │
│                                  ││                                                                                  │
│                                  ││                                                      ............................│
│                                  ││                                                         .........................│
│                                  │├──────────────────────────────────────────────────────────────────────────────────┤
│                                  ││                                                                                  │
└──────────────────────────────────┘└──────────────────────────────────────────────────────────────────────────────────┘
```

The same pairs, plus the two the larger frame adds: the date pill is
7.1:1 on dark and 10.8:1 on light in truecolor (6.3:1 and 8.1:1 in 256),
and the `2 new` rule over the pane is at least 6.5:1 on dark and 5.3:1 on
light in truecolor against the reference panes (`#000000` and `#282c34`,
`#ffffff` and `#fdf6e3`). The marks in this frame (`✓✓` `✓` `◷`, drawn
before 1232 as `✓` `⌂` `◇` `○`) are 5.1:1 to 6.7:1 over the own tint and
3.6:1 to 5.4:1 over the selected one in truecolor, 3.4:1 to 9.3:1 and 3.1:1
to 8.6:1 in 256; the ladder's three tones are among the values measured.

#### 5. Tests the layout slice will carry

- **The frames stay plain text.** The `frame` step of the key script
  (`loquax/src/tui.rs:1322`) writes characters only, and the existing
  snapshot frames keep pinning the monochrome look.
- **A script step `styles <name>`** writes one frame's per-cell style as a
  map in the letters above: `.` `,` `=` for the three tints, `s` for a
  selected block, `r` for reverse video, blank for the pane. One fixture
  per theme and tier, six in all, of one session that has every token on
  screen (both sides, a date, the `new` line, a selected block, a toast).
  The six differ where the table differs: no `,` and no `=` in 16 dark,
  `r` for selection in both 16 maps. A seventh fixture under `--no-color`
  holds only blanks and `r`.
- **A unit test over the token table.** The table is a struct per theme
  and tier, so a missing token does not compile. The test asserts every
  text pair (text and time over each tint, over each selected tint, the
  pill, the toast) at 4.5:1 or more, every pane foreground at 4.5:1 or
  more against the two reference panes of its theme, and every delivery
  tone at 3:1 or more over the own tint and the selected own tint
  (WCAG 2.1, 1.4.11, the bar for a graphic). Truecolor uses the values;
  256 uses the xterm-256 values of the indexes; 16 is checked on the
  xterm default palette and the VGA palette of the Linux console, and only
  for the text pairs, since the marks there are text. The ratio is the
  WCAG 2.x one, sRGB linearised: `luminance` in `leviculum-ui/src/theme.rs:66`
  is a linear approximation that serves the light or dark decision and
  must not be reused for it.
- **The tier ladder** gains unit tests over its pure decision, as
  `resolve_depth` had: each `TERM` and `COLORTERM` case above, and
  `NO_COLOR` empty and set.

#### 6. Decided (Lew, 2026-10-26)

Lew took the five proposals as they stood, on 2026-10-26 at 21:10:

1. **The hues** as the table has them: the own tint `#1d3f6e` on dark and
   `#d3e3fd` on light in truecolor, 24 and 189 in 256, blue (4) under
   bright white on dark and cyan (6) under black on light in 16. Blue
   because it is Signal's own side and because green, red, amber and
   yellow already mean a delivery state; pale with dark text on light,
   because a saturated block several rows tall is heavy in a cell grid.
2. **Their blocks tinted** with the grey of the table in truecolor and
   256, and on light in 16; untinted on dark in 16, where the eight have
   no grey background.
3. **The date as a pill** under its label; the rule with its dashes stays
   in every tier.
4. **The shared tokens move to `leviculum-ui`** (muted, selection, toast,
   error, the tier ladder and `NO_COLOR`), and `lnomad` reads them. It
   keeps NomadNet's heading bands and its link colour, which belong to
   micron pages; its chrome bars (`chrome_bg` in `Theme`) and the toast
   accents that `leviculum-ui-ratatui`'s toast drew before 1042 became
   tokens of the same table.
5. **The toggle is a palette action only**, no key.

#### Built 2026-10-26 (leviculum 1042)

Built in 53b6c159 (`leviculum-ui`, `lnomad`) and ddff9dd2 (`loquax`), with
these differences from the text above:

- **A colour is an `Ink`**, RGB or a palette index, because the 256 and
  16 tiers name their own indexes: the styled text model carried RGB
  only. The sixteen go out as the named SGR colours (`30` to `37`, `90`
  to `97`) a 16-colour terminal reads.
- **The style map has one letter for reverse video.** Part 5 kept `s`
  for a selected block and `r` for reverse video; the build writes `^`
  for both, because a selected block in 16 colours and in monochrome is
  reverse video and a map of styles cannot tell the two apart. The
  letters (`style_letter`, `loquax/src/tui.rs:1477`): `.` ours, `,`
  theirs, `=` the date pill, `!` the unread rule, `^` a selected block's
  tint or reverse video, `t` the toast, `e` the error colour, blank for
  the pane. Where two tokens share their colours the earlier letter wins:
  in 16 colours on light the date pill is their block's pair (0 on 7)
  and maps as `,`. The script sets the tier and theme with `tier` and
  `theme` steps and starts monochrome and dark, whatever the host.
- **The fixture session** is the 80 x 24 chat session of slice 2: while
  the reader is scrolled up it opens the palette's `theme`, whose toast
  then stands for the toast token, and writes the six maps and the
  monochrome one over that screen into `tui_chat.styles`. The error toast
  is not in that session; a unit test draws one in every tier.
- **A tint covers the margins, reverse video does not.** A tinted block
  is its text plus a margin cell either side, the bar in the left one
  when selected; a reverse-video block reverses its text alone, as the
  monochrome screen did before.
- **Muted** are the top line, the dashes of a date rule and the rule
  under the wide header. The `you` of the narrow pane sits in the
  block's suffix and takes the time colour with the rest of it.
- **The toast** is the toast token's pair over the whole box, and an
  error toast's border is the error token. Where the toast token is
  reverse video (16 colours, monochrome) the whole box is, the error
  border too. In `lnomad` this replaces the red and green toast text.
- **The marks are uncoloured in monochrome.** Before, `loquax --no-color`
  still sent each mark's RGB tone.
- **`loquax`'s footer and help** draw `lnomad`'s chrome colours in RGB, so
  `loquax` downgrades them in 256 and 16 colours as `lnomad` always has.
- **`lnomad`'s chrome tokens** hold its bars' colours: the truecolor
  values it drew before, and in 256 and 16 the indexes its depth
  post-pass lands them on, which a test pins, so the bars did not move.
  Its pictures' status line takes the muted token. Under `--color 16`
  its pictures fall back to text: sixteen colours draw no half-block
  picture a reader recognises.
- **`lnomad` has no settings file**, so the `theme` key is `loquax`'s
  alone; `lnomad` keeps `--theme`.
- **The 16-colour pane foregrounds are outside the test**, as part 5
  has it (only the text pairs there). Measured anyway: the error colour
  9 over the `#282c34` reference pane is 3.5:1 on the xterm palette and
  4.45:1 on the VGA one, under the 4.5 a text pair would need; every other 16-colour pane pair is above 5:1.

### What the mouse does

| Element | Click | Wheel | Other |
|---|---|---|---|
| a pane | focuses it | scrolls the pane under the pointer, not the focused one | |
| a conversation row | selects and opens it | | |
| an unread badge | opens the conversation at its `new` line | | |
| `+` in the list's title | new conversation | | |
| `/ search` | focuses the search field | | |
| a message | selects it | | drag selects text across lines and blocks, release copies by OSC 52 (loquax-ui.md, "The mouse as a first-class citizen") |
| a delivery mark | opens the message's detail view and ledger | | |
| a link in a body | follows it, as in `lnomad` | | |
| the compose box | focuses it and places the cursor | scrolls the body | |
| `send` on the box's border | sends (TX) | | |
| the propagation node's state in the header | fetches (TX) | | |
| a footer item | runs its row | | hover highlights, as in `lnomad` |
| outside an overlay | closes it | | |

The two TX targets are labelled with their word, never a bare glyph, so
a click that transmits is as deliberate as the `Enter` it stands for.

#### Built 2026-10-26 (leviculum 1036)

The table is built (36c8a80e, 91dfc862) in `loquax/src/ui/hit.rs`, which
answers what a cell is from the rectangles, the view and the laid-out
rows the renderer draws, so the model's tests click the cells a reader
sees. Where it differs from the table:

- **Not there yet:** `+` in the list's title, whose target is slice 5
  below; `f` hints for links. `/ search` came with slice 4, on the list's
  bottom border, and a click on a result opens it ("Built 2026-10-26, the
  search", above).
- **The wheel over the list** steps the conversation, a notch at a time:
  the list has no view of its own, it scrolls just far enough to show the
  selected one. Over the conversation it moves the view three rows and
  leaves the cursor; over the compose box, a line of the body.
- **A click on a message selects it where it is**; the view does not
  scroll to it. The mark's hit box is its glyph, the badge's its brackets
  and count.
- **The footer's `send` is not a button.** The only clicks that transmit
  are the two words, and a unit test clicks every cell at both mockup
  sizes in every focus to hold it; the footer item in compose stays the
  key it names. The other footer items run their row's action and
  highlight under the pointer, as in `lnomad`.
- **Any click closes the help**, as in `lnomad`: it fills an 80 x 24
  screen and leaves no outside. The palette and the detail view close on
  a click outside them; a click on a palette row does nothing yet, its
  footer's `Enter run` and `Esc close` work.
- **The help and the detail view scroll** (1095: Lew could not reach the
  bottom of the help on a phone over mosh). The shared scroll rows move
  them, as in the conversation, and the wheel moves either three rows
  wherever the pointer is; each opens at the top, and its scrollbar
  follows. A line wider than the help wraps on a narrow screen, and the
  scroll counts the wrapped rows, so the last row stays reachable. The
  scrollbar is not draggable: a click on the help closes it.
- **A drag selects from where it went down to where it is**, in reading
  order and held to the text: the margin, the bar, the time and the mark
  are never part of it. A copy has a new line between a body's lines and
  between two messages, and a space where a line wrapped. The selection
  is drawn in inverse video against the block, so on the reversed
  selected block it shows as plain. `y` and `Alt-w` (section 3, the copy
  row) copy the selection again, or without one the message under the
  cursor whole.
- **A link** is a word of a body that is an `http`, `https` or `mailto`
  URL, an `lxmf@` address or a NomadNet page (`leviculum-ui/src/link.rs`,
  the rule `lnomad` follows its links by). It is underlined; a click on it
  hands a web or mail URL to the system's handler, opens the conversation
  of an `lxmf@` address for writing (where `lnomad`, which has no
  composer, copies the address), and copies a NomadNet page to open in
  `lnomad`.
- **`--no-mouse`** starts without capture and the palette's `mouse`
  switches it; without capture the terminal's own selection works.
- **Shift+drag is the terminal's own selection, capture or not** (1444):
  xterm, VTE terminals (GNOME Terminal, Tilix), Konsole, kitty, Alacritty,
  foot and WezTerm hand a drag with Shift held to their own selection and
  never report it, so the text lands in the PRIMARY selection and the
  terminal's copy as it does in any other program. Loquax asks nothing
  that would stop it.
- **The window selects as a desktop program does** (1444): a double
  click on a bubble's words selects the word, a triple click its line, a
  press and a drag a range across bubbles in reading order, a Shift+click
  moves the end, a plain click or `Escape` clears it, `Ctrl-A` selects the
  whole conversation. Every change is put on the PRIMARY selection (a
  middle click elsewhere pastes it), `Ctrl-C` puts it on the clipboard.
  Within one bubble the copy is its words; across bubbles one line each,
  `Name, 14:05: text`. A middle click on the compose field pastes the
  PRIMARY selection where it clicked (at its cursor until 1475); the field keeps its own selection,
  `Ctrl-C` and `Ctrl-V`. The rules are the core's
  (`loquax/src/ui/selection.rs`), the pointer's place in a text the
  shared widget's (`leviculum-ui-iced/src/select.rs`).
- **Every text in the window selects** (1475), not only the bubbles'
  words: the day lines, a group's own lines ("Anna joined."), a reply's
  quote, a bubble's position line, each conversation's last line in the
  list, and in the profile the address and the code (shown under the QR
  as text for it). Each is a run of its own in the core, in reading
  order, so a drag runs through all of them. Across runs the copy is one
  line a bubble, `Name, 14:05: ` and the bubble's quote (`> `), words and
  position line joined by ` · `; a day line or a group's line stands
  alone on its line; the address is copied without the blanks between its
  fours. The window has no detail view to select in: the Info is the
  phone's.
- **A list row and a position line are buttons** (1475): a plain click
  opens the chat or the map as before; the pointer moved more than 4
  pixels with the button held, or a double click, selects instead. The
  open that the release of such a drag still sends is dropped within that
  one event; a double click opens with its first click and selects with
  its second, as two clicks are.
- **The pane scrolls at its edge** (1475): while a drag selects, the
  pointer within 32 pixels of the chat's or the list's top or bottom edge,
  or past it, scrolls that way on every frame, faster the deeper it is,
  and the selection follows into what scrolls in.
- **A middle click pastes where it clicks** (1475): the compose field's
  cursor goes to the click, then the PRIMARY selection is pasted there.
- **The terminal's copy mode** (1475, `loquax/src/ui/copy_mode.rs`): `v`
  (vi's visual mode), `Ctrl-Space` (the Emacs mark) or a `Shift`+arrow in
  the messages puts a cursor on the selected message's first character
  and selects from there to wherever it goes: the arrows, `h` `j` `k`
  `l`, `w` `b`, `0` `$` (`Home` `End`), and the Emacs `Ctrl-f` `Ctrl-b`
  `Ctrl-n` `Ctrl-p` `Ctrl-a` `Ctrl-e` `Alt-f` `Alt-b`. `y`, `Alt-w` or
  `Enter` copy it through OSC 52 and leave; `Esc`, `Ctrl-g`, `q` or `v`
  leave without a copy; a click leaves too. The selection is the core's,
  the same as the window's: a cursor cell is a place in its message's
  text, the copy one line a message with its writer and time across
  messages. It is drawn as a drag's, in inverse video. A terminal that
  does not take OSC 52 copies nothing; the copy mode cannot change that.
  The details moved from `v` to `I` for it.
- **The phone stays as it is** (1475): WhatsApp offers no selection
  inside a bubble either; a long press with Copy copies the message whole,
  and that is the phone's model.
- **A key script** says `click`, `wheel` and `drag` at cells counted from
  1, and writes each copy's OSC 52 sequence into its frames
  (`loquax/tests/fixtures/tui_mouse.frames`).

## 6. What is deliberately not copied from the phone apps

- **Read receipts inferred from the proof.** The proof says the bytes
  reached the destination identity, not that a person read them, and
  nothing is read off it. Replaced on 2026-10-31: Loquax shows a read only
  on the peer's own signed receipt, and its two ticks mean delivered, two
  heavier accent ticks read ([read receipts](loquax-read-receipts.md),
  the glyph ladder). A contact without receipts stays at delivered.
- **Typing indicators.** No LXMF field carries one, and sending one would
  be a transmission per keystroke, which the quiet keyboard rules out on
  its own.
- **Reactions beyond the field.** LXMF now specifies them
  (`FIELD_REACTION`, `reference/LXMF/LXMF/LXMF.py:25`, `:104-110`), and
  Columba, Ratspeak and MeshChatX send it, so Loquax reacts too (1104):
  `e` on a message, the reaction drawn under its target
  (`loquax/src/ui/reactions.rs`). What it does not copy is what the field
  cannot carry: no removal and no change of a reaction (Ratspeak removes
  only through its own envelope), and no fallback text in the content,
  which Columba draws as a second message.
- **Online and last-seen presence.** What the mesh knows is the last
  announce and the path, and the header says exactly that ("heard 14:05",
  "direct, 2 hops"); "online" would round an announce up to a person.
- **Edit and delete for everyone.** Once sent, an LXMF message is the
  peer's; a local delete is offered, a remote one would be a promise the
  protocol cannot keep.
- **Avatars and emoji pickers.** Cells are better spent on text; emoji
  arrive and are drawn as the terminal draws them.

One thing is copied on purpose that a terminal could leave out: a colour
per side, ours tinted and theirs neutral (Lew, 2026-10-26; section 5,
"Colour").

## 7. The slices after 1002

Dated 2026-10-26. Order and dependencies only. Each has its acceptance,
and where it changes what goes over the wire or what the screen shows of
a real exchange, a phase in the 1001 cell (periculum's
`regression/loquax_tui_chat_session.toml`, the `loquax_tui_chat` step: `loquax
tui` against the stock LXMF client on one `lnsd`). The order differs from
the one in the order text in one place: the mouse comes after the layout,
because its targets (the delivery mark, the badge, the blocks) are the
layout's.

1. **The keymap table and help from it.** *Done 2026-10-26, leviculum
   1030 (55fa7b45, 654ff43d).* Section 4: the shared rows move
   to `leviculum-ui`, `lnomad` reads them, `loquax`'s rows join, help,
   footer and palette derive from the table; the four changes of section
   3 if Lew takes them; `Alt-f`/`Alt-b`/`Alt-d`/`Ctrl-d`/`Ctrl-j` in the
   shared editing table; line-wise `Ctrl-a`/`Ctrl-e`; the keyboard
   enhancement pushed where supported. Depends on nothing open.
   *Acceptance:* the unit tests listed in section 4; `lnomad`'s tests and
   golden files unchanged; `tui_snapshot` gains a session driven only by
   vi spellings and one only by Emacs spellings that reach the same
   frames. *1001 phase:* Alice's own three-line body, typed with
   `Ctrl-j`, arrives at Bob as three lines.
2. **The chat layout.** Section 5 without the mouse: list rows with
   preview, time and badge; blocks aligned by side, date separators, the
   `new` line, the narrow fallback; the header with trust (every peer
   `unknown` until slice 5) and propagation node state; the per-message detail
   view with this session's ledger, behind `Enter` on a message. Depends
   on 1 (the `Enter` row). Its colours are section 5, "Colour", as Lew
   decides it: the tokens, the tier ladder, the `theme` key, and the
   delivery tones moved into the table; it invents none. *Done
   2026-10-26 in monochrome, leviculum 1033 (d13c1a6d, 7b72a2ef,
   27e356f0); section 5, "Built", names the differences. The colour half
   done 2026-10-26, leviculum 1042 (53b6c159, ddff9dd2): the tokens, the
   tier ladder, the `theme` key and palette action, the `styles` script
   step and the token test; "Colour", "Built", names the differences.
   Open: the 1001 phase, periculum's.* *Acceptance:*
   snapshots at 80 x 24 and 120 x 40 whose structure is the mockups';
   unit tests over `view` for alignment, wrapping and the `new` line; the
   style maps and the token test of "Colour", item 5. *1001 phase:* Bob's reply arriving while
   Alice reads earlier messages draws the `new` line above it, and her
   detail view of the first message lists the proof.
3. **The mouse.** *Done 2026-10-26, leviculum 1036 (36c8a80e,
   91dfc862); section 5, "What the mouse does", names the differences.
   Open: the 1001 phase, periculum's.* The table of section 5; drag
   selection and OSC 52;
   `--no-mouse` and the palette toggle. Depends on 2. *Acceptance:* unit
   tests feeding mouse events to the model; the key script language gains
   `click` and `wheel` steps and `tui_snapshot` pins a session clicking a
   conversation, a mark and `send`. *1001 phase:* none of its own; the
   send by click is the same send, so a script variant of phase 1 that
   sends with `click` on `send` is enough.
4. **Search.** *Done 2026-10-26, leviculum 1041 (c065767c, 7a4f57f8);
   section 5, "Built 2026-10-26, the search", names the differences.
   Open: the 1001 phase, periculum's.* The search field, results over
   conversations and this session's messages, `n`/`N`,
   `Ctrl-s`/`Ctrl-r`, the hit marked in place. Depends on 2 (the results
   take the list pane). *Acceptance:* unit
   tests over the matcher and the result order; a snapshot of the 120 x 40
   search mockup's structure. *1001 phase:* Alice searches a word of Bob's
   reply and lands on that message.
5. **Names from announces.** *Done 2026-10-26, leviculum 1047; section 5,
   "Built 2026-10-26, names", names the differences. Open: the 1001
   phase, periculum's.* The display rule, sanitising, the collision
   and name-change marks, petnames, and with them the three trust states
   in the header. Depends on 2 (where names are drawn); the library side
   is there, `RouterEvent::PeerAnnounced` closed gap 1 of the triage on
   [architecture](loquax-architecture.md) on 2026-08-11, and `loquax`
   already announces its own name (`loquax/src/display_name.rs`). *Acceptance:* unit tests for
   sanitising (control characters, glyph look-alikes, width cap) and the
   collision mark. *1001 phase:* Bob's client announces a display name and
   Alice's list and header carry it with its short hash.
6. **History.** *Done 2026-10-27, leviculum 1049; section 5, "Built
   2026-10-27, history", names the differences. Open: the 1001 phase,
   periculum's.* The SQLite store of [storage](loquax-storage.md),
   conversations and messages across restarts, the ledger persisted,
   search over FTS5, local delete, pinning, and the restart discontinuity
   shown honestly (a message queued at exit is `◷` again after restart).
   Depends on the storage decision's implementation, on 4 for FTS5, and
   on 2 for where history is drawn. *Acceptance:* tests over the store's
   durability rule (atomic writes, corrupt is not missing); a restart in
   `tui_snapshot`. *1001 phase:* Alice's `loquax tui` restarted mid-session
   shows the earlier exchange with its marks.
