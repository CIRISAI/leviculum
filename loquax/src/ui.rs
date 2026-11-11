//! The screen of `loquax tui` as a model: what the reader sees and what a key
//! does to it, with no terminal anywhere (`docs/src/concepts/loquax-ui.md`,
//! sections 1 and 2; `loquax-architecture.md`, "the core must not know it has
//! a terminal").
//!
//! The shape is lnomad's, TEA: [`UiModel`] is the state, [`update`] folds one
//! [`UiEvent`] into it and returns the [`UiEffect`]s the shell is to run, and
//! [`view`] turns the model into what each pane shows. The shell draws that
//! and runs the effects; a test does neither and asserts on the model, the
//! effects and the view directly.
//!
//! Three panes, focus follows the pane: conversations, messages, compose.
//! What a key does is one table, [`crate::keys`]: lnomad's scroll rows in
//! the two command panes, loquax's own rows beside them, each chord with the
//! panes it applies in. The key handler, the help overlay, the footer, the
//! palette's keys and the compose pane's hint all read it
//! (`docs/src/concepts/loquax-keys-and-layout.md`, section 4). Compose takes
//! plain typing; the palette (`:` or `Alt-x`) is layered on top over the
//! shared names in [`leviculum_ui::commands`]. No modes.
//!
//! # What a delivery glyph claims
//!
//! A sent message carries a [`DeliveryState`] from the shared glyph table.
//! The router's states map onto it in [`delivery_of`]. While the frontend's
//! send runner is still working on a message, a giving-up state is held
//! back: `--via auto` cancels or loses its direct leg and then uploads the
//! same text to a mailbox, and a `✗` drawn for the leg would be a lie about
//! the message. The `✗` appears when the runner reports it gave up
//! ([`UiEvent::SendDone`] with an error).
//!
//! # The layout
//!
//! Signal's shape in cells (`loquax-keys-and-layout.md`, section 5): the list
//! with two rows a conversation, most recent first; the open conversation as
//! blocks, ours flush right and theirs flush left, with a date line between
//! days and a `new` rule where the unread begins; a detail view per message
//! with what this device saw happen to it. The model knows the screen's size
//! ([`UiEvent::Resize`]) and the time of day ([`UiEvent::Clock`]), because
//! where a block wraps and what a page is depend on the one, and every time on
//! screen on the other; [`layout`] turns a conversation into rows.
//!
//! # The mouse
//!
//! Section 5's table, "What the mouse does": [`UiEvent::Mouse`] carries a
//! click, a wheel turn, a drag or a move at a cell, and [`hit`] says what is
//! drawn there from the same layout the view draws, so a click lands on what
//! the reader sees. The two targets that transmit, `send` on the compose
//! box's border and the mailbox state in the header, run the same commands
//! as `Enter` and `F5`. A drag over the conversation selects text and its
//! release copies it ([`UiEffect::Copy`], OSC 52 in the terminal). On the
//! map a drag moves the view with the pointer and a click opens the pin it
//! went down and came up on ([`map::MapPress`]); the wheel zooms about the
//! pointer, and tilted sideways pans an eighth as `h` and `l` do.
//!
//! # History
//!
//! The conversations, their messages and ledgers, the pins and the names
//! heard outlive the process in the shell's store ([`history`]): the model
//! starts from what it hands back ([`UiModel::restore`]) and says after each
//! fold what changed ([`UiModel::take_writes`]). The top of a conversation
//! is the top of the store (`nothing earlier on this device`), or says how
//! much more the store holds above what is loaded. A message is deleted from
//! the device with `d` and `y`, a conversation from the palette, and the
//! palette's `pin` puts a conversation above the list's rule.
//!
//! # What is not here
//!
//! QR codes are a later slice.
//!
//! # Names
//!
//! A peer is drawn as [`crate::names::Names::label`] says, everywhere: the
//! petname or the announced name with the short hash, `≈` when another
//! peer's name looks the same, the address while it has none. The header
//! adds the trust state in words, and a changed name is a line in the
//! conversation ([`Note`]). The palette's `rename` names the open
//! conversation's peer, `block` and `unblock` set its block, and `blocked`
//! lists the blocked conversations, which the list, the search, the map, the
//! unread counts and the toasts otherwise leave out ([`contacts`]).
//!
//! # The search
//!
//! `/` or `Ctrl-s` turns the list pane into the search ([`search`]): its
//! field on top, the results under it as the query is typed, the plain
//! list while the field is empty. A result opens with `Enter` or a click,
//! a message's at that message with the match marked in its block, and
//! while the search is open `n`/`N` and `Ctrl-s`/`Ctrl-r` step the matches
//! of the open conversation in place. `Esc` closes it and the list comes
//! back; the conversation keeps what it showed.
//!
//! # The map
//!
//! The palette's `map`, `M` or `Alt-m` puts the map in the conversation
//! pane's place ([`map`]): a pin per peer with a known position, fitted on
//! open, zoomed with `+` and `-`, panned with the arrows or `h` `j` `k`
//! `l`, refitted with `0`. `Tab` steps the pins, `Enter` or a click on one
//! opens its conversation, `Esc` brings the conversation back. The list
//! stays beside it. The shell draws the picture for the view the model
//! makes; the model places the pins, so what a click hits is what is drawn.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use leviculum_lxmf::router::MessageState;
use leviculum_ui::commands::{self, Command as PaletteCommand, PaletteEntry};
use leviculum_ui::delivery::DeliveryState;
use leviculum_ui::footer::{FooterItem, FooterRole};
use leviculum_ui::help::HelpGroup;
use leviculum_ui::input::{
    InputBuffer, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use leviculum_ui::keymap::{scrolled, Ctx, ScrollCmd};
use leviculum_ui::link::{classify_link, LinkKind};
use leviculum_ui::theme::ThemeFlag;
use leviculum_ui::toast::{Toast, ToastKind};
use leviculum_ui::tokens::{tokens, Tokens};
use leviculum_ui::{RLine, RStyle, Rect, StyledChar, Theme, Tier};

use crate::address::to_hex;
use crate::keys::{self, Action};
use crate::names::{Contact, Heard, Names, Trust};
use crate::outbox::OutboxEvent;
use crate::send::Leg;
use crate::sync::{Reason, Schedule, SyncEvent};
use crate::telemetry::{self, Fix};
use crate::when::{list_time, WallTime};
use leviculum_lxmf::Field;

use history::{Confirm, StoreRead, StoreWrite, StoredHit};

pub mod arrival;
pub mod browse;
pub mod by_hand;
pub mod chat;
pub mod compose;
pub mod contacts;
pub mod copy_mode;
pub mod counters;
pub mod explore;
pub mod files;
pub mod group_scene;
pub mod groups;
pub mod history;
pub mod hit;
pub mod hubs;
pub mod id_index;
pub mod info;
pub mod intent;
pub mod inventory;
pub mod layout;
pub mod listing;
pub mod liveness;
pub mod mailbox_page;
pub mod map;
pub mod pager;
pub mod photos;
pub mod profile;
pub mod radar;
pub mod reactions;
pub mod receipts;
pub mod recording;
pub mod relayed;
pub mod replies;
pub mod rooms;
pub mod scene;
pub mod search;
pub mod selection;
pub mod settings;
pub mod share;
pub mod state;
pub mod strings;
pub mod summary;
pub mod tracks;
pub mod verbs;
pub mod viewload;
pub mod voice;
pub mod zoom;

#[cfg(test)]
mod late;

/// How many ticks a toast stays up. The shell ticks every 250 ms, so three
/// seconds.
pub const TOAST_TICKS: u64 = 12;

/// The most toasts a model keeps (1603): only the newest is drawn
/// ([`UiModel::current_toast`]), and a shell that never ticks (the M9
/// sends no [`UiEvent::Tick`]) never ages one out, so every received
/// message left its toast in the model: a rebuild of 4464 entries kept
/// 4464 toasts and their texts, a reallocation of 4096 at a time.
pub const TOAST_KEEP: usize = 8;

/// Shell ticks in a second, for the leg's elapsed time on a row.
const TICKS_PER_SECOND: u64 = 4;

/// Hex characters of an address a pane shows; the full address is in the
/// title bar of the messages pane.
const SHORT_HEX: usize = 16;

/// The compose pane grows with its text up to this many rows, then scrolls.
pub const COMPOSE_MAX_ROWS: usize = 5;

/// The conversation list is never narrower than this, so a row keeps a name
/// and a time beside each other (the 80 x 24 mockup's 26 cells).
pub const LIST_MIN_WIDTH: u16 = 26;

/// From this inner width on the header is the 120 x 40 mockup's: the full
/// address in the title and a row for the mailbox under it.
pub const WIDE_HEADER: usize = 76;

/// The list pane's outer width on a screen `width` cells wide: three tenths,
/// at least [`LIST_MIN_WIDTH`], at most half.
pub fn list_width(width: u16) -> u16 {
    (width * 3 / 10).max(LIST_MIN_WIDTH).min(width / 2)
}

/// Rows the wheel moves the conversation by, a notch at a time.
pub const WHEEL_ROWS: usize = 3;

/// Where the screen's parts are, in cells: the list left the full height
/// above the footer, the conversation and the compose box beside it, the
/// footer at the bottom. The renderer draws in these rectangles and the
/// mouse hit-tests against them, so the two cannot disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen {
    pub list: Rect,
    pub messages: Rect,
    pub compose: Rect,
    pub footer: Rect,
}

impl Screen {
    /// The parts of a `width` x `height` screen whose compose box shows
    /// `compose_rows` rows.
    pub fn of(width: u16, height: u16, compose_rows: usize) -> Self {
        let body = height.saturating_sub(1);
        let left = list_width(width).min(width);
        let right = width - left;
        let compose = (compose_rows as u16 + 2).min(body.saturating_sub(1));
        Self {
            list: Rect::new(0, 0, left, body),
            messages: Rect::new(left, 0, right, body - compose),
            compose: Rect::new(left, body - compose, right, compose),
            footer: Rect::new(0, body, width, height.min(1)),
        }
    }

    /// The whole screen.
    pub fn area(&self) -> Rect {
        Rect::new(0, 0, self.footer.width, self.footer.bottom())
    }
}

/// The inner sizes of the two panes the model lays out, in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneSizes {
    /// The conversation list, inside its border.
    pub list: (usize, usize),
    /// The conversation, between its top border and the compose box.
    pub messages: (usize, usize),
}

impl PaneSizes {
    /// The panes of a `width` x `height` screen whose compose box shows
    /// `compose_rows` rows: the list the full height above the footer, the
    /// conversation and the compose box beside it, sharing one border.
    pub fn for_screen(width: u16, height: u16, compose_rows: usize) -> Self {
        let body = usize::from(height.saturating_sub(1));
        let left = list_width(width);
        let right = usize::from(width - left);
        Self {
            list: (usize::from(left).saturating_sub(2), body.saturating_sub(2)),
            messages: (
                right.saturating_sub(2),
                body.saturating_sub(compose_rows + 2).saturating_sub(1),
            ),
        }
    }
}

/// A pane, and where the focus is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    /// The conversation list.
    Conversations,
    /// The selected conversation's messages.
    Messages,
    /// The compose line.
    Compose,
    /// The search's field, at the top of the list pane while a search is
    /// open; the list under it is its results.
    Search,
    /// The map, in the conversation pane's place while it is open.
    Map,
}

impl Pane {
    /// The pane `Tab` goes to: the three in turn, the field before the
    /// list while a search is open.
    fn next(self, searching: bool) -> Self {
        match (self, searching) {
            (Self::Map, _) => Self::Map,
            (Self::Search, _) => Self::Conversations,
            (Self::Conversations, _) => Self::Messages,
            (Self::Messages, _) => Self::Compose,
            (Self::Compose, true) => Self::Search,
            (Self::Compose, false) => Self::Conversations,
        }
    }

    fn prev(self, searching: bool) -> Self {
        match (self, searching) {
            (Self::Map, _) => Self::Map,
            (Self::Search, _) => Self::Compose,
            (Self::Conversations, true) => Self::Search,
            (Self::Conversations, false) => Self::Compose,
            (Self::Messages, _) => Self::Conversations,
            (Self::Compose, _) => Self::Messages,
        }
    }
}

/// What a received message's signature says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signature {
    /// It holds against the sender's key.
    Verified,
    /// The sender's key is not known (yet): `SOURCE_UNKNOWN` in the
    /// reference. It turns into one of the other two when the key arrives
    /// ([`OutboxEvent::Verified`]).
    Unverified,
    /// It does not hold against the sender's key: the message is not what
    /// the sender signed, or not from the sender at all.
    Invalid,
}

/// One message in a conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// Whether we wrote it.
    pub outbound: bool,
    /// The body, as text (lossy for bytes that are not UTF-8).
    pub body: String,
    /// The router's id: a received message's, or the id the router gave the
    /// latest leg of a sent one. `None` until the router queued it.
    pub id: Option<[u8; 32]>,
    /// Where a sent message stands; `None` for a received one.
    pub delivery: Option<DeliveryState>,
    /// What a received message's signature says; ours are
    /// [`Signature::Verified`].
    pub signature: Signature,
    /// The leg the send runner is on and the tick it began, while the runner
    /// still works on the message: the row says `direct 12 s` or `via
    /// mailbox 40 s` beside the spinner, so a wait has a reason on screen.
    pub leg: Option<(Leg, u64)>,
    /// The runner ended with the message on a link to the peer and no proof
    /// ([`UiEvent::SendUnproven`]). It stays `◇` from then on: only a proof
    /// (`✓`) or a refusal (`✗`) moves it, not the router's retries on new
    /// links nor its giving up on them, since the peer may well have it.
    pub unproven: bool,
    /// When this device wrote or received it; `None` before the shell said
    /// the time.
    pub at: Option<WallTime>,
    /// The body's bytes, as they went or came.
    pub size: usize,
    /// What this device saw happen to it, oldest first, across restarts:
    /// the detail view.
    pub ledger: Vec<LedgerEntry>,
    /// The leg the runner last reported, kept after it ends so a late proof
    /// still names the way the message went.
    pub last_leg: Option<Leg>,
    /// The model's activity count when it was written or arrived: the
    /// search lists messages most recent first by it.
    pub activity: u64,
    /// The LXMF fields it came with, raw: telemetry is read from them on
    /// screen ([`crate::telemetry`]) and the store keeps them whole.
    pub fields: Vec<Field>,
}

impl Message {
    fn ours(body: String, at: Option<WallTime>) -> Self {
        Self {
            outbound: true,
            size: body.len(),
            body,
            id: None,
            delivery: Some(DeliveryState::Queued),
            signature: Signature::Verified,
            leg: None,
            unproven: false,
            at,
            ledger: vec![LedgerEntry::new(at, "queued")],
            last_leg: None,
            activity: 0,
            fields: Vec::new(),
        }
    }

    /// A telemetry report: no text, a telemetry field. It is drawn as one
    /// `◎ position` line instead of an empty block.
    pub fn is_report(&self) -> bool {
        !self.outbound && self.is_position()
    }

    /// A position without text, either way: a peer's report, or one we
    /// shared (1201).
    pub fn is_position(&self) -> bool {
        self.body.trim().is_empty() && telemetry::has_telemetry(&self.fields)
    }

    fn note(&mut self, at: Option<WallTime>, what: &str) {
        if self.ledger.last().map(|e| e.what.as_str()) != Some(what) {
            self.ledger.push(LedgerEntry::new(at, what));
        }
    }
}

/// One line of a message's ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerEntry {
    pub at: Option<WallTime>,
    pub what: String,
}

impl LedgerEntry {
    fn new(at: Option<WallTime>, what: &str) -> Self {
        Self {
            at,
            what: what.to_string(),
        }
    }
}

/// Where the unread begins in a conversation, and how many arrived unread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewRule {
    /// The first unread message's index.
    pub at: usize,
    pub count: usize,
}

/// A revision mark beside the data it counts (1490): bumped when the data
/// changed, compared by caches. Equal to every other, as
/// [`summary::Summary`] is: it is a cache's key, and two models with the
/// same content are the same model whatever their caches saw.
#[derive(Clone, Copy, Debug, Default, Eq)]
pub struct Rev(u64);

impl Rev {
    pub fn get(self) -> u64 {
        self.0
    }

    fn at(value: u64) -> Self {
        Self(value)
    }
}

impl PartialEq for Rev {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// FNV-1a, the change detectors' fingerprint (1490): cheap, `no_std`, and
/// not security — a collision costs one stale frame, found by the next
/// change.
pub(crate) struct Fnv(u64);

impl Fnv {
    pub(crate) fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn byte(&mut self, byte: u8) {
        self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }

    pub(crate) fn bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.byte(byte);
        }
    }

    pub(crate) fn word(&mut self, word: u64) {
        self.bytes(&word.to_le_bytes());
    }

    pub(crate) fn done(self) -> u64 {
        self.0
    }
}

/// A conversation's place in the list, total over its parts: pinned
/// first, the newest write first, then the activity, the index last so a
/// re-placed mover lands exactly where the stable full sort puts it among
/// equals.
type OrderKey = (
    core::cmp::Reverse<bool>,
    core::cmp::Reverse<Option<i64>>,
    core::cmp::Reverse<u64>,
    usize,
);

/// The list's order as the last fold left it (1490): what
/// [`UiModel::order`] answers from, what [`UiModel::note_changes`]
/// maintains. Its truth is checked, never assumed: a read recomputes the
/// cheap keys and compares ([`OrderCache::current`]), so a model changed
/// outside [`update`] gets a fresh sort, never a stale list.
#[derive(Clone, Debug)]
struct OrderCache {
    show_blocked: bool,
    show_archived: bool,
    look_rev: u64,
    /// By conversation index: its key while the list shows it.
    keys: Vec<Option<OrderKey>>,
    /// The listed conversations, sorted by their keys.
    order: Vec<usize>,
}

impl OrderCache {
    fn fresh<B>(model: &UiModel<B>) -> Self {
        let order = model.order_cold();
        let mut keys: Vec<Option<OrderKey>> = vec![None; model.conversations.len()];
        for &i in &order {
            keys[i] = Some(model.order_key(i));
        }
        Self {
            show_blocked: model.show_blocked,
            show_archived: model.show_archived,
            look_rev: model.look_rev,
            keys,
            order,
        }
    }

    /// Whether the cache still describes `model`: the same conversations,
    /// each listed as it would be and keyed as it would be. O(list), all
    /// of it cheap reads ([`Conversation::last_written`] is O(1) while
    /// the summary is fresh).
    fn current<B>(&self, model: &UiModel<B>) -> bool {
        self.keys.len() == model.conversations.len()
            && self.show_blocked == model.show_blocked
            && self.show_archived == model.show_archived
            && (0..self.keys.len())
                .all(|i| self.keys[i] == model.lists(i).then(|| model.order_key(i)))
    }
}

/// What the target's screen shows of the list and the open chat (1490):
/// the scene builds this many rows and bubbles in view plus as much
/// again as margin, half a screen either side. The default is a phone's
/// portrait screen; each target says its own ([`UiEvent::Viewing`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewing {
    /// Conversation rows in view in the chats list.
    pub list_rows: usize,
    /// Bubble rows in view in the open chat.
    pub chat_rows: usize,
    /// The list is a screen of its own (the M9, the phone): the chat is
    /// on screen only while the keys are in it, and is not built while
    /// the list is the screen. The terminal and the window show both
    /// panes at once and leave this off.
    pub apart: bool,
    /// The scene carries the tab bar (1532). A target whose keys reach
    /// the map and the list directly (the M9's map and house keys) turns
    /// it off and gets the row for content; the core keeps the tabs for
    /// the targets that draw them.
    pub tabs: bool,
}

impl Viewing {
    /// A target that scrolls a scene it keeps whole (the window, until
    /// its own step wires scrolling into the viewport): everything.
    pub const ALL: Viewing = Viewing {
        list_rows: usize::MAX / 4,
        chat_rows: usize::MAX / 4,
        apart: false,
        tabs: true,
    };
}

impl Default for Viewing {
    fn default() -> Self {
        Self {
            list_rows: 14,
            chat_rows: 24,
            apart: false,
            tabs: true,
        }
    }
}

/// The list's window as the last fold built it (1490).
#[derive(Clone, Debug)]
struct BuiltList {
    /// The fingerprint of everything the rows were built from
    /// ([`UiModel::list_stamp`]).
    stamp: u64,
    /// The first built row's place in the whole order, and the order's
    /// length: the scroll geometry.
    first: usize,
    total: usize,
    /// The first row on screen of a list apart (1546).
    top: Option<usize>,
    rows: Vec<scene::Row>,
}

/// The open conversation laid out, as the laid cache keeps it (1490).
#[derive(Clone, Debug)]
struct LaidCache {
    key: u64,
    /// The message the cursor was on when these rows were styled.
    selected: Option<usize>,
    laid: alloc::sync::Arc<layout::Laid>,
}

/// The stamps of the windows the last note left ([`UiModel::scene_owed`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SceneOwed {
    list: Option<u64>,
    chat: Option<(usize, u64)>,
}

/// The open chat's window as the last fold built it (1490).
#[derive(Clone, Debug)]
struct BuiltChat {
    /// The fingerprint of everything the chat was built from
    /// ([`UiModel::chat_stamp`]).
    stamp: u64,
    chat: scene::Chat,
}

/// A delivery state as one fingerprint word.
fn delivery_word(state: Option<DeliveryState>) -> u64 {
    match state {
        None => 0,
        Some(DeliveryState::Queued) => 1,
        Some(DeliveryState::Stamping) => 2,
        Some(DeliveryState::InFlight) => 3,
        Some(DeliveryState::Sent) => 4,
        Some(DeliveryState::InMailbox) => 5,
        Some(DeliveryState::Arrived) => 6,
        Some(DeliveryState::Delivered) => 7,
        Some(DeliveryState::Read { at }) => 8 | (u64::from(at) << 8),
        Some(DeliveryState::Rejected) => 9,
        Some(DeliveryState::Failed) => 10,
    }
}

/// One correspondent and the messages with them the screen loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    /// Their LXMF address.
    pub peer: [u8; 16],
    /// Oldest first, changed only through [`id_index::Messages`]'s
    /// methods so the model's id index hears of every id (1600).
    pub messages: id_index::Messages,
    /// Received messages not yet looked at: the list's badge.
    pub unread: usize,
    /// The `new` rule: set by the first message that arrives unread, kept
    /// until the conversation is left.
    pub new_rule: Option<NewRule>,
    /// The model's activity count at the last message, for the list's order:
    /// most recent first, a conversation without messages by its place.
    pub activity: u64,
    /// When something that is no message moved it up the list (a group's
    /// invitation or its making, unix seconds by our clock): the list's
    /// time for a conversation that has no message yet, in this run
    /// ([`Conversation::last_written`]).
    pub touched: Option<i64>,
    /// What the conversation says between its messages: the peer's name
    /// changes, each once.
    pub notes: Vec<Note>,
    /// Pinned above the list's rule (the palette's `pin`), kept in the
    /// store.
    pub pinned: bool,
    /// Muted, archived, verified (1146), kept in the store.
    pub flags: history::Flags,
    /// How many older messages the store holds than the screen loaded: the
    /// top of the pane says so, and reaching it loads the next page.
    pub earlier: usize,
    /// What was typed here and not sent (1219), kept in the store.
    pub draft: history::Draft,
    /// What the chats list asks of it, kept up to its messages by
    /// [`update`] (1463): a cache the readers check before they trust it.
    pub summary: summary::Summary,
    /// The row as the store last kept it (1490, schema 17): what the
    /// list shows while no message is loaded. The loaded messages win
    /// wherever they are present.
    pub listing: Option<listing::Listing>,
    /// The newest unread received ids while none of the messages is
    /// loaded (1521, [`listing::Row::unread_ids`]): a read mark naming
    /// one clears the badge here, as the store applies it to its rows.
    pub unread_ids: id_index::KeptIds,
    /// Bumped by [`update`] when anything a screen draws of this
    /// conversation changed (1490): what every cache keyed by this
    /// conversation asks before it reuses.
    revision: Rev,
    /// The fingerprint of the row state the last [`update`] saw
    /// ([`Conversation::row_fingerprint`]).
    stamp: Rev,
}

/// A line the conversation says between its messages, centred.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    /// The message it stands before; the conversation's length for after the
    /// last.
    pub before: usize,
    pub text: String,
}

impl Conversation {
    /// When its newest message was written, for the list's order (1445): a
    /// position report or a group's state moves the list nowhere, unless
    /// the conversation has nothing else; a group's invitation or making
    /// counts by when it came ([`Conversation::touched`]). `None` while
    /// nothing has a time.
    ///
    /// A conversation listed from its kept row (1521) answers no earlier
    /// than the row said (1591): reading its pages never moves it down
    /// the list. Window 18's board decoded a preloaded page before it
    /// knew the time, its messages came out with none, and every row the
    /// ring's cursor reached sank to the list's end under the cursor.
    pub fn last_written(&self) -> Option<i64> {
        let kept = self.listing.as_ref().and_then(|listing| listing.moved);
        // Nothing loaded: the store's row answers (1490, schema 17).
        if self.messages.is_empty() && self.listing.is_some() {
            return kept.max(self.touched);
        }
        if let Some(newest) = self.summary.newest(&self.messages) {
            return newest.max(self.touched).max(kept);
        }
        let newest = |moves: bool| {
            self.messages
                .iter()
                .filter(|m| !moves || !(m.is_report() || m.is_group_state()))
                .filter_map(|m| m.at.map(|at| at.unix_secs))
                .max()
        };
        newest(true)
            .or_else(|| newest(false))
            .max(self.touched)
            .max(kept)
    }

    fn new(peer: [u8; 16]) -> Self {
        Self {
            peer,
            messages: id_index::Messages::new(),
            unread: 0,
            new_rule: None,
            activity: 0,
            touched: None,
            notes: Vec::new(),
            pinned: false,
            flags: history::Flags::default(),
            earlier: 0,
            draft: history::Draft::default(),
            summary: summary::Summary::default(),
            listing: None,
            unread_ids: id_index::KeptIds::default(),
            revision: Rev::default(),
            stamp: Rev::default(),
        }
    }

    /// How often [`update`] saw this conversation change (1490): a cache
    /// keyed by it reuses while it stands still. A model changed outside
    /// `update` has not bumped it; the next `update` does.
    pub fn revision(&self) -> u64 {
        self.revision.get()
    }

    /// The fingerprint of what a list row or a bubble pane draws of this
    /// conversation besides the messages the summary watches: the cheap
    /// fields, and the newest message's mutable face (its delivery, its
    /// id). Deeper in-place changes are caught by the fold's own change
    /// sets ([`UiModel::note_changes`]).
    fn row_fingerprint(&self) -> u64 {
        let mut hash = Fnv::new();
        hash.word(self.unread as u64);
        hash.word(u64::from(self.pinned));
        hash.word(self.flags.muted_until.map_or(0, |at| at as u64));
        hash.word(u64::from(self.flags.archived));
        hash.word(u64::from(self.flags.verified));
        hash.word(u64::from(self.flags.receipts));
        hash.word(self.earlier as u64);
        hash.word(self.touched.map_or(0, |at| at as u64));
        hash.word(self.notes.len() as u64);
        if let Some(rule) = self.new_rule {
            hash.word(rule.at as u64);
            hash.word(rule.count as u64);
        }
        hash.bytes(self.draft.text.as_bytes());
        if let Some(reply) = self.draft.reply {
            hash.bytes(&reply);
        }
        hash.word(self.messages.len() as u64);
        if let Some(last) = self.messages.last() {
            hash.word(delivery_word(last.delivery));
            hash.word(u64::from(last.id.is_some()));
        }
        hash.done()
    }

    /// The messages that carry `id`, oldest first: the summary's index
    /// (1463), brought up to the messages first (1521: a batch notes
    /// once at its end, and until then every lookup walked the whole
    /// conversation), or a walk, counted into `ops`, when it still is
    /// not. Each one checked, so an index that missed a rename finds
    /// nothing wrong, only nothing.
    pub fn with_id(&mut self, id: &[u8; 32], ops: &counters::Ops) -> Vec<usize> {
        self.summary.catch_up(self.peer, &self.messages);
        let found: Vec<usize> = match self.summary.with_id(&self.messages, id) {
            Some(indices) => indices.to_vec(),
            None => {
                ops.add(counters::Op::Walked, self.messages.len());
                (0..self.messages.len()).collect()
            }
        };
        found
            .into_iter()
            .filter(|&m| self.messages[m].id == Some(*id))
            .collect()
    }

    /// The newest received icon appearance, foreground and background: the
    /// avatar's colours (`chat::latest_appearance`).
    pub fn appearance(&self) -> Option<(String, String)> {
        if self.messages.is_empty() {
            if let Some(listing) = &self.listing {
                return listing.appearance.clone();
            }
        }
        self.summary
            .appearance(&self.messages)
            .unwrap_or_else(|| chat::latest_appearance(&self.messages))
    }

    /// Note one more unread message, the one at `at` (1505: a late one
    /// sits at its writer's time, not at the end). The rule stays where
    /// the reader left off: above the first unread by arrival, so a
    /// later arrival written earlier sits above it, read or not.
    fn mark_new(&mut self, at: usize) {
        self.new_rule.get_or_insert(NewRule { at, count: 0 }).count += 1;
    }

    /// Where a message written at `written` (unix seconds) goes in the
    /// body (1505): after every message written no later, so the body
    /// runs in its writers' order and a tie keeps the arrival order. One
    /// without a time goes last, and one already there without a time
    /// is taken as written no later. Walks back over the newer messages
    /// only: an arrival in order costs one comparison.
    pub fn written_place(&self, written: Option<i64>) -> usize {
        let mut place = self.messages.len();
        let Some(written) = written else {
            return place;
        };
        while place > 0 {
            match self.messages[place - 1].at {
                Some(at) if at.unix_secs > written => place -= 1,
                _ => break,
            }
        }
        place
    }

    /// Put the messages in their writers' order, a tie and a message
    /// without a time (it keeps the place of the one before it) in the
    /// order they are in (1505): a page the store hands out comes in
    /// arrival order. Each old index's new one, or `None` when they were
    /// in order already.
    fn sort_written(&mut self) -> Option<Vec<usize>> {
        let mut last = i64::MIN;
        let keys: Vec<i64> = self
            .messages
            .iter()
            .map(|m| {
                if let Some(at) = m.at {
                    last = at.unix_secs;
                }
                last
            })
            .collect();
        if keys.windows(2).all(|w| w[0] <= w[1]) {
            return None;
        }
        let mut order: Vec<usize> = (0..keys.len()).collect();
        order.sort_by_key(|&i| keys[i]);
        let mut new_of = alloc::vec![0; keys.len()];
        for (new, &old) in order.iter().enumerate() {
            new_of[old] = new;
        }
        self.messages.reorder(&order);
        self.summary.forget();
        Some(new_of)
    }

    /// The lowest store key the screen holds (1505): where the store's
    /// next page ends. The body's first message is the earliest written,
    /// which a late arrival makes another than the earliest taken.
    pub(crate) fn oldest_key(&self) -> Option<u64> {
        self.messages.iter().map(|m| m.activity).min()
    }

    /// The received messages that have a row of their own, newest
    /// arrival first (1505): the unread are the newest by arrival, which
    /// the body's writers' order may scatter.
    fn received_by_arrival(&self, aside: bool) -> Vec<usize> {
        // From the end, so a tie (a model built by hand) keeps the body's
        // order, newest last.
        let mut received: Vec<usize> = (0..self.messages.len())
            .rev()
            .filter(|&i| {
                let m = &self.messages[i];
                !m.outbound && (aside || !m.is_aside())
            })
            .collect();
        received.sort_by_key(|&i| core::cmp::Reverse(self.messages[i].activity));
        received
    }

    /// The message after `index` that has a row of its own: reactions are
    /// drawn under their target, so they sit in no run's way.
    fn next_shown(&self, index: usize) -> Option<usize> {
        (index + 1..self.messages.len()).find(|&i| !self.messages[i].is_aside())
    }

    /// Whether the report at `index` is folded into the next one, so the
    /// screens draw one row for a run of a peer's position reports: the
    /// newest, with the run's count (1184). Any message in between ends the
    /// run, a text of either side; so do a day, a note and the `new` rule,
    /// which say something about where the run is. Every screen folds by
    /// this: the terminal's blocks, the bubbles, the unread count and the
    /// toast. The store keeps every report.
    pub fn report_folded(&self, index: usize) -> bool {
        let Some(this) = self.messages.get(index) else {
            return false;
        };
        let Some(next) = self.next_shown(index) else {
            return false;
        };
        let that = &self.messages[next];
        let between = index + 1..=next;
        this.is_report()
            && that.is_report()
            && this.at.map(|at| at.day()) == that.at.map(|at| at.day())
            && !self.notes.iter().any(|n| between.contains(&n.before))
            && self.new_rule.is_none_or(|rule| !between.contains(&rule.at))
    }

    /// The reports the row of message `index` stands for, oldest first: the
    /// run that ends there, or `index` alone when it is no report.
    pub fn run(&self, index: usize) -> Vec<usize> {
        let mut run = vec![index];
        let mut at = index;
        while let Some(before) = (0..at)
            .rev()
            .find(|&i| !self.messages[i].is_aside())
            .filter(|&i| self.report_folded(i))
        {
            run.push(before);
            at = before;
        }
        run.reverse();
        run
    }

    /// The unread count as the screens show it: a position report counts
    /// nothing (1209), it is a state and no message. [`Conversation::unread`]
    /// stays the messages, which the home's read marks name one by one.
    ///
    /// Unread messages beyond the received ones loaded lie in the part
    /// not loaded, a conversation listed from its kept row (1521) that a
    /// live page reached since: each counts, as the kept row counted it.
    /// Before 1563 the badge of a chat of 776 unread fell to the 96 a
    /// pull had brought, on a board's live pull and on the boot that
    /// folds a ring's tail after its summary.
    pub fn unread_rows(&self) -> usize {
        if self.messages.is_empty() {
            if let Some(listing) = &self.listing {
                return listing.unread_rows as usize;
            }
        }
        let (rows, received) = match self.summary.unread_rows(&self.messages, self.unread) {
            Some(rows) => (rows, self.summary.received_count()),
            None => (
                self.unread_messages().len(),
                self.messages
                    .iter()
                    .filter(|m| !m.outbound && !m.is_aside())
                    .count(),
            ),
        };
        rows + self.unread.saturating_sub(received)
    }

    /// The unread messages that ask for attention, oldest first: the raw
    /// [`Conversation::unread`] newest received, less the reports.
    fn unread_messages(&self) -> Vec<usize> {
        if self.unread == 0 {
            return Vec::new();
        }
        let mut shown: Vec<usize> = self
            .received_by_arrival(false)
            .into_iter()
            .take(self.unread)
            .filter(|&i| !self.messages[i].is_report())
            .collect();
        shown.sort_unstable();
        shown
    }

    /// The `new` rule the raw unread count gives, as a restart sets it:
    /// above the first unread message that is no report (1209), the
    /// first by arrival, where a running screen put it (1505).
    pub(crate) fn rule_from_unread(&self) -> Option<NewRule> {
        let shown = self.unread_messages();
        shown
            .iter()
            .copied()
            .min_by_key(|&i| self.messages[i].activity)
            .map(|at| NewRule {
                at,
                count: shown.len(),
            })
    }

    /// Whether the peer's position is alive (1209), from the reports the
    /// conversation holds, the newest [`liveness::CADENCE_GAPS`] and one
    /// with a position: each at its fix's time (the receive time where the
    /// sender's clock claims the future), judged at `now`.
    pub fn liveness(&self, now: Option<i64>) -> liveness::Liveness {
        // Nothing loaded: the store's row answers (1490).
        if self.messages.is_empty() {
            if let Some(listing) = &self.listing {
                return listing.liveness(now);
            }
        }
        let fixes = self.report_fixes();
        let times: Vec<i64> = fixes.iter().map(Fix::order_time).collect();
        // The newest report's share mark (1273): its promise, or its end.
        let mark = fixes
            .first()
            .map(|fix| leviculum_lxmf::telemetry::ShareMark::of_blob(&fix.telemetry))
            .unwrap_or_default();
        liveness::Liveness::of_marked(&times, mark, now)
    }

    /// The newest reports' fixes, newest first, the few the liveness is
    /// judged from: by the summary's indices (1463), or a walk when it
    /// is behind.
    pub fn report_fixes(&self) -> Vec<Fix> {
        match self.summary.reports(&self.messages) {
            Some(newest) => newest
                .filter_map(|i| message_fix(self.peer, &self.messages[i]))
                .collect(),
            None => self
                .messages
                .iter()
                .rev()
                .filter(|m| m.is_report())
                .filter_map(|m| message_fix(self.peer, m))
                .take(liveness::CADENCE_GAPS + 1)
                .collect(),
        }
    }

    /// The newest report with a position: the row that says whether the
    /// position is alive (1209). A report without one says so and no more.
    pub fn newest_fix_report(&self) -> Option<usize> {
        self.messages
            .iter()
            .rposition(|m| m.is_report() && message_fix(self.peer, m).is_some())
    }

    /// Where the list orders the conversation (1209): at its newest
    /// message that is no position report, or its first message when it
    /// has only reports.
    pub(crate) fn list_activity(&self) -> u64 {
        self.messages
            .iter()
            .rev()
            .find(|m| !m.is_report())
            .or(self.messages.first())
            .map_or(0, |m| m.activity)
    }
}

/// The mailbox as the header says it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MailboxShown {
    /// The selected node, once a run or the stored record named one.
    pub node: Option<[u8; 16]>,
    /// Named with `--pn`, not adopted.
    pub by_user: bool,
    /// The name the selected node announced (`PN_META_NAME`), sanitised:
    /// the header says the relay by it, "a relay" without one (1587). Only
    /// the selected node's is kept, so it is learnt at its next announce.
    pub name: Option<String>,
    /// When this session's last fetch finished.
    pub synced: Option<WallTime>,
}

/// The open command palette.
#[derive(Clone, Debug, Default)]
pub struct Palette<B> {
    /// What the reader typed.
    pub query: B,
    /// The highlighted row among the matches.
    pub selected: usize,
    /// What the palette asks for instead of a command, its text the answer
    /// (`rename`).
    pub ask: Option<contacts::Ask>,
    /// The palette lists the ways of drawing the map instead of the
    /// commands (`map drawing`, 1093), its query filtering them.
    pub drawings: bool,
}

impl<B: InputBuffer> Palette<B> {
    /// The rows the query matches, in table order; none while the palette
    /// asks.
    pub fn matches(&self) -> Vec<&'static PaletteEntry> {
        match (self.ask, self.drawings) {
            (None, false) => commands::filter(self.query.value()),
            _ => Vec::new(),
        }
    }

    /// While the palette lists the map's drawings: the indexes in
    /// `drawings` the query matches, in their order.
    pub fn drawing_matches(&self, drawings: &[map::Drawing]) -> Vec<usize> {
        drawings
            .iter()
            .enumerate()
            .filter(|(_, d)| commands::fuzzy_match(self.query.value(), &d.name))
            .map(|(i, _)| i)
            .collect()
    }
}

/// [`UiModel::selected`] while no conversation is open: every one there is
/// is blocked, or in the `blocked` view none is.
pub const NONE_OPEN: usize = usize::MAX;

/// The whole screen's state. `B` is the text field: `tui_input::Input` in
/// the terminal, whatever a board brings on a board.
#[derive(Clone, Debug)]
pub struct UiModel<B> {
    /// Conversations, in the order they appeared.
    pub conversations: Vec<Conversation>,
    /// The open conversation's index, one the list shows, or [`NONE_OPEN`].
    pub selected: usize,
    /// The message the messages pane's cursor is on, in the selected
    /// conversation.
    pub message_cursor: usize,
    /// The compose line.
    pub compose: B,
    /// Where keys go.
    pub focus: Pane,
    /// Notes floated over the content, newest last, at most
    /// [`TOAST_KEEP`] of them.
    pub toasts: Vec<Toast>,
    /// How many toasts were said (1603): what the redraw marks hash, as
    /// a toast said at [`TOAST_KEEP`] leaves the count where it was.
    toasts_said: u64,
    /// The palette, when open.
    pub palette: Option<Palette<B>>,
    /// Whether the help overlay is up.
    pub help: bool,
    /// The help's first row on screen, back to the top whenever it opens.
    pub help_scroll: usize,
    /// Our own address, once the engine is ready.
    pub address: Option<[u8; 16]>,
    /// Our own contact code and its QR, which the frontend computes at start
    /// (the QR needs the `qr` feature, the model does not): what `my code`
    /// shows (1279).
    pub own_code: Option<OwnCode>,
    /// Whether the profile is up (1443): what a tap on the avatar and
    /// `my code` open, the scene's [`profile::ProfileScreen`] while it is.
    pub profile_open: bool,
    /// The settings (1450): `Some(None)` their list of sections,
    /// `Some(Some(section))` one section, as the terminal's detail view;
    /// none while they are not up.
    pub settings: Option<Option<settings::Section>>,
    /// What the shell knows of the reader for the profile: the name the
    /// announce carries, the mailbox, the devices.
    pub profile: profile::ProfileFacts,
    /// Draw the delivery glyphs' ASCII fallback.
    pub ascii: bool,
    /// The tick counter: spinner frames and toast ages.
    pub tick: u64,
    /// Whether the reader asked to leave.
    pub quit: bool,
    /// Sent messages whose runner has not finished, as (conversation,
    /// message) indices, in the order the shell runs them.
    pub sending: VecDeque<(usize, usize)>,
    /// Whether a fetch is running.
    pub fetching: bool,
    /// What started the running fetch, or the last one.
    pub fetch_reason: Option<Reason>,
    /// When the running fetch, or the last one, began, on `elapsed_ms`.
    pub fetch_since_ms: u64,
    /// How many fetches ended, either way, for a key script's `until
    /// synced`.
    pub fetches_done: usize,
    /// The mailbox's own schedule (`crate::sync`): the terminal's. A model
    /// without one syncs only when asked, as a key script's does unless it
    /// names `--sync-interval`, so no frame shows a fetch the script did not
    /// ask for.
    pub sync: Option<Schedule>,
    /// The shell's monotonic clock in milliseconds, the schedule's
    /// ([`UiEvent::Elapsed`]).
    pub elapsed_ms: u64,
    /// Whether the terminal speaks the keyboard enhancement protocol, so
    /// `Shift-Enter` and `Ctrl-Enter` arrive as themselves: the hint and the
    /// footer offer them then.
    pub enhanced: bool,
    /// `Enter` in compose sends (the default). Off, it breaks the line and
    /// the modifier chord sends: `Ctrl-Enter` with enhancement, `Alt-Enter`
    /// without.
    pub send_on_enter: bool,
    /// The time of day, as the shell last said it.
    pub now: Option<WallTime>,
    /// The screen, in cells: what [`PaneSizes`] are made from.
    pub screen: (u16, u16),
    /// The conversation's first row on screen when the reader scrolled up;
    /// `None` keeps the newest at the bottom.
    pub scroll_top: Option<usize>,
    /// The message on the first row on screen and how far into its block
    /// the row is, while a fold splices a late bubble above it (1505):
    /// the fold's end puts the view's top back on it
    /// ([`UiModel::keep_top`]), after the `new` rule and anything else the
    /// fold added above.
    kept_top: Option<(u64, isize)>,
    /// The same for a target that lays only a window of the chat (the M9,
    /// 1529): the view's first row by the message it is in, so a window
    /// that moves keeps the line. `None` keeps the newest at the bottom.
    pub chat_anchor: Option<pager::Anchor>,
    /// The message whose detail view is up, as (conversation, message).
    pub detail: Option<(usize, usize)>,
    /// The detail view's first row on screen, back to the top whenever one
    /// opens.
    pub detail_scroll: usize,
    /// The mailbox, for the header.
    pub mailbox: MailboxShown,
    /// Counts messages in and out, for the list's order.
    pub activity: u64,
    /// Whether the mouse is captured (`--no-mouse` and the palette's `mouse`
    /// turn it off, and the terminal's own selection works again).
    pub mouse: bool,
    /// The footer button under the pointer, a footer slot's index.
    pub footer_hover: Option<usize>,
    /// What a drag over the conversation selected.
    pub selection: Option<Selection>,
    /// The copy mode's cursor and anchor while it is on (1475).
    pub copy_mode: Option<copy_mode::CopyMode>,
    /// The button press a drag or a release belongs to.
    pub press: Option<Press>,
    /// The colour tier the screen is drawn in: monochrome until the shell
    /// says another, which is what every test frame shows.
    pub tier: Tier,
    /// Light or dark: which half of the token table the tier draws from. The
    /// palette's `theme` turns it.
    pub theme: Theme,
    /// The search, when open: the list pane is its field and results.
    pub search: Option<search::Search<B>>,
    /// The names this session heard and the reader's contacts: how every
    /// pane names a peer ([`crate::names`]).
    pub names: Names,
    /// The palette's `blocked` view: the list holds the blocked
    /// conversations instead of the others.
    pub show_blocked: bool,
    /// The archived view (1146): the list holds the archived conversations
    /// instead of the others.
    pub show_archived: bool,
    /// Messages a fold changed, by conversation and key, for the store
    /// ([`UiModel::take_writes`]). Only the running fold's: each
    /// [`UiModel::note_changes`] moves them into the owed shadows, so a
    /// fold's own changes are knowable without a diff (1490).
    changed: BTreeSet<([u8; 16], u64)>,
    /// Conversations whose own state (pin, unread) a fold changed.
    changed_peers: BTreeSet<[u8; 16]>,
    /// Conversations made since the last note (1594): a note scoped to
    /// what a batch named ([`Batch`]) refreshes them too.
    made: Vec<[u8; 16]>,
    /// The conversations the running batch's events named (1594,
    /// [`Batch`]), and the scoped note's indices: kept between notes, so
    /// a chunk's note allocates nothing for its scope.
    note_scope: Vec<[u8; 16]>,
    note_indices: Vec<usize>,
    /// What earlier folds changed and the store has not drained
    /// ([`UiModel::take_writes`] reads both halves): each note's set as
    /// it was, merged only by the drain (1591: a merge per note was an
    /// allocation per few entries of a pulled page folded in chunks).
    owed_changed: Vec<BTreeSet<([u8; 16], u64)>>,
    owed_peers: BTreeSet<[u8; 16]>,
    owed_groups: BTreeSet<[u8; 16]>,
    /// Conversations whose listing the store must relearn (1490): every
    /// revision bump queues its peer here, take_writes derives the row.
    listing_owed: BTreeSet<[u8; 16]>,
    /// The note's list of the conversations it bumped, kept between
    /// notes so a note allocates nothing for it (1591).
    bumped: Vec<usize>,
    /// The conversation whose first page was asked of the store and has
    /// not come (1490): asked once per selection, cleared when it lands.
    page_asked: Option<[u8; 16]>,
    /// Read marks whose messages no loaded page holds yet (1490): the
    /// store applied them; the page that brings their conversation
    /// replays them here ([`history::READ_PENDING_MOST`]).
    read_pending: Vec<[u8; 32]>,
    /// Which conversation holds each message id (1600): what a read
    /// mark asks instead of every conversation in turn.
    ids: id_index::IdIndex,
    /// Deletions for the store, in the order they were made.
    forgotten: Vec<StoreWrite>,
    /// Whether this device is a client of a home node: what the reader
    /// reads and deletes here is marked for the home's other devices, and a
    /// send is followed by a pull.
    pub home_client: bool,
    /// The messages arriving now are the home's history, not live mail
    /// (1429): a client whose first pull has not yet settled (its cursor is
    /// still 0) is being handed the archive, the new device after `loquax
    /// mailbox join` or a phone the mailbox moved to. Nobody read those
    /// here, so none is marked read, even in the open conversation; their
    /// read state is the archive's, the read marks the other devices sent,
    /// which come in the same pull behind them. The shell sets it from the
    /// cursor at start and clears it once a pull settled.
    pub history: bool,
    /// The list is a screen of its own, as on a phone: a message arriving
    /// while no conversation is open leaves it closed and unread, where the
    /// terminal, which shows the list beside the conversation, opens it.
    pub list_apart: bool,
    /// Marks for the home node, in the order they were made
    /// ([`UiModel::take_marks`]).
    marks: Vec<crate::home::wire::Mark>,
    /// The petnames this device set that the home node has not taken
    /// yet, the newest of each peer (1635, [`UiModel::take_petnames`]):
    /// kept in the store until the home takes them, and the reader's word
    /// on the peer over any entry a pull brings meanwhile.
    pub(crate) owed_petnames: BTreeMap<[u8; 16], Option<String>>,
    /// A hand of [`UiModel::take_petnames`] is under way: the next waits
    /// for its [`UiEvent::PetnamesHanded`], so the home takes a peer's
    /// petnames in the order they were set.
    petnames_in_flight: bool,
    /// [`Self::owed_petnames`] changed since the store last kept it.
    owed_petnames_changed: bool,
    /// The petnames the home took from this device until the next pull
    /// ends, each with this device's clock when the home's answer came
    /// (1635): a pull under way when the home took it may still bring an
    /// older entry, which the home's stamp tells apart from a newer word.
    taken_petnames: BTreeMap<[u8; 16], (Option<String>, i64)>,
    /// Peer states for the home node, in the order they were set
    /// ([`UiModel::take_peer_states`]).
    peer_states: Vec<([u8; 16], crate::home::wire::PeerState)>,
    /// This device's peer states the home has not echoed back yet (1614):
    /// a pulled state for one of these peers is the home's older word,
    /// pulled before ours reached it, and is not taken; the echo of ours
    /// settles it. So the state shown does not depend on whether a pull or
    /// the hand-off arrives first. Petnames have their own, kept across a
    /// restart (1635, [`Self::owed_petnames`]).
    owed_peer_states: BTreeMap<[u8; 16], crate::home::wire::PeerState>,
    /// A pull a send or a wake asked for while one was running, with its
    /// reason: started when that one ends, since it may have begun before
    /// the home node had the message.
    pull_owed: Option<Reason>,
    /// A delete waiting for its `y`, and the question on screen.
    pub confirm: Option<(Confirm, Toast)>,
    /// A stored search result to open once its page has loaded.
    reveal_pending: Option<([u8; 16], u64)>,
    /// Each peer's latest position, by [`Fix::newer_than`]: the header's.
    pub positions: BTreeMap<[u8; 16], Fix>,
    /// Positions received since the store last wrote, in arrival order.
    fixes_received: Vec<Fix>,
    /// Every peer's fixes in time order, the track (1385): the store's at
    /// start, then every fix received or announced; ours from the shell's
    /// own fixes while it runs.
    /// Shared, so a frontend takes them without a copy.
    pub tracks: BTreeMap<[u8; 16], alloc::sync::Arc<Vec<tracks::TrackFix>>>,
    /// The map's time window for the tracks' lines: a map control, the
    /// current episode until the reader picks another.
    pub track_window: tracks::TrackWindow,
    /// The time zone view (1684): the zones' borders and each one's clock
    /// over the map, `z` in the terminal, the map menu's entry elsewhere.
    pub time_zones: bool,
    /// The fix time of the own fix the store last kept (1386): the way
    /// back is stored a row every [`OWN_TRACK_EVERY_SECS`] at most.
    own_stored_at: Option<i64>,
    /// The map, when open: it takes the conversation pane.
    pub map: Option<map::MapState>,
    /// What the shell draws the map with ([`UiEvent::MapSetup`]).
    pub map_setup: map::MapSetup,
    /// Where the map's picture is, in cells, on a shell that does not lay
    /// out the terminal's panes (the M9 draws it under its bar, the whole
    /// width, 1375); `None` for the terminal's conversation pane. The fit,
    /// the pan's step and the pins' cells go by it ([`UiModel::map_area`]).
    pub map_picture: Option<Rect>,
    /// The pane the map was opened from, which closing it returns to
    /// (1531): the list on a screen that showed the list, the chat on one
    /// that showed the chat.
    pub map_from: Pane,
    /// The left button held down on the map, a click or a drag.
    pub map_press: Option<map::MapPress>,
    /// Whether going to a place loads its region by itself (`map_autoload`,
    /// on in the shell's config by default; off here until the shell says).
    pub map_autoload: bool,
    /// The config's `map_theme`, which each frontend resolves against the
    /// theme it draws in (`picture::map_theme`); `None` for unset.
    pub map_theme: Option<ThemeFlag>,
    /// The word the map's title carries while a region loads.
    pub map_loading: Option<String>,
    /// A load runs or waits for what is on screen (1527): the title says
    /// [`MAP_LOADING_HERE`], the window's chip too.
    pub map_here: bool,
    /// The glyph test pattern is over the map (`g`, 1093).
    pub map_glyph_test: bool,
    /// The tick until which the map's title names the way it is drawn,
    /// after the drawing changed.
    pub map_named_until: Option<u64>,
    /// Regions to ask the shell for, handed out as [`UiEffect::LoadMap`]
    /// at the end of the fold.
    map_wants: Vec<map::MapWant>,
    /// The conversation whose draft [`UiModel::compose`] holds (1219).
    compose_for: Option<[u8; 16]>,
    /// The draft compose holds was sent: it empties after the fold.
    compose_cleared: bool,
    /// Drafts the store has not taken yet, by the `elapsed_ms` from which
    /// they are written ([`history::DRAFT_DEBOUNCE_MS`]).
    drafts_due: BTreeMap<[u8; 16], u64>,
    /// What the reader saw, and the read receipts owed (1231).
    pub reading: receipts::Reading,
    /// The propagation receipts owed (1395).
    pub arrivals: arrival::Arrivals,
    /// What the propagation node is to be asked about our uploads (1409),
    /// on a device that is its own node; a home's client waits for the
    /// home's [`crate::home::KIND_PN_STATUS`] instead. The shell restores
    /// it from the store ([`crate::pn_status::Asker::restore`]).
    pub pn_asker: crate::pn_status::Asker,
    /// The sync rounds this session finished: the asker asks a node once a
    /// round.
    pn_round: u64,
    /// How far a photo is scaled before it goes (1220, `photo_size` in
    /// the config): the shell scales by it before
    /// [`UiEvent::SendPhoto`].
    pub photo_size: photos::PhotoSize,
    /// The voice message that plays (1222): its bubble shows the pause
    /// button, every other the play button.
    pub playing: Option<voice::Playing>,
    /// The compose bar's buttons, sheets and staged photo (1270).
    pub bar: compose::Bar,
    /// The voice message being recorded, or the last one (1276).
    pub recording: recording::Recording,
    /// The level tracks of voice messages, by peer and message key
    /// (1276): the recording's own for one sent here, the decoded sound's
    /// for one the shell measured. Memory only; a bubble without one
    /// draws its stream's packet sizes.
    pub voice_tracks: BTreeMap<([u8; 16], u64), recording::LevelTrack>,
    /// The running position shares (1273).
    pub shares: share::Shares,
    /// The groups the reader is in, was invited to or left (1307), learned
    /// from the messages ([`groups`]).
    pub groups: groups::Groups,
    /// The group being made, while "New group" is open.
    pub group_draft: Option<groups::Draft>,
    /// A group's member sheet, while it is open.
    pub group_sheet: Option<groups::Sheet>,
    /// The groups whose state changed since the store last learned it
    /// (1307 G2): written by [`UiModel::take_writes`].
    groups_changed: BTreeSet<[u8; 16]>,
    /// The Browse tab and the browser in it (1337).
    pub browse: browse::Browse,
    /// The Rooms tab: Reticulum Relay Chat on a hub (1379).
    pub rooms: rooms::Rooms,
    /// The Explore tab (1578): the pages and the public rooms under one
    /// button.
    pub explore: explore::Explore,
    /// The language the model's own sentences from the catalogue are said
    /// in ([`strings::StrId`]): English unless the shell knows better, as
    /// the phone does.
    pub lang: strings::Lang,
    /// The debug overlay (1447): the map's zoom and its count of pins in
    /// view, which no user reads; `LOQUAX_FPS=1` turns it on with the
    /// frame counter.
    pub debug_overlay: bool,
    /// What the presentation built since the last event (1489,
    /// [`counters`]).
    ops: counters::Ops,
    /// Moves when a fold changed something a screen shows (1490, 1491):
    /// every event but the clock's three ([`UiEvent::is_clock`]), and
    /// those only when they moved what is shown (`UiModel::clock_marks`).
    /// A cache that outlives one fold keys on it, and a target's loop
    /// builds its view only when it moved ([`Presented`]), so a screen
    /// left alone builds nothing while the clock runs.
    pub revision: u64,
    /// The list's order as the last [`update`] left it (1490):
    /// [`UiModel::order`] reads it, [`UiModel::note_changes`] moves only
    /// the conversations whose revision moved. `None` until the first
    /// fold, and unused while one runs ([`UiModel::folding`]).
    order_cache: Option<OrderCache>,
    /// A fold is running: what it mutated is not yet noted, so caches
    /// keyed by revisions stand aside until [`UiModel::note_changes`].
    folding: bool,
    /// What the target's screen shows (1490): how many list rows and how
    /// many bubble rows are in view. The scene is built for this window
    /// and a margin as big again, never for the whole model
    /// ([`UiEvent::Viewing`] sets it; a window that scrolls itself asks
    /// for everything with [`Viewing::ALL`]).
    pub viewing: Viewing,
    /// The list's window as the first scene read after the last fold
    /// built it ([`scene::Scene`]'s list): checked against its stamp
    /// before every reuse. Interior-mutable since 1591: the read builds
    /// it, not the note ([`UiModel::scene_owed`]).
    built_list: core::cell::RefCell<Option<BuiltList>>,
    /// Where a list apart (the M9's compact list) last stood (1546): the
    /// selected conversation's peer and its line in the rows on screen.
    /// The cursor is the reader's: rows that move in the order move
    /// around it, its row keeps the cursor and the cursor keeps its line
    /// ([`UiModel::list_top`]). Interior-mutable: the scene's read keeps
    /// it.
    list_line: core::cell::Cell<Option<([u8; 16], usize)>>,
    /// The open chat as the first scene read after the last fold built
    /// it: the window of bubbles around the newest, or around the cursor
    /// while the keys are in the chat. Interior-mutable as `built_list`.
    built_chat: core::cell::RefCell<Option<BuiltChat>>,
    /// The stamps the last note's windows have (1591): the list's, and
    /// the open chat's with its conversation when one is shown. The
    /// scene's read builds a window and keeps it only while its stamp is
    /// the noted one, so what it keeps is what the note would have built;
    /// a model changed outside [`update`] since reads cold, as before.
    /// Before 1591 every note built both windows, and a pulled page
    /// folded a budget at a time (1575) paid the rows once per batch,
    /// dozens of times per page, for frames nobody drew (window 18:
    /// `finish` 9.2 of 16.3 ms per entry); now they are paid once per
    /// frame read, and not at all for a batch no frame follows.
    scene_owed: SceneOwed,
    /// The open conversation laid out as the terminal had it last
    /// (1490), keyed by [`UiModel::laid_key`]: a key that only moved the
    /// cursor restyles two blocks in place instead of laying thousands
    /// of rows. Interior-mutable: a read warms it.
    laid_cache: core::cell::RefCell<Option<LaidCache>>,
    /// The whole terminal view of one turn ([`view`]), keyed by its
    /// stamp: the viewport ask and the draw of one turn share a build,
    /// and a turn in which nothing changed builds nothing.
    view_cache: core::cell::RefCell<Option<(u64, View)>>,
    /// The fingerprint of the model-wide look the last [`update`] saw
    /// ([`UiModel::look_fingerprint`]), and how often it changed: part of
    /// every conversation-keyed cache's key, so a name heard, a day
    /// turned or a language set rebuilds what shows them.
    look_stamp: u64,
    look_rev: u64,
    /// Moves when a list row may have changed (1491): a conversation's
    /// revision, the look, a conversation come or gone. The phone makes
    /// its two lists by it, not by [`UiModel::revision`], which a node
    /// the Browse tab heard of moves too.
    lists_rev: u64,
    /// How many conversations the last fold left, for `lists_rev`.
    lists_len: usize,
}

/// Text a drag selected in the open conversation, from where the button went
/// down to where it is, each as a laid-out row and a column of the pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (usize, usize),
    pub head: (usize, usize),
}

/// A left button held down over the conversation: what it went down on,
/// and whether it has moved since, which makes the release a copy and not a
/// click.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Press {
    pub target: hit::Target,
    pub moved: bool,
}

impl<B: InputBuffer + Default> UiModel<B> {
    /// An empty screen with these correspondents already listed (`loquax tui
    /// --to`), focus on the conversation list.
    pub fn new(peers: &[[u8; 16]]) -> Self {
        let mut model = Self {
            conversations: Vec::new(),
            selected: 0,
            message_cursor: 0,
            compose: B::default(),
            focus: Pane::Conversations,
            toasts: Vec::new(),
            toasts_said: 0,
            palette: None,
            help: false,
            help_scroll: 0,
            address: None,
            ascii: false,
            tick: 0,
            quit: false,
            sending: VecDeque::new(),
            fetching: false,
            fetch_reason: None,
            fetch_since_ms: 0,
            fetches_done: 0,
            sync: None,
            elapsed_ms: 0,
            enhanced: false,
            send_on_enter: true,
            now: None,
            screen: (80, 24),
            scroll_top: None,
            kept_top: None,
            chat_anchor: None,
            detail: None,
            own_code: None,
            profile_open: false,
            settings: None,
            profile: profile::ProfileFacts::default(),
            detail_scroll: 0,
            mailbox: MailboxShown::default(),
            activity: 0,
            mouse: true,
            footer_hover: None,
            selection: None,
            copy_mode: None,
            press: None,
            tier: Tier::Mono,
            theme: Theme::Dark,
            search: None,
            names: Names::default(),
            show_blocked: false,
            show_archived: false,
            changed: BTreeSet::new(),
            changed_peers: BTreeSet::new(),
            made: Vec::new(),
            note_scope: Vec::new(),
            note_indices: Vec::new(),
            owed_changed: Vec::new(),
            owed_peers: BTreeSet::new(),
            owed_groups: BTreeSet::new(),
            listing_owed: BTreeSet::new(),
            bumped: Vec::new(),
            page_asked: None,
            read_pending: Vec::new(),
            ids: id_index::IdIndex::default(),
            forgotten: Vec::new(),
            home_client: false,
            history: false,
            list_apart: false,
            marks: Vec::new(),
            owed_petnames: BTreeMap::new(),
            petnames_in_flight: false,
            owed_petnames_changed: false,
            taken_petnames: BTreeMap::new(),
            peer_states: Vec::new(),
            owed_peer_states: BTreeMap::new(),
            pull_owed: None,
            confirm: None,
            reveal_pending: None,
            positions: BTreeMap::new(),
            fixes_received: Vec::new(),
            tracks: BTreeMap::new(),
            track_window: tracks::TrackWindow::default(),
            time_zones: false,
            own_stored_at: None,
            map: None,
            map_setup: map::MapSetup::default(),
            map_picture: None,
            map_press: None,
            map_from: Pane::Messages,
            map_autoload: false,
            map_theme: None,
            map_loading: None,
            map_here: false,
            map_glyph_test: false,
            map_named_until: None,
            map_wants: Vec::new(),
            compose_for: None,
            compose_cleared: false,
            drafts_due: BTreeMap::new(),
            reading: receipts::Reading::default(),
            arrivals: arrival::Arrivals::default(),
            pn_asker: crate::pn_status::Asker::default(),
            pn_round: 0,
            photo_size: photos::PhotoSize::default(),
            playing: None,
            bar: compose::Bar::default(),
            recording: recording::Recording::default(),
            voice_tracks: BTreeMap::new(),
            shares: share::Shares::default(),
            groups: groups::Groups::new(),
            group_draft: None,
            group_sheet: None,
            groups_changed: BTreeSet::new(),
            browse: browse::Browse::default(),
            rooms: rooms::Rooms::default(),
            explore: explore::Explore::default(),
            lang: strings::Lang::En,
            debug_overlay: false,
            ops: counters::Ops::new(),
            revision: 0,
            look_stamp: 0,
            look_rev: 0,
            lists_rev: 0,
            lists_len: 0,
            order_cache: None,
            folding: false,
            viewing: Viewing::default(),
            built_list: core::cell::RefCell::new(None),
            list_line: core::cell::Cell::new(None),
            built_chat: core::cell::RefCell::new(None),
            scene_owed: SceneOwed::default(),
            laid_cache: core::cell::RefCell::new(None),
            view_cache: core::cell::RefCell::new(None),
        };
        for peer in peers {
            model.conversation(*peer);
        }
        model
    }
}

impl<B: InputBuffer> UiModel<B> {
    /// Make compose hold the open conversation's draft (1219), after every
    /// fold. While the same conversation stays open, what compose holds is
    /// its draft's text, the terminal's typing written there; once another
    /// opens, the one left keeps its text, written at once, and compose
    /// takes the new one's.
    pub(crate) fn sync_compose(&mut self) {
        if core::mem::take(&mut self.compose_cleared) {
            self.compose.set_value("");
        }
        let open = self.conversations.get(self.selected).map(|c| c.peer);
        if open == self.compose_for {
            let Some(c) = self.conversations.get_mut(self.selected) else {
                return;
            };
            if c.draft.text != self.compose.value() {
                c.draft.text = self.compose.value().to_string();
                let peer = c.peer;
                self.draft_changed(peer, false);
            }
            return;
        }
        if let Some(left) = self.compose_for {
            if let Some(c) = self.conversations.iter_mut().find(|c| c.peer == left) {
                if c.draft.text != self.compose.value() {
                    c.draft.text = self.compose.value().to_string();
                }
                if self.drafts_due.contains_key(&left) {
                    self.draft_changed(left, true);
                }
            }
        }
        let was = core::mem::replace(&mut self.compose_for, open);
        let Some(c) = self.conversations.get_mut(self.selected) else {
            self.compose.set_value("");
            return;
        };
        // Text typed while no conversation was open, before the first one
        // came, goes to the one that opened when it has no draft.
        if was.is_none() && c.draft.text.is_empty() {
            if !self.compose.value().is_empty() {
                c.draft.text = self.compose.value().to_string();
                let peer = c.peer;
                self.draft_changed(peer, false);
            }
            return;
        }
        if self.compose.value() != c.draft.text {
            self.compose.set_value(&c.draft.text);
        }
    }

    /// [`UiEvent::Draft`]: `peer`'s draft as its frontend's field holds it,
    /// listed if new. Compose follows when it is the open one.
    fn set_draft(&mut self, peer: [u8; 16], text: String, reply: Option<[u8; 32]>) {
        let index = self.conversation(peer);
        let draft = &mut self.conversations[index].draft;
        if draft.text == text && draft.reply == reply {
            return;
        }
        if self.compose_for == Some(peer) && self.compose.value() != text {
            self.compose.set_value(&text);
        }
        let draft = &mut self.conversations[index].draft;
        draft.text = text;
        draft.reply = reply;
        self.draft_changed(peer, false);
    }
}

impl<B> UiModel<B> {
    /// Whether a detail view is up: a message's, the profile's, or the
    /// settings'.
    pub fn detail_open(&self) -> bool {
        self.detail.is_some() || self.profile_open || self.settings.is_some()
    }

    /// Close the detail view up, one step: a message's or the profile
    /// over the settings goes and leaves the settings; a section goes
    /// back to their list, the list closes them.
    pub fn close_detail(&mut self) {
        self.detail_scroll = 0;
        if self.detail.is_some() || self.profile_open {
            self.detail = None;
            self.profile_open = false;
            return;
        }
        self.settings = match self.settings {
            Some(Some(_)) => Some(None),
            _ => None,
        };
    }

    /// Open the settings (1450): their list with `None`, a section with
    /// it, over whatever detail view was up.
    pub fn show_settings(&mut self, at: Option<settings::Section>) -> Vec<UiEffect> {
        self.detail = None;
        self.profile_open = false;
        self.detail_scroll = 0;
        if at == Some(settings::Section::Profile) {
            // The profile is a screen of its own (the phone's activity,
            // the window's layer): the section opens it over the list.
            self.settings = Some(None);
            return self.show_profile(true);
        }
        self.settings = Some(at);
        Vec::new()
    }

    /// `peer`'s message went out: its draft goes, from the store at once,
    /// and compose empties with the next [`UiModel::sync_compose`].
    pub(crate) fn clear_draft(&mut self, peer: [u8; 16]) {
        let Some(c) = self.conversations.iter_mut().find(|c| c.peer == peer) else {
            return;
        };
        if c.draft.is_empty() {
            return;
        }
        c.draft = history::Draft::default();
        // Compose empties rather than writing its text back into it.
        self.compose_cleared = self.compose_for == Some(peer);
        self.draft_changed(peer, true);
    }

    /// The keymap's context: the terminal's keyboard protocol and the
    /// `send_on_enter` switch.
    pub fn ctx(&self) -> Ctx {
        let mut ctx = Ctx::NONE;
        if self.enhanced {
            ctx = ctx.with(Ctx::ENHANCED);
        }
        if !self.send_on_enter {
            ctx = ctx.with(keys::ENTER_BREAKS);
        }
        if self.search.is_some() {
            ctx = ctx.with(keys::SEARCHING);
        }
        if self.recording_bar().is_some() {
            ctx = ctx.with(keys::RECORDING);
        }
        ctx
    }

    /// The recording bar of the open conversation, while a voice message
    /// records for it (1276): the terminal's compose box is the bar then.
    pub fn recording_bar(&self) -> Option<scene::RecordingBar> {
        let peer = self.conversations.get(self.selected)?.peer;
        scene::recording_bar(self, peer)
    }

    /// What the open search looks for, when it looks for anything: the
    /// list shows results and the conversation marks the matches.
    pub fn query(&self) -> Option<&str>
    where
        B: InputBuffer,
    {
        self.search
            .as_ref()
            .map(|s| s.query.value())
            .filter(|q| !q.is_empty())
    }

    /// The open search's results, in the list's order.
    pub fn hits(&self) -> Vec<search::Hit>
    where
        B: InputBuffer,
    {
        let Some(query) = self.query() else {
            return Vec::new();
        };
        let stored = self.search.as_ref().map_or(&[][..], |s| &s.stored[..]);
        search::hits(
            &self.conversations,
            &self.names,
            &self.order(),
            query,
            stored,
        )
    }

    /// The colour tokens of the tier and theme.
    pub fn tokens(&self) -> &'static Tokens {
        tokens(self.tier, self.theme)
    }

    /// The selected conversation's messages.
    pub fn messages_of_selected(&self) -> &[Message] {
        self.conversations
            .get(self.selected)
            .map(|c| c.messages.as_slice())
            .unwrap_or(&[])
    }

    /// The index of `peer`'s conversation, made if new.
    fn conversation(&mut self, peer: [u8; 16]) -> usize {
        match self.conversations.iter().position(|c| c.peer == peer) {
            Some(index) => index,
            None => {
                self.names.meet(peer);
                self.made.push(peer);
                self.conversations.push(Conversation::new(peer));
                self.conversations.len() - 1
            }
        }
    }

    fn toast(&mut self, kind: ToastKind, text: String) {
        if self.toasts.len() >= TOAST_KEEP {
            let over = self.toasts.len() + 1 - TOAST_KEEP;
            self.toasts.drain(..over);
        }
        self.toasts_said = self.toasts_said.wrapping_add(1);
        self.toasts.push(Toast {
            kind,
            text,
            shown_at: self.tick,
        });
    }

    /// The toast to draw now, if any is still young enough.
    pub fn current_toast(&self) -> Option<&Toast> {
        // A question stays up until it is answered.
        if let Some((_, question)) = &self.confirm {
            return Some(question);
        }
        self.toasts
            .last()
            .filter(|t| self.tick.saturating_sub(t.shown_at) < TOAST_TICKS)
    }

    /// Open conversation `index`. The one left loses its `new` rule; the
    /// one opened keeps its own until it is left in turn, and shows its
    /// newest message at the bottom.
    fn select(&mut self, index: usize) {
        if index != self.selected {
            if let Some(left) = self.conversations.get_mut(self.selected) {
                left.new_rule = None;
            }
        }
        self.selected = index;
        self.scroll_top = None;
        self.chat_anchor = None;
        self.selection = None;
        // Where the list stands beside the chat, selecting is seeing.
        // Where the list is a screen of its own (1493: the phone, the
        // M9), moving the cursor shows nothing: [`UiEvent::Open`] reads.
        if !self.list_apart {
            self.read_out(index);
        }
        if let Some(conversation) = self.conversations.get(index) {
            self.message_cursor = conversation.messages.len().saturating_sub(1);
        }
    }

    /// Conversation `index` was looked at: its unread are read, here and,
    /// on a client of a home node, on the home's other devices.
    pub(crate) fn read_out(&mut self, index: usize) {
        let Some(conversation) = self.conversations.get_mut(index) else {
            return;
        };
        if conversation.unread == 0 {
            return;
        }
        let unread = core::mem::take(&mut conversation.unread);
        // A chat opened before its page loaded (1546, a board's two
        // cores): the row's kept ids name its unread, and its badge goes
        // with the count. Before, the count went to 0 with no mark.
        let kept = conversation.unread_ids.take();
        if let Some(listing) = conversation.listing.as_mut() {
            listing.unread_rows = 0;
        }
        self.changed_peers.insert(conversation.peer);
        if !self.home_client {
            return;
        }
        // The newest by arrival (1505), where the body's writers' order
        // may have put a late one anywhere.
        let mut ids: Vec<[u8; 32]> = conversation
            .received_by_arrival(true)
            .into_iter()
            .take(unread)
            .filter_map(|m| conversation.messages[m].id)
            .collect();
        // Newest first, the unloaded ones behind the loaded.
        if ids.len() < unread {
            let room = unread - ids.len();
            let missing: Vec<[u8; 32]> = kept
                .into_iter()
                .rev()
                .filter(|id| !ids.contains(id))
                .take(room)
                .collect();
            ids.extend(missing);
        }
        self.mark_out(crate::home::wire::MarkFlag::Read, ids);
    }

    /// Queue marks for the home node, split to the most one carries.
    pub(crate) fn mark_out(&mut self, flag: crate::home::wire::MarkFlag, ids: Vec<[u8; 32]>) {
        for chunk in ids.chunks(crate::home::wire::MARK_IDS_MAX) {
            self.marks.push(crate::home::wire::Mark {
                flag,
                ids: chunk.to_vec(),
            });
        }
    }

    /// What the presentation built since the last event or reset (1489):
    /// scenes, their nodes, list rows, bubbles, laid rows, sorts. `None`
    /// in a release build without the `counters` feature ([`counters`]).
    pub fn counters(&self) -> Option<counters::Counters> {
        self.ops.read()
    }

    /// What the presentation built since the model was made, across every
    /// event's fresh count (1514): what a batch of events cost together,
    /// a restore or a page folded one [`update`] each or as one
    /// [`Batch`]. `None` where [`UiModel::counters`] is.
    pub fn total_counters(&self) -> Option<counters::Counters> {
        self.ops.read_total()
    }

    /// Start the count afresh, as [`update`] does for every event: for a
    /// test that measures a frame no event asked for.
    pub fn reset_counters(&self) {
        self.ops.reset();
    }

    /// Count `n` more of `op` ([`counters`]).
    #[inline]
    pub(crate) fn count(&self, op: counters::Op, n: usize) {
        self.ops.add(op, n);
    }

    /// Note what the fold changed (1490), at the end of every [`update`]:
    /// the summaries' new tails (1463), each changed conversation's
    /// revision, the model-wide look. The fold's own change sets
    /// (`changed`, `changed_peers`, `groups_changed`) poke their
    /// conversations too, so an in-place change the summary cannot see (a
    /// delivery tick deep in the pane) still bumps; they drain with the
    /// store's writes, and a bump too many costs one rebuild, never a
    /// stale frame. The fold's sets hold only this fold's entries: each
    /// note moves them into the owed shadows the store drains, so the
    /// poke costs the fold's changes, never what accumulated.
    fn note_changes(&mut self) -> bool {
        self.note_changes_in(false)
    }

    /// [`UiModel::note_changes`], refreshing only the conversations a
    /// batch's events named when `scoped` (1594, [`Batch`]): those in
    /// `note_scope`, with the fold's own sets, the conversations made
    /// since the last note and the open one. The full walk refreshed the
    /// summary and the row fingerprint of every conversation at every
    /// note, 300 of them per chunk of a pulled page (window 19: 3.66 ms
    /// per entry on the board, about 11 ms a chunk). A build with debug
    /// assertions walks the rest as well, bumps what the scope missed,
    /// and fails the assertion naming it: the tests compare the two.
    fn note_changes_in(&mut self, scoped: bool) -> bool {
        // The fold's own sets are asked in place (1591): a set of the
        // poked peers built per note was the note's own allocations, paid
        // once per batch however small the batch.
        let changed = core::mem::take(&mut self.changed);
        let peers = core::mem::take(&mut self.changed_peers);
        let groups = core::mem::take(&mut self.groups_changed);
        let poked = |peer: &[u8; 16]| {
            peers.contains(peer)
                || groups.contains(peer)
                || changed
                    .range((*peer, 0)..=(*peer, u64::MAX))
                    .next()
                    .is_some()
        };
        let mut bumped = core::mem::take(&mut self.bumped);
        bumped.clear();
        let mut named = core::mem::take(&mut self.note_scope);
        let mut indices = core::mem::take(&mut self.note_indices);
        indices.clear();
        if scoped {
            // The scope's conversations, each found once.
            named.extend(peers.iter().chain(groups.iter()).chain(self.made.iter()));
            let mut last = None;
            for (peer, _) in changed.iter() {
                if last != Some(*peer) {
                    named.push(*peer);
                    last = Some(*peer);
                }
            }
            if let Some(open) = self.conversations.get(self.selected) {
                named.push(open.peer);
            }
            named.sort_unstable();
            named.dedup();
            let conversations = &self.conversations;
            indices.extend(
                named
                    .iter()
                    .filter_map(|peer| conversations.iter().position(|c| c.peer == *peer)),
            );
            indices.sort_unstable();
        }
        named.clear();
        self.note_scope = named;
        self.made.clear();
        let mut note = |index: usize, conversation: &mut Conversation, bumped: &mut Vec<usize>| {
            let refreshed = conversation
                .summary
                .refresh(conversation.peer, &conversation.messages);
            let stamp = conversation.row_fingerprint();
            if refreshed || stamp != conversation.stamp.get() || poked(&conversation.peer) {
                conversation.stamp = Rev::at(stamp);
                conversation.revision = Rev::at(conversation.revision.get().wrapping_add(1));
                self.listing_owed.insert(conversation.peer);
                bumped.push(index);
                return true;
            }
            false
        };
        if !scoped {
            for (index, conversation) in self.conversations.iter_mut().enumerate() {
                note(index, conversation, &mut bumped);
            }
        } else {
            for &index in &indices {
                note(index, &mut self.conversations[index], &mut bumped);
            }
            // The full walk beside it, in a build with debug assertions:
            // a conversation the scope missed is bumped all the same and
            // named.
            #[cfg(debug_assertions)]
            {
                let mut missed = Vec::new();
                for (index, conversation) in self.conversations.iter_mut().enumerate() {
                    if indices.binary_search(&index).is_err()
                        && note(index, conversation, &mut bumped)
                    {
                        missed.push(crate::address::to_hex(&conversation.peer));
                    }
                }
                bumped.sort_unstable();
                debug_assert!(
                    missed.is_empty(),
                    "a scoped note missed conversations a full walk bumps: {missed:?}"
                );
            }
        }
        self.note_indices = indices;
        // Into the owed shadows the store drains: moved whole when they
        // are drained already, no node copied.
        fn owe<T: Ord>(owed: &mut BTreeSet<T>, more: BTreeSet<T>) {
            match owed.is_empty() {
                true => *owed = more,
                false => owed.extend(more),
            }
        }
        if !changed.is_empty() {
            self.owed_changed.push(changed);
        }
        owe(&mut self.owed_peers, peers);
        owe(&mut self.owed_groups, groups);
        let look = self.look_fingerprint();
        let looked = look != self.look_stamp;
        if looked {
            self.look_stamp = look;
            self.look_rev = self.look_rev.wrapping_add(1);
        }
        // The fold is noted: the caches may stand on it again.
        self.folding = false;
        self.refresh_order(&bumped);
        // The list's and the chat's windows are built by the next scene
        // read ([`UiModel::built_list`]), not here (1591): only their
        // stamps are taken now.
        self.scene_owed = self.noted_stamps();
        let rows = looked || !bumped.is_empty() || self.lists_len != self.conversations.len();
        self.bumped = bumped;
        if rows {
            self.lists_len = self.conversations.len();
            self.lists_rev = self.lists_rev.wrapping_add(1);
        }
        rows
    }

    /// How often a list row may have changed (1491): what a target that
    /// makes only the lists, the phone's two, makes them again by.
    pub fn lists_revision(&self) -> u64 {
        self.lists_rev
    }

    /// What a clock event can move on a screen besides the
    /// conversations and the look (1491): a toast that aged out, a
    /// question, the fetch and the sends, the map's drawing named a
    /// moment, the header's fetch word and next sync. Two equal marks around a clock event mean it moved nothing
    /// shown. The tick itself is not here: only the terminal draws by it
    /// ([`UiModel::animating`]), and its loop stamps it while it does.
    fn clock_marks(&self) -> u64 {
        let mut hash = Fnv::new();
        hash.word(self.toasts.len() as u64);
        hash.word(self.toasts_said);
        hash.word(u64::from(self.current_toast().is_some()));
        hash.word(u64::from(self.confirm.is_some()));
        hash.word(u64::from(self.fetching));
        hash.word(self.sending.len() as u64);
        hash.word(u64::from(self.history));
        hash.word(u64::from(
            self.map_named_until.is_some_and(|until| self.tick < until),
        ));
        // The header's two readings of the monotonic clock: a quiet pull
        // said once it took a second, and the next sync's minute.
        hash.word(u64::from(fetch_shown(self)));
        hash.word(next_sync(self).map_or(u64::MAX, |at| at.unix_secs as u64));
        hash.done()
    }

    /// Whether the terminal draws something by the tick now (1491): the
    /// delivery spinner while a send or a fetch runs, a waveform playing,
    /// a recording's bar. Its loop builds a frame per tick then, and none
    /// otherwise.
    pub fn animating(&self) -> bool {
        !self.sending.is_empty()
            || self.fetching
            || self.playing.is_some()
            || self.recording.phase != recording::Phase::Idle
    }

    /// A change made outside [`update`] (1491): a shell that sets the
    /// model's fields itself (the window's abilities and viewport at its
    /// start), hands the hosted browser an event of its own, or reads
    /// history into the model, says so. The loops build once more, and
    /// the fold's built list and chat are dropped, as their stamps see
    /// only what a fold changes: the scene builds whole until the next
    /// fold builds them again. Before, the window's plus stayed away
    /// until the open chat changed (1490's cache kept the chat built
    /// before `run` gave the window its chooser).
    pub fn changed_outside(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        *self.built_list.get_mut() = None;
        *self.built_chat.get_mut() = None;
        self.scene_owed = SceneOwed::default();
        *self.view_cache.borrow_mut() = None;
    }

    /// How often the model-wide look changed (1490): the names, the
    /// language, the minute, what plays. Part of every
    /// conversation-keyed cache's key.
    pub fn look_rev(&self) -> u64 {
        self.look_rev
    }

    /// The fingerprint behind [`UiModel::look_rev`]: everything a list
    /// row or a bubble draws that belongs to no one conversation.
    fn look_fingerprint(&self) -> u64 {
        let mut hash = Fnv::new();
        hash.word(self.lang as u64);
        if let Some(now) = self.now {
            // To the minute: the rows' ago-words move by it.
            hash.word((now.unix_secs.div_euclid(60)) as u64);
            hash.word(now.offset_secs as u64);
        }
        hash.word(u64::from(self.home_client));
        hash.word(u64::from(self.reading.on));
        hash.word(u64::from(self.ascii));
        hash.word(u64::from(self.show_blocked));
        hash.word(u64::from(self.show_archived));
        if let Some(playing) = &self.playing {
            hash.bytes(&playing.peer);
            hash.word(playing.key);
        }
        hash.word(self.voice_tracks.len() as u64);
        for peer in self.shares.running.keys() {
            hash.bytes(peer);
        }
        for peer in self.bar.staged.keys() {
            hash.bytes(peer);
        }
        // The recording bar lives in the chat's compose node: its phase
        // and its growing track move the look while one runs.
        hash.bytes(&self.recording.peer);
        hash.word(match self.recording.phase {
            recording::Phase::Idle => 0,
            recording::Phase::Held { since } => 1 ^ (since << 3),
            recording::Phase::Locked { since } => 2 ^ (since << 3),
            recording::Phase::Paused => 3,
            recording::Phase::Preview => 4,
            recording::Phase::Cancelled => 5,
            recording::Phase::Sent => 6,
        });
        hash.word(self.recording.track.len() as u64);
        hash.word(self.recording.slide.0 as u64);
        hash.word(self.recording.slide.1 as u64);
        hash.word(u64::from(self.recording.previewing));
        hash.word(u64::from(self.recording.heard));
        hash.word(u64::from(self.recording.said.is_some()));
        self.names.fingerprint(&mut hash);
        hash.done()
    }

    /// Conversation `peer`'s row as the store keeps it (1490, schema
    /// 17), derived from its loaded messages as the list derives its
    /// row; `None` while nothing is loaded (the store keeps or heals
    /// its own then). A group the reader is invited to that no message
    /// reached yet, and none waits unloaded, has the invitation's row
    /// (1556): its line says "Einladung", from when the invitation came.
    /// Before, it had none, and a device that lists from kept rows lost
    /// it at every boot (1521c).
    pub fn listing_for(&self, peer: &[u8; 16]) -> Option<listing::Listing> {
        let conversation = self.conversations.iter().find(|c| c.peer == *peer)?;
        if conversation.messages.is_empty() {
            let invited = self
                .group(peer)
                .is_some_and(|g| g.standing == groups::Standing::Invited);
            return (invited && conversation.earlier == 0).then(|| listing::Listing {
                moved: conversation.touched,
                line: strings::StrId::GroupInvitation.text(self.lang).into(),
                ..listing::Listing::default()
            });
        }
        let messages = &conversation.messages;
        let last = messages.iter().rev().find(|m| !m.is_group_state());
        let updates = match last.is_some_and(Message::is_report) {
            true => conversation.run(messages.len().saturating_sub(1)).len(),
            false => 0,
        };
        let fixes = conversation.report_fixes();
        let mark = fixes
            .first()
            .map(|fix| leviculum_lxmf::telemetry::ShareMark::of_blob(&fix.telemetry))
            .unwrap_or_default();
        let line = last
            .filter(|m| !m.is_report())
            .map(|m| {
                chat::invitation_line(self, m)
                    .or_else(|| chat::reaction_line(self, peer, m))
                    .unwrap_or_else(|| chat::preview(m, self.lang))
            })
            .unwrap_or_default();
        Some(listing::Listing {
            moved: conversation.last_written(),
            line,
            ours: last.is_some_and(|m| m.outbound),
            at: last.and_then(|m| m.at),
            kind: listing::LastKind::of_word(last.map_or("", |m| chat::kind(m))),
            reaction: last
                .filter(|m| m.is_reaction())
                .and_then(|m| m.reaction())
                .map(|(_, content)| content),
            mark: last
                .filter(|m| m.outbound)
                .map(|m| chat::Mark::of(chat::shown(m.delivery, self))),
            read_at: last.filter(|m| m.outbound).and_then(|m| {
                match chat::shown(m.delivery, self) {
                    Some(DeliveryState::Read { at }) => Some(at),
                    _ => None,
                }
            }),
            unread_rows: conversation.unread_rows() as u32,
            updates: updates as u32,
            appearance: conversation.appearance(),
            report_times: fixes.iter().map(Fix::order_time).collect(),
            share_every: mark.every,
            share_ended: mark.ended,
        })
    }

    /// Conversation `peer` as a device keeps it to list it at boot
    /// (1521, [`listing::Row`]): its row derived from the loaded
    /// messages, or the one it was restored with while none is loaded;
    /// `None` for a conversation with neither.
    pub fn row(&self, peer: &[u8; 16]) -> Option<listing::Row> {
        let conversation = self.conversations.iter().find(|c| c.peer == *peer)?;
        let listing = self
            .listing_for(peer)
            .or_else(|| conversation.listing.clone())?;
        // The unread ids: those still unloaded, then the loaded ones,
        // the newest `unread` of them.
        let mut ids: Vec<[u8; 32]> = match conversation.earlier {
            0 => Vec::new(),
            _ => conversation.unread_ids.to_vec(),
        };
        ids.extend(
            conversation
                .messages
                .iter()
                .filter(|m| !m.outbound && !m.is_aside())
                .filter_map(|m| m.id),
        );
        let keep = conversation.unread.min(listing::UNREAD_IDS);
        let ids = ids.split_off(ids.len().saturating_sub(keep));
        Some(listing::Row {
            peer: *peer,
            unread: u32::try_from(conversation.unread).unwrap_or(u32::MAX),
            activity: conversation.activity,
            unread_ids: ids,
            listing,
        })
    }

    /// The list from kept rows alone (1521), each with how many messages
    /// the store holds for it, and the names heard: no message read, the conversations listed
    /// as their rows say, the list ordered and the scene's window built
    /// once. A conversation's messages load when it opens
    /// (`UiModel::ask_page`). The counters are the restore's.
    pub fn restore_rows(
        &mut self,
        rows: Vec<(listing::Row, usize)>,
        names: Vec<([u8; 16], String)>,
    ) {
        self.ops.reset();
        // The names the peers announced, as the fold of their entries
        // would have heard them.
        for (peer, name) in names {
            let _ = self.names.heard(peer, Some(&name));
        }
        for (row, earlier) in rows {
            let peer = row.peer;
            self.restore_one(history::Restored {
                peer,
                pinned: false,
                flags: history::Flags::default(),
                unread: row.unread as usize,
                messages: Vec::new(),
                earlier,
                draft: history::Draft::default(),
                group: None,
                listing: Some(row.listing),
            });
            if let Some(conversation) = self.conversations.iter_mut().find(|c| c.peer == peer) {
                conversation.activity = row.activity;
                conversation.unread_ids.set(row.unread_ids);
                self.activity = self.activity.max(row.activity);
            }
        }
        self.note_changes();
        self.revision = self.revision.wrapping_add(1);
    }

    /// Give every message from here on a key above `floor` (1521): a
    /// board keys the messages it loads from its ring by their entry's
    /// sequence, and the ones it folds live must never meet them.
    pub fn keys_above(&mut self, floor: u64) {
        self.activity = self.activity.max(floor);
    }

    /// The marks a fold made, for the shell to hand the home node.
    pub fn take_marks(&mut self) -> Vec<crate::home::wire::Mark> {
        core::mem::take(&mut self.marks)
    }

    /// The conversations in the list's order: the pinned first, then the
    /// rest, each by when their newest message was written, most recent
    /// first ([`Conversation::last_written`], 1445: a history pulled at
    /// once keeps its order), then by when it came here; those without one
    /// after, in the order they came. Only those the list shows
    /// ([`UiModel::shows`]): a blocked peer's outside the `blocked` view.
    ///
    /// The last fold's cache answers when it is current (1490): a sort is
    /// paid only when something moved, and only for what moved. A model
    /// changed outside [`update`], or read during a fold, sorts afresh.
    pub fn order(&self) -> Vec<usize> {
        if !self.folding {
            if let Some(cache) = self.order_cache.as_ref().filter(|c| c.current(self)) {
                return cache.order.clone();
            }
        }
        self.order_cold()
    }

    /// [`UiModel::order`] sorted afresh, the whole list counted.
    fn order_cold(&self) -> Vec<usize> {
        let mut order: Vec<usize> = self.listed().collect();
        self.count(counters::Op::Sorts, 1);
        self.count(counters::Op::Sorted, order.len());
        // Each key once (1463): `last_written` is a walk when the summary
        // is behind.
        order.sort_by_cached_key(|&i| self.order_key(i));
        order
    }

    /// Conversation `i`'s place in the list, total over the keys: the
    /// index last, so the incremental insert lands exactly where the
    /// stable full sort puts a mover among equals.
    fn order_key(&self, i: usize) -> OrderKey {
        let c = &self.conversations[i];
        (
            core::cmp::Reverse(c.pinned),
            core::cmp::Reverse(c.last_written()),
            core::cmp::Reverse(c.activity),
            i,
        )
    }

    /// Whether conversation `i` is in the list [`UiModel::order`] shows.
    fn lists(&self, i: usize) -> bool {
        let c = &self.conversations[i];
        self.shows(&c.peer) && (self.show_blocked || c.flags.archived == self.show_archived)
    }

    /// Bring the order cache up to this fold (1490), `bumped` naming the
    /// conversations whose revision moved: their keys are recomputed and
    /// only they are re-placed, so a pull's page costs the page, not the
    /// list. Anything structural (another count of conversations, a view
    /// switched, the look moved — a block lives in the look) rebuilds
    /// whole, counted as one full sort.
    fn refresh_order(&mut self, bumped: &[usize]) {
        let keep = self.order_cache.take().filter(|cache| {
            cache.keys.len() == self.conversations.len()
                && cache.show_blocked == self.show_blocked
                && cache.show_archived == self.show_archived
                && cache.look_rev == self.look_rev
        });
        let mut cache = match keep {
            Some(cache) => cache,
            None => {
                self.order_cache = Some(OrderCache::fresh(self));
                return;
            }
        };
        let mut moved = 0usize;
        for &i in bumped {
            let key = self.lists(i).then(|| self.order_key(i));
            if cache.keys[i] == key {
                continue;
            }
            if cache.keys[i].is_some() {
                if let Some(at) = cache.order.iter().position(|&o| o == i) {
                    cache.order.remove(at);
                }
            }
            if let Some(key) = key {
                let to = cache
                    .order
                    .partition_point(|&o| cache.keys[o].is_some_and(|k| k < key));
                cache.order.insert(to, i);
            }
            cache.keys[i] = key;
            moved += 1;
        }
        if moved > 0 {
            self.count(counters::Op::Sorts, 1);
            self.count(counters::Op::Sorted, moved);
        }
        self.order_cache = Some(cache);
    }

    /// The slice of `order` the scene's list is built for (1490): the
    /// rows in view and half a screen either side, the selected row
    /// inside. `(first, how many, the first row on screen)`, the last for
    /// a list apart only ([`UiModel::list_top`]).
    ///
    /// A list apart knows its first row on screen exactly (1546), so its
    /// window is the rows in view and nothing else (1595): the selected
    /// row is one of them, and every key or fold that moves the screen
    /// moves the window's stamp, so a margin was built and never drawn
    /// (1591: every row built is cloned into each scene the board draws).
    pub(crate) fn list_window(&self, order: &[usize]) -> (usize, usize, Option<usize>) {
        let visible = self.viewing.list_rows.max(1);
        if self.viewing.apart {
            let build = order.len().min(visible);
            let top = self.list_top(order);
            let first = top.min(order.len() - build);
            return (first, build, Some(top));
        }
        let build = order.len().min(visible.saturating_mul(2));
        let at = order.iter().position(|&i| i == self.selected).unwrap_or(0);
        let first = at.saturating_sub(visible / 2).min(order.len() - build);
        (first, build, None)
    }

    /// The first row of `order` a list apart shows (1546): the cursor's
    /// own rule, one for every compact list. The cursor never moves by
    /// itself: while the selected conversation stays selected it keeps
    /// the line it stood on, and the rows a pull or an arrival reorders
    /// move around it. A key that moves the selection moves the line by
    /// as many rows as the selection moved in the order, kept on the
    /// screen; the line gives way only where the list ends. A selection
    /// never seen stands as low as the rows above it let it, at most on
    /// the last line: the window the M9 had before 1546.
    pub(crate) fn list_top(&self, order: &[usize]) -> usize {
        let visible = self.viewing.list_rows.max(1);
        let Some(at) = order.iter().position(|&i| i == self.selected) else {
            return 0;
        };
        let peer = self.conversations[self.selected].peer;
        let line = match self.list_line.get() {
            Some((held, line)) if held == peer => line,
            Some((held, line)) => match order
                .iter()
                .position(|&i| self.conversations[i].peer == held)
            {
                Some(was) => (line + at).saturating_sub(was),
                None => at,
            },
            None => at,
        };
        let line = line.min(visible - 1).min(at);
        let top = (at - line).min(order.len().saturating_sub(visible));
        self.list_line.set(Some((peer, at - top)));
        top
    }

    /// The fingerprint of everything the list's window is built from:
    /// the look, the viewport, the whole order and the windowed
    /// conversations' revisions.
    fn list_stamp(&self, order: &[usize], first: usize, len: usize, top: Option<usize>) -> u64 {
        let mut hash = Fnv::new();
        hash.word(top.map_or(u64::MAX, |top| top as u64));
        hash.word(self.look_rev);
        hash.word(self.viewing.list_rows as u64);
        hash.word(self.selected as u64);
        hash.word(first as u64);
        hash.word(len as u64);
        for &i in order {
            hash.word(i as u64);
        }
        for &i in &order[first..first + len] {
            hash.word(self.conversations[i].revision());
        }
        hash.done()
    }

    /// Whether message `index` has a bubble row of its own: what the
    /// chat's window counts ([`chat::bubbles`] builds notes and orphan
    /// reactions besides, which the margin covers).
    fn bubble_row(&self, c: usize, index: usize) -> bool {
        let conversation = &self.conversations[c];
        !conversation.messages[index].is_aside() && !conversation.report_folded(index)
    }

    /// The window of conversation `c`'s messages the chat is built for
    /// (1490): the newest rows two screens deep, or the rows around the
    /// cursor once the keys took it above them. `(from, to)`, message
    /// indices.
    fn chat_window(&self, c: usize) -> (usize, usize) {
        let len = self.conversations[c].messages.len();
        let target = self.viewing.chat_rows.max(1).saturating_mul(2);
        let back = |end: usize, mut rows: usize| {
            let mut from = end;
            while from > 0 && rows > 0 {
                from -= 1;
                if self.bubble_row(c, from) {
                    rows -= 1;
                }
            }
            from
        };
        let mut from = back(len, target);
        let mut to = len;
        let cursor = self.message_cursor.min(len.saturating_sub(1));
        if self.focus == Pane::Messages && cursor < from {
            // The cursor climbed above the window: it follows, the
            // cursor a quarter in from its top.
            from = back(cursor + 1, (target / 4).max(1));
            let mut rows = 0;
            to = from;
            while to < len && rows < target {
                if self.bubble_row(c, to) {
                    rows += 1;
                }
                to += 1;
            }
        }
        (from, to)
    }

    /// The fingerprint of everything the open chat's window is built
    /// from.
    fn chat_stamp(&self, c: usize, from: usize, to: usize) -> u64 {
        let conversation = &self.conversations[c];
        let mut hash = Fnv::new();
        hash.bytes(&conversation.peer);
        hash.word(conversation.revision());
        hash.word(self.look_rev);
        hash.word(self.viewing.chat_rows as u64);
        hash.word(from as u64);
        hash.word(to as u64);
        hash.done()
    }

    /// The stamps of the windows the fold left (1591, [`SceneOwed`]),
    /// from the order the note just refreshed.
    fn noted_stamps(&self) -> SceneOwed {
        let list = self.order_cache.as_ref().map(|cache| {
            let (first, len, top) = self.list_window(&cache.order);
            self.list_stamp(&cache.order, first, len, top)
        });
        let shown = !self.viewing.apart || self.focus != Pane::Conversations;
        let chat = (shown && self.conversations.get(self.selected).is_some()).then(|| {
            let (from, to) = self.chat_window(self.selected);
            (self.selected, self.chat_stamp(self.selected, from, to))
        });
        SceneOwed { list, chat }
    }

    /// The list's window as the fold left it (1490, 1591): the rows in
    /// view and their margin, built once a fold was noted and kept while
    /// its stamp is current; `None` while a fold runs or after the model
    /// changed outside [`update`] (the scene builds whole then). Built by
    /// the first read after a note, counted into the event that read it.
    pub(crate) fn built_list(&self) -> Option<scene::List> {
        if self.folding {
            return None;
        }
        let order = self.order();
        let (first, len, top) = self.list_window(&order);
        let stamp = self.list_stamp(&order, first, len, top);
        let current = self.built_list.borrow().as_ref().map(|b| b.stamp) == Some(stamp);
        if !current {
            if self.scene_owed.list != Some(stamp) {
                return None;
            }
            let rows = scene::rows(self, order[first..first + len].to_vec());
            *self.built_list.borrow_mut() = Some(BuiltList {
                stamp,
                first,
                total: order.len(),
                top,
                rows,
            });
        }
        let built = self.built_list.borrow();
        let built = built.as_ref()?;
        Some(scene::List {
            id: scene::NodeId::List,
            rows: built.rows.clone(),
            first: built.first,
            total: built.total,
            top: built.top,
            empty: strings::Text::key(strings::StrId::NoConversations),
            a11y: scene::A11y::of(
                scene::NodeId::List,
                scene::Role::List,
                strings::Text::key(strings::StrId::TabChats),
            ),
        })
    }

    /// Conversation `c`'s chat window as the fold left it (1490, 1591),
    /// as [`UiModel::built_list`] keeps the list's.
    pub(crate) fn built_chat(&self, c: usize) -> Option<scene::Chat> {
        if self.folding {
            return None;
        }
        let (from, to) = self.chat_window(c);
        let stamp = self.chat_stamp(c, from, to);
        let current = self.built_chat.borrow().as_ref().map(|b| b.stamp) == Some(stamp);
        if !current {
            if self.scene_owed.chat != Some((c, stamp)) {
                return None;
            }
            let chat = scene::chat_in(self, c, from, to);
            *self.built_chat.borrow_mut() = Some(BuiltChat { stamp, chat });
        }
        self.built_chat.borrow().as_ref().map(|b| b.chat.clone())
    }

    /// The conversations the list shows, in no order: [`UiModel::order`]
    /// before its sort.
    pub fn listed(&self) -> impl Iterator<Item = usize> + '_ {
        // The blocked view lists the blocked, archived or not.
        (0..self.conversations.len()).filter(|&i| {
            let c = &self.conversations[i];
            self.shows(&c.peer) && (self.show_blocked || c.flags.archived == self.show_archived)
        })
    }

    fn bump(&mut self) -> u64 {
        self.activity += 1;
        self.activity
    }

    /// The rows the compose box shows.
    pub fn compose_rows(&self) -> usize
    where
        B: InputBuffer,
    {
        // The recording bar (1276): its row, and the line under it when it
        // says something.
        if let Some(bar) = self.recording_bar() {
            return 1 + usize::from(bar.notice.is_some());
        }
        self.compose
            .value()
            .split('\n')
            .count()
            .clamp(1, COMPOSE_MAX_ROWS)
    }

    /// The panes' inner sizes on this screen.
    pub fn sizes(&self) -> PaneSizes
    where
        B: InputBuffer,
    {
        PaneSizes::for_screen(self.screen.0, self.screen.1, self.compose_rows())
    }

    /// The header's rows inside the conversation pane: none, or the mailbox
    /// row and a rule on a wide one; then the share's chip row while our
    /// position share runs in the open conversation (1273).
    pub(crate) fn header_rows(&self) -> usize
    where
        B: InputBuffer,
    {
        let mailbox = match self.sizes().messages.0 >= WIDE_HEADER {
            true => 2,
            false => 0,
        };
        mailbox + self.chip_row().is_some() as usize
    }

    /// The share's chip row in the conversation pane (1273), counted from
    /// the pane's first inner row: under the mailbox's rows, while our
    /// share runs in the open conversation and the map is not up.
    pub fn chip_row(&self) -> Option<usize>
    where
        B: InputBuffer,
    {
        let peer = self.conversations.get(self.selected)?.peer;
        (self.map.is_none() && self.share(&peer).is_some()).then(|| {
            match self.sizes().messages.0 >= WIDE_HEADER {
                true => 2,
                false => 0,
            }
        })
    }

    /// The rows of the conversation pane that carry messages.
    pub fn message_rows(&self) -> usize
    where
        B: InputBuffer,
    {
        self.sizes().messages.1.saturating_sub(self.header_rows())
    }

    /// The open conversation laid out at the pane's width.
    fn laid(&self) -> alloc::sync::Arc<layout::Laid>
    where
        B: InputBuffer,
    {
        let selected = (self.focus == Pane::Messages).then_some(self.message_cursor);
        let width = self.sizes().messages.0;
        let Some(conversation) = self.conversations.get(self.selected) else {
            let laid = alloc::sync::Arc::new(layout::Laid::empty(width, self.tokens(), self.lang));
            self.count(counters::Op::RowsLaid, laid.rows.len());
            return laid;
        };
        let key = self.laid_key(conversation, width);
        let mut slot = self.laid_cache.borrow_mut();
        if let Some(entry) = slot.as_mut().filter(|entry| entry.key == key) {
            if entry.selected == selected {
                return alloc::sync::Arc::clone(&entry.laid);
            }
            // Only the cursor moved (1490): the two blocks it left and
            // took are restyled in place, nothing else is laid.
            let draw = self.draw_context(selected);
            let laid = alloc::sync::Arc::make_mut(&mut entry.laid);
            let mut held = true;
            let mut spliced = 0;
            for (at, on) in [(entry.selected, false), (selected, true)] {
                let Some(index) = at.filter(|&i| i < conversation.messages.len()) else {
                    continue;
                };
                held &= layout::respan(laid, conversation, index, width, &draw, on);
                spliced += laid.spans.get(index).map_or(0, |span| span.block.1);
            }
            if held {
                entry.selected = selected;
                self.count(counters::Op::RowsLaid, spliced);
                return alloc::sync::Arc::clone(&entry.laid);
            }
        }
        let laid = alloc::sync::Arc::new(layout::conversation(
            conversation,
            width,
            &self.draw_context(selected),
        ));
        self.count(counters::Op::RowsLaid, laid.rows.len());
        *slot = Some(LaidCache {
            key,
            selected,
            laid: alloc::sync::Arc::clone(&laid),
        });
        laid
    }

    /// What [`layout::conversation`] draws with, besides the
    /// conversation: everything here is in the laid cache's key, or is
    /// the selection the splice restyles.
    fn draw_context(&self, selected: Option<usize>) -> layout::Draw<'_>
    where
        B: InputBuffer,
    {
        layout::Draw {
            now: self.now,
            tick: self.tick,
            ascii: self.ascii,
            selected,
            tokens: self.tokens(),
            mark: self.query(),
            home_client: self.home_client,
            receipts: self.reading.on,
            tracks: Some(&self.voice_tracks),
            lang: self.lang,
            names: Some(&self.names),
        }
    }

    /// The laid cache's key (1490): the conversation's identity and
    /// revision, its content's cheap ends (so a fold that is still
    /// running misses rather than answers stale), the pane's width and
    /// the look. The cursor is not here: it is spliced.
    fn laid_key(&self, conversation: &Conversation, width: usize) -> u64
    where
        B: InputBuffer,
    {
        let mut hash = Fnv::new();
        hash.bytes(&conversation.peer);
        hash.word(conversation.revision());
        hash.word(conversation.messages.len() as u64);
        for message in [conversation.messages.first(), conversation.messages.last()]
            .into_iter()
            .flatten()
        {
            hash.word(message.activity);
            hash.word(message.at.map_or(0, |at| at.unix_secs as u64));
        }
        hash.word(conversation.notes.len() as u64);
        hash.word(conversation.earlier as u64);
        if let Some(rule) = conversation.new_rule {
            hash.word(rule.at as u64);
            hash.word(rule.count as u64);
        }
        // The suffix glyphs change in place: a sweep of words, far
        // cheaper than a row, as insurance for a delivery set outside
        // any fold.
        for message in &conversation.messages {
            hash.word(delivery_word(message.delivery));
        }
        hash.word(width as u64);
        hash.word(self.look_stamp);
        hash.word(u64::from(self.ascii));
        hash.word(self.lang as u64);
        hash.word(self.tier as u64);
        hash.word(self.theme as u64);
        if let Some(query) = self.query() {
            hash.bytes(query.as_bytes());
        }
        // The delivery spinner and a playing waveform move with the
        // tick; a still pane does not re-lay for it.
        if !self.sending.is_empty() || self.fetching || self.playing.is_some() {
            hash.word(self.tick);
        }
        hash.done()
    }

    /// The first row on screen of `total` laid-out rows.
    fn top_of(&self, total: usize) -> usize
    where
        B: InputBuffer,
    {
        let bottom = total.saturating_sub(self.message_rows());
        self.scroll_top.unwrap_or(bottom).min(bottom)
    }
}

/// What a target's loop presented last (1491): it builds its view, scene
/// or lists only when the stamp it asks with moved, the model's
/// [`UiModel::revision`] and whatever of its own moves outside the model
/// (a size, a map picture's epoch). Before 1491 the terminal and the
/// window built their whole presentation fifty times a second with
/// nothing changed, and only the present was skipped (1455).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Presented {
    at: Option<u64>,
    builds: u64,
}

impl Presented {
    /// Whether the loop builds now: `stamp` moved since the last build,
    /// or nothing was built yet. Counts the builds it allows.
    pub fn due(&mut self, stamp: u64) -> bool {
        if self.at == Some(stamp) {
            return false;
        }
        self.at = Some(stamp);
        self.builds += 1;
        true
    }

    /// The next [`Presented::due`] says yes, whatever the stamp: an input
    /// the loop handles outside the model (a map picture that landed, a
    /// screen to clear) needs a frame.
    pub fn owe(&mut self) {
        self.at = None;
    }

    /// The frame just built shows the model at `stamp`: a fold the build
    /// made itself (the terminal's viewport) is in it, and does not owe
    /// another.
    pub fn drawn(&mut self, stamp: u64) {
        self.at = Some(stamp);
    }

    /// How many builds [`Presented::due`] allowed.
    pub fn builds(&self) -> u64 {
        self.builds
    }

    /// The stamp of `revision` and the loop's own `extra` words.
    pub fn stamp(revision: u64, extra: &[u64]) -> u64 {
        let mut hash = Fnv::new();
        hash.word(revision);
        for &word in extra {
            hash.word(word);
        }
        hash.done()
    }
}

/// What the model is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiEvent {
    /// A key, routed to the focused pane, or to the palette or help on top.
    Key(KeyEvent),
    /// A palette command run directly, as a GUI menu would.
    Run(PaletteCommand),
    /// The shell's 250 ms tick: the spinner's frame, the toasts' age.
    Tick,
    /// The time of day: before the first frame and on every tick in the
    /// terminal, whenever a key script says so.
    Clock(WallTime),
    /// The shell's monotonic clock, in milliseconds from an origin of its
    /// choosing: the mailbox schedule runs on it, and a sync it finds due
    /// starts here.
    Elapsed(u64),
    /// The machine woke from suspend (`crate::sync::resumed`).
    Resumed,
    /// The platform says the device's network is `kind` now (1298),
    /// `changed` when it is another network than before: another WLAN,
    /// WLAN to mobile data, back from none. Also said with `changed` off
    /// after the phone's Doze, so the first wake re-runs the evaluation.
    /// Asks the shell to re-evaluate its interfaces
    /// ([`UiEffect::Network`]); nothing the screen shows changes here.
    Network {
        kind: crate::home::NetworkKind,
        changed: bool,
    },
    /// On a client of a home node: the home node said its archive grew
    /// past this device's cursor ([`crate::home::watch`]), so a pull starts,
    /// or follows the one running.
    HomeWake,
    /// The screen's size in cells.
    Resize { width: u16, height: u16 },
    /// What the target's screen shows of the list and the open chat
    /// (1490): the scene is built for this window plus a margin. Sent at
    /// start and whenever the window changes; a target that keeps whole
    /// scenes sends [`Viewing::ALL`].
    Viewing(Viewing),
    /// A click, a wheel turn, a drag or a move, at a screen cell.
    Mouse(MouseEvent),
    /// The mailbox the stored record or a run selected.
    MailboxChosen { node: [u8; 16], by_user: bool },
    /// One event from the message engine.
    Engine(OutboxEvent),
    /// The oldest running send finished; the error text when it failed.
    SendDone(Result<(), String>),
    /// The oldest running send ended with its message on a link to the peer
    /// and no delivery proof (`SendError::Unproven`): shown `◇`, sent and
    /// not proven, with the runner's text in a toast.
    SendUnproven(String),
    /// The oldest running send moved onto this leg.
    SendLeg(Leg),
    /// The fetch finished: how many messages were new, or why it failed.
    FetchDone(Result<usize, String>),
    /// The propagation node `node` answered [`UiEffect::AskPn`] (1409):
    /// the statuses, or `None` when the link failed, the request went
    /// unanswered or the node refused, which leaves every id open.
    PnAnswered {
        node: [u8; 16],
        answer: Option<Vec<([u8; 32], leviculum_lxmf::lev_status::MessageStatus)>>,
    },
    /// A line a runner or the stamp worker said along the way (the auto
    /// fallback's "trying a propagation node", a mailbox adoption), the
    /// lines the CLI writes to stderr.
    Notice(String),
    /// The store's answer to [`StoreRead::Earlier`]: older messages of
    /// `peer`'s conversation, oldest first, and how many it holds before
    /// them still.
    Earlier {
        peer: [u8; 16],
        messages: Vec<Message>,
        earlier: usize,
    },
    /// The store's answer to [`StoreRead::Search`].
    StoredHits { query: String, hits: Vec<StoredHit> },
    /// How the shell draws the map: the cell's pixels and the zooms its
    /// regions reach.
    MapSetup(map::MapSetup),
    /// A region load asked with [`UiEffect::LoadMap`] moved on.
    MapLoad(MapLoad),
    /// Open `peer`'s conversation, listed if new, as a tap on it does in a
    /// frontend without keys (the Android app): its unread are read, and on
    /// a client of a home node marked read for the home's other devices.
    Open { peer: [u8; 16] },
    /// Leave the open conversation for the list, as the system's back
    /// button does on a phone: none is open, so what arrives stays unread.
    Close,
    /// Send `body` to `peer` in their conversation, opened first, as the
    /// compose pane's `Enter` does with its text.
    Write { peer: [u8; 16], body: String },
    /// React with `content` to the message `target` in `peer`'s
    /// conversation, as the picker's choice does ([`reactions`]).
    React {
        peer: [u8; 16],
        target: [u8; 32],
        content: String,
    },
    /// Send `body` to `peer` as a reply to the message `target`
    /// ([`replies`]).
    Reply {
        peer: [u8; 16],
        target: [u8; 32],
        body: String,
    },
    /// Send a copy of the message `key` (its [`Message::activity`]) of
    /// `from`'s conversation to `to` ([`replies`]).
    Forward {
        from: [u8; 16],
        key: u64,
        to: [u8; 16],
    },
    /// Send the message `key` of `peer`'s conversation again, which was
    /// not sent (its mark's tap, 1232): the failed one goes, a new one with
    /// its text and fields takes its place at the end.
    SendAgain { peer: [u8; 16], key: u64 },
    /// Delete the messages `keys` of `peer`'s conversation without asking,
    /// as `y` after `d` does for one: a frontend that asked already.
    Delete { peer: [u8; 16], keys: Vec<u64> },
    /// Send the phone's own position to `peer`, the user asked for it
    /// (1201): a report with no text, its one field the Telemeter of
    /// `location` with the fix time as its time. Reading a position sends
    /// nothing; only this does.
    SharePosition {
        peer: [u8; 16],
        location: leviculum_lxmf::telemetry::Location,
    },
    /// Make `name` the reader's name for `peer`, the empty one clearing it,
    /// as the palette's `rename` does with what was typed: a frontend with
    /// a dialog of its own (1139).
    Rename { peer: [u8; 16], name: String },
    /// Make `state` the reader's word on `peer`: blocked, pinned, archived,
    /// muted (1146). A frontend changes the field it offers in
    /// [`UiModel::peer_state`] and hands the whole back.
    PeerState {
        peer: [u8; 16],
        state: crate::home::wire::PeerState,
    },
    /// The reader verified `peer`'s identity in person, or takes it back.
    Verify { peer: [u8; 16], verified: bool },
    /// Mark `peer`'s conversation unread on this device, or read as
    /// opening it would.
    MarkUnread { peer: [u8; 16], unread: bool },
    /// Delete `peer`'s conversation without asking, from this device or,
    /// on a client of a home node, from every device of the home.
    DeleteConversation { peer: [u8; 16], everywhere: bool },
    /// What is typed in `peer`'s conversation and not sent, and the message
    /// a reply being written answers (1219): a frontend's own text field
    /// says so on every change. The store keeps it once typing paused.
    Draft {
        peer: [u8; 16],
        text: String,
        reply: Option<[u8; 32]>,
    },
    /// These bubbles of `peer`'s chat are on screen now, by their keys
    /// ([`Message::activity`]), by the frontend's rule (the terminal's
    /// since 1529: a bubble whose last line is on screen): a frontend says
    /// it on every change (1231, [`receipts`]).
    Viewport { peer: [u8; 16], keys: Vec<u64> },
    /// A scroll key in the open chat on a target that lays its own window
    /// of it (the M9, 1529): `cmd` over the target's lines, by the one
    /// rule the terminal's keys take ([`pager::step`]).
    Lines {
        cmd: ScrollCmd,
        window: pager::Window,
    },
    /// The app is in front with the screen on, or went behind: an
    /// Android activity resumed or paused, a terminal's focus.
    Front(bool),
    /// The reader's switch "Send read receipts" (1233), set: the settings
    /// page's row, the palette's command toggles it.
    ReadReceipts(bool),
    /// Send a photo to `peer` (1220): `data` is the picture the shell
    /// scaled by [`UiModel::photo_size`] already (`crate::photo`), in
    /// `format` (`jpg`, `png`, `gif`). It goes as one message with the
    /// image field alone ([`photos::photo_fields`]).
    SendPhoto {
        peer: [u8; 16],
        format: String,
        data: Vec<u8>,
    },
    /// The reader's photo size (1220), set: the settings page's row.
    PhotoSize(photos::PhotoSize),
    /// The profile (1443), opened or closed: [`intent::Intent::Profile`],
    /// and Back from it.
    Profile { open: bool },
    /// What the shell knows of the reader for the profile, anew.
    ProfileFacts(Box<profile::ProfileFacts>),
    /// What the mailbox said of itself (1642, `/about`), or that it is too
    /// old to say; the rest of the profile's facts stay.
    MailboxTold(Box<mailbox_page::Told>),
    /// How this device reaches its mailbox now (1642); the rest stays.
    MailboxReach(Box<mailbox_page::Reach>),
    /// The profile's "Share my address" where the share is a copy: the
    /// window and the terminal ([`intent::Intent::ShareCode`]).
    ShareCode,
    /// A photo the camera took for `peer` (1270), scaled by the shell as
    /// [`UiEvent::SendPhoto`]'s: it waits in the compose bar, and the next
    /// [`UiEvent::Write`] sends it with the text as its caption.
    StagePhoto {
        peer: [u8; 16],
        format: String,
        data: Vec<u8>,
    },
    /// The staged photo's cross: it goes nowhere.
    Unstage { peer: [u8; 16] },
    /// The plus or the position button opened its sheet (1270).
    OpenSheet(compose::Sheet),
    /// The sheet was dismissed, or a row of it handed to the shell.
    CloseSheet,
    /// The shell's newest own fix, or none (1270): the position sheet
    /// shows it and its age.
    OwnFix(Option<leviculum_lxmf::telemetry::Location>),
    /// A position typed or pasted on a shell without a receiver (1371):
    /// read by [`by_hand::parse`], it becomes the own fix.
    EnterPosition { text: String },
    /// The map's centre as the shell's map shows it, or none while it has
    /// no map (1371): what the position sheet's "map centre" row takes.
    MapCentre(Option<leviculum_lxmf::telemetry::Location>),
    /// The tracks' time window chosen in the map's menu (1385, 1419): the
    /// window's single choice, the default among them.
    TrackWindow(tracks::TrackWindow),
    /// The position sheet's "map centre" row: the map's centre is the own
    /// fix (1371).
    FixAtMapCentre,
    /// A peer verb of a frontend without the model at hand (1371, the
    /// window's row menu): what [`UiModel::peer_word`] makes of it, or
    /// nothing.
    PeerWord {
        peer: intent::Target,
        verb: verbs::PeerVerb,
    },
    /// A friend's code the shell read from a picture (1371): its
    /// conversation opens, as a code typed into the palette does.
    CodeRead(String),
    /// A node announced its position to the mesh (1302,
    /// [`crate::mesh_position`]): `identity` the hash of the identity the
    /// announce proved, `fix` its position, `name` the name a shell knows
    /// for it, if any, and `age` how many seconds ago the node heard it.
    /// A state, not a message: it joins the peer's telemetry positions,
    /// the newest fix winning ([`UiModel::mesh_position`]), and the store
    /// keeps none of it.
    MeshPosition {
        identity: [u8; 16],
        fix: leviculum_lxmf::telemetry::Location,
        name: Option<String>,
        age: u32,
    },
    /// The map card's "Add" on a node nearby that is no contact (1302):
    /// its conversation opens, named by its announced name.
    AddNearby { peer: [u8; 16] },
    /// The position sheet's "Now, once": the own fix goes to `peer` as a
    /// report.
    ShareFix { peer: [u8; 16] },
    /// One of the position sheet's other rows (1273): a share of `span`
    /// starts for `peer`'s chat ([`share`]).
    StartShare { peer: [u8; 16], span: share::Span },
    /// The share's Stop, on the chat's chip or in Settings, Privacy: one
    /// last report marked as the end goes.
    StopShare { peer: [u8; 16] },
    /// A tap on a photo's bubble: the shell shows it whole
    /// ([`UiEffect::ShowPhoto`]).
    ViewPhoto { peer: [u8; 16], key: u64 },
    /// Send a file to `peer` (1221): its name as picked and its bytes. It
    /// goes as one message with the attachments field alone
    /// ([`files::file_fields`]); one past [`files::LARGEST`] is refused
    /// with a toast.
    SendFile {
        peer: [u8; 16],
        name: String,
        data: Vec<u8>,
    },
    /// A tap on a bubble's `index`th file: the shell saves and opens it
    /// ([`UiEffect::OpenFile`]).
    OpenFile {
        peer: [u8; 16],
        key: u64,
        index: usize,
    },
    /// Send a recording to `peer` (1222): Opus in Ogg as
    /// `crate::voice::encode` made it; it goes as one message with the
    /// audio field alone ([`voice::voice_fields`]).
    SendVoice { peer: [u8; 16], data: Vec<u8> },
    /// A tap on a voice message's play button: the shell plays it
    /// ([`UiEffect::PlayVoice`]), the bubble shows pause.
    PlayVoice { peer: [u8; 16], key: u64 },
    /// A tap on the pause button of the one that plays
    /// ([`UiEffect::PauseVoice`]).
    PauseVoice { peer: [u8; 16], key: u64 },
    /// The shell's player came to the end of a voice message.
    VoiceEnded { peer: [u8; 16], key: u64 },
    /// The microphone's gestures, its level every 50 ms and the recording
    /// bar's buttons, for `peer`'s chat (1276, [`recording::Input`]).
    Record {
        peer: [u8; 16],
        input: recording::Input,
    },
    /// The level track of a voice message the shell decoded (1276,
    /// [`recording::LevelTrack::from_pcm`]): its bubble draws it from
    /// now on instead of the stream's packet sizes.
    VoiceTrack {
        peer: [u8; 16],
        key: u64,
        track: recording::LevelTrack,
    },
    /// One of the reader's verbs on groups (1307): "New group", a tick,
    /// Next, the name, Create, Join, the member sheet, Leave, Remove.
    Group(groups::Verb),
    /// The home's answer to [`UiEffect::CreateGroup`]: the group's
    /// address, or why it was not made.
    GroupMade(Result<[u8; 16], groups::NotMade>),
    /// The home's answer to [`UiEffect::AddMembers`] (1307 G2): the
    /// group's address, or why nobody was added.
    MembersAdded(Result<[u8; 16], String>),
    /// The home's answer to [`UiEffect::DeleteGroup`] (1307 pass four):
    /// the group, or why not.
    GroupDeleted(Result<[u8; 16], String>),
    /// A hand of [`UiModel::take_petnames`] ended (1635): the petnames the
    /// home node took. Those still owed go with the next hand.
    PetnamesHanded {
        taken: Vec<([u8; 16], Option<String>)>,
    },
    /// A tab of the bar was chosen (1337): the Browse tab comes to the
    /// front, or leaves it for the chats or the map.
    Tab(scene::TabId),
    /// What the hosted browser of the Browse tab is told (1337): a verb,
    /// the page a fetch brought, a NomadNet node an announce named.
    Browse(browse::BrowseEvent),
    /// What the Rooms are told (1379): what the hub said, and the
    /// reader's verbs in the Rooms tab.
    Rooms(rooms::RoomsEvent),
    /// The reader's verbs in the Explore tab (1578).
    Explore(explore::ExploreEvent),
}

impl UiEvent {
    /// The clock's own events, which every loop folds several times a
    /// second (1491): they move [`UiModel::revision`] only when they
    /// moved what a screen shows.
    pub fn is_clock(&self) -> bool {
        matches!(self, Self::Clock(_) | Self::Tick | Self::Elapsed(_))
    }
}

/// How a region load goes, as the shell reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MapLoad {
    /// It started: the title says so.
    Started(map::MapWant),
    /// It landed, and [`UiEvent::MapSetup`] has the region already.
    Done(map::MapWant),
    /// It failed. `toast` is the line to show, `None` where the shell said
    /// it once already (no internet, a box backing off).
    Failed {
        want: map::MapWant,
        toast: Option<String>,
    },
    /// Whether a load runs or waits for what is on screen (1527, the
    /// loader's `loading_here`): "Loading the map of this area".
    Here(bool),
}

/// What the shell is asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiEffect {
    /// Re-evaluate the interfaces for a network change
    /// ([`UiEvent::Network`]): every rendezvous rotation hears it
    /// (`crate::home::rendezvous::Network`), drops a dead connection,
    /// dials again, asks its paths, bounded and idempotent.
    Network {
        kind: crate::home::NetworkKind,
        changed: bool,
    },
    /// Send `body` to `to` the way `loquax send --via auto` does. The shell
    /// runs sends one at a time in this order and reports each with
    /// [`UiEvent::SendDone`]. `fields` go with it as they are: a reaction's
    /// ([`reactions`]).
    Send {
        to: [u8; 16],
        body: String,
        fields: Vec<Field>,
    },
    /// Drain the mailbox, as `loquax fetch` does; reported with
    /// [`UiEvent::FetchDone`].
    Fetch,
    /// Ask the propagation node `node` what became of `ids`, our uploads'
    /// `transient_id`s, over a link of its own that is never identified
    /// (1409, `crate::home::client::ask_pn_status`); reported with
    /// [`UiEvent::PnAnswered`].
    AskPn { node: [u8; 16], ids: Vec<[u8; 32]> },
    /// A line of the asking for the log at info level
    /// (`PN_STATUS_ASKED …`, `PN_STATUS_SKIPPED reason=…`).
    PnLine(String),
    /// Leave.
    Quit,
    /// Draw the whole screen again from scratch (`Ctrl-l`).
    Redraw,
    /// Put this text on the clipboard (OSC 52 in the terminal).
    Copy(String),
    /// What the hosted browser of the Browse tab asks of the platform
    /// (1337): a fetch, a download, the clipboard, its stores. Never
    /// [`lnomad::Effect::Write`] or [`lnomad::Effect::Quit`], which the
    /// core does itself ([`browse::effects_of`]).
    Browse(lnomad::Effect),
    /// What the Rooms ask of their hub link (1379, `crate::rooms_link`).
    Rooms(rooms::RoomAsk),
    /// Make the contacts file say this about its peer, its other lines kept
    /// ([`crate::names::save_contact`]).
    Contact(Contact),
    /// Hand this URL to the system's handler for it: an `http`, `https` or
    /// `mailto` link in a body, which [`classify_link`] let through.
    Open(String),
    /// Capture the mouse, or let the terminal have it.
    Mouse(bool),
    /// A line worth keeping after the screen is gone: the shell prints it
    /// once the terminal is restored. A toast is cut to the screen's width,
    /// and a failure's full text is often longer.
    Notice(String),
    /// Ask the store for what the screen has not loaded.
    Store(StoreRead),
    /// A line for the shell's log (`tui.log`), not for the screen: a
    /// telemetry blob that did not decode, kept raw with its message.
    Log(String),
    /// Load this region into the maps directory, reported with
    /// [`UiEvent::MapLoad`] (1089).
    LoadMap(map::MapWant),
    /// Draw the map the `n`th way of [`map::MapSetup::drawings`]; the shell
    /// answers with a new [`UiEvent::MapSetup`] (1093).
    MapDrawing(usize),
    /// Import contacts and history from the messengers found in their
    /// default places (`crate::import`), never the identity, which needs a
    /// typed yes outside the screen; the shell answers with notices.
    Import,
    /// Write `key = value` to the config ([`crate::config::save`]), its
    /// other lines kept: a setting the reader changed on screen, which the
    /// next start reads (1233, `read_receipts`).
    Remember {
        key: &'static str,
        value: &'static str,
    },
    /// Write `key = <hash in hex>` to the config, or take the key out
    /// with `None` (1577: `rooms_hub`, the public rooms' hub the settings
    /// chose; `rooms_told`, the hub the reader was told about).
    RememberHash {
        key: &'static str,
        hash: Option<[u8; 16]>,
    },
    /// Show the photo of the message `key` of `peer`'s conversation whole
    /// (1220): the phone's viewer, the window's overlay. Its bytes are in
    /// the message's fields ([`photos::photo_bytes`]).
    ShowPhoto { peer: [u8; 16], key: u64 },
    /// Save the `index`th file of the message `key` of `peer`'s
    /// conversation under the app's files and open it (1221): the
    /// system's handler for its kind, the phone's chooser. Its bytes are
    /// in the message's fields ([`files::file_at`]).
    OpenFile {
        peer: [u8; 16],
        key: u64,
        index: usize,
    },
    /// Play the voice message `key` of `peer`'s conversation (1222): the
    /// phone's player, the desktop's WAV file in the system's player. Its
    /// bytes are in the message's fields ([`voice::voice_parts`]).
    PlayVoice { peer: [u8; 16], key: u64 },
    /// Stop the one that plays.
    PauseVoice { peer: [u8; 16], key: u64 },
    /// Ask the home to make the group `name` with `members`, each under
    /// the name the reader knows it by (1307, `/group`); answered with
    /// [`UiEvent::GroupMade`]. The invitations are English on the wire,
    /// whatever the reader's language (1348).
    CreateGroup {
        name: String,
        members: Vec<([u8; 16], String)>,
    },
    /// Ask the home to add `members` to its group `group` (1307 G2,
    /// `/groupadd`); answered with [`UiEvent::MembersAdded`].
    AddMembers {
        group: [u8; 16],
        members: Vec<([u8; 16], String)>,
    },
    /// Ask the home to delete its group `group` for everyone (1307 pass
    /// four, `/groupdelete`); answered with [`UiEvent::GroupDeleted`].
    DeleteGroup { group: [u8; 16] },
    /// Delete every map region on this device (1371, the palette's
    /// `delete all maps` once asked): the shell's, which holds the files,
    /// and tells the model the regions left with [`UiEvent::MapSetup`].
    DeleteAllMaps,
    /// Send the picture (`photo`) or file at `path` to `peer` (1371, the
    /// terminal's typed path): the shell reads it as the window's chooser
    /// hands one over, and answers with [`UiEvent::SendPhoto`] or
    /// [`UiEvent::SendFile`], or a notice.
    Attach {
        peer: [u8; 16],
        path: String,
        photo: bool,
    },
    /// Read the friend's code in the picture at `path` (1371): the shell
    /// answers with [`UiEvent::CodeRead`], or a notice.
    ScanCode { path: String },
    /// What the recorder of `peer`'s chat is to do (1276): start, pause,
    /// resume, stop, hand over, drop, play the preview, tick the haptics.
    Recording {
        peer: [u8; 16],
        effect: recording::Effect,
    },
}

/// Fold one event into the model.
pub fn update<B: InputBuffer + Default>(model: &mut UiModel<B>, event: UiEvent) -> Vec<UiEffect> {
    // Each event's work is counted on its own (1489).
    model.ops.reset();
    // The clock's events move the revision only when they moved what is
    // shown (1491): fifty a second while nothing changes, before, and
    // every loop built its whole view for each.
    let clock = event.is_clock();
    let marks = clock.then(|| model.clock_marks());
    let mut effects = step(model, event);
    let noted = model.note_changes();
    model.keep_top();
    if !clock || noted || !effects.is_empty() || marks != Some(model.clock_marks()) {
        model.revision = model.revision.wrapping_add(1);
    }
    model.ask_page(&mut effects);
    effects
}

/// Fold many events as one (1514): a restore, a pulled page. Each event
/// takes every step [`update`] takes but the note: the changes of all of
/// them are noted once at the end, so the list is ordered and the scene's
/// window built once for the batch, not once per event (O(conversations)
/// each, a restore of 4067 entries 17 minutes on the M9). The counters
/// are the batch's. An empty batch changes nothing.
pub fn update_all<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    events: impl IntoIterator<Item = UiEvent>,
) -> Vec<UiEffect> {
    let mut batch = Batch::new(model);
    for event in events {
        batch.fold(event);
    }
    batch.finish()
}

/// [`update_all`] fed one event at a time (1514): for a source that hands
/// its events out by a callback, a store's restore, without holding them
/// all first. Noted at [`Batch::finish`]; a batch dropped unfinished is
/// noted then, so the model's caches never stand on an unnoted fold.
pub struct Batch<'a, B: InputBuffer + Default> {
    model: &'a mut UiModel<B>,
    effects: Vec<UiEffect>,
    folded: bool,
    /// Whether every event folded so far named its conversations
    /// (1594): the note then refreshes those
    /// ([`UiModel::note_changes_in`], the names in the model's
    /// `note_scope`), not all. False once an event whose reach the batch
    /// cannot name was folded.
    scoped: bool,
}

/// The conversations a home's entry names when folded (1594): the
/// sender of a received message, the recipient of a sent one, the
/// announcer of a name; a mark names none of its own (the messages it
/// marks are in the fold's change sets). `None` for an event of another
/// reach: its batch notes every conversation.
fn named_by(event: &UiEvent) -> Option<Option<[u8; 16]>> {
    match event {
        UiEvent::Engine(OutboxEvent::Received { source, .. }) => Some(Some(*source)),
        UiEvent::Engine(OutboxEvent::HomeSent { destination, .. }) => Some(Some(*destination)),
        UiEvent::Engine(OutboxEvent::Announced { destination, .. }) => Some(Some(*destination)),
        UiEvent::Engine(OutboxEvent::HomeMarked { .. }) => Some(None),
        _ => None,
    }
}

impl<'a, B: InputBuffer + Default> Batch<'a, B> {
    /// A batch over `model`; its count starts afresh with its first
    /// event, so an empty batch leaves the model as it was.
    pub fn new(model: &'a mut UiModel<B>) -> Self {
        Self {
            model,
            effects: Vec::new(),
            folded: false,
            scoped: true,
        }
    }

    /// Fold `event`, its note owed to the end.
    pub fn fold(&mut self, event: UiEvent) {
        if !core::mem::replace(&mut self.folded, true) {
            self.model.ops.reset();
        }
        match named_by(&event) {
            // Each conversation once (1603): a set, sorted at the note;
            // one per event grew with the batch, 4464 peers in a ring's
            // rebuild, a reallocation of 8192 of them.
            Some(Some(peer)) if self.scoped => {
                let scope = &mut self.model.note_scope;
                if scope.last() != Some(&peer) && !scope.contains(&peer) {
                    scope.push(peer);
                }
            }
            Some(_) => {}
            None => self.scoped = false,
        }
        let effects = step(self.model, event);
        self.effects.extend(effects);
    }

    /// Fold `event`, an entry the home recorded at `recorded_at` (unix
    /// seconds, 0 for unknown), with the model's clock held to that time
    /// for this event alone (1594, [`recorded_now`]): a message's time
    /// then is the same whenever its entry is folded, by the pull, by a
    /// page read later or by a boot, and reading a page never lifts its
    /// conversation in the list.
    pub fn fold_recorded(&mut self, event: UiEvent, recorded_at: i64) {
        let now = self.model.now;
        self.model.now = recorded_now(now, recorded_at);
        self.fold(event);
        self.model.now = now;
    }

    /// Take out the conversations the fold left with nothing to list
    /// (1521c): no message and no row kept for one ([`UiModel::row`]), an
    /// invitation's group no message reached or a chat whose every message
    /// a mark deleted. A device that lists its conversations from kept rows
    /// at the next boot has no row for them, so the boot that folded them
    /// must not list them either. Noted with the batch; the conversations
    /// taken out.
    pub fn drop_rowless(&mut self) -> Vec<[u8; 16]> {
        let rowless: Vec<[u8; 16]> = self
            .model
            .conversations
            .iter()
            .map(|c| c.peer)
            .filter(|peer| self.model.row(peer).is_none())
            .collect();
        for peer in &rowless {
            self.folded = true;
            let _ = self.model.remove_conversation(*peer);
        }
        rowless
    }

    /// Note what the batch changed, once, and hand back what its events
    /// asked for.
    pub fn finish(mut self) -> Vec<UiEffect> {
        self.note();
        core::mem::take(&mut self.effects)
    }

    fn note(&mut self) {
        if !core::mem::take(&mut self.folded) {
            return;
        }
        self.model
            .note_changes_in(core::mem::replace(&mut self.scoped, true));
        self.model.keep_top();
        self.model.revision = self.model.revision.wrapping_add(1);
        self.model.ask_page(&mut self.effects);
    }
}

impl<B: InputBuffer + Default> Drop for Batch<'_, B> {
    fn drop(&mut self) {
        self.note();
    }
}

/// The clock a home's entry is folded under (1594): when the home
/// recorded the entry (`recorded_at`, unix seconds; 0 or less for
/// unknown), in the model's offset, so no message gets a time later
/// than the home took it. A received message's shown time is its
/// writer's held to the clock (`written_time`): folded under the model's
/// own clock it was held to whenever the fold ran, so a writer whose
/// clock ran ahead of ours got the pull's time in the row the pull kept
/// and a later time when a page of it was read again (window 19: the
/// preloaded row rose over a newer arrival to the list's top, and the
/// cursor with it).
///
/// The recorded time stands even where the model's clock is behind it
/// (1597): the board's day is the newest stamp it has seen, told to the
/// model on the next second's tick, so a pulled page is folded in the
/// turn it came under a clock that has not yet seen its stamp. Held to
/// the earlier of the two, window 21's "mark control" came out at the
/// stale day, 16 s before the home took it; the row kept said so, and
/// the page read after a reboot, under a clock past the stamp, gave the
/// message its recorded time and moved the row. Nor does it wait for
/// the model to know a time at all: a boot's rebuild and a page read
/// before the first stamp folded with none and gave the messages no
/// time, where a pull that knew the day gave them theirs, so a row's
/// time depended on when its entries happened to be folded. An entry
/// the home recorded carries its time; one without (`recorded_at` 0)
/// folds under the model's clock, `None` while it knows none.
pub fn recorded_now(now: Option<WallTime>, recorded_at: i64) -> Option<WallTime> {
    match recorded_at > 0 {
        true => Some(WallTime {
            unix_secs: recorded_at,
            offset_secs: now.map_or(0, |now| now.offset_secs),
        }),
        false => now,
    }
}

/// One event folded with what follows every fold, short of the note
/// ([`UiModel::note_changes`]): the half [`update`] and [`update_all`]
/// share.
fn step<B: InputBuffer + Default>(model: &mut UiModel<B>, event: UiEvent) -> Vec<UiEffect> {
    // Until the fold is noted, revision-keyed caches stand aside (1490).
    model.folding = true;
    let mut effects = fold(model, event);
    effects.extend(model.map_wants.drain(..).map(UiEffect::LoadMap));
    // Whatever hid the open conversation, a block, the `blocked` view or a
    // restore, another the list shows takes its place.
    model.settle();
    // A reaction has no block for the cursor to stand on.
    model.off_reactions();
    // Compose holds the open conversation's draft (1219).
    model.sync_compose();
    // The map holds the conversation's place: the focus going to the
    // conversation or compose takes it away, and without it there is no
    // map to focus.
    match (model.map.is_some(), model.focus) {
        (true, Pane::Messages | Pane::Compose) => model.map = None,
        (false, Pane::Map) => model.focus = Pane::Messages,
        _ => {}
    }
    // What the reader saw, and the receipts it owes: on this fold's sends,
    // or alone once due (1231).
    model.read_step(&mut effects);
    // The propagation receipts owed (1395): on this fold's sends, or alone
    // once due.
    model.arrival_step(&mut effects);
    // Every message sent says the sender takes read receipts (1230), while
    // the switch is on, understands groups (1307) and acknowledges
    // propagated post (1395), always.
    receipts::mark_sends(&mut effects, groups::client_caps(model.reading.on));
    // And names this device as the map's writer (1482).
    receipts::name_writer(&mut effects, model.address);
    // The list's summaries up to what the event did (1463), and the
    // revisions over them (1490), are the caller's note: every change
    // the fold made is noted there, so the caches keyed by a revision
    // reuse everything else.
    effects
}

impl<B: InputBuffer + Default> UiModel<B> {
    /// A selected conversation whose messages a restart deferred (1490):
    /// its newest page, asked once, whatever event selected it.
    fn ask_page(&mut self, effects: &mut Vec<UiEffect>) {
        if let Some(conversation) = self.conversations.get(self.selected) {
            if conversation.messages.is_empty()
                && conversation.earlier > 0
                && self.page_asked != Some(conversation.peer)
            {
                self.page_asked = Some(conversation.peer);
                effects.push(UiEffect::Store(history::StoreRead::Earlier {
                    peer: conversation.peer,
                    before: u64::MAX,
                    through: None,
                }));
            }
        }
    }
}

fn fold<B: InputBuffer + Default>(model: &mut UiModel<B>, event: UiEvent) -> Vec<UiEffect> {
    match event {
        UiEvent::Tab(tab) => {
            let mut effects = explore::tab(model, tab);
            effects.extend(browse::tab(model, tab));
            effects.extend(rooms::tab(model, tab));
            effects
        }
        UiEvent::Browse(event) => {
            let effects = browse::fold(model, event);
            // A page heard moves Explore's order, at most once a second
            // and never under a finger (1618).
            explore::settle(model, false);
            effects
        }
        UiEvent::Rooms(event) => rooms::fold(model, event),
        UiEvent::Explore(event) => explore::fold(model, event),
        UiEvent::MapSetup(setup) => {
            if let Some(state) = model.map.as_mut() {
                state.zoom = state.zoom.clamp(setup.zooms.0, setup.zooms.1);
            }
            // The drawing changed under the reader: the title names it a
            // moment. The first setup changes nothing the reader chose.
            let old = &model.map_setup;
            if !old.drawings.is_empty() && old.drawing != setup.drawing {
                model.map_named_until = Some(model.tick + MAP_NAMED_TICKS);
            }
            model.map_setup = setup;
            Vec::new()
        }
        UiEvent::MapLoad(load) => {
            map_load(model, load);
            Vec::new()
        }
        UiEvent::Key(key) if key.kind == KeyEventKind::Release => Vec::new(),
        // The Browse tab in front owns the keys and the mouse, as lnomad's
        // own screen does (1369): its quit leaves the tab.
        UiEvent::Key(key) if model.browse.showing() => {
            model.touched();
            browse::app(model, lnomad::AppEvent::Key(key))
        }
        // The Rooms tab in front: its input line has the keys (1379).
        UiEvent::Key(key) if model.rooms.showing() => {
            model.touched();
            rooms::key(model, key)
        }
        // The Explore tab's list in front: its search field and its
        // cursor have the keys (1578).
        UiEvent::Key(key) if explore::Explore::showing(model) => {
            model.touched();
            explore::key(model, key)
        }
        UiEvent::Key(key) => {
            model.touched();
            on_key(model, key)
        }
        UiEvent::Run(command) => run(model, command),
        UiEvent::Tick => {
            model.tick = model.tick.wrapping_add(1);
            let tick = model.tick;
            model
                .toasts
                .retain(|t| tick.saturating_sub(t.shown_at) < TOAST_TICKS);
            Vec::new()
        }
        UiEvent::Clock(now) => {
            model.now = Some(now);
            model.shares_due()
        }
        UiEvent::Elapsed(ms) => {
            model.elapsed_ms = ms;
            explore::settle(model, false);
            auto_sync(model)
        }
        UiEvent::Resumed => schedule(model, SyncEvent::Resume),
        UiEvent::HomeWake => pull_for(model, Reason::Wake),
        UiEvent::Viewing(viewing) => {
            model.viewing = viewing;
            Vec::new()
        }
        UiEvent::Resize { width, height } => {
            model.screen = (width, height);
            model.selection = None;
            model.press = None;
            // A taller screen brings the overlays' bottoms nearer.
            model.help_scroll = model.help_scroll.min(help_bounds(model).1);
            model.detail_scroll = model.detail_scroll.min(detail_bounds(model).1);
            // The terminal's screen is the Browse tab's whole page, which
            // lnomad's own screen fills (1369); only the terminal says
            // this event, the phone and the window name the tab's cells.
            browse::app(model, lnomad::AppEvent::Resize(width, height))
        }
        UiEvent::Mouse(mouse) if model.browse.showing() => {
            model.touched();
            browse::app(model, lnomad::AppEvent::Mouse(mouse))
        }
        UiEvent::Mouse(mouse) => {
            let effects = on_mouse(model, mouse);
            model.touched();
            effects
        }
        UiEvent::Viewport { peer, keys } => {
            let earlier = model.earlier_shown(peer, &keys);
            model.set_viewport(peer, keys);
            earlier
        }
        UiEvent::Lines { cmd, window } => {
            model.touched();
            page_window(model, cmd, &window);
            model.earlier_wanted()
        }
        UiEvent::Front(front) => {
            model.set_front(front);
            Vec::new()
        }
        UiEvent::Network { kind, changed } => vec![UiEffect::Network { kind, changed }],
        UiEvent::ReadReceipts(on) => model.set_receipts(on),
        UiEvent::SendPhoto { peer, format, data } => model.send_photo(peer, &format, data),
        UiEvent::StagePhoto { peer, format, data } => {
            model.stage_photo(peer, format, data);
            Vec::new()
        }
        UiEvent::Unstage { peer } => {
            model.unstage(peer);
            Vec::new()
        }
        UiEvent::OpenSheet(sheet) => {
            model.open_sheet(sheet);
            Vec::new()
        }
        UiEvent::CloseSheet => {
            model.bar.sheet = None;
            Vec::new()
        }
        UiEvent::OwnFix(fix) => {
            if let (Some(own), Some(location)) = (model.address, fix) {
                let at = model.now.map(|now| now.unix_secs);
                let t = match at {
                    Some(now) if location.last_update > now + telemetry::FUTURE_TOLERANCE_SECS => {
                        now
                    }
                    _ => location.last_update,
                };
                let own_fix = Fix {
                    peer: own,
                    via: own,
                    message: None,
                    received_at: at,
                    location,
                    telemetry: Vec::new(),
                    source: telemetry::Source::Telemetry,
                };
                let mut point = tracks::TrackFix::of(&own_fix);
                point.t = t;
                tracks::insert(
                    alloc::sync::Arc::make_mut(model.tracks.entry(own).or_default()),
                    point,
                );
                // The way back (1386): the own fixes stored too, a row a
                // minute at most, so the track outlives a restart and needs
                // no network.
                let due = model
                    .own_stored_at
                    .is_none_or(|last| t >= last + OWN_TRACK_EVERY_SECS || t < last);
                if due {
                    model.own_stored_at = Some(t);
                    model.fixes_received.push(own_fix);
                }
            }
            model.bar.own_fix = fix;
            model.bar.by_hand = false;
            model.shares_due()
        }
        UiEvent::EnterPosition { text } => model.enter_position(&text),
        UiEvent::TrackWindow(chosen) => {
            model.track_window = chosen;
            Vec::new()
        }
        UiEvent::MapCentre(centre) => {
            model.bar.map_centre = centre;
            Vec::new()
        }
        UiEvent::FixAtMapCentre => model.fix_at_map_centre(),
        UiEvent::CodeRead(code) => model.take_code(&code, false),
        UiEvent::PeerWord { peer, verb } => match model.peer_verb(peer, verb) {
            Some(event) => update(model, event),
            None => Vec::new(),
        },
        UiEvent::MeshPosition {
            identity,
            fix,
            name,
            age,
        } => {
            model.mesh_position(identity, fix, name.as_deref(), age);
            Vec::new()
        }
        UiEvent::AddNearby { peer } => model.add_nearby(peer),
        UiEvent::StartShare { peer, span } => model.start_share(peer, span),
        UiEvent::StopShare { peer } => model.stop_share(peer),
        UiEvent::ShareFix { peer } => model.share_fix(peer),
        UiEvent::Profile { open } => model.show_profile(open),
        UiEvent::ProfileFacts(facts) => {
            model.profile = *facts;
            Vec::new()
        }
        UiEvent::MailboxTold(told) => {
            model.profile.told = *told;
            Vec::new()
        }
        UiEvent::MailboxReach(reach) => {
            model.profile.reach = *reach;
            Vec::new()
        }
        UiEvent::ShareCode => model.share_code(),
        UiEvent::PhotoSize(size) => {
            model.photo_size = size;
            vec![UiEffect::Remember {
                key: "photo_size",
                value: size.word(),
            }]
        }
        UiEvent::ViewPhoto { peer, key } => model.view_photo(peer, key),
        UiEvent::SendFile { peer, name, data } => model.send_file(peer, &name, data),
        UiEvent::OpenFile { peer, key, index } => model.open_file(peer, key, index),
        UiEvent::SendVoice { peer, data } => model.send_voice(peer, data),
        UiEvent::PlayVoice { peer, key } => model.play_voice(peer, key),
        UiEvent::PauseVoice { peer, key } => model.pause_voice(peer, key),
        UiEvent::VoiceEnded { peer, key } => {
            model.voice_ended(peer, key);
            Vec::new()
        }
        UiEvent::Record { peer, input } => model.record(peer, input),
        UiEvent::VoiceTrack { peer, key, track } => {
            model.voice_tracks.insert((peer, key), track);
            Vec::new()
        }
        UiEvent::MailboxChosen { node, by_user } => {
            // Another node's name is not this one's.
            if model.mailbox.node != Some(node) {
                model.mailbox.name = None;
            }
            model.mailbox.node = Some(node);
            model.mailbox.by_user = by_user;
            Vec::new()
        }
        UiEvent::Engine(event) => on_engine(model, event),
        UiEvent::Rename { peer, name } => model.rename(peer, &name),
        UiEvent::Group(verb) => model.group_verb(verb),
        UiEvent::GroupMade(made) => model.group_made(made),
        UiEvent::MembersAdded(added) => model.members_added(added),
        UiEvent::GroupDeleted(deleted) => model.group_deleted(deleted),
        UiEvent::PetnamesHanded { taken } => {
            model.petnames_handed(taken);
            Vec::new()
        }
        UiEvent::PeerState { peer, state } => model.set_peer_state(peer, state),
        UiEvent::Verify { peer, verified } => {
            model.set_verified(peer, verified);
            Vec::new()
        }
        UiEvent::MarkUnread { peer, unread } => {
            match unread {
                true => model.mark_unread(peer),
                false => model.mark_read(peer),
            }
            Vec::new()
        }
        UiEvent::DeleteConversation { peer, everywhere } => {
            model.delete_conversation(peer, everywhere);
            Vec::new()
        }
        UiEvent::Notice(line) => {
            model.toast(ToastKind::Info, line);
            Vec::new()
        }
        UiEvent::SendDone(result) => on_send_done(model, result),
        UiEvent::Earlier {
            peer,
            messages,
            earlier,
        } => {
            if model.page_asked == Some(peer) {
                model.page_asked = None;
            }
            model.on_earlier(peer, messages, earlier);
            Vec::new()
        }
        UiEvent::StoredHits { query, hits } => {
            model.on_stored_hits(&query, hits);
            Vec::new()
        }
        UiEvent::SendUnproven(detail) => on_send_unproven(model, detail),
        UiEvent::SendLeg(leg) => {
            let (tick, now) = (model.tick, model.now);
            if let Some(&(c, m)) = model.sending.front() {
                if let Some(mut message) = model.message_mut(c, m) {
                    message.leg = Some((leg, tick));
                    message.last_leg = Some(leg);
                    message.note(now, &format!("trying {}", leg_words(leg)));
                }
            }
            Vec::new()
        }
        UiEvent::FetchDone(result) => {
            let synced = result.is_ok();
            let mut effects = on_fetch_done(model, result);
            if synced {
                effects.extend(pn_round(model));
            }
            effects
        }
        UiEvent::PnAnswered { node, answer } => {
            let (answers, line) = model.pn_asker.answered(node, answer.as_deref());
            model.pn_answered(&answers);
            vec![UiEffect::PnLine(line)]
        }
        UiEvent::Open { peer } => {
            let index = model.conversation(peer);
            model.select(index);
            // Opening shows the chat wherever the list lives: read here
            // and, through the mark, on the home's other devices.
            model.read_out(index);
            Vec::new()
        }
        UiEvent::Close => {
            if let Some(left) = model.conversations.get_mut(model.selected) {
                left.new_rule = None;
            }
            model.selected = NONE_OPEN;
            model.scroll_top = None;
            model.selection = None;
            model.message_cursor = 0;
            Vec::new()
        }
        UiEvent::Write { peer, body } => {
            let index = model.conversation(peer);
            model.select(index);
            // Writing happens in an open chat: read as Open reads.
            model.read_out(index);
            send_body(model, body)
        }
        UiEvent::React {
            peer,
            target,
            content,
        } => model.react(peer, target, &content),
        UiEvent::Reply { peer, target, body } => model.reply_to(peer, target, body),
        UiEvent::Forward { from, key, to } => model.forward(from, key, to),
        UiEvent::SendAgain { peer, key } => model.send_again(peer, key),
        UiEvent::Delete { peer, keys } => {
            model.delete_messages(peer, &keys);
            Vec::new()
        }
        UiEvent::SharePosition { peer, location } => {
            let index = model.conversation(peer);
            model.send_fields(index, String::new(), position_fields(location))
        }
        UiEvent::Draft { peer, text, reply } => {
            model.set_draft(peer, text, reply);
            Vec::new()
        }
    }
}

/// The fields of a position we share (1201): the Telemeter of `location`
/// alone, timed at its fix, as `docs/src/concepts/telemetry.md` asks of a
/// report (no title, no text, so a receiver shows no notification text).
pub fn position_fields(location: leviculum_lxmf::telemetry::Location) -> Vec<Field> {
    let telemetry = leviculum_lxmf::telemetry::Telemetry {
        time: Some(location.last_update),
        location: Some(location),
        ..Default::default()
    };
    vec![(
        leviculum_lxmf::constants::FIELD_TELEMETRY,
        telemetry.encode_field_value(),
    )]
}

/// The fetch ended. One the reader asked for says how it went either way;
/// one the schedule started says only what the reader must know: new
/// messages, and the first failure in a row, not every retry's.
fn on_fetch_done<B>(model: &mut UiModel<B>, result: Result<usize, String>) -> Vec<UiEffect> {
    model.fetching = false;
    // A pull ended: the next began after every petname the home took so
    // far, and brings nothing older than them (1635).
    model.taken_petnames.clear();
    model.fetches_done += 1;
    let now = model.elapsed_ms;
    let failures = model.sync.as_mut().map_or(1, |sync| {
        sync.on(
            match &result {
                Ok(new) => SyncEvent::Done { new: *new },
                Err(_) => SyncEvent::Failed,
            },
            now,
        );
        sync.failures()
    });
    if result.is_ok() {
        model.mailbox.synced = model.now;
    }
    let asked = model.fetch_reason.is_none_or(|r| r == Reason::Key);
    let mut effects = Vec::new();
    match result {
        Ok(0) if asked => model.toast(ToastKind::Info, "no new messages".to_string()),
        Ok(0) => {}
        Ok(new) => model.toast(ToastKind::Info, format!("{new} new message(s)")),
        Err(detail) if asked || failures == 1 => {
            model.toast(ToastKind::Error, format!("fetch failed: {detail}"));
            effects.push(UiEffect::Notice(format!("fetch: {detail}")));
        }
        Err(_) => {}
    }
    if let Some(reason) = model.pull_owed.take() {
        effects.extend(start_pull(model, reason));
    }
    effects
}

/// The end of a sync round on a device that is its own node (1409): ask
/// each propagation node that holds an open upload, once, or say why not.
/// Only a round that synced: one that failed found the node silent. A
/// home's client asks nothing; its home asks and archives the answer.
fn pn_round<B>(model: &mut UiModel<B>) -> Vec<UiEffect> {
    if model.home_client {
        return Vec::new();
    }
    model.pn_round += 1;
    let now = model.now.map_or(0, |at| at.unix_secs);
    model
        .pn_asker
        .round(model.pn_round, now)
        .into_iter()
        .map(|(node, asked)| match asked {
            Ok(ids) => UiEffect::AskPn { node, ids },
            Err(skip) => UiEffect::PnLine(skip.line()),
        })
        .collect()
}

/// On a client of a home node, a send is followed by a pull, so the bubble
/// takes the home node's word (sent, then whatever followed) without
/// waiting for the schedule. A pull already running may have asked before
/// the home node had the message, so another follows it.
fn pull_after_send<B>(model: &mut UiModel<B>) -> Vec<UiEffect> {
    pull_for(model, Reason::Sent)
}

/// A pull on a client, for `reason`: now, or behind the one running.
fn pull_for<B>(model: &mut UiModel<B>, reason: Reason) -> Vec<UiEffect> {
    if !model.home_client {
        return Vec::new();
    }
    if model.fetching {
        model.pull_owed = Some(model.pull_owed.unwrap_or(reason));
        return Vec::new();
    }
    start_pull(model, reason)
}

fn start_pull<B>(model: &mut UiModel<B>, reason: Reason) -> Vec<UiEffect> {
    let now = model.elapsed_ms;
    if let Some(sync) = model.sync.as_mut() {
        sync.begin(now);
    }
    model.fetching = true;
    model.fetch_reason = Some(reason);
    model.fetch_since_ms = now;
    vec![UiEffect::Fetch]
}

/// Tell the schedule, and start the sync it then finds due.
fn schedule<B>(model: &mut UiModel<B>, event: SyncEvent) -> Vec<UiEffect> {
    let now = model.elapsed_ms;
    match model.sync.as_mut() {
        Some(sync) => {
            sync.on(event, now);
            auto_sync(model)
        }
        None => Vec::new(),
    }
}

/// The sync the schedule finds due now, started; one at a time.
fn auto_sync<B>(model: &mut UiModel<B>) -> Vec<UiEffect> {
    let now = model.elapsed_ms;
    if model.fetching {
        return Vec::new();
    }
    let Some(sync) = model.sync.as_mut() else {
        return Vec::new();
    };
    let Some(reason) = sync.due(now) else {
        return Vec::new();
    };
    sync.begin(now);
    model.fetching = true;
    model.fetch_reason = Some(reason);
    model.fetch_since_ms = now;
    vec![UiEffect::Fetch]
}

fn on_key<B: InputBuffer + Default>(model: &mut UiModel<B>, key: KeyEvent) -> Vec<UiEffect> {
    if model.confirm.is_some() {
        let yes = key.code == KeyCode::Char('y')
            && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT);
        model.answer(yes);
        return Vec::new();
    }
    let ctx = model.ctx();
    if model.help || model.palette.is_some() || model.detail_open() {
        // Only `Ctrl-q`, `Ctrl-c` and backing out reach past an overlay.
        match keys::resolve(&key, keys::OVERLAY, ctx) {
            Some(Action::Run(PaletteCommand::Quit)) => return run(model, PaletteCommand::Quit),
            Some(Action::QuitHint) => return quit_hint(model),
            Some(Action::Back) => {
                model.help = false;
                model.palette = None;
                model.close_detail();
                return Vec::new();
            }
            _ => {}
        }
        return overlay_key(model, key);
    }
    // The copy mode takes every key while it is on (1475).
    if model.copy_mode.is_some() {
        return copy_mode::key(model, key);
    }
    let pane = model.focus;
    // A selection lasts until the next key: the copy row copies it.
    let selection = model.selection.take();
    // While a voice message records (1276) the bar is the compose box:
    // nothing is typed into the field it hides, and the send chord sends
    // the recording.
    let recording = pane == Pane::Compose && ctx.contains(keys::RECORDING);
    match keys::resolve(&key, keys::panes_of(pane), ctx) {
        Some(Action::Run(PaletteCommand::Send)) if recording => {
            record_key(model, keys::RecKey::Send)
        }
        Some(Action::Edit | Action::Newline | Action::ClearDraft) | None if recording => Vec::new(),
        Some(Action::Copy) => copy(model, selection),
        Some(action) => act(model, action, key),
        None => {
            match pane {
                Pane::Compose => {
                    model.compose.edit(&key);
                }
                Pane::Search => {
                    let more = model.store_has_more();
                    if let Some(search) = model.search.as_mut() {
                        // Every keystroke runs the matcher again: the
                        // results are derived, only the cursor resets. What
                        // the screen has not loaded the store searches.
                        if search.query.edit(&key) {
                            search.selected = 0;
                            search.stored.clear();
                            let query = search.query.value();
                            if more && !query.is_empty() {
                                return vec![UiEffect::Store(StoreRead::Search(query.to_string()))];
                            }
                        }
                    }
                }
                _ => {}
            }
            Vec::new()
        }
    }
}

/// A key while the help, the palette or a detail view is up. The help and
/// the detail view scroll by the conversation's scroll keys.
fn overlay_key<B: InputBuffer + Default>(model: &mut UiModel<B>, key: KeyEvent) -> Vec<UiEffect> {
    if model.help {
        if matches!(
            key.code,
            KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Enter
        ) {
            model.help = false;
        } else if let Some(cmd) = keys::overlay_scroll(&key, model.ctx()) {
            scroll_overlay(model, cmd);
        }
        return Vec::new();
    }
    // A palette that asks over the settings (1577: another hub's
    // address) takes the keys before the settings' rows do.
    if model.detail_open() && model.palette.is_none() {
        // The profile's "Share my address" (1443): the code to the
        // clipboard, as the window's button does.
        if model.profile_open && key.code == KeyCode::Char('s') {
            return model.share_code();
        }
        // The settings' keys (1450), under no profile or message.
        if let (Some(at), false, None, KeyCode::Char(c)) =
            (model.settings, model.profile_open, model.detail, key.code)
        {
            if c != 'q' {
                if let Some(effects) = settings_key(model, at, c) {
                    return effects;
                }
            }
        }
        if matches!(key.code, KeyCode::Char('q') | KeyCode::Enter) {
            model.close_detail();
        } else if let Some(cmd) = keys::overlay_scroll(&key, model.ctx()) {
            scroll_overlay(model, cmd);
        }
        return Vec::new();
    }
    let Some(palette) = model.palette.as_mut() else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Enter => {
            if let Some(ask) = palette.ask {
                let typed = palette.query.value().to_string();
                model.palette = None;
                return model.answer_ask(ask, typed);
            }
            if palette.drawings {
                let chosen = palette
                    .drawing_matches(&model.map_setup.drawings)
                    .get(palette.selected)
                    .copied();
                model.palette = None;
                return chosen.map(UiEffect::MapDrawing).into_iter().collect();
            }
            let chosen = palette.matches().get(palette.selected).map(|e| e.command);
            model.palette = None;
            if let Some(command) = chosen {
                return run(model, command);
            }
        }
        KeyCode::Down => {
            let rows = match palette.drawings {
                true => palette.drawing_matches(&model.map_setup.drawings).len(),
                false => palette.matches().len(),
            };
            palette.selected = (palette.selected + 1).min(rows.saturating_sub(1));
        }
        KeyCode::Up => palette.selected = palette.selected.saturating_sub(1),
        // The picker: a digit on an empty line is that quick reaction.
        KeyCode::Char(digit @ '1'..='6')
            if palette.query.value().is_empty()
                && matches!(palette.ask, Some(contacts::Ask::React { .. })) =>
        {
            if let Some(contacts::Ask::React { peer, target }) = palette.ask {
                model.palette = None;
                let quick = reactions::QUICK[usize::from(digit as u8 - b'1')];
                return model.react(peer, target, quick);
            }
        }
        _ => {
            if palette.query.edit(&key) {
                palette.selected = 0;
            }
        }
    }
    Vec::new()
}

/// Scroll the help, or else the detail view, by the shared scroll rule.
fn scroll_overlay<B: InputBuffer>(model: &mut UiModel<B>, cmd: ScrollCmd) {
    if model.help {
        let (viewport, max) = help_bounds(model);
        model.help_scroll = scrolled(model.help_scroll, cmd, viewport, max);
    } else if model.detail_open() {
        let (viewport, max) = detail_bounds(model);
        model.detail_scroll = scrolled(model.detail_scroll, cmd, viewport, max);
    }
}

/// The help's rows on screen at once and its largest first row, on the
/// model's screen: the bounds its renderer slices by.
fn help_bounds<B: InputBuffer>(model: &UiModel<B>) -> (usize, usize) {
    let (width, height) = model.screen;
    leviculum_ui::help::scroll_bounds(&help_groups(), Rect::new(0, 0, width, height))
}

/// The open detail view's rows on screen at once and its largest first row,
/// from [`detail_layout`] on the model's screen; nothing when none is open.
fn detail_bounds<B: InputBuffer>(model: &UiModel<B>) -> (usize, usize) {
    let Some(detail) = shown_detail(model) else {
        return (0, 0);
    };
    let (width, height) = model.screen;
    detail_scroll_bounds(&detail, Rect::new(0, 0, width, height))
}

/// A detail view's scroll bounds over `screen`: the rows inside its border
/// and the largest valid first row.
pub fn detail_scroll_bounds(detail: &DetailView, screen: Rect) -> (usize, usize) {
    let (area, rows) = detail_layout(detail, screen);
    let viewport = usize::from(area.height.saturating_sub(2));
    (viewport, rows.len().saturating_sub(viewport))
}

/// Run what a key resolved to.
fn act<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    action: Action,
    key: KeyEvent,
) -> Vec<UiEffect> {
    match action {
        Action::Run(command) => return run(model, command),
        Action::Scroll(cmd) => {
            scroll(model, model.focus, cmd);
            if model.focus == Pane::Messages {
                return model.earlier_wanted();
            }
        }
        Action::ScrollMessages(cmd) => {
            scroll(model, Pane::Messages, cmd);
            return model.earlier_wanted();
        }
        Action::Newline => model.compose.newline(),
        Action::Edit => {
            model.compose.edit(&key);
        }
        // `Ctrl-c` in compose: a reflex that would throw a half-written
        // message away (loquax-ui.md, section 1, "What must not be copied"),
        // so it clears the line into the field's kill buffer, back with
        // `Ctrl-y`.
        Action::ClearDraft => {
            model
                .compose
                .edit(&KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        }
        Action::Back => {
            model.focus = match model.focus {
                Pane::Compose => Pane::Messages,
                _ => Pane::Conversations,
            };
        }
        Action::NextPane => {
            model.focus = match model.focus.next(model.search.is_some()) {
                // The map is in the conversation's place in the cycle.
                Pane::Messages if model.map.is_some() => Pane::Map,
                next => next,
            };
        }
        Action::PreviousPane => model.focus = model.focus.prev(model.search.is_some()),
        Action::Palette => model.palette = Some(Palette::default()),
        Action::QuitHint => return quit_hint(model),
        Action::Copy => return copy(model, None),
        Action::CopyMode => {
            let effects = copy_mode::enter(model);
            // A Shift+arrow moves the cursor it put down at once.
            return match key.modifiers.contains(KeyModifiers::SHIFT) && model.copy_mode.is_some() {
                true => {
                    let plain = KeyEvent::new(key.code, KeyModifiers::NONE);
                    copy_mode::key(model, plain)
                }
                false => effects,
            };
        }
        Action::OpenResult => return open_result(model),
        Action::Match(forward) => match model.focus {
            Pane::Messages => step_match(model, forward),
            _ => step_result(model, if forward { 1 } else { -1 }),
        },
        Action::CloseSearch => close_search(model),
        Action::Map(cmd) => return map_key(model, cmd),
        Action::OpenMessage => return open_message(model),
        Action::Record(key) => return record_key(model, key),
        Action::PeerOnMap => peer_on_map(model),
    }
    Vec::new()
}

/// `Enter` on a message: one that carries a position, a report or a text
/// with a telemetry field, opens the map at the fix its line names, one
/// with a photo shows it whole (1220), one with a file opens its first
/// (1221), one with a voice message plays it (1222), any other message (and a report
/// without a position) its detail view, which `v` opens for every
/// message. A folded run of reports is drawn as its latest, which is
/// where the cursor stands, so its line opens the latest fix of the run.
fn open_message<B: InputBuffer>(model: &mut UiModel<B>) -> Vec<UiEffect> {
    let photo = model
        .conversations
        .get(model.selected)
        .and_then(|c| Some((c.peer, c.messages.get(model.message_cursor)?)))
        .filter(|(_, m)| photos::photo_of(&m.fields).is_some())
        .map(|(peer, m)| (peer, m.activity));
    if let Some((peer, key)) = photo {
        return model.view_photo(peer, key);
    }
    let file = model
        .conversations
        .get(model.selected)
        .and_then(|c| Some((c.peer, c.messages.get(model.message_cursor)?)))
        .filter(|(_, m)| !files::files_of(&m.fields).is_empty())
        .map(|(peer, m)| (peer, m.activity));
    if let Some((peer, key)) = file {
        return model.open_file(peer, key, 0);
    }
    let voice = model
        .conversations
        .get(model.selected)
        .and_then(|c| Some((c.peer, c.messages.get(model.message_cursor)?)))
        .filter(|(_, m)| voice::voice_of(&m.fields).is_some())
        .map(|(peer, m)| (peer, m.activity));
    if let Some((peer, key)) = voice {
        return model.play_voice(peer, key);
    }
    match model.message_fix(model.selected, model.message_cursor) {
        Some(fix) => {
            open_map_at(model, &fix);
            Vec::new()
        }
        None => details(model),
    }
}

/// `L`, `Alt-l` or a click on the header's position: the open
/// conversation's peer on the map, at its latest fix.
fn peer_on_map<B: InputBuffer>(model: &mut UiModel<B>) {
    let Some(peer) = model.conversations.get(model.selected).map(|c| c.peer) else {
        model.toast(ToastKind::Info, "no conversation open".to_string());
        return;
    };
    match model.positions.get(&peer).cloned() {
        Some(fix) => open_map_at(model, &fix),
        None => {
            let who = model.names.label(&peer, model.lang);
            model.toast(ToastKind::Info, format!("no position from {who}"));
        }
    }
}

/// The map in the conversation's place, centred on `fix` at the deepest
/// zoom of the region under it ([`map::fit_at`]), its peer's pin selected.
/// `Esc` brings the conversation back with the cursor where it was.
fn open_map_at<B: InputBuffer>(model: &mut UiModel<B>, fix: &Fix) {
    let at = leviculum_map::mercator::project(leviculum_map::mercator::LatLon::from_e6(
        fix.location.latitude_e6,
        fix.location.longitude_e6,
    ));
    let (centre, zoom) = map::fit_at(at, model.map_area(), &model.map_setup);
    model.map = Some(map::MapState {
        centre,
        zoom,
        selected: Some(fix.peer),
    });
    model.map_press = None;
    model.remember_map_from();
    model.focus = Pane::Map;
    model.selection = None;
    // Going to a place loads its region (1089): the map is shown at once
    // over what is there, and fitted again when the region lands.
    if model.map_autoload {
        model
            .map_wants
            .extend(map::want_place(at, &model.map_setup.regions));
    }
}

/// A region load moved on: the title's word, a toast for a failure, and a
/// place's map fitted again to the region that landed under it, unless the
/// reader moved it meanwhile.
fn map_load<B: InputBuffer>(model: &mut UiModel<B>, load: MapLoad) {
    match load {
        MapLoad::Started(_) => model.map_loading = Some(MAP_LOADING.to_string()),
        MapLoad::Done(want) => {
            model.map_loading = None;
            let area = model.map_area();
            let setup = model.map_setup.clone();
            if let Some(state) = model.map.as_mut() {
                if want.reason != map::LoadReason::Place || state.centre != want.centre {
                    return;
                }
                let (centre, zoom) = map::fit_at(want.centre, area, &setup);
                state.centre = centre;
                state.zoom = zoom;
            }
        }
        MapLoad::Failed { toast, .. } => {
            model.map_loading = None;
            if let Some(line) = toast {
                model.toast(ToastKind::Error, line);
            }
        }
        MapLoad::Here(here) => model.map_here = here,
    }
}

/// The own fixes the store keeps for the way back (1386): one a minute at
/// most, whatever the receiver's cadence; a day outdoors is 1 440 rows.
pub const OWN_TRACK_EVERY_SECS: i64 = 60;

/// World units a pixel of `view` spans.
fn viewport_unit(view: &leviculum_map::viewport::Viewport) -> f64 {
    libm::exp2(24.0 - f64::from(view.zoom()))
}

/// The palette's `map`: the map in the conversation's place, every pin in
/// view, the first in `Tab` order selected.
fn open_map<B: InputBuffer>(model: &mut UiModel<B>) {
    let pins = model.map_pins();
    let (centre, zoom) = map::fit(&pins, model.map_area(), &model.map_setup);
    model.map = Some(map::MapState {
        centre,
        zoom,
        selected: pins.first().map(|p| p.peer),
    });
    model.remember_map_from();
    model.focus = Pane::Map;
    model.selection = None;
    // Pins where no region is: their places load (1089).
    if model.map_autoload {
        let wants = map::want_pins(&pins, &model.map_setup.regions);
        model.map_wants.extend(wants);
    }
}

/// The palette's `load map area`, `d` on the map: the visible area's
/// region, asked whether autoload is on or not, since the reader asked.
fn load_map_area<B: InputBuffer>(model: &mut UiModel<B>) {
    let area = model.map_area();
    let Some(state) = model.map.as_ref() else {
        model.toast(ToastKind::Info, "open the map first".to_string());
        return;
    };
    let view = map::viewport(state, area, &model.map_setup);
    match map::want_view(&view, &model.map_setup.regions) {
        Some(want) => model.map_wants.push(want),
        None => model.toast(ToastKind::Info, MAP_LOADED_ALREADY.to_string()),
    }
}

/// Move the map's view by eighths of its width and height, east and south
/// positive: the arrows, `h` `j` `k` `l` and the tilted wheel.
fn pan_map(state: &mut map::MapState, area: Rect, setup: &map::MapSetup, dx: i8, dy: i8) {
    let mut view = map::viewport(state, area, setup);
    let (w, h) = view.size();
    let step = |n: i8, of: u32| i32::from(n) * (of / 8).max(1) as i32;
    view.pan(step(dx, w), step(dy, h));
    state.centre = view.centre;
    map::settle(state, area, setup);
}

/// A key on the open map.
fn map_key<B: InputBuffer>(model: &mut UiModel<B>, cmd: keys::MapCmd) -> Vec<UiEffect> {
    use keys::MapCmd;
    let setup = model.map_setup.clone();
    let area = model.map_area();
    let pins = model.map_pins();
    let inside = match (cmd, model.now) {
        (MapCmd::FitWindow, Some(now)) => {
            tracks::inside(&model.track_input(), model.track_window, &now)
        }
        _ => Vec::new(),
    };
    // Where the own fix is (1477): Enter with no pin chosen, or on the own
    // pin, jumps there.
    let own_at = model.bar.own_fix.map(|fix| {
        leviculum_map::mercator::project(leviculum_map::mercator::LatLon::from_e6(
            fix.latitude_e6,
            fix.longitude_e6,
        ))
    });
    let own = model.address;
    let Some(state) = model.map.as_mut() else {
        return Vec::new();
    };
    match cmd {
        MapCmd::ZoomIn => state.zoom = state.zoom.saturating_add(1).min(setup.zooms.1),
        MapCmd::ZoomOut => {
            state.zoom = state.zoom.saturating_sub(1).max(setup.zooms.0);
            map::settle(state, area, &setup);
        }
        MapCmd::Pan(dx, dy) => pan_map(state, area, &setup, dx, dy),
        MapCmd::Fit => {
            let (centre, zoom) = map::fit(&pins, area, &setup);
            state.centre = centre;
            state.zoom = zoom;
        }
        MapCmd::FitWindow => {
            let shown = map::in_window(&pins, &inside);
            let (centre, zoom) = map::fit(&shown, area, &setup);
            state.centre = centre;
            state.zoom = zoom;
        }
        MapCmd::Step(forward) => {
            let at = pins.iter().position(|p| Some(p.peer) == state.selected);
            let n = pins.len();
            let next = match (at, forward) {
                _ if n == 0 => None,
                (None, _) => Some(0),
                (Some(i), true) => Some((i + 1) % n),
                (Some(i), false) => Some((i + n - 1) % n),
            };
            if let Some(pin) = next.and_then(|i| pins.get(i)) {
                state.selected = Some(pin.peer);
                // A pin out of view is brought to the centre.
                if !map::viewport(state, area, &setup).contains(pin.at) {
                    state.centre = pin.at;
                }
            }
        }
        MapCmd::Open => match (state.selected, own_at) {
            // Jump to me (1477): no pin chosen, or the own pin, and a fix.
            (None, Some(at)) => {
                let (centre, zoom) = map::fit_at(at, area, &setup);
                state.centre = centre;
                state.zoom = zoom;
            }
            (Some(peer), Some(at)) if Some(peer) == own => {
                let (centre, zoom) = map::fit_at(at, area, &setup);
                state.centre = centre;
                state.zoom = zoom;
            }
            (Some(peer), _) => return open_pin(model, peer),
            (None, None) => {}
        },
        MapCmd::Close => {
            model.map = None;
            model.map_press = None;
            model.map_glyph_test = false;
            model.focus = model.map_from;
        }
        MapCmd::Drawing(forward) => {
            let n = setup.drawings.len();
            if n > 0 {
                let next = match forward {
                    true => (setup.drawing + 1) % n,
                    false => (setup.drawing + n - 1) % n,
                };
                return vec![UiEffect::MapDrawing(next)];
            }
        }
        MapCmd::GlyphTest => model.map_glyph_test = !model.map_glyph_test,
        MapCmd::TimeZones => model.time_zones = !model.time_zones,
        MapCmd::TrackWindow(forward) => {
            model.track_window = model.track_window.step(forward);
            let word = model.track_window.word();
            model.toast(ToastKind::Info, format!("tracks: {word}"));
        }
    }
    Vec::new()
}

/// A pin chosen by `Enter` or a click: that peer's conversation, opened;
/// a node nearby that is no contact is added so (1302), the card's "Add".
fn open_pin<B: InputBuffer>(model: &mut UiModel<B>, peer: [u8; 16]) -> Vec<UiEffect> {
    if Some(peer) == model.address {
        model.toast(ToastKind::Info, "that pin is you".to_string());
        return Vec::new();
    }
    if model.is_nearby(&peer) {
        model.map = None;
        return model.add_nearby(peer);
    }
    let index = model.conversation(peer);
    model.select(index);
    model.map = None;
    model.focus = Pane::Messages;
    Vec::new()
}

/// `/`, `Ctrl-s` or a click on `/ search`: the search opens with the cursor
/// in its field, or the cursor goes back to the open one's.
fn open_search<B: InputBuffer + Default>(model: &mut UiModel<B>) {
    if model.search.is_none() {
        model.search = Some(search::Search::default());
    }
    model.focus = Pane::Search;
}

/// `Esc` in the field or the results: the list comes back with the focus,
/// and the conversation keeps what it shows.
fn close_search<B>(model: &mut UiModel<B>) {
    model.search = None;
    model.focus = Pane::Conversations;
}

/// Open the selected result: a conversation at its newest message, a
/// message's conversation at that message, its block selected and the match
/// marked in it. The search stays open, so `n` goes on from there. With an
/// empty field the list is the plain one, and its selected conversation
/// opens.
fn open_result<B: InputBuffer>(model: &mut UiModel<B>) -> Vec<UiEffect> {
    let hits = model.hits();
    if model.query().is_none() {
        model.focus = Pane::Messages;
        return Vec::new();
    }
    let selected = model.search.as_ref().map_or(0, |s| s.selected);
    match hits.get(selected) {
        Some(&search::Hit::Conversation(index)) => {
            if index != model.selected {
                model.select(index);
            }
            model.focus = Pane::Messages;
        }
        Some(&search::Hit::Message {
            conversation,
            message,
        }) => {
            if conversation != model.selected {
                model.select(conversation);
            }
            model.focus = Pane::Messages;
            reveal(model, message);
        }
        Some(&search::Hit::Stored(index)) => {
            let found = model.search.as_ref().and_then(|s| s.stored.get(index));
            if let Some(hit) = found.cloned() {
                return model.open_stored(&hit);
            }
        }
        None => model.toast(ToastKind::Info, search::NO_MATCHES.to_string()),
    }
    Vec::new()
}

/// Move the results' cursor by `by` rows of results, held to the list.
fn step_result<B: InputBuffer>(model: &mut UiModel<B>, by: isize) {
    let count = model.hits().len();
    if let Some(search) = model.search.as_mut() {
        let last = count.saturating_sub(1);
        search.selected = search.selected.saturating_add_signed(by).min(last);
    }
}

/// `n` or `N` in the open conversation: the cursor onto the next or the
/// previous message that matches, the view scrolled to it, round to the
/// other end past the last.
fn step_match<B: InputBuffer>(model: &mut UiModel<B>, forward: bool) {
    let Some(query) = model.query() else {
        return;
    };
    let stops = match model.conversations.get(model.selected) {
        Some(conversation) => search::matching_messages(conversation, query),
        None => Vec::new(),
    };
    let at = model.message_cursor;
    let next = match forward {
        true => stops.iter().find(|&&m| m > at).or(stops.first()),
        false => stops.iter().rev().find(|&&m| m < at).or(stops.last()),
    };
    match next.copied() {
        Some(message) => {
            let wrapped = match forward {
                true => message <= at,
                false => message >= at,
            };
            if wrapped && stops.len() > 1 {
                let end = if forward { "first" } else { "last" };
                model.toast(ToastKind::Info, format!("round to the {end} match"));
            }
            reveal(model, message);
        }
        None => model.toast(ToastKind::Info, "no match in this conversation".to_string()),
    }
}

/// The cursor onto message `index` of the open conversation, the view
/// scrolled just far enough to show its block, its start where it is taller
/// than the pane; back at the newest with the bottom in view, the pane
/// follows the newest again.
fn reveal<B: InputBuffer>(model: &mut UiModel<B>, index: usize) {
    let last = model.messages_of_selected().len().saturating_sub(1);
    let index = index.min(last);
    model.message_cursor = index;
    let laid = model.laid();
    let rows = model.message_rows().max(1);
    let total = laid.rows.len();
    let bottom = total.saturating_sub(rows);
    let mut top = model.top_of(total);
    if let Some(span) = laid.spans.get(index) {
        if span.end > top + rows {
            top = span.end - rows;
        }
        if span.start < top {
            top = span.start;
        }
    }
    model.scroll_top = match index == last && top >= bottom {
        true => None,
        false => Some(top),
    };
}

/// `y` or `Alt-w`, and the release of a drag: the selection's text, or the
/// message under the cursor whole, onto the clipboard.
fn copy<B: InputBuffer>(model: &mut UiModel<B>, selection: Option<Selection>) -> Vec<UiEffect> {
    let text = match selection {
        Some(selection) => hit::selected_text(&model.laid(), &selection),
        None => model
            .messages_of_selected()
            .get(model.message_cursor)
            .map(|m| m.body.clone())
            .unwrap_or_default(),
    };
    copy_text(model, text)
}

/// `text` onto the clipboard (OSC 52 in the terminal), the toast saying
/// how much; nothing for an empty one.
fn copy_text<B>(model: &mut UiModel<B>, text: String) -> Vec<UiEffect> {
    if text.is_empty() {
        model.toast(ToastKind::Info, "nothing to copy".to_string());
        return Vec::new();
    }
    model.toast(
        ToastKind::Info,
        format!("copied {} chars", text.chars().count()),
    );
    vec![UiEffect::Copy(text)]
}

/// One mouse event (section 5, "What the mouse does").
fn on_mouse<B: InputBuffer + Default>(model: &mut UiModel<B>, mouse: MouseEvent) -> Vec<UiEffect> {
    if !model.mouse {
        return Vec::new();
    }
    let (col, row) = (mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::Moved => {
            model.footer_hover = hit::footer_slot_at(model, col, row);
            Vec::new()
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // A click ends the copy mode (1475); the mouse selects its own.
            copy_mode::leave(model);
            click(model, col, row)
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            drag(model, col, row);
            Vec::new()
        }
        MouseEventKind::Up(MouseButton::Left) => release(model, col, row),
        MouseEventKind::ScrollDown => wheel(model, col, row, true),
        MouseEventKind::ScrollUp => wheel(model, col, row, false),
        MouseEventKind::ScrollRight => tilt(model, col, row, true),
        MouseEventKind::ScrollLeft => tilt(model, col, row, false),
        _ => Vec::new(),
    }
}

/// The left button went down at a cell.
fn click<B: InputBuffer + Default>(model: &mut UiModel<B>, col: u16, row: u16) -> Vec<UiEffect> {
    use hit::Target;
    model.selection = None;
    model.press = None;
    model.map_press = None;
    let target = hit::target_at(model, col, row);
    match target {
        Target::Nothing | Target::InsideOverlay => {}
        Target::OutsideOverlay => {
            model.help = false;
            model.palette = None;
            model.close_detail();
        }
        Target::Footer(act) => return footer_click(model, act),
        // On the map a press is a click or a drag, which the release tells
        // apart: a pin opens only on a release where it went down.
        Target::Pane(Pane::Map) | Target::Pin(_) => {
            model.focus = Pane::Map;
            if let Some(state) = model.map {
                let pin = match target {
                    Target::Pin(peer) => Some(peer),
                    _ => None,
                };
                model.map_press = Some(map::MapPress::new((col, row), state.centre, pin));
            }
        }
        Target::Pane(pane) => model.focus = pane,
        Target::Conversation(index) => {
            model.focus = Pane::Conversations;
            if index != model.selected {
                model.select(index);
            }
        }
        Target::Badge(index) => open_at_new(model, index),
        Target::SearchHint | Target::SearchField => open_search(model),
        Target::Me => return model.show_profile(true),
        Target::Result(index) => {
            if let Some(search) = model.search.as_mut() {
                search.selected = index;
            }
            open_result(model);
        }
        Target::Block { message, .. } => {
            model.focus = Pane::Messages;
            point_at(model, message);
            model.press = Some(Press {
                target,
                moved: false,
            });
        }
        Target::Mailbox => return run(model, PaletteCommand::Fetch),
        Target::Position => peer_on_map(model),
        // The chip's one click (1273): the share stops with its end report.
        Target::ShareChip => {
            if let Some(peer) = model.conversations.get(model.selected).map(|c| c.peer) {
                return model.stop_share(peer);
            }
        }
        Target::Compose { at } => {
            model.focus = Pane::Compose;
            model.compose.set_cursor(at);
        }
        Target::Send => {
            model.focus = Pane::Compose;
            if model.recording_bar().is_some() {
                return record_key(model, keys::RecKey::Send);
            }
            return run(model, PaletteCommand::Send);
        }
        // A button of the recording bar, or its silence line (1276).
        Target::Rec(verb) => {
            model.focus = Pane::Compose;
            if let Some(peer) = model.conversations.get(model.selected).map(|c| c.peer) {
                return model.record(peer, recording::Input::Verb(verb));
            }
        }
    }
    Vec::new()
}

/// A key of the recording (1276) in the open conversation. A terminal has
/// no hold, so a recording starts locked; `p` pauses or goes on, `Space`
/// plays or stops, by where the recording is. The time is the shell's
/// clock, which the levels come on too.
fn record_key<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    key: keys::RecKey,
) -> Vec<UiEffect> {
    use recording::{Input, Phase, Verb};
    let Some(peer) = model.conversations.get(model.selected).map(|c| c.peer) else {
        return Vec::new();
    };
    let verb = match key {
        keys::RecKey::Start => {
            model.focus = Pane::Compose;
            let now = model.elapsed_ms;
            let mut effects = model.record(peer, Input::Press { now });
            if model.recording.peer == peer && matches!(model.recording.phase, Phase::Held { .. }) {
                effects.extend(model.record(peer, Input::Verb(Verb::Lock)));
            }
            return effects;
        }
        keys::RecKey::Send => Verb::Send,
        keys::RecKey::Discard => Verb::Discard,
        keys::RecKey::Pause => match model.recording.phase {
            Phase::Paused => Verb::Resume,
            _ => Verb::Pause,
        },
        keys::RecKey::Play => match model.recording.previewing {
            true => Verb::StopPlay,
            false => Verb::Play,
        },
    };
    model.record(peer, Input::Verb(verb))
}

/// What a footer button runs when clicked: its row's action, the same the
/// key runs.
fn footer_click<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    button: FooterAct,
) -> Vec<UiEffect> {
    match button {
        FooterAct::Row(Some(action)) => act(
            model,
            action,
            KeyEvent::new(KeyCode::Null, KeyModifiers::NONE),
        ),
        FooterAct::Row(None) => Vec::new(),
        FooterAct::PaletteRun => {
            overlay_key(model, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        }
        FooterAct::PaletteClose => {
            model.palette = None;
            Vec::new()
        }
    }
}

/// The badge: the conversation opened at its `new` rule, the first unread
/// message under the cursor and the rule at the top of the view.
fn open_at_new<B: InputBuffer>(model: &mut UiModel<B>, index: usize) {
    let rule = model.conversations.get(index).and_then(|c| c.new_rule);
    model.select(index);
    model.focus = Pane::Messages;
    let Some(rule) = rule else {
        return;
    };
    let laid = model.laid();
    let bottom = laid.rows.len().saturating_sub(model.message_rows());
    // The span's first row that is drawn: the rule, not the blank above it.
    let top = laid.spans.get(rule.at).map_or(bottom, |span| {
        (span.start..span.end)
            .find(|&row| !laid.rows[row].cells.is_empty())
            .unwrap_or(span.start)
    });
    model.message_cursor = rule.at;
    model.scroll_top = (top < bottom).then_some(top);
}

/// The cursor onto message `index`, the view where it is: a click selects
/// what the reader sees, it does not scroll to it.
fn point_at<B: InputBuffer>(model: &mut UiModel<B>, index: usize) {
    let total = model.laid().rows.len();
    let bottom = total.saturating_sub(model.message_rows());
    let top = model.top_of(total);
    let last = model.messages_of_selected().len().saturating_sub(1);
    model.message_cursor = index;
    model.scroll_top = match index == last && top >= bottom {
        true => None,
        false => Some(top),
    };
}

/// The pointer moved with the left button held: a press on the map moves
/// it with the pointer, a press on the conversation selects from where it
/// went down to here.
fn drag<B: InputBuffer>(model: &mut UiModel<B>, col: u16, row: u16) {
    if model.map_press.is_some() {
        map_drag(model, col, row);
        return;
    }
    let Some(press) = model.press.as_mut() else {
        return;
    };
    let hit::Target::Block { cell: anchor, .. } = press.target else {
        return;
    };
    let Some(head) = hit::pane_cell(model, col, row) else {
        return;
    };
    if head != anchor {
        if let Some(press) = model.press.as_mut() {
            press.moved = true;
        }
    }
    if model.press.as_ref().is_some_and(|p| p.moved) {
        model.selection = Some(Selection { anchor, head });
    }
}

/// The map follows the pointer: the view at the press, moved by how far
/// the pointer is from where the button went down. Nothing moves until the
/// press is a drag ([`map::DRAG_CELLS`]).
fn map_drag<B: InputBuffer>(model: &mut UiModel<B>, col: u16, row: u16) {
    let setup = model.map_setup.clone();
    let area = model.map_area();
    let (Some(press), Some(state)) = (model.map_press.as_mut(), model.map.as_mut()) else {
        return;
    };
    press.reach(col, row);
    state.centre = press.centre_at(col, row, state, &setup);
    map::settle(state, area, &setup);
}

/// The left button came up: on the map a drag ends where the pointer is
/// and a click on a pin opens it; in the conversation a drag copies what
/// it selected, a click on a mark opens the detail view, one on a link
/// follows it and one on a position report opens the map at its fix.
fn release<B: InputBuffer + Default>(model: &mut UiModel<B>, col: u16, row: u16) -> Vec<UiEffect> {
    if model.map_press.is_some() {
        // A terminal may report the release in a cell no motion reached.
        map_drag(model, col, row);
        let press = model.map_press.take();
        return match press {
            Some(press) if !press.dragging && model.map.is_some() => match press.pin {
                Some(peer) => open_pin(model, peer),
                None => Vec::new(),
            },
            _ => Vec::new(),
        };
    }
    let Some(press) = model.press.take() else {
        return Vec::new();
    };
    let hit::Target::Block {
        message,
        mark,
        link,
        ..
    } = press.target
    else {
        return Vec::new();
    };
    if press.moved {
        // The selection stays drawn, and `y` copies it again.
        let selection = model.selection;
        return copy(model, selection);
    }
    if mark {
        model.detail = Some((model.selected, message));
        model.detail_scroll = 0;
        return Vec::new();
    }
    if let Some(target) = link.and_then(|link| model.laid().links.get(link).cloned()) {
        return follow(model, &target);
    }
    // A click on a message with a position opens the map at its fix, as
    // `Enter`.
    if let Some(fix) = model.message_fix(model.selected, message) {
        open_map_at(model, &fix);
    }
    Vec::new()
}

/// A link in a body, followed as lnomad follows one ([`classify_link`]): a
/// web or mail URL goes to the system's handler, an `lxmf@` address opens
/// its conversation for writing, a NomadNet page opens in the Browse tab
/// where the host has one (1337) and is copied for lnomad where not.
fn follow<B: InputBuffer + Default>(model: &mut UiModel<B>, target: &str) -> Vec<UiEffect> {
    match classify_link(target) {
        LinkKind::External(url) => {
            model.toast(ToastKind::Info, format!("opening {url}"));
            vec![UiEffect::Open(url)]
        }
        LinkKind::Lxmf(hash) => match crate::address::parse(&hash) {
            Ok(peer) => {
                let index = model.conversation(peer);
                model.select(index);
                model.focus = Pane::Compose;
                model.toast(
                    ToastKind::Info,
                    format!("writing to {}", model.names.label(&peer, model.lang)),
                );
                Vec::new()
            }
            Err(_) => Vec::new(),
        },
        LinkKind::Rns if model.browse.enabled => browse::open_page(model, target.to_string()),
        LinkKind::Rns => {
            model.toast(
                ToastKind::Info,
                "page link copied: open it in lnomad".to_string(),
            );
            vec![UiEffect::Copy(target.to_string())]
        }
        LinkKind::Unsafe(scheme) => {
            model.toast(ToastKind::Error, format!("won't open {scheme}: link"));
            Vec::new()
        }
    }
}

/// The wheel scrolls the pane under the pointer, whichever has the focus:
/// the list a conversation a notch, the conversation [`WHEEL_ROWS`] rows
/// without moving its cursor, the compose box a line of the body.
fn wheel<B: InputBuffer>(model: &mut UiModel<B>, col: u16, row: u16, down: bool) -> Vec<UiEffect> {
    // Over the help or a detail view the wheel scrolls it, wherever the
    // pointer is: the help leaves no outside, and a detail view is the only
    // thing to scroll while it is up.
    if model.palette.is_none() && (model.help || model.detail_open()) {
        let cmd = match down {
            true => ScrollCmd::LineDown,
            false => ScrollCmd::LineUp,
        };
        for _ in 0..WHEEL_ROWS {
            scroll_overlay(model, cmd);
        }
        return Vec::new();
    }
    if model.help || model.palette.is_some() || model.detail_open() {
        return Vec::new();
    }
    match hit::pane_at(model, col, row) {
        Some(Pane::Conversations | Pane::Search) => scroll_list(
            model,
            match down {
                true => ScrollCmd::LineDown,
                false => ScrollCmd::LineUp,
            },
        ),
        Some(Pane::Messages) => {
            let total = model.laid().rows.len();
            let bottom = total.saturating_sub(model.message_rows());
            let top = model.top_of(total);
            let top = match down {
                true => (top + WHEEL_ROWS).min(bottom),
                false => top.saturating_sub(WHEEL_ROWS),
            };
            model.scroll_top = (top < bottom).then_some(top);
            return model.earlier_wanted();
        }
        Some(Pane::Compose) => {
            let code = match down {
                true => KeyCode::Down,
                false => KeyCode::Up,
            };
            model.compose.edit(&KeyEvent::new(code, KeyModifiers::NONE));
        }
        // The wheel zooms the map about the pointer, toward the reader as
        // it rolls back.
        Some(Pane::Map) => {
            let (setup, area) = (model.map_setup.clone(), model.map_area());
            if let Some(state) = model.map.as_mut() {
                let zoom = match down {
                    true => state.zoom.saturating_sub(1).max(setup.zooms.0),
                    false => state.zoom.saturating_add(1).min(setup.zooms.1),
                };
                map::zoom_at(state, area, &setup, col, row, zoom);
            }
        }
        None => {}
    }
    Vec::new()
}

/// The wheel tilted, or a touch screen's sideways swipe sent as one: over
/// the map it pans an eighth of the view, as `h` and `l` do. Nothing else
/// scrolls sideways, and an overlay takes it for nothing.
fn tilt<B: InputBuffer>(model: &mut UiModel<B>, col: u16, row: u16, right: bool) -> Vec<UiEffect> {
    if model.help || model.palette.is_some() || model.detail_open() {
        return Vec::new();
    }
    if hit::pane_at(model, col, row) == Some(Pane::Map) {
        let (setup, area) = (model.map_setup.clone(), model.map_area());
        if let Some(state) = model.map.as_mut() {
            pan_map(state, area, &setup, if right { 1 } else { -1 }, 0);
        }
    }
    Vec::new()
}

/// `Ctrl-c` quits nowhere: outside compose it says what does.
fn quit_hint<B>(model: &mut UiModel<B>) -> Vec<UiEffect> {
    let quit = keys::row_named("quit")
        .and_then(|row| {
            row.chords()
                .find(|c| c.in_force(keys::OVERLAY, Ctx::NONE))
                .map(|c| c.label())
        })
        .unwrap_or("q");
    model.toast(ToastKind::Info, format!("{quit} quits"));
    Vec::new()
}

/// Move the cursor of the conversation list or the messages.
fn scroll<B: InputBuffer>(model: &mut UiModel<B>, pane: Pane, cmd: ScrollCmd) {
    match pane {
        Pane::Conversations | Pane::Search => scroll_list(model, cmd),
        Pane::Messages | Pane::Compose => scroll_messages(model, cmd),
        Pane::Map => {}
    }
}

/// The results of an open search move by result, a page being the results
/// a list pane shows.
fn scroll_results<B: InputBuffer>(model: &mut UiModel<B>, cmd: ScrollCmd) {
    let count = model.hits().len() as isize;
    let page = layout::entries_shown(model.sizes().list.1.saturating_sub(search::FIELD_ROWS)).max(1)
        as isize;
    let by = match cmd {
        ScrollCmd::LineDown => 1,
        ScrollCmd::LineUp => -1,
        ScrollCmd::PageDown => page,
        ScrollCmd::PageUp => -page,
        ScrollCmd::HalfPageDown => (page / 2).max(1),
        ScrollCmd::HalfPageUp => -(page / 2).max(1),
        ScrollCmd::Top => -count,
        ScrollCmd::Bottom => count,
    };
    step_result(model, by);
}

/// The list moves in its own order, a page being the conversations it
/// shows.
fn scroll_list<B: InputBuffer>(model: &mut UiModel<B>, cmd: ScrollCmd) {
    if model.query().is_some() {
        return scroll_results(model, cmd);
    }
    let order = model.order();
    let Some(at) = order.iter().position(|&i| i == model.selected) else {
        return;
    };
    let last = order.len() - 1;
    let page = layout::entries_shown(model.sizes().list.1).max(1);
    let to = match cmd {
        ScrollCmd::LineDown => at + 1,
        ScrollCmd::LineUp => at.saturating_sub(1),
        ScrollCmd::PageDown => at + page,
        ScrollCmd::PageUp => at.saturating_sub(page),
        ScrollCmd::HalfPageDown => at + (page / 2).max(1),
        ScrollCmd::HalfPageUp => at.saturating_sub((page / 2).max(1)),
        ScrollCmd::Top => 0,
        ScrollCmd::Bottom => last,
    }
    .min(last);
    if order[to] != model.selected {
        model.select(order[to]);
    }
}

/// The open chat read by the line (1529, [`pager`]): the terminal hands
/// the rule its laid rows, each message's lines without the separators
/// above it. Back on the newest message with the bottom in view, the pane
/// follows the newest again.
fn scroll_messages<B: InputBuffer>(model: &mut UiModel<B>, cmd: ScrollCmd) {
    let count = model.messages_of_selected().len();
    if count == 0 {
        return;
    }
    let laid = model.laid();
    let rows = model.message_rows().max(1);
    let total = laid.rows.len();
    let lines = lines_of(&laid);
    let view = pager::View {
        top: model.top_of(total),
        selected: model.message_cursor.min(count - 1),
    };
    let to = pager::step(&lines, total, rows, view, cmd);
    model.message_cursor = to.selected;
    let bottom = total.saturating_sub(rows);
    // The newest drawn message: reactions after it have no rows.
    let newest = lines[to.selected.min(lines.len())..]
        .iter()
        .skip(1)
        .all(|l| !l.drawn());
    model.scroll_top = match newest && to.top >= bottom {
        true => None,
        false => Some(to.top),
    };
}

/// A scroll key over a target's own window of the open chat (1529, the
/// M9): the rule is [`pager::step`], as the terminal's; the cursor goes to
/// the message it selects and the view is kept by the message its first
/// row is in. Back on the newest with the bottom in view, the chat follows
/// the newest again.
fn page_window<B: InputBuffer>(model: &mut UiModel<B>, cmd: ScrollCmd, window: &pager::Window) {
    let c = model.selected;
    let Some(conversation) = model.conversations.get(c) else {
        return;
    };
    let cursor = conversation.messages.get(model.message_cursor);
    let selected = cursor
        .and_then(|m| window.keys.iter().position(|&k| k == m.activity))
        .unwrap_or(window.keys.len());
    let view = pager::View {
        top: window.top,
        selected,
    };
    let to = pager::step(&window.lines, window.total, window.rows, view, cmd);
    let Some(&key) = window.keys.get(to.selected) else {
        return;
    };
    let Some(index) = conversation.messages.iter().position(|m| m.activity == key) else {
        return;
    };
    let newest = !(index + 1..conversation.messages.len()).any(|i| model.bubble_row(c, i));
    model.message_cursor = index;
    let bottom = window.total.saturating_sub(window.rows);
    model.chat_anchor = match newest && to.top >= bottom {
        true => None,
        false => window.anchor(to.top),
    };
}

/// The lines of each message among the laid rows (1529): its block and
/// what hangs under it, the separators above it left out.
fn lines_of(laid: &layout::Laid) -> Vec<pager::Lines> {
    laid.spans
        .iter()
        .map(|span| match span.start < span.end {
            true => pager::Lines {
                start: span.block.0.clamp(span.start, span.end),
                end: span.end,
            },
            false => pager::Lines::default(),
        })
        .collect()
}

/// Select the conversation `step` away in the list's order, or the next one
/// with unread messages that way.
fn step_conversation<B>(model: &mut UiModel<B>, forward: bool, unread_only: bool) {
    let order = model.order();
    let count = order.len();
    let Some(at) = order.iter().position(|&i| i == model.selected) else {
        return;
    };
    if !unread_only {
        let to = match forward {
            true => (at + 1).min(count - 1),
            false => at.saturating_sub(1),
        };
        if order[to] != model.selected {
            model.select(order[to]);
        }
        return;
    }
    let found = (1..count)
        .map(|step| match forward {
            true => order[(at + step) % count],
            false => order[(at + count - step) % count],
        })
        .find(|&i| model.conversations[i].unread > 0);
    match found {
        Some(index) => model.select(index),
        None => model.toast(ToastKind::Info, "no unread conversation".to_string()),
    }
}

/// The words a leg is said in.
fn leg_words(leg: Leg) -> &'static str {
    match leg {
        Leg::Direct => "direct",
        Leg::Propagated => "via propagation node",
    }
}

/// The ledger line of a proof (1462): the recipient's identity, its
/// mailbox or its device, has it.
pub const PROOF_LINE: &str = "arrived: proof received";

/// The proof's ledger line before 1462, when a proof was shown as two
/// ticks: a ledger kept from then still says it.
pub const PROOF_LINE_BEFORE_1462: &str = "delivered: proof received";

/// What a message's ledger says when it reaches a state.
fn ledger_words(state: DeliveryState) -> &'static str {
    match state {
        DeliveryState::Arrived => PROOF_LINE,
        DeliveryState::Delivered => "on the recipient's device",
        DeliveryState::Rejected => "rejected by the peer",
        DeliveryState::Failed => "failed",
        other => state_words(other),
    }
}

/// The words a delivery state is said in.
fn state_words(state: DeliveryState) -> &'static str {
    match state {
        DeliveryState::Queued => "queued",
        DeliveryState::Stamping => "stamping",
        DeliveryState::InFlight => "in flight",
        DeliveryState::Sent => "sent, no proof yet",
        DeliveryState::InMailbox => "waiting at a propagation node",
        DeliveryState::Arrived => "arrived",
        DeliveryState::Delivered => "on the device",
        DeliveryState::Read { .. } => "read",
        DeliveryState::Rejected => "rejected",
        DeliveryState::Failed => "not sent",
    }
}

/// The state a sent message is shown in (1232): on a client of a home
/// node the home node is the reader's mailbox, so its `Sent` (the home
/// took the message, `home::shown_state`) is the mailbox's tick, not the
/// clock of a send nobody confirmed. With read receipts off (1233,
/// `receipts` false) a read message is shown delivered: who sends none
/// sees none, the old ones neither, and the state is kept for the switch
/// turned on again. Elsewhere the state itself.
pub fn shown_delivery(state: DeliveryState, home_client: bool, receipts: bool) -> DeliveryState {
    match state {
        DeliveryState::Sent if home_client => DeliveryState::InMailbox,
        DeliveryState::Read { .. } if !receipts => DeliveryState::Delivered,
        other => other,
    }
}

/// A sent message's state in the detail view's words (1232,
/// loquax-read-receipts.md, section 3): the ladder's clock covers both
/// waiting and sent without proof, one tick is the mailbox, so the words
/// say which it is; `who` is the peer as the screen names them.
fn detail_words(state: DeliveryState, who: &str, offset_secs: i32) -> String {
    match state {
        DeliveryState::Queued => "waiting to be sent".into(),
        DeliveryState::Stamping => "being sent: computing its stamp".into(),
        DeliveryState::InFlight => "being sent now".into(),
        DeliveryState::Sent => "sent; nobody has confirmed it arrived yet".into(),
        DeliveryState::InMailbox => format!("in the mailbox, delivered once {who} is online"),
        DeliveryState::Arrived => format!("arrived: {who}'s mailbox or device has it"),
        DeliveryState::Delivered => format!("on {who}'s device: it said so"),
        DeliveryState::Read { at } => {
            let at = WallTime {
                unix_secs: i64::from(at),
                offset_secs,
            };
            format!("read {}, delivered before", at.hh_mm())
        }
        DeliveryState::Rejected => "not sent: refused; its mark sends it again".into(),
        DeliveryState::Failed => "not sent: given up; its mark sends it again".into(),
    }
}

/// ` 10:31`, when this device learned something, for the detail view's
/// words; nothing without a time.
fn at_words(at: Option<WallTime>) -> String {
    at.map(|at| format!(" {}", at.hh_mm())).unwrap_or_default()
}

/// `Enter` on a message: its detail view, with what this device saw
/// happen to it (loquax-keys-and-layout.md, section 7, slice 2).
fn details<B>(model: &mut UiModel<B>) -> Vec<UiEffect> {
    if model
        .messages_of_selected()
        .get(model.message_cursor)
        .is_none()
    {
        model.toast(ToastKind::Info, "no message here".to_string());
        return Vec::new();
    }
    model.detail = Some((model.selected, model.message_cursor));
    model.detail_scroll = 0;
    Vec::new()
}

/// Run one palette command.
fn run<B: InputBuffer + Default>(model: &mut UiModel<B>, command: PaletteCommand) -> Vec<UiEffect> {
    match command {
        PaletteCommand::Send => send(model),
        PaletteCommand::Fetch => {
            if model.fetching {
                model.toast(ToastKind::Info, "a fetch is already running".to_string());
                return Vec::new();
            }
            let now = model.elapsed_ms;
            if let Some(sync) = model.sync.as_mut() {
                sync.on(SyncEvent::Key, now);
                sync.begin(now);
            }
            model.fetching = true;
            model.fetch_reason = Some(Reason::Key);
            model.fetch_since_ms = now;
            model.toast(ToastKind::Info, "checking the propagation node".to_string());
            vec![UiEffect::Fetch]
        }
        PaletteCommand::Quit => {
            model.quit = true;
            vec![UiEffect::Quit]
        }
        PaletteCommand::Help => {
            model.help = true;
            model.help_scroll = 0;
            Vec::new()
        }
        PaletteCommand::FocusConversations => focus(model, Pane::Conversations),
        PaletteCommand::FocusMessages => focus(model, Pane::Messages),
        PaletteCommand::FocusCompose => focus(model, Pane::Compose),
        PaletteCommand::NextConversation => {
            step_conversation(model, true, false);
            Vec::new()
        }
        PaletteCommand::PreviousConversation => {
            step_conversation(model, false, false);
            Vec::new()
        }
        PaletteCommand::NextUnread => {
            step_conversation(model, true, true);
            Vec::new()
        }
        PaletteCommand::PreviousUnread => {
            step_conversation(model, false, true);
            Vec::new()
        }
        PaletteCommand::Details => details(model),
        PaletteCommand::Redraw => vec![UiEffect::Redraw],
        PaletteCommand::SendOnEnter => {
            model.send_on_enter = !model.send_on_enter;
            let hint = keys::compose_hint(model.ctx());
            model.toast(ToastKind::Info, hint);
            Vec::new()
        }
        PaletteCommand::ReadReceipts => {
            let on = !model.reading.on;
            let effects = model.set_receipts(on);
            let text = match on {
                true => "read receipts on: others see what you read, you see what they read",
                false => "read receipts off: you send none and see none",
            };
            model.toast(ToastKind::Info, text.to_string());
            effects
        }
        PaletteCommand::Mouse => {
            model.mouse = !model.mouse;
            model.footer_hover = None;
            model.press = None;
            model.selection = None;
            let text = match model.mouse {
                true => "mouse on: click, wheel and drag",
                false => "mouse off: the terminal selects text",
            };
            model.toast(ToastKind::Info, text.to_string());
            vec![UiEffect::Mouse(model.mouse)]
        }
        PaletteCommand::Search => {
            open_search(model);
            Vec::new()
        }
        PaletteCommand::DeleteMessage => {
            model.ask_delete_message();
            Vec::new()
        }
        PaletteCommand::DeleteConversation => {
            model.ask_delete_conversation();
            Vec::new()
        }
        PaletteCommand::Pin => model.toggle_pin(),
        // The verbs the terminal lacked against the phone (1371).
        PaletteCommand::Reply => model.start_reply(),
        PaletteCommand::SendAgain => model.send_again_here(),
        PaletteCommand::Forward => {
            model.ask_forward();
            Vec::new()
        }
        PaletteCommand::Mute => model.toggle_mute(),
        PaletteCommand::Verify => {
            model.toggle_verified();
            Vec::new()
        }
        PaletteCommand::MarkUnread => {
            model.toggle_unread();
            Vec::new()
        }
        PaletteCommand::PhotoSize => model.next_photo_size(),
        PaletteCommand::Position => {
            model.ask_position();
            Vec::new()
        }
        PaletteCommand::SharePosition => {
            model.ask_share();
            Vec::new()
        }
        PaletteCommand::StopSharing => model.stop_share_here(),
        PaletteCommand::DeleteAllMaps => {
            model.ask_delete_all_maps();
            Vec::new()
        }
        PaletteCommand::SendPicture => {
            model.ask_path(true, false);
            Vec::new()
        }
        PaletteCommand::SendFile => {
            model.ask_path(false, false);
            Vec::new()
        }
        PaletteCommand::CodeFromPicture => {
            model.ask_path(false, true);
            Vec::new()
        }
        PaletteCommand::Archive => model.toggle_archive(),
        PaletteCommand::Archived => {
            model.toggle_archived_view();
            Vec::new()
        }
        PaletteCommand::LoadMapArea => {
            load_map_area(model);
            Vec::new()
        }
        PaletteCommand::Map => {
            open_map(model);
            Vec::new()
        }
        PaletteCommand::Browse => browse::open(model),
        PaletteCommand::Rooms => rooms::open(model),
        PaletteCommand::Explore => explore::tab(model, scene::TabId::Explore),
        PaletteCommand::MapDrawing => {
            if model.map_setup.drawings.is_empty() {
                model.toast(ToastKind::Info, "the map is drawn one way here".to_string());
            } else {
                model.palette = Some(Palette {
                    selected: model.map_setup.drawing,
                    drawings: true,
                    ..Palette::default()
                });
            }
            Vec::new()
        }
        PaletteCommand::Rename => {
            model.ask_rename();
            Vec::new()
        }
        PaletteCommand::React => {
            model.ask_react();
            Vec::new()
        }
        PaletteCommand::Block => model.set_blocked(true),
        PaletteCommand::Unblock => model.set_blocked(false),
        PaletteCommand::Blocked => {
            model.toggle_blocked_view();
            Vec::new()
        }
        PaletteCommand::Import => {
            model.toast(
                ToastKind::Info,
                "importing from the other messengers' default places".to_string(),
            );
            vec![UiEffect::Import]
        }
        PaletteCommand::NewConversation => {
            model.ask_code(false);
            Vec::new()
        }
        PaletteCommand::AddContact => {
            model.ask_code(true);
            Vec::new()
        }
        // The terminal's group flows (1307): the palette asks as `rename`
        // does; the verbs are the scene's.
        PaletteCommand::NewGroup => {
            model.group_verb(groups::Verb::New);
            model.palette = Some(Palette {
                ask: Some(contacts::Ask::GroupPick),
                ..Palette::default()
            });
            Vec::new()
        }
        PaletteCommand::JoinGroup => model.group_verb(groups::Verb::Join {
            group: intent::Target::Open,
        }),
        PaletteCommand::Members => match model
            .conversations
            .get(model.selected)
            .map(|c| c.peer)
            .filter(|p| model.groups.contains_key(p))
        {
            Some(group) => {
                model.palette = Some(Palette {
                    ask: Some(contacts::Ask::Members(group)),
                    ..Palette::default()
                });
                Vec::new()
            }
            None => {
                model.toast(ToastKind::Info, "this conversation is no group".to_string());
                Vec::new()
            }
        },
        PaletteCommand::LeaveGroup => match model
            .conversations
            .get(model.selected)
            .map(|c| c.peer)
            .filter(|p| model.groups.contains_key(p))
        {
            Some(group) => {
                model.palette = Some(Palette {
                    ask: Some(contacts::Ask::LeaveGroup(group)),
                    ..Palette::default()
                });
                Vec::new()
            }
            None => {
                model.toast(ToastKind::Info, "this conversation is no group".to_string());
                Vec::new()
            }
        },
        PaletteCommand::AddMembers => {
            let effects = model.group_verb(groups::Verb::Add {
                group: intent::Target::Open,
            });
            if model.group_draft.as_ref().is_some_and(|d| d.to.is_some()) {
                model.palette = Some(Palette {
                    ask: Some(contacts::Ask::GroupPick),
                    ..Palette::default()
                });
            }
            effects
        }
        // 1307 pass four: the admin's delete asks in the palette, as leave
        // does; start again opens the name, Enter keeps the old one.
        PaletteCommand::DeleteGroup => match model
            .conversations
            .get(model.selected)
            .map(|c| c.peer)
            .filter(|p| model.groups.get(p).is_some_and(|g| g.ours))
        {
            Some(group) => {
                model.palette = Some(Palette {
                    ask: Some(contacts::Ask::DeleteGroup(group)),
                    ..Palette::default()
                });
                Vec::new()
            }
            None => {
                model.toast(
                    ToastKind::Info,
                    "only the group's admin deletes it".to_string(),
                );
                Vec::new()
            }
        },
        PaletteCommand::StartAgain => {
            let effects = model.group_verb(groups::Verb::Again {
                group: intent::Target::Open,
            });
            if model.group_draft.is_some() {
                model.palette = Some(Palette {
                    ask: Some(contacts::Ask::GroupName),
                    ..Palette::default()
                });
            }
            effects
        }
        PaletteCommand::RemoveMember => model.group_verb(groups::Verb::Remove {
            group: intent::Target::Open,
            member: None,
            confirmed: false,
        }),
        PaletteCommand::MyCode => model.show_profile(true),
        PaletteCommand::Settings => model.show_settings(None),
        PaletteCommand::Theme => {
            model.theme = model.theme.toggle();
            let name = match model.theme {
                Theme::Dark => "dark",
                Theme::Light => "light",
            };
            model.toast(ToastKind::Info, format!("theme {name}"));
            Vec::new()
        }
    }
}

fn focus<B>(model: &mut UiModel<B>, pane: Pane) -> Vec<UiEffect> {
    model.focus = pane;
    Vec::new()
}

fn send<B: InputBuffer + Default>(model: &mut UiModel<B>) -> Vec<UiEffect> {
    if model.compose.value().trim().is_empty() {
        return Vec::new();
    }
    if model.conversations.get(model.selected).is_none() {
        model.toast(
            ToastKind::Error,
            "no conversation to send to: :new conversation starts one".to_string(),
        );
        return Vec::new();
    }
    let body = model.compose.take();
    // A reply the palette's `reply` started (1371) answers its message.
    if let Some((peer, target)) = model.open_reply() {
        return model.reply_to(peer, target, body);
    }
    send_body(model, body)
}

/// Send `body` in the open conversation: the compose pane's text, or a
/// frontend's ([`UiEvent::Write`]). Blank text sends nothing.
fn send_body<B>(model: &mut UiModel<B>, body: String) -> Vec<UiEffect> {
    // Whitespace at the edges never travels (walk 1 #26): a trailing
    // newline would draw a bubble with an empty line.
    let body = outgoing(body);
    // A staged photo goes with the text as its caption (1270).
    if let Some(effects) = model.send_staged(body.clone()) {
        return effects;
    }
    if body.trim().is_empty() {
        return Vec::new();
    }
    // A group its admin deleted takes nothing more (1307 pass four): its
    // compose is closed, and a frontend that still offers one is told.
    if let Some(ended) = model
        .conversations
        .get(model.selected)
        .and_then(|c| model.groups.get(&c.peer))
        .filter(|g| g.standing == groups::Standing::Ended)
    {
        let line = format!("{} ended; nothing can be written to it", ended.name);
        model.toast(ToastKind::Info, line);
        return Vec::new();
    }
    let Some(conversation) = model.conversations.get_mut(model.selected) else {
        return Vec::new();
    };
    conversation
        .messages
        .push(Message::ours(body.clone(), model.now));
    let to = conversation.peer;
    let index = conversation.messages.len() - 1;
    model.clear_draft(to);
    let activity = model.bump();
    model.conversations[model.selected].activity = activity;
    if let Some(mut message) = model.conversations[model.selected].messages.get_mut(index) {
        message.activity = activity;
    }
    model.touch(model.selected, index);
    // Signal's rule: what you send is where the view goes.
    model.message_cursor = index;
    model.scroll_top = None;
    model.sending.push_back((model.selected, index));
    let now = model.elapsed_ms;
    if let Some(sync) = model.sync.as_mut() {
        sync.on(SyncEvent::Sent, now);
    }
    vec![UiEffect::Send {
        to,
        body,
        fields: Vec::new(),
    }]
}

/// The text a send carries: `body` without leading and trailing
/// whitespace or newlines (walk 1 #26), on every target, because every
/// target's send passes through here, `send_body` and
/// `UiModel::reply_to`. Inner lines stay as typed.
pub fn outgoing(body: String) -> String {
    let trimmed = body.trim();
    match trimmed.len() == body.len() {
        true => body,
        false => trimmed.to_string(),
    }
}

/// The glyph-table state a router state is shown as.
///
/// | router state | shown as |
/// |---|---|
/// | `Generating`, `Outbound` | `Queued` ○ |
/// | `Sending` | `InFlight` (spinner) |
/// | `Sent` | `Sent` ◇ |
/// | `AwaitingCollection` | `InMailbox` ⌂ |
/// | `Delivered` (a proof) | `Arrived` ✓ |
/// | `Rejected` | `Rejected` ✗ |
/// | `Failed`, `Cancelled` | `Failed` ✗ (dim) |
///
/// `Stamping` has no router state: the engine mines off the core and says
/// nothing until the message moves on, so slice one never draws it.
pub fn delivery_of(state: MessageState) -> DeliveryState {
    match state {
        MessageState::Generating | MessageState::Outbound => DeliveryState::Queued,
        MessageState::Sending => DeliveryState::InFlight,
        MessageState::Sent => DeliveryState::Sent,
        MessageState::AwaitingCollection => DeliveryState::InMailbox,
        // A proof is the recipient's identity, which its mailbox holds
        // too (1462): one tick. Two are the device's own word.
        MessageState::Delivered => DeliveryState::Arrived,
        MessageState::Rejected => DeliveryState::Rejected,
        MessageState::Failed | MessageState::Cancelled => DeliveryState::Failed,
    }
}

fn gives_up(state: DeliveryState) -> bool {
    matches!(state, DeliveryState::Rejected | DeliveryState::Failed)
}

fn on_engine<B: InputBuffer>(model: &mut UiModel<B>, event: OutboxEvent) -> Vec<UiEffect> {
    match event {
        OutboxEvent::Ready { address } => {
            model.address = Some(address);
            return schedule(model, SyncEvent::Start);
        }
        // Free evidence that the mailbox is reachable now.
        OutboxEvent::NodeAnnounced {
            destination,
            status,
            name,
        } => {
            model.pn_asker.announced(destination, status);
            if model.mailbox.node == Some(destination) {
                // What the header says it by (1587); an announce without a
                // name unnames it.
                model.mailbox.name = name.as_deref().and_then(crate::names::sanitise);
                return schedule(model, SyncEvent::Announce);
            }
        }
        OutboxEvent::Broken { detail } => {
            model.toast(ToastKind::Error, format!("engine: {detail}"));
            return vec![UiEffect::Notice(format!("the message engine: {detail}"))];
        }
        // The router named the message the running send handed it: the
        // oldest unfinished one, because the shell runs sends in order. A
        // fallback leg names it again with the new id.
        OutboxEvent::Queued { message_id } => {
            let now = model.now;
            if let Some(&(c, m)) = model.sending.front() {
                if let Some(mut message) = model.message_mut(c, m) {
                    // Named already by its pulled sent entry (`home_sent`),
                    // which is further along than queued.
                    if message.id == Some(message_id) {
                        return Vec::new();
                    }
                    message.id = Some(message_id);
                    message.delivery = Some(DeliveryState::Queued);
                    let id = to_hex(&message_id[..4]);
                    message.note(now, &format!("queued by the router as {id}"));
                }
                // Named in place: the summary's ids start over (1463).
                if let Some(conversation) = model.conversations.get_mut(c) {
                    conversation.summary.forget();
                }
            }
        }
        OutboxEvent::State { message_id, state } => {
            if state == MessageState::Delivered {
                // A proof says more than the node can.
                model.pn_asker.settled(&message_id);
            }
            apply_state(model, message_id, state)
        }
        // A node proved our upload (1409): asked about at the next rounds'
        // ends, on a device that is its own node.
        OutboxEvent::Uploaded {
            message_id,
            transient_id,
            node,
        } => {
            if let (Some(node), false) = (node.or(model.mailbox.node), model.home_client) {
                let now = model.now.map_or(0, |at| at.unix_secs);
                model.pn_asker.uploaded(node, message_id, transient_id, now);
            }
        }
        // What the home's propagation node said (1409).
        OutboxEvent::HomePnStatus { answers } => model.pn_answered(&answers),
        OutboxEvent::HomeStalled { message_id } => home_stalled(model, message_id),
        OutboxEvent::HomeSent {
            message_id,
            destination,
            sent_at,
            body,
            fields,
        } => home_sent(model, message_id, destination, sent_at, body, fields),
        OutboxEvent::HomePetname {
            destination,
            petname,
            at,
        } => return model.home_petname(destination, petname, at),
        OutboxEvent::HomePeerState { destination, state } => {
            return model.home_peer_state(destination, state)
        }
        OutboxEvent::HomeMarked { mark } => match mark.flag {
            crate::home::wire::MarkFlag::Read => model.read_marks(&mark.ids),
            crate::home::wire::MarkFlag::Deleted => {
                for id in &mark.ids {
                    model.forget_by_id(id);
                }
            }
            crate::home::wire::MarkFlag::Seen { .. } => model.home_seen(&mark.ids),
            // Another device said these are on it (1462): this one owes
            // its own word no more.
            crate::home::wire::MarkFlag::Arrived { .. } => model.home_arrived(&mark.ids),
        },
        OutboxEvent::Left {
            message_id,
            last: Some(state),
            ..
        } => {
            if state == MessageState::Delivered {
                model.pn_asker.settled(&message_id);
            }
            apply_state(model, message_id, state)
        }
        OutboxEvent::MeshPosition {
            identity,
            location,
            name,
        } => model.mesh_position(identity, location, name.as_deref(), 0),
        OutboxEvent::HubAnnounced { hub } => {
            explore::owe_hub(model, &hub);
            model.rooms.heard.hear(hub);
            settings::settle_suggestion(&mut model.rooms);
        }
        // Our own announce, echoed back, names nobody else: a name in it
        // is the reader's own, which the mailbox announces and its archive
        // hands every device (1449 item 2, walk 1 finding 8). It names the
        // avatar and the profile, never a conversation; an echo without a
        // name (a phone's node announces none) keeps the one heard.
        OutboxEvent::Announced {
            destination, name, ..
        } if model.address == Some(destination) => {
            if let Some(name) = name.as_deref().filter(|n| !n.trim().is_empty()) {
                model.names.heard(destination, Some(name));
            }
        }
        OutboxEvent::Announced {
            destination, name, ..
        } => {
            // Said once, in the conversation, where the reader has one with
            // the peer: a peer nobody wrote to has nothing to compare with.
            if let Heard::Changed { was, now } = model.names.heard(destination, name.as_deref()) {
                if let Some(conversation) = model
                    .conversations
                    .iter_mut()
                    .find(|c| c.peer == destination)
                {
                    conversation.notes.push(Note {
                        before: conversation.messages.len(),
                        text: format!("was {was}, now announces {now}"),
                    });
                }
            }
        }
        OutboxEvent::Propagated { message_id, source } => {
            return model.arrived_here(source, message_id);
        }
        OutboxEvent::Received {
            message_id,
            source,
            body,
            fields,
            verified,
            sent_at,
            ..
        } => {
            // The source's own word only (1482): a relay's copy carries its
            // writer's map, which says nothing of the relay.
            let lev = leviculum_lxmf::relay::own_lev(&body, &fields, &source);
            // What a relay hands back of a receipt it was sent (by a writer
            // before 1482): the writer's name over no words, with the
            // namespace alone. Nobody's post; nothing is kept.
            if relayed::noise(&body, &fields, &source) {
                return Vec::from([UiEffect::Log(format!(
                    "relayed receipt from {} dropped: no words, no field but lev",
                    crate::address::to_hex(&source)
                ))]);
            }
            // A receipt alone is a state, not a message (1230): it changes
            // the messages it names and nothing else.
            if lev
                .as_ref()
                .is_some_and(|lev| receipts::alone(&body, &fields, lev))
            {
                return model.take_lev(source, lev.as_ref());
            }
            let blocked = model.names.trust(&source) == Trust::Blocked;
            let shown = model.shows(&source);
            let index = model.conversation(source);
            // A message to a screen with no conversation open opens its
            // own, where the list shows it.
            if shown && !model.list_apart && model.conversations.get(model.selected).is_none() {
                model.selected = index;
            }
            let open = index == model.selected && shown;
            let follow = model.scroll_top.is_none();
            let conversation = &mut model.conversations[index];
            if conversation
                .with_id(&message_id, &model.ops)
                .into_iter()
                .any(|m| !conversation.messages[m].outbound)
            {
                return Vec::new();
            }
            model.learn_group(source, false, &fields);
            let state = groups::is_state(&String::from_utf8_lossy(&body), &fields);
            // When this device took it, for its ledger and the Info sheet's
            // "Received"; the bubble shows when its writer wrote it (1445).
            let at = model.now;
            let shown = written_time(at, sent_at, lev.as_ref());
            let activity = model.bump();
            let first = model.conversations[index].messages.is_empty();
            // At its writer's time (1505): a late one, written before what
            // already shows, goes above it, not under the newest.
            let placed = model.place(
                index,
                Message {
                    outbound: false,
                    size: body.len(),
                    body: String::from_utf8_lossy(&body).into_owned(),
                    id: Some(message_id),
                    delivery: None,
                    signature: match verified {
                        true => Signature::Verified,
                        false => Signature::Unverified,
                    },
                    leg: None,
                    unproven: false,
                    at: shown,
                    ledger: vec![LedgerEntry::new(
                        at,
                        match verified {
                            true => "received, signature verified",
                            false => "received, signature NOT verified",
                        },
                    )],
                    last_leg: None,
                    activity,
                    fields,
                },
            );
            let conversation = &mut model.conversations[index];
            let report = conversation.messages[placed].is_report();
            // A position report is a state, not a message (1209): it moves
            // the conversation nowhere in the list, unless it is the first
            // the conversation has.
            if !report || first {
                conversation.activity = activity;
            }
            let peer = conversation.peer;
            let reaction = Some(&conversation.messages[placed])
                .filter(|m| m.is_reaction())
                .and_then(Message::reaction);
            model.changed.insert((peer, activity));
            let mut effects = take_telemetry(model, index, placed, message_id);
            effects.extend(model.take_lev(source, lev.as_ref()));
            // A group's state (1307) is kept and stored, and asks for no
            // attention: no unread, no toast, no cursor. A group the
            // reader deleted leaves this device with it (pass four).
            if !state {
                // On this device now: its sender is told so (1462).
                effects.extend(model.arrived_here(source, message_id));
            }
            if state {
                model.forget_deleted_groups();
                return effects;
            }
            // A reaction: kept and stored, drawn under its target, and the
            // reader told; it is no unread message and moves no cursor.
            if let Some((_, content)) = reaction {
                if !blocked {
                    let from = model.names.label(&source, model.lang);
                    model.toast(ToastKind::Info, format!("{from} reacted {content}"));
                }
                return effects;
            }
            // Blocked: kept and stored, and nothing more: no unread, no
            // `new` rule, no toast. Its position is kept too; the map leaves
            // it out.
            if blocked {
                if open && follow {
                    model.message_cursor = model.conversations[index].messages.len() - 1;
                }
                return effects;
            }
            // History (1429) is not read by arriving: the open conversation
            // takes it as a closed one would, unread until the archive's
            // marks or the reader say otherwise.
            let history = model.history;
            if open && model.home_client && !history {
                // Read where it arrived, so read on the home's other devices.
                model.mark_out(crate::home::wire::MarkFlag::Read, vec![message_id]);
            }
            // A position report asks for no attention (1209): it is counted
            // in the raw unread the read marks need, and read silently when
            // the chat opens, but it raises no shown count, no `new` rule,
            // no toast and brings no archived conversation back. Its row
            // and the peer's liveness say what it changed.
            if report {
                if !open || history {
                    model.changed_peers.insert(peer);
                    model.conversations[index].unread += 1;
                }
                if open && follow {
                    model.message_cursor = model.conversations[index].messages.len() - 1;
                }
                return effects;
            }
            // Muted: counted, raised nowhere; archived and not muted: back
            // in the list (1146).
            // History raises no toast either: thousands of old messages
            // are no news.
            let raise = model.arrived(index) && !history;
            if !open || history {
                model.changed_peers.insert(peer);
                let conversation = &mut model.conversations[index];
                conversation.unread += 1;
                conversation.mark_new(placed);
                if open && follow {
                    model.message_cursor = conversation.messages.len() - 1;
                }
            } else if follow {
                let conversation = &mut model.conversations[index];
                // A reader at the newest message stays at the newest.
                model.message_cursor = conversation.messages.len() - 1;
            } else {
                // One reading earlier messages keeps the view, and the rule
                // says where the new ones begin.
                model.conversations[index].mark_new(placed);
            }
            if raise {
                let from = model.names.label(&source, model.lang);
                model.toast(ToastKind::Info, format!("message from {from}"));
            }
            return effects;
        }
        OutboxEvent::Verified {
            message_id,
            source,
            valid,
        } => apply_verdict(model, message_id, source, valid),
        _ => {}
    }
    Vec::new()
}

/// A held message's signature, checked once its sender's key arrived: the
/// block changes in place and the ledger says when. Only the messages the
/// screen holds are here; the shell writes the verdict to the store for the
/// rest.
fn apply_verdict<B>(model: &mut UiModel<B>, message_id: [u8; 32], source: [u8; 16], valid: bool) {
    let now = model.now;
    let Some(conversation) = model.conversations.iter_mut().find(|c| c.peer == source) else {
        return;
    };
    let peer = conversation.peer;
    let changed = &mut model.changed;
    conversation.messages.for_each_mut(|message| {
        if message.outbound
            || message.id != Some(message_id)
            || message.signature != Signature::Unverified
        {
            return;
        }
        let (signature, line) = match valid {
            true => (
                Signature::Verified,
                "signature verified, once the sender's key arrived",
            ),
            false => (
                Signature::Invalid,
                "signature INVALID against the sender's key",
            ),
        };
        message.signature = signature;
        message.note(now, line);
        changed.insert((peer, message.activity));
    });
    if !valid {
        let from = model.names.label(&source, model.lang);
        model.toast(
            ToastKind::Error,
            format!("invalid signature on a message from {from}"),
        );
    }
}

/// The telemetry in the newest message of conversation `index`, just
/// received: each position into the peer's latest where it is newer, and
/// into the store; each blob that did not decode named in `tui.log`, the
/// bytes kept with the message.
fn take_telemetry<B>(
    model: &mut UiModel<B>,
    index: usize,
    placed: usize,
    message_id: [u8; 32],
) -> Vec<UiEffect> {
    let Some(conversation) = model.conversations.get(index) else {
        return Vec::new();
    };
    let Some(message) = conversation.messages.get(placed) else {
        return Vec::new();
    };
    if !telemetry::has_telemetry(&message.fields) {
        return Vec::new();
    }
    let source = conversation.peer;
    let key = message.activity;
    let read = telemetry::read(source, &message.fields, model.now.map(|now| now.unix_secs));
    let mut effects = Vec::new();
    for unreadable in &read.unreadable {
        effects.push(UiEffect::Log(telemetry::unreadable_line(
            &source,
            &message_id,
            unreadable,
        )));
    }
    for mut fix in read.fixes {
        fix.message = Some(key);
        model.take_fix(fix);
    }
    effects
}

/// The fix a message from `peer` carries, the one its position line names
/// ([`telemetry::shown_fix`]): a report's, and a text's that came with a
/// telemetry field. `None` for one of ours, which is our position and not
/// the peer's, and for a message without a readable position.
pub fn message_fix(peer: [u8; 16], message: &Message) -> Option<Fix> {
    if message.outbound || !telemetry::has_telemetry(&message.fields) {
        return None;
    }
    let received = message.at.map(|at| at.unix_secs);
    let read = telemetry::read(peer, &message.fields, received);
    telemetry::shown_fix(&read).cloned()
}

/// The fix a message of ours carries, measured by `own`: a position we
/// shared (1201). `None` for any other message.
pub fn own_fix(own: [u8; 16], message: &Message) -> Option<Fix> {
    if !message.outbound || !telemetry::has_telemetry(&message.fields) {
        return None;
    }
    let read = telemetry::read(own, &message.fields, message.at.map(|at| at.unix_secs));
    telemetry::shown_fix(&read).cloned()
}

impl<B> UiModel<B> {
    /// The map's pins in `Tab` order: every peer with a position, our own
    /// named `you`.
    pub fn map_pins(&self) -> Vec<map::MapPin> {
        let mut shown: BTreeMap<[u8; 16], Fix> = self
            .positions
            .iter()
            .filter(|(peer, _)| {
                self.names.trust(peer) != Trust::Blocked || self.address == Some(**peer)
            })
            .map(|(peer, fix)| (*peer, fix.clone()))
            .collect();
        // The shell's own fix (its receiver, or typed by hand) is our pin
        // where it is newer than the last we shared (1375): a board with a
        // GNSS shows where it is before it shared anything.
        if let (Some(own), Some(location)) = (self.address, self.bar.own_fix) {
            let fix = Fix {
                peer: own,
                via: own,
                message: None,
                received_at: self.now.map(|now| now.unix_secs),
                location,
                telemetry: Vec::new(),
                source: telemetry::Source::Telemetry,
            };
            if shown.get(&own).is_none_or(|held| fix.newer_than(held)) {
                shown.insert(own, fix);
            }
        }
        map::pins_of(
            &shown,
            |peer| self.names.label(peer, self.lang),
            |peer| self.is_nearby(peer),
            self.address,
            self.now.map(|now| now.unix_secs),
            self.lang,
        )
    }

    /// Whether `peer`'s position is alive (1209), at the model's clock
    /// ([`Conversation::liveness`]); stale for a peer without one.
    pub fn liveness(&self, peer: &[u8; 16]) -> liveness::Liveness {
        let now = self.now.map(|now| now.unix_secs);
        let reports = self
            .conversations
            .iter()
            .find(|c| c.peer == *peer)
            .map(|c| c.liveness(now))
            .unwrap_or_default();
        // A position the mesh announced (1302) that is newer than every
        // report is the peer's newest fix: the row's pin and ring go by it,
        // judged as one report alone, and keep the reports' cadence.
        let announced = self
            .positions
            .get(peer)
            .filter(|fix| fix.source == telemetry::Source::Mesh)
            .map(Fix::order_time)
            .filter(|at| reports.last_fix_at.is_none_or(|last| *at > last));
        match announced {
            Some(at) => liveness::Liveness {
                state: liveness::Liveness::of(&[at], now).state,
                cadence_s: reports.cadence_s,
                last_fix_at: Some(at),
            },
            None => reports,
        }
    }

    /// The fix message `m` of conversation `c` carries ([`message_fix`]).
    pub fn message_fix(&self, c: usize, m: usize) -> Option<Fix> {
        let conversation = self.conversations.get(c)?;
        let message = conversation.messages.get(m)?;
        match message.outbound {
            // A position we shared (1201) is ours: its line shows where we
            // were, measured by us.
            true => self.address.and_then(|own| own_fix(own, message)),
            false => message_fix(conversation.peer, message),
        }
    }

    /// Keep the pane the keys are in as the one the map returns to
    /// (1531); a map opened over the map keeps the first one.
    fn remember_map_from(&mut self) {
        if self.focus != Pane::Map {
            self.map_from = self.focus;
        }
    }

    /// The cells the map's picture covers: the shell's own place for it
    /// ([`UiModel::map_picture`]), else inside the conversation pane.
    pub fn map_area(&self) -> Rect
    where
        B: InputBuffer,
    {
        match self.map_picture {
            Some(picture) => picture,
            None => map::picture_area(
                Screen::of(self.screen.0, self.screen.1, self.compose_rows()).messages,
            ),
        }
    }

    /// The map is being dragged: the shell draws it at its tick, not at
    /// every motion the terminal reports.
    pub fn map_dragging(&self) -> bool {
        self.map.is_some() && self.map_press.is_some_and(|p| p.dragging)
    }

    /// The open map as drawn: its view and the pins in it.
    pub fn map_view(&self) -> Option<map::MapView>
    where
        B: InputBuffer,
    {
        let state = self.map.as_ref()?;
        let area = self.map_area();
        let viewport = map::viewport(state, area, &self.map_setup);
        let mut shown = self.map_pins();
        // No region touches the view: the radar (1386), its pins labelled
        // by distance and compass point from the centre.
        let radar = match radar::shows(&viewport, &self.map_setup.regions) {
            true => radar::radar(&shown, viewport_unit(&viewport), viewport.size(), self.lang),
            false => None,
        };
        if let Some(r) = &radar {
            radar::label(&mut shown, r);
        }
        let pins = map::place(
            &shown,
            &viewport,
            area,
            &self.map_setup,
            state.selected,
            self.ascii,
        );
        // The tracks under the pins: a pixel of the view is an image
        // pixel, a place's cell two cells across.
        let unit = libm::exp2(24.0 - f64::from(viewport.zoom()));
        let scale = f32::from(self.map_setup.cell_px.0.max(1)) / 4.0;
        let tracks = tracks::cells::cells(
            &self.map_tracks(unit, scale),
            &viewport,
            area,
            self.map_setup.cell_px,
            self.ascii,
        );
        let radar = radar.map(|r| map::RadarCells {
            note: r.note(),
            cells: radar::cells(&r, &viewport, area, self.map_setup.cell_px, self.ascii),
        });
        Some(map::MapView {
            area,
            viewport,
            cell_px: self.map_setup.cell_px,
            pins,
            tracks,
            radar,
            window_word: self
                .track_window
                .title_word()
                .map(|w| w.text(self.lang).to_string()),
            moving: self.map_press.as_ref().is_some_and(|p| p.dragging),
        })
    }

    /// One position received: the peer's latest when it is newer than what
    /// the screen holds, and for the store either way, which keeps the track.
    fn take_fix(&mut self, fix: Fix) {
        let newer = self
            .positions
            .get(&fix.peer)
            .is_none_or(|held| fix.newer_than(held));
        if newer {
            self.positions.insert(fix.peer, fix.clone());
        }
        tracks::insert(
            alloc::sync::Arc::make_mut(self.tracks.entry(fix.peer).or_default()),
            tracks::TrackFix::of(&fix),
        );
        self.fixes_received.push(fix);
    }

    /// The store's fixes at start, every peer's in time order: the tracks.
    pub fn restore_tracks(&mut self, fixes: Vec<Fix>) {
        for fix in fixes {
            tracks::insert(
                alloc::sync::Arc::make_mut(self.tracks.entry(fix.peer).or_default()),
                tracks::TrackFix::of(&fix),
            );
        }
    }

    /// The tracks of every peer the map shows a pin for (blocked peers
    /// have none), drawn on a view whose screen pixel spans `unit` world
    /// units at `scale` ([`tracks::track`]), in the model's window; none
    /// before the shell has said the time.
    pub fn map_tracks(&self, unit: f64, scale: f32) -> Vec<tracks::Track> {
        let Some(now) = self.now else {
            return Vec::new();
        };
        tracks::tracks(
            &self.track_input(),
            self.track_window,
            &now,
            unit,
            scale,
            self.lang,
        )
    }

    /// The fixes the map draws tracks of: every peer's but a blocked
    /// one's, ours marked; shared, so a frontend that draws them at its
    /// own zoom takes them without a copy.
    pub fn track_input(&self) -> tracks::TrackInput {
        self.tracks
            .iter()
            .filter(|(peer, fixes)| {
                !fixes.is_empty()
                    && (self.names.trust(peer) != Trust::Blocked || self.address == Some(**peer))
            })
            .map(|(peer, fixes)| (*peer, self.address == Some(*peer), fixes.clone()))
            .collect()
    }

    /// The store's latest positions at start, one a peer.
    pub fn restore_positions(&mut self, fixes: Vec<Fix>) {
        for fix in fixes {
            let newer = self
                .positions
                .get(&fix.peer)
                .is_none_or(|held| fix.newer_than(held));
            if newer {
                self.positions.insert(fix.peer, fix);
            }
        }
    }

    /// A position the mesh announced (1302): the peer is the delivery
    /// address of the identity that announced, so it joins the peer's
    /// telemetry positions, and the newest fix wins by [`Fix::newer_than`]
    /// whichever way it came; [`Fix::source`] says which. The store keeps
    /// it for the track (1385), marked as announced.
    pub fn mesh_position(
        &mut self,
        identity: [u8; 16],
        location: leviculum_lxmf::telemetry::Location,
        name: Option<&str>,
        age: u32,
    ) {
        let peer = crate::mesh_position::delivery_of(&identity);
        if name.is_some() {
            self.names.heard(peer, name);
        }
        let fix = Fix {
            peer,
            via: peer,
            message: None,
            received_at: self
                .now
                .map(|now| now.unix_secs.saturating_sub(i64::from(age))),
            location,
            telemetry: Vec::new(),
            source: telemetry::Source::Mesh,
        };
        let newer = self
            .positions
            .get(&peer)
            .is_none_or(|held| fix.newer_than(held));
        tracks::insert(
            alloc::sync::Arc::make_mut(self.tracks.entry(peer).or_default()),
            tracks::TrackFix::of(&fix),
        );
        // The track keeps it (1385): stored, one row a peer and fix time,
        // so a node announcing the same fix again costs nothing.
        self.fixes_received.push(fix.clone());
        if newer {
            self.positions.insert(peer, fix);
        }
    }

    /// Whether `peer` is one of the reader's people: ourselves, a peer
    /// with a conversation, or one the reader named or linked. A node the
    /// mesh shows on the map and is none of these is nearby (1302).
    pub fn is_contact(&self, peer: &[u8; 16]) -> bool {
        self.address == Some(*peer)
            || self.conversations.iter().any(|c| c.peer == *peer)
            || self.names.trust(peer) != Trust::Unknown
    }

    /// Whether `peer`'s pin is a node nearby (1302): its position came
    /// from the mesh, and it is none of the reader's people. A peer known
    /// from a collector's stream keeps the pin it had.
    pub fn is_nearby(&self, peer: &[u8; 16]) -> bool {
        self.positions
            .get(peer)
            .is_some_and(|fix| fix.source == telemetry::Source::Mesh)
            && !self.is_contact(peer)
    }

    /// The card's "Add" on a node nearby (1302): its conversation is
    /// listed and opened as a friend's code opens it (1263), and the name
    /// it announced becomes the reader's name for it, so it stays a
    /// contact by name whatever the mesh says next.
    pub(crate) fn add_nearby(&mut self, peer: [u8; 16]) -> Vec<UiEffect> {
        if self.address == Some(peer) {
            return Vec::new();
        }
        self.show_blocked = false;
        let index = self.conversation(peer);
        self.select(index);
        self.focus = Pane::Compose;
        match self.names.announced(&peer).map(str::to_string) {
            Some(name) if self.names.petname(&peer).is_none() => self.rename(peer, &name),
            _ => Vec::new(),
        }
    }
}

impl<B> UiModel<B> {
    /// Message `message` of conversation `conversation`, to change: the
    /// store will learn of it.
    fn message_mut(
        &mut self,
        conversation: usize,
        message: usize,
    ) -> Option<id_index::MessageMut<'_>> {
        self.touch(conversation, message);
        self.conversations
            .get_mut(conversation)?
            .messages
            .get_mut(message)
    }
}

fn apply_state<B>(model: &mut UiModel<B>, id: [u8; 32], state: MessageState) {
    let delivered = delivery_of(state);
    let now = model.now;
    let running = model.sending.clone();
    let changed = &mut model.changed;
    let groups = &model.groups;
    let ops = &model.ops;
    for (c, conversation) in model.conversations.iter_mut().enumerate() {
        // In a group (1307 pass four) the group's proof is one tick: the
        // group has it. Its status moves it on (1466: arrived once every
        // member's copy is proven, two ticks once every copy is on a
        // device), and a late proof takes neither back.
        let group = groups.contains_key(&conversation.peer);
        let shown = match delivered {
            DeliveryState::Arrived if group => DeliveryState::InMailbox,
            other => other,
        };
        // By the summary's index, not a walk over every message (1463).
        for m in conversation.with_id(&id, ops) {
            let Some(mut message) = conversation.messages.get_mut(m) else {
                continue;
            };
            if !message.outbound {
                continue;
            }
            if group
                && matches!(
                    message.delivery,
                    Some(DeliveryState::Arrived | DeliveryState::Delivered)
                )
            {
                continue;
            }
            if gives_up(shown) && running.contains(&(c, m)) {
                // The runner may still carry it another way.
                continue;
            }
            // Read implies delivered (1230), and the device's own word
            // (1462) says more than its identity's proof: a late proof
            // leaves either.
            if matches!(
                message.delivery,
                Some(DeliveryState::Read { .. } | DeliveryState::Delivered)
            ) {
                continue;
            }
            if message.unproven
                && !matches!(shown, DeliveryState::Arrived | DeliveryState::Rejected)
                && !(group && delivered == DeliveryState::Arrived)
            {
                continue;
            }
            if message.delivery != Some(shown) {
                let words = match group && delivered == DeliveryState::Arrived {
                    true => "the group has it: proof received",
                    false => ledger_words(shown),
                };
                let line = match message.last_leg {
                    Some(leg) => format!("{words} ({})", leg_words(leg)),
                    None => words.to_string(),
                };
                message.note(now, &line);
                changed.insert((conversation.peer, message.activity));
            }
            message.delivery = Some(shown);
        }
    }
}

/// The home node gave up a leg of `id` that went without an end (1293):
/// the ledger says so, and the bubble keeps its state, since the message
/// is tried again.
fn home_stalled<B>(model: &mut UiModel<B>, id: [u8; 32]) {
    let now = model.now;
    for conversation in &mut model.conversations {
        let peer = conversation.peer;
        let changed = &mut model.changed;
        conversation.messages.for_each_mut(|message| {
            if message.outbound && message.id == Some(id) {
                message.note(now, STALLED_NOTE);
                changed.insert((peer, message.activity));
            }
        });
    }
}

/// The ledger line of a leg the home node gave up (1293).
pub const STALLED_NOTE: &str = "sending stopped without an answer, trying again";

/// A message one of the home's devices sent through the home node: the
/// bubble this device has for it says sent, and a device without one gets
/// it. A bubble further along (waiting at a propagation node, delivered) stays: a pull
/// asked again after a crash brings the sent entry a second time.
/// The time a received message shows (1445): when its writer wrote it,
/// not when this device took it, so a history a new device pulls at once
/// keeps its days. A group copy's own timestamp is the home's acceptance,
/// the group's order (`crate::home::group`), so its writer's time comes
/// from the copy's `lev` pair. A time from the future (a writer's clock
/// ahead of ours) shows as now, and one not known (0) as now too; `None`
/// before the shell said the time.
fn written_time(
    now: Option<WallTime>,
    sent_at: i64,
    lev: Option<&leviculum_lxmf::lev::Lev>,
) -> Option<WallTime> {
    let written = lev
        .and_then(|lev| lev.group_copy)
        .and_then(|copy| i64::try_from(copy.sent_ms / 1000).ok())
        .unwrap_or(sent_at);
    now.map(|now| WallTime {
        unix_secs: match written > 0 {
            true => written.min(now.unix_secs),
            false => now.unix_secs,
        },
        ..now
    })
}

fn home_sent<B: InputBuffer>(
    model: &mut UiModel<B>,
    message_id: [u8; 32],
    destination: [u8; 16],
    sent_at: i64,
    body: Vec<u8>,
    fields: Vec<leviculum_lxmf::Field>,
) {
    // A receipt the home node signed (1234) is a state, not a bubble.
    if leviculum_lxmf::lev::Lev::from_fields(&fields)
        .is_some_and(|lev| receipts::alone(&body, &fields, &lev))
    {
        return;
    }
    let now = model.now;
    let mut found = false;
    for conversation in &mut model.conversations {
        // The messages that carry the id, by the summary's index (1463).
        for m in conversation.with_id(&message_id, &model.ops) {
            let Some(mut message) = conversation.messages.get_mut(m) else {
                continue;
            };
            if !message.outbound || message.id != Some(message_id) {
                continue;
            }
            found = true;
            if matches!(
                message.delivery,
                None | Some(DeliveryState::Queued | DeliveryState::InFlight)
            ) {
                message.delivery = Some(DeliveryState::Sent);
                message.note(now, "taken by the mailbox");
                model.changed.insert((conversation.peer, message.activity));
            }
        }
    }
    if found {
        return;
    }
    // The running send's own entry, pulled before the runner named it: the
    // home node answered the send on one link and the pull read its log on
    // another, so the pull can win (1335). Its bubble takes the id here and
    // the late `Queued` finds it named.
    if let Some(&(c, m)) = model.sending.front() {
        let running = model
            .conversations
            .get(c)
            .filter(|conversation| conversation.peer == destination)
            .and_then(|conversation| conversation.messages.get(m))
            .is_some_and(|message| {
                message.outbound && message.id.is_none() && message.body.as_bytes() == body
            });
        if running {
            if let Some(mut message) = model.message_mut(c, m) {
                message.id = Some(message_id);
                message.delivery = Some(DeliveryState::Sent);
                message.note(now, "taken by the mailbox");
            }
            // Named in place: the summary's ids start over (1463).
            if let Some(conversation) = model.conversations.get_mut(c) {
                conversation.summary.forget();
            }
            return;
        }
    }
    let index = model.conversation(destination);
    let activity = model.bump();
    // Its time is when the other device sent it, not when this one heard
    // of it: a read receipt is kept no earlier than the send (1234).
    let at = now.map(|now| WallTime {
        unix_secs: sent_at,
        ..now
    });
    model.learn_group(destination, true, &fields);
    let mut message = Message::ours(String::from_utf8_lossy(&body).into_owned(), at);
    message.id = Some(message_id);
    message.delivery = Some(DeliveryState::Sent);
    message.note(now, "sent from another device, taken by the mailbox");
    message.activity = activity;
    message.fields = fields;
    model.conversations[index].activity = activity;
    // At the time the other device sent it (1505), among what shows.
    model.place(index, message);
    let peer = model.conversations[index].peer;
    model.changed.insert((peer, activity));
    if index == model.selected && model.scroll_top.is_none() {
        model.message_cursor = model.conversations[index].messages.len() - 1;
    }
}

fn on_send_done<B>(model: &mut UiModel<B>, result: Result<(), String>) -> Vec<UiEffect> {
    let Some((c, m)) = model.sending.pop_front() else {
        return Vec::new();
    };
    // A receipt of its own has no bubble: a failure is the log's.
    if c == receipts::RECEIPT_SEND {
        return match result {
            Ok(()) => Vec::new(),
            Err(detail) => vec![UiEffect::Log(format!("RECEIPT not sent: {detail}"))],
        };
    }
    // Nor has a group's state (1307), but the reader asked for it: a
    // failure is said.
    if c == groups::STATE_SEND {
        return match result {
            Ok(()) => pull_after_send(model),
            Err(detail) => {
                model.toast(ToastKind::Error, NOT_SENT.to_string());
                vec![UiEffect::Notice(format!("group: {detail}"))]
            }
        };
    }
    if let Some(mut message) = model.message_mut(c, m) {
        message.leg = None;
    }
    let Err(detail) = result else {
        return pull_after_send(model);
    };
    // Each leg's reason as the runner worded it, its continuation lines cut:
    // an auto send that failed both ways says why each failed, not only why
    // the direct one did.
    let reasons: Vec<&str> = detail
        .lines()
        .filter(|line| !line.starts_with(' '))
        .collect();
    let text = format!("not sent: {}", reasons.join("; "));
    let now = model.now;
    if let Some(mut message) = model.message_mut(c, m) {
        message.delivery = Some(DeliveryState::Failed);
        message.note(now, &text);
    }
    // The legs are the detail view's (1279, walk finding 22): the toast says
    // what happened in plain words and where the why is.
    model.toast(ToastKind::Error, NOT_SENT.to_string());
    vec![UiEffect::Notice(format!("send: {detail}"))]
}

fn on_send_unproven<B>(model: &mut UiModel<B>, detail: String) -> Vec<UiEffect> {
    let Some((c, m)) = model.sending.pop_front() else {
        return Vec::new();
    };
    if c == receipts::RECEIPT_SEND || c == groups::STATE_SEND {
        return Vec::new();
    }
    let now = model.now;
    if let Some(mut message) = model.message_mut(c, m) {
        message.leg = None;
        message.unproven = true;
        message.note(now, "sent over a link, no delivery proof yet");
        // A proof that came in the same breath as the runner's end stays.
        if !matches!(
            message.delivery,
            Some(DeliveryState::Arrived | DeliveryState::Delivered | DeliveryState::Read { .. })
        ) {
            message.delivery = Some(DeliveryState::Sent);
        }
    }
    model.toast(
        ToastKind::Info,
        "sent over a link, no delivery proof yet".to_string(),
    );
    vec![UiEffect::Notice(format!("send: {detail}"))]
}

/// What a footer button runs when clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FooterAct {
    /// Its row's action, the one its key runs; `None` for a row the mouse
    /// leaves alone, the transmitting `send`, whose click target is the
    /// word on the compose box's border.
    Row(Option<Action>),
    /// The open palette's `Enter run`.
    PaletteRun,
    /// The open palette's `Esc close`.
    PaletteClose,
}

impl FooterRole for FooterAct {
    fn is_nav(&self) -> bool {
        false
    }
}

/// The palette as drawn.
#[derive(Clone, Debug)]
pub struct PaletteView {
    /// The query line.
    pub query: String,
    /// The cursor in it, in characters.
    pub cursor: usize,
    /// The matching names, the highlighted one bold behind `▸`.
    pub rows: Vec<RLine>,
}

/// A message's detail view as drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailView {
    /// The overlay's title.
    pub title: String,
    /// Its lines: who, the peer, the id, the size, the state, the ledger.
    pub rows: Vec<String>,
    /// Lines drawn as they are under the others, never wrapped: a QR code.
    pub fixed: Vec<String>,
}

/// Our own contact code as `my code` shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnCode {
    /// The `lxma://` code, what a friend pastes.
    pub code: String,
    /// Its QR code verbatim in byte mode, as the phone's and the
    /// window's scanners read it (`loquax::home::qr::verbatim_modules`):
    /// `side` modules a side, row by row, true for dark. A side of 0 where
    /// the build encodes none (no `qr` feature).
    pub side: u32,
    pub modules: Vec<bool>,
}

impl OwnCode {
    /// `code` with its QR, the side and the modules, where there is one.
    pub fn of(code: String, qr: Option<(u32, Vec<bool>)>) -> Self {
        let (side, modules) = qr.unwrap_or_default();
        Self {
            code,
            side,
            modules,
        }
    }
}

/// What each part of the screen shows.
#[derive(Clone, Debug)]
pub struct View {
    /// The conversations pane's title: our own address, short, or `Search`
    /// while a search is open.
    pub conversations_title: String,
    /// Its right end: `esc closes` while a search is open.
    pub conversations_title_right: String,
    /// On its bottom border, right-aligned: `/ search`, the click that
    /// opens the search, while none is open.
    pub list_hint: String,
    /// The column of the cursor in the search field, in the pane, while the
    /// field has the focus.
    pub search_cursor: Option<usize>,
    /// The list's rows on screen, two a conversation and a blank between.
    pub conversations: Vec<RLine>,
    /// The conversation's header: the peer's name and trust state.
    pub messages_title: String,
    /// The header's right end: the mailbox state, or the peer's full address
    /// on a wide pane, where the mailbox has a row of its own.
    pub messages_title_right: String,
    /// How many characters the peer's position takes at the start of where
    /// the header draws it: the row under the title past its one blank on a
    /// wide pane, the title's right end on a narrow one. A click there puts
    /// the peer on the map.
    pub position_len: usize,
    /// The conversation pane's rows, exactly its height: the header's rows on
    /// a wide pane, then the blocks, the newest at the bottom unless the
    /// reader scrolled up.
    pub messages: Vec<RLine>,
    /// The compose box's title: whom it writes to.
    pub compose_title: String,
    /// The compose line and its cursor, in characters.
    pub compose: String,
    pub compose_cursor: usize,
    /// The compose pane's border hint: the chords that send and break the
    /// line in this terminal and setting ([`keys::compose_hint`]).
    pub compose_hint: String,
    /// While a voice message records for the open conversation (1276):
    /// the recording bar, drawn in the compose box in the field's place.
    pub recording: Option<layout::RecRows>,
    /// Where the focus is: the renderer marks that pane.
    pub focus: Pane,
    /// The footer buttons.
    pub footer: Vec<FooterItem<FooterAct>>,
    /// The footer button under the pointer, a slot's index.
    pub footer_hover: Option<usize>,
    /// The toast to float, if any.
    pub toast: Option<Toast>,
    /// The palette, when open.
    pub palette: Option<PaletteView>,
    /// Whether the help overlay is up; its text is [`help_groups`].
    pub help: bool,
    /// The help's first row on screen.
    pub help_scroll: usize,
    /// The detail view, when open.
    pub detail: Option<DetailView>,
    /// The detail view's first row on screen.
    pub detail_scroll: usize,
    /// The colour tier and the theme the screen is drawn in.
    pub tier: Tier,
    pub theme: Theme,
    /// The config's `map_theme` ([`UiModel::map_theme`]).
    pub map_theme: Option<ThemeFlag>,
    /// The map, when open, drawn in the conversation pane's place.
    pub map: Option<map::MapView>,
    /// The glyph test pattern over the open map.
    pub map_glyph_test: bool,
    /// The time zone view over the open map (1684).
    pub time_zones: bool,
}

/// The map pane's title.
pub const MAP_TITLE: &str = "map";

/// How many ticks the map's title names a new way of drawing it: two
/// seconds at the shell's quarter-second tick.
pub const MAP_NAMED_TICKS: u64 = 8;

/// The map title's word while a region loads.
pub const MAP_LOADING: &str = "loading map";

/// The map title's words while a region loads for what is on screen
/// (1527), the phone chip's "Loading the map of this area".
pub const MAP_LOADING_HERE: &str = "loading the map of this area";

/// The window's chip for the same (1527), the phone's words.
pub const MAP_LOADING_HERE_WORDS: &str = "Loading the map of this area";

/// The toast for `load map area` over an area loaded already.
pub const MAP_LOADED_ALREADY: &str = "the map here is loaded already";

/// What the share's chip row says after its words in the terminal (1273).
pub const SHARE_CLICK: &str = "click to stop";

/// The toast of a send that failed: the legs' reasons are in its details.
pub const NOT_SENT: &str = "Not sent. Its details say why.";

/// The empty conversations pane's line, in the reader's language.
fn no_conversations(lang: strings::Lang) -> &'static str {
    strings::StrId::NoConversationsTyped.text(lang)
}

/// The `blocked` view's line while nothing is blocked.
pub const NO_BLOCKED: &str = "Nothing blocked. :blocked returns.";

/// The list pane's title in the `blocked` view, and its right end: the
/// palette name that returns, short enough for the narrow list.
pub const BLOCKED_TITLE: &str = "Blocked";
pub const BLOCKED_RETURNS: &str = ":blocked";

/// The archived view's (1146) line while nothing is archived, its title and
/// its right end.
pub const NO_ARCHIVED: &str = "Nothing archived. :archived returns.";
pub const ARCHIVED_TITLE: &str = "Archived";
pub const ARCHIVED_RETURNS: &str = ":archived";

/// The list's bottom border while no search is open: a click on it opens
/// one (section 5, "What the mouse does").
pub const SEARCH_HINT: &str = "/ search";

/// The list pane's title while a search is open, and its right end.
pub const SEARCH_TITLE: &str = "Search";
pub const SEARCH_CLOSES: &str = "esc closes";

/// The list pane while a search is open, `width` x `height` inside: the
/// field and a blank, then the plain list while the field is empty, else the
/// results scrolled to show the selected one. The cursor's column in the
/// field comes with them.
fn search_pane<B: InputBuffer>(
    model: &UiModel<B>,
    open: &search::Search<B>,
    width: usize,
    height: usize,
) -> (Vec<RLine>, usize) {
    const PREFIX: &str = " / ";
    let query: Vec<char> = open.query.value().chars().collect();
    let cursor = open.query.cursor().min(query.len());
    let visible = width.saturating_sub(PREFIX.len() + 1).max(1);
    let scroll = (cursor + 1).saturating_sub(visible);
    let shown: String = query.iter().skip(scroll).take(visible).collect();
    let mut rows = vec![plain(&format!("{PREFIX}{shown}")), RLine::default()];
    let below = height.saturating_sub(search::FIELD_ROWS);
    match model.query() {
        None if model.conversations.is_empty() => {
            rows.extend(wrap(&[plain(no_conversations(model.lang))], width, 0));
        }
        None => rows.extend(layout::list(model, width, below)),
        Some(query) => {
            let laid = search::rows(model, query, width);
            let first = search::first_row(&laid, open.selected, below);
            rows.extend(laid.rows.into_iter().skip(first).take(below));
        }
    }
    rows.truncate(height);
    (rows, PREFIX.len() + cursor - scroll)
}

/// An address cut to what a pane shows.
pub fn short(address: &[u8; 16]) -> String {
    let mut hex = to_hex(address);
    hex.truncate(SHORT_HEX);
    hex
}

/// The header's title: the peer as [`Names::label`] says it, its trust
/// state in words, what it announces beside a petname, and the sentence of
/// a shared name (1452). The default state, a peer the reader has not named, takes no
/// word (1279, walk finding 21): "unknown" on every new chat read as a
/// warning about nothing.
pub fn header_title(names: &Names, peer: &[u8; 16], lang: strings::Lang) -> String {
    let mut words = match names.trust(peer) {
        Trust::Unknown => Vec::new(),
        trust => vec![trust.as_str().to_string()],
    };
    words.extend(names.header_notes(peer));
    // Two peers of one name say so (1452), after the tails that tell them
    // apart.
    words.extend(names.same_name(peer).map(|sentence| sentence.resolve(lang)));
    match words.is_empty() {
        true => names.label(peer, lang),
        false => format!("{}  {}", names.label(peer, lang), words.join(" · ")),
    }
}

fn push(line: &mut RLine, text: &str, st: RStyle) {
    line.cells.extend(text.chars().map(|ch| StyledChar {
        ch,
        st,
        link: None,
        field: None,
    }));
}

fn plain(text: &str) -> RLine {
    let mut line = RLine::default();
    push(&mut line, text, RStyle::default());
    line
}

fn bold() -> RStyle {
    RStyle {
        bold: true,
        ..RStyle::default()
    }
}

/// How long a client's quiet pull runs before the header says so.
pub const QUIET_PULL_MS: u64 = 1_000;

/// Whether the header says a fetch runs: one the reader asked for or the
/// schedule started, at once; a client's quiet pull, for a wake or after a
/// send, only once it took longer than [`QUIET_PULL_MS`]. A wake comes for
/// every message, state and mark, and a line that said each pull would
/// flicker (1136).
fn fetch_shown<B>(model: &UiModel<B>) -> bool {
    let quiet = matches!(model.fetch_reason, Some(Reason::Wake | Reason::Sent));
    model.fetching
        && (!quiet || model.elapsed_ms.saturating_sub(model.fetch_since_ms) >= QUIET_PULL_MS)
}

/// The propagation node's state in the header, longest first: the renderer
/// takes the first that fits. The word is long, so the narrower forms say
/// `pn` (1153: "mailbox" now means the user's Loquax Mailbox).
fn mailbox_words<B>(model: &UiModel<B>) -> Vec<String> {
    if model.mailbox.node.is_none() {
        // Without a relay nothing is said until a send needs one (1279, walk
        // finding 21): "no propagation node" in every chat was the default
        // state worded as a fault.
        return match relay_needed(model) {
            true => vec!["no relay".to_string()],
            false => Vec::new(),
        };
    }
    // The relay by its announced name, else "a relay": never its hash on
    // the screen (1587, the names rule of 1452); the hash is the CLI's and
    // the log's `propagation node:` line.
    let node = match &model.mailbox.name {
        Some(name) => strings::Text::Key(
            strings::StrId::RelayNamed,
            Vec::from([strings::Arg::Str(name.clone())]),
        ),
        None => strings::Text::key(strings::StrId::RelayUnnamed),
    }
    .resolve(model.lang);
    let how = if model.mailbox.by_user {
        "chosen"
    } else {
        "adopted"
    };
    let sync = match (fetch_shown(model), model.mailbox.synced, model.now) {
        (true, _, _) => Some("fetching".to_string()),
        (false, Some(at), Some(now)) => Some(format!("synced {}", list_time(&at, &now))),
        (false, Some(at), None) => Some(format!("synced {}", at.hh_mm())),
        (false, None, _) => None,
    };
    let next = next_sync(model).map(|at| format!("next {}", at.hh_mm()));
    let mut words = Vec::new();
    match (&sync, &next) {
        (Some(sync), Some(next)) => {
            words.push(format!("{node} {how} · {sync} · {next}"));
            words.push(format!("propagation node · {sync} · {next}"));
            words.push(format!("pn · {sync} · {next}"));
            words.push(format!("pn · {next}"));
        }
        (Some(only), None) | (None, Some(only)) => {
            words.push(format!("{node} {how} · {only}"));
            words.push(format!("propagation node · {only}"));
            words.push(format!("pn · {only}"));
        }
        (None, None) => words.push(format!("{node} {how}")),
    }
    words.push(node);
    words.push("pn".to_string());
    words
}

/// Whether a send in the open conversation needs a relay: its newest
/// message of ours is still waiting to be handed on, or was not sent.
fn relay_needed<B>(model: &UiModel<B>) -> bool {
    let newest = model
        .conversations
        .get(model.selected)
        .and_then(|c| c.messages.iter().rev().find(|m| m.outbound));
    matches!(
        newest.and_then(|m| m.delivery),
        Some(
            DeliveryState::Queued
                | DeliveryState::Stamping
                | DeliveryState::InFlight
                | DeliveryState::Rejected
                | DeliveryState::Failed
        )
    )
}

/// When the schedule's next sync is due, as a time of day, up to the whole
/// minute: none without a
/// schedule, with its timer off, before the first sync or while one runs.
fn next_sync<B>(model: &UiModel<B>) -> Option<WallTime> {
    let next = model.sync.as_ref()?.next_ms()?;
    let now = model.now?;
    let ahead_secs = next.saturating_sub(model.elapsed_ms).div_ceil(1000);
    let at = now
        .unix_secs
        .saturating_add(i64::try_from(ahead_secs).unwrap_or(i64::MAX));
    // Up to the minute the header shows: `next 14:20` for a sync due at
    // 14:19:59, never a minute that has already gone.
    Some(WallTime {
        unix_secs: at.saturating_add(59).div_euclid(60).saturating_mul(60),
        offset_secs: now.offset_secs,
    })
}

/// The detail view of the message `key` (its [`Message::activity`]) in
/// `peer`'s conversation, what `v` opens in the terminal: for a frontend
/// that shows it its own way.
pub fn message_detail<B>(model: &UiModel<B>, peer: [u8; 16], key: u64) -> Option<DetailView> {
    let c = model.conversations.iter().position(|c| c.peer == peer)?;
    let m = model.conversations[c]
        .messages
        .iter()
        .position(|m| m.activity == key)?;
    detail_view(model, c, m)
}

/// The detail view of message `m` of conversation `c`.
fn detail_view<B>(model: &UiModel<B>, c: usize, m: usize) -> Option<DetailView> {
    let conversation = model.conversations.get(c)?;
    let message = conversation.messages.get(m)?;
    let who = model.names.label(&conversation.peer, model.lang);
    let offset = message.at.or(model.now).map_or(0, |at| at.offset_secs);
    let mut rows = vec![
        match message.outbound {
            true => format!("from you to {who}"),
            false => format!("from {who} to you"),
        },
        format!("peer   {}", to_hex(&conversation.peer)),
        match message.id {
            Some(id) => format!("id     {}", to_hex(&id)),
            None => "id     none yet: the router has not queued it".to_string(),
        },
        format!("size   {} bytes", message.size),
        match (message.delivery, message.signature) {
            (Some(state), _) => {
                let state = shown_delivery(state, model.home_client, model.reading.on);
                let words = match (state, arrival::witness(message)) {
                    (DeliveryState::Arrived, Some((arrival::Witness::Collected, at))) => {
                        format!("collected from the propagation network{}", at_words(at))
                    }
                    (
                        DeliveryState::Sent | DeliveryState::InMailbox,
                        Some((arrival::Witness::NotCollected, _)),
                    ) => "not collected at the propagation node; perhaps delivered via another"
                        .to_string(),
                    _ => detail_words(state, &who, offset),
                };
                format!("state  {words}")
            }
            (None, Signature::Verified) => "signature verified".to_string(),
            (None, Signature::Unverified) => "signature NOT verified".to_string(),
            (None, Signature::Invalid) => "signature INVALID".to_string(),
        },
    ];
    if let Some(why) = no_read_words(model, conversation, message, &who) {
        rows.push(why);
    }
    if let (true, Some(id)) = (message.outbound, message.id) {
        rows.extend(model.group_detail_rows(&conversation.peer, &id));
    }
    let received = message.at.map(|at| at.unix_secs);
    let sensors = telemetry::detail_rows(&conversation.peer, &message.fields, offset, received);
    if !sensors.is_empty() {
        rows.push(String::new());
        rows.extend(sensors);
    }
    rows.push(String::new());
    rows.push("on this device".to_string());
    for entry in &message.ledger {
        let at = entry
            .at
            .map(|at| at.hh_mm_ss())
            .unwrap_or_else(|| "--:--:--".to_string());
        rows.push(format!(
            "{at}  {}",
            groups::ledger_line(&entry.what, model.lang)
        ));
    }
    Some(DetailView {
        title: "message".to_string(),
        rows,
        fixed: Vec::new(),
    })
}

/// The profile's view (1443): the scene's [`profile::ProfileScreen`] in
/// the terminal's detail view, its words in the reader's language: the
/// name, the address in fours, the mailbox and how it is reached, the
/// devices, then the code a friend pastes and its QR to scan, and the key
/// that shares the address.
fn profile_view(screen: &profile::ProfileScreen, lang: strings::Lang) -> DetailView {
    let mut rows = vec![
        screen.name.resolve(lang),
        String::new(),
        screen.address_label.resolve(lang),
        screen.fours.clone(),
        String::new(),
        screen.mailbox.resolve(lang),
    ];
    rows.extend(screen.state.as_ref().map(|state| state.resolve(lang)));
    if !screen.devices.is_empty() {
        rows.push(String::new());
        rows.push(screen.devices_label.resolve(lang));
        rows.extend(
            screen
                .devices
                .iter()
                .map(|d| format!("  {}", d.resolve(lang))),
        );
    }
    let mut fixed = Vec::new();
    if let Some(code) = &screen.code {
        rows.push(String::new());
        rows.push(format!("s  {}", strings::StrId::ProfileShare.text(lang)));
        rows.push(String::new());
        rows.push(screen.caption.resolve(lang));
        rows.push(String::new());
        rows.push(code.clone());
        rows.push(String::new());
        fixed = screen
            .qr
            .as_ref()
            .map(|qr| qr.half_blocks(QR_QUIET))
            .unwrap_or_default();
    }
    DetailView {
        title: screen.title.resolve(lang),
        rows,
        fixed,
    }
}

/// The light margin a scanner needs around a QR code, in modules.
const QR_QUIET: u32 = 4;

/// The detail view up: the profile's, a message's, or the settings'.
fn shown_detail<B>(model: &UiModel<B>) -> Option<DetailView> {
    if let Some(screen) = model.profile_screen() {
        return Some(profile_view(&screen, model.lang));
    }
    if let Some(view) = model.detail.and_then(|(c, m)| detail_view(model, c, m)) {
        return Some(view);
    }
    model.settings.map(|at| settings_view(model, at))
}

/// The settings in the terminal's detail view (1450): the nine sections
/// of [`settings::SECTIONS`] numbered as the phone lists them, or one
/// section's rows, each with the key that runs it and the value in force.
fn settings_view<B>(model: &UiModel<B>, at: Option<settings::Section>) -> DetailView {
    use settings::Section;
    let lang = model.lang;
    let screen = settings::screen(model, settings::Target::Terminal, at);
    let mut rows = Vec::new();
    let foot = match at {
        None => {
            for entry in &screen.entries {
                rows.push(format!("{}  {}", entry.key, entry.title.resolve(lang)));
                rows.push(format!("   {}", entry.summary.resolve(lang)));
            }
            "1-9 open a section · q close"
        }
        Some(section) => {
            for row in &screen.rows {
                let label = row.label.resolve(lang);
                let value = row
                    .value
                    .as_ref()
                    .map(|v| format!(": {}", v.resolve(lang)))
                    .unwrap_or_default();
                rows.push(match row.key {
                    Some(key) => format!("{key}  {label}{value}"),
                    None => format!("{label}{value}"),
                });
            }
            match section {
                // The section opens the profile itself; this is the line
                // under it once the profile closes over the list.
                Section::Profile => "p your profile · q back",
                Section::Notifications | Section::Backup => "q back",
                Section::Mailbox
                | Section::Appearance
                | Section::Chats
                | Section::Privacy
                | Section::Maps
                | Section::Advanced => "a row's key runs it · q back",
            }
        }
    };
    rows.push(String::new());
    rows.push(foot.to_string());
    DetailView {
        title: screen.title.resolve(lang),
        rows,
        fixed: Vec::new(),
    }
}

/// A letter or a digit while the settings are the detail view up: a
/// digit opens its section from the list, a row's key runs the row as
/// its palette command does. What leaves for another screen (the
/// blocked list, the map's drawing) closes the settings first.
fn settings_key<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    at: Option<settings::Section>,
    c: char,
) -> Option<Vec<UiEffect>> {
    let screen = settings::screen(model, settings::Target::Terminal, at);
    if at.is_none() {
        let entry = screen.entries.iter().find(|e| e.key == c)?;
        return Some(model.show_settings(Some(entry.section)));
    }
    let row = screen.rows.iter().find(|r| r.key == Some(c))?;
    let command = match row.action.as_ref()?.intent.clone() {
        // The public rooms' hub (1577): a shipped one or none at once,
        // another asked for in the palette.
        intent::Intent::RoomsHub(rooms::HubPick::Paste) => {
            model.palette = Some(Palette {
                ask: Some(contacts::Ask::RoomsHub),
                ..Palette::default()
            });
            return Some(Vec::new());
        }
        intent::Intent::RoomsHub(rooms::HubPick::Hub(hub)) => {
            return Some(rooms::choose(model, rooms::HubChoice::Hub(hub)));
        }
        intent::Intent::RoomsHub(rooms::HubPick::None) => {
            return Some(rooms::choose(model, rooms::HubChoice::None));
        }
        intent::Intent::RoomsHub(rooms::HubPick::More) => {
            rooms::more(model);
            return Some(Vec::new());
        }
        intent::Intent::Profile => PaletteCommand::MyCode,
        intent::Intent::Theme => PaletteCommand::Theme,
        intent::Intent::PhotoSize(_) => PaletteCommand::PhotoSize,
        intent::Intent::SendOnEnter => PaletteCommand::SendOnEnter,
        intent::Intent::ReadReceipts { .. } => PaletteCommand::ReadReceipts,
        intent::Intent::Help => PaletteCommand::Help,
        intent::Intent::ShowBlocked => {
            model.settings = None;
            PaletteCommand::Blocked
        }
        intent::Intent::MapDrawing => {
            model.settings = None;
            PaletteCommand::MapDrawing
        }
        _ => return None,
    };
    Some(run(model, command))
}

/// Why a message of ours shown delivered will not turn read (1233,
/// loquax-read-receipts.md, section 4): the reader's switch is off, or
/// the peer sends no read receipts (a client without them, or Loquax with
/// the switch off: the wire does not tell them apart). Never "unread",
/// nothing that looks like waiting for one.
fn no_read_words<B>(
    model: &UiModel<B>,
    conversation: &Conversation,
    message: &Message,
    who: &str,
) -> Option<String> {
    let state = shown_delivery(message.delivery?, model.home_client, model.reading.on);
    if !message.outbound || !matches!(state, DeliveryState::Arrived | DeliveryState::Delivered) {
        return None;
    }
    match (model.reading.on, conversation.flags.receipts) {
        (false, _) => Some("read receipts are off: you send none and see none".to_string()),
        (true, false) => Some(format!("{who} sends no read receipts")),
        (true, true) => None,
    }
}

/// The screen.
pub fn view<B: InputBuffer>(model: &UiModel<B>) -> View {
    // One turn's builds are shared and an unchanged turn's reused
    // (1490): the stamp covers every update (the revision moves with
    // each) and the fields a shell or a test sets directly.
    let stamp = view_stamp(model);
    if let Some((at, view)) = model.view_cache.borrow().as_ref() {
        if *at == stamp {
            return view.clone();
        }
    }
    let view = view_fresh(model);
    *model.view_cache.borrow_mut() = Some((stamp, view.clone()));
    view
}

/// The fingerprint of everything [`view_fresh`] reads that an update
/// does not stamp by itself: the model's revision covers every fold,
/// the rest covers direct mutation between folds.
fn view_stamp<B: InputBuffer>(model: &UiModel<B>) -> u64 {
    let mut hash = Fnv::new();
    hash.word(model.revision);
    hash.word(model.look_stamp);
    hash.word(model.screen.0 as u64);
    hash.word(model.screen.1 as u64);
    hash.word(model.focus as u64);
    hash.word(model.selected as u64);
    hash.word(model.message_cursor as u64);
    hash.word(model.scroll_top.map_or(u64::MAX, |at| at as u64));
    if let Some(anchor) = model.chat_anchor {
        hash.word(anchor.key);
        hash.word(anchor.offset as u64);
    }
    hash.word(model.help_scroll as u64);
    hash.word(u64::from(model.help));
    hash.word(
        model
            .detail
            .map_or(u64::MAX, |(c, m)| ((c as u64) << 24) ^ m as u64),
    );
    hash.word(model.detail_scroll as u64);
    hash.word(model.footer_hover.map_or(u64::MAX, |at| at as u64));
    hash.word(model.tick);
    hash.word(model.tier as u64);
    hash.word(model.theme as u64);
    hash.word(u64::from(model.mouse));
    hash.word(u64::from(model.ascii));
    hash.word(u64::from(model.show_blocked));
    hash.word(u64::from(model.show_archived));
    hash.word(u64::from(model.fetching));
    hash.word(u64::from(model.palette.is_some()));
    hash.word(u64::from(model.search.is_some()));
    hash.word(u64::from(model.map.is_some()));
    hash.word(model.toasts.len() as u64);
    hash.word(model.toasts_said);
    hash.word(model.lang as u64);
    hash.word(u64::from(model.debug_overlay));
    hash.word(u64::from(model.enhanced));
    hash.word(u64::from(model.send_on_enter));
    hash.word(model.sending.len() as u64);
    hash.word(model.map_named_until.unwrap_or(u64::MAX));
    // The panes' content, swept cheaply: the folds bump the revision,
    // this covers a model mutated directly (tests do).
    for conversation in &model.conversations {
        hash.word(conversation.revision());
        hash.word(conversation.messages.len() as u64);
        hash.word(conversation.unread as u64);
        hash.word(u64::from(conversation.pinned));
        if let Some(last) = conversation.messages.last() {
            hash.word(last.activity);
            hash.word(delivery_word(last.delivery));
        }
    }
    if let Some(open) = model.conversations.get(model.selected) {
        hash.word(model.laid_key(open, model.sizes().messages.0));
    }
    hash.bytes(model.compose.value().as_bytes());
    if let Some(query) = model.query() {
        hash.bytes(query.as_bytes());
    }
    if let Some(selection) = model.selection {
        hash.word(selection.anchor.0 as u64);
        hash.word(selection.anchor.1 as u64);
        hash.word(selection.head.0 as u64);
        hash.word(selection.head.1 as u64);
    }
    hash.done()
}

/// [`view`] built fresh.
fn view_fresh<B: InputBuffer>(model: &UiModel<B>) -> View {
    let sizes = model.sizes();
    let (conversations, search_cursor) = match (&model.search, model.order().is_empty()) {
        (Some(open), _) => {
            let (rows, cursor) = search_pane(model, open, sizes.list.0, sizes.list.1);
            (rows, (model.focus == Pane::Search).then_some(cursor))
        }
        (None, true) => {
            let empty = match (model.show_blocked, model.show_archived) {
                (true, _) => NO_BLOCKED,
                (false, true) => NO_ARCHIVED,
                (false, false) => no_conversations(model.lang),
            };
            (wrap(&[plain(empty)], sizes.list.0, 0), None)
        }
        (None, false) => (layout::list(model, sizes.list.0, sizes.list.1), None),
    };
    let wide = sizes.messages.0 >= WIDE_HEADER;
    let peer = model.conversations.get(model.selected).map(|c| c.peer);
    let messages_title = match peer {
        // A relay's chat says it is a group via a relay (1482).
        Some(peer)
            if !model.groups.contains_key(&peer)
                && model
                    .conversations
                    .get(model.selected)
                    .is_some_and(relayed::is_relayed) =>
        {
            strings::Text::Key(
                strings::StrId::RelayedGroup,
                vec![strings::Arg::Str(header_title(
                    &model.names,
                    &peer,
                    model.lang,
                ))],
            )
            .resolve(model.lang)
        }
        Some(peer) => header_title(&model.names, &peer, model.lang),
        None => "messages".to_string(),
    };
    let mailbox = mailbox_words(model);
    let fix = peer.and_then(|peer| model.positions.get(&peer));
    let position = fix.map(|fix| telemetry::header_words(fix, model.now.as_ref()));
    let stale = match (fix, model.now) {
        (Some(fix), Some(now)) => fix.stale(now.unix_secs),
        _ => false,
    };
    // Room for the right end: the pane less its corners, the title with its
    // focus mark and the spaces around both.
    let room = sizes
        .messages
        .0
        .saturating_sub(leviculum_ui::text::fit_width(&messages_title) + 6);
    let short = position.as_ref().and_then(|words| words.last().cloned());
    let messages_title_right = match (wide, peer) {
        // Wide: the name alone (1452); the address is the detail view's
        // and the profile's.
        (true, Some(_)) => String::new(),
        // Narrow: the position's age before the mailbox, so the reader
        // knows there is one, then the mailbox alone where both do not fit.
        _ => {
            let mut choices: Vec<String> = Vec::new();
            if let Some(short) = &short {
                choices.extend(mailbox.iter().map(|words| format!("{short} · {words}")));
                choices.push(short.clone());
            }
            choices.extend(mailbox.iter().cloned());
            choices
                .into_iter()
                .find(|words| words.chars().count() <= room)
                .unwrap_or_default()
        }
    };
    let mut position_len = match (&short, wide) {
        (Some(short), false) if messages_title_right.starts_with(short.as_str()) => {
            short.chars().count()
        }
        _ => 0,
    };
    let laid = model.laid();
    let texts = laid.texts.clone();
    let rows = model.message_rows();
    let top = model.top_of(laid.rows.len());
    let mut messages = Vec::with_capacity(sizes.messages.1);
    if wide {
        let width = sizes.messages.0;
        let words = mailbox.first().cloned().unwrap_or_default();
        // The peer's position on the left of the mailbox's row, the longest
        // that leaves the mailbox room; dimmed once it is stale.
        let left_room = width.saturating_sub(words.chars().count() + 4);
        let at = position
            .as_ref()
            .and_then(|all| all.iter().find(|w| w.chars().count() <= left_room))
            .cloned()
            .unwrap_or_default();
        position_len = at.chars().count();
        let mut row = plain(" ");
        let st = match stale {
            true => layout::painted(model.tokens().muted),
            false => RStyle::default(),
        };
        push(&mut row, &at, st);
        let used = row.cells.len();
        let pad = width.saturating_sub(used + words.chars().count() + 1);
        push(
            &mut row,
            &format!("{}{words}", " ".repeat(pad)),
            RStyle::default(),
        );
        messages.push(row);
        let mut rule = plain(" ");
        push(
            &mut rule,
            &"─".repeat(width.saturating_sub(2)),
            layout::painted(model.tokens().muted),
        );
        push(&mut rule, " ", RStyle::default());
        messages.push(rule);
    }
    // Our share's chip (1273): its words, and that a click stops it.
    if model.chip_row().is_some() {
        if let Some(share) = peer.and_then(|peer| model.share(&peer)) {
            let now = model.now.map_or(share.started_at, |now| now.unix_secs);
            let words = scene::share_words(share, now).resolve(strings::Lang::En);
            let chip = format!("⌖ {words}");
            let hint = format!(" · {SHARE_CLICK}");
            let mut row = plain(" ");
            push(&mut row, &chip, RStyle::default());
            // The hint where the pane has room for it after the words.
            if 1 + chip.chars().count() + hint.chars().count() < sizes.messages.0 {
                push(&mut row, &hint, layout::painted(model.tokens().muted));
            }
            messages.push(row);
        }
    }
    let shown = laid.rows.len().saturating_sub(top).min(rows);
    messages.extend((shown..rows).map(|_| RLine::default()));
    let range = model.selection.map(|s| hit::ordered(&s));
    for (at, row) in laid.rows.iter().enumerate().skip(top).take(shown) {
        let mut row = row.clone();
        // The selected text in inverse video, against whatever it was drawn
        // in: the selected block is reversed already.
        if let (Some((from, to)), Some(Some(text))) = (range, texts.get(at)) {
            for col in hit::selected_columns(text, at, from, to) {
                if let Some(cell) = row.cells.get_mut(col) {
                    cell.st.reverse = !cell.st.reverse;
                }
            }
        }
        messages.push(row);
    }
    let compose_title = match peer {
        Some(peer) => format!("to {}", model.names.label(&peer, model.lang)),
        None => "compose".to_string(),
    };
    let ctx = model.ctx();
    let recording = model.recording_bar().map(|bar| {
        let width = hit::screen(model).compose.width.saturating_sub(2);
        layout::recording_rows(
            &bar,
            usize::from(width),
            model.ascii,
            model.tokens(),
            keys::send_chord(ctx),
            model.tick,
        )
    });
    let palette = model.palette.as_ref().map(|p| PaletteView {
        query: p.query.value().to_string(),
        cursor: p.query.cursor(),
        rows: match (p.ask, p.drawings) {
            (Some(ask), _) => model.ask_rows(ask).iter().map(|row| plain(row)).collect(),
            (None, true) => drawing_rows(p, &model.map_setup),
            (None, false) => palette_rows(p, ctx),
        },
    });
    let searching = model.search.is_some();
    let map = model.map_view();
    // The way the map is drawn, a moment after it changed.
    let named = model
        .map_named_until
        .filter(|&until| model.tick < until)
        .and_then(|_| model.map_setup.drawings.get(model.map_setup.drawing))
        .map_or(String::new(), |d| format!("{} · ", d.name));
    // The selected pin's card (1302): a node nearby that is no contact
    // says so, and how to add it.
    let card = model
        .map
        .as_ref()
        .and_then(|state| state.selected)
        .filter(|peer| {
            map.as_ref()
                .is_some_and(|m| m.pins.iter().any(|p| p.peer == *peer && p.nearby))
        })
        .map_or(String::new(), |_| {
            format!(
                " · {} · {}",
                strings::StrId::NearbyCard.text(strings::Lang::En),
                strings::StrId::NearbyEnterAdds.text(strings::Lang::En)
            )
        });
    // The screen's own load says so in words (1527), as the phone's chip
    // does.
    let loading_word = match model.map_here {
        true => Some(MAP_LOADING_HERE.to_string()),
        false => model.map_loading.clone(),
    };
    let (messages_title, messages_title_right) = match &map {
        Some(shown) => (
            format!("{MAP_TITLE}{card}"),
            // No zoom number and no count on screen (1447, walk 1 finding
            // 22): the debug overlay alone shows them.
            match (&loading_word, model.debug_overlay) {
                (Some(word), true) => format!(
                    "{named}z{} · {} in view · {word}",
                    shown.viewport.zoom(),
                    shown.pins.len()
                ),
                (None, true) => format!(
                    "{named}z{} · {} in view",
                    shown.viewport.zoom(),
                    shown.pins.len()
                ),
                (Some(word), false) => format!("{named}{word}"),
                (None, false) => named.trim_end_matches(" · ").to_string(),
            },
        ),
        None => (messages_title, messages_title_right),
    };
    View {
        conversations_title: match (searching, model.address) {
            (true, _) => SEARCH_TITLE.to_string(),
            (false, _) if model.show_blocked => BLOCKED_TITLE.to_string(),
            (false, _) if model.show_archived => ARCHIVED_TITLE.to_string(),
            // The own name, or "Loquax" alone while there is none (1447,
            // walk 1 finding 22): never the address's hex.
            (false, Some(address)) => {
                let named = Some(model.profile.name.trim())
                    .filter(|n| !n.is_empty())
                    .or_else(|| model.names.shown(&address).map(str::trim))
                    .filter(|n| !n.is_empty());
                match named {
                    Some(name) => name.to_string(),
                    None => strings::StrId::AppName.text(model.lang).to_string(),
                }
            }
            (false, None) => strings::StrId::AppConnecting.text(model.lang).to_string(),
        },
        conversations_title_right: match (searching, model.show_blocked) {
            (true, _) => SEARCH_CLOSES.to_string(),
            (false, true) => BLOCKED_RETURNS.to_string(),
            (false, false) if model.show_archived => ARCHIVED_RETURNS.to_string(),
            (false, false) => String::new(),
        },
        list_hint: match searching {
            true => String::new(),
            false => SEARCH_HINT.to_string(),
        },
        search_cursor,
        conversations,
        messages_title,
        messages_title_right,
        position_len: match map {
            Some(_) => 0,
            None => position_len,
        },
        messages,
        compose_title,
        compose: model.compose.value().to_string(),
        compose_cursor: model.compose.cursor(),
        compose_hint: match &recording {
            Some(_) => format!("{} send · Esc cancel", keys::send_chord(ctx)),
            None => keys::compose_hint(ctx),
        },
        recording,
        focus: model.focus,
        footer: footer(model),
        footer_hover: model.footer_hover,
        toast: model.current_toast().cloned(),
        palette,
        help: model.help,
        help_scroll: model.help_scroll,
        detail: shown_detail(model),
        detail_scroll: model.detail_scroll,
        tier: model.tier,
        theme: model.theme,
        map_theme: model.map_theme,
        map_glyph_test: map.is_some() && model.map_glyph_test,
        time_zones: map.is_some() && model.time_zones,
        map,
    }
}

/// The palette's command rows: the matching names, the highlighted one bold
/// behind `▸`, each with the first chord this terminal sends for it.
fn palette_rows<B: InputBuffer>(p: &Palette<B>, ctx: Ctx) -> Vec<RLine> {
    p.matches()
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let shown = match keys::palette_key(e.name, ctx) {
                Some(key) => format!("{}  {key}", e.name),
                None => e.name.to_string(),
            };
            let mut line = RLine::default();
            match i == p.selected {
                true => push(&mut line, &format!("▸ {shown}"), bold()),
                false => push(&mut line, &format!("  {shown}"), RStyle::default()),
            }
            line
        })
        .collect()
}

/// The palette's rows while it lists the map's drawings: each name, the
/// one drawing now behind `●`, the highlighted one bold behind `▸`, and the
/// ones the shell does not expect to work here marked so (1093).
fn drawing_rows<B: InputBuffer>(p: &Palette<B>, setup: &map::MapSetup) -> Vec<RLine> {
    p.drawing_matches(&setup.drawings)
        .iter()
        .enumerate()
        .filter_map(|(row, &i)| setup.drawings.get(i).map(|d| (row, i, d)))
        .map(|(row, i, d)| {
            let now = match i == setup.drawing {
                true => "● ",
                false => "  ",
            };
            let shown = match d.expected {
                true => format!("{now}{}", d.name),
                false => format!("{now}{}  (may not show here)", d.name),
            };
            let mut line = RLine::default();
            match row == p.selected {
                true => push(&mut line, &format!("▸ {shown}"), bold()),
                false => push(&mut line, &format!("  {shown}"), RStyle::default()),
            }
            line
        })
        .collect()
}

/// The rows and the column the compose box is scrolled to, a box `width` x
/// `height` cells inside showing `text` with the cursor at `cursor`: as far
/// down and right as keeps the cursor in it. The cursor's line and column
/// come back with them.
pub fn compose_window(
    text: &str,
    cursor: usize,
    width: usize,
    height: usize,
) -> (usize, usize, usize, usize) {
    let (line, column) = line_and_column(text, cursor);
    let scroll = (column + 1).saturating_sub(width.max(1));
    let top = (line + 1).saturating_sub(height.max(1));
    (top, scroll, line, column)
}

/// The line and the column, in characters, of the `cursor`th character of
/// `text`.
pub fn line_and_column(text: &str, cursor: usize) -> (usize, usize) {
    let mut line = 0;
    let mut column = 0;
    for c in text.chars().take(cursor) {
        if c == '\n' {
            line += 1;
            column = 0;
        } else {
            column += 1;
        }
    }
    (line, column)
}

/// Where the palette is drawn over `screen`, `rows` matches under its query
/// line: 40 cells wide, centred, two rows from the top.
pub fn palette_rect(rows: usize, screen: Rect) -> Rect {
    let width = 40.min(screen.width);
    let height = (rows as u16 + 3).min(screen.height);
    Rect::new(
        screen.x + screen.width.saturating_sub(width) / 2,
        screen.y + 2,
        width,
        height,
    )
}

/// The widest the detail view is drawn: the 64 hex digits of an id and its
/// label on one row.
pub const DETAIL_MAX_WIDTH: u16 = 76;

/// Where a detail view is drawn over `screen`, centred, and its rows wrapped
/// to that width.
pub fn detail_layout(detail: &DetailView, screen: Rect) -> (Rect, Vec<String>) {
    let width = DETAIL_MAX_WIDTH.min(screen.width.saturating_sub(2)).max(4);
    let rows: Vec<String> = detail
        .rows
        .iter()
        .flat_map(|row| match row.is_empty() {
            true => vec![String::new()],
            false => toast_rows(row, usize::from(width - 2)),
        })
        .chain(
            detail
                .fixed
                .iter()
                .map(|row| leviculum_ui::text::truncate_to_cols(row, usize::from(width - 2))),
        )
        .collect();
    let height = (rows.len() as u16 + 2).min(screen.height.saturating_sub(1));
    let area = Rect::new(
        screen.x + screen.width.saturating_sub(width) / 2,
        screen.y + screen.height.saturating_sub(height + 1) / 2,
        width,
        height,
    );
    (area, rows)
}

/// `direct 12 s` or `via propagation node 40 s`: which leg a waiting send is on and
/// for how long, while its mark still says it is waiting. Gone the moment it
/// is handed on, so the label never sits beside a tick.
fn leg_label(message: &Message, state: DeliveryState, tick: u64) -> Option<String> {
    let (leg, since) = message.leg?;
    if !matches!(
        state,
        DeliveryState::Queued | DeliveryState::Stamping | DeliveryState::InFlight
    ) {
        return None;
    }
    let secs = tick.saturating_sub(since) / TICKS_PER_SECOND;
    Some(format!("{} {secs} s", leg_words(leg)))
}

/// Cut `lines` to rows of at most `width` cells, every row after a line's
/// first set in by `indent`, and the cells keeping their styles. A row the
/// indent alone would fill is not indented, so a narrow pane still shows
/// text.
pub fn wrap(lines: &[RLine], width: usize, indent: usize) -> Vec<RLine> {
    let width = width.max(1);
    let indent = if indent < width { indent } else { 0 };
    let mut rows = Vec::new();
    for line in lines {
        let mut cells = line.cells.as_slice();
        let mut first = true;
        loop {
            let pad = if first { 0 } else { indent };
            let room = width - pad;
            // At the last space that fits, which the break then swallows; a
            // word longer than the row is cut where the row ends.
            let (take, skip) = if cells.len() <= room {
                (cells.len(), 0)
            } else {
                match cells[..=room].iter().rposition(|cell| cell.ch == ' ') {
                    Some(space) if space > 0 => (space, 1),
                    _ => (room, 0),
                }
            };
            let mut row = RLine::default();
            push(&mut row, &" ".repeat(pad), RStyle::default());
            row.cells.extend_from_slice(&cells[..take]);
            rows.push(row);
            cells = &cells[take + skip..];
            first = false;
            if cells.is_empty() {
                break;
            }
        }
    }
    rows
}

/// The most rows a toast takes: enough for both legs of a failed auto send
/// at 80 columns, few enough to leave the messages pane readable.
pub const TOAST_MAX_ROWS: usize = 3;

/// `text` wrapped to `width` at word boundaries with [`wrap`], as a block
/// wraps a message, in at most
/// [`TOAST_MAX_ROWS`] rows: what does not fit is cut at the end of the last
/// row with a `…`.
pub fn toast_rows(text: &str, width: usize) -> Vec<String> {
    let as_text = |row: &RLine| row.cells.iter().map(|cell| cell.ch).collect::<String>();
    let mut rows: Vec<String> = wrap(&[plain(text)], width, 0).iter().map(as_text).collect();
    if rows.len() > TOAST_MAX_ROWS {
        let rest = rows.split_off(TOAST_MAX_ROWS - 1).join(" ");
        rows.push(leviculum_ui::text::truncate_to_cols(&rest, width));
    }
    rows
}

/// The footer: the palette's own two keys while it is open, else the rows
/// with a footer label that have a chord in the focused pane.
fn footer<B>(model: &UiModel<B>) -> Vec<FooterItem<FooterAct>> {
    if model.palette.is_some() {
        return vec![
            FooterItem::action("Enter", "run", FooterAct::PaletteRun),
            FooterItem::action("Esc", "close", FooterAct::PaletteClose),
        ];
    }
    keys::footer(keys::panes_of(model.focus), model.ctx(), FooterAct::Row)
}

/// The help overlay's text, every row of the keymap ([`keys::help_groups`]).
pub fn help_groups() -> Vec<HelpGroup> {
    keys::help_groups()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// `fields` as the model sends them: with the read-receipt mark (1230).
    /// The mark every send of a Loquax with receipts on carries: bit 0
    /// and, since 1307, bit 1.
    fn marked(mut fields: Vec<leviculum_lxmf::Field>) -> Vec<leviculum_lxmf::Field> {
        caps(groups::client_caps(true)).put_into(&mut fields);
        fields
    }

    fn caps(capabilities: u64) -> leviculum_lxmf::lev::Lev {
        leviculum_lxmf::lev::Lev {
            capabilities: Some(capabilities),
            ..leviculum_lxmf::lev::Lev::default()
        }
    }

    /// A minimal field for the model's tests: insert and backspace at the
    /// end, the edits these tests make, `Ctrl-U`, and a cursor a click
    /// places, which the next edit puts back at the end. The terminal's
    /// field is `tui_input::Input`.
    #[derive(Default, Clone, Debug)]
    pub(crate) struct Field(String, Option<usize>);

    impl InputBuffer for Field {
        fn value(&self) -> &str {
            &self.0
        }
        fn cursor(&self) -> usize {
            self.1.unwrap_or(self.0.chars().count())
        }
        fn edit(&mut self, key: &KeyEvent) -> bool {
            self.1 = None;
            match key.code {
                KeyCode::Char(c)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    self.0.push(c);
                    true
                }
                KeyCode::Backspace => {
                    self.0.pop();
                    true
                }
                KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                    self.0.clear();
                    true
                }
                _ => false,
            }
        }
        fn take(&mut self) -> String {
            self.1 = None;
            core::mem::take(&mut self.0)
        }
        fn newline(&mut self) {
            self.1 = None;
            self.0.push('\n');
        }
        fn set_cursor(&mut self, at: usize) {
            self.1 = Some(at.min(self.0.chars().count()));
        }
        fn set_value(&mut self, text: &str) {
            self.0 = text.to_string();
            self.1 = None;
        }
    }

    const PEER: [u8; 16] = [0xab; 16];
    const OTHER: [u8; 16] = [0xcd; 16];
    const ID: [u8; 32] = [0x11; 32];
    const ID2: [u8; 32] = [0x22; 32];

    fn key(code: KeyCode) -> UiEvent {
        UiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl_key(c: char) -> UiEvent {
        UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn typed(model: &mut UiModel<Field>, text: &str) {
        for c in text.chars() {
            update(model, key(KeyCode::Char(c)));
        }
    }

    fn engine(model: &mut UiModel<Field>, event: OutboxEvent) {
        update(model, UiEvent::Engine(event));
    }

    fn state(model: &mut UiModel<Field>, id: [u8; 32], state: MessageState) {
        engine(
            model,
            OutboxEvent::State {
                message_id: id,
                state,
            },
        );
    }

    fn last_delivery(model: &UiModel<Field>) -> Option<DeliveryState> {
        model.messages_of_selected().last().and_then(|m| m.delivery)
    }

    /// Compose a message to the first conversation and press Enter.
    fn sent(model: &mut UiModel<Field>, body: &str) -> Vec<UiEffect> {
        update(model, key(KeyCode::Tab));
        update(model, key(KeyCode::Tab));
        assert_eq!(model.focus, Pane::Compose);
        typed(model, body);
        update(model, key(KeyCode::Enter))
    }

    /// 1603: a model that is never ticked (the M9's) keeps
    /// [`TOAST_KEEP`] toasts, not one per message ever received, and a
    /// toast said at the cap still changes what the redraw marks hash.
    #[test]
    fn a_model_never_ticked_keeps_a_few_toasts_and_shows_the_newest() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        for n in 0..TOAST_KEEP {
            model.toast(ToastKind::Info, format!("toast {n}"));
        }
        let (view, clock) = (view_stamp(&model), model.clock_marks());
        model.toast(ToastKind::Info, String::from("the newest"));
        for n in 0..1000 {
            model.toast(ToastKind::Info, format!("more {n}"));
        }
        model.toast(ToastKind::Info, String::from("the last"));
        assert_eq!(model.toasts.len(), TOAST_KEEP);
        assert_eq!(
            model.current_toast().map(|t| t.text.as_str()),
            Some("the last")
        );
        assert_ne!(view_stamp(&model), view, "a toast at the cap is drawn");
        assert_ne!(model.clock_marks(), clock, "a toast at the cap is marked");
    }

    #[test]
    fn tab_and_shift_tab_cycle_the_three_panes() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        let mut seen = Vec::new();
        for _ in 0..3 {
            seen.push(model.focus);
            update(&mut model, key(KeyCode::Tab));
        }
        assert_eq!(seen, [Pane::Conversations, Pane::Messages, Pane::Compose]);
        assert_eq!(model.focus, Pane::Conversations);
        update(&mut model, key(KeyCode::BackTab));
        assert_eq!(model.focus, Pane::Compose);
    }

    #[test]
    fn compose_takes_plain_typing_and_q_does_not_quit_there() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Tab));
        typed(&mut model, "quit?");
        assert!(!model.quit);
        assert!(!model.help);
        assert_eq!(model.compose.value(), "quit?");
        update(&mut model, key(KeyCode::Esc));
        assert_eq!(model.focus, Pane::Messages);
        let effects = update(&mut model, key(KeyCode::Char('q')));
        assert!(model.quit);
        assert_eq!(effects, [UiEffect::Quit]);
    }

    #[test]
    fn ctrl_c_clears_compose_instead_of_quitting_and_ctrl_q_quits() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Tab));
        typed(&mut model, "draft");
        assert!(update(&mut model, ctrl_key('c')).is_empty());
        assert!(!model.quit);
        assert_eq!(model.compose.value(), "");
        assert_eq!(update(&mut model, ctrl_key('q')), [UiEffect::Quit]);
        assert!(model.quit);
    }

    #[test]
    fn colon_opens_the_palette_in_a_command_pane_and_types_in_compose() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(&mut model, key(KeyCode::Char(':')));
        assert!(model.palette.is_some());
        update(&mut model, key(KeyCode::Esc));
        update(&mut model, key(KeyCode::BackTab));
        update(&mut model, key(KeyCode::Char(':')));
        assert!(model.palette.is_none());
        assert_eq!(model.compose.value(), ":");
    }

    #[test]
    fn enter_sends_to_the_selected_conversation_and_clears_the_line() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        update(&mut model, key(KeyCode::Char('j')));
        assert_eq!(model.selected, 1);
        let effects = sent(&mut model, "hello");
        assert_eq!(
            effects,
            [UiEffect::Send {
                to: OTHER,
                body: "hello".to_string(),
                fields: marked(Vec::new()),
            }]
        );
        assert_eq!(model.compose.value(), "");
        assert_eq!(last_delivery(&model), Some(DeliveryState::Queued));
        // An empty line sends nothing.
        assert!(update(&mut model, key(KeyCode::Enter)).is_empty());
    }

    /// A message from `source` with this namespace (1230), and its body.
    fn with_lev(
        source: [u8; 16],
        id: [u8; 32],
        body: &str,
        lev: &leviculum_lxmf::lev::Lev,
    ) -> OutboxEvent {
        OutboxEvent::Received {
            sent_at: 0,
            message_id: id,
            source,
            title: Vec::new(),
            body: body.as_bytes().to_vec(),
            fields: lev.fields().to_vec(),
            verified: true,
        }
    }

    fn receipt(read_at: u32, ids: &[[u8; 32]]) -> leviculum_lxmf::lev::Lev {
        leviculum_lxmf::lev::Lev::mark_with(leviculum_lxmf::lev::Receipt {
            read_at,
            ids: ids.to_vec(),
        })
    }

    fn wall(model: &mut UiModel<Field>, unix_secs: i64) {
        update(
            model,
            UiEvent::Clock(WallTime {
                unix_secs,
                offset_secs: 0,
            }),
        );
    }

    /// A message from `source` with bit 2 in its mark, that came by
    /// propagation (1395): the received event and the engine's word after it.
    fn propagated(model: &mut UiModel<Field>, source: [u8; 16], id: [u8; 32], caps: u64) {
        engine(model, with_lev(source, id, "via the node", &caps_lev(caps)));
        engine(
            model,
            OutboxEvent::Propagated {
                message_id: id,
                source,
            },
        );
    }

    fn caps_lev(capabilities: u64) -> leviculum_lxmf::lev::Lev {
        caps(capabilities)
    }

    /// The propagation receipts among `effects`: to whom, and the ids.
    fn arrival_receipts(effects: &[UiEffect]) -> Vec<([u8; 16], Vec<[u8; 32]>)> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                UiEffect::Send { to, fields, .. } => leviculum_lxmf::lev::Lev::from_fields(fields)
                    .and_then(|lev| lev.propagation_receipt)
                    .map(|receipt| (*to, receipt.ids)),
                _ => None,
            })
            .collect()
    }

    /// 1395 and 1462, the rule: a message on this device that carries bit
    /// 2 is acknowledged with 0x07 after the batch window, alone, with the
    /// clock of its arrival, whatever the read-receipts switch says, and
    /// however it came, by propagation or direct; one without bit 2 is not;
    /// a second copy owes none.
    #[test]
    fn a_message_with_bit_2_is_acknowledged_however_it_came_and_no_other() {
        use leviculum_lxmf::lev::{CAP_GROUPS, CAP_PROPAGATION_RECEIPTS, CAP_READ_RECEIPTS};
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        wall(&mut model, RT);
        // Receipts off: the propagation receipt does not follow the switch.
        update(&mut model, UiEvent::ReadReceipts(false));
        let all = CAP_READ_RECEIPTS | CAP_GROUPS | CAP_PROPAGATION_RECEIPTS;
        propagated(&mut model, PEER, ID, all);
        propagated(&mut model, PEER, ID2, all);
        // Direct: the same mark, no Propagated word; its proof says only
        // that the identity has it, so it owes the device's word too.
        engine(
            &mut model,
            with_lev(PEER, [0x33; 32], "direct", &caps_lev(all)),
        );
        // Propagated without bit 2: a client that does not ask.
        propagated(
            &mut model,
            OTHER,
            [0x44; 32],
            CAP_READ_RECEIPTS | CAP_GROUPS,
        );
        assert_eq!(model.arrivals.owed(&PEER), Some(&[ID, ID2, [0x33; 32]][..]));
        assert_eq!(model.arrivals.owed(&OTHER), None);
        let early = update(&mut model, UiEvent::Elapsed(receipts::BATCH_MS - 1));
        assert!(
            arrival_receipts(&early).is_empty(),
            "inside the batch window"
        );
        let due = update(&mut model, UiEvent::Elapsed(receipts::BATCH_MS));
        assert_eq!(arrival_receipts(&due), [(PEER, vec![ID, ID2, [0x33; 32]])]);
        let lev = due
            .iter()
            .find_map(|effect| match effect {
                UiEffect::Send { fields, body, .. } if body.is_empty() => {
                    leviculum_lxmf::lev::Lev::from_fields(fields)
                }
                _ => None,
            })
            .expect("a receipt alone");
        assert_eq!(lev.capabilities, None, "alone, without the mark");
        assert_eq!(
            lev.propagation_receipt.map(|r| r.received_at),
            Some(RT as u32)
        );
        assert_eq!(model.sending.back(), Some(&(receipts::RECEIPT_SEND, 0)));
        // The same message again by propagation owes nothing more.
        propagated(&mut model, PEER, ID, all);
        assert_eq!(model.arrivals.owed(&PEER), None);
    }

    /// 1395, read wins: a message seen inside the batch window while read
    /// receipts go to its sender is named by the read receipt only.
    #[test]
    fn a_message_read_inside_the_window_gets_the_read_receipt_only() {
        use leviculum_lxmf::lev::{CAP_GROUPS, CAP_PROPAGATION_RECEIPTS, CAP_READ_RECEIPTS};
        let mut model = UiModel::<Field>::new(&[PEER]);
        wall(&mut model, RT);
        propagated(
            &mut model,
            PEER,
            ID,
            CAP_READ_RECEIPTS | CAP_GROUPS | CAP_PROPAGATION_RECEIPTS,
        );
        let key = model.conversations[0].messages[0].activity;
        update(&mut model, UiEvent::Front(true));
        update(
            &mut model,
            UiEvent::Viewport {
                peer: PEER,
                keys: vec![key],
            },
        );
        update(&mut model, UiEvent::Elapsed(receipts::SEEN_MS + 10));
        let mut effects = Vec::new();
        effects.extend(update(
            &mut model,
            UiEvent::Elapsed(receipts::BATCH_MS + 2_000),
        ));
        effects.extend(update(&mut model, UiEvent::Front(false)));
        effects.extend(update(
            &mut model,
            UiEvent::Elapsed(receipts::BATCH_MS + 3_000),
        ));
        assert!(arrival_receipts(&effects).is_empty(), "{effects:?}");
        assert!(effects.iter().any(|effect| matches!(
            effect,
            UiEffect::Send { fields, .. }
                if leviculum_lxmf::lev::Lev::from_fields(fields).is_some_and(|l| l.receipt.is_some())
        )));
    }

    /// 1395 and 1462, the mark: a 0x07 for our message is two grey ticks,
    /// the delivered rung, "on the device" in the Info, the terminal's
    /// detail and for the screen reader; read receipts off do not hide it;
    /// a later proof or 0x07 changes nothing; read still wins.
    #[test]
    fn a_device_receipt_is_two_grey_ticks_with_its_own_words() {
        use super::arrival::{Witness, ON_DEVICE};
        use super::strings::{Lang, StrId};
        let mut model = UiModel::<Field>::new(&[PEER]);
        wall(&mut model, RT);
        update(&mut model, UiEvent::ReadReceipts(false));
        sent(&mut model, "hi");
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        state(&mut model, ID, MessageState::AwaitingCollection);
        update(&mut model, UiEvent::SendDone(Ok(())));
        assert_eq!(last_delivery(&model), Some(DeliveryState::InMailbox));
        wall(&mut model, RT + 3_600);
        let alone = super::arrival::alone(leviculum_lxmf::lev::PropagationReceipt {
            received_at: (RT + 60) as u32,
            ids: vec![ID, ID2],
        });
        let effects = update(
            &mut model,
            UiEvent::Engine(with_lev(PEER, [0x33; 32], "", &alone)),
        );
        assert_eq!(last_delivery(&model), Some(DeliveryState::Delivered));
        assert_eq!(model.conversations[0].messages.len(), 1, "no bubble");
        assert!(effects.iter().any(|e| matches!(
            e,
            UiEffect::Log(line) if line.starts_with("ARRIVAL receipt dropped id=2222")
        )));
        let message = &model.conversations[0].messages[0];
        assert_eq!(
            super::arrival::witness(message).map(|(w, _)| w),
            Some(Witness::Receipt)
        );
        assert!(message.ledger.iter().any(|e| e.what == ON_DEVICE));
        let key = message.activity;
        let info = info::message_info(&model, PEER, key).expect("its info");
        let state_row = |lang| {
            info.rows
                .iter()
                .map(|row| row.resolve(lang))
                .find(|(label, _)| {
                    label == &super::strings::Text::key(StrId::InfoState).resolve(lang)
                })
                .map(|(_, value)| value)
        };
        let who = model.names.label(&PEER, model.lang);
        assert_eq!(state_row(Lang::En), Some(format!("On {who}'s device")));
        assert_eq!(
            state_row(Lang::De),
            Some(format!("Auf dem Gerät von {who}"))
        );
        let detail = detail_view(&model, 0, 0).expect("its detail").rows;
        assert!(
            detail
                .iter()
                .any(|row| row == &format!("state  on {who}'s device: it said so")),
            "{detail:?}"
        );
        let bubble = chat::bubbles(&model, 0)
            .into_iter()
            .last()
            .expect("the bubble");
        assert_eq!(bubble.mark, Some(chat::Mark::Delivered));
        assert_eq!(bubble.witness, Some(Witness::Receipt));
        assert_eq!(
            scene::mark_words(chat::Mark::Delivered, None, bubble.witness, None).resolve(Lang::En),
            "On the recipient's device"
        );
        // A late proof takes nothing back; read still moves it on.
        state(&mut model, ID, MessageState::Delivered);
        assert_eq!(last_delivery(&model), Some(DeliveryState::Delivered));
        assert_eq!(
            super::arrival::witness(&model.conversations[0].messages[0]).map(|(w, _)| w),
            Some(Witness::Receipt)
        );
        update(&mut model, UiEvent::ReadReceipts(true));
        engine(
            &mut model,
            with_lev(PEER, [0x34; 32], "", &receipt((RT + 90) as u32, &[ID])),
        );
        assert!(matches!(
            last_delivery(&model),
            Some(DeliveryState::Read { .. })
        ));
    }

    /// 1395, the node's word: collected is the arrived rung (1462: one
    /// tick, the recipient's mailbox may have collected it) with its own
    /// words; expired and evicted keep the one tick and say "not collected
    /// here", never lost; held and unknown change nothing; a proof before
    /// keeps the proof's words.
    #[test]
    fn what_the_node_answers_moves_the_marks_it_may() {
        use super::strings::{Lang, StrId};
        use leviculum_lxmf::lev_status::MessageStatus;
        let ids = [[0x61; 32], [0x62; 32], [0x63; 32], [0x64; 32], [0x65; 32]];
        let mut model = UiModel::<Field>::new(&[PEER]);
        wall(&mut model, RT);
        for (i, id) in ids.iter().enumerate() {
            match i {
                0 => {
                    sent(&mut model, "m0");
                }
                _ => {
                    typed(&mut model, &format!("m{i}"));
                    update(&mut model, key(KeyCode::Enter));
                }
            }
            engine(&mut model, OutboxEvent::Queued { message_id: *id });
            state(&mut model, *id, MessageState::AwaitingCollection);
            update(&mut model, UiEvent::SendDone(Ok(())));
        }
        state(&mut model, ids[4], MessageState::Delivered);
        model.pn_answered(&[
            (ids[0], MessageStatus::Collected),
            (ids[1], MessageStatus::Expired),
            (ids[2], MessageStatus::Held),
            (ids[3], MessageStatus::Unknown),
            (ids[4], MessageStatus::Collected),
        ]);
        let states: Vec<Option<DeliveryState>> = model.conversations[0]
            .messages
            .iter()
            .map(|m| m.delivery)
            .collect();
        assert_eq!(
            states,
            [
                Some(DeliveryState::Arrived),
                Some(DeliveryState::InMailbox),
                Some(DeliveryState::InMailbox),
                Some(DeliveryState::InMailbox),
                Some(DeliveryState::Arrived),
            ]
        );
        let words = |m: usize, lang| {
            let key = model.conversations[0].messages[m].activity;
            info::message_info(&model, PEER, key)
                .expect("info")
                .rows
                .iter()
                .map(|row| row.resolve(lang))
                .find(|(label, _)| {
                    label == &super::strings::Text::key(StrId::InfoState).resolve(lang)
                })
                .map(|(_, value)| value)
                .unwrap_or_default()
        };
        assert_eq!(
            words(0, Lang::En),
            "Collected from the propagation network at 08:00"
        );
        assert_eq!(
            words(1, Lang::En),
            "Not collected at the propagation node; perhaps delivered via another"
        );
        assert!(!words(1, Lang::En).to_lowercase().contains("lost"));
        assert_eq!(
            words(1, Lang::De),
            "Am Propagation-Knoten nicht abgeholt; vielleicht über einen anderen zugestellt"
        );
        assert_eq!(words(4, Lang::En), "Arrived");
        assert_eq!(words(4, Lang::De), "Angekommen");
        assert_eq!(
            model.conversations[0].messages[2].ledger.len(),
            model.conversations[0].messages[3].ledger.len()
        );
    }

    /// 1409: a device that is its own node asks the node that proved the
    /// upload, at the end of a round that synced, once a round, and folds
    /// the answer into the marks; the store learns what is open.
    #[test]
    fn a_device_asks_the_node_after_a_round_and_folds_its_word() {
        use leviculum_lxmf::lev_status::MessageStatus;
        const PN: [u8; 16] = [0x9c; 16];
        let id = [0x61; 32];
        let transient = [0x81; 32];
        let mut model = UiModel::<Field>::new(&[PEER]);
        wall(&mut model, RT);
        sent(&mut model, "via the node");
        engine(&mut model, OutboxEvent::Queued { message_id: id });
        engine(
            &mut model,
            OutboxEvent::Uploaded {
                message_id: id,
                transient_id: transient,
                node: Some(PN),
            },
        );
        state(&mut model, id, MessageState::AwaitingCollection);
        update(&mut model, UiEvent::SendDone(Ok(())));
        let kept = model
            .take_writes()
            .into_iter()
            .find_map(|w| match w {
                history::StoreWrite::PnAsks(state) => Some(state),
                _ => None,
            })
            .expect("the upload is kept");
        assert_eq!(kept.open.len(), 1);
        assert_eq!(kept.open[0].transient_id, transient);
        // A stock node: never asked.
        assert_eq!(
            update(&mut model, UiEvent::FetchDone(Ok(0))),
            [UiEffect::PnLine(
                "PN_STATUS_SKIPPED reason=no_capability".into()
            )]
        );
        engine(
            &mut model,
            OutboxEvent::NodeAnnounced {
                destination: PN,
                status: Some(1),
                name: None,
            },
        );
        // A round that failed found the node silent: nothing asked.
        let failed = update(&mut model, UiEvent::FetchDone(Err("no route".into())));
        assert!(
            !failed.iter().any(|e| matches!(e, UiEffect::AskPn { .. })),
            "{failed:?}"
        );
        let asked = update(&mut model, UiEvent::FetchDone(Ok(0)));
        assert_eq!(
            asked,
            [UiEffect::AskPn {
                node: PN,
                ids: vec![transient]
            }]
        );
        // `unknown` changes nothing and leaves it open.
        let line = update(
            &mut model,
            UiEvent::PnAnswered {
                node: PN,
                answer: Some(vec![(transient, MessageStatus::Unknown)]),
            },
        );
        assert_eq!(
            line,
            [UiEffect::PnLine("PN_STATUS_ASKED ids=1 answered=0".into())]
        );
        assert_eq!(
            model.conversations[0].messages[0].delivery,
            Some(DeliveryState::InMailbox)
        );
        // A link that failed leaves it open: the next round asks again.
        update(&mut model, UiEvent::FetchDone(Ok(0)));
        update(
            &mut model,
            UiEvent::PnAnswered {
                node: PN,
                answer: None,
            },
        );
        assert_eq!(model.pn_asker.open(), 1);
        assert!(matches!(
            update(&mut model, UiEvent::FetchDone(Ok(0))).as_slice(),
            [UiEffect::AskPn { .. }]
        ));
        let line = update(
            &mut model,
            UiEvent::PnAnswered {
                node: PN,
                answer: Some(vec![(transient, MessageStatus::Collected)]),
            },
        );
        assert_eq!(
            line,
            [UiEffect::PnLine("PN_STATUS_ASKED ids=1 answered=1".into())]
        );
        assert_eq!(
            model.conversations[0].messages[0].delivery,
            Some(DeliveryState::Arrived)
        );
        assert_eq!(
            update(&mut model, UiEvent::FetchDone(Ok(0))),
            [UiEffect::PnLine(
                "PN_STATUS_SKIPPED reason=no_open_ids".into()
            )]
        );
        let kept = model
            .take_writes()
            .into_iter()
            .find_map(|w| match w {
                history::StoreWrite::PnAsks(state) => Some(state),
                _ => None,
            })
            .expect("the answer closed it");
        assert!(kept.open.is_empty());
    }

    /// 1409: a home's client never asks; the home's pulled word moves its
    /// marks, and a proof or a receipt settles an upload on a device that
    /// asks.
    #[test]
    fn a_homes_client_takes_the_homes_word_and_asks_nothing() {
        use leviculum_lxmf::lev_status::MessageStatus;
        let id = [0x62; 32];
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.home_client = true;
        wall(&mut model, RT);
        sent(&mut model, "through the home");
        engine(&mut model, OutboxEvent::Queued { message_id: id });
        state(&mut model, id, MessageState::AwaitingCollection);
        update(&mut model, UiEvent::SendDone(Ok(())));
        engine(
            &mut model,
            OutboxEvent::Uploaded {
                message_id: id,
                transient_id: [0x82; 32],
                node: Some([0x9c; 16]),
            },
        );
        assert_eq!(model.pn_asker.open(), 0);
        let round = update(&mut model, UiEvent::FetchDone(Ok(0)));
        assert!(
            !round
                .iter()
                .any(|e| matches!(e, UiEffect::AskPn { .. } | UiEffect::PnLine(_))),
            "{round:?}"
        );
        engine(
            &mut model,
            OutboxEvent::HomePnStatus {
                answers: vec![(id, MessageStatus::Collected)],
            },
        );
        assert_eq!(
            model.conversations[0].messages[0].delivery,
            Some(DeliveryState::Arrived)
        );
        let mut own = UiModel::<Field>::new(&[PEER]);
        own.pn_asker.uploaded([0x9c; 16], [1; 32], [0x91; 32], 0);
        own.pn_asker.uploaded([0x9c; 16], [2; 32], [0x92; 32], 0);
        engine(
            &mut own,
            OutboxEvent::State {
                message_id: [1; 32],
                state: MessageState::Delivered,
            },
        );
        assert_eq!(own.pn_asker.open(), 1, "a proof settles it");
    }

    /// 1230: a receipt alone reads the message it names, at the reader's
    /// time kept within sent and came, and is no message: no bubble, no
    /// unread, no toast, the list order kept. An id the conversation does
    /// not hold is dropped with a log line. Read stays through a late
    /// proof and a second receipt, and the store learns both.
    #[test]
    fn a_receipt_alone_reads_our_message_and_is_no_message() {
        const T: i64 = 1_800_000_000;
        let mut model = UiModel::<Field>::new(&[PEER]);
        wall(&mut model, T);
        sent(&mut model, "hi");
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        state(&mut model, ID, MessageState::Delivered);
        update(&mut model, UiEvent::SendDone(Ok(())));
        let activity = model.conversations[0].activity;
        let _ = model.take_writes();
        wall(&mut model, T + 50);
        // The reader's clock runs ahead: it is kept at when the receipt came.
        let effects = update(
            &mut model,
            UiEvent::Engine(with_lev(
                PEER,
                [0x33; 32],
                "",
                &receipt((T + 100) as u32, &[ID, ID2]),
            )),
        );
        assert_eq!(
            last_delivery(&model),
            Some(DeliveryState::Read {
                at: (T + 50) as u32
            })
        );
        let logs: Vec<&UiEffect> = effects
            .iter()
            .filter(
                |e| matches!(e, UiEffect::Log(line) if line.starts_with("RECEIPT dropped id=2222")),
            )
            .collect();
        assert_eq!(logs.len(), 1, "{effects:?}");
        assert_eq!(effects.len(), 1, "{effects:?}");
        let conversation = &model.conversations[0];
        assert_eq!(conversation.messages.len(), 1, "no bubble");
        assert_eq!(conversation.unread, 0);
        assert_eq!(conversation.activity, activity, "the list does not move");
        assert!(conversation.flags.receipts);
        assert!(model.current_toast().is_none());
        assert_eq!(
            conversation.messages[0]
                .ledger
                .last()
                .map(|e| e.what.as_str()),
            Some("read by the peer")
        );
        let writes = model.take_writes();
        assert!(writes.iter().any(|w| matches!(w,
            StoreWrite::Message { message, .. }
                if message.delivery == Some(DeliveryState::Read { at: (T + 50) as u32 }))));
        assert!(writes.iter().any(|w| matches!(w,
            StoreWrite::Conversation { flags, .. } if flags.receipts)));
        // Monotone: a late proof and another receipt leave it.
        state(&mut model, ID, MessageState::Delivered);
        engine(
            &mut model,
            with_lev(PEER, [0x34; 32], "", &receipt(T as u32, &[ID])),
        );
        assert_eq!(
            last_delivery(&model),
            Some(DeliveryState::Read {
                at: (T + 50) as u32
            })
        );
        // A clock behind the send is kept at the send. Compose has the
        // focus still.
        typed(&mut model, "again");
        update(&mut model, key(KeyCode::Enter));
        engine(&mut model, OutboxEvent::Queued { message_id: ID2 });
        engine(
            &mut model,
            with_lev(PEER, [0x35; 32], "", &receipt(5, &[ID2])),
        );
        assert_eq!(
            last_delivery(&model),
            Some(DeliveryState::Read { at: T as u32 + 50 })
        );
    }

    /// 1231: `n` messages from `PEER` with the mark, ids `0x40 + i`, in
    /// its conversation, open and in front, at the wall time `T` and the
    /// monotonic clock 0; their keys.
    fn marked_chat(n: u8, mark: bool) -> (UiModel<Field>, Vec<u64>) {
        let mut model = UiModel::<Field>::new(&[PEER]);
        wall(&mut model, RT);
        let lev = match mark {
            true => leviculum_lxmf::lev::Lev::mark(),
            false => leviculum_lxmf::lev::Lev::default(),
        };
        for i in 0..n {
            let event = match mark {
                true => with_lev(PEER, [0x40 + i; 32], &format!("line {i}"), &lev),
                false => OutboxEvent::Received {
                    sent_at: 0,
                    message_id: [0x40 + i; 32],
                    source: PEER,
                    title: Vec::new(),
                    body: format!("line {i}").into_bytes(),
                    fields: Vec::new(),
                    verified: true,
                },
            };
            engine(&mut model, event);
        }
        update(&mut model, UiEvent::Open { peer: PEER });
        update(&mut model, UiEvent::Front(true));
        let keys = model.conversations[0]
            .messages
            .iter()
            .map(|m| m.activity)
            .collect();
        (model, keys)
    }

    const RT: i64 = 1_800_000_000;

    fn ms_at(model: &mut UiModel<Field>, ms: u64) -> Vec<UiEffect> {
        update(model, UiEvent::Elapsed(ms))
    }

    /// The receipts among `effects`: to whom, the body, the ids.
    fn receipts_in(effects: &[UiEffect]) -> Vec<([u8; 16], String, Vec<[u8; 32]>)> {
        effects
            .iter()
            .filter_map(|e| match e {
                UiEffect::Send { to, body, fields } => {
                    leviculum_lxmf::lev::Lev::from_fields(fields)
                        .and_then(|lev| lev.receipt)
                        .map(|r| (*to, body.clone(), r.ids))
                }
                _ => None,
            })
            .collect()
    }

    /// 1231: three bubbles on screen for a second are seen, and five
    /// seconds after the last one one receipt goes with all three ids,
    /// alone: no text, the mark beside the receipt, the reader's clock at
    /// the first seen, no bubble. Not a millisecond early, and never twice.
    #[test]
    fn a_viewport_of_three_bubbles_yields_one_receipt_with_three_ids() {
        let (mut model, keys) = marked_chat(3, true);
        update(&mut model, UiEvent::Viewport { peer: PEER, keys });
        ms_at(&mut model, 999);
        assert!(model.conversations[0].messages[0]
            .ledger
            .iter()
            .all(|e| e.what != receipts::SEEN));
        assert!(ms_at(&mut model, 1_000).is_empty(), "seen, not sent yet");
        assert!(model.conversations[0]
            .messages
            .iter()
            .all(|m| m.ledger.iter().any(|e| e.what == receipts::SEEN)));
        assert!(model.take_writes().iter().any(|w| matches!(w,
            StoreWrite::Message { message, .. }
                if message.ledger.iter().any(|e| e.what == receipts::SEEN))));
        assert!(receipts_in(&ms_at(&mut model, 5_999)).is_empty());
        let effects = ms_at(&mut model, 6_000);
        let ids = vec![[0x40; 32], [0x41; 32], [0x42; 32]];
        assert_eq!(receipts_in(&effects), [(PEER, String::new(), ids.clone())]);
        let [UiEffect::Send { fields, .. }] = effects.as_slice() else {
            panic!("one send: {effects:?}");
        };
        let lev = leviculum_lxmf::lev::Lev::from_fields(fields).expect("lev");
        assert!(lev.supports_receipts(), "the mark rides with it");
        assert_eq!(lev.receipt.map(|r| r.read_at), Some(RT as u32));
        assert_eq!(model.conversations[0].messages.len(), 3, "no bubble");
        assert_eq!(model.sending, [(receipts::RECEIPT_SEND, 0)]);
        // The runner's end and its router id touch no message of ours, and
        // nothing is said on screen.
        let toasts = model.toasts.clone();
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        assert!(update(&mut model, UiEvent::SendDone(Ok(()))).is_empty());
        assert!(model.sending.is_empty());
        assert_eq!(model.toasts, toasts);
        assert!(model.conversations[0]
            .messages
            .iter()
            .all(|m| m.id != Some(ID)));
        // Seen once: the same bubbles again owe nothing.
        let keys = model.conversations[0]
            .messages
            .iter()
            .map(|m| m.activity)
            .collect();
        update(&mut model, UiEvent::Viewport { peer: PEER, keys });
        for ms in [7_000, 9_000, 20_000] {
            assert!(receipts_in(&ms_at(&mut model, ms)).is_empty());
        }
    }

    /// 1231: a bubble that flashed by is not seen: the front lost before
    /// the second, or the viewport moved on.
    #[test]
    fn a_bubble_on_screen_less_than_a_second_is_not_seen() {
        let (mut model, keys) = marked_chat(2, true);
        update(
            &mut model,
            UiEvent::Viewport {
                peer: PEER,
                keys: keys.clone(),
            },
        );
        ms_at(&mut model, 500);
        update(&mut model, UiEvent::Front(false));
        ms_at(&mut model, 1_500);
        update(&mut model, UiEvent::Front(true));
        ms_at(&mut model, 2_000);
        update(
            &mut model,
            UiEvent::Viewport {
                peer: PEER,
                keys: vec![keys[1]],
            },
        );
        // The second bubble stayed on screen since the front came back,
        // the first left after half a second: only the second is seen.
        ms_at(&mut model, 2_499);
        assert!(model.reading.owed(&PEER).is_none());
        ms_at(&mut model, 2_500);
        assert_eq!(model.reading.owed(&PEER), Some(&[[0x41; 32]][..]));
        ms_at(&mut model, 9_000);
        assert!(!model.conversations[0].messages[0]
            .ledger
            .iter()
            .any(|e| e.what == receipts::SEEN));
    }

    /// 1231: a chat left two seconds after its bubbles came on screen sends
    /// the receipt at once; with a draft in it, the receipt waits up to a
    /// minute after the last bubble seen for the reply.
    #[test]
    fn a_chat_left_after_two_seconds_sends_the_receipt() {
        let (mut model, keys) = marked_chat(2, true);
        update(
            &mut model,
            UiEvent::Viewport {
                peer: PEER,
                keys: keys.clone(),
            },
        );
        ms_at(&mut model, 1_000);
        ms_at(&mut model, 2_000);
        let effects = update(&mut model, UiEvent::Close);
        assert_eq!(
            receipts_in(&effects),
            [(PEER, String::new(), vec![[0x40; 32], [0x41; 32]])]
        );

        let (mut model, keys) = marked_chat(1, true);
        update(&mut model, UiEvent::Viewport { peer: PEER, keys });
        ms_at(&mut model, 1_000);
        update(
            &mut model,
            UiEvent::Draft {
                peer: PEER,
                text: "on my".to_string(),
                reply: None,
            },
        );
        assert!(receipts_in(&update(&mut model, UiEvent::Close)).is_empty());
        assert!(receipts_in(&ms_at(&mut model, 60_999)).is_empty());
        assert_eq!(receipts_in(&ms_at(&mut model, 61_000)).len(), 1);

        // The app going behind is leaving too.
        let (mut model, keys) = marked_chat(1, true);
        update(&mut model, UiEvent::Viewport { peer: PEER, keys });
        ms_at(&mut model, 1_200);
        assert_eq!(
            receipts_in(&update(&mut model, UiEvent::Front(false))).len(),
            1
        );
    }

    /// 1231: a peer whose messages carry no mark gets no receipt, however
    /// long its bubbles stay; the positive control, the same with the
    /// mark, gets one. The switch off sends none, and no mark either.
    #[test]
    fn a_peer_without_the_mark_gets_nothing() {
        for mark in [false, true] {
            let (mut model, keys) = marked_chat(2, mark);
            assert_eq!(model.conversations[0].flags.receipts, mark);
            update(&mut model, UiEvent::Viewport { peer: PEER, keys });
            let mut sent = Vec::new();
            for ms in (0..=12_000).step_by(500) {
                sent.extend(receipts_in(&ms_at(&mut model, ms)));
            }
            sent.extend(receipts_in(&update(&mut model, UiEvent::Close)));
            assert_eq!(sent.len(), usize::from(mark), "mark={mark}: {sent:?}");
            let seen = model.conversations[0].messages[0]
                .ledger
                .iter()
                .any(|e| e.what == receipts::SEEN);
            assert_eq!(seen, mark, "only what owes a receipt is marked seen");
        }

        let (mut model, keys) = marked_chat(2, true);
        model.reading.on = false;
        update(&mut model, UiEvent::Viewport { peer: PEER, keys });
        for ms in (0..=12_000).step_by(500) {
            assert!(receipts_in(&ms_at(&mut model, ms)).is_empty());
        }
        let effects = update(
            &mut model,
            UiEvent::Write {
                peer: PEER,
                body: "hi".to_string(),
            },
        );
        assert_eq!(
            effects,
            [UiEffect::Send {
                to: PEER,
                body: "hi".to_string(),
                fields: caps(
                    leviculum_lxmf::lev::CAP_GROUPS | leviculum_lxmf::lev::CAP_PROPAGATION_RECEIPTS
                )
                .fields()
                .to_vec(),
            }],
            "the switch off sends no receipt bit, the groups bit and bit 2 still (1307, 1395)"
        );
    }

    /// 1322: messages from `PEER`, ids `0x40 + i`, each with the mark or
    /// without it as `marks` says, the chat open and in front, all of
    /// them on screen; the ids every receipt sent named, the chat left at
    /// the end.
    fn named_by_receipts(marks: &[bool]) -> Vec<[u8; 32]> {
        let mut model = UiModel::<Field>::new(&[PEER]);
        wall(&mut model, RT);
        for (i, &mark) in (0u8..).zip(marks) {
            let id = [0x40 + i; 32];
            let event = match mark {
                true => with_lev(PEER, id, "loquax", &leviculum_lxmf::lev::Lev::mark()),
                false => received_from(PEER, id, "stock"),
            };
            engine(&mut model, event);
        }
        update(&mut model, UiEvent::Open { peer: PEER });
        update(&mut model, UiEvent::Front(true));
        let keys = model.conversations[0]
            .messages
            .iter()
            .map(|m| m.activity)
            .collect();
        update(&mut model, UiEvent::Viewport { peer: PEER, keys });
        let mut named = Vec::new();
        for ms in (0..=12_000).step_by(500) {
            named.extend(receipts_in(&ms_at(&mut model, ms)));
        }
        named.extend(receipts_in(&update(&mut model, UiEvent::Close)));
        named.into_iter().flat_map(|(_, _, ids)| ids).collect()
    }

    /// 1322: a receipt names only the messages that asked for one. A stock
    /// client's plain reply before a marked message stays unnamed though
    /// the marked one made the peer take receipts (periculum 1235 saw
    /// both named, `plain_named=yes`). The positive control, both marked,
    /// names both. The mirror, a marked message and a plain one after it,
    /// names none: the latest says the peer takes no receipts. A marked
    /// one after that names the two marked and still not the plain.
    #[test]
    fn a_receipt_names_only_the_messages_that_carried_the_mark() {
        assert_eq!(named_by_receipts(&[false, true]), [[0x41; 32]]);
        assert_eq!(named_by_receipts(&[true, true]), [[0x40; 32], [0x41; 32]]);
        assert_eq!(named_by_receipts(&[true, false]), Vec::<[u8; 32]>::new());
        assert_eq!(
            named_by_receipts(&[true, false, true]),
            [[0x40; 32], [0x42; 32]]
        );
    }

    /// 1233: the switch round trip. The palette's command turns it off
    /// and on, each change asks the config to keep it, setting it to what
    /// it is asks nothing; a message written while off carries no mark,
    /// one written after it is on again does.
    #[test]
    fn the_switch_round_trips_and_a_message_sent_while_off_carries_no_receipt_bit() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        assert!(model.reading.on, "on by default");
        let write = |model: &mut UiModel<Field>| {
            update(
                model,
                UiEvent::Write {
                    peer: PEER,
                    body: "hi".to_string(),
                },
            )
        };
        assert_eq!(
            update(&mut model, UiEvent::Run(PaletteCommand::ReadReceipts)),
            [UiEffect::Remember {
                key: "read_receipts",
                value: "off"
            }]
        );
        assert!(!model.reading.on);
        assert!(model.current_toast().is_some(), "the palette says it");
        assert!(update(&mut model, UiEvent::ReadReceipts(false)).is_empty());
        assert_eq!(
            write(&mut model),
            [UiEffect::Send {
                to: PEER,
                body: "hi".to_string(),
                fields: caps(
                    leviculum_lxmf::lev::CAP_GROUPS | leviculum_lxmf::lev::CAP_PROPAGATION_RECEIPTS
                )
                .fields()
                .to_vec(),
            }],
            "off: no receipt bit, the groups bit and bit 2 still (1307, 1395)"
        );
        assert_eq!(
            update(&mut model, UiEvent::ReadReceipts(true)),
            [UiEffect::Remember {
                key: "read_receipts",
                value: "on"
            }]
        );
        let effects = write(&mut model);
        assert_eq!(effects.len(), 1, "{effects:?}");
        let UiEffect::Send { fields, .. } = &effects[0] else {
            panic!("{effects:?}");
        };
        assert_eq!(fields, &marked(Vec::new()), "on again: the mark");
    }

    /// Our message to PEER, proven delivered, the peer's mark known.
    fn delivered_to_a_marked_peer() -> UiModel<Field> {
        const T: i64 = 1_800_000_000;
        let mut model = UiModel::<Field>::new(&[PEER]);
        wall(&mut model, T);
        engine(
            &mut model,
            with_lev(PEER, [0x30; 32], "hello", &leviculum_lxmf::lev::Lev::mark()),
        );
        sent(&mut model, "hi");
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        state(&mut model, ID, MessageState::Delivered);
        update(&mut model, UiEvent::SendDone(Ok(())));
        assert!(model.conversations[0].flags.receipts);
        // A proof is one tick (1462).
        assert_eq!(last_delivery(&model), Some(DeliveryState::Arrived));
        model
    }

    /// The mark our last bubble shows, and its detail view's rows.
    fn shown_last(model: &UiModel<Field>) -> (Option<chat::Mark>, Vec<String>) {
        let bubbles = chat::bubbles(model, 0);
        let last = bubbles.iter().rfind(|b| b.ours).expect("our bubble");
        let m = model.conversations[0].messages.len() - 1;
        let rows = detail_view(model, 0, m).expect("a detail view").rows;
        (last.mark, rows)
    }

    /// 1293: a leg the home node gave up is a ledger line in the detail
    /// view, and the bubble keeps its state, since the message is tried
    /// again; a message of another id hears nothing.
    #[test]
    fn a_stalled_leg_is_said_in_the_ledger_and_keeps_the_state() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        wall(&mut model, 1_800_000_000);
        sent(&mut model, "hi");
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        state(&mut model, ID, MessageState::Sent);
        update(&mut model, UiEvent::SendDone(Ok(())));
        let before = last_delivery(&model);
        engine(
            &mut model,
            OutboxEvent::HomeStalled {
                message_id: [0x77; 32],
            },
        );
        let (_, rows) = shown_last(&model);
        assert!(!rows.iter().any(|r| r.contains(STALLED_NOTE)), "{rows:?}");
        engine(&mut model, OutboxEvent::HomeStalled { message_id: ID });
        assert_eq!(last_delivery(&model), before);
        let (_, rows) = shown_last(&model);
        assert!(rows.iter().any(|r| r.contains(STALLED_NOTE)), "{rows:?}");
    }

    /// 1233, reciprocity: with the switch off a receipt received changes
    /// nothing shown and is not kept, and the detail view says the
    /// switch is off; a message read before is shown delivered while off,
    /// its state kept, and read again once the switch is on.
    #[test]
    fn a_receipt_received_while_off_does_not_change_the_shown_state() {
        let mut model = delivered_to_a_marked_peer();
        update(&mut model, UiEvent::ReadReceipts(false));
        let effects = update(
            &mut model,
            UiEvent::Engine(with_lev(
                PEER,
                [0x33; 32],
                "",
                &receipt(1_800_000_010, &[ID]),
            )),
        );
        assert!(
            effects.iter().any(
                |e| matches!(e, UiEffect::Log(line) if line.starts_with("RECEIPT ignored ids=1"))
            ),
            "{effects:?}"
        );
        assert_eq!(last_delivery(&model), Some(DeliveryState::Arrived));
        let (mark, rows) = shown_last(&model);
        assert_eq!(mark, Some(chat::Mark::Arrived));
        assert!(
            rows.iter().any(|r| r.starts_with("read receipts are off")),
            "{rows:?}"
        );
        // On again: the ignored receipt was not kept; the next one reads.
        update(&mut model, UiEvent::ReadReceipts(true));
        assert_eq!(last_delivery(&model), Some(DeliveryState::Arrived));
        engine(
            &mut model,
            with_lev(PEER, [0x34; 32], "", &receipt(1_800_000_020, &[ID])),
        );
        assert!(matches!(
            last_delivery(&model),
            Some(DeliveryState::Read { .. })
        ));
        let (mark, rows) = shown_last(&model);
        assert_eq!(mark, Some(chat::Mark::Read));
        assert!(
            rows.iter().any(|r| r.starts_with("state  read")),
            "{rows:?}"
        );
        // Off: shown delivered, the state kept; on: read again.
        update(&mut model, UiEvent::ReadReceipts(false));
        assert!(matches!(
            last_delivery(&model),
            Some(DeliveryState::Read { .. })
        ));
        let (mark, rows) = shown_last(&model);
        assert_eq!(mark, Some(chat::Mark::Delivered));
        assert!(rows.iter().any(|r| r.starts_with("state  on ")), "{rows:?}");
        assert!(chat::list_rows(&model)[0].read_at.is_none());
        update(&mut model, UiEvent::ReadReceipts(true));
        assert_eq!(shown_last(&model).0, Some(chat::Mark::Read));
        assert!(chat::list_rows(&model)[0].read_at.is_some());
    }

    /// 1233: a peer that sends no read receipts keeps ours at two grey
    /// ticks, and the detail view says so by name; with the mark it says
    /// nothing of the kind.
    #[test]
    fn a_peer_without_receipts_is_named_in_the_detail_view() {
        let mut model = delivered_to_a_marked_peer();
        let (mark, rows) = shown_last(&model);
        assert_eq!(mark, Some(chat::Mark::Arrived));
        assert!(
            !rows.iter().any(|r| r.contains("read receipts")),
            "{rows:?}"
        );
        // The peer's next message comes without the mark.
        engine(
            &mut model,
            OutboxEvent::Received {
                sent_at: 0,
                message_id: [0x31; 32],
                source: PEER,
                title: Vec::new(),
                body: b"plain".to_vec(),
                fields: Vec::new(),
                verified: true,
            },
        );
        assert!(!model.conversations[0].flags.receipts);
        let m = model.conversations[0]
            .messages
            .iter()
            .rposition(|m| m.outbound)
            .expect("ours");
        let rows = detail_view(&model, 0, m).expect("a detail view").rows;
        let who = model.names.label(&PEER, model.lang);
        assert!(
            rows.contains(&format!("{who} sends no read receipts")),
            "{rows:?}"
        );
    }

    /// 1231: a reply sent while the receipt waits carries it in the same
    /// LXMF message, its text beside it, and nothing goes alone after.
    #[test]
    fn the_receipt_rides_on_the_reply() {
        let (mut model, keys) = marked_chat(2, true);
        update(&mut model, UiEvent::Viewport { peer: PEER, keys });
        ms_at(&mut model, 1_000);
        ms_at(&mut model, 3_000);
        let effects = update(
            &mut model,
            UiEvent::Write {
                peer: PEER,
                body: "on my way".to_string(),
            },
        );
        assert_eq!(
            receipts_in(&effects),
            [(PEER, "on my way".to_string(), vec![[0x40; 32], [0x41; 32]])]
        );
        assert_eq!(effects.len(), 1, "one LXMF message: {effects:?}");
        let UiEffect::Send { fields, .. } = &effects[0] else {
            panic!("{effects:?}");
        };
        assert!(leviculum_lxmf::lev::Lev::from_fields(fields)
            .is_some_and(|lev| lev.supports_receipts()));
        assert_eq!(model.sending.len(), 1);
        assert_ne!(
            model.sending[0].0,
            receipts::RECEIPT_SEND,
            "the reply's bubble"
        );
        for ms in [8_000, 70_000] {
            assert!(receipts_in(&ms_at(&mut model, ms)).is_empty());
        }
        assert!(receipts_in(&update(&mut model, UiEvent::Close)).is_empty());
    }

    /// 1231: the terminal's viewport is the open chat's bubbles on screen,
    /// and nothing while the help covers them.
    #[test]
    fn the_terminal_viewport_names_the_bubbles_on_screen() {
        let (mut model, keys) = marked_chat(3, true);
        assert_eq!(hit::viewport(&model), Some((PEER, keys.clone())));
        update(
            &mut model,
            UiEvent::Resize {
                width: 80,
                height: 9,
            },
        );
        let shown = hit::viewport(&model).expect("a chat").1;
        assert!(!shown.is_empty() && shown.len() < 3, "{shown:?}");
        assert_eq!(shown.last(), keys.last(), "the newest at the bottom");
        model.help = true;
        assert_eq!(hit::viewport(&model), Some((PEER, Vec::new())));
    }

    /// 1230: a receipt on a text is a message as any, and reads ours too.
    #[test]
    fn a_receipt_on_a_text_shows_the_text_and_reads_ours() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        sent(&mut model, "hi");
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        engine(
            &mut model,
            with_lev(PEER, ID2, "hi back", &receipt(9, &[ID])),
        );
        let messages = &model.conversations[0].messages;
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].body, "hi back");
        assert!(matches!(
            messages[0].delivery,
            Some(DeliveryState::Read { .. })
        ));
    }

    /// 1230: the peer's latest message decides whether it takes receipts;
    /// a stock message, without the namespace, says no, and a receipt says
    /// yes whatever its capabilities.
    #[test]
    fn the_latest_message_decides_whether_the_peer_takes_receipts() {
        use leviculum_lxmf::lev::Lev;
        let mut model = UiModel::<Field>::new(&[PEER]);
        let receipts = |model: &UiModel<Field>| model.conversations[0].flags.receipts;
        engine(&mut model, received_from(PEER, [1; 32], "stock"));
        assert!(!receipts(&model), "a stock message leaves it off");
        engine(&mut model, with_lev(PEER, [2; 32], "loquax", &Lev::mark()));
        assert!(receipts(&model));
        engine(&mut model, received_from(PEER, [3; 32], "stock again"));
        assert!(!receipts(&model), "the latest message decides");
        let off = Lev {
            capabilities: Some(0),
            ..Lev::default()
        };
        engine(&mut model, with_lev(PEER, [4; 32], "turned off", &off));
        assert!(!receipts(&model));
        let bare = Lev {
            capabilities: None,
            receipt: Some(leviculum_lxmf::lev::Receipt {
                read_at: 1,
                ids: Vec::new(),
            }),
            ..Lev::default()
        };
        engine(&mut model, with_lev(PEER, [5; 32], "", &bare));
        assert!(receipts(&model), "a receipt proves it");
        assert_eq!(
            model.conversations[0].messages.len(),
            4,
            "the receipt is none"
        );
    }

    /// 1230: only the peer a message went to can say it read it; a receipt
    /// from anyone else is dropped, and makes no conversation.
    #[test]
    fn a_receipt_reads_only_what_went_to_its_sender() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        update(&mut model, key(KeyCode::Char('j')));
        sent(&mut model, "for other");
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        state(&mut model, ID, MessageState::Delivered);
        let effects = update(
            &mut model,
            UiEvent::Engine(with_lev(PEER, [0x44; 32], "", &receipt(9, &[ID]))),
        );
        assert!(
            matches!(effects.as_slice(), [UiEffect::Log(line)] if line.contains("RECEIPT dropped"))
        );
        assert_eq!(
            model.conversations[1].messages[0].delivery,
            Some(DeliveryState::Arrived)
        );
        let stranger = [0x99; 16];
        let effects = update(
            &mut model,
            UiEvent::Engine(with_lev(stranger, [0x45; 32], "", &receipt(9, &[ID]))),
        );
        assert!(matches!(effects.as_slice(), [UiEffect::Log(_)]));
        assert!(model.conversations.iter().all(|c| c.peer != stranger));
        assert_eq!(
            model.conversations[1].messages[0].delivery,
            Some(DeliveryState::Arrived)
        );
    }

    /// The seen marks among a fold's marks (1234): their time and ids.
    fn seen_marks(model: &mut UiModel<Field>) -> Vec<(u32, Vec<[u8; 32]>)> {
        use crate::home::wire::MarkFlag;
        model
            .take_marks()
            .into_iter()
            .filter_map(|mark| match mark.flag {
                MarkFlag::Seen { at } => Some((at, mark.ids)),
                MarkFlag::Read | MarkFlag::Deleted | MarkFlag::Arrived { .. } => None,
            })
            .collect()
    }

    /// 1234: a client of a home node holds no identity, so it signs no
    /// receipt: what is seen gathers by the same rules and goes to the
    /// home node as one seen mark, with the reader's clock at the first
    /// seen, five seconds after the last bubble. Nothing rides on a reply,
    /// and a draft holds nothing up.
    #[test]
    fn a_home_client_hands_the_mailbox_one_seen_mark_and_sends_no_receipt() {
        let (mut model, keys) = marked_chat(3, true);
        model.home_client = true;
        let _ = model.take_marks();
        typed(&mut model, "a draft");
        update(&mut model, UiEvent::Viewport { peer: PEER, keys });
        ms_at(&mut model, 1_000);
        assert!(seen_marks(&mut model).is_empty(), "seen, not said yet");
        assert!(ms_at(&mut model, 5_999).is_empty());
        assert!(seen_marks(&mut model).is_empty());
        let effects = ms_at(&mut model, 6_000);
        assert!(receipts_in(&effects).is_empty(), "{effects:?}");
        assert!(model.sending.is_empty());
        assert_eq!(
            seen_marks(&mut model),
            [(RT as u32, vec![[0x40; 32], [0x41; 32], [0x42; 32]])]
        );
        // Seen once: the same bubbles again say nothing more.
        let keys = model.conversations[0]
            .messages
            .iter()
            .map(|m| m.activity)
            .collect();
        update(&mut model, UiEvent::Viewport { peer: PEER, keys });
        ms_at(&mut model, 20_000);
        assert!(seen_marks(&mut model).is_empty());
        // The reply goes without a receipt in it.
        let (mut model, keys) = marked_chat(1, true);
        model.home_client = true;
        update(&mut model, UiEvent::Viewport { peer: PEER, keys });
        ms_at(&mut model, 1_000);
        let effects = sent(&mut model, "reply");
        assert!(receipts_in(&effects).is_empty(), "{effects:?}");
        ms_at(&mut model, 6_000);
        assert_eq!(seen_marks(&mut model), [(RT as u32, vec![[0x40; 32]])]);
    }

    /// 1234: two devices on one home node. A seen mark another device set
    /// makes the message seen here, kept in its ledger, and takes it out
    /// of the receipt this device gathers: a second viewport sends nothing
    /// new, and only the rest goes.
    #[test]
    fn a_seen_mark_from_another_device_owes_nothing_here() {
        use crate::home::wire::{Mark, MarkFlag};
        let (mut model, keys) = marked_chat(3, true);
        model.home_client = true;
        let _ = model.take_marks();
        let _ = model.take_writes();
        // The laptop saw the first before this phone showed anything.
        engine(
            &mut model,
            OutboxEvent::HomeMarked {
                mark: Mark {
                    flag: MarkFlag::Seen { at: 7 },
                    ids: vec![[0x40; 32]],
                },
            },
        );
        assert!(model.conversations[0].messages[0]
            .ledger
            .iter()
            .any(|e| e.what == receipts::SEEN));
        assert!(model.take_writes().iter().any(|w| matches!(w,
            StoreWrite::Message { message, .. }
                if message.ledger.iter().any(|e| e.what == receipts::SEEN))));
        update(&mut model, UiEvent::Viewport { peer: PEER, keys });
        ms_at(&mut model, 1_000);
        assert_eq!(
            model.reading.owed(&PEER),
            Some(&[[0x41; 32], [0x42; 32]][..])
        );
        // The laptop's mark for the second comes while this one gathers.
        engine(
            &mut model,
            OutboxEvent::HomeMarked {
                mark: Mark {
                    flag: MarkFlag::Seen { at: 8 },
                    ids: vec![[0x41; 32]],
                },
            },
        );
        assert_eq!(model.reading.owed(&PEER), Some(&[[0x42; 32]][..]));
        ms_at(&mut model, 6_000);
        assert_eq!(seen_marks(&mut model), [(RT as u32, vec![[0x42; 32]])]);
        // All three seen elsewhere: nothing is owed at all.
        let (mut model, keys) = marked_chat(1, true);
        model.home_client = true;
        let _ = model.take_marks();
        update(&mut model, UiEvent::Viewport { peer: PEER, keys });
        ms_at(&mut model, 1_000);
        engine(
            &mut model,
            OutboxEvent::HomeMarked {
                mark: Mark {
                    flag: MarkFlag::Seen { at: 9 },
                    ids: vec![[0x40; 32]],
                },
            },
        );
        assert!(model.reading.owed(&PEER).is_none());
        ms_at(&mut model, 6_000);
        assert!(seen_marks(&mut model).is_empty());
    }

    /// 1234: on a client of a home node, a receipt the home node took for
    /// our message comes in the pull and reads it, as on a device of its
    /// own; the receipt the home node signed for what we read comes back as
    /// its sent entry and is no bubble.
    #[test]
    fn on_a_home_client_a_pulled_receipt_reads_ours_and_the_homes_receipt_is_no_bubble() {
        const T: i64 = 1_800_000_000;
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.home_client = true;
        wall(&mut model, T);
        engine(
            &mut model,
            OutboxEvent::HomeSent {
                message_id: ID,
                destination: PEER,
                sent_at: T,
                body: b"from the laptop".to_vec(),
                fields: leviculum_lxmf::lev::Lev::mark().fields().to_vec(),
            },
        );
        state(&mut model, ID, MessageState::Delivered);
        wall(&mut model, T + 60);
        engine(
            &mut model,
            with_lev(PEER, [0x46; 32], "", &receipt((T + 30) as u32, &[ID])),
        );
        let conversation = &model.conversations[0];
        assert_eq!(conversation.messages.len(), 1, "no bubble");
        assert_eq!(
            conversation.messages[0].delivery,
            Some(DeliveryState::Read {
                at: (T + 30) as u32
            })
        );
        engine(
            &mut model,
            OutboxEvent::HomeSent {
                message_id: [0x47; 32],
                destination: PEER,
                sent_at: T + 40,
                body: Vec::new(),
                fields: receipt((T + 40) as u32, &[[0x48; 32]]).fields().to_vec(),
            },
        );
        assert_eq!(model.conversations.len(), 1);
        assert_eq!(model.conversations[0].messages.len(), 1, "no bubble");
    }

    #[test]
    fn the_glyph_follows_the_router_states_of_the_message_it_queued() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        sent(&mut model, "hi");
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        state(&mut model, ID, MessageState::Sending);
        assert_eq!(last_delivery(&model), Some(DeliveryState::InFlight));
        state(&mut model, ID, MessageState::Sent);
        assert_eq!(last_delivery(&model), Some(DeliveryState::Sent));
        update(&mut model, UiEvent::SendDone(Ok(())));
        assert!(model.sending.is_empty());
        // The proof arrives after the runner has finished.
        state(&mut model, ID, MessageState::Delivered);
        assert_eq!(last_delivery(&model), Some(DeliveryState::Arrived));
        // A state for some other id touches nothing.
        state(&mut model, ID2, MessageState::Failed);
        assert_eq!(last_delivery(&model), Some(DeliveryState::Arrived));
    }

    #[test]
    fn an_auto_fallback_never_shows_the_cancelled_direct_leg_as_failed() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        sent(&mut model, "hi");
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        state(&mut model, ID, MessageState::Cancelled);
        assert_eq!(last_delivery(&model), Some(DeliveryState::Queued));
        engine(&mut model, OutboxEvent::Queued { message_id: ID2 });
        state(&mut model, ID2, MessageState::AwaitingCollection);
        assert_eq!(last_delivery(&model), Some(DeliveryState::InMailbox));
        update(&mut model, UiEvent::SendDone(Ok(())));
        assert_eq!(last_delivery(&model), Some(DeliveryState::InMailbox));
    }

    #[test]
    fn a_runner_that_gave_up_marks_the_message_failed_and_keeps_the_text() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        sent(&mut model, "hi");
        let effects = update(
            &mut model,
            UiEvent::SendDone(Err("no route\n  more".to_string())),
        );
        assert_eq!(last_delivery(&model), Some(DeliveryState::Failed));
        assert_eq!(
            effects,
            [UiEffect::Notice("send: no route\n  more".to_string())]
        );
        assert_eq!(
            model.current_toast().map(|t| t.text.as_str()),
            Some(NOT_SENT)
        );
        assert_eq!(
            model.conversations[0].messages[0]
                .ledger
                .last()
                .map(|e| e.what.as_str()),
            Some("not sent: no route"),
            "the reason is the detail view's"
        );
    }

    /// 1279: the toast says it plainly; both legs' reasons are in the
    /// message's detail view.
    #[test]
    fn a_failed_auto_send_toasts_plainly_and_details_both_legs() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        sent(&mut model, "hi");
        update(
            &mut model,
            UiEvent::SendDone(Err(
                "direct: no route after 30.0s\n  more\npropagated: no propagation node".to_string(),
            )),
        );
        assert_eq!(
            model.current_toast().map(|t| t.text.as_str()),
            Some(NOT_SENT)
        );
        update(&mut model, UiEvent::Run(PaletteCommand::Details));
        let detail = view(&model).detail.expect("the detail view");
        assert!(
            detail.rows.iter().any(|row| row.ends_with(
                "not sent: direct: no route after 30.0s; propagated: no propagation node"
            )),
            "{:?}",
            detail.rows
        );
    }

    /// The policy half of leviculum 1002 on the screen: a message the runner
    /// left on a link without a proof shows the clock and keeps it through the
    /// router's retries and its giving up, with no red cross; a late proof
    /// still gives it two ticks.
    #[test]
    fn an_unproven_send_stays_sent_until_a_proof_and_never_turns_red() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        sent(&mut model, "hi");
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        state(&mut model, ID, MessageState::Sending);
        let effects = update(
            &mut model,
            UiEvent::SendUnproven("went onto a link".to_string()),
        );
        assert_eq!(last_delivery(&model), Some(DeliveryState::Sent));
        assert!(model.sending.is_empty());
        assert_eq!(
            effects,
            [UiEffect::Notice("send: went onto a link".to_string())]
        );
        for retry in [
            MessageState::Outbound,
            MessageState::Sending,
            MessageState::Failed,
        ] {
            state(&mut model, ID, retry);
            assert_eq!(
                last_delivery(&model),
                Some(DeliveryState::Sent),
                "{retry:?}"
            );
        }
        engine(
            &mut model,
            OutboxEvent::Left {
                message_id: ID,
                last: Some(MessageState::Failed),
                failure: None,
            },
        );
        assert_eq!(last_delivery(&model), Some(DeliveryState::Sent));

        let mut late = UiModel::<Field>::new(&[PEER]);
        sent(&mut late, "hi");
        engine(&mut late, OutboxEvent::Queued { message_id: ID });
        update(&mut late, UiEvent::SendUnproven(String::new()));
        state(&mut late, ID, MessageState::Delivered);
        assert_eq!(last_delivery(&late), Some(DeliveryState::Arrived));
    }

    /// The row names the leg and its seconds while the send waits, and drops
    /// both once the message is handed on.
    #[test]
    fn a_waiting_send_shows_its_leg_and_the_seconds_on_it() {
        let text =
            |model: &UiModel<Field>| -> String { shown(model).last().cloned().expect("a message") };
        let mut model = UiModel::<Field>::new(&[PEER]);
        sent(&mut model, "hi");
        update(&mut model, UiEvent::SendLeg(Leg::Direct));
        for _ in 0..12 {
            update(&mut model, UiEvent::Tick);
        }
        assert!(text(&model) == "hi  direct 3 s ◷", "{}", text(&model));
        update(&mut model, UiEvent::SendLeg(Leg::Propagated));
        for _ in 0..8 {
            update(&mut model, UiEvent::Tick);
        }
        assert!(
            text(&model) == "hi  via propagation node 2 s ◷",
            "{}",
            text(&model)
        );
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        state(&mut model, ID, MessageState::AwaitingCollection);
        assert_eq!(text(&model), "hi  ✓");
    }

    #[test]
    fn the_mapping_is_the_documented_table() {
        use MessageState::*;
        let rows = [
            (Generating, DeliveryState::Queued),
            (Outbound, DeliveryState::Queued),
            (Sending, DeliveryState::InFlight),
            (Sent, DeliveryState::Sent),
            (AwaitingCollection, DeliveryState::InMailbox),
            (Delivered, DeliveryState::Arrived),
            (Rejected, DeliveryState::Rejected),
            (Cancelled, DeliveryState::Failed),
            (Failed, DeliveryState::Failed),
        ];
        for (router, shown) in rows {
            assert_eq!(delivery_of(router), shown, "{router:?}");
        }
    }

    #[test]
    fn a_received_message_opens_its_conversation_and_counts_unread() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        let received = OutboxEvent::Received {
            sent_at: 0,
            message_id: ID,
            source: OTHER,
            title: Vec::new(),
            body: b"hey".to_vec(),
            fields: Vec::new(),
            verified: true,
        };
        engine(&mut model, received.clone());
        engine(&mut model, received);
        assert_eq!(model.conversations.len(), 2);
        assert_eq!(model.conversations[1].messages.len(), 1, "deduplicated");
        assert_eq!(model.conversations[1].unread, 1);
        // Most recent first: the new conversation is on top.
        assert_eq!(model.order(), [1, 0]);
        update(&mut model, key(KeyCode::Char('g')));
        assert_eq!(model.selected, 1);
        assert_eq!(model.conversations[1].unread, 0);
        assert_eq!(model.messages_of_selected()[0].body, "hey");
    }

    fn received_from(source: [u8; 16], id: [u8; 32], body: &str) -> OutboxEvent {
        OutboxEvent::Received {
            sent_at: 0,
            message_id: id,
            source,
            title: Vec::new(),
            body: body.as_bytes().to_vec(),
            fields: Vec::new(),
            verified: true,
        }
    }

    /// 1490: the laid cache's splice. Moving the cursor across every
    /// message yields, row for row and cell for cell, the layout a fresh
    /// pass draws with that cursor; the spliced rows count, the rest do
    /// not.
    #[test]
    fn a_cursor_move_splices_the_same_rows_a_fresh_layout_draws() {
        let mut model = UiModel::<Field>::new(&[]);
        for n in 0..9u8 {
            let mut id = [0x40; 32];
            id[0] = n;
            match n % 3 {
                0 => engine(
                    &mut model,
                    received_from(OTHER, id, &"word ".repeat(n as usize * 7 + 1)),
                ),
                1 => {
                    update(
                        &mut model,
                        UiEvent::Write {
                            peer: OTHER,
                            body: format!("reply {n}"),
                        },
                    );
                }
                _ => engine(&mut model, received_from(OTHER, id, "short")),
            }
        }
        model.focus = Pane::Messages;
        let len = model.conversations[model.selected].messages.len();
        for cursor in (0..len).rev() {
            model.message_cursor = cursor;
            let cached = model.laid();
            let fresh = layout::conversation(
                &model.conversations[model.selected],
                model.sizes().messages.0,
                &model.draw_context(Some(cursor)),
            );
            assert_eq!(cached.rows.len(), fresh.rows.len(), "cursor {cursor}");
            for (at, (a, b)) in cached.rows.iter().zip(&fresh.rows).enumerate() {
                assert_eq!(
                    format!("{:?}", a.cells),
                    format!("{:?}", b.cells),
                    "cursor {cursor}, row {at}"
                );
            }
            assert_eq!(cached.spans, fresh.spans, "cursor {cursor}");
        }
    }

    /// On a client of a home node, opening a conversation marks its unread
    /// read for the other devices; a device that is no client marks nothing.
    #[test]
    fn opening_a_conversation_on_a_client_marks_its_unread_read() {
        use crate::home::wire::{Mark, MarkFlag};
        for client in [false, true] {
            let mut model = UiModel::<Field>::new(&[PEER]);
            model.home_client = client;
            engine(&mut model, received_from(OTHER, ID, "one"));
            engine(&mut model, received_from(OTHER, ID2, "two"));
            assert_eq!(model.conversations[1].unread, 2);
            assert!(model.take_marks().is_empty(), "nothing read yet");
            update(&mut model, key(KeyCode::Char('g')));
            assert_eq!(model.selected, 1);
            let marks = model.take_marks();
            match client {
                true => assert_eq!(
                    marks,
                    [Mark {
                        flag: MarkFlag::Read,
                        ids: vec![ID2, ID],
                    }]
                ),
                false => assert!(marks.is_empty()),
            }
            // One arriving in the open conversation is read where it lands.
            engine(&mut model, received_from(OTHER, [0x33; 32], "three"));
            assert_eq!(model.conversations[1].unread, 0);
            let marks = model.take_marks();
            match client {
                true => assert_eq!(
                    marks,
                    [Mark {
                        flag: MarkFlag::Read,
                        ids: vec![[0x33; 32]],
                    }]
                ),
                false => assert!(marks.is_empty()),
            }
        }
    }

    /// The ids of every read mark in `marks`, in order.
    fn read_ids(marks: &[crate::home::wire::Mark]) -> Vec<[u8; 32]> {
        use crate::home::wire::MarkFlag;
        marks
            .iter()
            .inspect(|mark| assert_eq!(mark.flag, MarkFlag::Read))
            .flat_map(|mark| mark.ids.iter().copied())
            .collect()
    }

    /// A new device's first pull marks nothing read (1429): a device that
    /// joined a home with an archive of messages across three conversations
    /// pulls from cursor 0. On the terminal the first arriving conversation
    /// opens itself, on the phone the reader opens one while the pull runs;
    /// neither reads the history arriving into it. The read state comes
    /// from the archive's marks, which another device sent. Once the pull
    /// settled, a message the reader opens and one arriving in the open
    /// conversation are read, and only those.
    #[test]
    fn a_new_devices_first_pull_marks_nothing_read() {
        use crate::home::wire::{Mark, MarkFlag};
        const PER: u8 = 30;
        let peers: [[u8; 16]; 3] = [[0xa1; 16], [0xb2; 16], [0xc3; 16]];
        let id = |p: usize, n: u8| {
            let mut id = [0u8; 32];
            id[0] = 0xa0 + p as u8;
            id[1] = n;
            id
        };
        for phone in [false, true] {
            let mut model = UiModel::<Field>::new(&[]);
            model.home_client = true;
            model.list_apart = phone;
            // What both shells set for a client whose cursor is still 0.
            model.history = true;
            for n in 0..PER {
                for (p, peer) in peers.iter().enumerate() {
                    engine(&mut model, received_from(*peer, id(p, n), "history"));
                }
                // The phone's reader opens the first conversation early on:
                // what it shows then is read, what the pull brings into it
                // afterwards is not.
                if phone && n == 4 {
                    update(&mut model, UiEvent::Open { peer: peers[0] });
                    assert_eq!(read_ids(&model.take_marks()).len(), 5, "phone={phone}");
                }
            }
            // Another device read the second conversation up to its 20th.
            engine(
                &mut model,
                OutboxEvent::HomeMarked {
                    mark: Mark {
                        flag: MarkFlag::Read,
                        ids: vec![id(1, 19)],
                    },
                },
            );
            assert!(
                model.take_marks().is_empty(),
                "phone={phone}: the history pull marked read"
            );
            assert!(
                !model
                    .toasts
                    .iter()
                    .any(|t| t.text.starts_with("message from")),
                "phone={phone}: history is no news"
            );
            let unread = |model: &UiModel<Field>, p: usize| {
                let c = model.conversations.iter().find(|c| c.peer == peers[p]);
                c.expect("listed").unread
            };
            let open_unread = match phone {
                true => usize::from(PER) - 5,
                false => usize::from(PER),
            };
            assert_eq!(unread(&model, 0), open_unread, "phone={phone}");
            assert_eq!(unread(&model, 1), 10, "phone={phone}: the archive's marks");
            assert_eq!(unread(&model, 2), usize::from(PER), "phone={phone}");
            assert_eq!(model.conversations[model.selected].peer, peers[0]);
            // The pull settled: live mail from here on.
            model.history = false;
            let live = [0x77; 32];
            engine(&mut model, received_from(peers[0], live, "live"));
            assert_eq!(read_ids(&model.take_marks()), [live], "phone={phone}");
            // The reader opens the third conversation: its unread, and only
            // those, are read.
            update(&mut model, UiEvent::Open { peer: peers[2] });
            let read = read_ids(&model.take_marks());
            assert_eq!(read.len(), usize::from(PER), "phone={phone}");
            assert!(read.iter().all(|r| r[0] == 0xa2), "phone={phone}");
            assert_eq!(unread(&model, 1), 10, "phone={phone}: unopened stays");
        }
    }

    /// A frontend without keys opens a conversation by its peer: the same
    /// read and the same marks as the list's key, and a peer not listed yet
    /// gets a conversation.
    #[test]
    fn a_tap_opens_a_conversation_as_the_key_does() {
        use crate::home::wire::{Mark, MarkFlag};
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.home_client = true;
        engine(&mut model, received_from(OTHER, ID, "one"));
        assert_eq!(model.conversations[1].unread, 1);
        assert!(update(&mut model, UiEvent::Open { peer: OTHER }).is_empty());
        assert_eq!(model.selected, 1);
        assert_eq!(model.conversations[1].unread, 0);
        assert_eq!(
            model.take_marks(),
            [Mark {
                flag: MarkFlag::Read,
                ids: vec![ID],
            }]
        );
        let new = [0x5a; 16];
        update(&mut model, UiEvent::Open { peer: new });
        assert_eq!(model.conversations[model.selected].peer, new);
        // Back to the list: none open, so what arrives stays unread and
        // nothing is marked, where the list is a screen of its own.
        model.list_apart = true;
        update(&mut model, UiEvent::Close);
        assert_eq!(model.selected, NONE_OPEN);
        engine(&mut model, received_from(new, ID2, "later"));
        assert_eq!(model.selected, NONE_OPEN);
        let index = model.conversations.iter().position(|c| c.peer == new);
        assert_eq!(model.conversations[index.expect("listed")].unread, 1);
        assert!(model.take_marks().is_empty());
    }

    /// A chat opened before its page is loaded (1546): a board's two
    /// cores, the open said before the I/O core's page. The row's kept
    /// unread ids are what the mark names, and the list's badge goes with
    /// the count; before, the count went to 0 with no mark, and the home
    /// and the summary log kept the 7.
    #[test]
    fn an_open_before_the_page_marks_the_kept_unread_ids() {
        use crate::home::wire::MarkFlag;
        let group = [0x47; 16];
        let ids: Vec<[u8; 32]> = (1..=7u8).map(|n| [n; 32]).collect();
        let mut model = UiModel::<Field>::new(&[]);
        model.home_client = true;
        model.list_apart = true;
        model.restore_rows(
            vec![(
                listing::Row {
                    peer: group,
                    unread: 7,
                    activity: 9,
                    unread_ids: ids.clone(),
                    listing: listing::Listing {
                        line: String::from("gen! Ich habe"),
                        moved: Some(9),
                        unread_rows: 7,
                        ..listing::Listing::default()
                    },
                },
                500,
            )],
            Vec::new(),
        );
        assert!(model.conversations[0].messages.is_empty());
        let _ = update(&mut model, UiEvent::Open { peer: group });
        let marks = model.take_marks();
        assert!(marks.iter().all(|m| m.flag == MarkFlag::Read));
        let named: Vec<[u8; 32]> = marks.into_iter().flat_map(|m| m.ids).collect();
        let newest_first: Vec<[u8; 32]> = ids.iter().rev().copied().collect();
        assert_eq!(named, newest_first, "the kept ids, all of them");
        let row = model.row(&group).expect("the group's row");
        assert_eq!(row.unread, 0);
        assert_eq!(row.listing.unread_rows, 0, "the badge goes too");
        assert!(row.unread_ids.is_empty());
    }

    /// 1563: a chat listed from its kept row with 776 unread, none of its
    /// messages loaded, takes a live message: its badge says 777, not the
    /// one message loaded. Before, a board's pull and the boot that folds
    /// a ring's tail showed the unread of the loaded messages alone.
    #[test]
    fn a_live_message_on_a_kept_row_counts_the_unloaded_unread() {
        let peer = [0x52; 16];
        let mut model = UiModel::<Field>::new(&[]);
        model.home_client = true;
        model.list_apart = true;
        model.restore_rows(
            vec![(
                listing::Row {
                    peer,
                    unread: 776,
                    activity: 9,
                    unread_ids: Vec::new(),
                    listing: listing::Listing {
                        line: String::from("alt"),
                        moved: Some(9),
                        unread_rows: 776,
                        ..listing::Listing::default()
                    },
                },
                1700,
            )],
            Vec::new(),
        );
        assert_eq!(model.conversations[0].unread_rows(), 776);
        // The list is up: no chat is on the panel (the board's take).
        model.selected = NONE_OPEN;
        engine(&mut model, received_from(peer, ID2, "neu"));
        let conversation = &model.conversations[0];
        assert_eq!(conversation.messages.len(), 1);
        assert_eq!(conversation.unread, 777);
        assert_eq!(conversation.unread_rows(), 777, "the badge keeps the 776");
        assert_eq!(
            model.row(&peer).map(|row| row.listing.unread_rows),
            Some(777),
            "and the row the summary log keeps says it"
        );
    }

    /// A frontend's text goes out as compose's does: a bubble of ours,
    /// queued, in the peer's conversation, opened; blank text sends
    /// nothing; and a reaction by peer and target is the picker's.
    #[test]
    fn a_frontend_writes_and_reacts_without_the_compose_pane() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        assert!(update(
            &mut model,
            UiEvent::Write {
                peer: OTHER,
                body: "  ".to_string()
            }
        )
        .is_empty());
        let effects = update(
            &mut model,
            UiEvent::Write {
                peer: OTHER,
                body: "hallo".to_string(),
            },
        );
        assert_eq!(
            effects,
            [UiEffect::Send {
                to: OTHER,
                body: "hallo".to_string(),
                fields: marked(Vec::new()),
            }]
        );
        let conversation = &model.conversations[model.selected];
        assert_eq!(conversation.peer, OTHER);
        let ours = conversation.messages.last().expect("the bubble");
        assert!(ours.outbound);
        assert_eq!(ours.delivery, Some(DeliveryState::Queued));
        assert_eq!(model.sending.len(), 1);
        assert_eq!(model.compose.value(), "", "compose is not touched");

        engine(&mut model, received_from(OTHER, ID, "lunch?"));
        let effects = update(
            &mut model,
            UiEvent::React {
                peer: OTHER,
                target: ID,
                content: "ok".to_string(),
            },
        );
        let [UiEffect::Send { to, body, fields }] = effects.as_slice() else {
            panic!("one send: {effects:?}");
        };
        assert_eq!((*to, body.as_str()), (OTHER, ""));
        // The reaction and the read-receipt mark (1230).
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].0, leviculum_lxmf::constants::FIELD_REACTION);
        assert_eq!(model.sending.len(), 2);
    }

    /// Another device read a message: every received message up to it is
    /// read here, the later ones stay unread, and the same mark twice
    /// changes nothing more.
    #[test]
    fn a_read_mark_from_another_device_reads_up_to_its_message() {
        use crate::home::wire::{Mark, MarkFlag};
        let mut model = UiModel::<Field>::new(&[PEER]);
        engine(&mut model, received_from(OTHER, ID, "one"));
        engine(&mut model, received_from(OTHER, ID2, "two"));
        engine(&mut model, received_from(OTHER, [0x33; 32], "three"));
        assert_eq!(model.conversations[1].unread, 3);
        let read = OutboxEvent::HomeMarked {
            mark: Mark {
                flag: MarkFlag::Read,
                ids: vec![ID2],
            },
        };
        engine(&mut model, read.clone());
        assert_eq!(model.conversations[1].unread, 1);
        engine(&mut model, read);
        assert_eq!(model.conversations[1].unread, 1);
        // An id this device never had changes nothing.
        engine(
            &mut model,
            OutboxEvent::HomeMarked {
                mark: Mark {
                    flag: MarkFlag::Read,
                    ids: vec![[0x99; 32]],
                },
            },
        );
        assert_eq!(model.conversations[1].unread, 1);
        // The store learns the new count; a mark applied is not marked again.
        let writes = model.take_writes();
        assert!(writes.iter().any(|w| matches!(
            w,
            StoreWrite::Conversation { peer, unread: 1, .. } if *peer == OTHER
        )));
        assert!(model.take_marks().is_empty());
    }

    /// 1600: the model's id index, after a pull's worth of arrivals,
    /// sends named by the home, a deleted message, a deleted
    /// conversation, kept rows and read marks, is what a rebuild reads
    /// off the conversations; and it got there by its journals, not by
    /// reading everything anew, except where a conversation went.
    #[test]
    fn the_id_index_is_a_rebuild_of_the_conversations_after_a_pull_with_marks() {
        use crate::home::wire::{Mark, MarkFlag};
        let peers: Vec<[u8; 16]> = (0..40u8).map(|p| [p + 1; 16]).collect();
        let id = |p: u8, n: u8| {
            let mut id = [p; 32];
            id[31] = n;
            id
        };
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.home_client = true;
        let exact = |model: &mut UiModel<Field>| {
            let mut copy = model.conversations.clone();
            let rebuilt = id_index::IdIndex::rebuild(&mut copy);
            *model.id_index() == rebuilt
        };
        for n in 0..5u8 {
            for (p, &peer) in peers.iter().enumerate() {
                engine(&mut model, received_from(peer, id(p as u8 + 1, n), "hi"));
            }
            assert!(exact(&mut model), "after round {n}");
        }
        // Sends the home names as it takes them: an id appears in place.
        for (p, &peer) in peers.iter().enumerate().take(10) {
            engine(
                &mut model,
                OutboxEvent::HomeSent {
                    message_id: id(p as u8 + 1, 200),
                    destination: peer,
                    sent_at: 0,
                    body: b"back".to_vec(),
                    fields: Vec::new(),
                },
            );
        }
        // A send from here, named in place when the router takes it.
        let _ = update(&mut model, UiEvent::Open { peer: peers[0] });
        say(&mut model, "named later");
        engine(
            &mut model,
            OutboxEvent::Queued {
                message_id: id(1, 201),
            },
        );
        assert!(exact(&mut model));
        assert!(model.id_index().holding(&id(1, 201)).next().is_some());
        // Read marks, the store's way: 64 ids at a time.
        let all: Vec<[u8; 32]> = (0..5u8)
            .flat_map(|n| (1..=40u8).map(move |p| id(p, n)))
            .collect();
        for chunk in all.chunks(64) {
            engine(
                &mut model,
                OutboxEvent::HomeMarked {
                    mark: Mark {
                        flag: MarkFlag::Read,
                        ids: chunk.to_vec(),
                    },
                },
            );
        }
        assert!(model.conversations.iter().all(|c| c.unread == 0));
        // Another device deleted a message.
        engine(
            &mut model,
            OutboxEvent::HomeMarked {
                mark: Mark {
                    flag: MarkFlag::Deleted,
                    ids: vec![id(3, 0)],
                },
            },
        );
        assert!(exact(&mut model));
        assert!(model.id_index().holding(&id(3, 0)).next().is_none());
        assert_eq!(model.id_index().rebuilds(), 0, "the journals carried it");
        // A conversation deleted: everything after it moved up a place.
        update(
            &mut model,
            UiEvent::DeleteConversation {
                peer: peers[5],
                everywhere: false,
            },
        );
        assert!(exact(&mut model));
        assert!(model.id_index().holding(&id(6, 1)).next().is_none());
        assert_eq!(model.id_index().rebuilds(), 1, "a place moved: read anew");
        let at = model.conversations.iter().position(|c| c.peer == peers[7]);
        assert_eq!(
            model.id_index().holding(&id(8, 1)).collect::<Vec<_>>(),
            at.into_iter().collect::<Vec<_>>()
        );
        // A kept row, none of its messages loaded (1521).
        let row_peer = [0x99; 16];
        let kept: Vec<[u8; 32]> = (1..=3u8).map(|n| id(0x99, n)).collect();
        model.restore_rows(
            vec![(
                listing::Row {
                    peer: row_peer,
                    unread: 3,
                    activity: 9,
                    unread_ids: kept.clone(),
                    listing: listing::Listing::default(),
                },
                500,
            )],
            Vec::new(),
        );
        assert!(exact(&mut model));
        engine(
            &mut model,
            OutboxEvent::HomeMarked {
                mark: Mark {
                    flag: MarkFlag::Read,
                    ids: vec![kept[1]],
                },
            },
        );
        let row = model.conversations.iter().find(|c| c.peer == row_peer);
        assert_eq!(row.map(|c| c.unread), Some(1));
        assert!(exact(&mut model));
        assert!(model.id_index().keeping(&kept[0]).next().is_none());
        assert_eq!(model.id_index().rebuilds(), 1);
    }

    /// 1600: a read mark asks the id index, not every conversation in
    /// turn. 64 ids no conversation holds look into none and walk no
    /// message; 64 that one holds each look into that one.
    #[test]
    fn a_read_mark_looks_only_into_the_conversations_that_hold_its_ids() {
        use crate::home::wire::{Mark, MarkFlag};
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.home_client = true;
        let id = |p: u8, n: u8| {
            let mut id = [p; 32];
            id[31] = n;
            id
        };
        for n in 0..64u8 {
            for p in 1..=50u8 {
                engine(&mut model, received_from([p; 16], id(p, n), "hi"));
            }
        }
        let mark = |ids: Vec<[u8; 32]>| OutboxEvent::HomeMarked {
            mark: Mark {
                flag: MarkFlag::Read,
                ids,
            },
        };
        let unknown: Vec<[u8; 32]> = (0..64u8).map(|n| id(0xee, n)).collect();
        let _ = update(&mut model, UiEvent::Engine(mark(unknown)));
        let counted = model.counters().expect("this build counts");
        assert_eq!((counted.probed, counted.walked), (0, 0), "{counted}");
        let known: Vec<[u8; 32]> = (0..64u8).map(|n| id(7, n)).collect();
        let _ = update(&mut model, UiEvent::Engine(mark(known)));
        let counted = model.counters().expect("this build counts");
        assert_eq!((counted.probed, counted.walked), (64, 0), "{counted}");
        let seventh = model.conversations.iter().find(|c| c.peer == [7; 16]);
        assert_eq!(seventh.map(|c| c.unread), Some(0));
        assert!(model
            .conversations
            .iter()
            .filter(|c| c.peer != [7; 16] && c.peer != PEER)
            .all(|c| c.unread == 64));
    }

    /// Another device deleted a message: off the screen and out of the
    /// store, by its id, whether this device shows it or not.
    #[test]
    fn a_deleted_mark_from_another_device_deletes_the_message_here() {
        use crate::home::wire::{Mark, MarkFlag};
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.home_client = true;
        engine(&mut model, received_from(OTHER, ID, "one"));
        engine(&mut model, received_from(OTHER, ID2, "two"));
        model.take_writes();
        let deleted = OutboxEvent::HomeMarked {
            mark: Mark {
                flag: MarkFlag::Deleted,
                ids: vec![ID, [0x99; 32]],
            },
        };
        engine(&mut model, deleted);
        assert_eq!(model.conversations[1].messages.len(), 1);
        assert_eq!(model.conversations[1].messages[0].id, Some(ID2));
        assert_eq!(
            washed(model.take_writes()),
            [
                StoreWrite::ForgetId { id: ID },
                StoreWrite::ForgetId { id: [0x99; 32] }
            ]
        );
        assert!(model.take_marks().is_empty(), "not marked back");
    }

    /// A message another device sent through the home node comes as a sent
    /// bubble; the device that sent it keeps its own bubble, which leaves
    /// queued; a bubble further along does not go back.
    #[test]
    fn a_message_sent_from_another_device_shows_as_a_sent_bubble() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.home_client = true;
        let home_sent = |id: [u8; 32], to: [u8; 16]| OutboxEvent::HomeSent {
            message_id: id,
            destination: to,
            sent_at: 0,
            body: b"from the laptop".to_vec(),
            fields: Vec::new(),
        };
        engine(&mut model, home_sent(ID, OTHER));
        assert_eq!(model.conversations.len(), 2);
        let bubble = &model.conversations[1].messages[0];
        assert!(bubble.outbound);
        assert_eq!(bubble.body, "from the laptop");
        assert_eq!(bubble.delivery, Some(DeliveryState::Sent));
        assert_eq!(model.conversations[1].unread, 0, "ours, not unread");
        // Again: one bubble.
        engine(&mut model, home_sent(ID, OTHER));
        assert_eq!(model.conversations[1].messages.len(), 1);
        // Proven, then the sent entry again: stays proven.
        state(&mut model, ID, MessageState::Delivered);
        engine(&mut model, home_sent(ID, OTHER));
        assert_eq!(
            model.conversations[1].messages[0].delivery,
            Some(DeliveryState::Arrived)
        );
        // The device that sent it: its queued bubble says sent.
        sent(&mut model, "mine");
        engine(&mut model, OutboxEvent::Queued { message_id: ID2 });
        assert_eq!(last_delivery(&model), Some(DeliveryState::Queued));
        engine(&mut model, home_sent(ID2, PEER));
        assert_eq!(last_delivery(&model), Some(DeliveryState::Sent));
        assert_eq!(model.conversations[0].messages.len(), 1);
    }

    /// 1335: on a client the pull can bring the sent entry of the running
    /// send before the runner names it (the home's answer and the pull race
    /// on two links): the running bubble takes the id, no second bubble
    /// appears, and the late name does not take it back to queued.
    #[test]
    fn a_sent_entry_pulled_before_the_runner_names_it_is_the_running_bubble() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.home_client = true;
        sent(&mut model, "und die Antwort");
        let home_sent = |body: &[u8]| OutboxEvent::HomeSent {
            message_id: ID,
            destination: PEER,
            sent_at: 0,
            body: body.to_vec(),
            fields: Vec::new(),
        };
        // Another device's message to the same peer: not the running one.
        engine(&mut model, home_sent(b"from the laptop"));
        assert_eq!(model.conversations[0].messages.len(), 2);
        assert_eq!(model.conversations[0].messages[0].id, None);
        let id = [0x49; 32];
        engine(
            &mut model,
            OutboxEvent::HomeSent {
                message_id: id,
                destination: PEER,
                sent_at: 0,
                body: b"und die Antwort".to_vec(),
                fields: Vec::new(),
            },
        );
        engine(&mut model, OutboxEvent::Queued { message_id: id });
        let messages = &model.conversations[0].messages;
        assert_eq!(messages.len(), 2, "one bubble for the running send");
        assert_eq!(messages[0].id, Some(id));
        assert_eq!(messages[0].delivery, Some(DeliveryState::Sent));
        update(&mut model, UiEvent::SendDone(Ok(())));
        assert!(model.sending.is_empty());
    }

    /// A finished send on a client pulls at once; one finishing while a
    /// pull runs pulls again after it.
    #[test]
    fn a_send_on_a_client_is_followed_by_a_pull() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        sent(&mut model, "not a client");
        assert!(update(&mut model, UiEvent::SendDone(Ok(()))).is_empty());
        model.home_client = true;
        // Still in compose: the next sends are typed and sent from there.
        typed(&mut model, "a client");
        update(&mut model, key(KeyCode::Enter));
        assert_eq!(
            update(&mut model, UiEvent::SendDone(Ok(()))),
            [UiEffect::Fetch]
        );
        assert_eq!(model.fetch_reason, Some(Reason::Sent));
        typed(&mut model, "while it pulls");
        update(&mut model, key(KeyCode::Enter));
        assert!(update(&mut model, UiEvent::SendDone(Ok(()))).is_empty());
        assert_eq!(
            update(&mut model, UiEvent::FetchDone(Ok(0))),
            [UiEffect::Fetch]
        );
        assert!(update(&mut model, UiEvent::FetchDone(Ok(0))).is_empty());
    }

    /// A wake pulls at once on a client, follows a running pull once however
    /// many wakes come meanwhile, and does nothing on a messenger.
    #[test]
    fn a_wake_on_a_client_pulls_and_a_burst_behind_a_pull_is_one_more() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        assert!(update(&mut model, UiEvent::HomeWake).is_empty());
        model.home_client = true;
        assert_eq!(update(&mut model, UiEvent::HomeWake), [UiEffect::Fetch]);
        assert_eq!(model.fetch_reason, Some(Reason::Wake));
        for _ in 0..3 {
            assert!(update(&mut model, UiEvent::HomeWake).is_empty());
        }
        assert_eq!(
            update(&mut model, UiEvent::FetchDone(Ok(1))),
            [UiEffect::Fetch]
        );
        assert_eq!(model.fetch_reason, Some(Reason::Wake));
        assert!(update(&mut model, UiEvent::FetchDone(Ok(0))).is_empty());
    }

    /// 1136: a wake's pull is quiet. The header says "fetching" for it
    /// only past a second, a pull with nothing new raises no toast, and
    /// one asked for says so at once.
    #[test]
    fn a_wake_pull_is_silent_unless_it_is_slow_or_brings_a_message() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.home_client = true;
        update(
            &mut model,
            UiEvent::MailboxChosen {
                node: [0x9c; 16],
                by_user: false,
            },
        );
        let toasts = model.toasts.len();
        update(&mut model, UiEvent::Elapsed(10_000));
        assert_eq!(update(&mut model, UiEvent::HomeWake), [UiEffect::Fetch]);
        assert!(!view(&model).messages_title_right.contains("fetching"));
        update(&mut model, UiEvent::Elapsed(10_000 + QUIET_PULL_MS - 1));
        assert!(!view(&model).messages_title_right.contains("fetching"));
        update(&mut model, UiEvent::Elapsed(10_000 + QUIET_PULL_MS));
        assert!(view(&model).messages_title_right.contains("fetching"));
        update(&mut model, UiEvent::FetchDone(Ok(0)));
        assert_eq!(model.toasts.len(), toasts, "names only: nothing said");
        update(&mut model, UiEvent::HomeWake);
        update(&mut model, UiEvent::FetchDone(Ok(1)));
        assert_eq!(
            model.toasts.last().map(|t| t.text.as_str()),
            Some("1 new message(s)")
        );
        update(&mut model, UiEvent::Run(PaletteCommand::Fetch));
        assert!(view(&model).messages_title_right.contains("fetching"));
    }

    #[test]
    fn a_reader_at_the_newest_message_follows_a_new_one() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        let from_peer = |id: [u8; 32]| OutboxEvent::Received {
            sent_at: 0,
            message_id: id,
            source: PEER,
            title: Vec::new(),
            body: b"hey".to_vec(),
            fields: Vec::new(),
            verified: true,
        };
        engine(&mut model, from_peer(ID));
        assert_eq!(model.message_cursor, 0);
        engine(&mut model, from_peer(ID2));
        assert_eq!(model.message_cursor, 1, "followed");
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Char('g')));
        engine(&mut model, from_peer([0x33; 32]));
        assert_eq!(model.message_cursor, 0, "a reader scrolled up stays put");
    }

    fn alt_key(c: char) -> UiEvent {
        UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT))
    }

    #[test]
    fn the_palette_filters_runs_and_closes() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(&mut model, key(KeyCode::Char(':')));
        assert!(model.palette.is_some());
        typed(&mut model, "foc");
        update(&mut model, key(KeyCode::Down));
        update(&mut model, key(KeyCode::Down));
        update(&mut model, key(KeyCode::Enter));
        assert!(model.palette.is_none());
        assert_eq!(model.focus, Pane::Compose);
        // From compose the palette is Alt-x.
        update(&mut model, alt_key('x'));
        typed(&mut model, "fetch");
        assert_eq!(update(&mut model, key(KeyCode::Enter)), [UiEffect::Fetch]);
        assert!(model.fetching);
        update(&mut model, alt_key('x'));
        update(&mut model, key(KeyCode::Esc));
        assert!(model.palette.is_none());
        assert_eq!(
            model.focus,
            Pane::Compose,
            "closing the palette keeps the pane"
        );
    }

    /// `:` `q` `Enter` quits, as a vi reader expects.
    #[test]
    fn colon_q_enter_quits() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(&mut model, key(KeyCode::Char(':')));
        typed(&mut model, "q");
        assert_eq!(update(&mut model, key(KeyCode::Enter)), [UiEffect::Quit]);
        assert!(model.quit);
    }

    /// Decided change 1: `Enter` on a message opens its detail view; compose
    /// is `i`, `a`, `r`.
    #[test]
    fn enter_on_a_message_shows_its_details_and_i_a_r_compose() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        sent(&mut model, "hi");
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        update(&mut model, key(KeyCode::Esc));
        assert_eq!(model.focus, Pane::Messages);
        assert!(update(&mut model, key(KeyCode::Enter)).is_empty());
        assert_eq!(model.focus, Pane::Messages, "Enter no longer composes");
        let detail = view(&model).detail.expect("the detail view");
        assert!(
            detail
                .rows
                .contains(&"state  waiting to be sent".to_string()),
            "{detail:?}"
        );
        update(&mut model, key(KeyCode::Esc));
        assert!(model.detail.is_none());
        for c in ['i', 'a', 'r'] {
            model.focus = Pane::Messages;
            update(&mut model, key(KeyCode::Char(c)));
            assert_eq!(model.focus, Pane::Compose, "{c}");
        }
        // From the list too, and Enter there opens the conversation.
        model.focus = Pane::Conversations;
        update(&mut model, key(KeyCode::Enter));
        assert_eq!(model.focus, Pane::Messages);
    }

    /// Decided change 2: `Ctrl-p` and `Ctrl-n` in compose are the buffer's
    /// (line motions), not the palette; in a command pane they move the
    /// cursor, as in lnomad.
    #[test]
    fn ctrl_p_is_a_motion_everywhere_and_never_the_palette() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        update(&mut model, ctrl_key('n'));
        assert_eq!(model.selected, 1);
        update(&mut model, ctrl_key('p'));
        assert_eq!(model.selected, 0);
        assert!(model.palette.is_none());
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Tab));
        typed(&mut model, "x");
        update(&mut model, ctrl_key('p'));
        assert!(model.palette.is_none());
        assert_eq!(model.compose.value(), "x");
    }

    /// Decided change 3: `Ctrl-c` quits nowhere. Outside compose it says
    /// what does, and over an overlay too.
    #[test]
    fn ctrl_c_quits_nowhere_and_says_ctrl_q_does() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        assert!(update(&mut model, ctrl_key('c')).is_empty());
        assert!(!model.quit);
        assert_eq!(
            model.current_toast().map(|t| t.text.as_str()),
            Some("Ctrl-q quits")
        );
        update(&mut model, key(KeyCode::Char('?')));
        assert!(update(&mut model, ctrl_key('c')).is_empty());
        assert!(!model.quit);
        assert_eq!(update(&mut model, ctrl_key('q')), [UiEffect::Quit]);
    }

    /// Decided change 4: with `send_on_enter` off, `Enter` breaks the line
    /// and `Alt-Enter` sends; the palette toggles it and says the new rule.
    #[test]
    fn send_on_enter_off_swaps_enter_and_alt_enter() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(&mut model, key(KeyCode::Char(':')));
        typed(&mut model, "send on enter");
        update(&mut model, key(KeyCode::Enter));
        assert!(!model.send_on_enter);
        assert_eq!(
            model.current_toast().map(|t| t.text.as_str()),
            Some("Alt-Enter send · Enter newline")
        );
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Tab));
        typed(&mut model, "one");
        assert!(update(&mut model, key(KeyCode::Enter)).is_empty());
        typed(&mut model, "two");
        assert_eq!(view(&model).compose_hint, "Alt-Enter send · Enter newline");
        let effects = update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
        );
        assert_eq!(
            effects,
            [UiEffect::Send {
                to: PEER,
                body: "one\ntwo".to_string(),
                fields: marked(Vec::new()),
            }]
        );
        model.enhanced = true;
        assert_eq!(view(&model).compose_hint, "Ctrl-Enter send · Enter newline");
    }

    /// The Enter rule under the enhancement protocol: `Shift-Enter` breaks
    /// the line, `Ctrl-Enter` sends and never breaks it, and the hint says
    /// so; `Ctrl-j` breaks the line in every terminal.
    #[test]
    fn shift_enter_breaks_ctrl_enter_sends_and_ctrl_j_breaks_everywhere() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.enhanced = true;
        assert_eq!(
            view(&model).compose_hint,
            "Enter send · Shift-Enter newline"
        );
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Tab));
        typed(&mut model, "a");
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
        );
        typed(&mut model, "b");
        update(&mut model, ctrl_key('j'));
        typed(&mut model, "c");
        let effects = update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)),
        );
        assert_eq!(
            effects,
            [UiEffect::Send {
                to: PEER,
                body: "a\nb\nc".to_string(),
                fields: marked(Vec::new()),
            }]
        );
        model.enhanced = false;
        assert_eq!(view(&model).compose_hint, "Enter send · Alt-Enter newline");
    }

    /// Signal's conversation keys: `Alt-↓`/`Alt-↑` step, `Alt-Shift-↓` jumps
    /// to the next unread, from any pane.
    #[test]
    fn alt_arrows_step_conversations_and_alt_shift_finds_unread() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER, [0xef; 16]]);
        let alt = |code| UiEvent::Key(KeyEvent::new(code, KeyModifiers::ALT));
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, alt(KeyCode::Down));
        assert_eq!((model.selected, model.focus), (1, Pane::Compose));
        update(&mut model, alt(KeyCode::Up));
        assert_eq!(model.selected, 0);
        model.conversations[2].unread = 1;
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(
                KeyCode::Down,
                KeyModifiers::ALT | KeyModifiers::SHIFT,
            )),
        );
        assert_eq!(model.selected, 2);
        assert_eq!(model.conversations[2].unread, 0);
    }

    /// Help, footer and palette all read the table: the footer's keys move
    /// with the pane, the palette shows each name's chord, the help lists
    /// every row.
    #[test]
    fn help_footer_and_palette_derive_from_the_table() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        let footer_keys = |model: &UiModel<Field>| -> Vec<(&str, &str)> {
            view(model)
                .footer
                .iter()
                .map(|f| (f.key, f.label))
                .collect()
        };
        assert_eq!(
            footer_keys(&model),
            [
                ("Enter", "open"),
                ("i", "write"),
                ("Tab", "pane"),
                (":", "commands"),
                ("?", "help"),
                ("q", "quit")
            ]
        );
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Tab));
        assert_eq!(
            footer_keys(&model),
            [
                ("Enter", "send"),
                ("Tab", "pane"),
                ("Esc", "back"),
                ("Alt-x", "commands"),
                ("F1", "help"),
                ("Ctrl-q", "quit")
            ]
        );
        update(&mut model, alt_key('x'));
        let rows: Vec<String> = view(&model)
            .palette
            .map(|p| p.rows.iter().map(text).collect())
            .unwrap_or_default();
        assert_eq!(rows[0], "▸ send  Enter");
        assert!(rows.contains(&"  quit  q".to_string()), "{rows:?}");
        let help: Vec<&str> = help_groups()
            .iter()
            .flat_map(|g| g.entries.iter().map(|e| e.desc))
            .collect();
        assert!(help.contains(&"scroll a line"));
        assert!(help.contains(&"the message's details"));
    }

    #[test]
    fn help_opens_from_a_command_pane_and_closes_on_esc() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(&mut model, key(KeyCode::Char('?')));
        assert!(model.help);
        assert!(view(&model).help);
        update(&mut model, key(KeyCode::Char('j')));
        assert!(model.help, "keys do not leak through the overlay");
        update(&mut model, key(KeyCode::Esc));
        assert!(!model.help);
    }

    #[test]
    fn the_palette_s_theme_swaps_light_and_dark_and_says_which() {
        let mut model = UiModel::<Field>::new(&[]);
        assert_eq!(model.theme, Theme::Dark);
        update(&mut model, UiEvent::Run(PaletteCommand::Theme));
        assert_eq!(model.theme, Theme::Light);
        assert_eq!(
            model.current_toast().map(|t| t.text.as_str()),
            Some("theme light")
        );
        update(&mut model, UiEvent::Run(PaletteCommand::Theme));
        assert_eq!(model.theme, Theme::Dark);
        assert_eq!(
            model.current_toast().map(|t| t.text.as_str()),
            Some("theme dark")
        );
    }

    #[test]
    fn toasts_age_out_on_the_tick() {
        let mut model = UiModel::<Field>::new(&[]);
        sent(&mut model, "hi");
        assert!(model.current_toast().is_some());
        for _ in 0..TOAST_TICKS {
            update(&mut model, UiEvent::Tick);
        }
        assert!(model.current_toast().is_none());
        assert!(model.toasts.is_empty());
    }

    fn text(line: &RLine) -> String {
        line.cells.iter().map(|c| c.ch).collect()
    }

    /// The conversation pane's rows that carry something, as text, trimmed.
    fn shown(model: &UiModel<Field>) -> Vec<String> {
        view(model)
            .messages
            .iter()
            .map(text)
            .map(|row| row.trim().to_string())
            .filter(|row| !row.is_empty())
            .collect()
    }

    /// Walk 1 #26: whitespace and newlines at the edges never travel, on
    /// every way a target sends (Write, Reply, the photo caption); the
    /// inner newline stays, and a body of only whitespace sends nothing.
    #[test]
    fn a_send_trims_its_edges_on_every_path() {
        let body_of = |effects: &[UiEffect]| -> Vec<String> {
            effects
                .iter()
                .filter_map(|e| match e {
                    UiEffect::Send { body, .. } => Some(body.clone()),
                    _ => None,
                })
                .collect()
        };
        let mut model = UiModel::<Field>::new(&[PEER]);
        let body = String::from("  hi\nthere \n\n");
        let effects = update(&mut model, UiEvent::Write { peer: PEER, body });
        assert_eq!(body_of(&effects), ["hi\nthere"]);
        let shown = model.conversations[0]
            .messages
            .last()
            .map(|m| m.body.clone());
        assert_eq!(shown.as_deref(), Some("hi\nthere"));
        let body = String::from(" \n \t\n");
        assert!(body_of(&update(&mut model, UiEvent::Write { peer: PEER, body })).is_empty());
        let reply = UiEvent::Reply {
            peer: PEER,
            target: [7; 32],
            body: String::from("\nyes\n"),
        };
        assert_eq!(body_of(&update(&mut model, reply)), ["yes"]);
        assert_eq!(outgoing(String::from("as typed")), "as typed");
    }

    /// The terminal's column of the ladder (1232), a snapshot: five
    /// messages, one per rung, in Unicode and in ASCII, each glyph two
    /// cells, read and failed bold, read in its own tone.
    #[test]
    fn the_terminal_column_draws_the_ladder() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        for body in ["a", "b", "c", "d", "e"] {
            let body = body.to_string();
            update(&mut model, UiEvent::Write { peer: PEER, body });
        }
        let states = [
            DeliveryState::InFlight,
            DeliveryState::InMailbox,
            DeliveryState::Delivered,
            DeliveryState::Read { at: 1_800_000_000 },
            DeliveryState::Failed,
        ];
        model.conversations[0]
            .messages
            .for_each_indexed_mut(|m, message| message.delivery = Some(states[m]));
        model.sending.clear();
        let column = |model: &UiModel<Field>| -> Vec<String> {
            view(model)
                .messages
                .iter()
                .map(text)
                .filter(|row| row.trim().len() == 1 || row.contains("  "))
                .filter_map(|row| {
                    let row = row.trim().to_string();
                    (row.len() <= 12 && !row.is_empty()).then_some(row)
                })
                .collect()
        };
        assert_eq!(column(&model), ["a  ⠋", "b  ✓", "c  ✓✓", "d  ✔✔", "e  !"]);
        model.ascii = true;
        assert_eq!(column(&model), ["a  *", "b  v", "c  vv", "d  VV", "e  !"]);
        model.ascii = false;
        let rows = view(&model).messages;
        let cell = |glyph: char| {
            rows.iter()
                .flat_map(|row| &row.cells)
                .find(|c| c.ch == glyph)
                .map(|c| c.st)
                .expect("the glyph")
        };
        let tokens = model.tokens();
        assert!(cell('✔').bold && cell('!').bold);
        assert!(!cell('✓').bold);
        assert_eq!(cell('✔').fg, tokens.tones.read);
        assert_eq!(cell('✓').fg, tokens.tones.pending);
        assert_eq!(cell('!').fg, tokens.tones.failed);
    }

    #[test]
    fn the_view_draws_the_glyph_and_says_nothing_is_earlier() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        assert_eq!(shown(&model), ["No messages yet"]);
        assert_eq!(view(&model).messages_title, "Unknown");
        sent(&mut model, "hi");
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        assert_eq!(shown(&model), ["hi  ◷"]);
        state(&mut model, ID, MessageState::Delivered);
        assert_eq!(shown(&model)[0], "hi  ✓");
        model.ascii = true;
        assert_eq!(shown(&model)[0], "hi  v");
        assert_eq!(text(&view(&model).conversations[0]).trim_end(), "▌Unknown");
        assert_eq!(text(&view(&model).conversations[1]).trim_end(), "▌you: hi");
        // 1381, finding 22: the terminal's list speaks the reader's language.
        model.lang = strings::Lang::De;
        assert_eq!(text(&view(&model).conversations[1]).trim_end(), "▌du: hi");
    }

    /// `Alt-Enter` puts a line break in the compose pane, `Enter` still
    /// sends, and the sent body keeps its lines (leviculum 1002).
    #[test]
    fn alt_enter_breaks_the_line_and_enter_sends_both_lines() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Tab));
        typed(&mut model, "one");
        let effects = update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
        );
        assert!(effects.is_empty(), "Alt-Enter must not send: {effects:?}");
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
        );
        typed(&mut model, "three");
        assert_eq!(model.compose.value(), "one\n\nthree");
        let effects = update(&mut model, key(KeyCode::Enter));
        assert_eq!(
            effects,
            [UiEffect::Send {
                to: PEER,
                body: "one\n\nthree".to_string(),
                fields: marked(Vec::new()),
            }]
        );
    }

    /// A body with line breaks is drawn as its lines in one block, the mark
    /// closing the last, both ways.
    #[test]
    fn a_multi_line_body_is_its_lines_in_one_block_with_the_mark_last() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        sent(&mut model, "");
        model.compose.0 = "first\nsecond".to_string();
        update(&mut model, key(KeyCode::Enter));
        engine(
            &mut model,
            OutboxEvent::Received {
                sent_at: 0,
                message_id: ID2,
                source: PEER,
                title: Vec::new(),
                body: b"back\nagain".to_vec(),
                fields: Vec::new(),
                verified: true,
            },
        );
        assert_eq!(shown(&model), ["first", "second  ◷", "back", "again"]);
    }

    #[test]
    fn a_toast_wraps_at_words_to_three_rows_and_cuts_the_rest() {
        assert_eq!(toast_rows("short", 20), ["short"]);
        assert_eq!(
            toast_rows("not sent: direct: no route; propagated: none", 20),
            ["not sent: direct: no", "route; propagated:", "none"]
        );
        let long = toast_rows(&"word ".repeat(20), 12);
        assert_eq!(long.len(), TOAST_MAX_ROWS, "{long:?}");
        assert!(long[2].ends_with('…'), "{long:?}");
        assert!(long.iter().all(|row| row.chars().count() <= 12), "{long:?}");
    }

    #[test]
    fn wrap_cuts_to_the_width_and_sets_the_rest_in() {
        let rows: Vec<String> = wrap(&[plain("abcdefghij"), plain("")], 6, 2)
            .iter()
            .map(text)
            .collect();
        assert_eq!(rows, ["abcdef", "  ghij", ""]);
        // At a space when one fits, and the space goes with the break.
        let rows: Vec<String> = wrap(&[plain("ab cd efgh")], 6, 2)
            .iter()
            .map(text)
            .collect();
        assert_eq!(rows, ["ab cd", "  efgh"]);
        // An indent as wide as the pane would leave no room: none then.
        let rows: Vec<String> = wrap(&[plain("abcd")], 2, 2).iter().map(text).collect();
        assert_eq!(rows, ["ab", "cd"]);
    }

    // The chat layout (loquax-keys-and-layout.md, section 5; slice 2).

    /// 2026-10-26 at `hour:minute`, an hour east of UTC.
    fn at(day: u32, hour: u32, minute: u32) -> WallTime {
        WallTime::from_civil(2026, 10, day, hour, minute, 3600)
    }

    fn clock(model: &mut UiModel<Field>, day: u32, hour: u32, minute: u32) {
        update(model, UiEvent::Clock(at(day, hour, minute)));
    }

    fn receive(model: &mut UiModel<Field>, from: [u8; 16], id: u8, body: &str) {
        engine(
            model,
            OutboxEvent::Received {
                sent_at: 0,
                message_id: [id; 32],
                source: from,
                title: Vec::new(),
                body: body.as_bytes().to_vec(),
                fields: Vec::new(),
                verified: true,
            },
        );
    }

    /// Send from compose and come back to the list, the focus where it was.
    fn say(model: &mut UiModel<Field>, body: &str) {
        let focus = model.focus;
        model.focus = Pane::Compose;
        typed(model, body);
        update(model, key(KeyCode::Enter));
        model.focus = focus;
    }

    /// The conversation pane's rows as text, untrimmed.
    fn rows(model: &UiModel<Field>) -> Vec<String> {
        view(model).messages.iter().map(text).collect()
    }

    /// The column of a row's first and last non-blank cell.
    fn extent(row: &str) -> (usize, usize) {
        let cells: Vec<char> = row.chars().collect();
        let first = cells.iter().position(|c| *c != ' ').unwrap_or(0);
        let last = cells.iter().rposition(|c| *c != ' ').unwrap_or(0);
        (first, last)
    }

    const LONG: &str = "yes, from ten. Shall I bring the T114 and the long antenna, \
                        or is the stick enough for the hill?";

    /// Ours end one cell short of the pane's right edge, theirs start one cell
    /// in from its left, both at most three quarters of the pane wide, and a
    /// line breaks at a space.
    #[test]
    fn blocks_sit_by_side_capped_at_three_quarters_and_wrapped_at_a_space() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 26, 14, 2);
        say(&mut model, LONG);
        receive(&mut model, PEER, 1, LONG);
        let width = model.sizes().messages.0;
        assert_eq!(width, 52, "the 80 x 24 mockup's pane");
        let cap = layout::block_cap(width);
        assert_eq!(cap, 37);
        let rows = rows(&model);
        let text_rows: Vec<&String> = rows
            .iter()
            .filter(|r| {
                !r.trim().is_empty()
                    && !r.contains(layout::EARLIEST.text(strings::Lang::En))
                    && !r.contains("──")
            })
            .collect();
        let (ours, theirs) = text_rows.split_at(text_rows.len() / 2);
        // The text within the cap; the time closing the last row may reach
        // past it, as long as the row keeps its margin cells.
        let body_rows = |rows: &[&String]| -> Vec<String> {
            rows.iter()
                .map(|r| r.replace("  14:02", "").replace(" ◷", ""))
                .collect()
        };
        for row in body_rows(ours) {
            let (first, last) = extent(&row);
            assert!(last - first < cap, "{row:?}");
        }
        for row in ours {
            assert!(extent(row).0 > 1, "{row:?}");
        }
        // The block's right edge: its widest row ends at the pane's last
        // cell but one.
        assert_eq!(
            ours.iter().map(|r| extent(r).1).max(),
            Some(width - 2),
            "{ours:?}"
        );
        // Left-aligned inside the block: every row of it starts in one column.
        let starts: Vec<usize> = ours.iter().map(|r| extent(r).0).collect();
        assert!(starts.windows(2).all(|w| w[0] == w[1]), "{starts:?}");
        for row in theirs {
            assert_eq!(extent(row).0, 1, "{row:?}");
            assert!(extent(row).1 < width - 1, "{row:?}");
        }
        for row in body_rows(theirs) {
            assert!(extent(&row).1 < cap + 1, "{row:?}");
        }
        // Each wrapped row ends at a word's end: the words come out whole.
        let words: Vec<&str> = theirs.iter().flat_map(|r| r.split_whitespace()).collect();
        let expected: Vec<&str> = LONG.split_whitespace().chain(["14:02"]).collect();
        assert_eq!(words, expected);
        assert!(ours
            .last()
            .is_some_and(|r| r.trim_end().ends_with("14:02 ◷")));
        assert!(theirs
            .last()
            .is_some_and(|r| r.trim_end().ends_with("14:02")));
    }

    /// A centred date before each day's first message: the day before and
    /// `Today`, across midnight.
    #[test]
    fn a_date_line_separates_the_days_across_midnight() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 25, 23, 59);
        receive(&mut model, PEER, 1, "late");
        clock(&mut model, 26, 0, 1);
        receive(&mut model, PEER, 2, "early");
        receive(&mut model, PEER, 3, "again");
        assert_eq!(
            shown(&model),
            [
                "── Sunday 25 October ──",
                "late  23:59",
                "── Today ──",
                "early  00:01",
                "again  00:01",
            ]
        );
        let rows = rows(&model);
        let date = rows.iter().find(|r| r.contains("Today")).expect("today");
        let (first, last) = extent(date);
        assert!(first.abs_diff(52 - 1 - last) <= 1, "centred: {date:?}");
    }

    /// A conversation that is not open collects its unread under a `2 new`
    /// rule, which is there when it is opened and stays until it is left.
    #[test]
    fn the_new_rule_marks_where_the_unread_begins_until_the_conversation_is_left() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        clock(&mut model, 26, 14, 0);
        say(&mut model, "to the first");
        receive(&mut model, OTHER, 1, "read later");
        receive(&mut model, OTHER, 2, "and this");
        assert_eq!(model.conversations[1].unread, 2);
        let list: Vec<String> = view(&model).conversations.iter().map(text).collect();
        assert!(
            list.iter()
                .any(|r| r.starts_with(" and this ") && r.trim_end().ends_with("[2]")),
            "{list:?}"
        );
        update(&mut model, key(KeyCode::Char('g')));
        assert_eq!(model.selected, 1);
        let rule = "───── 2 new ─────";
        let shown_now = shown(&model);
        let at = shown_now
            .iter()
            .position(|r| r.contains(" 2 new "))
            .expect("the rule");
        assert_eq!(shown_now[at + 1], "read later  14:00", "{shown_now:?}");
        assert!(shown_now[at].contains(rule.trim_matches('─')));
        // Full width: the margin cell either side, dashes to them.
        let row = rows(&model)
            .into_iter()
            .find(|r| r.contains(" 2 new "))
            .expect("rule");
        assert_eq!(extent(&row), (1, 50), "{row:?}");
        // It stays while the conversation is open, through a reply.
        say(&mut model, "thanks");
        assert!(shown(&model).iter().any(|r| r.contains(" 2 new ")));
        // Left, and back: gone.
        update(&mut model, key(KeyCode::Char('j')));
        update(&mut model, key(KeyCode::Char('g')));
        assert_eq!(model.selected, 1);
        assert!(!shown(&model).iter().any(|r| r.contains(" new ")));
    }

    /// A reply arriving while the reader is scrolled up draws the rule above
    /// it and does not move the view; going back to the newest follows again.
    #[test]
    fn a_reply_while_scrolled_up_draws_the_rule_and_does_not_jump() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 26, 9, 0);
        for id in 1..=12 {
            receive(&mut model, PEER, id, &format!("message {id}"));
        }
        update(&mut model, key(KeyCode::Tab));
        assert_eq!(model.focus, Pane::Messages);
        for _ in 0..8 {
            update(&mut model, key(KeyCode::Char('k')));
        }
        assert!(model.scroll_top.is_some(), "scrolled up");
        let before = rows(&model);
        receive(&mut model, PEER, 13, "a reply");
        assert_eq!(rows(&model), before, "the view did not move");
        assert_eq!(model.message_cursor, 3, "nor did the cursor");
        update(&mut model, key(KeyCode::Char('G')));
        assert!(model.scroll_top.is_none(), "following again");
        let now = shown(&model);
        let n = now.len();
        assert!(now[n - 2].contains(" 1 new "), "{now:?}");
        assert_eq!(now[n - 1], "▌a reply  09:00");
        // Following, a message arrives read: no rule of its own, the view
        // keeps the newest at the bottom.
        receive(&mut model, PEER, 14, "and another");
        let now = shown(&model);
        assert_eq!(now.last().map(String::as_str), Some("▌and another  09:00"));
        assert_eq!(now.iter().filter(|r| r.contains(" new ")).count(), 1);
    }

    /// The chat 1529 reads by the line: a short message, one over three
    /// screens long, a short one, the long one's lines its own rows.
    fn long_between_short() -> UiModel<Field> {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(
            &mut model,
            UiEvent::Resize {
                width: 80,
                height: 24,
            },
        );
        receive(&mut model, PEER, 1, "short before");
        let lines = 3 * model.message_rows() + 5;
        let long: Vec<String> = (1..=lines).map(|n| format!("line {n}")).collect();
        receive(&mut model, PEER, 2, &long.join("\n"));
        receive(&mut model, PEER, 3, "short after");
        update(&mut model, key(KeyCode::Tab));
        assert_eq!(model.focus, Pane::Messages);
        model
    }

    /// The view's first row of the open chat's `total` laid rows.
    fn top_now(model: &UiModel<Field>) -> usize {
        model.top_of(model.laid().rows.len())
    }

    /// 1529: Down from the top shows every laid row exactly once, never
    /// more than one new a press; the selection stays on the long message
    /// until its last line is on screen and moves to the short one under
    /// it at the press that brings that one's first line on; the way back
    /// up enters the long message at its end, a line a press, and a walk
    /// into its middle and back lands on the line it left. Before 1529
    /// Down jumped a message a press and skipped the long one's middle.
    #[test]
    fn a_chat_scrolls_by_the_line_and_the_selection_follows_the_lines() {
        let mut model = long_between_short();
        let laid = model.laid();
        let total = laid.rows.len();
        let rows = model.message_rows();
        let long = laid.spans[1];
        let after = laid.spans[2];
        assert!(long.end - long.start > 3 * rows, "{long:?} over {rows}");
        update(&mut model, key(KeyCode::Home));
        assert_eq!((top_now(&model), model.message_cursor), (0, 0));
        let mut seen = vec![0usize; total];
        seen[..rows].iter_mut().for_each(|n| *n += 1);
        let (mut moved_at, mut ready) = (None, None);
        for press in 1..=total + 5 {
            let (before, on) = (top_now(&model), model.message_cursor);
            update(&mut model, key(KeyCode::Down));
            let top = top_now(&model);
            assert!(top - before <= 1, "press {press}: {before} to {top}");
            seen[before + rows..top + rows]
                .iter_mut()
                .for_each(|n| *n += 1);
            if on == 1 && before + rows < long.end {
                assert_eq!(
                    model.message_cursor, 1,
                    "press {press}: its end not shown yet"
                );
            }
            // The first press that finds the long one ended and the short
            // one's first line on screen is the one that selects it.
            if on == 1
                && ready.is_none()
                && before + rows >= long.end
                && before + rows > after.block.0
            {
                ready = Some(press);
            }
            if model.message_cursor == 2 && moved_at.is_none() {
                moved_at = Some(press);
            }
        }
        assert!(seen.iter().all(|&n| n == 1), "{seen:?}");
        assert!(moved_at.is_some());
        assert_eq!(moved_at, ready, "the selection moved at the right press");
        assert!(
            model.scroll_top.is_none(),
            "following the newest at the end"
        );
        // Back up: the long one's end, not its start.
        update(&mut model, key(KeyCode::Up));
        let bottom = total - rows;
        assert_eq!((top_now(&model), model.message_cursor), (bottom, 1));
        update(&mut model, key(KeyCode::Up));
        assert_eq!((top_now(&model), model.message_cursor), (bottom - 1, 1));
        // Into the middle, on, and back: the same line.
        update(&mut model, key(KeyCode::Home));
        for _ in 0..rows + 3 {
            update(&mut model, key(KeyCode::Down));
        }
        let left = top_now(&model);
        assert!(left > long.start && left + rows < long.end, "mid: {left}");
        for _ in 0..4 {
            update(&mut model, key(KeyCode::Down));
        }
        for _ in 0..4 {
            update(&mut model, key(KeyCode::Up));
        }
        assert_eq!((top_now(&model), model.message_cursor), (left, 1));
    }

    /// 1529: Left and Right in the chat page a screen less a line, as the
    /// page keys do; at the top the first message is selected, at the
    /// bottom the newest and the pane follows. The list is untouched by
    /// the rule: there Down is the next entry.
    #[test]
    fn left_and_right_page_the_chat_and_the_list_stays_by_entry() {
        let mut model = long_between_short();
        let total = model.laid().rows.len();
        let rows = model.message_rows();
        update(&mut model, key(KeyCode::Home));
        update(&mut model, key(KeyCode::Right));
        assert_eq!((top_now(&model), model.message_cursor), (rows - 1, 1));
        update(&mut model, key(KeyCode::PageDown));
        assert_eq!(top_now(&model), 2 * (rows - 1), "the page key the same");
        update(&mut model, key(KeyCode::Left));
        assert_eq!((top_now(&model), model.message_cursor), (rows - 1, 1));
        update(&mut model, key(KeyCode::Left));
        assert_eq!((top_now(&model), model.message_cursor), (0, 0));
        for _ in 0..total / (rows - 1) + 1 {
            update(&mut model, key(KeyCode::Right));
        }
        assert_eq!(model.message_cursor, 2);
        assert!(model.scroll_top.is_none(), "following again");
        // The list: an entry a press, Left and Right no page.
        let mut list = UiModel::<Field>::new(&[PEER, OTHER, [0x5a; 16]]);
        for (i, peer) in [PEER, OTHER, [0x5a; 16]].into_iter().enumerate() {
            receive(&mut list, peer, 10 + i as u8, "hello");
        }
        list.focus = Pane::Conversations;
        let order = list.order();
        let at = order
            .iter()
            .position(|&c| c == list.selected)
            .expect("in order");
        update(&mut list, key(KeyCode::Down));
        let next = order
            .iter()
            .position(|&c| c == list.selected)
            .expect("in order");
        assert_eq!(next, (at + 1).min(order.len() - 1));
    }

    /// 1529: a message is read when its last line was on screen, not when
    /// half of it or a third of the pane showed: at the top of the long
    /// message the viewport does not name it, with its end on screen it
    /// does.
    #[test]
    fn the_long_message_is_read_when_its_last_line_is_on_screen() {
        let mut model = long_between_short();
        let key_of = |model: &UiModel<Field>, m: usize| {
            model.conversations[model.selected].messages[m].activity
        };
        let long = key_of(&model, 1);
        update(&mut model, key(KeyCode::Home));
        let shown = hit::viewport(&model).expect("a chat").1;
        assert!(shown.contains(&key_of(&model, 0)), "{shown:?}");
        assert!(!shown.contains(&long), "its start only: {shown:?}");
        let end = model.laid().spans[1].end;
        while top_now(&model) + model.message_rows() < end {
            update(&mut model, key(KeyCode::Down));
        }
        let shown = hit::viewport(&model).expect("a chat").1;
        assert!(shown.contains(&long), "its last line: {shown:?}");
    }

    /// A page is the pane's height less a line of overlap (1529) now that
    /// the model knows it.
    #[test]
    fn a_page_moves_the_view_by_the_panes_height() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        for id in 1..=40 {
            receive(&mut model, PEER, id, &format!("message {id}"));
        }
        update(&mut model, key(KeyCode::Tab));
        let height = model.message_rows();
        let total = model.laid().rows.len();
        update(&mut model, key(KeyCode::PageUp));
        assert_eq!(model.scroll_top, Some(total - 2 * height + 1));
        update(&mut model, key(KeyCode::PageDown));
        assert_eq!(model.scroll_top, None, "back at the bottom");
        update(
            &mut model,
            UiEvent::Resize {
                width: 80,
                height: 40,
            },
        );
        assert!(model.message_rows() > height);
    }

    /// Below 40 cells of pane ours go flush left with `you` before the time;
    /// at 40 they are flush right again.
    #[test]
    fn the_narrow_fallback_starts_below_forty_cells() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 26, 9, 12);
        say(&mut model, "on my way");
        update(
            &mut model,
            UiEvent::Resize {
                width: 67,
                height: 24,
            },
        );
        assert_eq!(model.sizes().messages.0, 39);
        let narrow = rows(&model)
            .into_iter()
            .find(|r| r.contains("on my way"))
            .expect("row");
        assert_eq!(extent(&narrow).0, 1, "{narrow:?}");
        assert_eq!(narrow.trim(), "on my way  you 09:12 ◷");
        update(
            &mut model,
            UiEvent::Resize {
                width: 68,
                height: 24,
            },
        );
        assert_eq!(model.sizes().messages.0, 40);
        let wide = rows(&model)
            .into_iter()
            .find(|r| r.contains("on my way"))
            .expect("row");
        // The glyph's column is two cells (1232): its padding takes 38.
        assert_eq!(extent(&wide).1, 37, "{wide:?}");
        assert_eq!(wide.trim(), "on my way  09:12 ◷");
    }

    /// The selected block, under focus, is reversed behind the bar; nothing
    /// is marked while another pane has the focus.
    #[test]
    fn the_selected_block_is_reversed_behind_the_bar_under_focus() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        receive(&mut model, PEER, 1, "one");
        receive(&mut model, PEER, 2, "two");
        let marked = |model: &UiModel<Field>| -> Vec<String> {
            view(model)
                .messages
                .iter()
                .filter(|r| r.cells.iter().any(|c| c.st.reverse))
                .map(text)
                .map(|r| r.trim().to_string())
                .collect()
        };
        assert!(marked(&model).is_empty());
        update(&mut model, key(KeyCode::Tab));
        assert_eq!(marked(&model), ["▌two"]);
        update(&mut model, key(KeyCode::Char('k')));
        assert_eq!(marked(&model), ["▌one"]);
    }

    /// The detail view of a sent message lists what this session saw: the
    /// queue, the leg, the router's states and the proof, with the times, the
    /// full id, the peer and the size; `q` closes it.
    #[test]
    fn the_detail_view_lists_the_ledger_with_the_proof() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 26, 14, 2);
        say(&mut model, "hello");
        update(&mut model, UiEvent::SendLeg(Leg::Direct));
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        clock(&mut model, 26, 14, 3);
        state(&mut model, ID, MessageState::Sending);
        update(&mut model, UiEvent::SendDone(Ok(())));
        state(&mut model, ID, MessageState::Delivered);
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Enter));
        let detail = view(&model).detail.expect("the detail view");
        assert_eq!(
            detail.rows,
            [
                "from you to Unknown",
                "peer   abababababababababababababababab",
                &format!("id     {}", "11".repeat(32)),
                "size   5 bytes",
                "state  arrived: Unknown's mailbox or device has it",
                // The peer never sent a message with the mark (1233).
                "Unknown sends no read receipts",
                "",
                "on this device",
                "14:02:00  queued",
                "14:02:00  trying direct",
                "14:02:00  queued by the router as 11111111",
                "14:03:00  in flight (direct)",
                "14:03:00  arrived: proof received (direct)",
            ]
        );
        update(&mut model, key(KeyCode::Char('q')));
        assert!(model.detail.is_none());
        assert_eq!(model.focus, Pane::Messages);
    }

    /// A detail view taller than a short screen scrolls by the shared scroll
    /// keys and the wheel, clamped at both ends and on a resize, from the top
    /// each time it opens (1095).
    #[test]
    fn the_detail_view_scrolls_on_a_short_screen() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(
            &mut model,
            UiEvent::Resize {
                width: 80,
                height: 10,
            },
        );
        clock(&mut model, 26, 14, 2);
        say(&mut model, "hello");
        update(&mut model, UiEvent::SendLeg(Leg::Direct));
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        update(&mut model, UiEvent::SendDone(Ok(())));
        state(&mut model, ID, MessageState::Delivered);
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Enter));
        let detail = view(&model).detail.expect("the detail view");
        let (_, max) = detail_scroll_bounds(&detail, Rect::new(0, 0, 80, 10));
        assert!(max > 1, "the detail view overflows 80 x 10");
        assert_eq!(model.detail_scroll, 0);
        update(&mut model, key(KeyCode::Up));
        assert_eq!(model.detail_scroll, 0, "clamped at the top");
        update(&mut model, key(KeyCode::Char('j')));
        update(&mut model, key(KeyCode::Down));
        assert_eq!(model.detail_scroll, 2);
        assert_eq!(view(&model).detail_scroll, 2);
        update(&mut model, key(KeyCode::Char('k')));
        assert_eq!(model.detail_scroll, 1);
        update(&mut model, key(KeyCode::End));
        assert_eq!(model.detail_scroll, max);
        update(&mut model, key(KeyCode::PageDown));
        assert_eq!(model.detail_scroll, max, "clamped at the bottom");
        update(&mut model, mouse(MouseEventKind::ScrollUp, 40, 5));
        assert_eq!(model.detail_scroll, max.saturating_sub(WHEEL_ROWS));
        update(&mut model, key(KeyCode::Home));
        assert_eq!(model.detail_scroll, 0);
        update(&mut model, mouse(MouseEventKind::ScrollDown, 40, 5));
        assert_eq!(model.detail_scroll, WHEEL_ROWS.min(max));
        assert!(model.detail_open(), "no scroll key closes it");
        update(&mut model, key(KeyCode::End));
        update(
            &mut model,
            UiEvent::Resize {
                width: 80,
                height: 40,
            },
        );
        assert_eq!(model.detail_scroll, 0, "all of it fits now");
        update(
            &mut model,
            UiEvent::Resize {
                width: 80,
                height: 10,
            },
        );
        update(&mut model, key(KeyCode::End));
        update(&mut model, key(KeyCode::Char('q')));
        update(&mut model, key(KeyCode::Enter));
        assert_eq!(model.detail_scroll, 0, "it opens at the top");
    }

    /// A failure names its reason in the ledger; a received message says
    /// whether its signature verified.
    #[test]
    fn the_ledger_keeps_a_failure_and_a_received_messages_signature() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        say(&mut model, "hi");
        update(&mut model, UiEvent::SendDone(Err("no route".to_string())));
        let ledger: Vec<&str> = model.conversations[0].messages[0]
            .ledger
            .iter()
            .map(|e| e.what.as_str())
            .collect();
        assert_eq!(ledger, ["queued", "not sent: no route"]);
        engine(
            &mut model,
            OutboxEvent::Received {
                sent_at: 0,
                message_id: ID2,
                source: PEER,
                title: Vec::new(),
                body: b"who?".to_vec(),
                fields: Vec::new(),
                verified: false,
            },
        );
        let them = &model.conversations[0].messages[1];
        assert_eq!(them.ledger[0].what, "received, signature NOT verified");
        assert_eq!(them.size, 4);
        assert!(shown(&model).contains(&"who?  unverified".to_string()));
    }

    /// A held message's verdict changes its block in place (leviculum
    /// 1079): a signature that holds drops `unverified`, one that does not
    /// says `INVALID signature`, and each says so in the ledger. A verdict
    /// for a message of another sender changes nothing.
    #[test]
    fn a_verdict_turns_an_unverified_message_verified_or_invalid() {
        const ID3: [u8; 32] = [0x33; 32];
        let mut model = UiModel::<Field>::new(&[PEER]);
        for (id, body) in [(ID2, b"one".as_slice()), (ID3, b"two".as_slice())] {
            engine(
                &mut model,
                OutboxEvent::Received {
                    sent_at: 0,
                    message_id: id,
                    source: PEER,
                    title: Vec::new(),
                    body: body.to_vec(),
                    fields: Vec::new(),
                    verified: false,
                },
            );
        }
        engine(
            &mut model,
            OutboxEvent::Verified {
                message_id: ID2,
                source: [0x77; 16],
                valid: true,
            },
        );
        assert_eq!(
            model.conversations[0].messages[0].signature,
            Signature::Unverified
        );
        engine(
            &mut model,
            OutboxEvent::Verified {
                message_id: ID2,
                source: PEER,
                valid: true,
            },
        );
        engine(
            &mut model,
            OutboxEvent::Verified {
                message_id: ID3,
                source: PEER,
                valid: false,
            },
        );
        let messages = &model.conversations[0].messages;
        assert_eq!(messages[0].signature, Signature::Verified);
        assert_eq!(
            messages[0].ledger.last().map(|e| e.what.as_str()),
            Some("signature verified, once the sender's key arrived")
        );
        assert_eq!(messages[1].signature, Signature::Invalid);
        assert_eq!(
            messages[1].ledger.last().map(|e| e.what.as_str()),
            Some("signature INVALID against the sender's key")
        );
        let lines = shown(&model);
        assert!(lines.contains(&"one".to_string()), "{lines:?}");
        assert!(
            lines.contains(&"two  INVALID signature".to_string()),
            "{lines:?}"
        );
    }

    /// The header names the peer, its trust and the propagation node, the
    /// longest form that fits; a wide pane gives the address and a row for
    /// the propagation node. Without one it says nothing until a send needs
    /// one (1279): then `no relay`.
    #[test]
    fn the_header_says_the_trust_and_the_mailbox_state() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        assert_eq!(view(&model).messages_title_right, "");
        sent(&mut model, "hi");
        assert_eq!(view(&model).messages_title_right, "no relay", "it waits");
        update(&mut model, UiEvent::SendDone(Err("no route".to_string())));
        assert_eq!(view(&model).messages_title_right, "no relay", "it failed");
        let mut arrived = UiModel::<Field>::new(&[PEER]);
        sent(&mut arrived, "hi");
        engine(&mut arrived, OutboxEvent::Queued { message_id: ID });
        state(&mut arrived, ID, MessageState::Delivered);
        assert_eq!(view(&arrived).messages_title_right, "", "it arrived");
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(
            &mut model,
            UiEvent::MailboxChosen {
                node: [0x9c; 16],
                by_user: false,
            },
        );
        assert_eq!(view(&model).messages_title_right, "a relay adopted");
        clock(&mut model, 26, 14, 3);
        update(&mut model, UiEvent::Run(PaletteCommand::Fetch));
        update(&mut model, UiEvent::FetchDone(Ok(0)));
        assert_eq!(
            view(&model).messages_title_right,
            "a relay adopted · synced 14:03"
        );
        update(
            &mut model,
            UiEvent::Resize {
                width: 120,
                height: 40,
            },
        );
        let wide = view(&model);
        // Wide, the right end names no address any more (1452).
        assert_eq!(wide.messages_title_right, "");
        assert_eq!(
            text(&wide.messages[0]).trim(),
            "a relay adopted · synced 14:03"
        );
        assert!(text(&wide.messages[1]).trim().chars().all(|c| c == '─'));
        assert_eq!(wide.compose_title, "to Unknown");
    }

    /// The header says the relay by the name its announce carries, "a
    /// relay" without one, in the reader's language; its hash never (1587).
    /// Another node's announce names nothing, a choice of another node
    /// forgets the name, and an announce without a name unnames it.
    #[test]
    fn the_header_says_the_relay_by_its_name_never_its_hash() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        let chosen = |model: &mut UiModel<Field>, node: [u8; 16]| {
            update(
                model,
                UiEvent::MailboxChosen {
                    node,
                    by_user: false,
                },
            );
        };
        let announced = |model: &mut UiModel<Field>, node: [u8; 16], name: Option<&str>| {
            engine_says(
                model,
                OutboxEvent::NodeAnnounced {
                    destination: node,
                    status: None,
                    name: name.map(str::to_string),
                },
            );
        };
        let right = |model: &UiModel<Field>| view(model).messages_title_right;
        chosen(&mut model, [0x9c; 16]);
        assert_eq!(right(&model), "a relay adopted");
        announced(&mut model, [0x42; 16], Some("Elsewhere"));
        assert_eq!(right(&model), "a relay adopted", "another node's name");
        announced(&mut model, [0x9c; 16], Some("Bern\u{7}"));
        assert_eq!(right(&model), "relay Bern adopted", "sanitised");
        assert!(!right(&model).contains("9c9c"));
        model.lang = strings::Lang::De;
        assert_eq!(right(&model), "Relais Bern adopted");
        model.lang = strings::Lang::En;
        chosen(&mut model, [0x9c; 16]);
        assert_eq!(right(&model), "relay Bern adopted", "the same node");
        chosen(&mut model, [0x77; 16]);
        assert_eq!(right(&model), "a relay adopted", "another node");
        model.lang = strings::Lang::De;
        assert_eq!(right(&model), "ein Relais adopted");
        model.lang = strings::Lang::En;
        announced(&mut model, [0x77; 16], Some("Zug"));
        assert_eq!(right(&model), "relay Zug adopted");
        announced(&mut model, [0x77; 16], None);
        assert_eq!(right(&model), "a relay adopted", "unnamed");
    }

    fn engine_says(model: &mut UiModel<Field>, event: OutboxEvent) -> Vec<UiEffect> {
        update(model, UiEvent::Engine(event))
    }

    /// The mailbox schedule through the model: the engine's `Ready` starts
    /// the first sync, the header names the next one, an empty automatic
    /// sync says nothing, a full one says what came, and the timer starts
    /// the next sync without a key.
    #[test]
    fn the_schedule_syncs_at_start_and_on_its_timer_and_the_header_says_when() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.sync = Some(Schedule::new(Some(crate::sync::DEFAULT_BOUND_MS)));
        update(
            &mut model,
            UiEvent::MailboxChosen {
                node: [0x9c; 16],
                by_user: false,
            },
        );
        clock(&mut model, 26, 14, 5);
        let effects = engine_says(
            &mut model,
            OutboxEvent::Ready {
                address: [0xee; 16],
            },
        );
        assert_eq!(effects, [UiEffect::Fetch], "start syncs");
        assert_eq!(model.fetch_reason, Some(Reason::Start));
        assert_eq!(
            view(&model).messages_title_right,
            "a relay adopted · fetching"
        );
        update(&mut model, UiEvent::Elapsed(1_000));
        assert!(
            update(&mut model, UiEvent::Run(PaletteCommand::Fetch)).is_empty(),
            "one at a time"
        );
        let toasts = model.toasts.len();
        update(&mut model, UiEvent::FetchDone(Ok(0)));
        assert_eq!(
            model.toasts.len(),
            toasts,
            "an empty automatic sync is quiet"
        );
        // At 80 columns only the next sync fits: it is kept before the
        // last one and the node.
        assert_eq!(
            view(&model).messages_title_right,
            "pn · synced 14:05 · next 14:20"
        );
        update(
            &mut model,
            UiEvent::Resize {
                width: 120,
                height: 40,
            },
        );
        assert_eq!(
            text(&view(&model).messages[0]).trim(),
            "a relay adopted · synced 14:05 · next 14:20"
        );
        assert!(update(&mut model, UiEvent::Elapsed(899_999)).is_empty());
        let effects = update(&mut model, UiEvent::Elapsed(900_000));
        assert_eq!(effects, [UiEffect::Fetch], "the bound, without a key");
        assert_eq!(model.fetch_reason, Some(Reason::Timer));
        update(&mut model, UiEvent::Elapsed(901_000));
        update(&mut model, UiEvent::FetchDone(Ok(2)));
        assert_eq!(
            model.toasts.last().map(|t| t.text.as_str()),
            Some("2 new message(s)")
        );
        // Activity: two minutes, not fifteen.
        assert!(update(&mut model, UiEvent::Elapsed(1_020_999)).is_empty());
        assert_eq!(
            update(&mut model, UiEvent::Elapsed(1_021_000)),
            [UiEffect::Fetch]
        );
    }

    /// An announce of the selected node syncs; another node's does not.
    #[test]
    fn the_mailbox_announcing_syncs_and_another_node_does_not() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.sync = Some(Schedule::new(Some(crate::sync::DEFAULT_BOUND_MS)));
        update(
            &mut model,
            UiEvent::MailboxChosen {
                node: [0x9c; 16],
                by_user: true,
            },
        );
        engine_says(
            &mut model,
            OutboxEvent::Ready {
                address: [0xee; 16],
            },
        );
        update(&mut model, UiEvent::Elapsed(1_000));
        update(&mut model, UiEvent::FetchDone(Ok(0)));
        update(&mut model, UiEvent::Elapsed(120_000));
        let other = OutboxEvent::NodeAnnounced {
            destination: [0x42; 16],
            status: None,
            name: None,
        };
        assert!(engine_says(&mut model, other).is_empty());
        let ours = OutboxEvent::NodeAnnounced {
            destination: [0x9c; 16],
            status: None,
            name: None,
        };
        assert_eq!(engine_says(&mut model, ours), [UiEffect::Fetch]);
        assert_eq!(model.fetch_reason, Some(Reason::Announce));
    }

    /// A failed automatic sync says so once, not on every retry; one the
    /// reader asked for always says so.
    #[test]
    fn automatic_failures_are_said_once_in_a_row() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.sync = Some(Schedule::new(Some(crate::sync::DEFAULT_BOUND_MS)));
        engine_says(
            &mut model,
            OutboxEvent::Ready {
                address: [0xee; 16],
            },
        );
        let first = update(&mut model, UiEvent::FetchDone(Err("no route".into())));
        assert_eq!(first, [UiEffect::Notice("fetch: no route".into())]);
        update(&mut model, UiEvent::Elapsed(30_000));
        assert_eq!(model.fetch_reason, Some(Reason::Timer), "the retry");
        let second = update(&mut model, UiEvent::FetchDone(Err("no route".into())));
        assert!(second.is_empty(), "{second:?}");
        update(&mut model, UiEvent::Run(PaletteCommand::Fetch));
        let asked = update(&mut model, UiEvent::FetchDone(Err("no route".into())));
        assert_eq!(asked, [UiEffect::Notice("fetch: no route".into())]);
    }

    /// Without a schedule nothing syncs by itself: what every key script
    /// without `--sync-interval` relies on.
    #[test]
    fn without_a_schedule_only_the_key_syncs() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        assert!(engine_says(
            &mut model,
            OutboxEvent::Ready {
                address: [0xee; 16]
            }
        )
        .is_empty());
        assert!(update(&mut model, UiEvent::Elapsed(24 * 3_600_000)).is_empty());
        assert!(update(&mut model, UiEvent::Resumed).is_empty());
    }

    // The mouse (section 5, "What the mouse does"), fed to the model as the
    // terminal sends it: cells from 0, the left button down and up.

    fn mouse(kind: MouseEventKind, col: u16, row: u16) -> UiEvent {
        UiEvent::Mouse(MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    /// The drafts' writes among `writes` (1219).
    fn draft_writes(writes: &[StoreWrite]) -> Vec<([u8; 16], String)> {
        writes
            .iter()
            .filter_map(|w| match w {
                StoreWrite::Draft { peer, draft } => Some((*peer, draft.text.clone())),
                _ => None,
            })
            .collect()
    }

    /// 1219: what is typed in one conversation stays there when another
    /// opens, its row says `Draft: …` in italics, nothing counts it unread
    /// or moves the list, and it is back in compose when its conversation
    /// opens again.
    #[test]
    fn a_draft_stays_with_its_conversation() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        model.focus = Pane::Compose;
        typed(&mut model, "half a thought");
        let order = model.order();
        assert_eq!(
            chat::list_row(&model, &PEER).draft,
            None,
            "the open one's row shows none"
        );
        update(&mut model, UiEvent::Open { peer: OTHER });
        assert_eq!(model.compose.value(), "", "the other's compose is its own");
        let row = chat::list_row(&model, &PEER);
        assert_eq!(row.draft.as_deref(), Some("half a thought"));
        assert_eq!(row.unread, 0);
        assert_eq!(model.order(), order, "a draft moves nothing in the list");
        let scene = model.scene(scene::LayoutClass::Wide);
        let tabs = scene.tabs.as_ref().expect("the tabs");
        assert_eq!(tabs.items[0].badge, None, "and counts as nothing unread");
        let srow = scene::row(&model, &PEER);
        let words = srow.draft.as_ref().expect("the scene's draft").clone();
        assert_eq!(words.resolve(strings::Lang::En), "Draft: half a thought");
        assert_eq!(words.resolve(strings::Lang::De), "Entwurf: half a thought");
        // The terminal's row in italics, the open one's plain.
        let rows = layout::list(&model, 30, 12);
        let cells = |r: &RLine| r.cells.iter().map(|c| c.ch).collect::<String>();
        let at = rows
            .iter()
            .position(|r| cells(r).contains("Draft: half a thought"))
            .expect("the terminal's draft row");
        assert!(rows[at]
            .cells
            .iter()
            .filter(|c| c.ch != ' ')
            .all(|c| c.st.italic));
        assert!(
            rows.iter()
                .filter(|r| r.cells.iter().any(|c| c.st.italic))
                .count()
                == 1
        );

        update(&mut model, UiEvent::Open { peer: PEER });
        assert_eq!(model.compose.value(), "half a thought");
        assert_eq!(
            model
                .scene(scene::LayoutClass::Wide)
                .chat
                .expect("open")
                .compose
                .text,
            "half a thought"
        );
        assert_eq!(chat::list_row(&model, &PEER).draft, None);
    }

    /// 1219: a draft is written once typing paused for half a second,
    /// leaving its conversation writes it at once, quitting writes every
    /// one, and a send deletes it at once.
    #[test]
    fn a_draft_is_written_once_typing_pauses_and_goes_with_the_send() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        update(&mut model, UiEvent::Elapsed(1_000));
        model.take_writes();
        update(
            &mut model,
            UiEvent::Draft {
                peer: PEER,
                text: "on my".to_string(),
                reply: Some(ID),
            },
        );
        assert_eq!(
            model.compose.value(),
            "on my",
            "the open one's compose follows"
        );
        assert!(draft_writes(&model.take_writes()).is_empty(), "not yet");
        update(&mut model, UiEvent::Elapsed(1_300));
        update(
            &mut model,
            UiEvent::Draft {
                peer: PEER,
                text: "on my way".to_string(),
                reply: Some(ID),
            },
        );
        update(&mut model, UiEvent::Elapsed(1_799));
        assert!(
            draft_writes(&model.take_writes()).is_empty(),
            "each key moves it on"
        );
        update(&mut model, UiEvent::Elapsed(1_800));
        let writes = model.take_writes();
        assert_eq!(draft_writes(&writes), [(PEER, "on my way".to_string())]);
        assert!(writes.contains(&StoreWrite::Draft {
            peer: PEER,
            draft: history::Draft {
                text: "on my way".to_string(),
                reply: Some(ID),
                attachments: Vec::new(),
            }
        }));
        assert!(model.take_writes().is_empty(), "written once");

        // Leaving writes at once.
        typed(&mut model, "!");
        assert_eq!(model.conversations[model.selected].draft.text, "on my way");
        model.focus = Pane::Compose;
        typed(&mut model, "!");
        assert!(draft_writes(&model.take_writes()).is_empty());
        update(&mut model, UiEvent::Close);
        assert_eq!(
            draft_writes(&model.take_writes()),
            [(PEER, "on my way!".to_string())]
        );

        // Quitting writes what typing has not paused on.
        update(
            &mut model,
            UiEvent::Draft {
                peer: OTHER,
                text: "later".to_string(),
                reply: None,
            },
        );
        assert!(draft_writes(&model.take_writes()).is_empty());
        model.flush_drafts();
        assert_eq!(
            draft_writes(&model.take_writes()),
            [(OTHER, "later".to_string())]
        );

        // The send takes the draft with it, from the store at once.
        let effects = update(
            &mut model,
            UiEvent::Write {
                peer: PEER,
                body: "on my way!".to_string(),
            },
        );
        assert_eq!(effects.len(), 1);
        assert!(model.conversations[model.selected].draft.is_empty());
        assert_eq!(model.compose.value(), "", "compose empties with it");
        assert_eq!(draft_writes(&model.take_writes()), [(PEER, String::new())]);
        // A reply sent does so too.
        update(
            &mut model,
            UiEvent::Draft {
                peer: OTHER,
                text: "sure".to_string(),
                reply: Some(ID),
            },
        );
        update(
            &mut model,
            UiEvent::Reply {
                peer: OTHER,
                target: ID,
                body: "sure".to_string(),
            },
        );
        let c = model
            .conversations
            .iter()
            .find(|c| c.peer == OTHER)
            .expect("other");
        assert!(c.draft.is_empty());
        assert_eq!(draft_writes(&model.take_writes()), [(OTHER, String::new())]);
    }

    /// 1219: the terminal's own send empties the draft as it empties
    /// compose; a conversation restored with one opens with it in compose.
    #[test]
    fn the_terminal_sends_its_draft_and_a_restore_brings_it_back() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.focus = Pane::Compose;
        typed(&mut model, "hello");
        assert_eq!(model.conversations[0].draft.text, "hello");
        let effects = update(&mut model, key(KeyCode::Enter));
        assert!(matches!(effects.as_slice(), [UiEffect::Send { .. }]));
        assert!(model.conversations[0].draft.is_empty());
        assert_eq!(model.compose.value(), "");

        let mut again = UiModel::<Field>::new(&[]);
        let mut stored = restored(OTHER, Vec::new(), 0);
        stored.draft.text = "kept".to_string();
        again.restore(vec![stored]);
        assert_eq!(again.compose.value(), "kept", "in compose before any key");
        again.focus = Pane::Compose;
        typed(&mut again, "!");
        assert_eq!(again.conversations[again.selected].draft.text, "kept!");
    }

    fn press(model: &mut UiModel<Field>, (col, row): (u16, u16)) -> Vec<UiEffect> {
        let mut effects = update(
            model,
            mouse(MouseEventKind::Down(MouseButton::Left), col, row),
        );
        effects.extend(update(
            model,
            mouse(MouseEventKind::Up(MouseButton::Left), col, row),
        ));
        effects
    }

    /// Every cell of the screen whose target `want` takes, in reading order.
    fn cells(model: &UiModel<Field>, want: impl Fn(&hit::Target) -> bool) -> Vec<(u16, u16)> {
        let (width, height) = model.screen;
        (0..height)
            .flat_map(|row| (0..width).map(move |col| (col, row)))
            .filter(|&(col, row)| want(&hit::target_at(model, col, row)))
            .collect()
    }

    /// The screen cell of the `nth` character of the first conversation row
    /// holding `needle`.
    fn cell_of(model: &UiModel<Field>, needle: &str, nth: usize) -> (u16, u16) {
        let screen = hit::screen(model);
        let (row, col) = rows(model)
            .iter()
            .enumerate()
            .find_map(|(r, line)| {
                line.find(needle)
                    .map(|byte| (r, line[..byte].chars().count() + nth))
            })
            .unwrap_or_else(|| panic!("{needle:?} not on screen: {:#?}", rows(model)));
        (
            screen.messages.x + 1 + col as u16,
            screen.messages.y + 1 + row as u16,
        )
    }

    /// A model with one conversation of `n` short messages from the peer.
    fn chatty(n: u8) -> UiModel<Field> {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        clock(&mut model, 26, 12, 0);
        for id in 1..=n {
            receive(&mut model, PEER, id, &format!("message {id}"));
        }
        model.toasts.clear();
        model
    }

    #[test]
    fn a_click_focuses_its_pane_and_the_wheel_scrolls_the_one_under_the_pointer() {
        let mut model = chatty(15);
        assert_eq!(model.focus, Pane::Conversations);
        let screen = hit::screen(&model);
        let over_messages = (screen.messages.x + 10, screen.messages.y + 5);
        update(
            &mut model,
            mouse(MouseEventKind::ScrollUp, over_messages.0, over_messages.1),
        );
        // The conversation moved, the focus and the list did not.
        let total = model.laid().rows.len();
        let bottom = total - model.message_rows();
        assert_eq!(model.scroll_top, Some(bottom - WHEEL_ROWS));
        assert_eq!(model.focus, Pane::Conversations);
        assert_eq!(model.selected, 0);
        update(
            &mut model,
            mouse(MouseEventKind::ScrollDown, over_messages.0, over_messages.1),
        );
        assert_eq!(model.scroll_top, None, "back at the newest");
        // The wheel over the list steps the conversation, whatever has the
        // focus.
        model.focus = Pane::Compose;
        update(&mut model, mouse(MouseEventKind::ScrollDown, 3, 5));
        assert_eq!(model.selected, 1);
        assert_eq!(model.focus, Pane::Compose);
        // A click on a pane's border focuses it.
        press(&mut model, (screen.messages.x, screen.messages.y + 3));
        assert_eq!(model.focus, Pane::Messages);
        press(&mut model, (screen.compose.x, screen.compose.y + 1));
        assert_eq!(model.focus, Pane::Compose);
        press(&mut model, (0, 10));
        assert_eq!(model.focus, Pane::Conversations);
    }

    #[test]
    fn a_click_on_a_row_opens_it_and_one_on_the_badge_opens_it_at_its_new_rule() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        clock(&mut model, 26, 12, 0);
        // Read with OTHER, then left it for PEER, and two arrive from OTHER.
        model.selected = 1;
        for id in 1..=12 {
            receive(&mut model, OTHER, id, &format!("old {id}"));
        }
        update(&mut model, UiEvent::Run(PaletteCommand::NextConversation));
        let at_peer = cells(&model, |t| *t == hit::Target::Conversation(0));
        press(&mut model, at_peer[0]);
        assert_eq!((model.selected, model.focus), (0, Pane::Conversations));
        for id in 13..=24 {
            receive(&mut model, OTHER, id, &format!("new {id}"));
        }
        assert_eq!(model.conversations[1].unread, 12);
        let badge = cells(&model, |t| *t == hit::Target::Badge(1));
        assert_eq!(badge.len(), "[12]".len(), "{badge:?}");
        assert!(badge
            .windows(2)
            .all(|w| w[1].0 == w[0].0 + 1 && w[1].1 == w[0].1));
        press(&mut model, badge[1]);
        assert_eq!(model.selected, 1);
        assert_eq!(model.focus, Pane::Messages);
        assert_eq!(model.conversations[1].unread, 0);
        assert_eq!(model.message_cursor, 12, "the first unread message");
        let shown = rows(&model);
        assert!(shown[0].contains(" 12 new "), "{shown:#?}");
        assert!(shown[1].contains("new 13"), "{shown:#?}");
    }

    #[test]
    fn a_click_selects_a_message_and_one_on_its_mark_opens_the_detail_view() {
        let mut model = chatty(15);
        say(&mut model, "the newest, ours");
        let mark = cells(&model, |t| {
            matches!(t, hit::Target::Block { mark: true, .. })
        });
        assert_eq!(mark.len(), 2, "two cells, the glyph's column: {mark:?}");
        // A message up the screen, selected where it is: the view stays.
        let older = cell_of(&model, "message 10", 2);
        press(&mut model, older);
        assert_eq!(model.focus, Pane::Messages);
        assert_eq!(model.message_cursor, 9);
        assert!(rows(&model).iter().any(|r| r.contains("message 10")));
        assert!(rows(&model).iter().any(|r| r.contains("the newest, ours")));
        assert_eq!(model.detail, None);
        press(&mut model, mark[0]);
        assert_eq!(model.detail, Some((0, 15)));
        // Inside the detail view a click does nothing, outside it closes.
        let area = hit::screen(&model).area();
        let detail = detail_view(&model, 0, 15).expect("a detail view");
        let (rect, _) = detail_layout(&detail, area);
        press(&mut model, (rect.x + 2, rect.y + 1));
        assert_eq!(model.detail, Some((0, 15)));
        press(&mut model, (0, 0));
        assert_eq!(model.detail, None);
    }

    #[test]
    fn a_click_in_compose_places_the_cursor_at_its_cell() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.focus = Pane::Compose;
        typed(&mut model, "first line");
        model.compose.newline();
        typed(&mut model, "second");
        model.focus = Pane::Conversations;
        let screen = hit::screen(&model);
        let (x, y) = (screen.compose.x + 1, screen.compose.y + 1);
        press(&mut model, (x + 3, y + 1));
        assert_eq!(model.focus, Pane::Compose);
        assert_eq!(model.compose.cursor(), "first line\n".len() + 3);
        press(&mut model, (x + 30, y));
        assert_eq!(model.compose.cursor(), "first line".len(), "the line's end");
    }

    /// The quiet keyboard holds for the mouse: of every cell on the screen,
    /// in every pane's focus and at both mockup sizes, the only clicks that
    /// transmit are on the word `send` on the compose box's border and on
    /// the mailbox state in the header, and each of them does.
    #[test]
    fn the_only_clicks_that_transmit_are_send_and_the_mailbox_state() {
        let mut seen = Vec::new();
        for (width, height) in [(80, 24), (120, 40)] {
            for focus in [Pane::Conversations, Pane::Messages, Pane::Compose] {
                let mut model = chatty(4);
                update(&mut model, UiEvent::Resize { width, height });
                update(
                    &mut model,
                    UiEvent::MailboxChosen {
                        node: [0x9c; 16],
                        by_user: false,
                    },
                );
                say(&mut model, "see https://example.org/x");
                model.focus = Pane::Compose;
                typed(&mut model, "a draft");
                model.focus = focus;
                for row in 0..height {
                    for col in 0..width {
                        let target = hit::target_at(&model, col, row);
                        let mut clicked = model.clone();
                        let effects = press(&mut clicked, (col, row));
                        let tx = effects
                            .iter()
                            .any(|e| matches!(e, UiEffect::Send { .. } | UiEffect::Fetch));
                        if tx && !seen.contains(&target) {
                            seen.push(target.clone());
                        }
                        match target {
                            hit::Target::Send | hit::Target::Mailbox => {
                                assert!(tx, "{target:?} at {col},{row} did not transmit")
                            }
                            _ => assert!(!tx, "{target:?} at {col},{row} transmitted"),
                        }
                    }
                }
            }
        }
        assert_eq!(seen, [hit::Target::Mailbox, hit::Target::Send]);
    }

    #[test]
    fn a_footer_click_runs_its_row_and_a_hover_marks_it() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        let screen = hit::screen(&model);
        let slots = leviculum_ui::footer::layout(view(&model).footer, screen.footer);
        let help = slots
            .iter()
            .find(|slot| slot.item.label == "help")
            .expect("? help in the list's footer");
        update(
            &mut model,
            mouse(MouseEventKind::Moved, help.rect.x + 1, help.rect.y),
        );
        assert_eq!(model.footer_hover, Some(help.index));
        assert_eq!(view(&model).footer_hover, Some(help.index));
        update(&mut model, mouse(MouseEventKind::Moved, 5, 5));
        assert_eq!(model.footer_hover, None);
        press(&mut model, (help.rect.x, help.rect.y));
        assert!(model.help);
        // The help fills an 80 x 24 screen, so any click closes it, as in
        // lnomad.
        press(&mut model, (40, 12));
        assert!(!model.help);
        // `send` in compose's footer is left to the border's word.
        model.focus = Pane::Compose;
        let slots = leviculum_ui::footer::layout(view(&model).footer, screen.footer);
        let send = slots
            .iter()
            .find(|slot| slot.item.label == "send")
            .expect("Enter send in compose's footer");
        assert_eq!(send.item.action, FooterAct::Row(None));
        update(
            &mut model,
            mouse(MouseEventKind::Moved, send.rect.x, send.rect.y),
        );
        assert_eq!(model.footer_hover, None);
    }

    #[test]
    fn a_drag_copies_the_text_of_the_blocks_without_times_and_marks() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 26, 12, 0);
        receive(&mut model, PEER, 1, "the quick brown fox");
        say(&mut model, "jumps over\nthe lazy dog");
        model.toasts.clear();
        let from = cell_of(&model, "the quick", 0);
        let to = cell_of(&model, "the lazy dog", "the lazy dog".len() - 1);
        update(
            &mut model,
            mouse(MouseEventKind::Down(MouseButton::Left), from.0, from.1),
        );
        update(
            &mut model,
            mouse(MouseEventKind::Drag(MouseButton::Left), to.0, to.1),
        );
        let effects = update(
            &mut model,
            mouse(MouseEventKind::Up(MouseButton::Left), to.0, to.1),
        );
        let text = "the quick brown fox\njumps over\nthe lazy dog";
        assert_eq!(effects, [UiEffect::Copy(text.to_string())]);
        assert_eq!(
            model.current_toast().map(|t| t.text.as_str()),
            Some("copied 43 chars")
        );
        // The selection is drawn in inverse video against the block, which
        // the focus reversed: the time beside it stays as the block is.
        let line = view(&model)
            .messages
            .into_iter()
            .find(|l| text_of(l).contains("the quick"))
            .expect("the row");
        let quick: Vec<bool> = line
            .cells
            .iter()
            .filter(|c| c.ch != ' ' && c.ch != layout::BAR)
            .map(|c| c.st.reverse)
            .collect();
        assert!(quick[..16].iter().all(|r| *r != quick[16]), "{quick:?}");
        assert!(
            quick[16..].iter().all(|r| *r == quick[16]),
            "the time: {quick:?}"
        );
        // `y` copies the selection again; with none, `Alt-w` copies the
        // message under the cursor whole, the one the drag began on.
        assert_eq!(
            update(&mut model, key(KeyCode::Char('y'))),
            [UiEffect::Copy(text.to_string())]
        );
        assert_eq!(
            update(&mut model, alt_key('w')),
            [UiEffect::Copy("the quick brown fox".to_string())]
        );
        // Part of a line: from inside one word to inside the next.
        let from = cell_of(&model, "quick", 1);
        let to = cell_of(&model, "brown", 2);
        update(
            &mut model,
            mouse(MouseEventKind::Down(MouseButton::Left), from.0, from.1),
        );
        update(
            &mut model,
            mouse(MouseEventKind::Drag(MouseButton::Left), to.0, to.1),
        );
        let effects = update(
            &mut model,
            mouse(MouseEventKind::Up(MouseButton::Left), to.0, to.1),
        );
        assert_eq!(effects, [UiEffect::Copy("uick bro".to_string())]);
    }

    fn text_of(line: &RLine) -> String {
        line.cells.iter().map(|c| c.ch).collect()
    }

    #[test]
    fn a_click_on_a_link_follows_it_as_lnomad_does() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        let other = to_hex(&OTHER);
        receive(
            &mut model,
            PEER,
            1,
            &format!("see https://example.org/a. or write lxmf@{other}"),
        );
        let url = cell_of(&model, "https://", 3);
        assert_eq!(
            press(&mut model, url),
            [UiEffect::Open("https://example.org/a".to_string())]
        );
        let lxmf = cell_of(&model, "lxmf@", 0);
        assert_eq!(press(&mut model, lxmf), []);
        assert_eq!(model.conversations[model.selected].peer, OTHER);
        assert_eq!(model.focus, Pane::Compose);
    }

    #[test]
    fn a_page_link_in_a_chat_opens_the_browse_tab_where_the_host_has_one() {
        let node = to_hex(&OTHER);
        let body = format!("my node: {node}:/page/index.mu");
        // The terminal has no Browse tab: the link is copied for lnomad
        // (the control).
        let mut model = UiModel::<Field>::new(&[PEER]);
        receive(&mut model, PEER, 1, &body);
        let link = cell_of(&model, &node[..8], 0);
        assert_eq!(
            press(&mut model, link),
            [UiEffect::Copy(format!("{node}:/page/index.mu"))]
        );
        assert!(!model.browse.open);
        // The phone has one: the tab comes to the front at that page.
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.browse.enabled = true;
        receive(&mut model, PEER, 1, &body);
        let link = cell_of(&model, &node[..8], 0);
        assert_eq!(
            press(&mut model, link),
            [UiEffect::Browse(lnomad::Effect::Navigate(lnomad::Target {
                dest_hash: OTHER,
                path: "/page/index.mu".to_string(),
                fields: Vec::new(),
                is_file: false,
            }))]
        );
        assert!(model.browse.open);
        assert_eq!(
            model.scene(scene::LayoutClass::Compact).screen,
            scene::ScreenId::Browse
        );
    }

    #[test]
    fn the_mouse_switch_turns_capture_off_and_the_model_deaf_to_it() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        assert_eq!(
            update(&mut model, UiEvent::Run(PaletteCommand::Mouse)),
            [UiEffect::Mouse(false)]
        );
        assert!(!model.mouse);
        let screen = hit::screen(&model);
        press(&mut model, (screen.compose.x + 2, screen.compose.y + 1));
        assert_eq!(model.focus, Pane::Conversations);
        assert_eq!(
            update(&mut model, UiEvent::Run(PaletteCommand::Mouse)),
            [UiEffect::Mouse(true)]
        );
        press(&mut model, (screen.compose.x + 2, screen.compose.y + 1));
        assert_eq!(model.focus, Pane::Compose);
    }

    // The search (slice 4).

    /// The list pane's rows as text, trailing blanks cut.
    fn list_rows(model: &UiModel<Field>) -> Vec<String> {
        view(model)
            .conversations
            .iter()
            .map(|line| text(line).trim_end().to_string())
            .collect()
    }

    /// A model of two conversations: PEER with three messages, two of them
    /// with a `tripod`, OTHER with one, written in this order, so the list
    /// has OTHER on top.
    fn searchable() -> UiModel<Field> {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        update(
            &mut model,
            UiEvent::Resize {
                width: 120,
                height: 40,
            },
        );
        clock(&mut model, 26, 14, 0);
        receive(&mut model, PEER, 1, "did the board come back?");
        clock(&mut model, 26, 14, 5);
        receive(
            &mut model,
            PEER,
            2,
            "and the small tripod\nif it fits in the bag",
        );
        clock(&mut model, 26, 14, 6);
        say(&mut model, "the Tripod fits");
        update(&mut model, key(KeyCode::Char('j')));
        clock(&mut model, 26, 14, 7);
        say(&mut model, "the tripod stays in the van");
        model.toasts.clear();
        model
    }

    fn search_for(model: &mut UiModel<Field>, query: &str) {
        model.focus = Pane::Conversations;
        update(model, key(KeyCode::Char('/')));
        assert_eq!(model.focus, Pane::Search);
        typed(model, query);
    }

    /// Conversations by name first, then messages, each most recent first;
    /// the match is case-blind and a message whose conversation matches by
    /// name is in both groups.
    #[test]
    fn the_matcher_groups_conversations_then_messages_most_recent_first() {
        let mut model = searchable();
        let tripod = search::hits(
            &model.conversations,
            &model.names,
            &model.order(),
            "tripod",
            &[],
        );
        let m = |conversation, message| search::Hit::Message {
            conversation,
            message,
        };
        assert_eq!(tripod, [m(1, 0), m(0, 2), m(0, 1)]);
        assert_eq!(
            search::hits(
                &model.conversations,
                &model.names,
                &model.order(),
                "TRIPOD",
                &[]
            ),
            tripod
        );
        // `abab` is PEER's name, and one of our messages says it.
        say(&mut model, "ask abababab about it");
        let by_name = search::hits(
            &model.conversations,
            &model.names,
            &model.order(),
            "ABAB",
            &[],
        );
        assert_eq!(by_name, [search::Hit::Conversation(0), m(1, 1)]);
        assert!(
            search::hits(&model.conversations, &model.names, &model.order(), "", &[]).is_empty()
        );
        assert!(search::hits(
            &model.conversations,
            &model.names,
            &model.order(),
            "drone",
            &[]
        )
        .is_empty());
    }

    #[test]
    fn find_and_mark_ignore_case_and_mark_the_first_match_only() {
        assert_eq!(search::find("The Tripod", "tripod"), Some((4, 6)));
        assert_eq!(search::find("Ärger", "ä"), Some((0, 1)));
        assert_eq!(search::find("tripod", ""), None);
        assert_eq!(search::find("pod", "tripod"), None);
        assert_eq!(
            search::mark_first("tripod, and a tripod again", "TRIPOD"),
            "«tripod», and a tripod again"
        );
        assert_eq!(search::mark_first("no match", "x"), "no match");
    }

    /// A preview is the body on one line, the first match marked, and cut so
    /// the match stays in view.
    #[test]
    fn a_preview_joins_the_lines_and_keeps_the_match_in_view() {
        let body = "and the small tripod\nif it fits in the bag";
        assert_eq!(
            search::preview(body, "tripod", 29),
            "and the small «tripod» if it…"
        );
        // Past the room: the start goes.
        assert_eq!(search::preview(body, "bag", 24), "…if it fits in the «bag»");
        assert_eq!(search::preview(body, "fits", 18), "…pod if it «fits»…");
        let short = search::preview(body, "fits", 24);
        assert!(short.contains("«fits»"), "{short}");
        assert_eq!(short.chars().count(), 24, "{short}");
    }

    /// The search as the list shows it at 120 x 40: the field, the groups
    /// with their counts, two rows a result with who and when, the match
    /// marked; one row per message however often it matches; `no matches`
    /// when nothing does; the plain list again for an empty field.
    #[test]
    fn the_results_take_the_list_pane_as_the_query_is_typed() {
        let mut model = searchable();
        say(&mut model, "a tripod, and a tripod again");
        search_for(&mut model, "tri");
        let shown = list_rows(&model);
        assert_eq!(shown[0], " / tri");
        assert_eq!(shown[1], "");
        assert_eq!(view(&model).conversations_title, SEARCH_TITLE);
        assert_eq!(view(&model).conversations_title_right, SEARCH_CLOSES);
        assert_eq!(view(&model).list_hint, "");
        assert_eq!(view(&model).search_cursor, Some(6));
        typed(&mut model, "pod");
        let shown = list_rows(&model);
        assert!(shown[2].starts_with(" MESSAGES"), "{shown:#?}");
        assert!(shown[2].ends_with(" 4"), "{shown:#?}");
        assert_eq!(shown[3], " ▌you to Unknown ·cdcd      14:07");
        assert_eq!(shown[4], " ▌a «tripod», and a tripod again");
        assert_eq!(shown[7], "  the «tripod» stays in the van");
        assert_eq!(shown[9], "  you to Unknown ·abab      14:06");
        assert_eq!(shown[10], "  the «Tripod» fits");
        assert_eq!(shown[12], "  Unknown ·abab             14:05");
        assert_eq!(shown[13], "  and the small «tripod» if it f…");
        typed(&mut model, "s");
        assert_eq!(list_rows(&model)[2], format!(" {}", search::NO_MATCHES));
        // Backspaced to empty: the plain list under the field.
        for _ in 0.."tripods".len() {
            update(&mut model, key(KeyCode::Backspace));
        }
        let shown = list_rows(&model);
        assert_eq!(shown[0], " /");
        assert!(shown[2].starts_with("▌Unknown ·cdcd "), "{shown:#?}");
        assert!(shown[5].starts_with(" Unknown ·abab "), "{shown:#?}");
    }

    /// `Enter` on a message opens its conversation at it: the block selected
    /// and the match marked in place; `n` and `N` step the matches of that
    /// conversation round its ends; `Esc` twice brings the list back and the
    /// conversation keeps what it shows.
    #[test]
    fn a_message_result_opens_at_the_message_and_n_steps_the_matches() {
        let mut model = searchable();
        search_for(&mut model, "tripod");
        assert_eq!(model.focus, Pane::Search);
        update(&mut model, key(KeyCode::Tab));
        assert_eq!(model.focus, Pane::Conversations);
        update(&mut model, key(KeyCode::Char('j')));
        update(&mut model, key(KeyCode::Char('n')));
        assert_eq!(model.search.as_ref().map(|s| s.selected), Some(2));
        update(&mut model, key(KeyCode::Enter));
        assert_eq!(model.selected, 0);
        assert_eq!(model.focus, Pane::Messages);
        assert_eq!(model.message_cursor, 1);
        let shown = rows(&model);
        assert!(
            shown.iter().any(|r| r.contains("▌and the small «tripod»")),
            "{shown:#?}"
        );
        assert!(
            shown.iter().any(|r| r.contains("the «Tripod» fits")),
            "{shown:#?}"
        );
        assert!(shown.iter().all(|r| !r.contains("«board»")));
        update(&mut model, key(KeyCode::Char('n')));
        assert_eq!(model.message_cursor, 2);
        update(&mut model, key(KeyCode::Char('n')));
        assert_eq!(model.message_cursor, 1, "round to the first");
        assert!(model
            .current_toast()
            .is_some_and(|t| t.text.contains("first")));
        update(&mut model, ctrl_key('r'));
        assert_eq!(model.message_cursor, 2, "Ctrl-r is N, round to the last");
        update(&mut model, ctrl_key('s'));
        assert_eq!(model.message_cursor, 1, "Ctrl-s is n");
        update(&mut model, key(KeyCode::Esc));
        assert_eq!(model.focus, Pane::Conversations);
        assert!(model.search.is_some());
        update(&mut model, key(KeyCode::Esc));
        assert!(model.search.is_none());
        assert_eq!(model.focus, Pane::Conversations);
        assert_eq!(model.selected, 0, "the conversation stays open");
        assert_eq!(model.message_cursor, 1);
        assert!(rows(&model).iter().all(|r| !r.contains('«')));
        assert!(list_rows(&model)
            .iter()
            .any(|r| r.starts_with("▌Unknown ·abab")));
    }

    /// `Ctrl-s` opens the search from every pane, compose included; while it
    /// is open `Ctrl-s` in the results steps them and `Ctrl-r` is the previous
    /// one, not the fetch it is outside a search.
    #[test]
    fn ctrl_s_opens_the_search_everywhere_and_ctrl_r_fetches_only_outside_it() {
        let mut model = searchable();
        model.focus = Pane::Compose;
        update(&mut model, ctrl_key('s'));
        assert_eq!(model.focus, Pane::Search);
        typed(&mut model, "tripod");
        assert_eq!(model.compose.value(), "", "the field took the typing");
        update(&mut model, ctrl_key('s'));
        assert_eq!(model.search.as_ref().map(|s| s.selected), Some(1));
        update(&mut model, key(KeyCode::Tab));
        assert!(update(&mut model, ctrl_key('r')).is_empty(), "no fetch");
        assert_eq!(model.search.as_ref().map(|s| s.selected), Some(0));
        update(&mut model, key(KeyCode::Enter));
        assert_eq!(model.selected, 1, "the first result is OTHER's message");
        update(&mut model, key(KeyCode::Esc));
        update(&mut model, key(KeyCode::Esc));
        assert!(model.search.is_none());
        assert_eq!(update(&mut model, ctrl_key('r')), [UiEffect::Fetch]);
    }

    /// While the search is open the footer is its own: in the field `Enter
    /// open`, the `Ctrl-s / Ctrl-r` pair, `Tab to the results`, `Esc close
    /// search`; in the results `n / N` as well. Outside it neither.
    #[test]
    fn the_footer_of_an_open_search_comes_from_its_rows() {
        let mut model = searchable();
        let footer = |model: &UiModel<Field>| -> Vec<String> {
            view(model)
                .footer
                .iter()
                .map(|item| format!("{} {}", item.key, item.label))
                .collect()
        };
        assert!(!footer(&model).iter().any(|i| i.contains("next, previous")));
        search_for(&mut model, "tripod");
        let field = footer(&model);
        assert_eq!(
            field[..4],
            [
                "Enter open",
                "Ctrl-s / Ctrl-r next, previous",
                "Tab to the results",
                "Esc close search"
            ],
            "{field:?}"
        );
        update(&mut model, key(KeyCode::Tab));
        let results = footer(&model);
        assert!(
            results.contains(&"n / N next, previous".to_string()),
            "{results:?}"
        );
        assert!(
            results.contains(&"Esc close search".to_string()),
            "{results:?}"
        );
        assert!(!results.contains(&"Tab to the results".to_string()));
    }

    /// The mouse: `/ search` on the list's bottom border opens the search, a
    /// click on a result opens it, one on the field puts the cursor there.
    #[test]
    fn a_click_opens_the_search_and_a_result() {
        let mut model = searchable();
        let hint = cells(&model, |t| *t == hit::Target::SearchHint);
        assert_eq!(hint.len(), SEARCH_HINT.chars().count());
        let screen = hit::screen(&model);
        assert!(hint.iter().all(|&(_, row)| row == screen.list.bottom() - 1));
        press(&mut model, hint[0]);
        assert_eq!(model.focus, Pane::Search);
        assert!(cells(&model, |t| *t == hit::Target::SearchHint).is_empty());
        typed(&mut model, "tripod");
        let third = cells(&model, |t| *t == hit::Target::Result(2));
        assert_eq!(third.len(), 2 * usize::from(screen.list.width - 2));
        model.focus = Pane::Conversations;
        press(&mut model, third[0]);
        assert_eq!(model.selected, 0);
        assert_eq!(model.message_cursor, 1);
        assert_eq!(model.focus, Pane::Messages);
        let field = cells(&model, |t| *t == hit::Target::SearchField);
        press(&mut model, field[0]);
        assert_eq!(model.focus, Pane::Search);
    }

    fn announced(model: &mut UiModel<Field>, peer: [u8; 16], name: &str) {
        engine(
            model,
            OutboxEvent::Announced {
                destination: peer,
                name: Some(name.to_string()),
                path_response: false,
            },
        );
    }

    /// Slice 5: a peer's announced name replaces "Unknown" in the list,
    /// the header, the compose box, the detail view and the toasts, alone
    /// (1452): no hash beside it while nobody shares the name.
    #[test]
    fn an_announced_name_is_drawn_alone_everywhere() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.screen = (80, 24);
        announced(&mut model, PEER, "Alice\u{202e}\u{200b}");
        let shown = view(&model);
        assert_eq!(shown.messages_title, "Alice");
        assert_eq!(shown.compose_title, "to Alice");
        assert_eq!(text(&shown.conversations[0]).trim_end(), "▌Alice");
        engine(
            &mut model,
            OutboxEvent::Received {
                sent_at: 0,
                message_id: ID,
                source: PEER,
                title: Vec::new(),
                body: b"hi".to_vec(),
                fields: Vec::new(),
                verified: true,
            },
        );
        assert_eq!(
            model.current_toast().map(|t| t.text.as_str()),
            Some("message from Alice")
        );
        let detail = detail_view(&model, 0, 0).expect("a message");
        assert_eq!(detail.rows[0], "from Alice to you");
    }

    /// Two peers announcing names that look alike both carry their hash
    /// tail (1452), and the header says in words that two share the name.
    #[test]
    fn a_shared_name_marks_both_peers() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        announced(&mut model, PEER, "Alice");
        announced(&mut model, OTHER, "АLICE");
        assert_eq!(
            view(&model).messages_title,
            "Alice ·abab  Two contacts are called Alice"
        );
        let rows: Vec<String> = view(&model).conversations.iter().map(text).collect();
        assert!(
            rows.iter().any(|r| r.trim_end().ends_with("Alice ·abab")),
            "{rows:#?}"
        );
        assert!(
            rows.iter().any(|r| r.trim_end().ends_with("АLICE ·cdcd")),
            "{rows:#?}"
        );
        // The clash gone, both are their names alone again.
        announced(&mut model, OTHER, "Bob");
        assert_eq!(view(&model).messages_title, "Alice");
    }

    /// A name change is said once, in the conversation, where it happened;
    /// a repeat of the same name says nothing more.
    #[test]
    fn a_changed_name_is_said_once_in_the_conversation() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        announced(&mut model, PEER, "Alice");
        sent(&mut model, "hi");
        announced(&mut model, PEER, "Ally");
        announced(&mut model, PEER, "Ally");
        assert_eq!(
            shown(&model),
            ["hi  ◷", "── was Alice, now announces Ally ──"]
        );
        assert_eq!(view(&model).messages_title, "Ally");
        // Our own address names the reader alone (1449 item 2): the avatar
        // and the profile, no conversation and no note.
        model.address = Some(OTHER);
        let notes = model.conversations[0].notes.len();
        announced(&mut model, OTHER, "me");
        assert_eq!(model.names.shown(&OTHER), Some("me"));
        assert!(model.conversations.iter().all(|c| c.peer != OTHER));
        assert_eq!(model.conversations[0].notes.len(), notes);
        model.address = None;
        // A peer with no conversation gets no note, and none is invented.
        announced(&mut model, OTHER, "Carol");
        announced(&mut model, OTHER, "Carla");
        assert!(model.conversations.iter().all(|c| c.peer != OTHER));
    }

    /// The reader's name wins, the peer is `known`, and the header keeps
    /// the announced name as provenance; a block says `blocked`.
    #[test]
    fn a_petname_wins_and_the_header_says_the_trust() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        model.names = Names::new(
            crate::names::parse_contacts(&format!(
                "name {} Mum\nblock {}\n",
                to_hex(&PEER),
                to_hex(&OTHER)
            ))
            .expect("contacts"),
        );
        assert_eq!(view(&model).messages_title, "Mum  known");
        announced(&mut model, PEER, "Alice");
        assert_eq!(view(&model).messages_title, "Mum  known · announces Alice");
        assert_eq!(view(&model).compose_title, "to Mum");
        // A blocked peer is opened from the `blocked` view.
        update(&mut model, UiEvent::Run(PaletteCommand::Blocked));
        assert_eq!(model.selected, 1);
        assert_eq!(view(&model).messages_title, "Unknown  blocked");
    }

    /// The search matches the name as shown and the address in hex, which
    /// the reader has from the contact info (1452).
    #[test]
    fn the_search_matches_the_shown_name_and_the_address() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        announced(&mut model, PEER, "Alice");
        let order = model.order();
        for query in ["alice", "ABAB", "abababab"] {
            assert_eq!(
                search::hits(&model.conversations, &model.names, &order, query, &[]),
                [search::Hit::Conversation(0)],
                "{query}"
            );
        }
        // The other has no name: its address still matches.
        assert_eq!(
            search::hits(&model.conversations, &model.names, &order, "cdcdcdcd", &[]),
            [search::Hit::Conversation(1)]
        );
    }

    /// A wide name is cut in cells on a narrow list row; a colliding one
    /// keeps its hash tail.
    #[test]
    fn a_wide_name_is_cut_in_cells_and_keeps_its_hash() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        announced(&mut model, PEER, &"龙".repeat(16));
        announced(&mut model, OTHER, &"龙".repeat(16));
        let row = text(&view(&model).conversations[0]);
        let width = leviculum_ui::text::fit_width(&row);
        assert_eq!(width, usize::from(list_width(model.screen.0)) - 2, "{row}");
        assert!(row.trim_end().ends_with("… ·abab"), "{row}");
    }

    // The history (loquax-keys-and-layout.md, section 7, slice 6).

    /// A stored message for `restore`, keyed `key`.
    fn stored(key: u64, outbound: bool, body: &str, delivery: Option<DeliveryState>) -> Message {
        Message {
            outbound,
            body: body.to_string(),
            id: Some([key as u8; 32]),
            delivery,
            signature: Signature::Verified,
            leg: None,
            unproven: false,
            at: Some(at(25, 14, key as u32 % 60)),
            size: body.len(),
            ledger: vec![LedgerEntry::new(Some(at(25, 14, 0)), "queued")],
            last_leg: None,
            activity: key,
            fields: Vec::new(),
        }
    }

    fn restored(peer: [u8; 16], messages: Vec<Message>, earlier: usize) -> history::Restored {
        history::Restored {
            peer,
            pinned: false,
            flags: history::Flags::default(),
            unread: 0,
            messages,
            earlier,
            draft: history::Draft::default(),
            group: None,
            listing: None,
        }
    }

    /// `writes` without the listing rows every change also writes
    /// (1490): for assertions that pin the other writes exactly.
    fn washed(writes: Vec<StoreWrite>) -> Vec<StoreWrite> {
        writes
            .into_iter()
            .filter(|write| !matches!(write, StoreWrite::Listing { .. }))
            .collect()
    }

    fn message_writes(writes: &[StoreWrite]) -> Vec<u64> {
        writes
            .iter()
            .filter_map(|w| match w {
                StoreWrite::Message { message, .. } => Some(message.activity),
                _ => None,
            })
            .collect()
    }

    /// The restart, said honestly: what was in flight comes back on the clock with
    /// the reason in its ledger, and is written back so; what was proven
    /// stays proven; new messages are keyed after the store's last.
    #[test]
    fn a_restart_shows_a_cut_off_send_queued_and_keys_go_on() {
        let mut model = UiModel::<Field>::new(&[]);
        clock(&mut model, 26, 12, 0);
        model.restore(vec![restored(
            PEER,
            vec![
                stored(3, true, "proven", Some(DeliveryState::Delivered)),
                stored(4, false, "theirs", None),
                stored(7, true, "in flight", Some(DeliveryState::InFlight)),
            ],
            0,
        )]);
        let messages = model.messages_of_selected();
        assert_eq!(messages[0].delivery, Some(DeliveryState::Delivered));
        assert_eq!(messages[2].delivery, Some(DeliveryState::Queued));
        assert_eq!(
            messages[2].ledger.last().map(|e| e.what.as_str()),
            Some(history::RESTARTED)
        );
        assert_eq!(
            messages[2].ledger.last().and_then(|e| e.at),
            Some(at(26, 12, 0))
        );
        assert_eq!(model.message_cursor, 2);
        assert_eq!(message_writes(&model.take_writes()), [7]);
        assert!(model.take_writes().is_empty(), "taken once");
        assert!(
            !shown(&model).iter().any(|r| r.contains("earlier")),
            "{:?}",
            shown(&model)
        );

        model.focus = Pane::Compose;
        typed(&mut model, "after the restart");
        update(&mut model, key(KeyCode::Enter));
        let writes = model.take_writes();
        assert_eq!(message_writes(&writes), [8]);
    }

    /// Every change a fold makes to a message reaches the store: the send,
    /// its id, each state, and a reply with the unread count of a
    /// conversation not open.
    #[test]
    fn the_store_learns_every_change_once_per_fold() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        sent(&mut model, "hello");
        let first = message_writes(&model.take_writes());
        assert_eq!(first.len(), 1);
        engine(&mut model, OutboxEvent::Queued { message_id: ID });
        assert_eq!(message_writes(&model.take_writes()), first);
        state(&mut model, ID, MessageState::Delivered);
        let writes = model.take_writes();
        // The listing rides along once for the changed conversation
        // (1490).
        assert_eq!(
            writes
                .iter()
                .filter(|write| matches!(write, StoreWrite::Listing { .. }))
                .count(),
            1,
            "{writes:?}"
        );
        match &washed(writes)[..] {
            [StoreWrite::Message { message, .. }] => {
                assert_eq!(message.delivery, Some(DeliveryState::Arrived));
            }
            other => panic!("one message write, not {other:?}"),
        }
        // A tick changes nothing the store keeps.
        update(&mut model, UiEvent::Tick);
        assert!(model.take_writes().is_empty());
        receive(&mut model, OTHER, 9, "elsewhere");
        let writes = model.take_writes();
        assert!(writes.contains(&StoreWrite::Conversation {
            peer: OTHER,
            pinned: false,
            unread: 1,
            flags: history::Flags::default()
        }));
        assert_eq!(message_writes(&writes).len(), 1);
        // Opening it reads it: the store learns the count is gone.
        update(&mut model, UiEvent::Run(PaletteCommand::NextUnread));
        assert_eq!(
            washed(model.take_writes()),
            [StoreWrite::Conversation {
                peer: OTHER,
                pinned: false,
                unread: 0,
                flags: history::Flags::default()
            }]
        );
    }

    /// `d` asks, `y` deletes, any other key keeps; a send still running is
    /// not deleted at all.
    #[test]
    fn d_then_y_deletes_a_message_and_another_key_keeps_it() {
        let mut model = UiModel::<Field>::new(&[]);
        model.restore(vec![restored(
            PEER,
            vec![
                stored(1, false, "one", None),
                stored(2, false, "two", None),
                stored(3, false, "three", None),
            ],
            0,
        )]);
        model.take_writes();
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Up));
        assert_eq!(model.message_cursor, 1);
        update(&mut model, key(KeyCode::Char('d')));
        let question = view(&model).toast.map(|t| t.text).unwrap_or_default();
        assert!(question.contains("y deletes"), "{question}");
        // The question stays up past a toast's life.
        for _ in 0..TOAST_TICKS * 2 {
            update(&mut model, UiEvent::Tick);
        }
        assert!(view(&model).toast.is_some());
        update(&mut model, key(KeyCode::Char('n')));
        assert_eq!(model.messages_of_selected().len(), 3);
        assert!(model.take_writes().is_empty());

        update(&mut model, key(KeyCode::Char('d')));
        update(&mut model, key(KeyCode::Char('y')));
        let bodies: Vec<&str> = model
            .messages_of_selected()
            .iter()
            .map(|m| m.body.as_str())
            .collect();
        assert_eq!(bodies, ["one", "three"]);
        assert_eq!(model.message_cursor, 1);
        assert_eq!(
            washed(model.take_writes()),
            [StoreWrite::ForgetMessage { key: 2 }]
        );

        // A send the runner still works on stays.
        model.focus = Pane::Compose;
        typed(&mut model, "running");
        update(&mut model, key(KeyCode::Enter));
        model.focus = Pane::Messages;
        update(&mut model, key(KeyCode::Char('d')));
        assert!(model.confirm.is_none());
        assert_eq!(model.messages_of_selected().len(), 3);
    }

    #[test]
    fn the_palette_deletes_a_conversation_after_asking() {
        let mut model = UiModel::<Field>::new(&[]);
        model.restore(vec![
            restored(PEER, vec![stored(5, false, "newer", None)], 0),
            restored(OTHER, vec![stored(2, false, "older", None)], 0),
        ]);
        assert_eq!(model.conversations[model.selected].peer, PEER);
        update(&mut model, UiEvent::Run(PaletteCommand::DeleteConversation));
        update(&mut model, key(KeyCode::Char('y')));
        assert_eq!(model.conversations.len(), 1);
        assert_eq!(model.conversations[model.selected].peer, OTHER);
        assert_eq!(model.message_cursor, 0);
        assert_eq!(
            washed(model.take_writes()),
            [StoreWrite::ForgetConversation { peer: PEER }]
        );
    }

    /// A pinned conversation sits on top above the dotted rule, whatever
    /// came in since, and the pin is written.
    #[test]
    fn a_pinned_conversation_sits_above_the_rule() {
        let mut model = UiModel::<Field>::new(&[]);
        model.restore(vec![
            restored(PEER, vec![stored(5, false, "newer", None)], 0),
            restored(OTHER, vec![stored(2, false, "older", None)], 0),
        ]);
        update(&mut model, UiEvent::Run(PaletteCommand::NextConversation));
        assert_eq!(model.conversations[model.selected].peer, OTHER);
        update(&mut model, UiEvent::Run(PaletteCommand::Pin));
        assert_eq!(
            washed(model.take_writes()),
            [StoreWrite::Conversation {
                peer: OTHER,
                pinned: true,
                unread: 0,
                flags: history::Flags::default()
            }]
        );
        receive(&mut model, PEER, 9, "newest of all");
        let order: Vec<[u8; 16]> = model
            .order()
            .iter()
            .map(|&i| model.conversations[i].peer)
            .collect();
        assert_eq!(order, [OTHER, PEER]);
        let list: Vec<String> = view(&model).conversations.iter().map(text).collect();
        assert!(
            list[2].trim().chars().all(|c| c == layout::PIN_RULE),
            "{list:?}"
        );
        assert!(!list[2].trim().is_empty());
        assert!(
            list[5].trim().is_empty(),
            "no rule under the last: {list:?}"
        );
        update(&mut model, UiEvent::Run(PaletteCommand::Pin));
        let list: Vec<String> = view(&model).conversations.iter().map(text).collect();
        assert!(list.iter().all(|row| !row.contains(layout::PIN_RULE)));
    }

    /// A viewport that shows the open chat's first loaded bubble asks for
    /// the page above it (1496): the phone's only word that its list
    /// reached the top. Negative: a viewport without that bubble, another
    /// peer's, and a chat the store holds no more of ask nothing.
    #[test]
    fn a_viewport_at_the_first_loaded_bubble_loads_the_earlier_page() {
        let mut model = UiModel::<Field>::new(&[]);
        clock(&mut model, 26, 12, 0);
        model.restore(vec![restored(
            PEER,
            vec![
                stored(10, false, "ten", None),
                stored(11, false, "eleven", None),
            ],
            9,
        )]);
        let below = update(
            &mut model,
            UiEvent::Viewport {
                peer: PEER,
                keys: vec![11],
            },
        );
        assert!(below.is_empty(), "{below:?}");
        let other = update(
            &mut model,
            UiEvent::Viewport {
                peer: [0x77; 16],
                keys: vec![10, 11],
            },
        );
        assert!(other.is_empty(), "{other:?}");
        let effects = update(
            &mut model,
            UiEvent::Viewport {
                peer: PEER,
                keys: vec![10, 11],
            },
        );
        assert_eq!(
            effects,
            [UiEffect::Store(StoreRead::Earlier {
                peer: PEER,
                before: 10,
                through: None
            })]
        );
        let older: Vec<Message> = (1..=9).map(|k| stored(k, false, "old", None)).collect();
        update(
            &mut model,
            UiEvent::Earlier {
                peer: PEER,
                messages: older,
                earlier: 0,
            },
        );
        assert_eq!(model.messages_of_selected().len(), 11);
        let all = update(
            &mut model,
            UiEvent::Viewport {
                peer: PEER,
                keys: vec![1, 2],
            },
        );
        assert!(all.is_empty(), "the store holds no more: {all:?}");
    }

    /// The top of what is loaded says how much more the store holds;
    /// reaching it asks for the next page, which goes above with the cursor
    /// still on the message it was on.
    #[test]
    fn reaching_the_top_loads_the_earlier_page() {
        let mut model = UiModel::<Field>::new(&[]);
        clock(&mut model, 26, 12, 0);
        model.restore(vec![restored(
            PEER,
            vec![
                stored(10, false, "ten", None),
                stored(11, false, "eleven", None),
            ],
            9,
        )]);
        assert!(shown(&model).contains(&layout::earlier_line(9)));
        update(&mut model, key(KeyCode::Tab));
        let effects = update(&mut model, key(KeyCode::Up));
        assert_eq!(
            effects,
            [UiEffect::Store(StoreRead::Earlier {
                peer: PEER,
                before: 10,
                through: None
            })]
        );
        let older: Vec<Message> = (1..=9).map(|k| stored(k, false, "old", None)).collect();
        update(
            &mut model,
            UiEvent::Earlier {
                peer: PEER,
                messages: older,
                earlier: 0,
            },
        );
        assert_eq!(model.messages_of_selected().len(), 11);
        assert_eq!(model.message_cursor, 9, "still on ten");
        assert_eq!(model.messages_of_selected()[9].body, "ten");
        assert_eq!(
            text(&model.laid().rows[0]).trim(),
            "",
            "the top of the conversation is the top of the store now"
        );
        // Nothing more to ask for.
        update(&mut model, key(KeyCode::Char('g')));
        assert!(update(&mut model, key(KeyCode::Up)).is_empty());
    }

    /// A search reaches below what the screen loaded through the store: its
    /// hit joins the messages group, and opening it loads the conversation
    /// down to it and opens it there.
    #[test]
    fn a_stored_search_hit_opens_at_its_message() {
        let mut model = UiModel::<Field>::new(&[]);
        clock(&mut model, 26, 12, 0);
        model.restore(vec![restored(
            PEER,
            vec![stored(10, false, "the tripod fits", None)],
            9,
        )]);
        update(&mut model, key(KeyCode::Char('/')));
        typed(&mut model, "tri");
        let last = update(&mut model, key(KeyCode::Char('p')));
        assert_eq!(
            last,
            [UiEffect::Store(StoreRead::Search("trip".to_string()))]
        );
        assert_eq!(model.loaded_from().get(&PEER), Some(&10));
        let old = StoredHit {
            peer: PEER,
            key: 4,
            outbound: false,
            body: "bring the tripod".to_string(),
            at: Some(at(25, 14, 4)),
        };
        // An answer to an older query is dropped.
        update(
            &mut model,
            UiEvent::StoredHits {
                query: "tri".to_string(),
                hits: vec![old.clone()],
            },
        );
        assert_eq!(model.hits().len(), 1);
        update(
            &mut model,
            UiEvent::StoredHits {
                query: "trip".to_string(),
                hits: vec![old],
            },
        );
        assert_eq!(
            model.hits(),
            [
                search::Hit::Message {
                    conversation: 0,
                    message: 0
                },
                search::Hit::Stored(0)
            ]
        );
        let results: Vec<String> = view(&model).conversations.iter().map(text).collect();
        assert!(
            results.iter().any(|row| row.contains("bring the «trip»od")),
            "{results:?}"
        );
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Down));
        let effects = update(&mut model, key(KeyCode::Enter));
        assert_eq!(
            effects,
            [UiEffect::Store(StoreRead::Earlier {
                peer: PEER,
                before: 10,
                through: Some(4)
            })]
        );
        let older: Vec<Message> = (4..=9)
            .map(|k| match k {
                4 => stored(k, false, "bring the tripod", None),
                _ => stored(k, false, "old", None),
            })
            .collect();
        update(
            &mut model,
            UiEvent::Earlier {
                peer: PEER,
                messages: older,
                earlier: 3,
            },
        );
        assert_eq!(model.focus, Pane::Messages);
        assert_eq!(model.message_cursor, 0);
        assert_eq!(model.messages_of_selected()[0].body, "bring the tripod");
    }

    // Telemetry (loquax-map.md, slice (a)): positions in the model, shown as
    // text.

    use leviculum_lxmf::constants::{FIELD_TELEMETRY, FIELD_TELEMETRY_STREAM};
    use leviculum_lxmf::telemetry::{encode_stream_field_value, Location, StreamEntry, Telemetry};

    /// 2026-10-26 14:00 at +01:00, the clock these tests set.
    fn two_pm() -> i64 {
        at(26, 14, 0).unix_secs
    }

    fn fix_at(lat: i32, lon: i32, last_update: i64) -> Telemetry {
        Telemetry {
            location: Some(Location {
                latitude_e6: lat,
                longitude_e6: lon,
                altitude_e2: 4_210,
                speed_e2: 120,
                bearing_e2: 9_000,
                accuracy_e2: 500,
                last_update,
            }),
            ..Telemetry::default()
        }
    }

    fn arrive(
        model: &mut UiModel<Field>,
        from: [u8; 16],
        id: u8,
        body: &str,
        fields: Vec<(i64, Vec<u8>)>,
    ) -> Vec<UiEffect> {
        update(
            model,
            UiEvent::Engine(OutboxEvent::Received {
                sent_at: 0,
                message_id: [id; 32],
                source: from,
                title: Vec::new(),
                body: body.as_bytes().to_vec(),
                fields,
                verified: true,
            }),
        )
    }

    fn report_of(telemetry: &Telemetry) -> Vec<(i64, Vec<u8>)> {
        vec![(FIELD_TELEMETRY, telemetry.encode_field_value())]
    }

    #[test]
    fn a_report_is_one_position_line_and_the_header_says_where_and_how_old() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 26, 14, 0);
        arrive(
            &mut model,
            PEER,
            1,
            "",
            report_of(&fix_at(43_773_100, 11_256_000, two_pm() - 240)),
        );
        // The newest report says whether the position is alive (1209).
        assert_eq!(
            shown(&model),
            vec![
                "── Today ──".to_string(),
                // One sentence (1449 item 5); the coordinates are the Info's.
                "◎ Live position  14:00".to_string(),
            ]
        );
        assert_eq!(view(&model).messages_title_right, "pos 4 min");
        let list: Vec<String> = view(&model).conversations.iter().map(text).collect();
        assert!(
            list[1].contains("◎ position · live"),
            "the list's preview: {list:?}"
        );
        assert!(model.toasts.is_empty(), "a report toasts nothing");
        update(
            &mut model,
            UiEvent::Resize {
                width: 120,
                height: 40,
            },
        );
        let wide = view(&model);
        let row = text(&wide.messages[0]);
        assert!(
            row.starts_with(" at 43.77310 N 11.25600 E · ±5 m · 4 min ago "),
            "{row:?}"
        );
        assert_eq!(
            row.trim(),
            "at 43.77310 N 11.25600 E · ±5 m · 4 min ago",
            "no relay words: {row:?}"
        );
        // The age counts from the fix, not from when it arrived.
        clock(&mut model, 26, 15, 0);
        assert!(text(&view(&model).messages[0]).contains("· 1 h ago"));
    }

    #[test]
    fn consecutive_reports_fold_into_the_latest_and_the_cursor_steps_over_them() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 26, 14, 0);
        arrive(&mut model, PEER, 1, "on my way", Vec::new());
        for (id, minutes) in [(2u8, 10i64), (3, 5), (4, 1)] {
            arrive(
                &mut model,
                PEER,
                id,
                "",
                report_of(&fix_at(
                    43_773_100 + id as i32 * 100,
                    11_256_000,
                    two_pm() - minutes * 60,
                )),
            );
        }
        let rows = shown(&model);
        let lines: Vec<&String> = rows.iter().filter(|r| r.contains('◎')).collect();
        assert_eq!(lines.len(), 1, "{rows:?}");
        assert!(lines[0].contains("◎ Live position"), "the latest: {rows:?}");
        // The newest says whether the position is alive (1209): 13:50,
        // 13:55 and 13:59, so every 5 minutes, the last a minute ago.
        // The bubble says one sentence (1449 item 5); the cadence stays
        // the liveness's, which the list and the map read.
        let now = model.now.map(|now| now.unix_secs);
        assert_eq!(
            model.conversations[0].liveness(now).cadence_s,
            Some(300),
            "{rows:?}"
        );
        assert!(rows.contains(&"on my way  14:00".to_string()));
        // The store still gets every one of them.
        let writes = model.take_writes();
        let positions = writes
            .iter()
            .filter(|w| matches!(w, StoreWrite::Position(_)))
            .count();
        assert_eq!(positions, 3);
        // Up from the shown report goes past the folded ones to the text.
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Up));
        assert_eq!(model.message_cursor, 0, "{rows:?}");
        update(&mut model, key(KeyCode::Down));
        assert_eq!(model.message_cursor, 3);
        // A text between two reports breaks the run.
        arrive(&mut model, PEER, 5, "here", Vec::new());
        arrive(&mut model, PEER, 6, "", report_of(&fix_at(1, 1, two_pm())));
        let rows = shown(&model);
        assert_eq!(
            rows.iter().filter(|r| r.contains('◎')).count(),
            2,
            "{rows:?}"
        );
        // The older run says when its newest fix was taken, one sentence
        // (1449 item 5: "7 Meldungen seit 17:53" explained nothing); its
        // reports stay folded, the long press lists them (1184).
        assert!(
            rows.join(" ").contains("◎ Position from today 13:59"),
            "{rows:?}"
        );
    }

    /// Lew's phone, 2026-10-31 (1184): Columba sent a report a minute and
    /// every one was a bubble, an unread count and a notification. A run is
    /// one row on every screen; a text ends it, and the next report starts
    /// a new one. A report is a state, not a message (1209): it counts
    /// nothing unread and toasts nothing, only the texts do.
    #[test]
    fn a_run_of_reports_is_one_row_and_asks_for_no_attention() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 26, 14, 0);
        arrive(&mut model, OTHER, 1, "losgefahren", Vec::new());
        for id in 2..16u8 {
            let fix = fix_at(
                52_524_360 + i32::from(id),
                13_369_810,
                two_pm() + i64::from(id) * 60,
            );
            arrive(&mut model, OTHER, id, "", report_of(&fix));
        }
        arrive(&mut model, OTHER, 16, "angekommen", Vec::new());
        let c = model
            .conversations
            .iter()
            .position(|c| c.peer == OTHER)
            .expect("listed");
        let bubbles = chat::bubbles(&model, c);
        assert_eq!(bubbles.len(), 3, "{bubbles:?}");
        assert_eq!(bubbles[1].body, "");
        assert_eq!(
            bubbles[1].fix.as_ref().map(|f| f.location.latitude_e6),
            Some(52_524_375),
            "the newest"
        );
        assert_eq!(chat::list_row(&model, &OTHER).unread, 2);
        // A report after the text starts a new run, told and counted not.
        arrive(
            &mut model,
            OTHER,
            17,
            "",
            report_of(&fix_at(1, 1, two_pm())),
        );
        arrive(
            &mut model,
            OTHER,
            18,
            "",
            report_of(&fix_at(2, 2, two_pm())),
        );
        assert_eq!(chat::bubbles(&model, c).len(), 4);
        let row = chat::list_row(&model, &OTHER);
        assert_eq!(row.unread, 2);
        assert_eq!(row.last, "📍 live", "{row:?}");
        let told: Vec<&str> = model.toasts.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(told, ["message from Unknown ·cdcd"; 2], "the texts alone");
        // The messages stay whole: the home's read marks name every one.
        assert_eq!(model.conversations[c].messages.len(), 18);
        assert_eq!(model.conversations[c].unread, 18);
    }

    #[test]
    fn text_and_telemetry_show_the_text_and_move_the_position() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 26, 14, 0);
        arrive(
            &mut model,
            PEER,
            1,
            "parked at the gate",
            report_of(&fix_at(52_000_000, 13_000_000, two_pm() - 60)),
        );
        // The text, and under it the position line it came with (1132).
        let rows = shown(&model);
        let at = rows
            .iter()
            .position(|r| r == "parked at the gate")
            .unwrap_or_else(|| panic!("{rows:?}"));
        assert_eq!(rows[at + 1], "◎ Position from today 13:59  14:00");
        assert_eq!(model.positions[&PEER].location.latitude_e6, 52_000_000);
        assert_eq!(
            model.toasts.last().map(|t| t.text.as_str()),
            Some("message from Unknown")
        );
        let detail = detail_view(&model, 0, 0).expect("a detail view");
        assert!(
            detail
                .rows
                .contains(&"  position  52.00000 N 13.00000 E".to_string()),
            "{:?}",
            detail.rows
        );
        assert!(detail.rows.contains(&"  speed     1.20 m/s".to_string()));
        assert!(detail.rows.contains(&"  bearing   90.00°".to_string()));
        assert!(detail.rows.contains(&"  accuracy  ±5 m".to_string()));
    }

    #[test]
    fn a_fix_direct_and_again_in_a_stream_is_one_position_and_rows_name_their_peer() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        clock(&mut model, 26, 14, 0);
        let bob = fix_at(43_773_100, 11_256_000, two_pm() - 300);
        arrive(&mut model, PEER, 1, "", report_of(&bob));
        let carol = fix_at(-33_868_800, 151_209_300, two_pm() - 100);
        let collector = [0xc0; 16];
        let stream = encode_stream_field_value(&[
            StreamEntry {
                source: PEER.to_vec(),
                timestamp: leviculum_lxmf::msgpack::Number::Int(two_pm() - 300),
                telemetry: bob.encode(),
                appearance: None,
            },
            StreamEntry {
                source: OTHER.to_vec(),
                timestamp: leviculum_lxmf::msgpack::Number::Int(two_pm() - 100),
                telemetry: carol.encode(),
                appearance: None,
            },
        ]);
        arrive(
            &mut model,
            collector,
            2,
            "",
            vec![(FIELD_TELEMETRY_STREAM, stream)],
        );
        assert_eq!(model.positions.len(), 2);
        assert_eq!(model.positions[&PEER].via, PEER, "the direct one stays");
        assert_eq!(model.positions[&OTHER].via, collector);
        // Both go to the store; its key makes the repeated fix one row.
        let fixes: Vec<_> = model
            .take_writes()
            .into_iter()
            .filter_map(|w| match w {
                StoreWrite::Position(fix) => Some((fix.peer, fix.fix_time())),
                _ => None,
            })
            .collect();
        assert_eq!(
            fixes,
            vec![
                (PEER, two_pm() - 300),
                (PEER, two_pm() - 300),
                (OTHER, two_pm() - 100)
            ]
        );
    }

    #[test]
    fn a_broken_blob_is_kept_with_its_message_and_logged() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 26, 14, 0);
        let broken = vec![(FIELD_TELEMETRY, vec![0xc4, 0x01, 0x07])];
        let effects = arrive(&mut model, PEER, 1, "", broken.clone());
        match effects.as_slice() {
            [UiEffect::Log(line)] => assert!(
                line.starts_with("TELEMETRY_UNREADABLE source=abab"),
                "{line}"
            ),
            other => panic!("not logged: {other:?}"),
        }
        assert_eq!(
            model.conversations[0].messages[0].fields, broken,
            "kept raw"
        );
        assert!(model.positions.is_empty());
        assert!(shown(&model).contains(&"◎ telemetry, unreadable (kept)  14:00".to_string()));
    }

    #[test]
    fn a_stale_fix_is_dimmed_in_the_header_and_still_aged() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.tier = Tier::Ansi256;
        clock(&mut model, 26, 14, 0);
        update(
            &mut model,
            UiEvent::Resize {
                width: 120,
                height: 40,
            },
        );
        arrive(
            &mut model,
            PEER,
            1,
            "",
            report_of(&fix_at(
                1_000_000,
                1_000_000,
                two_pm() - telemetry::STALE_AFTER_SECS,
            )),
        );
        let fresh = view(&model).messages[0].cells[1].st;
        assert_eq!(fresh, RStyle::default(), "two hours is still current");
        clock(&mut model, 26, 14, 1);
        let row = &view(&model).messages[0];
        assert!(text(row).contains("· 2 h ago"));
        assert_eq!(row.cells[1].st, layout::painted(model.tokens().muted));
    }

    #[test]
    fn a_fix_from_the_future_shows_its_claim_and_does_not_hold_the_header() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 26, 14, 0);
        arrive(
            &mut model,
            PEER,
            1,
            "",
            report_of(&fix_at(1_000_000, 1_000_000, two_pm() + 7200)),
        );
        assert_eq!(view(&model).messages_title_right, "pos ahead");
        // Its liveness counts from when it came, not from the claim (1209).
        let rows = shown(&model).join(" ");
        assert!(rows.contains("◎ Live position  14:00"), "{rows:?}");
        // An honest fix a minute later replaces it: the claim sorted at 14:00.
        clock(&mut model, 26, 14, 2);
        arrive(
            &mut model,
            PEER,
            2,
            "",
            report_of(&fix_at(2_000_000, 1_000_000, two_pm() + 60)),
        );
        assert_eq!(model.positions[&PEER].location.latitude_e6, 2_000_000);
        assert_eq!(view(&model).messages_title_right, "pos 1 min");
    }

    #[test]
    fn restored_positions_keep_the_latest_and_a_deleted_conversation_takes_them() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        let fix = |lat: i32, t: i64| Fix {
            peer: PEER,
            via: PEER,
            message: None,
            received_at: Some(t),
            location: fix_at(lat, 0, t).location.expect("a location"),
            telemetry: Vec::new(),
            source: crate::telemetry::Source::Telemetry,
        };
        model.restore_positions(vec![fix(2, 200), fix(1, 100)]);
        assert_eq!(model.positions[&PEER].location.latitude_e6, 2);
        model.delete_conversation(PEER, true);
        assert!(model.positions.is_empty());
    }

    /// A node's `lev.position` announce at `lat`, fixed `ago` seconds
    /// before 14:00, heard `age` seconds ago (1302).
    fn mesh_fix(identity: [u8; 16], lat: i32, ago: i64, age: u32) -> UiEvent {
        UiEvent::MeshPosition {
            identity,
            fix: fix_at(lat, 11_260_000, two_pm() - ago)
                .location
                .expect("a location"),
            name: None,
            age,
        }
    }

    /// 1386, the way back: the own receiver's fixes go into our track and
    /// to the store, a row a minute at most however often the receiver
    /// reports, so the track outlives a restart without a network.
    #[test]
    fn the_own_fixes_are_stored_a_row_a_minute_for_the_way_back() {
        const OWN: [u8; 16] = [0x0e; 16];
        let mut model = UiModel::<Field>::new(&[]);
        model.address = Some(OWN);
        clock(&mut model, 26, 14, 0);
        let _ = model.take_writes();
        // Every 5 s for three minutes, as the phone's watch reports.
        for k in 0..36 {
            let at = two_pm() - 180 + 5 * i64::from(k);
            let location = fix_at(1_000_000 + k, 11_260_000, at).location;
            update(&mut model, UiEvent::OwnFix(location));
        }
        let stored: Vec<i64> = model
            .take_writes()
            .iter()
            .filter_map(|w| match w {
                StoreWrite::Position(f) if f.peer == OWN => Some(f.fix_time()),
                _ => None,
            })
            .collect();
        let t0 = two_pm() - 180;
        assert_eq!(stored, [t0, t0 + 60, t0 + 120], "a row a minute");
        assert_eq!(model.tracks[&OWN].len(), 36, "the track keeps every fix");
        assert!(model
            .track_input()
            .iter()
            .any(|(p, own, _)| *p == OWN && *own));
    }

    /// 1302, the merge rule: an announced position joins the telemetry
    /// positions of the identity's delivery address, the newest fix wins
    /// whichever way it came, and the fix remembers how it came. The store
    /// is handed none of the announced ones.
    #[test]
    fn an_announced_position_merges_with_telemetry_by_identity_the_newest_winning() {
        const ANNA: [u8; 16] = [0x33; 16];
        let peer = crate::mesh_position::delivery_of(&ANNA);
        let mut model = UiModel::<Field>::new(&[peer]);
        clock(&mut model, 26, 14, 0);
        arrive(
            &mut model,
            peer,
            1,
            "",
            report_of(&fix_at(1_000_000, 11_260_000, two_pm() - 240)),
        );
        let _ = model.take_writes();
        // Older than the report: the report stays.
        update(&mut model, mesh_fix(ANNA, 2_000_000, 600, 0));
        assert_eq!(model.positions[&peer].location.latitude_e6, 1_000_000);
        assert_eq!(model.positions[&peer].source, telemetry::Source::Telemetry);
        // Newer: the announce wins, and says so.
        update(&mut model, mesh_fix(ANNA, 3_000_000, 60, 0));
        assert_eq!(model.positions[&peer].location.latitude_e6, 3_000_000);
        assert_eq!(model.positions[&peer].source, telemetry::Source::Mesh);
        assert_eq!(model.positions[&peer].received_at, Some(two_pm()));
        // Heard a while ago: received then, not now.
        update(&mut model, mesh_fix(ANNA, 3_000_001, 30, 20));
        assert_eq!(model.positions[&peer].received_at, Some(two_pm() - 20));
        let positions = model
            .take_writes()
            .iter()
            .filter(|w| matches!(w, StoreWrite::Position(_)))
            .count();
        // 1385: the track keeps every announced fix, the store one row a
        // fix time, and the older one too, which the screen did not take.
        assert_eq!(positions, 3, "the store keeps the announced positions");
        let track: Vec<i64> = model.tracks[&peer].iter().map(|f| f.t).collect();
        assert_eq!(track.len(), 4, "the report and three announced: {track:?}");
        // A newer report wins back.
        arrive(
            &mut model,
            peer,
            2,
            "",
            report_of(&fix_at(4_000_000, 11_260_000, two_pm() - 5)),
        );
        assert_eq!(model.positions[&peer].location.latitude_e6, 4_000_000);
        assert_eq!(model.positions[&peer].source, telemetry::Source::Telemetry);
        // The same fix through the engine's seam.
        update(
            &mut model,
            UiEvent::Engine(OutboxEvent::MeshPosition {
                identity: ANNA,
                location: fix_at(5_000_000, 0, two_pm()).location.expect("a location"),
                name: None,
            }),
        );
        assert_eq!(model.positions[&peer].location.latitude_e6, 5_000_000);
    }

    /// 1302: an announced position newer than every report is the row's
    /// newest fix, so the list's pin (1224) and the ring see it; an older
    /// one changes nothing.
    #[test]
    fn an_announced_position_is_the_rows_newest_fix() {
        const ANNA: [u8; 16] = [0x33; 16];
        let peer = crate::mesh_position::delivery_of(&ANNA);
        let mut model = UiModel::<Field>::new(&[peer]);
        clock(&mut model, 26, 14, 0);
        arrive(
            &mut model,
            peer,
            1,
            "",
            report_of(&fix_at(1_000_000, 11_260_000, two_pm() - 3 * 3600)),
        );
        assert_eq!(model.liveness(&peer).state, liveness::State::Recent);
        update(&mut model, mesh_fix(ANNA, 2_000_000, 4 * 3600, 0));
        assert_eq!(
            model.liveness(&peer).last_fix_at,
            Some(two_pm() - 3 * 3600),
            "an older announce is no newer fix"
        );
        update(&mut model, mesh_fix(ANNA, 2_000_000, 120, 0));
        let l = model.liveness(&peer);
        assert_eq!(l.last_fix_at, Some(two_pm() - 120));
        assert_eq!(l.state, liveness::State::Live);
        let row = chat::list_row(&model, &peer);
        assert!(
            scene::PositionBadge::of(&peer, &row.name, &row.liveness, Some(two_pm())).is_some(),
            "the row carries its map pin"
        );
    }

    /// 1302: a node the mesh shows that is no contact is a pin with its
    /// announced name, marked nearby; the map's title is its card while it
    /// is selected, and Enter adds it: its conversation opens, named by its
    /// announce, and its pin is a friend's from then on. A contact's pin
    /// and our own are never nearby.
    /// The tracks' time window (1419): the map's scene has no word with
    /// the default, the chosen window's word with another; `w` steps,
    /// the menu's choice sets, the default among them, and a choice of
    /// the window shown keeps it.
    #[test]
    fn the_map_scene_names_a_window_that_is_not_the_default() {
        let mut model = mapped();
        update(&mut model, key(KeyCode::Char('M')));
        assert_eq!(model.map_view().expect("open").window_word, None);
        update(&mut model, key(KeyCode::Char('w')));
        assert_eq!(model.track_window, tracks::TrackWindow::Today);
        assert_eq!(
            model.map_view().expect("open").window_word.as_deref(),
            Some("Today")
        );
        update(&mut model, UiEvent::TrackWindow(tracks::TrackWindow::Week));
        update(&mut model, UiEvent::TrackWindow(tracks::TrackWindow::Week));
        assert_eq!(model.track_window, tracks::TrackWindow::Week);
        model.lang = strings::Lang::De;
        assert_eq!(
            model.map_view().expect("open").window_word.as_deref(),
            Some("Diese Woche")
        );
        update(
            &mut model,
            UiEvent::TrackWindow(tracks::TrackWindow::Episode),
        );
        assert_eq!(model.map_view().expect("open").window_word, None);
    }

    #[test]
    fn a_node_nearby_is_a_neutral_pin_with_a_card_and_enter_adds_it() {
        const STRANGER: [u8; 16] = [0x44; 16];
        let peer = crate::mesh_position::delivery_of(&STRANGER);
        let mut model = mapped();
        update(
            &mut model,
            UiEvent::MeshPosition {
                identity: STRANGER,
                fix: fix_at(43_775_500, 11_263_000, two_pm() - 120)
                    .location
                    .expect("a location"),
                name: Some("Wanderer".to_string()),
                age: 0,
            },
        );
        let pins = model.map_pins();
        let nearby: Vec<&map::MapPin> = pins.iter().filter(|p| p.nearby).collect();
        assert_eq!(nearby.len(), 1, "{pins:?}");
        assert_eq!(nearby[0].peer, peer);
        assert!(
            nearby[0].label.starts_with("Wanderer"),
            "{}",
            nearby[0].label
        );
        assert!(
            pins.iter().filter(|p| p.peer != peer).all(|p| !p.nearby),
            "bob, Alice and we are not nearby: {pins:?}"
        );
        update(&mut model, key(KeyCode::Char('M')));
        let shown = model.map_view().expect("open");
        assert!(shown.pins.iter().any(|p| p.peer == peer && p.nearby));
        while model.map.and_then(|m| m.selected) != Some(peer) {
            update(&mut model, key(KeyCode::Tab));
        }
        assert_eq!(
            view(&model).messages_title,
            "map · Nearby, not in your contacts · Enter adds"
        );
        update(&mut model, key(KeyCode::Enter));
        assert!(model.map.is_none(), "the map gives way to the chat");
        assert_eq!(model.conversations[model.selected].peer, peer);
        assert_eq!(model.names.petname(&peer), Some("Wanderer"));
        assert!(model.is_contact(&peer));
        assert!(model.map_pins().iter().all(|p| !p.nearby));
    }

    /// The Florence fixture's header, z11 to z15: the region the map
    /// tests' pins lie in.
    fn florence() -> leviculum_map::region::Header {
        const FLORENCE: &[u8] = include_bytes!("../../leviculum-map/fixtures/firenze.lmr");
        *leviculum_map::region::Region::from_bytes(FLORENCE)
            .expect("open")
            .header()
    }

    /// A model with three peers' positions around Florence, ours among them,
    /// at 14:00, on an 80 x 24 screen drawn with braille, the Florence
    /// region installed.
    fn mapped() -> UiModel<Field> {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 26, 14, 0);
        model.address = Some([0x01; 16]);
        model.names.remember(PEER, "bob");
        model.names.remember(OTHER, "Alice");
        let fix = |peer: [u8; 16], lat: i32, lon: i32, ago: i64| Fix {
            peer,
            via: PEER,
            message: None,
            received_at: Some(two_pm()),
            location: fix_at(lat, lon, two_pm() - ago)
                .location
                .expect("a location"),
            telemetry: Vec::new(),
            source: crate::telemetry::Source::Telemetry,
        };
        model.restore_positions(vec![
            fix(PEER, 43_775_000, 11_261_000, 240),
            fix(OTHER, 43_776_000, 11_266_000, 3 * 3600),
            fix([0x01; 16], 43_774_000, 11_258_000, 60),
        ]);
        update(
            &mut model,
            UiEvent::MapSetup(map::MapSetup {
                cell_px: (2, 4),
                zooms: (0, 20),
                regions: vec![florence()],
                ..map::MapSetup::default()
            }),
        );
        model
    }

    /// The shell's own fix is our pin where it is newer than the one we
    /// shared (1375): a board's GNSS shows it before it shared anything,
    /// and the older one does not move it back. The map's picture is the
    /// shell's own place where it says one, else the terminal's pane.
    #[test]
    fn the_own_fix_is_our_pin_and_a_shell_places_the_picture() {
        let mut model = mapped();
        let ours = |m: &UiModel<Field>| {
            let pins = m.map_pins();
            pins.iter()
                .find(|p| p.peer == [0x01; 16])
                .map(|p| (p.label.clone(), p.at))
        };
        let shared = ours(&model).expect("the shared fix");
        let gnss = fix_at(43_777_000, 11_255_000, two_pm() - 5)
            .location
            .expect("a location");
        update(&mut model, UiEvent::OwnFix(Some(gnss)));
        let (label, at) = ours(&model).expect("our pin");
        assert_eq!(label, "you");
        assert_ne!(at, shared.1, "the newer fix moved it");
        // Control: an own fix older than the share leaves the share's pin.
        let old = fix_at(43_777_000, 11_255_000, two_pm() - 3_600)
            .location
            .expect("a location");
        update(&mut model, UiEvent::OwnFix(Some(old)));
        assert_eq!(ours(&model), Some(shared));
        // Control: none at all, no position of ours stored, no pin.
        model.positions.remove(&[0x01; 16]);
        update(&mut model, UiEvent::OwnFix(None));
        assert_eq!(ours(&model), None);

        let pane = model.map_area();
        let picture = Rect::new(0, 1, 53, 21);
        model.map_picture = Some(picture);
        assert_eq!(model.map_area(), picture);
        assert_ne!(pane, picture, "control: the terminal's pane is another");
    }

    fn selected_label(model: &UiModel<Field>) -> String {
        let peer = model.map.and_then(|m| m.selected).expect("a pin selected");
        let pins = model.map_pins();
        pins.iter()
            .find(|p| p.peer == peer)
            .expect("pin")
            .label
            .clone()
    }

    /// The shell's offer of three drawings, the `now`th drawing.
    fn offered(model: &mut UiModel<Field>, now: usize) {
        let drawings = ["octant", "blocks", "braille"]
            .iter()
            .enumerate()
            .map(|(i, name)| map::Drawing {
                name: name.to_string(),
                expected: i != 1,
            })
            .collect();
        let setup = map::MapSetup {
            drawings,
            drawing: now,
            ..model.map_setup.clone()
        };
        update(model, UiEvent::MapSetup(setup));
    }

    /// `t` asks the shell for the next drawing, `T` and the wrap for the
    /// one before; the shell's answer is the drawing, and the title names
    /// it for [`MAP_NAMED_TICKS`] ticks. The first setup names nothing.
    #[test]
    fn t_cycles_the_drawings_with_a_wrap_and_the_title_names_the_new_one() {
        let mut model = mapped();
        update(&mut model, key(KeyCode::Char('t')));
        assert!(model.map.is_none(), "t opens nothing outside the map");
        offered(&mut model, 0);
        update(&mut model, key(KeyCode::Char('M')));
        assert!(
            view(&model).messages_title_right.is_empty(),
            "named at start, and no zoom number (1447)"
        );
        assert_eq!(
            update(&mut model, key(KeyCode::Char('t'))),
            vec![UiEffect::MapDrawing(1)]
        );
        assert_eq!(
            update(&mut model, key(KeyCode::Char('T'))),
            vec![UiEffect::MapDrawing(2)],
            "back from the first wraps to the last"
        );
        assert_eq!(
            update(&mut model, key(KeyCode::F(2))),
            vec![UiEffect::MapDrawing(1)]
        );
        offered(&mut model, 2);
        assert_eq!(
            update(&mut model, key(KeyCode::Char('t'))),
            vec![UiEffect::MapDrawing(0)],
            "on from the last wraps to the first"
        );
        let title = view(&model).messages_title_right;
        assert_eq!(title, "braille");
        // The debug overlay (LOQUAX_FPS=1) adds the zoom and the count.
        model.debug_overlay = true;
        let title = view(&model).messages_title_right;
        assert!(title.starts_with("braille · z"), "{title}");
        model.debug_overlay = false;
        for _ in 0..MAP_NAMED_TICKS {
            update(&mut model, UiEvent::Tick);
        }
        let title = view(&model).messages_title_right;
        assert!(title.is_empty(), "{title}");
        // A shell with one way has nothing to cycle.
        let mut plain = mapped();
        update(&mut plain, key(KeyCode::Char('M')));
        assert!(update(&mut plain, key(KeyCode::Char('t'))).is_empty());
    }

    /// The palette's `map drawing` lists every drawing, the one now behind
    /// `●`, the unexpected ones marked; `Enter` asks for the highlighted
    /// one, the query filters.
    #[test]
    fn the_palette_lists_the_drawings_and_enter_takes_one() {
        let mut model = mapped();
        offered(&mut model, 0);
        update(&mut model, key(KeyCode::Char('M')));
        update(&mut model, UiEvent::Run(PaletteCommand::MapDrawing));
        let rows: Vec<String> = view(&model)
            .palette
            .expect("open")
            .rows
            .iter()
            .map(|r| r.cells.iter().map(|c| c.ch).collect::<String>())
            .collect();
        assert_eq!(
            rows,
            [
                "▸ ● octant",
                "    blocks  (may not show here)",
                "    braille"
            ]
        );
        update(&mut model, key(KeyCode::Down));
        update(&mut model, key(KeyCode::Down));
        update(&mut model, key(KeyCode::Down));
        assert_eq!(
            update(&mut model, key(KeyCode::Enter)),
            vec![UiEffect::MapDrawing(2)]
        );
        assert!(model.palette.is_none());
        update(&mut model, UiEvent::Run(PaletteCommand::MapDrawing));
        for c in "blo".chars() {
            update(&mut model, key(KeyCode::Char(c)));
        }
        assert_eq!(
            update(&mut model, key(KeyCode::Enter)),
            vec![UiEffect::MapDrawing(1)]
        );
        // Without an offer there is nothing to choose.
        let mut plain = mapped();
        update(&mut plain, UiEvent::Run(PaletteCommand::MapDrawing));
        assert!(plain.palette.is_none());
    }

    /// `g` puts the glyph test over the map and takes it away; closing the
    /// map takes it too.
    #[test]
    fn g_shows_the_glyph_test_over_the_map() {
        let mut model = mapped();
        update(&mut model, key(KeyCode::Char('M')));
        assert!(!view(&model).map_glyph_test);
        update(&mut model, key(KeyCode::Char('g')));
        assert!(view(&model).map_glyph_test);
        update(&mut model, key(KeyCode::Char('g')));
        assert!(!view(&model).map_glyph_test);
        update(&mut model, key(KeyCode::Char('g')));
        update(&mut model, key(KeyCode::Esc));
        assert!(!view(&model).map_glyph_test && !model.map_glyph_test);
    }

    /// `M` puts the map in the conversation's place with every pin in view
    /// and the first by name selected; `Tab` steps by name; `Esc` brings
    /// the conversation back with the focus in it.
    #[test]
    fn the_map_opens_fitted_steps_its_pins_and_closes() {
        let mut model = mapped();
        update(&mut model, key(KeyCode::Char('M')));
        assert_eq!(model.focus, Pane::Map);
        let shown = view(&model);
        let map = shown.map.expect("the map is drawn");
        assert_eq!(shown.messages_title, MAP_TITLE);
        assert_eq!(map.pins.len(), 3, "every pin in view: {:?}", map.pins);
        // 0.008 degrees of longitude in 104 x 72 dots, two cells spare.
        assert_eq!(map.viewport.zoom(), 14);
        // By name, case folded: Alice, bob, you.
        let order: Vec<String> = model.map_pins().into_iter().map(|p| p.label).collect();
        assert_eq!(order.len(), 3);
        assert!(order[0].starts_with("Alice") && order[1].starts_with("bob"));
        assert_eq!(order[2], "you");
        assert!(selected_label(&model).starts_with("Alice"));
        // Alice's fix is three hours old: stale, dimmed, with its age.
        let alice = map
            .pins
            .iter()
            .find(|p| p.text.contains("Alice"))
            .expect("Alice");
        // She is at the picture's right edge: her name is set left of her
        // marker, which stays on her cell.
        assert!(alice.stale, "{alice:?}");
        assert_eq!(alice.text, "Alice · 3 h ago ◉");
        assert!(alice.col + 17 <= map.area.x + map.area.width, "{alice:?}");
        let last = map.pins.last().expect("pins");
        assert!(
            last.selected && last.text.contains('◉'),
            "drawn last: {last:?}"
        );
        update(&mut model, key(KeyCode::Tab));
        assert!(selected_label(&model).starts_with("bob"));
        update(&mut model, key(KeyCode::BackTab));
        update(&mut model, key(KeyCode::BackTab));
        assert_eq!(selected_label(&model), "you");
        update(&mut model, key(KeyCode::Esc));
        assert!(model.map.is_none());
        // Back where it was opened from, the list (1531).
        assert_eq!(model.focus, Pane::Conversations);
        assert!(view(&model).map.is_none());
    }

    #[test]
    fn zoom_pan_and_fit_move_the_view_within_the_setups_zooms() {
        let mut model = mapped();
        update(&mut model, UiEvent::Run(PaletteCommand::Map));
        let fitted = model.map.expect("open");
        update(&mut model, key(KeyCode::Char('-')));
        assert_eq!(model.map.expect("open").zoom, fitted.zoom - 1);
        for _ in 0..30 {
            update(&mut model, key(KeyCode::Char('+')));
        }
        assert_eq!(model.map.expect("open").zoom, 20, "the setup's deepest");
        for _ in 0..30 {
            update(&mut model, key(KeyCode::Char('-')));
        }
        assert_eq!(model.map.expect("open").zoom, 0);
        update(&mut model, key(KeyCode::Char('0')));
        assert_eq!(model.map, Some(fitted));
        // A pan moves the centre an eighth of the view, the arrows and vi's
        // keys alike, and back again.
        update(&mut model, key(KeyCode::Char('l')));
        let east = model.map.expect("open").centre;
        assert!(east.x > fitted.centre.x && east.y == fitted.centre.y);
        update(&mut model, key(KeyCode::Left));
        assert_eq!(model.map.expect("open").centre, fitted.centre);
        update(&mut model, key(KeyCode::Char('j')));
        assert!(model.map.expect("open").centre.y > fitted.centre.y);
        update(&mut model, key(KeyCode::Up));
        assert_eq!(model.map.expect("open").centre, fitted.centre);
        // Shift, or vi's capitals, moves four times as far: half the view.
        let at = |model: &UiModel<Field>| {
            let state = model.map.expect("open");
            let view = map::viewport(&state, model.map_area(), &model.map_setup);
            view.to_screen(fitted.centre)
        };
        let (w, h) = map::viewport(&fitted, model.map_area(), &model.map_setup).size();
        let (cx, cy) = at(&model);
        update(&mut model, key(KeyCode::Char('l')));
        assert_eq!(at(&model), (cx - i64::from(w / 8), cy));
        update(&mut model, key(KeyCode::Char('h')));
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT)),
        );
        assert_eq!(at(&model), (cx - i64::from(w / 8 * 4), cy));
        update(&mut model, key(KeyCode::Char('H')));
        update(&mut model, key(KeyCode::Char('J')));
        assert_eq!(at(&model), (cx, cy - i64::from(h / 8 * 4)));
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT)),
        );
        assert_eq!(model.map.expect("open").centre, fitted.centre);
    }

    /// `Enter` on a pin, or a click on it, opens that peer's conversation,
    /// a new one for a peer known only by position; our own pin opens none.
    #[test]
    fn a_pin_opens_its_peers_conversation_by_enter_or_click() {
        let mut model = mapped();
        update(&mut model, key(KeyCode::Char('M')));
        assert!(selected_label(&model).starts_with("Alice"));
        update(&mut model, key(KeyCode::Enter));
        assert!(model.map.is_none());
        assert_eq!(model.focus, Pane::Messages);
        assert_eq!(model.conversations[model.selected].peer, OTHER);
        // Open again, click bob's name.
        update(&mut model, ctrl_key('g'));
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT)),
        );
        let map = view(&model).map.expect("open");
        let bob = map
            .pins
            .iter()
            .find(|p| p.text.contains("bob"))
            .expect("bob");
        press(&mut model, (bob.col + 3, bob.row));
        assert!(model.map.is_none());
        assert_eq!(model.conversations[model.selected].peer, PEER);
        // A click on the map where no pin is focuses it and opens nothing.
        update(&mut model, key(KeyCode::Char('M')));
        let map = view(&model).map.expect("open");
        let empty = (map.area.x, map.area.y);
        assert!(map.pins.iter().all(|p| (p.col, p.row) != empty));
        press(&mut model, empty);
        assert_eq!(model.focus, Pane::Map);
        assert!(model.map.is_some());
        // Our own pin.
        update(&mut model, key(KeyCode::BackTab));
        assert_eq!(selected_label(&model), "you");
        update(&mut model, key(KeyCode::Enter));
        assert!(model.map.is_some(), "no conversation with ourselves");
    }

    /// A drag on the map moves the view with the pointer at every
    /// backend's cell: sixel's 9 x 19 pixels, half-blocks' 1 x 2,
    /// braille's 2 x 4. The world point at the cell the button went down
    /// on is at the cell it came up on, the view moved by the cells times
    /// the cell's pixels, and nothing opens.
    #[test]
    fn a_drag_moves_the_map_by_the_cells_at_each_backends_geometry() {
        for cell_px in [(9, 19), (1, 2), (2, 4)] {
            let mut model = mapped();
            update(
                &mut model,
                UiEvent::MapSetup(map::MapSetup {
                    cell_px,
                    zooms: (0, 20),
                    regions: vec![florence()],
                    ..map::MapSetup::default()
                }),
            );
            update(&mut model, key(KeyCode::Char('M')));
            let before = model.map.expect("open");
            let shown = view(&model).map.expect("drawn");
            let area = shown.area;
            let from = (area.x + 2, area.y + 2);
            assert_eq!(map::pin_at(&shown.pins, area, from.0, from.1), None);
            let to = (from.0 + 5, from.1 + 3);
            let corner = |view: &leviculum_map::viewport::Viewport, (col, row): (u16, u16)| {
                view.to_world(
                    i32::from(col - area.x) * i32::from(cell_px.0),
                    i32::from(row - area.y) * i32::from(cell_px.1),
                )
            };
            let held = corner(&shown.viewport, from);
            update(
                &mut model,
                mouse(MouseEventKind::Down(MouseButton::Left), from.0, from.1),
            );
            assert_eq!(model.focus, Pane::Map);
            assert!(!model.map_dragging(), "not before it moves");
            update(
                &mut model,
                mouse(MouseEventKind::Drag(MouseButton::Left), to.0, to.1),
            );
            assert!(model.map_dragging(), "{cell_px:?}");
            let after = model.map.expect("open");
            let view_after = view(&model).map.expect("drawn").viewport;
            assert_eq!(corner(&view_after, to), held, "{cell_px:?}");
            let pixel = 1u32 << (24 - u32::from(before.zoom));
            assert_eq!(
                (
                    before.centre.x - after.centre.x,
                    before.centre.y - after.centre.y
                ),
                (
                    5 * u32::from(cell_px.0) * pixel,
                    3 * u32::from(cell_px.1) * pixel
                ),
                "{cell_px:?}"
            );
            assert_eq!(after.zoom, before.zoom);
            update(
                &mut model,
                mouse(MouseEventKind::Up(MouseButton::Left), to.0, to.1),
            );
            assert!(!model.map_dragging());
            assert_eq!(model.map.expect("open").centre, after.centre);
            assert_eq!(model.focus, Pane::Map);
        }
    }

    /// A release in a cell no motion reached still ends the drag there: a
    /// terminal may report the button up without the move before it.
    #[test]
    fn a_release_away_from_the_press_is_a_drag_to_it() {
        let mut model = mapped();
        update(&mut model, key(KeyCode::Char('M')));
        let before = model.map.expect("open");
        let area = model.map_area();
        let from = (area.x + 2, area.y + 2);
        update(
            &mut model,
            mouse(MouseEventKind::Down(MouseButton::Left), from.0, from.1),
        );
        update(
            &mut model,
            mouse(MouseEventKind::Up(MouseButton::Left), from.0 + 1, from.1),
        );
        let after = model.map.expect("open");
        let pixel = 1u32 << (24 - u32::from(before.zoom));
        assert_eq!(before.centre.x - after.centre.x, 2 * pixel);
    }

    /// A press and release on a pin with no motion between, or a motion
    /// the terminal reports in the same cell, is a click: the pin opens. A
    /// drag that starts on a pin moves the map and opens nothing, also when
    /// the pointer comes back to the pin before the release.
    #[test]
    fn a_press_without_motion_is_a_click_and_a_drag_from_a_pin_opens_nothing() {
        let bob_of = |model: &UiModel<Field>| {
            let map = view(model).map.expect("open");
            let bob = map
                .pins
                .iter()
                .find(|p| p.text.contains("bob"))
                .expect("bob");
            (bob.col + 3, bob.row)
        };
        let mut model = mapped();
        update(&mut model, key(KeyCode::Char('M')));
        let bob = bob_of(&model);
        let before = model.map.expect("open");
        update(
            &mut model,
            mouse(MouseEventKind::Down(MouseButton::Left), bob.0, bob.1),
        );
        update(
            &mut model,
            mouse(MouseEventKind::Drag(MouseButton::Left), bob.0 + 4, bob.1),
        );
        update(
            &mut model,
            mouse(MouseEventKind::Drag(MouseButton::Left), bob.0, bob.1),
        );
        update(
            &mut model,
            mouse(MouseEventKind::Up(MouseButton::Left), bob.0, bob.1),
        );
        assert!(model.map.is_some(), "a drag from a pin does not open it");
        assert_eq!(model.map.expect("open").centre, before.centre, "and back");
        update(
            &mut model,
            mouse(MouseEventKind::Down(MouseButton::Left), bob.0, bob.1),
        );
        update(
            &mut model,
            mouse(MouseEventKind::Drag(MouseButton::Left), bob.0, bob.1),
        );
        assert!(!model.map_dragging(), "the same cell is no drag");
        update(
            &mut model,
            mouse(MouseEventKind::Up(MouseButton::Left), bob.0, bob.1),
        );
        assert!(model.map.is_none(), "a click opens it");
        assert_eq!(model.conversations[model.selected].peer, PEER);
    }

    /// The wheel zooms about the pointer: the point at the centre of the
    /// cell under it stays in that cell, in and out; off the picture, on
    /// the map's border, it zooms about the centre.
    #[test]
    fn the_wheel_zooms_the_map_about_the_pointer() {
        let mut model = mapped();
        update(&mut model, key(KeyCode::Char('M')));
        let area = model.map_area();
        let at = (area.x + 3, area.y + 2);
        let middle = |model: &UiModel<Field>| {
            let view = view(model).map.expect("drawn").viewport;
            view.to_world(
                i32::from(at.0 - area.x) * 2 + 1,
                i32::from(at.1 - area.y) * 4 + 2,
            )
        };
        let before = model.map.expect("open");
        let held = middle(&model);
        update(&mut model, mouse(MouseEventKind::ScrollUp, at.0, at.1));
        assert_eq!(model.map.expect("open").zoom, before.zoom + 1);
        assert_eq!(middle(&model), held);
        update(&mut model, mouse(MouseEventKind::ScrollDown, at.0, at.1));
        update(&mut model, mouse(MouseEventKind::ScrollDown, at.0, at.1));
        assert_eq!(model.map.expect("open").zoom, before.zoom - 1);
        assert_eq!(middle(&model), held);
        // On the border: the centre stays.
        let centre = model.map.expect("open").centre;
        update(
            &mut model,
            mouse(MouseEventKind::ScrollUp, area.x - 1, area.y + 2),
        );
        assert_eq!(model.map.expect("open").centre, centre);
        assert_eq!(model.map.expect("open").zoom, before.zoom);
    }

    /// The tilted wheel, which is what a sideways swipe on a touch terminal
    /// may send (1102), pans the map an eighth as `l` and `h` do; off the
    /// map and under an overlay it does nothing.
    #[test]
    fn the_tilted_wheel_pans_the_map_as_h_and_l() {
        let mut model = mapped();
        update(&mut model, key(KeyCode::Char('M')));
        let area = model.map_area();
        let at = (area.x + 3, area.y + 2);
        let start = model.map.expect("open");
        update(&mut model, mouse(MouseEventKind::ScrollRight, at.0, at.1));
        let tilted = model.map.expect("open");
        assert_ne!(tilted.centre, start.centre, "it moved");
        update(&mut model, mouse(MouseEventKind::ScrollLeft, at.0, at.1));
        assert_eq!(model.map.expect("open").centre, start.centre, "and back");
        update(&mut model, key(KeyCode::Char('l')));
        assert_eq!(model.map.expect("open").centre, tilted.centre, "as l");
        update(&mut model, mouse(MouseEventKind::ScrollLeft, at.0, at.1));
        assert_eq!(model.map.expect("open").centre, start.centre, "as h");
        assert_eq!(model.map.expect("open").zoom, start.zoom, "no zoom");
        // Over the list beside the map: nothing.
        assert_eq!(hit::pane_at(&model, 1, 1), Some(Pane::Conversations));
        update(&mut model, mouse(MouseEventKind::ScrollRight, 1, 1));
        assert_eq!(model.map.expect("open").centre, start.centre);
        // Under the help: nothing.
        update(&mut model, key(KeyCode::Char('?')));
        assert!(model.help);
        update(&mut model, mouse(MouseEventKind::ScrollRight, at.0, at.1));
        assert_eq!(model.map.expect("open").centre, start.centre);
    }

    /// The list stays beside the map: the focus goes there and back by
    /// `Tab`, and only the conversation or compose taking the focus closes
    /// the map.
    #[test]
    fn the_list_stays_beside_the_map_and_compose_closes_it() {
        let mut model = mapped();
        update(&mut model, key(KeyCode::Char('M')));
        update(&mut model, UiEvent::Run(PaletteCommand::FocusConversations));
        assert!(model.map.is_some());
        update(&mut model, key(KeyCode::Tab));
        assert_eq!(model.focus, Pane::Map, "Tab from the list goes to the map");
        update(&mut model, UiEvent::Run(PaletteCommand::FocusCompose));
        assert!(model.map.is_none());
        assert_eq!(model.focus, Pane::Compose);
    }

    /// No position at all: the whole world at the shallowest zoom.
    #[test]
    fn a_map_without_pins_shows_the_world() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(&mut model, key(KeyCode::Char('M')));
        let state = model.map.expect("open");
        assert_eq!((state.zoom, state.selected), (0, None));
        assert!(view(&model).map.expect("drawn").pins.is_empty());
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Enter));
        assert!(model.map.is_some());
    }

    /// Jump to me (1477): Enter on the map with no pin chosen centres on
    /// the own fix at the deepest zoom a region holds there; Enter on the
    /// own pin does the same instead of saying it is you. Control: without
    /// a fix Enter leaves the view where it was.
    #[test]
    fn enter_without_a_chosen_pin_jumps_to_the_own_fix() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.address = Some([0x01; 16]);
        update(
            &mut model,
            UiEvent::MapSetup(map::MapSetup {
                cell_px: (2, 4),
                zooms: (0, 20),
                regions: vec![florence()],
                ..map::MapSetup::default()
            }),
        );
        update(&mut model, key(KeyCode::Char('M')));
        let before = model.map.expect("open");
        assert_eq!(before.selected, None);
        update(&mut model, key(KeyCode::Enter));
        assert_eq!(model.map, Some(before), "control: no fix, no jump");
        let here = fix_at(43_774_000, 11_258_000, 0)
            .location
            .expect("a location");
        update(&mut model, UiEvent::OwnFix(Some(here)));
        if let Some(state) = model.map.as_mut() {
            state.selected = None;
        }
        update(&mut model, key(KeyCode::Enter));
        let state = model.map.expect("still open");
        let at = projected(43_774_000, 11_258_000);
        let (centre, zoom) = map::fit_at(at, model.map_area(), &model.map_setup);
        assert_eq!((state.centre, state.zoom), (centre, zoom));
        assert_ne!(state.zoom, before.zoom, "it zoomed to the place");
        // The own pin chosen: the same jump, no toast, the map stays.
        if let Some(state) = model.map.as_mut() {
            state.selected = Some([0x01; 16]);
            state.zoom = 0;
        }
        model.toasts.clear();
        update(&mut model, key(KeyCode::Enter));
        let state = model.map.expect("still open");
        assert_eq!((state.centre, state.zoom), (centre, zoom));
        assert!(model.toasts.is_empty(), "{:?}", model.toasts);
    }

    /// The peer's conversation with a text, an older report, a text and
    /// the latest report, on the Florence region at braille's cell (1076).
    fn reported() -> UiModel<Field> {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        clock(&mut model, 26, 14, 0);
        update(
            &mut model,
            UiEvent::MapSetup(map::MapSetup {
                cell_px: (2, 4),
                zooms: (0, 20),
                regions: vec![florence()],
                ..map::MapSetup::default()
            }),
        );
        arrive(&mut model, PEER, 1, "on my way", Vec::new());
        let older = fix_at(43_773_100, 11_256_000, two_pm() - 600);
        arrive(&mut model, PEER, 2, "", report_of(&older));
        arrive(&mut model, PEER, 3, "here", Vec::new());
        let latest = fix_at(43_775_000, 11_261_000, two_pm() - 60);
        arrive(&mut model, PEER, 4, "", report_of(&latest));
        model.toasts.clear();
        model
    }

    fn projected(lat: i32, lon: i32) -> leviculum_map::mercator::World {
        leviculum_map::mercator::project(leviculum_map::mercator::LatLon::from_e6(lat, lon))
    }

    /// `Enter` on a report opens the map centred on that report's fix, not
    /// the peer's latest, at the region's deepest zoom with the peer's pin
    /// selected; `Esc` brings the conversation back with the cursor where
    /// it was. `Enter` on a text still opens its detail view, and `v` opens
    /// a report's.
    #[test]
    fn enter_on_a_report_opens_the_map_at_its_fix_and_esc_returns() {
        let mut model = reported();
        update(&mut model, key(KeyCode::Tab));
        assert_eq!(model.focus, Pane::Messages);
        assert_eq!(model.message_cursor, 3);
        update(&mut model, key(KeyCode::Up));
        update(&mut model, key(KeyCode::Up));
        assert_eq!(model.message_cursor, 1, "the older report");
        update(&mut model, key(KeyCode::Enter));
        assert_eq!(model.focus, Pane::Map);
        assert_eq!(model.detail, None);
        let state = model.map.expect("the map is open");
        assert_eq!(state.centre, projected(43_773_100, 11_256_000));
        assert_eq!(state.zoom, florence().max_zoom);
        assert_eq!(state.selected, Some(PEER));
        update(&mut model, key(KeyCode::Esc));
        assert!(model.map.is_none());
        assert_eq!(model.focus, Pane::Messages);
        assert_eq!(model.message_cursor, 1);
        // The latest report, from the cursor's place at the bottom.
        update(&mut model, key(KeyCode::Char('G')));
        update(&mut model, key(KeyCode::Enter));
        let state = model.map.expect("open");
        assert_eq!(state.centre, projected(43_775_000, 11_261_000));
        update(&mut model, ctrl_key('g'));
        // A text opens its detail view, as before.
        update(&mut model, key(KeyCode::Char('g')));
        assert_eq!(model.message_cursor, 0);
        update(&mut model, key(KeyCode::Enter));
        assert_eq!(model.detail, Some((0, 0)));
        assert!(model.map.is_none());
        update(&mut model, key(KeyCode::Esc));
        // `I` opens a report's detail view, with its sensor rows.
        update(&mut model, key(KeyCode::Down));
        assert_eq!(model.message_cursor, 1);
        update(&mut model, key(KeyCode::Char('I')));
        assert_eq!(model.detail, Some((0, 1)));
        assert!(model.map.is_none());
        let detail = detail_view(&model, 0, 1).expect("a detail view");
        assert!(
            detail.rows.iter().any(|r| r.starts_with("  position")),
            "{:?}",
            detail.rows
        );
    }

    /// A text that came with a position (Sideband's telemetry beside a
    /// body) shows its position line under the text, and `Enter` or a
    /// click on it opens the map at that fix with the peer's pin selected,
    /// as on a report; `v` still opens its detail view (1132).
    #[test]
    fn enter_on_a_text_with_a_position_opens_the_map_at_it() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 26, 14, 0);
        let fix = fix_at(43_773_100, 11_256_000, two_pm() - 240);
        arrive(&mut model, PEER, 1, "at the bridge", report_of(&fix));
        assert_eq!(
            shown(&model),
            vec![
                "── Today ──".to_string(),
                "at the bridge".to_string(),
                "◎ Position from today 13:56  14:00".to_string(),
            ]
        );
        // The coordinates and the accuracy are the Info's (1449 item 5).
        let activity = model.conversations[0]
            .messages
            .last()
            .map(|m| m.activity)
            .expect("it");
        let info = info::message_info(&model, PEER, activity).expect("its info");
        let rows: Vec<(String, String)> = info
            .rows
            .iter()
            .map(|r| r.resolve(strings::Lang::En))
            .collect();
        assert!(
            rows.contains(&("Place".to_string(), "43.77310 N 11.25600 E".to_string())),
            "{rows:?}"
        );
        assert!(model.message_fix(0, 0).is_some());
        update(&mut model, key(KeyCode::Tab));
        assert_eq!(model.message_cursor, 0);
        update(&mut model, key(KeyCode::Enter));
        assert_eq!(model.focus, Pane::Map);
        assert_eq!(model.detail, None);
        let state = model.map.expect("the map is open");
        assert_eq!(state.centre, projected(43_773_100, 11_256_000));
        assert_eq!(state.selected, Some(PEER));
        assert!(view(&model).map.is_some(), "the frame draws the map");
        update(&mut model, key(KeyCode::Esc));
        assert!(model.map.is_none());
        update(&mut model, key(KeyCode::Char('I')));
        assert_eq!(model.detail, Some((0, 0)));
        update(&mut model, key(KeyCode::Esc));
        let line = cell_of(&model, "◎ Position from", 3);
        press(&mut model, line);
        assert_eq!(
            model.map.expect("a click opens it too").centre,
            projected(43_773_100, 11_256_000)
        );

        // Negative: the same text without the field is a text.
        let mut plain = UiModel::<Field>::new(&[PEER]);
        clock(&mut plain, 26, 14, 0);
        arrive(&mut plain, PEER, 1, "at the bridge", Vec::new());
        assert_eq!(plain.message_fix(0, 0), None);
        update(&mut plain, key(KeyCode::Tab));
        update(&mut plain, key(KeyCode::Enter));
        assert!(plain.map.is_none());
        assert_eq!(plain.detail, Some((0, 0)));
    }

    /// Outside the map, outside a fix: the map is somewhere else. Without a
    /// region the zoom is the outline's deepest.
    #[test]
    fn a_report_outside_every_region_opens_at_the_outlines_deepest_zoom() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 26, 14, 0);
        arrive(
            &mut model,
            PEER,
            1,
            "",
            report_of(&fix_at(50_110_600, 8_682_100, two_pm() - 60)),
        );
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Enter));
        let state = model.map.expect("open");
        assert_eq!(state.centre, projected(50_110_600, 8_682_100));
        assert_eq!(state.zoom, leviculum_map::outline::MAX_ZOOM);
    }

    /// A model with a report from Frankfurt, outside every region, the
    /// autoload as `autoload` says, the list focused on it.
    fn frankfurt(autoload: bool) -> UiModel<Field> {
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.map_autoload = autoload;
        clock(&mut model, 26, 14, 0);
        arrive(
            &mut model,
            PEER,
            1,
            "",
            report_of(&fix_at(50_110_600, 8_682_100, two_pm() - 60)),
        );
        update(&mut model, key(KeyCode::Tab));
        model
    }

    /// The region the shell would write for `want`, as its header.
    fn landed(want: &map::MapWant) -> leviculum_map::region::Header {
        leviculum_map::region::Header {
            bounds: want.bounds,
            min_zoom: want.zooms.0,
            max_zoom: want.zooms.1,
            ..florence()
        }
    }

    /// The terminal says in words when a load runs for what is on
    /// screen (1527), over the older "loading map", and goes back to it
    /// once the screen's load is done while another still runs.
    #[test]
    fn the_terminal_says_the_map_of_this_area_loads() {
        let mut model = frankfurt(true);
        let effects = update(&mut model, key(KeyCode::Enter));
        let [UiEffect::LoadMap(want)] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        update(&mut model, UiEvent::MapLoad(MapLoad::Started(want.clone())));
        update(&mut model, UiEvent::MapLoad(MapLoad::Here(true)));
        assert_eq!(view(&model).messages_title_right, MAP_LOADING_HERE);
        update(&mut model, UiEvent::MapLoad(MapLoad::Here(false)));
        assert_eq!(view(&model).messages_title_right, MAP_LOADING);
    }

    /// Going to a place no region holds asks for its region once (1089):
    /// a 12 km box around it, z10 to z15. The title says it loads; the
    /// region landing fits the map to its deepest zoom; asked again, the
    /// place is held and nothing is asked.
    #[test]
    fn opening_the_map_at_a_place_without_a_region_loads_it_once() {
        let mut model = frankfurt(true);
        let effects = update(&mut model, key(KeyCode::Enter));
        let [UiEffect::LoadMap(want)] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        let at = projected(50_110_600, 8_682_100);
        assert_eq!(
            model.map.expect("open").zoom,
            leviculum_map::outline::MAX_ZOOM
        );
        assert_eq!(want.reason, map::LoadReason::Place);
        assert_eq!(want.centre, at);
        assert_eq!((want.zooms, want.floor), ((10, 15), 13));
        let b = want.bounds;
        assert!(b.contains(leviculum_map::mercator::unproject(at)));
        // 12 km north to south: 0.1078 degrees.
        assert!((b.north - b.south - 1_078_000).abs() < 2_000, "{b:?}");
        update(&mut model, UiEvent::MapLoad(MapLoad::Started(want.clone())));
        assert!(
            view(&model).messages_title_right.ends_with("loading map"),
            "{}",
            view(&model).messages_title_right
        );
        let mut setup = model.map_setup.clone();
        setup.regions.push(landed(want));
        setup.zooms = (0, 20);
        update(&mut model, UiEvent::MapSetup(setup));
        update(&mut model, UiEvent::MapLoad(MapLoad::Done(want.clone())));
        assert_eq!(model.map_loading, None);
        let state = model.map.expect("open");
        assert_eq!((state.centre, state.zoom), (at, 15), "fitted to the region");
        // The place again: held, nothing asked.
        update(&mut model, key(KeyCode::Esc));
        assert_eq!(update(&mut model, key(KeyCode::Enter)), []);
    }

    /// With `map_autoload = off` going to a place asks for nothing.
    #[test]
    fn with_the_autoload_off_nothing_is_loaded() {
        let mut model = frankfurt(false);
        assert_eq!(update(&mut model, key(KeyCode::Enter)), []);
        assert!(model.map.is_some());
        update(&mut model, key(KeyCode::Esc));
        assert_eq!(update(&mut model, key(KeyCode::Char('M'))), []);
    }

    /// `d` on the map asks for the visible area, whether the autoload is on
    /// or not: from three zooms above the view's to z15, the deepest lowered
    /// no further than the view's. Over an area held already it says so.
    #[test]
    fn the_key_loads_the_visible_area_and_a_held_one_is_not_loaded() {
        let mut model = frankfurt(false);
        update(&mut model, key(KeyCode::Enter));
        let state = model.map.expect("open");
        let shown = map::viewport(&state, model.map_area(), &model.map_setup);
        let effects = update(&mut model, key(KeyCode::Char('d')));
        let [UiEffect::LoadMap(want)] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert_eq!(want.reason, map::LoadReason::View);
        assert_eq!(want.bounds, shown.bounds());
        assert_eq!(want.zooms, (state.zoom - 3, 15));
        assert_eq!(want.floor, state.zoom);
        let mut setup = model.map_setup.clone();
        setup.regions.push(landed(want));
        update(&mut model, UiEvent::MapSetup(setup));
        update(&mut model, UiEvent::MapLoad(MapLoad::Done(want.clone())));
        assert_eq!(
            model.map.expect("open"),
            state,
            "a view's load moves nothing"
        );
        assert_eq!(update(&mut model, key(KeyCode::Char('d'))), []);
        assert_eq!(
            model.toasts.last().map(|t| t.text.as_str()),
            Some(MAP_LOADED_ALREADY)
        );
    }

    /// A failed load is one toast, when the shell gives one; a failure it
    /// said already is silent.
    #[test]
    fn a_failed_load_is_one_toast() {
        let mut model = frankfurt(true);
        let effects = update(&mut model, key(KeyCode::Enter));
        let [UiEffect::LoadMap(want)] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        update(&mut model, UiEvent::MapLoad(MapLoad::Started(want.clone())));
        let before = model.toasts.len();
        update(
            &mut model,
            UiEvent::MapLoad(MapLoad::Failed {
                want: want.clone(),
                toast: Some("map: HTTP status 503".to_string()),
            }),
        );
        assert_eq!(model.toasts.len(), before + 1);
        assert_eq!(model.map_loading, None);
        update(
            &mut model,
            UiEvent::MapLoad(MapLoad::Failed {
                want: want.clone(),
                toast: None,
            }),
        );
        assert_eq!(model.toasts.len(), before + 1);
    }

    /// The palette's `map` asks for the places of the pins no region holds
    /// 3 km around: one place for three pins a few streets apart (the
    /// Florence fixture is too small to hold them), none once that place's
    /// region is there.
    #[test]
    fn the_map_asks_for_the_pins_no_region_holds() {
        let mut model = mapped();
        model.map_autoload = true;
        let effects = update(&mut model, key(KeyCode::Char('M')));
        let [UiEffect::LoadMap(want)] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert_eq!(want.reason, map::LoadReason::Place);
        let mut setup = model.map_setup.clone();
        setup.regions.push(landed(want));
        update(&mut model, UiEvent::MapSetup(setup));
        update(&mut model, key(KeyCode::Esc));
        assert_eq!(update(&mut model, key(KeyCode::Char('M'))), []);
    }

    /// A click on a report row opens the map at its fix; a click on the
    /// header's position, narrow or wide, and `L` or `Alt-l` put the peer
    /// on the map at its latest fix. The header's position is not the
    /// mailbox: a click there fetches nothing. A peer without a position
    /// says so.
    #[test]
    fn a_click_on_a_report_or_the_header_position_opens_the_map() {
        let mut model = reported();
        // Named, so the narrow header has room for its position: two
        // nameless peers would say so there (1452).
        announced(&mut model, PEER, "Alice");
        // A relay, so the header has its words to tell from the position.
        update(
            &mut model,
            UiEvent::MailboxChosen {
                node: [0x9c; 16],
                by_user: false,
            },
        );
        let older = cell_of(&model, "◎ ", 3);
        let effects = press(&mut model, older);
        assert!(effects.is_empty(), "{effects:?}");
        let state = model.map.expect("open");
        assert_eq!(state.centre, projected(43_773_100, 11_256_000));
        assert_eq!(state.selected, Some(PEER));
        update(&mut model, key(KeyCode::Esc));
        assert_eq!(model.message_cursor, 1, "the click selected it");
        // Narrow: `pos 1 min` at the title's right end.
        assert!(view(&model).messages_title_right.starts_with("pos 1 min"));
        let narrow = cells(&model, |t| *t == hit::Target::Position);
        assert_eq!(narrow.len(), "pos 1 min".len(), "{narrow:?}");
        assert!(!cells(&model, |t| *t == hit::Target::Mailbox).is_empty());
        let effects = press(&mut model, narrow[0]);
        assert!(effects.is_empty(), "no fetch: {effects:?}");
        let latest = projected(43_775_000, 11_261_000);
        assert_eq!(model.map.expect("open").centre, latest);
        update(&mut model, key(KeyCode::Esc));
        // Wide: the row under the title, from its second cell.
        update(
            &mut model,
            UiEvent::Resize {
                width: 120,
                height: 40,
            },
        );
        let shown = view(&model);
        let words = text(&shown.messages[0]);
        assert_eq!(
            shown.position_len,
            "at 43.77500 N 11.26100 E · ±5 m · 1 min ago"
                .chars()
                .count(),
            "{words:?}"
        );
        let wide = cells(&model, |t| *t == hit::Target::Position);
        assert_eq!(wide.len(), shown.position_len, "{wide:?}");
        let mailbox = cells(&model, |t| *t == hit::Target::Mailbox);
        assert_eq!(mailbox.len(), "a relay adopted".len(), "{mailbox:?}");
        let effects = press(&mut model, wide[wide.len() - 1]);
        assert!(effects.is_empty(), "no fetch: {effects:?}");
        assert_eq!(model.map.expect("open").centre, latest);
        update(&mut model, key(KeyCode::Esc));
        // `L` from the conversation, `Alt-l` from compose.
        update(&mut model, key(KeyCode::Char('L')));
        assert_eq!(model.map.expect("open").centre, latest);
        update(&mut model, key(KeyCode::Esc));
        update(&mut model, key(KeyCode::Char('i')));
        assert_eq!(model.focus, Pane::Compose);
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::ALT)),
        );
        assert_eq!(model.focus, Pane::Map);
        assert_eq!(model.map.expect("open").selected, Some(PEER));
        update(&mut model, key(KeyCode::Esc));
        // Back in the compose line it was opened from (1531); its Esc
        // leaves for the conversation.
        assert_eq!(model.focus, Pane::Compose);
        update(&mut model, key(KeyCode::Esc));
        assert_eq!(model.focus, Pane::Messages);
        // A peer without a position.
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT)),
        );
        assert_eq!(model.conversations[model.selected].peer, OTHER);
        update(&mut model, key(KeyCode::Char('L')));
        assert!(model.map.is_none());
        assert_eq!(
            model.toasts.last().map(|t| t.text.as_str()),
            Some("no position from Unknown")
        );
        assert!(cells(&model, |t| *t == hit::Target::Position).is_empty());
    }

    // Blocking and names from the palette (1082): blocked is stored
    // silently, and `rename`, `block`, `unblock` and `blocked` run from the
    // palette.

    /// `:` and a palette name, then `Enter`.
    fn palette(model: &mut UiModel<Field>, name: &str) -> Vec<UiEffect> {
        update(model, key(KeyCode::Char(':')));
        typed(model, name);
        update(model, key(KeyCode::Enter))
    }

    fn blocking(blocked: &[[u8; 16]]) -> Names {
        let text: String = blocked
            .iter()
            .map(|p| format!("block {}\n", to_hex(p)))
            .collect();
        Names::new(crate::names::parse_contacts(&text).expect("contacts"))
    }

    /// A message from a blocked peer is received and stored as any other,
    /// and the screen says nothing of it: not in the list, not unread, no
    /// toast, no `new` rule, no pin on the map, no search hit. The `blocked`
    /// view lists it, and `unblock` there brings the conversation back with
    /// its history.
    #[test]
    fn a_blocked_peers_message_is_stored_and_shown_nowhere_until_unblocked() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.names = blocking(&[OTHER]);
        clock(&mut model, 26, 14, 0);
        let toasts = model.toasts.len();
        let effects = arrive(
            &mut model,
            OTHER,
            1,
            "still there? tripod",
            report_of(&fix_at(43_773_100, 11_256_000, two_pm() - 240)),
        );
        assert!(effects.is_empty(), "{effects:?}");
        // Received and stored.
        let c = model
            .conversations
            .iter()
            .position(|c| c.peer == OTHER)
            .expect("kept");
        assert_eq!(model.conversations[c].messages.len(), 1);
        let writes = model.take_writes();
        assert!(
            writes
                .iter()
                .any(|w| matches!(w, StoreWrite::Message { peer, .. } if *peer == OTHER)),
            "{writes:?}"
        );
        // Shown nowhere.
        assert_eq!(model.order(), [0]);
        assert_eq!(model.conversations[c].unread, 0);
        assert_eq!(model.conversations[c].new_rule, None);
        assert_eq!(model.toasts.len(), toasts, "no toast");
        assert_eq!(model.selected, 0, "PEER stays open");
        assert!(
            list_rows(&model).iter().all(|r| !r.contains("cdcd")),
            "{:?}",
            list_rows(&model)
        );
        assert!(model.positions.contains_key(&OTHER), "the position is kept");
        assert!(model.map_pins().is_empty(), "and not pinned");
        assert!(search::hits(
            &model.conversations,
            &model.names,
            &model.order(),
            "tripod",
            &[]
        )
        .is_empty());
        // The `blocked` view lists it, opens it, and its search finds it.
        palette(&mut model, "blocked");
        assert_eq!(model.order(), [c]);
        assert_eq!(model.selected, c);
        let v = view(&model);
        assert_eq!(v.conversations_title, BLOCKED_TITLE);
        assert_eq!(v.conversations_title_right, BLOCKED_RETURNS);
        assert_eq!(v.messages_title, "Unknown  blocked");
        assert!(list_rows(&model).iter().any(|r| r.contains("Unknown")));
        assert_eq!(model.messages_of_selected()[0].body, "still there? tripod");
        // Unblock: back in the list, open, with its history.
        let effects = palette(&mut model, "unblock");
        assert_eq!(
            effects,
            [UiEffect::Contact(Contact {
                peer: OTHER,
                petname: None,
                blocked: false
            })]
        );
        assert!(!model.show_blocked);
        assert_eq!(model.conversations[model.selected].peer, OTHER);
        assert_eq!(model.order().len(), 2);
        assert_eq!(model.messages_of_selected().len(), 1);
        assert_eq!(model.map_pins().len(), 1);
        // Unblocked, the next one counts and toasts as any other.
        update(&mut model, key(KeyCode::Down));
        let open = model.selected;
        assert_ne!(model.conversations[open].peer, OTHER);
        receive(&mut model, OTHER, 2, "now you see me");
        assert_eq!(model.conversations[c].unread, 1);
        assert!(model
            .current_toast()
            .is_some_and(|t| t.text.contains("message from")));
    }

    /// `block` hides the open conversation and opens the next; with only a
    /// blocked peer on the screen nothing is open, and a message from it
    /// does not open one.
    #[test]
    fn block_hides_the_open_conversation_and_writes_the_contact() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        receive(&mut model, PEER, 1, "hi");
        assert_eq!(model.selected, 0);
        let effects = palette(&mut model, "block");
        assert_eq!(
            effects,
            [UiEffect::Contact(Contact {
                peer: PEER,
                petname: None,
                blocked: true
            })]
        );
        assert_eq!(model.names.trust(&PEER), Trust::Blocked);
        assert_eq!(model.conversations[model.selected].peer, OTHER);
        assert_eq!(model.order(), [1]);
        assert!(model
            .current_toast()
            .is_some_and(|t| t.text.starts_with("blocked Unknown ·abab")));
        // Again: said, not written.
        update(&mut model, UiEvent::Run(PaletteCommand::Blocked));
        assert!(palette(&mut model, "block").is_empty());
        assert!(model
            .current_toast()
            .is_some_and(|t| t.text.ends_with("already blocked")));
        update(&mut model, UiEvent::Run(PaletteCommand::Blocked));

        let mut alone = UiModel::<Field>::new(&[]);
        alone.names = blocking(&[PEER]);
        receive(&mut alone, PEER, 1, "hello?");
        assert_eq!(alone.selected, NONE_OPEN);
        assert_eq!(view(&alone).messages_title, "messages");
        assert_eq!(
            list_rows(&alone).join(" ").trim_end(),
            no_conversations(alone.lang),
            "the list is empty, wrapped"
        );
        assert!(palette(&mut alone, "block").is_empty());
        assert!(alone
            .current_toast()
            .is_some_and(|t| t.text == "no conversation here"));
        // A visible peer's message opens its conversation, as on an empty
        // screen.
        receive(&mut alone, OTHER, 2, "me too");
        assert_eq!(alone.conversations[alone.selected].peer, OTHER);
        assert_eq!(alone.conversations[alone.selected].unread, 0);
    }

    /// 1146: a muted conversation counts its unread and raises no toast
    /// until the mute ends; muted always never ends.
    #[test]
    fn a_muted_conversation_counts_and_raises_nothing_until_the_mute_ends() {
        let mut model = UiModel::<Field>::new(&[PEER, OTHER]);
        clock(&mut model, 1, 10, 0);
        let now = model.now.expect("clock").unix_secs;
        let state = |until| crate::home::wire::PeerState {
            muted_until: Some(until),
            ..crate::home::wire::PeerState::default()
        };
        update(
            &mut model,
            UiEvent::PeerState {
                peer: OTHER,
                state: state(now + 3600),
            },
        );
        assert!(model.muted(&OTHER) && !model.muted(&PEER));
        let toasts = model.toasts.len();
        receive(&mut model, OTHER, 1, "quiet please");
        assert_eq!(model.toasts.len(), toasts, "no toast while muted");
        let other = model.conversations.iter().find(|c| c.peer == OTHER);
        assert_eq!(other.map(|c| c.unread), Some(1), "still counted");
        // An hour and a minute later the mute is over.
        clock(&mut model, 1, 11, 1);
        assert!(!model.muted(&OTHER));
        receive(&mut model, OTHER, 2, "loud again");
        assert_eq!(model.toasts.len(), toasts + 1);
        update(
            &mut model,
            UiEvent::PeerState {
                peer: OTHER,
                state: state(state::MUTED_ALWAYS),
            },
        );
        clock(&mut model, 30, 23, 59);
        assert!(model.muted(&OTHER));
        let writes = model.take_writes();
        assert!(writes.iter().any(|w| matches!(
            w,
            StoreWrite::Conversation { peer: OTHER, flags, .. }
                if flags.muted_until == Some(state::MUTED_ALWAYS)
        )));
    }

    /// 1146: the contact facts say when the peer last wrote and how the
    /// last message to it went.
    #[test]
    fn the_contact_facts_say_the_last_message_each_way() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        clock(&mut model, 1, 10, 0);
        assert_eq!(model.contact_facts(&PEER).last_received, None);
        receive(&mut model, PEER, 1, "hi");
        say(&mut model, "hello back");
        let facts = model.contact_facts(&PEER);
        assert_eq!(facts.last_received, model.now);
        assert_eq!(facts.last_sent.map(|(_, words)| words), Some("queued"));
        let messages = &mut model.conversations[0].messages;
        let last_sent = messages.iter().rposition(|m| m.outbound).expect("sent");
        messages.get_mut(last_sent).expect("sent").delivery = Some(DeliveryState::InMailbox);
        assert_eq!(
            model.contact_facts(&PEER).last_sent.map(|(_, words)| words),
            Some("left at a propagation node")
        );
        assert!(!model.contact_facts(&PEER).verified);
    }

    /// 1146: verified is a local flag the store keeps; it goes to no home
    /// node, and setting it twice says it once.
    #[test]
    fn verified_is_kept_on_this_device_only() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.home_client = true;
        assert!(!model.verified(&PEER));
        update(
            &mut model,
            UiEvent::Verify {
                peer: PEER,
                verified: true,
            },
        );
        let toasts = model.toasts.len();
        update(
            &mut model,
            UiEvent::Verify {
                peer: PEER,
                verified: true,
            },
        );
        assert_eq!(model.toasts.len(), toasts);
        assert!(model.verified(&PEER));
        assert!(model.take_peer_states().is_empty());
        assert!(model.take_writes().iter().any(|w| matches!(
            w,
            StoreWrite::Conversation { peer: PEER, flags, .. } if flags.verified
        )));
        update(
            &mut model,
            UiEvent::Verify {
                peer: PEER,
                verified: false,
            },
        );
        assert!(!model.verified(&PEER));
    }

    /// 1146: an archived conversation leaves the list for the archived
    /// view; a new message brings it back unless it is muted.
    #[test]
    fn an_archived_conversation_comes_back_with_a_message_unless_muted() {
        let mut model = UiModel::<Field>::new(&[]);
        receive(&mut model, PEER, 1, "one");
        receive(&mut model, OTHER, 2, "two");
        let archived = crate::home::wire::PeerState {
            archived: true,
            ..crate::home::wire::PeerState::default()
        };
        update(
            &mut model,
            UiEvent::PeerState {
                peer: OTHER,
                state: archived,
            },
        );
        let peers = |model: &UiModel<Field>, order: Vec<usize>| -> Vec<[u8; 16]> {
            order.iter().map(|&i| model.conversations[i].peer).collect()
        };
        assert_eq!(peers(&model, model.order()), [PEER]);
        assert_eq!(peers(&model, model.archived_order()), [OTHER]);
        palette(&mut model, "archived");
        assert_eq!(peers(&model, model.order()), [OTHER]);
        palette(&mut model, "archived");
        receive(&mut model, OTHER, 3, "back");
        assert_eq!(peers(&model, model.order()), [OTHER, PEER]);
        assert!(model.archived_order().is_empty());
        // Muted and archived: it stays where it is.
        update(
            &mut model,
            UiEvent::PeerState {
                peer: OTHER,
                state: crate::home::wire::PeerState {
                    muted_until: Some(state::MUTED_ALWAYS),
                    ..archived
                },
            },
        );
        receive(&mut model, OTHER, 4, "still away");
        assert_eq!(peers(&model, model.order()), [PEER]);
        // Marked unread and read again, on this device.
        update(
            &mut model,
            UiEvent::MarkUnread {
                peer: PEER,
                unread: true,
            },
        );
        let unread = |model: &UiModel<Field>| {
            model
                .conversations
                .iter()
                .find(|c| c.peer == PEER)
                .map_or(0, |c| c.unread)
        };
        assert_eq!(unread(&model), 1);
        update(
            &mut model,
            UiEvent::MarkUnread {
                peer: PEER,
                unread: false,
            },
        );
        assert_eq!(unread(&model), 0);
    }

    /// 1146: on a client of a home node pin, block, archive and mute go to
    /// the home node whole, the palette's as well; one pulled is applied
    /// and never handed back.
    #[test]
    fn a_peer_state_travels_through_the_home_node() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        receive(&mut model, PEER, 1, "hi");
        palette(&mut model, "pin");
        assert!(model.take_peer_states().is_empty(), "no home node");
        model.home_client = true;
        palette(&mut model, "pin");
        let effects = palette(&mut model, "block");
        assert!(matches!(
            effects.as_slice(),
            [UiEffect::Contact(Contact { blocked: true, .. })]
        ));
        let unpinned = crate::home::wire::PeerState::default();
        assert_eq!(
            model.take_peer_states(),
            [
                (PEER, unpinned),
                (
                    PEER,
                    crate::home::wire::PeerState {
                        blocked: true,
                        ..unpinned
                    }
                )
            ]
        );
        // The home's echo of the last one first (1614): until then a pulled
        // state is the home's older word.
        let blocked = crate::home::wire::PeerState {
            blocked: true,
            ..unpinned
        };
        assert!(update(
            &mut model,
            UiEvent::Engine(OutboxEvent::HomePeerState {
                destination: PEER,
                state: blocked,
            }),
        )
        .is_empty());
        let pulled = crate::home::wire::PeerState {
            pinned: true,
            archived: true,
            ..unpinned
        };
        let effects = update(
            &mut model,
            UiEvent::Engine(OutboxEvent::HomePeerState {
                destination: PEER,
                state: pulled,
            }),
        );
        assert!(matches!(
            effects.as_slice(),
            [UiEffect::Contact(Contact { blocked: false, .. })]
        ));
        assert_eq!(model.peer_state(&PEER), pulled);
        assert!(model.take_peer_states().is_empty(), "never handed back");
        assert!(update(
            &mut model,
            UiEvent::Engine(OutboxEvent::HomePeerState {
                destination: PEER,
                state: pulled,
            })
        )
        .is_empty());
    }

    /// 1139: on a client of a home node a rename goes to the home node as
    /// well, the palette's and a frontend's own alike; one pulled from the
    /// home node is written here and never handed back. Off a home node
    /// nothing is queued.
    #[test]
    fn a_petname_travels_through_the_home_node() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        announced(&mut model, PEER, "Alice");
        update(
            &mut model,
            UiEvent::Rename {
                peer: PEER,
                name: "Mum".into(),
            },
        );
        assert!(model.take_petnames().is_empty(), "no home node");
        model.home_client = true;
        // The palette holds "Mum" to edit: emptied first.
        palette(&mut model, "rename");
        update(&mut model, ctrl_key('u'));
        typed(&mut model, "Mama");
        update(&mut model, key(KeyCode::Enter));
        let effects = update(
            &mut model,
            UiEvent::Rename {
                peer: PEER,
                name: " ".into(),
            },
        );
        assert_eq!(
            effects,
            [UiEffect::Contact(Contact {
                peer: PEER,
                petname: None,
                blocked: false
            })]
        );
        // The newest of the peer is what is owed (1635).
        assert_eq!(model.take_petnames(), [(PEER, None)]);
        update(
            &mut model,
            UiEvent::PetnamesHanded {
                taken: vec![(PEER, None)],
            },
        );
        assert!(model.take_petnames().is_empty(), "the home took it");
        // Pulled: the reader's word here, the file told, nothing queued.
        // Taken by the home after this device's word (1635).
        let pulled = |petname: Option<&str>| {
            UiEvent::Engine(OutboxEvent::HomePetname {
                destination: PEER,
                petname: petname.map(str::to_string),
                at: 1,
            })
        };
        // The home's echo of the clear first (1614): nothing to change.
        assert!(update(&mut model, pulled(None)).is_empty());
        let effects = update(&mut model, pulled(Some("Mutti \u{202e}")));
        assert_eq!(
            effects,
            [UiEffect::Contact(Contact {
                peer: PEER,
                petname: Some("Mutti".into()),
                blocked: false
            })]
        );
        assert_eq!(model.names.label(&PEER, model.lang), "Mutti");
        assert!(model.take_petnames().is_empty(), "never handed back");
        assert!(update(&mut model, pulled(Some("Mutti"))).is_empty());
        update(&mut model, pulled(None));
        assert_eq!(model.names.label(&PEER, model.lang), "Alice");
    }

    /// 1635: a petname this device set and the home node has not taken
    /// yet is the reader's newest word on the peer. The home stamps an
    /// entry when it takes it, so the one this device still owes will be
    /// the home's newest: an older entry a pull brings in the meantime,
    /// the "Mum" the reader just cleared, changes nothing here.
    #[test]
    fn a_petname_owed_to_the_home_outlives_an_older_pulled_one() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        announced(&mut model, PEER, "Alice");
        model.home_client = true;
        let rename = |name: &str| UiEvent::Rename {
            peer: PEER,
            name: name.into(),
        };
        // Stamped by the home at `at`; this test's model has no clock, so
        // a hand's answer came at 0.
        let pulled_at = |petname: Option<&str>, at: i64| {
            UiEvent::Engine(OutboxEvent::HomePetname {
                destination: PEER,
                petname: petname.map(str::to_string),
                at,
            })
        };
        let pulled = |petname: Option<&str>| pulled_at(petname, 0);
        let handed = |taken: &[Option<&str>]| UiEvent::PetnamesHanded {
            taken: taken
                .iter()
                .map(|name| (PEER, name.map(str::to_string)))
                .collect(),
        };
        update(&mut model, rename("Mum"));
        assert_eq!(model.take_petnames(), [(PEER, Some("Mum".to_string()))]);
        // One hand at a time: the clearing waits for the first.
        update(&mut model, rename(""));
        assert!(model.take_petnames().is_empty(), "a hand is under way");
        update(&mut model, handed(&[Some("Mum")]));
        // The home took "Mum", and the clearing is owed: the pull's "Mum"
        // is older than it and changes nothing.
        assert!(update(&mut model, pulled(Some("Mum"))).is_empty());
        assert_eq!(model.names.label(&PEER, model.lang), "Alice");
        // Kept in the store, so a restart hands it again.
        let owed = |writes: Vec<StoreWrite>| {
            writes
                .into_iter()
                .filter_map(|write| match write {
                    StoreWrite::OwedPetnames(owed) => Some(owed),
                    _ => None,
                })
                .next_back()
        };
        let kept = owed(model.take_writes()).expect("the owed kept");
        assert_eq!(kept, BTreeMap::from([(PEER, None)]));
        // A restart: the owed handed first, and over the first pull.
        let mut again = UiModel::<Field>::new(&[PEER]);
        announced(&mut again, PEER, "Alice");
        again.home_client = true;
        again.restore_owed_petnames(kept);
        assert_eq!(again.take_petnames(), [(PEER, None)]);
        // A hand the home did not take: still owed, handed again.
        update(&mut again, handed(&[]));
        assert_eq!(again.take_petnames(), [(PEER, None)]);
        assert!(update(&mut again, pulled(Some("Mum"))).is_empty());
        assert_eq!(again.names.label(&PEER, again.lang), "Alice");
        // The home's echo of the clearing: owed no more, kept so.
        update(&mut again, pulled(None));
        assert_eq!(owed(again.take_writes()), Some(BTreeMap::new()));
        update(&mut again, handed(&[None]));
        assert!(again.take_petnames().is_empty());
        // A name another device set later is the reader's word here.
        update(&mut again, pulled(Some("Mutti")));
        assert_eq!(again.names.label(&PEER, again.lang), "Mutti");

        // Taken: a pull under way when the home took the clearing may
        // still bring the older "Mum" (the forge's pipeline #620); the
        // home stamped it before the answer came, so it changes nothing.
        // A name the home took after it does, and once a pull ended the
        // next brings nothing older.
        let mut taken = UiModel::<Field>::new(&[PEER]);
        announced(&mut taken, PEER, "Alice");
        taken.home_client = true;
        update(&mut taken, rename("Mum"));
        update(&mut taken, rename(""));
        assert_eq!(taken.take_petnames(), [(PEER, None)]);
        update(&mut taken, handed(&[None]));
        assert!(update(&mut taken, pulled_at(Some("Mum"), 0)).is_empty());
        assert_eq!(taken.names.label(&PEER, taken.lang), "Alice");
        update(&mut taken, pulled_at(Some("Mutti"), 1));
        assert_eq!(taken.names.label(&PEER, taken.lang), "Mutti");
        update(&mut taken, rename(""));
        assert_eq!(taken.take_petnames(), [(PEER, None)]);
        update(&mut taken, handed(&[None]));
        update(&mut taken, UiEvent::FetchDone(Ok(0)));
        update(&mut taken, pulled_at(Some("Mum"), 0));
        assert_eq!(taken.names.label(&PEER, taken.lang), "Mum");
        // Off a home node nothing is owed.
        let mut alone = UiModel::<Field>::new(&[PEER]);
        update(
            &mut alone,
            UiEvent::Rename {
                peer: PEER,
                name: "Mum".into(),
            },
        );
        assert!(owed(alone.take_writes()).is_none());
    }

    /// 1614: the home's word on a peer that a pull brings while this
    /// device's own newer word is still on its way there is the older one,
    /// and the label does not depend on which arrives first. The phone set
    /// "Mum", the home took it, the phone cleared it, and the pull ran
    /// before the clear reached the home: the row stays the address book's
    /// name. The home's echo of the clear settles it, and a later name from
    /// another device is taken again.
    #[test]
    fn a_pull_older_than_the_readers_own_rename_does_not_bring_it_back() {
        let labels = |pull_first: bool| -> Vec<String> {
            let mut model = UiModel::<Field>::new(&[PEER]);
            model.home_client = true;
            model.names.link(PEER, "Alice Schmidt", "0r1-ALICE");
            update(
                &mut model,
                UiEvent::Rename {
                    peer: PEER,
                    name: "Mum".into(),
                },
            );
            let pulled = |petname: Option<&str>| {
                UiEvent::Engine(OutboxEvent::HomePetname {
                    destination: PEER,
                    petname: petname.map(str::to_string),
                    at: 0,
                })
            };
            let mut seen = Vec::new();
            if pull_first {
                // The home's echo of "Mum", pulled before the clear.
                update(&mut model, pulled(Some("Mum")));
            }
            update(
                &mut model,
                UiEvent::Rename {
                    peer: PEER,
                    name: String::new(),
                },
            );
            // Only the newest word of the peer is owed (1635).
            assert_eq!(model.take_petnames(), [(PEER, None)]);
            seen.push(model.names.label(&PEER, model.lang));
            // The pull that ran before the clear reached the home.
            update(&mut model, pulled(Some("Mum")));
            seen.push(model.names.label(&PEER, model.lang));
            // The clear's echo, then another device's name.
            update(&mut model, pulled(None));
            seen.push(model.names.label(&PEER, model.lang));
            update(&mut model, pulled(Some("Mutti")));
            seen.push(model.names.label(&PEER, model.lang));
            assert!(model.take_petnames().is_empty(), "never handed back");
            seen
        };
        let expected = ["Alice Schmidt", "Alice Schmidt", "Alice Schmidt", "Mutti"];
        assert_eq!(labels(false), expected);
        assert_eq!(labels(true), expected);
    }

    /// 1614: as a petname, a peer state the reader set is not undone by a
    /// pull of the home's older one; its echo settles it.
    #[test]
    fn a_pull_older_than_the_readers_own_peer_state_does_not_bring_it_back() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        receive(&mut model, PEER, 1, "hi");
        model.home_client = true;
        let muted = crate::home::wire::PeerState {
            muted_until: Some(i64::MAX),
            ..crate::home::wire::PeerState::default()
        };
        let pulled = |state| {
            UiEvent::Engine(OutboxEvent::HomePeerState {
                destination: PEER,
                state,
            })
        };
        palette(&mut model, "pin");
        let pinned = model.peer_state(&PEER);
        assert!(pinned.pinned);
        // The home's older word: muted, not pinned.
        update(&mut model, pulled(muted));
        assert_eq!(model.peer_state(&PEER), pinned, "the reader's newer word");
        // The echo, then another device's word.
        update(&mut model, pulled(pinned));
        update(&mut model, pulled(muted));
        assert_eq!(model.peer_state(&PEER), muted);
        assert_eq!(model.take_peer_states(), [(PEER, pinned)]);
    }

    /// `rename` asks in the palette, holding the petname to edit: `Enter`
    /// sets the typed one, sanitised, and an empty line clears it.
    /// 1279, 1443: `my code` and `P` open the profile, the scene's, in the
    /// detail view: the name, the address, the code a friend pastes and
    /// its QR in half blocks under the text, never wrapped. Without a code
    /// it opens all the same, without the code and with nothing to share;
    /// with one, `s` copies it and says so; Esc closes it.
    #[test]
    fn my_code_opens_the_profile_with_the_code_and_its_qr() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.address = Some([0xcd; 16]);
        assert!(palette(&mut model, "my code").is_empty());
        let detail = view(&model).detail.expect("the profile's view");
        assert_eq!(detail.title, "Your profile");
        assert!(detail.fixed.is_empty(), "no code, no QR: {detail:?}");
        assert!(update(&mut model, key(KeyCode::Char('s'))).is_empty());
        update(&mut model, key(KeyCode::Esc));
        assert!(!model.detail_open());

        model.own_code = Some(OwnCode::of(
            "lxma://00:11".to_string(),
            Some((1, vec![true])),
        ));
        update(&mut model, key(KeyCode::Char('P')));
        assert!(model.profile_open, "P opens it from the list");
        let detail = view(&model).detail.expect("the profile's view");
        assert!(
            detail.rows.contains(&"lxma://00:11".to_string()),
            "{detail:?}"
        );
        assert!(
            detail
                .rows
                .contains(&"cdcd cdcd cdcd cdcd cdcd cdcd cdcd cdcd".to_string()),
            "{detail:?}"
        );
        // A 1x1 code in a quiet zone of four: nine modules a side, five
        // rows of half blocks, drawn as they are after the text.
        assert_eq!(detail.fixed.len(), 5);
        let (_, rows) = detail_layout(&detail, Rect::new(0, 0, 80, 40));
        assert_eq!(rows[rows.len() - 5..], detail.fixed[..]);
        assert_eq!(
            update(&mut model, key(KeyCode::Char('s'))),
            [UiEffect::Copy("lxma://00:11".to_string())]
        );
        assert_eq!(
            model.current_toast().map(|t| t.text.as_str()),
            Some("Your address is on the clipboard")
        );
        update(&mut model, key(KeyCode::Esc));
        assert!(!model.detail_open());
        assert!(view(&model).detail.is_none());
    }

    #[test]
    fn rename_takes_text_in_the_palette_and_empty_clears() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        announced(&mut model, PEER, "Alice");
        assert!(palette(&mut model, "rename").is_empty());
        let asking = model.palette.as_ref().expect("the palette asks");
        assert_eq!(asking.ask, Some(contacts::Ask::Rename(PEER)));
        assert!(asking.matches().is_empty());
        let rows: Vec<String> = view(&model)
            .palette
            .expect("drawn")
            .rows
            .iter()
            .map(text)
            .collect();
        assert_eq!(
            rows,
            ["your name for Alice", "Enter sets it, empty clears it"]
        );
        assert_eq!(view(&model).footer.len(), 2);
        typed(&mut model, "Mum ✓");
        let effects = update(&mut model, key(KeyCode::Enter));
        assert_eq!(
            effects,
            [UiEffect::Contact(Contact {
                peer: PEER,
                petname: Some("Mum ?".into()),
                blocked: false
            })]
        );
        assert!(model.palette.is_none());
        assert_eq!(
            view(&model).messages_title,
            "Mum ?  known · announces Alice"
        );
        assert!(model
            .current_toast()
            .is_some_and(|t| t.text == "named Mum ?"));
        // The palette holds the petname to edit; `Enter` unchanged writes
        // nothing.
        palette(&mut model, "rename");
        assert_eq!(
            model.palette.as_ref().map(|p| p.query.value()),
            Some("Mum ?")
        );
        assert!(update(&mut model, key(KeyCode::Enter)).is_empty());
        // Emptied, it clears.
        palette(&mut model, "rename");
        update(&mut model, ctrl_key('u'));
        let effects = update(&mut model, key(KeyCode::Enter));
        assert_eq!(
            effects,
            [UiEffect::Contact(Contact {
                peer: PEER,
                petname: None,
                blocked: false
            })]
        );
        assert_eq!(view(&model).messages_title, "Alice");
        // Nothing visible is refused, not written.
        palette(&mut model, "rename");
        typed(&mut model, "\u{200b}");
        assert!(update(&mut model, key(KeyCode::Enter)).is_empty());
        assert_eq!(model.names.petname(&PEER), None);
        // `Esc` leaves without a write.
        palette(&mut model, "rename");
        typed(&mut model, "Bob");
        update(&mut model, key(KeyCode::Esc));
        assert!(model.palette.is_none());
        assert_eq!(model.names.petname(&PEER), None);
    }

    // Reactions (1104): FIELD_REACTION as Columba, Ratspeak and MeshChatX
    // send it, drawn under the target and never as a message of its own.

    const THUMBS: &str = "\u{1F44D}";
    const HEART: &str = "\u{2764}\u{FE0F}";

    /// The reaction field as `app` sends it, with the content beside it.
    /// Columba (`NativeRnsBackendImpl.sendReaction`, torlando-tech/columba
    /// `9222e38`, `rns-backend-kt/.../NativeRnsBackendImpl.kt:1498-1522`):
    /// the field alone, empty content; MeshChatX the same
    /// (`send_reaction`, Quad4-Software/MeshChatX `4378392`,
    /// `meshchatx/meshchat.py:11214-11231`). Ratspeak 1.0.35
    /// (`ratspeak_chat_custom_fields`, ratspeak/Ratspeak `7da1fe3`,
    /// `crates/ratspeak-runtime/src/lxmf.rs:410-435`) adds its own envelope
    /// in `FIELD_CUSTOM_TYPE`/`FIELD_CUSTOM_DATA`; 1.0.25 also put a text in
    /// the content for clients without reactions (ratspeak/Ratspeak#42).
    fn shaped(app: &str, target: [u8; 32], content: &str) -> (Vec<u8>, Vec<(i64, Vec<u8>)>) {
        use leviculum_lxmf::constants::{FIELD_CUSTOM_DATA, FIELD_CUSTOM_TYPE, FIELD_REACTION};
        use leviculum_lxmf::msgpack;
        let field = leviculum_lxmf::reaction::Reaction {
            to: target,
            content: content.into(),
        }
        .encode_field_value();
        let mut fields = vec![(FIELD_REACTION, field)];
        let mut ratspeak = || {
            let mut kind = Vec::new();
            msgpack::bin(&mut kind, b"ratspeak.chat.v2");
            let mut payload = Vec::new();
            msgpack::map(&mut payload, 4);
            for (k, v) in [("kind", "reaction"), ("emoji", content), ("action", "add")] {
                msgpack::string(&mut payload, k);
                msgpack::string(&mut payload, v);
            }
            msgpack::string(&mut payload, "target");
            msgpack::bin(&mut payload, &target);
            let mut data = Vec::new();
            msgpack::bin(&mut data, &payload);
            fields.push((FIELD_CUSTOM_TYPE, kind));
            fields.push((FIELD_CUSTOM_DATA, data));
        };
        let body = match app {
            "columba" | "meshchatx" => Vec::new(),
            "ratspeak-1.0.35" => {
                ratspeak();
                Vec::new()
            }
            "ratspeak-1.0.25" => {
                ratspeak();
                format!("Reacted to your message with {content}.").into_bytes()
            }
            _ => panic!("no shape for {app}"),
        };
        (body, fields)
    }

    fn reacted(model: &mut UiModel<Field>, id: [u8; 32], app: &str, to: [u8; 32], what: &str) {
        let (body, fields) = shaped(app, to, what);
        engine(
            model,
            OutboxEvent::Received {
                sent_at: 0,
                message_id: id,
                source: PEER,
                title: Vec::new(),
                body,
                fields,
                verified: true,
            },
        );
    }

    fn received(model: &mut UiModel<Field>, id: [u8; 32], body: &str) {
        engine(
            model,
            OutboxEvent::Received {
                sent_at: 0,
                message_id: id,
                source: PEER,
                title: Vec::new(),
                body: body.as_bytes().to_vec(),
                fields: Vec::new(),
                verified: true,
            },
        );
    }

    /// Every deployed shape is one reaction under its target and no
    /// message: no block, no unread, no `new` rule, Ratspeak's fallback
    /// text nowhere on screen; the same reaction twice counts once.
    #[test]
    fn each_clients_reaction_is_one_reaction_and_no_message() {
        for app in ["columba", "meshchatx", "ratspeak-1.0.35", "ratspeak-1.0.25"] {
            let mut model = UiModel::<Field>::new(&[PEER]);
            received(&mut model, ID, "lunch at noon?");
            let before = shown(&model);
            model.toasts.clear();
            reacted(&mut model, ID2, app, ID, THUMBS);
            reacted(&mut model, ID2, app, ID, THUMBS);
            reacted(&mut model, [0x33; 32], app, ID, THUMBS);
            let rows = shown(&model);
            assert_eq!(rows.len(), before.len() + 1, "{app}: {rows:?}");
            assert_eq!(rows.last().map(String::as_str), Some(THUMBS), "{app}");
            assert!(
                !rows.iter().any(|r| r.contains("Reacted")),
                "{app}: {rows:?}"
            );
            let conversation = &model.conversations[0];
            assert_eq!(conversation.messages.len(), 3, "{app}: deduplicated by id");
            assert_eq!(conversation.unread, 0, "{app}");
            assert_eq!(conversation.new_rule, None, "{app}");
            assert_eq!(
                model.message_cursor, 0,
                "{app}: the cursor stays on the text"
            );
            assert_eq!(
                model.current_toast().map(|t| t.text.clone()),
                Some(format!("Unknown reacted {THUMBS}")),
                "{app}"
            );
            assert_eq!(
                text(&view(&model).conversations[1]).trim(),
                format!("▌reacted {THUMBS}"),
                "{app}"
            );
        }
    }

    /// 1201: a shared position is a report of ours with no text, its one
    /// field the Telemeter of the fix; its bubble names where we were, the
    /// list calls it a position, and nothing of it becomes a peer's pin.
    #[test]
    fn a_shared_position_is_a_report_of_ours_and_its_bubble_says_where() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.address = Some(OTHER);
        clock(&mut model, 26, 14, 0);
        let location = leviculum_lxmf::telemetry::Location::saturating(
            52_520_008,
            13_404_954,
            3_400,
            0,
            0,
            500,
            1_790_000_000,
        );
        let effects = update(
            &mut model,
            UiEvent::SharePosition {
                peer: PEER,
                location,
            },
        );
        let [UiEffect::Send { to, body, fields }] = effects.as_slice() else {
            panic!("one send: {effects:?}");
        };
        assert_eq!((*to, body.as_str()), (PEER, ""));
        // The mark names its writer since 1482.
        let mut expected = position_fields(location);
        leviculum_lxmf::lev::Lev {
            capabilities: Some(groups::client_caps(true)),
            writer: Some(OTHER),
            ..leviculum_lxmf::lev::Lev::default()
        }
        .put_into(&mut expected);
        assert_eq!(
            *fields, expected,
            "the Telemeter and the receipt mark alone"
        );
        let read = telemetry::read(OTHER, fields, None);
        assert_eq!(read.fixes.len(), 1, "{read:?}");
        assert_eq!(read.fixes[0].location, location);
        let fix = model.message_fix(0, 0).expect("our bubble names the fix");
        assert_eq!(fix.peer, OTHER);
        assert_eq!(fix.location.latitude_e6, 52_520_008);
        let message = &model.conversations[0].messages[0];
        assert!(message.outbound && message.is_position() && !message.is_report());
        assert_eq!(chat::kind(message), "position");
        assert!(
            !model.positions.contains_key(&PEER),
            "our fix is no pin of the peer's"
        );
        // Without our address there is no one it measured.
        model.address = None;
        assert_eq!(model.message_fix(0, 0), None);
    }

    /// Ours and theirs on one message: each content once with its count,
    /// ours in brackets.
    #[test]
    fn two_reactions_count_and_ours_is_marked() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        received(&mut model, ID, "lunch at noon?");
        reacted(&mut model, ID2, "columba", ID, THUMBS);
        reacted(&mut model, [0x33; 32], "columba", ID, HEART);
        assert_eq!(shown(&model).last().unwrap(), &format!("{THUMBS}  {HEART}"));
        let effects = model.react(PEER, ID, THUMBS);
        assert_eq!(
            shown(&model).last().unwrap(),
            &format!("[{THUMBS} 2]  {HEART}")
        );
        let [UiEffect::Send { to, body, fields }] = effects.as_slice() else {
            panic!("one send: {effects:?}");
        };
        assert_eq!((*to, body.as_str()), (PEER, ""));
        let (key, value) = &fields[0];
        assert_eq!(*key, leviculum_lxmf::constants::FIELD_REACTION);
        assert_eq!(
            leviculum_lxmf::reaction::Reaction::decode_field_value(value),
            Some(leviculum_lxmf::reaction::Reaction {
                to: ID,
                content: THUMBS.into()
            })
        );
        assert_eq!(model.sending.len(), 1, "delivered like any send");
    }

    /// A reaction whose target is not here yet is kept, drawn nowhere,
    /// and attaches when the target arrives.
    /// 1447 (walk 1 finding 24): the list's line of a reaction someone
    /// else sent names who, in the reader's language; without a name it
    /// says what it said before.
    #[test]
    fn a_reaction_line_in_the_list_names_its_subject() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.lang = strings::Lang::De;
        received(&mut model, ID, "lunch at noon?");
        reacted(&mut model, ID2, "columba", ID, "❤️");
        let line = |model: &UiModel<Field>| chat::list_row(model, &PEER).last;
        assert_eq!(
            line(&model),
            "hat mit ❤️ reagiert",
            "no name known: the control"
        );
        model.names.remember(PEER, "Lew Mobile");
        assert_eq!(line(&model), "Lew Mobile hat mit ❤️ reagiert");
        model.lang = strings::Lang::En;
        assert_eq!(line(&model), "Lew Mobile reacted ❤️");
    }

    #[test]
    fn a_reaction_before_its_target_attaches_when_the_target_arrives() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        reacted(&mut model, ID2, "columba", ID, THUMBS);
        // The held reaction is a row of the conversation, drawn nowhere.
        assert!(shown(&model).is_empty(), "{:?}", shown(&model));
        received(&mut model, ID, "lunch at noon?");
        let rows = shown(&model);
        assert_eq!(rows.last().unwrap(), THUMBS, "{rows:?}");
        assert_eq!(model.message_cursor, 1, "on the text, past the reaction");
    }

    /// A blocked peer's reaction is kept and says nothing (1082).
    #[test]
    fn a_blocked_peers_reaction_is_kept_silently() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        received(&mut model, ID, "lunch at noon?");
        let mut contact = model.names.contacts.of(&PEER);
        contact.blocked = true;
        model.names.contacts.set(&contact);
        model.toasts.clear();
        reacted(&mut model, ID2, "columba", ID, THUMBS);
        assert_eq!(model.conversations[0].messages.len(), 2, "kept");
        assert!(model.current_toast().is_none());
    }

    /// `e` opens the picker on the message under the cursor, a digit sends
    /// that quick reaction, typed text and `Enter` any other.
    #[test]
    fn the_picker_sends_a_quick_or_a_typed_reaction() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        received(&mut model, ID, "lunch at noon?");
        update(&mut model, key(KeyCode::Tab));
        update(&mut model, key(KeyCode::Char('e')));
        let palette = model.palette.as_ref().expect("the picker");
        assert_eq!(
            palette.ask,
            Some(contacts::Ask::React {
                peer: PEER,
                target: ID
            })
        );
        assert_eq!(
            model.ask_rows(palette.ask.unwrap())[0],
            "react to: lunch at noon?"
        );
        let effects = update(&mut model, key(KeyCode::Char('2')));
        assert!(model.palette.is_none());
        assert!(matches!(effects.as_slice(), [UiEffect::Send { .. }]));
        assert_eq!(shown(&model).last().unwrap(), &format!("[{HEART}]"));
        update(&mut model, UiEvent::Run(PaletteCommand::React));
        typed(&mut model, "ok");
        update(&mut model, key(KeyCode::Enter));
        assert_eq!(shown(&model).last().unwrap(), &format!("[{HEART}]  [ok]"));
        // A send not yet queued has no id to react to.
        update(&mut model, key(KeyCode::Tab));
        typed(&mut model, "see you");
        update(&mut model, key(KeyCode::Enter));
        update(&mut model, key(KeyCode::BackTab));
        update(&mut model, UiEvent::Run(PaletteCommand::React));
        assert!(model.palette.is_none());
    }

    // Replies, forwards and a frontend's delete (1137).

    /// A reply goes out with the target's id and its quote, the shape the
    /// deployed clients read, the text as content; the bubble above it
    /// shows the target's text, ours or theirs. One whose target this
    /// device lacks shows the quote the sender put in.
    #[test]
    fn a_reply_carries_its_target_and_quote() {
        use leviculum_lxmf::reply::Reply;
        let mut model = UiModel::<Field>::new(&[PEER]);
        received(&mut model, ID, "lunch at noon?\nor later");
        let effects = update(
            &mut model,
            UiEvent::Reply {
                peer: PEER,
                target: ID,
                body: "noon".to_string(),
            },
        );
        let [UiEffect::Send { to, body, fields }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert_eq!((*to, body.as_str()), (PEER, "noon"));
        assert_eq!(
            Reply::from_fields(fields),
            Some(Reply {
                to: ID,
                quote: Some("lunch at noon?".to_string())
            })
        );
        let c = 0;
        let m = model.conversations[c].messages.len() - 1;
        let quoted = model.quoted(c, m).expect("quoted");
        assert_eq!(
            (quoted.text.as_str(), quoted.ours),
            ("lunch at noon?", Some(false))
        );
        assert_eq!(model.message_cursor, m);

        let elsewhere = Reply {
            to: [0x77; 32],
            quote: Some("an old one".to_string()),
        };
        engine(
            &mut model,
            OutboxEvent::Received {
                sent_at: 0,
                message_id: ID2,
                source: PEER,
                title: Vec::new(),
                body: b"yes".to_vec(),
                fields: elsewhere.fields(),
                verified: true,
            },
        );
        let m = model.conversations[c].messages.len() - 1;
        let quoted = model.quoted(c, m).expect("quoted");
        assert_eq!((quoted.text.as_str(), quoted.ours), ("an old one", None));
        assert!(update(
            &mut model,
            UiEvent::Reply {
                peer: PEER,
                target: ID,
                body: " ".to_string()
            }
        )
        .is_empty());
    }

    /// A received photo in the terminal (1220): its line alone where it has
    /// no text, above the text where it has one (1268), ASCII where the
    /// screen is; `Enter` on it asks the shell to show it whole, and a
    /// forward carries it.
    #[test]
    fn the_terminal_says_photo_and_enter_shows_it() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(
            &mut model,
            UiEvent::Resize {
                width: 100,
                height: 30,
            },
        );
        let photo = photos::photo_fields("jpg", vec![1; 278_601]);
        engine(
            &mut model,
            OutboxEvent::Received {
                sent_at: 0,
                message_id: ID,
                source: PEER,
                title: Vec::new(),
                body: Vec::new(),
                fields: photo.clone(),
                verified: true,
            },
        );
        engine(
            &mut model,
            OutboxEvent::Received {
                sent_at: 0,
                message_id: ID2,
                source: PEER,
                title: Vec::new(),
                body: b"the view".to_vec(),
                fields: photo,
                verified: true,
            },
        );
        let shown = |model: &UiModel<Field>| -> Vec<String> {
            view(model).messages.iter().map(text_of).collect()
        };
        let rows = shown(&model);
        assert_eq!(
            rows.iter()
                .filter(|r| r.contains("▣ photo · 279 KB"))
                .count(),
            2,
            "{rows:#?}"
        );
        let text = rows
            .iter()
            .position(|r| r.contains("the view"))
            .expect("the text");
        assert!(
            text > 0 && rows[text - 1].contains("▣ photo"),
            "above its text (1268): {rows:#?}"
        );
        model.ascii = true;
        assert!(shown(&model)
            .iter()
            .any(|r| r.contains("[#] photo - 279 KB")));
        model.focus = Pane::Messages;
        model.message_cursor = 0;
        let photo_key = model.conversations[0].messages[0].activity;
        let effects = update(&mut model, key(KeyCode::Enter));
        assert_eq!(
            effects,
            [UiEffect::ShowPhoto {
                peer: PEER,
                key: photo_key
            }]
        );
    }

    /// A received file in the terminal (1221): its name, made safe, and
    /// size alone or above its text (1268), ASCII where the screen is;
    /// `Enter` on it asks the shell to save and open it.
    #[test]
    fn the_terminal_says_a_file_and_enter_opens_it() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(
            &mut model,
            UiEvent::Resize {
                width: 100,
                height: 30,
            },
        );
        let file = files::file_fields("../notes.txt", vec![1; 1_400]);
        engine(
            &mut model,
            OutboxEvent::Received {
                sent_at: 0,
                message_id: ID,
                source: PEER,
                title: Vec::new(),
                body: Vec::new(),
                fields: file.clone(),
                verified: true,
            },
        );
        engine(
            &mut model,
            OutboxEvent::Received {
                sent_at: 0,
                message_id: ID2,
                source: PEER,
                title: Vec::new(),
                body: b"the minutes".to_vec(),
                fields: file,
                verified: true,
            },
        );
        let shown = |model: &UiModel<Field>| -> Vec<String> {
            view(model).messages.iter().map(text_of).collect()
        };
        let rows = shown(&model);
        assert_eq!(
            rows.iter()
                .filter(|r| r.contains("▤ notes.txt · 2 KB"))
                .count(),
            2,
            "{rows:#?}"
        );
        let text = rows
            .iter()
            .position(|r| r.contains("the minutes"))
            .expect("the text");
        assert!(
            text > 0 && rows[text - 1].contains("▤ notes"),
            "above its text (1268): {rows:#?}"
        );
        model.ascii = true;
        assert!(shown(&model)
            .iter()
            .any(|r| r.contains("[f] notes.txt - 2 KB")));
        model.focus = Pane::Messages;
        model.message_cursor = 0;
        let file_key = model.conversations[0].messages[0].activity;
        let effects = update(&mut model, key(KeyCode::Enter));
        assert_eq!(
            effects,
            [UiEffect::OpenFile {
                peer: PEER,
                key: file_key,
                index: 0
            }]
        );
    }

    /// A voice message in the terminal (1222): its length above its text
    /// or alone, ASCII where the screen is; `Enter` on it asks the shell
    /// to play it.
    #[test]
    fn the_terminal_says_a_voice_message_and_enter_plays_it() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(
            &mut model,
            UiEvent::Resize {
                width: 100,
                height: 30,
            },
        );
        engine(
            &mut model,
            OutboxEvent::Received {
                sent_at: 0,
                message_id: ID,
                source: PEER,
                title: Vec::new(),
                body: Vec::new(),
                fields: voice::voice_fields(voice::AM_CODEC2_2400, vec![0; 2_100]),
                verified: true,
            },
        );
        let shown = |model: &UiModel<Field>| -> Vec<String> {
            view(model).messages.iter().map(text_of).collect()
        };
        assert!(
            shown(&model)
                .iter()
                .any(|r| r.contains("♪ voice message · 0:07")),
            "{:#?}",
            shown(&model)
        );
        model.ascii = true;
        assert!(shown(&model)
            .iter()
            .any(|r| r.contains("[v] voice message - 0:07")));
        model.focus = Pane::Messages;
        model.message_cursor = 0;
        let voice_key = model.conversations[0].messages[0].activity;
        let effects = update(&mut model, key(KeyCode::Enter));
        assert_eq!(
            effects,
            [UiEffect::PlayVoice {
                peer: PEER,
                key: voice_key
            }]
        );
    }

    /// A forward is the text and the content fields again, in the other
    /// conversation, made if new; not the first sender's telemetry.
    #[test]
    fn a_forward_copies_text_and_content_fields() {
        use leviculum_lxmf::constants::{FIELD_IMAGE, FIELD_TELEMETRY};
        let mut model = UiModel::<Field>::new(&[PEER]);
        engine(
            &mut model,
            OutboxEvent::Received {
                sent_at: 0,
                message_id: ID,
                source: PEER,
                title: Vec::new(),
                body: b"look".to_vec(),
                fields: vec![(FIELD_TELEMETRY, vec![0xc0]), (FIELD_IMAGE, vec![0x01])],
                verified: true,
            },
        );
        let key = model.conversations[0].messages[0].activity;
        let effects = update(
            &mut model,
            UiEvent::Forward {
                from: PEER,
                key,
                to: OTHER,
            },
        );
        assert_eq!(
            effects,
            [UiEffect::Send {
                to: OTHER,
                body: "look".to_string(),
                fields: marked(vec![(FIELD_IMAGE, vec![0x01])]),
            }]
        );
        let other = model.conversations.iter().position(|c| c.peer == OTHER);
        let other = &model.conversations[other.expect("made")];
        assert_eq!(other.messages.last().map(|m| m.body.as_str()), Some("look"));
        assert!(other.messages.last().is_some_and(|m| m.outbound));
        assert!(update(
            &mut model,
            UiEvent::Forward {
                from: PEER,
                key: 999,
                to: OTHER
            }
        )
        .is_empty());
    }

    /// A frontend's delete of several: each gone from the screen and the
    /// store, one mark for the home's devices; one still sending is kept.
    #[test]
    fn a_frontend_deletes_several_and_keeps_a_send_in_flight() {
        use crate::home::wire::{Mark, MarkFlag};
        let mut model = UiModel::<Field>::new(&[PEER]);
        model.home_client = true;
        received(&mut model, ID, "one");
        received(&mut model, ID2, "two");
        update(
            &mut model,
            UiEvent::Write {
                peer: PEER,
                body: "three".to_string(),
            },
        );
        model.take_writes();
        model.take_marks();
        model.toasts.clear();
        let keys: Vec<u64> = model.conversations[0]
            .messages
            .iter()
            .map(|m| m.activity)
            .collect();
        update(&mut model, UiEvent::Delete { peer: PEER, keys });
        let left: Vec<&str> = model.conversations[0]
            .messages
            .iter()
            .map(|m| m.body.as_str())
            .collect();
        assert_eq!(left, ["three"]);
        assert_eq!(
            model.take_marks(),
            [Mark {
                flag: MarkFlag::Deleted,
                ids: vec![ID, ID2],
            }]
        );
        assert_eq!(washed(model.take_writes()).len(), 2);
        assert_eq!(
            model.current_toast().map(|t| t.text.clone()),
            Some(
                "2 messages deleted from every device of this home; 1 still being sent, kept"
                    .to_string()
            )
        );
    }

    /// The detail a frontend shows is the terminal's `v` view.
    #[test]
    fn a_frontend_gets_the_detail_view_by_key() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        received(&mut model, ID, "one");
        let key = model.conversations[0].messages[0].activity;
        let detail = message_detail(&model, PEER, key).expect("detail");
        assert!(
            detail.rows.iter().any(|r| r.contains(&to_hex(&ID))),
            "{:?}",
            detail.rows
        );
        assert!(detail.rows.iter().any(|r| r == "signature verified"));
        assert!(message_detail(&model, PEER, 999).is_none());
    }

    /// 1381, finding 19: the phone's Info is plain rows in the reader's
    /// language, no hex; the terminal's lines with the id come along for
    /// Advanced. Control: the same message in English.
    #[test]
    fn the_info_speaks_the_readers_language_and_keeps_the_hex_for_advanced() {
        let mut model = UiModel::<Field>::new(&[PEER]);
        update(&mut model, UiEvent::Clock(at(25, 14, 0)));
        received(&mut model, ID, "one");
        let key = model.conversations[0].messages[0].activity;
        let info = info::message_info(&model, PEER, key).expect("info");
        let said =
            |lang| -> Vec<(String, String)> { info.rows.iter().map(|r| r.resolve(lang)).collect() };
        let de = said(strings::Lang::De);
        let name = model.names.label(&PEER, model.lang);
        assert_eq!(de[0], ("Von".to_string(), name.clone()));
        // Written and taken at once here: both rows say so (1445).
        assert_eq!(de[1], ("Gesendet".to_string(), "Heute · 14:00".to_string()));
        assert_eq!(
            de[2],
            ("Empfangen".to_string(), "Heute · 14:00".to_string())
        );
        assert_eq!(
            de[3],
            (
                "Absender".to_string(),
                "Durch die Signatur bestätigt".to_string()
            )
        );
        assert_eq!(de[4], ("Größe".to_string(), "0,1 KB".to_string()));
        assert_eq!(de.len(), 5, "{de:?}");
        let hex = to_hex(&ID);
        assert!(
            de.iter()
                .all(|(l, v)| !l.contains(&hex[..8]) && !v.contains(&hex[..8])),
            "{de:?}"
        );
        assert!(
            info.advanced.contains(&format!("id     {hex}")),
            "{:?}",
            info.advanced
        );
        let en = said(strings::Lang::En);
        assert_eq!(en[0], ("From".to_string(), name));
        assert_eq!(en[4], ("Size".to_string(), "0.1 KB".to_string()));
        assert!(info::message_info(&model, PEER, 999).is_none());
    }

    /// The terminal's verbs of 1371, on two restored chats: PEER's
    /// message 5 under the cursor, PEER open.
    fn two_chats() -> UiModel<Field> {
        let mut model = UiModel::<Field>::new(&[]);
        model.restore(vec![
            restored(PEER, vec![stored(5, false, "lunch?", None)], 0),
            restored(OTHER, vec![stored(2, false, "older", None)], 0),
        ]);
        update(&mut model, UiEvent::Clock(at(25, 14, 0)));
        model.message_cursor = 0;
        model
    }

    fn sends(effects: &[UiEffect]) -> Vec<([u8; 16], String, Vec<i64>)> {
        effects
            .iter()
            .filter_map(|e| match e {
                UiEffect::Send { to, body, fields } => {
                    Some((*to, body.clone(), fields.iter().map(|(k, _)| *k).collect()))
                }
                _ => None,
            })
            .collect()
    }

    /// 1371: `reply` makes the next send answer the message under the
    /// cursor. Control: the same send without it carries no reply field.
    #[test]
    fn the_palette_replies_to_the_message_under_the_cursor() {
        use leviculum_lxmf::constants::FIELD_REPLY_TO;
        let mut model = two_chats();
        model.focus = Pane::Compose;
        typed(&mut model, "plain");
        let plain = update(&mut model, UiEvent::Run(PaletteCommand::Send));
        let plain = sends(&plain);
        assert_eq!(plain.len(), 1);
        assert!(!plain[0].2.contains(&FIELD_REPLY_TO), "control: no reply");

        model.message_cursor = 0;
        update(&mut model, UiEvent::Run(PaletteCommand::Reply));
        assert_eq!(model.open_reply(), Some((PEER, [5; 32])));
        typed(&mut model, "yes");
        let answered = sends(&update(&mut model, UiEvent::Run(PaletteCommand::Send)));
        assert_eq!(answered.len(), 1);
        assert_eq!(answered[0].0, PEER);
        assert!(answered[0].2.contains(&FIELD_REPLY_TO), "{answered:?}");
        assert_eq!(model.open_reply(), None, "the reply is spent");
    }

    /// `forward` asks a name and sends the copy there; a name nobody has
    /// sends nothing. `send again` does nothing to a message that did not
    /// fail.
    #[test]
    fn the_palette_forwards_and_sends_again_only_a_failed_one() {
        let mut model = two_chats();
        update(&mut model, UiEvent::Run(PaletteCommand::Forward));
        let ask = model.palette.as_ref().and_then(|p| p.ask);
        assert_eq!(ask, Some(contacts::Ask::Forward { from: PEER, key: 5 }));
        let label = model.names.label(&OTHER, model.lang);
        let none = model.answer_ask(ask.expect("asked"), "nobody at all".to_string());
        assert!(sends(&none).is_empty(), "control: no such friend");
        let forwarded = model.answer_ask(ask.expect("asked"), label);
        let forwarded = sends(&forwarded);
        assert_eq!(forwarded.len(), 1);
        assert_eq!((forwarded[0].0, forwarded[0].1.as_str()), (OTHER, "lunch?"));

        assert!(update(&mut model, UiEvent::Run(PaletteCommand::SendAgain)).is_empty());
        let mut failed = UiModel::<Field>::new(&[]);
        failed.restore(vec![restored(
            PEER,
            vec![stored(7, true, "lost", Some(DeliveryState::Failed))],
            0,
        )]);
        failed.message_cursor = 0;
        let again = sends(&update(
            &mut failed,
            UiEvent::Run(PaletteCommand::SendAgain),
        ));
        assert_eq!(again.len(), 1);
        assert_eq!((again[0].0, again[0].1.as_str()), (PEER, "lost"));
    }

    /// `mute`, `verify` and `mark unread` swap the open chat's state and
    /// back; the window's row menu reaches the same through `peer_word`,
    /// and a forget still asking is no event.
    #[test]
    fn mute_verify_and_unread_swap_and_the_window_gets_the_same_events() {
        let mut model = two_chats();
        assert!(!model.muted(&PEER));
        update(&mut model, UiEvent::Run(PaletteCommand::Mute));
        assert!(model.muted(&PEER));
        update(&mut model, UiEvent::Run(PaletteCommand::Mute));
        assert!(!model.muted(&PEER));
        update(&mut model, UiEvent::Run(PaletteCommand::Verify));
        assert!(model.verified(&PEER));
        update(&mut model, UiEvent::Run(PaletteCommand::Verify));
        assert!(!model.verified(&PEER));
        update(&mut model, UiEvent::Run(PaletteCommand::MarkUnread));
        assert_eq!(model.conversations[model.selected].unread, 1);
        update(&mut model, UiEvent::Run(PaletteCommand::MarkUnread));
        assert_eq!(model.conversations[model.selected].unread, 0);

        let target = intent::Target::Peer(OTHER);
        let pin = model.peer_word(&intent::Intent::Pin {
            peer: target,
            on: intent::Switch::On,
        });
        match pin {
            Some(UiEvent::PeerState { peer, state }) => {
                assert_eq!(peer, OTHER);
                assert!(state.pinned);
            }
            other => panic!("not the pin: {other:?}"),
        }
        let mute = model.peer_word(&intent::Intent::Mute {
            peer: target,
            secs: -1,
        });
        assert!(matches!(
            mute,
            Some(UiEvent::PeerState { state, .. }) if state.muted_until == Some(state::MUTED_ALWAYS)
        ));
        let asking = intent::Intent::Forget {
            peer: target,
            everywhere: false,
            confirmed: false,
        };
        assert_eq!(model.peer_word(&asking), None, "control: it still asks");
        assert_eq!(model.peer_word(&intent::Intent::Copy), None);
    }

    /// `photo size` steps through the three tiers and keeps each;
    /// `position` reads a typed fix, `share position` sends it once or
    /// starts a share, `stop sharing` ends it.
    #[test]
    fn the_palette_sets_the_photo_size_and_shares_a_typed_position() {
        let mut model = two_chats();
        let mut seen = Vec::new();
        for _ in 0..3 {
            let effects = update(&mut model, UiEvent::Run(PaletteCommand::PhotoSize));
            assert!(effects.iter().any(|e| matches!(
                e,
                UiEffect::Remember {
                    key: "photo_size",
                    ..
                }
            )));
            seen.push(model.photo_size);
        }
        assert_eq!(
            seen,
            [
                photos::PhotoSize::Original,
                photos::PhotoSize::Small,
                photos::PhotoSize::Phone
            ]
        );

        update(&mut model, UiEvent::Run(PaletteCommand::SharePosition));
        let share = model.palette.as_ref().and_then(|p| p.ask).expect("asked");
        assert!(
            sends(&model.answer_ask(share, String::new())).is_empty(),
            "no fix yet"
        );

        update(&mut model, UiEvent::Run(PaletteCommand::Position));
        let position = model.palette.as_ref().and_then(|p| p.ask).expect("asked");
        assert_eq!(position, contacts::Ask::Position);
        model.answer_ask(position, "52.5163, 13.3777".to_string());
        assert!(model.bar.by_hand);
        let once = sends(&model.answer_ask(share, String::new()));
        assert_eq!(once.len(), 1);
        assert!(once[0]
            .2
            .contains(&leviculum_lxmf::constants::FIELD_TELEMETRY));

        model.answer_ask(share, "1h".to_string());
        assert!(model.share(&PEER).is_some());
        update(&mut model, UiEvent::Run(PaletteCommand::StopSharing));
        assert!(model.share(&PEER).is_none());
    }

    /// `delete all maps` asks first and only where there are maps; once
    /// answered the shell is told.
    #[test]
    fn delete_all_maps_asks_and_then_tells_the_shell() {
        const FLORENCE: &[u8] = include_bytes!("../../leviculum-map/fixtures/firenze.lmr");
        let mut model = two_chats();
        update(&mut model, UiEvent::Run(PaletteCommand::DeleteAllMaps));
        assert!(model.palette.is_none(), "control: no maps, no question");
        let region = *leviculum_map::region::Region::from_bytes(FLORENCE)
            .expect("the fixture")
            .header();
        model.map_setup.regions = vec![region];
        update(&mut model, UiEvent::Run(PaletteCommand::DeleteAllMaps));
        let ask = model.palette.as_ref().and_then(|p| p.ask).expect("asked");
        assert!(model.ask_rows(ask)[0].contains("delete all 1 maps"));
        assert_eq!(
            model.answer_ask(ask, String::new()),
            [UiEffect::DeleteAllMaps]
        );
    }

    /// `send picture`, `send file` and `code in a picture` ask a path and
    /// hand it to the shell; an empty one hands nothing over, and a code
    /// the shell read opens its conversation.
    #[test]
    fn a_typed_path_goes_to_the_shell_and_a_read_code_opens_its_chat() {
        let mut model = two_chats();
        for (command, photo) in [
            (PaletteCommand::SendPicture, true),
            (PaletteCommand::SendFile, false),
        ] {
            update(&mut model, UiEvent::Run(command));
            let ask = model.palette.as_ref().and_then(|p| p.ask).expect("asked");
            assert!(
                model.answer_ask(ask, "  ".to_string()).is_empty(),
                "control"
            );
            assert_eq!(
                model.answer_ask(ask, "~/hill.jpg".to_string()),
                [UiEffect::Attach {
                    peer: PEER,
                    path: "~/hill.jpg".to_string(),
                    photo
                }]
            );
        }
        update(&mut model, UiEvent::Run(PaletteCommand::CodeFromPicture));
        let ask = model.palette.as_ref().and_then(|p| p.ask).expect("asked");
        assert_eq!(
            model.answer_ask(ask, "code.png".to_string()),
            [UiEffect::ScanCode {
                path: "code.png".to_string()
            }]
        );
        let private: [u8; 64] = core::array::from_fn(|i| i as u8 + 9);
        let key = leviculum_core::Identity::from_private_key_bytes(&private)
            .expect("a key")
            .public_key_bytes();
        let code = crate::lxma::encode(&key).expect("a code");
        let peer = crate::home::lxmf_address(&key).expect("an address");
        update(&mut model, UiEvent::CodeRead(code));
        assert_eq!(model.conversations[model.selected].peer, peer);
    }

    /// A pulled message keeps the time its writer wrote it (1445, UI walk 1
    /// finding 2). A mailbox holds three days of a direct history and a
    /// group's, a new device joins and pulls it all at once on the third
    /// day at 17:53, through the client engine as `take_in` hands it on,
    /// into the screen and its store. Every message shows its writer's
    /// time, a group copy its writer's (the `lev` pair), not the home's
    /// acceptance; each chat runs in that order with its day lines; the
    /// list is in order of each chat's newest message, though the oldest
    /// chat's message was pulled last; the Info sheet says when it was
    /// written and when this device fetched it; and the store gives the
    /// same back after a restart.
    #[test]
    fn a_pulled_history_keeps_its_writers_times_in_the_chats_the_list_and_the_store() {
        use crate::engine::{delivery_address, Engine};
        use crate::outbox::Command;
        use leviculum_core::node::NodeCoreBuilder;
        use leviculum_core::{DestinationHash, Identity};
        use leviculum_lxmf::lev::{GroupCopy, Lev};
        use leviculum_lxmf::DeliveryMethod;
        use leviculum_std::driver::{StdClock, StdStorage};

        const DAY: i64 = 86_400;
        const H: i64 = 3_600;
        const M: i64 = 60;
        let d0: i64 = 20_833 * DAY;
        let pulled_at = d0 + 2 * DAY + 17 * H + 53 * M;

        let dir = tempfile::tempdir().expect("tempdir");
        let us = leviculum_std::generate_identity();
        let address = delivery_address(&us).expect("our address");
        let mut engine = Engine::client(address);
        let mut core = NodeCoreBuilder::new().enable_transport(false).build(
            rand_core::OsRng,
            StdClock::new(),
            StdStorage::new(dir.path().join("core")).expect("storage"),
        );
        let person = || {
            let identity = leviculum_std::generate_identity();
            let address = delivery_address(&identity).expect("address");
            (identity, address)
        };
        let (alice, alice_at) = person();
        let (dave, dave_at) = person();
        let (group, group_at) = person();
        let (bob, bob_at) = person();
        let (carol, carol_at) = person();
        for (identity, at) in [(&alice, alice_at), (&dave, dave_at), (&group, group_at)] {
            core.remember_identity(DestinationHash::new(at), identity.clone());
        }
        let message = |to: [u8; 16], from: &Identity, at: i64, body: &str, fields| {
            leviculum_lxmf::Message::create(
                to,
                delivery_address(from).expect("source"),
                from,
                at as f64,
                Vec::new(),
                body.as_bytes().to_vec(),
                fields,
                DeliveryMethod::Direct,
            )
            .expect("message")
        };
        // The home's copy of what `writer` wrote at `written`, accepted at
        // `accepted`: the copy's own timestamp is the acceptance.
        let copy = |writer: [u8; 16], written: i64, accepted: i64, body: &str| {
            let mut fields = Vec::new();
            Lev {
                group_copy: Some(GroupCopy {
                    sender: writer,
                    sent_ms: written as u64 * 1000,
                    epoch: 1,
                }),
                ..Lev::default()
            }
            .put_into(&mut fields);
            message(address, &group, accepted, body, fields)
        };
        let _ = (&bob, &carol);
        // The archive in its order, as the pull brings it: (entry, writer's
        // time). Dave's came to the home late and is the last entry.
        enum Entry {
            In(leviculum_lxmf::Message),
            Sent(leviculum_lxmf::Message),
        }
        let entries = [
            (
                Entry::In(message(address, &alice, d0 + 9 * H, "question", Vec::new())),
                d0 + 9 * H,
            ),
            (
                Entry::Sent(message(
                    alice_at,
                    &us,
                    d0 + 9 * H + 48 * M,
                    "answer",
                    Vec::new(),
                )),
                d0 + 9 * H + 48 * M,
            ),
            (
                Entry::In(copy(bob_at, d0 + 12 * H, d0 + 12 * H + 5, "bob: hi all")),
                d0 + 12 * H,
            ),
            (
                Entry::Sent(message(
                    group_at,
                    &us,
                    d0 + DAY + 8 * H,
                    "morning",
                    Vec::new(),
                )),
                d0 + DAY + 8 * H,
            ),
            (
                Entry::In(message(
                    address,
                    &alice,
                    d0 + DAY + 10 * H,
                    "monday",
                    Vec::new(),
                )),
                d0 + DAY + 10 * H,
            ),
            // Written before midnight, accepted after: the writer's day.
            (
                Entry::In(copy(
                    carol_at,
                    d0 + DAY + 23 * H + 50 * M,
                    d0 + 2 * DAY + 10 * M,
                    "carol: late",
                )),
                d0 + DAY + 23 * H + 50 * M,
            ),
            (
                Entry::In(message(
                    address,
                    &alice,
                    d0 + 2 * DAY + 8 * H,
                    "today",
                    Vec::new(),
                )),
                d0 + 2 * DAY + 8 * H,
            ),
            (
                Entry::In(message(address, &dave, d0 + 7 * H, "old news", Vec::new())),
                d0 + 7 * H,
            ),
        ];
        let mut written = std::collections::HashMap::new();
        for (entry, at) in &entries {
            let (command, id) = match entry {
                Entry::In(m) => (Command::Ingest { packed: m.pack() }, m.message_id),
                Entry::Sent(m) => (Command::HomeSent { packed: m.pack() }, m.message_id),
            };
            written.insert(id, *at);
            engine.submit(command);
        }
        let now_ms = core.now_ms();
        let _ = engine.on_tick(&mut core, now_ms);
        let events: Vec<OutboxEvent> = core::iter::from_fn(|| engine.next_event()).collect();

        let clock = WallTime {
            unix_secs: pulled_at,
            offset_secs: 0,
        };
        let mut model = UiModel::<Field>::new(&[]);
        model.home_client = true;
        model.history = true;
        update(&mut model, UiEvent::Clock(clock));
        for event in events {
            update(&mut model, UiEvent::Engine(event));
        }
        model.history = false;

        // Read the screen: every message's time, each chat's order and day
        // lines, the list's order.
        let check = |model: &UiModel<Field>, when: &str| {
            let mut seen = 0;
            for conversation in &model.conversations {
                let times: Vec<i64> = conversation
                    .messages
                    .iter()
                    .map(|m| {
                        let id = m.id.expect("an id");
                        let shown = m.at.expect("a time").unix_secs;
                        assert_eq!(
                            shown, written[&id],
                            "{when}: {:?} shows {shown}, written {} (pulled at {pulled_at})",
                            m.body, written[&id]
                        );
                        seen += 1;
                        shown
                    })
                    .collect();
                assert!(
                    times.windows(2).all(|w| w[0] <= w[1]),
                    "{when}: not in writer order: {times:?}"
                );
            }
            assert_eq!(seen, entries.len(), "{when}");
            let peers: Vec<[u8; 16]> = model
                .order()
                .into_iter()
                .map(|i| model.conversations[i].peer)
                .collect();
            assert_eq!(
                peers,
                [alice_at, group_at, dave_at],
                "{when}: the list is not by each chat's newest message"
            );
            let g = model
                .conversations
                .iter()
                .position(|c| c.peer == group_at)
                .expect("the group's chat");
            let days: Vec<bool> = chat::placements(&chat::bubbles(model, g))
                .iter()
                .map(|p| p.day)
                .collect();
            assert_eq!(days, [true, true, false], "{when}: the group's day lines");
        };
        check(&model, "pulled");

        // The Info sheet of Alice's last: written then, fetched now.
        let today = written
            .iter()
            .find(|(_, at)| **at == d0 + 2 * DAY + 8 * H)
            .map(|(id, _)| *id)
            .expect("today's id");
        let alice_chat = model
            .conversations
            .iter()
            .find(|c| c.peer == alice_at)
            .expect("alice's chat");
        let key = alice_chat
            .messages
            .iter()
            .find(|m| m.id == Some(today))
            .expect("today's message")
            .activity;
        let sheet = info::message_info(&model, alice_at, key).expect("its Info");
        let row = |label: strings::StrId| {
            sheet
                .rows
                .iter()
                .find(|r| r.label == strings::Text::key(label))
                .map(|r| format!("{:?}", r.value))
        };
        let sent = row(strings::StrId::InfoSent).expect("a Sent row");
        let received = row(strings::StrId::InfoReceived).expect("a Received row");
        assert!(sent.contains("\"08:00\""), "Sent: {sent}");
        assert!(received.contains("\"17:53\""), "Received: {received}");

        // The store keeps it across a restart.
        #[cfg(feature = "history")]
        {
            let path = dir.path().join("history.sqlite3");
            let mut store = crate::store::Store::open(&path, address).expect("store");
            store.write(&model.take_writes()).expect("written");
            drop(store);
            let store = crate::store::Store::open(&path, address).expect("store again");
            let mut restarted = UiModel::<Field>::new(&[]);
            restarted.home_client = true;
            update(&mut restarted, UiEvent::Clock(clock));
            restarted.restore(store.restore().expect("restored"));
            check(&restarted, "restarted");
        }
    }
}
